use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct AppConfig {
    pub claude: ClaudeSection,
    pub scheduler: SchedulerSection,
    pub storage: StorageSection,
    pub server: ServerSection,
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
    pub timezone: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct StorageSection {
    pub db_path: String,
    pub vault_path: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServerSection {
    pub port: u16,
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
