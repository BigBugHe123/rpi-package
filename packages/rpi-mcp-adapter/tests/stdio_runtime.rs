#![forbid(unsafe_code)]

use rpi_mcp_adapter::{call_from_config, discover_from_config};
use serde_json::json;
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

fn config_path() -> std::path::PathBuf {
    let suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("rpi-mcp-test-{}-{suffix}.json", std::process::id()))
}

fn write_config(path: &std::path::Path) {
    let command = env!("CARGO_BIN_EXE_fake_mcp_server");
    let config = json!({
        "mcpServers": {
            "fake": {
                "command": command,
                "enabled": true,
                "timeout": 5
            }
        }
    });
    fs::write(path, config.to_string()).unwrap();
}

#[test]
fn stdio_runtime_discovers_and_calls_tool() {
    let path = config_path();
    write_config(&path);

    let discovered = discover_from_config(path.to_str().unwrap(), "fake").unwrap();
    assert_eq!(discovered.name, "fake");
    assert_eq!(discovered.info.name, "rpi-fake-mcp");
    assert_eq!(discovered.tools.len(), 1);
    assert_eq!(discovered.tools[0].name, "echo");

    let result = call_from_config(
        path.to_str().unwrap(),
        "fake",
        "echo",
        json!({"value":"hello"}),
    )
    .unwrap();
    assert_eq!(result, "echo:hello");

    rpi_mcp_adapter::close_from_config(path.to_str().unwrap(), "fake").unwrap();
    let _ = fs::remove_file(path);
}

#[test]
fn stdio_runtime_rejects_unknown_tool() {
    let path = config_path();
    write_config(&path);
    let error = call_from_config(path.to_str().unwrap(), "fake", "missing", json!({})).unwrap_err();
    assert!(error.contains("does not expose tool"));
    rpi_mcp_adapter::close_from_config(path.to_str().unwrap(), "fake").unwrap();
    let _ = fs::remove_file(path);
}

#[test]
fn stdio_timeout_bounds_unresponsive_server() {
    let mut transport = rpi_mcp_adapter::StdioTransport::start(
        env!("CARGO_BIN_EXE_fake_mcp_server"),
        &["--hang".into()],
        &Default::default(),
        None,
        None,
    )
    .unwrap();
    transport.set_timeout(1);
    let started = std::time::Instant::now();
    let error = transport
        .request(&rpi_mcp_adapter::tools_list_request(1))
        .unwrap_err();
    assert!(
        error.contains("tools/list timed out after 1 seconds"),
        "{error}"
    );
    assert!(started.elapsed() < std::time::Duration::from_secs(5));
}

#[test]
fn stdio_skips_notifications_before_response() {
    let mut transport = rpi_mcp_adapter::StdioTransport::start(
        env!("CARGO_BIN_EXE_fake_mcp_server"),
        &["--notify".into()],
        &Default::default(),
        None,
        None,
    )
    .unwrap();
    transport.set_timeout(2);
    let response = transport
        .request(&rpi_mcp_adapter::tools_list_request(7))
        .unwrap();
    assert_eq!(response.id, 7);
}

fn config_with_mode(mode: &str) -> std::path::PathBuf {
    let path = config_path();
    fs::write(&path, json!({"mcpServers":{"fake":{"command":env!("CARGO_BIN_EXE_fake_mcp_server"),"args":[mode],"timeout":2}}}).to_string()).unwrap();
    path
}

fn config_with_modes(modes: &[&str]) -> std::path::PathBuf {
    config_with_modes_and_timeout(modes, 2)
}

fn config_with_modes_and_timeout(modes: &[&str], timeout: u64) -> std::path::PathBuf {
    let path = config_path();
    fs::write(&path, json!({"mcpServers":{"fake":{"command":env!("CARGO_BIN_EXE_fake_mcp_server"),"args":modes,"timeout":timeout}}}).to_string()).unwrap();
    path
}

