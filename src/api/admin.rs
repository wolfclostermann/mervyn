//! Read-only operator endpoints (mounted only when `MERVYN_ADMIN_TOKEN` is set).

use axum::extract::{Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use constant_time_eq::constant_time_eq;
use serde::Deserialize;
use serde::Serialize;

use crate::state::AppState;
use crate::storage::message_ingest::{self, IngestListFilters, MessageIngestOutcome};
use crate::storage::{events, reminders, todos, vault_state, Event, Reminder, TodoItem};
use crate::vault::reconcile::VaultRow;

const BEARER_PREFIX: &str = "Bearer ";

#[derive(Debug, Deserialize)]
pub struct MessageIngestQuery {
    /// Max rows (clamped 1–500 server-side).
    #[serde(default = "default_limit")]
    pub limit: usize,
    pub since_ms: Option<i64>,
    pub until_ms: Option<i64>,
    /// Outcome discriminant: `Pending`, `Processed`, `Failed`, `DuplicateDelivery`, etc.
    pub outcome: Option<String>,
    /// Exact chat top-level `event_id`.
    pub event_id: Option<String>,
}

fn default_limit() -> usize {
    100
}

#[derive(Debug, Serialize)]
struct MessageIngestRowJson {
    id: u64,
    event_id: String,
    received_at_ms: i64,
    retry_num: Option<u32>,
    inner_type: String,
    outcome: String,
}

fn outcome_label(o: &MessageIngestOutcome) -> String {
    match o {
        MessageIngestOutcome::Pending => "Pending".into(),
        MessageIngestOutcome::DuplicateDelivery => "DuplicateDelivery".into(),
        MessageIngestOutcome::FilteredBot => "FilteredBot".into(),
        MessageIngestOutcome::FilteredNonText => "FilteredNonText".into(),
        MessageIngestOutcome::FilteredEmptyText => "FilteredEmptyText".into(),
        MessageIngestOutcome::Processed => "Processed".into(),
        MessageIngestOutcome::Failed(s) => format!("Failed: {s}"),
    }
}

fn bearer_token_ok(expected: &str, headers: &HeaderMap) -> bool {
    let Some(raw) = headers.get(header::AUTHORIZATION) else {
        return false;
    };
    let Ok(auth) = raw.to_str() else {
        return false;
    };
    let Some(provided) = auth.strip_prefix(BEARER_PREFIX) else {
        return false;
    };
    let a = provided.as_bytes();
    let b = expected.as_bytes();
    a.len() == b.len() && constant_time_eq(a, b)
}

pub async fn list_message_ingest(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<MessageIngestQuery>,
) -> impl IntoResponse {
    let Some(token) = state.secrets.admin_token.as_deref() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if !bearer_token_ok(token, &headers) {
        tracing::debug!("admin message-ingest: unauthorized");
        return StatusCode::UNAUTHORIZED.into_response();
    }

    let filters = IngestListFilters {
        since_ms: q.since_ms,
        until_ms: q.until_ms,
        outcome: q.outcome.as_deref(),
        event_id: q.event_id.as_deref(),
    };

    let rows = match message_ingest::list_recent(state.db.as_ref(), filters, q.limit) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, "admin message-ingest list");
            return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
        }
    };

    tracing::debug!(count = rows.len(), "admin message-ingest list");

    let body: Vec<MessageIngestRowJson> = rows
        .into_iter()
        .map(|(id, e)| MessageIngestRowJson {
            id,
            event_id: e.event_id,
            received_at_ms: e.received_at_ms,
            retry_num: e.retry_num,
            inner_type: e.inner_type,
            outcome: outcome_label(&e.outcome),
        })
        .collect();

    Json(serde_json::json!({ "rows": body })).into_response()
}
// ---------------------------------------------------------------------------------------------
// Stored data
//
// Added because diagnosing "is it actually in the database?" otherwise meant copying `mervyn.redb`
// off the server — redb takes an exclusive lock, so a second process cannot read it while Mervyn
// is running, and the only other answer came through Claude, which is the thing you are trying to
// check. These endpoints read the tables directly and say nothing more than what is in them.
// ---------------------------------------------------------------------------------------------

/// Row keys are `u64` and vault-derived ones use the full range, which JSON numbers cannot carry
/// exactly. They travel as the same lower-case hex the vault markers use, so a value here can be
/// grepped straight out of a Markdown file.
fn id_hex(id: u64) -> String {
    format!("{id:x}")
}

