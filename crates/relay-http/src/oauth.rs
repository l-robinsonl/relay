//! OAuth 2.0 token acquisition.
//!
//! Relay is the *client* here. The authorization server is whatever API
//! the user is testing — GitHub, Stripe, a company's own Okta — so
//! nothing about this can be specialised to one provider.
//!
//! Two grants run without a browser and are handled here directly:
//! client credentials and the resource owner password grant. The
//! authorization code grant needs a redirect and is driven by
//! [`crate::oauth_redirect`], which exchanges its result through
//! [`exchange_code`].
//!
//! The implicit grant is deliberately not implemented. It returns the
//! token in a URL fragment, has been removed from OAuth 2.1, and is
//! discouraged by the current security BCP; anyone who needs it should
//! use the authorization code grant with PKCE instead.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as BASE64URL;
use relay_models::{ClientAuthStyle, OAuth2Config, OAuth2Grant, Timestamp};
use sha2::{Digest, Sha256};
use std::time::Duration;

use crate::error::EngineError;

/// A token as the authorization server returned it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenResponse {
    /// The token to send as `Authorization: Bearer …`.
    pub access_token: String,
    /// Trades for a new access token without the user signing in again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    /// Almost always `Bearer`; kept for the rare server that differs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_type: Option<String>,
    /// When the access token stops working, derived from `expires_in`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<Timestamp>,
    /// What was actually granted, which may be less than was asked for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
}

/// What a server sent back, before interpretation.
#[derive(Debug, serde::Deserialize)]
struct RawToken {
    access_token: Option<String>,
    refresh_token: Option<String>,
    token_type: Option<String>,
    expires_in: Option<serde_json::Value>,
    scope: Option<String>,
    // The error shape from RFC 6749 §5.2.
    error: Option<String>,
    error_description: Option<String>,
}

/// A PKCE verifier and the challenge derived from it.
#[derive(Debug, Clone)]
pub struct PkcePair {
    /// Sent with the code exchange.
    pub verifier: String,
    /// Sent with the authorization request.
    pub challenge: String,
}

impl PkcePair {
    /// Generates a fresh pair using S256.
    ///
    /// The `plain` method is not offered: it exists only for clients that
    /// cannot compute SHA-256, which does not describe anything here.
    pub fn generate() -> Result<Self, EngineError> {
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes)
            .map_err(|e| EngineError::RequestBody(format!("no system randomness: {e}")))?;

        let verifier = BASE64URL.encode(bytes);
        let challenge = BASE64URL.encode(Sha256::digest(verifier.as_bytes()));

        Ok(Self {
            verifier,
            challenge,
        })
    }
}

/// Fetches a token without any user interaction.
///
/// Handles the client credentials and password grants. The authorization
/// code grant cannot be completed here because it requires a browser.
pub async fn fetch_token(config: &OAuth2Config) -> Result<TokenResponse, EngineError> {
    let mut form: Vec<(String, String)> = Vec::new();

    match config.grant {
        OAuth2Grant::ClientCredentials => {
            form.push(("grant_type".into(), "client_credentials".into()));
        }
        OAuth2Grant::Password => {
            form.push(("grant_type".into(), "password".into()));
            form.push(("username".into(), config.username.clone()));
            form.push(("password".into(), config.password.clone()));
        }
        OAuth2Grant::AuthorizationCode => {
            return Err(EngineError::RequestBody(
                "the authorization code grant needs a browser; use Authorize rather than \
                 fetching a token directly"
                    .into(),
            ));
        }
        OAuth2Grant::Implicit => {
            return Err(EngineError::RequestBody(
                "the implicit grant is not supported. It returns the token in a URL \
                 fragment, was removed in OAuth 2.1 and is discouraged by the current \
                 security guidance — use the authorization code grant with PKCE instead."
                    .into(),
            ));
        }
    }

    if !config.scope.trim().is_empty() {
        form.push(("scope".into(), config.scope.trim().into()));
    }
    if !config.audience.trim().is_empty() {
        form.push(("audience".into(), config.audience.trim().into()));
    }

    post_token(config, form).await
}

