//! Verification evidence records for build/test/provenance gates.
//!
//! The verification ledger models what happened during a gate without treating
//! "a command ran" as proof. In particular, remote-required Cargo gates that
//! fall back to local execution are explicit non-passing evidence.

use std::error::Error;
use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

use crate::models::{ProducerMetadata, ProducerSourceSystem};

pub const VERIFICATION_EVIDENCE_SCHEMA_V1: &str = "ee.verification.evidence.v1";
pub const VERIFICATION_CLOSURE_GUIDANCE_SCHEMA_V1: &str = "ee.verification.closure_guidance.v1";
pub const VERIFICATION_RUN_SCHEMA_V1: &str = "ee.verification.run.v1";
pub const VERIFICATION_REUSE_ADVISORY_SCHEMA_V1: &str = "ee.verification.reuse_advisory.v1";
pub const VERIFICATION_BROKER_VIEW_SCHEMA_V1: &str = "ee.verification.broker_view.v1";
pub const PROOF_BROKER_SCHEMA_V1: &str = "ee.proof_broker.v1";
pub const VERIFICATION_CLOSEOUT_CAPSULE_SCHEMA_V1: &str = "ee.verification.closeout_capsule.v1";
pub const VERIFICATION_COMPILE_BLOCKER_CACHE_SCHEMA_V1: &str =
    "ee.verification.compile_blocker_cache.v1";
pub const VERIFICATION_COMPILE_BLOCKER_LOOKUP_SCHEMA_V1: &str =
    "ee.verification.compile_blocker_lookup.v1";
pub const RCH_VERIFY_SCHEMA_V1: &str = "ee.rch.verify.v1";
pub const RCH_SELECTOR_ADMISSION_PROBE_SCHEMA_V1: &str = "ee.rch.selector_admission_probe.v1";
pub const GITHUB_ACTIONS_CHECK_RUN_SCHEMA_V1: &str = "ee.github_actions.check_run.v1";

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationStatus {
    Passed,
    Failed,
    Blocked,
    Interrupted,
    FallbackDetected,
    Unknown,
}

impl VerificationStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Passed => "passed",
            Self::Failed => "failed",
            Self::Blocked => "blocked",
            Self::Interrupted => "interrupted",
            Self::FallbackDetected => "fallback_detected",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VerificationOffload {
    pub required_remote: bool,
    pub remote_required_env: Option<String>,
    pub offload_tool: Option<String>,
    pub worker: Option<String>,
    pub fallback_detected: bool,
    pub fallback_reason: Option<String>,
}

impl VerificationOffload {
    #[must_use]
    pub fn local() -> Self {
        Self {
            required_remote: false,
            remote_required_env: None,
            offload_tool: None,
            worker: None,
            fallback_detected: false,
            fallback_reason: None,
        }
    }

    #[must_use]
    pub fn rch_required(worker: Option<&str>) -> Self {
        Self {
            required_remote: true,
            remote_required_env: Some("RCH_REQUIRE_REMOTE=1".to_owned()),
            offload_tool: Some("rch".to_owned()),
            worker: normalized_non_empty(worker),
            fallback_detected: false,
            fallback_reason: None,
        }
    }

