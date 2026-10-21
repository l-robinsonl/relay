# Relay — Architecture

A local-first, account-free API client. The desktop app owns its data in a local
SQLite database and works fully offline; an optional self-hosted server
synchronises non-secret workspace content between devices and teammates.

## 1. Overview

```
┌─────────────────────────── Tauri 2 desktop app ───────────────────────────┐
│                                                                           │
│  Vue 3 + TS (webview)          │  Rust (native)                           │
│  ────────────────────          │  ─────────────                           │
│  features/   presentation      │  src-tauri      thin command layer       │
│  stores/     Pinia state       │    ↓                                     │
│  services/   typed IPC client  │  relay-core     domain logic:            │
│       │                        │                 interpolation, request   │
│       │   invoke() / events    │                 assembly, import/export  │
│       └───────────────────────▶│    ↓                                     │
│                                │  relay-http     reqwest engine, cancel,  │
│                                │                 redirects, TLS, cookies  │
│                                │  relay-storage  rusqlite (WAL), migrations│
│                                │  relay-sync     outbox + reconciliation  │
│                                │  relay-models   shared domain types      │
└───────────────────────────────────────────────────────────┬───────────────┘
                                                            │ HTTPS + WS
                                            ┌───────────────▼──────────────┐
                                            │  relay-server (Axum, SQLite) │
                                            │  auth, membership, revisions │
                                            └──────────────────────────────┘
```

### Layering rules

- `relay-models` has no I/O and no dependency on any other Relay crate. Both the
  desktop app and the server depend on it, so the sync wire format cannot drift.
- `relay-http` knows nothing about workspaces or variables. It receives a fully
  resolved `HttpRequestSpec` and returns bytes, timings and metadata.
- `relay-core` is where interpolation, request assembly and import/export live.
  It is pure enough to unit-test without Tauri, a database or a network.
- `src-tauri` contains no business logic — only command signatures, state wiring
  and error mapping. This keeps the testable surface outside the webview binary.

### Decisions and trade-offs

| Decision | Why |
|---|---|
| `rusqlite` (bundled) on the desktop, `sqlx` on the server | Desktop is single-user and low-concurrency; a bundled synchronous driver behind `spawn_blocking` is simpler and has no system SQLite dependency. The server benefits from `sqlx`'s async pool and compile-time-checked queries. |
| Headers, params, body and auth stored as JSON columns | They are always read and written together with their request, are never queried independently, and preserve ordering and `enabled` flags without join gymnastics. It also makes each request a single document, which makes revision-based conflict detection tractable. |
| No `vue-router` | The app is one window with an internal tab model. Routing would add a dependency (and, at v5, a `@pinia/colada` peer) for no navigation we actually have. Tab state lives in Pinia and is persisted to SQLite. |
| Whole-object revisions, not CRDTs | A request is edited by one person at a time in practice. Optimistic concurrency with an explicit conflict UI is honest about the rare case; a CRDT would add large complexity to hide it badly. |
| Response bodies spooled to disk above a threshold | Keeps a 500 MB download from becoming 500 MB of resident webview memory. |

## 2. Repository structure

```
relay/
  apps/desktop/           Vue 3 frontend + src-tauri shell
  crates/
    relay-models/         domain types, serde, no I/O
    relay-http/           HTTP execution engine (reqwest/tokio)
    relay-storage/        SQLite schema, migrations, repositories
    relay-core/           interpolation, request assembly, import/export
    relay-sync/           sync protocol + client reconciliation
  server/                 Axum self-hosted sync server
  packages/shared-types/  TS mirror of the IPC contract
  docker/                 Compose + Dockerfile for the server
  docs/                   this file and friends
  tests/                  cross-cutting integration fixtures
```

## 3. SQLite data model (desktop)

Every syncable row carries `rev`, `updated_at`, `deleted_at` and `dirty`.
`rev` is the last server-acknowledged revision (0 = never synced); `dirty = 1`
means there is a local change not yet pushed. Deletion is always a tombstone.

