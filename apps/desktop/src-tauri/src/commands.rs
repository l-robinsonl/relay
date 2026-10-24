//! Tauri commands.
//!
//! Every command is a thin wrapper over [`relay_app::RelayService`]. The
//! logic lives there so it can be tested without a window; this file exists
//! only to define the IPC surface and keep it narrow.
//!
//! The surface is deliberately small and specific. There is no "run arbitrary
//! SQL" command, no "read any file" command, and no generic HTTP proxy the
//! webview can aim anywhere — `http_send` takes a request the user built.

use std::sync::Arc;

use relay_app::service::{ConflictView, OAuthTokenStatus, SendOutcome, SyncStatus};
use relay_app::sync::{ServerHealth, SyncOutcome};
use relay_app::{RelayService, ResolvedPreview};
use relay_core::import::ImportReport;
use relay_models::{
    Collection, CollectionId, Environment, EnvironmentId, ExecutionId, ExecutionProgress,
    HistoryEntry, HttpExchange, OAuth2Config, RelayError, RelayResult, RequestDoc, RequestId,
    TreeNode, Workspace, WorkspaceId,
};
use tauri::{Emitter, State};

/// The event channel carrying in-flight request progress to the webview.
pub const PROGRESS_EVENT: &str = "relay://exec-progress";

/// The event channel telling the webview a workspace changed on the server.
pub const SYNC_EVENT: &str = "relay://sync-changed";

/// The live watchers, one per connected workspace.
///
/// Held in Tauri's state so a watcher survives the command that started
/// it, and so switching workspace closes the previous socket rather than
/// accumulating one per workspace the user has ever opened.
#[derive(Default)]
pub struct SyncWatchers {
    inner: std::sync::Mutex<std::collections::HashMap<WorkspaceId, relay_app::sync::Watcher>>,
}

impl SyncWatchers {
    /// Stores a watcher, replacing and closing any previous one.
    pub fn insert(&self, workspace: WorkspaceId, watcher: relay_app::sync::Watcher) {
        if let Ok(mut guard) = self.inner.lock() {
            guard.insert(workspace, watcher);
        }
    }

    /// Closes the watcher for a workspace, if there is one.
    pub fn stop(&self, workspace: &WorkspaceId) {
        if let Ok(mut guard) = self.inner.lock()
            && let Some(watcher) = guard.remove(workspace)
        {
            watcher.stop();
        }
    }
}

/// Forwards engine progress to the frontend.
struct WindowProgress {
    app: tauri::AppHandle,
}

impl relay_http::ProgressSink for WindowProgress {
    fn emit(&self, progress: ExecutionProgress) {
        // A failed progress event must never fail the request itself.
        if let Err(err) = self.app.emit(PROGRESS_EVENT, &progress) {
            tracing::debug!("could not emit progress: {err}");
        }
    }
}

// --- workspaces ------------------------------------------------------------

/// Lists every workspace.
#[tauri::command]
pub async fn workspace_list(service: State<'_, RelayService>) -> RelayResult<Vec<Workspace>> {
    service.workspace_list().await
}

/// Returns the workspace to open on startup, creating one if needed.
#[tauri::command]
pub async fn workspace_active(service: State<'_, RelayService>) -> RelayResult<Workspace> {
    service.active_workspace().await
}

/// Creates a workspace.
#[tauri::command]
pub async fn workspace_create(
    service: State<'_, RelayService>,
    name: String,
) -> RelayResult<Workspace> {
    service.workspace_create(name).await
}

/// Saves a workspace.
#[tauri::command]
pub async fn workspace_save(
    service: State<'_, RelayService>,
    workspace: Workspace,
) -> RelayResult<Workspace> {
    service.workspace_save(workspace).await
}

/// Deletes a workspace and its contents.
#[tauri::command]
pub async fn workspace_delete(
    service: State<'_, RelayService>,
    id: WorkspaceId,
) -> RelayResult<()> {
    service.workspace_delete(id).await
}

// --- tree and collections --------------------------------------------------

/// The sidebar tree for a workspace.
#[tauri::command]
pub async fn collection_tree(
    service: State<'_, RelayService>,
    workspace_id: WorkspaceId,
) -> RelayResult<Vec<TreeNode>> {
    service.collection_tree(workspace_id).await
}

