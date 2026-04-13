//! Turn a free-form “note” into one or more todo lines: Claude extraction plus a small heuristic fallback.

use chrono::Utc;

use crate::claude::client::ClaudeClient;
use crate::claude::payloads::TodoItemsReplyV1;
use crate::claude::prompts;
use crate::intent::normalize_claude_json_block;
use crate::intent::slack_clean::{collapse_whitespace, strip_slack_mentions};

const MAX_ITEMS: usize = 15;
const MAX_ITEM_CHARS: usize = 400;

const NOTE_PREFIXES: &[&str] = &[
    "make a note that ",
    "please make a note that ",
    "note that ",
    "please note that ",
    "remind me that ",
    "remind me to ",
];

const JOINT_PAT: &str = " and i need to ";

fn strip_note_prefixes(s: &str) -> String {
    let mut t = s.trim().to_string();
    loop {
        let lower = t.to_lowercase();
        let Some(p) = NOTE_PREFIXES.iter().find(|p| lower.starts_with(*p)) else {
            break;
        };
        let n = p.chars().count();
        t = t
            .char_indices()
            .nth(n)
            .map(|(i, _)| t[i..].trim_start())
            .unwrap_or("")
            .to_string();
    }
    t
}

fn strip_leading_i_need_to(s: &str) -> String {
    let lower = s.trim().to_lowercase();
    const P: &str = "i need to ";
    let t = if lower.starts_with(P) {
        s.trim()
            .char_indices()
            .nth(P.chars().count())
            .map(|(i, _)| s.trim()[i..].trim_start())
            .unwrap_or("")
    } else {
        s.trim()
    };
    t.to_string()
}

fn split_joint_needs(s: &str) -> Vec<String> {
    let lower = s.to_lowercase();
    let mut out = Vec::new();
    let mut start = 0usize;
    while let Some(rel) = lower[start..].find(JOINT_PAT) {
        let abs = start + rel;
        let piece = s[start..abs].trim();
        if !piece.is_empty() {
            out.push(piece.to_string());
        }
        start = abs + JOINT_PAT.len();
    }
    let tail = s[start..].trim();
    if !tail.is_empty() {
        out.push(tail.to_string());
    }
    out
}

fn normalize_item_list(raw: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::<String>::new();
    let mut out = Vec::new();
    for mut s in raw {
        s = collapse_whitespace(&s);
        s = strip_leading_i_need_to(&s);
        s = s.trim().to_string();
        if s.is_empty() {
            continue;
        }
        let mut c = s.chars();
        s = match c.next() {
            None => continue,
            Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        };
        if s.chars().count() > MAX_ITEM_CHARS {
            s = s.chars().take(MAX_ITEM_CHARS).collect();
        }
        let key = s.to_lowercase();
        if seen.insert(key) {
            out.push(s);
        }
        if out.len() >= MAX_ITEMS {
            break;
        }
    }
    out
}

/// Fallback when Claude is unavailable or returns nothing usable.
pub(crate) fn heuristic_todo_items(raw: &str) -> Vec<String> {
    let cleaned = collapse_whitespace(&strip_slack_mentions(raw.trim()));
    if cleaned.is_empty() {
        return vec![];
    }
    let body = strip_note_prefixes(&cleaned);
    let chunks = split_joint_needs(&body);
    let chunks = if chunks.is_empty() {
        vec![body]
    } else {
        chunks
    };
    normalize_item_list(chunks)
}

fn parse_todo_items_reply(raw: &str) -> Option<Vec<String>> {
    let try_parse = |s: &str| -> Option<Vec<String>> {
        serde_json::from_str::<TodoItemsReplyV1>(s).ok().map(|r| {
            r.items
                .into_iter()
                .map(|x| x.trim().to_string())
                .filter(|x| !x.is_empty())
                .collect::<Vec<_>>()
        })
    };
    let trimmed = raw.trim();
    try_parse(trimmed).filter(|v| !v.is_empty()).or_else(|| {
        let unfenced = normalize_claude_json_block(trimmed);
        if unfenced != trimmed {
            try_parse(&unfenced).filter(|v| !v.is_empty())
        } else {
            None
        }
    })
}

pub async fn resolve_todo_items(
    claude: &ClaudeClient,
    raw: &str,
    situation: Option<String>,
) -> Vec<String> {
    let user = match prompts::todo_items_extraction_user_json(raw) {
        Ok(u) => u,
        Err(_) => return heuristic_todo_items(raw),
    };
    let now = Utc::now();
    let Ok(sys_base) = prompts::system_prompt_json(now, situation) else {
        return heuristic_todo_items(raw);
    };
    let system = format!(
        "{sys_base}\n\n{}",
        prompts::SUPPLEMENT_TODO_ITEMS_EXTRACTION
    );
    match claude.complete(Some(&system), &user).await {
        Ok(reply) => {
            let parsed = parse_todo_items_reply(&reply).unwrap_or_default();
            let norm = normalize_item_list(parsed);
            if norm.is_empty() {
                heuristic_todo_items(raw)
            } else {
                norm
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, "todo items extraction: Claude failed; using heuristic");
            heuristic_todo_items(raw)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heuristic_splits_joint_need() {
        let v = heuristic_todo_items(
            "Make a note that I need to arrange a plumber and I need to pay the gardener",
        );
        assert_eq!(
            v,
            vec!["Arrange a plumber".to_string(), "Pay the gardener".to_string()]
        );
    }

    #[test]
    fn parse_items_json() {
        let raw = r#"{"api_version":1,"items":["Alpha","Beta"]}"#;
        assert_eq!(
            parse_todo_items_reply(raw),
            Some(vec!["Alpha".into(), "Beta".into()])
        );
    }
}
