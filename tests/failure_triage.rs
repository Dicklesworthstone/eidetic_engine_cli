//! Failure triage, replay artifacts, and redaction-aware log capture (EE-TST-006).
//!
//! This module provides infrastructure for capturing structured failure artifacts
//! that help agents diagnose and replay failed test scenarios.

use std::collections::HashMap;
use std::env;
use std::path::PathBuf;
use std::process::Output;
use std::time::{Duration, Instant};

// =============================================================================
// Redaction Classes and Tracking
// =============================================================================

/// Classification of redacted content for audit trail.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum RedactionClass {
    /// API keys, tokens, passwords.
    Secret,
    /// File paths that may reveal system structure.
    Path,
    /// Email addresses, usernames.
    Identity,
    /// Environment variables with sensitive values.
    EnvVar,
    /// Database connection strings.
    ConnectionString,
    /// Other sensitive content.
    Other,
}

impl RedactionClass {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Secret => "secret",
            Self::Path => "path",
            Self::Identity => "identity",
            Self::EnvVar => "env_var",
            Self::ConnectionString => "connection_string",
            Self::Other => "other",
        }
    }
}

/// Tracks redaction events for audit.
#[derive(Clone, Debug, Default)]
pub struct RedactionLog {
    /// Count of redactions per class.
    pub counts: HashMap<RedactionClass, u32>,
    /// Total bytes redacted.
    pub total_bytes_redacted: usize,
    /// Hash of redacted content for evidence correlation.
    pub evidence_hashes: Vec<String>,
}

impl RedactionLog {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record(&mut self, class: RedactionClass, original_len: usize, hash: String) {
        *self.counts.entry(class).or_insert(0) += 1;
        self.total_bytes_redacted += original_len;
        self.evidence_hashes.push(hash);
    }

    pub fn is_empty(&self) -> bool {
        self.counts.is_empty()
    }

    pub fn total_count(&self) -> u32 {
        self.counts.values().sum()
    }

    /// Format as JSON for artifact embedding.
    pub fn to_json(&self) -> String {
        let counts: Vec<String> = self
            .counts
            .iter()
            .map(|(k, v)| format!("\"{}\":{}", k.as_str(), v))
            .collect();

        format!(
            "{{\"totalCount\":{},\"totalBytesRedacted\":{},\"classes\":{{{}}},\"evidenceHashCount\":{}}}",
            self.total_count(),
            self.total_bytes_redacted,
            counts.join(","),
            self.evidence_hashes.len()
        )
    }
}

// =============================================================================
// Content Redaction
// =============================================================================

/// Known secret patterns for redaction.
const SECRET_PATTERNS: &[&str] = &[
    "sk-",           // OpenAI keys
    "sk_live_",      // Stripe live keys
    "sk_test_",      // Stripe test keys
    "ghp_",          // GitHub PAT
    "gho_",          // GitHub OAuth
    "github_pat_",   // GitHub PAT new format
    "ANTHROPIC_API", // Anthropic keys
    "Bearer ",       // Bearer tokens
    "password=",     // Password params
    "secret=",       // Secret params
    "token=",        // Token params
    "api_key=",      // API key params
];

/// Redact secrets from content, tracking redactions.
pub fn redact_content(content: &str, log: &mut RedactionLog) -> String {
    let mut result = content.to_string();

    for pattern in SECRET_PATTERNS {
        if let Some(start) = result.find(pattern) {
            // Find end of secret (whitespace, quote, or end of line)
            let after_pattern = start + pattern.len();
            let end = result[after_pattern..]
                .find(|c: char| c.is_whitespace() || c == '"' || c == '\'' || c == ',' || c == '}')
                .map(|i| after_pattern + i)
                .unwrap_or(result.len());

            let secret = &result[start..end];
            let hash = format!("{:x}", blake3_hash(secret.as_bytes()));

            let short_hash = if hash.len() >= 8 { &hash[..8] } else { &hash };
            let redacted = format!("{}[REDACTED:{}]", pattern, short_hash);
            log.record(RedactionClass::Secret, secret.len(), hash);
            result = format!("{}{}{}", &result[..start], redacted, &result[end..]);
        }
    }

    // Redact home directory paths
    if let Ok(home) = env::var("HOME") {
        if result.contains(&home) {
            let hash = format!("{:x}", blake3_hash(home.as_bytes()));
            let short_hash: String = if hash.len() >= 8 {
                hash[..8].to_string()
            } else {
                hash.clone()
            };
            log.record(RedactionClass::Path, home.len(), hash);
            result = result.replace(&home, &format!("$HOME[{}]", short_hash));
        }
    }

    result
}

