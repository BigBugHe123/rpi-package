use crate::config::{self, Launch};
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::{routing::get, Router};
use futures_util::SinkExt;
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{watch, Semaphore};

const MAX_MESSAGE: usize = 4 * 1024 * 1024;

pub(crate) struct ServerState {
    pub token: String,
    pub executable: PathBuf,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub stop: watch::Receiver<bool>,
    pub slots: Arc<Semaphore>,
}

pub(crate) fn router(state: Arc<ServerState>) -> Router {
    Router::new()
        .route("/health", get(|| async { "rpi-ui-adapter" }))
        .route("/acp", get(upgrade))
        .with_state(state)
}

async fn upgrade(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    // This endpoint is called by the Open WebUI backend, not the browser. No
    // CORS or token-in-query workaround is needed.
    if headers.get("authorization").and_then(|s| s.to_str().ok())
        != Some(format!("Bearer {}", state.token).as_str())
    {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let Ok(slot) = Arc::clone(&state.slots).try_acquire_owned() else {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    };
    ws.max_message_size(MAX_MESSAGE)
        .max_frame_size(MAX_MESSAGE)
        .on_upgrade(move |socket| async move {
            let _slot = slot;
            if let Err(error) = relay(socket, state).await {
                eprintln!("[rpi-ui-adapter] ACP connection: {error}");
            }
        })
}

async fn relay(mut socket: WebSocket, state: Arc<ServerState>) -> Result<(), String> {
    let mut child = tokio::process::Command::new(&state.executable)
        .args(&state.args)
        .current_dir(&state.cwd)
        .env("RPI_ACP_NO_AUTO_REGISTER_ZED", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("Cannot start ACP agent: {e}"))?;
    let mut input = child.stdin.take().ok_or("ACP stdin unavailable")?;
    let output = child.stdout.take().ok_or("ACP stdout unavailable")?;
    let mut output = BufReader::new(output);
    let mut line = Vec::new();
    let mut stop = state.stop.clone();
    let mut active_prompts = HashMap::new();
    let result = async {
        loop {
            tokio::select! {
                _ = stop.changed() => break,
                message = socket.recv() => match message {
                    Some(Ok(Message::Text(text))) => {
                        // ACP framing is one JSON object per WebSocket message.
                        // Re-serialize to prohibit embedded newlines injecting a
                        // second stdio request. No tools are executed here.
                        let value: serde_json::Value = serde_json::from_str(&text).map_err(|_| "Invalid ACP JSON")?;
                        if !value.is_object() { return Err("ACP message must be an object".into()); }
                        if value["method"] == "session/prompt" {
                            if let (Some(id), Some(sid)) = (value.get("id"), value.pointer("/params/sessionId").and_then(|v| v.as_str())) {
                                active_prompts.insert(id.to_string(), sid.to_owned());
                            }
                        }
                        let mut data = serde_json::to_vec(&value).map_err(|e| e.to_string())?;
                        data.push(b'\n');
                        input.write_all(&data).await.map_err(|e| e.to_string())?;
                        input.flush().await.map_err(|e| e.to_string())?;
                    }
                    Some(Ok(Message::Ping(data))) => { socket.send(Message::Pong(data)).await.map_err(|e| e.to_string())?; }
                    Some(Ok(Message::Pong(_))) => {},
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(_)) => return Err("Only text ACP messages are accepted".into()),
                    Some(Err(e)) => return Err(e.to_string()),
                },
                read = read_line_limited(&mut output, &mut line) => {
                    if read.map_err(|e| e.to_string())? == 0 { break; }
                    let value: serde_json::Value = serde_json::from_slice(&line).map_err(|_| "Agent stdout is not ACP JSON")?;
                    if value.get("method").is_none() {
                        if let Some(id) = value.get("id") { active_prompts.remove(&id.to_string()); }
                    }
                    socket.send(Message::Text(value.to_string().into())).await.map_err(|e| e.to_string())?;
                    line.clear();
                }
            }
        }
        Ok(())
    }.await;
    // A UI server may close its stream before a generator's cancellation
    // callback runs. Cancel unfinished prompts on transport close as well.
    for sid in active_prompts.values() {
        let data = serde_json::json!({"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":sid}});
        let _ = input.write_all(format!("{data}\n").as_bytes()).await;
    }
    let _ = input.flush().await;
    // Keep stdin open briefly so the agent can process cancel and acknowledge
    // the prompt before EOF shuts down its RPC child.
    let _ = tokio::time::timeout(Duration::from_secs(2), async {
        while !active_prompts.is_empty() {
            line.clear();
            if read_line_limited(&mut output, &mut line).await? == 0 {
                break;
            }
            if let Ok(value) = serde_json::from_slice::<serde_json::Value>(&line) {
                if value.get("method").is_none() {
                    if let Some(id) = value.get("id") {
                        active_prompts.remove(&id.to_string());
                    }
                }
            }
        }
        Ok::<_, std::io::Error>(())
    })
    .await;
    // EOF lets rpi-acp retire dialogs and stop its RPC child. Killing the ACP
    // parent immediately would orphan that child on some platforms.
    drop(input);
    if tokio::time::timeout(Duration::from_secs(5), child.wait())
        .await
        .is_err()
    {
        let _ = child.kill().await;
    }
    let _ = socket.close().await;
    result
}

