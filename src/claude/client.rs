//! HTTP client for the Anthropic Messages API.

use anthropic_ai_sdk::client::AnthropicClient;
use anthropic_ai_sdk::types::message::{
    ContentBlock, CreateMessageParams, Message, MessageClient, MessageError, Role,
};

pub struct ClaudeClient {
    inner: AnthropicClient,
    model: String,
    max_tokens: u32,
}

impl ClaudeClient {
    pub fn new(api_key: String, model: String, max_tokens: u32) -> Result<Self, MessageError> {
        let inner = AnthropicClient::new::<MessageError>(api_key, AnthropicClient::DEFAULT_API_VERSION)?;
        Ok(Self {
            inner,
            model,
            max_tokens,
        })
    }

    pub async fn complete(
        &self,
        system: Option<&str>,
        user_message: &str,
    ) -> anyhow::Result<String> {
        let params = CreateMessageParams {
            model: self.model.clone(),
            max_tokens: self.max_tokens,
            messages: vec![Message::new_text(Role::User, user_message)],
            system: system.map(str::to_string),
            ..Default::default()
        };

        let response = self
            .inner
            .create_message(Some(&params))
            .await
            .map_err(anyhow::Error::from)?;

        let text = response
            .content
            .into_iter()
            .filter_map(|c| match c {
                ContentBlock::Text { text } => Some(text),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("");

        Ok(text)
    }
}
