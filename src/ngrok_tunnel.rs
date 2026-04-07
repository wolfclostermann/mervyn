use std::sync::Once;

use anyhow::Context;
use ngrok::config::ForwarderBuilder;
use ngrok::prelude::EndpointInfo;
use ngrok::session::Session;

use crate::config::NgrokSection;

pub type HttpForwarder = ngrok::forwarder::Forwarder<ngrok::tunnel::HttpTunnel>;

static RUSTLS_PROVIDER: Once = Once::new();

fn ensure_rustls_aws_lc() {
    RUSTLS_PROVIDER.call_once(|| {
        rustls::crypto::aws_lc_rs::default_provider()
            .install_default()
            .expect("install rustls aws_lc_rs provider (required for ngrok)");
    });
}

pub async fn start(ngrok_cfg: &NgrokSection, local_port: u16) -> anyhow::Result<Option<HttpForwarder>> {
    if !ngrok_cfg.enabled {
        return Ok(None);
    }

    ensure_rustls_aws_lc();

    let session = Session::builder()
        .authtoken_from_env()
        .connect()
        .await
        .context("connect ngrok session (set NGROK_AUTHTOKEN)")?;

    let mut endpoint = session.http_endpoint();
    if let Some(domain) = ngrok_cfg.domain.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        endpoint.domain(domain);
    }

    let upstream = format!("http://127.0.0.1:{local_port}");
    let forwarder = endpoint
        .listen_and_forward(
            upstream
                .parse()
                .with_context(|| format!("parse ngrok upstream URL from {upstream}"))?,
        )
        .await
        .context("start ngrok HTTP endpoint")?;

    tracing::info!(
        public_url = %forwarder.url(),
        upstream = %upstream,
        "ngrok tunnel started"
    );
    tracing::info!(
        slack_events_url = %format!("{}/slack/events", forwarder.url().trim_end_matches('/')),
        "use this URL for Slack Events API"
    );

    Ok(Some(forwarder))
}
