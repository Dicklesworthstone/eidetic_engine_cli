//! Shadow-tuning label extraction and replay evaluator (ADR 0070,
//! bd-2tehh.2 S1+S2).
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
use std::path::Path;

use asupersync::Cx;
use chrono::{DateTime, Duration, Utc};

use crate::core::search::{
    SearchDedupMode, SearchFusionWeights, SearchHit, SearchOptions, SearchSourceMode,
    configured_fusion_adjustment, resolved_search_fusion_weights,
    run_search_with_read_connection_seeded, search_hit_meets_relevance_floor,
    sort_search_hits_by_score_order,
};
use crate::db::{DbConnection, StoredAuditEntry, StoredFeedbackEvent, audit_actions};
use crate::models::MemoryScope;
use crate::obs::audit_events::query_hash as audit_query_hash;
use crate::runtime::determinism::Deterministic;
use crate::search::SpeedMode;

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

// ===================== S2: replay evaluator (ADR 0070 §2–3) =====================
//
// Fact-check correction folded in (banked on bd-2tehh.2): the ADR names
// `SearchScoringConfig` as the tunable, but that struct has no production
// consumer — live ranking moves through `SearchFusionWeights`
// (`search.lexical_weight` / `search.semantic_weight` / `search.graph_weight`),
// applied per hit by `configured_fusion_adjustment`. The evaluator therefore
// tunes fusion weights, and the ADR's recency-tau axis is dropped: live search
// has no recency knob, and sweeping a parameter ranking ignores would report
// fake capability. The graph axis is kept but its sensitivity is degenerate
// today (`graph_component` is hardcoded 0.0 in the live adjustment; the weight
// only moves scores through renormalization) — consumers of the evaluation
// must read `graph_axis_degenerate` honestly.
//
// Mechanism: single-replay offline re-fusion. Each distinct labeled query is
// replayed ONCE against the current index (read-only, seeded, floor disabled,
// pool capped); per-hit arm components ride on the returned `SearchHit`s, so
// every candidate vector re-scores the same pool with the production
// adjustment function, re-applies the production relevance floor, and re-sorts
// with the production ordering. No candidate can reach live ranking: the
// injection exists only inside this evaluator, and the CLI search surface has
// no weight argument (frozen by the golden help contracts).

/// ADR §3 clamps — deliberately in code next to the policy, not user config.
const FUSION_LEXICAL_CLAMP: (f32, f32) = (0.2, 0.7);
const FUSION_SEMANTIC_CLAMP: (f32, f32) = (0.2, 0.7);
const FUSION_GRAPH_CLAMP: (f32, f32) = (0.0, 0.3);
/// Fixed per-axis offset grid around the incumbent (ADR §3).
const FUSION_GRID_OFFSETS: [f32; 4] = [-0.10, -0.05, 0.05, 0.10];
const DESCENT_MAX_ROUNDS: u32 = 2;
const DESCENT_INITIAL_STEP: f32 = 0.025;
/// Replayed hit-pool cap per query. Labeled memories outside the pool count
/// as unranked (contribute 0), exactly like results beyond a live limit.
pub const REPLAY_POOL_LIMIT: u32 = 200;
const EVALUATION_HASH_DOMAIN: &str = "ee.shadow.retrieval_tuning_evaluation.v1";

/// One candidate fusion-weight vector.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TuningWeights {
    pub lexical: f32,
    pub semantic: f32,
    pub graph: f32,
}

impl TuningWeights {
    /// The compiled-in fusion defaults (used when no workspace overlay is set).
    #[must_use]
    pub fn compiled_defaults() -> Self {
        Self::from_fusion(SearchFusionWeights::default())
    }

    /// The incumbent vector actually in effect for a workspace.
    #[must_use]
    pub fn incumbent_for_workspace(workspace_path: &Path) -> Self {
        Self::from_fusion(resolved_search_fusion_weights(workspace_path))
    }

    fn from_fusion(weights: SearchFusionWeights) -> Self {
        Self {
            lexical: weights.lexical,
            semantic: weights.semantic,
            graph: weights.graph,
        }
    }

    fn to_fusion(self) -> SearchFusionWeights {
        SearchFusionWeights {
            lexical: self.lexical,
            semantic: self.semantic,
            graph: self.graph,
        }
    }

    fn clamped(self) -> Self {
        Self {
            lexical: self
                .lexical
                .clamp(FUSION_LEXICAL_CLAMP.0, FUSION_LEXICAL_CLAMP.1),
            semantic: self
                .semantic
                .clamp(FUSION_SEMANTIC_CLAMP.0, FUSION_SEMANTIC_CLAMP.1),
            graph: self.graph.clamp(FUSION_GRAPH_CLAMP.0, FUSION_GRAPH_CLAMP.1),
        }
    }

    /// Exact-bit identity key for deduplication and deterministic ordering.
    fn key(self) -> (u32, u32, u32) {
        (
            self.lexical.to_bits(),
            self.semantic.to_bits(),
            self.graph.to_bits(),
        )
    }
}

/// One replayed query with its raw-score (pre-adjustment) hit pool.
#[derive(Debug, Clone)]
pub struct QueryReplay {
    pub query: String,
    /// Hits with `score` restored to the raw fusion score (the workspace's
    /// own fusion adjustment divided back out), so candidate re-fusion starts
    /// from the same basis the live pipeline does.
    pub hits: Vec<SearchHit>,
}

