//! cURL import and export.
//!
//! Pasting a `curl` command is how most API documentation is shared, so this
//! is the import path that gets used most often. Export exists so a request
//! can leave Relay as easily as it arrived.

use relay_models::{
    AuthConfig, FormValue, HttpMethod, HttpRequestSpec, KeyValue, RequestBody, RequestDoc,
    WorkspaceId,
};

use crate::error::{CoreError, CoreResult};

/// Parses a `curl` command line into a request document.
pub fn import_curl(input: &str, workspace_id: WorkspaceId) -> CoreResult<RequestDoc> {
    let tokens = tokenise(input)?;
    if tokens.is_empty() {
        return Err(CoreError::Import("the command is empty".into()));
    }

    let mut iter = tokens.iter().peekable();

    // Skip a leading `curl`, which is usually but not always present.
    if iter.peek().is_some_and(|t| {
        let lowered = t.to_ascii_lowercase();
        lowered == "curl" || lowered.ends_with("/curl")
    }) {
        iter.next();
    }

    let mut url: Option<String> = None;
    let mut method: Option<HttpMethod> = None;
    let mut headers: Vec<KeyValue> = Vec::new();
    let mut data: Vec<String> = Vec::new();
    let mut form_fields: Vec<relay_models::FormField> = Vec::new();
    let mut auth: AuthConfig = AuthConfig::None;
    let mut insecure = false;
    let mut follow_redirects = false;
    let mut content_type: Option<String> = None;
    let mut unsupported: Vec<String> = Vec::new();
    let mut urlencoded = false;

    while let Some(token) = iter.next() {
        match token.as_str() {
            "-X" | "--request" => {
                let value = next_value(&mut iter, token)?;
                method = Some(HttpMethod::from(value.as_str()));
            }
            "-H" | "--header" => {
                let value = next_value(&mut iter, token)?;
                if let Some((name, raw)) = value.split_once(':') {
                    let name = name.trim();
                    let raw = raw.trim();
                    if name.eq_ignore_ascii_case("content-type") {
                        content_type = Some(raw.to_string());
                    }
                    headers.push(KeyValue::new(name, raw));
                }
            }
            "-d" | "--data" | "--data-raw" | "--data-binary" | "--data-ascii" => {
                let value = next_value(&mut iter, token)?;
                data.push(value);
            }
            "--data-urlencode" => {
                let value = next_value(&mut iter, token)?;
                urlencoded = true;
                data.push(value);
            }
            "-F" | "--form" => {
                let value = next_value(&mut iter, token)?;
                form_fields.push(parse_form_field(&value));
            }
            "-u" | "--user" => {
                let value = next_value(&mut iter, token)?;
                let (username, password) = value.split_once(':').unwrap_or((value.as_str(), ""));
                auth = AuthConfig::Basic {
                    username: username.to_string(),
                    password: password.to_string(),
                };
            }
            "-k" | "--insecure" => insecure = true,
            "-L" | "--location" => follow_redirects = true,
            "-I" | "--head" => method = Some(HttpMethod::Head),
            "-G" | "--get" => method = Some(HttpMethod::Get),
            "--url" => url = Some(next_value(&mut iter, token)?),
            // Flags that take a value we deliberately do not import.
            "-o" | "--output" | "-A" | "--user-agent" | "-e" | "--referer" | "-b" | "--cookie"
            | "--connect-timeout" | "--max-time" | "-x" | "--proxy" | "--cert" | "--key"
            | "--cacert" | "--retry" => {
                let value = next_value(&mut iter, token)?;
                match token.as_str() {
                    "-A" | "--user-agent" => headers.push(KeyValue::new("User-Agent", value)),
                    "-e" | "--referer" => headers.push(KeyValue::new("Referer", value)),
                    "-b" | "--cookie" => headers.push(KeyValue::new("Cookie", value)),
                    "-x" | "--proxy" => unsupported.push(format!("{token} {value}")),
                    _ => unsupported.push(token.clone()),
                }
            }
            // Valueless flags that change nothing we model.
            "-s" | "--silent" | "-v" | "--verbose" | "-S" | "--show-error" | "--compressed"
            | "-f" | "--fail" | "-#" | "--progress-bar" => {}
            other if other.starts_with('-') => unsupported.push(other.to_string()),
            other => {
                if url.is_none() {
                    url = Some(other.to_string());
                }
            }
        }
    }

    let url = url.ok_or_else(|| CoreError::Import("no URL found in the command".into()))?;

    // Split a query string off the URL so the params are editable.
    let (base_url, query_params) = split_query(&url);

    let body = build_body(&data, &form_fields, content_type.as_deref(), urlencoded);

    let method = method.unwrap_or(if body.is_empty() {
        HttpMethod::Get
    } else {
        // curl implies POST when given data and no explicit method.
        HttpMethod::Post
    });

    let mut doc = RequestDoc::new(workspace_id, derive_name(&base_url));
    doc.method = method;
    doc.url = base_url;
    doc.query_params = query_params;
    doc.headers = headers;
    doc.body = body;
    doc.auth = auth;
    doc.settings.verify_tls = !insecure;
    doc.settings.redirect_policy = if follow_redirects {
        relay_models::RedirectPolicy::Follow
    } else {
        // curl does not follow redirects unless asked; preserving that is
        // more faithful than quietly changing the request's behaviour.
        relay_models::RedirectPolicy::Never
    };

    if !unsupported.is_empty() {
        // Reported, not swallowed: the user needs to know what did not come
        // across rather than discovering it when the request behaves oddly.
        doc.description = format!(
            "Imported from cURL. These options were not imported: {}",
            unsupported.join(", ")
        );
    }

    Ok(doc)
}

