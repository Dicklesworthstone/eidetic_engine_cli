//! Session handoff and resume capsule operations (EE-HANDOFF-001).
//!
//! Create compact, provenance-rich state transfer capsules for agent continuity.
//! Capsules capture workspace state, evidence, objectives, and next actions
//! without being backups, raw exports, or diagnostic bundles.
//!
//! # Operations
//!
//! - **preview**: Plan capsule contents without writing
//! - **create**: Write a redacted continuity capsule
//! - **inspect**: Validate an existing capsule
//! - **resume**: Render next-agent payload from a capsule

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::{DateTime, Utc};
use ring::hmac;
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::core::focus::{focus_state_hash, read_active_focus_state};
use crate::core::singleflight::singleflight_posture_report;
use crate::core::swarm_brief::{
    collect_swarm_brief_summary, render_swarm_brief_summary_for_handoff,
    swarm_brief_summary_evidence_id,
};
use crate::core::task_frame::{
    NON_EXECUTING_CONTRACT, TaskFrameRecord, TaskFrameShowOptions, show_task_frame,
};
use crate::db::{CreateAuditInput, DbConnection, audit_actions, generate_audit_id};
use crate::models::{DomainError, RedactionLevel, SingleFlightPostureReport};
#[cfg(test)]
use crate::models::{
    SingleFlightLastKeyPosture, SingleFlightSurface, SingleFlightSurfaceCounters,
    SingleFlightSurfacePosture,
};

/// Schema for handoff capsule format.
pub const HANDOFF_CAPSULE_SCHEMA_V1: &str = "ee.handoff.capsule.v1";

/// Schema for handoff preview report.
pub const HANDOFF_PREVIEW_SCHEMA_V1: &str = "ee.handoff.preview.v1";

/// Schema for handoff create report.
pub const HANDOFF_CREATE_SCHEMA_V1: &str = "ee.handoff.create.v1";

/// Schema for handoff inspect report.
pub const HANDOFF_INSPECT_SCHEMA_V1: &str = "ee.handoff.inspect.v1";

/// Schema for handoff resume report.
pub const HANDOFF_RESUME_SCHEMA_V1: &str = "ee.handoff.resume.v1";

/// Schema for handoff HMAC key rotation report.
pub const HANDOFF_ROTATE_KEY_SCHEMA_V1: &str = "ee.handoff.rotate_key.v1";

/// Degraded code fired when a handoff capsule's captured memory set is stale.
pub const HANDOFF_SNAPSHOT_STALE_CODE: &str = "handoff_snapshot_stale";

/// Error code emitted when a capsule has no integrity block.
pub const HANDOFF_HMAC_MISSING_CODE: &str = "handoff_hmac_missing";

/// Error code emitted when signed capsule data does not verify.
pub const HANDOFF_CAPSULE_TAMPERED_CODE: &str = "handoff_capsule_tampered";

/// Error code emitted when a machine-bound capsule cannot verify here.
pub const HANDOFF_CAPSULE_MACHINE_MISMATCH_CODE: &str = "handoff_capsule_machine_mismatch";

/// Degraded code emitted when a caller explicitly bypasses HMAC checks.
pub const HANDOFF_HMAC_SKIPPED_CODE: &str = "handoff_hmac_skipped";

/// Error code emitted when strict mode cannot find the machine-local salt.
pub const STRICT_MODE_NO_SALT_FILE_CODE: &str = "strict_mode_no_salt_file";

const HANDOFF_INTEGRITY_SCHEMA_V1: &str = "ee.handoff.integrity.v1";
const HANDOFF_HMAC_ALGORITHM: &str = "hmac-sha256";
const HANDOFF_HMAC_KEY_MODE_WORKSPACE_SECRET: &str = "workspace_secret";
const HANDOFF_HMAC_KEY_MODE_MACHINE_BOUND: &str = "workspace_secret_machine_bound";
const HANDOFF_WORKSPACE_SECRET_FILE: &str = "handoff_hmac_key";
const HANDOFF_MACHINE_SALT_FILE: &str = "handoff_machine_salt";

/// ID prefix for handoff capsules.
pub const HANDOFF_CAPSULE_ID_PREFIX: &str = "hcap_";

// ============================================================================
// Capsule Profile
// ============================================================================

/// Profile controlling capsule contents and size.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapsuleProfile {
    /// Optimized for small next prompt (minimal content).
    Compact,
    /// Optimized for immediate continuation by same agent.
    #[default]
    Resume,
    /// Optimized for complete transfer to different agent.
    Handoff,
}

impl CapsuleProfile {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Compact => "compact",
            Self::Resume => "resume",
            Self::Handoff => "handoff",
        }
    }

    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "compact" => Some(Self::Compact),
            "resume" => Some(Self::Resume),
            "handoff" => Some(Self::Handoff),
            _ => None,
        }
    }

    #[must_use]
    pub const fn max_sections(self) -> usize {
        match self {
            Self::Compact => 4,
            Self::Resume => 8,
            Self::Handoff => 12,
        }
    }

    #[must_use]
    pub const fn include_full_evidence(self) -> bool {
        matches!(self, Self::Handoff)
    }
}

// ============================================================================
// Evidence Confidence
// ============================================================================

/// Confidence level for included evidence.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceConfidence {
    /// Backed by explicit durable evidence.
    Verified,
    /// Inferred from context but not explicitly stored.
    Inferred,
    /// Based on stale or low-confidence sources.
    Stale,
    /// Explicitly marked as unknown.
    Unknown,
}

impl EvidenceConfidence {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Verified => "verified",
            Self::Inferred => "inferred",
            Self::Stale => "stale",
            Self::Unknown => "unknown",
        }
    }
}

// ============================================================================
// Capsule Section
// ============================================================================

/// A section included in the handoff capsule.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CapsuleSection {
    /// Section identifier.
    pub id: String,
    /// Section title.
    pub title: String,
    /// Evidence confidence level.
    pub confidence: EvidenceConfidence,
    /// Content of the section.
    pub content: String,
    /// Source evidence IDs.
    pub evidence_ids: Vec<String>,
    /// Estimated token count.
    pub token_estimate: usize,
}

impl CapsuleSection {
    #[must_use]
    pub fn new(id: impl Into<String>, title: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            confidence: EvidenceConfidence::Unknown,
            content: String::new(),
            evidence_ids: Vec::new(),
            token_estimate: 0,
        }
    }

    #[must_use]
    pub fn with_content(mut self, content: impl Into<String>) -> Self {
        let content_str = content.into();
        self.token_estimate = estimate_tokens(&content_str);
        self.content = content_str;
        self
    }

    #[must_use]
    pub fn with_confidence(mut self, confidence: EvidenceConfidence) -> Self {
        self.confidence = confidence;
        self
    }

    #[must_use]
    pub fn with_evidence(mut self, evidence_ids: Vec<String>) -> Self {
        self.evidence_ids = evidence_ids;
        self
    }
}

// ============================================================================
// Next Action
// ============================================================================

/// A recommended next action for the resuming agent.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NextAction {
    /// Action priority (lower is higher priority).
    pub priority: u8,
    /// Action description.
    pub description: String,
    /// Suggested command if applicable.
    pub suggested_command: Option<String>,
    /// Reason for this action.
    pub reason: String,
    /// Evidence supporting this action.
    pub evidence_ids: Vec<String>,
}

impl NextAction {
    #[must_use]
    pub fn new(priority: u8, description: impl Into<String>) -> Self {
        Self {
            priority,
            description: description.into(),
            suggested_command: None,
            reason: String::new(),
            evidence_ids: Vec::new(),
        }
    }

    #[must_use]
    pub fn with_command(mut self, command: impl Into<String>) -> Self {
        self.suggested_command = Some(command.into());
        self
    }

    #[must_use]
    pub fn with_reason(mut self, reason: impl Into<String>) -> Self {
        self.reason = reason.into();
        self
    }
}

// ============================================================================
// Blocker
// ============================================================================

/// A blocker preventing progress.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Blocker {
    /// Blocker identifier.
    pub id: String,
    /// Blocker description.
    pub description: String,
    /// Suggested resolution.
    pub resolution: Option<String>,
    /// Whether this is a hard blocker.
    pub hard: bool,
}

impl Blocker {
    #[must_use]
    pub fn new(id: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            description: description.into(),
            resolution: None,
            hard: false,
        }
    }

    #[must_use]
    pub fn with_resolution(mut self, resolution: impl Into<String>) -> Self {
        self.resolution = Some(resolution.into());
        self
    }

    #[must_use]
    pub fn hard(mut self) -> Self {
        self.hard = true;
        self
    }
}

// ============================================================================
// Do Not Repeat
// ============================================================================

/// Guidance on what not to repeat.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DoNotRepeat {
    /// Pattern or action to avoid.
    pub pattern: String,
    /// Reason to avoid it.
    pub reason: String,
    /// Evidence from prior failure.
    pub evidence_id: Option<String>,
}

impl DoNotRepeat {
    #[must_use]
    pub fn new(pattern: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            pattern: pattern.into(),
            reason: reason.into(),
            evidence_id: None,
        }
    }
}

// ============================================================================
// Redaction Summary
// ============================================================================

/// Summary of redactions applied to the capsule.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RedactionSummary {
    /// Number of secrets redacted.
    pub secrets_redacted: usize,
    /// Number of paths redacted.
    pub paths_redacted: usize,
    /// Number of IDs redacted.
    pub ids_redacted: usize,
    /// Categories of redacted content.
    pub categories: Vec<String>,
}

// ============================================================================
// Degradation Info
// ============================================================================

/// Information about degraded or unavailable sources.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DegradationInfo {
    /// Degradation code.
    pub code: String,
    /// Human-readable message.
    pub message: String,
    /// Degraded severity, when the surface classifies one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub severity: Option<String>,
    /// Suggested next action.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_action: Option<String>,
}

impl DegradationInfo {
    #[must_use]
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            severity: None,
            next_action: None,
        }
    }

    #[must_use]
    pub fn with_severity(mut self, severity: impl Into<String>) -> Self {
        self.severity = Some(severity.into());
        self
    }

    #[must_use]
    pub fn with_next_action(mut self, action: impl Into<String>) -> Self {
        self.next_action = Some(action.into());
        self
    }
}

// ============================================================================
// Preview Options and Report
// ============================================================================

/// Options for previewing a handoff capsule.
#[derive(Clone, Debug)]
pub struct PreviewOptions {
    /// Workspace path.
    pub workspace: PathBuf,
    /// Capsule profile.
    pub profile: CapsuleProfile,
    /// Filter evidence since this time or run ID.
    pub since: Option<String>,
    /// Include token estimates.
    pub include_estimates: bool,
    /// Optional task-frame scope to include.
    pub task_frame_id: Option<String>,
}

impl Default for PreviewOptions {
    fn default() -> Self {
        Self {
            workspace: PathBuf::from("."),
            profile: CapsuleProfile::Resume,
            since: None,
            include_estimates: true,
            task_frame_id: None,
        }
    }
}

/// Report from previewing a handoff capsule.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PreviewReport {
    pub schema: String,
    pub workspace: PathBuf,
    pub profile: String,
    pub planned_sections: Vec<PlannedSection>,
    pub omitted_sections: Vec<OmittedSection>,
    pub evidence_ids: Vec<String>,
    pub active_focus: Option<serde_json::Value>,
    pub task_frame: Option<serde_json::Value>,
    pub swarm_brief_summary: Option<serde_json::Value>,
    pub token_estimate: usize,
    pub byte_estimate: usize,
    pub redaction_posture: String,
    pub degradations: Vec<DegradationInfo>,
    pub sufficient_for_resume: bool,
    pub generated_at: String,
}

/// A section planned for inclusion.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PlannedSection {
    pub id: String,
    pub title: String,
    pub confidence: String,
    pub evidence_count: usize,
    pub token_estimate: usize,
}

/// A section omitted from the capsule.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OmittedSection {
    pub id: String,
    pub reason: String,
}

impl PreviewReport {
    #[must_use]
    pub fn new(workspace: PathBuf, profile: CapsuleProfile) -> Self {
        Self {
            schema: HANDOFF_PREVIEW_SCHEMA_V1.to_owned(),
            workspace,
            profile: profile.as_str().to_owned(),
            planned_sections: Vec::new(),
            omitted_sections: Vec::new(),
            evidence_ids: Vec::new(),
            active_focus: None,
            task_frame: None,
            swarm_brief_summary: None,
            token_estimate: 0,
            byte_estimate: 0,
            redaction_posture: "standard".to_owned(),
            degradations: Vec::new(),
            sufficient_for_resume: false,
            generated_at: Utc::now().to_rfc3339(),
        }
    }

    #[must_use]
    pub fn to_json(&self) -> String {
        crate::core::serialize_or_error(self)
    }

    #[must_use]
    pub fn to_json_pretty(&self) -> String {
        crate::core::serialize_pretty_or_error(self)
    }
}

fn render_singleflight_posture_for_handoff(report: &SingleFlightPostureReport) -> String {
    let mut lines = vec![
        format!("Schema: {}", report.schema),
        format!("Status: {}", report.status),
        format!("Configured surfaces: {}", report.configured_surface_count),
        format!("Active leaders: {}", report.active_leader_count),
        format!("Follower waits: {}", report.follower_wait_count),
        format!("Follower timeouts: {}", report.follower_timeout_count),
        format!("Leader failures: {}", report.leader_failure_count),
        format!("Reused results: {}", report.reused_result_count),
    ];

    for surface in &report.surfaces {
        lines.push(format!(
            "- {}: status={}, configured={}, active_leaders={}, follower_waits={}, timeouts={}, failures={}, generation_counts=leaders:{} completed:{} reused:{}",
            surface.surface.as_str(),
            surface.status,
            surface.configured,
            surface.active_leader_count,
            surface.follower_join_count,
            surface.follower_timeout_count,
            surface.leader_failure_count,
            surface.leader_start_count,
            surface.completed_leader_count,
            surface.reused_result_count
        ));
        match &surface.last_key {
            Some(last_key) => lines.push(format!(
                "  last_key: key_hash={} workspace_generation={} index_generation={} graph_generation={}",
                last_key.key_hash,
                last_key.workspace_generation,
                generation_label(last_key.index_generation),
                generation_label(last_key.graph_generation)
            )),
            None => lines.push("  last_key: none".to_owned()),
        }
        lines.push(format!("  suggested_action: {}", surface.suggested_action));
    }

    lines.join("\n")
}

fn generation_label(generation: Option<u64>) -> String {
    generation.map_or_else(|| "none".to_owned(), |value| value.to_string())
}

// ============================================================================
// Create Options and Report
// ============================================================================

/// Options for creating a handoff capsule.
#[derive(Clone, Debug)]
pub struct CreateOptions {
    /// Workspace path.
    pub workspace: PathBuf,
    /// Output path for the capsule.
    pub output: PathBuf,
    /// Capsule profile.
    pub profile: CapsuleProfile,
    /// Filter evidence since this time or run ID.
    pub since: Option<String>,
    /// Whether to run in dry-run mode.
    pub dry_run: bool,
    /// Optional task-frame scope to include.
    pub task_frame_id: Option<String>,
    /// Bind the capsule HMAC to this machine's local salt.
    pub bind_to_machine: bool,
    /// Test/embedding override for the machine salt path. CLI callers
    /// leave this unset and use the platform default.
    pub machine_salt_path: Option<PathBuf>,
    /// Redaction level requested for capsule content.
    pub redaction_level: RedactionLevel,
}

impl Default for CreateOptions {
    fn default() -> Self {
        Self {
            workspace: PathBuf::from("."),
            output: PathBuf::from("handoff.json"),
            profile: CapsuleProfile::Resume,
            since: None,
            dry_run: false,
            task_frame_id: None,
            bind_to_machine: false,
            machine_salt_path: None,
            redaction_level: RedactionLevel::Standard,
        }
    }
}

/// Options for rotating a handoff capsule HMAC key.
#[derive(Clone, Debug)]
pub struct RotateKeyOptions {
    /// Workspace root selected by the CLI.
    pub workspace: PathBuf,
    /// Path to the capsule file to update in place.
    pub capsule: PathBuf,
    /// Override machine salt path for deterministic tests.
    pub machine_salt_path: Option<PathBuf>,
}

impl Default for RotateKeyOptions {
    fn default() -> Self {
        Self {
            workspace: PathBuf::from("."),
            capsule: PathBuf::new(),
            machine_salt_path: None,
        }
    }
}

/// Report from rotating a handoff capsule HMAC key.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RotateKeyReport {
    pub schema: String,
    pub capsule_id: String,
    pub capsule_path: PathBuf,
    pub key_mode: String,
    pub body_sha256: String,
    pub old_hmac_prefix: Option<String>,
    pub new_hmac_prefix: String,
    pub canonical_content_hash_before: String,
    pub canonical_content_hash_after: String,
    pub body_preserved: bool,
    pub audit_id: Option<String>,
    pub rotated_at: String,
}

impl RotateKeyReport {
    #[must_use]
    pub fn new(capsule_id: String, capsule_path: PathBuf) -> Self {
        Self {
            schema: HANDOFF_ROTATE_KEY_SCHEMA_V1.to_owned(),
            capsule_id,
            capsule_path,
            key_mode: String::new(),
            body_sha256: String::new(),
            old_hmac_prefix: None,
            new_hmac_prefix: String::new(),
            canonical_content_hash_before: String::new(),
            canonical_content_hash_after: String::new(),
            body_preserved: false,
            audit_id: None,
            rotated_at: Utc::now().to_rfc3339(),
        }
    }
}

/// Report from creating a handoff capsule.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CreateReport {
    pub schema: String,
    pub capsule_id: String,
    pub workspace: PathBuf,
    pub output_path: PathBuf,
    pub profile: String,
    pub sections_included: usize,
    pub evidence_count: usize,
    pub active_focus: Option<serde_json::Value>,
    pub task_frame: Option<serde_json::Value>,
    pub swarm_brief_summary: Option<serde_json::Value>,
    pub token_count: usize,
    pub byte_count: usize,
    pub content_hash: String,
    /// Round-trip-stable BLAKE3 hash over the capsule body with
    /// volatile fields stripped (capsule_id, created_at, audit_ids,
    /// last_accessed_at). Two creates of the same workspace state
    /// produce the same value regardless of when/who/where they ran.
    /// Bead bd-17c65.13.4 (M3).
    pub canonical_content_hash: String,
    pub redaction_summary: RedactionSummary,
    pub dry_run: bool,
    pub created_at: String,
}

impl CreateReport {
    #[must_use]
    pub fn new(capsule_id: String, workspace: PathBuf, output: PathBuf) -> Self {
        Self {
            schema: HANDOFF_CREATE_SCHEMA_V1.to_owned(),
            capsule_id,
            workspace,
            output_path: output,
            profile: CapsuleProfile::Resume.as_str().to_owned(),
            sections_included: 0,
            evidence_count: 0,
            active_focus: None,
            task_frame: None,
            swarm_brief_summary: None,
            token_count: 0,
            byte_count: 0,
            content_hash: String::new(),
            canonical_content_hash: String::new(),
            redaction_summary: RedactionSummary::default(),
            dry_run: false,
            created_at: Utc::now().to_rfc3339(),
        }
    }

