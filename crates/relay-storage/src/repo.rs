//! Repositories: typed CRUD over the SQLite tables.
//!
//! Every read filters tombstones, and every delete writes one. Hard deletion
//! happens only through explicit compaction, because a row removed outright
//! while offline cannot be synchronised — the deletion would simply vanish.

use relay_models::{
    AuthConfig, Collection, CollectionId, Environment, EnvironmentId, HistoryEntry, HistoryId,
    HttpMethod, HttpRequestSpec, KeyValue, RequestBody, RequestDoc, RequestId, RequestScripts,
    RequestSettings, Revision, SyncConfig, Timestamp, TreeNode, Variable, Workspace, WorkspaceId,
};
use rusqlite::{Connection, OptionalExtension, Row, params};

use crate::error::{StorageError, StorageResult, decode_json, encode_json};

/// Reads and writes workspaces.
pub struct WorkspaceRepo;

impl WorkspaceRepo {
    /// Every live workspace, oldest first.
    pub fn list(conn: &Connection) -> StorageResult<Vec<Workspace>> {
        let mut stmt = conn.prepare(
            "SELECT id, name, description, variables_json, sync_json, sort_order,
                    created_at, updated_at, rev, dirty, deleted_at
             FROM workspaces
             WHERE deleted_at IS NULL
             ORDER BY sort_order, created_at",
        )?;
        let rows = stmt.query_map([], map_workspace)?;
        rows.collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .collect::<StorageResult<Vec<_>>>()
    }

    /// One workspace by id.
    pub fn get(conn: &Connection, id: &WorkspaceId) -> StorageResult<Workspace> {
        let mut stmt = conn.prepare(
            "SELECT id, name, description, variables_json, sync_json, sort_order,
                    created_at, updated_at, rev, dirty, deleted_at
             FROM workspaces
             WHERE id = ?1 AND deleted_at IS NULL",
        )?;
        stmt.query_row([id.as_str()], map_workspace)
            .optional()?
            .transpose()?
            .ok_or_else(|| StorageError::NotFound {
                entity: "Workspace",
                id: id.to_string(),
            })
    }

    /// Inserts or replaces a workspace, marking it dirty for sync.
    pub fn save(conn: &Connection, workspace: &Workspace) -> StorageResult<()> {
        if workspace.name.trim().is_empty() {
            return Err(StorageError::Invalid("a workspace needs a name".into()));
        }

        conn.execute(
            "INSERT INTO workspaces
                (id, name, description, variables_json, sync_json, sort_order,
                 created_at, updated_at, rev, dirty, deleted_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 1, ?10)
             ON CONFLICT(id) DO UPDATE SET
                name           = excluded.name,
                description    = excluded.description,
                variables_json = excluded.variables_json,
                sync_json      = excluded.sync_json,
                sort_order     = excluded.sort_order,
                updated_at     = excluded.updated_at,
                dirty          = 1,
                deleted_at     = excluded.deleted_at",
            params![
                workspace.id.as_str(),
                workspace.name.trim(),
                workspace.description,
                encode_json("variables_json", &workspace.variables)?,
                workspace
                    .sync
                    .as_ref()
                    .map(|s| encode_json("sync_json", s))
                    .transpose()?,
                workspace.sort_order,
                workspace.created_at.as_millis(),
                workspace.updated_at.as_millis(),
                workspace.revision.rev,
                workspace.revision.deleted_at.map(|t| t.as_millis()),
            ],
        )?;
        Ok(())
    }

    /// Tombstones a workspace and everything beneath it.
    pub fn delete(conn: &Connection, id: &WorkspaceId) -> StorageResult<()> {
        let now = Timestamp::now().as_millis();
        // Children are tombstoned too, so that the deletion propagates through
        // sync instead of leaving orphans on a teammate's machine.
        for table in ["requests", "collections", "environments"] {
            conn.execute(
                &format!(
                    "UPDATE {table} SET deleted_at = ?1, updated_at = ?1, dirty = 1
                     WHERE workspace_id = ?2 AND deleted_at IS NULL"
                ),
                params![now, id.as_str()],
            )?;
        }
        let changed = conn.execute(
            "UPDATE workspaces SET deleted_at = ?1, updated_at = ?1, dirty = 1
             WHERE id = ?2 AND deleted_at IS NULL",
            params![now, id.as_str()],
        )?;

        if changed == 0 {
            return Err(StorageError::NotFound {
                entity: "Workspace",
                id: id.to_string(),
            });
        }
        Ok(())
    }
}

