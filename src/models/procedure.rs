//! Procedure and skill-capsule schema contracts (EE-410).
//!
//! Procedures are reusable, evidence-backed workflows distilled from recorder
//! runs and curation events. Skill capsules are render-only exports of those
//! procedures; defining the schemas here does not install or execute them.

use std::fmt;
use std::str::FromStr;

// ============================================================================
// Schema Constants
// ============================================================================

/// Schema for a reusable procedure record.
pub const PROCEDURE_SCHEMA_V1: &str = "ee.procedure.v1";

/// Schema for one ordered procedure step.
pub const PROCEDURE_STEP_SCHEMA_V1: &str = "ee.procedure.step.v1";

/// Schema for procedure verification results.
pub const PROCEDURE_VERIFICATION_SCHEMA_V1: &str = "ee.procedure.verification.v1";

/// Schema for procedure export manifests.
pub const PROCEDURE_EXPORT_SCHEMA_V1: &str = "ee.procedure.export.v1";

/// Schema for render-only skill capsules generated from procedures.
pub const SKILL_CAPSULE_SCHEMA_V1: &str = "ee.skill_capsule.v1";

/// Schema for the procedure schema catalog.
pub const PROCEDURE_SCHEMA_CATALOG_V1: &str = "ee.procedure.schemas.v1";

const JSON_SCHEMA_DRAFT_2020_12: &str = "https://json-schema.org/draft/2020-12/schema";

// ============================================================================
// Stable Wire Enums
// ============================================================================

/// Lifecycle state for a distilled procedure.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ProcedureStatus {
    Candidate,
    Verified,
    Retired,
}

impl ProcedureStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Candidate => "candidate",
            Self::Verified => "verified",
            Self::Retired => "retired",
        }
    }

    #[must_use]
    pub const fn all() -> [Self; 3] {
        [Self::Candidate, Self::Verified, Self::Retired]
    }
}

impl fmt::Display for ProcedureStatus {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for ProcedureStatus {
    type Err = ParseProcedureValueError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        match input {
            "candidate" => Ok(Self::Candidate),
            "verified" => Ok(Self::Verified),
            "retired" => Ok(Self::Retired),
            _ => Err(ParseProcedureValueError::new(
                "procedure_status",
                input,
                "candidate, verified, retired",
            )),
        }
    }
}

/// Verification outcome for a procedure or procedure step.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ProcedureVerificationStatus {
    Pending,
    Passed,
    Failed,
    Stale,
}

impl ProcedureVerificationStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Passed => "passed",
            Self::Failed => "failed",
            Self::Stale => "stale",
        }
    }

    #[must_use]
    pub const fn all() -> [Self; 4] {
        [Self::Pending, Self::Passed, Self::Failed, Self::Stale]
    }
}

impl fmt::Display for ProcedureVerificationStatus {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for ProcedureVerificationStatus {
    type Err = ParseProcedureValueError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        match input {
            "pending" => Ok(Self::Pending),
            "passed" => Ok(Self::Passed),
            "failed" => Ok(Self::Failed),
            "stale" => Ok(Self::Stale),
            _ => Err(ParseProcedureValueError::new(
                "procedure_verification_status",
                input,
                "pending, passed, failed, stale",
            )),
        }
    }
}

/// Stable procedure export format.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ProcedureExportFormat {
    Json,
    Markdown,
    Playbook,
    SkillCapsule,
}

impl ProcedureExportFormat {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Json => "json",
            Self::Markdown => "markdown",
            Self::Playbook => "playbook",
            Self::SkillCapsule => "skill_capsule",
        }
    }

    #[must_use]
    pub const fn all() -> [Self; 4] {
        [
            Self::Json,
            Self::Markdown,
            Self::Playbook,
            Self::SkillCapsule,
        ]
    }
}

impl fmt::Display for ProcedureExportFormat {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for ProcedureExportFormat {
    type Err = ParseProcedureValueError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        let normalized = input.trim().to_ascii_lowercase().replace('-', "_");
        match normalized.as_str() {
            "json" => Ok(Self::Json),
            "md" | "markdown" => Ok(Self::Markdown),
            "yaml" | "yml" | "playbook" => Ok(Self::Playbook),
            "skill" | "skill_capsule" | "skill_markdown" => Ok(Self::SkillCapsule),
            _ => Err(ParseProcedureValueError::new(
                "procedure_export_format",
                input,
                "json, markdown, playbook, skill_capsule",
            )),
        }
    }
}

