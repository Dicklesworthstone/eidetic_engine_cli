//! AFR1 (bd-1zb7k.19.1): redacted local-only flight recorder for agent
//! `ee` command workloads.
//!
//! The recorder is the trace-row builder; it consumes a caller-curated
//! [`FlightRecorderInputs`] snapshot (which CANNOT carry raw task strings,
//! raw query text, raw memory bodies, raw provenance text, raw mail
//! bodies, secrets, environment dumps, or full file listings) and emits
//! an [`AgentWorkloadTrace`] that conforms to
//! `docs/schemas/ee.agent_workload_trace.v1.json`. The output is suitable
//! for write-once append to a local JSONL file under `EE_FLIGHT_RECORDER_DIR`.
//!
//! Scope: pure constructor + redaction-safety guards + stable BLAKE3
//! hashing + append-only JSONL storage + deterministic replay summaries.
//! Automatic per-command capture, status/doctor posture wiring, and
//! integration coverage across `context`/`search`/`why`/`status` can layer
//! on this storage engine without changing the trace schema.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};

/// Public schema identifier, matching the title of
/// `docs/schemas/ee.agent_workload_trace.v1.json`.
pub const AGENT_WORKLOAD_TRACE_SCHEMA_V1: &str = "ee.agent_workload_trace.v1";

/// Default replay read cap. This mirrors the flight-recorder status default and
/// keeps ad-hoc replay from loading arbitrarily large trace files.
pub const DEFAULT_FLIGHT_RECORDER_REPLAY_MAX_BYTES: u64 = 268_435_456;

/// Default redaction posture for the recorder. The schema only allows
/// `strict` or `audit`; `strict` is the default. Both levels omit raw
/// text and only accept caller-provided BLAKE3 memory hashes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RedactionLevel {
    Strict,
    Audit,
}

impl RedactionLevel {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Strict => "strict",
            Self::Audit => "audit",
        }
    }

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "strict" => Some(Self::Strict),
            "audit" => Some(Self::Audit),
            _ => None,
        }
    }
}

impl<'de> Deserialize<'de> for RedactionLevel {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value)
            .ok_or_else(|| de::Error::custom(format!("invalid redaction level `{value}`")))
    }
}

impl Serialize for RedactionLevel {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

/// Stable token-estimator vocabulary mirroring the schema enum.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TokenEstimatorId {
    BytesDiv4,
    TiktokenCl100kBase,
    Approximate,
}

impl TokenEstimatorId {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::BytesDiv4 => "bytes_div_4",
            Self::TiktokenCl100kBase => "tiktoken_cl100k_base",
            Self::Approximate => "approximate",
        }
    }

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "bytes_div_4" => Some(Self::BytesDiv4),
            "tiktoken_cl100k_base" => Some(Self::TiktokenCl100kBase),
            "approximate" => Some(Self::Approximate),
            _ => None,
        }
    }
}

impl<'de> Deserialize<'de> for TokenEstimatorId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value)
            .ok_or_else(|| de::Error::custom(format!("invalid token estimator `{value}`")))
    }
}

impl Serialize for TokenEstimatorId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

/// Stable harness-program vocabulary mirroring the schema enum.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HarnessProgram {
    ClaudeCode,
    CodexCli,
    GeminiCli,
    Cursor,
    Windsurf,
    EeCliDirect,
    Unknown,
}

impl HarnessProgram {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude-code",
            Self::CodexCli => "codex-cli",
            Self::GeminiCli => "gemini-cli",
            Self::Cursor => "cursor",
            Self::Windsurf => "windsurf",
            Self::EeCliDirect => "ee-cli-direct",
            Self::Unknown => "unknown",
        }
    }

    #[must_use]
    pub fn parse(value: &str) -> Self {
        match value {
            "claude-code" => Self::ClaudeCode,
            "codex-cli" => Self::CodexCli,
            "gemini-cli" => Self::GeminiCli,
            "cursor" => Self::Cursor,
            "windsurf" => Self::Windsurf,
            "ee-cli-direct" => Self::EeCliDirect,
            _ => Self::Unknown,
        }
    }
}

impl Serialize for HarnessProgram {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for HarnessProgram {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Ok(Self::parse(&value))
    }
}

/// Command-shape input. Carries ONLY the verb chain, the names of flags
/// that were set, the count of positional args (NEVER the values), and
/// the output-format enum. Caller must never put raw query, raw task,
/// raw argv, or raw memory text into these fields.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandShapeInput<'a> {
    pub verbs: &'a [&'a str],
    pub positional_arity: u32,
    pub flag_names: &'a [&'a str],
    pub output_format: Option<&'a str>,
}

/// Caller inputs for [`record_workload`]. Every field is shape-only.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FlightRecorderInputs<'a> {
    pub redaction_level: RedactionLevel,
    pub recorded_at_rfc3339: &'a str,
    pub command: CommandShapeInput<'a>,
    pub exit_code: u8,
    pub elapsed_ms: u64,
    pub response_byte_count: u64,
    pub response_token_estimate: Option<u64>,
    pub token_estimator_id: Option<TokenEstimatorId>,
    pub harness_program: HarnessProgram,
    pub harness_model_family: Option<&'a str>,
    /// Caller-precomputed BLAKE3 hashes of selected memory rows. The
    /// recorder asserts every entry already starts with the `blake3:`
    /// prefix; raw memory ids must NOT be passed.
    pub memory_hashes: &'a [&'a str],
    pub degraded_codes: &'a [&'a str],
}

/// Output row produced by [`record_workload`].
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentWorkloadTrace {
    pub schema: String,
    pub side_effect_free: bool,
    pub redaction_level: RedactionLevel,
    pub trace_id: String,
    pub recorded_at: String,
    pub command: CommandShape,
    pub exit_code: u8,
    pub elapsed_ms: u64,
    pub response_byte_count: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_token_estimate: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_estimator_id: Option<TokenEstimatorId>,
    pub harness_identity: HarnessIdentity,
    pub memory_references: Vec<MemoryHashRef>,
    pub degraded_codes: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandShape {
    pub verbs: Vec<String>,
    pub positional_arity: u32,
    pub flag_names: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_format: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HarnessIdentity {
    pub program: HarnessProgram,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_family: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryHashRef {
    pub hash: String,
}

/// Validation error returned when the caller's inputs would violate the
/// recorder's redaction contract.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FlightRecorderError {
    InvalidVerbChain {
        verbs: Vec<String>,
    },
    InvalidFlagName {
        flag: String,
    },
    InvalidOutputFormat {
        value: String,
    },
    InvalidMemoryHash {
        value: String,
    },
    InvalidModelFamily {
        value: String,
    },
    InvalidRecordedAt {
        value: String,
    },
    InvalidDegradedCode {
        code: String,
    },
    SensitiveInput {
        field: &'static str,
    },
    Io {
        path: PathBuf,
        message: String,
    },
    QuotaExceeded {
        projected_bytes: u64,
        max_bytes: u64,
    },
    MalformedTrace {
        line: usize,
        message: String,
    },
}

impl std::fmt::Display for FlightRecorderError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidVerbChain { verbs } => {
                write!(formatter, "invalid flight-recorder verb chain: {verbs:?}")
            }
            Self::InvalidFlagName { flag } => {
                write!(formatter, "invalid flight-recorder flag name: {flag}")
            }
            Self::InvalidOutputFormat { value } => {
                write!(formatter, "invalid flight-recorder output format: {value}")
            }
            Self::InvalidMemoryHash { value } => {
                write!(formatter, "invalid flight-recorder memory hash: {value}")
            }
            Self::InvalidModelFamily { value } => {
                write!(formatter, "invalid flight-recorder model family: {value}")
            }
            Self::InvalidRecordedAt { value } => {
                write!(formatter, "invalid flight-recorder timestamp: {value}")
            }
            Self::InvalidDegradedCode { code } => {
                write!(formatter, "invalid flight-recorder degraded code: {code}")
            }
            Self::SensitiveInput { field } => write!(
                formatter,
                "flight-recorder {field} contained sensitive credential material"
            ),
            Self::Io { path, message } => {
                write!(
                    formatter,
                    "flight-recorder I/O failed at {}: {message}",
                    path.display()
                )
            }
            Self::QuotaExceeded {
                projected_bytes,
                max_bytes,
            } => write!(
                formatter,
                "flight-recorder quota exceeded: projected {projected_bytes} bytes exceeds max {max_bytes}"
            ),
            Self::MalformedTrace { line, message } => {
                write!(
                    formatter,
                    "malformed flight-recorder trace at line {line}: {message}"
                )
            }
        }
    }
}

