//! The daemon loop: a tick every `tick_seconds`, hot reload of the profile,
//! and one `execute` per due task (spawned when the task is `parallel`).
use crate::config::Config;
use crate::execute::{execute, Gates};
use crate::schedule;
use crate::state::Store;
use anyhow::Result;
use chrono::Local;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;
use tokio::sync::Mutex;
use tracing::{error, info};

/// Runs forever: reloads the profile when a source file changed, then runs every due task.
pub async fn daemon(cfg: Config, profile: PathBuf) -> Result<()> {
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
            if !is_due(&store, &task.name, expr).await {
                continue;
            }
            if task.parallel {
                let (cfg, store) = (cfg.clone(), store.clone());
                tokio::spawn(async move { execute(task, &cfg, store, Gates::All).await });
            } else {
                execute(task, &cfg, store.clone(), Gates::All).await;
            }
        }
    }
}

/// Whether the task's cron is due given its last recorded run. A schedule
/// error is logged and counts as not due.
async fn is_due(store: &Mutex<Store>, task: &str, expr: &str) -> bool {
    let last = store.lock().await.last_run(task);
    match schedule::is_due(expr, last, Local::now()) {
        Ok(due) => due,
        Err(e) => {
            error!(task, "schedule error: {e:#}");
            false
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