fn authorize(state: &AppState, headers: &HeaderMap, route: &str) -> Result<(), Response> {
    let Some(token) = state.secrets.admin_token.as_deref() else {
        return Err(StatusCode::NOT_FOUND.into_response());
    };
    if !bearer_token_ok(token, headers) {
        tracing::debug!(route, "admin: unauthorized");
        return Err(StatusCode::UNAUTHORIZED.into_response());
    }
    Ok(())
}

fn storage_error(route: &str, e: impl std::fmt::Display) -> Response {
    tracing::warn!(route, error = %e, "admin query failed");
    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response()
}

#[derive(Debug, Deserialize)]
pub struct ListQuery {
    /// Include rows already done. Default false — the open ones are the usual question.
    #[serde(default)]
    pub include_done: bool,
}

#[derive(Debug, Serialize)]
struct EventJson {
    id: String,
    title: String,
    start: String,
    end: Option<String>,
    description: Option<String>,
    tags: Vec<String>,
}

pub async fn list_events(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(r) = authorize(&state, &headers, "events") {
        return r;
    }
    let mut rows = match events::list_all(state.db.as_ref()) {
        Ok(r) => r,
        Err(e) => return storage_error("events", e),
    };
    rows.sort_by_key(|e| e.start);
    let body: Vec<EventJson> = rows
        .into_iter()
        .map(|e| EventJson {
            id: id_hex(e.id),
            title: e.title,
            start: e.start.to_rfc3339(),
            end: e.end.map(|x| x.to_rfc3339()),
            description: e.description,
            tags: e.tags,
        })
        .collect();
    Json(serde_json::json!({ "count": body.len(), "events": body })).into_response()
}

#[derive(Debug, Serialize)]
struct ReminderJson {
    id: String,
    body: String,
    due: String,
    recurrence: Option<String>,
    done: bool,
}

pub async fn list_reminders(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<ListQuery>,
) -> impl IntoResponse {
    if let Err(r) = authorize(&state, &headers, "reminders") {
        return r;
    }
    let mut rows = match reminders::list_all(state.db.as_ref()) {
        Ok(r) => r,
        Err(e) => return storage_error("reminders", e),
    };
    rows.retain(|r| q.include_done || !r.done);
    rows.sort_by_key(|r| r.due);
    let body: Vec<ReminderJson> = rows
        .into_iter()
        .map(|r| ReminderJson {
            id: id_hex(r.id),
            body: r.body,
            due: r.due.to_rfc3339(),
            recurrence: r.recurrence.map(|x| format!("{x:?}")),
            done: r.done,
        })
        .collect();
    Json(serde_json::json!({ "count": body.len(), "reminders": body })).into_response()
}

#[derive(Debug, Serialize)]
struct TodoJson {
    /// 1-based position, matching what chat shows and what `complete_todo` accepts.
    list_number: Option<usize>,
    id: String,
    body: String,
    created_at: String,
    done: bool,
}

pub async fn list_todos(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<ListQuery>,
) -> impl IntoResponse {
    if let Err(r) = authorize(&state, &headers, "todos") {
        return r;
    }
    let rows = match todos::list_all(state.db.as_ref()) {
        Ok(r) => r,
        Err(e) => return storage_error("todos", e),
    };
    // Number the open ones the way chat does, so "todo 2" can be matched to a row here.
    let mut open_seen = 0usize;
    let body: Vec<TodoJson> = rows
        .into_iter()
        .filter(|t| q.include_done || !t.done)
        .map(|t| {
            let list_number = if t.done {
                None
            } else {
                open_seen += 1;
                Some(open_seen)
            };
            TodoJson {
                list_number,
                id: id_hex(t.id),
                body: t.body,
                created_at: t.created_at.to_rfc3339(),
                done: t.done,
            }
        })
        .collect();
    Json(serde_json::json!({ "count": body.len(), "todos": body })).into_response()
}

// ---------------------------------------------------------------------------------------------
// Vault ↔ database agreement
// ---------------------------------------------------------------------------------------------

/// What the next sync would have to reconcile for one managed file.
///
/// Reported as facts rather than as a plan: which side of each row has moved since the snapshot
/// both last agreed on. That is the question worth asking when the vault and the database appear
/// to disagree, and it is not answerable from either one alone.
#[derive(Debug, Serialize)]
struct VaultFileReport {
    file: &'static str,
    file_exists: bool,
    rows_in_db: usize,
    items_in_file: usize,
    /// Lines with no id marker yet — the next write-back would give them one.
    unmarked_in_file: usize,
    /// In the database with no line in the file: appended if never synced, deleted if it had one.
    only_in_db: Vec<String>,
    /// Marked in the file but absent from the database.
    only_in_file: Vec<String>,
    /// The database has changed since the last agreed sync; the file has not.
    db_moved: Vec<String>,
    /// The file has changed since the last agreed sync.
    file_moved: Vec<String>,
    /// Tracked by neither side yet — no record of a previous sync.
    no_snapshot: Vec<String>,
    /// Lines still waiting to be removed. One that never clears means a write is failing.
    pending_tombstones: Vec<String>,
}

