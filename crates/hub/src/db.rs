//! SurrealDB layer (server-only). Connects to the shared instance at SURREAL_URL
//! using ns:jarvis db:modeldeck, keeping ModelDeck's data alongside the other
//! home-assistant apps in one database. Mirrors OrderTracker's connection pattern.
//!
//! Storage uses local `SurrealValue` row structs (scalars only) rather than
//! deriving `SurrealValue` on the shared enums, so persistence never depends on
//! Surreal's enum representation.

use std::sync::LazyLock;

use surrealdb::engine::remote::ws::{Client, Ws, Wss};
use surrealdb::opt::auth::Root;
use surrealdb::Surreal;
use surrealdb_types::SurrealValue;

use crate::model::{ServiceProfile, TiedFile, TiedFileRole, VmTarget};

const NS: &str = "jarvis";
const DB_NAME: &str = "modeldeck";

pub static DB: LazyLock<Surreal<Client>> = LazyLock::new(Surreal::init);
static DB_INIT: tokio::sync::OnceCell<()> = tokio::sync::OnceCell::const_new();

/// Connect the singleton exactly once (no-op on subsequent calls).
pub async fn ensure_db_init() -> Result<(), String> {
    DB_INIT
        .get_or_try_init(|| async {
            let url = std::env::var("SURREAL_URL").map_err(|_| "SURREAL_URL not set".to_string())?;
            let url = url.trim().to_string();
            if url.is_empty() {
                return Err("SURREAL_URL is empty".to_string());
            }
            let connect = if let Some(host) = url.strip_prefix("wss://") {
                DB.connect::<Wss>(host).await
            } else if let Some(host) = url.strip_prefix("ws://") {
                DB.connect::<Ws>(host).await
            } else {
                DB.connect::<Ws>(url.as_str()).await
            };
            connect.map_err(|e| e.to_string())?;
            if let (Ok(user), Ok(pass)) =
                (std::env::var("SURREAL_USER"), std::env::var("SURREAL_PASS"))
            {
                if !user.is_empty() {
                    DB.signin(Root { username: user, password: pass })
                        .await
                        .map_err(|e| e.to_string())?;
                }
            }
            DB.use_ns(NS).use_db(DB_NAME).await.map_err(|e| e.to_string())?;
            // Self-provision the tables. This instance rejects SELECT on an
            // undefined table, so define them up front (idempotent).
            DB.query(
                "DEFINE TABLE IF NOT EXISTS vm_target SCHEMALESS; \
                 DEFINE TABLE IF NOT EXISTS profile SCHEMALESS; \
                 DEFINE TABLE IF NOT EXISTS active SCHEMALESS; \
                 DEFINE INDEX IF NOT EXISTS profile_vm ON profile FIELDS vm;",
            )
            .await
            .map_err(|e| e.to_string())?;
            eprintln!("ModelDeck connected to SurrealDB {url} (ns:{NS} db:{DB_NAME})");
            Ok(())
        })
        .await
        .map(|_| ())
}

// ---------------------------------------------------------------------------
// VM targets
// ---------------------------------------------------------------------------

#[derive(SurrealValue)]
struct VmTargetRead {
    rid: String,
    name: String,
    accel: Option<String>,
    agent_url: String,
    jarvis_path: Option<String>,
    gpu_arch: Option<String>,
}

pub async fn list_targets() -> Result<Vec<VmTarget>, String> {
    let mut res = DB
        .query("SELECT <string>id AS rid, name, accel, agent_url, jarvis_path, gpu_arch FROM vm_target ORDER BY name")
        .await
        .map_err(|e| e.to_string())?;
    let rows: Vec<VmTargetRead> = res.take(0).map_err(|e| e.to_string())?;
    Ok(rows
        .into_iter()
        .map(|r| VmTarget {
            id: r.rid.strip_prefix("vm_target:").unwrap_or(&r.rid).trim_matches('⟨').trim_matches('⟩').to_string(),
            name: r.name,
            accel: r.accel,
            agent_url: r.agent_url,
            jarvis_path: r.jarvis_path.unwrap_or_default(),
            gpu_arch: r.gpu_arch,
            online: false,
        })
        .collect())
}

