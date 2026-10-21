//! OpenAPI import, against the shapes real specifications take.

use relay_core::openapi::import_openapi;
use relay_models::{ApiKeyLocation, AuthConfig, HttpMethod, RequestBody, WorkspaceId};

fn ws() -> WorkspaceId {
    WorkspaceId::from("w1")
}

const PETSTORE: &str = r#"
openapi: 3.0.3
info:
  title: Petstore
  description: A small API.
  version: 1.0.0
servers:
  - url: https://api.petstore.example/v1
security:
  - bearerAuth: []
components:
  securitySchemes:
    bearerAuth:
      type: http
      scheme: bearer
    apiKeyAuth:
      type: apiKey
      name: X-API-Key
      in: header
  schemas:
    Pet:
      type: object
      required: [name]
      properties:
        id:
          type: integer
          format: int64
        name:
          type: string
        status:
          type: string
          enum: [available, pending, sold]
        birthday:
          type: string
          format: date
        tags:
          type: array
          items:
            type: string
paths:
  /pets:
    parameters:
      - name: X-Request-Id
        in: header
        required: false
        schema:
          type: string
          format: uuid
    get:
      tags: [pets]
      summary: List pets
      parameters:
        - name: limit
          in: query
          required: false
          schema:
            type: integer
        - name: status
          in: query
          required: true
          schema:
            type: string
            enum: [available, sold]
      responses:
        '200':
          description: OK
    post:
      tags: [pets]
      operationId: createPet
      requestBody:
        required: true
        content:
          application/json:
            schema:
              $ref: '#/components/schemas/Pet'
      responses:
        '201':
          description: Created
  /pets/{petId}:
    get:
      tags: [pets]
      summary: Get a pet
      parameters:
        - name: petId
          in: path
          required: true
          schema:
            type: integer
      responses:
        '200':
          description: OK
    delete:
      tags: [admin]
      summary: Delete a pet
      deprecated: true
      security:
        - apiKeyAuth: []
      parameters:
        - name: petId
          in: path
          required: true
          schema:
            type: integer
      responses:
        '204':
          description: Gone
  /health:
    get:
      summary: Health check
      security: []
      responses:
        '200':
          description: OK
"#;

#[test]
fn imports_yaml_and_names_the_version() {
    let report = import_openapi(PETSTORE, ws()).unwrap();
    assert_eq!(report.source_format, "OpenAPI 3.0.3");
}

#[test]
fn the_same_document_as_json_imports_identically() {
    // The extension is often wrong or absent, so content decides.
    let as_json =
        serde_json::to_string(&serde_norway::from_str::<serde_json::Value>(PETSTORE).unwrap())
            .unwrap();

    let yaml = import_openapi(PETSTORE, ws()).unwrap();
    let json = import_openapi(&as_json, ws()).unwrap();

    assert_eq!(yaml.requests.len(), json.requests.len());
    assert_eq!(yaml.collections.len(), json.collections.len());
}

#[test]
fn every_operation_becomes_a_request() {
    let report = import_openapi(PETSTORE, ws()).unwrap();
    let names: Vec<&str> = report.requests.iter().map(|r| r.name.as_str()).collect();

    assert_eq!(report.requests.len(), 5);
    assert!(names.contains(&"List pets"));
    assert!(names.contains(&"Get a pet"));
    assert!(names.contains(&"Health check"));
}

#[test]
fn an_operation_with_no_summary_falls_back_to_its_id() {
    let report = import_openapi(PETSTORE, ws()).unwrap();
    assert!(report.requests.iter().any(|r| r.name == "createPet"));
}

#[test]
fn the_server_becomes_a_base_url_variable() {
    let report = import_openapi(PETSTORE, ws()).unwrap();
    let root = report
        .collections
        .iter()
        .find(|c| c.parent_id.is_none())
        .unwrap();

    let base = root.variables.iter().find(|v| v.key == "baseUrl").unwrap();
    assert_eq!(base.value, "https://api.petstore.example/v1");

    let list = report
        .requests
        .iter()
        .find(|r| r.name == "List pets")
        .unwrap();
    assert_eq!(list.url, "{{baseUrl}}/pets");
}

#[test]
fn operations_are_grouped_into_folders_by_tag() {
    let report = import_openapi(PETSTORE, ws()).unwrap();

    let pets = report
        .collections
        .iter()
        .find(|c| c.name == "pets")
        .unwrap();
    let admin = report
        .collections
        .iter()
        .find(|c| c.name == "admin")
        .unwrap();
    let root = report
        .collections
        .iter()
        .find(|c| c.parent_id.is_none())
        .unwrap();

    assert_eq!(pets.parent_id.as_ref(), Some(&root.id));
    assert_eq!(admin.parent_id.as_ref(), Some(&root.id));

    let in_pets = report
        .requests
        .iter()
        .filter(|r| r.collection_id.as_ref() == Some(&pets.id))
        .count();
    assert_eq!(in_pets, 3);

    // An untagged operation lands in the root rather than being lost.
    let health = report
        .requests
        .iter()
        .find(|r| r.name == "Health check")
        .unwrap();
    assert_eq!(health.collection_id.as_ref(), Some(&root.id));
}

