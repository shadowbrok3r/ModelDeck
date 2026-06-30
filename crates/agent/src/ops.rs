//! All side-effecting work the agent performs on its VM: docker/compose control,
//! GPU stats, model discovery, confined file IO, and profile activation. Everything
//! shells out to the host tooling (docker CLI, curl, hf, nvidia-smi) the same way
//! the operator does by hand, so behavior matches the existing workflow.

use std::path::{Component, Path, PathBuf};
use std::process::Stdio;

use anyhow::{anyhow, bail, Context, Result};
use modeldeck_shared::*;
use tokio::process::Command;

/// Agent configuration, sourced from env at startup.
#[derive(Clone)]
pub struct Config {
    pub secret: String,
    pub jarvis: PathBuf,
    pub accel: AccelKind,
    pub port: u16,
    /// Stable compose project name for the single active LLM service (swap target).
    pub project: String,
    pub version: String,
}

impl Config {
    pub fn from_env() -> Self {
        let jarvis = std::env::var("MODELDECK_JARVIS")
            .unwrap_or_else(|_| "/home/shadowbroker/jarvis".to_string());
        Config {
            secret: std::env::var("MODELDECK_AGENT_SECRET").unwrap_or_default(),
            jarvis: PathBuf::from(jarvis),
            accel: AccelKind::from_str_loose(
                &std::env::var("MODELDECK_ACCEL").unwrap_or_else(|_| "cpu".to_string()),
            ),
            port: std::env::var("MODELDECK_PORT")
                .ok()
                .and_then(|p| p.parse().ok())
                .unwrap_or(9777),
            project: std::env::var("MODELDECK_PROJECT")
                .unwrap_or_else(|_| "modeldeck-llm".to_string()),
            version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }

    pub fn models_dir(&self) -> PathBuf {
        self.jarvis.join("models")
    }
    pub fn services_dir(&self) -> PathBuf {
        self.jarvis.join("services")
    }
    pub fn state_dir(&self) -> PathBuf {
        self.jarvis.join(".modeldeck")
    }
}

// ---------------------------------------------------------------------------
// Command helpers
// ---------------------------------------------------------------------------

struct Out {
    code: i32,
    stdout: String,
    stderr: String,
}

async fn run(program: &str, args: &[&str]) -> Result<Out> {
    let output = Command::new(program)
        .args(args)
        .output()
        .await
        .with_context(|| format!("spawning {program}"))?;
    Ok(Out {
        code: output.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
    })
}

/// Run, returning stdout on success or an error carrying stderr.
async fn run_ok(program: &str, args: &[&str]) -> Result<String> {
    let o = run(program, args).await?;
    if o.code != 0 {
        bail!("{program} {:?} exited {}: {}", args, o.code, o.stderr.trim());
    }
    Ok(o.stdout)
}

// ---------------------------------------------------------------------------
// Docker / compose
// ---------------------------------------------------------------------------

pub async fn docker_version() -> String {
    run("docker", &["version", "--format", "{{.Server.Version}}"])
        .await
        .map(|o| o.stdout.trim().to_string())
        .unwrap_or_default()
}

/// `docker ps -a`, parsed with compose labels split out.
pub async fn containers() -> Result<Vec<ContainerStatus>> {
    let out = run_ok("docker", &["ps", "-a", "--no-trunc", "--format", "{{json .}}"]).await?;
    let mut list = Vec::new();
    for line in out.lines().filter(|l| !l.trim().is_empty()) {
        let v: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let labels = v.get("Labels").and_then(|x| x.as_str()).unwrap_or("");
        let label = |k: &str| -> Option<String> {
            labels.split(',').find_map(|kv| {
                kv.split_once('=')
                    .filter(|(key, _)| *key == k)
                    .map(|(_, val)| val.to_string())
            })
        };
        let status = v.get("Status").and_then(|x| x.as_str()).unwrap_or("").to_string();
        let health = if status.contains("(healthy)") {
            Some("healthy".into())
        } else if status.contains("(unhealthy)") {
            Some("unhealthy".into())
        } else if status.contains("(health: starting)") {
            Some("starting".into())
        } else {
            None
        };
        let ports = v
            .get("Ports")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        list.push(ContainerStatus {
            name: v.get("Names").and_then(|x| x.as_str()).unwrap_or("").to_string(),
            image: v.get("Image").and_then(|x| x.as_str()).unwrap_or("").to_string(),
            state: v.get("State").and_then(|x| x.as_str()).unwrap_or("").to_string(),
            status,
            ports,
            compose_project: label("com.docker.compose.project"),
            compose_service: label("com.docker.compose.service"),
            health,
        });
    }
    Ok(list)
}

/// `docker compose ls --all`.
pub async fn compose_projects() -> Result<Vec<ComposeProject>> {
    let out = run_ok("docker", &["compose", "ls", "--all", "--format", "json"]).await?;
    let arr: Vec<serde_json::Value> = serde_json::from_str(out.trim()).unwrap_or_default();
    Ok(arr
        .into_iter()
        .map(|v| ComposeProject {
            name: v.get("Name").and_then(|x| x.as_str()).unwrap_or("").to_string(),
            status: v.get("Status").and_then(|x| x.as_str()).unwrap_or("").to_string(),
            config_files: v
                .get("ConfigFiles")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect(),
        })
        .collect())
}

/// `docker compose -f <file> [-p <project>] up -d --remove-orphans`.
pub async fn compose_up(file: &Path, project: Option<&str>) -> Result<String> {
    let f = file.to_string_lossy().to_string();
    let mut args = vec!["compose", "-f", &f];
    if let Some(p) = project {
        args.push("-p");
        args.push(p);
    }
    args.extend_from_slice(&["up", "-d", "--remove-orphans"]);
    run_ok("docker", &args).await
}

pub async fn compose_down(project: &str) -> Result<String> {
    run_ok("docker", &["compose", "-p", project, "down"]).await
}

/// Restart a single container by name (`docker restart`).
pub async fn restart_container(name: &str) -> Result<String> {
    run_ok("docker", &["restart", name]).await
}

/// Last `tail` log lines from a container.
pub async fn logs(container: &str, tail: u32) -> Result<String> {
    let t = tail.to_string();
    let o = run("docker", &["logs", "--tail", &t, container]).await?;
    // docker logs writes app output to stderr for many images; merge both.
    Ok(format!("{}{}", o.stdout, o.stderr))
}

// ---------------------------------------------------------------------------
// GPU
// ---------------------------------------------------------------------------

pub async fn gpu_stats(accel: AccelKind) -> Vec<GpuStats> {
    match accel {
        AccelKind::Cuda => nvidia_stats().await.unwrap_or_default(),
        AccelKind::Rocm => rocm_stats().await,
        AccelKind::Cpu => Vec::new(),
    }
}

async fn nvidia_stats() -> Result<Vec<GpuStats>> {
    let out = run_ok(
        "nvidia-smi",
        &[
            "--query-gpu=index,name,memory.total,memory.used,utilization.gpu,temperature.gpu",
            "--format=csv,noheader,nounits",
        ],
    )
    .await?;
    let mut v = Vec::new();
    for line in out.lines().filter(|l| !l.trim().is_empty()) {
        let f: Vec<&str> = line.split(',').map(|s| s.trim()).collect();
        if f.len() < 6 {
            continue;
        }
        v.push(GpuStats {
            index: f[0].parse().unwrap_or(0),
            name: f[1].to_string(),
            mem_total_mb: f[2].parse().unwrap_or(0),
            mem_used_mb: f[3].parse().unwrap_or(0),
            util_pct: f[4].parse().ok(),
            temp_c: f[5].parse().ok(),
        });
    }
    Ok(v)
}

/// AMD stats from sysfs (no rocm-smi dependency inside the container). Falls back
/// to an empty list if the DRM nodes aren't mounted.
async fn rocm_stats() -> Vec<GpuStats> {
    let mut v = Vec::new();
    let read = |p: PathBuf| std::fs::read_to_string(p).ok().map(|s| s.trim().to_string());
    for idx in 0..8u32 {
        let base = PathBuf::from(format!("/sys/class/drm/card{idx}/device"));
        if !base.exists() {
            continue;
        }
        let total = read(base.join("mem_info_vram_total"))
            .and_then(|s| s.parse::<u64>().ok())
            .map(|b| b / 1_048_576);
        let used = read(base.join("mem_info_vram_used"))
            .and_then(|s| s.parse::<u64>().ok())
            .map(|b| b / 1_048_576);
        let Some(total) = total else { continue };
        v.push(GpuStats {
            index: idx,
            name: "AMD GPU".to_string(),
            mem_total_mb: total,
            mem_used_mb: used.unwrap_or(0),
            util_pct: read(base.join("gpu_busy_percent")).and_then(|s| s.parse().ok()),
            temp_c: None,
        });
    }
    v
}

// ---------------------------------------------------------------------------
// Models
// ---------------------------------------------------------------------------

/// Walk models/ for weights and hf-cache/hub for cached repos.
pub fn list_models(cfg: &Config) -> Vec<ModelFile> {
    let mut out = Vec::new();
    let vm = cfg.jarvis.to_string_lossy().to_string();
    let _ = &vm;
    // Single-file weights under models/ (recurse a couple levels for nested repos).
    walk_weights(&cfg.models_dir(), 0, 4, &mut out);
    // HF cache repos (one entry per models--*).
    let hub = cfg.jarvis.join("hf-cache").join("hub");
    if let Ok(rd) = std::fs::read_dir(&hub) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if let Some(repo) = name.strip_prefix("models--") {
                let repo = repo.replace("--", "/");
                let size = dir_size(&e.path(), 0, 6);
                out.push(ModelFile {
                    vm: String::new(),
                    path: e.path().to_string_lossy().to_string(),
                    filename: repo,
                    size_bytes: size,
                    format: Some(ModelFormat::HfRepo),
                    quant: None,
                    modified: None,
                });
            }
        }
    }
    out.sort_by(|a, b| a.filename.to_lowercase().cmp(&b.filename.to_lowercase()));
    out
}

