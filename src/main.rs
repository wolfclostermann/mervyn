mod api;
mod claude;
mod config;
mod context;
mod error;
mod intent;
mod ngrok_tunnel;
mod scheduler;
mod slack;
mod state;
mod storage;
mod user_situation;
mod vault;

use std::path::Path;
use std::sync::Arc;

use anyhow::Context;

#[cfg(unix)]
use tokio::signal::unix::{signal, SignalKind};

use crate::claude::client::ClaudeClient;
use crate::slack::client::SlackClient;
use crate::state::{AppState, Secrets};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();

    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let settings = Arc::new(config::load().context("load config")?);
    let secrets = Arc::new(Secrets::from_env().context("load secrets from environment")?);

    let db_path = Path::new(&settings.storage.db_path);
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent).context("create database directory")?;
    }
    let vault_path = Path::new(&settings.storage.vault_path);
    std::fs::create_dir_all(vault_path).context("create vault directory")?;

    let db = Arc::new(
        storage::db::open(&settings.storage.db_path).context("open redb")?,
    );

    let claude = Arc::new(ClaudeClient::new(
        secrets.anthropic_api_key.clone(),
        settings.claude.model.clone(),
        settings.claude.max_tokens,
    ));
    let slack = Arc::new(SlackClient::new(secrets.slack_bot_token.clone()));

    let app_state = AppState {
        settings: settings.clone(),
        secrets,
        db,
        claude,
        slack,
        vault_path: vault_path.to_path_buf(),
    };

    scheduler::spawn_scheduler(app_state.clone())
        .await
        .context("start scheduler")?;

    vault::watcher::spawn_vault_watcher(app_state.db.clone(), app_state.vault_path.clone());

    let _ngrok_forwarder = ngrok_tunnel::start(&settings.ngrok, settings.server.port)
        .await
        .context("start ngrok tunnel")?;

    let startup_msg = "Hello! Mervyn is up and running.";
    match app_state
        .slack
        .post_message(&app_state.secrets.slack_channel_id, startup_msg, None)
        .await
    {
        Ok(()) => tracing::info!("slack startup greeting sent"),
        Err(e) => tracing::warn!(error = %e, "slack startup greeting failed"),
    }

    tracing::info!(port = settings.server.port, "mervyn starting");

    let state_for_shutdown = app_state.clone();
    let app = api::router(app_state);
    let addr = format!("0.0.0.0:{}", settings.server.port);
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .with_context(|| format!("bind {addr}"))?;
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    let shutdown_msg = "Goodbye! Mervyn is shutting down.";
    match state_for_shutdown
        .slack
        .post_message(&state_for_shutdown.secrets.slack_channel_id, shutdown_msg, None)
        .await
    {
        Ok(()) => tracing::info!("slack shutdown message sent"),
        Err(e) => tracing::warn!(error = %e, "slack shutdown message failed"),
    }

    Ok(())
}

/// Waits for Ctrl+C or (on Unix) SIGTERM so the HTTP server can drain and exit cleanly.
async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let sigterm = async {
        signal(SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let sigterm = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {},
        _ = sigterm => {},
    }
}
