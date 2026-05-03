//! Demo schema types (EE-370).
//!
//! A demo is a CI-executable sequence of commands that demonstrates or verifies
//! a product claim. The demo.yaml file defines one or more demos, each with:
//!
//! - A unique `demo_id`
//! - An optional `claim_id` linking to a claim in claims.yaml
//! - A sequence of commands with expected outputs
//! - Artifact hashes for reproducibility verification
//!
//! # Demo.yaml Schema
//!
//! ```yaml
//! schema: ee.demo_file.v1
//! version: 1
//! demos:
//!   - id: demo_00000000000000000000000001
//!     claim_id: claim_00000000000000000000000001
//!     title: "Context packing produces stable output"
//!     description: "Verifies that ee context produces deterministic JSON"
//!     commands:
//!       - command: "ee context 'test query' --json"
//!         cwd: "/tmp/test_workspace"
//!         timeout_ms: 30000
//!         expected_exit_code: 0
//!         expected_stdout_schema: "ee.context_response.v1"
//!         expected_stderr_contains: []
//!         artifact_outputs:
//!           - path: "stdout.json"
//!             blake3_hash: "abc123..."
//!     env_overrides:
//!       EE_LOG_LEVEL: "warn"
//!     tags:
//!       - determinism
//!       - context
//! ```

use std::collections::HashMap;
use std::fmt;
use std::path::{Component, Path};
use std::str::FromStr;

use serde::Deserialize;

use super::{ClaimId, DemoId};

/// Schema version for demo file.
pub const DEMO_FILE_SCHEMA_V1: &str = "ee.demo_file.v1";

/// Schema version for individual demo entry.
pub const DEMO_ENTRY_SCHEMA_V1: &str = "ee.demo_entry.v1";

/// Schema version for demo command.
pub const DEMO_COMMAND_SCHEMA_V1: &str = "ee.demo_command.v1";

/// Schema version for demo artifact output.
pub const DEMO_ARTIFACT_OUTPUT_SCHEMA_V1: &str = "ee.demo_artifact_output.v1";

/// Schema version for demo run result.
pub const DEMO_RUN_RESULT_SCHEMA_V1: &str = "ee.demo_run_result.v1";

/// Status of a demo.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum DemoStatus {
    /// Demo has not been run yet.
    #[default]
    Pending,
    /// Demo is currently running.
    Running,
    /// Demo completed successfully.
    Passed,
    /// Demo failed verification.
    Failed,
    /// Demo was skipped (e.g., platform mismatch).
    Skipped,
    /// Demo timed out.
    TimedOut,
}

impl DemoStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Passed => "passed",
            Self::Failed => "failed",
            Self::Skipped => "skipped",
            Self::TimedOut => "timed_out",
        }
    }

    #[must_use]
    pub const fn all() -> [Self; 6] {
        [
            Self::Pending,
            Self::Running,
            Self::Passed,
            Self::Failed,
            Self::Skipped,
            Self::TimedOut,
        ]
    }

    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Passed | Self::Failed | Self::Skipped | Self::TimedOut
        )
    }
}

impl fmt::Display for DemoStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseDemoStatusError(pub String);

impl fmt::Display for ParseDemoStatusError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unknown demo status: {}", self.0)
    }
}

impl std::error::Error for ParseDemoStatusError {}

impl FromStr for DemoStatus {
    type Err = ParseDemoStatusError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "pending" => Ok(Self::Pending),
            "running" => Ok(Self::Running),
            "passed" => Ok(Self::Passed),
            "failed" => Ok(Self::Failed),
            "skipped" => Ok(Self::Skipped),
            "timed_out" => Ok(Self::TimedOut),
            other => Err(ParseDemoStatusError(other.to_owned())),
        }
    }
}

/// Type of output verification to perform.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum OutputVerification {
    /// No verification of output content.
    #[default]
    None,
    /// Verify output matches exact bytes/hash.
    Exact,
    /// Verify output contains expected substrings.
    Contains,
    /// Verify output matches a JSON schema.
    Schema,
    /// Verify output matches a regex pattern.
    Regex,
}

impl OutputVerification {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Exact => "exact",
            Self::Contains => "contains",
            Self::Schema => "schema",
            Self::Regex => "regex",
        }
    }

    #[must_use]
    pub const fn all() -> [Self; 5] {
        [
            Self::None,
            Self::Exact,
            Self::Contains,
            Self::Schema,
            Self::Regex,
        ]
    }
}

impl fmt::Display for OutputVerification {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseOutputVerificationError(pub String);

impl fmt::Display for ParseOutputVerificationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unknown output verification type: {}", self.0)
    }
}

impl std::error::Error for ParseOutputVerificationError {}

impl FromStr for OutputVerification {
    type Err = ParseOutputVerificationError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "none" => Ok(Self::None),
            "exact" => Ok(Self::Exact),
            "contains" => Ok(Self::Contains),
            "schema" => Ok(Self::Schema),
            "regex" => Ok(Self::Regex),
            other => Err(ParseOutputVerificationError(other.to_owned())),
        }
    }
}

#[derive(Debug, Deserialize)]
struct RawDemoFile {
    schema: Option<String>,
    version: Option<u32>,
    #[serde(default)]
    demos: Vec<RawDemoEntry>,
}

