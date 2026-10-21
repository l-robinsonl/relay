//! The application service: every operation the UI can invoke.
//!
//! Tauri commands are one-line wrappers around these methods. Keeping the
//! logic here means it is exercised by ordinary `cargo test` rather than only
//! by launching a desktop window.

use std::sync::Arc;

use relay_core::import::ImportReport;
use relay_core::portable::WorkspaceExport;
use relay_core::resolve::{ResolutionInput, build_context, resolve};
use relay_http::{HttpEngine, ProgressSink};
use relay_models::{
    Collection, CollectionId, Environment, EnvironmentId, ErrorKind, HistoryEntry, HistoryId,
    HttpExchange, HttpRequestSpec, RelayError, RelayResult, RequestDoc, RequestId, Timestamp,
    TreeNode, Workspace, WorkspaceId,
};
use relay_storage::{
    AppStateRepo, CollectionRepo, Database, EnvironmentRepo, HistoryRepo, RequestRepo, TreeRepo,
    WorkspaceRepo, state_keys,
};

use crate::secrets::SecretStore;

/// How many history entries are kept per workspace.
const HISTORY_LIMIT: u32 = 1000;

/// The result of resolving a request, as the UI sees it.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedPreview {
    /// The request that would be sent, with secrets left unresolved.
    pub spec: HttpRequestSpec,
    /// Variables that could not be resolved.
    pub missing_variables: Vec<String>,
}

/// The application's shared state.
#[derive(Clone)]
pub struct RelayService {
    db: Database,
    engine: HttpEngine,
    secrets: Arc<dyn SecretStore>,
}

impl std::fmt::Debug for RelayService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RelayService")
            .field("db", &self.db)
            .field("secrets", &self.secrets.backend_name())
            .finish()
    }
}

impl RelayService {
    /// Builds a service over the given database, engine and secret store.
    pub fn new(db: Database, engine: HttpEngine, secrets: Arc<dyn SecretStore>) -> Self {
        Self {
            db,
            engine,
            secrets,
        }
    }

    /// The underlying database, for maintenance operations.
    pub fn database(&self) -> &Database {
        &self.db
    }

    /// The HTTP engine, for cancellation.
    pub fn engine(&self) -> &HttpEngine {
        &self.engine
    }

    // --- workspaces --------------------------------------------------------

    /// Every workspace.
    pub async fn workspace_list(&self) -> RelayResult<Vec<Workspace>> {
        Ok(self.db.call(WorkspaceRepo::list).await?)
    }

    /// Creates a workspace and returns it.
    pub async fn workspace_create(&self, name: String) -> RelayResult<Workspace> {
        let workspace = Workspace::new(name);
        let saved = workspace.clone();
        self.db
            .call(move |c| WorkspaceRepo::save(c, &saved))
            .await?;
        Ok(workspace)
    }

    /// Saves a workspace.
    pub async fn workspace_save(&self, mut workspace: Workspace) -> RelayResult<Workspace> {
        workspace.updated_at = Timestamp::now();
        let saved = workspace.clone();
        self.db
            .call(move |c| WorkspaceRepo::save(c, &saved))
            .await?;
        Ok(workspace)
    }

    /// Deletes a workspace and everything in it.
    pub async fn workspace_delete(&self, id: WorkspaceId) -> RelayResult<()> {
        self.db.call(move |c| WorkspaceRepo::delete(c, &id)).await?;
        Ok(())
    }

    /// Ensures at least one workspace exists, creating a default if not.
    ///
    /// A first run must land on something usable rather than an empty shell
    /// with a "create a workspace" wall in front of it.
    pub async fn ensure_default_workspace(&self) -> RelayResult<Workspace> {
        let existing = self.workspace_list().await?;
        if let Some(first) = existing.into_iter().next() {
            return Ok(first);
        }
        self.workspace_create("My Workspace".into()).await
    }

    // --- tree and collections ---------------------------------------------

    /// The sidebar tree for a workspace.
    pub async fn collection_tree(&self, workspace: WorkspaceId) -> RelayResult<Vec<TreeNode>> {
        Ok(self.db.call(move |c| TreeRepo::tree(c, &workspace)).await?)
    }

    /// Every collection in a workspace.
    pub async fn collection_list(&self, workspace: WorkspaceId) -> RelayResult<Vec<Collection>> {
        Ok(self
            .db
            .call(move |c| CollectionRepo::list(c, &workspace))
            .await?)
    }

