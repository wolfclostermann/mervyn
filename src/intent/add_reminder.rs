use chrono::Utc;
use chrono_tz::Tz;

use crate::intent::text_datetime::due_datetime_from_reminder_text;
use crate::state::AppState;
use crate::storage::reminders::{self, Reminder};

pub async fn run(state: &AppState, text: &str) -> anyhow::Result<String> {
    let id = reminders::next_id(state.db.as_ref()).map_err(|e| anyhow::anyhow!(e))?;
    let tz: Tz = state
        .settings
        .scheduler
        .timezone
        .parse()
        .unwrap_or_else(|_| {
            tracing::warn!(
                tz = %state.settings.scheduler.timezone,
                "invalid scheduler.timezone; using UTC for reminder due parsing"
            );
            chrono_tz::UTC
        });
    let due = due_datetime_from_reminder_text(text, Utc::now(), tz);
    let r = Reminder {
        id,
        body: text.trim().to_string(),
        due,
        recurrence: None,
        done: false,
    };
    reminders::put(state.db.as_ref(), &r).map_err(|e| anyhow::anyhow!(e))?;
    Ok(format!(
        "Reminder logged for {} (UTC): {}",
        due.format("%Y-%m-%d %H:%M"),
        r.body
    ))
}
