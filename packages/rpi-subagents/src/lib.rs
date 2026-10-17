#![forbid(unsafe_code)]
// `export_plugin!` generates the C-ABI entrypoint whose raw pointer argument is
// validated and dereferenced inside the SDK's unsafe helper.
#![allow(clippy::not_unsafe_ptr_arg_deref)]

mod kit;

use crate::kit::{plugin_free_string, schema};
use rpi_plugin_sdk::handles::ToolHandles;
use rpi_plugin_sdk::process::ProcessTree;
use rpi_plugin_sdk::{
    FreeStringFn, StbString, StbStringRef, StepHandle, StepResult, ToolPartialCb,
};
use serde_json::Value;
use std::io::{self, BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

const MAX_TASK_BYTES: usize = 20_000;
const MAX_ROLE_BYTES: usize = 200;
const DEFAULT_MAX_TURNS: u64 = 8;
const DEFAULT_TIMEOUT_SECONDS: u64 = 300;
const MAX_TURNS: u64 = 32;
const MIN_TIMEOUT_SECONDS: u64 = 10;
const MAX_TIMEOUT_SECONDS: u64 = 1_800;
const MAX_OUTPUT_BYTES: usize = 4 * 1024 * 1024;
const MAX_PARTIAL_EVENTS: usize = 8 * 1024;
const MAX_RPC_LINE_BYTES: usize = MAX_OUTPUT_BYTES + 64 * 1024;

static JOBS: ToolHandles<Job> = ToolHandles::new();

type JobSender = Sender<JobMessage>;
type JobReceiver = Receiver<JobMessage>;

enum JobMessage {
    Partial(String),
    Done(String),
    Error(String),
}

struct Job {
    receiver: JobReceiver,
    completed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Request {
    task: String,
    role: String,
    max_turns: u64,
    timeout_seconds: u64,
}

fn parse_request(params: &Value) -> Result<Request, String> {
    let task = params
        .get("task")
        .and_then(Value::as_str)
        .ok_or_else(|| "missing string parameter `task`".to_string())?;
    if task.trim().is_empty() || task.len() > MAX_TASK_BYTES {
        return Err(format!("task must contain 1-{MAX_TASK_BYTES} bytes"));
    }

    let role = params
        .get("role")
        .and_then(Value::as_str)
        .unwrap_or("worker")
        .trim();
    if role.is_empty() || role.len() > MAX_ROLE_BYTES {
        return Err(format!("role must contain 1-{MAX_ROLE_BYTES} bytes"));
    }

    let max_turns = params
        .get("maxTurns")
        .and_then(Value::as_u64)
        .unwrap_or(DEFAULT_MAX_TURNS)
        .clamp(1, MAX_TURNS);
    let timeout_seconds = params
        .get("timeoutSeconds")
        .and_then(Value::as_u64)
        .unwrap_or(DEFAULT_TIMEOUT_SECONDS)
        .clamp(MIN_TIMEOUT_SECONDS, MAX_TIMEOUT_SECONDS);

    Ok(Request {
        task: task.to_owned(),
        role: role.to_owned(),
        max_turns,
        timeout_seconds,
    })
}

fn executable() -> String {
    if let Ok(value) = std::env::var("RPI_SUBAGENT_BIN") {
        if !value.trim().is_empty() {
            return value;
        }
    }
    if let Ok(current) = std::env::current_exe() {
        if current
            .file_stem()
            .and_then(|stem| stem.to_str())
            .is_some_and(|stem| stem.eq_ignore_ascii_case("rpi"))
        {
            return current.to_string_lossy().into_owned();
        }
        if let Some(parent) = current.parent() {
            let sibling = parent.join(format!("rpi{}", std::env::consts::EXE_SUFFIX));
            if sibling.is_file() {
                return sibling.to_string_lossy().into_owned();
            }
        }
    }
    "rpi".into()
}

fn command_args() -> Vec<String> {
    vec!["--mode".into(), "rpc".into(), "--no-session".into()]
}

fn child_prompt(request: &Request) -> String {
    format!(
        "You are a delegated {role} subagent. Complete the task below in at most {turns} agent turns. Do not delegate, spawn, or ask another agent to do work. Work only in the current project and return a concise final report.\n\nTask:\n{task}",
        role = request.role,
        turns = request.max_turns,
        task = request.task,
    )
}

fn send_command(stdin: &mut impl Write, value: Value) -> Result<(), String> {
    serde_json::to_writer(&mut *stdin, &value).map_err(|e| format!("encode RPC command: {e}"))?;
    stdin
        .write_all(b"\n")
        .map_err(|e| format!("write RPC command: {e}"))?;
    stdin.flush().map_err(|e| format!("flush RPC command: {e}"))
}

fn read_rpc_line(reader: &mut impl BufRead, max_bytes: usize) -> io::Result<Option<String>> {
    let mut line = Vec::new();
    loop {
        let buffer = reader.fill_buf()?;
        if buffer.is_empty() {
            if line.is_empty() {
                return Ok(None);
            }
            break;
        }

        let newline = buffer.iter().position(|byte| *byte == b'\n');
        let chunk_len = newline.map_or(buffer.len(), |index| index + 1);
        if line.len().saturating_add(chunk_len) > max_bytes {
            reader.consume(chunk_len);
            if newline.is_none() {
                loop {
                    let remainder = reader.fill_buf()?;
                    if remainder.is_empty() {
                        break;
                    }
                    let end = remainder
                        .iter()
                        .position(|byte| *byte == b'\n')
                        .map_or(remainder.len(), |index| index + 1);
                    let has_newline = end < remainder.len() || remainder[end - 1] == b'\n';
                    reader.consume(end);
                    if has_newline {
                        break;
                    }
                }
            }
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("subagent RPC line exceeds {max_bytes} bytes"),
            ));
        }

        line.extend_from_slice(&buffer[..chunk_len]);
        reader.consume(chunk_len);
        if newline.is_some() {
            break;
        }
    }

    String::from_utf8(line)
        .map(Some)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

fn text_delta(event: &Value) -> Option<String> {
    if event.get("type").and_then(Value::as_str) != Some("message_update") {
        return None;
    }
    event
        .get("delta")
        .and_then(Value::as_str)
        .or_else(|| {
            event
                .pointer("/assistantMessageEvent/delta")
                .and_then(Value::as_str)
        })
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RpcResult {
    outcome: String,
    final_text: String,
}

fn response_result(event: &Value, request_id: u64) -> Result<Option<RpcResult>, String> {
    if event.get("type").and_then(Value::as_str) != Some("response")
        || event.get("id").and_then(Value::as_u64) != Some(request_id)
    {
        return Ok(None);
    }
    if event.get("status").and_then(Value::as_str) != Some("ok") {
        return Err(event
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("subagent RPC request failed")
            .to_owned());
    }
    Ok(Some(RpcResult {
        outcome: event
            .pointer("/result/outcome")
            .and_then(Value::as_str)
            .unwrap_or("completed")
            .to_owned(),
        final_text: event
            .pointer("/result/finalText")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned(),
    }))
}

fn kill_child(child: &mut Child, process_tree: &ProcessTree) {
    // Kill the process tree before touching stdin. A stalled child may not be
    // reading its pipe, so an abort write must never delay cancellation.
    process_tree.kill();
    let _ = child.kill();
    let _ = child.wait();
}

fn send_child_command(stdin: &Arc<Mutex<ChildStdin>>, value: Value) -> Result<(), String> {
    let mut stdin = stdin
        .lock()
        .map_err(|_| "subagent stdin mutex poisoned".to_string())?;
    send_command(&mut *stdin, value)
}

fn send_abort_best_effort(
    child: &mut Child,
    process_tree: &ProcessTree,
    stdin: &Arc<Mutex<ChildStdin>>,
) -> Result<(), String> {
    let (result_tx, result_rx) = mpsc::channel();
    let abort_stdin = Arc::clone(stdin);
    let writer = thread::spawn(move || {
        let result =
            send_child_command(&abort_stdin, serde_json::json!({"id": 2, "type": "abort"}));
        let _ = result_tx.send(result);
    });

    match result_rx.recv_timeout(Duration::from_millis(100)) {
        Ok(result) => {
            let _ = writer.join();
            result
        }
        Err(RecvTimeoutError::Timeout) => {
            kill_child(child, process_tree);
            let _ = writer.join();
            Err("timed out sending subagent abort".into())
        }
        Err(RecvTimeoutError::Disconnected) => {
            kill_child(child, process_tree);
            let _ = writer.join();
            Err("subagent abort writer stopped".into())
        }
    }
}

fn run_request(request: Request, output: JobSender, cancelled: Arc<AtomicBool>) {
    let deadline = Instant::now() + Duration::from_secs(request.timeout_seconds);
    if cancelled.load(Ordering::Acquire) {
        let _ = output.send(JobMessage::Error("subagent cancelled".into()));
        return;
    }
    let mut command = Command::new(executable());
    command
        .args(command_args())
        // Keep provider and other useful extensions available, but make this
        // package decline registration in the child to prevent recursion.
        .env("RPI_SUBAGENT_CHILD", "1")
        // The harness enforces this at the end of each completed turn. The
        // turn_start handling below remains a defense for older RPC hosts.
        .env("RPI_MAX_TURNS_PER_RUN", request.max_turns.to_string())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    ProcessTree::prepare(&mut command);

    let spawn = command.spawn();
    let mut child = match spawn {
        Ok(child) => child,
        Err(error) => {
            let _ = output.send(JobMessage::Error(format!("spawn subagent: {error}")));
            return;
        }
    };
    let process_tree = match ProcessTree::attach(&child) {
        Ok(tree) => tree,
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            let _ = output.send(JobMessage::Error(format!(
                "attach subagent process tree: {error}"
            )));
            return;
        }
    };
    let Some(stdin) = child.stdin.take() else {
        let _ = output.send(JobMessage::Error("subagent stdin unavailable".into()));
        kill_child(&mut child, &process_tree);
        return;
    };
    let stdin = Arc::new(Mutex::new(stdin));
    let Some(stdout) = child.stdout.take() else {
        let _ = output.send(JobMessage::Error("subagent stdout unavailable".into()));
        kill_child(&mut child, &process_tree);
        return;
    };

    let (lines_tx, lines_rx) = mpsc::channel::<Result<Value, String>>();
    thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        loop {
            let line = match read_rpc_line(&mut reader, MAX_RPC_LINE_BYTES) {
                Ok(Some(line)) => line,
                Ok(None) => break,
                Err(error) => {
                    let _ = lines_tx.send(Err(format!("read subagent RPC: {error}")));
                    break;
                }
            };
            let result = serde_json::from_str(&line)
                .map_err(|error| format!("invalid subagent RPC: {error}"));
            if lines_tx.send(result).is_err() {
                break;
            }
        }
    });

    let request_id = 1_u64;
    let (prompt_tx, prompt_rx) = mpsc::channel();
    let prompt_stdin = Arc::clone(&stdin);
    let prompt = child_prompt(&request);
    let prompt_writer = thread::spawn(move || {
        let result = send_child_command(
            &prompt_stdin,
            serde_json::json!({
                "id": request_id,
                "type": "prompt",
                "content": prompt,
            }),
        );
        let _ = prompt_tx.send(result);
    });

    let prompt_result = loop {
        if cancelled.load(Ordering::Acquire) {
            break Err("subagent cancelled".to_string());
        }
        if Instant::now() >= deadline {
            break Err(format!(
                "subagent timed out after {} seconds",
                request.timeout_seconds
            ));
        }
        match prompt_rx.recv_timeout(Duration::from_millis(100)) {
            Ok(result) => break result,
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                break Err("subagent prompt writer stopped".to_string())
            }
        }
    };
    let writer_panicked = prompt_writer.join().is_err();
    if writer_panicked {
        kill_child(&mut child, &process_tree);
        let _ = output.send(JobMessage::Error("subagent prompt writer panicked".into()));
        return;
    }
    if let Err(error) = prompt_result {
        kill_child(&mut child, &process_tree);
        let _ = output.send(JobMessage::Error(error));
        return;
    }

    let mut turns = 0_u64;
    let mut turn_limited = false;
    let mut output_bytes = 0_usize;
    let mut partial_events = 0_usize;
    loop {
        if cancelled.load(Ordering::Acquire) {
            kill_child(&mut child, &process_tree);
            let _ = output.send(JobMessage::Error("subagent cancelled".into()));
            return;
        }
        if Instant::now() >= deadline {
            kill_child(&mut child, &process_tree);
            let _ = output.send(JobMessage::Error(format!(
                "subagent timed out after {} seconds",
                request.timeout_seconds
            )));
            return;
        }

        match lines_rx.recv_timeout(Duration::from_millis(100)) {
            Ok(Err(error)) => {
                kill_child(&mut child, &process_tree);
                let _ = output.send(JobMessage::Error(error));
                return;
            }
            Ok(Ok(event)) => {
                if event.get("type").and_then(Value::as_str) == Some("turn_start") {
                    turns += 1;
                    if turns > request.max_turns && !turn_limited {
                        turn_limited = true;
                        if send_abort_best_effort(&mut child, &process_tree, &stdin).is_err() {
                            kill_child(&mut child, &process_tree);
                            let _ = output.send(JobMessage::Error(
                                "failed to abort subagent at turn limit".into(),
                            ));
                            return;
                        }
                    }
                }
                if !turn_limited {
                    if let Some(delta) = text_delta(&event) {
                        output_bytes = output_bytes.saturating_add(delta.len());
                        partial_events = partial_events.saturating_add(1);
                        if output_bytes > MAX_OUTPUT_BYTES {
                            kill_child(&mut child, &process_tree);
                            let _ = output.send(JobMessage::Error(format!(
                                "subagent output exceeds {MAX_OUTPUT_BYTES} bytes"
                            )));
                            return;
                        }
                        if partial_events > MAX_PARTIAL_EVENTS {
                            kill_child(&mut child, &process_tree);
                            let _ = output.send(JobMessage::Error(format!(
                                "subagent output exceeds {MAX_PARTIAL_EVENTS} partial events"
                            )));
                            return;
                        }
                        if output.send(JobMessage::Partial(delta)).is_err() {
                            kill_child(&mut child, &process_tree);
                            return;
                        }
                    }
                }
                match response_result(&event, request_id) {
                    Err(error) => {
                        kill_child(&mut child, &process_tree);
                        let _ = output.send(JobMessage::Error(error));
                        return;
                    }
                    Ok(Some(response)) => {
                        if output_bytes.saturating_add(response.final_text.len()) > MAX_OUTPUT_BYTES
                        {
                            kill_child(&mut child, &process_tree);
                            let _ = output.send(JobMessage::Error(format!(
                                "subagent total output exceeds {MAX_OUTPUT_BYTES} bytes"
                            )));
                            return;
                        }
                        let status = if turn_limited
                            || (response.outcome == "aborted" && turns >= request.max_turns)
                        {
                            "turn_limit"
                        } else {
                            response.outcome.as_str()
                        };
                        let body = serde_json::json!({
                            "kind": "rpi-delegation",
                            "status": status,
                            "role": request.role,
                            "maxTurns": request.max_turns,
                            "timeoutSeconds": request.timeout_seconds,
                            "recursive": false,
                            "result": response.final_text,
                        });
                        if output.send(JobMessage::Done(body.to_string())).is_err() {
                            kill_child(&mut child, &process_tree);
                            return;
                        }
                        kill_child(&mut child, &process_tree);
                        return;
                    }
                    Ok(None) => {}
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                if child.try_wait().ok().flatten().is_some() {
                    let _ = output.send(JobMessage::Error(
                        "subagent exited before its response".into(),
                    ));
                    return;
                }
            }
            Err(RecvTimeoutError::Disconnected) => {
                kill_child(&mut child, &process_tree);
                let _ = output.send(JobMessage::Error("subagent RPC reader stopped".into()));
                return;
            }
        }
    }
}

