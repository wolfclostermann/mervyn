//! Route Telegram messages to intent handlers.
//!
//! Pipeline: **authorize** (sender allowlist) → **ingest** (append the delivery) →
//! **dedupe** ([`meta::try_claim_delivery`]) → **filter** (bot / non-text / empty) →
//! **action** (classify + dispatch). Final state lands in [`crate::storage::message_ingest`].
//! If the handler returns [`Err`], the dedupe claim is released so a redelivery can retry.
//!
//! After a successful claim, [`DeliveryClaimGuard`] releases the meta key on drop (e.g. panic)
//! unless [`DeliveryClaimGuard::disarm`] runs on the success path where the claim must stay.
//!
//! **Authorization runs before anything is written.** Long polling has no transport-level
//! authentication — unlike Slack's signing secret, nothing stops a stranger who discovers the
//! bot from messaging it. Rejecting before the ingest write means an unknown sender cannot
//! grow the database, and cannot reach Claude.

use std::sync::Arc;

use redb::Database;

use crate::intent::{self, IntentLabel};
use crate::state::AppState;
use crate::storage::message_ingest::{self, MessageIngestOutcome};
use crate::storage::meta;
use crate::telegram::updates::Update;
use crate::user_situation;

/// While held, a successful delivery claim is released on [`Drop`] unless [`disarm`](Self::disarm).
struct DeliveryClaimGuard {
    db: Arc<Database>,
    delivery_id: String,
    release_on_drop: bool,
}

impl DeliveryClaimGuard {
    fn new(db: Arc<Database>, delivery_id: String) -> Self {
        Self {
            db,
            delivery_id,
            release_on_drop: true,
        }
    }

    fn disarm(mut self) {
        self.release_on_drop = false;
    }
}

impl Drop for DeliveryClaimGuard {
    fn drop(&mut self) {
        if self.release_on_drop && !self.delivery_id.is_empty() {
            let _ = meta::release_delivery(&self.db, &self.delivery_id);
        }
    }
}

/// Is this update from the one chat Mervyn answers?
///
/// Pure so the allowlist is testable without a database or a network.
pub fn is_authorized(update: &Update, allowed_chat_id: i64) -> bool {
    update.chat_id() == Some(allowed_chat_id)
}

/// Handle one update. Returns `Ok(())` for updates that were deliberately ignored.
pub async fn process_update(state: AppState, update: Update) -> anyhow::Result<()> {
    let delivery_id = update.update_id.to_string();

    // Authorization first — before any write, and before any Claude call.
    if !is_authorized(&update, state.secrets.telegram_chat_id) {
        tracing::warn!(
            update_id = update.update_id,
            chat_id = ?update.chat_id(),
            sender_id = ?update.sender_id(),
            "telegram update from unauthorized chat; ignoring"
        );
        return Ok(());
    }

    let inner_type = if update.text().is_some() { "text" } else { "other" };
    let ingest_id = message_ingest::append(state.db.as_ref(), delivery_id.clone(), None, inner_type.to_string())
        .map_err(|e| anyhow::anyhow!(e))?;

    tracing::debug!(ingest_id, update_id = update.update_id, inner_type, "telegram delivery ingested");

    if !meta::try_claim_delivery(state.db.as_ref(), &delivery_id).map_err(|e| anyhow::anyhow!(e))? {
        set_outcome(&state, ingest_id, MessageIngestOutcome::DuplicateDelivery)?;
        tracing::debug!(ingest_id, update_id = update.update_id, "duplicate update_id after ingest");
        return Ok(());
    }

    let claim_guard = DeliveryClaimGuard::new(state.db.clone(), delivery_id.clone());

    if update.is_from_bot() {
        return finish_filtered(&state, claim_guard, ingest_id, MessageIngestOutcome::FilteredBot);
    }
    let Some(text) = update.text().map(str::to_string) else {
        // Either not a message at all, or a non-text message (sticker, photo, service event).
        let outcome = if update.message.is_some() {
            MessageIngestOutcome::FilteredEmptyText
        } else {
            MessageIngestOutcome::FilteredNonText
        };
        return finish_filtered(&state, claim_guard, ingest_id, outcome);
    };

    let chat_id = state.secrets.telegram_chat_id;
    let result = handle_user_message(&state, chat_id, text).await;
    match &result {
        Ok(()) => {
            // Disarm before persisting so a panic during the write cannot release the claim
            // after the handler has already run its side effects.
            claim_guard.disarm();
            set_outcome(&state, ingest_id, MessageIngestOutcome::Processed)?;
        }
        Err(e) => {
            meta::release_delivery(state.db.as_ref(), &delivery_id).map_err(|e| anyhow::anyhow!(e))?;
            claim_guard.disarm();
            set_outcome(&state, ingest_id, MessageIngestOutcome::Failed(short_err(e)))?;
        }
    }
    result
}

fn finish_filtered(
    state: &AppState,
    guard: DeliveryClaimGuard,
    ingest_id: u64,
    outcome: MessageIngestOutcome,
) -> anyhow::Result<()> {
    meta::release_delivery(state.db.as_ref(), &guard.delivery_id).map_err(|e| anyhow::anyhow!(e))?;
    guard.disarm();
    set_outcome(state, ingest_id, outcome)
}

fn set_outcome(state: &AppState, ingest_id: u64, outcome: MessageIngestOutcome) -> anyhow::Result<()> {
    message_ingest::set_outcome(state.db.as_ref(), ingest_id, outcome).map_err(|e| anyhow::anyhow!(e))
}

fn short_err(e: impl std::fmt::Display) -> String {
    e.to_string().chars().take(256).collect()
}

async fn handle_user_message(state: &AppState, chat_id: i64, text: String) -> anyhow::Result<()> {
    let situation = user_situation::load_for_prompts(state.settings.as_ref());
    let label = match intent::classify_intent(&state.claude, &text, situation.clone()).await {
        Ok(l) => l,
        Err(e) => {
            tracing::warn!(error = %e, "intent classification failed; defaulting to ask");
            IntentLabel::Ask
        }
    };
    intent::dispatch(state, label, &text, chat_id, situation).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn update_from_chat(chat_id: i64) -> Update {
        let raw = format!(
            r#"{{"update_id":1,"message":{{"message_id":1,"from":{{"id":{chat_id},"is_bot":false}},
                "chat":{{"id":{chat_id}}},"date":1,"text":"hi"}}}}"#
        );
        serde_json::from_str(&raw).unwrap()
    }

    const OWNER: i64 = 12345678;

    #[test]
    fn owner_chat_is_authorized() {
        assert!(is_authorized(&update_from_chat(OWNER), OWNER));
    }

    #[test]
    fn other_chat_is_rejected() {
        assert!(!is_authorized(&update_from_chat(99999999), OWNER));
    }

    #[test]
    fn negative_group_chat_id_is_rejected_when_not_the_owner() {
        // Group/channel ids are negative; make sure sign handling does not let one through.
        assert!(!is_authorized(&update_from_chat(-100200300), OWNER));
    }

    #[test]
    fn update_without_a_chat_is_rejected() {
        let u: Update = serde_json::from_str(r#"{"update_id":5}"#).unwrap();
        assert!(!is_authorized(&u, OWNER));
    }

    #[test]
    fn zero_allowlist_does_not_match_a_real_chat() {
        // Guards against a misconfigured/empty TELEGRAM_CHAT_ID silently accepting everyone.
        assert!(!is_authorized(&update_from_chat(OWNER), 0));
    }
}
