//! Cron jobs: morning briefing, reminder check, vault sync, optional vault git sync.

use chrono::Utc;


use crate::claude::prompts;
use crate::context::ContextAssembler;
use crate::state::AppState;
use crate::storage::reminder_notices::{self, ReminderNoticeState};
use crate::storage::reminders::{self, Reminder};
use crate::user_situation;
use crate::storage::message_ingest;
use tokio_cron_scheduler::{Job, JobScheduler};

/// One vault cycle. Synchronous on purpose: it takes the vault lock internally, so it must not
/// be held across an await — calling it as a plain expression keeps that impossible.
fn sync_vault(state: &AppState) -> anyhow::Result<crate::vault::sync::SyncStats> {
    crate::vault::sync::sync_vault_to_db(
        state.db.as_ref(),
        &state.vault_path,
        state.vault_sync_context(),
    )
}

async fn run_morning_briefing(state: &AppState) -> anyhow::Result<()> {
    let stats = sync_vault(state)?;
    tracing::info!(?stats, "vault synced before morning briefing");

    let now = Utc::now();
    let situation = user_situation::load_for_prompts(state.settings.as_ref());
    let asm = ContextAssembler::new(
        state.db.clone(),
        state.vault_path.clone(),
    );
    let (ev, rem, wl) = asm
        .briefing_prompt_sections(now, situation.as_deref())
        .await?;
    let sys = prompts::system_prompt_json(now, situation)?;
    let system = format!("{sys}\n\n{}", prompts::SUPPLEMENT_MORNING_BRIEFING);
    let user = prompts::morning_briefing_user_json(&ev, &rem, &wl)?;
    let text = state.claude.complete(Some(&system), &user).await?;
    state
        .telegram
        .send_message(&state.secrets.telegram_chat_id.to_string(), &text)
        .await?;
    Ok(())
}

/// Times one occurrence may be announced before the sweep stops trying.
///
/// Reached only when the send keeps succeeding and the row write keeps failing — a corrupt row,
/// say. Three is enough to ride out a transient problem and few enough that a stuck reminder does
/// not become a message a minute.
const MAX_REMINDER_SENDS: u32 = 3;

/// What to do with one due reminder.
#[derive(Debug, PartialEq, Eq)]
enum FireDecision {
    Send {
        /// What the notice count becomes if the row write then fails.
        attempt: u32,
    },
    /// Announced this occurrence `sends` times and the row never moved. Something is wrong with
    /// the row itself; announcing it again only adds noise.
    GiveUp { sends: u32 },
}

/// Decide from the reminder and any notice left by an earlier tick. Pure, so the awkward cases
/// are testable without a Telegram client.
fn decide_fire(
    reminder: &Reminder,
    notice: Option<&ReminderNoticeState>,
    max_sends: u32,
) -> FireDecision {
    // A notice for a different `due` belongs to an occurrence that has passed; ignore it.
    let sends = notice
        .filter(|n| n.due_unix == reminder.due.timestamp())
        .map_or(0, |n| n.sends);

    if sends >= max_sends {
        FireDecision::GiveUp { sends }
    } else {
        FireDecision::Send { attempt: sends + 1 }
    }
}

/// Advance a fired reminder: to its next occurrence, or to done.
fn advance_after_fire(reminder: &mut Reminder) {
    match reminders::next_due_after_fire(reminder) {
        Some(next) => {
            reminder.due = next;
            reminder.done = false;
        }
        None => reminder.done = true,
    }
}

