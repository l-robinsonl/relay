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
    HttpExchange, HttpRequestSpec, OAuth2Config, RelayError, RelayResult, RequestDoc, RequestId,
    Timestamp, TreeNode, Workspace, WorkspaceId,
};
use relay_script::{ScriptEngine, ScriptOutcome, ScriptRequest, ScriptResponse};
use relay_storage::{
    AppStateRepo, CollectionRepo, Database, EnvironmentRepo, HistoryRepo, RequestRepo, TreeRepo,
    WorkspaceRepo, state_keys,
};

use crate::oauth::{self, TokenCache};
use crate::secrets::SecretStore;
use crate::sync::{
    Resolution, SyncClient, SyncEngine, SyncOutcome, WatchEvent, Watcher, initial_state,
};

/// How many history entries are kept per workspace.
const HISTORY_LIMIT: u32 = 1000;

/// An exchange together with whatever its tests reported.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SendOutcome {
    /// What the HTTP engine returned.
    pub exchange: HttpExchange,
    /// What the post-response script reported, if there was one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tests: Option<ScriptOutcome>,
    /// What the pre-request script reported, if there was one. Carries
    /// its console output, so an author can see what it did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pre_request: Option<ScriptOutcome>,
}

/// What the Auth tab knows about a held OAuth token.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OAuthTokenStatus {
    /// Whether a token is held for this configuration.
    pub has_token: bool,
    /// When it lapses, if the server said.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<Timestamp>,
    /// What was actually granted, which may be less than was asked for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    /// Whether it can be renewed without the browser.
    pub can_refresh: bool,
}

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
    tokens: Arc<TokenCache>,
}

impl std::fmt::Debug for RelayService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RelayService")
            .field("db", &self.db)
            .field("secrets", &self.secrets.backend_name())
            .field("tokens", &self.tokens)
            .finish()
    }
}

/// The variables a script is allowed to see.
///
/// Secret values are excluded by the `!v.secret` filter, which is the
/// whole of the rule: a script never receives one, so it cannot read,
/// print or forward a credential.
fn readable_variables(input: &ResolutionInput<'_>) -> std::collections::BTreeMap<String, String> {
    let mut variables = std::collections::BTreeMap::new();

    let mut absorb = |list: &[relay_models::Variable]| {
        for variable in list.iter().filter(|v| v.enabled && !v.secret) {
            variables.insert(variable.key.clone(), variable.value.clone());
        }
    };

    if let Some(workspace) = input.workspace {
        absorb(&workspace.variables);
    }
    for collection in &input.folder_chain {
        absorb(&collection.variables);
    }
    if let Some(environment) = input.environment {
        absorb(&environment.variables);
    }

    variables
}

