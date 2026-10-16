use rpi_extensions::{load_session_mixed, NullDiagnostics};
use rpi_plugin_sdk::{StbString, StbStringRef};
use serde_json::Value;
use std::sync::Arc;

#[test]
#[ignore = "set RPI_PLAN_DLL to the installed plan-mode DLL"]
fn installed_plan_show_returns_structured_full_plan_display() {
    let Some(dll) = crate::smoke_fixtures::fixture_path("RPI_PLAN_DLL") else {
        return;
    };
    let session = load_session_mixed(&[], &[dll], Arc::new(NullDiagnostics), None);
    let snapshot = session.snapshot().unwrap();
    assert!(snapshot
        .tools()
        .iter()
        .any(|tool| tool.tool.name == "plan_mode_complete"));
    let command = snapshot
        .commands()
        .iter()
        .find(|command| command.name == "plan")
        .unwrap();
    let mut output = StbString::empty();
    let rc = (command.handler)(
        StbStringRef::from_str(r#"{"command":"/plan","args":"show"}"#),
        &mut output,
        command.user_data,
    );
    // SAFETY: the command handler returned a live SDK-owned allocation; copy it
    // before freeing it exactly once with the host allocator.
    let text = unsafe { output.to_string_lossy() };
    unsafe { rpi_extensions::host_free_string(output) };
    assert_eq!(rc, 0);
    let result: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(result["details"]["kind"], "plan");
    assert_eq!(result["details"]["expanded"], true);
    assert_eq!(result["details"]["active"], false);
    assert!(result["text"].as_str().unwrap().contains("Idle"));
}
