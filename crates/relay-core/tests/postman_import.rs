//! Postman import, tested against the shapes real exports actually take.

use relay_core::postman::{import_any, import_collection, import_environment};
use relay_models::{ApiKeyLocation, AuthConfig, FormValue, HttpMethod, RequestBody, WorkspaceId};

fn ws() -> WorkspaceId {
    WorkspaceId::from("w1")
}

/// A collection exercising most of what v2.1 can express.
const FULL_COLLECTION: &str = r#"{
  "info": {
    "_postman_id": "abc-123",
    "name": "Example API",
    "description": "The example API.",
    "schema": "https://schema.getpostman.com/json/collection/v2.1.0/collection.json"
  },
  "auth": {
    "type": "bearer",
    "bearer": [{ "key": "token", "value": "{{apiToken}}", "type": "string" }]
  },
  "variable": [
    { "key": "baseUrl", "value": "https://api.example.com" },
    { "key": "apiToken", "value": "leaked-token", "type": "secret" }
  ],
  "item": [
    {
      "name": "Users",
      "description": "User endpoints.",
      "item": [
        {
          "name": "List users",
          "request": {
            "method": "GET",
            "header": [
              { "key": "Accept", "value": "application/json" },
              { "key": "X-Debug", "value": "1", "disabled": true }
            ],
            "url": {
              "raw": "{{baseUrl}}/users?page=1&limit=20",
              "host": ["{{baseUrl}}"],
              "path": ["users"],
              "query": [
                { "key": "page", "value": "1" },
                { "key": "limit", "value": "20" },
                { "key": "verbose", "value": "true", "disabled": true }
              ]
            }
          },
          "response": [{ "name": "200 OK", "body": "{}" }]
        },
        {
          "name": "Create user",
          "request": {
            "method": "POST",
            "header": [{ "key": "Content-Type", "value": "application/json" }],
            "body": {
              "mode": "raw",
              "raw": "{\n  \"name\": \"Ada\"\n}",
              "options": { "raw": { "language": "json" } }
            },
            "url": { "raw": "{{baseUrl}}/users", "host": ["{{baseUrl}}"], "path": ["users"] }
          },
          "event": [
            {
              "listen": "test",
              "script": {
                "type": "text/javascript",
                "exec": ["pm.test('created', function () {", "  pm.response.to.have.status(201)", "})"]
              }
            }
          ]
        },
        {
          "name": "Get user",
          "request": {
            "method": "GET",
            "auth": {
              "type": "apikey",
              "apikey": [
                { "key": "key", "value": "X-API-Key" },
                { "key": "value", "value": "{{apiKey}}" },
                { "key": "in", "value": "header" }
              ]
            },
            "url": {
              "raw": "{{baseUrl}}/users/:id",
              "host": ["{{baseUrl}}"],
              "path": ["users", ":id"],
              "variable": [{ "key": "id", "value": "42" }]
            }
          }
        }
      ]
    },
    {
      "name": "Upload avatar",
      "request": {
        "method": "POST",
        "body": {
          "mode": "formdata",
          "formdata": [
            { "key": "name", "value": "Ada", "type": "text" },
            { "key": "avatar", "type": "file", "src": "/tmp/avatar.png" },
            { "key": "ignored", "value": "x", "type": "text", "disabled": true }
          ]
        },
        "url": { "raw": "{{baseUrl}}/avatar", "host": ["{{baseUrl}}"], "path": ["avatar"] }
      }
    },
    {
      "name": "Login",
      "request": {
        "method": "POST",
        "auth": {
          "type": "basic",
          "basic": [
            { "key": "username", "value": "alice" },
            { "key": "password", "value": "hunter2" }
          ]
        },
        "body": {
          "mode": "urlencoded",
          "urlencoded": [
            { "key": "grant_type", "value": "password" },
            { "key": "scope", "value": "read", "disabled": true }
          ]
        },
        "url": { "raw": "{{baseUrl}}/login", "host": ["{{baseUrl}}"], "path": ["login"] }
      }
    }
  ]
}"#;

#[test]
fn imports_the_collection_as_a_root_folder() {
    let report = import_collection(FULL_COLLECTION, ws()).unwrap();

    assert_eq!(report.source_format, "Postman Collection v2.1");

    let root = report
        .collections
        .iter()
        .find(|c| c.parent_id.is_none())
        .expect("no root collection");
    assert_eq!(root.name, "Example API");
    assert_eq!(root.description, "The example API.");
}

