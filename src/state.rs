use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context;
use redb::Database;

use crate::claude::client::ClaudeClient;
use crate::config::AppConfig;
use crate::telegram::client::TelegramClient;
use crate::vault::sync::{SyncContext, WriteBackPolicy};
use crate::vault::write::VaultAccess;

#[derive(Clone)]
pub struct Secrets {
    pub anthropic_api_key: String,
    pub telegram_bot_token: String,
    /// The only chat Mervyn answers. Everything else is dropped before it reaches Claude.
    pub telegram_chat_id: i64,
    /// When set, enables `GET /admin/message-ingest` with `Authorization: Bearer <token>`.
    pub admin_token: Option<String>,
}

/// Redacted on purpose: a derived `Debug` would print every credential the moment anyone
/// added a `{:?}` to a log line, and the Telegram token in particular also travels in a URL.
impl std::fmt::Debug for Secrets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Secrets")
            .field("anthropic_api_key", &"<redacted>")
            .field("telegram_bot_token", &"<redacted>")
            .field("telegram_chat_id", &self.telegram_chat_id)
            .field("admin_token", &self.admin_token.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

/// Read a required variable, treating present-but-empty as missing.
///
/// `std::env::var` returns `Ok("")` for a var that is set to nothing, so a half-filled `.env`
/// would otherwise start and only fail later inside the poll loop.
fn require_env(name: &'static str) -> anyhow::Result<String> {
    let v = std::env::var(name).context(name)?;
    anyhow::ensure!(!v.trim().is_empty(), "{name} is set but empty");
    Ok(v)
}

impl Secrets {
    pub fn from_env() -> anyhow::Result<Self> {
        let chat_id_raw = require_env("TELEGRAM_CHAT_ID")?;
        let telegram_chat_id: i64 = chat_id_raw
            .trim()
            .parse()
            .with_context(|| format!("TELEGRAM_CHAT_ID must be a number, got {chat_id_raw:?}"))?;
        // A zero/unset id would otherwise pair with an update that has no chat and look like a match.
        anyhow::ensure!(telegram_chat_id != 0, "TELEGRAM_CHAT_ID must not be 0");

        Ok(Self {
            anthropic_api_key: require_env("ANTHROPIC_API_KEY")?,
            telegram_bot_token: require_env("TELEGRAM_BOT_TOKEN")?,
            telegram_chat_id,
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
    pub telegram: Arc<TelegramClient>,
    pub vault_path: PathBuf,
    /// Serialises vault reconcile cycles and suppresses the watcher's echo of Mervyn's own
    /// writes. Shared, not cloned: every task must contend for the same lock.
    pub vault: Arc<VaultAccess>,
}

impl AppState {
    /// Whether this process may write to the vault, from `[vault]` in the config.
    pub fn write_back_policy(&self) -> WriteBackPolicy {
        WriteBackPolicy {
            enabled: self.settings.vault.write_back_enabled,
            backup_before_first_write: self.settings.vault.backup_before_first_write,
        }
    }

    /// The zone the vault's wall-clock times are written and read in. Falls back to UTC with a
    /// warning rather than failing a sync — the same choice the intent handlers make.
    pub fn vault_tz(&self) -> chrono_tz::Tz {
        self.settings
            .scheduler
            .timezone
            .parse()
            .unwrap_or_else(|_| {
                tracing::warn!(
                    tz = %self.settings.scheduler.timezone,
                    "invalid scheduler.timezone; using UTC for vault times"
                );
                chrono_tz::UTC
            })
    }

    pub fn vault_sync_context(&self) -> SyncContext<'_> {
        SyncContext {
            access: self.vault.as_ref(),
            policy: self.write_back_policy(),
            tz: self.vault_tz(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_does_not_leak_secrets() {
        let s = Secrets {
            anthropic_api_key: "sk-ant-supersecret".into(),
            telegram_bot_token: "123456:AAHsupersecret".into(),
            telegram_chat_id: 42,
            admin_token: Some("adm-supersecret".into()),
        };
        let rendered = format!("{s:?}");
        assert!(!rendered.contains("supersecret"), "leaked: {rendered}");
        assert!(rendered.contains("42"), "chat id should stay visible: {rendered}");
    }
}
