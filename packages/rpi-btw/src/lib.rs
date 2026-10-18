//! `/btw`: command-only, host-managed side questions. No plugin worker threads.
#![forbid(unsafe_code)]
// The pinned SDK exports a safe C entrypoint around its internal raw ABI read.
#![allow(clippy::not_unsafe_ptr_arg_deref)]

use rpi_plugin_sdk::{FreeStringFn, PluginApi, RuntimeActionFn, StbString, StbStringRef};
use serde_json::{json, Value};
use std::ffi::c_void;
use std::sync::Mutex;

// Documented host protocol; the workspace's published SDK predates this enum.
// Like rpi-subagents, retain the SDK pin and let the host validate the raw id.
const SIDE_QUESTION: u32 = 25;
const HELP: &str = r#"BTW — independent, read-only side conversation

```text
/btw                 Open or resume the side chat
/btw <question>      Ask or follow up; queue while an answer is running
/btw new <question>  Start a new topic, discarding the old thread
/btw status          Show the latest status and full answer
/btw cancel          Cancel generation and queued questions, keeping completed turns
/btw clear           Cancel and discard the side thread
/btw hide            Hide the side chat (does not cancel)
/btw show            Open or resume the side chat
/btw help            Show this help
```

Follow up in the BTW input box: Enter submits, PgUp/PgDn scroll, Ctrl+End goes to latest, Esc hides, Ctrl+C cancels only BTW. Questions use the current model and a committed conversation excerpt. No tools or main-context writes; bounded side history stays in memory only."#;

#[derive(Clone, Copy)]
struct HostFns {
    action: RuntimeActionFn,
    free: FreeStringFn,
}
// Only host function pointers, NEVER a registration's user_data. Each command
// carries its own live owner binding, including duplicate DLL registrations.
static HOST: Mutex<Option<HostFns>> = Mutex::new(None);