impl RelayService {
    /// Builds a service over the given database, engine and secret store.
    pub fn new(db: Database, engine: HttpEngine, secrets: Arc<dyn SecretStore>) -> Self {
        Self {
            db,
            engine,
            secrets,
            tokens: Arc::new(TokenCache::new()),
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

    /// What the UI shows about a request's OAuth token.
    ///
    /// Carries no token value. The Auth tab needs to say whether a token
    /// is held and when it lapses; it has no reason to display the
    /// credential itself, and putting one in an IPC payload would spread
    /// it into places that are harder to account for.
    pub fn oauth_token_status(&self, config: &OAuth2Config) -> OAuthTokenStatus {
        match self.tokens.get(config) {
            None => OAuthTokenStatus::default(),
            Some(token) => OAuthTokenStatus {
                has_token: true,
                expires_at: token.expires_at,
                scope: token.scope,
                can_refresh: token
                    .refresh_token
                    .as_deref()
                    .is_some_and(|t| !t.trim().is_empty()),
            },
        }
    }

    /// Obtains a token without a browser, for the grants that allow it.
    ///
    /// Returns the status rather than the token, for the reason above.
    pub async fn oauth_fetch_token(&self, config: OAuth2Config) -> RelayResult<OAuthTokenStatus> {
        let mut config = config;
        // An expired or stale token on the config would otherwise be
        // taken as good: pressing this button means "get me a new one".
        config.access_token = None;
        config.expires_at = None;
        self.tokens.forget(&config);

        oauth::ensure_token(&self.tokens, &mut config).await?;
        Ok(self.oauth_token_status(&config))
    }

    /// Runs the authorization code flow, opening a browser via `open`.
    ///
    /// The caller supplies the opening because only the desktop layer
    /// knows how; the listener is already bound before `open` is called,
    /// so a browser that responds instantly cannot arrive too early.
    pub async fn oauth_authorize<F>(
        &self,
        config: OAuth2Config,
        open: F,
    ) -> RelayResult<OAuthTokenStatus>
    where
        F: FnOnce(&str) -> RelayResult<()>,
    {
        let flow = oauth::begin_authorization(&config)?.start().await?;

        open(&flow.url)?;

        let config = flow.complete(&self.tokens).await?;
        Ok(self.oauth_token_status(&config))
    }

    /// Discards the token held for a configuration.
    pub fn oauth_forget_token(&self, config: &OAuth2Config) {
        self.tokens.forget(config);
    }

    /// Discards every held token.
    pub fn oauth_forget_all_tokens(&self) {
        self.tokens.forget_all();
    }

    /// Resolves and sends a request, recording it in history.
    pub async fn request_send(
        &self,
        execution_id: relay_models::ExecutionId,
        doc: RequestDoc,
        environment: Option<EnvironmentId>,
        progress: Arc<dyn ProgressSink>,
    ) -> RelayResult<HttpExchange> {
        self.send(execution_id, doc, environment, progress)
            .await
            .map(|(exchange, _)| exchange)
    }

    /// The send itself, keeping what the pre-request script reported.
    async fn send(
        &self,
        execution_id: relay_models::ExecutionId,
        doc: RequestDoc,
        environment: Option<EnvironmentId>,
        progress: Arc<dyn ProgressSink>,
    ) -> RelayResult<(HttpExchange, Option<ScriptOutcome>)> {
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

        // The pre-request script runs here: after secrets are in the
        // context, so it sees the request as it would actually be sent,
        // and before the missing-variable check, so a variable it
        // provides counts as provided.
        let pre_request = self.run_pre_request(&doc, &input, &mut context).await?;

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

        let mut spec = resolution.spec;

        // After resolution, so the values a script wrote are used exactly
        // as written. See `RequestPatch`.
        if let Some(outcome) = &pre_request {
            spec.apply_patch(&outcome.request_patch);
        }

        // The token goes onto the spec that is about to be sent, never
        // onto the stored document: `spec` is a resolution of `doc` and
        // is dropped when this call returns, so no access token can
        // reach SQLite, an export or a sync payload.
        oauth::prepare_auth(&self.tokens, &mut spec.auth).await?;

        let exchange = self.engine.send(execution_id, spec.clone(), progress).await;

        self.record_history(&doc, &spec, &exchange).await;

        Ok((exchange, pre_request))
    }

    /// Runs the pre-request script, if there is one.
    ///
    /// Variables it sets are added to the context for this send alone.
    /// They are deliberately not written back as workspace variables:
    /// this script runs on every send, and persisting a nonce each time
    /// would churn the workspace and push a revision per request. A
    /// value worth keeping is captured by the post-response script of
    /// whichever request produced it.
    async fn run_pre_request(
        &self,
        doc: &RequestDoc,
        input: &ResolutionInput<'_>,
        context: &mut relay_core::interpolate::VariableContext,
    ) -> RelayResult<Option<ScriptOutcome>> {
        let source = doc.scripts.pre_request.clone();
        if source.trim().is_empty() {
            return Ok(None);
        }

        // Resolved first, so the script reads real values rather than
        // `{{baseUrl}}`. Redacted on the way in, so it cannot read a
        // credential that is already on the request.
        let preview = resolve(doc, input, context);
        let variables = readable_variables(input);

        let outcome = ScriptEngine::new().run_pre_request(
            &source,
            Some(&ScriptRequest::from_spec(&preview.spec)),
            &variables,
        );

        // A pre-request script usually exists to produce something the
        // request needs — a signature, a timestamp, a token. Sending
        // anyway would turn a clear script error into a puzzling 401.
        if let Some(error) = &outcome.error {
            return Err(RelayError::new(
                ErrorKind::Invalid,
                format!("the pre-request script failed: {error}"),
            )
            .with_detail("The request was not sent.".to_string()));
        }

        for (key, value) in &outcome.variables {
            context.set_literal(key, value);
        }

        Ok(Some(outcome))
    }

    /// Sends a request and runs its tests against the response.
    ///
    /// A failing test never turns a successful exchange into an error:
    /// the response is what the user asked for, and the verdict on it is
    /// separate information shown beside it.
    pub async fn request_send_with_tests(
        &self,
        execution_id: relay_models::ExecutionId,
        doc: RequestDoc,
        environment: Option<EnvironmentId>,
        progress: Arc<dyn ProgressSink>,
    ) -> RelayResult<SendOutcome> {
        let script = doc.scripts.post_response.clone();
        let (exchange, pre_request) = self
            .send(execution_id, doc.clone(), environment.clone(), progress)
            .await?;

        if script.trim().is_empty() {
            return Ok(SendOutcome {
                exchange,
                tests: None,
                pre_request,
            });
        }

        // Only a response can be asserted about. A failed or cancelled
        // exchange has nothing to test, and running the script anyway
        // would produce confusing errors about `response` being absent.
        let HttpExchange::Success {
            response, request, ..
        } = &exchange
        else {
            return Ok(SendOutcome {
                exchange,
                tests: None,
                pre_request,
            });
        };

        let variables = self.script_variables(&doc, environment.clone()).await;

        let outcome = ScriptEngine::new().run(
            &script,
            Some(&ScriptResponse::from_response(response)),
            Some(&ScriptRequest::from_spec(request)),
            &variables,
        );

        // Variables a script set are written back as ordinary workspace
        // variables. Nothing a script writes becomes a secret: it has no
        // way to reach the credential store, and promoting its output
        // into one would be a route around that.
        if !outcome.variables.is_empty() {
            self.apply_script_variables(&doc.workspace_id, &outcome.variables)
                .await;
        }

        Ok(SendOutcome {
            exchange,
            tests: Some(outcome),
            pre_request,
        })
    }

    /// The non-secret variables a script may read.
    async fn script_variables(
        &self,
        doc: &RequestDoc,
        environment: Option<EnvironmentId>,
    ) -> std::collections::BTreeMap<String, String> {
        let Ok(scope) = self.load_scope(doc, environment).await else {
            return Default::default();
        };

        readable_variables(&scope.as_input())
    }

    /// Writes variables a script set back to the workspace.
    async fn apply_script_variables(
        &self,
        workspace_id: &WorkspaceId,
        variables: &std::collections::BTreeMap<String, String>,
    ) {
        let id = workspace_id.clone();
        let updates = variables.clone();

        let result = self
            .db
            .call(move |conn| {
                let mut workspace = WorkspaceRepo::get(conn, &id)?;
                for (key, value) in &updates {
                    match workspace.variables.iter_mut().find(|v| &v.key == key) {
                        // A secret variable is never overwritten by a
                        // script: that would let one replace a credential
                        // with a value of its choosing.
                        Some(existing) if existing.secret => {}
                        Some(existing) => existing.value = value.clone(),
                        None => workspace
                            .variables
                            .push(relay_models::Variable::new(key, value)),
                    }
                }
                workspace.updated_at = Timestamp::now();
                WorkspaceRepo::save(conn, &workspace)
            })
            .await;

        if let Err(error) = result {
            tracing::warn!("could not apply variables a script set: {error}");
        }
    }

    /// Runs a script against the last response, without sending anything.
    ///
    /// Lets someone iterate on assertions without hammering an endpoint.
    pub fn run_script(
        &self,
        source: &str,
        response: Option<&relay_models::HttpResponseData>,
    ) -> ScriptOutcome {
        ScriptEngine::new().run(
            source,
            response.map(ScriptResponse::from_response).as_ref(),
            None,
            &Default::default(),
        )
    }

    /// Discards every stored cookie.
    ///
    /// Cookies are shared across requests so that a login followed by a
    /// call works. The cost is that a stale session cookie then breaks
    /// every later request in a way that is hard to spot, so there has to
    /// be a way to throw them away.
    pub fn clear_cookies(&self) {
        self.engine.clear_cookies();
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

    /// Parses an OpenAPI 3.x document without saving anything.
    ///
    /// Accepts JSON or YAML; the content decides, because the extension
    /// on a specification is frequently wrong or absent.
    pub fn import_openapi(
        &self,
        source: &str,
        workspace: WorkspaceId,
    ) -> RelayResult<ImportReport> {
        Ok(relay_core::openapi::import_openapi(source, workspace)?)
    }

    /// Parses a Relay workspace export without saving anything.
    pub fn import_relay(&self, json: &str, workspace: WorkspaceId) -> RelayResult<ImportReport> {
        Ok(WorkspaceExport::from_json(json)?.into_report(&workspace))
    }

    /// Reads a file of either kind, choosing the importer by its content.
    pub fn import_detect(&self, source: &str, workspace: WorkspaceId) -> RelayResult<ImportReport> {
        // Each format announces itself — a Relay export has `format`,
        // OpenAPI has `openapi`, Postman has `item` or `values` — so the
        // checks are cheap and unambiguous, and only a file matching none
        // of them produces an error.
        if let Ok(export) = WorkspaceExport::from_json(source) {
            return Ok(export.into_report(&workspace));
        }

        if looks_like_openapi(source) {
            return self.import_openapi(source, workspace);
        }

        relay_core::postman::import_any(source, workspace).map_err(|postman_error| {
            RelayError::new(
                ErrorKind::Import,
                "this file is not a Relay export, an OpenAPI 3 document, or a Postman \
                 collection or environment",
            )
            .with_detail(postman_error.to_string())
        })
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

    // --- synchronisation --------------------------------------------------

    /// A sync engine over this service's database and credential store.
    pub fn sync_engine(&self) -> SyncEngine {
        SyncEngine::new(self.db.clone(), Arc::clone(&self.secrets))
    }

    /// Checks a server is reachable and speaks a protocol we understand.
    pub async fn sync_probe(&self, server_url: String) -> RelayResult<crate::sync::ServerHealth> {
        SyncClient::new(server_url)?.health().await
    }

    /// Connects a workspace to a server, signing in and creating it there.
    ///
    /// `join` adopts a workspace that already exists on the server rather
    /// than publishing the local one, which is what a second device does.
    /// The server and token a connected workspace syncs with.
    ///
    /// Every membership call needs both, and each would otherwise repeat
    /// the same two lookups and the same two ways of being wrong.
    async fn sync_session(&self, workspace: &WorkspaceId) -> RelayResult<(SyncClient, String)> {
        let id = workspace.clone();
        let state = self
            .db
            .call(move |c| relay_storage::SyncRepo::state(c, &id))
            .await?
            .ok_or_else(|| {
                RelayError::new(
                    ErrorKind::Sync,
                    "this workspace is not connected to a server",
                )
            })?;

        let token = self.sync_engine().token(workspace).ok_or_else(|| {
            RelayError::new(ErrorKind::Forbidden, "you are signed out of that server")
                .with_detail("Reconnect the workspace under Sync.".to_string())
        })?;

        Ok((SyncClient::new(&state.server_url)?, token))
    }

    /// Who is in a workspace, and as what.
    pub async fn sync_members(
        &self,
        workspace: WorkspaceId,
    ) -> RelayResult<Vec<relay_sync::MemberView>> {
        let (client, token) = self.sync_session(&workspace).await?;
        client.list_members(&token, workspace.as_str()).await
    }

    /// Changes what a member may do.
    pub async fn sync_set_member_role(
        &self,
        workspace: WorkspaceId,
        user_id: String,
        role: relay_sync::Role,
    ) -> RelayResult<()> {
        let (client, token) = self.sync_session(&workspace).await?;
        client
            .set_member_role(&token, workspace.as_str(), &user_id, role)
            .await
    }

    /// Removes someone from a workspace.
    pub async fn sync_remove_member(
        &self,
        workspace: WorkspaceId,
        user_id: String,
    ) -> RelayResult<()> {
        let (client, token) = self.sync_session(&workspace).await?;
        client
            .remove_member(&token, workspace.as_str(), &user_id)
            .await
    }

    /// Invites an address to a workspace.
    pub async fn sync_invite(
        &self,
        workspace: WorkspaceId,
        email: String,
        role: relay_sync::Role,
    ) -> RelayResult<relay_sync::InvitationView> {
        let (client, token) = self.sync_session(&workspace).await?;
        client
            .invite(&token, workspace.as_str(), &email, role)
            .await
    }

    /// Invitations sent but not yet accepted.
    pub async fn sync_invitations(
        &self,
        workspace: WorkspaceId,
    ) -> RelayResult<Vec<relay_sync::InvitationView>> {
        let (client, token) = self.sync_session(&workspace).await?;
        client.list_invitations(&token, workspace.as_str()).await
    }

    /// Withdraws an invitation.
    pub async fn sync_revoke_invitation(
        &self,
        workspace: WorkspaceId,
        invitation_id: String,
    ) -> RelayResult<()> {
        let (client, token) = self.sync_session(&workspace).await?;
        client
            .revoke_invitation(&token, workspace.as_str(), &invitation_id)
            .await
    }

    /// Creates an account on a sync server.
    ///
    /// Separate from connecting: someone may want an account before they
    /// have decided which workspace to share, and registration can
    /// succeed while sign-in is still blocked pending confirmation.
    pub async fn sync_register(
        &self,
        server_url: String,
        email: String,
        password: String,
        display_name: String,
    ) -> RelayResult<relay_sync::UserProfile> {
        let client = SyncClient::new(&server_url)?;
        client.health().await?;

        let display_name = if display_name.trim().is_empty() {
            email.split('@').next().unwrap_or("User").to_string()
        } else {
            display_name
        };

        client.register(&email, &password, &display_name).await
    }

    /// Asks a server to send another confirmation e-mail.
    pub async fn sync_request_verification(
        &self,
        server_url: String,
        email: String,
    ) -> RelayResult<()> {
        SyncClient::new(&server_url)?
            .request_verification(&email)
            .await
    }

    /// Asks a server to send a password reset e-mail.
    pub async fn sync_request_password_reset(
        &self,
        server_url: String,
        email: String,
    ) -> RelayResult<()> {
        SyncClient::new(&server_url)?
            .request_password_reset(&email)
            .await
    }

    /// Connects a workspace to a server, signing in and creating it there.
    ///
    /// `join` adopts a workspace that already exists on the server rather
    /// than publishing the local one, which is what a second device does.
    pub async fn sync_connect(
        &self,
        workspace: WorkspaceId,
        server_url: String,
        email: String,
        password: String,
        join: bool,
    ) -> RelayResult<SyncStatus> {
        let client = SyncClient::new(&server_url)?;
        client.health().await?;

        let login = client.login(&email, &password).await?;
        let engine = self.sync_engine();
        engine.set_token(&workspace, &login.token)?;

        if join {
            engine.prepare_join(&workspace).await?;
        } else {
            let name = {
                let id = workspace.clone();
                self.db
                    .call(move |c| WorkspaceRepo::get(c, &id))
                    .await?
                    .name
            };
            client
                .create_workspace(&login.token, workspace.as_str(), &name)
                .await?;
        }

        let state = initial_state(&workspace, &server_url, None, &login.user);
        self.db
            .call(move |c| relay_storage::SyncRepo::set_state(c, &state))
            .await?;

        self.sync_status(workspace).await
    }

    /// Runs one synchronisation pass.
    pub async fn sync_now(&self, workspace: WorkspaceId) -> RelayResult<SyncOutcome> {
        self.sync_engine().sync(workspace).await
    }

    /// Stops a workspace synchronising. Local data is untouched.
    pub async fn sync_disconnect(&self, workspace: WorkspaceId) -> RelayResult<()> {
        let engine = self.sync_engine();
        let _ = engine.clear_token(&workspace);

        let id = workspace.clone();
        self.db
            .call(move |c| relay_storage::SyncRepo::disconnect(c, &id))
            .await?;
        Ok(())
    }

    /// How a workspace's synchronisation stands.
    pub async fn sync_status(&self, workspace: WorkspaceId) -> RelayResult<SyncStatus> {
        let id = workspace.clone();
        let state = self
            .db
            .call(move |c| relay_storage::SyncRepo::state(c, &id))
            .await?;

        let Some(state) = state else {
            return Ok(SyncStatus::default());
        };

        let id = workspace.clone();
        let conflicts = self
            .db
            .call(move |c| relay_storage::SyncRepo::open_conflicts(c, &id))
            .await?
            .len();

        let id = workspace.clone();
        let pending = self
            .db
            .call(move |c| relay_storage::SyncRepo::pending_changes(c, &id))
            .await?
            .len();

        Ok(SyncStatus {
            connected: true,
            server_url: Some(state.server_url),
            user_email: state.user_email,
            signed_in: self.sync_engine().token(&workspace).is_some(),
            last_pulled_seq: state.last_pulled_seq,
            last_sync_at: state.last_sync_at,
            last_error: state.last_error,
            pending_changes: pending,
            open_conflicts: conflicts,
        })
    }

    /// Opens a live connection for a workspace's change notifications.
    ///
    /// Returns `None` when the workspace is local-only or signed out —
    /// not an error, because that is the ordinary case and the caller has
    /// nothing to do about it.
    ///
    /// The watcher is an optimisation over polling: every notification is
    /// a revision number and the only response is to sync. If it never
    /// connects, synchronisation still works, just not automatically.
    pub async fn sync_watch(
        &self,
        workspace: WorkspaceId,
        events: tokio::sync::mpsc::Sender<(WorkspaceId, WatchEvent)>,
    ) -> RelayResult<Option<Watcher>> {
        let id = workspace.clone();
        let Some(state) = self
            .db
            .call(move |c| relay_storage::SyncRepo::state(c, &id))
            .await?
        else {
            return Ok(None);
        };

        let engine = self.sync_engine();
        let Some(token) = engine.token(&workspace) else {
            return Ok(None);
        };

        let client = SyncClient::new(&state.server_url)?;
        let remote_id = state
            .remote_workspace_id
            .unwrap_or_else(|| workspace.to_string());

        Ok(Some(Watcher::start(
            client.websocket_url(&remote_id),
            token,
            workspace,
            events,
        )))
    }

    /// Conflicts awaiting a decision.
    pub async fn sync_conflicts(&self, workspace: WorkspaceId) -> RelayResult<Vec<ConflictView>> {
        let conflicts = self
            .db
            .call(move |c| relay_storage::SyncRepo::open_conflicts(c, &workspace))
            .await?;

        Ok(conflicts
            .into_iter()
            .map(|c| ConflictView {
                id: c.id,
                entity: c.entity.as_str().to_string(),
                entity_id: c.entity_id,
                name: name_of(&c.local).or_else(|| c.server.as_ref().and_then(name_of)),
                local: c.local,
                server: c.server,
                server_deleted: c.server_deleted,
                detected_at: c.detected_at,
            })
            .collect())
    }

    /// Applies the user's decision about a conflict.
    pub async fn sync_resolve(&self, conflict_id: String, keep_local: bool) -> RelayResult<()> {
        self.sync_engine()
            .resolve(
                conflict_id,
                if keep_local {
                    Resolution::KeepLocal
                } else {
                    Resolution::KeepServer
                },
            )
            .await
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

/// How a workspace's synchronisation stands, for the status indicator.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncStatus {
    /// False when the workspace is purely local.
    pub connected: bool,
    /// Where it synchronises.
    pub server_url: Option<String>,
    /// Who is signed in there.
    pub user_email: Option<String>,
    /// False when the stored token has gone and the user must sign in again.
    pub signed_in: bool,
    /// The highest revision applied.
    pub last_pulled_seq: i64,
    /// When synchronisation last succeeded.
    pub last_sync_at: Option<Timestamp>,
    /// Why it last failed.
    pub last_error: Option<String>,
    /// Local changes waiting to upload.
    pub pending_changes: usize,
    /// Disagreements awaiting a decision.
    pub open_conflicts: usize,
}

/// A conflict as the resolution UI shows it.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConflictView {
    /// The conflict's id, passed back to resolve it.
    pub id: String,
    /// Which kind of object, e.g. `request`.
    pub entity: String,
    /// Which object.
    pub entity_id: String,
    /// A readable name, when either side has one.
    pub name: Option<String>,
    /// The local version.
    pub local: serde_json::Value,
    /// The server's version. `None` when the server deleted it.
    pub server: Option<serde_json::Value>,
    /// True when the server's side is a deletion.
    pub server_deleted: bool,
    /// When the disagreement was noticed.
    pub detected_at: Timestamp,
}

/// True when a document announces itself as OpenAPI.
///
/// A cheap textual check rather than a full parse: `import_detect` is
/// called on every import, and parsing a large YAML document twice to
/// find out it was not OpenAPI would be wasteful.
fn looks_like_openapi(source: &str) -> bool {
    source.lines().take(40).any(|line| {
        let line = line.trim_start();
        line.starts_with("\"openapi\"") || line.starts_with("openapi:")
    })
}

/// Pulls a display name out of a stored document, if it has one.
fn name_of(value: &serde_json::Value) -> Option<String> {
    value
        .get("name")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
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
