#![allow(non_snake_case)]

mod api;
mod components;
#[cfg(feature = "server")]
mod agent_client;
#[cfg(feature = "server")]
mod db;
#[cfg(feature = "server")]
mod ha;
mod log;
mod model;

use dioxus::prelude::*;

use components::dialog::{DialogContent, DialogRoot, DialogTitle};
use components::FilterButton;
use log::{app_log, app_logs_snapshot, LogEntry};
use model::*;

#[derive(Debug, Clone, Copy, PartialEq)]
enum MainView {
    Models,
    Profiles,
    Files,
    System,
}

fn main() {
    #[cfg(feature = "server")]
    {
        server_main();
        return;
    }

    #[cfg(all(not(feature = "server"), target_arch = "wasm32"))]
    init_ha_ingress_server_url_for_fullstack();

    #[cfg(not(feature = "server"))]
    dioxus::launch(App);
}

// Server entry: tokio runtime, tracing, HA ingress middleware, GPU sensor loop.
#[cfg(feature = "server")]
fn server_main() {
    use dioxus::server::axum::{
        self,
        body::{to_bytes, Body},
        extract::Request,
        http::{
            header::{ACCEPT_ENCODING, CONTENT_ENCODING, CONTENT_LENGTH, CONTENT_TYPE},
            Uri,
        },
        middleware::{self, Next},
        response::Response,
        Router, ServiceExt,
    };
    use dioxus::server::{DioxusRouterExt, ServeConfig};
    use tower::Layer;
    use tower_http::trace::TraceLayer;
    use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

    dotenvy::dotenv().ok();
    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,tower_http=debug")))
        .with(tracing_subscriber::fmt::layer())
        .init();

    async fn strip_ingress_prefix(mut req: Request, next: Next) -> Response {
        req.headers_mut().remove(ACCEPT_ENCODING);
        if let Some(prefix) = req
            .headers()
            .get("x-ingress-path")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.trim_end_matches('/').to_string())
            .filter(|s| !s.is_empty())
        {
            let path = req.uri().path();
            if let Some(rest) = path.strip_prefix(&prefix) {
                let rest = if rest.is_empty() { "/" } else { rest };
                let pq = match req.uri().query() {
                    Some(q) => format!("{rest}?{q}"),
                    None => rest.to_string(),
                };
                if let Ok(uri) = pq.parse::<Uri>() {
                    *req.uri_mut() = uri;
                }
            }
        }
        next.run(req).await
    }

    async fn rewrite_ingress_assets(req: Request, next: Next) -> Response {
        let ingress = req
            .headers()
            .get("x-ingress-path")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.trim_end_matches('/').to_string())
            .filter(|s| !s.is_empty());
        let path = req.uri().path().to_string();
        let res = next.run(req).await;
        let Some(ingress) = ingress else { return res };
        if res.headers().contains_key(CONTENT_ENCODING) {
            return res;
        }
        let ctype = res
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let is_js = path.ends_with(".js");
        let is_css = path.ends_with(".css");
        let is_html = !is_js && !is_css && ctype.starts_with("text/html");
        if !(is_html || is_js || is_css) {
            return res;
        }
        let (mut parts, body) = res.into_parts();
        let bytes = match to_bytes(body, usize::MAX).await {
            Ok(b) => b,
            Err(_) => return Response::from_parts(parts, Body::empty()),
        };
        let text = String::from_utf8_lossy(&bytes);
        let rewritten = if is_html {
            text.replace("=\"/api/", &format!("=\"{ingress}/api/"))
                .replace("=\"/./assets/", &format!("=\"{ingress}/assets/"))
                .replace("=\"/assets/", &format!("=\"{ingress}/assets/"))
                .replacen("<head>", &format!("<head><base href=\"{ingress}/\">"), 1)
        } else {
            text.replace("/./assets/", &format!("{ingress}/assets/"))
                .replace("\"/assets/", &format!("\"{ingress}/assets/"))
                .replace("'/assets/", &format!("'{ingress}/assets/"))
                .replace("(/assets/", &format!("({ingress}/assets/"))
        };
        parts.headers.remove(CONTENT_LENGTH);
        Response::from_parts(parts, Body::from(rewritten))
    }

    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime")
        .block_on(async {
            let addr = dioxus::cli_config::fullstack_address_or_localhost();

            // Periodically reflect each VM's GPU usage into HA sensors.
            tokio::spawn(async {
                tokio::time::sleep(std::time::Duration::from_secs(20)).await;
                loop {
                    if db::ensure_db_init().await.is_ok() {
                        if let Ok(vms) = db::list_targets().await {
                            for vm in vms {
                                if let Ok(g) = agent_client::gpu(&vm.agent_url).await {
                                    ha::push_gpu(&vm.id, &g).await;
                                }
                            }
                        }
                    }
                    tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                }
            });

            let app: Router = Router::new()
                .serve_dioxus_application(ServeConfig::new(), App)
                .layer(middleware::from_fn(rewrite_ingress_assets))
                .layer(TraceLayer::new_for_http());
            let app = middleware::from_fn(strip_ingress_prefix).layer(app);

            let listener = tokio::net::TcpListener::bind(addr)
                .await
                .unwrap_or_else(|e| panic!("bind {addr}: {e}"));
            tracing::info!(%addr, "ModelDeck hub listening");
            axum::serve(listener, app.into_make_service()).await.expect("axum error");
        });
}

