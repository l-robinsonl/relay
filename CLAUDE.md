# Relay — working notes

Read this first. It is the handover between sessions: current state, the
decisions that are not obvious from the code, and the traps already paid
for. `docs/` holds the detail; this holds the orientation.

## What it is

A local-first desktop API client — a Postman replacement that needs no
account. Tauri 2 + Vue 3 + Rust, SQLite locally, with an optional
self-hostable sync server and a planned managed tier.

**The pitch is not speed, and not memory.** It is: no account, your data
on your disk, sync only if you ask for it, to a server you choose.
Postman will not let you send a GET request until you have signed in.
Being small is a *consequence* of not shipping a browser, not the reason
to switch. The user corrected me on this once; do not drift back.

## Status

Phases 1–6 complete, and Phase 7 built but never run against a real
Stripe account. Nothing is signed and nothing is deployed.

| Works | |
|---|---|
| HTTP engine | all methods, every body mode, auth, redirects, cookies, proxies, client certs, cancellation, bounded/spooled responses |
| Local storage | SQLite WAL, migrations, tombstones, revisions |
| Variables | `{{name}}` across workspace/collection/environment/request, documented precedence, secrets in the OS credential store |
| Import | OpenAPI 3 (JSON+YAML), Postman v2.1 + environments, cURL, Relay exports |
| Export | portable JSON, copy as cURL |
| Sync | Axum server, users, membership, revisions, conflicts, WebSocket live updates, client engine |
| Scripting | QuickJS sandbox, assertions, test results panel, pre-request scripts that may rewrite the request |
| OAuth 2.0 | authorization code + PKCE via loopback, client credentials, password; in-memory token cache, silent refresh |
| TLS | per-request verification toggle, client certificates with host matching, proxies — all reachable from Settings |
| Packaging | `.deb` / `.rpm` / `.AppImage` build |
| CI | GitHub Actions: Rust + desktop build on Linux/macOS/Windows, frontend, bindings drift, dependency audit |
| Accounts | register from the app, e-mail confirmation, password reset, single-use hashed tokens, Resend or log-only |
| Invitations | workspace + role, bound to the invited address, admin-only, revocable; `invite` signup mode now works |
| Members UI | invite, change a role, remove, withdraw an invitation — all from the Sync dialog |
| Joining | an invitee pastes the link, picks a password, and lands in the workspace with its contents |
| Assistant | `relay-agent` loop + nine tools + persistence + docked chat panel (Ctrl+J) + settings + MCP servers configurable from Settings. MCP over **stdio**; HTTP transport not wired. Verified against a real model |
| Packaging (server) | Dockerfile + compose, unprivileged, healthcheck; CI builds and smoke-tests the image |
| Billing | Stripe: entitlement model, signed webhooks, Checkout + portal, a seats panel in the Sync dialog. Off entirely unless all three Stripe variables are set |

| Not built | |
|---|---|
| Relay Cloud | nothing deployed: no host, no DNS |
| Signing | nothing signed; macOS Gatekeeper will refuse outright |

## Verify

```sh
. "$HOME/.cargo/env"            # rustup installed with --no-modify-path
cargo test --workspace          # 739
npm test                        # 161 frontend
cargo clippy --workspace --all-targets -- -D warnings
npm run build
npm run bindings                # regenerate TS from Rust; must be a no-op
```

Against real Postgres (needs `RELAY_DATABASE_URL` from `.env.local`):

```sh
RELAY_TEST_POSTGRES_URL="$RELAY_DATABASE_URL" \
  cargo test -p relay-server --test conformance -- --test-threads=1
```

Run it: `npm run tauri dev`, or the built binary at
`target/release/relay-desktop`.

## Map

| Crate | Holds | Notes |
|---|---|---|
| `relay-models` | domain types, TS bindings via ts-rs | no I/O, no Relay deps |
| `relay-http` | the reqwest engine | knows nothing of workspaces |
| `relay-storage` | SQLite, migrations, repos | tombstones, never hard deletes |
| `relay-core` | interpolation, resolution, import/export | pure; no DB, no network |
| `relay-script` | QuickJS sandbox | escape tests > feature tests |
| `relay-sync` | sync protocol types | shared with the server |
| `relay-app` | application services | **all logic lives here** |
| `server` | Axum sync server | SyncStore trait, SQLite + Postgres |
| `apps/desktop/src-tauri` | 40-odd command wrappers | no logic, by design |

`src-tauri` is deliberately logic-free so everything is testable with
`cargo test` rather than by driving a window.

