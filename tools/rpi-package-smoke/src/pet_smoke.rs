use rpi_extensions::{
    host_free_string, load_session_mixed, ActionBridge, ExtensionPanel, ExtensionStatusMailbox,
    NullDiagnostics, PanelAnchor, RuntimeActionHost,
};
use rpi_plugin_sdk::{EventTag, StablePluginEvent, StbString, StbStringRef};
use serde_json::{json, Value};
use std::sync::Arc;

/// The status mailbox publishes panels under `component:{order}:{owner}:{id}` keys
/// (order and owner were added for TUI layout / multi-owner disambiguation), so
/// the id is the trailing segment rather than the whole key.
fn find_panel(mailbox: &ExtensionStatusMailbox, id: &str) -> Option<ExtensionPanel> {
    let suffix = format!(":{id}");
    mailbox
        .panels()
        .into_iter()
        .find(|(key, _)| key.ends_with(&suffix))
        .map(|(_, panel)| panel)
}

fn has_panel(mailbox: &ExtensionStatusMailbox, id: &str) -> bool {
    find_panel(mailbox, id).is_some()
}

struct NoAgentHost;
macro_rules! no_agent_host {
    ($($name:ident),+ $(,)?) => {
        #[async_trait::async_trait]
        impl RuntimeActionHost for NoAgentHost {
            $(async fn $name(&self, _: Value) -> Result<Value,String> {
                Err(format!("unexpected agent action: {}",stringify!($name)))
            })+
        }
    };
}
no_agent_host!(
    send_message,
    send_user_message,
    append_entry,
    set_session_name,
    get_active_tools,
    set_active_tools,
    set_model,
    get_thinking_level,
    set_thinking_level,
    compact,
    get_system_prompt,
    new_session,
    fork,
    navigate_tree,
    switch_session,
    reload
);

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set RPI_VOICE_DLL to the installed voice DLL; does not open microphone"]
async fn installed_pet_commands_and_events_round_trip_without_audio() {
    std::env::set_var("RPI_VOICE_AUTO_TTS", "off");
    let Some(dll) = crate::smoke_fixtures::fixture_path("RPI_VOICE_DLL") else {
        return;
    };
    let bridge = ActionBridge::new(tokio::runtime::Handle::current(), Arc::new(NoAgentHost));
    let mailbox = bridge.extension_status_mailbox();
    let session = load_session_mixed(&[], &[dll], Arc::new(NullDiagnostics), Some(bridge));
    let snapshot = session.snapshot().unwrap();
    let pet = snapshot
        .commands()
        .iter()
        .find(|c| c.name == "pet")
        .expect("pet registered");
    let run = |args: &str| {
        let input = json!({"command":"pet","args":args}).to_string();
        let mut out = StbString::empty();
        let rc = (pet.handler)(StbStringRef::from_str(&input), &mut out, pet.user_data);
        // SAFETY: the handler returned a live SDK-owned allocation; copy it
        // before freeing it exactly once with the host allocator.
        let value: Value = serde_json::from_str(&unsafe { out.to_string_lossy() }).unwrap();
        unsafe { host_free_string(out) };
        assert_eq!(rc, 0);
        value["details"]["state"].clone()
    };
    let state = run("quiet");
    assert_eq!(state["enabled"], true);
    assert_eq!(state["auto"], false);
    assert_eq!(state["mood"], "sleep");
    let panel = find_panel(&mailbox, "rpi.pet").expect("rpi.pet panel registered");
    panel.validate().unwrap();
    assert_eq!(panel.anchor, PanelAnchor::Center);
    assert!(panel.lines.join("\n").contains("安静陪着你"));
    assert!(
        mailbox.get("rpi.pet").is_none(),
        "pet must not trigger the legacy domain renderer"
    );
    assert_eq!(run("bunny")["animal"], "bunny");
    assert_eq!(run("name 团团")["name"], "团团");
    assert_eq!(run("work")["layout"], "work");
    assert_eq!(
        find_panel(&mailbox, "rpi.pet").unwrap().anchor,
        PanelAnchor::RightCenter
    );
    assert_eq!(run("name bad\u{1b}name")["name"], "团团");
    let message = StbString::from_string(
        json!({"role":"assistant","content":[{"type":"text","text":"你好，测试成功了。"}]})
            .to_string(),
    );
    for handler in snapshot.handlers_for(EventTag::MessageEnd) {
        assert_eq!(
            (handler.handler)(
                StablePluginEvent::message(EventTag::MessageEnd, message),
                handler.user_data
            ),
            0
        );
    }
    // SAFETY: the handler borrowed `message`; it is still live and owned by this
    // test, which releases it exactly once here.
    unsafe { host_free_string(message) };
    assert_eq!(run("focus")["caption"], "你好，测试成功了。");
    assert!(find_panel(&mailbox, "rpi.pet")
        .unwrap()
        .lines
        .join("\n")
        .contains("你好，测试成功了。"));
    for handler in snapshot.handlers_for(EventTag::AgentStart) {
        assert_eq!(
            (handler.handler)(
                StablePluginEvent::empty(EventTag::AgentStart),
                handler.user_data
            ),
            0
        );
    }
    let before = mailbox.revision();
    std::thread::sleep(std::time::Duration::from_millis(700));
    assert!(
        mailbox.revision() > before,
        "plugin must publish animation frames through the generic interface"
    );
    let voice = snapshot
        .commands()
        .iter()
        .find(|c| c.name == "voice")
        .unwrap();
    let input = r#"{"args":"status","command":"voice"}"#;
    let mut out = StbString::empty();
    assert_eq!(
        (voice.handler)(StbStringRef::from_str(input), &mut out, voice.user_data),
        0
    );
    // SAFETY: the handler returned a live SDK-owned allocation; copy it before
    // freeing it exactly once with the host allocator.
    let text = unsafe { out.to_string_lossy() };
    unsafe { host_free_string(out) };
    assert!(
        text.contains("disabled"),
        "quiet must disable speech: {text}"
    );
    assert_eq!(run("off")["enabled"], false);
    assert!(!has_panel(&mailbox, "rpi.pet"));
    for handler in snapshot.handlers_for(EventTag::AgentEnd) {
        assert_eq!(
            (handler.handler)(
                StablePluginEvent::empty(EventTag::AgentEnd),
                handler.user_data
            ),
            0
        );
    }
    assert_eq!(run("status")["enabled"], false);
    assert!(!has_panel(&mailbox, "rpi.pet"));
    run("quiet");
    assert!(has_panel(&mailbox, "rpi.pet"));
    for handler in snapshot.handlers_for(EventTag::SessionShutdown) {
        assert_eq!(
            (handler.handler)(
                StablePluginEvent::empty(EventTag::SessionShutdown),
                handler.user_data
            ),
            0
        );
    }
    std::thread::sleep(std::time::Duration::from_millis(400));
    assert!(
        !has_panel(&mailbox, "rpi.pet"),
        "shutdown must remove the panel and join the animation worker"
    );
}
