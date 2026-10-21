//! Two desktop clients synchronising through a real server.
//!
//! Each "client" is its own SQLite database and its own engine, talking
//! over HTTP to a server process in the same test. Nothing is stubbed:
//! this is the arrangement two people on two laptops actually have.

use std::sync::Arc;

use relay_app::sync::{Resolution, SyncClient, SyncEngine, initial_state};
use relay_app::{MemorySecretStore, RelayService};
use relay_http::HttpEngine;
use relay_models::{Collection, RequestDoc, Workspace, WorkspaceId};
use relay_server::config::{Config, SignupMode};
use relay_server::store::SyncStore;
use relay_server::{AppState, SqliteStore, router};
use relay_storage::{Database, SyncRepo};

/// A running sync server.
struct Server {
    base: String,
    handle: tokio::task::JoinHandle<()>,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

async fn start_server() -> Server {
    let store = SqliteStore::connect("sqlite::memory:").await.unwrap();
    store.migrate().await.unwrap();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let config = Config {
        database_url: "sqlite::memory:".into(),
        bind: addr,
        public_url: String::new(),
        signup_mode: SignupMode::Open,
        session_ttl: std::time::Duration::from_secs(3600),
        admin_email: None,
        admin_password: None,
        max_body_bytes: relay_sync::MAX_BATCH_BYTES + 65536,
    };

    let state = AppState::new(Arc::new(store), config);
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, router(state)).await;
    });

    Server {
        base: format!("http://{addr}"),
        handle,
    }
}

/// One desktop installation: its own database, service and sync engine.
struct Client {
    service: RelayService,
    engine: SyncEngine,
    db: Database,
    workspace: WorkspaceId,
}

impl Client {
    /// Creates a local installation with one workspace.
    async fn new(workspace_id: &str, name: &str) -> Self {
        let db = Database::open_in_memory().unwrap();
        let secrets = Arc::new(MemorySecretStore::new());
        let service = RelayService::new(db.clone(), HttpEngine::with_defaults(), secrets.clone());

        // Both installations use the same workspace id, which is what
        // happens when one of them created it and the other joined.
        let mut workspace = Workspace::new(name);
        workspace.id = WorkspaceId::from(workspace_id);
        let saved = workspace.clone();
        db.call(move |c| relay_storage::WorkspaceRepo::save(c, &saved))
            .await
            .unwrap();

        Self {
            service,
            engine: SyncEngine::new(db.clone(), secrets),
            db,
            workspace: WorkspaceId::from(workspace_id),
        }
    }

    /// Registers, signs in and connects this installation to the server.
    async fn connect(&self, server: &Server, email: &str) {
        let client = SyncClient::new(&server.base).unwrap();

        let http = reqwest::Client::new();
        let _ = http
            .post(format!("{}/v1/auth/register", server.base))
            .json(&serde_json::json!({ "email": email, "password": "correct horse battery" }))
            .send()
            .await
            .unwrap();

        let login = client.login(email, "correct horse battery").await.unwrap();
        self.engine
            .set_token(&self.workspace, &login.token)
            .unwrap();

        let state = initial_state(&self.workspace, &server.base, None, &login.user);
        self.db
            .call(move |c| SyncRepo::set_state(c, &state))
            .await
            .unwrap();
    }

    /// Joins a workspace someone else already created.
    async fn join(&self, server: &Server, email: &str) {
        self.connect(server, email).await;
        self.engine.prepare_join(&self.workspace).await.unwrap();
    }

    /// Creates the workspace on the server.
    async fn create_remote(&self, server: &Server) {
        let client = SyncClient::new(&server.base).unwrap();
        let token = self.engine.token(&self.workspace).unwrap();
        client
            .create_workspace(&token, self.workspace.as_str(), "Shared")
            .await
            .unwrap();
    }

    async fn sync(&self) -> relay_app::SyncOutcome {
        self.engine.sync(self.workspace.clone()).await.unwrap()
    }

