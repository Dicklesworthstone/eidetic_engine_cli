//! Local coding-agent installation detection (EE-090, EE-091).
//!
//! Thin wrapper around `franken_agent_detection` for detecting installed
//! coding agent CLIs via filesystem probes.
//!
//! # Fixtures (EE-091)
//!
//! The `tests/fixtures/agent_detect/` directory contains simulated agent
//! installations for deterministic testing. Use `fixture_overrides()` to
//! build root overrides pointing to these fixtures.

use std::path::{Path, PathBuf};

pub use franken_agent_detection::{
    AgentDetectError, AgentDetectOptions, AgentDetectRootOverride, InstalledAgentDetectionEntry,
    InstalledAgentDetectionReport, InstalledAgentDetectionSummary, detect_installed_agents,
};

/// Build root overrides for the standard agent detection fixtures.
///
/// These fixtures are in `tests/fixtures/agent_detect/` and simulate:
/// - `codex/sessions/` - Codex CLI
/// - `gemini/tmp/` - Gemini CLI
/// - `claude/projects/` - Claude Code
/// - `cursor/.cursor/` - Cursor IDE
#[must_use]
pub fn fixture_overrides(fixtures_root: &Path) -> Vec<AgentDetectRootOverride> {
    vec![
        AgentDetectRootOverride {
            slug: "codex".to_string(),
            root: fixtures_root.join("codex").join("sessions"),
        },
        AgentDetectRootOverride {
            slug: "gemini".to_string(),
            root: fixtures_root.join("gemini").join("tmp"),
        },
        AgentDetectRootOverride {
            slug: "claude".to_string(),
            root: fixtures_root.join("claude").join("projects"),
        },
        AgentDetectRootOverride {
            slug: "cursor".to_string(),
            root: fixtures_root.join("cursor").join(".cursor"),
        },
    ]
}

/// Returns the path to agent detection fixtures for testing.
#[must_use]
pub fn fixtures_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("agent_detect")
}

/// Detect agents using the standard fixtures for deterministic testing.
///
/// # Errors
/// Returns error if detection fails (should not happen with valid fixtures).
pub fn detect_fixture_agents() -> Result<InstalledAgentDetectionReport, AgentDetectError> {
    let fixtures = fixtures_path();
    detect_installed_agents(&AgentDetectOptions {
        only_connectors: Some(vec![
            "codex".to_string(),
            "gemini".to_string(),
            "claude".to_string(),
            "cursor".to_string(),
        ]),
        include_undetected: false,
        root_overrides: fixture_overrides(&fixtures),
    })
}

#[cfg(test)]
mod tests {
    use crate::testing::{TestResult, ensure, ensure_equal};

    use super::*;

    #[test]
    fn detect_returns_report_with_version_and_summary() -> TestResult {
        let report = detect_installed_agents(&AgentDetectOptions::default())
            .map_err(|e| format!("detect failed: {e}"))?;

        ensure_equal(&report.format_version, &1, "format version")?;
        ensure(!report.generated_at.is_empty(), "generated_at not empty")?;
        ensure(
            report.summary.total_count >= report.summary.detected_count,
            "total >= detected",
        )
    }

    #[test]
    fn unknown_connector_is_rejected() -> TestResult {
        let result = detect_installed_agents(&AgentDetectOptions {
            only_connectors: Some(vec!["not-a-real-agent".to_string()]),
            include_undetected: true,
            root_overrides: vec![],
        });

        let Err(err) = result else {
            return Err("should reject unknown connector".to_string());
        };
        ensure(
            matches!(err, AgentDetectError::UnknownConnectors { .. }),
            "should be UnknownConnectors error",
        )
    }