/// Replay collection result with honest denominators.
#[derive(Debug, Clone)]
pub struct ReplayCollection {
    pub replays: Vec<QueryReplay>,
    /// Hits dropped because the workspace fusion multiplier was zero and the
    /// raw score could not be recovered (requires a degenerate zero-weight
    /// workspace config; counted, never guessed).
    pub unrecoverable_hits: usize,
}

/// Replay every distinct labeled query once against the current index.
///
/// Read-only by construction: the underlying search entry point passes no
/// audit connection, so no audit rows can be written. The relevance floor is
/// disabled at replay and re-applied per candidate offline, because floor
/// membership depends on the adjusted score each candidate produces.
pub fn collect_query_replays(
    cx: &Cx,
    read_connection: &crate::db::DbConnection,
    workspace_path: &Path,
    database_path: &Path,
    queries: &BTreeSet<String>,
    as_of: DateTime<Utc>,
) -> Result<ReplayCollection, ShadowTuningError> {
    let workspace_weights = resolved_search_fusion_weights(workspace_path);
    let determinism = Deterministic::from_seed(0);
    let mut replays = Vec::with_capacity(queries.len());
    let mut unrecoverable_hits = 0_usize;

    for query in queries {
        shadow_checkpoint(cx)?;
        let options = SearchOptions {
            workspace_path: workspace_path.to_path_buf(),
            database_path: Some(database_path.to_path_buf()),
            index_dir: None,
            query: query.clone(),
            limit: REPLAY_POOL_LIMIT,
            speed: SpeedMode::Default,
            explain: false,
            as_of: Some(as_of),
            include_tombstoned: false,
            include_expired: false,
            include_future: false,
            include_stale: false,
            relevance_floor: Some(0.0),
            dedup_mode: SearchDedupMode::DocId,
            source_mode: SearchSourceMode::Hybrid,
            strict_source_mode: false,
            memory_scope: MemoryScope::default(),
            strict_scope: false,
        };
        let report =
            run_search_with_read_connection_seeded(&options, read_connection, &determinism)
                .map_err(|error| storage_error("replay search failed", &error))?;
        let mut hits = Vec::with_capacity(report.results.len());
        for mut hit in report.results {
            // The live pipeline multiplied the raw fusion score by the
            // workspace adjustment; divide it back out so candidate
            // re-fusion starts from the raw basis. The multiplier depends
            // only on the hit's arm components, which are unchanged.
            if let Some(adjustment) =
                configured_fusion_adjustment(&hit, SearchSourceMode::Hybrid, workspace_weights)
            {
                if adjustment.multiplier > f32::EPSILON {
                    hit.score /= adjustment.multiplier;
                } else {
                    unrecoverable_hits += 1;
                    continue;
                }
            }
            hits.push(hit);
        }
        replays.push(QueryReplay {
            query: query.clone(),
            hits,
        });
    }

    Ok(ReplayCollection {
        replays,
        unrecoverable_hits,
    })
}

/// Outcome gain per ADR §2. Signals outside the ADR's mapping are excluded
/// from the metric and counted, never guessed.
fn signal_gain(signal: &str) -> Option<f64> {
    match signal {
        "helpful" | "confirmation" => Some(1.0),
        "harmful" | "contradiction" => Some(-2.0),
        _ => None,
    }
}

struct QueryLabelGains {
    /// `1 / Σ|w·gain|` — candidate-independent per-query normalizer.
    norm: f64,
    /// `(memory_id, w·gain)` pairs.
    gains: Vec<(String, f64)>,
}

struct GroupedLabels {
    by_query: BTreeMap<String, QueryLabelGains>,
    labels_unmapped_signal: usize,
    queries_without_gain: usize,
}

fn group_label_gains(labels: &[LabeledTriple]) -> GroupedLabels {
    let mut raw: BTreeMap<String, Vec<(String, f64)>> = BTreeMap::new();
    let mut labels_unmapped_signal = 0_usize;
    for label in labels {
        let Some(gain) = signal_gain(&label.signal) else {
            labels_unmapped_signal += 1;
            continue;
        };
        raw.entry(label.query.clone())
            .or_default()
            .push((label.memory_id.clone(), label.weight * gain));
    }
    let mut by_query = BTreeMap::new();
    let mut queries_without_gain = 0_usize;
    for (query, gains) in raw {
        let denom: f64 = gains.iter().map(|(_, value)| value.abs()).sum();
        if denom <= f64::EPSILON {
            queries_without_gain += 1;
            continue;
        }
        by_query.insert(
            query,
            QueryLabelGains {
                norm: 1.0 / denom,
                gains,
            },
        );
    }
    GroupedLabels {
        by_query,
        labels_unmapped_signal,
        queries_without_gain,
    }
}

/// Re-fuse one replayed pool with a candidate vector using the production
/// adjustment, floor, and ordering, and return 1-based ranks by memory id.
fn ranked_pool(hits: &[SearchHit], weights: SearchFusionWeights) -> BTreeMap<String, usize> {
    let mut pool: Vec<SearchHit> = hits.to_vec();
    for hit in &mut pool {
        if let Some(adjustment) =
            configured_fusion_adjustment(hit, SearchSourceMode::Hybrid, weights)
        {
            hit.score = (hit.score * adjustment.multiplier).max(0.0);
        }
    }
    pool.retain(|hit| search_hit_meets_relevance_floor(hit, None));
    sort_search_hits_by_score_order(&mut pool);
    pool.into_iter()
        .enumerate()
        .map(|(index, hit)| (hit.doc_id, index + 1))
        .collect()
}

