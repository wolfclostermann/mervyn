//! Persist a Slack request into the worklog with a tag consumed by the morning briefing assembler.

use chrono::Utc;

use crate::intent::slack_clean::{collapse_whitespace, strip_slack_mentions};
use crate::state::AppState;
use crate::storage::worklog::{self, WorklogEntry};

pub(crate) const WORKLOG_TAG: &str = "briefing";

pub async fn run(state: &AppState, text: &str) -> anyhow::Result<String> {
    let body = collapse_whitespace(&strip_slack_mentions(text.trim()));
    if body.is_empty() {
        return Ok("Say what you want called out in the next briefing.".into());
    }
    let id = worklog::next_id(state.db.as_ref()).map_err(|e| anyhow::anyhow!(e))?;
    let e = WorklogEntry {
        id,
        timestamp: Utc::now(),
        body,
        tags: vec![WORKLOG_TAG.to_string()],
        project: None,
    };
    worklog::put(state.db.as_ref(), &e).map_err(|e| anyhow::anyhow!(e))?;
    Ok(
        "Saved for your next morning briefing (stored in Mervyn’s database). I’ll pull it from the queued-for-briefing section when the cron runs."
            .into(),
    )
}
