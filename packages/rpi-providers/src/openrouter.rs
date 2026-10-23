//! OpenRouter integration, installed independently from the rpi CLI.
use rpi_ai::{Api, AssistantMessageEventStream, Context, Model, Provider, SimpleStreamOptions};
use std::{collections::BTreeMap, sync::Arc};

const BASE_URL: &str = "https://openrouter.ai/api/v1";

pub struct OpenRouterProvider {
    api_key: Option<String>,
    models: Vec<Model>,
    client: Option<reqwest::Client>,
}

impl OpenRouterProvider {
    pub fn new(api_key: Option<String>) -> Self {
        Self {
            api_key,
            models: vec![default_model()],
            client: None,
        }
    }
}

#[async_trait::async_trait]
impl Provider for OpenRouterProvider {
    fn id(&self) -> &str {
        "openrouter"
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
        let mut options = options.clone();
        if options.api_key.is_none() {
            options.api_key = self.api_key.clone();
        }
        let headers = options.headers.get_or_insert_with(BTreeMap::new);
        for (name, value) in [
            ("HTTP-Referer", "https://rpi.laofu.online/"),
            ("X-Title", "rpi"),
        ] {
            let configured_on_model = model
                .headers
                .as_ref()
                .is_some_and(|headers| headers.keys().any(|key| key.eq_ignore_ascii_case(name)));
            if !configured_on_model && !headers.keys().any(|key| key.eq_ignore_ascii_case(name)) {
                headers.insert(name.into(), value.into());
            }
        }
        crate::transport::stream(self.client.as_ref(), model, context, &options, false).await
    }
}

fn default_model() -> Model {
    let mut model = Model::new(
        "openrouter/auto",
        "OpenRouter Auto",
        Api::OpenaiCompletions,
        "openrouter",
        BASE_URL,
    );
    model.context_window = 128_000;
    model.max_tokens = 16_384;
    model
}

fn create(
    context: &rpi_plugin_sdk::provider::ProviderContext,
    model: &Model,
    options: &SimpleStreamOptions,
) -> Result<Arc<dyn Provider>, String> {
    Ok(Arc::new(OpenRouterProvider {
        api_key: options.api_key.clone(),
        models: vec![model.clone()],
        client: Some(context.client(&model.base_url)?),
    }))
}

crate::registration::register_provider!(
    "openrouter",
    BASE_URL,
    "openai-completions",
    serde_json::json!({"id":"openrouter", "models":[default_model()], "apiKeyEnv":"OPENROUTER_API_KEY", "requiresAuth":true}),
    create
);
