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
///
/// Cheap to clone: every field is either `Arc`-backed or a handle. The
/// assistant's tools each hold one, and cloning beats threading a
/// reference through a trait object with a lifetime.
#[derive(Clone)]
pub struct RelayService {
    db: Database,
    engine: HttpEngine,
    secrets: Arc<dyn SecretStore>,
    tokens: Arc<TokenCache>,
    streams: crate::streams::Streams,
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
            streams: crate::streams::Streams::new(),
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

    /// Opens a stream and reads it until it ends or is closed.
    ///
    /// Returns as soon as the connection is handed to a task, because
    /// the whole point is that it does not finish: events arrive
    /// through `events` and `stream_close` is how it stops.
    pub async fn stream_open(
        &self,
        execution_id: relay_models::ExecutionId,
        doc: RequestDoc,
        environment: Option<EnvironmentId>,
        events: Arc<dyn crate::streams::StreamSink>,
    ) -> RelayResult<()> {
        let context_data = self.load_scope(&doc, environment).await?;
        let input = context_data.as_input();
        let context = build_context(&input);
        let spec = resolve(&doc, &input, &context).spec;

        let token = self.streams.register(execution_id.clone());
        let engine = self.engine.clone();
        let streams = self.streams.clone();

        tokio::spawn(async move {
            events.emit(crate::streams::StreamEvent::Open {
                execution_id: execution_id.clone(),
            });

            let sink = events.clone();
            let id = execution_id.clone();
            let closing = token.clone();

            let outcome = relay_http::sse::stream(engine.pool(), &spec, move |event| {
                sink.emit(crate::streams::StreamEvent::Message {
                    execution_id: id.clone(),
                    event,
                    at: relay_models::Timestamp::now().as_millis(),
                });
                // Each event is also the moment to notice that the
                // person has pressed stop.
                !closing.is_cancelled()
            })
            .await;

            streams.forget(&execution_id);
            events.emit(crate::streams::StreamEvent::Closed {
                execution_id,
                // Being closed on purpose is not an error, and
                // reporting it as one would put a red box on screen
                // every time somebody pressed stop.
                error: match outcome {
                    Ok(()) => None,
                    Err(_) if token.is_cancelled() => None,
                    Err(e) => Some(e.to_string()),
                },
            });
        });

        Ok(())
    }

    /// Closes a stream. Says whether one was open.
    pub fn stream_close(&self, execution_id: &relay_models::ExecutionId) -> bool {
        self.streams.close(execution_id)
    }

