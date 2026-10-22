# Relay

[![CI](https://github.com/l-robinsonl/relay/actions/workflows/ci.yml/badge.svg)](https://github.com/l-robinsonl/relay/actions/workflows/ci.yml)
[![Licence: MIT OR Apache-2.0](https://img.shields.io/badge/licence-MIT%20OR%20Apache--2.0-blue)](#licence)

A local-first API client. It starts instantly, works entirely offline, and
never asks you to create an account.

Your workspaces, collections and requests live in a SQLite database on your own
machine. If a team wants to share them, they can run one small server
themselves — or pay us a small amount to run it for them. Nothing about the
desktop application changes either way.

> **Status: Phases 1–4 complete.** The HTTP engine, local persistence,
> variable resolution, import/export, the desktop interface, OAuth 2.0,
> scripting, the self-hosted sync server and the client that talks to it
> are built and tested — against SQLite and against PostgreSQL, on Linux,
> macOS and Windows. The hosted service and billing are designed but not
> yet implemented. See [the roadmap](#roadmap) for exactly what works
> today.

## Why

Postman will not let you send a GET request until you have signed in. Your
collections sync to their infrastructure unless you stop them. The offline
mode that used to exist has been whittled away. None of that is necessary
to send an HTTP request and look at the response.

Relay is built the other way round:

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

Being small is a consequence of those choices rather than the point of
them — Relay uses the webview your system already has instead of shipping
a Chromium, which on one machine measured 221 MB resident against
Postman's 1,067 MB, and 7.3 MB to download against 199 MB. Pleasant, but
not the reason to switch. [The numbers and their caveats](docs/FOOTPRINT.md).

## Install

No binaries are published yet, but `npm run tauri build` produces real
installers — `.deb`, `.rpm` and `.AppImage` on Linux, `.msi`/`.exe` on
Windows, `.dmg` on macOS. See [docs/RELEASING.md](docs/RELEASING.md).

Nothing is code-signed yet, so Windows SmartScreen will warn and macOS
Gatekeeper will refuse outright. That is stated plainly rather than left
for you to discover at the dialog.

To run it from source, see [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md).

```sh
git clone <this repository> && cd relay
./scripts/check-deps.sh      # tells you what your platform is missing
npm install
npm run tauri dev
```

## What works today

**HTTP engine** — GET, POST, PUT, PATCH, DELETE, HEAD, OPTIONS and arbitrary
custom methods. JSON, text, XML, form-urlencoded, multipart, GraphQL and raw
binary bodies, with file uploads streamed from disk. Basic, bearer, API-key
and OAuth 2.0 auth. Per-request timeouts, redirect policy, proxies, client
certificates scoped to the hosts you name, a shared cookie jar, and automatic
gzip/brotli/deflate/zstd decoding. Real
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

**OAuth 2.0** — the authorization code grant with PKCE, signing in through
your real browser rather than an embedded one, plus client credentials and
the password grant. Tokens refresh themselves before they lapse and are
held in memory only: they are never written to the database, put in an
export, or sent to a sync server, so a request can be shared without
sharing your access.

**Import and export** — bring in an OpenAPI 3 document (JSON or YAML), a
Postman v2.1 collection or environment, paste a `curl` command, or restore
a Relay export. Every import is previewed
first: you see what will be created *and* what cannot be represented before
anything is written, and the whole thing lands in one transaction or not at
all. Copy any request back out as `curl`, or export an entire workspace as
portable JSON.

**Scripts** — a pre-request script can rewrite the request before it goes
out, to sign it or stamp it; a post-response script asserts against what
came back and captures a value for the next request. Both run in a sandbox
with no filesystem, network or process access and hard limits on time and
memory, and neither can read a credential.
[How it works](docs/SCRIPTING.md).

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

- **Relay Cloud and billing.** Designed, not built.
- **Encrypted client-certificate keys.** `rustls` cannot ask for a
  passphrase, so the key in the PEM has to be decrypted first. Relay says
  so, and says how.
- **The OAuth 2.0 implicit grant.** Deliberately not implemented: it
  returns tokens in a URL fragment and issues no refresh token. Use the
  authorization code grant with PKCE, which Relay supports.
- **Digest, NTLM, AWS and Hawk auth.** Reported on import, not supported.

## Roadmap

| Phase | Scope | Status |
|---|---|---|
| 1 | Desktop shell, HTTP engine, SQLite persistence, save and reopen | **Done** |
| 2 | Collections, environments, interpolation, auth, history, import/export | **Done** |
| 3 | Self-hosted Axum sync server, users, revisions, WebSockets, conflicts | **Done** |
| 4 | OAuth 2.0, scripting, OpenAPI import, certificates | **Done** |
| 5 | Installers, signing, CI, benchmarks, security review | In progress — installers build, signing and CI unrun |
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

MIT or Apache-2.0, at your option — [LICENSE-MIT](LICENSE-MIT) and
[LICENSE-APACHE](LICENSE-APACHE). Unless you say otherwise, any
contribution you intentionally submit for inclusion is dual licensed on
the same terms, with no additional conditions.
