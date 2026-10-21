use crate::config::ExceptionLlm;
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::time::Duration;

const SYSTEM: &str = "You decide whether a scheduled automation should be skipped right now. \
Read the rule and the context, then answer with exactly one word: SKIP if the task must not run now, RUN otherwise.";

/// Returns `true` when the local model says the run should be dropped.
/// Any failure to reach the model is logged and treated as "run" so a dead LLM never silences the scheduler.
pub async fn should_skip(llm: &ExceptionLlm, rendered_rule: &str) -> Result<bool> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(llm.timeout_seconds))
        .build()?;
    let body = json!({
        "model": llm.model,
        "temperature": 0,
        "max_tokens": 8,
        "messages": [
            {"role": "system", "content": SYSTEM},
            {"role": "user", "content": format!("{rendered_rule}\n\nAnswer SKIP or RUN.")}
        ]
    });
    let resp: Value = client
        .post(format!("{}/chat/completions", llm.base_url.trim_end_matches('/')))
        .json(&body)
        .send()
        .await
        .context("calling exception LLM")?
        .error_for_status()?
        .json()
        .await?;
    let answer = resp["choices"][0]["message"]["content"]
        .as_str()
        .unwrap_or("")
        .trim()
        .to_uppercase();
    tracing::debug!(%answer, "exception llm answered");
    Ok(answer.starts_with("SKIP"))
}
