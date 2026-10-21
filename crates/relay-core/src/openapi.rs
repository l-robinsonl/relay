//! OpenAPI 3.x import.
//!
//! Turns a specification into a collection of requests that can actually
//! be sent: every operation becomes a request with its parameters, an
//! example body generated from the schema, and the security scheme the
//! document declares.
//!
//! A specification describes far more than a request collection can hold
//! — response schemas, callbacks, links, discriminated unions. Everything
//! that cannot be represented is reported rather than dropped, because
//! finding out later that an endpoint needs a header the importer skipped
//! is a bad afternoon.

use relay_models::{
    ApiKeyLocation, AuthConfig, Collection, CollectionId, HttpMethod, KeyValue, RequestBody,
    RequestDoc, Variable, WorkspaceId,
};
use serde_json::Value;
use std::collections::BTreeMap;

use crate::error::{CoreError, CoreResult};
use crate::import::ImportReport;

/// Methods an OpenAPI path item may define.
const METHODS: [&str; 7] = ["get", "put", "post", "delete", "options", "head", "patch"];

/// How deep schema expansion goes before giving up.
///
/// A self-referential schema — a tree node whose children are tree nodes —
/// is perfectly legal and would otherwise expand forever.
const MAX_SCHEMA_DEPTH: usize = 6;