fn map_workspace(row: &Row<'_>) -> rusqlite::Result<StorageResult<Workspace>> {
    Ok((|| {
        let sync_raw: Option<String> = row.get(4)?;
        Ok(Workspace {
            id: WorkspaceId::from(row.get::<_, String>(0)?),
            name: row.get(1)?,
            description: row.get(2)?,
            variables: decode_json::<Vec<Variable>>("variables_json", &row.get::<_, String>(3)?)?,
            sync: sync_raw
                .map(|raw| decode_json::<SyncConfig>("sync_json", &raw))
                .transpose()?,
            sort_order: row.get(5)?,
            created_at: Timestamp(row.get(6)?),
            updated_at: Timestamp(row.get(7)?),
            revision: Revision {
                rev: row.get(8)?,
                dirty: row.get::<_, i64>(9)? != 0,
                deleted_at: row.get::<_, Option<i64>>(10)?.map(Timestamp),
            },
        })
    })())
}

/// Reads and writes collections and folders.
pub struct CollectionRepo;

impl CollectionRepo {
    /// Every live collection in a workspace.
    pub fn list(conn: &Connection, workspace: &WorkspaceId) -> StorageResult<Vec<Collection>> {
        let mut stmt = conn.prepare(
            "SELECT id, workspace_id, parent_id, name, description, auth_json,
                    headers_json, variables_json, sort_order, created_at, updated_at,
                    rev, dirty, deleted_at
             FROM collections
             WHERE workspace_id = ?1 AND deleted_at IS NULL
             ORDER BY sort_order, created_at",
        )?;
        let rows = stmt.query_map([workspace.as_str()], map_collection)?;
        rows.collect::<Result<Vec<_>, _>>()?.into_iter().collect()
    }

    /// One collection by id.
    pub fn get(conn: &Connection, id: &CollectionId) -> StorageResult<Collection> {
        let mut stmt = conn.prepare(
            "SELECT id, workspace_id, parent_id, name, description, auth_json,
                    headers_json, variables_json, sort_order, created_at, updated_at,
                    rev, dirty, deleted_at
             FROM collections WHERE id = ?1 AND deleted_at IS NULL",
        )?;
        stmt.query_row([id.as_str()], map_collection)
            .optional()?
            .transpose()?
            .ok_or_else(|| StorageError::NotFound {
                entity: "Collection",
                id: id.to_string(),
            })
    }

    /// Inserts or replaces a collection.
    pub fn save(conn: &Connection, collection: &Collection) -> StorageResult<()> {
        if collection.name.trim().is_empty() {
            return Err(StorageError::Invalid("a collection needs a name".into()));
        }
        if collection.parent_id.as_ref() == Some(&collection.id) {
            return Err(StorageError::Invalid(
                "a folder cannot be its own parent".into(),
            ));
        }
        if let Some(parent) = &collection.parent_id
            && Self::would_create_cycle(conn, &collection.id, parent)?
        {
            // Without this check a drag-and-drop could detach a subtree from
            // the root entirely, making it invisible and unrecoverable.
            return Err(StorageError::Invalid(
                "that move would put a folder inside itself".into(),
            ));
        }

        conn.execute(
            "INSERT INTO collections
                (id, workspace_id, parent_id, name, description, auth_json,
                 headers_json, variables_json, sort_order, created_at, updated_at,
                 rev, dirty, deleted_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,1,?13)
             ON CONFLICT(id) DO UPDATE SET
                parent_id      = excluded.parent_id,
                name           = excluded.name,
                description    = excluded.description,
                auth_json      = excluded.auth_json,
                headers_json   = excluded.headers_json,
                variables_json = excluded.variables_json,
                sort_order     = excluded.sort_order,
                updated_at     = excluded.updated_at,
                dirty          = 1,
                deleted_at     = excluded.deleted_at",
            params![
                collection.id.as_str(),
                collection.workspace_id.as_str(),
                collection.parent_id.as_ref().map(|p| p.as_str()),
                collection.name.trim(),
                collection.description,
                encode_json("auth_json", &collection.auth)?,
                encode_json("headers_json", &collection.headers)?,
                encode_json("variables_json", &collection.variables)?,
                collection.sort_order,
                collection.created_at.as_millis(),
                collection.updated_at.as_millis(),
                collection.revision.rev,
                collection.revision.deleted_at.map(|t| t.as_millis()),
            ],
        )?;
        Ok(())
    }

