//! Domain types shared between the ModelDeck hub (Dioxus UI + server fns), the
//! per-VM agent, and the SurrealDB layer.
//!
//! Everything is `Serialize`/`Deserialize` so it can cross the server-fn boundary
//! (wasm UI <-> hub server) and the agent's HTTP API. `SurrealValue` is derived
//! only under the `surreal` feature (enabled by the hub's server build) so the UI
//! and agent don't need the Surreal type system. Same pattern as OrderTracker's
//! `jewelry_shared`.

use serde::{Deserialize, Serialize};

#[cfg(feature = "surreal")]
use surrealdb_types::SurrealValue;

// ============================================================================
// Enums
// ============================================================================

/// GPU acceleration stack of a managed VM. Drives which engine images apply and
/// how the agent reads GPU stats (sysfs for ROCm, nvidia-smi for CUDA).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "surreal", derive(SurrealValue))]
#[serde(rename_all = "lowercase")]
pub enum AccelKind {
    Rocm,
    Cuda,
    Cpu,
}

impl AccelKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            AccelKind::Rocm => "rocm",
            AccelKind::Cuda => "cuda",
            AccelKind::Cpu => "cpu",
        }
    }
    pub fn from_str_loose(s: &str) -> AccelKind {
        match s.trim().to_lowercase().as_str() {
            "rocm" | "amd" | "hip" => AccelKind::Rocm,
            "cuda" | "nvidia" | "nv" => AccelKind::Cuda,
            _ => AccelKind::Cpu,
        }
    }
}

/// Inference engine a profile serves with. The hub uses this for icons/filters and
/// the agent uses it for engine-specific health probes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "surreal", derive(SurrealValue))]
#[serde(rename_all = "snake_case")]
pub enum EngineKind {
    #[default]
    LlamaCpp,
    Vllm,
    Ollama,
    TabbyApi,
    Custom,
}

impl EngineKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            EngineKind::LlamaCpp => "llama.cpp",
            EngineKind::Vllm => "vllm",
            EngineKind::Ollama => "ollama",
            EngineKind::TabbyApi => "tabbyAPI",
            EngineKind::Custom => "custom",
        }
    }
}

/// On-disk weight format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "surreal", derive(SurrealValue))]
#[serde(rename_all = "lowercase")]
pub enum ModelFormat {
    Gguf,
    Safetensors,
    /// A HuggingFace repo cached under hf-cache/ (a directory, not a single file).
    HfRepo,
    Other,
}

/// Role of a small text file bound to a profile (materialized into the per-model
/// directory on activation). Binary siblings (mmproj, draft weights) are large and
/// already live under models/, so they are referenced by path in `compose_fragment`
/// rather than carried as tied files.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "surreal", derive(SurrealValue))]
#[serde(rename_all = "snake_case")]
pub enum TiedFileRole {
    ChatTemplate,
    Grammar,
    /// Arbitrary engine config (json/yaml/toml).
    Config,
    Other,
}

impl TiedFileRole {
    pub fn as_str(&self) -> &'static str {
        match self {
            TiedFileRole::ChatTemplate => "chat_template",
            TiedFileRole::Grammar => "grammar",
            TiedFileRole::Config => "config",
            TiedFileRole::Other => "other",
        }
    }
}

// ============================================================================
// Core records (persisted in SurrealDB ns:jarvis db:modeldeck)
// ============================================================================

/// A managed AI VM that runs a ModelDeck agent. The agent's bearer secret is NOT
/// persisted here — the hub matches it by `id` from its own env/options so the DB
/// never holds credentials.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "surreal", derive(SurrealValue))]
pub struct VmTarget {
    /// Stable slug, e.g. "amd" / "nvidia". Also the SurrealDB record key.
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub accel: Option<String>,
    /// Base URL of this VM's agent, e.g. "http://AGENT_IP:9777".
    pub agent_url: String,
    /// Where the jarvis tree lives on the VM, e.g. "/opt/jarvis".
    #[serde(default)]
    pub jarvis_path: String,
    /// GPU arch tag for display, e.g. "gfx1201" / "sm_86".
    #[serde(default)]
    pub gpu_arch: Option<String>,
    #[serde(default)]
    pub online: bool,
}