/// Simple hash for evidence correlation (using first 8 bytes of content hash).
fn blake3_hash(data: &[u8]) -> u64 {
    // Simple hash for testing - in production would use blake3
    let mut hash: u64 = 0;
    for (i, &byte) in data.iter().enumerate() {
        hash = hash.wrapping_add((byte as u64).wrapping_mul((i as u64).wrapping_add(1)));
    }
    hash
}

// =============================================================================
// Replay Metadata
// =============================================================================

/// Minimal metadata needed to replay a failed scenario.
#[derive(Clone, Debug)]
pub struct ReplayMetadata {
    /// The exact command that was run.
    pub command: String,
    /// Command arguments.
    pub args: Vec<String>,
    /// Working directory (redacted).
    pub cwd: String,
    /// Sanitized environment variables (names only, values redacted).
    pub env_names: Vec<String>,
    /// Binary version.
    pub binary_version: String,
    /// Schema version used.
    pub schema_version: String,
    /// Feature flags active.
    pub feature_flags: Vec<String>,
    /// Fixture hash if applicable.
    pub fixture_hash: Option<String>,
    /// Database migration version if applicable.
    pub db_migration_version: Option<String>,
}

impl ReplayMetadata {
    /// Create from a command execution context.
    pub fn capture(command: &str, args: &[&str]) -> Self {
        let cwd = env::current_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| "<unknown>".to_string());

        // Collect relevant env var names (not values)
        let env_names: Vec<String> = env::vars()
            .filter(|(k, _)| k.starts_with("EE_") || k.starts_with("CARGO_"))
            .map(|(k, _)| k)
            .collect();

        Self {
            command: command.to_string(),
            args: args.iter().map(|s| s.to_string()).collect(),
            cwd,
            env_names,
            binary_version: env!("CARGO_PKG_VERSION").to_string(),
            schema_version: "ee.response.v1".to_string(),
            feature_flags: vec![],
            fixture_hash: None,
            db_migration_version: None,
        }
    }

    /// Generate a one-command replay hint.
    pub fn replay_command(&self) -> String {
        let args = self.args.join(" ");
        format!("{} {}", self.command, args)
    }

    /// Format as JSON.
    pub fn to_json(&self) -> String {
        let args_json: Vec<String> = self.args.iter().map(|a| format!("\"{}\"", a)).collect();
        let env_json: Vec<String> = self.env_names.iter().map(|e| format!("\"{}\"", e)).collect();
        let flags_json: Vec<String> = self
            .feature_flags
            .iter()
            .map(|f| format!("\"{}\"", f))
            .collect();

        format!(
            "{{\"command\":\"{}\",\"args\":[{}],\"cwd\":\"{}\",\"envNames\":[{}],\"binaryVersion\":\"{}\",\"schemaVersion\":\"{}\",\"featureFlags\":[{}],\"fixtureHash\":{},\"dbMigrationVersion\":{}}}",
            self.command,
            args_json.join(","),
            self.cwd,
            env_json.join(","),
            self.binary_version,
            self.schema_version,
            flags_json.join(","),
            self.fixture_hash.as_ref().map(|h| format!("\"{}\"", h)).unwrap_or_else(|| "null".to_string()),
            self.db_migration_version.as_ref().map(|v| format!("\"{}\"", v)).unwrap_or_else(|| "null".to_string())
        )
    }
}

// =============================================================================
// Failure Artifact
// =============================================================================

