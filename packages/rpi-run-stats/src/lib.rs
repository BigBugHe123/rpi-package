#![forbid(unsafe_code)]

use rpi_plugin_sdk::{EventTag, PluginApi, RuntimeActionId, StbString, StbStringRef};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    sync::{Mutex, OnceLock},
    time::Instant,
};

const KEY: &str = "rpi.run-stats";
// Reuse the SDK's host-context lifetime and thread-safety contract.
type Runtime = PluginApi;
static RUNTIME: OnceLock<Runtime> = OnceLock::new();
static STATE: OnceLock<Mutex<Stats>> = OnceLock::new();
static TIMER: Mutex<Option<(std::sync::mpsc::Sender<()>, std::thread::JoinHandle<()>)>> =
    Mutex::new(None);
fn state() -> &'static Mutex<Stats> {
    STATE.get_or_init(|| Mutex::new(Stats::default()))
}

struct Stats {
    last_publication: Option<String>,
    enabled: bool,
    anchor: String,
    running: bool,
    rounds: u64,
    steps: u64,
    errors: u64,
    input: u64,
    output: u64,
    cache_read: u64,
    cache_write: u64,
    cost: Option<f64>,
    request: Option<Instant>,
    first: Option<Instant>,
    ttft: Option<f64>,
    latency: Option<f64>,
    speed: Option<f64>,
    recent: VecDeque<(Instant, u64)>,
}
impl Default for Stats {
    fn default() -> Self {
        Self {
            last_publication: None,
            enabled: false,
            anchor: "top-right".into(),
            running: false,
            rounds: 0,
            steps: 0,
            errors: 0,
            input: 0,
            output: 0,
            cache_read: 0,
            cache_write: 0,
            cost: None,
            request: None,
            first: None,
            ttft: None,
            latency: None,
            speed: None,
            recent: VecDeque::new(),
        }
    }
}
fn count(v: &Value, key: &str) -> u64 {
    v.get(key).and_then(Value::as_u64).unwrap_or(0)
}
fn assistant(v: &Value) -> bool {
    v.get("role")
        .or_else(|| v.get("kind"))
        .and_then(Value::as_str)
        == Some("assistant")
}
impl Stats {
    fn update(&mut self, tag: EventTag, v: &Value, now: Instant) {
        match tag {
            EventTag::SessionStart => {
                let enabled = self.enabled;
                let anchor = self.anchor.clone();
                *self = Self::default();
                self.enabled = enabled;
                self.anchor = anchor;
            }
            EventTag::AgentStart => {
                self.rounds += 1;
                self.running = true;
            }
            EventTag::AgentEnd => {
                self.running = false;
                self.request = None;
                self.first = None;
            }
            EventTag::BeforeProviderRequest => {
                self.steps += 1;
                self.request = Some(now);
                self.first = None;
                self.ttft = None;
                self.latency = None;
                self.speed = None;
            }
            EventTag::MessageUpdate => {
                let event = v.get("assistantMessageEvent").unwrap_or(v);
                let ty = event.get("type").and_then(Value::as_str).unwrap_or("");
                if self.first.is_none()
                    && matches!(ty, "text_delta" | "thinking_delta" | "toolcall_delta")
                    && event
                        .get("delta")
                        .and_then(Value::as_str)
                        .is_some_and(|s| !s.is_empty())
                {
                    if let Some(start) = self.request {
                        self.first = Some(now);
                        self.ttft = Some(now.duration_since(start).as_secs_f64());
                    }
                }
            }
            EventTag::MessageEnd if assistant(v) => {
                // Only finalize an open request; duplicate terminal events cannot double count.
                let Some(start) = self.request.take() else {
                    return;
                };
                self.latency = Some(now.duration_since(start).as_secs_f64());
                if matches!(
                    v.get("stopReason").and_then(Value::as_str),
                    Some("error" | "aborted")
                ) {
                    self.errors += 1;
                }
                if let Some(usage) = v.get("usage") {
                    let output = count(usage, "output");
                    self.input += count(usage, "input");
                    self.output += output;
                    self.cache_read += count(usage, "cacheRead");
                    self.cache_write += count(usage, "cacheWrite");
                    if let Some(cost) = usage
                        .pointer("/cost/total")
                        .and_then(Value::as_f64)
                        .filter(|v| v.is_finite() && *v >= 0.0)
                    {
                        self.cost = Some(self.cost.unwrap_or(0.0) + cost);
                    }
                    let total = usage
                        .get("totalTokens")
                        .and_then(Value::as_u64)
                        .unwrap_or_else(|| {
                            count(usage, "input")
                                + output
                                + count(usage, "cacheRead")
                                + count(usage, "cacheWrite")
                        });
                    self.recent.push_back((now, total));
                    // Include TTFT in throughput: true end-to-end output tokens / request second.
                    self.speed = self
                        .latency
                        .filter(|seconds| *seconds > 0.0)
                        .map(|seconds| output as f64 / seconds);
                }
                self.first = None;
            }
            _ => {}
        }
    }
    fn snapshot(&mut self, now: Instant) -> Value {
        while self
            .recent
            .front()
            .is_some_and(|(at, _)| now.duration_since(*at).as_secs_f64() >= 60.0)
        {
            self.recent.pop_front();
        }
        json!({"version":1,"running":self.running,"rounds":self.rounds,"steps":self.steps,
            "errors":self.errors,"input":self.input,"output":self.output,"cacheRead":self.cache_read,
            "cacheWrite":self.cache_write,"cost":self.cost,"ttft":self.ttft,"latency":self.latency,
            "speed":self.speed,"tpm":self.recent.iter().map(|(_,tokens)|tokens).sum::<u64>(),
            "waiting":self.request.map(|start|now.duration_since(start).as_secs_f64())})
    }
}
fn metric(v: &Value, key: &str, precision: usize, suffix: &str) -> String {
    v[key]
        .as_f64()
        .filter(|n| n.is_finite() && *n >= 0.0)
        .map(|n| format!("{n:.precision$}{suffix}"))
        .unwrap_or_else(|| "—".into())
}
fn panel(snapshot: &Value, _anchor: &str) -> Value {
    json!({"version":1,"layout":"footer","width":240,"maxHeight":1,"border":false,
        "lines":[format!("Turns {} · Steps {} · TPS {} · TTFT {} · Latency {} · TPM {} · Err {} · USD {}",
            compact(count(snapshot,"rounds")),compact(count(snapshot,"steps")),
            metric(snapshot,"speed",0,""),metric(snapshot,"ttft",2,"s"),
            metric(snapshot,"latency",2,"s"),compact(count(snapshot,"tpm")),
            compact(count(snapshot,"errors")),metric(snapshot,"cost",4,""))]})
}
fn compact(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}
fn component_action(runtime: &rpi_plugin_sdk::PluginApi, args: Value) -> bool {
    let args = args.to_string();
    let mut out = StbString::empty();
    let rc = (runtime.runtime_action)(
        RuntimeActionId::UiComponent as u32,
        StbStringRef::from_str(&args),
        &mut out,
        runtime.user_data,
    );
    (runtime.free_string)(out);
    rc == 0
}
fn publish(stats: &mut Stats) {
    let Some(runtime) = RUNTIME.get() else {
        return;
    };
    if stats.last_publication.is_none() {
        if !component_action(
            runtime,
            json!({"op":"register","id":KEY,"type":"text","slot":"footer.after","width":240,"maxHeight":1,"border":false,"visible":false}),
        ) {
            return;
        }
    }
    let value = panel(&stats.snapshot(Instant::now()), &stats.anchor);
    let args = json!({"op":"update","id":KEY,"text":value["lines"][0]}).to_string();
    let _ = component_action(
        runtime,
        json!({"op":if stats.enabled {"show"} else {"hide"},"id":KEY}),
    );
    if stats.last_publication.as_deref() == Some(args.as_str()) {
        return;
    }
    let mut out = StbString::empty();
    let rc = (runtime.runtime_action)(
        RuntimeActionId::UiComponent as u32,
        StbStringRef::from_str(&args),
        &mut out,
        runtime.user_data,
    );
    (runtime.free_string)(out);
    if rc == 0 {
        stats.last_publication = Some(args);
    }
}
rpi_plugin_sdk::event_handler! {
fn event(event, _user_data) {
    if event.tag() == EventTag::SessionShutdown {
        if let Some((stop, thread)) = TIMER.lock().unwrap_or_else(|p| p.into_inner()).take() {
            let _ = stop.send(());
            let _ = thread.join();
        }
        let mut stats = state().lock().unwrap_or_else(|p| p.into_inner());
        stats.enabled = false;
        publish(&mut stats);
        return 0;
    }
    let Some(_runtime) = RUNTIME.get() else {
        return 0;
    };
    // Events are borrowed during host fan-out; the host frees their payloads
    // once all subscribers have returned (dispatch_to_handlers contract).
    let payload = match event.tag() {
        EventTag::MessageUpdate | EventTag::MessageEnd => {
            event.message_text()
        }
        EventTag::BeforeProviderRequest => event.data_text(),
        _ => None,
    };
    let value = payload
        .map(|s| {
            let parsed = serde_json::from_str(&s).unwrap_or(Value::Null);
            parsed
        })
        .unwrap_or(Value::Null);
    let mut stats = state().lock().unwrap_or_else(|p| p.into_inner());
    stats.update(event.tag(), &value, Instant::now());
    // Stream deltas only latch TTFT. The one-second timer publishes ongoing
    // timing; request/lifecycle boundaries still publish immediately.
    if event.tag() != EventTag::MessageUpdate {
        publish(&mut stats);
    }
    0
}
}
fn shortcut(stats: &mut Stats, value: &Value) -> i32 {
    use rpi_plugin_sdk::{EVENT_HANDLER_CLAIMED, EVENT_HANDLER_CONTINUE};
    if value["type"] != "key"
        || value["key"] != "f8"
        || ["ctrl", "alt", "shift"]
            .iter()
            .any(|key| value[*key].as_bool().unwrap_or(false))
    {
        return EVENT_HANDLER_CONTINUE;
    }
    match value["kind"].as_str() {
        Some("press") => {
            stats.enabled = !stats.enabled;
            EVENT_HANDLER_CLAIMED
        }
        Some("repeat" | "release") => EVENT_HANDLER_CLAIMED,
        _ => EVENT_HANDLER_CONTINUE,
    }
}
rpi_plugin_sdk::event_handler! {
fn input(event, _user_data) {
    let raw = event.data_text().unwrap_or_default();
    let value = serde_json::from_str(&raw).unwrap_or(Value::Null);
    let mut stats = state().lock().unwrap_or_else(|p| p.into_inner());
    let enabled = stats.enabled;
    let result = shortcut(&mut stats, &value);
    if stats.enabled != enabled {
        publish(&mut stats);
    }
    result
}
}
rpi_plugin_sdk::string_handler! {
fn command(args, out, _user_data) {
    if !out.is_available() {
        return 1;
    }
    let mut stats = state().lock().unwrap_or_else(|p| p.into_inner());
    let envelope: Value = serde_json::from_str(args).unwrap_or(Value::Null);
    let text = match envelope
        .get("args")
        .and_then(Value::as_str)
        .unwrap_or("help")
        .trim()
    {
        "on" => {
            stats.enabled = true;
            "运行监控已打开"
        }
        "off" => {
            stats.enabled = false;
            "运行监控已关闭（继续统计）"
        }
        "" | "toggle" => {
            stats.enabled = !stats.enabled;
            if stats.enabled {
                "运行监控已打开"
            } else {
                "运行监控已关闭（继续统计）"
            }
        }
        position if position.starts_with("position ") => {
            "监控固定显示在用量统计下方，无需设置位置。"
        }
        _ => "用法：/stats [on|off|toggle]，也可使用 F8 切换",
    };
    publish(&mut stats);
    out.write(json!({"kind":"message","text":text}).to_string());
    0
}
}
rpi_plugin_sdk::export_plugin!(|api| {
    let _ = RUNTIME.set(*api);
    let Some(register) = api.register_event_handler else {
        return 1;
    };
    for tag in [
        EventTag::SessionStart,
        EventTag::SessionShutdown,
        EventTag::AgentStart,
        EventTag::AgentEnd,
        EventTag::BeforeProviderRequest,
        EventTag::MessageUpdate,
        EventTag::MessageEnd,
    ] {
        if register(tag, event, std::ptr::null_mut()) != 0 {
            return 1;
        }
    }
    if register(EventTag::Input, input, std::ptr::null_mut()) != 0 {
        return 1;
    }
    let Some(register_shortcut) = api.register_shortcut else {
        return 1;
    };
    if register_shortcut(
        StbStringRef::from_str("f8"),
        StbStringRef::from_str("Toggle runtime monitor (F8)"),
    ) != 0
    {
        return 1;
    }
    let Some(register) = api.register_command else {
        return 1;
    };
    if register(
        StbStringRef::from_str("stats"),
        StbStringRef::from_str("Footer runtime monitor: /stats on|off|toggle (F8)"),
        command,
    ) != 0
    {
        return 1;
    }
    publish(&mut state().lock().unwrap_or_else(|p| p.into_inner()));
    // Keep waiting time and the rolling TPM window fresh even without streaming events.
    let mut timer = TIMER.lock().unwrap_or_else(|p| p.into_inner());
    if timer.is_none() {
        let (stop, receiver) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || {
            while matches!(
                receiver.recv_timeout(std::time::Duration::from_secs(1)),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout)
            ) {
                publish(&mut state().lock().unwrap_or_else(|p| p.into_inner()));
            }
        });
        *timer = Some((stop, thread));
    }
    0
});

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn f8_toggles_once_and_modified_or_unrelated_keys_are_not_claimed() {
        use rpi_plugin_sdk::{EVENT_HANDLER_CLAIMED, EVENT_HANDLER_CONTINUE};
        let mut stats = Stats::default();
        for kind in ["press", "repeat", "release"] {
            assert_eq!(
                shortcut(&mut stats, &json!({"type":"key","key":"f8","kind":kind})),
                EVENT_HANDLER_CLAIMED
            );
            assert!(stats.enabled);
        }
        for value in [
            json!({"type":"key","key":"f8","kind":"press","ctrl":true}),
            json!({"type":"key","key":"f7","kind":"press"}),
            json!({"type":"key","key":"f8","kind":"invalid"}),
        ] {
            assert_eq!(shortcut(&mut stats, &value), EVENT_HANDLER_CONTINUE);
            assert!(stats.enabled);
        }
        assert_eq!(
            shortcut(&mut stats, &json!({"type":"key","key":"f8","kind":"press"})),
            EVENT_HANDLER_CLAIMED
        );
        assert!(!stats.enabled);
    }
    #[test]
    fn monitor_defaults_to_off_and_uses_compact_docked_metrics() {
        let mut stats = Stats::default();
        assert!(!stats.enabled);
        let view = panel(&stats.snapshot(Instant::now()), "top-right");
        assert_eq!(view["layout"], "footer");
        assert_eq!(view["maxHeight"], 1);
        assert_eq!(view["lines"].as_array().unwrap().len(), 1);
        assert!(view["lines"][0].as_str().unwrap().contains("TTFT"));
    }
    #[test]
    fn command_accepts_host_envelope_and_rejects_unknown_arguments() {
        for (args, enabled) in [
            ("off", false),
            ("on", true),
            ("toggle", false),
            ("", true),
            ("unknown", true),
            ("position bottom-left", true),
            ("position invalid", true),
        ] {
            let input = json!({"args":args,"command":"stats"}).to_string();
            let mut out = StbString::empty();
            assert_eq!(
                command(
                    StbStringRef::from_str(&input),
                    &mut out,
                    std::ptr::null_mut()
                ),
                0
            );
            assert_eq!(state().lock().unwrap().enabled, enabled);
            if args.starts_with("position ") {
                assert_eq!(state().lock().unwrap().anchor, "top-right");
            }
            let response: Value = serde_json::from_str(&out.to_string_lossy()).unwrap();
            assert_eq!(response["kind"], "message");
            if args == "unknown" {
                assert!(response["text"].as_str().unwrap().contains("用法"));
            }
            // The command output uses the SDK boxed-slice allocation contract.
            rpi_plugin_sdk::callbacks::free_string(out);
        }
    }
    #[test]
    fn counts_requests_and_real_usage_without_counting_tool_messages_or_duplicate_ends() {
        let mut s = Stats::default();
        let now = Instant::now();
        s.update(EventTag::AgentStart, &Value::Null, now);
        s.update(EventTag::BeforeProviderRequest, &Value::Null, now);
        s.update(
            EventTag::MessageUpdate,
            &json!({"assistantMessageEvent":{"type":"start"}}),
            now,
        );
        assert_eq!(s.ttft, None);
        s.update(
            EventTag::MessageUpdate,
            &json!({"assistantMessageEvent":{"type":"thinking_delta","delta":"x"}}),
            now + std::time::Duration::from_secs(1),
        );
        let message = json!({"role":"assistant","usage":{"input":100,"output":252,"cacheRead":20,"cacheWrite":0,"cost":{"total":0.01}}});
        s.update(EventTag::MessageEnd, &json!({"role":"toolResult"}), now);
        s.update(
            EventTag::MessageEnd,
            &message,
            now + std::time::Duration::from_secs(2),
        );
        s.update(
            EventTag::MessageEnd,
            &message,
            now + std::time::Duration::from_secs(2),
        );
        assert_eq!((s.rounds, s.steps, s.input, s.output), (1, 1, 100, 252));
        assert_eq!(s.ttft, Some(1.0));
        assert_eq!(s.speed, Some(126.0));
        assert_eq!(
            s.snapshot(now + std::time::Duration::from_secs(3))["tpm"],
            372
        );
        assert_eq!(
            s.snapshot(now + std::time::Duration::from_secs(62))["tpm"],
            0
        );
    }
    #[test]
    fn session_reset_preserves_off_and_nonstreaming_has_no_invented_ttft() {
        let mut s = Stats::default();
        let now = Instant::now();
        s.enabled = false;
        s.anchor = "bottom-left".into();
        s.update(EventTag::BeforeProviderRequest, &Value::Null, now);
        s.update(
            EventTag::MessageEnd,
            &json!({"role":"assistant","stopReason":"error"}),
            now,
        );
        assert_eq!(s.ttft, None);
        assert_eq!(s.errors, 1);
        s.update(EventTag::SessionStart, &Value::Null, now);
        assert!(!s.enabled);
        assert_eq!(s.anchor, "bottom-left");
        assert_eq!(s.steps, 0);
    }
}
