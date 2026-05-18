//! Tailscale local-probe model, parsers, and narrow probe runners for SRR6.46.1.
//!
//! The classification layer stays deterministic and testable: system I/O is
//! isolated behind small runner traits so status/doctor surfaces can share one
//! interpretation path.

use std::fs;
use std::io::{self, Read, Write};
#[cfg(unix)]
use std::os::unix::fs::FileTypeExt;
#[cfg(unix)]
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;

pub const TAILSCALE_LOCAL_SCHEMA_V1: &str = "ee.tailscale.local.v1";

pub const TAILSCALE_NOT_INSTALLED_CODE: &str = "tailscale_not_installed";
pub const TAILSCALE_DAEMON_UNREACHABLE_CODE: &str = "tailscale_daemon_unreachable";
pub const TAILSCALE_NOT_AUTHENTICATED_CODE: &str = "tailscale_not_authenticated";
pub const TAILSCALE_BINARY_INAUTHENTIC_CODE: &str = "tailscale_binary_inauthentic";
pub const TAILSCALE_SHIELDS_UP_CODE: &str = "tailscale_shields_up";
pub const TAILSCALE_PROBE_UNAVAILABLE_CODE: &str = "tailscale_probe_unavailable";
pub const TAILSCALE_PROBE_TIMEOUT_CODE: &str = "tailscale_probe_timeout";

pub const DEFAULT_TAILSCALE_PROBE_TIMEOUT_MS: u64 = 1_500;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TailscaleProbeMethod {
    Socket,
    Cli,
    Skipped,
}

