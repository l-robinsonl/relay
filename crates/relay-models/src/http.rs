//! The wire-level contract between the frontend, the HTTP engine and history.
//!
//! These types are deliberately separate from [`crate::request::RequestDoc`].
//! A saved request is an editable document full of `{{variables}}`, disabled
//! rows and inherited settings; an [`HttpRequestSpec`] is the fully resolved
//! result of all of that, with nothing left to interpret. Keeping them apart
//! means the engine can never accidentally send a template, and the resolver
//! has exactly one output shape to test.

use crate::common::{KeyValue, Timestamp};
use crate::ids::{ExecutionId, RequestId, WorkspaceId};
use crate::request::{AuthConfig, HttpMethod, RequestBody, RequestSettings};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// A request with every variable already substituted, ready to send.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct HttpRequestSpec {
    pub method: HttpMethod,
    /// Absolute URL. Query parameters in `query_params` are appended to it.
    pub url: String,
    #[serde(default)]
    pub headers: Vec<KeyValue>,
    #[serde(default)]
    pub query_params: Vec<KeyValue>,
    #[serde(default)]
    pub body: RequestBody,
    #[serde(default)]
    pub auth: AuthConfig,
    #[serde(default)]
    pub settings: RequestSettings,
}

impl HttpRequestSpec {
    /// A bare spec for the given method and URL.
    pub fn new(method: HttpMethod, url: impl Into<String>) -> Self {
        Self {
            method,
            url: url.into(),
            headers: Vec::new(),
            query_params: Vec::new(),
            body: RequestBody::None,
            auth: AuthConfig::None,
            settings: RequestSettings::default(),
        }
    }

    /// A copy with credentials removed, safe for logs, history and exports.
    ///
    /// History keeps a snapshot of what was sent so a request can be replayed
    /// and compared, but a bearer token written to the local database — and
    /// still there six months later — is a liability, not a feature.
    pub fn redacted(&self) -> Self {
        const SENSITIVE: [&str; 6] = [
            "authorization",
            "proxy-authorization",
            "cookie",
            "set-cookie",
            "x-api-key",
            "x-auth-token",
        ];

        let headers = self
            .headers
            .iter()
            .map(|h| {
                if SENSITIVE.contains(&h.key.to_ascii_lowercase().as_str()) {
                    KeyValue {
                        value: REDACTED.to_string(),
                        ..h.clone()
                    }
                } else {
                    h.clone()
                }
            })
            .collect();

        let auth = match &self.auth {
            AuthConfig::Basic { username, .. } => AuthConfig::Basic {
                username: username.clone(),
                password: REDACTED.to_string(),
            },
            AuthConfig::Bearer { .. } => AuthConfig::Bearer {
                token: REDACTED.to_string(),
            },
            AuthConfig::ApiKey { key, location, .. } => AuthConfig::ApiKey {
                key: key.clone(),
                value: REDACTED.to_string(),
                location: *location,
            },
            AuthConfig::OAuth2(_) => AuthConfig::Bearer {
                token: REDACTED.to_string(),
            },
            other => other.clone(),
        };

        let mut settings = self.settings.clone();
        if let Some(proxy) = settings.proxy.as_mut()
            && proxy.password.is_some()
        {
            proxy.password = Some(REDACTED.to_string());
        }

        Self {
            headers,
            auth,
            settings,
            ..self.clone()
        }
    }
}

/// The placeholder substituted for any redacted secret.
pub const REDACTED: &str = "[redacted]";

/// Where a response body ended up.
///
/// Small bodies travel inline to the webview. Large ones are written to a
/// temporary file and referenced, so that downloading a 2 GB artefact costs
/// disk rather than resident memory in both the Rust process and the webview.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "camelCase")]
#[ts(export)]
pub enum ResponseBody {
    /// No body was returned.
    Empty,
    /// Decoded text, held in memory.
    #[serde(rename_all = "camelCase")]
    Text { text: String },
    /// Binary content, base64-encoded for transport to the webview.
    #[serde(rename_all = "camelCase")]
    Binary { base64: String },
    /// Spilled to disk because it exceeded the in-memory limit.
    #[serde(rename_all = "camelCase")]
    File {
        path: String,
        #[ts(type = "number")]
        size: u64,
        /// The leading bytes, so the UI can still show a preview.
        preview: String,
    },
    /// Discarded because it exceeded the hard cap.
    #[serde(rename_all = "camelCase")]
    Truncated {
        #[ts(type = "number")]
        size: u64,
        reason: String,
    },
}

