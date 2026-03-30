use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context;
use redb::Database;

use crate::claude::client::ClaudeClient;
use crate::config::AppConfig;
use crate::slack::client::SlackClient;

#[derive(Debug, Clone)]
pub struct Secrets {
    pub anthropic_api_key: String,
    pub slack_bot_token: String,
    pub slack_signing_secret: String,
    pub slack_channel_id: String,
    /// When set, enables `GET /admin/slack-ingest` with `Authorization: Bearer <token>`.
    pub admin_token: Option<String>,
}

impl Secrets {
    pub fn from_env() -> anyhow::Result<Self> {
        Ok(Self {
            anthropic_api_key: std::env::var("ANTHROPIC_API_KEY").context("ANTHROPIC_API_KEY")?,
            slack_bot_token: std::env::var("SLACK_BOT_TOKEN").context("SLACK_BOT_TOKEN")?,
            slack_signing_secret: std::env::var("SLACK_SIGNING_SECRET")
                .context("SLACK_SIGNING_SECRET")?,
            slack_channel_id: std::env::var("SLACK_CHANNEL_ID").context("SLACK_CHANNEL_ID")?,
            admin_token: std::env::var("MERVYN_ADMIN_TOKEN")
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty()),
        })
    }
}

#[derive(Clone)]
pub struct AppState {
    pub settings: Arc<AppConfig>,
    pub secrets: Arc<Secrets>,
    pub db: Arc<Database>,
    pub claude: Arc<ClaudeClient>,
    pub slack: Arc<SlackClient>,
    pub vault_path: PathBuf,
}
