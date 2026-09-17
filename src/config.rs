
use serde::Deserialize;

fn default_message_ingest_prune_cron() -> String {
    "0 0 4 * * *".to_string()
}

#[derive(Debug, Clone, Deserialize)]
pub struct AppConfig {
    pub claude: ClaudeSection,
    pub scheduler: SchedulerSection,
    pub storage: StorageSection,
    pub server: ServerSection,
    #[serde(default)]
    pub user_context: UserContextSection,
    #[serde(default)]
    pub vault: VaultSection,
    /// Optional scheduled git sync of the vault clone while Mervyn is running.
    #[serde(default)]
    pub vault_git: VaultGitSection,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ClaudeSection {
    pub model: String,
    pub max_tokens: u32,
}

fn default_appointment_reminders_enabled() -> bool {
    true
}

fn default_appointment_reminder_advance_minutes() -> u32 {
    30
}

fn default_appointment_start_grace_minutes() -> u32 {
    30
}

#[derive(Debug, Clone, Deserialize)]
pub struct SchedulerSection {
    pub morning_briefing_cron: String,
    pub reminder_check_cron: String,
    pub vault_sync_cron: String,
    /// Cron for pruning `message_ingest` rows (see `[storage]` retention options).
    #[serde(default = "default_message_ingest_prune_cron")]
    pub message_ingest_prune_cron: String,
    pub timezone: String,
    /// Post chat for calendar events this many minutes before `start`, and at `start`.
    #[serde(default = "default_appointment_reminders_enabled")]
    pub appointment_reminders_enabled: bool,
    #[serde(default = "default_appointment_reminder_advance_minutes")]
    pub appointment_reminder_advance_minutes: u32,
    /// If the process is down past `start`, still post “starting now” within this window; otherwise mark done quietly.
    #[serde(default = "default_appointment_start_grace_minutes")]
    pub appointment_start_grace_minutes: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct StorageSection {
    pub db_path: String,
    pub vault_path: String,
    /// Delete `message_ingest` rows older than this many days (`None` / omit = no age pruning).
    #[serde(default)]
    pub message_ingest_retention_days: Option<u32>,
    /// After age pruning, keep at most this many rows, dropping lowest ids first (`None` = no cap).
    #[serde(default)]
    pub message_ingest_keep_last: Option<u64>,
    /// Ingest rows still `Pending` after this many minutes are marked failed and their delivery
    /// meta claims released (scheduled with prune). `0` disables.
    #[serde(default = "default_message_ingest_stale_pending_minutes")]
    pub message_ingest_stale_pending_minutes: u32,
}

fn default_message_ingest_stale_pending_minutes() -> u32 {
    30
}

fn default_backup_before_first_write() -> bool {
    true
}

/// Vault write-back: the db → Markdown direction of sync. See `docs/two-way-vault-sync.md`.
///
/// Off by default, and deliberately so — every phase of the work ships dark, is verified from
/// the logs against the real vault, and is only then enabled. Nothing here affects the
/// long-standing Markdown → db direction, which always runs.
#[derive(Debug, Clone, Deserialize)]
pub struct VaultSection {
    /// Allow Mervyn to modify files under `storage.vault_path`. `false` = read-only, as today.
    #[serde(default)]
    pub write_back_enabled: bool,
    /// Copy the managed files to a timestamped sibling directory before the first write of a run.
    #[serde(default = "default_backup_before_first_write")]
    pub backup_before_first_write: bool,
}

impl Default for VaultSection {
    fn default() -> Self {
        Self {
            write_back_enabled: false,
            backup_before_first_write: default_backup_before_first_write(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServerSection {
    pub port: u16,
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

fn default_vault_git_sync_cron() -> String {
    "0 */5 * * * *".to_string()
}

fn default_git_author_name() -> String {
    "Mervyn".to_string()
}

fn default_git_author_email() -> String {
    "mervyn@localhost".to_string()
}

/// Carry the vault to and from a private git remote, so a laptop and a phone can hold it too.
///
/// This replaces the old `[worklog_git]` section. That one only pulled, into a separate clone the
/// vault symlinked into; the worklog now lives in the vault repo like everything else, and the
/// sync runs in both directions. Off by default.
#[derive(Debug, Clone, Deserialize)]
pub struct VaultGitSection {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_git_remote")]
    pub remote: String,
    #[serde(default = "default_git_branch")]
    pub branch: String,
    #[serde(default = "default_vault_git_sync_cron")]
    pub sync_cron: String,
    /// Identity on Mervyn's own commits, so they are told apart from yours at a glance.
    #[serde(default = "default_git_author_name")]
    pub author_name: String,
    #[serde(default = "default_git_author_email")]
    pub author_email: String,
}

impl Default for VaultGitSection {
    fn default() -> Self {
        Self {
            enabled: false,
            remote: default_git_remote(),
            branch: default_git_branch(),
            sync_cron: default_vault_git_sync_cron(),
            author_name: default_git_author_name(),
            author_email: default_git_author_email(),
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