impl ResponseBody {
    /// The text content, when the body is text.
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Self::Text { text } => Some(text),
            Self::File { preview, .. } => Some(preview),
            _ => None,
        }
    }
}

/// Timing breakdown of a single exchange, in milliseconds.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct Timings {
    /// Wall-clock time from send to the body being fully read.
    pub total_ms: f64,
    /// Time until response headers arrived.
    pub time_to_first_byte_ms: f64,
    /// Time spent reading the body after headers.
    pub download_ms: f64,
}

/// One redirect that was followed on the way to the final response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct RedirectHop {
    pub status: u16,
    pub url: String,
    pub location: String,
}

/// A response as returned to the frontend.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct HttpResponseData {
    pub status: u16,
    pub status_text: String,
    pub http_version: String,
    pub headers: Vec<KeyValue>,
    pub cookies: Vec<ResponseCookie>,
    pub body: ResponseBody,
    /// Bytes received on the wire, before decompression.
    #[ts(type = "number")]
    pub size_bytes: u64,
    /// Body size after decompression, when it differs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(type = "number | null")]
    pub decoded_size_bytes: Option<u64>,
    pub timings: Timings,
    #[serde(default)]
    pub redirects: Vec<RedirectHop>,
    /// The URL that actually produced this response, after redirects.
    pub final_url: String,
    /// Content type as reported, parsed down to the essence, e.g. `application/json`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    /// True when the body was decoded as text using a detected charset.
    #[serde(default)]
    pub is_text: bool,
}

/// A cookie set by a response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ResponseCookie {
    pub name: String,
    pub value: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires: Option<Timestamp>,
    #[serde(default)]
    pub http_only: bool,
    #[serde(default)]
    pub secure: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub same_site: Option<String>,
}

/// The outcome of one execution: the response, or why there wasn't one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "outcome", rename_all = "camelCase")]
#[ts(export)]
pub enum HttpExchange {
    #[serde(rename_all = "camelCase")]
    Success {
        execution_id: ExecutionId,
        request: Box<HttpRequestSpec>,
        response: Box<HttpResponseData>,
    },
    #[serde(rename_all = "camelCase")]
    Failure {
        execution_id: ExecutionId,
        request: Box<HttpRequestSpec>,
        error: TransportError,
        elapsed_ms: f64,
    },
    #[serde(rename_all = "camelCase")]
    Cancelled {
        execution_id: ExecutionId,
        elapsed_ms: f64,
    },
}

/// Why a request did not produce a response.
///
/// A structured kind rather than a formatted string, so the UI can offer the
/// right remedy — "check the hostname" versus "this certificate is untrusted,
/// here is how to proceed if you expected that".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct TransportError {
    pub kind: TransportErrorKind,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// A coarse classification of transport failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum TransportErrorKind {
    /// The URL could not be parsed or was not absolute.
    InvalidUrl,
    /// DNS resolution failed.
    DnsFailure,
    /// TCP connection failed or was refused.
    ConnectionFailed,
    /// The TLS handshake failed, typically an untrusted certificate.
    TlsError,
    /// The request exceeded its timeout.
    Timeout,
    /// Too many redirects, per the request's policy.
    TooManyRedirects,
    /// The proxy could not be reached or rejected the connection.
    ProxyError,
    /// The body could not be prepared, e.g. an unreadable upload file.
    RequestBodyError,
    /// The response ended early or could not be read.
    ResponseBodyError,
    /// The response exceeded the configured size limit.
    ResponseTooLarge,
    /// Anything that does not fit the above.
    Other,
}

/// Progress emitted while a request is in flight.
///
/// The webview stays responsive because it is never blocked on the exchange;
/// it receives these events and renders a live byte count.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ExecutionProgress {
    pub execution_id: ExecutionId,
    pub phase: ExecutionPhase,
    #[serde(default)]
    #[ts(type = "number")]
    pub bytes_read: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(type = "number | null")]
    pub total_bytes: Option<u64>,
}

/// Where an in-flight request has got to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum ExecutionPhase {
    Preparing,
    Connecting,
    Sending,
    WaitingForResponse,
    Downloading,
    Done,
}