    async fn requests(&self) -> Vec<RequestDoc> {
        self.service
            .request_list(self.workspace.clone())
            .await
            .unwrap()
    }

    async fn conflicts(&self) -> Vec<relay_storage::StoredConflict> {
        let workspace = self.workspace.clone();
        self.db
            .call(move |c| SyncRepo::open_conflicts(c, &workspace))
            .await
            .unwrap()
    }

    async fn user_id_on(&self, server: &Server) -> String {
        let client = SyncClient::new(&server.base).unwrap();
        let token = self.engine.token(&self.workspace).unwrap();
        client.me(&token).await.unwrap().id
    }
}

/// Grants `user_id` editor rights on a workspace.
async fn grant_editor(server: &Server, owner_token: &str, workspace: &str, user_id: &str) {
    let http = reqwest::Client::new();
    let response = http
        .put(format!(
            "{}/v1/workspaces/{workspace}/members/{user_id}",
            server.base
        ))
        .bearer_auth(owner_token)
        .json(&serde_json::json!({ "role": "editor" }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 204);
}

#[tokio::test]
async fn a_local_change_reaches_a_second_device() {
    let server = start_server().await;

    let alice = Client::new("ws-shared", "Shared").await;
    alice.connect(&server, "alice@example.com").await;
    alice.create_remote(&server).await;

    let bob = Client::new("ws-shared", "Shared").await;
    bob.join(&server, "bob@example.com").await;

    let alice_token = alice.engine.token(&alice.workspace).unwrap();
    let bob_id = bob.user_id_on(&server).await;
    grant_editor(&server, &alice_token, "ws-shared", &bob_id).await;

    // Alice writes a request offline, then syncs.
    let mut doc = alice
        .service
        .request_create(alice.workspace.clone(), "List users".into(), None)
        .await
        .unwrap();
    doc.url = "https://api.example.com/users".into();
    alice.service.request_save(doc).await.unwrap();

    let pushed = alice.sync().await;
    assert_eq!(
        pushed.pushed, 2,
        "the workspace and the request should push"
    );
    assert_eq!(pushed.conflicts, 0);

    // Bob has nothing yet.
    assert!(bob.requests().await.is_empty());

    let pulled = bob.sync().await;
    assert!(pulled.pulled > 0, "bob pulled nothing: {pulled:?}");

    let bobs = bob.requests().await;
    assert_eq!(bobs.len(), 1);
    assert_eq!(bobs[0].name, "List users");
    assert_eq!(bobs[0].url, "https://api.example.com/users");
}

#[tokio::test]
async fn folders_arrive_before_the_requests_inside_them() {
    let server = start_server().await;

    let alice = Client::new("ws-tree", "Tree").await;
    alice.connect(&server, "alice2@example.com").await;
    alice.create_remote(&server).await;

    let bob = Client::new("ws-tree", "Tree").await;
    bob.join(&server, "bob2@example.com").await;
    let alice_token = alice.engine.token(&alice.workspace).unwrap();
    let bob_id = bob.user_id_on(&server).await;
    grant_editor(&server, &alice_token, "ws-tree", &bob_id).await;

    let collection = alice
        .service
        .collection_create(alice.workspace.clone(), "Users".into(), None)
        .await
        .unwrap();
    let mut doc = RequestDoc::new(alice.workspace.clone(), "Inside");
    doc.collection_id = Some(collection.id.clone());
    alice.service.request_save(doc).await.unwrap();

    alice.sync().await;
    bob.sync().await;

    // A request landing before its folder would violate the foreign key.
    let collections = bob
        .service
        .collection_list(bob.workspace.clone())
        .await
        .unwrap();
    assert_eq!(collections.len(), 1);

    let requests = bob.requests().await;
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].collection_id.as_ref(), Some(&collection.id));
}

