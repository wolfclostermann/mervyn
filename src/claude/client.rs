//! HTTP client for the Anthropic Messages API (`reqwest` + `rustls`, no SDK).

use anyhow::Context;
use serde::{Deserialize, Serialize};

const API_VERSION: &str = "2023-06-01";
const MESSAGES_URL: &str = "https://api.anthropic.com/v1/messages";

#[derive(Debug, Serialize)]
struct MessagesRequest<'a> {
    model: &'a str,
    max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<&'a str>,
    messages: Vec<RequestMessage<'a>>,
}

#[derive(Debug, Serialize)]
struct RequestMessage<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Debug, Deserialize)]
struct MessagesResponse {
    content: Vec<ResponseContentBlock>,
}

#[derive(Debug, Deserialize)]
struct ResponseContentBlock {
    #[serde(rename = "type")]
    kind: String,
    text: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ApiErrorEnvelope {
    error: ApiErrorDetail,
}

#[derive(Debug, Deserialize)]
struct ApiErrorDetail {
    message: String,
}

pub struct ClaudeClient {
    http: reqwest::Client,
    api_key: String,
    model: String,
    max_tokens: u32,
}

impl ClaudeClient {
    pub fn new(api_key: String, model: String, max_tokens: u32) -> Self {
        Self {
            http: reqwest::Client::new(),
            api_key,
            model,
            max_tokens,
        }
    }

    pub async fn complete(
        &self,
        system: Option<&str>,
        user_message: &str,
    ) -> anyhow::Result<String> {
        let body = MessagesRequest {
            model: &self.model,
            max_tokens: self.max_tokens,
            system,
            messages: vec![RequestMessage {
                role: "user",
                content: user_message,
            }],
        };

        let res = self
            .http
            .post(MESSAGES_URL)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", API_VERSION)
            .json(&body)
            .send()
            .await
            .context("anthropic request")?;

        let status = res.status();
        let bytes = res.bytes().await.context("anthropic read body")?;

        if !status.is_success() {
            let msg = parse_error_body(&bytes).unwrap_or_else(|| {
                String::from_utf8_lossy(&bytes).into_owned()
            });
            anyhow::bail!("anthropic API {}: {}", status, msg);
        }

        let parsed: MessagesResponse =
            serde_json::from_slice(&bytes).context("decode anthropic response")?;

        let text = parsed
            .content
            .into_iter()
            .filter(|b| b.kind == "text")
            .filter_map(|b| b.text)
            .collect::<Vec<_>>()
            .join("");

        Ok(text)
    }
}

fn parse_error_body(bytes: &[u8]) -> Option<String> {
    let e: ApiErrorEnvelope = serde_json::from_slice(bytes).ok()?;
    Some(e.error.message)
}
