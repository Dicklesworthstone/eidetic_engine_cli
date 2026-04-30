//! CI/local verification orchestration and artifact policy (EE-TST-007).
//!
//! This module defines the verification pipeline that runs both locally and in CI,
//! ensuring consistent quality gates across development environments.
//!
//! # Verification Steps
//!
//! The verification pipeline runs these steps in order:
//! 1. `cargo fmt --check` - formatting consistency
//! 2. `cargo clippy --all-targets -- -D warnings` - lint checks
//! 3. `cargo test` - unit and integration tests
//! 4. Forbidden dependency audit - no tokio, rusqlite, petgraph, etc.
//!
//! # Artifact Policy
//!
//! Defines what gets generated, cached, and excluded from version control:
//! - `target/` - build artifacts (gitignored, cached in CI)
//! - `.ee/` - workspace state (user-specific, gitignored)
//! - `tests/fixtures/` - test fixtures (versioned)
//! - Golden test outputs regenerated on demand, versioned

use std::fmt;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use super::build_info;

// ============================================================================
// Schema Constants
// ============================================================================

/// Schema for verification reports.
pub const VERIFY_REPORT_SCHEMA_V1: &str = "ee.verify.report.v1";

/// Schema for artifact policy.
pub const ARTIFACT_POLICY_SCHEMA_V1: &str = "ee.artifact_policy.v1";

// ============================================================================
// Verification Steps
// ============================================================================

/// A verification step in the pipeline.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VerifyStep {
    Format,
    Clippy,
    Test,
    ForbiddenDeps,
}

impl VerifyStep {
    /// All verification steps in execution order.
    pub const ALL: &'static [Self] = &[Self::Format, Self::Clippy, Self::Test, Self::ForbiddenDeps];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Format => "format",
            Self::Clippy => "clippy",
            Self::Test => "test",
            Self::ForbiddenDeps => "forbidden_deps",
        }
    }

    #[must_use]
    pub const fn description(self) -> &'static str {
        match self {
            Self::Format => "Check code formatting with cargo fmt",
            Self::Clippy => "Run clippy lints with warnings as errors",
            Self::Test => "Run unit and integration tests",
            Self::ForbiddenDeps => "Audit for forbidden dependencies",
        }
    }

    #[must_use]
    pub const fn command(self) -> &'static str {
        match self {
            Self::Format => "cargo fmt --check",
            Self::Clippy => "cargo clippy --all-targets -- -D warnings",
            Self::Test => "cargo test",
            Self::ForbiddenDeps => "cargo test forbidden_deps",
        }
    }

    #[must_use]
    pub const fn is_required(self) -> bool {
        true
    }
}

impl fmt::Display for VerifyStep {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

// ============================================================================
// Step Result
// ============================================================================

/// Result of running a verification step.
#[derive(Clone, Debug)]
pub struct StepResult {
    pub step: VerifyStep,
    pub passed: bool,
    pub duration_ms: u64,
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i32>,
    pub skipped: bool,
    pub skip_reason: Option<String>,
}

impl StepResult {
    fn passed(step: VerifyStep, duration: Duration, stdout: String, stderr: String) -> Self {
        Self {
            step,
            passed: true,
            duration_ms: duration.as_millis() as u64,
            stdout,
            stderr,
            exit_code: Some(0),
            skipped: false,
            skip_reason: None,
        }
    }

    fn failed(
        step: VerifyStep,
        duration: Duration,
        stdout: String,
        stderr: String,
        exit_code: Option<i32>,
    ) -> Self {
        Self {
            step,
            passed: false,
            duration_ms: duration.as_millis() as u64,
            stdout,
            stderr,
            exit_code,
            skipped: false,
            skip_reason: None,
        }
    }