    #[must_use]
    pub fn to_json(&self) -> String {
        crate::core::serialize_or_error(self)
    }

    #[must_use]
    pub fn to_json_pretty(&self) -> String {
        crate::core::serialize_pretty_or_error(self)
    }
}

// ============================================================================
// Inspect Options and Report
// ============================================================================

/// Options for inspecting a handoff capsule.
#[derive(Clone, Debug)]
pub struct InspectOptions {
    /// Path to the capsule file.
    pub path: PathBuf,
    /// Verify content hash.
    pub verify_hash: bool,
    /// Check evidence references.
    pub check_evidence: bool,
}

impl Default for InspectOptions {
    fn default() -> Self {
        Self {
            path: PathBuf::new(),
            verify_hash: true,
            check_evidence: true,
        }
    }
}

/// Validation status for capsule inspection.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValidationStatus {
    Valid,
    Invalid,
    Stale,
    Partial,
}

impl ValidationStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Valid => "valid",
            Self::Invalid => "invalid",
            Self::Stale => "stale",
            Self::Partial => "partial",
        }
    }
}

/// Report from inspecting a handoff capsule.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InspectReport {
    pub schema: String,
    pub path: PathBuf,
    pub capsule_id: String,
    pub capsule_schema: String,
    pub validation_status: String,
    pub workspace_id: Option<String>,
    pub repository_fingerprint: Option<String>,
    pub profile: String,
    pub section_count: usize,
    pub evidence_count: usize,
    pub hash_valid: bool,
    pub hash_expected: Option<String>,
    pub hash_actual: Option<String>,
    pub stale_evidence: Vec<String>,
    pub missing_evidence: Vec<String>,
    pub redaction_status: String,
    pub compatible_versions: Vec<String>,
    pub warnings: Vec<String>,
    pub inspected_at: String,
}

impl InspectReport {
    #[must_use]
    pub fn new(path: PathBuf) -> Self {
        Self {
            schema: HANDOFF_INSPECT_SCHEMA_V1.to_owned(),
            path,
            capsule_id: String::new(),
            capsule_schema: HANDOFF_CAPSULE_SCHEMA_V1.to_owned(),
            validation_status: ValidationStatus::Valid.as_str().to_owned(),
            workspace_id: None,
            repository_fingerprint: None,
            profile: CapsuleProfile::Resume.as_str().to_owned(),
            section_count: 0,
            evidence_count: 0,
            hash_valid: true,
            hash_expected: None,
            hash_actual: None,
            stale_evidence: Vec::new(),
            missing_evidence: Vec::new(),
            redaction_status: "complete".to_owned(),
            compatible_versions: vec!["1.0".to_owned()],
            warnings: Vec::new(),
            inspected_at: Utc::now().to_rfc3339(),
        }
    }

    #[must_use]
    pub fn to_json(&self) -> String {
        crate::core::serialize_or_error(self)
    }

    #[must_use]
    pub fn to_json_pretty(&self) -> String {
        crate::core::serialize_pretty_or_error(self)
    }
}

// ============================================================================
// Resume Options and Report
// ============================================================================

/// Options for resuming from a handoff capsule.
#[derive(Clone, Debug)]
pub struct ResumeOptions {
    /// Path to the capsule file (or "latest").
    pub path: PathBuf,
    /// Use latest capsule in workspace.
    pub use_latest: bool,
    /// Workspace path for latest lookup.
    pub workspace: PathBuf,
    /// Maximum sections to include.
    pub max_sections: Option<usize>,
    /// Optional task-frame scope to merge into the resume payload.
    pub task_frame_id: Option<String>,
    /// When set, the resume binds to the workspace identified by
    /// this ID and compares against the capsule's `workspace` field
    /// to surface workspace_mismatch_soft / _hard degradations.
    /// `None` skips the comparison (legacy / unbounded resume).
    /// Bead bd-17c65.13.2 (M1).
    pub bound_workspace_id: Option<String>,
    /// When set, the resume runs the four-level
    /// `WorkspaceMatch` reconciliation against the capsule's embedded
    /// `workspace_identity` block (M2 / bd-17c65.13.3). This takes
    /// precedence over `bound_workspace_id` when both are set — the
    /// structured identity carries more signal. `None` falls back to
    /// the M1 string-ID comparison.
    pub bound_workspace_identity: Option<WorkspaceIdentity>,
    /// When false, callers can suppress rendering the
    /// `prompt_fragment` markdown body to save bytes when they only
    /// want the structured data. Defaults to `true`.
    /// Bead bd-17c65.13.2 (M1).
    pub include_prompt_fragment: bool,
    /// When true, any non-zero drift between the capsule's captured
    /// workspace state and the current workspace state turns the
    /// resume into an error (`handoff_snapshot_stale`, exit code 6 —
    /// degraded). When false (default), drift surfaces as a
    /// `degraded[]` entry but the resume still completes.
    /// Bead bd-17c65.13.5 (M4).
    pub require_fresh: bool,
    /// Explicitly bypass HMAC verification for emergency recovery.
    /// Resume still emits `handoff_hmac_skipped` and records a
    /// best-effort audit row.
    pub insecure_skip_hmac: bool,
    /// Test/embedding override for the machine salt path. CLI callers
    /// leave this unset and use the platform default.
    pub machine_salt_path: Option<PathBuf>,
}

impl Default for ResumeOptions {
    fn default() -> Self {
        Self {
            path: PathBuf::new(),
            use_latest: false,
            workspace: PathBuf::from("."),
            max_sections: None,
            task_frame_id: None,
            bound_workspace_id: None,
            bound_workspace_identity: None,
            include_prompt_fragment: true,
            require_fresh: false,
            insecure_skip_hmac: false,
            machine_salt_path: None,
        }
    }
}

/// Ready-to-prepend prompt fragment that the next agent can paste into
/// its system prompt without re-rendering. Mirrors `pack.text` from A4
/// for the handoff surface (M1 / bd-17c65.13.2).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResumePromptFragment {
    /// Markdown body. Stable structure: H1 capsule title, summary line,
    /// then per-section H2 blocks for objective / next actions /
    /// decisions when present. Invisible to standard markdown renderers
    /// past the body (no trailing comments) so it composes cleanly when
    /// prepended into a larger prompt.
    pub text: String,
    /// Approximate token count for the body (chars / 4, conservative).
    /// Used by the next agent to budget its prompt; not authoritative
    /// for billing-grade token math.
    pub estimated_tokens: u32,
    /// BLAKE3 prefix of the body so callers can correlate logged prompt
    /// fragments back to the resume that produced them.
    pub hash: String,
}

/// Workspace mismatch severity reported when a capsule produced in one
/// workspace is resumed in another. The producer side (M0 / capsule
/// fixtures) records the source workspace ID; the resume side compares
/// against the bound workspace's ID to surface the mismatch as either
/// soft (different root, recoverable) or hard (different workspace ID,
/// memory IDs may not exist in the destination DB).
#[derive(Clone, Copy, Debug, Serialize, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceMismatchSeverity {
    None,
    Soft,
    Hard,
}

impl WorkspaceMismatchSeverity {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Soft => "soft",
            Self::Hard => "hard",
        }
    }
}

/// Identity fingerprint that the producer side embeds into the
/// handoff capsule's `workspace_identity` block (bead bd-17c65.13.3 / M2).
/// Carries the BLAKE3 fingerprint of the canonical root, plus the
/// canonical root path itself and the optional repository
/// fingerprint when a git repository scope was detected. The resume
/// side compares this against its own bound workspace to decide
/// whether the match is exact, soft (same path or same repository),
/// or hard (no shared identity).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkspaceIdentity {
    /// 24-hex-char BLAKE3 prefix over canonical_root.
    pub fingerprint: String,
    /// Canonical root path string (already canonicalize'd by the
    /// producer side via WorkspaceResolution).
    pub canonical_root: String,
    /// `WorkspaceScopeKind` as a stable string ("standalone" |
    /// "repository" | "repository_subdir"). Stored as String so the
    /// capsule shape doesn't drift with future enum additions.
    pub scope_kind: String,
    /// Optional repository fingerprint when the workspace lives
    /// inside a git repository. Used by the resume side to detect
    /// "same repo, different workspace" soft matches.
    pub repository_fingerprint: Option<String>,
}

/// Granular four-level reconciliation result returned by
/// [`compare_workspace_identity`] (M2 / bd-17c65.13.3). Maps to
/// `WorkspaceMismatchSeverity` for the M1 boolean flag:
///   - `Exact` → `None`
///   - `SoftSamePath` / `SoftSameRepo` → `Soft`
///   - `Hard` → `Hard`
#[derive(Clone, Copy, Debug, Serialize, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceMatch {
    /// Fingerprints match — same canonical root.
    Exact,
    /// Fingerprints differ but canonical roots are equal (e.g. the
    /// path is the same but the producer hashed it under a different
    /// canonicalization). Treated as recoverable.
    SoftSamePath,
    /// Different canonical roots but the same containing repository
    /// fingerprint. Common monorepo case: two workspaces in different
    /// subdirectories of the same repo.
    SoftSameRepo,
    /// No shared identity — different workspace, different repo (or
    /// no repo on at least one side). Memory IDs from the capsule
    /// almost certainly don't exist in the bound workspace.
    Hard,
}

impl WorkspaceMatch {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::SoftSamePath => "soft_same_path",
            Self::SoftSameRepo => "soft_same_repo",
            Self::Hard => "hard",
        }
    }

    /// Project the four-level match to the M1 boolean severity used
    /// throughout the resume report.
    #[must_use]
    pub const fn to_mismatch_severity(self) -> WorkspaceMismatchSeverity {
        match self {
            Self::Exact => WorkspaceMismatchSeverity::None,
            Self::SoftSamePath | Self::SoftSameRepo => WorkspaceMismatchSeverity::Soft,
            Self::Hard => WorkspaceMismatchSeverity::Hard,
        }
    }
}

/// Compare a capsule's recorded `WorkspaceIdentity` against the resume
/// side's bound identity and return the four-level reconciliation
/// result. Bead bd-17c65.13.3 (M2).
#[must_use]
pub fn compare_workspace_identity(
    capsule: &WorkspaceIdentity,
    bound: &WorkspaceIdentity,
) -> WorkspaceMatch {
    if capsule.fingerprint.eq_ignore_ascii_case(&bound.fingerprint) {
        return WorkspaceMatch::Exact;
    }
    if capsule.canonical_root == bound.canonical_root {
        return WorkspaceMatch::SoftSamePath;
    }
    if let (Some(cap_repo), Some(bound_repo)) = (
        capsule.repository_fingerprint.as_deref(),
        bound.repository_fingerprint.as_deref(),
    ) {
        if cap_repo.eq_ignore_ascii_case(bound_repo) {
            return WorkspaceMatch::SoftSameRepo;
        }
    }
    WorkspaceMatch::Hard
}

/// Report from resuming a handoff capsule.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ResumeReport {
    pub schema: String,
    pub capsule_id: String,
    pub capsule_path: PathBuf,
    pub workspace: Option<String>,
    pub current_objective: Option<String>,
    pub status_summary: Option<String>,
    pub next_actions: Vec<NextAction>,
    pub blockers: Vec<Blocker>,
    pub do_not_repeat: Vec<DoNotRepeat>,
    pub recent_decisions: Vec<String>,
    pub recent_outcomes: Vec<String>,
    pub selected_memories: Vec<SelectedMemory>,
    pub active_focus: Option<serde_json::Value>,
    pub task_frame: Option<serde_json::Value>,
    pub swarm_brief_summary: Option<serde_json::Value>,
    pub artifact_pointers: Vec<ArtifactPointer>,
    pub degradations: Vec<DegradationInfo>,
    pub resumed_at: String,
    /// Ready-to-prepend prompt body (M1 / bd-17c65.13.2). Always present
    /// — `None` only when the capsule has zero usable content (no
    /// sections, no objective, no actions).
    pub prompt_fragment: Option<ResumePromptFragment>,
    /// Workspace mismatch severity computed against
    /// `ResumeOptions::bound_workspace_id`. `None` when no binding was
    /// requested; `Soft`/`Hard` when a binding was requested and the
    /// IDs differ.
    pub workspace_mismatch: WorkspaceMismatchSeverity,
    /// Four-level workspace match result (M2 / bd-17c65.13.3) computed
    /// against `ResumeOptions::bound_workspace_identity`. Always
    /// `None` when no structured identity was bound; otherwise one of
    /// {Exact, SoftSamePath, SoftSameRepo, Hard}. When this is set,
    /// `workspace_mismatch` is derived from it via
    /// `to_mismatch_severity()` so both fields stay in sync.
    pub workspace_match: Option<WorkspaceMatch>,
    /// Memory-set drift between capsule capture and resume time
    /// (M4 / bd-17c65.13.5). Always present in the resume output even
    /// when no drift is detected — agents inspect `drift_detected` to
    /// decide whether to refresh. `None` only when the capsule did
    /// not carry a captured state hash (legacy capsules pre-M3) or
    /// the current state hash could not be computed (no database).
    pub stale_snapshot: Option<StaleSnapshot>,
}

/// Memory-set drift report comparing the workspace state at capsule
/// capture against the workspace state at resume time
/// (M4 / bd-17c65.13.5).
///
/// Capsules created by M4 embed a compact memory snapshot containing
/// IDs, timestamps, validity windows, and content projection hashes.
/// Resume compares that captured snapshot with the current workspace
/// DB so agents can distinguish added, expired, revised, and
/// set-level drift. Legacy capsules without the snapshot still get
/// hash-only drift detection, with per-signal fields left as `None`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct StaleSnapshot {
    /// Wall-clock time the resume side computed this comparison. This
    /// is intentionally volatile and should be stripped by deterministic
    /// comparisons before hashing machine output.
    pub computed_at: String,
    /// Workspace state hash captured when the capsule was created.
    /// 16-char hex BLAKE3 prefix. `None` only for legacy capsules
    /// captured before M3.
    pub captured_state_hash: Option<String>,
    /// Workspace state hash recomputed at resume time. `None` when
    /// the workspace database is unavailable (e.g. resuming from a
    /// capsule against a directory that no longer has `.ee/ee.db`).
    pub current_state_hash: Option<String>,
    /// True when both hashes are available and differ. The honest
    /// fail-closed default: when either hash is missing, drift is
    /// reported as `false` — we lack the evidence to claim drift.
    /// Callers that want strict mode pass `require_fresh: true` and
    /// inspect the degraded array directly.
    pub drift_detected: bool,
    /// Count of new memories created in the workspace after the
    /// capsule's capture timestamp. `None` for legacy capsules or
    /// unreadable workspaces where the signal cannot be measured.
    pub memories_added_since: Option<u64>,
    /// Count of capsule-included memories whose `valid_to` has
    /// elapsed since capture. `None` for legacy capsules or unreadable
    /// workspaces where the signal cannot be measured.
    pub memories_expired_since: Option<u64>,
    /// Count of capsule-included memories whose level/kind/content/tag
    /// projection changed since capture. This remains distinct from
    /// `memories_expired_since`; validity-window changes are measured
    /// separately.
    pub memories_revised_since: Option<u64>,
    /// Jaccard drift between the memory ID set captured in the
    /// capsule and the active memory ID set at resume time. `0.0`
    /// means the active set is unchanged; `1.0` means no active IDs
    /// overlap. `None` for legacy capsules that did not embed the
    /// memory snapshot.
    pub content_drift_score: Option<f64>,
    /// Concrete repair hints tied to the drift signals. Empty when no
    /// drift was detected.
    pub repair_hints: Vec<String>,
}

/// Configurable thresholds deciding when measured stale-snapshot drift
/// should emit a degraded condition. The drift metrics themselves are
/// always reported; these thresholds only gate `degradations[]`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HandoffStaleThresholds {
    pub memories_added: u64,
    pub any_expired_in_pack: bool,
    pub content_drift_score: f64,
    pub memories_revised: u64,
}

impl Default for HandoffStaleThresholds {
    fn default() -> Self {
        Self {
            memories_added: 20,
            any_expired_in_pack: true,
            content_drift_score: 0.15,
            memories_revised: 0,
        }
    }
}

