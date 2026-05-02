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
    InstalledAgentDetectionReport, InstalledAgentDetectionSummary, default_probe_paths_tilde,
    detect_installed_agents,
};

/// Stable schema identifier for `ee agent status --json`.
pub const AGENT_STATUS_SCHEMA_V1: &str = "ee.agent.status.v1";
/// Stable schema identifier for `ee agent sources --json`.
pub const AGENT_SOURCES_SCHEMA_V1: &str = "ee.agent.sources.v1";

/// High-level status of the local agent inventory.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AgentInventoryStatus {
    /// Local detection ran and found at least one agent source.
    Ready,
    /// Local detection ran but did not find any agent source.
    Empty,
    /// Detection is available but intentionally deferred by a broader status report.
    NotInspected,
    /// Detection could not run.
    Unavailable,
}

impl AgentInventoryStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Empty => "empty",
            Self::NotInspected => "not_inspected",
            Self::Unavailable => "unavailable",
        }
    }
}

/// Degraded-state detail for agent inventory.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentInventoryDegradation {
    pub code: String,
    pub severity: &'static str,
    pub message: String,
    pub repair: &'static str,
}

/// Stable, CLI-facing local agent inventory report.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentInventoryReport {
    pub schema: &'static str,
    pub status: AgentInventoryStatus,
    pub format_version: u32,
    pub summary: InstalledAgentDetectionSummary,
    pub installed_agents: Vec<InstalledAgentDetectionEntry>,
    pub degraded: Vec<AgentInventoryDegradation>,
    pub inspection_command: &'static str,
}

/// Options for `ee agent sources`.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AgentSourcesOptions {
    pub only: Option<String>,
    pub include_paths: bool,
    pub include_origin_fixtures: bool,
    pub fixtures_root: Option<PathBuf>,
}

/// Stable catalog report for known agent sources and optional origin fixtures.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSourcesReport {
    pub schema: &'static str,
    pub command: &'static str,
    pub version: &'static str,
    pub total_count: usize,
    pub include_paths: bool,
    pub sources: Vec<AgentSourceCatalogEntry>,
    pub origin_fixtures: Vec<AgentSourceOriginFixture>,
    pub path_rewrites: Vec<AgentPathRewrite>,
}

/// A known coding-agent connector and its optional probe paths.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSourceCatalogEntry {
    pub slug: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub probe_paths: Vec<String>,
}

/// Deterministic fixture describing where remote or mirrored agent roots live.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSourceOriginFixture {
    pub origin_id: String,
    pub kind: String,
    pub host: String,
    pub remote_root: String,
    pub local_root: String,
    pub connector_slugs: Vec<String>,
    pub description: String,
}

/// A deterministic path-prefix rewrite for mirrored or remote agent sources.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentPathRewrite {
    pub origin_id: String,
    pub connector_slug: String,
    pub from: String,
    pub to: String,
}

impl AgentPathRewrite {
    /// Apply this rewrite rule to a source path when the prefix matches on a
    /// path boundary.
    #[must_use]
    pub fn apply(&self, path: &str) -> Option<String> {
        if path == self.from {
            return Some(self.to.clone());
        }

        let rest = path.strip_prefix(&self.from)?;
        let from_ends_with_sep = self.from.ends_with('/') || self.from.ends_with('\\');
        let rest_without_leading_sep = rest.strip_prefix('/').or_else(|| rest.strip_prefix('\\'));
        let rest_starts_with_sep = rest_without_leading_sep.is_some();
        if !from_ends_with_sep && !rest_starts_with_sep {
            return None;
        }

        let to_ends_with_sep = self.to.ends_with('/') || self.to.ends_with('\\');
        let rewritten = match (to_ends_with_sep, rest_without_leading_sep) {
            (true, Some(rest_without_sep)) => format!("{}{}", self.to, rest_without_sep),
            (true, None) | (false, Some(_)) => format!("{}{}", self.to, rest),
            (false, None) => {
                let sep = if self.from.ends_with('\\') { '\\' } else { '/' };
                format!("{}{}{}", self.to, sep, rest)
            }
        };
        Some(rewritten)
    }
}

