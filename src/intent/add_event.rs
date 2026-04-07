use chrono::Utc;

use crate::intent::event_title::resolve_event_title;
use crate::intent::text_datetime::event_timing_from_text;
use crate::state::AppState;
use crate::storage::events::{self, Event};

pub async fn run(
    state: &AppState,
    text: &str,
    situation: Option<String>,
) -> anyhow::Result<String> {
    let id = events::next_id(state.db.as_ref()).map_err(|e| anyhow::anyhow!(e))?;
    let now = Utc::now();
    let (start, end) = event_timing_from_text(text.trim(), now);
    let title = resolve_event_title(
        &state.claude,
        text.trim(),
        now.date_naive(),
        situation,
    )
    .await;
    let e = Event {
        id,
        title,
        description: None,
        start,
        end,
        tags: vec![],
    };
    events::put(state.db.as_ref(), &e).map_err(|e| anyhow::anyhow!(e))?;
    let time_line = match e.end {
        Some(end) => format!(
            "{}–{} (UTC)",
            start.format("%Y-%m-%d %H:%M"),
            end.format("%H:%M")
        ),
        None => format!("{} (UTC)", start.format("%Y-%m-%d %H:%M")),
    };
    Ok(format!(
        "Event “{}” saved for {} — edit time/details in Obsidian or ask me to refine.",
        e.title, time_line
    ))
}
