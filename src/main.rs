mod config;
mod exception;
mod herdr;
mod runner;
mod schedule;
mod service;
mod state;
mod template;
mod workstation;

use anyhow::{Context, Result};
use chrono::Local;
use clap::{Parser, Subcommand};
use config::{Config, ResolvedTask};
use state::{Store, TaskState};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;
use tokio::sync::Mutex;
use tracing::{error, info, warn};

#[derive(Parser)]
#[command(name = "work-agent", version, about = "Scheduled agent-task orchestrator")]
struct Cli {
    /// Profile folder: `config.toml` (optional), `tasks/`, `workstations/` and
    /// `state.json` live there, outside this repo, so a profile can be its own
    /// (private) repository and one checkout can hold several of them.
    #[arg(short, long, env = "WORK_AGENT_PROFILE", default_value_os_t = default_profile())]
    profile: PathBuf,
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
    /// Install the daemon as a systemd user service, one per profile path given
    /// (`--uninstall` / `--status` to remove or inspect them).
    Service(service::ServiceArgs),
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    let cli = Cli::parse();
    if let Some(Cmd::Service(args)) = cli.cmd {
        return service::run(args);
    }
    let cfg = Config::load(&cli.profile)?;

    match cli.cmd.unwrap_or(Cmd::Daemon) {
        Cmd::Check => {
            let mut missing = 0;
            for t in cfg.resolved_tasks() {
                if t.cwd != t.workstation && !t.cwd.is_dir() {
                    eprintln!("warning: task `{}` cwd {} does not exist", t.name, t.cwd.display());
                    missing += 1;
                }
            }
            println!("ok: {} task(s) in profile {}", cfg.tasks.len(), cfg.profile.display());
            if missing > 0 {
                anyhow::bail!("{missing} task(s) point at a missing cwd");
            }
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
        Cmd::Daemon => daemon(cfg, cli.profile).await?,
        Cmd::Service(_) => unreachable!("handled before loading the profile"),
    }
    Ok(())
}

/// `$XDG_CONFIG_HOME/work-agent/default`, the profile used when none is given.
fn default_profile() -> PathBuf {
    let config_home = config::user_config_home().unwrap_or_else(|| PathBuf::from("."));
    config_home.join("work-agent").join("default")
}

type When = Option<chrono::DateTime<Local>>;

/// Every enabled task with its last and next run, plus the `now` they were computed against.
fn task_rows(cfg: &Config) -> Result<(chrono::DateTime<Local>, Vec<(ResolvedTask, When, When)>)> {
    let store = Store::load(&cfg.state_file)?;
    let now = Local::now();
    let mut rows = Vec::new();
    for t in cfg.resolved_tasks() {
        let last = store.last_run(&t.name);
        let next = match &t.schedule {
            Some(s) => schedule::next_after(s, last.unwrap_or(now))?,
            None => None,
        };
        rows.push((t, last, next));
    }
    Ok((now, rows))
}

/// `list`: every task with its schedule, last/next run and whether it is due now.
fn print_schedule(cfg: &Config) -> Result<()> {
    let (now, rows) = task_rows(cfg)?;
    for (t, last, next) in rows {
        let due = match &t.schedule {
            Some(s) => schedule::is_due(s, last, now)?,
            None => false,
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
    let (_, rows) = task_rows(cfg)?;
    println!("{:<24} {:<20} {:<20} {:<28} {}", "TASK", "LAST RUN", "NEXT RUN", "HARNESS/MODEL", "DESCRIPTION");
    for (t, last, next) in rows {
        let fmt = |d: When| {
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

async fn daemon(cfg: Config, profile: PathBuf) -> Result<()> {
    let store = Arc::new(Mutex::new(Store::load(&cfg.state_file)?));
    let mut cfg = Arc::new(cfg);
    let mut last_modified = modified_at(&profile);
    info!(profile = %cfg.profile.display(), tasks = cfg.tasks.len(), tick = cfg.tick_seconds, "work-agent daemon started");
    let mut ticker = tokio::time::interval(std::time::Duration::from_secs(cfg.tick_seconds));
    loop {
        ticker.tick().await;
        if let Some(fresh) = reload_if_changed(&profile, &mut last_modified) {
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

/// Picks up edits to the profile's `config.toml` and any file added, removed
/// or changed under its `tasks/`. A broken config keeps the last good one
/// (returns None); tasks already running keep the config they started with.
fn reload_if_changed(profile: &Path, last_modified: &mut Fingerprint) -> Option<Config> {
    let modified = modified_at(profile);
    if modified == *last_modified {
        return None;
    }
    *last_modified = modified;
    match Config::load(profile) {
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

fn modified_at(profile: &Path) -> Fingerprint {
    Config::source_files(profile)
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
