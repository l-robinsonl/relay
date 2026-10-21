//! Construction and caching of `reqwest` clients.

use relay_models::{ClientCertificate, ProxyConfig, RedirectPolicy, RequestSettings};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::error::EngineError;

/// The subset of [`RequestSettings`] that affects client construction.
///
/// A `reqwest::Client` owns a connection pool, so rebuilding one per request
/// would discard every keep-alive connection and TLS session. Instead clients
/// are cached under this key: requests that differ only in URL, headers or
/// body share a client and therefore share connections.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct ClientKey {
    verify_tls: bool,
    redirect: Option<u32>,
    decompress: bool,
    cookies: bool,
    proxy: Option<String>,
    proxy_user: Option<String>,
    cert_path: Option<String>,
    timeout_ms: Option<u64>,
}

impl ClientKey {
    pub(crate) fn from_settings(settings: &RequestSettings) -> Self {
        Self {
            verify_tls: settings.verify_tls,
            redirect: match settings.redirect_policy {
                RedirectPolicy::Never => None,
                RedirectPolicy::Follow => Some(settings.max_redirects),
            },
            decompress: settings.decompress,
            cookies: settings.store_cookies || settings.send_cookies,
            proxy: settings.proxy.as_ref().map(|p| p.url.clone()),
            proxy_user: settings.proxy.as_ref().and_then(|p| p.username.clone()),
            cert_path: settings
                .client_certificate
                .as_ref()
                .map(|c| c.pem_path.clone()),
            timeout_ms: settings.timeout_ms,
        }
    }
}

/// Builds and caches HTTP clients keyed by the settings that shape them.
#[derive(Clone)]
pub struct ClientPool {
    clients: Arc<Mutex<HashMap<ClientKey, reqwest::Client>>>,
    /// Shared across every client so that a cookie set by one request is
    /// visible to the next, which is what makes login-then-call flows work.
    ///
    /// Behind a mutex because clearing cookies means replacing the jar:
    /// `reqwest`'s `Jar` can be added to but not emptied.
    cookie_jar: Arc<Mutex<Arc<reqwest::cookie::Jar>>>,
    default_timeout: Duration,
    user_agent: String,
}

impl ClientPool {
    /// A pool with the given default timeout and `User-Agent`.
    pub fn new(default_timeout: Duration, user_agent: impl Into<String>) -> Self {
        Self {
            clients: Arc::new(Mutex::new(HashMap::new())),
            cookie_jar: Arc::new(Mutex::new(Arc::new(reqwest::cookie::Jar::default()))),
            default_timeout,
            user_agent: user_agent.into(),
        }
    }

    /// The shared cookie jar.
    pub fn cookie_jar(&self) -> Arc<reqwest::cookie::Jar> {
        match self.cookie_jar.lock() {
            Ok(guard) => Arc::clone(&guard),
            Err(poisoned) => Arc::clone(&poisoned.into_inner()),
        }
    }

    /// Discards every stored cookie.
    ///
    /// `reqwest::cookie::Jar` has no way to remove anything, so this
    /// swaps in a fresh one and drops the pooled clients that were built
    /// around the old jar. Without dropping them, existing connections
    /// would keep sending the cookies the user just asked to forget.
    pub fn clear_cookies(&self) {
        if let Ok(mut guard) = self.cookie_jar.lock() {
            *guard = Arc::new(reqwest::cookie::Jar::default());
        }
        self.clear();
    }

    /// Discards every pooled client, dropping their connections.
    pub fn clear(&self) {
        if let Ok(mut guard) = self.clients.lock() {
            guard.clear();
        }
    }

    /// Returns a client matching `settings`, building one if necessary.
    pub fn get(&self, settings: &RequestSettings) -> Result<reqwest::Client, EngineError> {
        let key = ClientKey::from_settings(settings);

        if let Ok(guard) = self.clients.lock()
            && let Some(client) = guard.get(&key)
        {
            return Ok(client.clone());
        }

        let client = self.build(settings)?;

        if let Ok(mut guard) = self.clients.lock() {
            // Another thread may have inserted while we were building; either
            // client is equivalent, so keep whichever landed first.
            return Ok(guard.entry(key).or_insert(client).clone());
        }

        Ok(client)
    }