pub async fn upsert_target(t: &VmTarget) -> Result<(), String> {
    DB.query(
        "UPSERT type::record('vm_target', $id) SET name=$name, accel=$accel, agent_url=$url, jarvis_path=$jp, gpu_arch=$arch",
    )
    .bind(("id", t.id.clone()))
    .bind(("name", t.name.clone()))
    .bind(("accel", t.accel.clone()))
    .bind(("url", t.agent_url.clone()))
    .bind(("jp", t.jarvis_path.clone()))
    .bind(("arch", t.gpu_arch.clone()))
    .await
    .map_err(|e| e.to_string())?
    .check()
    .map_err(|e| e.to_string())?;
    Ok(())
}

pub async fn delete_target(id: &str) -> Result<(), String> {
    DB.query("DELETE type::record('vm_target', $id)")
        .bind(("id", id.to_string()))
        .await
        .map_err(|e| e.to_string())?
        .check()
        .map_err(|e| e.to_string())?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Service profiles
// ---------------------------------------------------------------------------

#[derive(SurrealValue)]
struct TiedRow {
    role: String,
    filename: String,
    content: String,
}

#[derive(SurrealValue)]
struct ProfileWrite {
    name: String,
    vm: String,
    engine: String,
    model_ref: String,
    model_dir_name: String,
    service_name: String,
    container_name: String,
    host_port: Option<i64>,
    compose_fragment: String,
    dockerfile_name: Option<String>,
    dockerfile: Option<String>,
    tied_files: Vec<TiedRow>,
    notes: Option<String>,
    known_good: bool,
    created_at: chrono::DateTime<chrono::Utc>,
    updated_at: chrono::DateTime<chrono::Utc>,
}

#[derive(SurrealValue)]
struct ProfileRead {
    rid: String,
    name: String,
    vm: String,
    engine: String,
    model_ref: String,
    model_dir_name: String,
    service_name: String,
    container_name: String,
    host_port: Option<i64>,
    compose_fragment: String,
    dockerfile_name: Option<String>,
    dockerfile: Option<String>,
    tied_files: Vec<TiedRow>,
    notes: Option<String>,
    known_good: bool,
    created_at: Option<chrono::DateTime<chrono::Utc>>,
    updated_at: Option<chrono::DateTime<chrono::Utc>>,
}

fn role_str(r: &TiedFileRole) -> String {
    r.as_str().to_string()
}
fn role_from(s: &str) -> TiedFileRole {
    match s {
        "chat_template" => TiedFileRole::ChatTemplate,
        "grammar" => TiedFileRole::Grammar,
        "config" => TiedFileRole::Config,
        _ => TiedFileRole::Other,
    }
}

fn to_write(p: &ServiceProfile) -> ProfileWrite {
    let now = chrono::Utc::now();
    ProfileWrite {
        name: p.name.clone(),
        vm: p.vm.clone(),
        engine: p.engine.as_str().to_string(),
        model_ref: p.model_ref.clone(),
        model_dir_name: p.model_dir_name.clone(),
        service_name: p.service_name.clone(),
        container_name: p.container_name.clone(),
        host_port: p.host_port.map(|x| x as i64),
        compose_fragment: p.compose_fragment.clone(),
        dockerfile_name: p.dockerfile_name.clone(),
        dockerfile: p.dockerfile.clone(),
        tied_files: p
            .tied_files
            .iter()
            .map(|t| TiedRow {
                role: role_str(&t.role),
                filename: t.filename.clone(),
                content: t.content.clone(),
            })
            .collect(),
        notes: p.notes.clone(),
        known_good: p.known_good,
        created_at: p.created_at.unwrap_or(now),
        updated_at: now,
    }
}

fn from_read(r: ProfileRead) -> ServiceProfile {
    ServiceProfile {
        id: r.rid.strip_prefix("profile:").unwrap_or(&r.rid).trim_matches('⟨').trim_matches('⟩').to_string(),
        name: r.name,
        vm: r.vm,
        engine: crate::model::engine_from_label(&r.engine),
        model_ref: r.model_ref,
        model_dir_name: r.model_dir_name,
        service_name: r.service_name,
        container_name: r.container_name,
        host_port: r.host_port.map(|x| x as u16),
        compose_fragment: r.compose_fragment,
        dockerfile_name: r.dockerfile_name,
        dockerfile: r.dockerfile,
        tied_files: r
            .tied_files
            .into_iter()
            .map(|t| TiedFile {
                role: role_from(&t.role),
                filename: t.filename,
                content: t.content,
            })
            .collect(),
        notes: r.notes,
        known_good: r.known_good,
        created_at: r.created_at,
        updated_at: r.updated_at,
    }
}

const PROFILE_COLS: &str = "<string>id AS rid, name, vm, engine, model_ref, model_dir_name, \
    service_name, container_name, host_port, compose_fragment, dockerfile_name, dockerfile, \
    tied_files, notes, known_good, created_at, updated_at";

pub async fn list_profiles() -> Result<Vec<ServiceProfile>, String> {
    let q = format!("SELECT {PROFILE_COLS} FROM profile ORDER BY name");
    let mut res = DB.query(q).await.map_err(|e| e.to_string())?;
    let rows: Vec<ProfileRead> = res.take(0).map_err(|e| e.to_string())?;
    Ok(rows.into_iter().map(from_read).collect())
}

/// Create (empty id) or update an existing profile; returns the stored id.
pub async fn save_profile(p: &ServiceProfile) -> Result<String, String> {
    let w = to_write(p);
    if p.id.trim().is_empty() {
        let mut res = DB
            .query("CREATE profile CONTENT $p RETURN VALUE <string>id")
            .bind(("p", w))
            .await
            .map_err(|e| e.to_string())?;
        let ids: Vec<String> = res.take(0).map_err(|e| e.to_string())?;
        let rid = ids.into_iter().next().unwrap_or_default();
        Ok(rid.strip_prefix("profile:").unwrap_or(&rid).trim_matches('⟨').trim_matches('⟩').to_string())
    } else {
        DB.query("UPDATE type::record('profile', $key) CONTENT $p")
            .bind(("key", p.id.clone()))
            .bind(("p", w))
            .await
            .map_err(|e| e.to_string())?
            .check()
            .map_err(|e| e.to_string())?;
        Ok(p.id.clone())
    }
}

pub async fn get_profile(id: &str) -> Result<Option<ServiceProfile>, String> {
    let q = format!("SELECT {PROFILE_COLS} FROM type::record('profile', $key)");
    let mut res = DB
        .query(q)
        .bind(("key", id.to_string()))
        .await
        .map_err(|e| e.to_string())?;
    let rows: Vec<ProfileRead> = res.take(0).map_err(|e| e.to_string())?;
    Ok(rows.into_iter().next().map(from_read))
}

pub async fn delete_profile(id: &str) -> Result<(), String> {
    DB.query("DELETE type::record('profile', $key)")
        .bind(("key", id.to_string()))
        .await
        .map_err(|e| e.to_string())?
        .check()
        .map_err(|e| e.to_string())?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Active-profile pointer (per VM)
// ---------------------------------------------------------------------------

pub async fn set_active(vm: &str, profile_id: &str) -> Result<(), String> {
    DB.query("UPSERT type::record('active', $vm) SET profile_id=$pid, at=time::now()")
        .bind(("vm", vm.to_string()))
        .bind(("pid", profile_id.to_string()))
        .await
        .map_err(|e| e.to_string())?
        .check()
        .map_err(|e| e.to_string())?;
    Ok(())
}

pub async fn get_active(vm: &str) -> Result<Option<String>, String> {
    let mut res = DB
        .query("SELECT VALUE profile_id FROM type::record('active', $vm)")
        .bind(("vm", vm.to_string()))
        .await
        .map_err(|e| e.to_string())?;
    let ids: Vec<String> = res.take(0).map_err(|e| e.to_string())?;
    Ok(ids.into_iter().next())
}
