//! Tailscale local-probe model, parsers, and narrow probe runners for SRR6.46.1.
//!
//! The classification layer stays deterministic and testable: system I/O is
//! isolated behind small runner traits so status/doctor surfaces can share one
//! interpretation path.

use std::collections::BTreeMap;
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
pub struct TailscaleUserProfile {
    pub user_id: String,
    pub login_name: String,
    pub display_name: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TailnetOwnerDisposition {
    Attested,
    Missing,
    DomainMismatch,
    Reassigned,
}

impl TailnetOwnerDisposition {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Attested => "attested",
            Self::Missing => "missing",
            Self::DomainMismatch => "domain_mismatch",
            Self::Reassigned => "reassigned",
        }
    }
}

#[must_use]
pub fn evaluate_tailnet_owner(
    observed: Option<&TailscaleUserProfile>,
    recorded_login: Option<&str>,
    allowed_domain: Option<&str>,
) -> TailnetOwnerDisposition {
    let Some(owner) = observed else {
        return TailnetOwnerDisposition::Missing;
    };
    let login = owner.login_name.trim();
    if login.is_empty() {
        return TailnetOwnerDisposition::Missing;
    }
    if allowed_domain
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .is_some_and(|domain| !login_belongs_to_domain(login, domain))
    {
        return TailnetOwnerDisposition::DomainMismatch;
    }
    if recorded_login
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .is_some_and(|recorded| !recorded.eq_ignore_ascii_case(login))
    {
        return TailnetOwnerDisposition::Reassigned;
    }
    TailnetOwnerDisposition::Attested
}