fn walk_weights(dir: &Path, depth: usize, max: usize, out: &mut Vec<ModelFile>) {
    if depth > max {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let path = e.path();
        let Ok(ft) = e.file_type() else { continue };
        if ft.is_dir() {
            // Skip the HF cache dotdir under models/.
            if e.file_name().to_string_lossy().starts_with('.') {
                continue;
            }
            walk_weights(&path, depth + 1, max, out);
            continue;
        }
        let fname = e.file_name().to_string_lossy().to_string();
        let lower = fname.to_lowercase();
        if !(lower.ends_with(".gguf") || lower.ends_with(".safetensors")) {
            continue;
        }
        let meta = e.metadata().ok();
        out.push(ModelFile {
            vm: String::new(),
            path: path.to_string_lossy().to_string(),
            filename: fname.clone(),
            size_bytes: meta.as_ref().map(|m| m.len()).unwrap_or(0),
            format: Some(infer_format(&lower)),
            quant: parse_quant(&fname),
            modified: meta
                .and_then(|m| m.modified().ok())
                .map(chrono::DateTime::<chrono::Utc>::from),
        });
    }
}

fn dir_size(dir: &Path, depth: usize, max: usize) -> u64 {
    if depth > max {
        return 0;
    }
    let mut total = 0;
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_symlink() {
                // hf-cache snapshots symlink into blobs/; count the blob target once.
                if let Ok(m) = std::fs::metadata(e.path()) {
                    total += m.len();
                }
            } else if ft.is_dir() {
                total += dir_size(&e.path(), depth + 1, max);
            } else if let Ok(m) = e.metadata() {
                total += m.len();
            }
        }
    }
    total
}

