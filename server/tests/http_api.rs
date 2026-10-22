//! The HTTP surface, exercised over a real socket.
//!
//! These drive the server the way the desktop client will: real requests,
//! real bearer tokens, a real WebSocket. Handler-level tests would not
//! catch a missing route, a wrong status code or an extractor that lets an
//! unauthenticated request through.

use std::sync::Arc;

use relay_server::config::{Config, SignupMode};
use relay_server::store::SyncStore;
use relay_server::{AppState, SqliteStore, router};
use serde_json::{Value, json};

/// A running server and a client pointed at it.
struct TestServer {
    base: String,
    client: reqwest::Client,
    handle: tokio::task::JoinHandle<()>,
    /// What the server tried to e-mail.
    mail: Arc<relay_server::mail::CapturingMailer>,
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

impl TestServer {
    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    async fn post(&self, path: &str, body: Value) -> reqwest::Response {
        self.client
            .post(self.url(path))
            .json(&body)
            .send()
            .await
            .unwrap()
    }

    async fn post_auth(&self, path: &str, token: &str, body: Value) -> reqwest::Response {
        self.client
            .post(self.url(path))
            .bearer_auth(token)
            .json(&body)
            .send()
            .await
            .unwrap()
    }

    async fn get(&self, path: &str) -> reqwest::Response {
        self.client.get(self.url(path)).send().await.unwrap()
    }

    async fn get_auth(&self, path: &str, token: &str) -> reqwest::Response {
        self.client
            .get(self.url(path))
            .bearer_auth(token)
            .send()
            .await
            .unwrap()
    }

    /// Registers a user and signs them in, returning their bearer token.
    async fn sign_up(&self, email: &str) -> String {
        let response = self
            .post(
                "/v1/auth/register",
                json!({ "email": email, "password": "correct horse battery" }),
            )
            .await;
        assert!(
            response.status().is_success(),
            "registration failed: {}",
            response.text().await.unwrap_or_default()
        );

        let response = self
            .post(
                "/v1/auth/login",
                json!({ "email": email, "password": "correct horse battery" }),
            )
            .await;
        assert_eq!(response.status(), 200);
        response.json::<Value>().await.unwrap()["token"]
            .as_str()
            .unwrap()
            .to_string()
    }
}

/// Builds a genuine WebSocket upgrade request, so the handshake reaches the
/// handler rather than being rejected by the upgrade extractor.
fn ws_request(
    base: &str,
    path: &str,
    token: &str,
) -> tokio_tungstenite::tungstenite::http::Request<()> {
    let host = base.replace("http://", "");
    tokio_tungstenite::tungstenite::http::Request::builder()
        .uri(format!("ws://{host}{path}"))
        .header("Authorization", format!("Bearer {token}"))
        .header("Host", host)
        .header("Connection", "Upgrade")
        .header("Upgrade", "websocket")
        .header("Sec-WebSocket-Version", "13")
        .header(
            "Sec-WebSocket-Key",
            tokio_tungstenite::tungstenite::handshake::client::generate_key(),
        )
        .body(())
        .unwrap()
}

async fn start(signup: SignupMode) -> TestServer {
    let store = SqliteStore::connect("sqlite::memory:").await.unwrap();
    store.migrate().await.unwrap();

    let config = Config {
        database_url: "sqlite::memory:".into(),
        bind: "127.0.0.1:0".parse().unwrap(),
        public_url: String::new(),
        signup_mode: signup,
        session_ttl: std::time::Duration::from_secs(3600),
        admin_email: None,
        admin_password: None,
        max_body_bytes: relay_sync::MAX_BATCH_BYTES + 65536,
        resend_api_key: None,
        mail_from: None,
        require_verified_email: false,
    };

    start_with(store, config).await
}

/// Starts a server that refuses sign-in until the address is confirmed.
async fn start_requiring_verification() -> TestServer {
    let store = SqliteStore::connect("sqlite::memory:").await.unwrap();
    store.migrate().await.unwrap();

    let config = Config {
        database_url: "sqlite::memory:".into(),
        bind: "127.0.0.1:0".parse().unwrap(),
        public_url: "https://relay.test".into(),
        signup_mode: SignupMode::Open,
        session_ttl: std::time::Duration::from_secs(3600),
        admin_email: None,
        admin_password: None,
        max_body_bytes: relay_sync::MAX_BATCH_BYTES + 65536,
        resend_api_key: None,
        mail_from: None,
        require_verified_email: true,
    };

    start_with(store, config).await
}

async fn start_with(store: SqliteStore, config: Config) -> TestServer {
    let mail = Arc::new(relay_server::mail::CapturingMailer::new());
    let state = AppState::with_mailer(Arc::new(store), config, mail.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, router(state)).await;
    });

    TestServer {
        base: format!("http://{addr}"),
        client: reqwest::Client::new(),
        handle,
        mail,
    }
}

