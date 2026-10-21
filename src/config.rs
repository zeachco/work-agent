use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Top-level JSON config (see `crons.example.json`).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// How often the daemon wakes up to look for due tasks, in seconds.
    #[serde(default = "default_tick")]
    pub tick_seconds: u64,
    /// Where `last run` timestamps are persisted (relative to the config file).
    #[serde(default = "default_state_file")]
    pub state_file: PathBuf,
    /// Defaults applied to every task that does not override them.
    #[serde(default)]
    pub defaults: Defaults,
    /// Local LLM used to evaluate `exception` prompts.
    #[serde(default)]
    pub exception_llm: ExceptionLlm,
    /// Dates exposed to exception prompts as `${holidays}` (YYYY-MM-DD).
    #[serde(default)]
    pub holidays: Vec<String>,
    pub tasks: Vec<Task>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Defaults {
    /// Harness executable: `pi`, `claude`, ...
    #[serde(default = "default_harness")]
    pub harness: String,
    /// Extra arguments passed to the harness after `--`.
    #[serde(default = "default_args")]
    pub args: Vec<String>,
    /// Default working directory for tasks.
    #[serde(default)]
    pub cwd: Option<PathBuf>,
    /// `herdr` (spawn a workspace + interactive agent) or `process` (plain subprocess).
    #[serde(default)]
    pub runner: Runner,
    /// Max wall-clock time for one task run, in seconds.
    #[serde(default = "default_timeout")]
    pub timeout_seconds: u64,
    /// Close the herdr workspace once the agent settles.
    #[serde(default)]
    pub close_workspace_on_done: bool,
    /// Always spawn a new workspace labelled `cron:<name>:<date time>` instead
    /// of reusing the task's existing one.
    #[serde(default)]
    pub fresh_workspace: bool,
}

impl Default for Defaults {
    fn default() -> Self {
        Self {
            harness: default_harness(),
            args: default_args(),
            cwd: None,
            runner: Runner::default(),
            timeout_seconds: default_timeout(),
            close_workspace_on_done: false,
            fresh_workspace: false,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Runner {
    #[default]
    Herdr,
    Process,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExceptionLlm {
    #[serde(default = "default_llm_url")]
    pub base_url: String,
    #[serde(default = "default_llm_model")]
    pub model: String,
    #[serde(default = "default_llm_timeout")]
    pub timeout_seconds: u64,
}

impl Default for ExceptionLlm {
    fn default() -> Self {
        Self {
            base_url: default_llm_url(),
            model: default_llm_model(),
            timeout_seconds: default_llm_timeout(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Task {
    /// Unique name, `[a-z][a-z0-9_-]{0,31}` (used as the herdr agent name).
    pub name: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Harness executable override.
    pub harness: Option<String>,
    /// Harness argument override (replaces defaults.args).
    pub args: Option<Vec<String>>,
    /// What the agent is asked to do. Supports `${...}` placeholders.
    pub prompt: String,
    /// Directory the harness is started from.
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

fn default_tick() -> u64 { 60 }
fn default_state_file() -> PathBuf { PathBuf::from("state.json") }
fn default_harness() -> String { "pi".into() }
fn default_args() -> Vec<String> { vec!["--model".into(), "llamacpp/gemma-4-E4B-it".into()] }
fn default_timeout() -> u64 { 3600 }
fn default_llm_url() -> String { "http://localhost:7070/v1".into() }
fn default_llm_model() -> String { "gemma-4-E4B-it".into() }
fn default_llm_timeout() -> u64 { 60 }
fn default_true() -> bool { true }

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("reading config {}", path.display()))?;
        let mut cfg: Config = serde_json::from_str(&raw)
            .with_context(|| format!("parsing config {}", path.display()))?;
        let base = path.parent().map(Path::to_path_buf).unwrap_or_default();
        if cfg.state_file.is_relative() {
            cfg.state_file = base.join(&cfg.state_file);
        }
        cfg.validate()?;
        Ok(cfg)
    }

    fn validate(&self) -> Result<()> {
        let mut seen = std::collections::HashSet::new();
        for t in &self.tasks {
            anyhow::ensure!(seen.insert(&t.name), "duplicate task name `{}`", t.name);
            anyhow::ensure!(valid_name(&t.name), "task name `{}` must match [a-z][a-z0-9_-]{{0,31}}", t.name);
            if let Some(schedule) = &t.schedule {
                crate::schedule::parse(schedule)
                    .with_context(|| format!("task `{}` has an invalid schedule", t.name))?;
            }
        }
        Ok(())
    }
}

fn valid_name(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some('a'..='z'))
        && s.len() <= 32
        && chars.all(|c| matches!(c, 'a'..='z' | '0'..='9' | '_' | '-'))
}

/// A task with every default resolved.
#[derive(Debug, Clone)]
pub struct ResolvedTask {
    pub name: String,
    pub harness: String,
    pub args: Vec<String>,
    pub prompt: String,
    pub cwd: PathBuf,
    pub parallel: bool,
    /// `None` means manual: only `run <name>` starts it.
    pub schedule: Option<String>,
    pub exception: Option<String>,
    pub runner: Runner,
    pub timeout_seconds: u64,
    pub close_workspace_on_done: bool,
    pub fresh_workspace: bool,
}

impl Config {
    pub fn resolved_tasks(&self) -> Vec<ResolvedTask> {
        let d = &self.defaults;
        self.tasks
            .iter()
            .filter(|t| t.enabled)
            .map(|t| ResolvedTask {
                name: t.name.clone(),
                harness: t.harness.clone().unwrap_or_else(|| d.harness.clone()),
                args: t.args.clone().unwrap_or_else(|| d.args.clone()),
                prompt: t.prompt.clone(),
                cwd: t
                    .cwd
                    .clone()
                    .or_else(|| d.cwd.clone())
                    .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))),
                parallel: t.parallel,
                schedule: t.schedule.clone(),
                exception: t.exception.clone(),
                runner: t.runner.unwrap_or(d.runner),
                timeout_seconds: t.timeout_seconds.unwrap_or(d.timeout_seconds),
                close_workspace_on_done: t.close_workspace_on_done.unwrap_or(d.close_workspace_on_done),
                fresh_workspace: t.fresh_workspace.unwrap_or(d.fresh_workspace),
            })
            .collect()
    }
}