#[derive(Debug, Deserialize)]
struct RawDemoEntry {
    id: Option<String>,
    #[serde(default, alias = "claimId")]
    claim_id: Option<String>,
    title: Option<String>,
    description: Option<String>,
    #[serde(default)]
    commands: Vec<RawDemoCommand>,
    #[serde(default, alias = "envOverrides")]
    env_overrides: HashMap<String, String>,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    platforms: Vec<String>,
    #[serde(default, alias = "minEeVersion")]
    min_ee_version: Option<String>,
    #[serde(default = "default_true", alias = "ciEnabled")]
    ci_enabled: bool,
    #[serde(default = "default_demo_timeout_ms", alias = "totalTimeoutMs")]
    total_timeout_ms: u64,
}

#[derive(Debug, Deserialize)]
struct RawDemoCommand {
    command: Option<String>,
    cwd: Option<String>,
    #[serde(default = "default_command_timeout_ms", alias = "timeoutMs")]
    timeout_ms: u64,
    #[serde(default, alias = "expectedExitCode")]
    expected_exit_code: i32,
    #[serde(default, alias = "expectedStdoutSchema")]
    expected_stdout_schema: Option<String>,
    #[serde(default, alias = "expectedStderrContains")]
    expected_stderr_contains: Vec<String>,
    #[serde(default, alias = "expectedStderrExcludes")]
    expected_stderr_excludes: Vec<String>,
    #[serde(default, alias = "expectedStdoutContains")]
    expected_stdout_contains: Vec<String>,
    #[serde(default, alias = "expectedStdoutExcludes")]
    expected_stdout_excludes: Vec<String>,
    #[serde(default, alias = "stdoutVerification")]
    stdout_verification: Option<String>,
    #[serde(default, alias = "stderrVerification")]
    stderr_verification: Option<String>,
    #[serde(default, alias = "artifactOutputs")]
    artifact_outputs: Vec<RawDemoArtifactOutput>,
    description: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawDemoArtifactOutput {
    path: Option<String>,
    #[serde(default, alias = "blake3Hash")]
    blake3_hash: Option<String>,
    #[serde(default, alias = "sizeBytes")]
    size_bytes: Option<u64>,
    #[serde(default)]
    optional: bool,
}

const fn default_true() -> bool {
    true
}

const fn default_command_timeout_ms() -> u64 {
    30_000
}

const fn default_demo_timeout_ms() -> u64 {
    300_000
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DemoParseError {
    pub message: String,
}

impl DemoParseError {
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for DemoParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for DemoParseError {}

/// Parse a `demo.yaml` manifest into typed demo records.
///
/// The parser accepts the documented snake_case YAML fields and the camelCase
/// wire names used by JSON/golden fixtures, then validation enforces the stable
/// typed ID, timeout, hash, and artifact-path contracts.
pub fn parse_demo_file_yaml(input: &str) -> Result<DemoFile, DemoParseError> {
    let raw: RawDemoFile = serde_yaml::from_str(input)
        .map_err(|error| DemoParseError::new(format!("failed to parse demo.yaml: {error}")))?;

    if let Some(schema) = raw.schema.as_deref() {
        if schema != DEMO_FILE_SCHEMA_V1 {
            return Err(DemoParseError::new(format!(
                "unsupported demo schema `{schema}`; expected `{DEMO_FILE_SCHEMA_V1}`"
            )));
        }
    }

    if let Some(version) = raw.version {
        if version != 1 {
            return Err(DemoParseError::new(format!(
                "unsupported demo manifest version `{version}`; expected `1`"
            )));
        }
    }

    let mut file = DemoFile {
        schema: DEMO_FILE_SCHEMA_V1,
        version: 1,
        demos: Vec::new(),
    };

    for (demo_index, raw_demo) in raw.demos.into_iter().enumerate() {
        file.demos.push(convert_raw_demo(demo_index, raw_demo)?);
    }

    let validation_errors = validate_demo_file(&file);
    if !validation_errors.is_empty() {
        let messages = validation_errors
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("; ");
        return Err(DemoParseError::new(format!(
            "demo.yaml validation failed: {messages}"
        )));
    }

    Ok(file)
}

fn convert_raw_demo(
    demo_index: usize,
    raw_demo: RawDemoEntry,
) -> Result<DemoEntry, DemoParseError> {
    let raw_id = required_field(raw_demo.id, "demo id", demo_index, None)?;
    let id = raw_id.parse::<DemoId>().map_err(|error| {
        DemoParseError::new(format!(
            "invalid demo id `{raw_id}` at demos[{demo_index}]: {error}"
        ))
    })?;
    let title = required_field(raw_demo.title, "demo title", demo_index, None)?;
    let description = raw_demo.description.unwrap_or_default();
    let mut entry = DemoEntry::new(id, title, description);

    if let Some(raw_claim_id) = raw_demo.claim_id {
        let claim_id = raw_claim_id.parse::<ClaimId>().map_err(|error| {
            DemoParseError::new(format!(
                "invalid claim id `{raw_claim_id}` at demos[{demo_index}]: {error}"
            ))
        })?;
        entry.claim_id = Some(claim_id);
    }

    entry.env_overrides = raw_demo.env_overrides;
    entry.tags = raw_demo.tags;
    entry.platforms = raw_demo.platforms;
    entry.min_ee_version = raw_demo.min_ee_version;
    entry.ci_enabled = raw_demo.ci_enabled;
    entry.total_timeout_ms = raw_demo.total_timeout_ms;

    for (command_index, raw_command) in raw_demo.commands.into_iter().enumerate() {
        entry
            .commands
            .push(convert_raw_command(demo_index, command_index, raw_command)?);
    }

    Ok(entry)
}

fn convert_raw_command(
    demo_index: usize,
    command_index: usize,
    raw_command: RawDemoCommand,
) -> Result<DemoCommand, DemoParseError> {
    let command = required_field(
        raw_command.command,
        "demo command",
        demo_index,
        Some(command_index),
    )?;
    let stdout_verification = parse_optional_verification(
        raw_command.stdout_verification,
        "stdoutVerification",
        demo_index,
        command_index,
    )?;
    let stderr_verification = parse_optional_verification(
        raw_command.stderr_verification,
        "stderrVerification",
        demo_index,
        command_index,
    )?;

    let mut converted = DemoCommand::new(command)
        .with_timeout_ms(raw_command.timeout_ms)
        .with_expected_exit_code(raw_command.expected_exit_code);
    converted.cwd = raw_command.cwd;
    converted.expected_stdout_schema = raw_command.expected_stdout_schema;
    converted.expected_stderr_contains = raw_command.expected_stderr_contains;
    converted.expected_stderr_excludes = raw_command.expected_stderr_excludes;
    converted.expected_stdout_contains = raw_command.expected_stdout_contains;
    converted.expected_stdout_excludes = raw_command.expected_stdout_excludes;
    converted.stdout_verification = stdout_verification;
    converted.stderr_verification = stderr_verification;
    converted.description = raw_command.description;

    if converted.expected_stdout_schema.is_some() {
        converted.stdout_verification = OutputVerification::Schema;
    }

    for raw_artifact in raw_command.artifact_outputs {
        converted.artifact_outputs.push(convert_raw_artifact(
            demo_index,
            command_index,
            raw_artifact,
        )?);
    }

    Ok(converted)
}

fn convert_raw_artifact(
    demo_index: usize,
    command_index: usize,
    raw_artifact: RawDemoArtifactOutput,
) -> Result<DemoArtifactOutput, DemoParseError> {
    let path = required_field(
        raw_artifact.path,
        "artifact output path",
        demo_index,
        Some(command_index),
    )?;
    let mut artifact = DemoArtifactOutput::new(path);
    artifact.blake3_hash = raw_artifact.blake3_hash;
    artifact.size_bytes = raw_artifact.size_bytes;
    artifact.optional = raw_artifact.optional;
    Ok(artifact)
}

fn required_field(
    value: Option<String>,
    field_name: &str,
    demo_index: usize,
    command_index: Option<usize>,
) -> Result<String, DemoParseError> {
    let value = value.unwrap_or_default();
    if value.trim().is_empty() {
        let location = match command_index {
            Some(index) => format!("demos[{demo_index}].commands[{index}]"),
            None => format!("demos[{demo_index}]"),
        };
        return Err(DemoParseError::new(format!(
            "missing {field_name} at {location}"
        )));
    }
    Ok(value)
}

fn parse_optional_verification(
    value: Option<String>,
    field_name: &str,
    demo_index: usize,
    command_index: usize,
) -> Result<OutputVerification, DemoParseError> {
    let Some(value) = value else {
        return Ok(OutputVerification::None);
    };
    value.parse::<OutputVerification>().map_err(|error| {
        DemoParseError::new(format!(
            "invalid {field_name} `{value}` at demos[{demo_index}].commands[{command_index}]: {error}"
        ))
    })
}

/// An artifact output produced by a demo command.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DemoArtifactOutput {
    /// Relative path where the artifact is written.
    pub path: String,
    /// Expected blake3 hash of the artifact content.
    pub blake3_hash: Option<String>,
    /// Size in bytes (for quick validation).
    pub size_bytes: Option<u64>,
    /// Whether this artifact is optional.
    pub optional: bool,
}

impl DemoArtifactOutput {
    #[must_use]
    pub fn new(path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            blake3_hash: None,
            size_bytes: None,
            optional: false,
        }
    }

    #[must_use]
    pub fn with_blake3_hash(mut self, hash: impl Into<String>) -> Self {
        self.blake3_hash = Some(hash.into());
        self
    }

    #[must_use]
    pub fn with_size_bytes(mut self, size: u64) -> Self {
        self.size_bytes = Some(size);
        self
    }

    #[must_use]
    pub fn optional(mut self) -> Self {
        self.optional = true;
        self
    }
}

/// A command to execute as part of a demo.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DemoCommand {
    /// The command string to execute.
    pub command: String,
    /// Working directory for the command.
    pub cwd: Option<String>,
    /// Timeout in milliseconds.
    pub timeout_ms: u64,
    /// Expected exit code (0 for success).
    pub expected_exit_code: i32,
    /// Schema to validate stdout against.
    pub expected_stdout_schema: Option<String>,
    /// Substrings that stderr must contain.
    pub expected_stderr_contains: Vec<String>,
    /// Substrings that stderr must NOT contain.
    pub expected_stderr_excludes: Vec<String>,
    /// Substrings that stdout must contain.
    pub expected_stdout_contains: Vec<String>,
    /// Substrings that stdout must NOT contain.
    pub expected_stdout_excludes: Vec<String>,
    /// Type of stdout verification.
    pub stdout_verification: OutputVerification,
    /// Type of stderr verification.
    pub stderr_verification: OutputVerification,
    /// Expected artifact outputs from this command.
    pub artifact_outputs: Vec<DemoArtifactOutput>,
    /// Human-readable description of what this command does.
    pub description: Option<String>,
}

