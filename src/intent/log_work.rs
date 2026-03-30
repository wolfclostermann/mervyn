use chrono::Utc;

use crate::state::AppState;
use crate::storage::worklog::{self, WorklogEntry};

pub async fn run(state: &AppState, text: &str) -> anyhow::Result<String> {
    let id = worklog::next_id(state.db.as_ref()).map_err(|e| anyhow::anyhow!(e))?;
    let e = WorklogEntry {
        id,
        timestamp: Utc::now(),
        body: text.trim().to_string(),
        tags: vec![],
        project: None,
    };
    worklog::put(state.db.as_ref(), &e).map_err(|e| anyhow::anyhow!(e))?;
    Ok(format!("Logged: {}", e.body))
}
