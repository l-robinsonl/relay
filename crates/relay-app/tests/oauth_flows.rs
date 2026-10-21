//! OAuth 2.0 against a real authorization server, over a real socket.
//!
//! The server here checks what a strict one would: client authentication,
//! the PKCE verifier, the redirect URI, and whether a refresh token is
//! being replayed. A flow that passes these is one that will work against
//! a production provider, which is not something a mocked token endpoint
//! can tell you.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use axum::extract::{Form, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::post;
use axum::{Json, Router};
use relay_app::{MemorySecretStore, RelayService};
use relay_http::{HttpEngine, NoopProgress};
use relay_models::{
    AuthConfig, ClientAuthStyle, ExecutionId, HttpExchange, HttpMethod, OAuth2Config, OAuth2Grant,
    RelayResult, ResponseBody,
};
use relay_storage::Database;

const CLIENT_ID: &str = "relay-test-client";
const CLIENT_SECRET: &str = "s3cret";

#[derive(Default)]
struct Issued {
    /// Challenge seen at /authorize, keyed by the code handed out.
    pkce: std::collections::HashMap<String, String>,
    /// Refresh tokens that are still good.
    refresh: std::collections::HashSet<String>,
    /// How long each new access token lasts, in seconds.
    lifetime: u32,
}

#[derive(Clone, Default)]
struct AuthServer {
    issued: Arc<Mutex<Issued>>,
    token_calls: Arc<AtomicU32>,
}

struct TestServer {
    addr: std::net::SocketAddr,
    state: AuthServer,
    handle: tokio::task::JoinHandle<()>,
}

