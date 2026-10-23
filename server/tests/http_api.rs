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
    /// The backing store, for seeding past a closed registration.
    store: Arc<dyn SyncStore>,
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
        // Never empty in production — `Config::from_env` falls back to
        // the bind address — so a fixture that left it blank produced
        // relative links no test would notice were wrong.
        public_url: "https://relay.test".into(),
        signup_mode: signup,
        session_ttl: std::time::Duration::from_secs(3600),
        ..Config::default()
    };

    start_with(store, config).await
}

/// Starts a server that refuses sign-in until the address is confirmed.
async fn start_requiring_verification() -> TestServer {
    let store = SqliteStore::connect("sqlite::memory:").await.unwrap();
    store.migrate().await.unwrap();

    let config = Config {
        public_url: "https://relay.test".into(),
        signup_mode: SignupMode::Open,
        session_ttl: std::time::Duration::from_secs(3600),
        require_verified_email: true,
        ..Config::default()
    };

    start_with(store, config).await
}

async fn start_with(store: SqliteStore, config: Config) -> TestServer {
    let mail = Arc::new(relay_server::mail::CapturingMailer::new());
    let store: Arc<dyn SyncStore> = Arc::new(store);
    let state = AppState::with_mailer(store.clone(), config, mail.clone());
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
        store,
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

// --- following the link, as a person would --------------------------------

/// The whole link out of a message body, not just its token.
fn link_in(body: &str) -> String {
    body.split_whitespace()
        .find(|word| word.starts_with("http") && word.contains("token="))
        .unwrap_or_else(|| panic!("no link in the message:\n{body}"))
        .to_string()
}

#[tokio::test]
async fn the_verification_link_in_the_e_mail_actually_works() {
    // Every other test posts to the JSON API. This one follows the URL
    // the recipient is given, which is the only thing they can do, and
    // is how a 404 in a shipped feature goes unnoticed.
    let server = start(SignupMode::Open).await;
    let token = server.sign_up("ada@example.com").await;

    let link = link_in(&server.mail.last().unwrap().body);
    assert!(
        link.starts_with("https://relay.test/auth/verify?token="),
        "{link}"
    );

    // Point it at the test server rather than the configured origin.
    let path = link.strip_prefix("https://relay.test").unwrap();
    let response = server.get(path).await;

    assert_eq!(response.status(), 200, "the e-mailed link 404s");
    let html = response.text().await.unwrap();
    assert!(html.contains("Address confirmed"), "{html}");

    let me: Value = server
        .get_auth("/v1/auth/me", &token)
        .await
        .json()
        .await
        .unwrap();
    assert!(me["emailVerifiedAt"].is_number(), "{me}");
}

#[tokio::test]
async fn a_spent_verification_link_explains_itself_rather_than_erroring() {
    let server = start(SignupMode::Open).await;
    server.sign_up("ada@example.com").await;
    let path = link_in(&server.mail.last().unwrap().body)
        .strip_prefix("https://relay.test")
        .unwrap()
        .to_string();

    server.get(&path).await;
    let second = server.get(&path).await;

    assert_eq!(second.status(), 200);
    let html = second.text().await.unwrap();
    assert!(html.contains("no longer works"), "{html}");
}

#[tokio::test]
async fn the_reset_link_shows_a_form_and_changes_nothing() {
    let server = start(SignupMode::Open).await;
    server.sign_up("ada@example.com").await;
    server.mail.clear();
    server
        .post(
            "/v1/auth/password/reset",
            json!({ "email": "ada@example.com" }),
        )
        .await;

    let path = link_in(&server.mail.last().unwrap().body)
        .strip_prefix("https://relay.test")
        .unwrap()
        .to_string();

    let html = server.get(&path).await.text().await.unwrap();
    assert!(html.contains("<form"), "{html}");
    assert!(html.contains("method=\"post\""));

    // Loading the page must not have spent the token: a link scanner
    // following it would otherwise lock the user out of their own reset.
    let original_still_works = server
        .post(
            "/v1/auth/login",
            json!({ "email": "ada@example.com", "password": "correct horse battery" }),
        )
        .await;
    assert_eq!(original_still_works.status(), 200);

    let again = server.get(&path).await.text().await.unwrap();
    assert!(again.contains("<form"), "the form vanished after one view");
}

#[tokio::test]
async fn submitting_the_reset_form_changes_the_password() {
    let server = start(SignupMode::Open).await;
    server.sign_up("ada@example.com").await;
    server.mail.clear();
    server
        .post(
            "/v1/auth/password/reset",
            json!({ "email": "ada@example.com" }),
        )
        .await;
    let token = token_in(&server.mail.last().unwrap().body);

    let response = server
        .client
        .post(server.url("/auth/reset"))
        .form(&[
            ("token", token.as_str()),
            ("password", "a brand new passphrase"),
            ("confirm", "a brand new passphrase"),
        ])
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), 200);
    assert!(response.text().await.unwrap().contains("Password changed"));

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
}

