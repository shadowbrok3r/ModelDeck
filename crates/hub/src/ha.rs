//! Push ModelDeck state to Home Assistant via the Supervisor proxy
//! (`http://supervisor/core/api`, authed with SUPERVISOR_TOKEN). Server-only.
//! No-op when not running under the Supervisor.

fn supervisor() -> Option<(String, reqwest::Client)> {
    let token = std::env::var("SUPERVISOR_TOKEN").ok().filter(|t| !t.is_empty())?;
    Some((token, reqwest::Client::new()))
}

async fn push_state(entity: &str, state: String, attributes: serde_json::Value) {
    let Some((token, client)) = supervisor() else { return };
    let url = format!("http://supervisor/core/api/states/{entity}");
    let body = serde_json::json!({ "state": state, "attributes": attributes });
    if let Err(e) = client.post(&url).bearer_auth(token).json(&body).send().await {
        crate::log::app_log("INFO", format!("HA push {entity} failed: {e}"));
    }
}

/// Reflect the active profile for a VM as a sensor (e.g. sensor.modeldeck_amd_active).
pub async fn push_active(vm: &str, profile_name: &str) {
    let entity = format!("sensor.modeldeck_{}_active", crate::model::slugify(vm).replace('-', "_"));
    push_state(
        &entity,
        profile_name.to_string(),
        serde_json::json!({
            "friendly_name": format!("ModelDeck {vm} active model"),
            "icon": "mdi:robot",
            "vm": vm,
        }),
    )
    .await;
}

/// Reflect GPU VRAM use for a VM (sensor.modeldeck_<vm>_vram), percent used.
pub async fn push_gpu(vm: &str, gpus: &[crate::model::GpuStats]) {
    let Some(g) = gpus.first() else { return };
    let pct = if g.mem_total_mb > 0 {
        (g.mem_used_mb as f64 / g.mem_total_mb as f64 * 100.0).round()
    } else {
        0.0
    };
    let entity = format!("sensor.modeldeck_{}_vram", crate::model::slugify(vm).replace('-', "_"));
    push_state(
        &entity,
        format!("{pct:.0}"),
        serde_json::json!({
            "friendly_name": format!("ModelDeck {vm} VRAM"),
            "unit_of_measurement": "%",
            "state_class": "measurement",
            "icon": "mdi:memory",
            "gpu": g.name,
            "mem_used_mb": g.mem_used_mb,
            "mem_total_mb": g.mem_total_mb,
            "util_pct": g.util_pct,
        }),
    )
    .await;
}
