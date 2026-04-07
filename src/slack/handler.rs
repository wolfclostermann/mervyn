//! Route Slack messages to intent handlers.
//!
//! Pipeline: **ingest** (append every signed delivery) → **dedupe** ([`try_claim_slack_delivery`])
//! → **filter** (bot / subtype / type / empty text) → **action** (classify + dispatch). Final state is
//! written to [`crate::storage::slack_ingest`]. If the handler returns [`Err`], the dedupe claim is
//! released so Slack can retry.
//!
//! After a successful dedupe **claim**, [`SlackMetaClaimGuard`] releases the meta key on drop (e.g.
//! panic) unless [`SlackMetaClaimGuard::disarm`] runs on the success path where the claim must stay.
//! Stale `Pending` rows are still handled by the scheduled sweeper in [`crate::storage::slack_ingest`].

use std::sync::Arc;

use redb::Database;

use crate::intent::{self, IntentLabel};
use crate::user_situation;
use crate::slack::events::SlackEvent;
use crate::state::AppState;
use crate::storage::meta;
use crate::storage::slack_ingest::{self, SlackIngestOutcome};

/// While held, a successful Slack `event_id` meta claim is released on [`Drop`] unless [`disarm`](Self::disarm).
struct SlackMetaClaimGuard {
    db: Arc<Database>,
    event_id: String,
    release_on_drop: bool,
}

impl SlackMetaClaimGuard {
    fn new(db: Arc<Database>, event_id: String) -> Self {
        Self {
            db,
            event_id,
            release_on_drop: true,
        }
    }

    fn disarm(mut self) {
        self.release_on_drop = false;
    }
}

impl Drop for SlackMetaClaimGuard {
    fn drop(&mut self) {
        if self.release_on_drop && !self.event_id.is_empty() {
            let _ = meta::release_slack_delivery(&self.db, &self.event_id);
        }
    }
}

pub async fn process_event(
    state: AppState,
    slack_event_id: String,
    retry_num: Option<u32>,
    event: SlackEvent,
) -> anyhow::Result<()> {
    let ingest_id = slack_ingest::append(
        state.db.as_ref(),
        slack_event_id.clone(),
        retry_num,
        event.kind.clone(),
    )
    .map_err(|e| anyhow::anyhow!(e))?;

    tracing::debug!(
        ingest_id,
        event_id = %slack_event_id,
        ?retry_num,
        inner_type = %event.kind,
        "slack delivery ingested"
    );

    let dedupe = !slack_event_id.is_empty();
    if dedupe
        && !meta::try_claim_slack_delivery(state.db.as_ref(), &slack_event_id)
            .map_err(|e| anyhow::anyhow!(e))?
    {
        slack_ingest::set_outcome(
            state.db.as_ref(),
            ingest_id,
            SlackIngestOutcome::DuplicateDelivery,
        )
        .map_err(|e| anyhow::anyhow!(e))?;
        tracing::debug!(ingest_id, "slack duplicate event_id after ingest");
        return Ok(());
    }

    let claim_guard = dedupe.then(|| {
        SlackMetaClaimGuard::new(state.db.clone(), slack_event_id.clone())
    });

    if event.bot_id.is_some() {
        release_if_claimed(&state, dedupe, &slack_event_id)?;
        disarm_claim(claim_guard);
        slack_ingest::set_outcome(state.db.as_ref(), ingest_id, SlackIngestOutcome::FilteredBot)
            .map_err(|e| anyhow::anyhow!(e))?;
        return Ok(());
    }
    if event.subtype.is_some() {
        release_if_claimed(&state, dedupe, &slack_event_id)?;
        disarm_claim(claim_guard);
        slack_ingest::set_outcome(state.db.as_ref(), ingest_id, SlackIngestOutcome::FilteredSubtype)
            .map_err(|e| anyhow::anyhow!(e))?;
        return Ok(());
    }
    if !matches!(event.kind.as_str(), "message" | "app_mention") {
        release_if_claimed(&state, dedupe, &slack_event_id)?;
        disarm_claim(claim_guard);
        slack_ingest::set_outcome(
            state.db.as_ref(),
            ingest_id,
            SlackIngestOutcome::FilteredUnsupportedType,
        )
        .map_err(|e| anyhow::anyhow!(e))?;
        return Ok(());
    }
    let text = event.text.clone().unwrap_or_default();
    if text.trim().is_empty() {
        release_if_claimed(&state, dedupe, &slack_event_id)?;
        disarm_claim(claim_guard);
        slack_ingest::set_outcome(
            state.db.as_ref(),
            ingest_id,
            SlackIngestOutcome::FilteredEmptyText,
        )
        .map_err(|e| anyhow::anyhow!(e))?;
        return Ok(());
    }

    let result = handle_user_message(&state, event, text).await;
    match &result {
        Ok(()) => {
            // Disarm before `set_outcome` so a panic during persistence does not release the dedupe
            // claim after a successful handler run.
            disarm_claim(claim_guard);
            slack_ingest::set_outcome(state.db.as_ref(), ingest_id, SlackIngestOutcome::Processed)
                .map_err(|e| anyhow::anyhow!(e))?;
        }
        Err(e) => {
            release_if_claimed(&state, dedupe, &slack_event_id)?;
            disarm_claim(claim_guard);
            slack_ingest::set_outcome(
                state.db.as_ref(),
                ingest_id,
                SlackIngestOutcome::Failed(short_err(e)),
            )
            .map_err(|e| anyhow::anyhow!(e))?;
        }
    }
    result
}

fn disarm_claim(guard: Option<SlackMetaClaimGuard>) {
    if let Some(g) = guard {
        g.disarm();
    }
}

fn release_if_claimed(state: &AppState, dedupe: bool, slack_event_id: &str) -> anyhow::Result<()> {
    if dedupe {
        meta::release_slack_delivery(state.db.as_ref(), slack_event_id).map_err(|e| anyhow::anyhow!(e))?;
    }
    Ok(())
}

fn short_err(e: impl std::fmt::Display) -> String {
    e.to_string().chars().take(256).collect()
}

async fn handle_user_message(
    state: &AppState,
    event: SlackEvent,
    text: String,
) -> anyhow::Result<()> {
    let cleaned = strip_leading_bot_mention(&text);
    let channel = event
        .channel
        .as_deref()
        .unwrap_or(state.secrets.slack_channel_id.as_str());
    let thread_parent = event.thread_ts.as_deref().or(event.ts.as_deref());

    let situation = user_situation::load_for_prompts(state.settings.as_ref());
    let label = match intent::classify_intent(&state.claude, &cleaned, situation.clone()).await {
        Ok(l) => l,
        Err(e) => {
            tracing::warn!(error = %e, "intent classification failed; defaulting to ask");
            IntentLabel::Ask
        }
    };
    intent::dispatch(state, label, &cleaned, channel, thread_parent, situation).await
}

fn strip_leading_bot_mention(text: &str) -> String {
    let t = text.trim();
    if let Some(rest) = t.strip_prefix("<@") {
        if let Some(end) = rest.find('>') {
            return rest[end + 1..].trim().to_string();
        }
    }
    t.to_string()
}