    /// Tombstones a collection, its descendant folders and their requests.
    pub fn delete(conn: &Connection, id: &CollectionId) -> StorageResult<()> {
        let now = Timestamp::now().as_millis();
        let mut ids = vec![id.clone()];
        let mut index = 0;

        // Breadth-first walk rather than a recursive CTE: the depth is small,
        // and this keeps the deletion explicit and easy to follow.
        while index < ids.len() {
            let current = ids[index].clone();
            index += 1;
            let mut stmt = conn.prepare(
                "SELECT id FROM collections WHERE parent_id = ?1 AND deleted_at IS NULL",
            )?;
            let children = stmt.query_map([current.as_str()], |row| {
                Ok(CollectionId::from(row.get::<_, String>(0)?))
            })?;
            for child in children {
                ids.push(child?);
            }
        }

        for collection_id in &ids {
            conn.execute(
                "UPDATE requests SET deleted_at = ?1, updated_at = ?1, dirty = 1
                 WHERE collection_id = ?2 AND deleted_at IS NULL",
                params![now, collection_id.as_str()],
            )?;
            conn.execute(
                "UPDATE collections SET deleted_at = ?1, updated_at = ?1, dirty = 1
                 WHERE id = ?2 AND deleted_at IS NULL",
                params![now, collection_id.as_str()],
            )?;
        }

        Ok(())
    }

    /// True when making `parent` the parent of `id` would form a cycle.
    fn would_create_cycle(
        conn: &Connection,
        id: &CollectionId,
        parent: &CollectionId,
    ) -> StorageResult<bool> {
        let mut cursor = Some(parent.clone());
        let mut hops = 0;
        while let Some(current) = cursor {
            if &current == id {
                return Ok(true);
            }
            // A corrupted parent chain must not spin forever.
            hops += 1;
            if hops > 1000 {
                return Ok(true);
            }
            cursor = conn
                .query_row(
                    "SELECT parent_id FROM collections WHERE id = ?1",
                    [current.as_str()],
                    |row| row.get::<_, Option<String>>(0),
                )
                .optional()?
                .flatten()
                .map(CollectionId::from);
        }
        Ok(false)
    }
}

fn map_collection(row: &Row<'_>) -> rusqlite::Result<StorageResult<Collection>> {
    Ok((|| {
        Ok(Collection {
            id: CollectionId::from(row.get::<_, String>(0)?),
            workspace_id: WorkspaceId::from(row.get::<_, String>(1)?),
            parent_id: row.get::<_, Option<String>>(2)?.map(CollectionId::from),
            name: row.get(3)?,
            description: row.get(4)?,
            auth: decode_json::<AuthConfig>("auth_json", &row.get::<_, String>(5)?)?,
            headers: decode_json::<Vec<KeyValue>>("headers_json", &row.get::<_, String>(6)?)?,
            variables: decode_json::<Vec<Variable>>("variables_json", &row.get::<_, String>(7)?)?,
            sort_order: row.get(8)?,
            created_at: Timestamp(row.get(9)?),
            updated_at: Timestamp(row.get(10)?),
            revision: Revision {
                rev: row.get(11)?,
                dirty: row.get::<_, i64>(12)? != 0,
                deleted_at: row.get::<_, Option<i64>>(13)?.map(Timestamp),
            },
        })
    })())
}

/// Reads and writes saved requests.
pub struct RequestRepo;

const REQUEST_COLUMNS: &str = "id, workspace_id, collection_id, name, description, method, url,
     headers_json, query_params_json, path_params_json, body_json, auth_json,
     settings_json, scripts_json, tags_json, sort_order, created_at, updated_at,
     rev, dirty, deleted_at";

