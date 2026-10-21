//! The reconciliation loop.
//!
//! One pass is: pull what is new, apply what does not clash, push what is
//! local, and record anything the two sides disagree about. Every step is
//! idempotent, so an interrupted sync is resumed simply by running again.
//!
//! The ordering matters. Pulling first means a local push is made against
//! the freshest revision we can get, which turns many would-be conflicts
//! into clean fast-forwards. Conflicts that remain are genuine: two people
//! really did change the same object.

use std::sync::Arc;

use relay_models::{ErrorKind, RelayError, RelayResult, Timestamp, WorkspaceId};
use relay_storage::{Database, PendingChange, SyncEntity, SyncRepo, SyncState};
use relay_sync::{EntityKind, Mutation, MutationBatch, MutationOutcome};

use super::client::SyncClient;

/// What one synchronisation pass did.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncOutcome {
    /// Objects received and applied.
    pub pulled: usize,
    /// Local changes the server accepted.
    pub pushed: usize,
    /// Disagreements recorded for the user.
    pub conflicts: usize,
    /// Mutations the server refused outright.
    pub rejected: usize,
    /// The workspace's revision afterwards.
    pub head: i64,
}

impl SyncOutcome {
    /// True when nothing at all moved.
    pub fn is_idle(&self) -> bool {
        self.pulled == 0 && self.pushed == 0 && self.conflicts == 0 && self.rejected == 0
    }

    /// True when the user needs to decide something.
    pub fn needs_attention(&self) -> bool {
        self.conflicts > 0 || self.rejected > 0
    }
}

/// How the user resolved a conflict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Resolution {
    /// Keep the local version and push it over the server's.
    KeepLocal,
    /// Discard the local change and take the server's.
    KeepServer,
}

/// Runs synchronisation for a workspace.
#[derive(Clone)]
pub struct SyncEngine {
    db: Database,
    secrets: Arc<dyn crate::secrets::SecretStore>,
}

impl std::fmt::Debug for SyncEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SyncEngine").finish()
    }
}

/// Where a workspace's sync token is kept in the credential store.
///
/// A bearer token is a credential, so it goes where the other credentials
/// go: the OS store, never the database, never an export.
fn token_key(workspace: &WorkspaceId) -> relay_models::EnvironmentId {
    relay_models::EnvironmentId::from(format!("sync:{workspace}"))
}

impl SyncEngine {
    /// Builds an engine over a database and credential store.
    pub fn new(db: Database, secrets: Arc<dyn crate::secrets::SecretStore>) -> Self {
        Self { db, secrets }
    }

    /// Stores the bearer token for a workspace's server.
    pub fn set_token(&self, workspace: &WorkspaceId, token: &str) -> RelayResult<()> {
        self.secrets
            .set(&token_key(workspace), "token", token)
            .map_err(|e| RelayError::new(ErrorKind::SecretStore, e))
    }

    /// Reads the stored bearer token.
    pub fn token(&self, workspace: &WorkspaceId) -> Option<String> {
        self.secrets.get(&token_key(workspace), "token")
    }

    /// Forgets the stored token.
    pub fn clear_token(&self, workspace: &WorkspaceId) -> RelayResult<()> {
        self.secrets
            .delete(&token_key(workspace), "token")
            .map_err(|e| RelayError::new(ErrorKind::SecretStore, e))
    }

    /// Prepares a workspace to join one that already exists on a server.
    ///
    /// Marks the local workspace row clean so the first pull adopts the
    /// server's copy. Without this, a second device that created the
    /// workspace locally would report a conflict on the workspace object
    /// itself before it had synchronised anything — which is noise, not a
    /// disagreement anyone needs to resolve.
    pub async fn prepare_join(&self, workspace: &WorkspaceId) -> RelayResult<()> {
        let id = workspace.clone();
        self.db
            .call(move |conn| SyncRepo::mark_clean(conn, SyncEntity::Workspace, id.as_str(), 0))
            .await?;
        Ok(())
    }

