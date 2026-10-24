# Hosting the assistant

**Status: designed, not built.** Nothing in this document exists in the
code yet. It is the plan for letting a subscriber use the assistant
without supplying their own API key.

## The decision this encodes

Relay's assistant is bring-your-own-key today. It runs in the desktop
application, talks straight from the user's machine to whichever
provider they chose, and needs no account and no server. That stays.

The paid option adds one thing: **Relay's server holds the key and
forwards the request**, so somebody who does not want to think about
API keys does not have to. £5 per person per month, covering both sync
and the assistant.

Three rules decide most of the detail below.

1. **Bring-your-own-key stays free and unlimited.** It is the escape
   hatch that makes every cap safe to enforce: the answer to anybody
   who hits a limit is "use your own key, it costs nothing", not a
   bigger bill or a throttle. It is also what keeps the product's
   claim true — Relay still works with no account at all.
2. **The provider key never leaves the server.** Same rule as every
   other credential here. The desktop application never sees it; it
   authenticates with the session token it already has.
3. **A cap is not a punishment.** Running out of included usage must
   read as a limit reached, say what the limit was, and offer the key
   field in the same breath. It must never look like a fault.

## What it costs to run

Measured, not estimated. `crates/relay-app/tests/measure_prompt.rs`
prints what one turn sends before the conversation starts:

| | |
|---|---:|
| System prompt | 886 characters |
| Nine tool definitions | 3,630 characters |
| Fixed overhead per request | **~1,200 tokens** |

That overhead is paid on *every* request in a turn, because the
Messages API is stateless and the loop resends the conversation each
step. A useful turn is three or four tool calls, which works out at
roughly **8,000 input and 500 output tokens**.

At current prices, per turn:

| Model | $/MTok in | $/MTok out | Per turn | 1,000 turns |
|---|---:|---:|---:|---:|
| Claude Haiku 5.5 | 0.10 | 0.50 | ~0.08p | ~80p |
| Claude Sonnet 5.5 | 2.00 | 10.00 | ~1.6p (0.9p cached) | ~£9 |
| Claude Opus 5.5 | 4.00 | 20.00 | ~3.3p | ~£33 |

£5 less Stripe's ~28p, less the £2 the sync half is worth, leaves about
**£2.70 of model budget per user per month**. On Haiku that is several
thousand turns. On Sonnet it is about three hundred, and one
enthusiast at two thousand turns costs £18 — a £13 loss on a £5
subscription.

**So: Haiku 5.5 is the default, with a cap around 1,000 turns a
month.** Sonnet is offered to those who want it, at a lower cap. Both
numbers are settings, not constants in the code.

## Shape

```
desktop app                relay-server                  Anthropic
     │                          │                            │
     │ POST /v1/assistant/messages                           │
     │  Authorization: Bearer <session token>                │
     │─────────────────────────>│                            │
     │                          │ 1. resolve user            │
     │                          │ 2. check entitlement       │
     │                          │ 3. check the period cap    │
     │                          │ 4. add the key + caching   │
     │                          │───────────────────────────>│
     │                          │<─ ─ ─ ─ SSE ─ ─ ─ ─ ─ ─ ─ ─│
     │<─ ─ ─ ─ ─ SSE ─ ─ ─ ─ ─ ─│ 5. meter from usage        │
```

The agent loop stays in `relay-agent`, on the user's machine. The
server is a metered pass-through, not an agent host: it never sees a
tool definition it has to understand, never runs a tool, and never
holds conversation state. That matters — conversations stay in the
user's SQLite, which is the answer the product already gives to "where
do my conversations live".

### The client side

`relay-agent`'s provider list gains a third entry beside Anthropic and
OpenAI:

```rust
Provider::Relay { base_url: String, token: String }
```

It speaks the Anthropic wire format to Relay's own endpoint, with the
sync session token instead of an API key. In the settings dialog it
appears as a provider with **no key field** and a line saying it is
included in the subscription.

A workspace that is not connected to a server does not see it at all,
for the same reason the billing panel renders nothing when the server
does not charge.

### Streaming

The assistant streams, so the proxy must. Server-sent events straight
through, with the usage figures read off the final event and written
after the stream closes. A dropped connection mid-stream still bills
what was generated — the provider charges for it either way, and not
metering it is an obvious hole.

## Entitlement

`Subscription` currently carries seats and a status. It gains a plan:

```rust
pub enum Plan {
    /// Sync only. The assistant works, with the person's own key.
    Sync,
    /// Sync and a monthly allowance of assistant usage.
    SyncAndAssistant,
}
```

Derived from the Stripe price id on the subscription, not stored
separately — one source of truth, and a price change cannot leave the
two disagreeing.

