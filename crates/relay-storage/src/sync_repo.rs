//! Storage for synchronisation: what needs pushing, what is in flight,
//! what conflicted, and how far we have pulled.

use relay_models::{
    Collection, CollectionId, Environment, EnvironmentId, RequestDoc, RequestId, Timestamp,
    WorkspaceId,
};
use rusqlite::{Connection, OptionalExtension, Row, params};

use crate::error::{StorageError, StorageResult, decode_json, encode_json};

/// Which kind of object a sync record refers to.
///
/// Mirrors `relay_sync::EntityKind`, but `relay-storage` does not depend
/// on `relay-sync` — storage should not need the protocol crate to
/// describe its own rows. The two are kept in step by a test.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SyncEntity {
    /// The workspace's own metadata.
    Workspace,
    /// A collection or folder.
    Collection,
    /// A saved request.
    Request,
    /// An environment.
    Environment,
}

impl SyncEntity {
    /// The stored discriminator.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Workspace => "workspace",
            Self::Collection => "collection",
            Self::Request => "request",
            Self::Environment => "environment",
        }
    }

    /// Parses a stored discriminator.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "workspace" => Some(Self::Workspace),
            "collection" => Some(Self::Collection),
            "request" => Some(Self::Request),
            "environment" => Some(Self::Environment),
            _ => None,
        }
    }

    /// Parents before children, so applying a page never references a row
    /// that has not been written yet.
    pub const ORDERED: [Self; 4] = [
        Self::Workspace,
        Self::Collection,
        Self::Request,
        Self::Environment,
    ];
}

/// A local change waiting to be pushed.
#[derive(Debug, Clone, PartialEq)]
pub struct PendingChange {
    /// Which kind of object changed.
    pub entity: SyncEntity,
    /// Its identifier.
    pub id: String,
    /// The revision this edit was made against; zero for a new object.
    pub base_rev: i64,
    /// True when the local row is a tombstone.
    pub deleted: bool,
    /// The document, already redacted of anything that must not sync.
    /// `None` for a deletion.
    pub payload: Option<serde_json::Value>,
}

/// A mutation that has been sent but not yet acknowledged.
#[derive(Debug, Clone, PartialEq)]
pub struct OutboxEntry {
    /// The id the server deduplicates on.
    pub mutation_id: String,
    /// Which kind of object it targets.
    pub entity: SyncEntity,
    /// Which object.
    pub entity_id: String,
    /// The revision the edit was made against.
    pub base_rev: i64,
    /// How many times sending has been attempted.
    pub attempts: i64,
}

/// A conflict awaiting the user's decision.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredConflict {
    /// The conflict's own id.
    pub id: String,
    /// The workspace it belongs to.
    pub workspace_id: WorkspaceId,
    /// Which kind of object disagreed.
    pub entity: SyncEntity,
    /// Which object.
    pub entity_id: String,
    /// The local document when the conflict was found.
    pub local: serde_json::Value,
    /// The server's copy. `None` when the server deleted it.
    pub server: Option<serde_json::Value>,
    /// The server's revision.
    pub server_rev: i64,
    /// True when the server's copy is a tombstone.
    pub server_deleted: bool,
    /// When the disagreement was noticed.
    pub detected_at: Timestamp,
}

/// Where a workspace synchronises and how far it has got.
#[derive(Debug, Clone, PartialEq)]
pub struct SyncState {
    /// The local workspace.
    pub workspace_id: WorkspaceId,
    /// Where it synchronises.
    pub server_url: String,
    /// Its id on the server, when that differs from the local one.
    pub remote_workspace_id: Option<String>,
    /// The highest revision successfully applied.
    pub last_pulled_seq: i64,
    /// When synchronisation last succeeded.
    pub last_sync_at: Option<Timestamp>,
    /// Why it last failed, if it did.
    pub last_error: Option<String>,
    /// The signed-in user's id on that server.
    pub user_id: Option<String>,
    /// Their address, shown in the UI.
    pub user_email: Option<String>,
}

/// Reads and writes synchronisation bookkeeping.
pub struct SyncRepo;

impl SyncRepo {
    // --- what needs pushing -------------------------------------------------