    /// Creates a collection or folder.
    pub async fn collection_create(
        &self,
        workspace: WorkspaceId,
        name: String,
        parent: Option<CollectionId>,
    ) -> RelayResult<Collection> {
        let mut collection = Collection::new(workspace, name);
        collection.parent_id = parent;
        let saved = collection.clone();
        self.db
            .call(move |c| CollectionRepo::save(c, &saved))
            .await?;
        Ok(collection)
    }

    /// Saves a collection.
    pub async fn collection_save(&self, mut collection: Collection) -> RelayResult<Collection> {
        collection.updated_at = Timestamp::now();
        let saved = collection.clone();
        self.db
            .call(move |c| CollectionRepo::save(c, &saved))
            .await?;
        Ok(collection)
    }

    /// Deletes a collection and its subtree.
    pub async fn collection_delete(&self, id: CollectionId) -> RelayResult<()> {
        self.db
            .call(move |c| CollectionRepo::delete(c, &id))
            .await?;
        Ok(())
    }

    // --- requests ----------------------------------------------------------

    /// Loads a request.
    pub async fn request_get(&self, id: RequestId) -> RelayResult<RequestDoc> {
        Ok(self.db.call(move |c| RequestRepo::get(c, &id)).await?)
    }

    /// Every request in a workspace.
    pub async fn request_list(&self, workspace: WorkspaceId) -> RelayResult<Vec<RequestDoc>> {
        Ok(self
            .db
            .call(move |c| RequestRepo::list(c, &workspace))
            .await?)
    }

    /// Creates a request.
    pub async fn request_create(
        &self,
        workspace: WorkspaceId,
        name: String,
        collection: Option<CollectionId>,
    ) -> RelayResult<RequestDoc> {
        let mut doc = RequestDoc::new(workspace, name);
        doc.collection_id = collection;
        let saved = doc.clone();
        self.db.call(move |c| RequestRepo::save(c, &saved)).await?;
        Ok(doc)
    }

    /// Saves a request, creating it if it does not exist.
    pub async fn request_save(&self, mut doc: RequestDoc) -> RelayResult<RequestDoc> {
        doc.updated_at = Timestamp::now();
        let saved = doc.clone();
        self.db.call(move |c| RequestRepo::save(c, &saved)).await?;
        Ok(doc)
    }

    /// Deletes a request.
    pub async fn request_delete(&self, id: RequestId) -> RelayResult<()> {
        self.db.call(move |c| RequestRepo::delete(c, &id)).await?;
        Ok(())
    }

    /// Duplicates a request.
    pub async fn request_duplicate(&self, id: RequestId) -> RelayResult<RequestDoc> {
        Ok(self
            .db
            .call(move |c| RequestRepo::duplicate(c, &id))
            .await?)
    }

    /// Moves a request into a collection.
    pub async fn request_move(
        &self,
        id: RequestId,
        collection: Option<CollectionId>,
        sort_order: f64,
    ) -> RelayResult<()> {
        self.db
            .call(move |c| RequestRepo::move_to(c, &id, collection.as_ref(), sort_order))
            .await?;
        Ok(())
    }

    // --- environments ------------------------------------------------------

    /// Every environment in a workspace.
    pub async fn environment_list(&self, workspace: WorkspaceId) -> RelayResult<Vec<Environment>> {
        Ok(self
            .db
            .call(move |c| EnvironmentRepo::list(c, &workspace))
            .await?)
    }

    /// Creates an environment.
    pub async fn environment_create(
        &self,
        workspace: WorkspaceId,
        name: String,
    ) -> RelayResult<Environment> {
        let environment = Environment::new(workspace, name);
        let saved = environment.clone();
        self.db
            .call(move |c| EnvironmentRepo::save(c, &saved))
            .await?;
        Ok(environment)
    }

    /// Saves an environment. Secret values are not persisted here.
    pub async fn environment_save(&self, mut environment: Environment) -> RelayResult<Environment> {
        environment.updated_at = Timestamp::now();
        let saved = environment.clone();
        self.db
            .call(move |c| EnvironmentRepo::save(c, &saved))
            .await?;
        Ok(environment.redacted())
    }

    /// Deletes an environment.
    pub async fn environment_delete(&self, id: EnvironmentId) -> RelayResult<()> {
        self.db
            .call(move |c| EnvironmentRepo::delete(c, &id))
            .await?;
        Ok(())
    }

    /// Stores a secret variable value in the credential store.
    pub fn secret_set(
        &self,
        environment: &EnvironmentId,
        key: &str,
        value: &str,
    ) -> RelayResult<()> {
        self.secrets
            .set(environment, key, value)
            .map_err(|e| RelayError::new(ErrorKind::SecretStore, e))
    }

