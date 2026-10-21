//! Import and export through the service, against a real database.

use std::sync::Arc;

use relay_app::{MemorySecretStore, RelayService};
use relay_http::HttpEngine;
use relay_models::{AuthConfig, RequestBody, Variable};
use relay_storage::Database;

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

const COLLECTION: &str = r#"{
  "info": {
    "name": "Example API",
    "schema": "https://schema.getpostman.com/json/collection/v2.1.0/collection.json"
  },
  "variable": [{ "key": "baseUrl", "value": "https://api.example.com" }],
  "item": [
    {
      "name": "Users",
      "item": [
        {
          "name": "List users",
          "request": {
            "method": "GET",
            "url": { "raw": "{{baseUrl}}/users", "host": ["{{baseUrl}}"], "path": ["users"] }
          }
        },
        {
          "name": "Create user",
          "request": {
            "method": "POST",
            "body": {
              "mode": "raw",
              "raw": "{\"name\":\"Ada\"}",
              "options": { "raw": { "language": "json" } }
            },
            "url": { "raw": "{{baseUrl}}/users", "host": ["{{baseUrl}}"], "path": ["users"] }
          }
        }
      ]
    }
  ]
}"#;

#[tokio::test]
async fn importing_a_postman_collection_does_not_save_until_applied() {
    let service = service();
    let workspace = service.active_workspace().await.unwrap();

    let report = service
        .import_postman(COLLECTION, workspace.id.clone())
        .unwrap();

    assert_eq!(report.requests.len(), 2);
    assert_eq!(report.collections.len(), 2); // the root plus Users

    // Nothing has been written yet: the user has not accepted it.
    assert!(
        service
            .collection_tree(workspace.id.clone())
            .await
            .unwrap()
            .is_empty(),
        "the import wrote to the database before being applied"
    );

    let saved = service.import_apply(report).await.unwrap();
    assert_eq!(saved, 4);

    let tree = service.collection_tree(workspace.id).await.unwrap();
    assert_eq!(tree.len(), 4);
}

#[tokio::test]
async fn an_imported_collection_keeps_its_structure_in_the_database() {
    let service = service();
    let workspace = service.active_workspace().await.unwrap();

    let report = service
        .import_postman(COLLECTION, workspace.id.clone())
        .unwrap();
    service.import_apply(report).await.unwrap();

    let collections = service.collection_list(workspace.id.clone()).await.unwrap();
    let root = collections.iter().find(|c| c.parent_id.is_none()).unwrap();
    let users = collections.iter().find(|c| c.name == "Users").unwrap();
    assert_eq!(users.parent_id.as_ref(), Some(&root.id));

    let requests = service.request_list(workspace.id).await.unwrap();
    assert_eq!(requests.len(), 2);
    assert!(
        requests
            .iter()
            .all(|r| r.collection_id.as_ref() == Some(&users.id))
    );

    let create = requests.iter().find(|r| r.name == "Create user").unwrap();
    assert!(matches!(create.body, RequestBody::Json { .. }));
}

