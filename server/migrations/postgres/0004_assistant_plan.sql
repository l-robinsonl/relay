-- Which plan a subscription is on. See the SQLite copy for why the
-- price id is stored rather than a plan name.
ALTER TABLE relay.subscriptions ADD COLUMN IF NOT EXISTS price_id TEXT;
