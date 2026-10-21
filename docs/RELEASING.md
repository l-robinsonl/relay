# Releasing Relay

## What gets built

`npm run tauri build` produces, for the platform it runs on:

| Platform | Artefacts |
|---|---|
| Linux | `.deb`, `.rpm`, `.AppImage` |
| Windows | `.msi` (WiX), `.exe` (NSIS) |
| macOS | `.app`, `.dmg` |

They land in `target/release/bundle/`. Tauri cannot cross-compile the
bundles, so each platform's artefacts are built on that platform — which
is what the `desktop` job in `.github/workflows/ci.yml` does.

`--no-bundle` builds the binary alone, which is what CI uses on every pull
request: it catches a broken build without spending minutes packaging
artefacts nobody will download.

## Before tagging

```sh
npm run verify          # deps, Rust tests, clippy, typecheck, frontend tests, build
cargo test --workspace  # includes the desktop crate
```

Then, with a PostgreSQL database available, confirm the sync server is
equivalent on both backends — a release that works on SQLite and quietly
misbehaves on Postgres is the failure this catches:

```sh
RELAY_TEST_POSTGRES_URL="$RELAY_DATABASE_URL" \
  cargo test -p relay-server --test conformance -- --test-threads=1
```

## Version numbers

The version lives in three places and they must agree:

- `Cargo.toml` → `[workspace.package] version`
- `apps/desktop/src-tauri/tauri.conf.json` → `version`
- `package.json` files → `version`

`tauri.conf.json` is what the installer and the About box show.

## Signing

Unsigned builds work, but every platform makes the user fight for it.

**Windows.** Without an Authenticode certificate, SmartScreen warns on
first run and the warning persists until enough people install it anyway.
An OV certificate needs reputation to accumulate; an EV certificate skips
that. Set `tauri.conf.json` → `bundle.windows.certificateThumbprint`.

**macOS.** Unsigned and unnotarised applications are refused outright on
anything recent — Gatekeeper does not offer an override in the normal
flow. This needs a paid Apple Developer account, a Developer ID
Application certificate, and notarisation:

```sh
export APPLE_CERTIFICATE="$(base64 -w0 cert.p12)"
export APPLE_CERTIFICATE_PASSWORD=…
export APPLE_SIGNING_IDENTITY="Developer ID Application: …"
export APPLE_ID=…
export APPLE_PASSWORD=…        # an app-specific password
export APPLE_TEAM_ID=…
npm run tauri build
```

**Linux.** Nothing is signed. A `.deb` in an apt repository would want a
GPG-signed `Release` file; a bare download does not.

Relay publishes no signed builds yet. Until it does, the honest thing is
to say so on the download page rather than let people discover it at the
Gatekeeper dialog.

## Updating

There is no auto-updater. Tauri ships one, but it needs an update server
and a signing key, and shipping a self-updating binary before there is a
release process to feed it would be premature. Installing a new version
over the old one works: the database lives in the user's data directory,
not beside the binary.

## The database survives upgrades

Relay's SQLite database is at:

| Platform | Path |
|---|---|
| Linux | `~/.local/share/relay/relay.db` |
| macOS | `~/Library/Application Support/relay/relay.db` |
| Windows | `%APPDATA%\relay\relay.db` |

Migrations run at startup and are append-only, so a newer build upgrades
an older database in place. A build that is *older* than the database
refuses to start rather than corrupting it, with a message naming both
schema versions.

Before a release that adds a migration, test the upgrade on a real
database rather than an empty one:

```sh
cp ~/.local/share/relay/relay.db /tmp/before.db
# install the new build, start it, confirm the data is intact
```

## Checklist

- [ ] `npm run verify` passes
- [ ] Conformance suite passes against PostgreSQL
- [ ] Versions agree across `Cargo.toml`, `tauri.conf.json`, `package.json`
- [ ] Upgrade tested against a database from the previous release
- [ ] `CHANGELOG` updated
- [ ] Artefacts built on each target platform
- [ ] Known-unsigned platforms stated on the download page
