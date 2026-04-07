//! Cron jobs: morning briefing, reminder check, vault sync.

use chrono::Utc;

use crate::claude::prompts;
use crate::context::ContextAssembler;
use crate::state::AppState;
use crate::storage::reminders;
use crate::user_situation;
use crate::storage::slack_ingest;
use tokio_cron_scheduler::{Job, JobScheduler};

async fn run_morning_briefing(state: &AppState) -> anyhow::Result<()> {
    let stats = crate::vault::sync::sync_vault_to_db(state.db.as_ref(), &state.vault_path)?;
    tracing::info!(?stats, "vault synced before morning briefing");

    let now = Utc::now();
    let situation = user_situation::load_for_prompts(state.settings.as_ref());
    let asm = ContextAssembler::new(state.db.clone(), state.vault_path.clone());
    let (ev, rem, wl) = asm
        .briefing_prompt_sections(now, situation.as_deref())
        .await?;
    let sys = prompts::system_prompt_json(now, situation)?;
    let system = format!("{sys}\n\n{}", prompts::SUPPLEMENT_MORNING_BRIEFING);
    let user = prompts::morning_briefing_user_json(&ev, &rem, &wl)?;
    let text = state.claude.complete(Some(&system), &user).await?;
    state
        .slack
        .post_message(&state.secrets.slack_channel_id, &text, None)
        .await?;
    Ok(())
}

async fn run_reminder_check(state: &AppState) -> anyhow::Result<()> {
    let now = Utc::now();
    let due =
        reminders::pending_due_by(state.db.as_ref(), now, 50).map_err(|e| anyhow::anyhow!(e))?;
    for mut r in due {
        let msg = format!("Reminder: {}", r.body);
        state
            .slack
            .post_message(&state.secrets.slack_channel_id, &msg, None)
            .await?;
        if let Some(next) = reminders::next_due_after_fire(&r) {
            r.due = next;
            r.done = false;
        } else {
            r.done = true;
        }
        reminders::put(state.db.as_ref(), &r).map_err(|e| anyhow::anyhow!(e))?;
    }
    Ok(())
}

async fn run_vault_sync(state: &AppState) -> anyhow::Result<()> {
    let s = crate::vault::sync::sync_vault_to_db(state.db.as_ref(), &state.vault_path)?;
    tracing::debug!(?s, "scheduled vault sync");
    Ok(())
}

async fn run_slack_ingest_prune(state: &AppState) -> anyhow::Result<()> {
    let now_ms = Utc::now().timestamp_millis();
    let report = slack_ingest::prune(
        state.db.as_ref(),
        now_ms,
        state.settings.storage.slack_ingest_retention_days,
        state.settings.storage.slack_ingest_keep_last,
    )
    .map_err(|e| anyhow::anyhow!(e))?;
    if report.total_removed() > 0 {
        tracing::info!(
            removed_by_age = report.removed_by_age,
            removed_by_cap = report.removed_by_cap,
            "slack_ingest pruned"
        );
    } else {
        tracing::debug!("slack_ingest prune: nothing to remove");
    }

    let sweep = slack_ingest::sweep_stale_pending(
        state.db.as_ref(),
        now_ms,
        state.settings.storage.slack_ingest_stale_pending_minutes,
    )
    .map_err(|e| anyhow::anyhow!(e))?;
    if sweep.rewound > 0 {
        tracing::info!(rewound = sweep.rewound, "slack_ingest stale Pending swept");
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
    let c = st.settings.scheduler.slack_ingest_prune_cron.clone();
    sched
        .add(Job::new_async_tz(c.as_str(), tz, move |_uuid, _lock| {
            let st = st.clone();
            Box::pin(async move {
                if let Err(e) = run_slack_ingest_prune(&st).await {
                    tracing::error!(error = %e, "slack_ingest_prune job");
                }
            })
        })?)
        .await?;

    tokio::spawn(async move {
        if let Err(e) = sched.start().await {
            tracing::error!(error = %e, "cron scheduler exited");
        }
    });

    Ok(())
}