```sql
workspaces      (id, name, description, sort_order,
                 created_at, updated_at, rev, dirty, deleted_at)

collections     (id, workspace_id → workspaces, parent_id → collections,
                 name, description, sort_order,
                 created_at, updated_at, rev, dirty, deleted_at)
                 -- parent_id NULL = a root collection; otherwise a folder

requests        (id, workspace_id, collection_id → collections,
                 name, description, method, url,
                 headers_json, query_params_json, path_params_json,
                 body_json, auth_json, settings_json, scripts_json,
                 sort_order, created_at, updated_at, rev, dirty, deleted_at)

environments    (id, workspace_id, name, variables_json, sort_order,
                 created_at, updated_at, rev, dirty, deleted_at)
                 -- secret variables store {key, secret:true} with no value;
                 -- the value lives in the OS keyring under relay/<env_id>/<key>

history_entries (id, workspace_id, request_id?, name, method, url,
                 status, status_text, duration_ms, response_size,
                 executed_at, error, request_snapshot_json,
                 response_meta_json, response_body_path?)
                 -- local only, never synced

tabs            (id, workspace_id, request_id?, title, draft_json,
                 sort_order, is_active)

app_state       (key, value)    -- active workspace/env, layout, theme
sync_state      (workspace_id, server_url, last_pulled_seq, last_sync_at)
outbox          (seq, workspace_id, entity, entity_id, op, base_rev,
                 payload_json, created_at, attempts, last_error)
schema_migrations (version, applied_at)
```

Indices: `requests(workspace_id, collection_id, sort_order)`,
`collections(workspace_id, parent_id, sort_order)`,
`history_entries(workspace_id, executed_at DESC)`, plus partial indices
excluding tombstones on the hot read paths.

The database runs in WAL mode with `foreign_keys = ON`, `synchronous = NORMAL`
and a busy timeout, which is the right durability/throughput point for a
single-user desktop working set.

## 4. Rust ↔ Vue interface contract

All IPC is through narrowly scoped Tauri commands. Every command returns
`Result<T, RelayError>`, where `RelayError` is a tagged union
(`{ kind, message, detail? }`) so the frontend can branch on `kind` instead of
pattern-matching strings.

```ts
// workspaces & tree
workspace_list()                 → Workspace[]
workspace_create(input)          → Workspace
workspace_update(id, patch)      → Workspace
workspace_delete(id)             → void
collection_tree(workspaceId)     → TreeNode[]        // folders + requests
collection_create / _update / _delete / _move

// requests
request_get(id)                  → RequestDoc
request_save(doc)                → RequestDoc        // create or update
request_delete(id) / _duplicate(id) / _move(id, collectionId, sortOrder)

// environments
environment_list(workspaceId)    → Environment[]
environment_save(env)            → Environment
environment_delete(id)
secret_set(envId, key, value)    → void              // OS keyring, never synced
secret_delete(envId, key)

// execution
http_send(execId, spec)          → HttpExchange      // spec is pre-resolved
http_cancel(execId)              → void
    event  relay://exec-progress { execId, phase, bytesRead, totalBytes? }

// history, import/export, state
history_list(workspaceId, limit, offset) → HistoryEntry[]
history_body(entryId)            → BodyHandle
import_curl(text)                → RequestDoc
export_curl(spec)                → string
import_postman(json)             → ImportReport
export_workspace(id)             → string
app_state_get(key) / app_state_set(key, value)
```

`HttpRequestSpec` is already interpolated when it reaches `http_send` — the
frontend resolves variables through `relay-core` (exposed as
`resolve_variables`) so that the editor can show resolution errors live, and the
engine never has to know what a variable is.

The TypeScript side of this contract lives in `packages/shared-types` and is
generated from the Rust types, so a change to a struct breaks the build rather
than the app.

## 5. Synchronisation strategy

**Model.** Each workspace on the server has a monotonically increasing
`change_seq`. Every mutation bumps it and stamps the affected object with
`rev = change_seq`. A client tracks `last_pulled_seq` per workspace.

