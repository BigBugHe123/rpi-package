//! DeepSeek policy belongs to this optional package. Wire parsing, retries and
//! the native streaming ABI use the maintained provider protocol and bridge.
use rpi_ai::{
    model::{
        MaxTokensField, OpenaiCompletionsCompat, OpenaiThinkingFormat, StreamingProtocolCompat,
    },
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
        thinking_format: Some(OpenaiThinkingFormat::Deepseek),
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

fn thinking_levels() -> BTreeMap<ThinkingLevel, Option<String>> {
    use ThinkingLevel::*;
    [
        (Minimal, "low"),
        (Low, "low"),
        (Medium, "high"),
        (High, "high"),
        (Xhigh, "high"),
        (Max, "max"),
    ]
    .into_iter()
    .map(|(level, effort)| (level, Some(effort.into())))
    .collect()
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
        model.thinking_level_map = Some(thinking_levels());
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
    client: Option<reqwest::Client>,
}

impl DeepSeekProvider {
    pub fn new(api_key: Option<String>) -> Self {
        Self {
            api_key,
            models: models(),
            client: None,
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
            compat.supports_store.get_or_insert(false);
            compat.supports_developer_role.get_or_insert(false);
            compat.supports_strict_mode.get_or_insert(false);
            compat.supports_reasoning_effort.get_or_insert(true);
            compat
                .reasoning_counts_toward_max_tokens
                .get_or_insert(true);
            compat
                .max_tokens_field
                .get_or_insert(MaxTokensField::MaxTokens);
            compat
                .thinking_format
                .get_or_insert(OpenaiThinkingFormat::Deepseek);
        }
        let levels = model
            .thinking_level_map
            .get_or_insert_with(Default::default);
        for (level, effort) in thinking_levels() {
            levels.entry(level).or_insert(effort);
        }
        let mut options = options.clone();
        if options.api_key.is_none() {
            options.api_key = self.api_key.clone();
        }
        crate::transport::stream(self.client.as_ref(), &model, context, &options, false).await
    }
}

fn create(
    context: &rpi_plugin_sdk::provider::ProviderContext,
    model: &Model,
    options: &SimpleStreamOptions,
) -> Result<Arc<dyn Provider>, String> {
    let base = if model.base_url.is_empty() {
        BASE_URL
    } else {
        &model.base_url
    };
    Ok(Arc::new(DeepSeekProvider {
        api_key: options.api_key.clone(),
        models: models(),
        client: Some(context.client(base)?),
    }))
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