#[tokio::test]
async fn health_reports_the_backend_without_authentication() {
    let server = start(SignupMode::Open).await;
    let response = server.get("/health").await;

    assert_eq!(response.status(), 200);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["status"], "ok");
    assert_eq!(body["backend"], "sqlite");
    assert_eq!(body["protocol"], "v1");
}

#[tokio::test]
async fn registration_and_login_issue_a_working_token() {
    let server = start(SignupMode::Open).await;
    let token = server.sign_up("ada@example.com").await;

    let response = server.get_auth("/v1/auth/me", &token).await;
    assert_eq!(response.status(), 200);

    let body: Value = response.json().await.unwrap();
    assert_eq!(body["email"], "ada@example.com");
    // A profile must never carry credential material.
    assert!(body.get("passwordHash").is_none());
    assert!(body.get("password").is_none());
}

#[tokio::test]
async fn registration_is_refused_when_signup_is_closed() {
    let server = start(SignupMode::Off).await;

    let response = server
        .post(
            "/v1/auth/register",
            json!({ "email": "nobody@example.com", "password": "correct horse battery" }),
        )
        .await;

    assert!(!response.status().is_success());
}

#[tokio::test]
async fn invite_only_does_not_silently_mean_open() {
    // Failing shut matters: a server configured invite-only before
    // invitations exist must refuse, not accept everyone.
    let server = start(SignupMode::Invite).await;

    let response = server
        .post(
            "/v1/auth/register",
            json!({ "email": "nobody@example.com", "password": "correct horse battery" }),
        )
        .await;

    assert!(!response.status().is_success());
}

#[tokio::test]
async fn a_wrong_password_is_refused() {
    let server = start(SignupMode::Open).await;
    server.sign_up("ada@example.com").await;

    let response = server
        .post(
            "/v1/auth/login",
            json!({ "email": "ada@example.com", "password": "not the password" }),
        )
        .await;

    assert_eq!(response.status(), 401);
}

#[tokio::test]
async fn an_unknown_address_fails_the_same_way_as_a_wrong_password() {
    // Distinguishing them would let anyone enumerate registered addresses.
    let server = start(SignupMode::Open).await;
    server.sign_up("ada@example.com").await;

    let wrong_password = server
        .post(
            "/v1/auth/login",
            json!({ "email": "ada@example.com", "password": "wrong" }),
        )
        .await;
    let unknown_user = server
        .post(
            "/v1/auth/login",
            json!({ "email": "nobody@example.com", "password": "wrong" }),
        )
        .await;

    assert_eq!(wrong_password.status(), unknown_user.status());
    assert_eq!(
        wrong_password.text().await.unwrap(),
        unknown_user.text().await.unwrap()
    );
}

#[tokio::test]
async fn protected_routes_refuse_unauthenticated_requests() {
    let server = start(SignupMode::Open).await;

    for path in [
        "/v1/auth/me",
        "/v1/workspaces",
        "/v1/workspaces/w1/head",
        "/v1/workspaces/w1/changes",
        "/v1/workspaces/w1/export",
        "/v1/workspaces/w1/members",
    ] {
        let response = server.get(path).await;
        assert_eq!(response.status(), 401, "{path} was not protected");
    }
}

#[tokio::test]
async fn a_malformed_token_is_refused() {
    let server = start(SignupMode::Open).await;

    for token in ["", "garbage", "Bearer nested"] {
        let response = server.get_auth("/v1/auth/me", token).await;
        assert_eq!(response.status(), 401, "token '{token}' was accepted");
    }
}

