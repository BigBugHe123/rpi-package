//! Load the real bundle and verify transport controls without a model service.
use std::{path::PathBuf, sync::Arc, time::Duration};

use rpi_ai::{
    AssistantMessage, Content, Context, Message, Model, Provider, SimpleStreamOptions, StopReason,
    StreamingProtocolCompat, ThinkingLevel,
};
use serde_json::{json, Value};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::oneshot,
};

fn library_path() -> PathBuf {
    let profile = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let filename = format!(
        "{}rpi_providers{}",
        std::env::consts::DLL_PREFIX,
        std::env::consts::DLL_SUFFIX
    );
    [profile.join(&filename), profile.join("deps").join(filename)]
        .into_iter()
        .find(|path| path.is_file())
        .expect("first cargo build -p rpi-providers")
}

fn providers() -> (rpi_extensions::ExtensionSession, Vec<Arc<dyn Provider>>) {
    let session = rpi_extensions::load_session_mixed(
        &[],
        &[library_path()],
        Arc::new(rpi_extensions::NullDiagnostics),
        None,
    );
    let providers = rpi_extensions::PluggableProvider::from_session(
        &session,
        tokio::runtime::Handle::current(),
    );
    assert_eq!(providers.len(), 4);
    (session, providers)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires the matching host in RPI_TEST_CLI; loopback HTTP only"]
async fn host_retries_extension_stalls_and_checks_compat_after_manifest_defaults() {
    let host = std::env::var_os("RPI_TEST_CLI").expect("set RPI_TEST_CLI");
    let root = tempfile::tempdir().unwrap();
    let agent = root.path().join("agent");
    std::fs::create_dir(&agent).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    std::fs::write(agent.join("models.json"), json!({"providers":{"proxy":{
        "apiKey":"fixture-only", "baseUrl":format!("http://{address}/v1"),
        "models":[{"id":"custom", "reasoning":true, "contextWindow":131072, "maxTokens":128,
            "compat":{"thinkingFormat":"deepseek", "requiresReasoningContentOnAssistantMessages":true,
                "misspelledFlag":true}}]
    }}}).to_string()).unwrap();
    std::fs::write(
        agent.join("settings.json"),
        json!({
            "compaction":{"enabled":false}, "retry":{"enabled":true,"maxRetries":1,"baseDelayMs":0}
        })
        .to_string(),
    )
    .unwrap();
    let server = tokio::spawn(async move {
        let mut stalled = None;
        for attempt in 0..2 {
            let (mut socket, _) = listener.accept().await.unwrap();
            let request = read_request(&mut socket).await;
            assert_eq!(request["thinking"]["type"], "enabled");
            if attempt == 0 {
                socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n").await.unwrap();
                let data = frame("partial", false);
                socket
                    .write_all(format!("{:x}\r\n{data}\r\n", data.len()).as_bytes())
                    .await
                    .unwrap();
                stalled = Some(socket);
            } else {
                let data = format!(
                    "{}data: [DONE]\n\n",
                    frame("Recovered through extension.", true)
                );
                socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{data}", data.len()).as_bytes()).await.unwrap();
            }
        }
        drop(stalled);
    });
    let mut command = tokio::process::Command::new(host);
    command
        .current_dir(root.path())
        .env("RPI_CODING_AGENT_DIR", agent)
        .env_remove("HTTP_PROXY")
        .env_remove("HTTPS_PROXY")
        .env_remove("ALL_PROXY")
        .arg("--extension")
        .arg(library_path())
        .args([
            "--offline",
            "--package-only",
            "--no-session",
            "--no-builtin-tools",
            "--no-skills",
            "--no-context-files",
            "--no-prompt-templates",
            "--provider",
            "proxy",
            "--model",
            "custom",
            "--thinking",
            "high",
            "--print",
            "--timeout=10",
            "--stream-idle-timeout=1",
            "hello",
        ])
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
    let output = tokio::time::timeout(Duration::from_secs(8), command.output())
        .await
        .unwrap()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    assert!(String::from_utf8_lossy(&output.stdout).contains("Recovered through extension."));
    assert!(
        stderr.contains("unknown field \"misspelledFlag\""),
        "{stderr}"
    );
    tokio::time::timeout(Duration::from_secs(2), server)
        .await
        .unwrap()
        .unwrap();
}

async fn read_request(socket: &mut TcpStream) -> Value {
    let mut bytes = Vec::new();
    loop {
        let mut buffer = [0; 4096];
        let count = socket.read(&mut buffer).await.unwrap();
        assert_ne!(count, 0);
        bytes.extend_from_slice(&buffer[..count]);
        assert!(bytes.len() < 1_000_000);
        if let Some(end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            let headers = String::from_utf8_lossy(&bytes[..end]).to_ascii_lowercase();
            let length: usize = headers
                .lines()
                .find_map(|line| line.strip_prefix("content-length:"))
                .unwrap()
                .trim()
                .parse()
                .unwrap();
            if bytes.len() >= end + 4 + length {
                return serde_json::from_slice(&bytes[end + 4..end + 4 + length]).unwrap();
            }
        }
    }
}

fn frame(text: &str, terminal: bool) -> String {
    format!(
        "data: {}\n\n",
        json!({"choices":[{"index":0,"delta":{"content":text},
        "finish_reason": if terminal { json!("stop") } else { Value::Null }}]})
    )
}

struct Fixture {
    url: String,
    request: oneshot::Receiver<Value>,
    stop: Option<oneshot::Sender<()>>,
    server: tokio::task::JoinHandle<()>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}

impl Fixture {
    async fn new(chunks: Vec<(Duration, String)>, finish: bool) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/v1", listener.local_addr().unwrap());
        let (request_tx, request) = oneshot::channel();
        let (stop, stop_rx) = oneshot::channel();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            request_tx.send(read_request(&mut socket).await).unwrap();
            socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n").await.unwrap();
            for (delay, chunk) in chunks {
                tokio::time::sleep(delay).await;
                let wire = format!("{:x}\r\n{chunk}\r\n", chunk.len());
                if socket.write_all(wire.as_bytes()).await.is_err() {
                    return;
                }
            }
            if finish {
                // The consumer may close as soon as it reads [DONE].
                let _ = socket
                    .write_all(b"e\r\ndata: [DONE]\n\n\r\n0\r\n\r\n")
                    .await;
            } else {
                let _ = stop_rx.await;
            }
        });
        Self {
            url,
            request,
            stop: Some(stop),
            server,
        }
    }

    async fn finish(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        tokio::time::timeout(Duration::from_secs(3), &mut self.server)
            .await
            .unwrap()
            .unwrap();
    }
}

