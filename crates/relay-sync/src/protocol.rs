//! The pull and push wire types.

use relay_models::Timestamp;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Which kind of object a sync record describes.
///
/// Deliberately a closed set: a server that does not understand a kind must
/// reject it rather than storing an opaque blob it cannot authorise or
/// migrate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum EntityKind {
    /// The workspace's own metadata and variables.
    Workspace,
    /// A collection or folder.
    Collection,
    /// A saved request.
    Request,
    /// An environment, with secret values already stripped.
    Environment,
}

impl EntityKind {
    /// The wire name, also used as the storage discriminator.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Workspace => "workspace",
            Self::Collection => "collection",
            Self::Request => "request",
            Self::Environment => "environment",
        }
    }

    /// Parses a wire name.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "workspace" => Some(Self::Workspace),
            "collection" => Some(Self::Collection),
            "request" => Some(Self::Request),
            "environment" => Some(Self::Environment),
            _ => None,
        }
    }

    /// Every kind, in the order a client should apply them.
    ///
    /// Parents before children, so applying a page never creates a row that
    /// references something not yet written.
    pub const ORDERED: [Self; 4] = [
        Self::Workspace,
        Self::Collection,
        Self::Request,
        Self::Environment,
    ];
}

impl std::fmt::Display for EntityKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One object as it travels over the wire.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SyncObject {
    pub kind: EntityKind,
    pub id: String,
    /// The revision this copy was written at.
    #[ts(type = "number")]
    pub rev: i64,
    /// True when this is a tombstone; `payload` is then `null`.
    #[serde(default)]
    pub deleted: bool,
    pub updated_at: Timestamp,
    /// Who last wrote it, for the conflict UI. `None` for a server-side
    /// change with no attributable user.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_by: Option<String>,
    /// The document. Absent for a tombstone: a deleted object's contents
    /// are not something other devices need, and keeping them would mean
    /// deletion never actually reclaimed anything.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<serde_json::Value>,
}

/// A page of changes returned by a pull.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ChangePage {
    /// The objects, in ascending revision order.
    pub objects: Vec<SyncObject>,
    /// The workspace's current revision.
    ///
    /// A client that has consumed every page up to `head` is fully caught
    /// up. It is reported even on a partial page so the client knows how
    /// far behind it still is.
    #[ts(type = "number")]
    pub head: i64,
    /// True when more objects remain past this page.
    pub has_more: bool,
}

impl ChangePage {
    /// The highest revision in this page, or `None` when it is empty.
    ///
    /// This, not `head`, is what a client advances `last_pulled_seq` to: a
    /// partial page leaves later revisions unseen.
    pub fn highest_rev(&self) -> Option<i64> {
        self.objects.iter().map(|o| o.rev).max()
    }
}

/// One local change being pushed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct Mutation {
    /// Client-generated and unique. The server records it and ignores a
    /// repeat, so re-sending a batch after a dropped connection is safe.
    pub mutation_id: String,
    pub kind: EntityKind,
    pub id: String,
    /// The revision this edit was made against. Zero means "this object is
    /// new"; the server rejects the mutation if the object already exists.
    #[ts(type = "number")]
    pub base_rev: i64,
    #[serde(default)]
    pub deleted: bool,
    /// The new document. `None` only for a deletion.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<serde_json::Value>,
}

/// A batch of mutations, applied in order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct MutationBatch {
    /// Identifies the device, so a client can recognise its own echo when
    /// it pulls back the changes it just pushed.
    pub client_id: String,
    pub mutations: Vec<Mutation>,
}

/// What happened to one mutation.
///
/// A batch is not all-or-nothing: one conflicting request must not block
/// twenty unrelated ones. Each mutation gets its own verdict.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "status", rename_all = "camelCase")]
#[ts(export)]
pub enum MutationOutcome {
    /// Accepted and stamped with a new revision.
    #[serde(rename_all = "camelCase")]
    Applied {
        mutation_id: String,
        kind: EntityKind,
        id: String,
        #[ts(type = "number")]
        rev: i64,
    },
    /// Already applied; the stored result is returned unchanged.
    #[serde(rename_all = "camelCase")]
    Duplicate {
        mutation_id: String,
        kind: EntityKind,
        id: String,
        #[ts(type = "number")]
        rev: i64,
    },
    /// The object moved on since the edit was made. The server's copy is
    /// returned so the client can show a diff rather than guess.
    #[serde(rename_all = "camelCase")]
    Conflict {
        mutation_id: String,
        kind: EntityKind,
        id: String,
        server: Box<SyncObject>,
    },
    /// Refused outright — too large, malformed, or not permitted.
    #[serde(rename_all = "camelCase")]
    Rejected {
        mutation_id: String,
        kind: EntityKind,
        id: String,
        reason: String,
    },
}