#[cfg(target_arch = "wasm32")]
fn init_ha_ingress_server_url_for_fullstack() {
    use dioxus::fullstack::set_server_url;
    let Some(window) = web_sys::window() else { return };
    let Ok(pathname) = window.location().pathname() else { return };
    let Ok(origin) = window.location().origin() else { return };
    let segments: Vec<&str> = pathname.split('/').filter(|s| !s.is_empty()).collect();
    if segments.len() >= 3 && segments[0] == "api" && segments[1] == "hassio_ingress" {
        let token = segments[2];
        let base = format!("{origin}/api/hassio_ingress/{token}");
        let leaked: &'static str = Box::leak(base.into_boxed_str());
        set_server_url(leaked);
    }
}

// ============================================================================
// Root component
// ============================================================================

#[component]
fn App() -> Element {
    let mut vms = use_signal(Vec::<VmTarget>::new);
    let mut vm = use_signal(String::new);
    let mut view = use_signal(|| MainView::Models);
    let mut settings_open = use_signal(|| false);
    let mut logs_open = use_signal(|| false);
    let mut log_snapshot = use_signal(Vec::<LogEntry>::new);
    let draft = use_signal(|| None::<ServiceProfile>);

    // Load VM targets once; preselect the first.
    use_effect(move || {
        spawn(async move {
            match api::list_vms().await {
                Ok(list) => {
                    if vm.read().is_empty() {
                        if let Some(first) = list.first() {
                            vm.set(first.id.clone());
                        }
                    }
                    vms.set(list);
                }
                Err(e) => app_log("ERROR", format!("list VMs: {e}")),
            }
        });
    });

    let current = use_memo(move || {
        let id = vm.read().clone();
        vms.read().iter().find(|t| t.id == id).cloned()
    });

    rsx! {
        document::Stylesheet { href: asset!("/assets/styles.css") }
        document::Stylesheet { href: asset!("/assets/dx-components-theme.css") }
        document::Stylesheet { href: asset!("/assets/dialog.css") }

        div { class: "bg-galaxy min-h-screen",
            nav { class: "nav-galaxy px-6 py-4",
                div { class: "container flex items-center justify-between flex-wrap gap-3",
                    div { class: "flex items-center gap-4 flex-wrap",
                        h1 { class: "text-2xl font-bold text-star-white", "ModelDeck" }
                        div { class: "flex items-center gap-2",
                            for t in vms.read().iter().cloned() {
                                FilterButton {
                                    label: t.name.clone(),
                                    active: *vm.read() == t.id,
                                    onclick: move |_| vm.set(t.id.clone()),
                                }
                            }
                            {if vms.read().is_empty() {
                                rsx! { span { class: "text-stardust text-sm", "No VMs — open Settings to add one" } }
                            } else { rsx!{} }}
                        }
                    }
                    div { class: "flex items-center gap-2 flex-wrap",
                        FilterButton { label: "Models", active: *view.read() == MainView::Models, onclick: move |_| view.set(MainView::Models) }
                        FilterButton { label: "Profiles", active: *view.read() == MainView::Profiles, onclick: move |_| view.set(MainView::Profiles) }
                        FilterButton { label: "Files", active: *view.read() == MainView::Files, onclick: move |_| view.set(MainView::Files) }
                        FilterButton { label: "System", active: *view.read() == MainView::System, onclick: move |_| view.set(MainView::System) }
                        button { class: "btn-cosmic", onclick: move |_| settings_open.set(true), "Settings" }
                        button {
                            class: "btn-cosmic",
                            onclick: move |_| { logs_open.set(true); log_snapshot.set(app_logs_snapshot()); },
                            "Logs"
                        }
                    }
                }
            }

            div { class: "container px-6 py-6",
                // Don't mount the data views (which fetch per-VM) until a VM is
                // selected, or they fire server fns with an empty vm id on first paint.
                {if vm.read().is_empty() {
                    rsx! { div { class: "card-cosmic p-8 text-center text-stardust",
                        "No VM selected. Add a VM target in Settings, then pick it above." } }
                } else {
                    match *view.read() {
                        MainView::Models => rsx! { ModelsView { vm } },
                        MainView::Profiles => rsx! { ProfilesView { vm, vms, draft } },
                        MainView::Files => rsx! { FilesView { vm } },
                        MainView::System => rsx! { SystemView { vm } },
                    }
                }}
            }

            // Profile editor
            {if draft.read().is_some() {
                rsx! { ProfileEditor { vm, vms, draft } }
            } else { rsx!{} }}

            // Settings (VM targets)
            {if *settings_open.read() {
                rsx! { SettingsDialog { vms, on_close: move |_| settings_open.set(false) } }
            } else { rsx!{} }}

            // Logs
            DialogRoot {
                open: *logs_open.read(),
                on_open_change: move |o: bool| logs_open.set(o),
                DialogContent { class: "flex flex-col max-h-[85vh]",
                    DialogTitle { "Logs" }
                    p { class: "text-stardust text-sm", "App + download activity. Re-open to refresh." }
                    div { class: "flex-1 overflow-y-auto font-mono text-xs bg-nebula-dark rounded-lg p-3 border border-nebula-purple/30 min-h-[240px]",
                        for entry in log_snapshot.read().iter() {
                            div { class: "log-line py-0.5",
                                span { class: "text-stardust mr-2", "{entry.time}" }
                                span { class: if entry.level == "ERROR" { "text-warning-red font-semibold" } else { "text-aurora-purple" }, "{entry.level}" }
                                span { class: "text-moonlight ml-2", "{entry.message}" }
                            }
                        }
                    }
                    div { class: "flex gap-2 mt-4",
                        button { class: "btn-cosmic", onclick: move |_| log_snapshot.set(app_logs_snapshot()), "Refresh" }
                        button { class: "btn-cosmic", onclick: move |_| logs_open.set(false), "Close" }
                    }
                }
            }

            {current.read().as_ref().map(|t| rsx! {
                div { class: "hidden", "selected: {t.name}" }
            })}
        }
    }
}

