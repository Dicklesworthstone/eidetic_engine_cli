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
}

impl Default for PreviewOptions {
    fn default() -> Self {
        Self {
            workspace: PathBuf::from("."),
            profile: CapsuleProfile::Resume,
            since: None,
            include_estimates: true,
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
        serde_json::to_string(self).unwrap_or_default()
    }

    #[must_use]
    pub fn to_json_pretty(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_default()
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
}

impl Default for CreateOptions {
    fn default() -> Self {
        Self {
            workspace: PathBuf::from("."),
            output: PathBuf::from("handoff.json"),
            profile: CapsuleProfile::Resume,
            since: None,
            dry_run: false,
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
    pub token_count: usize,
    pub byte_count: usize,
    pub content_hash: String,
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
            token_count: 0,
            byte_count: 0,
            content_hash: String::new(),
            redaction_summary: RedactionSummary::default(),
            dry_run: false,
            created_at: Utc::now().to_rfc3339(),
        }
    }

    #[must_use]
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    #[must_use]
    pub fn to_json_pretty(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_default()
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
        serde_json::to_string(self).unwrap_or_default()
    }

    #[must_use]
    pub fn to_json_pretty(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_default()
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
}

impl Default for ResumeOptions {
    fn default() -> Self {
        Self {
            path: PathBuf::new(),
            use_latest: false,
            workspace: PathBuf::from("."),
            max_sections: None,
        }
    }
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
    pub artifact_pointers: Vec<ArtifactPointer>,
    pub degradations: Vec<DegradationInfo>,
    pub resumed_at: String,
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
            artifact_pointers: Vec::new(),
            degradations: Vec::new(),
            resumed_at: Utc::now().to_rfc3339(),
        }
    }

    #[must_use]
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    #[must_use]
    pub fn to_json_pretty(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_default()
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

    let capsule_content = serde_json::json!({
        "schema": HANDOFF_CAPSULE_SCHEMA_V1,
        "capsule_id": capsule_id,
        "workspace": options.workspace,
        "profile": options.profile.as_str(),
        "sections": sections,
        "active_focus": active_focus,
        "created_at": Utc::now().to_rfc3339(),
    });

    let content_str = serde_json::to_string_pretty(&capsule_content).unwrap_or_default();
    report.content_hash = compute_content_hash(&content_str);

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

    if let Some(sections) = capsule.get("sections").and_then(|v| v.as_array()) {
        for section in sections {
            let section_id = section.get("id").and_then(|v| v.as_str()).unwrap_or("");
            let section_content = section
                .get("content")
                .and_then(|v| v.as_str())
                .unwrap_or("");

            match section_id {
                "objective" => {
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

    Ok(report)
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
        })
        .map_err(|error| error.message())?;
        ensure(create.active_focus.is_some(), "create includes focus")?;

        let resume = resume_handoff(&ResumeOptions {
            path: output,
            use_latest: false,
            workspace: dir.path().to_path_buf(),
            max_sections: None,
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