#[tokio::test]
async fn mismatched_passwords_are_caught_before_the_token_is_spent() {
    let server = start(SignupMode::Open).await;
    server.sign_up("ada@example.com").await;
    server.mail.clear();
    server
        .post(
            "/v1/auth/password/reset",
            json!({ "email": "ada@example.com" }),
        )
        .await;
    let token = token_in(&server.mail.last().unwrap().body);

    let typo = server
        .client
        .post(server.url("/auth/reset"))
        .form(&[
            ("token", token.as_str()),
            ("password", "a brand new passphrase"),
            ("confirm", "a brand new passphrasr"),
        ])
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();

    assert!(typo.contains("did not match"), "{typo}");

    // And the link still works, rather than being burned by a typo.
    let retry = server
        .client
        .post(server.url("/auth/reset"))
        .form(&[
            ("token", token.as_str()),
            ("password", "a brand new passphrase"),
            ("confirm", "a brand new passphrase"),
        ])
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();

    assert!(retry.contains("Password changed"), "{retry}");
}

#[tokio::test]
async fn a_crafted_token_cannot_inject_markup_into_the_page() {
    let server = start(SignupMode::Open).await;

    let html = server
        .get("/auth/reset?token=%22%3E%3Cscript%3Ealert(1)%3C/script%3E")
        .await
        .text()
        .await
        .unwrap();

    assert!(!html.contains("<script>"), "{html}");
    assert!(html.contains("&lt;script&gt;"));
}

// --- invitations over HTTP -------------------------------------------------

/// A server where registration needs an invitation.
async fn start_invite_only() -> TestServer {
    let store = SqliteStore::connect("sqlite::memory:").await.unwrap();
    store.migrate().await.unwrap();

    let config = Config {
        database_url: "sqlite::memory:".into(),
        bind: "127.0.0.1:0".parse().unwrap(),
        public_url: "https://relay.test".into(),
        signup_mode: SignupMode::Invite,
        session_ttl: std::time::Duration::from_secs(3600),
        admin_email: None,
        admin_password: None,
        max_body_bytes: relay_sync::MAX_BATCH_BYTES + 65536,
        resend_api_key: None,
        mail_from: None,
        require_verified_email: false,
        stripe_secret_key: None,
        stripe_webhook_secret: None,
        stripe_price_id: None,
        stripe_return_url: None,
    };

    start_with(store, config).await
}

/// The first account on an invite-only server, with a workspace.
///
/// Created straight in the store: registration is shut to everyone
/// without a link, which is the situation a real operator bootstraps
/// out of with `RELAY_ADMIN_EMAIL`.
async fn owner_with_workspace_invite_only(server: &TestServer) -> String {
    let hash = relay_server::auth::hash_password("correct horse battery").unwrap();
    server
        .store
        .create_user(relay_server::store::NewUser {
            email: "ada@example.com".into(),
            password_hash: hash,
            display_name: "Ada".into(),
            is_server_admin: false,
        })
        .await
        .unwrap();

    let token = server
        .post(
            "/v1/auth/login",
            json!({ "email": "ada@example.com", "password": "correct horse battery" }),
        )
        .await
        .json::<Value>()
        .await
        .unwrap()["token"]
        .as_str()
        .unwrap()
        .to_string();

    server
        .post_auth(
            "/v1/workspaces",
            &token,
            json!({ "id": "w1", "name": "Team" }),
        )
        .await;

    token
}

/// Creates a workspace owned by a freshly registered user.
async fn owner_with_workspace(server: &TestServer) -> String {
    let token = server.sign_up("ada@example.com").await;
    let response = server
        .post_auth(
            "/v1/workspaces",
            &token,
            json!({ "id": "w1", "name": "Team" }),
        )
        .await;
    assert!(response.status().is_success());
    token
}

