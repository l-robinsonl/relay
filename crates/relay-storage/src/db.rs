//! Connection management.

use rusqlite::Connection;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use crate::error::{StorageError, StorageResult};
use crate::migrations;

/// The local database.
///
/// Relay is a single-user desktop application, so contention is low and a
/// single write connection behind a mutex is both simpler and faster than a
/// pool: there is no `SQLITE_BUSY` to handle, no pool exhaustion, and no
/// connection-per-task bookkeeping. Callers that must not block the async
/// runtime use [`Database::call`], which hops to the blocking pool.
#[derive(Clone)]
pub struct Database {
    conn: Arc<Mutex<Connection>>,
    path: Option<PathBuf>,
}

impl std::fmt::Debug for Database {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Database")
            .field("path", &self.path)
            .finish()
    }
}

impl Database {
    /// Opens (or creates) the database at `path` and migrates it.
    pub fn open(path: impl AsRef<Path>) -> StorageResult<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let mut conn = Connection::open(&path)?;
        configure(&conn)?;
        migrations::migrate(&mut conn)?;

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            path: Some(path),
        })
    }

    /// Opens a private in-memory database, migrated. For tests.
    pub fn open_in_memory() -> StorageResult<Self> {
        let mut conn = Connection::open_in_memory()?;
        configure(&conn)?;
        migrations::migrate(&mut conn)?;

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            path: None,
        })
    }

    /// Where the database lives, if it is not in memory.
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Runs `f` against the connection, holding the lock for its duration.
    pub fn with<T>(&self, f: impl FnOnce(&Connection) -> StorageResult<T>) -> StorageResult<T> {
        let guard = self.lock()?;
        f(&guard)
    }

    /// Runs `f` inside a transaction, committing on `Ok` and rolling back on
    /// `Err`. Multi-row writes go through here so a failure cannot leave the
    /// database half-updated.
    pub fn transaction<T>(
        &self,
        f: impl FnOnce(&rusqlite::Transaction<'_>) -> StorageResult<T>,
    ) -> StorageResult<T> {
        let mut guard = self.lock()?;
        let tx = guard.transaction()?;
        let value = f(&tx)?;
        tx.commit()?;
        Ok(value)
    }

    /// Runs a blocking database closure on the blocking thread pool.
    ///
    /// SQLite calls are synchronous; running them directly on an async worker
    /// would stall every other task sharing that thread, which on a desktop
    /// app shows up as the UI hitching during a large write.
    pub async fn call<T, F>(&self, f: F) -> StorageResult<T>
    where
        T: Send + 'static,
        F: FnOnce(&Connection) -> StorageResult<T> + Send + 'static,
    {
        let db = self.clone();
        tokio::task::spawn_blocking(move || db.with(f))
            .await
            .map_err(|e| StorageError::TaskJoin(e.to_string()))?
    }

    /// Writes a consistent snapshot of the database to `dest`.
    ///
    /// Uses SQLite's online backup API rather than copying the file, which
    /// would race with WAL checkpoints and could produce a torn copy.
    pub fn backup_to(&self, dest: impl AsRef<Path>) -> StorageResult<()> {
        let dest = dest.as_ref();
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let guard = self.lock()?;
        guard.backup(rusqlite::MAIN_DB, dest, None)?;
        Ok(())
    }

    fn lock(&self) -> StorageResult<MutexGuard<'_, Connection>> {
        self.conn.lock().map_err(|_| {
            // A poisoned mutex means a previous caller panicked mid-write.
            // Continuing could observe a partially applied change.
            StorageError::Invalid(
                "the database lock was poisoned by an earlier failure; restart Relay".into(),
            )
        })
    }
}

