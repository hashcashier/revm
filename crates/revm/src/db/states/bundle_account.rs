use super::{
    plain_account::PlainStorage, AccountRevert, AccountStatus, PlainAccount, RevertToSlot, Storage,
    TransitionAccount,
};
use revm_interpreter::primitives::{AccountInfo, U256};
use revm_precompile::HashMap;

/// Seems better, and more cleaner. But all informations is there.
/// Should we extract storage...
#[derive(Clone, Debug)]
pub struct BundleAccount {
    pub account: Option<PlainAccount>,
    pub status: AccountStatus,
}

impl BundleAccount {
    pub fn new_loaded(info: AccountInfo, storage: PlainStorage) -> Self {
        Self {
            account: Some(PlainAccount { info, storage }),
            status: AccountStatus::Loaded,
        }
    }
    pub fn new_loaded_empty_eip161(storage: PlainStorage) -> Self {
        Self {
            account: Some(PlainAccount::new_empty_with_storage(storage)),
            status: AccountStatus::LoadedEmptyEIP161,
        }
    }
    pub fn new_loaded_not_existing() -> Self {
        Self {
            account: None,
            status: AccountStatus::LoadedNotExisting,
        }
    }
    /// Create new account that is newly created (State is AccountStatus::New)
    pub fn new_newly_created(info: AccountInfo, storage: PlainStorage) -> Self {
        Self {
            account: Some(PlainAccount { info, storage }),
            status: AccountStatus::New,
        }
    }

    /// Create account that is destroyed.
    pub fn new_destroyed() -> Self {
        Self {
            account: None,
            status: AccountStatus::Destroyed,
        }
    }

    /// Create changed account
    pub fn new_changed(info: AccountInfo, storage: PlainStorage) -> Self {
        Self {
            account: Some(PlainAccount { info, storage }),
            status: AccountStatus::Changed,
        }
    }

    pub fn is_some(&self) -> bool {
        match self.status {
            AccountStatus::Changed => true,
            AccountStatus::New => true,
            AccountStatus::NewChanged => true,
            AccountStatus::DestroyedNew => true,
            AccountStatus::DestroyedNewChanged => true,
            _ => false,
        }
    }

    pub fn storage_slot(&self, slot: U256) -> Option<U256> {
        self.account
            .as_ref()
            .and_then(|a| a.storage.get(&slot).cloned())
    }

    /// Fetch account info if it exist.
    pub fn account_info(&self) -> Option<AccountInfo> {
        self.account.as_ref().map(|a| a.info.clone())
    }

    /// Touche empty account, related to EIP-161 state clear.
    pub fn touch_empty(&mut self) {
        self.status = match self.status {
            AccountStatus::DestroyedNew => AccountStatus::DestroyedAgain,
            AccountStatus::New => {
                // account can be created empty them touched.
                // Note: we can probably set it to LoadedNotExisting.
                AccountStatus::Destroyed
            }
            AccountStatus::LoadedEmptyEIP161 => AccountStatus::Destroyed,
            _ => {
                // do nothing
                unreachable!("Wrong state transition, touch empty is not possible from {self:?}");
            }
        };
        self.account = None;
    }

    /// Consume self and make account as destroyed.
    ///
    /// Set account as None and set status to Destroyer or DestroyedAgain.
    pub fn selfdestruct(&mut self) -> TransitionAccount {
        // account should be None after selfdestruct so we can take it.
        let previous_info = self.account.take().map(|a| a.info);
        let previous_status = self.status;

        self.status = match self.status {
            AccountStatus::DestroyedNew | AccountStatus::DestroyedNewChanged => {
                AccountStatus::DestroyedAgain
            }
            AccountStatus::Destroyed => {
                // mark as destroyed again, this can happen if account is created and
                // then selfdestructed in same block.
                // Note: there is no big difference between Destroyed and DestroyedAgain
                // in this case, but was added for clarity.
                AccountStatus::DestroyedAgain
            }
            _ => AccountStatus::Destroyed,
        };

        TransitionAccount {
            info: None,
            status: self.status,
            previous_info,
            previous_status,
            storage: HashMap::new(),
        }
    }

    /// Newly created account.
    pub fn newly_created(
        &mut self,
        new_info: AccountInfo,
        new_storage: Storage,
    ) -> TransitionAccount {
        let previous_status = self.status;
        let mut previous_info = self.account.take();
        let mut storage = previous_info
            .as_mut()
            .map(|a| core::mem::take(&mut a.storage))
            .unwrap_or_default();

        storage.extend(new_storage.iter().map(|(k, s)| (*k, s.present_value)));
        self.status = match self.status {
            // if account was destroyed previously just copy new info to it.
            AccountStatus::DestroyedAgain | AccountStatus::Destroyed => AccountStatus::DestroyedNew,
            // if account is loaded from db.
            AccountStatus::LoadedNotExisting => AccountStatus::New,
            AccountStatus::LoadedEmptyEIP161 | AccountStatus::Loaded => {
                // if account is loaded and not empty this means that account has some balance
                // this does not mean that accoun't can be created.
                // We are assuming that EVM did necessary checks before allowing account to be created.
                AccountStatus::New
            }
            _ => unreachable!(
                "Wrong state transition to create:\nfrom: {:?}\nto: {:?}",
                self, new_info
            ),
        };
        let transition_account = TransitionAccount {
            info: Some(new_info.clone()),
            status: self.status,
            previous_status,
            previous_info: previous_info.map(|a| a.info),
            storage: new_storage,
        };
        self.account = Some(PlainAccount {
            info: new_info,
            storage,
        });
        transition_account
    }