    fn build(&self, settings: &RequestSettings) -> Result<reqwest::Client, EngineError> {
        let mut builder = reqwest::Client::builder()
            .user_agent(self.user_agent.clone())
            .cookie_provider(self.cookie_jar())
            // Connection timeouts are separate from the overall deadline so a
            // dead host fails quickly rather than burning the whole budget.
            .connect_timeout(Duration::from_secs(30))
            .pool_idle_timeout(Duration::from_secs(90))
            .tcp_nodelay(true);

        builder = match settings.redirect_policy {
            RedirectPolicy::Never => builder.redirect(reqwest::redirect::Policy::none()),
            RedirectPolicy::Follow => builder.redirect(reqwest::redirect::Policy::limited(
                settings.max_redirects as usize,
            )),
        };

        // `None` here means no overall deadline; the engine still enforces one
        // via its own timeout so cancellation stays uniform.
        if let Some(ms) = settings.timeout_ms {
            builder = builder.timeout(Duration::from_millis(ms));
        } else {
            builder = builder.timeout(self.default_timeout);
        }

        if !settings.decompress {
            builder = builder.no_gzip().no_brotli().no_deflate().no_zstd();
        }

        if !settings.verify_tls {
            // Opt-in only, per request, surfaced prominently in the UI. The
            // warning is logged so that an unexpected one is traceable.
            tracing::warn!(
                "TLS certificate verification disabled for this request by explicit user setting"
            );
            builder = builder
                .danger_accept_invalid_certs(true)
                .danger_accept_invalid_hostnames(true);
        }

        if let Some(proxy) = &settings.proxy {
            builder = builder.proxy(build_proxy(proxy)?);
        }

        if let Some(cert) = &settings.client_certificate {
            builder = builder.identity(load_identity(cert)?);
        }

        builder
            .build()
            .map_err(|e| EngineError::ClientBuild(e.to_string()))
    }
}

fn build_proxy(config: &ProxyConfig) -> Result<reqwest::Proxy, EngineError> {
    let mut proxy =
        reqwest::Proxy::all(&config.url).map_err(|e| EngineError::InvalidProxy(e.to_string()))?;

    if let Some(user) = &config.username {
        proxy = proxy.basic_auth(user, config.password.as_deref().unwrap_or_default());
    }

    if !config.bypass.is_empty() {
        let bypass: Vec<String> = config.bypass.iter().map(|s| s.to_lowercase()).collect();
        proxy = proxy.no_proxy(reqwest::NoProxy::from_string(&bypass.join(",")));
    }

    Ok(proxy)
}

fn load_identity(cert: &ClientCertificate) -> Result<reqwest::Identity, EngineError> {
    let pem = std::fs::read(&cert.pem_path).map_err(|e| {
        EngineError::ClientCertificate(format!("cannot read {}: {e}", cert.pem_path))
    })?;

    reqwest::Identity::from_pem(&pem)
        .map_err(|e| EngineError::ClientCertificate(format!("invalid PEM identity: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clients_are_reused_for_equivalent_settings() {
        let pool = ClientPool::new(Duration::from_secs(30), "relay-test");
        let a = pool.get(&RequestSettings::default()).unwrap();
        let b = pool.get(&RequestSettings::default()).unwrap();
        // reqwest::Client is an Arc internally; the pool must hand back the
        // same one so the connection pool survives between requests.
        assert_eq!(Arc::strong_count(&pool.clients), 1);
        drop((a, b));
        assert_eq!(pool.clients.lock().unwrap().len(), 1);
    }

    #[test]
    fn differing_tls_settings_get_separate_clients() {
        let pool = ClientPool::new(Duration::from_secs(30), "relay-test");
        let secure = RequestSettings::default();
        let insecure = RequestSettings {
            verify_tls: false,
            ..Default::default()
        };
        pool.get(&secure).unwrap();
        pool.get(&insecure).unwrap();
        assert_eq!(pool.clients.lock().unwrap().len(), 2);
    }

    #[test]
    fn client_key_ignores_irrelevant_settings() {
        // max_response_bytes is enforced by the engine, not the client, so it
        // must not fragment the pool.
        let a = RequestSettings::default();
        let b = RequestSettings {
            max_response_bytes: Some(1024),
            ..Default::default()
        };
        assert_eq!(ClientKey::from_settings(&a), ClientKey::from_settings(&b));
    }

    #[test]
    fn clearing_cookies_also_drops_the_clients_holding_the_old_jar() {
        // A pooled client keeps a reference to the jar it was built with,
        // so leaving it in place would keep sending cookies the user just
        // cleared.
        let pool = ClientPool::new(Duration::from_secs(30), "relay-test");
        pool.get(&RequestSettings::default()).unwrap();
        assert_eq!(pool.clients.lock().unwrap().len(), 1);

        let before = pool.cookie_jar();
        pool.clear_cookies();

        assert_eq!(pool.clients.lock().unwrap().len(), 0);
        assert!(
            !Arc::ptr_eq(&before, &pool.cookie_jar()),
            "the jar was reused rather than replaced"
        );
    }

    #[test]
    fn an_invalid_proxy_url_is_rejected() {
        let pool = ClientPool::new(Duration::from_secs(5), "relay-test");
        let settings = RequestSettings {
            proxy: Some(ProxyConfig {
                url: "not a url".into(),
                username: None,
                password: None,
                bypass: vec![],
            }),
            ..Default::default()
        };
        assert!(pool.get(&settings).is_err());
    }

    #[test]
    fn a_missing_client_certificate_is_reported_clearly() {
        let pool = ClientPool::new(Duration::from_secs(5), "relay-test");
        let settings = RequestSettings {
            client_certificate: Some(ClientCertificate {
                hosts: vec![],
                pem_path: "/nonexistent/relay-test.pem".into(),
                passphrase: None,
            }),
            ..Default::default()
        };
        let err = pool.get(&settings).unwrap_err();
        assert!(matches!(err, EngineError::ClientCertificate(_)));
    }
}
