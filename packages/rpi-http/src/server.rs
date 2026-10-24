use crate::security::{self, Security};
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    middleware::from_fn_with_state,
    response::{
        sse::{Event, Sse},
        IntoResponse,
    },
    routing::{get, post},
    Json, Router,
};
use futures_util::{
    stream::{self, Stream},
    StreamExt,
};
use rpi_extcore::{
    host::BufferedEvent,
    protocol::{self, ApiCommand},
    reply::{IntoReply, ReplyKind, ServiceReply},
    service::{
        self, AuthRefreshRequest, BackendConfig, CreateProjectRequest, CreateSessionRequest,
        FileQuery, ForkRequest, MetadataRequest, ProjectFoldersQuery, RenameRequest, Service,
        SessionListQuery,
    },
};
use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use std::{convert::Infallible, net::SocketAddr, path::PathBuf, sync::Arc};
use tokio::sync::{broadcast, watch};
#[derive(Clone, Debug, serde::Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct AppConfig {
    #[serde(skip, default = "default_service_name")]
    pub service_name: &'static str,
    pub project: PathBuf,
    /// Additional workspace roots explicitly made available to clients.
    pub projects: Vec<PathBuf>,
    pub projects_root: Option<PathBuf>,
    pub rpi_bin: PathBuf,
    pub bind: String,
    pub port: u16,
    /// Optional Host allowlist; RPI_WEB_ALLOWED_HOSTS takes precedence.
    pub allowed_hosts: Option<Vec<String>>,
    /// Optional `--session-dir` forwarded to the native host.
    pub session_dir: Option<PathBuf>,
    /// Where the bridge remembers session ids across restarts.
    pub registry: PathBuf,
}

fn default_service_name() -> &'static str {
    "rpi-http"
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            service_name: "rpi-http",
            project: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            projects: Vec::new(),
            projects_root: None,
            rpi_bin: PathBuf::from("rpi"),
            bind: "127.0.0.1".into(),
            port: 3030,
            allowed_hosts: None,
            session_dir: None,
            registry: rpi_extcore::registry::default_path(),
        }
    }
}

#[derive(Clone)]
struct AppState {
    backend: Arc<Service>,
    security: Arc<Security>,
    shutdown: watch::Receiver<bool>,
    service_name: &'static str,
}
pub struct Listening {
    pub address: SocketAddr,
    pub hosts: Vec<String>,
    pub password_enabled: bool,
}
struct HttpReply(ServiceReply);
impl IntoResponse for HttpReply {
    fn into_response(self) -> axum::response::Response {
        let status = match self.0.kind {
            ReplyKind::Success => StatusCode::OK,
            ReplyKind::Created => StatusCode::CREATED,
            ReplyKind::InvalidInput => StatusCode::BAD_REQUEST,
            ReplyKind::NotFound => StatusCode::NOT_FOUND,
            ReplyKind::AccessDenied => StatusCode::FORBIDDEN,
            ReplyKind::LimitExceeded => StatusCode::PAYLOAD_TOO_LARGE,
            ReplyKind::UnsupportedContent => StatusCode::UNSUPPORTED_MEDIA_TYPE,
            ReplyKind::Internal => StatusCode::INTERNAL_SERVER_ERROR,
            ReplyKind::HostUnavailable => StatusCode::BAD_GATEWAY,
            ReplyKind::Timeout => StatusCode::GATEWAY_TIMEOUT,
            ReplyKind::Unsupported => StatusCode::NOT_IMPLEMENTED,
            ReplyKind::Conflict => StatusCode::CONFLICT,
            ReplyKind::Empty => StatusCode::NO_CONTENT,
            ReplyKind::Unavailable => StatusCode::SERVICE_UNAVAILABLE,
        };
        if matches!(self.0.kind, ReplyKind::Empty) {
            status.into_response()
        } else {
            (status, Json(self.0.value)).into_response()
        }
    }
}
pub async fn run(config: AppConfig) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    serve_until(
        config,
        Router::new(),
        |info| println!("rpi HTTP API listening on {}", info.address),
        async {
            let _ = tokio::signal::ctrl_c().await;
        },
    )
    .await
}
pub async fn serve_until(
    config: AppConfig,
    frontend: Router,
    on_listen: impl FnOnce(Listening),
    stop: impl std::future::Future<Output = ()> + Send + 'static,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut config = config;
    let requested = SocketAddr::new(config.bind.parse()?, config.port);
    let listener = tokio::net::TcpListener::bind(requested)
        .await
        .map_err(|error| {
            std::io::Error::new(
                error.kind(),
                format!("cannot listen on {requested}: {error}"),
            )
        })?;
    let address = listener.local_addr()?;
    config.port = address.port();
    let backend = Service::new(BackendConfig {
        project: config.project.clone(),
        projects: config.projects.clone(),
        projects_root: config.projects_root.clone(),
        rpi_bin: config.rpi_bin.clone(),
        session_dir: config.session_dir.clone(),
        registry: config.registry.clone(),
    })?;
    let hosts = allowed_hosts(&config);
    let password = configured_password(&config);
    let security = Arc::new(Security::new(hosts.clone(), password.as_deref()));
    let (shutdown, receiver) = watch::channel(false);
    let state = AppState {
        backend,
        security: security.clone(),
        shutdown: receiver,
        service_name: config.service_name,
    };
    let router = routes(state.clone())
        .merge(frontend)
        .layer(from_fn_with_state(security, crate::security::guard));
    on_listen(Listening {
        address,
        hosts,
        password_enabled: state.security.enabled(),
    });
    std::io::Write::flush(&mut std::io::stdout())?;
    let mut shutdown_receiver = state.shutdown.clone();
    let serving = std::future::IntoFuture::into_future(
        axum::serve(listener, router).with_graceful_shutdown(async move {
            if !*shutdown_receiver.borrow_and_update() {
                let _ = shutdown_receiver.changed().await;
            }
        }),
    );
    tokio::pin!(serving);
    tokio::select! {
        result = &mut serving => result?,
        _ = stop => {
            println!("  Status      Stopping...");
            let _ = shutdown.send(true);
            let _ = tokio::time::timeout(std::time::Duration::from_secs(8), async {
                state.backend.shutdown().await;
                let _ = serving.await;
            }).await;
        }
    }
    println!("  Status      Stopped");
    Ok(())
}