impl std::error::Error for FlightRecorderError {}

/// Append-only JSONL storage options. Retention is reported and hashed into
/// posture, but this module deliberately never deletes trace files.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FlightRecorderStorageOptions {
    pub directory: PathBuf,
    pub retention_days: u32,
    pub max_bytes: u64,
}

impl FlightRecorderStorageOptions {
    #[must_use]
    pub fn trace_path(&self) -> PathBuf {
        self.directory.join("agent-workload-trace.jsonl")
    }
}

/// Result of one append-only trace write.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FlightRecorderAppendReport {
    pub schema: &'static str,
    pub side_effect_free: bool,
    pub trace_id: String,
    pub trace_path: String,
    pub bytes_written: u64,
    pub retained_bytes: u64,
    pub retention_days: u32,
    pub max_bytes: u64,
    pub redaction_level: RedactionLevel,
}

/// Deterministic replay summary over one redacted JSONL trace.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FlightRecorderReplayReport {
    pub schema: &'static str,
    pub side_effect_free: bool,
    pub trace_path: String,
    pub trace_hash: String,
    pub row_count: usize,
    pub malformed_lines: usize,
    pub command_counts: Vec<FlightRecorderCount>,
    pub degraded_code_counts: Vec<FlightRecorderCount>,
    pub response_bytes_total: u64,
    pub response_token_estimate_total: u64,
    pub memory_reference_count: usize,
    pub replay_hash: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FlightRecorderCount {
    pub key: String,
    pub count: usize,
}

/// Pure constructor: validate the caller-curated inputs against the
/// recorder's redaction contract, derive the trace id, and emit the
/// `AgentWorkloadTrace` row.
pub fn record_workload(
    inputs: &FlightRecorderInputs<'_>,
) -> Result<AgentWorkloadTrace, FlightRecorderError> {
    validate_inputs(inputs)?;
    let deduped_flag_names = sorted_unique_strs(inputs.command.flag_names);
    let deduped_memory_hashes = sorted_unique_strs(inputs.memory_hashes);
    let deduped_codes = sorted_unique_strs(inputs.degraded_codes);
    let trace_id = derive_trace_id(
        inputs,
        &deduped_flag_names,
        &deduped_memory_hashes,
        &deduped_codes,
    );
    Ok(AgentWorkloadTrace {
        schema: AGENT_WORKLOAD_TRACE_SCHEMA_V1.to_owned(),
        side_effect_free: true,
        redaction_level: inputs.redaction_level,
        trace_id,
        recorded_at: inputs.recorded_at_rfc3339.to_string(),
        command: CommandShape {
            verbs: inputs
                .command
                .verbs
                .iter()
                .map(|v| (*v).to_string())
                .collect(),
            positional_arity: inputs.command.positional_arity,
            flag_names: deduped_flag_names
                .iter()
                .map(|f| (*f).to_string())
                .collect(),
            output_format: inputs.command.output_format.map(str::to_string),
        },
        exit_code: inputs.exit_code,
        elapsed_ms: inputs.elapsed_ms,
        response_byte_count: inputs.response_byte_count,
        response_token_estimate: inputs.response_token_estimate,
        token_estimator_id: inputs.token_estimator_id,
        harness_identity: HarnessIdentity {
            program: inputs.harness_program,
            model_family: inputs.harness_model_family.map(str::to_string),
        },
        memory_references: deduped_memory_hashes
            .iter()
            .map(|hash| MemoryHashRef {
                hash: (*hash).to_string(),
            })
            .collect(),
        degraded_codes: deduped_codes
            .iter()
            .map(|code| (*code).to_string())
            .collect(),
    })
}

fn sorted_unique_strs<'a>(values: &'a [&'a str]) -> Vec<&'a str> {
    let mut unique = BTreeSet::new();
    for value in values {
        unique.insert(*value);
    }
    unique.into_iter().collect()
}

pub fn append_workload_trace(
    options: &FlightRecorderStorageOptions,
    trace: &AgentWorkloadTrace,
) -> Result<FlightRecorderAppendReport, FlightRecorderError> {
    let trace_path = options.trace_path();
    reject_mesh_approval_bearer("trace path", &trace_path.display().to_string())?;
    ensure_trace_append_path_safe(&trace_path)?;
    let mut line = serde_json::to_string(trace).map_err(|error| FlightRecorderError::Io {
        path: trace_path.clone(),
        message: error.to_string(),
    })?;
    reject_mesh_approval_bearer("trace row", &line)?;
    line.push('\n');
    fs::create_dir_all(&options.directory).map_err(|error| FlightRecorderError::Io {
        path: options.directory.clone(),
        message: error.to_string(),
    })?;
    ensure_trace_append_path_safe(&trace_path)?;
    let existing_bytes = match fs::symlink_metadata(&trace_path) {
        Ok(metadata) => metadata.len(),
        Err(error) if error.kind() == io::ErrorKind::NotFound => 0,
        Err(error) => {
            return Err(FlightRecorderError::Io {
                path: trace_path,
                message: error.to_string(),
            });
        }
    };
    let bytes_written = u64::try_from(line.len()).unwrap_or(u64::MAX);
    let projected_bytes = existing_bytes.saturating_add(bytes_written);
    if options.max_bytes > 0 && projected_bytes > options.max_bytes {
        return Err(FlightRecorderError::QuotaExceeded {
            projected_bytes,
            max_bytes: options.max_bytes,
        });
    }
    let mut file = open_flight_recorder_append_file(&trace_path)?;
    file.write_all(line.as_bytes())
        .map_err(|error| FlightRecorderError::Io {
            path: trace_path.clone(),
            message: error.to_string(),
        })?;
    Ok(FlightRecorderAppendReport {
        schema: "ee.flight_recorder.append.v1",
        side_effect_free: false,
        trace_id: trace.trace_id.clone(),
        trace_path: trace_path.display().to_string(),
        bytes_written,
        retained_bytes: projected_bytes,
        retention_days: options.retention_days,
        max_bytes: options.max_bytes,
        redaction_level: trace.redaction_level,
    })
}

fn open_flight_recorder_append_file(path: &Path) -> Result<fs::File, FlightRecorderError> {
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    configure_flight_recorder_append_options(&mut options);
    options.open(path).map_err(|error| FlightRecorderError::Io {
        path: path.to_path_buf(),
        message: format!("failed to open flight-recorder trace append file: {error}"),
    })
}

#[cfg(all(unix, not(any(target_os = "espidf", target_os = "horizon"))))]
fn configure_flight_recorder_append_options(options: &mut OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt;

    options.custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32);
    options.mode(0o600);
}

#[cfg(not(all(unix, not(any(target_os = "espidf", target_os = "horizon")))))]
fn configure_flight_recorder_append_options(_options: &mut OpenOptions) {}