#[test]
fn required_parameters_arrive_enabled_and_optional_ones_do_not() {
    // The imported request should work as sent; the extras are one click.
    let report = import_openapi(PETSTORE, ws()).unwrap();
    let list = report
        .requests
        .iter()
        .find(|r| r.name == "List pets")
        .unwrap();

    let status = list
        .query_params
        .iter()
        .find(|p| p.key == "status")
        .unwrap();
    assert!(status.enabled, "a required parameter was switched off");

    let limit = list.query_params.iter().find(|p| p.key == "limit").unwrap();
    assert!(!limit.enabled, "an optional parameter was switched on");
}

#[test]
fn path_level_parameters_apply_to_every_operation_in_it() {
    let report = import_openapi(PETSTORE, ws()).unwrap();
    let list = report
        .requests
        .iter()
        .find(|r| r.name == "List pets")
        .unwrap();

    assert!(
        list.headers.iter().any(|h| h.key == "X-Request-Id"),
        "a path-level parameter was dropped"
    );
}

#[test]
fn path_parameters_are_imported_separately_from_the_url() {
    let report = import_openapi(PETSTORE, ws()).unwrap();
    let get = report
        .requests
        .iter()
        .find(|r| r.name == "Get a pet")
        .unwrap();

    assert_eq!(get.url, "{{baseUrl}}/pets/{petId}");
    assert_eq!(get.path_params.len(), 1);
    assert_eq!(get.path_params[0].key, "petId");
}

#[test]
fn an_enum_parameter_uses_its_first_value() {
    let report = import_openapi(PETSTORE, ws()).unwrap();
    let list = report
        .requests
        .iter()
        .find(|r| r.name == "List pets")
        .unwrap();
    let status = list
        .query_params
        .iter()
        .find(|p| p.key == "status")
        .unwrap();

    assert_eq!(status.value, "available");
}

#[test]
fn a_body_is_generated_from_the_schema_behind_a_ref() {
    let report = import_openapi(PETSTORE, ws()).unwrap();
    let create = report
        .requests
        .iter()
        .find(|r| r.name == "createPet")
        .unwrap();

    let RequestBody::Json { text } = &create.body else {
        panic!("expected a JSON body, got {:?}", create.body);
    };
    let value: serde_json::Value = serde_json::from_str(text).unwrap();

    assert_eq!(value["id"], 0);
    assert_eq!(value["name"], "string");
    // An enum contributes its first value, not a placeholder.
    assert_eq!(value["status"], "available");
    assert!(value["tags"].is_array());
}

#[test]
fn string_formats_produce_values_a_server_will_accept() {
    // "string" in a date field is a 400 and a puzzled developer.
    let report = import_openapi(PETSTORE, ws()).unwrap();
    let create = report
        .requests
        .iter()
        .find(|r| r.name == "createPet")
        .unwrap();

    let RequestBody::Json { text } = &create.body else {
        panic!("expected a JSON body");
    };
    let value: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(value["birthday"], "2024-01-01");

    let list = report
        .requests
        .iter()
        .find(|r| r.name == "List pets")
        .unwrap();
    let request_id = list
        .headers
        .iter()
        .find(|h| h.key == "X-Request-Id")
        .unwrap();
    assert!(
        request_id.value.contains('-'),
        "a uuid format gave {}",
        request_id.value
    );
}

#[test]
fn document_security_lands_on_the_collection_and_requests_inherit() {
    let report = import_openapi(PETSTORE, ws()).unwrap();
    let root = report
        .collections
        .iter()
        .find(|c| c.parent_id.is_none())
        .unwrap();

    assert_eq!(
        root.auth,
        AuthConfig::Bearer {
            token: "{{token}}".into()
        }
    );

    let list = report
        .requests
        .iter()
        .find(|r| r.name == "List pets")
        .unwrap();
    assert_eq!(list.auth, AuthConfig::Inherit);
}

#[test]
fn an_operation_can_override_the_documents_security() {
    let report = import_openapi(PETSTORE, ws()).unwrap();
    let delete = report
        .requests
        .iter()
        .find(|r| r.name == "Delete a pet")
        .unwrap();

    assert_eq!(
        delete.auth,
        AuthConfig::ApiKey {
            key: "X-API-Key".into(),
            value: "{{apiKey}}".into(),
            location: ApiKeyLocation::Header,
        }
    );
}

#[test]
fn an_empty_security_requirement_means_the_operation_is_public() {
    let report = import_openapi(PETSTORE, ws()).unwrap();
    let health = report
        .requests
        .iter()
        .find(|r| r.name == "Health check")
        .unwrap();

    assert_eq!(health.auth, AuthConfig::None);
}

#[test]
fn a_deprecated_operation_says_so() {
    let report = import_openapi(PETSTORE, ws()).unwrap();
    let delete = report
        .requests
        .iter()
        .find(|r| r.name == "Delete a pet")
        .unwrap();

    assert!(delete.description.starts_with("DEPRECATED"));
    assert!(delete.tags.contains(&"deprecated".to_string()));
}

