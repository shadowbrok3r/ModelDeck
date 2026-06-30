# Thin runtime image for CI: the hub is compiled on the runner (with rust-cache),
# and this only packages the artifacts. Build context is a staged dir containing
# `model_deck` (the server binary), `public/`, and `rootfs/`.
#
# (homeassistant/Dockerfile remains the self-contained from-source build used when
# the HA Supervisor has to build locally instead of pulling the prebuilt image.)
FROM ghcr.io/home-assistant/base-debian:bookworm

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates libssl3 \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY model_deck /app/model_deck
COPY public /app/public
COPY rootfs /
RUN chmod a+x /etc/services.d/modeldeck-hub/run
