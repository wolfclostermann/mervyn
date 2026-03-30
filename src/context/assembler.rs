use std::path::PathBuf;
use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use redb::Database;

use crate::storage::{events, reminders, worklog};

/// Max characters from `vault/notes/*.md` injected into briefing context.
const NOTES_CHAR_BUDGET: usize = 24_000;

pub struct ContextAssembler {
    db: Arc<Database>,
    vault_path: PathBuf,
}

struct VaultMirror {
    events_md: String,
    reminders_md: String,
    worklog_md: String,
    notes_section: String,
}

struct BriefingData {
    events_text: String,
    reminders_text: String,
    worklog_text: String,
    vault: VaultMirror,
}

impl ContextAssembler {
    pub fn new(db: Arc<Database>, vault_path: PathBuf) -> Self {
        Self { db, vault_path }
    }

    async fn load_briefing_data(&self, now: DateTime<Utc>) -> anyhow::Result<BriefingData> {
        let until_events = now + Duration::days(7);
        let events = events::upcoming_within(self.db.as_ref(), now, until_events, 50)
            .map_err(|e| anyhow::anyhow!(e))?;
        let reminder_horizon = now + Duration::days(7);
        let pending = reminders::pending_due_within(self.db.as_ref(), reminder_horizon, 80)
            .map_err(|e| anyhow::anyhow!(e))?;
        let worklog_since = now - Duration::days(3);
        let work = worklog::recent_since(self.db.as_ref(), worklog_since, 40)
            .map_err(|e| anyhow::anyhow!(e))?;

        let vault = self.read_vault_mirror().await?;
        Ok(BriefingData {
            events_text: format_events(&events),
            reminders_text: format_reminders(&pending),
            worklog_text: format_worklog(&work),
            vault,
        })
    }

    /// Three blobs for [`crate::claude::prompts::morning_briefing_user_json`] (DB + vault per domain).
    pub async fn briefing_prompt_sections(
        &self,
        now: DateTime<Utc>,
    ) -> anyhow::Result<(String, String, String)> {
        let d = self.load_briefing_data(now).await?;
        let events_blob = format!(
            "### Database (upcoming)\n{}\n### Vault events.md\n{}",
            d.events_text, d.vault.events_md
        );
        let reminders_blob = format!(
            "### Database (pending / horizon)\n{}\n### Vault reminders.md\n{}",
            d.reminders_text, d.vault.reminders_md
        );
        let work_blob = format!(
            "### Database (recent)\n{}\n### Vault worklog.md\n{}\n### Vault notes\n{}",
            d.worklog_text, d.vault.worklog_md, d.vault.notes_section
        );
        Ok((events_blob, reminders_blob, work_blob))
    }

    /// Full context for the morning briefing: capped DB slices plus vault Markdown mirrors.
    #[allow(dead_code)] // Handy for debugging; cron uses [`Self::briefing_prompt_sections`].
    pub async fn build_briefing_context(&self, now: DateTime<Utc>) -> anyhow::Result<String> {
        let d = self.load_briefing_data(now).await?;
        let vault = format!(
            "## Vault events.md\n{}\n\n## Vault reminders.md\n{}\n\n## Vault worklog.md\n{}\n\n{}",
            d.vault.events_md, d.vault.reminders_md, d.vault.worklog_md, d.vault.notes_section
        );

        Ok(format!(
            "## Upcoming events (next 7 days, database)\n{}\n\n\
             ## Pending reminders (due within 7 days or overdue, database)\n{}\n\n\
             ## Recent worklog (last 3 days, database)\n{}\n\n\
             {vault}",
            d.events_text, d.reminders_text, d.worklog_text
        ))
    }

    /// Smaller context for freeform Q&A: reminders + recent worklog only (no vault notes).
    pub async fn build_query_context(&self, now: DateTime<Utc>) -> anyhow::Result<String> {
        let reminder_horizon = now + Duration::days(7);
        let pending = reminders::pending_due_within(self.db.as_ref(), reminder_horizon, 40)
            .map_err(|e| anyhow::anyhow!(e))?;
        let worklog_since = now - Duration::days(2);
        let work = worklog::recent_since(self.db.as_ref(), worklog_since, 25)
            .map_err(|e| anyhow::anyhow!(e))?;

        Ok(format!(
            "## Pending reminders\n{}\n\n## Recent worklog\n{}",
            format_reminders(&pending),
            format_worklog(&work)
        ))
    }

