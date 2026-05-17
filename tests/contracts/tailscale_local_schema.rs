//! Contract checks for the SRR6.46.1 Tailscale local-probe status block.

use std::fs;
use std::path::PathBuf;

use ee::core::tailscale_probe::{
    TAILSCALE_BINARY_INAUTHENTIC_CODE, TAILSCALE_DAEMON_UNREACHABLE_CODE,
    TAILSCALE_LOCAL_SCHEMA_V1, TAILSCALE_NOT_AUTHENTICATED_CODE, TAILSCALE_NOT_INSTALLED_CODE,
    TAILSCALE_PROBE_TIMEOUT_CODE, TAILSCALE_PROBE_UNAVAILABLE_CODE, TAILSCALE_SHIELDS_UP_CODE,
};
use serde_json::Value;

type TestResult = Result<(), String>;

const SCHEMA_PATH: &str = "docs/schemas/ee.tailscale.local.v1.json";
const FAILURE_FIXTURES: &[(&str, &str, &str)] = &[
    (
        "tests/fixtures/failure_modes/tailscale_not_installed.json",
        TAILSCALE_NOT_INSTALLED_CODE,
        "warning",
    ),
    (
        "tests/fixtures/failure_modes/tailscale_daemon_unreachable.json",
        TAILSCALE_DAEMON_UNREACHABLE_CODE,
        "warning",
    ),
    (
        "tests/fixtures/failure_modes/tailscale_not_authenticated.json",
        TAILSCALE_NOT_AUTHENTICATED_CODE,
        "warning",
    ),
    (
        "tests/fixtures/failure_modes/tailscale_binary_inauthentic.json",
        TAILSCALE_BINARY_INAUTHENTIC_CODE,
        "high",
    ),
    (
        "tests/fixtures/failure_modes/tailscale_shields_up.json",
        TAILSCALE_SHIELDS_UP_CODE,
        "warning",
    ),
    (
        "tests/fixtures/failure_modes/tailscale_probe_unavailable.json",
        TAILSCALE_PROBE_UNAVAILABLE_CODE,
        "info",
    ),
    (
        "tests/fixtures/failure_modes/tailscale_probe_timeout.json",
        TAILSCALE_PROBE_TIMEOUT_CODE,
        "warning",
    ),
];

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read_json(relative: &str) -> Result<Value, String> {
    let path = repo_root().join(relative);
    let text =
        fs::read_to_string(&path).map_err(|error| format!("read {}: {error}", path.display()))?;
    serde_json::from_str(&text).map_err(|error| format!("parse {}: {error}", path.display()))
}

fn ensure(condition: bool, context: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(context.into())
    }
}

fn ensure_str(value: Option<&str>, expected: &str, context: &str) -> TestResult {
    if value == Some(expected) {
        Ok(())
    } else {
        Err(format!("{context}: expected {expected:?}, got {value:?}"))
    }
}

#[test]
fn tailscale_local_schema_pins_mesh_safe_status_shape() -> TestResult {
    let schema = read_json(SCHEMA_PATH)?;

    ensure_str(
        schema.pointer("/title").and_then(Value::as_str),
        TAILSCALE_LOCAL_SCHEMA_V1,
        "schema title",
    )?;
    ensure_str(
        schema
            .pointer("/properties/schema/const")
            .and_then(Value::as_str),
        TAILSCALE_LOCAL_SCHEMA_V1,
        "schema const",
    )?;
    ensure(
        schema
            .pointer("/required")
            .and_then(Value::as_array)
            .is_some_and(|required| {
                required
                    .iter()
                    .any(|field| field.as_str() == Some("binaryAuthentic"))
                    && required
                        .iter()
                        .any(|field| field.as_str() == Some("degraded"))
                    && required.iter().any(|field| field.as_str() == Some("peers"))
            }),
        "schema must require binaryAuthentic, peers, and degraded",
    )?;
    ensure_str(
        schema
            .pointer("/properties/peers/items/$ref")
            .and_then(Value::as_str),
        "#/$defs/peer",
        "peers item ref",
    )?;
    ensure(
        schema
            .pointer("/$defs/peer/additionalProperties")
            .and_then(Value::as_bool)
            == Some(false),
        "peer schema must reject additional properties",
    )?;
    for field in ["peerNodeKey", "peerTailscaleIps", "peerAdvertisedTags"] {
        ensure(
            schema
                .pointer("/$defs/peer/required")
                .and_then(Value::as_array)
                .is_some_and(|required| required.iter().any(|item| item.as_str() == Some(field))),
            format!("peer schema missing required field {field}"),
        )?;
    }

    let degraded_codes = schema
        .pointer("/$defs/degradation/properties/code/enum")
        .and_then(Value::as_array)
        .ok_or_else(|| "missing degradation code enum".to_owned())?;
    for (_, code, _) in FAILURE_FIXTURES {
        ensure(
            degraded_codes
                .iter()
                .any(|value| value.as_str() == Some(*code)),
            format!("schema missing degraded code {code}"),
        )?;
    }
    Ok(())
}

#[test]
fn tailscale_failure_fixtures_match_probe_codes_and_severity() -> TestResult {
    for (path, code, severity) in FAILURE_FIXTURES {
        let fixture = read_json(path)?;
        ensure_str(
            fixture.get("schema").and_then(Value::as_str),
            "ee.failure_mode_fixture.v1",
            path,
        )?;
        ensure_str(fixture.get("code").and_then(Value::as_str), code, path)?;
        ensure_str(
            fixture.get("severity").and_then(Value::as_str),
            severity,
            path,
        )?;
        ensure(
            fixture
                .get("surfaces")
                .and_then(Value::as_array)
                .is_some_and(|surfaces| {
                    surfaces.iter().any(|item| item.as_str() == Some("status"))
                }),
            format!("{path}: status surface missing"),
        )?;
        ensure_str(
            fixture
                .pointer("/expected_emission/code")
                .and_then(Value::as_str),
            code,
            path,
        )?;
    }
    Ok(())
}