impl AgentInventoryReport {
    #[must_use]
    pub fn from_detection(report: InstalledAgentDetectionReport) -> Self {
        let status = if report.summary.detected_count == 0 {
            AgentInventoryStatus::Empty
        } else {
            AgentInventoryStatus::Ready
        };

        Self {
            schema: AGENT_STATUS_SCHEMA_V1,
            status,
            format_version: report.format_version,
            summary: report.summary,
            installed_agents: report.installed_agents,
            degraded: Vec::new(),
            inspection_command: "ee agent status --json",
        }
    }

    #[must_use]
    pub fn not_inspected() -> Self {
        Self {
            schema: AGENT_STATUS_SCHEMA_V1,
            status: AgentInventoryStatus::NotInspected,
            format_version: 1,
            summary: InstalledAgentDetectionSummary {
                detected_count: 0,
                total_count: default_probe_paths_tilde().len(),
            },
            installed_agents: Vec::new(),
            degraded: Vec::new(),
            inspection_command: "ee agent status --json",
        }
    }

    #[must_use]
    pub fn unavailable(error: &AgentDetectError) -> Self {
        Self {
            schema: AGENT_STATUS_SCHEMA_V1,
            status: AgentInventoryStatus::Unavailable,
            format_version: 1,
            summary: InstalledAgentDetectionSummary {
                detected_count: 0,
                total_count: default_probe_paths_tilde().len(),
            },
            installed_agents: Vec::new(),
            degraded: vec![AgentInventoryDegradation {
                code: "agent_detection_unavailable".to_string(),
                severity: "medium",
                message: error.to_string(),
                repair: "ee agent sources --json",
            }],
            inspection_command: "ee agent status --json",
        }
    }
}

/// Options for `ee agent status`.
#[derive(Clone, Debug, Default)]
pub struct AgentStatusOptions {
    pub only_connectors: Option<Vec<String>>,
    pub include_undetected: bool,
    pub root_overrides: Vec<AgentDetectRootOverride>,
}

impl AgentStatusOptions {
    #[must_use]
    fn detection_options(&self) -> AgentDetectOptions {
        AgentDetectOptions {
            only_connectors: self.only_connectors.clone(),
            include_undetected: self.include_undetected,
            root_overrides: self.root_overrides.clone(),
        }
    }
}

/// Gather the local coding-agent inventory.
///
/// # Errors
/// Returns an agent detection error when an invalid connector filter is used or
/// the detector is unavailable.
pub fn gather_agent_status(
    options: &AgentStatusOptions,
) -> Result<AgentInventoryReport, AgentDetectError> {
    detect_installed_agents(&options.detection_options()).map(AgentInventoryReport::from_detection)
}

/// Build a stable catalog of known agent connectors and optional origin
/// fixtures for remote or mirrored source roots.
#[must_use]
pub fn build_agent_sources_report(options: &AgentSourcesOptions) -> AgentSourcesReport {
    let only = options.only.as_deref().map(normalize_connector_slug);
    let sources = default_probe_paths_tilde()
        .into_iter()
        .filter(|(slug, _)| only.as_deref().is_none_or(|wanted| *slug == wanted))
        .map(|(slug, paths)| AgentSourceCatalogEntry {
            slug: slug.to_string(),
            probe_paths: if options.include_paths {
                paths
            } else {
                Vec::new()
            },
        })
        .collect::<Vec<_>>();

    let fixtures_root = options.fixtures_root.clone().unwrap_or_else(fixtures_path);
    let mut origin_fixtures = if options.include_origin_fixtures {
        remote_mirror_origin_fixtures(&fixtures_root)
    } else {
        Vec::new()
    };
    let path_rewrites = if options.include_origin_fixtures {
        remote_mirror_path_rewrites(&fixtures_root)
            .into_iter()
            .filter(|rewrite| {
                only.as_deref().is_none_or(|wanted| {
                    normalize_connector_slug(&rewrite.connector_slug) == wanted
                })
            })
            .collect()
    } else {
        Vec::new()
    };

    if let Some(wanted) = only {
        for fixture in &mut origin_fixtures {
            fixture
                .connector_slugs
                .retain(|slug| normalize_connector_slug(slug) == wanted);
        }
        origin_fixtures.retain(|fixture| !fixture.connector_slugs.is_empty());
    }

    AgentSourcesReport {
        schema: AGENT_SOURCES_SCHEMA_V1,
        command: "agent sources",
        version: env!("CARGO_PKG_VERSION"),
        total_count: sources.len(),
        include_paths: options.include_paths,
        sources,
        origin_fixtures,
        path_rewrites,
    }
}