#[test]
fn methods_are_imported_correctly() {
    let report = import_openapi(PETSTORE, ws()).unwrap();

    let by_name = |name: &str| {
        report
            .requests
            .iter()
            .find(|r| r.name == name)
            .unwrap()
            .method
            .clone()
    };

    assert_eq!(by_name("List pets"), HttpMethod::Get);
    assert_eq!(by_name("createPet"), HttpMethod::Post);
    assert_eq!(by_name("Delete a pet"), HttpMethod::Delete);
}

#[test]
fn losing_response_schemas_is_reported() {
    let report = import_openapi(PETSTORE, ws()).unwrap();
    assert!(
        report
            .warnings
            .iter()
            .any(|w| w.message.contains("Response schemas")),
        "the user was not told response schemas are not imported"
    );
}

#[test]
fn templated_server_variables_are_imported() {
    let spec = r#"
openapi: 3.0.0
info: { title: Regional, version: '1' }
servers:
  - url: https://{region}.example.com
    variables:
      region:
        default: eu-west-1
paths: {}
"#;
    let report = import_openapi(spec, ws()).unwrap();
    let root = &report.collections[0];

    assert_eq!(
        root.variables
            .iter()
            .find(|v| v.key == "baseUrl")
            .unwrap()
            .value,
        "https://{region}.example.com"
    );
    assert_eq!(
        root.variables
            .iter()
            .find(|v| v.key == "region")
            .unwrap()
            .value,
        "eu-west-1"
    );
}

#[test]
fn a_document_with_no_server_says_so_rather_than_producing_broken_urls() {
    let spec = "openapi: 3.0.0\ninfo: { title: X, version: '1' }\npaths: {}\n";
    let report = import_openapi(spec, ws()).unwrap();

    assert!(
        report
            .warnings
            .iter()
            .any(|w| w.message.contains("no server")),
        "an empty baseUrl was not reported"
    );
}

#[test]
fn several_servers_are_reported_so_the_others_are_not_a_surprise() {
    let spec = r#"
openapi: 3.0.0
info: { title: X, version: '1' }
servers:
  - url: https://prod.example.com
  - url: https://staging.example.com
paths: {}
"#;
    let report = import_openapi(spec, ws()).unwrap();
    assert!(
        report
            .warnings
            .iter()
            .any(|w| w.message.contains("2 servers"))
    );
    assert_eq!(
        report.collections[0]
            .variables
            .iter()
            .find(|v| v.key == "baseUrl")
            .unwrap()
            .value,
        "https://prod.example.com"
    );
}

#[test]
fn swagger_2_is_rejected_with_a_pointer_to_the_fix() {
    let spec = r#"{"swagger":"2.0","info":{"title":"Old"},"paths":{}}"#;
    let error = import_openapi(spec, ws()).unwrap_err().to_string();

    assert!(error.contains("Swagger 2.0"), "got {error}");
    assert!(
        error.contains("convert"),
        "the user was not told what to do"
    );
}

#[test]
fn a_file_that_is_neither_json_nor_yaml_is_rejected_readably() {
    let error = import_openapi("\t\x00not a document {{{", ws())
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("neither valid JSON nor valid YAML"),
        "got {error}"
    );
}

#[test]
fn an_unsupported_openapi_major_version_is_refused() {
    let spec = r#"{"openapi":"4.0.0","info":{"title":"Future"},"paths":{}}"#;
    let error = import_openapi(spec, ws()).unwrap_err().to_string();
    assert!(error.contains("not supported"), "got {error}");
}

#[test]
fn a_document_with_no_paths_imports_as_an_empty_collection() {
    let spec = "openapi: 3.1.0\ninfo: { title: Empty, version: '1' }\n";
    let report = import_openapi(spec, ws()).unwrap();

    assert_eq!(report.collections.len(), 1);
    assert!(report.requests.is_empty());
    assert!(
        report
            .warnings
            .iter()
            .any(|w| w.message.contains("no paths"))
    );
}

#[test]
fn an_explicit_example_beats_anything_generated() {
    let spec = r#"
openapi: 3.0.0
info: { title: X, version: '1' }
paths:
  /thing:
    post:
      summary: Make a thing
      requestBody:
        content:
          application/json:
            schema: { type: object, properties: { a: { type: string } } }
            example: { a: "the real thing" }
      responses: { '200': { description: OK } }
"#;
    let report = import_openapi(spec, ws()).unwrap();
    let RequestBody::Json { text } = &report.requests[0].body else {
        panic!("expected a JSON body");
    };
    assert!(text.contains("the real thing"));
}

#[test]
fn every_imported_object_belongs_to_the_target_workspace() {
    let target = WorkspaceId::from("target");
    let report = import_openapi(PETSTORE, target.clone()).unwrap();

    assert!(report.collections.iter().all(|c| c.workspace_id == target));
    assert!(report.requests.iter().all(|r| r.workspace_id == target));
}
