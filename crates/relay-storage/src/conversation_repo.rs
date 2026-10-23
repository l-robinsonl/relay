//! Conversations with the assistant.
//!
//! Local only, and not part of synchronisation. A conversation is a
//! working note — often about something half-finished, sometimes about
//! a mistake — and pushing it to a shared server would put one
//! person's thinking in front of their colleagues without them
//! choosing to. The tables carry no revision or tombstone columns,
//! which is also what keeps the sync engine from noticing them.

use relay_models::{Timestamp, WorkspaceId};
use rusqlite::{Connection, params};

use crate::error::StorageResult;

/// A conversation, without its messages.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Conversation {
    /// Its id.
    pub id: String,
    /// Which workspace it belongs to.
    pub workspace_id: WorkspaceId,
    /// Drawn from the first thing the user said.
    pub title: String,
    /// When it started.
    pub created_at: Timestamp,
    /// When it last changed, which is what the list is ordered by.
    pub updated_at: Timestamp,
}

/// One stored message.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredMessage {
    /// Its id.
    pub id: String,
    /// Which conversation.
    pub conversation_id: String,
    /// `user` or `assistant`.
    pub role: String,
    /// What was said.
    pub body: String,
    /// Tool calls, as JSON.
    pub tool_calls: serde_json::Value,
    /// Their results, as JSON.
    pub tool_results: serde_json::Value,
    /// Order within the conversation.
    pub position: i64,
    /// When it was written.
    pub created_at: Timestamp,
}

/// Reading and writing conversations.
pub struct ConversationRepo;

impl ConversationRepo {
    /// Starts a conversation.
    pub fn create(
        conn: &Connection,
        id: &str,
        workspace: &WorkspaceId,
        title: &str,
    ) -> StorageResult<Conversation> {
        let now = Timestamp::now();
        conn.execute(
            "INSERT INTO conversations (id, workspace_id, title, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?4)",
            params![id, workspace.as_str(), title, now.as_millis()],
        )?;

        Ok(Conversation {
            id: id.to_string(),
            workspace_id: workspace.clone(),
            title: title.to_string(),
            created_at: now,
            updated_at: now,
        })
    }

    /// The conversations in a workspace, most recently touched first.
    pub fn list(
        conn: &Connection,
        workspace: &WorkspaceId,
        limit: u32,
    ) -> StorageResult<Vec<Conversation>> {
        let mut statement = conn.prepare(
            "SELECT id, workspace_id, title, created_at, updated_at
             FROM conversations WHERE workspace_id = ?1
             ORDER BY updated_at DESC LIMIT ?2",
        )?;

        let rows = statement.query_map(params![workspace.as_str(), limit], |row| {
            Ok(Conversation {
                id: row.get(0)?,
                workspace_id: WorkspaceId::from(row.get::<_, String>(1)?),
                title: row.get(2)?,
                created_at: Timestamp(row.get(3)?),
                updated_at: Timestamp(row.get(4)?),
            })
        })?;

        rows.collect::<Result<_, _>>().map_err(Into::into)
    }

    /// Renames a conversation and marks it touched.
    pub fn rename(conn: &Connection, id: &str, title: &str) -> StorageResult<()> {
        conn.execute(
            "UPDATE conversations SET title = ?2, updated_at = ?3 WHERE id = ?1",
            params![id, title, Timestamp::now().as_millis()],
        )?;
        Ok(())
    }

    /// Deletes a conversation and its messages.
    ///
    /// A real delete, not a tombstone. Nothing synchronises these, so
    /// there is no other replica to tell — and somebody deleting a
    /// conversation usually means it.
    pub fn delete(conn: &Connection, id: &str) -> StorageResult<()> {
        conn.execute(
            "DELETE FROM conversation_messages WHERE conversation_id = ?1",
            params![id],
        )?;
        conn.execute("DELETE FROM conversations WHERE id = ?1", params![id])?;
        Ok(())
    }

    /// Every message, in the order they were written.
    pub fn messages(conn: &Connection, conversation: &str) -> StorageResult<Vec<StoredMessage>> {
        let mut statement = conn.prepare(
            "SELECT id, conversation_id, role, body, tool_calls, tool_results, position, created_at
             FROM conversation_messages WHERE conversation_id = ?1
             ORDER BY position",
        )?;

        let rows = statement.query_map(params![conversation], |row| {
            Ok(StoredMessage {
                id: row.get(0)?,
                conversation_id: row.get(1)?,
                role: row.get(2)?,
                body: row.get(3)?,
                tool_calls: parse_json(row.get::<_, String>(4)?),
                tool_results: parse_json(row.get::<_, String>(5)?),
                position: row.get(6)?,
                created_at: Timestamp(row.get(7)?),
            })
        })?;

        rows.collect::<Result<_, _>>().map_err(Into::into)
    }

    /// Appends messages and marks the conversation touched.
    ///
    /// One transaction: a turn is several messages — what was asked,
    /// what the model said, what the tools returned — and a half-saved
    /// turn replays to the model as a conversation that makes no sense.
    pub fn append(
        conn: &Connection,
        conversation: &str,
        messages: &[(String, String, serde_json::Value, serde_json::Value)],
    ) -> StorageResult<()> {
        // `unchecked_transaction` so this composes with `Database::with`,
        // which hands out a shared reference.
        let tx = conn.unchecked_transaction()?;

        let next: i64 = tx
            .query_row(
                "SELECT COALESCE(MAX(position), -1) + 1 FROM conversation_messages
                 WHERE conversation_id = ?1",
                params![conversation],
                |row| row.get(0),
            )
            .unwrap_or(0);

        let now = Timestamp::now().as_millis();

        for (offset, (role, body, calls, results)) in messages.iter().enumerate() {
            tx.execute(
                "INSERT INTO conversation_messages
                    (id, conversation_id, role, body, tool_calls, tool_results, position, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    uuid::Uuid::now_v7().to_string(),
                    conversation,
                    role,
                    body,
                    calls.to_string(),
                    results.to_string(),
                    next + offset as i64,
                    now,
                ],
            )?;
        }

        tx.execute(
            "UPDATE conversations SET updated_at = ?2 WHERE id = ?1",
            params![conversation, now],
        )?;

        tx.commit()?;
        Ok(())
    }
}

/// Reads stored JSON, treating corruption as absence.
///
/// A message whose tool calls will not parse is still worth showing:
/// losing the whole conversation because one column was mangled is a
/// worse outcome than losing the calls from one turn.
fn parse_json(raw: String) -> serde_json::Value {
    serde_json::from_str(&raw).unwrap_or(serde_json::Value::Array(Vec::new()))
}