/// Every collection in a workspace.
#[tauri::command]
pub async fn collection_list(
    service: State<'_, RelayService>,
    workspace_id: WorkspaceId,
) -> RelayResult<Vec<Collection>> {
    service.collection_list(workspace_id).await
}

/// Creates a collection or folder.
#[tauri::command]
pub async fn collection_create(
    service: State<'_, RelayService>,
    workspace_id: WorkspaceId,
    name: String,
    parent_id: Option<CollectionId>,
) -> RelayResult<Collection> {
    service
        .collection_create(workspace_id, name, parent_id)
        .await
}

/// Saves a collection.
#[tauri::command]
pub async fn collection_save(
    service: State<'_, RelayService>,
    collection: Collection,
) -> RelayResult<Collection> {
    service.collection_save(collection).await
}

/// Deletes a collection and its subtree.
#[tauri::command]
pub async fn collection_delete(
    service: State<'_, RelayService>,
    id: CollectionId,
) -> RelayResult<()> {
    service.collection_delete(id).await
}

// --- requests --------------------------------------------------------------

/// Loads a request.
#[tauri::command]
pub async fn request_get(
    service: State<'_, RelayService>,
    id: RequestId,
) -> RelayResult<RequestDoc> {
    service.request_get(id).await
}

/// Creates a request.
#[tauri::command]
pub async fn request_create(
    service: State<'_, RelayService>,
    workspace_id: WorkspaceId,
    name: String,
    collection_id: Option<CollectionId>,
) -> RelayResult<RequestDoc> {
    service
        .request_create(workspace_id, name, collection_id)
        .await
}

/// Saves a request.
#[tauri::command]
pub async fn request_save(
    service: State<'_, RelayService>,
    doc: RequestDoc,
) -> RelayResult<RequestDoc> {
    service.request_save(doc).await
}

/// Deletes a request.
#[tauri::command]
pub async fn request_delete(service: State<'_, RelayService>, id: RequestId) -> RelayResult<()> {
    service.request_delete(id).await
}

/// Duplicates a request.
#[tauri::command]
pub async fn request_duplicate(
    service: State<'_, RelayService>,
    id: RequestId,
) -> RelayResult<RequestDoc> {
    service.request_duplicate(id).await
}

/// Moves a request into a collection.
#[tauri::command]
pub async fn request_move(
    service: State<'_, RelayService>,
    id: RequestId,
    collection_id: Option<CollectionId>,
    sort_order: f64,
) -> RelayResult<()> {
    service.request_move(id, collection_id, sort_order).await
}

/// Resolves a request's variables without sending it.
#[tauri::command]
pub async fn request_resolve(
    service: State<'_, RelayService>,
    doc: RequestDoc,
    environment_id: Option<EnvironmentId>,
) -> RelayResult<ResolvedPreview> {
    service.request_resolve(doc, environment_id).await
}

// --- execution -------------------------------------------------------------

/// Sends a request and returns the exchange.
///
/// The command is `async`, so the webview is never blocked: it awaits the
/// promise while rendering progress events.
#[tauri::command]
pub async fn http_send(
    app: tauri::AppHandle,
    service: State<'_, RelayService>,
    execution_id: ExecutionId,
    doc: RequestDoc,
    environment_id: Option<EnvironmentId>,
) -> RelayResult<HttpExchange> {
    let progress = Arc::new(WindowProgress { app });
    service
        .request_send(execution_id, doc, environment_id, progress)
        .await
}

/// Discards every stored cookie.
#[tauri::command]
pub fn cookies_clear(service: State<'_, RelayService>) {
    service.clear_cookies();
}

/// Sends a request and runs its tests against the response.
///
/// A failing test does not fail the call: the response is returned
/// either way, with the verdict beside it.
#[tauri::command]
pub async fn http_send_with_tests(
    app: tauri::AppHandle,
    service: State<'_, RelayService>,
    execution_id: ExecutionId,
    doc: RequestDoc,
    environment_id: Option<EnvironmentId>,
) -> RelayResult<SendOutcome> {
    let progress = Arc::new(WindowProgress { app });
    service
        .request_send_with_tests(execution_id, doc, environment_id, progress)
        .await
}