/// Normalize public connector names and common CLI aliases to catalog slugs.
#[must_use]
pub fn normalize_connector_slug(slug: &str) -> String {
    let normalized = slug.trim().to_ascii_lowercase().replace('-', "_");
    match normalized.as_str() {
        "claude_code" => "claude".to_string(),
        "codex_cli" => "codex".to_string(),
        "gemini_cli" => "gemini".to_string(),
        "copilot_cli" => "copilot_cli".to_string(),
        _ => normalized,
    }
}

/// Deterministic remote-home mirror fixtures used to prove path rewrite and
/// origin reporting without enabling connector-backed readers by default.
#[must_use]
pub fn remote_mirror_origin_fixtures(fixtures_root: &Path) -> Vec<AgentSourceOriginFixture> {
    vec![AgentSourceOriginFixture {
        origin_id: "fixture-ssh-csd".to_string(),
        kind: "ssh".to_string(),
        host: "csd".to_string(),
        remote_root: "/home/agent".to_string(),
        local_root: remote_mirror_home(fixtures_root).display().to_string(),
        connector_slugs: vec!["claude".to_string(), "codex".to_string()],
        description:
            "Deterministic remote-home mirror for SSH or rsync-collected agent source tests."
                .to_string(),
    }]
}

/// Path rewrite rules for deterministic remote-home mirror fixtures.
#[must_use]
pub fn remote_mirror_path_rewrites(fixtures_root: &Path) -> Vec<AgentPathRewrite> {
    let local_home = remote_mirror_home(fixtures_root);
    vec![
        AgentPathRewrite {
            origin_id: "fixture-ssh-csd".to_string(),
            connector_slug: "claude".to_string(),
            from: "/home/agent/.claude/projects".to_string(),
            to: local_home
                .join(".claude")
                .join("projects")
                .display()
                .to_string(),
        },
        AgentPathRewrite {
            origin_id: "fixture-ssh-csd".to_string(),
            connector_slug: "codex".to_string(),
            from: "/home/agent/.codex/sessions".to_string(),
            to: local_home
                .join(".codex")
                .join("sessions")
                .display()
                .to_string(),
        },
    ]
}

/// Detection root overrides derived from the remote-home mirror fixtures.
#[must_use]
pub fn remote_mirror_fixture_overrides(fixtures_root: &Path) -> Vec<AgentDetectRootOverride> {
    remote_mirror_path_rewrites(fixtures_root)
        .into_iter()
        .map(|rewrite| AgentDetectRootOverride {
            slug: rewrite.connector_slug,
            root: PathBuf::from(rewrite.to),
        })
        .collect()
}

/// Rewrite a path for a connector using deterministic longest-prefix matching.
#[must_use]
pub fn rewrite_agent_source_path(
    rewrites: &[AgentPathRewrite],
    connector_slug: &str,
    source_path: &str,
) -> Option<String> {
    let connector = normalize_connector_slug(connector_slug);
    rewrites
        .iter()
        .filter(|rewrite| normalize_connector_slug(&rewrite.connector_slug) == connector)
        .filter_map(|rewrite| {
            rewrite
                .apply(source_path)
                .map(|rewritten| (rewrite.from.len(), rewrite.origin_id.as_str(), rewritten))
        })
        .max_by(|left, right| left.0.cmp(&right.0).then_with(|| right.1.cmp(left.1)))
        .map(|(_, _, rewritten)| rewritten)
}