fn ensure_trace_append_path_safe(path: &Path) -> Result<(), FlightRecorderError> {
    if let Some(symlink_path) = first_existing_symlink_component(path)? {
        return Err(FlightRecorderError::Io {
            path: path.to_path_buf(),
            message: format!(
                "refusing to append flight-recorder trace to '{}': path traverses symbolic link '{}'",
                path.display(),
                symlink_path.display()
            ),
        });
    }
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => Ok(()),
        Ok(_) => Err(FlightRecorderError::Io {
            path: path.to_path_buf(),
            message: format!(
                "refusing to append flight-recorder trace to '{}': path is not a regular file",
                path.display()
            ),
        }),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(FlightRecorderError::Io {
            path: path.to_path_buf(),
            message: format!(
                "failed to inspect flight-recorder append path '{}': {error}",
                path.display()
            ),
        }),
    }
}

fn first_existing_symlink_component(path: &Path) -> Result<Option<PathBuf>, FlightRecorderError> {
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        #[cfg(windows)]
        if matches!(
            component,
            std::path::Component::Prefix(_) | std::path::Component::RootDir
        ) {
            continue;
        }
        #[cfg(not(windows))]
        if matches!(component, std::path::Component::RootDir) {
            continue;
        }
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => return Ok(Some(current)),
            Ok(_) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
                ) =>
            {
                return Ok(None);
            }
            Err(error) => {
                return Err(FlightRecorderError::Io {
                    path: current.clone(),
                    message: format!(
                        "failed to inspect flight-recorder append path component '{}': {error}",
                        current.display()
                    ),
                });
            }
        }
    }
    Ok(None)
}

pub fn replay_workload_trace(
    trace_path: &Path,
) -> Result<FlightRecorderReplayReport, FlightRecorderError> {
    replay_workload_trace_with_max_bytes(trace_path, DEFAULT_FLIGHT_RECORDER_REPLAY_MAX_BYTES)
}

fn replay_workload_trace_with_max_bytes(
    trace_path: &Path,
    max_bytes: u64,
) -> Result<FlightRecorderReplayReport, FlightRecorderError> {
    reject_mesh_approval_bearer("trace path", &trace_path.display().to_string())?;
    let body = read_trace_body(trace_path, max_bytes)?;
    let trace_hash = format!("blake3:{}", blake3::hash(body.as_bytes()).to_hex());
    let mut command_counts = BTreeMap::<String, usize>::new();
    let mut degraded_code_counts = BTreeMap::<String, usize>::new();
    let mut response_bytes_total = 0_u64;
    let mut response_token_estimate_total = 0_u64;
    let mut memory_reference_count = 0_usize;
    let mut row_count = 0_usize;
    let mut malformed_lines = 0_usize;

    for (index, line) in body.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        reject_mesh_approval_bearer("trace replay row", trimmed)?;
        match serde_json::from_str::<AgentWorkloadTrace>(trimmed) {
            Ok(row) => {
                // Scan the decoded representation as well as the raw JSONL
                // bytes above. JSON escapes can otherwise conceal a bearer
                // prefix until serde materializes the string fields.
                let decoded = serde_json::to_string(&row).map_err(|error| {
                    FlightRecorderError::MalformedTrace {
                        line: index + 1,
                        message: error.to_string(),
                    }
                })?;
                reject_mesh_approval_bearer("trace replay row", &decoded)?;
                if row.schema != AGENT_WORKLOAD_TRACE_SCHEMA_V1 {
                    malformed_lines = malformed_lines.saturating_add(1);
                    continue;
                }
                row_count = row_count.saturating_add(1);
                let command_key = command_key(&row.command.verbs);
                *command_counts.entry(command_key).or_default() += 1;
                for code in row.degraded_codes {
                    *degraded_code_counts.entry(code).or_default() += 1;
                }
                response_bytes_total = response_bytes_total.saturating_add(row.response_byte_count);
                response_token_estimate_total = response_token_estimate_total.saturating_add(
                    row.response_token_estimate.unwrap_or_else(|| {
                        row.response_byte_count.saturating_add(3).saturating_div(4)
                    }),
                );
                memory_reference_count =
                    memory_reference_count.saturating_add(row.memory_references.len());
            }
            Err(error) => {
                if row_count == 0 && malformed_lines == 0 {
                    return Err(FlightRecorderError::MalformedTrace {
                        line: index + 1,
                        message: error.to_string(),
                    });
                }
                malformed_lines = malformed_lines.saturating_add(1);
            }
        }
    }

    let command_counts = count_vec(command_counts);
    let degraded_code_counts = count_vec(degraded_code_counts);
    let replay_hash = replay_hash(
        &trace_hash,
        row_count,
        &command_counts,
        &degraded_code_counts,
        response_bytes_total,
        response_token_estimate_total,
        memory_reference_count,
    );

    Ok(FlightRecorderReplayReport {
        schema: "ee.flight_recorder.replay.v1",
        side_effect_free: true,
        trace_path: trace_path.display().to_string(),
        trace_hash,
        row_count,
        malformed_lines,
        command_counts,
        degraded_code_counts,
        response_bytes_total,
        response_token_estimate_total,
        memory_reference_count,
        replay_hash,
    })
}

fn read_trace_body(trace_path: &Path, max_bytes: u64) -> Result<String, FlightRecorderError> {
    ensure_trace_replay_path_safe(trace_path, max_bytes)?;

    let file = open_flight_recorder_read_file(trace_path)?;
    let opened_metadata = file.metadata().map_err(|error| FlightRecorderError::Io {
        path: trace_path.to_path_buf(),
        message: error.to_string(),
    })?;
    if !opened_metadata.file_type().is_file() {
        return Err(FlightRecorderError::Io {
            path: trace_path.to_path_buf(),
            message: format!(
                "refusing to replay flight-recorder trace from '{}': path is not a regular file after open",
                trace_path.display()
            ),
        });
    }
    if max_bytes > 0 && opened_metadata.len() > max_bytes {
        return Err(FlightRecorderError::QuotaExceeded {
            projected_bytes: opened_metadata.len(),
            max_bytes,
        });
    }

    if max_bytes == 0 {
        let mut body = String::new();
        let mut reader = file;
        reader
            .read_to_string(&mut body)
            .map_err(|error| FlightRecorderError::Io {
                path: trace_path.to_path_buf(),
                message: error.to_string(),
            })?;
        return Ok(body);
    }

    let mut bytes = Vec::new();
    let mut limited = file.take(max_bytes.saturating_add(1));
    limited
        .read_to_end(&mut bytes)
        .map_err(|error| FlightRecorderError::Io {
            path: trace_path.to_path_buf(),
            message: error.to_string(),
        })?;
    let bytes_read = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    if bytes_read > max_bytes {
        return Err(FlightRecorderError::QuotaExceeded {
            projected_bytes: bytes_read,
            max_bytes,
        });
    }

    String::from_utf8(bytes).map_err(|error| FlightRecorderError::Io {
        path: trace_path.to_path_buf(),
        message: error.to_string(),
    })
}

fn open_flight_recorder_read_file(path: &Path) -> Result<fs::File, FlightRecorderError> {
    let mut options = OpenOptions::new();
    options.read(true);
    configure_flight_recorder_read_options(&mut options);
    options.open(path).map_err(|error| FlightRecorderError::Io {
        path: path.to_path_buf(),
        message: format!("failed to open flight-recorder trace replay file: {error}"),
    })
}

#[cfg(all(unix, not(any(target_os = "espidf", target_os = "horizon"))))]
fn configure_flight_recorder_read_options(options: &mut OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt;

    options.custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32);
}

#[cfg(not(all(unix, not(any(target_os = "espidf", target_os = "horizon")))))]
fn configure_flight_recorder_read_options(_options: &mut OpenOptions) {}

