//! Slack Events API payloads and request signature verification.
//! See <https://api.slack.com/authentication/verifying-requests-from-slack>

use std::time::{SystemTime, UNIX_EPOCH};

use axum::http::HeaderMap;
use hmac::{Hmac, Mac};
use serde::Deserialize;
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// Slack retry counter when present (`1`, `2`, …). Logged in ingest; dedupe uses `event_id`.
pub fn parse_retry_num(headers: &HeaderMap) -> Option<u32> {
    headers
        .get("x-slack-retry-num")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse().ok())
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SlackEnvelope {
    UrlVerification {
        challenge: String,
    },
    EventCallback {
        event: SlackEvent,
        /// Unique per delivery; used for idempotent handling when Slack retries the same event.
        #[serde(default)]
        event_id: Option<String>,
    },
}

#[derive(Debug, Deserialize)]
pub struct SlackEvent {
    #[serde(rename = "type")]
    pub kind: String,
    pub channel: Option<String>,
    pub text: Option<String>,
    pub ts: Option<String>,
    pub thread_ts: Option<String>,
    pub bot_id: Option<String>,
    pub subtype: Option<String>,
}

pub fn verify_slack_signature(
    signing_secret: &[u8],
    headers: &HeaderMap,
    raw_body: &[u8],
) -> Result<(), &'static str> {
    let ts = headers
        .get("x-slack-request-timestamp")
        .and_then(|v| v.to_str().ok())
        .ok_or("missing X-Slack-Request-Timestamp")?;
    let sig = headers
        .get("x-slack-signature")
        .and_then(|v| v.to_str().ok())
        .ok_or("missing X-Slack-Signature")?;

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "clock error")?
        .as_secs() as i64;
    let slack_ts: i64 = ts.parse().map_err(|_| "bad timestamp")?;
    if (now - slack_ts).abs() > 60 * 5 {
        return Err("timestamp too old");
    }

    let mut mac = HmacSha256::new_from_slice(signing_secret).map_err(|_| "bad secret len")?;
    mac.update(b"v0:");
    mac.update(ts.as_bytes());
    mac.update(b":");
    mac.update(raw_body);
    let digest = mac.finalize().into_bytes();

    let expected = sig.strip_prefix("v0=").ok_or("bad signature prefix")?;
    let expected_bytes = hex::decode(expected).map_err(|_| "bad signature hex")?;
    if expected_bytes.len() != digest.len()
        || !constant_time_eq::constant_time_eq(&expected_bytes, digest.as_slice())
    {
        return Err("signature mismatch");
    }
    Ok(())
}
