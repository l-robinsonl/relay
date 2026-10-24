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

    /// The credentials were right but the address is unconfirmed.
    ///
    /// Distinct from [`Self::Unauthenticated`] so the client can say
    /// "check your inbox" rather than "wrong password". Only reachable
    /// after the password has already checked out, so it reveals
    /// nothing to someone who does not have it.
    #[error("confirm your e-mail address before signing in")]
    EmailNotVerified,

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

    /// Anything unclassified. Its detail is logged, never returned.
    #[error("internal error")]
    Internal(String),

    /// The mail provider would not take the message.
    ///
    /// Unlike [`Self::Internal`], this keeps its detail in `Display`.
    /// The provider's refusals are the operator's only clue — "domain
    /// not verified", "invalid from address" — and they describe the
    /// mail configuration, not Relay's schema or queries. Clients still
    /// learn nothing: `to_sync_error` flattens it like any other
    /// internal fault, and the e-mail endpoints never surface it at all.
    #[error("the mail provider refused the message: {0}")]
    Mail(String),

    /// The payment provider refused a request, or could not be reached.
    ///
    /// Keeps its detail in `Display` for the same reason as
    /// [`Self::Mail`]: "No such price: price_x", or a test key used
    /// against a live price, is the operator's entire diagnosis, and
    /// it describes the Stripe configuration rather than Relay's
    /// schema or queries. Clients still get a flat `Internal`.
    #[error("Stripe refused the request: {0}")]
    Stripe(String),

    /// The model provider refused, or could not be reached.
    ///
    /// Keeps its detail in `Display` for the same reason as
    /// [`Self::Stripe`]: an expired key or a retired model is the
    /// operator's whole diagnosis, and none of it is the
    /// conversation. Clients get a flat `Internal`.
    #[error("the assistant provider failed: {0}")]
    Assistant(String),

    /// The server is misconfigured.
    ///
    /// Keeps its message, unlike [`Self::Internal`]: this is reported to
    /// the operator at startup before anything is listening, so there is
    /// no client to leak it to — and "internal error" would tell them
    /// nothing about which variable they got wrong.
    #[error("configuration error: {0}")]
    Config(String),
}

impl ServerError {
    /// The HTTP status this maps to.
    pub fn status(&self) -> StatusCode {
        match self {
            Self::Unauthenticated => StatusCode::UNAUTHORIZED,
            Self::EmailNotVerified => StatusCode::FORBIDDEN,
            Self::Forbidden => StatusCode::NOT_FOUND,
            Self::BadRequest(_) => StatusCode::BAD_REQUEST,
            // 402, not 413. The only limit this carries is seats, and
            // "payload too large" described a body-size cap that
            // tower-http now answers by itself. A seat limit read as
            // 413 in a real run and looked like a malformed request.
            Self::LimitExceeded { .. } => StatusCode::PAYMENT_REQUIRED,
            Self::Conflict(_) => StatusCode::CONFLICT,
            Self::Database(_)
            | Self::PasswordHash(_)
            | Self::Internal(_)
            | Self::Mail(_)
            | Self::Stripe(_)
            | Self::Assistant(_)
            | Self::Config(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    /// The structured body the client receives.
    pub fn to_sync_error(&self) -> SyncError {
        match self {
            Self::Unauthenticated => SyncError::Unauthenticated,
            // Carried as a bad request with its own wording so the client
            // can show "check your inbox" without needing a new variant
            // on the wire that older clients would not understand.
            Self::EmailNotVerified => SyncError::BadRequest {
                message: self.to_string(),
            },
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
            // database error message can disclose schema and query shape,
            // and a configuration message can disclose deployment layout.
            Self::Database(_)
            | Self::PasswordHash(_)
            | Self::Internal(_)
            | Self::Mail(_)
            | Self::Stripe(_)
            | Self::Assistant(_)
            | Self::Config(_) => SyncError::Internal,
        }
    }
}

impl IntoResponse for ServerError {
    fn into_response(self) -> Response {
        if matches!(
            self,
            ServerError::Database(_)
                | ServerError::PasswordHash(_)
                | ServerError::Internal(_)
                | ServerError::Stripe(_)
                | ServerError::Assistant(_)
                | ServerError::Config(_)
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
    fn a_seat_limit_asks_for_payment_rather_than_complaining_about_size() {
        let err = ServerError::LimitExceeded {
            limit: "seats",
            message: "buy another seat".into(),
        };
        assert_eq!(err.status(), StatusCode::PAYMENT_REQUIRED);
    }

    #[test]
    fn a_stripe_refusal_reaches_the_log_but_not_the_client() {
        // The whole point of the variant. Routing it through
        // `Internal` logged the literal string "internal error", so
        // an operator with a wrong price id had nothing to go on.
        let err = ServerError::Stripe("No such price: 'price_wrong'".into());

        assert!(err.to_string().contains("price_wrong"), "{err}");
        let body = serde_json::to_string(&err.to_sync_error()).unwrap();
        assert!(!body.contains("price_wrong"), "leaked: {body}");
    }

    #[test]
    fn configuration_errors_name_the_problem_for_the_operator() {
        // Startup failures are read by whoever deployed this, not by a
        // client, so "internal error" would be useless to them.
        let err = ServerError::Config("RELAY_BIND is not an address".into());
        assert!(err.to_string().contains("RELAY_BIND"));
    }

    #[test]
    fn configuration_detail_still_never_reaches_a_client() {
        let err = ServerError::Config("postgres://user:pw@internal-host/db".into());
        let body = serde_json::to_string(&err.to_sync_error()).unwrap();
        assert!(!body.contains("internal-host"), "leaked: {body}");
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