    /// Runs one full synchronisation pass.
    pub async fn sync(&self, workspace: WorkspaceId) -> RelayResult<SyncOutcome> {
        let state = self.state(&workspace).await?;
        let client = SyncClient::new(&state.server_url)?;

        let token = self.token(&workspace).ok_or_else(|| {
            RelayError::new(
                ErrorKind::Forbidden,
                "not signed in to that server — sign in again to resume syncing",
            )
        })?;

        let remote_id = state
            .remote_workspace_id
            .clone()
            .unwrap_or_else(|| workspace.to_string());

        let result = self
            .run(
                &client,
                &token,
                &workspace,
                &remote_id,
                state.last_pulled_seq,
            )
            .await;

        // A failure is recorded so the UI can show why, rather than the
        // sync indicator simply never turning green.
        if let Err(error) = &result {
            let workspace = workspace.clone();
            let message = error.message.clone();
            let _ = self
                .db
                .call(move |conn| SyncRepo::set_error(conn, &workspace, &message))
                .await;
        }

        result
    }

    async fn run(
        &self,
        client: &SyncClient,
        token: &str,
        workspace: &WorkspaceId,
        remote_id: &str,
        since: i64,
    ) -> RelayResult<SyncOutcome> {
        let mut outcome = SyncOutcome::default();

        // --- pull ---------------------------------------------------------
        let mut cursor = since;
        loop {
            let page = client
                .pull(token, remote_id, cursor, relay_sync::MAX_PAGE_SIZE)
                .await?;

            outcome.head = page.head;
            if page.objects.is_empty() {
                break;
            }

            let applied = self.apply_page(workspace, &page.objects).await?;
            outcome.pulled += applied.0;
            outcome.conflicts += applied.1;

            // Advance to the highest revision actually seen, never to
            // `head`: a partial page leaves later revisions unread, and
            // skipping them would lose them permanently.
            let Some(highest) = page.highest_rev() else {
                break;
            };
            cursor = highest;

            let workspace = workspace.clone();
            self.db
                .call(move |conn| SyncRepo::set_pulled_seq(conn, &workspace, cursor))
                .await?;

            if !page.has_more {
                break;
            }
        }

        // --- push ---------------------------------------------------------
        let pending = {
            let workspace = workspace.clone();
            self.db
                .call(move |conn| SyncRepo::pending_changes(conn, &workspace))
                .await?
        };

        if !pending.is_empty() {
            let pushed = self
                .push_changes(client, token, workspace, remote_id, pending)
                .await?;
            outcome.pushed += pushed.0;
            outcome.conflicts += pushed.1;
            outcome.rejected += pushed.2;
            outcome.head = outcome.head.max(pushed.3);

            // Our own writes are already correct locally, so there is no
            // point pulling them back. Skip past them — but only when
            // every revision between the cursor and the new head is one we
            // produced. If another client interleaved a push, those
            // revisions are still unseen and must not be jumped over.
            let ours = pushed.4;
            let span = pushed.3 - cursor;
            if span > 0 && ours.len() as i64 == span && ours.iter().max() == Some(&pushed.3) {
                cursor = pushed.3;
            }
        }

        let workspace_for_state = workspace.clone();
        let final_cursor = cursor.max(0);
        self.db
            .call(move |conn| SyncRepo::set_pulled_seq(conn, &workspace_for_state, final_cursor))
            .await?;

        Ok(outcome)
    }

