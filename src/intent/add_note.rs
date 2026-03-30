pub async fn run(_state: &crate::state::AppState, text: &str) -> anyhow::Result<String> {
    Ok(format!(
        "Noted (chat only). For a durable note, add it under the Obsidian vault notes/ folder: {}",
        text.trim()
    ))
}
