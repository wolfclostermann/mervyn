//! Calendar event titles: Claude extraction with a small heuristic fallback (no API / parse errors).

use chrono::{NaiveDate, Utc};

use crate::claude::client::ClaudeClient;
use crate::claude::payloads::EventTitleReplyV1;
use crate::claude::prompts;
use crate::intent::normalize_claude_json_block;
use crate::intent::slack_clean::{collapse_whitespace, strip_slack_mentions};
use crate::intent::text_datetime::strip_date_clause_after_on;

const LEADING_PREFIXES: &[&str] = &[
    "i have a ",
    "i have an ",
    "i have ",
    "i've got a ",
    "i've got an ",
    "i've got ",
    "i need a ",
    "i need an ",
    "i need ",
    "calendar: ",
    "event: ",
    "schedule ",
    "reminder: ",
];

fn strip_leading_prefix(s: &str) -> String {
    let lower = s.to_lowercase();
    for p in LEADING_PREFIXES {
        if lower.starts_with(p) {
            let n = p.chars().count();
            return s
                .char_indices()
                .nth(n)
                .map(|(i, _)| s[i..].trim_start())
                .unwrap_or("")
                .to_string();
        }
    }
    s.to_string()
}

fn parse_claude_title(raw: &str) -> Option<String> {
    let try_parse = |s: &str| -> Option<String> {
        serde_json::from_str::<EventTitleReplyV1>(s).ok().and_then(|r| {
            let t = r.title.trim();
            if t.is_empty() {
                None
            } else {
                Some(t.chars().take(200).collect())
            }
        })
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

/// Fallback when Claude is unavailable or returns an unusable reply.
pub(crate) fn heuristic_event_title(raw: &str, today: NaiveDate) -> String {
    let cleaned = collapse_whitespace(&strip_slack_mentions(raw.trim()));
    if cleaned.is_empty() {
        return "Event".to_string();
    }
    let mut s = strip_leading_prefix(&cleaned);
    s = strip_date_clause_after_on(&s, today);
    s = collapse_whitespace(&s);
    let s = s.trim().to_string();
    if s.is_empty() {
        let fb = collapse_whitespace(&strip_slack_mentions(raw.trim()));
        return fb.chars().take(120).collect::<String>().trim().to_string();
    }
    let mut c = s.chars();
    match c.next() {
        None => s,
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
    }
}

/// Best title for a new event: ask Claude first, then [`heuristic_event_title`].
pub async fn resolve_event_title(
    claude: &ClaudeClient,
    raw: &str,
    today: NaiveDate,
    situation: Option<String>,
) -> String {
    let user = match prompts::event_title_extraction_user_json(raw) {
        Ok(u) => u,
        Err(_) => return heuristic_event_title(raw, today),
    };
    let now = Utc::now();
    let Ok(sys_base) = prompts::system_prompt_json(now, situation) else {
        return heuristic_event_title(raw, today);
    };
    let system = format!(
        "{sys_base}\n\n{}",
        prompts::SUPPLEMENT_EVENT_TITLE_EXTRACTION
    );
    match claude.complete(Some(&system), &user).await {
        Ok(reply) => parse_claude_title(&reply).unwrap_or_else(|| heuristic_event_title(raw, today)),
        Err(e) => {
            tracing::warn!(error = %e, "event title extraction: Claude failed; using heuristic");
            heuristic_event_title(raw, today)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dentist_on_april_8th_heuristic() {
        let today = NaiveDate::from_ymd_opt(2026, 4, 5).unwrap();
        let t = heuristic_event_title(
            "<@U123> I have a dentist appointment on April 8th 9 til 11",
            today,
        );
        assert_eq!(t, "Dentist appointment");
    }

    #[test]
    fn parse_title_json() {
        let raw = r#"{"api_version":1,"title":"Team standup"}"#;
        assert_eq!(parse_claude_title(raw).as_deref(), Some("Team standup"));
    }

    #[test]
    fn parse_title_fenced_json() {
        let raw = "```json\n{\"api_version\":1,\"title\":\"Dentist\"}\n```";
        assert_eq!(parse_claude_title(raw).as_deref(), Some("Dentist"));
    }
}