impl DemoCommand {
    #[must_use]
    pub fn new(command: impl Into<String>) -> Self {
        Self {
            command: command.into(),
            cwd: None,
            timeout_ms: 30_000,
            expected_exit_code: 0,
            expected_stdout_schema: None,
            expected_stderr_contains: Vec::new(),
            expected_stderr_excludes: Vec::new(),
            expected_stdout_contains: Vec::new(),
            expected_stdout_excludes: Vec::new(),
            stdout_verification: OutputVerification::None,
            stderr_verification: OutputVerification::None,
            artifact_outputs: Vec::new(),
            description: None,
        }
    }

    #[must_use]
    pub fn with_cwd(mut self, cwd: impl Into<String>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    #[must_use]
    pub fn with_timeout_ms(mut self, timeout_ms: u64) -> Self {
        self.timeout_ms = timeout_ms;
        self
    }

    #[must_use]
    pub fn with_expected_exit_code(mut self, code: i32) -> Self {
        self.expected_exit_code = code;
        self
    }

    #[must_use]
    pub fn with_stdout_schema(mut self, schema: impl Into<String>) -> Self {
        self.expected_stdout_schema = Some(schema.into());
        self.stdout_verification = OutputVerification::Schema;
        self
    }

    #[must_use]
    pub fn with_stderr_contains(mut self, substring: impl Into<String>) -> Self {
        self.expected_stderr_contains.push(substring.into());
        self
    }

    #[must_use]
    pub fn with_stdout_contains(mut self, substring: impl Into<String>) -> Self {
        self.expected_stdout_contains.push(substring.into());
        self
    }

    #[must_use]
    pub fn with_artifact_output(mut self, artifact: DemoArtifactOutput) -> Self {
        self.artifact_outputs.push(artifact);
        self
    }

    #[must_use]
    pub fn with_description(mut self, desc: impl Into<String>) -> Self {
        self.description = Some(desc.into());
        self
    }
}

/// A demo entry in the demo.yaml file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DemoEntry {
    /// Unique identifier for this demo.
    pub id: DemoId,
    /// Optional claim this demo verifies.
    pub claim_id: Option<ClaimId>,
    /// Human-readable title.
    pub title: String,
    /// Detailed description of what the demo demonstrates.
    pub description: String,
    /// Sequence of commands to execute.
    pub commands: Vec<DemoCommand>,
    /// Environment variable overrides for all commands.
    pub env_overrides: HashMap<String, String>,
    /// Tags for filtering/categorization.
    pub tags: Vec<String>,
    /// Platform restrictions (e.g., "linux", "macos").
    pub platforms: Vec<String>,
    /// Minimum ee version required.
    pub min_ee_version: Option<String>,
    /// Whether this demo should run in CI.
    pub ci_enabled: bool,
    /// Timeout for the entire demo in milliseconds.
    pub total_timeout_ms: u64,
}

