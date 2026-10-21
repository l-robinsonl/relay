# Relay

A local-first API client. It starts instantly, works entirely offline, and
never asks you to create an account.

Your workspaces, collections and requests live in a SQLite database on your own
machine. If a team wants to share them, they can run one small server
themselves — or pay us a small amount to run it for them. Nothing about the
desktop application changes either way.

> **Status: Phases 1–3 complete.** The HTTP engine, local persistence,
> variable resolution, import/export, the desktop interface, the
> self-hosted sync server and the client that talks to it are built and
> tested — against SQLite and against PostgreSQL. The hosted service and
> billing are designed but not yet implemented. See
> [the roadmap](#roadmap) for exactly what works today.

## Why

Most API clients want an account before they will let you send a GET request,
sync your collections to someone else's infrastructure by default, and treat
the secrets in your environment variables as ordinary data. Relay is built the
other way round:

- **No account, ever, for local use.** There is no sign-in screen.
- **Local-first.** The local database is authoritative. Nothing waits on a
  network round trip to show you your own data.
- **Real HTTP.** Requests execute in Rust through `reqwest`, not in a webview's
  `fetch`. That is what makes arbitrary methods, cookie control, client
  certificates, streaming bodies, real cancellation and honest timings
  possible.
- **Secrets stay put.** Values marked secret are never written to the database,
  never exported, and never synchronised — by construction, not by filter.
- **No telemetry.** None. There is no analytics code to disable.

## Install

No binaries are published yet. To run it from source, see
[docs/DEVELOPMENT.md](docs/DEVELOPMENT.md).

```sh
git clone <this repository> && cd relay
./scripts/check-deps.sh      # tells you what your platform is missing
npm install
npm run tauri dev
```

## What works today

**HTTP engine** — GET, POST, PUT, PATCH, DELETE, HEAD, OPTIONS and arbitrary
custom methods. JSON, text, XML, form-urlencoded, multipart, GraphQL and raw
binary bodies, with file uploads streamed from disk. Basic, bearer and API-key
auth. Per-request timeouts, redirect policy, proxies, client certificates, a
shared cookie jar, and automatic gzip/brotli/deflate/zstd decoding. Real
cancellation that closes the connection. Responses are bounded and spool to
disk instead of filling memory.

**Workspaces** — unlimited local workspaces, nested collections and folders,
saved requests, duplication, moving and reordering. Deletion is a tombstone, so
a deletion made offline is not lost.

**Variables** — `{{name}}` resolution across workspace, collection, environment
and request scopes with documented precedence, live highlighting of unresolved
references in the URL bar, and nested variables that terminate on a cycle
rather than hanging.

**Secrets** — environment variables can be marked secret. The name is stored;
the value goes to a credential store and is injected only for the moment a
request is sent.

**Import and export** — bring in a Postman v2.1 collection or environment,
paste a `curl` command, or restore a Relay export. Every import is previewed
first: you see what will be created *and* what cannot be represented before
anything is written, and the whole thing lands in one transaction or not at
all. Copy any request back out as `curl`, or export an entire workspace as
portable JSON.

**Interface** — a designed dark theme and an equally designed light one,
request tabs, resizable and re-orientable panes, drag-and-drop reordering, a
command palette, keyboard shortcuts throughout, and local request history.

**Synchronisation** — optional, per workspace. Connect to a server you run,
sign in, and your collections follow you between machines. Changes from a
teammate arrive within moments over a live connection; edits you make
offline upload when you reconnect. When two people change the same thing,
you get both versions side by side and choose — nothing is overwritten on
the strength of a timestamp. Secret values never leave your device.

## What does not work yet

Stated plainly, because a README that implies otherwise wastes your time:

- **Synchronisation and the server.** Designed in detail, schema in place
  (revisions, tombstones, an outbox table), but no server exists yet.
- **Relay Cloud and billing.** Designed, not built.
- **Scripts and assertions.** You can write and save them, and a Postman
  import brings them across, but nothing executes them. The sandbox is
  Phase 4.
- **OAuth 2.0 flows.** The configuration round-trips; the flows do not run.
  A Postman OAuth 2.0 block imports as a bearer token.
- **OpenAPI import.** Phase 4. Postman, cURL and Relay exports all work.
- **Digest, NTLM, AWS and Hawk auth.** Reported on import, not supported.

## Roadmap

| Phase | Scope | Status |
|---|---|---|
| 1 | Desktop shell, HTTP engine, SQLite persistence, save and reopen | **Done** |
| 2 | Collections, environments, interpolation, auth, history, import/export | **Done** |
| 3 | Self-hosted Axum sync server, users, revisions, WebSockets, conflicts | **Done** |
| 4 | OAuth 2.0 flows, sandboxed scripting, OpenAPI import, certificates | Designed |
| 5 | Installers, signing, CI, benchmarks, security review | Not started |
| 6 | Relay Cloud on Neon Postgres, multitenant, RLS | Designed |
| 7 | Stripe billing, per-seat annual subscriptions, entitlements | Designed |

## Hosting

Three modes, one protocol, no lock-in:

| | Data | Account | Cost |
|---|---|---|---|
| **Local-only** | Your disk | None | Free |
| **Self-hosted** | Your server | Yours, self-administered | Free |
| **Relay Cloud** | Managed | Relay Cloud | £1/seat/month, annual |

The paid tier buys somebody else running the server. It buys no features.
Export is never an entitlement: it keeps working when a subscription lapses,
because holding your own API collections hostage over £12 a year would be
indefensible.

See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for the full design.

## Security

- Requests execute on your machine. Relay is never a server-side request proxy.
- TLS verification is on by default; disabling it is per-request and the
  interface says so loudly.
- The webview is granted no filesystem, shell or HTTP permissions. Files are
  read only through a dialog you opened.
- Credentials are redacted before anything is written to history, and again
  inside the storage layer, so a forgetful caller cannot leak one.
- Every database query is parameterised.

Found a problem? Open an issue, or email the maintainers for anything
sensitive.

## Licence

MIT or Apache-2.0, at your option.
