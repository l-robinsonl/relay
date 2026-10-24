-- What each person has spent on the hosted assistant this period.
--
-- Per user, not per workspace: the allowance follows the person, so
-- somebody in three workspaces has one of them rather than three.
--
-- One row per person per period, incremented in place. A per-request
-- log would be better for spotting abuse and worse for everything
-- else; the rollup is what the cap is enforced against, and the
-- request log can be added beside it if it is ever needed.
CREATE TABLE IF NOT EXISTS relay.assistant_usage (
    user_id       TEXT    NOT NULL REFERENCES relay.users(id) ON DELETE CASCADE,
    -- The first millisecond of the calendar month, UTC. Calendar
    -- rather than the Stripe billing period: the amounts are pennies,
    -- "this month" is what somebody reading a usage line already
    -- assumes, and it needs no subscription to compute — which
    -- matters, because usage is recorded per person and a person can
    -- belong to workspaces on different subscriptions.
    period_start  BIGINT  NOT NULL,
    turns         BIGINT  NOT NULL DEFAULT 0,
    -- Tokens are the real cost and turns are what a person is shown.
    -- Both are kept: the cap is enforced on turns, and the tokens say
    -- whether the price is right.
    input_tokens  BIGINT  NOT NULL DEFAULT 0,
    output_tokens BIGINT  NOT NULL DEFAULT 0,
    cached_tokens BIGINT  NOT NULL DEFAULT 0,
    updated_at    BIGINT  NOT NULL,
    PRIMARY KEY (user_id, period_start)
);
