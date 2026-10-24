//! A small fixed-window rate limiter for the routes that cost
//! something to abuse.
//!
//! Not general protection — a determined flood still needs something
//! in front of the server. This is aimed at the specific thing an
//! open signup page invites: somebody creating accounts in a loop, or
//! grinding passwords, or using the verification endpoint to post
//! e-mail at strangers.
//!
//! In process and in memory, so it resets when the server restarts
//! and does not coordinate between instances. That is the right
//! trade for one small deployment and the wrong one for several; a
//! shared store is the upgrade when there is a second instance.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::{ConnectInfo, Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::app::AppState;

/// How many requests a caller gets, and over how long.
#[derive(Debug, Clone, Copy)]
pub struct Quota {
    pub requests: u32,
    pub per: Duration,
}

impl Quota {
    /// Creating accounts. Tight: there is no legitimate reason to
    /// make six accounts from one address in an hour, and this is
    /// the endpoint that costs money to abuse.
    pub const SIGNUP: Self = Self {
        requests: 5,
        per: Duration::from_secs(3600),
    };

    /// Signing in. Looser, because a shared office address is one
    /// caller as far as this can tell, and locking a building out of
    /// its own workspaces would be a worse failure than a slow
    /// password grind.
    pub const SIGN_IN: Self = Self {
        requests: 30,
        per: Duration::from_secs(300),
    };

    /// Anything that sends an e-mail on request. Abuse here lands in
    /// somebody else's inbox and on the sending domain's reputation.
    pub const SENDS_MAIL: Self = Self {
        requests: 5,
        per: Duration::from_secs(3600),
    };
}

/// One caller's usage of one quota.
#[derive(Debug, Clone, Copy)]
struct Window {
    started: Instant,
    count: u32,
}

/// The limiter itself.
#[derive(Clone, Default)]
pub struct RateLimiter {
    windows: Arc<Mutex<HashMap<(String, &'static str), Window>>>,
}

impl std::fmt::Debug for RateLimiter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RateLimiter").finish_non_exhaustive()
    }
}

impl RateLimiter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Takes one request from a caller's allowance.
    ///
    /// Returns how long they must wait when there is nothing left.
    pub fn check(&self, caller: &str, bucket: &'static str, quota: Quota) -> Result<(), Duration> {
        self.check_at(caller, bucket, quota, Instant::now())
    }

    /// The same, at a given instant, so the behaviour is testable
    /// without sleeping through an hour.
    pub fn check_at(
        &self,
        caller: &str,
        bucket: &'static str,
        quota: Quota,
        now: Instant,
    ) -> Result<(), Duration> {
        let mut windows = self.windows.lock().unwrap_or_else(|e| e.into_inner());

        // Opportunistic pruning. Without it the map grows by one entry
        // per address for ever, which is a memory leak an attacker
        // chooses the size of.
        if windows.len() > 4096 {
            windows.retain(|_, w| now.duration_since(w.started) < Duration::from_secs(3600));
        }

        let key = (caller.to_string(), bucket);
        let window = windows.entry(key).or_insert(Window {
            started: now,
            count: 0,
        });

        if now.duration_since(window.started) >= quota.per {
            *window = Window {
                started: now,
                count: 0,
            };
        }

        if window.count >= quota.requests {
            return Err(quota.per - now.duration_since(window.started));
        }

        window.count += 1;
        Ok(())
    }
}

/// Who is calling, for rate-limiting purposes.
///
/// Behind a proxy the peer address is the proxy, so every caller
/// would share one allowance and the first person to sign up would
/// lock out the rest. `X-Forwarded-For` has the real address — but
/// only when something trustworthy set it. A client can send the
/// header itself, so trusting it on a directly-exposed server hands
/// out an unlimited supply of identities and turns the limiter off.
///
/// Hence the configuration switch, and hence it defaulting to off.
pub fn caller(headers: &HeaderMap, peer: Option<SocketAddr>, trust_forwarded: bool) -> String {
    if trust_forwarded {
        if let Some(forwarded) = headers
            .get("x-forwarded-for")
            .and_then(|v| v.to_str().ok())
            // Leftmost: the original client. Everything after it is
            // the chain of proxies that handled the request.
            .and_then(|v| v.split(',').next())
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .and_then(|v| v.parse::<IpAddr>().ok())
        {
            return forwarded.to_string();
        }
    }

    peer.map(|p| p.ip().to_string())
        // No peer address and no trusted header: everybody shares one
        // allowance. Degraded, but the failure is a limit that is too
        // strict rather than one that does nothing.
        .unwrap_or_else(|| "unknown".to_string())
}

