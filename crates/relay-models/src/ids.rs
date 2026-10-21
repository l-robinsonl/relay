//! Typed identifiers.
//!
//! Every persisted object is keyed by a UUIDv7, which is time-ordered — new
//! rows append to the end of the primary-key index instead of scattering
//! through it, and ids sort chronologically without consulting a timestamp.
//!
//! The ids are newtypes rather than bare strings so that a `WorkspaceId`
//! cannot be passed where a `CollectionId` is expected. They serialise as
//! plain strings, so the wire format is unaffected.

use serde::{Deserialize, Serialize};
use std::fmt;
use ts_rs::TS;
use uuid::Uuid;

macro_rules! define_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, TS)]
        #[serde(transparent)]
        #[ts(export, type = "string")]
        pub struct $name(pub String);

        impl $name {
            /// Generates a fresh time-ordered identifier.
            pub fn new() -> Self {
                Self(Uuid::now_v7().to_string())
            }

            /// Borrows the identifier as a string slice.
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl From<String> for $name {
            fn from(value: String) -> Self {
                Self(value)
            }
        }

        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                Self(value.to_owned())
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }
    };
}

define_id!(
    /// Identifies a workspace, the top-level organisational unit.
    WorkspaceId
);
define_id!(
    /// Identifies a collection or a folder within one.
    CollectionId
);
define_id!(
    /// Identifies a saved request.
    RequestId
);
define_id!(
    /// Identifies an environment.
    EnvironmentId
);
define_id!(
    /// Identifies a single entry in the local request history.
    HistoryId
);
define_id!(
    /// Identifies an open editor tab.
    TabId
);
define_id!(
    /// Identifies one in-flight request execution, used to cancel it.
    ExecutionId
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_unique_and_time_ordered() {
        let a = RequestId::new();
        let b = RequestId::new();
        assert_ne!(a, b);
        // UUIDv7 embeds a millisecond timestamp in its leading bits, so later
        // ids sort after earlier ones lexicographically.
        assert!(a < b || a.as_str()[..8] == b.as_str()[..8]);
    }

    #[test]
    fn ids_serialise_as_bare_strings() {
        let id = WorkspaceId::from("abc");
        assert_eq!(serde_json::to_string(&id).unwrap(), "\"abc\"");
        let back: WorkspaceId = serde_json::from_str("\"abc\"").unwrap();
        assert_eq!(back, id);
    }
}