/// Parses an OpenAPI 3.x document in JSON or YAML.
pub fn import_openapi(source: &str, workspace_id: WorkspaceId) -> CoreResult<ImportReport> {
    let root = parse(source)?;

    let version = root
        .get("openapi")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            CoreError::Import(
                "this does not look like an OpenAPI 3 document — it has no \"openapi\" version \
                 field. Swagger 2.0 files have \"swagger\" instead and are not supported; \
                 convert it to OpenAPI 3 first."
                    .into(),
            )
        })?
        .to_string();

    if !version.starts_with('3') {
        return Err(CoreError::Import(format!(
            "OpenAPI {version} is not supported; this reads 3.x"
        )));
    }

    let mut report = ImportReport::new(format!("OpenAPI {version}"));

    let title = root
        .get("info")
        .and_then(|i| i.get("title"))
        .and_then(Value::as_str)
        .unwrap_or("Imported API")
        .to_string();

    // The whole document becomes one root collection, so an import never
    // mixes into whatever is already in the workspace.
    let mut root_collection = Collection::new(workspace_id.clone(), &title);
    root_collection.description = root
        .get("info")
        .and_then(|i| i.get("description"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();

    let (base_url, server_variables) = resolve_server(&root, &mut report);
    root_collection.variables = std::iter::once(Variable::new("baseUrl", &base_url))
        .chain(server_variables)
        .collect();

    // Security declared at the document level applies to every operation
    // that does not override it, so it belongs on the collection.
    root_collection.auth = document_auth(&root, &mut report);

    let root_id = root_collection.id.clone();
    report.collections.push(root_collection);

    let paths = root.get("paths").and_then(Value::as_object);
    let Some(paths) = paths else {
        report.warn(&title, "The document declares no paths.");
        return Ok(report);
    };

    // Operations are grouped by their first tag, which is how a
    // specification already organises itself for its own documentation.
    let mut folders: BTreeMap<String, CollectionId> = BTreeMap::new();
    let mut order = 0.0;

    for (path, item) in paths {
        let Some(item) = item.as_object() else {
            continue;
        };

        // Parameters declared on the path apply to every operation in it.
        let shared = item.get("parameters").and_then(Value::as_array);

        for method in METHODS {
            let Some(operation) = item.get(method) else {
                continue;
            };

            let tag = operation
                .get("tags")
                .and_then(Value::as_array)
                .and_then(|tags| tags.first())
                .and_then(Value::as_str)
                .map(str::to_owned);

            let parent = match &tag {
                Some(name) => match folders.get(name) {
                    Some(existing) => existing.clone(),
                    None => {
                        let mut folder = Collection::new(workspace_id.clone(), name);
                        folder.parent_id = Some(root_id.clone());
                        folder.sort_order = folders.len() as f64;
                        let id = folder.id.clone();
                        folders.insert(name.clone(), id.clone());
                        report.collections.push(folder);
                        id
                    }
                },
                None => root_id.clone(),
            };

            let doc = build_request(
                &root,
                &workspace_id,
                &parent,
                path,
                method,
                operation,
                shared,
                order,
                &mut report,
            );
            order += 1.0;
            report.requests.push(doc);
        }
    }

    warn_about_unsupported(&root, &title, &mut report);
    Ok(report)
}

/// Reads the document as JSON, falling back to YAML.
///
/// Both are common in the wild and the extension is often wrong or
/// missing, so the content decides rather than the filename.
fn parse(source: &str) -> CoreResult<Value> {
    if let Ok(value) = serde_json::from_str::<Value>(source) {
        return Ok(value);
    }

    serde_norway::from_str::<Value>(source)
        .map_err(|e| CoreError::Import(format!("this is neither valid JSON nor valid YAML: {e}")))
}

/// Picks the first server and expands its template variables.
fn resolve_server(root: &Value, report: &mut ImportReport) -> (String, Vec<Variable>) {
    let servers = root.get("servers").and_then(Value::as_array);

    let Some(server) = servers.and_then(|s| s.first()) else {
        report.warn(
            "servers",
            "The document declares no server, so {{baseUrl}} was left empty. Set it in an \
             environment before sending anything.",
        );
        return (String::new(), Vec::new());
    };

    if servers.is_some_and(|s| s.len() > 1) {
        report.warn(
            "servers",
            format!(
                "{} servers are declared; the first was used. The others are worth an \
                 environment each.",
                servers.map(Vec::len).unwrap_or(0)
            ),
        );
    }

    let url = server
        .get("url")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim_end_matches('/')
        .to_string();

    // A server URL may itself be templated: https://{region}.example.com
    let variables = server
        .get("variables")
        .and_then(Value::as_object)
        .map(|vars| {
            vars.iter()
                .map(|(name, spec)| {
                    Variable::new(
                        name,
                        spec.get("default")
                            .and_then(Value::as_str)
                            .unwrap_or_default(),
                    )
                })
                .collect()
        })
        .unwrap_or_default();

    (url, variables)
}

/// Converts the document-level security requirement into an auth config.
fn document_auth(root: &Value, report: &mut ImportReport) -> AuthConfig {
    let scheme_name = root
        .get("security")
        .and_then(Value::as_array)
        .and_then(|s| s.first())
        .and_then(Value::as_object)
        .and_then(|o| o.keys().next().cloned());

    let Some(name) = scheme_name else {
        return AuthConfig::None;
    };

    let scheme = root
        .get("components")
        .and_then(|c| c.get("securitySchemes"))
        .and_then(|s| s.get(&name));

    let Some(scheme) = scheme else {
        report.warn(
            "security",
            format!("The security scheme '{name}' is referenced but not defined."),
        );
        return AuthConfig::None;
    };

    convert_scheme(&name, scheme, report)
}

fn convert_scheme(name: &str, scheme: &Value, report: &mut ImportReport) -> AuthConfig {
    match scheme.get("type").and_then(Value::as_str).unwrap_or("") {
        "http" => match scheme.get("scheme").and_then(Value::as_str).unwrap_or("") {
            "bearer" => AuthConfig::Bearer {
                token: "{{token}}".into(),
            },
            "basic" => AuthConfig::Basic {
                username: "{{username}}".into(),
                password: "{{password}}".into(),
            },
            other => {
                report.warn(
                    "security",
                    format!("The HTTP '{other}' scheme is not supported and was not imported."),
                );
                AuthConfig::None
            }
        },

        "apiKey" => {
            let key = scheme
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("X-API-Key")
                .to_string();
            let location = match scheme.get("in").and_then(Value::as_str) {
                Some("query") => ApiKeyLocation::Query,
                Some("cookie") => {
                    report.warn(
                        "security",
                        "A cookie API key was imported as a header; adjust it under Auth.",
                    );
                    ApiKeyLocation::Header
                }
                _ => ApiKeyLocation::Header,
            };
            AuthConfig::ApiKey {
                key,
                value: "{{apiKey}}".into(),
                location,
            }
        }

        "oauth2" => {
            report.warn(
                "security",
                "OAuth 2.0 was imported as a bearer token. Relay does not run OAuth flows \
                 yet, so supply a token in the {{token}} variable for now.",
            );
            AuthConfig::Bearer {
                token: "{{token}}".into(),
            }
        }

        "openIdConnect" => {
            report.warn(
                "security",
                "OpenID Connect was imported as a bearer token; the discovery document was \
                 not followed.",
            );
            AuthConfig::Bearer {
                token: "{{token}}".into(),
            }
        }

        other => {
            report.warn(
                "security",
                format!("The '{other}' security scheme '{name}' was not imported."),
            );
            AuthConfig::None
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn build_request(
    root: &Value,
    workspace_id: &WorkspaceId,
    parent: &CollectionId,
    path: &str,
    method: &str,
    operation: &Value,
    shared_parameters: Option<&Vec<Value>>,
    order: f64,
    report: &mut ImportReport,
) -> RequestDoc {
    let name = operation
        .get("summary")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .or_else(|| operation.get("operationId").and_then(Value::as_str))
        .map(str::to_owned)
        .unwrap_or_else(|| format!("{} {path}", method.to_uppercase()));

    let where_from = format!("{} {path}", method.to_uppercase());

    let mut doc = RequestDoc::new(workspace_id.clone(), name);
    doc.collection_id = Some(parent.clone());
    doc.sort_order = order;
    doc.method = HttpMethod::from(method.to_uppercase().as_str());
    doc.url = format!("{{{{baseUrl}}}}{path}");
    doc.description = operation
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();

    if let Some(tags) = operation.get("tags").and_then(Value::as_array) {
        doc.tags = tags
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect();
    }

    // Path-level parameters first, then the operation's own, so an
    // operation can override what the path declared.
    let mut parameters: Vec<&Value> = Vec::new();
    if let Some(shared) = shared_parameters {
        parameters.extend(shared.iter());
    }
    if let Some(own) = operation.get("parameters").and_then(Value::as_array) {
        parameters.extend(own.iter());
    }

    for parameter in parameters {
        let parameter = resolve_ref(root, parameter);
        let Some(name) = parameter.get("name").and_then(Value::as_str) else {
            continue;
        };
        let required = parameter
            .get("required")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let example = parameter_example(root, &parameter);
        let description = parameter
            .get("description")
            .and_then(Value::as_str)
            .map(str::to_owned);

        let entry = KeyValue {
            key: name.to_string(),
            value: example,
            // An optional parameter arrives switched off, so the request
            // works as sent and the extras are one click away.
            enabled: required,
            description,
        };

        match parameter.get("in").and_then(Value::as_str) {
            Some("query") => doc.query_params.push(entry),
            Some("header") => doc.headers.push(entry),
            Some("path") => doc.path_params.push(KeyValue {
                enabled: true,
                ..entry
            }),
            Some("cookie") => report.warn(
                &where_from,
                format!("The cookie parameter '{name}' was not imported."),
            ),
            _ => {}
        }
    }

    doc.body = build_body(root, operation, &where_from, report);

    // An operation may override the document's security.
    if let Some(security) = operation.get("security").and_then(Value::as_array) {
        doc.auth = match security
            .first()
            .and_then(Value::as_object)
            .and_then(|o| o.keys().next())
        {
            // An empty requirement means this operation is public.
            None => AuthConfig::None,
            Some(name) => root
                .get("components")
                .and_then(|c| c.get("securitySchemes"))
                .and_then(|s| s.get(name))
                .map(|scheme| convert_scheme(name, scheme, report))
                .unwrap_or(AuthConfig::None),
        };
    } else {
        doc.auth = AuthConfig::Inherit;
    }

    if operation.get("deprecated").and_then(Value::as_bool) == Some(true) {
        doc.description = format!("DEPRECATED. {}", doc.description)
            .trim_end()
            .to_string();
        doc.tags.push("deprecated".into());
    }

    doc
}

fn build_body(
    root: &Value,
    operation: &Value,
    where_from: &str,
    report: &mut ImportReport,
) -> RequestBody {
    let Some(body) = operation.get("requestBody") else {
        return RequestBody::None;
    };
    let body = resolve_ref(root, body);

    let Some(content) = body.get("content").and_then(Value::as_object) else {
        return RequestBody::None;
    };

    // JSON first: it is what most APIs want and what Relay renders best.
    let chosen = content
        .get("application/json")
        .map(|c| ("application/json", c))
        .or_else(|| {
            content
                .iter()
                .find(|(k, _)| k.contains("json"))
                .map(|(k, v)| (k.as_str(), v))
        })
        .or_else(|| content.iter().next().map(|(k, v)| (k.as_str(), v)));

    let Some((media_type, media)) = chosen else {
        return RequestBody::None;
    };

    if content.len() > 1 {
        report.warn(
            where_from,
            format!(
                "The request body accepts {} media types; '{media_type}' was used.",
                content.len()
            ),
        );
    }

    // An explicit example beats anything generated from the schema.
    if let Some(example) = media.get("example") {
        return RequestBody::Json {
            text: serde_json::to_string_pretty(example).unwrap_or_default(),
        };
    }

    let Some(schema) = media.get("schema") else {
        return RequestBody::None;
    };

    let example = example_from_schema(root, schema, 0);

    match media_type {
        t if t.contains("json") => RequestBody::Json {
            text: serde_json::to_string_pretty(&example).unwrap_or_default(),
        },
        "application/x-www-form-urlencoded" => RequestBody::UrlEncoded {
            fields: example
                .as_object()
                .map(|o| {
                    o.iter()
                        .map(|(k, v)| KeyValue::new(k, scalar_to_string(v)))
                        .collect()
                })
                .unwrap_or_default(),
        },
        "multipart/form-data" => RequestBody::Multipart {
            fields: example
                .as_object()
                .map(|o| {
                    o.iter()
                        .map(|(k, v)| relay_models::FormField {
                            key: k.clone(),
                            enabled: true,
                            value: relay_models::FormValue::Text {
                                value: scalar_to_string(v),
                            },
                            description: None,
                        })
                        .collect()
                })
                .unwrap_or_default(),
        },
        t if t.contains("xml") => {
            report.warn(
                where_from,
                "An XML body was declared; Relay imported an empty one because generating \
                 XML from a JSON Schema is guesswork.",
            );
            RequestBody::Xml {
                text: String::new(),
            }
        }
        other => RequestBody::Raw {
            text: String::new(),
            content_type: other.to_string(),
        },
    }
}

/// Follows a `$ref` into `components`, once.
///
/// Returns the value unchanged when it is not a reference or cannot be
/// resolved — a dangling `$ref` should degrade, not abort the import.
fn resolve_ref(root: &Value, value: &Value) -> Value {
    let Some(reference) = value.get("$ref").and_then(Value::as_str) else {
        return value.clone();
    };

    let Some(path) = reference.strip_prefix("#/") else {
        // External references would mean fetching a URL during an import,
        // which is not something an importer should do unasked.
        return value.clone();
    };

    let mut cursor = root;
    for segment in path.split('/') {
        let segment = segment.replace("~1", "/").replace("~0", "~");
        match cursor.get(&segment) {
            Some(next) => cursor = next,
            None => return value.clone(),
        }
    }
    cursor.clone()
}

/// Builds a plausible example value from a JSON Schema.
fn example_from_schema(root: &Value, schema: &Value, depth: usize) -> Value {
    if depth > MAX_SCHEMA_DEPTH {
        return Value::Null;
    }

    let schema = resolve_ref(root, schema);

    if let Some(example) = schema.get("example") {
        return example.clone();
    }
    if let Some(default) = schema.get("default") {
        return default.clone();
    }
    if let Some(first) = schema
        .get("enum")
        .and_then(Value::as_array)
        .and_then(|e| e.first())
    {
        return first.clone();
    }

    // A composed schema is merged shallowly: enough for an example, and
    // far less confusing than emitting nothing.
    for key in ["allOf", "oneOf", "anyOf"] {
        if let Some(variants) = schema.get(key).and_then(Value::as_array) {
            if key == "allOf" {
                let mut merged = serde_json::Map::new();
                for variant in variants {
                    if let Some(object) = example_from_schema(root, variant, depth + 1).as_object()
                    {
                        merged.extend(object.clone());
                    }
                }
                return Value::Object(merged);
            }
            if let Some(first) = variants.first() {
                return example_from_schema(root, first, depth + 1);
            }
        }
    }

    match schema.get("type").and_then(Value::as_str) {
        Some("object") | None if schema.get("properties").is_some() => {
            let mut object = serde_json::Map::new();
            if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
                for (name, property) in properties {
                    object.insert(name.clone(), example_from_schema(root, property, depth + 1));
                }
            }
            Value::Object(object)
        }
        Some("object") => Value::Object(serde_json::Map::new()),
        Some("array") => {
            let item = schema
                .get("items")
                .map(|i| example_from_schema(root, i, depth + 1))
                .unwrap_or(Value::Null);
            Value::Array(vec![item])
        }
        Some("integer") => Value::from(0),
        Some("number") => Value::from(0.0),
        Some("boolean") => Value::Bool(true),
        Some("string") => Value::String(string_example(&schema)),
        Some("null") => Value::Null,
        _ => Value::Null,
    }
}

/// A placeholder shaped like the format the schema asks for.
///
/// `"string"` in a date field produces a 400 and a puzzled developer;
/// a plausible value makes the imported request likely to work as sent.
fn string_example(schema: &Value) -> String {
    match schema.get("format").and_then(Value::as_str) {
        Some("date-time") => "2024-01-01T00:00:00Z".into(),
        Some("date") => "2024-01-01".into(),
        Some("time") => "00:00:00".into(),
        Some("email") => "user@example.com".into(),
        Some("uuid") => "00000000-0000-0000-0000-000000000000".into(),
        Some("uri") | Some("url") => "https://example.com".into(),
        Some("hostname") => "example.com".into(),
        Some("ipv4") => "192.0.2.1".into(),
        Some("ipv6") => "2001:db8::1".into(),
        Some("byte") => "aGVsbG8=".into(),
        Some("password") => "".into(),
        _ => "string".into(),
    }
}

fn parameter_example(root: &Value, parameter: &Value) -> String {
    if let Some(example) = parameter.get("example") {
        return scalar_to_string(example);
    }
    match parameter.get("schema") {
        Some(schema) => scalar_to_string(&example_from_schema(root, schema, 0)),
        None => String::new(),
    }
}

fn scalar_to_string(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// Reports the parts of a specification a request collection cannot hold.
fn warn_about_unsupported(root: &Value, title: &str, report: &mut ImportReport) {
    let mentions = |key: &str| root.get(key).is_some_and(|v| !v.is_null());

    if mentions("webhooks") {
        report.warn(
            title,
            "Webhooks were not imported: they describe calls the API makes to you, which \
             Relay has nothing to send.",
        );
    }

    let has_callbacks = root
        .get("paths")
        .and_then(Value::as_object)
        .is_some_and(|paths| {
            paths.values().any(|item| {
                item.as_object()
                    .is_some_and(|ops| ops.values().any(|op| op.get("callbacks").is_some()))
            })
        });
    if has_callbacks {
        report.warn(title, "Operation callbacks were not imported.");
    }

    report.warn(
        title,
        "Response schemas were not imported. Relay stores requests, not the shapes it \
         expects back — assertions over responses arrive with scripting.",
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn string_formats_produce_values_a_server_will_accept() {
        // "string" in a date field is a 400 and a puzzled developer.
        let date = serde_json::json!({ "type": "string", "format": "date-time" });
        assert_eq!(string_example(&date), "2024-01-01T00:00:00Z");

        let uuid = serde_json::json!({ "type": "string", "format": "uuid" });
        assert!(string_example(&uuid).contains('-'));

        let plain = serde_json::json!({ "type": "string" });
        assert_eq!(string_example(&plain), "string");
    }

    #[test]
    fn a_self_referential_schema_terminates() {
        let root = serde_json::json!({
            "components": { "schemas": { "Node": {
                "type": "object",
                "properties": { "child": { "$ref": "#/components/schemas/Node" } }
            }}}
        });
        let schema = serde_json::json!({ "$ref": "#/components/schemas/Node" });

        // Legal, and would expand forever without a depth limit.
        let example = example_from_schema(&root, &schema, 0);
        assert!(example.is_object());
    }

    #[test]
    fn a_dangling_ref_degrades_rather_than_aborting() {
        let root = serde_json::json!({ "components": {} });
        let value = serde_json::json!({ "$ref": "#/components/schemas/Missing" });
        assert_eq!(resolve_ref(&root, &value), value);
    }

    #[test]
    fn external_refs_are_not_fetched() {
        // Following one would mean an importer making network requests.
        let root = serde_json::json!({});
        let value = serde_json::json!({ "$ref": "https://example.com/schema.json" });
        assert_eq!(resolve_ref(&root, &value), value);
    }

    #[test]
    fn json_pointer_escapes_are_decoded() {
        let root = serde_json::json!({ "paths": { "/a/b": { "x": 1 } } });
        let value = serde_json::json!({ "$ref": "#/paths/~1a~1b" });
        assert_eq!(resolve_ref(&root, &value), serde_json::json!({ "x": 1 }));
    }
}