/// A local record of one executed request. Never synchronised.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct HistoryEntry {
    pub id: crate::ids::HistoryId,
    pub workspace_id: WorkspaceId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<RequestId>,
    pub name: String,
    pub method: HttpMethod,
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_text: Option<String>,
    pub duration_ms: f64,
    #[ts(type = "number")]
    pub response_size: u64,
    pub executed_at: Timestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// The redacted spec that was sent, for replay and comparison.
    pub request_snapshot: Box<HttpRequestSpec>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::request::{ApiKeyLocation, ProxyConfig};

    fn patched(patch: RequestPatch) -> HttpRequestSpec {
        let mut spec = HttpRequestSpec::new(HttpMethod::Get, "https://api.example.com/users");
        spec.headers = vec![
            KeyValue::new("Content-Type", "application/json"),
            KeyValue::new("Accept", "application/json"),
        ];
        spec.query_params = vec![KeyValue::new("page", "1")];
        spec.apply_patch(&patch);
        spec
    }

    fn header<'a>(spec: &'a HttpRequestSpec, name: &str) -> Option<&'a KeyValue> {
        spec.headers
            .iter()
            .find(|h| h.key.eq_ignore_ascii_case(name))
    }

    #[test]
    fn a_patch_adds_a_header_without_disturbing_the_others() {
        let spec = patched(RequestPatch {
            headers: [("X-Signature".to_string(), "abc".to_string())].into(),
            ..Default::default()
        });

        assert_eq!(spec.headers.len(), 3);
        assert_eq!(header(&spec, "X-Signature").unwrap().value, "abc");
        assert_eq!(header(&spec, "Accept").unwrap().value, "application/json");
    }

    #[test]
    fn a_header_is_matched_without_regard_to_case() {
        // HTTP header names are case-insensitive; a script that writes
        // `content-type` means the `Content-Type` already on the request,
        // and sending both would be wrong.
        let spec = patched(RequestPatch {
            headers: [("content-type".to_string(), "text/plain".to_string())].into(),
            ..Default::default()
        });

        assert_eq!(spec.headers.len(), 2, "a duplicate header was added");
        let row = header(&spec, "Content-Type").unwrap();
        assert_eq!(row.value, "text/plain");
        assert_eq!(row.key, "Content-Type", "the user's spelling was not kept");
    }

    #[test]
    fn a_removed_header_goes_whatever_its_case() {
        let spec = patched(RequestPatch {
            removed_headers: vec!["ACCEPT".into()],
            ..Default::default()
        });

        assert!(header(&spec, "Accept").is_none());
        assert_eq!(spec.headers.len(), 1);
    }

    #[test]
    fn a_disabled_row_the_script_sets_is_turned_back_on() {
        let mut spec = HttpRequestSpec::new(HttpMethod::Get, "https://api.example.com");
        spec.headers = vec![KeyValue {
            enabled: false,
            ..KeyValue::new("X-Trace", "off")
        }];

        spec.apply_patch(&RequestPatch {
            headers: [("X-Trace".to_string(), "on".to_string())].into(),
            ..Default::default()
        });

        assert!(
            spec.headers[0].enabled,
            "the script set it, so it should be sent"
        );
        assert_eq!(spec.headers[0].value, "on");
    }

    #[test]
    fn query_parameter_names_are_case_sensitive() {
        // Unlike headers: `?Page=2` and `?page=2` are different
        // parameters, and merging them would silently change the request.
        let spec = patched(RequestPatch {
            query: [("Page".to_string(), "2".to_string())].into(),
            ..Default::default()
        });

        assert_eq!(spec.query_params.len(), 2);
    }

    #[test]
    fn the_method_url_and_body_are_replaced() {
        let spec = patched(RequestPatch {
            method: Some("POST".into()),
            url: Some("https://api.example.com/v2/users".into()),
            body: Some("hello".into()),
            ..Default::default()
        });

        assert_eq!(spec.method, HttpMethod::Post);
        assert_eq!(spec.url, "https://api.example.com/v2/users");
        assert!(matches!(spec.body, RequestBody::Text { ref text } if text == "hello"));
    }

    #[test]
    fn an_empty_patch_changes_nothing() {
        let before = patched(RequestPatch::default());
        assert_eq!(before.headers.len(), 2);
        assert_eq!(before.query_params.len(), 1);
        assert_eq!(before.method, HttpMethod::Get);
        assert!(RequestPatch::default().is_empty());
    }

    fn spec_with_secrets() -> HttpRequestSpec {
        let mut spec = HttpRequestSpec::new(HttpMethod::Get, "https://api.example.com");
        spec.headers = vec![
            KeyValue::new("Authorization", "Bearer sk-live-SECRET"),
            KeyValue::new("X-API-Key", "key-SECRET"),
            KeyValue::new("Cookie", "session=SECRET"),
            KeyValue::new("Accept", "application/json"),
        ];
        spec.auth = AuthConfig::Basic {
            username: "alice".into(),
            password: "hunter2".into(),
        };
        spec.settings.proxy = Some(ProxyConfig {
            url: "http://proxy:8080".into(),
            username: Some("puser".into()),
            password: Some("ppass-SECRET".into()),
            bypass: vec![],
        });
        spec
    }

    #[test]
    fn redaction_removes_every_secret_from_the_payload() {
        let redacted = spec_with_secrets().redacted();
        let json = serde_json::to_string(&redacted).unwrap();

        for secret in ["sk-live-SECRET", "key-SECRET", "hunter2", "ppass-SECRET"] {
            assert!(!json.contains(secret), "leaked {secret} in {json}");
        }
        // Non-secret content survives so history stays useful.
        assert!(json.contains("application/json"));
        assert!(json.contains("alice"));
        assert!(json.contains("puser"));
    }

    #[test]
    fn redaction_is_case_insensitive_for_header_names() {
        let mut spec = HttpRequestSpec::new(HttpMethod::Get, "https://x");
        spec.headers = vec![KeyValue::new("AUTHORIZATION", "Bearer SECRET")];
        let json = serde_json::to_string(&spec.redacted()).unwrap();
        assert!(!json.contains("SECRET"));
    }

    #[test]
    fn redaction_collapses_oauth2_to_an_opaque_bearer() {
        let mut spec = HttpRequestSpec::new(HttpMethod::Get, "https://x");
        spec.auth = AuthConfig::OAuth2(Box::new(crate::request::OAuth2Config {
            client_secret: "cs-SECRET".into(),
            access_token: Some("at-SECRET".into()),
            refresh_token: Some("rt-SECRET".into()),
            ..Default::default()
        }));
        let json = serde_json::to_string(&spec.redacted()).unwrap();
        assert!(!json.contains("SECRET"), "leaked in {json}");
    }

    #[test]
    fn api_key_redaction_keeps_the_key_name() {
        let mut spec = HttpRequestSpec::new(HttpMethod::Get, "https://x");
        spec.auth = AuthConfig::ApiKey {
            key: "X-Token".into(),
            value: "SECRET".into(),
            location: ApiKeyLocation::Header,
        };
        let json = serde_json::to_string(&spec.redacted()).unwrap();
        assert!(json.contains("X-Token"));
        assert!(!json.contains("SECRET"));
    }

    #[test]
    fn exchange_is_tagged_by_outcome() {
        let ex = HttpExchange::Cancelled {
            execution_id: ExecutionId::from("e1"),
            elapsed_ms: 12.0,
        };
        let json = serde_json::to_value(&ex).unwrap();
        assert_eq!(json["outcome"], "cancelled");
    }
}

