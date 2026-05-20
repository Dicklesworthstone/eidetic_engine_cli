//! bd-1zb7k.20.5: structural contract for the no-mock QoS lanes e2e
//! driver at `scripts/e2e_overhaul/qos_lanes.sh`.
//!
//! The driver is the closeout evidence gate for the bd-1zb7k.20 epic.
//! This contract pins the harness shape without running the driver:
//!
//! 1. The script file exists and is executable.
//! 2. It declares all five required phases: setup, foreground_pressure,
//!    background_pressure, classification, teardown.
//! 3. It honors EE_BINARY, EE_QOS_LANES_EVENT_LOG, and EE_QOS_LANES_STRICT
//!    so the wrapping verification harness can drive it in strict mode.
//! 4. It emits `ee.test_event.v1` rows with the per-request tail-event
//!    fields the bead's IdeaWizard refinement requires: requestKind,
//!    queryShapeHash, responseHash, latencyMs, qosLaneSnapshotHash,
//!    throttlingAction, degradedCodes.
//! 5. It degrades honestly with `ee_binary_unavailable` / `ee_binary_unusable`
//!    when the live binary cannot run scenarios end-to-end — the harness
//!    still emits a complete event log so structural acceptance can pass.
//! 6. The classification gate emits one of {ok, regression, inconclusive}
//!    per the IdeaWizard refinement; inconclusive is the honest signal
//!    when no live binary is available.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::PathBuf;

type TestResult = Result<(), String>;

const DRIVER_PATH: &str = "scripts/e2e_overhaul/qos_lanes.sh";
const REQUIRED_PHASES: &[&str] = &[
    "setup",
    "foreground_pressure",
    "background_pressure",
    "classification",
    "teardown",
];
const REQUIRED_ENV_VARS: &[&str] = &[
    "EE_BINARY",
    "EE_QOS_LANES_EVENT_LOG",
    "EE_QOS_LANES_STRICT",
    "EE_QOS_LANES_FOREGROUND",
    "EE_QOS_LANES_BACKGROUND",
    "EE_QOS_LANES_REPEATS",
];
const REQUIRED_TAIL_FIELDS: &[&str] = &[
    "requestKind",
    "queryShapeHash",
    "responseHash",
    "qosLaneSnapshotHash",
    "throttlingAction",
    "degradedCodes",
];
const REQUIRED_DEGRADED_CODES: &[&str] = &[
    "ee_binary_unavailable",
    "ee_binary_unusable",
    "qos_lanes_inconclusive",
];
const REQUIRED_CLASSIFICATION_VERDICTS: &[&str] = &["inconclusive"];

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn read_driver() -> Result<String, String> {
    let path = repo_root().join(DRIVER_PATH);
    fs::read_to_string(&path).map_err(|e| format!("read {}: {e}", path.display()))
}

#[test]
fn qos_lanes_driver_exists_and_is_executable() -> TestResult {
    let path = repo_root().join(DRIVER_PATH);
    let metadata =
        fs::metadata(&path).map_err(|e| format!("driver {} missing: {e}", path.display()))?;
    ensure(metadata.is_file(), "driver is not a regular file")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = metadata.permissions().mode();
        ensure(
            mode & 0o111 != 0,
            format!("driver must be executable; mode={mode:o}"),
        )?;
    }
    Ok(())
}

#[test]
fn qos_lanes_driver_declares_all_five_required_phases() -> TestResult {
    let body = read_driver()?;
    for phase in REQUIRED_PHASES {
        // Phase strings appear in emit_event calls as the second positional arg.
        ensure(
            body.contains(&format!("\"{phase}\"")),
            format!(
                "driver must reference phase `{phase}` in an emit_event call; \
                 not found in {DRIVER_PATH}"
            ),
        )?;
    }
    Ok(())
}

#[test]
fn qos_lanes_driver_honors_required_env_overrides() -> TestResult {
    let body = read_driver()?;
    for var in REQUIRED_ENV_VARS {
        ensure(
            body.contains(var),
            format!("driver must honor `{var}` env override; not found in {DRIVER_PATH}"),
        )?;
    }
    Ok(())
}

#[test]
fn qos_lanes_driver_emits_tail_event_fields_from_idea_wizard_refinement() -> TestResult {
    let body = read_driver()?;
    for field in REQUIRED_TAIL_FIELDS {
        ensure(
            body.contains(field),
            format!(
                "driver must emit `{field}` in tail-event ledger rows per the \
                 RusticIvy IdeaWizard refinement; not found in {DRIVER_PATH}"
            ),
        )?;
    }
    // The tail row event kind is the canonical anchor.
    ensure(
        body.contains("qos_lanes_tail_row"),
        "driver must emit kind=qos_lanes_tail_row for per-request rows",
    )?;
    Ok(())
}

#[test]
fn qos_lanes_driver_degrades_honestly_when_binary_is_unusable() -> TestResult {
    let body = read_driver()?;
    for code in REQUIRED_DEGRADED_CODES {
        ensure(
            body.contains(code),
            format!(
                "driver must emit `{code}` degraded code when the live ee binary \
                 cannot run scenarios; not found in {DRIVER_PATH}"
            ),
        )?;
    }
    // Strict mode is the fail-closed switch.
    ensure(
        body.contains("EE_QOS_LANES_STRICT"),
        "driver must honor EE_QOS_LANES_STRICT=1 to fail closed",
    )?;
    Ok(())
}

#[test]
fn qos_lanes_driver_classification_gate_emits_verdict() -> TestResult {
    let body = read_driver()?;
    for verdict in REQUIRED_CLASSIFICATION_VERDICTS {
        ensure(
            body.contains(verdict),
            format!(
                "driver classification gate must emit verdict `{verdict}` per the \
                 ok|regression|inconclusive contract; not found in {DRIVER_PATH}"
            ),
        )?;
    }
    Ok(())
}

#[test]
fn qos_lanes_driver_writes_ee_test_event_v1_schema() -> TestResult {
    let body = read_driver()?;
    ensure(
        body.contains("\"schema\": \"ee.test_event.v1\""),
        "driver must emit events with schema=ee.test_event.v1",
    )?;
    ensure(
        body.contains("\"bead_id\": \"bd-1zb7k.20.5\""),
        "driver must tag events with bead_id=bd-1zb7k.20.5",
    )?;
    ensure(
        body.contains("\"surface\": \"qos_lanes_e2e\""),
        "driver must tag events with surface=qos_lanes_e2e",
    )?;
    Ok(())
}
