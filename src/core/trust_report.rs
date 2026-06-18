use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};

use serde_json::json;

use crate::db::{DbConnection, DbError, StoredFeedbackEvent, StoredMemory};
use crate::models::TRUST_REPORT_SCHEMA_V1;

const DEFAULT_BUCKET_COUNT: usize = 5;
const DEFAULT_LEADERBOARD_LIMIT: usize = 10;
const DEFAULT_PACK_ITEM_LIMIT: u32 = 10_000;
const HELPFUL_SIGNALS: &[&str] = &["positive", "confirmation", "helpful"];
const HARMFUL_SIGNALS: &[&str] = &["negative", "contradiction", "harmful", "inaccurate"];

/// Options for the read-only trust calibration report.
#[derive(Clone, Debug)]
pub struct TrustReportOptions {
    pub workspace_path: PathBuf,
    pub database_path: Option<PathBuf>,
    pub bucket_count: usize,
    pub leaderboard_limit: usize,
    pub pack_item_limit: u32,
}

impl TrustReportOptions {
    #[must_use]
    pub fn new(workspace_path: PathBuf) -> Self {
        Self {
            workspace_path,
            database_path: None,
            bucket_count: DEFAULT_BUCKET_COUNT,
            leaderboard_limit: DEFAULT_LEADERBOARD_LIMIT,
            pack_item_limit: DEFAULT_PACK_ITEM_LIMIT,
        }
    }
}

/// Error returned by the trust report use case.
#[derive(Debug)]
pub enum TrustReportError {
    Storage(DbError),
}

impl fmt::Display for TrustReportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Storage(error) => write!(formatter, "{error}"),
        }
    }
}

impl std::error::Error for TrustReportError {}

impl From<DbError> for TrustReportError {
    fn from(error: DbError) -> Self {
        Self::Storage(error)
    }
}

/// Calibration and reliability report for recorded outcome feedback.
#[derive(Clone, Debug, PartialEq)]
pub struct TrustReport {
    pub schema: &'static str,
    pub workspace_id: String,
    pub memory_count: usize,
    pub memory_with_outcome_count: usize,
    pub outcome_event_count: usize,
    pub helpful_event_count: usize,
    pub harmful_event_count: usize,
    pub packed_memory_count: usize,
    pub packed_memory_with_outcome_count: usize,
    pub bucket_count: usize,
    pub expected_calibration_error: f64,
    pub buckets: Vec<CalibrationBucket>,
    pub most_helpful: Vec<ReliabilityLeader>,
    pub most_harmful: Vec<ReliabilityLeader>,
    pub recommendations: Vec<TrustRecommendation>,
}

impl TrustReport {
    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        json!({
            "schema": self.schema,
            "workspaceId": self.workspace_id,
            "memoryCount": self.memory_count,
            "outcomeEvents": {
                "total": self.outcome_event_count,
                "helpful": self.helpful_event_count,
                "harmful": self.harmful_event_count
            },
            "outcomeCoverage": {
                "memoryCount": self.memory_count,
                "withOutcomeCount": self.memory_with_outcome_count,
                "ratio": stable_ratio(self.memory_with_outcome_count, self.memory_count),
                "packedMemoryCount": self.packed_memory_count,
                "packedWithOutcomeCount": self.packed_memory_with_outcome_count,
                "packedRatio": stable_ratio(
                    self.packed_memory_with_outcome_count,
                    self.packed_memory_count
                )
            },
            "calibration": {
                "bucketCount": self.bucket_count,
                "expectedCalibrationError": round6(self.expected_calibration_error),
                "buckets": self
                    .buckets
                    .iter()
                    .map(CalibrationBucket::data_json)
                    .collect::<Vec<_>>()
            },
            "reliability": {
                "mostHelpful": self
                    .most_helpful
                    .iter()
                    .map(ReliabilityLeader::data_json)
                    .collect::<Vec<_>>(),
                "mostHarmful": self
                    .most_harmful
                    .iter()
                    .map(ReliabilityLeader::data_json)
                    .collect::<Vec<_>>()
            },
            "recommendations": self
                .recommendations
                .iter()
                .map(TrustRecommendation::data_json)
                .collect::<Vec<_>>()
        })
    }

    #[must_use]
    pub fn human_summary(&self) -> String {
        let coverage = stable_ratio(self.memory_with_outcome_count, self.memory_count) * 100.0;
        let packed_coverage =
            stable_ratio(self.packed_memory_with_outcome_count, self.packed_memory_count) * 100.0;
        format!(
            "Trust report: ECE {:.3}; memory outcome coverage {:.1}% ({}/{}); packed-memory outcome coverage {:.1}% ({}/{}); {} recommendation(s).",
            self.expected_calibration_error,
            coverage,
            self.memory_with_outcome_count,
            self.memory_count,
            packed_coverage,
            self.packed_memory_with_outcome_count,
            self.packed_memory_count,
            self.recommendations.len()
        )
    }
}