    /// Applies a page of incoming objects. Returns (applied, conflicts).
    async fn apply_page(
        &self,
        workspace: &WorkspaceId,
        objects: &[relay_sync::SyncObject],
    ) -> RelayResult<(usize, usize)> {
        // Parents before children, so a request never lands before the
        // folder it belongs to.
        let mut ordered: Vec<&relay_sync::SyncObject> = objects.iter().collect();
        ordered.sort_by_key(|o| (EntityKind::ORDERED.iter().position(|k| *k == o.kind), o.rev));

        let workspace = workspace.clone();
        let payload: Vec<(SyncEntity, String, i64, bool, Option<serde_json::Value>)> = ordered
            .into_iter()
            .map(|o| {
                (
                    to_storage_entity(o.kind),
                    o.id.clone(),
                    o.rev,
                    o.deleted,
                    o.payload.clone(),
                )
            })
            .collect();

        // One transaction: a page half-applied would leave the pulled-to
        // marker and the data disagreeing.
        let counts = self
            .db
            .call_transaction(move |tx| {
                let mut applied = 0usize;
                let mut conflicts = 0usize;

                for (entity, id, rev, deleted, body) in &payload {
                    let local_rev = SyncRepo::local_rev(tx, *entity, id)?;
                    let dirty = SyncRepo::is_dirty(tx, *entity, id)?;

                    // A row with unpushed local edits is never overwritten
                    // by a pull, whatever the revisions say. Doing so
                    // would discard the user's work — including, in the
                    // case of a pending deletion, resurrecting something
                    // they deleted.
                    if dirty {
                        match local_rev {
                            // The server has moved past what this edit was
                            // based on: a genuine disagreement.
                            Some(local) if local < *rev => {
                                let local_doc = local_document(tx, *entity, id)?;
                                SyncRepo::record_conflict(
                                    tx,
                                    &workspace,
                                    *entity,
                                    id,
                                    &local_doc,
                                    body.as_ref(),
                                    *rev,
                                    *deleted,
                                )?;
                                conflicts += 1;
                            }
                            // Our own echo, or a revision we already have.
                            // The local edit is still waiting to be pushed,
                            // so leave it exactly as it is.
                            _ => {}
                        }
                        continue;
                    }

                    SyncRepo::apply_remote(
                        tx,
                        &workspace,
                        *entity,
                        id,
                        *rev,
                        *deleted,
                        body.as_ref(),
                    )?;
                    applied += 1;
                }

                Ok((applied, conflicts))
            })
            .await?;

        Ok(counts)
    }

    /// Pushes local changes.
    ///
    /// Returns (pushed, conflicts, rejected, head, revisions we produced).
    async fn push_changes(
        &self,
        client: &SyncClient,
        token: &str,
        workspace: &WorkspaceId,
        remote_id: &str,
        pending: Vec<PendingChange>,
    ) -> RelayResult<(usize, usize, usize, i64, Vec<i64>)> {
        // Reserve a mutation id per object, reusing any already in flight,
        // so a retry after a lost response is recognised as a duplicate.
        let reserved: Vec<(String, PendingChange)> = {
            let workspace = workspace.clone();
            let pending = pending.clone();
            self.db
                .call(move |conn| {
                    pending
                        .into_iter()
                        .map(|change| {
                            let id = SyncRepo::reserve_mutation(conn, &workspace, &change)?;
                            Ok((id, change))
                        })
                        .collect::<relay_storage::StorageResult<Vec<_>>>()
                })
                .await?
        };

        let batch = MutationBatch {
            client_id: client_id(),
            mutations: reserved
                .iter()
                .map(|(mutation_id, change)| Mutation {
                    mutation_id: mutation_id.clone(),
                    kind: to_protocol_kind(change.entity),
                    id: change.id.clone(),
                    base_rev: change.base_rev,
                    deleted: change.deleted,
                    payload: change.payload.clone(),
                })
                .collect(),
        };

        let result = match client.push(token, remote_id, &batch).await {
            Ok(result) => result,
            Err(error) => {
                // The entries stay queued with their ids, so the retry is
                // the same mutation rather than a second one.
                let ids: Vec<String> = reserved.iter().map(|(id, _)| id.clone()).collect();
                let message = error.message.clone();
                let _ = self
                    .db
                    .call(move |conn| {
                        for id in &ids {
                            SyncRepo::record_mutation_error(conn, id, &message)?;
                        }
                        Ok(())
                    })
                    .await;
                return Err(error);
            }
        };

        let workspace_for_apply = workspace.clone();
        let outcomes = result.outcomes.clone();
        let locals: std::collections::HashMap<String, PendingChange> =
            reserved.into_iter().collect();

        let counts = self
            .db
            .call_transaction(move |tx| {
                let mut pushed = 0usize;
                let mut conflicts = 0usize;
                let mut rejected = 0usize;
                let mut revisions: Vec<i64> = Vec::new();

                for outcome in &outcomes {
                    let mutation_id = outcome.mutation_id();
                    let Some(change) = locals.get(mutation_id) else {
                        continue;
                    };

                    match outcome {
                        MutationOutcome::Applied { rev, .. }
                        | MutationOutcome::Duplicate { rev, .. } => {
                            SyncRepo::mark_clean(tx, change.entity, &change.id, *rev)?;
                            SyncRepo::release_mutation(tx, mutation_id)?;
                            revisions.push(*rev);
                            pushed += 1;
                        }

                        MutationOutcome::Conflict { server, .. } => {
                            let local_doc =
                                change.payload.clone().unwrap_or(serde_json::Value::Null);
                            SyncRepo::record_conflict(
                                tx,
                                &workspace_for_apply,
                                change.entity,
                                &change.id,
                                &local_doc,
                                server.payload.as_ref(),
                                server.rev,
                                server.deleted,
                            )?;
                            // The id is spent; resolving will mint a new
                            // one, so the server does not dedupe the retry.
                            SyncRepo::release_mutation(tx, mutation_id)?;
                            conflicts += 1;
                        }

                        MutationOutcome::Rejected { reason, .. } => {
                            SyncRepo::record_mutation_error(tx, mutation_id, reason)?;
                            rejected += 1;
                        }
                    }
                }

                Ok((pushed, conflicts, rejected, revisions))
            })
            .await?;

        Ok((counts.0, counts.1, counts.2, result.head, counts.3))
    }

