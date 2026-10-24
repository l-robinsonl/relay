-- Which plan a subscription is on, so the server knows whether the
-- assistant is included.
--
-- The price id is stored rather than a plan name: Stripe's price is
-- the fact, and a plan is this deployment's reading of it. Keeping
-- the fact means changing what a price includes does not require
-- rewriting rows, and an operator can always see which price a
-- workspace is actually paying.
ALTER TABLE subscriptions ADD COLUMN price_id TEXT;