fn ensure_trace_replay_path_safe(path: &Path, max_bytes: u64) -> Result<(), FlightRecorderError> {
    if let Some(symlink_path) = first_existing_symlink_component(path)? {
        return Err(FlightRecorderError::Io {
            path: path.to_path_buf(),
            message: format!(
                "refusing to replay flight-recorder trace from '{}': path traverses symbolic link '{}'",
                path.display(),
                symlink_path.display()
            ),
        });
    }
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => {
            if max_bytes > 0 && metadata.len() > max_bytes {
                return Err(FlightRecorderError::QuotaExceeded {
                    projected_bytes: metadata.len(),
                    max_bytes,
                });
            }
            Ok(())
        }
        Ok(_) => Err(FlightRecorderError::Io {
            path: path.to_path_buf(),
            message: format!(
                "refusing to replay flight-recorder trace from '{}': path is not a regular file",
                path.display()
            ),
        }),
        Err(error) => Err(FlightRecorderError::Io {
            path: path.to_path_buf(),
            message: format!(
                "failed to inspect flight-recorder replay path '{}': {error}",
                path.display()
            ),
        }),
    }
}

fn validate_inputs(inputs: &FlightRecorderInputs<'_>) -> Result<(), FlightRecorderError> {
    reject_mesh_approval_bearer("recorded_at", inputs.recorded_at_rfc3339)?;
    for verb in inputs.command.verbs {
        reject_mesh_approval_bearer("command.verbs", verb)?;
    }
    for flag in inputs.command.flag_names {
        reject_mesh_approval_bearer("command.flag_names", flag)?;
    }
    if let Some(output_format) = inputs.command.output_format {
        reject_mesh_approval_bearer("command.output_format", output_format)?;
    }
    for hash in inputs.memory_hashes {
        reject_mesh_approval_bearer("memory_hashes", hash)?;
    }
    if let Some(model_family) = inputs.harness_model_family {
        reject_mesh_approval_bearer("harness_model_family", model_family)?;
    }
    for code in inputs.degraded_codes {
        reject_mesh_approval_bearer("degraded_codes", code)?;
    }

    if inputs.command.verbs.is_empty()
        || !inputs.command.verbs.iter().all(|verb| is_verb_token(verb))
    {
        return Err(FlightRecorderError::InvalidVerbChain {
            verbs: inputs
                .command
                .verbs
                .iter()
                .map(|v| (*v).to_string())
                .collect(),
        });
    }
    for flag in inputs.command.flag_names {
        if !is_flag_name(flag) {
            return Err(FlightRecorderError::InvalidFlagName {
                flag: (*flag).to_string(),
            });
        }
    }
    if let Some(output_format) = inputs.command.output_format
        && !is_output_format(output_format)
    {
        return Err(FlightRecorderError::InvalidOutputFormat {
            value: output_format.to_string(),
        });
    }
    for hash in inputs.memory_hashes {
        if !is_memory_hash(hash) {
            return Err(FlightRecorderError::InvalidMemoryHash {
                value: (*hash).to_string(),
            });
        }
    }
    if let Some(model_family) = inputs.harness_model_family
        && !is_model_family(model_family)
    {
        return Err(FlightRecorderError::InvalidModelFamily {
            value: model_family.to_string(),
        });
    }
    if !is_rfc3339_timestamp(inputs.recorded_at_rfc3339) {
        return Err(FlightRecorderError::InvalidRecordedAt {
            value: inputs.recorded_at_rfc3339.to_string(),
        });
    }
    for code in inputs.degraded_codes {
        if !is_degraded_code(code) {
            return Err(FlightRecorderError::InvalidDegradedCode {
                code: (*code).to_string(),
            });
        }
    }
    Ok(())
}

fn reject_mesh_approval_bearer(
    field: &'static str,
    value: &str,
) -> Result<(), FlightRecorderError> {
    let redaction = crate::policy::redact_secret_like_content(value);
    if redaction
        .redacted_reasons
        .iter()
        .any(|reason| *reason == "mesh_approval_token")
    {
        return Err(FlightRecorderError::SensitiveInput { field });
    }
    Ok(())
}

fn is_verb_token(value: &str) -> bool {
    let mut chars = value.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn is_flag_name(value: &str) -> bool {
    let Some(stripped) = value.strip_prefix("--") else {
        return false;
    };
    is_verb_token(stripped)
}

fn is_output_format(value: &str) -> bool {
    matches!(
        value,
        "json" | "human" | "markdown" | "toon" | "jsonl" | "compact" | "hook"
    )
}

fn is_memory_hash(value: &str) -> bool {
    let Some(hex) = value.strip_prefix("blake3:") else {
        return false;
    };
    (32..=128).contains(&hex.len()) && hex.chars().all(|c| c.is_ascii_hexdigit())
}

fn is_model_family(value: &str) -> bool {
    matches!(
        value,
        "claude-opus"
            | "claude-sonnet"
            | "claude-haiku"
            | "gpt-5"
            | "gpt-4"
            | "gemini-pro"
            | "other"
            | "unknown"
    )
}

fn is_degraded_code(value: &str) -> bool {
    let mut chars = value.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

fn is_rfc3339_timestamp(value: &str) -> bool {
    chrono::DateTime::parse_from_rfc3339(value).is_ok()
}

fn command_key(verbs: &[String]) -> String {
    if verbs.is_empty() {
        "unknown".to_string()
    } else {
        verbs.join(" ")
    }
}

fn count_vec(counts: BTreeMap<String, usize>) -> Vec<FlightRecorderCount> {
    counts
        .into_iter()
        .map(|(key, count)| FlightRecorderCount { key, count })
        .collect()
}

fn replay_hash(
    trace_hash: &str,
    row_count: usize,
    command_counts: &[FlightRecorderCount],
    degraded_code_counts: &[FlightRecorderCount],
    response_bytes_total: u64,
    response_token_estimate_total: u64,
    memory_reference_count: usize,
) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"ee.flight_recorder.replay.v1");
    hasher.update(trace_hash.as_bytes());
    hasher.update(&usize_to_u64(row_count).to_le_bytes());
    for count in command_counts {
        hasher.update(count.key.as_bytes());
        hasher.update(&usize_to_u64(count.count).to_le_bytes());
    }
    for count in degraded_code_counts {
        hasher.update(count.key.as_bytes());
        hasher.update(&usize_to_u64(count.count).to_le_bytes());
    }
    hasher.update(&response_bytes_total.to_le_bytes());
    hasher.update(&response_token_estimate_total.to_le_bytes());
    hasher.update(&usize_to_u64(memory_reference_count).to_le_bytes());
    format!("blake3:{}", hasher.finalize().to_hex())
}

