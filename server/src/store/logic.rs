//! The parts of synchronisation that must behave identically on every
//! backend.
//!
//! The SQL differs between SQLite and Postgres; the decisions must not. If
//! the two backends disagreed about what counts as a conflict, the same
//! edit would be accepted on a self-hosted server and rejected on Relay
//! Cloud. So the decision is made here, by a pure function both call, and
//! the backends only supply the current state and carry out the verdict.

use relay_sync::{EntityKind, Mutation, MutationOutcome, SyncObject};

/// What the server currently holds for the object a mutation targets.
#[derive(Debug, Clone, PartialEq)]
pub struct CurrentState {
    /// The revision the server currently holds.
    pub rev: i64,
    /// Whether the server's copy is a tombstone.
    pub deleted: bool,
}

/// What to do with a mutation.
#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    /// Write it, stamping the given revision.
    Write,
    /// Already applied; report the recorded result without writing.
    AlreadyApplied {
        /// The revision the earlier application produced.
        rev: i64,
    },
    /// The object moved on; report a conflict with the server's copy.
    Conflict,
    /// Refuse it outright, with a reason for the user.
    Reject(String),
}

/// Decides what happens to one mutation.
///
/// `current` is the server's copy, or `None` if the object does not exist.
/// `already_applied` is the revision recorded for this `mutation_id`, if the
/// batch has been seen before.
pub fn decide(
    mutation: &Mutation,
    current: Option<&CurrentState>,
    already_applied: Option<i64>,
) -> Verdict {
    // Idempotency comes first: a re-sent batch must be a no-op whatever the
    // object's state has since become.
    if let Some(rev) = already_applied {
        return Verdict::AlreadyApplied { rev };
    }

    if mutation.id.trim().is_empty() {
        return Verdict::Reject("the object id is empty".into());
    }
    if mutation.mutation_id.trim().is_empty() {
        return Verdict::Reject("the mutation id is empty".into());
    }
    if !mutation.deleted && mutation.payload.is_none() {
        return Verdict::Reject("a non-deleting mutation must carry a payload".into());
    }
    if let Some(payload) = &mutation.payload
        && serde_json::to_string(payload).map(|s| s.len()).unwrap_or(0)
            > relay_sync::MAX_OBJECT_BYTES
    {
        return Verdict::Reject(format!(
            "the object exceeds the {} KiB limit",
            relay_sync::MAX_OBJECT_BYTES / 1024
        ));
    }

    match current {
        // The object is new here. base_rev 0 means the client believes it is
        // creating it, which is consistent.
        None => {
            if mutation.base_rev == 0 {
                Verdict::Write
            } else {
                // The client edited something this server has never had —
                // it was deleted and compacted, or they are talking to the
                // wrong server. Either way, guessing would be wrong.
                Verdict::Conflict
            }
        }
        Some(state) => {
            if mutation.base_rev == state.rev {
                Verdict::Write
            } else if state.deleted && mutation.deleted {
                // Both sides deleted it. There is nothing to disagree
                // about, so this is not worth troubling the user with.
                Verdict::AlreadyApplied { rev: state.rev }
            } else {
                Verdict::Conflict
            }
        }
    }
}

/// Builds the outcome for a mutation the server accepted.
pub fn applied(mutation: &Mutation, rev: i64) -> MutationOutcome {
    MutationOutcome::Applied {
        mutation_id: mutation.mutation_id.clone(),
        kind: mutation.kind,
        id: mutation.id.clone(),
        rev,
    }
}

/// Builds the outcome for a mutation that had already been applied.
pub fn duplicate(mutation: &Mutation, rev: i64) -> MutationOutcome {
    MutationOutcome::Duplicate {
        mutation_id: mutation.mutation_id.clone(),
        kind: mutation.kind,
        id: mutation.id.clone(),
        rev,
    }
}

/// Builds the outcome for a conflicting mutation.
pub fn conflict(mutation: &Mutation, server: SyncObject) -> MutationOutcome {
    MutationOutcome::Conflict {
        mutation_id: mutation.mutation_id.clone(),
        kind: mutation.kind,
        id: mutation.id.clone(),
        server: Box::new(server),
    }
}

/// Builds the outcome for a refused mutation.
pub fn rejected(mutation: &Mutation, reason: impl Into<String>) -> MutationOutcome {
    MutationOutcome::Rejected {
        mutation_id: mutation.mutation_id.clone(),
        kind: mutation.kind,
        id: mutation.id.clone(),
        reason: reason.into(),
    }
}

/// Validates a whole batch before any of it is applied.
pub fn validate_batch(batch: &MutationBatchRef<'_>) -> Result<(), String> {
    if batch.mutations.is_empty() {
        return Err("the batch is empty".into());
    }
    let size = serde_json::to_string(batch.mutations)
        .map(|s| s.len())
        .unwrap_or(0);
    if size > relay_sync::MAX_BATCH_BYTES {
        return Err(format!(
            "the batch is {} KiB, over the {} KiB limit",
            size / 1024,
            relay_sync::MAX_BATCH_BYTES / 1024
        ));
    }
    Ok(())
}

