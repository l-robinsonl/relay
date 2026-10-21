//! Domain-logic errors.

use relay_models::{ErrorKind, RelayError};
use thiserror::Error;

/// A failure in domain logic.
#[derive(Debug, Error)]
pub enum CoreError {
    /// An import file or command could not be understood.
    #[error("{0}")]
    Import(String),

    /// An export could not be produced.
    #[error("{0}")]
    Export(String),

    /// The input failed validation.
    #[error("{0}")]
    Invalid(String),

    /// A variable could not be resolved.
    #[error("unresolved variables: {0}")]
    UnresolvedVariables(String),
}

impl From<CoreError> for RelayError {
    fn from(err: CoreError) -> Self {
        let kind = match &err {
            CoreError::Import(_) | CoreError::Export(_) => ErrorKind::Import,
            CoreError::Invalid(_) => ErrorKind::Invalid,
            CoreError::UnresolvedVariables(_) => ErrorKind::Interpolation,
        };
        RelayError::new(kind, err.to_string())
    }
}

/// Shorthand for domain operations.
pub type CoreResult<T> = Result<T, CoreError>;