#[tokio::test]
async fn an_admin_can_invite_and_the_invitee_gets_a_link() {
    let server = start(SignupMode::Open).await;
    let token = owner_with_workspace(&server).await;
    server.mail.clear();

    let response = server
        .post_auth(
            "/v1/workspaces/w1/invitations",
            &token,
            json!({ "email": "grace@example.com", "role": "editor" }),
        )
        .await;
    assert_eq!(response.status(), 200);

    let sent = server.mail.last().expect("an invitation e-mail");
    assert_eq!(sent.to, "grace@example.com");
    assert!(sent.body.contains("/invite?token="), "{}", sent.body);
    assert!(sent.body.contains("editor"), "{}", sent.body);
}

#[tokio::test]
async fn the_invitation_link_says_what_is_on_offer() {
    let server = start(SignupMode::Open).await;
    let token = owner_with_workspace(&server).await;
    server.mail.clear();
    server
        .post_auth(
            "/v1/workspaces/w1/invitations",
            &token,
            json!({ "email": "grace@example.com", "role": "editor" }),
        )
        .await;

    let path = link_in(&server.mail.last().unwrap().body)
        .strip_prefix("https://relay.test")
        .unwrap()
        .to_string();

    let html = server.get(&path).await.text().await.unwrap();
    assert!(html.contains("Team"), "{html}");
    assert!(html.contains("editor"));
    assert!(html.contains("grace@example.com"));
    // It must not ask for a password: that is what a phishing page
    // sent to the same address would do.
    assert!(!html.contains("type=\"password\""), "{html}");
}

#[tokio::test]
async fn an_invited_person_can_register_and_lands_in_the_workspace() {
    let server = start_invite_only().await;

    // Registration is closed without one.
    assert_eq!(
        server
            .post(
                "/v1/auth/register",
                json!({ "email": "nobody@example.com", "password": "correct horse battery" }),
            )
            .await
            .status(),
        404
    );

    // The first account still has to exist; make it directly in the store.
    let token = owner_with_workspace_invite_only(&server).await;
    server.mail.clear();
    server
        .post_auth(
            "/v1/workspaces/w1/invitations",
            &token,
            json!({ "email": "grace@example.com", "role": "editor" }),
        )
        .await;
    let invitation = token_in(&server.mail.last().unwrap().body);

    let response = server
        .post(
            "/v1/auth/register",
            json!({
                "email": "grace@example.com",
                "password": "correct horse battery",
                "invitation": invitation,
            }),
        )
        .await;
    assert_eq!(response.status(), 200, "{}", response.text().await.unwrap());

    // And they are in the workspace, as an editor.
    let guest = server
        .post(
            "/v1/auth/login",
            json!({ "email": "grace@example.com", "password": "correct horse battery" }),
        )
        .await
        .json::<Value>()
        .await
        .unwrap()["token"]
        .as_str()
        .unwrap()
        .to_string();

    let workspaces: Value = server
        .get_auth("/v1/workspaces", &guest)
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(workspaces[0]["id"], "w1");
}

#[tokio::test]
async fn an_invitation_cannot_be_used_to_register_a_different_address() {
    let server = start_invite_only().await;
    let token = owner_with_workspace_invite_only(&server).await;
    server.mail.clear();
    server
        .post_auth(
            "/v1/workspaces/w1/invitations",
            &token,
            json!({ "email": "grace@example.com", "role": "admin" }),
        )
        .await;
    let invitation = token_in(&server.mail.last().unwrap().body);

    let response = server
        .post(
            "/v1/auth/register",
            json!({
                "email": "mallory@example.com",
                "password": "correct horse battery",
                "invitation": invitation,
            }),
        )
        .await;

    assert_eq!(response.status(), 400);
    let body = response.text().await.unwrap();
    assert!(body.contains("grace@example.com"), "{body}");
}

