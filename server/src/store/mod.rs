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

/// What an e-mailed token is for.
///
/// Part of the lookup, not a label on it: a token issued to confirm an
/// address must never be redeemable to set a password, or anyone who
/// can read a verification mail can take the account.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmailTokenPurpose {
    /// Confirms the address belongs to whoever registered it.
    VerifyEmail,
    /// Authorises setting a new password without the old one.
    ResetPassword,
}

impl EmailTokenPurpose {
    /// How it is spelled in the database.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::VerifyEmail => "verify_email",
            Self::ResetPassword => "reset_password",
        }
    }

    /// How long a token of this kind stays valid.
    ///
    /// A reset token is short-lived because it is a bearer credential
    /// for the whole account; a verification token is merely a proof of
    /// delivery, and expiring it overnight strands anyone who registered
    /// before going to bed.
    pub fn lifetime(self) -> std::time::Duration {
        match self {
            Self::VerifyEmail => std::time::Duration::from_secs(24 * 60 * 60),
            Self::ResetPassword => std::time::Duration::from_secs(60 * 60),
        }
    }
}

/// What a lookup by e-mail returns, including the hash to verify against.
#[derive(Debug, Clone)]
pub struct StoredUser {
    /// The publishable part of the record.
    pub profile: UserProfile,
    /// The stored hash, for verification only.
    pub password_hash: String,
}

/// A pending invitation, as an administrator sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invitation {
    /// Its id, for revoking.
    pub id: String,
    /// Which workspace it grants access to.
    pub workspace_id: String,
    /// The address it was sent to.
    pub email: String,
    /// The role it grants. The holder cannot choose a better one.
    pub role: Role,
    /// Who sent it.
    pub invited_by: String,
    /// When it was sent, in epoch milliseconds.
    pub created_at: i64,
    /// When it stops working.
    pub expires_at: i64,
}

