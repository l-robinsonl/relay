# Hosting Relay's sync server

One binary, two database backends, three deployment modes. What changes is
the storage driver and who runs it; the `/v1` sync API is identical.

## Backends

`SyncStore` is implemented twice:

| Backend | Used by | Driver |
|---|---|---|
| `SqliteStore` | Self-hosting | `sqlx` + SQLite (WAL) |
| `PostgresStore` | Relay Cloud | `sqlx` + PostgreSQL (Neon) |

Both are held to the same behaviour by one conformance suite
(`server/tests/conformance.rs`). It is written against the trait and runs
against SQLite always, and against PostgreSQL whenever
`RELAY_TEST_POSTGRES_URL` is set:

```sh
cargo test -p relay-server                                   # SQLite only
RELAY_TEST_POSTGRES_URL="$DATABASE_URL" \
  cargo test -p relay-server --test conformance -- --test-threads=1
```

Two implementations only stay equivalent if something checks. Conflict
*decisions* are made by shared pure functions in `server/src/store/logic.rs`
precisely so the SQL can differ while the semantics cannot: if the two
disagreed about what counts as a conflict, the same edit would be accepted
self-hosted and rejected on Cloud.

## PostgreSQL lives in its own schema

Relay's tables are created in a `relay` schema, never in `public`.

A managed Postgres database is routinely shared — a starter template, an
auth provider's tables, another app's migrations. `users`, `sessions` and
`objects` are names anything might want. Owning a schema means Relay cannot
collide with what is already there, and the entire deployment can be
inspected or removed with one statement:

```sql
DROP SCHEMA relay CASCADE;
```

Before pointing Relay at a database, check what is in it:

```sh
cargo run -p relay-server --example dbinfo -- "$DATABASE_URL"
```

That is read-only. It reports the server version, the current user and
every table, so an existing application's schema is never migrated over by
accident.

## Row-level security

The Postgres migration enables RLS on `relay.workspaces`,
`relay.memberships`, `relay.objects` and `relay.invitations`, with policies
that read `app.user_id` from the current transaction.

**This is the second lock, not the only one.** Every query the application
issues is already filtered by workspace, and authorisation is resolved
inside `SyncStore` — each method takes an `Actor` and checks membership as
part of its own query, so no handler can forget. RLS exists so that a query
which one day loses its `WHERE` clause returns nothing rather than another
tenant's rows.

For RLS to do anything, the application must **not** connect as a superuser
or as the schema owner: both bypass policies. In production, create a
dedicated role:

```sql
CREATE ROLE relay_app LOGIN PASSWORD '…';
GRANT USAGE ON SCHEMA relay TO relay_app;
GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA relay TO relay_app;
ALTER DEFAULT PRIVILEGES IN SCHEMA relay
  GRANT SELECT, INSERT, UPDATE, DELETE ON TABLES TO relay_app;
```