impl HandoffStaleThresholds {
    #[must_use]
    pub fn from_config(config: &crate::config::HandoffStaleThresholdConfig) -> Self {
        let defaults = Self::default();
        Self {
            memories_added: config.memories_added.unwrap_or(defaults.memories_added),
            any_expired_in_pack: config
                .any_expired_in_pack
                .unwrap_or(defaults.any_expired_in_pack),
            content_drift_score: config
                .content_drift_score
                .unwrap_or(defaults.content_drift_score),
            memories_revised: config.memories_revised.unwrap_or(defaults.memories_revised),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
struct CapsuleMemorySnapshot {
    captured_at: String,
    memory_count: u64,
    level_counts: BTreeMap<String, u64>,
    content_hash_tree_root: String,
    valid_window: CapsuleMemoryValidWindow,
    memories: Vec<CapsuleMemorySnapshotItem>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
struct CapsuleMemoryValidWindow {
    min_valid_from: Option<String>,
    max_valid_to: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
struct CapsuleMemorySnapshotItem {
    workspace_id: String,
    id: String,
    level: String,
    created_at: String,
    updated_at: String,
    content_hash: String,
    valid_from: Option<String>,
    valid_to: Option<String>,
}

/// A memory selected for the resume payload.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SelectedMemory {
    pub id: String,
    pub reason: String,
    pub confidence: String,
}

/// A pointer to a relevant artifact.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ArtifactPointer {
    pub id: String,
    pub path: Option<String>,
    pub description: String,
}

impl ResumeReport {
    #[must_use]
    pub fn new(capsule_id: String, path: PathBuf) -> Self {
        Self {
            schema: HANDOFF_RESUME_SCHEMA_V1.to_owned(),
            capsule_id,
            capsule_path: path,
            workspace: None,
            current_objective: None,
            status_summary: None,
            next_actions: Vec::new(),
            blockers: Vec::new(),
            do_not_repeat: Vec::new(),
            recent_decisions: Vec::new(),
            recent_outcomes: Vec::new(),
            selected_memories: Vec::new(),
            active_focus: None,
            task_frame: None,
            swarm_brief_summary: None,
            artifact_pointers: Vec::new(),
            degradations: Vec::new(),
            prompt_fragment: None,
            workspace_mismatch: WorkspaceMismatchSeverity::None,
            workspace_match: None,
            stale_snapshot: None,
            resumed_at: Utc::now().to_rfc3339(),
        }
    }

    #[must_use]
    pub fn to_json(&self) -> String {
        crate::core::serialize_or_error(self)
    }

    #[must_use]
    pub fn to_json_pretty(&self) -> String {
        crate::core::serialize_pretty_or_error(self)
    }
}

// ============================================================================
// Core Functions
// ============================================================================

fn estimate_tokens(text: &str) -> usize {
    text.split_whitespace().count().saturating_mul(4) / 3
}

fn generate_capsule_id() -> String {
    format!(
        "{}{}",
        HANDOFF_CAPSULE_ID_PREFIX,
        uuid::Uuid::now_v7().simple()
    )
}

fn compute_content_hash(content: &str) -> String {
    use blake3::Hasher;
    let mut hasher = Hasher::new();
    hasher.update(content.as_bytes());
    hasher.finalize().to_hex()[..16].to_owned()
}

/// Compute the round-trip-stable canonical content hash for a capsule
/// (M3 / bd-17c65.13.4). Delegates to the single source of truth
/// [`crate::obs::volatile_fields::strip_volatile_fields`] (augmented with
/// capsule-specific names per bd-1um33) for all common volatile timestamps,
/// paths, ids, run indices, and "swarm_brief_summary" keys. Then applies
/// the one value-dependent special case that cannot be expressed as a pure
/// field-name list: when an object carries `"id": "swarm_brief_summary"`,
/// its large diagnostic content subtree is redacted for both determinism
/// (host-specific posture) and because it is not durable workspace memory.
///
/// Two creates of the same workspace state always produce the same
/// canonical hash regardless of when/where they ran. This is the
/// equivalence anchor for the handoff round-trip determinism contract.
fn compute_canonical_capsule_hash(content: &serde_json::Value) -> String {
    let mut clone = content.clone();
    strip_volatile_capsule_fields(&mut clone);
    let canonical = canonical_json_string(&clone);
    compute_content_hash(&canonical)
}

/// Strip volatile fields for handoff capsule determinism.
///
/// Reuses the canonical `obs::volatile_fields` implementation (the single
/// source of truth for VOLATILE_FIELD_NAMES and the recursive stripper)
/// so that any future timestamp/path/elapsed/runIndex/etc. field added for
/// J7 determinism automatically applies to capsules without drift.
///
/// Only the value-dependent "swarm_brief_summary by id" content redaction
/// remains local; everything else is delegated.
fn strip_volatile_capsule_fields(value: &mut serde_json::Value) {
    // Delegate name-based volatile stripping (timestamps, paths, runIndex,
    // capsule_id, integrity, swarm_brief_summary key itself, etc.) to the
    // single source of truth. This recurses into every object/array.
    crate::obs::volatile_fields::strip_volatile_fields(value);

    // Apply the *one* value-dependent rule that the pure name-based registry
    // cannot express: any object whose "id" field is "swarm_brief_summary"
    // has its large diagnostic content subtree redacted (for determinism
    // of host posture + because it is not part of durable memory state).
    // We must recurse ourselves for this rule because it is not a simple
    // key-name match.
    strip_swarm_brief_content_by_id(value);
}

/// Recursively find objects with `"id": "swarm_brief_summary"` and remove
/// their volatile diagnostic payload fields. Kept separate so the main
/// name-based stripping can stay in the canonical obs implementation.
fn strip_swarm_brief_content_by_id(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(object) => {
            if object.get("id").and_then(serde_json::Value::as_str) == Some("swarm_brief_summary") {
                object.remove("content");
                object.remove("evidence_ids");
                object.remove("token_estimate");
            }
            for (_, child) in object.iter_mut() {
                strip_swarm_brief_content_by_id(child);
            }
        }
        serde_json::Value::Array(items) => {
            for child in items.iter_mut() {
                strip_swarm_brief_content_by_id(child);
            }
        }
        _ => {}
    }
}

/// Serialize a JSON value with keys sorted alphabetically at every
/// nesting level so the same logical content always produces the same
/// byte sequence. serde_json's default Object iteration order depends
/// on whether the `preserve_order` feature is on; this helper sorts
/// keys explicitly so we don't depend on global feature flags.
fn canonical_json_string(value: &serde_json::Value) -> String {
    let canonical = canonicalize_json_value(value);
    serde_json::to_string(&canonical).unwrap_or_else(|_| "null".to_owned())
}

fn canonicalize_json_value(value: &serde_json::Value) -> serde_json::Value {
    use std::collections::BTreeMap;
    match value {
        serde_json::Value::Object(map) => {
            let sorted: BTreeMap<String, serde_json::Value> = map
                .iter()
                .map(|(k, v)| (k.clone(), canonicalize_json_value(v)))
                .collect();
            // Convert BTreeMap back into a Map; serde_json::to_string
            // over a Map without the preserve_order feature already
            // sorts alphabetically, but going through BTreeMap → Map
            // makes the sorting explicit and resilient to feature flag
            // changes.
            let mut out = serde_json::Map::new();
            for (k, v) in sorted {
                out.insert(k, v);
            }
            serde_json::Value::Object(out)
        }
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.iter().map(canonicalize_json_value).collect())
        }
        other => other.clone(),
    }
}

fn signed_capsule_body(capsule: &serde_json::Value) -> serde_json::Value {
    let mut body = capsule.clone();
    if let Some(object) = body.as_object_mut() {
        object.remove("integrity");
    }
    body
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    bytes_to_lower_hex(digest.as_slice())
}

fn bytes_to_lower_hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(out, "{byte:02x}");
    }
    out
}

fn hmac_workspace_secret_path(workspace: &Path) -> PathBuf {
    workspace
        .join(".ee")
        .join("keys")
        .join(HANDOFF_WORKSPACE_SECRET_FILE)
}

fn default_machine_salt_path() -> PathBuf {
    std::env::var_os("HOME").map_or_else(
        || {
            PathBuf::from(".ee")
                .join("keys")
                .join(HANDOFF_MACHINE_SALT_FILE)
        },
        |home| {
            PathBuf::from(home)
                .join(".local")
                .join("share")
                .join("ee")
                .join("keys")
                .join(HANDOFF_MACHINE_SALT_FILE)
        },
    )
}

fn machine_salt_path(override_path: Option<&Path>) -> PathBuf {
    override_path.map_or_else(default_machine_salt_path, Path::to_path_buf)
}

fn read_or_create_secret(path: &Path) -> Result<[u8; 32], DomainError> {
    match fs::read(path) {
        Ok(bytes) => bytes_to_32_byte_secret(&bytes, path),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let secret = random_secret()?;
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).map_err(|error| DomainError::Storage {
                    message: format!("Failed to create handoff key directory: {error}"),
                    repair: Some(format!("Check permissions for {}", parent.display())),
                })?;
            }
            write_private_secret(path, &secret)?;
            Ok(secret)
        }
        Err(error) => Err(DomainError::Storage {
            message: format!("Failed to read handoff HMAC key: {error}"),
            repair: Some(format!("Check permissions for {}", path.display())),
        }),
    }
}

fn read_existing_secret(path: &Path, missing_code: &'static str) -> Result<[u8; 32], DomainError> {
    let bytes = fs::read(path).map_err(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            handoff_hmac_error(
                missing_code,
                "Required handoff HMAC key material is missing.",
                "Resume on the machine/workspace that created the capsule, or recreate the capsule.",
            )
        } else {
            DomainError::Storage {
                message: format!("Failed to read handoff HMAC key: {error}"),
                repair: Some(format!("Check permissions for {}", path.display())),
            }
        }
    })?;
    bytes_to_32_byte_secret(&bytes, path)
}

fn bytes_to_32_byte_secret(bytes: &[u8], path: &Path) -> Result<[u8; 32], DomainError> {
    let array: [u8; 32] = bytes.try_into().map_err(|_| DomainError::Storage {
        message: format!(
            "Invalid handoff key length at {}; expected 32 bytes.",
            path.display()
        ),
        repair: Some("Regenerate handoff keys after backing up existing key files.".to_owned()),
    })?;
    Ok(array)
}

fn random_secret() -> Result<[u8; 32], DomainError> {
    let rng = SystemRandom::new();
    let mut secret = [0_u8; 32];
    rng.fill(&mut secret).map_err(|_| DomainError::Storage {
        message: "Failed to generate handoff HMAC key material.".to_owned(),
        repair: Some("Verify the operating system random source is available.".to_owned()),
    })?;
    Ok(secret)
}

#[cfg(unix)]
fn write_private_secret(path: &Path, secret: &[u8; 32]) -> Result<(), DomainError> {
    use std::fs::OpenOptions;
    use std::os::unix::fs::OpenOptionsExt;

    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to write handoff HMAC key: {error}"),
            repair: Some(format!("Check permissions for {}", path.display())),
        })?;
    file.write_all(secret)
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to write handoff HMAC key: {error}"),
            repair: Some(format!("Check permissions for {}", path.display())),
        })?;
    file.sync_all()
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to sync handoff HMAC key: {error}"),
            repair: Some(format!("Check disk health for {}", path.display())),
        })?;
    set_private_file_mode(path)
}

#[cfg(not(unix))]
fn write_private_secret(path: &Path, secret: &[u8; 32]) -> Result<(), DomainError> {
    fs::write(path, secret).map_err(|error| DomainError::Storage {
        message: format!("Failed to write handoff HMAC key: {error}"),
        repair: Some(format!("Check permissions for {}", path.display())),
    })?;
    set_private_file_mode(path)
}

fn rotate_workspace_secret(workspace: &Path) -> Result<[u8; 32], DomainError> {
    let path = hmac_workspace_secret_path(workspace);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| DomainError::Storage {
            message: format!("Failed to create handoff key directory: {error}"),
            repair: Some(format!("Check permissions for {}", parent.display())),
        })?;
    }
    let secret = random_secret()?;
    write_private_secret(&path, &secret)?;
    Ok(secret)
}

#[cfg(unix)]
fn set_private_file_mode(path: &Path) -> Result<(), DomainError> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).map_err(|error| {
        DomainError::Storage {
            message: format!("Failed to set handoff key permissions: {error}"),
            repair: Some(format!("Set mode 0600 on {}", path.display())),
        }
    })
}

#[cfg(not(unix))]
fn set_private_file_mode(_path: &Path) -> Result<(), DomainError> {
    Ok(())
}

fn append_len_prefixed(out: &mut Vec<u8>, bytes: &[u8]) {
    let len = u32::try_from(bytes.len()).unwrap_or(u32::MAX);
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(bytes);
}

fn key_input(
    workspace_identity: &WorkspaceIdentity,
    capsule_schema_version: &str,
    workspace_secret: &[u8; 32],
) -> Vec<u8> {
    let mut input = Vec::new();
    append_len_prefixed(&mut input, capsule_schema_version.as_bytes());
    append_len_prefixed(
        &mut input,
        workspace_identity.fingerprint.to_lowercase().as_bytes(),
    );
    append_len_prefixed(&mut input, workspace_identity.canonical_root.as_bytes());
    append_len_prefixed(&mut input, workspace_identity.scope_kind.as_bytes());
    match &workspace_identity.repository_fingerprint {
        Some(repo) => {
            input.push(1);
            append_len_prefixed(&mut input, repo.to_lowercase().as_bytes());
        }
        None => input.push(0),
    }
    append_len_prefixed(&mut input, workspace_secret);
    input
}

fn derive_handoff_hmac_key(
    workspace_identity: &WorkspaceIdentity,
    capsule_schema_version: &str,
    workspace_secret: &[u8; 32],
    machine_salt: Option<&[u8; 32]>,
) -> ([u8; 32], String) {
    let mut input = key_input(workspace_identity, capsule_schema_version, workspace_secret);
    let context = if let Some(salt) = machine_salt {
        append_len_prefixed(&mut input, salt);
        "ee.handoff.capsule.hmac.strict.v1"
    } else {
        "ee.handoff.capsule.hmac.workspace.v1"
    };
    let key = blake3::derive_key(context, &input);
    let input_hash = blake3::hash(&input).to_hex()[..16].to_owned();
    (key, format!("blake3:{input_hash}"))
}

fn sign_capsule_content(
    capsule: serde_json::Value,
    workspace: &Path,
    bind_to_machine: bool,
    machine_salt_path_override: Option<&Path>,
) -> Result<serde_json::Value, DomainError> {
    let workspace_secret = read_or_create_secret(&hmac_workspace_secret_path(workspace))?;
    let machine_salt = if bind_to_machine {
        let path = machine_salt_path(machine_salt_path_override);
        Some(read_or_create_secret(&path)?)
    } else {
        None
    };
    sign_capsule_content_with_key_material(
        capsule,
        &workspace_secret,
        machine_salt.as_ref(),
        bind_to_machine,
    )
}

fn sign_capsule_content_with_key_material(
    mut capsule: serde_json::Value,
    workspace_secret: &[u8; 32],
    machine_salt: Option<&[u8; 32]>,
    bind_to_machine: bool,
) -> Result<serde_json::Value, DomainError> {
    let workspace_identity = capsule
        .get("workspace_identity")
        .cloned()
        .and_then(|value| serde_json::from_value::<WorkspaceIdentity>(value).ok())
        .ok_or_else(|| DomainError::Storage {
            message: "Cannot sign handoff capsule without workspace_identity.".to_owned(),
            repair: Some("Regenerate the capsule with a workspace-aware ee version.".to_owned()),
        })?;
    let capsule_schema = capsule
        .get("schema")
        .and_then(serde_json::Value::as_str)
        .unwrap_or(HANDOFF_CAPSULE_SCHEMA_V1);
    let (key_bytes, key_derivation_input_hash) = derive_handoff_hmac_key(
        &workspace_identity,
        capsule_schema,
        workspace_secret,
        machine_salt,
    );
    let signed_body = signed_capsule_body(&capsule);
    let signed_body_bytes = canonical_json_string(&signed_body);
    let key = hmac::Key::new(hmac::HMAC_SHA256, &key_bytes);
    let tag = hmac::sign(&key, signed_body_bytes.as_bytes());
    let encoded_hmac = URL_SAFE_NO_PAD.encode(tag.as_ref());
    let hmac_prefix = encoded_hmac.chars().take(8).collect::<String>();
    let key_mode = if bind_to_machine {
        HANDOFF_HMAC_KEY_MODE_MACHINE_BOUND
    } else {
        HANDOFF_HMAC_KEY_MODE_WORKSPACE_SECRET
    };
    let integrity = serde_json::json!({
        "schema": HANDOFF_INTEGRITY_SCHEMA_V1,
        "algorithm": HANDOFF_HMAC_ALGORITHM,
        "keyMode": key_mode,
        "bodySha256": format!("sha256:{}", sha256_hex(signed_body_bytes.as_bytes())),
        "keyDerivationInputHash": key_derivation_input_hash,
        "hmac": format!("base64url:{encoded_hmac}"),
        "hmacPrefix": hmac_prefix,
    });
    if let Some(object) = capsule.as_object_mut() {
        object.insert("integrity".to_owned(), integrity);
    }
    tracing::info!(
        event = "handoff_hmac_computed",
        capsule_id = capsule
            .get("capsule_id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown"),
        hmac_prefix = %hmac_prefix,
        key_mode = key_mode,
        "handoff capsule HMAC computed"
    );
    Ok(capsule)
}

fn verify_capsule_integrity(
    capsule: &serde_json::Value,
    workspace: &Path,
    machine_salt_path_override: Option<&Path>,
) -> Result<(), DomainError> {
    let integrity = capsule
        .get("integrity")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| {
            handoff_hmac_error(
                HANDOFF_HMAC_MISSING_CODE,
                "Handoff capsule is unsigned and cannot be trusted.",
                "Recreate the capsule with this ee version, or pass --insecure-skip-hmac for audited emergency recovery.",
            )
        })?;
    let algorithm = integrity
        .get("algorithm")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    let key_mode = integrity
        .get("keyMode")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    if algorithm != HANDOFF_HMAC_ALGORITHM {
        return Err(handoff_hmac_error(
            HANDOFF_CAPSULE_TAMPERED_CODE,
            "Handoff capsule uses an unsupported integrity algorithm.",
            "Recreate the capsule with this ee version.",
        ));
    }

    let signed_body = signed_capsule_body(capsule);
    let signed_body_bytes = canonical_json_string(&signed_body);
    let expected_body_sha = integrity
        .get("bodySha256")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    let actual_body_sha = format!("sha256:{}", sha256_hex(signed_body_bytes.as_bytes()));
    if expected_body_sha != actual_body_sha {
        return Err(handoff_hmac_error(
            HANDOFF_CAPSULE_TAMPERED_CODE,
            "Handoff capsule body hash does not match its integrity record.",
            "Discard the capsule and recreate it from the source workspace.",
        ));
    }

    let workspace_identity = capsule
        .get("workspace_identity")
        .cloned()
        .and_then(|value| serde_json::from_value::<WorkspaceIdentity>(value).ok())
        .ok_or_else(|| {
            handoff_hmac_error(
                HANDOFF_CAPSULE_TAMPERED_CODE,
                "Handoff capsule is missing signed workspace identity.",
                "Discard the capsule and recreate it from the source workspace.",
            )
        })?;
    let capsule_schema = capsule
        .get("schema")
        .and_then(serde_json::Value::as_str)
        .unwrap_or(HANDOFF_CAPSULE_SCHEMA_V1);
    let workspace_secret = read_existing_secret(
        &hmac_workspace_secret_path(workspace),
        HANDOFF_CAPSULE_MACHINE_MISMATCH_CODE,
    )?;
    let machine_salt = match key_mode {
        HANDOFF_HMAC_KEY_MODE_WORKSPACE_SECRET => None,
        HANDOFF_HMAC_KEY_MODE_MACHINE_BOUND => {
            let path = machine_salt_path(machine_salt_path_override);
            Some(read_existing_secret(&path, STRICT_MODE_NO_SALT_FILE_CODE)?)
        }
        _ => {
            return Err(handoff_hmac_error(
                HANDOFF_CAPSULE_TAMPERED_CODE,
                "Handoff capsule declares an unknown HMAC key mode.",
                "Recreate the capsule with this ee version.",
            ));
        }
    };
    let (key_bytes, key_derivation_input_hash) = derive_handoff_hmac_key(
        &workspace_identity,
        capsule_schema,
        &workspace_secret,
        machine_salt.as_ref(),
    );
    if integrity
        .get("keyDerivationInputHash")
        .and_then(serde_json::Value::as_str)
        != Some(key_derivation_input_hash.as_str())
    {
        return Err(handoff_hmac_error(
            HANDOFF_CAPSULE_MACHINE_MISMATCH_CODE,
            "Handoff capsule key context does not match this workspace or machine.",
            "Resume on the original workspace/machine, or recreate the capsule here.",
        ));
    }
    let encoded_hmac = integrity
        .get("hmac")
        .and_then(serde_json::Value::as_str)
        .and_then(|value| value.strip_prefix("base64url:"))
        .ok_or_else(|| {
            handoff_hmac_error(
                HANDOFF_CAPSULE_TAMPERED_CODE,
                "Handoff capsule integrity record is malformed.",
                "Discard the capsule and recreate it from the source workspace.",
            )
        })?;
    let expected_tag = URL_SAFE_NO_PAD.decode(encoded_hmac).map_err(|_| {
        handoff_hmac_error(
            HANDOFF_CAPSULE_TAMPERED_CODE,
            "Handoff capsule HMAC is not valid base64url.",
            "Discard the capsule and recreate it from the source workspace.",
        )
    })?;
    let key = hmac::Key::new(hmac::HMAC_SHA256, &key_bytes);
    hmac::verify(&key, signed_body_bytes.as_bytes(), &expected_tag).map_err(|_| {
        let code = if key_mode == HANDOFF_HMAC_KEY_MODE_MACHINE_BOUND {
            HANDOFF_CAPSULE_MACHINE_MISMATCH_CODE
        } else {
            HANDOFF_CAPSULE_TAMPERED_CODE
        };
        handoff_hmac_error(
            code,
            "Handoff capsule HMAC verification failed.",
            "Discard the capsule and recreate it from the source workspace.",
        )
    })?;
    tracing::info!(
        event = "handoff_hmac_verify_result",
        capsule_id = capsule
            .get("capsule_id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown"),
        status = "ok",
        key_mode = key_mode,
        "handoff capsule HMAC verified"
    );
    Ok(())
}

