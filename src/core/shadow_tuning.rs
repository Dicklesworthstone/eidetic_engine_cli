//! Shadow-tuning label extraction (ADR 0070, bd-2tehh.2 S1).
//!
//! Joins outcome-labeled retrieval triples `(query, memory, signal, weight,
//! age)` from persisted state only:
//!
//! - **Dense source** (weight 1.0): feedback events whose `evidence_json`
//!   carries the `ee.outcome.pack_item_evidence.v1` linkage written by the
//!   `ee outcome --pack/--item` path. The referenced pack record's task text
//!   is the query. Historical events recorded before that linkage shipped
//!   have no pack evidence and are never guessed dense.
//! - **Weak source** (weight 0.5): remaining memory-target feedback events
//!   temporally associated with a `search.returned_mem` audit row for the
//!   same memory. The audit row must precede the outcome (a search after the
//!   outcome cannot have caused the use) within
//!   [`LabelExtractionConfig::label_window_minutes`]; the nearest preceding
//!   row wins, with deterministic ties.
//!
//! Search audits deliberately persist only a query *hash*
//! (`redaction.strategy = query_hash_only_v1`), so weak triples resolve
//! their query text by the ratified hash-join: the audit `queryHash` is
//! matched against persisted pack-record query texts hashed with the same
//! [`audit_query_hash`] function. Weak candidates whose hash matches no
//! persisted text are counted as unreplayable — an honest denominator in
//! the tuning report — never guessed and never silently dropped.
//!
//! Quarantined feedback is excluded by construction: quarantine screening
//! happens at record time, so poisoned events land in `feedback_quarantine`
//! instead of `feedback_events` and never reach this join.
//!
//! Extraction is offline, read-only, cancellable (`&Cx` checkpoints between
//! phases and event chunks), and deterministic: triples are sorted by
//! `(query, memory_id, feedback_event_id)` and the label set is fingerprinted
//! with a length-prefixed BLAKE3 hash so evaluation reports are reproducible.

use std::collections::{BTreeMap, BTreeSet};

use asupersync::Cx;
use chrono::{DateTime, Duration, Utc};

use crate::db::{DbConnection, StoredAuditEntry, StoredFeedbackEvent, audit_actions};
use crate::obs::audit_events::query_hash as audit_query_hash;

/// Evidence schema stamped on pack-item outcome events by the CLI outcome
/// path; the dense label source keys on it.
pub const PACK_ITEM_EVIDENCE_SCHEMA_V1: &str = "ee.outcome.pack_item_evidence.v1";

/// Default `[shadow.retrieval] label_window_minutes` (ADR 0070).
pub const DEFAULT_LABEL_WINDOW_MINUTES: u32 = 30;

/// Domain separator for the label-set fingerprint.
const LABEL_SET_HASH_DOMAIN: &str = "ee.shadow.label_set.v1";

/// Freshness half-life in days: label weight is discounted by
/// `2^(-age_days / 90)`.
const FRESHNESS_HALF_LIFE_DAYS: f64 = 90.0;

const DENSE_BASE_WEIGHT: f64 = 1.0;
const WEAK_BASE_WEIGHT: f64 = 0.5;

/// Events processed between cooperative-cancellation checkpoints.
const CANCELLATION_CHUNK: usize = 256;

/// SQL character cap applied by
/// `list_recent_pack_record_metadata_for_workspace`; query texts at the cap
/// may be truncated and must be reloaded from the full record before hashing.
const PACK_METADATA_QUERY_CHAR_CAP: usize = 2048;

/// Tuning knobs for label extraction (config-file wiring lands with the CLI
/// slice; core takes explicit values so extraction stays deterministic).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LabelExtractionConfig {
    /// Maximum minutes a `search.returned_mem` audit row may precede a
    /// memory outcome and still label it (weak source).
    pub label_window_minutes: u32,
}

impl Default for LabelExtractionConfig {
    fn default() -> Self {
        Self {
            label_window_minutes: DEFAULT_LABEL_WINDOW_MINUTES,
        }
    }
}

/// Which persisted linkage produced a labeled triple.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LabelSource {
    /// Pack-item outcome with persisted pack linkage (dense, base 1.0).
    PackItemOutcome,
    /// Temporal association with a `search.returned_mem` audit row (weak,
    /// base 0.5).
    SearchWindowAssociation,
}

impl LabelSource {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PackItemOutcome => "dense",
            Self::SearchWindowAssociation => "weak",
        }
    }
}

/// One outcome-labeled retrieval example with full provenance.
#[derive(Debug, Clone, PartialEq)]
pub struct LabeledTriple {
    /// Replayable query text (pack task for dense; hash-joined pack query
    /// text for weak).
    pub query: String,
    pub memory_id: String,
    /// Outcome signal as stored on the feedback event (`helpful`,
    /// `harmful`, ...). Gain mapping is the evaluator's concern.
    pub signal: String,
    /// Source base weight before the freshness discount.
    pub base_weight: f64,
    /// `base_weight * 2^(-age_days / 90)`.
    pub weight: f64,
    /// Whole-signal age of the outcome at `as_of`, in fractional days.
    pub age_days: f64,
    pub source: LabelSource,
    /// Provenance: the feedback event that produced this label.
    pub feedback_event_id: String,
    /// Provenance: dense labels carry the linked pack record id.
    pub pack_record_id: Option<String>,
    /// Provenance: weak labels carry the matched audit row id.
    pub audit_row_id: Option<String>,
}

