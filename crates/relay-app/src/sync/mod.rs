//! Client-side synchronisation.
//!
//! The desktop application is local-first: this is how its local database
//! reconciles with a server, not how it reads or writes data. Everything
//! continues to work with no server configured, and a sync failure never
//! blocks local editing.

mod client;
mod engine;
mod watcher;

pub use client::{ServerHealth, SyncClient};
pub use engine::{Resolution, SyncEngine, SyncOutcome, initial_state};
pub use watcher::{WatchEvent, Watcher};
