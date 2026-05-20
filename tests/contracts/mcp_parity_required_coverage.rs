//! bd-3usjw.28 — initial-coverage gate for the MCP parity test suite.
//!
//! The existing static coverage test at tests/mcp_parity_coverage.rs
//! is gated on `#[cfg(feature = "mcp")]` and proves that every tool
//! registered in src/mcp.rs has both a parity invocation AND an input
//! fixture. That's the right contract once `--features mcp` runs.
//!
//! The bead acceptance also names an INITIAL-COVERAGE floor:
//!
//!     "Initial coverage: at least context, search, why, status,
//!      doctor, remember have parity tests"
//!
//! This contract pins that floor as a feature-flag-independent gate:
//! the six required surfaces must each have a parity fixture directory
//! with at least one `*.json` input. A regression that deletes any of
//! those fixtures fails the suite in the default-feature build,
//! BEFORE `--features mcp` runs — which matters because the RCH
//! topology blocker (bd-17c65.10.17.1.3) means the mcp feature gate
//! has been unprovable for weeks, leaving the README's CLI/MCP parity
//! promise un-checked at the default-build layer.
//!
//! Asserts:
//!
//! 1. Each of the six bead-required surfaces has a fixture directory
//!    at `tests/mcp_parity/<surface>/inputs/`.
//! 2. Each fixture directory contains at least one `*.json` file.
//! 3. Each `*.json` file parses cleanly.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::PathBuf;

type TestResult = Result<(), String>;

/// Surfaces the bead acceptance explicitly names as the initial-coverage floor.
/// These six surfaces are the README CLI/MCP parity promise's load-bearing set.
/// Extending this list requires extending the bead acceptance.
const REQUIRED_SURFACES: &[&str] = &["context", "search", "why", "status", "doctor", "remember"];

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn fixture_dir(surface: &str) -> PathBuf {
    repo_root()
        .join("tests")
        .join("mcp_parity")
        .join(surface)
        .join("inputs")
}

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

#[test]
fn every_required_surface_has_a_fixture_directory() -> TestResult {
    for surface in REQUIRED_SURFACES {
        let dir = fixture_dir(surface);
        let metadata = fs::metadata(&dir).map_err(|error| {
            format!(
                "REQUIRED surface `{surface}` is missing its parity fixture directory at {}: {error}",
                dir.display()
            )
        })?;
        ensure(
            metadata.is_dir(),
            format!(
                "REQUIRED surface `{surface}` fixture path {} must be a directory",
                dir.display()
            ),
        )?;
    }
    Ok(())
}

#[test]
fn every_required_surface_has_at_least_one_json_fixture() -> TestResult {
    for surface in REQUIRED_SURFACES {
        let dir = fixture_dir(surface);
        let entries = fs::read_dir(&dir).map_err(|error| {
            format!("REQUIRED surface `{surface}` fixture dir unreadable: {error}")
        })?;
        let json_count = entries
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .path()
                    .extension()
                    .and_then(|ext| ext.to_str())
                    .map(|ext| ext.eq_ignore_ascii_case("json"))
                    .unwrap_or(false)
            })
            .count();
        ensure(
            json_count > 0,
            format!(
                "REQUIRED surface `{surface}` must have at least one *.json parity input fixture in {}",
                dir.display()
            ),
        )?;
    }
    Ok(())
}

#[test]
fn every_required_surface_fixture_parses_as_json() -> TestResult {
    for surface in REQUIRED_SURFACES {
        let dir = fixture_dir(surface);
        let entries = fs::read_dir(&dir).map_err(|error| {
            format!("REQUIRED surface `{surface}` fixture dir unreadable: {error}")
        })?;
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            if !path
                .extension()
                .and_then(|ext| ext.to_str())
                .map(|ext| ext.eq_ignore_ascii_case("json"))
                .unwrap_or(false)
            {
                continue;
            }
            let body = fs::read_to_string(&path).map_err(|error| {
                format!(
                    "REQUIRED surface `{surface}` fixture {} unreadable: {error}",
                    path.display()
                )
            })?;
            let _: serde_json::Value = serde_json::from_str(&body).map_err(|error| {
                format!(
                    "REQUIRED surface `{surface}` fixture {} does not parse as JSON: {error}",
                    path.display()
                )
            })?;
        }
    }
    Ok(())
}
