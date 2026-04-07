//! Delete upcoming calendar rows in `redb` using Claude to map natural language → ids.

use std::collections::HashSet;

use chrono::{Duration, Utc};

use crate::claude::payloads::{RemoveEventCandidateV1, RemoveEventReplyV1};
use crate::claude::prompts;
use crate::intent::normalize_claude_json_block;
use crate::state::AppState;
use crate::storage::events;
use crate::user_situation;

fn parse_remove_reply(raw: &str) -> anyhow::Result<Vec<u64>> {
    let trimmed = raw.trim();
    let try_parse = |s: &str| -> Option<Vec<u64>> {
        serde_json::from_str::<RemoveEventReplyV1>(s)
            .ok()
            .map(|r| r.event_ids_to_delete)
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
    anyhow::bail!("could not parse remove_event model reply as JSON")
}

pub async fn run(state: &AppState, text: &str) -> anyhow::Result<String> {
    let now = Utc::now();
    let until = now + Duration::days(60);
    let list = events::upcoming_within(state.db.as_ref(), now, until, 120)
        .map_err(|e| anyhow::anyhow!(e))?;
    if list.is_empty() {
        return Ok(
            "There are no upcoming events in the database in the next 60 days — nothing to remove."
                .into(),
        );
    }

    let candidates: Vec<RemoveEventCandidateV1> = list
        .iter()
        .map(|e| RemoveEventCandidateV1 {
            id: e.id,
            title: e.title.clone(),
            start_rfc3339: e.start.to_rfc3339(),
        })
        .collect();

    let user = prompts::remove_event_user_json(text.trim(), &candidates)?;
    let situation = user_situation::load_for_prompts(state.settings.as_ref());
    let sys_base = prompts::system_prompt_json(now, situation)?;
    let system = format!("{sys_base}\n\n{}", prompts::SUPPLEMENT_REMOVE_EVENT);
    let reply = state
        .claude
        .complete(Some(&system), &user)
        .await
        .map_err(|e| anyhow::anyhow!(e))?;

    let to_delete = parse_remove_reply(&reply).unwrap_or_else(|e| {
        tracing::warn!(error = %e, raw = %reply, "remove_event: bad JSON from model");
        Vec::new()
    });

    let allowed: HashSet<u64> = list.iter().map(|e| e.id).collect();
    let mut removed_titles: Vec<String> = Vec::new();
    for id in to_delete {
        if !allowed.contains(&id) {
            tracing::warn!(id, "remove_event: model returned id not in candidate set");
            continue;
        }
        if let Some(ev) = list.iter().find(|e| e.id == id) {
            let title = ev.title.clone();
            if events::delete(state.db.as_ref(), id).map_err(|e| anyhow::anyhow!(e))? {
                removed_titles.push(title);
            }
        }
    }

    if removed_titles.is_empty() {
        return Ok(
            "No events were deleted. Try naming the date or title to remove, e.g. “delete the dentist event on 6 April”."
                .into(),
        );
    }

    let summary = removed_titles.join("; ");
    Ok(if removed_titles.len() == 1 {
        format!("Removed from the database: {summary}")
    } else {
        format!(
            "Removed {} events from the database: {summary}",
            removed_titles.len()
        )
    })
}
