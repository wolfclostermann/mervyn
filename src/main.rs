mod api;
mod claude;
mod config;
mod context;
mod error;
mod intent;
mod scheduler;
mod slack;
mod state;
mod storage;
mod vault;

use std::path::Path;
use std::sync::Arc;

use anyhow::Context;

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

    let claude = Arc::new(
        ClaudeClient::new(
            secrets.anthropic_api_key.clone(),
            settings.claude.model.clone(),
            settings.claude.max_tokens,
        )
        .context("anthropic client")?,
    );
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

    tracing::info!(port = settings.server.port, "mervyn starting");

    let app = api::router(app_state);
    let addr = format!("0.0.0.0:{}", settings.server.port);
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .with_context(|| format!("bind {addr}"))?;
    axum::serve(listener, app).await?;
    Ok(())
}
