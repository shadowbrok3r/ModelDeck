//! ModelDeck agent: a small HTTP service that runs on each AI VM (as a compose
//! service with docker.sock + ~/jarvis mounted) and performs all docker/fs/model/
//! GPU work on behalf of the hub. Authenticated with a shared bearer secret.

mod ops;

use std::convert::Infallible;
use std::sync::Arc;

use axum::{
    extract::{Query, Request, State},
    http::StatusCode,
    middleware::{self, Next},
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse, Response,
    },
    routing::{get, post, put},
    Json, Router,
};
use modeldeck_shared::*;
use ops::Config;
use serde::Deserialize;
use tokio_stream::wrappers::ReceiverStream;
use tokio_stream::StreamExt;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

/// anyhow -> 500 with the message in the body.
struct ApiError(anyhow::Error);
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        tracing::warn!(error = %self.0, "request failed");
        (StatusCode::INTERNAL_SERVER_ERROR, self.0.to_string()).into_response()
    }
}
impl<E: Into<anyhow::Error>> From<E> for ApiError {
    fn from(e: E) -> Self {
        ApiError(e.into())
    }
}
type Api<T> = Result<Json<T>, ApiError>;

#[tokio::main]
async fn main() {
    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .with(tracing_subscriber::fmt::layer())
        .init();

    let cfg = Config::from_env();
    if cfg.secret.is_empty() {
        tracing::warn!("MODELDECK_AGENT_SECRET is empty — refusing unauthenticated requests");
    }
    let port = cfg.port;
    let state = Arc::new(cfg);

    let public = Router::new().route("/health", get(|| async { "ok" }));
    let private = Router::new()
        .route("/info", get(get_info))
        .route("/gpu", get(get_gpu))
        .route("/containers", get(get_containers))
        .route("/compose", get(get_compose))
        .route("/models", get(get_models))
        .route("/models/delete", post(post_model_delete))
        .route("/models/download", post(post_download))
        .route("/file", get(get_file).put(put_file))
        .route("/file/write", put(put_file))
        .route("/compose/up", post(post_up))
        .route("/compose/down", post(post_down))
        .route("/compose/restart", post(post_restart))
        .route("/activate", post(post_activate))
        .route("/logs", get(get_logs))
        .route_layer(middleware::from_fn_with_state(state.clone(), auth));

    let app = public.merge(private).with_state(state);

    let addr = format!("0.0.0.0:{port}");
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .unwrap_or_else(|e| panic!("bind {addr}: {e}"));
    tracing::info!(%addr, "modeldeck-agent listening");
    axum::serve(listener, app).await.expect("server error");
}

/// Bearer-token gate. Constant-ish comparison; the secret is shared with the hub.
async fn auth(State(cfg): State<Arc<Config>>, req: Request, next: Next) -> Response {
    let presented = req
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("");
    if cfg.secret.is_empty() || presented != cfg.secret {
        return (StatusCode::UNAUTHORIZED, "unauthorized").into_response();
    }
    next.run(req).await
}

// --- handlers ---------------------------------------------------------------

async fn get_info(State(cfg): State<Arc<Config>>) -> Json<AgentInfo> {
    Json(ops::info(&cfg).await)
}

async fn get_gpu(State(cfg): State<Arc<Config>>) -> Json<Vec<GpuStats>> {
    Json(ops::gpu_stats(cfg.accel).await)
}

async fn get_containers() -> Api<Vec<ContainerStatus>> {
    Ok(Json(ops::containers().await?))
}

async fn get_compose() -> Api<Vec<ComposeProject>> {
    Ok(Json(ops::compose_projects().await?))
}

async fn get_models(State(cfg): State<Arc<Config>>) -> Json<Vec<ModelFile>> {
    Json(ops::list_models(&cfg))
}

#[derive(Deserialize)]
struct PathQuery {
    path: String,
}

async fn get_file(State(cfg): State<Arc<Config>>, Query(q): Query<PathQuery>) -> Api<FilePayload> {
    Ok(Json(ops::read_file(&cfg, &q.path)?))
}

async fn put_file(State(cfg): State<Arc<Config>>, Json(p): Json<FilePayload>) -> Api<Ack> {
    ops::write_file(&cfg, &p)?;
    Ok(Json(Ack::ok()))
}

#[derive(Deserialize)]
struct DeleteReq {
    path: String,
}

async fn post_model_delete(
    State(cfg): State<Arc<Config>>,
    Json(req): Json<DeleteReq>,
) -> Api<Ack> {
    ops::delete_model(&cfg, &req.path).await?;
    Ok(Json(Ack::ok()))
}

#[derive(Deserialize)]
struct UpReq {
    /// jarvis-relative or absolute compose file path.
    file: String,
    #[serde(default)]
    project: Option<String>,
}

async fn post_up(State(cfg): State<Arc<Config>>, Json(req): Json<UpReq>) -> Api<Ack> {
    let path = ops::confine(&cfg.jarvis, &req.file)?;
    let out = ops::compose_up(&path, req.project.as_deref()).await?;
    Ok(Json(Ack::msg(out)))
}

#[derive(Deserialize)]
struct ProjectReq {
    project: String,
}

async fn post_down(Json(req): Json<ProjectReq>) -> Api<Ack> {
    Ok(Json(Ack::msg(ops::compose_down(&req.project).await?)))
}

#[derive(Deserialize)]
struct RestartReq {
    container: String,
}

async fn post_restart(Json(req): Json<RestartReq>) -> Api<Ack> {
    Ok(Json(Ack::msg(ops::restart_container(&req.container).await?)))
}

async fn post_activate(
    State(cfg): State<Arc<Config>>,
    Json(profile): Json<ServiceProfile>,
) -> Api<ActivateResult> {
    Ok(Json(ops::activate(&cfg, &profile).await?))
}

#[derive(Deserialize)]
struct LogsQuery {
    container: String,
    #[serde(default = "default_tail")]
    tail: u32,
}
fn default_tail() -> u32 {
    200
}

async fn get_logs(Query(q): Query<LogsQuery>) -> Api<FilePayload> {
    let content = ops::logs(&q.container, q.tail).await?;
    Ok(Json(FilePayload {
        path: q.container,
        content,
    }))
}

/// Stream `hf download` progress to the browser as Server-Sent Events.
async fn post_download(
    State(cfg): State<Arc<Config>>,
    Json(req): Json<DownloadRequest>,
) -> Result<Sse<impl futures::Stream<Item = Result<Event, Infallible>>>, ApiError> {
    let rx = ops::spawn_download(&cfg, &req)?;
    let stream = ReceiverStream::new(rx).map(|line| Ok(Event::default().data(line)));
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

/// Minimal acknowledgement payload.
#[derive(serde::Serialize)]
struct Ack {
    ok: bool,
    message: String,
}
impl Ack {
    fn ok() -> Self {
        Ack { ok: true, message: String::new() }
    }
    fn msg(m: String) -> Self {
        Ack { ok: true, message: m }
    }
}
