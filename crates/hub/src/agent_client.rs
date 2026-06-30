//! Server-only HTTP client the hub uses to talk to each VM's ModelDeck agent.
//! All agents share one bearer secret (MODELDECK_AGENT_SECRET); the per-VM base
//! URL comes from the [VmTarget] record.

use crate::model::*;
use serde::de::DeserializeOwned;
use serde::Serialize;

fn secret() -> String {
    std::env::var("MODELDECK_AGENT_SECRET")
        .ok()
        .or_else(|| option_env!("MODELDECK_AGENT_SECRET").map(|s| s.to_string()))
        .unwrap_or_default()
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(180))
        .build()
        .unwrap_or_default()
}

fn url(base: &str, path: &str) -> String {
    format!("{}/{}", base.trim_end_matches('/'), path.trim_start_matches('/'))
}

async fn get<T: DeserializeOwned>(base: &str, path: &str) -> Result<T, String> {
    let resp = client()
        .get(url(base, path))
        .bearer_auth(secret())
        .send()
        .await
        .map_err(|e| format!("{path}: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("{path}: HTTP {}", resp.status()));
    }
    resp.json::<T>().await.map_err(|e| format!("{path} decode: {e}"))
}

async fn post<B: Serialize, T: DeserializeOwned>(base: &str, path: &str, body: &B) -> Result<T, String> {
    let resp = client()
        .post(url(base, path))
        .bearer_auth(secret())
        .json(body)
        .send()
        .await
        .map_err(|e| format!("{path}: {e}"))?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(format!("{path}: HTTP {status}: {text}"));
    }
    serde_json::from_str::<T>(&text).map_err(|e| format!("{path} decode: {e} ({text})"))
}

// --- typed wrappers ---------------------------------------------------------

pub async fn info(base: &str) -> Result<AgentInfo, String> {
    get(base, "info").await
}
pub async fn gpu(base: &str) -> Result<Vec<GpuStats>, String> {
    get(base, "gpu").await
}
pub async fn containers(base: &str) -> Result<Vec<ContainerStatus>, String> {
    get(base, "containers").await
}
pub async fn compose_projects(base: &str) -> Result<Vec<ComposeProject>, String> {
    get(base, "compose").await
}
pub async fn models(base: &str) -> Result<Vec<ModelFile>, String> {
    get(base, "models").await
}

pub async fn read_file(base: &str, path: &str) -> Result<FilePayload, String> {
    get(base, &format!("file?path={}", urlencode(path))).await
}

pub async fn write_file(base: &str, payload: &FilePayload) -> Result<(), String> {
    let _: serde_json::Value = put(base, "file", payload).await?;
    Ok(())
}

async fn put<B: Serialize, T: DeserializeOwned>(base: &str, path: &str, body: &B) -> Result<T, String> {
    let resp = client()
        .put(url(base, path))
        .bearer_auth(secret())
        .json(body)
        .send()
        .await
        .map_err(|e| format!("{path}: {e}"))?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(format!("{path}: HTTP {status}: {text}"));
    }
    serde_json::from_str::<T>(&text).map_err(|e| format!("{path} decode: {e} ({text})"))
}

pub async fn logs(base: &str, container: &str, tail: u32) -> Result<String, String> {
    let p: FilePayload = get(base, &format!("logs?container={}&tail={tail}", urlencode(container))).await?;
    Ok(p.content)
}

pub async fn restart(base: &str, container: &str) -> Result<(), String> {
    let _: serde_json::Value =
        post(base, "compose/restart", &serde_json::json!({ "container": container })).await?;
    Ok(())
}

pub async fn compose_up(base: &str, file: &str, project: Option<&str>) -> Result<(), String> {
    let _: serde_json::Value =
        post(base, "compose/up", &serde_json::json!({ "file": file, "project": project })).await?;
    Ok(())
}

pub async fn compose_down(base: &str, project: &str) -> Result<(), String> {
    let _: serde_json::Value =
        post(base, "compose/down", &serde_json::json!({ "project": project })).await?;
    Ok(())
}

pub async fn delete_model(base: &str, path: &str) -> Result<(), String> {
    let _: serde_json::Value =
        post(base, "models/delete", &serde_json::json!({ "path": path })).await?;
    Ok(())
}

pub async fn activate(base: &str, profile: &ServiceProfile) -> Result<ActivateResult, String> {
    post(base, "activate", profile).await
}

/// Kick off a HuggingFace download and drain the agent's SSE progress into the
/// in-app log (visible in the Logs panel). Returns immediately; the stream is
/// drained on a background task.
pub fn start_download(base: String, req: DownloadRequest) {
    use futures_util::StreamExt;
    tokio::spawn(async move {
        crate::log::app_log("INFO", format!("Starting download: {}", req.repo));
        let resp = match client()
            .post(url(&base, "models/download"))
            .bearer_auth(secret())
            .json(&req)
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                crate::log::app_log("ERROR", format!("download {}: {e}", req.repo));
                return;
            }
        };
        let mut stream = resp.bytes_stream();
        let mut buf = String::new();
        while let Some(chunk) = stream.next().await {
            let Ok(bytes) = chunk else { break };
            buf.push_str(&String::from_utf8_lossy(&bytes));
            while let Some(nl) = buf.find('\n') {
                let line = buf[..nl].to_string();
                buf.drain(..=nl);
                if let Some(data) = line.strip_prefix("data:") {
                    let data = data.trim();
                    if !data.is_empty() {
                        crate::log::app_log("INFO", format!("[hf] {data}"));
                    }
                }
            }
        }
        crate::log::app_log("INFO", format!("Download stream ended: {}", req.repo));
    });
}

fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}
