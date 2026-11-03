mod config;
mod exception;
mod herdr;
mod runner;
mod schedule;
mod state;
mod template;
mod workstation;

use anyhow::{Context, Result};
use chrono::Local;
use clap::{Parser, Subcommand};
use config::{Config, ResolvedTask};
use state::Store;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;
use tokio::sync::Mutex;
use tracing::{error, info, warn};

#[derive(Parser)]
#[command(name = "work-agent", version, about = "Scheduled agent-task orchestrator")]
struct Cli {
    /// Path to `config.toml`; tasks are read from `tasks/*.toml` next to it.
    #[arg(short, long, default_value = "config.toml", env = "CRONS_CONFIG")]
    config: PathBuf,
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run forever, cycling through due tasks (default).
    Daemon,
    /// Print each task with its next due time and exit.
    List,
    /// Pick a task with fzf (previewing its file) and open it in $VISUAL/$EDITOR, or nvim.
    Show,
    /// Run one task immediately, ignoring its schedule and exception rule.
    /// This is the only way to start a task that has no `schedule` (manual task).
    /// Without a name, lists every runnable task instead.
    Run { name: Option<String> },
    /// Validate the config and exit.
    Check,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    let cli = Cli::parse();
    let cfg = Config::load(&cli.config)?;

    match cli.cmd.unwrap_or(Cmd::Daemon) {
        Cmd::Check => {
            println!("ok: {} task(s)", cfg.tasks.len());
        }
        Cmd::List => print_schedule(&cfg)?,
        Cmd::Show => show_task(&cfg)?,
        Cmd::Run { name: None } => print_runnable(&cfg)?,
        Cmd::Run { name: Some(name) } => {
            let task = cfg
                .resolved_tasks()
                .into_iter()
                .find(|t| t.name == name)
                .ok_or_else(|| anyhow::anyhow!("no enabled task named `{name}`"))?;
            let store = Arc::new(Mutex::new(Store::load(&cfg.state_file)?));
            execute(task, &cfg, store).await;
        }
        Cmd::Daemon => daemon(cfg, cli.config).await?,
    }
    Ok(())
}

/// `list`: every task with its schedule, last/next run and whether it is due now.
fn print_schedule(cfg: &Config) -> Result<()> {
    let store = Store::load(&cfg.state_file)?;
    let now = Local::now();
    for t in cfg.resolved_tasks() {
        let last = store.last_run(&t.name);
        let (due, next) = match &t.schedule {
            Some(s) => (schedule::is_due(s, last, now)?, schedule::next_after(s, last.unwrap_or(now))?),
            None => (false, None),
        };
        println!(
            "{:<24} {:<16} harness={:<7} parallel={:<5} last={} next={} {}",
            t.name,
            t.schedule.as_deref().unwrap_or("manual"),
            t.harness,
            t.parallel,
            last.map(|d| d.to_rfc3339()).unwrap_or_else(|| "never".into()),
            next.map(|d| d.to_rfc3339()).unwrap_or_else(|| "-".into()),
            if due { "DUE" } else { "" }
        );
    }
    Ok(())
}

