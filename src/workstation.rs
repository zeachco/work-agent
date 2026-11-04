//! Per-task scratch directories ("workstations").
//!
//! `<workstations_dir>/<task>/` lives in the profile and persists across runs. It holds
//! `memory.md` (notes the agent keeps for its future self, tracked when the
//! profile is a git repo, so never a secret), `short_memory.md` (gitignored:
//! secrets, sensitive matter and details not worth tracking) and any helper
//! file the agent decides to keep, e.g. `workstations/say-hello/format.py`.

use anyhow::{Context, Result};
use std::path::Path;

const MEMORY_FILE: &str = "memory.md";
const SHORT_MEMORY_FILE: &str = "short_memory.md";

/// Creates the directory and stub `memory.md` / `short_memory.md` files for
/// whichever does not exist yet.
pub fn prepare(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating workstation {}", dir.display()))?;
    let name = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let stubs = [
        (MEMORY_FILE, format!(
            "# {name} memory\n\nNotes carried from one run to the next. Keep it short; prune what is stale.\n\
             This file may be tracked in git: never write secrets or sensitive details here, use {SHORT_MEMORY_FILE}.\n"
        )),
        (SHORT_MEMORY_FILE, format!(
            "# {name} short memory\n\nGitignored. Secrets, sensitive matter and small details not worth tracking go here.\n"
        )),
    ];
    for (file, body) in stubs {
        let path = dir.join(file);
        if !path.exists() {
            std::fs::write(&path, body).with_context(|| format!("writing {}", path.display()))?;
        }
    }
    Ok(())
}

/// Prefixes the task prompt with a short note telling the agent where its
/// workstation is and how to use it.
pub fn with_preamble(prompt: &str, dir: &Path) -> String {
    let dir = dir.display();
    format!(
        "Your workstation is `{dir}` (outside the working tree, persists across runs). \
         Read `{dir}/{MEMORY_FILE}` and `{dir}/{SHORT_MEMORY_FILE}` first and update them before finishing with anything worth remembering next time: \
         `{MEMORY_FILE}` is tracked and must never contain secrets or sensitive details; those, and unimportant details, go in `{SHORT_MEMORY_FILE}` (gitignored). \
         Put temp files and any script or artifact you want to reuse in that directory, not in the working tree.\n\n{prompt}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prepare_creates_memory_once() {
        let tmp = std::env::temp_dir().join(format!("ws-test-{}", std::process::id()));
        let dir = tmp.join("say-hello");
        prepare(&dir).unwrap();
        let memory = dir.join(MEMORY_FILE);
        assert!(std::fs::read_to_string(&memory).unwrap().contains("say-hello"));
        assert!(dir.join(SHORT_MEMORY_FILE).is_file());
        std::fs::write(&memory, "kept").unwrap();
        prepare(&dir).unwrap();
        assert_eq!(std::fs::read_to_string(&memory).unwrap(), "kept");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn preamble_mentions_dir_and_prompt() {
        let out = with_preamble("do it", Path::new("/ws/x"));
        assert!(out.contains("/ws/x/memory.md"));
        assert!(out.contains("/ws/x/short_memory.md"));
        assert!(out.ends_with("\n\ndo it"));
    }
}
