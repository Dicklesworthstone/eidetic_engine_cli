//! Redaction-safe normalization models for regression causality capsules.
//!
//! The capsule schema is intentionally compact. This module owns the read-only
//! evidence normalization layer that turns heterogeneous verification, replay,
//! pack, perf, tracker, git, and support-bundle artifacts into deterministic
//! rows without copying raw logs or private checkout paths.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

pub const REGRESSION_CAUSALITY_SCHEMA_V1: &str = "ee.regression_causality.v1";
pub const REGRESSION_EVIDENCE_NORMALIZATION_SCHEMA_V1: &str =
    "ee.regression_evidence_normalization.v1";
pub const REGRESSION_HYPOTHESIS_RANKING_SCHEMA_V1: &str = "ee.regression_hypothesis_ranking.v1";

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
    pub const ALL: [Self; 7] = [
        Self::Available,
        Self::Missing,
        Self::Malformed,
        Self::Stale,
        Self::Blocked,
        Self::Unsupported,
        Self::RedactedOnly,
    ];

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
    pub const ALL: [Self; 5] = [
        Self::Safe,
        Self::Redacted,
        Self::HashOnly,
        Self::Refused,
        Self::Unknown,
    ];

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
    pub const ALL: [Self; 6] = [
        Self::CommittedTree,
        Self::DirtySourceMaterialized,
        Self::RemoteCheckoutUnverified,
        Self::SourceStateRefused,
        Self::NotApplicable,
        Self::Unknown,
    ];

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
    pub const ALL: [Self; 6] = [
        Self::Info,
        Self::Low,
        Self::Warning,
        Self::Medium,
        Self::High,
        Self::Critical,
    ];

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

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RegressionCauseHypothesisCode {
    SourceNotMaterialized,
    SchemaContractDrift,
    StaleDerivedAsset,
    KnownEnvironmentBlocker,
    OutputBudgetRegression,
    FixtureGap,
    PackSelectionChange,
    PerfBudgetRegression,
    TrackerStateMismatch,
    UnknownInsufficientEvidence,
}

impl RegressionCauseHypothesisCode {
    pub const ALL: [Self; 10] = [
        Self::SourceNotMaterialized,
        Self::SchemaContractDrift,
        Self::StaleDerivedAsset,
        Self::KnownEnvironmentBlocker,
        Self::OutputBudgetRegression,
        Self::FixtureGap,
        Self::PackSelectionChange,
        Self::PerfBudgetRegression,
        Self::TrackerStateMismatch,
        Self::UnknownInsufficientEvidence,
    ];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SourceNotMaterialized => "source_not_materialized",
            Self::SchemaContractDrift => "schema_contract_drift",
            Self::StaleDerivedAsset => "stale_derived_asset",
            Self::KnownEnvironmentBlocker => "known_environment_blocker",
            Self::OutputBudgetRegression => "output_budget_regression",
            Self::FixtureGap => "fixture_gap",
            Self::PackSelectionChange => "pack_selection_change",
            Self::PerfBudgetRegression => "perf_budget_regression",
            Self::TrackerStateMismatch => "tracker_state_mismatch",
            Self::UnknownInsufficientEvidence => "unknown_insufficient_evidence",
        }
    }
}

impl fmt::Display for RegressionCauseHypothesisCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RegressionCounterEvidenceEffect {
    Supports,
    Weakens,
    Neutral,
    MissingRequiredSource,
}

impl RegressionCounterEvidenceEffect {
    pub const ALL: [Self; 4] = [
        Self::Supports,
        Self::Weakens,
        Self::Neutral,
        Self::MissingRequiredSource,
    ];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Supports => "supports",
            Self::Weakens => "weakens",
            Self::Neutral => "neutral",
            Self::MissingRequiredSource => "missing_required_source",
        }
    }
}

impl fmt::Display for RegressionCounterEvidenceEffect {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegressionCounterEvidence {
    pub source_id: Option<String>,
    pub summary: String,
    pub effect: RegressionCounterEvidenceEffect,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RegressionOwnerHintKind {
    Bead,
    Agent,
    Module,
    Command,
    Unknown,
}

impl RegressionOwnerHintKind {
    pub const ALL: [Self; 5] = [
        Self::Bead,
        Self::Agent,
        Self::Module,
        Self::Command,
        Self::Unknown,
    ];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Bead => "bead",
            Self::Agent => "agent",
            Self::Module => "module",
            Self::Command => "command",
            Self::Unknown => "unknown",
        }
    }
}

