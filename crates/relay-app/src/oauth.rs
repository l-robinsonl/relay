//! OAuth 2.0 token acquisition and caching.
//!
//! Access tokens are credentials, and Relay's rule for credentials is that
//! they never enter the SQLite database, an export or a sync payload. So a
//! token obtained here is held in memory for the life of the process and
//! keyed by the configuration that produced it. A request document stores
//! the parameters of the flow — client id, scopes, URLs — and never the
//! token itself.
//!
//! Losing tokens on restart is a small cost: a client credentials grant
//! re-fetches silently, and an authorization code grant usually has a
//! refresh token the user can trade in without visiting a browser again.
//! Writing them to disk to avoid that would undo the guarantee above.

use std::collections::HashMap;
use std::sync::RwLock;
use std::time::Duration;

use relay_http::oauth::{self, TokenResponse};
use relay_models::{AuthConfig, ErrorKind, OAuth2Config, OAuth2Grant, RelayError, RelayResult};
use sha2::{Digest, Sha256};

/// How long to wait for the user to finish in their browser.
const AUTHORIZE_TIMEOUT: Duration = Duration::from_secs(300);

/// Tokens held for the life of the process, keyed by OAuth configuration.
#[derive(Default)]
pub struct TokenCache {
    entries: RwLock<HashMap<String, TokenResponse>>,
}

impl std::fmt::Debug for TokenCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Deliberately prints a count and no values: a cache full of
        // bearer tokens must not end up in a log line.
        let held = self.entries.read().map(|e| e.len()).unwrap_or(0);
        f.debug_struct("TokenCache").field("held", &held).finish()
    }
}

impl TokenCache {
    /// An empty cache.
    pub fn new() -> Self {
        Self::default()
    }

    /// Identifies the token a configuration should use.
    ///
    /// Everything that changes which token is correct goes into the key,
    /// the client secret included: rotating a secret is nearly always a
    /// user fixing their configuration, and they should not have to work
    /// out why the old token is still being sent. The key is a digest so
    /// that no credential is held as a map key.
    pub fn key(config: &OAuth2Config) -> String {
        let mut hasher = Sha256::new();
        for part in [
            format!("{:?}", config.grant),
            config.token_url.trim().to_string(),
            config.auth_url.trim().to_string(),
            config.client_id.trim().to_string(),
            config.client_secret.to_string(),
            config.scope.trim().to_string(),
            config.audience.trim().to_string(),
            config.username.trim().to_string(),
        ] {
            hasher.update(part.as_bytes());
            // A separator no field can contain, so that ("ab", "c") and
            // ("a", "bc") cannot hash alike.
            hasher.update([0u8]);
        }
        hasher
            .finalize()
            .iter()
            .fold(String::with_capacity(64), |mut s, byte| {
                use std::fmt::Write as _;
                let _ = write!(s, "{byte:02x}");
                s
            })
    }

    /// The token held for this configuration, if any.
    pub fn get(&self, config: &OAuth2Config) -> Option<TokenResponse> {
        self.entries.read().ok()?.get(&Self::key(config)).cloned()
    }

    /// Stores a token against its configuration.
    pub fn put(&self, config: &OAuth2Config, token: TokenResponse) {
        if let Ok(mut entries) = self.entries.write() {
            entries.insert(Self::key(config), token);
        }
    }

    /// Drops the token for one configuration, so the next send re-acquires.
    pub fn forget(&self, config: &OAuth2Config) {
        if let Ok(mut entries) = self.entries.write() {
            entries.remove(&Self::key(config));
        }
    }

    /// Drops every token. Used when signing out or clearing session data.
    pub fn forget_all(&self) {
        if let Ok(mut entries) = self.entries.write() {
            entries.clear();
        }
    }

    /// How many tokens are held. For diagnostics and tests.
    pub fn len(&self) -> usize {
        self.entries.read().map(|e| e.len()).unwrap_or(0)
    }