    /// Applies the user's decision about a conflict.
    pub async fn resolve(&self, conflict_id: String, resolution: Resolution) -> RelayResult<()> {
        let id = conflict_id.clone();
        let conflict = self
            .db
            .call(move |conn| SyncRepo::conflict(conn, &id))
            .await?;

        let id = conflict_id.clone();
        self.db
            .call_transaction(move |tx| {
                match resolution {
                    // Take the server's copy, discarding the local edit.
                    Resolution::KeepServer => {
                        SyncRepo::apply_remote(
                            tx,
                            &conflict.workspace_id,
                            conflict.entity,
                            &conflict.entity_id,
                            conflict.server_rev,
                            conflict.server_deleted,
                            conflict.server.as_ref(),
                        )?;
                    }
                    // Keep the local edit, but rebase it onto the server's
                    // revision so the next push is accepted rather than
                    // conflicting again against the same copy.
                    Resolution::KeepLocal => {
                        let table = table_for(conflict.entity);
                        tx.execute(
                            &format!("UPDATE {table} SET rev = ?1, dirty = 1 WHERE id = ?2"),
                            rusqlite::params![conflict.server_rev, conflict.entity_id],
                        )?;
                    }
                }

                SyncRepo::resolve_conflict(
                    tx,
                    &id,
                    match resolution {
                        Resolution::KeepLocal => "local",
                        Resolution::KeepServer => "server",
                    },
                )?;
                Ok(())
            })
            .await?;

        Ok(())
    }

    async fn state(&self, workspace: &WorkspaceId) -> RelayResult<SyncState> {
        let id = workspace.clone();
        self.db
            .call(move |conn| SyncRepo::state(conn, &id))
            .await?
            .ok_or_else(|| {
                RelayError::new(
                    ErrorKind::Sync,
                    "that workspace is not connected to a server",
                )
            })
    }
}

/// Identifies this device in a push, so a client can recognise its own echo.
fn client_id() -> String {
    // Stable for the process. A device identifier persisted across
    // restarts would be nicer, but nothing depends on it today: the server
    // uses it only for diagnostics.
    static ID: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    ID.get_or_init(|| uuid::Uuid::now_v7().to_string()).clone()
}

