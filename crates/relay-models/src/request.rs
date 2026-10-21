//! The saved-request document and everything it contains.

use crate::common::{KeyValue, Revision, Timestamp, default_true};
use crate::ids::{CollectionId, RequestId, WorkspaceId};
use serde::{Deserialize, Serialize};
use std::fmt;
use ts_rs::TS;

/// An HTTP method.
///
/// The common methods are named variants so that the UI can offer them and
/// code can match on them; anything else round-trips through `Other`, because
/// plenty of real APIs use `PURGE`, `LINK` or a WebDAV verb and an API client
/// that refuses to send them is useless for those APIs.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(from = "String", into = "String")]
#[ts(export, type = "string")]
#[derive(Default)]
pub enum HttpMethod {
    #[default]
    Get,
    Post,
    Put,
    Patch,
    Delete,
    Head,
    Options,
    Other(String),
}

impl HttpMethod {
    /// The uppercase wire representation.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
            Self::Put => "PUT",
            Self::Patch => "PATCH",
            Self::Delete => "DELETE",
            Self::Head => "HEAD",
            Self::Options => "OPTIONS",
            Self::Other(s) => s,
        }
    }

    /// Whether the method is idempotent by RFC 9110.
    ///
    /// Relay never automatically retries a request that is not, so a failed
    /// `POST` is reported rather than silently duplicated.
    pub fn is_idempotent(&self) -> bool {
        matches!(
            self,
            Self::Get | Self::Put | Self::Delete | Self::Head | Self::Options
        )
    }

    /// Whether a body is conventionally meaningful for this method.
    pub fn allows_body(&self) -> bool {
        !matches!(self, Self::Get | Self::Head)
    }
}

impl From<String> for HttpMethod {
    fn from(value: String) -> Self {
        match value.to_ascii_uppercase().as_str() {
            "GET" => Self::Get,
            "POST" => Self::Post,
            "PUT" => Self::Put,
            "PATCH" => Self::Patch,
            "DELETE" => Self::Delete,
            "HEAD" => Self::Head,
            "OPTIONS" => Self::Options,
            _ => Self::Other(value.to_ascii_uppercase()),
        }
    }
}

impl From<&str> for HttpMethod {
    fn from(value: &str) -> Self {
        Self::from(value.to_owned())
    }
}

impl From<HttpMethod> for String {
    fn from(value: HttpMethod) -> Self {
        value.as_str().to_owned()
    }
}

impl fmt::Display for HttpMethod {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One field of a multipart form.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct FormField {
    pub key: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(flatten)]
    pub value: FormValue,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Either literal text or a file picked from disk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "camelCase")]
#[ts(export)]
pub enum FormValue {
    /// A text field.
    #[serde(rename_all = "camelCase")]
    Text { value: String },
    /// A file upload, streamed from disk rather than read into memory.
    #[serde(rename_all = "camelCase")]
    File {
        path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        file_name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        content_type: Option<String>,
    },
}

/// The body of a request, tagged by mode.
///
/// A tagged union rather than a mode field plus a pile of optional columns, so
/// that switching from JSON to multipart in the UI cannot leave a stale body
/// behind that silently gets sent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "mode", rename_all = "camelCase")]
#[ts(export)]
#[derive(Default)]
pub enum RequestBody {
    /// No body at all.
    #[default]
    None,
    /// `application/json`.
    #[serde(rename_all = "camelCase")]
    Json { text: String },
    /// `text/plain`.
    #[serde(rename_all = "camelCase")]
    Text { text: String },
    /// `application/xml`.
    #[serde(rename_all = "camelCase")]
    Xml { text: String },
    /// An arbitrary body with a caller-chosen content type.
    #[serde(rename_all = "camelCase")]
    Raw { text: String, content_type: String },
    /// `application/x-www-form-urlencoded`.
    #[serde(rename = "urlencoded", rename_all = "camelCase")]
    UrlEncoded { fields: Vec<KeyValue> },
    /// `multipart/form-data`.
    #[serde(rename_all = "camelCase")]
    Multipart { fields: Vec<FormField> },
    /// The raw bytes of a file on disk, streamed.
    #[serde(rename_all = "camelCase")]
    Binary {
        path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        content_type: Option<String>,
    },
    /// A GraphQL query and its variables, sent as JSON.
    #[serde(rename = "graphql", rename_all = "camelCase")]
    GraphQl {
        query: String,
        #[serde(default)]
        variables: String,
    },
}

