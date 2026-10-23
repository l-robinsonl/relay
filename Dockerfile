# Relay's synchronisation server.
#
# Two stages: a builder that compiles the workspace, and a runtime image
# holding one binary and the certificates it needs to reach PostgreSQL
# and a mail provider. The desktop application is not built here — this
# image is only the server a team self-hosts.

# --- build ------------------------------------------------------------------
# Pinned, so an image built today and one built next year are the same
# image. sqlx 0.9 needs 1.94 at the least; see rust-version in Cargo.toml.
FROM rust:1.99.1-bookworm AS builder

WORKDIR /build

# Dependencies first, in their own layer, so editing Relay's own source
# does not re-download and recompile the whole crate graph.
COPY Cargo.toml Cargo.lock ./
# Every crate's manifest, by wildcard rather than by name. An earlier
# version listed them, and adding a crate broke this build with an
# error about a workspace member that could not be found — a long way
# from the change that caused it.
COPY crates/ /tmp/crates/
COPY server/Cargo.toml server/

# Stub sources, enough for `cargo build` to resolve and compile the
# dependency graph. The real sources replace them below.
RUN set -eux; \
    for manifest in /tmp/crates/*/Cargo.toml; do \
      crate="crates/$(basename "$(dirname "$manifest")")"; \
      mkdir -p "$crate/src"; \
      cp "$manifest" "$crate/Cargo.toml"; \
      echo '' > "$crate/src/lib.rs"; \
    done; \
    rm -rf /tmp/crates; \
    mkdir -p server/src && echo 'fn main() {}' > server/src/main.rs; \
    # The desktop crate is not part of this image and needs GTK to build.
    sed -i '/apps\/desktop\/src-tauri/d' Cargo.toml; \
    cargo build --release -p relay-server 2>/dev/null || true

# Now the real thing.
COPY crates/ crates/
COPY server/ server/

# Touching the sources is not enough on its own: cargo keys on mtime, and
# COPY can preserve one older than the stub build above.
RUN set -eux; \
    sed -i '/apps\/desktop\/src-tauri/d' Cargo.toml; \
    find crates server -name '*.rs' -exec touch {} +; \
    cargo build --release -p relay-server; \
    strip target/release/relay-server

# --- runtime ----------------------------------------------------------------
FROM debian:bookworm-slim AS runtime

# ca-certificates only: SQLite is compiled in, TLS is rustls, and there
# is no other native dependency. Without it, every outbound connection —
# PostgreSQL over TLS, the mail provider — fails with an opaque
# certificate error.
RUN set -eux; \
    apt-get update; \
    apt-get install -y --no-install-recommends ca-certificates curl; \
    rm -rf /var/lib/apt/lists/*

# Unprivileged, and owning the data directory so a SQLite deployment can
# actually write to its volume.
RUN useradd --system --create-home --uid 10001 relay \
 && mkdir -p /data \
 && chown relay:relay /data

COPY --from=builder /build/target/release/relay-server /usr/local/bin/relay-server

USER relay
WORKDIR /home/relay

ENV RELAY_BIND=0.0.0.0:8787 \
    RELAY_DATABASE_URL=sqlite:///data/relay.db \
    RELAY_LOG=info,sqlx=warn

EXPOSE 8787
VOLUME ["/data"]

# The orchestrator restarts an unhealthy container; /health is cheap and
# touches the database, so it fails when the backend is gone rather than
# only when the process has died.
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
    CMD curl --fail --silent --show-error http://127.0.0.1:8787/health || exit 1

ENTRYPOINT ["/usr/local/bin/relay-server"]