#[test]
fn nested_folders_keep_their_parents() {
    let report = import_collection(FULL_COLLECTION, ws()).unwrap();

    let root = report
        .collections
        .iter()
        .find(|c| c.parent_id.is_none())
        .unwrap();
    let users = report
        .collections
        .iter()
        .find(|c| c.name == "Users")
        .expect("Users folder missing");

    assert_eq!(users.parent_id.as_ref(), Some(&root.id));
    assert_eq!(users.description, "User endpoints.");

    // The three requests inside Users point at it.
    let inside = report
        .requests
        .iter()
        .filter(|r| r.collection_id.as_ref() == Some(&users.id))
        .count();
    assert_eq!(inside, 3);
}

#[test]
fn top_level_requests_attach_to_the_root_collection() {
    let report = import_collection(FULL_COLLECTION, ws()).unwrap();
    let root = report
        .collections
        .iter()
        .find(|c| c.parent_id.is_none())
        .unwrap();

    let upload = report
        .requests
        .iter()
        .find(|r| r.name == "Upload avatar")
        .unwrap();
    assert_eq!(upload.collection_id.as_ref(), Some(&root.id));
}

#[test]
fn imports_method_url_and_query_parameters() {
    let report = import_collection(FULL_COLLECTION, ws()).unwrap();
    let request = report
        .requests
        .iter()
        .find(|r| r.name == "List users")
        .unwrap();

    assert_eq!(request.method, HttpMethod::Get);
    // The query string is split off so the parameters stay editable.
    assert_eq!(request.url, "{{baseUrl}}/users");
    assert_eq!(request.query_params.len(), 3);
    assert_eq!(request.query_params[0].key, "page");
    assert_eq!(request.query_params[0].value, "1");
    assert!(request.query_params[0].enabled);
}

#[test]
fn disabled_rows_stay_disabled_rather_than_being_dropped() {
    // Postman records `disabled`; Relay records `enabled`. Getting the
    // inversion wrong would silently start sending a header the user turned
    // off, which is worse than losing it.
    let report = import_collection(FULL_COLLECTION, ws()).unwrap();
    let request = report
        .requests
        .iter()
        .find(|r| r.name == "List users")
        .unwrap();

    let debug = request.headers.iter().find(|h| h.key == "X-Debug").unwrap();
    assert!(!debug.enabled);

    let accept = request.headers.iter().find(|h| h.key == "Accept").unwrap();
    assert!(accept.enabled);

    let verbose = request
        .query_params
        .iter()
        .find(|p| p.key == "verbose")
        .unwrap();
    assert!(!verbose.enabled);
}

#[test]
fn imports_a_json_body_by_its_declared_language() {
    let report = import_collection(FULL_COLLECTION, ws()).unwrap();
    let request = report
        .requests
        .iter()
        .find(|r| r.name == "Create user")
        .unwrap();

    match &request.body {
        RequestBody::Json { text } => assert!(text.contains("\"name\": \"Ada\"")),
        other => panic!("expected a JSON body, got {other:?}"),
    }
}

#[test]
fn imports_urlencoded_bodies() {
    let report = import_collection(FULL_COLLECTION, ws()).unwrap();
    let request = report.requests.iter().find(|r| r.name == "Login").unwrap();

    match &request.body {
        RequestBody::UrlEncoded { fields } => {
            assert_eq!(fields.len(), 2);
            assert_eq!(fields[0].key, "grant_type");
            assert!(!fields[1].enabled, "the disabled field lost its state");
        }
        other => panic!("expected a urlencoded body, got {other:?}"),
    }
}

#[test]
fn imports_multipart_bodies_including_file_fields() {
    let report = import_collection(FULL_COLLECTION, ws()).unwrap();
    let request = report
        .requests
        .iter()
        .find(|r| r.name == "Upload avatar")
        .unwrap();

    match &request.body {
        RequestBody::Multipart { fields } => {
            assert_eq!(fields.len(), 3);
            assert_eq!(
                fields[0].value,
                FormValue::Text {
                    value: "Ada".into()
                }
            );
            match &fields[1].value {
                FormValue::File { path, .. } => assert_eq!(path, "/tmp/avatar.png"),
                other => panic!("expected a file field, got {other:?}"),
            }
            assert!(!fields[2].enabled);
        }
        other => panic!("expected multipart, got {other:?}"),
    }
}