/// Build only the API routes around an existing shared service.
/// The port must be the actual listener port when validating Host headers.
pub fn router(
    backend: Arc<Service>,
    config: &AppConfig,
    shutdown: watch::Receiver<bool>,
) -> Router {
    let security = Arc::new(Security::new(
        allowed_hosts(config),
        configured_password(config).as_deref(),
    ));
    let state = AppState {
        backend,
        security: security.clone(),
        shutdown,
        service_name: config.service_name,
    };
    routes(state).layer(from_fn_with_state(security, crate::security::guard))
}
fn routes(state: AppState) -> Router {
    Router::new()
        .route("/api/v1/health", get(health))
        .route("/api/v1/capabilities", get(capabilities))
        .route("/api/v1/projects", get(list_projects).post(create_project))
        .route("/api/v1/project-folders", get(list_project_folders))
        .route("/api/v1/projects/{id}/files", get(project_files))
        .route("/api/v1/projects/{id}/file", get(project_file))
        .route("/api/v1/projects/{id}/git/status", get(project_status))
        .route("/api/v1/projects/{id}/git/diff", get(project_diff))
        .route(
            "/api/v1/auth",
            get(auth_status).post(auth_login).delete(auth_logout),
        )
        .route("/api/v1/auth/refresh", post(refresh_auth))
        .route("/api/v1/sessions", post(create_session).get(list_sessions))
        .route(
            "/api/v1/sessions/{id}",
            get(get_session).delete(delete_session),
        )
        .route("/api/v1/sessions/{id}/open", post(open_session))
        .route("/api/v1/sessions/{id}/fork", post(fork_session))
        .route("/api/v1/sessions/{id}/clone", post(clone_session))
        .route("/api/v1/sessions/{id}/rename", post(rename_session))
        .route("/api/v1/sessions/{id}/metadata", post(session_metadata))
        .route("/api/v1/sessions/{id}/commands", post(command))
        .route("/api/v1/sessions/{id}/snapshot", get(snapshot))
        .route("/api/v1/sessions/{id}/events", get(events))
        .with_state(state)
}