/// Deterministic label-extraction result with honest denominators.
#[derive(Debug, Clone, PartialEq)]
pub struct LabelExtractionReport {
    /// Sorted by `(query, memory_id, feedback_event_id)`.
    pub triples: Vec<LabeledTriple>,
    pub distinct_queries: usize,
    /// Memory-target feedback events inspected.
    pub memory_event_count: usize,
    pub dense_count: usize,
    pub weak_count: usize,
    /// Events claiming pack-item linkage that could not be resolved to a
    /// persisted pack record (malformed linkage or missing record). Counted,
    /// never guessed dense and never demoted to weak.
    pub dense_unresolvable: usize,
    /// Weak candidates whose matched audit row's query hash resolved to no
    /// persisted query text — the ratified honest denominator.
    pub weak_unreplayable: usize,
    /// Weak candidates with no `search.returned_mem` row for their memory
    /// inside the label window.
    pub weak_unmatched: usize,
    /// `blake3:<hex>` fingerprint of the sorted label set.
    pub label_set_hash: String,
}

/// Label-extraction failure.
#[derive(Debug)]
pub enum ShadowTuningError {
    /// Cooperative cancellation observed at a checkpoint.
    Cancelled(asupersync::CancelReason),
    /// Storage read or stored-state integrity failure.
    Storage { message: String },
}

impl std::fmt::Display for ShadowTuningError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cancelled(reason) => {
                write!(f, "shadow-tuning label extraction cancelled: {reason:?}")
            }
            Self::Storage { message } => {
                write!(f, "shadow-tuning storage error: {message}")
            }
        }
    }
}

impl std::error::Error for ShadowTuningError {}

fn shadow_checkpoint(cx: &Cx) -> Result<(), ShadowTuningError> {
    cx.checkpoint().map_err(|_| {
        ShadowTuningError::Cancelled(cx.cancel_reason().unwrap_or_else(|| {
            crate::core::outcome::attributed_cancel_reason(
                cx,
                asupersync::CancelKind::User,
                "shadow-tuning label extraction cancelled without a recorded reason",
            )
        }))
    })
}

fn storage_error(context: &str, error: &dyn std::fmt::Display) -> ShadowTuningError {
    ShadowTuningError::Storage {
        message: format!("{context}: {error}"),
    }
}

/// Extract the labeled-triple set for one workspace from persisted state.
///
/// Reads feedback events, `search.returned_mem` audit rows, and pack records
/// through the supplied connection, then delegates to
/// [`join_labeled_triples`]. `as_of` is an explicit input so freshness
/// discounts (and therefore reports) are reproducible.
pub fn extract_labeled_triples(
    cx: &Cx,
    connection: &DbConnection,
    workspace_id: &str,
    config: &LabelExtractionConfig,
    as_of: DateTime<Utc>,
) -> Result<LabelExtractionReport, ShadowTuningError> {
    shadow_checkpoint(cx)?;
    let events = connection
        .list_feedback_events(workspace_id)
        .map_err(|error| storage_error("list feedback events", &error))?;

    shadow_checkpoint(cx)?;
    let returned_mem_audits: Vec<StoredAuditEntry> = connection
        .list_audit_by_action(audit_actions::SEARCH_RETURNED_MEM, None)
        .map_err(|error| storage_error("list search.returned_mem audit rows", &error))?
        .into_iter()
        .filter(|row| row.workspace_id.as_deref() == Some(workspace_id))
        .collect();

    shadow_checkpoint(cx)?;
    let metadata = connection
        .list_recent_pack_record_metadata_for_workspace(workspace_id, u32::MAX)
        .map_err(|error| storage_error("list pack record metadata", &error))?;

    let mut pack_queries: BTreeMap<String, String> = BTreeMap::new();
    let mut query_text_by_hash: BTreeMap<String, String> = BTreeMap::new();
    for (index, meta) in metadata.into_iter().enumerate() {
        if index % CANCELLATION_CHUNK == 0 {
            shadow_checkpoint(cx)?;
        }
        // The metadata projection caps query text in SQL; a text at the cap
        // may be truncated, and hashing truncated text would silently break
        // the hash-join. Reload the full record for exact text.
        let query = if meta.query.chars().count() >= PACK_METADATA_QUERY_CHAR_CAP {
            connection
                .get_pack_record(&meta.id)
                .map_err(|error| storage_error("load full pack record", &error))?
                .map_or(meta.query, |record| record.query)
        } else {
            meta.query
        };
        let hash = audit_query_hash(&query);
        // Keep the lexicographically smallest text per hash so the map is
        // deterministic regardless of enumeration order.
        match query_text_by_hash.get(&hash) {
            Some(existing) if existing <= &query => {}
            _ => {
                query_text_by_hash.insert(hash, query.clone());
            }
        }
        pack_queries.insert(meta.id, query);
    }

    join_labeled_triples(
        cx,
        &events,
        &returned_mem_audits,
        &pack_queries,
        &query_text_by_hash,
        config,
        as_of,
    )
}

