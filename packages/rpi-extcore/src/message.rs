use serde_json::{json, Value};
/// Reconcile incremental text with authoritative native snapshots. Reconcile with cumulative backend snapshots so a
/// dropped delta or an early RPC completion cannot silently truncate a reply.
#[derive(Default)]
pub struct MessageProjection {
    pub text: String,
    pub thinking: String,
    pub stop_reason: Option<String>,
    pub error: Option<String>,
    pub repaired: usize,
}

impl MessageProjection {
    pub fn event(&mut self, event: &Value) -> Result<Vec<Value>, String> {
        let kind = event["type"].as_str().unwrap_or("");
        let message = &event["message"];
        if kind == "message_start" && message["role"] == "assistant" {
            *self = Self {
                repaired: self.repaired,
                ..Self::default()
            };
            return Ok(Vec::new());
        }
        if matches!(kind, "message_update" | "message_end") && message["role"] == "assistant" {
            if kind == "message_end" {
                self.stop_reason = message["stopReason"].as_str().map(str::to_owned);
                self.error = message["errorMessage"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned);
            }
            if let Some(blocks) = message["content"].as_array() {
                let mut updates = Vec::new();
                for (block_type, field, variant, sent) in [
                    ("thinking", "thinking", "thinking", &mut self.thinking),
                    ("text", "text", "text", &mut self.text),
                ] {
                    let parts: Vec<&str> = blocks
                        .iter()
                        .filter(|b| b["type"] == block_type)
                        .filter_map(|b| b[field].as_str())
                        .collect();
                    if !parts.is_empty() {
                        if let Some(update) = append_snapshot(sent, &parts.concat(), variant)? {
                            if kind == "message_end" {
                                self.repaired += update["text"].as_str().unwrap_or("").len();
                            }
                            updates.push(update);
                        }
                    }
                }
                // Older RPC senders may omit snapshots. Fall back only when the
                // delta's content kind is absent, never append it twice.
                let delta_type = match event["eventType"].as_str() {
                    Some("text_delta") => "text",
                    Some("thinking_delta") => "thinking",
                    _ => "",
                };
                if kind != "message_end" && !blocks.iter().any(|b| b["type"] == delta_type) {
                    if let Some(update) = self.delta(event) {
                        updates.push(update);
                    }
                }
                return Ok(updates);
            }
        }
        Ok(self.delta(event).into_iter().collect())
    }

    fn delta(&mut self, event: &Value) -> Option<Value> {
        let update = if event["type"] == "message_update" {
            let kind = match event["eventType"].as_str() {
                Some("text_delta") => "text",
                Some("thinking_delta") => "thinking",
                _ => return None,
            };
            let text = event["delta"].as_str().filter(|text| !text.is_empty())?;
            Some(json!({"kind":kind,"text":text}))
        } else {
            None
        };
        if let Some(update) = &update {
            let sent = match update["kind"].as_str() {
                Some("text") => Some(&mut self.text),
                Some("thinking") => Some(&mut self.thinking),
                _ => None,
            };
            if let Some(sent) = sent {
                sent.push_str(update["text"].as_str().unwrap_or(""));
            }
        }
        update
    }

    pub fn complete(&mut self, final_text: &str) -> Result<Option<Value>, String> {
        let update = append_snapshot(&mut self.text, final_text, "text")?;
        if let Some(update) = &update {
            self.repaired += update["text"].as_str().unwrap_or("").len();
        }
        Ok(update)
    }
}

fn append_snapshot(
    sent: &mut String,
    complete: &str,
    variant: &str,
) -> Result<Option<Value>, String> {
    if let Some(suffix) = complete.strip_prefix(sent.as_str()) {
        if suffix.is_empty() {
            return Ok(None);
        }
        let update = json!({"kind":variant,"text":suffix});
        *sent = complete.to_owned();
        return Ok(Some(update));
    }
    // A delayed partial snapshot must not replace or duplicate newer text.
    if sent.starts_with(complete) {
        return Ok(None);
    }
    Err("rpi final message differs from the streamed text; reload this session to recover the saved reply".into())
}
