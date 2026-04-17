//! Mark open todos done in `redb` using Claude to map natural language → ids.

use std::collections::HashSet;

use chrono::Utc;

use crate::claude::payloads::{CompleteTodoReplyV1, TodoDoneCandidateV1};
use crate::claude::prompts;
use crate::intent::normalize_claude_json_block;
use crate::state::AppState;
use crate::storage::todos::{self, TodoItem};
use crate::user_situation;

/// Split on commas and ASCII whitespace; trim light punctuation from each piece.
fn tokenize_command_tokens(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for c in text.chars() {
        if c.is_whitespace() || c == ',' {
            if !cur.is_empty() {
                let t = cur
                    .trim_matches(|ch: char| ".,;:!".contains(ch))
                    .to_string();
                if !t.is_empty() {
                    out.push(t);
                }
                cur.clear();
            }
        } else {
            cur.push(c);
        }
    }
    if !cur.is_empty() {
        let t = cur
            .trim_matches(|ch: char| ".,;:!".contains(ch))
            .to_string();
        if !t.is_empty() {
            out.push(t);
        }
    }
    out
}

/// When the message is only list positions plus completion filler (e.g. `7, 11 done`),
/// map 1-based positions → database ids. Otherwise `None` so Claude can interpret.
fn try_ids_from_strict_list_command(text: &str, list: &[TodoItem]) -> Option<Vec<u64>> {
    const SKIP: &[&str] = &[
        "done",
        "complete",
        "completed",
        "finished",
        "mark",
        "tick",
        "checkbox",
        "todo",
        "todos",
        "and",
    ];
    let alpha_tokens: Vec<String> = text
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    let has_completion_word = alpha_tokens.iter().any(|t| {
        matches!(
            t.as_str(),
            "done" | "complete" | "completed" | "finished" | "mark" | "tick" | "checkbox" | "todo" | "todos"
        )
    });
    if !has_completion_word {
        return None;
    }
    let tokens = tokenize_command_tokens(text);
    if tokens.is_empty() {
        return None;
    }
    let mut positions = Vec::new();
    for t in tokens {
        let tl = t.to_lowercase();
        if SKIP.iter().any(|k| *k == tl.as_str()) {
            continue;
        }
        let n: u32 = t.parse().ok()?;
        if n == 0 || n as usize > list.len() {
            return None;
        }
        positions.push(n);
    }
    if positions.is_empty() {
        return None;
    }
    let mut seen = HashSet::new();
    let mut ids = Vec::new();
    for n in positions {
        if seen.insert(n) {
            ids.push(list[n as usize - 1].id);
        }
    }
    Some(ids)
}

fn parse_complete_reply(raw: &str) -> anyhow::Result<Vec<u64>> {
    let trimmed = raw.trim();
    let try_parse = |s: &str| -> Option<Vec<u64>> {
        serde_json::from_str::<CompleteTodoReplyV1>(s)
            .ok()
            .map(|r| r.todo_ids_to_complete)
    };
    if let Some(v) = try_parse(trimmed) {
        return Ok(v);
    }
    let unfenced = normalize_claude_json_block(trimmed);
    if unfenced != trimmed {
        if let Some(v) = try_parse(&unfenced) {
            return Ok(v);
        }
    }
    anyhow::bail!("could not parse complete_todo model reply as JSON")
}

