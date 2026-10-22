//! Transactional e-mail, behind a trait.
//!
//! A self-hosted Relay should not need an e-mail provider to work. So
//! sending sits behind [`Mailer`], the default implementation writes the
//! message to the log instead of sending it, and nothing in the sync
//! path depends on delivery succeeding. Relay Cloud configures
//! [`ResendMailer`]; a self-hoster who wants verification e-mails can
//! configure the same thing, and one who does not gets a server that
//! still works.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use crate::error::{ServerError, ServerResult};

/// One message to one recipient.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Email {
    /// Who it goes to.
    pub to: String,
    /// The subject line.
    pub subject: String,
    /// The plain-text body. Every message Relay sends is plain text:
    /// there is nothing here that HTML would make clearer, and a
    /// plain-text mail cannot carry a tracking pixel or a hidden link.
    pub body: String,
}

/// Somewhere to send mail.
#[async_trait]
pub trait Mailer: Send + Sync + 'static {
    /// Sends one message.
    async fn send(&self, message: &Email) -> ServerResult<()>;

    /// A human-readable name for the backend, shown at startup.
    fn name(&self) -> &'static str;

    /// Whether this mailer actually delivers.
    ///
    /// The registration flow asks, because requiring a user to click a
    /// link in an e-mail that was only ever written to a log file would
    /// lock them out of their own server.
    fn delivers(&self) -> bool {
        true
    }
}

/// Writes messages to the log rather than sending them.
///
/// The default, so that a self-hosted server runs with no e-mail
/// configuration at all. The body is logged in full: on a server the
/// operator controls, that is how they retrieve a verification link,
/// and it is strictly less exposure than sending it to a third party.
#[derive(Debug, Default, Clone)]
pub struct LogMailer;

#[async_trait]
impl Mailer for LogMailer {
    async fn send(&self, message: &Email) -> ServerResult<()> {
        tracing::info!(
            to = %message.to,
            subject = %message.subject,
            "e-mail not sent — no mail provider is configured. Body follows:\n{}",
            message.body
        );
        Ok(())
    }

    fn name(&self) -> &'static str {
        "log"
    }

    fn delivers(&self) -> bool {
        false
    }
}

/// Sends through Resend's HTTP API.
pub struct ResendMailer {
    client: reqwest::Client,
    api_key: String,
    from: String,
}

impl std::fmt::Debug for ResendMailer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // No api_key field: this is the kind of struct that ends up in a
        // panic message or a tracing span.
        f.debug_struct("ResendMailer")
            .field("from", &self.from)
            .finish_non_exhaustive()
    }
}

impl ResendMailer {
    /// Builds a mailer. `from` must be an address on a domain verified
    /// with Resend, or every send is rejected.
    pub fn new(api_key: impl Into<String>, from: impl Into<String>) -> ServerResult<Self> {
        let api_key = api_key.into();
        let from = from.into();

        if api_key.trim().is_empty() {
            return Err(ServerError::Config(
                "RELAY_RESEND_API_KEY is empty".to_string(),
            ));
        }
        if from.trim().is_empty() {
            return Err(ServerError::Config(
                "RELAY_MAIL_FROM must be set when a mail provider is configured".to_string(),
            ));
        }

        let client = reqwest::Client::builder()
            // A slow provider must not hold a request handler open.
            .timeout(Duration::from_secs(15))
            .build()
            .map_err(|e| ServerError::Config(format!("could not build the mail client: {e}")))?;

        Ok(Self {
            client,
            api_key,
            from,
        })
    }
}

#[async_trait]
impl Mailer for ResendMailer {
    async fn send(&self, message: &Email) -> ServerResult<()> {
        let response = self
            .client
            .post("https://api.resend.com/emails")
            .bearer_auth(&self.api_key)
            .json(&serde_json::json!({
                "from": self.from,
                "to": [message.to],
                "subject": message.subject,
                "text": message.body,
            }))
            .send()
            .await
            .map_err(|e| ServerError::Mail(format!("provider unreachable: {e}")))?;

        let status = response.status();
        if status.is_success() {
            return Ok(());
        }

        // Read the body for the log: Resend explains refusals usefully
        // ("domain not verified", "invalid from"), and an operator
        // debugging this needs it. It never reaches the client.
        let detail = response.text().await.unwrap_or_default();
        Err(ServerError::Mail(format!("{status} {detail}")))
    }

