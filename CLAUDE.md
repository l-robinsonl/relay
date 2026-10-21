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
| Scripting | QuickJS sandbox, assertions, test results panel |
| OAuth 2.0 | authorization code + PKCE via loopback, client credentials, password; in-memory token cache, silent refresh |
| Packaging | `.deb` / `.rpm` / `.AppImage` build |

| Not built | |
|---|---|
| Pre-request scripts | stored and imported; never executed |
| Client certificate UI | engine supports it, no settings screen |
| Relay Cloud / billing | designed only (Phases 6–7) |
| CI | written, never executed — no git remote |
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
  touch), `RELAY_DATABASE_URL` (Relay's own `relay` database), and
  `RESEND_API_KEY` (unused so far).
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

1. Pre-request scripts — the runtime exists, nothing calls it before send.
2. Client certificate settings UI.
3. CI — written at `.github/workflows/ci.yml`, never run.
4. Phase 6/7 — Relay Cloud on Neon, then Stripe. Billing last, by the
   user's own sequencing.

**OAuth, as built.** `relay-http/src/oauth.rs` does the protocol,
`oauth_redirect.rs` is the one-shot loopback listener, and
`relay-app/src/oauth.rs` holds `TokenCache` — in memory, keyed by a
SHA-256 digest of the flow's identity *including the client secret*, so
rotating a secret does not keep serving the old token. Tokens are put on
the resolved spec inside `request_send` and never on the `RequestDoc`,
which is what keeps them out of SQLite, exports and sync. The browser is
opened by a closure the desktop layer passes in, so the flow is testable
without a window.