    #[must_use]
    pub fn rch_fallback(worker: Option<&str>, reason: Option<&str>) -> Self {
        Self {
            required_remote: true,
            remote_required_env: Some("RCH_REQUIRE_REMOTE=1".to_owned()),
            offload_tool: Some("rch".to_owned()),
            worker: normalized_non_empty(worker),
            fallback_detected: true,
            fallback_reason: normalized_non_empty(reason),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VerificationSelectorAdmission {
    pub status: Option<String>,
    pub required_runtime: Option<String>,
    pub workers_reported: Vec<String>,
    pub daemon_workers_reported: Vec<String>,
    pub selected_worker: Option<String>,
    pub selection_failure_reason: Option<String>,
    pub workers_vs_selection_contradiction: bool,
    pub path_normalization_warning: Option<String>,
    pub remote_required: bool,
    pub local_fallback_refused: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VerificationEnvironment {
    pub workspace_fingerprint: Option<String>,
    pub cwd: Option<String>,
    pub toolchain: Option<String>,
}

impl VerificationEnvironment {
    #[must_use]
    pub fn new(
        workspace_fingerprint: Option<&str>,
        cwd: Option<&str>,
        toolchain: Option<&str>,
    ) -> Self {
        Self {
            workspace_fingerprint: normalized_non_empty(workspace_fingerprint),
            cwd: normalized_non_empty(cwd),
            toolchain: normalized_non_empty(toolchain),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VerificationOutputSummary {
    pub stdout_tail: Option<String>,
    pub stderr_tail: Option<String>,
    pub redacted: bool,
}

impl VerificationOutputSummary {
    #[must_use]
    pub fn empty() -> Self {
        Self {
            stdout_tail: None,
            stderr_tail: None,
            redacted: false,
        }
    }

    #[must_use]
    pub fn redacted(stderr_tail: Option<&str>) -> Self {
        Self {
            stdout_tail: None,
            stderr_tail: normalized_non_empty(stderr_tail),
            redacted: true,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VerificationArtifactRef {
    pub path: String,
    pub kind: String,
    pub content_hash: Option<String>,
}

impl VerificationArtifactRef {
    #[must_use]
    pub fn new(path: &str, kind: &str, content_hash: Option<&str>) -> Self {
        Self {
            path: path.to_owned(),
            kind: kind.to_owned(),
            content_hash: normalized_non_empty(content_hash),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VerificationEvidenceRecord {
    pub schema: String,
    pub verification_id: String,
    pub bead_id: Option<String>,
    pub gate_name: String,
    pub command: String,
    pub command_hash: String,
    pub status: VerificationStatus,
    pub exit_code: Option<i32>,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub duration_ms: Option<u64>,
    pub environment: VerificationEnvironment,
    pub offload: VerificationOffload,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selector_admission: Option<VerificationSelectorAdmission>,
    pub output_summary: VerificationOutputSummary,
    pub artifacts: Vec<VerificationArtifactRef>,
    pub producer: ProducerMetadata,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VerificationGateRequirement {
    pub gate_name: String,
    pub command_contains: Option<String>,
    pub requires_remote: bool,
}

impl VerificationGateRequirement {
    #[must_use]
    pub fn new(gate_name: &str, command_contains: Option<&str>, requires_remote: bool) -> Self {
        Self {
            gate_name: gate_name.to_owned(),
            command_contains: normalized_non_empty(command_contains),
            requires_remote,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VerificationGateAssessment {
    pub gate_name: String,
    pub command_contains: Option<String>,
    pub requires_remote: bool,
    pub satisfied: bool,
    pub matched_verification_id: Option<String>,
    pub matched_status: Option<VerificationStatus>,
    pub reason: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VerificationClosureGuidance {
    pub schema: String,
    pub bead_id: Option<String>,
    pub can_close: bool,
    pub assessments: Vec<VerificationGateAssessment>,
    pub rejected_reasons: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VerificationRunProvenance {
    pub source: String,
    pub event_kind: String,
    pub line: Option<usize>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VerificationRunRecord {
    pub schema: String,
    pub run_id: String,
    pub bead_id: Option<String>,
    pub agent_name: Option<String>,
    pub source_hash: Option<String>,
    pub command_hash: String,
    pub command_argv_hash: String,
    pub cargo_target_dir_hash_or_class: Option<String>,
    pub execution_substrate: String,
    pub worker_host: Option<String>,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub exit_code: Option<i32>,
    pub stdout_hash: Option<String>,
    pub stderr_excerpt_hash: Option<String>,
    pub artifact_manifest_hash: Option<String>,
    pub retained_log_path_hash: Option<String>,
    pub provenance: Vec<VerificationRunProvenance>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationReuseStatus {
    ReusablePass,
    ReusableFail,
    InFlight,
    StaleSource,
    MismatchedCommand,
    MissingEvidence,
    RerunRequired,
}

impl VerificationReuseStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ReusablePass => "reusable_pass",
            Self::ReusableFail => "reusable_fail",
            Self::InFlight => "in_flight",
            Self::StaleSource => "stale_source",
            Self::MismatchedCommand => "mismatched_command",
            Self::MissingEvidence => "missing_evidence",
            Self::RerunRequired => "rerun_required",
        }
    }
}

#[derive(Clone, Debug)]
pub struct VerificationReuseRequest<'a> {
    pub bead_id: Option<&'a str>,
    pub source_hash: Option<&'a str>,
    pub command_hash: &'a str,
    pub execution_substrate: &'a str,
    pub feature_profile_hash: Option<&'a str>,
    pub workspace_generation: Option<u64>,
    pub strictness_flags: Vec<&'a str>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VerificationReuseRepairAction {
    pub priority: u8,
    pub kind: String,
    pub command: Option<String>,
    pub message: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VerificationReuseAdvisory {
    pub schema: String,
    pub bead_id: Option<String>,
    pub status: VerificationReuseStatus,
    pub requested_source_hash: Option<String>,
    pub requested_command_hash: String,
    pub requested_execution_substrate: String,
    pub feature_profile_hash: Option<String>,
    pub workspace_generation: Option<u64>,
    pub strictness_flags: Vec<String>,
    pub matched_run_id: Option<String>,
    pub matched_agent_name: Option<String>,
    pub matched_finished_at: Option<String>,
    pub reason: String,
    pub repair_actions: Vec<VerificationReuseRepairAction>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationBrokerStatus {
    Reusable,
    Stale,
    Incompatible,
    InProgress,
    KnownBlocker,
    Unavailable,
}

impl VerificationBrokerStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Reusable => "reusable",
            Self::Stale => "stale",
            Self::Incompatible => "incompatible",
            Self::InProgress => "in_progress",
            Self::KnownBlocker => "known_blocker",
            Self::Unavailable => "unavailable",
        }
    }
}

#[derive(Clone, Debug)]
pub struct VerificationBrokerViewRequest<'a> {
    pub bead_id: Option<&'a str>,
    pub source_hash: Option<&'a str>,
    pub command_hash: &'a str,
    pub command_class: &'a str,
    pub normalized_argv_hash: &'a str,
    pub execution_substrate: &'a str,
    pub env_fingerprint_class: Option<&'a str>,
    pub target_profile: Option<&'a str>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VerificationBrokerRchMetadata {
    pub required_remote: bool,
    pub worker_host: Option<String>,
    pub job_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VerificationFirstFailureSummaryRef {
    pub stderr_excerpt_hash: Option<String>,
    pub artifact_manifest_hash: Option<String>,
    pub retained_log_path_hash: Option<String>,
    pub raw_output_included: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VerificationBrokerView {
    pub schema: String,
    pub bead_id: Option<String>,
    pub status: VerificationBrokerStatus,
    pub matched_run_id: Option<String>,
    pub command_class: String,
    pub command_hash: String,
    pub normalized_argv_hash: String,
    pub source_tree_fingerprint_class: String,
    pub env_fingerprint_class: String,
    pub target_profile: Option<String>,
    pub execution_substrate: String,
    pub rch: VerificationBrokerRchMetadata,
    pub exit_code: Option<i32>,
    pub finished_at: Option<String>,
    pub compatibility_reason_codes: Vec<String>,
    pub stale_reason_codes: Vec<String>,
    pub first_failure_summary_ref: Option<VerificationFirstFailureSummaryRef>,
    pub suggested_action: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProofBrokerAdmissionVerdict {
    ReuseExisting,
    WaitForInflight,
    DispatchAllowed,
    SourceStateMismatch,
    EnvironmentBlocked,
    ProofUnusable,
    UnknownInsufficientEvidence,
}

impl ProofBrokerAdmissionVerdict {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ReuseExisting => "reuse_existing",
            Self::WaitForInflight => "wait_for_inflight",
            Self::DispatchAllowed => "dispatch_allowed",
            Self::SourceStateMismatch => "source_state_mismatch",
            Self::EnvironmentBlocked => "environment_blocked",
            Self::ProofUnusable => "proof_unusable",
            Self::UnknownInsufficientEvidence => "unknown_insufficient_evidence",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProofBrokerLedgerState {
    AdmissionOnly,
    InFlight,
    Completed,
    Rejected,
}

impl ProofBrokerLedgerState {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AdmissionOnly => "admission_only",
            Self::InFlight => "in_flight",
            Self::Completed => "completed",
            Self::Rejected => "rejected",
        }
    }
}

#[derive(Clone, Debug)]
pub struct ProofBrokerFingerprintInput<'a> {
    pub bead_id: Option<&'a str>,
    pub command_class: &'a str,
    pub command_hash: &'a str,
    pub normalized_argv_hash: &'a str,
    pub source_tree_fingerprint: Option<&'a str>,
    pub source_materialization: Option<&'a str>,
    pub dirty_status_hash: Option<&'a str>,
    pub env_fingerprint_class: Option<&'a str>,
    pub target_profile: Option<&'a str>,
    pub execution_substrate: &'a str,
    pub rch_runtime_class: Option<&'a str>,
    pub worker_requirement: Option<&'a str>,
    pub local_cargo_tripwire_class: Option<&'a str>,
    pub build_admission_posture: Option<&'a str>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProofBrokerFingerprint {
    pub fingerprint_id: String,
    pub bead_id: Option<String>,
    pub command_class: String,
    pub command_hash: String,
    pub normalized_argv_hash: String,
    pub source_tree_fingerprint: String,
    pub source_materialization: String,
    pub dirty_status_hash: String,
    pub env_fingerprint_class: String,
    pub target_profile: Option<String>,
    pub execution_substrate: String,
    pub rch_runtime_class: String,
    pub worker_requirement: String,
    pub local_cargo_tripwire_class: String,
    pub build_admission_posture: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProofBrokerOwnerRef {
    pub agent_name: Option<String>,
    pub bead_id: Option<String>,
    pub mail_thread_id: Option<String>,
    pub build_slot: Option<String>,
    pub rch_job_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProofBrokerEvidenceRef {
    pub kind: String,
    pub id: String,
    pub content_hash: Option<String>,
    pub redacted: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProofBrokerAdmissionDecision {
    pub verdict: ProofBrokerAdmissionVerdict,
    pub reason_codes: Vec<String>,
    pub next_action: String,
    pub reuse_run_id: Option<String>,
    pub wait_owner: Option<ProofBrokerOwnerRef>,
}

#[derive(Clone, Debug)]
pub struct ProofBrokerLedgerRecordInput<'a> {
    pub fingerprint: ProofBrokerFingerprint,
    pub state: ProofBrokerLedgerState,
    pub admission_verdict: ProofBrokerAdmissionVerdict,
    pub reason_codes: Vec<&'a str>,
    pub next_action: &'a str,
    pub reuse_run_id: Option<&'a str>,
    pub wait_owner: Option<ProofBrokerOwnerRef>,
    pub run_id: Option<&'a str>,
    pub owner: Option<ProofBrokerOwnerRef>,
    pub created_at: &'a str,
    pub started_at: Option<&'a str>,
    pub completed_at: Option<&'a str>,
    pub expires_at: Option<&'a str>,
    pub source_state_valid_until: Option<&'a str>,
    pub invalidation_reasons: Vec<&'a str>,
    pub evidence_refs: Vec<ProofBrokerEvidenceRef>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProofBrokerLedgerRecord {
    pub schema: String,
    pub row_id: String,
    pub fingerprint: ProofBrokerFingerprint,
    pub state: ProofBrokerLedgerState,
    pub admission: ProofBrokerAdmissionDecision,
    pub run_id: Option<String>,
    pub owner: Option<ProofBrokerOwnerRef>,
    pub created_at: String,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub expires_at: Option<String>,
    pub source_state_valid_until: Option<String>,
    pub invalidation_reasons: Vec<String>,
    pub evidence_refs: Vec<ProofBrokerEvidenceRef>,
    pub raw_output_included: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CompileBlockerCacheStatus {
    Current,
    StaleSourceTree,
    StaleSourceFile,
}

impl CompileBlockerCacheStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Current => "current",
            Self::StaleSourceTree => "stale_source_tree",
            Self::StaleSourceFile => "stale_source_file",
        }
    }
}

#[derive(Clone, Debug)]
pub struct CompileBlockerCacheInput<'a> {
    pub record: &'a VerificationRunRecord,
    pub command_class: &'a str,
    pub first_source_path: &'a str,
    pub source_file_hash: Option<&'a str>,
    pub rust_error_code: Option<&'a str>,
    pub line: Option<u32>,
    pub column: Option<u32>,
    pub short_message: &'a str,
    pub owner_candidates: Vec<&'a str>,
    pub reservation_holder: Option<&'a str>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CompileBlockerCacheEntry {
    pub schema: String,
    pub cache_key: String,
    pub run_id: String,
    pub bead_id: Option<String>,
    pub agent_name: Option<String>,
    pub source_tree_fingerprint: Option<String>,
    pub command_class: String,
    pub command_hash: String,
    pub normalized_argv_hash: String,
    pub execution_substrate: String,
    pub worker_host: Option<String>,
    pub first_source_path: String,
    pub source_file_hash: Option<String>,
    pub rust_error_code: Option<String>,
    pub line: Option<u32>,
    pub column: Option<u32>,
    pub short_message: String,
    pub owner_candidates: Vec<String>,
    pub reservation_holder: Option<String>,
    pub first_failure_summary_ref: VerificationFirstFailureSummaryRef,
}

#[derive(Clone, Debug)]
pub struct CompileBlockerLookupRequest<'a> {
    pub current_source_tree_fingerprint: Option<&'a str>,
    pub current_source_file_hash: Option<&'a str>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CompileBlockerLookup {
    pub schema: String,
    pub cache_key: String,
    pub status: CompileBlockerCacheStatus,
    pub stale_reason_codes: Vec<String>,
    pub suggested_action: String,
    pub coordination_target: Option<String>,
    pub entry: CompileBlockerCacheEntry,
}

#[derive(Clone, Debug)]
pub struct VerificationCloseoutCapsuleRequest<'a> {
    pub requested_surface: &'a str,
    pub bead_id: Option<&'a str>,
    pub source_hash: Option<&'a str>,
    pub reusable_until: Option<&'a str>,
    pub source_must_match: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VerificationCloseoutSupportBundleMetadata {
    pub run_id: String,
    pub stdout_hash: Option<String>,
    pub stderr_excerpt_hash: Option<String>,
    pub provenance: Vec<VerificationRunProvenance>,
    pub raw_output_included: bool,
    pub local_paths_redacted: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VerificationCloseoutCapsule {
    pub schema: String,
    pub requested_surface: String,
    pub bead_id: Option<String>,
    pub command_summary: String,
    pub source_hash: Option<String>,
    pub command_hash: String,
    pub execution_substrate: String,
    pub worker_host: Option<String>,
    pub result: String,
    pub passed_count: Option<u32>,
    pub failed_count: Option<u32>,
    pub artifact_manifest_hash: Option<String>,
    pub retained_log_reference: Option<String>,
    pub caveats: Vec<String>,
    pub reusable_until: Option<String>,
    pub source_must_match: bool,
    pub failure_mode_codes: Vec<String>,
    pub support_bundle_metadata: VerificationCloseoutSupportBundleMetadata,
}

#[derive(Clone, Debug)]
pub struct VerificationRunInput<'a> {
    pub run_id: Option<&'a str>,
    pub bead_id: Option<&'a str>,
    pub agent_name: Option<&'a str>,
    pub source_hash: Option<&'a str>,
    pub command_hash: Option<&'a str>,
    pub command_argv: &'a [&'a str],
    pub cargo_target_dir: Option<&'a str>,
    pub execution_substrate: &'a str,
    pub worker_host: Option<&'a str>,
    pub started_at: Option<&'a str>,
    pub finished_at: Option<&'a str>,
    pub exit_code: Option<i32>,
    pub stdout_hash: Option<&'a str>,
    pub stderr_excerpt: Option<&'a str>,
    pub artifact_manifest_hash: Option<&'a str>,
    pub retained_log_path: Option<&'a str>,
    pub provenance: Vec<VerificationRunProvenance>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VerificationRunImportError {
    InvalidJsonLine { line: usize, message: String },
    MissingArtifactManifest { line: usize },
    RawOutputRejected { line: usize, field: String },
}

impl fmt::Display for VerificationRunImportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidJsonLine { line, message } => {
                write!(f, "invalid J1 JSONL at line {line}: {message}")
            }
            Self::MissingArtifactManifest { line } => {
                write!(
                    f,
                    "command_end at line {line} has no following artifact_manifest"
                )
            }
            Self::RawOutputRejected { line, field } => {
                write!(f, "raw output field {field} is not allowed at line {line}")
            }
        }
    }
}

impl Error for VerificationRunImportError {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RchVerificationEvidenceParseError {
    NotAnObject,
    MissingSchema,
    UnexpectedSchema { found: String },
    MissingCommand,
    MissingCommandHash,
}

impl fmt::Display for RchVerificationEvidenceParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotAnObject => write!(f, "RCH verification proof must be a JSON object"),
            Self::MissingSchema => write!(f, "RCH verification proof is missing schema"),
            Self::UnexpectedSchema { found } => {
                write!(f, "expected schema {RCH_VERIFY_SCHEMA_V1}, found {found}")
            }
            Self::MissingCommand => write!(f, "RCH verification proof is missing command"),
            Self::MissingCommandHash => {
                write!(f, "RCH verification proof is missing command_hash")
            }
        }
    }
}

impl Error for RchVerificationEvidenceParseError {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GithubActionsVerificationEvidenceParseError {
    NotAnObject,
    UnexpectedSchema { found: String },
    MissingName,
    MissingStatus,
}

impl fmt::Display for GithubActionsVerificationEvidenceParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotAnObject => write!(f, "GitHub Actions proof must be a JSON object"),
            Self::UnexpectedSchema { found } => write!(
                f,
                "expected schema {GITHUB_ACTIONS_CHECK_RUN_SCHEMA_V1}, found {found}"
            ),
            Self::MissingName => write!(f, "GitHub Actions proof is missing check name"),
            Self::MissingStatus => write!(f, "GitHub Actions proof is missing status/conclusion"),
        }
    }
}

impl Error for GithubActionsVerificationEvidenceParseError {}

#[derive(Clone, Debug)]
pub struct VerificationEvidenceInput<'a> {
    pub verification_id: &'a str,
    pub bead_id: Option<&'a str>,
    pub gate_name: &'a str,
    pub command: &'a str,
    pub status: VerificationStatus,
    pub exit_code: Option<i32>,
    pub started_at: Option<&'a str>,
    pub finished_at: Option<&'a str>,
    pub duration_ms: Option<u64>,
    pub environment: VerificationEnvironment,
    pub offload: VerificationOffload,
    pub output_summary: VerificationOutputSummary,
    pub artifacts: Vec<VerificationArtifactRef>,
    pub producer: ProducerMetadata,
}

impl VerificationEvidenceRecord {
    #[must_use]
    pub fn from_input(input: VerificationEvidenceInput<'_>) -> Self {
        Self {
            schema: VERIFICATION_EVIDENCE_SCHEMA_V1.to_owned(),
            verification_id: input.verification_id.to_owned(),
            bead_id: normalized_non_empty(input.bead_id),
            gate_name: input.gate_name.to_owned(),
            command: input.command.to_owned(),
            command_hash: command_hash(input.command),
            status: input.status,
            exit_code: input.exit_code,
            started_at: normalized_non_empty(input.started_at),
            finished_at: normalized_non_empty(input.finished_at),
            duration_ms: input.duration_ms,
            environment: input.environment,
            offload: input.offload,
            selector_admission: None,
            output_summary: input.output_summary,
            artifacts: input.artifacts,
            producer: input.producer,
        }
    }

    #[must_use]
    pub fn is_authoritative_pass(&self) -> bool {
        self.status == VerificationStatus::Passed
            && self.exit_code == Some(0)
            && !(self.offload.required_remote && self.offload.fallback_detected)
    }
}

#[must_use]
pub fn verification_evidence_beads_summary(record: &VerificationEvidenceRecord) -> String {
    let mut parts = vec![
        format!("schema={}", shell_safe_beads_summary_value(&record.schema)),
        format!(
            "verification_id={}",
            shell_safe_beads_summary_value(&record.verification_id)
        ),
        format!("status={}", record.status.as_str()),
        format!(
            "result_class={}",
            verification_evidence_result_class(record)
        ),
        format!("gate={}", shell_safe_beads_summary_value(&record.gate_name)),
        format!(
            "command_hash={}",
            shell_safe_beads_summary_value(&record.command_hash)
        ),
    ];
    if let Some(bead_id) = record.bead_id.as_deref() {
        parts.push(format!(
            "bead_id={}",
            shell_safe_beads_summary_value(bead_id)
        ));
    }
    if let Some(exit_code) = record.exit_code {
        parts.push(format!("exit_code={exit_code}"));
    }
    if let Some(tool) = record.offload.offload_tool.as_deref() {
        parts.push(format!(
            "offload_tool={}",
            shell_safe_beads_summary_value(tool)
        ));
    }
    if record.offload.required_remote {
        parts.push("remote_required=true".to_owned());
    }
    if let Some(worker) = record.offload.worker.as_deref() {
        parts.push(format!("worker={}", shell_safe_beads_summary_value(worker)));
    }
    if record.offload.fallback_detected {
        parts.push("fallback_detected=true".to_owned());
    }
    if let Some(reason) = record.offload.fallback_reason.as_deref() {
        parts.push(format!(
            "fallback_reason={}",
            shell_safe_beads_summary_value(reason)
        ));
    }
    if let Some(selector) = record.selector_admission.as_ref() {
        if let Some(status) = selector.status.as_deref() {
            parts.push(format!(
                "selector_admission={}",
                shell_safe_beads_summary_value(status)
            ));
        }
        if let Some(reason) = selector.selection_failure_reason.as_deref() {
            parts.push(format!(
                "selector_failure_reason={}",
                shell_safe_beads_summary_value(reason)
            ));
        }
        if selector.local_fallback_refused {
            parts.push("selector_local_fallback_refused=true".to_owned());
        }
    }
    if let Some(workspace) = record.environment.workspace_fingerprint.as_deref() {
        parts.push(format!(
            "workspace_fingerprint={}",
            shell_safe_beads_summary_value(workspace)
        ));
    }
    parts.push("raw_output_included=false".to_owned());
    parts.join("; ")
}

fn verification_evidence_result_class(record: &VerificationEvidenceRecord) -> &'static str {
    match record.status {
        VerificationStatus::Passed if record.is_authoritative_pass() => "authoritative_pass",
        VerificationStatus::Passed => "non_authoritative_pass",
        VerificationStatus::Failed => "code_failure",
        VerificationStatus::Blocked => "environment_blocker",
        VerificationStatus::FallbackDetected if record.offload.required_remote => {
            "environment_blocker"
        }
        VerificationStatus::FallbackDetected => "fallback_detected",
        VerificationStatus::Interrupted => "interrupted",
        VerificationStatus::Unknown => "unknown",
    }
}

fn shell_safe_beads_summary_value(value: &str) -> String {
    let sanitized: String = value
        .trim()
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, ':' | '_' | '-' | '.' | '/' | '=' | ',') {
                ch
            } else {
                '_'
            }
        })
        .collect();
    if sanitized.is_empty() {
        "empty".to_owned()
    } else {
        sanitized
    }
}

pub fn verification_evidence_record_from_rch_verify(
    value: &JsonValue,
) -> Result<VerificationEvidenceRecord, RchVerificationEvidenceParseError> {
    if !value.is_object() {
        return Err(RchVerificationEvidenceParseError::NotAnObject);
    }

    let schema = value
        .get("schema")
        .and_then(JsonValue::as_str)
        .ok_or(RchVerificationEvidenceParseError::MissingSchema)?;
    if schema != RCH_VERIFY_SCHEMA_V1 {
        return Err(RchVerificationEvidenceParseError::UnexpectedSchema {
            found: schema.to_owned(),
        });
    }

    let command = rch_command(value).ok_or(RchVerificationEvidenceParseError::MissingCommand)?;
    let command_hash = rch_string(value, "command_hash")
        .ok_or(RchVerificationEvidenceParseError::MissingCommandHash)?;
    let exit_code = rch_i32(value, "exit_code");
    let started_at = rch_string(value, "started_at");
    let finished_at = rch_finished_at(value);
    let duration_ms = rch_u64(value, "duration_ms").or_else(|| rch_u64(value, "elapsed_ms"));
    let workspace_fingerprint = rch_workspace_fingerprint(value);
    let observed_at = finished_at
        .clone()
        .or_else(|| rch_string(value, "generated_at"));
    let degraded_codes = rch_degraded_codes(value);
    let status = rch_verification_status(
        rch_string(value, "status").as_deref(),
        exit_code,
        &degraded_codes,
    );
    let worker = rch_string(value, "worker_id");
    let fallback_reason = rch_fallback_reason(value, &degraded_codes);
    let offload = rch_offload(
        value,
        worker.as_deref(),
        fallback_reason.as_deref(),
        &degraded_codes,
    );
    let artifacts = rch_artifacts(value);

    Ok(VerificationEvidenceRecord {
        schema: VERIFICATION_EVIDENCE_SCHEMA_V1.to_owned(),
        verification_id: stable_rch_verification_id(
            &command_hash,
            finished_at.as_deref(),
            exit_code,
        ),
        bead_id: rch_string(value, "bead_id"),
        gate_name: rch_string(value, "command_kind").unwrap_or_else(|| "rch verify".to_owned()),
        command,
        command_hash,
        status,
        exit_code,
        started_at,
        finished_at,
        duration_ms,
        environment: VerificationEnvironment::new(
            workspace_fingerprint.as_deref(),
            rch_string(value, "remote_project_root").as_deref(),
            rch_toolchain(value).as_deref(),
        ),
        offload,
        selector_admission: rch_selector_admission(value),
        output_summary: VerificationOutputSummary {
            stdout_tail: rch_string(value, "stdout_tail"),
            stderr_tail: rch_string(value, "stderr_tail"),
            redacted: true,
        },
        artifacts,
        producer: ProducerMetadata::unknown_agent(
            ProducerSourceSystem::Verification,
            Some(RCH_VERIFY_SCHEMA_V1),
            None,
            workspace_fingerprint.as_deref(),
            observed_at.as_deref(),
        ),
    })
}

pub fn verification_evidence_record_from_github_actions_check_run(
    value: &JsonValue,
) -> Result<VerificationEvidenceRecord, GithubActionsVerificationEvidenceParseError> {
    if !value.is_object() {
        return Err(GithubActionsVerificationEvidenceParseError::NotAnObject);
    }
    if let Some(schema) = value.get("schema").and_then(JsonValue::as_str)
        && schema != GITHUB_ACTIONS_CHECK_RUN_SCHEMA_V1
    {
        return Err(
            GithubActionsVerificationEvidenceParseError::UnexpectedSchema {
                found: schema.to_owned(),
            },
        );
    }

    let name = gha_string(value, "name")
        .or_else(|| gha_string(value, "check_name"))
        .or_else(|| gha_string(value, "job_name"))
        .ok_or(GithubActionsVerificationEvidenceParseError::MissingName)?;
    let status_text = gha_string(value, "status");
    let conclusion = gha_string(value, "conclusion");
    if status_text.is_none() && conclusion.is_none() {
        return Err(GithubActionsVerificationEvidenceParseError::MissingStatus);
    }

    let workflow = gha_string(value, "workflow_name").or_else(|| gha_workflow_name(value));
    let run_id = gha_run_id(value);
    let head_sha = gha_string(value, "head_sha").or_else(|| gha_head_sha(value));
    let completed_at =
        gha_string(value, "completed_at").or_else(|| gha_string(value, "updated_at"));
    let started_at = gha_string(value, "started_at");
    let command = gha_command_summary(
        workflow.as_deref(),
        &name,
        run_id.as_deref(),
        head_sha.as_deref(),
    );
    let command_hash = gha_string(value, "command_hash").unwrap_or_else(|| command_hash(&command));
    let status = gha_verification_status(status_text.as_deref(), conclusion.as_deref());
    let exit_code = gha_exit_code(status, conclusion.as_deref());
    let repo = gha_string(value, "repository")
        .or_else(|| gha_string(value, "repo"))
        .or_else(|| gha_repository_full_name(value));
    let observed_at = completed_at
        .clone()
        .or_else(|| gha_string(value, "created_at"));
    let check_run_id = gha_string(value, "check_run_id").or_else(|| gha_string(value, "id"));

    Ok(VerificationEvidenceRecord {
        schema: VERIFICATION_EVIDENCE_SCHEMA_V1.to_owned(),
        verification_id: stable_github_actions_verification_id(
            &command_hash,
            completed_at.as_deref(),
            conclusion.as_deref(),
        ),
        bead_id: gha_string(value, "bead_id"),
        gate_name: gha_gate_name(workflow.as_deref(), &name),
        command,
        command_hash,
        status,
        exit_code,
        started_at,
        finished_at: completed_at,
        duration_ms: gha_u64(value, "duration_ms"),
        environment: VerificationEnvironment::new(
            head_sha
                .as_deref()
                .map(|sha| format!("github_head_sha:{sha}"))
                .as_deref(),
            repo.as_deref(),
            gha_toolchain(value, workflow.as_deref(), run_id.as_deref()).as_deref(),
        ),
        offload: VerificationOffload {
            required_remote: false,
            remote_required_env: None,
            offload_tool: Some("github_actions".to_owned()),
            worker: gha_string(value, "runner_name"),
            fallback_detected: false,
            fallback_reason: None,
        },
        selector_admission: None,
        output_summary: gha_output_summary(value, conclusion.as_deref()),
        artifacts: gha_artifacts(value),
        producer: ProducerMetadata::unknown_agent(
            ProducerSourceSystem::Verification,
            run_id.as_deref(),
            check_run_id.as_deref(),
            head_sha.as_deref(),
            observed_at.as_deref(),
        ),
    })
}

impl VerificationRunRecord {
    #[must_use]
    pub fn from_input(input: VerificationRunInput<'_>) -> Self {
        let command_argv_hash = hash_json_array(input.command_argv);
        let command_hash = input
            .command_hash
            .map(str::to_owned)
            .unwrap_or_else(|| command_argv_hash.clone());
        let stderr_excerpt_hash = input.stderr_excerpt.map(hash_str);
        let run_id = input
            .run_id
            .map(str::to_owned)
            .unwrap_or_else(|| stable_run_id(&command_hash, input.finished_at, input.exit_code));

        Self {
            schema: VERIFICATION_RUN_SCHEMA_V1.to_owned(),
            run_id,
            bead_id: normalized_non_empty(input.bead_id),
            agent_name: normalized_non_empty(input.agent_name),
            source_hash: normalized_non_empty(input.source_hash),
            command_hash,
            command_argv_hash,
            cargo_target_dir_hash_or_class: input.cargo_target_dir.map(classify_or_hash_path),
            execution_substrate: normalized_non_empty(Some(input.execution_substrate))
                .unwrap_or_else(|| "unknown".to_owned()),
            worker_host: normalized_non_empty(input.worker_host),
            started_at: normalized_non_empty(input.started_at),
            finished_at: normalized_non_empty(input.finished_at),
            exit_code: input.exit_code,
            stdout_hash: normalized_non_empty(input.stdout_hash),
            stderr_excerpt_hash,
            artifact_manifest_hash: normalized_non_empty(input.artifact_manifest_hash),
            retained_log_path_hash: input.retained_log_path.map(hash_str),
            provenance: input.provenance,
        }
    }
}

#[must_use]
pub fn verification_evidence_record_from_run_record(
    record: &VerificationRunRecord,
) -> VerificationEvidenceRecord {
    let observed_at = record
        .finished_at
        .as_deref()
        .or(record.started_at.as_deref());

    VerificationEvidenceRecord {
        schema: VERIFICATION_EVIDENCE_SCHEMA_V1.to_owned(),
        verification_id: stable_run_verification_id(record),
        bead_id: record.bead_id.clone(),
        gate_name: run_record_gate_name(record),
        command: format!("verification_run {}", command_summary(record)),
        command_hash: record.command_hash.clone(),
        status: run_record_status(record),
        exit_code: record.exit_code,
        started_at: record.started_at.clone(),
        finished_at: record.finished_at.clone(),
        duration_ms: None,
        environment: VerificationEnvironment::new(
            record.source_hash.as_deref(),
            None,
            record.cargo_target_dir_hash_or_class.as_deref(),
        ),
        offload: run_record_offload(record),
        selector_admission: None,
        output_summary: run_record_output_summary(record),
        artifacts: run_record_artifacts(record),
        producer: ProducerMetadata::known_agent(
            ProducerSourceSystem::Verification,
            record.agent_name.as_deref(),
            None,
            None,
            Some(&record.run_id),
            None,
            record.source_hash.as_deref(),
            observed_at,
        ),
    }
}

#[must_use]
pub fn compile_blocker_cache_entry(
    input: CompileBlockerCacheInput<'_>,
) -> Option<CompileBlockerCacheEntry> {
    let record = input.record;
    if record.exit_code.is_none_or(|code| code == 0) {
        return None;
    }

    let command_class = normalized_non_empty(Some(input.command_class))
        .unwrap_or_else(|| "unknown_command".to_owned());
    let first_source_path = redaction_safe_source_path(input.first_source_path);
    let owner_candidates = sorted_owner_candidates(&input.owner_candidates);
    let reservation_holder = normalized_non_empty(input.reservation_holder);
    let short_message = compact_short_message(input.short_message);

    Some(CompileBlockerCacheEntry {
        schema: VERIFICATION_COMPILE_BLOCKER_CACHE_SCHEMA_V1.to_owned(),
        cache_key: compile_blocker_cache_key(
            record.source_hash.as_deref(),
            &command_class,
            &first_source_path,
        ),
        run_id: record.run_id.clone(),
        bead_id: record.bead_id.clone(),
        agent_name: record.agent_name.clone(),
        source_tree_fingerprint: record.source_hash.clone(),
        command_class,
        command_hash: record.command_hash.clone(),
        normalized_argv_hash: record.command_argv_hash.clone(),
        execution_substrate: record.execution_substrate.clone(),
        worker_host: record.worker_host.clone(),
        first_source_path,
        source_file_hash: normalized_non_empty(input.source_file_hash),
        rust_error_code: normalized_non_empty(input.rust_error_code),
        line: input.line,
        column: input.column,
        short_message,
        owner_candidates,
        reservation_holder,
        first_failure_summary_ref: VerificationFirstFailureSummaryRef {
            stderr_excerpt_hash: record.stderr_excerpt_hash.clone(),
            artifact_manifest_hash: record.artifact_manifest_hash.clone(),
            retained_log_path_hash: record.retained_log_path_hash.clone(),
            raw_output_included: false,
        },
    })
}

#[must_use]
pub fn compile_blocker_lookup(
    entry: CompileBlockerCacheEntry,
    request: CompileBlockerLookupRequest<'_>,
) -> CompileBlockerLookup {
    let requested_source = normalized_non_empty(request.current_source_tree_fingerprint);
    let requested_file_hash = normalized_non_empty(request.current_source_file_hash);
    let mut stale_reason_codes = Vec::new();

    if requested_source.is_some()
        && entry.source_tree_fingerprint.is_some()
        && requested_source != entry.source_tree_fingerprint
    {
        stale_reason_codes.push("source_tree_fingerprint_changed".to_owned());
    }
    if requested_file_hash.is_some()
        && entry.source_file_hash.is_some()
        && requested_file_hash != entry.source_file_hash
    {
        stale_reason_codes.push("source_file_hash_changed".to_owned());
    }
    stale_reason_codes.sort();
    stale_reason_codes.dedup();

    let status = if stale_reason_codes
        .iter()
        .any(|code| code == "source_tree_fingerprint_changed")
    {
        CompileBlockerCacheStatus::StaleSourceTree
    } else if stale_reason_codes
        .iter()
        .any(|code| code == "source_file_hash_changed")
    {
        CompileBlockerCacheStatus::StaleSourceFile
    } else {
        CompileBlockerCacheStatus::Current
    };
    let coordination_target = coordination_target(&entry);
    let suggested_action = compile_blocker_suggested_action(status, &entry, &coordination_target);

    CompileBlockerLookup {
        schema: VERIFICATION_COMPILE_BLOCKER_LOOKUP_SCHEMA_V1.to_owned(),
        cache_key: entry.cache_key.clone(),
        status,
        stale_reason_codes,
        suggested_action,
        coordination_target,
        entry,
    }
}

#[must_use]
pub fn command_hash(command: &str) -> String {
    hash_str(command)
}

pub fn verification_run_records_from_j1_jsonl(
    jsonl: &str,
) -> Result<Vec<VerificationRunRecord>, VerificationRunImportError> {
    let mut records = Vec::new();
    let mut pending_command: Option<PendingJ1Command> = None;

    for (line_index, line) in jsonl.lines().enumerate() {
        let line_number = line_index + 1;
        if line.trim().is_empty() {
            continue;
        }
        let event: JsonValue = serde_json::from_str(line).map_err(|error| {
            VerificationRunImportError::InvalidJsonLine {
                line: line_number,
                message: error.to_string(),
            }
        })?;
        reject_raw_output_fields(&event, line_number)?;

        if event.get("schema").and_then(JsonValue::as_str) != Some("ee.test_event.v1") {
            continue;
        }
        match event.get("kind").and_then(JsonValue::as_str) {
            Some("command_end") => {
                if let Some(command) = pending_command.take() {
                    return Err(VerificationRunImportError::MissingArtifactManifest {
                        line: command.line,
                    });
                }
                pending_command = Some(PendingJ1Command::from_event(line_number, &event));
            }
            Some("artifact_manifest") => {
                let command = pending_command.take();
                records.push(run_record_from_artifact_manifest_event(
                    line_number,
                    &event,
                    command.as_ref(),
                )?);
            }
            _ => {}
        }
    }

    if let Some(command) = pending_command {
        return Err(VerificationRunImportError::MissingArtifactManifest { line: command.line });
    }

    Ok(records)
}

#[must_use]
pub fn verification_reuse_advisory(
    request: VerificationReuseRequest<'_>,
    records: &[VerificationRunRecord],
) -> VerificationReuseAdvisory {
    let mut candidates = records.iter().collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        right
            .finished_at
            .cmp(&left.finished_at)
            .then_with(|| right.run_id.cmp(&left.run_id))
    });

    let exact = candidates
        .iter()
        .copied()
        .find(|record| record_matches_request(record, &request));
    let command_match = candidates
        .iter()
        .copied()
        .find(|record| record.command_hash == request.command_hash);
    let source_match = request.source_hash.and_then(|source_hash| {
        candidates
            .iter()
            .copied()
            .find(|record| record.source_hash.as_deref() == Some(source_hash))
    });

    let (status, matched, reason) = if let Some(record) = exact {
        match record.exit_code {
            Some(0) => (
                VerificationReuseStatus::ReusablePass,
                Some(record),
                "matching verification run passed for the same source, command, and substrate",
            ),
            Some(_) => (
                VerificationReuseStatus::ReusableFail,
                Some(record),
                "matching verification run failed for the same source, command, and substrate",
            ),
            None => (
                VerificationReuseStatus::InFlight,
                Some(record),
                "matching verification run is still in flight or has no final exit code",
            ),
        }
    } else if let Some(record) = command_match {
        (
            VerificationReuseStatus::StaleSource,
            Some(record),
            "matching command exists, but the source fingerprint differs or is unavailable",
        )
    } else if let Some(record) = source_match {
        (
            VerificationReuseStatus::MismatchedCommand,
            Some(record),
            "matching source exists, but the command fingerprint differs",
        )
    } else if records.is_empty() {
        (
            VerificationReuseStatus::MissingEvidence,
            None,
            "no verification run evidence is available",
        )
    } else {
        (
            VerificationReuseStatus::RerunRequired,
            None,
            "no reusable run matches the requested source, command, and substrate",
        )
    };

    VerificationReuseAdvisory {
        schema: VERIFICATION_REUSE_ADVISORY_SCHEMA_V1.to_owned(),
        bead_id: normalized_non_empty(request.bead_id),
        status,
        requested_source_hash: normalized_non_empty(request.source_hash),
        requested_command_hash: request.command_hash.to_owned(),
        requested_execution_substrate: request.execution_substrate.to_owned(),
        feature_profile_hash: normalized_non_empty(request.feature_profile_hash),
        workspace_generation: request.workspace_generation,
        strictness_flags: sorted_flags(&request.strictness_flags),
        matched_run_id: matched.map(|record| record.run_id.clone()),
        matched_agent_name: matched.and_then(|record| record.agent_name.clone()),
        matched_finished_at: matched.and_then(|record| record.finished_at.clone()),
        reason: reason.to_owned(),
        repair_actions: reuse_repair_actions(status, matched, &request),
    }
}

#[must_use]
pub fn verification_broker_view(
    request: VerificationBrokerViewRequest<'_>,
    records: &[VerificationRunRecord],
) -> VerificationBrokerView {
    let mut candidates = records.iter().collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        right
            .finished_at
            .cmp(&left.finished_at)
            .then_with(|| right.run_id.cmp(&left.run_id))
    });

    let exact = candidates
        .iter()
        .copied()
        .find(|record| broker_exact_match(record, &request));
    let command_match = candidates
        .iter()
        .copied()
        .find(|record| broker_command_match(record, &request));
    let source_match = request.source_hash.and_then(|source_hash| {
        candidates.iter().copied().find(|record| {
            record.source_hash.as_deref() == Some(source_hash)
                && record.execution_substrate == request.execution_substrate
                && broker_env_match(record, &request)
        })
    });

    let (status, matched, compatibility_reason_codes, stale_reason_codes, suggested_action) =
        if let Some(record) = exact {
            match record.exit_code {
                Some(0) => (
                    VerificationBrokerStatus::Reusable,
                    Some(record),
                    vec![
                        "source_match",
                        "command_match",
                        "substrate_match",
                        "env_class_match",
                    ],
                    Vec::new(),
                    "cite_existing_run",
                ),
                Some(_) => (
                    VerificationBrokerStatus::KnownBlocker,
                    Some(record),
                    vec![
                        "source_match",
                        "command_match",
                        "substrate_match",
                        "nonzero_exit_code",
                    ],
                    Vec::new(),
                    "inspect_known_blocker",
                ),
                None => (
                    VerificationBrokerStatus::InProgress,
                    Some(record),
                    vec![
                        "source_match",
                        "command_match",
                        "substrate_match",
                        "no_final_exit_code",
                    ],
                    Vec::new(),
                    "wait_for_in_progress_run",
                ),
            }
        } else if let Some(record) = command_match {
            (
                VerificationBrokerStatus::Stale,
                Some(record),
                vec!["command_match", "substrate_match"],
                vec!["source_hash_mismatch"],
                "rerun_current_source",
            )
        } else if let Some(record) = source_match {
            if record.exit_code.is_some_and(|code| code != 0) {
                (
                    VerificationBrokerStatus::KnownBlocker,
                    Some(record),
                    vec!["source_match", "prior_nonzero_exit_code"],
                    vec!["command_hash_mismatch"],
                    "inspect_known_blocker",
                )
            } else {
                (
                    VerificationBrokerStatus::Incompatible,
                    Some(record),
                    vec!["source_match"],
                    vec!["command_hash_mismatch"],
                    "adjust_command_or_profile",
                )
            }
        } else {
            (
                VerificationBrokerStatus::Unavailable,
                None,
                vec!["no_matching_record"],
                Vec::new(),
                "import_or_run_verification",
            )
        };

    VerificationBrokerView {
        schema: VERIFICATION_BROKER_VIEW_SCHEMA_V1.to_owned(),
        bead_id: normalized_non_empty(request.bead_id),
        status,
        matched_run_id: matched.map(|record| record.run_id.clone()),
        command_class: normalized_non_empty(Some(request.command_class))
            .unwrap_or_else(|| "unknown".to_owned()),
        command_hash: request.command_hash.to_owned(),
        normalized_argv_hash: request.normalized_argv_hash.to_owned(),
        source_tree_fingerprint_class: request
            .source_hash
            .map(str::to_owned)
            .unwrap_or_else(|| "class:unknown_source".to_owned()),
        env_fingerprint_class: normalized_non_empty(request.env_fingerprint_class)
            .or_else(|| matched.and_then(|record| record.cargo_target_dir_hash_or_class.clone()))
            .unwrap_or_else(|| "class:unknown_env".to_owned()),
        target_profile: normalized_non_empty(request.target_profile),
        execution_substrate: request.execution_substrate.to_owned(),
        rch: VerificationBrokerRchMetadata {
            required_remote: request.execution_substrate == "rch",
            worker_host: matched.and_then(|record| record.worker_host.clone()),
            job_id: None,
        },
        exit_code: matched.and_then(|record| record.exit_code),
        finished_at: matched.and_then(|record| record.finished_at.clone()),
        compatibility_reason_codes: sorted_owned_codes(compatibility_reason_codes),
        stale_reason_codes: sorted_owned_codes(stale_reason_codes),
        first_failure_summary_ref: broker_failure_ref(status, matched),
        suggested_action: suggested_action.to_owned(),
    }
}