fn score_candidate(
    replays: &BTreeMap<&str, &QueryReplay>,
    grouped: &GroupedLabels,
    weights: SearchFusionWeights,
) -> f64 {
    let mut total = 0.0_f64;
    for (query, label_gains) in &grouped.by_query {
        let Some(replay) = replays.get(query.as_str()) else {
            // No replay pool for this query: every label is unranked and
            // contributes 0 (ADR §2).
            continue;
        };
        let ranks = ranked_pool(&replay.hits, weights);
        let mut query_score = 0.0_f64;
        for (memory_id, weighted_gain) in &label_gains.gains {
            if let Some(rank) = ranks.get(memory_id) {
                #[allow(clippy::cast_precision_loss)]
                let discount = (1.0 + *rank as f64).log2();
                query_score += weighted_gain / discount;
            }
        }
        total += label_gains.norm * query_score;
    }
    total
}

/// One evaluated candidate.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CandidateScore {
    pub weights: TuningWeights,
    pub score: f64,
    /// `incumbent`, `grid`, or `descent` — deterministic provenance.
    pub origin: &'static str,
}

/// Deterministic sweep result (ADR §2–3). The §4 evidence gate and the
/// `ee.shadow.retrieval_tuning_report.v1` envelope land with S3.
#[derive(Debug, Clone, PartialEq)]
pub struct TuningEvaluation {
    pub incumbent: CandidateScore,
    /// Every non-incumbent candidate in evaluation order.
    pub candidates: Vec<CandidateScore>,
    /// Best strictly-improving candidate, if any.
    pub winner: Option<CandidateScore>,
    /// `(winner - incumbent) / |incumbent|`; `None` when the incumbent score
    /// is too close to zero for a relative margin to be meaningful.
    pub relative_margin: Option<f64>,
    pub queries_scored: usize,
    pub queries_without_gain: usize,
    pub labels_unmapped_signal: usize,
    pub labels_total: usize,
    /// The graph fusion axis currently only moves scores through
    /// renormalization (`graph_component` is 0.0 in the live adjustment), so
    /// its deltas must not be read as real graph-signal tuning.
    pub graph_axis_degenerate: bool,
    /// `blake3:<hex>` over the full evaluation (candidates + scores).
    pub evaluation_hash: String,
}

fn enumerate_grid(incumbent: TuningWeights) -> Vec<TuningWeights> {
    let mut seen = BTreeSet::new();
    let mut vectors = Vec::new();
    let mut push = |candidate: TuningWeights, vectors: &mut Vec<TuningWeights>| {
        if seen.insert(candidate.key()) {
            vectors.push(candidate);
        }
    };
    push(incumbent.clamped(), &mut vectors);
    for axis in 0..3_usize {
        for offset in FUSION_GRID_OFFSETS {
            let mut candidate = incumbent;
            match axis {
                0 => candidate.lexical += offset,
                1 => candidate.semantic += offset,
                _ => candidate.graph += offset,
            }
            push(candidate.clamped(), &mut vectors);
        }
    }
    vectors
}

fn descent_neighbors(center: TuningWeights, step: f32) -> Vec<TuningWeights> {
    let mut neighbors = Vec::with_capacity(6);
    for axis in 0..3_usize {
        for direction in [-1.0_f32, 1.0] {
            let mut candidate = center;
            let delta = step * direction;
            match axis {
                0 => candidate.lexical += delta,
                1 => candidate.semantic += delta,
                _ => candidate.graph += delta,
            }
            neighbors.push(candidate.clamped());
        }
    }
    neighbors
}

fn evaluation_hash(
    incumbent: &CandidateScore,
    candidates: &[CandidateScore],
    labels_total: usize,
) -> String {
    let mut input = Vec::new();
    append_len_prefixed(&mut input, EVALUATION_HASH_DOMAIN.as_bytes());
    input.extend_from_slice(
        &u32::try_from(labels_total)
            .unwrap_or(u32::MAX)
            .to_be_bytes(),
    );
    let push_candidate = |candidate: &CandidateScore, input: &mut Vec<u8>| {
        append_len_prefixed(input, candidate.origin.as_bytes());
        input.extend_from_slice(&candidate.weights.lexical.to_bits().to_be_bytes());
        input.extend_from_slice(&candidate.weights.semantic.to_bits().to_be_bytes());
        input.extend_from_slice(&candidate.weights.graph.to_bits().to_be_bytes());
        input.extend_from_slice(&candidate.score.to_bits().to_be_bytes());
    };
    push_candidate(incumbent, &mut input);
    for candidate in candidates {
        push_candidate(candidate, &mut input);
    }
    format!("blake3:{}", blake3::hash(&input).to_hex())
}

