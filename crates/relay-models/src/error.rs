//! The error type that crosses the Tauri boundary.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// A structured error the frontend can branch on.
///
/// `kind` is a closed set so the UI can react — offer a retry, open the TLS
/// settings, point at the offending variable — instead of matching on prose.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct RelayError {
    pub kind: ErrorKind,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// Categories of failure that reach the frontend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum ErrorKind {
    /// The requested object does not exist, or is a tombstone.
    NotFound,
    /// The input failed validation before anything was attempted.
    Invalid,
    /// A conflicting object already exists.
    Conflict,
    /// Local persistence failed.
    Storage,
    /// The HTTP engine could not complete the exchange.
    Transport,
    /// A variable could not be resolved.
    Interpolation,
    /// An import file could not be understood.
    Import,
    /// The OS credential store refused or is unavailable.
    SecretStore,
    /// A filesystem operation failed or was refused.
    Io,
    /// The sync server rejected the operation.
    Sync,
    /// Not permitted.
    Forbidden,
    /// An unclassified internal failure.
    Internal,
}

impl RelayError {
    /// Builds an error of the given kind.
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            detail: None,
        }
    }

    /// Attaches a longer explanation.
    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    /// The object asked for does not exist.
    pub fn not_found(what: impl std::fmt::Display) -> Self {
        Self::new(ErrorKind::NotFound, format!("{what} not found"))
    }

    /// The input was rejected before any work was done.
    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Invalid, message)
    }

    /// Local persistence failed.
    pub fn storage(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Storage, message)
    }

    /// An unclassified internal failure.
    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Internal, message)
    }
}

impl std::fmt::Display for RelayError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}: {}", self.kind, self.message)?;
        if let Some(detail) = &self.detail {
            write!(f, " ({detail})")?;
        }
        Ok(())
    }
}

impl std::error::Error for RelayError {}

/// Shorthand for fallible operations crossing the IPC boundary.
pub type RelayResult<T> = Result<T, RelayError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_serialise_with_a_machine_readable_kind() {
        let err = RelayError::not_found("Request").with_detail("id=abc");
        let json = serde_json::to_value(&err).unwrap();
        assert_eq!(json["kind"], "notFound");
        assert_eq!(json["message"], "Request not found");
        assert_eq!(json["detail"], "id=abc");
    }

    #[test]
    fn detail_is_omitted_when_absent() {
        let json = serde_json::to_value(RelayError::invalid("bad")).unwrap();
        assert!(json.get("detail").is_none());
    }
}