/// Middleware applying a quota to whatever it wraps.
pub async fn limit(
    quota: Quota,
    bucket: &'static str,
    State(state): State<AppState>,
    connect: Option<ConnectInfo<SocketAddr>>,
    request: Request,
    next: Next,
) -> Response {
    let who = caller(
        request.headers(),
        connect.map(|ConnectInfo(addr)| addr),
        state.config.trust_forwarded_for,
    );

    match state.limiter.check(&who, bucket, quota) {
        Ok(()) => next.run(request).await,
        Err(wait) => {
            tracing::warn!(caller = %who, bucket, "rate limited");
            (
                StatusCode::TOO_MANY_REQUESTS,
                [("retry-after", wait.as_secs().max(1).to_string())],
                axum::Json(serde_json::json!({
                    "error": "tooManyRequests",
                    "message": "Too many attempts from this address. Try again shortly.",
                })),
            )
                .into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_quota_allows_exactly_what_it_says() {
        let limiter = RateLimiter::new();
        let quota = Quota {
            requests: 3,
            per: Duration::from_secs(60),
        };
        let now = Instant::now();

        for attempt in 0..3 {
            assert!(
                limiter.check_at("1.2.3.4", "signup", quota, now).is_ok(),
                "attempt {attempt} should have been allowed"
            );
        }
        assert!(limiter.check_at("1.2.3.4", "signup", quota, now).is_err());
    }

    #[test]
    fn the_window_moves_on() {
        let limiter = RateLimiter::new();
        let quota = Quota {
            requests: 1,
            per: Duration::from_secs(60),
        };
        let now = Instant::now();

        assert!(limiter.check_at("1.2.3.4", "signup", quota, now).is_ok());
        assert!(limiter.check_at("1.2.3.4", "signup", quota, now).is_err());

        let later = now + Duration::from_secs(61);
        assert!(limiter.check_at("1.2.3.4", "signup", quota, later).is_ok());
    }

    #[test]
    fn callers_and_buckets_do_not_share_an_allowance() {
        let limiter = RateLimiter::new();
        let quota = Quota {
            requests: 1,
            per: Duration::from_secs(60),
        };
        let now = Instant::now();

        assert!(limiter.check_at("1.2.3.4", "signup", quota, now).is_ok());
        // A different address is a different caller.
        assert!(limiter.check_at("5.6.7.8", "signup", quota, now).is_ok());
        // And running out of signups must not stop them signing in.
        assert!(limiter.check_at("1.2.3.4", "signin", quota, now).is_ok());
    }

    #[test]
    fn a_forwarded_header_is_ignored_unless_it_is_trusted() {
        // The whole point. A client can send this header itself, so
        // honouring it on a directly-exposed server would hand out an
        // unlimited supply of identities and turn the limiter off.
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", "9.9.9.9".parse().unwrap());
        let peer: SocketAddr = "1.2.3.4:5000".parse().unwrap();

        assert_eq!(caller(&headers, Some(peer), false), "1.2.3.4");
        assert_eq!(caller(&headers, Some(peer), true), "9.9.9.9");
    }

    #[test]
    fn the_leftmost_forwarded_address_is_the_client() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-forwarded-for",
            "9.9.9.9, 10.0.0.1, 10.0.0.2".parse().unwrap(),
        );

        assert_eq!(caller(&headers, None, true), "9.9.9.9");
    }

    #[test]
    fn rubbish_in_the_header_falls_back_rather_than_trusting_it() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", "not-an-address".parse().unwrap());
        let peer: SocketAddr = "1.2.3.4:5000".parse().unwrap();

        assert_eq!(caller(&headers, Some(peer), true), "1.2.3.4");
    }
}
