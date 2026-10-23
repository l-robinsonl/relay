//! Verifying that a webhook really came from Stripe.
//!
//! This is the security-critical part of billing. The endpoint is
//! public by necessity — Stripe has to reach it — so without a
//! signature check anyone who knows the URL can POST a
//! `customer.subscription.updated` saying they have a thousand seats
//! and never pay for them.

use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;
use subtle::ConstantTimeEq;

use crate::error::{ServerError, ServerResult};

/// How far out of date a signature may be.
///
/// Stripe signs a timestamp alongside the body. Without a bound, a
/// signature captured once could be replayed for ever; with one, a
/// replay is only possible inside this window. Five minutes is
/// Stripe's own recommendation and leaves room for clock drift.
const TOLERANCE_SECONDS: i64 = 300;

/// Checks a `Stripe-Signature` header against the body.
///
/// Returns the body on success so a caller cannot accidentally parse
/// an unverified payload: there is no way to get the content out
/// except through this function.
pub fn verify<'a>(body: &'a str, header: &str, secret: &str, now: i64) -> ServerResult<&'a str> {
    if secret.trim().is_empty() {
        return Err(ServerError::Config(
            "STRIPE_WEBHOOK_SECRET is not set, so webhooks cannot be verified".into(),
        ));
    }

    let mut timestamp: Option<i64> = None;
    let mut signatures: Vec<&str> = Vec::new();

    // `t=1234,v1=abc,v1=def` — more than one v1 during a secret
    // rotation, and any of them matching is enough.
    for part in header.split(',') {
        match part.trim().split_once('=') {
            Some(("t", value)) => timestamp = value.trim().parse().ok(),
            Some(("v1", value)) => signatures.push(value.trim()),
            _ => {}
        }
    }

    let Some(timestamp) = timestamp else {
        return Err(ServerError::BadRequest(
            "the Stripe signature header carried no timestamp".into(),
        ));
    };

    if (now - timestamp).abs() > TOLERANCE_SECONDS {
        return Err(ServerError::BadRequest(
            "the Stripe signature is too old to accept".into(),
        ));
    }

    if signatures.is_empty() {
        return Err(ServerError::BadRequest(
            "the Stripe signature header carried no v1 signature".into(),
        ));
    }

    let mut mac = <Hmac<Sha256>>::new_from_slice(secret.as_bytes())
        .map_err(|e| ServerError::Internal(e.to_string()))?;
    mac.update(format!("{timestamp}.{body}").as_bytes());
    let expected = mac.finalize().into_bytes();

    for candidate in signatures {
        let Ok(decoded) = decode_hex(candidate) else {
            continue;
        };
        // Constant time: a byte-by-byte comparison that returns early
        // leaks, through timing, how much of a guess was right.
        if decoded.len() == expected.len() && bool::from(decoded.ct_eq(&expected)) {
            return Ok(body);
        }
    }

    Err(ServerError::Forbidden)
}

/// Decodes lowercase hex. Stripe's signatures are always hex.
fn decode_hex(raw: &str) -> Result<Vec<u8>, ()> {
    if !raw.len().is_multiple_of(2) {
        return Err(());
    }
    (0..raw.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&raw[i..i + 2], 16).map_err(|_| ()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "whsec_test_secret";
    const BODY: &str = r#"{"id":"evt_1","type":"customer.subscription.updated"}"#;

    /// Signs a body the way Stripe would.
    fn sign(body: &str, timestamp: i64, secret: &str) -> String {
        let mut mac = <Hmac<Sha256>>::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(format!("{timestamp}.{body}").as_bytes());
        let digest = mac.finalize().into_bytes();
        let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
        format!("t={timestamp},v1={hex}")
    }

    #[test]
    fn a_genuine_signature_is_accepted() {
        let header = sign(BODY, 1_700_000_000, SECRET);
        assert_eq!(verify(BODY, &header, SECRET, 1_700_000_000).unwrap(), BODY);
    }

    #[test]
    fn a_forged_signature_is_refused() {
        // The whole reason this function exists: the endpoint is public,
        // so without it anyone could grant themselves a subscription.
        let header = sign(BODY, 1_700_000_000, "whsec_the_wrong_secret");
        assert!(matches!(
            verify(BODY, &header, SECRET, 1_700_000_000),
            Err(ServerError::Forbidden)
        ));
    }

    #[test]
    fn a_tampered_body_is_refused() {
        let header = sign(BODY, 1_700_000_000, SECRET);
        let altered = r#"{"id":"evt_1","type":"customer.subscription.deleted"}"#;

        assert!(matches!(
            verify(altered, &header, SECRET, 1_700_000_000),
            Err(ServerError::Forbidden)
        ));
    }

    #[test]
    fn an_old_signature_is_refused() {
        // Without this a signature captured once could be replayed for
        // ever.
        let header = sign(BODY, 1_700_000_000, SECRET);
        let much_later = 1_700_000_000 + TOLERANCE_SECONDS + 1;

        let error = verify(BODY, &header, SECRET, much_later).unwrap_err();
        assert!(error.to_string().contains("too old"), "{error}");
    }

    #[test]
    fn a_signature_from_the_near_future_is_accepted() {
        // Clocks drift, and refusing a webhook because the sender is a
        // second ahead would be a self-inflicted outage.
        let header = sign(BODY, 1_700_000_060, SECRET);
        assert!(verify(BODY, &header, SECRET, 1_700_000_000).is_ok());
    }

    #[test]
    fn any_of_several_signatures_matching_is_enough() {
        // Stripe sends more than one v1 while a secret is being
        // rotated; insisting on the first would break the rotation.
        let genuine = sign(BODY, 1_700_000_000, SECRET);
        let hex = genuine.split("v1=").nth(1).unwrap();
        let header = format!("t=1700000000,v1=00{},v1={hex}", &hex[2..]);

        assert!(verify(BODY, &header, SECRET, 1_700_000_000).is_ok());
    }

    #[test]
    fn a_header_with_no_signature_is_refused() {
        let error = verify(BODY, "t=1700000000", SECRET, 1_700_000_000).unwrap_err();
        assert!(error.to_string().contains("no v1 signature"), "{error}");
    }

    #[test]
    fn a_header_with_no_timestamp_is_refused() {
        let error = verify(BODY, "v1=abcdef", SECRET, 1_700_000_000).unwrap_err();
        assert!(error.to_string().contains("no timestamp"), "{error}");
    }

    #[test]
    fn rubbish_in_the_signature_does_not_panic() {
        for header in ["", "nonsense", "t=notanumber,v1=zz", "t=1,v1=abc"] {
            assert!(verify(BODY, header, SECRET, 1_700_000_000).is_err());
        }
    }

    #[test]
    fn an_unconfigured_secret_refuses_rather_than_accepting_everything() {
        // The dangerous failure: an empty secret must not make every
        // signature valid.
        let header = sign(BODY, 1_700_000_000, "");
        assert!(verify(BODY, &header, "", 1_700_000_000).is_err());
    }
}