#[test]
fn imports_basic_and_api_key_auth() {
    let report = import_collection(FULL_COLLECTION, ws()).unwrap();

    let login = report.requests.iter().find(|r| r.name == "Login").unwrap();
    assert_eq!(
        login.auth,
        AuthConfig::Basic {
            username: "alice".into(),
            password: "hunter2".into()
        }
    );

    let get_user = report
        .requests
        .iter()
        .find(|r| r.name == "Get user")
        .unwrap();
    assert_eq!(
        get_user.auth,
        AuthConfig::ApiKey {
            key: "X-API-Key".into(),
            value: "{{apiKey}}".into(),
            location: ApiKeyLocation::Header,
        }
    );
}

#[test]
fn collection_level_auth_becomes_the_folders_auth_and_requests_inherit() {
    let report = import_collection(FULL_COLLECTION, ws()).unwrap();

    let root = report
        .collections
        .iter()
        .find(|c| c.parent_id.is_none())
        .unwrap();
    assert_eq!(
        root.auth,
        AuthConfig::Bearer {
            token: "{{apiToken}}".into()
        }
    );

    // A request with no auth block of its own inherits, which is what
    // Postman does too.
    let list = report
        .requests
        .iter()
        .find(|r| r.name == "List users")
        .unwrap();
    assert_eq!(list.auth, AuthConfig::Inherit);
}

#[test]
fn path_variables_are_imported_separately_from_the_url() {
    let report = import_collection(FULL_COLLECTION, ws()).unwrap();
    let request = report
        .requests
        .iter()
        .find(|r| r.name == "Get user")
        .unwrap();

    assert_eq!(request.url, "{{baseUrl}}/users/:id");
    assert_eq!(request.path_params.len(), 1);
    assert_eq!(request.path_params[0].key, "id");
    assert_eq!(request.path_params[0].value, "42");
}

#[test]
fn collection_variables_are_imported_and_secrets_are_not() {
    let report = import_collection(FULL_COLLECTION, ws()).unwrap();
    let root = report
        .collections
        .iter()
        .find(|c| c.parent_id.is_none())
        .unwrap();

    let base = root.variables.iter().find(|v| v.key == "baseUrl").unwrap();
    assert_eq!(base.value, "https://api.example.com");
    assert!(!base.secret);

    // Postman writes the secret's value into the export file. Relay keeps the
    // name and drops the value rather than copying it into its database.
    let token = root.variables.iter().find(|v| v.key == "apiToken").unwrap();
    assert!(token.secret);
    assert_eq!(token.value, "");

    let serialised = serde_json::to_string(&report.collections).unwrap();
    assert!(
        !serialised.contains("leaked-token"),
        "a secret was imported"
    );
}

#[test]
fn scripts_are_carried_across_but_reported_as_not_running() {
    let report = import_collection(FULL_COLLECTION, ws()).unwrap();
    let request = report
        .requests
        .iter()
        .find(|r| r.name == "Create user")
        .unwrap();

    assert!(request.scripts.post_response.contains("pm.test"));
    assert!(
        report
            .warnings
            .iter()
            .any(|w| w.path.contains("Create user") && w.message.contains("does not execute")),
        "the user was not told the script will not run: {:?}",
        report.warnings
    );
}

#[test]
fn saved_example_responses_are_reported_as_lost() {
    let report = import_collection(FULL_COLLECTION, ws()).unwrap();
    assert!(
        report
            .warnings
            .iter()
            .any(|w| w.message.contains("example responses")),
        "losing saved examples was not reported"
    );
}

#[test]
fn warnings_name_where_they_came_from() {
    let report = import_collection(FULL_COLLECTION, ws()).unwrap();
    for warning in &report.warnings {
        assert!(!warning.path.is_empty(), "a warning had no source path");
        assert!(!warning.message.is_empty());
    }
}

#[test]
fn unsupported_auth_schemes_are_reported_not_silently_dropped() {
    let json = r#"{
      "info": { "name": "X", "schema": "https://schema.getpostman.com/json/collection/v2.1.0/collection.json" },
      "item": [{
        "name": "Digest request",
        "request": {
          "method": "GET",
          "auth": { "type": "digest", "digest": [{ "key": "username", "value": "a" }] },
          "url": "https://example.com"
        }
      }]
    }"#;

    let report = import_collection(json, ws()).unwrap();
    assert_eq!(report.requests[0].auth, AuthConfig::None);
    assert!(
        report.warnings.iter().any(|w| w.message.contains("digest")),
        "an unsupported auth scheme was dropped silently"
    );
}