fn handoff_hmac_error(
    code: &'static str,
    message: impl Into<String>,
    repair: impl Into<String>,
) -> DomainError {
    DomainError::UnsatisfiedDegradedModeCode {
        code,
        message: message.into(),
        repair: Some(repair.into()),
    }
}

fn record_handoff_insecure_load_audit(
    workspace: &Path,
    capsule_path: &Path,
    capsule_bytes: &str,
    capsule_id: &str,
) {
    let database_path = workspace.join(".ee").join("ee.db");
    let Ok(conn) = DbConnection::open_file(&database_path) else {
        return;
    };
    let details = serde_json::json!({
        "schema": "ee.audit.handoff_insecure_load.v1",
        "capsulePath": capsule_path.display().to_string(),
        "capsuleSha256": format!("sha256:{}", sha256_hex(capsule_bytes.as_bytes())),
        "capsuleId": capsule_id,
        "reason": "caller passed --insecure-skip-hmac",
    });
    let input = CreateAuditInput {
        workspace_id: None,
        actor: Some("ee handoff resume".to_owned()),
        action: audit_actions::HANDOFF_INSECURE_LOAD.to_owned(),
        target_type: Some("handoff_capsule".to_owned()),
        target_id: Some(capsule_id.to_owned()),
        details: Some(details.to_string()),
    };
    let _ = conn.insert_audit(&generate_audit_id(), &input);
}

fn record_handoff_hmac_rotate_audit(
    workspace: &Path,
    capsule_path: &Path,
    capsule_id: &str,
    key_mode: &str,
    old_hmac_prefix: Option<&str>,
    new_hmac_prefix: &str,
    body_sha256: &str,
) -> Option<String> {
    let database_path = workspace.join(".ee").join("ee.db");
    let conn = DbConnection::open_file(&database_path).ok()?;
    let audit_id = generate_audit_id();
    let details = serde_json::json!({
        "schema": "ee.audit.handoff_hmac_rotate.v1",
        "capsulePath": capsule_path.display().to_string(),
        "capsuleId": capsule_id,
        "keyMode": key_mode,
        "oldHmacPrefix": old_hmac_prefix,
        "newHmacPrefix": new_hmac_prefix,
        "bodySha256": body_sha256,
        "secretMaterialLogged": false,
    });
    let input = CreateAuditInput {
        workspace_id: None,
        actor: Some("ee handoff rotate-key".to_owned()),
        action: audit_actions::HANDOFF_HMAC_ROTATE.to_owned(),
        target_type: Some("handoff_capsule".to_owned()),
        target_id: Some(capsule_id.to_owned()),
        details: Some(details.to_string()),
    };
    conn.insert_audit(&audit_id, &input).ok()?;
    Some(audit_id)
}

fn focus_memory_ids(focus_state: &crate::models::FocusState) -> Vec<String> {
    focus_state
        .items
        .iter()
        .map(|item| item.memory_id.to_string())
        .collect()
}

fn render_focus_section(focus_state: &crate::models::FocusState) -> String {
    let mut lines = vec![format!(
        "Passive focus state: {} memories / capacity {} (hash {})",
        focus_state.item_count(),
        focus_state.capacity,
        focus_state_hash(focus_state)
    )];
    if let Some(focal) = focus_state.focal_memory_id {
        lines.push(format!("Focal memory: {focal}"));
    }
    for item in &focus_state.items {
        lines.push(format!(
            "- {}{}: {}",
            item.memory_id,
            if item.pinned { " [pinned]" } else { "" },
            item.reason
        ));
    }
    lines.join("\n")
}

fn task_frame_evidence_ids(frame: &TaskFrameRecord) -> Vec<String> {
    let mut ids = vec![frame.id.clone()];
    ids.extend(
        frame
            .evidence_links
            .iter()
            .map(|link| format!("{}:{}", link.kind, link.id)),
    );
    ids.sort();
    ids.dedup();
    ids
}

fn render_task_frame_section(frame: &TaskFrameRecord) -> String {
    let mut lines = vec![
        format!("Task frame: {}", frame.id),
        format!("Goal: {}", frame.root_goal),
        format!("Status: {}", frame.status.as_str()),
        format!("Redaction: {}", frame.redaction_status),
        format!("Contract: {NON_EXECUTING_CONTRACT}"),
    ];
    if let Some(focus) = &frame.current_focus {
        lines.push(format!("Current focus: {focus}"));
    }
    if !frame.blockers.is_empty() {
        lines.push("Blockers:".to_owned());
        for blocker in &frame.blockers {
            lines.push(format!("- {blocker}"));
        }
    }
    if !frame.subgoals.is_empty() {
        lines.push("Subgoals:".to_owned());
        for subgoal in &frame.subgoals {
            let parent = subgoal
                .parent_id
                .as_deref()
                .map_or(String::new(), |parent| format!(" parent={parent}"));
            lines.push(format!(
                "- {} [{}{}] {}",
                subgoal.id,
                subgoal.status.as_str(),
                parent,
                subgoal.title
            ));
        }
    }
    lines.join("\n")
}

fn read_handoff_task_frame(
    workspace: &Path,
    task_frame_id: Option<&str>,
) -> Result<Option<TaskFrameRecord>, DomainError> {
    match show_task_frame(&TaskFrameShowOptions {
        workspace_path: workspace.to_path_buf(),
        frame_id: task_frame_id.map(ToOwned::to_owned),
        active: task_frame_id.is_none(),
    }) {
        Ok(report) => Ok(report.frame),
        Err(error) if task_frame_id.is_none() && error.code() == "not_found" => Ok(None),
        Err(error) => Err(error),
    }
}

fn task_frame_json(frame: &TaskFrameRecord) -> Option<serde_json::Value> {
    serde_json::to_value(frame).ok()
}

fn add_task_frame_to_resume(report: &mut ResumeReport, task_frame: serde_json::Value) {
    let frame_id = task_frame
        .get("id")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("unknown");
    let status = task_frame
        .get("status")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("unknown");
    let redaction_status = task_frame
        .get("redactionStatus")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("unknown");

    if let Some(goal) = task_frame
        .get("rootGoal")
        .and_then(serde_json::Value::as_str)
    {
        report.current_objective = Some(goal.to_owned());
    }
    report.status_summary = Some(format!(
        "Passive task frame {frame_id} is {status}; redaction={redaction_status}; non_executing=true."
    ));
    report.next_actions.push(
        NextAction::new(1, "Review passive task-frame state.")
            .with_command(format!("ee task-frame show {frame_id} --json"))
            .with_reason(
                "Task frame was included as durable resume context, not as an execution plan.",
            ),
    );

    if let Some(blockers) = task_frame
        .get("blockers")
        .and_then(serde_json::Value::as_array)
    {
        for (index, blocker) in blockers.iter().enumerate() {
            if let Some(description) = blocker.as_str() {
                report.blockers.push(
                    Blocker::new(format!("task_frame_blocker_{}", index + 1), description)
                        .with_resolution(format!("Resolve or update {frame_id} before closing."))
                        .hard(),
                );
            }
        }
    }

    if let Some(subgoals) = task_frame
        .get("subgoals")
        .and_then(serde_json::Value::as_array)
    {
        for subgoal in subgoals {
            let subgoal_status = subgoal
                .get("status")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown");
            let title = subgoal
                .get("title")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("untitled subgoal");
            if subgoal_status == "blocked" {
                let subgoal_id = subgoal
                    .get("id")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("unknown_subgoal");
                report.blockers.push(
                    Blocker::new(subgoal_id, format!("Blocked subgoal: {title}"))
                        .with_resolution(format!("Update {frame_id} subgoal status when resolved."))
                        .hard(),
                );
            }
        }
    }

    if let Some(links) = task_frame
        .get("evidenceLinks")
        .and_then(serde_json::Value::as_array)
    {
        for link in links {
            let kind = link
                .get("kind")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            let id = link
                .get("id")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            if kind.is_empty() || id.is_empty() {
                continue;
            }
            if kind == "memory" {
                report.selected_memories.push(SelectedMemory {
                    id: id.to_owned(),
                    reason: format!("Linked by passive task frame {frame_id}."),
                    confidence: "verified".to_owned(),
                });
            } else {
                report.artifact_pointers.push(ArtifactPointer {
                    id: id.to_owned(),
                    path: None,
                    description: format!("{kind} linked by passive task frame {frame_id}."),
                });
            }
        }
    }

    report.task_frame = Some(task_frame);
}

fn add_swarm_brief_summary_to_resume(report: &mut ResumeReport, summary: &serde_json::Value) {
    let counts = summary.get("counts").unwrap_or(&serde_json::Value::Null);
    let ready = counts
        .get("readyWorkCount")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let blocked = counts
        .get("blockedWorkCount")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let conflicts = counts
        .get("activeConflictCount")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let degraded = counts
        .get("degradedCount")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let pressure = summary
        .get("resourcePressurePosture")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("unknown");
    let evidence_id = swarm_brief_summary_evidence_id(summary);
    let posture = format!(
        "Embedded swarm brief summary: ready={ready}, blocked={blocked}, active_conflicts={conflicts}, resource_pressure={pressure}, degraded_sources={degraded}; diagnostic_not_live=true."
    );
    report.status_summary = Some(match report.status_summary.take() {
        Some(existing) => format!("{existing}\n{posture}"),
        None => posture,
    });
    report.artifact_pointers.push(ArtifactPointer {
        id: evidence_id.clone(),
        path: None,
        description: "Redacted read-only swarm brief summary embedded in the handoff capsule."
            .to_owned(),
    });
    report.next_actions.push(
        NextAction::new(2, "Refresh live coordination posture before claiming work.")
            .with_reason("Embedded swarm brief summaries are diagnostic handoff context only.")
            .with_command("ee swarm brief --json"),
    );

    if let Some(codes) = summary
        .get("degradedCodes")
        .and_then(serde_json::Value::as_array)
    {
        for code in codes.iter().filter_map(serde_json::Value::as_str).take(8) {
            report.degradations.push(
                DegradationInfo::new(
                    format!("swarm_brief_{code}"),
                    "Embedded swarm brief summary reported a degraded coordination source.",
                )
                .with_next_action("ee swarm brief --json"),
            );
        }
    }
}

/// Preview a handoff capsule without writing it.
pub fn preview_handoff(options: &PreviewOptions) -> Result<PreviewReport, DomainError> {
    let mut report = PreviewReport::new(options.workspace.clone(), options.profile);

    let workspace_section = CapsuleSection::new("workspace", "Workspace Identity")
        .with_content(format!("Workspace: {}", options.workspace.display()))
        .with_confidence(EvidenceConfidence::Verified);

    report.planned_sections.push(PlannedSection {
        id: workspace_section.id.clone(),
        title: workspace_section.title.clone(),
        confidence: workspace_section.confidence.as_str().to_owned(),
        evidence_count: 0,
        token_estimate: workspace_section.token_estimate,
    });

    let objective_section = CapsuleSection::new("objective", "Current Objective")
        .with_content("Continue work on current task.")
        .with_confidence(EvidenceConfidence::Inferred);

    report.planned_sections.push(PlannedSection {
        id: objective_section.id.clone(),
        title: objective_section.title.clone(),
        confidence: objective_section.confidence.as_str().to_owned(),
        evidence_count: 0,
        token_estimate: objective_section.token_estimate,
    });

    let actions_section = CapsuleSection::new("next_actions", "Next Actions")
        .with_content("Review current state and continue.")
        .with_confidence(EvidenceConfidence::Inferred);

    report.planned_sections.push(PlannedSection {
        id: actions_section.id.clone(),
        title: actions_section.title.clone(),
        confidence: actions_section.confidence.as_str().to_owned(),
        evidence_count: 0,
        token_estimate: actions_section.token_estimate,
    });

    let swarm_brief_summary = collect_swarm_brief_summary(&options.workspace);
    let swarm_brief_evidence = vec![swarm_brief_summary_evidence_id(&swarm_brief_summary)];
    let swarm_brief_section = CapsuleSection::new("swarm_brief_summary", "Swarm Brief Summary")
        .with_content(render_swarm_brief_summary_for_handoff(&swarm_brief_summary))
        .with_confidence(EvidenceConfidence::Verified)
        .with_evidence(swarm_brief_evidence.clone());
    report.evidence_ids.extend(swarm_brief_evidence);
    report.swarm_brief_summary = Some(swarm_brief_summary);
    report.planned_sections.push(PlannedSection {
        id: swarm_brief_section.id.clone(),
        title: swarm_brief_section.title.clone(),
        confidence: swarm_brief_section.confidence.as_str().to_owned(),
        evidence_count: swarm_brief_section.evidence_ids.len(),
        token_estimate: swarm_brief_section.token_estimate,
    });

    let singleflight_posture = singleflight_posture_report();
    let singleflight_section = CapsuleSection::new("singleflight_posture", "Single-flight Posture")
        .with_content(render_singleflight_posture_for_handoff(
            &singleflight_posture,
        ))
        .with_confidence(EvidenceConfidence::Verified);
    report.planned_sections.push(PlannedSection {
        id: singleflight_section.id.clone(),
        title: singleflight_section.title.clone(),
        confidence: singleflight_section.confidence.as_str().to_owned(),
        evidence_count: 0,
        token_estimate: singleflight_section.token_estimate,
    });

    match read_active_focus_state(&options.workspace) {
        Ok(Some(focus_state)) => {
            let focus_section = CapsuleSection::new("active_focus", "Active Memory Focus")
                .with_content(render_focus_section(&focus_state))
                .with_confidence(EvidenceConfidence::Verified)
                .with_evidence(focus_memory_ids(&focus_state));
            report.evidence_ids.extend(focus_memory_ids(&focus_state));
            report.active_focus = Some(focus_state.data_json());
            report.planned_sections.push(PlannedSection {
                id: focus_section.id.clone(),
                title: focus_section.title.clone(),
                confidence: focus_section.confidence.as_str().to_owned(),
                evidence_count: focus_section.evidence_ids.len(),
                token_estimate: focus_section.token_estimate,
            });
        }
        Ok(None) => {}
        Err(error) => report.degradations.push(
            DegradationInfo::new(
                "handoff_focus_state_unavailable",
                format!("Active focus state could not be read: {}", error.message()),
            )
            .with_next_action("ee focus show --json"),
        ),
    }

    match read_handoff_task_frame(&options.workspace, options.task_frame_id.as_deref()) {
        Ok(Some(frame)) => {
            let evidence_ids = task_frame_evidence_ids(&frame);
            let task_frame_section = CapsuleSection::new("task_frame", "Passive Task Frame")
                .with_content(render_task_frame_section(&frame))
                .with_confidence(EvidenceConfidence::Verified)
                .with_evidence(evidence_ids.clone());
            report.evidence_ids.extend(evidence_ids);
            report.task_frame = task_frame_json(&frame);
            report.planned_sections.push(PlannedSection {
                id: task_frame_section.id.clone(),
                title: task_frame_section.title.clone(),
                confidence: task_frame_section.confidence.as_str().to_owned(),
                evidence_count: task_frame_section.evidence_ids.len(),
                token_estimate: task_frame_section.token_estimate,
            });
        }
        Ok(None) => {}
        Err(error) => report.degradations.push(
            DegradationInfo::new(
                "handoff_task_frame_unavailable",
                format!("Task-frame state could not be read: {}", error.message()),
            )
            .with_next_action("ee task-frame show --active --json"),
        ),
    }

    if options.profile == CapsuleProfile::Compact {
        report.omitted_sections.push(OmittedSection {
            id: "decisions".to_owned(),
            reason: "Compact profile omits historical decisions.".to_owned(),
        });
        report.omitted_sections.push(OmittedSection {
            id: "outcomes".to_owned(),
            reason: "Compact profile omits outcome history.".to_owned(),
        });
    }

    report.token_estimate = report
        .planned_sections
        .iter()
        .map(|s| s.token_estimate)
        .sum();
    report.byte_estimate = report.token_estimate * 4;
    report.sufficient_for_resume = !report.planned_sections.is_empty();

    if !options.workspace.exists() {
        report.degradations.push(
            DegradationInfo::new("workspace_not_found", "Workspace path does not exist.")
                .with_next_action("Verify workspace path or run ee init."),
        );
        report.sufficient_for_resume = false;
    }

    Ok(report)
}

