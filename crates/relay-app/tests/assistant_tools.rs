//! The assistant's tools, against a real service and a real server.
//!
//! The important ones here are not the happy paths: they are the
//! guarantees. A secret must not reach the model, a credential typed
//! into a header must not either, and a tool that sends must be
//! classified so the interface asks first.

use std::sync::Arc;

use axum::{Json, Router, routing::get};
use relay_agent::tool::{ToolCall, ToolRisk};
use relay_app::{MemorySecretStore, RelayService};
use relay_http::HttpEngine;
use relay_models::{HttpMethod, KeyValue, Variable};
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
    let app = Router::new().route(
        "/thing",
        get(|headers: axum::http::HeaderMap| async move {
            Json(serde_json::json!({
                "sawAuth": headers
                    .get("authorization")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or(""),
            }))
        }),
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

fn call(name: &str, input: serde_json::Value) -> ToolCall {
    ToolCall {
        id: "c1".into(),
        name: name.into(),
        input,
    }
}

#[tokio::test]
async fn a_secret_variable_value_never_reaches_the_model() {
    // The rule this whole file exists for. An agent can be steered by
    // whatever a web page said; it is a weaker vessel than a script the
    // user wrote, and scripts have never been allowed to read these.
    let service = service();
    let workspace = service.active_workspace().await.unwrap();

    let mut environment = service
        .environment_create(workspace.id.clone(), "Live".into())
        .await
        .unwrap();
    environment.variables = vec![
        Variable {
            key: "apiSecret".into(),
            value: "sk-live-MUST-NOT-LEAK".into(),
            enabled: true,
            secret: true,
            description: None,
        },
        Variable::new("baseUrl", "https://api.example.com"),
    ];
    service.environment_save(environment).await.unwrap();

    let tools = relay_app::assistant::registry(service, workspace.id);
    let outcome = tools
        .dispatch(&call("list_environments", serde_json::json!({})))
        .await
        .unwrap();

    assert!(
        !outcome.content.contains("sk-live-MUST-NOT-LEAK"),
        "the secret reached the model: {}",
        outcome.content
    );
    // It still knows the variable exists, which is what it needs to
    // write {{apiSecret}} into a request.
    assert!(outcome.content.contains("apiSecret"));
    assert!(outcome.content.contains(r#""secret":true"#));
    // And a non-secret value is readable, or it cannot be useful.
    assert!(outcome.content.contains("https://api.example.com"));
}

#[tokio::test]
async fn a_credential_typed_into_a_header_is_redacted_too() {
    // Not every credential is a variable reference. Somebody pastes a
    // bearer token into the header box and it is just as sensitive.
    let service = service();
    let workspace = service.active_workspace().await.unwrap();

    let mut doc = service
        .request_create(workspace.id.clone(), "Thing".into(), None)
        .await
        .unwrap();
    doc.headers = vec![
        KeyValue::new("Authorization", "Bearer sk-live-PASTED-BY-HAND"),
        KeyValue::new("Accept", "application/json"),
    ];
    let doc = service.request_save(doc).await.unwrap();

    let tools = relay_app::assistant::registry(service, workspace.id);
    let outcome = tools
        .dispatch(&call(
            "get_request",
            serde_json::json!({ "id": doc.id.as_str() }),
        ))
        .await
        .unwrap();

    assert!(
        !outcome.content.contains("sk-live-PASTED-BY-HAND"),
        "{}",
        outcome.content
    );
    // The model can still see the header is set, which it needs to
    // reason about the request.
    assert!(outcome.content.contains("Authorization"));
    assert!(outcome.content.contains("application/json"));
}

#[tokio::test]
async fn sending_is_classified_so_the_interface_asks_first() {
    let service = service();
    let workspace = service.active_workspace().await.unwrap();
    let tools = relay_app::assistant::registry(service, workspace.id);

    assert_eq!(tools.risk_of("send_request"), Some(ToolRisk::Send));
    assert_eq!(tools.risk_of("delete_request"), Some(ToolRisk::Destructive));
    assert_eq!(tools.risk_of("list_requests"), Some(ToolRisk::Read));
    assert_eq!(tools.risk_of("create_request"), Some(ToolRisk::Write));

    assert!(tools.risk_of("send_request").unwrap().needs_confirmation());
    assert!(!tools.risk_of("list_requests").unwrap().needs_confirmation());
}

#[tokio::test]
async fn the_assistant_can_build_and_send_a_request() {
    // The thing the feature is for: ask for something, get it made and
    // sent, read the answer.
    let server = start().await;
    let service = service();
    let workspace = service.active_workspace().await.unwrap();
    let tools = relay_app::assistant::registry(service.clone(), workspace.id.clone());

    let created = tools
        .dispatch(&call(
            "create_request",
            serde_json::json!({
                "name": "Get the thing",
                "method": "get",
                "url": format!("{}/thing", server.base),
                "headers": { "Authorization": "Bearer from-the-assistant" },
            }),
        ))
        .await
        .unwrap();

    let id = serde_json::from_str::<serde_json::Value>(&created.content).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    // Lowercase "get" became a real method.
    let saved = service
        .request_get(relay_models::RequestId::from(id.as_str()))
        .await
        .unwrap();
    assert_eq!(saved.method, HttpMethod::Get);

    let sent = tools
        .dispatch(&call("send_request", serde_json::json!({ "id": id })))
        .await
        .unwrap();

    assert!(!sent.is_error, "{}", sent.content);
    let response: serde_json::Value = serde_json::from_str(&sent.content).unwrap();
    assert_eq!(response["status"], 200);
    // The header the assistant set actually went out.
    assert!(
        response["body"]
            .as_str()
            .unwrap()
            .contains("from-the-assistant")
    );
}

#[tokio::test]
async fn updating_leaves_fields_it_was_not_given_alone() {
    // A model that sets one field must not blank the others.
    let service = service();
    let workspace = service.active_workspace().await.unwrap();

    let mut doc = service
        .request_create(workspace.id.clone(), "Original".into(), None)
        .await
        .unwrap();
    doc.url = "https://api.example.com/v1".into();
    doc.method = HttpMethod::Post;
    doc.headers = vec![KeyValue::new("Accept", "application/json")];
    let doc = service.request_save(doc).await.unwrap();

    let tools = relay_app::assistant::registry(service.clone(), workspace.id);
    tools
        .dispatch(&call(
            "update_request",
            serde_json::json!({ "id": doc.id.as_str(), "name": "Renamed" }),
        ))
        .await
        .unwrap();

    let after = service.request_get(doc.id).await.unwrap();
    assert_eq!(after.name, "Renamed");
    assert_eq!(after.url, "https://api.example.com/v1");
    assert_eq!(after.method, HttpMethod::Post);
    assert_eq!(after.headers.len(), 1);
}

#[tokio::test]
async fn a_header_is_replaced_by_name_rather_than_duplicated() {
    let service = service();
    let workspace = service.active_workspace().await.unwrap();

    let mut doc = service
        .request_create(workspace.id.clone(), "Thing".into(), None)
        .await
        .unwrap();
    doc.headers = vec![KeyValue::new("Content-Type", "text/plain")];
    let doc = service.request_save(doc).await.unwrap();

    let tools = relay_app::assistant::registry(service.clone(), workspace.id);
    tools
        .dispatch(&call(
            "update_request",
            serde_json::json!({
                "id": doc.id.as_str(),
                "headers": { "content-type": "application/json" },
            }),
        ))
        .await
        .unwrap();

    let after = service.request_get(doc.id).await.unwrap();
    assert_eq!(after.headers.len(), 1, "{:?}", after.headers);
    assert_eq!(after.headers[0].value, "application/json");
    // The user's own spelling is kept.
    assert_eq!(after.headers[0].key, "Content-Type");
}

#[tokio::test]
async fn a_missing_argument_is_reported_rather_than_guessed() {
    let service = service();
    let workspace = service.active_workspace().await.unwrap();
    let tools = relay_app::assistant::registry(service, workspace.id);

    let error = tools
        .dispatch(&call("get_request", serde_json::json!({})))
        .await
        .unwrap_err();

    assert!(error.to_string().contains("'id' is required"), "{error}");
}

#[tokio::test]
async fn a_long_response_body_is_truncated_before_it_reaches_the_context() {
    // An unbounded body would be re-read, and charged for, on every
    // subsequent turn of the conversation.
    let big = "x".repeat(50_000);
    let body = Arc::new(big.clone());
    let app = Router::new().route(
        "/big",
        get(move || {
            let body = body.clone();
            async move { (*body).clone() }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    let service = service();
    let workspace = service.active_workspace().await.unwrap();
    let mut doc = service
        .request_create(workspace.id.clone(), "Big".into(), None)
        .await
        .unwrap();
    doc.url = format!("http://{addr}/big");
    let doc = service.request_save(doc).await.unwrap();

    let tools = relay_app::assistant::registry(service, workspace.id);
    let sent = tools
        .dispatch(&call(
            "send_request",
            serde_json::json!({ "id": doc.id.as_str() }),
        ))
        .await
        .unwrap();

    let response: serde_json::Value = serde_json::from_str(&sent.content).unwrap();
    assert_eq!(response["truncated"], true);
    assert!(response["body"].as_str().unwrap().len() < 5_000);

    handle.abort();
}

#[tokio::test]
async fn every_tool_declares_a_schema_the_model_can_follow() {
    // A tool with no schema is one the model calls with nothing.
    let service = service();
    let workspace = service.active_workspace().await.unwrap();
    let tools = relay_app::assistant::registry(service, workspace.id);

    assert!(tools.len() >= 9);
    for tool in tools.iter() {
        let schema = tool.input_schema();
        assert_eq!(
            schema["type"],
            "object",
            "{} has no object schema",
            tool.name()
        );
        assert!(
            !tool.description().trim().is_empty(),
            "{} has no description, which is the only thing telling the model when to use it",
            tool.name()
        );
    }
}