impl DemoEntry {
    #[must_use]
    pub fn new(id: DemoId, title: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            id,
            claim_id: None,
            title: title.into(),
            description: description.into(),
            commands: Vec::new(),
            env_overrides: HashMap::new(),
            tags: Vec::new(),
            platforms: Vec::new(),
            min_ee_version: None,
            ci_enabled: true,
            total_timeout_ms: 300_000,
        }
    }

    #[must_use]
    pub fn with_claim_id(mut self, claim_id: ClaimId) -> Self {
        self.claim_id = Some(claim_id);
        self
    }

    #[must_use]
    pub fn with_command(mut self, command: DemoCommand) -> Self {
        self.commands.push(command);
        self
    }

    #[must_use]
    pub fn with_env_override(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env_overrides.insert(key.into(), value.into());
        self
    }

    #[must_use]
    pub fn with_tag(mut self, tag: impl Into<String>) -> Self {
        self.tags.push(tag.into());
        self
    }

    #[must_use]
    pub fn with_platform(mut self, platform: impl Into<String>) -> Self {
        self.platforms.push(platform.into());
        self
    }

    #[must_use]
    pub fn ci_disabled(mut self) -> Self {
        self.ci_enabled = false;
        self
    }

    #[must_use]
    pub fn with_total_timeout_ms(mut self, timeout_ms: u64) -> Self {
        self.total_timeout_ms = timeout_ms;
        self
    }
}

/// The top-level demo.yaml file structure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DemoFile {
    /// Schema identifier.
    pub schema: &'static str,
    /// Schema version number.
    pub version: u32,
    /// List of demos defined in this file.
    pub demos: Vec<DemoEntry>,
}

impl Default for DemoFile {
    fn default() -> Self {
        Self {
            schema: DEMO_FILE_SCHEMA_V1,
            version: 1,
            demos: Vec::new(),
        }
    }
}

impl DemoFile {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn with_demo(mut self, demo: DemoEntry) -> Self {
        self.demos.push(demo);
        self
    }

    #[must_use]
    pub fn demo_count(&self) -> usize {
        self.demos.len()
    }

    #[must_use]
    pub fn ci_enabled_count(&self) -> usize {
        self.demos.iter().filter(|d| d.ci_enabled).count()
    }
}

/// Result of running a single demo command.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DemoCommandResult {
    /// Index of the command in the demo's command list.
    pub command_index: usize,
    /// The command that was executed.
    pub command: String,
    /// Actual exit code.
    pub exit_code: i32,
    /// Whether the exit code matched expected.
    pub exit_code_matched: bool,
    /// Whether stdout verification passed.
    pub stdout_verified: bool,
    /// Whether stderr verification passed.
    pub stderr_verified: bool,
    /// Paths to actual artifact outputs produced.
    pub artifact_paths: Vec<String>,
    /// Whether all artifact hashes matched.
    pub artifacts_verified: bool,
    /// Elapsed time in milliseconds.
    pub elapsed_ms: u64,
    /// Error message if command failed.
    pub error: Option<String>,
}