fn usize_to_u64(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

/// Derive the trace id from the recorder's redaction-safe row fields. Does NOT
/// embed any task / query / memory body content. The result is stable after the
/// same dedupe/sort normalization used for schema `uniqueItems` fields.
fn derive_trace_id(
    inputs: &FlightRecorderInputs<'_>,
    flag_names: &[&str],
    memory_hashes: &[&str],
    degraded_codes: &[&str],
) -> String {
    let mut hasher = blake3::Hasher::new();
    update_str(&mut hasher, "schema", AGENT_WORKLOAD_TRACE_SCHEMA_V1);
    update_str(
        &mut hasher,
        "redaction_level",
        inputs.redaction_level.as_str(),
    );
    update_str(&mut hasher, "recorded_at", inputs.recorded_at_rfc3339);
    update_str_list(&mut hasher, "command.verbs", inputs.command.verbs);
    update_str_list(&mut hasher, "command.flag_names", flag_names);
    update_optional_str(
        &mut hasher,
        "command.output_format",
        inputs.command.output_format,
    );
    hasher.update(b"command.positional_arity\0");
    hasher.update(&inputs.command.positional_arity.to_le_bytes());
    hasher.update(b"exit_code\0");
    hasher.update(&[inputs.exit_code]);
    hasher.update(b"elapsed_ms\0");
    hasher.update(&inputs.elapsed_ms.to_le_bytes());
    hasher.update(b"response_byte_count\0");
    hasher.update(&inputs.response_byte_count.to_le_bytes());
    update_optional_u64(
        &mut hasher,
        "response_token_estimate",
        inputs.response_token_estimate,
    );
    update_optional_str(
        &mut hasher,
        "token_estimator_id",
        inputs.token_estimator_id.map(TokenEstimatorId::as_str),
    );
    update_str(
        &mut hasher,
        "harness_identity.program",
        inputs.harness_program.as_str(),
    );
    update_optional_str(
        &mut hasher,
        "harness_identity.model_family",
        inputs.harness_model_family,
    );
    update_str_list(&mut hasher, "memory_references.hash", memory_hashes);
    update_str_list(&mut hasher, "degraded_codes", degraded_codes);
    let digest = hasher.finalize().to_hex();
    let mut id = String::from("trc_");
    id.push_str(&digest.as_str()[..32]);
    id
}

fn update_str(hasher: &mut blake3::Hasher, label: &str, value: &str) {
    hasher.update(label.as_bytes());
    hasher.update(b"\0");
    hasher.update(value.as_bytes());
    hasher.update(b"\0");
}

fn update_optional_str(hasher: &mut blake3::Hasher, label: &str, value: Option<&str>) {
    hasher.update(label.as_bytes());
    hasher.update(b"\0");
    if let Some(value) = value {
        hasher.update(b"some\0");
        hasher.update(value.as_bytes());
    } else {
        hasher.update(b"none");
    }
    hasher.update(b"\0");
}

fn update_optional_u64(hasher: &mut blake3::Hasher, label: &str, value: Option<u64>) {
    hasher.update(label.as_bytes());
    hasher.update(b"\0");
    if let Some(value) = value {
        hasher.update(b"some\0");
        hasher.update(&value.to_le_bytes());
    } else {
        hasher.update(b"none");
    }
    hasher.update(b"\0");
}

fn update_str_list(hasher: &mut blake3::Hasher, label: &str, values: &[&str]) {
    hasher.update(label.as_bytes());
    hasher.update(b"\0");
    hasher.update(&usize_to_u64(values.len()).to_le_bytes());
    for value in values {
        hasher.update(b"\0");
        hasher.update(value.as_bytes());
    }
    hasher.update(b"\0");
}

/// Posture vocabulary the flight recorder reports to `ee status` / `ee doctor`.
///
/// Pure-policy summary derived from the configured `EE_FLIGHT_RECORDER`,
/// `EE_FLIGHT_RECORDER_DIR`, and `EE_FLIGHT_RECORDER_RETENTION_DAYS`
/// environment overrides plus the directory probe. Decoupled from
/// `status::SubsystemPostureStatus` so this crate can be reused by any
/// renderer without pulling the full status module.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FlightRecorderPosture {
    /// `EE_FLIGHT_RECORDER` is unset or `false`; no traces are being written.
    Disabled,
    /// Enabled, directory writable, retention within bounds.
    Enabled,
    /// Enabled but the configured retention is outside the supported range
    /// (less than one day or greater than thirty).
    RetentionOutOfRange,
    /// Enabled but the configured directory could not be opened for writes.
    DirectoryUnwritable,
    /// Enabled but the configured directory override resolves to a path
    /// underneath the workspace `.git/` tree.
    DirectoryInsideGit,
}

impl FlightRecorderPosture {
    /// Stable lower-snake code for posture reporting.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::Enabled => "enabled",
            Self::RetentionOutOfRange => "retention_out_of_range",
            Self::DirectoryUnwritable => "directory_unwritable",
            Self::DirectoryInsideGit => "directory_inside_git",
        }
    }

    /// Whether the recorder is currently writing traces.
    #[must_use]
    pub const fn is_writing(self) -> bool {
        matches!(self, Self::Enabled)
    }

    /// Stable reason code surfaced to `ee status` and `ee doctor` when the
    /// posture is not the nominal Enabled / Disabled pair.
    #[must_use]
    pub const fn reason_code(self) -> Option<&'static str> {
        match self {
            Self::Disabled | Self::Enabled => None,
            Self::RetentionOutOfRange => Some("flight_recorder_retention_out_of_range"),
            Self::DirectoryUnwritable => Some("flight_recorder_directory_unwritable"),
            Self::DirectoryInsideGit => Some("flight_recorder_directory_inside_git"),
        }
    }

    /// Non-destructive repair command. Always emits an `ee` or `mkdir -p`
    /// invocation — never proposes `rm`, `git clean`, or any other destructive
    /// operation, per AGENTS.md.
    #[must_use]
    pub const fn repair_command(self) -> Option<&'static str> {
        match self {
            Self::Disabled | Self::Enabled => None,
            Self::RetentionOutOfRange => {
                Some("Set EE_FLIGHT_RECORDER_RETENTION_DAYS to a value in [1, 30].")
            }
            Self::DirectoryUnwritable => Some(
                "Ensure EE_FLIGHT_RECORDER_DIR points to a writable directory and re-run the command.",
            ),
            Self::DirectoryInsideGit => Some(
                "Point EE_FLIGHT_RECORDER_DIR at a path outside the workspace .git/ directory.",
            ),
        }
    }
}

/// Pure-policy classifier for the flight recorder posture given the
/// configured override snapshot. Inputs are caller-curated (already
/// resolved from the environment registry and the directory probe);
/// nothing inside this function touches the filesystem.
///
/// `retention_days` is the parsed retention value (defaults to 7 when
/// the override is unset); `directory_writable` is `Some(false)` when
/// the configured override exists but failed an `open(O_WRONLY)` probe,
/// `Some(true)` when the probe succeeded, and `None` when the recorder
/// is disabled (the probe is skipped). `directory_inside_git_tree` is
/// `true` when the configured directory override resolves to a path
/// underneath the workspace `.git/` tree.
#[must_use]
pub const fn classify_flight_recorder_posture(
    enabled: bool,
    retention_days: u32,
    directory_writable: Option<bool>,
    directory_inside_git_tree: bool,
) -> FlightRecorderPosture {
    if !enabled {
        return FlightRecorderPosture::Disabled;
    }
    if directory_inside_git_tree {
        return FlightRecorderPosture::DirectoryInsideGit;
    }
    if matches!(directory_writable, Some(false)) {
        return FlightRecorderPosture::DirectoryUnwritable;
    }
    if retention_days == 0 || retention_days > 30 {
        return FlightRecorderPosture::RetentionOutOfRange;
    }
    FlightRecorderPosture::Enabled
}

#[cfg(test)]
mod tests {
    use super::*;

    fn baseline_inputs<'a>() -> FlightRecorderInputs<'a> {
        FlightRecorderInputs {
            redaction_level: RedactionLevel::Strict,
            recorded_at_rfc3339: "2026-05-20T07:10:00Z",
            command: CommandShapeInput {
                verbs: &["context"],
                positional_arity: 1,
                flag_names: &["--json", "--explain"],
                output_format: Some("json"),
            },
            exit_code: 0,
            elapsed_ms: 142,
            response_byte_count: 8_192,
            response_token_estimate: Some(2_048),
            token_estimator_id: Some(TokenEstimatorId::BytesDiv4),
            harness_program: HarnessProgram::ClaudeCode,
            harness_model_family: Some("claude-opus"),
            memory_hashes: &[
                "blake3:0123456789abcdef0123456789abcdef",
                "blake3:fedcba9876543210fedcba9876543210",
            ],
            degraded_codes: &["context_low_relevance_floor"],
        }
    }

