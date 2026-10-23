//! Configuration, entirely from the environment.
//!
//! A self-hoster should be able to run this with a handful of environment
//! variables and no config file to mount. Everything has a defensible
//! default except the things that genuinely cannot have one.

use std::net::SocketAddr;
use std::time::Duration;

use crate::error::{ServerError, ServerResult};

/// Who may create an account.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignupMode {
    /// Nobody; an administrator creates every account.
    Off,
    /// Only with a valid invitation.
    Invite,
    /// Anyone who can reach the server.
    Open,
}

impl SignupMode {
    fn parse(value: &str) -> ServerResult<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "off" | "closed" => Ok(Self::Off),
            "invite" | "invitation" => Ok(Self::Invite),
            "open" | "public" => Ok(Self::Open),
            other => Err(ServerError::Config(format!(
                "RELAY_SIGNUP_MODE must be off, invite or open; got '{other}'"
            ))),
        }
    }
}

/// The server's settings.
#[derive(Debug, Clone)]
pub struct Config {
    /// `sqlite://…` or `postgres://…`.
    pub database_url: String,
    /// Where to listen.
    pub bind: SocketAddr,
    /// The externally reachable origin, used in e-mailed links.
    pub public_url: String,
    /// Who may register.
    pub signup_mode: SignupMode,
    /// How long a session lasts.
    pub session_ttl: Duration,
    /// The bootstrap administrator, consumed on first start only.
    pub admin_email: Option<String>,
    /// The bootstrap administrator's password.
    pub admin_password: Option<String>,
    /// Largest request body accepted, in bytes.
    pub max_body_bytes: usize,
    /// Resend API key. Absent means e-mail is logged, not sent.
    pub resend_api_key: Option<String>,
    /// The `From:` address, which the provider must have verified.
    pub mail_from: Option<String>,
    /// Stripe's secret key. Absent means billing is off entirely and
    /// every workspace is unmetered — which is what self-hosting is.
    pub stripe_secret_key: Option<String>,
    /// The secret webhooks are signed with.
    pub stripe_webhook_secret: Option<String>,
    /// The per-seat price to charge.
    pub stripe_price_id: Option<String>,
    /// Where the customer portal sends people back to.
    pub stripe_return_url: Option<String>,
    /// Whether an unverified account may sign in.
    ///
    /// Off by default so a self-hosted server with no mail provider
    /// works out of the box. Relay Cloud turns it on.
    pub require_verified_email: bool,
}