## Decisions worth not relitigating

- **Authorisation lives inside `SyncStore`**, not above it. Every method
  takes an `Actor` and resolves membership in its own query, so no
  handler can forget. An inaccessible workspace returns 404, never 403 —
  403 confirms the id exists and lets someone enumerate.
- **Conflict *decisions* are shared pure functions** (`server/src/store/logic.rs`)
  while the SQL differs per backend. If the two disagreed, the same edit
  would be accepted self-hosted and rejected on Cloud.
- **Postgres lives in a `relay` schema**, never `public`. The Neon
  database is shared with other things and `users`/`objects` are names
  anything might want.
- **A pull never overwrites a dirty row.** Doing so resurrected deleted
  requests. If the server is ahead it is a conflict; otherwise skip.
- **Secrets are redacted in one place per path** — `Variable::redacted`,
  `HttpRequestSpec::redacted`, `WorkspaceExport::build` — so a new call
  site cannot forget. Scripts cannot read or overwrite them.
- **JSON columns** for headers/body/auth: always read and written with
  their parent, never queried independently, and it makes each object one
  document for revision-based conflict detection.
- **No vue-router.** One window, tabs in Pinia.
- **Neon Auth is for Relay Cloud sign-in, never for the API client's
  OAuth.** Two different things share the name. The Phase 4 feature is
  Relay acting as an OAuth *client* against whatever API the user is
  testing — GitHub, Stripe, their own Okta — where Neon has no role.
  Neon Auth (already provisioned: users, sessions, social accounts,
  orgs, invitations, verification, JWKS) is a candidate for signing in
  to Relay's *own* sync server, in Phase 6, behind an `Authenticator`
  trait mirroring `SyncStore` — `LocalAuthenticator` for self-hosting so
  it stays dependency-free, `NeonAuthenticator` for Cloud. Map Neon
  identity to a Relay user and keep workspace membership ours;
  `neon_auth.organization` is a parallel concept, not our workspaces.
- **MSRV is 1.94, and that is the true floor.** It went 1.85 → 1.88 to
  take `time` 0.3.47+ (RUSTSEC-2026-0009) and `plist` 1.10+ (which
  pulls `quick-xml` past two DoS advisories), then → 1.94 because sqlx
  0.9 refuses anything below it. That last one had been false for
  months: every machine building this ran something newer, and it took
  pinning an older toolchain in the container to notice. Relay is an
  application, not a library anyone builds against, so raising it costs
  nothing. It also makes clippy enforce let-chains, which is why
  `interpolate.rs` uses one.
- **A configured-but-broken Postgres must fail, not skip.**
  `postgres_store()` in the conformance suite used to `.ok()?` on a
  connection error, so a wrong `RELAY_TEST_POSTGRES_URL` meant every
  test quietly ran SQLite-only and still reported success. It now
  panics. Absent configuration still skips; that part is fine.
- **An e-mailed token is scoped to its purpose.** A confirmation token
  must never redeem as a password reset — otherwise reading a
  verification mail is enough to take the account. The purpose is part
  of the `DELETE … RETURNING` predicate, not a label beside it.
- **`ServerError::Internal` hides its detail from the log too.** Its
  `Display` is the literal string "internal error", so
  `tracing::error!(%error)` records nothing useful. `ServerError::Mail`
  exists because of this: a provider's refusal ("domain not verified")
  is the operator's only clue and contains no schema or query detail.
  Think twice before routing a new failure through `Internal`.
- **`npm run bindings` must name every crate that exports types.** It
  listed relay-models and relay-core only, so relay-sync's generated
  TypeScript was refreshed solely as a side effect of a full
  `cargo test --workspace`, and CI's drift check could not have caught
  a stale one. relay-sync and relay-script are in the list now; add any
  new crate that derives `TS`.
- **The agent loop is Rust, deliberately.** The obvious route was an
  agent framework in TypeScript beside the interface. That puts the
  user's API key in the webview, where Relay has never put a
  credential; it sends every tool call across IPC twice; and it cannot
  spawn a local MCP server. The loop is in `relay-agent`, the webview
  sees a transcript. No LangChain — a tool-calling loop is a hundred
  lines, and the framework's value is provider abstraction Relay
  already has.
- **Conversations go in SQLite, not a hosted store.** DynamoDB was
  suggested and declined: a product arguing your data is on your disk
  cannot answer "where do my conversations live" with someone else's
  cloud.