and point `RELAY_DATABASE_URL` at that role. Connecting as the owner (for
example Neon's default `neondb_owner`) works, but RLS will not be enforced
and you are relying on the application's own filtering alone.

### Setting up a Neon database

The CLI can mint its own API key, which avoids hunting for the setting in
the console. Note that **Settings → Credentials** on a *project* issues S3
keys for object storage and AI Gateway tokens — not a management key.

```sh
npm i -g neon@latest
neon profile create agent --mint      # one browser sign-in, stores an API key
```

After that the CLI works unattended, either with `--profile agent` or with
`NEON_API_KEY` in the environment.

Give Relay a database of its own rather than sharing one:

```sh
export NEON_API_KEY=napi_…
neon databases create --name relay --owner-name <owner> --project-id <id>
```

Then point `RELAY_DATABASE_URL` at it — the same connection string with
`/relay` as the path. A Neon project often arrives with a starter template
already in `neondb`, and sharing a database with it is avoidable work.

Relay still creates its `relay` schema inside whatever database it is
given. That is deliberate: the same migration then runs unchanged whether
the database is Relay's alone or shared.

## Neon specifics

- **Use the pooled endpoint** (`-pooler` in the hostname) for the
  application. The server keeps its own small pool on top; Neon's pooler
  handles the rest.
- **Idle compute suspends.** The pool recycles connections well inside that
  window (`idle_timeout` 60s, `max_lifetime` 30min) so a suspended compute
  produces a reconnect rather than a user-visible error.
- **Expect latency.** The conformance suite takes ~40s against a Neon
  instance in another region versus ~0.03s on local SQLite. That is network
  round trips, not slow queries — but it is a reminder that the sync
  protocol should stay chatty-by-batch, not chatty-by-object.
- **One project, one shared database.** Relay Cloud is multitenant by
  `workspace_id`, not by Neon project. A project per customer would multiply
  cost and operational surface for isolation the schema already provides.

## The API

All of it is under `/v1`, with `/health` outside for load balancers.

| Method | Path | Who |
|---|---|---|
| `GET` | `/health` | anyone |
| `POST` | `/v1/auth/register` | anyone, subject to `RELAY_SIGNUP_MODE` |
| `POST` | `/v1/auth/verify/request` | anyone; always 204 |
| `POST` | `/v1/auth/verify/confirm` | anyone with the token |
| `POST` | `/v1/auth/password/reset` | anyone; always 204 |
| `POST` | `/v1/auth/password/confirm` | anyone with the token |
| `POST` | `/v1/auth/login` | anyone |
| `GET` | `/v1/auth/me` | any signed-in user |
| `POST` | `/v1/auth/logout` | any signed-in user |
| `GET`/`POST` | `/v1/workspaces` | any signed-in user |
| `GET` | `/v1/workspaces/{id}/head` | viewer |
| `GET` | `/v1/workspaces/{id}/changes?since=N&limit=M` | viewer |
| `POST` | `/v1/workspaces/{id}/changes` | editor |
| `GET` | `/v1/workspaces/{id}/export` | viewer |
| `GET` | `/v1/workspaces/{id}/members` | viewer |
| `PUT`/`DELETE` | `/v1/workspaces/{id}/members/{userId}` | admin |
| `GET` | `/v1/workspaces/{id}/ws` | viewer |

Authentication is a bearer token from `/v1/auth/login`.

**A workspace you cannot reach returns `404`, not `403`**, whether it does
not exist or merely is not yours. A `403` would confirm the id is real,
which is enough to enumerate workspaces.

### Sessions

Tokens are 256 bits of system randomness, opaque, and stored only as a
SHA-256 hash — a database disclosure does not hand over live sessions.
They are not JWTs: a self-hosted server has no need to validate a token
without touching its database, and the thing you actually want from a
session is for `logout` to work immediately.

Passwords are Argon2id at the `argon2` crate's recommended parameters. A
login against an unregistered address performs a dummy verification, so
response timing does not reveal which addresses exist.

### Live notifications

`GET /v1/workspaces/{id}/ws` upgrades to a WebSocket that emits
`{"type":"head","seq":N}` — the current head on connect, then one message
per change.

It carries **no workspace content**, deliberately. Clients treat it purely
as a hint to pull, so a missed, duplicated or out-of-order notification
cannot corrupt anything; correctness lives entirely in pull/push. A client
that falls behind is not disconnected, because the next message corrects
it. Membership is resolved *before* the upgrade, so a non-member never
gets a socket at all.

### The client side

The desktop app holds the socket open while a synced workspace is open,
reconnecting with exponential backoff and jitter — jitter because
otherwise every client of a restarted server reconnects in the same
instant, which is how a recovering server gets knocked over twice.

A notification only schedules a sync, debounced by 400 ms so a burst of
saves from several teammates costs one pass. A background sync is silent
unless it finds a conflict: announcing every colleague's save would make
the app unusable in a team.

If the socket never connects, nothing breaks. Sync still works; it just
waits for the next manual pass.

## Administrator bootstrap

On an empty server, `RELAY_ADMIN_EMAIL` and `RELAY_ADMIN_PASSWORD` create
the first account:

```sh
RELAY_ADMIN_EMAIL=you@example.com \
RELAY_ADMIN_PASSWORD='something long' \
  relay-server
```

It runs **only when no users exist**, so restarting with the variables
still set will not reset a password or resurrect a deleted account. The
server logs a warning telling you to remove them once the account exists.

## Self-hosting

SQLite needs nothing but a writable directory:

```sh
RELAY_DATABASE_URL=sqlite://data/relay.db relay-server
```

No Postgres, no external identity provider, no licence check, no Relay
Cloud account. That is the point: hosting it yourself must cost nothing and
depend on nobody.

## E-mail

Relay sends two kinds of message: confirm your address, and reset your
password. Both are plain text and both contain a single-use link.

**Without a mail provider the server still works.** The default mailer
writes the message to the log instead of sending it, so a small
self-hosted deployment needs no e-mail account at all — an operator who
needs a link fetches it from the log. Set `RELAY_REQUIRE_VERIFIED_EMAIL`
only once mail is genuinely being delivered, or the first person to
register cannot get in.

To send for real, set `RESEND_API_KEY` (or `RELAY_RESEND_API_KEY`) and
`RELAY_MAIL_FROM`. The from-address must be on a domain verified with
Resend; anything else is refused on every send, and the refusal is
logged with the provider's own explanation.

### What the tokens guarantee

- **Hashed at rest.** Only the SHA-256 of a token is stored, exactly as
  for session tokens. A leaked database dump contains nothing that can
  be presented to the server.
- **Single use.** Redeeming is one `DELETE … RETURNING`, so a link
  presented twice concurrently is honoured once.
- **Scoped to a purpose.** A confirmation token cannot be redeemed to
  set a password. Otherwise anyone who could read a verification e-mail
  — a shared mailbox, a forwarded message — could take the account.
- **Short-lived.** Confirmation lasts 24 hours, a reset one hour.
- **A reset signs every device out.** People reset a password precisely
  when they suspect somebody else holds a session.
- **No account enumeration.** Asking to verify or reset an unknown
  address returns exactly what a known one returns, and sends nothing.
  A repeat request within a minute is silently suppressed rather than
  rate-limit-errored, because an error would itself confirm the address.
