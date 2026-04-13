use std::io::ErrorKind;
use std::path::PathBuf;
use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use redb::Database;

use crate::intent::remember_briefing::WORKLOG_TAG;
use crate::storage::{events, reminders, todos, worklog};

/// Max characters from `vault/notes/*.md` injected into briefing context.
const NOTES_CHAR_BUDGET: usize = 24_000;

/// Events horizon for freeform Q&A (`ask`) — covers “next month” style questions.
const QUERY_EVENT_HORIZON_DAYS: i64 = 40;
/// Cap for `events.md` mirror in query context (keeps token use bounded).
const QUERY_EVENTS_MD_MAX_CHARS: usize = 12_000;
/// Cap for `worklog.md` mirror in query context (git-synced file may be ahead of redb briefly).
const QUERY_WORKLOG_MD_MAX_CHARS: usize = 16_000;

/// Slack-queued briefing lines stay visible to the morning job for this long.
const BRIEFING_QUEUE_MAX_AGE: Duration = Duration::hours(48);

pub struct ContextAssembler {
    db: Arc<Database>,
    vault_path: PathBuf,
    /// `worklog.md` inside `[worklog_git].repo_path` when set (see [`crate::config::AppConfig::worklog_md_git_mirror_path`]).
    worklog_md_fallback: Option<PathBuf>,
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
    pub fn new(
        db: Arc<Database>,
        vault_path: PathBuf,
        worklog_md_fallback: Option<PathBuf>,
    ) -> Self {
        Self {
            db,
            vault_path,
            worklog_md_fallback,
        }
    }

    /// Prefer vault `worklog.md`; if missing, unreadable, or whitespace-only, try `worklog_md_fallback`.
    async fn read_worklog_md_with_fallback(&self) -> anyhow::Result<String> {
        let primary_path = self.vault_path.join("worklog.md");
        let primary = match tokio::fs::read_to_string(&primary_path).await {
            Ok(s) => s,
            Err(e) if e.kind() == ErrorKind::NotFound => String::new(),
            Err(e) => {
                tracing::warn!(
                    path = %primary_path.display(),
                    error = %e,
                    "vault worklog.md read failed; will try worklog_git mirror if configured"
                );
                String::new()
            }
        };
        if !primary.trim().is_empty() {
            return Ok(primary);
        }
        if let Some(ref fb) = self.worklog_md_fallback {
            match tokio::fs::read_to_string(fb).await {
                Ok(s) if !s.trim().is_empty() => {
                    tracing::debug!(path = %fb.display(), "using worklog.md from worklog_git mirror path");
                    return Ok(s);
                }
                Ok(_) => {}
                Err(e) if e.kind() == ErrorKind::NotFound => {}
                Err(e) => tracing::warn!(path = %fb.display(), error = %e, "worklog git mirror read failed"),
            }
        }
        Ok(primary)
    }

    async fn load_briefing_data(
        &self,
        now: DateTime<Utc>,
        situation: Option<&str>,
    ) -> anyhow::Result<BriefingData> {
        let until_events = now + Duration::days(7);
        let events = events::upcoming_within(self.db.as_ref(), now, until_events, 50)
            .map_err(|e| anyhow::anyhow!(e))?;
        let reminder_horizon = now + Duration::days(7);
        let pending = reminders::pending_due_within(self.db.as_ref(), reminder_horizon, 80)
            .map_err(|e| anyhow::anyhow!(e))?;
        let work_horizon = now - Duration::days(7);
        let work_all = worklog::recent_since(self.db.as_ref(), work_horizon, 100)
            .map_err(|e| anyhow::anyhow!(e))?;
        let three_days = now - Duration::days(3);
        let brief_cutoff = now - BRIEFING_QUEUE_MAX_AGE;

        let mut briefing_queue = Vec::new();
        let mut other = Vec::new();
        for e in work_all {
            let tagged = e
                .tags
                .iter()
                .any(|t| t.eq_ignore_ascii_case(WORKLOG_TAG));
            if tagged && e.timestamp >= brief_cutoff {
                briefing_queue.push(e);
            } else if !tagged && e.timestamp >= three_days {
                other.push(e);
            }
        }

        let situation_block = situation
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| format!("### Wolf's standing context (vault situation file)\n{s}\n\n"))
            .unwrap_or_default();

