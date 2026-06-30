# Supervisor's build context is only this app folder, not the monorepo root, so
# fetch the full workspace tree (public clone) to build the hub crate.
FROM rust:1-bookworm AS builder

ARG GIT_URL=https://github.com/shadowbrok3r/ModelDeck.git

RUN apt-get update && apt-get install -y \
    pkg-config libssl-dev git curl ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# dx CLI pinned to the dioxus = "0.7" library (built from source: prebuilt
# binaries require glibc 2.39, bookworm has 2.36). --locked uses dioxus-cli's own
# lockfile (pins git2 < 0.21 for auth-git2 compatibility).
RUN cargo install dioxus-cli@0.7.9 --locked

WORKDIR /build

# Bust the clone cache when this add-on folder changes (Rust sources live only on
# GitHub, so Dockerfile/config bumps are what invalidate the layer).
COPY Dockerfile /tmp/addon.Dockerfile
COPY config.yaml /tmp/addon.config.yaml

ARG BUILD_VERSION=0.0.0
ARG BUILD_ARCH=amd64

RUN echo "ModelDeck build ${BUILD_VERSION} (${BUILD_ARCH})" \
    && git clone --depth 1 "${GIT_URL}" .

# build.rs reads .env; provide an empty one.
RUN touch crates/hub/.env

WORKDIR /build/crates/hub
RUN dx build --release --platform web --debug-symbols false

# ---------------------------------------------------------------------------
# Runtime
# ---------------------------------------------------------------------------
FROM ghcr.io/home-assistant/base-debian:bookworm

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates libssl3 \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Workspace target dir is at the repo root.
COPY --from=builder /build/target/dx/model_deck/release/web/server /app/model_deck
COPY --from=builder /build/target/dx/model_deck/release/web/public /app/public

# s6 supervises the long-running app so Stop/Restart from Supervisor sends SIGTERM.
COPY rootfs /
RUN chmod a+x /etc/services.d/modeldeck-hub/run