// ============================================================================
// Models view
// ============================================================================

#[component]
fn ModelsView(vm: Signal<String>) -> Element {
    let mut models = use_resource(move || async move { api::list_models(vm()).await });
    let mut search = use_signal(String::new);
    let mut dl_repo = use_signal(String::new);
    let mut dl_file = use_signal(String::new);

    rsx! {
        div { class: "card-cosmic p-6 mb-6",
            div { class: "flex items-center justify-between flex-wrap gap-3 mb-4",
                h2 { class: "text-xl font-bold text-star-white", "Models" }
                button { class: "btn-cosmic", onclick: move |_| models.restart(), "Refresh" }
            }
            div { class: "flex flex-wrap items-end gap-3",
                div { class: "flex-1 min-w-0",
                    input { r#type: "search", class: "w-full", placeholder: "Filter models...",
                        value: "{search}", oninput: move |e| search.set(e.value()) }
                }
            }
            div { class: "mt-4 border-t border-nebula-purple/30 pt-4",
                h3 { class: "text-star-white font-medium mb-2", "Download from HuggingFace" }
                div { class: "flex flex-wrap gap-2 items-center",
                    input { r#type: "text", class: "flex-1 min-w-[240px]", placeholder: "repo id, e.g. unsloth/Qwen3-30B-A3B-GGUF",
                        value: "{dl_repo}", oninput: move |e| dl_repo.set(e.value()) }
                    input { r#type: "text", class: "min-w-[180px]", placeholder: "file glob (optional) *Q4_K_M.gguf",
                        value: "{dl_file}", oninput: move |e| dl_file.set(e.value()) }
                    button { class: "btn-nebula",
                        onclick: move |_| {
                            let repo = dl_repo.read().trim().to_string();
                            if repo.is_empty() { return; }
                            let file = { let f = dl_file.read().trim().to_string(); if f.is_empty() { None } else { Some(f) } };
                            let v = vm();
                            spawn(async move {
                                match api::start_download(v, repo.clone(), file, None).await {
                                    Ok(()) => app_log("INFO", format!("Download started: {repo} (watch Logs)")),
                                    Err(e) => app_log("ERROR", format!("Download: {e}")),
                                }
                            });
                        },
                        "Download"
                    }
                }
                p { class: "text-stardust text-xs mt-1", "Progress streams into the Logs panel. Uses the add-on's HF_TOKEN." }
            }
        }

        div { class: "card-cosmic overflow-hidden",
            {match models.read().clone() {
                None => rsx! { div { class: "p-8 text-center text-stardust", "Loading models..." } },
                Some(Err(e)) => rsx! { div { class: "p-6 text-warning-red", "Error: {e}" } },
                Some(Ok(list)) => {
                    let q = search.read().to_lowercase();
                    let rows: Vec<ModelFile> = list.into_iter()
                        .filter(|m| q.is_empty() || m.filename.to_lowercase().contains(&q))
                        .collect();
                    if rows.is_empty() {
                        rsx! { div { class: "p-8 text-center text-stardust", "No models found." } }
                    } else {
                        rsx! {
                            table { class: "table-cosmic",
                                thead { tr {
                                    th { "Model" } th { "Quant" } th { "Format" } th { "Size" } th { "" }
                                } }
                                tbody {
                                    for m in rows {
                                        ModelRow { vm, model: m, on_changed: move |_| models.restart() }
                                    }
                                }
                            }
                        }
                    }
                }
            }}
        }
    }
}