impl RequestBody {
    /// The `Content-Type` this body implies, unless the user set one directly.
    pub fn implied_content_type(&self) -> Option<&str> {
        match self {
            Self::None => None,
            Self::Json { .. } | Self::GraphQl { .. } => Some("application/json"),
            Self::Text { .. } => Some("text/plain"),
            Self::Xml { .. } => Some("application/xml"),
            Self::Raw { content_type, .. } => Some(content_type),
            Self::UrlEncoded { .. } => Some("application/x-www-form-urlencoded"),
            // reqwest appends the generated boundary, so multipart must not be
            // given a Content-Type here.
            Self::Multipart { .. } => None,
            Self::Binary { content_type, .. } => content_type.as_deref(),
        }
    }

    /// The mode name, for UI tabs and diagnostics.
    pub fn mode_name(&self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Json { .. } => "json",
            Self::Text { .. } => "text",
            Self::Xml { .. } => "xml",
            Self::Raw { .. } => "raw",
            Self::UrlEncoded { .. } => "urlencoded",
            Self::Multipart { .. } => "multipart",
            Self::Binary { .. } => "binary",
            Self::GraphQl { .. } => "graphql",
        }
    }

    /// True when the body carries no content to send.
    pub fn is_empty(&self) -> bool {
        match self {
            Self::None => true,
            Self::Json { text } | Self::Text { text } | Self::Xml { text } => text.is_empty(),
            Self::Raw { text, .. } => text.is_empty(),
            Self::UrlEncoded { fields } => !fields.iter().any(KeyValue::is_active),
            Self::Multipart { fields } => !fields.iter().any(|f| f.enabled),
            Self::Binary { path, .. } => path.is_empty(),
            Self::GraphQl { query, .. } => query.is_empty(),
        }
    }
}

/// Where an API key is placed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum ApiKeyLocation {
    Header,
    Query,
}

/// Which OAuth 2.0 grant to run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
#[derive(Default)]
pub enum OAuth2Grant {
    #[default]
    AuthorizationCode,
    ClientCredentials,
    Password,
    Implicit,
}

/// How client credentials are presented to the token endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
#[derive(Default)]
pub enum ClientAuthStyle {
    /// HTTP Basic, per RFC 6749 §2.3.1.
    #[default]
    Header,
    /// Form fields in the request body.
    Body,
}

/// A request's authentication scheme.
///
/// Secret material (passwords, tokens, client secrets) is held here only while
/// a request is in flight. Persisted requests reference a variable instead, so
/// that the secret lives in the OS keyring and never reaches sync or exports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "type", rename_all = "camelCase")]
#[ts(export)]
#[derive(Default)]
pub enum AuthConfig {
    /// Send nothing; the request may still carry an explicit header.
    None,
    /// Use whatever the parent folder, collection or workspace specifies.
    #[default]
    Inherit,
    #[serde(rename_all = "camelCase")]
    Basic { username: String, password: String },
    #[serde(rename_all = "camelCase")]
    Bearer { token: String },
    #[serde(rename_all = "camelCase")]
    ApiKey {
        key: String,
        value: String,
        #[serde(rename = "in")]
        location: ApiKeyLocation,
    },
    /// Named explicitly: serde's camelCase would spell this `oAuth2`,
    /// which is how nobody writes it. The tag reaches the database,
    /// exports and the sync wire, so it is worth getting right.
    #[serde(rename = "oauth2")]
    OAuth2(Box<OAuth2Config>),
}

/// OAuth 2.0 parameters. Boxed inside [`AuthConfig`] to keep that enum small.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct OAuth2Config {
    pub grant: OAuth2Grant,
    #[serde(default)]
    pub auth_url: String,
    #[serde(default)]
    pub token_url: String,
    #[serde(default)]
    pub client_id: String,
    #[serde(default)]
    pub client_secret: String,
    #[serde(default)]
    pub scope: String,
    #[serde(default)]
    pub audience: String,
    #[serde(default)]
    pub redirect_uri: String,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub password: String,
    /// PKCE is on by default; turning it off is a deliberate downgrade.
    #[serde(default = "default_true")]
    pub use_pkce: bool,
    #[serde(default)]
    pub client_auth: ClientAuthStyle,
    /// The most recently obtained access token, if any. Never synchronised.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<Timestamp>,
}