/// Whether a skill capsule may be installed automatically.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum SkillCapsuleInstallMode {
    RenderOnly,
    ManualReview,
}

impl SkillCapsuleInstallMode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::RenderOnly => "render_only",
            Self::ManualReview => "manual_review",
        }
    }

    #[must_use]
    pub const fn all() -> [Self; 2] {
        [Self::RenderOnly, Self::ManualReview]
    }
}

impl fmt::Display for SkillCapsuleInstallMode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for SkillCapsuleInstallMode {
    type Err = ParseProcedureValueError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        match input {
            "render_only" => Ok(Self::RenderOnly),
            "manual_review" => Ok(Self::ManualReview),
            _ => Err(ParseProcedureValueError::new(
                "skill_capsule_install_mode",
                input,
                "render_only, manual_review",
            )),
        }
    }
}

/// Error returned when a stable procedure wire value cannot be parsed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseProcedureValueError {
    field: &'static str,
    value: String,
    expected: &'static str,
}

impl ParseProcedureValueError {
    #[must_use]
    pub fn new(field: &'static str, value: impl Into<String>, expected: &'static str) -> Self {
        Self {
            field,
            value: value.into(),
            expected,
        }
    }

    #[must_use]
    pub const fn field(&self) -> &'static str {
        self.field
    }

    #[must_use]
    pub fn value(&self) -> &str {
        &self.value
    }

    #[must_use]
    pub const fn expected(&self) -> &'static str {
        self.expected
    }
}

impl fmt::Display for ParseProcedureValueError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "invalid {} value '{}'; expected one of: {}",
            self.field, self.value, self.expected
        )
    }
}

impl std::error::Error for ParseProcedureValueError {}

// ============================================================================
// Domain Records
// ============================================================================

/// Reusable workflow distilled from successful traces and supporting evidence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Procedure {
    pub schema: &'static str,
    pub procedure_id: String,
    pub title: String,
    pub summary: String,
    pub status: ProcedureStatus,
    pub source_run_ids: Vec<String>,
    pub evidence_ids: Vec<String>,
    pub step_count: u32,
    pub created_at: String,
    pub updated_at: String,
    pub verified_at: Option<String>,
}

impl Procedure {
    #[must_use]
    pub fn new(
        procedure_id: impl Into<String>,
        title: impl Into<String>,
        summary: impl Into<String>,
        created_at: impl Into<String>,
    ) -> Self {
        let created_at = created_at.into();
        Self {
            schema: PROCEDURE_SCHEMA_V1,
            procedure_id: procedure_id.into(),
            title: title.into(),
            summary: summary.into(),
            status: ProcedureStatus::Candidate,
            source_run_ids: Vec::new(),
            evidence_ids: Vec::new(),
            step_count: 0,
            updated_at: created_at.clone(),
            created_at,
            verified_at: None,
        }
    }

    #[must_use]
    pub fn with_status(mut self, status: ProcedureStatus) -> Self {
        self.status = status;
        self
    }

    #[must_use]
    pub fn with_source_run(mut self, source_run_id: impl Into<String>) -> Self {
        self.source_run_ids.push(source_run_id.into());
        self
    }

    #[must_use]
    pub fn with_evidence(mut self, evidence_id: impl Into<String>) -> Self {
        self.evidence_ids.push(evidence_id.into());
        self
    }

    #[must_use]
    pub const fn with_step_count(mut self, step_count: u32) -> Self {
        self.step_count = step_count;
        self
    }

    #[must_use]
    pub fn verified_at(mut self, verified_at: impl Into<String>) -> Self {
        self.verified_at = Some(verified_at.into());
        self
    }
}

/// One deterministic, ordered step in a procedure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProcedureStep {
    pub schema: &'static str,
    pub procedure_id: String,
    pub step_id: String,
    pub sequence: u32,
    pub title: String,
    pub instruction: String,
    pub command_hint: Option<String>,
    pub expected_evidence: Vec<String>,
    pub failure_modes: Vec<String>,
    pub required: bool,
}

