//! Parse Obsidian vault Markdown (tolerant heuristics) and upsert into redb.

use std::hash::Hasher;
use std::path::Path;

use chrono::{NaiveDate, Utc};
use fnv::FnvHasher;
use redb::Database;

use crate::storage::{events, reminders, worklog};
use crate::storage::{Event, Recurrence, Reminder, WorklogEntry};

/// FNV-1a 64-bit — stable across Rust versions for vault-derived primary keys.
fn stable_id(prefix: &[u8], key: &str) -> u64 {
    let mut h = FnvHasher::default();
    h.write(prefix);
    h.write(key.as_bytes());
    h.finish()
}

fn date_at_noon_utc(date: NaiveDate) -> chrono::DateTime<Utc> {
    date.and_hms_opt(12, 0, 0)
        .expect("valid noon")
        .and_utc()
}

#[derive(Debug, Default, Clone)]
pub struct SyncStats {
    pub reminders: usize,
    pub events: usize,
    pub worklog_entries: usize,
}

/// Read `reminders.md`, `events.md`, and `worklog.md` under `vault_path` and upsert rows.
pub fn sync_vault_to_db(db: &Database, vault_path: &Path) -> anyhow::Result<SyncStats> {
    let mut stats = SyncStats::default();

    let reminders_path = vault_path.join("reminders.md");
    if reminders_path.exists() {
        let text = std::fs::read_to_string(&reminders_path)?;
        for r in parse_reminders(&text) {
            reminders::put(db, &r).map_err(|e| anyhow::anyhow!(e))?;
            stats.reminders += 1;
        }
    }

    let events_path = vault_path.join("events.md");
    if events_path.exists() {
        let text = std::fs::read_to_string(&events_path)?;
        for e in parse_events(&text) {
            events::put(db, &e).map_err(|e| anyhow::anyhow!(e))?;
            stats.events += 1;
        }
    }

    let worklog_path = vault_path.join("worklog.md");
    if worklog_path.exists() {
        let text = std::fs::read_to_string(&worklog_path)?;
        for w in parse_worklog(&text) {
            worklog::put(db, &w).map_err(|e| anyhow::anyhow!(e))?;
            stats.worklog_entries += 1;
        }
    }

    Ok(stats)
}

fn parse_reminders(text: &str) -> Vec<Reminder> {
    let mut out = Vec::new();
    for raw in text.lines() {
        let line = raw.trim();
        let (done, rest) = if let Some(r) = line.strip_prefix("- [x]") {
            (true, r.trim_start())
        } else if let Some(r) = line.strip_prefix("- [X]") {
            (true, r.trim_start())
        } else if let Some(r) = line.strip_prefix("- [ ]") {
            (false, r.trim_start())
        } else {
            continue;
        };

        let (body, due, recurrence) = parse_reminder_body(rest);
        let due = match due {
            Some(d) => date_at_noon_utc(d),
            None => Utc::now(),
        };

        let norm = format!("{body}|{due}|{done}");
        let id = stable_id(b"rem:", &norm);
        out.push(Reminder {
            id,
            body,
            due,
            recurrence,
            done,
        });
    }
    out
}

fn parse_reminder_body(s: &str) -> (String, Option<NaiveDate>, Option<Recurrence>) {
    let mut recurrence = None;
    let mut due = None;
    let mut body_part = s.to_string();

    if let Some(idx) = body_part.find(" — recurs ") {
        let tail = body_part[idx + " — recurs ".len()..].trim();
        recurrence = Some(Recurrence::Custom(tail.to_string()));
        body_part.truncate(idx);
    }

    if let Some(idx) = body_part.find(" — due ") {
        let date_str = body_part[idx + " — due ".len()..].trim();
        if let Ok(d) = NaiveDate::parse_from_str(
            date_str.split_whitespace().next().unwrap_or(date_str),
            "%Y-%m-%d",
        ) {
            due = Some(d);
        }
        body_part.truncate(idx);
    }

    let body = body_part.trim().to_string();
    (body, due, recurrence)
}

