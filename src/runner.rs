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

/// Runs the task inside herdr. Reuses the agent named after the task if it
/// is still alive, otherwise reuses the `cron:<name>` workspace if it is still
/// open, and only creates a fresh workspace when neither exists.
async fn run_herdr(task: &ResolvedTask, prompt: &str) -> Result<String> {
    if task.fresh_workspace {
        let stamp = chrono::Local::now();
        let label = format!("{}:{}", workspace_label(&task.name), stamp.format("%m-%d %H:%M"));
        let agent = format!("{}-{}", task.name, stamp.format("%m%d-%H%M"));
        let (ws, pane) = create_workspace(task, &label).await?;
        start_agent(task, &agent, &pane).await?;
        return prompt_and_finish(task, &agent, &ws, prompt).await;
    }
    let workspace_id = match find_agent(&task.name).await? {
        Some(agent) => {
            let status = pick(&agent, &["agent_status"]).unwrap_or_default();
            if status == "working" {
                bail!("agent `{}` is still working on a previous run", task.name);
            }
            let ws = pick(&agent, &["workspace_id"]).context("agent has no workspace_id")?;
            tracing::info!(task = %task.name, workspace_id = %ws, %status, "reusing running herdr agent");
            ws
        }
        None => {
            let (ws, pane) = match find_workspace(&task.name).await? {
                Some(ws) => {
                    let pane = shell_pane(&ws, task).await?;
                    tracing::info!(task = %task.name, workspace_id = %ws, pane_id = %pane, "reusing herdr workspace");
                    (ws, pane)
                }
                None => create_workspace(task, &workspace_label(&task.name)).await?,
            };
            start_agent(task, &task.name, &pane).await?;
            ws
        }
    };
    prompt_and_finish(task, &task.name, &workspace_id, prompt).await
}

async fn prompt_and_finish(task: &ResolvedTask, agent: &str, workspace_id: &str, prompt: &str) -> Result<String> {
    let timeout_ms = (task.timeout_seconds * 1000).to_string();
    let result = herdr_json(&[
        "agent", "prompt", agent, prompt,
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
        if let Err(e) = herdr_json(&["workspace", "close", workspace_id]).await {
            tracing::warn!(task = %task.name, "failed to close workspace: {e:#}");
        }
    }
    result.map(|_| outcome)
}

fn workspace_label(name: &str) -> String {
    format!("cron:{name}")
}

/// The agent registered under the task name, if any.
async fn find_agent(name: &str) -> Result<Option<Value>> {
    let v = herdr_json(&["agent", "list"]).await.context("herdr agent list")?;
    Ok(v.pointer("/result/agents")
        .and_then(Value::as_array)
        .and_then(|a| a.iter().find(|a| a.get("name").and_then(Value::as_str) == Some(name)).cloned()))
}

/// The workspace labelled `cron:<name>`, if it is still open.
async fn find_workspace(name: &str) -> Result<Option<String>> {
    let label = workspace_label(name);
    let v = herdr_json(&["workspace", "list"]).await.context("herdr workspace list")?;
    Ok(v.pointer("/result/workspaces")
        .and_then(Value::as_array)
        .and_then(|ws| ws.iter().find(|w| w.get("label").and_then(Value::as_str) == Some(&label)))
        .and_then(|w| pick(w, &["workspace_id"])))
}

/// A pane at a shell prompt inside `workspace_id`: the first pane without an
/// agent, or a fresh split when every pane is busy.
async fn shell_pane(workspace_id: &str, task: &ResolvedTask) -> Result<String> {
    let v = herdr_json(&["pane", "list", "--workspace", workspace_id]).await.context("herdr pane list")?;
    let panes = v.pointer("/result/panes").and_then(Value::as_array).cloned().unwrap_or_default();
    let free = panes.iter().find(|p| {
        let status = p.get("agent_status").and_then(Value::as_str).unwrap_or("unknown");
        status == "unknown" || status == "none"
    });
    if let Some(p) = free.and_then(|p| pick(p, &["pane_id"])) {
        return Ok(p);
    }
    let anchor = panes.first().and_then(|p| pick(p, &["pane_id"])).context("workspace has no panes")?;
    let split = herdr_json(&[
        "pane", "split", &anchor,
        "--direction", "down",
        "--cwd", &task.cwd.to_string_lossy(),
    ])
    .await
    .context("herdr pane split")?;
    pick(&split, &["result", "pane", "pane_id"])
        .or_else(|| pick(&split, &["result", "pane_id"]))
        .context("pane id missing in herdr split response")
}

async fn create_workspace(task: &ResolvedTask, label: &str) -> Result<(String, String)> {
    let ws = herdr_json(&[
        "workspace", "create",
        "--cwd", &task.cwd.to_string_lossy(),
        "--label", label,
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
    Ok((workspace_id, pane_id))
}

async fn start_agent(task: &ResolvedTask, agent: &str, pane_id: &str) -> Result<()> {
    let mut start = vec![
        "agent".to_string(), "start".into(), agent.to_string(),
        "--kind".into(), task.harness.clone(),
        "--pane".into(), pane_id.to_string(),
    ];
    if !task.args.is_empty() {
        start.push("--".into());
        start.extend(task.args.iter().cloned());
    }
    let start_refs: Vec<&str> = start.iter().map(String::as_str).collect();
    herdr_json(&start_refs).await.context("herdr agent start").map(|_| ())
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
