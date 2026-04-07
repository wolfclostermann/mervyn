use chrono::Utc;

use crate::claude::prompts;
use crate::context::ContextAssembler;
use crate::state::AppState;
use crate::user_situation;

pub async fn run(state: &AppState, text: &str) -> anyhow::Result<String> {
    let situation = user_situation::load_for_prompts(state.settings.as_ref());
    let asm = ContextAssembler::new(
        state.db.clone(),
        state.vault_path.clone(),
        state.settings.worklog_md_git_mirror_path(),
    );
    let ctx = asm
        .build_query_context(Utc::now(), situation.as_deref())
        .await?;
    let now = Utc::now();
    let sys = prompts::system_prompt_json(now, situation)?;
    let system = format!("{sys}\n\n{}", prompts::SUPPLEMENT_FREEFORM_QUERY);
    let user = prompts::freeform_query_user_json(&ctx, text)?;
    let answer = state
        .claude
        .complete(Some(&system), &user)
        .await?;
    Ok(answer)
}