pub async fn delete_model(cfg: &Config, path: &str) -> Result<()> {
    let target = confine(&cfg.models_dir(), path)
        .or_else(|_| confine(&cfg.jarvis.join("hf-cache"), path))
        .context("model path must be under models/ or hf-cache/")?;
    let meta = std::fs::symlink_metadata(&target)?;
    if meta.is_dir() {
        std::fs::remove_dir_all(&target)?;
    } else {
        std::fs::remove_file(&target)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Confined file IO
// ---------------------------------------------------------------------------

/// Resolve `requested` (absolute or relative-to-jarvis) and guarantee it stays
/// within `root`. Rejects any `..` component up front, then verifies via the
/// canonical parent so symlinks can't escape.
pub fn confine(root: &Path, requested: &str) -> Result<PathBuf> {
    let req = Path::new(requested);
    let joined = if req.is_absolute() {
        req.to_path_buf()
    } else {
        root.join(req)
    };
    if joined.components().any(|c| matches!(c, Component::ParentDir)) {
        bail!("path traversal rejected: {requested}");
    }
    let root_c = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    // Canonicalize the existing prefix (parent for new files).
    let check = if joined.exists() {
        joined.canonicalize()?
    } else {
        let parent = joined.parent().ok_or_else(|| anyhow!("no parent for {requested}"))?;
        let parent_c = parent.canonicalize().unwrap_or_else(|_| parent.to_path_buf());
        parent_c.join(joined.file_name().ok_or_else(|| anyhow!("no filename"))?)
    };
    if !check.starts_with(&root_c) {
        bail!("path {} escapes {}", check.display(), root_c.display());
    }
    Ok(check)
}

pub fn read_file(cfg: &Config, path: &str) -> Result<FilePayload> {
    let target = confine(&cfg.jarvis, path)?;
    let content = std::fs::read_to_string(&target)
        .with_context(|| format!("reading {}", target.display()))?;
    Ok(FilePayload {
        path: target.to_string_lossy().to_string(),
        content,
    })
}

pub fn write_file(cfg: &Config, payload: &FilePayload) -> Result<()> {
    let target = confine(&cfg.jarvis, &payload.path)?;
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&target, &payload.content)
        .with_context(|| format!("writing {}", target.display()))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Profile activation (the swap)
// ---------------------------------------------------------------------------

/// Materialize a profile's tied files into `services/<model_dir_name>/`, write its
/// managed compose file at the jarvis root (so the profile's `./models` etc. resolve
/// like the hand-edited compose), then `up -d` under the stable swap project,
/// replacing whatever was active. Health-checks the published port.
pub async fn activate(cfg: &Config, profile: &ServiceProfile) -> Result<ActivateResult> {
    if !is_safe_segment(&profile.model_dir_name) {
        bail!("unsafe model_dir_name: {}", profile.model_dir_name);
    }
    if profile.service_name.trim().is_empty() {
        bail!("profile has no service_name");
    }

    // 1. Tied files -> services/<model_dir_name>/
    let svc_dir = cfg.services_dir().join(&profile.model_dir_name);
    std::fs::create_dir_all(&svc_dir)?;
    for tf in &profile.tied_files {
        if !is_safe_segment(&tf.filename) {
            bail!("unsafe tied filename: {}", tf.filename);
        }
        std::fs::write(svc_dir.join(&tf.filename), &tf.content)?;
    }

    // 2. Optional Dockerfile -> jarvis root (compose build context is jarvis root).
    if let (Some(name), Some(body)) = (&profile.dockerfile_name, &profile.dockerfile) {
        if !is_safe_segment(name) {
            bail!("unsafe dockerfile name: {name}");
        }
        std::fs::write(cfg.jarvis.join(name), body)?;
    }

    // 3. Managed compose file at jarvis root.
    let slug = slugify(&profile.name);
    let slug = if slug.is_empty() { "profile".into() } else { slug };
    let compose_path = cfg.jarvis.join(format!("{slug}.mdk.yml"));
    let doc = render_compose(profile);
    std::fs::write(&compose_path, doc)?;

    // 4. Swap: up under the stable project name (removes the previous service).
    std::fs::create_dir_all(cfg.state_dir())?;
    let up = compose_up(&compose_path, Some(&cfg.project)).await;
    let up_msg = match &up {
        Ok(s) => s.clone(),
        Err(e) => e.to_string(),
    };

    // 5. Record active pointer for the hub to reconcile against.
    let _ = std::fs::write(
        cfg.state_dir().join("active.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "profile_id": profile.id,
            "name": profile.name,
            "service_name": profile.service_name,
            "compose_file": compose_path.to_string_lossy(),
        }))
        .unwrap_or_default(),
    );

    if up.is_err() {
        return Ok(ActivateResult {
            ok: false,
            message: format!("compose up failed: {up_msg}"),
            health_ok: false,
            log_tail: logs(&profile.container_name, 60).await.unwrap_or_default()
                .lines().rev().take(40).map(|s| s.to_string()).collect(),
        });
    }

    // 6. Health check the published port.
    let health_ok = match profile.host_port {
        Some(port) => wait_health(port, 120).await,
        None => true,
    };

    Ok(ActivateResult {
        ok: true,
        message: format!("activated {} ({})", profile.name, up_msg.trim()),
        health_ok,
        log_tail: logs(&profile.container_name, 40).await.unwrap_or_default()
            .lines().map(|s| s.to_string()).collect(),
    })
}