#[component]
fn ModelRow(vm: Signal<String>, model: ModelFile, on_changed: EventHandler<()>) -> Element {
    let fmt = model.format.map(|f| format!("{f:?}")).unwrap_or_default();
    let path = model.path.clone();
    rsx! {
        tr {
            td { class: "font-mono text-sm text-moonlight", "{model.filename}" }
            td { {model.quant.clone().unwrap_or_default()} }
            td { class: "text-stardust", "{fmt}" }
            td { class: "text-stardust", {human_size(model.size_bytes)} }
            td {
                button { class: "btn-cosmic",
                    onclick: move |_| {
                        let v = vm(); let p = path.clone();
                        spawn(async move {
                            match api::delete_model(v, p.clone()).await {
                                Ok(()) => { app_log("INFO", format!("Deleted {p}")); on_changed.call(()); }
                                Err(e) => app_log("ERROR", format!("Delete: {e}")),
                            }
                        });
                    },
                    "Delete"
                }
            }
        }
    }
}

// ============================================================================
// Profiles view
// ============================================================================

#[component]
fn ProfilesView(vm: Signal<String>, vms: Signal<Vec<VmTarget>>, draft: Signal<Option<ServiceProfile>>) -> Element {
    let mut profiles = use_resource(move || async move { api::list_profiles().await });
    let mut active = use_resource(move || async move { api::active_profile(vm()).await });

    let new_profile = move |_| {
        let v = vm();
        draft.set(Some(ServiceProfile {
            vm: v,
            engine: EngineKind::LlamaCpp,
            ..Default::default()
        }));
    };

    rsx! {
        div { class: "card-cosmic p-6 mb-6",
            div { class: "flex items-center justify-between flex-wrap gap-3",
                h2 { class: "text-xl font-bold text-star-white", "Service profiles" }
                div { class: "flex gap-2",
                    button { class: "btn-cosmic", onclick: move |_| profiles.restart(), "Refresh" }
                    button { class: "btn-nebula", onclick: new_profile, "New profile" }
                }
            }
            p { class: "text-stardust text-sm mt-1",
                "Saved known-good services. Activate swaps the running LLM service on this VM and restarts it."
            }
        }

        {match profiles.read().clone() {
            None => rsx! { div { class: "card-cosmic p-8 text-center text-stardust", "Loading..." } },
            Some(Err(e)) => rsx! { div { class: "card-cosmic p-6 text-warning-red", "Error: {e}" } },
            Some(Ok(list)) => {
                let cur_vm = vm();
                let active_id = active.read().clone().and_then(|r| r.ok()).flatten().unwrap_or_default();
                let rows: Vec<ServiceProfile> = list.into_iter().filter(|p| p.vm == cur_vm).collect();
                if rows.is_empty() {
                    rsx! { div { class: "card-cosmic p-8 text-center text-stardust", "No profiles for this VM yet. Create one from a known-good compose service." } }
                } else {
                    rsx! {
                        div { class: "grid gap-4",
                            for p in rows {
                                ProfileCard {
                                    profile: p.clone(),
                                    is_active: p.id == active_id,
                                    draft,
                                    on_changed: move |_| { profiles.restart(); active.restart(); },
                                }
                            }
                        }
                    }
                }
            }
        }}
    }
}

#[component]
fn ProfileCard(profile: ServiceProfile, is_active: bool, draft: Signal<Option<ServiceProfile>>, on_changed: EventHandler<()>) -> Element {
    let mut busy = use_signal(|| false);
    let p_activate = profile.clone();
    let p_edit = profile.clone();
    let id_del = profile.id.clone();
    let port_txt = profile.host_port.map(|p| format!(":{p}")).unwrap_or_default();

    rsx! {
        div { class: if is_active { "card-cosmic p-5 border-2 border-nebula-purple" } else { "card-cosmic p-5" },
            div { class: "flex items-start justify-between gap-3 flex-wrap",
                div {
                    div { class: "flex items-center gap-2 flex-wrap",
                        span { class: "text-lg", {engine_icon(&profile.engine)} }
                        h3 { class: "text-star-white font-semibold", "{profile.name}" }
                        {if is_active { rsx! { span { class: "badge badge-nebula", "ACTIVE" } } } else { rsx!{} }}
                        {if profile.known_good { rsx! { span { class: "badge badge-method", "known-good" } } } else { rsx!{} }}
                    }
                    p { class: "text-stardust text-sm mt-1 font-mono", "{profile.engine.as_str()} · {profile.model_ref}{port_txt}" }
                    {profile.notes.clone().filter(|n| !n.is_empty()).map(|n| rsx! { p { class: "text-moonlight text-sm mt-1", "{n}" } })}
                }
                div { class: "flex gap-2",
                    button {
                        class: "btn-nebula",
                        disabled: *busy.read(),
                        onclick: move |_| {
                            let id = p_activate.id.clone();
                            busy.set(true);
                            spawn(async move {
                                app_log("INFO", format!("Activating profile {id}..."));
                                match api::activate_profile(id).await {
                                    Ok(r) => {
                                        app_log(if r.ok {"INFO"} else {"ERROR"}, format!("Activate: {} (health_ok={})", r.message, r.health_ok));
                                        for l in r.log_tail { app_log("INFO", format!("[svc] {l}")); }
                                        on_changed.call(());
                                    }
                                    Err(e) => app_log("ERROR", format!("Activate: {e}")),
                                }
                                busy.set(false);
                            });
                        },
                        {if *busy.read() { "Activating..." } else { "Activate" }}
                    }
                    button { class: "btn-cosmic", onclick: move |_| draft.set(Some(p_edit.clone())), "Edit" }
                    button {
                        class: "btn-cosmic",
                        onclick: move |_| {
                            let id = id_del.clone();
                            spawn(async move {
                                match api::delete_profile(id).await {
                                    Ok(()) => on_changed.call(()),
                                    Err(e) => app_log("ERROR", format!("Delete: {e}")),
                                }
                            });
                        },
                        "Delete"
                    }
                }
            }
        }
    }
}

