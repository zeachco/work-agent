//! Thin client over the `herdr` CLI: JSON calls plus the lookups the runner
//! needs (agent by name, workspace by label, a free pane, workspace/agent
//! creation).
use crate::config::ResolvedTask;
use anyhow::{bail, Context, Result};
use serde_json::Value;
use std::process::Stdio;
use tokio::process::Command;

pub(crate) fn workspace_label(name: &str) -> String {
    format!("cron:{name}")
}

/// The agent registered under the task name, if any.
pub(crate) async fn find_agent(name: &str) -> Result<Option<Value>> {
    let v = herdr_json(&["agent", "list"]).await.context("herdr agent list")?;
    Ok(v.pointer("/result/agents")
        .and_then(Value::as_array)
        .and_then(|a| a.iter().find(|a| a.get("name").and_then(Value::as_str) == Some(name)).cloned()))
}

/// The workspace labelled `cron:<name>`, if it is still open.
pub(crate) async fn find_workspace(name: &str) -> Result<Option<String>> {
    let label = workspace_label(name);
    let v = herdr_json(&["workspace", "list"]).await.context("herdr workspace list")?;
    Ok(v.pointer("/result/workspaces")
        .and_then(Value::as_array)
        .and_then(|ws| ws.iter().find(|w| w.get("label").and_then(Value::as_str) == Some(&label)))
        .and_then(|w| pick(w, &["workspace_id"])))
}

/// A pane at a shell prompt inside `workspace_id`: the first pane without an
/// agent, or a fresh split when every pane is busy.
pub(crate) async fn shell_pane(workspace_id: &str, task: &ResolvedTask) -> Result<String> {
    let v = herdr_json(&["pane", "list", "--workspace", workspace_id]).await.context("herdr pane list")?;
    let panes: &[Value] = v.pointer("/result/panes").and_then(Value::as_array).map_or(&[], Vec::as_slice);
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

pub(crate) async fn create_workspace(task: &ResolvedTask, label: &str) -> Result<(String, String)> {
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

pub(crate) async fn start_agent(task: &ResolvedTask, agent: &str, pane_id: &str) -> Result<()> {
    let mut argv = vec!["agent", "start", agent, "--kind", &task.harness, "--pane", pane_id];
    if !task.args.is_empty() {
        argv.push("--");
        argv.extend(task.args.iter().map(String::as_str));
    }
    herdr_json(&argv).await.context("herdr agent start").map(|_| ())
}

pub(crate) async fn herdr_json(args: &[&str]) -> Result<Value> {
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

pub(crate) fn pick(v: &Value, path: &[&str]) -> Option<String> {
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