    #[test]
    fn root_override_enables_fixture_detection() -> TestResult {
        let tmp = tempfile::tempdir().map_err(|e| format!("tempdir: {e}"))?;
        let fixture_root = tmp.path().join("codex-fixture").join("sessions");
        std::fs::create_dir_all(&fixture_root).map_err(|e| format!("mkdir: {e}"))?;

        let report = detect_installed_agents(&AgentDetectOptions {
            only_connectors: Some(vec!["codex".to_string()]),
            include_undetected: true,
            root_overrides: vec![AgentDetectRootOverride {
                slug: "codex".to_string(),
                root: fixture_root.clone(),
            }],
        })
        .map_err(|e| format!("detect failed: {e}"))?;

        ensure_equal(&report.summary.detected_count, &1, "detected count")?;
        ensure_equal(&report.summary.total_count, &1, "total count")?;

        let entry = report.installed_agents.first().ok_or("no entries")?;
        ensure_equal(&entry.slug.as_str(), &"codex", "slug")?;
        ensure(entry.detected, "should be detected")?;
        ensure(
            entry.root_paths.iter().any(|p| p.ends_with("/sessions")),
            "root path should end with /sessions",
        )
    }

    #[test]
    fn include_undetected_false_filters_output() -> TestResult {
        let tmp = tempfile::tempdir().map_err(|e| format!("tempdir: {e}"))?;

        let report = detect_installed_agents(&AgentDetectOptions {
            only_connectors: Some(vec!["codex".to_string(), "gemini".to_string()]),
            include_undetected: false,
            root_overrides: vec![AgentDetectRootOverride {
                slug: "codex".to_string(),
                root: tmp.path().join("nonexistent"),
            }],
        })
        .map_err(|e| format!("detect failed: {e}"))?;

        ensure(
            report.installed_agents.iter().all(|entry| entry.detected),
            "only detected agents should be in output",
        )
    }

    #[test]
    fn fixture_overrides_creates_four_entries() -> TestResult {
        let tmp = tempfile::tempdir().map_err(|e| format!("tempdir: {e}"))?;
        let overrides = fixture_overrides(tmp.path());

        ensure_equal(&overrides.len(), &4, "four fixture overrides")?;

        let slugs: Vec<&str> = overrides.iter().map(|o| o.slug.as_str()).collect();
        ensure(slugs.contains(&"codex"), "has codex")?;
        ensure(slugs.contains(&"gemini"), "has gemini")?;
        ensure(slugs.contains(&"claude"), "has claude")?;
        ensure(slugs.contains(&"cursor"), "has cursor")
    }

    #[test]
    fn detect_fixture_agents_returns_all_four() -> TestResult {
        let report = detect_fixture_agents().map_err(|e| format!("detect failed: {e}"))?;

        ensure_equal(&report.format_version, &1, "format version")?;
        ensure_equal(&report.summary.detected_count, &4, "detected count")?;
        ensure_equal(&report.summary.total_count, &4, "total count")?;

        let slugs: Vec<&str> = report
            .installed_agents
            .iter()
            .map(|e| e.slug.as_str())
            .collect();
        ensure(slugs.contains(&"codex"), "has codex")?;
        ensure(slugs.contains(&"gemini"), "has gemini")?;
        ensure(slugs.contains(&"claude"), "has claude")?;
        ensure(slugs.contains(&"cursor"), "has cursor")
    }

    #[test]
    fn fixture_detection_is_deterministic() -> TestResult {
        let report1 = detect_fixture_agents().map_err(|e| format!("detect1 failed: {e}"))?;
        let report2 = detect_fixture_agents().map_err(|e| format!("detect2 failed: {e}"))?;

        ensure_equal(
            &report1.format_version,
            &report2.format_version,
            "format version",
        )?;
        ensure_equal(
            &report1.summary.detected_count,
            &report2.summary.detected_count,
            "detected count",
        )?;
        ensure_equal(
            &report1.summary.total_count,
            &report2.summary.total_count,
            "total count",
        )?;

        for (e1, e2) in report1
            .installed_agents
            .iter()
            .zip(report2.installed_agents.iter())
        {
            ensure_equal(&e1.slug, &e2.slug, "slug")?;
            ensure_equal(&e1.detected, &e2.detected, "detected")?;
            ensure_equal(&e1.root_paths, &e2.root_paths, "root_paths")?;
        }
        Ok(())
    }
}
