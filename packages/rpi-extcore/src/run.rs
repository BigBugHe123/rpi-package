//! Native run acceptance, settlement and text repair, independent of a wire protocol.
use crate::message::MessageProjection;
use serde_json::Value;
use tokio::sync::{broadcast, watch, OwnedMutexGuard};

pub struct PromptRun {
    pub events: broadcast::Receiver<Value>,
    pub cancellation: watch::Receiver<u64>,
    pub tracker: RunTracker,
    _lease: OwnedMutexGuard<()>,
}

impl PromptRun {
    pub(crate) fn new(
        events: broadcast::Receiver<Value>,
        cancellation: watch::Receiver<u64>,
        id: Value,
        lease: OwnedMutexGuard<()>,
    ) -> Self {
        Self {
            events,
            cancellation,
            tracker: RunTracker::new(id),
            _lease: lease,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunOutcome {
    Completed,
    Cancelled,
    TokenLimit,
}

#[derive(Default)]
pub struct RunProgress {
    pub deltas: Vec<Value>,
    pub outcome: Option<Result<RunOutcome, String>>,
}

pub struct RunTracker {
    id: Value,
    accepted: bool,
    messages: MessageProjection,
    stream_error: Option<String>,
}

impl RunTracker {
    pub fn new(id: Value) -> Self {
        Self {
            id,
            accepted: false,
            messages: MessageProjection::default(),
            stream_error: None,
        }
    }

    pub fn observe(&mut self, event: &Value, cancelled: bool) -> RunProgress {
        let mut progress = RunProgress::default();
        if event["type"] == "response" && event["id"] == self.id {
            if event["success"] != true {
                progress.outcome = Some(if cancelled {
                    Ok(RunOutcome::Cancelled)
                } else {
                    Err(event["error"]
                        .as_str()
                        .unwrap_or("agent rejected the prompt")
                        .into())
                });
            } else if event.pointer("/data/accepted") != Some(&Value::Bool(true)) {
                progress.outcome = Some(Err("rpi did not acknowledge prompt acceptance".into()));
            } else {
                self.accepted = true;
            }
            return progress;
        }
        if event["type"] == "agent_settled" && self.accepted {
            if let Some(error) = self.stream_error.take() {
                progress.outcome = Some(Err(error));
                return progress;
            }
            if let Some(text) = event["finalText"].as_str() {
                match self.messages.complete(text) {
                    Ok(delta) => progress.deltas.extend(delta),
                    Err(error) => {
                        progress.outcome = Some(Err(error));
                        return progress;
                    }
                }
            }
            progress.outcome = Some(match event["outcome"].as_str().unwrap_or("unknown") {
                "aborted" => Ok(RunOutcome::Cancelled),
                "failed" => Err(event["error"]
                    .as_str()
                    .filter(|error| !error.is_empty())
                    .map(str::to_owned)
                    .or_else(|| self.messages.error.clone())
                    .unwrap_or_else(|| {
                        "rpi run failed before completing the reply; check the agent stderr log"
                            .into()
                    })),
                "completed" if self.messages.stop_reason.as_deref() == Some("length") => {
                    Ok(RunOutcome::TokenLimit)
                }
                "completed" => Ok(RunOutcome::Completed),
                outcome => Err(format!("rpi run did not complete: {outcome}")),
            });
        } else if matches!(event["type"].as_str(), Some("session_end" | "host_closed")) {
            progress.outcome = Some(Err("rpi session ended before the prompt completed".into()));
        } else {
            match self.messages.event(event) {
                Ok(deltas) => progress.deltas = deltas,
                Err(error) => {
                    self.stream_error.get_or_insert(error);
                }
            }
        }
        progress
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn acceptance_is_correlated_and_settlement_repairs_the_tail() {
        let mut run = RunTracker::new(json!("request"));
        assert!(run
            .observe(
                &json!({"type":"response","id":"other","success":true,"data":{"accepted":true}}),
                false
            )
            .outcome
            .is_none());
        assert!(run
            .observe(
                &json!({"type":"agent_settled","outcome":"completed"}),
                false
            )
            .outcome
            .is_none());
        run.observe(
            &json!({"type":"response","id":"request","success":true,"data":{"accepted":true}}),
            false,
        );
        assert!(run
            .observe(&json!({"type":"agent_end"}), false)
            .outcome
            .is_none());
        let update = run.observe(
            &json!({"type":"message_update","eventType":"text_delta","delta":"你好🌊"}),
            false,
        );
        assert_eq!(
            update.deltas,
            json!([{"kind":"text","text":"你好🌊"}])
                .as_array()
                .unwrap()
                .to_vec()
        );
        let update = run.observe(
            &json!({"type":"agent_settled","outcome":"completed","finalText":"你好🌊，完成。"}),
            false,
        );
        assert_eq!(update.deltas[0]["text"], "，完成。");
        assert_eq!(update.outcome, Some(Ok(RunOutcome::Completed)));
    }

    #[test]
    fn incompatible_snapshots_drain_to_settlement_before_failure() {
        let mut run = RunTracker::new(json!(1));
        run.observe(
            &json!({"type":"response","id":1,"success":true,"data":{"accepted":true}}),
            false,
        );
        run.observe(
            &json!({"type":"message_update","eventType":"text_delta","delta":"first"}),
            false,
        );
        let update=run.observe(&json!({"type":"message_end","message":{"role":"assistant","content":[{"type":"text","text":"different"}]}}),false);
        assert!(update.outcome.is_none());
        assert!(run
            .observe(
                &json!({"type":"agent_settled","outcome":"completed"}),
                false
            )
            .outcome
            .unwrap()
            .is_err());
    }
}