#[tokio::test]
async fn syncing_twice_changes_nothing_the_second_time() {
    let server = start_server().await;
    let alice = Client::new("ws-idem", "Idem").await;
    alice.connect(&server, "alice3@example.com").await;
    alice.create_remote(&server).await;

    alice
        .service
        .request_create(alice.workspace.clone(), "Once".into(), None)
        .await
        .unwrap();

    let first = alice.sync().await;
    assert!(first.pushed > 0);

    let second = alice.sync().await;
    assert_eq!(second.pushed, 0, "a clean workspace pushed again");
    assert_eq!(second.conflicts, 0);
    assert!(second.is_idle(), "{second:?}");

    assert_eq!(alice.requests().await.len(), 1);
}

#[tokio::test]
async fn a_concurrent_edit_to_the_same_request_conflicts_and_loses_nothing() {
    let server = start_server().await;

    let alice = Client::new("ws-conflict", "Conflict").await;
    alice.connect(&server, "alice4@example.com").await;
    alice.create_remote(&server).await;

    let bob = Client::new("ws-conflict", "Conflict").await;
    bob.join(&server, "bob4@example.com").await;
    let alice_token = alice.engine.token(&alice.workspace).unwrap();
    let bob_id = bob.user_id_on(&server).await;
    grant_editor(&server, &alice_token, "ws-conflict", &bob_id).await;

    // Both start from the same state.
    let doc = alice
        .service
        .request_create(alice.workspace.clone(), "Shared request".into(), None)
        .await
        .unwrap();
    alice.sync().await;
    bob.sync().await;
    assert_eq!(bob.requests().await.len(), 1);

    // Both edit it offline.
    let mut alices = alice.service.request_get(doc.id.clone()).await.unwrap();
    alices.url = "https://alice.example.com".into();
    alice.service.request_save(alices).await.unwrap();

    let mut bobs = bob.service.request_get(doc.id.clone()).await.unwrap();
    bobs.url = "https://bob.example.com".into();
    bob.service.request_save(bobs).await.unwrap();

    // Alice gets there first.
    let alice_result = alice.sync().await;
    assert_eq!(alice_result.conflicts, 0);

    // Bob's edit was made against a revision that has moved on.
    let bob_result = bob.sync().await;
    assert_eq!(bob_result.conflicts, 1, "bob's stale edit was not caught");

    let conflicts = bob.conflicts().await;
    assert_eq!(conflicts.len(), 1);
    assert_eq!(conflicts[0].entity_id, doc.id.to_string());

    // Both versions are preserved for the user to choose between. Nothing
    // was silently overwritten in either direction.
    assert_eq!(conflicts[0].local["url"], "https://bob.example.com");
    assert_eq!(
        conflicts[0].server.as_ref().unwrap()["url"],
        "https://alice.example.com"
    );
}

#[tokio::test]
async fn keeping_the_local_version_pushes_it_on_the_next_sync() {
    let server = start_server().await;

    let alice = Client::new("ws-keeplocal", "KeepLocal").await;
    alice.connect(&server, "alice5@example.com").await;
    alice.create_remote(&server).await;

    let bob = Client::new("ws-keeplocal", "KeepLocal").await;
    bob.join(&server, "bob5@example.com").await;
    let alice_token = alice.engine.token(&alice.workspace).unwrap();
    let bob_id = bob.user_id_on(&server).await;
    grant_editor(&server, &alice_token, "ws-keeplocal", &bob_id).await;

    let doc = alice
        .service
        .request_create(alice.workspace.clone(), "Contested".into(), None)
        .await
        .unwrap();
    alice.sync().await;
    bob.sync().await;

    let mut alices = alice.service.request_get(doc.id.clone()).await.unwrap();
    alices.url = "https://alice.example.com".into();
    alice.service.request_save(alices).await.unwrap();
    alice.sync().await;

    let mut bobs = bob.service.request_get(doc.id.clone()).await.unwrap();
    bobs.url = "https://bob.example.com".into();
    bob.service.request_save(bobs).await.unwrap();
    bob.sync().await;

    let conflict = bob.conflicts().await.into_iter().next().unwrap();
    bob.engine
        .resolve(conflict.id.clone(), Resolution::KeepLocal)
        .await
        .unwrap();

    assert!(bob.conflicts().await.is_empty(), "the conflict stayed open");

    // Bob's version now wins, and reaches Alice.
    let after = bob.sync().await;
    assert_eq!(after.conflicts, 0, "resolving did not clear the conflict");
    assert!(after.pushed > 0);

    alice.sync().await;
    let alices_view = alice.service.request_get(doc.id).await.unwrap();
    assert_eq!(alices_view.url, "https://bob.example.com");
}

