pub async fn run(_state: &crate::state::AppState, text: &str) -> anyhow::Result<String> {
    Ok(format!(
        "Noted (chat only). To persist in Mervyn’s database, ask me to **remember that for the morning briefing** (I’ll store it for the next cron briefing) or **log it as work**. For Obsidian, add under vault **notes/**: {}",
        text.trim()
    ))
}