    fn name(&self) -> &'static str {
        "resend"
    }
}

/// Keeps messages in memory instead of sending them.
///
/// For tests, and for anyone who wants to exercise the flows without a
/// provider. Public rather than `#[cfg(test)]` because the integration
/// suite lives outside this crate and has to read what was "sent".
#[derive(Debug, Default)]
pub struct CapturingMailer {
    sent: std::sync::Mutex<Vec<Email>>,
}

impl CapturingMailer {
    /// An empty mailbox.
    pub fn new() -> Self {
        Self::default()
    }

    /// Everything sent so far, oldest first.
    pub fn sent(&self) -> Vec<Email> {
        self.sent.lock().map(|s| s.clone()).unwrap_or_default()
    }

    /// The most recent message, if any.
    pub fn last(&self) -> Option<Email> {
        self.sent.lock().ok()?.last().cloned()
    }

    /// Forgets everything sent.
    pub fn clear(&self) {
        if let Ok(mut sent) = self.sent.lock() {
            sent.clear();
        }
    }
}

#[async_trait]
impl Mailer for CapturingMailer {
    async fn send(&self, message: &Email) -> ServerResult<()> {
        if let Ok(mut sent) = self.sent.lock() {
            sent.push(message.clone());
        }
        Ok(())
    }

    fn name(&self) -> &'static str {
        "capturing"
    }
}

/// Chooses a mailer from the configuration.
pub fn from_config(config: &crate::config::Config) -> ServerResult<Arc<dyn Mailer>> {
    match (&config.resend_api_key, &config.mail_from) {
        (Some(key), Some(from)) => Ok(Arc::new(ResendMailer::new(key, from)?)),
        (Some(_), None) => Err(ServerError::Config(
            "RELAY_RESEND_API_KEY is set but RELAY_MAIL_FROM is not; \
             Resend rejects a message with no verified sender address"
                .to_string(),
        )),
        _ => Ok(Arc::new(LogMailer)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_log_mailer_succeeds_but_says_it_does_not_deliver() {
        // Both halves matter: registration must not fail without a mail
        // provider, and must not demand a click that cannot arrive.
        let mailer = LogMailer;
        assert!(!mailer.delivers());
        assert!(
            mailer
                .send(&Email {
                    to: "ada@example.com".into(),
                    subject: "Verify".into(),
                    body: "link".into(),
                })
                .await
                .is_ok()
        );
    }

    #[test]
    fn a_resend_mailer_needs_a_key_and_a_sender() {
        assert!(ResendMailer::new("", "relay@example.com").is_err());
        assert!(ResendMailer::new("re_key", "  ").is_err());
        assert!(ResendMailer::new("re_key", "relay@example.com").is_ok());
    }

    #[test]
    fn debug_output_does_not_contain_the_api_key() {
        let mailer = ResendMailer::new("re_super_secret_key", "relay@example.com").unwrap();
        let printed = format!("{mailer:?}");

        assert!(!printed.contains("re_super_secret_key"), "{printed}");
        assert!(printed.contains("relay@example.com"));
    }

    #[tokio::test]
    async fn a_capturing_mailer_captures_what_was_sent() {
        let mailer = CapturingMailer::new();
        mailer
            .send(&Email {
                to: "ada@example.com".into(),
                subject: "Verify your address".into(),
                body: "https://relay.test/verify?token=abc".into(),
            })
            .await
            .unwrap();

        assert_eq!(mailer.sent().len(), 1);
        assert_eq!(mailer.last().unwrap().to, "ada@example.com");

        mailer.clear();
        assert!(mailer.sent().is_empty());
    }
}