#[tokio::test]
async fn keeping_the_server_version_discards_the_local_edit() {
    let server = start_server().await;

    let alice = Client::new("ws-keepserver", "KeepServer").await;
    alice.connect(&server, "alice6@example.com").await;
    alice.create_remote(&server).await;

    let bob = Client::new("ws-keepserver", "KeepServer").await;
    bob.join(&server, "bob6@example.com").await;
    let alice_token = alice.engine.token(&alice.workspace).unwrap();
    let bob_id = bob.user_id_on(&server).await;
    grant_editor(&server, &alice_token, "ws-keepserver", &bob_id).await;

    let doc = alice
        .service
        .request_create(alice.workspace.clone(), "Contested".into(), None)
        .await
        .unwrap();
    alice.sync().await;
    bob.sync().await;

    let mut alices = alice.service.request_get(doc.id.clone()).await.unwrap();
    alices.url = "https://alice.example.com".into();
    alice.service.request_save(alices).await.unwrap();
    alice.sync().await;

    let mut bobs = bob.service.request_get(doc.id.clone()).await.unwrap();
    bobs.url = "https://bob.example.com".into();
    bob.service.request_save(bobs).await.unwrap();
    bob.sync().await;

    let conflict = bob.conflicts().await.into_iter().next().unwrap();
    bob.engine
        .resolve(conflict.id, Resolution::KeepServer)
        .await
        .unwrap();

    let bobs_view = bob.service.request_get(doc.id).await.unwrap();
    assert_eq!(bobs_view.url, "https://alice.example.com");

    // And nothing is left queued to push.
    let after = bob.sync().await;
    assert_eq!(after.pushed, 0, "a discarded edit was still pushed");
    assert_eq!(after.conflicts, 0);
}

#[tokio::test]
async fn a_deletion_propagates_as_a_tombstone() {
    let server = start_server().await;

    let alice = Client::new("ws-delete", "Delete").await;
    alice.connect(&server, "alice7@example.com").await;
    alice.create_remote(&server).await;

    let bob = Client::new("ws-delete", "Delete").await;
    bob.join(&server, "bob7@example.com").await;
    let alice_token = alice.engine.token(&alice.workspace).unwrap();
    let bob_id = bob.user_id_on(&server).await;
    grant_editor(&server, &alice_token, "ws-delete", &bob_id).await;

    let doc = alice
        .service
        .request_create(alice.workspace.clone(), "Doomed".into(), None)
        .await
        .unwrap();
    alice.sync().await;
    bob.sync().await;
    assert_eq!(bob.requests().await.len(), 1);

    alice.service.request_delete(doc.id.clone()).await.unwrap();
    alice.sync().await;

    bob.sync().await;
    assert!(
        bob.requests().await.is_empty(),
        "the deletion did not reach the second device"
    );
}

