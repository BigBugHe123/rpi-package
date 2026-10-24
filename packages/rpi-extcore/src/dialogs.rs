//! Shared native question answer validation and request deadlines.
use serde_json::{json, Value};
use std::time::Duration;
const MAX_TEXT: usize = 65_536;
pub fn titles(ui: &Value) -> Option<Vec<&str>> {
    let mut titles = Vec::new();
    if let Some(options) = ui.get("options").filter(|v| !v.is_null()) {
        let options = options.as_array()?;
        if options.len() > 64 {
            return None;
        }
        for option in options {
            let title = option
                .as_str()
                .or_else(|| option.get("title").and_then(Value::as_str))?;
            if title.trim().is_empty() || title.len() > MAX_TEXT || titles.contains(&title) {
                return None;
            }
            titles.push(title);
        }
    }
    Some(titles)
}

fn flag(ui: &Value, name: &str, default: bool) -> bool {
    ui.get(name).and_then(Value::as_bool).unwrap_or(default)
}

pub fn timeout(ui: &Value) -> Duration {
    Duration::from_secs(
        ui.get("timeout")
            .and_then(Value::as_u64)
            .unwrap_or(300)
            .clamp(1, 86_400),
    )
}

/// Reject malformed, duplicate and unoffered choices instead of inventing an answer.
pub fn answer(request_id: &str, ui: &Value, result: &Value) -> Value {
    let cancelled =
        || json!({"type":"ui_dialog_response", "requestId":request_id, "cancelled":true});
    if result.get("cancelled").and_then(Value::as_bool) == Some(true) {
        return cancelled();
    }
    let Some(answer) = result.get("answer").and_then(Value::as_object) else {
        return cancelled();
    };
    let Some(values) = answer.get("values").and_then(Value::as_array) else {
        return cancelled();
    };
    let Some(options) = titles(ui) else {
        return cancelled();
    };
    let input = ui.get("kind").and_then(Value::as_str) == Some("input");
    if values.is_empty()
        || values.len() > 64
        || (!flag(ui, "allowMultiple", false) && values.len() != 1)
        || (input && values.len() != 1)
    {
        return cancelled();
    }
    let mut selected = Vec::new();
    for value in values {
        let Some(value) = value.as_str() else {
            return cancelled();
        };
        if value.trim().is_empty()
            || value.len() > MAX_TEXT
            || selected.contains(&value)
            || (!flag(ui, "allowFreeform", input) && !options.contains(&value))
        {
            return cancelled();
        }
        selected.push(value);
    }
    let mut normalized = json!({"values": selected});
    if let Some(text) = answer.get("text") {
        let Some(text) = text.as_str() else {
            return cancelled();
        };
        if text.len() > MAX_TEXT || !(input || flag(ui, "allowComment", false)) {
            return cancelled();
        }
        if input && text != selected[0] {
            return cancelled();
        }
        normalized["text"] = json!(text);
    } else if input {
        normalized["text"] = json!(selected[0]);
    }
    json!({"type":"ui_dialog_response", "requestId":request_id, "answer": normalized})
}