**Pull.** `GET /v1/workspaces/:id/changes?since=N` returns objects with
`rev > N` in revision order, tombstones included, plus the current head. The
client applies any object it has not locally modified. If the local row is
`dirty`, it is a conflict.

**Push.** Local mutations append to an `outbox` row carrying `base_rev` — the
revision the edit was made against. `POST /v1/workspaces/:id/changes` submits
them in order. The server accepts a change only when `base_rev` matches the
object's current `rev`; otherwise it responds `409` with the server's copy, and
the client records a conflict for the user to resolve (keep mine / keep theirs /
open a field-level diff). Nothing is ever discarded silently.

**Live.** A WebSocket at `/v1/workspaces/:id/ws` emits `{type:"head", seq}` on
every committed change. The client treats it purely as a hint to pull, so a
missed or duplicated notification cannot corrupt state. Reconnection is
exponential backoff with jitter, and a pull always runs on reconnect.

**Properties.** The protocol is versioned (`/v1`) and the exchange is
idempotent: re-running any step after an interrupted connection converges,
because pull is driven by `since` and push is keyed by a client-generated
mutation id the server deduplicates. Sync state is committed in one SQLite
transaction with the applied changes, so a crash cannot advance
`last_pulled_seq` past data that was not written.

**Secrets never sync.** Environment variables marked secret transmit their key
and the `secret` flag only. Cookies, tokens, client private keys and OAuth
credentials are excluded from the sync payload by construction, not by filter.

**Authorisation.** Every read and write is authorised against workspace
membership on the server. Knowing an object id grants nothing.

## 6. OAuth 2.0

Relay acts as an OAuth *client* against whatever API the user is testing.
This is unrelated to signing in to a Relay sync server.

| Grant | How |
|---|---|
| Authorization code | PKCE (S256) by default, through the system browser |
| Client credentials | Direct to the token endpoint |
| Password | Direct, for providers that offer nothing else |
| Implicit | Deliberately absent — see below |

The authorization code grant binds a listener on `127.0.0.1` *before*
opening the browser, so a provider that redirects instantly cannot arrive
before anything is listening. RFC 8252 §7.3 prefers loopback to a custom
URI scheme, which any other program on the machine could claim. The
listener accepts one callback and shuts down; it checks the `state` it
issued before it will exchange a code, ignores unrelated requests such as
`/favicon.ico`, escapes the provider's `error_description` into the page
it shows, and gives up after five minutes.

The browser is the user's real one, not an embedded webview: a webview
would not carry their existing session with the identity provider, would
not show the address bar they need in order to check what they are typing
a password into, and could in principle read it.

**Tokens are never persisted.** They live in a process-lifetime cache
keyed by a SHA-256 digest of the flow's identity — grant, endpoints,
client id and secret, scope, audience, username. The client secret is in
the digest so that rotating it does not keep serving the token issued
under the old one. A token is applied to the resolved request spec at
send time and never written back to the `RequestDoc`, which is what keeps
it out of SQLite, exports and the sync payload. The cost is that tokens
are lost on restart; client credentials re-fetch silently, and an
authorization code grant usually has a refresh token to trade in.

A send refreshes a token that is within sixty seconds of expiry rather
than letting it lapse mid-flight and return a confusing 401. If a refresh
token is rejected, grants that can start again without a browser do so;
the authorization code grant asks the user to authorize again.

The implicit grant is not implemented. It returns tokens in a URL
fragment, where they reach browser history and server logs, and it issues
no refresh token. A Postman collection that uses it imports with a
warning pointing at the authorization code grant.

A cached access token inside an imported Postman collection is dropped,
not adopted: collection files get emailed around, and a credential in one
should not silently become a credential Relay stores.

## 7. Phased plan

| Phase | Scope | Exit criteria |
|---|---|---|
| **1** | Monorepo, Tauri shell, SQLite persistence, Rust HTTP engine, sidebar + tabs + response panel | Create, save, send and inspect a request; reopen it after a restart |
| **2** | Collections/folders, environments, interpolation, all body modes, auth, history, search, shortcuts, import/export | A developer can drop Postman for everyday REST work, with no server |

