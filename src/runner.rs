use crate::config::{ResolvedTask, Runner};
use crate::herdr::{create_workspace, find_agent, find_workspace, herdr_json, pick, shell_pane, start_agent, workspace_label};
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
        Runner::Command => run_command(task).await,
    }
}

/// Runs the task's `command` line directly in a herdr pane, with no agent in
/// the loop. The pane stays in the user's herdr session for the whole run, so
/// interactive prompts (e.g. `sudo`'s password) can be answered by hand.
/// Completion is detected when the pane's foreground process returns to its
/// shell; the run then counts as settled even though no agent reported it.
async fn run_command(task: &ResolvedTask) -> Result<String> {
    let command = task
        .command
        .as_ref()
        .context("command runner needs a `command` field")?;
    let (ws, pane) = if task.fresh_workspace {
        let stamp = chrono::Local::now().format("%m-%d %H:%M");
        create_workspace(task, &format!("{}:{}", workspace_label(&task.name), stamp)).await?
    } else {
        match find_workspace(&task.name).await? {
            Some(ws) => {
                let pane = shell_pane(&ws, task).await?;
                tracing::info!(task = %task.name, workspace_id = %ws, pane_id = %pane, "reusing herdr workspace");
                (ws, pane)
            }
            None => create_workspace(task, &workspace_label(&task.name)).await?,
        }
    };
    herdr_json(&["pane", "run", &pane, command])
        .await
        .context("herdr pane run")?;
    // Poll the pane's foreground process until it is the shell again. The
    // first check is delayed so the shell has parsed the line and the command
    // is actually in the foreground; an instant check would see the idle shell
    // and declare the run done.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(task.timeout_seconds);
    loop {
        tokio::time::sleep(Duration::from_secs(2)).await;
        let v = herdr_json(&["pane", "process-info", "--pane", &pane])
            .await
            .context("herdr pane process-info")?;
        let info = v.pointer("/result/process_info").context("process-info returned no data")?;
        let settled = match (
            info.get("foreground_process_group_id").and_then(Value::as_u64),
            info.get("shell_pid").and_then(Value::as_u64),
        ) {
            (Some(fg), Some(shell)) => fg == shell,
            _ => false,
        };
        if settled {
            break;
        }
        if tokio::time::Instant::now() >= deadline {
            bail!("command did not settle within {}s", task.timeout_seconds);
        }
    }
    if task.close_workspace_on_done {
        if let Err(e) = herdr_json(&["workspace", "close", &ws]).await {
            tracing::warn!(task = %task.name, "failed to close workspace: {e:#}");
        }
    }
    Ok("settled".into())
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
        None => start_agent_in_workspace(task).await?,
    };
    prompt_and_finish(task, &task.name, &workspace_id, prompt).await
}

/// Starts the task's agent in its `cron:<name>` workspace, reusing the
/// workspace when it is still open. Returns the workspace id.
async fn start_agent_in_workspace(task: &ResolvedTask) -> Result<String> {
    let (ws, pane) = match find_workspace(&task.name).await? {
        Some(ws) => {
            let pane = shell_pane(&ws, task).await?;
            tracing::info!(task = %task.name, workspace_id = %ws, pane_id = %pane, "reusing herdr workspace");
            (ws, pane)
        }
        None => create_workspace(task, &workspace_label(&task.name)).await?,
    };
    start_agent(task, &task.name, &pane).await?;
    Ok(ws)
}

async fn prompt_and_finish(task: &ResolvedTask, agent: &str, workspace_id: &str, prompt: &str) -> Result<String> {
    let timeout_ms = (task.timeout_seconds * 1000).to_string();
    let result = herdr_json(&[
        "agent", "prompt", agent, prompt,
        "--wait", "--timeout", &timeout_ms,
    ])
    .await;

    // Close before propagating a prompt failure so the workspace is not leaked.
    if task.close_workspace_on_done {
        if let Err(e) = herdr_json(&["workspace", "close", workspace_id]).await {
            tracing::warn!(task = %task.name, "failed to close workspace: {e:#}");
        }
    }
    let v = result?;
    Ok(pick(&v, &["result", "status"])
        .or_else(|| pick(&v, &["result", "state"]))
        .unwrap_or_else(|| "settled".into()))
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