fn vault_report<T: VaultRow>(
    db: &redb::Database,
    vault_path: &std::path::Path,
    tz: chrono_tz::Tz,
    now: chrono::DateTime<chrono::Utc>,
    tombstones: &[(String, u64)],
) -> anyhow::Result<VaultFileReport> {
    let path = vault_path.join(T::FILE);
    let raw = if path.exists() {
        Some(std::fs::read_to_string(&path)?)
    } else {
        None
    };
    let found = raw.as_deref().map(|r| T::find(r, tz, now)).unwrap_or_default();
    let db_rows = T::load_all(db)?;

    let in_file: std::collections::HashMap<u64, &T> = found
        .iter()
        .filter(|f| f.had_marker)
        .map(|f| (f.item.id(), &f.item))
        .collect();

    let mut report = VaultFileReport {
        file: T::FILE,
        file_exists: raw.is_some(),
        rows_in_db: db_rows.len(),
        items_in_file: found.len(),
        unmarked_in_file: found.iter().filter(|f| !f.had_marker).count(),
        only_in_db: Vec::new(),
        only_in_file: Vec::new(),
        db_moved: Vec::new(),
        file_moved: Vec::new(),
        no_snapshot: Vec::new(),
        pending_tombstones: tombstones
            .iter()
            .filter(|(f, _)| f == T::FILE)
            .map(|(_, id)| id_hex(*id))
            .collect(),
    };

    for row in &db_rows {
        let id = row.id();
        let snapshot = vault_state::get(db, id, T::FILE)?.map(|s| s.rendered);
        match (in_file.get(&id), snapshot) {
            (None, _) => report.only_in_db.push(id_hex(id)),
            (Some(_), None) => report.no_snapshot.push(id_hex(id)),
            (Some(file_item), Some(snap)) => {
                if row.render_block(tz) != snap {
                    report.db_moved.push(id_hex(id));
                }
                if file_item.render_block(tz) != snap {
                    report.file_moved.push(id_hex(id));
                }
            }
        }
    }

    let db_ids: std::collections::HashSet<u64> = db_rows.iter().map(|r| r.id()).collect();
    for id in in_file.keys() {
        if !db_ids.contains(id) {
            report.only_in_file.push(id_hex(*id));
        }
    }

    Ok(report)
}