    /// True when no token is held.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Copies a token's fields onto a configuration.
fn apply(config: &mut OAuth2Config, token: &TokenResponse) {
    config.access_token = Some(token.access_token.clone());
    config.expires_at = token.expires_at;
    // A refresh response may legitimately omit a new refresh token, in
    // which case the old one stays valid (RFC 6749 §6).
    if token.refresh_token.is_some() {
        config.refresh_token = token.refresh_token.clone();
    }
}

/// What `ensure_token` did, so the caller can report it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenSource {
    /// The configuration already carried a usable token.
    Inline,
    /// A cached token was still good.
    Cache,
    /// A refresh token was traded for a new access token.
    Refreshed,
    /// The token endpoint was called directly.
    Fetched,
}

/// Makes sure a configuration carries a usable access token.
///
/// Silent paths only: anything needing a browser is refused with an
/// explanation, because a send is not the moment to open a window.
pub async fn ensure_token(
    cache: &TokenCache,
    config: &mut OAuth2Config,
) -> RelayResult<TokenSource> {
    // A token typed in by hand, or one already on the config, wins. The
    // user may be pasting a token from somewhere else entirely.
    if !oauth::needs_refresh(config) {
        return Ok(TokenSource::Inline);
    }

    if let Some(cached) = cache.get(config) {
        let mut probe = config.clone();
        apply(&mut probe, &cached);

        if !oauth::needs_refresh(&probe) {
            apply(config, &cached);
            return Ok(TokenSource::Cache);
        }

        // Expired, but a refresh token avoids sending the user back to a
        // browser. This is the usual path for an authorization code grant
        // on the second day of use.
        if let Some(refresh_token) = cached.refresh_token.as_deref().filter(|t| !t.is_empty()) {
            match oauth::refresh(config, refresh_token).await {
                Ok(token) => {
                    // Carry the old refresh token forward when the server
                    // did not issue a new one, or the next refresh fails.
                    let token = TokenResponse {
                        refresh_token: token.refresh_token.or_else(|| cached.refresh_token.clone()),
                        ..token
                    };
                    apply(config, &token);
                    cache.put(config, token);
                    return Ok(TokenSource::Refreshed);
                }
                Err(error) => {
                    // A refresh token can be revoked or simply expire. For
                    // grants that can start again without a browser, fall
                    // through and do that rather than failing.
                    if matches!(
                        config.grant,
                        OAuth2Grant::AuthorizationCode | OAuth2Grant::Implicit
                    ) {
                        cache.forget(config);
                        return Err(RelayError::new(
                            ErrorKind::Transport,
                            format!("the OAuth refresh token was rejected: {error}"),
                        )
                        .with_detail("Authorize again from the Auth tab.".to_string()));
                    }
                    tracing::debug!(%error, "OAuth refresh failed; re-fetching");
                }
            }
        }
    }

    match config.grant {
        OAuth2Grant::ClientCredentials | OAuth2Grant::Password => {
            let token = oauth::fetch_token(config)
                .await
                .map_err(|e| RelayError::new(ErrorKind::Transport, e.to_string()))?;
            apply(config, &token);
            cache.put(config, token);
            Ok(TokenSource::Fetched)
        }
        OAuth2Grant::AuthorizationCode => Err(RelayError::new(
            ErrorKind::Transport,
            "this request uses the OAuth 2.0 authorization code grant but has no access token",
        )
        .with_detail(
            "Open the Auth tab and choose Authorize. That signs in through your browser \
             once; after that Relay refreshes the token on its own."
                .to_string(),
        )),
        OAuth2Grant::Implicit => Err(RelayError::new(
            ErrorKind::Transport,
            "the implicit grant is not supported",
        )
        .with_detail(
            "It returns tokens in a URL fragment, where they end up in browser history \
             and server logs, and it issues no refresh token. Use the authorization code \
             grant with PKCE instead — it is the replacement, and Relay supports it."
                .to_string(),
        )),
    }
}

