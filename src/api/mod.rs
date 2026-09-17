//! Local HTTP surface.
//!
//! Since the switch to Telegram long polling there is no inbound webhook: Mervyn dials out to
//! `api.telegram.org` instead of being called. This server exists only for `/health`, the
//! optional admin route, and to drive graceful shutdown — it is not publicly reachable.

use axum::routing::get;
use axum::Router;
use tower_http::trace::TraceLayer;

use crate::state::AppState;

mod admin;

pub fn router(state: AppState) -> Router {
    let mut app = Router::new().route("/health", get(|| async { "ok" }));

    if state.secrets.admin_token.is_some() {
        app = app
            .route("/admin/message-ingest", get(admin::list_message_ingest))
            .route("/admin/events", get(admin::list_events))
            .route("/admin/reminders", get(admin::list_reminders))
            .route("/admin/todos", get(admin::list_todos))
            // Why the vault and the database disagree — not answerable from either alone.
            .route("/admin/vault", get(admin::vault_status));
    }

    app.layer(TraceLayer::new_for_http()).with_state(state)
}