impl RequestRepo {
    /// One request by id.
    pub fn get(conn: &Connection, id: &RequestId) -> StorageResult<RequestDoc> {
        let sql =
            format!("SELECT {REQUEST_COLUMNS} FROM requests WHERE id = ?1 AND deleted_at IS NULL");
        let mut stmt = conn.prepare(&sql)?;
        stmt.query_row([id.as_str()], map_request)
            .optional()?
            .transpose()?
            .ok_or_else(|| StorageError::NotFound {
                entity: "Request",
                id: id.to_string(),
            })
    }

    /// Every live request in a workspace.
    pub fn list(conn: &Connection, workspace: &WorkspaceId) -> StorageResult<Vec<RequestDoc>> {
        let sql = format!(
            "SELECT {REQUEST_COLUMNS} FROM requests
             WHERE workspace_id = ?1 AND deleted_at IS NULL
             ORDER BY sort_order, created_at"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([workspace.as_str()], map_request)?;
        rows.collect::<Result<Vec<_>, _>>()?.into_iter().collect()
    }

    /// Inserts or replaces a request.
    pub fn save(conn: &Connection, doc: &RequestDoc) -> StorageResult<()> {
        if doc.name.trim().is_empty() {
            return Err(StorageError::Invalid("a request needs a name".into()));
        }

        conn.execute(
            "INSERT INTO requests
                (id, workspace_id, collection_id, name, description, method, url,
                 headers_json, query_params_json, path_params_json, body_json, auth_json,
                 settings_json, scripts_json, tags_json, sort_order, created_at, updated_at,
                 rev, dirty, deleted_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,1,?20)
             ON CONFLICT(id) DO UPDATE SET
                collection_id     = excluded.collection_id,
                name              = excluded.name,
                description       = excluded.description,
                method            = excluded.method,
                url               = excluded.url,
                headers_json      = excluded.headers_json,
                query_params_json = excluded.query_params_json,
                path_params_json  = excluded.path_params_json,
                body_json         = excluded.body_json,
                auth_json         = excluded.auth_json,
                settings_json     = excluded.settings_json,
                scripts_json      = excluded.scripts_json,
                tags_json         = excluded.tags_json,
                sort_order        = excluded.sort_order,
                updated_at        = excluded.updated_at,
                dirty             = 1,
                deleted_at        = excluded.deleted_at",
            params![
                doc.id.as_str(),
                doc.workspace_id.as_str(),
                doc.collection_id.as_ref().map(|c| c.as_str()),
                doc.name.trim(),
                doc.description,
                doc.method.as_str(),
                doc.url,
                encode_json("headers_json", &doc.headers)?,
                encode_json("query_params_json", &doc.query_params)?,
                encode_json("path_params_json", &doc.path_params)?,
                encode_json("body_json", &doc.body)?,
                encode_json("auth_json", &doc.auth)?,
                encode_json("settings_json", &doc.settings)?,
                encode_json("scripts_json", &doc.scripts)?,
                encode_json("tags_json", &doc.tags)?,
                doc.sort_order,
                doc.created_at.as_millis(),
                doc.updated_at.as_millis(),
                doc.revision.rev,
                doc.revision.deleted_at.map(|t| t.as_millis()),
            ],
        )?;
        Ok(())
    }

    /// Tombstones a request.
    pub fn delete(conn: &Connection, id: &RequestId) -> StorageResult<()> {
        let now = Timestamp::now().as_millis();
        let changed = conn.execute(
            "UPDATE requests SET deleted_at = ?1, updated_at = ?1, dirty = 1
             WHERE id = ?2 AND deleted_at IS NULL",
            params![now, id.as_str()],
        )?;
        if changed == 0 {
            return Err(StorageError::NotFound {
                entity: "Request",
                id: id.to_string(),
            });
        }
        Ok(())
    }

    /// Copies a request, giving the copy a new id and an unused name.
    pub fn duplicate(conn: &Connection, id: &RequestId) -> StorageResult<RequestDoc> {
        let original = Self::get(conn, id)?;
        let now = Timestamp::now();
        let copy = RequestDoc {
            id: RequestId::new(),
            name: format!("{} copy", original.name),
            sort_order: original.sort_order + 0.5,
            created_at: now,
            updated_at: now,
            revision: Revision::default(),
            ..original
        };
        Self::save(conn, &copy)?;
        Ok(copy)
    }

    /// Moves a request into a collection at a given position.
    pub fn move_to(
        conn: &Connection,
        id: &RequestId,
        collection: Option<&CollectionId>,
        sort_order: f64,
    ) -> StorageResult<()> {
        let changed = conn.execute(
            "UPDATE requests SET collection_id = ?1, sort_order = ?2, updated_at = ?3, dirty = 1
             WHERE id = ?4 AND deleted_at IS NULL",
            params![
                collection.map(|c| c.as_str()),
                sort_order,
                Timestamp::now().as_millis(),
                id.as_str()
            ],
        )?;
        if changed == 0 {
            return Err(StorageError::NotFound {
                entity: "Request",
                id: id.to_string(),
            });
        }
        Ok(())
    }
}

fn map_request(row: &Row<'_>) -> rusqlite::Result<StorageResult<RequestDoc>> {
    Ok((|| {
        Ok(RequestDoc {
            id: RequestId::from(row.get::<_, String>(0)?),
            workspace_id: WorkspaceId::from(row.get::<_, String>(1)?),
            collection_id: row.get::<_, Option<String>>(2)?.map(CollectionId::from),
            name: row.get(3)?,
            description: row.get(4)?,
            method: HttpMethod::from(row.get::<_, String>(5)?),
            url: row.get(6)?,
            headers: decode_json("headers_json", &row.get::<_, String>(7)?)?,
            query_params: decode_json("query_params_json", &row.get::<_, String>(8)?)?,
            path_params: decode_json("path_params_json", &row.get::<_, String>(9)?)?,
            body: decode_json::<RequestBody>("body_json", &row.get::<_, String>(10)?)?,
            auth: decode_json::<AuthConfig>("auth_json", &row.get::<_, String>(11)?)?,
            settings: decode_json::<RequestSettings>("settings_json", &row.get::<_, String>(12)?)?,
            scripts: decode_json::<RequestScripts>("scripts_json", &row.get::<_, String>(13)?)?,
            tags: decode_json("tags_json", &row.get::<_, String>(14)?)?,
            sort_order: row.get(15)?,
            created_at: Timestamp(row.get(16)?),
            updated_at: Timestamp(row.get(17)?),
            revision: Revision {
                rev: row.get(18)?,
                dirty: row.get::<_, i64>(19)? != 0,
                deleted_at: row.get::<_, Option<i64>>(20)?.map(Timestamp),
            },
        })
    })())
}

/// Builds the sidebar tree.
pub struct TreeRepo;

impl TreeRepo {
    /// Every folder and request in a workspace, flattened.
    pub fn tree(conn: &Connection, workspace: &WorkspaceId) -> StorageResult<Vec<TreeNode>> {
        let mut nodes = Vec::new();

        let mut folders = conn.prepare(
            "SELECT c.id, c.parent_id, c.name, c.sort_order,
                    (SELECT COUNT(*) FROM collections cc
                       WHERE cc.parent_id = c.id AND cc.deleted_at IS NULL)
                  + (SELECT COUNT(*) FROM requests r
                       WHERE r.collection_id = c.id AND r.deleted_at IS NULL)
             FROM collections c
             WHERE c.workspace_id = ?1 AND c.deleted_at IS NULL
             ORDER BY c.sort_order, c.name",
        )?;
        let folder_rows = folders.query_map([workspace.as_str()], |row| {
            Ok(TreeNode::Folder {
                id: CollectionId::from(row.get::<_, String>(0)?),
                parent_id: row.get::<_, Option<String>>(1)?.map(CollectionId::from),
                name: row.get(2)?,
                sort_order: row.get(3)?,
                child_count: row.get::<_, i64>(4)? as u32,
            })
        })?;
        for node in folder_rows {
            nodes.push(node?);
        }

        let mut requests = conn.prepare(
            "SELECT id, collection_id, name, method, url, sort_order
             FROM requests
             WHERE workspace_id = ?1 AND deleted_at IS NULL
             ORDER BY sort_order, name",
        )?;
        let request_rows = requests.query_map([workspace.as_str()], |row| {
            Ok(TreeNode::Request {
                id: RequestId::from(row.get::<_, String>(0)?),
                parent_id: row.get::<_, Option<String>>(1)?.map(CollectionId::from),
                name: row.get(2)?,
                method: HttpMethod::from(row.get::<_, String>(3)?),
                url: row.get(4)?,
                sort_order: row.get(5)?,
            })
        })?;
        for node in request_rows {
            nodes.push(node?);
        }

        Ok(nodes)
    }
}

/// Reads and writes environments.
pub struct EnvironmentRepo;

impl EnvironmentRepo {
    /// Every live environment in a workspace.
    pub fn list(conn: &Connection, workspace: &WorkspaceId) -> StorageResult<Vec<Environment>> {
        let mut stmt = conn.prepare(
            "SELECT id, workspace_id, name, variables_json, color, sort_order,
                    created_at, updated_at, rev, dirty, deleted_at
             FROM environments
             WHERE workspace_id = ?1 AND deleted_at IS NULL
             ORDER BY sort_order, created_at",
        )?;
        let rows = stmt.query_map([workspace.as_str()], map_environment)?;
        rows.collect::<Result<Vec<_>, _>>()?.into_iter().collect()
    }