/// Root cause category for quick triage.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FailureCategory {
    /// Assertion failure in test.
    Assertion,
    /// Process exited with non-zero code.
    ExitCode,
    /// Process timed out.
    Timeout,
    /// Schema validation failed.
    SchemaValidation,
    /// Golden file mismatch.
    GoldenMismatch,
    /// Panic or crash.
    Panic,
    /// Unknown or unclassified.
    Unknown,
}

impl FailureCategory {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Assertion => "assertion",
            Self::ExitCode => "exit_code",
            Self::Timeout => "timeout",
            Self::SchemaValidation => "schema_validation",
            Self::GoldenMismatch => "golden_mismatch",
            Self::Panic => "panic",
            Self::Unknown => "unknown",
        }
    }

    /// Classify from error message.
    pub fn classify(message: &str) -> Self {
        let lower = message.to_lowercase();
        if lower.contains("assertion") || lower.contains("assert_eq") || lower.contains("ensure") {
            Self::Assertion
        } else if lower.contains("exit code") || lower.contains("exitcode") {
            Self::ExitCode
        } else if lower.contains("timeout") || lower.contains("timed out") {
            Self::Timeout
        } else if lower.contains("schema") {
            Self::SchemaValidation
        } else if lower.contains("golden") || lower.contains("mismatch") {
            Self::GoldenMismatch
        } else if lower.contains("panic") || lower.contains("panicked") {
            Self::Panic
        } else {
            Self::Unknown
        }
    }
}

/// A structured failure artifact for agent consumption.
#[derive(Clone, Debug)]
pub struct FailureArtifact {
    /// Test name or scenario identifier.
    pub test_name: String,
    /// Root cause category.
    pub category: FailureCategory,
    /// First failure message (concise, for status updates).
    pub first_failure: String,
    /// Full error message (redacted).
    pub full_message: String,
    /// Exit code if applicable.
    pub exit_code: Option<i32>,
    /// Elapsed time.
    pub elapsed: Duration,
    /// Replay metadata.
    pub replay: ReplayMetadata,
    /// Redaction log.
    pub redaction: RedactionLog,
    /// Stdout artifact path.
    pub stdout_path: Option<PathBuf>,
    /// Stderr artifact path.
    pub stderr_path: Option<PathBuf>,
}

impl FailureArtifact {
    /// Create from a test failure context.
    pub fn from_test_failure(
        test_name: &str,
        error_message: &str,
        output: Option<&Output>,
        replay: ReplayMetadata,
        elapsed: Duration,
    ) -> Self {
        let mut redaction = RedactionLog::new();

        // Redact error message
        let full_message = redact_content(error_message, &mut redaction);

        // Extract first line for concise summary
        let first_failure = full_message
            .lines()
            .next()
            .unwrap_or(&full_message)
            .chars()
            .take(200)
            .collect();

        let exit_code = output.map(|o| o.status.code().unwrap_or(-1));

        Self {
            test_name: test_name.to_string(),
            category: FailureCategory::classify(&full_message),
            first_failure,
            full_message,
            exit_code,
            elapsed,
            replay,
            redaction,
            stdout_path: None,
            stderr_path: None,
        }
    }

    /// Format as JSON summary (optimized for agents).
    pub fn to_json(&self) -> String {
        format!(
            "{{\"schema\":\"ee.test_failure.v1\",\"testName\":\"{}\",\"category\":\"{}\",\"firstFailure\":\"{}\",\"exitCode\":{},\"elapsedMs\":{},\"replay\":{},\"redaction\":{},\"stdoutPath\":{},\"stderrPath\":{}}}",
            self.test_name,
            self.category.as_str(),
            self.first_failure.replace('\"', "\\\"").replace('\n', "\\n"),
            self.exit_code.map(|c| c.to_string()).unwrap_or_else(|| "null".to_string()),
            self.elapsed.as_millis(),
            self.replay.to_json(),
            self.redaction.to_json(),
            self.stdout_path.as_ref().map(|p| format!("\"{}\"", p.display())).unwrap_or_else(|| "null".to_string()),
            self.stderr_path.as_ref().map(|p| format!("\"{}\"", p.display())).unwrap_or_else(|| "null".to_string())
        )
    }

