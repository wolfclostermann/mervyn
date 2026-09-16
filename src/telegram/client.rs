//! Telegram Bot API client: `getUpdates` (long polling) and `sendMessage`.
//!
//! Two behaviours the old Slack client did not have and this one needs:
//! * **Chunking.** Telegram caps `text` at 4096 characters; Slack's cap was ~40k, so long
//!   `Ask` answers that used to go out in one piece must now be split.
//! * **429 backoff.** Telegram's per-chat rate limit is roughly one message per second and it
//!   answers with `parameters.retry_after`. The reminder job posts serially in a loop, so a
//!   burst of due reminders will hit this.
//!
//! Replies are sent as plain text with **no `parse_mode`**: Claude's output is Markdown-ish and
//! appointment reminders embed curly quotes, all of which `MarkdownV2` would require escaping.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::updates::Update;

/// Telegram's documented `sendMessage` text cap, counted in UTF-16 code units.
pub const MAX_MESSAGE_UTF16: usize = 4096;

/// Give up after this many consecutive 429s for one chunk.
const MAX_RATE_LIMIT_RETRIES: u32 = 3;

/// Upper bound on a server-supplied `retry_after`, so a hostile or buggy value cannot wedge
/// the sender for hours.
const MAX_RETRY_AFTER: Duration = Duration::from_secs(60);

#[derive(Clone)]
pub struct TelegramClient {
    http: reqwest::Client,
    token: String,
}

#[derive(Serialize)]
struct SendMessageBody<'a> {
    chat_id: &'a str,
    text: &'a str,
}

#[derive(Serialize)]
struct GetUpdatesBody {
    #[serde(skip_serializing_if = "Option::is_none")]
    offset: Option<i64>,
    timeout: u64,
    allowed_updates: Vec<&'static str>,
}

#[derive(Debug, Deserialize)]
#[serde(bound = "T: serde::de::DeserializeOwned")]
struct ApiResponse<T> {
    ok: bool,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    error_code: Option<i64>,
    #[serde(default)]
    parameters: Option<ResponseParameters>,
    #[serde(default)]
    result: Option<T>,
}

#[derive(Debug, Default, Deserialize)]
struct ResponseParameters {
    #[serde(default)]
    retry_after: Option<u64>,
}

/// What to do with a `sendMessage` reply.
#[derive(Debug, PartialEq, Eq)]
enum SendOutcome {
    Ok,
    RateLimited(Duration),
    Failed(String),
}

/// Classify an API response without performing I/O, so the retry policy is testable.
fn classify<T>(resp: &ApiResponse<T>) -> SendOutcome {
    if resp.ok {
        return SendOutcome::Ok;
    }
    if resp.error_code == Some(429) {
        let secs = resp
            .parameters
            .as_ref()
            .and_then(|p| p.retry_after)
            .unwrap_or(1);
        let wait = Duration::from_secs(secs).min(MAX_RETRY_AFTER);
        return SendOutcome::RateLimited(wait);
    }
    SendOutcome::Failed(format!(
        "{} (error_code {})",
        resp.description.as_deref().unwrap_or("unknown"),
        resp.error_code.unwrap_or(0)
    ))
}

/// Split `text` into pieces that each fit within `limit` UTF-16 code units.
///
/// Breaks on a newline when one is available inside the piece, so paragraphs stay intact;
/// otherwise breaks at the last character boundary that fits. Never splits mid-character.
pub fn split_message(text: &str, limit: usize) -> Vec<String> {
    assert!(limit > 0, "limit must be positive");
    if text.is_empty() {
        return Vec::new();
    }
    if utf16_len(text) <= limit {
        return vec![text.to_string()];
    }

    let mut out = Vec::new();
    let mut rest = text;
    while !rest.is_empty() {
        if utf16_len(rest) <= limit {
            out.push(rest.to_string());
            break;
        }
        // Longest prefix of `rest` that fits the budget, respecting char boundaries.
        let mut used = 0usize;
        let mut end = 0usize;
        let mut last_newline: Option<usize> = None;
        for (idx, ch) in rest.char_indices() {
            let w = ch.len_utf16();
            if used + w > limit {
                break;
            }
            used += w;
            end = idx + ch.len_utf8();
            if ch == '\n' {
                last_newline = Some(end);
            }
        }
        // Prefer a newline break, but not one so early it wastes most of the message.
        let split_at = match last_newline {
            Some(nl) if nl * 2 >= end => nl,
            _ => end,
        };
        out.push(rest[..split_at].trim_end().to_string());
        rest = rest[split_at..].trim_start_matches('\n');
    }
    out.retain(|s| !s.is_empty());
    out
}