    fn exact_length_mesh_approval_bearer() -> String {
        let mut bearer = ["e", "e", "a", "p", "1", "_"].concat();
        bearer.push_str(
            &"A".repeat(crate::mesh::lane_grant::APPROVAL_TOKEN_BEARER_LEN - bearer.len()),
        );
        assert_eq!(
            bearer.len(),
            crate::mesh::lane_grant::APPROVAL_TOKEN_BEARER_LEN,
            "fixture must exercise the complete production bearer boundary"
        );
        bearer
    }

    #[test]
    fn baseline_record_produces_strict_redaction_trace_with_stable_id() {
        let inputs = baseline_inputs();
        let trace = record_workload(&inputs).expect("baseline inputs validate");
        assert_eq!(trace.schema, AGENT_WORKLOAD_TRACE_SCHEMA_V1);
        assert_eq!(trace.redaction_level, RedactionLevel::Strict);
        assert!(trace.side_effect_free);
        assert!(trace.trace_id.starts_with("trc_"));
        assert_eq!(trace.trace_id.len(), 4 + 32);
        assert_eq!(trace.command.verbs, vec!["context".to_string()]);
        assert_eq!(trace.memory_references.len(), 2);
    }

    #[test]
    fn identical_inputs_produce_byte_identical_traces() {
        let inputs = baseline_inputs();
        let a = record_workload(&inputs).expect("a validates");
        let b = record_workload(&inputs).expect("b validates");
        assert_eq!(a, b);
        let a_json = serde_json::to_string(&a).expect("serialize a");
        let b_json = serde_json::to_string(&b).expect("serialize b");
        assert_eq!(a_json, b_json);
    }

    #[test]
    fn distinct_command_shape_produces_distinct_trace_id() {
        let a = record_workload(&baseline_inputs()).expect("a validates");
        let mut shifted = baseline_inputs();
        shifted.command.verbs = &["search"];
        let b = record_workload(&shifted).expect("b validates");
        assert_ne!(a.trace_id, b.trace_id);
    }

    #[test]
    fn redaction_canary_no_raw_text_substring_present_in_serialized_trace() {
        let inputs = baseline_inputs();
        let trace = record_workload(&inputs).expect("baseline validates");
        let serialized = serde_json::to_string(&trace).expect("serialize trace");
        // Canary inputs: known raw task/query/secret strings the
        // recorder MUST never carry. The struct layout cannot embed
        // these — but we still scan to catch a future field drift.
        for forbidden in [
            "OPENAI_API_KEY",
            "sk-proj-",
            "task description",
            "query text",
            "memory body",
            "mail body",
            "password",
        ] {
            assert!(
                !serialized.contains(forbidden),
                "trace must not surface raw '{forbidden}': {serialized}"
            );
        }
    }

    #[test]
    fn invalid_verb_chain_rejected() {
        let mut inputs = baseline_inputs();
        // Uppercase + spaces / raw text would smuggle in raw task content.
        inputs.command.verbs = &["Context", "EXPLAIN"];
        let err = record_workload(&inputs).expect_err("uppercase verbs rejected");
        assert!(matches!(err, FlightRecorderError::InvalidVerbChain { .. }));
    }

    #[test]
    fn invalid_flag_name_rejected() {
        let mut inputs = baseline_inputs();
        inputs.command.flag_names = &["json"]; // missing -- prefix
        let err = record_workload(&inputs).expect_err("flag without -- prefix rejected");
        assert!(matches!(err, FlightRecorderError::InvalidFlagName { .. }));
    }

    #[test]
    fn invalid_output_format_rejected_before_serialization() {
        let mut inputs = baseline_inputs();
        inputs.command.output_format = Some("json sk-proj-secret");
        let err = record_workload(&inputs).expect_err("raw output format rejected");
        assert!(matches!(
            err,
            FlightRecorderError::InvalidOutputFormat { .. }
        ));
    }

    #[test]
    fn invalid_memory_hash_rejected() {
        let mut inputs = baseline_inputs();
        inputs.memory_hashes = &["mem_01234567"]; // raw memory id, not a blake3 hash
        let err = record_workload(&inputs).expect_err("raw memory id rejected");
        assert!(matches!(err, FlightRecorderError::InvalidMemoryHash { .. }));
    }

    #[test]
    fn invalid_model_family_rejected_before_serialization() {
        let mut inputs = baseline_inputs();
        inputs.harness_model_family = Some("gpt-5 sk-proj-secret");
        let err = record_workload(&inputs).expect_err("raw model family rejected");
        assert!(matches!(
            err,
            FlightRecorderError::InvalidModelFamily { .. }
        ));
    }

    #[test]
    fn invalid_degraded_code_rejected() {
        let mut inputs = baseline_inputs();
        inputs.degraded_codes = &["context_LOW_relevance_floor"]; // uppercase
        let err = record_workload(&inputs).expect_err("uppercase degraded code rejected");
        assert!(matches!(
            err,
            FlightRecorderError::InvalidDegradedCode { .. }
        ));
    }

    #[test]
    fn invalid_recorded_at_timestamp_rejected() {
        let mut inputs = baseline_inputs();
        inputs.recorded_at_rfc3339 = "2026-05-20 07:10:00";
        let err = record_workload(&inputs).expect_err("non-RFC3339 timestamp rejected");
        assert!(matches!(err, FlightRecorderError::InvalidRecordedAt { .. }));
    }

    #[test]
    fn degraded_codes_are_deduped_and_sorted() {
        let mut inputs = baseline_inputs();
        inputs.degraded_codes = &[
            "context_low_relevance_floor",
            "context_pack_truncated",
            "context_low_relevance_floor",
        ];
        let trace = record_workload(&inputs).expect("validates");
        assert_eq!(
            trace.degraded_codes,
            vec![
                "context_low_relevance_floor".to_string(),
                "context_pack_truncated".to_string(),
            ]
        );
    }

    #[test]
    fn flag_names_and_memory_references_are_deduped_for_schema_unique_items() {
        let mut inputs = baseline_inputs();
        inputs.command.flag_names = &["--json", "--explain", "--json"];
        inputs.memory_hashes = &[
            "blake3:0123456789abcdef0123456789abcdef",
            "blake3:fedcba9876543210fedcba9876543210",
            "blake3:0123456789abcdef0123456789abcdef",
        ];
        let trace = record_workload(&inputs).expect("validates");
        assert_eq!(
            trace.command.flag_names,
            vec!["--explain".to_string(), "--json".to_string()]
        );
        assert_eq!(
            trace.memory_references,
            vec![
                MemoryHashRef {
                    hash: "blake3:0123456789abcdef0123456789abcdef".to_string(),
                },
                MemoryHashRef {
                    hash: "blake3:fedcba9876543210fedcba9876543210".to_string(),
                },
            ]
        );

        let baseline = record_workload(&baseline_inputs()).expect("baseline validates");
        assert_eq!(
            trace.trace_id, baseline.trace_id,
            "duplicate-only flags must not perturb the stable trace identity"
        );
    }