    /// One environment by id.
    pub fn get(conn: &Connection, id: &EnvironmentId) -> StorageResult<Environment> {
        let mut stmt = conn.prepare(
            "SELECT id, workspace_id, name, variables_json, color, sort_order,
                    created_at, updated_at, rev, dirty, deleted_at
             FROM environments WHERE id = ?1 AND deleted_at IS NULL",
        )?;
        stmt.query_row([id.as_str()], map_environment)
            .optional()?
            .transpose()?
            .ok_or_else(|| StorageError::NotFound {
                entity: "Environment",
                id: id.to_string(),
            })
    }

    /// Inserts or replaces an environment.
    ///
    /// Secret values are stripped before the row is written: the database is
    /// backed up, exported and copied between machines, and a secret written
    /// here would travel with it. Values live in the OS credential store.
    pub fn save(conn: &Connection, environment: &Environment) -> StorageResult<()> {
        if environment.name.trim().is_empty() {
            return Err(StorageError::Invalid("an environment needs a name".into()));
        }

        let safe = environment.redacted();

        conn.execute(
            "INSERT INTO environments
                (id, workspace_id, name, variables_json, color, sort_order,
                 created_at, updated_at, rev, dirty, deleted_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,1,?10)
             ON CONFLICT(id) DO UPDATE SET
                name           = excluded.name,
                variables_json = excluded.variables_json,
                color          = excluded.color,
                sort_order     = excluded.sort_order,
                updated_at     = excluded.updated_at,
                dirty          = 1,
                deleted_at     = excluded.deleted_at",
            params![
                safe.id.as_str(),
                safe.workspace_id.as_str(),
                safe.name.trim(),
                encode_json("variables_json", &safe.variables)?,
                safe.color,
                safe.sort_order,
                safe.created_at.as_millis(),
                safe.updated_at.as_millis(),
                safe.revision.rev,
                safe.revision.deleted_at.map(|t| t.as_millis()),
            ],
        )?;
        Ok(())
    }

