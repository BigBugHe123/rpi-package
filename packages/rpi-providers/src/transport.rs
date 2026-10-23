//! Shared transport without retaining request credentials in the client pool.
use rpi_ai::{
    event_stream::create_assistant_message_event_stream,
    providers::openai_completions::OpenAiCompletionsProvider, AssistantMessage,
    AssistantMessageEvent, AssistantMessageEventStream, Context, Model, Provider,
    SimpleStreamOptions, StopReason,
};
use std::sync::Arc;

pub(crate) async fn stream(
    client: Option<&reqwest::Client>,
    model: &Model,
    context: &Context,
    options: &SimpleStreamOptions,
    anonymous: bool,
) -> AssistantMessageEventStream {
    let protocol: Result<Arc<dyn Provider>, String> = match client {
        Some(client) if anonymous && options.api_key.is_none() => Ok(Arc::new(
            OpenAiCompletionsProvider::with_models_without_auth(
                model.provider.clone(),
                client.clone(),
                vec![model.clone()],
            ),
        )),
        Some(client) => Ok(Arc::new(
            OpenAiCompletionsProvider::with_models_without_env_api_key(
                model.provider.clone(),
                options.api_key.clone(),
                client.clone(),
                vec![model.clone()],
            ),
        )),
        None => rpi_plugin_sdk::provider::completions(model, options, anonymous),
    };
    match protocol {
        Ok(provider) => provider.stream_simple(model, context, options).await,
        Err(error) => {
            let (mut producer, stream) = create_assistant_message_event_stream();
            producer.push(AssistantMessageEvent::Error {
                reason: rpi_ai::types::ErrorReason::Error,
                error: AssistantMessage::terminal(
                    model.api.clone(),
                    model.provider.clone(),
                    model.id.clone(),
                    StopReason::Error,
                    error,
                    0,
                ),
            });
            stream
        }
    }
}