    #[test]
    fn trace_id_distinguishes_redaction_safe_row_fields() {
        let baseline = record_workload(&baseline_inputs()).expect("baseline validates");

        let mut changed_memory = baseline_inputs();
        changed_memory.memory_hashes = &["blake3:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"];
        let changed_memory =
            record_workload(&changed_memory).expect("changed memory hash validates");
        assert_ne!(
            changed_memory.trace_id, baseline.trace_id,
            "different selected memory hashes must produce different trace ids"
        );

        let mut changed_degradation = baseline_inputs();
        changed_degradation.degraded_codes = &["context_pack_truncated"];
        let changed_degradation =
            record_workload(&changed_degradation).expect("changed degradation validates");
        assert_ne!(
            changed_degradation.trace_id, baseline.trace_id,
            "different degraded codes must produce different trace ids"
        );

        let mut changed_token_estimate = baseline_inputs();
        changed_token_estimate.response_token_estimate = Some(2_049);
        let changed_token_estimate =
            record_workload(&changed_token_estimate).expect("changed token estimate validates");
        assert_ne!(
            changed_token_estimate.trace_id, baseline.trace_id,
            "different response token estimates must produce different trace ids"
        );

        let mut changed_model_family = baseline_inputs();
        changed_model_family.harness_model_family = Some("claude-sonnet");
        let changed_model_family =
            record_workload(&changed_model_family).expect("changed model family validates");
        assert_ne!(
            changed_model_family.trace_id, baseline.trace_id,
            "different harness model families must produce different trace ids"
        );

        let mut changed_redaction = baseline_inputs();
        changed_redaction.redaction_level = RedactionLevel::Audit;
        let changed_redaction =
            record_workload(&changed_redaction).expect("changed redaction level validates");
        assert_ne!(
            changed_redaction.trace_id, baseline.trace_id,
            "different redaction levels must produce different trace ids"
        );
    }

    #[test]
    fn serialized_trace_uses_camelcase_and_pins_required_fields() {
        let inputs = baseline_inputs();
        let trace = record_workload(&inputs).expect("validates");
        let json = serde_json::to_value(&trace).expect("serialize");
        for required in [
            "schema",
            "sideEffectFree",
            "redactionLevel",
            "traceId",
            "recordedAt",
            "command",
            "exitCode",
            "elapsedMs",
            "responseByteCount",
            "harnessIdentity",
            "memoryReferences",
            "degradedCodes",
        ] {
            assert!(json.get(required).is_some(), "missing field {required}");
        }
        assert_eq!(
            json.get("schema").and_then(|v| v.as_str()),
            Some(AGENT_WORKLOAD_TRACE_SCHEMA_V1)
        );
    }

    #[test]
    fn append_trace_enforces_quota_without_deleting_existing_trace() {
        let temp = tempfile::tempdir().expect("tempdir");
        let options = FlightRecorderStorageOptions {
            directory: temp.path().to_path_buf(),
            retention_days: 7,
            max_bytes: 8,
        };
        let trace = record_workload(&baseline_inputs()).expect("trace");
        let err = append_workload_trace(&options, &trace).expect_err("quota exceeded");
        assert!(matches!(err, FlightRecorderError::QuotaExceeded { .. }));
        assert!(!options.trace_path().exists());
    }

    #[test]
    fn append_and_replay_reject_exact_length_mesh_approval_bearer() {
        let bearer = exact_length_mesh_approval_bearer();
        let degraded_codes = [bearer.as_str()];
        let mut poisoned_inputs = baseline_inputs();
        poisoned_inputs.degraded_codes = &degraded_codes;
        let record_error = record_workload(&poisoned_inputs)
            .expect_err("record boundary must reject an approval bearer");
        assert!(!record_error.to_string().contains(&bearer));
        assert!(matches!(
            record_error,
            FlightRecorderError::SensitiveInput {
                field: "degraded_codes"
            }
        ));

        let temp = tempfile::tempdir().expect("tempdir");
        let options = FlightRecorderStorageOptions {
            directory: temp.path().join("recorder"),
            retention_days: 7,
            max_bytes: 64 * 1024,
        };
        let mut trace = record_workload(&baseline_inputs()).expect("trace");
        trace.harness_identity.model_family = Some(bearer.clone());

        let append_error = append_workload_trace(&options, &trace)
            .expect_err("append boundary must reject an approval bearer");
        assert!(!append_error.to_string().contains(&bearer));
        assert!(matches!(
            append_error,
            FlightRecorderError::SensitiveInput { field: "trace row" }
        ));
        assert!(
            !options.trace_path().exists(),
            "rejected append must not persist credential material"
        );
        assert!(
            !options.directory.exists(),
            "rejected append must not create the recorder directory"
        );

        std::fs::create_dir_all(&options.directory).expect("create replay fixture directory");
        let serialized = serde_json::to_string(&trace).expect("serialize poisoned trace");
        std::fs::write(options.trace_path(), format!("{serialized}\n"))
            .expect("write replay boundary fixture");
        let replay_error = replay_workload_trace(&options.trace_path())
            .expect_err("replay boundary must reject an approval bearer");
        assert!(!replay_error.to_string().contains(&bearer));
        assert!(matches!(
            replay_error,
            FlightRecorderError::SensitiveInput {
                field: "trace replay row"
            }
        ));
    }

    #[test]
    fn replay_rejects_json_escaped_mesh_approval_bearer_after_decode() {
        let bearer = exact_length_mesh_approval_bearer();
        let mut trace = record_workload(&baseline_inputs()).expect("trace");
        trace.degraded_codes = vec![bearer.clone()];
        let serialized = serde_json::to_string(&trace).expect("serialize poisoned trace");
        let escaped_prefix = format!("\\u0065{}", &bearer[1..]);
        let escaped = serialized.replacen(&bearer, &escaped_prefix, 1);
        assert_ne!(escaped, serialized, "fixture must replace the bearer");
        assert!(
            !escaped.contains(&bearer),
            "raw replay fixture must conceal the prefix until JSON decoding"
        );

        let temp = tempfile::tempdir().expect("tempdir");
        let trace_path = temp.path().join("agent-workload-trace.jsonl");
        std::fs::write(&trace_path, format!("{escaped}\n")).expect("write escaped replay fixture");

        let error = replay_workload_trace(&trace_path)
            .expect_err("decoded replay boundary must reject an approval bearer");
        assert!(!error.to_string().contains(&bearer));
        assert!(matches!(
            error,
            FlightRecorderError::SensitiveInput {
                field: "trace replay row"
            }
        ));
    }

    #[test]
    fn append_and_replay_reject_mesh_approval_bearers_in_trace_paths() {
        let bearer = exact_length_mesh_approval_bearer();
        let temp = tempfile::tempdir().expect("tempdir");
        let options = FlightRecorderStorageOptions {
            directory: temp.path().join(&bearer),
            retention_days: 7,
            max_bytes: 64 * 1024,
        };
        let trace = record_workload(&baseline_inputs()).expect("trace");

        let append_error = append_workload_trace(&options, &trace)
            .expect_err("append must reject a bearer-bearing output path");
        assert!(!append_error.to_string().contains(&bearer));
        assert!(matches!(
            append_error,
            FlightRecorderError::SensitiveInput {
                field: "trace path"
            }
        ));
        assert!(
            !options.directory.exists(),
            "rejected bearer-bearing trace path must not create directories"
        );

        let replay_error = replay_workload_trace(&options.trace_path())
            .expect_err("replay must reject a bearer-bearing input path");
        assert!(!replay_error.to_string().contains(&bearer));
        assert!(matches!(
            replay_error,
            FlightRecorderError::SensitiveInput {
                field: "trace path"
            }
        ));
    }

    #[cfg(unix)]
    #[test]
    fn append_trace_rejects_symlinked_directory_component() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().expect("tempdir");
        let real_dir = temp.path().join("real-recorder");
        std::fs::create_dir_all(&real_dir).expect("real recorder dir");
        let linked_dir = temp.path().join("obs");
        symlink(&real_dir, &linked_dir).expect("symlink recorder parent");
        let options = FlightRecorderStorageOptions {
            directory: linked_dir.join("flight_recorder"),
            retention_days: 7,
            max_bytes: 64 * 1024,
        };
        let trace = record_workload(&baseline_inputs()).expect("trace");

