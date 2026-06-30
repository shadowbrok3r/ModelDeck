//! Server functions: the only surface the UI calls. Each resolves a VM's agent
//! URL from its [VmTarget] record, then delegates to the agent or SurrealDB.

use dioxus::prelude::*;

use crate::model::*;

#[cfg(feature = "server")]
async fn db_ready() -> Result<(), ServerFnError> {
    crate::db::ensure_db_init().await.map_err(ServerFnError::new)
}

/// Resolve a VM id to its agent base URL.
#[cfg(feature = "server")]
async fn target_url(vm: &str) -> Result<String, ServerFnError> {
    db_ready().await?;
    let targets = crate::db::list_targets().await.map_err(ServerFnError::new)?;
    targets
        .into_iter()
        .find(|t| t.id == vm)
        .map(|t| t.agent_url)
        .filter(|u| !u.is_empty())
        .ok_or_else(|| ServerFnError::new(format!("no agent URL for VM '{vm}'")))
}

// --- VM targets -------------------------------------------------------------

#[server]
pub async fn list_vms() -> Result<Vec<VmTarget>, ServerFnError> {
    db_ready().await?;
    crate::db::list_targets().await.map_err(ServerFnError::new)
}

#[server]
pub async fn save_vm(target: VmTarget) -> Result<(), ServerFnError> {
    db_ready().await?;
    crate::db::upsert_target(&target).await.map_err(ServerFnError::new)
}

#[server]
pub async fn delete_vm(id: String) -> Result<(), ServerFnError> {
    db_ready().await?;
    crate::db::delete_target(&id).await.map_err(ServerFnError::new)
}

/// Live agent snapshot (hostname, docker, GPU). Errors become a string so the UI
/// can show a VM as offline without failing the whole page.
#[server]
pub async fn agent_info(vm: String) -> Result<AgentInfo, ServerFnError> {
    let base = target_url(&vm).await?;
    crate::agent_client::info(&base).await.map_err(ServerFnError::new)
}

#[server]
pub async fn list_containers(vm: String) -> Result<Vec<ContainerStatus>, ServerFnError> {
    let base = target_url(&vm).await?;
    crate::agent_client::containers(&base).await.map_err(ServerFnError::new)
}

#[server]
pub async fn list_compose(vm: String) -> Result<Vec<ComposeProject>, ServerFnError> {
    let base = target_url(&vm).await?;
    crate::agent_client::compose_projects(&base).await.map_err(ServerFnError::new)
}

#[server]
pub async fn list_models(vm: String) -> Result<Vec<ModelFile>, ServerFnError> {
    let base = target_url(&vm).await?;
    let mut models = crate::agent_client::models(&base).await.map_err(ServerFnError::new)?;
    for m in &mut models {
        m.vm = vm.clone();
    }
    Ok(models)
}

#[server]
pub async fn read_file(vm: String, path: String) -> Result<FilePayload, ServerFnError> {
    let base = target_url(&vm).await?;
    crate::agent_client::read_file(&base, &path).await.map_err(ServerFnError::new)
}

#[server]
pub async fn write_file(vm: String, path: String, content: String) -> Result<(), ServerFnError> {
    let base = target_url(&vm).await?;
    crate::agent_client::write_file(&base, &FilePayload { path, content })
        .await
        .map_err(ServerFnError::new)
}

#[server]
pub async fn restart_container(vm: String, container: String) -> Result<(), ServerFnError> {
    let base = target_url(&vm).await?;
    crate::agent_client::restart(&base, &container).await.map_err(ServerFnError::new)
}

#[server]
pub async fn compose_up(vm: String, file: String, project: Option<String>) -> Result<(), ServerFnError> {
    let base = target_url(&vm).await?;
    crate::agent_client::compose_up(&base, &file, project.as_deref())
        .await
        .map_err(ServerFnError::new)
}

#[server]
pub async fn compose_down(vm: String, project: String) -> Result<(), ServerFnError> {
    let base = target_url(&vm).await?;
    crate::agent_client::compose_down(&base, &project).await.map_err(ServerFnError::new)
}

#[server]
pub async fn container_logs(vm: String, container: String, tail: u32) -> Result<String, ServerFnError> {
    let base = target_url(&vm).await?;
    crate::agent_client::logs(&base, &container, tail).await.map_err(ServerFnError::new)
}

#[server]
pub async fn delete_model(vm: String, path: String) -> Result<(), ServerFnError> {
    let base = target_url(&vm).await?;
    crate::agent_client::delete_model(&base, &path).await.map_err(ServerFnError::new)
}

/// Begin a HuggingFace download on a VM. The HF token is read from the add-on
/// option (HF_TOKEN) server-side, never sent from the browser. Returns once the
/// transfer has started; progress streams into the Logs panel.
#[server]
pub async fn start_download(
    vm: String,
    repo: String,
    file: Option<String>,
    dest: Option<String>,
) -> Result<(), ServerFnError> {
    let base = target_url(&vm).await?;
    let hf_token = std::env::var("HF_TOKEN").ok().filter(|t| !t.is_empty());
    let req = DownloadRequest { repo, file, dest, hf_token };
    crate::agent_client::start_download(base, req);
    Ok(())
}

// --- profiles ---------------------------------------------------------------

#[server]
pub async fn list_profiles() -> Result<Vec<ServiceProfile>, ServerFnError> {
    db_ready().await?;
    crate::db::list_profiles().await.map_err(ServerFnError::new)
}

#[server]
pub async fn save_profile(profile: ServiceProfile) -> Result<String, ServerFnError> {
    db_ready().await?;
    crate::db::save_profile(&profile).await.map_err(ServerFnError::new)
}

#[server]
pub async fn delete_profile(id: String) -> Result<(), ServerFnError> {
    db_ready().await?;
    crate::db::delete_profile(&id).await.map_err(ServerFnError::new)
}

#[server]
pub async fn active_profile(vm: String) -> Result<Option<String>, ServerFnError> {
    db_ready().await?;
    crate::db::get_active(&vm).await.map_err(ServerFnError::new)
}

/// The swap: materialize the profile's tied files, write its managed compose, and
/// `up -d` under the stable swap project on its VM, replacing whatever ran before.
#[server]
pub async fn activate_profile(profile_id: String) -> Result<ActivateResult, ServerFnError> {
    db_ready().await?;
    let profile = crate::db::get_profile(&profile_id)
        .await
        .map_err(ServerFnError::new)?
        .ok_or_else(|| ServerFnError::new(format!("profile {profile_id} not found")))?;
    let base = target_url(&profile.vm).await?;
    let result = crate::agent_client::activate(&base, &profile)
        .await
        .map_err(ServerFnError::new)?;
    if result.ok {
        let _ = crate::db::set_active(&profile.vm, &profile_id).await;
        crate::ha::push_active(&profile.vm, &profile.name).await;
    }
    Ok(result)
}