pub async fn vault_status(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(r) = authorize(&state, &headers, "vault") {
        return r;
    }

    let db = state.db.clone();
    let vault_path = state.vault_path.clone();
    let tz = state.vault_tz();
    let write_back = state.settings.vault.write_back_enabled;
    let git_enabled = state.settings.vault_git.enabled;
    let git_cfg = state.settings.vault_git.clone();

    // Reading three files and shelling out to git is blocking work; keep it off the runtime.
    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<serde_json::Value> {
        let now = chrono::Utc::now();
        let tombstones = vault_state::pending_tombstones(db.as_ref())
            .map_err(|e| anyhow::anyhow!(e))?;

        let files = vec![
            vault_report::<Reminder>(db.as_ref(), &vault_path, tz, now, &tombstones)?,
            vault_report::<Event>(db.as_ref(), &vault_path, tz, now, &tombstones)?,
            vault_report::<TodoItem>(db.as_ref(), &vault_path, tz, now, &tombstones)?,
        ];

        let is_repo = crate::vault::git::is_repo(&vault_path);
        let git = if is_repo && git_enabled {
            // Uncommitted changes mean the last cycle did not finish; the next one commits them.
            let dirty = crate::vault::git::is_dirty(&vault_path, &git_cfg).ok();
            serde_json::json!({
                "enabled": true,
                "is_repo": true,
                "uncommitted_changes": dirty,
                "remote": git_cfg.remote,
                "branch": git_cfg.branch,
            })
        } else {
            serde_json::json!({ "enabled": git_enabled, "is_repo": is_repo })
        };

        Ok(serde_json::json!({
            "vault_path": vault_path.display().to_string(),
            "write_back_enabled": write_back,
            "timezone": tz.to_string(),
            "git": git,
            "files": files,
        }))
    })
    .await;

    match result {
        Ok(Ok(body)) => Json(body).into_response(),
        Ok(Err(e)) => storage_error("vault", e),
        Err(e) => storage_error("vault", e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::db;
    use crate::vault::sync::{sync_vault_to_db, SyncContext, WriteBackPolicy};
    use crate::vault::write::VaultAccess;
    use chrono::Utc;
    use tempfile::tempdir;

    fn london() -> chrono_tz::Tz {
        "Europe/London".parse().unwrap()
    }

    /// A settled vault: one todo, synced, both sides agreeing.
    fn settled() -> (redb::Database, tempfile::TempDir, tempfile::TempDir) {
        let dir = tempdir().unwrap();
        let database = db::open(dir.path().join("t.redb").to_str().unwrap()).unwrap();
        let vault = tempdir().unwrap();
        std::fs::write(
            vault.path().join("todos.md"),
            "- [ ] Call the plumber <!--mv:7-->\n",
        )
        .unwrap();
        let access = VaultAccess::new();
        sync_vault_to_db(
            &database,
            vault.path(),
            SyncContext {
                access: &access,
                policy: WriteBackPolicy {
                    enabled: true,
                    backup_before_first_write: false,
                },
                tz: london(),
            },
        )
        .unwrap();
        (database, vault, dir)
    }

    fn report(database: &redb::Database, vault: &std::path::Path) -> VaultFileReport {
        let stones = vault_state::pending_tombstones(database).unwrap();
        vault_report::<TodoItem>(database, vault, london(), Utc::now(), &stones).unwrap()
    }

    #[test]
    fn a_settled_vault_reports_nothing_outstanding() {
        let (database, vault, _d) = settled();
        let r = report(&database, vault.path());

        assert_eq!((r.rows_in_db, r.items_in_file), (1, 1));
        assert_eq!(r.unmarked_in_file, 0);
        assert!(r.only_in_db.is_empty() && r.only_in_file.is_empty());
        assert!(r.db_moved.is_empty() && r.file_moved.is_empty());
        assert!(r.no_snapshot.is_empty() && r.pending_tombstones.is_empty());
    }

    #[test]
    fn it_says_which_side_moved() {
        let (database, vault, _d) = settled();

        // The database moves: chat completed it.
        todos::mark_done(&database, 7).unwrap();
        let r = report(&database, vault.path());
        assert_eq!(r.db_moved, vec!["7"], "the database changed");
        assert!(r.file_moved.is_empty(), "the file did not");

        // Now the file moves too, to something different again.
        std::fs::write(
            vault.path().join("todos.md"),
            "- [ ] Call the plumber about the leak <!--mv:7-->\n",
        )
        .unwrap();
        let r = report(&database, vault.path());
        assert_eq!(r.db_moved, vec!["7"]);
        assert_eq!(r.file_moved, vec!["7"], "both sides now differ from the snapshot");
    }

    #[test]
    fn a_row_with_no_line_and_a_line_with_no_row_are_told_apart() {
        let (database, vault, _d) = settled();

        todos::put(
            &database,
            &TodoItem {
                id: 9,
                body: "Added from chat".into(),
                created_at: Utc::now(),
                done: false,
            },
        )
        .unwrap();
        std::fs::write(
            vault.path().join("todos.md"),
            "- [ ] Call the plumber <!--mv:7-->\n- [ ] Typed by hand <!--mv:ff-->\n",
        )
        .unwrap();

        let r = report(&database, vault.path());
        assert_eq!(r.only_in_db, vec!["9"], "never written to the vault yet");
        assert_eq!(r.only_in_file, vec!["ff"], "marked, but no row");
    }

    #[test]
    fn an_unmarked_line_and_a_pending_tombstone_are_visible() {
        let (database, vault, _d) = settled();
        std::fs::write(
            vault.path().join("todos.md"),
            "- [ ] Call the plumber <!--mv:7-->\n- [ ] Just typed, no marker\n",
        )
        .unwrap();
        vault_state::tombstone(&database, 7, "todos.md").unwrap();

        let r = report(&database, vault.path());
        assert_eq!(r.unmarked_in_file, 1);
        assert_eq!(r.pending_tombstones, vec!["7"]);
    }

    #[test]
    fn keys_are_hex_so_they_match_the_markers_and_survive_json() {
        // 0xc75ba9194ac1a291 is past 2^53: as a JSON number it would come back changed.
        assert_eq!(id_hex(0xc75b_a919_4ac1_a291), "c75ba9194ac1a291");
        assert!(serde_json::to_string(&id_hex(u64::MAX))
            .unwrap()
            .contains("ffffffffffffffff"));
    }
}
