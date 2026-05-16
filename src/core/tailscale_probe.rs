//! Pure Tailscale local-probe model and parsers for SRR6.46.1.
//!
//! This module deliberately does not execute `tailscale`, connect to
//! tailscaled, or open network sockets. It classifies already-collected status
//! and prefs payloads so CLI/status/doctor surfaces can share one deterministic
//! interpretation layer.

use std::path::{Path, PathBuf};

use serde_json::Value;

pub const TAILSCALE_LOCAL_SCHEMA_V1: &str = "ee.tailscale.local.v1";

pub const TAILSCALE_NOT_INSTALLED_CODE: &str = "tailscale_not_installed";
pub const TAILSCALE_DAEMON_UNREACHABLE_CODE: &str = "tailscale_daemon_unreachable";
pub const TAILSCALE_NOT_AUTHENTICATED_CODE: &str = "tailscale_not_authenticated";
pub const TAILSCALE_BINARY_INAUTHENTIC_CODE: &str = "tailscale_binary_inauthentic";
pub const TAILSCALE_SHIELDS_UP_CODE: &str = "tailscale_shields_up";
pub const TAILSCALE_PROBE_UNAVAILABLE_CODE: &str = "tailscale_probe_unavailable";
pub const TAILSCALE_PROBE_TIMEOUT_CODE: &str = "tailscale_probe_timeout";

const DEFAULT_PROBE_TIMEOUT_MS: u64 = 1_500;

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
            format!("Tailscale probe exceeded the {DEFAULT_PROBE_TIMEOUT_MS}ms default budget."),
            "Run tailscale status directly or raise EE_TAILSCALE_PROBE_TIMEOUT_MS.",
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

#[must_use]
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
        || !go_version.starts_with("go version:")
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
