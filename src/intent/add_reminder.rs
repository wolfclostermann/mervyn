use chrono::{Duration, NaiveDate, Utc};

use crate::state::AppState;
use crate::storage::reminders::{self, Reminder};

fn find_iso_date(text: &str) -> Option<NaiveDate> {
    for w in text.as_bytes().windows(10) {
        if w.len() == 10 && w[4] == b'-' && w[7] == b'-' {
            if let Ok(s) = std::str::from_utf8(w) {
                if let Ok(d) = NaiveDate::parse_from_str(s, "%Y-%m-%d") {
                    return Some(d);
                }
            }
        }
    }
    None
}

fn due_from_text_or_default(text: &str) -> chrono::DateTime<Utc> {
    if let Some(d) = find_iso_date(text) {
        if let Some(naive) = d.and_hms_opt(9, 0, 0) {
            return naive.and_utc();
        }
    }
    Utc::now() + Duration::days(1)
}

pub async fn run(state: &AppState, text: &str) -> anyhow::Result<String> {
    let id = reminders::next_id(state.db.as_ref()).map_err(|e| anyhow::anyhow!(e))?;
    let due = due_from_text_or_default(text);
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
