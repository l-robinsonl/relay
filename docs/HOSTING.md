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

## Self-hosting

SQLite needs nothing but a writable directory:

```sh
RELAY_DATABASE_URL=sqlite://data/relay.db relay-server
```

No Postgres, no external identity provider, no licence check, no Relay
Cloud account. That is the point: hosting it yourself must cost nothing and
depend on nobody.