/// A borrowed view of a batch, for validation without cloning it.
pub struct MutationBatchRef<'a> {
    /// The mutations to validate.
    pub mutations: &'a [Mutation],
}

/// Parses a stored `kind` column, rejecting anything unrecognised.
pub fn parse_kind(value: &str) -> Result<EntityKind, String> {
    EntityKind::parse(value).ok_or_else(|| format!("unknown object kind '{value}'"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mutation(base_rev: i64, deleted: bool) -> Mutation {
        Mutation {
            mutation_id: "m1".into(),
            kind: EntityKind::Request,
            id: "r1".into(),
            base_rev,
            deleted,
            payload: if deleted {
                None
            } else {
                Some(serde_json::json!({ "name": "mine" }))
            },
        }
    }

    #[test]
    fn a_new_object_with_base_rev_zero_is_written() {
        assert_eq!(decide(&mutation(0, false), None, None), Verdict::Write);
    }

    #[test]
    fn an_edit_matching_the_servers_revision_is_written() {
        let current = CurrentState {
            rev: 5,
            deleted: false,
        };
        assert_eq!(
            decide(&mutation(5, false), Some(&current), None),
            Verdict::Write
        );
    }

    #[test]
    fn an_edit_against_a_stale_revision_conflicts() {
        // The whole point of optimistic concurrency: a teammate changed it
        // while this edit was being made, so last-write-wins would lose
        // their work silently.
        let current = CurrentState {
            rev: 9,
            deleted: false,
        };
        assert_eq!(
            decide(&mutation(5, false), Some(&current), None),
            Verdict::Conflict
        );
    }

    #[test]
    fn creating_an_object_that_already_exists_conflicts() {
        let current = CurrentState {
            rev: 3,
            deleted: false,
        };
        assert_eq!(
            decide(&mutation(0, false), Some(&current), None),
            Verdict::Conflict
        );
    }

    #[test]
    fn editing_an_object_the_server_has_never_seen_conflicts() {
        // base_rev > 0 means the client thinks it is editing something
        // established. If the server has nothing, they disagree about
        // reality and only the user can resolve it.
        assert_eq!(decide(&mutation(4, false), None, None), Verdict::Conflict);
    }

    #[test]
    fn a_replayed_mutation_is_a_no_op_whatever_the_object_now_looks_like() {
        let current = CurrentState {
            rev: 42,
            deleted: true,
        };
        assert_eq!(
            decide(&mutation(1, false), Some(&current), Some(7)),
            Verdict::AlreadyApplied { rev: 7 }
        );
    }

    #[test]
    fn two_sides_deleting_the_same_object_is_not_a_conflict() {
        // Both parties want it gone. Asking the user to resolve that would
        // be pedantry.
        let current = CurrentState {
            rev: 9,
            deleted: true,
        };
        assert_eq!(
            decide(&mutation(5, true), Some(&current), None),
            Verdict::AlreadyApplied { rev: 9 }
        );
    }

    #[test]
    fn deleting_an_object_someone_else_just_edited_conflicts() {
        let current = CurrentState {
            rev: 9,
            deleted: false,
        };
        assert_eq!(
            decide(&mutation(5, true), Some(&current), None),
            Verdict::Conflict
        );
    }

    #[test]
    fn a_non_deleting_mutation_without_a_payload_is_refused() {
        let mut m = mutation(0, false);
        m.payload = None;
        assert!(matches!(decide(&m, None, None), Verdict::Reject(_)));
    }

    #[test]
    fn an_empty_id_is_refused() {
        let mut m = mutation(0, false);
        m.id = "  ".into();
        assert!(matches!(decide(&m, None, None), Verdict::Reject(_)));
    }

    #[test]
    fn an_oversized_object_is_refused_rather_than_stored() {
        let mut m = mutation(0, false);
        m.payload = Some(serde_json::json!({ "blob": "x".repeat(relay_sync::MAX_OBJECT_BYTES) }));
        match decide(&m, None, None) {
            Verdict::Reject(reason) => assert!(reason.contains("limit")),
            other => panic!("expected a rejection, got {other:?}"),
        }
    }

    #[test]
    fn an_empty_batch_is_refused() {
        assert!(validate_batch(&MutationBatchRef { mutations: &[] }).is_err());
    }

    #[test]
    fn an_oversized_batch_is_refused() {
        let big = Mutation {
            mutation_id: "m".into(),
            kind: EntityKind::Request,
            id: "r".into(),
            base_rev: 0,
            deleted: false,
            payload: Some(serde_json::json!({ "x": "y".repeat(relay_sync::MAX_BATCH_BYTES) })),
        };
        let batch = [big];
        let err = validate_batch(&MutationBatchRef { mutations: &batch }).unwrap_err();
        assert!(err.contains("limit"));
    }

    #[test]
    fn unknown_object_kinds_are_rejected_not_stored_opaquely() {
        assert!(parse_kind("cookie_jar").is_err());
        assert_eq!(parse_kind("request").unwrap(), EntityKind::Request);
    }
}
