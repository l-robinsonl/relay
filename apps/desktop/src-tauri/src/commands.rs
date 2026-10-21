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

use relay_app::{RelayService, ResolvedPreview};
use relay_core::import::ImportReport;
use relay_models::{
    Collection, CollectionId, Environment, EnvironmentId, ExecutionId, ExecutionProgress,
    HistoryEntry, HttpExchange, RelayError, RelayResult, RequestDoc, RequestId, TreeNode,
    Workspace, WorkspaceId,
};
use tauri::{Emitter, State};

/// The event channel carrying in-flight request progress to the webview.
pub const PROGRESS_EVENT: &str = "relay://exec-progress";

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