fn utf16_len(s: &str) -> usize {
    s.chars().map(char::len_utf16).sum()
}

impl TelegramClient {
    pub fn new(token: String) -> Self {
        Self {
            // Long polling holds a request open for `timeout` seconds; the client timeout must
            // exceed that or every poll aborts.
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(90))
                .build()
                .expect("build reqwest client"),
            token,
        }
    }

    /// The bot token lives in the URL path, so this string must never reach a log or an error.
    fn url(&self, method: &str) -> String {
        format!("https://api.telegram.org/bot{}/{}", self.token, method)
    }

    async fn call<B: Serialize, T: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        body: &B,
    ) -> anyhow::Result<ApiResponse<T>> {
        let resp = self
            .http
            .post(self.url(method))
            .json(body)
            .send()
            .await
            // `without_url` strips the bot token out of the error's Display.
            .map_err(|e| anyhow::anyhow!("telegram {method}: {}", e.without_url()))?;
        let parsed = resp
            .json::<ApiResponse<T>>()
            .await
            .map_err(|e| anyhow::anyhow!("telegram {method} decode: {}", e.without_url()))?;
        Ok(parsed)
    }

    /// Send `text` to `chat_id`, splitting it across messages when it exceeds Telegram's cap.
    pub async fn send_message(&self, chat_id: &str, text: &str) -> anyhow::Result<()> {
        for chunk in split_message(text, MAX_MESSAGE_UTF16) {
            self.send_chunk(chat_id, &chunk).await?;
        }
        Ok(())
    }

    async fn send_chunk(&self, chat_id: &str, text: &str) -> anyhow::Result<()> {
        let body = SendMessageBody { chat_id, text };
        for attempt in 0..=MAX_RATE_LIMIT_RETRIES {
            let resp: ApiResponse<serde::de::IgnoredAny> =
                self.call("sendMessage", &body).await?;
            match classify(&resp) {
                SendOutcome::Ok => return Ok(()),
                SendOutcome::RateLimited(wait) => {
                    if attempt == MAX_RATE_LIMIT_RETRIES {
                        anyhow::bail!(
                            "telegram sendMessage: rate limited after {} retries",
                            MAX_RATE_LIMIT_RETRIES
                        );
                    }
                    tracing::warn!(
                        wait_secs = wait.as_secs(),
                        attempt,
                        "telegram rate limited; backing off"
                    );
                    tokio::time::sleep(wait).await;
                }
                SendOutcome::Failed(msg) => anyhow::bail!("telegram sendMessage: {msg}"),
            }
        }
        unreachable!("loop returns or bails")
    }

    /// Long-poll for updates. Returns as soon as Telegram has something, or after `timeout_secs`.
    pub async fn get_updates(
        &self,
        offset: Option<i64>,
        timeout_secs: u64,
    ) -> anyhow::Result<Vec<Update>> {
        let body = GetUpdatesBody {
            offset,
            timeout: timeout_secs,
            allowed_updates: vec!["message"],
        };
        let resp: ApiResponse<Vec<Update>> = self.call("getUpdates", &body).await?;
        match classify(&resp) {
            SendOutcome::Ok => Ok(resp.result.unwrap_or_default()),
            SendOutcome::RateLimited(wait) => {
                tokio::time::sleep(wait).await;
                Ok(Vec::new())
            }
            SendOutcome::Failed(msg) => anyhow::bail!("telegram getUpdates: {msg}"),
        }
    }

    /// Confirm the token works and return the bot's username, for a startup log line.
    pub async fn get_me(&self) -> anyhow::Result<String> {
        #[derive(Deserialize)]
        struct Me {
            #[serde(default)]
            username: Option<String>,
        }
        let resp: ApiResponse<Me> = self.call("getMe", &serde_json::json!({})).await?;
        match classify(&resp) {
            SendOutcome::Ok => Ok(resp
                .result
                .and_then(|m| m.username)
                .unwrap_or_else(|| "unknown".to_string())),
            SendOutcome::RateLimited(_) => anyhow::bail!("telegram getMe: rate limited"),
            SendOutcome::Failed(msg) => anyhow::bail!("telegram getMe: {msg}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse<T: serde::de::DeserializeOwned>(s: &str) -> ApiResponse<T> {
        serde_json::from_str(s).unwrap()
    }

    #[test]
    fn short_text_is_one_chunk() {
        let out = split_message("hello", MAX_MESSAGE_UTF16);
        assert_eq!(out, vec!["hello".to_string()]);
    }

    #[test]
    fn empty_text_produces_no_chunks() {
        assert!(split_message("", MAX_MESSAGE_UTF16).is_empty());
    }

    #[test]
    fn text_exactly_at_limit_is_not_split() {
        let text = "a".repeat(MAX_MESSAGE_UTF16);
        assert_eq!(split_message(&text, MAX_MESSAGE_UTF16).len(), 1);
    }

    #[test]
    fn one_over_the_limit_splits_in_two() {
        let text = "a".repeat(MAX_MESSAGE_UTF16 + 1);
        let out = split_message(&text, MAX_MESSAGE_UTF16);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].chars().count(), MAX_MESSAGE_UTF16);
        assert_eq!(out[1].chars().count(), 1);
    }

    #[test]
    fn every_chunk_respects_the_budget() {
        let text = "paragraph text here\n".repeat(600);
        for chunk in split_message(&text, MAX_MESSAGE_UTF16) {
            assert!(utf16_len(&chunk) <= MAX_MESSAGE_UTF16);
        }
    }

    #[test]
    fn prefers_breaking_on_a_newline() {
        // Two lines that together exceed a small budget: the break should land on the newline.
        let text = format!("{}\n{}", "a".repeat(8), "b".repeat(8));
        let out = split_message(&text, 10);
        assert_eq!(out[0], "a".repeat(8));
        assert_eq!(out[1], "b".repeat(8));
    }

    #[test]
    fn never_splits_a_multibyte_char() {
        // 'é' is 2 bytes / 1 UTF-16 unit; a naive byte slice at an odd offset would panic.
        let text = "é".repeat(50);
        let out = split_message(&text, 10);
        assert!(out.len() > 1);
        let rejoined: String = out.concat();
        assert_eq!(rejoined.chars().count(), 50);
        for chunk in &out {
            assert!(chunk.chars().all(|c| c == 'é'));
        }
    }

    #[test]
    fn counts_astral_chars_as_two_utf16_units() {
        // Each emoji is 1 char but 2 UTF-16 units, so a budget of 10 fits only 5 of them.
        let text = "😀".repeat(20);
        let out = split_message(&text, 10);
        for chunk in &out {
            assert!(utf16_len(chunk) <= 10);
            assert_eq!(chunk.chars().count(), 5);
        }
        assert_eq!(out.concat().chars().count(), 20);
    }

    #[test]
    fn ok_response_classifies_ok() {
        let r: ApiResponse<serde::de::IgnoredAny> = parse(r#"{"ok":true,"result":{}}"#);
        assert_eq!(classify(&r), SendOutcome::Ok);
    }

    #[test]
    fn rate_limit_uses_retry_after() {
        let r: ApiResponse<serde::de::IgnoredAny> = parse(
            r#"{"ok":false,"error_code":429,"description":"Too Many Requests: retry after 7",
                "parameters":{"retry_after":7}}"#,
        );
        assert_eq!(classify(&r), SendOutcome::RateLimited(Duration::from_secs(7)));
    }

    #[test]
    fn rate_limit_without_parameters_defaults_to_one_second() {
        let r: ApiResponse<serde::de::IgnoredAny> =
            parse(r#"{"ok":false,"error_code":429,"description":"Too Many Requests"}"#);
        assert_eq!(classify(&r), SendOutcome::RateLimited(Duration::from_secs(1)));
    }

    #[test]
    fn absurd_retry_after_is_capped() {
        let r: ApiResponse<serde::de::IgnoredAny> = parse(
            r#"{"ok":false,"error_code":429,"parameters":{"retry_after":86400}}"#,
        );
        assert_eq!(classify(&r), SendOutcome::RateLimited(MAX_RETRY_AFTER));
    }

    #[test]
    fn other_errors_are_terminal_and_keep_the_description() {
        let r: ApiResponse<serde::de::IgnoredAny> = parse(
            r#"{"ok":false,"error_code":400,"description":"Bad Request: chat not found"}"#,
        );
        match classify(&r) {
            SendOutcome::Failed(msg) => {
                assert!(msg.contains("chat not found"), "got: {msg}");
                assert!(msg.contains("400"), "got: {msg}");
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }
}
