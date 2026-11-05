use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};

/// Global settings from `<profile>/config.toml` (see `examples/config.toml`).
/// The file is optional: a profile folder holding only `tasks/` runs with the
/// built-in defaults. Tasks are not in this file: each one is its own
/// `<tasks_dir>/<name>.toml`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// How often the daemon wakes up to look for due tasks, in seconds.
    #[serde(default = "default_tick")]
    pub tick_seconds: u64,
    /// Where `last run` timestamps are persisted (relative to the profile).
    #[serde(default = "default_state_file")]
    pub state_file: PathBuf,
    /// Per-task scratch area (relative to the profile). Each task gets
    /// `<workstations_dir>/<name>/` with a `memory.md` for notes carried across
    /// runs plus any helper files the agent wants to keep. Exposed to prompts as
    /// `${workstation}`.
    #[serde(default = "default_workstations_dir")]
    pub workstations_dir: PathBuf,
    /// Folder of task files (relative to the profile). Every `*.toml` in it
    /// is one task whose name is the file stem; files are loaded in name order.
    #[serde(default = "default_tasks_dir")]
    pub tasks_dir: PathBuf,
    /// Defaults applied to every task that does not override them.
    #[serde(default)]
    pub defaults: Defaults,
    /// Local LLM used to evaluate `exception` prompts.
    #[serde(default)]
    pub exception_llm: ExceptionLlm,
    /// Dates exposed to exception prompts as `${holidays}` (YYYY-MM-DD).
    #[serde(default)]
    pub holidays: Vec<String>,
    #[serde(skip)]
    pub tasks: Vec<Task>,
    /// The profile folder everything above was resolved against.
    #[serde(skip)]
    pub profile: PathBuf,
}

/// Missing fields fall back to `Defaults::default()`, the single source of
/// the built-in values.
#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Defaults {
    /// Harness executable: `pi`, `claude`, ...
    pub harness: String,
    /// Extra arguments passed to the harness after `--`.
    pub args: Vec<String>,
    /// Default working directory for tasks. Omitted: each task runs from its
    /// own workstation directory.
    pub cwd: Option<PathBuf>,
    /// `herdr` (spawn a workspace + interactive agent), `process` (plain
    /// subprocess) or `command` (run a shell line directly in a herdr pane).
    pub runner: Runner,
    /// Max wall-clock time for one task run, in seconds.
    pub timeout_seconds: u64,
    /// Close the herdr workspace once the agent settles.
    pub close_workspace_on_done: bool,
    /// Always spawn a new workspace labelled `cron:<name>:<date time>` instead
    /// of reusing the task's existing one.
    pub fresh_workspace: bool,
}

