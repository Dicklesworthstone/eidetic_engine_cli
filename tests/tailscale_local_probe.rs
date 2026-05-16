use std::path::Path;

use ee::core::tailscale_probe::{
    TAILSCALE_BINARY_INAUTHENTIC_CODE, TAILSCALE_DAEMON_UNREACHABLE_CODE,
    TAILSCALE_LOCAL_SCHEMA_V1, TAILSCALE_NOT_AUTHENTICATED_CODE, TAILSCALE_PROBE_TIMEOUT_CODE,
    TailscaleBinaryReport, TailscaleLocalReport, TailscalePlatform, TailscaleProbeMethod,
    TailscaleStatusProbeInput, classify_binary, classify_status_payload, validate_binary_path,
};

type TestResult = Result<(), String>;

fn good_binary() -> TailscaleBinaryReport {
    classify_binary(
        "/opt/homebrew/bin/tailscale",
        "1.66.0\n  tailscale commit: 0123456789abcdef0123456789abcdef01234567\n  other commit: 89abcdef0123456789abcdef0123456789abcdef\n  go version: go1.22.3\n",
    )
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
fn local_probe_timeout_report_is_deterministic() -> TestResult {
    let first = TailscaleLocalReport::timed_out(TailscaleProbeMethod::Cli, 1_501);
    let second = TailscaleLocalReport::timed_out(TailscaleProbeMethod::Cli, 1_501);
    assert_eq!(first, second);
    assert_eq!(first.degradations[0].code, TAILSCALE_PROBE_TIMEOUT_CODE);
    Ok(())
}