    /// Tombstones an environment.
    pub fn delete(conn: &Connection, id: &EnvironmentId) -> StorageResult<()> {
        let now = Timestamp::now().as_millis();
        let changed = conn.execute(
            "UPDATE environments SET deleted_at = ?1, updated_at = ?1, dirty = 1
             WHERE id = ?2 AND deleted_at IS NULL",
            params![now, id.as_str()],
        )?;
        if changed == 0 {
            return Err(StorageError::NotFound {
                entity: "Environment",
                id: id.to_string(),
            });
        }
        Ok(())
    }
}

fn map_environment(row: &Row<'_>) -> rusqlite::Result<StorageResult<Environment>> {
    Ok((|| {
        Ok(Environment {
            id: EnvironmentId::from(row.get::<_, String>(0)?),
            workspace_id: WorkspaceId::from(row.get::<_, String>(1)?),
            name: row.get(2)?,
            variables: decode_json("variables_json", &row.get::<_, String>(3)?)?,
            color: row.get(4)?,
            sort_order: row.get(5)?,
            created_at: Timestamp(row.get(6)?),
            updated_at: Timestamp(row.get(7)?),
            revision: Revision {
                rev: row.get(8)?,
                dirty: row.get::<_, i64>(9)? != 0,
                deleted_at: row.get::<_, Option<i64>>(10)?.map(Timestamp),
            },
        })
    })())
}

/// Reads and writes the local request history.
pub struct HistoryRepo;

impl HistoryRepo {
    /// Appends an entry.
    pub fn insert(conn: &Connection, entry: &HistoryEntry) -> StorageResult<()> {
        conn.execute(
            "INSERT INTO history_entries
                (id, workspace_id, request_id, name, method, url, status, status_text,
                 duration_ms, response_size, executed_at, error, request_snapshot_json)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
            params![
                entry.id.as_str(),
                entry.workspace_id.as_str(),
                entry.request_id.as_ref().map(|r| r.as_str()),
                entry.name,
                entry.method.as_str(),
                entry.url,
                entry.status,
                entry.status_text,
                entry.duration_ms,
                entry.response_size as i64,
                entry.executed_at.as_millis(),
                entry.error,
                // Redacted again at the boundary. The caller is expected to
                // have done it, but history is the one table that outlives
                // everything, so it does not rely on that.
                encode_json("request_snapshot_json", &entry.request_snapshot.redacted())?,
            ],
        )?;
        Ok(())
    }