impl Default for OAuth2Config {
    fn default() -> Self {
        Self {
            grant: OAuth2Grant::default(),
            auth_url: String::new(),
            token_url: String::new(),
            client_id: String::new(),
            client_secret: String::new(),
            scope: String::new(),
            audience: String::new(),
            redirect_uri: String::new(),
            username: String::new(),
            password: String::new(),
            use_pkce: true,
            client_auth: ClientAuthStyle::default(),
            access_token: None,
            refresh_token: None,
            expires_at: None,
        }
    }
}

/// How redirects are handled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
#[derive(Default)]
pub enum RedirectPolicy {
    /// Do not follow; return the 3xx response as-is.
    Never,
    /// Follow up to `max_redirects`.
    #[default]
    Follow,
}

/// A client certificate presented during the TLS handshake.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ClientCertificate {
    /// Hosts this certificate applies to, e.g. `api.internal`. Empty means all.
    #[serde(default)]
    pub hosts: Vec<String>,
    /// PEM file containing the certificate chain and private key.
    pub pem_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub passphrase: Option<String>,
}

/// An outbound proxy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ProxyConfig {
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
    /// Hosts that bypass the proxy.
    #[serde(default)]
    pub bypass: Vec<String>,
}

/// Per-request execution settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct RequestSettings {
    /// Total request timeout. `None` means the workspace default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(type = "number | null")]
    pub timeout_ms: Option<u64>,
    #[serde(default)]
    pub redirect_policy: RedirectPolicy,
    #[serde(default = "default_max_redirects")]
    pub max_redirects: u32,
    /// Turning this off disables certificate verification for this request.
    ///
    /// It exists because internal services with self-signed certificates are a
    /// fact of life, but it is per-request, defaults to on, and the UI warns
    /// prominently whenever it is off.
    #[serde(default = "default_true")]
    pub verify_tls: bool,
    #[serde(default = "default_true")]
    pub store_cookies: bool,
    #[serde(default = "default_true")]
    pub send_cookies: bool,
    /// Decompress `gzip`/`br`/`deflate`/`zstd` responses automatically.
    #[serde(default = "default_true")]
    pub decompress: bool,
    /// Largest response kept in memory. Larger ones spool to a temp file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(type = "number | null")]
    pub max_response_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy: Option<ProxyConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_certificate: Option<ClientCertificate>,
    /// Encode the URL before sending. Off lets a caller send a deliberately
    /// malformed URL, which is occasionally what you are testing.
    #[serde(default = "default_true")]
    pub encode_url: bool,
}

fn default_max_redirects() -> u32 {
    10
}

impl Default for RequestSettings {
    fn default() -> Self {
        Self {
            timeout_ms: None,
            redirect_policy: RedirectPolicy::default(),
            max_redirects: default_max_redirects(),
            verify_tls: true,
            store_cookies: true,
            send_cookies: true,
            decompress: true,
            max_response_bytes: None,
            proxy: None,
            client_certificate: None,
            encode_url: true,
        }
    }
}

/// Scripts attached to a request.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct RequestScripts {
    /// Runs before the request is sent; may set variables.
    #[serde(default)]
    pub pre_request: String,
    /// Runs after a response arrives; typically assertions.
    #[serde(default)]
    pub post_response: String,
}

impl RequestScripts {
    /// True when there is nothing to run.
    pub fn is_empty(&self) -> bool {
        self.pre_request.trim().is_empty() && self.post_response.trim().is_empty()
    }
}

/// A saved request: the unit a user names, organises and reruns.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct RequestDoc {
    pub id: RequestId,
    pub workspace_id: WorkspaceId,
    /// `None` for a request that sits directly in the workspace root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collection_id: Option<CollectionId>,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub method: HttpMethod,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub headers: Vec<KeyValue>,
    #[serde(default)]
    pub query_params: Vec<KeyValue>,
    #[serde(default)]
    pub path_params: Vec<KeyValue>,
    #[serde(default)]
    pub body: RequestBody,
    #[serde(default)]
    pub auth: AuthConfig,
    #[serde(default)]
    pub settings: RequestSettings,
    #[serde(default)]
    pub scripts: RequestScripts,
    #[serde(default)]
    pub tags: Vec<String>,
    /// Position among siblings. A float so that reordering by drag-and-drop
    /// only rewrites the moved row instead of renumbering the whole folder.
    #[serde(default)]
    pub sort_order: f64,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    #[serde(default, flatten)]
    pub revision: Revision,
}

