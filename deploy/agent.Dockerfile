# ModelDeck agent image. Build context = repo root.
#   docker build -f deploy/agent.Dockerfile -t modeldeck-agent .
FROM rust:1-bookworm AS builder
WORKDIR /build
COPY . .
RUN cargo build -p modeldeck-agent --release

# ---------------------------------------------------------------------------
FROM debian:bookworm-slim

# docker CLI + compose plugin (talks to the host daemon via the mounted socket),
# curl (health checks), python + huggingface_hub (model downloads).
RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates curl gnupg python3 python3-pip \
    && install -m 0755 -d /etc/apt/keyrings \
    && curl -fsSL https://download.docker.com/linux/debian/gpg -o /etc/apt/keyrings/docker.asc \
    && chmod a+r /etc/apt/keyrings/docker.asc \
    && echo "deb [arch=$(dpkg --print-architecture) signed-by=/etc/apt/keyrings/docker.asc] https://download.docker.com/linux/debian bookworm stable" \
        > /etc/apt/sources.list.d/docker.list \
    && apt-get update && apt-get install -y --no-install-recommends \
        docker-ce-cli docker-compose-plugin \
    && pip3 install --no-cache-dir --break-system-packages "huggingface_hub[hf_transfer]" hf_xet \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /build/target/release/modeldeck-agent /usr/local/bin/modeldeck-agent

ENV MODELDECK_PORT=9777
EXPOSE 9777
ENTRYPOINT ["/usr/local/bin/modeldeck-agent"]