/// Cancels an in-flight request. Returns true if one was running.
#[tauri::command]
pub fn http_cancel(service: State<'_, RelayService>, execution_id: ExecutionId) -> bool {
    service.request_cancel(&execution_id)
}

// --- environments and secrets ----------------------------------------------

/// Every environment in a workspace.
#[tauri::command]
pub async fn environment_list(
    service: State<'_, RelayService>,
    workspace_id: WorkspaceId,
) -> RelayResult<Vec<Environment>> {
    service.environment_list(workspace_id).await
}

/// Creates an environment.
#[tauri::command]
pub async fn environment_create(
    service: State<'_, RelayService>,
    workspace_id: WorkspaceId,
    name: String,
) -> RelayResult<Environment> {
    service.environment_create(workspace_id, name).await
}

/// Saves an environment. Secret values are not written to the database.
#[tauri::command]
pub async fn environment_save(
    service: State<'_, RelayService>,
    environment: Environment,
) -> RelayResult<Environment> {
    service.environment_save(environment).await
}

/// Deletes an environment.
#[tauri::command]
pub async fn environment_delete(
    service: State<'_, RelayService>,
    id: EnvironmentId,
) -> RelayResult<()> {
    service.environment_delete(id).await
}

/// Stores a secret value in the OS credential store.
#[tauri::command]
pub fn secret_set(
    service: State<'_, RelayService>,
    environment_id: EnvironmentId,
    key: String,
    value: String,
) -> RelayResult<()> {
    service.secret_set(&environment_id, &key, &value)
}

/// Removes a stored secret value.
#[tauri::command]
pub fn secret_delete(
    service: State<'_, RelayService>,
    environment_id: EnvironmentId,
    key: String,
) -> RelayResult<()> {
    service.secret_delete(&environment_id, &key)
}

/// Whether a secret has a value, without returning it.
///
/// There is deliberately no `secret_get`: nothing in the webview has a reason
/// to hold a credential, so the IPC surface does not offer one.
#[tauri::command]
pub fn secret_is_set(
    service: State<'_, RelayService>,
    environment_id: EnvironmentId,
    key: String,
) -> bool {
    service.secret_is_set(&environment_id, &key)
}

/// The name of the credential backend in use, for the settings screen.
#[tauri::command]
pub fn secret_backend(service: State<'_, RelayService>) -> String {
    service.secret_backend().to_string()
}

// --- history ---------------------------------------------------------------

/// Recent executions, newest first.
#[tauri::command]
pub async fn history_list(
    service: State<'_, RelayService>,
    workspace_id: WorkspaceId,
    limit: u32,
    offset: u32,
) -> RelayResult<Vec<HistoryEntry>> {
    service.history_list(workspace_id, limit, offset).await
}

/// Clears a workspace's history.
#[tauri::command]
pub async fn history_clear(
    service: State<'_, RelayService>,
    workspace_id: WorkspaceId,
) -> RelayResult<usize> {
    service.history_clear(workspace_id).await
}

// --- import and export -----------------------------------------------------

/// Parses a cURL command into an unsaved request.
#[tauri::command]
pub fn import_curl(
    service: State<'_, RelayService>,
    command: String,
    workspace_id: WorkspaceId,
) -> RelayResult<RequestDoc> {
    service.import_curl(&command, workspace_id)
}

/// Parses an import file without saving anything.
///
/// The frontend shows the report — including everything that could not be
/// brought across — and calls `import_apply` only if the user accepts it.
#[tauri::command]
pub fn import_preview(
    service: State<'_, RelayService>,
    content: String,
    workspace_id: WorkspaceId,
) -> RelayResult<ImportReport> {
    service.import_detect(&content, workspace_id)
}

/// Reads an import file from disk and previews it.
#[tauri::command]
pub async fn import_preview_path(
    service: State<'_, RelayService>,
    path: String,
    workspace_id: WorkspaceId,
) -> RelayResult<ImportReport> {
    service.import_preview_path(path, workspace_id).await
}

/// Writes a workspace export to a file the user chose.
#[tauri::command]
pub async fn export_workspace_to_path(
    service: State<'_, RelayService>,
    workspace_id: WorkspaceId,
    path: String,
) -> RelayResult<()> {
    service.export_workspace_to_path(workspace_id, path).await
}

