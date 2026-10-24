//! Negotiated ACP v1 extension for questions; permissions keep their standard method.
use serde_json::{json, Value};

pub const REQUEST_METHOD: &str = "_rpi/request_question";
pub const CLOSED_METHOD: &str = "_rpi/dialog_closed";
const MAX_TEXT: usize = 65_536;

pub fn request(event: &Value, session_id: &str) -> Option<Value> {
    let ui = event.get("ui")?;
    if !matches!(
        ui.get("purpose").and_then(Value::as_str),
        None | Some("question")
    ) {
        return None;
    }
    rpi_plugin_sdk::dialogs::validate(ui, event.get("toolCallId").and_then(Value::as_str)).ok()?;
    let kind = ui.get("kind")?.as_str()?;
    if !matches!(kind, "input" | "selector" | "confirm") {
        return None;
    }
    let question = ui.get("question").or_else(|| ui.get("message"))?.as_str()?;
    if question.trim().is_empty() || question.len() > MAX_TEXT {
        return None;
    }
    let request_id = event.get("requestId")?.as_str()?;
    if request_id.is_empty() {
        return None;
    }
    let options = titles(ui)?;
    if kind != "input" && options.is_empty() {
        return None;
    }
    Some(json!({"sessionId":session_id, "requestId":request_id,
        "toolCallId":event.get("toolCallId"), "ui":ui}))
}

use rpi_extcore::dialogs::titles;
pub use rpi_extcore::dialogs::{answer, timeout};

#[cfg(test)]
mod tests {
    use super::*;
    fn ui() -> Value {
        json!({"kind":"selector","question":"语言？","options":[{"title":"Rust"},{"title":"Python"}],"allowMultiple":true,"allowComment":true})
    }
    #[test]
    fn multiple_choices_and_comment_survive() {
        assert_eq!(
            answer(
                "q",
                &ui(),
                &json!({"answer":{"values":["Rust","Python"],"text":"一起使用"}})
            ),
            json!({"type":"ui_dialog_response","requestId":"q","answer":{"values":["Rust","Python"],"text":"一起使用"}})
        );
    }
    #[test]
    fn invalid_and_duplicate_choices_are_cancelled() {
        for values in [
            json!(["Go"]),
            json!(["Rust", "Rust"]),
            json!([1]),
            json!([]),
        ] {
            assert_eq!(
                answer("q", &ui(), &json!({"answer":{"values":values}}))["cancelled"],
                true
            );
        }
        let mut single = ui();
        single["allowMultiple"] = json!(false);
        assert_eq!(
            answer(
                "q",
                &single,
                &json!({"answer":{"values":["Rust","Python"]}})
            )["cancelled"],
            true
        );
    }
    #[test]
    fn input_and_freeform_are_preserved() {
        let input = json!({"kind":"input","question":"端口？"});
        assert_eq!(
            answer("q", &input, &json!({"answer":{"values":["7897"]}}))["answer"],
            json!({"values":["7897"],"text":"7897"})
        );
        let mut custom = ui();
        custom["allowFreeform"] = json!(true);
        assert_eq!(
            answer("q", &custom, &json!({"answer":{"values":["Go"]}}))["answer"]["values"],
            json!(["Go"])
        );
    }
    #[test]
    fn permission_and_unknown_ui_never_use_question_extension() {
        let mut event = json!({"requestId":"q","ui":ui()});
        assert!(request(&event, "s").is_some());
        event["ui"]["purpose"] = json!("permission");
        assert!(request(&event, "s").is_none());
        event["ui"]["purpose"] = json!("future");
        assert!(request(&event, "s").is_none());
        event["ui"].as_object_mut().unwrap().remove("purpose");
        event["ui"]["kind"] = json!("message");
        assert!(request(&event, "s").is_none());
    }

    #[test]
    fn comments_and_input_text_cannot_override_an_unrelated_choice() {
        let selector = json!({"kind":"selector", "options":["Yes","No"]});
        assert_eq!(
            answer("q", &selector, &json!({"answer":{"values":["No"]}}))["answer"]["values"],
            json!(["No"])
        );
        assert_eq!(
            answer(
                "q",
                &selector,
                &json!({"answer":{"values":["Yes"],"text":"extra"}})
            )["cancelled"],
            true
        );
        let input = json!({"kind":"input"});
        assert_eq!(
            answer(
                "q",
                &input,
                &json!({"answer":{"values":["one"],"text":"two"}})
            )["cancelled"],
            true
        );
    }
}
