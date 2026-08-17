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
pub const WINDOWS_TASK_NAME: &str = "ai.eideticengine.ee-daemon";
pub const WINDOWS_TASK_FILE_NAME: &str = "ee-daemon.task.xml";

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DaemonServiceKind {
    Launchd,
    SystemdUser,
    WindowsUserTask,
    Unsupported,
}

impl DaemonServiceKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Launchd => "launchd",
            Self::SystemdUser => "systemd_user",
            Self::WindowsUserTask => "windows_user_task",
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
    } else if cfg!(target_os = "windows") {
        DaemonServiceKind::WindowsUserTask
    } else {
        DaemonServiceKind::Unsupported
    }
}

/// User home for service files. Windows soak hosts often have USERPROFILE
/// and no HOME; do not require a Unix-only variable.
#[must_use]
pub fn resolve_user_home() -> Option<PathBuf> {
    resolve_user_home_from_env(|key| std::env::var_os(key))
}

#[must_use]
pub fn resolve_user_home_from_env(
    mut env_var: impl FnMut(&str) -> Option<std::ffi::OsString>,
) -> Option<PathBuf> {
    env_var("HOME")
        .filter(|value| !value.is_empty())
        .or_else(|| env_var("USERPROFILE").filter(|value| !value.is_empty()))
        .map(PathBuf::from)
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
        DaemonServiceKind::WindowsUserTask => Some(
            home.join("AppData")
                .join("Local")
                .join("eidetic-engine")
                .join(WINDOWS_TASK_FILE_NAME),
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
        DaemonServiceKind::WindowsUserTask => Some(render_windows_task_xml(ee_binary)),
        DaemonServiceKind::Unsupported => None,
    }
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn windows_command_path(path: &Path) -> String {
    let text = path.to_string_lossy();
    let trimmed = text
        .strip_prefix(r"\\?\")
        .or_else(|| text.strip_prefix("//?/"))
        .unwrap_or(&text);
    xml_escape(trimmed)
}

fn render_windows_task_xml(ee_binary: &Path) -> String {
    let command = windows_command_path(ee_binary);
    format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo>
    <URI>\{WINDOWS_TASK_NAME}</URI>
    <Description>ee team/mesh steward daemon</Description>
  </RegistrationInfo>
  <Triggers>
    <LogonTrigger>
      <Enabled>true</Enabled>
    </LogonTrigger>
  </Triggers>
  <Principals>
    <Principal id="Author">
      <LogonType>InteractiveToken</LogonType>
      <RunLevel>LeastPrivilege</RunLevel>
    </Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <AllowHardTerminate>true</AllowHardTerminate>
    <StartWhenAvailable>true</StartWhenAvailable>
    <RunOnlyIfNetworkAvailable>false</RunOnlyIfNetworkAvailable>
    <IdleSettings>
      <StopOnIdleEnd>false</StopOnIdleEnd>
      <RestartOnIdle>false</RestartOnIdle>
    </IdleSettings>
    <AllowStartOnDemand>true</AllowStartOnDemand>
    <Enabled>true</Enabled>
    <Hidden>false</Hidden>
    <RunOnlyIfIdle>false</RunOnlyIfIdle>
    <WakeToRun>false</WakeToRun>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <Priority>7</Priority>
    <RestartOnFailure>
      <Interval>PT1M</Interval>
      <Count>3</Count>
    </RestartOnFailure>
  </Settings>
  <Actions Context="Author">
    <Exec>
      <Command>{command}</Command>
      <Arguments>daemon --foreground --job team_steward --job decay_sweep --job health_check</Arguments>
    </Exec>
  </Actions>
</Task>
"#
    )
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
        DaemonServiceKind::WindowsUserTask => vec![
            format!("schtasks /Create /TN {WINDOWS_TASK_NAME} /XML <unit-path> /F"),
            format!("schtasks /Run /TN {WINDOWS_TASK_NAME}"),
            "ee daemon --status --json".to_owned(),
        ],
        DaemonServiceKind::Unsupported => {
            vec!["ee mesh hello-responder run --workspace . --json".to_owned()]
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
        DaemonServiceKind::WindowsUserTask => std::process::Command::new("schtasks")
            .args([
                "/Create",
                "/TN",
                WINDOWS_TASK_NAME,
                "/XML",
                &unit_path.display().to_string(),
                "/F",
            ])
            .output(),
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
        DaemonServiceKind::WindowsUserTask => prove_windows_user_task_load(home, unit_stem),
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

fn prove_windows_user_task_load(home: &Path, unit_stem: &str) -> Result<String, String> {
    let task_name = format!("ai.eideticengine.{unit_stem}");
    let path = home
        .join("AppData")
        .join("Local")
        .join("eidetic-engine")
        .join(format!("{unit_stem}.task.xml"));
    let cmd = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".to_owned());
    let body = format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo>
    <URI>\{task_name}</URI>
    <Description>ee team-confed supervisor proof</Description>
  </RegistrationInfo>
  <Triggers>
    <RegistrationTrigger>
      <Enabled>true</Enabled>
    </RegistrationTrigger>
  </Triggers>
  <Principals>
    <Principal id="Author">
      <LogonType>InteractiveToken</LogonType>
      <RunLevel>LeastPrivilege</RunLevel>
    </Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <AllowStartOnDemand>true</AllowStartOnDemand>
    <Enabled>true</Enabled>
    <Hidden>false</Hidden>
    <ExecutionTimeLimit>PT1M</ExecutionTimeLimit>
  </Settings>
  <Actions Context="Author">
    <Exec>
      <Command>{}\System32\cmd.exe</Command>
      <Arguments>/c exit 0</Arguments>
    </Exec>
  </Actions>
</Task>
"#,
        xml_escape(&cmd)
    );
    write_windows_task_file(&path, &body)?;
    let create = std::process::Command::new("schtasks")
        .args([
            "/Create",
            "/TN",
            &task_name,
            "/XML",
            &path.display().to_string(),
            "/F",
        ])
        .output()
        .map_err(|error| format!("schtasks create: {error}"))?;
    if !create.status.success() {
        let _ = quarantine_unit_file(&path);
        return Err(format!(
            "schtasks create failed: {}",
            String::from_utf8_lossy(&create.stderr)
        ));
    }
    let query = std::process::Command::new("schtasks")
        .args(["/Query", "/TN", &task_name])
        .output()
        .map_err(|error| format!("schtasks query: {error}"))?;
    let _ = std::process::Command::new("schtasks")
        .args(["/Delete", "/TN", &task_name, "/F"])
        .output();
    let _ = quarantine_unit_file(&path);
    if query.status.success() {
        return Ok("active".to_owned());
    }
    Err(format!(
        "windows task was not visible after create: {}",
        String::from_utf8_lossy(&query.stderr)
    ))
}

/// Unregister a previously loaded Windows user task. Unix uninstall still
/// only quarantines the unit file (existing launchd/systemd behavior).
pub fn deactivate_daemon_service(kind: DaemonServiceKind) -> Result<(), String> {
    match kind {
        DaemonServiceKind::WindowsUserTask => {
            let output = std::process::Command::new("schtasks")
                .args(["/Delete", "/TN", WINDOWS_TASK_NAME, "/F"])
                .output()
                .map_err(|error| format!("schtasks delete: {error}"))?;
            if output.status.success() {
                return Ok(());
            }
            let stderr = String::from_utf8_lossy(&output.stderr);
            if stderr.to_ascii_lowercase().contains("cannot find")
                || stderr
                    .to_ascii_lowercase()
                    .contains("the system cannot find")
            {
                return Ok(());
            }
            Err(stderr.trim().to_owned())
        }
        DaemonServiceKind::Launchd
        | DaemonServiceKind::SystemdUser
        | DaemonServiceKind::Unsupported => Ok(()),
    }
}

/// Write the unit file. Callers must have already confirmed.
pub fn write_service_unit(kind: DaemonServiceKind, path: &Path, body: &str) -> Result<(), String> {
    if kind == DaemonServiceKind::WindowsUserTask {
        return write_windows_task_file(path, body);
    }
    write_unit_file(path, body)
}

fn write_windows_task_file(path: &Path, body: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let temp = path.with_extension("tmp");
    let utf16: Vec<u8> = {
        let mut bytes = vec![0xFF, 0xFE];
        for unit in body.encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        bytes
    };
    std::fs::write(&temp, utf16).map_err(|error| error.to_string())?;
    std::fs::rename(&temp, path).map_err(|error| error.to_string())?;
    Ok(())
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
    fn windows_task_xml_is_user_logon_and_escapes_paths() {
        let body = render_unit_body(
            DaemonServiceKind::WindowsUserTask,
            Path::new(r"C:\Program Files\ee & tools\ee.exe"),
        )
        .expect("windows body");
        assert!(body.contains(WINDOWS_TASK_NAME));
        assert!(body.contains("LogonTrigger"));
        assert!(body.contains("team_steward"));
        assert!(body.contains(r"C:\Program Files\ee &amp; tools\ee.exe"));
        assert!(!body.contains(r"\\?\"));
        assert!(body.contains("InteractiveToken"));
        assert!(body.contains("LeastPrivilege"));
    }

    #[test]
    fn windows_task_plan_lives_under_localappdata_not_workspace_ee() {
        let plan = plan_daemon_service(
            "daemon install",
            DaemonServiceKind::WindowsUserTask,
            Path::new(r"C:\Users\jeffr"),
            Path::new(r"C:\Users\jeffr\ee.exe"),
            false,
        );
        let path = PathBuf::from(plan.unit_path.expect("path"));
        let parts: Vec<_> = path
            .components()
            .filter_map(|part| part.as_os_str().to_str())
            .collect();
        assert!(
            parts
                .windows(3)
                .any(|window| window == ["AppData", "Local", "eidetic-engine"])
        );
        assert!(path.file_name().and_then(|name| name.to_str()) == Some(WINDOWS_TASK_FILE_NAME));
        assert!(!parts.iter().any(|part| *part == ".ee"));
        assert!(plan.unit_body.is_some());
        assert!(
            plan.next_commands
                .iter()
                .any(|cmd| cmd.contains("schtasks"))
        );
    }

    #[test]
    fn resolve_user_home_prefers_home_then_userprofile() {
        let home = resolve_user_home_from_env(|key| match key {
            "HOME" => Some("/Users/jeff".into()),
            "USERPROFILE" => Some(r"C:\Users\jeff".into()),
            _ => None,
        })
        .expect("home");
        assert_eq!(home, PathBuf::from("/Users/jeff"));
        let profile = resolve_user_home_from_env(|key| match key {
            "USERPROFILE" => Some(r"C:\Users\jeff".into()),
            _ => None,
        })
        .expect("profile");
        assert_eq!(profile, PathBuf::from(r"C:\Users\jeff"));
        assert!(resolve_user_home_from_env(|_| None).is_none());
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
                .any(|cmd| cmd.contains("hello-responder"))
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
    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
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
            DaemonServiceKind::WindowsUserTask => std::process::Command::new("schtasks")
                .args(["/Query"])
                .output()
                .is_ok_and(|output| output.status.success()),
            DaemonServiceKind::Unsupported => false,
        };
        if !ready {
            return;
        }
        let home = resolve_user_home().expect("HOME or USERPROFILE");
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
