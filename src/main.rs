mod api;
mod claude;
mod config;
mod context;
mod error;
mod intent;
mod scheduler;
mod slack;
mod storage;
mod vault;

use std::path::Path;

use anyhow::Context;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();

    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let settings = config::load().context("load config")?;

    let db_path = Path::new(&settings.storage.db_path);
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent).context("create database directory")?;
    }
    let vault_path = Path::new(&settings.storage.vault_path);
    std::fs::create_dir_all(vault_path).context("create vault directory")?;

    tracing::info!(port = settings.server.port, "mervyn starting");

    let app = api::router();
    let addr = format!("0.0.0.0:{}", settings.server.port);
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .with_context(|| format!("bind {addr}"))?;
    axum::serve(listener, app).await?;
    Ok(())
}