fn tool_result(text: &str) -> String {
    serde_json::json!({"content": [{"type": "text", "text": text}]}).to_string()
}

extern "C" fn execute(
    _: StbStringRef,
    params: StbString,
    free_params: Option<FreeStringFn>,
) -> StepHandle {
    rpi_plugin_sdk::guard(|| {
        let text = params.to_string_lossy();
        params.free_with(free_params);
        let value = serde_json::from_str::<Value>(&text).unwrap_or(Value::Null);
        let (sender, receiver) = mpsc::channel();
        let cancelled = Arc::new(AtomicBool::new(false));
        match parse_request(&value) {
            Ok(request) => {
                let token = cancelled.clone();
                thread::spawn(move || run_request(request, sender, token));
            }
            Err(error) => {
                let _ = sender.send(JobMessage::Error(error));
            }
        }
        JOBS.insert_with_cancel(
            Job {
                receiver,
                completed: false,
            },
            cancelled,
        )
    })
    .unwrap_or(std::ptr::null_mut())
}

extern "C" fn poll(
    handle: StepHandle,
    partial_cb: Option<ToolPartialCb>,
    user_data: *mut std::ffi::c_void,
) -> StepResult {
    rpi_plugin_sdk::guard(|| {
        if handle.is_null() {
            return StepResult::err(StbString::from_string("null subagent handle".into()));
        }
        let Some(entry) = JOBS.get(handle) else {
            return StepResult::err(StbString::from_string(
                "invalid or destroyed subagent handle".into(),
            ));
        };
        if entry.cancelled.load(Ordering::Acquire) {
            return StepResult::err(StbString::from_string("subagent cancelled".into()));
        }

        let mut partials = Vec::new();
        let terminal = {
            let mut job = entry.lock();
            if job.completed {
                return StepResult::err(StbString::from_string(
                    "subagent polled after completion".into(),
                ));
            }
            let mut terminal = None;
            while let Ok(message) = job.receiver.try_recv() {
                match message {
                    JobMessage::Partial(text) => partials.push(text),
                    JobMessage::Done(text) => {
                        job.completed = true;
                        terminal = Some(Ok(text));
                        break;
                    }
                    JobMessage::Error(error) => {
                        job.completed = true;
                        terminal = Some(Err(error));
                        break;
                    }
                }
            }
            terminal
        };

        if let Some(callback) = partial_cb {
            for text in partials {
                callback(StbString::from_string(tool_result(&text)), user_data);
            }
        }
        match terminal {
            Some(Ok(text)) => StepResult::done(StbString::from_string(tool_result(&text))),
            Some(Err(error)) => StepResult::err(StbString::from_string(error)),
            None => StepResult::pending(StbString::empty()),
        }
    })
    .unwrap_or_else(|_| StepResult::err(StbString::from_string("subagent poll panicked".into())))
}