impl ProcedureStep {
    #[must_use]
    pub fn new(
        procedure_id: impl Into<String>,
        step_id: impl Into<String>,
        sequence: u32,
        title: impl Into<String>,
        instruction: impl Into<String>,
    ) -> Self {
        Self {
            schema: PROCEDURE_STEP_SCHEMA_V1,
            procedure_id: procedure_id.into(),
            step_id: step_id.into(),
            sequence,
            title: title.into(),
            instruction: instruction.into(),
            command_hint: None,
            expected_evidence: Vec::new(),
            failure_modes: Vec::new(),
            required: true,
        }
    }

    #[must_use]
    pub fn command_hint(mut self, command_hint: impl Into<String>) -> Self {
        self.command_hint = Some(command_hint.into());
        self
    }

    #[must_use]
    pub fn with_expected_evidence(mut self, evidence: impl Into<String>) -> Self {
        self.expected_evidence.push(evidence.into());
        self
    }

    #[must_use]
    pub fn with_failure_mode(mut self, failure_mode: impl Into<String>) -> Self {
        self.failure_modes.push(failure_mode.into());
        self
    }

    #[must_use]
    pub const fn optional(mut self) -> Self {
        self.required = false;
        self
    }
}

/// Verification report for a procedure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProcedureVerification {
    pub schema: &'static str,
    pub verification_id: String,
    pub procedure_id: String,
    pub status: ProcedureVerificationStatus,
    pub verified_at: String,
    pub verifier: String,
    pub evidence_ids: Vec<String>,
    pub replay_artifact_ids: Vec<String>,
    pub failure_reason: Option<String>,
}

impl ProcedureVerification {
    #[must_use]
    pub fn new(
        verification_id: impl Into<String>,
        procedure_id: impl Into<String>,
        status: ProcedureVerificationStatus,
        verified_at: impl Into<String>,
        verifier: impl Into<String>,
    ) -> Self {
        Self {
            schema: PROCEDURE_VERIFICATION_SCHEMA_V1,
            verification_id: verification_id.into(),
            procedure_id: procedure_id.into(),
            status,
            verified_at: verified_at.into(),
            verifier: verifier.into(),
            evidence_ids: Vec::new(),
            replay_artifact_ids: Vec::new(),
            failure_reason: None,
        }
    }

    #[must_use]
    pub fn with_evidence(mut self, evidence_id: impl Into<String>) -> Self {
        self.evidence_ids.push(evidence_id.into());
        self
    }

    #[must_use]
    pub fn with_replay_artifact(mut self, artifact_id: impl Into<String>) -> Self {
        self.replay_artifact_ids.push(artifact_id.into());
        self
    }

    #[must_use]
    pub fn failure_reason(mut self, failure_reason: impl Into<String>) -> Self {
        self.failure_reason = Some(failure_reason.into());
        self
    }
}

/// Manifest for a rendered procedure export.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProcedureExport {
    pub schema: &'static str,
    pub export_id: String,
    pub procedure_id: String,
    pub format: ProcedureExportFormat,
    pub generated_at: String,
    pub includes_evidence: bool,
    pub redaction_status: String,
    pub artifact_hash: Option<String>,
}

impl ProcedureExport {
    #[must_use]
    pub fn new(
        export_id: impl Into<String>,
        procedure_id: impl Into<String>,
        format: ProcedureExportFormat,
        generated_at: impl Into<String>,
    ) -> Self {
        Self {
            schema: PROCEDURE_EXPORT_SCHEMA_V1,
            export_id: export_id.into(),
            procedure_id: procedure_id.into(),
            format,
            generated_at: generated_at.into(),
            includes_evidence: false,
            redaction_status: "not_required".to_string(),
            artifact_hash: None,
        }
    }

    #[must_use]
    pub const fn include_evidence(mut self) -> Self {
        self.includes_evidence = true;
        self
    }

    #[must_use]
    pub fn redaction_status(mut self, redaction_status: impl Into<String>) -> Self {
        self.redaction_status = redaction_status.into();
        self
    }

    #[must_use]
    pub fn artifact_hash(mut self, artifact_hash: impl Into<String>) -> Self {
        self.artifact_hash = Some(artifact_hash.into());
        self
    }
}

