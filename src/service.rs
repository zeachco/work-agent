//! `work-agent service a b c`: install the daemon as systemd user services,
//! one unit per profile, making the installed set exactly those profiles
//! (other `work-agent-*` units are removed unless asked to keep them).
//! With no path the profile in hand is the only one touched, which is what bare
//! `work-agent` in a profile folder does.

use crate::config::Config;
use anyhow::{Context, Result};
use clap::Args;
use std::path::{Path, PathBuf};
use std::process::Command;

/// `work-agent service <profile>...`: install one daemon per profile.
#[derive(Args)]
pub struct ServiceArgs {
    /// Profile folders, e.g. `~/.config/work-agent/default ~/dev/private-profile`.
    /// Missing folders are created (scaffolded). One `work-agent-<folder>.service`
    /// each; other installed units are removed unless `--keep-others`. Omit them
    /// to act on the profile in hand (the current folder when it is one), which
    /// never touches the other daemons.
    #[arg(num_args = 0..)]
    profiles: Vec<PathBuf>,
    /// Leave `work-agent-*.service` units of other profiles running
    /// instead of removing them first.
    #[arg(long, conflicts_with_all = ["uninstall", "status"])]
    keep_others: bool,
    /// Stop, disable and remove the units of these profiles instead of installing.
    #[arg(long, conflicts_with = "status")]
    uninstall: bool,
    /// `systemctl --user status` of these profiles' units (every installed one when no path is given).
    #[arg(long)]
    status: bool,
    /// With `--uninstall`: remove every installed `work-agent-*.service`.
    #[arg(long, requires = "uninstall", conflicts_with = "profiles")]
    all: bool,
}

/// Installs, uninstalls or inspects the given profiles' daemons. `fallback` is
/// the profile to act on when no path was given (the one the user pointed at),
/// `None` when there is no such profile.
pub fn run(args: ServiceArgs, fallback: Option<PathBuf>) -> Result<()> {
    if args.status {
        let units = target_units(&args.profiles, args.profiles.is_empty())?;
        anyhow::ensure!(!units.is_empty(), "no work-agent service installed");
        let mut argv = vec!["status", "--no-pager"];
        argv.extend(units.iter().map(String::as_str));
        return systemctl(&argv);
    }
    // No path: the profile in hand, and the other daemons stay as they are,
    // because nothing here asked to replace the installed set.
    let only = args.profiles.is_empty();
    let profiles = match only {
        true => vec![fallback.context(
            "no profile folder given and the current folder is not a profile (no `tasks/` or `config.toml`); pass one",
        )?],
        false => args.profiles.clone(),
    };
    if args.uninstall {
        let units = target_units(&profiles, args.all)?;
        if units.is_empty() {
            println!("no work-agent service installed");
        }
        for unit in &units {
            remove_unit(unit)?;
        }
        return systemctl(&["daemon-reload"]);
    }
    install(&profiles, args.keep_others || only)
}

/// Bare `work-agent` inside a profile folder: install (or refresh) that one
/// daemon, leave every other daemon alone, and link this checkout's skills into
/// the folder. Idempotent, so it is safe to run it as the way to deploy a profile.
pub fn install_here(profile: &Path) -> Result<()> {
    let profile = crate::config::expand_home(profile);
    install(std::slice::from_ref(&profile), true)?;
    println!("foreground run: work-agent daemon --profile {}", profile.display());
    println!("every daemon: work-agent service --status");
    Ok(())
}

/// Every installed unit when `use_installed`, otherwise one unit per given profile.
fn target_units(profiles: &[PathBuf], use_installed: bool) -> Result<Vec<String>> {
    if use_installed {
        installed_units()
    } else {
        profiles.iter().map(|p| unit_name(p)).collect()
    }
}

