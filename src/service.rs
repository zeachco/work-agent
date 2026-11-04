//! `work-agent service`: install the daemon as systemd user services, one
//! unit per profile. `install a b c` makes the installed set exactly those
//! three profiles (other `work-agent-*` units are removed unless asked to
//! keep them).

use anyhow::{Context, Result};
use clap::Subcommand;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Subcommand)]
pub enum ServiceCmd {
    /// Write, enable and start one `work-agent-<folder>.service` per profile.
    /// Missing folders are created (scaffolded).
    Install {
        /// Profile folders the daemons load, e.g. `~/.config/work-agent/default`.
        #[arg(required = true)]
        profiles: Vec<PathBuf>,
        /// Leave `work-agent-*.service` units of other profiles running
        /// instead of removing them first.
        #[arg(long)]
        keep_others: bool,
    },
    /// Stop, disable and remove the units of these profiles.
    Uninstall {
        #[arg(required = true)]
        profiles: Vec<PathBuf>,
    },
    /// `systemctl --user status` of the units of these profiles (all installed ones when none is given).
    Status { profiles: Vec<PathBuf> },
}

pub fn run(cmd: ServiceCmd) -> Result<()> {
    match cmd {
        ServiceCmd::Install { profiles, keep_others } => install(&profiles, keep_others),
        ServiceCmd::Uninstall { profiles } => {
            for p in &profiles {
                remove_unit(&unit_name(p)?)?;
            }
            systemctl(&["daemon-reload"])
        }
        ServiceCmd::Status { profiles } => {
            let units = if profiles.is_empty() {
                installed_units()?
            } else {
                profiles.iter().map(|p| unit_name(p)).collect::<Result<_>>()?
            };
            anyhow::ensure!(!units.is_empty(), "no work-agent service installed");
            let mut args = vec!["status", "--no-pager"];
            args.extend(units.iter().map(String::as_str));
            systemctl(&args)
        }
    }
}

fn install(profiles: &[PathBuf], keep_others: bool) -> Result<()> {
    // Resolve everything first so a bad path aborts before anything is removed.
    let mut wanted = Vec::new();
    for profile in profiles {
        crate::config::scaffold(profile)?;
        let profile = std::fs::canonicalize(profile)?;
        let unit = unit_name(&profile)?;
        anyhow::ensure!(
            !wanted.iter().any(|(_, u)| *u == unit),
            "two profiles share the folder name behind {unit}; rename one of them"
        );
        wanted.push((profile, unit));
    }
    if !keep_others {
        for other in installed_units()? {
            if !wanted.iter().any(|(_, u)| *u == other) {
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
    for (profile, unit) in &wanted {
        let body = format!(
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
            path,
        );
        let file = dir.join(unit);
        std::fs::write(&file, body).with_context(|| format!("writing {}", file.display()))?;
    }
    systemctl(&["daemon-reload"])?;
    for (profile, unit) in &wanted {
        // `restart` so an already-installed profile picks up the rewritten unit.
        systemctl(&["enable", unit])?;
        systemctl(&["restart", unit])?;
        println!("installed {unit} -> {} (binary {})", profile.display(), exe.display());
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
    let home = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .context("neither XDG_CONFIG_HOME nor HOME is set")?;
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
