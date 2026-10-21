//! Persistence behind one trait, implemented for SQLite and Postgres.
//!
//! The important property is that **authorisation lives inside the trait**,
//! not above it. Every method takes an [`Actor`] and resolves membership as
//! part of its own query, so there is no way to add a handler that forgets
//! to check. Knowing an object id is never sufficient to read or write it.

use async_trait::async_trait;
use relay_sync::{ApplyResult, ChangePage, Membership, MutationBatch, Role, UserProfile};

use crate::error::ServerResult;

pub mod logic;
pub mod postgres;
pub mod sqlite;

pub use postgres::PostgresStore;
pub use sqlite::SqliteStore;

/// Who is making a request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Actor {
    /// The authenticated user.
    pub user_id: String,
    /// Whether they administer the server itself, not merely a workspace.
    pub is_server_admin: bool,
}

impl Actor {
    /// An ordinary authenticated user.
    pub fn user(user_id: impl Into<String>) -> Self {
        Self {
            user_id: user_id.into(),
            is_server_admin: false,
        }
    }
}

/// A workspace as the server holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerWorkspace {
    /// The workspace id, shared with the client that created it.
    pub id: String,
    /// Its display name.
    pub name: String,
    /// The workspace's current revision; every mutation bumps it.
    pub change_seq: i64,
    /// When it was created, in epoch milliseconds.
    pub created_at: i64,
}

/// A newly created user, including the fields never sent to a client.
#[derive(Debug, Clone)]
pub struct NewUser {
    /// The address as typed; compared case-insensitively.
    pub email: String,
    /// An Argon2id hash. The plaintext never reaches this layer.
    pub password_hash: String,
    /// The name shown to teammates.
    pub display_name: String,
    /// Whether this user administers the server itself.
    pub is_server_admin: bool,
}

/// What a lookup by e-mail returns, including the hash to verify against.
#[derive(Debug, Clone)]
pub struct StoredUser {
    /// The publishable part of the record.
    pub profile: UserProfile,
    /// The stored hash, for verification only.
    pub password_hash: String,
}

/// Everything the server needs to persist.
#[async_trait]
pub trait SyncStore: Send + Sync + 'static {
    /// Applies any pending schema migrations.
    async fn migrate(&self) -> ServerResult<()>;

    /// A short name for the backend, for the health endpoint.
    fn backend(&self) -> &'static str;

    /// Verifies the database is reachable.
    async fn ping(&self) -> ServerResult<()>;

    // --- users and sessions -------------------------------------------------

    /// Creates a user. Fails if the e-mail is already registered.
    async fn create_user(&self, user: NewUser) -> ServerResult<UserProfile>;

    /// Looks a user up by e-mail, case-insensitively.
    async fn find_user_by_email(&self, email: &str) -> ServerResult<Option<StoredUser>>;

    /// How many users exist, used to decide whether to bootstrap an admin.
    async fn user_count(&self) -> ServerResult<i64>;

    /// Records a session. `token_hash` is a hash; the token itself is never
    /// stored, so a database leak does not hand over live sessions.
    async fn create_session(
        &self,
        user_id: &str,
        token_hash: &str,
        expires_at: i64,
    ) -> ServerResult<()>;

    /// Resolves a session token hash to its user, if it has not expired.
    async fn session_user(&self, token_hash: &str) -> ServerResult<Option<UserProfile>>;

    /// Ends one session.
    async fn delete_session(&self, token_hash: &str) -> ServerResult<()>;

    // --- workspaces and membership -----------------------------------------

    /// Creates a workspace owned by `actor`, who becomes its admin.
    async fn create_workspace(
        &self,
        actor: &Actor,
        id: &str,
        name: &str,
    ) -> ServerResult<ServerWorkspace>;

    /// Every workspace `actor` is a member of.
    async fn list_workspaces(&self, actor: &Actor) -> ServerResult<Vec<ServerWorkspace>>;

    /// `actor`'s membership of a workspace, or `None` if they have none.
    async fn membership(
        &self,
        workspace_id: &str,
        user_id: &str,
    ) -> ServerResult<Option<Membership>>;

    /// Adds or re-roles a member. Requires `actor` to be a workspace admin.
    async fn upsert_membership(
        &self,
        actor: &Actor,
        workspace_id: &str,
        user_id: &str,
        role: Role,
    ) -> ServerResult<Membership>;

    /// Removes a member. Requires `actor` to be a workspace admin.
    async fn remove_membership(
        &self,
        actor: &Actor,
        workspace_id: &str,
        user_id: &str,
    ) -> ServerResult<()>;

    /// Everyone in a workspace. Requires membership.
    async fn list_members(
        &self,
        actor: &Actor,
        workspace_id: &str,
    ) -> ServerResult<Vec<(Membership, UserProfile)>>;

    // --- synchronisation -----------------------------------------------------

    /// The workspace's current revision. Requires membership.
    async fn head(&self, actor: &Actor, workspace_id: &str) -> ServerResult<i64>;

    /// Objects with `rev > since`, oldest first. Requires membership.
    async fn changes_since(
        &self,
        actor: &Actor,
        workspace_id: &str,
        since: i64,
        limit: u32,
    ) -> ServerResult<ChangePage>;

    /// Applies a batch of mutations. Requires an editor or admin role.
    async fn apply(
        &self,
        actor: &Actor,
        workspace_id: &str,
        batch: MutationBatch,
    ) -> ServerResult<ApplyResult>;

    /// Every object in a workspace, for export. Requires membership.
    async fn export(&self, actor: &Actor, workspace_id: &str) -> ServerResult<ChangePage>;
}