/// What the holder of an invitation token is shown before accepting.
///
/// Deliberately thin: someone holding a token has not signed in yet, so
/// this is readable without authentication and must not disclose more
/// than the invitation itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvitationOffer {
    /// Which workspace, by id.
    pub workspace_id: String,
    /// Its name, so the page can say what is being offered.
    pub workspace_name: String,
    /// The address it was sent to.
    pub email: String,
    /// The role it grants.
    pub role: Role,
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

    // --- e-mailed tokens -------------------------------------------------

    /// Stores the hash of a token that was e-mailed to a user.
    async fn create_email_token(
        &self,
        user_id: &str,
        purpose: EmailTokenPurpose,
        token_hash: &str,
        expires_at: i64,
    ) -> ServerResult<()>;

    /// Redeems a token, returning the user it belonged to.
    ///
    /// Deletes and returns in one statement, so a token presented twice
    /// concurrently is honoured exactly once. An expired token is
    /// removed and reported as absent, which is the same answer a
    /// forged one gets.
    async fn consume_email_token(
        &self,
        purpose: EmailTokenPurpose,
        token_hash: &str,
        now: i64,
    ) -> ServerResult<Option<String>>;

    /// When the most recent token of this kind was issued to this user.
    ///
    /// Backs the resend cooldown: without one, anyone who knows an
    /// address can use this server to post mail at it.
    async fn latest_email_token_at(
        &self,
        user_id: &str,
        purpose: EmailTokenPurpose,
    ) -> ServerResult<Option<i64>>;

    /// Discards every outstanding token of one kind for a user.
    async fn clear_email_tokens(
        &self,
        user_id: &str,
        purpose: EmailTokenPurpose,
    ) -> ServerResult<()>;

    /// Records that a user has confirmed their address.
    async fn mark_email_verified(&self, user_id: &str, at: i64) -> ServerResult<()>;

    /// Replaces a user's password hash.
    async fn set_password_hash(&self, user_id: &str, password_hash: &str) -> ServerResult<()>;

    /// Signs every device out, by dropping all of a user's sessions.
    async fn delete_sessions_for_user(&self, user_id: &str) -> ServerResult<u64>;

    // --- billing ---------------------------------------------------------

    /// What a workspace is entitled to.
    ///
    /// No row means unmetered: a self-hosted server never writes one,
    /// and on Cloud a workspace that predates billing should keep
    /// working rather than silently losing its seats.
    async fn entitlement(&self, workspace_id: &str) -> ServerResult<crate::billing::Entitlement>;

    /// Every subscription covering a workspace this person belongs to.
    ///
    /// The allowance follows the person, and which plans they are on
    /// is read from these. Deliberately returns the subscriptions
    /// rather than a plan: what a price means is configuration, which
    /// the store does not have.
    async fn subscriptions_for_user(
        &self,
        user_id: &str,
    ) -> ServerResult<Vec<crate::billing::Subscription>>;

    /// What this person has spent on the hosted assistant in a period.
    async fn assistant_usage(
        &self,
        user_id: &str,
        period_start: i64,
    ) -> ServerResult<crate::billing::AssistantUsage>;

    /// Records a finished turn, returning the running total.
    ///
    /// One statement, because two turns can finish at the same
    /// moment and a read-then-write would lose one of them — which is
    /// the wrong way round for a cap to be wrong.
    async fn record_assistant_turn(
        &self,
        user_id: &str,
        period_start: i64,
        usage: crate::billing::AssistantUsage,
    ) -> ServerResult<crate::billing::AssistantUsage>;

    /// Records or updates a subscription.
    async fn save_subscription(
        &self,
        subscription: &crate::billing::Subscription,
    ) -> ServerResult<()>;

    /// The subscription for a workspace, if it has one.
    async fn subscription(
        &self,
        workspace_id: &str,
    ) -> ServerResult<Option<crate::billing::Subscription>>;

    /// Which workspace a Stripe customer belongs to.
    ///
    /// A webhook names the customer, not the workspace.
    async fn workspace_for_customer(&self, customer_id: &str) -> ServerResult<Option<String>>;

    /// Records that an event has been applied.
    ///
    /// Returns false when it had already been applied, which is the
    /// caller's cue to skip: Stripe retries until acknowledged, so the
    /// same event arrives more than once.
    async fn claim_billing_event(&self, event_id: &str) -> ServerResult<bool>;

    /// How many people are in a workspace, for the seat check.
    async fn member_count(&self, workspace_id: &str) -> ServerResult<u32>;

    // --- invitations -----------------------------------------------------

    /// Invites an address to a workspace. Workspace administrators only.
    async fn create_invitation(
        &self,
        actor: &Actor,
        workspace_id: &str,
        email: &str,
        role: Role,
        token_hash: &str,
        expires_at: i64,
    ) -> ServerResult<Invitation>;

    /// Describes an unaccepted, unexpired invitation.
    ///
    /// Takes no [`Actor`]: holding the token is the credential, and the
    /// recipient has to see what they are being offered before they can
    /// have an account to be authorised as.
    async fn invitation_offer(
        &self,
        token_hash: &str,
        now: i64,
    ) -> ServerResult<Option<InvitationOffer>>;

    /// Accepts an invitation on behalf of a user, joining the workspace.
    ///
    /// One transaction: the membership and the `accepted_at` stamp land
    /// together, so a token cannot be redeemed twice by racing.
    async fn accept_invitation(
        &self,
        user_id: &str,
        user_email: &str,
        token_hash: &str,
        now: i64,
    ) -> ServerResult<Membership>;

    /// Lists the invitations still outstanding. Administrators only.
    async fn list_invitations(
        &self,
        actor: &Actor,
        workspace_id: &str,
    ) -> ServerResult<Vec<Invitation>>;

    /// Withdraws an invitation. Administrators only.
    async fn revoke_invitation(
        &self,
        actor: &Actor,
        workspace_id: &str,
        invitation_id: &str,
    ) -> ServerResult<()>;

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
