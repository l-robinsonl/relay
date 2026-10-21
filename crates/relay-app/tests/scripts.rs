//! Scripts running against real responses, through the service.

use std::sync::Arc;

use axum::{Json, Router, routing::get};
use relay_app::{MemorySecretStore, RelayService};
use relay_http::{HttpEngine, NoopProgress};
use relay_models::{ExecutionId, Variable};
use relay_storage::Database;

struct Server {
    base: String,
    handle: tokio::task::JoinHandle<()>,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

async fn start() -> Server {
    let app = Router::new()
        .route(
            "/user",
            get(|| async { Json(serde_json::json!({ "id": 42, "name": "Ada", "active": true })) }),
        )
        .route(
            "/boom",
            get(|| async { (axum::http::StatusCode::INTERNAL_SERVER_ERROR, "no") }),
        );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    Server {
        base: format!("http://{addr}"),
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

#[tokio::test]
async fn assertions_run_against_a_real_response() {
    let server = start().await;
    let service = service();
    let workspace = service.active_workspace().await.unwrap();

    let mut doc = service
        .request_create(workspace.id.clone(), "Get user".into(), None)
        .await
        .unwrap();
    doc.url = server.base.clone() + "/user";
    doc.scripts.post_response = r#"
        test('it succeeds', () => {
          expect(response.status).toBe(200)
          expect(response.ok).toBeTruthy()
        })
        test('it returns the user', () => {
          expect(response.json().id).toBe(42)
          expect(response.json().name).toBe('Ada')
        })
    "#
    .into();
    let doc = service.request_save(doc).await.unwrap();

    let outcome = service
        .request_send_with_tests(ExecutionId::new(), doc, None, Arc::new(NoopProgress))
        .await
        .unwrap();

    let tests = outcome.tests.expect("the script did not run");
    assert!(tests.passed(), "{tests:#?}");
    assert_eq!(tests.tests.len(), 2);
    assert_eq!(tests.passed_count(), 4);
}

#[tokio::test]
async fn a_failing_assertion_does_not_fail_the_request() {
    // The response is what the user asked for; the verdict on it is
    // separate information, not a reason to hide it.
    let server = start().await;
    let service = service();
    let workspace = service.active_workspace().await.unwrap();

    let mut doc = service
        .request_create(workspace.id.clone(), "Wrong".into(), None)
        .await
        .unwrap();
    doc.url = server.base.clone() + "/user";
    doc.scripts.post_response = "expect(response.status).toBe(404)".into();
    let doc = service.request_save(doc).await.unwrap();

    let outcome = service
        .request_send_with_tests(ExecutionId::new(), doc, None, Arc::new(NoopProgress))
        .await
        .unwrap();

    match &outcome.exchange {
        relay_models::HttpExchange::Success { response, .. } => {
            assert_eq!(response.status, 200, "the response was lost");
        }
        other => panic!("expected a response, got {other:?}"),
    }
    assert!(!outcome.tests.unwrap().passed());
}

#[tokio::test]
async fn a_script_can_read_the_workspaces_variables() {
    let server = start().await;
    let service = service();
    let mut workspace = service.active_workspace().await.unwrap();
    workspace.variables = vec![Variable::new("expectedName", "Ada")];
    let workspace = service.workspace_save(workspace).await.unwrap();

    let mut doc = service
        .request_create(workspace.id.clone(), "Check".into(), None)
        .await
        .unwrap();
    doc.url = server.base.clone() + "/user";
    doc.scripts.post_response =
        "expect(response.json().name).toBe(relay.getVariable('expectedName'))".into();
    let doc = service.request_save(doc).await.unwrap();

    let outcome = service
        .request_send_with_tests(ExecutionId::new(), doc, None, Arc::new(NoopProgress))
        .await
        .unwrap();

    assert!(outcome.tests.unwrap().passed());
}

#[tokio::test]
async fn a_script_cannot_read_a_secret_variable() {
    let server = start().await;
    let service = service();
    let workspace = service.active_workspace().await.unwrap();

    let mut environment = service
        .environment_create(workspace.id.clone(), "Prod".into())
        .await
        .unwrap();
    environment.variables = vec![
        Variable::new("baseUrl", &server.base),
        Variable::new("apiToken", "").into_secret(),
    ];
    let environment = service.environment_save(environment).await.unwrap();
    service
        .secret_set(
            &environment.id,
            "apiToken",
            "sk-live-MUST-NOT-REACH-SCRIPTS",
        )
        .unwrap();

    let mut doc = service
        .request_create(workspace.id.clone(), "Peek".into(), None)
        .await
        .unwrap();
    doc.url = "{{baseUrl}}/user".into();
    doc.scripts.post_response =
        "console.log('token=' + String(relay.getVariable('apiToken')))".into();
    let doc = service.request_save(doc).await.unwrap();

    let outcome = service
        .request_send_with_tests(
            ExecutionId::new(),
            doc,
            Some(environment.id),
            Arc::new(NoopProgress),
        )
        .await
        .unwrap();

    let printed: String = outcome
        .tests
        .unwrap()
        .console
        .iter()
        .map(|l| l.text.clone())
        .collect();

    assert!(
        !printed.contains("sk-live-MUST-NOT-REACH-SCRIPTS"),
        "a script read a secret: {printed}"
    );
    assert_eq!(printed, "token=undefined");
}

#[tokio::test]
async fn a_script_can_set_a_variable_for_later_requests() {
    // The reason scripts exist for most people: capture a token from a
    // login response and use it in the next request.
    let server = start().await;
    let service = service();
    let workspace = service.active_workspace().await.unwrap();

    let mut doc = service
        .request_create(workspace.id.clone(), "Login".into(), None)
        .await
        .unwrap();
    doc.url = server.base.clone() + "/user";
    doc.scripts.post_response = "relay.setVariable('userId', String(response.json().id))".into();
    let doc = service.request_save(doc).await.unwrap();

    service
        .request_send_with_tests(ExecutionId::new(), doc, None, Arc::new(NoopProgress))
        .await
        .unwrap();

    let saved = service
        .workspace_list()
        .await
        .unwrap()
        .into_iter()
        .find(|w| w.id == workspace.id)
        .unwrap();

    let captured = saved.variables.iter().find(|v| v.key == "userId").unwrap();
    assert_eq!(captured.value, "42");
    assert!(!captured.secret, "a script created a secret variable");
}

#[tokio::test]
async fn a_script_cannot_overwrite_a_secret_variable() {
    let service = service();
    let mut workspace = service.active_workspace().await.unwrap();
    workspace.variables = vec![Variable::new("apiToken", "").into_secret()];
    let workspace = service.workspace_save(workspace).await.unwrap();

    let server = start().await;
    let mut doc = service
        .request_create(workspace.id.clone(), "Overwrite".into(), None)
        .await
        .unwrap();
    doc.url = server.base.clone() + "/user";
    doc.scripts.post_response = "relay.setVariable('apiToken', 'replaced-by-a-script')".into();
    let doc = service.request_save(doc).await.unwrap();

    service
        .request_send_with_tests(ExecutionId::new(), doc, None, Arc::new(NoopProgress))
        .await
        .unwrap();

    let saved = service
        .workspace_list()
        .await
        .unwrap()
        .into_iter()
        .find(|w| w.id == workspace.id)
        .unwrap();
    let token = saved
        .variables
        .iter()
        .find(|v| v.key == "apiToken")
        .unwrap();

    // Letting a script replace a credential would be a way around the
    // credential store, not a feature.
    assert!(token.secret);
    assert_ne!(token.value, "replaced-by-a-script");
}

#[tokio::test]
async fn a_request_with_no_script_reports_no_tests() {
    let server = start().await;
    let service = service();
    let workspace = service.active_workspace().await.unwrap();

    let mut doc = service
        .request_create(workspace.id.clone(), "Plain".into(), None)
        .await
        .unwrap();
    doc.url = server.base.clone() + "/user";
    let doc = service.request_save(doc).await.unwrap();

    let outcome = service
        .request_send_with_tests(ExecutionId::new(), doc, None, Arc::new(NoopProgress))
        .await
        .unwrap();

    assert!(outcome.tests.is_none());
}

#[tokio::test]
async fn a_script_is_not_run_when_the_request_never_got_a_response() {
    // Asserting about `response` when there isn't one produces confusing
    // errors about the wrong thing.
    let service = service();
    let workspace = service.active_workspace().await.unwrap();

    let mut doc = service
        .request_create(workspace.id.clone(), "Unreachable".into(), None)
        .await
        .unwrap();
    doc.url = "http://127.0.0.1:1/".into();
    doc.scripts.post_response = "expect(response.status).toBe(200)".into();
    let doc = service.request_save(doc).await.unwrap();

    let outcome = service
        .request_send_with_tests(ExecutionId::new(), doc, None, Arc::new(NoopProgress))
        .await
        .unwrap();

    assert!(matches!(
        outcome.exchange,
        relay_models::HttpExchange::Failure { .. }
    ));
    assert!(outcome.tests.is_none());
}

#[tokio::test]
async fn a_script_that_loops_forever_does_not_hang_the_request() {
    let server = start().await;
    let service = service();
    let workspace = service.active_workspace().await.unwrap();

    let mut doc = service
        .request_create(workspace.id.clone(), "Runaway".into(), None)
        .await
        .unwrap();
    doc.url = server.base.clone() + "/user";
    doc.scripts.post_response = "while (true) {}".into();
    let doc = service.request_save(doc).await.unwrap();

    let started = std::time::Instant::now();
    let outcome = service
        .request_send_with_tests(ExecutionId::new(), doc, None, Arc::new(NoopProgress))
        .await
        .unwrap();

    assert!(
        started.elapsed() < std::time::Duration::from_secs(20),
        "it took {:?}",
        started.elapsed()
    );
    assert!(outcome.tests.unwrap().error.is_some());
    // And the response survived.
    assert!(matches!(
        outcome.exchange,
        relay_models::HttpExchange::Success { .. }
    ));
}

#[tokio::test]
async fn an_error_status_can_be_asserted_about() {
    let server = start().await;
    let service = service();
    let workspace = service.active_workspace().await.unwrap();

    let mut doc = service
        .request_create(workspace.id.clone(), "Expect failure".into(), None)
        .await
        .unwrap();
    doc.url = server.base.clone() + "/boom";
    doc.scripts.post_response =
        "test('it reports the failure', () => { expect(response.status).toBe(500); \
         expect(response.ok).toBeFalsy() })"
            .into();
    let doc = service.request_save(doc).await.unwrap();

    let outcome = service
        .request_send_with_tests(ExecutionId::new(), doc, None, Arc::new(NoopProgress))
        .await
        .unwrap();

    assert!(outcome.tests.unwrap().passed());
}