#[tokio::test]
async fn work_done_entirely_offline_uploads_when_the_server_returns() {
    let server = start_server().await;

    let alice = Client::new("ws-offline", "Offline").await;
    alice.connect(&server, "alice8@example.com").await;
    alice.create_remote(&server).await;

    // Several edits with no sync in between, as if disconnected.
    for i in 0..5 {
        alice
            .service
            .request_create(alice.workspace.clone(), format!("Offline {i}"), None)
            .await
            .unwrap();
    }
    let collection = Collection::new(alice.workspace.clone(), "Made offline");
    let saved = collection.clone();
    alice
        .db
        .call(move |c| relay_storage::CollectionRepo::save(c, &saved))
        .await
        .unwrap();

    let outcome = alice.sync().await;
    // Five requests, one collection, the workspace itself.
    assert_eq!(outcome.pushed, 7, "{outcome:?}");
    assert_eq!(outcome.conflicts, 0);

    let bob = Client::new("ws-offline", "Offline").await;
    bob.join(&server, "bob8@example.com").await;
    let alice_token = alice.engine.token(&alice.workspace).unwrap();
    let bob_id = bob.user_id_on(&server).await;
    grant_editor(&server, &alice_token, "ws-offline", &bob_id).await;

    bob.sync().await;
    assert_eq!(bob.requests().await.len(), 5);
}

#[tokio::test]
async fn secret_values_are_never_uploaded() {
    let server = start_server().await;

    let alice = Client::new("ws-secrets", "Secrets").await;
    alice.connect(&server, "alice9@example.com").await;
    alice.create_remote(&server).await;

    let mut environment = alice
        .service
        .environment_create(alice.workspace.clone(), "Production".into())
        .await
        .unwrap();
    environment.variables = vec![
        relay_models::Variable::new("baseUrl", "https://api.example.com"),
        relay_models::Variable::new("apiToken", "").into_secret(),
    ];
    let environment = alice.service.environment_save(environment).await.unwrap();
    alice
        .service
        .secret_set(&environment.id, "apiToken", "sk-live-MUST-NOT-UPLOAD")
        .unwrap();

    alice.sync().await;

    // Read it back from the server as a second device would.
    let bob = Client::new("ws-secrets", "Secrets").await;
    bob.join(&server, "bob9@example.com").await;
    let alice_token = alice.engine.token(&alice.workspace).unwrap();
    let bob_id = bob.user_id_on(&server).await;
    grant_editor(&server, &alice_token, "ws-secrets", &bob_id).await;

    let client = SyncClient::new(&server.base).unwrap();
    let bob_token = bob.engine.token(&bob.workspace).unwrap();
    let page = client.pull(&bob_token, "ws-secrets", 0, 500).await.unwrap();

    let raw = serde_json::to_string(&page).unwrap();
    assert!(
        !raw.contains("sk-live-MUST-NOT-UPLOAD"),
        "a secret was uploaded: {raw}"
    );
    // The variable's name still travels, so a teammate knows to supply it.
    assert!(raw.contains("apiToken"));
    assert!(raw.contains("https://api.example.com"));
}

#[tokio::test]
async fn syncing_without_a_token_fails_clearly_rather_than_silently() {
    let server = start_server().await;
    let alice = Client::new("ws-notoken", "NoToken").await;
    alice.connect(&server, "alice10@example.com").await;
    alice.create_remote(&server).await;
    alice.engine.clear_token(&alice.workspace).unwrap();

    let error = alice
        .engine
        .sync(alice.workspace.clone())
        .await
        .unwrap_err();

    assert_eq!(error.kind, relay_models::ErrorKind::Forbidden);
    assert!(error.message.contains("sign in"), "got {}", error.message);
}

#[tokio::test]
async fn an_unconnected_workspace_reports_that_rather_than_failing_obscurely() {
    let alice = Client::new("ws-local-only", "Local only").await;

    let error = alice
        .engine
        .sync(alice.workspace.clone())
        .await
        .unwrap_err();

    assert!(
        error.message.contains("not connected"),
        "got {}",
        error.message
    );
}