impl RequestDoc {
    /// A new, empty `GET` request in the given workspace.
    pub fn new(workspace_id: WorkspaceId, name: impl Into<String>) -> Self {
        let now = Timestamp::now();
        Self {
            id: RequestId::new(),
            workspace_id,
            collection_id: None,
            name: name.into(),
            description: String::new(),
            method: HttpMethod::Get,
            url: String::new(),
            headers: Vec::new(),
            query_params: Vec::new(),
            path_params: Vec::new(),
            body: RequestBody::None,
            auth: AuthConfig::Inherit,
            settings: RequestSettings::default(),
            scripts: RequestScripts::default(),
            tags: Vec::new(),
            sort_order: 0.0,
            created_at: now,
            updated_at: now,
            revision: Revision::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn methods_round_trip_through_strings() {
        for m in ["GET", "POST", "PURGE", "LINK"] {
            let parsed = HttpMethod::from(m);
            assert_eq!(parsed.as_str(), m);
            let json = serde_json::to_string(&parsed).unwrap();
            assert_eq!(json, format!("\"{m}\""));
            assert_eq!(serde_json::from_str::<HttpMethod>(&json).unwrap(), parsed);
        }
    }

    #[test]
    fn method_parsing_is_case_insensitive_and_normalises() {
        assert_eq!(HttpMethod::from("get"), HttpMethod::Get);
        assert_eq!(HttpMethod::from("purge").as_str(), "PURGE");
    }

    #[test]
    fn non_idempotent_methods_are_identified() {
        assert!(!HttpMethod::Post.is_idempotent());
        assert!(!HttpMethod::Patch.is_idempotent());
        assert!(HttpMethod::Get.is_idempotent());
        assert!(HttpMethod::Put.is_idempotent());
        assert!(HttpMethod::Delete.is_idempotent());
    }

    #[test]
    fn multipart_has_no_implied_content_type() {
        // reqwest must append its own boundary parameter.
        let body = RequestBody::Multipart { fields: vec![] };
        assert_eq!(body.implied_content_type(), None);
    }

    #[test]
    fn body_serialises_with_a_mode_tag() {
        let body = RequestBody::Json {
            text: "{}".to_string(),
        };
        let json = serde_json::to_value(&body).unwrap();
        assert_eq!(json["mode"], "json");
        assert_eq!(json["text"], "{}");
    }

    #[test]
    fn body_mode_tags_use_conventional_spellings() {
        // These strings appear in exported files and in the TypeScript
        // bindings, so they are part of the contract, not an implementation
        // detail of how the Rust variants happen to be spelled.
        let cases = [
            (RequestBody::UrlEncoded { fields: vec![] }, "urlencoded"),
            (
                RequestBody::GraphQl {
                    query: String::new(),
                    variables: String::new(),
                },
                "graphql",
            ),
            (RequestBody::None, "none"),
            (RequestBody::Multipart { fields: vec![] }, "multipart"),
        ];
        for (body, expected) in cases {
            let json = serde_json::to_value(&body).unwrap();
            assert_eq!(json["mode"], expected);
            assert_eq!(body.mode_name(), expected);
        }
    }

    #[test]
    fn switching_body_mode_cannot_leave_stale_content() {
        // A tagged union means the JSON text simply is not present once the
        // body is urlencoded; there is no orphan column to leak.
        let body = RequestBody::UrlEncoded { fields: vec![] };
        let json = serde_json::to_value(&body).unwrap();
        assert!(json.get("text").is_none());
    }

    #[test]
    fn settings_default_to_secure_values() {
        let s = RequestSettings::default();
        assert!(s.verify_tls, "TLS verification must default to on");
        assert_eq!(s.redirect_policy, RedirectPolicy::Follow);
        assert_eq!(s.max_redirects, 10);
    }

    #[test]
    fn oauth2_defaults_to_pkce() {
        assert!(OAuth2Config::default().use_pkce);
    }

    #[test]
    fn request_doc_round_trips() {
        let doc = RequestDoc::new(WorkspaceId::new(), "Get users");
        let json = serde_json::to_string(&doc).unwrap();
        let back: RequestDoc = serde_json::from_str(&json).unwrap();
        assert_eq!(doc, back);
    }

    #[test]
    fn request_doc_parses_from_a_minimal_payload() {
        // The frontend sends partial documents for new requests; defaults must
        // fill the rest rather than failing.
        let json = r#"{
            "id": "r1", "workspaceId": "w1", "name": "Ping",
            "createdAt": 1, "updatedAt": 2
        }"#;
        let doc: RequestDoc = serde_json::from_str(json).unwrap();
        assert_eq!(doc.method, HttpMethod::Get);
        assert_eq!(doc.body, RequestBody::None);
        assert!(doc.settings.verify_tls);
    }
}