impl fmt::Display for RegressionOwnerHintKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegressionOwnerHint {
    pub kind: RegressionOwnerHintKind,
    pub value: String,
    pub confidence: f64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegressionCausalityCommand {
    pub command: String,
    pub rationale: String,
    pub mutates_workspace: bool,
    pub requires_rch: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegressionCauseHypothesis {
    pub rank: usize,
    pub code: RegressionCauseHypothesisCode,
    pub confidence: f64,
    pub severity: RegressionCausalitySeverity,
    pub summary: String,
    pub evidence_refs: Vec<String>,
    pub counter_evidence: Vec<RegressionCounterEvidence>,
    pub owner_hints: Vec<RegressionOwnerHint>,
    pub next_commands: Vec<RegressionCausalityCommand>,
    pub authoritative: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegressionHypothesisRankingReport {
    pub schema: String,
    pub hypotheses: Vec<RegressionCauseHypothesis>,
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

#[must_use]
pub fn rank_regression_cause_hypotheses(
    rows: &[NormalizedRegressionEvidenceRow],
) -> RegressionHypothesisRankingReport {
    let mut accumulators = BTreeMap::<RegressionCauseHypothesisCode, HypothesisAccumulator>::new();

    for row in rows {
        classify_source_materialization(row, &mut accumulators);
        classify_environment_blocker(row, &mut accumulators);
        classify_schema_contract(row, &mut accumulators);
        classify_staleness(row, &mut accumulators);
        classify_fixture_gap(row, &mut accumulators);
        classify_pack_selection(row, &mut accumulators);
        classify_perf_regression(row, &mut accumulators);
        classify_output_budget(row, &mut accumulators);
        classify_tracker_state(row, &mut accumulators);
    }

    if accumulators.is_empty() {
        let mut accumulator = HypothesisAccumulator::default();
        accumulator.points = if rows.is_empty() { 42 } else { 34 };
        for row in rows.iter().take(4) {
            accumulator.evidence_refs.insert(row.id.clone());
            accumulator.counter_evidence.push(RegressionCounterEvidence {
                source_id: Some(row.id.clone()),
                summary: format!(
                    "Evidence `{}` was normalized as {}, but did not match a stronger cause category.",
                    row.id, row.status
                ),
                effect: RegressionCounterEvidenceEffect::Neutral,
            });
        }
        accumulator
            .counter_evidence
            .push(RegressionCounterEvidence {
                source_id: None,
                summary: "No direct failing source category had enough normalized evidence."
                    .to_owned(),
                effect: RegressionCounterEvidenceEffect::MissingRequiredSource,
            });
        accumulators.insert(
            RegressionCauseHypothesisCode::UnknownInsufficientEvidence,
            accumulator,
        );
    }

    let missing_required = missing_required_source_degradations(rows);
    if !missing_required.is_empty()
        && !accumulators.contains_key(&RegressionCauseHypothesisCode::UnknownInsufficientEvidence)
    {
        let mut accumulator = HypothesisAccumulator {
            points: 28,
            ..HypothesisAccumulator::default()
        };
        accumulator
            .counter_evidence
            .extend(
                missing_required
                    .iter()
                    .map(|kind| RegressionCounterEvidence {
                        source_id: None,
                        summary: format!(
                            "No `{kind}` evidence source was present in the normalized rows."
                        ),
                        effect: RegressionCounterEvidenceEffect::MissingRequiredSource,
                    }),
            );
        accumulators.insert(
            RegressionCauseHypothesisCode::UnknownInsufficientEvidence,
            accumulator,
        );
    }

    let mut hypotheses = accumulators
        .into_iter()
        .map(|(code, mut accumulator)| {
            add_counter_evidence(code, rows, &mut accumulator);
            accumulator.into_hypothesis(code)
        })
        .collect::<Vec<_>>();

    hypotheses.sort_by(|left, right| {
        right
            .confidence
            .partial_cmp(&left.confidence)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| right.severity.cmp(&left.severity))
            .then_with(|| left.code.cmp(&right.code))
    });
    for (index, hypothesis) in hypotheses.iter_mut().enumerate() {
        hypothesis.rank = index + 1;
    }

    RegressionHypothesisRankingReport {
        schema: REGRESSION_HYPOTHESIS_RANKING_SCHEMA_V1.to_owned(),
        hypotheses,
        degraded: missing_required
            .into_iter()
            .map(|kind| {
                degradation(
                    "regression_hypothesis_missing_required_source",
                    RegressionCausalitySeverity::Warning,
                    format!("No `{kind}` evidence source was available for hypothesis ranking."),
                    None,
                    "Provide the missing structured artifact before treating low-confidence hypotheses as actionable.",
                )
            })
            .collect(),
    }
}

#[derive(Default)]
struct HypothesisAccumulator {
    points: u16,
    evidence_refs: BTreeSet<String>,
    counter_evidence: Vec<RegressionCounterEvidence>,
    owner_hints: Vec<RegressionOwnerHint>,
    next_commands: Vec<RegressionCausalityCommand>,
}

impl HypothesisAccumulator {
    fn add_support(&mut self, row: &NormalizedRegressionEvidenceRow, points: u16) {
        self.points = self.points.saturating_add(points).min(95);
        self.evidence_refs.insert(row.id.clone());
        self.owner_hints.extend(owner_hints_for_row(row));
    }

    fn into_hypothesis(mut self, code: RegressionCauseHypothesisCode) -> RegressionCauseHypothesis {
        self.owner_hints.sort_by(|left, right| {
            left.kind
                .cmp(&right.kind)
                .then_with(|| left.value.cmp(&right.value))
        });
        self.owner_hints
            .dedup_by(|left, right| left.kind == right.kind && left.value == right.value);
        if self.owner_hints.is_empty() {
            self.owner_hints.push(default_owner_hint(code));
        }

        self.counter_evidence.sort_by(|left, right| {
            left.source_id
                .cmp(&right.source_id)
                .then_with(|| left.effect.cmp(&right.effect))
                .then_with(|| left.summary.cmp(&right.summary))
        });
        self.counter_evidence.dedup();

        self.next_commands
            .extend(next_commands_for_hypothesis(code));
        self.next_commands.sort_by(|left, right| {
            left.command
                .cmp(&right.command)
                .then_with(|| left.rationale.cmp(&right.rationale))
        });
        self.next_commands.dedup();

        let confidence = ((self.points.max(20) as f64) / 100.0).min(0.95);
        RegressionCauseHypothesis {
            rank: 0,
            code,
            confidence,
            severity: severity_for_hypothesis(code, confidence),
            summary: summary_for_hypothesis(code),
            evidence_refs: self.evidence_refs.into_iter().collect(),
            counter_evidence: self.counter_evidence,
            owner_hints: self.owner_hints,
            next_commands: self.next_commands,
            authoritative: false,
        }
    }
}

fn accumulator_for(
    accumulators: &mut BTreeMap<RegressionCauseHypothesisCode, HypothesisAccumulator>,
    code: RegressionCauseHypothesisCode,
) -> &mut HypothesisAccumulator {
    accumulators.entry(code).or_default()
}

fn classify_source_materialization(
    row: &NormalizedRegressionEvidenceRow,
    accumulators: &mut BTreeMap<RegressionCauseHypothesisCode, HypothesisAccumulator>,
) {
    if matches!(
        row.source_materialization,
        RegressionSourceMaterialization::RemoteCheckoutUnverified
            | RegressionSourceMaterialization::SourceStateRefused
    ) || row.remote_source_materialized == Some(false)
        || row
            .degraded_codes
            .iter()
            .any(|code| code == "regression_evidence_source_not_materialized")
    {
        accumulator_for(
            accumulators,
            RegressionCauseHypothesisCode::SourceNotMaterialized,
        )
        .add_support(row, 84);
    }
}

fn classify_environment_blocker(
    row: &NormalizedRegressionEvidenceRow,
    accumulators: &mut BTreeMap<RegressionCauseHypothesisCode, HypothesisAccumulator>,
) {
    let signal = row.status == RegressionEvidenceStatus::Blocked
        || row
            .verdict
            .as_deref()
            .map(normalized_token)
            .is_some_and(|token| {
                matches!(
                    token.as_str(),
                    "selection_failed" | "rch_environment_failure" | "fallback_refused" | "blocked"
                )
            })
        || row.degraded_codes.iter().any(|code| {
            let token = normalized_token(code);
            token.contains("rch")
                || token.contains("worker")
                || token.contains("topology")
                || token.contains("fallback")
                || token.contains("environment")
        });
    if signal
        && matches!(
            row.kind.as_str(),
            "verification_evidence" | "rch_selector_admission" | "support_bundle"
        )
    {
        accumulator_for(
            accumulators,
            RegressionCauseHypothesisCode::KnownEnvironmentBlocker,
        )
        .add_support(row, 64);
    }
}

fn classify_schema_contract(
    row: &NormalizedRegressionEvidenceRow,
    accumulators: &mut BTreeMap<RegressionCauseHypothesisCode, HypothesisAccumulator>,
) {
    let schema_required = matches!(
        row.kind.as_str(),
        "verification_evidence" | "swarm_replay" | "perf_report" | "support_bundle"
    );
    let schema_signal = row.status == RegressionEvidenceStatus::Malformed
        || (schema_required && row.schema_id.is_none())
        || row.degraded_codes.iter().any(|code| {
            let token = normalized_token(code);
            token.contains("schema") || token.contains("contract") || token.contains("drift")
        });
    if schema_signal {
        accumulator_for(
            accumulators,
            RegressionCauseHypothesisCode::SchemaContractDrift,
        )
        .add_support(
            row,
            if row.status == RegressionEvidenceStatus::Malformed {
                68
            } else {
                50
            },
        );
    }
}

fn classify_staleness(
    row: &NormalizedRegressionEvidenceRow,
    accumulators: &mut BTreeMap<RegressionCauseHypothesisCode, HypothesisAccumulator>,
) {
    if row.status == RegressionEvidenceStatus::Stale
        || row
            .degraded_codes
            .iter()
            .any(|code| normalized_token(code).contains("stale"))
    {
        accumulator_for(
            accumulators,
            RegressionCauseHypothesisCode::StaleDerivedAsset,
        )
        .add_support(row, 58);
    }
}

fn classify_fixture_gap(
    row: &NormalizedRegressionEvidenceRow,
    accumulators: &mut BTreeMap<RegressionCauseHypothesisCode, HypothesisAccumulator>,
) {
    let fixture_signal = row.kind == "degraded_fixture"
        && matches!(
            row.status,
            RegressionEvidenceStatus::Missing
                | RegressionEvidenceStatus::Malformed
                | RegressionEvidenceStatus::Unsupported
        )
        || row.degraded_codes.iter().any(|code| {
            let token = normalized_token(code);
            token.contains("fixture") || token.contains("catalog")
        });
    if fixture_signal {
        accumulator_for(accumulators, RegressionCauseHypothesisCode::FixtureGap)
            .add_support(row, 56);
    }
}

fn classify_pack_selection(
    row: &NormalizedRegressionEvidenceRow,
    accumulators: &mut BTreeMap<RegressionCauseHypothesisCode, HypothesisAccumulator>,
) {
    let pack_signal = matches!(row.kind.as_str(), "pack_replay" | "pack_diff")
        && (row.status != RegressionEvidenceStatus::Available
            || row.degraded_codes.iter().any(|code| {
                let token = normalized_token(code);
                token.contains("pack") || token.contains("selection") || token.contains("omission")
            }));
    if pack_signal {
        accumulator_for(
            accumulators,
            RegressionCauseHypothesisCode::PackSelectionChange,
        )
        .add_support(row, 58);
    }
}

fn classify_perf_regression(
    row: &NormalizedRegressionEvidenceRow,
    accumulators: &mut BTreeMap<RegressionCauseHypothesisCode, HypothesisAccumulator>,
) {
    let perf_signal = row.kind == "perf_report"
        && (row
            .verdict
            .as_deref()
            .map(normalized_token)
            .is_some_and(|token| {
                matches!(token.as_str(), "failed" | "regression" | "budget_exceeded")
            })
            || row.degraded_codes.iter().any(|code| {
                let token = normalized_token(code);
                token.contains("perf") || token.contains("latency")
            }));
    if perf_signal {
        accumulator_for(
            accumulators,
            RegressionCauseHypothesisCode::PerfBudgetRegression,
        )
        .add_support(row, 62);
    }
}

fn classify_output_budget(
    row: &NormalizedRegressionEvidenceRow,
    accumulators: &mut BTreeMap<RegressionCauseHypothesisCode, HypothesisAccumulator>,
) {
    if row.degraded_codes.iter().any(|code| {
        let token = normalized_token(code);
        token.contains("output_budget")
            || token.contains("prompt_budget")
            || token.contains("too_verbose")
    }) {
        accumulator_for(
            accumulators,
            RegressionCauseHypothesisCode::OutputBudgetRegression,
        )
        .add_support(row, 60);
    }
}

fn classify_tracker_state(
    row: &NormalizedRegressionEvidenceRow,
    accumulators: &mut BTreeMap<RegressionCauseHypothesisCode, HypothesisAccumulator>,
) {
    let tracker_signal = matches!(row.kind.as_str(), "beads_history" | "bv_history")
        && (row.status != RegressionEvidenceStatus::Available
            || row.degraded_codes.iter().any(|code| {
                let token = normalized_token(code);
                token.contains("tracker")
                    || token.contains("beads")
                    || token.contains("bv")
                    || token.contains("mismatch")
            }));
    if tracker_signal {
        accumulator_for(
            accumulators,
            RegressionCauseHypothesisCode::TrackerStateMismatch,
        )
        .add_support(row, 60);
    }
}

fn add_counter_evidence(
    code: RegressionCauseHypothesisCode,
    rows: &[NormalizedRegressionEvidenceRow],
    accumulator: &mut HypothesisAccumulator,
) {
    for row in rows {
        if !row.authoritative || accumulator.evidence_refs.contains(&row.id) {
            continue;
        }
        if weakens_hypothesis(code, row) {
            accumulator
                .counter_evidence
                .push(RegressionCounterEvidence {
                    source_id: Some(row.id.clone()),
                    summary: format!("Evidence `{}` was available and weakens `{code}`.", row.id),
                    effect: RegressionCounterEvidenceEffect::Weakens,
                });
        }
    }
}

fn weakens_hypothesis(
    code: RegressionCauseHypothesisCode,
    row: &NormalizedRegressionEvidenceRow,
) -> bool {
    match code {
        RegressionCauseHypothesisCode::SourceNotMaterialized => {
            row.source_materialization == RegressionSourceMaterialization::CommittedTree
                || row.remote_source_materialized == Some(true)
        }
        RegressionCauseHypothesisCode::SchemaContractDrift => {
            row.schema_id.is_some() && row.status == RegressionEvidenceStatus::Available
        }
        RegressionCauseHypothesisCode::StaleDerivedAsset => {
            row.observed_at.is_some() && row.status == RegressionEvidenceStatus::Available
        }
        RegressionCauseHypothesisCode::KnownEnvironmentBlocker => {
            row.status == RegressionEvidenceStatus::Available
                && matches!(
                    row.kind.as_str(),
                    "verification_evidence" | "rch_selector_admission"
                )
        }
        RegressionCauseHypothesisCode::PerfBudgetRegression => {
            row.kind == "perf_report" && row.status == RegressionEvidenceStatus::Available
        }
        RegressionCauseHypothesisCode::TrackerStateMismatch => {
            matches!(row.kind.as_str(), "beads_history" | "bv_history")
                && row.status == RegressionEvidenceStatus::Available
        }
        RegressionCauseHypothesisCode::FixtureGap => {
            row.kind == "degraded_fixture" && row.status == RegressionEvidenceStatus::Available
        }
        RegressionCauseHypothesisCode::PackSelectionChange => {
            matches!(row.kind.as_str(), "pack_replay" | "pack_diff")
                && row.status == RegressionEvidenceStatus::Available
        }
        RegressionCauseHypothesisCode::OutputBudgetRegression => {
            row.kind == "perf_report" && row.status == RegressionEvidenceStatus::Available
        }
        RegressionCauseHypothesisCode::UnknownInsufficientEvidence => false,
    }
}

fn missing_required_source_degradations(rows: &[NormalizedRegressionEvidenceRow]) -> Vec<String> {
    let kinds = rows
        .iter()
        .map(|row| row.kind.as_str())
        .collect::<BTreeSet<_>>();
    ["verification_evidence", "rch_selector_admission"]
        .into_iter()
        .filter(|kind| !kinds.contains(kind))
        .map(str::to_owned)
        .collect()
}

fn owner_hints_for_row(row: &NormalizedRegressionEvidenceRow) -> Vec<RegressionOwnerHint> {
    if let Some(bead_id) = first_bead_id(&row.id) {
        return vec![RegressionOwnerHint {
            kind: RegressionOwnerHintKind::Bead,
            value: bead_id,
            confidence: 0.72,
        }];
    }

    let module = match row.kind.as_str() {
        "verification_evidence" | "rch_selector_admission" => "rch",
        "pack_replay" | "pack_diff" => "pack",
        "perf_report" => "perf",
        "beads_history" | "bv_history" => "tracker",
        "degraded_fixture" => "failure-mode-catalog",
        "git_metadata" => "git",
        "swarm_replay" => "swarm-replay",
        "e2e_event_log" => "e2e",
        "support_bundle" => "support-bundle",
        _ => "regression-causality",
    };
    vec![RegressionOwnerHint {
        kind: RegressionOwnerHintKind::Module,
        value: module.to_owned(),
        confidence: 0.56,
    }]
}

fn first_bead_id(text: &str) -> Option<String> {
    text.split(|character: char| {
        !(character.is_ascii_alphanumeric() || character == '-' || character == '.')
    })
    .find(|token| {
        token
            .strip_prefix("bd-")
            .is_some_and(|suffix| suffix.chars().any(|character| character.is_ascii_digit()))
    })
    .map(str::to_owned)
}

fn default_owner_hint(code: RegressionCauseHypothesisCode) -> RegressionOwnerHint {
    let value = match code {
        RegressionCauseHypothesisCode::SourceNotMaterialized
        | RegressionCauseHypothesisCode::KnownEnvironmentBlocker => "rch",
        RegressionCauseHypothesisCode::SchemaContractDrift => "schema-contracts",
        RegressionCauseHypothesisCode::StaleDerivedAsset => "derived-artifacts",
        RegressionCauseHypothesisCode::OutputBudgetRegression => "output-budget",
        RegressionCauseHypothesisCode::FixtureGap => "failure-mode-catalog",
        RegressionCauseHypothesisCode::PackSelectionChange => "pack",
        RegressionCauseHypothesisCode::PerfBudgetRegression => "perf",
        RegressionCauseHypothesisCode::TrackerStateMismatch => "tracker",
        RegressionCauseHypothesisCode::UnknownInsufficientEvidence => "unknown",
    };
    RegressionOwnerHint {
        kind: if value == "unknown" {
            RegressionOwnerHintKind::Unknown
        } else {
            RegressionOwnerHintKind::Module
        },
        value: value.to_owned(),
        confidence: 0.5,
    }
}

fn next_commands_for_hypothesis(
    code: RegressionCauseHypothesisCode,
) -> Vec<RegressionCausalityCommand> {
    match code {
        RegressionCauseHypothesisCode::SourceNotMaterialized => vec![
            causality_command(
                "ee verify rch runs --json",
                "Inspect recorded RCH verifier runs before treating the failure as a source verdict.",
                false,
            ),
            causality_command(
                "RCH_REQUIRE_REMOTE=1 ./scripts/rch_verify.sh --summary --no-write -- cargo test --all-targets",
                "Rerun remote-only verification and capture source-materialization evidence.",
                true,
            ),
        ],
        RegressionCauseHypothesisCode::KnownEnvironmentBlocker => vec![
            causality_command(
                "ee verify rch blockers --json",
                "List known RCH blockers that can explain a failed proof without source changes.",
                false,
            ),
            causality_command(
                "rch status --workers --jobs",
                "Inspect remote worker topology and slot pressure without launching Cargo.",
                false,
            ),
        ],
        RegressionCauseHypothesisCode::SchemaContractDrift => vec![causality_command(
            "jq empty docs/schemas/ee.regression_causality.v1.json",
            "Validate the causality schema before changing ranking code.",
            false,
        )],
        RegressionCauseHypothesisCode::StaleDerivedAsset => vec![causality_command(
            "git status --short --branch",
            "Refresh source and derived-artifact posture before trusting stale evidence.",
            false,
        )],
        RegressionCauseHypothesisCode::OutputBudgetRegression => vec![causality_command(
            "ee perf prompt-budget --help",
            "Inspect the prompt/output budget surface before changing renderers.",
            false,
        )],
        RegressionCauseHypothesisCode::FixtureGap => vec![causality_command(
            "ls tests/fixtures/failure_modes",
            "Check whether the degraded-code fixture catalog contains the failing mode.",
            false,
        )],
        RegressionCauseHypothesisCode::PackSelectionChange => vec![causality_command(
            "ee pack replay <pack-id> --json",
            "Replay the affected pack before changing retrieval or packing logic.",
            false,
        )],
        RegressionCauseHypothesisCode::PerfBudgetRegression => vec![causality_command(
            "ee perf explain-latency --report <artifact.json> --json",
            "Explain the latency or budget report before changing hot paths.",
            false,
        )],
        RegressionCauseHypothesisCode::TrackerStateMismatch => vec![
            causality_command(
                "br doctor --json",
                "Check Beads tracker health before trusting owner or dependency evidence.",
                false,
            ),
            causality_command(
                "bv --robot-insights",
                "Inspect graph health and dependency contradictions without opening the TUI.",
                false,
            ),
        ],
        RegressionCauseHypothesisCode::UnknownInsufficientEvidence => vec![causality_command(
            "ee verify rch runs --json",
            "Gather structured verifier evidence before guessing from raw logs.",
            false,
        )],
    }
}

fn causality_command(
    command: &str,
    rationale: &str,
    requires_rch: bool,
) -> RegressionCausalityCommand {
    RegressionCausalityCommand {
        command: command.to_owned(),
        rationale: rationale.to_owned(),
        mutates_workspace: false,
        requires_rch,
    }
}

fn severity_for_hypothesis(
    code: RegressionCauseHypothesisCode,
    confidence: f64,
) -> RegressionCausalitySeverity {
    match code {
        RegressionCauseHypothesisCode::SourceNotMaterialized
        | RegressionCauseHypothesisCode::KnownEnvironmentBlocker => {
            if confidence >= 0.8 {
                RegressionCausalitySeverity::High
            } else {
                RegressionCausalitySeverity::Medium
            }
        }
        RegressionCauseHypothesisCode::SchemaContractDrift
        | RegressionCauseHypothesisCode::PerfBudgetRegression
        | RegressionCauseHypothesisCode::TrackerStateMismatch => {
            if confidence >= 0.75 {
                RegressionCausalitySeverity::High
            } else {
                RegressionCausalitySeverity::Medium
            }
        }
        RegressionCauseHypothesisCode::UnknownInsufficientEvidence => {
            RegressionCausalitySeverity::Warning
        }
        _ => RegressionCausalitySeverity::Medium,
    }
}

fn summary_for_hypothesis(code: RegressionCauseHypothesisCode) -> String {
    match code {
        RegressionCauseHypothesisCode::SourceNotMaterialized => {
            "The failing gate cannot yet be used as a source verdict because source materialization was not proven.".to_owned()
        }
        RegressionCauseHypothesisCode::SchemaContractDrift => {
            "A schema, contract, or malformed-artifact mismatch may explain the failure.".to_owned()
        }
        RegressionCauseHypothesisCode::StaleDerivedAsset => {
            "One or more derived artifacts are stale and should be refreshed before blaming source code.".to_owned()
        }
        RegressionCauseHypothesisCode::KnownEnvironmentBlocker => {
            "A known environment or RCH blocker can explain the failing gate independently of source changes.".to_owned()
        }
        RegressionCauseHypothesisCode::OutputBudgetRegression => {
            "The evidence points at output or prompt-budget growth rather than semantic failure.".to_owned()
        }
        RegressionCauseHypothesisCode::FixtureGap => {
            "The failure-mode fixture catalog appears incomplete for this regression shape.".to_owned()
        }
        RegressionCauseHypothesisCode::PackSelectionChange => {
            "Pack replay or diff evidence indicates that context selection changed.".to_owned()
        }
        RegressionCauseHypothesisCode::PerfBudgetRegression => {
            "Performance evidence indicates a latency or resource-budget regression.".to_owned()
        }
        RegressionCauseHypothesisCode::TrackerStateMismatch => {
            "Tracker or BV evidence disagrees with the expected owner/dependency state.".to_owned()
        }
        RegressionCauseHypothesisCode::UnknownInsufficientEvidence => {
            "The normalized evidence is insufficient for a stronger deterministic hypothesis.".to_owned()
        }
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
    fn ranks_source_materialization_above_environment_and_perf_signals() {
        let inputs = vec![
            RegressionEvidenceInput::new(
                "bd-ppbue.18:rch",
                RegressionEvidenceKind::RchSelectorAdmission,
                json!({
                    "schema": "ee.rch.verify.v1",
                    "status": "rch_environment_failure",
                    "selector_admission_probe": {
                        "status": "selection_failed",
                        "local_fallback_refused": true
                    },
                    "source_materialization": "remote_checkout_unverified",
                    "remote_source_materialized": false,
                    "degradedCodes": ["rch_worker_topology_blocked"],
                    "redactionStatus": "safe"
                }),
            ),
            RegressionEvidenceInput::new(
                "perf:latency",
                RegressionEvidenceKind::PerfReport,
                json!({
                    "schema": "ee.perf.v1",
                    "status": "budget_exceeded",
                    "degradedCodes": ["perf_latency_budget_exceeded"],
                    "redactionStatus": "safe"
                }),
            ),
            RegressionEvidenceInput::new(
                "git:tree",
                RegressionEvidenceKind::GitMetadata,
                json!({
                    "status": "passed",
                    "sourceMaterialization": "committed_tree",
                    "remoteSourceMaterialized": true,
                    "redactionStatus": "safe"
                }),
            ),
        ];
        let normalized = normalize_regression_evidence_inputs(&inputs);
        let ranking = rank_regression_cause_hypotheses(&normalized.rows);
        let top = ranking.hypotheses.first().expect("top hypothesis");

        assert_eq!(
            top.code,
            RegressionCauseHypothesisCode::SourceNotMaterialized
        );
        assert_eq!(top.rank, 1);
        assert!((top.confidence - 0.84).abs() < f64::EPSILON);
        assert_eq!(top.severity, RegressionCausalitySeverity::High);
        assert!(top.evidence_refs.contains(&"bd-ppbue.18:rch".to_owned()));
        assert!(
            top.counter_evidence
                .iter()
                .any(|entry| entry.source_id.as_deref() == Some("git:tree")
                    && entry.effect == RegressionCounterEvidenceEffect::Weakens)
        );
        assert!(!top.authoritative);
        assert!(top.next_commands.iter().any(|command| command.requires_rch));
    }

    #[test]
    fn ranking_abstains_when_only_weak_evidence_is_available() {
        let inputs = vec![RegressionEvidenceInput::new(
            "git:clean",
            RegressionEvidenceKind::GitMetadata,
            json!({
                "status": "passed",
                "sourceMaterialization": "committed_tree",
                "remoteSourceMaterialized": true,
                "redactionStatus": "safe"
            }),
        )];
        let normalized = normalize_regression_evidence_inputs(&inputs);
        let ranking = rank_regression_cause_hypotheses(&normalized.rows);
        let top = ranking.hypotheses.first().expect("top hypothesis");

        assert_eq!(
            top.code,
            RegressionCauseHypothesisCode::UnknownInsufficientEvidence
        );
        assert_eq!(top.rank, 1);
        assert!(
            top.counter_evidence
                .iter()
                .any(|entry| entry.effect
                    == RegressionCounterEvidenceEffect::MissingRequiredSource)
        );
        assert!(
            ranking
                .degraded
                .iter()
                .any(|entry| entry.code == "regression_hypothesis_missing_required_source")
        );
    }

    #[test]
    fn every_hypothesis_code_has_a_direct_trigger_or_abstention_path() {
        let inputs = vec![
            RegressionEvidenceInput::new(
                "bd-ppbue.18:rch",
                RegressionEvidenceKind::RchSelectorAdmission,
                json!({
                    "schema": "ee.rch.verify.v1",
                    "status": "rch_environment_failure",
                    "selector_admission_probe": {
                        "status": "selection_failed",
                        "local_fallback_refused": true
                    },
                    "source_materialization": "remote_checkout_unverified",
                    "remote_source_materialized": false,
                    "degradedCodes": ["rch_worker_topology_blocked"],
                    "redactionStatus": "safe"
                }),
            ),
            RegressionEvidenceInput::new(
                "schema:contract",
                RegressionEvidenceKind::VerificationEvidence,
                json!({
                    "status": "passed",
                    "artifactHash": "blake3:schema-missing",
                    "redactionStatus": "safe"
                }),
            ),
            RegressionEvidenceInput::new(
                "pack:replay",
                RegressionEvidenceKind::PackReplay,
                json!({
                    "schema": "ee.pack_replay.v1",
                    "stale": true,
                    "artifactHash": "blake3:pack-stale",
                    "redactionStatus": "safe"
                }),
            ),
            RegressionEvidenceInput::new(
                "perf:latency",
                RegressionEvidenceKind::PerfReport,
                json!({
                    "schema": "ee.perf.v1",
                    "status": "budget_exceeded",
                    "degradedCodes": ["perf_latency_budget_exceeded"],
                    "redactionStatus": "safe"
                }),
            ),
            RegressionEvidenceInput::new(
                "support:prompt-budget",
                RegressionEvidenceKind::SupportBundle,
                json!({
                    "schema": "ee.support_bundle.v1",
                    "status": "passed",
                    "artifactHash": "blake3:support",
                    "degradedCodes": ["prompt_budget_exceeded"],
                    "redactionStatus": "safe"
                }),
            ),
            RegressionEvidenceInput::new(
                "pack:diff",
                RegressionEvidenceKind::PackDiff,
                json!({
                    "schema": "ee.pack_diff.v1",
                    "status": "passed",
                    "degradedCodes": ["pack_selection_changed"],
                    "redactionStatus": "safe"
                }),
            ),
            RegressionEvidenceInput::new(
                "bd-391ze.3:tracker",
                RegressionEvidenceKind::BvHistory,
                json!({
                    "status": "available",
                    "artifactHash": "blake3:tracker",
                    "degradedCodes": ["bv_tracker_mismatch"],
                    "redactionStatus": "hash_only"
                }),
            ),
            RegressionEvidenceInput::new(
                "fixture:missing",
                RegressionEvidenceKind::DegradedFixture,
                None,
            ),
        ];

        let normalized = normalize_regression_evidence_inputs(&inputs);
        let ranking = rank_regression_cause_hypotheses(&normalized.rows);
        let actual_codes = ranking
            .hypotheses
            .iter()
            .map(|hypothesis| hypothesis.code)
            .collect::<BTreeSet<_>>();
        let expected_codes = RegressionCauseHypothesisCode::ALL
            .into_iter()
            .filter(|code| *code != RegressionCauseHypothesisCode::UnknownInsufficientEvidence)
            .collect::<BTreeSet<_>>();

        assert_eq!(actual_codes, expected_codes);
        assert!(ranking.hypotheses.iter().all(|hypothesis| {
            !hypothesis.authoritative && !hypothesis.next_commands.is_empty()
        }));

        let abstention_input = vec![RegressionEvidenceInput::new(
            "git:clean",
            RegressionEvidenceKind::GitMetadata,
            json!({
                "status": "passed",
                "sourceMaterialization": "committed_tree",
                "remoteSourceMaterialized": true,
                "redactionStatus": "safe"
            }),
        )];
        let abstention_rows = normalize_regression_evidence_inputs(&abstention_input);
        let abstention = rank_regression_cause_hypotheses(&abstention_rows.rows);

        assert_eq!(
            abstention
                .hypotheses
                .first()
                .map(|hypothesis| hypothesis.code),
            Some(RegressionCauseHypothesisCode::UnknownInsufficientEvidence)
        );
        assert!(abstention.hypotheses.iter().all(|hypothesis| {
            !hypothesis.authoritative && !hypothesis.next_commands.is_empty()
        }));
    }

    #[test]
    fn golden_ranked_hypotheses_are_stable() {
        let inputs = vec![
            RegressionEvidenceInput::new(
                "bd-ppbue.18:rch",
                RegressionEvidenceKind::RchSelectorAdmission,
                json!({
                    "schema": "ee.rch.verify.v1",
                    "status": "rch_environment_failure",
                    "selector_admission_probe": {
                        "status": "selection_failed",
                        "local_fallback_refused": true
                    },
                    "source_materialization": "remote_checkout_unverified",
                    "remote_source_materialized": false,
                    "degradedCodes": ["rch_worker_topology_blocked"],
                    "redactionStatus": "safe"
                }),
            ),
            RegressionEvidenceInput::new(
                "perf:latency",
                RegressionEvidenceKind::PerfReport,
                json!({
                    "schema": "ee.perf.v1",
                    "status": "budget_exceeded",
                    "degradedCodes": ["perf_latency_budget_exceeded"],
                    "redactionStatus": "safe"
                }),
            ),
            RegressionEvidenceInput::new(
                "pack:diff",
                RegressionEvidenceKind::PackDiff,
                json!({
                    "schema": "ee.pack_diff.v1",
                    "status": "passed",
                    "degradedCodes": ["pack_selection_changed"],
                    "redactionStatus": "safe"
                }),
            ),
            RegressionEvidenceInput::new(
                "bd-391ze.3:tracker",
                RegressionEvidenceKind::BeadsHistory,
                json!({
                    "status": "available",
                    "stale": true,
                    "artifactHash": "blake3:tracker",
                    "redactionStatus": "hash_only"
                }),
            ),
            RegressionEvidenceInput::new(
                "fixture:missing",
                RegressionEvidenceKind::DegradedFixture,
                None,
            ),
            RegressionEvidenceInput::new(
                "git:tree",
                RegressionEvidenceKind::GitMetadata,
                json!({
                    "status": "passed",
                    "sourceMaterialization": "committed_tree",
                    "remoteSourceMaterialized": true,
                    "redactionStatus": "safe"
                }),
            ),
        ];

        let normalized = normalize_regression_evidence_inputs(&inputs);
        let ranking = rank_regression_cause_hypotheses(&normalized.rows);
        let actual = serde_json::to_string_pretty(&ranking)
            .expect("serialize regression hypothesis ranking");
        let expected =
            include_str!("../../tests/fixtures/golden/regression_causality/ranked_hypotheses.json")
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