fn configured_password(config: &AppConfig) -> Option<String> {
    let (primary, legacy) = if config.service_name == "rpi-web" {
        ("RPI_WEB_PASSWORD", "RPI_HTTP_PASSWORD")
    } else {
        ("RPI_HTTP_PASSWORD", "RPI_WEB_PASSWORD")
    };
    std::env::var(primary)
        .or_else(|_| std::env::var(legacy))
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn allowed_hosts(config: &AppConfig) -> Vec<String> {
    let (primary, legacy) = if config.service_name == "rpi-web" {
        ("RPI_WEB_ALLOWED_HOSTS", "RPI_HTTP_ALLOWED_HOSTS")
    } else {
        ("RPI_HTTP_ALLOWED_HOSTS", "RPI_WEB_ALLOWED_HOSTS")
    };
    std::env::var(primary)
        .or_else(|_| std::env::var(legacy))
        .ok()
        .map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|host| !host.is_empty())
                .map(str::to_string)
                .collect()
        })
        .filter(|hosts: &Vec<String>| !hosts.is_empty())
        .or_else(|| config.allowed_hosts.clone())
        .unwrap_or_else(|| {
            let mut hosts = vec![
                format!("127.0.0.1:{}", config.port),
                format!("localhost:{}", config.port),
            ];
            if let Ok(ip) = config.bind.parse::<std::net::IpAddr>() {
                if ip.is_ipv6() {
                    hosts.push(format!("[::1]:{}", config.port));
                }
                if !ip.is_unspecified() {
                    let authority = SocketAddr::new(ip, config.port).to_string();
                    if !hosts.contains(&authority) {
                        hosts.push(authority);
                    }
                }
            }
            hosts
        })
}

async fn auth_status(State(state): State<AppState>, headers: axum::http::HeaderMap) -> Json<Value> {
    security::status_for(&state.security, &headers).await
}

async fn auth_login(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(request): Json<security::LoginRequest>,
) -> impl IntoResponse {
    security::login_for(&state.security, &headers, request).await
}

async fn auth_logout(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    security::logout_for(&state.security, &headers).await
}

async fn health(State(state): State<AppState>) -> Json<Value> {
    Json(json!({"ok":true,"service":state.service_name,"protocol":"rpi"}))
}
async fn capabilities() -> Json<Value> {
    Json(protocol::capabilities())
}
async fn list_projects(State(state): State<AppState>) -> axum::response::Response {
    HttpReply(service::list_projects(&state.backend).await.into_reply()).into_response()
}

async fn list_project_folders(
    State(state): State<AppState>,
    Query(query): Query<ProjectFoldersQuery>,
) -> axum::response::Response {
    HttpReply(
        service::list_project_folders(&state.backend, query)
            .await
            .into_reply(),
    )
    .into_response()
}

async fn create_project(
    State(state): State<AppState>,
    Json(request): Json<CreateProjectRequest>,
) -> axum::response::Response {
    HttpReply(
        service::create_project(&state.backend, request)
            .await
            .into_reply(),
    )
    .into_response()
}

async fn project_files(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<FileQuery>,
) -> axum::response::Response {
    HttpReply(
        service::project_files(&state.backend, id, query)
            .await
            .into_reply(),
    )
    .into_response()
}

async fn project_file(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<FileQuery>,
) -> axum::response::Response {
    HttpReply(
        service::project_file(&state.backend, id, query)
            .await
            .into_reply(),
    )
    .into_response()
}