/// Exchanges an authorization code for a token.
pub async fn exchange_code(
    config: &OAuth2Config,
    code: &str,
    verifier: Option<&str>,
) -> Result<TokenResponse, EngineError> {
    let mut form = vec![
        ("grant_type".to_string(), "authorization_code".to_string()),
        ("code".to_string(), code.to_string()),
    ];

    if !config.redirect_uri.trim().is_empty() {
        // The redirect URI must match the one sent to /authorize, even
        // though nothing is redirected at this step. RFC 6749 §4.1.3.
        form.push(("redirect_uri".into(), config.redirect_uri.trim().into()));
    }
    if let Some(verifier) = verifier {
        form.push(("code_verifier".into(), verifier.to_string()));
    }

    post_token(config, form).await
}

/// Trades a refresh token for a new access token.
pub async fn refresh(
    config: &OAuth2Config,
    refresh_token: &str,
) -> Result<TokenResponse, EngineError> {
    let mut form = vec![
        ("grant_type".to_string(), "refresh_token".to_string()),
        ("refresh_token".to_string(), refresh_token.to_string()),
    ];

    if !config.scope.trim().is_empty() {
        form.push(("scope".into(), config.scope.trim().into()));
    }

    post_token(config, form).await
}

/// True when a token is absent, or close enough to expiry to replace.
///
/// The sixty-second margin avoids the case where a token passes a check
/// and expires in flight, which produces a confusing 401 on a request
/// that looked fine.
pub fn needs_refresh(config: &OAuth2Config) -> bool {
    if config
        .access_token
        .as_deref()
        .unwrap_or("")
        .trim()
        .is_empty()
    {
        return true;
    }
    match config.expires_at {
        None => false,
        Some(expiry) => expiry.as_millis() <= Timestamp::now().as_millis() + 60_000,
    }
}

async fn post_token(
    config: &OAuth2Config,
    mut form: Vec<(String, String)>,
) -> Result<TokenResponse, EngineError> {
    let token_url = config.token_url.trim();
    if token_url.is_empty() {
        return Err(EngineError::InvalidUrl(
            "the token URL is empty — set it under Auth".into(),
        ));
    }
    if !token_url.starts_with("https://") && !is_loopback(token_url) {
        // Sending a client secret over plain HTTP hands it to anyone on
        // the path. Loopback is allowed because local test servers are
        // how people develop against this.
        return Err(EngineError::InvalidUrl(
            "the token URL must use https — a client secret sent over http is readable \
             by anyone on the network path"
                .into(),
        ));
    }

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| EngineError::ClientBuild(e.to_string()))?;

    let mut request = client
        .post(token_url)
        .header(reqwest::header::ACCEPT, "application/json");

    match config.client_auth {
        // RFC 6749 §2.3.1 prefers HTTP Basic, and some servers accept
        // nothing else.
        ClientAuthStyle::Header => {
            let credentials = BASE64.encode(format!(
                "{}:{}",
                form_encode(&config.client_id),
                form_encode(&config.client_secret)
            ));
            let mut value = reqwest::header::HeaderValue::from_str(&format!("Basic {credentials}"))
                .map_err(|_| EngineError::InvalidHeader {
                    name: "Authorization".into(),
                    reason: "the client id or secret contains invalid characters".into(),
                })?;
            value.set_sensitive(true);
            request = request.header(reqwest::header::AUTHORIZATION, value);
        }
        ClientAuthStyle::Body => {
            form.push(("client_id".into(), config.client_id.clone()));
            if !config.client_secret.is_empty() {
                form.push(("client_secret".into(), config.client_secret.clone()));
            }
        }
    }

    let response = request
        .form(&form)
        .send()
        .await
        .map_err(EngineError::Transport)?;

    let status = response.status();
    let body = response.text().await.map_err(EngineError::Transport)?;

    let parsed: RawToken = serde_json::from_str(&body).map_err(|_| {
        // A server that answers a token request with HTML is almost
        // always a wrong URL, and saying so saves a long hunt.
        EngineError::OAuth(format!(
            "the token endpoint returned {status} and something that is not JSON. \
             Check the token URL."
        ))
    })?;

    if let Some(error) = parsed.error {
        let detail = parsed
            .error_description
            .map(|d| format!(": {d}"))
            .unwrap_or_default();
        return Err(EngineError::OAuth(format!(
            "the authorization server refused: {error}{detail}"
        )));
    }

    let access_token = parsed.access_token.ok_or_else(|| {
        EngineError::OAuth(format!(
            "the token endpoint returned {status} with no access_token"
        ))
    })?;

    Ok(TokenResponse {
        access_token,
        refresh_token: parsed.refresh_token,
        token_type: parsed.token_type,
        expires_at: parsed.expires_in.and_then(expiry_from),
        scope: parsed.scope,
    })
}

