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
        next_commands,
        mesh_primitives: vec!["team_steward", "daemon_service"],
    }
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
}
