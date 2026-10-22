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
// Re-exported so the Tauri layer can name it without taking a direct
// dependency on relay-sync; those command wrappers should only ever
// need what relay-app exposes.
pub use engine::{Resolution, SyncEngine, SyncOutcome, initial_state};
pub use relay_sync::UserProfile;
pub use watcher::{WatchEvent, Watcher};