/// Create a handoff capsule.
pub fn create_handoff(options: &CreateOptions) -> Result<CreateReport, DomainError> {
    let capsule_id = generate_capsule_id();
    let mut report = CreateReport::new(
        capsule_id.clone(),
        options.workspace.clone(),
        options.output.clone(),
    );
    report.profile = options.profile.as_str().to_owned();
    report.dry_run = options.dry_run;

    let mut sections = Vec::new();

    sections.push(
        CapsuleSection::new("workspace", "Workspace Identity")
            .with_content(format!(
                "Workspace: {}\nProfile: {}",
                options.workspace.display(),
                options.profile.as_str()
            ))
            .with_confidence(EvidenceConfidence::Verified),
    );

    sections.push(
        CapsuleSection::new("objective", "Current Objective")
            .with_content("Continue work on current task.")
            .with_confidence(EvidenceConfidence::Inferred),
    );

    sections.push(CapsuleSection::new("next_actions", "Next Actions")
        .with_content("1. Review current state\n2. Check for uncommitted changes\n3. Continue implementation")
        .with_confidence(EvidenceConfidence::Inferred));

    let active_focus = match read_active_focus_state(&options.workspace) {
        Ok(Some(focus_state)) => {
            let focus_memory_ids = focus_memory_ids(&focus_state);
            sections.push(
                CapsuleSection::new("active_focus", "Active Memory Focus")
                    .with_content(render_focus_section(&focus_state))
                    .with_confidence(EvidenceConfidence::Verified)
                    .with_evidence(focus_memory_ids.clone()),
            );
            report.evidence_count = report.evidence_count.saturating_add(focus_memory_ids.len());
            Some(focus_state.data_json())
        }
        Ok(None) => None,
        Err(_) => None,
    };
    report.active_focus = active_focus.clone();

    let task_frame = read_handoff_task_frame(&options.workspace, options.task_frame_id.as_deref())?;
    let task_frame_json = task_frame.as_ref().and_then(task_frame_json);
    if let Some(frame) = &task_frame {
        let task_frame_evidence_ids = task_frame_evidence_ids(frame);
        sections.push(
            CapsuleSection::new("task_frame", "Passive Task Frame")
                .with_content(render_task_frame_section(frame))
                .with_confidence(EvidenceConfidence::Verified)
                .with_evidence(task_frame_evidence_ids.clone()),
        );
        report.evidence_count = report
            .evidence_count
            .saturating_add(task_frame_evidence_ids.len());
    }
    report.task_frame = task_frame_json.clone();

    let swarm_brief_summary = collect_swarm_brief_summary(&options.workspace);
    let swarm_brief_evidence = vec![swarm_brief_summary_evidence_id(&swarm_brief_summary)];
    sections.push(
        CapsuleSection::new("swarm_brief_summary", "Swarm Brief Summary")
            .with_content(render_swarm_brief_summary_for_handoff(&swarm_brief_summary))
            .with_confidence(EvidenceConfidence::Verified)
            .with_evidence(swarm_brief_evidence.clone()),
    );
    report.evidence_count = report
        .evidence_count
        .saturating_add(swarm_brief_evidence.len());
    report.swarm_brief_summary = Some(swarm_brief_summary.clone());

    let singleflight_posture = singleflight_posture_report();
    sections.push(
        CapsuleSection::new("singleflight_posture", "Single-flight Posture")
            .with_content(render_singleflight_posture_for_handoff(
                &singleflight_posture,
            ))
            .with_confidence(EvidenceConfidence::Verified),
    );

    if options.profile.include_full_evidence() {
        sections.push(
            CapsuleSection::new("decisions", "Recent Decisions")
                .with_content("No recent decisions recorded.")
                .with_confidence(EvidenceConfidence::Unknown),
        );
    }

    report.sections_included = sections.len();
    report.token_count = sections.iter().map(|s| s.token_estimate).sum();
    report.byte_count = report.token_count * 4;

    // Bead bd-17c65.13.3 (M2): embed structured workspace_identity so
    // the resume side can run a four-level reconciliation (Exact /
    // SoftSamePath / SoftSameRepo / Hard) against its bound workspace.
    let workspace_identity = compute_workspace_identity_for_capsule(&options.workspace);

    let created_at = Utc::now().to_rfc3339();

    // Bead bd-1xkxi (M3 follow-up): embed a deterministic
    // workspace_state_hash so the canonical_content_hash actually
    // reflects the underlying memory set. Without this, adding a new
    // memory wouldn't change the capsule hash because the v1 capsule
    // body uses templated section placeholders. The hash is computed
    // from the DB file at <workspace>/.ee/ee.db when present;
    // otherwise None (preview-on-empty-workspace path).
    let db_path = options.workspace.join(".ee").join("ee.db");
    let workspace_state_hash = compute_workspace_state_hash(&db_path);
    let memory_snapshot = compute_capsule_memory_snapshot(&db_path, &created_at);
    if let Some(snapshot) = &memory_snapshot {
        tracing::info!(
            event = "handoff_capsule_fingerprint_computed",
            capsule_id = %capsule_id,
            level_counts = ?snapshot.level_counts,
            content_hash_tree_root = %snapshot.content_hash_tree_root,
            valid_window = ?snapshot.valid_window,
            "handoff capsule memory fingerprint computed"
        );
    }

    let capsule_content = serde_json::json!({
        "schema": HANDOFF_CAPSULE_SCHEMA_V1,
        "capsule_id": capsule_id,
        "workspace": options.workspace,
        "workspace_identity": workspace_identity,
        "workspace_state_hash": workspace_state_hash,
        "memory_snapshot": memory_snapshot,
        "profile": options.profile.as_str(),
        "sections": sections,
        "active_focus": active_focus,
        "task_frame": task_frame_json,
        "swarm_brief_summary": swarm_brief_summary,
        "created_at": created_at,
    });
    let capsule_content = sign_capsule_content(
        capsule_content,
        &options.workspace,
        options.bind_to_machine,
        options.machine_salt_path.as_deref(),
    )?;

    let content_str = crate::core::serialize_pretty_or_error(&capsule_content);
    report.content_hash = compute_content_hash(&content_str);
    // Bead bd-17c65.13.4 (M3): canonical content hash for round-trip
    // determinism. Strips volatile fields (capsule_id, created_at,
    // audit_id, last_accessed_at) so two creates of the same workspace
    // state always produce the same value.
    report.canonical_content_hash = compute_canonical_capsule_hash(&capsule_content);

    if !options.dry_run {
        std::fs::write(&options.output, &content_str).map_err(|e| DomainError::Storage {
            message: format!("Failed to write capsule: {e}"),
            repair: Some(format!(
                "Check write permissions for {}",
                options.output.display()
            )),
        })?;
    }

    Ok(report)
}

