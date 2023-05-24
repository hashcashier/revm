pub mod emptydb;
pub mod in_memory_db;

#[cfg(feature = "ethersdb")]
pub mod ethersdb;
#[cfg(feature = "ethersdb")]
pub use ethersdb::EthersDB;

pub mod states;

pub use states::{
    AccountStatus, BlockState, BundleAccount, BundleState, PlainAccount, RevertAccountState,
    RevertToSlot, Storage,
};

#[cfg(all(not(feature = "ethersdb"), feature = "web3db"))]
compile_error!(
    "`web3db` feature is deprecated, drop-in replacement can be found with feature `ethersdb`"
);

pub use crate::primitives::db::*;
pub use in_memory_db::*;

pub use emptydb::EmptyDB;