#[tokio::test]
async fn an_imported_request_can_actually_be_sent() {
    // The real test of an importer is whether what comes out works.
    use axum::{Router, routing::get};

    let app = Router::new().route("/users", get(|| async { "[]" }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    let service = service();
    let workspace = service.active_workspace().await.unwrap();

    let report = service
        .import_postman(COLLECTION, workspace.id.clone())
        .unwrap();
    service.import_apply(report).await.unwrap();

    // Supply the variable the collection references.
    let mut environment = service
        .environment_create(workspace.id.clone(), "Local".into())
        .await
        .unwrap();
    environment.variables = vec![Variable::new("baseUrl", format!("http://{addr}"))];
    let environment = service.environment_save(environment).await.unwrap();

    let requests = service.request_list(workspace.id).await.unwrap();
    let list = requests.iter().find(|r| r.name == "List users").unwrap();

    let exchange = service
        .request_send(
            relay_models::ExecutionId::new(),
            list.clone(),
            Some(environment.id),
            Arc::new(relay_http::NoopProgress),
        )
        .await
        .unwrap();

    match exchange {
        relay_models::HttpExchange::Success { response, .. } => {
            assert_eq!(response.status, 200);
        }
        other => panic!("the imported request did not work: {other:?}"),
    }

    server.abort();
}

#[tokio::test]
async fn a_failed_import_writes_nothing_at_all() {
    let service = service();
    let workspace = service.active_workspace().await.unwrap();

    let mut report = service
        .import_postman(COLLECTION, workspace.id.clone())
        .unwrap();

    // A request with no name is rejected by the repository, which must take
    // the whole import down rather than leaving folders with no contents.
    if let Some(request) = report.requests.get_mut(0) {
        request.name = "   ".into();
    }

    assert!(service.import_apply(report).await.is_err());
    assert!(
        service
            .collection_tree(workspace.id)
            .await
            .unwrap()
            .is_empty(),
        "a failed import left rows behind"
    );
}

#[tokio::test]
async fn a_postman_environment_imports_and_is_selectable() {
    let service = service();
    let workspace = service.active_workspace().await.unwrap();

    let json = r#"{
      "name": "Staging",
      "values": [
        { "key": "baseUrl", "value": "https://staging.example.com", "enabled": true },
        { "key": "token", "value": "LEAKED", "enabled": true, "type": "secret" }
      ]
    }"#;

    let report = service.import_postman(json, workspace.id.clone()).unwrap();
    service.import_apply(report).await.unwrap();

    let environments = service.environment_list(workspace.id).await.unwrap();
    assert_eq!(environments.len(), 1);
    assert_eq!(environments[0].name, "Staging");

    let serialised = serde_json::to_string(&environments).unwrap();
    assert!(!serialised.contains("LEAKED"), "a secret was persisted");
}

#[tokio::test]
async fn a_workspace_exports_and_re_imports_intact() {
    let service = service();
    let workspace = service.active_workspace().await.unwrap();

    let collection = service
        .collection_create(workspace.id.clone(), "API".into(), None)
        .await
        .unwrap();

    let mut request = service
        .request_create(
            workspace.id.clone(),
            "Get thing".into(),
            Some(collection.id.clone()),
        )
        .await
        .unwrap();
    request.url = "{{baseUrl}}/thing".into();
    request.auth = AuthConfig::Bearer {
        token: "{{token}}".into(),
    };
    service.request_save(request.clone()).await.unwrap();

    let mut environment = service
        .environment_create(workspace.id.clone(), "Production".into())
        .await
        .unwrap();
    environment.variables = vec![
        Variable::new("baseUrl", "https://api.example.com"),
        Variable::new("token", "").into_secret(),
    ];
    service.environment_save(environment.clone()).await.unwrap();
    service
        .secret_set(&environment.id, "token", "REAL-SECRET")
        .unwrap();

    let exported = service
        .export_workspace(workspace.id.clone())
        .await
        .unwrap();

    // The export must not carry the secret, even though it is stored.
    assert!(
        !exported.contains("REAL-SECRET"),
        "the export leaked a secret"
    );
    assert!(exported.contains("https://api.example.com"));

    // Import it into a brand new workspace.
    let other = service.workspace_create("Restored".into()).await.unwrap();
    let report = service.import_relay(&exported, other.id.clone()).unwrap();
    service.import_apply(report).await.unwrap();

    let restored = service.request_list(other.id.clone()).await.unwrap();
    assert_eq!(restored.len(), 1);
    assert_eq!(restored[0].url, "{{baseUrl}}/thing");
    assert_eq!(
        restored[0].auth,
        AuthConfig::Bearer {
            token: "{{token}}".into()
        }
    );

    let collections = service.collection_list(other.id.clone()).await.unwrap();
    assert_eq!(collections.len(), 1);
    assert_eq!(
        restored[0].collection_id.as_ref(),
        Some(&collections[0].id),
        "the restored request lost its folder"
    );

    let environments = service.environment_list(other.id).await.unwrap();
    assert_eq!(environments.len(), 1);
    let token = environments[0]
        .variables
        .iter()
        .find(|v| v.key == "token")
        .unwrap();
    assert!(token.secret);
    assert_eq!(token.value, "", "the secret came back from the export");
}

#[tokio::test]
async fn a_workspace_survives_a_full_backup_and_restore_cycle() {
    // Export from one database, restore into a completely separate one, the
    // way a user moving between machines would.
    let source = service();
    let workspace = source.active_workspace().await.unwrap();

    let report = source
        .import_postman(COLLECTION, workspace.id.clone())
        .unwrap();
    source.import_apply(report).await.unwrap();
    let exported = source.export_workspace(workspace.id).await.unwrap();

    let destination = service_with(Database::open_in_memory().unwrap());
    let target = destination.active_workspace().await.unwrap();
    let report = destination
        .import_relay(&exported, target.id.clone())
        .unwrap();
    destination.import_apply(report).await.unwrap();

    let tree = destination.collection_tree(target.id).await.unwrap();
    assert_eq!(tree.len(), 4, "the restore lost objects");
}

#[tokio::test]
async fn import_detect_picks_the_right_importer() {
    let service = service();
    let workspace = service.active_workspace().await.unwrap();

    let postman = service
        .import_detect(COLLECTION, workspace.id.clone())
        .unwrap();
    assert!(postman.source_format.starts_with("Postman"));

    let exported = service
        .export_workspace(workspace.id.clone())
        .await
        .unwrap();
    let relay = service.import_detect(&exported, workspace.id).unwrap();
    assert!(relay.source_format.starts_with("Relay"));
}

#[tokio::test]
async fn an_unrecognised_file_is_rejected_with_a_useful_message() {
    let service = service();
    let workspace = service.active_workspace().await.unwrap();

    let err = service
        .import_detect(r#"{"openapi":"3.0.0","paths":{}}"#, workspace.id)
        .unwrap_err();

    assert_eq!(err.kind, relay_models::ErrorKind::Import);
    assert!(!err.message.is_empty());
}

#[tokio::test]
async fn importing_the_same_collection_twice_does_not_merge_the_two() {
    // Each import generates fresh ids, so a second import is a second copy
    // rather than an in-place overwrite the user did not ask for.
    let service = service();
    let workspace = service.active_workspace().await.unwrap();

    for _ in 0..2 {
        let report = service
            .import_postman(COLLECTION, workspace.id.clone())
            .unwrap();
        service.import_apply(report).await.unwrap();
    }

    let requests = service.request_list(workspace.id).await.unwrap();
    assert_eq!(requests.len(), 4);
}

const OPENAPI: &str = r#"
openapi: 3.0.3
info: { title: Weather, version: '1.0' }
servers:
  - url: https://api.weather.example
paths:
  /forecast:
    get:
      tags: [forecast]
      summary: Get the forecast
      parameters:
        - name: city
          in: query
          required: true
          schema: { type: string }
      responses: { '200': { description: OK } }
"#;

#[tokio::test]
async fn an_openapi_document_is_detected_and_imported() {
    let service = service();
    let workspace = service.active_workspace().await.unwrap();

    let report = service
        .import_detect(OPENAPI, workspace.id.clone())
        .unwrap();
    assert!(
        report.source_format.starts_with("OpenAPI"),
        "{}",
        report.source_format
    );

    service.import_apply(report).await.unwrap();

    let requests = service.request_list(workspace.id.clone()).await.unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].name, "Get the forecast");
    assert_eq!(requests[0].url, "{{baseUrl}}/forecast");

    // Grouped under its tag, inside the document's own collection.
    let collections = service.collection_list(workspace.id).await.unwrap();
    assert_eq!(collections.len(), 2);
    assert!(collections.iter().any(|c| c.name == "Weather"));
    assert!(collections.iter().any(|c| c.name == "forecast"));
}

