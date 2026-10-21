//! Postman Collection v2.1 and environment import.
//!
//! Real exported collections are untidy: fields that the schema says are
//! arrays arrive as strings, `url` is sometimes an object and sometimes a
//! bare string, and collections written years apart disagree about almost
//! everything. So this parses against `serde_json::Value` rather than a
//! strict struct. A collection with one field Relay has never seen must
//! still import the other 199 requests, not fail outright.
//!
//! Postman uses the same `{{variable}}` syntax Relay does, so templates
//! survive import untouched.

use relay_models::{
    ApiKeyLocation, AuthConfig, Collection, CollectionId, Environment, FormField, FormValue,
    HttpMethod, KeyValue, RequestBody, RequestDoc, RequestScripts, Variable, WorkspaceId,
};
use serde_json::Value;

use crate::error::{CoreError, CoreResult};
use crate::import::ImportReport;

/// Parses the document, distinguishing malformed JSON from JSON that is
/// merely nested too deeply for `serde_json` to accept.
fn parse(json: &str) -> CoreResult<Value> {
    serde_json::from_str(json).map_err(|e| {
        let message = e.to_string();
        if message.contains("recursion limit exceeded") {
            CoreError::Import(
                "this file nests folders far too deeply to import safely. Flatten the \
                 deepest folders in Postman and export again."
                    .into(),
            )
        } else {
            CoreError::Import(format!("this is not valid JSON: {e}"))
        }
    })
}

/// Parses a Postman collection export.
pub fn import_collection(json: &str, workspace_id: WorkspaceId) -> CoreResult<ImportReport> {
    let root = parse(json)?;

    let info = root.get("info");
    let schema = info
        .and_then(|i| i.get("schema"))
        .and_then(Value::as_str)
        .unwrap_or_default();

    // `item` is the one field every version of the format has.
    let items = root.get("item").and_then(Value::as_array).ok_or_else(|| {
        CoreError::Import(
            "this does not look like a Postman collection — it has no \"item\" array".into(),
        )
    })?;

    let version = detect_version(schema);
    let mut report = ImportReport::new(format!("Postman Collection {version}"));

    if schema.contains("v1") {
        report.warn(
            "collection",
            "This is a v1 collection. Relay reads it on a best-effort basis; re-exporting \
             it from Postman as v2.1 will preserve more.",
        );
    }

    let name = info
        .and_then(|i| i.get("name"))
        .and_then(Value::as_str)
        .unwrap_or("Imported collection")
        .to_string();

    // The collection itself becomes a root folder, so an import never mixes
    // with whatever is already in the workspace.
    let mut root_collection = Collection::new(workspace_id.clone(), &name);
    root_collection.description = info
        .and_then(|i| i.get("description"))
        .and_then(description_text)
        .unwrap_or_default();

    if let Some(auth) = root.get("auth") {
        root_collection.auth = convert_auth(auth, &name, &mut report);
    }

    root_collection.variables = root
        .get("variable")
        .and_then(Value::as_array)
        .map(|values| convert_variables(values))
        .unwrap_or_default();

    let root_id = root_collection.id.clone();
    report.collections.push(root_collection);

    warn_about_scripts(root.get("event"), &name, &mut report, "collection");

    if root.get("protocolProfileBehavior").is_some() {
        report.warn(
            &name,
            "Protocol profile behaviour (such as disabling body stripping or following \
             original HTTP method) was not imported.",
        );
    }

    walk_items(items, Some(&root_id), &workspace_id, &name, &mut report, 0);

    Ok(report)
}

/// Parses a Postman environment export.
pub fn import_environment(json: &str, workspace_id: WorkspaceId) -> CoreResult<ImportReport> {
    let root = parse(json)?;

    let values = root
        .get("values")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            CoreError::Import(
                "this does not look like a Postman environment — it has no \"values\" array".into(),
            )
        })?;

    let name = root
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("Imported environment")
        .to_string();

    let mut report = ImportReport::new("Postman environment");
    let mut environment = Environment::new(workspace_id, &name);
    environment.variables = convert_variables(values);

    let secret_count = environment.variables.iter().filter(|v| v.secret).count();
    if secret_count > 0 {
        // Postman exports mark secrets but still write their values into the
        // file. Relay keeps the name and drops the value, which is the whole
        // point of marking something secret.
        report.warn(
            &name,
            format!(
                "{secret_count} variable{} marked secret. The name was imported but the \
                 value was not — re-enter {} in Relay so it is stored in your operating \
                 system's credential store rather than a file.",
                if secret_count == 1 { " was" } else { "s were" },
                if secret_count == 1 { "it" } else { "them" },
            ),
        );
    }

    report.environments.push(environment);
    Ok(report)
}