/// Writes a previously previewed import to the database, atomically.
#[tauri::command]
pub async fn import_apply(
    service: State<'_, RelayService>,
    report: ImportReport,
) -> RelayResult<usize> {
    service.import_apply(report).await
}

/// Exports a whole workspace as portable JSON, with secrets removed.
#[tauri::command]
pub async fn export_workspace(
    service: State<'_, RelayService>,
    workspace_id: WorkspaceId,
) -> RelayResult<String> {
    service.export_workspace(workspace_id).await
}

/// Renders a request as a cURL command.
#[tauri::command]
pub async fn export_curl(
    service: State<'_, RelayService>,
    doc: RequestDoc,
    environment_id: Option<EnvironmentId>,
) -> RelayResult<String> {
    service.export_curl(doc, environment_id).await
}

// --- synchronisation -------------------------------------------------------

/// Checks a server is reachable and speaks a protocol we understand.
#[tauri::command]
pub async fn sync_probe(
    service: State<'_, RelayService>,
    server_url: String,
) -> RelayResult<ServerHealth> {
    service.sync_probe(server_url).await
}

/// Connects a workspace to a server and signs in.
#[tauri::command]
pub async fn sync_connect(
    service: State<'_, RelayService>,
    workspace_id: WorkspaceId,
    server_url: String,
    email: String,
    password: String,
    join: bool,
) -> RelayResult<SyncStatus> {
    service
        .sync_connect(workspace_id, server_url, email, password, join)
        .await
}

/// Runs one synchronisation pass.
#[tauri::command]
pub async fn sync_now(
    service: State<'_, RelayService>,
    workspace_id: WorkspaceId,
) -> RelayResult<SyncOutcome> {
    service.sync_now(workspace_id).await
}

/// How a workspace's synchronisation stands.
#[tauri::command]
pub async fn sync_status(
    service: State<'_, RelayService>,
    workspace_id: WorkspaceId,
) -> RelayResult<SyncStatus> {
    service.sync_status(workspace_id).await
}

/// Stops a workspace synchronising, leaving its local data alone.
#[tauri::command]
pub async fn sync_disconnect(
    service: State<'_, RelayService>,
    workspace_id: WorkspaceId,
) -> RelayResult<()> {
    service.sync_disconnect(workspace_id).await
}

/// Starts listening for changes to a workspace on its server.
///
/// Returns true when a connection was opened. False means the workspace
/// is local-only or signed out, which is not a failure.
#[tauri::command]
pub async fn sync_watch(
    app: tauri::AppHandle,
    watchers: State<'_, SyncWatchers>,
    service: State<'_, RelayService>,
    workspace_id: WorkspaceId,
) -> RelayResult<bool> {
    // Replace any existing watcher for this workspace; dropping the old
    // one closes its socket.
    watchers.stop(&workspace_id);

    let (tx, mut rx) = tokio::sync::mpsc::channel(32);
    let Some(watcher) = service.sync_watch(workspace_id.clone(), tx).await? else {
        return Ok(false);
    };

    watchers.insert(workspace_id, watcher);

    tokio::spawn(async move {
        while let Some((workspace, event)) = rx.recv().await {
            let payload = match event {
                relay_app::sync::WatchEvent::Changed { seq } => {
                    serde_json::json!({ "workspaceId": workspace, "event": "changed", "seq": seq })
                }
                relay_app::sync::WatchEvent::Connected => {
                    serde_json::json!({ "workspaceId": workspace, "event": "connected" })
                }
                relay_app::sync::WatchEvent::Disconnected => {
                    serde_json::json!({ "workspaceId": workspace, "event": "disconnected" })
                }
            };

            if let Err(error) = app.emit(SYNC_EVENT, payload) {
                tracing::debug!("could not emit a sync event: {error}");
                break;
            }
        }
    });

    Ok(true)
}

/// Stops listening for changes to a workspace.
#[tauri::command]
pub fn sync_unwatch(watchers: State<'_, SyncWatchers>, workspace_id: WorkspaceId) {
    watchers.stop(&workspace_id);
}

/// Conflicts awaiting a decision.
#[tauri::command]
pub async fn sync_conflicts(
    service: State<'_, RelayService>,
    workspace_id: WorkspaceId,
) -> RelayResult<Vec<ConflictView>> {
    service.sync_conflicts(workspace_id).await
}