#[must_use]
pub fn login_belongs_to_domain(login: &str, domain: &str) -> bool {
    let login = login.trim();
    let domain = domain.trim().trim_start_matches('@');
    if login.is_empty() || domain.is_empty() {
        return false;
    }
    login
        .rsplit_once('@')
        .is_some_and(|(_, host)| host.eq_ignore_ascii_case(domain))
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TailscalePeerReport {
    pub node_key: String,
    pub tailscale_ips: Vec<String>,
    pub magic_dns_name: Option<String>,
    pub hostname: Option<String>,
    pub advertised_tags: Vec<String>,
    pub online: Option<bool>,
    pub ee_capability: Option<TailscalePeerEeCapability>,
    pub owner: Option<TailscaleUserProfile>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TailscalePeerEeCapability {
    pub ee_version: String,
    pub ee_protocol_version: String,
    pub workspace_ids: Vec<String>,
    pub respond: bool,
    pub latency_ms: u64,
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
    pub self_owner: Option<TailscaleUserProfile>,
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
    pub fn not_installed(
        method: TailscaleProbeMethod,
        elapsed_ms: u64,
        platform: TailscalePlatform,
    ) -> Self {
        let mut report = Self::base(method, elapsed_ms, platform);
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
        platform: TailscalePlatform,
        detail: impl Into<String>,
    ) -> Self {
        let mut report = Self::base(method, elapsed_ms, platform);
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
    pub fn timed_out(
        method: TailscaleProbeMethod,
        elapsed_ms: u64,
        timeout_budget_ms: u64,
        platform: TailscalePlatform,
    ) -> Self {
        let mut report = Self::base(method, elapsed_ms, platform);
        report.installed = true;
        report.degradations.push(TailscaleProbeDegradation::new(
            TAILSCALE_PROBE_TIMEOUT_CODE,
            "warning",
            format!("Tailscale probe exceeded the configured {timeout_budget_ms}ms budget."),
            "Run tailscale status directly or raise EE_TAILSCALE_PROBE_TIMEOUT_MS.",
        ));
        report
    }

    #[must_use]
    pub fn binary_inauthentic(
        path: PathBuf,
        version_raw: impl Into<String>,
        elapsed_ms: u64,
        platform: TailscalePlatform,
        detail: impl Into<String>,
    ) -> Self {
        let mut report = Self::base(TailscaleProbeMethod::Cli, elapsed_ms, platform);
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
            self_owner: None,
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
    if !socket_config.mesh_enabled && !cli_config.mesh_enabled {
        return TailscaleLocalReport::mesh_disabled();
    }

    if socket_config.mesh_enabled
        && let Some(socket_path) = resolve_tailscale_socket(socket_config, socket_runner)
    {
        return probe_tailscale_socket_with_runner(socket_config, socket_runner, &socket_path);
    }

    if cli_config.mesh_enabled {
        return probe_tailscale_cli_with_runner(cli_config, cli_runner);
    }

    TailscaleLocalReport::not_installed(
        TailscaleProbeMethod::Socket,
        0,
        socket_config.platform_hint,
    )
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
            config.timeout_ms,
            config.platform_hint,
        );
    }
    if !status_output.success {
        return TailscaleLocalReport::daemon_unreachable(
            TailscaleProbeMethod::Socket,
            status_output.elapsed_ms,
            config.platform_hint,
            command_error_detail(&status_output),
        );
    }

    let Some(prefs_timeout_ms) =
        remaining_probe_timeout_ms(config.timeout_ms, status_output.elapsed_ms)
    else {
        let mut report = classify_status_payload(TailscaleStatusProbeInput {
            status_json: &status_output.stdout,
            prefs_json: None,
            binary: None,
            method: TailscaleProbeMethod::Socket,
            elapsed_ms: status_output.elapsed_ms,
            platform_hint: config.platform_hint,
        });
        push_probe_timeout_degradation(&mut report, config.timeout_ms);
        return report;
    };

    let prefs_output = runner.request(socket_path, "/localapi/v0/prefs", prefs_timeout_ms);
    let prefs_json = successful_localapi_json_payload(&prefs_output);
    let mut report = classify_status_payload(TailscaleStatusProbeInput {
        status_json: &status_output.stdout,
        prefs_json,
        binary: None,
        method: TailscaleProbeMethod::Socket,
        elapsed_ms: status_output
            .elapsed_ms
            .saturating_add(prefs_output.elapsed_ms),
        platform_hint: config.platform_hint,
    });
    if prefs_output.timed_out {
        push_probe_timeout_degradation(&mut report, config.timeout_ms);
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
        return TailscaleLocalReport::not_installed(
            TailscaleProbeMethod::Cli,
            0,
            config.platform_hint,
        );
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
            config.timeout_ms,
            config.platform_hint,
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
            config.platform_hint,
            detail,
        );
    }

    let Some(status_timeout_ms) =
        remaining_probe_timeout_ms(config.timeout_ms, version_output.elapsed_ms)
    else {
        return TailscaleLocalReport::timed_out(
            TailscaleProbeMethod::Cli,
            version_output.elapsed_ms,
            config.timeout_ms,
            config.platform_hint,
        );
    };

    let status_output = runner.run(
        &binary_path,
        &["status", "--json", "--self=true", "--peers=true"],
        status_timeout_ms,
    );
    let status_elapsed_ms = version_output
        .elapsed_ms
        .saturating_add(status_output.elapsed_ms);
    if status_output.timed_out {
        return TailscaleLocalReport::timed_out(
            TailscaleProbeMethod::Cli,
            status_elapsed_ms,
            config.timeout_ms,
            config.platform_hint,
        );
    }
    if !status_output.success {
        return TailscaleLocalReport::daemon_unreachable(
            TailscaleProbeMethod::Cli,
            status_elapsed_ms,
            config.platform_hint,
            command_error_detail(&status_output),
        );
    }

    let Some(prefs_timeout_ms) = remaining_probe_timeout_ms(config.timeout_ms, status_elapsed_ms)
    else {
        let mut report = classify_status_payload(TailscaleStatusProbeInput {
            status_json: &status_output.stdout,
            prefs_json: None,
            binary: Some(binary),
            method: TailscaleProbeMethod::Cli,
            elapsed_ms: status_elapsed_ms,
            platform_hint: config.platform_hint,
        });
        push_probe_timeout_degradation(&mut report, config.timeout_ms);
        return report;
    };

    let prefs_output = runner.run(
        &binary_path,
        &["debug", "localapi", "/localapi/v0/prefs"],
        prefs_timeout_ms,
    );
    let prefs_json = successful_localapi_json_payload(&prefs_output);
    let mut report = classify_status_payload(TailscaleStatusProbeInput {
        status_json: &status_output.stdout,
        prefs_json,
        binary: Some(binary),
        method: TailscaleProbeMethod::Cli,
        elapsed_ms: status_elapsed_ms.saturating_add(prefs_output.elapsed_ms),
        platform_hint: config.platform_hint,
    });
    if prefs_output.timed_out {
        push_probe_timeout_degradation(&mut report, config.timeout_ms);
    }
    report
}

fn remaining_probe_timeout_ms(timeout_budget_ms: u64, elapsed_ms: u64) -> Option<u64> {
    let remaining = timeout_budget_ms.saturating_sub(elapsed_ms);
    (remaining > 0).then_some(remaining)
}

#[must_use]
pub fn classify_status_payload(input: TailscaleStatusProbeInput<'_>) -> TailscaleLocalReport {
    let status: Value = match serde_json::from_slice(input.status_json) {
        Ok(value) => value,
        Err(error) => {
            return TailscaleLocalReport::daemon_unreachable(
                input.method,
                input.elapsed_ms,
                input.platform_hint,
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
    let platform_value =
        string_value(self_node, "Platform").or_else(|| string_value(self_node, "OS"));
    let platform = platform_value
        .as_deref()
        .map(|value| TailscalePlatform::parse(Some(value)))
        .filter(|platform| *platform != TailscalePlatform::Other)
        .unwrap_or(input.platform_hint);
    let current_tailnet = status.get("CurrentTailnet").unwrap_or(&Value::Null);
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
    report.tailnet_id = string_value(self_node, "Tailnet")
        .or_else(|| string_value(current_tailnet, "MagicDNSSuffix"))
        .or_else(|| string_value(&status, "MagicDNSSuffix"));
    report.tailnet_display_name = string_value(self_node, "TailnetName")
        .or_else(|| string_value(current_tailnet, "Name"))
        .or_else(|| report.tailnet_id.clone());
    report.self_node_key = node_key_value(self_node, None);
    report.self_magic_dns_name = string_value(self_node, "DNSName");
    report.self_tailscale_ip = first_string_array_value(self_node, "TailscaleIPs");
    report.self_advertised_tags = string_array_value(self_node, "Tags");
    let users = user_profiles(&status);
    report.self_owner = node_owner(self_node, &users);
    report.peers = peer_reports(&status, &users);

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

fn push_probe_timeout_degradation(report: &mut TailscaleLocalReport, timeout_budget_ms: u64) {
    report.degradations.push(TailscaleProbeDegradation::new(
        TAILSCALE_PROBE_TIMEOUT_CODE,
        "warning",
        format!("Tailscale probe exceeded the configured {timeout_budget_ms}ms budget."),
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
        if let Some(path) = normalize_tailscale_binary_override(path) {
            return Some(path);
        }
    }
    config
        .binary_candidates
        .iter()
        .find(|path| path.is_absolute() && runner.binary_exists(path))
        .cloned()
}

fn normalize_tailscale_binary_override(path: &Path) -> Option<PathBuf> {
    if path.as_os_str().is_empty() {
        return None;
    }
    if let Some(raw) = path.to_str() {
        let trimmed = raw.trim();
        return (!trimmed.is_empty()).then(|| PathBuf::from(trimmed));
    }
    Some(path.to_path_buf())
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

fn successful_localapi_json_payload(output: &TailscaleCliCommandOutput) -> Option<&[u8]> {
    (output.success && !output.timed_out)
        .then(|| strip_localapi_debug_prelude(&output.stdout))
        .filter(|payload| !payload.is_empty())
}

fn strip_localapi_debug_prelude(bytes: &[u8]) -> &[u8] {
    let mut remaining = trim_ascii_start(bytes);
    while remaining.first() == Some(&b'#') {
        let Some(line_end) = remaining.iter().position(|byte| *byte == b'\n') else {
            return remaining;
        };
        remaining = trim_ascii_start(&remaining[line_end + 1..]);
    }
    remaining
}

fn trim_ascii_start(bytes: &[u8]) -> &[u8] {
    let first_non_whitespace = bytes
        .iter()
        .position(|byte| !byte.is_ascii_whitespace())
        .unwrap_or(bytes.len());
    &bytes[first_non_whitespace..]
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

fn http_response_body(response: &[u8]) -> Result<Vec<u8>, String> {
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
    let body = &response[header_end + 4..];
    if has_chunked_transfer_encoding(&header) {
        return decode_chunked_body(body);
    }
    Ok(body.to_vec())
}

fn has_chunked_transfer_encoding(header: &str) -> bool {
    header.lines().any(|line| {
        let Some((name, value)) = line.split_once(':') else {
            return false;
        };
        name.trim().eq_ignore_ascii_case("transfer-encoding")
            && value
                .split(',')
                .any(|token| token.trim().eq_ignore_ascii_case("chunked"))
    })
}

fn decode_chunked_body(body: &[u8]) -> Result<Vec<u8>, String> {
    let mut decoded = Vec::new();
    let mut cursor = 0;
    loop {
        let line_end = body[cursor..]
            .windows(2)
            .position(|window| window == b"\r\n")
            .ok_or_else(|| "chunked local API response had unterminated chunk size".to_owned())?;
        let size_line = String::from_utf8_lossy(&body[cursor..cursor + line_end]);
        let size_token = size_line.split(';').next().unwrap_or_default().trim();
        let chunk_size = usize::from_str_radix(size_token, 16).map_err(|_| {
            format!("chunked local API response had invalid chunk size `{size_token}`")
        })?;
        cursor += line_end + 2;
        if chunk_size == 0 {
            return Ok(decoded);
        }
        let chunk_end = cursor
            .checked_add(chunk_size)
            .ok_or_else(|| "chunked local API response chunk size overflowed".to_owned())?;
        if chunk_end > body.len() {
            return Err("chunked local API response ended before chunk payload".to_owned());
        }
        decoded.extend_from_slice(&body[cursor..chunk_end]);
        cursor = chunk_end;
        if body.get(cursor..cursor + 2) != Some(b"\r\n") {
            return Err("chunked local API response chunk missing trailing CRLF".to_owned());
        }
        cursor += 2;
    }
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
    let version_metadata = lines.next()?;
    let go_version = lines.next()?;
    if !has_commit_suffix(tailscale_commit, "tailscale commit:")
        || !has_version_metadata_line(version_metadata, version)
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

fn has_version_metadata_line(line: &str, parsed_version: &str) -> bool {
    has_commit_suffix(line, "other commit:") || has_long_version_suffix(line, parsed_version)
}

fn has_long_version_suffix(line: &str, parsed_version: &str) -> bool {
    let Some(value) = line.strip_prefix("long version:") else {
        return false;
    };
    let value = value.trim();
    if value == parsed_version {
        return true;
    }
    value
        .strip_prefix(parsed_version)
        .is_some_and(|suffix| suffix.starts_with('-') && suffix.len() > 1)
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

fn peer_reports(
    status: &Value,
    users: &BTreeMap<String, TailscaleUserProfile>,
) -> Vec<TailscalePeerReport> {
    let Some(peers) = status.get("Peer").and_then(Value::as_object) else {
        return Vec::new();
    };

    let mut reports: Vec<_> = peers
        .iter()
        .filter_map(|(fallback_node_key, peer)| {
            if !peer.is_object() {
                return None;
            }
            let node_key = node_key_value(peer, Some(fallback_node_key))
                .unwrap_or_else(|| fallback_node_key.to_owned());
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
                ee_capability: peer_ee_capability(peer),
                owner: node_owner(peer, users),
            })
        })
        .collect();
    reports.sort_by(|left, right| left.node_key.cmp(&right.node_key));
    reports
}

fn user_profiles(status: &Value) -> BTreeMap<String, TailscaleUserProfile> {
    let Some(users) = status.get("User").and_then(Value::as_object) else {
        return BTreeMap::new();
    };
    users
        .iter()
        .filter_map(|(key, value)| {
            parse_user_profile(value, Some(key)).map(|profile| (profile.user_id.clone(), profile))
        })
        .collect()
}

fn parse_user_profile(value: &Value, fallback_id: Option<&str>) -> Option<TailscaleUserProfile> {
    if !value.is_object() {
        return None;
    }
    let user_id = numeric_or_string_id(value, "ID")
        .or_else(|| fallback_id.map(str::to_owned))
        .filter(|id| !id.trim().is_empty())?;
    let login_name = string_value(value, "LoginName")
        .or_else(|| string_value(value, "loginName"))
        .filter(|login| !login.trim().is_empty())?;
    Some(TailscaleUserProfile {
        user_id,
        login_name,
        display_name: string_value(value, "DisplayName")
            .or_else(|| string_value(value, "displayName"))
            .filter(|name| !name.trim().is_empty()),
    })
}

fn node_owner(
    node: &Value,
    users: &BTreeMap<String, TailscaleUserProfile>,
) -> Option<TailscaleUserProfile> {
    if let Some(nested) = node.get("UserProfile")
        && let Some(profile) = parse_user_profile(nested, None)
    {
        return Some(profile);
    }
    let user_id =
        numeric_or_string_id(node, "UserID").or_else(|| numeric_or_string_id(node, "User"))?;
    users.get(&user_id).cloned()
}

fn numeric_or_string_id(value: &Value, key: &str) -> Option<String> {
    let field = value.get(key)?;
    field
        .as_str()
        .map(str::to_owned)
        .or_else(|| field.as_i64().map(|n| n.to_string()))
        .or_else(|| field.as_u64().map(|n| n.to_string()))
}

fn peer_ee_capability(peer: &Value) -> Option<TailscalePeerEeCapability> {
    let capabilities = peer.get("Capabilities")?;
    let ee_version = string_value(capabilities, "eeVersion");
    let ee_protocol_version = string_value(capabilities, "eeProtocol");
    let workspace_ids_key_present = capabilities.get("workspaceIds").is_some();
    if ee_version.is_none() && ee_protocol_version.is_none() && !workspace_ids_key_present {
        return None;
    }

    Some(TailscalePeerEeCapability {
        ee_version: ee_version.unwrap_or_default(),
        ee_protocol_version: ee_protocol_version.unwrap_or_default(),
        workspace_ids: string_array_value(capabilities, "workspaceIds"),
        respond: bool_value(capabilities, "respond").unwrap_or(true),
        latency_ms: capabilities
            .get("latencyMs")
            .and_then(Value::as_u64)
            .unwrap_or(0),
    })
}

fn node_key_value(value: &Value, fallback_node_key: Option<&str>) -> Option<String> {
    string_value(value, "PublicKey")
        .and_then(|candidate| normalize_node_key(&candidate))
        .or_else(|| fallback_node_key.and_then(normalize_node_key))
        .or_else(|| string_value(value, "ID").and_then(|candidate| normalize_node_key(&candidate)))
}

fn normalize_node_key(value: &str) -> Option<String> {
    let trimmed = value.trim();
    trimmed.starts_with("nodekey:").then(|| trimmed.to_owned())
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
    fn peer_capability_metadata_is_parsed_from_status_payload() {
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
                "nodekey:alpha": {
                  "ID": "nodekey:alpha",
                  "Tags": ["tag:ee-mesh"],
                  "TailscaleIPs": ["100.64.0.20"],
                  "Capabilities": {
                    "eeVersion": "0.2.0",
                    "eeProtocol": "1.0",
                    "workspaceIds": ["workspace-alpha"],
                    "respond": true,
                    "latencyMs": 17
                  }
                }
              }
            }"#,
        );

        let capability = report.peers[0].ee_capability.as_ref().expect("capability");
        assert_eq!(capability.ee_version, "0.2.0");
        assert_eq!(capability.ee_protocol_version, "1.0");
        assert_eq!(capability.workspace_ids, vec!["workspace-alpha".to_owned()]);
        assert!(capability.respond);
        assert_eq!(capability.latency_ms, 17);
    }

    #[test]
    fn generic_tailscale_capabilities_do_not_create_empty_ee_capability() {
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
                "nodekey:alpha": {
                  "ID": "nodekey:alpha",
                  "Capabilities": ["https://tailscale.com/cap/file-sharing"]
                },
                "nodekey:bravo": {
                  "ID": "nodekey:bravo",
                  "Capabilities": {"unrelated": true}
                }
              }
            }"#,
        );

        assert_eq!(report.peers.len(), 2);
        assert!(report.peers.iter().all(|peer| peer.ee_capability.is_none()));
    }

    #[test]
    fn malformed_binary_version_marks_binary_inauthentic() {
        let binary = classify_binary("/usr/local/bin/tailscale", "definitely not tailscale");
        assert!(!binary.authentic);
        assert_eq!(binary.parsed_version, None);
    }

    #[test]
    fn current_tailscale_long_version_output_is_authentic() {
        let binary = classify_binary(
            "/opt/homebrew/bin/tailscale",
            "1.98.5\n  tailscale commit: 295179bf294d3d076397bcef6815b1d6854e197d\n  long version: 1.98.5-t295179bf2\n  go version: go1.26.3\n",
        );
        assert!(binary.authentic);
        assert_eq!(binary.parsed_version.as_deref(), Some("1.98.5"));
    }

    #[test]
    fn mismatched_long_version_output_is_inauthentic() {
        let binary = classify_binary(
            "/opt/homebrew/bin/tailscale",
            "1.98.5\n  tailscale commit: 295179bf294d3d076397bcef6815b1d6854e197d\n  long version: 1.98.6-t295179bf2\n  go version: go1.26.3\n",
        );
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

        assert_eq!(body.as_slice(), br#"{"BackendState":"Running"}"#);
        Ok(())
    }

    #[test]
    fn localapi_http_response_body_decodes_chunked_payload() -> TestResult {
        let body = http_response_body(
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nf\r\n{\"BackendState\"\r\nb\r\n:\"Running\"}\r\n0\r\n\r\n",
        )?;

        assert_eq!(body.as_slice(), br#"{"BackendState":"Running"}"#);
        Ok(())
    }

    #[test]
    fn localapi_http_response_body_rejects_malformed_chunk_size() {
        let error =
            http_response_body(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nzz\r\n{}")
                .expect_err("malformed chunk size should fail");

        assert!(error.contains("invalid chunk size `zz`"));
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

    #[test]
    fn status_user_map_is_joined_to_self_and_peer_user_ids() {
        let report = classify(
            r#"{
              "BackendState": "Running",
              "Self": {
                "ID":"nodekey:self",
                "Authenticated":true,
                "TailscaleIPs":["100.64.0.10"],
                "Platform":"linux",
                "UserID": 11
              },
              "Peer": {
                "nodekey:alpha": {
                  "ID": "nodekey:alpha",
                  "UserID": 22,
                  "HostName": "alpha",
                  "TailscaleIPs": ["100.64.0.20"]
                }
              },
              "User": {
                "11": {
                  "ID": 11,
                  "LoginName": "alice@acme.com",
                  "DisplayName": "Alice"
                },
                "22": {
                  "ID": 22,
                  "LoginName": "bob@acme.com",
                  "DisplayName": "Bob"
                }
              }
            }"#,
        );
        let self_owner = report.self_owner.as_ref().expect("self owner");
        assert_eq!(self_owner.user_id, "11");
        assert_eq!(self_owner.login_name, "alice@acme.com");
        assert_eq!(self_owner.display_name.as_deref(), Some("Alice"));
        let peer = &report.peers[0];
        let owner = peer.owner.as_ref().expect("peer owner");
        assert_eq!(owner.user_id, "22");
        assert_eq!(owner.login_name, "bob@acme.com");
    }

    #[test]
    fn nested_user_profile_is_accepted_when_user_map_is_absent() {
        let report = classify(
            r#"{
              "BackendState": "Running",
              "Self": {
                "ID":"nodekey:self",
                "Authenticated":true,
                "Platform":"linux",
                "UserProfile": {
                  "ID": "77",
                  "LoginName": "owner@example.test",
                  "DisplayName": "Owner"
                }
              }
            }"#,
        );
        let owner = report.self_owner.as_ref().expect("nested owner");
        assert_eq!(owner.user_id, "77");
        assert_eq!(owner.login_name, "owner@example.test");
        assert_eq!(owner.display_name.as_deref(), Some("Owner"));
    }

    #[test]
    fn missing_user_map_leaves_owner_unset() {
        let report = classify(
            r#"{
              "BackendState": "Running",
              "Self": {
                "ID":"nodekey:self",
                "Authenticated":true,
                "UserID": 99,
                "Platform":"linux"
              },
              "Peer": {
                "nodekey:alpha": {
                  "ID": "nodekey:alpha",
                  "UserID": 99
                }
              }
            }"#,
        );
        assert!(report.self_owner.is_none());
        assert!(report.peers[0].owner.is_none());
    }

    #[test]
    fn key_rotation_keeps_the_same_owner_while_reassignment_changes_login() {
        let first = classify(
            r#"{
              "BackendState": "Running",
              "Self": {"ID":"nodekey:self","Authenticated":true,"UserID":1,"Platform":"linux"},
              "Peer": {
                "nodekey:old": {"ID":"nodekey:old","UserID":1,"HostName":"peer"}
              },
              "User": {"1": {"ID":1,"LoginName":"alice@acme.com","DisplayName":"Alice"}}
            }"#,
        );
        let rotated = classify(
            r#"{
              "BackendState": "Running",
              "Self": {"ID":"nodekey:self","Authenticated":true,"UserID":1,"Platform":"linux"},
              "Peer": {
                "nodekey:new": {"ID":"nodekey:new","UserID":1,"HostName":"peer"}
              },
              "User": {"1": {"ID":1,"LoginName":"alice@acme.com","DisplayName":"Alice"}}
            }"#,
        );
        let reassigned = classify(
            r#"{
              "BackendState": "Running",
              "Self": {"ID":"nodekey:self","Authenticated":true,"UserID":2,"Platform":"linux"},
              "Peer": {
                "nodekey:new": {"ID":"nodekey:new","UserID":2,"HostName":"peer"}
              },
              "User": {"2": {"ID":2,"LoginName":"mallory@acme.com","DisplayName":"Mallory"}}
            }"#,
        );
        let first_login = first.peers[0]
            .owner
            .as_ref()
            .expect("first")
            .login_name
            .clone();
        let rotated_login = rotated.peers[0]
            .owner
            .as_ref()
            .expect("rotated")
            .login_name
            .clone();
        let reassigned_login = reassigned.peers[0]
            .owner
            .as_ref()
            .expect("reassigned")
            .login_name
            .clone();
        assert_eq!(first_login, "alice@acme.com");
        assert_eq!(rotated_login, first_login);
        assert_eq!(reassigned_login, "mallory@acme.com");
        assert_eq!(
            evaluate_tailnet_owner(
                rotated.peers[0].owner.as_ref(),
                Some(&first_login),
                Some("acme.com"),
            ),
            TailnetOwnerDisposition::Attested
        );
        assert_eq!(
            evaluate_tailnet_owner(
                reassigned.peers[0].owner.as_ref(),
                Some(&first_login),
                Some("acme.com"),
            ),
            TailnetOwnerDisposition::Reassigned
        );
        assert_eq!(
            evaluate_tailnet_owner(reassigned.peers[0].owner.as_ref(), None, Some("other.com"),),
            TailnetOwnerDisposition::DomainMismatch
        );
        assert_eq!(
            evaluate_tailnet_owner(None, Some("alice@acme.com"), Some("acme.com")),
            TailnetOwnerDisposition::Missing
        );
    }
}
