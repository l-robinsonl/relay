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

/// True when a certificate should be presented to this host.
///
/// An empty host list means every host, which is the common case: the
/// setting is already per-request. Naming hosts is for a certificate
/// shared by a folder of requests, where it should go to the internal
/// service and not to the public API next to it.
fn certificate_applies(cert: &ClientCertificate, host: Option<&str>) -> bool {
    if cert.hosts.is_empty() {
        return true;
    }

    let Some(host) = host else {
        return false;
    };

    cert.hosts.iter().any(|pattern| {
        let pattern = pattern.trim().trim_start_matches('.');
        if pattern.is_empty() {
            return false;
        }
        // `example.com` covers `api.example.com`, as a certificate
        // scoped to an internal domain is meant to.
        host.eq_ignore_ascii_case(pattern)
            || host.len() > pattern.len()
                && host.as_bytes()[host.len() - pattern.len() - 1] == b'.'
                && host[host.len() - pattern.len()..].eq_ignore_ascii_case(pattern)
    })
}

impl ClientKey {
    pub(crate) fn from_settings(settings: &RequestSettings, host: Option<&str>) -> Self {
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
            // A certificate that does not apply to this host is not part
            // of the key: those requests share one client, rather than
            // each host getting a connection pool of its own.
            cert_path: settings
                .client_certificate
                .as_ref()
                .filter(|c| certificate_applies(c, host))
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
    pub fn get(
        &self,
        settings: &RequestSettings,
        host: Option<&str>,
    ) -> Result<reqwest::Client, EngineError> {
        let key = ClientKey::from_settings(settings, host);

        if let Ok(guard) = self.clients.lock()
            && let Some(client) = guard.get(&key)
        {
            return Ok(client.clone());
        }

        let client = self.build(settings, host)?;

        if let Ok(mut guard) = self.clients.lock() {
            // Another thread may have inserted while we were building; either
            // client is equivalent, so keep whichever landed first.
            return Ok(guard.entry(key).or_insert(client).clone());
        }

        Ok(client)
    }

    fn build(
        &self,
        settings: &RequestSettings,
        host: Option<&str>,
    ) -> Result<reqwest::Client, EngineError> {
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

        if let Some(cert) = &settings.client_certificate
            && certificate_applies(cert, host)
        {
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

    // An encrypted key fails inside rustls with a message about PKCS#8
    // parsing, which sends people looking in the wrong place entirely.
    if is_encrypted(&pem) {
        return Err(EngineError::ClientCertificate(format!(
            "the private key in {} is encrypted, and Relay cannot ask for its \
             passphrase. Decrypt it first: \
             openssl pkey -in key.pem -out key-decrypted.pem",
            cert.pem_path
        )));
    }

    reqwest::Identity::from_pem(&pem)
        .map_err(|e| EngineError::ClientCertificate(format!("invalid PEM identity: {e}")))
}

/// True when the PEM holds a passphrase-protected private key.
fn is_encrypted(pem: &[u8]) -> bool {
    let text = String::from_utf8_lossy(pem);
    // PKCS#8 encrypted keys, and the older OpenSSL per-key header.
    text.contains("ENCRYPTED PRIVATE KEY") || text.contains("Proc-Type: 4,ENCRYPTED")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clients_are_reused_for_equivalent_settings() {
        let pool = ClientPool::new(Duration::from_secs(30), "relay-test");
        let a = pool.get(&RequestSettings::default(), None).unwrap();
        let b = pool.get(&RequestSettings::default(), None).unwrap();
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
        pool.get(&secure, None).unwrap();
        pool.get(&insecure, None).unwrap();
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
        assert_eq!(
            ClientKey::from_settings(&a, None),
            ClientKey::from_settings(&b, None)
        );
    }

    #[test]
    fn clearing_cookies_also_drops_the_clients_holding_the_old_jar() {
        // A pooled client keeps a reference to the jar it was built with,
        // so leaving it in place would keep sending cookies the user just
        // cleared.
        let pool = ClientPool::new(Duration::from_secs(30), "relay-test");
        pool.get(&RequestSettings::default(), None).unwrap();
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
        assert!(pool.get(&settings, None).is_err());
    }

    fn certificate_for(hosts: &[&str]) -> ClientCertificate {
        ClientCertificate {
            hosts: hosts.iter().map(|h| h.to_string()).collect(),
            pem_path: "/tmp/client.pem".into(),
        }
    }

    #[test]
    fn a_certificate_with_no_hosts_goes_everywhere() {
        // The usual case: the setting is already per-request, so naming
        // a host adds nothing unless the setting is shared.
        let cert = certificate_for(&[]);
        assert!(certificate_applies(&cert, Some("api.example.com")));
        assert!(certificate_applies(&cert, None));
    }

    #[test]
    fn a_named_host_matches_itself_and_its_subdomains() {
        let cert = certificate_for(&["internal.example.com"]);

        assert!(certificate_applies(&cert, Some("internal.example.com")));
        assert!(certificate_applies(&cert, Some("api.internal.example.com")));
        assert!(certificate_applies(&cert, Some("INTERNAL.EXAMPLE.COM")));
    }

    #[test]
    fn a_host_that_merely_ends_with_the_name_does_not_match() {
        // `notinternal.example.com` is a different host, and an attacker
        // who can register one should not be handed a client
        // certificate because of a suffix comparison.
        let cert = certificate_for(&["internal.example.com"]);

        assert!(!certificate_applies(&cert, Some("notinternal.example.com")));
        assert!(!certificate_applies(&cert, Some("example.com")));
        assert!(!certificate_applies(&cert, Some("evil.com")));
        assert!(!certificate_applies(&cert, None));
    }

    #[test]
    fn a_leading_dot_is_accepted_as_people_write_it() {
        let cert = certificate_for(&[".example.com"]);
        assert!(certificate_applies(&cert, Some("api.example.com")));
        assert!(certificate_applies(&cert, Some("example.com")));
    }

    #[test]
    fn a_certificate_that_does_not_apply_is_not_part_of_the_cache_key() {
        // Otherwise every host would get a connection pool of its own
        // even though none of them present a certificate.
        let settings = RequestSettings {
            client_certificate: Some(certificate_for(&["internal.example.com"])),
            ..Default::default()
        };

        let inside = ClientKey::from_settings(&settings, Some("api.internal.example.com"));
        let outside = ClientKey::from_settings(&settings, Some("api.public.com"));
        let none = ClientKey::from_settings(&RequestSettings::default(), Some("api.public.com"));

        assert_ne!(inside, outside);
        assert_eq!(outside, none);
    }

    #[test]
    fn an_encrypted_private_key_says_how_to_fix_it() {
        // Otherwise rustls reports a PKCS#8 parsing failure, which sends
        // people looking at their certificate rather than their key.
        let dir = std::env::temp_dir().join(format!("relay-cert-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("encrypted.pem");
        std::fs::write(
            &path,
            "-----BEGIN ENCRYPTED PRIVATE KEY-----\nMIIB\n-----END ENCRYPTED PRIVATE KEY-----\n",
        )
        .unwrap();

        let cert = ClientCertificate {
            hosts: Vec::new(),
            pem_path: path.to_string_lossy().into_owned(),
        };

        let error = load_identity(&cert).unwrap_err().to_string();
        assert!(error.contains("encrypted"), "{error}");
        assert!(
            error.contains("openssl pkey"),
            "it should say how to fix it: {error}"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_missing_client_certificate_is_reported_clearly() {
        let pool = ClientPool::new(Duration::from_secs(5), "relay-test");
        let settings = RequestSettings {
            client_certificate: Some(ClientCertificate {
                hosts: vec![],
                pem_path: "/nonexistent/relay-test.pem".into(),
            }),
            ..Default::default()
        };
        let err = pool.get(&settings, None).unwrap_err();
        assert!(matches!(err, EngineError::ClientCertificate(_)));
    }
}
