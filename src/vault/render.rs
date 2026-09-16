//! Canonical Markdown for a database row — the exact text Mervyn writes when it materialises a
//! row that has no line in the vault yet.
//!
//! Every renderer is the inverse of the corresponding parser in [`super::md`], and the round-trip
//! is the property the tests actually assert: `parse(render(row)) == row`. Without that, a row
//! written to the vault and read back on the next tick would drift a field at a time.
//!
//! Times are local wall-clock in `scheduler.timezone`, written without an offset. A bare date
//! means local noon, so noon is rendered *as* a bare date and still round-trips exactly.
//!
//! The reconciler that calls these arrives in the next commit; until then the round-trip tests
//! are the only consumer.
#![allow(dead_code)]

use chrono::{DateTime, Timelike, Utc};
use chrono_tz::Tz;

use super::marker;
use crate::storage::{Event, Recurrence, Reminder, TodoItem};

fn checkbox(done: bool) -> &'static str {
    if done {
        "- [x] "
    } else {
        "- [ ] "
    }
}

fn recurrence_word(r: &Recurrence) -> String {
    match r {
        Recurrence::Daily => "daily".to_string(),
        Recurrence::Weekly => "weekly".to_string(),
        Recurrence::Monthly => "monthly".to_string(),
        Recurrence::Custom(s) => s.clone(),
    }
}

/// `2026-11-15`, or `2026-11-15 09:00` when the local time is not noon.
fn date_and_time(at: DateTime<Utc>, tz: Tz) -> String {
    let local = at.with_timezone(&tz);
    if local.hour() == 12 && local.minute() == 0 {
        local.format("%Y-%m-%d").to_string()
    } else {
        local.format("%Y-%m-%d %H:%M").to_string()
    }
}

/// `- [ ] Body — due 2026-11-15 09:00 — recurs weekly <!--mv:1a-->`
pub fn reminder_line(r: &Reminder, tz: Tz) -> String {
    let mut s = String::from(checkbox(r.done));
    s.push_str(r.body.trim());
    s.push_str(" — due ");
    s.push_str(&date_and_time(r.due, tz));
    if let Some(rec) = &r.recurrence {
        s.push_str(" — recurs ");
        s.push_str(&recurrence_word(rec));
    }
    s.push(' ');
    s.push_str(&marker::render(r.id));
    s
}

/// A whole `## …` section, newline-terminated, with the marker on its own line beneath the
/// heading and an optional `Tags:` line at the end.
pub fn event_section(e: &Event, tz: Tz) -> String {
    let mut s = String::from("## ");
    s.push_str(&date_and_time(e.start, tz));
    if let Some(end) = e.end {
        // En dash, no spaces — that is what keeps it distinct from the ` — ` before the title.
        s.push('\u{2013}');
        s.push_str(&end.with_timezone(&tz).format("%H:%M").to_string());
    }
    s.push_str(" — ");
    s.push_str(e.title.trim());
    s.push('\n');
    s.push_str(&marker::render(e.id));
    s.push('\n');
    if let Some(d) = e.description.as_deref().map(str::trim).filter(|d| !d.is_empty()) {
        s.push_str(d);
        s.push('\n');
    }
    if !e.tags.is_empty() {
        // Blank line first: without it CommonMark folds `Tags:` into the description paragraph
        // as a soft break, and the parser reads the pair as one long description.
        if e.description.is_some() {
            s.push('\n');
        }
        s.push_str("Tags: ");
        s.push_str(&e.tags.join(", "));
        s.push('\n');
    }
    s
}

