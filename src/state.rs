//! The `state.json` file: what the daemon remembers about each task between
//! runs — the cron anchor, the last verdict and its streak, and whether a run
//! is in flight right now.
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

/// Whether a run that really executed the task worked. A skip is deliberately
/// not a verdict: a task held off before it started has neither succeeded nor
/// failed, it has just not run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    Success,
    Failure,
}

impl Verdict {
    /// The sign of a streak of this verdict: `+1` successes, `-1` failures.
    fn step(self) -> i64 {
        match self {
            Verdict::Success => 1,
            Verdict::Failure => -1,
        }
    }
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct TaskState {
    /// When the last run (or the last skipped run) started: the anchor cron is
    /// compared against, so it moves as soon as a run begins.
    pub last_run: Option<DateTime<Local>>,
    /// Human-readable tail of the last event: a runner status, a skip reason,
    /// or `running` while a run is in flight.
    pub last_outcome: Option<String>,
    /// Set when the `exception` rule dropped a scheduled run.
    pub last_skipped: Option<DateTime<Local>>,
    /// Verdict of the last run that executed the task. Absent for a task that
    /// only ever skipped, and in state files written before this field existed.
    pub last_result: Option<Verdict>,
    /// Consecutive runs of `last_result`: positive for successes, negative for
    /// failures, 0 when nothing has run yet. Skips leave it untouched.
    #[serde(default)]
    pub streak: i64,
    /// Pid of the process running this task right now, so `work-agent list`
    /// (another process) can say so. Left behind by a crash, which is why
    /// `running` checks the pid instead of trusting it.
    pub running_pid: Option<u32>,
}

impl TaskState {
    /// Marks a run as started *before* it happens, so a crash mid-run cannot
    /// retrigger the task on restart.
    pub fn mark_running(&mut self, started: DateTime<Local>) {
        self.last_run = Some(started);
        self.last_outcome = Some("running".into());
        self.running_pid = Some(std::process::id());
    }

    /// Records the verdict of a run that executed the task: extends the streak
    /// when it matches the previous one, restarts it at one otherwise. Keeps
    /// `last_run` on the start time, which is what cron compares against.
    pub fn mark_finished(&mut self, started: DateTime<Local>, verdict: Verdict, outcome: impl Into<String>) {
        let step = verdict.step();
        let prev = self.streak_len();
        self.streak = if prev != 0 && prev.signum() == step { prev + step } else { step };
        self.last_run = Some(started);
        self.last_result = Some(verdict);
        self.last_outcome = Some(outcome.into());
        self.running_pid = None;
    }

    /// Records a run dropped before it started (`exception` rule). It
    /// moves the cron anchor so the task is not retried this tick, but does not
    /// touch the streak: nothing ran, so there is nothing to judge.
    pub fn mark_skipped(&mut self, started: DateTime<Local>, outcome: impl Into<String>) {
        self.last_run = Some(started);
        self.last_skipped = Some(started);
        self.last_outcome = Some(outcome.into());
        self.running_pid = None;
    }

    /// The verdict of the last run that executed the task. A state file written
    /// before `last_result` existed is read back through its `last_outcome`
    /// string, so an upgraded daemon still shows a sensible streak.
    pub fn verdict(&self) -> Option<Verdict> {
        if self.last_result.is_some() {
            return self.last_result;
        }
        let outcome = self.last_outcome.as_deref()?;
        if outcome.starts_with("skipped") || outcome == "running" {
            return None;
        }
        Some(if outcome.starts_with("error:") || outcome.starts_with("failed:") {
            Verdict::Failure
        } else {
            Verdict::Success
        })
    }

    /// How many runs the current verdict has been seen for, signed, at least
    /// one whenever a verdict exists (an old entry knows the verdict but not
    /// how long the streak is).
    fn streak_len(&self) -> i64 {
        let step = match self.verdict() {
            Some(v) => v.step(),
            None => 0,
        };
        if step == 0 {
            0
        } else {
            step * self.streak.abs().max(1)
        }
    }

    /// Whether a live process is running this task right now. `live` reads
    /// Linux's procfs: a pid left behind by a dead daemon is not running.
    fn running(&self) -> bool {
        self.running_pid.is_some_and(live_pid)
    }

    /// Whether the last event for this task was a skip rather than a run.
    fn skipped_last(&self) -> bool {
        // Both marks are stamped at the skip; a later run moves only `last_run`.
        matches!((self.last_skipped, self.last_run), (Some(s), Some(r)) if s >= r)
    }

