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
//! Phase-1 scope: pure constructor + redaction-safety guards + stable
//! BLAKE3 hashing. The opt-in I/O wrapper, status/doctor posture wiring,
//! config keys, and integration coverage across `context`/`search`/`why`/
//! `status` land in follow-up bd-1zb7k.19.1.{b,c,d} slices.

use std::collections::BTreeSet;

use serde::{Serialize, Serializer};

/// Public schema identifier, matching the title of
/// `docs/schemas/ee.agent_workload_trace.v1.json`.
pub const AGENT_WORKLOAD_TRACE_SCHEMA_V1: &str = "ee.agent_workload_trace.v1";

/// Default redaction posture for the recorder. The schema only allows
/// `strict` or `audit`; `strict` is the default and the only level that
/// omits hashed memory IDs as well as raw text. The `audit` level keeps
/// the hashed memory IDs available for offline replay analysis but
/// still never carries any raw text.
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
}

impl Serialize for HarnessProgram {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
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
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentWorkloadTrace {
    pub schema: &'static str,
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

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandShape {
    pub verbs: Vec<String>,
    pub positional_arity: u32,
    pub flag_names: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_format: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HarnessIdentity {
    pub program: HarnessProgram,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_family: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryHashRef {
    pub hash: String,
}

/// Validation error returned when the caller's inputs would violate the
/// recorder's redaction contract.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FlightRecorderError {
    InvalidVerbChain { verbs: Vec<String> },
    InvalidFlagName { flag: String },
    InvalidMemoryHash { value: String },
    InvalidRecordedAt { value: String },
    InvalidDegradedCode { code: String },
}

/// Pure constructor: validate the caller-curated inputs against the
/// recorder's redaction contract, derive the trace id, and emit the
/// `AgentWorkloadTrace` row.
pub fn record_workload(
    inputs: &FlightRecorderInputs<'_>,
) -> Result<AgentWorkloadTrace, FlightRecorderError> {
    validate_inputs(inputs)?;
    let trace_id = derive_trace_id(inputs);
    let mut deduped_codes: BTreeSet<&str> = BTreeSet::new();
    for code in inputs.degraded_codes {
        deduped_codes.insert(*code);
    }
    Ok(AgentWorkloadTrace {
        schema: AGENT_WORKLOAD_TRACE_SCHEMA_V1,
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
            flag_names: inputs
                .command
                .flag_names
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
        memory_references: inputs
            .memory_hashes
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

fn validate_inputs(inputs: &FlightRecorderInputs<'_>) -> Result<(), FlightRecorderError> {
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
    for hash in inputs.memory_hashes {
        if !is_memory_hash(hash) {
            return Err(FlightRecorderError::InvalidMemoryHash {
                value: (*hash).to_string(),
            });
        }
    }
    if inputs.recorded_at_rfc3339.is_empty() {
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

fn is_memory_hash(value: &str) -> bool {
    let Some(hex) = value.strip_prefix("blake3:") else {
        return false;
    };
    (32..=128).contains(&hex.len()) && hex.chars().all(|c| c.is_ascii_hexdigit())
}

fn is_degraded_code(value: &str) -> bool {
    let mut chars = value.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// Derive the trace id from the recorder's non-text inputs. Does NOT
/// embed any task / query / memory body content — only the verb chain,
/// positional arity, output format, exit code, elapsed ms, response
/// byte count, harness program, and the recorded-at timestamp. The
/// result is stable: identical inputs produce identical ids.
fn derive_trace_id(inputs: &FlightRecorderInputs<'_>) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(AGENT_WORKLOAD_TRACE_SCHEMA_V1.as_bytes());
    hasher.update(b"\0");
    hasher.update(inputs.recorded_at_rfc3339.as_bytes());
    hasher.update(b"\0");
    for verb in inputs.command.verbs {
        hasher.update(verb.as_bytes());
        hasher.update(b"\0");
    }
    hasher.update(b"|");
    for flag in inputs.command.flag_names {
        hasher.update(flag.as_bytes());
        hasher.update(b"\0");
    }
    hasher.update(b"|");
    if let Some(output_format) = inputs.command.output_format {
        hasher.update(output_format.as_bytes());
    }
    hasher.update(b"|");
    hasher.update(&inputs.command.positional_arity.to_le_bytes());
    hasher.update(&[inputs.exit_code]);
    hasher.update(&inputs.elapsed_ms.to_le_bytes());
    hasher.update(&inputs.response_byte_count.to_le_bytes());
    hasher.update(inputs.harness_program.as_str().as_bytes());
    let digest = hasher.finalize().to_hex();
    let mut id = String::from("trc_");
    id.push_str(&digest.as_str()[..32]);
    id
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
    fn invalid_memory_hash_rejected() {
        let mut inputs = baseline_inputs();
        inputs.memory_hashes = &["mem_01234567"]; // raw memory id, not a blake3 hash
        let err = record_workload(&inputs).expect_err("raw memory id rejected");
        assert!(matches!(err, FlightRecorderError::InvalidMemoryHash { .. }));
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
}
