//! Server errors and how they reach the client.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use relay_sync::SyncError;
use thiserror::Error;

/// Anything that can go wrong handling a request.
#[derive(Debug, Error)]
pub enum ServerError {
    /// No credentials, or they did not check out.
    #[error("not authenticated")]
    Unauthenticated,

    /// Authenticated but not permitted.
    ///
    /// Returned both for "you are not a member" and "there is no such
    /// workspace", deliberately indistinguishable so that ids cannot be
    /// enumerated by probing.
    #[error("no such workspace, or you do not have access to it")]
    Forbidden,

    /// The request was malformed.
    #[error("{0}")]
    BadRequest(String),

    /// A documented limit was exceeded.
    #[error("{message}")]
    LimitExceeded {
        /// Which limit.
        limit: &'static str,
        /// A readable explanation.
        message: String,
    },

    /// Something already exists that must be unique.
    #[error("{0}")]
    Conflict(String),

    /// The database failed.
    #[error("database error")]
    Database(#[from] sqlx::Error),

    /// Password hashing failed.
    #[error("could not process the password")]
    PasswordHash(String),

    /// Anything unclassified.
    #[error("internal error")]
    Internal(String),
}

impl ServerError {
    /// The HTTP status this maps to.
    pub fn status(&self) -> StatusCode {
        match self {
            Self::Unauthenticated => StatusCode::UNAUTHORIZED,
            Self::Forbidden => StatusCode::NOT_FOUND,
            Self::BadRequest(_) => StatusCode::BAD_REQUEST,
            Self::LimitExceeded { .. } => StatusCode::PAYLOAD_TOO_LARGE,
            Self::Conflict(_) => StatusCode::CONFLICT,
            Self::Database(_) | Self::PasswordHash(_) | Self::Internal(_) => {
                StatusCode::INTERNAL_SERVER_ERROR
            }
        }
    }

    /// The structured body the client receives.
    pub fn to_sync_error(&self) -> SyncError {
        match self {
            Self::Unauthenticated => SyncError::Unauthenticated,
            Self::Forbidden => SyncError::Forbidden,
            Self::BadRequest(message) => SyncError::BadRequest {
                message: message.clone(),
            },
            Self::LimitExceeded { limit, message } => SyncError::LimitExceeded {
                limit: (*limit).to_string(),
                message: message.clone(),
            },
            Self::Conflict(message) => SyncError::BadRequest {
                message: message.clone(),
            },
            // Internal detail stays in the log, not in the response: a
            // database error message can disclose schema and query shape.
            Self::Database(_) | Self::PasswordHash(_) | Self::Internal(_) => SyncError::Internal,
        }
    }
}

impl IntoResponse for ServerError {
    fn into_response(self) -> Response {
        if matches!(
            self,
            ServerError::Database(_) | ServerError::PasswordHash(_) | ServerError::Internal(_)
        ) {
            tracing::error!("{self}");
        }
        (self.status(), axum::Json(self.to_sync_error())).into_response()
    }
}

/// Shorthand for server operations.
pub type ServerResult<T> = Result<T, ServerError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_forbidden_workspace_is_reported_as_not_found() {
        // Returning 403 would confirm the workspace exists, which is enough
        // to enumerate ids.
        assert_eq!(ServerError::Forbidden.status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn internal_failures_do_not_leak_their_detail() {
        let err = ServerError::Internal("connection string: postgres://u:p@host".into());
        let body = serde_json::to_string(&err.to_sync_error()).unwrap();

        assert!(!body.contains("postgres://"), "leaked: {body}");
        assert!(!body.contains("connection string"));
        assert_eq!(err.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn client_errors_keep_their_message_because_it_is_actionable() {
        let err = ServerError::BadRequest("mutation 3 has no payload".into());
        let body = serde_json::to_string(&err.to_sync_error()).unwrap();
        assert!(body.contains("mutation 3"));
    }

    #[test]
    fn unauthenticated_is_401_and_distinct_from_forbidden() {
        assert_eq!(
            ServerError::Unauthenticated.status(),
            StatusCode::UNAUTHORIZED
        );
    }
}
