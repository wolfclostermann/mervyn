use reqwest::header::{AUTHORIZATION, CONTENT_TYPE};
use serde::{Deserialize, Serialize};

#[derive(Clone)]
pub struct SlackClient {
    http: reqwest::Client,
    bot_token: String,
}

#[derive(Serialize)]
struct PostMessageBody<'a> {
    channel: &'a str,
    text: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    thread_ts: Option<&'a str>,
}

#[derive(Debug, Deserialize)]
struct PostMessageResponse {
    ok: bool,
    #[serde(default)]
    error: Option<String>,
}

impl SlackClient {
    pub fn new(bot_token: String) -> Self {
        Self {
            http: reqwest::Client::new(),
            bot_token,
        }
    }

    /// `thread_ts`: parent message timestamp to reply in a thread (Slack `thread_ts` API field).
    pub async fn post_message(
        &self,
        channel: &str,
        text: &str,
        thread_ts: Option<&str>,
    ) -> anyhow::Result<()> {
        let body = PostMessageBody {
            channel,
            text,
            thread_ts,
        };
        let resp = self
            .http
            .post("https://slack.com/api/chat.postMessage")
            .header(AUTHORIZATION, format!("Bearer {}", self.bot_token))
            .header(CONTENT_TYPE, "application/json; charset=utf-8")
            .json(&body)
            .send()
            .await?
            .error_for_status()?;
        let parsed: PostMessageResponse = resp.json().await?;
        if !parsed.ok {
            anyhow::bail!(
                "slack chat.postMessage: {}",
                parsed.error.as_deref().unwrap_or("unknown")
            );
        }
        Ok(())
    }
}