#[tokio::test]
async fn an_editor_cannot_invite_an_administrator() {
    let server = start(SignupMode::Open).await;
    let owner = owner_with_workspace(&server).await;
    let guest_token = server.sign_up("grace@example.com").await;

    let guest_id: String = server
        .get_auth("/v1/auth/me", &guest_token)
        .await
        .json::<Value>()
        .await
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    server
        .client
        .put(server.url(&format!("/v1/workspaces/w1/members/{guest_id}")))
        .bearer_auth(&owner)
        .json(&json!({ "role": "editor" }))
        .send()
        .await
        .unwrap();

    let response = server
        .post_auth(
            "/v1/workspaces/w1/invitations",
            &guest_token,
            json!({ "email": "mallory@example.com", "role": "admin" }),
        )
        .await;

    assert_eq!(response.status(), 404, "an editor widened the membership");
}

#[tokio::test]
async fn an_invitation_defaults_to_the_least_privileged_role() {
    let server = start(SignupMode::Open).await;
    let token = owner_with_workspace(&server).await;

    let invitation: Value = server
        .post_auth(
            "/v1/workspaces/w1/invitations",
            &token,
            json!({ "email": "grace@example.com" }),
        )
        .await
        .json()
        .await
        .unwrap();

    assert_eq!(invitation["role"], "viewer");
}

#[tokio::test]
async fn pending_invitations_can_be_listed_and_revoked() {
    let server = start(SignupMode::Open).await;
    let token = owner_with_workspace(&server).await;

    let created: Value = server
        .post_auth(
            "/v1/workspaces/w1/invitations",
            &token,
            json!({ "email": "grace@example.com", "role": "viewer" }),
        )
        .await
        .json()
        .await
        .unwrap();

    let listed: Value = server
        .get_auth("/v1/workspaces/w1/invitations", &token)
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(listed.as_array().unwrap().len(), 1);

    let id = created["id"].as_str().unwrap();
    let response = server
        .client
        .delete(server.url(&format!("/v1/workspaces/w1/invitations/{id}")))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 204);

    let after: Value = server
        .get_auth("/v1/workspaces/w1/invitations", &token)
        .await
        .json()
        .await
        .unwrap();
    assert!(after.as_array().unwrap().is_empty());
}

#[tokio::test]
async fn a_non_member_cannot_list_who_has_been_invited() {
    // The pending list is a list of colleagues' e-mail addresses.
    let server = start(SignupMode::Open).await;
    let owner = owner_with_workspace(&server).await;
    server
        .post_auth(
            "/v1/workspaces/w1/invitations",
            &owner,
            json!({ "email": "grace@example.com" }),
        )
        .await;

    let outsider = server.sign_up("mallory@example.com").await;
    let response = server
        .get_auth("/v1/workspaces/w1/invitations", &outsider)
        .await;

    assert_eq!(response.status(), 404);
}

// --- billing ---------------------------------------------------------------

/// A stand-in for Stripe, so the checkout and portal routes can be
/// driven end to end. It records what it was sent: the point of these
/// tests is as much *what Relay asks Stripe for* — the seat count, the
/// workspace id in the metadata — as what it does with the answer.
struct StubStripe {
    base: String,
    calls: Arc<std::sync::Mutex<Vec<(String, String)>>>,
    handle: tokio::task::JoinHandle<()>,
}

impl Drop for StubStripe {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

impl StubStripe {
    async fn start() -> Self {
        let calls: Arc<std::sync::Mutex<Vec<(String, String)>>> = Arc::default();
        let recorder = calls.clone();

        let app = axum::Router::new().fallback(axum::routing::any(
            move |uri: axum::http::Uri, body: String| {
                let recorder = recorder.clone();
                async move {
                    recorder
                        .lock()
                        .unwrap()
                        .push((uri.path().to_string(), body));
                    axum::Json(json!({ "id": "cs_test_1", "url": "https://stripe.test/pay" }))
                }
            },
        ));

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        Self {
            base: format!("http://{addr}"),
            calls,
            handle,
        }
    }

    /// The body of the one call made to `path`.
    fn body(&self, path: &str) -> String {
        let calls = self.calls.lock().unwrap();
        let matching: Vec<_> = calls.iter().filter(|(p, _)| p == path).collect();
        assert_eq!(
            matching.len(),
            1,
            "expected one call to {path}, got {calls:?}"
        );
        matching[0].1.clone()
    }

