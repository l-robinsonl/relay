# Developing Relay

## Prerequisites

- **Rust** stable (1.85+). `rustup toolchain install stable`
- **Node** 22.12+ or 24+.
- Platform build dependencies for Tauri, below.

### Linux (Debian, Ubuntu, KDE neon, Mint)

Tauri renders through the system WebKit, so the desktop shell needs GTK and
WebKitGTK development headers. These are system packages and need `sudo`:

```sh
sudo apt update && sudo apt install -y \
  libwebkit2gtk-4.1-dev \
  libgtk-3-dev \
  libayatana-appindicator3-dev \
  librsvg2-dev \
  libdbus-1-dev \
  libsoup-3.0-dev \
  pkg-config \
  build-essential \
  curl wget file
```

Fedora:

```sh
sudo dnf install -y webkit2gtk4.1-devel gtk3-devel libappindicator-gtk3-devel \
  librsvg2-devel dbus-devel libsoup3-devel pkgconf-pkg-config \
  @development-tools
```

Arch:

```sh
sudo pacman -S --needed webkit2gtk-4.1 gtk3 libayatana-appindicator librsvg \
  dbus libsoup3 pkgconf base-devel
```

**None of this is needed to work on the Rust crates or the frontend** — only
to build and run the desktop shell itself. `cargo test --workspace --exclude
relay-desktop` and `npm test` both run on a bare machine.

Run `./scripts/check-deps.sh` to see what is missing.

### macOS

Xcode command line tools: `xcode-select --install`.

### Windows

Microsoft C++ Build Tools, and WebView2 (preinstalled on Windows 11; the
installer bundles a bootstrapper for Windows 10).

## Layout

| Path | What it is |
|---|---|
| `crates/relay-models` | Shared domain types. No I/O. |
| `crates/relay-http` | The HTTP engine. |
| `crates/relay-storage` | SQLite schema, migrations, repositories. |
| `crates/relay-core` | Interpolation, request assembly, import/export. |
| `crates/relay-app` | Application services — the logic behind every command. |
| `apps/desktop/src-tauri` | Tauri command wrappers and app setup. |
| `apps/desktop/src` | The Vue 3 frontend. |
| `packages/shared-types` | TypeScript bindings, generated from Rust. |

Business logic lives in `relay-app` rather than in the Tauri layer so it can be
tested with `cargo test` instead of by driving a window.

## Commands

```sh
npm install              # once

npm run dev              # frontend only, in a browser at :1420
npm run tauri dev        # the full desktop app
npm run tauri build      # installers for the current platform

npm test                 # frontend tests (vitest)
npm run typecheck        # vue-tsc
npm run build            # typecheck + production frontend build

cargo test --workspace   # every Rust test
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings

npm run bindings         # regenerate packages/shared-types from the Rust types
```

## The type contract

TypeScript definitions in `packages/shared-types/src/generated` are produced
from the Rust structs by `ts-rs`. They are committed, so a reviewer can see
the wire format change in a diff.

After changing any type in `relay-models`, run `npm run bindings`. If you
forget, `npm run typecheck` fails — which is the point.

`i64` and `u64` fields are annotated `#[ts(type = "number")]`. Without that,
`ts-rs` maps them to `bigint`, which is wrong: `serde_json` writes them as JSON
numbers and the webview receives `number`.

## Testing

- **Rust unit tests** sit beside the code they test.
- **Rust integration tests** run against a real local HTTP server
  (`crates/relay-http/tests/support`) on a real socket, because header
  encoding, redirects, chunked transfer and early disconnects only misbehave at
  the socket.
- **End-to-end tests** (`crates/relay-app/tests/end_to_end.rs`) drive the whole
  stack — database, resolution, engine, history — exactly as a Tauri command
  would.
- **Frontend tests** use Vitest with the IPC layer mocked.

Every test that asserts a secret is absent first asserts the secret was
actually transmitted, so the test cannot pass by sending nothing.

## Conventions

Rust: idiomatic, structured errors, no `unwrap()` on a request path, documented
public APIs.

TypeScript and Vue: `<script setup lang="ts">`, strict mode, no trailing
semicolons, a space between a function name and its parentheses, business logic
in stores rather than components.
