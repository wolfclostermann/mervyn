use chrono::{Duration, Utc};

use crate::state::AppState;
use crate::storage::events::{self, Event};

pub async fn run(state: &AppState, text: &str) -> anyhow::Result<String> {
    let id = events::next_id(state.db.as_ref()).map_err(|e| anyhow::anyhow!(e))?;
    let start = Utc::now() + Duration::hours(24);
    let e = Event {
        id,
        title: text.trim().to_string(),
        description: None,
        start,
        end: None,
        tags: vec![],
    };
    events::put(state.db.as_ref(), &e).map_err(|e| anyhow::anyhow!(e))?;
    Ok(format!(
        "Event “{}” saved for {} (UTC) — edit time/details in Obsidian or ask me to refine.",
        e.title,
        start.format("%Y-%m-%d %H:%M")
    ))
}