/// Installs one daemon per profile: unit written, skills linked into the
/// profile, daemon enabled and restarted. `keep_others` leaves the units of
/// profiles not listed here alone.
fn install(profiles: &[PathBuf], keep_others: bool) -> Result<()> {
    // Resolve and validate everything first, so a bad path or a bad task file
    // aborts before anything installed is removed.
    let mut wanted: Vec<(PathBuf, String, usize)> = Vec::new();
    for profile in profiles {
        let profile = crate::config::expand_home(profile);
        // `load` scaffolds a folder that is not there yet, and rejects a task
        // file that is not, so nothing installed changes for a bad profile.
        let cfg = Config::load(&profile).with_context(|| format!("profile {}", profile.display()))?;
        let unit = unit_name(&cfg.profile)?;
        anyhow::ensure!(
            !wanted.iter().any(|(_, u, _)| *u == unit),
            "two profiles share the folder name behind {unit}; rename one of them"
        );
        wanted.push((cfg.profile, unit, cfg.tasks.len()));
    }
    if !keep_others {
        for other in installed_units()? {
            if !wanted.iter().any(|(_, u, _)| *u == other) {
                println!("removing other daemon {other}");
                remove_unit(&other)?;
            }
        }
    }
    let exe = std::env::current_exe().context("locating the work-agent binary")?;
    let exe = std::fs::canonicalize(&exe).unwrap_or(exe);
    let path = std::env::var("PATH").unwrap_or_default();
    let dir = unit_dir()?;
    std::fs::create_dir_all(&dir)?;
    let mut fresh = Vec::new();
    for (profile, unit, tasks) in &wanted {
        let file = dir.join(unit);
        let verb = if file.is_file() { "reinstalled" } else { "installed" };
        std::fs::write(&file, render_unit(profile, &exe, &path))
            .with_context(|| format!("writing {}", file.display()))?;
        crate::skills::link_into(profile)?;
        fresh.push((profile.as_path(), unit.as_str(), *tasks, verb));
    }
    systemctl(&["daemon-reload"])?;
    for (profile, unit, tasks, verb) in fresh {
        // `restart` so an already-installed profile picks up the rewritten unit.
        systemctl(&["enable", unit])?;
        systemctl(&["restart", unit])?;
        println!("{verb} {unit} -> {} ({} task(s), binary {})", profile.display(), tasks, exe.display());
        println!("logs: journalctl --user -fu {unit}");
    }
    Ok(())
}

/// Stops and disables a unit and deletes its file; a unit that was never
/// installed is not an error.
fn remove_unit(unit: &str) -> Result<()> {
    let _ = systemctl(&["disable", "--now", unit]);
    let file = unit_dir()?.join(unit);
    match std::fs::remove_file(&file) {
        Ok(()) => println!("removed {}", file.display()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => println!("{unit} was not installed"),
        Err(e) => return Err(e).with_context(|| format!("removing {}", file.display())),
    }
    Ok(())
}

/// Every `work-agent-*.service` file in the user unit folder.
/// The systemd user unit text for one profile's daemon.
fn render_unit(profile: &Path, exe: &Path, path_env: &str) -> String {
    format!(
        "[Unit]\n\
         Description=work-agent daemon ({})\n\
         After=default.target\n\
         \n\
         [Service]\n\
         ExecStart={} --profile {} daemon\n\
         Environment=PATH={}\n\
         Restart=on-failure\n\
         RestartSec=10\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n",
        profile.display(),
        exe.display(),
        profile.display(),
        path_env,
    )
}

fn installed_units() -> Result<Vec<String>> {
    let dir = unit_dir()?;
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e).with_context(|| format!("reading {}", dir.display())),
    };
    let mut units: Vec<String> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_file())
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| n.starts_with("work-agent-") && n.ends_with(".service"))
        .collect();
    units.sort();
    Ok(units)
}

/// `work-agent-<folder name>.service`.
fn unit_name(profile: &Path) -> Result<String> {
    let name = profile
        .file_name()
        .and_then(|n| n.to_str())
        .with_context(|| format!("profile path {} has no folder name", profile.display()))?;
    Ok(format!("work-agent-{name}.service"))
}

fn unit_dir() -> Result<PathBuf> {
    let home = crate::config::user_config_home().context("neither XDG_CONFIG_HOME nor HOME is set")?;
    Ok(home.join("systemd").join("user"))
}

fn systemctl(args: &[&str]) -> Result<()> {
    let status = Command::new("systemctl")
        .arg("--user")
        .args(args)
        .status()
        .context("running systemctl --user (is systemd available?)")?;
    anyhow::ensure!(status.success(), "systemctl --user {} exited with {status}", args.join(" "));
    Ok(())
}
