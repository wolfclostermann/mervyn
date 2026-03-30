use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::Json;
use axum::Router;
use tower_http::trace::TraceLayer;

use crate::slack::events::{parse_retry_num, verify_slack_signature, SlackEnvelope};
use crate::slack::handler;
use crate::state::AppState;

mod admin;

pub fn router(state: AppState) -> Router {
    let mut app = Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/slack/events", post(slack_events));

    if state.secrets.admin_token.is_some() {
        app = app.route("/admin/slack-ingest", get(admin::list_slack_ingest));
    }

    app.layer(TraceLayer::new_for_http()).with_state(state)
}

async fn slack_events(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    if let Err(e) = verify_slack_signature(
        state.secrets.slack_signing_secret.as_bytes(),
        &headers,
        &body,
    ) {
        tracing::warn!("slack signature: {e}");
        return StatusCode::UNAUTHORIZED.into_response();
    }

    let env: SlackEnvelope = match serde_json::from_slice(&body) {
        Ok(e) => e,
        Err(err) => {
            tracing::warn!(?err, "slack envelope parse");
            return StatusCode::BAD_REQUEST.into_response();
        }
    };

    match env {
        SlackEnvelope::UrlVerification { challenge } => {
            Json(serde_json::json!({ "challenge": challenge })).into_response()
        }
        SlackEnvelope::EventCallback { event, event_id } => {
            let slack_event_id = event_id.unwrap_or_default();
            let retry_num = parse_retry_num(&headers);
            let st = state.clone();
            tokio::spawn(async move {
                if let Err(e) =
                    handler::process_event(st, slack_event_id, retry_num, event).await
                {
                    tracing::error!(error = %e, "slack event handler");
                }
            });
            StatusCode::OK.into_response()
        }
    }
}