/// Evaluate the incumbent plus the deterministic candidate set against the
/// replayed pools (ADR §2–3): fixed offset grid, then ≤2 rounds of bounded
/// coordinate descent with step halving around the best vector so far.
/// Cancellable between candidate evaluations; pure — no partial state.
pub fn evaluate_fusion_candidates(
    cx: &Cx,
    replays: &[QueryReplay],
    labels: &[LabeledTriple],
    incumbent: TuningWeights,
) -> Result<TuningEvaluation, ShadowTuningError> {
    shadow_checkpoint(cx)?;
    let replays_by_query: BTreeMap<&str, &QueryReplay> = replays
        .iter()
        .map(|replay| (replay.query.as_str(), replay))
        .collect();
    let grouped = group_label_gains(labels);

    let incumbent = incumbent.clamped();
    let incumbent_score = CandidateScore {
        weights: incumbent,
        score: score_candidate(&replays_by_query, &grouped, incumbent.to_fusion()),
        origin: "incumbent",
    };

    let mut evaluated: BTreeSet<(u32, u32, u32)> = BTreeSet::new();
    evaluated.insert(incumbent.key());
    let mut candidates: Vec<CandidateScore> = Vec::new();
    for weights in enumerate_grid(incumbent) {
        if !evaluated.insert(weights.key()) {
            continue;
        }
        shadow_checkpoint(cx)?;
        candidates.push(CandidateScore {
            weights,
            score: score_candidate(&replays_by_query, &grouped, weights.to_fusion()),
            origin: "grid",
        });
    }

    let mut best = candidates
        .iter()
        .copied()
        .fold(incumbent_score, |best, candidate| {
            if candidate.score > best.score {
                candidate
            } else {
                best
            }
        });
    for round in 0..DESCENT_MAX_ROUNDS {
        let step = DESCENT_INITIAL_STEP / 2.0_f32.powi(i32::try_from(round).unwrap_or(0));
        let mut improved = false;
        for weights in descent_neighbors(best.weights, step) {
            if !evaluated.insert(weights.key()) {
                continue;
            }
            shadow_checkpoint(cx)?;
            let candidate = CandidateScore {
                weights,
                score: score_candidate(&replays_by_query, &grouped, weights.to_fusion()),
                origin: "descent",
            };
            candidates.push(candidate);
            if candidate.score > best.score {
                best = candidate;
                improved = true;
            }
        }
        if !improved {
            break;
        }
    }

    let winner = (best.origin != "incumbent" && best.score > incumbent_score.score).then_some(best);
    let relative_margin = winner.and_then(|winner| {
        (incumbent_score.score.abs() > f64::EPSILON)
            .then(|| (winner.score - incumbent_score.score) / incumbent_score.score.abs())
    });
    let hash = evaluation_hash(&incumbent_score, &candidates, labels.len());

    Ok(TuningEvaluation {
        incumbent: incumbent_score,
        candidates,
        winner,
        relative_margin,
        queries_scored: grouped.by_query.len(),
        queries_without_gain: grouped.queries_without_gain,
        labels_unmapped_signal: grouped.labels_unmapped_signal,
        labels_total: labels.len(),
        graph_axis_degenerate: true,
        evaluation_hash: hash,
    })
}

// ================ S3: evidence gate + tuning report (ADR 0070 §4) ================

/// Schema id of the persisted tuning report (normative draft in ADR 0070's
/// appendix; honest diagnostics ride under `labelSet` and `diagnostics`).
pub const RETRIEVAL_TUNING_REPORT_SCHEMA_V1: &str = "ee.shadow.retrieval_tuning_report.v1";
/// Policy id registered in `SHADOW_POLICY_INVENTORY` (src/shadow.rs).
pub const RETRIEVAL_TUNING_POLICY_ID: &str = "candidate.retrieval.outcome_tuned_weights";
/// Abstention code (response_time class). The `degraded[]` emission and its
/// failure-mode fixture land with the CLI surface in bd-2tehh.3.
pub const INSUFFICIENT_OUTCOME_EVIDENCE_CODE: &str = "insufficient_outcome_evidence";
const REPORT_HASH_DOMAIN: &str = "ee.shadow.retrieval_tuning_report.hash.v1";

/// ADR §4 evidence gate. Tune the thresholds with data; never remove them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RetrievalTuningGateConfig {
    pub min_triples: usize,
    pub min_queries: usize,
    /// Minimum relative winner-over-incumbent margin for `promotable`.
    pub promote_margin: f64,
}

impl Default for RetrievalTuningGateConfig {
    fn default() -> Self {
        Self {
            min_triples: 50,
            min_queries: 15,
            promote_margin: 0.03,
        }
    }
}

/// Assembled tuning report (core shape; bd-2tehh.3 persists and renders it
/// through the shadow CLI surface).
#[derive(Debug, Clone, PartialEq)]
pub struct RetrievalTuningReport {
    pub db_generation: u64,
    pub labels: LabelExtractionReport,
    pub abstained: bool,
    pub abstention_reason: Option<&'static str>,
    /// Present exactly when the gate passed.
    pub evaluation: Option<TuningEvaluation>,
    pub promotable: bool,
    /// `blake3:<hex>` over the canonical report JSON (without this field).
    pub report_hash: String,
}

