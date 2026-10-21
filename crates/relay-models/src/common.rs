//! Primitives shared by every domain type.

use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};
use ts_rs::TS;

/// A point in time as milliseconds since the Unix epoch, UTC.
///
/// Milliseconds rather than an RFC 3339 string because timestamps are compared
/// and sorted far more often than they are displayed: this is one integer
/// column in SQLite, one number in JSON, and one `new Date(ms)` in the
/// frontend, with no parsing or timezone ambiguity anywhere in between.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, TS)]
#[serde(transparent)]
#[ts(export, type = "number")]
pub struct Timestamp(pub i64);

impl Timestamp {
    /// The current wall-clock time.
    pub fn now() -> Self {
        let millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        Self(millis)
    }

    /// The raw millisecond value.
    pub fn as_millis(self) -> i64 {
        self.0
    }
}

impl Default for Timestamp {
    fn default() -> Self {
        Self::now()
    }
}

impl From<i64> for Timestamp {
    fn from(value: i64) -> Self {
        Self(value)
    }
}

/// An ordered, individually disableable key/value pair.
///
/// Used for headers, query parameters, path parameters, form fields and
/// environment variables. `enabled` exists because developers routinely toggle
/// a header off to test something and expect it back, not deleted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct KeyValue {
    pub key: String,
    #[serde(default)]
    pub value: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

impl KeyValue {
    /// An enabled pair with no description.
    pub fn new(key: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            value: value.into(),
            enabled: true,
            description: None,
        }
    }

    /// True when the pair should take part in a request: enabled, with a key.
    pub fn is_active(&self) -> bool {
        self.enabled && !self.key.trim().is_empty()
    }
}

pub(crate) fn default_true() -> bool {
    true
}

/// Revision metadata carried by every synchronisable object.
///
/// `rev` is the server revision this copy was last confirmed at — zero means
/// the object has never been synchronised. `dirty` marks a local edit that has
/// not yet been acknowledged. Together they drive optimistic concurrency:
/// a push carries `rev` as its base, and the server rejects it if the object
/// has moved on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct Revision {
    #[serde(default)]
    #[ts(type = "number")]
    pub rev: i64,
    #[serde(default)]
    pub dirty: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deleted_at: Option<Timestamp>,
}

impl Revision {
    /// True when this object is a tombstone.
    pub fn is_deleted(&self) -> bool {
        self.deleted_at.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_value_defaults_to_enabled() {
        let kv: KeyValue = serde_json::from_str(r#"{"key":"Accept"}"#).unwrap();
        assert!(kv.enabled);
        assert_eq!(kv.value, "");
    }

    #[test]
    fn blank_keys_are_not_active() {
        assert!(!KeyValue::new("   ", "v").is_active());
        assert!(KeyValue::new("k", "").is_active());
        let mut kv = KeyValue::new("k", "v");
        kv.enabled = false;
        assert!(!kv.is_active());
    }

    #[test]
    fn timestamp_is_plausible() {
        // Later than 2024-01-01 and earlier than 2100, i.e. actually millis.
        let now = Timestamp::now().as_millis();
        assert!(now > 1_704_067_200_000);
        assert!(now < 4_102_444_800_000);
    }
}
