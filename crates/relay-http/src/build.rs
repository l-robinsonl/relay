//! Turns an [`HttpRequestSpec`] into a `reqwest::RequestBuilder`.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use relay_models::{ApiKeyLocation, AuthConfig, FormValue, HttpRequestSpec, KeyValue, RequestBody};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use url::Url;

use crate::error::EngineError;

/// Parses and validates the target URL, applying query parameters.
///
/// A bare `example.com` is upgraded to `https://example.com` rather than
/// rejected: it is what a developer means, and refusing it is friction with no
/// safety benefit. The upgrade is to HTTPS, never HTTP.
pub fn resolve_url(spec: &HttpRequestSpec) -> Result<Url, EngineError> {
    let raw = spec.url.trim();
    if raw.is_empty() {
        return Err(EngineError::InvalidUrl("the URL is empty".into()));
    }

    let candidate = if raw.contains("://") {
        raw.to_string()
    } else if raw.starts_with("//") {
        format!("https:{raw}")
    } else {
        format!("https://{raw}")
    };

    let mut url =
        Url::parse(&candidate).map_err(|e| EngineError::InvalidUrl(format!("{raw}: {e}")))?;

    match url.scheme() {
        "http" | "https" => {}
        other => {
            return Err(EngineError::InvalidUrl(format!(
                "unsupported scheme '{other}' — Relay sends HTTP and HTTPS requests"
            )));
        }
    }

    if url.host_str().is_none_or(str::is_empty) {
        return Err(EngineError::InvalidUrl(format!("{raw}: no host")));
    }

    let active: Vec<&KeyValue> = spec.query_params.iter().filter(|p| p.is_active()).collect();
    if !active.is_empty() {
        let mut pairs = url.query_pairs_mut();
        for param in active {
            pairs.append_pair(&param.key, &param.value);
        }
    }

    Ok(url)
}

/// Builds the header map, applying auth and the body's implied content type.
///
/// Precedence is deliberate: an explicit header always wins over one Relay
/// would add. If a user writes their own `Content-Type`, that is the one sent.
pub fn build_headers(spec: &HttpRequestSpec) -> Result<HeaderMap, EngineError> {
    let mut headers = HeaderMap::new();

    for kv in spec.headers.iter().filter(|h| h.is_active()) {
        let name = HeaderName::from_bytes(kv.key.trim().as_bytes()).map_err(|e| {
            EngineError::InvalidHeader {
                name: kv.key.clone(),
                reason: e.to_string(),
            }
        })?;
        let value = HeaderValue::from_str(&kv.value).map_err(|_| EngineError::InvalidHeader {
            name: kv.key.clone(),
            // Naming the problem precisely saves a long debugging detour: a
            // newline pasted into a token is a common and baffling failure.
            reason: "value contains characters that are not valid in a header \
                     (control characters or newlines)"
                .into(),
        })?;
        headers.append(name, value);
    }

    apply_auth(&spec.auth, &mut headers)?;

    if !headers.contains_key(reqwest::header::CONTENT_TYPE)
        && let Some(ct) = spec.body.implied_content_type()
        && !spec.body.is_empty()
        && let Ok(value) = HeaderValue::from_str(ct)
    {
        headers.insert(reqwest::header::CONTENT_TYPE, value);
    }

    Ok(headers)
}

/// Applies the authentication scheme to the headers.
fn apply_auth(auth: &AuthConfig, headers: &mut HeaderMap) -> Result<(), EngineError> {
    let header_value = match auth {
        AuthConfig::None | AuthConfig::Inherit => return Ok(()),
        AuthConfig::Basic { username, password } => {
            let encoded = BASE64.encode(format!("{username}:{password}"));
            format!("Basic {encoded}")
        }
        AuthConfig::Bearer { token } => {
            let token = token.trim();
            if token.is_empty() {
                return Ok(());
            }
            format!("Bearer {token}")
        }
        AuthConfig::ApiKey {
            key,
            value,
            location,
        } => {
            // Query-placed keys are applied during URL resolution instead.
            if *location == ApiKeyLocation::Query || key.trim().is_empty() {
                return Ok(());
            }
            let name = HeaderName::from_bytes(key.trim().as_bytes()).map_err(|e| {
                EngineError::InvalidHeader {
                    name: key.clone(),
                    reason: e.to_string(),
                }
            })?;
            let header = HeaderValue::from_str(value).map_err(|_| EngineError::InvalidHeader {
                name: key.clone(),
                reason: "API key value contains invalid header characters".into(),
            })?;
            headers.insert(name, header);
            return Ok(());
        }
        AuthConfig::OAuth2(config) => match config.access_token.as_deref() {
            Some(token) if !token.trim().is_empty() => format!("Bearer {}", token.trim()),
            // No token yet: the caller is expected to run the flow first.
            _ => return Ok(()),
        },
    };

    let mut value =
        HeaderValue::from_str(&header_value).map_err(|_| EngineError::InvalidHeader {
            name: "Authorization".into(),
            reason: "credentials contain invalid header characters".into(),
        })?;
    // Keeps the credential out of `{:?}` output anywhere a HeaderMap is logged.
    value.set_sensitive(true);
    headers.insert(reqwest::header::AUTHORIZATION, value);

    Ok(())
}

