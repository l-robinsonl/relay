//! Engine-internal errors and their mapping to the public wire type.

use relay_models::{TransportError, TransportErrorKind};
use thiserror::Error;

/// A failure inside the HTTP engine.
#[derive(Debug, Error)]
pub enum EngineError {
    /// The URL was missing, relative or unparseable.
    #[error("invalid URL: {0}")]
    InvalidUrl(String),

    /// The configured proxy URL could not be used.
    #[error("invalid proxy: {0}")]
    InvalidProxy(String),

    /// The client certificate could not be loaded.
    #[error("client certificate: {0}")]
    ClientCertificate(String),

    /// The HTTP client could not be constructed.
    #[error("could not build HTTP client: {0}")]
    ClientBuild(String),

    /// The request body could not be prepared, e.g. an unreadable file.
    #[error("request body: {0}")]
    RequestBody(String),

    /// An OAuth 2.0 flow could not be completed.
    #[error("{0}")]
    OAuth(String),

    /// A header name or value was not valid.
    #[error("invalid header {name}: {reason}")]
    InvalidHeader {
        /// The offending header name.
        name: String,
        /// Why it was rejected.
        reason: String,
    },

    /// The exchange failed in transport.
    #[error("{0}")]
    Transport(#[from] reqwest::Error),

    /// The response exceeded the configured limit.
    #[error("response exceeded the {limit} byte limit")]
    ResponseTooLarge {
        /// The limit that was hit.
        limit: u64,
    },

    /// Reading or writing a spool file failed.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    /// The request deadline elapsed.
    #[error("request timed out after {0} ms")]
    Timeout(u64),
}

impl EngineError {
    /// Converts to the structured error the frontend consumes.
    pub fn to_transport_error(&self) -> TransportError {
        let (kind, detail) = match self {
            Self::InvalidUrl(d) => (TransportErrorKind::InvalidUrl, Some(d.clone())),
            Self::InvalidProxy(d) => (TransportErrorKind::ProxyError, Some(d.clone())),
            Self::ClientCertificate(d) | Self::ClientBuild(d) => {
                (TransportErrorKind::TlsError, Some(d.clone()))
            }
            Self::RequestBody(d) => (TransportErrorKind::RequestBodyError, Some(d.clone())),
            Self::OAuth(d) => (TransportErrorKind::RequestBodyError, Some(d.clone())),
            Self::InvalidHeader { reason, .. } => {
                (TransportErrorKind::RequestBodyError, Some(reason.clone()))
            }
            Self::ResponseTooLarge { .. } => (TransportErrorKind::ResponseTooLarge, None),
            Self::Io(e) => (TransportErrorKind::ResponseBodyError, Some(e.to_string())),
            Self::Timeout(_) => (TransportErrorKind::Timeout, None),
            Self::Transport(e) => return classify_reqwest(e),
        };

        TransportError {
            kind,
            message: self.to_string(),
            detail,
        }
    }
}

/// Classifies a `reqwest` error into something a user can act on.
///
/// `reqwest` reports most failures as a generic error with a source chain, so
/// the chain is walked for the distinguishing signal. The alternative —
/// showing the raw chain — tells a user that `tcp connect error: Connection
/// refused (os error 111)` without telling them what to do about it.
fn classify_reqwest(err: &reqwest::Error) -> TransportError {
    let chain = error_chain(err);
    let lowered = chain.to_lowercase();

    let kind = if err.is_timeout() {
        TransportErrorKind::Timeout
    } else if err.is_redirect() {
        TransportErrorKind::TooManyRedirects
    } else if err.is_builder() {
        TransportErrorKind::InvalidUrl
    } else if lowered.contains("certificate")
        || lowered.contains("tls")
        || lowered.contains("ssl")
        || lowered.contains("handshake")
    {
        TransportErrorKind::TlsError
    } else if lowered.contains("dns")
        || lowered.contains("name or service not known")
        || lowered.contains("failed to lookup address")
        || lowered.contains("nodename nor servname")
    {
        TransportErrorKind::DnsFailure
    } else if lowered.contains("proxy") {
        TransportErrorKind::ProxyError
    } else if err.is_connect()
        || lowered.contains("connection refused")
        || lowered.contains("connection reset")
        || lowered.contains("network is unreachable")
    {
        TransportErrorKind::ConnectionFailed
    } else if err.is_body() || err.is_decode() {
        TransportErrorKind::ResponseBodyError
    } else {
        TransportErrorKind::Other
    };

    TransportError {
        kind,
        message: friendly_message(kind, &chain),
        detail: Some(chain),
    }
}

/// Flattens an error and its sources into one line.
fn error_chain(err: &reqwest::Error) -> String {
    let mut parts = vec![err.to_string()];
    let mut source = std::error::Error::source(err);
    while let Some(e) = source {
        let text = e.to_string();
        if !parts.iter().any(|p| p == &text) {
            parts.push(text);
        }
        source = e.source();
    }
    parts.join(": ")
}

fn friendly_message(kind: TransportErrorKind, chain: &str) -> String {
    match kind {
        TransportErrorKind::DnsFailure => {
            "Could not resolve the hostname. Check the URL and your DNS settings.".into()
        }
        TransportErrorKind::ConnectionFailed => {
            "Could not connect to the server. It may be down, or the port may be wrong.".into()
        }
        TransportErrorKind::TlsError => {
            "The TLS handshake failed. The certificate may be untrusted, expired, or issued \
             for a different hostname. If you expected this — a self-signed certificate on an \
             internal service — you can disable verification for this request in Settings."
                .into()
        }
        TransportErrorKind::Timeout => "The request timed out.".into(),
        TransportErrorKind::TooManyRedirects => {
            "The server redirected too many times. Raise the limit or disable redirects in \
             Settings to see the raw response."
                .into()
        }
        TransportErrorKind::ProxyError => "The proxy refused or could not be reached.".into(),
        TransportErrorKind::ResponseBodyError => {
            "The response ended before it was complete.".into()
        }
        _ => chain.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_url_maps_to_invalid_url_kind() {
        let err = EngineError::InvalidUrl("missing scheme".into());
        let t = err.to_transport_error();
        assert_eq!(t.kind, TransportErrorKind::InvalidUrl);
        assert_eq!(t.detail.as_deref(), Some("missing scheme"));
    }

    #[test]
    fn response_too_large_maps_cleanly() {
        let t = EngineError::ResponseTooLarge { limit: 100 }.to_transport_error();
        assert_eq!(t.kind, TransportErrorKind::ResponseTooLarge);
        assert!(t.message.contains("100"));
    }

    #[test]
    fn timeout_maps_to_timeout_kind() {
        let t = EngineError::Timeout(5000).to_transport_error();
        assert_eq!(t.kind, TransportErrorKind::Timeout);
    }

    #[test]
    fn tls_messages_tell_the_user_what_to_do() {
        let msg = friendly_message(TransportErrorKind::TlsError, "");
        assert!(msg.contains("Settings"), "should point at the remedy");
    }
}