    fn skipped(step: VerifyStep, reason: &str) -> Self {
        Self {
            step,
            passed: true,
            duration_ms: 0,
            stdout: String::new(),
            stderr: String::new(),
            exit_code: None,
            skipped: true,
            skip_reason: Some(reason.to_string()),
        }
    }
}

// ============================================================================
// Verification Report
// ============================================================================

/// Complete verification report.
#[derive(Clone, Debug)]
pub struct VerifyReport {
    pub version: &'static str,
    pub workspace_path: String,
    pub all_passed: bool,
    pub total_duration_ms: u64,
    pub steps: Vec<StepResult>,
    pub failed_count: usize,
    pub passed_count: usize,
    pub skipped_count: usize,
}

impl VerifyReport {
    #[must_use]
    pub fn human_summary(&self) -> String {
        let mut out = String::with_capacity(1024);
        out.push_str("Verification Report\n");
        out.push_str("===================\n\n");
        out.push_str(&format!("Workspace: {}\n", self.workspace_path));
        out.push_str(&format!("Duration: {}ms\n\n", self.total_duration_ms));

        for result in &self.steps {
            let status = if result.skipped {
                "SKIP"
            } else if result.passed {
                "PASS"
            } else {
                "FAIL"
            };
            out.push_str(&format!(
                "[{}] {} ({}ms)\n",
                status,
                result.step.as_str(),
                result.duration_ms
            ));
            if !result.passed && !result.stderr.is_empty() {
                let preview: String = result.stderr.lines().take(5).collect::<Vec<_>>().join("\n");
                out.push_str(&format!("    {}\n", preview.replace('\n', "\n    ")));
            }
        }

        out.push_str(&format!(
            "\nSummary: {} passed, {} failed, {} skipped\n",
            self.passed_count, self.failed_count, self.skipped_count
        ));

        if self.all_passed {
            out.push_str("Result: PASSED\n");
        } else {
            out.push_str("Result: FAILED\n");
        }

        out
    }

    #[must_use]
    pub fn toon_output(&self) -> String {
        let status = if self.all_passed { "PASS" } else { "FAIL" };
        format!(
            "VERIFY|{}|{}|{}|{}ms",
            status, self.passed_count, self.failed_count, self.total_duration_ms
        )
    }

    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        let steps: Vec<serde_json::Value> = self
            .steps
            .iter()
            .map(|s| {
                serde_json::json!({
                    "step": s.step.as_str(),
                    "passed": s.passed,
                    "durationMs": s.duration_ms,
                    "exitCode": s.exit_code,
                    "skipped": s.skipped,
                    "skipReason": s.skip_reason,
                })
            })
            .collect();

        serde_json::json!({
            "command": "verify",
            "version": self.version,
            "workspacePath": self.workspace_path,
            "allPassed": self.all_passed,
            "totalDurationMs": self.total_duration_ms,
            "passedCount": self.passed_count,
            "failedCount": self.failed_count,
            "skippedCount": self.skipped_count,
            "steps": steps,
        })
    }
}

// ============================================================================
// Artifact Policy
// ============================================================================

/// Artifact category for policy rules.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ArtifactCategory {
    BuildOutput,
    TestFixture,
    WorkspaceState,
    GoldenOutput,
    CacheDirectory,
    GeneratedCode,
}

impl ArtifactCategory {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::BuildOutput => "build_output",
            Self::TestFixture => "test_fixture",
            Self::WorkspaceState => "workspace_state",
            Self::GoldenOutput => "golden_output",
            Self::CacheDirectory => "cache_directory",
            Self::GeneratedCode => "generated_code",
        }
    }
}

/// Policy rule for an artifact pattern.
#[derive(Clone, Debug)]
pub struct ArtifactRule {
    pub pattern: &'static str,
    pub category: ArtifactCategory,
    pub versioned: bool,
    pub ci_cached: bool,
    pub description: &'static str,
}

/// Standard artifact policy for ee workspaces.
pub const ARTIFACT_RULES: &[ArtifactRule] = &[
    ArtifactRule {
        pattern: "target/",
        category: ArtifactCategory::BuildOutput,
        versioned: false,
        ci_cached: true,
        description: "Cargo build artifacts",
    },
    ArtifactRule {
        pattern: ".ee/",
        category: ArtifactCategory::WorkspaceState,
        versioned: false,
        ci_cached: false,
        description: "User workspace state (database, indexes)",
    },
    ArtifactRule {
        pattern: "tests/fixtures/",
        category: ArtifactCategory::TestFixture,
        versioned: true,
        ci_cached: false,
        description: "Deterministic test fixtures",
    },
    ArtifactRule {
        pattern: "tests/fixtures/golden/",
        category: ArtifactCategory::GoldenOutput,
        versioned: true,
        ci_cached: false,
        description: "Golden test expected outputs",
    },
    ArtifactRule {
        pattern: "Cargo.lock",
        category: ArtifactCategory::GeneratedCode,
        versioned: true,
        ci_cached: false,
        description: "Locked dependency versions",
    },
    ArtifactRule {
        pattern: ".rch-target/",
        category: ArtifactCategory::CacheDirectory,
        versioned: false,
        ci_cached: false,
        description: "Remote compilation helper cache",
    },
];

/// Get artifact policy report.
#[must_use]
pub fn artifact_policy_report() -> ArtifactPolicyReport {
    ArtifactPolicyReport {
        version: build_info().version,
        rules: ARTIFACT_RULES.to_vec(),
    }
}