fn remote_mirror_home(fixtures_root: &Path) -> PathBuf {
    fixtures_root
        .join("remote_mirror")
        .join("ssh-csd")
        .join("home")
        .join("agent")
}

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
    fn source_report_filters_origin_fixtures_by_connector() -> TestResult {
        let report = build_agent_sources_report(&AgentSourcesOptions {
            only: Some("codex-cli".to_string()),
            include_paths: true,
            include_origin_fixtures: true,
            fixtures_root: Some(fixtures_path()),
        });

        ensure_equal(&report.schema, &AGENT_SOURCES_SCHEMA_V1, "schema")?;
        ensure_equal(&report.total_count, &1, "source count")?;
        let source = report
            .sources
            .first()
            .ok_or_else(|| "expected one filtered source".to_string())?;
        ensure_equal(&source.slug.as_str(), &"codex", "source slug")?;
        ensure_equal(&report.origin_fixtures.len(), &1, "origin fixture count")?;
        let origin_fixture = report
            .origin_fixtures
            .first()
            .ok_or_else(|| "expected one origin fixture".to_string())?;
        ensure_equal(
            &origin_fixture.connector_slugs,
            &vec!["codex".to_string()],
            "filtered connector slugs",
        )?;
        ensure_equal(&report.path_rewrites.len(), &1, "rewrite count")?;
        let path_rewrite = report
            .path_rewrites
            .first()
            .ok_or_else(|| "expected one path rewrite".to_string())?;
        ensure_equal(
            &path_rewrite.from.as_str(),
            &"/home/agent/.codex/sessions",
            "rewrite source",
        )
    }

    #[test]
    fn remote_mirror_fixture_overrides_detect_agents() -> TestResult {
        let report = detect_installed_agents(&AgentDetectOptions {
            only_connectors: Some(vec!["codex".to_string(), "claude".to_string()]),
            include_undetected: false,
            root_overrides: remote_mirror_fixture_overrides(&fixtures_path()),
        })
        .map_err(|e| format!("detect failed: {e}"))?;

        ensure_equal(&report.summary.detected_count, &2, "detected count")?;
        ensure_equal(&report.summary.total_count, &2, "total count")?;
        let slugs: Vec<&str> = report
            .installed_agents
            .iter()
            .map(|entry| entry.slug.as_str())
            .collect();
        ensure(slugs.contains(&"claude"), "has claude")?;
        ensure(slugs.contains(&"codex"), "has codex")
    }

    #[test]
    fn source_path_rewrite_respects_connector_and_path_boundary() -> TestResult {
        let rewrites = remote_mirror_path_rewrites(&fixtures_path());
        let rewritten = rewrite_agent_source_path(
            &rewrites,
            "codex-cli",
            "/home/agent/.codex/sessions/2026/session.jsonl",
        )
        .ok_or_else(|| "codex path should rewrite".to_string())?;

        ensure(
            rewritten
                .ends_with("/remote_mirror/ssh-csd/home/agent/.codex/sessions/2026/session.jsonl"),
            format!("unexpected rewritten path: {rewritten}"),
        )?;
        ensure(
            rewrite_agent_source_path(&rewrites, "claude", "/home/agent/.codex/sessions/2026")
                .is_none(),
            "claude rewrite must not apply to codex path",
        )?;
        ensure(
            rewrite_agent_source_path(&rewrites, "codex", "/home/agent/.codex/sessions-old")
                .is_none(),
            "rewrite must not match partial path component",
        )
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
    fn agent_status_report_classifies_fixture_inventory_as_ready() -> TestResult {
        let report = gather_agent_status(&AgentStatusOptions {
            only_connectors: Some(vec![
                "codex".to_string(),
                "gemini".to_string(),
                "claude".to_string(),
                "cursor".to_string(),
            ]),
            include_undetected: false,
            root_overrides: fixture_overrides(&fixtures_path()),
        })
        .map_err(|e| format!("agent status failed: {e}"))?;

        ensure_equal(&report.schema, &AGENT_STATUS_SCHEMA_V1, "schema")?;
        ensure_equal(
            &report.status,
            &AgentInventoryStatus::Ready,
            "inventory status",
        )?;
        ensure_equal(&report.summary.detected_count, &4, "detected count")?;
        ensure_equal(&report.installed_agents.len(), &4, "agent count")
    }

    #[test]
    fn not_inspected_inventory_preserves_known_connector_count() -> TestResult {
        let report = AgentInventoryReport::not_inspected();

        ensure_equal(
            &report.status,
            &AgentInventoryStatus::NotInspected,
            "not inspected status",
        )?;
        ensure(
            report.summary.total_count >= 4,
            "known connector count should include common agents",
        )?;
        ensure(
            report.installed_agents.is_empty(),
            "deferred status should not expose machine-specific paths",
        )
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
