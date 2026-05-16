use std::path::Path;

use ee::core::status::StatusReport;
use ee::core::tailscale_probe::{
    TAILSCALE_BINARY_INAUTHENTIC_CODE, TAILSCALE_DAEMON_UNREACHABLE_CODE,
    TAILSCALE_LOCAL_SCHEMA_V1, TAILSCALE_NOT_AUTHENTICATED_CODE, TAILSCALE_PROBE_TIMEOUT_CODE,
    TAILSCALE_PROBE_UNAVAILABLE_CODE, TailscaleBinaryReport, TailscaleCliCommandOutput,
    TailscaleCliProbeConfig, TailscaleCliProbeRunner, TailscaleLocalReport, TailscalePlatform,
    TailscaleProbeMethod, TailscaleStatusProbeInput, classify_binary, classify_status_payload,
    probe_tailscale_cli_with_runner, tailscale_probe_timeout_ms_from_env_value,
    validate_binary_path,
};
use ee::output::render_status_json;

type TestResult = Result<(), String>;

#[derive(Clone, Debug, Default)]
struct FakeCliRunner {
    existing_paths: Vec<String>,
    version: Option<TailscaleCliCommandOutput>,
    status: Option<TailscaleCliCommandOutput>,
    prefs: Option<TailscaleCliCommandOutput>,
    calls: Vec<String>,
}

impl FakeCliRunner {
    fn with_existing(path: &str) -> Self {
        Self {
            existing_paths: vec![path.to_owned()],
            ..Self::default()
        }
    }
}

impl TailscaleCliProbeRunner for FakeCliRunner {
    fn binary_exists(&self, path: &Path) -> bool {
        self.existing_paths
            .iter()
            .any(|candidate| Path::new(candidate) == path)
    }

    fn run(&mut self, _path: &Path, args: &[&str], _timeout_ms: u64) -> TailscaleCliCommandOutput {
        self.calls.push(args.join(" "));
        match args {
            ["--version"] => self
                .version
                .clone()
                .unwrap_or_else(|| TailscaleCliCommandOutput::failure("missing version", 1)),
            ["status", "--json", "--self=true", "--peers=true"] => self
                .status
                .clone()
                .unwrap_or_else(|| TailscaleCliCommandOutput::failure("missing status", 1)),
            ["debug", "localapi", "/localapi/v0/prefs"] => self
                .prefs
                .clone()
                .unwrap_or_else(|| TailscaleCliCommandOutput::failure("missing prefs", 1)),
            _ => TailscaleCliCommandOutput::failure("unexpected args", 1),
        }
    }
}

fn good_binary() -> TailscaleBinaryReport {
    classify_binary("/opt/homebrew/bin/tailscale", good_version_output())
}

fn good_version_output() -> &'static str {
    "1.66.0\n  tailscale commit: 0123456789abcdef0123456789abcdef01234567\n  other commit: 89abcdef0123456789abcdef0123456789abcdef\n  go version: go1.22.3\n"
}

fn cli_probe_config(binary_path: &str) -> TailscaleCliProbeConfig {
    TailscaleCliProbeConfig {
        mesh_enabled: true,
        binary_override: None,
        binary_candidates: vec![Path::new(binary_path).to_path_buf()],
        timeout_ms: 1_500,
        platform_hint: TailscalePlatform::Linux,
    }
}

fn healthy_status() -> &'static [u8] {
    br#"{
      "Version": "fake-tailscale.v1",
      "BackendState": "Running",
      "UnexpectedFutureField": {"must": "be ignored"},
      "Self": {
        "ID": "nodekey:selfalpha",
        "HostName": "ee-local",
        "DNSName": "ee-local.tailnet.test.",
        "TailscaleIPs": ["100.64.0.10"],
        "Tailnet": "tailnet-alpha",
        "TailnetName": "alpha.example",
        "Authenticated": true,
        "Platform": "linux",
        "Tags": ["tag:ee-mesh"]
      },
      "Peer": {}
    }"#
}

