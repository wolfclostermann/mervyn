//! Read-only operator endpoints (mounted only when `MERVYN_ADMIN_TOKEN` is set).

use axum::extract::{Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use constant_time_eq::constant_time_eq;
use serde::Deserialize;
use serde::Serialize;

use crate::state::AppState;
use crate::storage::slack_ingest::{self, IngestListFilters, SlackIngestOutcome};

const BEARER_PREFIX: &str = "Bearer ";

#[derive(Debug, Deserialize)]
pub struct SlackIngestQuery {
    /// Max rows (clamped 1–500 server-side).
    #[serde(default = "default_limit")]
    pub limit: usize,
    pub since_ms: Option<i64>,
    pub until_ms: Option<i64>,
    /// Outcome discriminant: `Pending`, `Processed`, `Failed`, `DuplicateDelivery`, etc.
    pub outcome: Option<String>,
    /// Exact Slack top-level `event_id`.
    pub event_id: Option<String>,
}

fn default_limit() -> usize {
    100
}

#[derive(Debug, Serialize)]
struct SlackIngestRowJson {
    id: u64,
    event_id: String,
    received_at_ms: i64,
    retry_num: Option<u32>,
    inner_type: String,
    outcome: String,
}

fn outcome_label(o: &SlackIngestOutcome) -> String {
    match o {
        SlackIngestOutcome::Pending => "Pending".into(),
        SlackIngestOutcome::DuplicateDelivery => "DuplicateDelivery".into(),
        SlackIngestOutcome::FilteredBot => "FilteredBot".into(),
        SlackIngestOutcome::FilteredSubtype => "FilteredSubtype".into(),
        SlackIngestOutcome::FilteredUnsupportedType => "FilteredUnsupportedType".into(),
        SlackIngestOutcome::FilteredEmptyText => "FilteredEmptyText".into(),
        SlackIngestOutcome::Processed => "Processed".into(),
        SlackIngestOutcome::Failed(s) => format!("Failed: {s}"),
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

pub async fn list_slack_ingest(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<SlackIngestQuery>,
) -> impl IntoResponse {
    let Some(token) = state.secrets.admin_token.as_deref() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if !bearer_token_ok(token, &headers) {
        tracing::debug!("admin slack-ingest: unauthorized");
        return StatusCode::UNAUTHORIZED.into_response();
    }

    let filters = IngestListFilters {
        since_ms: q.since_ms,
        until_ms: q.until_ms,
        outcome: q.outcome.as_deref(),
        event_id: q.event_id.as_deref(),
    };

    let rows = match slack_ingest::list_recent(state.db.as_ref(), filters, q.limit) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, "admin slack-ingest list");
            return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
        }
    };

    tracing::debug!(count = rows.len(), "admin slack-ingest list");

    let body: Vec<SlackIngestRowJson> = rows
        .into_iter()
        .map(|(id, e)| SlackIngestRowJson {
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
