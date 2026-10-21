//! Passwords, tokens and sessions.
//!
//! Relay's server authenticates its own users. There is no external
//! identity provider, because requiring one would make a self-hoster
//! depend on a third party to let their own colleagues in.

use argon2::Argon2;
use argon2::password_hash::phc::PasswordHash;
use argon2::password_hash::{PasswordHasher, PasswordVerifier};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as BASE64URL;
use sha2::{Digest, Sha256};

use crate::error::{ServerError, ServerResult};

/// How many random bytes a session token carries.
///
/// 32 bytes is 256 bits of entropy — far beyond guessable, and short
/// enough to sit in an `Authorization` header without comment.
const TOKEN_BYTES: usize = 32;

/// Hashes a password with Argon2id.
///
/// Argon2id rather than bcrypt or PBKDF2: it is the current recommendation
/// and is memory-hard, which is what makes GPU cracking expensive. The
/// default parameters from the `argon2` crate follow OWASP guidance; they
/// are not tuned down to make tests faster, because a test that exercises
/// different parameters from production is not testing production.
pub fn hash_password(password: &str) -> ServerResult<String> {
    if password.len() < 8 {
        return Err(ServerError::BadRequest(
            "a password must be at least 8 characters".into(),
        ));
    }
    // Argon2 handles long inputs fine, but an unbounded password is a free
    // CPU-exhaustion vector: hashing is deliberately expensive.
    if password.len() > 1024 {
        return Err(ServerError::BadRequest(
            "that password is implausibly long".into(),
        ));
    }

    // A fresh random salt is generated per call by the hasher itself.
    let hash: PasswordHash = Argon2::default()
        .hash_password(password.as_bytes())
        .map_err(|e| ServerError::PasswordHash(e.to_string()))?;

    Ok(hash.to_string())
}

/// Verifies a password against a stored hash.
///
/// Returns `false` rather than an error for a wrong password; an error
/// means the stored hash itself could not be parsed.
pub fn verify_password(password: &str, stored_hash: &str) -> bool {
    let Ok(parsed) = PasswordHash::new(stored_hash) else {
        // A corrupt hash must fail closed, not open.
        return false;
    };
    Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok()
}

/// Spends roughly the same effort as a real verification, and fails.
///
/// Called when no user matches the submitted address, so that signing in
/// with an unknown e-mail takes about as long as signing in with a known
/// one. Without this, response timing tells an attacker which addresses
/// are registered.
pub fn verify_dummy(password: &str) {
    // A fixed, valid Argon2id hash of a value nobody has.
    const DUMMY: &str = "$argon2id$v=19$m=19456,t=2,p=1$c29tZXNhbHRzb21lc2FsdA$\
                         Qm9ndXNIYXNoRm9yVGltaW5nRXF1YWxpc2F0aW9u";
    let _ = verify_password(password, DUMMY);
}

/// A freshly minted session token and the hash to store for it.
#[derive(Debug, Clone)]
pub struct SessionToken {
    /// Given to the client once and never stored.
    pub token: String,
    /// What the database keeps.
    pub hash: String,
}

/// Mints a session token.
pub fn new_session_token() -> ServerResult<SessionToken> {
    let mut bytes = [0u8; TOKEN_BYTES];
    getrandom::fill(&mut bytes)
        .map_err(|e| ServerError::Internal(format!("no system randomness: {e}")))?;

    let token = BASE64URL.encode(bytes);
    let hash = hash_token(&token);
    Ok(SessionToken { token, hash })
}

/// Hashes a session token for storage.
///
/// SHA-256 rather than Argon2: a token already has 256 bits of entropy, so
/// there is nothing to brute-force and no reason to pay a memory-hard cost
/// on every single authenticated request. Hashing at all is what stops a
/// database disclosure from handing over live sessions.
pub fn hash_token(token: &str) -> String {
    let digest = Sha256::digest(token.as_bytes());
    BASE64URL.encode(digest)
}

/// Extracts a bearer token from an `Authorization` header value.
pub fn bearer_token(header: &str) -> Option<&str> {
    let rest = header
        .strip_prefix("Bearer ")
        .or_else(|| header.strip_prefix("bearer "))?;
    let token = rest.trim();
    if token.is_empty() { None } else { Some(token) }
}

