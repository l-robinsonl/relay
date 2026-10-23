-- The assistant's conversations.
--
-- In the same database as everything else, deliberately. A product
-- arguing that your data is on your disk cannot answer "where do my
-- conversations live?" with the name of someone else's cloud.
--
-- These do not synchronise. A conversation is a working note, often
-- about something half-finished, and pushing it to a shared server
-- would put one person's thinking in front of their colleagues without
-- them choosing to. No revision or tombstone columns for that reason:
-- the sync engine only sees tables that have them.

CREATE TABLE IF NOT EXISTS conversations (
    id            TEXT    PRIMARY KEY,
    workspace_id  TEXT    NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    -- Drawn from the first thing the user said, so the list is
    -- readable without opening each one.
    title         TEXT    NOT NULL DEFAULT '',
    created_at    INTEGER NOT NULL,
    updated_at    INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS conversations_by_workspace
    ON conversations (workspace_id, updated_at DESC);

CREATE TABLE IF NOT EXISTS conversation_messages (
    id              TEXT    PRIMARY KEY,
    conversation_id TEXT    NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
    -- 'user' or 'assistant'.
    role            TEXT    NOT NULL,
    body            TEXT    NOT NULL DEFAULT '',
    -- Tool calls and their results, as JSON. Kept with the message
    -- rather than in their own table because they are only ever read
    -- and written together with it, and because replaying a
    -- conversation to a model needs them in exactly this order.
    tool_calls      TEXT    NOT NULL DEFAULT '[]',
    tool_results    TEXT    NOT NULL DEFAULT '[]',
    -- Monotonic within a conversation. Not a timestamp: two messages
    -- in the same millisecond are ordinary during a tool loop.
    position        INTEGER NOT NULL,
    created_at      INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS conversation_messages_in_order
    ON conversation_messages (conversation_id, position);