/// `- [ ] Call the plumber <!--mv:1a-->`
pub fn todo_line(t: &TodoItem) -> String {
    let mut s = String::from(checkbox(t.done));
    s.push_str(t.body.trim());
    s.push(' ');
    s.push_str(&marker::render(t.id));
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vault::md;

    fn london() -> Tz {
        "Europe/London".parse().unwrap()
    }

    fn utc(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn reminder(due: &str, recurrence: Option<Recurrence>, done: bool) -> Reminder {
        Reminder {
            id: 0x1a2b,
            body: "Pay the tax bill".into(),
            due: utc(due),
            recurrence,
            done,
        }
    }

    fn round_trip_reminder(r: &Reminder) {
        let line = format!("{}\n", reminder_line(r, london()));
        let parsed = md::parse_reminders(&line, london());
        assert_eq!(parsed.len(), 1, "did not parse back: {line:?}");
        assert_eq!(&parsed[0], r, "round trip changed the row via {line:?}");
    }

    #[test]
    fn a_reminder_survives_the_round_trip_in_both_offsets() {
        round_trip_reminder(&reminder("2026-11-15T09:00:00Z", None, false)); // GMT
        round_trip_reminder(&reminder("2026-06-15T08:00:00Z", None, false)); // BST, 09:00 local
    }

    #[test]
    fn a_reminder_survives_done_and_every_recurrence() {
        round_trip_reminder(&reminder("2026-11-15T09:00:00Z", None, true));
        for rec in [
            Recurrence::Daily,
            Recurrence::Weekly,
            Recurrence::Monthly,
            Recurrence::Custom("yearly".into()),
        ] {
            round_trip_reminder(&reminder("2026-11-15T09:00:00Z", Some(rec), false));
        }
    }

    #[test]
    fn local_noon_renders_as_a_bare_date_and_still_round_trips() {
        let r = reminder("2026-11-15T12:00:00Z", None, false);
        let line = reminder_line(&r, london());
        assert!(line.contains("— due 2026-11-15 —") || line.contains("— due 2026-11-15 <"), "{line}");
        round_trip_reminder(&r);
    }

    #[test]
    fn a_body_containing_the_field_separator_still_round_trips() {
        let mut r = reminder("2026-11-15T09:00:00Z", None, false);
        r.body = "Ask them — due diligence — about the survey".into();
        round_trip_reminder(&r);
    }

    #[test]
    fn an_event_survives_the_round_trip_with_description_tags_and_an_end() {
        let e = Event {
            id: 0xbeef,
            title: "Hospital appointment".into(),
            description: Some("Queen Alexandra, Cosham. Ref AB12345.".into()),
            start: utc("2026-09-23T13:30:00Z"), // 14:30 BST
            end: Some(utc("2026-09-23T15:00:00Z")),
            tags: vec!["health".into()],
        };
        let section = event_section(&e, london());
        assert!(section.starts_with("## 2026-09-23 14:30–16:00 — Hospital appointment\n"), "{section}");
        assert!(section.ends_with("\n\nTags: health\n"), "tags need their own paragraph: {section:?}");

        let parsed = md::parse_events(&section, london());
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0], e);
    }

    #[test]
    fn an_all_day_event_renders_without_a_time() {
        let e = Event {
            id: 7,
            title: "Gig at the Tap".into(),
            description: None,
            start: utc("2026-11-15T12:00:00Z"),
            end: None,
            tags: vec![],
        };
        assert_eq!(
            event_section(&e, london()),
            "## 2026-11-15 — Gig at the Tap\n<!--mv:7-->\n"
        );
        assert_eq!(md::parse_events(&event_section(&e, london()), london())[0], e);
    }

    #[test]
    fn rendering_is_stable_so_a_second_pass_writes_the_same_bytes() {
        let e = Event {
            id: 9,
            title: "Standup".into(),
            description: Some("Daily".into()),
            start: utc("2026-06-15T08:00:00Z"),
            end: None,
            tags: vec!["work".into()],
        };
        let once = event_section(&e, london());
        let reparsed = &md::parse_events(&once, london())[0];
        assert_eq!(event_section(reparsed, london()), once);
    }

    #[test]
    fn a_todo_line_carries_its_state_and_id() {
        let t = TodoItem {
            id: 0x2f,
            body: "Call the plumber".into(),
            created_at: utc("2026-09-16T10:00:00Z"),
            done: false,
        };
        assert_eq!(todo_line(&t), "- [ ] Call the plumber <!--mv:2f-->");
        let done = TodoItem { done: true, ..t };
        assert_eq!(todo_line(&done), "- [x] Call the plumber <!--mv:2f-->");
    }
}