#[must_use]
pub fn proof_broker_fingerprint(input: ProofBrokerFingerprintInput<'_>) -> ProofBrokerFingerprint {
    let mut fingerprint = ProofBrokerFingerprint {
        fingerprint_id: String::new(),
        bead_id: normalized_non_empty(input.bead_id),
        command_class: normalized_non_empty(Some(input.command_class))
            .unwrap_or_else(|| "class:unknown_command".to_owned()),
        command_hash: normalized_non_empty(Some(input.command_hash))
            .unwrap_or_else(|| "class:unknown_command_hash".to_owned()),
        normalized_argv_hash: normalized_non_empty(Some(input.normalized_argv_hash))
            .unwrap_or_else(|| "class:unknown_argv_hash".to_owned()),
        source_tree_fingerprint: normalized_non_empty(input.source_tree_fingerprint)
            .unwrap_or_else(|| "class:unknown_source".to_owned()),
        source_materialization: normalized_non_empty(input.source_materialization)
            .unwrap_or_else(|| "class:unknown_materialization".to_owned()),
        dirty_status_hash: normalized_non_empty(input.dirty_status_hash)
            .unwrap_or_else(|| "class:clean_or_unknown_dirty_state".to_owned()),
        env_fingerprint_class: normalized_non_empty(input.env_fingerprint_class)
            .unwrap_or_else(|| "class:unknown_env".to_owned()),
        target_profile: normalized_non_empty(input.target_profile),
        execution_substrate: normalized_non_empty(Some(input.execution_substrate))
            .unwrap_or_else(|| "unknown".to_owned()),
        rch_runtime_class: normalized_non_empty(input.rch_runtime_class)
            .unwrap_or_else(|| "class:unknown_rch_runtime".to_owned()),
        worker_requirement: normalized_non_empty(input.worker_requirement)
            .unwrap_or_else(|| "class:any_worker".to_owned()),
        local_cargo_tripwire_class: normalized_non_empty(input.local_cargo_tripwire_class)
            .unwrap_or_else(|| "class:tripwire_unknown".to_owned()),
        build_admission_posture: normalized_non_empty(input.build_admission_posture)
            .unwrap_or_else(|| "class:admission_unknown".to_owned()),
    };
    fingerprint.fingerprint_id = stable_proof_broker_fingerprint_id(&fingerprint);
    fingerprint
}

#[must_use]
pub fn proof_broker_ledger_record(
    input: ProofBrokerLedgerRecordInput<'_>,
) -> ProofBrokerLedgerRecord {
    let created_at = normalized_non_empty(Some(input.created_at))
        .unwrap_or_else(|| "1970-01-01T00:00:00Z".to_owned());
    let run_id = normalized_non_empty(input.run_id);
    ProofBrokerLedgerRecord {
        schema: PROOF_BROKER_SCHEMA_V1.to_owned(),
        row_id: stable_proof_broker_row_id(
            &input.fingerprint.fingerprint_id,
            input.state,
            run_id.as_deref(),
            &created_at,
        ),
        fingerprint: input.fingerprint,
        state: input.state,
        admission: ProofBrokerAdmissionDecision {
            verdict: input.admission_verdict,
            reason_codes: sorted_flags(&input.reason_codes),
            next_action: normalized_non_empty(Some(input.next_action))
                .unwrap_or_else(|| "inspect_broker_record".to_owned()),
            reuse_run_id: normalized_non_empty(input.reuse_run_id),
            wait_owner: input.wait_owner,
        },
        run_id,
        owner: input.owner,
        created_at,
        started_at: normalized_non_empty(input.started_at),
        completed_at: normalized_non_empty(input.completed_at),
        expires_at: normalized_non_empty(input.expires_at),
        source_state_valid_until: normalized_non_empty(input.source_state_valid_until),
        invalidation_reasons: sorted_flags(&input.invalidation_reasons),
        evidence_refs: input.evidence_refs,
        raw_output_included: false,
    }
}