impl DemoCommandResult {
    #[must_use]
    pub fn success(command_index: usize, command: String, elapsed_ms: u64) -> Self {
        Self {
            command_index,
            command,
            exit_code: 0,
            exit_code_matched: true,
            stdout_verified: true,
            stderr_verified: true,
            artifact_paths: Vec::new(),
            artifacts_verified: true,
            elapsed_ms,
            error: None,
        }
    }

    #[must_use]
    pub fn failure(
        command_index: usize,
        command: String,
        exit_code: i32,
        error: impl Into<String>,
        elapsed_ms: u64,
    ) -> Self {
        Self {
            command_index,
            command,
            exit_code,
            exit_code_matched: false,
            stdout_verified: false,
            stderr_verified: false,
            artifact_paths: Vec::new(),
            artifacts_verified: false,
            elapsed_ms,
            error: Some(error.into()),
        }
    }

    #[must_use]
    pub fn all_passed(&self) -> bool {
        self.exit_code_matched
            && self.stdout_verified
            && self.stderr_verified
            && self.artifacts_verified
            && self.error.is_none()
    }
}

/// Result of running an entire demo.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DemoRunResult {
    /// Schema identifier.
    pub schema: &'static str,
    /// ID of the demo that was run.
    pub demo_id: DemoId,
    /// Overall status of the demo run.
    pub status: DemoStatus,
    /// Results for each command.
    pub command_results: Vec<DemoCommandResult>,
    /// Total elapsed time in milliseconds.
    pub total_elapsed_ms: u64,
    /// Timestamp when the demo started (RFC 3339).
    pub started_at: String,
    /// Timestamp when the demo completed (RFC 3339).
    pub completed_at: Option<String>,
    /// Summary error message if the demo failed.
    pub error_summary: Option<String>,
}

impl DemoRunResult {
    #[must_use]
    pub fn new(demo_id: DemoId, started_at: impl Into<String>) -> Self {
        Self {
            schema: DEMO_RUN_RESULT_SCHEMA_V1,
            demo_id,
            status: DemoStatus::Running,
            command_results: Vec::new(),
            total_elapsed_ms: 0,
            started_at: started_at.into(),
            completed_at: None,
            error_summary: None,
        }
    }

    #[must_use]
    pub fn with_command_result(mut self, result: DemoCommandResult) -> Self {
        self.command_results.push(result);
        self
    }

    pub fn complete(&mut self, completed_at: impl Into<String>, total_elapsed_ms: u64) {
        self.completed_at = Some(completed_at.into());
        self.total_elapsed_ms = total_elapsed_ms;

        let all_passed =
            !self.command_results.is_empty() && self.command_results.iter().all(|r| r.all_passed());
        self.status = if all_passed {
            DemoStatus::Passed
        } else {
            DemoStatus::Failed
        };

        if !all_passed {
            self.error_summary = self
                .command_results
                .iter()
                .find(|r| !r.all_passed())
                .and_then(|r| r.error.clone())
                .or_else(|| Some("Demo completed without command results".to_string()));
        }
    }

    pub fn timeout(&mut self, completed_at: impl Into<String>, total_elapsed_ms: u64) {
        self.completed_at = Some(completed_at.into());
        self.total_elapsed_ms = total_elapsed_ms;
        self.status = DemoStatus::TimedOut;
        self.error_summary = Some("Demo exceeded total timeout".to_string());
    }

    pub fn skip(&mut self, reason: impl Into<String>) {
        self.status = DemoStatus::Skipped;
        self.error_summary = Some(reason.into());
    }

    #[must_use]
    pub fn passed_count(&self) -> usize {
        self.command_results
            .iter()
            .filter(|r| r.all_passed())
            .count()
    }

    #[must_use]
    pub fn failed_count(&self) -> usize {
        self.command_results
            .iter()
            .filter(|r| !r.all_passed())
            .count()
    }
}

/// Validation errors for demo files.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DemoValidationErrorKind {
    /// Demo ID is missing or invalid.
    InvalidDemoId,
    /// Claim ID reference is invalid.
    InvalidClaimId,
    /// No commands defined.
    NoCommands,
    /// Command is empty.
    EmptyCommand,
    /// Timeout is zero or negative.
    InvalidTimeout,
    /// Duplicate demo ID.
    DuplicateDemoId,
    /// Invalid blake3 hash format.
    InvalidBlake3Hash,
    /// Artifact path is absolute or escapes the artifact root.
    InvalidArtifactPath,
    /// Artifact output has no expected hash or size predicate.
    MissingArtifactVerification,
    /// Invalid schema reference.
    InvalidSchemaReference,
}

impl DemoValidationErrorKind {
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::InvalidDemoId => "invalid_demo_id",
            Self::InvalidClaimId => "invalid_claim_id",
            Self::NoCommands => "no_commands",
            Self::EmptyCommand => "empty_command",
            Self::InvalidTimeout => "invalid_timeout",
            Self::DuplicateDemoId => "duplicate_demo_id",
            Self::InvalidBlake3Hash => "invalid_blake3_hash",
            Self::InvalidArtifactPath => "invalid_artifact_path",
            Self::MissingArtifactVerification => "missing_artifact_verification",
            Self::InvalidSchemaReference => "invalid_schema_reference",
        }
    }
}