// ============================================================================
// Profile editor dialog
// ============================================================================

#[component]
fn ProfileEditor(vm: Signal<String>, vms: Signal<Vec<VmTarget>>, draft: Signal<Option<ServiceProfile>>) -> Element {
    let models = use_resource(move || async move { api::list_models(vm()).await });
    let mut msg = use_signal(|| None::<String>);

    // Field accessors that read/write the Option<ServiceProfile> draft.
    macro_rules! field {
        ($get:expr) => {
            draft.read().as_ref().map($get).unwrap_or_default()
        };
    }

    let close = move |_| draft.set(None);

    rsx! {
        div { class: "fixed inset-0 z-50 flex items-center justify-center bg-black/60",
            div { class: "card-cosmic p-6 max-w-3xl w-full mx-4 max-h-[92vh] overflow-y-auto",
                onclick: move |e| e.stop_propagation(),
                h2 { class: "text-xl font-bold text-star-white mb-4", "Service profile" }
                div { class: "grid gap-3",
                    div { class: "grid grid-cols-2 gap-3",
                        label { class: "flex flex-col text-sm text-stardust", "Name"
                            input { r#type: "text", value: field!(|p| p.name.clone()),
                                oninput: move |e| { if let Some(p) = draft.write().as_mut() { p.name = e.value(); } } }
                        }
                        label { class: "flex flex-col text-sm text-stardust", "Engine"
                            select {
                                onchange: move |e| { if let Some(p) = draft.write().as_mut() { p.engine = engine_from_label(&e.value()); } },
                                for eng in ALL_ENGINES {
                                    option { value: eng.as_str(), selected: field!(|p| p.engine) == eng, "{eng.as_str()}" }
                                }
                            }
                        }
                    }
                    div { class: "grid grid-cols-2 gap-3",
                        label { class: "flex flex-col text-sm text-stardust", "VM"
                            select {
                                onchange: move |e| { if let Some(p) = draft.write().as_mut() { p.vm = e.value(); } },
                                for t in vms.read().iter().cloned() {
                                    option { value: t.id.clone(), selected: field!(|p| p.vm.clone()) == t.id, "{t.name}" }
                                }
                            }
                        }
                        label { class: "flex flex-col text-sm text-stardust", "Host port"
                            input { r#type: "number", value: field!(|p| p.host_port.map(|x| x.to_string()).unwrap_or_default()),
                                oninput: move |e| { if let Some(p) = draft.write().as_mut() { p.host_port = e.value().trim().parse().ok(); } } }
                        }
                    }
                    label { class: "flex flex-col text-sm text-stardust", "Model (path or HF repo)"
                        input { r#type: "text", list: "model-list", value: field!(|p| p.model_ref.clone()),
                            oninput: move |e| {
                                let v = e.value();
                                if let Some(p) = draft.write().as_mut() {
                                    // Default the per-model tied-file dir to the model's basename.
                                    let base = v.rsplit('/').next().unwrap_or(&v).to_string();
                                    if p.model_dir_name.is_empty() { p.model_dir_name = base; }
                                    p.model_ref = v;
                                }
                            }
                        }
                        datalist { id: "model-list",
                            {match models.read().clone() {
                                Some(Ok(list)) => rsx! { for m in list { option { value: m.path.clone(), "{m.filename}" } } },
                                _ => rsx!{},
                            }}
                        }
                    }
                    div { class: "grid grid-cols-3 gap-3",
                        label { class: "flex flex-col text-sm text-stardust", "Service name"
                            input { r#type: "text", value: field!(|p| p.service_name.clone()),
                                oninput: move |e| { if let Some(p) = draft.write().as_mut() { p.service_name = e.value(); } } }
                        }
                        label { class: "flex flex-col text-sm text-stardust", "Container name"
                            input { r#type: "text", value: field!(|p| p.container_name.clone()),
                                oninput: move |e| { if let Some(p) = draft.write().as_mut() { p.container_name = e.value(); } } }
                        }
                        label { class: "flex flex-col text-sm text-stardust", "Model dir (tied files)"
                            input { r#type: "text", value: field!(|p| p.model_dir_name.clone()),
                                oninput: move |e| { if let Some(p) = draft.write().as_mut() { p.model_dir_name = e.value(); } } }
                        }
                    }
                    label { class: "flex flex-col text-sm text-stardust", "Compose service YAML"
                        textarea { class: "font-mono text-xs min-h-[220px]", value: field!(|p| p.compose_fragment.clone()),
                            oninput: move |e| { if let Some(p) = draft.write().as_mut() { p.compose_fragment = e.value(); } } }
                    }
                    div { class: "grid grid-cols-2 gap-3",
                        label { class: "flex flex-col text-sm text-stardust", "Dockerfile name (optional)"
                            input { r#type: "text", placeholder: "Dockerfile.llama", value: field!(|p| p.dockerfile_name.clone().unwrap_or_default()),
                                oninput: move |e| { if let Some(p) = draft.write().as_mut() { let v=e.value(); p.dockerfile_name = if v.is_empty(){None}else{Some(v)}; } } }
                        }
                        label { class: "flex flex-col text-sm text-stardust", "Notes"
                            input { r#type: "text", value: field!(|p| p.notes.clone().unwrap_or_default()),
                                oninput: move |e| { if let Some(p) = draft.write().as_mut() { let v=e.value(); p.notes = if v.is_empty(){None}else{Some(v)}; } } }
                        }
                    }
                    {if field!(|p| p.dockerfile_name.is_some()) {
                        rsx! {
                            label { class: "flex flex-col text-sm text-stardust", "Dockerfile contents"
                                textarea { class: "font-mono text-xs min-h-[140px]", value: field!(|p| p.dockerfile.clone().unwrap_or_default()),
                                    oninput: move |e| { if let Some(p) = draft.write().as_mut() { p.dockerfile = Some(e.value()); } } }
                            }
                        }
                    } else { rsx!{} }}

                    TiedFilesEditor { draft }

                    label { class: "flex items-center gap-2 text-sm text-stardust",
                        input { r#type: "checkbox", checked: field!(|p| p.known_good),
                            onchange: move |e| { if let Some(p) = draft.write().as_mut() { p.known_good = e.checked(); } } }
                        "Mark as known-good"
                    }
                }

                {msg.read().as_ref().map(|m| rsx! { p { class: "text-warning-red text-sm mt-2", "{m}" } })}

                div { class: "flex justify-end gap-2 mt-4",
                    button { class: "btn-cosmic", onclick: close, "Cancel" }
                    button { class: "btn-nebula",
                        onclick: move |_| {
                            let Some(p) = draft.read().clone() else { return };
                            if p.name.trim().is_empty() { msg.set(Some("Name is required.".into())); return; }
                            if p.service_name.trim().is_empty() { msg.set(Some("Service name is required.".into())); return; }
                            spawn(async move {
                                match api::save_profile(p).await {
                                    Ok(_) => draft.set(None),
                                    Err(e) => msg.set(Some(e.to_string())),
                                }
                            });
                        },
                        "Save"
                    }
                }
            }
        }
    }
}