fn request(args: &str) -> Result<Option<Value>, String> {
    let args = args.trim();
    match args {
        "help" => Ok(None),
        "" => Ok(Some(json!({"op":"show"}))),
        "status" | "cancel" | "hide" | "show" | "clear" => Ok(Some(json!({"op":args}))),
        "new" => Err("Usage: /btw new <question>".into()),
        _ => {
            let (op, question) = args
                .strip_prefix("new")
                .filter(|rest| rest.starts_with(char::is_whitespace))
                .map(|rest| ("new", rest.trim()))
                .unwrap_or(("start", args));
            if question.len() > 20_000 {
                return Err("Question must contain at most 20000 UTF-8 bytes.".into());
            }
            Ok(Some(json!({"op":op,"question":question})))
        }
    }
}
fn call(host: HostFns, args: Value, user_data: *mut c_void) -> Result<Value, String> {
    let capabilities = raw_call(host, json!({"op":"capabilities"}), user_data)?;
    if capabilities["protocolVersion"].as_u64().unwrap_or(0) < 2 {
        return Err(
            "Upgrade rpi: BTW side-chat protocol v2 is required; no request was started.".into(),
        );
    }
    raw_call(host, args, user_data)
}
fn raw_call(host: HostFns, args: Value, user_data: *mut c_void) -> Result<Value, String> {
    let encoded = args.to_string();
    let mut out = StbString::empty();
    let rc = (host.action)(
        SIDE_QUESTION,
        StbStringRef::from_str(&encoded),
        &mut out,
        user_data,
    );
    let text = out.to_string_lossy();
    (host.free)(out);
    let value: Value = serde_json::from_str(&text).map_err(|_| {
        format!(
            "Invalid SideQuestion response (status {rc}). Upgrade rpi to a host supporting /btw."
        )
    })?;
    if rc != 0 {
        let error = value["error"].as_str().unwrap_or("SideQuestion failed");
        return Err(format!("{error}\n/btw requires host SideQuestion support (action 25). Upgrade rpi if this action is unsupported; no subprocess fallback is used."));
    }
    Ok(value)
}
fn render(operation: &str, value: &Value) -> String {
    if matches!(operation, "start" | "new") {
        return match value["status"].as_str() {
            Some("queued") => "BTW question queued. It will run after the current answer; follow up in the BTW input box.".into(),
            Some("running" | "started") => "BTW started in the independent side chat. Follow up in its input box; the main task continues unchanged.".into(),
            status => format!("BTW request returned {}. Use /btw status to inspect it.", status.unwrap_or("an unknown status")),
        };
    }
    if operation == "clear" {
        return "BTW thread cleared; generation and queued questions cancelled.".into();
    }
    if operation == "hide" {
        return "BTW side chat hidden; any running question continues.".into();
    }
    if operation == "show" {
        return "BTW side chat opened. Ask or follow up in its input box.".into();
    }
    let status = value["status"].as_str().unwrap_or("unknown");
    let question = value["question"].as_str().unwrap_or("");
    let text = value["text"].as_str().unwrap_or("");
    let error = value["error"].as_str().unwrap_or("");
    let queued = value["queued"].as_array().map_or(0, Vec::len);
    let history = if value["historyTruncated"] == true {
        " · recent history only"
    } else {
        ""
    };
    format!(
        "BTW · {status} · queued {queued}{history}\n\n{question}\n\n{text}{}",
        if error.is_empty() {
            String::new()
        } else {
            format!("\n\n{error}")
        }
    )
}
fn command_text(raw: &str, action: impl FnOnce(Value) -> Result<Value, String>) -> String {
    let result = (|| {
        let envelope: Value =
            serde_json::from_str(raw).map_err(|e| format!("Invalid command: {e}"))?;
        let args = envelope["args"]
            .as_str()
            .ok_or("Missing command arguments")?;
        let Some(payload) = request(args)? else {
            return Ok(HELP.to_owned());
        };
        let op = payload["op"].as_str().unwrap_or("").to_owned();
        action(payload).map(|value| render(&op, &value))
    })();
    result.unwrap_or_else(|e: String| format!("Unable to run /btw: {e}"))
}
rpi_plugin_sdk::string_handler! {
fn btw_command(input, out, user_data) {
    if !out.is_available() { return 1; }
    let host = *HOST.lock().unwrap_or_else(|e| e.into_inner());
    let text = command_text(input, |args| {
        call(host.ok_or("BTW host runtime is unavailable")?, args, user_data)
    });
    out.write(json!({"kind":"message","text":text,"markdown":true}).to_string());
    0
}
}
fn register(api: &PluginApi) -> i32 {
    let Some(register_command) = api.register_command else {
        return 1;
    };
    *HOST.lock().unwrap_or_else(|e| e.into_inner()) = Some(HostFns {
        action: api.runtime_action,
        free: api.free_string,
    });
    register_command(
        StbStringRef::from_str("btw"),
        StbStringRef::from_str(
            "Ask an independent side question without interrupting the main task",
        ),
        btw_command,
    )
}
rpi_plugin_sdk::export_plugin!(register);

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_exact_commands_and_utf8_questions() {
        assert!(request("help").unwrap().is_none());
        for empty in ["", "  "] {
            assert_eq!(request(empty).unwrap(), Some(json!({"op":"show"})));
        }
        for op in ["status", "cancel", "hide", "show", "clear"] {
            assert_eq!(request(op).unwrap(), Some(json!({"op":op})));
        }
        assert_eq!(
            request("  顺便问一下？  ").unwrap().unwrap()["question"],
            "顺便问一下？"
        );
        assert_eq!(
            request("status of this change?").unwrap().unwrap()["op"],
            "start"
        );
        assert!(request(&"中".repeat(6667)).is_err());
        assert!(request(&"a".repeat(20_000)).is_ok());
        assert!(request("new").is_err());
        assert_eq!(
            request("new  新话题").unwrap(),
            Some(json!({"op":"new","question":"新话题"}))
        );
        assert_eq!(request("new\t新话题").unwrap().unwrap()["op"], "new");
        assert_eq!(request("newspaper?").unwrap().unwrap()["op"], "start");
        assert!(request(&format!("new {}", "a".repeat(20_000))).is_ok());
        assert!(request(&format!("new {}", "中".repeat(6667))).is_err());
    }
    #[test]
    fn help_and_invalid_input_do_not_call_host() {
        for input in [r#"{"args":"help"}"#, "not json", "{}"] {
            assert!(!command_text(input, |_| panic!("must not call host")).is_empty());
        }
    }
    #[test]
    fn open_and_queued_feedback_are_not_misreported_as_started() {
        let text = command_text(r#"{"args":""}"#, |payload| {
            assert_eq!(payload, json!({"op":"show"}));
            Ok(json!({"status":"idle"}))
        });
        assert!(text.contains("input box"));
        let text = command_text(r#"{"args":"follow up"}"#, |_| {
            Ok(json!({"status":"queued"}))
        });
        assert!(text.contains("queued"));
        assert!(!text.contains("started"));
        let text = command_text(r#"{"args":"status"}"#, |_| {
            Ok(json!({"status":"running","queued":["next"],"text":"**full answer**"}))
        });
        assert!(text.contains("queued 1"));
        assert!(text.contains("**full answer**"));
    }
    #[test]
    fn routes_without_messages_or_context_writes() {
        let text = command_text(r#"{"args":"为什么？"}"#, |payload| {
            assert_eq!(payload, json!({"op":"start","question":"为什么？"}));
            Ok(json!({"status":"running"}))
        });
        assert!(text.contains("main task continues"));
        let text = command_text(r#"{"args":"status"}"#, |_| {
            Ok(json!({"status":"done","text":"answer","question":"question"}))
        });
        assert!(text.contains("answer"));
        assert!(
            command_text(r#"{"args":"cancel"}"#, |_| Err("unsupported".into()))
                .contains("unsupported")
        );
    }
}

