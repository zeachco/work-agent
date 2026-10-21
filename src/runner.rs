use crate::config::{ResolvedTask, Runner};
use anyhow::{bail, Context, Result};
use serde_json::Value;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;

/// Runs one task to completion and returns a short human-readable outcome.
pub async fn run(task: &ResolvedTask, prompt: &str) -> Result<String> {
    match task.runner {
        Runner::Herdr => run_herdr(task, prompt).await,
        Runner::Process => run_process(task, prompt).await,
    }
}

/// Spawns a herdr workspace rooted at the task cwd, starts the interactive
/// harness in it, submits the prompt and waits for the agent to settle.
async fn run_herdr(task: &ResolvedTask, prompt: &str) -> Result<String> {
    let ws = herdr_json(&[
        "workspace", "create",
        "--cwd", &task.cwd.to_string_lossy(),
        "--label", &format!("cron:{}", task.name),
        "--no-focus",
    ])
    .await
    .context("herdr workspace create")?;
    let workspace_id = pick(&ws, &["result", "workspace", "workspace_id"])
        .or_else(|| pick(&ws, &["result", "workspace", "id"]))
        .context("workspace id missing in herdr response")?;
    let pane_id = pick(&ws, &["result", "root_pane", "pane_id"])
        .or_else(|| pick(&ws, &["result", "root_pane", "id"]))
        .context("root pane id missing in herdr response")?;
    tracing::info!(task = %task.name, %workspace_id, %pane_id, "herdr workspace created");

    let mut start = vec![
        "agent".to_string(), "start".into(), task.name.clone(),
        "--kind".into(), task.harness.clone(),
        "--pane".into(), pane_id.clone(),
    ];
    if !task.args.is_empty() {
        start.push("--".into());
        start.extend(task.args.iter().cloned());
    }
    let start_refs: Vec<&str> = start.iter().map(String::as_str).collect();
    herdr_json(&start_refs).await.context("herdr agent start")?;

    let timeout_ms = (task.timeout_seconds * 1000).to_string();
    let result = herdr_json(&[
        "agent", "prompt", &task.name, prompt,
        "--wait", "--timeout", &timeout_ms,
    ])
    .await;

    let outcome = match &result {
        Ok(v) => pick(v, &["result", "status"])
            .or_else(|| pick(v, &["result", "state"]))
            .unwrap_or_else(|| "settled".into()),
        Err(e) => format!("error: {e:#}"),
    };

    if task.close_workspace_on_done {
        if let Err(e) = herdr_json(&["workspace", "close", &workspace_id]).await {
            tracing::warn!(task = %task.name, "failed to close workspace: {e:#}");
        }
    }
    result.map(|_| outcome)
}

/// Fallback when herdr is not wanted: run the harness non-interactively.
async fn run_process(task: &ResolvedTask, prompt: &str) -> Result<String> {
    let mut cmd = Command::new(&task.harness);
    cmd.current_dir(&task.cwd)
        .args(&task.args)
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    match task.harness.as_str() {
        "pi" | "claude" => { cmd.arg("-p").arg(prompt); }
        _ => { cmd.arg(prompt); }
    }
    let child = cmd.spawn().with_context(|| format!("spawning `{}`", task.harness))?;
    let status = tokio::time::timeout(Duration::from_secs(task.timeout_seconds), child.wait_with_output())
        .await
        .context("task timed out")??
        .status;
    if !status.success() {
        bail!("`{}` exited with {status}", task.harness);
    }
    Ok(format!("exit {status}"))
}

async fn herdr_json(args: &[&str]) -> Result<Value> {
    tracing::debug!(?args, "herdr");
    let out = Command::new("herdr")
        .args(args)
        .stdin(Stdio::null())
        .output()
        .await
        .context("spawning herdr (is it installed and is a session running?)")?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        bail!("herdr {} failed: {}", args.join(" "), stderr.trim().to_string() + stdout.trim());
    }
    Ok(serde_json::from_str(stdout.trim()).unwrap_or(Value::Null))
}

fn pick(v: &Value, path: &[&str]) -> Option<String> {
    let mut cur = v;
    for key in path {
        cur = cur.get(key)?;
    }
    match cur {
        Value::String(s) => Some(s.clone()),
        Value::Null => None,
        other => Some(other.to_string()),
    }
}