### Import and export

Every import is a two-step: `import_preview` parses the file and returns an
`ImportReport` — what would be created, and an `ImportWarning` for each thing
that cannot be represented — and `import_apply` writes it in one transaction.
The preview exists because an import is hard to undo: discovering after the
fact that the auth on 200 requests did not survive is worse than being told
first, and a half-applied import is worse than either.

Postman collections are parsed against `serde_json::Value` rather than a
strict struct. Real exports disagree with the published schema and with each
other — `url` is sometimes an object and sometimes a string, numbers appear
where strings are specified, `description` is a string in one version and an
object in the next. One unrecognised field must not fail the other 199
requests.

Relay's own export (`relay.workspace`, format 1) is the portability
guarantee. Object ids are preserved, so re-importing keeps every reference
intact. Importing into the **same** workspace id is a restore and keeps those
ids; importing into a **different** one is a copy, which mints fresh ids and
rewrites the references between them — otherwise importing an export into a
second workspace would move the originals out of the first.

Secret values are stripped in `WorkspaceExport::build`, which is the single
place that decides what leaves the device. Postman writes secret values into
its export files in the clear; Relay reads the name, drops the value, and
tells the user to enter it again.
| **3** | Axum server, admin bootstrap, users, membership, revision sync, WebSockets, outbox, conflicts, Docker | Two users share a workspace, edit offline, reconnect and converge with no silent loss |
| **4** | OAuth 2.0, sandboxed scripting and assertions, OpenAPI import, advanced search, certificates, performance work | Done |
| **5** | Installers, signing guidance, CI, migration/recovery tests, security review, benchmarks | — |
| **6** | Relay Cloud: Neon Postgres behind the same `SyncStore` trait, shared multitenant schema, RLS, quotas | The hosted service speaks the identical `/v1` sync API; a workspace migrates between modes without export/import |
| **7** | Stripe Checkout, per-seat annual subscriptions, entitlements, webhooks, billing portal | Seats are enforced server-side; a lapsed subscription still permits full data export |

Billing is deliberately last. It is the only part of the system whose absence
costs a user nothing, and the only part whose premature arrival would shape the
sync protocol around a payment model rather than the other way round.


## 8. Deployment modes

The same server binary and the same `/v1` sync API serve all three modes. What
changes is the storage driver and whether billing is compiled in.

| Mode | Data | Account | Cost | Database |
|---|---|---|---|---|
| **Local-only** | On the user's disk | None | Free | Local SQLite (no server) |
| **Self-hosted** | On the team's server | Server-local, self-administered | Free | SQLite (WAL) |
| **Relay Cloud** | Managed, multitenant | Relay Cloud account | £1/seat/month, annual | Neon Postgres |

Three invariants hold across all of them:

1. **The desktop client never needs an account.** Sync is opt-in per workspace.
2. **No functional lock-in.** The paid tier buys convenience — somebody else
   running the server — not features. A self-hoster gets the same protocol,
   the same client and the same capabilities.
3. **Export always works.** Data export is not an entitlement. It remains
   available when a subscription lapses, is cancelled or is in arrears, because
   holding a user's own API collections hostage over £12 a year would be
   indefensible.

### Persistence abstraction

The server's data access sits behind one trait, implemented twice:

```rust
#[async_trait]
pub trait SyncStore: Send + Sync + 'static {
    async fn head(&self, ws: &WorkspaceId, actor: &Actor) -> Result<i64>;
    async fn changes_since(&self, ws: &WorkspaceId, actor: &Actor, since: i64, limit: u32)
        -> Result<ChangePage>;
    async fn apply(&self, ws: &WorkspaceId, actor: &Actor, batch: MutationBatch)
        -> Result<ApplyOutcome>;   // per-mutation accept / conflict
    async fn membership(&self, ws: &WorkspaceId, user: &UserId) -> Result<Option<Membership>>;
    // ... users, invitations, export
}
```

