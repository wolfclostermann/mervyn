use chrono::Utc;

use crate::claude::prompts;
use crate::context::ContextAssembler;
use crate::state::AppState;

pub async fn run(state: &AppState, text: &str) -> anyhow::Result<String> {
    let asm = ContextAssembler::new(state.db.clone(), state.vault_path.clone());
    let ctx = asm.build_query_context(Utc::now()).await?;
    let now = Utc::now();
    let sys = prompts::system_prompt_json(now)?;
    let system = format!("{sys}\n\n{}", prompts::SUPPLEMENT_FREEFORM_QUERY);
    let user = prompts::freeform_query_user_json(&ctx, text)?;
    let answer = state
        .claude
        .complete(Some(&system), &user)
        .await?;
    Ok(answer)
}