#[tokio::test]
async fn logging_out_revokes_the_token_immediately() {
    let server = start(SignupMode::Open).await;
    let token = server.sign_up("ada@example.com").await;

    assert_eq!(server.get_auth("/v1/auth/me", &token).await.status(), 200);

    let response = server.post_auth("/v1/auth/logout", &token, json!({})).await;
    assert_eq!(response.status(), 204);

    // The whole point of opaque tokens over JWTs.
    assert_eq!(server.get_auth("/v1/auth/me", &token).await.status(), 401);
}

#[tokio::test]
async fn a_workspace_round_trips_through_push_and_pull() {
    let server = start(SignupMode::Open).await;
    let token = server.sign_up("ada@example.com").await;

    let created = server
        .post_auth(
            "/v1/workspaces",
            &token,
            json!({ "id": "w1", "name": "Team" }),
        )
        .await;
    assert_eq!(created.status(), 201);

    let push = server
        .post_auth(
            "/v1/workspaces/w1/changes",
            &token,
            json!({
                "clientId": "device-a",
                "mutations": [{
                    "mutationId": "m1",
                    "kind": "request",
                    "id": "r1",
                    "baseRev": 0,
                    "deleted": false,
                    "payload": { "name": "List users" }
                }]
            }),
        )
        .await;
    assert_eq!(push.status(), 200);

    let result: Value = push.json().await.unwrap();
    assert_eq!(result["head"], 1);
    assert_eq!(result["outcomes"][0]["status"], "applied");

    let pull: Value = server
        .get_auth("/v1/workspaces/w1/changes?since=0", &token)
        .await
        .json()
        .await
        .unwrap();

    assert_eq!(pull["head"], 1);
    assert_eq!(pull["objects"][0]["id"], "r1");
    assert_eq!(pull["objects"][0]["payload"]["name"], "List users");
}

#[tokio::test]
async fn a_stale_push_returns_a_conflict_over_http() {
    let server = start(SignupMode::Open).await;
    let token = server.sign_up("ada@example.com").await;
    server
        .post_auth(
            "/v1/workspaces",
            &token,
            json!({ "id": "w1", "name": "Team" }),
        )
        .await;

    let mutation = |id: &str, base: i64, name: &str| {
        json!({
            "clientId": "d",
            "mutations": [{
                "mutationId": id, "kind": "request", "id": "r1",
                "baseRev": base, "deleted": false, "payload": { "name": name }
            }]
        })
    };

    server
        .post_auth(
            "/v1/workspaces/w1/changes",
            &token,
            mutation("m1", 0, "First"),
        )
        .await;
    server
        .post_auth(
            "/v1/workspaces/w1/changes",
            &token,
            mutation("m2", 1, "Second"),
        )
        .await;

    let response = server
        .post_auth(
            "/v1/workspaces/w1/changes",
            &token,
            mutation("m3", 1, "Stale"),
        )
        .await;

    // A conflict is a normal outcome, not a failed request.
    assert_eq!(response.status(), 200);
    let result: Value = response.json().await.unwrap();
    assert_eq!(result["outcomes"][0]["status"], "conflict");
    assert_eq!(result["outcomes"][0]["server"]["payload"]["name"], "Second");
}