/// Render-only skill capsule generated from a procedure export.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SkillCapsule {
    pub schema: &'static str,
    pub capsule_id: String,
    pub procedure_id: String,
    pub title: String,
    pub summary: String,
    pub generated_at: String,
    pub install_mode: SkillCapsuleInstallMode,
    pub markdown_body_hash: String,
    pub source_export_id: String,
    pub warnings: Vec<String>,
}

impl SkillCapsule {
    #[must_use]
    pub fn new(
        capsule_id: impl Into<String>,
        procedure_id: impl Into<String>,
        title: impl Into<String>,
        summary: impl Into<String>,
        generated_at: impl Into<String>,
        markdown_body_hash: impl Into<String>,
        source_export_id: impl Into<String>,
    ) -> Self {
        Self {
            schema: SKILL_CAPSULE_SCHEMA_V1,
            capsule_id: capsule_id.into(),
            procedure_id: procedure_id.into(),
            title: title.into(),
            summary: summary.into(),
            generated_at: generated_at.into(),
            install_mode: SkillCapsuleInstallMode::RenderOnly,
            markdown_body_hash: markdown_body_hash.into(),
            source_export_id: source_export_id.into(),
            warnings: Vec::new(),
        }
    }

    #[must_use]
    pub fn with_install_mode(mut self, install_mode: SkillCapsuleInstallMode) -> Self {
        self.install_mode = install_mode;
        self
    }

    #[must_use]
    pub fn with_warning(mut self, warning: impl Into<String>) -> Self {
        self.warnings.push(warning.into());
        self
    }
}

// ============================================================================
// Schema Catalog
// ============================================================================

/// Field descriptor used by the procedure schema catalog.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProcedureFieldSchema {
    pub name: &'static str,
    pub type_name: &'static str,
    pub required: bool,
    pub description: &'static str,
}

impl ProcedureFieldSchema {
    #[must_use]
    pub const fn new(
        name: &'static str,
        type_name: &'static str,
        required: bool,
        description: &'static str,
    ) -> Self {
        Self {
            name,
            type_name,
            required,
            description,
        }
    }
}

/// Stable JSON-schema-like catalog entry for procedure records.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProcedureObjectSchema {
    pub schema_name: &'static str,
    pub schema_uri: &'static str,
    pub kind: &'static str,
    pub title: &'static str,
    pub description: &'static str,
    pub fields: &'static [ProcedureFieldSchema],
}

impl ProcedureObjectSchema {
    #[must_use]
    pub fn required_count(&self) -> usize {
        self.fields.iter().filter(|field| field.required).count()
    }
}

const PROCEDURE_FIELDS: &[ProcedureFieldSchema] = &[
    ProcedureFieldSchema::new("schema", "string", true, "Schema identifier."),
    ProcedureFieldSchema::new(
        "procedureId",
        "string",
        true,
        "Stable procedure identifier.",
    ),
    ProcedureFieldSchema::new("title", "string", true, "Human-readable procedure title."),
    ProcedureFieldSchema::new("summary", "string", true, "Short procedure summary."),
    ProcedureFieldSchema::new("status", "string", true, "Procedure lifecycle status."),
    ProcedureFieldSchema::new(
        "sourceRunIds",
        "array<string>",
        true,
        "Recorder run identifiers that support the procedure.",
    ),
    ProcedureFieldSchema::new(
        "evidenceIds",
        "array<string>",
        true,
        "Evidence identifiers cited by the procedure.",
    ),
    ProcedureFieldSchema::new("stepCount", "integer", true, "Number of ordered steps."),
    ProcedureFieldSchema::new("createdAt", "string", true, "RFC 3339 creation timestamp."),
    ProcedureFieldSchema::new("updatedAt", "string", true, "RFC 3339 update timestamp."),
    ProcedureFieldSchema::new(
        "verifiedAt",
        "string|null",
        false,
        "RFC 3339 verification timestamp when the procedure is verified.",
    ),
];