async fn project_status(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> axum::response::Response {
    HttpReply(
        service::project_status(&state.backend, id)
            .await
            .into_reply(),
    )
    .into_response()
}

async fn project_diff(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<FileQuery>,
) -> axum::response::Response {
    HttpReply(
        service::project_diff(&state.backend, id, query)
            .await
            .into_reply(),
    )
    .into_response()
}

async fn refresh_auth(
    State(state): State<AppState>,
    Json(request): Json<AuthRefreshRequest>,
) -> axum::response::Response {
    HttpReply(
        service::refresh_auth(&state.backend, request)
            .await
            .into_reply(),
    )
    .into_response()
}

async fn create_session(
    State(state): State<AppState>,
    body: axum::body::Bytes,
) -> axum::response::Response {
    let request: CreateSessionRequest = match optional_json(&body) {
        Ok(request) => request,
        Err(error) => return error.into_response(),
    };
    HttpReply(
        service::create_session(&state.backend, request)
            .await
            .into_reply(),
    )
    .into_response()
}

async fn rename_session(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(request): Json<RenameRequest>,
) -> axum::response::Response {
    HttpReply(
        service::rename_session(&state.backend, id, request)
            .await
            .into_reply(),
    )
    .into_response()
}

async fn session_metadata(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(request): Json<MetadataRequest>,
) -> axum::response::Response {
    HttpReply(
        service::session_metadata(&state.backend, id, request)
            .await
            .into_reply(),
    )
    .into_response()
}

async fn fork_session(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(request): Json<ForkRequest>,
) -> axum::response::Response {
    HttpReply(
        service::fork_session(&state.backend, id, request)
            .await
            .into_reply(),
    )
    .into_response()
}

async fn clone_session(
    State(state): State<AppState>,
    Path(id): Path<String>,
    body: axum::body::Bytes,
) -> axum::response::Response {
    let request: ForkRequest = match optional_json(&body) {
        Ok(request) => request,
        Err(error) => return error.into_response(),
    };
    HttpReply(
        service::clone_session(&state.backend, id, request)
            .await
            .into_reply(),
    )
    .into_response()
}

async fn open_session(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> axum::response::Response {
    HttpReply(service::open_session(&state.backend, id).await.into_reply()).into_response()
}

async fn list_sessions(
    State(state): State<AppState>,
    Query(query): Query<SessionListQuery>,
) -> axum::response::Response {
    HttpReply(
        service::list_sessions(&state.backend, query)
            .await
            .into_reply(),
    )
    .into_response()
}

async fn get_session(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> axum::response::Response {
    HttpReply(service::get_session(&state.backend, id).await.into_reply()).into_response()
}

async fn delete_session(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> axum::response::Response {
    HttpReply(
        service::delete_session(&state.backend, id)
            .await
            .into_reply(),
    )
    .into_response()
}

async fn command(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(command): Json<ApiCommand>,
) -> axum::response::Response {
    HttpReply(
        service::command(&state.backend, id, command)
            .await
            .into_reply(),
    )
    .into_response()
}

async fn snapshot(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> axum::response::Response {
    HttpReply(service::snapshot(&state.backend, id).await.into_reply()).into_response()
}

// An absent body keeps the legacy default, but a malformed supplied body must
// never turn into a successful creation with silently discarded parameters.
fn optional_json<T: DeserializeOwned + Default>(
    body: &[u8],
) -> Result<T, (StatusCode, Json<Value>)> {
    if body.is_empty() {
        return Ok(T::default());
    }
    serde_json::from_slice(body).map_err(|_| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"invalid JSON request body"})),
        )
    })
}

#[derive(Debug, serde::Deserialize)]
struct EventQuery {
    after: Option<u64>,
}

async fn events(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: axum::http::HeaderMap,
    Query(query): Query<EventQuery>,
) -> axum::response::Response {
    let after = headers
        .get("last-event-id")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .or(query.after);
    let Some(subscription) = state.backend.subscribe_events(&id, after).await else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error":"session not found"})),
        )
            .into_response();
    };
    let mut shutdown = state.shutdown;
    Sse::new(
        authorized_events(
            event_stream(
                subscription.ready,
                subscription.replay,
                subscription.receiver,
                subscription.gap,
                subscription.after,
            ),
            state.security,
            headers,
        )
        .take_until(async move {
            if !*shutdown.borrow_and_update() {
                let _ = shutdown.changed().await;
            }
        }),
    )
    .keep_alive(axum::response::sse::KeepAlive::default())
    .into_response()
}

fn event_stream(
    ready: Value,
    replay: Vec<BufferedEvent>,
    receiver: broadcast::Receiver<BufferedEvent>,
    gap: bool,
    after: Option<u64>,
) -> impl Stream<Item = Result<Event, Infallible>> {
    let head: Vec<Result<Event, Infallible>> =
        std::iter::once(Ok(Event::default().event("ready").data(ready.to_string())))
            .chain(gap.then(|| {
                Ok(Event::default()
                    .event("resync_required")
                    .data("{\"type\":\"resync_required\"}"))
            }))
            .collect();
    stream::iter(head).chain(rpi_extcore::events::replay_stream(replay,receiver,after).map(|item| {
        Ok(match item {
            rpi_extcore::events::StreamItem::Event(event)=>to_sse(event),
            rpi_extcore::events::StreamItem::ResyncRequired{dropped}=>Event::default().event("resync_required").data(json!({"type":"resync_required","code":"event_stream_lagged","dropped":dropped}).to_string()),
        })
    }))
}