/// Wrap a single-service fragment into a complete, named compose document.
fn render_compose(profile: &ServiceProfile) -> String {
    let frag = profile.compose_fragment.trim_end();
    // If the operator pasted a whole `services:`-rooted doc, keep it; otherwise
    // wrap the bare service entry.
    if frag.trim_start().starts_with("services:") || frag.trim_start().starts_with("name:") {
        frag.to_string()
    } else {
        let indented = frag
            .lines()
            .map(|l| if l.is_empty() { String::new() } else { format!("  {l}") })
            .collect::<Vec<_>>()
            .join("\n");
        format!("services:\n{indented}\n")
    }
}

async fn wait_health(port: u16, timeout_secs: u64) -> bool {
    let url_models = format!("http://127.0.0.1:{port}/v1/models");
    let url_health = format!("http://127.0.0.1:{port}/health");
    let deadline = timeout_secs / 3;
    for _ in 0..deadline {
        for url in [&url_health, &url_models] {
            if let Ok(o) = run("curl", &["-fsS", "-m", "3", "-o", "/dev/null", url]).await {
                if o.code == 0 {
                    return true;
                }
            }
        }
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    }
    false
}

// ---------------------------------------------------------------------------
// HuggingFace download (streamed)
// ---------------------------------------------------------------------------

