pub mod add_event;
pub mod add_note;
pub mod add_reminder;
pub mod ask;
pub mod log_work;

use chrono::Utc;

use crate::claude::prompts;
use crate::state::AppState;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntentLabel {
    AddReminder,
    AddEvent,
    LogWork,
    AddNote,
    Ask,
}

pub fn parse_intent_label(raw: &str) -> Option<IntentLabel> {
    let line = raw.trim().lines().next().unwrap_or("").trim();
    let token = line
        .split_whitespace()
        .next()
        .unwrap_or("")
        .trim_matches(|c: char| c == '`' || c == '.' || c == ',' || c == '"');
    match token.to_lowercase().as_str() {
        "add_reminder" => Some(IntentLabel::AddReminder),
        "add_event" => Some(IntentLabel::AddEvent),
        "log_work" => Some(IntentLabel::LogWork),
        "add_note" => Some(IntentLabel::AddNote),
        "ask" => Some(IntentLabel::Ask),
        _ => None,
    }
}

pub async fn classify_intent(
    claude: &crate::claude::client::ClaudeClient,
    message: &str,
) -> anyhow::Result<IntentLabel> {
    let now = Utc::now();
    let sys = prompts::system_prompt_json(now)?;
    let system = format!("{sys}\n\n{}", prompts::SUPPLEMENT_INTENT_CLASSIFICATION);
    let user_json = prompts::intent_classification_user_json(message)?;
    let label_text = claude.complete(Some(&system), &user_json).await?;
    parse_intent_label(&label_text).ok_or_else(|| anyhow::anyhow!("unrecognised intent: {label_text:?}"))
}

pub async fn dispatch(
    state: &AppState,
    label: IntentLabel,
    user_text: &str,
    channel: &str,
    thread_parent_ts: Option<&str>,
) -> anyhow::Result<()> {
    let reply = match label {
        IntentLabel::AddReminder => add_reminder::run(state, user_text).await?,
        IntentLabel::AddEvent => add_event::run(state, user_text).await?,
        IntentLabel::LogWork => log_work::run(state, user_text).await?,
        IntentLabel::AddNote => add_note::run(state, user_text).await?,
        IntentLabel::Ask => ask::run(state, user_text).await?,
    };
    state
        .slack
        .post_message(channel, &reply, thread_parent_ts)
        .await?;
    Ok(())
}