impl fmt::Display for DemoValidationErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DemoValidationError {
    pub kind: DemoValidationErrorKind,
    pub message: String,
    pub demo_id: Option<String>,
    pub command_index: Option<usize>,
}

impl DemoValidationError {
    #[must_use]
    pub fn new(kind: DemoValidationErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            demo_id: None,
            command_index: None,
        }
    }

    #[must_use]
    pub fn with_demo_id(mut self, demo_id: impl Into<String>) -> Self {
        self.demo_id = Some(demo_id.into());
        self
    }

    #[must_use]
    pub fn with_command_index(mut self, index: usize) -> Self {
        self.command_index = Some(index);
        self
    }
}

impl fmt::Display for DemoValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.kind, self.message)?;
        if let Some(ref demo_id) = self.demo_id {
            write!(f, " (demo: {})", demo_id)?;
        }
        if let Some(index) = self.command_index {
            write!(f, " (command: {})", index)?;
        }
        Ok(())
    }
}

impl std::error::Error for DemoValidationError {}

/// Validate a demo file structure.
pub fn validate_demo_file(file: &DemoFile) -> Vec<DemoValidationError> {
    let mut errors = Vec::new();
    let mut seen_ids = std::collections::HashSet::new();

    for demo in &file.demos {
        let demo_id_str = demo.id.to_string();

        if !seen_ids.insert(demo_id_str.clone()) {
            errors.push(
                DemoValidationError::new(
                    DemoValidationErrorKind::DuplicateDemoId,
                    format!("Duplicate demo ID: {}", demo_id_str),
                )
                .with_demo_id(&demo_id_str),
            );
        }

        if demo.commands.is_empty() {
            errors.push(
                DemoValidationError::new(
                    DemoValidationErrorKind::NoCommands,
                    "Demo has no commands",
                )
                .with_demo_id(&demo_id_str),
            );
        }

        for (i, cmd) in demo.commands.iter().enumerate() {
            if cmd.command.trim().is_empty() {
                errors.push(
                    DemoValidationError::new(
                        DemoValidationErrorKind::EmptyCommand,
                        "Command is empty",
                    )
                    .with_demo_id(&demo_id_str)
                    .with_command_index(i),
                );
            }

            if cmd.timeout_ms == 0 {
                errors.push(
                    DemoValidationError::new(
                        DemoValidationErrorKind::InvalidTimeout,
                        "Command timeout must be greater than 0",
                    )
                    .with_demo_id(&demo_id_str)
                    .with_command_index(i),
                );
            }

            for artifact in &cmd.artifact_outputs {
                if !is_valid_demo_artifact_path(&artifact.path) {
                    errors.push(
                        DemoValidationError::new(
                            DemoValidationErrorKind::InvalidArtifactPath,
                            format!("Invalid artifact path: {}", artifact.path),
                        )
                        .with_demo_id(&demo_id_str)
                        .with_command_index(i),
                    );
                }

                if artifact.blake3_hash.is_none() && artifact.size_bytes.is_none() {
                    errors.push(
                        DemoValidationError::new(
                            DemoValidationErrorKind::MissingArtifactVerification,
                            format!(
                                "Artifact output must declare blake3 hash or size: {}",
                                artifact.path
                            ),
                        )
                        .with_demo_id(&demo_id_str)
                        .with_command_index(i),
                    );
                }

                if let Some(ref hash) = artifact.blake3_hash {
                    if hash.len() != 64 || !hash.chars().all(|c| c.is_ascii_hexdigit()) {
                        errors.push(
                            DemoValidationError::new(
                                DemoValidationErrorKind::InvalidBlake3Hash,
                                format!("Invalid blake3 hash: {}", hash),
                            )
                            .with_demo_id(&demo_id_str)
                            .with_command_index(i),
                        );
                    }
                }
            }
        }

        if demo.total_timeout_ms == 0 {
            errors.push(
                DemoValidationError::new(
                    DemoValidationErrorKind::InvalidTimeout,
                    "Demo total timeout must be greater than 0",
                )
                .with_demo_id(&demo_id_str),
            );
        }
    }

    errors
}