#[cfg(test)]
mod abi_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    static V1_STARTS: AtomicUsize = AtomicUsize::new(0);
    static V2_CALLS: AtomicUsize = AtomicUsize::new(0);
    rpi_plugin_sdk::string_handler! {
        fn v1_host(_action, input, out, _user_data) {
            let args: Value = serde_json::from_str(input).unwrap();
            if args["op"] == "start" { V1_STARTS.fetch_add(1, Ordering::SeqCst); }
            out.write(json!({"status":"idle"}).to_string());
            0
        }
    }
    rpi_plugin_sdk::string_handler! {
        fn v2_host(action, input, out, user_data) {
            if action != SIDE_QUESTION || user_data as usize != 0x42 { return 9; }
            V2_CALLS.fetch_add(1, Ordering::SeqCst);
            let args: Value = serde_json::from_str(input).unwrap();
            let response = if args["op"] == "capabilities" { json!({"protocolVersion":2}) }
                else { json!({"status":"running"}) };
            out.write(response.to_string());
            0
        }
    }
    #[test]
    fn protocol_probe_prevents_silent_old_host_requests() {
        let error = call(
            HostFns {
                action: v1_host,
                free: rpi_plugin_sdk::callbacks::free_string,
            },
            json!({"op":"start","question":"q"}),
            0x42usize as *mut c_void,
        )
        .unwrap_err();
        assert!(error.contains("protocol v2"));
        assert_eq!(V1_STARTS.load(Ordering::SeqCst), 0);
    }
    #[test]
    fn protocol_probe_and_request_keep_the_same_owner_binding() {
        let response = call(
            HostFns {
                action: v2_host,
                free: rpi_plugin_sdk::callbacks::free_string,
            },
            json!({"op":"start","question":"q"}),
            0x42usize as *mut c_void,
        )
        .unwrap();
        assert_eq!(response["status"], "running");
        assert_eq!(V2_CALLS.load(Ordering::SeqCst), 2);
    }
    rpi_plugin_sdk::string_handler! {
        fn old_host(action, _input, out, user_data) {
            if action != SIDE_QUESTION || user_data as usize != 0x42 { return 9; }
            out.write(json!({"error":"unknown runtime action id 25"}).to_string());
            2
        }
    }
    #[test]
    fn old_host_reports_upgrade_and_receives_the_callers_handle() {
        let result = call(
            HostFns {
                action: old_host,
                free: rpi_plugin_sdk::callbacks::free_string,
            },
            json!({"op":"start","question":"q"}),
            0x42usize as *mut c_void,
        )
        .unwrap_err();
        assert!(result.contains("unknown runtime action id 25"));
        assert!(result.contains("Upgrade rpi"));
        assert!(result.contains("no subprocess fallback"));
    }
}