/// A small text file bound to a [ServiceProfile]. On activation the agent writes
/// `~/jarvis/services/<model_dir_name>/<filename>` with `content`, so a profile's
/// custom chat template can never be used by another model by accident.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "surreal", derive(SurrealValue))]
pub struct TiedFile {
    pub role: TiedFileRole,
    /// Bare filename, e.g. "a3btemplate.jinja". Path-free (validated on the agent).
    pub filename: String,
    pub content: String,
}

/// A saved, known-good inference service. This is the formalized version of the
/// commented-out `services:` blocks in the hand-edited docker-compose.yml: one
/// container that serves one model, plus the files it depends on.
///
/// The source of truth is `compose_fragment` (the YAML for this single service);
/// the structured fields are extracted for display, filtering, and swap logic and
/// are kept in sync when the fragment is edited.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "surreal", derive(SurrealValue))]
pub struct ServiceProfile {
    /// SurrealDB record key (empty when creating).
    #[serde(default)]
    pub id: String,
    pub name: String,
    /// [VmTarget::id] this profile targets (engine images are VM-specific).
    pub vm: String,
    pub engine: EngineKind,
    /// The model the service serves: a `/models/*.gguf` path or an HF repo id.
    pub model_ref: String,
    /// Directory name (the model's filename) under `services/` that holds this
    /// profile's tied files, e.g. "Huihui-Qwen3.6-35B-A3B-abliterated-...-Q4_K.gguf".
    pub model_dir_name: String,
    /// Compose service key, e.g. "llama".
    pub service_name: String,
    #[serde(default)]
    pub container_name: String,
    /// Host port the service publishes (for health checks + quick links).
    #[serde(default)]
    pub host_port: Option<u16>,
    /// YAML for this single compose service (the proven config).
    pub compose_fragment: String,
    /// If the engine builds from a Dockerfile, its name (e.g. "Dockerfile.llama").
    #[serde(default)]
    pub dockerfile_name: Option<String>,
    /// Contents of that Dockerfile, tied to the profile so a build recipe travels
    /// with the service.
    #[serde(default)]
    pub dockerfile: Option<String>,
    #[serde(default)]
    pub tied_files: Vec<TiedFile>,
    #[serde(default)]
    pub notes: Option<String>,
    /// Marked by the user once a config is confirmed working.
    #[serde(default)]
    pub known_good: bool,
    #[serde(default)]
    pub created_at: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default)]
    pub updated_at: Option<chrono::DateTime<chrono::Utc>>,
}

// ============================================================================
// Agent API DTOs (hub <-> agent over HTTP)
// ============================================================================

/// Snapshot returned by the agent's `GET /info`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct AgentInfo {
    pub hostname: String,
    pub accel: String,
    pub jarvis_path: String,
    pub models_dir: String,
    pub docker_version: String,
    pub agent_version: String,
    #[serde(default)]
    pub gpus: Vec<GpuStats>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct GpuStats {
    pub index: u32,
    pub name: String,
    pub mem_total_mb: u64,
    pub mem_used_mb: u64,
    #[serde(default)]
    pub util_pct: Option<f32>,
    #[serde(default)]
    pub temp_c: Option<f32>,
}

/// One docker container as seen by `docker ps` (compose labels parsed out).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ContainerStatus {
    pub name: String,
    pub image: String,
    pub state: String,
    pub status: String,
    #[serde(default)]
    pub ports: Vec<String>,
    #[serde(default)]
    pub compose_project: Option<String>,
    #[serde(default)]
    pub compose_service: Option<String>,
    #[serde(default)]
    pub health: Option<String>,
}

/// A docker-compose project on a VM (from `docker compose ls`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ComposeProject {
    pub name: String,
    pub status: String,
    #[serde(default)]
    pub config_files: Vec<String>,
}

/// A file the editor reads/writes on a VM (compose file, Dockerfile, template).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct FilePayload {
    pub path: String,
    pub content: String,
}