        let err = append_workload_trace(&options, &trace)
            .expect_err("symlinked recorder parent rejected");
        assert!(
            matches!(err, FlightRecorderError::Io { ref message, .. } if message.contains("path traverses symbolic link")),
            "unexpected error: {err}"
        );
        assert!(
            !real_dir.join("flight_recorder").exists(),
            "append must not create directories through a symlinked parent"
        );
    }

    #[cfg(unix)]
    #[test]
    fn append_trace_rejects_symlinked_trace_file() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().expect("tempdir");
        let trace_dir = temp.path().join("flight_recorder");
        std::fs::create_dir_all(&trace_dir).expect("trace dir");
        let outside_trace = temp.path().join("outside.jsonl");
        std::fs::write(&outside_trace, "outside\n").expect("outside trace");
        let linked_trace = trace_dir.join("agent-workload-trace.jsonl");
        symlink(&outside_trace, &linked_trace).expect("symlink trace file");
        let options = FlightRecorderStorageOptions {
            directory: trace_dir,
            retention_days: 7,
            max_bytes: 64 * 1024,
        };
        let trace = record_workload(&baseline_inputs()).expect("trace");

        let err =
            append_workload_trace(&options, &trace).expect_err("symlinked trace file rejected");
        assert!(
            matches!(err, FlightRecorderError::Io { ref message, .. } if message.contains("path traverses symbolic link")),
            "unexpected error: {err}"
        );
        assert_eq!(
            std::fs::read_to_string(&outside_trace).expect("outside trace content"),
            "outside\n",
            "append must not write through a symlinked trace file"
        );
    }

    #[test]
    fn replay_trace_enforces_max_bytes_before_parsing() {
        let temp = tempfile::tempdir().expect("tempdir");
        let trace_path = temp.path().join("agent-workload-trace.jsonl");
        std::fs::write(&trace_path, b"{\"schema\":\"not-jsonl-yet\"")
            .expect("oversized malformed trace");
        let err = replay_workload_trace_with_max_bytes(&trace_path, 8).expect_err("quota exceeded");
        assert!(matches!(
            err,
            FlightRecorderError::QuotaExceeded {
                projected_bytes,
                max_bytes: 8
            } if projected_bytes > 8
        ));
    }

    #[test]
    fn replay_trace_rejects_non_regular_path() {
        let temp = tempfile::tempdir().expect("tempdir");
        let trace_path = temp.path().join("agent-workload-trace.jsonl");
        std::fs::create_dir_all(&trace_path).expect("trace directory");
        let err = replay_workload_trace(&trace_path).expect_err("directory rejected");
        assert!(
            matches!(err, FlightRecorderError::Io { ref message, .. } if message.contains("not a regular file")),
            "unexpected error: {err}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn replay_trace_rejects_symlinked_trace_file() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().expect("tempdir");
        let outside_trace = temp.path().join("outside.jsonl");
        std::fs::write(&outside_trace, "{\"schema\":\"ignored\"}\n").expect("outside trace");
        let linked_trace = temp.path().join("agent-workload-trace.jsonl");
        symlink(&outside_trace, &linked_trace).expect("symlink trace file");

        let err = replay_workload_trace(&linked_trace).expect_err("symlinked replay rejected");
        assert!(
            matches!(err, FlightRecorderError::Io { ref message, .. } if message.contains("path traverses symbolic link")),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn append_and_replay_trace_is_deterministic_and_redacted() {
        let temp = tempfile::tempdir().expect("tempdir");
        let options = FlightRecorderStorageOptions {
            directory: temp.path().to_path_buf(),
            retention_days: 7,
            max_bytes: 64 * 1024,
        };
        let trace = record_workload(&baseline_inputs()).expect("trace");
        let append = append_workload_trace(&options, &trace).expect("append");
        assert_eq!(append.trace_id, trace.trace_id);
        let first = replay_workload_trace(&options.trace_path()).expect("first replay");
        let second = replay_workload_trace(&options.trace_path()).expect("second replay");
        assert_eq!(first, second);
        assert_eq!(first.row_count, 1);
        assert_eq!(first.memory_reference_count, 2);
        let stored = std::fs::read_to_string(options.trace_path()).expect("stored trace");
        for forbidden in ["sk-proj-", "query text", "memory body", "mail body"] {
            assert!(
                !stored.contains(forbidden),
                "stored trace leaked {forbidden}"
            );
        }
    }

    #[test]
    fn posture_disabled_when_recorder_off_regardless_of_other_inputs() {
        assert_eq!(
            classify_flight_recorder_posture(false, 7, Some(true), false),
            FlightRecorderPosture::Disabled
        );
        assert_eq!(
            classify_flight_recorder_posture(false, 99, Some(false), true),
            FlightRecorderPosture::Disabled,
        );
    }

    #[test]
    fn posture_enabled_when_writable_and_retention_in_range() {
        assert_eq!(
            classify_flight_recorder_posture(true, 7, Some(true), false),
            FlightRecorderPosture::Enabled
        );
        assert_eq!(
            classify_flight_recorder_posture(true, 1, None, false),
            FlightRecorderPosture::Enabled
        );
        assert_eq!(
            classify_flight_recorder_posture(true, 30, Some(true), false),
            FlightRecorderPosture::Enabled
        );
    }

    #[test]
    fn posture_flags_retention_out_of_range_for_zero_and_above_thirty() {
        assert_eq!(
            classify_flight_recorder_posture(true, 0, Some(true), false),
            FlightRecorderPosture::RetentionOutOfRange
        );
        assert_eq!(
            classify_flight_recorder_posture(true, 31, Some(true), false),
            FlightRecorderPosture::RetentionOutOfRange
        );
    }

    #[test]
    fn posture_flags_unwritable_directory_before_retention_check() {
        assert_eq!(
            classify_flight_recorder_posture(true, 999, Some(false), false),
            FlightRecorderPosture::DirectoryUnwritable
        );
    }

    #[test]
    fn posture_flags_directory_inside_git_tree_with_highest_priority() {
        assert_eq!(
            classify_flight_recorder_posture(true, 7, Some(true), true),
            FlightRecorderPosture::DirectoryInsideGit
        );
        assert_eq!(
            classify_flight_recorder_posture(true, 0, Some(false), true),
            FlightRecorderPosture::DirectoryInsideGit
        );
    }

    #[test]
    fn posture_reason_and_repair_are_non_destructive_for_every_variant() {
        let variants = [
            FlightRecorderPosture::Disabled,
            FlightRecorderPosture::Enabled,
            FlightRecorderPosture::RetentionOutOfRange,
            FlightRecorderPosture::DirectoryUnwritable,
            FlightRecorderPosture::DirectoryInsideGit,
        ];
        let forbidden_tokens = [
            "rm ",
            "rm -",
            "git reset",
            "git clean",
            "git checkout --",
            "git restore --staged",
            "git branch -D",
            "--force",
            "--hard",
            "drop table",
            "truncate ",
            "delete from",
        ];
        for variant in variants {
            assert!(!variant.as_str().is_empty(), "posture stable code");
            for text in [variant.reason_code(), variant.repair_command()]
                .into_iter()
                .flatten()
            {
                let lowered = text.to_ascii_lowercase();
                for token in &forbidden_tokens {
                    assert!(
                        !lowered.contains(token),
                        "posture {:?} text {:?} contains destructive token {:?}",
                        variant,
                        text,
                        token
                    );
                }
            }
        }
    }

    #[test]
    fn posture_writing_predicate_matches_enabled_only() {
        assert!(FlightRecorderPosture::Enabled.is_writing());
        for non_writing in [
            FlightRecorderPosture::Disabled,
            FlightRecorderPosture::RetentionOutOfRange,
            FlightRecorderPosture::DirectoryUnwritable,
            FlightRecorderPosture::DirectoryInsideGit,
        ] {
            assert!(
                !non_writing.is_writing(),
                "{:?} must not report writing",
                non_writing
            );
        }
    }
}