/// Reads `expires_in`, which servers send as a number or a string.
fn expiry_from(value: serde_json::Value) -> Option<Timestamp> {
    let seconds = match value {
        serde_json::Value::Number(n) => n.as_i64(),
        serde_json::Value::String(s) => s.parse::<i64>().ok(),
        _ => None,
    }?;

    Some(Timestamp(Timestamp::now().as_millis() + seconds * 1000))
}

/// True for a URL pointing at this machine.
fn is_loopback(url: &str) -> bool {
    let Ok(parsed) = url::Url::parse(url) else {
        return false;
    };
    matches!(
        parsed.host_str(),
        Some("localhost" | "127.0.0.1" | "[::1]" | "::1")
    )
}

/// Percent-encodes a credential for HTTP Basic, per RFC 6749 §2.3.1.
/// Encodes one half of a client credential pair.
///
/// RFC 6749 §2.3.1 says to apply `application/x-www-form-urlencoded`
/// encoding to the id and secret before joining them with a colon and
/// base64-encoding the result. That encoding leaves `*`, `-`, `.` and `_`
/// alone — which matters, because real client ids are full of hyphens and
/// dots. Percent-encoding those turns a valid credential into one the
/// server has never seen.
fn form_encode(value: &str) -> String {
    const FORM: &percent_encoding::AsciiSet = &percent_encoding::NON_ALPHANUMERIC
        .remove(b'*')
        .remove(b'-')
        .remove(b'.')
        .remove(b'_');

    // In this encoding a space is `+`. Anything that was a literal `%20`
    // in the input has already become `%2520`, so this cannot corrupt it.
    percent_encoding::utf8_percent_encode(value, FORM)
        .to_string()
        .replace("%20", "+")
}