/// Apply the ADR §4 evidence gate and assemble the report.
///
/// `evaluation` must be `Some` exactly when the gate passes — the caller
/// runs the sweep only after the gate admits the label set (see
/// [`run_retrieval_tuning`]); a mismatch is reported as a storage-integrity
/// error rather than guessed around.
pub fn assemble_retrieval_tuning_report(
    labels: LabelExtractionReport,
    evaluation: Option<TuningEvaluation>,
    db_generation: u64,
    gate: &RetrievalTuningGateConfig,
) -> Result<RetrievalTuningReport, ShadowTuningError> {
    let abstained =
        labels.triples.len() < gate.min_triples || labels.distinct_queries < gate.min_queries;
    if abstained != evaluation.is_none() {
        return Err(ShadowTuningError::Storage {
            message: format!(
                "evidence gate and evaluation presence disagree (abstained={abstained}, evaluation={})",
                if evaluation.is_some() {
                    "present"
                } else {
                    "absent"
                }
            ),
        });
    }
    let promotable = !abstained
        && evaluation.as_ref().is_some_and(|evaluation| {
            evaluation.winner.is_some()
                && evaluation
                    .relative_margin
                    .is_some_and(|margin| margin >= gate.promote_margin)
        });
    let mut report = RetrievalTuningReport {
        db_generation,
        labels,
        abstained,
        abstention_reason: abstained.then_some(INSUFFICIENT_OUTCOME_EVIDENCE_CODE),
        evaluation,
        promotable,
        report_hash: String::new(),
    };
    let canonical = retrieval_tuning_report_json_value(&report, false).to_string();
    let mut input = Vec::new();
    append_len_prefixed(&mut input, REPORT_HASH_DOMAIN.as_bytes());
    append_len_prefixed(&mut input, canonical.as_bytes());
    report.report_hash = format!("blake3:{}", blake3::hash(&input).to_hex());
    Ok(report)
}

/// Full offline tuning pass: extract labels, gate, replay, sweep, assemble.
///
/// Read-only against the workspace; deterministic given the same database,
/// index, config, and `as_of`.
pub fn run_retrieval_tuning(
    cx: &Cx,
    connection: &DbConnection,
    workspace_path: &Path,
    database_path: &Path,
    workspace_id: &str,
    as_of: DateTime<Utc>,
    extraction: &LabelExtractionConfig,
    gate: &RetrievalTuningGateConfig,
) -> Result<RetrievalTuningReport, ShadowTuningError> {
    let labels = extract_labeled_triples(cx, connection, workspace_id, extraction, as_of)?;
    let db_generation = connection
        .get_workspace_generation(workspace_id)
        .map_err(|error| storage_error("read workspace generation", &error))?
        .unwrap_or(0);
    if labels.triples.len() < gate.min_triples || labels.distinct_queries < gate.min_queries {
        return assemble_retrieval_tuning_report(labels, None, db_generation, gate);
    }
    let queries: BTreeSet<String> = labels
        .triples
        .iter()
        .map(|triple| triple.query.clone())
        .collect();
    let replays = collect_query_replays(
        cx,
        connection,
        workspace_path,
        database_path,
        &queries,
        as_of,
    )?;
    let incumbent = TuningWeights::incumbent_for_workspace(workspace_path);
    let evaluation = evaluate_fusion_candidates(cx, &replays.replays, &labels.triples, incumbent)?;
    assemble_retrieval_tuning_report(labels, Some(evaluation), db_generation, gate)
}

fn weights_json(weights: TuningWeights) -> serde_json::Value {
    serde_json::json!({
        "lexical": f64::from(weights.lexical),
        "semantic": f64::from(weights.semantic),
        "graph": f64::from(weights.graph),
    })
}

fn candidate_json(candidate: &CandidateScore) -> serde_json::Value {
    serde_json::json!({
        "weights": weights_json(candidate.weights),
        "score": candidate.score,
        "origin": candidate.origin,
    })
}

fn retrieval_tuning_report_json_value(
    report: &RetrievalTuningReport,
    include_hash: bool,
) -> serde_json::Value {
    let labels = &report.labels;
    #[allow(clippy::cast_precision_loss)]
    let dense_share = if labels.triples.is_empty() {
        0.0
    } else {
        labels.dense_count as f64 / labels.triples.len() as f64
    };
    let incumbent = report
        .evaluation
        .as_ref()
        .map(|evaluation| candidate_json(&evaluation.incumbent));
    let candidates = report.evaluation.as_ref().map(|evaluation| {
        evaluation
            .candidates
            .iter()
            .map(candidate_json)
            .collect::<Vec<_>>()
    });
    let winner = report.evaluation.as_ref().and_then(|evaluation| {
        evaluation.winner.map(|winner| {
            let mut value = candidate_json(&winner);
            if let Some(object) = value.as_object_mut() {
                object.insert(
                    "relativeMargin".to_owned(),
                    evaluation
                        .relative_margin
                        .map_or(serde_json::Value::Null, serde_json::Value::from),
                );
            }
            value
        })
    });
    let mut value = serde_json::json!({
        "schema": RETRIEVAL_TUNING_REPORT_SCHEMA_V1,
        "policyId": RETRIEVAL_TUNING_POLICY_ID,
        "dbGeneration": report.db_generation,
        "labelSet": {
            "triples": labels.triples.len(),
            "distinctQueries": labels.distinct_queries,
            "hash": labels.label_set_hash,
            "denseShare": dense_share,
            "denseUnresolvable": labels.dense_unresolvable,
            "weakUnreplayable": labels.weak_unreplayable,
            "weakUnmatched": labels.weak_unmatched,
        },
        "abstained": report.abstained,
        "abstentionReason": report.abstention_reason,
        "incumbent": incumbent,
        "candidates": candidates,
        "winner": winner,
        "promotable": report.promotable,
        "diagnostics": report.evaluation.as_ref().map(|evaluation| {
            serde_json::json!({
                "evaluationHash": evaluation.evaluation_hash,
                "graphAxisDegenerate": evaluation.graph_axis_degenerate,
                "queriesScored": evaluation.queries_scored,
                "queriesWithoutGain": evaluation.queries_without_gain,
                "labelsUnmappedSignal": evaluation.labels_unmapped_signal,
            })
        }),
    });
    if include_hash {
        if let Some(object) = value.as_object_mut() {
            object.insert(
                "reportHash".to_owned(),
                serde_json::Value::from(report.report_hash.clone()),
            );
        }
    }
    value
}

