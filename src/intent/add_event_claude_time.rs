use chrono::{DateTime, Utc};

use crate::claude::client::ClaudeClient;
use crate::claude::payloads::EventTimeReplyV1;
use crate::claude::prompts;
use crate::intent::normalize_claude_json_block;

fn parse_claude_time_reply(raw: &str) -> Option<(DateTime<Utc>, Option<DateTime<Utc>>)> {
    let try_parse = |s: &str| -> Option<(DateTime<Utc>, Option<DateTime<Utc>>)> {
        let r: EventTimeReplyV1 = serde_json::from_str(s).ok()?;
        let sstr = r.start_utc.as_deref()?.trim();
        if sstr.is_empty() {
            return None;
        }
        let start = DateTime::parse_from_rfc3339(sstr)
            .ok()?
            .with_timezone(&Utc);
        let end = r
            .end_utc
            .as_deref()
            .map(str::trim)
            .and_then(|e| {
                if e.is_empty() {
                    None
                } else {
                    DateTime::parse_from_rfc3339(e)
                        .ok()
                        .map(|dt| dt.with_timezone(&Utc))
                }
            });
        Some((start, end))
    };
    let trimmed = raw.trim();
    try_parse(trimmed).or_else(|| {
        let unfenced = normalize_claude_json_block(trimmed);
        if unfenced != trimmed {
            try_parse(&unfenced)
        } else {
            None
        }
    })
}

/// Infer start/end in UTC from natural language when the heuristic parser is unsure.
pub async fn resolve_add_event_time_via_claude(
    claude: &ClaudeClient,
    raw: &str,
    situation: Option<String>,
    interpret_in_timezone: &str,
) -> anyhow::Result<(DateTime<Utc>, Option<DateTime<Utc>>)> {
    let user = prompts::event_time_extraction_user_json(raw, interpret_in_timezone)
        .map_err(|e| anyhow::anyhow!(e))?;
    let now = Utc::now();
    let sys_base = prompts::system_prompt_json(now, situation)
        .map_err(|e| anyhow::anyhow!(e))?;
    let system = format!(
        "{}\n\n{}",
        sys_base,
        prompts::SUPPLEMENT_EVENT_TIME_EXTRACTION
    );
    let reply = claude.complete(Some(&system), &user).await?;
    parse_claude_time_reply(&reply)
        .ok_or_else(|| anyhow::anyhow!(
            "Could not infer a clear time from your message. Add an explicit day and time, or a phrase like 'tomorrow at 3'."
        ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_openai_style_json() {
        let (s, e) = parse_claude_time_reply(
            r#"{"start_utc": "2026-05-08T10:50:00+00:00", "end_utc": null}"#,
        )
        .expect("ok");
        assert_eq!(
            s,
            DateTime::parse_from_rfc3339("2026-05-08T10:50:00Z")
                .unwrap()
                .with_timezone(&Utc)
        );
        assert!(e.is_none());
    }
}
