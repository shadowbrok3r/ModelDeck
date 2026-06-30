# Thin runtime image for CI: the agent binary is compiled on the runner (with
# rust-cache); this packages it with the docker CLI + huggingface_hub. Build context
# is a staged dir containing the `modeldeck-agent` binary.
#
# (deploy/agent.Dockerfile remains the self-contained from-source build for manual
# `docker build` use.)
FROM debian:bookworm-slim

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

COPY modeldeck-agent /usr/local/bin/modeldeck-agent

ENV MODELDECK_PORT=9777
EXPOSE 9777
ENTRYPOINT ["/usr/local/bin/modeldeck-agent"]