#[tokio::test]
async fn one_user_cannot_reach_anothers_workspace() {
    let server = start(SignupMode::Open).await;
    let ada = server.sign_up("ada@example.com").await;
    let mallory = server.sign_up("mallory@example.com").await;

    server
        .post_auth(
            "/v1/workspaces",
            &ada,
            json!({ "id": "w1", "name": "Private" }),
        )
        .await;

    // Knowing the id grants nothing, and a 404 does not confirm it exists.
    for path in [
        "/v1/workspaces/w1/head",
        "/v1/workspaces/w1/changes",
        "/v1/workspaces/w1/export",
        "/v1/workspaces/w1/members",
    ] {
        let response = server.get_auth(path, &mallory).await;
        assert_eq!(response.status(), 404, "{path} leaked");
    }

    let push = server
        .post_auth(
            "/v1/workspaces/w1/changes",
            &mallory,
            json!({ "clientId": "m", "mutations": [{
                "mutationId": "x", "kind": "request", "id": "r1",
                "baseRev": 0, "deleted": false, "payload": {}
            }]}),
        )
        .await;
    assert_eq!(push.status(), 404);

    // A workspace that genuinely does not exist responds identically.
    assert_eq!(
        server
            .get_auth("/v1/workspaces/never-existed/head", &mallory)
            .await
            .status(),
        404
    );

    // And it is not listed.
    let list: Value = server
        .get_auth("/v1/workspaces", &mallory)
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(list.as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn membership_can_be_granted_and_revoked() {
    let server = start(SignupMode::Open).await;
    let ada = server.sign_up("ada@example.com").await;
    let bob = server.sign_up("bob@example.com").await;

    server
        .post_auth(
            "/v1/workspaces",
            &ada,
            json!({ "id": "w1", "name": "Shared" }),
        )
        .await;

    let bob_id = server
        .get_auth("/v1/auth/me", &bob)
        .await
        .json::<Value>()
        .await
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    assert_eq!(
        server
            .get_auth("/v1/workspaces/w1/head", &bob)
            .await
            .status(),
        404
    );

    let granted = server
        .client
        .put(server.url(&format!("/v1/workspaces/w1/members/{bob_id}")))
        .bearer_auth(&ada)
        .json(&json!({ "role": "editor" }))
        .send()
        .await
        .unwrap();
    assert_eq!(granted.status(), 204);

    assert_eq!(
        server
            .get_auth("/v1/workspaces/w1/head", &bob)
            .await
            .status(),
        200
    );

    let members: Value = server
        .get_auth("/v1/workspaces/w1/members", &ada)
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(members.as_array().unwrap().len(), 2);

    let revoked = server
        .client
        .delete(server.url(&format!("/v1/workspaces/w1/members/{bob_id}")))
        .bearer_auth(&ada)
        .send()
        .await
        .unwrap();
    assert_eq!(revoked.status(), 204);

    assert_eq!(
        server
            .get_auth("/v1/workspaces/w1/head", &bob)
            .await
            .status(),
        404
    );
}

#[tokio::test]
async fn a_viewer_is_refused_at_the_http_layer() {
    let server = start(SignupMode::Open).await;
    let ada = server.sign_up("ada@example.com").await;
    let bob = server.sign_up("bob@example.com").await;

    server
        .post_auth(
            "/v1/workspaces",
            &ada,
            json!({ "id": "w1", "name": "Shared" }),
        )
        .await;

    let bob_id = server
        .get_auth("/v1/auth/me", &bob)
        .await
        .json::<Value>()
        .await
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    server
        .client
        .put(server.url(&format!("/v1/workspaces/w1/members/{bob_id}")))
        .bearer_auth(&ada)
        .json(&json!({ "role": "viewer" }))
        .send()
        .await
        .unwrap();

    assert_eq!(
        server
            .get_auth("/v1/workspaces/w1/changes", &bob)
            .await
            .status(),
        200
    );

    let push = server
        .post_auth(
            "/v1/workspaces/w1/changes",
            &bob,
            json!({ "clientId": "b", "mutations": [{
                "mutationId": "x", "kind": "request", "id": "r1",
                "baseRev": 0, "deleted": false, "payload": {}
            }]}),
        )
        .await;
    assert_eq!(push.status(), 404);
}

#[tokio::test]
async fn a_malformed_body_is_a_client_error_not_a_crash() {
    let server = start(SignupMode::Open).await;
    let token = server.sign_up("ada@example.com").await;
    server
        .post_auth(
            "/v1/workspaces",
            &token,
            json!({ "id": "w1", "name": "Team" }),
        )
        .await;

    let response = server
        .client
        .post(server.url("/v1/workspaces/w1/changes"))
        .bearer_auth(&token)
        .header("content-type", "application/json")
        .body("{ not json")
        .send()
        .await
        .unwrap();

    assert!(
        response.status().is_client_error(),
        "got {}",
        response.status()
    );

    // And the server is still answering.
    assert_eq!(server.get("/health").await.status(), 200);
}

#[tokio::test]
async fn an_internal_error_body_never_carries_detail() {
    let server = start(SignupMode::Open).await;
    let token = server.sign_up("ada@example.com").await;
    server
        .post_auth(
            "/v1/workspaces",
            &token,
            json!({ "id": "w1", "name": "Team" }),
        )
        .await;

    // An unknown entity kind is rejected by deserialisation.
    let response = server
        .post_auth(
            "/v1/workspaces/w1/changes",
            &token,
            json!({ "clientId": "d", "mutations": [{
                "mutationId": "m", "kind": "cookieJar", "id": "x",
                "baseRev": 0, "deleted": false, "payload": {}
            }]}),
        )
        .await;

    assert!(response.status().is_client_error());
    let body = response.text().await.unwrap();
    for leak in ["sqlite", "SELECT", "INSERT", "panicked", "src/"] {
        assert!(!body.contains(leak), "response leaked '{leak}': {body}");
    }
}

#[tokio::test]
async fn the_websocket_announces_the_head_on_connect_and_on_change() {
    use futures_util::StreamExt;

    let server = start(SignupMode::Open).await;
    let token = server.sign_up("ada@example.com").await;
    server
        .post_auth(
            "/v1/workspaces",
            &token,
            json!({ "id": "w1", "name": "Team" }),
        )
        .await;

    let request = ws_request(&server.base, "/v1/workspaces/w1/ws", &token);
    let (mut socket, _) = tokio_tungstenite::connect_async(request).await.unwrap();

    // The current head arrives immediately, so a reconnecting client knows
    // at once whether it is behind.
    let hello = socket.next().await.unwrap().unwrap();
    let parsed: Value = serde_json::from_str(hello.to_text().unwrap()).unwrap();
    assert_eq!(parsed["type"], "head");
    assert_eq!(parsed["seq"], 0);

    // A push elsewhere is announced.
    server
        .post_auth(
            "/v1/workspaces/w1/changes",
            &token,
            json!({ "clientId": "other", "mutations": [{
                "mutationId": "m1", "kind": "request", "id": "r1",
                "baseRev": 0, "deleted": false, "payload": { "name": "New" }
            }]}),
        )
        .await;

    let notification = tokio::time::timeout(std::time::Duration::from_secs(5), socket.next())
        .await
        .expect("no notification arrived")
        .unwrap()
        .unwrap();

    let parsed: Value = serde_json::from_str(notification.to_text().unwrap()).unwrap();
    assert_eq!(parsed["type"], "head");
    assert_eq!(parsed["seq"], 1);
    // It is a hint to pull and carries no workspace content.
    assert!(parsed.get("payload").is_none());
    assert!(parsed.get("objects").is_none());

    drop(socket);
}

#[tokio::test]
async fn the_websocket_refuses_a_non_member_at_the_handshake() {
    let server = start(SignupMode::Open).await;
    let ada = server.sign_up("ada@example.com").await;
    let mallory = server.sign_up("mallory@example.com").await;
    server
        .post_auth(
            "/v1/workspaces",
            &ada,
            json!({ "id": "w1", "name": "Private" }),
        )
        .await;

    // A genuine handshake, so the request reaches the handler rather than
    // being turned away by the upgrade extractor. Membership is resolved
    // before the upgrade, so the socket is never established — a connected
    // socket that silently delivered nothing would still confirm the
    // workspace exists.
    let request = ws_request(&server.base, "/v1/workspaces/w1/ws", &mallory);
    let outcome = tokio_tungstenite::connect_async(request).await;

    assert!(outcome.is_err(), "a non-member was given a websocket");

    // And a member on the same server still connects, so the refusal is
    // about membership rather than the handshake being broken.
    let request = ws_request(&server.base, "/v1/workspaces/w1/ws", &ada);
    assert!(tokio_tungstenite::connect_async(request).await.is_ok());
}

#[tokio::test]
async fn the_websocket_refuses_an_unauthenticated_handshake() {
    let server = start(SignupMode::Open).await;
    let ada = server.sign_up("ada@example.com").await;
    server
        .post_auth(
            "/v1/workspaces",
            &ada,
            json!({ "id": "w1", "name": "Private" }),
        )
        .await;

    let request = ws_request(&server.base, "/v1/workspaces/w1/ws", "not-a-token");
    assert!(tokio_tungstenite::connect_async(request).await.is_err());
}

// --- verification and password reset ---------------------------------------

/// Pulls the token out of a link in an e-mail body.
fn token_in(body: &str) -> String {
    body.split("token=")
        .nth(1)
        .unwrap_or_else(|| panic!("no token in the message:\n{body}"))
        .split_whitespace()
        .next()
        .unwrap()
        .to_string()
}

#[tokio::test]
async fn registering_sends_a_verification_link() {
    let server = start(SignupMode::Open).await;
    server.sign_up("ada@example.com").await;

    let sent = server.mail.sent();
    assert_eq!(sent.len(), 1, "expected one message, got {sent:?}");
    assert_eq!(sent[0].to, "ada@example.com");
    assert!(sent[0].subject.to_lowercase().contains("confirm"));
    assert!(
        sent[0].body.contains("/auth/verify?token="),
        "{}",
        sent[0].body
    );
}

#[tokio::test]
async fn confirming_marks_the_address_verified() {
    let server = start(SignupMode::Open).await;
    let token = server.sign_up("ada@example.com").await;

    let link = token_in(&server.mail.last().unwrap().body);
    let response = server
        .post("/v1/auth/verify/confirm", json!({ "token": link }))
        .await;
    assert_eq!(response.status(), 204);

    let me: Value = server
        .get_auth("/v1/auth/me", &token)
        .await
        .json()
        .await
        .unwrap();
    assert!(
        me["emailVerifiedAt"].is_number(),
        "the profile should report verification: {me}"
    );
}

#[tokio::test]
async fn a_verification_link_works_only_once() {
    let server = start(SignupMode::Open).await;
    server.sign_up("ada@example.com").await;
    let link = token_in(&server.mail.last().unwrap().body);

    assert_eq!(
        server
            .post("/v1/auth/verify/confirm", json!({ "token": link }))
            .await
            .status(),
        204
    );
    assert_eq!(
        server
            .post("/v1/auth/verify/confirm", json!({ "token": link }))
            .await
            .status(),
        400
    );
}

#[tokio::test]
async fn a_forged_token_is_refused() {
    let server = start(SignupMode::Open).await;
    server.sign_up("ada@example.com").await;

    let response = server
        .post(
            "/v1/auth/verify/confirm",
            json!({ "token": "not-a-real-token" }),
        )
        .await;
    assert_eq!(response.status(), 400);
}

#[tokio::test]
async fn an_unverified_user_cannot_sign_in_when_the_server_requires_it() {
    let server = start_requiring_verification().await;

    assert!(
        server
            .post(
                "/v1/auth/register",
                json!({ "email": "ada@example.com", "password": "correct horse battery" }),
            )
            .await
            .status()
            .is_success()
    );

    let response = server
        .post(
            "/v1/auth/login",
            json!({ "email": "ada@example.com", "password": "correct horse battery" }),
        )
        .await;
    assert_eq!(response.status(), 403);

    // And once confirmed, they can.
    let link = token_in(&server.mail.last().unwrap().body);
    server
        .post("/v1/auth/verify/confirm", json!({ "token": link }))
        .await;

    let response = server
        .post(
            "/v1/auth/login",
            json!({ "email": "ada@example.com", "password": "correct horse battery" }),
        )
        .await;
    assert_eq!(response.status(), 200);
}

#[tokio::test]
async fn a_wrong_password_still_reads_as_unauthenticated_not_unverified() {
    // The verification check sits after the password check, so it never
    // tells someone without the password that an address is registered.
    let server = start_requiring_verification().await;
    server
        .post(
            "/v1/auth/register",
            json!({ "email": "ada@example.com", "password": "correct horse battery" }),
        )
        .await;

    let response = server
        .post(
            "/v1/auth/login",
            json!({ "email": "ada@example.com", "password": "wrong" }),
        )
        .await;
    assert_eq!(
        response.status(),
        401,
        "403 would confirm the address exists"
    );
}

#[tokio::test]
async fn asking_to_verify_an_unknown_address_looks_the_same_as_a_known_one() {
    let server = start(SignupMode::Open).await;
    server.sign_up("ada@example.com").await;
    server.mail.clear();

    let known = server
        .post(
            "/v1/auth/verify/request",
            json!({ "email": "ada@example.com" }),
        )
        .await;
    let unknown = server
        .post(
            "/v1/auth/verify/request",
            json!({ "email": "nobody@example.com" }),
        )
        .await;

    assert_eq!(known.status(), 204);
    assert_eq!(unknown.status(), unknown.status());
    assert_eq!(
        known.status(),
        unknown.status(),
        "the two must be indistinguishable"
    );
}

#[tokio::test]
async fn a_password_can_be_reset_through_the_e_mailed_link() {
    let server = start(SignupMode::Open).await;
    server.sign_up("ada@example.com").await;
    server.mail.clear();

    assert_eq!(
        server
            .post(
                "/v1/auth/password/reset",
                json!({ "email": "ada@example.com" })
            )
            .await
            .status(),
        204
    );

    let message = server.mail.last().expect("a reset message");
    assert!(message.subject.to_lowercase().contains("reset"));
    let link = token_in(&message.body);

    assert_eq!(
        server
            .post(
                "/v1/auth/password/confirm",
                json!({ "token": link, "password": "a brand new passphrase" }),
            )
            .await
            .status(),
        204
    );

    // The new password works and the old one does not.
    assert_eq!(
        server
            .post(
                "/v1/auth/login",
                json!({ "email": "ada@example.com", "password": "a brand new passphrase" }),
            )
            .await
            .status(),
        200
    );
    assert_eq!(
        server
            .post(
                "/v1/auth/login",
                json!({ "email": "ada@example.com", "password": "correct horse battery" }),
            )
            .await
            .status(),
        401
    );
}

#[tokio::test]
async fn resetting_a_password_signs_existing_sessions_out() {
    // Someone resets a password precisely when they fear another party
    // holds a session. That session must not survive it.
    let server = start(SignupMode::Open).await;
    let attacker_token = server.sign_up("ada@example.com").await;
    assert_eq!(
        server
            .get_auth("/v1/auth/me", &attacker_token)
            .await
            .status(),
        200
    );

    server.mail.clear();
    server
        .post(
            "/v1/auth/password/reset",
            json!({ "email": "ada@example.com" }),
        )
        .await;
    let link = token_in(&server.mail.last().unwrap().body);

    server
        .post(
            "/v1/auth/password/confirm",
            json!({ "token": link, "password": "a brand new passphrase" }),
        )
        .await;

    assert_eq!(
        server
            .get_auth("/v1/auth/me", &attacker_token)
            .await
            .status(),
        401,
        "the session outlived the reset"
    );
}

#[tokio::test]
async fn a_reset_link_works_only_once() {
    let server = start(SignupMode::Open).await;
    server.sign_up("ada@example.com").await;
    server.mail.clear();

    server
        .post(
            "/v1/auth/password/reset",
            json!({ "email": "ada@example.com" }),
        )
        .await;
    let link = token_in(&server.mail.last().unwrap().body);

    let body = json!({ "token": link, "password": "a brand new passphrase" });
    assert_eq!(
        server
            .post("/v1/auth/password/confirm", body.clone())
            .await
            .status(),
        204
    );
    assert_eq!(
        server
            .post("/v1/auth/password/confirm", body)
            .await
            .status(),
        400
    );
}

#[tokio::test]
async fn a_verification_token_cannot_reset_a_password() {
    // The two are separate purposes in the store. If they were not,
    // anyone who could read a verification e-mail could take the account.
    let server = start(SignupMode::Open).await;
    server.sign_up("ada@example.com").await;
    let verification = token_in(&server.mail.last().unwrap().body);

    let response = server
        .post(
            "/v1/auth/password/confirm",
            json!({ "token": verification, "password": "attacker chosen" }),
        )
        .await;
    assert_eq!(response.status(), 400);

    // The original password still works.
    assert_eq!(
        server
            .post(
                "/v1/auth/login",
                json!({ "email": "ada@example.com", "password": "correct horse battery" }),
            )
            .await
            .status(),
        200
    );
}

#[tokio::test]
async fn asking_for_a_reset_for_an_unknown_address_reveals_nothing() {
    let server = start(SignupMode::Open).await;
    server.mail.clear();

    let response = server
        .post(
            "/v1/auth/password/reset",
            json!({ "email": "nobody@example.com" }),
        )
        .await;

    assert_eq!(response.status(), 204, "a 404 here enumerates accounts");
    assert!(server.mail.sent().is_empty());
}

#[tokio::test]
async fn repeated_requests_within_the_cooldown_send_one_message() {
    // Without this the endpoint is a way to post mail at any address.
    let server = start(SignupMode::Open).await;
    server.sign_up("ada@example.com").await;
    server.mail.clear();

    for _ in 0..5 {
        server
            .post(
                "/v1/auth/password/reset",
                json!({ "email": "ada@example.com" }),
            )
            .await;
    }

    assert_eq!(
        server.mail.sent().len(),
        1,
        "the cooldown should have suppressed the rest"
    );
}