#[test]
fn a_bare_string_url_is_accepted() {
    let json = r#"{
      "info": { "name": "X", "schema": "v2.1.0" },
      "item": [{
        "name": "Simple",
        "request": { "method": "GET", "url": "https://example.com/a?b=1" }
      }]
    }"#;

    let report = import_collection(json, ws()).unwrap();
    assert_eq!(report.requests[0].url, "https://example.com/a");
    assert_eq!(report.requests[0].query_params[0].key, "b");
}

#[test]
fn a_url_with_no_raw_field_is_rebuilt_from_its_parts() {
    // Older exports omit `raw` and give only the exploded form.
    let json = r#"{
      "info": { "name": "X", "schema": "v2.1.0" },
      "item": [{
        "name": "Exploded",
        "request": {
          "method": "GET",
          "url": {
            "protocol": "https",
            "host": ["api", "example", "com"],
            "port": "8443",
            "path": ["v1", "users"]
          }
        }
      }]
    }"#;

    let report = import_collection(json, ws()).unwrap();
    assert_eq!(
        report.requests[0].url,
        "https://api.example.com:8443/v1/users"
    );
}

#[test]
fn an_object_description_is_read_from_its_content_field() {
    let json = r#"{
      "info": {
        "name": "X",
        "description": { "content": "Markdown description", "type": "text/markdown" },
        "schema": "v2.1.0"
      },
      "item": []
    }"#;

    let report = import_collection(json, ws()).unwrap();
    assert_eq!(report.collections[0].description, "Markdown description");
}

#[test]
fn numeric_values_do_not_break_the_import() {
    // Exports in the wild contain numbers where the schema says string.
    let json = r#"{
      "info": { "name": "X", "schema": "v2.1.0" },
      "item": [{
        "name": "Numeric",
        "request": {
          "method": "GET",
          "header": [{ "key": "X-Count", "value": 42 }],
          "url": { "raw": "https://example.com", "query": [{ "key": "page", "value": 2 }] }
        }
      }]
    }"#;

    let report = import_collection(json, ws()).unwrap();
    assert_eq!(report.requests[0].headers[0].value, "42");
    assert_eq!(report.requests[0].query_params[0].value, "2");
}

#[test]
fn an_empty_collection_imports_as_just_its_root() {
    let json = r#"{ "info": { "name": "Empty", "schema": "v2.1.0" }, "item": [] }"#;
    let report = import_collection(json, ws()).unwrap();

    assert_eq!(report.collections.len(), 1);
    assert!(report.requests.is_empty());
    assert!(!report.is_empty());
}