#[must_use]
pub fn verification_closeout_capsule(
    request: VerificationCloseoutCapsuleRequest<'_>,
    record: &VerificationRunRecord,
) -> VerificationCloseoutCapsule {
    let requested_source_hash = normalized_non_empty(request.source_hash);
    let mut caveats = Vec::new();
    let mut failure_mode_codes = Vec::new();

    if record.artifact_manifest_hash.is_none() {
        failure_mode_codes.push("no_artifact_manifest".to_owned());
        caveats.push(
            "artifact manifest hash is unavailable; cite as advisory evidence only".to_owned(),
        );
    }
    if record.execution_substrate == "local_cargo" {
        failure_mode_codes.push("local_cargo_disallowed".to_owned());
        caveats
            .push("cargo evidence was local; remote-required gates need an RCH rerun".to_owned());
    }
    if request.source_must_match
        && requested_source_hash.is_some()
        && record.source_hash != requested_source_hash
    {
        failure_mode_codes.push("source_hash_mismatch".to_owned());
        caveats.push("source hash differs from the requested closeout source".to_owned());
    }
    if record.finished_at.is_none() || record.exit_code.is_none() || record.provenance.is_empty() {
        failure_mode_codes.push("evidence_incomplete".to_owned());
        caveats.push(
            "verification evidence is incomplete; do not treat as final closure proof".to_owned(),
        );
    }

    failure_mode_codes.sort();
    failure_mode_codes.dedup();
    caveats.sort();
    caveats.dedup();

    let (result, passed_count, failed_count) = match record.exit_code {
        Some(0) => ("passed", Some(1), Some(0)),
        Some(_) => ("failed", Some(0), Some(1)),
        None => ("in_flight", None, None),
    };

    VerificationCloseoutCapsule {
        schema: VERIFICATION_CLOSEOUT_CAPSULE_SCHEMA_V1.to_owned(),
        requested_surface: normalized_non_empty(Some(request.requested_surface))
            .unwrap_or_else(|| "beads_comment".to_owned()),
        bead_id: normalized_non_empty(request.bead_id).or_else(|| record.bead_id.clone()),
        command_summary: command_summary(record),
        source_hash: record.source_hash.clone(),
        command_hash: record.command_hash.clone(),
        execution_substrate: record.execution_substrate.clone(),
        worker_host: record.worker_host.clone(),
        result: result.to_owned(),
        passed_count,
        failed_count,
        artifact_manifest_hash: record.artifact_manifest_hash.clone(),
        retained_log_reference: record
            .retained_log_path_hash
            .as_ref()
            .map(|hash| format!("retained_log_path_hash:{hash}")),
        caveats,
        reusable_until: normalized_non_empty(request.reusable_until),
        source_must_match: request.source_must_match,
        failure_mode_codes,
        support_bundle_metadata: VerificationCloseoutSupportBundleMetadata {
            run_id: record.run_id.clone(),
            stdout_hash: record.stdout_hash.clone(),
            stderr_excerpt_hash: record.stderr_excerpt_hash.clone(),
            provenance: record.provenance.clone(),
            raw_output_included: false,
            local_paths_redacted: true,
        },
    }
}

#[must_use]
pub fn sample_verification_evidence_records() -> Vec<VerificationEvidenceRecord> {
    let env = VerificationEnvironment::new(
        Some("repo:25e38e130474e7f0292de2a3"),
        Some("/repo"),
        Some("rustc 1.96.0-nightly"),
    );
    let producer = ProducerMetadata::unknown_agent(
        ProducerSourceSystem::Verification,
        Some("verify-run-20260513"),
        None,
        Some("repo:25e38e130474e7f0292de2a3"),
        Some("2026-05-13T00:00:00Z"),
    );

    vec![
        VerificationEvidenceRecord::from_input(VerificationEvidenceInput {
            verification_id: "ver_pass_00000000000000000001",
            bead_id: Some("bd-example"),
            gate_name: "cargo fmt",
            command: "cargo fmt --check",
            status: VerificationStatus::Passed,
            exit_code: Some(0),
            started_at: Some("2026-05-13T00:00:00Z"),
            finished_at: Some("2026-05-13T00:00:01Z"),
            duration_ms: Some(1000),
            environment: env.clone(),
            offload: VerificationOffload::local(),
            output_summary: VerificationOutputSummary::empty(),
            artifacts: Vec::new(),
            producer: producer.clone(),
        }),
        VerificationEvidenceRecord::from_input(VerificationEvidenceInput {
            verification_id: "ver_fail_00000000000000000001",
            bead_id: Some("bd-example"),
            gate_name: "cargo clippy",
            command: "cargo clippy --all-targets -- -D warnings",
            status: VerificationStatus::Failed,
            exit_code: Some(101),
            started_at: Some("2026-05-13T00:01:00Z"),
            finished_at: Some("2026-05-13T00:01:20Z"),
            duration_ms: Some(20000),
            environment: env.clone(),
            offload: VerificationOffload::rch_required(Some("css")),
            output_summary: VerificationOutputSummary::redacted(Some(
                "error: could not compile `ee` due to warnings",
            )),
            artifacts: vec![VerificationArtifactRef::new(
                "target/verify/clippy.log",
                "log",
                Some("blake3:clippylog"),
            )],
            producer: producer.clone(),
        }),
        VerificationEvidenceRecord::from_input(VerificationEvidenceInput {
            verification_id: "ver_blocked_0000000000000001",
            bead_id: Some("bd-example"),
            gate_name: "cargo test",
            command: "RCH_REQUIRE_REMOTE=1 rch exec -- cargo test",
            status: VerificationStatus::Blocked,
            exit_code: None,
            started_at: Some("2026-05-13T00:02:00Z"),
            finished_at: Some("2026-05-13T00:02:02Z"),
            duration_ms: Some(2000),
            environment: env.clone(),
            offload: VerificationOffload::rch_required(None),
            output_summary: VerificationOutputSummary::redacted(Some(
                "RCH worker unavailable; command did not run",
            )),
            artifacts: Vec::new(),
            producer: producer.clone(),
        }),
        VerificationEvidenceRecord::from_input(VerificationEvidenceInput {
            verification_id: "ver_interrupted_000000000001",
            bead_id: Some("bd-example"),
            gate_name: "verify script",
            command: "./scripts/verify.sh",
            status: VerificationStatus::Interrupted,
            exit_code: None,
            started_at: Some("2026-05-13T00:03:00Z"),
            finished_at: None,
            duration_ms: None,
            environment: env.clone(),
            offload: VerificationOffload::local(),
            output_summary: VerificationOutputSummary::redacted(Some(
                "verification interrupted before a final exit code",
            )),
            artifacts: Vec::new(),
            producer: producer.clone(),
        }),
        VerificationEvidenceRecord::from_input(VerificationEvidenceInput {
            verification_id: "ver_fallback_0000000000001",
            bead_id: Some("bd-example"),
            gate_name: "cargo test producer",
            command: "rch exec -- cargo test --lib producer",
            status: VerificationStatus::FallbackDetected,
            exit_code: Some(0),
            started_at: Some("2026-05-13T00:04:00Z"),
            finished_at: Some("2026-05-13T00:04:45Z"),
            duration_ms: Some(45000),
            environment: env,
            offload: VerificationOffload::rch_fallback(
                Some("css"),
                Some("project path normalized outside canonical remote root"),
            ),
            output_summary: VerificationOutputSummary::redacted(Some(
                "RCH fell back to local execution; remote-required gate is not verified",
            )),
            artifacts: Vec::new(),
            producer,
        }),
    ]
}

#[must_use]
pub fn sample_verification_run_records() -> Vec<VerificationRunRecord> {
    vec![
        VerificationRunRecord {
            schema: VERIFICATION_RUN_SCHEMA_V1.to_owned(),
            run_id: "vrun_rch_00000000000000000001".to_owned(),
            bead_id: Some("bd-example".to_owned()),
            agent_name: Some("RubyWolf".to_owned()),
            source_hash: Some("blake3:source".to_owned()),
            command_hash: "blake3:rch-command".to_owned(),
            command_argv_hash: "blake3:rch-command-argv".to_owned(),
            cargo_target_dir_hash_or_class: Some("class:external_cargo_target".to_owned()),
            execution_substrate: "rch".to_owned(),
            worker_host: Some("css".to_owned()),
            started_at: Some("2026-05-15T05:00:00Z".to_owned()),
            finished_at: Some("2026-05-15T05:00:42Z".to_owned()),
            exit_code: Some(0),
            stdout_hash: Some("blake3:stdout".to_owned()),
            stderr_excerpt_hash: Some("blake3:stderr-excerpt".to_owned()),
            artifact_manifest_hash: Some("blake3:artifact-manifest".to_owned()),
            retained_log_path_hash: Some("blake3:retained-log-path".to_owned()),
            provenance: vec![VerificationRunProvenance {
                source: "j1_jsonl".to_owned(),
                event_kind: "artifact_manifest".to_owned(),
                line: Some(2),
            }],
        },
        VerificationRunRecord {
            schema: VERIFICATION_RUN_SCHEMA_V1.to_owned(),
            run_id: "vrun_shell_0000000000000000001".to_owned(),
            bead_id: Some("bd-example".to_owned()),
            agent_name: Some("RubyWolf".to_owned()),
            source_hash: Some("blake3:source".to_owned()),
            command_hash: "blake3:shell-command".to_owned(),
            command_argv_hash: "blake3:shell-command-argv".to_owned(),
            cargo_target_dir_hash_or_class: None,
            execution_substrate: "local_shell_static".to_owned(),
            worker_host: None,
            started_at: Some("2026-05-15T05:01:00Z".to_owned()),
            finished_at: Some("2026-05-15T05:01:01Z".to_owned()),
            exit_code: Some(0),
            stdout_hash: None,
            stderr_excerpt_hash: None,
            artifact_manifest_hash: None,
            retained_log_path_hash: None,
            provenance: vec![VerificationRunProvenance {
                source: "closeout_snippet".to_owned(),
                event_kind: "static_verifier".to_owned(),
                line: None,
            }],
        },
    ]
}

#[must_use]
pub fn sample_verification_reuse_advisories() -> Vec<VerificationReuseAdvisory> {
    let records = sample_verification_run_records();
    vec![
        verification_reuse_advisory(
            VerificationReuseRequest {
                bead_id: Some("bd-example"),
                source_hash: Some("blake3:source"),
                command_hash: "blake3:rch-command",
                execution_substrate: "rch",
                feature_profile_hash: Some("blake3:profile"),
                workspace_generation: Some(42),
                strictness_flags: vec!["RCH_REQUIRE_REMOTE=1", "--all-targets"],
            },
            &records,
        ),
        verification_reuse_advisory(
            VerificationReuseRequest {
                bead_id: Some("bd-example"),
                source_hash: Some("blake3:new-source"),
                command_hash: "blake3:rch-command",
                execution_substrate: "rch",
                feature_profile_hash: Some("blake3:profile"),
                workspace_generation: Some(43),
                strictness_flags: vec!["--all-targets", "RCH_REQUIRE_REMOTE=1"],
            },
            &records,
        ),
    ]
}

#[must_use]
pub fn sample_verification_broker_views() -> Vec<VerificationBrokerView> {
    let records = sample_verification_broker_records();
    vec![
        verification_broker_view(
            broker_request(Some("blake3:source"), "blake3:rch-command"),
            &records,
        ),
        verification_broker_view(
            broker_request(Some("blake3:source"), "blake3:failed-command"),
            &records,
        ),
        verification_broker_view(
            broker_request(Some("blake3:source"), "blake3:in-flight-command"),
            &records,
        ),
        verification_broker_view(
            broker_request(Some("blake3:new-source"), "blake3:rch-command"),
            &records,
        ),
        verification_broker_view(
            broker_request_for_substrate(
                Some("blake3:source"),
                "blake3:new-shell-command",
                "shell_static",
                "local_shell_static",
            ),
            &records,
        ),
        verification_broker_view(
            broker_request(Some("blake3:other-source"), "blake3:other-command"),
            &records,
        ),
    ]
}

#[must_use]
pub fn sample_proof_broker_ledger_records() -> Vec<ProofBrokerLedgerRecord> {
    let owner = ProofBrokerOwnerRef {
        agent_name: Some("RubyWolf".to_owned()),
        bead_id: Some("bd-1n3x1.1".to_owned()),
        mail_thread_id: Some("8198".to_owned()),
        build_slot: Some("proof:bd-1n3x1.1:broker".to_owned()),
        rch_job_id: Some("rch-job-20260605-0001".to_owned()),
    };
    let evidence = ProofBrokerEvidenceRef {
        kind: "verification_run".to_owned(),
        id: "vrun_rch_00000000000000000001".to_owned(),
        content_hash: Some("blake3:artifact-manifest".to_owned()),
        redacted: true,
    };

    let reusable = proof_broker_ledger_record(ProofBrokerLedgerRecordInput {
        fingerprint: sample_proof_broker_fingerprint(
            Some("blake3:source"),
            "blake3:rch-command",
            "blake3:rch-command-argv",
            Some("class:external_cargo_target"),
            Some("class:rch_client_1_0_37_daemon_0_1_3"),
            Some("class:tripwire_clean"),
        ),
        state: ProofBrokerLedgerState::Completed,
        admission_verdict: ProofBrokerAdmissionVerdict::ReuseExisting,
        reason_codes: vec![
            "source_match",
            "command_match",
            "env_class_match",
            "runtime_match",
            "tripwire_clean",
        ],
        next_action: "cite_existing_run",
        reuse_run_id: Some("vrun_rch_00000000000000000001"),
        wait_owner: None,
        run_id: Some("vrun_rch_00000000000000000001"),
        owner: Some(owner.clone()),
        created_at: "2026-06-05T17:20:00Z",
        started_at: Some("2026-06-05T17:00:00Z"),
        completed_at: Some("2026-06-05T17:08:00Z"),
        expires_at: Some("2026-06-05T19:08:00Z"),
        source_state_valid_until: Some("2026-06-05T19:08:00Z"),
        invalidation_reasons: Vec::new(),
        evidence_refs: vec![evidence.clone()],
    });

    let in_flight = proof_broker_ledger_record(ProofBrokerLedgerRecordInput {
        fingerprint: sample_proof_broker_fingerprint(
            Some("blake3:source"),
            "blake3:in-flight-command",
            "blake3:in-flight-argv",
            Some("class:external_cargo_target"),
            Some("class:rch_client_1_0_37_daemon_0_1_3"),
            Some("class:tripwire_clean"),
        ),
        state: ProofBrokerLedgerState::InFlight,
        admission_verdict: ProofBrokerAdmissionVerdict::WaitForInflight,
        reason_codes: vec!["equivalent_in_flight", "owner_known"],
        next_action: "wait_for_owner_or_watch_job",
        reuse_run_id: None,
        wait_owner: Some(owner.clone()),
        run_id: Some("vrun_in_flight_00000000000001"),
        owner: Some(owner.clone()),
        created_at: "2026-06-05T17:21:00Z",
        started_at: Some("2026-06-05T17:21:00Z"),
        completed_at: None,
        expires_at: Some("2026-06-05T18:21:00Z"),
        source_state_valid_until: Some("2026-06-05T18:21:00Z"),
        invalidation_reasons: Vec::new(),
        evidence_refs: Vec::new(),
    });

    let dispatch_allowed = proof_broker_ledger_record(ProofBrokerLedgerRecordInput {
        fingerprint: sample_proof_broker_fingerprint(
            Some("blake3:source-v2"),
            "blake3:new-command",
            "blake3:new-argv",
            Some("class:external_cargo_target"),
            Some("class:rch_client_1_0_37_daemon_0_1_3"),
            Some("class:tripwire_clean"),
        ),
        state: ProofBrokerLedgerState::AdmissionOnly,
        admission_verdict: ProofBrokerAdmissionVerdict::DispatchAllowed,
        reason_codes: vec!["no_equivalent_record", "source_current", "remote_required"],
        next_action: "launch_single_rch_proof",
        reuse_run_id: None,
        wait_owner: None,
        run_id: None,
        owner: None,
        created_at: "2026-06-05T17:22:00Z",
        started_at: None,
        completed_at: None,
        expires_at: Some("2026-06-05T17:32:00Z"),
        source_state_valid_until: Some("2026-06-05T17:32:00Z"),
        invalidation_reasons: Vec::new(),
        evidence_refs: Vec::new(),
    });

    let source_mismatch = proof_broker_ledger_record(ProofBrokerLedgerRecordInput {
        fingerprint: sample_proof_broker_fingerprint(
            Some("blake3:dirty-current-tree"),
            "blake3:rch-command",
            "blake3:rch-command-argv",
            Some("class:external_cargo_target"),
            Some("class:rch_client_1_0_37_daemon_0_1_3"),
            Some("class:tripwire_clean"),
        ),
        state: ProofBrokerLedgerState::Rejected,
        admission_verdict: ProofBrokerAdmissionVerdict::SourceStateMismatch,
        reason_codes: vec!["source_tree_mismatch", "dirty_status_changed"],
        next_action: "rerun_current_source",
        reuse_run_id: Some("vrun_rch_00000000000000000001"),
        wait_owner: None,
        run_id: Some("vrun_rch_00000000000000000001"),
        owner: None,
        created_at: "2026-06-05T17:23:00Z",
        started_at: None,
        completed_at: None,
        expires_at: None,
        source_state_valid_until: None,
        invalidation_reasons: vec!["source_tree_fingerprint_changed"],
        evidence_refs: vec![evidence.clone()],
    });

    let environment_blocked = proof_broker_ledger_record(ProofBrokerLedgerRecordInput {
        fingerprint: sample_proof_broker_fingerprint(
            Some("blake3:source"),
            "blake3:env-blocked-command",
            "blake3:env-blocked-argv",
            Some("class:external_cargo_target"),
            Some("class:rch_runtime_mismatch"),
            Some("class:tripwire_clean"),
        ),
        state: ProofBrokerLedgerState::Rejected,
        admission_verdict: ProofBrokerAdmissionVerdict::EnvironmentBlocked,
        reason_codes: vec!["rch_runtime_mismatch", "no_eligible_remote_worker"],
        next_action: "repair_remote_runtime_before_dispatch",
        reuse_run_id: None,
        wait_owner: None,
        run_id: None,
        owner: None,
        created_at: "2026-06-05T17:24:00Z",
        started_at: None,
        completed_at: None,
        expires_at: Some("2026-06-05T17:34:00Z"),
        source_state_valid_until: Some("2026-06-05T17:34:00Z"),
        invalidation_reasons: Vec::new(),
        evidence_refs: Vec::new(),
    });

    let proof_unusable = proof_broker_ledger_record(ProofBrokerLedgerRecordInput {
        fingerprint: sample_proof_broker_fingerprint(
            Some("blake3:source"),
            "blake3:local-cargo-command",
            "blake3:local-cargo-argv",
            Some("class:external_cargo_target"),
            Some("class:rch_client_1_0_37_daemon_0_1_3"),
            Some("class:local_cargo_bypass_detected"),
        ),
        state: ProofBrokerLedgerState::Rejected,
        admission_verdict: ProofBrokerAdmissionVerdict::ProofUnusable,
        reason_codes: vec!["local_cargo_bypass_detected", "remote_required"],
        next_action: "discard_local_cargo_evidence_and_rerun_remote",
        reuse_run_id: None,
        wait_owner: None,
        run_id: Some("vrun_local_cargo_bypass"),
        owner: None,
        created_at: "2026-06-05T17:25:00Z",
        started_at: None,
        completed_at: None,
        expires_at: None,
        source_state_valid_until: None,
        invalidation_reasons: vec!["local_cargo_tripwire_blocked"],
        evidence_refs: Vec::new(),
    });

    let unknown = proof_broker_ledger_record(ProofBrokerLedgerRecordInput {
        fingerprint: sample_proof_broker_fingerprint(
            None,
            "blake3:ambiguous-command",
            "blake3:ambiguous-argv",
            None,
            None,
            None,
        ),
        state: ProofBrokerLedgerState::AdmissionOnly,
        admission_verdict: ProofBrokerAdmissionVerdict::UnknownInsufficientEvidence,
        reason_codes: vec![
            "source_fingerprint_missing",
            "env_class_missing",
            "tripwire_unknown",
        ],
        next_action: "collect_source_and_environment_evidence",
        reuse_run_id: None,
        wait_owner: None,
        run_id: None,
        owner: None,
        created_at: "2026-06-05T17:26:00Z",
        started_at: None,
        completed_at: None,
        expires_at: Some("2026-06-05T17:36:00Z"),
        source_state_valid_until: None,
        invalidation_reasons: Vec::new(),
        evidence_refs: Vec::new(),
    });

    vec![
        reusable,
        in_flight,
        dispatch_allowed,
        source_mismatch,
        environment_blocked,
        proof_unusable,
        unknown,
    ]
}