/// Post due reminders and advance them.
///
/// Sends before it persists, and that order is deliberate. `sendMessage` has no idempotency key,
/// so a failure between the two steps loses something whichever way round they go: persisting
/// first risks a reminder that never arrives, sending first risks announcing it twice. A missed
/// reminder defeats the purpose; a repeated one is a nuisance. Hence send first, and bound the
/// nuisance with [`MAX_REMINDER_SENDS`].
///
/// Each reminder is isolated. A single failure used to abort the whole tick through `?`, taking
/// every later reminder and the appointment sweep with it — so one Telegram hiccup could silently
/// drop an appointment reminder that had nothing to do with it.
async fn run_reminder_check(state: &AppState) -> anyhow::Result<()> {
    let now = Utc::now();
    let due =
        reminders::pending_due_by(state.db.as_ref(), now, 50).map_err(|e| anyhow::anyhow!(e))?;

    let chat_id = state.secrets.telegram_chat_id.to_string();
    let mut failures = 0usize;

    for mut r in due {
        let notice = match reminder_notices::get(state.db.as_ref(), r.id) {
            Ok(n) => n,
            Err(e) => {
                tracing::warn!(id = r.id, error = %e, "reminder notice read failed");
                None
            }
        };

        match decide_fire(&r, notice.as_ref(), MAX_REMINDER_SENDS) {
            FireDecision::GiveUp { sends } => {
                tracing::error!(
                    id = r.id,
                    body = %r.body,
                    sends,
                    "reminder announced repeatedly but its row will not advance; not sending again \
                     (visible in /admin/reminders; clear it by editing or completing the reminder)"
                );
                failures += 1;
                continue;
            }
            FireDecision::Send { attempt } => {
                let msg = format!("Reminder: {}", r.body);
                if let Err(e) = state.telegram.send_message(&chat_id, &msg).await {
                    // Left un-advanced on purpose: the next tick tries again rather than
                    // swallowing a reminder nobody saw.
                    tracing::warn!(id = r.id, error = %e, "reminder send failed; will retry");
                    failures += 1;
                    continue;
                }

                advance_after_fire(&mut r);
                match reminders::put(state.db.as_ref(), &r) {
                    Ok(()) => {
                        // The row moved, so the occurrence is settled and its notice is spent.
                        if notice.is_some() {
                            let _ = reminder_notices::delete(state.db.as_ref(), r.id);
                        }
                    }
                    Err(e) => {
                        // Announced but not advanced: the next tick would announce it again, so
                        // record the attempt to keep that bounded.
                        tracing::error!(id = r.id, attempt, error = %e, "reminder sent but row not advanced");
                        let state_row = ReminderNoticeState {
                            due_unix: r.due.timestamp(),
                            sends: attempt,
                        };
                        if let Err(e) = reminder_notices::put(state.db.as_ref(), r.id, &state_row) {
                            tracing::error!(id = r.id, error = %e, "reminder notice write failed too");
                        }
                        failures += 1;
                    }
                }
            }
        }
    }

    // Runs whatever happened above: an appointment reminder must not be lost to an unrelated
    // reminder's bad day.
    let appointments = super::appointment_reminders::run(state).await;

    if let Err(e) = appointments {
        tracing::error!(error = %e, "appointment reminders");
        failures += 1;
    }
    if failures > 0 {
        anyhow::bail!("{failures} reminder(s) did not complete this tick");
    }
    Ok(())
}

async fn run_vault_sync(state: &AppState) -> anyhow::Result<()> {
    let s = sync_vault(state)?;
    tracing::debug!(?s, "scheduled vault sync");
    Ok(())
}

/// Pull the vault clone, reconcile it with the database, then commit and push what changed.
///
/// This is the whole cycle, and it is deliberately one blocking unit run off the async runtime:
/// it holds the vault lock from the pull through to the push, so that a file cannot move under a
/// merge that has already measured its byte offsets.
///
/// With `MERVYN_VAULT_GITHUB_PAT` set, HTTPS pushes to a private GitHub repo work without a
/// prompt. Pushing needs a token with **write** access, where the old worklog pull only needed
/// read.
pub async fn run_vault_git_sync(state: &AppState) -> anyhow::Result<()> {
    if !state.settings.vault_git.enabled {
        return Ok(());
    }

    let db = state.db.clone();
    let vault_path = state.vault_path.clone();
    let vault = state.vault.clone();
    let policy = state.write_back_policy();
    let tz = state.vault_tz();
    let git_cfg = state.settings.vault_git.clone();

    let stats = tokio::task::spawn_blocking(move || {
        let ctx = crate::vault::sync::SyncContext {
            access: vault.as_ref(),
            policy,
            tz,
        };
        crate::vault::sync::sync_vault_with_git(db.as_ref(), &vault_path, ctx, &git_cfg)
    })
    .await??;

    if stats.git_conflict {
        // A paused vault is otherwise invisible: the phone just quietly stops updating.
        let msg = "Heads up: the vault has diverged from its git remote and I could not rebase it. \
I have stopped writing to it until that is sorted — your notes are safe, but anything I add will \
not reach your other devices. Resolve the conflict in the vault clone and I will pick up again.";
        if let Err(e) = state
            .telegram
            .send_message(&state.secrets.telegram_chat_id.to_string(), msg)
            .await
        {
            tracing::warn!(error = %e, "could not report vault git conflict to chat");
        }
    }

    tracing::debug!(?stats, "vault git sync");
    Ok(())
}