fn build_body(
    data: &[String],
    form_fields: &[relay_models::FormField],
    content_type: Option<&str>,
    urlencoded_flag: bool,
) -> RequestBody {
    if !form_fields.is_empty() {
        return RequestBody::Multipart {
            fields: form_fields.to_vec(),
        };
    }
    if data.is_empty() {
        return RequestBody::None;
    }

    let joined = data.join("&");
    let ct = content_type.unwrap_or("").to_ascii_lowercase();

    if ct.contains("json") || looks_like_json(&joined) {
        RequestBody::Json { text: joined }
    } else if ct.contains("x-www-form-urlencoded") || urlencoded_flag {
        RequestBody::UrlEncoded {
            fields: joined
                .split('&')
                .filter(|p| !p.is_empty())
                .map(|pair| {
                    let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
                    KeyValue::new(k, v)
                })
                .collect(),
        }
    } else if ct.contains("xml") {
        RequestBody::Xml { text: joined }
    } else if ct.is_empty() {
        // curl's default for --data is form-urlencoded.
        RequestBody::UrlEncoded {
            fields: joined
                .split('&')
                .filter(|p| !p.is_empty())
                .map(|pair| {
                    let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
                    KeyValue::new(k, v)
                })
                .collect(),
        }
    } else {
        RequestBody::Raw {
            text: joined,
            content_type: ct,
        }
    }
}

fn looks_like_json(text: &str) -> bool {
    let trimmed = text.trim();
    (trimmed.starts_with('{') && trimmed.ends_with('}'))
        || (trimmed.starts_with('[') && trimmed.ends_with(']'))
}

fn parse_form_field(raw: &str) -> relay_models::FormField {
    let (key, value) = raw.split_once('=').unwrap_or((raw, ""));
    let value = if let Some(path) = value.strip_prefix('@') {
        FormValue::File {
            path: path.to_string(),
            file_name: None,
            content_type: None,
        }
    } else {
        FormValue::Text {
            value: value.to_string(),
        }
    };
    relay_models::FormField {
        key: key.to_string(),
        enabled: true,
        value,
        description: None,
    }
}