    /// Removes a secret variable value.
    pub fn secret_delete(&self, environment: &EnvironmentId, key: &str) -> RelayResult<()> {
        self.secrets
            .delete(environment, key)
            .map_err(|e| RelayError::new(ErrorKind::SecretStore, e))
    }

    /// Whether a secret has a value stored, without revealing it.
    pub fn secret_is_set(&self, environment: &EnvironmentId, key: &str) -> bool {
        self.secrets.get(environment, key).is_some()
    }

    /// The name of the credential backend in use.
    pub fn secret_backend(&self) -> &'static str {
        self.secrets.backend_name()
    }

    // --- resolution and execution -----------------------------------------

    /// Resolves a request without sending it, for the URL preview.
    ///
    /// Secrets are deliberately left unsubstituted: this output is displayed
    /// in the UI and may be copied, so it must not contain credentials.
    pub async fn request_resolve(
        &self,
        doc: RequestDoc,
        environment: Option<EnvironmentId>,
    ) -> RelayResult<ResolvedPreview> {
        let context_data = self.load_scope(&doc, environment.clone()).await?;
        let input = context_data.as_input();
        let context = build_context(&input);
        let resolution = resolve(&doc, &input, &context);

        Ok(ResolvedPreview {
            spec: resolution.spec,
            missing_variables: resolution.missing_variables,
        })
    }

    /// Resolves and sends a request, recording it in history.
    pub async fn request_send(
        &self,
        execution_id: relay_models::ExecutionId,
        doc: RequestDoc,
        environment: Option<EnvironmentId>,
        progress: Arc<dyn ProgressSink>,
    ) -> RelayResult<HttpExchange> {
        let context_data = self.load_scope(&doc, environment.clone()).await?;
        let input = context_data.as_input();
        let mut context = build_context(&input);

        // Secrets are injected here and nowhere else: they exist in memory
        // for the duration of this call and are never written back.
        if let Some(environment_id) = &environment {
            let resolution = resolve(&doc, &input, &context);
            for key in &resolution.required_secrets {
                if let Some(value) = self.secrets.get(environment_id, key) {
                    context.set_secret(key, value);
                }
            }
        }

        let resolution = resolve(&doc, &input, &context);

        if !resolution.missing_variables.is_empty() {
            return Err(RelayError::new(
                ErrorKind::Interpolation,
                format!(
                    "unresolved variable{}: {}",
                    if resolution.missing_variables.len() == 1 {
                        ""
                    } else {
                        "s"
                    },
                    resolution.missing_variables.join(", ")
                ),
            )
            .with_detail(
                "Define them in the active environment, the collection, or the workspace."
                    .to_string(),
            ));
        }

        let spec = resolution.spec;
        let exchange = self.engine.send(execution_id, spec.clone(), progress).await;

        self.record_history(&doc, &spec, &exchange).await;

        Ok(exchange)
    }

    /// Cancels an in-flight request.
    pub fn request_cancel(&self, execution_id: &relay_models::ExecutionId) -> bool {
        self.engine.cancel(execution_id)
    }

    async fn record_history(
        &self,
        doc: &RequestDoc,
        spec: &HttpRequestSpec,
        exchange: &HttpExchange,
    ) {
        let (status, status_text, duration_ms, size, error) = match exchange {
            HttpExchange::Success { response, .. } => (
                Some(response.status),
                Some(response.status_text.clone()),
                response.timings.total_ms,
                response.size_bytes,
                None,
            ),
            HttpExchange::Failure {
                error, elapsed_ms, ..
            } => (None, None, *elapsed_ms, 0, Some(error.message.clone())),
            HttpExchange::Cancelled { elapsed_ms, .. } => {
                (None, None, *elapsed_ms, 0, Some("Cancelled".to_string()))
            }
        };

        let entry = HistoryEntry {
            id: HistoryId::new(),
            workspace_id: doc.workspace_id.clone(),
            request_id: Some(doc.id.clone()),
            name: doc.name.clone(),
            method: spec.method.clone(),
            url: spec.url.clone(),
            status,
            status_text,
            duration_ms,
            response_size: size,
            executed_at: Timestamp::now(),
            error,
            // Redacted here and again inside the repository.
            request_snapshot: Box::new(spec.redacted()),
        };

        let workspace = doc.workspace_id.clone();
        // A history write must never fail a request the user already made.
        if let Err(err) = self
            .db
            .call(move |c| {
                HistoryRepo::insert(c, &entry)?;
                HistoryRepo::prune(c, &workspace, HISTORY_LIMIT)?;
                Ok(())
            })
            .await
        {
            tracing::warn!("could not record history: {err}");
        }
    }

    async fn load_scope(
        &self,
        doc: &RequestDoc,
        environment: Option<EnvironmentId>,
    ) -> RelayResult<ScopeData> {
        let workspace_id = doc.workspace_id.clone();
        let collection_id = doc.collection_id.clone();

        let workspace = self
            .db
            .call({
                let id = workspace_id.clone();
                move |c| WorkspaceRepo::get(c, &id)
            })
            .await
            .ok();

        // Walk from the request's folder up to the root, then reverse so the
        // chain runs outermost-first, which is the order precedence needs.
        let folder_chain = match collection_id {
            None => Vec::new(),
            Some(start) => {
                self.db
                    .call(move |c| {
                        let mut chain = Vec::new();
                        let mut cursor = Some(start);
                        let mut hops = 0;
                        while let Some(id) = cursor {
                            hops += 1;
                            if hops > 100 {
                                break;
                            }
                            match CollectionRepo::get(c, &id) {
                                Ok(collection) => {
                                    cursor = collection.parent_id.clone();
                                    chain.push(collection);
                                }
                                Err(_) => break,
                            }
                        }
                        chain.reverse();
                        Ok(chain)
                    })
                    .await?
            }
        };

        let environment = match environment {
            None => None,
            Some(id) => self
                .db
                .call(move |c| EnvironmentRepo::get(c, &id))
                .await
                .ok(),
        };

        Ok(ScopeData {
            workspace,
            folder_chain,
            environment,
        })
    }

    // --- history -----------------------------------------------------------

    /// Recent executions, newest first.
    pub async fn history_list(
        &self,
        workspace: WorkspaceId,
        limit: u32,
        offset: u32,
    ) -> RelayResult<Vec<HistoryEntry>> {
        Ok(self
            .db
            .call(move |c| HistoryRepo::list(c, &workspace, limit, offset))
            .await?)
    }

    /// Clears a workspace's history.
    pub async fn history_clear(&self, workspace: WorkspaceId) -> RelayResult<usize> {
        Ok(self
            .db
            .call(move |c| HistoryRepo::clear(c, &workspace))
            .await?)
    }

    // --- import and export --------------------------------------------------

    /// Imports a cURL command into a new request document.
    ///
    /// The document is returned unsaved so the user can review it first.
    pub fn import_curl(&self, command: &str, workspace: WorkspaceId) -> RelayResult<RequestDoc> {
        Ok(relay_core::import_curl(command, workspace)?)
    }

    /// Parses a Postman collection or environment without saving anything.
    ///
    /// The caller shows the report, including everything that could not be
    /// brought across, and only calls [`Self::import_apply`] if the user
    /// accepts it. Importing 200 requests and finding out afterwards that
    /// the auth did not survive is the failure mode this avoids.
    pub fn import_postman(&self, json: &str, workspace: WorkspaceId) -> RelayResult<ImportReport> {
        Ok(relay_core::postman::import_any(json, workspace)?)
    }

    /// Parses a Relay workspace export without saving anything.
    pub fn import_relay(&self, json: &str, workspace: WorkspaceId) -> RelayResult<ImportReport> {
        Ok(WorkspaceExport::from_json(json)?.into_report(&workspace))
    }

    /// Reads a file of either kind, choosing the importer by its content.
    pub fn import_detect(&self, json: &str, workspace: WorkspaceId) -> RelayResult<ImportReport> {
        // A Relay export is self-identifying, so try it first and fall back.
        match WorkspaceExport::from_json(json) {
            Ok(export) => Ok(export.into_report(&workspace)),
            Err(_) => Ok(relay_core::postman::import_any(json, workspace)?),
        }
    }

    /// Writes an accepted import to the database.
    ///
    /// Everything lands in one transaction: a half-applied import would leave
    /// folders with no requests and no obvious way to tell what was missing.
    pub async fn import_apply(&self, report: ImportReport) -> RelayResult<usize> {
        let count = report.object_count();

        self.db
            .call_transaction(move |tx| {
                // Collections first, and parents before children, so a
                // foreign key never points at a row that is not there yet.
                for collection in &report.collections {
                    CollectionRepo::save(tx, collection)?;
                }
                for request in &report.requests {
                    RequestRepo::save(tx, request)?;
                }
                for environment in &report.environments {
                    EnvironmentRepo::save(tx, environment)?;
                }
                Ok(())
            })
            .await?;

        Ok(count)
    }

    /// Largest import file Relay will read.
    ///
    /// A Postman collection with thousands of requests is a few megabytes;
    /// anything past this is a mistake or an attempt to exhaust memory.
    pub const MAX_IMPORT_BYTES: u64 = 64 * 1024 * 1024;

    /// Reads an import file from disk and previews it.
    ///
    /// The file is parsed and summarised; its contents are never returned to
    /// the caller, so this cannot be used to read an arbitrary file.
    pub async fn import_preview_path(
        &self,
        path: String,
        workspace: WorkspaceId,
    ) -> RelayResult<ImportReport> {
        let metadata = tokio::fs::metadata(&path).await.map_err(|e| {
            RelayError::new(ErrorKind::Io, format!("could not open that file: {e}"))
        })?;

        if !metadata.is_file() {
            return Err(RelayError::new(
                ErrorKind::Invalid,
                "that is a directory, not a file",
            ));
        }
        if metadata.len() > Self::MAX_IMPORT_BYTES {
            return Err(RelayError::new(
                ErrorKind::Invalid,
                format!(
                    "that file is {} MB, larger than the {} MB import limit",
                    metadata.len() / 1_048_576,
                    Self::MAX_IMPORT_BYTES / 1_048_576
                ),
            ));
        }

        let content = tokio::fs::read_to_string(&path).await.map_err(|e| {
            RelayError::new(
                ErrorKind::Io,
                format!("could not read that file as text: {e}"),
            )
        })?;

        self.import_detect(&content, workspace)
    }

    /// Writes a workspace export to a file the user chose.
    pub async fn export_workspace_to_path(
        &self,
        workspace_id: WorkspaceId,
        path: String,
    ) -> RelayResult<()> {
        let json = self.export_workspace(workspace_id).await?;
        tokio::fs::write(&path, json)
            .await
            .map_err(|e| RelayError::new(ErrorKind::Io, format!("could not write that file: {e}")))
    }

    /// Builds a portable export of an entire workspace.
    pub async fn export_workspace(&self, workspace_id: WorkspaceId) -> RelayResult<String> {
        let workspace = {
            let id = workspace_id.clone();
            self.db.call(move |c| WorkspaceRepo::get(c, &id)).await?
        };
        let collections = {
            let id = workspace_id.clone();
            self.db.call(move |c| CollectionRepo::list(c, &id)).await?
        };
        let requests = {
            let id = workspace_id.clone();
            self.db.call(move |c| RequestRepo::list(c, &id)).await?
        };
        let environments = self
            .db
            .call(move |c| EnvironmentRepo::list(c, &workspace_id))
            .await?;

        Ok(WorkspaceExport::build(&workspace, &collections, &requests, &environments).to_json()?)
    }

    /// Renders a request as a cURL command.
    pub async fn export_curl(
        &self,
        doc: RequestDoc,
        environment: Option<EnvironmentId>,
    ) -> RelayResult<String> {
        let preview = self.request_resolve(doc, environment).await?;
        Ok(relay_core::export_curl(&preview.spec))
    }

    // --- app state ----------------------------------------------------------

    /// Reads a UI state value.
    pub async fn state_get(&self, key: String) -> RelayResult<Option<String>> {
        Ok(self.db.call(move |c| AppStateRepo::get(c, &key)).await?)
    }

    /// Writes a UI state value.
    pub async fn state_set(&self, key: String, value: String) -> RelayResult<()> {
        self.db
            .call(move |c| AppStateRepo::set(c, &key, &value))
            .await?;
        Ok(())
    }

    /// The workspace to show on startup.
    pub async fn active_workspace(&self) -> RelayResult<Workspace> {
        if let Some(id) = self.state_get(state_keys::ACTIVE_WORKSPACE.into()).await? {
            let workspace_id = WorkspaceId::from(id);
            if let Ok(workspace) = self
                .db
                .call(move |c| WorkspaceRepo::get(c, &workspace_id))
                .await
            {
                return Ok(workspace);
            }
        }
        self.ensure_default_workspace().await
    }
}

/// Owned scope data, borrowed into a [`ResolutionInput`].
struct ScopeData {
    workspace: Option<Workspace>,
    folder_chain: Vec<Collection>,
    environment: Option<Environment>,
}

impl ScopeData {
    fn as_input(&self) -> ResolutionInput<'_> {
        ResolutionInput {
            workspace: self.workspace.as_ref(),
            folder_chain: self.folder_chain.iter().collect(),
            environment: self.environment.as_ref(),
        }
    }
}