pub async fn run(state: &AppState, text: &str) -> anyhow::Result<String> {
    let list = todos::list_open(state.db.as_ref(), 80).map_err(|e| anyhow::anyhow!(e))?;
    if list.is_empty() {
        return Ok("You have no open todos in the database — nothing to mark done.".into());
    }

    let candidates: Vec<TodoDoneCandidateV1> = list
        .iter()
        .enumerate()
        .map(|(i, t)| TodoDoneCandidateV1 {
            id: t.id,
            list_number: i as u32 + 1,
            body: t.body.clone(),
            created_rfc3339: t.created_at.to_rfc3339(),
        })
        .collect();

    let to_complete: Vec<u64> = if let Some(ids) = try_ids_from_strict_list_command(text, &list) {
        tracing::debug!(?ids, "complete_todo: resolved list positions without model");
        ids
    } else {
        let user = prompts::complete_todo_user_json(text.trim(), &candidates)?;
        let now = Utc::now();
        let situation = user_situation::load_for_prompts(state.settings.as_ref());
        let sys_base = prompts::system_prompt_json(now, situation)?;
        let system = format!("{sys_base}\n\n{}", prompts::SUPPLEMENT_COMPLETE_TODO);
        let reply = state
            .claude
            .complete(Some(&system), &user)
            .await
            .map_err(|e| anyhow::anyhow!(e))?;

        parse_complete_reply(&reply).unwrap_or_else(|e| {
            tracing::warn!(error = %e, raw = %reply, "complete_todo: bad JSON from model");
            Vec::new()
        })
    };

    let allowed: HashSet<u64> = list.iter().map(|t| t.id).collect();
    let mut completed: Vec<String> = Vec::new();
    for id in to_complete {
        if !allowed.contains(&id) {
            tracing::warn!(id, "complete_todo: model returned id not in candidate set");
            continue;
        }
        if let Some(t) = list.iter().find(|x| x.id == id) {
            let body = t.body.clone();
            if todos::mark_done(state.db.as_ref(), id).map_err(|e| anyhow::anyhow!(e))? {
                completed.push(body);
            }
        }
    }

    if completed.is_empty() {
        return Ok(
            "No todos were marked done. Try naming the task or the numeric id, e.g. “mark todo 2 complete” or “done with the plumber”."
                .into(),
        );
    }

    let summary = completed.join("; ");
    Ok(if completed.len() == 1 {
        format!("Marked done: {summary}")
    } else {
        format!("Marked {} todos done: {summary}", completed.len())
    })
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use crate::storage::TodoItem;

    use super::{parse_complete_reply, try_ids_from_strict_list_command};

    fn todo_row(id: u64, body: &str) -> TodoItem {
        TodoItem {
            id,
            body: body.into(),
            created_at: Utc::now(),
            done: false,
        }
    }

    /// List order does not match database ids (the bug case: "7" must mean row 7, not `id` 7).
    fn sample_open_list() -> Vec<TodoItem> {
        vec![
            todo_row(101, "one"),
            todo_row(55, "two"),
            todo_row(3, "three"),
            todo_row(404, "four"),
            todo_row(12, "five"),
            todo_row(6, "six — Komorebi"),
            todo_row(999, "seven — Brendon"),
            todo_row(8, "eight"),
            todo_row(9, "nine — OCI"),
            todo_row(77, "ten"),
            todo_row(11, "eleven — Facebook"),
            todo_row(200, "twelve"),
        ]
    }

    #[test]
    fn strict_list_command_maps_positions_not_db_ids() {
        let list = sample_open_list();
        assert_eq!(
            try_ids_from_strict_list_command("7, 11 done", &list).unwrap(),
            vec![999, 11]
        );
    }

    #[test]
    fn strict_list_command_done_prefix_and_and() {
        let list = sample_open_list();
        assert_eq!(
            try_ids_from_strict_list_command("done 1 and 3", &list).unwrap(),
            vec![101, 3]
        );
    }

    #[test]
    fn strict_list_command_rejects_out_of_range() {
        let list = sample_open_list();
        assert!(try_ids_from_strict_list_command("7, 99 done", &list).is_none());
    }

    #[test]
    fn strict_list_command_rejects_prose() {
        let list = sample_open_list();
        assert!(try_ids_from_strict_list_command(
            "Finally finished seven and eleven",
            &list
        )
        .is_none());
    }

    #[test]
    fn strict_list_command_requires_completion_word() {
        let list = sample_open_list();
        assert!(try_ids_from_strict_list_command("7, 11", &list).is_none());
    }

    #[test]
    fn parse_complete_reply_json() {
        let raw = r#"{"api_version":1,"todo_ids_to_complete":[1,3]}"#;
        assert_eq!(parse_complete_reply(raw).unwrap(), vec![1, 3]);
    }

    #[test]
    fn parse_complete_reply_fenced() {
        let raw = "```json\n{\"todo_ids_to_complete\":[7]}\n```";
        assert_eq!(parse_complete_reply(raw).unwrap(), vec![7]);
    }
}