    async fn read_vault_mirror(&self) -> anyhow::Result<VaultMirror> {
        let events_md = read_file_or_empty(self.vault_path.join("events.md")).await?;
        let reminders_md = read_file_or_empty(self.vault_path.join("reminders.md")).await?;
        let worklog_md = read_file_or_empty(self.vault_path.join("worklog.md")).await?;

        let mut notes_section = String::from("## Vault notes (*.md under notes/)\n");
        let notes_dir = self.vault_path.join("notes");
        if tokio::fs::metadata(&notes_dir).await.is_ok() {
            let mut rd = tokio::fs::read_dir(&notes_dir).await?;
            let mut files: Vec<PathBuf> = Vec::new();
            while let Some(e) = rd.next_entry().await? {
                let p = e.path();
                if p.extension().is_some_and(|x| x == "md") {
                    files.push(p);
                }
            }
            files.sort();
            let mut consumed = 0usize;
            for p in files {
                if consumed >= NOTES_CHAR_BUDGET {
                    notes_section.push_str("\n… (remaining note files omitted)\n");
                    break;
                }
                let body = tokio::fs::read_to_string(&p).await?;
                let name = p
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let header = format!("\n### {name}\n");
                let header_chars = header.chars().count();
                let room = NOTES_CHAR_BUDGET.saturating_sub(consumed + header_chars);
                let slice: String = body.chars().take(room).collect();
                consumed += header_chars + slice.chars().count();
                notes_section.push_str(&header);
                notes_section.push_str(&slice);
                if slice.chars().count() < body.chars().count() {
                    notes_section.push_str("\n… (file truncated)\n");
                    break;
                }
            }
        }

        Ok(VaultMirror {
            events_md,
            reminders_md,
            worklog_md,
            notes_section,
        })
    }
}

async fn read_file_or_empty(path: PathBuf) -> anyhow::Result<String> {
    match tokio::fs::read_to_string(&path).await {
        Ok(s) => Ok(s),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(e.into()),
    }
}

fn format_events(events: &[crate::storage::Event]) -> String {
    if events.is_empty() {
        return "(none)\n".into();
    }
    let mut s = String::new();
    for e in events {
        s.push_str(&format!(
            "- {} — start {} (UTC){}\n",
            e.title,
            e.start.format("%Y-%m-%d %H:%M"),
            e.end
                .map(|x| format!(" — end {}", x.format("%Y-%m-%d %H:%M")))
                .unwrap_or_default()
        ));
        if let Some(desc) = &e.description {
            s.push_str(&format!("  {desc}\n"));
        }
        if !e.tags.is_empty() {
            s.push_str(&format!("  tags: {}\n", e.tags.join(", ")));
        }
    }
    s
}

fn format_reminders(list: &[crate::storage::Reminder]) -> String {
    if list.is_empty() {
        return "(none)\n".into();
    }
    let mut s = String::new();
    for r in list {
        s.push_str(&format!(
            "- {} — due {} (UTC){}\n",
            r.body,
            r.due.format("%Y-%m-%d %H:%M"),
            if r.done { " [done]" } else { "" }
        ));
    }
    s
}

fn format_worklog(entries: &[crate::storage::WorklogEntry]) -> String {
    if entries.is_empty() {
        return "(none)\n".into();
    }
    let mut s = String::new();
    for e in entries {
        s.push_str(&format!(
            "- {} — {}\n",
            e.timestamp.format("%Y-%m-%d %H:%M UTC"),
            e.body.replace('\n', " ")
        ));
        if !e.tags.is_empty() {
            s.push_str(&format!("  tags: {}\n", e.tags.join(", ")));
        }
        if let Some(p) = &e.project {
            s.push_str(&format!("  project: {p}\n"));
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::db;
    use crate::storage::events as ev;
    use tempfile::NamedTempFile;

    #[tokio::test]
    async fn briefing_includes_db_and_empty_vault_sections() {
        let tmp = NamedTempFile::new().unwrap();
        let db = Arc::new(db::open(tmp.path().to_str().unwrap()).unwrap());
        let vault = tempfile::tempdir().unwrap();

        let start = DateTime::parse_from_rfc3339("2026-04-05T19:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        ev::put(
            db.as_ref(),
            &crate::storage::Event {
                id: 1,
                title: "Gig".into(),
                description: None,
                start,
                end: None,
                tags: vec![],
            },
        )
        .unwrap();

        let asm = ContextAssembler::new(db, vault.path().to_path_buf());
        let now = DateTime::parse_from_rfc3339("2026-04-01T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let ctx = asm.build_briefing_context(now).await.unwrap();
        assert!(ctx.contains("Gig"));
        assert!(ctx.contains("Vault events.md"));
    }

    #[tokio::test]
    async fn query_context_skips_vault() {
        let tmp = NamedTempFile::new().unwrap();
        let db = Arc::new(db::open(tmp.path().to_str().unwrap()).unwrap());
        let vault = tempfile::tempdir().unwrap();
        std::fs::write(vault.path().join("events.md"), "SECRET").unwrap();

        let asm = ContextAssembler::new(db, vault.path().to_path_buf());
        let now = Utc::now();
        let ctx = asm.build_query_context(now).await.unwrap();
        assert!(!ctx.contains("SECRET"));
    }
}
