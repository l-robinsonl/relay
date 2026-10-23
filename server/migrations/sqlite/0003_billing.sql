-- Subscriptions, for the managed tier.
--
-- A self-hosted server creates these tables and never writes a row to
-- them: with no Stripe configuration, every workspace is unmetered.
-- They exist in both backends so the schema does not fork.

CREATE TABLE IF NOT EXISTS subscriptions (
    workspace_id       TEXT    PRIMARY KEY REFERENCES workspaces(id) ON DELETE CASCADE,
    customer_id        TEXT    NOT NULL,
    subscription_id    TEXT,
    -- Stripe's own spelling, stored as given. Mapping it on the way in
    -- would lose a status this build has not heard of.
    status             TEXT    NOT NULL,
    -- BIGINT, not INTEGER: Postgres INTEGER is INT4 and sqlx refuses to
    -- decode it as i64, while SQLite is dynamically typed and does
    -- not care. Declaring the wider type means both backends read the
    -- column the same way.
    seats              BIGINT  NOT NULL DEFAULT 0,
    current_period_end BIGINT,
    updated_at         BIGINT  NOT NULL
);

CREATE INDEX IF NOT EXISTS subscriptions_by_customer
    ON subscriptions (customer_id);

-- Stripe retries a webhook until it is acknowledged, so the same event
-- arrives more than once. Recording what has been applied makes
-- replaying one a no-op rather than, say, doubling a seat count.
CREATE TABLE IF NOT EXISTS billing_events (
    event_id   TEXT   PRIMARY KEY,
    applied_at BIGINT NOT NULL
);