    pub fn change(&mut self, new: AccountInfo, storage: Storage) -> TransitionAccount {
        let previous_status = self.status;
        let previous_info = self.account.as_ref().map(|a| a.info.clone());

        let mut this_storage = self
            .account
            .take()
            .map(|acc| acc.storage)
            .unwrap_or_default();
        let mut this_storage = core::mem::take(&mut this_storage);

        this_storage.extend(storage.iter().map(|(k, s)| (*k, s.present_value)));
        let changed_account = PlainAccount {
            info: new,
            storage: this_storage,
        };

        self.status = match self.status {
            AccountStatus::Loaded => {
                // If account was initially loaded we are just overwriting it.
                // We are not checking if account is changed.
                // storage can be.
                AccountStatus::Changed
            }
            AccountStatus::Changed => {
                // Update to new changed state.
                AccountStatus::Changed
            }
            AccountStatus::New => {
                // promote to NewChanged.
                // Check if account is empty is done outside of this fn.
                AccountStatus::NewChanged
            }
            AccountStatus::NewChanged => {
                // Update to new changed state.
                AccountStatus::NewChanged
            }
            AccountStatus::DestroyedNew => {
                // promote to DestroyedNewChanged.
                AccountStatus::DestroyedNewChanged
            }
            AccountStatus::DestroyedNewChanged => {
                // Update to new changed state.
                AccountStatus::DestroyedNewChanged
            }
            AccountStatus::LoadedEmptyEIP161 => {
                // Change on empty account, should transfer storage if there is any.
                AccountStatus::Changed
            }
            AccountStatus::LoadedNotExisting
            | AccountStatus::Destroyed
            | AccountStatus::DestroyedAgain => {
                unreachable!("Wronge state transition change: \nfrom:{self:?}")
            }
        };
        self.account = Some(changed_account);

        TransitionAccount {
            info: self.account.as_ref().map(|a| a.info.clone()),
            status: self.status,
            previous_info,
            previous_status,
            storage,
        }
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
                    account: Some(previous_account),
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
                account: Some(previous_account),
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
                        this.storage.extend(new_present_storage);
                        this.info = updated_info;
                        return Some(AccountRevert {
                            account: Some(this.info.clone()),
                            storage: previous_storage_from_update,
                            original_status: AccountStatus::Changed,
                        });
                    }
                    AccountStatus::Loaded => {
                        // extend the storage. original values is not used inside bundle.
                        let mut storage = core::mem::take(&mut this.storage);
                        storage.extend(new_present_storage);
                        let previous_account = this.info.clone();
                        self.status = AccountStatus::Changed;
                        self.account = Some(PlainAccount {
                            info: updated_info,
                            storage,
                        });
                        return Some(AccountRevert {
                            account: Some(previous_account),
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
                            account: Some(AccountInfo::default()),
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
                            account: None,
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
                    storage.extend(new_present_storage);
                    // set as new as we didn't have that transition
                    self.status = AccountStatus::New;
                    self.account = Some(PlainAccount {
                        info: updated_info,
                        storage: storage,
                    });
                    return Some(AccountRevert {
                        account: Some(AccountInfo::default()),
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
                        account: None,
                        storage: previous_storage_from_update,
                        original_status: AccountStatus::LoadedNotExisting,
                    });
                }
                AccountStatus::New => {
                    let mut storage = core::mem::take(&mut this.storage);
                    storage.extend(new_present_storage);

                    let previous_account = this.info.clone();
                    // set as new as we didn't have that transition
                    self.status = AccountStatus::NewChanged;
                    self.account = Some(PlainAccount {
                        info: updated_info,
                        storage: storage,
                    });
                    return Some(AccountRevert {
                        account: Some(previous_account),
                        storage: previous_storage_from_update,
                        original_status: AccountStatus::New,
                    });
                }
                AccountStatus::NewChanged => {
                    let mut storage = core::mem::take(&mut this.storage);
                    storage.extend(new_present_storage);

                    let previous_account = this.info.clone();
                    // set as new as we didn't have that transition
                    self.status = AccountStatus::NewChanged;
                    self.account = Some(PlainAccount {
                        info: updated_info,
                        storage: storage,
                    });
                    return Some(AccountRevert {
                        account: Some(previous_account),
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
                            account: None,
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
                            account: None,
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
                            account: None,
                            storage: previous_storage_from_update,
                            original_status: AccountStatus::DestroyedNewChanged,
                        }
                    }
                    AccountStatus::DestroyedNew => {
                        // Becomes DestroyedNewChanged
                        AccountRevert {
                            // empty account
                            account: Some(this.info.clone()),
                            storage: previous_storage_from_update,
                            original_status: AccountStatus::DestroyedNewChanged,
                        }
                    }
                    AccountStatus::DestroyedNewChanged => {
                        // Stays same as DestroyedNewChanged
                        AccountRevert {
                            // empty account
                            account: Some(this.info.clone()),
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
                            account: None,
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
                            account: Some(this.info.clone()),
                            storage: previous_storage_from_update,
                            original_status: AccountStatus::DestroyedNew,
                        };
                        return Some(ret);
                    }
                    AccountStatus::DestroyedNewChanged => {
                        // From DestroyedNewChanged to DestroyedAgain
                        let ret = AccountRevert {
                            // empty account
                            account: Some(this.info.clone()),
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