- **The assistant never sees a secret variable's value.** Same rule as
  scripts, and more important: an agent can be steered by whatever a
  web page said, so it is a weaker vessel than a script the user
  wrote. `list_environments` reports names and the secret flag, never
  values; `get_request` redacts credential-shaped headers. Both have
  tests in `crates/relay-app/tests/assistant_tools.rs`.
- **An MCP tool is always `ToolRisk::Send`.** Relay cannot know what
  someone else's tool does. The protocol has hints (`readOnlyHint` and
  friends) but they are written by the same server being asked to
  behave, so letting them decide whether to ask the person is trusting
  the wrong party.
- **MCP over HTTP is not wired, deliberately.** `rmcp`'s reqwest-backed
  streamable-HTTP transport would not resolve `StreamableHttpClient`
  against this workspace's feature set, and the feature graph looked
  correct (`__reqwest`, `reqwest`, `client-side-sse` all enabled, same
  reqwest 0.13.5 in both). Rather than ship a transport that
  half-works, `connect` returns a message naming the limit. stdio —
  which is how most local MCP servers run — works and is tested
  against a real child process. Worth another look with a later rmcp.
- **Approval is a parked channel, and silence means no.** The loop is
  in Rust, the question is in a window. `AskTheUser` parks on a
  oneshot, the interface is told, and `assistant_approve` resolves it.
  A dropped sender — window closed, turn abandoned — resolves to
  `Deny`, never a hang: a tool that sends a request must not run
  because nobody answered.
- **The approve buttons are not disabled while the turn is busy.** The
  turn is *always* in flight when they are on screen — it is in flight
  because it is waiting for that answer — so a `:disabled="busy"` made
  them dead exactly when needed. A test caught it; keep the test.
- **`ToolRisk` decides what gets confirmed.** `Send` and `Destructive`
  always ask. `Send` is its own level because a request Relay makes is
  indistinguishable at the far end from one the user made, and "list
  the invoices" and "delete the invoices" differ by one word in a
  prompt. Classify a new tool deliberately.
- **Build the client path in the same breath as the backend one.**
  This has now happened three times: e-mailed links pointing at routes
  that did not exist, invitations the client could not present, and an
  MCP client with no way to configure a server. Each read as complete
  from the test suite, because the tests reached past the part a
  person uses. Writing the rule down did not stop the third. Before
  calling anything done, name the click that reaches it.
- **An invitation is bound to the address it was sent to.** Not just
  to its token. E-mails get forwarded, and a token-only check would
  turn a forwarded invitation into access to someone else's workspace.
  Both `accept_invitation` and registration enforce it.
- **An e-mailed link has to lead somewhere.** The first cut of
  verification shipped messages pointing at `/auth/verify`, a route that
  did not exist, because every test posted to the JSON API instead of
  following the URL the recipient is handed. `server/src/pages.rs`
  serves the two pages; `http_api.rs` has tests that follow the link out
  of the message body. Add one whenever a flow sends a URL.
- **Seats bind on growth, by both routes in.** `create_invitation`
  and `upsert_membership` each check the entitlement, inside
  `SyncStore` for the same reason authorisation is. Gating only the
  invitation left an administrator able to add members by user id and
  never pay. Only *additions* are checked: an existing member stays
  demotable and removable, or an over-limit workspace's only way out
  is to pay. Nothing else is ever blocked — not syncing, not reading,
  not `past_due`. A tool that holds your API requests must not hold
  them hostage over a failed card.
- **The Stripe return URLs are followed by a test.** The checkout
  names `{public_url}/billing/done` as its `success_url`, and that
  route did not exist, so anyone who paid landed on a 404. Third
  instance of the same class of bug. The test pulls the URLs back out
  of the form that was sent to Stripe and GETs them, rather than
  asserting a string the same code produced; deleting the route fails
  it.
- **`ServerError::Stripe` exists for the same reason as `Mail`.**
  Routing a provider refusal through `Internal` logs the literal
  string "internal error", so an operator with a wrong price id or a
  test key against a live price has nothing at all to go on. The
  variant keeps its detail in `Display` and still flattens to
  `Internal` on the wire. Found by running the real binary against
  the real API; no test caught it, because every test asserts on the
  response and the response was correct.
- **A seat limit answers 402, not 413.** `LimitExceeded` carried
  `PAYLOAD_TOO_LARGE`, left over from when it meant a body-size cap —
  which tower-http now answers by itself, so seats are the only thing
  that variant reports. In a real run "you need another seat" came
  back looking like a malformed request. Payment Required is the
  status this situation exists for.