fn nested_collection(depth: usize) -> String {
    let mut json = String::from(r#"{"info":{"name":"Deep","schema":"v2.1.0"},"item":["#);
    for i in 0..depth {
        json.push_str(&format!(r#"{{"name":"L{i}","item":["#));
    }
    json.push_str(&"]}".repeat(depth));
    json.push_str("]}");
    json
}

#[test]
fn folders_nested_past_relays_limit_are_reported_not_imported() {
    // Deeper than Relay's own guard but still parseable, so this exercises
    // the guard rather than the JSON parser.
    let report = import_collection(&nested_collection(40), ws()).unwrap();

    assert!(
        report.warnings.iter().any(|w| w.message.contains("nested")),
        "hitting the depth limit was not reported: {:?}",
        report.warnings
    );
    assert!(report.collections.len() < 40);
}

#[test]
fn folders_nested_past_the_parsers_limit_fail_with_an_actionable_message() {
    // serde_json refuses this before Relay sees it, which is what keeps a
    // malicious file from exhausting the stack. The message must still tell
    // the user what to do rather than claiming their JSON is malformed.
    let err = import_collection(&nested_collection(200), ws()).unwrap_err();
    let message = err.to_string();

    assert!(message.contains("nests folders"), "got {message}");
    assert!(
        !message.contains("not valid JSON"),
        "well-formed JSON was called invalid: {message}"
    );
}

#[test]
fn invalid_json_is_rejected_with_a_readable_message() {
    let err = import_collection("{not json", ws()).unwrap_err();
    assert!(err.to_string().contains("not valid JSON"), "got {err}");
}

#[test]
fn json_that_is_not_a_collection_is_rejected() {
    let err = import_collection(r#"{"hello":"world"}"#, ws()).unwrap_err();
    assert!(err.to_string().contains("item"), "got {err}");
}

// --- environments ---------------------------------------------------------

const ENVIRONMENT: &str = r#"{
  "id": "env-1",
  "name": "Production",
  "values": [
    { "key": "baseUrl", "value": "https://api.example.com", "enabled": true, "type": "default" },
    { "key": "apiToken", "value": "sk-live-SHOULD-NOT-IMPORT", "enabled": true, "type": "secret" },
    { "key": "legacy", "value": "x", "enabled": false, "type": "default" }
  ],
  "_postman_variable_scope": "environment"
}"#;

#[test]
fn imports_an_environment() {
    let report = import_environment(ENVIRONMENT, ws()).unwrap();

    assert_eq!(report.environments.len(), 1);
    let environment = &report.environments[0];
    assert_eq!(environment.name, "Production");
    assert_eq!(environment.variables.len(), 3);

    let base = environment
        .variables
        .iter()
        .find(|v| v.key == "baseUrl")
        .unwrap();
    assert_eq!(base.value, "https://api.example.com");
    assert!(base.enabled);

    let legacy = environment
        .variables
        .iter()
        .find(|v| v.key == "legacy")
        .unwrap();
    assert!(!legacy.enabled);
}

#[test]
fn environment_secrets_are_not_imported_and_the_user_is_told() {
    let report = import_environment(ENVIRONMENT, ws()).unwrap();

    let token = report.environments[0]
        .variables
        .iter()
        .find(|v| v.key == "apiToken")
        .unwrap();
    assert!(token.secret);
    assert_eq!(token.value, "", "a secret value was imported");

    let serialised = serde_json::to_string(&report).unwrap();
    assert!(!serialised.contains("sk-live-SHOULD-NOT-IMPORT"));

    assert!(
        report
            .warnings
            .iter()
            .any(|w| w.message.contains("credential store")),
        "the user was not told their secret needs re-entering: {:?}",
        report.warnings
    );
}

#[test]
fn import_any_recognises_both_shapes() {
    let collection = import_any(FULL_COLLECTION, ws()).unwrap();
    assert!(!collection.requests.is_empty());
    assert!(collection.environments.is_empty());

    let environment = import_any(ENVIRONMENT, ws()).unwrap();
    assert!(environment.requests.is_empty());
    assert_eq!(environment.environments.len(), 1);
}

#[test]
fn import_any_rejects_unrelated_json_clearly() {
    let err = import_any(r#"{"openapi":"3.0.0"}"#, ws()).unwrap_err();
    assert!(err.to_string().contains("neither"), "got {err}");
}

#[test]
fn every_imported_object_belongs_to_the_target_workspace() {
    let target = WorkspaceId::from("target-workspace");
    let report = import_collection(FULL_COLLECTION, target.clone()).unwrap();

    assert!(report.collections.iter().all(|c| c.workspace_id == target));
    assert!(report.requests.iter().all(|r| r.workspace_id == target));
}

#[test]
fn ordering_within_a_folder_is_preserved() {
    let report = import_collection(FULL_COLLECTION, ws()).unwrap();
    let users = report
        .collections
        .iter()
        .find(|c| c.name == "Users")
        .unwrap();

    let mut inside: Vec<_> = report
        .requests
        .iter()
        .filter(|r| r.collection_id.as_ref() == Some(&users.id))
        .collect();
    inside.sort_by(|a, b| a.sort_order.total_cmp(&b.sort_order));

    assert_eq!(
        inside.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(),
        ["List users", "Create user", "Get user"]
    );
}

/// A collection using each OAuth 2.0 grant Postman can express.
const OAUTH_COLLECTION: &str = r#"{
  "info": {
    "name": "OAuth API",
    "schema": "https://schema.getpostman.com/json/collection/v2.1.0/collection.json"
  },
  "item": [
    {
      "name": "Authorization code",
      "request": {
        "method": "GET",
        "url": "https://api.example.com/me",
        "auth": {
          "type": "oauth2",
          "oauth2": [
            { "key": "grant_type", "value": "authorization_code_with_pkce" },
            { "key": "authUrl", "value": "https://id.example.com/authorize" },
            { "key": "accessTokenUrl", "value": "https://id.example.com/token" },
            { "key": "clientId", "value": "abc-123" },
            { "key": "clientSecret", "value": "shhh" },
            { "key": "scope", "value": "read:me" },
            { "key": "redirect_uri", "value": "http://127.0.0.1:8723/callback" },
            { "key": "accessToken", "value": "stale-token-from-the-file" },
            { "key": "client_authentication", "value": "body" }
          ]
        }
      }
    },
    {
      "name": "Client credentials",
      "request": {
        "method": "GET",
        "url": "https://api.example.com/stats",
        "auth": {
          "type": "oauth2",
          "oauth2": [
            { "key": "grant_type", "value": "client_credentials" },
            { "key": "accessTokenUrl", "value": "https://id.example.com/token" },
            { "key": "clientId", "value": "svc" },
            { "key": "clientSecret", "value": "svc-secret" },
            { "key": "audience", "value": "https://api.example.com" }
          ]
        }
      }
    },
    {
      "name": "Implicit",
      "request": {
        "method": "GET",
        "url": "https://api.example.com/old",
        "auth": {
          "type": "oauth2",
          "oauth2": [
            { "key": "grant_type", "value": "implicit" },
            { "key": "authUrl", "value": "https://id.example.com/authorize" },
            { "key": "clientId", "value": "legacy" }
          ]
        }
      }
    }
  ]
}"#;

fn oauth_of(report: &relay_core::import::ImportReport, name: &str) -> relay_models::OAuth2Config {
    let request = report
        .requests
        .iter()
        .find(|r| r.name == name)
        .unwrap_or_else(|| panic!("no request named {name}"));

    match &request.auth {
        AuthConfig::OAuth2(config) => (**config).clone(),
        other => panic!("expected OAuth2 for {name}, got {other:?}"),
    }
}

#[test]
fn an_oauth_authorization_code_grant_imports_its_whole_configuration() {
    use relay_models::{ClientAuthStyle, OAuth2Grant};

    let report = import_collection(OAUTH_COLLECTION, ws()).unwrap();
    let config = oauth_of(&report, "Authorization code");

    assert_eq!(config.grant, OAuth2Grant::AuthorizationCode);
    assert_eq!(config.auth_url, "https://id.example.com/authorize");
    assert_eq!(config.token_url, "https://id.example.com/token");
    assert_eq!(config.client_id, "abc-123");
    assert_eq!(config.client_secret, "shhh");
    assert_eq!(config.scope, "read:me");
    assert_eq!(config.redirect_uri, "http://127.0.0.1:8723/callback");
    assert_eq!(config.client_auth, ClientAuthStyle::Body);
    assert!(config.use_pkce);
}

#[test]
fn a_cached_token_in_the_file_is_not_imported() {
    // Collections get emailed around and committed to repositories. A
    // credential inside one should not be silently adopted and then
    // written to the local database.
    let report = import_collection(OAUTH_COLLECTION, ws()).unwrap();
    let config = oauth_of(&report, "Authorization code");

    assert_eq!(config.access_token, None);
    assert_eq!(config.refresh_token, None);

    assert!(
        report
            .warnings
            .iter()
            .any(|w| w.message.contains("cached OAuth access token")),
        "the user should be told why their token did not come across: {:?}",
        report.warnings
    );
}

#[test]
fn a_client_credentials_grant_does_not_ask_for_pkce() {
    use relay_models::OAuth2Grant;

    let report = import_collection(OAUTH_COLLECTION, ws()).unwrap();
    let config = oauth_of(&report, "Client credentials");

    assert_eq!(config.grant, OAuth2Grant::ClientCredentials);
    assert_eq!(config.audience, "https://api.example.com");
    assert!(
        !config.use_pkce,
        "PKCE belongs to the browser grants; asking for it here would be sent and ignored"
    );
}

#[test]
fn the_implicit_grant_is_imported_but_flagged() {
    let report = import_collection(OAUTH_COLLECTION, ws()).unwrap();

    assert!(
        report
            .warnings
            .iter()
            .any(|w| w.message.contains("implicit grant")),
        "{:?}",
        report.warnings
    );
}
