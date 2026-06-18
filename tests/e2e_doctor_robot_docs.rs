//! bd-33i48: real-binary pin test for `ee doctor --robot-docs --json`.
//!
//! The robot-docs route is an agent-facing discovery surface for doctor
//! subcommands. CLI unit coverage pins the formatter; this E2E exercises the
//! compiled binary so argument routing and the JSON envelope stay stable.

use std::process::{Command, Output};

use serde_json::Value;

type TestResult = Result<(), String>;

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn run_ee(args: &[&str]) -> Result<Output, String> {
    Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(args)
        .env("EE_NO_COLOR", "1")
        .output()
        .map_err(|error| format!("failed to run ee {}: {error}", args.join(" ")))
}

#[test]
fn doctor_robot_docs_json_route_is_agent_readable_response_envelope() -> TestResult {
    let output = run_ee(&["doctor", "--robot-docs", "--json"])?;
    ensure(
        output.status.success(),
        format!(
            "ee doctor --robot-docs --json must succeed; stdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    ensure(
        output.stderr.is_empty(),
        format!(
            "ee doctor --robot-docs --json must keep stderr empty; got {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;

    let value: Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("doctor robot-docs stdout must be JSON: {error}"))?;
    ensure(
        value["schema"].as_str() == Some("ee.response.v2"),
        format!("top-level schema must be ee.response.v2; got {value}"),
    )?;
    ensure(
        value["success"].as_bool() == Some(true),
        format!("robot-docs response must succeed; got {value}"),
    )?;

    let data = &value["data"];
    ensure(
        data["schema"].as_str() == Some("ee.doctor.robot_docs.v1"),
        format!("data schema must be ee.doctor.robot_docs.v1; got {data}"),
    )?;
    ensure(
        data["doctor_version"].as_str() == Some(env!("CARGO_PKG_VERSION")),
        format!("doctor_version must track the crate version; got {data}"),
    )?;
    ensure(
        data["doctor_contract_version"].as_str() == Some("1.0.0"),
        format!("doctor_contract_version must remain pinned; got {data}"),
    )?;

    let surfaces = data["surfaces"]
        .as_array()
        .ok_or_else(|| format!("data.surfaces must be an array; got {data}"))?;
    let surface_names = surfaces
        .iter()
        .map(|surface| {
            surface["name"]
                .as_str()
                .ok_or_else(|| format!("surface entry must include string name; got {surface}"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    ensure(
        surface_names
            == [
                "ee doctor",
                "ee doctor --full",
                "ee doctor --fix-plan",
                "ee doctor --franken-health",
                "ee doctor --capabilities",
                "ee doctor --robot-docs",
            ],
        format!("doctor robot-docs surfaces drifted; got {surface_names:?}"),
    )?;

    for (index, surface) in surfaces.iter().enumerate() {
        ensure(
            surface["kind"]
                .as_str()
                .is_some_and(|kind| !kind.is_empty()),
            format!("surfaces[{index}].kind must be non-empty; got {surface}"),
        )?;
        ensure(
            surface["purpose"]
                .as_str()
                .is_some_and(|purpose| !purpose.is_empty()),
            format!("surfaces[{index}].purpose must be non-empty; got {surface}"),
        )?;
        ensure(
            surface["example"]
                .as_str()
                .is_some_and(|example| example.starts_with("ee doctor")),
            format!("surfaces[{index}].example must be a doctor invocation; got {surface}"),
        )?;
    }

    let full_surface = surfaces
        .iter()
        .find(|surface| surface["name"].as_str() == Some("ee doctor --full"))
        .ok_or_else(|| format!("robot-docs must include the --full surface; got {surfaces:?}"))?;
    ensure(
        full_surface["kind"].as_str() == Some("flag"),
        format!("--full surface must be documented as a flag; got {full_surface}"),
    )?;
    ensure(
        full_surface["example"].as_str() == Some("ee doctor --full --json"),
        format!("--full surface must pin the canonical JSON invocation; got {full_surface}"),
    )?;
    ensure(
        full_surface["purpose"]
            .as_str()
            .is_some_and(|purpose| purpose.contains("exhaustive")),
        format!(
            "--full surface purpose should describe exhaustive diagnostics; got {full_surface}"
        ),
    )?;

    let related_schemas = data["related_schemas"]
        .as_array()
        .ok_or_else(|| format!("data.related_schemas must be an array; got {data}"))?;
    for schema in [
        "ee.doctor.capabilities.v1",
        "ee.doctor.run_state.v1",
        "ee.doctor.action_line.v1",
    ] {
        ensure(
            related_schemas
                .iter()
                .any(|candidate| candidate.as_str() == Some(schema)),
            format!("related_schemas must include {schema}; got {related_schemas:?}"),
        )?;
    }

    Ok(())
}
