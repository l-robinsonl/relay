//! Local SQLite persistence for Relay.
//!
//! The local database is authoritative: the application reads and writes it
//! directly and never waits on a network round trip to show a user their own
//! data. Synchronisation, when configured, reconciles this database with a
//! server afterwards.
//!
//! Deletion is always a tombstone. A row removed outright while offline would
//! leave no trace to synchronise, so the deletion itself would be lost and the
//! object would reappear on the next pull.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod conversation_repo;
mod db;
mod error;
pub mod migrations;
mod repo;
mod sync_repo;

pub use conversation_repo::{Conversation, ConversationRepo, StoredMessage};
pub use db::Database;
pub use error::{StorageError, StorageResult};
pub use repo::{
    AppStateRepo, CollectionRepo, EnvironmentRepo, HistoryRepo, RequestRepo, TreeRepo,
    WorkspaceRepo,
};
pub use sync_repo::{OutboxEntry, PendingChange, StoredConflict, SyncEntity, SyncRepo, SyncState};

/// Keys used in the `app_state` table.
pub mod state_keys {
    /// The workspace shown when the app starts.
    pub const ACTIVE_WORKSPACE: &str = "active_workspace";
    /// The selected environment, per workspace: `active_environment:<ws id>`.
    pub const ACTIVE_ENVIRONMENT_PREFIX: &str = "active_environment:";
    /// The UI theme.
    pub const THEME: &str = "theme";
    /// Serialised panel sizes and orientation.
    pub const LAYOUT: &str = "layout";
}