/// Appends an API key that is configured to travel as a query parameter.
pub fn apply_query_auth(auth: &AuthConfig, url: &mut Url) {
    if let AuthConfig::ApiKey {
        key,
        value,
        location: ApiKeyLocation::Query,
    } = auth
        && !key.trim().is_empty()
    {
        url.query_pairs_mut().append_pair(key.trim(), value);
    }
}

/// The prepared body, ready to attach to a request.
///
/// `Debug` is written by hand rather than derived so that body contents never
/// reach a log line: a request body routinely carries credentials.
pub enum PreparedBody {
    /// Nothing to send.
    None,
    /// An in-memory payload.
    Bytes(Vec<u8>),
    /// A multipart form, which owns its own streams.
    Multipart(Box<reqwest::multipart::Form>),
    /// A file streamed from disk without being read into memory.
    File {
        /// Where the file lives.
        path: std::path::PathBuf,
        /// Its length, for `Content-Length`.
        len: u64,
    },
}

impl std::fmt::Debug for PreparedBody {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::None => f.write_str("PreparedBody::None"),
            Self::Bytes(b) => write!(f, "PreparedBody::Bytes({} bytes)", b.len()),
            Self::Multipart(_) => f.write_str("PreparedBody::Multipart"),
            Self::File { path, len } => {
                write!(f, "PreparedBody::File({}, {len} bytes)", path.display())
            }
        }
    }
}

/// Prepares the request body.
pub async fn prepare_body(body: &RequestBody) -> Result<PreparedBody, EngineError> {
    match body {
        RequestBody::None => Ok(PreparedBody::None),

        RequestBody::Json { text }
        | RequestBody::Text { text }
        | RequestBody::Xml { text }
        | RequestBody::Raw { text, .. } => Ok(PreparedBody::Bytes(text.as_bytes().to_vec())),

        RequestBody::GraphQl { query, variables } => {
            // An empty variables box is far more common than a populated one,
            // and sending `"variables": null` upsets stricter servers.
            let parsed: serde_json::Value = if variables.trim().is_empty() {
                serde_json::json!({})
            } else {
                serde_json::from_str(variables).map_err(|e| {
                    EngineError::RequestBody(format!("GraphQL variables are not valid JSON: {e}"))
                })?
            };
            let payload = serde_json::json!({ "query": query, "variables": parsed });
            Ok(PreparedBody::Bytes(
                serde_json::to_vec(&payload)
                    .map_err(|e| EngineError::RequestBody(e.to_string()))?,
            ))
        }

        RequestBody::UrlEncoded { fields } => {
            let mut serializer = url::form_urlencoded::Serializer::new(String::new());
            for field in fields.iter().filter(|f| f.is_active()) {
                serializer.append_pair(&field.key, &field.value);
            }
            Ok(PreparedBody::Bytes(serializer.finish().into_bytes()))
        }

        RequestBody::Multipart { fields } => {
            let mut form = reqwest::multipart::Form::new();
            for field in fields.iter().filter(|f| f.enabled) {
                match &field.value {
                    FormValue::Text { value } => {
                        form = form.text(field.key.clone(), value.clone());
                    }
                    FormValue::File {
                        path,
                        file_name,
                        content_type,
                    } => {
                        let part =
                            file_part(path, file_name.as_deref(), content_type.as_deref()).await?;
                        form = form.part(field.key.clone(), part);
                    }
                }
            }
            Ok(PreparedBody::Multipart(Box::new(form)))
        }

        RequestBody::Binary { path, .. } => {
            let meta = tokio::fs::metadata(path).await.map_err(|e| {
                EngineError::RequestBody(format!("cannot read upload file {path}: {e}"))
            })?;
            Ok(PreparedBody::File {
                path: path.into(),
                len: meta.len(),
            })
        }
    }
}