#[must_use]
pub fn sample_verification_closeout_capsules() -> Vec<VerificationCloseoutCapsule> {
    let records = sample_verification_run_records();
    vec![
        verification_closeout_capsule(
            VerificationCloseoutCapsuleRequest {
                requested_surface: "beads_comment",
                bead_id: Some("bd-example"),
                source_hash: Some("blake3:source"),
                reusable_until: Some("2026-05-15T07:00:42Z"),
                source_must_match: true,
            },
            &records[0],
        ),
        verification_closeout_capsule(
            VerificationCloseoutCapsuleRequest {
                requested_surface: "support_bundle",
                bead_id: Some("bd-example"),
                source_hash: Some("blake3:source"),
                reusable_until: None,
                source_must_match: true,
            },
            &records[1],
        ),
    ]
}

#[must_use]
pub fn rch_cargo_closure_requirements() -> Vec<VerificationGateRequirement> {
    vec![
        VerificationGateRequirement::new("cargo fmt", Some("cargo fmt --check"), true),
        VerificationGateRequirement::new(
            "cargo clippy",
            Some("cargo clippy --all-targets -- -D warnings"),
            true,
        ),
        VerificationGateRequirement::new("cargo test", Some("cargo test"), true),
        VerificationGateRequirement::new("forbidden deps", Some("forbidden_deps"), true),
    ]
}

#[must_use]
pub fn verification_closure_guidance(
    bead_id: Option<&str>,
    requirements: &[VerificationGateRequirement],
    records: &[VerificationEvidenceRecord],
) -> VerificationClosureGuidance {
    let assessments = requirements
        .iter()
        .map(|requirement| assess_requirement(requirement, records))
        .collect::<Vec<_>>();
    let rejected_reasons = assessments
        .iter()
        .filter(|assessment| !assessment.satisfied)
        .map(|assessment| format!("{}: {}", assessment.gate_name, assessment.reason))
        .collect::<Vec<_>>();

    VerificationClosureGuidance {
        schema: VERIFICATION_CLOSURE_GUIDANCE_SCHEMA_V1.to_owned(),
        bead_id: normalized_non_empty(bead_id),
        can_close: rejected_reasons.is_empty(),
        assessments,
        rejected_reasons,
    }
}

fn normalized_non_empty(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|trimmed| !trimmed.is_empty())
        .map(str::to_owned)
}

fn rch_string(value: &JsonValue, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(JsonValue::as_str)
        .and_then(|raw| normalized_non_empty(Some(raw)))
}

fn rch_i32(value: &JsonValue, key: &str) -> Option<i32> {
    value
        .get(key)
        .and_then(JsonValue::as_i64)
        .and_then(|raw| i32::try_from(raw).ok())
}

fn rch_u64(value: &JsonValue, key: &str) -> Option<u64> {
    value.get(key).and_then(JsonValue::as_u64)
}

fn rch_bool(value: &JsonValue, key: &str) -> Option<bool> {
    value.get(key).and_then(JsonValue::as_bool)
}

fn rch_command(value: &JsonValue) -> Option<String> {
    rch_string(value, "command_text").or_else(|| {
        let command = value.get("command")?.as_array()?;
        let argv = command
            .iter()
            .map(JsonValue::as_str)
            .collect::<Option<Vec<_>>>()?;
        normalized_non_empty(Some(&argv.join(" ")))
    })
}

fn rch_finished_at(value: &JsonValue) -> Option<String> {
    rch_string(value, "completed_at")
        .or_else(|| rch_string(value, "finished_at"))
        .or_else(|| rch_string(value, "generated_at"))
}

fn rch_workspace_fingerprint(value: &JsonValue) -> Option<String> {
    let git_tree = rch_string(value, "git_tree");
    let dirty_status_hash = rch_string(value, "dirty_status_hash");

    match (git_tree, dirty_status_hash) {
        (Some(tree), Some(dirty)) => Some(format!("git_tree:{tree};dirty_status:{dirty}")),
        (Some(tree), None) => Some(format!("git_tree:{tree}")),
        (None, Some(dirty)) => Some(format!("dirty_status:{dirty}")),
        (None, None) => rch_string(value, "workspace_fingerprint"),
    }
}

fn rch_toolchain(value: &JsonValue) -> Option<String> {
    let runtime = value.get("rch_runtime");
    let client_version = runtime
        .and_then(|raw| raw.get("client_version"))
        .and_then(JsonValue::as_str)
        .and_then(|raw| normalized_non_empty(Some(raw)))
        .or_else(|| rch_string(value, "client_version"));
    let daemon_version = runtime
        .and_then(|raw| raw.get("daemon_version"))
        .and_then(JsonValue::as_str)
        .and_then(|raw| normalized_non_empty(Some(raw)))
        .or_else(|| rch_string(value, "daemon_version"));

    match (client_version, daemon_version) {
        (Some(client), Some(daemon)) => Some(format!("rch client={client} daemon={daemon}")),
        (Some(client), None) => Some(format!("rch client={client}")),
        (None, Some(daemon)) => Some(format!("rch daemon={daemon}")),
        (None, None) => None,
    }
}

fn rch_degraded_codes(value: &JsonValue) -> Vec<String> {
    let mut codes = Vec::new();
    for key in [
        "degraded_codes",
        "source_state_degraded_codes",
        "worker_state_degraded_codes",
        "error_codes",
        "degraded",
    ] {
        collect_rch_codes(value.get(key), &mut codes);
    }
    collect_rch_codes(
        value.get("error").and_then(|error| error.get("code")),
        &mut codes,
    );
    codes.sort();
    codes.dedup();
    codes
}

fn collect_rch_codes(value: Option<&JsonValue>, codes: &mut Vec<String>) {
    match value {
        Some(JsonValue::Array(values)) => {
            for item in values {
                collect_rch_codes(Some(item), codes);
            }
        }
        Some(JsonValue::Object(object)) => {
            collect_rch_codes(object.get("code"), codes);
            collect_rch_codes(object.get("reason_code"), codes);
        }
        Some(JsonValue::String(raw)) => {
            if let Some(code) = normalized_non_empty(Some(raw)) {
                codes.push(code);
            }
        }
        _ => {}
    }
}

fn rch_verification_status(
    raw_status: Option<&str>,
    exit_code: Option<i32>,
    degraded_codes: &[String],
) -> VerificationStatus {
    let normalized_status = raw_status.unwrap_or_default().trim().to_ascii_lowercase();
    if rch_status_is_fallback(&normalized_status, degraded_codes) {
        return VerificationStatus::FallbackDetected;
    }
    if matches!(
        normalized_status.as_str(),
        "interrupted" | "cancelled" | "canceled" | "timeout" | "timed_out"
    ) {
        return VerificationStatus::Interrupted;
    }
    if rch_status_is_blocked(&normalized_status, degraded_codes) {
        return VerificationStatus::Blocked;
    }
    if normalized_status == "remote_pass"
        || normalized_status == "passed"
        || normalized_status == "pass"
        || (exit_code == Some(0) && normalized_status.is_empty())
    {
        return VerificationStatus::Passed;
    }
    if normalized_status == "remote_failure"
        || normalized_status == "failed"
        || normalized_status == "failure"
        || exit_code.is_some_and(|code| code != 0)
    {
        return VerificationStatus::Failed;
    }
    VerificationStatus::Unknown
}

fn rch_status_is_fallback(raw_status: &str, degraded_codes: &[String]) -> bool {
    raw_status.contains("fallback")
        || degraded_codes
            .iter()
            .any(|code| code.contains("local_fallback") || code.contains("fallback_detected"))
}

fn rch_status_is_blocked(raw_status: &str, degraded_codes: &[String]) -> bool {
    raw_status.contains("blocked")
        || raw_status.contains("refused")
        || raw_status.contains("unsupported")
        || raw_status.contains("environment_failure")
        || raw_status.contains("admission_denied")
        || degraded_codes.iter().any(|code| {
            matches!(
                code.as_str(),
                "rch_verify_build_admission_unavailable"
                    | "rch_verify_cargo_path_dependency_version_blocked"
                    | "rch_verify_cargo_workspace_inheritance_blocked"
                    | "rch_verify_capacity_or_timeout"
                    | "rch_verify_client_daemon_version_skew"
                    | "rch_verify_committed_tree_path_deps_unsupported"
                    | "rch_verify_remote_marker_missing"
                    | "rch_verify_topology_blocked"
                    | "rch_verify_worker_disk_full"
            ) || code.starts_with("rch_verify_dirty_")
                || code.contains("version_skew")
                || code.contains("path_dependency")
                || code.contains("workspace_inheritance")
                || code.contains("topology_blocked")
        })
}

fn rch_fallback_reason(value: &JsonValue, degraded_codes: &[String]) -> Option<String> {
    rch_string(value, "fallback_reason").or_else(|| {
        degraded_codes
            .iter()
            .find(|code| code.contains("local_fallback") || code.contains("fallback_detected"))
            .cloned()
    })
}

fn rch_offload(
    value: &JsonValue,
    worker: Option<&str>,
    fallback_reason: Option<&str>,
    degraded_codes: &[String],
) -> VerificationOffload {
    if fallback_reason.is_some() || rch_status_is_fallback("", degraded_codes) {
        return VerificationOffload::rch_fallback(worker, fallback_reason);
    }

    let remote_required = rch_bool(value, "remote_required").unwrap_or(true);
    let would_offload = rch_bool(value, "would_offload").unwrap_or(worker.is_some());
    if remote_required || would_offload || worker.is_some() {
        VerificationOffload::rch_required(worker)
    } else {
        VerificationOffload::local()
    }
}

fn rch_artifacts(value: &JsonValue) -> Vec<VerificationArtifactRef> {
    let mut artifacts = Vec::new();
    if let Some(hash) = rch_string(value, "source_manifest_hash") {
        artifacts.push(VerificationArtifactRef::new(
            "rch:source_manifest",
            "source_manifest",
            Some(&hash),
        ));
    }
    artifacts
}

fn rch_selector_admission(value: &JsonValue) -> Option<VerificationSelectorAdmission> {
    let probe = value.get("selector_admission_probe")?.as_object()?;
    let schema = probe.get("schema")?.as_str()?;
    if schema != RCH_SELECTOR_ADMISSION_PROBE_SCHEMA_V1 {
        return None;
    }
    Some(VerificationSelectorAdmission {
        status: probe
            .get("status")
            .and_then(JsonValue::as_str)
            .and_then(|raw| normalized_non_empty(Some(raw))),
        required_runtime: probe
            .get("required_runtime")
            .and_then(JsonValue::as_str)
            .and_then(|raw| normalized_non_empty(Some(raw))),
        workers_reported: rch_string_array(probe.get("workers_reported")),
        daemon_workers_reported: rch_string_array(probe.get("daemon_workers_reported")),
        selected_worker: probe
            .get("selected_worker")
            .and_then(JsonValue::as_str)
            .and_then(|raw| normalized_non_empty(Some(raw))),
        selection_failure_reason: probe
            .get("selection_failure_reason")
            .and_then(JsonValue::as_str)
            .and_then(|raw| normalized_non_empty(Some(raw))),
        workers_vs_selection_contradiction: probe
            .get("workers_vs_selection_contradiction")
            .and_then(JsonValue::as_bool)
            .unwrap_or(false),
        path_normalization_warning: probe
            .get("path_normalization_warning")
            .and_then(JsonValue::as_str)
            .and_then(|raw| normalized_non_empty(Some(raw))),
        remote_required: probe
            .get("remote_required")
            .and_then(JsonValue::as_bool)
            .unwrap_or(false),
        local_fallback_refused: probe
            .get("local_fallback_refused")
            .and_then(JsonValue::as_bool)
            .unwrap_or(false),
    })
}

fn rch_string_array(value: Option<&JsonValue>) -> Vec<String> {
    let Some(JsonValue::Array(items)) = value else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(JsonValue::as_str)
        .filter_map(|raw| normalized_non_empty(Some(raw)))
        .collect()
}

fn gha_string(value: &JsonValue, key: &str) -> Option<String> {
    value.get(key).and_then(|raw| match raw {
        JsonValue::String(text) => normalized_non_empty(Some(text)),
        JsonValue::Number(number) => Some(number.to_string()),
        _ => None,
    })
}

fn gha_u64(value: &JsonValue, key: &str) -> Option<u64> {
    value.get(key).and_then(JsonValue::as_u64)
}

fn gha_workflow_name(value: &JsonValue) -> Option<String> {
    value
        .get("workflow")
        .and_then(|workflow| workflow.get("name"))
        .and_then(JsonValue::as_str)
        .and_then(|name| normalized_non_empty(Some(name)))
        .or_else(|| {
            value
                .get("workflow_run")
                .and_then(|run| run.get("name"))
                .and_then(JsonValue::as_str)
                .and_then(|name| normalized_non_empty(Some(name)))
        })
}

fn gha_run_id(value: &JsonValue) -> Option<String> {
    gha_string(value, "run_id").or_else(|| {
        value
            .get("workflow_run")
            .and_then(|run| run.get("id"))
            .and_then(|raw| match raw {
                JsonValue::String(text) => normalized_non_empty(Some(text)),
                JsonValue::Number(number) => Some(number.to_string()),
                _ => None,
            })
    })
}

fn gha_head_sha(value: &JsonValue) -> Option<String> {
    value
        .get("check_suite")
        .and_then(|suite| suite.get("head_sha"))
        .and_then(JsonValue::as_str)
        .and_then(|sha| normalized_non_empty(Some(sha)))
        .or_else(|| {
            value
                .get("workflow_run")
                .and_then(|run| run.get("head_sha"))
                .and_then(JsonValue::as_str)
                .and_then(|sha| normalized_non_empty(Some(sha)))
        })
}

fn gha_repository_full_name(value: &JsonValue) -> Option<String> {
    value
        .get("repository")
        .and_then(|repo| repo.get("full_name"))
        .and_then(JsonValue::as_str)
        .and_then(|repo| normalized_non_empty(Some(repo)))
}

fn gha_command_summary(
    workflow: Option<&str>,
    name: &str,
    run_id: Option<&str>,
    head_sha: Option<&str>,
) -> String {
    let mut parts = vec![format!("check_name={}", summary_token(name))];
    if let Some(workflow) = workflow {
        parts.push(format!("workflow={}", summary_token(workflow)));
    }
    if let Some(run_id) = run_id {
        parts.push(format!("run_id={}", summary_token(run_id)));
    }
    if let Some(head_sha) = head_sha {
        parts.push(format!("head_sha={}", summary_token(head_sha)));
    }
    format!("github_actions_check_run {}", parts.join(" "))
}

fn gha_gate_name(workflow: Option<&str>, name: &str) -> String {
    workflow.map_or_else(
        || format!("github_actions:{name}"),
        |workflow| format!("github_actions:{workflow}:{name}"),
    )
}

fn gha_toolchain(
    value: &JsonValue,
    workflow: Option<&str>,
    run_id: Option<&str>,
) -> Option<String> {
    if workflow.is_none() && run_id.is_none() && gha_string(value, "run_attempt").is_none() {
        return None;
    }
    let mut parts = Vec::new();
    if let Some(workflow) = workflow {
        parts.push(format!("workflow={}", summary_token(workflow)));
    }
    if let Some(run_id) = run_id {
        parts.push(format!("run_id={}", summary_token(run_id)));
    }
    if let Some(run_attempt) = gha_string(value, "run_attempt") {
        parts.push(format!("run_attempt={}", summary_token(&run_attempt)));
    }
    Some(format!("github_actions {}", parts.join(" ")))
}

fn gha_verification_status(status: Option<&str>, conclusion: Option<&str>) -> VerificationStatus {
    let normalized_status = status.unwrap_or_default().trim().to_ascii_lowercase();
    let normalized_conclusion = conclusion.unwrap_or_default().trim().to_ascii_lowercase();
    match normalized_conclusion.as_str() {
        "success" => VerificationStatus::Passed,
        "failure" | "startup_failure" => VerificationStatus::Failed,
        "cancelled" | "canceled" | "timed_out" => VerificationStatus::Interrupted,
        "action_required" | "neutral" | "skipped" | "stale" => VerificationStatus::Blocked,
        // Pending GitHub checks are non-final evidence, not terminal interruption.
        "" if matches!(
            normalized_status.as_str(),
            "queued" | "in_progress" | "waiting"
        ) =>
        {
            VerificationStatus::Unknown
        }
        "" => VerificationStatus::Unknown,
        _ => VerificationStatus::Unknown,
    }
}

