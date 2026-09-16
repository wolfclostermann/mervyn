//! Long-poll loop: ask Telegram for updates, hand each to the handler, advance the cursor.
//!
//! The cursor (`offset`) is both the acknowledgement and the replay guard: calling `getUpdates`
//! with an offset above an `update_id` confirms it server-side, so Telegram stops resending it.
//! It is persisted after each batch, so a restart resumes rather than replaying.
//!
//! The offset advances only **after** a batch is handled. A crash mid-batch therefore redelivers,
//! which is why the handler still claims each `update_id` for dedupe.

use std::time::Duration;

use tokio::sync::watch;

use crate::state::AppState;
use crate::storage::meta;
use crate::telegram::handler;

/// Seconds to hold each long-poll request open.
const POLL_TIMEOUT_SECS: u64 = 30;

/// Backoff after a failed poll, so a network outage does not become a hot loop.
const ERROR_BACKOFF: Duration = Duration::from_secs(5);

/// Run until `shutdown` flips to `true`.
pub async fn run(state: AppState, mut shutdown: watch::Receiver<bool>) {
    let mut offset = match meta::get_poll_offset(state.db.as_ref()) {
        Ok(o) => o,
        Err(e) => {
            tracing::warn!(error = %e, "could not read poll offset; starting from the current tail");
            None
        }
    };
    tracing::info!(?offset, "telegram poller started");

    loop {
        if *shutdown.borrow() {
            break;
        }

        let poll = state.telegram.get_updates(offset, POLL_TIMEOUT_SECS);
        let updates = tokio::select! {
            r = poll => match r {
                Ok(u) => u,
                Err(e) => {
                    tracing::warn!(error = %e, "telegram getUpdates failed; backing off");
                    tokio::select! {
                        _ = tokio::time::sleep(ERROR_BACKOFF) => {}
                        _ = shutdown.changed() => {}
                    }
                    continue;
                }
            },
            _ = shutdown.changed() => break,
        };

        if updates.is_empty() {
            continue;
        }

        let highest = updates.iter().map(|u| u.update_id).max();
        for update in updates {
            let update_id = update.update_id;
            if let Err(e) = handler::process_update(state.clone(), update).await {
                // Already recorded as `Failed` in the ingest log; keep polling.
                tracing::error!(error = %e, update_id, "telegram update handler");
            }
        }

        // Advance past the whole batch, whether individual updates succeeded or not: a poisoned
        // update must not wedge the loop into redelivering it forever.
        if let Some(h) = highest {
            offset = Some(h + 1);
            if let Err(e) = meta::set_poll_offset(state.db.as_ref(), h + 1) {
                tracing::warn!(error = %e, "could not persist poll offset");
            }
        }
    }

    tracing::info!("telegram poller stopped");
}