- **`Config` has a `Default` for fixtures only.** Four test fixtures
  in two crates spelled out every field, so each new setting broke
  them all. `from_env` stays an exhaustive struct literal — a new
  setting must still be read from the environment deliberately.
- **rusqlite pinned to 0.39.** 0.40 needs libsqlite3-sys 0.38; sqlx-sqlite
  needs <0.38; both link native sqlite3. Do not bump without checking.

## Traps already paid for

- **`sr-only` as a grid child breaks the layout.** Tailwind's `sr-only`
  is `position:absolute`, so it leaves the grid flow and every visible
  column shifts left. Wrap it: `<span><span class="sr-only">…</span></span>`.
- **Wayland cannot set a window icon from the app.** The compositor
  matches `app_id` to an installed `.desktop` file. A binary run from
  `target/` gets a generic icon; that is not a bug.
  `./scripts/install-desktop-entry.sh` fixes it for development.
- **Electron apps will not launch from a terminal inside VS Code.** The
  editor exports `ELECTRON_RUN_AS_NODE=1`; the binary obeys it, starts as
  Node and dies on `--no-sandbox`. Unset `ELECTRON_*` and `VSCODE_*`.
- **`pkill -f 'some-pattern'` matches this shell's own command line** and
  kills the session (exit 144). Use `pkill -x name`, or a bracket class:
  `pgrep -f 'nam[e]'`.
- **A string replacement that matches nothing fails silently.** One did,
  after `cargo fmt` reformatted the target, and `import_detect` kept its
  old body. Always `assert old in t` before replacing.
- **`serde` cannot serialise a newtype variant in an internally tagged
  enum.** It compiles and fails at runtime. Use a struct variant.
- **`derive(Default)` ignores serde's `default = "..."`.** It silently
  defaulted OAuth PKCE to off. Write `Default` by hand where it matters.
- **An async JS test body reports a pass having asserted nothing** unless
  bodies are queued and awaited. Likewise `import('fs')` returns a
  rejected promise rather than throwing, invisible unless jobs are pumped.
- **`i64`/`u64` map to `bigint` in ts-rs** but arrive as `number` over
  JSON. Annotate `#[ts(type = "number")]`.

## Environment

**This repository is public.** Hostnames, project ids, account names
and the sending domain live in `CLAUDE.local.md`, which is
gitignored. Read it first — it is half of this section. Keep new
specifics there and the lesson here, and never commit a value that
names where this infrastructure actually is.

- Rust via rustup, **not on PATH** — `. "$HOME/.cargo/env"` first.
- `.env.local` (gitignored) holds the Neon and Resend credentials,
  plus `RELAY_DATABASE_URL` and `RELAY_TEST_DATABASE_URL`. One of the
  variables in there belongs to a **different application** — check
  `CLAUDE.local.md` before touching any of them.
- **There are two Neon projects and they are easy to confuse.** One
  is Relay's, one is not, and the Neon Auth JWKS endpoint sits on the
  wrong one. `RELAY_DATABASE_URL` once pointed at the other project's
  host with Relay's database name — a database that exists only on
  the first — so every Postgres test silently skipped and still
  reported success. `CLAUDE.local.md` has the table.
- **`RELAY_TEST_DATABASE_URL` points at the DIRECT host, not the
  pooler.** The suite drops and recreates the schema, and a
  transaction-mode pooler keeps server-side prepared statements across
  runs: after a column changes type, every query fails with "cached
  plan must not change result type" and stays failing. Disabling
  sqlx's own cache (which `PostgresStore::connect` now does) stops new
  stale plans but cannot clear ones the pooler already holds. The
  endpoint without `-pooler` in the hostname is the one to use.
- **`RELAY_TEST_DATABASE_URL` is the scratch database** (`relay_test`)
  for the conformance suite, which drops the schema every run. Never
  aim it at `relay`; the suite now refuses a database whose name has no
  `test` in it, after a run wiped the real one.
- **`RELAY_MAIL_FROM` must be on a domain verified with Resend**, or
  every send is refused. The verified one is in `CLAUDE.local.md`.
  `delivered@resend.dev` is Resend's own sink — send test mail there,
  never to a real person.
- The Neon CLI is authenticated; `CLAUDE.local.md` says as what. Note
  that project settings → Credentials issues S3 keys, not management
  keys.
- Quote URLs with query strings in env files, or `. .env` dies on the `&`.
- Postman 11 installed at `~/.local/share/Postman` for comparison.

## Measured, on the user's machine

Both idle. `./scripts/measure-memory.sh`.

