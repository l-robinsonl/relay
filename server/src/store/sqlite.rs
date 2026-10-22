//! The SQLite backend: the reference implementation, and what self-hosting
//! uses.

use async_trait::async_trait;
use relay_models::Timestamp;
use relay_sync::{
    ApplyResult, ChangePage, Membership, MutationBatch, Role, SyncObject, UserProfile,
};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};
use std::str::FromStr;

use super::logic::{self, CurrentState, MutationBatchRef, Verdict};
use super::{Actor, EmailTokenPurpose, NewUser, ServerWorkspace, StoredUser, SyncStore};
use crate::error::{ServerError, ServerResult};

/// A sync store backed by a local SQLite file.
#[derive(Clone)]
pub struct SqliteStore {
    pool: SqlitePool,
}

impl SqliteStore {
    /// Opens (or creates) the database at `url`, e.g. `sqlite://data/relay.db`.
    pub async fn connect(url: &str) -> ServerResult<Self> {
        let options = SqliteConnectOptions::from_str(url)
            .map_err(|e| ServerError::Internal(format!("bad SQLITE url: {e}")))?
            .create_if_missing(true)
            // WAL lets readers proceed during a write, which matters once
            // several clients are pulling while one pushes.
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
            .synchronous(sqlx::sqlite::SqliteSynchronous::Normal)
            .foreign_keys(true)
            .busy_timeout(std::time::Duration::from_secs(5));

        let pool = SqlitePoolOptions::new()
            // SQLite serialises writes anyway; a large pool buys contention,
            // not throughput.
            .max_connections(5)
            .connect_with(options)
            .await?;

        Ok(Self { pool })
    }

    /// Wraps an existing pool, for tests.
    pub fn from_pool(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Confirms `user_id` may act on `workspace_id`, at or above `required`.
    ///
    /// Returns [`ServerError::Forbidden`] both when the workspace does not
    /// exist and when the user is not a member, so the two are
    /// indistinguishable from outside.
    async fn authorise(
        &self,
        workspace_id: &str,
        user_id: &str,
        required: Role,
    ) -> ServerResult<Role> {
        let row =
            sqlx::query("SELECT role FROM memberships WHERE workspace_id = ?1 AND user_id = ?2")
                .bind(workspace_id)
                .bind(user_id)
                .fetch_optional(&self.pool)
                .await?;

        let role = row
            .and_then(|r| r.try_get::<String, _>("role").ok())
            .and_then(|r| Role::parse(&r))
            .ok_or(ServerError::Forbidden)?;

        if role < required {
            return Err(ServerError::Forbidden);
        }
        Ok(role)
    }
}

fn now_millis() -> i64 {
    Timestamp::now().as_millis()
}

#[async_trait]
impl SyncStore for SqliteStore {
    async fn migrate(&self) -> ServerResult<()> {
        for sql in [
            include_str!("../../migrations/sqlite/0001_initial.sql"),
            include_str!("../../migrations/sqlite/0002_email_tokens.sql"),
        ] {
            sqlx::raw_sql(sql)
                .execute(&self.pool)
                .await
                .map(|_| ())
                .or_else(|e| {
                    // Re-running the schema on an initialised database is not
                    // an error; anything else is.
                    if e.to_string().contains("already exists") {
                        Ok(())
                    } else {
                        Err(ServerError::Database(e))
                    }
                })?;
        }
        Ok(())
    }

    fn backend(&self) -> &'static str {
        "sqlite"
    }

    async fn ping(&self) -> ServerResult<()> {
        sqlx::query("SELECT 1").execute(&self.pool).await?;
        Ok(())
    }

    async fn create_user(&self, user: NewUser) -> ServerResult<UserProfile> {
        let id = uuid::Uuid::now_v7().to_string();
        let normalised = user.email.trim().to_lowercase();
        let created_at = now_millis();

        let result = sqlx::query(
            "INSERT INTO users
                (id, email, email_normalised, password_hash, display_name,
                 is_server_admin, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )
        .bind(&id)
        .bind(user.email.trim())
        .bind(&normalised)
        .bind(&user.password_hash)
        .bind(&user.display_name)
        .bind(i64::from(user.is_server_admin))
        .bind(created_at)
        .execute(&self.pool)
        .await;

        if let Err(e) = result {
            if e.to_string().contains("UNIQUE") {
                return Err(ServerError::Conflict(
                    "that e-mail address is already registered".into(),
                ));
            }
            return Err(ServerError::Database(e));
        }

        Ok(UserProfile {
            id,
            email: user.email.trim().to_string(),
            display_name: user.display_name,
            is_server_admin: user.is_server_admin,
            created_at: Timestamp(created_at),
            email_verified_at: None,
        })
    }

    async fn find_user_by_email(&self, email: &str) -> ServerResult<Option<StoredUser>> {
        let row = sqlx::query(
            "SELECT id, email, display_name, is_server_admin, created_at, password_hash,
                    email_verified_at
             FROM users WHERE email_normalised = ?1",
        )
        .bind(email.trim().to_lowercase())
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(|r| StoredUser {
            profile: UserProfile {
                id: r.get("id"),
                email: r.get("email"),
                display_name: r.get("display_name"),
                is_server_admin: r.get::<i64, _>("is_server_admin") != 0,
                created_at: Timestamp(r.get("created_at")),
                email_verified_at: r.get::<Option<i64>, _>("email_verified_at").map(Timestamp),
            },
            password_hash: r.get("password_hash"),
        }))
    }

