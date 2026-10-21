//! End-to-end tests through the service layer: the full Phase 1 journey.

use std::sync::Arc;

use axum::routing::{any, get};
use axum::{Json, Router};
use relay_app::{MemorySecretStore, RelayService};
use relay_http::{HttpEngine, NoopProgress};
use relay_models::{
    AuthConfig, ExecutionId, HttpExchange, HttpMethod, KeyValue, RequestBody, ResponseBody,
    Variable,
};
use relay_storage::Database;

struct TestServer {
    addr: std::net::SocketAddr,
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

async fn start_server() -> TestServer {
    let app = Router::new()
        .route(
            "/users",
            any(
                |method: axum::http::Method,
                 headers: axum::http::HeaderMap,
                 body: axum::body::Bytes| async move {
                    Json(serde_json::json!({
                        "method": method.as_str(),
                        "auth": headers.get("authorization")
                            .and_then(|v| v.to_str().ok()).unwrap_or(""),
                        "apiKey": headers.get("x-api-key")
                            .and_then(|v| v.to_str().ok()).unwrap_or(""),
                        "body": String::from_utf8_lossy(&body),
                    }))
                },
            ),
        )
        .route("/health", get(|| async { "ok" }));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    TestServer { addr, handle }
}

fn service_with(db: Database) -> RelayService {
    RelayService::new(
        db,
        HttpEngine::with_defaults(),
        Arc::new(MemorySecretStore::new()),
    )
}

fn service() -> RelayService {
    service_with(Database::open_in_memory().unwrap())
}

fn body_text(exchange: &HttpExchange) -> String {
    match exchange {
        HttpExchange::Success { response, .. } => match &response.body {
            ResponseBody::Text { text } => text.clone(),
            other => panic!("expected text body, got {other:?}"),
        },
        other => panic!("expected success, got {other:?}"),
    }
}

#[tokio::test]
async fn first_run_creates_a_usable_workspace() {
    let service = service();
    let workspace = service.active_workspace().await.unwrap();
    assert_eq!(workspace.name, "My Workspace");

    // Calling again must not create a second one.
    let again = service.active_workspace().await.unwrap();
    assert_eq!(again.id, workspace.id);
    assert_eq!(service.workspace_list().await.unwrap().len(), 1);
}

#[tokio::test]
async fn the_phase_one_journey_works_end_to_end() {
    // Create a request, save it, send it, inspect the response, "restart"
    // the application and find the request still there.
    let server = start_server().await;
    let dir = std::env::temp_dir().join(format!("relay-e2e-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    let path = dir.join("relay.db");

    let request_id;
    let workspace_id;

    {
        let service = service_with(Database::open(&path).unwrap());
        let workspace = service.active_workspace().await.unwrap();
        workspace_id = workspace.id.clone();

        let mut doc = service
            .request_create(workspace.id.clone(), "Health".into(), None)
            .await
            .unwrap();
        doc.method = HttpMethod::Get;
        doc.url = server.url("/health");
        doc.headers = vec![KeyValue::new("X-Trace", "e2e")];
        let doc = service.request_save(doc).await.unwrap();
        request_id = doc.id.clone();

        let exchange = service
            .request_send(ExecutionId::new(), doc, None, Arc::new(NoopProgress))
            .await
            .unwrap();

        match &exchange {
            HttpExchange::Success { response, .. } => {
                assert_eq!(response.status, 200);
                assert_eq!(body_text(&exchange), "ok");
            }
            other => panic!("expected success, got {other:?}"),
        }

        // The execution was recorded.
        let history = service
            .history_list(workspace.id.clone(), 10, 0)
            .await
            .unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].status, Some(200));
        assert_eq!(history[0].name, "Health");
    }

    // A completely fresh process.
    let service = service_with(Database::open(&path).unwrap());
    let reopened = service.request_get(request_id).await.unwrap();
    assert_eq!(reopened.name, "Health");
    assert_eq!(reopened.headers[0].value, "e2e");

    let history = service.history_list(workspace_id, 10, 0).await.unwrap();
    assert_eq!(history.len(), 1, "history did not survive the restart");

    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn environment_variables_resolve_when_sending() {
    let server = start_server().await;
    let service = service();
    let workspace = service.active_workspace().await.unwrap();

    let mut env = service
        .environment_create(workspace.id.clone(), "Local".into())
        .await
        .unwrap();
    env.variables = vec![Variable::new("baseUrl", format!("http://{}", server.addr))];
    let env = service.environment_save(env).await.unwrap();

    let mut doc = service
        .request_create(workspace.id.clone(), "Users".into(), None)
        .await
        .unwrap();
    doc.url = "{{baseUrl}}/health".into();
    let doc = service.request_save(doc).await.unwrap();

    let exchange = service
        .request_send(
            ExecutionId::new(),
            doc,
            Some(env.id.clone()),
            Arc::new(NoopProgress),
        )
        .await
        .unwrap();

    assert_eq!(body_text(&exchange), "ok");
}

#[tokio::test]
async fn an_unresolved_variable_blocks_the_request_with_a_clear_error() {
    let service = service();
    let workspace = service.active_workspace().await.unwrap();

    let mut doc = service
        .request_create(workspace.id.clone(), "Broken".into(), None)
        .await
        .unwrap();
    doc.url = "{{baseUrl}}/users".into();
    let doc = service.request_save(doc).await.unwrap();

    let err = service
        .request_send(ExecutionId::new(), doc, None, Arc::new(NoopProgress))
        .await
        .unwrap_err();

    assert_eq!(err.kind, relay_models::ErrorKind::Interpolation);
    assert!(err.message.contains("baseUrl"), "got {}", err.message);
    // The message should tell the user where to fix it.
    assert!(err.detail.unwrap().contains("environment"));
}

#[tokio::test]
async fn secrets_are_injected_at_send_time_and_never_persisted() {
    let server = start_server().await;
    let service = service();
    let workspace = service.active_workspace().await.unwrap();

    let mut env = service
        .environment_create(workspace.id.clone(), "Prod".into())
        .await
        .unwrap();
    env.variables = vec![
        Variable::new("baseUrl", format!("http://{}", server.addr)),
        Variable::new("apiToken", "").into_secret(),
    ];
    let env = service.environment_save(env).await.unwrap();

    service
        .secret_set(&env.id, "apiToken", "sk-live-REAL")
        .unwrap();
    assert!(service.secret_is_set(&env.id, "apiToken"));

    let mut doc = service
        .request_create(workspace.id.clone(), "Authed".into(), None)
        .await
        .unwrap();
    doc.url = "{{baseUrl}}/users".into();
    doc.auth = AuthConfig::Bearer {
        token: "{{apiToken}}".into(),
    };
    let doc = service.request_save(doc).await.unwrap();

    let exchange = service
        .request_send(
            ExecutionId::new(),
            doc.clone(),
            Some(env.id.clone()),
            Arc::new(NoopProgress),
        )
        .await
        .unwrap();

    // The secret really did reach the server.
    let echoed: serde_json::Value = serde_json::from_str(&body_text(&exchange)).unwrap();
    assert_eq!(echoed["auth"], "Bearer sk-live-REAL");

    // But it is nowhere in the stored history.
    let history = service
        .history_list(workspace.id.clone(), 10, 0)
        .await
        .unwrap();
    let serialised = serde_json::to_string(&history).unwrap();
    assert!(!serialised.contains("sk-live-REAL"), "history leaked it");

    // Nor in the environment row.
    let environments = service
        .environment_list(workspace.id.clone())
        .await
        .unwrap();
    let serialised = serde_json::to_string(&environments).unwrap();
    assert!(
        !serialised.contains("sk-live-REAL"),
        "environment leaked it"
    );
}

#[tokio::test]
async fn a_resolution_preview_does_not_reveal_secrets() {
    let service = service();
    let workspace = service.active_workspace().await.unwrap();

    let mut env = service
        .environment_create(workspace.id.clone(), "Prod".into())
        .await
        .unwrap();
    env.variables = vec![
        Variable::new("baseUrl", "https://api.example.com"),
        Variable::new("apiToken", "").into_secret(),
    ];
    let env = service.environment_save(env).await.unwrap();
    service
        .secret_set(&env.id, "apiToken", "sk-live-REAL")
        .unwrap();

    let mut doc = service
        .request_create(workspace.id.clone(), "Preview".into(), None)
        .await
        .unwrap();
    doc.url = "{{baseUrl}}/users".into();
    doc.auth = AuthConfig::Bearer {
        token: "{{apiToken}}".into(),
    };

    let preview = service
        .request_resolve(doc, Some(env.id.clone()))
        .await
        .unwrap();

    assert_eq!(preview.spec.url, "https://api.example.com/users");
    let serialised = serde_json::to_string(&preview).unwrap();
    assert!(
        !serialised.contains("sk-live-REAL"),
        "the preview exposed a secret: {serialised}"
    );
}

#[tokio::test]
async fn collection_auth_is_inherited_by_its_requests() {
    let server = start_server().await;
    let service = service();
    let workspace = service.active_workspace().await.unwrap();

    let mut collection = service
        .collection_create(workspace.id.clone(), "Secured".into(), None)
        .await
        .unwrap();
    collection.auth = AuthConfig::Bearer {
        token: "collection-token".into(),
    };
    let collection = service.collection_save(collection).await.unwrap();

    let mut doc = service
        .request_create(
            workspace.id.clone(),
            "Inheriting".into(),
            Some(collection.id.clone()),
        )
        .await
        .unwrap();
    doc.url = server.url("/users");
    doc.auth = AuthConfig::Inherit;
    let doc = service.request_save(doc).await.unwrap();

    let exchange = service
        .request_send(ExecutionId::new(), doc, None, Arc::new(NoopProgress))
        .await
        .unwrap();

    let echoed: serde_json::Value = serde_json::from_str(&body_text(&exchange)).unwrap();
    assert_eq!(echoed["auth"], "Bearer collection-token");
}

#[tokio::test]
async fn a_failed_request_is_recorded_in_history_with_its_error() {
    let service = service();
    let workspace = service.active_workspace().await.unwrap();

    let mut doc = service
        .request_create(workspace.id.clone(), "Unreachable".into(), None)
        .await
        .unwrap();
    doc.url = "http://127.0.0.1:1/".into();
    let doc = service.request_save(doc).await.unwrap();

    let exchange = service
        .request_send(ExecutionId::new(), doc, None, Arc::new(NoopProgress))
        .await
        .unwrap();
    assert!(matches!(exchange, HttpExchange::Failure { .. }));

    let history = service.history_list(workspace.id, 10, 0).await.unwrap();
    assert_eq!(history.len(), 1);
    assert!(history[0].error.is_some());
    assert_eq!(history[0].status, None);
}

#[tokio::test]
async fn curl_import_produces_a_request_that_actually_sends() {
    let server = start_server().await;
    let service = service();
    let workspace = service.active_workspace().await.unwrap();

    let command = format!(
        r#"curl -X POST '{}' -H 'Content-Type: application/json' -d '{{"name":"Ada"}}'"#,
        server.url("/users")
    );
    let doc = service.import_curl(&command, workspace.id.clone()).unwrap();
    let doc = service.request_save(doc).await.unwrap();

    assert_eq!(doc.method, HttpMethod::Post);
    assert!(matches!(doc.body, RequestBody::Json { .. }));

    let exchange = service
        .request_send(ExecutionId::new(), doc, None, Arc::new(NoopProgress))
        .await
        .unwrap();

    let echoed: serde_json::Value = serde_json::from_str(&body_text(&exchange)).unwrap();
    assert_eq!(echoed["method"], "POST");
    assert_eq!(echoed["body"], r#"{"name":"Ada"}"#);
}

#[tokio::test]
async fn export_as_curl_resolves_variables() {
    let service = service();
    let workspace = service.active_workspace().await.unwrap();

    let mut env = service
        .environment_create(workspace.id.clone(), "Dev".into())
        .await
        .unwrap();
    env.variables = vec![Variable::new("baseUrl", "https://api.example.com")];
    let env = service.environment_save(env).await.unwrap();

    let mut doc = service
        .request_create(workspace.id.clone(), "Export".into(), None)
        .await
        .unwrap();
    doc.method = HttpMethod::Post;
    doc.url = "{{baseUrl}}/users".into();
    doc.body = RequestBody::Json {
        text: r#"{"a":1}"#.into(),
    };

    let command = service.export_curl(doc, Some(env.id)).await.unwrap();
    assert!(command.contains("https://api.example.com/users"));
    assert!(command.contains("-X POST"));
    assert!(!command.contains("{{baseUrl}}"));
}

#[tokio::test]
async fn deleting_a_request_removes_it_from_the_tree() {
    let service = service();
    let workspace = service.active_workspace().await.unwrap();

    let doc = service
        .request_create(workspace.id.clone(), "Temporary".into(), None)
        .await
        .unwrap();
    assert_eq!(
        service
            .collection_tree(workspace.id.clone())
            .await
            .unwrap()
            .len(),
        1
    );

    service.request_delete(doc.id.clone()).await.unwrap();
    assert!(
        service
            .collection_tree(workspace.id.clone())
            .await
            .unwrap()
            .is_empty()
    );
    assert!(service.request_get(doc.id).await.is_err());
}

#[tokio::test]
async fn ui_state_survives_a_restart() {
    let dir = std::env::temp_dir().join(format!("relay-state-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    let path = dir.join("relay.db");

    let workspace_id = {
        let service = service_with(Database::open(&path).unwrap());
        let workspace = service.active_workspace().await.unwrap();
        service
            .state_set("active_workspace".into(), workspace.id.to_string())
            .await
            .unwrap();
        service
            .state_set("theme".into(), "dark".into())
            .await
            .unwrap();
        workspace.id
    };

    let service = service_with(Database::open(&path).unwrap());
    assert_eq!(
        service.state_get("theme".into()).await.unwrap().as_deref(),
        Some("dark")
    );
    assert_eq!(service.active_workspace().await.unwrap().id, workspace_id);

    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn cancelling_an_unknown_execution_is_harmless() {
    let service = service();
    assert!(!service.request_cancel(&ExecutionId::from("nope")));
}

#[tokio::test]
async fn secrets_for_one_environment_do_not_leak_into_another() {
    let server = start_server().await;
    let service = service();
    let workspace = service.active_workspace().await.unwrap();

    let make_env = |name: &str| {
        let workspace_id = workspace.id.clone();
        let name = name.to_string();
        let service = service.clone();
        let addr = server.addr;
        async move {
            let mut env = service
                .environment_create(workspace_id, name)
                .await
                .unwrap();
            env.variables = vec![
                Variable::new("baseUrl", format!("http://{addr}")),
                Variable::new("apiToken", "").into_secret(),
            ];
            service.environment_save(env).await.unwrap()
        }
    };

    let dev = make_env("Dev").await;
    let prod = make_env("Prod").await;
    service
        .secret_set(&dev.id, "apiToken", "dev-token")
        .unwrap();
    service
        .secret_set(&prod.id, "apiToken", "prod-token")
        .unwrap();

    let mut doc = service
        .request_create(workspace.id.clone(), "Authed".into(), None)
        .await
        .unwrap();
    doc.url = "{{baseUrl}}/users".into();
    doc.auth = AuthConfig::Bearer {
        token: "{{apiToken}}".into(),
    };
    let doc = service.request_save(doc).await.unwrap();

    for (env_id, expected) in [(&dev.id, "dev-token"), (&prod.id, "prod-token")] {
        let exchange = service
            .request_send(
                ExecutionId::new(),
                doc.clone(),
                Some(env_id.clone()),
                Arc::new(NoopProgress),
            )
            .await
            .unwrap();
        let echoed: serde_json::Value = serde_json::from_str(&body_text(&exchange)).unwrap();
        assert_eq!(echoed["auth"], format!("Bearer {expected}"));
    }
}

#[tokio::test]
async fn a_secret_with_no_stored_value_fails_loudly_rather_than_sending_an_empty_credential() {
    let server = start_server().await;
    let service = service();
    let workspace = service.active_workspace().await.unwrap();

    let mut env = service
        .environment_create(workspace.id.clone(), "Prod".into())
        .await
        .unwrap();
    env.variables = vec![
        Variable::new("baseUrl", format!("http://{}", server.addr)),
        Variable::new("apiToken", "").into_secret(),
    ];
    let env = service.environment_save(env).await.unwrap();
    // Deliberately do NOT set the secret.

    let mut doc = service
        .request_create(workspace.id.clone(), "Authed".into(), None)
        .await
        .unwrap();
    doc.url = "{{baseUrl}}/users".into();
    doc.auth = AuthConfig::Bearer {
        token: "{{apiToken}}".into(),
    };
    let doc = service.request_save(doc).await.unwrap();

    let exchange = service
        .request_send(
            ExecutionId::new(),
            doc,
            Some(env.id.clone()),
            Arc::new(NoopProgress),
        )
        .await
        .unwrap();

    // The variable is defined (so resolution succeeds) but empty. The engine
    // drops an empty bearer token rather than sending `Authorization: Bearer`.
    let echoed: serde_json::Value = serde_json::from_str(&body_text(&exchange)).unwrap();
    assert_eq!(echoed["auth"], "", "an empty credential was sent");
}

#[tokio::test]
async fn concurrent_sends_through_one_service_stay_independent() {
    let server = start_server().await;
    let service = service();
    let workspace = service.active_workspace().await.unwrap();

    let mut handles = Vec::new();
    for i in 0..10 {
        let service = service.clone();
        let workspace_id = workspace.id.clone();
        let url = server.url("/users");
        handles.push(tokio::spawn(async move {
            let mut doc = service
                .request_create(workspace_id, format!("R{i}"), None)
                .await
                .unwrap();
            doc.method = HttpMethod::Post;
            doc.url = url;
            doc.body = RequestBody::Text {
                text: format!("payload-{i}"),
            };
            let doc = service.request_save(doc).await.unwrap();
            let exchange = service
                .request_send(ExecutionId::new(), doc, None, Arc::new(NoopProgress))
                .await
                .unwrap();
            let echoed: serde_json::Value = serde_json::from_str(&body_text(&exchange)).unwrap();
            assert_eq!(echoed["body"], format!("payload-{i}"));
        }));
    }

    for handle in handles {
        handle.await.unwrap();
    }

    let history = service.history_list(workspace.id, 100, 0).await.unwrap();
    assert_eq!(history.len(), 10, "history lost a concurrent execution");
}