| | RSS | PSS | Download | Installed |
|---|---:|---:|---:|---:|
| Relay | 415 MB | 221 MB | 7.3 MB | ~20 MB |
| Postman 11 | 1,584 MB | 1,067 MB | 199 MB | 538 MB |

Quote PSS. Never quote a figure this project did not measure.

## Working with Lee

- He wants the thing built end to end and will test-drive it. "You build
  the car, I'll test drive." Do not stop to ask permission for ordinary
  work.
- **He finds real bugs from screenshots that no test catches** — the
  native `window.prompt` dialog, the overlapping grid header, the sidebar
  spilling into the editor. Take them seriously and fix the root cause,
  not the symptom.
- Be straight about what is not built and what is unverified. He is not
  looking for reassurance.
- Say plainly when something is still unseen. Much of this UI has never
  been looked at by me; only he can report on it.

## Next

1. A look at the UI built blind. The members panel, scripts editor,
   invited-join tab, chat panel, assistant settings, MCP settings and
   now the seats panel have all been written without ever being seen.
2. Deploy. The only things between here and a usable Relay Cloud are
   a host and a DNS name; `render.yaml` and `deploy/compose.prod.yml`
   are both ready and neither has been run in anger.
3. Signing, which needs certificates (~£99/yr Apple, ~£120/yr
   Windows).

**The assistant has a live test.** `crates/relay-app/tests/assistant_live.rs`
runs the real loop against a real model, skipping unless
`ANTHROPIC_API_KEY` or `OPENAI_API_KEY` is set (it prefers Claude). One
of its two tests asks the model directly for a secret variable's value
and a pasted `Authorization` header, then asserts that neither canary
string appears anywhere — including in the raw tool results, because a
leak into a tool result is a leak even if the model then declines to
repeat it. Lee's OpenAI key is in `.env.local`; there is no Anthropic
key yet.

**CI runs** at `github.com/l-robinsonl/relay`, on push to `master` or
`main` and on pull requests. Its first run proved the workspace builds
and tests on Linux, macOS *and* Windows, which nothing had checked
before. Two things it caught immediately: `rustsec/audit-check` cannot
create its check run under the default read-only `GITHUB_TOKEN`, and
three advisories in transitive dependencies. Do not lower the audit job
to a warning — it earned its place on its first run.

Signing is the one thing left in Phase 5, and it needs certificates
rather than code.

**The webhook half is proven against real Stripe signatures.** Via
the Stripe CLI against sandbox `acct_1IuHoz…`, with
`stripe listen --events … --forward-to localhost:8791/v1/billing/webhook`:
a real `checkout.session.completed` carrying
`client_reference_id=w1` verified and activated the workspace;
`stripe events resend` of the *same event id* logged "already
applied" and changed nothing; a real `customer.subscription.updated`
carrying `items[0][quantity]=5` moved the seat count, which is worth
more than the stub tests because the payload shape was Stripe's and
not mine; `customer.subscription.deleted` cancelled it, after which
an invitation was refused and `head`/`export` still answered 200. A
forged signature at the same running endpoint changed nothing.

**The checkout half is still unproven.** It needs an `sk_test_` key
in `STRIPE_SECRET_KEY`; the CLI's device auth keeps its token in the
keyring and exposes no key to lift. Everything else is ready: test
product `prod_VUpc1mJgVTjzTI`, price
`price_1UTpxiHqleqjJY5Ok6CCOC79` (per-unit, annual, £24, sandbox).

**Pre-request scripts, as built.** `ScriptEngine::run_pre_request` returns
a `RequestPatch` (in `relay-models`, applied by
`HttpRequestSpec::apply_patch`) rather than handing the script a mutable
request. The patch is applied *after* resolution and script-set variables
are bound with `set_literal`, which does not expand — together those two
stop a script naming `{{apiSecret}}` and having Relay fetch a secret it
is not allowed to read. Both holes have tests in
`crates/relay-app/tests/pre_request_scripts.rs`; do not "simplify" either
one away.

**OAuth, as built.** `relay-http/src/oauth.rs` does the protocol,
`oauth_redirect.rs` is the one-shot loopback listener, and
`relay-app/src/oauth.rs` holds `TokenCache` — in memory, keyed by a
SHA-256 digest of the flow's identity *including the client secret*, so
rotating a secret does not keep serving the old token. Tokens are put on
the resolved spec inside `request_send` and never on the `RequestDoc`,
which is what keeps them out of SQLite, exports and sync. The browser is
opened by a closure the desktop layer passes in, so the flow is testable
without a window.
