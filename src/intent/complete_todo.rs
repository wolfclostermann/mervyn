//! Mark open todos done in `redb` using Claude to map natural language → ids.

use std::collections::HashSet;

use chrono::Utc;

use crate::claude::payloads::{CompleteTodoReplyV1, TodoDoneCandidateV1};
use crate::claude::prompts;
use crate::intent::normalize_claude_json_block;
use crate::state::AppState;
use crate::storage::todos;
use crate::user_situation;

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
        .map(|t| TodoDoneCandidateV1 {
            id: t.id,
            body: t.body.clone(),
            created_rfc3339: t.created_at.to_rfc3339(),
        })
        .collect();

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

    let to_complete = parse_complete_reply(&reply).unwrap_or_else(|e| {
        tracing::warn!(error = %e, raw = %reply, "complete_todo: bad JSON from model");
        Vec::new()
    });

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
    #[test]
    fn parse_complete_reply_json() {
        let raw = r#"{"api_version":1,"todo_ids_to_complete":[1,3]}"#;
        assert_eq!(super::parse_complete_reply(raw).unwrap(), vec![1, 3]);
    }

    #[test]
    fn parse_complete_reply_fenced() {
        let raw = "```json\n{\"todo_ids_to_complete\":[7]}\n```";
        assert_eq!(super::parse_complete_reply(raw).unwrap(), vec![7]);
    }
}
