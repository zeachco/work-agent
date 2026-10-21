mod config;
mod exception;
mod runner;
mod schedule;
mod state;
mod template;

use anyhow::Result;
use chrono::Local;
use clap::{Parser, Subcommand};
use config::{Config, ResolvedTask};
use state::Store;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{error, info, warn};

#[derive(Parser)]
#[command(name = "crons", version, about = "Scheduled agent-task orchestrator")]
struct Cli {
    /// Path to the JSON config.
    #[arg(short, long, default_value = "crons.json", env = "CRONS_CONFIG")]
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
    /// Run one task immediately, ignoring its schedule and exception rule.
    Run { name: String },
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
        Cmd::List => {
            let store = Store::load(&cfg.state_file)?;
            let now = Local::now();
            for t in cfg.resolved_tasks() {
                let last = store.last_run(&t.name);
                let due = schedule::is_due(&t.schedule, last, now)?;
                let next = schedule::parse(&t.schedule)?.after(&last.unwrap_or(now)).next();
                println!(
                    "{:<24} {:<16} harness={:<7} parallel={:<5} last={} next={} {}",
                    t.name,
                    t.schedule,
                    t.harness,
                    t.parallel,
                    last.map(|d| d.to_rfc3339()).unwrap_or_else(|| "never".into()),
                    next.map(|d| d.to_rfc3339()).unwrap_or_else(|| "-".into()),
                    if due { "DUE" } else { "" }
                );
            }
        }
        Cmd::Run { name } => {
            let task = cfg
                .resolved_tasks()
                .into_iter()
                .find(|t| t.name == name)
                .ok_or_else(|| anyhow::anyhow!("no enabled task named `{name}`"))?;
            let store = Arc::new(Mutex::new(Store::load(&cfg.state_file)?));
            execute(task, &cfg, store).await;
        }
        Cmd::Daemon => daemon(cfg).await?,
    }
    Ok(())
}

async fn daemon(cfg: Config) -> Result<()> {
    let store = Arc::new(Mutex::new(Store::load(&cfg.state_file)?));
    let cfg = Arc::new(cfg);
    info!(tasks = cfg.tasks.len(), tick = cfg.tick_seconds, "crons daemon started");
    let mut ticker = tokio::time::interval(std::time::Duration::from_secs(cfg.tick_seconds));
    loop {
        ticker.tick().await;
        // One full pass over the tasks, in config order. Serial tasks block the
        // pass; `parallel: true` tasks are detached and the pass moves on.
        for task in cfg.resolved_tasks() {
            let due = {
                let s = store.lock().await;
                match schedule::is_due(&task.schedule, s.last_run(&task.name), Local::now()) {
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

/// Evaluates the exception rule, runs the task and records the outcome.
async fn execute(task: ResolvedTask, cfg: &Config, store: Arc<Mutex<Store>>) {
    let started = Local::now();
    if let Some(rule) = &task.exception {
        let rendered = template::render(rule, &task.name, &cfg.holidays);
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
    let prompt = template::render(&task.prompt, &task.name, &cfg.holidays);
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