    /// One line on the last execution, for `list` and `run`: `never ran`,
    /// `3 successes in a row`, `2 failures in a row (running now)`.
    pub fn status_label(&self) -> String {
        let len = self.streak_len();
        let mut label = match (len, self.verdict()) {
            (n, Some(Verdict::Success)) if n > 0 => counted(n, "success", "successes"),
            (n, Some(Verdict::Failure)) => counted(-n, "failure", "failures"),
            _ => "never ran".into(),
        };
        if self.skipped_last() {
            label.push_str(", skipped last");
        }
        if self.running() {
            label.push_str(" (running now)");
        }
        label
    }
}

/// `1 failure in a row`, `2 failures in a row`: a streak count with the noun it agrees with.
fn counted(n: i64, one: &str, many: &str) -> String {
    format!("{n} {} in a row", if n == 1 { one } else { many })
}

/// Whether a pid belongs to a live process (Linux procfs; no procfs, no check).
fn live_pid(pid: u32) -> bool {
    Path::new("/proc").is_dir() && Path::new(&format!("/proc/{pid}")).exists()
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

    /// What is known about one task, if it ever ran or skipped.
    pub fn task_state(&self, task: &str) -> Option<&TaskState> {
        self.state.tasks.get(task)
    }

    /// Status of the last execution, for the tables in `main.rs`: `never ran`
    /// for a task with no entry at all.
    pub fn status_label(&self, task: &str) -> String {
        self.task_state(task).map(TaskState::status_label).unwrap_or_else(|| "never ran".into())
    }

    pub fn record(&mut self, task: &str, f: impl FnOnce(&mut TaskState)) -> Result<()> {
        f(self.state.tasks.entry(task.to_string()).or_default());
        self.save()
    }

    fn save(&self) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        // why: write-then-rename keeps state.json atomic, so a crash mid-save cannot
        // leave a truncated file that would make every task look never-run.
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(&self.state)?)?;
        std::fs::rename(&tmp, &self.path).with_context(|| format!("writing {}", self.path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> DateTime<Local> {
        Local::now()
    }

    #[test]
    fn a_streak_counts_runs_of_the_same_verdict() {
        let mut s = TaskState::default();
        assert_eq!(s.status_label(), "never ran");
        for _ in 0..3 {
            s.mark_finished(now(), Verdict::Success, "settled");
        }
        assert_eq!(s.status_label(), "3 successes in a row");
        s.mark_finished(now(), Verdict::Failure, "error: boom");
        assert_eq!(s.status_label(), "1 failure in a row");
        s.mark_finished(now(), Verdict::Failure, "error: boom");
        assert_eq!(s.streak, -2);
    }

    #[test]
    fn a_run_in_flight_keeps_the_previous_streak() {
        let mut s = TaskState::default();
        s.mark_finished(now(), Verdict::Failure, "error: boom");
        s.mark_running(now());
        // The pid of this test process is alive, so the mark reads as running.
        assert_eq!(s.status_label(), "1 failure in a row (running now)");
        s.mark_finished(now(), Verdict::Success, "settled");
        assert_eq!(s.status_label(), "1 success in a row");
        assert_eq!(s.streak, 1);
    }

    #[test]
    fn a_dead_pid_is_not_running() {
        let mut s = TaskState::default();
        s.mark_finished(now(), Verdict::Success, "settled");
        s.running_pid = Some(u32::MAX); // no such process: a mark left by a crash
        assert_eq!(s.status_label(), "1 success in a row");
    }

    #[test]
    fn a_skip_is_neutral_and_says_so() {
        let mut s = TaskState::default();
        s.mark_finished(now(), Verdict::Success, "settled");
        s.mark_finished(now(), Verdict::Success, "settled");
        s.mark_skipped(now(), "skipped: nothing to do today");
        assert_eq!(s.status_label(), "2 successes in a row, skipped last");
        s.mark_finished(now(), Verdict::Success, "settled");
        assert_eq!(s.status_label(), "3 successes in a row");
    }

    #[test]
    fn a_task_only_ever_skipped_has_never_ran() {
        let mut s = TaskState::default();
        s.mark_skipped(now(), "skipped: exception said yes");
        assert_eq!(s.status_label(), "never ran, skipped last");
        assert_eq!(s.verdict(), None);
    }

    /// State written before `last_result` and `streak` existed: the verdict is
    /// read back from the outcome string and an unknown streak counts as one.
    #[test]
    fn an_old_state_file_still_reports_a_streak() {
        let s: TaskState = serde_json::from_str(
            r#"{"last_run":"2026-11-06T04:00:00-05:00","last_outcome":"settled","last_skipped":null}"#,
        )
        .unwrap();
        assert_eq!(s.verdict(), Some(Verdict::Success));
        assert_eq!(s.status_label(), "1 success in a row");

        let s: TaskState = serde_json::from_str(r#"{"last_outcome":"error: timed out"}"#).unwrap();
        assert_eq!(s.status_label(), "1 failure in a row");

        let s: TaskState = serde_json::from_str(r#"{"last_outcome":"skipped"}"#).unwrap();
        assert_eq!(s.status_label(), "never ran");

        let s: TaskState = serde_json::from_str(r#"{"last_outcome":"failed: cwd missing"}"#).unwrap();
        assert_eq!(s.verdict(), Some(Verdict::Failure));
    }

    #[test]
    fn state_round_trips_through_json() {
        let mut s = TaskState::default();
        s.mark_running(now());
        s.mark_finished(now(), Verdict::Success, "settled");
        let json = serde_json::to_string(&s).unwrap();
        let back: TaskState = serde_json::from_str(&json).unwrap();
        assert_eq!(back.streak, 1);
        assert_eq!(back.last_result, Some(Verdict::Success));
        assert_eq!(back.running_pid, None);
    }
}