fn split_query(url: &str) -> (String, Vec<KeyValue>) {
    match url.split_once('?') {
        None => (url.to_string(), Vec::new()),
        Some((base, query)) => {
            let params = query
                .split('&')
                .filter(|p| !p.is_empty())
                .map(|pair| {
                    let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
                    KeyValue::new(decode_component(k), decode_component(v))
                })
                .collect();
            (base.to_string(), params)
        }
    }
}

fn decode_component(raw: &str) -> String {
    percent_encoding::percent_decode_str(&raw.replace('+', " "))
        .decode_utf8_lossy()
        .into_owned()
}

/// A readable default name, derived from the URL path.
fn derive_name(url: &str) -> String {
    let without_scheme = url.rsplit("://").next().unwrap_or(url);
    let path = without_scheme.split_once('/').map(|(_, p)| p).unwrap_or("");
    let last = path
        .trim_end_matches('/')
        .rsplit('/')
        .find(|s| !s.is_empty())
        .unwrap_or("");

    if last.is_empty() {
        without_scheme
            .split('/')
            .next()
            .unwrap_or("Imported request")
            .to_string()
    } else {
        last.to_string()
    }
}

/// Splits a shell command into tokens, honouring quotes, escapes and
/// line continuations.
fn tokenise(input: &str) -> CoreResult<Vec<String>> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut has_token = false;
    let mut chars = input.chars().peekable();

    #[derive(PartialEq)]
    enum Quote {
        None,
        Single,
        Double,
    }
    let mut quote = Quote::None;

    while let Some(ch) = chars.next() {
        match ch {
            '\\' if quote != Quote::Single => match chars.peek() {
                // A backslash-newline is a line continuation; both vanish.
                Some('\n') => {
                    chars.next();
                }
                Some('\r') => {
                    chars.next();
                    if chars.peek() == Some(&'\n') {
                        chars.next();
                    }
                }
                Some(&next) => {
                    chars.next();
                    current.push(next);
                    has_token = true;
                }
                None => {}
            },
            // Windows-style continuation, as pasted from PowerShell or CMD.
            '^' if quote == Quote::None && chars.peek() == Some(&'\n') => {
                chars.next();
            }
            '\'' if quote == Quote::None => {
                quote = Quote::Single;
                has_token = true;
            }
            '\'' if quote == Quote::Single => quote = Quote::None,
            '"' if quote == Quote::None => {
                quote = Quote::Double;
                has_token = true;
            }
            '"' if quote == Quote::Double => quote = Quote::None,
            c if c.is_whitespace() && quote == Quote::None => {
                if has_token {
                    tokens.push(std::mem::take(&mut current));
                    has_token = false;
                }
            }
            c => {
                current.push(c);
                has_token = true;
            }
        }
    }

    if quote != Quote::None {
        return Err(CoreError::Import(
            "the command has an unclosed quote".into(),
        ));
    }
    if has_token {
        tokens.push(current);
    }

    Ok(tokens)
}

fn next_value<'a>(
    iter: &mut std::iter::Peekable<impl Iterator<Item = &'a String>>,
    flag: &str,
) -> CoreResult<String> {
    iter.next()
        .cloned()
        .ok_or_else(|| CoreError::Import(format!("{flag} is missing its value")))
}