`SqliteStore` is the reference implementation and the one the protocol is
tested against; `PostgresStore` backs Relay Cloud. Authorisation is inside the
trait, not above it — every method takes an `Actor` and resolves membership as
part of the query, so no caller can forget to check. Knowing an object id is
never sufficient to read it.

### Multitenancy on Neon

One shared Neon project and one database, not a project per customer: a project
per tenant would multiply cost and operational surface for no isolation the
schema cannot provide.

- Every tenant-scoped table carries a non-null `tenant_id`, part of every
  primary key and every index prefix.
- Application queries are always filtered by `tenant_id` — this is the primary
  control.
- PostgreSQL row-level security is enabled on those tables as *defence in
  depth*, with the tenant set per transaction via `SET LOCAL app.tenant_id`.
  It exists to make a missing `WHERE` clause return nothing rather than return
  someone else's data. It is a second lock, not the only one.
- The desktop client never receives database credentials. It talks only to the
  Relay API over HTTPS.

### Limits

Bounded by configuration, with self-hosted defaults generous and cloud defaults
firm. These protect the service and are not a feature gate:

| Limit | Default |
|---|---|
| Mutation batch payload | 2 MiB |
| Single object document | 256 KiB |
| Sync pull page | 500 objects |
| Push frequency per client | 30/min sustained, burst 120 |
| Retained revisions per object | 50 |
| Hosted storage per workspace | 100 MiB |

### What the server stores — and does not

The server holds **request definitions and sync metadata**. It does not store
API traffic. Response bodies, request history and cookies stay on the device
that made the call, and are not part of the sync payload at all.

Secret environment variables transmit their key and `secret` flag with no
value, by construction in `Variable::redacted`. Client certificates, OAuth
tokens and proxy credentials are likewise local-only.

### Migrating between modes

Because all three modes share one object model, migration is a change of
`SyncConfig`, not an export/import cycle:

| From → to | What happens | Secrets |
|---|---|---|
| Local → self-hosted or cloud | Workspace objects are pushed as new revisions | Stay on the device. Teammates see the variable *names* and must supply their own values. |
| Self-hosted → cloud (or back) | Export from one, import to the other; ids are preserved so history and references survive | Never leave the device in either direction |
| Cloud → local | Disable sync. The local SQLite copy is already complete and authoritative | Unaffected — they were never uploaded |

The asymmetry is deliberate and worth stating plainly to users: **moving a
workspace never moves its secrets.** A teammate joining a shared workspace gets
the shape of the environment and must fill in their own credentials. This is
the behaviour that makes it safe to share a Production environment at all.

## 9. Billing architecture (Phase 7)

- **Stripe Checkout** for subscription creation, **Billing Portal** for
  self-service changes. Relay never handles card data and stores no PAN.
- **Per-seat, billed annually.** Annual billing is the default because at
  £1/seat/month the per-transaction fee on a monthly charge is a significant
  fraction of the charge itself.
- **Entitlements are server-side.** A `subscriptions` row per tenant records
  plan, seat count, status and `current_period_end`. Seat enforcement happens
  when a membership is added, never in the client.
- **Webhooks are verified and idempotent.** Every event is checked against
  `STRIPE_WEBHOOK_SECRET` and recorded by Stripe event id in a
  `processed_webhooks` table before it is acted on, so redelivery is a no-op.
  Stripe is the source of truth; the local row is a cache that reconciles.
- **Failure is graceful.** Payment failure moves the tenant to `past_due` with
  a grace period of 14 days during which sync continues read-write. After it,
  the workspace becomes **read-only for sync** — the desktop client keeps
  working entirely normally against its local database, and export stays
  available indefinitely.
- **Seat changes** prorate through Stripe. Removing a seat below current
  membership count is refused with a clear message rather than silently
  evicting somebody.

### E-mail (Resend)

Transactional e-mail only: address verification, workspace invitations,
password reset and dunning notices. No marketing, no tracking pixels, no
analytics. Resend is configured through `RESEND_API_KEY`; when it is absent —
the normal case for self-hosting — the server logs the link it would have sent
instead, so a self-hoster is never forced to configure an e-mail provider to
invite a colleague.