impl Config {
    /// Reads the configuration from the process environment.
    pub fn from_env() -> ServerResult<Self> {
        let database_url = std::env::var("RELAY_DATABASE_URL")
            .unwrap_or_else(|_| "sqlite://data/relay.db".to_string());

        let bind = std::env::var("RELAY_BIND")
            .unwrap_or_else(|_| "0.0.0.0:8787".to_string())
            .parse()
            .map_err(|e| ServerError::Config(format!("RELAY_BIND is not an address: {e}")))?;

        let signup_mode = SignupMode::parse(
            &std::env::var("RELAY_SIGNUP_MODE").unwrap_or_else(|_| "invite".to_string()),
        )?;

        let session_days: u64 = std::env::var("RELAY_SESSION_DAYS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(30);

        Ok(Self {
            database_url,
            bind,
            public_url: std::env::var("RELAY_PUBLIC_URL")
                .unwrap_or_else(|_| format!("http://{bind}")),
            signup_mode,
            session_ttl: Duration::from_secs(session_days * 24 * 60 * 60),
            admin_email: non_empty("RELAY_ADMIN_EMAIL"),
            admin_password: non_empty("RELAY_ADMIN_PASSWORD"),
            max_body_bytes: relay_sync::MAX_BATCH_BYTES + 64 * 1024,
            // Both spellings: every other Relay variable is prefixed, but
            // `RESEND_API_KEY` is what the provider's own dashboard and
            // most deployment platforms hand you, and silently ignoring
            // it would look exactly like a broken key.
            resend_api_key: non_empty("RELAY_RESEND_API_KEY")
                .or_else(|| non_empty("RESEND_API_KEY")),
            mail_from: non_empty("RELAY_MAIL_FROM"),
            require_verified_email: flag("RELAY_REQUIRE_VERIFIED_EMAIL", false),
            stripe_secret_key: non_empty("STRIPE_SECRET_KEY"),
            stripe_webhook_secret: non_empty("STRIPE_WEBHOOK_SECRET"),
            stripe_price_id: non_empty("STRIPE_PRICE_SEAT_ANNUAL"),
            stripe_return_url: non_empty("STRIPE_PORTAL_RETURN_URL"),
        })
    }
}

/// Settings a test can start from.
///
/// Every fixture used to spell out all sixteen fields, so adding one
/// broke four files in two crates and taught nothing. `from_env` stays
/// an exhaustive literal — a new setting must still be read from the
/// environment deliberately — but a test may say only what it cares
/// about.
impl Default for Config {
    fn default() -> Self {
        Self {
            database_url: "sqlite::memory:".into(),
            bind: ([127, 0, 0, 1], 0).into(),
            public_url: "http://127.0.0.1".into(),
            // Invite, matching `from_env`: a default that failed open
            // would be the wrong thing to copy into a fixture.
            signup_mode: SignupMode::Invite,
            session_ttl: Duration::from_secs(30 * 24 * 60 * 60),
            admin_email: None,
            admin_password: None,
            max_body_bytes: relay_sync::MAX_BATCH_BYTES + 64 * 1024,
            resend_api_key: None,
            mail_from: None,
            require_verified_email: false,
            stripe_secret_key: None,
            stripe_webhook_secret: None,
            stripe_price_id: None,
            stripe_return_url: None,
        }
    }
}

impl Config {
    /// True when this deployment charges for seats.
    ///
    /// Both halves are required: a key with no webhook secret would
    /// take payments and never hear that they succeeded, which is a
    /// worse state than not charging at all.
    pub fn bills(&self) -> bool {
        self.stripe_secret_key.is_some()
            && self.stripe_webhook_secret.is_some()
            && self.stripe_price_id.is_some()
    }

    /// True when the configured database is PostgreSQL.
    pub fn is_postgres(&self) -> bool {
        let url = self.database_url.trim_start();
        url.starts_with("postgres://") || url.starts_with("postgresql://")
    }
}

/// Reads a boolean variable, accepting the spellings people actually use.
fn flag(key: &str, default: bool) -> bool {
    match non_empty(key) {
        None => default,
        Some(value) => matches!(
            value.to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        ),
    }
}

/// Reads a variable, treating blank as absent.
///
/// Docker Compose and systemd both hand through empty strings for unset
/// values, and an empty admin password must not look like a configured one.
fn non_empty(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signup_modes_parse_and_reject_nonsense() {
        assert_eq!(SignupMode::parse("open").unwrap(), SignupMode::Open);
        assert_eq!(SignupMode::parse(" Invite ").unwrap(), SignupMode::Invite);
        assert_eq!(SignupMode::parse("OFF").unwrap(), SignupMode::Off);

        let err = SignupMode::parse("maybe").unwrap_err();
        assert!(err.to_string().contains("off, invite or open"));
    }

    #[test]
    fn postgres_urls_are_recognised_by_either_scheme() {
        let base = |url: &str| Config {
            database_url: url.to_string(),
            bind: "127.0.0.1:0".parse().unwrap(),
            public_url: String::new(),
            signup_mode: SignupMode::Off,
            session_ttl: Duration::from_secs(1),
            admin_email: None,
            admin_password: None,
            max_body_bytes: 1024,
            resend_api_key: None,
            mail_from: None,
            require_verified_email: false,
            stripe_secret_key: None,
            stripe_webhook_secret: None,
            stripe_price_id: None,
            stripe_return_url: None,
        };

        assert!(base("postgres://host/db").is_postgres());
        assert!(base("postgresql://host/db").is_postgres());
        assert!(!base("sqlite://relay.db").is_postgres());
    }
}
