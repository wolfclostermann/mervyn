use chrono::Utc;
use chrono_tz::Tz;

use crate::intent::add_event_claude_time;
use crate::intent::event_title::resolve_event_title;
use crate::intent::text_datetime::{add_event_time_from_text, format_event_range_for_reply, AddEventTimeFromText};
use crate::state::AppState;
use crate::storage::events::{self, Event};

pub async fn run(
    state: &AppState,
    text: &str,
    situation: Option<String>,
) -> anyhow::Result<String> {
    let id = events::next_id(state.db.as_ref()).map_err(|e| anyhow::anyhow!(e))?;
    let now = Utc::now();
    let tz: Tz = state
        .settings
        .scheduler
        .timezone
        .parse()
        .unwrap_or_else(|_| {
            tracing::warn!(
                tz = %state.settings.scheduler.timezone,
                "invalid scheduler.timezone; using UTC for event time parsing"
            );
            chrono_tz::UTC
        });
    let iana = &state.settings.scheduler.timezone;
    let trimmed = text.trim();
    let (start, end) = match add_event_time_from_text(trimmed, now, tz) {
        AddEventTimeFromText::Deterministic { start, end } => (start, end),
        AddEventTimeFromText::UseLanguageModel => {
            add_event_claude_time::resolve_add_event_time_via_claude(
                &state.claude,
                trimmed,
                situation.clone(),
                iana,
            )
            .await?
        }
    };
    let title = resolve_event_title(
        &state.claude,
        trimmed,
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
    let time_line = format_event_range_for_reply(e.start, e.end, tz);
    Ok(format!(
        "Event “{}” saved for {} — edit time/details in Obsidian or ask me to refine.",
        e.title, time_line
    ))
}