impl MutationOutcome {
    /// The mutation this verdict belongs to.
    pub fn mutation_id(&self) -> &str {
        match self {
            Self::Applied { mutation_id, .. }
            | Self::Duplicate { mutation_id, .. }
            | Self::Conflict { mutation_id, .. }
            | Self::Rejected { mutation_id, .. } => mutation_id,
        }
    }

    /// True when the change is now on the server.
    pub fn is_accepted(&self) -> bool {
        matches!(self, Self::Applied { .. } | Self::Duplicate { .. })
    }

    /// True when the user has to decide something.
    pub fn is_conflict(&self) -> bool {
        matches!(self, Self::Conflict { .. })
    }
}

/// The result of pushing a batch.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ApplyResult {
    /// The workspace's revision after the batch.
    #[ts(type = "number")]
    pub head: i64,
    /// One verdict per submitted mutation, in the order submitted.
    pub outcomes: Vec<MutationOutcome>,
}

impl ApplyResult {
    /// Every outcome needing the user's attention.
    pub fn conflicts(&self) -> impl Iterator<Item = &MutationOutcome> {
        self.outcomes.iter().filter(|o| o.is_conflict())
    }

    /// True when every mutation was accepted.
    pub fn is_clean(&self) -> bool {
        self.outcomes.iter().all(MutationOutcome::is_accepted)
    }
}

/// Why a synchronisation request failed as a whole.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, thiserror::Error)]
#[serde(tag = "error", rename_all = "camelCase")]
#[ts(export)]
pub enum SyncError {
    /// The credentials were missing, malformed or expired.
    #[error("not authenticated")]
    Unauthenticated,
    /// Authenticated, but not a member of this workspace with sufficient rights.
    ///
    /// Deliberately indistinguishable from a workspace that does not exist,
    /// so that probing ids cannot be used to enumerate them.
    #[error("no such workspace, or you do not have access to it")]
    Forbidden,
    /// The request exceeded a documented limit.
    #[error("{message}")]
    #[serde(rename_all = "camelCase")]
    LimitExceeded {
        /// Which limit, e.g. `batchBytes`.
        limit: String,
        /// A readable explanation.
        message: String,
    },
    /// The client speaks a protocol this server does not.
    #[error("unsupported protocol version: {version}")]
    #[serde(rename_all = "camelCase")]
    UnsupportedVersion {
        /// What the client asked for.
        version: String,
    },
    /// The request body did not make sense.
    ///
    /// A struct variant rather than a newtype: serde cannot serialise a
    /// tagged newtype variant containing a string, and this enum is
    /// internally tagged so the client can branch on `error`.
    #[error("{message}")]
    #[serde(rename_all = "camelCase")]
    BadRequest {
        /// What was wrong, phrased for the person who has to fix it.
        message: String,
    },
    /// Something went wrong on the server.
    #[error("the server could not complete the request")]
    Internal,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entity_kinds_round_trip_through_their_wire_names() {
        for kind in EntityKind::ORDERED {
            assert_eq!(EntityKind::parse(kind.as_str()), Some(kind));
            let json = serde_json::to_string(&kind).unwrap();
            assert_eq!(serde_json::from_str::<EntityKind>(&json).unwrap(), kind);
        }
    }

    #[test]
    fn an_unknown_entity_kind_is_rejected_rather_than_guessed() {
        assert_eq!(EntityKind::parse("cookie_jar"), None);
        assert!(serde_json::from_str::<EntityKind>("\"cookieJar\"").is_err());
    }

    #[test]
    fn kinds_are_ordered_parents_before_children() {
        // Applying a page in this order never writes a row whose parent is
        // not there yet.
        assert_eq!(EntityKind::ORDERED[0], EntityKind::Workspace);
        assert!(
            EntityKind::ORDERED
                .iter()
                .position(|k| *k == EntityKind::Collection)
                < EntityKind::ORDERED
                    .iter()
                    .position(|k| *k == EntityKind::Request)
        );
    }

    #[test]
    fn a_tombstone_carries_no_payload() {
        let tombstone = SyncObject {
            kind: EntityKind::Request,
            id: "r1".into(),
            rev: 7,
            deleted: true,
            updated_at: Timestamp(1),
            updated_by: None,
            payload: None,
        };
        let json = serde_json::to_value(&tombstone).unwrap();
        assert_eq!(json["deleted"], true);
        assert!(json.get("payload").is_none());
    }

