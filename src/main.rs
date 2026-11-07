mod config;
mod daemon;
mod exception;
mod execute;
mod herdr;
mod hooks;
mod runner;
mod schedule;
mod service;
mod skills;
mod state;
mod template;
mod workstation;

use anyhow::{Context, Result};
use chrono::Local;
use clap::{Parser, Subcommand};
use config::{Config, ResolvedTask};
use daemon::daemon;
use execute::{execute, Gates};
use state::Store;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;

#[derive(Parser)]
#[command(name = "work-agent", version, about = "Scheduled agent-task orchestrator")]
struct Cli {
    /// Profile folder: `config.toml` (optional), `tasks/`, `workstations/` and
    /// `state.json` live there, outside this repo, so a profile can be its own
    /// (private) repository and one checkout can hold several of them. Defaults
    /// to the current folder when it looks like a profile (it has a `tasks/`
    /// folder or a `config.toml`), else `$XDG_CONFIG_HOME/work-agent/default`.
    #[arg(short, long, env = "WORK_AGENT_PROFILE")]
    profile: Option<PathBuf>,
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run forever, cycling through due tasks. The default command everywhere
    /// except inside a profile folder, where bare `work-agent` installs instead.
    Daemon,
    /// Print each task with its schedule, last run, run status and next due time, then exit.
    List,
    /// Pick a task with fzf (previewing its file) and open it in $VISUAL/$EDITOR, or nvim.
    Show,
    /// Run one task immediately, ignoring its schedule and exception rule.
    /// Its `preflight` and `validate` gates still apply, unless --ignore-gates.
    /// This is the only way to start a task that has no `schedule` (manual task).
    /// Without a name, lists every runnable task instead.
    Run {
        name: Option<String>,
        /// Run the task whatever its `preflight` says, and take the runner's own
        /// word for the outcome instead of running `validate`.
        #[arg(long)]
        ignore_gates: bool,
    },
    /// Validate the config and exit.
    Check,
    /// Install the daemon as a systemd user service, one per profile path given
    /// (`--uninstall` / `--status` to remove or inspect them). With no path, the
    /// profile in hand: bare `work-agent` in a profile folder is the same install.
    Service(service::ServiceArgs),
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    let cli = Cli::parse();
    let (profile, source) = resolved_profile(cli.profile.as_deref());
    // A profile folder is a deployment, so bare `work-agent` inside one installs
    // it rather than running a daemon here: `work-agent daemon` (or a unit file
    // that stops when you close the terminal) is the foreground case.
    if cli.cmd.is_none() && source == Source::Cwd {
        return service::install_here(&profile);
    }
    // Loaded per arm: `service` manages other profiles and must not need this one.
    let load = || Config::load(&profile);
    // `service` with no path acts on this profile, but only when someone named
    // one: falling back to a service for the built-in default would be a guess.
    let fallback = (source != Source::Default).then(|| profile.clone());
    match cli.cmd.unwrap_or(Cmd::Daemon) {
        Cmd::Check => check(&load()?),
        Cmd::List => print_schedule(&load()?),
        Cmd::Show => show_task(&load()?),
        Cmd::Run { name: None, .. } => print_runnable(&load()?),
        Cmd::Run { name: Some(name), ignore_gates } => {
            let gates = if ignore_gates { Gates::Off } else { Gates::All };
            run_one(&load()?, &name, gates).await
        }
        Cmd::Daemon => daemon(load()?, profile.clone()).await,
        Cmd::Service(args) => service::run(args, fallback),
    }
}

/// Where the profile folder came from.
#[derive(PartialEq)]
enum Source {
    /// `--profile` or `WORK_AGENT_PROFILE`: named by the user.
    Given,
    /// The current folder, because it looks like a profile.
    Cwd,
    /// Neither: `$XDG_CONFIG_HOME/work-agent/default`.
    Default,
}

