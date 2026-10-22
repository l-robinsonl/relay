-- Single-use, expiring tokens e-mailed to a user: address verification
-- and password reset.
--
-- Only the SHA-256 of the token is stored, exactly as for sessions. A
-- reader of this table — a backup, a replica, a leaked dump — learns
-- nothing they can present to the server.

CREATE TABLE IF NOT EXISTS relay.email_tokens (
    token_hash  TEXT    PRIMARY KEY,
    user_id     TEXT    NOT NULL REFERENCES relay.users(id) ON DELETE CASCADE,
    -- 'verify_email' or 'reset_password'. A token issued for one must
    -- never be accepted for the other, so the purpose is part of the
    -- lookup rather than a label.
    purpose     TEXT    NOT NULL,
    expires_at  BIGINT  NOT NULL,
    created_at  BIGINT  NOT NULL
);

-- Supports the per-user cooldown, and revoking outstanding tokens when
-- one is used or a password changes.
CREATE INDEX IF NOT EXISTS email_tokens_user_purpose
    ON relay.email_tokens (user_id, purpose, created_at DESC);

CREATE INDEX IF NOT EXISTS email_tokens_expiry
    ON relay.email_tokens (expires_at);
