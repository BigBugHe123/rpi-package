//! DeepSeek policy belongs to this optional package. Wire parsing, retries and
//! the native streaming ABI use the maintained provider protocol and bridge.
use rpi_ai::{
    model::{MaxTokensField, OpenaiCompletionsCompat, StreamingProtocolCompat},
    types::ModelCostRates,
    Api, AssistantMessageEventStream, Context, Model, Provider, SimpleStreamOptions, ThinkingLevel,
};
use serde_json::json;
use std::{collections::BTreeMap, sync::Arc};

pub const PROVIDER_ID: &str = "deepseek";
pub const DEFAULT_MODEL_ID: &str = "deepseek-v4-pro";
pub const MODEL_ID: &str = "deepseek-flash";
const BASE_URL: &str = "https://api.deepseek.com/v1";

fn compatibility() -> OpenaiCompletionsCompat {
    OpenaiCompletionsCompat {
        supports_store: Some(false),
        supports_developer_role: Some(false),
        supports_strict_mode: Some(false),
        supports_reasoning_effort: Some(true),
        requires_reasoning_content: Some(true),
        // The wire `reasoning_content` is part of the completion, and DeepSeek
        // bills those hidden tokens inside `max_tokens`. Declaring it lets the
        // provider protocol reserve the active thinking level's budget on top of
        // the answer allowance; without it a long chain of thought consumed the
        // whole 8k ceiling and the answer arrived empty with
        // `finish_reason: "length"`.
        reasoning_counts_toward_max_tokens: Some(true),
        max_tokens_field: Some(MaxTokensField::MaxTokens),
        ..Default::default()
    }
}

pub fn models() -> Vec<Model> {
    [
        (DEFAULT_MODEL_ID, "DeepSeek V4 Pro"),
        (MODEL_ID, "DeepSeek Flash"),
    ]
    .into_iter()
    .map(|(id, name)| {
        let mut model = Model::new(id, name, Api::OpenaiCompletions, PROVIDER_ID, BASE_URL);
        model.reasoning = true;
        model.context_window = 1_000_000;
        // The visible answer's allowance. Thinking room is added on top by the
        // provider protocol (`reasoningCountsTowardMaxTokens`), and the server
        // accepts well above this — it is a per-request default, not its maximum.
        model.max_tokens = 16_384;
        model.thinking_level_map = Some(BTreeMap::from([
            (ThinkingLevel::Xhigh, Some("high".into())),
            (ThinkingLevel::Max, Some("max".into())),
        ]));
        model.compat = Some(StreamingProtocolCompat::OpenaiCompletions(compatibility()));
        // Peak USD / million tokens, checked 2026-10-16. Off-peak billing
        // can be lower; users can override estimates in models.json.
        model.cost.rates = if id == MODEL_ID {
            ModelCostRates {
                input: 0.3,
                output: 1.2,
                cache_read: 0.006,
                cache_write: 0.0,
            }
        } else {
            ModelCostRates {
                input: 1.32,
                output: 3.96,
                cache_read: 0.044,
                cache_write: 0.0,
            }
        };
        model
    })
    .collect()
}

/// The former core entry point, now owned by the optional provider crate.
/// No process environment is read here; the host supplies scoped credentials.
#[derive(Clone)]
pub struct DeepSeekProvider {
    api_key: Option<String>,
    models: Vec<Model>,
}

impl DeepSeekProvider {
    pub fn new(api_key: Option<String>) -> Self {
        Self {
            api_key,
            models: models(),
        }
    }
}

#[async_trait::async_trait]
impl Provider for DeepSeekProvider {
    fn id(&self) -> &str {
        PROVIDER_ID
    }
    fn models(&self) -> &[Model] {
        &self.models
    }

    async fn stream_simple(
        &self,
        model: &Model,
        context: &Context,
        options: &SimpleStreamOptions,
    ) -> AssistantMessageEventStream {
        let mut model = model.clone();
        model.provider = PROVIDER_ID.into();
        if model.base_url.is_empty() {
            model.base_url = BASE_URL.into();
        }
        let compat = model
            .compat
            .get_or_insert_with(|| StreamingProtocolCompat::OpenaiCompletions(compatibility()));
        if let StreamingProtocolCompat::OpenaiCompletions(compat) = compat {
            // Tool continuation requires prior reasoning, including assistant
            // turns without a tool call. Preserve other user compat overrides.
            compat.requires_reasoning_content = Some(true);
        }
        let params = model
            .sampling_params
            .get_or_insert_with(serde_json::Map::new);
        // The host represents reasoning=off as None as well as Some(Off).
        // Follow SimpleStreamOptions semantics rather than the API's default.
        let level = options.reasoning_level();
        params.insert(
            "thinking".into(),
            json!({
                "type": if level == ThinkingLevel::Off { "disabled" } else { "enabled" }
            }),
        );
        if level == ThinkingLevel::Off {
            params.remove("reasoning_effort");
        } else {
            let effort = match level {
                ThinkingLevel::Minimal | ThinkingLevel::Low => "low",
                ThinkingLevel::Max => "max",
                _ => "high",
            };
            params.insert("reasoning_effort".into(), json!(effort));
        }
        let mut options = options.clone();
        if options.api_key.is_none() {
            options.api_key = self.api_key.clone();
        }
        let protocol = rpi_plugin_sdk::provider::completions(&model, &options, false);
        protocol.stream_simple(&model, context, &options).await
    }
}

fn create(_: &Model, options: &SimpleStreamOptions) -> Arc<dyn Provider> {
    Arc::new(DeepSeekProvider::new(options.api_key.clone()))
}

crate::registration::register_provider!(
    PROVIDER_ID,
    BASE_URL,
    "openai-completions",
    json!({
        "id": PROVIDER_ID, "models": models(),
        "apiKeyEnv": "DEEPSEEK_API_KEY", "requiresAuth": true
    }),
    create
);