/// A weight/model discovered under models/ or hf-cache/.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ModelFile {
    pub vm: String,
    pub path: String,
    pub filename: String,
    pub size_bytes: u64,
    pub format: Option<ModelFormat>,
    #[serde(default)]
    pub quant: Option<String>,
    #[serde(default)]
    pub modified: Option<chrono::DateTime<chrono::Utc>>,
}

/// Request to start a HuggingFace download on a VM. `hf_token` is passed per-call
/// (sourced from the HA add-on option) rather than stored.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct DownloadRequest {
    /// HF repo id, e.g. "unsloth/Qwen3-30B-A3B-GGUF".
    pub repo: String,
    /// Optional single file to pull (glob ok), e.g. "*Q4_K_M.gguf".
    #[serde(default)]
    pub file: Option<String>,
    /// Destination subdir under models/ (defaults to the repo's basename).
    #[serde(default)]
    pub dest: Option<String>,
    #[serde(default)]
    pub hf_token: Option<String>,
}

/// Result of activating a profile (swap + restart + health probe).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ActivateResult {
    pub ok: bool,
    pub message: String,
    #[serde(default)]
    pub health_ok: bool,
    #[serde(default)]
    pub log_tail: Vec<String>,
}

// ============================================================================
// Helpers
// ============================================================================

/// Lowercase, filesystem/url-safe slug from a display name.
pub fn slugify(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_dash = false;
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash && !out.is_empty() {
            out.push('-');
            prev_dash = true;
        }
    }
    out.trim_matches('-').to_string()
}

/// Reject path traversal / separators in a tied-file or model-dir name. The agent
/// MUST call this before writing into `services/<dir>/<filename>`.
pub fn is_safe_segment(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.contains('/')
        && !name.contains('\\')
        && !name.contains("..")
        && name.bytes().all(|b| {
            b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'+' | b'=')
        })
}

/// Best-effort quantization tag parsed from a GGUF filename
/// (e.g. "...-Q4_K.gguf" -> "Q4_K", "...i1-Q4_0.gguf" -> "Q4_0").
pub fn parse_quant(filename: &str) -> Option<String> {
    let upper = filename.to_uppercase();
    // Scan dash/dot separated tokens for a quant-looking token.
    for tok in upper.split(|c| c == '-' || c == '.' || c == '_') {
        let t = tok.trim();
        if (t.starts_with('Q') || t.starts_with("IQ"))
            && t.chars().any(|c| c.is_ascii_digit())
            && t.len() <= 8
        {
            return Some(t.to_string());
        }
        if matches!(t, "F16" | "F32" | "BF16" | "FP8" | "INT4" | "INT8" | "AWQ") {
            return Some(t.to_string());
        }
    }
    None
}

/// Infer format from a path.
pub fn infer_format(path: &str) -> ModelFormat {
    let p = path.to_lowercase();
    if p.ends_with(".gguf") {
        ModelFormat::Gguf
    } else if p.ends_with(".safetensors") {
        ModelFormat::Safetensors
    } else if p.contains("/hub/models--") || p.contains("hf-cache") {
        ModelFormat::HfRepo
    } else {
        ModelFormat::Other
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quant_parsing() {
        assert_eq!(
            parse_quant("Huihui-Qwen3.6-35B-A3B-abliterated-ggml-model-Q4_K.gguf").as_deref(),
            Some("Q4_K")
        );
        assert_eq!(
            parse_quant("Llama-3_3-Nemotron-Super-49B-v1.i1-Q4_0.gguf").as_deref(),
            Some("Q4_0")
        );
        assert_eq!(parse_quant("mmproj-F32.gguf").as_deref(), Some("F32"));
    }

    #[test]
    fn safe_segments() {
        assert!(is_safe_segment("a3btemplate.jinja"));
        assert!(is_safe_segment("Huihui-Qwen3.6-35B-A3B-Q4_K.gguf"));
        assert!(!is_safe_segment("../etc/passwd"));
        assert!(!is_safe_segment("a/b"));
        assert!(!is_safe_segment(".."));
    }
}