fn table_for(entity: SyncEntity) -> &'static str {
    match entity {
        SyncEntity::Workspace => "workspaces",
        SyncEntity::Collection => "collections",
        SyncEntity::Request => "requests",
        SyncEntity::Environment => "environments",
    }
}

/// Reads the local document for a conflict record.
fn local_document(
    conn: &rusqlite::Connection,
    entity: SyncEntity,
    id: &str,
) -> relay_storage::StorageResult<serde_json::Value> {
    use relay_models::{CollectionId, EnvironmentId, RequestId};
    use relay_storage::{CollectionRepo, EnvironmentRepo, RequestRepo, WorkspaceRepo};

    let value = match entity {
        SyncEntity::Workspace => {
            serde_json::to_value(WorkspaceRepo::get(conn, &WorkspaceId::from(id))?)
        }
        SyncEntity::Collection => {
            serde_json::to_value(CollectionRepo::get(conn, &CollectionId::from(id))?)
        }
        SyncEntity::Request => serde_json::to_value(RequestRepo::get(conn, &RequestId::from(id))?),
        SyncEntity::Environment => {
            serde_json::to_value(EnvironmentRepo::get(conn, &EnvironmentId::from(id))?.redacted())
        }
    };

    value.map_err(|source| relay_storage::StorageError::Json {
        field: "local document",
        source,
    })
}

fn to_storage_entity(kind: EntityKind) -> SyncEntity {
    match kind {
        EntityKind::Workspace => SyncEntity::Workspace,
        EntityKind::Collection => SyncEntity::Collection,
        EntityKind::Request => SyncEntity::Request,
        EntityKind::Environment => SyncEntity::Environment,
    }
}

fn to_protocol_kind(entity: SyncEntity) -> EntityKind {
    match entity {
        SyncEntity::Workspace => EntityKind::Workspace,
        SyncEntity::Collection => EntityKind::Collection,
        SyncEntity::Request => EntityKind::Request,
        SyncEntity::Environment => EntityKind::Environment,
    }
}

/// Builds the initial sync state for connecting a workspace to a server.
pub fn initial_state(
    workspace: &WorkspaceId,
    server_url: &str,
    remote_id: Option<String>,
    user: &relay_sync::UserProfile,
) -> SyncState {
    SyncState {
        workspace_id: workspace.clone(),
        server_url: server_url.trim_end_matches('/').to_string(),
        remote_workspace_id: remote_id,
        last_pulled_seq: 0,
        last_sync_at: Some(Timestamp::now()),
        last_error: None,
        user_id: Some(user.id.clone()),
        user_email: Some(user.email.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entity_kinds_map_both_ways_without_loss() {
        for kind in EntityKind::ORDERED {
            assert_eq!(to_protocol_kind(to_storage_entity(kind)), kind);
        }
    }

    #[test]
    fn an_idle_pass_is_reported_as_idle() {
        assert!(SyncOutcome::default().is_idle());
        assert!(!SyncOutcome::default().needs_attention());
    }

    #[test]
    fn conflicts_and_rejections_demand_attention() {
        let conflicted = SyncOutcome {
            conflicts: 1,
            ..Default::default()
        };
        assert!(conflicted.needs_attention());
        assert!(!conflicted.is_idle());

        let rejected = SyncOutcome {
            rejected: 1,
            ..Default::default()
        };
        assert!(rejected.needs_attention());
    }

    #[test]
    fn the_token_key_is_scoped_per_workspace() {
        // Two workspaces on different servers must not share a token.
        let a = token_key(&WorkspaceId::from("w1"));
        let b = token_key(&WorkspaceId::from("w2"));
        assert_ne!(a, b);
        assert!(a.as_str().starts_with("sync:"));
    }

    #[test]
    fn the_client_id_is_stable_within_a_process() {
        assert_eq!(client_id(), client_id());
    }
}