/// Spawn `hf download` and stream merged stdout/stderr lines over an mpsc channel.
/// Returns a receiver the handler turns into an SSE stream.
pub fn spawn_download(
    cfg: &Config,
    req: &DownloadRequest,
) -> Result<tokio::sync::mpsc::Receiver<String>> {
    use tokio::io::{AsyncBufReadExt, BufReader};

    if req.repo.trim().is_empty() {
        bail!("empty repo");
    }
    let dest_name = req
        .dest
        .clone()
        .filter(|d| is_safe_segment(d))
        .unwrap_or_else(|| {
            req.repo
                .rsplit('/')
                .next()
                .map(|s| s.to_string())
                .unwrap_or_else(|| "model".into())
        });
    let dest = cfg.models_dir().join(&dest_name);
    std::fs::create_dir_all(&dest)?;

    let mut cmd = Command::new("hf");
    cmd.arg("download").arg(&req.repo);
    if let Some(file) = &req.file {
        cmd.arg(file);
    }
    cmd.arg("--local-dir").arg(&dest);
    cmd.env("HF_HUB_ENABLE_HF_TRANSFER", "1");
    cmd.env("HF_HOME", cfg.jarvis.join("hf-cache"));
    if let Some(tok) = &req.hf_token {
        if !tok.is_empty() {
            cmd.env("HF_TOKEN", tok);
        }
    }
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

    let mut child = cmd.spawn().context("spawning hf download (is huggingface_hub installed?)")?;
    let (tx, rx) = tokio::sync::mpsc::channel::<String>(256);

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    tokio::spawn(async move {
        let _ = tx.send(format!("Downloading {dest_name} ...")).await;
        if let Some(o) = stdout {
            let mut lines = BufReader::new(o).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if tx.send(line).await.is_err() {
                    break;
                }
            }
        }
        if let Some(e) = stderr {
            let mut lines = BufReader::new(e).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if tx.send(line).await.is_err() {
                    break;
                }
            }
        }
        match child.wait().await {
            Ok(s) if s.success() => {
                let _ = tx.send("✅ download complete".to_string()).await;
            }
            Ok(s) => {
                let _ = tx.send(format!("❌ hf exited {}", s.code().unwrap_or(-1))).await;
            }
            Err(e) => {
                let _ = tx.send(format!("❌ {e}")).await;
            }
        }
    });
    Ok(rx)
}

// ---------------------------------------------------------------------------
// Info
// ---------------------------------------------------------------------------

pub async fn info(cfg: &Config) -> AgentInfo {
    AgentInfo {
        hostname: std::env::var("HOSTNAME")
            .ok()
            .or_else(|| std::fs::read_to_string("/etc/hostname").ok())
            .map(|s| s.trim().to_string())
            .unwrap_or_default(),
        accel: cfg.accel.as_str().to_string(),
        jarvis_path: cfg.jarvis.to_string_lossy().to_string(),
        models_dir: cfg.models_dir().to_string_lossy().to_string(),
        docker_version: docker_version().await,
        agent_version: cfg.version.clone(),
        gpus: gpu_stats(cfg.accel).await,
    }
}
