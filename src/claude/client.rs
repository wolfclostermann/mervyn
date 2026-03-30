use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize)]
pub struct ClaudeRequest {
    pub model: String,
    pub max_tokens: u32,
    pub system: Option<String>,
    pub messages: Vec<ClaudeMessage>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ClaudeMessage {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Deserialize)]
pub struct ClaudeResponse {
    pub content: Vec<ClaudeContent>,
}

#[derive(Debug, Deserialize)]
pub struct ClaudeContent {
    #[serde(rename = "type")]
    pub kind: String,
    pub text: Option<String>,
}

pub struct ClaudeClient {
    _http: reqwest::Client,
    _api_key: String,
    _model: String,
}

impl ClaudeClient {
    pub fn new(api_key: String, model: String) -> Self {
        Self {
            _http: reqwest::Client::new(),
            _api_key: api_key,
            _model: model,
        }
    }

    pub async fn complete(
        &self,
        _system: Option<&str>,
        _user_message: &str,
    ) -> anyhow::Result<String> {
        anyhow::bail!("Claude client not wired yet")
    }
}
