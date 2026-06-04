//! Redaction-safe normalization models for regression causality capsules.
//!
//! The capsule schema is intentionally compact. This module owns the read-only
//! evidence normalization layer that turns heterogeneous verification, replay,
//! pack, perf, tracker, git, and support-bundle artifacts into deterministic
//! rows without copying raw logs or private checkout paths.

use std::collections::{BTreeSet, HashMap};
use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

pub const REGRESSION_CAUSALITY_SCHEMA_V1: &str = "ee.regression_causality.v1";
pub const REGRESSION_EVIDENCE_NORMALIZATION_SCHEMA_V1: &str =
    "ee.regression_evidence_normalization.v1";

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RegressionEvidenceKind {
    VerificationEvidence,
    RchSelectorAdmission,
    SwarmReplay,
    E2eEventLog,
    PackReplay,
    PackDiff,
    PerfReport,
    BeadsHistory,
    BvHistory,
    DegradedFixture,
    GitMetadata,
    SupportBundle,
}

impl RegressionEvidenceKind {
    pub const ALL: [Self; 12] = [
        Self::VerificationEvidence,
        Self::RchSelectorAdmission,
        Self::SwarmReplay,
        Self::E2eEventLog,
        Self::PackReplay,
        Self::PackDiff,
        Self::PerfReport,
        Self::BeadsHistory,
        Self::BvHistory,
        Self::DegradedFixture,
        Self::GitMetadata,
        Self::SupportBundle,
    ];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::VerificationEvidence => "verification_evidence",
            Self::RchSelectorAdmission => "rch_selector_admission",
            Self::SwarmReplay => "swarm_replay",
            Self::E2eEventLog => "e2e_event_log",
            Self::PackReplay => "pack_replay",
            Self::PackDiff => "pack_diff",
            Self::PerfReport => "perf_report",
            Self::BeadsHistory => "beads_history",
            Self::BvHistory => "bv_history",
            Self::DegradedFixture => "degraded_fixture",
            Self::GitMetadata => "git_metadata",
            Self::SupportBundle => "support_bundle",
        }
    }

    #[must_use]
    pub fn parse(input: &str) -> Option<Self> {
        let token = normalized_token(input);
        Self::ALL.into_iter().find(|kind| token == kind.as_str())
    }
}

impl fmt::Display for RegressionEvidenceKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RegressionEvidenceStatus {
    Available,
    Missing,
    Malformed,
    Stale,
    Blocked,
    Unsupported,
    RedactedOnly,
}

impl RegressionEvidenceStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Available => "available",
            Self::Missing => "missing",
            Self::Malformed => "malformed",
            Self::Stale => "stale",
            Self::Blocked => "blocked",
            Self::Unsupported => "unsupported",
            Self::RedactedOnly => "redacted_only",
        }
    }
}

impl fmt::Display for RegressionEvidenceStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RegressionRedactionStatus {
    Safe,
    Redacted,
    HashOnly,
    Refused,
    Unknown,
}

impl RegressionRedactionStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Safe => "safe",
            Self::Redacted => "redacted",
            Self::HashOnly => "hash_only",
            Self::Refused => "refused",
            Self::Unknown => "unknown",
        }
    }
}

impl fmt::Display for RegressionRedactionStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RegressionSourceMaterialization {
    CommittedTree,
    DirtySourceMaterialized,
    RemoteCheckoutUnverified,
    SourceStateRefused,
    NotApplicable,
    Unknown,
}

impl RegressionSourceMaterialization {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CommittedTree => "committed_tree",
            Self::DirtySourceMaterialized => "dirty_source_materialized",
            Self::RemoteCheckoutUnverified => "remote_checkout_unverified",
            Self::SourceStateRefused => "source_state_refused",
            Self::NotApplicable => "not_applicable",
            Self::Unknown => "unknown",
        }
    }
}

impl fmt::Display for RegressionSourceMaterialization {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RegressionCausalitySeverity {
    Info,
    Low,
    Warning,
    Medium,
    High,
    Critical,
}

impl RegressionCausalitySeverity {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Low => "low",
            Self::Warning => "warning",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Critical => "critical",
        }
    }
}

impl fmt::Display for RegressionCausalitySeverity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct RegressionEvidenceInput {
    pub id: String,
    pub kind: String,
    pub artifact: Option<JsonValue>,
    pub artifact_hash_override: Option<String>,
}

impl RegressionEvidenceInput {
    #[must_use]
    pub fn new(
        id: impl Into<String>,
        kind: RegressionEvidenceKind,
        artifact: impl Into<Option<JsonValue>>,
    ) -> Self {
        Self {
            id: id.into(),
            kind: kind.as_str().to_owned(),
            artifact: artifact.into(),
            artifact_hash_override: None,
        }
    }

    #[must_use]
    pub fn unsupported(
        id: impl Into<String>,
        kind: impl Into<String>,
        artifact: impl Into<Option<JsonValue>>,
    ) -> Self {
        Self {
            id: id.into(),
            kind: normalized_token(&kind.into()),
            artifact: artifact.into(),
            artifact_hash_override: None,
        }
    }

