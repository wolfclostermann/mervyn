mod api;
mod claude;
mod config;
mod context;
mod error;
mod intent;
mod scheduler;
mod telegram;
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
use crate::telegram::client::TelegramClient;
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
    let telegram = Arc::new(TelegramClient::new(secrets.telegram_bot_token.clone()));

    let app_state = AppState {
        settings: settings.clone(),
        secrets,
        db,
        claude,
        telegram,
        vault_path: vault_path.to_path_buf(),
        vault: Arc::new(vault::write::VaultAccess::new()),
    };

    scheduler::spawn_scheduler(app_state.clone())
        .await
        .context("start scheduler")?;

    vault::watcher::spawn_vault_watcher(
        app_state.db.clone(),
        app_state.vault_path.clone(),
        app_state.vault.clone(),
        app_state.write_back_policy(),
    );

    if let Err(e) = scheduler::run_worklog_git_pull(&app_state).await {
        tracing::error!(error = %e, "worklog git pull on startup");
    }

    match app_state.telegram.get_me().await {
        Ok(username) => tracing::info!(bot = %username, "telegram bot authenticated"),
        Err(e) => tracing::warn!(error = %e, "telegram getMe failed; check TELEGRAM_BOT_TOKEN"),
    }

    let chat_id = app_state.secrets.telegram_chat_id.to_string();
    let startup_msg = "Hello! Mervyn is up and running.";
    match app_state.telegram.send_message(&chat_id, startup_msg).await {
        Ok(()) => tracing::info!("telegram startup greeting sent"),
        Err(e) => tracing::warn!(error = %e, "telegram startup greeting failed"),
    }

    // Long polling runs alongside the HTTP server and stops when the server begins draining.
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let poller = tokio::spawn(telegram::poller::run(app_state.clone(), shutdown_rx));

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

    let _ = shutdown_tx.send(true);
    if let Err(e) = poller.await {
        tracing::warn!(error = %e, "telegram poller join");
    }

    let shutdown_msg = "Goodbye! Mervyn is shutting down.";
    match state_for_shutdown
        .telegram
        .send_message(&state_for_shutdown.secrets.telegram_chat_id.to_string(), shutdown_msg)
        .await
    {
        Ok(()) => tracing::info!("telegram shutdown message sent"),
        Err(e) => tracing::warn!(error = %e, "telegram shutdown message failed"),
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
