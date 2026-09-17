//! Advance + start chat notifications for calendar events.

use chrono::{DateTime, Duration, Utc};

use crate::state::AppState;
use crate::storage::event_notices::{self, EventNoticeState};
use crate::storage::events;

/// Returns `(send_advance, send_start)` and updates `state` in place.
pub(crate) fn tick_notice_state(
    state: &mut EventNoticeState,
    event_start: DateTime<Utc>,
    now: DateTime<Utc>,
    advance: Duration,
    start_grace: Duration,
) -> (bool, bool) {
    let start_ts = event_start.timestamp();
    if state.start_unix != start_ts {
        *state = EventNoticeState {
            start_unix: start_ts,
            advance_sent: false,
            start_sent: false,
        };
    }

    let mut send_advance = false;
    let mut send_start = false;

    let advance_at = event_start - advance;

    if !state.advance_sent {
        if now >= event_start {
            state.advance_sent = true;
        } else if now >= advance_at {
            state.advance_sent = true;
            send_advance = true;
        }
    }

    if !state.start_sent && now >= event_start {
        state.start_sent = true;
        if now <= event_start + start_grace {
            send_start = true;
        }
    }

    (send_advance, send_start)
}

fn format_event_time(state: &AppState, event_start: DateTime<Utc>) -> anyhow::Result<String> {
    let tz: chrono_tz::Tz = state
        .settings
        .scheduler
        .timezone
        .parse()
        .map_err(|_| anyhow::anyhow!("config scheduler.timezone is not a valid IANA zone"))?;
    let local = event_start.with_timezone(&tz);
    Ok(local.format("%a %d %b %H:%M %Z").to_string())
}

