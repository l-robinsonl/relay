//! Relay's synchronisation server.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod error;
pub mod store;

pub use error::{ServerError, ServerResult};
pub use store::{Actor, PostgresStore, SqliteStore, SyncStore};
