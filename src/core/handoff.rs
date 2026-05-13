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

use std::path::{Path, PathBuf};

use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::core::focus::{focus_state_hash, read_active_focus_state};
use crate::core::swarm_brief::{
    collect_swarm_brief_summary, render_swarm_brief_summary_for_handoff,
    swarm_brief_summary_evidence_id,
};
use crate::core::task_frame::{
    NON_EXECUTING_CONTRACT, TaskFrameRecord, TaskFrameShowOptions, show_task_frame,
};
use crate::models::DomainError;

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
    /// Suggested next action.
    pub next_action: Option<String>,
}

impl DegradationInfo {
    #[must_use]
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            next_action: None,
        }
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
/// Phase-1 ships hash-based drift detection: any change to the
/// non-tombstoned memory set's content/level/kind/tags projection
/// flips `drift_detected` to `true`. The per-signal counts
/// (`memories_added_since` et al.) report `None` ("unknown") in
/// phase 1 because they require the capsule to embed the memory ID
/// set at capture time. That richer capture is tracked as a
/// follow-up bead.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct StaleSnapshot {
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
    /// capsule's capture timestamp. `None` in phase 1 because the
    /// capsule does not embed the captured memory ID set; emitting
    /// `Some(0)` would be a lie when in fact we cannot tell.
    pub memories_added_since: Option<u64>,
    /// Count of capsule-included memories whose `valid_to` has
    /// elapsed since capture. `None` in phase 1 for the same reason.
    pub memories_expired_since: Option<u64>,
    /// Count of capsule-included memories whose content hash has
    /// changed since capture. `None` until N15.1 (immutable
    /// revisions) lands; the underlying content_hash mutation
    /// pathway is not yet observable.
    pub memories_revised_since: Option<u64>,
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
/// (M3 / bd-17c65.13.4). Strips the volatile top-level fields that
/// change on every create regardless of workspace state:
///   - `capsule_id` — freshly generated ULID per call
///   - `created_at` — wall-clock timestamp
///     and recursively scrubs the volatile per-section fields:
///   - `audit_id` (assigned at write time)
///   - `last_accessed_at` (changes on every memory touch)
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

fn strip_volatile_capsule_fields(value: &mut serde_json::Value) {
    if let Some(object) = value.as_object_mut() {
        object.remove("capsule_id");
        object.remove("created_at");
        object.remove("audit_id");
        object.remove("last_accessed_at");
        for (_, child) in object.iter_mut() {
            strip_volatile_capsule_fields(child);
        }
    } else if let Some(array) = value.as_array_mut() {
        for child in array.iter_mut() {
            strip_volatile_capsule_fields(child);
        }
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

    // Bead bd-1xkxi (M3 follow-up): embed a deterministic
    // workspace_state_hash so the canonical_content_hash actually
    // reflects the underlying memory set. Without this, adding a new
    // memory wouldn't change the capsule hash because the v1 capsule
    // body uses templated section placeholders. The hash is computed
    // from the DB file at <workspace>/.ee/ee.db when present;
    // otherwise None (preview-on-empty-workspace path).
    let db_path = options.workspace.join(".ee").join("ee.db");
    let workspace_state_hash = compute_workspace_state_hash(&db_path);

    let capsule_content = serde_json::json!({
        "schema": HANDOFF_CAPSULE_SCHEMA_V1,
        "capsule_id": capsule_id,
        "workspace": options.workspace,
        "workspace_identity": workspace_identity,
        "workspace_state_hash": workspace_state_hash,
        "profile": options.profile.as_str(),
        "sections": sections,
        "active_focus": active_focus,
        "task_frame": task_frame_json,
        "swarm_brief_summary": swarm_brief_summary,
        "created_at": Utc::now().to_rfc3339(),
    });

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
    if let Some(s) = &stale {
        if s.drift_detected {
            report.degradations.push(
                DegradationInfo::new(
                    "handoff_snapshot_stale",
                    "Workspace memory set has drifted since this capsule was captured; the resumed pack may not reflect current ground truth.".to_owned(),
                )
                .with_next_action("ee handoff create --workspace . --refresh"),
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

    let db_path = workspace_path.join(".ee").join("ee.db");
    let current = compute_workspace_state_hash(&db_path);

    if captured.is_none() && current.is_none() {
        return None;
    }

    let drift_detected = match (captured.as_deref(), current.as_deref()) {
        (Some(a), Some(b)) => a != b,
        _ => false,
    };

    Some(StaleSnapshot {
        captured_state_hash: captured,
        current_state_hash: current,
        drift_detected,
        memories_added_since: None,
        memories_expired_since: None,
        memories_revised_since: None,
    })
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
    fn validation_status_has_stable_string_representation() -> TestResult {
        ensure_equal(&ValidationStatus::Valid.as_str(), &"valid", "valid")?;
        ensure_equal(&ValidationStatus::Invalid.as_str(), &"invalid", "invalid")?;
        ensure_equal(&ValidationStatus::Stale.as_str(), &"stale", "stale")?;
        ensure_equal(&ValidationStatus::Partial.as_str(), &"partial", "partial")
    }
}