    fn was_called(&self) -> bool {
        !self.calls.lock().unwrap().is_empty()
    }
}

const WEBHOOK_SECRET: &str = "whsec_test_http";

/// Signs a body the way Stripe signs a webhook.
fn stripe_signature(body: &str, timestamp: i64) -> String {
    use hmac::{Hmac, KeyInit, Mac};
    let mut mac = <Hmac<sha2::Sha256>>::new_from_slice(WEBHOOK_SECRET.as_bytes()).unwrap();
    mac.update(format!("{timestamp}.{body}").as_bytes());
    let hex: String = mac
        .finalize()
        .into_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("t={timestamp},v1={hex}")
}

fn now_seconds() -> i64 {
    relay_models::Timestamp::now().as_millis() / 1000
}

/// Starts a server that charges, with Stripe pointed at the stub.
async fn start_billing(stub: &StubStripe) -> TestServer {
    let store = SqliteStore::connect("sqlite::memory:").await.unwrap();
    store.migrate().await.unwrap();

    let config = Config {
        public_url: "https://relay.test".into(),
        signup_mode: SignupMode::Open,
        session_ttl: std::time::Duration::from_secs(3600),
        stripe_secret_key: Some("sk_test_whatever".into()),
        stripe_webhook_secret: Some(WEBHOOK_SECRET.into()),
        stripe_price_id: Some("price_seat".into()),
        ..Config::default()
    };

    let mail = Arc::new(relay_server::mail::CapturingMailer::new());
    let store: Arc<dyn SyncStore> = Arc::new(store);
    let mut state = AppState::with_mailer(store.clone(), config, mail.clone());
    state.stripe = Some(Arc::new(
        relay_server::billing::stripe::Stripe::new("sk_test_whatever", "price_seat")
            .unwrap()
            .with_base_url(&stub.base),
    ));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, relay_server::router(state)).await;
    });

    TestServer {
        base: format!("http://{addr}"),
        client: reqwest::Client::new(),
        handle,
        mail,
        store,
    }
}

/// Posts a webhook body with a valid signature.
async fn send_webhook(server: &TestServer, body: Value) -> reqwest::Response {
    let raw = body.to_string();
    let signature = stripe_signature(&raw, now_seconds());
    server
        .client
        .post(server.url("/v1/billing/webhook"))
        .header("stripe-signature", signature)
        .header("content-type", "application/json")
        .body(raw)
        .send()
        .await
        .unwrap()
}

#[tokio::test]
async fn a_self_hosted_server_charges_for_nothing() {
    // The whole product promise: run it yourself and no seat is
    // counted against you. `metered` false is what the UI hangs the
    // entire billing panel on.
    let server = start(SignupMode::Open).await;
    let token = owner_with_workspace(&server).await;

    let response = server.get_auth("/v1/workspaces/w1/billing", &token).await;
    assert_eq!(response.status(), 200);

    let body: Value = response.json().await.unwrap();
    assert_eq!(body["metered"], false);
    assert_eq!(body["active"], true);
    assert_eq!(body["members"], 1);
    assert_eq!(body["hasSubscription"], false);
    assert!(body.get("seats").is_none(), "unmetered means no seat limit");
}

#[tokio::test]
async fn billing_tells_a_stranger_nothing_about_a_workspace() {
    let server = start(SignupMode::Open).await;
    owner_with_workspace(&server).await;
    let outsider = server.sign_up("mallory@example.com").await;

    // 404, not 403: the same rule as everywhere else here — 403 would
    // confirm the id exists.
    let response = server
        .get_auth("/v1/workspaces/w1/billing", &outsider)
        .await;
    assert_eq!(response.status(), 404);
}

#[tokio::test]
async fn a_server_that_does_not_charge_refuses_to_start_a_checkout() {
    let server = start(SignupMode::Open).await;
    let token = owner_with_workspace(&server).await;

    let response = server
        .post_auth("/v1/workspaces/w1/billing/checkout", &token, json!({}))
        .await;
    assert_eq!(response.status(), 400);
}

#[tokio::test]
async fn a_checkout_buys_a_seat_for_every_member() {
    // Asking for fewer seats than there are people would take payment
    // for a workspace that then refuses to let them in.
    let stub = StubStripe::start().await;
    let server = start_billing(&stub).await;
    let ada = owner_with_workspace(&server).await;
    let bob = server.sign_up("bob@example.com").await;

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
        .json(&json!({ "role": "editor" }))
        .send()
        .await
        .unwrap();

    let response = server
        .post_auth(
            "/v1/workspaces/w1/billing/checkout",
            &ada,
            json!({ "seats": 1 }),
        )
        .await;
    assert_eq!(response.status(), 200);
    assert_eq!(
        response.json::<Value>().await.unwrap()["url"],
        "https://stripe.test/pay"
    );

    let form = stub.body("/v1/checkout/sessions");
    assert!(
        form.contains("quantity%5D=2") || form.contains("quantity]=2"),
        "one seat was asked for but there are two members: {form}"
    );
    // Both places, because only one of them comes back on each event.
    assert!(form.contains("client_reference_id=w1"), "{form}");
    assert!(form.contains("workspace_id%5D=w1"), "{form}");
}

