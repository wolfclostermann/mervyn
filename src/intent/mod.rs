pub mod add_event;
pub mod add_note;
pub mod add_reminder;
pub mod ask;
pub mod event_title;
pub mod log_work;
pub mod remember_briefing;
pub mod remove_event;
pub mod slack_clean;
pub mod text_datetime;

use chrono::Utc;

use crate::claude::payloads::IntentClassificationReplyV1;
use crate::claude::prompts;
use crate::state::AppState;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntentLabel {
    AddReminder,
    AddEvent,
    LogWork,
    AddNote,
    RemoveEvent,
    RememberBriefing,
    Ask,
}

fn intent_from_token(token: &str) -> Option<IntentLabel> {
    let token = token
        .trim()
        .trim_matches(|c: char| c == '`' || c == '.' || c == ',' || c == '"');
    match token.to_lowercase().as_str() {
        "add_reminder" => Some(IntentLabel::AddReminder),
        "add_event" => Some(IntentLabel::AddEvent),
        "log_work" => Some(IntentLabel::LogWork),
        "add_note" => Some(IntentLabel::AddNote),
        "remove_event" => Some(IntentLabel::RemoveEvent),
        "remember_briefing" => Some(IntentLabel::RememberBriefing),
        "ask" => Some(IntentLabel::Ask),
        _ => None,
    }
}

/// Legacy plain-text reply: first line, first token (e.g. `add_reminder` or prose starting with a label).
pub fn parse_intent_label(raw: &str) -> Option<IntentLabel> {
    let line = raw.trim().lines().next().unwrap_or("").trim();
    let token = line.split_whitespace().next().unwrap_or("");
    intent_from_token(token)
}

pub(crate) fn normalize_claude_json_block(raw: &str) -> String {
    let s = raw.trim();
    let Some(after_open) = s.strip_prefix("```") else {
        return s.to_string();
    };
    let mut rest = after_open;
    if let Some(after) = rest.strip_prefix("json") {
        rest = after;
    }
    rest = rest.trim_start_matches(|c: char| c == '\n' || c == '\r');
    if let Some(idx) = rest.rfind("```") {
        return rest[..idx].trim().to_string();
    }
    s.to_string()
}

/// Parse model output: JSON object [`IntentClassificationReplyV1`] first, then markdown-fenced JSON, then plain label.
pub fn parse_intent_reply(raw: &str) -> Option<IntentLabel> {
    let trimmed = raw.trim();
    if let Ok(r) = serde_json::from_str::<IntentClassificationReplyV1>(trimmed) {
        return intent_from_token(&r.intent);
    }
    let unfenced = normalize_claude_json_block(trimmed);
    if unfenced != trimmed {
        if let Ok(r) = serde_json::from_str::<IntentClassificationReplyV1>(&unfenced) {
            return intent_from_token(&r.intent);
        }
    }
    parse_intent_label(raw)
}

pub async fn classify_intent(
    claude: &crate::claude::client::ClaudeClient,
    message: &str,
    situation: Option<String>,
) -> anyhow::Result<IntentLabel> {
    let now = Utc::now();
    let sys = prompts::system_prompt_json(now, situation)?;
    let system = format!("{sys}\n\n{}", prompts::SUPPLEMENT_INTENT_CLASSIFICATION);
    let user_json = prompts::intent_classification_user_json(message)?;
    let label_text = claude.complete(Some(&system), &user_json).await?;
    parse_intent_reply(&label_text).ok_or_else(|| anyhow::anyhow!("unrecognised intent: {label_text:?}"))
}

pub async fn dispatch(
    state: &AppState,
    label: IntentLabel,
    user_text: &str,
    channel: &str,
    thread_parent_ts: Option<&str>,
    situation: Option<String>,
) -> anyhow::Result<()> {
    let reply = match label {
        IntentLabel::AddReminder => add_reminder::run(state, user_text).await?,
        IntentLabel::AddEvent => add_event::run(state, user_text, situation).await?,
        IntentLabel::LogWork => log_work::run(state, user_text).await?,
        IntentLabel::AddNote => add_note::run(state, user_text).await?,
        IntentLabel::RemoveEvent => remove_event::run(state, user_text).await?,
        IntentLabel::RememberBriefing => remember_briefing::run(state, user_text).await?,
        IntentLabel::Ask => ask::run(state, user_text).await?,
    };
    state
        .slack
        .post_message(channel, &reply, thread_parent_ts)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_intent_reply_json_object() {
        let j = r#"{"api_version":1,"intent":"log_work"}"#;
        assert_eq!(parse_intent_reply(j), Some(IntentLabel::LogWork));
    }

    #[test]
    fn parse_intent_reply_fenced_json() {
        let j = "```json\n{\"api_version\":1,\"intent\":\"add_note\"}\n```";
        assert_eq!(parse_intent_reply(j), Some(IntentLabel::AddNote));
    }

    #[test]
    fn parse_intent_reply_plain_label_fallback() {
        assert_eq!(parse_intent_reply("add_reminder"), Some(IntentLabel::AddReminder));
        assert_eq!(parse_intent_reply("`ask`"), Some(IntentLabel::Ask));
    }

    #[test]
    fn parse_intent_reply_remove_and_briefing() {
        let j = r#"{"api_version":1,"intent":"remove_event"}"#;
        assert_eq!(parse_intent_reply(j), Some(IntentLabel::RemoveEvent));
        let j2 = r#"{"api_version":1,"intent":"remember_briefing"}"#;
        assert_eq!(parse_intent_reply(j2), Some(IntentLabel::RememberBriefing));
    }
}