    /// Generate replay hint for agents.
    pub fn replay_hint(&self) -> String {
        format!(
            "# Replay: {}\n# Category: {}\n# Exit: {:?}\n{}",
            self.test_name,
            self.category.as_str(),
            self.exit_code,
            self.replay.replay_command()
        )
    }
}

// =============================================================================
// Test Execution Wrapper
// =============================================================================

/// Wrapper for running commands with failure artifact capture.
pub struct TriageRunner {
    pub command: String,
    pub args: Vec<String>,
}

impl TriageRunner {
    pub fn new(command: &str, args: &[&str]) -> Self {
        Self {
            command: command.to_string(),
            args: args.iter().map(|s| s.to_string()).collect(),
        }
    }

    /// Run and capture failure artifact if command fails.
    #[allow(clippy::result_large_err)]
    pub fn run_with_triage(&self) -> Result<Output, FailureArtifact> {
        let start = Instant::now();
        let args_refs: Vec<&str> = self.args.iter().map(|s| s.as_str()).collect();
        let replay = ReplayMetadata::capture(&self.command, &args_refs);

        let output = std::process::Command::new(&self.command)
            .args(&self.args)
            .output()
            .map_err(|e| {
                FailureArtifact::from_test_failure(
                    &format!("{}:{}", self.command, self.args.join(":")),
                    &format!("Failed to execute: {}", e),
                    None,
                    replay.clone(),
                    start.elapsed(),
                )
            })?;

        if output.status.success() {
            Ok(output)
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            let message = format!(
                "Exit code: {:?}\nStderr:\n{}\nStdout:\n{}",
                output.status.code(),
                stderr,
                stdout
            );

            Err(FailureArtifact::from_test_failure(
                &format!("{}:{}", self.command, self.args.join(":")),
                &message,
                Some(&output),
                replay,
                start.elapsed(),
            ))
        }
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), String>;

    fn ensure<T: std::fmt::Debug + PartialEq>(actual: T, expected: T, ctx: &str) -> TestResult {
        if actual == expected {
            Ok(())
        } else {
            Err(format!("{ctx}: expected {expected:?}, got {actual:?}"))
        }
    }

    #[test]
    fn redaction_class_as_str_is_stable() -> TestResult {
        ensure(RedactionClass::Secret.as_str(), "secret", "secret")?;
        ensure(RedactionClass::Path.as_str(), "path", "path")?;
        ensure(RedactionClass::Identity.as_str(), "identity", "identity")?;
        ensure(RedactionClass::EnvVar.as_str(), "env_var", "env_var")?;
        ensure(
            RedactionClass::ConnectionString.as_str(),
            "connection_string",
            "connection_string",
        )?;
        ensure(RedactionClass::Other.as_str(), "other", "other")
    }

    #[test]
    fn redaction_log_tracks_counts() -> TestResult {
        let mut log = RedactionLog::new();
        ensure(log.is_empty(), true, "initially empty")?;

        log.record(RedactionClass::Secret, 32, "hash1".to_string());
        log.record(RedactionClass::Secret, 16, "hash2".to_string());
        log.record(RedactionClass::Path, 64, "hash3".to_string());

        ensure(log.is_empty(), false, "not empty after records")?;
        ensure(log.total_count(), 3, "total count")?;
        ensure(log.total_bytes_redacted, 112, "total bytes")?;
        ensure(log.evidence_hashes.len(), 3, "evidence hash count")
    }

    #[test]
    fn redact_content_removes_secrets() -> TestResult {
        let mut log = RedactionLog::new();
        let input = "API key: sk-abc123secret and token=mytoken123";
        let output = redact_content(input, &mut log);

        ensure(
            output.contains("sk-abc123secret"),
            false,
            "secret key removed",
        )?;
        ensure(
            output.contains("mytoken123"),
            false,
            "token value removed",
        )?;
        ensure(output.contains("[REDACTED:"), true, "redaction marker present")?;
        ensure(log.total_count() > 0, true, "redactions logged")
    }