// The middleware checks only the initial HTTP request. A long-lived stream must
// also stop after another tab logs out or the authentication session expires.
fn authorized_events(
    events: impl Stream<Item = Result<Event, Infallible>> + Send + 'static,
    security: Arc<Security>,
    headers: axum::http::HeaderMap,
) -> impl Stream<Item = Result<Event, Infallible>> {
    let expiry_check = tokio::time::interval(std::time::Duration::from_secs(10));
    stream::unfold(
        (Box::pin(events), expiry_check),
        move |(mut events, mut expiry_check)| {
            let security = security.clone();
            let headers = headers.clone();
            async move {
                loop {
                    tokio::select! {
                        event = events.next() => {
                            let event = event?;
                            if !security.authorized(&headers).await { return None; }
                            return Some((event, (events, expiry_check)));
                        }
                        _ = expiry_check.tick() => {
                            if !security.authorized(&headers).await { return None; }
                        }
                    }
                }
            }
        },
    )
}

fn to_sse(mut event: BufferedEvent) -> Event {
    rpi_extcore::timing::refresh_event(&mut event.value);
    let event_type = event
        .value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("message");
    Event::default()
        .event(event_type)
        .id(event.seq.to_string())
        .data(event.value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn optional_creation_bodies_reject_invalid_json_and_types() {
        assert!(optional_json::<CreateSessionRequest>(b"").is_ok());
        assert!(optional_json::<CreateSessionRequest>(br#"{"projectId":"project-a"}"#).is_ok());
        for body in [b"{".as_slice(), b"null", br#"{"projectId":42}"#] {
            assert_eq!(
                optional_json::<CreateSessionRequest>(body).unwrap_err().0,
                StatusCode::BAD_REQUEST
            );
        }
        assert!(optional_json::<ForkRequest>(b"").is_ok());
        assert!(optional_json::<ForkRequest>(br#"{"entryId":42}"#).is_err());
    }

    #[tokio::test]
    async fn live_events_do_not_duplicate_replay_or_the_snapshot_cursor() {
        for replay in [
            vec![],
            vec![BufferedEvent {
                seq: 5,
                value: json!({"type":"agent_start"}),
            }],
        ] {
            let (sender, receiver) = broadcast::channel(8);
            for seq in [4, 5, 6] {
                sender
                    .send(BufferedEvent {
                        seq,
                        value: json!({"type":"agent_start"}),
                    })
                    .unwrap();
            }
            drop(sender);
            let has_replay = !replay.is_empty();
            let stream = event_stream(json!({"type":"ready"}), replay, receiver, false, Some(5));
            let body = axum::body::to_bytes(Sse::new(stream).into_response().into_body(), 4096)
                .await
                .unwrap();
            let text = String::from_utf8(body.to_vec()).unwrap();
            assert!(!text.contains("id: 4\n"));
            assert_eq!(text.matches("id: 5\n").count(), usize::from(has_replay));
            assert_eq!(text.matches("id: 6\n").count(), 1);
        }
    }

    #[tokio::test]
    async fn logout_revokes_an_already_open_event_stream() {
        let security = Arc::new(Security::new(["example.test".into()], Some("secret")));
        let token = security.login("secret").await.unwrap();
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            axum::http::header::COOKIE,
            format!("rpi_web_session={token}").parse().unwrap(),
        );
        let (sender, receiver) = broadcast::channel(8);
        let stream = authorized_events(
            event_stream(json!({"type":"ready"}), vec![], receiver, false, None),
            security.clone(),
            headers.clone(),
        );
        futures_util::pin_mut!(stream);
        assert!(stream.next().await.is_some());
        security.logout(&headers).await;
        sender
            .send(BufferedEvent {
                seq: 1,
                value: json!({"type":"message_end","message":"private"}),
            })
            .unwrap();
        assert!(stream.next().await.is_none());
    }
}
