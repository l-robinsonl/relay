//! Pre-request scripts, through the service and over a real socket.

use std::sync::Arc;

use axum::http::HeaderMap;
use axum::{Json, Router, routing::any};
use relay_app::{MemorySecretStore, RelayService};
use relay_http::{HttpEngine, NoopProgress};
use relay_models::{ExecutionId, HttpExchange, HttpMethod, RequestDoc, ResponseBody, Variable};
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

/// Echoes back exactly what it was sent, so a test can see the request
/// the script produced rather than only that the send succeeded.
async fn start() -> Server {
    let app = Router::new().route(
        "/echo",
        any(
            |method: axum::http::Method,
             uri: axum::http::Uri,
             headers: HeaderMap,
             body: axum::body::Bytes| async move {
                let seen: std::collections::BTreeMap<_, _> = headers
                    .iter()
                    .map(|(k, v)| (k.as_str().to_string(), v.to_str().unwrap_or("").to_string()))
                    .collect();

                Json(serde_json::json!({
                    "method": method.as_str(),
                    "query": uri.query().unwrap_or(""),
                    "headers": seen,
                    "body": String::from_utf8_lossy(&body),
                }))
            },
        ),
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

async fn doc_for(service: &RelayService, url: String, script: &str) -> RequestDoc {
    let workspace = service.active_workspace().await.unwrap();
    let mut doc = service
        .request_create(workspace.id, "Echo".into(), None)
        .await
        .unwrap();
    doc.method = HttpMethod::Get;
    doc.url = url;
    doc.scripts.pre_request = script.to_string();
    doc
}

/// Sends and returns what the echo server saw.
async fn echoed(service: &RelayService, doc: RequestDoc) -> serde_json::Value {
    let exchange = service
        .request_send(ExecutionId::new(), doc, None, Arc::new(NoopProgress))
        .await
        .unwrap();

    match &exchange {
        HttpExchange::Success { response, .. } => match &response.body {
            ResponseBody::Text { text } => serde_json::from_str(text).unwrap(),
            other => panic!("expected a text body, got {other:?}"),
        },
        other => panic!("expected success, got {other:?}"),
    }
}

#[tokio::test]
async fn a_script_can_add_a_header_to_the_request_it_precedes() {
    let server = start().await;
    let service = service();

    let doc = doc_for(
        &service,
        format!("{}/echo", server.base),
        "relay.setHeader('X-Signature', 'computed-at-send');",
    )
    .await;

    let seen = echoed(&service, doc).await;
    assert_eq!(seen["headers"]["x-signature"], "computed-at-send");
}

#[tokio::test]
async fn a_variable_a_script_sets_resolves_in_the_url() {
    // The common Postman idiom: compute a nonce, reference it in the URL.
    let server = start().await;
    let service = service();

    let doc = doc_for(
        &service,
        format!("{}/echo?nonce={{{{nonce}}}}", server.base),
        "relay.setVariable('nonce', 'n-42');",
    )
    .await;

    let seen = echoed(&service, doc).await;
    assert_eq!(seen["query"], "nonce=n-42");
}

#[tokio::test]
async fn a_script_sees_variables_already_resolved() {
    let server = start().await;
    let service = service();

    let workspace = service.active_workspace().await.unwrap();
    let mut ws = workspace.clone();
    ws.variables = vec![Variable {
        key: "edition".into(),
        value: "v2".into(),
        enabled: true,
        secret: false,
        description: None,
    }];
    service.workspace_save(ws).await.unwrap();

    let mut doc = doc_for(
        &service,
        format!("{}/echo", server.base),
        "relay.setHeader('X-Seen-Url', request.url);",
    )
    .await;
    doc.url = format!("{}/echo?edition={{{{edition}}}}", server.base);

    let seen = echoed(&service, doc).await;
    assert!(
        seen["headers"]["x-seen-url"]
            .as_str()
            .unwrap()
            .contains("edition=v2"),
        "the script saw an unresolved URL: {}",
        seen["headers"]["x-seen-url"]
    );
}

#[tokio::test]
async fn the_method_and_body_can_be_replaced() {
    let server = start().await;
    let service = service();

    let doc = doc_for(
        &service,
        format!("{}/echo", server.base),
        "relay.setMethod('POST'); relay.setBody('from the script');",
    )
    .await;

    let seen = echoed(&service, doc).await;
    assert_eq!(seen["method"], "POST");
    assert_eq!(seen["body"], "from the script");
}

#[tokio::test]
async fn a_script_cannot_reach_a_secret_through_a_variable_it_sets() {
    // The guarantee: a script never receives a secret value. Setting a
    // variable whose value *references* one must not make resolution
    // fetch it — otherwise the rule has a hole exactly the shape of this
    // test, and a collection from a colleague could post your API key
    // to an address of its choosing.
    let server = start().await;
    let service = service();

    let workspace = service.active_workspace().await.unwrap();
    let mut environment = service
        .environment_create(workspace.id.clone(), "Live".into())
        .await
        .unwrap();
    environment.variables = vec![Variable {
        key: "apiSecret".into(),
        value: String::new(),
        enabled: true,
        secret: true,
        description: None,
    }];
    let environment = service.environment_save(environment).await.unwrap();
    service
        .secret_set(&environment.id, "apiSecret", "sk-live-DO-NOT-LEAK")
        .unwrap();

    let mut doc = doc_for(
        &service,
        // The request legitimately uses the secret, so it is in the
        // context — the only case where the hole could open.
        format!(
            "{}/echo?real={{{{apiSecret}}}}&stolen={{{{loot}}}}",
            server.base
        ),
        "relay.setVariable('loot', '{{apiSecret}}');",
    )
    .await;
    doc.scripts.pre_request = "relay.setVariable('loot', '{{apiSecret}}');".into();

    let exchange = service
        .request_send(
            ExecutionId::new(),
            doc,
            Some(environment.id.clone()),
            Arc::new(NoopProgress),
        )
        .await
        .unwrap();

    let seen: serde_json::Value = match &exchange {
        HttpExchange::Success { response, .. } => match &response.body {
            ResponseBody::Text { text } => serde_json::from_str(text).unwrap(),
            other => panic!("{other:?}"),
        },
        other => panic!("{other:?}"),
    };

    let query = seen["query"].as_str().unwrap();
    assert!(
        query.contains("real=sk-live-DO-NOT-LEAK"),
        "the request's own use of the secret should still work: {query}"
    );
    assert!(
        !query.contains("stolen=sk-live-DO-NOT-LEAK"),
        "the script reached the secret through a variable: {query}"
    );
}

#[tokio::test]
async fn a_script_cannot_reach_a_secret_through_a_header_it_sets() {
    // The same hole through the other door: a patch is applied after
    // resolution, so a reference in it is sent as the literal text.
    let server = start().await;
    let service = service();

    let workspace = service.active_workspace().await.unwrap();
    let mut environment = service
        .environment_create(workspace.id.clone(), "Live".into())
        .await
        .unwrap();
    environment.variables = vec![Variable {
        key: "apiSecret".into(),
        value: String::new(),
        enabled: true,
        secret: true,
        description: None,
    }];
    let environment = service.environment_save(environment).await.unwrap();
    service
        .secret_set(&environment.id, "apiSecret", "sk-live-DO-NOT-LEAK")
        .unwrap();

    let doc = doc_for(
        &service,
        format!("{}/echo?real={{{{apiSecret}}}}", server.base),
        "relay.setHeader('X-Stolen', '{{apiSecret}}');",
    )
    .await;

    let exchange = service
        .request_send(
            ExecutionId::new(),
            doc,
            Some(environment.id),
            Arc::new(NoopProgress),
        )
        .await
        .unwrap();

    let seen: serde_json::Value = match &exchange {
        HttpExchange::Success { response, .. } => match &response.body {
            ResponseBody::Text { text } => serde_json::from_str(text).unwrap(),
            other => panic!("{other:?}"),
        },
        other => panic!("{other:?}"),
    };

    assert_eq!(seen["headers"]["x-stolen"], "{{apiSecret}}");
}

#[tokio::test]
async fn a_failing_script_stops_the_request() {
    // A pre-request script usually exists to produce something the
    // request needs. Sending without it turns a clear error into a 401.
    let server = start().await;
    let service = service();

    let doc = doc_for(
        &service,
        format!("{}/echo", server.base),
        "throw new Error('the signing key is missing');",
    )
    .await;

    let error = service
        .request_send(ExecutionId::new(), doc, None, Arc::new(NoopProgress))
        .await
        .unwrap_err();

    assert!(
        error.message.contains("the signing key is missing"),
        "{error:?}"
    );
    assert_eq!(error.detail.as_deref(), Some("The request was not sent."));
}

#[tokio::test]
async fn a_scripts_console_output_comes_back_with_the_response() {
    let server = start().await;
    let service = service();

    let doc = doc_for(
        &service,
        format!("{}/echo", server.base),
        "console.log('signing as', 'ada');",
    )
    .await;

    let outcome = service
        .request_send_with_tests(ExecutionId::new(), doc, None, Arc::new(NoopProgress))
        .await
        .unwrap();

    let pre = outcome.pre_request.expect("the pre-request outcome");
    assert_eq!(pre.console.len(), 1);
    assert!(
        pre.console[0].text.contains("signing as ada"),
        "{:?}",
        pre.console
    );
}

#[tokio::test]
async fn a_pre_request_variable_does_not_outlive_the_request() {
    // It runs on every send; persisting a nonce each time would churn
    // the workspace and push a revision per request.
    let server = start().await;
    let service = service();

    let doc = doc_for(
        &service,
        format!("{}/echo?n={{{{nonce}}}}", server.base),
        "relay.setVariable('nonce', 'n-1');",
    )
    .await;
    let workspace_id = doc.workspace_id.clone();

    echoed(&service, doc).await;

    let workspace = service
        .workspace_list()
        .await
        .unwrap()
        .into_iter()
        .find(|w| w.id == workspace_id)
        .unwrap();
    assert!(
        !workspace.variables.iter().any(|v| v.key == "nonce"),
        "the nonce was persisted: {:?}",
        workspace.variables
    );
}

#[tokio::test]
async fn a_request_with_no_script_is_unaffected() {
    let server = start().await;
    let service = service();

    let doc = doc_for(&service, format!("{}/echo", server.base), "   ").await;
    let seen = echoed(&service, doc).await;

    assert_eq!(seen["method"], "GET");
    assert_eq!(seen["query"], "");
}