    #[test]
    fn a_client_advances_to_the_highest_rev_it_saw_not_to_head() {
        // A partial page leaves later revisions unseen; advancing to `head`
        // would skip them permanently.
        let page = ChangePage {
            objects: vec![
                SyncObject {
                    kind: EntityKind::Request,
                    id: "a".into(),
                    rev: 3,
                    deleted: false,
                    updated_at: Timestamp(1),
                    updated_by: None,
                    payload: Some(serde_json::json!({})),
                },
                SyncObject {
                    kind: EntityKind::Request,
                    id: "b".into(),
                    rev: 5,
                    deleted: false,
                    updated_at: Timestamp(1),
                    updated_by: None,
                    payload: Some(serde_json::json!({})),
                },
            ],
            head: 99,
            has_more: true,
        };
        assert_eq!(page.highest_rev(), Some(5));
    }

    #[test]
    fn an_empty_page_has_no_highest_rev() {
        let page = ChangePage {
            objects: vec![],
            head: 10,
            has_more: false,
        };
        assert_eq!(page.highest_rev(), None);
    }

    #[test]
    fn outcomes_are_tagged_by_status() {
        let applied = MutationOutcome::Applied {
            mutation_id: "m1".into(),
            kind: EntityKind::Request,
            id: "r1".into(),
            rev: 4,
        };
        assert_eq!(serde_json::to_value(&applied).unwrap()["status"], "applied");
        assert!(applied.is_accepted());
        assert!(!applied.is_conflict());
    }

    #[test]
    fn a_duplicate_counts_as_accepted_so_a_resend_is_not_an_error() {
        let duplicate = MutationOutcome::Duplicate {
            mutation_id: "m1".into(),
            kind: EntityKind::Request,
            id: "r1".into(),
            rev: 4,
        };
        assert!(duplicate.is_accepted());
    }

    #[test]
    fn apply_result_surfaces_conflicts() {
        let result = ApplyResult {
            head: 9,
            outcomes: vec![
                MutationOutcome::Applied {
                    mutation_id: "m1".into(),
                    kind: EntityKind::Request,
                    id: "r1".into(),
                    rev: 8,
                },
                MutationOutcome::Conflict {
                    mutation_id: "m2".into(),
                    kind: EntityKind::Request,
                    id: "r2".into(),
                    server: Box::new(SyncObject {
                        kind: EntityKind::Request,
                        id: "r2".into(),
                        rev: 9,
                        deleted: false,
                        updated_at: Timestamp(1),
                        updated_by: Some("someone-else".into()),
                        payload: Some(serde_json::json!({"name": "theirs"})),
                    }),
                },
            ],
        };

        assert!(!result.is_clean());
        assert_eq!(result.conflicts().count(), 1);
        assert_eq!(result.conflicts().next().unwrap().mutation_id(), "m2");
    }

    #[test]
    fn forbidden_does_not_reveal_whether_the_workspace_exists() {
        // Distinguishing "no such workspace" from "not your workspace"
        // would let anyone enumerate ids.
        let message = SyncError::Forbidden.to_string();
        assert!(message.contains("no such workspace"));
        assert!(message.contains("do not have access"));
    }

    #[test]
    fn every_error_variant_actually_serialises() {
        // An internally tagged enum silently accepts newtype variants at
        // compile time and fails on them at run time, so each variant is
        // round-tripped here rather than trusted.
        let variants = [
            SyncError::Unauthenticated,
            SyncError::Forbidden,
            SyncError::LimitExceeded {
                limit: "batchBytes".into(),
                message: "too big".into(),
            },
            SyncError::UnsupportedVersion {
                version: "v9".into(),
            },
            SyncError::BadRequest {
                message: "bad".into(),
            },
            SyncError::Internal,
        ];

        for variant in variants {
            let json = serde_json::to_string(&variant)
                .unwrap_or_else(|e| panic!("{variant:?} does not serialise: {e}"));
            let back: SyncError = serde_json::from_str(&json).unwrap();
            assert_eq!(back, variant);
        }
    }

    #[test]
    fn sync_errors_are_tagged_for_the_client_to_branch_on() {
        let json = serde_json::to_value(SyncError::LimitExceeded {
            limit: "batchBytes".into(),
            message: "too big".into(),
        })
        .unwrap();
        assert_eq!(json["error"], "limitExceeded");
        assert_eq!(json["limit"], "batchBytes");
    }
}
