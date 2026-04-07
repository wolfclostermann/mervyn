//! One-shot: load `.env` + `config/default.toml` + `MERVYN__*` and call the Messages API once.

use anyhow::Context;

const API_VERSION: &str = "2023-06-01";
const MESSAGES_URL: &str = "https://api.anthropic.com/v1/messages";

#[derive(serde::Serialize)]
struct MessagesRequest<'a> {
    model: &'a str,
    max_tokens: u32,
    messages: Vec<RequestMessage<'a>>,
}

#[derive(serde::Serialize)]
struct RequestMessage<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(serde::Deserialize)]
struct MessagesResponse {
    content: Vec<ResponseContentBlock>,
}

#[derive(serde::Deserialize)]
struct ResponseContentBlock {
    #[serde(rename = "type")]
    kind: String,
    text: Option<String>,
}

#[derive(serde::Deserialize)]
struct ApiErrorEnvelope {
    error: ApiErrorDetail,
}

#[derive(serde::Deserialize)]
struct ApiErrorDetail {
    message: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();

    let settings = config::Config::builder()
        .add_source(config::File::with_name("config/default"))
        .add_source(
            config::Environment::with_prefix("MERVYN")
                .separator("__")
                .try_parsing(true),
        )
        .build()
        .context("load config")?;

    let model: String = settings.get_string("claude.model").context("claude.model")?;
    let api_key =
        std::env::var("ANTHROPIC_API_KEY").context("ANTHROPIC_API_KEY not set in environment")?;

    let body = MessagesRequest {
        model: &model,
        max_tokens: 16,
        messages: vec![RequestMessage {
            role: "user",
            content: "Reply with exactly the word: ok",
        }],
    };

    let client = reqwest::Client::new();
    let res = client
        .post(MESSAGES_URL)
        .header("x-api-key", &api_key)
        .header("anthropic-version", API_VERSION)
        .json(&body)
        .send()
        .await
        .context("anthropic HTTP request")?;

    let status = res.status();
    let bytes = res.bytes().await.context("read response body")?;

    if !status.is_success() {
        let msg = serde_json::from_slice::<ApiErrorEnvelope>(&bytes)
            .map(|e| e.error.message)
            .unwrap_or_else(|_| String::from_utf8_lossy(&bytes).into_owned());
        anyhow::bail!("anthropic API {}: {}", status, msg);
    }

    let parsed: MessagesResponse = serde_json::from_slice(&bytes).context("decode JSON")?;
    let text = parsed
        .content
        .into_iter()
        .filter(|b| b.kind == "text")
        .filter_map(|b| b.text)
        .collect::<Vec<_>>()
        .join("");

    println!("Anthropic API key OK (model={model}).");
    println!("Sample reply: {text}");
    Ok(())
}