/// Reads either a collection or an environment, whichever it turns out to be.
pub fn import_any(json: &str, workspace_id: WorkspaceId) -> CoreResult<ImportReport> {
    let root = parse(json)?;

    if root.get("item").is_some() {
        import_collection(json, workspace_id)
    } else if root.get("values").is_some() {
        import_environment(json, workspace_id)
    } else {
        Err(CoreError::Import(
            "this JSON is neither a Postman collection (no \"item\") nor an environment \
             (no \"values\")"
                .into(),
        ))
    }
}

fn detect_version(schema: &str) -> &'static str {
    if schema.contains("v2.1") {
        "v2.1"
    } else if schema.contains("v2.0") {
        "v2.0"
    } else if schema.contains("v1") {
        "v1"
    } else {
        "(unknown version)"
    }
}

/// How deep a folder tree may go before the import gives up.
///
/// A cycle cannot occur in a JSON tree, but a pathological file could nest
/// thousands deep and blow the stack.
const MAX_DEPTH: usize = 32;

fn walk_items(
    items: &[Value],
    parent_id: Option<&CollectionId>,
    workspace_id: &WorkspaceId,
    path: &str,
    report: &mut ImportReport,
    depth: usize,
) {
    if depth > MAX_DEPTH {
        report.warn(
            path,
            format!("Folders nested more than {MAX_DEPTH} deep were not imported."),
        );
        return;
    }

    for (index, item) in items.iter().enumerate() {
        let name = item
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("Untitled")
            .to_string();
        let item_path = format!("{path} / {name}");

        match item.get("item").and_then(Value::as_array) {
            // A folder: it has children of its own.
            Some(children) => {
                let mut folder = Collection::new(workspace_id.clone(), &name);
                folder.parent_id = parent_id.cloned();
                folder.sort_order = index as f64;
                folder.description = item
                    .get("description")
                    .and_then(description_text)
                    .unwrap_or_default();

                if let Some(auth) = item.get("auth") {
                    folder.auth = convert_auth(auth, &item_path, report);
                }
                if let Some(variables) = item.get("variable").and_then(Value::as_array) {
                    folder.variables = convert_variables(variables);
                }

                warn_about_scripts(item.get("event"), &item_path, report, "folder");

                let folder_id = folder.id.clone();
                report.collections.push(folder);

                walk_items(
                    children,
                    Some(&folder_id),
                    workspace_id,
                    &item_path,
                    report,
                    depth + 1,
                );
            }
            // A request.
            None => {
                if let Some(request) = item.get("request") {
                    let doc = convert_request(
                        request,
                        item,
                        &name,
                        parent_id,
                        workspace_id,
                        index,
                        &item_path,
                        report,
                    );
                    report.requests.push(doc);
                } else {
                    report.warn(
                        &item_path,
                        "This item is neither a folder nor a request and was skipped.",
                    );
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn convert_request(
    request: &Value,
    item: &Value,
    name: &str,
    parent_id: Option<&CollectionId>,
    workspace_id: &WorkspaceId,
    index: usize,
    path: &str,
    report: &mut ImportReport,
) -> RequestDoc {
    let mut doc = RequestDoc::new(workspace_id.clone(), name);
    doc.collection_id = parent_id.cloned();
    doc.sort_order = index as f64;

    doc.method = request
        .get("method")
        .and_then(Value::as_str)
        .map(HttpMethod::from)
        .unwrap_or(HttpMethod::Get);

    doc.description = request
        .get("description")
        .and_then(description_text)
        .unwrap_or_default();

    let (url, query, path_variables) = convert_url(request.get("url"));
    doc.url = url;
    doc.query_params = query;
    doc.path_params = path_variables;

    doc.headers = request
        .get("header")
        .and_then(Value::as_array)
        .map(|headers| convert_key_values(headers))
        .unwrap_or_default();

    if let Some(auth) = request.get("auth") {
        doc.auth = convert_auth(auth, path, report);
    } else {
        // No auth block means "inherit from the folder", which is what
        // Postman does and what Relay's Inherit means.
        doc.auth = AuthConfig::Inherit;
    }

    doc.body = convert_body(request.get("body"), path, report);

    // Scripts are carried across so nothing is lost, but they do not run yet.
    doc.scripts = convert_scripts(item.get("event"));
    if !doc.scripts.is_empty() {
        report.warn(
            path,
            "Pre-request and test scripts were imported and saved, but Relay does not \
             execute scripts yet — they will not run when you send the request.",
        );
    }

    if item
        .get("response")
        .and_then(Value::as_array)
        .is_some_and(|r| !r.is_empty())
    {
        report.warn(
            path,
            "Saved example responses were not imported; Relay has no equivalent.",
        );
    }

    doc
}

/// Converts Postman's URL, which may be a string or a structured object.
///
/// Returns the base URL, its query parameters and its path variables, kept
/// apart so all three are editable rather than frozen into one string.
fn convert_url(url: Option<&Value>) -> (String, Vec<KeyValue>, Vec<KeyValue>) {
    let Some(url) = url else {
        return (String::new(), Vec::new(), Vec::new());
    };

    if let Some(raw) = url.as_str() {
        let (base, query) = split_query(raw);
        return (base, query, Vec::new());
    }

    // The structured form. `raw` is usually present and authoritative, but it
    // includes the query string, which is held separately here.
    let raw = url.get("raw").and_then(Value::as_str).unwrap_or_default();

    let base = if raw.is_empty() {
        build_url_from_parts(url)
    } else {
        raw.split_once('?')
            .map(|(b, _)| b)
            .unwrap_or(raw)
            .to_string()
    };

    let query = url
        .get("query")
        .and_then(Value::as_array)
        .map(|values| convert_key_values(values))
        .unwrap_or_else(|| split_query(raw).1);

    let variables = url
        .get("variable")
        .and_then(Value::as_array)
        .map(|values| convert_key_values(values))
        .unwrap_or_default();

    (base, query, variables)
}

/// Rebuilds a URL from Postman's exploded `protocol`/`host`/`path` fields.
fn build_url_from_parts(url: &Value) -> String {
    let protocol = url
        .get("protocol")
        .and_then(Value::as_str)
        .unwrap_or("https");

    let host = match url.get("host") {
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join("."),
        Some(Value::String(host)) => host.clone(),
        _ => String::new(),
    };

    let port = url
        .get("port")
        .and_then(|p| {
            p.as_str()
                .map(str::to_owned)
                .or_else(|| p.as_u64().map(|n| n.to_string()))
        })
        .map(|p| format!(":{p}"))
        .unwrap_or_default();

    let path = match url.get("path") {
        Some(Value::Array(parts)) => {
            let joined = parts
                .iter()
                .filter_map(|p| {
                    // A path segment may be a bare string or {"value": "..."}.
                    p.as_str()
                        .map(str::to_owned)
                        .or_else(|| p.get("value").and_then(Value::as_str).map(str::to_owned))
                })
                .collect::<Vec<_>>()
                .join("/");
            if joined.is_empty() {
                String::new()
            } else {
                format!("/{joined}")
            }
        }
        Some(Value::String(path)) => {
            if path.starts_with('/') {
                path.clone()
            } else {
                format!("/{path}")
            }
        }
        _ => String::new(),
    };

    if host.is_empty() {
        return path;
    }
    format!("{protocol}://{host}{port}{path}")
}

fn split_query(raw: &str) -> (String, Vec<KeyValue>) {
    match raw.split_once('?') {
        None => (raw.to_string(), Vec::new()),
        Some((base, query)) => {
            let params = query
                .split('&')
                .filter(|p| !p.is_empty())
                .map(|pair| {
                    let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
                    KeyValue::new(key, value)
                })
                .collect();
            (base.to_string(), params)
        }
    }
}

/// Converts Postman's `[{key, value, disabled}]` lists.
///
/// Note the inversion: Postman stores `disabled`, Relay stores `enabled`. A
/// missing `disabled` means the row is active.
fn convert_key_values(values: &[Value]) -> Vec<KeyValue> {
    values
        .iter()
        .filter_map(|entry| {
            let key = entry.get("key").and_then(Value::as_str)?;
            Some(KeyValue {
                key: key.to_string(),
                value: entry.get("value").and_then(scalar_text).unwrap_or_default(),
                enabled: !entry
                    .get("disabled")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                description: entry.get("description").and_then(description_text),
            })
        })
        .collect()
}

fn convert_variables(values: &[Value]) -> Vec<Variable> {
    values
        .iter()
        .filter_map(|entry| {
            let key = entry.get("key").and_then(Value::as_str)?;
            let secret = entry.get("type").and_then(Value::as_str) == Some("secret");
            Some(Variable {
                key: key.to_string(),
                // A secret's value is deliberately dropped. Postman writes it
                // into the export file in the clear; Relay will not copy it
                // into its database.
                value: if secret {
                    String::new()
                } else {
                    entry.get("value").and_then(scalar_text).unwrap_or_default()
                },
                enabled: entry
                    .get("enabled")
                    .and_then(Value::as_bool)
                    .unwrap_or_else(|| {
                        !entry
                            .get("disabled")
                            .and_then(Value::as_bool)
                            .unwrap_or(false)
                    }),
                secret,
                description: entry.get("description").and_then(description_text),
            })
        })
        .collect()
}

/// Converts Postman's auth block, whose parameters are an array of pairs.
fn convert_auth(auth: &Value, path: &str, report: &mut ImportReport) -> AuthConfig {
    let kind = auth.get("type").and_then(Value::as_str).unwrap_or("noauth");

    /// Reads one parameter out of the `[{key, value}]` array for a scheme.
    fn param(auth: &Value, scheme: &str, key: &str) -> String {
        auth.get(scheme)
            .and_then(Value::as_array)
            .and_then(|entries| {
                entries
                    .iter()
                    .find(|e| e.get("key").and_then(Value::as_str) == Some(key))
                    .and_then(|e| e.get("value").and_then(scalar_text))
            })
            // Newer exports sometimes use an object instead of an array.
            .or_else(|| {
                auth.get(scheme)
                    .and_then(|s| s.get(key))
                    .and_then(scalar_text)
            })
            .unwrap_or_default()
    }

    match kind {
        "noauth" => AuthConfig::None,
        "inherit" => AuthConfig::Inherit,
        "basic" => AuthConfig::Basic {
            username: param(auth, "basic", "username"),
            password: param(auth, "basic", "password"),
        },
        "bearer" => AuthConfig::Bearer {
            token: param(auth, "bearer", "token"),
        },
        "apikey" => {
            let location = if param(auth, "apikey", "in").eq_ignore_ascii_case("query") {
                ApiKeyLocation::Query
            } else {
                ApiKeyLocation::Header
            };
            AuthConfig::ApiKey {
                key: param(auth, "apikey", "key"),
                value: param(auth, "apikey", "value"),
                location,
            }
        }
        "oauth2" => {
            // The access token, if Postman saved one, is all that can be used
            // immediately; the flow configuration does not map cleanly.
            let token = param(auth, "oauth2", "accessToken");
            report.warn(
                path,
                "OAuth 2.0 was imported as a bearer token only. Relay does not run OAuth \
                 flows yet, so re-enter the grant configuration once it does.",
            );
            AuthConfig::Bearer { token }
        }
        other => {
            report.warn(
                path,
                format!(
                    "The '{other}' authentication scheme is not supported and was not \
                     imported. Set the equivalent header manually under Headers."
                ),
            );
            AuthConfig::None
        }
    }
}

fn convert_body(body: Option<&Value>, path: &str, report: &mut ImportReport) -> RequestBody {
    let Some(body) = body else {
        return RequestBody::None;
    };

    if body.get("disabled").and_then(Value::as_bool) == Some(true) {
        return RequestBody::None;
    }

    match body.get("mode").and_then(Value::as_str).unwrap_or("") {
        "raw" => {
            let text = body
                .get("raw")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            // Postman records the editor's language, which is the only clue
            // to what the body actually is.
            let language = body
                .get("options")
                .and_then(|o| o.get("raw"))
                .and_then(|r| r.get("language"))
                .and_then(Value::as_str)
                .unwrap_or("text");

            match language {
                "json" => RequestBody::Json { text },
                "xml" => RequestBody::Xml { text },
                "html" => RequestBody::Raw {
                    text,
                    content_type: "text/html".into(),
                },
                "javascript" => RequestBody::Raw {
                    text,
                    content_type: "application/javascript".into(),
                },
                _ => RequestBody::Text { text },
            }
        }

        "urlencoded" => RequestBody::UrlEncoded {
            fields: body
                .get("urlencoded")
                .and_then(Value::as_array)
                .map(|values| convert_key_values(values))
                .unwrap_or_default(),
        },

        "formdata" => RequestBody::Multipart {
            fields: body
                .get("formdata")
                .and_then(Value::as_array)
                .map(|values| convert_form_data(values, path, report))
                .unwrap_or_default(),
        },

        "file" => {
            let source = body
                .get("file")
                .and_then(|f| f.get("src"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            if source.is_empty() {
                report.warn(
                    path,
                    "A binary body was declared with no file path; choose the file again.",
                );
            }
            RequestBody::Binary {
                path: source,
                content_type: None,
            }
        }

        "graphql" => RequestBody::GraphQl {
            query: body
                .get("graphql")
                .and_then(|g| g.get("query"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            variables: body
                .get("graphql")
                .and_then(|g| g.get("variables"))
                .and_then(scalar_text)
                .unwrap_or_default(),
        },

        "" => RequestBody::None,

        other => {
            report.warn(path, format!("The '{other}' body mode was not imported."));
            RequestBody::None
        }
    }
}

fn convert_form_data(values: &[Value], path: &str, report: &mut ImportReport) -> Vec<FormField> {
    values
        .iter()
        .filter_map(|entry| {
            let key = entry.get("key").and_then(Value::as_str)?.to_string();
            let enabled = !entry
                .get("disabled")
                .and_then(Value::as_bool)
                .unwrap_or(false);

            let value = if entry.get("type").and_then(Value::as_str) == Some("file") {
                // `src` is a string for one file and an array for several;
                // Relay sends one file per field.
                let source = match entry.get("src") {
                    Some(Value::String(s)) => s.clone(),
                    Some(Value::Array(items)) => {
                        if items.len() > 1 {
                            report.warn(
                                path,
                                format!(
                                    "The form field '{key}' had {} files attached; Relay \
                                     kept the first.",
                                    items.len()
                                ),
                            );
                        }
                        items
                            .first()
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string()
                    }
                    _ => String::new(),
                };
                FormValue::File {
                    path: source,
                    file_name: None,
                    content_type: entry
                        .get("contentType")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                }
            } else {
                FormValue::Text {
                    value: entry.get("value").and_then(scalar_text).unwrap_or_default(),
                }
            };

            Some(FormField {
                key,
                enabled,
                value,
                description: entry.get("description").and_then(description_text),
            })
        })
        .collect()
}

/// Pulls pre-request and test scripts out of Postman's `event` array.
fn convert_scripts(events: Option<&Value>) -> RequestScripts {
    let mut scripts = RequestScripts::default();
    let Some(events) = events.and_then(Value::as_array) else {
        return scripts;
    };

    for event in events {
        let body = event
            .get("script")
            .and_then(|s| s.get("exec"))
            .map(|exec| match exec {
                // `exec` is usually an array of lines, occasionally one string.
                Value::Array(lines) => lines
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join("\n"),
                Value::String(text) => text.clone(),
                _ => String::new(),
            })
            .unwrap_or_default();

        if body.trim().is_empty() {
            continue;
        }

        match event.get("listen").and_then(Value::as_str) {
            Some("prerequest") => scripts.pre_request = body,
            Some("test") => scripts.post_response = body,
            _ => {}
        }
    }

    scripts
}

fn warn_about_scripts(events: Option<&Value>, path: &str, report: &mut ImportReport, level: &str) {
    if !convert_scripts(events).is_empty() {
        report.warn(
            path,
            format!(
                "Scripts attached to this {level} were not imported. Relay has no \
                 {level}-level scripts, and does not execute scripts yet."
            ),
        );
    }
}

/// Postman's `description` is a string in some versions and an object with a
/// `content` field in others.
fn description_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) if !text.is_empty() => Some(text.clone()),
        Value::Object(_) => value
            .get("content")
            .and_then(Value::as_str)
            .filter(|t| !t.is_empty())
            .map(str::to_owned),
        _ => None,
    }
}

/// Reads a field that should be a string but may be a number or a boolean.
fn scalar_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(flag) => Some(flag.to_string()),
        _ => None,
    }
}
