//! User-scoped launchd/systemd unit render + install plan for T6.2.
//!
//! `ee daemon install` never requires root. Confirm writes go through
//! doctor-runtime when a workspace is available; dry-run only reports the
//! planned unit path and bytes. Uninstall quarantines by rename.

use std::path::{Path, PathBuf};

use serde::Serialize;

pub const DAEMON_SERVICE_SCHEMA_V1: &str = "ee.daemon.service.v1";
pub const LAUNCHD_LABEL: &str = "ai.eideticengine.ee-daemon";
pub const SYSTEMD_UNIT_NAME: &str = "ee-daemon.service";

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DaemonServiceKind {
    Launchd,
    SystemdUser,
    Unsupported,
}

impl DaemonServiceKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Launchd => "launchd",
            Self::SystemdUser => "systemd_user",
            Self::Unsupported => "unsupported",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DaemonServicePlan {
    pub schema: &'static str,
    pub command: &'static str,
    pub kind: DaemonServiceKind,
    pub unit_path: Option<String>,
    pub unit_body: Option<String>,
    pub already_installed: bool,
    pub confirmed: bool,
    pub written: bool,
    pub loaded: bool,
    pub load_output: Option<String>,
    pub next_commands: Vec<String>,
    pub mesh_primitives: Vec<&'static str>,
}

#[must_use]
pub fn current_service_kind() -> DaemonServiceKind {
    if cfg!(target_os = "macos") {
        DaemonServiceKind::Launchd
    } else if cfg!(target_os = "linux") {
        DaemonServiceKind::SystemdUser
    } else {
        DaemonServiceKind::Unsupported
    }
}

#[must_use]
pub fn default_unit_path(kind: DaemonServiceKind, home: &Path) -> Option<PathBuf> {
    match kind {
        DaemonServiceKind::Launchd => Some(
            home.join("Library")
                .join("LaunchAgents")
                .join(format!("{LAUNCHD_LABEL}.plist")),
        ),
        DaemonServiceKind::SystemdUser => Some(
            home.join(".config")
                .join("systemd")
                .join("user")
                .join(SYSTEMD_UNIT_NAME),
        ),
        DaemonServiceKind::Unsupported => None,
    }
}

#[must_use]
pub fn render_unit_body(kind: DaemonServiceKind, ee_binary: &Path) -> Option<String> {
    let binary = ee_binary.display();
    match kind {
        DaemonServiceKind::Launchd => Some(format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{LAUNCHD_LABEL}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{binary}</string>
    <string>daemon</string>
    <string>--foreground</string>
    <string>--job</string>
    <string>team_steward</string>
    <string>--job</string>
    <string>decay_sweep</string>
    <string>--job</string>
    <string>health_check</string>
  </array>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <true/>
</dict>
</plist>
"#
        )),
        DaemonServiceKind::SystemdUser => Some(format!(
            "[Unit]\nDescription=ee team/mesh steward daemon\nAfter=default.target\n\n[Service]\nType=simple\nExecStart={binary} daemon --foreground --job team_steward --job decay_sweep --job health_check\nRestart=on-failure\nRestartSec=5\n\n[Install]\nWantedBy=default.target\n"
        )),
        DaemonServiceKind::Unsupported => None,
    }
}

#[must_use]
pub fn plan_daemon_service(
    command: &'static str,
    kind: DaemonServiceKind,
    home: &Path,
    ee_binary: &Path,
    confirm: bool,
) -> DaemonServicePlan {
    let unit_path = default_unit_path(kind, home);
    let unit_body = render_unit_body(kind, ee_binary);
    let already_installed = unit_path.as_ref().is_some_and(|path| path.is_file());
    let next_commands = match kind {
        DaemonServiceKind::Launchd => {
            if let Some(path) = unit_path.as_ref() {
                vec![
                    format!("launchctl load {}", path.display()),
                    "ee daemon --status --json".to_owned(),
                ]
            } else {
                Vec::new()
            }
        }
        DaemonServiceKind::SystemdUser => vec![
            "systemctl --user daemon-reload".to_owned(),
            format!("systemctl --user enable --now {SYSTEMD_UNIT_NAME}"),
            "ee daemon --status --json".to_owned(),
        ],
        DaemonServiceKind::Unsupported => {
            vec!["Windows remains client-only until credential-store parity lands".to_owned()]
        }
    };
    DaemonServicePlan {
        schema: DAEMON_SERVICE_SCHEMA_V1,
        command,
        kind,
        unit_path: unit_path.map(|path| path.display().to_string()),
        unit_body,
        already_installed,
        confirmed: confirm,
        written: false,
        loaded: false,
        load_output: None,
        next_commands,
        mesh_primitives: vec!["team_steward", "daemon_service"],
    }
}