/// Stable JSON rendering of the tuning report.
#[must_use]
pub fn render_retrieval_tuning_report_json(report: &RetrievalTuningReport) -> String {
    retrieval_tuning_report_json_value(report, true).to_string()
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

    // ===== S2: replay evaluator tests =====

    use crate::core::search::ScoreSource;

    fn hybrid_hit(
        doc_id: &str,
        raw_score: f32,
        lexical: Option<f32>,
        semantic: Option<f32>,
    ) -> SearchHit {
        SearchHit {
            doc_id: doc_id.to_owned(),
            score: raw_score,
            source: ScoreSource::Hybrid,
            fast_score: None,
            quality_score: semantic,
            lexical_score: lexical,
            rerank_score: None,
            metadata: None,
            explanation: None,
        }
    }

    fn triple(query: &str, memory_id: &str, signal: &str, weight: f64) -> LabeledTriple {
        LabeledTriple {
            query: query.to_owned(),
            memory_id: memory_id.to_owned(),
            signal: signal.to_owned(),
            base_weight: weight,
            weight,
            age_days: 0.0,
            source: LabelSource::PackItemOutcome,
            feedback_event_id: format!("fev-{memory_id}"),
            pack_record_id: Some("pack-1".to_owned()),
            audit_row_id: None,
        }
    }

    fn incumbent() -> TuningWeights {
        TuningWeights::compiled_defaults()
    }

    #[test]
    fn evaluator_hand_computed_metric_and_rank_flip_winner() -> TestResult {
        // mem-a is lexical-only (raw 0.0255), mem-b semantic-only (raw
        // 0.020). Flipping their order needs mult(b)/mult(a) > 0.0255/0.020
        // = 1.275. Hand-derived ratios per single-axis grid candidate:
        // lexical -0.10 → 1.0125/0.7875 ≈ 1.2857 (flips); semantic +0.10 →
        // 1.21/0.99 ≈ 1.2222 (does not); all smaller offsets are weaker, and
        // the graph axis rescales both arms identically (degenerate — never
        // flips). The single helpful label on mem-b therefore makes
        // lexical -0.10 the unique strict winner: incumbent = 1/log2(3),
        // winner = 1/log2(2) = 1.0, and descent cannot beat a perfect score
        // so the sweep stops after one non-improving round.
        let replays = [QueryReplay {
            query: "q1".to_owned(),
            hits: vec![
                hybrid_hit("mem-a", 0.0255, Some(1.0), None),
                hybrid_hit("mem-b", 0.020, None, Some(1.0)),
            ],
        }];
        let labels = [triple("q1", "mem-b", "helpful", 1.0)];
        let cx = Cx::for_testing();
        let evaluation = evaluate_fusion_candidates(&cx, &replays, &labels, incumbent())
            .map_err(|error| error.to_string())?;

        let expected_incumbent = 1.0 / 3.0_f64.log2();
        if !approx_eq(evaluation.incumbent.score, expected_incumbent) {
            return Err(format!(
                "incumbent metric must be 1/log2(3): {evaluation:?}"
            ));
        }
        let Some(winner) = evaluation.winner else {
            return Err(format!("lexical -0.10 vector must win: {evaluation:?}"));
        };
        if !approx_eq(winner.score, 1.0)
            || (f64::from(winner.weights.lexical) - 0.35).abs() > 1e-6
            || (f64::from(winner.weights.semantic) - 0.45).abs() > 1e-6
        {
            return Err(format!(
                "winner must be lexical -0.10 at score 1.0: {winner:?}"
            ));
        }
        let Some(margin) = evaluation.relative_margin else {
            return Err("winner must carry a relative margin".to_owned());
        };
        if margin <= 0.0 {
            return Err(format!("margin must be positive: {margin}"));
        }
        Ok(())
    }

    #[test]
    fn evaluator_is_deterministic_across_runs() -> TestResult {
        let replays = [QueryReplay {
            query: "q1".to_owned(),
            hits: vec![
                hybrid_hit("mem-a", 0.021, Some(1.0), None),
                hybrid_hit("mem-b", 0.020, None, Some(1.0)),
                hybrid_hit("mem-c", 0.015, Some(0.4), Some(0.4)),
            ],
        }];
        let labels = [
            triple("q1", "mem-a", "helpful", 0.8),
            triple("q1", "mem-b", "harmful", 0.5),
            triple("q1", "mem-c", "confirmation", 1.0),
        ];
        let cx = Cx::for_testing();
        let first = evaluate_fusion_candidates(&cx, &replays, &labels, incumbent())
            .map_err(|error| error.to_string())?;
        let second = evaluate_fusion_candidates(&cx, &replays, &labels, incumbent())
            .map_err(|error| error.to_string())?;
        if first != second {
            return Err("evaluation must be byte-identical across runs".to_owned());
        }
        if !first.evaluation_hash.starts_with("blake3:") {
            return Err(format!("hash must be prefixed: {}", first.evaluation_hash));
        }
        // A different label set must fingerprint differently.
        let third = evaluate_fusion_candidates(&cx, &replays, &labels[..1], incumbent())
            .map_err(|error| error.to_string())?;
        if third.evaluation_hash == first.evaluation_hash {
            return Err("different label sets must not share an evaluation hash".to_owned());
        }
        Ok(())
    }

    #[test]
    fn candidate_grid_respects_clamps_and_dedups() -> TestResult {
        // Near-boundary incumbent: +0.05 and +0.10 on lexical both clamp to
        // 0.7 and must collapse to one candidate; graph +0.10 clamps to 0.3;
        // semantic -0.10 clamps to 0.2.
        let near_edge = TuningWeights {
            lexical: 0.65,
            semantic: 0.25,
            graph: 0.25,
        };
        let vectors = enumerate_grid(near_edge);
        for vector in &vectors {
            if vector.lexical < FUSION_LEXICAL_CLAMP.0
                || vector.lexical > FUSION_LEXICAL_CLAMP.1
                || vector.semantic < FUSION_SEMANTIC_CLAMP.0
                || vector.semantic > FUSION_SEMANTIC_CLAMP.1
                || vector.graph < FUSION_GRAPH_CLAMP.0
                || vector.graph > FUSION_GRAPH_CLAMP.1
            {
                return Err(format!("clamp violated: {vector:?}"));
            }
        }
        let mut keys = BTreeSet::new();
        for vector in &vectors {
            if !keys.insert(vector.key()) {
                return Err(format!("duplicate candidate survived dedup: {vector:?}"));
            }
        }
        if vectors != enumerate_grid(near_edge) {
            return Err("grid enumeration must be deterministic".to_owned());
        }
        Ok(())
    }

    #[test]
    fn unmapped_signals_and_zero_gain_queries_are_counted_not_guessed() -> TestResult {
        let replays = [QueryReplay {
            query: "q1".to_owned(),
            hits: vec![hybrid_hit("mem-a", 0.02, Some(1.0), None)],
        }];
        // "stale" is outside the ADR gain mapping; the zero-weight helpful
        // label makes q2's normalizer denominator zero.
        let labels = [
            triple("q1", "mem-a", "stale", 1.0),
            triple("q2", "mem-a", "helpful", 0.0),
        ];
        let cx = Cx::for_testing();
        let evaluation = evaluate_fusion_candidates(&cx, &replays, &labels, incumbent())
            .map_err(|error| error.to_string())?;
        if evaluation.labels_unmapped_signal != 1
            || evaluation.queries_without_gain != 1
            || evaluation.queries_scored != 0
        {
            return Err(format!("honest counters wrong: {evaluation:?}"));
        }
        if !approx_eq(evaluation.incumbent.score, 0.0) || evaluation.winner.is_some() {
            return Err(format!(
                "no usable labels must mean zero scores and no winner: {evaluation:?}"
            ));
        }
        Ok(())
    }

    #[test]
    fn reranked_hits_outrank_fusion_hits_under_every_candidate() -> TestResult {
        let reranked = SearchHit {
            doc_id: "mem-reranked".to_owned(),
            score: 0.9,
            source: ScoreSource::Reranked,
            fast_score: None,
            quality_score: None,
            lexical_score: None,
            rerank_score: Some(0.9),
            metadata: None,
            explanation: None,
        };
        let hits = vec![
            hybrid_hit("mem-a", 0.021, Some(1.0), None),
            reranked,
            hybrid_hit("mem-b", 0.020, None, Some(1.0)),
        ];
        for weights in [
            TuningWeights {
                lexical: 0.7,
                semantic: 0.2,
                graph: 0.0,
            },
            TuningWeights {
                lexical: 0.2,
                semantic: 0.7,
                graph: 0.3,
            },
        ] {
            let ranks = ranked_pool(&hits, weights.to_fusion());
            if ranks.get("mem-reranked") != Some(&1) {
                return Err(format!(
                    "reranked hit must stay rank 1 under {weights:?}: {ranks:?}"
                ));
            }
        }
        Ok(())
    }

    #[test]
    fn cancelled_cx_aborts_evaluation_sweep() -> TestResult {
        let cx = Cx::for_testing();
        cx.set_cancel_reason(CancelReason::user("shadow tuning sweep cancellation test"));
        let replays = [QueryReplay {
            query: "q1".to_owned(),
            hits: vec![hybrid_hit("mem-a", 0.02, Some(1.0), None)],
        }];
        let labels = [triple("q1", "mem-a", "helpful", 1.0)];
        match evaluate_fusion_candidates(&cx, &replays, &labels, incumbent()) {
            Err(ShadowTuningError::Cancelled(_)) => Ok(()),
            other => Err(format!("cancelled Cx must abort the sweep: {other:?}")),
        }
    }

    // ===== S3: evidence gate + report tests =====

    fn flip_fixture_label_report() -> Result<LabelExtractionReport, String> {
        // One dense triple: query "q1", mem-b, helpful, weight 1.0.
        let created = ts(0);
        let events = [event(
            "fev-1",
            "mem-b",
            created,
            Some(pack_item_evidence("pack-1")),
        )];
        let pack_queries = BTreeMap::from([("pack-1".to_owned(), "q1".to_owned())]);
        join(&events, &[], &pack_queries, &BTreeMap::new(), created)
    }

    fn flip_fixture_replays() -> [QueryReplay; 1] {
        [QueryReplay {
            query: "q1".to_owned(),
            hits: vec![
                hybrid_hit("mem-a", 0.0255, Some(1.0), None),
                hybrid_hit("mem-b", 0.020, None, Some(1.0)),
            ],
        }]
    }

    #[test]
    fn evidence_gate_abstains_below_thresholds() -> TestResult {
        let labels = join(&[], &[], &BTreeMap::new(), &BTreeMap::new(), ts(0))?;
        let report = assemble_retrieval_tuning_report(
            labels,
            None,
            7,
            &RetrievalTuningGateConfig::default(),
        )
        .map_err(|error| error.to_string())?;
        if !report.abstained
            || report.abstention_reason != Some(INSUFFICIENT_OUTCOME_EVIDENCE_CODE)
            || report.promotable
            || report.evaluation.is_some()
        {
            return Err(format!("abstention shape wrong: {report:?}"));
        }
        let rendered: serde_json::Value =
            serde_json::from_str(&render_retrieval_tuning_report_json(&report))
                .map_err(|error| error.to_string())?;
        if rendered["schema"] != RETRIEVAL_TUNING_REPORT_SCHEMA_V1
            || rendered["abstained"] != true
            || !rendered["winner"].is_null()
            || rendered["promotable"] != false
            || rendered["dbGeneration"] != 7
        {
            return Err(format!("abstention rendering wrong: {rendered}"));
        }
        Ok(())
    }

    #[test]
    fn gate_pass_produces_promotable_report() -> TestResult {
        let cx = Cx::for_testing();
        let labels = flip_fixture_label_report()?;
        let evaluation =
            evaluate_fusion_candidates(&cx, &flip_fixture_replays(), &labels.triples, incumbent())
                .map_err(|error| error.to_string())?;
        let gate = RetrievalTuningGateConfig {
            min_triples: 1,
            min_queries: 1,
            promote_margin: 0.03,
        };
        let report = assemble_retrieval_tuning_report(labels, Some(evaluation), 3, &gate)
            .map_err(|error| error.to_string())?;
        if report.abstained || !report.promotable || report.abstention_reason.is_some() {
            return Err(format!("gate-pass shape wrong: {report:?}"));
        }
        let rendered: serde_json::Value =
            serde_json::from_str(&render_retrieval_tuning_report_json(&report))
                .map_err(|error| error.to_string())?;
        if rendered["policyId"] != RETRIEVAL_TUNING_POLICY_ID
            || rendered["promotable"] != true
            || rendered["labelSet"]["triples"] != 1
        {
            return Err(format!("gate-pass rendering wrong: {rendered}"));
        }
        let margin = rendered["winner"]["relativeMargin"]
            .as_f64()
            .ok_or("winner must carry relativeMargin")?;
        if margin <= 0.03 {
            return Err(format!("relative margin must clear the gate: {margin}"));
        }
        if rendered["reportHash"].as_str().map(str::to_owned) != Some(report.report_hash.clone()) {
            return Err("rendered reportHash must match the struct".to_owned());
        }
        Ok(())
    }

    #[test]
    fn report_hash_is_deterministic_and_content_bound() -> TestResult {
        let labels_a = join(&[], &[], &BTreeMap::new(), &BTreeMap::new(), ts(0))?;
        let labels_b = join(&[], &[], &BTreeMap::new(), &BTreeMap::new(), ts(0))?;
        let gate = RetrievalTuningGateConfig::default();
        let first = assemble_retrieval_tuning_report(labels_a, None, 1, &gate)
            .map_err(|error| error.to_string())?;
        let second = assemble_retrieval_tuning_report(labels_b.clone(), None, 1, &gate)
            .map_err(|error| error.to_string())?;
        if first.report_hash != second.report_hash {
            return Err("identical reports must share a hash".to_owned());
        }
        let generation_shifted = assemble_retrieval_tuning_report(labels_b, None, 2, &gate)
            .map_err(|error| error.to_string())?;
        if generation_shifted.report_hash == first.report_hash {
            return Err("dbGeneration must be hash-bound".to_owned());
        }
        Ok(())
    }

    #[test]
    fn gate_evaluation_mismatch_is_loud() -> TestResult {
        let cx = Cx::for_testing();
        let labels = flip_fixture_label_report()?;
        let evaluation =
            evaluate_fusion_candidates(&cx, &flip_fixture_replays(), &labels.triples, incumbent())
                .map_err(|error| error.to_string())?;
        // One triple is below the default 50-triple gate, so supplying an
        // evaluation anyway must fail loudly instead of being guessed around.
        match assemble_retrieval_tuning_report(
            labels,
            Some(evaluation),
            1,
            &RetrievalTuningGateConfig::default(),
        ) {
            Err(ShadowTuningError::Storage { .. }) => Ok(()),
            other => Err(format!("gate/evaluation mismatch must be loud: {other:?}")),
        }
    }
}