/// Dense-linkage classification of one event's `evidence_json`.
enum DenseLinkage {
    /// No pack-item linkage: the event is a weak candidate.
    NotDense,
    /// The evidence claims the pack-item schema but the linkage cannot be
    /// used (malformed `packId`). Never guessed either way.
    Unresolvable,
    Linked {
        pack_id: String,
    },
}

fn parse_pack_item_linkage(evidence_json: Option<&str>) -> DenseLinkage {
    let Some(raw) = evidence_json else {
        return DenseLinkage::NotDense;
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(raw) else {
        return DenseLinkage::NotDense;
    };
    if value.get("schema").and_then(serde_json::Value::as_str) != Some(PACK_ITEM_EVIDENCE_SCHEMA_V1)
    {
        return DenseLinkage::NotDense;
    }
    match value.get("packId").and_then(serde_json::Value::as_str) {
        Some(pack_id) if !pack_id.trim().is_empty() => DenseLinkage::Linked {
            pack_id: pack_id.to_owned(),
        },
        _ => DenseLinkage::Unresolvable,
    }
}

/// One pre-parsed `search.returned_mem` audit row.
struct ParsedReturnedMem {
    id: String,
    memory_id: String,
    timestamp: DateTime<Utc>,
    query_hash: Option<String>,
}

fn parse_returned_mem_rows(
    rows: &[StoredAuditEntry],
) -> Result<Vec<ParsedReturnedMem>, ShadowTuningError> {
    let mut parsed = Vec::with_capacity(rows.len());
    for row in rows {
        if row.action != audit_actions::SEARCH_RETURNED_MEM {
            continue;
        }
        let Some(memory_id) = row.target_id.as_deref() else {
            continue;
        };
        // Audit timestamps are written by this binary as RFC 3339; a row
        // that no longer parses is stored-state corruption and silently
        // skipping it could flip a window match. Fail loudly.
        let timestamp = DateTime::parse_from_rfc3339(&row.timestamp)
            .map_err(|error| {
                storage_error(
                    &format!("audit row {} has an unparsable timestamp", row.id),
                    &error,
                )
            })?
            .with_timezone(&Utc);
        let query_hash = row
            .details
            .as_deref()
            .and_then(|details| serde_json::from_str::<serde_json::Value>(details).ok())
            .and_then(|value| {
                value
                    .get("queryHash")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned)
            });
        parsed.push(ParsedReturnedMem {
            id: row.id.clone(),
            memory_id: memory_id.to_owned(),
            timestamp,
            query_hash,
        });
    }
    Ok(parsed)
}