/// Applies the user's decision about a conflict.
#[tauri::command]
pub async fn sync_resolve(
    service: State<'_, RelayService>,
    conflict_id: String,
    keep_local: bool,
) -> RelayResult<()> {
    service.sync_resolve(conflict_id, keep_local).await
}

// --- application state -----------------------------------------------------

/// Reads a UI state value.
#[tauri::command]
pub async fn state_get(
    service: State<'_, RelayService>,
    key: String,
) -> RelayResult<Option<String>> {
    service.state_get(key).await
}

/// Writes a UI state value.
#[tauri::command]
pub async fn state_set(
    service: State<'_, RelayService>,
    key: String,
    value: String,
) -> RelayResult<()> {
    service.state_set(key, value).await
}

/// Writes a consistent backup of the local database to `destination`.
#[tauri::command]
pub async fn database_backup(
    service: State<'_, RelayService>,
    destination: String,
) -> RelayResult<()> {
    let db = service.database().clone();
    tokio::task::spawn_blocking(move || db.backup_to(&destination))
        .await
        .map_err(|e| RelayError::internal(e.to_string()))?
        .map_err(Into::into)
}

/// What the Auth tab knows about a held OAuth token.
#[tauri::command]
pub fn oauth_token_status(
    service: State<'_, RelayService>,
    config: OAuth2Config,
) -> OAuthTokenStatus {
    service.oauth_token_status(&config)
}

/// Fetches a token for the grants that need no browser.
#[tauri::command]
pub async fn oauth_fetch_token(
    service: State<'_, RelayService>,
    config: OAuth2Config,
) -> RelayResult<OAuthTokenStatus> {
    service.oauth_fetch_token(config).await
}

/// Runs the authorization code flow, opening the user's browser.
///
/// The browser is deliberately the real one rather than a webview: a
/// webview would not have the user's existing session with the identity
/// provider, would not show the address bar they need to check before
/// typing a password, and could in principle read what they typed.
#[tauri::command]
pub async fn oauth_authorize(
    app: tauri::AppHandle,
    service: State<'_, RelayService>,
    config: OAuth2Config,
) -> RelayResult<OAuthTokenStatus> {
    service
        .oauth_authorize(config, move |url| {
            use tauri_plugin_opener::OpenerExt;
            app.opener().open_url(url, None::<&str>).map_err(|e| {
                RelayError::new(
                    relay_models::ErrorKind::Io,
                    format!("could not open your browser: {e}"),
                )
            })
        })
        .await
}

/// Discards the token held for one configuration.
#[tauri::command]
pub fn oauth_forget_token(service: State<'_, RelayService>, config: OAuth2Config) {
    service.oauth_forget_token(&config);
}

/// Discards every held token.
#[tauri::command]
pub fn oauth_forget_all_tokens(service: State<'_, RelayService>) {
    service.oauth_forget_all_tokens();
}

/// Creates an account on a sync server.
#[tauri::command]
pub async fn sync_register(
    service: State<'_, RelayService>,
    server_url: String,
    email: String,
    password: String,
    display_name: String,
    invitation: Option<String>,
) -> RelayResult<relay_app::sync::UserProfile> {
    service
        .sync_register(server_url, email, password, display_name, invitation)
        .await
}

/// Describes an invitation without accepting it.
#[tauri::command]
pub async fn sync_invitation_offer(
    service: State<'_, RelayService>,
    server_url: String,
    token: String,
) -> RelayResult<relay_app::sync::InvitationOffer> {
    service.sync_invitation_offer(server_url, token).await
}

/// Joins the workspace an invitation names, signing in or registering.
#[tauri::command]
pub async fn sync_join_invited(
    service: State<'_, RelayService>,
    server_url: String,
    invitation: String,
    password: String,
    display_name: String,
) -> RelayResult<Workspace> {
    service
        .sync_join_invited(server_url, invitation, password, display_name)
        .await
}

/// Asks the server for another confirmation e-mail.
#[tauri::command]
pub async fn sync_request_verification(
    service: State<'_, RelayService>,
    server_url: String,
    email: String,
) -> RelayResult<()> {
    service.sync_request_verification(server_url, email).await
}

/// Asks the server for a password reset e-mail.
#[tauri::command]
pub async fn sync_request_password_reset(
    service: State<'_, RelayService>,
    server_url: String,
    email: String,
) -> RelayResult<()> {
    service.sync_request_password_reset(server_url, email).await
}