/// Builds the URL to open in a browser for the authorization code grant.
pub fn authorization_url(
    config: &OAuth2Config,
    state: &str,
    challenge: Option<&str>,
) -> Result<String, EngineError> {
    let auth_url = config.auth_url.trim();
    if auth_url.is_empty() {
        return Err(EngineError::InvalidUrl(
            "the authorization URL is empty — set it under Auth".into(),
        ));
    }

    let mut url = url::Url::parse(auth_url)
        .map_err(|e| EngineError::InvalidUrl(format!("{auth_url}: {e}")))?;

    // This URL is handed to the operating system to open. A collection
    // can arrive by import or from a shared sync server, so the scheme
    // is not something to take on trust: without this check a crafted
    // collection could have Relay launch a `file://` path or whatever
    // custom scheme handler happens to be registered.
    if !matches!(url.scheme(), "https" | "http") {
        return Err(EngineError::InvalidUrl(format!(
            "the authorization URL must be http or https, not {}",
            url.scheme()
        )));
    }

    {
        let mut query = url.query_pairs_mut();
        query.append_pair("response_type", "code");
        query.append_pair("client_id", &config.client_id);
        if !config.redirect_uri.trim().is_empty() {
            query.append_pair("redirect_uri", config.redirect_uri.trim());
        }
        if !config.scope.trim().is_empty() {
            query.append_pair("scope", config.scope.trim());
        }
        if !config.audience.trim().is_empty() {
            query.append_pair("audience", config.audience.trim());
        }
        // `state` is what ties the callback back to this attempt, and is
        // the defence against a forged redirect.
        query.append_pair("state", state);

        if let Some(challenge) = challenge {
            query.append_pair("code_challenge", challenge);
            query.append_pair("code_challenge_method", "S256");
        }
    }

    Ok(url.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Decodes a Basic credential back to the pair it was built from.
    fn decode_basic(header: &str) -> String {
        let encoded = header.trim_start_matches("Basic ").trim();
        String::from_utf8(BASE64.decode(encoded).expect("standard base64")).unwrap()
    }

    #[test]
    fn a_client_id_with_hyphens_and_dots_survives_basic_encoding() {
        // Real client ids look like this. Percent-encoding the hyphens
        // produces a credential the server has never issued, and the
        // only symptom is `invalid_client`, which reads like a typo.
        let id = form_encode("relay-test.client_9");
        let secret = form_encode("s3cret");
        let header = format!("Basic {}", BASE64.encode(format!("{id}:{secret}")));

        assert_eq!(decode_basic(&header), "relay-test.client_9:s3cret");
    }

    #[test]
    fn an_authorization_url_with_an_exotic_scheme_is_refused() {
        // The authorization URL is opened by the operating system, and a
        // collection can arrive by import or from a shared workspace.
        let mut config = OAuth2Config {
            grant: OAuth2Grant::AuthorizationCode,
            client_id: "id".into(),
            ..Default::default()
        };

        for hostile in [
            "file:///etc/passwd",
            "javascript:alert(1)",
            "ms-msdt:/id",
            "smb://host/share",
        ] {
            config.auth_url = hostile.into();
            let error = authorization_url(&config, "state", None).unwrap_err();
            assert!(
                error.to_string().contains("must be http or https"),
                "{hostile} was allowed: {error}"
            );
        }

        config.auth_url = "https://example.test/authorize".into();
        assert!(authorization_url(&config, "state", None).is_ok());
    }

    #[test]
    fn characters_that_would_break_the_pair_are_still_encoded() {
        // A colon in the id would otherwise split the pair in the wrong
        // place, and a secret with a space must not be truncated.
        assert_eq!(form_encode("a:b"), "a%3Ab");
        assert_eq!(form_encode("two words"), "two+words");
        assert_eq!(form_encode("100%"), "100%25");
        assert_eq!(form_encode("a+b"), "a%2Bb");
    }

    fn config() -> OAuth2Config {
        OAuth2Config {
            grant: OAuth2Grant::ClientCredentials,
            token_url: "https://auth.example.com/token".into(),
            auth_url: "https://auth.example.com/authorize".into(),
            client_id: "my-client".into(),
            client_secret: "my-secret".into(),
            redirect_uri: "http://127.0.0.1:8900/callback".into(),
            ..Default::default()
        }
    }

    #[test]
    fn pkce_uses_s256_and_a_fresh_verifier_each_time() {
        let a = PkcePair::generate().unwrap();
        let b = PkcePair::generate().unwrap();

        assert_ne!(a.verifier, b.verifier);
        assert_ne!(a.challenge, b.challenge);
        // 32 bytes base64url, unpadded.
        assert!(a.verifier.len() >= 43);
        assert!(!a.verifier.contains('='), "the verifier must be unpadded");

        // The challenge really is S256 of the verifier.
        let expected = BASE64URL.encode(Sha256::digest(a.verifier.as_bytes()));
        assert_eq!(a.challenge, expected);
    }

    #[test]
    fn the_authorization_url_carries_everything_the_server_needs() {
        let pkce = PkcePair::generate().unwrap();
        let url = authorization_url(&config(), "the-state", Some(&pkce.challenge)).unwrap();
        let parsed = url::Url::parse(&url).unwrap();
        let query: std::collections::HashMap<_, _> = parsed.query_pairs().collect();

        assert_eq!(query["response_type"], "code");
        assert_eq!(query["client_id"], "my-client");
        assert_eq!(query["state"], "the-state");
        assert_eq!(query["code_challenge_method"], "S256");
        assert_eq!(query["code_challenge"], pkce.challenge);
        assert_eq!(query["redirect_uri"], "http://127.0.0.1:8900/callback");
    }

    #[test]
    fn the_authorization_url_never_carries_the_client_secret() {
        let url = authorization_url(&config(), "s", None).unwrap();
        assert!(
            !url.contains("my-secret"),
            "the secret reached the browser: {url}"
        );
    }

    #[test]
    fn an_existing_query_on_the_authorization_url_is_kept() {
        let mut config = config();
        config.auth_url = "https://auth.example.com/authorize?tenant=acme".into();
        let url = authorization_url(&config, "s", None).unwrap();

        assert!(url.contains("tenant=acme"));
        assert!(url.contains("response_type=code"));
    }

    #[tokio::test]
    async fn a_plain_http_token_url_is_refused() {
        // A client secret over http is readable by anyone on the path.
        let mut config = config();
        config.token_url = "http://auth.example.com/token".into();

        let error = fetch_token(&config).await.unwrap_err();
        assert!(error.to_string().contains("https"), "{error}");
    }

    #[tokio::test]
    async fn a_loopback_token_url_is_allowed_for_local_development() {
        let mut config = config();
        config.token_url = "http://127.0.0.1:1/token".into();

        // Reaches the network and fails to connect, rather than being
        // refused for its scheme.
        let error = fetch_token(&config).await.unwrap_err();
        assert!(!error.to_string().contains("must use https"), "{error}");
    }

    #[tokio::test]
    async fn the_implicit_grant_explains_why_it_is_not_supported() {
        let mut config = config();
        config.grant = OAuth2Grant::Implicit;

        let error = fetch_token(&config).await.unwrap_err().to_string();
        assert!(
            error.contains("PKCE"),
            "no alternative was offered: {error}"
        );
    }

    #[tokio::test]
    async fn the_authorization_code_grant_sends_the_user_to_the_browser() {
        let mut config = config();
        config.grant = OAuth2Grant::AuthorizationCode;

        let error = fetch_token(&config).await.unwrap_err().to_string();
        assert!(error.contains("browser"), "{error}");
    }

    #[test]
    fn expiry_is_read_from_a_number_or_a_string() {
        let from_number = expiry_from(serde_json::json!(3600)).unwrap();
        let from_string = expiry_from(serde_json::json!("3600")).unwrap();

        let expected = Timestamp::now().as_millis() + 3_600_000;
        assert!((from_number.as_millis() - expected).abs() < 2000);
        assert!((from_string.as_millis() - expected).abs() < 2000);

        assert!(expiry_from(serde_json::json!(null)).is_none());
        assert!(expiry_from(serde_json::json!("soon")).is_none());
    }

    #[test]
    fn a_token_about_to_expire_counts_as_needing_refresh() {
        let mut config = config();
        config.access_token = Some("abc".into());

        // A token valid for another ten seconds would expire in flight.
        config.expires_at = Some(Timestamp(Timestamp::now().as_millis() + 10_000));
        assert!(needs_refresh(&config));

        config.expires_at = Some(Timestamp(Timestamp::now().as_millis() + 600_000));
        assert!(!needs_refresh(&config));

        // No expiry means the server did not say, so it is taken on trust.
        config.expires_at = None;
        assert!(!needs_refresh(&config));
    }

    #[test]
    fn a_missing_token_always_needs_fetching() {
        let mut config = config();
        assert!(needs_refresh(&config));

        config.access_token = Some("   ".into());
        assert!(needs_refresh(&config), "whitespace is not a token");
    }

    #[test]
    fn loopback_hosts_are_recognised() {
        assert!(is_loopback("http://localhost:8080/token"));
        assert!(is_loopback("http://127.0.0.1/token"));
        assert!(!is_loopback("http://auth.example.com/token"));
        assert!(!is_loopback("not a url"));
    }
}