/// Join labeled triples from pre-loaded rows (pure core of the extractor).
///
/// `pack_queries` maps pack record id → task text (dense resolution);
/// `query_text_by_hash` maps [`audit_query_hash`] output → query text (weak
/// hash-join resolution).
#[allow(clippy::too_many_lines)]
pub fn join_labeled_triples(
    cx: &Cx,
    events: &[StoredFeedbackEvent],
    returned_mem_audits: &[StoredAuditEntry],
    pack_queries: &BTreeMap<String, String>,
    query_text_by_hash: &BTreeMap<String, String>,
    config: &LabelExtractionConfig,
    as_of: DateTime<Utc>,
) -> Result<LabelExtractionReport, ShadowTuningError> {
    shadow_checkpoint(cx)?;
    let window = Duration::minutes(i64::from(config.label_window_minutes));

    let parsed_audits = parse_returned_mem_rows(returned_mem_audits)?;
    let mut audits_by_memory: BTreeMap<&str, Vec<&ParsedReturnedMem>> = BTreeMap::new();
    for audit in &parsed_audits {
        audits_by_memory
            .entry(audit.memory_id.as_str())
            .or_default()
            .push(audit);
    }

    let mut triples: Vec<LabeledTriple> = Vec::new();
    let mut memory_event_count = 0_usize;
    let mut dense_unresolvable = 0_usize;
    let mut weak_unreplayable = 0_usize;
    let mut weak_unmatched = 0_usize;

    for (index, event) in events.iter().enumerate() {
        if index % CANCELLATION_CHUNK == 0 {
            shadow_checkpoint(cx)?;
        }
        if event.target_type != "memory" {
            continue;
        }
        memory_event_count += 1;

        let created_at = DateTime::parse_from_rfc3339(&event.created_at)
            .map_err(|error| {
                storage_error(
                    &format!("feedback event {} has an unparsable created_at", event.id),
                    &error,
                )
            })?
            .with_timezone(&Utc);
        let age_days = age_in_days(created_at, as_of);
        let freshness = (-age_days / FRESHNESS_HALF_LIFE_DAYS).exp2();

        match parse_pack_item_linkage(event.evidence_json.as_deref()) {
            DenseLinkage::Linked { pack_id } => match pack_queries.get(&pack_id) {
                Some(query) => triples.push(LabeledTriple {
                    query: query.clone(),
                    memory_id: event.target_id.clone(),
                    signal: event.signal.clone(),
                    base_weight: DENSE_BASE_WEIGHT,
                    weight: DENSE_BASE_WEIGHT * freshness,
                    age_days,
                    source: LabelSource::PackItemOutcome,
                    feedback_event_id: event.id.clone(),
                    pack_record_id: Some(pack_id),
                    audit_row_id: None,
                }),
                // Linked to a pack record that no longer resolves: the
                // event asserted pack context, so demoting it to the weak
                // path would double-interpret it. Count it honestly.
                None => dense_unresolvable += 1,
            },
            DenseLinkage::Unresolvable => dense_unresolvable += 1,
            DenseLinkage::NotDense => {
                let nearest = audits_by_memory
                    .get(event.target_id.as_str())
                    .into_iter()
                    .flatten()
                    .filter(|audit| {
                        audit.timestamp <= created_at && created_at - audit.timestamp <= window
                    })
                    .max_by(|a, b| a.timestamp.cmp(&b.timestamp).then_with(|| a.id.cmp(&b.id)));
                match nearest {
                    None => weak_unmatched += 1,
                    Some(audit) => {
                        let resolved = audit
                            .query_hash
                            .as_deref()
                            .and_then(|hash| query_text_by_hash.get(hash));
                        match resolved {
                            None => weak_unreplayable += 1,
                            Some(query) => triples.push(LabeledTriple {
                                query: query.clone(),
                                memory_id: event.target_id.clone(),
                                signal: event.signal.clone(),
                                base_weight: WEAK_BASE_WEIGHT,
                                weight: WEAK_BASE_WEIGHT * freshness,
                                age_days,
                                source: LabelSource::SearchWindowAssociation,
                                feedback_event_id: event.id.clone(),
                                pack_record_id: None,
                                audit_row_id: Some(audit.id.clone()),
                            }),
                        }
                    }
                }
            }
        }
    }

    shadow_checkpoint(cx)?;
    triples.sort_by(|a, b| {
        a.query
            .cmp(&b.query)
            .then_with(|| a.memory_id.cmp(&b.memory_id))
            .then_with(|| a.feedback_event_id.cmp(&b.feedback_event_id))
    });

    let distinct_queries = triples
        .iter()
        .map(|triple| triple.query.as_str())
        .collect::<BTreeSet<&str>>()
        .len();
    let dense_count = triples
        .iter()
        .filter(|triple| triple.source == LabelSource::PackItemOutcome)
        .count();
    let weak_count = triples.len() - dense_count;
    let label_set_hash = label_set_hash(&triples);

    Ok(LabelExtractionReport {
        triples,
        distinct_queries,
        memory_event_count,
        dense_count,
        weak_count,
        dense_unresolvable,
        weak_unreplayable,
        weak_unmatched,
        label_set_hash,
    })
}

fn age_in_days(created_at: DateTime<Utc>, as_of: DateTime<Utc>) -> f64 {
    let seconds = (as_of - created_at).num_seconds().max(0);
    #[allow(clippy::cast_precision_loss)]
    let seconds = seconds as f64;
    seconds / 86_400.0
}

fn append_len_prefixed(out: &mut Vec<u8>, bytes: &[u8]) {
    let len = u32::try_from(bytes.len()).unwrap_or(u32::MAX);
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(bytes);
}