/// Applies the pragmas Relay depends on.
fn configure(conn: &Connection) -> StorageResult<()> {
    // WAL lets reads proceed during a write, which keeps the sidebar
    // responsive while a large import is running.
    conn.pragma_update(None, "journal_mode", "WAL")?;
    // NORMAL is the right durability point for a working set that is also
    // synchronised: a crash can lose the last transaction, not the database.
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.pragma_update(None, "busy_timeout", 5000)?;
    // Keeps temp b-trees off disk during sorts on the history view.
    conn.pragma_update(None, "temp_store", "MEMORY")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_in_memory_database_is_migrated_on_open() {
        let db = Database::open_in_memory().unwrap();
        let version = db.with(migrations::current_version).unwrap();
        assert_eq!(version, migrations::target_version());
    }

    #[test]
    fn foreign_keys_are_enforced() {
        let db = Database::open_in_memory().unwrap();
        let result = db.with(|conn| {
            conn.execute(
                "INSERT INTO collections (id, workspace_id, name, created_at, updated_at)
                 VALUES ('c1', 'nonexistent-workspace', 'X', 0, 0)",
                [],
            )
            .map_err(Into::into)
        });
        assert!(result.is_err(), "a dangling foreign key was accepted");
    }

    #[test]
    fn a_failed_transaction_rolls_back() {
        let db = Database::open_in_memory().unwrap();
        let outcome: StorageResult<()> = db.transaction(|tx| {
            tx.execute(
                "INSERT INTO workspaces (id, name, created_at, updated_at)
                 VALUES ('w1', 'Doomed', 0, 0)",
                [],
            )?;
            Err(StorageError::Invalid("deliberate".into()))
        });
        assert!(outcome.is_err());

        let count: i64 = db
            .with(|conn| {
                conn.query_row("SELECT COUNT(*) FROM workspaces", [], |r| r.get(0))
                    .map_err(Into::into)
            })
            .unwrap();
        assert_eq!(count, 0, "the rolled-back row survived");
    }

    #[test]
    fn a_file_database_persists_across_opens() {
        let dir = std::env::temp_dir().join(format!("relay-db-{}", std::process::id()));
        let path = dir.join("relay.db");
        std::fs::remove_dir_all(&dir).ok();

        {
            let db = Database::open(&path).unwrap();
            db.with(|conn| {
                conn.execute(
                    "INSERT INTO workspaces (id, name, created_at, updated_at)
                     VALUES ('w1', 'Persisted', 1, 1)",
                    [],
                )
                .map_err(Into::into)
            })
            .unwrap();
        }

        let db = Database::open(&path).unwrap();
        let name: String = db
            .with(|conn| {
                conn.query_row("SELECT name FROM workspaces WHERE id='w1'", [], |r| {
                    r.get(0)
                })
                .map_err(Into::into)
            })
            .unwrap();
        assert_eq!(name, "Persisted");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn backup_produces_a_readable_copy() {
        let dir = std::env::temp_dir().join(format!("relay-backup-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        let db = Database::open_in_memory().unwrap();
        db.with(|conn| {
            conn.execute(
                "INSERT INTO workspaces (id, name, created_at, updated_at)
                 VALUES ('w1', 'Backed up', 1, 1)",
                [],
            )
            .map_err(Into::into)
        })
        .unwrap();

        let dest = dir.join("backup.db");
        db.backup_to(&dest).unwrap();

        let restored = Database::open(&dest).unwrap();
        let name: String = restored
            .with(|conn| {
                conn.query_row("SELECT name FROM workspaces WHERE id='w1'", [], |r| {
                    r.get(0)
                })
                .map_err(Into::into)
            })
            .unwrap();
        assert_eq!(name, "Backed up");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn call_runs_on_the_blocking_pool() {
        let db = Database::open_in_memory().unwrap();
        let count: i64 = db
            .call(|conn| {
                conn.query_row("SELECT COUNT(*) FROM workspaces", [], |r| r.get(0))
                    .map_err(Into::into)
            })
            .await
            .unwrap();
        assert_eq!(count, 0);
    }
}