/// Artifact policy report.
#[derive(Clone, Debug)]
pub struct ArtifactPolicyReport {
    pub version: &'static str,
    pub rules: Vec<ArtifactRule>,
}

impl ArtifactPolicyReport {
    #[must_use]
    pub fn human_summary(&self) -> String {
        let mut out = String::with_capacity(512);
        out.push_str("Artifact Policy\n");
        out.push_str("===============\n\n");

        for rule in &self.rules {
            let versioned = if rule.versioned {
                "versioned"
            } else {
                "gitignored"
            };
            let cached = if rule.ci_cached { ", CI cached" } else { "" };
            out.push_str(&format!(
                "{} ({}{}) - {}\n",
                rule.pattern, versioned, cached, rule.description
            ));
        }

        out
    }

    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        let rules: Vec<serde_json::Value> = self
            .rules
            .iter()
            .map(|r| {
                serde_json::json!({
                    "pattern": r.pattern,
                    "category": r.category.as_str(),
                    "versioned": r.versioned,
                    "ciCached": r.ci_cached,
                    "description": r.description,
                })
            })
            .collect();

        serde_json::json!({
            "command": "artifact-policy",
            "version": self.version,
            "rules": rules,
        })
    }
}

// ============================================================================
// Verification Options
// ============================================================================

/// Options for running verification.
#[derive(Clone, Debug, Default)]
pub struct VerifyOptions {
    pub workspace_path: Option<String>,
    pub steps: Option<Vec<VerifyStep>>,
    pub fail_fast: bool,
    pub dry_run: bool,
}

// ============================================================================
// Verification Runner
// ============================================================================

/// Run the verification pipeline.
#[must_use]
pub fn run_verification(options: &VerifyOptions) -> VerifyReport {
    let version = build_info().version;
    let workspace_path = options
        .workspace_path
        .clone()
        .unwrap_or_else(|| ".".to_string());

    let steps_to_run = options
        .steps
        .clone()
        .unwrap_or_else(|| VerifyStep::ALL.to_vec());

    let start = Instant::now();
    let mut results: Vec<StepResult> = Vec::new();
    let mut had_failure = false;

    for step in &steps_to_run {
        if options.fail_fast && had_failure {
            results.push(StepResult::skipped(*step, "fail-fast after prior failure"));
            continue;
        }

        if options.dry_run {
            results.push(StepResult::skipped(*step, "dry-run mode"));
            continue;
        }

        let result = run_step(*step, &workspace_path);
        if !result.passed {
            had_failure = true;
        }
        results.push(result);
    }

    let total_duration = start.elapsed();

    let passed_count = results.iter().filter(|r| r.passed && !r.skipped).count();
    let failed_count = results.iter().filter(|r| !r.passed).count();
    let skipped_count = results.iter().filter(|r| r.skipped).count();

    VerifyReport {
        version,
        workspace_path,
        all_passed: failed_count == 0,
        total_duration_ms: total_duration.as_millis() as u64,
        steps: results,
        failed_count,
        passed_count,
        skipped_count,
    }
}

fn run_step(step: VerifyStep, workspace_path: &str) -> StepResult {
    let start = Instant::now();

    let (program, args) = match step {
        VerifyStep::Format => ("cargo", vec!["fmt", "--check"]),
        VerifyStep::Clippy => (
            "cargo",
            vec!["clippy", "--all-targets", "--", "-D", "warnings"],
        ),
        VerifyStep::Test => ("cargo", vec!["test"]),
        VerifyStep::ForbiddenDeps => ("cargo", vec!["test", "forbidden_deps"]),
    };

    let result = Command::new(program)
        .args(&args)
        .current_dir(workspace_path)
        .output();

    let duration = start.elapsed();

    match result {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            let exit_code = output.status.code();

            if output.status.success() {
                StepResult::passed(step, duration, stdout, stderr)
            } else {
                StepResult::failed(step, duration, stdout, stderr, exit_code)
            }
        }
        Err(e) => StepResult::failed(
            step,
            duration,
            String::new(),
            format!("Failed to execute command: {e}"),
            None,
        ),
    }
}

/// Check if a path should be gitignored based on artifact policy.
#[must_use]
pub fn should_gitignore(path: &Path) -> bool {
    let path_str = path.to_string_lossy();
    for rule in ARTIFACT_RULES {
        if !rule.versioned && path_str.starts_with(rule.pattern.trim_end_matches('/')) {
            return true;
        }
    }
    false
}