#[component]
fn TiedFilesEditor(draft: Signal<Option<ServiceProfile>>) -> Element {
    let files = draft.read().as_ref().map(|p| p.tied_files.clone()).unwrap_or_default();
    rsx! {
        div { class: "border border-nebula-purple/40 rounded-lg p-3",
            div { class: "flex items-center justify-between mb-2",
                span { class: "text-star-white text-sm font-medium", "Tied files (chat templates, grammars) → services/<model dir>/" }
                button { class: "btn-cosmic",
                    onclick: move |_| { if let Some(p) = draft.write().as_mut() {
                        p.tied_files.push(TiedFile { role: TiedFileRole::ChatTemplate, filename: String::new(), content: String::new() });
                    } },
                    "+ Add"
                }
            }
            for (i, tf) in files.into_iter().enumerate() {
                div { class: "grid gap-2 mb-3 border-b border-nebula-purple/20 pb-3",
                    div { class: "flex gap-2",
                        select {
                            onchange: move |e| { if let Some(p) = draft.write().as_mut() { if let Some(f) = p.tied_files.get_mut(i) {
                                f.role = match e.value().as_str() { "chat_template"=>TiedFileRole::ChatTemplate, "grammar"=>TiedFileRole::Grammar, "config"=>TiedFileRole::Config, _=>TiedFileRole::Other };
                            } } },
                            option { value: "chat_template", selected: tf.role == TiedFileRole::ChatTemplate, "chat_template" }
                            option { value: "grammar", selected: tf.role == TiedFileRole::Grammar, "grammar" }
                            option { value: "config", selected: tf.role == TiedFileRole::Config, "config" }
                            option { value: "other", selected: tf.role == TiedFileRole::Other, "other" }
                        }
                        input { r#type: "text", class: "flex-1", placeholder: "filename e.g. a3btemplate.jinja", value: "{tf.filename}",
                            oninput: move |e| { if let Some(p) = draft.write().as_mut() { if let Some(f) = p.tied_files.get_mut(i) { f.filename = e.value(); } } } }
                        button { class: "btn-cosmic",
                            onclick: move |_| { if let Some(p) = draft.write().as_mut() { if i < p.tied_files.len() { p.tied_files.remove(i); } } },
                            "✕"
                        }
                    }
                    textarea { class: "font-mono text-xs min-h-[90px]", placeholder: "file contents", value: "{tf.content}",
                        oninput: move |e| { if let Some(p) = draft.write().as_mut() { if let Some(f) = p.tied_files.get_mut(i) { f.content = e.value(); } } } }
                }
            }
        }
    }
}

