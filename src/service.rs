//! `work-agent service`: install the daemon as a systemd user service, one
//! unit per profile so several profiles can run side by side.

use anyhow::{Context, Result};
use clap::Subcommand;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Subcommand)]
pub enum ServiceCmd {
    /// Write, enable and start `work-agent-<name>.service` for a profile folder.
    /// The folder is created (scaffolded) when it does not exist yet.
    Install {
        /// Profile folder the daemon loads, e.g. `~/.config/work-agent/default`.
        profile: PathBuf,
        /// Unit suffix; defaults to the profile folder's name.
        #[arg(long)]
        name: Option<String>,
    },
    /// Stop, disable and remove the unit of a profile.
    Uninstall { profile: PathBuf, #[arg(long)] name: Option<String> },
    /// `systemctl --user status` of the unit of a profile.
    Status { profile: PathBuf, #[arg(long)] name: Option<String> },
}

pub fn run(cmd: ServiceCmd) -> Result<()> {
    match cmd {
        ServiceCmd::Install { profile, name } => install(&profile, name),
        ServiceCmd::Uninstall { profile, name } => uninstall(&profile, name),
        ServiceCmd::Status { profile, name } => {
            let unit = unit_name(&profile, name)?;
            systemctl(&["status", "--no-pager", &unit])
        }
    }
}

fn install(profile: &Path, name: Option<String>) -> Result<()> {
    crate::config::scaffold(profile)?;
    let profile = std::fs::canonicalize(profile)?;
    let unit = unit_name(&profile, name)?;
    let exe = std::env::current_exe().context("locating the work-agent binary")?;
    let exe = std::fs::canonicalize(&exe).unwrap_or(exe);
    let path = std::env::var("PATH").unwrap_or_default();
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
    let file = unit_dir()?.join(&unit);
    std::fs::create_dir_all(file.parent().unwrap())?;
    std::fs::write(&file, body).with_context(|| format!("writing {}", file.display()))?;
    systemctl(&["daemon-reload"])?;
    systemctl(&["enable", "--now", &unit])?;
    println!("installed {} -> {} (binary {})", file.display(), profile.display(), exe.display());
    println!("logs: journalctl --user -fu {unit}");
    Ok(())
}

fn uninstall(profile: &Path, name: Option<String>) -> Result<()> {
    let unit = unit_name(profile, name)?;
    let _ = systemctl(&["disable", "--now", &unit]);
    let file = unit_dir()?.join(&unit);
    match std::fs::remove_file(&file) {
        Ok(()) => println!("removed {}", file.display()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => println!("{} was not installed", unit),
        Err(e) => return Err(e).with_context(|| format!("removing {}", file.display())),
    }
    systemctl(&["daemon-reload"])
}

/// `work-agent-<name>.service`, `<name>` being the profile folder's name.
fn unit_name(profile: &Path, name: Option<String>) -> Result<String> {
    let name = match name {
        Some(n) => n,
        None => profile
            .file_name()
            .and_then(|n| n.to_str())
            .map(str::to_string)
            .context("profile path has no folder name; pass --name")?,
    };
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
