//! Per-task scratch directories ("workstations").
//!
//! `<workstations_dir>/<task>/` is gitignored and persists across runs. It holds
//! `memory.md` (notes the agent keeps for its future self) and any helper file
//! the agent decides to keep, e.g. `workstations/say-hello/format.py`.

use anyhow::{Context, Result};
use std::path::Path;

const MEMORY_FILE: &str = "memory.md";

/// Creates the directory and an empty-ish `memory.md` if they do not exist yet.
pub fn prepare(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating workstation {}", dir.display()))?;
    let memory = dir.join(MEMORY_FILE);
    if !memory.exists() {
        let name = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        std::fs::write(
            &memory,
            format!("# {name} memory\n\nNotes carried from one run to the next. Keep it short; prune what is stale.\n"),
        )
        .with_context(|| format!("writing {}", memory.display()))?;
    }
    Ok(())
}

/// Prefixes the task prompt with a short note telling the agent where its
/// workstation is and how to use it.
pub fn with_preamble(prompt: &str, dir: &Path) -> String {
    let dir = dir.display();
    format!(
        "Your workstation is `{dir}` (gitignored, persists across runs). \
         Read `{dir}/{MEMORY_FILE}` first and update it before finishing with anything worth remembering next time. \
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
        std::fs::write(&memory, "kept").unwrap();
        prepare(&dir).unwrap();
        assert_eq!(std::fs::read_to_string(&memory).unwrap(), "kept");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn preamble_mentions_dir_and_prompt() {
        let out = with_preamble("do it", Path::new("/ws/x"));
        assert!(out.contains("/ws/x/memory.md"));
        assert!(out.ends_with("\n\ndo it"));
    }
}