/// Rotate the workspace HMAC key and update one handoff capsule in place.
pub fn rotate_handoff_key(options: &RotateKeyOptions) -> Result<RotateKeyReport, DomainError> {
    if !options.capsule.exists() {
        return Err(DomainError::Storage {
            message: format!("Capsule not found: {}", options.capsule.display()),
            repair: Some("Check the --capsule path and recreate the capsule if needed.".to_owned()),
        });
    }

    let content =
        std::fs::read_to_string(&options.capsule).map_err(|error| DomainError::Storage {
            message: format!("Failed to read capsule: {error}"),
            repair: Some(format!(
                "Verify {} exists and is readable",
                options.capsule.display()
            )),
        })?;
    let capsule: serde_json::Value =
        serde_json::from_str(&content).map_err(|error| DomainError::Storage {
            message: format!("Failed to parse capsule: {error}"),
            repair: Some("Verify the capsule file contains valid JSON".to_owned()),
        })?;

    verify_capsule_integrity(
        &capsule,
        &options.workspace,
        options.machine_salt_path.as_deref(),
    )?;

    let capsule_id = capsule
        .get("capsule_id")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("unknown")
        .to_owned();
    let old_integrity = capsule
        .get("integrity")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| {
            handoff_hmac_error(
                HANDOFF_HMAC_MISSING_CODE,
                "Handoff capsule is unsigned and cannot be rotated safely.",
                "Recreate the capsule with this ee version.",
            )
        })?;
    let key_mode = old_integrity
        .get("keyMode")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .to_owned();
    let bind_to_machine = match key_mode.as_str() {
        HANDOFF_HMAC_KEY_MODE_WORKSPACE_SECRET => false,
        HANDOFF_HMAC_KEY_MODE_MACHINE_BOUND => true,
        _ => {
            return Err(handoff_hmac_error(
                HANDOFF_CAPSULE_TAMPERED_CODE,
                "Handoff capsule declares an unknown HMAC key mode.",
                "Recreate the capsule with this ee version.",
            ));
        }
    };
    let old_hmac_prefix = old_integrity
        .get("hmacPrefix")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    let signed_body_before = signed_capsule_body(&capsule);
    let signed_body_before_bytes = canonical_json_string(&signed_body_before);
    let body_sha256 = format!("sha256:{}", sha256_hex(signed_body_before_bytes.as_bytes()));
    let canonical_before = compute_canonical_capsule_hash(&capsule);

    let new_workspace_secret = rotate_workspace_secret(&options.workspace)?;
    let machine_salt = if bind_to_machine {
        let path = machine_salt_path(options.machine_salt_path.as_deref());
        Some(read_existing_secret(&path, STRICT_MODE_NO_SALT_FILE_CODE)?)
    } else {
        None
    };
    let rotated_capsule = sign_capsule_content_with_key_material(
        signed_body_before.clone(),
        &new_workspace_secret,
        machine_salt.as_ref(),
        bind_to_machine,
    )?;
    let rotated_body_bytes = canonical_json_string(&signed_capsule_body(&rotated_capsule));
    let new_hmac_prefix = rotated_capsule
        .get("integrity")
        .and_then(|value| value.get("hmacPrefix"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .to_owned();
    let canonical_after = compute_canonical_capsule_hash(&rotated_capsule);
    let body_preserved = signed_body_before_bytes == rotated_body_bytes;

    let rotated_content = crate::core::serialize_pretty_or_error(&rotated_capsule);
    std::fs::write(&options.capsule, rotated_content).map_err(|error| DomainError::Storage {
        message: format!("Failed to write rotated capsule: {error}"),
        repair: Some(format!(
            "Check write permissions for {}",
            options.capsule.display()
        )),
    })?;

    let audit_id = record_handoff_hmac_rotate_audit(
        &options.workspace,
        &options.capsule,
        &capsule_id,
        &key_mode,
        old_hmac_prefix.as_deref(),
        &new_hmac_prefix,
        &body_sha256,
    );
    tracing::info!(
        event = "handoff_hmac_rotated",
        capsule_id = %capsule_id,
        old_hmac_prefix = old_hmac_prefix.as_deref().unwrap_or(""),
        new_hmac_prefix = %new_hmac_prefix,
        audit_entry_id = audit_id.as_deref().unwrap_or(""),
        "handoff capsule HMAC rotated"
    );

    let mut report = RotateKeyReport::new(capsule_id, options.capsule.clone());
    report.key_mode = key_mode;
    report.body_sha256 = body_sha256;
    report.old_hmac_prefix = old_hmac_prefix;
    report.new_hmac_prefix = new_hmac_prefix;
    report.canonical_content_hash_before = canonical_before;
    report.canonical_content_hash_after = canonical_after;
    report.body_preserved = body_preserved;
    report.audit_id = audit_id;
    Ok(report)
}

/// Inspect a handoff capsule.
pub fn inspect_handoff(options: &InspectOptions) -> Result<InspectReport, DomainError> {
    let mut report = InspectReport::new(options.path.clone());

    if !options.path.exists() {
        report.validation_status = ValidationStatus::Invalid.as_str().to_owned();
        report
            .warnings
            .push("Capsule file does not exist.".to_owned());
        return Ok(report);
    }

    let content = std::fs::read_to_string(&options.path).map_err(|e| DomainError::Storage {
        message: format!("Failed to read capsule: {e}"),
        repair: Some(format!(
            "Verify {} exists and is readable",
            options.path.display()
        )),
    })?;

    let capsule: serde_json::Value =
        serde_json::from_str(&content).map_err(|e| DomainError::Storage {
            message: format!("Failed to parse capsule: {e}"),
            repair: Some("Verify the capsule file contains valid JSON".to_owned()),
        })?;

    report.capsule_id = capsule
        .get("capsule_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_owned();

    report.capsule_schema = capsule
        .get("schema")
        .and_then(|v| v.as_str())
        .unwrap_or(HANDOFF_CAPSULE_SCHEMA_V1)
        .to_owned();

    report.profile = capsule
        .get("profile")
        .and_then(|v| v.as_str())
        .unwrap_or("resume")
        .to_owned();

    report.workspace_id = capsule
        .get("workspace")
        .and_then(|v| v.as_str())
        .map(|s| s.to_owned());

    if let Some(sections) = capsule.get("sections").and_then(|v| v.as_array()) {
        report.section_count = sections.len();
        for section in sections {
            if let Some(evidence) = section.get("evidence_ids").and_then(|v| v.as_array()) {
                report.evidence_count += evidence.len();
            }
        }
    }

    if options.verify_hash {
        let actual_hash = compute_content_hash(&content);
        report.hash_actual = Some(actual_hash.clone());
        if let Some(expected) = capsule.get("content_hash").and_then(|v| v.as_str()) {
            report.hash_expected = Some(expected.to_owned());
            report.hash_valid = actual_hash == expected;
            if !report.hash_valid {
                report.warnings.push("Content hash mismatch.".to_owned());
                report.validation_status = ValidationStatus::Invalid.as_str().to_owned();
            }
        }
    }

    Ok(report)
}

/// Resume from a handoff capsule.
pub fn resume_handoff(options: &ResumeOptions) -> Result<ResumeReport, DomainError> {
    let path = if options.use_latest {
        find_latest_capsule(&options.workspace)?
    } else {
        options.path.clone()
    };

    if !path.exists() {
        return Err(DomainError::Storage {
            message: format!("Capsule not found: {}", path.display()),
            repair: Some("Check the path or use 'latest' to find recent capsules".to_owned()),
        });
    }

    let content = std::fs::read_to_string(&path).map_err(|e| DomainError::Storage {
        message: format!("Failed to read capsule: {e}"),
        repair: Some(format!("Verify {} exists and is readable", path.display())),
    })?;

    let capsule: serde_json::Value =
        serde_json::from_str(&content).map_err(|e| DomainError::Storage {
            message: format!("Failed to parse capsule: {e}"),
            repair: Some("Verify the capsule file contains valid JSON".to_owned()),
        })?;

    let capsule_id = capsule
        .get("capsule_id")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_owned();

    let mut report = ResumeReport::new(capsule_id, path);
    if options.insecure_skip_hmac {
        record_handoff_insecure_load_audit(
            &options.workspace,
            &report.capsule_path,
            &content,
            &report.capsule_id,
        );
        report.degradations.push(
            DegradationInfo::new(
                HANDOFF_HMAC_SKIPPED_CODE,
                "Handoff capsule HMAC verification was explicitly skipped; resumed content may be tampered.",
            )
            .with_severity("high")
            .with_next_action("Recreate the capsule without --insecure-skip-hmac before trusting it."),
        );
        tracing::warn!(
            event = "handoff_hmac_insecure_load",
            capsule_id = %report.capsule_id,
            capsule_sha256 = %format!("sha256:{}", sha256_hex(content.as_bytes())),
            "handoff capsule loaded with HMAC verification disabled"
        );
    } else {
        verify_capsule_integrity(
            &capsule,
            &options.workspace,
            options.machine_salt_path.as_deref(),
        )?;
    }

    report.workspace = capsule
        .get("workspace")
        .and_then(|v| v.as_str())
        .map(|s| s.to_owned());

    report.active_focus = capsule
        .get("active_focus")
        .cloned()
        .filter(|value| !value.is_null());
    if let Some(active_focus) = &report.active_focus {
        if let Some(items) = active_focus.get("items").and_then(|value| value.as_array()) {
            for item in items {
                if let Some(memory_id) = item.get("memoryId").and_then(|value| value.as_str()) {
                    let reason = item
                        .get("reason")
                        .and_then(|value| value.as_str())
                        .unwrap_or("Included by passive focus state.");
                    report.selected_memories.push(SelectedMemory {
                        id: memory_id.to_owned(),
                        reason: format!("From active focus state: {reason}"),
                        confidence: "verified".to_owned(),
                    });
                }
            }
        }
    }

    let task_frame = match options.task_frame_id.as_deref() {
        Some(_) => {
            match read_handoff_task_frame(&options.workspace, options.task_frame_id.as_deref()) {
                Ok(Some(frame)) => task_frame_json(&frame),
                Ok(None) => None,
                Err(error) => {
                    report.degradations.push(
                        DegradationInfo::new(
                            "handoff_task_frame_unavailable",
                            format!("Task-frame state could not be read: {}", error.message()),
                        )
                        .with_next_action("ee task-frame show --active --json"),
                    );
                    None
                }
            }
        }
        None => capsule
            .get("task_frame")
            .cloned()
            .filter(|value| !value.is_null()),
    };
    if let Some(task_frame) = task_frame {
        add_task_frame_to_resume(&mut report, task_frame);
    }

    report.swarm_brief_summary = capsule
        .get("swarm_brief_summary")
        .cloned()
        .filter(|value| !value.is_null());
    if let Some(summary) = report.swarm_brief_summary.clone() {
        add_swarm_brief_summary_to_resume(&mut report, &summary);
    }

    if let Some(sections) = capsule.get("sections").and_then(|v| v.as_array()) {
        for section in sections {
            let section_id = section.get("id").and_then(|v| v.as_str()).unwrap_or("");
            let section_content = section
                .get("content")
                .and_then(|v| v.as_str())
                .unwrap_or("");

            match section_id {
                "objective" if report.current_objective.is_none() => {
                    report.current_objective = Some(section_content.to_owned());
                }
                "next_actions" => {
                    report.next_actions.push(
                        NextAction::new(1, section_content).with_reason("From handoff capsule."),
                    );
                }
                "decisions"
                    if !section_content.is_empty()
                        && section_content != "No recent decisions recorded." =>
                {
                    report.recent_decisions.push(section_content.to_owned());
                }
                _ => {}
            }
        }
    }

    if report.next_actions.is_empty() {
        report.next_actions.push(
            NextAction::new(1, "Review capsule contents and current state.")
                .with_reason("No explicit next actions in capsule."),
        );
    }

    // Bead bd-17c65.13.3 (M2): structured workspace identity
    // reconciliation. When the caller supplies a bound
    // WorkspaceIdentity AND the capsule carries a workspace_identity
    // block, run the four-level compare_workspace_identity. The
    // result populates both `workspace_match` (Exact/SoftSamePath/
    // SoftSameRepo/Hard) and `workspace_mismatch` (None/Soft/Hard via
    // to_mismatch_severity for M1 callers). Takes precedence over the
    // M1 string-ID path below.
    let capsule_identity = capsule
        .get("workspace_identity")
        .and_then(|v| serde_json::from_value::<WorkspaceIdentity>(v.clone()).ok());
    let mut m2_reconciled = false;
    if let (Some(bound_identity), Some(capsule_identity)) = (
        options.bound_workspace_identity.as_ref(),
        capsule_identity.as_ref(),
    ) {
        let match_level = compare_workspace_identity(capsule_identity, bound_identity);
        report.workspace_match = Some(match_level);
        report.workspace_mismatch = match_level.to_mismatch_severity();
        m2_reconciled = true;
        if !matches!(match_level, WorkspaceMatch::Exact) {
            let code = if matches!(match_level, WorkspaceMatch::Hard) {
                "workspace_mismatch_hard"
            } else {
                "workspace_mismatch_soft"
            };
            report.degradations.push(
                DegradationInfo::new(
                    code,
                    format!(
                        "Workspace identity match: {} (capsule fingerprint=`{}`, bound fingerprint=`{}`). Memory IDs and audit references may not resolve in the bound workspace.",
                        match_level.as_str(),
                        capsule_identity.fingerprint,
                        bound_identity.fingerprint
                    ),
                )
                .with_next_action(match match_level {
                    WorkspaceMatch::Hard => "re-import or rebuild the workspace before resuming",
                    _ => "verify the capsule was produced from the same workspace branch",
                }),
            );
        }
    }

    // Bead bd-17c65.13.2 (M1): legacy string-ID workspace mismatch
    // detection. Skipped when the M2 structured path already
    // reconciled.
    if !m2_reconciled {
        if let Some(bound) = options.bound_workspace_id.as_deref() {
            let capsule_workspace = report.workspace.as_deref();
            if let Some(cap) = capsule_workspace {
                if !workspace_ids_match(cap, bound) {
                    report.workspace_mismatch = WorkspaceMismatchSeverity::Hard;
                    report.degradations.push(
                        DegradationInfo::new(
                            "workspace_mismatch_hard",
                            format!(
                                "Capsule was created from workspace `{cap}` but this resume is bound to `{bound}`; memory IDs and audit references may not resolve in the bound workspace."
                            ),
                        )
                        .with_next_action("re-import or rebuild the workspace before resuming"),
                    );
                }
            } else {
                report.workspace_mismatch = WorkspaceMismatchSeverity::Soft;
                report.degradations.push(
                    DegradationInfo::new(
                        "workspace_mismatch_soft",
                        "Capsule did not record a workspace ID; cannot verify it matches the bound workspace.".to_owned(),
                    )
                    .with_next_action("regenerate the capsule with a workspace-aware version of ee handoff create"),
                );
            }
        }
    }

    // Bead bd-17c65.13.5 (M4): stale-snapshot detection. Compare the
    // workspace_state_hash captured in the capsule body against the
    // current workspace_state_hash recomputed from the live database.
    // When both hashes are available and differ, fire
    // `handoff_snapshot_stale` so the agent knows the resumed pack
    // may have drifted from current ground truth. When
    // `require_fresh` is set, drift converts to a hard error.
    let stale = compute_stale_snapshot_state(&capsule, &options.workspace);
    let stale_thresholds = handoff_stale_thresholds_for_workspace(&options.workspace);
    if let Some(s) = &stale {
        tracing::info!(
            event = "handoff_resume_drift_computed",
            capsule_id = %report.capsule_id,
            memories_added = ?s.memories_added_since,
            memories_expired = ?s.memories_expired_since,
            memories_revised = ?s.memories_revised_since,
            content_drift_score = ?s.content_drift_score,
            drift_detected = s.drift_detected,
            "handoff resume memory drift computed"
        );
        if let Some(breach) = stale_threshold_breach(s, &stale_thresholds) {
            tracing::info!(
                event = "handoff_stale_threshold_breached",
                code = HANDOFF_SNAPSHOT_STALE_CODE,
                severity = breach.severity,
                threshold_field = breach.threshold_field,
                threshold_value = %breach.threshold_value,
                observed_value = %breach.observed_value,
                "handoff stale snapshot threshold breached"
            );
            report.degradations.push(
                DegradationInfo::new(
                    HANDOFF_SNAPSHOT_STALE_CODE,
                    "Workspace memory set has drifted since this capsule was captured; the resumed pack may not reflect current ground truth.".to_owned(),
                )
                .with_severity(breach.severity)
                .with_next_action(breach.repair_hint),
            );
        }
    }
    report.stale_snapshot = stale;
    if options.require_fresh {
        if let Some(s) = &report.stale_snapshot {
            if s.drift_detected {
                return Err(DomainError::UnsatisfiedDegradedMode {
                    message: "Capsule snapshot is stale and --require-fresh was set; refresh the capsule before resuming.".to_owned(),
                    repair: Some("ee handoff create --workspace . --refresh".to_owned()),
                });
            }
        }
    }

    // Bead bd-17c65.13.2 (M1): prompt fragment. Render a markdown body
    // the next agent can prepend to its prompt. Stable structure for
    // greppability; pack.text in A4 follows the same pattern.
    if options.include_prompt_fragment {
        report.prompt_fragment = Some(render_resume_prompt_fragment(&report));
    }

    Ok(report)
}

/// Compute the stale-snapshot drift report by comparing the capsule's
/// embedded `workspace_state_hash` against the current workspace
/// state. Returns `None` only when neither side carries a hash —
/// otherwise emits a `StaleSnapshot` with explicit `Some`/`None`
/// fields so downstream callers know exactly what's measurable.
/// Bead bd-17c65.13.5 (M4).
fn compute_stale_snapshot_state(
    capsule: &serde_json::Value,
    workspace_path: &Path,
) -> Option<StaleSnapshot> {
    let captured = capsule
        .get("workspace_state_hash")
        .and_then(|v| v.as_str())
        .map(str::to_owned);

    let captured_at = capsule
        .get("memory_snapshot")
        .and_then(|v| v.get("captured_at"))
        .and_then(|v| v.as_str())
        .or_else(|| capsule.get("created_at").and_then(|v| v.as_str()))
        .map(str::to_owned);

    let captured_snapshot = capsule
        .get("memory_snapshot")
        .and_then(|v| serde_json::from_value::<CapsuleMemorySnapshot>(v.clone()).ok());

    let db_path = workspace_path.join(".ee").join("ee.db");
    let current = compute_workspace_state_hash(&db_path);
    let computed_at = Utc::now().to_rfc3339();
    let current_snapshot = compute_capsule_memory_snapshot(&db_path, &computed_at);

    if captured.is_none()
        && current.is_none()
        && captured_snapshot.is_none()
        && current_snapshot.is_none()
    {
        return None;
    }

    let hash_drift_detected = match (captured.as_deref(), current.as_deref()) {
        (Some(a), Some(b)) => a != b,
        _ => false,
    };
    let captured_at_time = captured_at.as_deref().and_then(parse_rfc3339_timestamp);
    let computed_at_time = parse_rfc3339_timestamp(&computed_at);

    let memories_added_since = match (
        captured_snapshot.as_ref(),
        current_snapshot.as_ref(),
        captured_at_time.as_ref(),
    ) {
        (Some(_), Some(snapshot), Some(captured_at)) => Some(
            snapshot
                .memories
                .iter()
                .filter(|memory| {
                    parse_rfc3339_timestamp(&memory.created_at)
                        .is_some_and(|created_at| &created_at > captured_at)
                })
                .count()
                .try_into()
                .unwrap_or(u64::MAX),
        ),
        _ => None,
    };

    let memories_expired_since = match (
        captured_snapshot.as_ref(),
        current_snapshot.as_ref(),
        captured_at_time.as_ref(),
        computed_at_time.as_ref(),
    ) {
        (Some(captured_snapshot), Some(current_snapshot), Some(captured_at), Some(computed_at)) => {
            let captured_by_key = snapshot_by_key(captured_snapshot);
            Some(
                current_snapshot
                    .memories
                    .iter()
                    .filter(|memory| captured_by_key.contains_key(&snapshot_key(memory)))
                    .filter(|memory| {
                        memory.valid_to.as_deref().is_some_and(|valid_to| {
                            parse_rfc3339_timestamp(valid_to).is_some_and(|valid_to| {
                                &valid_to > captured_at && &valid_to <= computed_at
                            })
                        })
                    })
                    .count()
                    .try_into()
                    .unwrap_or(u64::MAX),
            )
        }
        _ => None,
    };

    let memories_revised_since = match (captured_snapshot.as_ref(), current_snapshot.as_ref()) {
        (Some(captured_snapshot), Some(current_snapshot)) => {
            let current_by_key = snapshot_by_key(current_snapshot);
            Some(
                captured_snapshot
                    .memories
                    .iter()
                    .filter(|captured_memory| {
                        current_by_key
                            .get(&snapshot_key(captured_memory))
                            .is_some_and(|current_memory| {
                                current_memory.content_hash != captured_memory.content_hash
                            })
                    })
                    .count()
                    .try_into()
                    .unwrap_or(u64::MAX),
            )
        }
        _ => None,
    };

    let content_drift_score = match (
        captured_snapshot.as_ref(),
        current_snapshot.as_ref(),
        captured_at_time.as_ref(),
        computed_at_time.as_ref(),
    ) {
        (Some(captured_snapshot), Some(current_snapshot), Some(captured_at), Some(computed_at)) => {
            Some(snapshot_content_drift_score(
                captured_snapshot,
                captured_at,
                current_snapshot,
                computed_at,
            ))
        }
        _ => None,
    };

    let measured_drift_detected = memories_added_since.is_some_and(|count| count > 0)
        || memories_expired_since.is_some_and(|count| count > 0)
        || memories_revised_since.is_some_and(|count| count > 0)
        || content_drift_score.is_some_and(|score| score > 0.0);
    let drift_detected = hash_drift_detected || measured_drift_detected;
    let mut repair_hints = Vec::new();
    if memories_added_since.is_some_and(|count| count > 0) {
        repair_hints.push("Refresh: ee handoff create --refresh <capsule>".to_owned());
        repair_hints.push("Add new evidence: ee context --extend-capsule <capsule>".to_owned());
    }
    if memories_expired_since.is_some_and(|count| count > 0) {
        repair_hints.push(
            "Expired memories likely misleading; rebuild: ee handoff create --workspace . --refresh-evidence"
                .to_owned(),
        );
    }
    if content_drift_score.is_some_and(|score| score > 0.0) {
        repair_hints.push(
            "Significant drift; consider rebuilding: ee handoff create --workspace .".to_owned(),
        );
    }
    if memories_revised_since.is_some_and(|count| count > 0) {
        repair_hints.push("Refresh: ee handoff create --refresh <capsule>".to_owned());
    }
    repair_hints.sort();
    repair_hints.dedup();

    Some(StaleSnapshot {
        computed_at,
        captured_state_hash: captured,
        current_state_hash: current,
        drift_detected,
        memories_added_since,
        memories_expired_since,
        memories_revised_since,
        content_drift_score,
        repair_hints,
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct StaleThresholdBreach {
    severity: &'static str,
    threshold_field: &'static str,
    threshold_value: String,
    observed_value: String,
    repair_hint: &'static str,
}

fn handoff_stale_thresholds_for_workspace(workspace_path: &Path) -> HandoffStaleThresholds {
    let config_path = workspace_path.join(".ee").join("config.toml");
    let Ok(contents) = std::fs::read_to_string(config_path) else {
        return HandoffStaleThresholds::default();
    };
    let Ok(config) = crate::config::ConfigFile::parse(&contents) else {
        return HandoffStaleThresholds::default();
    };
    HandoffStaleThresholds::from_config(&config.handoff.stale_threshold)
}

fn stale_threshold_breach(
    snapshot: &StaleSnapshot,
    thresholds: &HandoffStaleThresholds,
) -> Option<StaleThresholdBreach> {
    let mut breaches = Vec::new();
    if snapshot
        .memories_added_since
        .is_some_and(|observed| observed > thresholds.memories_added)
    {
        breaches.push(StaleThresholdBreach {
            severity: "medium",
            threshold_field: "memories_added",
            threshold_value: thresholds.memories_added.to_string(),
            observed_value: snapshot
                .memories_added_since
                .unwrap_or_default()
                .to_string(),
            repair_hint: "Refresh: ee handoff create --refresh <capsule>",
        });
    }
    if thresholds.any_expired_in_pack
        && snapshot
            .memories_expired_since
            .is_some_and(|observed| observed > 0)
    {
        breaches.push(StaleThresholdBreach {
            severity: "high",
            threshold_field: "any_expired_in_pack",
            threshold_value: thresholds.any_expired_in_pack.to_string(),
            observed_value: snapshot
                .memories_expired_since
                .unwrap_or_default()
                .to_string(),
            repair_hint: "Expired memories likely misleading; rebuild: ee handoff create --workspace . --refresh-evidence",
        });
    }
    if snapshot
        .content_drift_score
        .is_some_and(|observed| observed > thresholds.content_drift_score)
    {
        breaches.push(StaleThresholdBreach {
            severity: "medium",
            threshold_field: "content_drift_score",
            threshold_value: thresholds.content_drift_score.to_string(),
            observed_value: format!("{:.6}", snapshot.content_drift_score.unwrap_or_default()),
            repair_hint: "Significant drift; consider rebuilding: ee handoff create --workspace .",
        });
    }
    if snapshot
        .memories_revised_since
        .is_some_and(|observed| observed > thresholds.memories_revised)
    {
        breaches.push(StaleThresholdBreach {
            severity: "low",
            threshold_field: "memories_revised",
            threshold_value: thresholds.memories_revised.to_string(),
            observed_value: snapshot
                .memories_revised_since
                .unwrap_or_default()
                .to_string(),
            repair_hint: "Refresh: ee handoff create --refresh <capsule>",
        });
    }
    breaches.into_iter().fold(None, |best, breach| match best {
        Some(current) if severity_rank(current.severity) >= severity_rank(breach.severity) => {
            Some(current)
        }
        _ => Some(breach),
    })
}

fn severity_rank(severity: &str) -> u8 {
    match severity {
        "high" => 3,
        "medium" => 2,
        "low" => 1,
        _ => 0,
    }
}

fn parse_rfc3339_timestamp(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|timestamp| timestamp.with_timezone(&Utc))
}

fn compute_capsule_memory_snapshot(
    database_path: &Path,
    captured_at: &str,
) -> Option<CapsuleMemorySnapshot> {
    let conn = crate::db::DbConnection::open_file(database_path).ok()?;
    let workspaces = conn.list_workspaces().ok()?;
    let mut memories = Vec::new();
    for workspace in &workspaces {
        let stored_memories = conn
            .list_memories_for_retrieval(&workspace.id, None, false)
            .ok()?;
        for memory in stored_memories {
            let tags = conn.get_memory_tags(&memory.id).unwrap_or_default();
            memories.push(snapshot_item_from_memory(&memory, &tags));
        }
    }
    let mut level_counts = BTreeMap::new();
    for memory in &memories {
        *level_counts.entry(memory.level.clone()).or_insert(0) += 1;
    }
    let valid_window = memory_snapshot_valid_window(&memories);
    memories.sort_by(|left, right| {
        snapshot_key(left)
            .cmp(&snapshot_key(right))
            .then_with(|| left.content_hash.cmp(&right.content_hash))
    });
    let content_hash_tree_root = memory_snapshot_tree_root(&memories);
    let memory_count = u64::try_from(memories.len()).unwrap_or(u64::MAX);
    Some(CapsuleMemorySnapshot {
        captured_at: captured_at.to_owned(),
        memory_count,
        level_counts,
        content_hash_tree_root,
        valid_window,
        memories,
    })
}

fn snapshot_item_from_memory(
    memory: &crate::db::StoredMemory,
    tags: &[String],
) -> CapsuleMemorySnapshotItem {
    let mut tags_sorted = tags.to_vec();
    tags_sorted.sort();
    CapsuleMemorySnapshotItem {
        workspace_id: memory.workspace_id.clone(),
        id: memory.id.clone(),
        level: memory.level.clone(),
        created_at: memory.created_at.clone(),
        updated_at: memory.updated_at.clone(),
        content_hash: snapshot_memory_content_hash(memory, &tags_sorted),
        valid_from: memory.valid_from.clone(),
        valid_to: memory.valid_to.clone(),
    }
}

fn memory_snapshot_valid_window(
    memories: &[CapsuleMemorySnapshotItem],
) -> CapsuleMemoryValidWindow {
    let min_valid_from = memories
        .iter()
        .filter_map(|memory| memory.valid_from.clone())
        .min();
    let max_valid_to = memories
        .iter()
        .filter_map(|memory| memory.valid_to.clone())
        .max();
    CapsuleMemoryValidWindow {
        min_valid_from,
        max_valid_to,
    }
}

fn memory_snapshot_tree_root(memories: &[CapsuleMemorySnapshotItem]) -> String {
    let mut hasher = blake3::Hasher::new();
    for memory in memories {
        hash_snapshot_field(&mut hasher, "workspace_id", &memory.workspace_id);
        hash_snapshot_field(&mut hasher, "id", &memory.id);
        hash_snapshot_field(&mut hasher, "content_hash", &memory.content_hash);
    }
    let digest = hasher.finalize().to_hex();
    format!("blake3:{}", &digest.as_str()[..16])
}

fn snapshot_memory_content_hash(memory: &crate::db::StoredMemory, tags: &[String]) -> String {
    let mut hasher = blake3::Hasher::new();
    hash_snapshot_field(&mut hasher, "workspace_id", &memory.workspace_id);
    hash_snapshot_field(&mut hasher, "id", &memory.id);
    hash_snapshot_field(&mut hasher, "level", &memory.level);
    hash_snapshot_field(&mut hasher, "kind", &memory.kind);
    hash_snapshot_field(&mut hasher, "content", &memory.content);
    hash_snapshot_field(&mut hasher, "tags", &tags.join(","));
    let digest = hasher.finalize().to_hex();
    format!("blake3:{}", &digest.as_str()[..16])
}

fn hash_snapshot_field(hasher: &mut blake3::Hasher, field: &str, value: &str) {
    hasher.update(field.as_bytes());
    hasher.update(b"\0");
    hasher.update(value.len().to_string().as_bytes());
    hasher.update(b":");
    hasher.update(value.as_bytes());
    hasher.update(b"\n");
}

fn snapshot_key(memory: &CapsuleMemorySnapshotItem) -> (String, String) {
    (memory.workspace_id.clone(), memory.id.clone())
}

fn snapshot_by_key(
    snapshot: &CapsuleMemorySnapshot,
) -> BTreeMap<(String, String), &CapsuleMemorySnapshotItem> {
    snapshot
        .memories
        .iter()
        .map(|memory| (snapshot_key(memory), memory))
        .collect()
}

fn active_snapshot_keys(
    snapshot: &CapsuleMemorySnapshot,
    as_of: &DateTime<Utc>,
) -> BTreeSet<(String, String)> {
    snapshot
        .memories
        .iter()
        .filter(|memory| {
            memory
                .valid_to
                .as_deref()
                .and_then(parse_rfc3339_timestamp)
                .is_none_or(|valid_to| &valid_to > as_of)
        })
        .map(snapshot_key)
        .collect()
}

fn snapshot_content_drift_score(
    captured_snapshot: &CapsuleMemorySnapshot,
    captured_at: &DateTime<Utc>,
    current_snapshot: &CapsuleMemorySnapshot,
    computed_at: &DateTime<Utc>,
) -> f64 {
    let captured_keys = active_snapshot_keys(captured_snapshot, captured_at);
    let current_keys = active_snapshot_keys(current_snapshot, computed_at);
    let union_count = captured_keys.union(&current_keys).count();
    if union_count == 0 {
        return 0.0;
    }
    let intersection_count = captured_keys.intersection(&current_keys).count();
    let score = 1.0 - (intersection_count as f64 / union_count as f64);
    (score * 1_000_000.0).round() / 1_000_000.0
}

/// Render the ready-to-prepend markdown body for `ee handoff resume`.
/// Same project-wide layout as the A4 pack.text contract: H1 title,
/// optional summary paragraph, then ## sections in a stable order:
/// objective, next actions, recent decisions, selected memories,
/// blockers. Empty sections are skipped (no "No X recorded" stubs in
/// the prompt — that would waste the next agent's tokens).
fn render_resume_prompt_fragment(report: &ResumeReport) -> ResumePromptFragment {
    let mut body = String::new();
    body.push_str(&format!("# Resumed Session: {}\n\n", report.capsule_id));

    if let Some(workspace) = &report.workspace {
        body.push_str(&format!("**Workspace:** `{}`\n\n", workspace));
    }
    if !matches!(report.workspace_mismatch, WorkspaceMismatchSeverity::None) {
        body.push_str(&format!(
            "**Workspace mismatch:** `{}` — see degradations below before relying on memory IDs.\n\n",
            report.workspace_mismatch.as_str()
        ));
    }

    if let Some(objective) = &report.current_objective {
        if !objective.is_empty() {
            body.push_str("## Current Objective\n\n");
            body.push_str(objective);
            body.push_str("\n\n");
        }
    }

    if !report.next_actions.is_empty() {
        body.push_str("## Next Actions\n\n");
        for (idx, action) in report.next_actions.iter().enumerate() {
            body.push_str(&format!("{}. {}\n", idx + 1, action.description));
            if !action.reason.is_empty() {
                body.push_str(&format!("   - *{}*\n", action.reason));
            }
        }
        body.push('\n');
    }

    if !report.recent_decisions.is_empty() {
        body.push_str("## Recent Decisions\n\n");
        for decision in &report.recent_decisions {
            body.push_str(&format!("- {decision}\n"));
        }
        body.push('\n');
    }

    if !report.selected_memories.is_empty() {
        body.push_str("## Selected Memories\n\n");
        for memory in &report.selected_memories {
            body.push_str(&format!("- `{}` — {}\n", memory.id, memory.reason));
        }
        body.push('\n');
    }

    if !report.blockers.is_empty() {
        body.push_str("## Blockers\n\n");
        for blocker in &report.blockers {
            body.push_str(&format!("- {}\n", blocker.description));
        }
        body.push('\n');
    }

    if !report.degradations.is_empty() {
        body.push_str("## Degradations\n\n");
        for degradation in &report.degradations {
            body.push_str(&format!(
                "- **{}** {}\n",
                degradation.code, degradation.message
            ));
        }
        body.push('\n');
    }

    // Token estimate: conservative chars/4 to match A4's convention.
    // Saturating cast guards the upper bound; bodies in the wild are
    // measured in low thousands of chars.
    let estimated_tokens = u32::try_from(body.len() / 4).unwrap_or(u32::MAX);
    let digest = blake3::hash(body.as_bytes()).to_hex();
    let hash = format!("blake3:{}", &digest.as_str()[..16]);

    ResumePromptFragment {
        text: body,
        estimated_tokens,
        hash,
    }
}

/// Compare two workspace IDs ignoring trivial cosmetic differences
/// (leading/trailing whitespace, case). The IDs are deterministic
/// BLAKE3-derived ULIDs in practice; this helper exists so a future
/// change to workspace ID format can centralize the comparison.
fn workspace_ids_match(a: &str, b: &str) -> bool {
    a.trim().eq_ignore_ascii_case(b.trim())
}

/// Compute the [`WorkspaceIdentity`] that the producer side embeds in
/// a handoff capsule (M2 / bd-17c65.13.3). Best-effort: when the
/// workspace path can't be canonicalized (e.g. it doesn't exist yet
/// for dry-run preview), the lexical path is used instead. The
/// fingerprint is always computable.
#[must_use]
pub fn compute_workspace_identity_for_capsule(workspace: &Path) -> WorkspaceIdentity {
    let canonical_root = workspace
        .canonicalize()
        .unwrap_or_else(|_| workspace.to_path_buf());
    let fingerprint = crate::config::workspace_fingerprint(&canonical_root);
    let scope = crate::config::derive_workspace_scope(&canonical_root);
    WorkspaceIdentity {
        fingerprint,
        canonical_root: canonical_root.to_string_lossy().into_owned(),
        scope_kind: scope.kind.as_str().to_owned(),
        repository_fingerprint: scope.repository_fingerprint.clone(),
    }
}

/// Compute a deterministic workspace_state_hash over the memory set in
/// `database_path` (bead bd-1xkxi, M3 follow-up). Hash is BLAKE3 over
/// the sorted canonical projection of every non-tombstoned memory in
/// every workspace — `level;kind;content;sorted_tags` per row. Same
/// projection scheme L2 uses for backup round-trip determinism, so a
/// capsule embedding this hash and an L2 backup of the same workspace
/// will land on the same fingerprint.
///
/// Returns `None` when the DB can't be opened (e.g. the workspace
/// hasn't been initialized yet) — capsule producers fall back to
/// embedding only the identity fingerprint in that case.
#[must_use]
pub fn compute_workspace_state_hash(database_path: &Path) -> Option<String> {
    let conn = crate::db::DbConnection::open_file(database_path).ok()?;
    let workspaces = conn.list_workspaces().ok()?;
    let mut projections: Vec<String> = Vec::new();
    for workspace in &workspaces {
        let memories = conn.list_memories(&workspace.id, None, false).ok()?;
        for memory in memories {
            let tags = conn.get_memory_tags(&memory.id).unwrap_or_default();
            let mut tags_sorted: Vec<String> = tags;
            tags_sorted.sort();
            projections.push(format!(
                "level={};kind={};content={};tags={}",
                memory.level,
                memory.kind,
                memory.content,
                tags_sorted.join(",")
            ));
        }
    }
    projections.sort();
    let joined = projections.join("\n");
    let digest = blake3::hash(joined.as_bytes()).to_hex();
    Some(format!("blake3:{}", &digest.as_str()[..16]))
}

/// Resolve a [`WorkspaceIdentity`] for a path that the resume-side
/// caller wants to bind to. Mirrors
/// [`compute_workspace_identity_for_capsule`] but lives here so the
/// asymmetry between producer/consumer code paths stays explicit.
#[must_use]
pub fn resolve_bound_workspace_identity(workspace: &Path) -> WorkspaceIdentity {
    compute_workspace_identity_for_capsule(workspace)
}

fn find_latest_capsule(workspace: &Path) -> Result<PathBuf, DomainError> {
    let ee_dir = workspace.join(".ee");
    if !ee_dir.exists() {
        return Err(DomainError::Storage {
            message: "No .ee directory found in workspace.".to_owned(),
            repair: Some("Run ee init to initialize the workspace.".to_owned()),
        });
    }

    let handoffs_dir = ee_dir.join("handoffs");
    if !handoffs_dir.exists() {
        return Err(DomainError::Storage {
            message: "No handoffs directory found.".to_owned(),
            repair: Some("Create a handoff capsule first with ee handoff create.".to_owned()),
        });
    }

    let mut latest: Option<(std::time::SystemTime, PathBuf)> = None;

    if let Ok(entries) = std::fs::read_dir(&handoffs_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "json") {
                if let Ok(metadata) = path.metadata() {
                    if let Ok(modified) = metadata.modified() {
                        match &latest {
                            None => latest = Some((modified, path)),
                            Some((prev_time, _)) if modified > *prev_time => {
                                latest = Some((modified, path));
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
    }

    latest
        .map(|(_, path)| path)
        .ok_or_else(|| DomainError::Storage {
            message: "No handoff capsules found.".to_owned(),
            repair: Some("Create a handoff capsule first with ee handoff create.".to_owned()),
        })
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use std::fmt::Debug;

    use super::*;

    type TestResult = Result<(), String>;

    fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
        if condition {
            Ok(())
        } else {
            Err(message.into())
        }
    }

    fn ensure_equal<T: Debug + PartialEq>(actual: &T, expected: &T, context: &str) -> TestResult {
        if actual == expected {
            Ok(())
        } else {
            Err(format!("{context}: expected {expected:?}, got {actual:?}"))
        }
    }

    fn repo_tempdir() -> Result<tempfile::TempDir, String> {
        tempfile::Builder::new()
            .prefix("ee-handoff-test-")
            .tempdir()
            .map_err(|error| error.to_string())
    }

    #[test]
    fn capsule_profile_has_stable_string_representation() -> TestResult {
        ensure_equal(&CapsuleProfile::Compact.as_str(), &"compact", "compact")?;
        ensure_equal(&CapsuleProfile::Resume.as_str(), &"resume", "resume")?;
        ensure_equal(&CapsuleProfile::Handoff.as_str(), &"handoff", "handoff")
    }

    #[test]
    fn capsule_profile_parses_from_string() -> TestResult {
        ensure_equal(
            &CapsuleProfile::parse("compact"),
            &Some(CapsuleProfile::Compact),
            "compact",
        )?;
        ensure_equal(
            &CapsuleProfile::parse("resume"),
            &Some(CapsuleProfile::Resume),
            "resume",
        )?;
        ensure_equal(
            &CapsuleProfile::parse("handoff"),
            &Some(CapsuleProfile::Handoff),
            "handoff",
        )?;
        ensure_equal(&CapsuleProfile::parse("invalid"), &None, "invalid")
    }

    #[test]
    fn capsule_profile_controls_section_count() -> TestResult {
        ensure(
            CapsuleProfile::Compact.max_sections() < CapsuleProfile::Resume.max_sections(),
            "compact < resume",
        )?;
        ensure(
            CapsuleProfile::Resume.max_sections() < CapsuleProfile::Handoff.max_sections(),
            "resume < handoff",
        )
    }

    #[test]
    fn canonical_capsule_hash_ignores_registered_and_value_dependent_volatiles() -> TestResult {
        let stable_section = serde_json::json!({
            "id": "objective",
            "title": "Current Objective",
            "content": "Continue deterministic handoff work.",
        });
        let first = serde_json::json!({
            "schema": HANDOFF_CAPSULE_SCHEMA_V1,
            "capsule_id": "cap_first",
            "created_at": "2026-05-16T00:00:00Z",
            "integrity": {"hmac": "secret-one", "hmacPrefix": "aaa"},
            "swarm_brief_summary": {"hostname": "agent-host-a", "generatedAt": "now"},
            "sections": [
                stable_section.clone(),
                {
                    "id": "swarm_brief_summary",
                    "title": "Swarm Brief Summary",
                    "content": "host-specific diagnostic body A",
                    "evidence_ids": ["evidence-a"],
                    "token_estimate": 99,
                }
            ]
        });
        let second = serde_json::json!({
            "schema": HANDOFF_CAPSULE_SCHEMA_V1,
            "capsule_id": "cap_second",
            "created_at": "2026-05-16T00:01:00Z",
            "integrity": {"hmac": "secret-two", "hmacPrefix": "bbb"},
            "swarm_brief_summary": {"hostname": "agent-host-b", "generatedAt": "later"},
            "sections": [
                stable_section,
                {
                    "id": "swarm_brief_summary",
                    "title": "Swarm Brief Summary",
                    "content": "host-specific diagnostic body B",
                    "evidence_ids": ["evidence-b"],
                    "token_estimate": 5,
                }
            ]
        });

        let first_hash = compute_canonical_capsule_hash(&first);
        let second_hash = compute_canonical_capsule_hash(&second);
        ensure_equal(
            &first_hash,
            &second_hash,
            "volatile capsule fields do not affect canonical hash",
        )?;

        let changed = serde_json::json!({
            "schema": HANDOFF_CAPSULE_SCHEMA_V1,
            "capsule_id": "cap_third",
            "created_at": "2026-05-16T00:02:00Z",
            "integrity": {"hmac": "secret-three", "hmacPrefix": "ccc"},
            "sections": [
                {
                    "id": "objective",
                    "title": "Current Objective",
                    "content": "A durable section changed.",
                }
            ]
        });
        ensure(
            first_hash != compute_canonical_capsule_hash(&changed),
            "durable capsule content still affects canonical hash",
        )
    }

    #[test]
    fn evidence_confidence_has_stable_string_representation() -> TestResult {
        ensure_equal(
            &EvidenceConfidence::Verified.as_str(),
            &"verified",
            "verified",
        )?;
        ensure_equal(
            &EvidenceConfidence::Inferred.as_str(),
            &"inferred",
            "inferred",
        )?;
        ensure_equal(&EvidenceConfidence::Stale.as_str(), &"stale", "stale")?;
        ensure_equal(&EvidenceConfidence::Unknown.as_str(), &"unknown", "unknown")
    }

    #[test]
    fn capsule_section_builder_works() -> TestResult {
        let section = CapsuleSection::new("test", "Test Section")
            .with_content("Some content here")
            .with_confidence(EvidenceConfidence::Verified)
            .with_evidence(vec!["ev_1".to_owned()]);

        ensure_equal(&section.id, &"test".to_owned(), "id")?;
        ensure_equal(&section.title, &"Test Section".to_owned(), "title")?;
        ensure_equal(
            &section.confidence,
            &EvidenceConfidence::Verified,
            "confidence",
        )?;
        ensure(
            section.token_estimate > 0,
            "token estimate should be positive",
        )?;
        ensure_equal(&section.evidence_ids.len(), &1, "evidence count")
    }

    #[test]
    fn next_action_builder_works() -> TestResult {
        let action = NextAction::new(1, "Do something")
            .with_command("ee status")
            .with_reason("Because.");

        ensure_equal(&action.priority, &1, "priority")?;
        ensure_equal(
            &action.description,
            &"Do something".to_owned(),
            "description",
        )?;
        ensure_equal(
            &action.suggested_command,
            &Some("ee status".to_owned()),
            "command",
        )?;
        ensure_equal(&action.reason, &"Because.".to_owned(), "reason")
    }

    #[test]
    fn blocker_builder_works() -> TestResult {
        let blocker = Blocker::new("block_1", "Something is blocking")
            .with_resolution("Fix it")
            .hard();

        ensure_equal(&blocker.id, &"block_1".to_owned(), "id")?;
        ensure_equal(&blocker.hard, &true, "hard")?;
        ensure_equal(
            &blocker.resolution,
            &Some("Fix it".to_owned()),
            "resolution",
        )
    }

    #[test]
    fn preview_report_uses_correct_schema() -> TestResult {
        let report = PreviewReport::new(PathBuf::from("."), CapsuleProfile::Resume);
        ensure_equal(
            &report.schema,
            &HANDOFF_PREVIEW_SCHEMA_V1.to_owned(),
            "schema",
        )
    }

    #[test]
    fn create_report_uses_correct_schema() -> TestResult {
        let report = CreateReport::new(
            "test".to_owned(),
            PathBuf::from("."),
            PathBuf::from("out.json"),
        );
        ensure_equal(
            &report.schema,
            &HANDOFF_CREATE_SCHEMA_V1.to_owned(),
            "schema",
        )
    }

    #[test]
    fn inspect_report_uses_correct_schema() -> TestResult {
        let report = InspectReport::new(PathBuf::from("test.json"));
        ensure_equal(
            &report.schema,
            &HANDOFF_INSPECT_SCHEMA_V1.to_owned(),
            "schema",
        )
    }

    #[test]
    fn resume_report_uses_correct_schema() -> TestResult {
        let report = ResumeReport::new("test".to_owned(), PathBuf::from("test.json"));
        ensure_equal(
            &report.schema,
            &HANDOFF_RESUME_SCHEMA_V1.to_owned(),
            "schema",
        )
    }

    #[test]
    fn handoff_preview_create_and_resume_include_active_focus() -> TestResult {
        let dir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let memory_id = crate::models::MemoryId::from_uuid(uuid::Uuid::from_u128(99)).to_string();
        crate::core::focus::set_focus(&crate::core::focus::FocusSetOptions {
            workspace_path: dir.path().to_path_buf(),
            memory_ids: vec![memory_id.clone()],
            focal_memory_id: Some(memory_id.clone()),
            pinned_memory_ids: vec![memory_id.clone()],
            capacity: 3,
            reason: "handoff continuity".to_owned(),
            provenance: vec!["test".to_owned()],
            scope: crate::core::focus::FocusScope::default(),
        })
        .map_err(|error| error.message())?;

        let preview = preview_handoff(&PreviewOptions {
            workspace: dir.path().to_path_buf(),
            profile: CapsuleProfile::Resume,
            since: None,
            include_estimates: true,
            task_frame_id: None,
        })
        .map_err(|error| error.message())?;
        ensure(preview.active_focus.is_some(), "preview includes focus")?;
        ensure(
            preview
                .planned_sections
                .iter()
                .any(|section| section.id == "active_focus"),
            "preview includes focus section",
        )?;

        let output = dir.path().join("handoff.json");
        let create = create_handoff(&CreateOptions {
            workspace: dir.path().to_path_buf(),
            output: output.clone(),
            profile: CapsuleProfile::Resume,
            since: None,
            dry_run: false,
            task_frame_id: None,
            bind_to_machine: false,
            machine_salt_path: None,
            redaction_level: RedactionLevel::Standard,
        })
        .map_err(|error| error.message())?;
        ensure(create.active_focus.is_some(), "create includes focus")?;

        let resume = resume_handoff(&ResumeOptions {
            path: output,
            use_latest: false,
            workspace: dir.path().to_path_buf(),
            max_sections: None,
            task_frame_id: None,
            bound_workspace_id: None,
            bound_workspace_identity: None,
            include_prompt_fragment: true,
            require_fresh: false,
            insecure_skip_hmac: false,
            machine_salt_path: None,
        })
        .map_err(|error| error.message())?;
        ensure(resume.active_focus.is_some(), "resume includes focus")?;
        ensure(
            resume
                .selected_memories
                .iter()
                .any(|memory| memory.id == memory_id),
            "resume selected focused memory",
        )
    }

    #[test]
    fn handoff_preview_create_and_resume_include_redacted_task_frame() -> TestResult {
        let dir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let created = crate::core::task_frame::create_task_frame(
            &crate::core::task_frame::TaskFrameCreateOptions {
                workspace_path: dir.path().to_path_buf(),
                goal: "Continue release with api_key=sk-live-123".to_owned(),
                actor: "cod-pane6".to_owned(),
                status: crate::core::task_frame::TaskFrameStatus::Active,
                current_focus: Some("verify handoff".to_owned()),
                blockers: vec!["password=hunter2 still unavailable".to_owned()],
                evidence_links: vec![
                    crate::core::task_frame::TaskEvidenceLink {
                        kind: "memory".to_owned(),
                        id: "mem_task_frame".to_owned(),
                    },
                    crate::core::task_frame::TaskEvidenceLink {
                        kind: "context_pack".to_owned(),
                        id: "pack_task_frame".to_owned(),
                    },
                ],
                created_at: Some("2026-05-04T00:00:00Z".to_owned()),
                dry_run: false,
            },
        )
        .map_err(|error| error.message())?;
        let frame_id = created.frame.ok_or_else(|| "missing frame".to_owned())?.id;

        let preview = preview_handoff(&PreviewOptions {
            workspace: dir.path().to_path_buf(),
            profile: CapsuleProfile::Resume,
            since: None,
            include_estimates: true,
            task_frame_id: Some(frame_id.clone()),
        })
        .map_err(|error| error.message())?;
        ensure(preview.task_frame.is_some(), "preview includes task frame")?;
        ensure(
            preview
                .planned_sections
                .iter()
                .any(|section| section.id == "task_frame"),
            "preview includes task-frame section",
        )?;

        let output = dir.path().join("handoff.json");
        let create = create_handoff(&CreateOptions {
            workspace: dir.path().to_path_buf(),
            output: output.clone(),
            profile: CapsuleProfile::Resume,
            since: None,
            dry_run: false,
            task_frame_id: Some(frame_id.clone()),
            bind_to_machine: false,
            machine_salt_path: None,
            redaction_level: RedactionLevel::Standard,
        })
        .map_err(|error| error.message())?;
        ensure(create.task_frame.is_some(), "create includes task frame")?;

        let capsule_text = std::fs::read_to_string(&output).map_err(|error| error.to_string())?;
        ensure(
            !capsule_text.contains("sk-live-123") && !capsule_text.contains("hunter2"),
            "capsule must contain redacted task-frame state",
        )?;

        let resume = resume_handoff(&ResumeOptions {
            path: output,
            use_latest: false,
            workspace: dir.path().to_path_buf(),
            max_sections: None,
            task_frame_id: None,
            bound_workspace_id: None,
            bound_workspace_identity: None,
            include_prompt_fragment: true,
            require_fresh: false,
            insecure_skip_hmac: false,
            machine_salt_path: None,
        })
        .map_err(|error| error.message())?;
        ensure(resume.task_frame.is_some(), "resume includes task frame")?;
        ensure_equal(
            &resume.current_objective,
            &Some("Continue release with api_key=[REDACTED:api_key]".to_owned()),
            "resume objective comes from redacted task frame",
        )?;
        ensure(
            resume
                .selected_memories
                .iter()
                .any(|memory| memory.id == "mem_task_frame"),
            "resume links task-frame memory evidence",
        )?;
        ensure(
            resume
                .artifact_pointers
                .iter()
                .any(|artifact| artifact.id == "pack_task_frame"),
            "resume links task-frame artifacts",
        )
    }

    #[test]
    fn capsule_id_has_correct_prefix() -> TestResult {
        let id = generate_capsule_id();
        ensure(
            id.starts_with(HANDOFF_CAPSULE_ID_PREFIX),
            "capsule ID prefix",
        )
    }

    #[test]
    fn content_hash_is_deterministic() -> TestResult {
        let content = "test content";
        let hash1 = compute_content_hash(content);
        let hash2 = compute_content_hash(content);
        ensure_equal(&hash1, &hash2, "hash determinism")
    }

    #[test]
    fn token_estimation_approximates_word_count() -> TestResult {
        let text = "one two three four";
        let estimate = estimate_tokens(text);
        ensure(estimate >= 4, "at least one token per word")?;
        ensure(estimate <= 8, "not more than double word count")
    }

    fn create_test_capsule(
        workspace: &Path,
        bind_to_machine: bool,
        machine_salt_path: Option<PathBuf>,
    ) -> Result<PathBuf, String> {
        let output = workspace.join("handoff.json");
        create_handoff(&CreateOptions {
            workspace: workspace.to_path_buf(),
            output: output.clone(),
            profile: CapsuleProfile::Resume,
            since: None,
            dry_run: false,
            task_frame_id: None,
            bind_to_machine,
            machine_salt_path,
            redaction_level: RedactionLevel::Standard,
        })
        .map_err(|error| error.message())?;
        Ok(output)
    }

    fn capsule_integrity(path: &Path) -> Result<serde_json::Value, String> {
        let capsule: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(path).map_err(|error| error.to_string())?)
                .map_err(|error| error.to_string())?;
        capsule
            .get("integrity")
            .cloned()
            .ok_or_else(|| "missing integrity".to_owned())
    }

    #[test]
    fn handoff_hmac_default_mode_round_trips_same_workspace() -> TestResult {
        let dir = repo_tempdir()?;
        let output = create_test_capsule(dir.path(), false, None)?;
        let capsule_text = fs::read_to_string(&output).map_err(|error| error.to_string())?;
        ensure(
            capsule_text.contains("\"integrity\""),
            "capsule carries integrity block",
        )?;

        let resume = resume_handoff(&ResumeOptions {
            path: output,
            use_latest: false,
            workspace: dir.path().to_path_buf(),
            max_sections: None,
            task_frame_id: None,
            bound_workspace_id: None,
            bound_workspace_identity: None,
            include_prompt_fragment: false,
            require_fresh: false,
            insecure_skip_hmac: false,
            machine_salt_path: None,
        })
        .map_err(|error| error.message())?;
        ensure(
            !resume
                .degradations
                .iter()
                .any(|degradation| degradation.code == HANDOFF_HMAC_SKIPPED_CODE),
            "verified resume must not emit hmac skipped degradation",
        )
    }

    #[test]
    fn handoff_hmac_signing_same_body_is_deterministic_three_times() -> TestResult {
        let dir = repo_tempdir()?;
        let output = create_test_capsule(dir.path(), false, None)?;
        let capsule: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&output).map_err(|error| error.to_string())?)
                .map_err(|error| error.to_string())?;
        let signed_body = signed_capsule_body(&capsule);
        let workspace_secret_bytes =
            fs::read(hmac_workspace_secret_path(dir.path())).map_err(|error| error.to_string())?;
        let workspace_secret: [u8; 32] = workspace_secret_bytes
            .as_slice()
            .try_into()
            .map_err(|_| "workspace hmac key should be 32 bytes".to_owned())?;
        let mut observed = Vec::new();

        for _ in 0..3 {
            let signed = sign_capsule_content_with_key_material(
                signed_body.clone(),
                &workspace_secret,
                None,
                false,
            )
            .map_err(|error| error.message())?;
            let hmac = signed
                .get("integrity")
                .and_then(|value| value.get("hmac"))
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| "missing signed hmac".to_owned())?
                .to_owned();
            observed.push(hmac);
        }

        ensure_equal(&observed.len(), &3_usize, "three hmac observations")?;
        ensure_equal(&observed[0], &observed[1], "first two hmacs match")?;
        ensure_equal(&observed[1], &observed[2], "last two hmacs match")
    }

    #[test]
    fn handoff_hmac_missing_fails_closed() -> TestResult {
        let dir = repo_tempdir()?;
        let output = create_test_capsule(dir.path(), false, None)?;
        let mut capsule: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&output).map_err(|error| error.to_string())?)
                .map_err(|error| error.to_string())?;
        let Some(capsule_object) = capsule.as_object_mut() else {
            return Err("capsule object".to_owned());
        };
        capsule_object.remove("integrity");
        fs::write(
            &output,
            serde_json::to_string_pretty(&capsule).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;

        let error = match resume_handoff(&ResumeOptions {
            path: output,
            use_latest: false,
            workspace: dir.path().to_path_buf(),
            max_sections: None,
            task_frame_id: None,
            bound_workspace_id: None,
            bound_workspace_identity: None,
            include_prompt_fragment: false,
            require_fresh: false,
            insecure_skip_hmac: false,
            machine_salt_path: None,
        }) {
            Ok(_) => panic!("unsigned capsule should fail closed"),
            Err(error) => error,
        };
        ensure_equal(&error.code(), &HANDOFF_HMAC_MISSING_CODE, "error code")
    }

    #[test]
    fn handoff_hmac_tampered_body_fails_closed() -> TestResult {
        let dir = repo_tempdir()?;
        let output = create_test_capsule(dir.path(), false, None)?;
        let capsule: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&output).map_err(|error| error.to_string())?)
                .map_err(|error| error.to_string())?;

        for index in 0..10 {
            let mut tampered = capsule.clone();
            if index == 0 {
                tampered["sections"][0]["content"] = serde_json::Value::String(
                    "tampered workspace section at signed body position zero".to_owned(),
                );
            } else {
                let key = format!("tamper_marker_{index:02}");
                tampered[key.as_str()] = serde_json::json!({
                    "mutation": index,
                    "signedBodyChange": true
                });
            }
            fs::write(
                &output,
                serde_json::to_string_pretty(&tampered).map_err(|error| error.to_string())?,
            )
            .map_err(|error| error.to_string())?;

            let error = match resume_handoff(&ResumeOptions {
                path: output.clone(),
                use_latest: false,
                workspace: dir.path().to_path_buf(),
                max_sections: None,
                task_frame_id: None,
                bound_workspace_id: None,
                bound_workspace_identity: None,
                include_prompt_fragment: false,
                require_fresh: false,
                insecure_skip_hmac: false,
                machine_salt_path: None,
            }) {
                Ok(_) => panic!("tampered capsule should fail closed"),
                Err(error) => error,
            };
            ensure_equal(
                &error.code(),
                &HANDOFF_CAPSULE_TAMPERED_CODE,
                &format!("error code for mutation {index}"),
            )?;
        }

        Ok(())
    }

    #[test]
    fn handoff_hmac_insecure_skip_loads_with_high_degradation() -> TestResult {
        let dir = repo_tempdir()?;
        let output = create_test_capsule(dir.path(), false, None)?;
        let mut capsule: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&output).map_err(|error| error.to_string())?)
                .map_err(|error| error.to_string())?;
        capsule["sections"][0]["content"] =
            serde_json::Value::String("tampered but explicitly bypassed".to_owned());
        fs::write(
            &output,
            serde_json::to_string_pretty(&capsule).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;

        let report = resume_handoff(&ResumeOptions {
            path: output,
            use_latest: false,
            workspace: dir.path().to_path_buf(),
            max_sections: None,
            task_frame_id: None,
            bound_workspace_id: None,
            bound_workspace_identity: None,
            include_prompt_fragment: false,
            require_fresh: false,
            insecure_skip_hmac: true,
            machine_salt_path: None,
        })
        .map_err(|error| error.message())?;
        let degradation = report
            .degradations
            .iter()
            .find(|degradation| degradation.code == HANDOFF_HMAC_SKIPPED_CODE)
            .ok_or_else(|| "missing handoff_hmac_skipped degradation".to_owned())?;
        ensure_equal(
            &degradation.severity.as_deref(),
            &Some("high"),
            "degradation severity",
        )
    }

    #[test]
    fn handoff_hmac_strict_missing_salt_fails_closed() -> TestResult {
        let dir = repo_tempdir()?;
        let salt_dir = repo_tempdir()?;
        let create_salt = salt_dir.path().join("machine-a-salt");
        let resume_salt = salt_dir.path().join("missing-machine-b-salt");
        let output = create_test_capsule(dir.path(), true, Some(create_salt))?;

        let error = match resume_handoff(&ResumeOptions {
            path: output,
            use_latest: false,
            workspace: dir.path().to_path_buf(),
            max_sections: None,
            task_frame_id: None,
            bound_workspace_id: None,
            bound_workspace_identity: None,
            include_prompt_fragment: false,
            require_fresh: false,
            insecure_skip_hmac: false,
            machine_salt_path: Some(resume_salt),
        }) {
            Ok(_) => panic!("strict capsule should fail when machine salt is missing"),
            Err(error) => error,
        };
        ensure_equal(&error.code(), &STRICT_MODE_NO_SALT_FILE_CODE, "error code")
    }

    #[test]
    fn handoff_hmac_rotate_key_preserves_body_and_changes_hmac() -> TestResult {
        let dir = repo_tempdir()?;
        let db_path = dir.path().join(".ee").join("ee.db");
        if let Some(parent) = db_path.parent() {
            fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        let conn = DbConnection::open_file(&db_path).map_err(|error| error.to_string())?;
        conn.migrate().map_err(|error| error.to_string())?;

        let output = create_test_capsule(dir.path(), false, None)?;
        let before = capsule_integrity(&output)?;
        let before_prefix = before
            .get("hmacPrefix")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| "missing old hmac prefix".to_owned())?
            .to_owned();

        let report = rotate_handoff_key(&RotateKeyOptions {
            workspace: dir.path().to_path_buf(),
            capsule: output.clone(),
            machine_salt_path: None,
        })
        .map_err(|error| error.message())?;
        let after = capsule_integrity(&output)?;
        let after_prefix = after
            .get("hmacPrefix")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| "missing new hmac prefix".to_owned())?
            .to_owned();

        ensure(report.body_preserved, "rotated capsule body preserved")?;
        ensure_equal(
            &report.canonical_content_hash_before,
            &report.canonical_content_hash_after,
            "canonical hash preserved",
        )?;
        ensure(before_prefix != after_prefix, "hmac prefix changed")?;
        ensure_equal(&report.new_hmac_prefix, &after_prefix, "report new prefix")?;
        ensure(report.audit_id.is_some(), "rotation audit id recorded")?;

        let resume = resume_handoff(&ResumeOptions {
            path: output,
            use_latest: false,
            workspace: dir.path().to_path_buf(),
            max_sections: None,
            task_frame_id: None,
            bound_workspace_id: None,
            bound_workspace_identity: None,
            include_prompt_fragment: false,
            require_fresh: false,
            insecure_skip_hmac: false,
            machine_salt_path: None,
        })
        .map_err(|error| error.message())?;
        ensure_equal(
            &resume.capsule_id,
            &report.capsule_id,
            "rotated capsule resumes",
        )
    }

    #[cfg(unix)]
    #[test]
    fn handoff_hmac_key_files_are_private_on_posix() -> TestResult {
        use std::os::unix::fs::PermissionsExt;

        let dir = repo_tempdir()?;
        let salt_dir = repo_tempdir()?;
        let machine_salt = salt_dir.path().join("machine-salt");
        let _output = create_test_capsule(dir.path(), true, Some(machine_salt.clone()))?;

        let workspace_key = hmac_workspace_secret_path(dir.path());
        let workspace_mode = fs::metadata(&workspace_key)
            .map_err(|error| error.to_string())?
            .permissions()
            .mode()
            & 0o777;
        let salt_mode = fs::metadata(&machine_salt)
            .map_err(|error| error.to_string())?
            .permissions()
            .mode()
            & 0o777;
        ensure_equal(&workspace_mode, &0o600, "workspace hmac key mode")?;
        ensure_equal(&salt_mode, &0o600, "machine salt mode")
    }

    #[test]
    fn handoff_hmac_rotate_report_redacts_secret_material() -> TestResult {
        let dir = repo_tempdir()?;
        let db_path = dir.path().join(".ee").join("ee.db");
        if let Some(parent) = db_path.parent() {
            fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        let conn = DbConnection::open_file(&db_path).map_err(|error| error.to_string())?;
        conn.migrate().map_err(|error| error.to_string())?;

        let output = create_test_capsule(dir.path(), false, None)?;
        let workspace_secret_bytes =
            fs::read(hmac_workspace_secret_path(dir.path())).map_err(|error| error.to_string())?;
        let workspace_secret_hex = bytes_to_lower_hex(&workspace_secret_bytes);
        let workspace_secret_b64 = URL_SAFE_NO_PAD.encode(&workspace_secret_bytes);
        let old_integrity = capsule_integrity(&output)?;
        let old_full_hmac = old_integrity
            .get("hmac")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| "missing old full hmac".to_owned())?
            .to_owned();
        let report = rotate_handoff_key(&RotateKeyOptions {
            workspace: dir.path().to_path_buf(),
            capsule: output.clone(),
            machine_salt_path: None,
        })
        .map_err(|error| error.message())?;
        let new_integrity = capsule_integrity(&output)?;
        let new_full_hmac = new_integrity
            .get("hmac")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| "missing new full hmac".to_owned())?
            .to_owned();
        let audit_id = report
            .audit_id
            .as_deref()
            .ok_or_else(|| "missing rotate audit id".to_owned())?;
        let audit = conn
            .get_audit(audit_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "missing rotate audit row".to_owned())?;
        let audit_details = audit.details.unwrap_or_default();
        let rendered = crate::core::serialize_or_error(&report);
        let redaction_surface = format!("{rendered}\n{audit_details}");

        ensure(
            !redaction_surface.contains(&old_full_hmac),
            "report/audit must not expose old full hmac",
        )?;
        ensure(
            !redaction_surface.contains(&new_full_hmac),
            "report/audit must not expose new full hmac",
        )?;
        ensure(
            !redaction_surface.contains(&workspace_secret_hex),
            "report/audit must not expose workspace secret hex",
        )?;
        ensure(
            !redaction_surface.contains(&workspace_secret_b64),
            "report/audit must not expose workspace secret base64url",
        )?;
        ensure(
            rendered.contains(&report.new_hmac_prefix),
            "report keeps bounded hmac prefix",
        )?;
        ensure(
            audit_details.contains("\"secretMaterialLogged\":false"),
            "audit row explicitly records that secret material was not logged",
        )
    }

    #[test]
    fn preview_handoff_returns_valid_report() -> TestResult {
        let options = PreviewOptions {
            workspace: PathBuf::from("."),
            profile: CapsuleProfile::Resume,
            since: None,
            include_estimates: true,
            task_frame_id: None,
        };

        let result = preview_handoff(&options);
        ensure(result.is_ok(), "preview should succeed")?;

        let report = result.map_err(|e| e.message())?;
        ensure(
            !report.planned_sections.is_empty(),
            "should have planned sections",
        )
    }

    #[test]
    fn handoff_includes_redaction_safe_singleflight_posture() -> TestResult {
        let options = PreviewOptions {
            workspace: PathBuf::from("."),
            profile: CapsuleProfile::Resume,
            since: None,
            include_estimates: true,
            task_frame_id: None,
        };

        let report = preview_handoff(&options).map_err(|error| error.message())?;
        ensure(
            report
                .planned_sections
                .iter()
                .any(|section| section.id == "singleflight_posture"),
            "preview plans single-flight posture section",
        )?;

        let rendered = render_singleflight_posture_for_handoff(
            &SingleFlightPostureReport::from_surfaces(vec![SingleFlightSurfacePosture::new(
                SingleFlightSurface::GraphFeatureEnrichment,
                true,
                0,
                SingleFlightSurfaceCounters {
                    leader_start_count: 1,
                    completed_leader_count: 1,
                    follower_join_count: 2,
                    follower_timeout_count: 0,
                    leader_failure_count: 0,
                    reused_result_count: 2,
                    state_poisoned_count: 0,
                },
                250,
                Some(SingleFlightLastKeyPosture {
                    key_hash: "abc123redacted".to_owned(),
                    workspace_generation: 9,
                    index_generation: Some(4),
                    graph_generation: Some(7),
                }),
            )]),
        );
        ensure(
            rendered.contains("graph_feature_enrichment"),
            "handoff summary names configured surface",
        )?;
        ensure(
            rendered.contains("key_hash=abc123redacted")
                && rendered.contains("workspace_generation=9")
                && rendered.contains("index_generation=4")
                && rendered.contains("graph_generation=7"),
            "handoff summary includes redaction-safe key hash and generation details",
        )?;
        ensure(
            !rendered.contains("raw_query") && !rendered.contains("memory_body"),
            "handoff summary omits raw query and memory body fields",
        )
    }

    #[test]
    fn validation_status_has_stable_string_representation() -> TestResult {
        ensure_equal(&ValidationStatus::Valid.as_str(), &"valid", "valid")?;
        ensure_equal(&ValidationStatus::Invalid.as_str(), &"invalid", "invalid")?;
        ensure_equal(&ValidationStatus::Stale.as_str(), &"stale", "stale")?;
        ensure_equal(&ValidationStatus::Partial.as_str(), &"partial", "partial")
    }
}