/// What a pre-request script asked to change about the request.
///
/// A patch rather than a mutable handle: the script states intent, and
/// Rust decides what that means and what is allowed. It is applied after
/// variables are resolved, so the values are used exactly as written —
/// a script cannot smuggle `{{apiSecret}}` into a header and have
/// resolution fetch a credential it may not read.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct RequestPatch {
    /// Replaces the method.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    /// Replaces the URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Replaces the body with this text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    /// Headers to add or overwrite, keyed by name.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub headers: std::collections::BTreeMap<String, String>,
    /// Header names to drop.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub removed_headers: Vec<String>,
    /// Query parameters to add or overwrite.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub query: std::collections::BTreeMap<String, String>,
    /// Query parameter names to drop.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub removed_query: Vec<String>,
}

impl RequestPatch {
    /// True when the script asked for no change at all.
    pub fn is_empty(&self) -> bool {
        self.method.is_none()
            && self.url.is_none()
            && self.body.is_none()
            && self.headers.is_empty()
            && self.removed_headers.is_empty()
            && self.query.is_empty()
            && self.removed_query.is_empty()
    }
}

impl HttpRequestSpec {
    /// Applies what a pre-request script asked for.
    ///
    /// Header names match case-insensitively, because HTTP header names
    /// are case-insensitive and a script that writes `content-type`
    /// means the `Content-Type` already on the request.
    pub fn apply_patch(&mut self, patch: &RequestPatch) {
        if let Some(method) = &patch.method {
            self.method = HttpMethod::from(method.as_str());
        }
        if let Some(url) = &patch.url {
            self.url = url.clone();
        }
        if let Some(body) = &patch.body {
            self.body = RequestBody::Text { text: body.clone() };
        }

        apply_rows(
            &mut self.headers,
            &patch.headers,
            &patch.removed_headers,
            true,
        );
        // Query parameter names are case-sensitive, unlike header names.
        apply_rows(
            &mut self.query_params,
            &patch.query,
            &patch.removed_query,
            false,
        );
    }
}