#[test]
fn local_probe_returns_authenticated_self_when_status_json_well_formed() -> TestResult {
    let report = classify_status_payload(TailscaleStatusProbeInput {
        status_json: healthy_status(),
        prefs_json: Some(br#"{"ShieldsUp": false}"#),
        binary: Some(good_binary()),
        method: TailscaleProbeMethod::Cli,
        elapsed_ms: 14,
        platform_hint: TailscalePlatform::Other,
    });
    assert_eq!(report.schema, TAILSCALE_LOCAL_SCHEMA_V1);
    assert!(report.installed);
    assert!(report.daemon_reachable);
    assert!(report.authenticated);
    assert!(report.binary_authentic);
    assert_eq!(report.version.as_deref(), Some("1.66.0"));
    assert_eq!(report.tailnet_id.as_deref(), Some("tailnet-alpha"));
    assert_eq!(report.self_node_key.as_deref(), Some("nodekey:selfalpha"));
    assert_eq!(report.self_tailscale_ip.as_deref(), Some("100.64.0.10"));
    assert_eq!(
        report.self_magic_dns_name.as_deref(),
        Some("ee-local.tailnet.test.")
    );
    assert_eq!(report.self_advertised_tags, vec!["tag:ee-mesh"]);
    assert_eq!(report.platform, TailscalePlatform::Linux);
    assert!(report.degradations.is_empty(), "{:?}", report.degradations);
    Ok(())
}

#[test]
fn local_probe_returns_not_authenticated_when_logged_out() -> TestResult {
    let report = classify_status_payload(TailscaleStatusProbeInput {
        status_json: br#"{
          "BackendState": "NeedsLogin",
          "AuthURL": "https://login.tailscale.test",
          "Self": {
            "ID": "nodekey:selfalpha",
            "TailscaleIPs": ["100.64.0.10"],
            "Authenticated": false,
            "Platform": "linux"
          }
        }"#,
        prefs_json: None,
        binary: Some(good_binary()),
        method: TailscaleProbeMethod::Cli,
        elapsed_ms: 11,
        platform_hint: TailscalePlatform::Linux,
    });
    assert!(report.daemon_reachable);
    assert!(!report.authenticated);
    assert!(
        report
            .degradations
            .iter()
            .any(|entry| entry.code == TAILSCALE_NOT_AUTHENTICATED_CODE)
    );
    Ok(())
}

#[test]
fn local_probe_treats_malformed_status_json_as_daemon_unreachable_not_panic() -> TestResult {
    let report = classify_status_payload(TailscaleStatusProbeInput {
        status_json: b"{\"BackendState\":\"Running\",\"Peer\":",
        prefs_json: None,
        binary: Some(good_binary()),
        method: TailscaleProbeMethod::Cli,
        elapsed_ms: 4,
        platform_hint: TailscalePlatform::Linux,
    });
    assert!(report.installed);
    assert!(!report.daemon_reachable);
    assert_eq!(
        report.degradations[0].code,
        TAILSCALE_DAEMON_UNREACHABLE_CODE
    );
    Ok(())
}

#[test]
fn local_probe_skipped_when_mesh_disabled() -> TestResult {
    let report = TailscaleLocalReport::mesh_disabled();
    assert_eq!(report.probe_method, TailscaleProbeMethod::Skipped);
    assert!(!report.installed);
    assert_eq!(report.degradations.len(), 1);
    Ok(())
}

#[test]
fn local_probe_rejects_relative_tailscale_path_on_path_hijack_fixture() -> TestResult {
    let err = validate_binary_path(Path::new("tailscale")).expect_err("relative path rejected");
    assert_eq!(err.code, TAILSCALE_BINARY_INAUTHENTIC_CODE);
    Ok(())
}

#[test]
fn local_probe_rejects_binary_with_malformed_version_output() -> TestResult {
    let binary = classify_binary("/usr/local/bin/tailscale", "not tailscale\n");
    assert!(!binary.authentic);
    let report = classify_status_payload(TailscaleStatusProbeInput {
        status_json: healthy_status(),
        prefs_json: None,
        binary: Some(binary),
        method: TailscaleProbeMethod::Cli,
        elapsed_ms: 14,
        platform_hint: TailscalePlatform::Linux,
    });
    assert!(
        report
            .degradations
            .iter()
            .any(|entry| entry.code == TAILSCALE_BINARY_INAUTHENTIC_CODE)
    );
    Ok(())
}

#[test]
fn local_probe_accepts_binary_with_well_formed_version_output() -> TestResult {
    let binary = good_binary();
    assert!(binary.authentic);
    assert_eq!(binary.parsed_version.as_deref(), Some("1.66.0"));
    Ok(())
}