async fn run_message_ingest_prune(state: &AppState) -> anyhow::Result<()> {
    let now_ms = Utc::now().timestamp_millis();
    let report = message_ingest::prune(
        state.db.as_ref(),
        now_ms,
        state.settings.storage.message_ingest_retention_days,
        state.settings.storage.message_ingest_keep_last,
    )
    .map_err(|e| anyhow::anyhow!(e))?;
    if report.total_removed() > 0 {
        tracing::info!(
            removed_by_age = report.removed_by_age,
            removed_by_cap = report.removed_by_cap,
            "message_ingest pruned"
        );
    } else {
        tracing::debug!("message_ingest prune: nothing to remove");
    }

    let sweep = message_ingest::sweep_stale_pending(
        state.db.as_ref(),
        now_ms,
        state.settings.storage.message_ingest_stale_pending_minutes,
    )
    .map_err(|e| anyhow::anyhow!(e))?;
    if sweep.rewound > 0 {
        tracing::info!(rewound = sweep.rewound, "message_ingest stale Pending swept");
    }

    Ok(())
}

pub async fn spawn_scheduler(state: AppState) -> anyhow::Result<()> {
    let tz: chrono_tz::Tz = state
        .settings
        .scheduler
        .timezone
        .parse()
        .map_err(|_| anyhow::anyhow!("config scheduler.timezone is not a valid IANA zone"))?;

    let sched = JobScheduler::new().await?;

    let st = state.clone();
    let c = st.settings.scheduler.morning_briefing_cron.clone();
    sched
        .add(Job::new_async_tz(c.as_str(), tz, move |_uuid, _lock| {
            let st = st.clone();
            Box::pin(async move {
                if let Err(e) = run_morning_briefing(&st).await {
                    tracing::error!(error = %e, "morning_briefing job");
                }
            })
        })?)
        .await?;

    let st = state.clone();
    let c = st.settings.scheduler.reminder_check_cron.clone();
    sched
        .add(Job::new_async_tz(c.as_str(), tz, move |_uuid, _lock| {
            let st = st.clone();
            Box::pin(async move {
                if let Err(e) = run_reminder_check(&st).await {
                    tracing::error!(error = %e, "reminder_check job");
                }
            })
        })?)
        .await?;

    let st = state.clone();
    let c = st.settings.scheduler.vault_sync_cron.clone();
    sched
        .add(Job::new_async_tz(c.as_str(), tz, move |_uuid, _lock| {
            let st = st.clone();
            Box::pin(async move {
                if let Err(e) = run_vault_sync(&st).await {
                    tracing::error!(error = %e, "vault_sync job");
                }
            })
        })?)
        .await?;

    let st = state.clone();
    let c = st.settings.scheduler.message_ingest_prune_cron.clone();
    sched
        .add(Job::new_async_tz(c.as_str(), tz, move |_uuid, _lock| {
            let st = st.clone();
            Box::pin(async move {
                if let Err(e) = run_message_ingest_prune(&st).await {
                    tracing::error!(error = %e, "message_ingest_prune job");
                }
            })
        })?)
        .await?;

    if state.settings.vault_git.enabled {
        let st = state.clone();
        let c = st.settings.vault_git.sync_cron.clone();
        sched
            .add(Job::new_async_tz(c.as_str(), tz, move |_uuid, _lock| {
                let st = st.clone();
                Box::pin(async move {
                    if let Err(e) = run_vault_git_sync(&st).await {
                        tracing::error!(error = %e, "vault_git_sync job");
                    }
                })
            })?)
            .await?;
    }

    tokio::spawn(async move {
        if let Err(e) = sched.start().await {
            tracing::error!(error = %e, "cron scheduler exited");
        }
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn reminder(due: &str, recurrence: Option<crate::storage::Recurrence>) -> Reminder {
        Reminder {
            id: 1,
            body: "Pay the tax bill".into(),
            due: chrono::DateTime::parse_from_rfc3339(due)
                .unwrap()
                .with_timezone(&Utc),
            recurrence,
            done: false,
        }
    }

    #[test]
    fn a_reminder_with_no_history_is_sent() {
        let r = reminder("2026-11-15T09:00:00Z", None);
        assert_eq!(
            decide_fire(&r, None, MAX_REMINDER_SENDS),
            FireDecision::Send { attempt: 1 }
        );
    }

    #[test]
    fn announcing_stops_once_the_row_has_refused_to_advance_enough_times() {
        let r = reminder("2026-11-15T09:00:00Z", None);
        for sends in 0..MAX_REMINDER_SENDS {
            let n = ReminderNoticeState {
                due_unix: r.due.timestamp(),
                sends,
            };
            assert_eq!(
                decide_fire(&r, Some(&n), MAX_REMINDER_SENDS),
                FireDecision::Send { attempt: sends + 1 },
                "still trying at {sends}"
            );
        }
        let spent = ReminderNoticeState {
            due_unix: r.due.timestamp(),
            sends: MAX_REMINDER_SENDS,
        };
        assert_eq!(
            decide_fire(&r, Some(&spent), MAX_REMINDER_SENDS),
            FireDecision::GiveUp {
                sends: MAX_REMINDER_SENDS
            },
            "a reminder that will not settle must stop shouting"
        );
    }

    #[test]
    fn a_notice_for_a_past_occurrence_does_not_silence_the_next_one() {
        // A recurring reminder that got stuck last week must still fire this week; a reminder
        // rescheduled in the vault likewise starts clean.
        let r = reminder("2026-11-15T09:00:00Z", None);
        let stale = ReminderNoticeState {
            due_unix: Utc
                .with_ymd_and_hms(2026, 11, 8, 9, 0, 0)
                .unwrap()
                .timestamp(),
            sends: 99,
        };
        assert_eq!(
            decide_fire(&r, Some(&stale), MAX_REMINDER_SENDS),
            FireDecision::Send { attempt: 1 }
        );
    }

    #[test]
    fn a_one_off_reminder_closes_and_a_recurring_one_moves_on() {
        let mut once = reminder("2026-11-15T09:00:00Z", None);
        advance_after_fire(&mut once);
        assert!(once.done, "nothing left to fire");

        let mut weekly = reminder("2026-11-15T09:00:00Z", Some(crate::storage::Recurrence::Weekly));
        advance_after_fire(&mut weekly);
        assert!(!weekly.done, "still live");
        assert_eq!(weekly.due.to_rfc3339(), "2026-11-22T09:00:00+00:00");
    }

    #[test]
    fn advancing_a_recurring_reminder_moves_it_past_the_notice_that_bounded_it() {
        // The bound keys on `due`, so a reminder that recovers is not held back by its own history.
        let mut r = reminder("2026-11-15T09:00:00Z", Some(crate::storage::Recurrence::Daily));
        let stuck = ReminderNoticeState {
            due_unix: r.due.timestamp(),
            sends: MAX_REMINDER_SENDS,
        };
        assert!(matches!(
            decide_fire(&r, Some(&stuck), MAX_REMINDER_SENDS),
            FireDecision::GiveUp { .. }
        ));

        advance_after_fire(&mut r);
        assert_eq!(
            decide_fire(&r, Some(&stuck), MAX_REMINDER_SENDS),
            FireDecision::Send { attempt: 1 },
            "tomorrow's occurrence is not today's"
        );
    }
}