fn gha_exit_code(status: VerificationStatus, conclusion: Option<&str>) -> Option<i32> {
    match (status, conclusion.unwrap_or_default()) {
        (VerificationStatus::Passed, _) => Some(0),
        (VerificationStatus::Failed, _) => Some(1),
        _ => None,
    }
}

fn gha_output_summary(value: &JsonValue, conclusion: Option<&str>) -> VerificationOutputSummary {
    let title = value
        .get("output")
        .and_then(|output| output.get("title"))
        .and_then(JsonValue::as_str)
        .and_then(|title| normalized_non_empty(Some(title)))
        .or_else(|| gha_string(value, "summary"));
    let mut summary = Vec::new();
    if let Some(conclusion) = conclusion {
        summary.push(format!("conclusion={}", summary_token(conclusion)));
    }
    if let Some(title) = title.as_deref() {
        summary.push(format!("title={}", summary_token(title)));
    }

    VerificationOutputSummary {
        stdout_tail: None,
        stderr_tail: (!summary.is_empty()).then(|| summary.join("; ")),
        redacted: true,
    }
}

fn gha_artifacts(value: &JsonValue) -> Vec<VerificationArtifactRef> {
    let mut artifacts = Vec::new();
    for (key, kind) in [
        ("html_url", "ci_url"),
        ("details_url", "ci_details_url"),
        ("logs_url", "ci_logs_url"),
    ] {
        if let Some(url) = gha_string(value, key) {
            artifacts.push(VerificationArtifactRef::new(
                &format!("github_actions:{key}"),
                kind,
                Some(&hash_str(&url)),
            ));
        }
    }
    artifacts
}

fn summary_token(value: &str) -> String {
    value
        .chars()
        .map(|ch| match ch {
            '`' | '$' | '(' | ')' | '\n' | '\r' | '\t' => '_',
            _ => ch,
        })
        .collect()
}

fn record_matches_request(
    record: &VerificationRunRecord,
    request: &VerificationReuseRequest<'_>,
) -> bool {
    record.command_hash == request.command_hash
        && record.execution_substrate == request.execution_substrate
        && match request.source_hash {
            Some(source_hash) => record.source_hash.as_deref() == Some(source_hash),
            None => true,
        }
}

fn broker_exact_match(
    record: &VerificationRunRecord,
    request: &VerificationBrokerViewRequest<'_>,
) -> bool {
    broker_command_match(record, request)
        && match request.source_hash {
            Some(source_hash) => record.source_hash.as_deref() == Some(source_hash),
            None => true,
        }
        && broker_env_match(record, request)
}

fn broker_env_match(
    record: &VerificationRunRecord,
    request: &VerificationBrokerViewRequest<'_>,
) -> bool {
    match (
        request.env_fingerprint_class,
        record.cargo_target_dir_hash_or_class.as_deref(),
    ) {
        (Some(req), Some(rec)) => req == rec,
        (None, _) => true,
        (Some(_), None) => false,
    }
}

fn broker_command_match(
    record: &VerificationRunRecord,
    request: &VerificationBrokerViewRequest<'_>,
) -> bool {
    record.command_hash == request.command_hash
        && record.execution_substrate == request.execution_substrate
}

fn sorted_flags(flags: &[&str]) -> Vec<String> {
    let mut flags = flags
        .iter()
        .filter_map(|flag| normalized_non_empty(Some(flag)))
        .collect::<Vec<_>>();
    flags.sort();
    flags.dedup();
    flags
}

fn sorted_owned_codes(codes: Vec<&str>) -> Vec<String> {
    let mut codes = codes.into_iter().map(str::to_owned).collect::<Vec<_>>();
    codes.sort();
    codes.dedup();
    codes
}

fn sorted_owner_candidates(candidates: &[&str]) -> Vec<String> {
    let mut candidates = candidates
        .iter()
        .filter_map(|candidate| normalized_non_empty(Some(candidate)))
        .collect::<Vec<_>>();
    candidates.sort();
    candidates.dedup();
    candidates
}

fn compact_short_message(message: &str) -> String {
    let one_line = message.split_whitespace().collect::<Vec<_>>().join(" ");
    one_line.chars().take(200).collect()
}

fn redaction_safe_source_path(path: &str) -> String {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return "unknown".to_owned();
    }
    if trimmed.starts_with('/') || trimmed.contains('\\') {
        return format!("path_hash:{}", hash_str(trimmed));
    }
    trimmed.trim_start_matches("./").to_owned()
}

fn compile_blocker_cache_key(
    source_tree_fingerprint: Option<&str>,
    command_class: &str,
    first_source_path: &str,
) -> String {
    hash_json_array(&[
        source_tree_fingerprint.unwrap_or("class:unknown_source"),
        command_class,
        first_source_path,
    ])
}

fn coordination_target(entry: &CompileBlockerCacheEntry) -> Option<String> {
    entry
        .reservation_holder
        .clone()
        .or_else(|| entry.owner_candidates.first().cloned())
        .or_else(|| entry.agent_name.clone())
}

fn compile_blocker_suggested_action(
    status: CompileBlockerCacheStatus,
    entry: &CompileBlockerCacheEntry,
    coordination_target: &Option<String>,
) -> String {
    match status {
        CompileBlockerCacheStatus::StaleSourceTree | CompileBlockerCacheStatus::StaleSourceFile => {
            "rerun_current_source".to_owned()
        }
        CompileBlockerCacheStatus::Current => coordination_target.as_ref().map_or_else(
            || "claim_unowned_blocker".to_owned(),
            |target| {
                if entry.reservation_holder.as_ref() == Some(target) {
                    "message_reservation_holder".to_owned()
                } else {
                    "message_owner_candidate".to_owned()
                }
            },
        ),
    }
}

fn broker_failure_ref(
    status: VerificationBrokerStatus,
    matched: Option<&VerificationRunRecord>,
) -> Option<VerificationFirstFailureSummaryRef> {
    if status != VerificationBrokerStatus::KnownBlocker {
        return None;
    }
    matched.map(|record| VerificationFirstFailureSummaryRef {
        stderr_excerpt_hash: record.stderr_excerpt_hash.clone(),
        artifact_manifest_hash: record.artifact_manifest_hash.clone(),
        retained_log_path_hash: record.retained_log_path_hash.clone(),
        raw_output_included: false,
    })
}

fn broker_request<'a>(
    source_hash: Option<&'a str>,
    command_hash: &'a str,
) -> VerificationBrokerViewRequest<'a> {
    broker_request_for_substrate(source_hash, command_hash, "cargo_test", "rch")
}

fn broker_request_for_substrate<'a>(
    source_hash: Option<&'a str>,
    command_hash: &'a str,
    command_class: &'a str,
    execution_substrate: &'a str,
) -> VerificationBrokerViewRequest<'a> {
    VerificationBrokerViewRequest {
        bead_id: Some("bd-example"),
        source_hash,
        command_hash,
        command_class,
        normalized_argv_hash: "blake3:broker-argv",
        execution_substrate,
        env_fingerprint_class: Some("class:external_cargo_target"),
        target_profile: Some("debug"),
    }
}

fn sample_proof_broker_fingerprint(
    source_tree_fingerprint: Option<&str>,
    command_hash: &str,
    normalized_argv_hash: &str,
    env_fingerprint_class: Option<&str>,
    rch_runtime_class: Option<&str>,
    local_cargo_tripwire_class: Option<&str>,
) -> ProofBrokerFingerprint {
    proof_broker_fingerprint(ProofBrokerFingerprintInput {
        bead_id: Some("bd-1n3x1.1"),
        command_class: "cargo_test",
        command_hash,
        normalized_argv_hash,
        source_tree_fingerprint,
        source_materialization: Some("git_worktree"),
        dirty_status_hash: Some("blake3:dirty-status-clean"),
        env_fingerprint_class,
        target_profile: Some("debug"),
        execution_substrate: "rch",
        rch_runtime_class,
        worker_requirement: Some("required_runtime:rust"),
        local_cargo_tripwire_class,
        build_admission_posture: Some("remote_required_no_local_fallback"),
    })
}

fn sample_verification_broker_records() -> Vec<VerificationRunRecord> {
    let mut records = sample_verification_run_records();
    records.push(VerificationRunRecord::from_input(VerificationRunInput {
        run_id: Some("vrun_failed_000000000000000001"),
        bead_id: Some("bd-example"),
        agent_name: Some("RubyWolf"),
        source_hash: Some("blake3:source"),
        command_hash: Some("blake3:failed-command"),
        command_argv: &["cargo", "test", "failed"],
        cargo_target_dir: Some("/Volumes/USBNVME16TB/temp_agent_space/rch-target-failed"),
        execution_substrate: "rch",
        worker_host: Some("css"),
        started_at: Some("2026-05-15T05:02:00Z"),
        finished_at: Some("2026-05-15T05:02:42Z"),
        exit_code: Some(101),
        stdout_hash: Some("blake3:stdout"),
        stderr_excerpt: None,
        artifact_manifest_hash: Some("blake3:manifest-failed"),
        retained_log_path: None,
        provenance: vec![VerificationRunProvenance {
            source: "j1_jsonl".to_owned(),
            event_kind: "artifact_manifest".to_owned(),
            line: Some(4),
        }],
    }));
    records.push(VerificationRunRecord::from_input(VerificationRunInput {
        run_id: Some("vrun_in_flight_00000000000001"),
        bead_id: Some("bd-example"),
        agent_name: Some("NobleStork"),
        source_hash: Some("blake3:source"),
        command_hash: Some("blake3:in-flight-command"),
        command_argv: &["cargo", "test", "in-flight"],
        cargo_target_dir: Some("/Volumes/USBNVME16TB/temp_agent_space/rch-target-in-flight"),
        execution_substrate: "rch",
        worker_host: Some("csd"),
        started_at: Some("2026-05-15T05:03:00Z"),
        finished_at: None,
        exit_code: None,
        stdout_hash: None,
        stderr_excerpt: None,
        artifact_manifest_hash: Some("blake3:manifest-in-flight"),
        retained_log_path: Some("/tmp/in-flight-log.jsonl"),
        provenance: vec![VerificationRunProvenance {
            source: "j1_jsonl".to_owned(),
            event_kind: "artifact_manifest".to_owned(),
            line: Some(6),
        }],
    }));
    records
}

fn reuse_repair_actions(
    status: VerificationReuseStatus,
    matched: Option<&VerificationRunRecord>,
    request: &VerificationReuseRequest<'_>,
) -> Vec<VerificationReuseRepairAction> {
    match status {
        VerificationReuseStatus::ReusablePass => vec![VerificationReuseRepairAction {
            priority: 1,
            kind: "cite_existing_run".to_owned(),
            command: None,
            message: "cite the matched passing verification run instead of rerunning".to_owned(),
        }],
        VerificationReuseStatus::ReusableFail => vec![VerificationReuseRepairAction {
            priority: 1,
            kind: "inspect_failure".to_owned(),
            command: None,
            message: "inspect the matched failing run before launching another verification"
                .to_owned(),
        }],
        VerificationReuseStatus::InFlight => {
            let agent = matched.and_then(|record| record.agent_name.as_deref());
            vec![VerificationReuseRepairAction {
                priority: 1,
                kind: "wait_for_agent".to_owned(),
                command: None,
                message: agent.map_or_else(
                    || "wait for the in-flight verification run to finish".to_owned(),
                    |agent| format!("wait for {agent}'s in-flight verification run to finish"),
                ),
            }]
        }
        VerificationReuseStatus::StaleSource => vec![VerificationReuseRepairAction {
            priority: 1,
            kind: "rerun_with_current_source".to_owned(),
            command: Some(rch_rerun_hint(request)),
            message: "source fingerprint changed; rerun with remote-required verification"
                .to_owned(),
        }],
        VerificationReuseStatus::MismatchedCommand => vec![VerificationReuseRepairAction {
            priority: 1,
            kind: "broaden_or_narrow_command".to_owned(),
            command: None,
            message: "source evidence exists, but the requested command fingerprint differs"
                .to_owned(),
        }],
        VerificationReuseStatus::MissingEvidence => vec![VerificationReuseRepairAction {
            priority: 1,
            kind: "import_retained_j1_log".to_owned(),
            command: None,
            message: "import a retained J1 JSONL log or run the requested verifier".to_owned(),
        }],
        VerificationReuseStatus::RerunRequired => vec![VerificationReuseRepairAction {
            priority: 1,
            kind: "rerun_with_rch_required".to_owned(),
            command: Some(rch_rerun_hint(request)),
            message: "no equivalent run was found; rerun without allowing local Cargo fallback"
                .to_owned(),
        }],
    }
}

fn rch_rerun_hint(request: &VerificationReuseRequest<'_>) -> String {
    format!(
        "RCH_REQUIRE_REMOTE=1 rch exec -- <command with command_hash={}>",
        request.command_hash
    )
}

fn command_summary(record: &VerificationRunRecord) -> String {
    format!(
        "{} command_hash={} argv_hash={}",
        record.execution_substrate, record.command_hash, record.command_argv_hash
    )
}

fn run_record_gate_name(record: &VerificationRunRecord) -> String {
    let substrate = record.execution_substrate.trim();
    if run_record_uses_rch(record) {
        "rch verification run".to_owned()
    } else if run_record_is_local_cargo(record) {
        "local cargo verification run".to_owned()
    } else if substrate.contains("static") {
        "static verification run".to_owned()
    } else if substrate.is_empty() || substrate == "unknown" {
        "verification run".to_owned()
    } else {
        format!("{substrate} verification run")
    }
}

fn run_record_status(record: &VerificationRunRecord) -> VerificationStatus {
    if run_record_is_local_cargo(record) {
        return VerificationStatus::FallbackDetected;
    }

    match record.exit_code {
        Some(0) => VerificationStatus::Passed,
        Some(_) => VerificationStatus::Failed,
        None if record.finished_at.is_none() => VerificationStatus::Interrupted,
        None => VerificationStatus::Unknown,
    }
}

fn run_record_offload(record: &VerificationRunRecord) -> VerificationOffload {
    if run_record_is_local_cargo(record) {
        return VerificationOffload::rch_fallback(
            record.worker_host.as_deref(),
            Some(
                "verification run executed on local Cargo; remote-required Cargo gate is unverified",
            ),
        );
    }

    if run_record_uses_rch(record) {
        VerificationOffload::rch_required(record.worker_host.as_deref())
    } else {
        VerificationOffload::local()
    }
}

fn run_record_output_summary(record: &VerificationRunRecord) -> VerificationOutputSummary {
    VerificationOutputSummary {
        stdout_tail: record
            .stdout_hash
            .as_ref()
            .map(|hash| format!("stdout_hash={hash}")),
        stderr_tail: record
            .stderr_excerpt_hash
            .as_ref()
            .map(|hash| format!("stderr_excerpt_hash={hash}")),
        redacted: record.stdout_hash.is_some() || record.stderr_excerpt_hash.is_some(),
    }
}

fn run_record_artifacts(record: &VerificationRunRecord) -> Vec<VerificationArtifactRef> {
    let mut artifacts = Vec::new();
    if let Some(hash) = record.artifact_manifest_hash.as_deref() {
        artifacts.push(VerificationArtifactRef::new(
            "artifact_manifest_hash",
            "artifact_manifest",
            Some(hash),
        ));
    }
    if let Some(hash) = record.retained_log_path_hash.as_deref() {
        artifacts.push(VerificationArtifactRef::new(
            "retained_log_path_hash",
            "retained_log",
            Some(hash),
        ));
    }
    artifacts
}

fn run_record_uses_rch(record: &VerificationRunRecord) -> bool {
    matches!(
        record.execution_substrate.as_str(),
        "rch" | "remote_rch" | "rch_remote"
    )
}

fn run_record_is_local_cargo(record: &VerificationRunRecord) -> bool {
    matches!(
        record.execution_substrate.as_str(),
        "local_cargo" | "cargo_local" | "local-cargo"
    )
}

#[derive(Clone, Debug)]
struct PendingJ1Command {
    line: usize,
    command_argv: Vec<String>,
    finished_at: Option<String>,
    exit_code: Option<i32>,
    stdout_hash: Option<String>,
    stderr_excerpt: Option<String>,
}

impl PendingJ1Command {
    fn from_event(line: usize, event: &JsonValue) -> Self {
        let command = event
            .get("command")
            .and_then(JsonValue::as_str)
            .unwrap_or("unknown")
            .to_owned();
        let mut command_argv = vec![command];
        if let Some(args) = event.get("args").and_then(JsonValue::as_array) {
            command_argv.extend(args.iter().filter_map(JsonValue::as_str).map(str::to_owned));
        }
        Self {
            line,
            command_argv,
            finished_at: event
                .get("ts")
                .and_then(JsonValue::as_str)
                .map(str::to_owned),
            exit_code: event
                .get("exit_code")
                .and_then(JsonValue::as_i64)
                .and_then(|code| i32::try_from(code).ok()),
            stdout_hash: event
                .get("stdout_hash")
                .and_then(JsonValue::as_str)
                .map(str::to_owned),
            stderr_excerpt: event
                .get("stderr_excerpt")
                .and_then(JsonValue::as_str)
                .map(str::to_owned),
        }
    }
}

