use chrono::Utc;

use crate::intent::todo_items::resolve_todo_items;
use crate::state::AppState;
use crate::storage::todos::{self, TodoItem};

pub async fn run(state: &AppState, text: &str, situation: Option<String>) -> anyhow::Result<String> {
    let items = resolve_todo_items(state.claude.as_ref(), text, situation).await;
    if items.is_empty() {
        return Ok("I didn’t catch a specific task — say what you need to do (one or more items).".into());
    }

    let mut lines = Vec::new();
    for body in items {
        let id = todos::next_id(state.db.as_ref()).map_err(|e| anyhow::anyhow!(e))?;
        let item = TodoItem {
            id,
            body,
            created_at: Utc::now(),
            done: false,
        };
        todos::put(state.db.as_ref(), &item).map_err(|e| anyhow::anyhow!(e))?;
        lines.push(format!("• {}", item.body));
    }

    let n = lines.len();
    Ok(format!(
        "Added {n} to your todo list (saved in Mervyn’s database). They’ll show up in morning briefing context and when you ask me questions.\n\n{}",
        lines.join("\n")
    ))
}
