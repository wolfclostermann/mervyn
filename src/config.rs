use std::path::{Path, PathBuf};

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
    /// Optional scheduled `git pull` for a worklog (or vault) repo while Mervyn is running.
    #[serde(default)]
    pub worklog_git: WorklogGitSection,
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

fn default_git_remote() -> String {
    "origin".to_string()
}

fn default_git_branch() -> String {
    "main".to_string()
}

fn default_worklog_git_pull_cron() -> String {
    "0 */5 * * * *".to_string()
}

/// Run `git pull --ff-only` on a schedule (same Tokio scheduler as vault sync). Off by default.
#[derive(Debug, Clone, Deserialize)]
pub struct WorklogGitSection {
    #[serde(default)]
    pub enabled: bool,
    /// Repository working tree (path passed to `git -C`). Relative paths use the process cwd.
    #[serde(default)]
    pub repo_path: String,
    #[serde(default = "default_git_remote")]
    pub remote: String,
    #[serde(default = "default_git_branch")]
    pub branch: String,
    #[serde(default = "default_worklog_git_pull_cron")]
    pub pull_cron: String,
}

impl Default for WorklogGitSection {
    fn default() -> Self {
        Self {
            enabled: false,
            repo_path: String::new(),
            remote: default_git_remote(),
            branch: default_git_branch(),
            pull_cron: default_worklog_git_pull_cron(),
        }
    }
}

impl AppConfig {
    /// When `[worklog_git]` is on, path to `worklog.md` inside that clone — used if vault `worklog.md`
    /// is missing or empty (e.g. broken symlink into an unmounted path in Docker).
    pub fn worklog_md_git_mirror_path(&self) -> Option<PathBuf> {
        if !self.worklog_git.enabled {
            return None;
        }
        let r = self.worklog_git.repo_path.trim();
        if r.is_empty() {
            return None;
        }
        Some(Path::new(r).join("worklog.md"))
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