/// Renders a resolved request as a `curl` command.
///
/// Credentials are included because the point of "copy as cURL" is to run the
/// command; callers that are sharing it should export from the redacted spec.
pub fn export_curl(spec: &HttpRequestSpec) -> String {
    let mut parts: Vec<String> = vec!["curl".into()];

    if spec.method != HttpMethod::Get {
        parts.push(format!("-X {}", spec.method.as_str()));
    }

    let mut url = spec.url.clone();
    let active: Vec<&KeyValue> = spec.query_params.iter().filter(|p| p.is_active()).collect();
    if !active.is_empty() {
        let query: Vec<String> = active
            .iter()
            .map(|p| {
                format!(
                    "{}={}",
                    encode_component(&p.key),
                    encode_component(&p.value)
                )
            })
            .collect();
        let separator = if url.contains('?') { '&' } else { '?' };
        url = format!("{url}{separator}{}", query.join("&"));
    }
    parts.push(shell_quote(&url));

    for header in spec.headers.iter().filter(|h| h.is_active()) {
        parts.push(format!(
            "-H {}",
            shell_quote(&format!("{}: {}", header.key, header.value))
        ));
    }

    match &spec.auth {
        AuthConfig::Basic { username, password } => {
            parts.push(format!(
                "-u {}",
                shell_quote(&format!("{username}:{password}"))
            ));
        }
        AuthConfig::Bearer { token } if !token.is_empty() => {
            parts.push(format!(
                "-H {}",
                shell_quote(&format!("Authorization: Bearer {token}"))
            ));
        }
        AuthConfig::ApiKey {
            key,
            value,
            location: relay_models::ApiKeyLocation::Header,
        } if !key.is_empty() => {
            parts.push(format!("-H {}", shell_quote(&format!("{key}: {value}"))));
        }
        _ => {}
    }

    match &spec.body {
        RequestBody::None => {}
        RequestBody::Json { text }
        | RequestBody::Text { text }
        | RequestBody::Xml { text }
        | RequestBody::Raw { text, .. } => {
            if !text.is_empty() {
                parts.push(format!("-d {}", shell_quote(text)));
            }
        }
        RequestBody::UrlEncoded { fields } => {
            for field in fields.iter().filter(|f| f.is_active()) {
                parts.push(format!(
                    "-d {}",
                    shell_quote(&format!("{}={}", field.key, field.value))
                ));
            }
        }
        RequestBody::Multipart { fields } => {
            for field in fields.iter().filter(|f| f.enabled) {
                let rendered = match &field.value {
                    FormValue::Text { value } => format!("{}={}", field.key, value),
                    FormValue::File { path, .. } => format!("{}=@{}", field.key, path),
                };
                parts.push(format!("-F {}", shell_quote(&rendered)));
            }
        }
        RequestBody::Binary { path, .. } => {
            parts.push(format!(
                "--data-binary {}",
                shell_quote(&format!("@{path}"))
            ));
        }
        RequestBody::GraphQl { query, variables } => {
            let vars: serde_json::Value =
                serde_json::from_str(variables).unwrap_or_else(|_| serde_json::json!({}));
            let payload = serde_json::json!({ "query": query, "variables": vars });
            parts.push(format!("-d {}", shell_quote(&payload.to_string())));
        }
    }

    if matches!(
        spec.settings.redirect_policy,
        relay_models::RedirectPolicy::Follow
    ) {
        parts.push("-L".into());
    }
    if !spec.settings.verify_tls {
        parts.push("-k".into());
    }

    parts.join(" \\\n  ")
}

fn encode_component(value: &str) -> String {
    percent_encoding::utf8_percent_encode(value, percent_encoding::NON_ALPHANUMERIC).to_string()
}

