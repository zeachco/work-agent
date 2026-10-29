use anyhow::{Context, Result};
use chrono::{DateTime, Local};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct State {
    #[serde(default)]
    pub tasks: BTreeMap<String, TaskState>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct TaskState {
    pub last_run: Option<DateTime<Local>>,
    pub last_outcome: Option<String>,
    /// Set when the exception prompt told us to drop a scheduled run.
    pub last_skipped: Option<DateTime<Local>>,
}

pub struct Store {
    path: PathBuf,
    state: State,
}

impl Store {
    pub fn load(path: &Path) -> Result<Self> {
        let state = match std::fs::read_to_string(path) {
            Ok(raw) => serde_json::from_str(&raw).with_context(|| format!("parsing {}", path.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => State::default(),
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        Ok(Self { path: path.to_path_buf(), state })
    }

    pub fn last_run(&self, task: &str) -> Option<DateTime<Local>> {
        self.state.tasks.get(task).and_then(|t| t.last_run)
    }

    pub fn record(&mut self, task: &str, f: impl FnOnce(&mut TaskState)) -> Result<()> {
        f(self.state.tasks.entry(task.to_string()).or_default());
        self.save()
    }

    fn save(&self) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(&self.state)?)?;
        std::fs::rename(&tmp, &self.path).with_context(|| format!("writing {}", self.path.display()))
    }
}