// ============================================================================
// Files view (raw compose / Dockerfile editor)
// ============================================================================

#[component]
fn FilesView(vm: Signal<String>) -> Element {
    let mut path = use_signal(|| "docker-compose.yml".to_string());
    let mut content = use_signal(String::new);
    let mut loaded_path = use_signal(String::new);
    let mut status = use_signal(|| None::<String>);

    let do_load = use_callback(move |p: String| {
        let v = vm();
        path.set(p.clone());
        spawn(async move {
            match api::read_file(v, p.clone()).await {
                Ok(fp) => { content.set(fp.content); loaded_path.set(fp.path); status.set(Some(format!("Loaded {p}"))); }
                Err(e) => status.set(Some(format!("Load error: {e}"))),
            }
        });
    });

    rsx! {
        div { class: "card-cosmic p-6 mb-4",
            div { class: "flex items-center justify-between flex-wrap gap-3 mb-3",
                h2 { class: "text-xl font-bold text-star-white", "Files" }
                div { class: "flex gap-2 flex-wrap",
                    button { class: "btn-cosmic", onclick: move |_| do_load.call("docker-compose.yml".into()), "docker-compose.yml" }
                    button { class: "btn-cosmic", onclick: move |_| do_load.call("Dockerfile.llama".into()), "Dockerfile.llama" }
                    button { class: "btn-cosmic", onclick: move |_| do_load.call("Dockerfile.vllm".into()), "Dockerfile.vllm" }
                }
            }
            div { class: "flex gap-2 items-center",
                input { r#type: "text", class: "flex-1 font-mono text-sm", placeholder: "jarvis-relative path",
                    value: "{path}", oninput: move |e| path.set(e.value()) }
                button { class: "btn-cosmic", onclick: move |_| do_load.call(path.read().clone()), "Load" }
            }
            {status.read().as_ref().map(|s| rsx! { p { class: "text-stardust text-sm mt-2", "{s}" } })}
        }

        div { class: "card-cosmic p-4",
            textarea { class: "w-full font-mono text-xs min-h-[440px]", value: "{content}",
                oninput: move |e| content.set(e.value()) }
            div { class: "flex gap-2 mt-3 justify-end flex-wrap",
                button { class: "btn-nebula",
                    onclick: move |_| {
                        let v = vm(); let p = path.read().clone(); let c = content.read().clone();
                        spawn(async move {
                            match api::write_file(v, p.clone(), c).await {
                                Ok(()) => { app_log("INFO", format!("Saved {p}")); status.set(Some(format!("Saved {p}"))); }
                                Err(e) => status.set(Some(format!("Save error: {e}"))),
                            }
                        });
                    },
                    "Save"
                }
                button { class: "btn-cosmic",
                    onclick: move |_| {
                        let v = vm(); let p = loaded_path.read().clone();
                        let file = if p.is_empty() { path.read().clone() } else { p };
                        spawn(async move {
                            match api::compose_up(v, file.clone(), None).await {
                                Ok(()) => app_log("INFO", format!("compose up {file}")),
                                Err(e) => app_log("ERROR", format!("compose up: {e}")),
                            }
                        });
                    },
                    "Compose up"
                }
            }
        }
    }
}

// ============================================================================
// System view (agent info, GPU, containers)
// ============================================================================

#[component]
fn SystemView(vm: Signal<String>) -> Element {
    let mut info = use_resource(move || async move { api::agent_info(vm()).await });
    let mut containers = use_resource(move || async move { api::list_containers(vm()).await });

    rsx! {
        div { class: "card-cosmic p-6 mb-4",
            div { class: "flex items-center justify-between mb-3",
                h2 { class: "text-xl font-bold text-star-white", "System" }
                button { class: "btn-cosmic", onclick: move |_| { info.restart(); containers.restart(); }, "Refresh" }
            }
            {match info.read().clone() {
                None => rsx! { p { class: "text-stardust", "Querying agent..." } },
                Some(Err(e)) => rsx! { p { class: "text-warning-red", "Agent offline: {e}" } },
                Some(Ok(i)) => rsx! {
                    div { class: "flex flex-wrap gap-6",
                        div { span { class: "text-stardust text-xs", "Host" } p { class: "text-star-white", "{i.hostname}" } }
                        div { span { class: "text-stardust text-xs", "Accel" } p { class: "text-star-white", "{i.accel}" } }
                        div { span { class: "text-stardust text-xs", "Docker" } p { class: "text-star-white", "{i.docker_version}" } }
                        div { span { class: "text-stardust text-xs", "Agent" } p { class: "text-star-white", "v{i.agent_version}" } }
                    }
                    div { class: "grid gap-3 mt-4",
                        for g in i.gpus {
                            div { class: "border border-nebula-purple/30 rounded-lg p-3",
                                div { class: "flex justify-between",
                                    span { class: "text-star-white font-medium", "{g.name}" }
                                    span { class: "text-stardust text-sm", {format!("{} / {} MB", g.mem_used_mb, g.mem_total_mb)} }
                                }
                                {g.util_pct.map(|u| rsx! { span { class: "text-stardust text-xs", "util {u:.0}%" } })}
                            }
                        }
                    }
                },
            }}
        }

        div { class: "card-cosmic overflow-hidden",
            div { class: "p-4 border-b border-nebula-purple/30", h3 { class: "text-star-white font-semibold", "Containers" } }
            {match containers.read().clone() {
                None => rsx! { div { class: "p-6 text-stardust", "Loading..." } },
                Some(Err(e)) => rsx! { div { class: "p-6 text-warning-red", "Error: {e}" } },
                Some(Ok(list)) => rsx! {
                    table { class: "table-cosmic",
                        thead { tr { th { "Name" } th { "Image" } th { "State" } th { "Status" } th { "" } } }
                        tbody {
                            for c in list {
                                ContainerRow { vm, container: c, on_changed: move |_| containers.restart() }
                            }
                        }
                    }
                },
            }}
        }
    }
}

