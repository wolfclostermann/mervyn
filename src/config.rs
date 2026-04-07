use serde::Deserialize;

fn default_slack_ingest_prune_cron() -> String {
    "0 0 4 * * *".to_string()
}

#[derive(Debug, Clone, Deserialize)]
pub struct AppConfig {
    pub claude: ClaudeSection,
    pub scheduler: SchedulerSection,
    pub storage: StorageSection,
    pub server: ServerSection,
    #[serde(default)]
    pub ngrok: NgrokSection,
    #[serde(default)]
    pub user_context: UserContextSection,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ClaudeSection {
    pub model: String,
    pub max_tokens: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SchedulerSection {
    pub morning_briefing_cron: String,
    pub reminder_check_cron: String,
    pub vault_sync_cron: String,
    /// Cron for pruning `slack_ingest` rows (see `[storage]` retention options).
    #[serde(default = "default_slack_ingest_prune_cron")]
    pub slack_ingest_prune_cron: String,
    pub timezone: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct StorageSection {
    pub db_path: String,
    pub vault_path: String,
    /// Delete `slack_ingest` rows older than this many days (`None` / omit = no age pruning).
    #[serde(default)]
    pub slack_ingest_retention_days: Option<u32>,
    /// After age pruning, keep at most this many rows, dropping lowest ids first (`None` = no cap).
    #[serde(default)]
    pub slack_ingest_keep_last: Option<u64>,
    /// Ingest rows still `Pending` after this many minutes are marked failed and Slack `event_id` meta claims released (scheduled with prune). `0` disables.
    #[serde(default = "default_slack_ingest_stale_pending_minutes")]
    pub slack_ingest_stale_pending_minutes: u32,
}

fn default_slack_ingest_stale_pending_minutes() -> u32 {
    30
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServerSection {
    pub port: u16,
}

#[derive(Debug, Clone, Deserialize)]
pub struct NgrokSection {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub domain: Option<String>,
}

impl Default for NgrokSection {
    fn default() -> Self {
        Self {
            enabled: false,
            domain: None,
        }
    }
}

fn default_user_context_enabled() -> bool {
    true
}

fn default_situation_file() -> String {
    "mervyn-situation.md".to_string()
}

fn default_situation_max_chars() -> usize {
    12_000
}

/// Markdown file (under `storage.vault_path`) merged into Claude session JSON for every call.
#[derive(Debug, Clone, Deserialize)]
pub struct UserContextSection {
    #[serde(default = "default_user_context_enabled")]
    pub enabled: bool,
    #[serde(default = "default_situation_file")]
    pub situation_file: String,
    #[serde(default = "default_situation_max_chars")]
    pub situation_max_chars: usize,
}

impl Default for UserContextSection {
    fn default() -> Self {
        Self {
            enabled: default_user_context_enabled(),
            situation_file: default_situation_file(),
            situation_max_chars: default_situation_max_chars(),
        }
    }
}

pub fn load() -> anyhow::Result<AppConfig> {
    let settings = ::config::Config::builder()
        .add_source(::config::File::with_name("config/default"))
        .add_source(
            ::config::Environment::with_prefix("MERVYN")
                .separator("__")
                .try_parsing(true),
        )
        .build()?;
    Ok(settings.try_deserialize()?)
}
