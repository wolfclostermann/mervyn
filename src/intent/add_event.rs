use chrono::Utc;
use chrono_tz::Tz;

use crate::intent::add_event_claude_time;
use crate::intent::event_title::resolve_event_title;
use crate::intent::text_datetime::{add_event_time_from_text, format_event_range_for_reply, AddEventTimeFromText};
use crate::state::AppState;
use crate::storage::events::{self, Event};

/// Longest original message kept as the event description. It is inlined into the briefing
/// prompt and appended to appointment reminders, so it has to stay bounded.
const MAX_DESCRIPTION_CHARS: usize = 600;

/// Below this length the message is a terse command ("add dentist tuesday 2pm") and the title
/// already carries everything; keeping a description would just duplicate it. Above it the
/// message is usually a forwarded confirmation whose address, phone number and reference
/// number are worth preserving - the title cannot hold them.
const MIN_TEXT_FOR_DESCRIPTION: usize = 80;

/// Original message to keep alongside the parsed title and time, if it adds anything.
fn description_from_text(text: &str) -> Option<String> {
    let t = text.trim();
    if t.chars().count() <= MIN_TEXT_FOR_DESCRIPTION {
        return None;
    }
    let mut out: String = t.chars().take(MAX_DESCRIPTION_CHARS).collect();
    if t.chars().count() > MAX_DESCRIPTION_CHARS {
        out.push('\u{2026}');
    }
    Some(out)
}

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
        description: description_from_text(trimmed),
        start,
        end,
        tags: vec![],
    };
    events::put(state.db.as_ref(), &e).map_err(|e| anyhow::anyhow!(e))?;
    let time_line = format_event_range_for_reply(e.start, e.end, tz);
    Ok(format!(
        "Event “{}” saved for {}. If that is wrong, ask me to remove it and send the corrected details.",
        e.title, time_line
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terse_command_keeps_no_description() {
        // The title already says "dentist"; a description would just repeat it.
        assert_eq!(description_from_text("add dentist tuesday 2pm"), None);
    }

    #[test]
    fn forwarded_confirmation_is_preserved() {
        let sms = "Your appointment at Queen Alexandra Hospital, Cosham, Portsmouth PO6 3LY \
is confirmed for Tue 23 Sep at 08:30. Please bring your letter. Ref AB12345. \
To cancel call 023 9228 6000.";
        let d = description_from_text(sms).expect("long text should be kept");
        assert!(d.contains("PO6 3LY"), "address should survive");
        assert!(d.contains("AB12345"), "reference should survive");
    }

    #[test]
    fn overlong_text_is_capped_and_marked() {
        let long = "x".repeat(MAX_DESCRIPTION_CHARS + 50);
        let d = description_from_text(&long).unwrap();
        assert_eq!(d.chars().count(), MAX_DESCRIPTION_CHARS + 1); // + the ellipsis
        assert!(d.ends_with('\u{2026}'));
    }

    #[test]
    fn multibyte_text_is_not_split_mid_char() {
        let long = "\u{e9}".repeat(MAX_DESCRIPTION_CHARS + 50);
        let d = description_from_text(&long).unwrap();
        assert!(d.chars().all(|c| c == '\u{e9}' || c == '\u{2026}'));
    }

    #[test]
    fn whitespace_only_is_none() {
        assert_eq!(description_from_text("   \n  "), None);
    }
}
