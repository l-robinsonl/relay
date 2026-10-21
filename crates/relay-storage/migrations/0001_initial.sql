-- Relay local database, initial schema.
--
-- Conventions used throughout:
--   * ids are UUIDv7 text, so they sort chronologically
--   * timestamps are integer milliseconds since the Unix epoch, UTC
--   * `rev` is the last server-acknowledged revision; 0 means never synced
--   * `dirty` marks a local edit not yet pushed
--   * deletion is a tombstone (`deleted_at`), never a DELETE, so that a
--     deletion made offline can be synchronised rather than lost
--   * structured sub-objects are JSON columns: they are always read and
--     written with their parent and never queried independently

CREATE TABLE workspaces (
    id              TEXT    PRIMARY KEY,
    name            TEXT    NOT NULL,
    description     TEXT    NOT NULL DEFAULT '',
    variables_json  TEXT    NOT NULL DEFAULT '[]',
    sync_json       TEXT,
    sort_order      REAL    NOT NULL DEFAULT 0,
    created_at      INTEGER NOT NULL,
    updated_at      INTEGER NOT NULL,
    rev             INTEGER NOT NULL DEFAULT 0,
    dirty           INTEGER NOT NULL DEFAULT 0,
    deleted_at      INTEGER
);

CREATE TABLE collections (
    id              TEXT    PRIMARY KEY,
    workspace_id    TEXT    NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    parent_id       TEXT    REFERENCES collections(id) ON DELETE CASCADE,
    name            TEXT    NOT NULL,
    description     TEXT    NOT NULL DEFAULT '',
    auth_json       TEXT    NOT NULL DEFAULT '{"type":"none"}',
    headers_json    TEXT    NOT NULL DEFAULT '[]',
    variables_json  TEXT    NOT NULL DEFAULT '[]',
    sort_order      REAL    NOT NULL DEFAULT 0,
    created_at      INTEGER NOT NULL,
    updated_at      INTEGER NOT NULL,
    rev             INTEGER NOT NULL DEFAULT 0,
    dirty           INTEGER NOT NULL DEFAULT 0,
    deleted_at      INTEGER
);

CREATE TABLE requests (
    id                TEXT    PRIMARY KEY,
    workspace_id      TEXT    NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    collection_id     TEXT    REFERENCES collections(id) ON DELETE CASCADE,
    name              TEXT    NOT NULL,
    description       TEXT    NOT NULL DEFAULT '',
    method            TEXT    NOT NULL DEFAULT 'GET',
    url               TEXT    NOT NULL DEFAULT '',
    headers_json      TEXT    NOT NULL DEFAULT '[]',
    query_params_json TEXT    NOT NULL DEFAULT '[]',
    path_params_json  TEXT    NOT NULL DEFAULT '[]',
    body_json         TEXT    NOT NULL DEFAULT '{"mode":"none"}',
    auth_json         TEXT    NOT NULL DEFAULT '{"type":"inherit"}',
    settings_json     TEXT    NOT NULL DEFAULT '{}',
    scripts_json      TEXT    NOT NULL DEFAULT '{}',
    tags_json         TEXT    NOT NULL DEFAULT '[]',
    sort_order        REAL    NOT NULL DEFAULT 0,
    created_at        INTEGER NOT NULL,
    updated_at        INTEGER NOT NULL,
    rev               INTEGER NOT NULL DEFAULT 0,
    dirty             INTEGER NOT NULL DEFAULT 0,
    deleted_at        INTEGER
);

CREATE TABLE environments (
    id              TEXT    PRIMARY KEY,
    workspace_id    TEXT    NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    name            TEXT    NOT NULL,
    variables_json  TEXT    NOT NULL DEFAULT '[]',
    color           TEXT,
    sort_order      REAL    NOT NULL DEFAULT 0,
    created_at      INTEGER NOT NULL,
    updated_at      INTEGER NOT NULL,
    rev             INTEGER NOT NULL DEFAULT 0,
    dirty           INTEGER NOT NULL DEFAULT 0,
    deleted_at      INTEGER
);