/// Merges a script's additions and removals into a key/value list.
fn apply_rows(
    rows: &mut Vec<KeyValue>,
    set: &std::collections::BTreeMap<String, String>,
    removed: &[String],
    ignore_case: bool,
) {
    let matches = |a: &str, b: &str| {
        if ignore_case {
            a.eq_ignore_ascii_case(b)
        } else {
            a == b
        }
    };

    for name in removed {
        rows.retain(|row| !matches(&row.key, name));
    }

    for (name, value) in set {
        // An existing row is overwritten in place, keeping its position
        // and the spelling the user chose, so the request the script
        // produced still looks like the one they wrote.
        if let Some(existing) = rows.iter_mut().find(|row| matches(&row.key, name)) {
            existing.value = value.clone();
            existing.enabled = true;
        } else {
            rows.push(KeyValue::new(name, value));
        }
    }
}

/// One event from the stream.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ServerEvent {
    /// The `event:` name. Servers often omit it, and the default is
    /// `message`.
    pub event: String,
    /// The `data:` payload, with multiple data lines joined by
    /// newlines as the specification requires.
    pub data: String,
    /// The `id:`, when the server sent one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// A `retry:` hint, in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(type = "number | null")]
    pub retry_ms: Option<u64>,
}

/// Which way a message travelled.
///
/// Server-sent events are only ever inbound; a websocket is a
/// conversation, and a log that did not say who spoke would be
/// unreadable.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum StreamDirection {
    /// From the server.
    #[default]
    Inbound,
    /// Sent by us.
    Outbound,
}

/// One line in a stream's log, from either protocol.
///
/// Server-sent events and websocket frames are different things, but
/// what a person wants to see of them is the same: which way it went,
/// what kind it was, and what it said. One type means the log panel
/// does not branch, and means history and export would not need a
/// second shape later.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct StreamMessage {
    /// Which way it went.
    pub direction: StreamDirection,
    /// The `event:` name for a server-sent event, or the frame type
    /// for a websocket — `text`, `binary`, `ping`, `pong`, `close`.
    pub name: String,
    /// The payload, as text. A binary frame is rendered as hex.
    pub data: String,
    /// The server-sent `id:`, when there was one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// How many bytes the frame held, when it was binary. `data` is a
    /// hex rendering in that case, so its length says nothing useful.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(type = "number | null")]
    pub binary_bytes: Option<u64>,
}

impl StreamMessage {
    /// A text frame, in whichever direction.
    pub fn text(direction: StreamDirection, data: impl Into<String>) -> Self {
        Self {
            direction,
            name: "text".to_string(),
            data: data.into(),
            id: None,
            binary_bytes: None,
        }
    }

    /// A binary frame, shown as hex with its true length beside it.
    pub fn binary(direction: StreamDirection, bytes: &[u8]) -> Self {
        /// Enough to recognise a payload without filling the window
        /// with a megabyte of hex.
        const PREVIEW: usize = 256;

        let mut data = bytes
            .iter()
            .take(PREVIEW)
            .map(|byte| format!("{byte:02x}"))
            .collect::<Vec<_>>()
            .join(" ");
        if bytes.len() > PREVIEW {
            data.push('…');
        }

        Self {
            direction,
            name: "binary".to_string(),
            data,
            id: None,
            binary_bytes: Some(bytes.len() as u64),
        }
    }
}

impl From<ServerEvent> for StreamMessage {
    fn from(event: ServerEvent) -> Self {
        Self {
            direction: StreamDirection::Inbound,
            name: event.event,
            data: event.data,
            id: event.id,
            binary_bytes: None,
        }
    }
}