const PROCEDURE_STEP_FIELDS: &[ProcedureFieldSchema] = &[
    ProcedureFieldSchema::new("schema", "string", true, "Schema identifier."),
    ProcedureFieldSchema::new(
        "procedureId",
        "string",
        true,
        "Parent procedure identifier.",
    ),
    ProcedureFieldSchema::new(
        "stepId",
        "string",
        true,
        "Stable procedure step identifier.",
    ),
    ProcedureFieldSchema::new(
        "sequence",
        "integer",
        true,
        "One-based deterministic step order.",
    ),
    ProcedureFieldSchema::new("title", "string", true, "Short step title."),
    ProcedureFieldSchema::new(
        "instruction",
        "string",
        true,
        "Action the agent should perform.",
    ),
    ProcedureFieldSchema::new(
        "commandHint",
        "string|null",
        false,
        "Optional command or tool hint, never an automatic execution request.",
    ),
    ProcedureFieldSchema::new(
        "expectedEvidence",
        "array<string>",
        true,
        "Evidence expected after completing the step.",
    ),
    ProcedureFieldSchema::new(
        "failureModes",
        "array<string>",
        true,
        "Known step failure modes or tripwires.",
    ),
    ProcedureFieldSchema::new("required", "boolean", true, "Whether the step is required."),
];

const PROCEDURE_VERIFICATION_FIELDS: &[ProcedureFieldSchema] = &[
    ProcedureFieldSchema::new("schema", "string", true, "Schema identifier."),
    ProcedureFieldSchema::new(
        "verificationId",
        "string",
        true,
        "Stable verification record identifier.",
    ),
    ProcedureFieldSchema::new(
        "procedureId",
        "string",
        true,
        "Verified procedure identifier.",
    ),
    ProcedureFieldSchema::new("status", "string", true, "Verification outcome status."),
    ProcedureFieldSchema::new(
        "verifiedAt",
        "string",
        true,
        "RFC 3339 verification timestamp.",
    ),
    ProcedureFieldSchema::new(
        "verifier",
        "string",
        true,
        "Tool, agent, or harness verifier.",
    ),
    ProcedureFieldSchema::new(
        "evidenceIds",
        "array<string>",
        true,
        "Evidence identifiers checked during verification.",
    ),
    ProcedureFieldSchema::new(
        "replayArtifactIds",
        "array<string>",
        true,
        "Replay or evaluation artifact identifiers used for verification.",
    ),
    ProcedureFieldSchema::new(
        "failureReason",
        "string|null",
        false,
        "Reason verification failed or became stale.",
    ),
];

const PROCEDURE_EXPORT_FIELDS: &[ProcedureFieldSchema] = &[
    ProcedureFieldSchema::new("schema", "string", true, "Schema identifier."),
    ProcedureFieldSchema::new("exportId", "string", true, "Stable export identifier."),
    ProcedureFieldSchema::new(
        "procedureId",
        "string",
        true,
        "Exported procedure identifier.",
    ),
    ProcedureFieldSchema::new("format", "string", true, "Stable export format."),
    ProcedureFieldSchema::new(
        "generatedAt",
        "string",
        true,
        "RFC 3339 generation timestamp.",
    ),
    ProcedureFieldSchema::new(
        "includesEvidence",
        "boolean",
        true,
        "Whether the export includes supporting evidence excerpts.",
    ),
    ProcedureFieldSchema::new(
        "redactionStatus",
        "string",
        true,
        "Redaction state applied before export.",
    ),
    ProcedureFieldSchema::new(
        "artifactHash",
        "string|null",
        false,
        "Hash of the rendered export artifact when available.",
    ),
];

const SKILL_CAPSULE_FIELDS: &[ProcedureFieldSchema] = &[
    ProcedureFieldSchema::new("schema", "string", true, "Schema identifier."),
    ProcedureFieldSchema::new(
        "capsuleId",
        "string",
        true,
        "Stable skill capsule identifier.",
    ),
    ProcedureFieldSchema::new(
        "procedureId",
        "string",
        true,
        "Source procedure identifier.",
    ),
    ProcedureFieldSchema::new("title", "string", true, "Capsule title."),
    ProcedureFieldSchema::new("summary", "string", true, "Capsule summary."),
    ProcedureFieldSchema::new(
        "generatedAt",
        "string",
        true,
        "RFC 3339 generation timestamp.",
    ),
    ProcedureFieldSchema::new(
        "installMode",
        "string",
        true,
        "Installation posture; capsules are render-only unless reviewed manually.",
    ),
    ProcedureFieldSchema::new(
        "markdownBodyHash",
        "string",
        true,
        "Hash of the rendered Markdown body.",
    ),
    ProcedureFieldSchema::new(
        "sourceExportId",
        "string",
        true,
        "Source procedure export ID.",
    ),
    ProcedureFieldSchema::new(
        "warnings",
        "array<string>",
        true,
        "Sorted warnings shown with the capsule.",
    ),
];