/// The profile to act on, and where it came from: `--profile` (or its env
/// variable), else the current folder when that is a profile, else the default.
fn resolved_profile(flag: Option<&std::path::Path>) -> (PathBuf, Source) {
    if let Some(profile) = flag {
        return (config::expand_home(profile), Source::Given);
    }
    match std::env::current_dir().ok().filter(|dir| config::looks_like_profile(dir)) {
        Some(dir) => (dir, Source::Cwd),
        None => (default_profile(), Source::Default),
    }
}

/// `check`: the config loaded, so report it, and warn about every cwd that does not exist.
fn check(cfg: &Config) -> Result<()> {
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
    Ok(())
}

/// `run <name>`: one task now, schedule and exception rule ignored (and the
/// gates too with `--ignore-gates`).
async fn run_one(cfg: &Config, name: &str, gates: Gates) -> Result<()> {
    let task = cfg
        .resolved_tasks()
        .into_iter()
        .find(|t| t.name == name)
        .ok_or_else(|| anyhow::anyhow!("no enabled task named `{name}`"))?;
    let store = Arc::new(Mutex::new(Store::load(&cfg.state_file)?));
    execute(task, cfg, store, gates).await;
    Ok(())
}

/// `$XDG_CONFIG_HOME/work-agent/default`, the profile used when none is given.
fn default_profile() -> PathBuf {
    let config_home = config::user_config_home().unwrap_or_else(|| PathBuf::from("."));
    config_home.join("work-agent").join("default")
}

type When = Option<chrono::DateTime<Local>>;

/// One row of the `list` / `run` tables: a task with its timestamps, the status
/// of its last execution and whether it is due, all computed against one `now`.
struct Row {
    task: ResolvedTask,
    last: When,
    next: When,
    /// `never ran`, `3 successes in a row`, `2 failures in a row (running now)`.
    status: String,
    due: bool,
}

/// Every enabled task with what both tables show for it.
fn task_rows(cfg: &Config) -> Result<Vec<Row>> {
    let store = Store::load(&cfg.state_file)?;
    let now = Local::now();
    let mut rows = Vec::new();
    for task in cfg.resolved_tasks() {
        let last = store.last_run(&task.name);
        let (next, due) = match &task.schedule {
            Some(s) => (schedule::next_after(s, last.unwrap_or(now))?, schedule::is_due(s, last, now)?),
            None => (None, false),
        };
        let status = store.status_label(&task.name);
        rows.push(Row { task, last, next, status, due });
    }
    Ok(rows)
}

/// `YYYY-MM-DD HH:MM`, else `none`: `never` for a run that has not happened,
/// `-` for a task with no schedule to compute a next run from.
fn stamp(d: When, none: &str) -> String {
    d.map(|d| d.format("%Y-%m-%d %H:%M").to_string()).unwrap_or_else(|| none.into())
}

/// `list`: every task with its schedule, last run, streak, next run and whether it is due now.
fn print_schedule(cfg: &Config) -> Result<()> {
    println!(
        "{:<24} {:<14} {:<7} {:<5} {:<17} {:<17} {:<4} STATUS",
        "TASK", "SCHEDULE", "HARNESS", "PAR", "LAST RUN", "NEXT RUN", "DUE"
    );
    for r in task_rows(cfg)? {
        println!(
            "{:<24} {:<14} {:<7} {:<5} {:<17} {:<17} {:<4} {}",
            r.task.name,
            r.task.schedule.as_deref().unwrap_or("manual"),
            r.task.harness,
            r.task.parallel,
            stamp(r.last, "never"),
            stamp(r.next, "-"),
            if r.due { "DUE" } else { "" },
            r.status
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
    println!(
        "{:<24} {:<32} {:<17} {:<17} {:<28} DESCRIPTION",
        "TASK", "STATUS", "LAST RUN", "NEXT RUN", "HARNESS/MODEL"
    );
    for r in task_rows(cfg)? {
        println!(
            "{:<24} {:<32} {:<17} {:<17} {:<28} {}",
            r.task.name,
            r.status,
            stamp(r.last, "never"),
            stamp(r.next, "-"),
            format!("{}/{}", r.task.harness, r.task.model().unwrap_or("-")),
            r.task.description.as_deref().unwrap_or(""),
        );
    }
    Ok(())
}