-- Local only. History records what a user actually sent and is never
-- synchronised: it is personal, large, and frequently contains credentials
-- even after redaction of the obvious ones.
CREATE TABLE history_entries (
    id                    TEXT    PRIMARY KEY,
    workspace_id          TEXT    NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    request_id            TEXT,
    name                  TEXT    NOT NULL DEFAULT '',
    method                TEXT    NOT NULL,
    url                   TEXT    NOT NULL,
    status                INTEGER,
    status_text           TEXT,
    duration_ms           REAL    NOT NULL DEFAULT 0,
    response_size         INTEGER NOT NULL DEFAULT 0,
    executed_at           INTEGER NOT NULL,
    error                 TEXT,
    request_snapshot_json TEXT    NOT NULL,
    response_meta_json    TEXT,
    response_body_path    TEXT
);

-- Open editor tabs, so the app reopens exactly as it was left.
CREATE TABLE tabs (
    id           TEXT    PRIMARY KEY,
    workspace_id TEXT    NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    request_id   TEXT    REFERENCES requests(id) ON DELETE CASCADE,
    title        TEXT    NOT NULL DEFAULT '',
    draft_json   TEXT,
    sort_order   REAL    NOT NULL DEFAULT 0,
    is_active    INTEGER NOT NULL DEFAULT 0,
    updated_at   INTEGER NOT NULL
);

CREATE TABLE app_state (
    key        TEXT PRIMARY KEY,
    value      TEXT NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE TABLE sync_state (
    workspace_id    TEXT PRIMARY KEY REFERENCES workspaces(id) ON DELETE CASCADE,
    server_url      TEXT    NOT NULL,
    last_pulled_seq INTEGER NOT NULL DEFAULT 0,
    last_sync_at    INTEGER,
    last_error      TEXT
);

-- Pending local mutations awaiting push. `base_rev` is the revision the edit
-- was made against, which is what lets the server detect a conflict instead
-- of silently overwriting a teammate.
CREATE TABLE outbox (
    seq          INTEGER PRIMARY KEY AUTOINCREMENT,
    mutation_id  TEXT    NOT NULL UNIQUE,
    workspace_id TEXT    NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    entity       TEXT    NOT NULL,
    entity_id    TEXT    NOT NULL,
    op           TEXT    NOT NULL,
    base_rev     INTEGER NOT NULL DEFAULT 0,
    payload_json TEXT    NOT NULL,
    created_at   INTEGER NOT NULL,
    attempts     INTEGER NOT NULL DEFAULT 0,
    last_error   TEXT
);

-- Read paths. The partial indices exclude tombstones, which the UI never
-- shows, keeping the common queries off the deleted rows entirely.
CREATE INDEX idx_collections_tree
    ON collections (workspace_id, parent_id, sort_order)
    WHERE deleted_at IS NULL;

CREATE INDEX idx_requests_tree
    ON requests (workspace_id, collection_id, sort_order)
    WHERE deleted_at IS NULL;

CREATE INDEX idx_requests_workspace
    ON requests (workspace_id)
    WHERE deleted_at IS NULL;

CREATE INDEX idx_environments_workspace
    ON environments (workspace_id, sort_order)
    WHERE deleted_at IS NULL;

CREATE INDEX idx_history_recent
    ON history_entries (workspace_id, executed_at DESC);

CREATE INDEX idx_history_request
    ON history_entries (request_id, executed_at DESC);

CREATE INDEX idx_tabs_workspace
    ON tabs (workspace_id, sort_order);

CREATE INDEX idx_outbox_pending
    ON outbox (workspace_id, seq);

-- Dirty-row lookup drives the sync push; it must not scan the whole table.
CREATE INDEX idx_requests_dirty    ON requests (workspace_id)    WHERE dirty = 1;
CREATE INDEX idx_collections_dirty ON collections (workspace_id) WHERE dirty = 1;
CREATE INDEX idx_environments_dirty ON environments (workspace_id) WHERE dirty = 1;
