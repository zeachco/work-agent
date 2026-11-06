//! The work-agent skills (`add-task`, ...) live in this repository. Installing a
//! profile drops a symlink to them into the profile folder, so a harness started
//! there finds them without anyone copying anything: pi reads `.agents/skills`
//! from the working directory up through its ancestors (as far as the repository
//! root), which covers every task workstation under the profile, and claude reads
//! `.claude/skills`. Both point at the one folder in the checkout, so editing a
//! skill there is instantly true everywhere, and a profile never holds a stale
//! copy.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// Where each harness looks for a project's skills, relative to the profile, with
/// the harness that reads it. Both resolve to the same folder, and pi dedupes
/// skills by canonical path, so two links are still one skill.
const LINKS: [(&str, &str); 2] = [(".agents/skills", "pi"), (".claude/skills", "claude")];

/// The gitignore patterns that keep the links out of the profile's status, and
/// the comment marking them as work-agent's doing.
const IGNORE_NOTE: &str = "# skills links dropped by work-agent: they point at the work-agent checkout";

/// Points the profile's skill folders at this checkout's skills and keeps the
/// links out of the profile's `git status`. Only reports what it changed, so a
/// profile that is already set up stays quiet.
pub fn link_into(profile: &Path) -> Result<()> {
    let Some(skills) = source() else {
        eprintln!(
            "warning: cannot find the work-agent skills (built from {}); not linking any. \
             Set WORK_AGENT_SKILLS to the folder holding them.",
            env!("CARGO_MANIFEST_DIR")
        );
        return Ok(());
    };
    link_from(&skills, profile)
}

/// `link_into` with the skills folder given, so the tests do not depend on where
/// the binary was built.
fn link_from(skills: &Path, profile: &Path) -> Result<()> {
    // Only the links that are ours get gitignored: a folder of the user's own in
    // one of these places must stay visible to git.
    let mut ours = Vec::new();
    for (rel, harness) in LINKS {
        let link = profile.join(rel);
        match link_one(skills, &link)? {
            Linked::New => println!("skills: {} -> {} ({harness})", link.display(), skills.display()),
            Linked::Same => {}
            Linked::Occupied => {
                eprintln!(
                    "warning: {} is not a work-agent skills link, left it alone ({harness} will not see the skills)",
                    link.display()
                );
                continue;
            }
        }
        ours.push(rel);
    }
    if ours.is_empty() {
        return Ok(());
    }
    ignore(profile, &ours)
}

/// The folder to link: `WORK_AGENT_SKILLS` when it is there, else the one in the
/// checkout this binary was built from (`cargo run`, `cargo install --path .` and
/// the `.cargo` `service` alias all keep that path valid). `None` when neither
/// exists, e.g. a binary installed from a tarball whose checkout is long gone.
fn source() -> Option<PathBuf> {
    let checkout = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    [
        std::env::var_os("WORK_AGENT_SKILLS").map(PathBuf::from),
        Some(checkout.join(".claude/skills")),
        Some(checkout.join("skills")),
    ]
    .into_iter()
    .flatten()
    .filter_map(|p| std::fs::canonicalize(p).ok())
    .find(|p| p.is_dir())
}

#[derive(Debug, PartialEq)]
enum Linked {
    New,
    Same,
    Occupied,
}

/// Points one link at `skills`: created when missing, replaced when it is a
/// symlink to somewhere else (a moved checkout), and left alone when it already
/// points here or when the profile has a real folder of its own in the way.
fn link_one(skills: &Path, link: &Path) -> Result<Linked> {
    // A profile folder holding the skills (the checkout is a profile candidate
    // too) must never be replaced by a link into itself.
    if skills.starts_with(link) {
        return Ok(Linked::Occupied);
    }
    match std::fs::symlink_metadata(link) {
        Ok(meta) if meta.file_type().is_symlink() => {
            if std::fs::canonicalize(link).ok().as_deref() == Some(skills) {
                return Ok(Linked::Same);
            }
            std::fs::remove_file(link).with_context(|| format!("replacing old link {}", link.display()))?;
        }
        Ok(_) => return Ok(Linked::Occupied),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e).with_context(|| format!("checking {}", link.display())),
    }
    if let Some(parent) = link.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    std::os::unix::fs::symlink(skills, link).with_context(|| format!("linking {}", link.display()))?;
    Ok(Linked::New)
}