#[must_use]
pub const fn procedure_schemas() -> [ProcedureObjectSchema; 5] {
    [
        ProcedureObjectSchema {
            schema_name: PROCEDURE_SCHEMA_V1,
            schema_uri: "urn:ee:schema:procedure:v1",
            kind: "procedure",
            title: "Procedure",
            description: "Evidence-backed reusable workflow distilled from successful traces.",
            fields: PROCEDURE_FIELDS,
        },
        ProcedureObjectSchema {
            schema_name: PROCEDURE_STEP_SCHEMA_V1,
            schema_uri: "urn:ee:schema:procedure-step:v1",
            kind: "procedure_step",
            title: "ProcedureStep",
            description: "One deterministic ordered step within a procedure.",
            fields: PROCEDURE_STEP_FIELDS,
        },
        ProcedureObjectSchema {
            schema_name: PROCEDURE_VERIFICATION_SCHEMA_V1,
            schema_uri: "urn:ee:schema:procedure-verification:v1",
            kind: "procedure_verification",
            title: "ProcedureVerification",
            description: "Verification result for a procedure and its supporting evidence.",
            fields: PROCEDURE_VERIFICATION_FIELDS,
        },
        ProcedureObjectSchema {
            schema_name: PROCEDURE_EXPORT_SCHEMA_V1,
            schema_uri: "urn:ee:schema:procedure-export:v1",
            kind: "procedure_export",
            title: "ProcedureExport",
            description: "Manifest for a rendered procedure export artifact.",
            fields: PROCEDURE_EXPORT_FIELDS,
        },
        ProcedureObjectSchema {
            schema_name: SKILL_CAPSULE_SCHEMA_V1,
            schema_uri: "urn:ee:schema:skill-capsule:v1",
            kind: "skill_capsule",
            title: "SkillCapsule",
            description: "Render-only reusable skill capsule derived from a verified procedure.",
            fields: SKILL_CAPSULE_FIELDS,
        },
    ]
}

#[must_use]
pub fn procedure_schema_catalog_json() -> String {
    let schemas = procedure_schemas();
    let mut output = String::from("{\n");
    output.push_str(&format!(
        "  \"schema\": \"{PROCEDURE_SCHEMA_CATALOG_V1}\",\n"
    ));
    output.push_str("  \"schemas\": [\n");
    for (schema_index, schema) in schemas.iter().enumerate() {
        output.push_str("    {\n");
        output.push_str(&format!(
            "      \"$schema\": \"{JSON_SCHEMA_DRAFT_2020_12}\",\n"
        ));
        output.push_str("      \"$id\": ");
        push_json_string(&mut output, schema.schema_uri);
        output.push_str(",\n");
        output.push_str("      \"eeSchema\": ");
        push_json_string(&mut output, schema.schema_name);
        output.push_str(",\n");
        output.push_str("      \"kind\": ");
        push_json_string(&mut output, schema.kind);
        output.push_str(",\n");
        output.push_str("      \"title\": ");
        push_json_string(&mut output, schema.title);
        output.push_str(",\n");
        output.push_str("      \"description\": ");
        push_json_string(&mut output, schema.description);
        output.push_str(",\n");
        output.push_str("      \"type\": \"object\",\n");
        output.push_str("      \"required\": [\n");
        let mut emitted_required = 0;
        for field in schema.fields {
            if field.required {
                emitted_required += 1;
                output.push_str("        ");
                push_json_string(&mut output, field.name);
                if emitted_required == schema.required_count() {
                    output.push('\n');
                } else {
                    output.push_str(",\n");
                }
            }
        }
        output.push_str("      ],\n");
        output.push_str("      \"fields\": [\n");
        for (field_index, field) in schema.fields.iter().enumerate() {
            output.push_str("        {\"name\": ");
            push_json_string(&mut output, field.name);
            output.push_str(", \"type\": ");
            push_json_string(&mut output, field.type_name);
            output.push_str(", \"required\": ");
            output.push_str(if field.required { "true" } else { "false" });
            output.push_str(", \"description\": ");
            push_json_string(&mut output, field.description);
            if field_index + 1 == schema.fields.len() {
                output.push_str("}\n");
            } else {
                output.push_str("},\n");
            }
        }
        output.push_str("      ],\n");
        output.push_str("      \"additionalProperties\": false\n");
        if schema_index + 1 == schemas.len() {
            output.push_str("    }\n");
        } else {
            output.push_str("    },\n");
        }
    }
    output.push_str("  ]\n");
    output.push_str("}\n");
    output
}