extern "C" fn cancel(handle: StepHandle) {
    JOBS.cancel(handle);
}

extern "C" fn destroy(handle: StepHandle) {
    drop(JOBS.remove(handle));
}

rpi_plugin_sdk::export_plugin!(|api| {
    if std::env::var("RPI_SUBAGENT_CHILD").ok().as_deref() == Some("1") {
        return 0;
    }
    let Some(register) = api.register_tool else {
        return 1;
    };
    let schema = Box::new(schema(
        "delegate_task",
        "Run a bounded, non-recursive subagent and return its final report.",
        r#"{"type":"object","properties":{"task":{"type":"string","maxLength":20000},"role":{"type":"string","maxLength":200},"maxTurns":{"type":"integer","minimum":1,"maximum":32},"timeoutSeconds":{"type":"integer","minimum":10,"maximum":1800}},"required":["task"]}"#,
    ));
    let result = register(&*schema, execute, poll, cancel, destroy, plugin_free_string);
    drop(schema);
    result
});

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::{Mutex, OnceLock};
    use std::time::{SystemTime, UNIX_EPOCH};

    static PROCESS_TEST_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

    struct FakeRpcFixture {
        dir: PathBuf,
        binary: PathBuf,
    }

    impl FakeRpcFixture {
        fn new() -> Self {
            let unique = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock before epoch")
                .as_nanos();
            let dir = std::env::temp_dir().join(format!("rpi-subagents-test-{unique}"));
            std::fs::create_dir_all(&dir).expect("create fake RPC directory");
            let source = dir.join("fake_rpc.rs");
            let binary = dir.join(format!("fake-rpc{}", std::env::consts::EXE_SUFFIX));
            std::fs::write(
                &source,
                r##"
use std::io::{self, BufRead, Write};
use std::time::Duration;
fn main() {
    if std::env::var("RPI_FAKE_RPC_MODE").ok().as_deref() == Some("blocked") {
        std::thread::sleep(Duration::from_secs(60));
        return;
    }
    if std::env::var("RPI_FAKE_RPC_MODE").ok().as_deref() == Some("shared-output") {
        let delta = "a".repeat(4 * 1024 * 1024 - 1);
        println!("{{\"type\":\"message_update\",\"delta\":\"{}\"}}", delta);
        println!("{}", r#"{"type":"response","id":1,"status":"ok","result":{"outcome":"completed","finalText":"xx"}}"#);
        io::stdout().flush().unwrap();
        return;
    }
    let mut request = String::new();
    io::stdin().lock().read_line(&mut request).unwrap();
    if std::env::var("RPI_FAKE_RPC_MODE").ok().as_deref() == Some("cancel") {
        std::thread::sleep(Duration::from_secs(60));
        return;
    }
    println!("{}", r#"{"type":"turn_start"}"#);
    println!("{}", r#"{"type":"message_update","delta":"hello"}"#);
    println!("{}", r#"{"type":"response","id":1,"status":"ok","result":{"outcome":"completed","finalText":"done"}}"#);
    io::stdout().flush().unwrap();
}
"##,
            )
            .expect("write fake RPC source");
            let status = Command::new("rustc")
                .args(["--edition=2021"])
                .arg(&source)
                .arg("-o")
                .arg(&binary)
                .status()
                .expect("run rustc for fake RPC");
            assert!(status.success(), "fake RPC helper failed to compile");
            Self { dir, binary }
        }
    }

    impl Drop for FakeRpcFixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    struct EnvGuard {
        values: Vec<(&'static str, Option<std::ffi::OsString>)>,
    }

    impl EnvGuard {
        fn set(values: &[(&'static str, &str)]) -> Self {
            let saved = values
                .iter()
                .map(|(key, _)| (*key, std::env::var_os(key)))
                .collect();
            for (key, value) in values {
                std::env::set_var(key, value);
            }
            Self { values: saved }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (key, value) in &self.values {
                match value {
                    Some(value) => std::env::set_var(key, value),
                    None => std::env::remove_var(key),
                }
            }
        }
    }

    fn with_fake_rpc<F>(mode: &str, test: F)
    where
        F: FnOnce(),
    {
        let _lock = PROCESS_TEST_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .expect("lock process test environment");
        let fixture = FakeRpcFixture::new();
        let _env = EnvGuard::set(&[
            (
                "RPI_SUBAGENT_BIN",
                fixture.binary.to_str().expect("fixture path"),
            ),
            ("RPI_FAKE_RPC_MODE", mode),
        ]);
        test();
    }

    #[test]
    fn parses_and_clamps_delegation_limits() {
        let request = parse_request(&serde_json::json!({
            "task": "review",
            "role": "reviewer",
            "maxTurns": 999,
            "timeoutSeconds": 99999
        }))
        .unwrap();
        assert_eq!(request.max_turns, MAX_TURNS);
        assert_eq!(request.timeout_seconds, MAX_TIMEOUT_SECONDS);
        assert_eq!(request.role, "reviewer");
    }

    #[test]
    fn rejects_missing_and_oversized_values() {
        assert!(parse_request(&Value::Null).is_err());
        assert!(parse_request(&serde_json::json!({"task":"  "})).is_err());
        assert!(parse_request(&serde_json::json!({
            "task": "x".repeat(MAX_TASK_BYTES + 1)
        }))
        .is_err());
    }

    #[test]
    fn extracts_only_text_deltas() {
        assert_eq!(
            text_delta(&serde_json::json!({"type":"message_update","delta":"hello"})),
            Some("hello".into())
        );
        assert_eq!(
            text_delta(&serde_json::json!({
                "type":"message_update",
                "assistantMessageEvent":{"delta":"world"}
            })),
            Some("world".into())
        );
        assert!(text_delta(&serde_json::json!({"type":"tool_execution_start"})).is_none());
    }

    #[test]
    fn parses_rpc_response() {
        let event = serde_json::json!({
            "type":"response", "id":1, "status":"ok",
            "result":{"outcome":"completed","finalText":"done"}
        });
        assert_eq!(
            response_result(&event, 1).unwrap(),
            Some(RpcResult {
                outcome: "completed".into(),
                final_text: "done".into(),
            })
        );
        assert!(response_result(&event, 2).unwrap().is_none());
        assert!(response_result(
            &serde_json::json!({"type":"response","id":1,"status":"error","error":"bad"}),
            1
        )
        .is_err());
    }

    #[test]
    fn bounded_child_command_disables_sessions_and_sets_turn_budget() {
        let args = command_args();
        assert_eq!(args, ["--mode", "rpc", "--no-session"]);
        let request = Request {
            task: "inspect".into(),
            role: "worker".into(),
            max_turns: 3,
            timeout_seconds: 10,
        };
        assert!(child_prompt(&request).contains("at most 3 agent turns"));
    }

    #[test]
    fn rejects_overlong_rpc_lines_without_unbounded_buffering() {
        let mut reader = BufReader::new(&b"1234567890\n"[..]);
        let error = read_rpc_line(&mut reader, 5).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    }

    #[test]
    fn runs_a_fake_rpc_child_and_forwards_progress() {
        with_fake_rpc("success", || {
            let request = Request {
                task: "inspect".into(),
                role: "worker".into(),
                max_turns: 2,
                timeout_seconds: 10,
            };
            let (sender, receiver) = mpsc::channel();
            let cancelled = Arc::new(AtomicBool::new(false));
            let worker_cancelled = cancelled.clone();
            let worker = thread::spawn(move || run_request(request, sender, worker_cancelled));

            let mut saw_partial = false;
            loop {
                match receiver
                    .recv_timeout(Duration::from_secs(3))
                    .expect("fake RPC worker result")
                {
                    JobMessage::Partial(text) => {
                        assert_eq!(text, "hello");
                        saw_partial = true;
                    }
                    JobMessage::Done(text) => {
                        assert!(saw_partial);
                        assert!(text.contains("\"status\":\"completed\""));
                        assert!(text.contains("done"));
                        break;
                    }
                    JobMessage::Error(error) => panic!("fake RPC failed: {error}"),
                }
            }
            worker.join().expect("join fake RPC worker");
        });
    }

    #[test]
    fn partial_and_final_output_share_one_budget() {
        with_fake_rpc("shared-output", || {
            let request = Request {
                task: "inspect".into(),
                role: "worker".into(),
                max_turns: 2,
                timeout_seconds: 10,
            };
            let (sender, receiver) = mpsc::channel();
            let cancelled = Arc::new(AtomicBool::new(false));
            let worker_cancelled = cancelled.clone();
            let worker = thread::spawn(move || run_request(request, sender, worker_cancelled));

            loop {
                match receiver
                    .recv_timeout(Duration::from_secs(3))
                    .expect("shared output budget result")
                {
                    JobMessage::Error(error) => {
                        assert!(error.contains("total output"), "{error}");
                        break;
                    }
                    JobMessage::Partial(_) => {}
                    JobMessage::Done(_) => panic!("shared output budget was not enforced"),
                }
            }
            worker.join().expect("join shared output worker");
        });
    }

    #[test]
    fn cancellation_terminates_a_fake_rpc_child() {
        with_fake_rpc("cancel", || {
            let request = Request {
                task: "wait".into(),
                role: "worker".into(),
                max_turns: 2,
                timeout_seconds: 10,
            };
            let (sender, receiver) = mpsc::channel();
            let cancelled = Arc::new(AtomicBool::new(false));
            let worker_cancelled = cancelled.clone();
            let worker = thread::spawn(move || run_request(request, sender, worker_cancelled));
            thread::sleep(Duration::from_millis(100));
            cancelled.store(true, Ordering::Release);

            match receiver
                .recv_timeout(Duration::from_secs(3))
                .expect("cancellation result")
            {
                JobMessage::Error(error) => assert!(error.contains("cancelled"), "{error}"),
                JobMessage::Partial(_) | JobMessage::Done(_) => {
                    panic!("cancellation did not terminate the worker")
                }
            }
            worker.join().expect("join cancelled fake RPC worker");
        });
    }

    #[test]
    fn cancellation_interrupts_a_blocked_prompt_write() {
        with_fake_rpc("blocked", || {
            let request = Request {
                task: "x".repeat(MAX_TASK_BYTES),
                role: "worker".into(),
                max_turns: 2,
                timeout_seconds: 10,
            };
            let (sender, receiver) = mpsc::channel();
            let cancelled = Arc::new(AtomicBool::new(false));
            let worker_cancelled = cancelled.clone();
            let worker = thread::spawn(move || run_request(request, sender, worker_cancelled));
            thread::sleep(Duration::from_millis(100));
            cancelled.store(true, Ordering::Release);

            match receiver
                .recv_timeout(Duration::from_secs(3))
                .expect("blocked prompt cancellation result")
            {
                JobMessage::Error(error) => assert!(error.contains("cancelled"), "{error}"),
                JobMessage::Partial(_) | JobMessage::Done(_) => {
                    panic!("blocked prompt write was not cancelled")
                }
            }
            worker.join().expect("join blocked prompt worker");
        });
    }

    #[test]
    fn prompt_carries_role_and_limits() {
        let request = Request {
            task: "inspect code".into(),
            role: "reviewer".into(),
            max_turns: 3,
            timeout_seconds: 10,
        };
        let prompt = child_prompt(&request);
        assert!(prompt.contains("reviewer"));
        assert!(prompt.contains("at most 3 agent turns"));
        assert!(prompt.contains("inspect code"));
        assert!(prompt.contains("Do not delegate"));
    }
}
