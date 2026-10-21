-- Client-side synchronisation state.

-- A conflict the user has to decide about.
--
-- Stored rather than held in memory: a conflict detected during a sync
-- must survive the app being closed, or the user's local edit is quietly
-- stranded as dirty forever with nothing explaining why it never uploads.
CREATE TABLE conflicts (
    id           TEXT    PRIMARY KEY,
    workspace_id TEXT    NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    entity       TEXT    NOT NULL,
    entity_id    TEXT    NOT NULL,
    -- The local document at the moment the conflict was found.
    local_json   TEXT    NOT NULL,
    -- The server's copy, so the UI can show a diff without another fetch.
    server_json  TEXT,
    server_rev   INTEGER NOT NULL,
    server_deleted INTEGER NOT NULL DEFAULT 0,
    detected_at  INTEGER NOT NULL,
    resolved_at  INTEGER,
    -- 'local', 'server' — recorded so the decision is auditable.
    resolution   TEXT
);

CREATE INDEX idx_conflicts_open
    ON conflicts (workspace_id, detected_at)
    WHERE resolved_at IS NULL;

-- At most one mutation in flight per object. A second push for the same
-- object before the first is acknowledged would race, and the later
-- outcome could reconcile the earlier revision.
CREATE UNIQUE INDEX idx_outbox_object
    ON outbox (workspace_id, entity, entity_id);

-- Where the signed-in user's identity is cached, per server. The token
-- itself is never stored here; it lives in the OS credential store.
ALTER TABLE sync_state ADD COLUMN user_id TEXT;
ALTER TABLE sync_state ADD COLUMN user_email TEXT;
ALTER TABLE sync_state ADD COLUMN remote_workspace_id TEXT;
