//! Reading a schema out of a real endpoint, over a real socket.
//!
//! The parser has its own tests. This is about the path: that the
//! lookup goes out as the request it belongs to, with that request's
//! authentication, and that a server refusing introspection — which
//! most production servers do — says so rather than failing
//! mysteriously.

use std::sync::Arc;

use axum::{Json, Router, routing::post};
use relay_app::{MemorySecretStore, RelayService};
use relay_http::HttpEngine;
use relay_models::{AuthConfig, HttpMethod, RequestBody, RequestDoc};
use relay_storage::Database;
use serde_json::{Value, json};

/// A GraphQL endpoint that answers introspection only when a token
/// was presented, so the test can tell whether auth came along.
async fn endpoint(require_token: bool, refuse: bool) -> (String, tokio::task::JoinHandle<()>) {
    let app = Router::new().route(
        "/graphql",
        post(
            move |headers: axum::http::HeaderMap, Json(body): Json<Value>| async move {
                let authorised = headers
                    .get("authorization")
                    .and_then(|v| v.to_str().ok())
                    .is_some_and(|v| v.contains("sekrit"));

                if require_token && !authorised {
                    return Json(json!({
                        "errors": [{ "message": "unauthorised" }]
                    }));
                }
                if refuse {
                    return Json(json!({
                        "errors": [{ "message": "GraphQL introspection is not allowed" }]
                    }));
                }
                // Prove it really was the introspection query.
                let query = body["query"].as_str().unwrap_or_default();
                assert!(query.contains("__schema"), "not an introspection query");

                Json(json!({ "data": { "__schema": {
                    "queryType": { "name": "Query" },
                    "mutationType": null,
                    "subscriptionType": null,
                    "types": [{
                        "kind": "OBJECT", "name": "Query", "description": null,
                        "fields": [{
                            "name": "invoices",
                            "description": "Every invoice.",
                            "args": [],
                            "type": { "kind": "NON_NULL", "name": null, "ofType": {
                                "kind": "LIST", "name": null,
                                "ofType": { "kind": "OBJECT", "name": "Invoice" }
                            }}
                        }]
                    }]
                }}}))
            },
        ),
    );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}"), handle)
}

fn service() -> RelayService {
    RelayService::new(
        Database::open_in_memory().unwrap(),
        HttpEngine::with_defaults(),
        Arc::new(MemorySecretStore::new()),
    )
}

async fn a_request(service: &RelayService, url: String) -> RequestDoc {
    let workspace = service.active_workspace().await.unwrap();
    let mut doc = service
        .request_create(workspace.id.clone(), "Invoices".into(), None)
        .await
        .unwrap();
    doc.url = url;
    // Deliberately GET: introspection must post regardless.
    doc.method = HttpMethod::Get;
    doc.body = RequestBody::GraphQl {
        query: "{ invoices { id } }".into(),
        variables: String::new(),
    };
    doc
}

#[tokio::test]
async fn a_schema_comes_back_from_a_live_endpoint() {
    let (base, _server) = endpoint(false, false).await;
    let service = service();
    let doc = a_request(&service, format!("{base}/graphql")).await;

    let schema = service.graphql_schema(doc, None).await.unwrap();

    assert_eq!(schema.queries.len(), 1);
    assert_eq!(schema.queries[0].name, "invoices");
    assert_eq!(schema.queries[0].type_name, "[Invoice]!");
}

#[tokio::test]
async fn the_lookup_carries_the_requests_own_authentication() {
    // The whole reason this goes through the ordinary request path.
    // Every interesting API is behind a key, and a schema browser
    // that could not reach one would be for public demos only.
    let (base, _server) = endpoint(true, false).await;
    let service = service();

    let mut doc = a_request(&service, format!("{base}/graphql")).await;
    doc.auth = AuthConfig::Bearer {
        token: "sekrit".into(),
    };

    let schema = service.graphql_schema(doc, None).await.unwrap();
    assert_eq!(schema.queries.len(), 1);
}

#[tokio::test]
async fn without_the_token_it_reports_what_the_server_said() {
    let (base, _server) = endpoint(true, false).await;
    let service = service();
    let doc = a_request(&service, format!("{base}/graphql")).await;

    let error = service.graphql_schema(doc, None).await.unwrap_err();
    assert!(error.message.contains("unauthorised"), "{}", error.message);
}

#[tokio::test]
async fn introspection_turned_off_is_explained_not_swallowed() {
    // The common case in production, and the one most likely to be
    // reported as "the schema button does nothing".
    let (base, _server) = endpoint(false, true).await;
    let service = service();
    let doc = a_request(&service, format!("{base}/graphql")).await;

    let error = service.graphql_schema(doc, None).await.unwrap_err();
    assert!(error.message.contains("not allowed"), "{}", error.message);
}

#[tokio::test]
async fn a_url_that_is_not_graphql_says_so_usefully() {
    let service = service();
    let doc = a_request(&service, "http://127.0.0.1:1/nope".into()).await;

    // Unreachable rather than unparseable: either way it must not
    // come back as a bare "error".
    let error = service.graphql_schema(doc, None).await.unwrap_err();
    assert!(!error.message.is_empty());
}