/// Rejects addresses that are obviously not addresses.
///
/// Deliberately permissive. Full RFC 5322 validation rejects addresses
/// that work, and the only real proof an address exists is sending to it.
pub fn validate_email(email: &str) -> ServerResult<String> {
    let trimmed = email.trim();

    let plausible = trimmed.len() >= 3
        && trimmed.len() <= 254
        && !trimmed.contains(char::is_whitespace)
        && matches!(trimmed.split('@').collect::<Vec<_>>().as_slice(),
                    [local, domain] if !local.is_empty() && domain.contains('.') && domain.len() > 2);

    if !plausible {
        return Err(ServerError::BadRequest(
            "that does not look like an e-mail address".into(),
        ));
    }
    Ok(trimmed.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_password_round_trips() {
        let hash = hash_password("correct horse battery").unwrap();
        assert!(verify_password("correct horse battery", &hash));
        assert!(!verify_password("wrong horse battery", &hash));
    }

    #[test]
    fn the_hash_does_not_contain_the_password() {
        let hash = hash_password("hunter2hunter2").unwrap();
        assert!(!hash.contains("hunter2"));
        assert!(hash.starts_with("$argon2id$"));
    }

    #[test]
    fn the_same_password_hashes_differently_each_time() {
        // A per-password salt is what stops one rainbow table covering
        // every account that chose the same password.
        let a = hash_password("same password").unwrap();
        let b = hash_password("same password").unwrap();
        assert_ne!(a, b);
        assert!(verify_password("same password", &a));
        assert!(verify_password("same password", &b));
    }

    #[test]
    fn short_passwords_are_refused() {
        assert!(hash_password("short").is_err());
        assert!(hash_password("12345678").is_ok());
    }

    #[test]
    fn an_absurdly_long_password_is_refused_rather_than_hashed() {
        // Hashing is deliberately expensive; unbounded input is a free way
        // to spend the server's CPU.
        assert!(hash_password(&"x".repeat(10_000)).is_err());
    }

    #[test]
    fn a_corrupt_stored_hash_fails_closed() {
        assert!(!verify_password("anything", "not-a-hash"));
        assert!(!verify_password("anything", ""));
    }

    #[test]
    fn the_dummy_verification_does_not_panic() {
        // It runs on every failed login, so it must be robust whatever it
        // is handed.
        verify_dummy("anything");
        verify_dummy("");
    }

    #[test]
    fn session_tokens_are_unique_and_long_enough() {
        let a = new_session_token().unwrap();
        let b = new_session_token().unwrap();

        assert_ne!(a.token, b.token);
        assert_ne!(a.hash, b.hash);
        // 32 bytes base64url, unpadded.
        assert!(a.token.len() >= 43, "token is only {} chars", a.token.len());
    }

    #[test]
    fn the_stored_hash_does_not_reveal_the_token() {
        let session = new_session_token().unwrap();
        assert_ne!(session.hash, session.token);
        assert!(!session.hash.contains(&session.token));
        // And the same token always hashes the same, so lookup works.
        assert_eq!(hash_token(&session.token), session.hash);
    }

    #[test]
    fn bearer_tokens_are_extracted_and_nonsense_is_not() {
        assert_eq!(bearer_token("Bearer abc123"), Some("abc123"));
        assert_eq!(bearer_token("bearer abc123"), Some("abc123"));
        assert_eq!(bearer_token("Bearer   abc123  "), Some("abc123"));

        assert_eq!(bearer_token("abc123"), None);
        assert_eq!(bearer_token("Basic abc123"), None);
        assert_eq!(bearer_token("Bearer "), None);
        assert_eq!(bearer_token(""), None);
    }

    #[test]
    fn plausible_addresses_are_accepted() {
        for address in [
            "a@example.com",
            "first.last+tag@sub.example.co.uk",
            "  spaced@example.com  ",
        ] {
            assert!(validate_email(address).is_ok(), "rejected {address}");
        }
        assert_eq!(validate_email("  a@example.com ").unwrap(), "a@example.com");
    }

    #[test]
    fn implausible_addresses_are_refused() {
        for address in [
            "",
            "no-at-sign",
            "@example.com",
            "a@b",
            "a@ b.com",
            "a@@b.com",
        ] {
            assert!(validate_email(address).is_err(), "accepted {address}");
        }
    }
}