/// Makes sure an authentication configuration is ready to be sent.
///
/// Anything other than OAuth 2.0 passes straight through.
pub async fn prepare_auth(cache: &TokenCache, auth: &mut AuthConfig) -> RelayResult<()> {
    if let AuthConfig::OAuth2(config) = auth {
        ensure_token(cache, config).await?;
    }
    Ok(())
}

/// The browser step of the authorization code grant.
///
/// Returns the URL to open and the work that completes once the user has
/// finished with it. Splitting it this way lets the caller open the
/// browser — which only the desktop layer knows how to do — while the
/// listener is already bound and waiting, with no race between the two.
pub struct Authorization {
    /// Where to send the user.
    pub url: String,
    listener: relay_http::oauth_redirect::RedirectListener,
    state: String,
    verifier: Option<String>,
    config: OAuth2Config,
}

impl std::fmt::Debug for Authorization {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Authorization")
            .field("url", &self.url)
            .finish()
    }
}

/// Begins an authorization code flow: binds the listener, builds the URL.
pub fn begin_authorization(config: &OAuth2Config) -> RelayResult<AuthorizationPending> {
    if config.grant != OAuth2Grant::AuthorizationCode {
        return Err(RelayError::new(
            ErrorKind::Invalid,
            "only the authorization code grant uses the browser",
        ));
    }

    Ok(AuthorizationPending {
        config: config.clone(),
    })
}

/// A flow that has been asked for but not yet bound to a port.
#[derive(Debug)]
pub struct AuthorizationPending {
    config: OAuth2Config,
}

impl AuthorizationPending {
    /// Binds the loopback listener and composes the authorization URL.
    pub async fn start(self) -> RelayResult<Authorization> {
        let config = self.config;

        // A server that insists on a registered redirect URI pins the
        // port; otherwise let the OS pick a free one.
        let requested_port = fixed_port(&config.redirect_uri);

        let listener = relay_http::oauth_redirect::RedirectListener::bind(requested_port)
            .await
            .map_err(|e| RelayError::new(ErrorKind::Transport, e.to_string()))?;

        let mut config = config;
        if config.redirect_uri.trim().is_empty() {
            config.redirect_uri = listener.redirect_uri();
        }

        // Unguessable, and checked when the browser comes back, so that a
        // redirect Relay did not start cannot have its code exchanged.
        let state = random_token()?;

        let pkce = if config.use_pkce {
            Some(
                oauth::PkcePair::generate()
                    .map_err(|e| RelayError::new(ErrorKind::Transport, e.to_string()))?,
            )
        } else {
            None
        };

        let url =
            oauth::authorization_url(&config, &state, pkce.as_ref().map(|p| p.challenge.as_str()))
                .map_err(|e| RelayError::new(ErrorKind::Transport, e.to_string()))?;

        Ok(Authorization {
            url,
            listener,
            state,
            verifier: pkce.map(|p| p.verifier),
            config,
        })
    }
}

impl Authorization {
    /// The redirect URI the authorization server must be told about.
    pub fn redirect_uri(&self) -> &str {
        &self.config.redirect_uri
    }

    /// Waits for the browser, exchanges the code, and caches the token.
    pub async fn complete(self, cache: &TokenCache) -> RelayResult<OAuth2Config> {
        let redirect = self
            .listener
            .wait(&self.state, AUTHORIZE_TIMEOUT)
            .await
            .map_err(|e| RelayError::new(ErrorKind::Transport, e.to_string()))?;

        let token = oauth::exchange_code(&self.config, &redirect.code, self.verifier.as_deref())
            .await
            .map_err(|e| RelayError::new(ErrorKind::Transport, e.to_string()))?;

        let mut config = self.config;
        apply(&mut config, &token);
        cache.put(&config, token);

        Ok(config)
    }
}