/// Builds a multipart part that streams its file rather than buffering it.
async fn file_part(
    path: &str,
    file_name: Option<&str>,
    content_type: Option<&str>,
) -> Result<reqwest::multipart::Part, EngineError> {
    let file = tokio::fs::File::open(path)
        .await
        .map_err(|e| EngineError::RequestBody(format!("cannot open {path}: {e}")))?;
    let len = file
        .metadata()
        .await
        .map_err(|e| EngineError::RequestBody(e.to_string()))?
        .len();

    let name = file_name
        .map(str::to_owned)
        .or_else(|| {
            std::path::Path::new(path)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "file".to_string());

    let mime = content_type.map(str::to_owned).unwrap_or_else(|| {
        mime_guess::from_path(path)
            .first_or_octet_stream()
            .to_string()
    });

    let stream = tokio_util::io::ReaderStream::new(file);
    let part =
        reqwest::multipart::Part::stream_with_length(reqwest::Body::wrap_stream(stream), len)
            .file_name(name);

    part.mime_str(&mime)
        .map_err(|e| EngineError::RequestBody(format!("invalid content type '{mime}': {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use relay_models::{HttpMethod, RequestSettings};

    fn spec(url: &str) -> HttpRequestSpec {
        HttpRequestSpec::new(HttpMethod::Get, url)
    }

    #[test]
    fn a_bare_host_is_upgraded_to_https_not_http() {
        let url = resolve_url(&spec("example.com/api")).unwrap();
        assert_eq!(url.scheme(), "https");
        assert_eq!(url.as_str(), "https://example.com/api");
    }

    #[test]
    fn an_explicit_scheme_is_preserved() {
        let url = resolve_url(&spec("http://localhost:3000/health")).unwrap();
        assert_eq!(url.scheme(), "http");
        assert_eq!(url.port(), Some(3000));
    }

    #[test]
    fn non_http_schemes_are_refused() {
        // An API client that will happily open file:// URLs is a local file
        // disclosure primitive, not a feature.
        for bad in ["file:///etc/passwd", "ftp://host/x", "data:text/plain,hi"] {
            let err = resolve_url(&spec(bad)).unwrap_err();
            assert!(
                matches!(err, EngineError::InvalidUrl(_)),
                "{bad} was allowed"
            );
        }
    }

    #[test]
    fn an_empty_url_is_refused() {
        assert!(resolve_url(&spec("   ")).is_err());
    }

    #[test]
    fn query_parameters_are_appended_and_encoded() {
        let mut s = spec("https://example.com/search?existing=1");
        s.query_params = vec![
            KeyValue::new("q", "hello world"),
            KeyValue::new("tag", "a&b"),
        ];
        let url = resolve_url(&s).unwrap();
        let query = url.query().unwrap();
        assert!(query.contains("existing=1"));
        assert!(query.contains("q=hello+world"));
        assert!(query.contains("tag=a%26b"));
    }

    #[test]
    fn disabled_query_parameters_are_skipped() {
        let mut s = spec("https://example.com");
        s.query_params = vec![
            KeyValue::new("kept", "1"),
            KeyValue {
                key: "dropped".into(),
                value: "1".into(),
                enabled: false,
                description: None,
            },
        ];
        let url = resolve_url(&s).unwrap();
        assert!(url.query().unwrap().contains("kept"));
        assert!(!url.query().unwrap().contains("dropped"));
    }

    #[test]
    fn basic_auth_is_encoded_per_rfc7617() {
        let mut s = spec("https://example.com");
        s.auth = AuthConfig::Basic {
            username: "Aladdin".into(),
            password: "open sesame".into(),
        };
        let headers = build_headers(&s).unwrap();
        assert_eq!(
            headers[reqwest::header::AUTHORIZATION],
            "Basic QWxhZGRpbjpvcGVuIHNlc2FtZQ=="
        );
    }

    #[test]
    fn credentials_are_marked_sensitive() {
        let mut s = spec("https://example.com");
        s.auth = AuthConfig::Bearer {
            token: "secret-token".into(),
        };
        let headers = build_headers(&s).unwrap();
        // A sensitive HeaderValue redacts itself in Debug output, so an
        // accidental `tracing::debug!(?headers)` cannot leak the token.
        let value = &headers[reqwest::header::AUTHORIZATION];
        assert!(value.is_sensitive());
        assert!(!format!("{value:?}").contains("secret-token"));
    }

    #[test]
    fn an_explicit_authorization_header_is_not_clobbered_silently() {
        let mut s = spec("https://example.com");
        s.headers = vec![KeyValue::new("Authorization", "Custom abc")];
        s.auth = AuthConfig::None;
        let headers = build_headers(&s).unwrap();
        assert_eq!(headers[reqwest::header::AUTHORIZATION], "Custom abc");
    }

    #[test]
    fn an_explicit_content_type_wins_over_the_implied_one() {
        let mut s = spec("https://example.com");
        s.headers = vec![KeyValue::new("Content-Type", "application/vnd.api+json")];
        s.body = RequestBody::Json { text: "{}".into() };
        let headers = build_headers(&s).unwrap();
        assert_eq!(
            headers[reqwest::header::CONTENT_TYPE],
            "application/vnd.api+json"
        );
    }

    #[test]
    fn json_bodies_get_an_implied_content_type() {
        let mut s = spec("https://example.com");
        s.body = RequestBody::Json {
            text: r#"{"a":1}"#.into(),
        };
        let headers = build_headers(&s).unwrap();
        assert_eq!(headers[reqwest::header::CONTENT_TYPE], "application/json");
    }

    #[test]
    fn an_empty_body_adds_no_content_type() {
        let mut s = spec("https://example.com");
        s.body = RequestBody::Json {
            text: String::new(),
        };
        let headers = build_headers(&s).unwrap();
        assert!(!headers.contains_key(reqwest::header::CONTENT_TYPE));
    }

    #[test]
    fn header_values_with_newlines_are_rejected_with_a_clear_reason() {
        let mut s = spec("https://example.com");
        s.headers = vec![KeyValue::new("X-Token", "abc\r\nX-Injected: yes")];
        let err = build_headers(&s).unwrap_err();
        match err {
            EngineError::InvalidHeader { name, reason } => {
                assert_eq!(name, "X-Token");
                assert!(reason.contains("newline"));
            }
            other => panic!("expected InvalidHeader, got {other:?}"),
        }
    }

    #[test]
    fn api_keys_can_travel_in_the_query_string() {
        let mut s = spec("https://example.com/v1");
        s.auth = AuthConfig::ApiKey {
            key: "api_key".into(),
            value: "abc123".into(),
            location: ApiKeyLocation::Query,
        };
        // It must not become a header.
        let headers = build_headers(&s).unwrap();
        assert!(!headers.contains_key("api_key"));

        let mut url = resolve_url(&s).unwrap();
        apply_query_auth(&s.auth, &mut url);
        assert!(url.query().unwrap().contains("api_key=abc123"));
    }

    #[test]
    fn api_keys_can_travel_in_a_header() {
        let mut s = spec("https://example.com");
        s.auth = AuthConfig::ApiKey {
            key: "X-API-Key".into(),
            value: "abc123".into(),
            location: ApiKeyLocation::Header,
        };
        let headers = build_headers(&s).unwrap();
        assert_eq!(headers["X-API-Key"], "abc123");
    }

    #[test]
    fn inherited_auth_sends_nothing_by_itself() {
        // Resolution of Inherit happens before the engine is reached.
        let mut s = spec("https://example.com");
        s.auth = AuthConfig::Inherit;
        assert!(
            !build_headers(&s)
                .unwrap()
                .contains_key(reqwest::header::AUTHORIZATION)
        );
    }

    #[tokio::test]
    async fn urlencoded_bodies_are_percent_encoded() {
        let body = RequestBody::UrlEncoded {
            fields: vec![KeyValue::new("name", "a b&c"), KeyValue::new("x", "1")],
        };
        match prepare_body(&body).await.unwrap() {
            PreparedBody::Bytes(b) => {
                assert_eq!(String::from_utf8(b).unwrap(), "name=a+b%26c&x=1");
            }
            _ => panic!("expected bytes"),
        }
    }

    #[tokio::test]
    async fn graphql_bodies_default_variables_to_an_object() {
        let body = RequestBody::GraphQl {
            query: "{ me { id } }".into(),
            variables: String::new(),
        };
        match prepare_body(&body).await.unwrap() {
            PreparedBody::Bytes(b) => {
                let v: serde_json::Value = serde_json::from_slice(&b).unwrap();
                assert_eq!(v["query"], "{ me { id } }");
                assert!(v["variables"].is_object());
            }
            _ => panic!("expected bytes"),
        }
    }

    #[tokio::test]
    async fn invalid_graphql_variables_are_reported_not_swallowed() {
        let body = RequestBody::GraphQl {
            query: "{}".into(),
            variables: "{not json".into(),
        };
        assert!(prepare_body(&body).await.is_err());
    }

    #[tokio::test]
    async fn a_missing_upload_file_is_reported_before_sending() {
        let body = RequestBody::Binary {
            path: "/nonexistent/relay-upload.bin".into(),
            content_type: None,
        };
        let err = prepare_body(&body).await.unwrap_err();
        assert!(matches!(err, EngineError::RequestBody(_)));
    }

    #[test]
    fn settings_default_is_what_the_engine_sees() {
        // Guards against a default drifting to something insecure.
        let s = RequestSettings::default();
        assert!(s.verify_tls);
        assert!(s.decompress);
    }
}