/// Load the written unit into launchd or systemd --user.
pub fn activate_daemon_service(
    kind: DaemonServiceKind,
    unit_path: &Path,
) -> Result<String, String> {
    let output = match kind {
        DaemonServiceKind::Launchd => std::process::Command::new("launchctl")
            .args(["load", &unit_path.display().to_string()])
            .output(),
        DaemonServiceKind::SystemdUser => {
            let reload = std::process::Command::new("systemctl")
                .args(["--user", "daemon-reload"])
                .output()
                .map_err(|error| format!("systemctl daemon-reload: {error}"))?;
            if !reload.status.success() {
                return Err(format!(
                    "systemctl daemon-reload failed: {}",
                    String::from_utf8_lossy(&reload.stderr)
                ));
            }
            std::process::Command::new("systemctl")
                .args(["--user", "enable", "--now", SYSTEMD_UNIT_NAME])
                .output()
        }
        DaemonServiceKind::Unsupported => {
            return Err("no user service supervisor on this platform".to_owned());
        }
    };
    let output = output.map_err(|error| error.to_string())?;
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    if !output.status.success() {
        return Err(if stderr.is_empty() { stdout } else { stderr });
    }
    Ok(if stdout.is_empty() { stderr } else { stdout })
}

/// Start a uniquely named oneshot user unit, assert it became active, then
/// stop it and quarantine the unit file. Proves the T6.2 supervisor-load path
/// without taking over [`SYSTEMD_UNIT_NAME`] or [`LAUNCHD_LABEL`].
pub fn prove_user_supervisor_load(home: &Path, unit_stem: &str) -> Result<String, String> {
    if unit_stem.is_empty()
        || unit_stem.len() > 48
        || !unit_stem
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
    {
        return Err("unit stem is not a safe service name".to_owned());
    }
    match current_service_kind() {
        DaemonServiceKind::SystemdUser => prove_systemd_user_supervisor_load(home, unit_stem),
        DaemonServiceKind::Launchd => prove_launchd_user_supervisor_load(home, unit_stem),
        DaemonServiceKind::Unsupported => {
            Err("no user service supervisor on this platform".to_owned())
        }
    }
}

fn prove_systemd_user_supervisor_load(home: &Path, unit_stem: &str) -> Result<String, String> {
    let unit_name = format!("{unit_stem}.service");
    let path = home
        .join(".config")
        .join("systemd")
        .join("user")
        .join(&unit_name);
    write_unit_file(
        &path,
        "[Unit]\nDescription=ee team-confed supervisor proof\n\n[Service]\nType=oneshot\nRemainAfterExit=yes\nExecStart=/bin/true\n\n[Install]\nWantedBy=default.target\n",
    )?;
    let reload = std::process::Command::new("systemctl")
        .args(["--user", "daemon-reload"])
        .output()
        .map_err(|error| format!("systemctl daemon-reload: {error}"))?;
    if !reload.status.success() {
        return Err(format!(
            "systemctl daemon-reload failed: {}",
            String::from_utf8_lossy(&reload.stderr)
        ));
    }
    let start = std::process::Command::new("systemctl")
        .args(["--user", "start", &unit_name])
        .output()
        .map_err(|error| format!("systemctl start: {error}"))?;
    if !start.status.success() {
        return Err(format!(
            "systemctl start failed: {}",
            String::from_utf8_lossy(&start.stderr)
        ));
    }
    let active = std::process::Command::new("systemctl")
        .args(["--user", "is-active", &unit_name])
        .output()
        .map_err(|error| format!("systemctl is-active: {error}"))?;
    let status = String::from_utf8_lossy(&active.stdout).trim().to_owned();
    let _ = std::process::Command::new("systemctl")
        .args(["--user", "stop", &unit_name])
        .output();
    let _ = quarantine_unit_file(&path);
    if status != "active" {
        return Err(format!("unit was not active after start: {status}"));
    }
    Ok(status)
}

fn prove_launchd_user_supervisor_load(home: &Path, unit_stem: &str) -> Result<String, String> {
    let label = format!("ai.eideticengine.{unit_stem}");
    let path = home
        .join("Library")
        .join("LaunchAgents")
        .join(format!("{label}.plist"));
    write_unit_file(
        &path,
        &format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{label}</string>
  <key>ProgramArguments</key>
  <array>
    <string>/usr/bin/true</string>
  </array>
  <key>RunAtLoad</key>
  <true/>
</dict>
</plist>
"#
        ),
    )?;
    let uid = std::process::Command::new("id")
        .arg("-u")
        .output()
        .ok()
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|text| text.trim().to_owned())
        .unwrap_or_else(|| "501".to_owned());
    let domain = format!("gui/{uid}");
    let service = format!("{domain}/{label}");
    let _ = std::process::Command::new("launchctl")
        .args(["bootout", &service])
        .output();
    let bootstrap = std::process::Command::new("launchctl")
        .args(["bootstrap", &domain, &path.display().to_string()])
        .output()
        .map_err(|error| format!("launchctl bootstrap: {error}"))?;
    if !bootstrap.status.success() {
        let load = std::process::Command::new("launchctl")
            .args(["load", &path.display().to_string()])
            .output()
            .map_err(|error| format!("launchctl load: {error}"))?;
        if !load.status.success() {
            let _ = quarantine_unit_file(&path);
            return Err(format!(
                "launchctl bootstrap/load failed: {} {}",
                String::from_utf8_lossy(&bootstrap.stderr),
                String::from_utf8_lossy(&load.stderr)
            ));
        }
    }
    let printed = std::process::Command::new("launchctl")
        .args(["print", &service])
        .output()
        .map_err(|error| format!("launchctl print: {error}"))?;
    let listed = std::process::Command::new("launchctl")
        .args(["list", &label])
        .output()
        .map_err(|error| format!("launchctl list: {error}"))?;
    let _ = std::process::Command::new("launchctl")
        .args(["bootout", &service])
        .output();
    let _ = std::process::Command::new("launchctl")
        .args(["unload", &path.display().to_string()])
        .output();
    let _ = quarantine_unit_file(&path);
    if printed.status.success() || listed.status.success() {
        return Ok("active".to_owned());
    }
    Err(format!(
        "launchd unit was not visible after load: {}",
        String::from_utf8_lossy(&printed.stderr)
    ))
}