/// Wraps a value in single quotes for POSIX shells.
fn shell_quote(value: &str) -> String {
    if !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._-:/@".contains(c))
    {
        return value.to_string();
    }
    // A single quote inside single quotes must be closed, escaped and
    // reopened — there is no escape for it within the quotes themselves.
    format!("'{}'", value.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ws() -> WorkspaceId {
        WorkspaceId::from("w1")
    }

    #[test]
    fn imports_a_simple_get() {
        let doc = import_curl("curl https://api.example.com/users", ws()).unwrap();
        assert_eq!(doc.method, HttpMethod::Get);
        assert_eq!(doc.url, "https://api.example.com/users");
        assert_eq!(doc.name, "users");
    }

    #[test]
    fn imports_without_the_leading_curl_word() {
        let doc = import_curl("https://api.example.com/health", ws()).unwrap();
        assert_eq!(doc.url, "https://api.example.com/health");
    }

    #[test]
    fn imports_headers() {
        let doc = import_curl(
            r#"curl https://x -H "Accept: application/json" -H 'X-Trace: abc'"#,
            ws(),
        )
        .unwrap();
        assert_eq!(doc.headers.len(), 2);
        assert_eq!(doc.headers[0].key, "Accept");
        assert_eq!(doc.headers[0].value, "application/json");
        assert_eq!(doc.headers[1].value, "abc");
    }

    #[test]
    fn data_without_a_method_implies_post() {
        let doc = import_curl(r#"curl https://x -d '{"a":1}'"#, ws()).unwrap();
        assert_eq!(doc.method, HttpMethod::Post);
    }

    #[test]
    fn json_bodies_are_detected_by_shape_and_by_content_type() {
        let by_shape = import_curl(r#"curl https://x -d '{"a":1}'"#, ws()).unwrap();
        assert_eq!(
            by_shape.body,
            RequestBody::Json {
                text: r#"{"a":1}"#.into()
            }
        );

        let by_header = import_curl(
            r#"curl https://x -H 'Content-Type: application/json' -d 'not-json-shaped'"#,
            ws(),
        )
        .unwrap();
        assert!(matches!(by_header.body, RequestBody::Json { .. }));
    }

    #[test]
    fn form_data_is_imported_as_urlencoded() {
        let doc = import_curl("curl https://x -d 'a=1&b=2'", ws()).unwrap();
        match doc.body {
            RequestBody::UrlEncoded { fields } => {
                assert_eq!(fields.len(), 2);
                assert_eq!(fields[0].key, "a");
                assert_eq!(fields[1].value, "2");
            }
            other => panic!("expected urlencoded, got {other:?}"),
        }
    }

    #[test]
    fn multipart_forms_and_file_uploads_are_imported() {
        let doc = import_curl("curl https://x -F name=Ada -F avatar=@/tmp/a.png", ws()).unwrap();
        match doc.body {
            RequestBody::Multipart { fields } => {
                assert_eq!(fields.len(), 2);
                assert_eq!(
                    fields[0].value,
                    FormValue::Text {
                        value: "Ada".into()
                    }
                );
                match &fields[1].value {
                    FormValue::File { path, .. } => assert_eq!(path, "/tmp/a.png"),
                    other => panic!("expected a file field, got {other:?}"),
                }
            }
            other => panic!("expected multipart, got {other:?}"),
        }
    }

    #[test]
    fn basic_auth_is_imported() {
        let doc = import_curl("curl https://x -u alice:hunter2", ws()).unwrap();
        assert_eq!(
            doc.auth,
            AuthConfig::Basic {
                username: "alice".into(),
                password: "hunter2".into()
            }
        );
    }

    #[test]
    fn an_explicit_method_is_respected() {
        let doc = import_curl("curl -X DELETE https://x/users/1", ws()).unwrap();
        assert_eq!(doc.method, HttpMethod::Delete);
    }

    #[test]
    fn query_strings_are_split_into_editable_params() {
        let doc = import_curl("curl 'https://x/search?q=hello%20world&page=2'", ws()).unwrap();
        assert_eq!(doc.url, "https://x/search");
        assert_eq!(doc.query_params.len(), 2);
        assert_eq!(doc.query_params[0].key, "q");
        assert_eq!(doc.query_params[0].value, "hello world");
        assert_eq!(doc.query_params[1].value, "2");
    }

    #[test]
    fn line_continuations_are_handled() {
        let command = "curl https://api.example.com/users \\\n  -H 'Accept: application/json' \\\n  -d '{\"a\":1}'";
        let doc = import_curl(command, ws()).unwrap();
        assert_eq!(doc.url, "https://api.example.com/users");
        assert_eq!(doc.headers.len(), 1);
        assert_eq!(doc.method, HttpMethod::Post);
    }

    #[test]
    fn nested_quotes_survive_tokenising() {
        let doc = import_curl(r#"curl https://x -d '{"msg":"it'\''s fine"}'"#, ws()).unwrap();
        match doc.body {
            RequestBody::Json { text } => assert!(text.contains("it's fine"), "got {text}"),
            other => panic!("expected json, got {other:?}"),
        }
    }

    #[test]
    fn insecure_and_location_flags_map_to_settings() {
        let doc = import_curl("curl -k -L https://x", ws()).unwrap();
        assert!(!doc.settings.verify_tls);
        assert_eq!(
            doc.settings.redirect_policy,
            relay_models::RedirectPolicy::Follow
        );
    }

    #[test]
    fn redirects_default_to_off_matching_curl() {
        let doc = import_curl("curl https://x", ws()).unwrap();
        assert_eq!(
            doc.settings.redirect_policy,
            relay_models::RedirectPolicy::Never,
            "curl does not follow redirects without -L"
        );
    }

    #[test]
    fn unsupported_options_are_reported_rather_than_silently_dropped() {
        let doc = import_curl("curl https://x --proxy http://proxy:8080", ws()).unwrap();
        assert!(
            doc.description.contains("--proxy"),
            "the user was not told: {}",
            doc.description
        );
    }

    #[test]
    fn an_unclosed_quote_is_an_error_not_a_mangled_request() {
        assert!(import_curl("curl 'https://x", ws()).is_err());
    }

    #[test]
    fn a_command_with_no_url_is_an_error() {
        assert!(import_curl("curl -X POST", ws()).is_err());
    }

    #[test]
    fn exports_a_simple_get() {
        let spec = HttpRequestSpec::new(HttpMethod::Get, "https://api.example.com/users");
        let exported = export_curl(&spec);
        assert!(exported.starts_with("curl"));
        assert!(exported.contains("https://api.example.com/users"));
        assert!(!exported.contains("-X GET"), "GET is the default");
    }

    #[test]
    fn exports_method_headers_and_body() {
        let mut spec = HttpRequestSpec::new(HttpMethod::Post, "https://x/users");
        spec.headers = vec![KeyValue::new("Content-Type", "application/json")];
        spec.body = RequestBody::Json {
            text: r#"{"name":"Ada"}"#.into(),
        };
        let exported = export_curl(&spec);

        assert!(exported.contains("-X POST"));
        assert!(exported.contains("-H 'Content-Type: application/json'"));
        assert!(exported.contains(r#"-d '{"name":"Ada"}'"#));
    }

    #[test]
    fn export_quotes_values_containing_single_quotes_safely() {
        let mut spec = HttpRequestSpec::new(HttpMethod::Post, "https://x");
        spec.body = RequestBody::Text {
            text: "it's".into(),
        };
        let exported = export_curl(&spec);
        assert!(exported.contains(r"'it'\''s'"), "got {exported}");
    }

    #[test]
    fn a_round_trip_preserves_the_essentials() {
        let original = r#"curl -X POST 'https://api.example.com/users' -H 'Content-Type: application/json' -H 'Accept: application/json' -d '{"name":"Ada"}'"#;
        let doc = import_curl(original, ws()).unwrap();

        let spec = HttpRequestSpec {
            method: doc.method.clone(),
            url: doc.url.clone(),
            headers: doc.headers.clone(),
            query_params: doc.query_params.clone(),
            body: doc.body.clone(),
            auth: doc.auth.clone(),
            settings: doc.settings.clone(),
        };
        let exported = export_curl(&spec);
        let reimported = import_curl(&exported, ws()).unwrap();

        assert_eq!(reimported.method, doc.method);
        assert_eq!(reimported.url, doc.url);
        assert_eq!(reimported.body, doc.body);
        assert_eq!(reimported.headers.len(), doc.headers.len());
    }

    #[test]
    fn query_parameters_survive_a_round_trip() {
        let doc = import_curl("curl 'https://x/s?q=a+b&n=1'", ws()).unwrap();
        let spec = HttpRequestSpec {
            method: doc.method.clone(),
            url: doc.url.clone(),
            headers: vec![],
            query_params: doc.query_params.clone(),
            body: RequestBody::None,
            auth: AuthConfig::None,
            settings: Default::default(),
        };
        let reimported = import_curl(&export_curl(&spec), ws()).unwrap();
        assert_eq!(reimported.query_params.len(), 2);
        assert_eq!(reimported.query_params[0].value, "a b");
    }
}