fn run_record_from_artifact_manifest_event(
    line: usize,
    event: &JsonValue,
    command: Option<&PendingJ1Command>,
) -> Result<VerificationRunRecord, VerificationRunImportError> {
    let fields = event
        .get("fields")
        .and_then(JsonValue::as_object)
        .ok_or(VerificationRunImportError::MissingArtifactManifest { line })?;
    let manifest_schema = field_str(fields, "manifest_schema");
    if manifest_schema != Some("ee.test_artifact_manifest.v1") {
        return Err(VerificationRunImportError::MissingArtifactManifest { line });
    }
    let artifact_manifest_hash = field_str(fields, "artifact_manifest_hash")
        .ok_or(VerificationRunImportError::MissingArtifactManifest { line })?;
    let command_argv = command
        .map(|command| command.command_argv.clone())
        .unwrap_or_else(|| {
            let count = field_str(fields, "command_arg_count")
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or(0);
            vec![format!("artifact_manifest_arg_count={count}")]
        });
    let command_argv_refs = command_argv.iter().map(String::as_str).collect::<Vec<_>>();
    let provenance_line = command.map_or(line, |command| command.line);

    Ok(VerificationRunRecord::from_input(VerificationRunInput {
        run_id: None,
        bead_id: field_str(fields, "bead_id"),
        agent_name: field_str(fields, "agent_name"),
        source_hash: field_str(fields, "source_hash"),
        command_hash: field_str(fields, "command_hash"),
        command_argv: &command_argv_refs,
        cargo_target_dir: field_str(fields, "target_directory"),
        execution_substrate: field_str(fields, "execution_substrate").unwrap_or("unknown"),
        worker_host: field_str(fields, "worker_host"),
        started_at: None,
        finished_at: command
            .and_then(|command| command.finished_at.as_deref())
            .or_else(|| event.get("ts").and_then(JsonValue::as_str)),
        exit_code: command.and_then(|command| command.exit_code),
        stdout_hash: command.and_then(|command| command.stdout_hash.as_deref()),
        stderr_excerpt: command.and_then(|command| command.stderr_excerpt.as_deref()),
        artifact_manifest_hash: Some(artifact_manifest_hash),
        retained_log_path: field_str(fields, "log_path"),
        provenance: vec![
            VerificationRunProvenance {
                source: "j1_jsonl".to_owned(),
                event_kind: "command_end".to_owned(),
                line: command.map(|command| command.line),
            },
            VerificationRunProvenance {
                source: "j1_jsonl".to_owned(),
                event_kind: "artifact_manifest".to_owned(),
                line: Some(provenance_line.max(line)),
            },
        ],
    }))
}

fn reject_raw_output_fields(
    event: &JsonValue,
    line: usize,
) -> Result<(), VerificationRunImportError> {
    for field in ["stdout", "stderr", "stdout_bytes", "stderr_bytes"] {
        if event.get(field).is_some() {
            return Err(VerificationRunImportError::RawOutputRejected {
                line,
                field: field.to_owned(),
            });
        }
    }
    if let Some(fields) = event.get("fields").and_then(JsonValue::as_object) {
        for field in ["stdout", "stderr", "stdout_bytes", "stderr_bytes"] {
            if fields.contains_key(field) {
                return Err(VerificationRunImportError::RawOutputRejected {
                    line,
                    field: format!("fields.{field}"),
                });
            }
        }
    }
    Ok(())
}

fn field_str<'a>(fields: &'a serde_json::Map<String, JsonValue>, key: &str) -> Option<&'a str> {
    fields.get(key).and_then(JsonValue::as_str)
}

fn hash_str(value: &str) -> String {
    format!("blake3:{}", blake3::hash(value.as_bytes()).to_hex())
}

fn hash_json_array(values: &[&str]) -> String {
    let mut bytes = Vec::new();
    for value in values {
        bytes.extend_from_slice(value.len().to_string().as_bytes());
        bytes.push(b':');
        bytes.extend_from_slice(value.as_bytes());
        bytes.push(0);
    }
    format!("blake3:{}", blake3::hash(&bytes).to_hex())
}

fn stable_proof_broker_fingerprint_id(fingerprint: &ProofBrokerFingerprint) -> String {
    let digest = hash_json_array(&[
        PROOF_BROKER_SCHEMA_V1,
        fingerprint.bead_id.as_deref().unwrap_or(""),
        fingerprint.command_class.as_str(),
        fingerprint.command_hash.as_str(),
        fingerprint.normalized_argv_hash.as_str(),
        fingerprint.source_tree_fingerprint.as_str(),
        fingerprint.source_materialization.as_str(),
        fingerprint.dirty_status_hash.as_str(),
        fingerprint.env_fingerprint_class.as_str(),
        fingerprint.target_profile.as_deref().unwrap_or(""),
        fingerprint.execution_substrate.as_str(),
        fingerprint.rch_runtime_class.as_str(),
        fingerprint.worker_requirement.as_str(),
        fingerprint.local_cargo_tripwire_class.as_str(),
        fingerprint.build_admission_posture.as_str(),
    ]);
    let hex = digest.strip_prefix("blake3:").unwrap_or(digest.as_str());
    format!("proof_{}", &hex[..26])
}

fn stable_proof_broker_row_id(
    fingerprint_id: &str,
    state: ProofBrokerLedgerState,
    run_id: Option<&str>,
    created_at: &str,
) -> String {
    let digest = hash_json_array(&[
        PROOF_BROKER_SCHEMA_V1,
        fingerprint_id,
        state.as_str(),
        run_id.unwrap_or(""),
        created_at,
    ]);
    let hex = digest.strip_prefix("blake3:").unwrap_or(digest.as_str());
    format!("proof_row_{}", &hex[..26])
}

fn stable_rch_verification_id(
    command_hash: &str,
    finished_at: Option<&str>,
    exit_code: Option<i32>,
) -> String {
    let payload = format!(
        "{RCH_VERIFY_SCHEMA_V1}\0{command_hash}\0{}\0{}",
        finished_at.unwrap_or(""),
        exit_code.map_or_else(|| "none".to_owned(), |code| code.to_string())
    );
    let digest = blake3::hash(payload.as_bytes()).to_hex().to_string();
    format!("ver_rch_{}", &digest[..26])
}

fn stable_github_actions_verification_id(
    command_hash: &str,
    completed_at: Option<&str>,
    conclusion: Option<&str>,
) -> String {
    let payload = format!(
        "{GITHUB_ACTIONS_CHECK_RUN_SCHEMA_V1}\0{command_hash}\0{}\0{}",
        completed_at.unwrap_or(""),
        conclusion.unwrap_or("")
    );
    let digest = blake3::hash(payload.as_bytes()).to_hex().to_string();
    format!("ver_gha_{}", &digest[..26])
}

fn stable_run_verification_id(record: &VerificationRunRecord) -> String {
    let payload = format!(
        "{VERIFICATION_RUN_SCHEMA_V1}\0{}\0{}\0{}\0{}",
        record.run_id,
        record.command_hash,
        record.finished_at.as_deref().unwrap_or(""),
        record
            .exit_code
            .map_or_else(|| "none".to_owned(), |code| code.to_string())
    );
    let digest = blake3::hash(payload.as_bytes()).to_hex().to_string();
    format!("ver_run_{}", &digest[..26])
}

fn stable_run_id(command_hash: &str, finished_at: Option<&str>, exit_code: Option<i32>) -> String {
    let payload = format!(
        "{}\n{}\n{}",
        command_hash,
        finished_at.unwrap_or(""),
        exit_code.map_or_else(String::new, |code| code.to_string())
    );
    let hex = blake3::hash(payload.as_bytes()).to_hex().to_string();
    format!("vrun_{}", &hex[..26])
}

fn classify_or_hash_path(path: &str) -> String {
    let path = path.trim();
    if path.contains("/temp_agent_space/") || path.starts_with("/Volumes/USBNVME16TB/") {
        "class:external_cargo_target".to_owned()
    } else if path == "target" || path.ends_with("/target") || path.contains("/target/") {
        "class:cargo_target".to_owned()
    } else {
        hash_str(path)
    }
}

fn assess_requirement(
    requirement: &VerificationGateRequirement,
    records: &[VerificationEvidenceRecord],
) -> VerificationGateAssessment {
    let matched = records
        .iter()
        .rev()
        .find(|record| requirement_matches(requirement, record));

    let Some(record) = matched else {
        return VerificationGateAssessment {
            gate_name: requirement.gate_name.clone(),
            command_contains: requirement.command_contains.clone(),
            requires_remote: requirement.requires_remote,
            satisfied: false,
            matched_verification_id: None,
            matched_status: None,
            reason: "no matching verification evidence recorded".to_owned(),
        };
    };

    let satisfied = record_satisfies_requirement(requirement, record);
    VerificationGateAssessment {
        gate_name: requirement.gate_name.clone(),
        command_contains: requirement.command_contains.clone(),
        requires_remote: requirement.requires_remote,
        satisfied,
        matched_verification_id: Some(record.verification_id.clone()),
        matched_status: Some(record.status),
        reason: if satisfied {
            "authoritative pass evidence recorded".to_owned()
        } else {
            rejection_reason(requirement, record)
        },
    }
}

fn requirement_matches(
    requirement: &VerificationGateRequirement,
    record: &VerificationEvidenceRecord,
) -> bool {
    if record.gate_name == requirement.gate_name {
        return true;
    }

    requirement
        .command_contains
        .as_ref()
        .is_some_and(|fragment| record.command.contains(fragment))
}

fn record_satisfies_requirement(
    requirement: &VerificationGateRequirement,
    record: &VerificationEvidenceRecord,
) -> bool {
    record.is_authoritative_pass()
        && (!requirement.requires_remote
            || (record.offload.required_remote && !record.offload.fallback_detected))
}

