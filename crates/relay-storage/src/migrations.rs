//! Schema migrations.
//!
//! Migrations are an ordered list of SQL scripts applied inside a transaction
//! and recorded in `schema_migrations`. They are append-only: an applied
//! migration is never edited, because a user's database has already run it.

use rusqlite::Connection;

use crate::error::StorageError;

/// One forward migration.
pub struct Migration {
    /// Monotonic version. Never reused, never reordered.
    pub version: i64,
    /// Human-readable name, recorded for diagnostics.
    pub name: &'static str,
    /// The SQL to run.
    pub sql: &'static str,
}

/// Every migration, in order.
pub const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        name: "initial_schema",
        sql: include_str!("../migrations/0001_initial.sql"),
    },
    Migration {
        version: 2,
        name: "sync_state",
        sql: include_str!("../migrations/0002_sync.sql"),
    },
    Migration {
        version: 3,
        name: "assistant_conversations",
        sql: include_str!("../migrations/0003_conversations.sql"),
    },
];

/// The schema version this build expects.
pub fn target_version() -> i64 {
    MIGRATIONS.last().map(|m| m.version).unwrap_or(0)
}

/// The version currently recorded in the database.
pub fn current_version(conn: &Connection) -> Result<i64, StorageError> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
            version    INTEGER PRIMARY KEY,
            name       TEXT    NOT NULL,
            applied_at INTEGER NOT NULL
        )",
        [],
    )?;

    let version: Option<i64> =
        conn.query_row("SELECT MAX(version) FROM schema_migrations", [], |row| {
            row.get(0)
        })?;

    Ok(version.unwrap_or(0))
}

/// Applies every migration newer than the recorded version.
///
/// Each runs in its own transaction, so an interrupted upgrade leaves the
/// database at the last fully-applied version rather than half-migrated.
pub fn migrate(conn: &mut Connection) -> Result<i64, StorageError> {
    let from = current_version(conn)?;
    let to = target_version();

    if from > to {
        // The user has opened a database written by a newer Relay. Carrying
        // on would silently corrupt it, so refuse clearly instead.
        return Err(StorageError::SchemaTooNew {
            found: from,
            supported: to,
        });
    }

    for migration in MIGRATIONS.iter().filter(|m| m.version > from) {
        tracing::info!(
            version = migration.version,
            name = migration.name,
            "applying migration"
        );

        let tx = conn.transaction()?;
        tx.execute_batch(migration.sql)?;
        tx.execute(
            "INSERT INTO schema_migrations (version, name, applied_at) VALUES (?1, ?2, ?3)",
            rusqlite::params![
                migration.version,
                migration.name,
                relay_models::Timestamp::now().as_millis()
            ],
        )?;
        tx.commit()?;
    }

    Ok(to)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrations_have_unique_increasing_versions() {
        let mut last = 0;
        for m in MIGRATIONS {
            assert!(m.version > last, "migration {} is out of order", m.version);
            last = m.version;
        }
    }

    #[test]
    fn migrating_an_empty_database_reaches_the_target_version() {
        let mut conn = Connection::open_in_memory().unwrap();
        assert_eq!(current_version(&conn).unwrap(), 0);
        let version = migrate(&mut conn).unwrap();
        assert_eq!(version, target_version());
        assert_eq!(current_version(&conn).unwrap(), target_version());
    }

    #[test]
    fn migrating_twice_is_a_no_op() {
        let mut conn = Connection::open_in_memory().unwrap();
        migrate(&mut conn).unwrap();
        let applied: i64 = conn
            .query_row("SELECT COUNT(*) FROM schema_migrations", [], |r| r.get(0))
            .unwrap();
        migrate(&mut conn).unwrap();
        let after: i64 = conn
            .query_row("SELECT COUNT(*) FROM schema_migrations", [], |r| r.get(0))
            .unwrap();
        assert_eq!(applied, after, "a migration was applied twice");
    }

    #[test]
    fn a_newer_schema_is_refused_rather_than_corrupted() {
        let mut conn = Connection::open_in_memory().unwrap();
        migrate(&mut conn).unwrap();
        conn.execute(
            "INSERT INTO schema_migrations (version, name, applied_at) VALUES (9999, 'future', 0)",
            [],
        )
        .unwrap();

        match migrate(&mut conn) {
            Err(StorageError::SchemaTooNew { found, .. }) => assert_eq!(found, 9999),
            other => panic!("expected SchemaTooNew, got {other:?}"),
        }
    }

    #[test]
    fn every_expected_table_exists_after_migrating() {
        let mut conn = Connection::open_in_memory().unwrap();
        migrate(&mut conn).unwrap();

        for table in [
            "workspaces",
            "collections",
            "requests",
            "environments",
            "history_entries",
            "tabs",
            "app_state",
            "sync_state",
            "outbox",
            "conflicts",
        ] {
            let count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
                    [table],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(count, 1, "table {table} is missing");
        }
    }
}