    #[must_use]
    pub fn with_artifact_hash(mut self, artifact_hash: impl Into<String>) -> Self {
        self.artifact_hash_override = normalized_non_empty_string(artifact_hash.into());
        self
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NormalizedRegressionEvidenceRow {
    pub id: String,
    pub kind: String,
    #[serde(rename = "schema")]
    pub schema_id: Option<String>,
    pub status: RegressionEvidenceStatus,
    pub verdict: Option<String>,
    pub artifact_hash: Option<String>,
    pub command_hash: Option<String>,
    pub observed_at: Option<String>,
    pub source_hash: Option<String>,
    pub source_materialization: RegressionSourceMaterialization,
    pub remote_source_materialized: Option<bool>,
    pub redaction_status: RegressionRedactionStatus,
    pub authoritative: bool,
    pub summary: String,
    pub degraded_codes: Vec<String>,
    pub provenance: RegressionEvidenceProvenance,
}

impl NormalizedRegressionEvidenceRow {
    #[must_use]
    pub fn capsule_source(&self) -> RegressionCapsuleEvidenceSource {
        RegressionCapsuleEvidenceSource {
            id: self.id.clone(),
            kind: self.kind.clone(),
            schema_id: self.schema_id.clone(),
            status: self.status,
            artifact_hash: self.artifact_hash.clone(),
            summary: self.summary.clone(),
            redaction_status: self.redaction_status,
            authoritative: self.authoritative,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegressionCapsuleEvidenceSource {
    pub id: String,
    pub kind: String,
    #[serde(rename = "schema")]
    pub schema_id: Option<String>,
    pub status: RegressionEvidenceStatus,
    pub artifact_hash: Option<String>,
    pub summary: String,
    pub redaction_status: RegressionRedactionStatus,
    pub authoritative: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegressionEvidenceProvenance {
    pub source_fields: Vec<String>,
    pub suppressed_fields: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegressionNormalizationDegradation {
    pub code: String,
    pub severity: RegressionCausalitySeverity,
    pub message: String,
    pub evidence_source_id: Option<String>,
    pub repair: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegressionEvidenceNormalizationReport {
    pub schema: String,
    pub rows: Vec<NormalizedRegressionEvidenceRow>,
    pub degraded: Vec<RegressionNormalizationDegradation>,
}

#[must_use]
pub fn normalize_regression_evidence_inputs(
    inputs: &[RegressionEvidenceInput],
) -> RegressionEvidenceNormalizationReport {
    let mut rows = Vec::with_capacity(inputs.len());
    let mut degraded = Vec::new();

    for input in inputs {
        let mut row_degraded = Vec::new();
        let row = normalize_one(input, &mut row_degraded);
        degraded.extend(row_degraded);
        rows.push(row);
    }

    rows.sort_by(|left, right| {
        left.kind
            .cmp(&right.kind)
            .then_with(|| left.id.cmp(&right.id))
    });
    degraded.sort_by(|left, right| {
        left.evidence_source_id
            .cmp(&right.evidence_source_id)
            .then_with(|| left.code.cmp(&right.code))
            .then_with(|| left.message.cmp(&right.message))
    });

    RegressionEvidenceNormalizationReport {
        schema: REGRESSION_EVIDENCE_NORMALIZATION_SCHEMA_V1.to_owned(),
        rows,
        degraded,
    }
}

fn normalize_one(
    input: &RegressionEvidenceInput,
    degraded: &mut Vec<RegressionNormalizationDegradation>,
) -> NormalizedRegressionEvidenceRow {
    let accepted_kind = RegressionEvidenceKind::parse(&input.kind);
    let kind = accepted_kind
        .map(|kind| kind.as_str().to_owned())
        .unwrap_or_else(|| normalized_token(&input.kind));

    if accepted_kind.is_none() {
        degraded.push(degradation(
            "regression_evidence_unsupported_kind",
            RegressionCausalitySeverity::Medium,
            format!("Evidence kind `{kind}` is not accepted by the regression causality contract."),
            Some(&input.id),
            "Use one of the accepted evidence kinds from docs/agent-ux/regression-causality.md.",
        ));
        return empty_row(
            input,
            kind,
            RegressionEvidenceStatus::Unsupported,
            RegressionRedactionStatus::Unknown,
            "Unsupported evidence kind; no artifact fields were trusted.",
        );
    }

    let Some(artifact) = &input.artifact else {
        degraded.push(degradation(
            "regression_evidence_missing",
            RegressionCausalitySeverity::Warning,
            format!(
                "Evidence source `{}` did not provide an artifact.",
                input.id
            ),
            Some(&input.id),
            "Re-run the producing command with JSON output and pass the generated artifact.",
        ));
        return empty_row(
            input,
            kind,
            RegressionEvidenceStatus::Missing,
            RegressionRedactionStatus::Unknown,
            "No artifact was provided for this evidence source.",
        );
    };

    let Some(object) = artifact.as_object() else {
        degraded.push(degradation(
            "regression_evidence_malformed",
            RegressionCausalitySeverity::Medium,
            format!("Evidence source `{}` is not a JSON object.", input.id),
            Some(&input.id),
            "Pass a single JSON object artifact instead of raw text, arrays, or logs.",
        ));
        return empty_row(
            input,
            kind,
            RegressionEvidenceStatus::Malformed,
            RegressionRedactionStatus::Unknown,
            "Artifact was malformed and could not be normalized.",
        );
    };

    let schema_id = first_string(object, &["schema", "$schema", "schemaId", "schema_id"]);
    let verdict = first_string(object, &["status", "verdict", "result", "outcome"]);
    let explicit_status = status_from_artifact(artifact, verdict.as_deref());
    let artifact_hash = input
        .artifact_hash_override
        .clone()
        .or_else(|| first_string(object, &["artifactHash", "contentHash", "hash"]));
    let command_hash = first_path_string(
        artifact,
        &[
            &["commandHash"],
            &["command_hash"],
            &["command", "hash"],
            &["subject", "commandHash"],
        ],
    );
    let observed_at = first_path_string(
        artifact,
        &[
            &["observedAt"],
            &["finishedAt"],
            &["createdAt"],
            &["timestamp"],
            &["ts"],
            &["producer", "observedAt"],
        ],
    );
    let source_hash = first_path_string(
        artifact,
        &[
            &["sourceHash"],
            &["source_state_hash"],
            &["gitTree"],
            &["git_tree"],
            &["sourceState", "sourceHash"],
            &["environment", "workspaceFingerprint"],
        ],
    );
    let source_materialization = source_materialization_from_artifact(artifact);
    let remote_source_materialized = first_path_bool(
        artifact,
        &[
            &["remoteSourceMaterialized"],
            &["remote_source_materialized"],
            &["sourceState", "remoteSourceMaterialized"],
            &["source_state", "remote_source_materialized"],
        ],
    );
    let mut degraded_codes = collect_degraded_codes(artifact);
    let (suppressed_fields, private_path_seen, raw_output_seen) = suppressed_field_report(artifact);
    let redaction_status = redaction_status_from_artifact(artifact, private_path_seen);
    let status = match (explicit_status, redaction_status) {
        (RegressionEvidenceStatus::Available, RegressionRedactionStatus::HashOnly) => {
            RegressionEvidenceStatus::RedactedOnly
        }
        (status, _) => status,
    };

    if private_path_seen {
        degraded_codes.push("regression_evidence_private_path_redacted".to_owned());
        degraded.push(degradation(
            "regression_evidence_private_path_redacted",
            RegressionCausalitySeverity::Warning,
            format!("Evidence source `{}` contained host-private path text that was suppressed.", input.id),
            Some(&input.id),
            "Regenerate the source artifact through a support-bundle-safe path or provide only path hashes.",
        ));
    }
    if raw_output_seen {
        degraded_codes.push("regression_evidence_raw_output_suppressed".to_owned());
        degraded.push(degradation(
            "regression_evidence_raw_output_suppressed",
            RegressionCausalitySeverity::Info,
            format!("Evidence source `{}` contained raw output fields that were not copied.", input.id),
            Some(&input.id),
            "Use bounded output summaries or content hashes instead of raw stdout, stderr, or logs.",
        ));
    }
    if matches!(
        source_materialization,
        RegressionSourceMaterialization::RemoteCheckoutUnverified
            | RegressionSourceMaterialization::SourceStateRefused
    ) || remote_source_materialized == Some(false)
    {
        degraded_codes.push("regression_evidence_source_not_materialized".to_owned());
    }
    if schema_id.is_none()
        && matches!(
            accepted_kind,
            Some(
                RegressionEvidenceKind::VerificationEvidence
                    | RegressionEvidenceKind::SwarmReplay
                    | RegressionEvidenceKind::PerfReport
                    | RegressionEvidenceKind::SupportBundle
            )
        )
    {
        degraded_codes.push("regression_evidence_schema_missing".to_owned());
        degraded.push(degradation(
            "regression_evidence_schema_missing",
            RegressionCausalitySeverity::Low,
            format!(
                "Evidence source `{}` did not include a schema id.",
                input.id
            ),
            Some(&input.id),
            "Regenerate the artifact with a schema-bearing JSON output mode.",
        ));
    }

    degraded_codes.sort();
    degraded_codes.dedup();

    NormalizedRegressionEvidenceRow {
        id: input.id.clone(),
        kind,
        schema_id,
        status,
        verdict,
        artifact_hash,
        command_hash,
        observed_at,
        source_hash,
        source_materialization,
        remote_source_materialized,
        redaction_status,
        authoritative: status != RegressionEvidenceStatus::Unsupported,
        summary: evidence_summary(
            status,
            accepted_kind.expect("accepted kind checked"),
            &input.id,
        ),
        degraded_codes,
        provenance: RegressionEvidenceProvenance {
            source_fields: present_source_fields(artifact),
            suppressed_fields,
        },
    }
}

fn empty_row(
    input: &RegressionEvidenceInput,
    kind: String,
    status: RegressionEvidenceStatus,
    redaction_status: RegressionRedactionStatus,
    summary: &str,
) -> NormalizedRegressionEvidenceRow {
    NormalizedRegressionEvidenceRow {
        id: input.id.clone(),
        kind,
        schema_id: None,
        status,
        verdict: None,
        artifact_hash: input.artifact_hash_override.clone(),
        command_hash: None,
        observed_at: None,
        source_hash: None,
        source_materialization: RegressionSourceMaterialization::Unknown,
        remote_source_materialized: None,
        redaction_status,
        authoritative: false,
        summary: summary.to_owned(),
        degraded_codes: vec![format!("regression_evidence_{status}")],
        provenance: RegressionEvidenceProvenance {
            source_fields: Vec::new(),
            suppressed_fields: Vec::new(),
        },
    }
}

fn degradation(
    code: &str,
    severity: RegressionCausalitySeverity,
    message: String,
    evidence_source_id: Option<&str>,
    repair: &str,
) -> RegressionNormalizationDegradation {
    RegressionNormalizationDegradation {
        code: code.to_owned(),
        severity,
        message,
        evidence_source_id: evidence_source_id.map(str::to_owned),
        repair: repair.to_owned(),
    }
}

fn evidence_summary(
    status: RegressionEvidenceStatus,
    kind: RegressionEvidenceKind,
    id: &str,
) -> String {
    match status {
        RegressionEvidenceStatus::Available => {
            format!("Normalized {kind} evidence `{id}` as an available redaction-safe summary.")
        }
        RegressionEvidenceStatus::Blocked => {
            format!("Normalized {kind} evidence `{id}` as a first-class blocked state.")
        }
        RegressionEvidenceStatus::Stale => {
            format!("Normalized {kind} evidence `{id}` as stale evidence requiring refresh.")
        }
        RegressionEvidenceStatus::Malformed => {
            format!("Normalized {kind} evidence `{id}` as malformed evidence.")
        }
        RegressionEvidenceStatus::Missing => {
            format!("Normalized {kind} evidence `{id}` as missing evidence.")
        }
        RegressionEvidenceStatus::Unsupported => {
            format!("Normalized {kind} evidence `{id}` as unsupported evidence.")
        }
        RegressionEvidenceStatus::RedactedOnly => {
            format!("Normalized {kind} evidence `{id}` as hash-only or redacted-only evidence.")
        }
    }
}

fn status_from_artifact(artifact: &JsonValue, verdict: Option<&str>) -> RegressionEvidenceStatus {
    if path_bool(artifact, &["stale"]).unwrap_or(false) {
        return RegressionEvidenceStatus::Stale;
    }
    if path_bool(artifact, &["localFallbackRefused"]).unwrap_or(false)
        || path_bool(artifact, &["selectorAdmission", "localFallbackRefused"]).unwrap_or(false)
        || path_bool(artifact, &["selector_admission", "local_fallback_refused"]).unwrap_or(false)
        || path_bool(
            artifact,
            &["selectorAdmissionProbe", "localFallbackRefused"],
        )
        .unwrap_or(false)
        || path_bool(
            artifact,
            &["selector_admission_probe", "local_fallback_refused"],
        )
        .unwrap_or(false)
        || (source_materialization_from_artifact(artifact)
            == RegressionSourceMaterialization::RemoteCheckoutUnverified
            && first_path_bool(
                artifact,
                &[
                    &["remoteSourceMaterialized"],
                    &["remote_source_materialized"],
                    &["sourceState", "remoteSourceMaterialized"],
                    &["source_state", "remote_source_materialized"],
                ],
            ) == Some(false))
    {
        return RegressionEvidenceStatus::Blocked;
    }

    let Some(verdict) = verdict else {
        return RegressionEvidenceStatus::Available;
    };
    match normalized_token(verdict).as_str() {
        "missing" => RegressionEvidenceStatus::Missing,
        "malformed" | "invalid" | "parse_error" => RegressionEvidenceStatus::Malformed,
        "stale" => RegressionEvidenceStatus::Stale,
        "blocked"
        | "selection_failed"
        | "source_state_refused"
        | "fallback_refused"
        | "rch_environment_failure" => RegressionEvidenceStatus::Blocked,
        "unsupported" => RegressionEvidenceStatus::Unsupported,
        "redacted_only" | "hash_only" => RegressionEvidenceStatus::RedactedOnly,
        _ => RegressionEvidenceStatus::Available,
    }
}

fn source_materialization_from_artifact(artifact: &JsonValue) -> RegressionSourceMaterialization {
    let explicit = first_path_string(
        artifact,
        &[
            &["sourceMaterialization"],
            &["source_materialization"],
            &["sourceState", "materialization"],
            &["source_state", "materialization"],
        ],
    );

    let explicit_token = explicit.as_deref().map(normalized_token);
    match explicit_token.as_deref() {
        Some("committed_tree") => RegressionSourceMaterialization::CommittedTree,
        Some("dirty_source_materialized") => {
            RegressionSourceMaterialization::DirtySourceMaterialized
        }
        Some("remote_checkout_unverified") => {
            RegressionSourceMaterialization::RemoteCheckoutUnverified
        }
        Some("source_state_refused") => RegressionSourceMaterialization::SourceStateRefused,
        Some("not_applicable") => RegressionSourceMaterialization::NotApplicable,
        Some("unknown") => RegressionSourceMaterialization::Unknown,
        Some(_) | None => RegressionSourceMaterialization::Unknown,
    }
}

fn redaction_status_from_artifact(
    artifact: &JsonValue,
    private_path_seen: bool,
) -> RegressionRedactionStatus {
    if private_path_seen {
        return RegressionRedactionStatus::Redacted;
    }
    if path_bool(artifact, &["redaction", "refused"]).unwrap_or(false) {
        return RegressionRedactionStatus::Refused;
    }
    if path_bool(artifact, &["outputSummary", "redacted"]).unwrap_or(false)
        || path_bool(artifact, &["redacted"]).unwrap_or(false)
    {
        return RegressionRedactionStatus::Redacted;
    }

    let explicit = first_path_string(
        artifact,
        &[
            &["redactionStatus"],
            &["redaction_status"],
            &["redaction", "status"],
        ],
    );
    let explicit_token = explicit.as_deref().map(normalized_token);
    match explicit_token.as_deref() {
        Some("safe" | "clean") => RegressionRedactionStatus::Safe,
        Some("redacted") => RegressionRedactionStatus::Redacted,
        Some("hash_only") => RegressionRedactionStatus::HashOnly,
        Some("refused") => RegressionRedactionStatus::Refused,
        Some(_) | None => RegressionRedactionStatus::Unknown,
    }
}

fn collect_degraded_codes(artifact: &JsonValue) -> Vec<String> {
    let mut codes = BTreeSet::new();
    collect_codes_at(artifact, &["degradedCodes"], &mut codes);
    collect_codes_at(artifact, &["degraded_codes"], &mut codes);
    collect_codes_at(artifact, &["degraded"], &mut codes);
    collect_codes_at(artifact, &["sourceState", "degradedCodes"], &mut codes);
    collect_codes_at(
        artifact,
        &["selectorAdmission", "degradedCodes"],
        &mut codes,
    );
    codes.into_iter().collect()
}

fn collect_codes_at(artifact: &JsonValue, path: &[&str], codes: &mut BTreeSet<String>) {
    let Some(value) = path_value(artifact, path) else {
        return;
    };
    match value {
        JsonValue::String(code) => {
            if let Some(code) = normalized_non_empty_str(code) {
                codes.insert(code.to_owned());
            }
        }
        JsonValue::Array(entries) => {
            for entry in entries {
                match entry {
                    JsonValue::String(code) => {
                        if let Some(code) = normalized_non_empty_str(code) {
                            codes.insert(code.to_owned());
                        }
                    }
                    JsonValue::Object(object) => {
                        if let Some(code) = first_string(object, &["code", "id", "ruleId"]) {
                            codes.insert(code);
                        }
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }
}

fn present_source_fields(artifact: &JsonValue) -> Vec<String> {
    let mut fields = BTreeSet::new();
    collect_present_fields("", artifact, &mut fields);
    fields
        .into_iter()
        .filter(|field| !is_suppressed_field(field))
        .take(32)
        .collect()
}

fn collect_present_fields(prefix: &str, value: &JsonValue, fields: &mut BTreeSet<String>) {
    if fields.len() >= 64 {
        return;
    }
    match value {
        JsonValue::Object(object) => {
            for (key, child) in object {
                let path = if prefix.is_empty() {
                    key.clone()
                } else {
                    format!("{prefix}.{key}")
                };
                fields.insert(path.clone());
                collect_present_fields(&path, child, fields);
            }
        }
        JsonValue::Array(entries) => {
            for child in entries.iter().take(4) {
                collect_present_fields(prefix, child, fields);
            }
        }
        _ => {}
    }
}

fn suppressed_field_report(artifact: &JsonValue) -> (Vec<String>, bool, bool) {
    let mut suppressed = BTreeSet::new();
    let mut private_path_seen = false;
    let mut raw_output_seen = false;
    scan_suppressed(
        "",
        artifact,
        &mut suppressed,
        &mut private_path_seen,
        &mut raw_output_seen,
    );
    (
        suppressed.into_iter().collect(),
        private_path_seen,
        raw_output_seen,
    )
}

fn scan_suppressed(
    prefix: &str,
    value: &JsonValue,
    suppressed: &mut BTreeSet<String>,
    private_path_seen: &mut bool,
    raw_output_seen: &mut bool,
) {
    match value {
        JsonValue::Object(object) => {
            for (key, child) in object {
                let path = if prefix.is_empty() {
                    key.clone()
                } else {
                    format!("{prefix}.{key}")
                };
                if is_suppressed_field(&path) {
                    suppressed.insert(path.clone());
                    if is_raw_output_field(&path) {
                        *raw_output_seen = true;
                    }
                }
                scan_suppressed(&path, child, suppressed, private_path_seen, raw_output_seen);
            }
        }
        JsonValue::Array(entries) => {
            for child in entries.iter().take(16) {
                scan_suppressed(
                    prefix,
                    child,
                    suppressed,
                    private_path_seen,
                    raw_output_seen,
                );
            }
        }
        JsonValue::String(text) if looks_like_private_path(text) => {
            *private_path_seen = true;
            if !prefix.is_empty() {
                suppressed.insert(prefix.to_owned());
            }
        }
        _ => {}
    }
}

fn is_suppressed_field(path: &str) -> bool {
    let token = normalized_token(path);
    token.contains("stdout")
        || token.contains("stderr")
        || token.contains("raw_log")
        || token == "logs"
        || token.ends_with("_logs")
        || token.contains("mail_body")
        || token.contains("memory_body")
        || token.contains("environment_variables")
        || token.contains("env_dump")
        || token.ends_with("_path")
        || token.contains("private_path")
}

fn is_raw_output_field(path: &str) -> bool {
    let token = normalized_token(path);
    token.contains("stdout")
        || token.contains("stderr")
        || token.contains("raw_log")
        || token == "logs"
        || token.ends_with("_logs")
}

fn looks_like_private_path(text: &str) -> bool {
    text.contains("/Users/")
        || text.contains("/Volumes/")
        || text.contains("/data/projects/")
        || text.contains("\\Users\\")
}

fn first_string(object: &serde_json::Map<String, JsonValue>, keys: &[&str]) -> Option<String> {
    let lookup = object
        .iter()
        .map(|(key, value)| (normalized_token(key), value))
        .collect::<HashMap<_, _>>();
    keys.iter().find_map(|key| {
        lookup
            .get(&normalized_token(key))
            .and_then(|value| value.as_str())
            .and_then(normalized_non_empty_str)
            .map(str::to_owned)
    })
}

fn first_path_string(artifact: &JsonValue, paths: &[&[&str]]) -> Option<String> {
    paths.iter().find_map(|path| {
        path_value(artifact, path)
            .and_then(JsonValue::as_str)
            .and_then(normalized_non_empty_str)
            .map(str::to_owned)
    })
}

fn path_bool(artifact: &JsonValue, path: &[&str]) -> Option<bool> {
    path_value(artifact, path).and_then(JsonValue::as_bool)
}

fn first_path_bool(artifact: &JsonValue, paths: &[&[&str]]) -> Option<bool> {
    paths
        .iter()
        .find_map(|path| path_value(artifact, path).and_then(JsonValue::as_bool))
}

fn path_value<'a>(artifact: &'a JsonValue, path: &[&str]) -> Option<&'a JsonValue> {
    let mut value = artifact;
    for segment in path {
        let object = value.as_object()?;
        value = object
            .iter()
            .find(|(key, _)| normalized_token(key) == normalized_token(segment))
            .map(|(_, value)| value)?;
    }
    Some(value)
}

fn normalized_token(input: &str) -> String {
    let mut token = String::with_capacity(input.len());
    let mut previous_was_lowercase_or_digit = false;

    for character in input.trim().chars() {
        match character {
            '-' | '_' | '.' | ' ' => {
                if !token.ends_with('_') {
                    token.push('_');
                }
                previous_was_lowercase_or_digit = false;
            }
            ch if ch.is_ascii_uppercase() => {
                if previous_was_lowercase_or_digit && !token.ends_with('_') {
                    token.push('_');
                }
                token.push(ch.to_ascii_lowercase());
                previous_was_lowercase_or_digit = false;
            }
            ch if ch.is_ascii_alphanumeric() => {
                token.push(ch.to_ascii_lowercase());
                previous_was_lowercase_or_digit = ch.is_ascii_lowercase() || ch.is_ascii_digit();
            }
            _ => {
                if !token.ends_with('_') {
                    token.push('_');
                }
                previous_was_lowercase_or_digit = false;
            }
        }
    }

    token.trim_matches('_').to_owned()
}

fn normalized_non_empty_string(input: String) -> Option<String> {
    normalized_non_empty_str(&input).map(str::to_owned)
}

fn normalized_non_empty_str(input: &str) -> Option<&str> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeSet, HashMap};

    use serde::Serialize;
    use serde_json::json;

    #[test]
    fn normalizes_all_accepted_evidence_kinds() {
        let inputs = RegressionEvidenceKind::ALL
            .into_iter()
            .map(|kind| {
                RegressionEvidenceInput::new(
                    format!("evidence:{kind}"),
                    kind,
                    json!({
                        "schema": format!("ee.{kind}.v1"),
                        "status": "passed",
                        "artifactHash": format!("blake3:{kind}"),
                        "commandHash": "blake3:command",
                        "observedAt": "2026-06-04T00:00:00Z",
                        "sourceHash": "git:tree",
                        "redactionStatus": "safe"
                    }),
                )
            })
            .collect::<Vec<_>>();

        let report = normalize_regression_evidence_inputs(&inputs);

        assert_eq!(report.schema, REGRESSION_EVIDENCE_NORMALIZATION_SCHEMA_V1);
        assert_eq!(report.rows.len(), RegressionEvidenceKind::ALL.len());
        assert!(report.degraded.is_empty());
        assert!(
            report
                .rows
                .iter()
                .all(|row| row.status == RegressionEvidenceStatus::Available)
        );
        assert!(
            report
                .rows
                .iter()
                .all(|row| row.redaction_status == RegressionRedactionStatus::Safe)
        );
        assert!(
            report
                .rows
                .iter()
                .all(|row| row.command_hash.as_deref() == Some("blake3:command"))
        );
    }

    #[test]
    fn missing_malformed_and_unsupported_inputs_emit_repair_hints() {
        let inputs = vec![
            RegressionEvidenceInput::new(
                "evidence:missing",
                RegressionEvidenceKind::VerificationEvidence,
                None,
            ),
            RegressionEvidenceInput::new(
                "evidence:malformed",
                RegressionEvidenceKind::PerfReport,
                json!("raw log text"),
            ),
            RegressionEvidenceInput::unsupported("evidence:unsupported", "raw_http_trace", None),
        ];

        let report = normalize_regression_evidence_inputs(&inputs);
        let statuses = report
            .rows
            .iter()
            .map(|row| (row.id.as_str(), row.status))
            .collect::<HashMap<_, _>>();

        assert_eq!(
            statuses.get("evidence:missing"),
            Some(&RegressionEvidenceStatus::Missing)
        );
        assert_eq!(
            statuses.get("evidence:malformed"),
            Some(&RegressionEvidenceStatus::Malformed)
        );
        assert_eq!(
            statuses.get("evidence:unsupported"),
            Some(&RegressionEvidenceStatus::Unsupported)
        );
        assert_eq!(report.degraded.len(), 3);
        assert!(report.degraded.iter().all(|entry| !entry.repair.is_empty()));
    }

    #[test]
    fn blocked_stale_and_redacted_states_are_first_class() {
        let inputs = vec![
            RegressionEvidenceInput::new(
                "evidence:rch",
                RegressionEvidenceKind::RchSelectorAdmission,
                json!({
                    "schema": "ee.rch.selector_admission_probe.v1",
                    "status": "selection_failed",
                    "selectorAdmission": {
                        "localFallbackRefused": true,
                        "degradedCodes": ["rch_remote_required_fallback_prevented"]
                    },
                    "redactionStatus": "safe"
                }),
            ),
            RegressionEvidenceInput::new(
                "evidence:pack",
                RegressionEvidenceKind::PackReplay,
                json!({
                    "schema": "ee.pack_replay.v1",
                    "stale": true,
                    "artifactHash": "blake3:pack",
                    "redactionStatus": "safe"
                }),
            ),
            RegressionEvidenceInput::new(
                "evidence:bundle",
                RegressionEvidenceKind::SupportBundle,
                json!({
                    "schema": "ee.support_bundle.v1",
                    "status": "available",
                    "artifactHash": "blake3:bundle",
                    "redactionStatus": "hash_only"
                }),
            ),
        ];

        let report = normalize_regression_evidence_inputs(&inputs);
        let statuses = report
            .rows
            .iter()
            .map(|row| (row.id.as_str(), row.status))
            .collect::<HashMap<_, _>>();

        assert_eq!(
            statuses.get("evidence:rch"),
            Some(&RegressionEvidenceStatus::Blocked)
        );
        assert_eq!(
            statuses.get("evidence:pack"),
            Some(&RegressionEvidenceStatus::Stale)
        );
        assert_eq!(
            statuses.get("evidence:bundle"),
            Some(&RegressionEvidenceStatus::RedactedOnly)
        );
    }

    #[test]
    fn rch_environment_failure_with_unmaterialized_remote_source_is_blocked() {
        let inputs = vec![RegressionEvidenceInput::new(
            "evidence:rch-live",
            RegressionEvidenceKind::RchSelectorAdmission,
            json!({
                "schema": "ee.rch.verify.v1",
                "status": "rch_environment_failure",
                "commandHash": "blake3:verify-command",
                "selector_admission_probe": {
                    "status": "selection_failed",
                    "selection_failure_reason": "all_workers_preflight_failed",
                    "local_fallback_refused": true
                },
                "source_materialization": "remote_checkout_unverified",
                "remote_source_materialized": false,
                "redactionStatus": "safe"
            }),
        )];

        let report = normalize_regression_evidence_inputs(&inputs);
        let row = &report.rows[0];

        assert_eq!(row.status, RegressionEvidenceStatus::Blocked);
        assert_eq!(
            row.source_materialization,
            RegressionSourceMaterialization::RemoteCheckoutUnverified
        );
        assert_eq!(row.remote_source_materialized, Some(false));
        assert!(
            row.degraded_codes
                .iter()
                .any(|code| code == "regression_evidence_source_not_materialized")
        );
    }

    #[test]
    fn suppresses_raw_output_and_private_paths() {
        let inputs = vec![RegressionEvidenceInput::new(
            "evidence:verify",
            RegressionEvidenceKind::VerificationEvidence,
            json!({
                "schema": "ee.verification.evidence.v1",
                "status": "failed",
                "artifactHash": "blake3:verify",
                "stderrTail": "error in /Users/jemanuel/projects/eidetic_engine_cli/src/main.rs",
                "outputSummary": {
                    "stdoutTail": "raw output",
                    "redacted": true
                }
            }),
        )];

        let report = normalize_regression_evidence_inputs(&inputs);
        let row = &report.rows[0];

        assert_eq!(row.status, RegressionEvidenceStatus::Available);
        assert_eq!(row.redaction_status, RegressionRedactionStatus::Redacted);
        assert!(
            row.degraded_codes
                .iter()
                .any(|code| code == "regression_evidence_private_path_redacted")
        );
        assert!(
            row.provenance
                .suppressed_fields
                .iter()
                .any(|field| field.contains("stderr"))
        );
        assert!(!row.summary.contains("/Users/"));
    }

    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct KindStatusMatrixReport {
        schema: &'static str,
        rows: Vec<KindStatusSummary>,
    }

    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct KindStatusSummary {
        kind: String,
        available: usize,
        blocked: usize,
        malformed: usize,
        stale: usize,
        failed_verdict_preserved: bool,
        degraded_codes: Vec<String>,
        redaction_statuses: Vec<String>,
    }

    #[test]
    fn golden_kind_status_matrix_covers_all_evidence_kinds() {
        let mut inputs = Vec::new();
        for kind in RegressionEvidenceKind::ALL {
            let kind_name = kind.as_str();
            inputs.push(RegressionEvidenceInput::new(
                format!("{kind_name}:success"),
                kind,
                json!({
                    "schema": format!("ee.{kind_name}.v1"),
                    "status": "passed",
                    "artifactHash": format!("blake3:{kind_name}:success"),
                    "redactionStatus": "safe"
                }),
            ));
            inputs.push(RegressionEvidenceInput::new(
                format!("{kind_name}:failure"),
                kind,
                json!({
                    "schema": format!("ee.{kind_name}.v1"),
                    "status": "failed",
                    "artifactHash": format!("blake3:{kind_name}:failure"),
                    "redactionStatus": "safe"
                }),
            ));
            inputs.push(RegressionEvidenceInput::new(
                format!("{kind_name}:degraded"),
                kind,
                json!({
                    "schema": format!("ee.{kind_name}.v1"),
                    "status": "passed",
                    "artifactHash": format!("blake3:{kind_name}:degraded"),
                    "degradedCodes": ["matrix_degraded"],
                    "redactionStatus": "safe"
                }),
            ));
            inputs.push(RegressionEvidenceInput::new(
                format!("{kind_name}:stale"),
                kind,
                json!({
                    "schema": format!("ee.{kind_name}.v1"),
                    "stale": true,
                    "artifactHash": format!("blake3:{kind_name}:stale"),
                    "redactionStatus": "safe"
                }),
            ));
            inputs.push(RegressionEvidenceInput::new(
                format!("{kind_name}:blocked"),
                kind,
                json!({
                    "schema": format!("ee.{kind_name}.v1"),
                    "status": "blocked",
                    "localFallbackRefused": true,
                    "artifactHash": format!("blake3:{kind_name}:blocked"),
                    "redactionStatus": "safe"
                }),
            ));
            inputs.push(RegressionEvidenceInput::new(
                format!("{kind_name}:malformed"),
                kind,
                json!("raw artifact should not be copied"),
            ));
        }

        let report = normalize_regression_evidence_inputs(&inputs);
        assert_eq!(
            report.rows.len(),
            RegressionEvidenceKind::ALL.len() * 6,
            "each accepted evidence kind should have six matrix rows"
        );

        let mut rows = Vec::new();
        for kind in RegressionEvidenceKind::ALL {
            let kind_name = kind.as_str();
            let kind_rows = report
                .rows
                .iter()
                .filter(|row| row.kind == kind_name)
                .collect::<Vec<_>>();
            let degraded_codes = kind_rows
                .iter()
                .flat_map(|row| row.degraded_codes.iter().map(String::as_str))
                .collect::<BTreeSet<_>>()
                .into_iter()
                .map(str::to_owned)
                .collect::<Vec<_>>();
            let redaction_statuses = kind_rows
                .iter()
                .map(|row| row.redaction_status.as_str())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .map(str::to_owned)
                .collect::<Vec<_>>();

            rows.push(KindStatusSummary {
                kind: kind_name.to_owned(),
                available: kind_rows
                    .iter()
                    .filter(|row| row.status == RegressionEvidenceStatus::Available)
                    .count(),
                blocked: kind_rows
                    .iter()
                    .filter(|row| row.status == RegressionEvidenceStatus::Blocked)
                    .count(),
                malformed: kind_rows
                    .iter()
                    .filter(|row| row.status == RegressionEvidenceStatus::Malformed)
                    .count(),
                stale: kind_rows
                    .iter()
                    .filter(|row| row.status == RegressionEvidenceStatus::Stale)
                    .count(),
                failed_verdict_preserved: kind_rows.iter().any(|row| {
                    row.id.ends_with(":failure")
                        && row.verdict.as_deref() == Some("failed")
                        && row.status == RegressionEvidenceStatus::Available
                }),
                degraded_codes,
                redaction_statuses,
            });
        }
        rows.sort_by(|left, right| left.kind.cmp(&right.kind));

        let matrix = KindStatusMatrixReport {
            schema: "ee.regression_evidence_kind_status_matrix.v1",
            rows,
        };
        let actual = serde_json::to_string_pretty(&matrix).expect("serialize kind status matrix");
        let expected = include_str!(
            "../../tests/fixtures/golden/regression_causality/kind_status_matrix.json"
        )
        .trim_end();

        assert_eq!(actual, expected);
    }

    #[test]
    fn golden_normalized_rows_are_stable() {
        let inputs = vec![
            RegressionEvidenceInput::new(
                "evidence:beads",
                RegressionEvidenceKind::BeadsHistory,
                json!({
                    "status": "available",
                    "artifactHash": "blake3:beads",
                    "observedAt": "2026-06-04T01:30:00Z",
                    "redactionStatus": "hash_only"
                }),
            ),
            RegressionEvidenceInput::new(
                "evidence:rch",
                RegressionEvidenceKind::RchSelectorAdmission,
                json!({
                    "schema": "ee.rch.selector_admission_probe.v1",
                    "status": "selection_failed",
                    "commandHash": "blake3:verify-command",
                    "sourceState": {
                        "sourceHash": "git:tree-123",
                        "degradedCodes": ["rch_verify_remote_source_unknown"]
                    },
                    "selectorAdmission": {
                        "localFallbackRefused": true
                    },
                    "redactionStatus": "safe"
                }),
            ),
            RegressionEvidenceInput::new(
                "evidence:events",
                RegressionEvidenceKind::E2eEventLog,
                json!({
                    "schema": "ee.test_event.v1",
                    "status": "failed",
                    "artifactHash": "blake3:events",
                    "timestamp": "2026-06-04T01:31:00Z",
                    "redactionStatus": "safe",
                    "stderrTail": "suppressed"
                }),
            ),
        ];

        let report = normalize_regression_evidence_inputs(&inputs);
        let actual =
            serde_json::to_string_pretty(&report).expect("serialize regression normalization");
        let expected = include_str!(
            "../../tests/fixtures/golden/regression_causality/normalized_sources.json"
        )
        .trim_end();

        assert_eq!(actual, expected);
    }
}