fn push_json_string(output: &mut String, value: &str) {
    output.push('"');
    for character in value.chars() {
        match character {
            '"' => output.push_str("\\\""),
            '\\' => output.push_str("\\\\"),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            other => output.push(other),
        }
    }
    output.push('"');
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::*;

    const PROCEDURE_SCHEMA_GOLDEN: &str =
        include_str!("../../tests/fixtures/golden/models/procedure_schemas.json.golden");

    type TestResult = Result<(), String>;

    fn ensure<T: std::fmt::Debug + PartialEq>(actual: T, expected: T, ctx: &str) -> TestResult {
        if actual == expected {
            Ok(())
        } else {
            Err(format!("{ctx}: expected {expected:?}, got {actual:?}"))
        }
    }

    #[test]
    fn schema_constants_are_stable() -> TestResult {
        ensure(PROCEDURE_SCHEMA_V1, "ee.procedure.v1", "procedure")?;
        ensure(PROCEDURE_STEP_SCHEMA_V1, "ee.procedure.step.v1", "step")?;
        ensure(
            PROCEDURE_VERIFICATION_SCHEMA_V1,
            "ee.procedure.verification.v1",
            "verification",
        )?;
        ensure(
            PROCEDURE_EXPORT_SCHEMA_V1,
            "ee.procedure.export.v1",
            "export",
        )?;
        ensure(SKILL_CAPSULE_SCHEMA_V1, "ee.skill_capsule.v1", "capsule")?;
        ensure(
            PROCEDURE_SCHEMA_CATALOG_V1,
            "ee.procedure.schemas.v1",
            "catalog",
        )
    }

    #[test]
    fn stable_wire_enums_round_trip() -> TestResult {
        for status in ProcedureStatus::all() {
            ensure(
                ProcedureStatus::from_str(status.as_str()),
                Ok(status),
                "procedure status",
            )?;
        }
        for status in ProcedureVerificationStatus::all() {
            ensure(
                ProcedureVerificationStatus::from_str(status.as_str()),
                Ok(status),
                "verification status",
            )?;
        }
        for format in ProcedureExportFormat::all() {
            ensure(
                ProcedureExportFormat::from_str(format.as_str()),
                Ok(format),
                "export format",
            )?;
        }
        for mode in SkillCapsuleInstallMode::all() {
            ensure(
                SkillCapsuleInstallMode::from_str(mode.as_str()),
                Ok(mode),
                "install mode",
            )?;
        }
        ensure(
            ProcedureStatus::from_str("draft").map_err(|error| error.field()),
            Err("procedure_status"),
            "invalid status field",
        )
    }

    #[test]
    fn procedure_builders_set_schemas_and_defaults() -> TestResult {
        let procedure = Procedure::new(
            "proc-001",
            "Release verification",
            "Run release checks before tagging.",
            "2026-04-30T12:00:00Z",
        )
        .with_source_run("run-001")
        .with_evidence("ev-001")
        .with_step_count(2)
        .with_status(ProcedureStatus::Verified)
        .verified_at("2026-04-30T12:05:00Z");

        ensure(procedure.schema, PROCEDURE_SCHEMA_V1, "procedure schema")?;
        ensure(procedure.status, ProcedureStatus::Verified, "status")?;
        ensure(procedure.step_count, 2, "step count")?;
        ensure(
            procedure.verified_at.as_deref(),
            Some("2026-04-30T12:05:00Z"),
            "verified_at",
        )?;

        let step = ProcedureStep::new(
            "proc-001",
            "step-001",
            1,
            "Format check",
            "Run cargo fmt --check through RCH.",
        )
        .command_hint("rch exec -- cargo fmt --check")
        .with_expected_evidence("fmt stdout artifact")
        .with_failure_mode("rustfmt drift");

        ensure(step.schema, PROCEDURE_STEP_SCHEMA_V1, "step schema")?;
        ensure(step.required, true, "step required")?;
        ensure(
            step.command_hint.as_deref(),
            Some("rch exec -- cargo fmt --check"),
            "command hint",
        )?;

        let optional_step = step.clone().optional();
        ensure(optional_step.required, false, "optional step")
    }

    #[test]
    fn verification_export_and_capsule_builders_set_safe_defaults() -> TestResult {
        let verification = ProcedureVerification::new(
            "ver-001",
            "proc-001",
            ProcedureVerificationStatus::Passed,
            "2026-04-30T12:10:00Z",
            "fixture-harness",
        )
        .with_evidence("ev-001")
        .with_replay_artifact("artifact-001");

        ensure(
            verification.schema,
            PROCEDURE_VERIFICATION_SCHEMA_V1,
            "verification schema",
        )?;
        ensure(
            verification.status,
            ProcedureVerificationStatus::Passed,
            "verification status",
        )?;
        ensure(
            verification.failure_reason.is_none(),
            true,
            "passed verification has no failure reason",
        )?;

        let export = ProcedureExport::new(
            "export-001",
            "proc-001",
            ProcedureExportFormat::SkillCapsule,
            "2026-04-30T12:15:00Z",
        )
        .include_evidence()
        .redaction_status("redacted")
        .artifact_hash("blake3:abc123");

        ensure(export.schema, PROCEDURE_EXPORT_SCHEMA_V1, "export schema")?;
        ensure(export.includes_evidence, true, "includes evidence")?;
        ensure(export.redaction_status, "redacted".to_string(), "redaction")?;

        let capsule = SkillCapsule::new(
            "capsule-001",
            "proc-001",
            "Release verification",
            "Render-only release verification procedure.",
            "2026-04-30T12:16:00Z",
            "blake3:def456",
            "export-001",
        )
        .with_warning("manual review required");

        ensure(capsule.schema, SKILL_CAPSULE_SCHEMA_V1, "capsule schema")?;
        ensure(
            capsule.install_mode,
            SkillCapsuleInstallMode::RenderOnly,
            "render-only default",
        )?;
        ensure(capsule.warnings.len(), 1, "warning count")
    }

    #[test]
    fn procedure_schema_catalog_order_is_stable() -> TestResult {
        let schemas = procedure_schemas();
        ensure(schemas.len(), 5, "schema count")?;
        ensure(schemas[0].schema_name, PROCEDURE_SCHEMA_V1, "procedure")?;
        ensure(schemas[1].schema_name, PROCEDURE_STEP_SCHEMA_V1, "step")?;
        ensure(
            schemas[2].schema_name,
            PROCEDURE_VERIFICATION_SCHEMA_V1,
            "verification",
        )?;
        ensure(schemas[3].schema_name, PROCEDURE_EXPORT_SCHEMA_V1, "export")?;
        ensure(schemas[4].schema_name, SKILL_CAPSULE_SCHEMA_V1, "capsule")
    }

    #[test]
    fn procedure_schema_catalog_matches_golden_fixture() {
        assert_eq!(procedure_schema_catalog_json(), PROCEDURE_SCHEMA_GOLDEN);
    }

    #[test]
    fn procedure_schema_catalog_is_valid_json() -> TestResult {
        let parsed: serde_json::Value = serde_json::from_str(PROCEDURE_SCHEMA_GOLDEN)
            .map_err(|error| format!("procedure schema golden must be valid JSON: {error}"))?;
        ensure(
            parsed.get("schema").and_then(serde_json::Value::as_str),
            Some(PROCEDURE_SCHEMA_CATALOG_V1),
            "catalog schema",
        )?;
        let schemas = parsed
            .get("schemas")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| "schemas must be an array".to_string())?;
        ensure(schemas.len(), 5, "catalog length")
    }
}