/// Appends the patterns for our links to `<profile>/.gitignore`, creating it
/// when the profile has none, so installing leaves no untracked noise behind.
fn ignore(profile: &Path, links: &[&str]) -> Result<()> {
    let file = profile.join(".gitignore");
    let mut text = std::fs::read_to_string(&file).unwrap_or_default();
    let missing: Vec<String> = links
        .iter()
        .map(|rel| format!("/{rel}"))
        .filter(|p| !text.contains(p))
        .collect();
    if missing.is_empty() {
        return Ok(());
    }
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    if !text.contains(IGNORE_NOTE) {
        text.push_str(IGNORE_NOTE);
        text.push('\n');
    }
    for pattern in missing {
        text.push_str(&pattern);
        text.push('\n');
    }
    std::fs::write(&file, text).with_context(|| format!("writing {}", file.display()))?;
    tracing::info!(file = %file.display(), "skills links ignored");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch tree: a fake checkout holding one skill, and an empty profile.
    /// Each test gets its own folder, since tests run in one process.
    fn setup(name: &str) -> (PathBuf, PathBuf, PathBuf) {
        let root = std::env::temp_dir().join(format!("work-agent-skills-{name}"));
        let _ = std::fs::remove_dir_all(&root);
        let skills = root.join("checkout/.claude/skills");
        let profile = root.join("configs/stay22");
        std::fs::create_dir_all(skills.join("add-task")).unwrap();
        std::fs::write(skills.join("add-task/SKILL.md"), "---\nname: add-task\ndescription: d\n---\n").unwrap();
        std::fs::create_dir_all(&profile).unwrap();
        (skills, profile, root)
    }

    fn assert_linked(profile: &Path, skills: &Path) {
        for rel in [".agents/skills", ".claude/skills"] {
            assert_eq!(std::fs::canonicalize(profile.join(rel)).unwrap(), skills.to_path_buf(), "{rel}");
        }
    }

    #[test]
    fn links_both_harness_locations_and_twice_is_a_no_op() {
        let (skills, profile, root) = setup("links");
        link_from(&skills, &profile).unwrap();
        assert_linked(&profile, &skills);
        let gitignore = std::fs::read_to_string(profile.join(".gitignore")).unwrap();
        for (rel, _) in LINKS {
            let pattern = format!("/{rel}");
            assert!(gitignore.contains(&pattern), "{pattern} missing from .gitignore");
        }
        // Second install: same links, and the gitignore is not written twice.
        link_from(&skills, &profile).unwrap();
        assert_linked(&profile, &skills);
        let again = std::fs::read_to_string(profile.join(".gitignore")).unwrap();
        assert_eq!(gitignore, again);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn replaces_a_stale_link_and_keeps_a_real_folder() {
        let (skills, profile, root) = setup("stale");
        std::fs::create_dir_all(profile.join(".agents")).unwrap();
        std::os::unix::fs::symlink("/nowhere", profile.join(".agents/skills")).unwrap();
        std::fs::create_dir_all(profile.join(".claude/skills/my-own")).unwrap();
        link_from(&skills, &profile).unwrap();
        assert_eq!(std::fs::canonicalize(profile.join(".agents/skills")).unwrap(), skills);
        // The user's own folder is left in place, not deleted and not shadowed.
        assert!(profile.join(".claude/skills/my-own").is_dir());
        assert!(!profile.join(".claude/skills").symlink_metadata().unwrap().file_type().is_symlink());
        // ... and it stays visible to git: only the link we made is ignored.
        let gitignore = std::fs::read_to_string(profile.join(".gitignore")).unwrap();
        assert!(gitignore.contains("/.agents/skills"), "{gitignore}");
        assert!(!gitignore.contains("/.claude/skills"), "{gitignore}");
        std::fs::remove_dir_all(root).unwrap();
    }

    /// A profile folder that contains the skills (the checkout itself is a
    /// profile candidate) must never be replaced by a link into itself.
    #[test]
    fn never_links_over_the_skills_themselves() {
        let (skills, _, root) = setup("self");
        assert_eq!(link_one(&skills, &skills).unwrap(), Linked::Occupied);
        assert_eq!(link_one(&skills, skills.parent().unwrap()).unwrap(), Linked::Occupied);
        assert!(skills.join("add-task").is_dir());
        std::fs::remove_dir_all(root).unwrap();
    }

    /// An existing `.gitignore` keeps its content and gains only the two lines.
    #[test]
    fn appends_to_an_existing_gitignore() {
        let (skills, profile, root) = setup("gitignore");
        std::fs::write(profile.join(".gitignore"), "state.json").unwrap();
        link_from(&skills, &profile).unwrap();
        let text = std::fs::read_to_string(profile.join(".gitignore")).unwrap();
        assert!(text.starts_with("state.json\n"), "kept the user's line: {text:?}");
        assert_eq!(text.lines().filter(|l| *l == "/.agents/skills").count(), 1);
        std::fs::remove_dir_all(root).unwrap();
    }
}