fn parse_events(text: &str) -> Vec<Event> {
    let mut out = Vec::new();
    let lines: Vec<&str> = text.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i].trim();
        if let Some(rest) = line.strip_prefix("## ") {
            if let Some((date_str, title)) = rest.split_once(" — ") {
                if let Ok(date) = NaiveDate::parse_from_str(date_str.trim(), "%Y-%m-%d") {
                    let start = date_at_noon_utc(date);
                    i += 1;
                    let mut desc_lines = Vec::new();
                    let mut tags = Vec::new();
                    while i < lines.len() {
                        let l = lines[i];
                        if l.trim_start().starts_with("## ") {
                            break;
                        }
                        let t = l.trim();
                        if let Some(rest) = t.strip_prefix("Tags:").or_else(|| t.strip_prefix("tags:")) {
                            tags = rest
                                .split(',')
                                .map(|x| x.trim().to_string())
                                .filter(|x| !x.is_empty())
                                .collect();
                        } else if !t.is_empty() {
                            desc_lines.push(l.trim_end());
                        }
                        i += 1;
                    }
                    let description = if desc_lines.is_empty() {
                        None
                    } else {
                        Some(desc_lines.join("\n"))
                    };
                    let key = format!("{title}|{start}");
                    let id = stable_id(b"evt:", &key);
                    out.push(Event {
                        id,
                        title: title.trim().to_string(),
                        description,
                        start,
                        end: None,
                        tags,
                    });
                    continue;
                }
            }
        }
        i += 1;
    }
    out
}

fn parse_worklog(text: &str) -> Vec<WorklogEntry> {
    let mut out = Vec::new();
    let lines: Vec<&str> = text.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i].trim();
        if let Some(rest) = line.strip_prefix("## ") {
            if let Ok(day) = NaiveDate::parse_from_str(rest.trim(), "%Y-%m-%d") {
                let day_start = date_at_noon_utc(day);
                i += 1;
                let mut bullets: Vec<String> = Vec::new();
                let mut tags: Vec<String> = Vec::new();
                while i < lines.len() {
                    let l = lines[i];
                    if l.trim_start().starts_with("## ") {
                        break;
                    }
                    let t = l.trim();
                    if let Some(rest) = t.strip_prefix("Tags:").or_else(|| t.strip_prefix("tags:")) {
                        tags = rest
                            .split(',')
                            .map(|x| x.trim().to_string())
                            .filter(|x| !x.is_empty())
                            .collect();
                    } else if let Some(body) = t.strip_prefix("- ") {
                        bullets.push(body.to_string());
                    }
                    i += 1;
                }
                for body in bullets {
                    let key = format!("{day}|{body}");
                    let id = stable_id(b"wlog:", &key);
                    out.push(WorklogEntry {
                        id,
                        timestamp: day_start,
                        body,
                        tags: tags.clone(),
                        project: None,
                    });
                }
                continue;
            }
        }
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::db;
    use tempfile::tempdir;

    #[test]
    fn parse_reminder_due_and_recurrence() {
        let md = "# Reminders\n\n- [ ] Pay tax — due 2026-04-10 — recurs yearly\n";
        let list = parse_reminders(md);
        assert_eq!(list.len(), 1);
        assert!(!list[0].done);
        assert_eq!(list[0].body, "Pay tax");
        assert!(matches!(
            list[0].recurrence,
            Some(Recurrence::Custom(ref s)) if s == "yearly"
        ));
    }

    #[test]
    fn sync_roundtrip_to_db() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("t.redb");
        let db = db::open(db_path.to_str().unwrap()).unwrap();
        let vault = tempdir().unwrap();
        std::fs::write(
            vault.path().join("reminders.md"),
            "- [ ] Test — due 2026-05-01\n",
        )
        .unwrap();

        let s = sync_vault_to_db(&db, vault.path()).unwrap();
        assert_eq!(s.reminders, 1);
        let all = reminders::list_all(&db).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].body, "Test");
    }
}