fn model(provider: &dyn Provider, fixture: &Fixture) -> Model {
    let mut model = provider.models()[0].clone();
    model.base_url = fixture.url.clone();
    model.max_tokens = 128;
    model
}

fn options(idle: Duration) -> SimpleStreamOptions {
    SimpleStreamOptions {
        api_key: Some("fixture-only".into()),
        timeout: Some(Duration::from_secs(3)),
        stream_idle_timeout: Some(idle),
        max_tokens: Some(128),
        ..Default::default()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_loaded_provider_interrupts_stalled_streams_before_the_total_deadline() {
    let (_session, providers) = providers();
    for provider in providers {
        let mut fixture =
            Fixture::new(vec![(Duration::ZERO, frame("partial", false))], false).await;
        let options = options(Duration::from_millis(100));
        let stream = provider
            .stream_simple(
                &model(provider.as_ref(), &fixture),
                &Context::new(vec![]),
                &options,
            )
            .await;
        let message = tokio::time::timeout(Duration::from_secs(2), stream.result())
            .await
            .expect("the plugin must honor idle timeout before the 3-second total deadline")
            .unwrap();
        assert_eq!(message.stop_reason, StopReason::Error, "{}", provider.id());
        assert!(
            message
                .error_message
                .as_deref()
                .unwrap()
                .contains("stream idle timeout"),
            "{message:?}"
        );
        fixture.finish().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_loaded_provider_keeps_active_fragmented_streams_alive() {
    let (_session, providers) = providers();
    for provider in providers {
        let data = frame("healthy", true);
        let split = data.len() / 2;
        let chunks = vec![
            (Duration::ZERO, "event: ping\n\ndata: \n\n".into()),
            (Duration::from_millis(200), ": ping\n\n".into()),
            (Duration::from_millis(200), data[..split].into()),
            (Duration::from_millis(200), data[split..].into()),
        ];
        let mut fixture = Fixture::new(chunks, true).await;
        let stream = provider
            .stream_simple(
                &model(provider.as_ref(), &fixture),
                &Context::new(vec![]),
                &options(Duration::from_millis(500)),
            )
            .await;
        let message = tokio::time::timeout(Duration::from_secs(2), stream.result())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(message.stop_reason, StopReason::Stop, "{message:?}");
        assert_eq!(Content::text_only(&message.content, ""), "healthy");
        fixture.finish().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn loaded_provider_allows_disabling_and_cancelling_the_idle_watchdog() {
    let (_session, providers) = providers();
    for provider in providers {
        let mut fixture = Fixture::new(
            vec![(Duration::from_millis(150), frame("late", true))],
            true,
        )
        .await;
        let stream = provider
            .stream_simple(
                &model(provider.as_ref(), &fixture),
                &Context::new(vec![]),
                &options(Duration::ZERO),
            )
            .await;
        let message = tokio::time::timeout(Duration::from_secs(2), stream.result())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(message.stop_reason, StopReason::Stop);
        fixture.finish().await;

        let mut fixture =
            Fixture::new(vec![(Duration::ZERO, frame("partial", false))], false).await;
        let options = options(Duration::from_secs(1));
        let stream = provider
            .stream_simple(
                &model(provider.as_ref(), &fixture),
                &Context::new(vec![]),
                &options,
            )
            .await;
        tokio::time::timeout(Duration::from_secs(2), &mut fixture.request)
            .await
            .unwrap()
            .unwrap();
        options.signal.cancel();
        let message = tokio::time::timeout(Duration::from_secs(2), stream.result())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(message.stop_reason, StopReason::Aborted, "{message:?}");
        fixture.finish().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_loaded_provider_preserves_compat_and_replayed_reasoning() {
    let (_session, providers) = providers();
    for provider in providers {
        let mut fixture = Fixture::new(vec![(Duration::ZERO, frame("done", true))], true).await;
        let mut model = model(provider.as_ref(), &fixture);
        model.reasoning = true;
        model.compat = Some(StreamingProtocolCompat::OpenaiCompletions(
            serde_json::from_value(json!({
                "thinkingFormat":"deepseek", "requiresReasoningContentOnAssistantMessages":true
                , "supportsReasoningEffort":false
            }))
            .unwrap(),
        ));
        let mut thinking =
            AssistantMessage::empty(model.api.clone(), &model.provider, &model.id, 1);
        thinking.content = vec![
            Content::thinking("retained reasoning"),
            Content::text("answer"),
        ];
        let mut plain = thinking.clone();
        plain.content = vec![Content::text("plain answer")];
        let context = Context::new(vec![
            Message::Assistant(Box::new(thinking)),
            Message::Assistant(Box::new(plain)),
        ]);
        let mut options = options(Duration::from_secs(1));
        options.reasoning = Some(ThinkingLevel::High);
        if provider.id() == "deepseek" {
            options.max_tokens = None;
        }
        let stream = provider.stream_simple(&model, &context, &options).await;
        let message = tokio::time::timeout(Duration::from_secs(2), stream.result())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(message.stop_reason, StopReason::Stop, "{message:?}");
        let request = (&mut fixture.request).await.unwrap();
        assert_eq!(request["thinking"]["type"], "enabled", "{}", provider.id());
        assert!(
            request.get("reasoning_effort").is_none(),
            "{}",
            provider.id()
        );
        if provider.id() == "deepseek" {
            assert!(request.get("store").is_none());
            assert!(
                request["max_tokens"].as_u64().unwrap() > model.max_tokens,
                "partial compat must preserve DeepSeek's separate thinking allowance"
            );
        }
        assert_eq!(
            request["messages"][0]["reasoning_content"],
            "retained reasoning"
        );
        assert_eq!(request["messages"][1]["reasoning_content"], "");
        fixture.finish().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deepseek_preserves_explicit_thinking_format_and_effort_mapping() {
    let (_session, providers) = providers();
    let provider = providers
        .iter()
        .find(|provider| provider.id() == "deepseek")
        .unwrap();
    let mut fixture = Fixture::new(vec![(Duration::ZERO, frame("done", true))], true).await;
    let mut model = model(provider.as_ref(), &fixture);
    model.thinking_level_map = Some(std::collections::BTreeMap::from([(
        ThinkingLevel::High,
        Some("low".into()),
    )]));
    model.compat = Some(StreamingProtocolCompat::OpenaiCompletions(
        serde_json::from_value(json!({"thinkingFormat":"openai"})).unwrap(),
    ));
    let mut options = options(Duration::from_secs(1));
    options.reasoning = Some(ThinkingLevel::High);
    let stream = provider
        .stream_simple(&model, &Context::new(vec![]), &options)
        .await;
    let message = tokio::time::timeout(Duration::from_secs(2), stream.result())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(message.stop_reason, StopReason::Stop, "{message:?}");
    let request = (&mut fixture.request).await.unwrap();
    assert!(request.get("thinking").is_none());
    assert_eq!(request["reasoning_effort"], "low");
    fixture.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deepseek_disables_thinking_for_custom_non_reasoning_models() {
    let (_session, providers) = providers();
    let provider = providers
        .iter()
        .find(|provider| provider.id() == "deepseek")
        .unwrap();
    for requested in [None, Some(ThinkingLevel::High)] {
        let mut fixture = Fixture::new(vec![(Duration::ZERO, frame("done", true))], true).await;
        let mut model = model(provider.as_ref(), &fixture);
        // models.json entries without `reasoning` default to false. Preserve
        // explicit thinking=off even if the remote endpoint defaults it on.
        model.reasoning = false;
        let mut options = options(Duration::from_secs(1));
        options.reasoning = requested;
        let stream = provider
            .stream_simple(&model, &Context::new(vec![]), &options)
            .await;
        let message = tokio::time::timeout(Duration::from_secs(2), stream.result())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(message.stop_reason, StopReason::Stop, "{message:?}");
        let request = (&mut fixture.request).await.unwrap();
        assert_eq!(request["thinking"]["type"], "disabled");
        assert!(request.get("reasoning_effort").is_none());
        fixture.finish().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn proxy_config_timeout_is_a_fallback_and_request_options_take_precedence() {
    use rpi_providers::proxy::{ProxyConfig, ProxyProvider};
    let mut fixture = Fixture::new(vec![(Duration::ZERO, frame("partial", false))], false).await;
    let provider =
        ProxyProvider::new(ProxyConfig::new(&fixture.url).with_timeout(Duration::from_millis(50)));
    let model = Model::new(
        "test",
        "Test",
        rpi_ai::Api::OpenaiCompletions,
        "proxy",
        &fixture.url,
    );
    let mut options = options(Duration::ZERO);
    options.timeout = None;
    let stream = provider
        .stream_simple(&model, &Context::new(vec![]), &options)
        .await;
    let message = tokio::time::timeout(Duration::from_secs(2), stream.result())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(message.stop_reason, StopReason::Error);
    assert!(message.error_message.unwrap().contains("timed out"));
    fixture.finish().await;

    let mut fixture = Fixture::new(
        vec![(Duration::from_millis(150), frame("override", true))],
        true,
    )
    .await;
    let provider =
        ProxyProvider::new(ProxyConfig::new(&fixture.url).with_timeout(Duration::from_millis(20)));
    let mut model = model;
    model.base_url = fixture.url.clone();
    options.timeout = Some(Duration::from_secs(1));
    let stream = provider
        .stream_simple(&model, &Context::new(vec![]), &options)
        .await;
    assert_eq!(stream.result().await.unwrap().stop_reason, StopReason::Stop);
    fixture.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_loaded_provider_keeps_the_total_deadline_independent_of_idle_timeout() {
    let (_session, providers) = providers();
    for provider in providers {
        let mut fixture =
            Fixture::new(vec![(Duration::ZERO, frame("partial", false))], false).await;
        let mut options = options(Duration::from_secs(1));
        options.timeout = Some(Duration::from_millis(50));
        let stream = provider
            .stream_simple(
                &model(provider.as_ref(), &fixture),
                &Context::new(vec![]),
                &options,
            )
            .await;
        let message = tokio::time::timeout(Duration::from_secs(2), stream.result())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(message.stop_reason, StopReason::Error);
        let error = message.error_message.unwrap();
        assert!(error.contains("timed out"), "{error}");
        assert!(!error.contains("stream idle timeout"), "{error}");
        fixture.finish().await;
    }
}