`Entitlement` gains the matching fields:

```rust
pub struct Entitlement {
    pub seats: Option<u32>,
    pub active: bool,
    pub notice: Option<String>,
    /// Turns included per period. None means the assistant is not
    /// included; the person uses their own key, as everyone can.
    pub assistant_turns: Option<u32>,
}
```

`past_due` keeps entitling, exactly as it does for sync. A failed card
must not cut somebody off mid-sentence while Stripe is still retrying.

## Metering

One row per user per billing period, incremented in the same
transaction that records the request:

```sql
CREATE TABLE relay.assistant_usage (
    user_id       TEXT    NOT NULL,
    period_start  BIGINT  NOT NULL,   -- epoch millis, from the subscription
    turns         BIGINT  NOT NULL DEFAULT 0,
    input_tokens  BIGINT  NOT NULL DEFAULT 0,
    output_tokens BIGINT  NOT NULL DEFAULT 0,
    cached_tokens BIGINT  NOT NULL DEFAULT 0,
    PRIMARY KEY (user_id, period_start)
);
```

Per user, not per workspace: the cap follows the person, and somebody
in three workspaces has one allowance, not three.

**Count turns, not tokens, in what the user sees.** Tokens are the
real cost and are what the table stores, but "847 of your 1,000
requests this month" is a sentence somebody can act on and "612,000 of
your 800,000 tokens" is not. Enforce on turns; keep tokens for working
out whether the price is right.

## What happens at the cap

The request is refused with `402 Payment Required` — the same status
the seat limit uses, and for the same reason — and a message naming
the limit and the reset date. The chat panel renders it as a notice,
not an error, with two buttons: **use my own key** (opens settings
with the key field focused) and **see usage**.

Nothing else stops. Sync keeps working, the workspace keeps working,
and the assistant keeps working the moment a key is supplied.

## Abuse

An endpoint that turns a session token into model inference is a free
LLM for anyone who gets one. Three defences, all of them needed:

- **The period cap**, above. Bounds the worst case per account to a
  known number of pence.
- **A rate limit per user** — a few requests a second, enough for the
  loop's bursts and not enough to resell. None of the server's
  endpoints rate limit today, so this is new machinery.
- **Registration that is not free in effort.** The cap only bounds the
  damage per account; without e-mail confirmation somebody can have a
  thousand accounts. `RELAY_REQUIRE_VERIFIED_EMAIL` must be on before
  this ships, and it is off today.

Log per-request token counts against the user. Not for billing — the
rollup does that — but because the first sign of abuse is a usage
distribution with a long tail, and that is invisible without the rows.

## Prompt caching

Worth doing on the first version rather than later. The 1,200-token
prefix of system prompt and tool definitions is identical on every
request Relay makes, and cache reads are a tenth of the input price.
On a short turn that prefix is around a third of the input cost.

The proxy adds `cache_control` to the last tool definition, so the
whole `tools` → `system` prefix caches. The client must not reorder
tools between requests — `ToolRegistry` iterates a `BTreeMap`, so the
order is already stable, and a test should pin that it stays so.

## Configuration

```
RELAY_ASSISTANT_KEY            the Anthropic key the server uses
RELAY_ASSISTANT_MODEL          default claude-haiku-5-5
RELAY_ASSISTANT_TURNS          included turns per period, default 1000
```

All three unset means the server hosts no assistant, and
`/v1/assistant/messages` answers that it does not — the same pattern as
billing being off unless every Stripe variable is present. A
self-hosted Relay offers nothing here by default and costs its operator
nothing.

## Order to build it

1. **`Plan` on the subscription and `assistant_turns` on the
   entitlement.** Pure types and store work, testable against both
   backends in the conformance suite, and it lands before anything
   depends on it.
2. **The usage table and the cap check**, with no proxy behind it yet.
   A handler that refuses or allows, and tests for the boundary.
3. **The proxy itself**, non-streaming first, against a stub provider
   the way the Stripe client is tested. Then streaming.
4. **The client provider**, the settings entry, and the cap notice in
   the chat panel. Not a day later — the rule this project keeps
   relearning is that a backend without the click that reaches it
   reads as finished and is not.
5. **Rate limiting**, before the endpoint is reachable by anybody who
   did not build it.

## What is not decided

- **Whether the allowance is per person or per workspace.** Per person
  above, because it is simpler to explain and harder to game. Per
  workspace would let a team pool it, which is friendlier for the
  buyer and worse for the margin.
- **What happens to unused turns.** Nothing, above. Rollover is a
  feature with a long tail of support questions.
- **Whether Sonnet is a per-request choice or a plan.** Per request is
  nicer and makes the cap harder to explain, because turns then cost
  different amounts.