    /// The most recent entries, newest first.
    pub fn list(
        conn: &Connection,
        workspace: &WorkspaceId,
        limit: u32,
        offset: u32,
    ) -> StorageResult<Vec<HistoryEntry>> {
        let mut stmt = conn.prepare(
            "SELECT id, workspace_id, request_id, name, method, url, status, status_text,
                    duration_ms, response_size, executed_at, error, request_snapshot_json
             FROM history_entries
             WHERE workspace_id = ?1
             ORDER BY executed_at DESC
             LIMIT ?2 OFFSET ?3",
        )?;
        let rows = stmt.query_map(params![workspace.as_str(), limit, offset], |row| {
            Ok((|| {
                Ok(HistoryEntry {
                    id: HistoryId::from(row.get::<_, String>(0)?),
                    workspace_id: WorkspaceId::from(row.get::<_, String>(1)?),
                    request_id: row.get::<_, Option<String>>(2)?.map(RequestId::from),
                    name: row.get(3)?,
                    method: HttpMethod::from(row.get::<_, String>(4)?),
                    url: row.get(5)?,
                    status: row.get(6)?,
                    status_text: row.get(7)?,
                    duration_ms: row.get(8)?,
                    response_size: row.get::<_, i64>(9)? as u64,
                    executed_at: Timestamp(row.get(10)?),
                    error: row.get(11)?,
                    request_snapshot: Box::new(decode_json::<HttpRequestSpec>(
                        "request_snapshot_json",
                        &row.get::<_, String>(12)?,
                    )?),
                })
            })())
        })?;
        rows.collect::<Result<Vec<_>, _>>()?.into_iter().collect()
    }

    /// Deletes every entry for a workspace.
    pub fn clear(conn: &Connection, workspace: &WorkspaceId) -> StorageResult<usize> {
        Ok(conn.execute(
            "DELETE FROM history_entries WHERE workspace_id = ?1",
            [workspace.as_str()],
        )?)
    }

    /// Trims history to the newest `keep` entries per workspace.
    ///
    /// History grows without bound otherwise: a few hundred requests a day
    /// turns into a database measured in gigabytes within a year.
    pub fn prune(conn: &Connection, workspace: &WorkspaceId, keep: u32) -> StorageResult<usize> {
        Ok(conn.execute(
            "DELETE FROM history_entries
             WHERE workspace_id = ?1
               AND id NOT IN (
                   SELECT id FROM history_entries
                   WHERE workspace_id = ?1
                   ORDER BY executed_at DESC
                   LIMIT ?2
               )",
            params![workspace.as_str(), keep],
        )?)
    }
}

/// Simple key/value application state.
pub struct AppStateRepo;

impl AppStateRepo {
    /// Reads a value.
    pub fn get(conn: &Connection, key: &str) -> StorageResult<Option<String>> {
        Ok(conn
            .query_row("SELECT value FROM app_state WHERE key = ?1", [key], |row| {
                row.get(0)
            })
            .optional()?)
    }

    /// Writes a value.
    pub fn set(conn: &Connection, key: &str, value: &str) -> StorageResult<()> {
        conn.execute(
            "INSERT INTO app_state (key, value, updated_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value,
                                            updated_at = excluded.updated_at",
            params![key, value, Timestamp::now().as_millis()],
        )?;
        Ok(())
    }
}