/// The port of a loopback redirect URI, or zero to let the OS choose.
fn fixed_port(redirect_uri: &str) -> u16 {
    let uri = redirect_uri.trim();
    if uri.is_empty() {
        return 0;
    }

    url::Url::parse(uri)
        .ok()
        .filter(|u| {
            matches!(
                u.host_str(),
                Some("127.0.0.1") | Some("localhost") | Some("[::1]")
            )
        })
        .and_then(|u| u.port())
        .unwrap_or(0)
}

/// 32 bytes of randomness, URL-safe.
fn random_token() -> RelayResult<String> {
    use base64::Engine as _;
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes)
        .map_err(|e| RelayError::new(ErrorKind::Internal, format!("no secure randomness: {e}")))?;
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use relay_models::Timestamp;

    fn config() -> OAuth2Config {
        OAuth2Config {
            grant: OAuth2Grant::ClientCredentials,
            token_url: "https://example.test/token".into(),
            client_id: "id".into(),
            client_secret: "secret".into(),
            ..Default::default()
        }
    }

    fn token(access: &str, expires_in_ms: i64) -> TokenResponse {
        TokenResponse {
            access_token: access.into(),
            refresh_token: None,
            token_type: Some("Bearer".into()),
            expires_at: Some(Timestamp::from(
                Timestamp::now().as_millis() + expires_in_ms,
            )),
            scope: None,
        }
    }

    #[test]
    fn the_key_separates_configurations_that_need_different_tokens() {
        let base = config();

        let mut other_scope = base.clone();
        other_scope.scope = "read:all".into();
        assert_ne!(TokenCache::key(&base), TokenCache::key(&other_scope));

        let mut other_client = base.clone();
        other_client.client_id = "other".into();
        assert_ne!(TokenCache::key(&base), TokenCache::key(&other_client));

        // Rotating the secret must not keep serving the old token: a user
        // who changes it is nearly always fixing a broken configuration.
        let mut rotated = base.clone();
        rotated.client_secret = "new".into();
        assert_ne!(TokenCache::key(&base), TokenCache::key(&rotated));
    }

    #[test]
    fn the_key_cannot_be_confused_by_field_boundaries() {
        let mut a = config();
        a.client_id = "ab".into();
        a.scope = "c".into();

        let mut b = config();
        b.client_id = "a".into();
        b.scope = "bc".into();

        assert_ne!(TokenCache::key(&a), TokenCache::key(&b));
    }

    #[test]
    fn the_key_holds_no_credential() {
        let key = TokenCache::key(&config());
        assert!(
            !key.contains("secret"),
            "the client secret leaked into the cache key"
        );
        assert_eq!(key.len(), 64);
    }

    #[test]
    fn debug_output_does_not_print_tokens() {
        let cache = TokenCache::new();
        cache.put(&config(), token("super-secret-token", 60_000));

        let printed = format!("{cache:?}");
        assert!(!printed.contains("super-secret-token"), "{printed}");
        assert!(printed.contains("held: 1"), "{printed}");
    }

    #[tokio::test]
    async fn a_cached_token_is_reused() {
        let cache = TokenCache::new();
        cache.put(&config(), token("cached", 600_000));

        let mut c = config();
        let source = ensure_token(&cache, &mut c).await.unwrap();

        assert_eq!(source, TokenSource::Cache);
        assert_eq!(c.access_token.as_deref(), Some("cached"));
    }

    #[tokio::test]
    async fn an_inline_token_is_left_alone() {
        let cache = TokenCache::new();
        cache.put(&config(), token("cached", 600_000));

        let mut c = config();
        c.access_token = Some("pasted-by-hand".into());

        let source = ensure_token(&cache, &mut c).await.unwrap();

        assert_eq!(source, TokenSource::Inline);
        assert_eq!(c.access_token.as_deref(), Some("pasted-by-hand"));
    }

    #[tokio::test]
    async fn a_token_about_to_expire_is_not_reused() {
        let cache = TokenCache::new();
        // Inside the sixty-second margin: using it risks a 401 in flight.
        cache.put(&config(), token("nearly-stale", 10_000));

        let mut c = config();
        // No server here, so the re-fetch fails — which is itself the
        // proof that the cached token was rejected.
        let error = ensure_token(&cache, &mut c).await.unwrap_err();
        assert_ne!(c.access_token.as_deref(), Some("nearly-stale"));
        assert_eq!(error.kind, ErrorKind::Transport);
    }

    #[tokio::test]
    async fn the_authorization_code_grant_asks_the_user_to_authorize() {
        let cache = TokenCache::new();
        let mut c = config();
        c.grant = OAuth2Grant::AuthorizationCode;

        let error = ensure_token(&cache, &mut c).await.unwrap_err();

        assert!(error.message.contains("authorization code"), "{error:?}");
        assert!(
            error.detail.as_deref().unwrap_or("").contains("Authorize"),
            "the error should say what to do: {error:?}"
        );
    }

    #[tokio::test]
    async fn the_implicit_grant_explains_itself() {
        let cache = TokenCache::new();
        let mut c = config();
        c.grant = OAuth2Grant::Implicit;

        let error = ensure_token(&cache, &mut c).await.unwrap_err();
        assert!(
            error.detail.as_deref().unwrap_or("").contains("PKCE"),
            "{error:?}"
        );
    }

    #[tokio::test]
    async fn forgetting_a_token_drops_only_that_one() {
        let cache = TokenCache::new();
        let a = config();
        let mut b = config();
        b.scope = "other".into();

        cache.put(&a, token("a", 600_000));
        cache.put(&b, token("b", 600_000));
        assert_eq!(cache.len(), 2);

        cache.forget(&a);
        assert!(cache.get(&a).is_none());
        assert!(cache.get(&b).is_some());

        cache.forget_all();
        assert!(cache.is_empty());
    }

    #[tokio::test]
    async fn non_oauth_auth_passes_through_untouched() {
        let cache = TokenCache::new();
        let mut auth = AuthConfig::Bearer {
            token: "plain".into(),
        };

        prepare_auth(&cache, &mut auth).await.unwrap();

        assert_eq!(
            auth,
            AuthConfig::Bearer {
                token: "plain".into()
            }
        );
    }

    #[test]
    fn a_pinned_loopback_port_is_honoured_and_anything_else_is_not() {
        assert_eq!(fixed_port("http://127.0.0.1:8723/callback"), 8723);
        assert_eq!(fixed_port("http://localhost:9000/cb"), 9000);
        assert_eq!(fixed_port(""), 0);
        assert_eq!(fixed_port("http://127.0.0.1/callback"), 0);
        // Not loopback: binding a port because a remote URI named one
        // would be pointless, and the OS-chosen port is the safe default.
        assert_eq!(fixed_port("https://example.test:443/callback"), 0);
    }

    #[tokio::test]
    async fn only_the_authorization_code_grant_opens_a_browser() {
        let error = begin_authorization(&config()).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Invalid);
    }

    #[tokio::test]
    async fn starting_a_flow_binds_a_port_and_builds_a_url() {
        let mut c = config();
        c.grant = OAuth2Grant::AuthorizationCode;
        c.auth_url = "https://example.test/authorize".into();

        let flow = begin_authorization(&c).unwrap().start().await.unwrap();

        assert!(
            flow.url.starts_with("https://example.test/authorize?"),
            "{}",
            flow.url
        );
        assert!(
            flow.url.contains("code_challenge="),
            "PKCE should be on by default"
        );
        assert!(flow.url.contains("code_challenge_method=S256"));
        assert!(flow.redirect_uri().starts_with("http://127.0.0.1:"));
        assert!(flow.url.contains("state="));
    }

    #[tokio::test]
    async fn two_flows_get_different_state() {
        let mut c = config();
        c.grant = OAuth2Grant::AuthorizationCode;
        c.auth_url = "https://example.test/authorize".into();

        let a = begin_authorization(&c).unwrap().start().await.unwrap();
        let b = begin_authorization(&c).unwrap().start().await.unwrap();

        assert_ne!(a.state, b.state);
    }
}
