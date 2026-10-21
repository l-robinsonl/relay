-- Relay sync server, SQLite schema.
--
-- The reference implementation: the protocol is developed and tested
-- against this, and the Postgres schema mirrors it.
--
-- Timestamps are integer milliseconds since the Unix epoch, UTC, matching
-- the desktop database so a value never needs converting as it syncs.

CREATE TABLE users (
    id                TEXT    PRIMARY KEY,
    email             TEXT    NOT NULL,
    -- Lower-cased copy, uniquely indexed: addresses are compared without
    -- regard to case, but the form the user typed is what we show back.
    email_normalised  TEXT    NOT NULL UNIQUE,
    password_hash     TEXT    NOT NULL,
    display_name      TEXT    NOT NULL DEFAULT '',
    is_server_admin   INTEGER NOT NULL DEFAULT 0,
    email_verified_at INTEGER,
    created_at        INTEGER NOT NULL
);

-- Only the hash of a session token is stored, so a database disclosure
-- does not hand over live sessions.
CREATE TABLE sessions (
    token_hash   TEXT    PRIMARY KEY,
    user_id      TEXT    NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    created_at   INTEGER NOT NULL,
    expires_at   INTEGER NOT NULL,
    last_seen_at INTEGER NOT NULL
);

CREATE INDEX idx_sessions_user ON sessions (user_id);
CREATE INDEX idx_sessions_expiry ON sessions (expires_at);

CREATE TABLE workspaces (
    id         TEXT    PRIMARY KEY,
    name       TEXT    NOT NULL,
    -- Monotonic per workspace. Every accepted mutation bumps it, and the
    -- new value becomes the affected object's revision.
    change_seq INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE TABLE memberships (
    workspace_id TEXT    NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    user_id      TEXT    NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    role         TEXT    NOT NULL CHECK (role IN ('viewer', 'editor', 'admin')),
    created_at   INTEGER NOT NULL,
    PRIMARY KEY (workspace_id, user_id)
);

CREATE INDEX idx_memberships_user ON memberships (user_id);

-- Every synchronised object, whatever its kind. One table rather than one
-- per kind: the sync protocol treats them identically — fetch by revision,
-- compare a base revision, write a new one — and a single table makes
-- "everything above revision N" one indexed scan instead of four.
CREATE TABLE objects (
    workspace_id TEXT    NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    kind         TEXT    NOT NULL CHECK (kind IN ('workspace', 'collection', 'request', 'environment')),
    id           TEXT    NOT NULL,
    rev          INTEGER NOT NULL,
    deleted      INTEGER NOT NULL DEFAULT 0,
    -- NULL for a tombstone: a deleted object's contents help nobody, and
    -- keeping them would mean deletion never reclaimed anything.
    payload      TEXT,
    updated_at   INTEGER NOT NULL,
    updated_by   TEXT,
    PRIMARY KEY (workspace_id, kind, id)
);

-- The pull query: everything in a workspace above a revision, in order.
CREATE INDEX idx_objects_rev ON objects (workspace_id, rev);

-- Applied mutations, so a batch re-sent after a dropped connection is a
-- no-op rather than a duplicate write.
CREATE TABLE applied_mutations (
    mutation_id  TEXT    PRIMARY KEY,
    workspace_id TEXT    NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    kind         TEXT    NOT NULL,
    object_id    TEXT    NOT NULL,
    rev          INTEGER NOT NULL,
    applied_at   INTEGER NOT NULL
);

CREATE INDEX idx_applied_workspace ON applied_mutations (workspace_id, applied_at);

CREATE TABLE invitations (
    id           TEXT    PRIMARY KEY,
    workspace_id TEXT    NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    email        TEXT    NOT NULL,
    role         TEXT    NOT NULL CHECK (role IN ('viewer', 'editor', 'admin')),
    token_hash   TEXT    NOT NULL UNIQUE,
    invited_by   TEXT    NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    created_at   INTEGER NOT NULL,
    expires_at   INTEGER NOT NULL,
    accepted_at  INTEGER
);

CREATE INDEX idx_invitations_workspace ON invitations (workspace_id);