/// Write the unit file. Callers must have already confirmed.
pub fn write_unit_file(path: &Path, body: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let temp = path.with_extension("tmp");
    std::fs::write(&temp, body).map_err(|error| error.to_string())?;
    std::fs::rename(&temp, path).map_err(|error| error.to_string())?;
    Ok(())
}

/// Quarantine the unit file by rename. Never deletes.
pub fn quarantine_unit_file(path: &Path) -> Result<PathBuf, String> {
    let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%SZ");
    let dest = path.with_extension(format!("quarantined-{stamp}"));
    std::fs::rename(path, &dest).map_err(|error| error.to_string())?;
    Ok(dest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launchd_unit_mentions_team_steward_and_label() {
        let body = render_unit_body(DaemonServiceKind::Launchd, Path::new("/usr/local/bin/ee"))
            .expect("launchd body");
        assert!(body.contains(LAUNCHD_LABEL));
        assert!(body.contains("team_steward"));
        assert!(body.contains("/usr/local/bin/ee"));
        assert!(body.contains("KeepAlive"));
    }

    #[test]
    fn systemd_unit_is_user_scoped_and_restarts() {
        let body = render_unit_body(DaemonServiceKind::SystemdUser, Path::new("/usr/bin/ee"))
            .expect("systemd body");
        assert!(body.contains("ExecStart=/usr/bin/ee daemon --foreground"));
        assert!(body.contains("team_steward"));
        assert!(body.contains("[Install]"));
        assert!(body.contains("WantedBy=default.target"));
    }

    #[test]
    fn unsupported_kind_has_no_unit_path() {
        let plan = plan_daemon_service(
            "daemon install",
            DaemonServiceKind::Unsupported,
            Path::new("/tmp/home"),
            Path::new("/usr/bin/ee"),
            false,
        );
        assert!(plan.unit_path.is_none());
        assert!(!plan.written);
        assert!(!plan.loaded);
        assert!(
            plan.next_commands
                .iter()
                .any(|cmd| cmd.contains("client-only"))
        );
    }

    #[test]
    fn write_then_quarantine_leaves_the_original_path_empty() {
        let dir = tempfile::tempdir().expect("temp");
        let path = dir.path().join("ee-daemon.service");
        write_unit_file(&path, "[Unit]\nDescription=test\n").expect("write");
        assert!(path.is_file());
        let quarantined = quarantine_unit_file(&path).expect("quarantine");
        assert!(!path.exists());
        assert!(quarantined.is_file());
        assert!(
            quarantined
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.contains("quarantined-"))
        );
    }

    #[test]
    fn prove_user_supervisor_load_refuses_unsafe_stem() {
        let error =
            prove_user_supervisor_load(Path::new("/tmp"), "../escape").expect_err("unsafe stem");
        assert!(error.contains("safe service name"));
    }

    #[test]
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn prove_user_supervisor_load_when_user_manager_exists() {
        let ready = match current_service_kind() {
            DaemonServiceKind::SystemdUser => std::process::Command::new("systemctl")
                .args(["--user", "is-system-running"])
                .output()
                .is_ok_and(|output| output.status.success()),
            DaemonServiceKind::Launchd => {
                std::path::Path::new("/bin/launchctl").is_file()
                    || std::path::Path::new("/usr/bin/launchctl").is_file()
            }
            DaemonServiceKind::Unsupported => false,
        };
        if !ready {
            return;
        }
        let home = std::path::PathBuf::from(std::env::var_os("HOME").expect("HOME"));
        let status =
            prove_user_supervisor_load(&home, "ee-team-confed-proof").expect("supervisor load");
        assert_eq!(status, "active");
    }

    #[test]
    fn activate_unsupported_is_refused() {
        let error = activate_daemon_service(
            DaemonServiceKind::Unsupported,
            Path::new("/tmp/ee-daemon.service"),
        )
        .expect_err("unsupported");
        assert!(error.contains("no user service"));
    }
}