    #[test]
    fn failure_category_classify_detects_assertion() -> TestResult {
        ensure(
            FailureCategory::classify("assertion failed: expected 1, got 2"),
            FailureCategory::Assertion,
            "assertion",
        )?;
        ensure(
            FailureCategory::classify("assert_eq! failed"),
            FailureCategory::Assertion,
            "assert_eq",
        )
    }

    #[test]
    fn failure_category_classify_detects_exit_code() -> TestResult {
        ensure(
            FailureCategory::classify("process exited with exit code 1"),
            FailureCategory::ExitCode,
            "exit code",
        )
    }

    #[test]
    fn failure_category_classify_detects_timeout() -> TestResult {
        ensure(
            FailureCategory::classify("operation timed out after 30s"),
            FailureCategory::Timeout,
            "timeout",
        )
    }

    #[test]
    fn failure_category_classify_detects_schema() -> TestResult {
        ensure(
            FailureCategory::classify("schema validation failed for ee.response.v1"),
            FailureCategory::SchemaValidation,
            "schema validation",
        )
    }

    #[test]
    fn failure_category_classify_detects_golden() -> TestResult {
        ensure(
            FailureCategory::classify("golden file mismatch: status.json"),
            FailureCategory::GoldenMismatch,
            "golden mismatch",
        )
    }

    #[test]
    fn failure_category_classify_detects_panic() -> TestResult {
        ensure(
            FailureCategory::classify("thread 'main' panicked at 'index out of bounds'"),
            FailureCategory::Panic,
            "panic",
        )
    }

    #[test]
    fn failure_category_classify_unknown() -> TestResult {
        ensure(
            FailureCategory::classify("something weird happened"),
            FailureCategory::Unknown,
            "unknown",
        )
    }

    #[test]
    fn replay_metadata_captures_context() -> TestResult {
        let meta = ReplayMetadata::capture("ee", &["status", "--json"]);

        ensure(meta.command, "ee".to_string(), "command")?;
        ensure(meta.args.len(), 2, "args count")?;
        ensure(meta.args[0].clone(), "status".to_string(), "first arg")?;
        ensure(meta.binary_version.is_empty(), false, "has version")
    }

    #[test]
    fn replay_metadata_generates_command() -> TestResult {
        let meta = ReplayMetadata::capture("ee", &["status", "--json"]);
        let cmd = meta.replay_command();

        ensure(cmd.contains("ee"), true, "contains command")?;
        ensure(cmd.contains("status"), true, "contains arg")
    }

    #[test]
    fn failure_artifact_to_json_is_valid() -> TestResult {
        let replay = ReplayMetadata::capture("ee", &["test"]);
        let artifact = FailureArtifact::from_test_failure(
            "test_example",
            "assertion failed",
            None,
            replay,
            Duration::from_millis(100),
        );

        let json = artifact.to_json();

        ensure(
            json.contains("\"schema\":\"ee.test_failure.v1\""),
            true,
            "has schema",
        )?;
        ensure(
            json.contains("\"testName\":\"test_example\""),
            true,
            "has test name",
        )?;
        ensure(
            json.contains("\"category\":\"assertion\""),
            true,
            "has category",
        )
    }

    #[test]
    fn failure_artifact_replay_hint_format() -> TestResult {
        let replay = ReplayMetadata::capture("cargo", &["test", "my_test"]);
        let artifact = FailureArtifact::from_test_failure(
            "my_test",
            "test failed",
            None,
            replay,
            Duration::from_millis(50),
        );

        let hint = artifact.replay_hint();

        ensure(hint.contains("# Replay:"), true, "has replay header")?;
        ensure(hint.contains("cargo test"), true, "has command")
    }

    #[test]
    fn redaction_log_to_json_format() -> TestResult {
        let mut log = RedactionLog::new();
        log.record(RedactionClass::Secret, 32, "hash1".to_string());

        let json = log.to_json();

        ensure(json.contains("\"totalCount\":1"), true, "has total count")?;
        ensure(json.contains("\"secret\":1"), true, "has secret class")
    }
}
