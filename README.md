# ModelDeck

A Rust/Dioxus control plane for the docker/AI stacks on **UbuntuAI-AMD** (VM 102,
ROCm/gfx1201) and **UbuntuAI-Nvidia** (VM 107, CUDA), packaged as a Home Assistant
add-on with GitHub CI → GHCR — a sibling of [OrderTracker](https://github.com/shadowbrok3r/OrderTracker).

It turns the commented-out `services:` blocks in your hand-edited
`~/jarvis/docker-compose.yml` into first-class, swappable **service profiles**: save
a known-good llama.cpp / vLLM / ollama config, then activate it to swap + restart
the running LLM service — ollama-easy, but for whole containers. Each profile carries
its tied files (custom chat templates, grammars) into a per-model directory so they
can never be used by the wrong model.

## Architecture

```
HA add-on (hub, Dioxus fullstack)            AI VMs (proxmox2)
┌────────────────────┐  HTTPS (bearer)  ┌────────────────────────┐
│  ModelDeck hub UI  │ ───────────────▶ │ modeldeck-agent :9777  │ VM102 AMD  (rocm)
│  ingress :8099     │ ───────────────▶ │ modeldeck-agent :9777  │ VM107 Nvidia (cuda)
│  SurrealDB client  │                  └────────────────────────┘
└─────────┬──────────┘     agents mount /var/run/docker.sock + ~/jarvis
          │ profiles, vm targets, active pointer
          ▼
   SurrealDB  ns:jarvis  db:modeldeck   (wss://surrealdb.shadowbroker.app)
```

- **`crates/shared`** (`modeldeck_shared`) — domain types shared by all three: `ServiceProfile`,
  `TiedFile`, `VmTarget`, `ModelFile`, `GpuStats`, `ContainerStatus`, agent DTOs.
- **`crates/hub`** (`model_deck`) — the Dioxus 0.7 fullstack app / HA add-on. UI for the four
  sections; server fns persist profiles to SurrealDB and proxy to the agents. Reuses
  OrderTracker's HA-ingress middleware and SurrealDB singleton.
- **`crates/agent`** (`modeldeck-agent`) — axum service on each VM. Shells out to
  `docker`/`docker compose`, reads GPU stats (nvidia-smi / AMD sysfs), lists+downloads
  models (`hf download`), confined file IO, and performs the profile swap.

## How a swap works

Activating a profile (`POST /activate`) makes the agent:
1. Write each tied file to `~/jarvis/services/<model-filename>/`.
2. Write the optional `Dockerfile.*` to the jarvis root (build context).
3. Render the profile's compose fragment to `~/jarvis/<slug>.mdk.yml` (jarvis root, so
   `./models`, `./Dockerfile.llama`, etc. resolve exactly like your hand-edited compose).
4. `docker compose -p modeldeck-llm -f <file> up -d --remove-orphans` — replacing whatever
   ran under the stable swap project. Your `open-webui` / `sillytavern` / `proxy` services in
   the main `jarvis` project are left untouched.
5. Health-check the published port and tail logs back to the UI.

## Deploy runbook

1. **SurrealDB schema** (once):
   `surreal sql --endpoint wss://surrealdb.shadowbroker.app --username root --password <pass> < db/schema.surql`
2. **Push to GitHub** as `shadowbrok3r/ModelDeck` (CI builds `modeldeck-{arch}` + `modeldeck-agent-{arch}`).
3. **Agents** — pick a shared secret, then on each VM:
   - AMD: `MODELDECK_AGENT_SECRET=… docker compose -f deploy/agent.amd.compose.yml up -d`
   - Nvidia: `MODELDECK_AGENT_SECRET=… docker compose -f deploy/agent.nvidia.compose.yml up -d`
4. **HA add-on** — add this repo in HA → install ModelDeck → set `SURREAL_PASS`,
   `MODELDECK_AGENT_SECRET` (same as agents), `HF_TOKEN`.
5. In the add-on **Settings**, add the two VM targets (id `amd`/`nvidia`, each agent's
   URL `http://<vm-ip-or-tailscale>:9777`). These live in the database, not in this repo.

## Status

Scaffold complete. Not yet compiled (the Proxmox host it was authored on has no Rust
toolchain) — first `cargo`/`dx` pass happens via CI or a VM build; expect minor fixups,
especially in the hand-written Dioxus `rsx!` and the SurrealDB row derives.
