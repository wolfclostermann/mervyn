//! Verify `NGROK_AUTHTOKEN` and print the public tunnel URL without running Mervyn.
//!
//! Loads `.env`, `config/default.toml`, and `MERVYN__*` (same as the main binary). Forwards the
//! tunnel to `http://127.0.0.1:<server.port>` so once Mervyn is up, Slack can hit your app.
//!
//! By default waits for **Ctrl+C** so the URL stays valid. For a quick auth check only, set
//! `MERVYN_CHECK_NGROK_SECONDS` to a small number (the tunnel is torn down when this process exits).

use std::sync::Once;

use anyhow::Context;
use ngrok::config::ForwarderBuilder;
use ngrok::prelude::EndpointInfo;
use ngrok::session::Session;
use tokio::time::{sleep, Duration};

static RUSTLS_PROVIDER: Once = Once::new();

fn ensure_rustls_aws_lc() {
    RUSTLS_PROVIDER.call_once(|| {
        rustls::crypto::aws_lc_rs::default_provider()
            .install_default()
            .expect("install rustls aws_lc_rs provider (required for ngrok)");
    });
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    ensure_rustls_aws_lc();

    dotenvy::dotenv().ok();

    let settings = config::Config::builder()
        .add_source(config::File::with_name("config/default"))
        .add_source(
            config::Environment::with_prefix("MERVYN")
                .separator("__")
                .try_parsing(true),
        )
        .build()
        .context("load config")?;

    let port: u16 = settings
        .get_int("server.port")
        .unwrap_or(3000)
        .try_into()
        .context("server.port must fit in u16")?;

    let domain = settings
        .get_string("ngrok.domain")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    let session = Session::builder()
        .authtoken_from_env()
        .connect()
        .await
        .context("ngrok session connect (set NGROK_AUTHTOKEN in .env)")?;

    let mut endpoint = session.http_endpoint();
    if let Some(ref d) = domain {
        endpoint.domain(d);
    }

    let upstream = format!("http://127.0.0.1:{port}");
    let forwarder = endpoint
        .listen_and_forward(
            upstream
                .parse()
                .with_context(|| format!("parse upstream URL {upstream}"))?,
        )
        .await
        .context("start ngrok HTTP tunnel")?;

    let base = forwarder.url().trim_end_matches('/');
    println!("ngrok authentication OK.");
    println!("public URL: {base}");
    println!(
        "Slack Events URL (with Mervyn listening on {upstream}): {base}/slack/events"
    );

    if let Ok(s) = std::env::var("MERVYN_CHECK_NGROK_SECONDS") {
        let secs: u64 = s.parse().context("MERVYN_CHECK_NGROK_SECONDS must be a u64")?;
        println!("MERVYN_CHECK_NGROK_SECONDS={secs}: exiting after {secs}s (tunnel closes).");
        sleep(Duration::from_secs(secs)).await;
        return Ok(());
    }

    println!("Tunnel stays up until Ctrl+C.");
    tokio::signal::ctrl_c().await.context("wait for Ctrl+C")?;
    Ok(())
}
