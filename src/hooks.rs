//! The two shell gates a task can carry: `preflight` before the run and
//! `validate` after it. Each is a shell line where the exit code is a verdict
//! rather than a crash — a non-zero `preflight` skips the run, a non-zero
//! `validate` fails it.
use crate::config::ResolvedTask;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;
use tracing::debug;

/// How much of a gate's output to keep in the log and in the state detail.
const EXCERPT: usize = 200;

/// Runs `line` through `sh -c` from the task's cwd, so a relative script path
/// (`bash ./preflight.sh`) resolves against the directory the task itself runs
/// in. `Ok(())` on exit 0, else a one-line reason: the exit code plus whatever
/// the command said. A gate that cannot start at all (bad `cwd`) is an error
/// too, so a typo never silently runs the task without its gate.
pub async fn check(gate: &str, task: &ResolvedTask, line: &str) -> Result<(), String> {
    let shown = one_line(line);
    let what = || format!("{gate} `{shown}`");
    let child = Command::new("sh")
        .arg("-c")
        .arg(line)
        .current_dir(&task.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("{} could not start: {e}", what()))?;
    let out = tokio::time::timeout(Duration::from_secs(task.timeout_seconds), child.wait_with_output())
        .await
        .map_err(|_| format!("{} timed out after {}s", what(), task.timeout_seconds))?
        .map_err(|e| format!("{} failed: {e}", what()))?;
    if out.status.success() {
        debug!(task = %task.name, gate, "gate passed");
        return Ok(());
    }
    let code = out.status.code().map(|c| c.to_string()).unwrap_or_else(|| "a signal".into());
    Err(format!("{} exited {code}{}", what(), excerpt(&out.stderr, &out.stdout)))
}

/// A gate written as a literal multi-line script would otherwise split the log
/// entry it is quoted into, so messages show it flattened.
fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The command's own explanation of its exit: stderr when it wrote any, else
/// stdout, one line, shortened.
fn excerpt(stderr: &[u8], stdout: &[u8]) -> String {
    let text_of = |b: &[u8]| String::from_utf8_lossy(b).trim().to_string();
    let mut text = text_of(stderr);
    if text.is_empty() {
        text = text_of(stdout);
    }
    if text.is_empty() {
        return String::new();
    }
    format!(": {}", clip(&text.replace('\n', " | ")))
}

/// Keeps the first `EXCERPT` bytes of `s`, cutting on a character boundary.
fn clip(s: &str) -> String {
    if s.len() <= EXCERPT {
        return s.to_string();
    }
    let mut cut = EXCERPT;
    while !s.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}…", &s[..cut])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn task(cwd: &str) -> ResolvedTask {
        ResolvedTask {
            name: "gate-test".into(),
            profile: "test".into(),
            description: None,
            harness: "pi".into(),
            args: vec![],
            prompt: None,
            command: None,
            cwd: PathBuf::from(cwd),
            workstation: PathBuf::from(cwd),
            parallel: false,
            schedule: None,
            exception: None,
            preflight: None,
            validate: None,
            runner: crate::config::Runner::Process,
            timeout_seconds: 10,
            close_workspace_on_done: false,
            fresh_workspace: false,
        }
    }

    #[tokio::test]
    async fn zero_passes_and_non_zero_reports_the_code_and_the_output() {
        assert!(check("preflight", &task("/"), "true").await.is_ok());
        let err = check("validate", &task("/"), "echo hi; echo nope >&2; exit 3").await.unwrap_err();
        assert_eq!(err, "validate `echo hi; echo nope >&2; exit 3` exited 3: nope");
        let err = check("preflight", &task("/"), "exit 1").await.unwrap_err();
        assert_eq!(err, "preflight `exit 1` exited 1");
    }

    /// A relative script must be looked up in the task's cwd, not the daemon's.
    #[tokio::test]
    async fn a_relative_script_runs_from_the_task_cwd() {
        let dir = std::env::temp_dir().join(format!("work-agent-gate-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("preflight.sh"), "#!/bin/sh\npwd\n").unwrap();
        let task = task(&dir.display().to_string());
        let err = check("preflight", &task, "bash ./preflight.sh && exit 7").await.unwrap_err();
        assert!(err.contains("exited 7"), "{err}");
        assert!(err.contains(&dir.display().to_string()), "{err}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn a_gate_that_cannot_run_is_an_error_not_a_pass() {
        let err = check("preflight", &task("/no/such/dir"), "true").await.unwrap_err();
        assert!(err.contains("could not start"), "{err}");
    }

    #[test]
    fn a_multiline_gate_is_quoted_as_one_line() {
        let err = one_line("if [ -f x ]; then\n  exit 1\nfi\n");
        assert_eq!(err, "if [ -f x ]; then exit 1 fi");
    }

    #[test]
    fn long_output_is_clipped_on_a_character_boundary() {
        let long = "x".repeat(300);
        assert_eq!(clip(&long).chars().count(), EXCERPT + 1);
        // A cut in the middle of a multi-byte character must not panic.
        assert!(clip(&"é".repeat(300)).ends_with('…'));
        assert_eq!(clip("short"), "short");
    }
}