    /// Every locally modified object in a workspace that is ready to push.
    ///
    /// Reads the row's *current* state rather than an edit log: the server
    /// only needs the latest version, and replaying every intermediate
    /// keystroke would be a great deal of traffic to arrive at the same
    /// place.
    ///
    /// Objects with an unresolved conflict are excluded. Pushing one would
    /// simply be told about the same disagreement again, and would report
    /// it to the user twice in a single pass.
    pub fn pending_changes(
        conn: &Connection,
        workspace: &WorkspaceId,
    ) -> StorageResult<Vec<PendingChange>> {
        let mut pending = Vec::new();

        // Workspace metadata.
        let mut stmt = conn.prepare(
            "SELECT id, name, description, variables_json, sort_order,
                    created_at, updated_at, rev, deleted_at
             FROM workspaces WHERE id = ?1 AND dirty = 1",
        )?;
        let rows = stmt.query_map([workspace.as_str()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(7)?,
                row.get::<_, Option<i64>>(8)?,
                serde_json::json!({
                    "id": row.get::<_, String>(0)?,
                    "name": row.get::<_, String>(1)?,
                    "description": row.get::<_, String>(2)?,
                    "variables": serde_json::from_str::<serde_json::Value>(
                        &row.get::<_, String>(3)?
                    ).unwrap_or(serde_json::Value::Null),
                    "sortOrder": row.get::<_, f64>(4)?,
                    "createdAt": row.get::<_, i64>(5)?,
                    "updatedAt": row.get::<_, i64>(6)?,
                }),
            ))
        })?;
        for row in rows {
            let (id, rev, deleted_at, payload) = row?;
            pending.push(PendingChange {
                entity: SyncEntity::Workspace,
                id,
                base_rev: rev,
                deleted: deleted_at.is_some(),
                payload: if deleted_at.is_some() {
                    None
                } else {
                    Some(payload)
                },
            });
        }

        pending.extend(Self::dirty_collections(conn, workspace)?);
        pending.extend(Self::dirty_requests(conn, workspace)?);
        pending.extend(Self::dirty_environments(conn, workspace)?);

        let contested = Self::contested(conn, workspace)?;
        pending.retain(|change| !contested.contains(&(change.entity, change.id.clone())));

        Ok(pending)
    }

    /// Objects with a conflict the user has not yet resolved.
    fn contested(
        conn: &Connection,
        workspace: &WorkspaceId,
    ) -> StorageResult<std::collections::HashSet<(SyncEntity, String)>> {
        let mut stmt = conn.prepare(
            "SELECT entity, entity_id FROM conflicts
             WHERE workspace_id = ?1 AND resolved_at IS NULL",
        )?;
        let rows = stmt.query_map([workspace.as_str()], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;

        let mut set = std::collections::HashSet::new();
        for row in rows {
            let (entity, id) = row?;
            if let Some(entity) = SyncEntity::parse(&entity) {
                set.insert((entity, id));
            }
        }
        Ok(set)
    }

    fn dirty_collections(
        conn: &Connection,
        workspace: &WorkspaceId,
    ) -> StorageResult<Vec<PendingChange>> {
        let mut stmt = conn.prepare(
            "SELECT id, rev, deleted_at FROM collections
             WHERE workspace_id = ?1 AND dirty = 1",
        )?;
        let ids = stmt
            .query_map([workspace.as_str()], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, Option<i64>>(2)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;

        let mut out = Vec::new();
        for (id, rev, deleted_at) in ids {
            let payload = if deleted_at.is_some() {
                None
            } else {
                let collection =
                    crate::repo::CollectionRepo::get(conn, &CollectionId::from(id.clone()))?;
                Some(
                    serde_json::to_value(collection).map_err(|source| StorageError::Json {
                        field: "collection",
                        source,
                    })?,
                )
            };
            out.push(PendingChange {
                entity: SyncEntity::Collection,
                id,
                base_rev: rev,
                deleted: deleted_at.is_some(),
                payload,
            });
        }
        Ok(out)
    }

    fn dirty_requests(
        conn: &Connection,
        workspace: &WorkspaceId,
    ) -> StorageResult<Vec<PendingChange>> {
        let mut stmt = conn.prepare(
            "SELECT id, rev, deleted_at FROM requests
             WHERE workspace_id = ?1 AND dirty = 1",
        )?;
        let ids = stmt
            .query_map([workspace.as_str()], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, Option<i64>>(2)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;

        let mut out = Vec::new();
        for (id, rev, deleted_at) in ids {
            let payload = if deleted_at.is_some() {
                None
            } else {
                let doc = crate::repo::RequestRepo::get(conn, &RequestId::from(id.clone()))?;
                Some(
                    serde_json::to_value(doc).map_err(|source| StorageError::Json {
                        field: "request",
                        source,
                    })?,
                )
            };
            out.push(PendingChange {
                entity: SyncEntity::Request,
                id,
                base_rev: rev,
                deleted: deleted_at.is_some(),
                payload,
            });
        }
        Ok(out)
    }

    fn dirty_environments(
        conn: &Connection,
        workspace: &WorkspaceId,
    ) -> StorageResult<Vec<PendingChange>> {
        let mut stmt = conn.prepare(
            "SELECT id, rev, deleted_at FROM environments
             WHERE workspace_id = ?1 AND dirty = 1",
        )?;
        let ids = stmt
            .query_map([workspace.as_str()], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, Option<i64>>(2)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;

        let mut out = Vec::new();
        for (id, rev, deleted_at) in ids {
            let payload = if deleted_at.is_some() {
                None
            } else {
                let env =
                    crate::repo::EnvironmentRepo::get(conn, &EnvironmentId::from(id.clone()))?;
                // Redacted again here. The row should already be free of
                // secret values, but this is the last point before they
                // would leave the device.
                Some(
                    serde_json::to_value(env.redacted()).map_err(|source| StorageError::Json {
                        field: "environment",
                        source,
                    })?,
                )
            };
            out.push(PendingChange {
                entity: SyncEntity::Environment,
                id,
                base_rev: rev,
                deleted: deleted_at.is_some(),
                payload,
            });
        }
        Ok(out)
    }

    // --- applying what the server sent ---------------------------------------

    /// Writes an object received from the server.
    ///
    /// Marks the row clean and stamps the server's revision: it came from
    /// the server, so it is by definition not a local change waiting to be
    /// pushed back.
    pub fn apply_remote(
        conn: &Connection,
        workspace: &WorkspaceId,
        entity: SyncEntity,
        id: &str,
        rev: i64,
        deleted: bool,
        payload: Option<&serde_json::Value>,
    ) -> StorageResult<()> {
        if deleted {
            let table = match entity {
                SyncEntity::Workspace => "workspaces",
                SyncEntity::Collection => "collections",
                SyncEntity::Request => "requests",
                SyncEntity::Environment => "environments",
            };
            // A tombstone arriving for a row we have never seen is not an
            // error; there is simply nothing to mark deleted.
            let sql = format!(
                "UPDATE {table} SET deleted_at = COALESCE(deleted_at, ?1),
                                    rev = ?2, dirty = 0
                 WHERE id = ?3"
            );
            conn.execute(&sql, params![Timestamp::now().as_millis(), rev, id])?;
            return Ok(());
        }

        let Some(payload) = payload else {
            return Err(StorageError::Invalid(format!(
                "the server sent {entity:?} {id} with no payload and no tombstone"
            )));
        };

        match entity {
            SyncEntity::Workspace => {
                let mut workspace: relay_models::Workspace =
                    decode_json("workspace", &payload.to_string())?;
                workspace.revision.rev = rev;
                crate::repo::WorkspaceRepo::save(conn, &workspace)?;
            }
            SyncEntity::Collection => {
                let mut collection: Collection = decode_json("collection", &payload.to_string())?;
                collection.workspace_id = workspace.clone();
                collection.revision.rev = rev;
                crate::repo::CollectionRepo::save(conn, &collection)?;
            }
            SyncEntity::Request => {
                let mut doc: RequestDoc = decode_json("request", &payload.to_string())?;
                doc.workspace_id = workspace.clone();
                doc.revision.rev = rev;
                crate::repo::RequestRepo::save(conn, &doc)?;
            }
            SyncEntity::Environment => {
                let mut env: Environment = decode_json("environment", &payload.to_string())?;
                env.workspace_id = workspace.clone();
                env.revision.rev = rev;
                crate::repo::EnvironmentRepo::save(conn, &env)?;
            }
        }

        // Every repository `save` marks the row dirty, because almost every
        // call is a local edit. This one is not, so the flag is cleared.
        Self::mark_clean(conn, entity, id, rev)
    }

    /// Clears the dirty flag and records the acknowledged revision.
    pub fn mark_clean(
        conn: &Connection,
        entity: SyncEntity,
        id: &str,
        rev: i64,
    ) -> StorageResult<()> {
        let table = match entity {
            SyncEntity::Workspace => "workspaces",
            SyncEntity::Collection => "collections",
            SyncEntity::Request => "requests",
            SyncEntity::Environment => "environments",
        };
        conn.execute(
            &format!("UPDATE {table} SET rev = ?1, dirty = 0 WHERE id = ?2"),
            params![rev, id],
        )?;
        Ok(())
    }

    /// True when a local row has unpushed changes.
    pub fn is_dirty(conn: &Connection, entity: SyncEntity, id: &str) -> StorageResult<bool> {
        let table = match entity {
            SyncEntity::Workspace => "workspaces",
            SyncEntity::Collection => "collections",
            SyncEntity::Request => "requests",
            SyncEntity::Environment => "environments",
        };
        let dirty: Option<i64> = conn
            .query_row(
                &format!("SELECT dirty FROM {table} WHERE id = ?1"),
                [id],
                |r| r.get(0),
            )
            .optional()?;
        Ok(dirty.unwrap_or(0) != 0)
    }

    /// The locally stored revision of an object, if it exists.
    pub fn local_rev(
        conn: &Connection,
        entity: SyncEntity,
        id: &str,
    ) -> StorageResult<Option<i64>> {
        let table = match entity {
            SyncEntity::Workspace => "workspaces",
            SyncEntity::Collection => "collections",
            SyncEntity::Request => "requests",
            SyncEntity::Environment => "environments",
        };
        Ok(conn
            .query_row(
                &format!("SELECT rev FROM {table} WHERE id = ?1"),
                [id],
                |r| r.get(0),
            )
            .optional()?)
    }

    // --- the outbox -----------------------------------------------------------

    /// Reserves a mutation id for an object, or returns the existing one.
    ///
    /// Reusing the id across retries is what makes a push idempotent: if a
    /// batch was applied but the response never arrived, re-sending it is
    /// recognised by the server as a duplicate rather than applied twice.
    pub fn reserve_mutation(
        conn: &Connection,
        workspace: &WorkspaceId,
        change: &PendingChange,
    ) -> StorageResult<String> {
        let existing: Option<String> = conn
            .query_row(
                "SELECT mutation_id FROM outbox
                 WHERE workspace_id = ?1 AND entity = ?2 AND entity_id = ?3",
                params![workspace.as_str(), change.entity.as_str(), change.id],
                |r| r.get(0),
            )
            .optional()?;

        if let Some(mutation_id) = existing {
            conn.execute(
                "UPDATE outbox SET attempts = attempts + 1
                 WHERE mutation_id = ?1",
                [&mutation_id],
            )?;
            return Ok(mutation_id);
        }

        let mutation_id = uuid::Uuid::now_v7().to_string();
        conn.execute(
            "INSERT INTO outbox
                (mutation_id, workspace_id, entity, entity_id, op, base_rev,
                 payload_json, created_at, attempts)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 1)",
            params![
                mutation_id,
                workspace.as_str(),
                change.entity.as_str(),
                change.id,
                if change.deleted { "delete" } else { "upsert" },
                change.base_rev,
                change
                    .payload
                    .as_ref()
                    .map(|p| p.to_string())
                    .unwrap_or_default(),
                Timestamp::now().as_millis(),
            ],
        )?;
        Ok(mutation_id)
    }

    /// Removes an outbox entry once its outcome is known.
    pub fn release_mutation(conn: &Connection, mutation_id: &str) -> StorageResult<()> {
        conn.execute("DELETE FROM outbox WHERE mutation_id = ?1", [mutation_id])?;
        Ok(())
    }

    /// Records why a mutation could not be sent, keeping it queued.
    pub fn record_mutation_error(
        conn: &Connection,
        mutation_id: &str,
        error: &str,
    ) -> StorageResult<()> {
        conn.execute(
            "UPDATE outbox SET last_error = ?1 WHERE mutation_id = ?2",
            params![error, mutation_id],
        )?;
        Ok(())
    }

    /// Everything currently in flight for a workspace.
    pub fn outbox(conn: &Connection, workspace: &WorkspaceId) -> StorageResult<Vec<OutboxEntry>> {
        let mut stmt = conn.prepare(
            "SELECT mutation_id, entity, entity_id, base_rev, attempts
             FROM outbox WHERE workspace_id = ?1 ORDER BY seq",
        )?;
        let rows = stmt.query_map([workspace.as_str()], |row| {
            Ok(OutboxEntry {
                mutation_id: row.get(0)?,
                entity: SyncEntity::parse(&row.get::<_, String>(1)?).unwrap_or(SyncEntity::Request),
                entity_id: row.get(2)?,
                base_rev: row.get(3)?,
                attempts: row.get(4)?,
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    // --- conflicts -------------------------------------------------------------

    /// Records a conflict for the user to resolve.
    ///
    /// Replaces any existing open conflict for the same object: only the
    /// most recent disagreement is worth showing, and a pile of stale ones
    /// would bury it.
    #[allow(clippy::too_many_arguments)]
    pub fn record_conflict(
        conn: &Connection,
        workspace: &WorkspaceId,
        entity: SyncEntity,
        entity_id: &str,
        local: &serde_json::Value,
        server: Option<&serde_json::Value>,
        server_rev: i64,
        server_deleted: bool,
    ) -> StorageResult<String> {
        conn.execute(
            "DELETE FROM conflicts
             WHERE workspace_id = ?1 AND entity = ?2 AND entity_id = ?3
               AND resolved_at IS NULL",
            params![workspace.as_str(), entity.as_str(), entity_id],
        )?;

        let id = uuid::Uuid::now_v7().to_string();
        conn.execute(
            "INSERT INTO conflicts
                (id, workspace_id, entity, entity_id, local_json, server_json,
                 server_rev, server_deleted, detected_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                id,
                workspace.as_str(),
                entity.as_str(),
                entity_id,
                encode_json("local_json", local)?,
                server.map(|s| s.to_string()),
                server_rev,
                i64::from(server_deleted),
                Timestamp::now().as_millis(),
            ],
        )?;
        Ok(id)
    }

    /// Conflicts still awaiting a decision.
    pub fn open_conflicts(
        conn: &Connection,
        workspace: &WorkspaceId,
    ) -> StorageResult<Vec<StoredConflict>> {
        let mut stmt = conn.prepare(
            "SELECT id, workspace_id, entity, entity_id, local_json, server_json,
                    server_rev, server_deleted, detected_at
             FROM conflicts
             WHERE workspace_id = ?1 AND resolved_at IS NULL
             ORDER BY detected_at",
        )?;
        let rows = stmt.query_map([workspace.as_str()], map_conflict)?;
        rows.collect::<Result<Vec<_>, _>>()?.into_iter().collect()
    }

    /// One conflict by id, resolved or not.
    pub fn conflict(conn: &Connection, id: &str) -> StorageResult<StoredConflict> {
        let mut stmt = conn.prepare(
            "SELECT id, workspace_id, entity, entity_id, local_json, server_json,
                    server_rev, server_deleted, detected_at
             FROM conflicts WHERE id = ?1",
        )?;
        stmt.query_row([id], map_conflict)
            .optional()?
            .transpose()?
            .ok_or_else(|| StorageError::NotFound {
                entity: "Conflict",
                id: id.to_string(),
            })
    }

    /// Marks a conflict decided.
    pub fn resolve_conflict(conn: &Connection, id: &str, resolution: &str) -> StorageResult<()> {
        let changed = conn.execute(
            "UPDATE conflicts SET resolved_at = ?1, resolution = ?2
             WHERE id = ?3 AND resolved_at IS NULL",
            params![Timestamp::now().as_millis(), resolution, id],
        )?;
        if changed == 0 {
            return Err(StorageError::NotFound {
                entity: "Open conflict",
                id: id.to_string(),
            });
        }
        Ok(())
    }

    // --- sync state --------------------------------------------------------------

    /// The synchronisation state for a workspace, if it has one.
    pub fn state(conn: &Connection, workspace: &WorkspaceId) -> StorageResult<Option<SyncState>> {
        let mut stmt = conn.prepare(
            "SELECT workspace_id, server_url, remote_workspace_id, last_pulled_seq,
                    last_sync_at, last_error, user_id, user_email
             FROM sync_state WHERE workspace_id = ?1",
        )?;
        Ok(stmt
            .query_row([workspace.as_str()], |row| {
                Ok(SyncState {
                    workspace_id: WorkspaceId::from(row.get::<_, String>(0)?),
                    server_url: row.get(1)?,
                    remote_workspace_id: row.get(2)?,
                    last_pulled_seq: row.get(3)?,
                    last_sync_at: row.get::<_, Option<i64>>(4)?.map(Timestamp),
                    last_error: row.get(5)?,
                    user_id: row.get(6)?,
                    user_email: row.get(7)?,
                })
            })
            .optional()?)
    }

    /// Configures or updates where a workspace synchronises.
    pub fn set_state(conn: &Connection, state: &SyncState) -> StorageResult<()> {
        conn.execute(
            "INSERT INTO sync_state
                (workspace_id, server_url, remote_workspace_id, last_pulled_seq,
                 last_sync_at, last_error, user_id, user_email)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(workspace_id) DO UPDATE SET
                server_url          = excluded.server_url,
                remote_workspace_id = excluded.remote_workspace_id,
                last_pulled_seq     = excluded.last_pulled_seq,
                last_sync_at        = excluded.last_sync_at,
                last_error          = excluded.last_error,
                user_id             = excluded.user_id,
                user_email          = excluded.user_email",
            params![
                state.workspace_id.as_str(),
                state.server_url,
                state.remote_workspace_id,
                state.last_pulled_seq,
                state.last_sync_at.map(|t| t.as_millis()),
                state.last_error,
                state.user_id,
                state.user_email,
            ],
        )?;
        Ok(())
    }

    /// Advances the pulled-to marker.
    pub fn set_pulled_seq(
        conn: &Connection,
        workspace: &WorkspaceId,
        seq: i64,
    ) -> StorageResult<()> {
        conn.execute(
            "UPDATE sync_state SET last_pulled_seq = ?1, last_sync_at = ?2, last_error = NULL
             WHERE workspace_id = ?3",
            params![seq, Timestamp::now().as_millis(), workspace.as_str()],
        )?;
        Ok(())
    }

    /// Records why the last synchronisation failed.
    pub fn set_error(conn: &Connection, workspace: &WorkspaceId, error: &str) -> StorageResult<()> {
        conn.execute(
            "UPDATE sync_state SET last_error = ?1 WHERE workspace_id = ?2",
            params![error, workspace.as_str()],
        )?;
        Ok(())
    }

    /// Stops a workspace synchronising, leaving its local data alone.
    pub fn disconnect(conn: &Connection, workspace: &WorkspaceId) -> StorageResult<()> {
        conn.execute(
            "DELETE FROM sync_state WHERE workspace_id = ?1",
            [workspace.as_str()],
        )?;
        conn.execute(
            "DELETE FROM outbox WHERE workspace_id = ?1",
            [workspace.as_str()],
        )?;
        Ok(())
    }
}

fn map_conflict(row: &Row<'_>) -> rusqlite::Result<StorageResult<StoredConflict>> {
    Ok((|| {
        let server_raw: Option<String> = row.get(5)?;
        Ok(StoredConflict {
            id: row.get(0)?,
            workspace_id: WorkspaceId::from(row.get::<_, String>(1)?),
            entity: SyncEntity::parse(&row.get::<_, String>(2)?).ok_or_else(|| {
                StorageError::Invalid("a conflict names an unknown object kind".into())
            })?,
            entity_id: row.get(3)?,
            local: decode_json("local_json", &row.get::<_, String>(4)?)?,
            server: server_raw
                .map(|raw| decode_json::<serde_json::Value>("server_json", &raw))
                .transpose()?,
            server_rev: row.get(6)?,
            server_deleted: row.get::<_, i64>(7)? != 0,
            detected_at: Timestamp(row.get(8)?),
        })
    })())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entity_names_round_trip() {
        for entity in SyncEntity::ORDERED {
            assert_eq!(SyncEntity::parse(entity.as_str()), Some(entity));
        }
        assert_eq!(SyncEntity::parse("cookie_jar"), None);
    }

    #[test]
    fn entities_are_ordered_parents_before_children() {
        let position = |target: SyncEntity| {
            SyncEntity::ORDERED
                .iter()
                .position(|e| *e == target)
                .unwrap()
        };
        assert!(position(SyncEntity::Workspace) < position(SyncEntity::Collection));
        assert!(position(SyncEntity::Collection) < position(SyncEntity::Request));
    }
}