    /// Closes every stream.
    pub fn streams_close_all(&self) {
        self.streams.close_all();
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

    /// Asks a GraphQL endpoint what it accepts.
    ///
    /// Sent as the request it belongs to — same URL, same headers,
    /// same authentication, same TLS settings — with the body swapped
    /// for the introspection query. A schema browser that could not
    /// reach a private API would be a schema browser for public
    /// demos, and every interesting API is behind a key.
    pub async fn graphql_schema(
        &self,
        doc: RequestDoc,
        environment: Option<EnvironmentId>,
    ) -> RelayResult<relay_core::graphql::Schema> {
        let mut probe = doc;
        probe.body = relay_models::RequestBody::GraphQl {
            query: relay_core::graphql::INTROSPECTION_QUERY.to_string(),
            variables: String::new(),
        };
        // Introspection is a POST whatever the request normally is.
        probe.method = relay_models::HttpMethod::Post;
        // Scripts belong to the request the person wrote, not to a
        // schema lookup: a pre-request script that rewrites the body
        // would overwrite the introspection query, and a test script
        // would run against a response nobody asked for.
        probe.scripts = Default::default();

        let exchange = self
            .request_send(
                relay_models::ExecutionId::new(),
                probe,
                environment,
                Arc::new(relay_http::NoopProgress),
            )
            .await?;

        let body = match &exchange {
            HttpExchange::Success { response, .. } => match &response.body {
                relay_models::ResponseBody::Text { text } => text.clone(),
                // A schema arriving as a file means it was enormous,
                // and as binary means it is not a schema at all.
                _ => {
                    return Err(RelayError::new(
                        ErrorKind::Invalid,
                        "the endpoint did not answer with text",
                    ));
                }
            },
            HttpExchange::Failure { error, .. } => {
                return Err(RelayError::new(ErrorKind::Transport, error.message.clone()));
            }
            HttpExchange::Cancelled { .. } => {
                return Err(RelayError::new(
                    ErrorKind::Transport,
                    "the schema lookup was cancelled",
                ));
            }
        };

        let json: serde_json::Value = serde_json::from_str(&body).map_err(|_| {
            RelayError::new(ErrorKind::Invalid, "the endpoint did not answer with JSON")
                .with_detail(
                    "Check the URL points at the GraphQL endpoint itself rather than a \
                 page describing it."
                        .to_string(),
                )
        })?;

        relay_core::graphql::Schema::parse(&json)
            .map_err(|message| RelayError::new(ErrorKind::Invalid, message))
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

    /// What the workspace is entitled to, and whether it is paid for.
    pub async fn sync_billing(
        &self,
        workspace: WorkspaceId,
    ) -> RelayResult<relay_sync::BillingView> {
        let (client, token) = self.sync_session(&workspace).await?;
        client.billing(&token, workspace.as_str()).await
    }

    /// Starts a checkout, returning the URL for the browser.
    pub async fn sync_billing_checkout(
        &self,
        workspace: WorkspaceId,
        seats: Option<u32>,
    ) -> RelayResult<String> {
        let (client, token) = self.sync_session(&workspace).await?;
        client
            .billing_checkout(&token, workspace.as_str(), seats)
            .await
    }

    /// Opens Stripe's customer portal, returning the URL.
    pub async fn sync_billing_portal(&self, workspace: WorkspaceId) -> RelayResult<String> {
        let (client, token) = self.sync_session(&workspace).await?;
        client.billing_portal(&token, workspace.as_str()).await
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
        invitation: Option<String>,
    ) -> RelayResult<relay_sync::UserProfile> {
        let client = SyncClient::new(&server_url)?;
        client.health().await?;

        let display_name = if display_name.trim().is_empty() {
            email.split('@').next().unwrap_or("User").to_string()
        } else {
            display_name
        };

        client
            .register(&email, &password, &display_name, invitation.as_deref())
            .await
    }

    /// Reads what an invitation offers, without accepting it.
    pub async fn sync_invitation_offer(
        &self,
        server_url: String,
        token: String,
    ) -> RelayResult<relay_sync::InvitationOffer> {
        SyncClient::new(&server_url)?
            .invitation_offer(&extract_invitation_token(&token))
            .await
    }

    /// Joins the workspace an invitation names, end to end.
    ///
    /// One call because it is one intention. Signing in, accepting,
    /// creating the local workspace and connecting it are four steps
    /// that are useless apart, and leaving the interface to sequence
    /// them means every partial failure becomes a state a person has to
    /// understand and unpick.
    pub async fn sync_join_invited(
        &self,
        server_url: String,
        invitation: String,
        password: String,
        display_name: String,
    ) -> RelayResult<Workspace> {
        let token = extract_invitation_token(&invitation);
        let client = SyncClient::new(&server_url)?;
        client.health().await?;

        // What is on offer, and crucially which address it is for: the
        // invitation decides that, not the person accepting.
        let offer = client.invitation_offer(&token).await?;

        // An existing account signs in; a new one registers with the
        // invitation, which is also what gets past an invite-only
        // server. Which of the two applies is not something to make
        // somebody choose when the server already knows.
        let session = match client.login(&offer.email, &password).await {
            Ok(session) => session,
            Err(error) if error.kind == ErrorKind::Forbidden => {
                let display_name = if display_name.trim().is_empty() {
                    offer.email.split('@').next().unwrap_or("User").to_string()
                } else {
                    display_name
                };

                client
                    .register(&offer.email, &password, &display_name, Some(&token))
                    .await?;

                // Registering with the invitation already joined them.
                return self
                    .adopt_invited_workspace(&server_url, &offer, &client, &password)
                    .await;
            }
            Err(error) => return Err(error),
        };

        client.accept_invitation(&session.token, &token).await?;
        self.adopt_invited_workspace(&server_url, &offer, &client, &password)
            .await
    }

    /// Creates the local workspace an invitation points at and connects it.
    async fn adopt_invited_workspace(
        &self,
        server_url: &str,
        offer: &relay_sync::InvitationOffer,
        _client: &SyncClient,
        password: &str,
    ) -> RelayResult<Workspace> {
        // The local workspace must carry the server's id, or the join
        // would publish a second, empty workspace beside the real one.
        let workspace = Workspace {
            id: WorkspaceId::from(offer.workspace_id.clone()),
            name: offer.workspace_name.clone(),
            ..Workspace::new(offer.workspace_name.clone())
        };

        let saved = workspace.clone();
        self.db
            .call(move |c| WorkspaceRepo::save(c, &saved))
            .await?;

        self.sync_connect(
            workspace.id.clone(),
            server_url.to_string(),
            offer.email.clone(),
            password.to_string(),
            // Adopt the server's copy rather than publishing this empty
            // one over the top of it.
            true,
        )
        .await?;

        Ok(workspace)
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

/// Pulls the token out of whatever the invitee pasted.
///
/// People paste the whole link, because that is what they were given.
/// Asking them to find the part after `token=` is a pointless step that
/// some of them will get wrong.
fn extract_invitation_token(pasted: &str) -> String {
    let pasted = pasted.trim();

    match pasted.split_once("token=") {
        Some((_, rest)) => rest
            .split(['&', '#', ' '])
            .next()
            .unwrap_or(rest)
            .to_string(),
        None => pasted.to_string(),
    }
}

#[cfg(test)]
mod invitation_tests {
    use super::extract_invitation_token;

    #[test]
    fn the_whole_link_works_because_that_is_what_people_paste() {
        assert_eq!(
            extract_invitation_token("https://sync.example.com/invite?token=abc123"),
            "abc123"
        );
    }

    #[test]
    fn a_bare_token_works_too() {
        assert_eq!(extract_invitation_token("abc123"), "abc123");
    }

    #[test]
    fn surrounding_rubbish_is_trimmed() {
        // Copied out of an e-mail, a link often brings whitespace, a
        // trailing fragment, or another parameter with it.
        assert_eq!(
            extract_invitation_token("  https://sync.example.com/invite?token=abc123  "),
            "abc123"
        );
        assert_eq!(
            extract_invitation_token("https://x/invite?token=abc123&utm_source=mail"),
            "abc123"
        );
        assert_eq!(
            extract_invitation_token("https://x/invite?token=abc123#top"),
            "abc123"
        );
    }

    #[test]
    fn a_token_is_not_invented_from_a_link_without_one() {
        assert_eq!(
            extract_invitation_token("https://sync.example.com/invite"),
            "https://sync.example.com/invite"
        );
    }
}

// --- the assistant ---------------------------------------------------------

/// What the assistant is configured to use.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantSettings {
    /// `anthropic`, `openai`, or `compatible` for anything else.
    pub provider: String,
    /// The model id. Empty means the provider's default.
    #[serde(default)]
    pub model: String,
    /// For a local or third-party endpoint speaking the OpenAI shape.
    #[serde(default)]
    pub base_url: String,
}

impl Default for AssistantSettings {
    fn default() -> Self {
        Self {
            provider: "anthropic".into(),
            model: String::new(),
            base_url: String::new(),
        }
    }
}

/// Where the assistant's key lives in the credential store.
///
/// Reusing the secret store rather than inventing a second one: it is
/// already the OS keyring, already has a memory fallback for a headless
/// machine, and already never reaches the webview.
fn assistant_key_scope() -> EnvironmentId {
    EnvironmentId::from("assistant")
}

impl RelayService {
    /// Stores the key for a provider.
    pub fn assistant_set_key(&self, provider: &str, key: &str) -> RelayResult<()> {
        self.secrets
            .set(&assistant_key_scope(), provider, key)
            .map_err(|e| RelayError::new(ErrorKind::SecretStore, e))
    }

    /// Forgets the key for a provider.
    pub fn assistant_clear_key(&self, provider: &str) -> RelayResult<()> {
        self.secrets
            .delete(&assistant_key_scope(), provider)
            .map_err(|e| RelayError::new(ErrorKind::SecretStore, e))
    }

    /// Whether a key is set. Never returns the key itself.
    pub fn assistant_has_key(&self, provider: &str) -> bool {
        self.secrets
            .get(&assistant_key_scope(), provider)
            .is_some_and(|k| !k.trim().is_empty())
    }

    /// Reads a key, falling back to the environment.
    ///
    /// The environment fallback is for development: a contributor with
    /// `ANTHROPIC_API_KEY` already exported should not have to paste it
    /// into a settings box as well. The keyring wins when both are set.
    fn assistant_key(&self, provider: &str) -> Option<String> {
        self.secrets
            .get(&assistant_key_scope(), provider)
            .filter(|k| !k.trim().is_empty())
            .or_else(|| {
                let variable = match provider {
                    "anthropic" => "ANTHROPIC_API_KEY",
                    "openai" => "OPENAI_API_KEY",
                    _ => return None,
                };
                std::env::var(variable)
                    .ok()
                    .filter(|k| !k.trim().is_empty())
            })
    }

    /// Builds the configured provider.
    async fn assistant_provider(
        &self,
        settings: &AssistantSettings,
    ) -> RelayResult<Arc<dyn relay_agent::provider::ModelProvider>> {
        let missing = |name: &str| {
            RelayError::new(ErrorKind::Invalid, format!("no API key is set for {name}"))
                .with_detail("Add one under Settings → Assistant.".to_string())
        };

        match settings.provider.as_str() {
            // Relay's own server, on the subscription. No key here:
            // the sync session is the credential, and the provider
            // key never reaches this process.
            "relay" => {
                let workspace = self.active_workspace().await?;
                let (client, token) = self.sync_session(&workspace.id).await.map_err(|_| {
                    RelayError::new(
                        ErrorKind::Invalid,
                        "connect this workspace to a Relay server first",
                    )
                    .with_detail(
                        "Sync → connect, then sign in. The assistant then runs on your \
                         subscription."
                            .to_string(),
                    )
                })?;

                Ok(Arc::new(
                    relay_agent::provider::RelayProvider::new(client.base_url(), token)
                        .map_err(|e| RelayError::new(ErrorKind::Invalid, e.to_string()))?,
                ))
            }
            "openai" => {
                let key = self
                    .assistant_key("openai")
                    .ok_or_else(|| missing("OpenAI"))?;
                Ok(Arc::new(
                    relay_agent::provider::OpenAiProvider::new(key)
                        .map_err(|e| RelayError::new(ErrorKind::Invalid, e.to_string()))?,
                ))
            }
            // A local endpoint usually wants no key at all, so an empty
            // one is not an error here.
            "compatible" => {
                let base = if settings.base_url.trim().is_empty() {
                    return Err(RelayError::new(
                        ErrorKind::Invalid,
                        "set the endpoint address for a compatible provider",
                    ));
                } else {
                    settings.base_url.trim().to_string()
                };

                Ok(Arc::new(
                    relay_agent::provider::OpenAiProvider::with_base_url(
                        self.assistant_key("compatible").unwrap_or_default(),
                        base,
                        "the configured endpoint",
                    )
                    .map_err(|e| RelayError::new(ErrorKind::Invalid, e.to_string()))?,
                ))
            }
            _ => {
                let key = self
                    .assistant_key("anthropic")
                    .ok_or_else(|| missing("Anthropic"))?;
                Ok(Arc::new(
                    relay_agent::provider::AnthropicProvider::new(key)
                        .map_err(|e| RelayError::new(ErrorKind::Invalid, e.to_string()))?,
                ))
            }
        }
    }

    /// The conversations in a workspace, most recently touched first.
    pub async fn assistant_conversations(
        &self,
        workspace: WorkspaceId,
    ) -> RelayResult<Vec<relay_storage::Conversation>> {
        Ok(self
            .db
            .call(move |c| relay_storage::ConversationRepo::list(c, &workspace, 100))
            .await?)
    }

    /// Everything said in one conversation.
    pub async fn assistant_messages(
        &self,
        conversation: String,
    ) -> RelayResult<Vec<relay_storage::StoredMessage>> {
        Ok(self
            .db
            .call(move |c| relay_storage::ConversationRepo::messages(c, &conversation))
            .await?)
    }

    /// Deletes a conversation.
    pub async fn assistant_delete_conversation(&self, conversation: String) -> RelayResult<()> {
        self.db
            .call(move |c| relay_storage::ConversationRepo::delete(c, &conversation))
            .await?;
        Ok(())
    }

    /// Asks the assistant something, running tools until it answers.
    ///
    /// Creates the conversation on the first message. Everything the
    /// turn produced is persisted before this returns, so a crash
    /// mid-answer loses the answer but never the question.
    pub async fn assistant_ask(
        &self,
        workspace: WorkspaceId,
        conversation: Option<String>,
        question: String,
        settings: AssistantSettings,
        approvals: Arc<crate::assistant::Approvals>,
        events: Arc<dyn crate::assistant::EventSink>,
    ) -> RelayResult<String> {
        let provider = self.assistant_provider(&settings).await?;

        // A new conversation is titled from the question, trimmed, so
        // the list reads as a list of topics.
        let conversation_id = match conversation {
            Some(id) => id,
            None => {
                let id = uuid::Uuid::now_v7().to_string();
                let title: String = question.chars().take(60).collect();
                let workspace = workspace.clone();
                let new_id = id.clone();
                self.db
                    .call(move |c| {
                        relay_storage::ConversationRepo::create(
                            c,
                            &new_id,
                            &workspace,
                            title.trim(),
                        )
                    })
                    .await?;
                id
            }
        };

        let history = self.assistant_history(&conversation_id).await?;

        let mut tools = crate::assistant::registry(self.clone(), workspace);

        // Connected for the length of the turn, then shut down. A
        // persistent pool would avoid a process spawn per question and
        // is the obvious improvement; starting one local server takes
        // a fraction of a second, so this is not worth the state
        // management yet. A server that will not start is reported to
        // the model as a missing tool rather than failing the turn —
        // the other tools still work.
        let mut connections = Vec::new();
        for server in self.assistant_mcp_servers().await.unwrap_or_default() {
            match relay_agent::mcp::connect(&server).await {
                Ok(connection) => {
                    for tool in connection.tools() {
                        tools.add(tool.clone());
                    }
                    connections.push(connection);
                }
                Err(error) => {
                    tracing::warn!(server = server.name(), %error, "an MCP server did not start");
                }
            }
        }

        let agent = relay_agent::Agent::new(provider, tools, SYSTEM_PROMPT)
            .with_model(settings.model.clone())
            .with_approver(Arc::new(crate::assistant::AskTheUser::new(
                approvals,
                events.clone(),
            )))
            .with_progress(Arc::new(crate::assistant::Forwarding::new(events)));

        let turn = agent
            .run(&history, relay_agent::provider::Message::user(question))
            .await
            .map_err(|e| {
                RelayError::new(ErrorKind::Internal, e.to_string()).with_detail(
                    if e.is_retryable() {
                        "Worth trying again."
                    } else {
                        "Check the assistant settings."
                    }
                    .to_string(),
                )
            })?;

        let rows: Vec<_> = turn
            .messages
            .iter()
            .map(|message| {
                (
                    match message.role {
                        relay_agent::provider::Role::User => "user".to_string(),
                        relay_agent::provider::Role::Assistant => "assistant".to_string(),
                    },
                    message.text.clone(),
                    serde_json::to_value(&message.tool_calls).unwrap_or_default(),
                    serde_json::to_value(&message.tool_results).unwrap_or_default(),
                )
            })
            .collect();

        let id = conversation_id.clone();
        self.db
            .call(move |c| relay_storage::ConversationRepo::append(c, &id, &rows))
            .await?;

        for connection in connections {
            connection.close().await;
        }

        Ok(conversation_id)
    }

    /// Replays a stored conversation into the shape the model expects.
    async fn assistant_history(
        &self,
        conversation: &str,
    ) -> RelayResult<Vec<relay_agent::provider::Message>> {
        let id = conversation.to_string();
        let stored = self
            .db
            .call(move |c| relay_storage::ConversationRepo::messages(c, &id))
            .await?;

        Ok(stored
            .into_iter()
            .map(|row| relay_agent::provider::Message {
                role: if row.role == "assistant" {
                    relay_agent::provider::Role::Assistant
                } else {
                    relay_agent::provider::Role::User
                },
                text: row.body,
                tool_calls: serde_json::from_value(row.tool_calls).unwrap_or_default(),
                tool_results: serde_json::from_value(row.tool_results).unwrap_or_default(),
            })
            .collect())
    }
}

impl RelayService {
    /// The MCP servers the user has configured.
    pub async fn assistant_mcp_servers(&self) -> RelayResult<Vec<relay_agent::McpServer>> {
        let stored = self
            .state_get(relay_storage::state_keys::ASSISTANT_MCP.to_string())
            .await?;

        Ok(stored
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default())
    }

    /// Replaces the configured servers.
    pub async fn assistant_set_mcp_servers(
        &self,
        servers: Vec<relay_agent::McpServer>,
    ) -> RelayResult<()> {
        let encoded = serde_json::to_string(&servers)
            .map_err(|e| RelayError::new(ErrorKind::Internal, e.to_string()))?;

        self.state_set(
            relay_storage::state_keys::ASSISTANT_MCP.to_string(),
            encoded,
        )
        .await
    }

    /// Connects to one server and reports what it offers.
    ///
    /// For the settings screen: somebody who has just typed a command
    /// wants to know it works before closing the dialog, not when
    /// their next question quietly has fewer tools than they expected.
    pub async fn assistant_mcp_probe(
        &self,
        server: relay_agent::McpServer,
    ) -> RelayResult<Vec<String>> {
        let connection = relay_agent::mcp::connect(&server)
            .await
            .map_err(|e| RelayError::new(ErrorKind::Internal, e.to_string()))?;

        let names = connection
            .tools()
            .iter()
            .map(|t| t.name().to_string())
            .collect();

        connection.close().await;
        Ok(names)
    }
}

/// What the assistant is told about itself.
///
/// Short on purpose. A long prompt listing every tool duplicates the
/// descriptions the tools already carry, and the two drift apart.
const SYSTEM_PROMPT: &str = "\
You are the assistant inside Relay, a local-first API client. You help the person \
with the HTTP requests, collections and environments in their workspace.

Use the tools rather than guessing. When they name a request in words, list the \
requests first to find its id.

You cannot read the value of a secret variable, and you cannot read a credential \
already set on a request — both come back redacted, by design. You can still \
reference a secret by name: write {{thatVariable}} into a URL, header or body and \
Relay substitutes it when the request is actually sent.

Sending a request reaches a real server with the person's credentials, and deleting \
something destroys their work, so they are asked before either. If they decline, do \
not try it again — ask what they would rather do.

Be brief. Say what you did, not what you are about to do.";