impl Default for Defaults {
    fn default() -> Self {
        Self {
            harness: "pi".into(),
            args: vec!["--model".into(), "llamacpp/gemma-4-E4B-it".into()],
            cwd: None,
            runner: Runner::default(),
            timeout_seconds: 3600,
            close_workspace_on_done: false,
            fresh_workspace: false,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Runner {
    #[default]
    Herdr,
    Process,
    /// Runs the task's `command` line directly in a herdr pane, no agent: the
    /// pane stays attached to the user's herdr session so interactive prompts
    /// (e.g. `sudo`'s password) can be answered by hand.
    Command,
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ExceptionLlm {
    pub base_url: String,
    pub model: String,
    pub timeout_seconds: u64,
}

impl Default for ExceptionLlm {
    fn default() -> Self {
        Self {
            base_url: "http://localhost:7070/v1".into(),
            model: "gemma-4-E4B-it".into(),
            timeout_seconds: 60,
        }
    }
}

/// One `<tasks_dir>/<name>.toml` file.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Task {
    /// The file stem, `[a-z][a-z0-9_-]{0,31}` (also the herdr agent name).
    /// Not a field of the file: it comes from the file name.
    #[serde(skip)]
    pub name: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Free-text summary shown by `run` with no task name.
    pub description: Option<String>,
    /// Harness executable override.
    pub harness: Option<String>,
    /// Harness argument override (replaces defaults.args).
    pub args: Option<Vec<String>>,
    /// What the agent is asked to do. Supports `${...}` placeholders. Required
    /// for the `herdr` and `process` runners; ignored by `command`.
    pub prompt: Option<String>,
    /// Shell line run directly in a herdr pane by the `command` runner.
    pub command: Option<String>,
    /// Directory the harness is started from. Omitted: the task's workstation.
    pub cwd: Option<PathBuf>,
    /// Allow this task to run alongside others (default: false, strictly one at a time).
    #[serde(default)]
    pub parallel: bool,
    /// 5-field (`m h dom mon dow`) or 6-field (`s m h dom mon dow`) cron expression.
    /// Omit it for a manual task: never scheduled by the daemon, only started
    /// with `work-agent run <name>`.
    pub schedule: Option<String>,
    /// Optional small prompt evaluated by the local LLM; a "yes" answer skips this run.
    pub exception: Option<String>,
    pub runner: Option<Runner>,
    pub timeout_seconds: Option<u64>,
    pub close_workspace_on_done: Option<bool>,
    pub fresh_workspace: Option<bool>,
}

/// `$XDG_CONFIG_HOME` when it is absolute, else `$HOME/.config`; `None` when
/// neither variable is set. Callers pick their own fallback.
pub fn user_config_home() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
}


pub const CONFIG_FILE: &str = "config.toml";

/// What a profile keeps out of git: daemon state and the per-task scratch
/// files that are either sensitive (`short_memory.md`) or disposable. Written
/// once, when the profile folder is created; existing files are left alone.
const PROFILE_GITIGNORE: &str = "\
# written by work-agent when this profile was created; edit freely
/state.json
/state.json.tmp
# per-task scratch: only memory.md is worth tracking
/workstations/*/short_memory.md
/workstations/*/*
!/workstations/*/memory.md
";

/// Creates a missing profile folder with `tasks/`, `workstations/` and a
/// `.gitignore`, so a fresh path is ready to be `git init`ed. Leaves an
/// existing folder untouched.
pub fn scaffold(profile: &Path) -> Result<()> {
    if profile.is_dir() {
        return Ok(());
    }
    anyhow::ensure!(
        !profile.exists(),
        "profile path {} exists but is not a folder",
        profile.display()
    );
    for sub in [default_tasks_dir(), default_workstations_dir()] {
        std::fs::create_dir_all(profile.join(sub))
            .with_context(|| format!("creating profile {}", profile.display()))?;
    }
    std::fs::write(profile.join(".gitignore"), PROFILE_GITIGNORE)
        .with_context(|| format!("writing {}/.gitignore", profile.display()))?;
    tracing::info!(profile = %profile.display(), "created empty profile");
    Ok(())
}

fn default_tick() -> u64 {
    60
}
fn default_state_file() -> PathBuf {
    PathBuf::from("state.json")
}
fn default_workstations_dir() -> PathBuf {
    PathBuf::from("workstations")
}
fn default_tasks_dir() -> PathBuf {
    PathBuf::from("tasks")
}
fn default_true() -> bool {
    true
}

impl Config {
    /// Loads a profile folder: `<profile>/config.toml` when present (otherwise
    /// the built-in settings) plus every task under its `tasks_dir`.
    pub fn load(profile: &Path) -> Result<Self> {
        scaffold(profile)?;
        // Absolute so prompts carry usable paths even when the agent runs in another cwd.
        let base = std::fs::canonicalize(profile).unwrap_or_else(|_| profile.to_path_buf());
        let file = base.join(CONFIG_FILE);
        // Every field has a serde default, so a missing file is an empty one.
        let raw = if file.is_file() {
            std::fs::read_to_string(&file).with_context(|| format!("reading config {}", file.display()))?
        } else {
            String::new()
        };
        let mut cfg: Config =
            toml::from_str(&raw).with_context(|| format!("parsing config {}", file.display()))?;
        for path in [&mut cfg.state_file, &mut cfg.workstations_dir, &mut cfg.tasks_dir] {
            if path.is_relative() {
                *path = base.join(&*path);
            }
        }
        cfg.tasks = load_tasks(&cfg.tasks_dir)?;
        cfg.profile = base;
        cfg.validate()?;
        Ok(cfg)
    }

    /// Every file the config is built from, so the daemon can notice edits.
    pub fn source_files(profile: &Path) -> Vec<PathBuf> {
        let mut files = vec![profile.join(CONFIG_FILE)];
        if let Ok(cfg) = Self::load(profile) {
            files.extend(task_files(&cfg.tasks_dir).unwrap_or_default());
        }
        files
    }

    fn validate(&self) -> Result<()> {
        let mut seen = std::collections::HashSet::new();
        for t in &self.tasks {
            anyhow::ensure!(seen.insert(&t.name), "duplicate task name `{}`", t.name);
            anyhow::ensure!(
                valid_name(&t.name),
                "task file name `{}.toml` must match [a-z][a-z0-9_-]{{0,{}}}",
                t.name,
                MAX_TASK_NAME_LEN - 1
            );
            if let Some(schedule) = &t.schedule {
                crate::schedule::parse(schedule)
                    .with_context(|| format!("task `{}` has an invalid schedule", t.name))?;
            }
            if t.runner.unwrap_or(self.defaults.runner) == Runner::Command {
                anyhow::ensure!(
                    t.command.is_some(),
                    "task `{}` uses the command runner but has no `command`",
                    t.name,
                );
            } else {
                anyhow::ensure!(
                    t.prompt.is_some(),
                    "task `{}` needs a `prompt` (the command runner needs `command` instead)",
                    t.name,
                );
            }
        }
        Ok(())
    }
}

/// `<tasks_dir>/*.toml`, sorted by file name. A missing folder is an error:
/// a daemon with nothing to run is almost always a wrong `--profile`.
pub fn task_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let entries = std::fs::read_dir(dir)
        .with_context(|| format!("reading tasks folder {}", dir.display()))?;
    let mut files: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_file() && p.extension().is_some_and(|x| x == "toml"))
        .collect();
    files.sort();
    Ok(files)
}

fn load_tasks(dir: &Path) -> Result<Vec<Task>> {
    task_files(dir)?
        .into_iter()
        .map(|path| {
            let raw = std::fs::read_to_string(&path)
                .with_context(|| format!("reading task {}", path.display()))?;
            let mut task: Task = toml::from_str(&raw)
                .with_context(|| format!("parsing task {}", path.display()))?;
            task.name = path
                .file_stem()
                .and_then(|s| s.to_str())
                .map(str::to_string)
                .unwrap_or_default();
            Ok(task)
        })
        .collect()
}

/// Upper bound on a task name (the file stem, also used as the herdr agent name).
const MAX_TASK_NAME_LEN: usize = 32;

fn valid_name(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some('a'..='z'))
        && s.len() <= MAX_TASK_NAME_LEN
        && chars.all(|c| matches!(c, 'a'..='z' | '0'..='9' | '_' | '-'))
}

/// A task with every default resolved.
#[derive(Debug)]
pub struct ResolvedTask {
    pub name: String,
    /// Folder name of the profile the task belongs to (e.g. `stay22`).
    pub profile: String,
    pub description: Option<String>,
    pub harness: String,
    pub args: Vec<String>,
    pub prompt: Option<String>,
    /// Shell line for the `command` runner.
    pub command: Option<String>,
    pub cwd: PathBuf,
    /// `<workstations_dir>/<name>`: the task's persistent scratch directory.
    pub workstation: PathBuf,
    pub parallel: bool,
    /// `None` means manual: only `run <name>` starts it.
    pub schedule: Option<String>,
    pub exception: Option<String>,
    pub runner: Runner,
    pub timeout_seconds: u64,
    pub close_workspace_on_done: bool,
    pub fresh_workspace: bool,
}

impl ResolvedTask {
    /// The value following `--model` in the harness args, if any.
    pub fn model(&self) -> Option<&str> {
        let mut it = self.args.iter();
        while let Some(a) = it.next() {
            if a == "--model" {
                return it.next().map(String::as_str);
            }
            if let Some(m) = a.strip_prefix("--model=") {
                return Some(m);
            }
        }
        None
    }
}

impl Config {
    pub fn resolved_tasks(&self) -> Vec<ResolvedTask> {
        let d = &self.defaults;
        let profile = self.profile.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        self.tasks
            .iter()
            .filter(|t| t.enabled)
            .map(|t| {
                let runner = t.runner.unwrap_or(d.runner);
                let is_command = runner == Runner::Command;
                let workstation = self.workstations_dir.join(&t.name);
                ResolvedTask {
                    name: t.name.clone(),
                    profile: profile.clone(),
                    description: t.description.clone(),
                    harness: if is_command { "command".into() } else { t.harness.clone().unwrap_or_else(|| d.harness.clone()) },
                    args: if is_command { Vec::new() } else { t.args.clone().unwrap_or_else(|| d.args.clone()) },
                    prompt: t.prompt.clone(),
                    command: t.command.clone(),
                    cwd: t.cwd.clone().or_else(|| d.cwd.clone()).unwrap_or_else(|| workstation.clone()),
                    workstation,
                    parallel: t.parallel,
                    schedule: t.schedule.clone(),
                    exception: t.exception.clone(),
                    runner,
                    timeout_seconds: t.timeout_seconds.unwrap_or(d.timeout_seconds),
                    close_workspace_on_done: t
                        .close_workspace_on_done
                        .unwrap_or(d.close_workspace_on_done),
                    fresh_workspace: t.fresh_workspace.unwrap_or(d.fresh_workspace),
                }
            })
            .collect()
    }
}
