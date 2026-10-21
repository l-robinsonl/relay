//! Relay application services.
//!
//! Everything the user interface can do lives here as an ordinary async
//! method. The Tauri layer is a set of one-line wrappers around these, which
//! keeps the application logic testable with `cargo test` instead of only
//! through a running desktop window.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod paths;
pub mod secrets;
mod service;

pub use secrets::{MemorySecretStore, SecretStore};
pub use service::{RelayService, ResolvedPreview};
