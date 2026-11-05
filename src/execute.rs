//! One task run: exception check, prompt build, cwd check, state marks and
//! the runner call.
use crate::config::{Config, ResolvedTask};
use crate::state::{Store, TaskState};
use crate::{exception, runner, template, workstation};
use chrono::Local;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{error, info, warn};

/// Evaluates the exception rule, runs the task and records the outcome.
pub async fn execute(task: ResolvedTask, cfg: &Config, store: Arc<Mutex<Store>>) {
    let started = Local::now();
    if skipped_by_exception(&task, cfg).await {
        record(&store, &task.name, |s| {
            s.last_run = Some(started);
            s.last_skipped = Some(started);
            s.last_outcome = Some("skipped".into());
        })
        .await;
        return;
    }
    let prompt = build_prompt(&task, cfg);
    // The default cwd is the workstation; command tasks skip `prepare` above, so make sure it exists.
    if task.cwd == task.workstation {
        if let Err(e) = std::fs::create_dir_all(&task.cwd) {
            warn!(task = %task.name, "cannot create workstation cwd {}: {e}", task.cwd.display());
        }
    } else if !task.cwd.is_dir() {
        error!(task = %task.name, "cwd {} does not exist, refusing to run", task.cwd.display());
        record(&store, &task.name, |s| {
            s.last_run = Some(started);
            s.last_outcome = Some("failed: cwd missing".into());
        })
        .await;
        return;
    }
    info!(task = %task.name, harness = %task.harness, cwd = %task.cwd.display(), "running");
    // Mark the start first so a crash mid-run does not retrigger the task on restart.
    record(&store, &task.name, |s| {
        s.last_run = Some(started);
        s.last_outcome = Some("running".into());
    })
    .await;
    let outcome = match runner::run(&task, &prompt).await {
        Ok(o) => { info!(task = %task.name, %o, "done"); o }
        Err(e) => { error!(task = %task.name, "failed: {e:#}"); format!("error: {e:#}") }
    };
    record(&store, &task.name, |s| s.last_outcome = Some(outcome)).await;
}

/// Asks the exception LLM whether the task's `exception` rule says to skip
/// this run. No rule, or a failed check, means run.
async fn skipped_by_exception(task: &ResolvedTask, cfg: &Config) -> bool {
    let Some(rule) = &task.exception else { return false };
    let rendered = template::render(rule, &task.name, &task.workstation, &cfg.holidays);
    match exception::should_skip(&cfg.exception_llm, &rendered).await {
        Ok(true) => {
            info!(task = %task.name, "skipped by exception rule");
            true
        }
        Ok(false) => false,
        Err(e) => {
            warn!(task = %task.name, "exception check failed, running anyway: {e:#}");
            false
        }
    }
}

/// The rendered prompt with the workstation preamble when the workstation
/// could be prepared. Command tasks carry no prompt; the workstation note
/// only matters to agents.
fn build_prompt(task: &ResolvedTask, cfg: &Config) -> String {
    let Some(raw) = &task.prompt else { return String::new() };
    let body = template::render(raw, &task.name, &task.workstation, &cfg.holidays);
    match workstation::prepare(&task.workstation) {
        Ok(()) => workstation::with_preamble(&body, &task.workstation),
        Err(e) => {
            warn!(task = %task.name, "workstation unavailable, prompting without it: {e:#}");
            body
        }
    }
}

/// Updates one task's persisted state. A failed write is deliberately ignored:
/// the run itself must not fail because state.json could not be saved.
async fn record(store: &Mutex<Store>, task: &str, f: impl FnOnce(&mut TaskState)) {
    let _ = store.lock().await.record(task, f);
}