    async fn user_count(&self) -> ServerResult<i64> {
        let row = sqlx::query("SELECT COUNT(*) AS n FROM users")
            .fetch_one(&self.pool)
            .await?;
        Ok(row.get("n"))
    }

    async fn create_session(
        &self,
        user_id: &str,
        token_hash: &str,
        expires_at: i64,
    ) -> ServerResult<()> {
        let now = now_millis();
        sqlx::query(
            "INSERT INTO sessions (token_hash, user_id, created_at, expires_at, last_seen_at)
             VALUES (?1, ?2, ?3, ?4, ?3)",
        )
        .bind(token_hash)
        .bind(user_id)
        .bind(now)
        .bind(expires_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn session_user(&self, token_hash: &str) -> ServerResult<Option<UserProfile>> {
        let row = sqlx::query(
            "SELECT u.id, u.email, u.display_name, u.is_server_admin, u.created_at,
                    u.email_verified_at
             FROM sessions s
             JOIN users u ON u.id = s.user_id
             WHERE s.token_hash = ?1 AND s.expires_at > ?2",
        )
        .bind(token_hash)
        .bind(now_millis())
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(|r| UserProfile {
            id: r.get("id"),
            email: r.get("email"),
            display_name: r.get("display_name"),
            is_server_admin: r.get::<i64, _>("is_server_admin") != 0,
            created_at: Timestamp(r.get("created_at")),
            email_verified_at: r.get::<Option<i64>, _>("email_verified_at").map(Timestamp),
        }))
    }

    async fn delete_session(&self, token_hash: &str) -> ServerResult<()> {
        sqlx::query("DELETE FROM sessions WHERE token_hash = ?1")
            .bind(token_hash)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn create_email_token(
        &self,
        user_id: &str,
        purpose: EmailTokenPurpose,
        token_hash: &str,
        expires_at: i64,
    ) -> ServerResult<()> {
        sqlx::query(
            "INSERT INTO email_tokens
                 (token_hash, user_id, purpose, expires_at, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
        )
        .bind(token_hash)
        .bind(user_id)
        .bind(purpose.as_str())
        .bind(expires_at)
        .bind(now_millis())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn consume_email_token(
        &self,
        purpose: EmailTokenPurpose,
        token_hash: &str,
        now: i64,
    ) -> ServerResult<Option<String>> {
        // One statement: DELETE ... RETURNING. Two concurrent redemptions
        // of the same token cannot both see it, so a token is single-use
        // even under a race.
        let row = sqlx::query(
            "DELETE FROM email_tokens
             WHERE token_hash = ?1 AND purpose = ?2 AND expires_at > ?3
             RETURNING user_id",
        )
        .bind(token_hash)
        .bind(purpose.as_str())
        .bind(now)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(|r| r.get("user_id")))
    }

    async fn latest_email_token_at(
        &self,
        user_id: &str,
        purpose: EmailTokenPurpose,
    ) -> ServerResult<Option<i64>> {
        let row = sqlx::query(
            "SELECT MAX(created_at) AS latest FROM email_tokens
             WHERE user_id = ?1 AND purpose = ?2",
        )
        .bind(user_id)
        .bind(purpose.as_str())
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.and_then(|r| r.get::<Option<i64>, _>("latest")))
    }

    async fn clear_email_tokens(
        &self,
        user_id: &str,
        purpose: EmailTokenPurpose,
    ) -> ServerResult<()> {
        sqlx::query("DELETE FROM email_tokens WHERE user_id = ?1 AND purpose = ?2")
            .bind(user_id)
            .bind(purpose.as_str())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn mark_email_verified(&self, user_id: &str, at: i64) -> ServerResult<()> {
        sqlx::query("UPDATE users SET email_verified_at = ?1 WHERE id = ?2")
            .bind(at)
            .bind(user_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn set_password_hash(&self, user_id: &str, password_hash: &str) -> ServerResult<()> {
        sqlx::query("UPDATE users SET password_hash = ?1 WHERE id = ?2")
            .bind(password_hash)
            .bind(user_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn delete_sessions_for_user(&self, user_id: &str) -> ServerResult<u64> {
        let result = sqlx::query("DELETE FROM sessions WHERE user_id = ?1")
            .bind(user_id)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected())
    }

    async fn create_workspace(
        &self,
        actor: &Actor,
        id: &str,
        name: &str,
    ) -> ServerResult<ServerWorkspace> {
        if name.trim().is_empty() {
            return Err(ServerError::BadRequest("a workspace needs a name".into()));
        }
        let now = now_millis();
        let mut tx = self.pool.begin().await?;

        sqlx::query(
            "INSERT INTO workspaces (id, name, change_seq, created_at, updated_at)
             VALUES (?1, ?2, 0, ?3, ?3)",
        )
        .bind(id)
        .bind(name.trim())
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(|e| {
            if e.to_string().contains("UNIQUE") {
                ServerError::Conflict("that workspace already exists".into())
            } else {
                ServerError::Database(e)
            }
        })?;

        // The creator is its first admin, in the same transaction: a
        // workspace nobody can administer would be unusable.
        sqlx::query(
            "INSERT INTO memberships (workspace_id, user_id, role, created_at)
             VALUES (?1, ?2, 'admin', ?3)",
        )
        .bind(id)
        .bind(&actor.user_id)
        .bind(now)
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;

        Ok(ServerWorkspace {
            id: id.to_string(),
            name: name.trim().to_string(),
            change_seq: 0,
            created_at: now,
        })
    }

    async fn list_workspaces(&self, actor: &Actor) -> ServerResult<Vec<ServerWorkspace>> {
        let rows = sqlx::query(
            "SELECT w.id, w.name, w.change_seq, w.created_at
             FROM workspaces w
             JOIN memberships m ON m.workspace_id = w.id
             WHERE m.user_id = ?1
             ORDER BY w.created_at",
        )
        .bind(&actor.user_id)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .map(|r| ServerWorkspace {
                id: r.get("id"),
                name: r.get("name"),
                change_seq: r.get("change_seq"),
                created_at: r.get("created_at"),
            })
            .collect())
    }

    async fn membership(
        &self,
        workspace_id: &str,
        user_id: &str,
    ) -> ServerResult<Option<Membership>> {
        let row = sqlx::query(
            "SELECT role, created_at FROM memberships WHERE workspace_id = ?1 AND user_id = ?2",
        )
        .bind(workspace_id)
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.and_then(|r| {
            Role::parse(&r.get::<String, _>("role")).map(|role| Membership {
                workspace_id: workspace_id.to_string(),
                user_id: user_id.to_string(),
                role,
                created_at: Timestamp(r.get("created_at")),
            })
        }))
    }

    async fn upsert_membership(
        &self,
        actor: &Actor,
        workspace_id: &str,
        user_id: &str,
        role: Role,
    ) -> ServerResult<Membership> {
        self.authorise(workspace_id, &actor.user_id, Role::Admin)
            .await?;

        let now = now_millis();
        sqlx::query(
            "INSERT INTO memberships (workspace_id, user_id, role, created_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(workspace_id, user_id) DO UPDATE SET role = excluded.role",
        )
        .bind(workspace_id)
        .bind(user_id)
        .bind(role.as_str())
        .bind(now)
        .execute(&self.pool)
        .await?;

        Ok(Membership {
            workspace_id: workspace_id.to_string(),
            user_id: user_id.to_string(),
            role,
            created_at: Timestamp(now),
        })
    }

    async fn remove_membership(
        &self,
        actor: &Actor,
        workspace_id: &str,
        user_id: &str,
    ) -> ServerResult<()> {
        self.authorise(workspace_id, &actor.user_id, Role::Admin)
            .await?;

        // Removing the last admin would strand the workspace with nobody
        // able to manage it.
        let row = sqlx::query(
            "SELECT COUNT(*) AS n FROM memberships WHERE workspace_id = ?1 AND role = 'admin'",
        )
        .bind(workspace_id)
        .fetch_one(&self.pool)
        .await?;
        let admins: i64 = row.get("n");

        let target = self.membership(workspace_id, user_id).await?;
        if admins <= 1 && target.is_some_and(|m| m.role == Role::Admin) {
            return Err(ServerError::BadRequest(
                "that is the only administrator; promote someone else first".into(),
            ));
        }

        sqlx::query("DELETE FROM memberships WHERE workspace_id = ?1 AND user_id = ?2")
            .bind(workspace_id)
            .bind(user_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn list_members(
        &self,
        actor: &Actor,
        workspace_id: &str,
    ) -> ServerResult<Vec<(Membership, UserProfile)>> {
        self.authorise(workspace_id, &actor.user_id, Role::Viewer)
            .await?;

        let rows = sqlx::query(
            "SELECT m.user_id, m.role, m.created_at AS joined_at,
                    u.email, u.display_name, u.is_server_admin, u.created_at AS user_created_at
             FROM memberships m
             JOIN users u ON u.id = m.user_id
             WHERE m.workspace_id = ?1
             ORDER BY m.created_at",
        )
        .bind(workspace_id)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .filter_map(|r| {
                let role = Role::parse(&r.get::<String, _>("role"))?;
                let user_id: String = r.get("user_id");
                Some((
                    Membership {
                        workspace_id: workspace_id.to_string(),
                        user_id: user_id.clone(),
                        role,
                        created_at: Timestamp(r.get("joined_at")),
                    },
                    UserProfile {
                        id: user_id,
                        email: r.get("email"),
                        display_name: r.get("display_name"),
                        is_server_admin: r.get::<i64, _>("is_server_admin") != 0,
                        created_at: Timestamp(r.get("user_created_at")),
                        // Not selected: whether a teammate has confirmed
                        // their address is their business, not the
                        // workspace's.
                        email_verified_at: None,
                    },
                ))
            })
            .collect())
    }

    async fn head(&self, actor: &Actor, workspace_id: &str) -> ServerResult<i64> {
        self.authorise(workspace_id, &actor.user_id, Role::Viewer)
            .await?;
        let row = sqlx::query("SELECT change_seq FROM workspaces WHERE id = ?1")
            .bind(workspace_id)
            .fetch_optional(&self.pool)
            .await?
            .ok_or(ServerError::Forbidden)?;
        Ok(row.get("change_seq"))
    }

    async fn changes_since(
        &self,
        actor: &Actor,
        workspace_id: &str,
        since: i64,
        limit: u32,
    ) -> ServerResult<ChangePage> {
        self.authorise(workspace_id, &actor.user_id, Role::Viewer)
            .await?;

        let limit = limit.clamp(1, relay_sync::MAX_PAGE_SIZE);
        let head = self.head(actor, workspace_id).await?;

        // One extra row tells us whether another page exists without a
        // second COUNT query.
        let rows = sqlx::query(
            "SELECT kind, id, rev, deleted, payload, updated_at, updated_by
             FROM objects
             WHERE workspace_id = ?1 AND rev > ?2
             ORDER BY rev
             LIMIT ?3",
        )
        .bind(workspace_id)
        .bind(since)
        .bind(i64::from(limit) + 1)
        .fetch_all(&self.pool)
        .await?;

        let has_more = rows.len() > limit as usize;
        let objects = rows
            .into_iter()
            .take(limit as usize)
            .map(row_to_object)
            .collect::<Result<Vec<_>, _>>()?;

        Ok(ChangePage {
            objects,
            head,
            has_more,
        })
    }

    async fn apply(
        &self,
        actor: &Actor,
        workspace_id: &str,
        batch: MutationBatch,
    ) -> ServerResult<ApplyResult> {
        // A viewer may read everything and change nothing.
        self.authorise(workspace_id, &actor.user_id, Role::Editor)
            .await?;

        logic::validate_batch(&MutationBatchRef {
            mutations: &batch.mutations,
        })
        .map_err(ServerError::BadRequest)?;

        let now = now_millis();
        let mut tx = self.pool.begin().await?;

        let mut seq: i64 = sqlx::query("SELECT change_seq FROM workspaces WHERE id = ?1")
            .bind(workspace_id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or(ServerError::Forbidden)?
            .get("change_seq");

        let mut outcomes = Vec::with_capacity(batch.mutations.len());

        for mutation in &batch.mutations {
            let current = sqlx::query(
                "SELECT rev, deleted FROM objects
                 WHERE workspace_id = ?1 AND kind = ?2 AND id = ?3",
            )
            .bind(workspace_id)
            .bind(mutation.kind.as_str())
            .bind(&mutation.id)
            .fetch_optional(&mut *tx)
            .await?
            .map(|r| CurrentState {
                rev: r.get("rev"),
                deleted: r.get::<i64, _>("deleted") != 0,
            });

            let already = sqlx::query("SELECT rev FROM applied_mutations WHERE mutation_id = ?1")
                .bind(&mutation.mutation_id)
                .fetch_optional(&mut *tx)
                .await?
                .map(|r| r.get::<i64, _>("rev"));

            match logic::decide(mutation, current.as_ref(), already) {
                Verdict::Write => {
                    seq += 1;
                    let payload = if mutation.deleted {
                        None
                    } else {
                        mutation.payload.as_ref().map(|p| p.to_string())
                    };

                    sqlx::query(
                        "INSERT INTO objects
                            (workspace_id, kind, id, rev, deleted, payload, updated_at, updated_by)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                         ON CONFLICT(workspace_id, kind, id) DO UPDATE SET
                            rev        = excluded.rev,
                            deleted    = excluded.deleted,
                            payload    = excluded.payload,
                            updated_at = excluded.updated_at,
                            updated_by = excluded.updated_by",
                    )
                    .bind(workspace_id)
                    .bind(mutation.kind.as_str())
                    .bind(&mutation.id)
                    .bind(seq)
                    .bind(i64::from(mutation.deleted))
                    .bind(payload)
                    .bind(now)
                    .bind(&actor.user_id)
                    .execute(&mut *tx)
                    .await?;

                    sqlx::query(
                        "INSERT INTO applied_mutations
                            (mutation_id, workspace_id, kind, object_id, rev, applied_at)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    )
                    .bind(&mutation.mutation_id)
                    .bind(workspace_id)
                    .bind(mutation.kind.as_str())
                    .bind(&mutation.id)
                    .bind(seq)
                    .bind(now)
                    .execute(&mut *tx)
                    .await?;

                    outcomes.push(logic::applied(mutation, seq));
                }

                Verdict::AlreadyApplied { rev } => {
                    outcomes.push(logic::duplicate(mutation, rev));
                }

                Verdict::Conflict => {
                    let server = sqlx::query(
                        "SELECT kind, id, rev, deleted, payload, updated_at, updated_by
                         FROM objects WHERE workspace_id = ?1 AND kind = ?2 AND id = ?3",
                    )
                    .bind(workspace_id)
                    .bind(mutation.kind.as_str())
                    .bind(&mutation.id)
                    .fetch_optional(&mut *tx)
                    .await?;

                    match server {
                        Some(row) => outcomes.push(logic::conflict(mutation, row_to_object(row)?)),
                        // The object vanished between the check and here,
                        // which only a concurrent compaction could do.
                        None => outcomes.push(logic::rejected(
                            mutation,
                            "the object no longer exists on the server",
                        )),
                    }
                }

                Verdict::Reject(reason) => outcomes.push(logic::rejected(mutation, reason)),
            }
        }

        sqlx::query("UPDATE workspaces SET change_seq = ?1, updated_at = ?2 WHERE id = ?3")
            .bind(seq)
            .bind(now)
            .bind(workspace_id)
            .execute(&mut *tx)
            .await?;

        tx.commit().await?;

        Ok(ApplyResult {
            head: seq,
            outcomes,
        })
    }

    async fn export(&self, actor: &Actor, workspace_id: &str) -> ServerResult<ChangePage> {
        self.authorise(workspace_id, &actor.user_id, Role::Viewer)
            .await?;
        let head = self.head(actor, workspace_id).await?;

        // Export excludes tombstones: a backup wants what exists, not a
        // record of what used to.
        let rows = sqlx::query(
            "SELECT kind, id, rev, deleted, payload, updated_at, updated_by
             FROM objects
             WHERE workspace_id = ?1 AND deleted = 0
             ORDER BY rev",
        )
        .bind(workspace_id)
        .fetch_all(&self.pool)
        .await?;

        Ok(ChangePage {
            objects: rows
                .into_iter()
                .map(row_to_object)
                .collect::<Result<Vec<_>, _>>()?,
            head,
            has_more: false,
        })
    }
}

fn row_to_object(row: sqlx::sqlite::SqliteRow) -> ServerResult<SyncObject> {
    let kind = logic::parse_kind(&row.get::<String, _>("kind")).map_err(ServerError::Internal)?;
    let payload: Option<String> = row.get("payload");

    Ok(SyncObject {
        kind,
        id: row.get("id"),
        rev: row.get("rev"),
        deleted: row.get::<i64, _>("deleted") != 0,
        updated_at: Timestamp(row.get("updated_at")),
        updated_by: row.get("updated_by"),
        payload: payload
            .map(|p| serde_json::from_str(&p))
            .transpose()
            .map_err(|e| ServerError::Internal(format!("stored payload is not JSON: {e}")))?,
    })
}
