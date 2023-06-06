use super::{
    plain_account::PlainStorage, reverts::AccountInfoRevert, AccountRevert, AccountStatus,
    PlainAccount, RevertToSlot, Storage, TransitionAccount,
};
use revm_interpreter::primitives::{AccountInfo, U256};
use revm_precompile::HashMap;

/// Account information focused on creating of database changesets
/// and Reverts.
///
/// Status is needed to know from what state we are applying the TransitionAccount.
///
/// Original account info is needed to know if there was a change.
/// Same thing for storage where original.
///
/// On selfdestruct storage original value should be ignored.
#[derive(Clone, Debug)]
pub struct BundleAccount {
    pub info: Option<AccountInfo>,
    pub original_info: Option<AccountInfo>,
    /// Contain both original and present state.
    /// When extracting changeset we compare if original value is different from present value.
    /// If it is different we add it to changeset.
    ///
    /// If Account was destroyed we ignore original value.
    pub storage: Storage,
    pub status: AccountStatus,
}

impl BundleAccount {
    pub fn storage_slot(&self, slot: U256) -> Option<U256> {
        self.storage.get(&slot).map(|s| s.present_value)
    }

    /// Fetch account info if it exist.
    pub fn account_info(&self) -> Option<AccountInfo> {
        self.info.clone()
    }

