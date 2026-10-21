//! Storage errors.

use relay_models::{ErrorKind, RelayError};
use thiserror::Error;

/// A failure in the local persistence layer.
#[derive(Debug, Error)]
pub enum StorageError {
    /// The underlying SQLite call failed.
    #[error("database error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    /// A JSON column could not be parsed or written.
    #[error("could not decode stored {field}: {source}")]
    Json {
        /// The column that failed.
        field: &'static str,
        /// The underlying serde error.
        #[source]
        source: serde_json::Error,
    },

    /// The requested row does not exist, or is a tombstone.
    #[error("{entity} {id} not found")]
    NotFound {
        /// The kind of object.
        entity: &'static str,
        /// Its identifier.
        id: String,
    },

    /// The database was written by a newer version of Relay.
    #[error(
        "this database was created by a newer version of Relay \
         (schema {found}, this build supports {supported}). Upgrade Relay to open it."
    )]
    SchemaTooNew {
        /// The version found on disk.
        found: i64,
        /// The newest version this build knows.
        supported: i64,
    },

    /// The input was rejected before any write was attempted.
    #[error("{0}")]
    Invalid(String),

    /// A filesystem operation failed.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    /// A blocking database task could not be joined.
    #[error("database task failed: {0}")]
    TaskJoin(String),
}

impl From<StorageError> for RelayError {
    fn from(err: StorageError) -> Self {
        let kind = match &err {
            StorageError::NotFound { .. } => ErrorKind::NotFound,
            StorageError::Invalid(_) => ErrorKind::Invalid,
            StorageError::Io(_) => ErrorKind::Io,
            _ => ErrorKind::Storage,
        };
        RelayError::new(kind, err.to_string())
    }
}

/// Shorthand for storage operations.
pub type StorageResult<T> = Result<T, StorageError>;

/// Decodes a JSON column, naming the field when it fails.
pub(crate) fn decode_json<T: serde::de::DeserializeOwned>(
    field: &'static str,
    raw: &str,
) -> StorageResult<T> {
    serde_json::from_str(raw).map_err(|source| StorageError::Json { field, source })
}

/// Encodes a value for a JSON column.
pub(crate) fn encode_json<T: serde::Serialize>(
    field: &'static str,
    value: &T,
) -> StorageResult<String> {
    serde_json::to_string(value).map_err(|source| StorageError::Json { field, source })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn not_found_maps_to_the_not_found_kind() {
        let err = StorageError::NotFound {
            entity: "Request",
            id: "abc".into(),
        };
        assert_eq!(RelayError::from(err).kind, ErrorKind::NotFound);
    }

    #[test]
    fn a_newer_schema_explains_the_remedy() {
        let err = StorageError::SchemaTooNew {
            found: 5,
            supported: 1,
        };
        assert!(err.to_string().contains("Upgrade Relay"));
    }

    #[test]
    fn json_errors_name_the_offending_column() {
        let err = decode_json::<Vec<String>>("variables_json", "{not json").unwrap_err();
        assert!(err.to_string().contains("variables_json"));
    }
}
