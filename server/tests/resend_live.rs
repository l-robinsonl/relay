//! The Resend path, against the real API.
//!
//! Skipped unless `RELAY_RESEND_API_KEY` and `RELAY_MAIL_FROM` are set,
//! exactly like the Postgres conformance suite. Messages go to
//! `delivered@resend.dev`, Resend's own sink: it exercises
//! authentication, the sender domain and the request shape without
//! putting anything in a real person's inbox.

use relay_server::mail::{Email, Mailer, ResendMailer};

fn configured() -> Option<(String, String)> {
    let key = std::env::var("RELAY_RESEND_API_KEY").ok()?;
    let from = std::env::var("RELAY_MAIL_FROM").ok()?;
    if key.trim().is_empty() || from.trim().is_empty() {
        return None;
    }
    Some((key, from))
}

#[tokio::test]
async fn a_message_is_accepted_by_resend() {
    let Some((key, from)) = configured() else {
        eprintln!("skipping: RELAY_RESEND_API_KEY / RELAY_MAIL_FROM are not set");
        return;
    };

    let mailer = ResendMailer::new(key, from).unwrap();

    mailer
        .send(&Email {
            to: "delivered@resend.dev".into(),
            subject: "Relay integration test".into(),
            body: "Sent by the Relay test suite. Nothing to do.\n".into(),
        })
        .await
        .expect("Resend rejected the message");
}

#[tokio::test]
async fn an_unverified_sender_is_reported_as_a_refusal_not_a_success() {
    // The failure an operator is most likely to hit. It has to surface
    // as an error, or verification mail silently goes nowhere.
    let Some((key, _)) = configured() else {
        return;
    };

    let mailer = ResendMailer::new(key, "relay@definitely-not-verified.invalid").unwrap();

    let error = mailer
        .send(&Email {
            to: "delivered@resend.dev".into(),
            subject: "Should not send".into(),
            body: "…".into(),
        })
        .await
        .expect_err("an unverified sender domain should be refused");

    assert!(
        error.to_string().contains("refused"),
        "unhelpful error: {error}"
    );
}

#[tokio::test]
async fn a_bad_key_is_reported_as_a_refusal() {
    if configured().is_none() {
        return;
    }

    let mailer = ResendMailer::new("re_not_a_real_key", "relay@example.com").unwrap();

    let error = mailer
        .send(&Email {
            to: "delivered@resend.dev".into(),
            subject: "Should not send".into(),
            body: "…".into(),
        })
        .await
        .expect_err("an invalid key should be refused");

    assert!(error.to_string().contains("refused"), "{error}");
    // The key must not be echoed back into an error that gets logged.
    assert!(!error.to_string().contains("re_not_a_real_key"), "{error}");
}