/// Length-prefixed BLAKE3 fingerprint of a sorted label set.
fn label_set_hash(triples: &[LabeledTriple]) -> String {
    let mut input = Vec::new();
    append_len_prefixed(&mut input, LABEL_SET_HASH_DOMAIN.as_bytes());
    input.extend_from_slice(
        &u32::try_from(triples.len())
            .unwrap_or(u32::MAX)
            .to_be_bytes(),
    );
    for triple in triples {
        append_len_prefixed(&mut input, triple.query.as_bytes());
        append_len_prefixed(&mut input, triple.memory_id.as_bytes());
        append_len_prefixed(&mut input, triple.signal.as_bytes());
        append_len_prefixed(&mut input, triple.source.as_str().as_bytes());
        append_len_prefixed(&mut input, triple.feedback_event_id.as_bytes());
        append_len_prefixed(
            &mut input,
            triple.pack_record_id.as_deref().unwrap_or("").as_bytes(),
        );
        append_len_prefixed(
            &mut input,
            triple.audit_row_id.as_deref().unwrap_or("").as_bytes(),
        );
        input.extend_from_slice(&triple.base_weight.to_bits().to_be_bytes());
        input.extend_from_slice(&triple.weight.to_bits().to_be_bytes());
        input.extend_from_slice(&triple.age_days.to_bits().to_be_bytes());
    }
    format!("blake3:{}", blake3::hash(&input).to_hex())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::db::{
        CreateAuditInput, CreateFeedbackEventInput, CreateFeedbackQuarantineInput,
        CreatePackRecordInput, CreateWorkspaceInput,
    };
    use asupersync::CancelReason;
    use chrono::TimeZone;

    type TestResult = Result<(), String>;

    const WORKSPACE: &str = "ws-shadow-tuning";

    fn ts(minute: i64) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 8, 1, 12, 0, 0).unwrap() + Duration::minutes(minute)
    }

    fn event(
        id: &str,
        memory_id: &str,
        created_at: DateTime<Utc>,
        evidence_json: Option<String>,
    ) -> StoredFeedbackEvent {
        StoredFeedbackEvent {
            id: id.to_owned(),
            workspace_id: WORKSPACE.to_owned(),
            target_type: "memory".to_owned(),
            target_id: memory_id.to_owned(),
            signal: "helpful".to_owned(),
            weight: 1.0,
            source_type: "outcome_observed".to_owned(),
            source_id: None,
            reason: None,
            evidence_json,
            session_id: None,
            applied_at: None,
            created_at: created_at.to_rfc3339(),
        }
    }

    fn returned_mem_audit(
        id: &str,
        memory_id: &str,
        timestamp: DateTime<Utc>,
        query_hash: Option<&str>,
    ) -> StoredAuditEntry {
        let details = query_hash.map(|hash| {
            serde_json::json!({
                "queryHash": hash,
                "rank": 1,
                "score": 0.9,
                "source": "semantic",
            })
            .to_string()
        });
        StoredAuditEntry {
            id: id.to_owned(),
            workspace_id: Some(WORKSPACE.to_owned()),
            timestamp: timestamp.to_rfc3339(),
            actor: None,
            action: audit_actions::SEARCH_RETURNED_MEM.to_owned(),
            target_type: Some("memory".to_owned()),
            target_id: Some(memory_id.to_owned()),
            details,
            surface: "search".to_owned(),
            mutation_kind: audit_actions::SEARCH_RETURNED_MEM.to_owned(),
            before_hash: None,
            after_hash: None,
            prev_row_hash: None,
            this_row_hash: None,
        }
    }

    fn pack_item_evidence(pack_id: &str) -> String {
        serde_json::json!({
            "schema": PACK_ITEM_EVIDENCE_SCHEMA_V1,
            "packId": pack_id,
            "itemRank": 2,
        })
        .to_string()
    }

    fn join(
        events: &[StoredFeedbackEvent],
        audits: &[StoredAuditEntry],
        pack_queries: &BTreeMap<String, String>,
        query_text_by_hash: &BTreeMap<String, String>,
        as_of: DateTime<Utc>,
    ) -> Result<LabelExtractionReport, String> {
        let cx = Cx::for_testing();
        join_labeled_triples(
            &cx,
            events,
            audits,
            pack_queries,
            query_text_by_hash,
            &LabelExtractionConfig::default(),
            as_of,
        )
        .map_err(|error| error.to_string())
    }

    fn approx_eq(actual: f64, expected: f64) -> bool {
        (actual - expected).abs() < 1e-9
    }

    #[test]
    fn dense_pack_item_event_yields_weight_one_triple() -> TestResult {
        let created = ts(0);
        let events = [event(
            "fev-1",
            "mem-1",
            created,
            Some(pack_item_evidence("pack-1")),
        )];
        let pack_queries = BTreeMap::from([(
            "pack-1".to_owned(),
            "fix failing release workflow".to_owned(),
        )]);
        let report = join(&events, &[], &pack_queries, &BTreeMap::new(), created)?;

        if report.triples.len() != 1 {
            return Err(format!("expected one dense triple, got {report:?}"));
        }
        let triple = &report.triples[0];
        if triple.query != "fix failing release workflow"
            || triple.memory_id != "mem-1"
            || triple.source != LabelSource::PackItemOutcome
            || triple.pack_record_id.as_deref() != Some("pack-1")
            || triple.audit_row_id.is_some()
        {
            return Err(format!("dense triple fields wrong: {triple:?}"));
        }
        // Zero age at as_of == created_at: no freshness discount.
        if !approx_eq(triple.base_weight, 1.0) || !approx_eq(triple.weight, 1.0) {
            return Err(format!("dense weight wrong: {triple:?}"));
        }
        if report.dense_count != 1 || report.weak_count != 0 || report.distinct_queries != 1 {
            return Err(format!("report counts wrong: {report:?}"));
        }
        Ok(())
    }

    #[test]
    fn dense_linkage_with_missing_pack_record_is_counted_not_guessed() -> TestResult {
        let created = ts(0);
        let events = [event(
            "fev-1",
            "mem-1",
            created,
            Some(pack_item_evidence("pack-gone")),
        )];
        // A matching weak-side audit row exists; the dense-claiming event
        // must NOT fall back to it.
        let query = "weak query";
        let hash = audit_query_hash(query);
        let audits = [returned_mem_audit("aud-1", "mem-1", ts(-5), Some(&hash))];
        let query_text_by_hash = BTreeMap::from([(hash.clone(), query.to_owned())]);

        let report = join(
            &events,
            &audits,
            &BTreeMap::new(),
            &query_text_by_hash,
            created,
        )?;
        if !report.triples.is_empty()
            || report.dense_unresolvable != 1
            || report.weak_unmatched != 0
            || report.weak_unreplayable != 0
        {
            return Err(format!("missing pack record mishandled: {report:?}"));
        }
        Ok(())
    }

    #[test]
    fn weak_event_joins_nearest_preceding_returned_mem_within_window() -> TestResult {
        let created = ts(0);
        let near_query = "near query";
        let far_query = "far query";
        let near_hash = audit_query_hash(near_query);
        let far_hash = audit_query_hash(far_query);
        let events = [event("fev-1", "mem-1", created, None)];
        let audits = [
            returned_mem_audit("aud-far", "mem-1", ts(-25), Some(&far_hash)),
            returned_mem_audit("aud-near", "mem-1", ts(-10), Some(&near_hash)),
        ];
        let query_text_by_hash = BTreeMap::from([
            (near_hash.clone(), near_query.to_owned()),
            (far_hash.clone(), far_query.to_owned()),
        ]);

        let report = join(
            &events,
            &audits,
            &BTreeMap::new(),
            &query_text_by_hash,
            created,
        )?;
        if report.triples.len() != 1 {
            return Err(format!("expected one weak triple, got {report:?}"));
        }
        let triple = &report.triples[0];
        if triple.query != near_query
            || triple.source != LabelSource::SearchWindowAssociation
            || triple.audit_row_id.as_deref() != Some("aud-near")
            || !approx_eq(triple.base_weight, 0.5)
        {
            return Err(format!("weak triple fields wrong: {triple:?}"));
        }
        Ok(())
    }

    #[test]
    fn weak_window_edge_is_inclusive_and_beyond_is_unmatched() -> TestResult {
        let created = ts(0);
        let query = "edge query";
        let hash = audit_query_hash(query);
        let query_text_by_hash = BTreeMap::from([(hash.clone(), query.to_owned())]);

        // Exactly at the 30-minute default window edge: included.
        let events = [event("fev-1", "mem-1", created, None)];
        let at_edge = [returned_mem_audit("aud-1", "mem-1", ts(-30), Some(&hash))];
        let report = join(
            &events,
            &at_edge,
            &BTreeMap::new(),
            &query_text_by_hash,
            created,
        )?;
        if report.weak_count != 1 || report.weak_unmatched != 0 {
            return Err(format!("window edge must be inclusive: {report:?}"));
        }

        // One second beyond the window: unmatched.
        let beyond = [returned_mem_audit(
            "aud-1",
            "mem-1",
            ts(-30) - Duration::seconds(1),
            Some(&hash),
        )];
        let report = join(
            &events,
            &beyond,
            &BTreeMap::new(),
            &query_text_by_hash,
            created,
        )?;
        if report.weak_count != 0 || report.weak_unmatched != 1 {
            return Err(format!("beyond-window audit must not label: {report:?}"));
        }
        Ok(())
    }

    #[test]
    fn search_after_outcome_never_labels() -> TestResult {
        let created = ts(0);
        let query = "later query";
        let hash = audit_query_hash(query);
        let events = [event("fev-1", "mem-1", created, None)];
        let audits = [returned_mem_audit("aud-1", "mem-1", ts(5), Some(&hash))];
        let query_text_by_hash = BTreeMap::from([(hash.clone(), query.to_owned())]);

        let report = join(
            &events,
            &audits,
            &BTreeMap::new(),
            &query_text_by_hash,
            created,
        )?;
        if report.weak_count != 0 || report.weak_unmatched != 1 {
            return Err(format!(
                "an audit row after the outcome cannot have caused it: {report:?}"
            ));
        }
        Ok(())
    }

    #[test]
    fn weak_hash_miss_is_counted_unreplayable_not_guessed() -> TestResult {
        let created = ts(0);
        let events = [event("fev-1", "mem-1", created, None)];
        let audits = [returned_mem_audit(
            "aud-1",
            "mem-1",
            ts(-5),
            Some("blake3:0000000000000000"),
        )];
        let report = join(
            &events,
            &audits,
            &BTreeMap::new(),
            &BTreeMap::new(),
            created,
        )?;
        if !report.triples.is_empty() || report.weak_unreplayable != 1 || report.weak_unmatched != 0
        {
            return Err(format!("hash miss must count unreplayable: {report:?}"));
        }
        Ok(())
    }

    #[test]
    fn freshness_discount_halves_weight_at_ninety_days() -> TestResult {
        let created = ts(0);
        let as_of = created + Duration::days(90);
        let events = [event(
            "fev-1",
            "mem-1",
            created,
            Some(pack_item_evidence("pack-1")),
        )];
        let pack_queries = BTreeMap::from([("pack-1".to_owned(), "aged query".to_owned())]);
        let report = join(&events, &[], &pack_queries, &BTreeMap::new(), as_of)?;
        let triple = &report.triples[0];
        if !approx_eq(triple.age_days, 90.0) || !approx_eq(triple.weight, 0.5) {
            return Err(format!("90-day freshness discount wrong: {triple:?}"));
        }
        Ok(())
    }

    #[test]
    fn non_memory_targets_are_ignored() -> TestResult {
        let created = ts(0);
        let mut pack_event = event(
            "fev-1",
            "pack-1",
            created,
            Some(pack_item_evidence("pack-1")),
        );
        pack_event.target_type = "pack".to_owned();
        let pack_queries = BTreeMap::from([("pack-1".to_owned(), "some query".to_owned())]);
        let report = join(&[pack_event], &[], &pack_queries, &BTreeMap::new(), created)?;
        if report.memory_event_count != 0 || !report.triples.is_empty() {
            return Err(format!("non-memory targets must be ignored: {report:?}"));
        }
        Ok(())
    }

    #[test]
    fn triples_are_sorted_and_hash_is_order_independent() -> TestResult {
        let created = ts(0);
        let pack_queries = BTreeMap::from([
            ("pack-a".to_owned(), "alpha query".to_owned()),
            ("pack-b".to_owned(), "beta query".to_owned()),
        ]);
        let forward = [
            event(
                "fev-1",
                "mem-1",
                created,
                Some(pack_item_evidence("pack-a")),
            ),
            event(
                "fev-2",
                "mem-2",
                created,
                Some(pack_item_evidence("pack-b")),
            ),
        ];
        let reversed = [forward[1].clone(), forward[0].clone()];

        let report_a = join(&forward, &[], &pack_queries, &BTreeMap::new(), created)?;
        let report_b = join(&reversed, &[], &pack_queries, &BTreeMap::new(), created)?;
        if report_a != report_b {
            return Err("label extraction must be input-order independent".to_owned());
        }
        let queries: Vec<&str> = report_a
            .triples
            .iter()
            .map(|triple| triple.query.as_str())
            .collect();
        if queries != ["alpha query", "beta query"] {
            return Err(format!("triples must sort by query: {queries:?}"));
        }
        if !report_a.label_set_hash.starts_with("blake3:") {
            return Err(format!(
                "hash must be prefixed: {}",
                report_a.label_set_hash
            ));
        }

        // A different label set must fingerprint differently.
        let smaller = join(&forward[..1], &[], &pack_queries, &BTreeMap::new(), created)?;
        if smaller.label_set_hash == report_a.label_set_hash {
            return Err("different label sets must not share a fingerprint".to_owned());
        }
        Ok(())
    }

    #[test]
    fn cancelled_cx_aborts_extraction() -> TestResult {
        let cx = Cx::for_testing();
        cx.set_cancel_reason(CancelReason::user("shadow tuning cancellation test"));
        let created = ts(0);
        let events = [event("fev-1", "mem-1", created, None)];
        let outcome = join_labeled_triples(
            &cx,
            &events,
            &[],
            &BTreeMap::new(),
            &BTreeMap::new(),
            &LabelExtractionConfig::default(),
            created,
        );
        match outcome {
            Err(ShadowTuningError::Cancelled(_)) => Ok(()),
            other => Err(format!("cancelled Cx must abort extraction: {other:?}")),
        }
    }

    #[test]
    fn extract_from_database_joins_dense_and_excludes_quarantine() -> TestResult {
        let tempdir = tempfile::tempdir_in("/tmp").map_err(|error| error.to_string())?;
        let database_path = tempdir.path().join("ee.db");
        let connection =
            DbConnection::open_file(&database_path).map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        connection
            .insert_workspace(
                WORKSPACE,
                &CreateWorkspaceInput {
                    path: tempdir.path().display().to_string(),
                    name: None,
                },
            )
            .map_err(|error| error.to_string())?;
        connection
            .insert_pack_record(
                "pack-1",
                &CreatePackRecordInput {
                    workspace_id: WORKSPACE.to_owned(),
                    query: "prepare release".to_owned(),
                    profile: "default".to_owned(),
                    max_tokens: 4000,
                    used_tokens: 0,
                    item_count: 0,
                    omitted_count: 0,
                    pack_hash: "blake3:test".to_owned(),
                    degraded_json: None,
                    created_by: None,
                },
                &[],
                &[],
            )
            .map_err(|error| error.to_string())?;
        connection
            .insert_feedback_event(
                "fev-dense",
                &CreateFeedbackEventInput {
                    workspace_id: WORKSPACE.to_owned(),
                    target_type: "memory".to_owned(),
                    target_id: "mem-1".to_owned(),
                    signal: "helpful".to_owned(),
                    weight: 1.0,
                    source_type: "outcome_observed".to_owned(),
                    source_id: None,
                    reason: None,
                    evidence_json: Some(pack_item_evidence("pack-1")),
                    session_id: None,
                },
            )
            .map_err(|error| error.to_string())?;
        // Quarantined feedback lands in a separate table at record time and
        // must never reach the join.
        connection
            .insert_feedback_quarantine(
                "fq-1",
                &CreateFeedbackQuarantineInput {
                    workspace_id: WORKSPACE.to_owned(),
                    source_id: "poisoned-source".to_owned(),
                    target_type: "memory".to_owned(),
                    target_id: "mem-1".to_owned(),
                    signal: "harmful".to_owned(),
                    weight: 1.0,
                    source_type: "agent_inference".to_owned(),
                    proposed_event_id: None,
                    recorded_at: Utc::now().to_rfc3339(),
                    reason: "sprt_quarantine".to_owned(),
                    event_reason: None,
                    evidence_json: None,
                    session_id: None,
                    raw_event_hash: "blake3:quarantine-fixture".to_owned(),
                },
            )
            .map_err(|error| error.to_string())?;

        let cx = Cx::for_testing();
        let report = extract_labeled_triples(
            &cx,
            &connection,
            WORKSPACE,
            &LabelExtractionConfig::default(),
            Utc::now() + Duration::minutes(1),
        )
        .map_err(|error| error.to_string())?;

        if report.triples.len() != 1 || report.dense_count != 1 {
            return Err(format!("expected exactly the dense triple: {report:?}"));
        }
        let triple = &report.triples[0];
        if triple.query != "prepare release"
            || triple.feedback_event_id != "fev-dense"
            || triple.signal != "helpful"
        {
            return Err(format!("db dense triple wrong: {triple:?}"));
        }
        if report.memory_event_count != 1 {
            return Err(format!(
                "quarantined feedback must be invisible to the join: {report:?}"
            ));
        }
        Ok(())
    }

    #[test]
    fn extract_from_database_resolves_weak_query_via_hash_join() -> TestResult {
        let tempdir = tempfile::tempdir_in("/tmp").map_err(|error| error.to_string())?;
        let database_path = tempdir.path().join("ee.db");
        let connection =
            DbConnection::open_file(&database_path).map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        connection
            .insert_workspace(
                WORKSPACE,
                &CreateWorkspaceInput {
                    path: tempdir.path().display().to_string(),
                    name: None,
                },
            )
            .map_err(|error| error.to_string())?;
        let query = "hunt flaky mesh test";
        connection
            .insert_pack_record(
                "pack-weak",
                &CreatePackRecordInput {
                    workspace_id: WORKSPACE.to_owned(),
                    query: query.to_owned(),
                    profile: "default".to_owned(),
                    max_tokens: 4000,
                    used_tokens: 0,
                    item_count: 0,
                    omitted_count: 0,
                    pack_hash: "blake3:test-weak".to_owned(),
                    degraded_json: None,
                    created_by: None,
                },
                &[],
                &[],
            )
            .map_err(|error| error.to_string())?;
        // The audit row precedes the feedback event (insert order), so the
        // outcome falls inside the default label window.
        connection
            .insert_audit(
                "aud-weak",
                &CreateAuditInput {
                    workspace_id: Some(WORKSPACE.to_owned()),
                    actor: None,
                    action: audit_actions::SEARCH_RETURNED_MEM.to_owned(),
                    target_type: Some("memory".to_owned()),
                    target_id: Some("mem-9".to_owned()),
                    details: Some(
                        serde_json::json!({
                            "queryHash": audit_query_hash(query),
                            "rank": 1,
                            "score": 0.8,
                            "source": "lexical",
                        })
                        .to_string(),
                    ),
                },
            )
            .map_err(|error| error.to_string())?;
        connection
            .insert_feedback_event(
                "fev-weak",
                &CreateFeedbackEventInput {
                    workspace_id: WORKSPACE.to_owned(),
                    target_type: "memory".to_owned(),
                    target_id: "mem-9".to_owned(),
                    signal: "helpful".to_owned(),
                    weight: 1.0,
                    source_type: "outcome_observed".to_owned(),
                    source_id: None,
                    reason: None,
                    evidence_json: None,
                    session_id: None,
                },
            )
            .map_err(|error| error.to_string())?;

        let cx = Cx::for_testing();
        let report = extract_labeled_triples(
            &cx,
            &connection,
            WORKSPACE,
            &LabelExtractionConfig::default(),
            Utc::now() + Duration::minutes(1),
        )
        .map_err(|error| error.to_string())?;

        if report.triples.len() != 1 || report.weak_count != 1 {
            return Err(format!("expected exactly the weak triple: {report:?}"));
        }
        let triple = &report.triples[0];
        if triple.query != query
            || triple.source != LabelSource::SearchWindowAssociation
            || triple.audit_row_id.as_deref() != Some("aud-weak")
            || !approx_eq(triple.base_weight, 0.5)
        {
            return Err(format!("db weak triple wrong: {triple:?}"));
        }
        if report.weak_unreplayable != 0 || report.weak_unmatched != 0 {
            return Err(format!("weak denominators must be clean: {report:?}"));
        }
        Ok(())
    }
}