#[test]
fn local_probe_reports_shields_up_when_prefs_shields_up_true() -> TestResult {
    let report = classify_status_payload(TailscaleStatusProbeInput {
        status_json: healthy_status(),
        prefs_json: Some(br#"{"ShieldsUp": true}"#),
        binary: Some(good_binary()),
        method: TailscaleProbeMethod::Socket,
        elapsed_ms: 9,
        platform_hint: TailscalePlatform::Linux,
    });
    assert_eq!(report.shields_up, Some(true));
    assert!(
        report
            .degradations
            .iter()
            .any(|entry| entry.code == "tailscale_shields_up")
    );
    Ok(())
}

#[test]
fn cli_probe_short_circuits_without_runner_calls_when_mesh_disabled() -> TestResult {
    let mut runner = FakeCliRunner::with_existing("/opt/homebrew/bin/tailscale");
    let report =
        probe_tailscale_cli_with_runner(&TailscaleCliProbeConfig::mesh_disabled(), &mut runner);

    assert_eq!(report.probe_method, TailscaleProbeMethod::Skipped);
    assert!(runner.calls.is_empty());
    assert_eq!(
        report.degradations[0].code,
        TAILSCALE_PROBE_UNAVAILABLE_CODE
    );
    Ok(())
}

#[test]
fn cli_probe_reports_not_installed_when_no_binary_candidate_exists() -> TestResult {
    let mut runner = FakeCliRunner::default();
    let report = probe_tailscale_cli_with_runner(
        &cli_probe_config("/opt/homebrew/bin/tailscale"),
        &mut runner,
    );

    assert!(!report.installed);
    assert!(runner.calls.is_empty());
    assert_eq!(report.degradations[0].code, "tailscale_not_installed");
    Ok(())
}

#[test]
fn cli_probe_rejects_relative_override_without_running_it() -> TestResult {
    let mut runner = FakeCliRunner::with_existing("tailscale");
    let mut config = cli_probe_config("/opt/homebrew/bin/tailscale");
    config.binary_override = Some(Path::new("tailscale").to_path_buf());
    let report = probe_tailscale_cli_with_runner(&config, &mut runner);

    assert!(report.installed);
    assert!(runner.calls.is_empty());
    assert_eq!(
        report.degradations[0].code,
        TAILSCALE_BINARY_INAUTHENTIC_CODE
    );
    Ok(())
}

#[test]
fn cli_probe_reports_timeout_when_version_command_times_out() -> TestResult {
    let mut runner = FakeCliRunner::with_existing("/opt/homebrew/bin/tailscale");
    runner.version = Some(TailscaleCliCommandOutput::timeout(1_501));

    let report = probe_tailscale_cli_with_runner(
        &cli_probe_config("/opt/homebrew/bin/tailscale"),
        &mut runner,
    );

    assert_eq!(report.degradations[0].code, TAILSCALE_PROBE_TIMEOUT_CODE);
    assert_eq!(runner.calls, vec!["--version"]);
    Ok(())
}

#[test]
fn cli_probe_reports_timeout_when_status_command_times_out() -> TestResult {
    let mut runner = FakeCliRunner::with_existing("/opt/homebrew/bin/tailscale");
    runner.version = Some(TailscaleCliCommandOutput::success(good_version_output(), 3));
    runner.status = Some(TailscaleCliCommandOutput::timeout(1_501));

    let report = probe_tailscale_cli_with_runner(
        &cli_probe_config("/opt/homebrew/bin/tailscale"),
        &mut runner,
    );

    assert_eq!(report.degradations[0].code, TAILSCALE_PROBE_TIMEOUT_CODE);
    assert_eq!(
        runner.calls,
        vec!["--version", "status --json --self=true --peers=true"]
    );
    Ok(())
}

#[test]
fn cli_probe_reports_binary_inauthentic_when_version_output_is_malformed() -> TestResult {
    let mut runner = FakeCliRunner::with_existing("/opt/homebrew/bin/tailscale");
    runner.version = Some(TailscaleCliCommandOutput::success("not tailscale", 3));

    let report = probe_tailscale_cli_with_runner(
        &cli_probe_config("/opt/homebrew/bin/tailscale"),
        &mut runner,
    );

    assert_eq!(
        report.degradations[0].code,
        TAILSCALE_BINARY_INAUTHENTIC_CODE
    );
    assert_eq!(report.binary_version_raw.as_deref(), Some("not tailscale"));
    assert_eq!(runner.calls, vec!["--version"]);
    Ok(())
}

#[test]
fn cli_probe_reports_daemon_unreachable_when_status_command_fails() -> TestResult {
    let mut runner = FakeCliRunner::with_existing("/opt/homebrew/bin/tailscale");
    runner.version = Some(TailscaleCliCommandOutput::success(good_version_output(), 3));
    runner.status = Some(TailscaleCliCommandOutput::failure("daemon offline", 4));

    let report = probe_tailscale_cli_with_runner(
        &cli_probe_config("/opt/homebrew/bin/tailscale"),
        &mut runner,
    );

    assert_eq!(
        report.degradations[0].code,
        TAILSCALE_DAEMON_UNREACHABLE_CODE
    );
    assert!(report.degradations[0].message.contains("daemon offline"));
    Ok(())
}

#[test]
fn cli_probe_classifies_healthy_status_and_prefs_payloads() -> TestResult {
    let mut runner = FakeCliRunner::with_existing("/opt/homebrew/bin/tailscale");
    runner.version = Some(TailscaleCliCommandOutput::success(good_version_output(), 3));
    runner.status = Some(TailscaleCliCommandOutput::success(healthy_status(), 4));
    runner.prefs = Some(TailscaleCliCommandOutput::success(
        br#"{"ShieldsUp": false}"#.as_slice(),
        5,
    ));

    let report = probe_tailscale_cli_with_runner(
        &cli_probe_config("/opt/homebrew/bin/tailscale"),
        &mut runner,
    );

    assert!(report.installed);
    assert!(report.daemon_reachable);
    assert!(report.authenticated);
    assert!(report.binary_authentic);
    assert_eq!(report.shields_up, Some(false));
    assert_eq!(report.probe_elapsed_ms, 12);
    assert_eq!(
        runner.calls,
        vec![
            "--version",
            "status --json --self=true --peers=true",
            "debug localapi /localapi/v0/prefs"
        ]
    );
    Ok(())
}

#[test]
fn cli_probe_timeout_env_parser_uses_default_for_missing_or_invalid_values() -> TestResult {
    assert_eq!(tailscale_probe_timeout_ms_from_env_value(None), 1_500);
    assert_eq!(tailscale_probe_timeout_ms_from_env_value(Some("")), 1_500);
    assert_eq!(tailscale_probe_timeout_ms_from_env_value(Some("0")), 1_500);
    assert_eq!(
        tailscale_probe_timeout_ms_from_env_value(Some("abc")),
        1_500
    );
    assert_eq!(
        tailscale_probe_timeout_ms_from_env_value(Some("2500")),
        2_500
    );
    Ok(())
}

#[test]
fn local_probe_timeout_report_is_deterministic() -> TestResult {
    let first = TailscaleLocalReport::timed_out(TailscaleProbeMethod::Cli, 1_501);
    let second = TailscaleLocalReport::timed_out(TailscaleProbeMethod::Cli, 1_501);
    assert_eq!(first, second);
    assert_eq!(first.degradations[0].code, TAILSCALE_PROBE_TIMEOUT_CODE);
    Ok(())
}

#[test]
fn status_json_embeds_tailscale_report_under_mesh_block() -> TestResult {
    let tailscale = classify_status_payload(TailscaleStatusProbeInput {
        status_json: healthy_status(),
        prefs_json: Some(br#"{"ShieldsUp": false}"#),
        binary: Some(good_binary()),
        method: TailscaleProbeMethod::Cli,
        elapsed_ms: 14,
        platform_hint: TailscalePlatform::Linux,
    });
    let mut report = StatusReport::gather();
    report.tailscale_local = Some(tailscale);

    let rendered = render_status_json(&report);
    let value: serde_json::Value =
        serde_json::from_str(&rendered).map_err(|error| format!("parse status JSON: {error}"))?;
    let mesh = value
        .pointer("/data/mesh/tailscale")
        .ok_or_else(|| "missing /data/mesh/tailscale".to_owned())?;

    assert_eq!(
        mesh.get("schema").and_then(serde_json::Value::as_str),
        Some(TAILSCALE_LOCAL_SCHEMA_V1)
    );
    assert_eq!(
        mesh.get("tailnetId").and_then(serde_json::Value::as_str),
        Some("tailnet-alpha")
    );
    assert_eq!(
        mesh.get("selfTailscaleIp")
            .and_then(serde_json::Value::as_str),
        Some("100.64.0.10")
    );
    assert_eq!(
        mesh.get("probeMethod").and_then(serde_json::Value::as_str),
        Some("cli")
    );
    Ok(())
}