#[tokio::test]
async fn an_imported_openapi_request_actually_sends() {
    // The real test of an importer is whether what comes out works.
    use axum::{Router, routing::get};

    let app = Router::new().route("/forecast", get(|| async { "sunny" }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    let service = service();
    let workspace = service.active_workspace().await.unwrap();
    let report = service
        .import_detect(OPENAPI, workspace.id.clone())
        .unwrap();
    service.import_apply(report).await.unwrap();

    let mut environment = service
        .environment_create(workspace.id.clone(), "Local".into())
        .await
        .unwrap();
    environment.variables = vec![Variable::new("baseUrl", format!("http://{addr}"))];
    let environment = service.environment_save(environment).await.unwrap();

    let doc = service.request_list(workspace.id).await.unwrap().remove(0);
    let exchange = service
        .request_send(
            relay_models::ExecutionId::new(),
            doc,
            Some(environment.id),
            Arc::new(relay_http::NoopProgress),
        )
        .await
        .unwrap();

    match exchange {
        relay_models::HttpExchange::Success { response, .. } => {
            assert_eq!(response.status, 200);
        }
        other => panic!("the imported request did not work: {other:?}"),
    }

    server.abort();
}

#[tokio::test]
async fn detection_tells_the_three_formats_apart() {
    let service = service();
    let workspace = service.active_workspace().await.unwrap();

    assert!(
        service
            .import_detect(COLLECTION, workspace.id.clone())
            .unwrap()
            .source_format
            .starts_with("Postman")
    );
    assert!(
        service
            .import_detect(OPENAPI, workspace.id.clone())
            .unwrap()
            .source_format
            .starts_with("OpenAPI")
    );

    let exported = service
        .export_workspace(workspace.id.clone())
        .await
        .unwrap();
    assert!(
        service
            .import_detect(&exported, workspace.id.clone())
            .unwrap()
            .source_format
            .starts_with("Relay")
    );
}

#[tokio::test]
async fn an_unrecognised_file_names_every_format_it_tried() {
    let service = service();
    let workspace = service.active_workspace().await.unwrap();

    let error = service
        .import_detect(r#"{"something":"else"}"#, workspace.id)
        .unwrap_err();

    for expected in ["Relay", "OpenAPI", "Postman"] {
        assert!(
            error.message.contains(expected),
            "the error did not mention {expected}: {}",
            error.message
        );
    }
}
