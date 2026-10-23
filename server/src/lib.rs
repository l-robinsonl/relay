//! Relay's synchronisation server.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod app;
pub mod auth;
pub mod billing;
pub mod config;
pub mod error;
pub mod hub;
pub mod mail;
pub mod pages;
pub mod routes;
pub mod store;

pub use app::{AppState, AuthenticatedUser, router};
pub use config::Config;
pub use error::{ServerError, ServerResult};
pub use store::{Actor, PostgresStore, SqliteStore, SyncStore};