#[tokio::test]
async fn an_unreachable_server_records_the_failure_and_keeps_local_data() {
    let alice = Client::new("ws-unreachable", "Unreachable").await;

    // Point at a closed port.
    let state = relay_storage::SyncState {
        workspace_id: alice.workspace.clone(),
        server_url: "http://127.0.0.1:1".into(),
        remote_workspace_id: None,
        last_pulled_seq: 0,
        last_sync_at: None,
        last_error: None,
        user_id: None,
        user_email: None,
    };
    alice
        .db
        .call(move |c| SyncRepo::set_state(c, &state))
        .await
        .unwrap();
    alice
        .engine
        .set_token(&alice.workspace, "irrelevant")
        .unwrap();

    alice
        .service
        .request_create(alice.workspace.clone(), "Still mine".into(), None)
        .await
        .unwrap();

    assert!(alice.engine.sync(alice.workspace.clone()).await.is_err());

    // Local work is untouched, and the reason is recorded for the UI.
    assert_eq!(alice.requests().await.len(), 1);

    let workspace = alice.workspace.clone();
    let state = alice
        .db
        .call(move |c| SyncRepo::state(c, &workspace))
        .await
        .unwrap()
        .unwrap();
    assert!(state.last_error.is_some(), "the failure was not recorded");
}

#[tokio::test]
async fn a_change_by_one_device_wakes_the_other_over_the_websocket() {
    use relay_app::sync::WatchEvent;

    let server = start_server().await;

    let alice = Client::new("ws-live", "Live").await;
    alice.connect(&server, "alice11@example.com").await;
    alice.create_remote(&server).await;

    let bob = Client::new("ws-live", "Live").await;
    bob.join(&server, "bob11@example.com").await;
    let alice_token = alice.engine.token(&alice.workspace).unwrap();
    let bob_id = bob.user_id_on(&server).await;
    grant_editor(&server, &alice_token, "ws-live", &bob_id).await;

    // Bob starts listening.
    let (tx, mut rx) = tokio::sync::mpsc::channel(32);
    let watcher = bob
        .service
        .sync_watch(bob.workspace.clone(), tx)
        .await
        .unwrap()
        .expect("bob should get a watcher");

    // The connection opens, and the current head arrives.
    let first = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
        .await
        .expect("no connection event")
        .unwrap();
    assert_eq!(first.1, WatchEvent::Connected);

    // Alice writes something.
    alice
        .service
        .request_create(alice.workspace.clone(), "Live change".into(), None)
        .await
        .unwrap();
    alice.sync().await;

    // Bob is told, without polling. The server also sends the current
    // head on connect — so this waits for one that is genuinely higher,
    // not merely the opening greeting.
    let mut highest = 0i64;
    let mut advanced = false;

    for _ in 0..6 {
        let Ok(Some((workspace, event))) =
            tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv()).await
        else {
            break;
        };
        assert_eq!(workspace, bob.workspace);

        if let WatchEvent::Changed { seq } = event {
            if seq > highest && highest > 0 {
                advanced = true;
                break;
            }
            highest = highest.max(seq);
            // The greeting arrives before Alice has pushed, so head 0 is
            // expected; keep waiting for the real change.
            if highest > 0 {
                advanced = true;
                break;
            }
        }
    }
    assert!(advanced, "bob was never told the workspace changed");

    // And acting on it brings the change across.
    bob.sync().await;
    assert_eq!(bob.requests().await.len(), 1);

    drop(watcher);
}

#[tokio::test]
async fn a_local_only_workspace_gets_no_watcher_and_that_is_not_an_error() {
    let alice = Client::new("ws-nowatch", "No watch").await;

    let (tx, _rx) = tokio::sync::mpsc::channel(4);
    let watcher = alice
        .service
        .sync_watch(alice.workspace.clone(), tx)
        .await
        .unwrap();

    // Running without a server is the ordinary case, not a failure.
    assert!(watcher.is_none());
}