pub async fn run(state: &AppState) -> anyhow::Result<()> {
    let cfg = &state.settings.scheduler;
    if !cfg.appointment_reminders_enabled {
        return Ok(());
    }

    let advance = Duration::minutes(cfg.appointment_reminder_advance_minutes as i64);
    let grace = Duration::minutes(cfg.appointment_start_grace_minutes as i64);
    let now = Utc::now();

    let list = events::list_all(state.db.as_ref()).map_err(|e| anyhow::anyhow!(e))?;

    let mut failures = 0usize;

    for event in list {
        let mut st = match event_notices::get(state.db.as_ref(), event.id)
            .map_err(|e| anyhow::anyhow!(e))?
        {
            Some(s) => s,
            None => EventNoticeState {
                start_unix: event.start.timestamp(),
                advance_sent: false,
                start_sent: false,
            },
        };

        let before_tick = st.clone();
        let (send_advance, send_start) =
            tick_notice_state(&mut st, event.start, now, advance, grace);

        // Each send is isolated and each flag is only kept if its own message went out. A `?`
        // here used to abandon every later appointment as well — and `tick_notice_state` has
        // already set the flags, so an aborted tick could also have marked something sent that
        // never was.
        if send_advance {
            match format_event_time(state, event.start) {
                Ok(when) => {
                    let until = event.start.signed_duration_since(now);
                    let secs = until.num_seconds().max(0);
                    let mut msg = if secs < 60 {
                        format!(
                            "Appointment in less than a minute: “{}” — {}",
                            event.title, when
                        )
                    } else {
                        let mins = until.num_minutes().clamp(0, 10_000) as u32;
                        format!(
                            "Appointment in about {} min: “{}” — {}",
                            mins, event.title, when
                        )
                    };
                    if let Some(desc) = event.description.as_deref() {
                        let d = desc.trim();
                        if !d.is_empty() {
                            msg.push('\n');
                            msg.push_str(d);
                        }
                    }
                    if let Err(e) = state
                        .telegram
                        .send_message(&state.secrets.telegram_chat_id.to_string(), &msg)
                        .await
                    {
                        tracing::warn!(id = event.id, error = %e, "appointment advance notice failed; will retry");
                        st.advance_sent = before_tick.advance_sent;
                        failures += 1;
                    }
                }
                Err(e) => {
                    tracing::warn!(id = event.id, error = %e, "appointment time formatting failed");
                    st.advance_sent = before_tick.advance_sent;
                    failures += 1;
                }
            }
        }

        if send_start {
            match format_event_time(state, event.start) {
                Ok(when) => {
                    let mut msg =
                        format!("Appointment starting now: “{}” — {}", event.title, when);
                    if let Some(desc) = event.description.as_deref() {
                        let d = desc.trim();
                        if !d.is_empty() {
                            msg.push('\n');
                            msg.push_str(d);
                        }
                    }
                    if let Err(e) = state
                        .telegram
                        .send_message(&state.secrets.telegram_chat_id.to_string(), &msg)
                        .await
                    {
                        tracing::warn!(id = event.id, error = %e, "appointment start notice failed; will retry");
                        st.start_sent = before_tick.start_sent;
                        failures += 1;
                    }
                }
                Err(e) => {
                    tracing::warn!(id = event.id, error = %e, "appointment time formatting failed");
                    st.start_sent = before_tick.start_sent;
                    failures += 1;
                }
            }
        }

        if st != before_tick {
            if let Err(e) = event_notices::put(state.db.as_ref(), event.id, &st) {
                tracing::error!(id = event.id, error = %e, "appointment notice state write failed");
                failures += 1;
            }
        }
    }

    if failures > 0 {
        anyhow::bail!("{failures} appointment notice(s) did not complete this tick");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s)
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn advance_then_start() {
        let start = ts("2026-04-10T12:00:00Z");
        let advance = Duration::minutes(30);
        let grace = Duration::minutes(15);

        let mut st = EventNoticeState {
            start_unix: start.timestamp(),
            advance_sent: false,
            start_sent: false,
        };

        let (a, s) = tick_notice_state(&mut st, start, ts("2026-04-10T11:29:00Z"), advance, grace);
        assert!(!a && !s);
        assert!(!st.advance_sent);

        let (a, s) = tick_notice_state(&mut st, start, ts("2026-04-10T11:30:00Z"), advance, grace);
        assert!(a && !s);
        assert!(st.advance_sent && !st.start_sent);

        let (a, s) = tick_notice_state(&mut st, start, ts("2026-04-10T12:00:00Z"), advance, grace);
        assert!(!a && s);
        assert!(st.start_sent);
    }

    #[test]
    fn missed_advance_still_gets_start_within_grace() {
        let start = ts("2026-04-10T12:00:00Z");
        let advance = Duration::minutes(30);
        let grace = Duration::minutes(15);
        let mut st = EventNoticeState {
            start_unix: start.timestamp(),
            advance_sent: false,
            start_sent: false,
        };

        let (a, s) = tick_notice_state(&mut st, start, ts("2026-04-10T12:05:00Z"), advance, grace);
        assert!(!a && s);
        assert!(st.advance_sent && st.start_sent);
    }

    #[test]
    fn start_too_late_skips_send_but_marks_sent() {
        let start = ts("2026-04-10T12:00:00Z");
        let advance = Duration::minutes(30);
        let grace = Duration::minutes(10);
        let mut st = EventNoticeState {
            start_unix: start.timestamp(),
            advance_sent: false,
            start_sent: false,
        };

        let (a, s) = tick_notice_state(&mut st, start, ts("2026-04-10T12:20:00Z"), advance, grace);
        assert!(!a && !s);
        assert!(st.start_sent);
    }

    #[test]
    fn reschedule_resets_flags() {
        let start = ts("2026-04-10T12:00:00Z");
        let mut st = EventNoticeState {
            start_unix: start.timestamp(),
            advance_sent: true,
            start_sent: true,
        };
        let new_start = ts("2026-04-11T12:00:00Z");
        let (a, s) = tick_notice_state(
            &mut st,
            new_start,
            ts("2026-04-11T11:30:00Z"),
            Duration::minutes(30),
            Duration::minutes(15),
        );
        assert!(a && !s);
        assert!(st.advance_sent && !st.start_sent);
        assert_eq!(st.start_unix, new_start.timestamp());
    }
}