#[tokio::test]
async fn only_an_administrator_commits_the_workspace_to_a_bill() {
    let stub = StubStripe::start().await;
    let server = start_billing(&stub).await;
    let ada = owner_with_workspace(&server).await;
    let bob = server.sign_up("bob@example.com").await;

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
        .json(&json!({ "role": "editor" }))
        .send()
        .await
        .unwrap();

    let response = server
        .post_auth("/v1/workspaces/w1/billing/checkout", &bob, json!({}))
        .await;
    assert_eq!(response.status(), 404);
    assert!(
        !stub.was_called(),
        "Stripe was reached before the role check"
    );
}

#[tokio::test]
async fn the_portal_needs_a_subscription_to_open() {
    let stub = StubStripe::start().await;
    let server = start_billing(&stub).await;
    let token = owner_with_workspace(&server).await;

    let response = server
        .post_auth("/v1/workspaces/w1/billing/portal", &token, json!({}))
        .await;
    assert_eq!(response.status(), 400);
    assert!(!stub.was_called());
}

#[tokio::test]
async fn a_forged_webhook_buys_nothing() {
    // The reason the signature check exists. The endpoint is public,
    // so without it anyone could POST themselves a thousand seats.
    let stub = StubStripe::start().await;
    let server = start_billing(&stub).await;
    let token = owner_with_workspace(&server).await;

    let body = json!({
        "id": "evt_forged",
        "type": "customer.subscription.updated",
        "data": { "object": {
            "id": "sub_1",
            "customer": "cus_1",
            "status": "active",
            "metadata": { "workspace_id": "w1" },
            "items": { "data": [{ "quantity": 1000 }] }
        }}
    })
    .to_string();

    let response = server
        .client
        .post(server.url("/v1/billing/webhook"))
        .header("stripe-signature", "t=1,v1=deadbeef")
        .header("content-type", "application/json")
        .body(body)
        .send()
        .await
        .unwrap();
    assert!(!response.status().is_success());

    let billing: Value = server
        .get_auth("/v1/workspaces/w1/billing", &token)
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(billing["hasSubscription"], false);
    assert!(billing.get("seats").is_none());
}

#[tokio::test]
async fn a_webhook_signed_hours_ago_is_refused() {
    // Replay. A signature captured once would otherwise work for ever.
    let stub = StubStripe::start().await;
    let server = start_billing(&stub).await;
    owner_with_workspace(&server).await;

    let raw = json!({ "id": "evt_old", "type": "customer.subscription.deleted",
                      "data": { "object": { "metadata": { "workspace_id": "w1" } } } })
    .to_string();
    let signature = stripe_signature(&raw, now_seconds() - 7200);

    let response = server
        .client
        .post(server.url("/v1/billing/webhook"))
        .header("stripe-signature", signature)
        .header("content-type", "application/json")
        .body(raw)
        .send()
        .await
        .unwrap();
    assert!(!response.status().is_success());
}

#[tokio::test]
async fn a_genuine_webhook_puts_the_workspace_in_good_standing() {
    let stub = StubStripe::start().await;
    let server = start_billing(&stub).await;
    let token = owner_with_workspace(&server).await;

    let response = send_webhook(
        &server,
        json!({
            "id": "evt_1",
            "type": "customer.subscription.updated",
            "data": { "object": {
                "id": "sub_1",
                "customer": "cus_1",
                "status": "active",
                "current_period_end": 1_800_000_000i64,
                "metadata": { "workspace_id": "w1" },
                "items": { "data": [{ "quantity": 5 }] }
            }}
        }),
    )
    .await;
    assert_eq!(response.status(), 204);

    let billing: Value = server
        .get_auth("/v1/workspaces/w1/billing", &token)
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(billing["metered"], true);
    assert_eq!(billing["active"], true);
    assert_eq!(billing["seats"], 5);
    assert_eq!(billing["hasSubscription"], true);
}

