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

Phases 1–3 complete. Phase 4 mostly. Phase 5 part-done.

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
| Packaging (server) | Dockerfile + compose, unprivileged, healthcheck; CI builds and smoke-tests the image |

| Not built | |
|---|---|
| Relay Cloud / billing | designed only (Phases 6–7) |
| Signing | nothing signed; macOS Gatekeeper will refuse outright |

## Verify

```sh
. "$HOME/.cargo/env"            # rustup installed with --no-modify-path
cargo test --workspace          # 540
npx vitest run --root apps/desktop   # 122
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
- **MSRV is 1.88, raised from 1.85 to take security fixes.** `time`
  0.3.47+ (RUSTSEC-2026-0009) and `plist` 1.10+ (which pulls `quick-xml`
  past two DoS advisories) both need it. Relay is an application, not a
  library anyone builds against, so the promise cost nothing to raise.
  Raising it also makes clippy enforce let-chains, which is why
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
- **Build the client path in the same breath as the server one.**
  Twice now a feature has been finished server-side, tested over HTTP,
  and shipped with no way for the application to reach it — e-mailed
  links pointing at routes that did not exist, then invitations the
  client could not present. Both read as complete from the test suite.
  If a flow sends a person somewhere, follow them there.
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

- Rust via rustup, **not on PATH** — `. "$HOME/.cargo/env"` first.
- `.env.local` (gitignored) holds `NEON_API_KEY`, `NEON_ORG_ID`,
  `NEON_PROJECT_ID`, `DATABASE_URL` (the user's *other* app — do not
  touch), `RELAY_DATABASE_URL` (Relay's own `relay` database) and
  `RESEND_API_KEY`.
- **Two Neon projects, and they are easy to confuse.** `NEON_PROJECT_ID`
  is project **postbox** (`REDACTED-neon-project`), endpoint
  `REDACTED-neon-endpoint`, host suffix **c-7** — that is Relay's. The
  other app lives on `REDACTED-other-endpoint`, suffix **c-8**, which
  is also where the Neon Auth JWKS endpoint is. `RELAY_DATABASE_URL`
  once pointed at the c-8 host with dbname `relay`, a database that
  exists only on c-7, so every Postgres test silently skipped.
- **`RELAY_TEST_DATABASE_URL` is the scratch database** (`relay_test`)
  for the conformance suite, which drops the schema every run. Never
  aim it at `relay`; the suite now refuses a database whose name has no
  `test` in it, after a run wiped the real one.
- The verified Resend sending domain is **REDACTED-mail-domain**; use
  something like `relay@REDACTED-mail-domain` for `RELAY_MAIL_FROM`.
  `delivered@resend.dev` is Resend's sink — send test mail there, never
  to a real person.
- Neon CLI authenticated as profile `agent`. `neon profile create agent --mint`
  is how it got there; project settings → Credentials issues S3 keys, not
  management keys.
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

1. Phase 6/7 — Relay Cloud on Neon, then Stripe. Billing last, by the
   user's own sequencing.

**CI runs** at `github.com/l-robinsonl/relay`, on push to `master` or
`main` and on pull requests. Its first run proved the workspace builds
and tests on Linux, macOS *and* Windows, which nothing had checked
before. Two things it caught immediately: `rustsec/audit-check` cannot
create its check run under the default read-only `GITHUB_TOKEN`, and
three advisories in transitive dependencies. Do not lower the audit job
to a warning — it earned its place on its first run.

Phase 4 is finished. What is left in Phase 5 is CI and signing, both of
which need a git remote and a certificate respectively.

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