fn rejection_reason(
    requirement: &VerificationGateRequirement,
    record: &VerificationEvidenceRecord,
) -> String {
    if record.offload.fallback_detected || record.status == VerificationStatus::FallbackDetected {
        return "matching evidence detected local fallback; remote-required gate is unverified"
            .to_owned();
    }
    if record.status == VerificationStatus::Blocked {
        return "matching evidence is blocked".to_owned();
    }
    if record.status == VerificationStatus::Interrupted {
        return "matching evidence was interrupted before completion".to_owned();
    }
    if record.status == VerificationStatus::Failed {
        return format!(
            "matching evidence failed with exitCode={}",
            record
                .exit_code
                .map_or_else(|| "null".to_owned(), |code| code.to_string())
        );
    }
    if record.status == VerificationStatus::Passed && record.exit_code != Some(0) {
        return "matching pass evidence lacks exitCode=0".to_owned();
    }
    if requirement.requires_remote && !record.offload.required_remote {
        return "matching pass evidence was local but this gate requires remote evidence"
            .to_owned();
    }

    format!("matching evidence has status={}", record.status.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    const VERIFICATION_EVIDENCE_GOLDEN: &str = include_str!(
        "../../tests/fixtures/golden/models/verification_evidence_records.json.golden"
    );
    const VERIFICATION_RUN_GOLDEN: &str =
        include_str!("../../tests/fixtures/golden/verification/run_records.json.golden");
    const VERIFICATION_CLOSEOUT_CAPSULE_GOLDEN: &str =
        include_str!("../../tests/fixtures/golden/verification/closeout_capsules.json.golden");

    #[test]
    fn verification_evidence_records_match_golden_fixture() -> TestResult {
        let json = serde_json::to_string(&sample_verification_evidence_records())?;
        assert_eq!(json, VERIFICATION_EVIDENCE_GOLDEN.trim_end_matches('\n'));
        Ok(())
    }

    #[test]
    fn verification_run_records_match_golden_fixture() -> TestResult {
        let json = serde_json::to_string(&sample_verification_run_records())?;
        assert_eq!(json, VERIFICATION_RUN_GOLDEN.trim_end_matches('\n'));
        Ok(())
    }

    #[test]
    fn verification_closeout_capsules_match_golden_fixture() -> TestResult {
        let json = serde_json::to_string(&sample_verification_closeout_capsules())?;
        assert_eq!(
            json,
            VERIFICATION_CLOSEOUT_CAPSULE_GOLDEN.trim_end_matches('\n')
        );
        Ok(())
    }

    #[test]
    fn fallback_detected_is_never_an_authoritative_pass() -> TestResult {
        let records = sample_verification_evidence_records();
        let fallback = records
            .iter()
            .find(|record| record.status == VerificationStatus::FallbackDetected)
            .ok_or_else(|| std::io::Error::other("sample records include fallback_detected"))?;

        assert!(!fallback.is_authoritative_pass());
        assert!(fallback.offload.required_remote);
        assert!(fallback.offload.fallback_detected);
        assert_eq!(fallback.exit_code, Some(0));
        Ok(())
    }

    #[test]
    fn remote_required_blocked_record_has_no_exit_code() -> TestResult {
        let records = sample_verification_evidence_records();
        let blocked = records
            .iter()
            .find(|record| record.status == VerificationStatus::Blocked)
            .ok_or_else(|| std::io::Error::other("sample records include blocked"))?;

        assert!(!blocked.is_authoritative_pass());
        assert!(blocked.offload.required_remote);
        assert_eq!(blocked.exit_code, None);
        Ok(())
    }

    #[test]
    fn closure_guidance_rejects_fallback_cargo_evidence() {
        let records = sample_verification_evidence_records();
        let requirements = vec![VerificationGateRequirement::new(
            "cargo test producer",
            Some("cargo test --lib producer"),
            true,
        )];

        let guidance = verification_closure_guidance(Some("bd-example"), &requirements, &records);

        assert!(!guidance.can_close);
        assert!(!guidance.assessments[0].satisfied);
        assert_eq!(
            guidance.assessments[0].matched_status,
            Some(VerificationStatus::FallbackDetected)
        );
        assert_eq!(
            guidance.rejected_reasons,
            vec![
                "cargo test producer: matching evidence detected local fallback; remote-required gate is unverified"
                    .to_owned()
            ]
        );
    }

    #[test]
    fn closure_guidance_accepts_authoritative_remote_pass() {
        let producer = ProducerMetadata::unknown_agent(
            ProducerSourceSystem::Verification,
            Some("verify-run-pass"),
            None,
            Some("repo:abc"),
            Some("2026-05-13T01:00:00Z"),
        );
        let record = VerificationEvidenceRecord::from_input(VerificationEvidenceInput {
            verification_id: "ver_remote_pass",
            bead_id: Some("bd-example"),
            gate_name: "cargo test",
            command: "RCH_REQUIRE_REMOTE=1 rch exec -- cargo test",
            status: VerificationStatus::Passed,
            exit_code: Some(0),
            started_at: Some("2026-05-13T01:00:00Z"),
            finished_at: Some("2026-05-13T01:01:00Z"),
            duration_ms: Some(60_000),
            environment: VerificationEnvironment::new(Some("repo:abc"), Some("/repo"), None),
            offload: VerificationOffload::rch_required(Some("css")),
            output_summary: VerificationOutputSummary::empty(),
            artifacts: Vec::new(),
            producer,
        });
        let requirements = vec![VerificationGateRequirement::new(
            "cargo test",
            Some("cargo test"),
            true,
        )];

        let guidance = verification_closure_guidance(Some("bd-example"), &requirements, &[record]);

        assert!(guidance.can_close);
        assert!(guidance.rejected_reasons.is_empty());
        assert!(guidance.assessments[0].satisfied);
    }

    #[test]
    fn verification_run_record_maps_to_canonical_rch_evidence() -> TestResult {
        let run = VerificationRunRecord::from_input(VerificationRunInput {
            run_id: Some("vrun_rch_bridge"),
            bead_id: Some("bd-1nxz4.5"),
            agent_name: Some("ChartreuseHawk"),
            source_hash: Some("git_tree:tree123"),
            command_hash: Some("sha256:j1-command"),
            command_argv: &["cargo", "test", "--lib", "rch_verify"],
            cargo_target_dir: Some("/Volumes/USBNVME16TB/temp_agent_space/cargo-target"),
            execution_substrate: "rch",
            worker_host: Some("vmi123"),
            started_at: Some("2026-05-19T05:00:00Z"),
            finished_at: Some("2026-05-19T05:00:42Z"),
            exit_code: Some(0),
            stdout_hash: Some("blake3:stdout"),
            stderr_excerpt: Some("remote worker passed"),
            artifact_manifest_hash: Some("blake3:manifest"),
            retained_log_path: Some("/tmp/verify-log.jsonl"),
            provenance: vec![VerificationRunProvenance {
                source: "j1_jsonl".to_owned(),
                event_kind: "artifact_manifest".to_owned(),
                line: Some(2),
            }],
        });

        let record = verification_evidence_record_from_run_record(&run);

        assert_eq!(record.schema, VERIFICATION_EVIDENCE_SCHEMA_V1);
        assert!(record.verification_id.starts_with("ver_run_"));
        assert_eq!(record.bead_id.as_deref(), Some("bd-1nxz4.5"));
        assert_eq!(record.gate_name, "rch verification run");
        assert_eq!(record.command_hash, "sha256:j1-command");
        assert!(record.command.contains("command_hash=sha256:j1-command"));
        assert_eq!(record.status, VerificationStatus::Passed);
        assert!(record.is_authoritative_pass());
        assert!(record.offload.required_remote);
        assert_eq!(record.offload.worker.as_deref(), Some("vmi123"));
        assert_eq!(
            record.environment.workspace_fingerprint.as_deref(),
            Some("git_tree:tree123")
        );
        assert_eq!(
            record.environment.toolchain.as_deref(),
            Some("class:external_cargo_target")
        );
        assert_eq!(record.artifacts.len(), 2);

        let encoded = serde_json::to_string(&record)?;
        assert!(!encoded.contains("remote worker passed"));
        assert!(!encoded.contains("/tmp/verify-log.jsonl"));
        Ok(())
    }

    #[test]
    fn local_cargo_run_record_maps_to_fallback_evidence() {
        let run = VerificationRunRecord::from_input(VerificationRunInput {
            run_id: Some("vrun_local_cargo"),
            bead_id: Some("bd-1nxz4.5"),
            agent_name: Some("ChartreuseHawk"),
            source_hash: Some("git_tree:tree123"),
            command_hash: Some("sha256:local-cargo"),
            command_argv: &["cargo", "test", "--lib", "rch_verify"],
            cargo_target_dir: Some("target"),
            execution_substrate: "local_cargo",
            worker_host: None,
            started_at: Some("2026-05-19T05:00:00Z"),
            finished_at: Some("2026-05-19T05:00:42Z"),
            exit_code: Some(0),
            stdout_hash: None,
            stderr_excerpt: None,
            artifact_manifest_hash: Some("blake3:manifest"),
            retained_log_path: None,
            provenance: Vec::new(),
        });

        let record = verification_evidence_record_from_run_record(&run);

        assert_eq!(record.status, VerificationStatus::FallbackDetected);
        assert!(!record.is_authoritative_pass());
        assert!(record.offload.required_remote);
        assert!(record.offload.fallback_detected);
        assert_eq!(record.gate_name, "local cargo verification run");
    }

    #[test]
    fn verification_evidence_beads_summary_is_shell_safe() {
        let producer = ProducerMetadata::unknown_agent(
            ProducerSourceSystem::Verification,
            Some("verify-run-danger"),
            None,
            Some("repo:abc$(leak)"),
            Some("2026-05-19T07:10:00Z"),
        );
        let mut record = VerificationEvidenceRecord::from_input(VerificationEvidenceInput {
            verification_id: "ver_$(touch pwned)",
            bead_id: Some("bd-1nxz4.5`oops`"),
            gate_name: "cargo test $(launch)",
            command: "cargo test --lib $(cat secret)",
            status: VerificationStatus::Blocked,
            exit_code: None,
            started_at: Some("2026-05-19T07:00:00Z"),
            finished_at: None,
            duration_ms: None,
            environment: VerificationEnvironment::new(
                Some("git_tree:abc$(oops)"),
                Some("/repo"),
                None,
            ),
            offload: VerificationOffload::rch_fallback(
                Some("vmi`123`"),
                Some("$(launch remote) && retry"),
            ),
            output_summary: VerificationOutputSummary::redacted(Some("$(raw log)")),
            artifacts: Vec::new(),
            producer,
        });
        record.command_hash = "sha256:$(unsafe)`hash`".to_owned();

        let summary = verification_evidence_beads_summary(&record);

        assert!(summary.contains("schema=ee.verification.evidence.v1"));
        assert!(summary.contains("status=blocked"));
        assert!(summary.contains("result_class=environment_blocker"));
        assert!(summary.contains("raw_output_included=false"));
        assert!(!summary.contains("cargo test --lib"));
        assert!(!summary.contains("raw log"));
        assert!(!summary.contains('$'));
        assert!(!summary.contains('`'));
        assert!(!summary.contains('('));
        assert!(!summary.contains(')'));
        assert!(!summary.contains('\n'));
    }

    #[test]
    fn github_actions_success_maps_to_canonical_ci_evidence() -> TestResult {
        let proof = serde_json::json!({
            "schema": GITHUB_ACTIONS_CHECK_RUN_SCHEMA_V1,
            "name": "verify",
            "workflow_name": "CI",
            "run_id": 26077398253_u64,
            "run_attempt": 1,
            "check_run_id": 123456_u64,
            "repository": "Dicklesworthstone/eidetic_engine_cli",
            "head_sha": "9e02dd424891623d2b5049525b2762a9376e41f9",
            "status": "completed",
            "conclusion": "success",
            "started_at": "2026-05-19T06:00:00Z",
            "completed_at": "2026-05-19T06:10:00Z",
            "html_url": "https://github.example/checks/123",
            "details_url": "https://github.example/details/123",
            "output": {
                "title": "verify passed",
                "text": "raw log text must not be copied into the evidence summary"
            }
        });

        let record = verification_evidence_record_from_github_actions_check_run(&proof)?;

        assert_eq!(record.schema, VERIFICATION_EVIDENCE_SCHEMA_V1);
        assert!(record.verification_id.starts_with("ver_gha_"));
        assert_eq!(record.gate_name, "github_actions:CI:verify");
        assert_eq!(record.status, VerificationStatus::Passed);
        assert_eq!(record.exit_code, Some(0));
        assert!(record.is_authoritative_pass());
        assert_eq!(
            record.offload.offload_tool.as_deref(),
            Some("github_actions")
        );
        assert_eq!(
            record.environment.workspace_fingerprint.as_deref(),
            Some("github_head_sha:9e02dd424891623d2b5049525b2762a9376e41f9")
        );
        assert_eq!(record.artifacts.len(), 2);

        let encoded = serde_json::to_string(&record)?;
        assert!(!encoded.contains("raw log text"));
        assert!(encoded.contains("verify passed"));
        Ok(())
    }

    #[test]
    fn github_actions_failure_maps_to_failed_ci_evidence() -> TestResult {
        let proof = serde_json::json!({
            "name": "verify",
            "workflow_run": {
                "id": 26077398253_u64,
                "name": "CI",
                "head_sha": "9e02dd424891623d2b5049525b2762a9376e41f9"
            },
            "check_suite": {
                "head_sha": "9e02dd424891623d2b5049525b2762a9376e41f9"
            },
            "status": "completed",
            "conclusion": "failure",
            "completed_at": "2026-05-19T06:10:00Z",
            "html_url": "https://github.example/checks/124",
            "output": {
                "title": "verify failed"
            }
        });

        let record = verification_evidence_record_from_github_actions_check_run(&proof)?;

        assert_eq!(record.status, VerificationStatus::Failed);
        assert_eq!(record.exit_code, Some(1));
        assert!(!record.is_authoritative_pass());
        assert_eq!(record.producer.run.run_id.as_deref(), Some("26077398253"));
        Ok(())
    }

    #[test]
    fn github_actions_pending_maps_to_unknown_non_authoritative_evidence() -> TestResult {
        let proof = serde_json::json!({
            "schema": GITHUB_ACTIONS_CHECK_RUN_SCHEMA_V1,
            "name": "verify",
            "workflow_name": "CI",
            "run_id": 27050712596_u64,
            "check_run_id": 125_u64,
            "repository": "Dicklesworthstone/eidetic_engine_cli",
            "head_sha": "eb2e05e25a66b9b002caf62212f205c6d05a6b67",
            "status": "queued",
            "created_at": "2026-06-06T02:59:09Z",
            "html_url": "https://github.example/checks/125",
            "output": {
                "title": "verify queued",
                "text": "raw pending log text must not be copied"
            }
        });

        let record = verification_evidence_record_from_github_actions_check_run(&proof)?;

        assert_eq!(record.status, VerificationStatus::Unknown);
        assert_eq!(record.exit_code, None);
        assert!(!record.is_authoritative_pass());
        assert_eq!(record.finished_at, None);
        assert_eq!(
            record.environment.workspace_fingerprint.as_deref(),
            Some("github_head_sha:eb2e05e25a66b9b002caf62212f205c6d05a6b67")
        );

        let encoded = serde_json::to_string(&record)?;
        assert!(!encoded.contains("raw pending log"));
        assert!(encoded.contains("verify queued"));
        Ok(())
    }

    #[test]
    fn github_actions_cancelled_maps_to_interrupted_evidence() -> TestResult {
        let proof = serde_json::json!({
            "name": "verify",
            "workflow_run": {
                "id": 27050680484_u64,
                "name": "CI",
                "head_sha": "53773bb107a83334560dfcf1c126d9c8a5518033"
            },
            "status": "completed",
            "conclusion": "cancelled",
            "completed_at": "2026-06-06T02:59:11Z",
            "html_url": "https://github.example/checks/126"
        });

        let record = verification_evidence_record_from_github_actions_check_run(&proof)?;

        assert_eq!(record.status, VerificationStatus::Interrupted);
        assert_eq!(record.exit_code, None);
        assert!(!record.is_authoritative_pass());
        assert_eq!(record.producer.run.run_id.as_deref(), Some("27050680484"));
        Ok(())
    }

    #[test]
    fn rch_verify_remote_pass_maps_to_canonical_pass_record() -> TestResult {
        let proof = serde_json::json!({
            "schema": RCH_VERIFY_SCHEMA_V1,
            "command_text": "cargo test --lib verification",
            "command_hash": "sha256:remote-pass",
            "command_kind": "cargo_test",
            "status": "remote_pass",
            "exit_code": 0,
            "bead_id": "bd-1nxz4.5",
            "worker_id": "vmi123",
            "remote_required": true,
            "started_at": "2026-05-19T04:00:00Z",
            "completed_at": "2026-05-19T04:00:09Z",
            "elapsed_ms": 9000,
            "git_tree": "tree123",
            "dirty_status_hash": "dirty456",
            "remote_project_root": "/data/projects/eidetic_engine_cli",
            "source_manifest_hash": "blake3:manifest",
            "stdout_tail": "test result: ok",
            "selector_admission_probe": {
                "schema": "ee.rch.selector_admission_probe.v1",
                "status": "selected",
                "required_runtime": "Rust",
                "workers_reported": ["vmi123", "vmi456"],
                "daemon_workers_reported": ["vmi123"],
                "selected_worker": "vmi123",
                "selection_failure_reason": null,
                "workers_vs_selection_contradiction": false,
                "path_normalization_warning": null,
                "remote_required": true,
                "local_fallback_refused": false
            },
            "rch_runtime": {
                "client_version": "1.0.24",
                "daemon_version": "0.1.3"
            }
        });

        let record = verification_evidence_record_from_rch_verify(&proof)?;

        assert_eq!(record.schema, VERIFICATION_EVIDENCE_SCHEMA_V1);
        assert_eq!(record.bead_id.as_deref(), Some("bd-1nxz4.5"));
        assert_eq!(record.gate_name, "cargo_test");
        assert_eq!(record.command, "cargo test --lib verification");
        assert_eq!(record.command_hash, "sha256:remote-pass");
        assert_eq!(record.status, VerificationStatus::Passed);
        assert_eq!(record.exit_code, Some(0));
        assert!(record.is_authoritative_pass());
        assert!(record.offload.required_remote);
        assert!(!record.offload.fallback_detected);
        assert_eq!(record.offload.worker.as_deref(), Some("vmi123"));
        let selector = record
            .selector_admission
            .as_ref()
            .ok_or_else(|| "selector admission probe should parse".to_owned())?;
        assert_eq!(selector.status.as_deref(), Some("selected"));
        assert_eq!(selector.required_runtime.as_deref(), Some("Rust"));
        assert_eq!(selector.workers_reported, ["vmi123", "vmi456"]);
        assert_eq!(selector.daemon_workers_reported, ["vmi123"]);
        assert_eq!(selector.selected_worker.as_deref(), Some("vmi123"));
        assert_eq!(selector.selection_failure_reason, None);
        assert!(!selector.workers_vs_selection_contradiction);
        assert!(selector.remote_required);
        assert!(!selector.local_fallback_refused);
        assert_eq!(
            record.environment.workspace_fingerprint.as_deref(),
            Some("git_tree:tree123;dirty_status:dirty456")
        );
        assert_eq!(
            record.environment.toolchain.as_deref(),
            Some("rch client=1.0.24 daemon=0.1.3")
        );
        assert_eq!(record.artifacts.len(), 1);
        Ok(())
    }

    #[test]
    fn rch_verify_environment_failure_maps_to_blocked() -> TestResult {
        let proof = serde_json::json!({
            "schema": RCH_VERIFY_SCHEMA_V1,
            "command": ["cargo", "check", "--all-targets"],
            "command_hash": "sha256:blocked",
            "status": "rch_environment_failure",
            "exit_code": 1,
            "degraded_codes": ["rch_verify_client_daemon_version_skew"],
            "stderr_tail": "client/daemon version skew"
        });

        let record = verification_evidence_record_from_rch_verify(&proof)?;

        assert_eq!(record.command, "cargo check --all-targets");
        assert_eq!(record.status, VerificationStatus::Blocked);
        assert!(!record.is_authoritative_pass());
        assert!(record.offload.required_remote);
        Ok(())
    }

    #[test]
    fn rch_verify_selector_admission_failure_maps_to_typed_metadata() -> TestResult {
        let proof = serde_json::json!({
            "schema": RCH_VERIFY_SCHEMA_V1,
            "command_text": "cargo test --lib selector_probe",
            "command_hash": "sha256:selector-blocked",
            "status": "rch_environment_failure",
            "exit_code": 1,
            "remote_required": true,
            "worker_id": null,
            "degraded_codes": [
                "rch_verify_remote_command_failed",
                "rch_verify_local_fallback_refused",
                "rch_verify_capacity_or_timeout",
                "rch_verify_remote_marker_missing"
            ],
            "selector_admission_probe": {
                "schema": "ee.rch.selector_admission_probe.v1",
                "status": "selection_failed",
                "required_runtime": "Rust",
                "workers_reported": ["vmi1227854", "vmi1264463"],
                "daemon_workers_reported": ["vmi1227854"],
                "selected_worker": null,
                "selection_failure_reason": "no_workers_with_rust_installed",
                "workers_vs_selection_contradiction": true,
                "path_normalization_warning": "[RCH] project root normalization warning: canonical /Users/<redacted>/projects/eidetic_engine_cli -> /data/projects/eidetic_engine_cli",
                "remote_required": true,
                "local_fallback_refused": true
            }
        });

        let record = verification_evidence_record_from_rch_verify(&proof)?;

        assert_eq!(record.status, VerificationStatus::Blocked);
        assert!(!record.is_authoritative_pass());
        assert!(record.offload.required_remote);
        assert!(!record.offload.fallback_detected);
        let selector = record
            .selector_admission
            .as_ref()
            .ok_or_else(|| "selector admission probe should parse".to_owned())?;
        assert_eq!(selector.status.as_deref(), Some("selection_failed"));
        assert_eq!(
            selector.selection_failure_reason.as_deref(),
            Some("no_workers_with_rust_installed")
        );
        assert_eq!(selector.workers_reported, ["vmi1227854", "vmi1264463"]);
        assert_eq!(selector.daemon_workers_reported, ["vmi1227854"]);
        assert!(selector.workers_vs_selection_contradiction);
        assert!(selector.remote_required);
        assert!(selector.local_fallback_refused);
        assert!(
            selector
                .path_normalization_warning
                .as_deref()
                .is_some_and(|warning| warning.contains("/Users/<redacted>"))
        );

        let summary = verification_evidence_beads_summary(&record);
        assert!(summary.contains("selector_admission=selection_failed"));
        assert!(summary.contains("selector_failure_reason=no_workers_with_rust_installed"));
        assert!(summary.contains("selector_local_fallback_refused=true"));
        Ok(())
    }

    #[test]
    fn rch_verify_remote_compile_failure_maps_to_failed_not_blocked() -> TestResult {
        let proof = serde_json::json!({
            "schema": RCH_VERIFY_SCHEMA_V1,
            "command_text": "cargo test --lib search::simhash",
            "command_hash": "sha256:compile-failure",
            "status": "remote_failure",
            "exit_code": 101,
            "degraded_codes": ["rch_verify_remote_command_failed"],
            "stderr_tail": "error[E0425]: cannot find value"
        });

        let record = verification_evidence_record_from_rch_verify(&proof)?;

        assert_eq!(record.status, VerificationStatus::Failed);
        assert_eq!(record.exit_code, Some(101));
        assert!(!record.is_authoritative_pass());
        assert!(!record.offload.fallback_detected);
        Ok(())
    }

    #[test]
    fn rch_verify_local_fallback_maps_to_fallback_detected() -> TestResult {
        let proof = serde_json::json!({
            "schema": RCH_VERIFY_SCHEMA_V1,
            "command_text": "cargo test --lib explain_latency",
            "command_hash": "sha256:fallback",
            "status": "local_fallback",
            "exit_code": 0,
            "remote_required": true,
            "would_offload": false,
            "degraded_codes": ["rch_verify_local_fallback"],
            "fallback_reason": "remote marker missing"
        });

        let record = verification_evidence_record_from_rch_verify(&proof)?;

        assert_eq!(record.status, VerificationStatus::FallbackDetected);
        assert_eq!(record.exit_code, Some(0));
        assert!(!record.is_authoritative_pass());
        assert!(record.offload.required_remote);
        assert!(record.offload.fallback_detected);
        assert_eq!(
            record.offload.fallback_reason.as_deref(),
            Some("remote marker missing")
        );
        Ok(())
    }

    #[test]
    fn verification_evidence_deserializes_old_records_without_selector_admission() -> TestResult {
        let sample = sample_verification_evidence_records()
            .into_iter()
            .find(|record| record.status == VerificationStatus::Blocked)
            .ok_or_else(|| "blocked sample exists".to_owned())?;
        let mut raw = serde_json::to_value(sample)
            .map_err(|error| format!("serialize sample record: {error}"))?;
        raw.as_object_mut()
            .ok_or_else(|| "sample record serializes as object".to_owned())?
            .remove("selectorAdmission");

        let record: VerificationEvidenceRecord = serde_json::from_value(raw)
            .map_err(|error| format!("deserialize old record: {error}"))?;
        assert!(record.selector_admission.is_none());
        Ok(())
    }

    #[test]
    fn rch_verify_ignores_selector_admission_probe_with_wrong_schema() -> TestResult {
        let proof = serde_json::json!({
            "schema": RCH_VERIFY_SCHEMA_V1,
            "command_text": "cargo test --lib selector_probe",
            "command_hash": "sha256:selector-wrong-schema",
            "status": "rch_environment_failure",
            "exit_code": 1,
            "remote_required": true,
            "degraded_codes": ["rch_verify_local_fallback_refused"],
            "selector_admission_probe": {
                "schema": "ee.rch.selector_admission_probe.v0",
                "status": "selection_failed",
                "required_runtime": "Rust",
                "workers_reported": ["vmi1227854"],
                "daemon_workers_reported": ["vmi1227854"],
                "selected_worker": null,
                "selection_failure_reason": "no_workers_with_rust_installed",
                "workers_vs_selection_contradiction": true,
                "path_normalization_warning": null,
                "remote_required": true,
                "local_fallback_refused": true
            }
        });

        let record = verification_evidence_record_from_rch_verify(&proof)?;

        assert!(record.selector_admission.is_none());
        Ok(())
    }

    #[test]
    fn rch_verify_rejects_duplicate_underscore_schema() {
        let proof = serde_json::json!({
            "schema": "ee.verification_evidence.v1",
            "command_text": "cargo test",
            "command_hash": "sha256:duplicate"
        });

        let error =
            verification_evidence_record_from_rch_verify(&proof).expect_err("schema is rejected");

        assert_eq!(
            error,
            RchVerificationEvidenceParseError::UnexpectedSchema {
                found: "ee.verification_evidence.v1".to_owned()
            }
        );
    }
}