impl TailscaleProbeMethod {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Socket => "socket",
            Self::Cli => "cli",
            Self::Skipped => "skipped",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TailscalePlatform {
    Linux,
    MacosSandboxed,
    MacosOpen,
    Windows,
    Other,
}

impl TailscalePlatform {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Linux => "linux",
            Self::MacosSandboxed => "macos_sandboxed",
            Self::MacosOpen => "macos_open",
            Self::Windows => "windows",
            Self::Other => "other",
        }
    }

    #[must_use]
    pub fn parse(value: Option<&str>) -> Self {
        match value.unwrap_or_default().to_ascii_lowercase().as_str() {
            "linux" => Self::Linux,
            "macos_sandboxed" => Self::MacosSandboxed,
            "macos_open" | "darwin" | "macos" => Self::MacosOpen,
            "windows" => Self::Windows,
            _ => Self::Other,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TailscaleProbeDegradation {
    pub code: &'static str,
    pub severity: &'static str,
    pub message: String,
    pub repair: &'static str,
}

impl TailscaleProbeDegradation {
    #[must_use]
    pub fn new(
        code: &'static str,
        severity: &'static str,
        message: impl Into<String>,
        repair: &'static str,
    ) -> Self {
        Self {
            code,
            severity,
            message: message.into(),
            repair,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TailscaleBinaryReport {
    pub path: PathBuf,
    pub version_raw: String,
    pub authentic: bool,
    pub parsed_version: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TailscalePeerReport {
    pub node_key: String,
    pub tailscale_ips: Vec<String>,
    pub magic_dns_name: Option<String>,
    pub hostname: Option<String>,
    pub advertised_tags: Vec<String>,
    pub online: Option<bool>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TailscaleLocalReport {
    pub schema: &'static str,
    pub installed: bool,
    pub daemon_reachable: bool,
    pub authenticated: bool,
    pub binary_authentic: bool,
    pub binary_version_raw: Option<String>,
    pub binary_absolute_path: Option<PathBuf>,
    pub shields_up: Option<bool>,
    pub tailnet_id: Option<String>,
    pub tailnet_display_name: Option<String>,
    pub self_node_key: Option<String>,
    pub self_tailscale_ip: Option<String>,
    pub self_magic_dns_name: Option<String>,
    pub self_advertised_tags: Vec<String>,
    pub peers: Vec<TailscalePeerReport>,
    pub version: Option<String>,
    pub probe_method: TailscaleProbeMethod,
    pub probe_elapsed_ms: u64,
    pub platform: TailscalePlatform,
    pub degradations: Vec<TailscaleProbeDegradation>,
}

impl TailscaleLocalReport {
    #[must_use]
    pub fn mesh_disabled() -> Self {
        let mut report = Self::base(TailscaleProbeMethod::Skipped, 0, TailscalePlatform::Other);
        report.degradations.push(TailscaleProbeDegradation::new(
            TAILSCALE_PROBE_UNAVAILABLE_CODE,
            "info",
            "Tailscale probe skipped because mesh is disabled.",
            "Set EE_MESH_ENABLED=1 to enable optional mesh-memory probes.",
        ));
        report
    }

    #[must_use]
    pub fn not_installed(method: TailscaleProbeMethod, elapsed_ms: u64) -> Self {
        let mut report = Self::base(method, elapsed_ms, TailscalePlatform::Other);
        report.degradations.push(TailscaleProbeDegradation::new(
            TAILSCALE_NOT_INSTALLED_CODE,
            "warning",
            "Tailscale binary and local daemon socket were not found.",
            "Install Tailscale, then run tailscale up if you want optional mesh memory.",
        ));
        report
    }

    #[must_use]
    pub fn daemon_unreachable(
        method: TailscaleProbeMethod,
        elapsed_ms: u64,
        detail: impl Into<String>,
    ) -> Self {
        let mut report = Self::base(method, elapsed_ms, TailscalePlatform::Other);
        report.installed = true;
        report.degradations.push(TailscaleProbeDegradation::new(
            TAILSCALE_DAEMON_UNREACHABLE_CODE,
            "warning",
            format!("Tailscale daemon was not reachable: {}", detail.into()),
            "Run tailscale status and inspect the local tailscaled service.",
        ));
        report
    }

    #[must_use]
    pub fn timed_out(method: TailscaleProbeMethod, elapsed_ms: u64) -> Self {
        let mut report = Self::base(method, elapsed_ms, TailscalePlatform::Other);
        report.installed = true;
        report.degradations.push(TailscaleProbeDegradation::new(
            TAILSCALE_PROBE_TIMEOUT_CODE,
            "warning",
            format!(
                "Tailscale probe exceeded the {DEFAULT_TAILSCALE_PROBE_TIMEOUT_MS}ms default budget."
            ),
            "Run tailscale status directly or raise EE_TAILSCALE_PROBE_TIMEOUT_MS.",
        ));
        report
    }

    #[must_use]
    pub fn binary_inauthentic(
        path: PathBuf,
        version_raw: impl Into<String>,
        elapsed_ms: u64,
        detail: impl Into<String>,
    ) -> Self {
        let mut report = Self::base(
            TailscaleProbeMethod::Cli,
            elapsed_ms,
            TailscalePlatform::Other,
        );
        report.installed = true;
        report.binary_absolute_path = Some(path);
        report.binary_version_raw = Some(version_raw.into());
        report.degradations.push(TailscaleProbeDegradation::new(
            TAILSCALE_BINARY_INAUTHENTIC_CODE,
            "high",
            format!(
                "Tailscale binary authenticity check failed: {}",
                detail.into()
            ),
            "Run which tailscale, verify provenance, and reinstall Tailscale if needed.",
        ));
        report
    }

    fn base(
        probe_method: TailscaleProbeMethod,
        probe_elapsed_ms: u64,
        platform: TailscalePlatform,
    ) -> Self {
        Self {
            schema: TAILSCALE_LOCAL_SCHEMA_V1,
            installed: false,
            daemon_reachable: false,
            authenticated: false,
            binary_authentic: false,
            binary_version_raw: None,
            binary_absolute_path: None,
            shields_up: None,
            tailnet_id: None,
            tailnet_display_name: None,
            self_node_key: None,
            self_tailscale_ip: None,
            self_magic_dns_name: None,
            self_advertised_tags: Vec::new(),
            peers: Vec::new(),
            version: None,
            probe_method,
            probe_elapsed_ms,
            platform,
            degradations: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TailscaleStatusProbeInput<'a> {
    pub status_json: &'a [u8],
    pub prefs_json: Option<&'a [u8]>,
    pub binary: Option<TailscaleBinaryReport>,
    pub method: TailscaleProbeMethod,
    pub elapsed_ms: u64,
    pub platform_hint: TailscalePlatform,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TailscaleCliProbeConfig {
    pub mesh_enabled: bool,
    pub binary_override: Option<PathBuf>,
    pub binary_candidates: Vec<PathBuf>,
    pub timeout_ms: u64,
    pub platform_hint: TailscalePlatform,
}

impl TailscaleCliProbeConfig {
    #[must_use]
    pub fn mesh_disabled() -> Self {
        Self {
            mesh_enabled: false,
            binary_override: None,
            binary_candidates: default_tailscale_binary_candidates(),
            timeout_ms: DEFAULT_TAILSCALE_PROBE_TIMEOUT_MS,
            platform_hint: TailscalePlatform::Other,
        }
    }

    #[must_use]
    pub fn mesh_enabled() -> Self {
        Self {
            mesh_enabled: true,
            binary_override: None,
            binary_candidates: default_tailscale_binary_candidates(),
            timeout_ms: DEFAULT_TAILSCALE_PROBE_TIMEOUT_MS,
            platform_hint: TailscalePlatform::Other,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TailscaleSocketProbeConfig {
    pub mesh_enabled: bool,
    pub socket_candidates: Vec<PathBuf>,
    pub timeout_ms: u64,
    pub platform_hint: TailscalePlatform,
}

impl TailscaleSocketProbeConfig {
    #[must_use]
    pub fn mesh_disabled() -> Self {
        Self {
            mesh_enabled: false,
            socket_candidates: default_tailscale_socket_candidates(),
            timeout_ms: DEFAULT_TAILSCALE_PROBE_TIMEOUT_MS,
            platform_hint: TailscalePlatform::Other,
        }
    }

    #[must_use]
    pub fn mesh_enabled() -> Self {
        Self {
            mesh_enabled: true,
            socket_candidates: default_tailscale_socket_candidates(),
            timeout_ms: DEFAULT_TAILSCALE_PROBE_TIMEOUT_MS,
            platform_hint: TailscalePlatform::Other,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TailscaleCliCommandOutput {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub success: bool,
    pub timed_out: bool,
    pub elapsed_ms: u64,
}

impl TailscaleCliCommandOutput {
    #[must_use]
    pub fn success(stdout: impl AsRef<[u8]>, elapsed_ms: u64) -> Self {
        Self {
            stdout: stdout.as_ref().to_vec(),
            stderr: Vec::new(),
            success: true,
            timed_out: false,
            elapsed_ms,
        }
    }

    #[must_use]
    pub fn failure(stderr: impl AsRef<[u8]>, elapsed_ms: u64) -> Self {
        Self {
            stdout: Vec::new(),
            stderr: stderr.as_ref().to_vec(),
            success: false,
            timed_out: false,
            elapsed_ms,
        }
    }

    #[must_use]
    pub fn timeout(elapsed_ms: u64) -> Self {
        Self {
            stdout: Vec::new(),
            stderr: Vec::new(),
            success: false,
            timed_out: true,
            elapsed_ms,
        }
    }
}

pub trait TailscaleCliProbeRunner {
    fn binary_exists(&self, path: &Path) -> bool;
    fn run(&mut self, path: &Path, args: &[&str], timeout_ms: u64) -> TailscaleCliCommandOutput;
}

pub trait TailscaleSocketProbeRunner {
    fn socket_exists(&self, path: &Path) -> bool;
    fn request(
        &mut self,
        path: &Path,
        endpoint: &str,
        timeout_ms: u64,
    ) -> TailscaleCliCommandOutput;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SystemTailscaleCliProbeRunner;

impl TailscaleCliProbeRunner for SystemTailscaleCliProbeRunner {
    fn binary_exists(&self, path: &Path) -> bool {
        path.is_file()
    }

    fn run(&mut self, path: &Path, args: &[&str], timeout_ms: u64) -> TailscaleCliCommandOutput {
        run_system_tailscale_command(path, args, timeout_ms)
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SystemTailscaleSocketProbeRunner;

impl TailscaleSocketProbeRunner for SystemTailscaleSocketProbeRunner {
    fn socket_exists(&self, path: &Path) -> bool {
        socket_candidate_exists(path)
    }

    fn request(
        &mut self,
        path: &Path,
        endpoint: &str,
        timeout_ms: u64,
    ) -> TailscaleCliCommandOutput {
        run_system_tailscale_socket_request(path, endpoint, timeout_ms)
    }
}

#[must_use]
pub fn tailscale_probe_timeout_ms_from_env_value(value: Option<&str>) -> u64 {
    value
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_TAILSCALE_PROBE_TIMEOUT_MS)
}

pub fn probe_tailscale_local_with_runners<
    S: TailscaleSocketProbeRunner,
    C: TailscaleCliProbeRunner,
>(
    socket_config: &TailscaleSocketProbeConfig,
    cli_config: &TailscaleCliProbeConfig,
    socket_runner: &mut S,
    cli_runner: &mut C,
) -> TailscaleLocalReport {
    if !socket_config.mesh_enabled || !cli_config.mesh_enabled {
        return TailscaleLocalReport::mesh_disabled();
    }

    if let Some(socket_path) = resolve_tailscale_socket(socket_config, socket_runner) {
        return probe_tailscale_socket_with_runner(socket_config, socket_runner, &socket_path);
    }

    probe_tailscale_cli_with_runner(cli_config, cli_runner)
}

pub fn probe_tailscale_socket_with_runner<R: TailscaleSocketProbeRunner>(
    config: &TailscaleSocketProbeConfig,
    runner: &mut R,
    socket_path: &Path,
) -> TailscaleLocalReport {
    if !config.mesh_enabled {
        return TailscaleLocalReport::mesh_disabled();
    }

    let status_output = runner.request(socket_path, "/localapi/v0/status", config.timeout_ms);
    if status_output.timed_out {
        return TailscaleLocalReport::timed_out(
            TailscaleProbeMethod::Socket,
            status_output.elapsed_ms,
        );
    }
    if !status_output.success {
        return TailscaleLocalReport::daemon_unreachable(
            TailscaleProbeMethod::Socket,
            status_output.elapsed_ms,
            command_error_detail(&status_output),
        );
    }

    let prefs_output = runner.request(socket_path, "/localapi/v0/prefs", config.timeout_ms);
    let prefs_json =
        (prefs_output.success && !prefs_output.timed_out).then_some(prefs_output.stdout.as_slice());
    let mut report = classify_status_payload(TailscaleStatusProbeInput {
        status_json: &status_output.stdout,
        prefs_json,
        binary: None,
        method: TailscaleProbeMethod::Socket,
        elapsed_ms: status_output.elapsed_ms + prefs_output.elapsed_ms,
        platform_hint: config.platform_hint,
    });
    if prefs_output.timed_out {
        push_probe_timeout_degradation(&mut report);
    }
    report
}

pub fn probe_tailscale_cli_with_runner<R: TailscaleCliProbeRunner>(
    config: &TailscaleCliProbeConfig,
    runner: &mut R,
) -> TailscaleLocalReport {
    if !config.mesh_enabled {
        return TailscaleLocalReport::mesh_disabled();
    }

    let Some(binary_path) = resolve_tailscale_binary(config, runner) else {
        return TailscaleLocalReport::not_installed(TailscaleProbeMethod::Cli, 0);
    };
    if let Err(degradation) = validate_binary_path(&binary_path) {
        let mut report =
            TailscaleLocalReport::base(TailscaleProbeMethod::Cli, 0, config.platform_hint);
        report.installed = true;
        report.binary_absolute_path = Some(binary_path);
        report.degradations.push(degradation);
        return report;
    }

    let version_output = runner.run(&binary_path, &["--version"], config.timeout_ms);
    if version_output.timed_out {
        return TailscaleLocalReport::timed_out(
            TailscaleProbeMethod::Cli,
            version_output.elapsed_ms,
        );
    }
    let version_raw = String::from_utf8_lossy(&version_output.stdout).to_string();
    let binary = classify_binary(binary_path.clone(), version_raw.clone());
    if !version_output.success || !binary.authentic {
        let detail = if version_output.success {
            "version output did not match expected Tailscale format".to_owned()
        } else {
            command_error_detail(&version_output)
        };
        return TailscaleLocalReport::binary_inauthentic(
            binary_path,
            version_raw,
            version_output.elapsed_ms,
            detail,
        );
    }

    let status_output = runner.run(
        &binary_path,
        &["status", "--json", "--self=true", "--peers=true"],
        config.timeout_ms,
    );
    if status_output.timed_out {
        return TailscaleLocalReport::timed_out(
            TailscaleProbeMethod::Cli,
            status_output.elapsed_ms,
        );
    }
    if !status_output.success {
        return TailscaleLocalReport::daemon_unreachable(
            TailscaleProbeMethod::Cli,
            status_output.elapsed_ms,
            command_error_detail(&status_output),
        );
    }

    let prefs_output = runner.run(
        &binary_path,
        &["debug", "localapi", "/localapi/v0/prefs"],
        config.timeout_ms,
    );
    let prefs_json =
        (prefs_output.success && !prefs_output.timed_out).then_some(prefs_output.stdout.as_slice());
    let mut report = classify_status_payload(TailscaleStatusProbeInput {
        status_json: &status_output.stdout,
        prefs_json,
        binary: Some(binary),
        method: TailscaleProbeMethod::Cli,
        elapsed_ms: version_output.elapsed_ms + status_output.elapsed_ms + prefs_output.elapsed_ms,
        platform_hint: config.platform_hint,
    });
    if prefs_output.timed_out {
        push_probe_timeout_degradation(&mut report);
    }
    report
}

#[must_use]
pub fn classify_status_payload(input: TailscaleStatusProbeInput<'_>) -> TailscaleLocalReport {
    let status: Value = match serde_json::from_slice(input.status_json) {
        Ok(value) => value,
        Err(error) => {
            return TailscaleLocalReport::daemon_unreachable(
                input.method,
                input.elapsed_ms,
                format!("malformed status JSON ({error})"),
            );
        }
    };

    let self_node = status.get("Self").unwrap_or(&Value::Null);
    let authenticated = bool_value(self_node, "Authenticated").unwrap_or_else(|| {
        matches!(
            string_value(&status, "BackendState").as_deref(),
            Some("Running")
        )
    });
    let backend_state = string_value(&status, "BackendState");
    let daemon_reachable = matches!(
        backend_state.as_deref(),
        Some("Running" | "NeedsLogin" | "Starting")
    );
    let platform = string_value(self_node, "Platform")
        .as_deref()
        .map(|value| TailscalePlatform::parse(Some(value)))
        .filter(|platform| *platform != TailscalePlatform::Other)
        .unwrap_or(input.platform_hint);
    let prefs = input
        .prefs_json
        .and_then(|bytes| serde_json::from_slice::<Value>(bytes).ok());
    let shields_up = prefs
        .as_ref()
        .and_then(|value| bool_value(value, "ShieldsUp"))
        .or_else(|| bool_value(self_node, "ShieldsUp"));

    let mut report = TailscaleLocalReport::base(input.method, input.elapsed_ms, platform);
    report.installed = true;
    report.daemon_reachable = daemon_reachable;
    report.authenticated = authenticated && daemon_reachable;
    report.shields_up = shields_up;
    report.tailnet_id = string_value(self_node, "Tailnet");
    report.tailnet_display_name =
        string_value(self_node, "TailnetName").or_else(|| report.tailnet_id.clone());
    report.self_node_key = string_value(self_node, "ID");
    report.self_magic_dns_name = string_value(self_node, "DNSName");
    report.self_tailscale_ip = first_string_array_value(self_node, "TailscaleIPs");
    report.self_advertised_tags = string_array_value(self_node, "Tags");
    report.peers = peer_reports(&status);

    if let Some(binary) = input.binary {
        report.binary_authentic = binary.authentic;
        report.binary_version_raw = Some(binary.version_raw);
        report.binary_absolute_path = Some(binary.path);
        report.version = binary.parsed_version;
        if !report.binary_authentic {
            report.degradations.push(TailscaleProbeDegradation::new(
                TAILSCALE_BINARY_INAUTHENTIC_CODE,
                "high",
                "Resolved tailscale binary did not match the expected version-output shape.",
                "Run which tailscale, verify provenance, and reinstall Tailscale if needed.",
            ));
        }
    } else if input.method == TailscaleProbeMethod::Socket {
        report.binary_authentic = true;
    }

    if !report.daemon_reachable {
        report.degradations.push(TailscaleProbeDegradation::new(
            TAILSCALE_DAEMON_UNREACHABLE_CODE,
            "warning",
            format!(
                "Tailscale daemon returned backend state {}.",
                backend_state.unwrap_or_else(|| "<missing>".to_owned())
            ),
            "Run tailscale status and inspect the local tailscaled service.",
        ));
    } else if !report.authenticated {
        report.degradations.push(TailscaleProbeDegradation::new(
            TAILSCALE_NOT_AUTHENTICATED_CODE,
            "warning",
            "Tailscale daemon is running but this node is not authenticated.",
            "Run tailscale up.",
        ));
    }

    if report.shields_up == Some(true) {
        report.degradations.push(TailscaleProbeDegradation::new(
            TAILSCALE_SHIELDS_UP_CODE,
            "warning",
            "Tailscale shields-up mode is enabled; peers cannot initiate discovery.",
            "Run tailscale set --shields-up=false if you want symmetric mesh discovery.",
        ));
    }

    report
}

fn push_probe_timeout_degradation(report: &mut TailscaleLocalReport) {
    report.degradations.push(TailscaleProbeDegradation::new(
        TAILSCALE_PROBE_TIMEOUT_CODE,
        "warning",
        format!(
            "Tailscale probe exceeded the {DEFAULT_TAILSCALE_PROBE_TIMEOUT_MS}ms default budget."
        ),
        "Run tailscale status directly or raise EE_TAILSCALE_PROBE_TIMEOUT_MS.",
    ));
}

#[must_use]
pub fn classify_binary(
    path: impl Into<PathBuf>,
    version_raw: impl Into<String>,
) -> TailscaleBinaryReport {
    let path = path.into();
    let version_raw = version_raw.into();
    let parsed_version = parse_tailscale_version(&version_raw);
    let authentic = path.is_absolute() && parsed_version.is_some();
    TailscaleBinaryReport {
        path,
        version_raw,
        authentic,
        parsed_version,
    }
}

pub fn validate_binary_path(path: &Path) -> Result<(), TailscaleProbeDegradation> {
    if path.is_absolute() {
        return Ok(());
    }
    Err(TailscaleProbeDegradation::new(
        TAILSCALE_BINARY_INAUTHENTIC_CODE,
        "high",
        format!(
            "Refusing relative tailscale binary path `{}`; mesh probes require an absolute binary path.",
            path.display()
        ),
        "Use an absolute Tailscale binary path from the trusted install location.",
    ))
}

fn resolve_tailscale_binary<R: TailscaleCliProbeRunner>(
    config: &TailscaleCliProbeConfig,
    runner: &R,
) -> Option<PathBuf> {
    if let Some(path) = &config.binary_override {
        return Some(path.clone());
    }
    config
        .binary_candidates
        .iter()
        .find(|path| path.is_absolute() && runner.binary_exists(path))
        .cloned()
}

fn resolve_tailscale_socket<R: TailscaleSocketProbeRunner>(
    config: &TailscaleSocketProbeConfig,
    runner: &R,
) -> Option<PathBuf> {
    config
        .socket_candidates
        .iter()
        .find(|path| path.is_absolute() && runner.socket_exists(path))
        .cloned()
}

fn command_error_detail(output: &TailscaleCliCommandOutput) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let trimmed = stderr.trim();
    if trimmed.is_empty() {
        "command exited unsuccessfully without stderr".to_owned()
    } else {
        trimmed.to_owned()
    }
}

#[cfg(unix)]
fn run_system_tailscale_socket_request(
    path: &Path,
    endpoint: &str,
    timeout_ms: u64,
) -> TailscaleCliCommandOutput {
    let start = Instant::now();
    let timeout = Duration::from_millis(timeout_ms);
    let mut stream = match UnixStream::connect(path) {
        Ok(stream) => stream,
        Err(error) if is_timeout_error(&error) => {
            return TailscaleCliCommandOutput::timeout(elapsed_ms_since(start));
        }
        Err(error) => {
            return TailscaleCliCommandOutput::failure(
                error.to_string().into_bytes(),
                elapsed_ms_since(start),
            );
        }
    };
    if let Err(error) = stream.set_read_timeout(Some(timeout)) {
        return TailscaleCliCommandOutput::failure(
            error.to_string().into_bytes(),
            elapsed_ms_since(start),
        );
    }
    if let Err(error) = stream.set_write_timeout(Some(timeout)) {
        return TailscaleCliCommandOutput::failure(
            error.to_string().into_bytes(),
            elapsed_ms_since(start),
        );
    }

    let request = format!(
        "GET {endpoint} HTTP/1.1\r\nHost: local-tailscaled.sock\r\nConnection: close\r\n\r\n"
    );
    if let Err(error) = stream.write_all(request.as_bytes()) {
        if is_timeout_error(&error) {
            return TailscaleCliCommandOutput::timeout(elapsed_ms_since(start));
        }
        return TailscaleCliCommandOutput::failure(
            error.to_string().into_bytes(),
            elapsed_ms_since(start),
        );
    }

    let mut response = Vec::new();
    if let Err(error) = stream.read_to_end(&mut response) {
        if is_timeout_error(&error) {
            return TailscaleCliCommandOutput::timeout(elapsed_ms_since(start));
        }
        return TailscaleCliCommandOutput::failure(
            error.to_string().into_bytes(),
            elapsed_ms_since(start),
        );
    }

    match http_response_body(&response) {
        Ok(body) => TailscaleCliCommandOutput::success(body, elapsed_ms_since(start)),
        Err(error) => {
            TailscaleCliCommandOutput::failure(error.into_bytes(), elapsed_ms_since(start))
        }
    }
}

#[cfg(not(unix))]
fn run_system_tailscale_socket_request(
    _path: &Path,
    _endpoint: &str,
    _timeout_ms: u64,
) -> TailscaleCliCommandOutput {
    TailscaleCliCommandOutput::failure(
        "Tailscale socket probing is not implemented on this platform",
        0,
    )
}

fn run_system_tailscale_command(
    path: &Path,
    args: &[&str],
    timeout_ms: u64,
) -> TailscaleCliCommandOutput {
    let start = Instant::now();
    let mut child = match Command::new(path)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(error) => {
            return TailscaleCliCommandOutput::failure(
                error.to_string().into_bytes(),
                elapsed_ms_since(start),
            );
        }
    };

    let timeout = Duration::from_millis(timeout_ms);
    loop {
        match child.try_wait() {
            Ok(Some(_status)) => {
                return match child.wait_with_output() {
                    Ok(output) => TailscaleCliCommandOutput {
                        stdout: output.stdout,
                        stderr: output.stderr,
                        success: output.status.success(),
                        timed_out: false,
                        elapsed_ms: elapsed_ms_since(start),
                    },
                    Err(error) => TailscaleCliCommandOutput::failure(
                        error.to_string().into_bytes(),
                        elapsed_ms_since(start),
                    ),
                };
            }
            Ok(None) => {
                if start.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return TailscaleCliCommandOutput::timeout(elapsed_ms_since(start));
                }
                thread::sleep(Duration::from_millis(10));
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return TailscaleCliCommandOutput::failure(
                    error.to_string().into_bytes(),
                    elapsed_ms_since(start),
                );
            }
        }
    }
}

fn elapsed_ms_since(start: Instant) -> u64 {
    start.elapsed().as_millis().try_into().unwrap_or(u64::MAX)
}

#[cfg(unix)]
fn socket_candidate_exists(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .map(|metadata| metadata.file_type().is_socket())
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn socket_candidate_exists(_path: &Path) -> bool {
    false
}

fn is_timeout_error(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
    )
}

fn http_response_body(response: &[u8]) -> Result<&[u8], String> {
    let header_end = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or_else(|| "local API response did not include an HTTP header terminator".to_owned())?;
    let header = String::from_utf8_lossy(&response[..header_end]);
    let status_line = header
        .lines()
        .next()
        .ok_or_else(|| "local API response did not include an HTTP status line".to_owned())?;
    let mut status_parts = status_line.split_whitespace();
    let http_version = status_parts.next();
    if !matches!(http_version, Some(version) if version.starts_with("HTTP/")) {
        return Err(format!(
            "local API response had invalid HTTP status line {status_line}"
        ));
    }
    let status_code = status_parts.next();
    if status_code != Some("200") {
        return Err(format!("local API returned {status_line}"));
    }
    Ok(&response[header_end + 4..])
}

fn default_tailscale_socket_candidates() -> Vec<PathBuf> {
    let mut candidates = vec![
        PathBuf::from("/var/run/tailscale/tailscaled.sock"),
        PathBuf::from("/run/tailscale/tailscaled.sock"),
        PathBuf::from("/var/run/tailscaled.socket"),
    ];
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        candidates
            .push(home.join("Library/Containers/io.tailscale.ipn.macsys/Data/IPN/tailscaled.sock"));
        candidates.push(
            home.join("Library/Group Containers/io.tailscale.ipn.macos/Data/IPN/tailscaled.sock"),
        );
    }
    candidates
}

fn default_tailscale_binary_candidates() -> Vec<PathBuf> {
    [
        "/usr/bin/tailscale",
        "/usr/local/bin/tailscale",
        "/opt/homebrew/bin/tailscale",
        "C:\\Program Files\\Tailscale\\tailscale.exe",
    ]
    .into_iter()
    .map(PathBuf::from)
    .collect()
}

fn parse_tailscale_version(raw: &str) -> Option<String> {
    let mut lines = raw.lines().map(str::trim);
    let version = lines.next()?;
    if !looks_like_semver(version) {
        return None;
    }
    let tailscale_commit = lines.next()?;
    let other_commit = lines.next()?;
    let go_version = lines.next()?;
    if !has_commit_suffix(tailscale_commit, "tailscale commit:")
        || !has_commit_suffix(other_commit, "other commit:")
        || !has_go_version_suffix(go_version)
        || lines.next().is_some()
    {
        return None;
    }
    Some(version.to_owned())
}

fn looks_like_semver(value: &str) -> bool {
    let parts = value.split('.').collect::<Vec<_>>();
    parts.len() >= 3
        && parts.iter().take(3).all(|part| {
            !part.is_empty() && part.chars().all(|character| character.is_ascii_digit())
        })
}

fn has_commit_suffix(line: &str, prefix: &str) -> bool {
    let Some(value) = line.strip_prefix(prefix) else {
        return false;
    };
    let value = value.trim();
    value.len() == 40 && value.chars().all(|character| character.is_ascii_hexdigit())
}

fn has_go_version_suffix(line: &str) -> bool {
    let Some(value) = line.strip_prefix("go version:") else {
        return false;
    };
    let value = value.trim();
    let Some(version) = value.strip_prefix("go") else {
        return false;
    };
    looks_like_semver(version)
}

fn string_value(value: &Value, key: &str) -> Option<String> {
    value.get(key)?.as_str().map(str::to_owned)
}

fn bool_value(value: &Value, key: &str) -> Option<bool> {
    value.get(key)?.as_bool()
}

fn first_string_array_value(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)?
        .as_array()?
        .iter()
        .find_map(|item| item.as_str().map(str::to_owned))
}

fn string_array_value(value: &Value, key: &str) -> Vec<String> {
    value
        .get(key)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| item.as_str().map(str::to_owned))
        .collect()
}

fn peer_reports(status: &Value) -> Vec<TailscalePeerReport> {
    let Some(peers) = status.get("Peer").and_then(Value::as_object) else {
        return Vec::new();
    };

    let mut reports: Vec<_> = peers
        .iter()
        .filter_map(|(fallback_node_key, peer)| {
            if !peer.is_object() {
                return None;
            }
            let node_key = string_value(peer, "ID").unwrap_or_else(|| fallback_node_key.to_owned());
            if node_key.trim().is_empty() {
                return None;
            }
            Some(TailscalePeerReport {
                node_key,
                tailscale_ips: string_array_value(peer, "TailscaleIPs"),
                magic_dns_name: string_value(peer, "DNSName"),
                hostname: string_value(peer, "HostName"),
                advertised_tags: string_array_value(peer, "Tags"),
                online: bool_value(peer, "Online"),
            })
        })
        .collect();
    reports.sort_by(|left, right| left.node_key.cmp(&right.node_key));
    reports
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), String>;

    fn fixture_binary() -> TailscaleBinaryReport {
        classify_binary(
            "/usr/local/bin/tailscale",
            "1.66.0\n  tailscale commit: 0123456789abcdef0123456789abcdef01234567\n  other commit: 89abcdef0123456789abcdef0123456789abcdef\n  go version: go1.22.3\n",
        )
    }

    fn classify(status: &str) -> TailscaleLocalReport {
        classify_status_payload(TailscaleStatusProbeInput {
            status_json: status.as_bytes(),
            prefs_json: None,
            binary: Some(fixture_binary()),
            method: TailscaleProbeMethod::Cli,
            elapsed_ms: 12,
            platform_hint: TailscalePlatform::Linux,
        })
    }

    #[test]
    fn mesh_disabled_report_is_explicitly_skipped() {
        let report = TailscaleLocalReport::mesh_disabled();
        assert_eq!(report.probe_method, TailscaleProbeMethod::Skipped);
        assert_eq!(
            report.degradations[0].code,
            TAILSCALE_PROBE_UNAVAILABLE_CODE
        );
    }

    #[test]
    fn malformed_status_json_is_daemon_unreachable_not_panic() {
        let report = classify("{\"Version\":\"fake\",\"Peer\":");
        assert!(report.installed);
        assert!(!report.daemon_reachable);
        assert_eq!(
            report.degradations[0].code,
            TAILSCALE_DAEMON_UNREACHABLE_CODE
        );
    }

    #[test]
    fn logged_out_status_reports_not_authenticated() {
        let report = classify(
            r#"{
              "BackendState": "NeedsLogin",
              "Self": {"ID":"nodekey:self","Authenticated":false,"TailscaleIPs":["100.64.0.1"],"Platform":"linux"}
            }"#,
        );
        assert!(report.daemon_reachable);
        assert!(!report.authenticated);
        assert!(
            report
                .degradations
                .iter()
                .any(|item| item.code == TAILSCALE_NOT_AUTHENTICATED_CODE)
        );
    }

    #[test]
    fn unknown_status_fields_are_ignored() {
        let report = classify(
            r#"{
              "UnexpectedFakeField": {"ignored": true},
              "BackendState": "Running",
              "Self": {
                "ID":"nodekey:self",
                "Authenticated":true,
                "DNSName":"ee-local.tailnet.test.",
                "TailscaleIPs":["100.64.0.10"],
                "Tailnet":"tailnet-alpha",
                "TailnetName":"alpha.example",
                "Tags":["tag:ee-mesh"],
                "Platform":"linux"
              }
            }"#,
        );
        assert_eq!(report.self_node_key.as_deref(), Some("nodekey:self"));
        assert_eq!(report.self_tailscale_ip.as_deref(), Some("100.64.0.10"));
        assert!(report.degradations.is_empty());
    }

    #[test]
    fn peer_map_is_reported_in_deterministic_node_key_order() {
        let report = classify(
            r#"{
              "BackendState": "Running",
              "Self": {
                "ID":"nodekey:self",
                "Authenticated":true,
                "TailscaleIPs":["100.64.0.10"],
                "Platform":"linux"
              },
              "Peer": {
                "nodekey:zulu": {
                  "ID": "nodekey:zulu",
                  "DNSName": "zulu.tailnet.test.",
                  "HostName": "zulu",
                  "Online": false,
                  "Tags": [],
                  "TailscaleIPs": ["100.64.0.30"]
                },
                "nodekey:alpha": {
                  "DNSName": "alpha.tailnet.test.",
                  "HostName": "alpha",
                  "Online": true,
                  "Tags": ["tag:ee-mesh"],
                  "TailscaleIPs": ["100.64.0.20", "fd7a:115c:a1e0::20"]
                }
              }
            }"#,
        );

        assert_eq!(report.peers.len(), 2);
        assert_eq!(report.peers[0].node_key, "nodekey:alpha");
        assert_eq!(report.peers[0].tailscale_ips[0], "100.64.0.20");
        assert_eq!(
            report.peers[0].magic_dns_name.as_deref(),
            Some("alpha.tailnet.test.")
        );
        assert_eq!(report.peers[0].hostname.as_deref(), Some("alpha"));
        assert_eq!(report.peers[0].advertised_tags, vec!["tag:ee-mesh"]);
        assert_eq!(report.peers[0].online, Some(true));
        assert_eq!(report.peers[1].node_key, "nodekey:zulu");
    }

    #[test]
    fn malformed_binary_version_marks_binary_inauthentic() {
        let binary = classify_binary("/usr/local/bin/tailscale", "definitely not tailscale");
        assert!(!binary.authentic);
        assert_eq!(binary.parsed_version, None);
    }

    #[test]
    fn relative_binary_path_is_rejected() {
        let err = validate_binary_path(Path::new("tailscale")).expect_err("relative path rejected");
        assert_eq!(err.code, TAILSCALE_BINARY_INAUTHENTIC_CODE);
    }

    #[cfg(unix)]
    #[test]
    fn socket_candidate_rejects_symlink_to_socket() -> TestResult {
        if std::env::var("TMPDIR")
            .unwrap_or_default()
            .contains("USBNVME")
        {
            return Ok(());
        }
        use std::os::unix::{fs::symlink, net::UnixListener};

        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let socket_path = temp.path().join("tailscaled.sock");
        let _listener = UnixListener::bind(&socket_path).map_err(|error| error.to_string())?;
        let socket_link = temp.path().join("linked.sock");
        symlink(&socket_path, &socket_link).map_err(|error| error.to_string())?;

        assert!(
            socket_candidate_exists(&socket_path),
            "real Unix socket should be a candidate"
        );
        assert!(
            !socket_candidate_exists(&socket_link),
            "symlinked socket candidate should not be followed"
        );

        Ok(())
    }

    #[test]
    fn localapi_http_response_body_extracts_success_payload() -> TestResult {
        let body = http_response_body(
            b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n{\"BackendState\":\"Running\"}",
        )?;

        assert_eq!(body, br#"{"BackendState":"Running"}"#);
        Ok(())
    }

    #[test]
    fn localapi_http_response_body_rejects_non_200_status() {
        let error = http_response_body(b"HTTP/1.1 503 Service Unavailable\r\n\r\nbusy")
            .expect_err("non-200 localapi response should fail");

        assert!(error.contains("HTTP/1.1 503 Service Unavailable"));
    }

    #[test]
    fn localapi_http_response_body_rejects_malformed_status_line() {
        let error = http_response_body(b"not-http 200 OK\r\n\r\n{}")
            .expect_err("malformed localapi status line should fail");

        assert!(error.contains("invalid HTTP status line not-http 200 OK"));
    }

    #[test]
    fn localapi_http_response_body_rejects_missing_header_terminator() {
        let error = http_response_body(b"HTTP/1.1 200 OK\n\n{}")
            .expect_err("localapi response without CRLF header terminator should fail");

        assert!(error.contains("HTTP header terminator"));
    }

    #[test]
    fn shields_up_is_classified_from_prefs_payload() -> TestResult {
        let report = classify_status_payload(TailscaleStatusProbeInput {
            status_json: br#"{
              "BackendState": "Running",
              "Self": {"ID":"nodekey:self","Authenticated":true,"TailscaleIPs":["100.64.0.1"],"Platform":"linux"}
            }"#,
            prefs_json: Some(br#"{"ShieldsUp": true}"#),
            binary: Some(fixture_binary()),
            method: TailscaleProbeMethod::Socket,
            elapsed_ms: 8,
            platform_hint: TailscalePlatform::Linux,
        });
        assert_eq!(report.shields_up, Some(true));
        assert!(
            report
                .degradations
                .iter()
                .any(|item| item.code == TAILSCALE_SHIELDS_UP_CODE)
        );
        Ok(())
    }
}