/// One confidence bucket in the calibration curve.
#[derive(Clone, Debug, PartialEq)]
pub struct CalibrationBucket {
    pub index: usize,
    pub lower_bound: f64,
    pub upper_bound: f64,
    pub memory_count: usize,
    pub outcome_memory_count: usize,
    pub helpful_weight: f64,
    pub harmful_weight: f64,
    pub predicted_helpfulness: Option<f64>,
    pub observed_helpfulness: Option<f64>,
    pub calibration_error: Option<f64>,
    pub posture: CalibrationPosture,
}

impl CalibrationBucket {
    #[must_use]
    fn data_json(&self) -> serde_json::Value {
        json!({
            "index": self.index,
            "range": {
                "minInclusive": round6(self.lower_bound),
                "maxExclusive": round6(self.upper_bound)
            },
            "memoryCount": self.memory_count,
            "outcomeMemoryCount": self.outcome_memory_count,
            "helpfulWeight": round6(self.helpful_weight),
            "harmfulWeight": round6(self.harmful_weight),
            "predictedHelpfulness": self.predicted_helpfulness.map(round6),
            "observedHelpfulness": self.observed_helpfulness.map(round6),
            "calibrationError": self.calibration_error.map(round6),
            "posture": self.posture.as_str()
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CalibrationPosture {
    NoOutcomeSignal,
    Calibrated,
    OverConfident,
    UnderConfident,
}

impl CalibrationPosture {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NoOutcomeSignal => "no_outcome_signal",
            Self::Calibrated => "calibrated",
            Self::OverConfident => "over_confident",
            Self::UnderConfident => "under_confident",
        }
    }
}

/// Reliability row for memories with recorded outcome feedback.
#[derive(Clone, Debug, PartialEq)]
pub struct ReliabilityLeader {
    pub memory_id: String,
    pub level: String,
    pub kind: String,
    pub confidence: f64,
    pub utility: f64,
    pub helpful_weight: f64,
    pub harmful_weight: f64,
    pub net_weight: f64,
    pub event_count: usize,
    pub content_preview: String,
    pub candidate_action: ReliabilityCandidateAction,
}

impl ReliabilityLeader {
    #[must_use]
    fn data_json(&self) -> serde_json::Value {
        json!({
            "memoryId": self.memory_id,
            "level": self.level,
            "kind": self.kind,
            "confidence": round6(self.confidence),
            "utility": round6(self.utility),
            "helpfulWeight": round6(self.helpful_weight),
            "harmfulWeight": round6(self.harmful_weight),
            "netWeight": round6(self.net_weight),
            "eventCount": self.event_count,
            "contentPreview": self.content_preview,
            "candidateAction": self.candidate_action.as_str()
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReliabilityCandidateAction {
    PromoteCandidate,
    QuarantineCandidate,
    Observe,
}

impl ReliabilityCandidateAction {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PromoteCandidate => "promote_candidate",
            Self::QuarantineCandidate => "quarantine_candidate",
            Self::Observe => "observe",
        }
    }
}

/// Non-mutating action proposal derived from the report.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TrustRecommendation {
    pub code: &'static str,
    pub severity: &'static str,
    pub summary: String,
    pub command: Option<String>,
}

impl TrustRecommendation {
    #[must_use]
    fn data_json(&self) -> serde_json::Value {
        json!({
            "code": self.code,
            "severity": self.severity,
            "summary": self.summary,
            "command": self.command
        })
    }
}

/// Generate a read-only trust report from the workspace database.
pub fn generate_trust_report(options: TrustReportOptions) -> Result<TrustReport, TrustReportError> {
    let workspace_root = default_workspace_root(&options.workspace_path);
    let database_path = options
        .database_path
        .unwrap_or_else(|| default_workspace_database_path(&workspace_root));
    let connection = DbConnection::open_file(&database_path)?;
    let workspace_id = resolve_workspace_id(&connection, &workspace_root)?;
    let memories = connection.list_memories(&workspace_id, None, false)?;
    let feedback_events = connection.list_feedback_events(&workspace_id)?;
    let packed_memory_ids = connection
        .list_recent_pack_items_for_workspace(&workspace_id, options.pack_item_limit)?
        .into_iter()
        .map(|(_, item)| item.memory_id)
        .collect::<BTreeSet<_>>();

    Ok(build_trust_report(
        workspace_id,
        memories
            .iter()
            .map(MemoryTrustInput::from)
            .collect::<Vec<_>>(),
        feedback_events
            .iter()
            .map(FeedbackTrustInput::from)
            .collect::<Vec<_>>(),
        packed_memory_ids,
        options.bucket_count,
        options.leaderboard_limit,
    ))
}

fn build_trust_report(
    workspace_id: String,
    memories: Vec<MemoryTrustInput>,
    feedback_events: Vec<FeedbackTrustInput>,
    packed_memory_ids: BTreeSet<String>,
    bucket_count: usize,
    leaderboard_limit: usize,
) -> TrustReport {
    let bucket_count = bucket_count.max(1);
    let leaderboard_limit = leaderboard_limit.max(1);
    let memory_ids = memories
        .iter()
        .map(|memory| memory.id.clone())
        .collect::<BTreeSet<_>>();
    let mut outcome_by_memory = BTreeMap::<String, OutcomeAggregate>::new();
    let mut helpful_event_count = 0_usize;
    let mut harmful_event_count = 0_usize;

    for event in feedback_events {
        if event.target_type != "memory" || !memory_ids.contains(&event.target_id) {
            continue;
        }
        let weight = f64::from(event.weight).max(0.0);
        if HELPFUL_SIGNALS.contains(&event.signal.as_str()) {
            helpful_event_count += 1;
            outcome_by_memory
                .entry(event.target_id)
                .or_default()
                .record_helpful(weight);
        } else if HARMFUL_SIGNALS.contains(&event.signal.as_str()) {
            harmful_event_count += 1;
            outcome_by_memory
                .entry(event.target_id)
                .or_default()
                .record_harmful(weight);
        }
    }

    let mut bucket_builders = (0..bucket_count)
        .map(|index| BucketBuilder::new(index, bucket_count))
        .collect::<Vec<_>>();
    let mut reliability_rows = Vec::new();

    for memory in &memories {
        let confidence = clamp_unit(memory.confidence);
        let bucket_index = bucket_index(confidence, bucket_count);
        let outcome = outcome_by_memory.get(&memory.id);
        bucket_builders[bucket_index].memory_count += 1;
        if let Some(outcome) = outcome {
            bucket_builders[bucket_index].record_outcome(confidence, outcome);
            reliability_rows.push(ReliabilityLeader {
                memory_id: memory.id.clone(),
                level: memory.level.clone(),
                kind: memory.kind.clone(),
                confidence,
                utility: clamp_unit(memory.utility),
                helpful_weight: outcome.helpful_weight,
                harmful_weight: outcome.harmful_weight,
                net_weight: outcome.net_weight(),
                event_count: outcome.event_count,
                content_preview: content_preview(&memory.content),
                candidate_action: candidate_action(outcome),
            });
        }
    }

    let buckets = bucket_builders
        .into_iter()
        .map(BucketBuilder::finish)
        .collect::<Vec<_>>();
    let outcome_memory_count = outcome_by_memory.len();
    let expected_calibration_error = expected_calibration_error(&buckets, outcome_memory_count);

    let mut most_helpful = reliability_rows.clone();
    most_helpful.sort_by(compare_most_helpful);
    most_helpful.truncate(leaderboard_limit);

    let mut most_harmful = reliability_rows;
    most_harmful.sort_by(compare_most_harmful);
    most_harmful.truncate(leaderboard_limit);

    let packed_memory_count = packed_memory_ids.len();
    let packed_memory_with_outcome_count = packed_memory_ids
        .iter()
        .filter(|memory_id| outcome_by_memory.contains_key(*memory_id))
        .count();
    let outcome_event_count = helpful_event_count + harmful_event_count;
    let recommendations = recommendations(
        memories.len(),
        outcome_memory_count,
        packed_memory_count,
        packed_memory_with_outcome_count,
        expected_calibration_error,
        &most_harmful,
    );

    TrustReport {
        schema: TRUST_REPORT_SCHEMA_V1,
        workspace_id,
        memory_count: memories.len(),
        memory_with_outcome_count: outcome_memory_count,
        outcome_event_count,
        helpful_event_count,
        harmful_event_count,
        packed_memory_count,
        packed_memory_with_outcome_count,
        bucket_count,
        expected_calibration_error,
        buckets,
        most_helpful,
        most_harmful,
        recommendations,
    }
}

#[derive(Clone, Debug)]
struct MemoryTrustInput {
    id: String,
    level: String,
    kind: String,
    content: String,
    confidence: f32,
    utility: f32,
}

impl From<&StoredMemory> for MemoryTrustInput {
    fn from(memory: &StoredMemory) -> Self {
        Self {
            id: memory.id.clone(),
            level: memory.level.clone(),
            kind: memory.kind.clone(),
            content: memory.content.clone(),
            confidence: memory.confidence,
            utility: memory.utility,
        }
    }
}

#[derive(Clone, Debug)]
struct FeedbackTrustInput {
    target_type: String,
    target_id: String,
    signal: String,
    weight: f32,
}

impl From<&StoredFeedbackEvent> for FeedbackTrustInput {
    fn from(event: &StoredFeedbackEvent) -> Self {
        Self {
            target_type: event.target_type.clone(),
            target_id: event.target_id.clone(),
            signal: event.signal.clone(),
            weight: event.weight,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct OutcomeAggregate {
    helpful_weight: f64,
    harmful_weight: f64,
    event_count: usize,
}

impl OutcomeAggregate {
    fn record_helpful(&mut self, weight: f64) {
        self.helpful_weight += weight;
        self.event_count += 1;
    }

    fn record_harmful(&mut self, weight: f64) {
        self.harmful_weight += weight;
        self.event_count += 1;
    }

    fn net_weight(self) -> f64 {
        self.helpful_weight - self.harmful_weight
    }

}

#[derive(Clone, Debug)]
struct BucketBuilder {
    index: usize,
    lower_bound: f64,
    upper_bound: f64,
    memory_count: usize,
    outcome_memory_count: usize,
    predicted_sum: f64,
    helpful_weight: f64,
    harmful_weight: f64,
}

impl BucketBuilder {
    fn new(index: usize, bucket_count: usize) -> Self {
        let width = 1.0 / bucket_count as f64;
        Self {
            index,
            lower_bound: index as f64 * width,
            upper_bound: if index + 1 == bucket_count {
                1.0
            } else {
                (index + 1) as f64 * width
            },
            memory_count: 0,
            outcome_memory_count: 0,
            predicted_sum: 0.0,
            helpful_weight: 0.0,
            harmful_weight: 0.0,
        }
    }

    fn record_outcome(&mut self, confidence: f64, outcome: &OutcomeAggregate) {
        self.outcome_memory_count += 1;
        self.predicted_sum += confidence;
        self.helpful_weight += outcome.helpful_weight;
        self.harmful_weight += outcome.harmful_weight;
    }

    fn finish(self) -> CalibrationBucket {
        let predicted_helpfulness = if self.outcome_memory_count == 0 {
            None
        } else {
            Some(self.predicted_sum / self.outcome_memory_count as f64)
        };
        let total_outcome_weight = self.helpful_weight + self.harmful_weight;
        let observed_helpfulness = if total_outcome_weight <= f64::EPSILON {
            None
        } else {
            Some(self.helpful_weight / total_outcome_weight)
        };
        let calibration_error = predicted_helpfulness
            .zip(observed_helpfulness)
            .map(|(predicted, observed)| (predicted - observed).abs());
        let posture = match predicted_helpfulness.zip(observed_helpfulness) {
            None => CalibrationPosture::NoOutcomeSignal,
            Some((predicted, observed)) if (predicted - observed).abs() <= 0.05 => {
                CalibrationPosture::Calibrated
            }
            Some((predicted, observed)) if predicted > observed => CalibrationPosture::OverConfident,
            Some(_) => CalibrationPosture::UnderConfident,
        };

        CalibrationBucket {
            index: self.index,
            lower_bound: self.lower_bound,
            upper_bound: self.upper_bound,
            memory_count: self.memory_count,
            outcome_memory_count: self.outcome_memory_count,
            helpful_weight: self.helpful_weight,
            harmful_weight: self.harmful_weight,
            predicted_helpfulness,
            observed_helpfulness,
            calibration_error,
            posture,
        }
    }
}

fn expected_calibration_error(buckets: &[CalibrationBucket], outcome_memory_count: usize) -> f64 {
    if outcome_memory_count == 0 {
        return 0.0;
    }
    buckets
        .iter()
        .filter_map(|bucket| {
            bucket.calibration_error.map(|error| {
                error * bucket.outcome_memory_count as f64 / outcome_memory_count as f64
            })
        })
        .sum()
}

fn compare_most_helpful(left: &ReliabilityLeader, right: &ReliabilityLeader) -> Ordering {
    compare_f64_desc(left.net_weight, right.net_weight)
        .then_with(|| compare_f64_desc(left.helpful_weight, right.helpful_weight))
        .then_with(|| left.memory_id.cmp(&right.memory_id))
}

fn compare_most_harmful(left: &ReliabilityLeader, right: &ReliabilityLeader) -> Ordering {
    compare_f64_asc(left.net_weight, right.net_weight)
        .then_with(|| compare_f64_desc(left.harmful_weight, right.harmful_weight))
        .then_with(|| left.memory_id.cmp(&right.memory_id))
}

fn compare_f64_desc(left: f64, right: f64) -> Ordering {
    right.partial_cmp(&left).unwrap_or(Ordering::Equal)
}

fn compare_f64_asc(left: f64, right: f64) -> Ordering {
    left.partial_cmp(&right).unwrap_or(Ordering::Equal)
}

fn candidate_action(outcome: &OutcomeAggregate) -> ReliabilityCandidateAction {
    match outcome.net_weight() {
        net if net >= 2.0 => ReliabilityCandidateAction::PromoteCandidate,
        net if net <= -1.0 => ReliabilityCandidateAction::QuarantineCandidate,
        _ => ReliabilityCandidateAction::Observe,
    }
}

fn recommendations(
    memory_count: usize,
    outcome_memory_count: usize,
    packed_memory_count: usize,
    packed_memory_with_outcome_count: usize,
    expected_calibration_error: f64,
    most_harmful: &[ReliabilityLeader],
) -> Vec<TrustRecommendation> {
    let mut recommendations = Vec::new();
    if memory_count > 0 && stable_ratio(outcome_memory_count, memory_count) < 0.2 {
        recommendations.push(TrustRecommendation {
            code: "increase_outcome_coverage",
            severity: "medium",
            summary: "Outcome coverage is low; wire ambient capture and explicit outcome hooks for packed memories.".to_string(),
            command: Some("ee outcome helpful|harmful <memory-id> --json".to_string()),
        });
    }
    if packed_memory_count > 0 && stable_ratio(packed_memory_with_outcome_count, packed_memory_count) < 0.5 {
        recommendations.push(TrustRecommendation {
            code: "close_packed_memory_feedback_loop",
            severity: "medium",
            summary: "Packed memories are not receiving enough outcome signal after use.".to_string(),
            command: Some("ee pack <task> --json && ee outcome helpful|harmful <memory-id> --json".to_string()),
        });
    }
    if expected_calibration_error > 0.2 {
        recommendations.push(TrustRecommendation {
            code: "review_confidence_calibration",
            severity: "warning",
            summary: "Observed helpfulness diverges from stored confidence; review confidence/decay policy before trusting rank scores.".to_string(),
            command: None,
        });
    }
    if most_harmful
        .first()
        .is_some_and(|leader| leader.net_weight <= -1.0)
    {
        recommendations.push(TrustRecommendation {
            code: "review_harmful_memory_candidates",
            severity: "high",
            summary: "One or more memories have net harmful outcomes; review quarantine candidates explicitly.".to_string(),
            command: Some("ee outcome quarantine list --json".to_string()),
        });
    }
    recommendations
}

fn bucket_index(confidence: f64, bucket_count: usize) -> usize {
    let bucket_count = bucket_count.max(1);
    let raw = (confidence * bucket_count as f64).floor() as usize;
    raw.min(bucket_count - 1)
}

fn stable_ratio(numerator: usize, denominator: usize) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        round6(numerator as f64 / denominator as f64)
    }
}

fn clamp_unit(value: f32) -> f64 {
    let value = f64::from(value);
    if value.is_finite() {
        value.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

fn round6(value: f64) -> f64 {
    if value.is_finite() {
        (value * 1_000_000.0).round() / 1_000_000.0
    } else {
        0.0
    }
}

fn content_preview(content: &str) -> String {
    const MAX_CHARS: usize = 160;
    let collapsed = content.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= MAX_CHARS {
        return collapsed;
    }
    collapsed.chars().take(MAX_CHARS).collect()
}

fn default_workspace_root(workspace_path: &Path) -> PathBuf {
    crate::config::workspace::canonical_workspace_root_or_lexical(workspace_path)
}

fn default_workspace_database_path(workspace_path: &Path) -> PathBuf {
    default_workspace_root(workspace_path)
        .join(".ee")
        .join("ee.db")
}

fn resolve_workspace_id(
    connection: &DbConnection,
    workspace_path: &Path,
) -> Result<String, DbError> {
    let path = workspace_path.to_string_lossy();
    Ok(connection
        .get_workspace_by_path(&path)?
        .map(|workspace| workspace.id)
        .unwrap_or_else(|| crate::core::curate::stable_workspace_id(workspace_path)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn memory(id: &str, confidence: f32, utility: f32, content: &str) -> MemoryTrustInput {
        MemoryTrustInput {
            id: id.to_string(),
            level: "procedural".to_string(),
            kind: "rule".to_string(),
            content: content.to_string(),
            confidence,
            utility,
        }
    }

    fn feedback(target_id: &str, signal: &str, weight: f32) -> FeedbackTrustInput {
        FeedbackTrustInput {
            target_type: "memory".to_string(),
            target_id: target_id.to_string(),
            signal: signal.to_string(),
            weight,
        }
    }

    #[test]
    fn calibration_curve_and_leaderboards_are_deterministic() {
        let memories = vec![
            memory("mem_a", 0.80, 0.70, "helpful rule"),
            memory("mem_b", 0.20, 0.30, "harmful rule"),
            memory("mem_c", 0.90, 0.50, "mixed rule"),
        ];
        let feedback_events = vec![
            feedback("mem_a", "helpful", 2.0),
            feedback("mem_b", "harmful", 1.0),
            feedback("mem_c", "positive", 1.0),
            feedback("mem_c", "negative", 1.0),
        ];
        let report = build_trust_report(
            "wsp_test".to_string(),
            memories,
            feedback_events,
            BTreeSet::from(["mem_a".to_string(), "mem_c".to_string()]),
            5,
            10,
        );

        assert_eq!(report.memory_count, 3);
        assert_eq!(report.memory_with_outcome_count, 3);
        assert_eq!(report.helpful_event_count, 2);
        assert_eq!(report.harmful_event_count, 2);
        assert_eq!(report.packed_memory_with_outcome_count, 2);
        assert_eq!(round6(report.expected_calibration_error), 0.233333);
        assert_eq!(report.buckets[1].posture, CalibrationPosture::UnderConfident);
        assert_eq!(report.buckets[4].posture, CalibrationPosture::OverConfident);
        assert_eq!(report.most_helpful[0].memory_id, "mem_a");
        assert_eq!(
            report.most_helpful[0].candidate_action,
            ReliabilityCandidateAction::PromoteCandidate
        );
        assert_eq!(report.most_harmful[0].memory_id, "mem_b");
        assert_eq!(
            report.most_harmful[0].candidate_action,
            ReliabilityCandidateAction::QuarantineCandidate
        );
        assert!(report
            .recommendations
            .iter()
            .any(|recommendation| recommendation.code == "review_confidence_calibration"));
    }

    #[test]
    fn low_feedback_coverage_recommends_closing_the_loop_without_mutation() {
        let report = build_trust_report(
            "wsp_test".to_string(),
            vec![
                memory("mem_a", 0.80, 0.70, "rule one"),
                memory("mem_b", 0.20, 0.30, "rule two"),
                memory("mem_c", 0.60, 0.50, "rule three"),
                memory("mem_d", 0.40, 0.40, "rule four"),
                memory("mem_e", 0.50, 0.50, "rule five"),
            ],
            vec![feedback("mem_a", "helpful", 1.0)],
            BTreeSet::from([
                "mem_a".to_string(),
                "mem_b".to_string(),
                "mem_c".to_string(),
            ]),
            5,
            10,
        );

        assert_eq!(report.memory_with_outcome_count, 1);
        assert_eq!(report.packed_memory_with_outcome_count, 1);
        assert!(report
            .recommendations
            .iter()
            .any(|recommendation| recommendation.code == "close_packed_memory_feedback_loop"));
        assert!(!report
            .recommendations
            .iter()
            .any(|recommendation| recommendation.summary.contains("auto")));
    }

    #[test]
    fn empty_report_coerces_boundaries_without_false_recommendations() {
        let report = build_trust_report(
            "wsp_test".to_string(),
            Vec::new(),
            vec![feedback("missing_memory", "helpful", 1.0)],
            BTreeSet::new(),
            0,
            0,
        );

        assert_eq!(report.memory_count, 0);
        assert_eq!(report.memory_with_outcome_count, 0);
        assert_eq!(report.outcome_event_count, 0);
        assert_eq!(report.bucket_count, 1, "bucket count has a safe lower bound");
        assert_eq!(report.buckets.len(), 1);
        assert_eq!(report.buckets[0].posture, CalibrationPosture::NoOutcomeSignal);
        assert_eq!(report.expected_calibration_error, 0.0);
        assert!(report.most_helpful.is_empty());
        assert!(report.most_harmful.is_empty());
        assert!(
            report.recommendations.is_empty(),
            "an empty workspace should not invent trust actions"
        );
        assert_eq!(
            report.data_json()["outcomeCoverage"]["ratio"],
            serde_json::json!(0.0)
        );
    }
}