// --- workspace membership --------------------------------------------------

/// Who is in a workspace, and as what.
#[tauri::command]
pub async fn sync_members(
    service: State<'_, RelayService>,
    workspace_id: WorkspaceId,
) -> RelayResult<Vec<relay_app::sync::MemberView>> {
    service.sync_members(workspace_id).await
}

/// Asks a GraphQL endpoint what it accepts.
#[tauri::command]
pub async fn graphql_schema(
    service: State<'_, RelayService>,
    doc: relay_models::RequestDoc,
    environment: Option<relay_models::EnvironmentId>,
) -> RelayResult<relay_core::graphql::Schema> {
    service.graphql_schema(doc, environment).await
}

// --- billing ---------------------------------------------------------------

/// What the workspace is entitled to, and whether it is paid for.
#[tauri::command]
pub async fn sync_billing(
    service: State<'_, RelayService>,
    workspace_id: WorkspaceId,
) -> RelayResult<relay_app::sync::BillingView> {
    service.sync_billing(workspace_id).await
}

/// Starts a checkout and opens it in the user's browser.
///
/// The same reasoning as the OAuth flow: card details belong on
/// Stripe's own page in a real browser, where the address bar is
/// visible, not inside a desktop app's webview.
#[tauri::command]
pub async fn sync_billing_checkout(
    app: tauri::AppHandle,
    service: State<'_, RelayService>,
    workspace_id: WorkspaceId,
    seats: Option<u32>,
) -> RelayResult<String> {
    let url = service.sync_billing_checkout(workspace_id, seats).await?;
    open_externally(&app, &url)?;
    Ok(url)
}

/// Opens Stripe's customer portal in the user's browser.
#[tauri::command]
pub async fn sync_billing_portal(
    app: tauri::AppHandle,
    service: State<'_, RelayService>,
    workspace_id: WorkspaceId,
) -> RelayResult<String> {
    let url = service.sync_billing_portal(workspace_id).await?;
    open_externally(&app, &url)?;
    Ok(url)
}

/// Hands a URL to the system browser.
///
/// The URL is returned to the caller as well as opened, so the panel
/// can offer it as a link when the browser does not appear — on a
/// bare Linux session there may be no handler registered at all.
fn open_externally(app: &tauri::AppHandle, url: &str) -> RelayResult<()> {
    use tauri_plugin_opener::OpenerExt;
    app.opener().open_url(url, None::<&str>).map_err(|e| {
        RelayError::new(
            relay_models::ErrorKind::Io,
            format!("could not open your browser: {e}"),
        )
    })
}

/// Changes what a member may do.
#[tauri::command]
pub async fn sync_set_member_role(
    service: State<'_, RelayService>,
    workspace_id: WorkspaceId,
    user_id: String,
    role: relay_app::sync::Role,
) -> RelayResult<()> {
    service
        .sync_set_member_role(workspace_id, user_id, role)
        .await
}

/// Removes someone from a workspace.
#[tauri::command]
pub async fn sync_remove_member(
    service: State<'_, RelayService>,
    workspace_id: WorkspaceId,
    user_id: String,
) -> RelayResult<()> {
    service.sync_remove_member(workspace_id, user_id).await
}

/// Invites an address to a workspace.
#[tauri::command]
pub async fn sync_invite(
    service: State<'_, RelayService>,
    workspace_id: WorkspaceId,
    email: String,
    role: relay_app::sync::Role,
) -> RelayResult<relay_app::sync::InvitationView> {
    service.sync_invite(workspace_id, email, role).await
}

/// Invitations sent but not yet accepted.
#[tauri::command]
pub async fn sync_invitations(
    service: State<'_, RelayService>,
    workspace_id: WorkspaceId,
) -> RelayResult<Vec<relay_app::sync::InvitationView>> {
    service.sync_invitations(workspace_id).await
}

/// Withdraws an invitation.
#[tauri::command]
pub async fn sync_revoke_invitation(
    service: State<'_, RelayService>,
    workspace_id: WorkspaceId,
    invitation_id: String,
) -> RelayResult<()> {
    service
        .sync_revoke_invitation(workspace_id, invitation_id)
        .await
}

