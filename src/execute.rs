//! One task run: cwd check, the `preflight` gate, exception check, prompt
//! build, the runner call, the `validate` gate, and the state marks around it
//! all.
use crate::config::{Config, ResolvedTask};
use crate::state::{Store, TaskState, Verdict};
use crate::{exception, hooks, runner, template, workstation};
use chrono::Local;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{error, info, warn};

/// Whether to honour a task's `preflight`/`validate` gates. `Off` is for
/// `work-agent run --ignore-gates`, which runs the task however the gates
/// answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gates {
    All,
    Off,
}

/// Runs one task, gated and recorded: a failing `preflight` skips the run, a
/// failing `validate` fails it, and every outcome updates the task's streak.
pub async fn execute(task: ResolvedTask, cfg: &Config, store: Arc<Mutex<Store>>, gates: Gates) {
    let started = Local::now();
    // The default cwd is the workstation; command tasks skip `prepare`, so make
    // sure it exists. Gates run from here too, so it has to be settled first.
    if task.cwd == task.workstation {
        if let Err(e) = std::fs::create_dir_all(&task.cwd) {
            warn!(task = %task.name, "cannot create workstation cwd {}: {e}", task.cwd.display());
        }
    } else if !task.cwd.is_dir() {
        error!(task = %task.name, "cwd {} does not exist, refusing to run", task.cwd.display());
        record(&store, &task.name, |s| {
            s.mark_finished(started, Verdict::Failure, "failed: cwd missing")
        })
        .await;
        return;
    }
    // The gates come before the exception rule: a shell check is cheap and
    // deterministic, a local-LLM call is neither.
    if gates == Gates::All {
        if let Some(line) = &task.preflight {
            if let Err(why) = hooks::check("preflight", &task, &render_gate(&task, &cfg.holidays, line)).await {
                info!(task = %task.name, "{why}");
                record(&store, &task.name, |s| s.mark_skipped(started, format!("skipped: {why}"))).await;
                return;
            }
        }
    }
    if skipped_by_exception(&task, cfg).await {
        record(&store, &task.name, |s| s.mark_skipped(started, "skipped")).await;
        return;
    }
    let prompt = build_prompt(&task, cfg);
    info!(task = %task.name, harness = %task.harness, cwd = %task.cwd.display(), "running");
    // Mark the start first so a crash mid-run does not retrigger the task on restart.
    record(&store, &task.name, |s| s.mark_running(started)).await;
    let (mut verdict, mut outcome) = match runner::run(&task, &prompt).await {
        Ok(o) => { info!(task = %task.name, %o, "done"); (Verdict::Success, o) }
        Err(e) => { error!(task = %task.name, "failed: {e:#}"); (Verdict::Failure, format!("error: {e:#}")) }
    };
    // Only a run that looked good gets verified: when the task itself failed
    // there is nothing to check and its own error is the better message.
    if gates == Gates::All && verdict == Verdict::Success {
        if let Some(line) = &task.validate {
            if let Err(why) = hooks::check("validate", &task, &render_gate(&task, &cfg.holidays, line)).await {
                warn!(task = %task.name, "{why}");
                verdict = Verdict::Failure;
                outcome = format!("failed: {why}");
            }
        }
    }
    record(&store, &task.name, |s| s.mark_finished(started, verdict, outcome)).await;
}

/// A gate with its `${...}` tokens filled, exactly like a prompt: the same
/// placeholders work, so a task with its own `cwd` can still reach a script it
/// keeps in its workstation (`bash ${workstation}/check.sh`).
fn render_gate(task: &ResolvedTask, holidays: &[String], line: &str) -> String {
    template::render(line, &task.name, &task.workstation, holidays)
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
