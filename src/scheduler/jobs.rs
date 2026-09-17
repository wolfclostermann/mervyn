//! Cron jobs: morning briefing, reminder check, vault sync, optional vault git sync.

use chrono::Utc;


use crate::claude::prompts;
use crate::context::ContextAssembler;
use crate::state::AppState;
use crate::storage::reminders;
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

async fn run_reminder_check(state: &AppState) -> anyhow::Result<()> {
    let now = Utc::now();
    let due =
        reminders::pending_due_by(state.db.as_ref(), now, 50).map_err(|e| anyhow::anyhow!(e))?;
    for mut r in due {
        let msg = format!("Reminder: {}", r.body);
        state
            .telegram
            .send_message(&state.secrets.telegram_chat_id.to_string(), &msg)
            .await?;
        if let Some(next) = reminders::next_due_after_fire(&r) {
            r.due = next;
            r.done = false;
        } else {
            r.done = true;
        }
        reminders::put(state.db.as_ref(), &r).map_err(|e| anyhow::anyhow!(e))?;
    }

    super::appointment_reminders::run(state).await?;
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
