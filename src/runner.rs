use crate::config::{ResolvedTask, Runner};
use crate::herdr::{close_if_wanted, create_workspace, find_agent, find_workspace, herdr_json, pick, shell_pane, start_agent, workspace_label};
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
    let (ws, pane) = task_pane(task, chrono::Local::now()).await?;
    herdr_json(&["pane", "run", &pane, command])
        .await
        .context("herdr pane run")?;
    wait_until_settled(task, &pane).await?;
    close_if_wanted(task, &ws).await;
    Ok("settled".into())
}

/// How often a command pane is checked for completion. Also the delay before
/// the first check, so the shell has parsed the line and the command is
/// actually in the foreground; an instant check would see the idle shell and
/// declare the run done.
const SETTLE_POLL: Duration = Duration::from_secs(2);

/// Polls the pane until its foreground process is the shell again, or the
/// task's timeout elapses.
async fn wait_until_settled(task: &ResolvedTask, pane: &str) -> Result<()> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(task.timeout_seconds);
    loop {
        tokio::time::sleep(SETTLE_POLL).await;
        let v = herdr_json(&["pane", "process-info", "--pane", pane])
            .await
            .context("herdr pane process-info")?;
        let info = v.pointer("/result/process_info").context("process-info returned no data")?;
        let foreground = info.get("foreground_process_group_id").and_then(Value::as_u64);
        let shell = info.get("shell_pid").and_then(Value::as_u64);
        if foreground.zip(shell).is_some_and(|(fg, sh)| fg == sh) {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            bail!("command did not settle within {}s", task.timeout_seconds);
        }
    }
}

/// Finds or creates the pane a task runs in. With `fresh_workspace` it is a
/// new workspace labelled with `now`; otherwise the still-open `cron:<profile>/<name>`
/// workspace is reused, and a new one is created only when none is open.
/// Returns `(workspace_id, pane_id)`.
async fn task_pane(task: &ResolvedTask, now: chrono::DateTime<chrono::Local>) -> Result<(String, String)> {
    if task.fresh_workspace {
        let label = format!("{}:{}", workspace_label(task), now.format("%m-%d %H:%M"));
        return create_workspace(task, &label).await;
    }
    match find_workspace(task).await? {
        Some(ws) => {
            let pane = shell_pane(&ws, task).await?;
            tracing::info!(task = %task.name, workspace_id = %ws, pane_id = %pane, "reusing herdr workspace");
            Ok((ws, pane))
        }
        None => create_workspace(task, &workspace_label(task)).await,
    }
}

/// Runs the task inside herdr. Reuses the agent named after the task if it
/// is still alive, otherwise reuses the `cron:<profile>/<name>` workspace if it is still
/// open, and only creates a fresh workspace when neither exists.
async fn run_herdr(task: &ResolvedTask, prompt: &str) -> Result<String> {
    if task.fresh_workspace {
        let now = chrono::Local::now();
        let agent = format!("{}-{}", task.name, now.format("%m%d-%H%M"));
        let (ws, pane) = task_pane(task, now).await?;
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
            let (ws, pane) = task_pane(task, chrono::Local::now()).await?;
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

    // Close before propagating a prompt failure so the workspace is not leaked.
    close_if_wanted(task, workspace_id).await;
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
    // why: harness CLI contract — pi and claude take the prompt behind `-p`,
    // any other harness takes it as a bare positional argument.
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