/// The pending queue is process-global, so tests that inspect it must not run
/// concurrently with each other.
static RESOURCE_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn drain_updates_until(within: std::time::Duration) -> Vec<rpi_mcp_adapter::ResourceUpdate> {
    let deadline = std::time::Instant::now() + within;
    loop {
        let updates = rpi_mcp_adapter::take_resource_updates(None);
        if !updates.is_empty() || std::time::Instant::now() >= deadline {
            return updates;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// Regression: the local subscription mark used to be written *after* the
/// `subscribe` response, so a server that pushed the first
/// `notifications/resources/updated` while the handshake was still in flight had
/// that update dropped by the dispatcher.
#[test]
fn subscribe_keeps_an_update_pushed_before_the_response() {
    let _serial = RESOURCE_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let path = config_with_modes(&["--notify-early"]);
    let _ = rpi_mcp_adapter::take_resource_updates(None);

    let subscribed = rpi_mcp_adapter::resources_from_config(
        path.to_str().unwrap(),
        "fake",
        "subscribe",
        Some("mem://alpha"),
    )
    .unwrap();
    assert_eq!(subscribed["subscribed"], true);

    let updates = drain_updates_until(std::time::Duration::from_secs(5));
    assert_eq!(
        updates.len(),
        1,
        "the update pushed before the subscribe response was lost: {updates:?}"
    );
    assert!(updates[0].text.contains("value-of-mem://alpha"));

    rpi_mcp_adapter::close_from_config(path.to_str().unwrap(), "fake").unwrap();
    let _ = fs::remove_file(path);
}

#[test]
fn unsubscribe_and_resubscribe_keep_resource_delivery_consistent() {
    let _serial = RESOURCE_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let path = config_with_modes(&["--notify-early"]);
    let _ = rpi_mcp_adapter::take_resource_updates(None);
    let resource = |action| {
        rpi_mcp_adapter::resources_from_config(
            path.to_str().unwrap(),
            "fake",
            action,
            Some("mem://alpha"),
        )
        .unwrap()
    };
    resource("subscribe");
    assert_eq!(
        drain_updates_until(std::time::Duration::from_secs(5)).len(),
        1
    );
    assert_eq!(resource("unsubscribe")["unsubscribed"], true);
    assert!(rpi_mcp_adapter::take_resource_updates(None).is_empty());
    assert_eq!(resource("subscribe")["subscribed"], true);
    let updates = drain_updates_until(std::time::Duration::from_secs(5));
    rpi_mcp_adapter::close_from_config(path.to_str().unwrap(), "fake").unwrap();
    let _ = fs::remove_file(path);
    assert_eq!(
        updates.len(),
        1,
        "new subscription failed to deliver: {updates:?}"
    );
}

#[test]
fn resource_pagination_does_not_report_partial_results_as_complete() {
    for pages in [100, 101] {
        let mode = format!("--resource-pages={pages}");
        let path = config_with_mode(&mode);
        let result =
            rpi_mcp_adapter::resources_from_config(path.to_str().unwrap(), "fake", "list", None);
        rpi_mcp_adapter::close_from_config(path.to_str().unwrap(), "fake").unwrap();
        let _ = fs::remove_file(path);
        if pages == 100 {
            let resources = result.unwrap()["resources"].as_array().unwrap().clone();
            assert_eq!(resources.len(), 100);
            assert_eq!(resources[99]["uri"], "mem://page-99");
        } else {
            assert!(result.unwrap_err().contains("exceeded 100 pages"));
        }
    }
}

/// Regression: the background `resources/read` used a standalone control, so
/// `close` waited for its timeout (up to 120 s) instead of cancelling it.
#[test]
fn close_cancels_a_blocked_background_read() {
    let _serial = RESOURCE_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    // A long client timeout: without the fix, close would wait for the blocked
    // read to time out (30 s) instead of cancelling it.
    let path = config_with_modes_and_timeout(&["--notify-early", "--hang-read"], 30);
    let _ = rpi_mcp_adapter::take_resource_updates(None);

    rpi_mcp_adapter::resources_from_config(
        path.to_str().unwrap(),
        "fake",
        "subscribe",
        Some("mem://alpha"),
    )
    .unwrap();
    // Give the dispatcher a moment to enter the (blocking) read.
    std::thread::sleep(std::time::Duration::from_millis(200));

    let started = std::time::Instant::now();
    rpi_mcp_adapter::close_from_config(path.to_str().unwrap(), "fake").unwrap();
    let elapsed = started.elapsed();
    assert!(
        elapsed < std::time::Duration::from_secs(3),
        "close waited for the blocked read instead of cancelling it: {elapsed:?}"
    );
    let _ = fs::remove_file(path);
}

/// Regression: closing a connection must leave no queued updates behind, so a
/// later session cannot be shown a released connection's resource contents.
///
/// Deliberately per-connection (`close_from_config`) rather than the global
/// `shutdown_connections`, which would tear down every other test's session.
#[test]
fn close_drops_queued_resource_updates() {
    let _serial = RESOURCE_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let path = config_with_modes(&["--notify-early"]);
    let _ = rpi_mcp_adapter::take_resource_updates(None);

    let subscribe = || {
        rpi_mcp_adapter::resources_from_config(
            path.to_str().unwrap(),
            "fake",
            "subscribe",
            Some("mem://alpha"),
        )
        .unwrap()
    };

    // The pipeline works: the update pushed before the response is queued.
    subscribe();
    let first = drain_updates_until(std::time::Duration::from_secs(5));
    assert_eq!(first.len(), 1, "expected one queued update, got {first:?}");

    // Queue another and close without draining it.
    subscribe();
    std::thread::sleep(std::time::Duration::from_millis(200));
    rpi_mcp_adapter::close_from_config(path.to_str().unwrap(), "fake").unwrap();

    assert!(
        rpi_mcp_adapter::take_resource_updates(None).is_empty(),
        "close left resource updates queued for a later session"
    );
    let _ = fs::remove_file(path);
}
#[test]
fn persistent_session_and_paginated_tools() {
    let path = config_with_mode("--paged");
    let discovered = discover_from_config(path.to_str().unwrap(), "fake").unwrap();
    assert_eq!(discovered.tools.len(), 4);
    let first = call_from_config(path.to_str().unwrap(), "fake", "counter", json!({})).unwrap();
    let second = call_from_config(path.to_str().unwrap(), "fake", "counter", json!({})).unwrap();
    let (pid, count) = first.split_once(':').unwrap();
    assert_eq!(count, "1");
    assert_eq!(second, format!("{pid}:2"));
    assert!(
        call_from_config(path.to_str().unwrap(), "fake", "fail", json!({}))
            .unwrap_err()
            .contains("intentional failure")
    );
    // A tool error must not discard the healthy session.
    assert_eq!(
        call_from_config(path.to_str().unwrap(), "fake", "counter", json!({})).unwrap(),
        format!("{pid}:4")
    );
    rpi_mcp_adapter::close_from_config(path.to_str().unwrap(), "fake").unwrap();
    let fresh = call_from_config(path.to_str().unwrap(), "fake", "counter", json!({})).unwrap();
    assert!(fresh.ends_with(":1"));
    assert_ne!(fresh.split(':').next().unwrap(), pid);
    rpi_mcp_adapter::close_from_config(path.to_str().unwrap(), "fake").unwrap();
    fs::remove_file(path).unwrap();
}
#[test]
fn pagination_rejects_repeated_cursor() {
    let path = config_with_mode("--bad-cursor");
    assert!(discover_from_config(path.to_str().unwrap(), "fake")
        .unwrap_err()
        .contains("repeated"));
    rpi_mcp_adapter::close_from_config(path.to_str().unwrap(), "fake").unwrap();
    fs::remove_file(path).unwrap();
}
#[test]
fn timeout_covers_a_full_stdin_pipe() {
    let mut transport = rpi_mcp_adapter::StdioTransport::start(
        env!("CARGO_BIN_EXE_fake_mcp_server"),
        &["--no-read".into()],
        &Default::default(),
        None,
        None,
    )
    .unwrap();
    transport.set_timeout(1);
    let start = std::time::Instant::now();
    let request = rpi_mcp_adapter::Request::new(
        1,
        "large",
        Some(json!({"data":"x".repeat(2 * 1024 * 1024)})),
    );
    assert!(transport
        .request(&request)
        .unwrap_err()
        .contains("timed out"));
    assert!(start.elapsed().as_secs_f32() < 3.0);
}
#[test]
fn cancellation_interrupts_pipe_write_and_cleans_workers() {
    let mut transport = rpi_mcp_adapter::StdioTransport::start(
        env!("CARGO_BIN_EXE_fake_mcp_server"),
        &["--no-read".into()],
        &Default::default(),
        None,
        None,
    )
    .unwrap();
    transport.set_timeout(60);
    let control = rpi_mcp_adapter::Control::default();
    let cancel = control.clone();
    let worker = std::thread::spawn(move || {
        let request = rpi_mcp_adapter::Request::new(
            1,
            "large",
            Some(json!({"data":"x".repeat(2 * 1024 * 1024)})),
        );
        transport.request_control(&request, &control)
    });
    std::thread::sleep(std::time::Duration::from_millis(100));
    let start = std::time::Instant::now();
    cancel.cancel();
    assert!(worker.join().unwrap().unwrap_err().contains("cancelled"));
    assert!(start.elapsed().as_secs_f32() < 2.0);
}
#[test]
fn server_ping_is_answered() {
    let path = config_with_mode("--ping");
    assert_eq!(
        call_from_config(
            path.to_str().unwrap(),
            "fake",
            "echo",
            json!({"value":"ping"})
        )
        .unwrap(),
        "echo:ping"
    );
    rpi_mcp_adapter::close_from_config(path.to_str().unwrap(), "fake").unwrap();
    fs::remove_file(path).unwrap();
}
#[test]
fn oversize_unterminated_stdout_is_rejected() {
    let mut transport = rpi_mcp_adapter::StdioTransport::start(
        env!("CARGO_BIN_EXE_fake_mcp_server"),
        &["--oversize".into()],
        &Default::default(),
        None,
        None,
    )
    .unwrap();
    transport.set_timeout(5);
    assert!(transport
        .request(&rpi_mcp_adapter::tools_list_request(1))
        .unwrap_err()
        .contains("16 MiB"));
}
#[test]
fn timeout_error_includes_server_stderr() {
    let path = config_with_mode("--hang-call");
    let error = call_from_config(path.to_str().unwrap(), "fake", "echo", json!({})).unwrap_err();
    assert!(error.contains("tools/call timed out"), "{error}");
    assert!(error.contains("deliberately blocked"), "{error}");
    rpi_mcp_adapter::close_from_config(path.to_str().unwrap(), "fake").unwrap();
    fs::remove_file(path).unwrap();
}

#[cfg(windows)]
#[test]
fn timeout_kills_descendants_holding_inherited_pipes() {
    let mut transport = rpi_mcp_adapter::StdioTransport::start(
        env!("CARGO_BIN_EXE_fake_mcp_server"),
        &["--spawn-child".into()],
        &Default::default(),
        None,
        None,
    )
    .unwrap();
    transport.set_timeout(1);
    let start = std::time::Instant::now();
    let error = transport
        .request(&rpi_mcp_adapter::tools_list_request(1))
        .unwrap_err();
    assert!(start.elapsed() < std::time::Duration::from_secs(3));
    let pid: u32 = error
        .split("descendant-pid=")
        .nth(1)
        .unwrap()
        .lines()
        .next()
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(
        !rpi_plugin_sdk::process::process_is_running(pid).unwrap(),
        "MCP descendant survived transport shutdown"
    );
}
