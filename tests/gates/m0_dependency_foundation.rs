//! M0 Gate: Dependency Foundation (Gate 7 requirement)
//!
//! Validates that the core binary has the correct dependency foundation:
//! - No forbidden dependencies (tokio, rusqlite, petgraph, sqlx, diesel)
//! - Asupersync runtime present
//! - SQLModel/FrankenSQLite bridge present
//! - Frankensearch local profile available
//! - Agent-native envelope support present

use std::process::Command;

type TestResult = Result<(), String>;

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

#[test]
fn m0_no_forbidden_dependencies() -> TestResult {
    let output = Command::new("cargo")
        .args(["tree", "-e", "features"])
        .output()
        .map_err(|e| format!("failed to run cargo tree: {e}"))?;

    let stdout = String::from_utf8_lossy(&output.stdout);

    let forbidden = ["tokio", "rusqlite", "petgraph", "sqlx", "diesel", "sea-orm"];
    let mut found = Vec::new();

    for dep in forbidden {
        if stdout.contains(dep) {
            found.push(dep);
        }
    }

    ensure(
        found.is_empty(),
        format!("Forbidden dependencies found: {}", found.join(", ")),
    )
}

#[test]
fn m0_asupersync_runtime_present() -> TestResult {
    let output = Command::new("cargo")
        .args(["tree", "-p", "asupersync"])
        .output()
        .map_err(|e| format!("failed to run cargo tree: {e}"))?;

    ensure(
        output.status.success(),
        "Asupersync runtime must be in dependency tree",
    )
}

#[test]
fn m0_frankensqlite_bridge_present() -> TestResult {
    let output = Command::new("cargo")
        .args(["tree", "-p", "fsqlite"])
        .output()
        .map_err(|e| format!("failed to run cargo tree: {e}"))?;

    ensure(
        output.status.success(),
        "FrankenSQLite (fsqlite) must be in dependency tree",
    )
}

#[test]
fn m0_sqlmodel_bridge_present() -> TestResult {
    let output = Command::new("cargo")
        .args(["tree", "-p", "sqlmodel-frankensqlite"])
        .output()
        .map_err(|e| format!("failed to run cargo tree: {e}"))?;

    ensure(
        output.status.success(),
        "SQLModel FrankenSQLite bridge must be in dependency tree",
    )
}

#[test]
fn m0_frankensearch_present() -> TestResult {
    let output = Command::new("cargo")
        .args(["tree", "-p", "frankensearch"])
        .output()
        .map_err(|e| format!("failed to run cargo tree: {e}"))?;

    ensure(
        output.status.success(),
        "Frankensearch must be in dependency tree",
    )
}

#[test]
fn m0_agent_native_envelope_in_source() -> TestResult {
    let output = Command::new("grep")
        .args(["-r", "ee.response.v1", "src/"])
        .output()
        .map_err(|e| format!("failed to run grep: {e}"))?;

    ensure(
        output.status.success() && !output.stdout.is_empty(),
        "Agent-native envelope (ee.response.v1) must be defined in source",
    )
}
