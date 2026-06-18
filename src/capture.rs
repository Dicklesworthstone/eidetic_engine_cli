use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::core::curate::{ReviewSessionCandidate, ReviewSessionOptions, review_session_proposals};
use crate::db::{DbConnection, StoredCurationCandidate, StoredMemory};
use crate::models::DomainError;

pub const CAPTURE_SUGGESTIONS_SCHEMA_V1: &str = "ee.capture_suggestions.v1";
pub const DEFAULT_CAPTURE_SUGGEST_LIMIT: u32 = 2;
pub const DEFAULT_CAPTURE_SUGGEST_MIN_CONFIDENCE: f32 = 0.58;
const CAPTURE_DUPLICATE_SIMILARITY_FLOOR: f32 = 0.72;

#[derive(Clone, Debug)]
pub struct CaptureSuggestOptions<'a> {
    pub workspace_path: &'a Path,
    pub database_path: Option<&'a Path>,
    pub from_session: Option<&'a str>,
    pub from_recent: bool,
    pub max: u32,
    pub min_confidence: f32,
    pub include_suppressed: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureSuggestReport {
    pub schema: &'static str,
    pub command: &'static str,
    pub version: &'static str,
    pub workspace_id: String,
    pub workspace_path: String,
    pub database_path: String,
    pub source: CaptureSuggestSource,
    pub max: u32,
    pub min_confidence: f32,
    pub read_only: bool,
    pub durable_mutation: bool,
    pub evidence_span_count: usize,
    pub candidate_count: usize,
    pub suggestion_count: usize,
    pub suppressed_count: usize,
    pub suggestions: Vec<CaptureSuggestion>,
    pub suppressed: Vec<CaptureSuggestionSuppression>,
    pub decision_log: Vec<CaptureSuggestDecisionLog>,
    pub next_action: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureSuggestSource {
    pub mode: String,
    pub session_id: String,
    pub cass_session_id: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureSuggestion {
    pub suggestion_id: String,
    pub candidate_id: String,
    pub candidate_kind: String,
    pub topic_key: String,
    pub proposed_fields: CaptureProposedFields,
    pub evidence: Vec<CaptureEvidenceSpan>,
    pub confidence: f32,
    pub dedupe_status: CaptureDedupeStatus,
    pub reason: String,
    pub content_hash: String,
    pub accept_command: String,
    pub reject_command: String,
    pub propose_command: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureProposedFields {
    pub level: String,
    pub kind: String,
    pub tags: Vec<String>,
    pub content: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureEvidenceSpan {
    pub id: String,
    pub source_type: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureDedupeStatus {
    pub status: String,
    pub reason_code: String,
    pub matched_id: Option<String>,
    pub score: Option<f32>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureSuggestionSuppression {
    pub candidate_id: String,
    pub reason_code: String,
    pub matched_id: Option<String>,
    pub score: Option<f32>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureSuggestDecisionLog {
    pub event: String,
    pub candidate_id: String,
    pub decision: String,
    pub reason_code: String,
    pub score: Option<f32>,
}

#[derive(Clone, Debug, Default)]
struct SuppressionIndex {
    existing_memory_content: Vec<(String, String)>,
    queued_candidate_hashes: BTreeMap<String, String>,
    declined_candidate_hashes: BTreeMap<String, String>,
}

#[derive(Clone, Debug, PartialEq)]
enum CandidateDisposition {
    Suggest,
    Suppress {
        reason_code: &'static str,
        matched_id: Option<String>,
        score: Option<f32>,
    },
}

pub fn capture_suggest(
    options: &CaptureSuggestOptions<'_>,
) -> Result<CaptureSuggestReport, DomainError> {
    validate_capture_suggest_options(options)?;
    let review = review_session_proposals(&ReviewSessionOptions {
        workspace_path: options.workspace_path,
        database_path: options.database_path,
        session_id: options.from_session,
        propose: false,
        dry_run: true,
        min_confidence: options.min_confidence,
        limit: options.max.saturating_mul(4).max(options.max),
    })?;
    let suppression = load_suppression_index(&review)?;
    let propose_command = propose_command_for(&review, options);
    let mut suggestions = Vec::new();
    let mut suppressed = Vec::new();
    let mut decision_log = Vec::new();

    for candidate in &review.candidates {
        let disposition = classify_candidate(candidate, &suppression);
        match disposition {
            CandidateDisposition::Suggest if suggestions.len() < options.max as usize => {
                suggestions.push(capture_suggestion_from_candidate(
                    candidate,
                    &review.cass_session_id,
                    &propose_command,
                ));
                decision_log.push(CaptureSuggestDecisionLog {
                    event: "candidate_evaluated".to_owned(),
                    candidate_id: candidate.candidate_id.clone(),
                    decision: "suggest".to_owned(),
                    reason_code: "above_threshold_unique".to_owned(),
                    score: Some(candidate.confidence),
                });
            }
            CandidateDisposition::Suggest => {
                suppressed.push(CaptureSuggestionSuppression {
                    candidate_id: candidate.candidate_id.clone(),
                    reason_code: "max_suggestions_reached".to_owned(),
                    matched_id: None,
                    score: Some(candidate.confidence),
                });
                decision_log.push(CaptureSuggestDecisionLog {
                    event: "candidate_evaluated".to_owned(),
                    candidate_id: candidate.candidate_id.clone(),
                    decision: "suppress".to_owned(),
                    reason_code: "max_suggestions_reached".to_owned(),
                    score: Some(candidate.confidence),
                });
            }
            CandidateDisposition::Suppress {
                reason_code,
                matched_id,
                score,
            } => {
                suppressed.push(CaptureSuggestionSuppression {
                    candidate_id: candidate.candidate_id.clone(),
                    reason_code: reason_code.to_owned(),
                    matched_id: matched_id.clone(),
                    score,
                });
                decision_log.push(CaptureSuggestDecisionLog {
                    event: "candidate_evaluated".to_owned(),
                    candidate_id: candidate.candidate_id.clone(),
                    decision: "suppress".to_owned(),
                    reason_code: reason_code.to_owned(),
                    score,
                });
            }
        }
    }

    let suppressed_count = suppressed.len();
    if !options.include_suppressed {
        suppressed.clear();
    }
    let source_mode = if options.from_session.is_some() {
        "session"
    } else {
        "recent"
    };
    let next_action = if suggestions.is_empty() {
        "no capture suggestions above threshold; continue work or import richer CASS evidence"
            .to_owned()
    } else {
        "review suggested acceptCommand/rejectCommand entries; no memory is stored until a curation command is run".to_owned()
    };

    Ok(CaptureSuggestReport {
        schema: CAPTURE_SUGGESTIONS_SCHEMA_V1,
        command: "capture suggest",
        version: env!("CARGO_PKG_VERSION"),
        workspace_id: review.workspace_id,
        workspace_path: review.workspace_path,
        database_path: review.database_path,
        source: CaptureSuggestSource {
            mode: source_mode.to_owned(),
            session_id: review.session_id,
            cass_session_id: review.cass_session_id,
        },
        max: options.max,
        min_confidence: options.min_confidence,
        read_only: true,
        durable_mutation: false,
        evidence_span_count: review.evidence_span_count,
        candidate_count: review.candidate_count,
        suggestion_count: suggestions.len(),
        suppressed_count,
        suggestions,
        suppressed,
        decision_log,
        next_action,
    })
}

fn validate_capture_suggest_options(options: &CaptureSuggestOptions<'_>) -> Result<(), DomainError> {
    if options.from_recent && options.from_session.is_some() {
        return Err(DomainError::Usage {
            message: "`ee capture suggest` accepts either --from-recent or --from-session, not both."
                .to_owned(),
            repair: Some("Use `ee capture suggest --from-recent --json`.".to_owned()),
        });
    }
    if options.max == 0 {
        return Err(DomainError::Usage {
            message: "`ee capture suggest --max` must be greater than zero.".to_owned(),
            repair: Some("Use `ee capture suggest --max 2 --json`.".to_owned()),
        });
    }
    if !(0.0..=1.0).contains(&options.min_confidence) {
        return Err(DomainError::Usage {
            message: "`ee capture suggest --min-confidence` must be between 0.0 and 1.0."
                .to_owned(),
            repair: Some("Use `ee capture suggest --min-confidence 0.58 --json`.".to_owned()),
        });
    }
    Ok(())
}

fn load_suppression_index(review: &crate::core::curate::ReviewSessionReport) -> Result<SuppressionIndex, DomainError> {
    let database_path = PathBuf::from(&review.database_path);
    let connection = DbConnection::open_file(&database_path).map_err(|error| DomainError::Storage {
        message: format!("Failed to open database for capture dedupe suppression: {error}"),
        repair: Some("ee doctor --json".to_owned()),
    })?;
    let memories = connection
        .list_memories(&review.workspace_id, None, false)
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to list memories for capture dedupe suppression: {error}"),
            repair: Some("ee memory list --json".to_owned()),
        })?;
    let candidates = connection
        .list_curation_candidates(&review.workspace_id, None, None, None)
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to list curation candidates for capture suppression: {error}"),
            repair: Some("ee curate candidates --json".to_owned()),
        })?;
    Ok(suppression_index_from_rows(&memories, &candidates))
}

fn suppression_index_from_rows(
    memories: &[StoredMemory],
    candidates: &[StoredCurationCandidate],
) -> SuppressionIndex {
    let existing_memory_content = memories
        .iter()
        .map(|memory| (memory.id.clone(), memory.content.clone()))
        .collect();
    let mut queued_candidate_hashes = BTreeMap::new();
    let mut declined_candidate_hashes = BTreeMap::new();
    for candidate in candidates {
        let Some(content) = candidate.proposed_content.as_deref() else {
            continue;
        };
        let hash = content_hash(content);
        if candidate.status == "rejected" || candidate.review_state == "rejected" {
            declined_candidate_hashes.insert(hash, candidate.id.clone());
        } else if matches!(
            candidate.status.as_str(),
            "pending" | "approved" | "accepted" | "applied"
        ) {
            queued_candidate_hashes.insert(hash, candidate.id.clone());
        }
    }
    SuppressionIndex {
        existing_memory_content,
        queued_candidate_hashes,
        declined_candidate_hashes,
    }
}

fn classify_candidate(
    candidate: &ReviewSessionCandidate,
    suppression: &SuppressionIndex,
) -> CandidateDisposition {
    if let Some(candidate_id) = suppression
        .declined_candidate_hashes
        .get(&candidate.content_hash)
    {
        return CandidateDisposition::Suppress {
            reason_code: "declined_capture",
            matched_id: Some(candidate_id.clone()),
            score: Some(1.0),
        };
    }
    if let Some(candidate_id) = suppression.queued_candidate_hashes.get(&candidate.content_hash) {
        return CandidateDisposition::Suppress {
            reason_code: "already_queued",
            matched_id: Some(candidate_id.clone()),
            score: Some(1.0),
        };
    }
    let mut best: Option<(&str, f32)> = None;
    for (memory_id, content) in &suppression.existing_memory_content {
        let score = lexical_similarity(&candidate.proposed_content, content);
        if score >= CAPTURE_DUPLICATE_SIMILARITY_FLOOR
            && best.is_none_or(|(_, best_score)| score > best_score)
        {
            best = Some((memory_id.as_str(), score));
        }
    }
    if let Some((memory_id, score)) = best {
        return CandidateDisposition::Suppress {
            reason_code: "existing_memory_covers",
            matched_id: Some(memory_id.to_owned()),
            score: Some(score),
        };
    }
    CandidateDisposition::Suggest
}

fn capture_suggestion_from_candidate(
    candidate: &ReviewSessionCandidate,
    cass_session_id: &str,
    propose_command: &str,
) -> CaptureSuggestion {
    let suggestion_id = deterministic_capture_id(&[
        "capture_suggest",
        candidate.candidate_id.as_str(),
        candidate.content_hash.as_str(),
    ]);
    CaptureSuggestion {
        suggestion_id,
        candidate_id: candidate.candidate_id.clone(),
        candidate_kind: candidate.candidate_kind.clone(),
        topic_key: candidate.topic_key.clone(),
        proposed_fields: proposed_fields_for_candidate(candidate),
        evidence: candidate
            .source_ids
            .iter()
            .map(|id| CaptureEvidenceSpan {
                id: id.clone(),
                source_type: candidate.source_type.clone(),
            })
            .collect(),
        confidence: candidate.confidence,
        dedupe_status: CaptureDedupeStatus {
            status: "unique".to_owned(),
            reason_code: "above_threshold_unique".to_owned(),
            matched_id: None,
            score: Some(candidate.confidence),
        },
        reason: candidate.reason.clone(),
        content_hash: candidate.content_hash.clone(),
        propose_command: propose_command.to_owned(),
        accept_command: format!(
            "{propose_command} && ee curate accept {} --reason \"accepted capture suggestion from {cass_session_id}\" --json",
            candidate.candidate_id
        ),
        reject_command: format!(
            "{propose_command} && ee curate reject {} --reason \"declined ambient capture suggestion from {cass_session_id}\" --json",
            candidate.candidate_id
        ),
    }
}

fn proposed_fields_for_candidate(candidate: &ReviewSessionCandidate) -> CaptureProposedFields {
    let (level, kind) = match candidate.candidate_kind.as_str() {
        "failure" => ("episodic", "failure"),
        "decision" => ("semantic", "decision"),
        "rule" | "propose_new_memory" => ("procedural", "rule"),
        other if other.contains("failure") => ("episodic", "failure"),
        other if other.contains("decision") => ("semantic", "decision"),
        _ => ("procedural", "rule"),
    };
    let mut tags = BTreeSet::from([
        "ambient-capture".to_owned(),
        "capture-suggest".to_owned(),
        candidate.topic_key.clone(),
    ]);
    for token in normalized_tokens(&candidate.proposed_content) {
        if matches!(
            token.as_str(),
            "cargo" | "release" | "test" | "build" | "search" | "curate" | "hook"
        ) {
            tags.insert(token);
        }
    }
    CaptureProposedFields {
        level: level.to_owned(),
        kind: kind.to_owned(),
        tags: tags.into_iter().collect(),
        content: candidate.proposed_content.clone(),
    }
}

fn propose_command_for(
    review: &crate::core::curate::ReviewSessionReport,
    options: &CaptureSuggestOptions<'_>,
) -> String {
    let session = options.from_session.unwrap_or(&review.cass_session_id);
    format!(
        "ee review session {} --propose --limit {} --min-confidence {:.2} --json",
        shell_word(session),
        options.max.saturating_mul(4).max(options.max),
        options.min_confidence
    )
}

fn deterministic_capture_id(parts: &[&str]) -> String {
    let mut hasher = blake3::Hasher::new();
    for part in parts {
        hasher.update(part.as_bytes());
        hasher.update(b"\0");
    }
    let hash = hasher.finalize();
    format!("capture_{}", &hash.to_hex()[..26])
}

fn content_hash(content: &str) -> String {
    format!("blake3:{}", blake3::hash(content.as_bytes()).to_hex())
}

fn lexical_similarity(left: &str, right: &str) -> f32 {
    let left = normalized_tokens(left);
    let right = normalized_tokens(right);
    if left.is_empty() || right.is_empty() {
        return 0.0;
    }
    let intersection = left.intersection(&right).count() as f32;
    let union = left.union(&right).count() as f32;
    if union <= f32::EPSILON {
        0.0
    } else {
        intersection / union
    }
}

fn normalized_tokens(text: &str) -> BTreeSet<String> {
    let mut tokens = BTreeSet::new();
    let mut token = String::new();
    for ch in text.chars().flat_map(char::to_lowercase) {
        if ch.is_ascii_alphanumeric() {
            token.push(ch);
        } else {
            push_capture_token(&mut tokens, &mut token);
        }
    }
    push_capture_token(&mut tokens, &mut token);
    tokens
}

fn push_capture_token(tokens: &mut BTreeSet<String>, token: &mut String) {
    if token.len() >= 3 && !capture_stopword(token) {
        tokens.insert(std::mem::take(token));
    } else {
        token.clear();
    }
}

fn capture_stopword(token: &str) -> bool {
    matches!(
        token,
        "the" | "and" | "for" | "with" | "this" | "that" | "from" | "when" | "into" | "must"
            | "should" | "before" | "after" | "session" | "work"
    )
}

fn shell_word(value: &str) -> String {
    if value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b'/' | b':'))
    {
        value.to_owned()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(content: &str) -> ReviewSessionCandidate {
        ReviewSessionCandidate {
            candidate_id: "curate_capturetest000000000001".to_owned(),
            candidate_type: "create_derived_memory".to_owned(),
            candidate_kind: "rule".to_owned(),
            topic_key: "cargo".to_owned(),
            target_memory_id: None,
            proposed_content: content.to_owned(),
            proposed_confidence: 0.76,
            source_type: "cass_evidence".to_owned(),
            source_ids: vec!["span_1".to_owned()],
            reason: "test candidate".to_owned(),
            confidence: 0.76,
            content_hash: content_hash(content),
            persisted: false,
        }
    }

    #[test]
    fn capture_dedupe_suppresses_declined_candidate_by_hash() {
        let candidate = candidate("Run cargo fmt before release verification.");
        let suppression = SuppressionIndex {
            declined_candidate_hashes: BTreeMap::from([(
                candidate.content_hash.clone(),
                "curate_declined".to_owned(),
            )]),
            ..SuppressionIndex::default()
        };
        assert_eq!(
            classify_candidate(&candidate, &suppression),
            CandidateDisposition::Suppress {
                reason_code: "declined_capture",
                matched_id: Some("curate_declined".to_owned()),
                score: Some(1.0),
            }
        );
    }

    #[test]
    fn capture_dedupe_suppresses_existing_memory_by_lexical_overlap() {
        let candidate = candidate("Run cargo fmt before release verification.");
        let suppression = SuppressionIndex {
            existing_memory_content: vec![(
                "mem_existing".to_owned(),
                "Always run cargo fmt before release verification.".to_owned(),
            )],
            ..SuppressionIndex::default()
        };
        match classify_candidate(&candidate, &suppression) {
            CandidateDisposition::Suppress {
                reason_code,
                matched_id,
                score,
            } => {
                assert_eq!(reason_code, "existing_memory_covers");
                assert_eq!(matched_id.as_deref(), Some("mem_existing"));
                assert!(score.unwrap_or_default() >= CAPTURE_DUPLICATE_SIMILARITY_FLOOR);
            }
            CandidateDisposition::Suggest => panic!("expected lexical duplicate suppression"),
        }
    }

    #[test]
    fn capture_candidate_projection_is_stable_and_audited_by_command() {
        let candidate = candidate("Run cargo fmt before release verification.");
        let suggestion = capture_suggestion_from_candidate(
            &candidate,
            "cass-123",
            "ee review session cass-123 --propose --json",
        );
        assert!(suggestion.suggestion_id.starts_with("capture_"));
        assert_eq!(suggestion.proposed_fields.level, "procedural");
        assert_eq!(suggestion.proposed_fields.kind, "rule");
        assert!(
            suggestion
                .proposed_fields
                .tags
                .contains(&"ambient-capture".to_owned())
        );
        assert!(suggestion.accept_command.contains("ee curate accept"));
        assert!(suggestion.reject_command.contains("ee curate reject"));
    }

    #[test]
    fn capture_suggestions_report_matches_golden_fixture() {
        let report = CaptureSuggestReport {
            schema: CAPTURE_SUGGESTIONS_SCHEMA_V1,
            command: "capture suggest",
            version: "0.0.0-test",
            workspace_id: "wsp_demo".to_owned(),
            workspace_path: "/workspace/demo".to_owned(),
            database_path: "/workspace/demo/.ee/ee.db".to_owned(),
            source: CaptureSuggestSource {
                mode: "recent".to_owned(),
                session_id: "latest".to_owned(),
                cass_session_id: "cass_demo".to_owned(),
            },
            max: 2,
            min_confidence: DEFAULT_CAPTURE_SUGGEST_MIN_CONFIDENCE,
            read_only: true,
            durable_mutation: false,
            evidence_span_count: 1,
            candidate_count: 1,
            suggestion_count: 1,
            suppressed_count: 0,
            suggestions: vec![CaptureSuggestion {
                suggestion_id: "capture_demo".to_owned(),
                candidate_id: "curate_demo".to_owned(),
                candidate_kind: "rule".to_owned(),
                topic_key: "cargo".to_owned(),
                proposed_fields: CaptureProposedFields {
                    level: "procedural".to_owned(),
                    kind: "rule".to_owned(),
                    tags: vec![
                        "ambient-capture".to_owned(),
                        "capture-suggest".to_owned(),
                        "cargo".to_owned(),
                    ],
                    content: "Run cargo fmt before release verification.".to_owned(),
                },
                evidence: vec![CaptureEvidenceSpan {
                    id: "span_demo".to_owned(),
                    source_type: "cass_evidence".to_owned(),
                }],
                confidence: 0.76,
                dedupe_status: CaptureDedupeStatus {
                    status: "unique".to_owned(),
                    reason_code: "above_threshold_unique".to_owned(),
                    matched_id: None,
                    score: Some(0.76),
                },
                reason: "frequent command hygiene pattern".to_owned(),
                content_hash: "blake3:demo".to_owned(),
                accept_command: "ee review session cass_demo --propose --json && ee curate accept curate_demo --reason \"accepted capture suggestion from cass_demo\" --json".to_owned(),
                reject_command: "ee review session cass_demo --propose --json && ee curate reject curate_demo --reason \"declined ambient capture suggestion from cass_demo\" --json".to_owned(),
                propose_command: "ee review session cass_demo --propose --json".to_owned(),
            }],
            suppressed: Vec::new(),
            decision_log: vec![CaptureSuggestDecisionLog {
                event: "candidate_evaluated".to_owned(),
                candidate_id: "curate_demo".to_owned(),
                decision: "suggest".to_owned(),
                reason_code: "above_threshold_unique".to_owned(),
                score: Some(0.76),
            }],
            next_action: "review suggested acceptCommand/rejectCommand entries; no memory is stored until a curation command is run".to_owned(),
        };
        let actual = serde_json::to_string_pretty(&report).expect("capture report serializes");
        let expected = include_str!("../tests/fixtures/golden/capture_suggestions_v1.json.golden")
            .trim_end();
        assert_eq!(actual, expected);
    }
}