/// Get patterns that should be in .gitignore.
#[must_use]
pub fn gitignore_patterns() -> Vec<&'static str> {
    ARTIFACT_RULES
        .iter()
        .filter(|r| !r.versioned)
        .map(|r| r.pattern)
        .collect()
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{TestResult, ensure, ensure_equal};

    #[test]
    fn verify_step_strings_are_stable() -> TestResult {
        ensure_equal(&VerifyStep::Format.as_str(), &"format", "format")?;
        ensure_equal(&VerifyStep::Clippy.as_str(), &"clippy", "clippy")?;
        ensure_equal(&VerifyStep::Test.as_str(), &"test", "test")?;
        ensure_equal(
            &VerifyStep::ForbiddenDeps.as_str(),
            &"forbidden_deps",
            "forbidden_deps",
        )
    }

    #[test]
    fn verify_step_all_contains_all_steps() -> TestResult {
        ensure_equal(&VerifyStep::ALL.len(), &4, "step count")
    }

    #[test]
    fn all_steps_are_required() -> TestResult {
        for step in VerifyStep::ALL {
            ensure(step.is_required(), format!("{} should be required", step))?;
        }
        Ok(())
    }

    #[test]
    fn artifact_policy_has_expected_rules() -> TestResult {
        ensure(ARTIFACT_RULES.len() >= 4, "at least 4 rules")?;

        let patterns: Vec<&str> = ARTIFACT_RULES.iter().map(|r| r.pattern).collect();
        ensure(patterns.contains(&"target/"), "has target/")?;
        ensure(patterns.contains(&".ee/"), "has .ee/")?;
        ensure(patterns.contains(&"tests/fixtures/"), "has tests/fixtures/")
    }

    #[test]
    fn target_is_not_versioned() -> TestResult {
        let target_rule = ARTIFACT_RULES
            .iter()
            .find(|r| r.pattern == "target/")
            .ok_or("target rule not found")?;
        ensure(!target_rule.versioned, "target should not be versioned")?;
        ensure(target_rule.ci_cached, "target should be CI cached")
    }

    #[test]
    fn test_fixtures_are_versioned() -> TestResult {
        let fixtures_rule = ARTIFACT_RULES
            .iter()
            .find(|r| r.pattern == "tests/fixtures/")
            .ok_or("fixtures rule not found")?;
        ensure(fixtures_rule.versioned, "fixtures should be versioned")
    }

    #[test]
    fn gitignore_patterns_excludes_versioned() -> TestResult {
        let patterns = gitignore_patterns();
        ensure(
            !patterns.contains(&"tests/fixtures/"),
            "fixtures not ignored",
        )?;
        ensure(patterns.contains(&"target/"), "target is ignored")
    }

    #[test]
    fn should_gitignore_detects_target() -> TestResult {
        ensure(
            should_gitignore(Path::new("target/debug/ee")),
            "target/debug/ee should be ignored",
        )
    }

    #[test]
    fn should_gitignore_allows_fixtures() -> TestResult {
        ensure(
            !should_gitignore(Path::new("tests/fixtures/agent_detect/codex")),
            "fixtures should not be ignored",
        )
    }

    #[test]
    fn dry_run_skips_all_steps() -> TestResult {
        let options = VerifyOptions {
            dry_run: true,
            ..Default::default()
        };
        let report = run_verification(&options);

        ensure_equal(&report.skipped_count, &4, "all skipped")?;
        ensure(report.all_passed, "dry run passes")
    }

    #[test]
    fn schema_constants_are_stable() -> TestResult {
        ensure_equal(
            &VERIFY_REPORT_SCHEMA_V1,
            &"ee.verify.report.v1",
            "verify schema",
        )?;
        ensure_equal(
            &ARTIFACT_POLICY_SCHEMA_V1,
            &"ee.artifact_policy.v1",
            "artifact schema",
        )
    }

    #[test]
    fn verify_report_json_has_required_fields() -> TestResult {
        let options = VerifyOptions {
            dry_run: true,
            ..Default::default()
        };
        let report = run_verification(&options);
        let json = report.data_json();

        ensure(json.get("command").is_some(), "has command")?;
        ensure(json.get("allPassed").is_some(), "has allPassed")?;
        ensure(json.get("steps").is_some(), "has steps")
    }

    #[test]
    fn artifact_policy_report_json_has_rules() -> TestResult {
        let report = artifact_policy_report();
        let json = report.data_json();

        ensure(json.get("rules").is_some(), "has rules")?;
        let rules = json.get("rules").and_then(|v| v.as_array());
        ensure(rules.is_some(), "rules is array")?;
        ensure(rules.unwrap().len() >= 4, "at least 4 rules")
    }
}
