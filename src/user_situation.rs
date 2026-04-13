//! Wolf-maintained Markdown under the vault (`[user_context]`) → Claude system JSON + ask context.

use std::fs;
use std::path::Path;

use crate::config::AppConfig;

/// Load and cap text for prompts. `None` if disabled, missing file, or empty after trim.
pub fn load_for_prompts(settings: &AppConfig) -> Option<String> {
    if !settings.user_context.enabled {
        return None;
    }
    let name = settings.user_context.situation_file.trim();
    if name.is_empty() {
        return None;
    }
    let path = Path::new(&settings.storage.vault_path).join(name);
    let raw = fs::read_to_string(&path).ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let max = settings.user_context.situation_max_chars.max(500);
    let s: String = trimmed.chars().take(max).collect();
    if trimmed.chars().count() > max {
        Some(format!("{s}\n\n… (truncated to {max} characters; shorten {name} or raise user_context.situation_max_chars)"))
    } else {
        Some(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        AppConfig, ClaudeSection, NgrokSection, SchedulerSection, ServerSection, StorageSection,
        UserContextSection, WorklogGitSection,
    };

    fn cfg_with_vault(dir: &std::path::Path, file_body: &str) -> AppConfig {
        let v = dir.to_path_buf();
        std::fs::write(v.join("mervyn-situation.md"), file_body).unwrap();
        AppConfig {
            claude: ClaudeSection {
                model: "m".into(),
                max_tokens: 1,
            },
            scheduler: SchedulerSection {
                morning_briefing_cron: "0 0 7 * * *".into(),
                reminder_check_cron: "0 * * * * *".into(),
                vault_sync_cron: "0 */5 * * * *".into(),
                slack_ingest_prune_cron: "0 0 4 * * *".into(),
                timezone: "Europe/London".into(),
                appointment_reminders_enabled: true,
                appointment_reminder_advance_minutes: 30,
                appointment_start_grace_minutes: 30,
            },
            storage: StorageSection {
                db_path: "/tmp/x.redb".into(),
                vault_path: v.to_string_lossy().into_owned(),
                slack_ingest_retention_days: None,
                slack_ingest_keep_last: None,
                slack_ingest_stale_pending_minutes: 0,
            },
            server: ServerSection { port: 3000 },
            ngrok: NgrokSection::default(),
            user_context: UserContextSection {
                enabled: true,
                situation_file: "mervyn-situation.md".into(),
                situation_max_chars: 5000,
            },
            worklog_git: WorklogGitSection::default(),
        }
    }

    #[test]
    fn loads_trimmed_markdown() {
        let dir = tempfile::tempdir().unwrap();
        let c = cfg_with_vault(dir.path(), "\n# Projects\n- Mervyn\n");
        let s = load_for_prompts(&c).unwrap();
        assert!(s.contains("Mervyn"));
        assert!(s.contains("Projects"));
    }

    #[test]
    fn disabled_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let mut c = cfg_with_vault(dir.path(), "x");
        c.user_context.enabled = false;
        assert!(load_for_prompts(&c).is_none());
    }
}