        let open_todos = todos::list_open(self.db.as_ref(), 80).map_err(|e| anyhow::anyhow!(e))?;

        let worklog_text = {
            let mut parts = Vec::new();
            if !briefing_queue.is_empty() {
                parts.push(format!(
                    "### Queued for this morning briefing (from Slack, last 48h)\n{}",
                    format_worklog(&briefing_queue)
                ));
            }
            parts.push(format!(
                "### Open todos (database)\n{}",
                format_todos(&open_todos, false)
            ));
            parts.push(format!(
                "### Other recent worklog (database, last 3 days)\n{}",
                format_worklog(&other)
            ));
            let inner = parts.join("\n\n");
            if situation_block.is_empty() {
                inner
            } else {
                format!("{situation_block}{inner}")
            }
        };

        let vault = self.read_vault_mirror().await?;
        Ok(BriefingData {
            events_text: format_events(&events),
            reminders_text: format_reminders(&pending),
            worklog_text,
            vault,
        })
    }

    /// Three blobs for [`crate::claude::prompts::morning_briefing_user_json`] (DB + vault per domain).
    pub async fn briefing_prompt_sections(
        &self,
        now: DateTime<Utc>,
        situation: Option<&str>,
    ) -> anyhow::Result<(String, String, String)> {
        let d = self.load_briefing_data(now, situation).await?;
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
        let d = self.load_briefing_data(now, None).await?;
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

    /// Context for freeform Q&A: upcoming events (DB + `events.md`), reminders, recent worklog (no `notes/`).
    pub async fn build_query_context(
        &self,
        now: DateTime<Utc>,
        situation: Option<&str>,
    ) -> anyhow::Result<String> {
        let event_until = now + Duration::days(QUERY_EVENT_HORIZON_DAYS);
        let evs = events::upcoming_within(self.db.as_ref(), now, event_until, 120)
            .map_err(|e| anyhow::anyhow!(e))?;

        let events_md_raw = read_file_or_empty(self.vault_path.join("events.md")).await?;
        let md_len = events_md_raw.chars().count();
        let events_md: String = events_md_raw.chars().take(QUERY_EVENTS_MD_MAX_CHARS).collect();
        let events_md_block = if events_md.trim().is_empty() {
            "(empty or missing)\n".to_string()
        } else {
            let mut s = events_md;
            if md_len > QUERY_EVENTS_MD_MAX_CHARS {
                s.push_str("\n… (events.md truncated)\n");
            }
            s
        };

        let reminder_horizon = now + Duration::days(7);
        let pending = reminders::pending_due_within(self.db.as_ref(), reminder_horizon, 40)
            .map_err(|e| anyhow::anyhow!(e))?;
        let worklog_since = now - Duration::days(7);
        let work = worklog::recent_since(self.db.as_ref(), worklog_since, 50)
            .map_err(|e| anyhow::anyhow!(e))?;
        let open_todos = todos::list_open(self.db.as_ref(), 80).map_err(|e| anyhow::anyhow!(e))?;

        let worklog_md_raw = self.read_worklog_md_with_fallback().await?;
        let wl_len = worklog_md_raw.chars().count();
        let worklog_md: String = worklog_md_raw
            .chars()
            .take(QUERY_WORKLOG_MD_MAX_CHARS)
            .collect();
        let worklog_md_block = if worklog_md.trim().is_empty() {
            "(empty or missing)\n".to_string()
        } else {
            let mut s = worklog_md;
            if wl_len > QUERY_WORKLOG_MD_MAX_CHARS {
                s.push_str("\n… (worklog.md truncated)\n");
            }
            s
        };

        let situation_block = situation
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| {
                format!("## Wolf's standing context (vault situation file)\n{s}\n\n")
            })
            .unwrap_or_default();

        Ok(format!(
            "{situation_block}\
             ## Upcoming events (database, next {} days)\n{}\n\n\
             ## Vault events.md\n{}\n\n\
             ## Pending reminders\n{}\n\n\
             ## Open todos (database; numeric ids for reference)\n{}\n\n\
             ## Recent worklog (database, last 7 days)\n{}\n\n\
             ## Vault worklog.md (file on disk; may include git-backed lines not yet in DB)\n{}",
            QUERY_EVENT_HORIZON_DAYS,
            format_events(&evs),
            events_md_block,
            format_reminders(&pending),
            format_todos(&open_todos, true),
            format_worklog(&work),
            worklog_md_block
        ))
    }

    async fn read_vault_mirror(&self) -> anyhow::Result<VaultMirror> {
        let events_md = read_file_or_empty(self.vault_path.join("events.md")).await?;
        let reminders_md = read_file_or_empty(self.vault_path.join("reminders.md")).await?;
        let worklog_md = self.read_worklog_md_with_fallback().await?;

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

fn format_todos(items: &[crate::storage::TodoItem], include_ids: bool) -> String {
    if items.is_empty() {
        return "(none)\n".into();
    }
    let mut s = String::new();
    for t in items {
        if include_ids {
            s.push_str(&format!("- [{}] {}\n", t.id, t.body));
        } else {
            s.push_str(&format!("- {}\n", t.body));
        }
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

        let asm = ContextAssembler::new(db, vault.path().to_path_buf(), None);
        let now = DateTime::parse_from_rfc3339("2026-04-01T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let ctx = asm.build_briefing_context(now).await.unwrap();
        assert!(ctx.contains("Gig"));
        assert!(ctx.contains("Vault events.md"));
    }

    #[tokio::test]
    async fn query_context_skips_notes_tree() {
        let tmp = NamedTempFile::new().unwrap();
        let db = Arc::new(db::open(tmp.path().to_str().unwrap()).unwrap());
        let vault = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(vault.path().join("notes")).unwrap();
        std::fs::write(vault.path().join("notes/leak.md"), "SECRET").unwrap();

        let asm = ContextAssembler::new(db, vault.path().to_path_buf(), None);
        let now = Utc::now();
        let ctx = asm.build_query_context(now, None).await.unwrap();
        assert!(!ctx.contains("SECRET"));
    }

    #[tokio::test]
    async fn query_context_includes_db_events_within_horizon() {
        let tmp = NamedTempFile::new().unwrap();
        let db = Arc::new(db::open(tmp.path().to_str().unwrap()).unwrap());
        let vault = tempfile::tempdir().unwrap();
        let start = DateTime::parse_from_rfc3339("2026-04-10T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        ev::put(
            db.as_ref(),
            &crate::storage::Event {
                id: 1,
                title: "Team sync".into(),
                description: None,
                start,
                end: None,
                tags: vec![],
            },
        )
        .unwrap();

        let asm = ContextAssembler::new(db, vault.path().to_path_buf(), None);
        let now = DateTime::parse_from_rfc3339("2026-04-05T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let ctx = asm.build_query_context(now, None).await.unwrap();
        assert!(ctx.contains("Team sync"));
    }

    #[tokio::test]
    async fn query_context_includes_vault_worklog_md() {
        let tmp = NamedTempFile::new().unwrap();
        let db = Arc::new(db::open(tmp.path().to_str().unwrap()).unwrap());
        let vault = tempfile::tempdir().unwrap();
        std::fs::write(
            vault.path().join("worklog.md"),
            "## 2026-04-07\n- **mervyn** `abc1234` shipped feature\n",
        )
        .unwrap();

        let asm = ContextAssembler::new(db, vault.path().to_path_buf(), None);
        let now = DateTime::parse_from_rfc3339("2026-04-07T18:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let ctx = asm.build_query_context(now, None).await.unwrap();
        assert!(ctx.contains("Vault worklog.md"));
        assert!(ctx.contains("shipped feature"));
    }

    #[tokio::test]
    async fn query_context_reads_worklog_from_fallback_when_vault_missing() {
        let tmp = NamedTempFile::new().unwrap();
        let db = Arc::new(db::open(tmp.path().to_str().unwrap()).unwrap());
        let vault = tempfile::tempdir().unwrap();
        let mirror = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(
            mirror.path(),
            "## 2026-04-07\n- from git mirror only\n",
        )
        .unwrap();

        let asm = ContextAssembler::new(
            db,
            vault.path().to_path_buf(),
            Some(mirror.path().to_path_buf()),
        );
        let now = DateTime::parse_from_rfc3339("2026-04-07T18:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let ctx = asm.build_query_context(now, None).await.unwrap();
        assert!(ctx.contains("from git mirror only"));
    }
}