/// `show`: fzf over every task file (including disabled ones) with the file as
/// preview; the picked file opens in `$VISUAL`, else `$EDITOR`, else `nvim`.
fn show_task(cfg: &Config) -> Result<()> {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let dir = &cfg.tasks_dir;
    let names: Vec<String> = cfg.tasks.iter().map(|t| t.name.clone()).collect();
    anyhow::ensure!(!names.is_empty(), "no task files under {}", dir.display());
    let preview = format!("cat {}/{{}}.toml", shell_quote(&dir.display().to_string()));
    let mut fzf = Command::new("fzf")
        .args(["--prompt", "task> ", "--preview", &preview, "--preview-window", "right:60%:wrap"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .context("spawning fzf (is it installed?)")?;
    fzf.stdin.take().unwrap().write_all((names.join("\n") + "\n").as_bytes())?;
    let out = fzf.wait_with_output()?;
    let picked = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if picked.is_empty() {
        return Ok(()); // cancelled
    }
    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .ok()
        .filter(|e| !e.trim().is_empty())
        .unwrap_or_else(|| "nvim".into());
    // $EDITOR may carry flags ("code -w"), so split it on whitespace.
    let mut parts = editor.split_whitespace();
    let bin = parts.next().unwrap();
    let status = Command::new(bin)
        .args(parts)
        .arg(dir.join(format!("{picked}.toml")))
        .status()
        .with_context(|| format!("launching editor `{editor}`"))?;
    anyhow::ensure!(status.success(), "editor `{editor}` exited with {status}");
    Ok(())
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// `run` with no name: the picker table for manual runs.
fn print_runnable(cfg: &Config) -> Result<()> {
    let store = Store::load(&cfg.state_file)?;
    let now = Local::now();
    println!("{:<24} {:<20} {:<20} {:<28} {}", "TASK", "LAST RUN", "NEXT RUN", "HARNESS/MODEL", "DESCRIPTION");
    for t in cfg.resolved_tasks() {
        let last = store.last_run(&t.name);
        let next = match &t.schedule {
            Some(s) => schedule::next_after(s, last.unwrap_or(now))?,
            None => None,
        };
        let fmt = |d: Option<chrono::DateTime<Local>>| {
            d.map(|d| d.format("%Y-%m-%d %H:%M").to_string()).unwrap_or_else(|| "never".into())
        };
        println!(
            "{:<24} {:<20} {:<20} {:<28} {}",
            t.name,
            fmt(last),
            fmt(next),
            format!("{}/{}", t.harness, t.model().unwrap_or("-")),
            t.description.as_deref().unwrap_or(""),
        );
    }
    Ok(())
}

async fn daemon(cfg: Config, path: PathBuf) -> Result<()> {
    let store = Arc::new(Mutex::new(Store::load(&cfg.state_file)?));
    let mut cfg = Arc::new(cfg);
    let mut last_modified = modified_at(&path);
    info!(tasks = cfg.tasks.len(), tick = cfg.tick_seconds, "work-agent daemon started");
    let mut ticker = tokio::time::interval(std::time::Duration::from_secs(cfg.tick_seconds));
    loop {
        ticker.tick().await;
        if let Some(fresh) = reload_if_changed(&path, &mut last_modified) {
            cfg = Arc::new(fresh);
            ticker = tokio::time::interval(std::time::Duration::from_secs(cfg.tick_seconds));
            ticker.tick().await; // first tick of a new interval fires immediately
        }
        // One full pass over the tasks, in config order. Serial tasks block the
        // pass; `parallel: true` tasks are detached and the pass moves on.
        for task in cfg.resolved_tasks() {
            // Manual tasks (no schedule) are never due here; only `run <name>` starts them.
            let Some(expr) = task.schedule.as_deref() else { continue };
            let due = {
                let s = store.lock().await;
                match schedule::is_due(expr, s.last_run(&task.name), Local::now()) {
                    Ok(d) => d,
                    Err(e) => { error!(task = %task.name, "schedule error: {e:#}"); false }
                }
            };
            if !due {
                continue;
            }
            if task.parallel {
                let (cfg, store) = (cfg.clone(), store.clone());
                tokio::spawn(async move { execute(task, &cfg, store).await });
            } else {
                execute(task, &cfg, store.clone()).await;
            }
        }
    }
}

/// Picks up edits to `config.toml` and any file added, removed or changed
/// under `tasks/`. A broken config keeps the last good one (returns None);
/// tasks already running keep the config they started with.
fn reload_if_changed(path: &Path, last_modified: &mut Fingerprint) -> Option<Config> {
    let modified = modified_at(path);
    if modified == *last_modified {
        return None;
    }
    *last_modified = modified;
    match Config::load(path) {
        Ok(fresh) => {
            info!(tasks = fresh.tasks.len(), "config reloaded");
            Some(fresh)
        }
        Err(e) => {
            error!("config reload failed, keeping previous: {e:#}");
            None
        }
    }
}

/// Every source file with its modification time; `None` for an unreadable one.
type Fingerprint = Vec<(PathBuf, Option<SystemTime>)>;

fn modified_at(config: &Path) -> Fingerprint {
    Config::source_files(config)
        .into_iter()
        .map(|p| {
            let m = std::fs::metadata(&p).and_then(|m| m.modified()).ok();
            (p, m)
        })
        .collect()
}

/// Evaluates the exception rule, runs the task and records the outcome.
async fn execute(task: ResolvedTask, cfg: &Config, store: Arc<Mutex<Store>>) {
    let started = Local::now();
    if let Some(rule) = &task.exception {
        let rendered = template::render(rule, &task.name, &task.workstation, &cfg.holidays);
        match exception::should_skip(&cfg.exception_llm, &rendered).await {
            Ok(true) => {
                info!(task = %task.name, "skipped by exception rule");
                let _ = store.lock().await.record(&task.name, |s| {
                    s.last_run = Some(started);
                    s.last_skipped = Some(started);
                    s.last_outcome = Some("skipped".into());
                });
                return;
            }
            Ok(false) => {}
            Err(e) => warn!(task = %task.name, "exception check failed, running anyway: {e:#}"),
        }
    }
    let body = template::render(&task.prompt, &task.name, &task.workstation, &cfg.holidays);
    let prompt = match workstation::prepare(&task.workstation) {
        Ok(()) => workstation::with_preamble(&body, &task.workstation),
        Err(e) => {
            warn!(task = %task.name, "workstation unavailable, prompting without it: {e:#}");
            body
        }
    };
    info!(task = %task.name, harness = %task.harness, cwd = %task.cwd.display(), "running");
    // Mark the start first so a crash mid-run does not retrigger the task on restart.
    let _ = store.lock().await.record(&task.name, |s| {
        s.last_run = Some(started);
        s.last_outcome = Some("running".into());
    });
    let outcome = match runner::run(&task, &prompt).await {
        Ok(o) => { info!(task = %task.name, %o, "done"); o }
        Err(e) => { error!(task = %task.name, "failed: {e:#}"); format!("error: {e:#}") }
    };
    let _ = store.lock().await.record(&task.name, |s| s.last_outcome = Some(outcome));
}