#[component]
fn ContainerRow(vm: Signal<String>, container: ContainerStatus, on_changed: EventHandler<()>) -> Element {
    let name = container.name.clone();
    let state_cls = if container.state == "running" { "text-alien-green" } else { "text-stardust" };
    rsx! {
        tr {
            td { class: "font-mono text-sm text-moonlight", "{container.name}" }
            td { class: "text-stardust text-sm", "{container.image}" }
            td { class: "{state_cls}", "{container.state}" }
            td { class: "text-stardust text-sm", "{container.status}" }
            td {
                button { class: "btn-cosmic",
                    onclick: move |_| {
                        let v = vm(); let n = name.clone();
                        spawn(async move {
                            match api::restart_container(v, n.clone()).await {
                                Ok(()) => { app_log("INFO", format!("Restarted {n}")); on_changed.call(()); }
                                Err(e) => app_log("ERROR", format!("Restart: {e}")),
                            }
                        });
                    },
                    "Restart"
                }
            }
        }
    }
}

// ============================================================================
// Settings dialog (VM targets)
// ============================================================================

#[component]
fn SettingsDialog(vms: Signal<Vec<VmTarget>>, on_close: EventHandler<()>) -> Element {
    let mut rows = use_signal(|| vms.read().clone());

    rsx! {
        div { class: "fixed inset-0 z-50 flex items-center justify-center bg-black/60",
            div { class: "card-cosmic p-6 max-w-3xl w-full mx-4 max-h-[90vh] overflow-y-auto",
                onclick: move |e| e.stop_propagation(),
                h2 { class: "text-xl font-bold text-star-white mb-2", "VM targets" }
                p { class: "text-stardust text-sm mb-4", "Each VM runs a ModelDeck agent. URL e.g. http://AGENT_IP:9777 (its Tailscale or LAN IP, port 9777)." }
                for (i, t) in rows.read().clone().into_iter().enumerate() {
                    div { class: "grid grid-cols-6 gap-2 mb-2 items-center",
                        input { r#type: "text", placeholder: "id", value: "{t.id}",
                            oninput: move |e| { if let Some(r) = rows.write().get_mut(i) { r.id = e.value(); } } }
                        input { r#type: "text", placeholder: "name", value: "{t.name}",
                            oninput: move |e| { if let Some(r) = rows.write().get_mut(i) { r.name = e.value(); } } }
                        input { r#type: "text", placeholder: "accel", value: t.accel.clone().unwrap_or_default(),
                            oninput: move |e| { if let Some(r) = rows.write().get_mut(i) { r.accel = Some(e.value()); } } }
                        input { r#type: "text", class: "col-span-2", placeholder: "agent_url", value: "{t.agent_url}",
                            oninput: move |e| { if let Some(r) = rows.write().get_mut(i) { r.agent_url = e.value(); } } }
                        div { class: "flex gap-1",
                            button { class: "btn-nebula",
                                onclick: move |_| {
                                    let Some(t) = rows.read().get(i).cloned() else { return };
                                    spawn(async move {
                                        match api::save_vm(t).await { Ok(()) => app_log("INFO", "Saved VM"), Err(e) => app_log("ERROR", format!("Save VM: {e}")) }
                                    });
                                },
                                "Save"
                            }
                            button { class: "btn-cosmic",
                                onclick: move |_| {
                                    let id = rows.read().get(i).map(|r| r.id.clone()).unwrap_or_default();
                                    rows.write().remove(i);
                                    spawn(async move { let _ = api::delete_vm(id).await; });
                                },
                                "✕"
                            }
                        }
                    }
                }
                div { class: "flex gap-2 mt-3",
                    button { class: "btn-cosmic",
                        onclick: move |_| rows.write().push(VmTarget { jarvis_path: "/home/shadowbroker/jarvis".into(), ..Default::default() }),
                        "+ Add VM"
                    }
                    button { class: "btn-cosmic",
                        onclick: move |_| {
                            let snapshot = rows.read().clone();
                            spawn(async move {
                                if let Ok(list) = api::list_vms().await { vms.set(list); }
                            });
                            vms.set(snapshot);
                            on_close.call(());
                        },
                        "Close"
                    }
                }
            }
        }
    }
}