#[must_use]
pub fn is_valid_demo_artifact_path(path: &str) -> bool {
    let trimmed = path.trim();
    if trimmed.is_empty() || trimmed != path {
        return false;
    }
    if path
        .chars()
        .any(|ch| ch == '\\' || ch == ':' || ch.is_control())
    {
        return false;
    }
    let mut has_normal_component = false;
    for component in Path::new(path).components() {
        match component {
            Component::Normal(_) => has_normal_component = true,
            Component::CurDir => {}
            Component::RootDir | Component::Prefix(_) | Component::ParentDir => return false,
        }
    }
    has_normal_component
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_demo_id() -> DemoId {
        DemoId::from_uuid(uuid::Uuid::nil())
    }

    fn test_claim_id() -> ClaimId {
        ClaimId::from_uuid(uuid::Uuid::nil())
    }

    #[test]
    fn demo_status_roundtrip() {
        for status in DemoStatus::all() {
            let parsed = status.as_str().parse::<DemoStatus>();
            assert_eq!(parsed, Ok(status));
        }
    }

    #[test]
    fn output_verification_roundtrip() {
        for verification in OutputVerification::all() {
            let parsed = verification.as_str().parse::<OutputVerification>();
            assert_eq!(parsed, Ok(verification));
        }
    }

    #[test]
    fn demo_command_builder() {
        let cmd = DemoCommand::new("ee context 'test' --json")
            .with_cwd("/tmp/workspace")
            .with_timeout_ms(60_000)
            .with_expected_exit_code(0)
            .with_stdout_schema("ee.context_response.v1")
            .with_stderr_contains("INFO")
            .with_artifact_output(
                DemoArtifactOutput::new("stdout.json")
                    .with_blake3_hash("a".repeat(64))
                    .with_size_bytes(1024),
            )
            .with_description("Test context command");

        assert_eq!(cmd.command, "ee context 'test' --json");
        assert_eq!(cmd.cwd, Some("/tmp/workspace".to_string()));
        assert_eq!(cmd.timeout_ms, 60_000);
        assert_eq!(cmd.expected_exit_code, 0);
        assert_eq!(
            cmd.expected_stdout_schema,
            Some("ee.context_response.v1".to_string())
        );
        assert_eq!(cmd.stdout_verification, OutputVerification::Schema);
        assert_eq!(cmd.expected_stderr_contains, vec!["INFO"]);
        assert_eq!(cmd.artifact_outputs.len(), 1);
        assert_eq!(cmd.description, Some("Test context command".to_string()));
    }

    #[test]
    fn demo_entry_builder() {
        let demo = DemoEntry::new(test_demo_id(), "Test Demo", "A demo for testing")
            .with_claim_id(test_claim_id())
            .with_command(DemoCommand::new("ee status --json"))
            .with_env_override("EE_LOG_LEVEL", "debug")
            .with_tag("test")
            .with_platform("linux")
            .with_total_timeout_ms(120_000);

        assert_eq!(demo.id, test_demo_id());
        assert_eq!(demo.claim_id, Some(test_claim_id()));
        assert_eq!(demo.title, "Test Demo");
        assert_eq!(demo.commands.len(), 1);
        assert_eq!(
            demo.env_overrides.get("EE_LOG_LEVEL"),
            Some(&"debug".to_string())
        );
        assert_eq!(demo.tags, vec!["test"]);
        assert_eq!(demo.platforms, vec!["linux"]);
        assert!(demo.ci_enabled);
        assert_eq!(demo.total_timeout_ms, 120_000);
    }

    #[test]
    fn demo_file_builder() {
        let file = DemoFile::new().with_demo(
            DemoEntry::new(test_demo_id(), "Test Demo", "Description")
                .with_command(DemoCommand::new("echo hello")),
        );

        assert_eq!(file.schema, DEMO_FILE_SCHEMA_V1);
        assert_eq!(file.version, 1);
        assert_eq!(file.demo_count(), 1);
        assert_eq!(file.ci_enabled_count(), 1);
    }

    #[test]
    fn demo_command_result_success() {
        let result = DemoCommandResult::success(0, "echo hello".to_string(), 50);
        assert!(result.all_passed());
        assert_eq!(result.exit_code, 0);
        assert!(result.error.is_none());
    }

    #[test]
    fn demo_command_result_failure() {
        let result = DemoCommandResult::failure(0, "exit 1".to_string(), 1, "Command failed", 100);
        assert!(!result.all_passed());
        assert_eq!(result.exit_code, 1);
        assert_eq!(result.error, Some("Command failed".to_string()));
    }

    #[test]
    fn demo_run_result_completion() {
        let mut result = DemoRunResult::new(test_demo_id(), "2026-05-01T00:00:00Z");
        result =
            result.with_command_result(DemoCommandResult::success(0, "echo hello".to_string(), 50));
        result.complete("2026-05-01T00:00:01Z", 1000);

        assert_eq!(result.status, DemoStatus::Passed);
        assert_eq!(result.passed_count(), 1);
        assert_eq!(result.failed_count(), 0);
        assert!(result.error_summary.is_none());
    }

    #[test]
    fn demo_run_result_failure() {
        let mut result = DemoRunResult::new(test_demo_id(), "2026-05-01T00:00:00Z");
        result = result.with_command_result(DemoCommandResult::failure(
            0,
            "exit 1".to_string(),
            1,
            "Command failed",
            100,
        ));
        result.complete("2026-05-01T00:00:01Z", 1000);

        assert_eq!(result.status, DemoStatus::Failed);
        assert_eq!(result.passed_count(), 0);
        assert_eq!(result.failed_count(), 1);
        assert_eq!(result.error_summary, Some("Command failed".to_string()));
    }

    #[test]
    fn demo_run_result_empty_completion_fails() {
        let mut result = DemoRunResult::new(test_demo_id(), "2026-05-01T00:00:00Z");
        result.complete("2026-05-01T00:00:01Z", 1000);

        assert_eq!(result.status, DemoStatus::Failed);
        assert_eq!(result.passed_count(), 0);
        assert_eq!(result.failed_count(), 0);
        assert_eq!(
            result.error_summary,
            Some("Demo completed without command results".to_string())
        );
    }

    #[test]
    fn demo_run_result_timeout() {
        let mut result = DemoRunResult::new(test_demo_id(), "2026-05-01T00:00:00Z");
        result.timeout("2026-05-01T00:05:00Z", 300_000);

        assert_eq!(result.status, DemoStatus::TimedOut);
        assert!(result.error_summary.is_some());
    }

    #[test]
    fn demo_run_result_skip() {
        let mut result = DemoRunResult::new(test_demo_id(), "2026-05-01T00:00:00Z");
        result.skip("Platform not supported");

        assert_eq!(result.status, DemoStatus::Skipped);
        assert_eq!(
            result.error_summary,
            Some("Platform not supported".to_string())
        );
    }

    #[test]
    fn validate_demo_file_valid() {
        let file = DemoFile::new().with_demo(
            DemoEntry::new(test_demo_id(), "Test", "Description")
                .with_command(DemoCommand::new("echo hello")),
        );

        let errors = validate_demo_file(&file);
        assert!(errors.is_empty(), "Expected no errors, got: {:?}", errors);
    }

    #[test]
    fn validate_demo_file_no_commands() {
        let file = DemoFile::new().with_demo(DemoEntry::new(test_demo_id(), "Test", "Description"));

        let errors = validate_demo_file(&file);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].kind, DemoValidationErrorKind::NoCommands);
    }

    #[test]
    fn validate_demo_file_empty_command() {
        let file = DemoFile::new().with_demo(
            DemoEntry::new(test_demo_id(), "Test", "Description")
                .with_command(DemoCommand::new("   ")),
        );

        let errors = validate_demo_file(&file);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].kind, DemoValidationErrorKind::EmptyCommand);
    }

    #[test]
    fn validate_demo_file_invalid_timeout() {
        let file = DemoFile::new().with_demo(
            DemoEntry::new(test_demo_id(), "Test", "Description")
                .with_command(DemoCommand::new("echo hello").with_timeout_ms(0)),
        );

        let errors = validate_demo_file(&file);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].kind, DemoValidationErrorKind::InvalidTimeout);
    }

    #[test]
    fn validate_demo_file_invalid_blake3_hash() {
        let file = DemoFile::new().with_demo(
            DemoEntry::new(test_demo_id(), "Test", "Description").with_command(
                DemoCommand::new("echo hello").with_artifact_output(
                    DemoArtifactOutput::new("out.txt").with_blake3_hash("invalid"),
                ),
            ),
        );

        let errors = validate_demo_file(&file);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].kind, DemoValidationErrorKind::InvalidBlake3Hash);
    }

    #[test]
    fn validate_demo_file_rejects_unsafe_artifact_paths() {
        for path in [
            "",
            ".",
            "/tmp/out.txt",
            "../out.txt",
            "nested/../out.txt",
            r"..\out.txt",
            r"C:\tmp\out.txt",
            r"nested\out.txt",
            "out.txt:ads",
            " out.txt",
            "out.txt ",
        ] {
            let file = DemoFile::new().with_demo(
                DemoEntry::new(test_demo_id(), "Test", "Description").with_command(
                    DemoCommand::new("echo hello")
                        .with_artifact_output(DemoArtifactOutput::new(path).with_size_bytes(1)),
                ),
            );

            let errors = validate_demo_file(&file);
            assert_eq!(errors.len(), 1);
            assert_eq!(errors[0].kind, DemoValidationErrorKind::InvalidArtifactPath);
        }

        let file = DemoFile::new().with_demo(
            DemoEntry::new(test_demo_id(), "Test", "Description").with_command(
                DemoCommand::new("echo hello").with_artifact_output(
                    DemoArtifactOutput::new("./nested/out.txt").with_size_bytes(1),
                ),
            ),
        );

        assert!(validate_demo_file(&file).is_empty());
    }

    #[test]
    fn validate_demo_file_rejects_artifacts_without_verification_predicate() {
        let file = DemoFile::new().with_demo(
            DemoEntry::new(test_demo_id(), "Test", "Description").with_command(
                DemoCommand::new("echo hello")
                    .with_artifact_output(DemoArtifactOutput::new("out.txt")),
            ),
        );

        let errors = validate_demo_file(&file);
        assert_eq!(errors.len(), 1);
        assert_eq!(
            errors[0].kind,
            DemoValidationErrorKind::MissingArtifactVerification
        );
    }

    #[test]
    fn validate_demo_file_duplicate_id() {
        let file = DemoFile::new()
            .with_demo(
                DemoEntry::new(test_demo_id(), "Test 1", "Description")
                    .with_command(DemoCommand::new("echo 1")),
            )
            .with_demo(
                DemoEntry::new(test_demo_id(), "Test 2", "Description")
                    .with_command(DemoCommand::new("echo 2")),
            );

        let errors = validate_demo_file(&file);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].kind, DemoValidationErrorKind::DuplicateDemoId);
    }

    #[test]
    fn demo_status_is_terminal() {
        assert!(!DemoStatus::Pending.is_terminal());
        assert!(!DemoStatus::Running.is_terminal());
        assert!(DemoStatus::Passed.is_terminal());
        assert!(DemoStatus::Failed.is_terminal());
        assert!(DemoStatus::Skipped.is_terminal());
        assert!(DemoStatus::TimedOut.is_terminal());
    }

    #[test]
    fn demo_artifact_output_builder() {
        let artifact = DemoArtifactOutput::new("output.json")
            .with_blake3_hash("a".repeat(64))
            .with_size_bytes(2048)
            .optional();

        assert_eq!(artifact.path, "output.json");
        assert_eq!(artifact.blake3_hash, Some("a".repeat(64)));
        assert_eq!(artifact.size_bytes, Some(2048));
        assert!(artifact.optional);
    }

    #[test]
    fn demo_entry_ci_disabled() {
        let demo = DemoEntry::new(test_demo_id(), "Test", "Description")
            .with_command(DemoCommand::new("echo hello"))
            .ci_disabled();

        assert!(!demo.ci_enabled);

        let file = DemoFile::new().with_demo(demo);
        assert_eq!(file.ci_enabled_count(), 0);
    }
}