    /// Update to new state and generate AccountRevert that if applied to new state will
    /// revert it to previous state. If not revert is present, update is noop.
    ///
    /// TODO consume state and return it back with AccountRevert. This would skip some bugs
    /// of not setting the state.
    ///
    /// TODO recheck if change to simple account state enum disrupts anything.
    pub fn update_and_create_revert(
        &mut self,
        transition: TransitionAccount,
    ) -> Option<AccountRevert> {
        let updated_info = transition.info.unwrap_or_default();
        let updated_storage = transition.storage;
        let updated_status = transition.status;

        let new_present_storage = updated_storage
            .iter()
            .map(|(k, s)| (*k, s.present_value))
            .collect();

        // Helper function that exploads account and returns revert state.
        let make_it_explode =
            |original_status: AccountStatus, mut this: PlainAccount| -> Option<AccountRevert> {
                let previous_account = this.info.clone();
                // Take present storage values as the storages that we are going to revert to.
                // As those values got destroyed.
                let previous_storage = this
                    .storage
                    .drain()
                    .into_iter()
                    .map(|(key, value)| (key, RevertToSlot::Some(value)))
                    .collect();
                let revert = Some(AccountRevert {
                    account: AccountInfoRevert::RevertTo(previous_account),
                    storage: previous_storage,
                    original_status,
                });

                revert
            };
        // Very similar to make_it_explode but it will add additional zeros (RevertToSlot::Destroyed)
        // for the storage that are set if account is again created.
        //
        // Example is of going from New (state: 1: 10) -> DestroyedNew (2:10)
        // Revert of that needs to be list of key previous values.
        // [1:10,2:0]
        let make_it_expload_with_aftereffect = |original_status: AccountStatus,
                                                mut this: PlainAccount,
                                                destroyed_storage: HashMap<U256, RevertToSlot>|
         -> Option<AccountRevert> {
            let previous_account = this.info.clone();
            // Take present storage values as the storages that we are going to revert to.
            // As those values got destroyed.
            let mut previous_storage: HashMap<U256, RevertToSlot> = this
                .storage
                .drain()
                .into_iter()
                .map(|(key, value)| (key, RevertToSlot::Some(value)))
                .collect();
            for (key, _) in destroyed_storage {
                previous_storage
                    .entry(key)
                    .or_insert(RevertToSlot::Destroyed);
            }
            let revert = Some(AccountRevert {
                account: AccountInfoRevert::RevertTo(previous_account),
                storage: previous_storage,
                original_status,
            });

            revert
        };

        // Helper to extract storage from plain state and convert it to RevertToSlot::Destroyed.
        let destroyed_storage = |updated_storage: &Storage| -> HashMap<U256, RevertToSlot> {
            updated_storage
                .iter()
                .map(|(key, _)| (*key, RevertToSlot::Destroyed))
                .collect()
        };

        // handle it more optimal in future but for now be more flexible to set the logic.
        let previous_storage_from_update = updated_storage
            .iter()
            .filter(|s| s.1.original_value != s.1.present_value)
            .map(|(key, value)| (*key, RevertToSlot::Some(value.original_value.clone())))
            .collect();

        // Missing update is for Destroyed,DestroyedAgain,DestroyedNew,DestroyedChange.
        // as those update are different between each other.
        // It updated from state before destroyed. And that is NewChanged,New,Changed,LoadedEmptyEIP161.
        // take a note that is not updating LoadedNotExisting.
        let update_part_of_destroyed =
            |this: &mut Self, updated_storage: &Storage| -> Option<AccountRevert> {
                match this.status {
                    AccountStatus::NewChanged => make_it_expload_with_aftereffect(
                        AccountStatus::NewChanged,
                        this.account.clone().unwrap_or_default(),
                        destroyed_storage(&updated_storage),
                    ),
                    AccountStatus::New => make_it_expload_with_aftereffect(
                        // Previous block created account, this block destroyed it and created it again.
                        // This means that bytecode get changed.
                        AccountStatus::New,
                        this.account.clone().unwrap_or_default(),
                        destroyed_storage(&updated_storage),
                    ),
                    AccountStatus::Changed => make_it_expload_with_aftereffect(
                        AccountStatus::Changed,
                        this.account.clone().unwrap_or_default(),
                        destroyed_storage(&updated_storage),
                    ),
                    AccountStatus::LoadedEmptyEIP161 => make_it_expload_with_aftereffect(
                        AccountStatus::LoadedEmptyEIP161,
                        this.account.clone().unwrap_or_default(),
                        destroyed_storage(&updated_storage),
                    ),
                    _ => None,
                }
            };
        // Assume this account is going to be overwritten.
        let mut this = self.account.take().unwrap_or_default();
        match updated_status {
            AccountStatus::Changed => {
                match self.status {
                    AccountStatus::Changed => {
                        // extend the storage. original values is not used inside bundle.
                        let revert_info = if this.info != updated_info {
                            AccountInfoRevert::RevertTo(updated_info.clone())
                        } else {
                            AccountInfoRevert::DoNothing
                        };
                        this.storage.extend(new_present_storage);
                        this.info = updated_info;
                        return Some(AccountRevert {
                            account: revert_info,
                            storage: previous_storage_from_update,
                            original_status: AccountStatus::Changed,
                        });
                    }
                    AccountStatus::Loaded => {
                        // extend the storage. original values is not used inside bundle.
                        let mut storage = core::mem::take(&mut this.storage);
                        storage.extend(new_present_storage);
                        let info_revert = if this.info != updated_info {
                            AccountInfoRevert::RevertTo(this.info.clone())
                        } else {
                            AccountInfoRevert::DoNothing
                        };
                        self.status = AccountStatus::Changed;
                        self.account = Some(PlainAccount {
                            info: updated_info,
                            storage,
                        });
                        return Some(AccountRevert {
                            account: info_revert,
                            storage: previous_storage_from_update,
                            original_status: AccountStatus::Loaded,
                        });
                    } //discard changes
                    _ => unreachable!("Invalid state"),
                }
            }
            AccountStatus::New => {
                // this state need to be loaded from db
                match self.status {
                    AccountStatus::LoadedEmptyEIP161 => {
                        let mut storage = core::mem::take(&mut this.storage);
                        storage.extend(new_present_storage);
                        self.status = AccountStatus::New;
                        self.account = Some(PlainAccount {
                            info: updated_info,
                            storage: storage,
                        });
                        // old account is empty. And that is diffeerent from not existing.
                        return Some(AccountRevert {
                            account: AccountInfoRevert::RevertTo(AccountInfo::default()
                            ),
                            storage: previous_storage_from_update,
                            original_status: AccountStatus::LoadedEmptyEIP161,
                        });
                    }
                    AccountStatus::LoadedNotExisting => {
                        self.status = AccountStatus::New;
                        self.account = Some(PlainAccount {
                            info: updated_info,
                            storage: new_present_storage,
                        });
                        return Some(AccountRevert {
                            account: AccountInfoRevert::DeleteIt,
                            storage: previous_storage_from_update,
                            original_status: AccountStatus::LoadedNotExisting,
                        });
                    }
                    _ => unreachable!(
                        "Invalid transition to New account from: {self:?} to {updated_info:?} {updated_status:?}"
                    ),
                }
            }
            AccountStatus::NewChanged => match self.status {
                AccountStatus::LoadedEmptyEIP161 => {
                    let mut storage = core::mem::take(&mut this.storage);
                    let revert_info = if this.info != updated_info {
                        AccountInfoRevert::RevertTo(AccountInfo::default())
                    } else {
                        AccountInfoRevert::DoNothing
                    };
                    storage.extend(new_present_storage);
                    // set as new as we didn't have that transition
                    self.status = AccountStatus::New;
                    self.account = Some(PlainAccount {
                        info: updated_info,
                        storage: storage,
                    });
                    return Some(AccountRevert {
                        account: revert_info,
                        storage: previous_storage_from_update,
                        original_status: AccountStatus::LoadedEmptyEIP161,
                    });
                }
                AccountStatus::LoadedNotExisting => {
                    // set as new as we didn't have that transition
                    self.status = AccountStatus::New;
                    self.account = Some(PlainAccount {
                        info: updated_info,
                        storage: new_present_storage,
                    });
                    return Some(AccountRevert {
                        account: AccountInfoRevert::DeleteIt,
                        storage: previous_storage_from_update,
                        original_status: AccountStatus::LoadedNotExisting,
                    });
                }
                AccountStatus::New => {
                    let mut storage = core::mem::take(&mut this.storage);
                    storage.extend(new_present_storage);
                    let revert_info = if this.info != updated_info {
                        AccountInfoRevert::RevertTo(AccountInfo::default())
                    } else {
                        AccountInfoRevert::DoNothing
                    };
                    // set as new as we didn't have that transition
                    self.status = AccountStatus::NewChanged;
                    self.account = Some(PlainAccount {
                        info: updated_info,
                        storage: storage,
                    });
                    return Some(AccountRevert {
                        account: revert_info,
                        storage: previous_storage_from_update,
                        original_status: AccountStatus::New,
                    });
                }
                AccountStatus::NewChanged => {
                    let mut storage = core::mem::take(&mut this.storage);
                    storage.extend(new_present_storage);
                    let revert_info = if this.info != updated_info {
                        AccountInfoRevert::RevertTo(AccountInfo::default())
                    } else {
                        AccountInfoRevert::DoNothing
                    };
                    // set as new as we didn't have that transition
                    self.status = AccountStatus::NewChanged;
                    self.account = Some(PlainAccount {
                        info: updated_info,
                        storage: storage,
                    });
                    return Some(AccountRevert {
                        account: revert_info,
                        storage: previous_storage_from_update,
                        original_status: AccountStatus::NewChanged,
                    });
                }
                _ => unreachable!("Invalid state"),
            },
            AccountStatus::Loaded => {
                // No changeset, maybe just update data
                // Do nothing for now.
                return None;
            }
            AccountStatus::LoadedNotExisting => {
                // Not changeset, maybe just update data.
                // Do nothing for now.
                return None;
            }
            AccountStatus::LoadedEmptyEIP161 => {
                // No changeset maybe just update data.
                // Do nothing for now
                return None;
            }
            AccountStatus::Destroyed => {
                let ret = match self.status {
                    AccountStatus::NewChanged => make_it_explode(AccountStatus::NewChanged, this),
                    AccountStatus::New => make_it_explode(AccountStatus::New, this),
                    AccountStatus::Changed => make_it_explode(AccountStatus::Changed, this),
                    AccountStatus::LoadedEmptyEIP161 => {
                        make_it_explode(AccountStatus::LoadedEmptyEIP161, this)
                    }
                    AccountStatus::Loaded => {
                        make_it_explode(AccountStatus::LoadedEmptyEIP161, this)
                    }
                    AccountStatus::LoadedNotExisting => {
                        // Do nothing as we have LoadedNotExisting -> Destroyed (It is noop)
                        return None;
                    }
                    _ => unreachable!("Invalid state"),
                };

                // set present to destroyed.
                self.status = AccountStatus::Destroyed;
                // present state of account is `None`.
                self.account = None;
                return ret;
            }
            AccountStatus::DestroyedNew => {
                // Previous block created account
                // (It was destroyed on previous block or one before).

                // check common pre destroy paths.
                if let Some(revert_state) = update_part_of_destroyed(self, &updated_storage) {
                    // set to destroyed and revert state.
                    self.status = AccountStatus::DestroyedNew;
                    self.account = Some(PlainAccount {
                        info: updated_info,
                        storage: new_present_storage,
                    });
                    return Some(revert_state);
                }

                let ret = match self.status {
                    AccountStatus::Destroyed => {
                        // from destroyed state new account is made
                        Some(AccountRevert {
                            account: AccountInfoRevert::DeleteIt,
                            storage: previous_storage_from_update,
                            original_status: AccountStatus::Destroyed,
                        })
                    }
                    AccountStatus::LoadedNotExisting => {
                        // we can make self to be New
                        //
                        // Example of this transition is loaded empty -> New -> destroyed -> New.
                        // Is same as just loaded empty -> New.
                        //
                        // This will devour the Selfdestruct as it is not needed.
                        self.status = AccountStatus::New;
                        self.account = Some(PlainAccount {
                            info: updated_info,
                            storage: new_present_storage,
                        });
                        return Some(AccountRevert {
                            // empty account
                            account: AccountInfoRevert::DeleteIt,
                            storage: previous_storage_from_update,
                            original_status: AccountStatus::LoadedNotExisting,
                        });
                    }
                    AccountStatus::DestroyedAgain => make_it_expload_with_aftereffect(
                        // destroyed again will set empty account.
                        AccountStatus::DestroyedAgain,
                        PlainAccount::default(),
                        destroyed_storage(&updated_storage),
                    ),
                    AccountStatus::DestroyedNew => {
                        // From DestroyeNew -> DestroyedAgain -> DestroyedNew
                        // Note: how to handle new bytecode changed?
                        // TODO
                        return None;
                    }
                    _ => unreachable!("Invalid state"),
                };
                self.status = AccountStatus::DestroyedNew;
                self.account = Some(PlainAccount {
                    info: updated_info,
                    storage: new_present_storage,
                });
                return ret;
            }
            AccountStatus::DestroyedNewChanged => {
                // Previous block created account
                // (It was destroyed on previous block or one before).

                // check common pre destroy paths.
                if let Some(revert_state) = update_part_of_destroyed(self, &updated_storage) {
                    // set it to destroyed changed and update account as it is newest best state.
                    self.status = AccountStatus::DestroyedNewChanged;
                    self.account = Some(PlainAccount {
                        info: updated_info,
                        storage: new_present_storage,
                    });
                    return Some(revert_state);
                }

                let ret = match self.status {
                    AccountStatus::Destroyed => {
                        // Becomes DestroyedNew
                        AccountRevert {
                            // empty account
                            account: AccountInfoRevert::DeleteIt,
                            storage: previous_storage_from_update,
                            original_status: AccountStatus::DestroyedNewChanged,
                        }
                    }
                    AccountStatus::DestroyedNew => {
                        // Becomes DestroyedNewChanged
                        AccountRevert {
                            // empty account
                            account: AccountInfoRevert::RevertTo(this.info.clone()),
                            storage: previous_storage_from_update,
                            original_status: AccountStatus::DestroyedNewChanged,
                        }
                    }
                    AccountStatus::DestroyedNewChanged => {
                        let revert_info = if this.info != updated_info {
                            AccountInfoRevert::RevertTo(AccountInfo::default())
                        } else {
                            AccountInfoRevert::DoNothing
                        };
                        // Stays same as DestroyedNewChanged
                        AccountRevert {
                            // empty account
                            account: revert_info,
                            storage: previous_storage_from_update,
                            original_status: AccountStatus::DestroyedNewChanged,
                        }
                    }
                    AccountStatus::LoadedNotExisting => {
                        // Becomes New.
                        // Example of this happening is NotExisting -> New -> Destroyed -> New -> Changed.
                        // This is same as NotExisting -> New.
                        self.status = AccountStatus::New;
                        self.account = Some(PlainAccount {
                            info: updated_info,
                            storage: new_present_storage,
                        });
                        return Some(AccountRevert {
                            // empty account
                            account: AccountInfoRevert::DeleteIt,
                            storage: previous_storage_from_update,
                            original_status: AccountStatus::DestroyedNewChanged,
                        });
                    }
                    _ => unreachable!("Invalid state"),
                };

                self.status = AccountStatus::DestroyedNew;
                self.account = Some(PlainAccount {
                    info: updated_info,
                    storage: new_present_storage,
                });
                return Some(ret);
            }
            AccountStatus::DestroyedAgain => {
                // Previous block created account
                // (It was destroyed on previous block or one before).

                // check common pre destroy paths.
                if let Some(revert_state) = update_part_of_destroyed(self, &HashMap::default()) {
                    // set to destroyed and revert state.
                    self.status = AccountStatus::DestroyedAgain;
                    self.account = None;
                    return Some(revert_state);
                }
                match self.status {
                    AccountStatus::Destroyed => {
                        // From destroyed to destroyed again. is noop
                        return None;
                    }
                    AccountStatus::DestroyedNew => {
                        // From destroyed new to destroyed again.
                        let ret = AccountRevert {
                            // empty account
                            account: AccountInfoRevert::RevertTo(this.info.clone()),
                            storage: previous_storage_from_update,
                            original_status: AccountStatus::DestroyedNew,
                        };
                        return Some(ret);
                    }
                    AccountStatus::DestroyedNewChanged => {
                        // From DestroyedNewChanged to DestroyedAgain
                        let ret = AccountRevert {
                            // empty account
                            account: AccountInfoRevert::RevertTo(this.info.clone()),
                            storage: previous_storage_from_update,
                            original_status: AccountStatus::DestroyedNewChanged,
                        };
                        return Some(ret);
                    }
                    AccountStatus::DestroyedAgain => {
                        // DestroyedAgain to DestroyedAgain is noop
                        return None;
                    }
                    AccountStatus::LoadedNotExisting => {
                        // From LoadedNotExisting to DestroyedAgain
                        // is noop as account is destroyed again
                        return None;
                    }
                    _ => unreachable!("Invalid state"),
                }
            }
        }
    }
}