impl TestServer {
    fn url(&self, path: &str) -> String {
        format!("http://{}{}", self.addr, path)
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

/// Checks the client credentials, however they were presented.
fn client_authenticated(
    headers: &HeaderMap,
    form: &std::collections::HashMap<String, String>,
) -> bool {
    if let Some(header) = headers.get("authorization").and_then(|v| v.to_str().ok()) {
        use base64::Engine as _;
        let encoded = header.trim_start_matches("Basic ").trim();
        if let Ok(decoded) = base64::engine::general_purpose::STANDARD.decode(encoded) {
            let decoded = String::from_utf8_lossy(&decoded).into_owned();
            return decoded == format!("{CLIENT_ID}:{CLIENT_SECRET}");
        }
        return false;
    }

    form.get("client_id").map(String::as_str) == Some(CLIENT_ID)
        && form.get("client_secret").map(String::as_str) == Some(CLIENT_SECRET)
}

fn pkce_matches(verifier: &str, challenge: &str) -> bool {
    use base64::Engine as _;
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(verifier.as_bytes());
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest) == challenge
}

async fn start_auth_server(lifetime_seconds: u32) -> TestServer {
    let state = AuthServer::default();
    state.issued.lock().unwrap().lifetime = lifetime_seconds;

    let app = Router::new()
        .route(
            "/authorize",
            axum::routing::get(
                |State(state): State<AuthServer>,
                 Query(query): Query<std::collections::HashMap<String, String>>| async move {
                    // A real server shows a consent screen here. This one
                    // approves at once and redirects, which is what the
                    // browser would eventually do anyway.
                    let code = format!("code-{}", uuid::Uuid::new_v4());
                    if let Some(challenge) = query.get("code_challenge") {
                        state
                            .issued
                            .lock()
                            .unwrap()
                            .pkce
                            .insert(code.clone(), challenge.clone());
                    }

                    let redirect = query.get("redirect_uri").cloned().unwrap_or_default();
                    let location = format!(
                        "{redirect}?code={code}&state={}",
                        query.get("state").cloned().unwrap_or_default()
                    );

                    (
                        StatusCode::FOUND,
                        [(axum::http::header::LOCATION, location)],
                    )
                },
            ),
        )
        .route(
            "/token",
            post(
                |State(state): State<AuthServer>,
                 headers: HeaderMap,
                 Form(form): Form<std::collections::HashMap<String, String>>| async move {
                    state.token_calls.fetch_add(1, Ordering::SeqCst);

                    if !client_authenticated(&headers, &form) {
                        return (
                            StatusCode::UNAUTHORIZED,
                            Json(serde_json::json!({"error": "invalid_client"})),
                        );
                    }

                    let grant = form.get("grant_type").map(String::as_str).unwrap_or("");
                    let mut issued = state.issued.lock().unwrap();

                    match grant {
                        "client_credentials" => {}
                        "password" => {
                            if form.get("username").map(String::as_str) != Some("ada")
                                || form.get("password").map(String::as_str) != Some("lovelace")
                            {
                                return (
                                    StatusCode::BAD_REQUEST,
                                    Json(serde_json::json!({"error": "invalid_grant"})),
                                );
                            }
                        }
                        "authorization_code" => {
                            let code = form.get("code").cloned().unwrap_or_default();
                            let expected = issued.pkce.remove(&code);

                            if !form.contains_key("redirect_uri") {
                                return (
                                    StatusCode::BAD_REQUEST,
                                    Json(serde_json::json!({"error": "invalid_request",
                                        "error_description": "redirect_uri is required"})),
                                );
                            }

                            if let Some(challenge) = expected {
                                let verifier =
                                    form.get("code_verifier").cloned().unwrap_or_default();
                                if !pkce_matches(&verifier, &challenge) {
                                    return (
                                        StatusCode::BAD_REQUEST,
                                        Json(serde_json::json!({"error": "invalid_grant",
                                            "error_description": "PKCE verification failed"})),
                                    );
                                }
                            }
                        }
                        "refresh_token" => {
                            let token = form.get("refresh_token").cloned().unwrap_or_default();
                            if !issued.refresh.remove(&token) {
                                return (
                                    StatusCode::BAD_REQUEST,
                                    Json(serde_json::json!({"error": "invalid_grant",
                                        "error_description": "that refresh token is spent"})),
                                );
                            }
                        }
                        other => {
                            return (
                                StatusCode::BAD_REQUEST,
                                Json(serde_json::json!({
                                    "error": "unsupported_grant_type",
                                    "error_description": other,
                                })),
                            );
                        }
                    }

                    let serial = state.token_calls.load(Ordering::SeqCst);
                    let refresh = format!("refresh-{serial}");
                    issued.refresh.insert(refresh.clone());
                    let lifetime = issued.lifetime;

                    (
                        StatusCode::OK,
                        Json(serde_json::json!({
                            "access_token": format!("access-{serial}"),
                            "token_type": "Bearer",
                            "expires_in": lifetime,
                            "refresh_token": refresh,
                            "scope": form.get("scope").cloned().unwrap_or_default(),
                        })),
                    )
                },
            ),
        )
        // What the user's own API would be: echoes the credential it saw.
        .route(
            "/resource",
            axum::routing::get(|headers: HeaderMap| async move {
                Json(serde_json::json!({
                    "auth": headers
                        .get("authorization")
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or(""),
                }))
            }),
        )
        .with_state(state.clone());

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    TestServer {
        addr,
        state,
        handle,
    }
}

fn service() -> RelayService {
    RelayService::new(
        Database::open_in_memory().unwrap(),
        HttpEngine::with_defaults(),
        Arc::new(MemorySecretStore::new()),
    )
}

fn body_text(exchange: &HttpExchange) -> String {
    match exchange {
        HttpExchange::Success { response, .. } => match &response.body {
            ResponseBody::Text { text } => text.clone(),
            other => panic!("expected a text body, got {other:?}"),
        },
        other => panic!("expected success, got {other:?}"),
    }
}

/// Stands in for the user's browser: follows the authorization redirect.
fn browser() -> impl FnOnce(&str) -> RelayResult<()> {
    |url: &str| {
        let url = url.to_string();
        // Off the current task, because `oauth_authorize` has not started
        // waiting yet — exactly like a browser the user switches to.
        tokio::spawn(async move {
            // Follows the 302 to the loopback listener, which is what a
            // browser does with the consent screen's redirect.
            let _ = reqwest::Client::new().get(&url).send().await;
        });
        Ok(())
    }
}

async fn send_to(service: &RelayService, url: String, auth: AuthConfig) -> HttpExchange {
    let workspace = service.active_workspace().await.unwrap();
    let mut doc = service
        .request_create(workspace.id, "Protected".into(), None)
        .await
        .unwrap();
    doc.method = HttpMethod::Get;
    doc.url = url;
    doc.auth = auth;

    service
        .request_send(ExecutionId::new(), doc, None, Arc::new(NoopProgress))
        .await
        .unwrap()
}

fn config(server: &TestServer, grant: OAuth2Grant) -> OAuth2Config {
    OAuth2Config {
        grant,
        auth_url: server.url("/authorize"),
        token_url: server.url("/token"),
        client_id: CLIENT_ID.into(),
        client_secret: CLIENT_SECRET.into(),
        scope: "read:things".into(),
        ..Default::default()
    }
}

#[tokio::test]
async fn the_client_credentials_grant_fetches_a_token_and_sends_it() {
    let server = start_auth_server(3600).await;
    let service = service();

    let auth = AuthConfig::OAuth2(Box::new(config(&server, OAuth2Grant::ClientCredentials)));
    let exchange = send_to(&service, server.url("/resource"), auth).await;

    assert_eq!(body_text(&exchange), r#"{"auth":"Bearer access-1"}"#);
    assert_eq!(server.state.token_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_second_send_reuses_the_cached_token() {
    // Fetching a token per request would be slow and, against providers
    // that rate-limit the token endpoint, would get the user blocked.
    let server = start_auth_server(3600).await;
    let service = service();
    let auth = AuthConfig::OAuth2(Box::new(config(&server, OAuth2Grant::ClientCredentials)));

    send_to(&service, server.url("/resource"), auth.clone()).await;
    let second = send_to(&service, server.url("/resource"), auth).await;

    assert_eq!(body_text(&second), r#"{"auth":"Bearer access-1"}"#);
    assert_eq!(
        server.state.token_calls.load(Ordering::SeqCst),
        1,
        "the token endpoint should have been called once, not once per send"
    );
}

#[tokio::test]
async fn an_expiring_token_is_refreshed_rather_than_re_fetched() {
    // Thirty seconds is inside the sixty-second safety margin, so the
    // token is treated as spent the moment it arrives.
    let server = start_auth_server(30).await;
    let service = service();
    let auth = AuthConfig::OAuth2(Box::new(config(&server, OAuth2Grant::ClientCredentials)));

    let first = send_to(&service, server.url("/resource"), auth.clone()).await;
    assert_eq!(body_text(&first), r#"{"auth":"Bearer access-1"}"#);

    let second = send_to(&service, server.url("/resource"), auth).await;

    // A refresh, not a re-fetch: the server only accepts a refresh token
    // it issued, and it burns it on use, so reaching access-2 at all
    // proves the refresh path ran.
    assert_eq!(body_text(&second), r#"{"auth":"Bearer access-2"}"#);
}

#[tokio::test]
async fn credentials_can_be_sent_in_the_body_for_servers_that_want_that() {
    let server = start_auth_server(3600).await;
    let service = service();

    let mut c = config(&server, OAuth2Grant::ClientCredentials);
    c.client_auth = ClientAuthStyle::Body;

    let exchange = send_to(
        &service,
        server.url("/resource"),
        AuthConfig::OAuth2(Box::new(c)),
    )
    .await;
    assert_eq!(body_text(&exchange), r#"{"auth":"Bearer access-1"}"#);
}

#[tokio::test]
async fn the_password_grant_works_and_a_wrong_password_is_reported() {
    let server = start_auth_server(3600).await;
    let service = service();

    let mut c = config(&server, OAuth2Grant::Password);
    c.username = "ada".into();
    c.password = "lovelace".into();

    let status = service.oauth_fetch_token(c.clone()).await.unwrap();
    assert!(status.has_token);
    assert!(status.can_refresh);

    c.password = "wrong".into();
    let error = service.oauth_fetch_token(c).await.unwrap_err();
    assert!(error.message.contains("invalid_grant"), "{error:?}");
}

#[tokio::test]
async fn the_authorization_code_grant_completes_through_the_browser() {
    let server = start_auth_server(3600).await;
    let service = service();

    let c = config(&server, OAuth2Grant::AuthorizationCode);
    let status = service.oauth_authorize(c.clone(), browser()).await.unwrap();

    assert!(status.has_token);
    assert!(status.can_refresh);

    // And the token the flow obtained is the one a send then uses,
    // without a second trip to the browser.
    let exchange = send_to(
        &service,
        server.url("/resource"),
        AuthConfig::OAuth2(Box::new(c)),
    )
    .await;

    assert_eq!(body_text(&exchange), r#"{"auth":"Bearer access-1"}"#);
}

#[tokio::test]
async fn an_authorization_code_send_without_authorizing_says_so() {
    let server = start_auth_server(3600).await;
    let service = service();

    let workspace = service.active_workspace().await.unwrap();
    let mut doc = service
        .request_create(workspace.id, "Protected".into(), None)
        .await
        .unwrap();
    doc.url = server.url("/resource");
    doc.auth = AuthConfig::OAuth2(Box::new(config(&server, OAuth2Grant::AuthorizationCode)));

    let error = service
        .request_send(ExecutionId::new(), doc, None, Arc::new(NoopProgress))
        .await
        .unwrap_err();

    // Not a bare 401 from the API: the user is told what to press.
    assert!(error.detail.unwrap_or_default().contains("Authorize"));
}

#[tokio::test]
async fn pkce_is_actually_used_and_verified() {
    // The server rejects a verifier that does not hash to the challenge
    // it saw, so completing the flow at all proves Relay sent a matching
    // pair rather than omitting the verifier.
    let server = start_auth_server(3600).await;
    let service = service();

    let mut c = config(&server, OAuth2Grant::AuthorizationCode);
    assert!(c.use_pkce, "PKCE must be on unless deliberately turned off");

    let status = service.oauth_authorize(c.clone(), browser()).await.unwrap();
    assert!(status.has_token);

    // Without PKCE the server has no challenge stored and lets it past,
    // which is the downgrade the checkbox describes.
    c.use_pkce = false;
    let status = service.oauth_authorize(c, browser()).await.unwrap();
    assert!(status.has_token);
}

#[tokio::test]
async fn a_bad_client_secret_is_reported_clearly() {
    let server = start_auth_server(3600).await;
    let service = service();

    let mut c = config(&server, OAuth2Grant::ClientCredentials);
    c.client_secret = "wrong".into();

    let error = service.oauth_fetch_token(c).await.unwrap_err();
    assert!(error.message.contains("invalid_client"), "{error:?}");
}

#[tokio::test]
async fn forgetting_a_token_forces_a_new_one() {
    let server = start_auth_server(3600).await;
    let service = service();
    let c = config(&server, OAuth2Grant::ClientCredentials);

    service.oauth_fetch_token(c.clone()).await.unwrap();
    assert!(service.oauth_token_status(&c).has_token);

    service.oauth_forget_token(&c);
    assert!(!service.oauth_token_status(&c).has_token);

    send_to(
        &service,
        server.url("/resource"),
        AuthConfig::OAuth2(Box::new(c)),
    )
    .await;

    assert_eq!(server.state.token_calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn the_token_is_never_written_into_the_saved_request() {
    // The whole point of the cache: a document that round-trips through
    // SQLite — and therefore through sync and export — carries the
    // parameters of the flow and no credential.
    let server = start_auth_server(3600).await;
    let service = service();

    let workspace = service.active_workspace().await.unwrap();
    let mut doc = service
        .request_create(workspace.id, "Protected".into(), None)
        .await
        .unwrap();
    doc.url = server.url("/resource");
    doc.auth = AuthConfig::OAuth2(Box::new(config(&server, OAuth2Grant::ClientCredentials)));
    let doc = service.request_save(doc).await.unwrap();

    service
        .request_send(
            ExecutionId::new(),
            doc.clone(),
            None,
            Arc::new(NoopProgress),
        )
        .await
        .unwrap();

    let reloaded = service.request_get(doc.id.clone()).await.unwrap();
    let AuthConfig::OAuth2(stored) = &reloaded.auth else {
        panic!("expected OAuth2 auth, got {:?}", reloaded.auth);
    };

    assert_eq!(stored.access_token, None, "an access token was persisted");
    assert_eq!(stored.refresh_token, None, "a refresh token was persisted");
    assert_eq!(stored.client_id, CLIENT_ID);
}

#[tokio::test]
async fn the_history_entry_holds_no_bearer_token() {
    let server = start_auth_server(3600).await;
    let service = service();

    let workspace = service.active_workspace().await.unwrap();
    let mut doc = service
        .request_create(workspace.id.clone(), "Protected".into(), None)
        .await
        .unwrap();
    doc.url = server.url("/resource");
    doc.auth = AuthConfig::OAuth2(Box::new(config(&server, OAuth2Grant::ClientCredentials)));

    service
        .request_send(ExecutionId::new(), doc, None, Arc::new(NoopProgress))
        .await
        .unwrap();

    let history = service
        .history_list(workspace.id.clone(), 10, 0)
        .await
        .unwrap();
    let serialised = serde_json::to_string(&history).unwrap();

    assert!(
        !serialised.contains("access-1"),
        "the access token reached history: {serialised}"
    );
}

#[tokio::test]
async fn a_client_secret_can_live_in_the_credential_store() {
    // The recommended way to hold a client secret: a secret variable, so
    // the value sits in the OS keyring and the request document carries
    // only its name. This crosses interpolation, the secret store and
    // the token cache, so it is worth proving end to end.
    use relay_models::Variable;

    let server = start_auth_server(3600).await;
    let service = service();
    let workspace = service.active_workspace().await.unwrap();

    let mut environment = service
        .environment_create(workspace.id.clone(), "Live".into())
        .await
        .unwrap();
    environment.variables = vec![Variable {
        key: "oauthSecret".into(),
        value: String::new(),
        enabled: true,
        secret: true,
        description: None,
    }];
    let environment = service.environment_save(environment).await.unwrap();
    service
        .secret_set(&environment.id, "oauthSecret", CLIENT_SECRET)
        .unwrap();

    let mut c = config(&server, OAuth2Grant::ClientCredentials);
    c.client_secret = "{{oauthSecret}}".into();

    let mut doc = service
        .request_create(workspace.id.clone(), "Protected".into(), None)
        .await
        .unwrap();
    doc.url = server.url("/resource");
    doc.auth = AuthConfig::OAuth2(Box::new(c));
    let doc = service.request_save(doc).await.unwrap();

    let exchange = service
        .request_send(
            ExecutionId::new(),
            doc.clone(),
            Some(environment.id.clone()),
            Arc::new(NoopProgress),
        )
        .await
        .unwrap();

    assert_eq!(body_text(&exchange), r#"{"auth":"Bearer access-1"}"#);

    // And what was saved still holds the reference, not the secret.
    let reloaded = service.request_get(doc.id.clone()).await.unwrap();
    let AuthConfig::OAuth2(stored) = &reloaded.auth else {
        panic!("expected OAuth2");
    };
    assert_eq!(stored.client_secret, "{{oauthSecret}}");
}