async fn read_line_limited<R: tokio::io::AsyncBufRead + Unpin>(
    reader: &mut R,
    line: &mut Vec<u8>,
) -> std::io::Result<usize> {
    // read_until without a bound could grow indefinitely on malformed stdout.
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return Ok(line.len());
        }
        let count = available
            .iter()
            .position(|b| *b == b'\n')
            .map_or(available.len(), |i| i + 1);
        if line.len() + count > MAX_MESSAGE {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "ACP line too large",
            ));
        }
        let done = available[count - 1] == b'\n';
        line.extend_from_slice(&available[..count]);
        reader.consume(count);
        if done {
            return Ok(line.len());
        }
    }
}

pub async fn run(mut launch: Launch, mut stop: watch::Receiver<bool>) -> Result<(), String> {
    let listener = tokio::net::TcpListener::bind(launch.config.bind)
        .await
        .map_err(|e| format!("Cannot bind UI adapter: {e}"))?;
    let state = Arc::new(ServerState {
        token: launch.config.adapter_token.clone(),
        executable: launch.executable,
        args: launch.args,
        cwd: launch.config.cwd.clone(),
        stop: stop.clone(),
        slots: Arc::new(Semaphore::new(8)),
    });
    let (server_stop, mut server_receiver) = watch::channel(false);
    let mut task = tokio::spawn(async move {
        axum::serve(listener, router(state))
            .with_graceful_shutdown(async move {
                let _ = server_receiver.changed().await;
            })
            .await
    });
    let configured = async {
        if let Some(token) = &launch.admin_token {
            crate::openwebui::install(&launch.config, token, &launch.path).await?;
            launch.config.installed = true;
            launch.config.installed_version = env!("CARGO_PKG_VERSION").into();
        }
        config::save(&launch.path, &launch.config)?;
        eprintln!(
            "[rpi-ui-adapter] Open WebUI: {} (model rpi_ui_adapter)",
            launch.config.url
        );
        eprintln!(
            "[rpi-ui-adapter] Listening on {}; project {}",
            launch.config.bind,
            launch.config.cwd.display()
        );
        Ok::<_, String>(())
    }
    .await;
    if let Err(error) = configured {
        let _ = server_stop.send(true);
        let _ = task.await;
        return Err(error);
    }
    tokio::select! {
        _ = stop.changed() => {},
        result = &mut task => return result.map_err(|e| e.to_string())?.map_err(|e| e.to_string()),
    }
    let _ = server_stop.send(true);
    // Connections also observe `stop` and close their ACP process gracefully.
    if tokio::time::timeout(Duration::from_secs(8), &mut task)
        .await
        .is_err()
    {
        task.abort();
    }
    Ok(())
}