#[tokio::test]
async fn the_same_event_twice_is_applied_once() {
    // Stripe retries until it gets a 2xx, so every event arrives more
    // than once. The guard is on the event id, so a second body
    // wearing the same id must change nothing.
    let stub = StubStripe::start().await;
    let server = start_billing(&stub).await;
    let token = owner_with_workspace(&server).await;

    let event = |seats: u64| {
        json!({
            "id": "evt_same",
            "type": "customer.subscription.updated",
            "data": { "object": {
                "id": "sub_1", "customer": "cus_1", "status": "active",
                "metadata": { "workspace_id": "w1" },
                "items": { "data": [{ "quantity": seats }] }
            }}
        })
    };

    assert_eq!(send_webhook(&server, event(5)).await.status(), 204);
    assert_eq!(send_webhook(&server, event(500)).await.status(), 204);

    let billing: Value = server
        .get_auth("/v1/workspaces/w1/billing", &token)
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(billing["seats"], 5, "the replay was applied");
}

#[tokio::test]
async fn an_event_naming_no_workspace_is_acknowledged_anyway() {
    // Anything but a 2xx and Stripe retries it for days.
    let stub = StubStripe::start().await;
    let server = start_billing(&stub).await;
    owner_with_workspace(&server).await;

    let response = send_webhook(
        &server,
        json!({
            "id": "evt_orphan",
            "type": "customer.subscription.updated",
            "data": { "object": { "id": "sub_9", "customer": "cus_9", "status": "active" } }
        }),
    )
    .await;
    assert_eq!(response.status(), 204);
}

#[tokio::test]
async fn an_event_type_this_build_ignores_is_acknowledged() {
    let stub = StubStripe::start().await;
    let server = start_billing(&stub).await;
    owner_with_workspace(&server).await;

    let response = send_webhook(
        &server,
        json!({
            "id": "evt_invoice",
            "type": "invoice.payment_succeeded",
            "data": { "object": { "customer": "cus_1" } }
        }),
    )
    .await;
    assert_eq!(response.status(), 204);
}

#[tokio::test]
async fn a_later_event_finds_the_workspace_through_the_customer() {
    // Only the checkout carries the workspace in client_reference_id.
    // Later events name a customer, and the subscription metadata is
    // the bridge — but when that is missing too, the customer already
    // on file has to be enough or a cancellation would never land.
    let stub = StubStripe::start().await;
    let server = start_billing(&stub).await;
    let token = owner_with_workspace(&server).await;

    send_webhook(
        &server,
        json!({
            "id": "evt_checkout",
            "type": "checkout.session.completed",
            "data": { "object": {
                "customer": "cus_7", "subscription": "sub_7",
                "client_reference_id": "w1"
            }}
        }),
    )
    .await;

    let response = send_webhook(
        &server,
        json!({
            "id": "evt_gone",
            "type": "customer.subscription.deleted",
            "data": { "object": { "id": "sub_7", "customer": "cus_7", "status": "canceled" } }
        }),
    )
    .await;
    assert_eq!(response.status(), 204);

    let billing: Value = server
        .get_auth("/v1/workspaces/w1/billing", &token)
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(billing["active"], false);
    assert!(
        billing["notice"].is_string(),
        "a cancelled workspace must say why it is blocked"
    );
}

#[tokio::test]
async fn the_portal_opens_once_there_is_a_subscription() {
    let stub = StubStripe::start().await;
    let server = start_billing(&stub).await;
    let token = owner_with_workspace(&server).await;

    send_webhook(
        &server,
        json!({
            "id": "evt_ck",
            "type": "checkout.session.completed",
            "data": { "object": {
                "customer": "cus_42", "subscription": "sub_42",
                "client_reference_id": "w1"
            }}
        }),
    )
    .await;

    let response = server
        .post_auth("/v1/workspaces/w1/billing/portal", &token, json!({}))
        .await;
    assert_eq!(response.status(), 200);
    assert_eq!(
        response.json::<Value>().await.unwrap()["url"],
        "https://stripe.test/pay"
    );

    let form = stub.body("/v1/billing_portal/sessions");
    assert!(form.contains("customer=cus_42"), "{form}");
}