// --- the assistant ---------------------------------------------------------

/// The channel carrying assistant progress to the webview.
pub const ASSISTANT_EVENT: &str = "relay://assistant";

/// Forwards assistant events to the window.
struct WindowEvents {
    app: tauri::AppHandle,
}

impl relay_app::assistant::EventSink for WindowEvents {
    fn emit(&self, event: relay_app::assistant::AssistantEvent) {
        let _ = self.app.emit(ASSISTANT_EVENT, event);
    }
}

/// The approvals waiting on an answer, shared across commands.
///
/// One set for the whole application rather than one per turn: the
/// command that answers arrives on its own, with only a call id to go
/// on, and ids are unique.
#[derive(Default)]
pub struct PendingApprovals(pub std::sync::Arc<relay_app::assistant::Approvals>);

/// Asks the assistant something.
///
/// Returns the conversation id, which the caller keeps for the next
/// question. Progress arrives on `relay://assistant` while this runs.
#[tauri::command]
pub async fn assistant_ask(
    app: tauri::AppHandle,
    service: State<'_, RelayService>,
    approvals: State<'_, PendingApprovals>,
    workspace_id: WorkspaceId,
    conversation_id: Option<String>,
    question: String,
    settings: relay_app::service::AssistantSettings,
) -> RelayResult<String> {
    service
        .assistant_ask(
            workspace_id,
            conversation_id,
            question,
            settings,
            approvals.0.clone(),
            Arc::new(WindowEvents { app }),
        )
        .await
}

/// Answers a tool call the assistant is waiting on.
#[tauri::command]
pub fn assistant_approve(
    approvals: State<'_, PendingApprovals>,
    call_id: String,
    allow: bool,
) -> bool {
    approvals.0.resolve(&call_id, allow)
}

/// Denies everything outstanding, for when a turn is abandoned.
#[tauri::command]
pub fn assistant_deny_all(approvals: State<'_, PendingApprovals>) {
    approvals.0.deny_all();
}

/// The conversations in a workspace.
#[tauri::command]
pub async fn assistant_conversations(
    service: State<'_, RelayService>,
    workspace_id: WorkspaceId,
) -> RelayResult<Vec<relay_storage::Conversation>> {
    service.assistant_conversations(workspace_id).await
}

/// Everything said in one conversation.
#[tauri::command]
pub async fn assistant_messages(
    service: State<'_, RelayService>,
    conversation_id: String,
) -> RelayResult<Vec<relay_storage::StoredMessage>> {
    service.assistant_messages(conversation_id).await
}

/// Deletes a conversation.
#[tauri::command]
pub async fn assistant_delete_conversation(
    service: State<'_, RelayService>,
    conversation_id: String,
) -> RelayResult<()> {
    service.assistant_delete_conversation(conversation_id).await
}

/// Stores an API key for a provider. The key never comes back out.
#[tauri::command]
pub fn assistant_set_key(
    service: State<'_, RelayService>,
    provider: String,
    key: String,
) -> RelayResult<()> {
    service.assistant_set_key(&provider, &key)
}

/// Forgets a provider's key.
#[tauri::command]
pub fn assistant_clear_key(service: State<'_, RelayService>, provider: String) -> RelayResult<()> {
    service.assistant_clear_key(&provider)
}

/// Whether a key is set, without disclosing it.
#[tauri::command]
pub fn assistant_has_key(service: State<'_, RelayService>, provider: String) -> bool {
    service.assistant_has_key(&provider)
}

/// The MCP servers the assistant may use.
#[tauri::command]
pub async fn assistant_mcp_servers(
    service: State<'_, RelayService>,
) -> RelayResult<Vec<relay_agent::McpServer>> {
    service.assistant_mcp_servers().await
}

/// Replaces the configured MCP servers.
#[tauri::command]
pub async fn assistant_set_mcp_servers(
    service: State<'_, RelayService>,
    servers: Vec<relay_agent::McpServer>,
) -> RelayResult<()> {
    service.assistant_set_mcp_servers(servers).await
}

/// Connects to one server and lists what it offers.
#[tauri::command]
pub async fn assistant_mcp_probe(
    service: State<'_, RelayService>,
    server: relay_agent::McpServer,
) -> RelayResult<Vec<String>> {
    service.assistant_mcp_probe(server).await
}
