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
    SearchDedupMode, SearchError, SearchFusionWeights, SearchHit, SearchOptions, SearchSourceMode,
    resolved_search_fusion_weights, run_search_with_read_connection_seeded_with_cx,
    search_hit_meets_relevance_floor, sort_search_hits_by_score_order,
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
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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
    /// The configured incumbent ranking could not be resolved.
    Configuration { message: String },
    /// Storage read or stored-state integrity failure.
    Storage { message: String },
}

impl std::fmt::Display for ShadowTuningError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cancelled(reason) => {
                write!(f, "shadow-tuning label extraction cancelled: {reason:?}")
            }
            Self::Configuration { message } => {
                write!(f, "shadow-tuning configuration error: {message}")
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
// passed directly to Frankensearch's weighted RRF surface. The evaluator
// therefore tunes those same upstream fusion weights, and the ADR's
// recency-tau axis is dropped: live search has no recency knob, and sweeping a
// parameter ranking ignores would report fake capability. The graph axis is
// kept but its sensitivity is degenerate today because no graph retrieval arm
// is attached to the live searcher; consumers of the evaluation must read
// `graph_axis_degenerate` honestly.
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
    pub fn incumbent_for_workspace(workspace_path: &Path) -> Result<Self, ShadowTuningError> {
        resolved_search_fusion_weights(workspace_path)
            .map(Self::from_fusion)
            .map_err(|error| ShadowTuningError::Configuration {
                message: error.to_string(),
            })
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
    /// Bounded final-pool hits retaining the lexical/semantic arm scores that
    /// Frankensearch exposes for native weighted-RRF replay.
    pub hits: Vec<SearchHit>,
}

/// Replay collection result with honest denominators.
#[derive(Debug, Clone)]
pub struct ReplayCollection {
    pub replays: Vec<QueryReplay>,
    /// Retained for the stable report contract. Native upstream RRF replay no
    /// longer needs to divide out a local multiplier, so this is always zero.
    pub unrecoverable_hits: usize,
}

/// Replay every distinct labeled query once against the current index.
///
/// Read-only by construction: the underlying search entry point passes no
/// audit connection, so no audit rows can be written. The relevance floor is
/// disabled at replay and re-applied per candidate offline, because floor
/// membership depends on the adjusted score each candidate produces.
///
/// Async on the caller's `Cx` so the whole tuning pipeline runs under ONE
/// runtime root — the sync search wrappers each start their own root, and
/// nesting roots from inside a running runtime is not a supported shape.
pub async fn collect_query_replays_with_cx(
    cx: &Cx,
    read_connection: &crate::db::DbConnection,
    workspace_path: &Path,
    database_path: &Path,
    queries: &BTreeSet<String>,
    as_of: DateTime<Utc>,
) -> Result<ReplayCollection, ShadowTuningError> {
    let determinism = Deterministic::from_seed(0);
    let mut replays = Vec::with_capacity(queries.len());

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
        let report = run_search_with_read_connection_seeded_with_cx(
            cx,
            &options,
            read_connection,
            determinism.shared_child("search.rerank"),
        )
        .await
        .map_err(|error| match error {
            SearchError::Configuration(message) => ShadowTuningError::Configuration { message },
            SearchError::Cancelled(reason) => ShadowTuningError::Cancelled(reason),
            error => storage_error("replay search failed", &error),
        })?;
        replays.push(QueryReplay {
            query: query.clone(),
            hits: report.results,
        });
    }

    Ok(ReplayCollection {
        replays,
        unrecoverable_hits: 0,
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

/// Re-fuse one replayed pool through Frankensearch's weighted RRF primitive,
/// apply the production relevance floor, and return 1-based ranks by memory.
#[allow(clippy::cast_possible_truncation)]
fn ranked_pool(hits: &[SearchHit], weights: SearchFusionWeights) -> BTreeMap<String, usize> {
    let mut reranked = hits
        .iter()
        .filter(|hit| hit.source == crate::core::search::ScoreSource::Reranked)
        .cloned()
        .collect::<Vec<_>>();
    reranked.retain(|hit| search_hit_meets_relevance_floor(hit, None));
    sort_search_hits_by_score_order(&mut reranked);

    let mut lexical = hits
        .iter()
        .filter(|hit| hit.source != crate::core::search::ScoreSource::Reranked)
        .filter_map(|hit| {
            hit.lexical_score
                .filter(|score| score.is_finite())
                .map(|score| crate::search::ScoredResult {
                    doc_id: hit.doc_id.clone().into(),
                    score,
                    source: crate::search::ScoreSource::Lexical,
                    index: None,
                    fast_score: None,
                    quality_score: None,
                    lexical_score: Some(score),
                    rerank_score: None,
                    explanation: None,
                    metadata: None,
                })
        })
        .collect::<Vec<_>>();
    lexical.sort_by(|left, right| {
        right
            .score
            .total_cmp(&left.score)
            .then_with(|| left.doc_id.cmp(&right.doc_id))
    });

    let mut semantic = hits
        .iter()
        .filter(|hit| hit.source != crate::core::search::ScoreSource::Reranked)
        .filter_map(|hit| {
            hit.quality_score
                .or(hit.fast_score)
                .filter(|score| score.is_finite())
                .map(|score| (hit.doc_id.clone(), score))
        })
        .collect::<Vec<_>>();
    semantic.sort_by(|left, right| {
        right
            .1
            .total_cmp(&left.1)
            .then_with(|| left.0.cmp(&right.0))
    });
    let semantic = semantic
        .into_iter()
        .enumerate()
        .map(
            |(index, (doc_id, score))| frankensearch::core::types::VectorHit {
                index: u32::try_from(index).unwrap_or(u32::MAX),
                score,
                doc_id: doc_id.into(),
            },
        )
        .collect::<Vec<_>>();

    let (lexical_weight, semantic_weight) = weights.upstream_rrf_weights();
    let config = frankensearch::RrfConfig {
        lexical_weight,
        semantic_weight,
        ..frankensearch::RrfConfig::default()
    };
    let by_id = hits
        .iter()
        .map(|hit| (hit.doc_id.as_str(), hit))
        .collect::<BTreeMap<_, _>>();
    let fused_ids = frankensearch::rrf_fuse(&lexical, &semantic, hits.len(), 0, &config)
        .into_iter()
        .filter_map(|fused| {
            let doc_id = fused.doc_id.to_string();
            let mut hit = (*by_id.get(doc_id.as_str())?).clone();
            hit.score = fused.rrf_score as f32;
            hit.source = crate::core::search::ScoreSource::Hybrid;
            search_hit_meets_relevance_floor(&hit, None).then_some(hit.doc_id)
        })
        .collect::<Vec<_>>();
    reranked
        .into_iter()
        .map(|hit| hit.doc_id)
        .chain(fused_ids)
        .enumerate()
        .map(|(index, doc_id)| (doc_id, index + 1))
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

/// Score one candidate weight vector against replayed pools and labels
/// (the ADR §2 outcome-weighted rank metric). Exposed for property tests
/// and diagnostic tooling; the sweep uses the same path internally.
#[must_use]
pub fn score_fusion_candidate(
    replays: &[QueryReplay],
    labels: &[LabeledTriple],
    weights: TuningWeights,
) -> f64 {
    let replays_by_query: BTreeMap<&str, &QueryReplay> = replays
        .iter()
        .map(|replay| (replay.query.as_str(), replay))
        .collect();
    let grouped = group_label_gains(labels);
    score_candidate(&replays_by_query, &grouped, weights.to_fusion())
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
    /// No graph retrieval arm is attached to the live searcher, so graph-axis
    /// deltas must not be read as real graph-signal tuning.
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
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
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
    /// Only the evaluator can bind a report to persisted source authority.
    /// This is deliberately absent from the stable public report JSON.
    authority: Option<PromotionAuthority>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct PromotionAuthority {
    schema: String,
    report_hash: String,
    workspace_id: String,
    workspace_path: String,
    database_path: String,
    database_schema: Option<u32>,
    config_hash: String,
    effective_config_hash: String,
    effective_context_hash: String,
    as_of: DateTime<Utc>,
    extraction: LabelExtractionConfig,
    gate: RetrievalTuningGateConfig,
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
    if gate.min_triples == 0
        || gate.min_queries == 0
        || !gate.promote_margin.is_finite()
        || gate.promote_margin < RetrievalTuningGateConfig::default().promote_margin
    {
        return Err(ShadowTuningError::Configuration {
            message: "retrieval tuning requires nonzero evidence thresholds and a promotion margin of at least 0.03".to_owned(),
        });
    }
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
        authority: None,
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
pub async fn run_retrieval_tuning_with_cx(
    cx: &Cx,
    connection: &DbConnection,
    workspace_path: &Path,
    database_path: &Path,
    workspace_id: &str,
    as_of: DateTime<Utc>,
    extraction: &LabelExtractionConfig,
    gate: &RetrievalTuningGateConfig,
) -> Result<RetrievalTuningReport, ShadowTuningError> {
    shadow_checkpoint(cx)?;
    if extraction.label_window_minutes == 0 {
        return Err(ShadowTuningError::Configuration {
            message: "retrieval tuning requires a nonzero label window".to_owned(),
        });
    }
    let authority = capture_promotion_authority(
        workspace_path,
        connection,
        workspace_id,
        as_of,
        extraction,
        gate,
    )?;
    let incumbent = TuningWeights::incumbent_for_workspace(workspace_path)?;
    let before_generation = connection
        .get_workspace_generation(workspace_id)
        .map_err(|error| storage_error("read workspace generation", &error))?
        .unwrap_or(0);
    let labels = extract_labeled_triples(cx, connection, workspace_id, extraction, as_of)?;
    let db_generation = connection
        .get_workspace_generation(workspace_id)
        .map_err(|error| storage_error("read workspace generation", &error))?
        .unwrap_or(0);
    let evaluation =
        if labels.triples.len() < gate.min_triples || labels.distinct_queries < gate.min_queries {
            None
        } else {
            let queries = labels
                .triples
                .iter()
                .map(|triple| triple.query.clone())
                .collect();
            let replays = collect_query_replays_with_cx(
                cx,
                connection,
                workspace_path,
                database_path,
                &queries,
                as_of,
            )
            .await?;
            Some(evaluate_fusion_candidates(
                cx,
                &replays.replays,
                &labels.triples,
                incumbent,
            )?)
        };
    let after_generation = connection
        .get_workspace_generation(workspace_id)
        .map_err(|error| storage_error("recheck workspace generation", &error))?
        .unwrap_or(0);
    let after_authority = capture_promotion_authority(
        workspace_path,
        connection,
        workspace_id,
        as_of,
        extraction,
        gate,
    )?;
    let after_labels = extract_labeled_triples(cx, connection, workspace_id, extraction, as_of)?;
    if before_generation != db_generation
        || after_generation != db_generation
        || after_authority != authority
        || after_labels != labels
    {
        return Err(storage_error(
            "evaluate retrieval weights",
            &"source or configuration changed during evaluation; rerun shadow run",
        ));
    }
    let mut report = assemble_retrieval_tuning_report(labels, evaluation, db_generation, gate)?;
    report.authority = Some(PromotionAuthority {
        report_hash: report.report_hash.clone(),
        ..authority
    });
    Ok(report)
}

/// Synchronous entry for CLI handlers: runs the full tuning pass under one
/// fresh runtime root (never nest this inside an existing root).
pub fn run_retrieval_tuning(
    workspace_path: &Path,
    database_path: &Path,
    workspace_id: &str,
    as_of: DateTime<Utc>,
    extraction: &LabelExtractionConfig,
    gate: &RetrievalTuningGateConfig,
) -> Result<RetrievalTuningReport, ShadowTuningError> {
    crate::core::run_cli_with_cx(std::time::Duration::from_secs(600), |cx| async move {
        let connection =
            DbConnection::open_file(database_path).map_err(|error| ShadowTuningError::Storage {
                message: format!("open workspace database: {error}"),
            })?;
        run_retrieval_tuning_with_cx(
            &cx,
            &connection,
            workspace_path,
            database_path,
            workspace_id,
            as_of,
            extraction,
            gate,
        )
        .await
    })
    .map_err(|error| ShadowTuningError::Storage {
        message: format!("start shadow-tuning runtime: {error}"),
    })?
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

// ============ S4: promotion mechanics (ADR 0070 §5, bd-2tehh.3) ============
//
// Live ranking changes only through an evaluated, database-attested receipt.
// Promotion binds source evidence and effective configuration; an audit intent
// precedes atomic config publication. Demotion restores exact original bytes
// when untouched and only owned fields when unrelated settings changed. A
// previously absent config restores as empty, never as a file deletion.

/// Persisted report location under the workspace store directory.
pub const RETRIEVAL_TUNING_REPORT_FILENAME: &str = "retrieval_tuning_report.json";
pub const PROMOTE_RETRIEVAL_WEIGHTS_AUDIT_ACTION: &str = "shadow.promote_retrieval_weights";
pub const DEMOTE_RETRIEVAL_WEIGHTS_AUDIT_ACTION: &str = "shadow.demote_retrieval_weights";
const OVERLAY_INTENT_AUDIT_ACTION: &str = "shadow.retrieval_weights_intent";
const EVALUATION_AUDIT_ACTION: &str = "shadow.retrieval_tuning_evaluated";
const PROMOTION_AUTHORITY_SCHEMA: &str = "ee.shadow.retrieval_promotion_authority.v1";
const OWNED_WEIGHT_KEYS: [&str; 3] = ["lexical_weight", "semantic_weight", "graph_weight"];

/// `<workspace>/.ee/shadow/retrieval_tuning_report.json`.
#[must_use]
pub fn shadow_report_path(workspace_path: &Path) -> std::path::PathBuf {
    workspace_path
        .join(".ee")
        .join("shadow")
        .join(RETRIEVAL_TUNING_REPORT_FILENAME)
}

/// Persist the rendered report for the promote step to validate later.
pub fn persist_retrieval_tuning_report(
    workspace_path: &Path,
    report: &RetrievalTuningReport,
) -> Result<std::path::PathBuf, ShadowTuningError> {
    let authority = report.authority.as_ref().ok_or_else(|| {
        storage_error(
            "persist shadow report",
            &"report has no evaluator source receipt; rerun shadow run",
        )
    })?;
    if authority.report_hash != report.report_hash {
        return Err(storage_error(
            "persist shadow report",
            &"report and source receipt disagree",
        ));
    }
    let workspace = workspace_path
        .canonicalize()
        .map_err(|error| storage_error("bind report destination", &error))?;
    if workspace.to_string_lossy().as_ref() != authority.workspace_path.as_str() {
        return Err(storage_error(
            "persist shadow report",
            &"report belongs to a different workspace",
        ));
    }
    let receipt = serde_json::to_string(authority)
        .map_err(|error| storage_error("encode shadow source receipt", &error))?;
    let authority_hash = content_digest(&receipt);
    let connection = DbConnection::open_file(&authority.database_path)
        .map_err(|error| storage_error("open tuning receipt database", &error))?;
    connection.with_write_owner_fence(
        |error| storage_error("own tuning receipt publication", &error),
        || {
            // The typed evaluator result owns the receipt. Files alone cannot
            // manufacture this append-only database attestation by recomputing
            // checksums over invented scores or weakened evidence thresholds.
            if !evaluation_attested(&connection, &authority.workspace_id, authority)? {
                insert_overlay_audit(
                    &connection,
                    &authority.workspace_id,
                    EVALUATION_AUDIT_ACTION,
                    &serde_json::json!({
                        "schema": "ee.shadow.retrieval_evaluation_receipt.v1",
                        "policyId": RETRIEVAL_TUNING_POLICY_ID,
                        "reportHash": report.report_hash,
                        "authorityHash": authority_hash,
                    }),
                )?;
            }
            let path = shadow_report_path(workspace_path);
            atomic_shadow_write(&path, &render_retrieval_tuning_report_json(report))?;
            atomic_shadow_write(&authority_path(workspace_path), &receipt)?;
            Ok(path)
        },
    )
}

/// Typed promote refusal (exit-7 material at the CLI).
#[derive(Debug, Clone, PartialEq)]
pub enum PromoteRefusal {
    ReportMissing { path: String },
    ReportInvalid { reason: String },
    Abstained,
    NotPromotable { relative_margin: Option<f64> },
    StaleGeneration { report: u64, current: u64 },
    NoPriorPromotion,
}

impl PromoteRefusal {
    #[must_use]
    pub fn message(&self) -> String {
        match self {
            Self::ReportMissing { path } => {
                format!("no persisted tuning report at {path}; run ee shadow run first")
            }
            Self::ReportInvalid { reason } => format!("persisted tuning report unusable: {reason}"),
            Self::Abstained => {
                "the persisted report abstained (insufficient outcome evidence); nothing to promote"
                    .to_owned()
            }
            Self::NotPromotable { relative_margin } => format!(
                "the persisted report is not promotable (relative margin {:?} below the gate or no strict winner)",
                relative_margin
            ),
            Self::StaleGeneration { report, current } => format!(
                "the persisted report was evaluated at db generation {report} but the workspace is now at {current}; re-run ee shadow run"
            ),
            Self::NoPriorPromotion => {
                "no prior promotion audit found for this workspace; nothing to demote".to_owned()
            }
        }
    }
}

/// Outcome of a promote/demote apply (or dry-run plan).
#[derive(Debug, Clone, PartialEq)]
pub struct OverlayChange {
    pub applied: bool,
    /// Full prior config.toml bytes ("" when the file did not exist).
    pub prior_config: String,
    pub new_config: String,
    /// Human-readable per-key diff lines.
    pub diff: Vec<String>,
    pub report_hash: Option<String>,
}

fn workspace_config_path(workspace_path: &Path) -> std::path::PathBuf {
    workspace_path.join(".ee").join("config.toml")
}

fn read_config_bytes(workspace_path: &Path) -> Result<(String, bool), ShadowTuningError> {
    read_shadow_file(&workspace_config_path(workspace_path))
        .map(|contents| contents.map_or_else(|| (String::new(), false), |text| (text, true)))
}

fn read_shadow_file(path: &Path) -> Result<Option<String>, ShadowTuningError> {
    crate::core::config_surface::read_optional_config_contents(path)
        .map_err(|error| storage_error("read shadow state", &error))
}

fn authority_path(workspace: &Path) -> std::path::PathBuf {
    workspace.join(".ee/shadow/retrieval_tuning_authority.json")
}

fn content_digest(text: &str) -> String {
    format!("blake3:{}", blake3::hash(text.as_bytes()).to_hex())
}

fn config_digest(text: &str, _existed: bool) -> String {
    let mut hash = blake3::Hasher::new();
    hash.update(b"ee.shadow.config.v1\0");
    hash.update(text.as_bytes());
    format!("blake3:{}", hash.finalize().to_hex())
}

fn capture_promotion_authority(
    workspace: &Path,
    connection: &DbConnection,
    workspace_id: &str,
    as_of: DateTime<Utc>,
    extraction: &LabelExtractionConfig,
    gate: &RetrievalTuningGateConfig,
) -> Result<PromotionAuthority, ShadowTuningError> {
    let (config, existed) = read_config_bytes(workspace)?;
    let effective_config_hash = effective_config_digest(workspace)?;
    let workspace = workspace
        .canonicalize()
        .map_err(|error| storage_error("bind tuning workspace", &error))?;
    let crate::db::DatabaseLocation::File(database) = connection.location() else {
        return Err(storage_error(
            "bind tuning database",
            &"persisted tuning requires a file database",
        ));
    };
    let database = database
        .canonicalize()
        .map_err(|error| storage_error("bind tuning database", &error))?;
    Ok(PromotionAuthority {
        schema: PROMOTION_AUTHORITY_SCHEMA.to_owned(),
        report_hash: String::new(),
        workspace_id: workspace_id.to_owned(),
        workspace_path: workspace.to_string_lossy().into_owned(),
        database_path: database.to_string_lossy().into_owned(),
        database_schema: connection
            .schema_version()
            .map_err(|error| storage_error("bind tuning schema", &error))?,
        config_hash: config_digest(&config, existed),
        effective_config_hash,
        effective_context_hash: effective_context_digest(workspace.as_path())?,
        as_of,
        extraction: extraction.clone(),
        gate: *gate,
    })
}

fn effective_config_digest(workspace: &Path) -> Result<String, ShadowTuningError> {
    let effective =
        crate::core::config_surface::merged_workspace_config(workspace).map_err(|error| {
            ShadowTuningError::Configuration {
                message: error.to_string(),
            }
        })?;
    let rendered = serde_json::to_string(&effective.to_show_report())
        .map_err(|error| storage_error("fingerprint tuning configuration", &error))?;
    Ok(content_digest(&rendered))
}

fn effective_context_digest(workspace: &Path) -> Result<String, ShadowTuningError> {
    let effective =
        crate::core::config_surface::merged_workspace_config(workspace).map_err(|error| {
            ShadowTuningError::Configuration {
                message: error.to_string(),
            }
        })?;
    let entries: Vec<_> = effective
        .to_show_report()
        .entries
        .into_iter()
        .filter(|entry| {
            !matches!(
                entry.key,
                "search.lexical_weight" | "search.semantic_weight" | "search.graph_weight"
            )
        })
        .collect();
    let rendered = serde_json::to_string(&entries)
        .map_err(|error| storage_error("fingerprint tuning context", &error))?;
    Ok(content_digest(&rendered))
}

/// Publish complete bytes via the same no-follow, bounded config path checks
/// as `ee config set`. Failed temporaries are retained rather than deleted.
fn atomic_shadow_write(path: &Path, text: &str) -> Result<(), ShadowTuningError> {
    atomic_shadow_write_expected(path, text, None)
}

fn atomic_shadow_write_expected(
    path: &Path,
    text: &str,
    expected: Option<&str>,
) -> Result<(), ShadowTuningError> {
    atomic_shadow_write_authorized(path, text, expected, None, || {})
}

fn atomic_shadow_write_authorized(
    path: &Path,
    text: &str,
    expected: Option<&str>,
    authority: Option<&PromotionAuthority>,
    before_publish: impl FnOnce(),
) -> Result<(), ShadowTuningError> {
    use std::io::Write as _;

    let _ = read_shadow_file(path)?;
    let parent = path
        .parent()
        .ok_or_else(|| storage_error("write shadow state", &"missing parent"))?;
    std::fs::create_dir_all(parent)
        .map_err(|error| storage_error("create shadow state directory", &error))?;
    let _ = read_shadow_file(path)?;
    let temporary = parent.join(format!(".shadow-{}.tmp", crate::db::generate_audit_id()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options
        .open(&temporary)
        .map_err(|error| storage_error("create shadow state temporary", &error))?;
    if let Ok(metadata) = std::fs::symlink_metadata(path) {
        file.set_permissions(metadata.permissions())
            .map_err(|error| storage_error("preserve shadow state permissions", &error))?;
    }
    file.write_all(text.as_bytes())
        .and_then(|()| file.sync_all())
        .map_err(|error| storage_error("write shadow state temporary", &error))?;
    before_publish();
    if let Some(authority) = authority {
        if effective_config_digest(Path::new(&authority.workspace_path))?
            != authority.effective_config_hash
        {
            return Err(storage_error(
                "publish shadow state",
                &"effective configuration changed before publication; no replacement made",
            ));
        }
    }
    if let Some(expected) = expected {
        if read_shadow_file(path)?.as_deref().unwrap_or("") != expected {
            return Err(storage_error(
                "publish shadow state",
                &"configuration changed before publication; no replacement made",
            ));
        }
    }
    crate::core::config_surface::publish_config_temp_file(path, &temporary)
        .map_err(|error| storage_error("publish shadow state", &error))
}

fn invalid_report(reason: impl Into<String>) -> PromoteRefusal {
    PromoteRefusal::ReportInvalid {
        reason: reason.into(),
    }
}

fn parsed_candidate(
    value: &serde_json::Value,
    incumbent: bool,
) -> Result<CandidateScore, PromoteRefusal> {
    let origin = match value.get("origin").and_then(serde_json::Value::as_str) {
        Some("incumbent") if incumbent => "incumbent",
        Some("grid") if !incumbent => "grid",
        Some("descent") if !incumbent => "descent",
        _ => return Err(invalid_report("candidate origin is invalid")),
    };
    let weights = value
        .get("weights")
        .ok_or_else(|| invalid_report("candidate has no weights"))?;
    let read_weight = |key: &str, bounds: (f32, f32)| {
        let number = weights
            .get(key)
            .and_then(serde_json::Value::as_f64)
            .ok_or_else(|| invalid_report("candidate weight is not numeric"))?;
        let bounds = if incumbent { (0.0, 1.0) } else { bounds };
        if !number.is_finite() || number < f64::from(bounds.0) || number > f64::from(bounds.1) {
            return Err(invalid_report("candidate weight exceeds the policy bounds"));
        }
        #[allow(clippy::cast_possible_truncation)]
        let number = number as f32;
        Ok(number)
    };
    let score = value
        .get("score")
        .and_then(serde_json::Value::as_f64)
        .filter(|score| score.is_finite())
        .ok_or_else(|| invalid_report("candidate score is not finite"))?;
    Ok(CandidateScore {
        weights: TuningWeights {
            lexical: read_weight("lexical", FUSION_LEXICAL_CLAMP)?,
            semantic: read_weight("semantic", FUSION_SEMANTIC_CLAMP)?,
            graph: read_weight("graph", FUSION_GRAPH_CLAMP)?,
        },
        score,
        origin,
    })
}

/// Reconstruct the same bounded grid/descent walk. A self-consistent hash on a
/// hand-edited winner is not an evaluated policy: every vector and its origin
/// must still belong to the actual evaluator's deterministic search.
fn validate_candidate_walk(
    incumbent: CandidateScore,
    candidates: &[CandidateScore],
) -> Result<CandidateScore, PromoteRefusal> {
    let mut seen = BTreeSet::from([incumbent.weights.key()]);
    let mut position = 0;
    let mut take = |weights: TuningWeights, origin: &'static str| {
        if !seen.insert(weights.key()) {
            return Ok(None);
        }
        let candidate = candidates
            .get(position)
            .ok_or_else(|| invalid_report("evaluation is missing a candidate"))?;
        if candidate.weights != weights || candidate.origin != origin {
            return Err(invalid_report(
                "candidate order or origin differs from the evaluator",
            ));
        }
        position += 1;
        Ok(Some(*candidate))
    };
    let mut best = incumbent;
    for weights in enumerate_grid(incumbent.weights) {
        if let Some(candidate) = take(weights, "grid")? {
            if candidate.score > best.score {
                best = candidate;
            }
        }
    }
    for round in 0..DESCENT_MAX_ROUNDS {
        let step = DESCENT_INITIAL_STEP / 2.0_f32.powi(i32::try_from(round).unwrap_or(0));
        let mut improved = false;
        for weights in descent_neighbors(best.weights, step) {
            if let Some(candidate) = take(weights, "descent")? {
                if candidate.score > best.score {
                    best = candidate;
                    improved = true;
                }
            }
        }
        if !improved {
            break;
        }
    }
    if position != candidates.len() {
        return Err(invalid_report("evaluation contains extra candidates"));
    }
    Ok(best)
}

fn validated_report(
    report: &serde_json::Value,
    authority: &PromotionAuthority,
) -> Result<(CandidateScore, CandidateScore, u64), PromoteRefusal> {
    let mut unhashed = report.clone();
    let object = unhashed
        .as_object_mut()
        .ok_or_else(|| invalid_report("report is not an object"))?;
    let hash = object
        .remove("reportHash")
        .and_then(|hash| hash.as_str().map(str::to_owned))
        .ok_or_else(|| invalid_report("report hash is missing"))?;
    let mut bytes = Vec::new();
    append_len_prefixed(&mut bytes, REPORT_HASH_DOMAIN.as_bytes());
    append_len_prefixed(&mut bytes, unhashed.to_string().as_bytes());
    let expected_hash = format!("blake3:{}", blake3::hash(&bytes).to_hex());
    if hash != expected_hash
        || hash != authority.report_hash
        || report.get("schema").and_then(serde_json::Value::as_str)
            != Some(RETRIEVAL_TUNING_REPORT_SCHEMA_V1)
        || report.get("policyId").and_then(serde_json::Value::as_str)
            != Some(RETRIEVAL_TUNING_POLICY_ID)
        || authority.schema != PROMOTION_AUTHORITY_SCHEMA
    {
        return Err(invalid_report(
            "report hash, policy or evaluator receipt does not match",
        ));
    }
    if report.get("abstained").and_then(serde_json::Value::as_bool) == Some(true) {
        return Err(PromoteRefusal::Abstained);
    }
    if report.get("abstained").and_then(serde_json::Value::as_bool) != Some(false)
        || !report
            .get("abstentionReason")
            .is_some_and(serde_json::Value::is_null)
        || authority.gate.min_triples == 0
        || authority.gate.min_queries == 0
        || !authority.gate.promote_margin.is_finite()
        || authority.gate.promote_margin < RetrievalTuningGateConfig::default().promote_margin
        || authority.extraction.label_window_minutes == 0
    {
        return Err(invalid_report("evaluator evidence gate is invalid"));
    }
    let count = |pointer| {
        report
            .pointer(pointer)
            .and_then(serde_json::Value::as_u64)
            .and_then(|count| usize::try_from(count).ok())
            .ok_or_else(|| invalid_report("evidence count is missing or invalid"))
    };
    let triples = count("/labelSet/triples")?;
    let queries = count("/labelSet/distinctQueries")?;
    if triples < authority.gate.min_triples
        || queries < authority.gate.min_queries
        || queries > triples
    {
        return Err(invalid_report("report does not satisfy its evidence gate"));
    }
    let incumbent = parsed_candidate(&report["incumbent"], true)?;
    let values = report
        .get("candidates")
        .and_then(serde_json::Value::as_array)
        .filter(|candidates| candidates.len() <= 64)
        .ok_or_else(|| invalid_report("evaluation candidate set is missing or oversized"))?;
    let candidates = values
        .iter()
        .map(|value| parsed_candidate(value, false))
        .collect::<Result<Vec<_>, _>>()?;
    if report
        .pointer("/diagnostics/evaluationHash")
        .and_then(serde_json::Value::as_str)
        != Some(evaluation_hash(&incumbent, &candidates, triples).as_str())
    {
        return Err(invalid_report(
            "evaluation hash does not match its candidates",
        ));
    }
    let best = validate_candidate_walk(incumbent, &candidates)?;
    let winner = parsed_candidate(&report["winner"], false)?;
    let margin = report
        .pointer("/winner/relativeMargin")
        .and_then(serde_json::Value::as_f64);
    let expected_margin = (incumbent.score.abs() > f64::EPSILON)
        .then(|| (best.score - incumbent.score) / incumbent.score.abs());
    if report
        .get("promotable")
        .and_then(serde_json::Value::as_bool)
        != Some(true)
        || best != winner
        || winner.score <= incumbent.score
        || margin != expected_margin
        || !margin.is_some_and(|margin| margin >= authority.gate.promote_margin)
    {
        return Err(PromoteRefusal::NotPromotable {
            relative_margin: margin,
        });
    }
    let generation = report
        .get("dbGeneration")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| invalid_report("database generation is missing"))?;
    Ok((incumbent, winner, generation))
}

/// Validate the persisted report and apply the `[search]` fusion-weight
/// overlay (ADR 0070 §5). Policy refusals come back as `Ok(Err(refusal))`;
/// storage failures are hard errors.
#[allow(clippy::type_complexity)]
pub fn promote_retrieval_weights(
    workspace_path: &Path,
    connection: &DbConnection,
    workspace_id: &str,
    dry_run: bool,
) -> Result<Result<OverlayChange, PromoteRefusal>, ShadowTuningError> {
    connection.with_write_owner_fence(
        |error| storage_error("own retrieval promotion", &error),
        || promote_retrieval_weights_owned(workspace_path, connection, workspace_id, dry_run),
    )
}

#[allow(clippy::type_complexity)]
fn promote_retrieval_weights_owned(
    workspace_path: &Path,
    connection: &DbConnection,
    workspace_id: &str,
    dry_run: bool,
) -> Result<Result<OverlayChange, PromoteRefusal>, ShadowTuningError> {
    let report_path = shadow_report_path(workspace_path);
    let raw = match read_shadow_file(&report_path)? {
        Some(raw) => raw,
        None => {
            return Ok(Err(PromoteRefusal::ReportMissing {
                path: report_path.display().to_string(),
            }));
        }
    };
    let report: serde_json::Value = match serde_json::from_str(&raw) {
        Ok(value) => value,
        Err(error) => {
            return Ok(Err(PromoteRefusal::ReportInvalid {
                reason: format!("not valid JSON: {error}"),
            }));
        }
    };
    let Some(receipt) = read_shadow_file(&authority_path(workspace_path))? else {
        return Ok(Err(invalid_report(
            "evaluator source receipt is missing; rerun shadow run",
        )));
    };
    let authority: PromotionAuthority = match serde_json::from_str(&receipt) {
        Ok(authority) => authority,
        Err(_) => return Ok(Err(invalid_report("evaluator source receipt is malformed"))),
    };
    let (incumbent, winner, report_generation) = match validated_report(&report, &authority) {
        Ok(validated) => validated,
        Err(refusal) => return Ok(Err(refusal)),
    };
    let current_generation = connection
        .get_workspace_generation(workspace_id)
        .map_err(|error| storage_error("read workspace generation", &error))?
        .unwrap_or(0);
    let (prior_config, prior_existed) = read_config_bytes(workspace_path)?;
    let mut current_authority = capture_promotion_authority(
        workspace_path,
        connection,
        workspace_id,
        authority.as_of,
        &authority.extraction,
        &authority.gate,
    )?;
    current_authority.report_hash = authority.report_hash.clone();
    if current_authority.workspace_id != authority.workspace_id
        || current_authority.workspace_path != authority.workspace_path
        || current_authority.database_path != authority.database_path
        || current_authority.database_schema < authority.database_schema
    {
        return Ok(Err(invalid_report(
            "receipt belongs to a different workspace or database",
        )));
    }
    if !evaluation_attested(connection, workspace_id, &authority)? {
        return Ok(Err(invalid_report(
            "evaluation receipt is not attested by this database; rerun shadow run",
        )));
    }
    // Finishing a transition already proved by its durable intent is recovery,
    // not a new policy decision. Later memory/outcome writes must not strand a
    // published overlay just because its evaluation is no longer fresh.
    if let Some(replay) =
        recover_promotion(connection, workspace_id, &authority, &prior_config, dry_run)?
    {
        return Ok(Ok(replay));
    }
    if report_generation != current_generation {
        return Ok(Err(PromoteRefusal::StaleGeneration {
            report: report_generation,
            current: current_generation,
        }));
    }
    let extraction = &authority.extraction;
    let as_of = authority.as_of;
    let labels =
        crate::core::run_cli_with_cx(std::time::Duration::from_secs(30), |cx| async move {
            extract_labeled_triples(&cx, connection, workspace_id, extraction, as_of)
        })
        .map_err(|error| storage_error("recheck tuning evidence", &error))??;
    if report
        .pointer("/labelSet/hash")
        .and_then(serde_json::Value::as_str)
        != Some(labels.label_set_hash.as_str())
        || report
            .pointer("/labelSet/triples")
            .and_then(serde_json::Value::as_u64)
            != u64::try_from(labels.triples.len()).ok()
        || report
            .pointer("/labelSet/distinctQueries")
            .and_then(serde_json::Value::as_u64)
            != u64::try_from(labels.distinct_queries).ok()
    {
        return Ok(Err(invalid_report(
            "outcome evidence changed after evaluation; rerun shadow run",
        )));
    }
    if current_authority != authority {
        return Ok(Err(invalid_report(
            "workspace or effective configuration changed after evaluation; rerun shadow run",
        )));
    }
    if TuningWeights::incumbent_for_workspace(workspace_path)? != incumbent.weights {
        return Ok(Err(invalid_report(
            "report incumbent differs from the live retrieval weights",
        )));
    }
    let lexical = f64::from(winner.weights.lexical);
    let semantic = f64::from(winner.weights.semantic);
    let graph = f64::from(winner.weights.graph);
    let report_hash = Some(authority.report_hash.clone());
    let mut document = prior_config
        .parse::<toml_edit::DocumentMut>()
        .map_err(|error| {
            storage_error(
                "parse workspace config.toml",
                &crate::config::file::toml_syntax_error_summary(&prior_config, &error),
            )
        })?;
    let mut diff = Vec::new();
    {
        let search_item = document.entry("search").or_insert(toml_edit::table());
        let Some(search) = search_item.as_table_like_mut() else {
            return Ok(Err(PromoteRefusal::ReportInvalid {
                reason: "[search] in config.toml is not a table".to_owned(),
            }));
        };
        for (key, value) in [
            ("lexical_weight", lexical),
            ("semantic_weight", semantic),
            ("graph_weight", graph),
        ] {
            let prior = search
                .get(key)
                .and_then(toml_edit::Item::as_value)
                .map(std::string::ToString::to_string);
            diff.push(format!(
                "search.{key}: {} -> {value}",
                prior.as_deref().unwrap_or("(unset)")
            ));
            search.insert(key, toml_edit::value(value));
        }
    }
    let new_config = document.to_string();

    let change = OverlayChange {
        applied: !dry_run,
        prior_config: prior_config.clone(),
        new_config: new_config.clone(),
        diff,
        report_hash: report_hash.clone(),
    };
    if dry_run {
        return Ok(Ok(change));
    }

    let details = serde_json::json!({
        "schema": "ee.shadow.retrieval_weights_promotion.v1",
        "policyId": RETRIEVAL_TUNING_POLICY_ID,
        "workspacePath": authority.workspace_path,
        "databasePath": authority.database_path,
        "databaseSchema": authority.database_schema,
        "reportHash": report_hash,
        "priorExisted": prior_existed,
        "priorConfigToml": prior_config,
        "newConfigToml": new_config,
        "priorConfigHash": content_digest(&change.prior_config),
        "newConfigHash": content_digest(&change.new_config),
        "overlayHash": content_digest(&weights_json(winner.weights).to_string()),
        "authorityHash": content_digest(&serde_json::to_string(&authority).map_err(|error| storage_error("encode tuning authority", &error))?),
        "newValues": { "lexical": lexical, "semantic": semantic, "graph": graph },
    });
    publish_audited_overlay(
        workspace_path,
        connection,
        workspace_id,
        &change,
        details,
        Some(&authority),
        PROMOTE_RETRIEVAL_WEIGHTS_AUDIT_ACTION,
    )?;
    Ok(Ok(change))
}

fn overlay_audits(
    connection: &DbConnection,
    workspace_id: &str,
    action: &str,
) -> Result<Vec<StoredAuditEntry>, ShadowTuningError> {
    let mut audits = connection
        .list_audit_by_action(action, None)
        .map_err(|error| storage_error("read retrieval overlay history", &error))?;
    audits.retain(|entry| entry.workspace_id.as_deref() == Some(workspace_id));
    audits.sort_by(|left, right| {
        right
            .timestamp
            .cmp(&left.timestamp)
            .then_with(|| right.id.cmp(&left.id))
    });
    Ok(audits)
}

fn overlay_details(entry: &StoredAuditEntry) -> Option<serde_json::Value> {
    serde_json::from_str(entry.details.as_deref()?).ok()
}

fn evaluation_attested(
    connection: &DbConnection,
    workspace_id: &str,
    authority: &PromotionAuthority,
) -> Result<bool, ShadowTuningError> {
    let hash = content_digest(
        &serde_json::to_string(authority)
            .map_err(|error| storage_error("encode evaluation authority", &error))?,
    );
    evaluation_hashes_attested(connection, workspace_id, &authority.report_hash, &hash)
}

fn evaluation_hashes_attested(
    connection: &DbConnection,
    workspace_id: &str,
    report_hash: &str,
    authority_hash: &str,
) -> Result<bool, ShadowTuningError> {
    Ok(
        overlay_audits(connection, workspace_id, EVALUATION_AUDIT_ACTION)?
            .iter()
            .filter_map(overlay_details)
            .any(|receipt| {
                receipt.get("schema").and_then(serde_json::Value::as_str)
                    == Some("ee.shadow.retrieval_evaluation_receipt.v1")
                    && receipt.get("policyId").and_then(serde_json::Value::as_str)
                        == Some(RETRIEVAL_TUNING_POLICY_ID)
                    && receipt
                        .get("reportHash")
                        .and_then(serde_json::Value::as_str)
                        == Some(report_hash)
                    && receipt
                        .get("authorityHash")
                        .and_then(serde_json::Value::as_str)
                        == Some(authority_hash)
            }),
    )
}

fn insert_overlay_audit(
    connection: &DbConnection,
    workspace_id: &str,
    action: &str,
    details: &serde_json::Value,
) -> Result<String, ShadowTuningError> {
    let id = crate::db::generate_audit_id();
    connection
        .insert_audit(
            &id,
            &crate::db::CreateAuditInput {
                workspace_id: Some(workspace_id.to_owned()),
                actor: None,
                action: action.to_owned(),
                target_type: Some("workspace".to_owned()),
                target_id: Some(workspace_id.to_owned()),
                details: Some(details.to_string()),
            },
        )
        .map_err(|error| storage_error("record retrieval overlay audit", &error))?;
    Ok(id)
}

fn receipt_has_config(details: &serde_json::Value, current: &str) -> bool {
    details.get("policyId").and_then(serde_json::Value::as_str) == Some(RETRIEVAL_TUNING_POLICY_ID)
        && details
            .get("newConfigToml")
            .and_then(serde_json::Value::as_str)
            == Some(current)
        && details
            .get("newConfigHash")
            .and_then(serde_json::Value::as_str)
            == Some(content_digest(current).as_str())
        && details
            .get("priorConfigToml")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|prior| {
                details
                    .get("priorConfigHash")
                    .and_then(serde_json::Value::as_str)
                    == Some(content_digest(prior).as_str())
            })
}

fn receipt_matches_store(
    details: &serde_json::Value,
    workspace: &Path,
    connection: &DbConnection,
) -> Result<bool, ShadowTuningError> {
    let workspace = workspace
        .canonicalize()
        .map_err(|error| storage_error("bind overlay workspace", &error))?;
    let crate::db::DatabaseLocation::File(database) = connection.location() else {
        return Ok(false);
    };
    let database = database
        .canonicalize()
        .map_err(|error| storage_error("bind overlay database", &error))?;
    let schema = connection
        .schema_version()
        .map_err(|error| storage_error("bind overlay schema", &error))?;
    let recorded_schema = details
        .get("databaseSchema")
        .and_then(serde_json::Value::as_u64);
    Ok(details
        .get("workspacePath")
        .and_then(serde_json::Value::as_str)
        == Some(workspace.to_string_lossy().as_ref())
        && details
            .get("databasePath")
            .and_then(serde_json::Value::as_str)
            == Some(database.to_string_lossy().as_ref())
        // A normal migration preserves historical transition authority.
        // New promotions still require the exact evaluator schema snapshot.
        && schema.zip(recorded_schema).is_some_and(|(current, recorded)| u64::from(current) >= recorded))
}

/// An intent is committed before replacement. If the second audit write fails,
/// restore only the exact bytes we published, never an intervening edit. A
/// crash leaves the full intended transition in the audit log for retry.
#[allow(clippy::too_many_arguments)]
fn publish_audited_overlay(
    workspace: &Path,
    connection: &DbConnection,
    workspace_id: &str,
    change: &OverlayChange,
    mut details: serde_json::Value,
    authority: Option<&PromotionAuthority>,
    action: &str,
) -> Result<(), ShadowTuningError> {
    let completed: BTreeSet<_> = overlay_audits(connection, workspace_id, action)?
        .iter()
        .filter_map(overlay_details)
        .filter_map(|details| {
            details
                .get("intentId")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        })
        .collect();
    let existing = overlay_audits(connection, workspace_id, OVERLAY_INTENT_AUDIT_ACTION)?
        .into_iter()
        .find(|entry| {
            !completed.contains(&entry.id)
                && overlay_details(entry).is_some_and(|intent| {
                    intent.get("action").and_then(serde_json::Value::as_str) == Some(action)
                        && intent.get("completion") == Some(&details)
                })
        });
    let intent_id = match existing {
        Some(entry) => entry.id,
        None => insert_overlay_audit(
            connection,
            workspace_id,
            OVERLAY_INTENT_AUDIT_ACTION,
            &serde_json::json!({
                "schema": "ee.shadow.retrieval_weights_intent.v1",
                "action": action,
                "completion": details,
            }),
        )?,
    };
    atomic_shadow_write_authorized(
        &workspace_config_path(workspace),
        &change.new_config,
        Some(&change.prior_config),
        authority,
        || {},
    )?;
    details["intentId"] = serde_json::Value::String(intent_id);
    if let Err(error) = insert_overlay_audit(connection, workspace_id, action, &details) {
        let rollback = atomic_shadow_write_expected(
            &workspace_config_path(workspace),
            &change.prior_config,
            Some(&change.new_config),
        );
        return Err(storage_error(
            "complete retrieval overlay audit",
            &match rollback {
                Ok(()) => format!("{error}; prior configuration restored; retry the same command"),
                Err(rollback) => format!(
                    "{error}; restoration refused or failed: {rollback}; durable intent retained for recovery"
                ),
            },
        ));
    }
    Ok(())
}

fn recover_promotion(
    connection: &DbConnection,
    workspace_id: &str,
    authority: &PromotionAuthority,
    current: &str,
    dry_run: bool,
) -> Result<Option<OverlayChange>, ShadowTuningError> {
    let authority_hash = content_digest(
        &serde_json::to_string(authority)
            .map_err(|error| storage_error("encode tuning authority", &error))?,
    );
    let matches = |details: &serde_json::Value| {
        receipt_has_config(details, current)
            && details
                .get("reportHash")
                .and_then(serde_json::Value::as_str)
                == Some(authority.report_hash.as_str())
            && details
                .get("authorityHash")
                .and_then(serde_json::Value::as_str)
                == Some(authority_hash.as_str())
    };
    let promotions = overlay_audits(
        connection,
        workspace_id,
        PROMOTE_RETRIEVAL_WEIGHTS_AUDIT_ACTION,
    )?;
    let demoted: BTreeSet<_> = overlay_audits(
        connection,
        workspace_id,
        DEMOTE_RETRIEVAL_WEIGHTS_AUDIT_ACTION,
    )?
    .iter()
    .filter_map(overlay_details)
    .filter_map(|details| {
        details
            .get("restoredFromAudit")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
    })
    .collect();
    if promotions.iter().any(|entry| {
        !demoted.contains(&entry.id)
            && overlay_details(entry).is_some_and(|details| matches(&details))
    }) {
        return Ok(Some(OverlayChange {
            applied: false,
            prior_config: current.to_owned(),
            new_config: current.to_owned(),
            diff: Vec::new(),
            report_hash: Some(authority.report_hash.clone()),
        }));
    }
    let completed: BTreeSet<_> = promotions
        .iter()
        .filter_map(overlay_details)
        .filter_map(|details| {
            details
                .get("intentId")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        })
        .collect();
    for intent in overlay_audits(connection, workspace_id, OVERLAY_INTENT_AUDIT_ACTION)? {
        if completed.contains(&intent.id) {
            continue;
        }
        let Some(value) = overlay_details(&intent) else {
            continue;
        };
        if value.get("action").and_then(serde_json::Value::as_str)
            != Some(PROMOTE_RETRIEVAL_WEIGHTS_AUDIT_ACTION)
        {
            continue;
        }
        let Some(mut details) = value
            .get("completion")
            .cloned()
            .filter(|details| matches(details))
        else {
            continue;
        };
        if !dry_run {
            details["intentId"] = serde_json::Value::String(intent.id);
            insert_overlay_audit(
                connection,
                workspace_id,
                PROMOTE_RETRIEVAL_WEIGHTS_AUDIT_ACTION,
                &details,
            )?;
        }
        return Ok(Some(OverlayChange {
            applied: !dry_run,
            prior_config: current.to_owned(),
            new_config: current.to_owned(),
            diff: vec!["Complete the audit for the already-published retrieval overlay".to_owned()],
            report_hash: Some(authority.report_hash.clone()),
        }));
    }
    Ok(None)
}

/// Rollback remains available after a subsequent `shadow run` replaces the
/// singleton report. The durable evaluated intent, exact published bytes and
/// absence of a newer completed transition suffice to finish its audit.
fn recover_published_promotion_for_demote(
    workspace: &Path,
    connection: &DbConnection,
    workspace_id: &str,
    current: &str,
    dry_run: bool,
) -> Result<Option<StoredAuditEntry>, ShadowTuningError> {
    let mut transitions = overlay_audits(
        connection,
        workspace_id,
        PROMOTE_RETRIEVAL_WEIGHTS_AUDIT_ACTION,
    )?;
    transitions.extend(overlay_audits(
        connection,
        workspace_id,
        DEMOTE_RETRIEVAL_WEIGHTS_AUDIT_ACTION,
    )?);
    let latest = transitions
        .iter()
        .map(|entry| (&entry.timestamp, &entry.id))
        .max();
    let completed: BTreeSet<_> = transitions
        .iter()
        .filter_map(overlay_details)
        .filter_map(|details| {
            details
                .get("intentId")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        })
        .collect();
    for mut intent in overlay_audits(connection, workspace_id, OVERLAY_INTENT_AUDIT_ACTION)? {
        if completed.contains(&intent.id)
            || latest.is_some_and(|last| (&intent.timestamp, &intent.id) <= last)
        {
            continue;
        }
        let Some(value) = overlay_details(&intent) else {
            continue;
        };
        if value.get("action").and_then(serde_json::Value::as_str)
            != Some(PROMOTE_RETRIEVAL_WEIGHTS_AUDIT_ACTION)
        {
            continue;
        }
        let Some(mut details) = value
            .get("completion")
            .cloned()
            .filter(|details| receipt_has_config(details, current))
        else {
            continue;
        };
        if !receipt_matches_store(&details, workspace, connection)? {
            continue;
        }
        let (Some(report_hash), Some(authority_hash)) = (
            details
                .get("reportHash")
                .and_then(serde_json::Value::as_str),
            details
                .get("authorityHash")
                .and_then(serde_json::Value::as_str),
        ) else {
            continue;
        };
        if !evaluation_hashes_attested(connection, workspace_id, report_hash, authority_hash)? {
            continue;
        }
        details["intentId"] = serde_json::Value::String(intent.id.clone());
        if !dry_run {
            intent.id = insert_overlay_audit(
                connection,
                workspace_id,
                PROMOTE_RETRIEVAL_WEIGHTS_AUDIT_ACTION,
                &details,
            )?;
        }
        intent.action = PROMOTE_RETRIEVAL_WEIGHTS_AUDIT_ACTION.to_owned();
        intent.details = Some(details.to_string());
        return Ok(Some(intent));
    }
    Ok(None)
}

/// Restore only tuning-owned fields from the most recent completed promotion.
/// An untouched config regains its exact original bytes; unrelated later edits
/// survive and conflicting edits to an owned weight refuse the rollback.
#[allow(clippy::type_complexity)]
pub fn demote_retrieval_weights(
    workspace_path: &Path,
    connection: &DbConnection,
    workspace_id: &str,
    dry_run: bool,
) -> Result<Result<OverlayChange, PromoteRefusal>, ShadowTuningError> {
    connection.with_write_owner_fence(
        |error| storage_error("own retrieval demotion", &error),
        || demote_retrieval_weights_owned(workspace_path, connection, workspace_id, dry_run),
    )
}

#[allow(clippy::type_complexity)]
fn demote_retrieval_weights_owned(
    workspace_path: &Path,
    connection: &DbConnection,
    workspace_id: &str,
    dry_run: bool,
) -> Result<Result<OverlayChange, PromoteRefusal>, ShadowTuningError> {
    let (current_config, _) = read_config_bytes(workspace_path)?;
    let recovered = recover_published_promotion_for_demote(
        workspace_path,
        connection,
        workspace_id,
        &current_config,
        dry_run,
    )?;
    let audits = overlay_audits(
        connection,
        workspace_id,
        PROMOTE_RETRIEVAL_WEIGHTS_AUDIT_ACTION,
    )?;
    let Some(promotion) = recovered.as_ref().or_else(|| audits.first()) else {
        return Ok(Err(PromoteRefusal::NoPriorPromotion));
    };
    let details = overlay_details(promotion).unwrap_or(serde_json::Value::Null);
    if !receipt_matches_store(&details, workspace_path, connection)? {
        return Ok(Err(invalid_report(
            "promotion belongs to a different workspace or database",
        )));
    }
    let report_hash = details
        .get("reportHash")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    if overlay_audits(
        connection,
        workspace_id,
        DEMOTE_RETRIEVAL_WEIGHTS_AUDIT_ACTION,
    )?
    .iter()
    .filter_map(overlay_details)
    .any(|details| {
        details
            .get("restoredFromAudit")
            .and_then(serde_json::Value::as_str)
            == Some(promotion.id.as_str())
    }) {
        return Ok(Ok(OverlayChange {
            applied: false,
            prior_config: current_config.clone(),
            new_config: current_config,
            diff: Vec::new(),
            report_hash,
        }));
    }
    let Some(prior_config) = details
        .get("priorConfigToml")
        .and_then(serde_json::Value::as_str)
    else {
        return Ok(Err(invalid_report(
            "promotion audit has no prior configuration",
        )));
    };
    let Some(promoted_config) = details
        .get("newConfigToml")
        .and_then(serde_json::Value::as_str)
    else {
        return Ok(Err(invalid_report(
            "promotion audit has no owned-field snapshot; cannot safely demote",
        )));
    };
    if !receipt_has_config(&details, promoted_config) {
        return Ok(Err(invalid_report(
            "promotion audit configuration hashes disagree",
        )));
    }
    // A demotion can itself have published just before a crash. Only the
    // matching durable intent may authorize finishing that audit on retry.
    for intent in overlay_audits(connection, workspace_id, OVERLAY_INTENT_AUDIT_ACTION)? {
        let Some(value) = overlay_details(&intent) else {
            continue;
        };
        if value.get("action").and_then(serde_json::Value::as_str)
            != Some(DEMOTE_RETRIEVAL_WEIGHTS_AUDIT_ACTION)
        {
            continue;
        }
        let Some(mut completion) = value.get("completion").cloned() else {
            continue;
        };
        if completion
            .get("restoredFromAudit")
            .and_then(serde_json::Value::as_str)
            == Some(promotion.id.as_str())
            && receipt_has_config(&completion, &current_config)
        {
            if !dry_run {
                completion["intentId"] = serde_json::Value::String(intent.id);
                insert_overlay_audit(
                    connection,
                    workspace_id,
                    DEMOTE_RETRIEVAL_WEIGHTS_AUDIT_ACTION,
                    &completion,
                )?;
            }
            return Ok(Ok(OverlayChange {
                applied: !dry_run,
                prior_config: current_config.clone(),
                new_config: current_config,
                diff: vec![
                    "Complete the audit for the already-restored retrieval weights".to_owned(),
                ],
                report_hash,
            }));
        }
    }
    let (restored, diff) =
        match restore_owned_weights(&current_config, promoted_config, prior_config) {
            Ok(restored) => restored,
            Err(refusal) => return Ok(Err(refusal)),
        };
    let change = OverlayChange {
        applied: !dry_run,
        prior_config: current_config,
        new_config: restored,
        diff,
        report_hash,
    };
    if dry_run {
        return Ok(Ok(change));
    }
    let demote_details = serde_json::json!({
        "schema": "ee.shadow.retrieval_weights_demotion.v1",
        "policyId": RETRIEVAL_TUNING_POLICY_ID,
        "restoredFromAudit": promotion.id,
        "restoredBytes": change.new_config.len(),
        "reportHash": change.report_hash,
        "priorConfigToml": change.prior_config,
        "newConfigToml": change.new_config,
        "priorConfigHash": content_digest(&change.prior_config),
        "newConfigHash": content_digest(&change.new_config),
    });
    publish_audited_overlay(
        workspace_path,
        connection,
        workspace_id,
        &change,
        demote_details,
        None,
        DEMOTE_RETRIEVAL_WEIGHTS_AUDIT_ACTION,
    )?;
    Ok(Ok(change))
}

fn restore_owned_weights(
    current: &str,
    promoted: &str,
    prior: &str,
) -> Result<(String, Vec<String>), PromoteRefusal> {
    let parse = |text: &str| {
        text.parse::<toml_edit::DocumentMut>()
            .map_err(|_| invalid_report("configuration snapshot is not valid TOML"))
    };
    let prior_document = parse(prior)?;
    let promoted_document = parse(promoted)?;
    let mut current_document = parse(current)?;
    let table = |document: &toml_edit::DocumentMut, key: &str| {
        document
            .get("search")
            .and_then(toml_edit::Item::as_table_like)
            .and_then(|search| search.get(key))
            .cloned()
    };
    let mut diff = Vec::new();
    for key in OWNED_WEIGHT_KEYS {
        let expected = table(&promoted_document, key)
            .and_then(|item| item.as_float())
            .ok_or_else(|| invalid_report("promotion does not carry all owned weights"))?;
        if table(&current_document, key).and_then(|item| item.as_float()) != Some(expected) {
            return Err(invalid_report(format!(
                "search.{key} changed since promotion; refusing to overwrite that edit"
            )));
        }
        let original = table(&prior_document, key);
        diff.push(format!(
            "search.{key}: {expected} -> {}",
            original
                .as_ref()
                .map_or_else(|| "(unset)".to_owned(), std::string::ToString::to_string)
        ));
        let search = current_document
            .get_mut("search")
            .and_then(toml_edit::Item::as_table_like_mut)
            .ok_or_else(|| invalid_report("current search configuration is not a table"))?;
        match original {
            Some(item) => {
                search.insert(key, item);
            }
            None => {
                search.remove(key);
            }
        }
    }
    Ok((
        if current == promoted {
            prior.to_owned()
        } else {
            current_document.to_string()
        },
        diff,
    ))
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

    const WORKSPACE: &str = "wsp_00000000000000000000000901";

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
                "pack_00000000000000000000001001",
                &CreatePackRecordInput {
                    task_paths: Vec::new(),
                    workspace_id: WORKSPACE.to_owned(),
                    query: "prepare release".to_owned(),
                    profile: "balanced".to_owned(),
                    max_tokens: 4000,
                    used_tokens: 0,
                    item_count: 0,
                    omitted_count: 0,
                    pack_hash:
                        "blake3:0000000000000000000000000000000000000000000000000000000000000000"
                            .to_owned(),
                    degraded_json: None,
                    created_by: None,
                },
                &[],
                &[],
            )
            .map_err(|error| error.to_string())?;
        connection
            .insert_feedback_event(
                "fb_00000000000000000000001001",
                &CreateFeedbackEventInput {
                    workspace_id: WORKSPACE.to_owned(),
                    target_type: "memory".to_owned(),
                    target_id: "mem_00000000000000000000001001".to_owned(),
                    signal: "helpful".to_owned(),
                    weight: 1.0,
                    source_type: "outcome_observed".to_owned(),
                    source_id: None,
                    reason: None,
                    evidence_json: Some(pack_item_evidence("pack_00000000000000000000001001")),
                    session_id: None,
                },
            )
            .map_err(|error| error.to_string())?;
        // Quarantined feedback lands in a separate table at record time and
        // must never reach the join.
        connection
            .insert_feedback_quarantine(
                "fq_00000000000000000000001001",
                &CreateFeedbackQuarantineInput {
                    workspace_id: WORKSPACE.to_owned(),
                    source_id: "poisoned-source".to_owned(),
                    target_type: "memory".to_owned(),
                    target_id: "mem_00000000000000000000001001".to_owned(),
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
            || triple.feedback_event_id != "fb_00000000000000000000001001"
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
                "pack_00000000000000000000001002",
                &CreatePackRecordInput {
                    task_paths: Vec::new(),
                    workspace_id: WORKSPACE.to_owned(),
                    query: query.to_owned(),
                    profile: "balanced".to_owned(),
                    max_tokens: 4000,
                    used_tokens: 0,
                    item_count: 0,
                    omitted_count: 0,
                    pack_hash:
                        "blake3:1111111111111111111111111111111111111111111111111111111111111111"
                            .to_owned(),
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
                "audit_00000000000000000000001001",
                &CreateAuditInput {
                    workspace_id: Some(WORKSPACE.to_owned()),
                    actor: None,
                    action: audit_actions::SEARCH_RETURNED_MEM.to_owned(),
                    target_type: Some("memory".to_owned()),
                    target_id: Some("mem_00000000000000000000001002".to_owned()),
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
                "fb_00000000000000000000001002",
                &CreateFeedbackEventInput {
                    workspace_id: WORKSPACE.to_owned(),
                    target_type: "memory".to_owned(),
                    target_id: "mem_00000000000000000000001002".to_owned(),
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
            || triple.audit_row_id.as_deref() != Some("audit_00000000000000000000001001")
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
        // mem-a is lexical-only and mem-b semantic-only. Neutral upstream RRF
        // ties their rank contributions, so Frankensearch's lexical tiebreak
        // puts mem-a first. The first grid vector (`lexical -0.10`) normalizes
        // to lexical=0.875, semantic=1.125 and flips the order. The single
        // helpful label on mem-b therefore makes that first strict improver
        // the deterministic winner: incumbent = 1/log2(3), winner =
        // 1/log2(2) = 1.0. Descent cannot beat a perfect score.
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

    // ===== S4: promotion mechanics tests =====

    fn promotable_report_fixture(
        workspace: &Path,
        connection: &DbConnection,
        generation: Option<u64>,
    ) -> Result<RetrievalTuningReport, String> {
        const PACK: &str = "pack_00000000000000000000001001";
        connection
            .insert_pack_record(
                PACK,
                &CreatePackRecordInput {
                    task_paths: Vec::new(),
                    workspace_id: WORKSPACE.to_owned(),
                    query: "q1".to_owned(),
                    profile: "balanced".to_owned(),
                    max_tokens: 4000,
                    used_tokens: 0,
                    item_count: 0,
                    omitted_count: 0,
                    pack_hash: format!("blake3:{}", "0".repeat(64)),
                    degraded_json: None,
                    created_by: None,
                },
                &[],
                &[],
            )
            .map_err(|error| error.to_string())?;
        connection
            .insert_feedback_event_for_recovery(&event(
                "fb_00000000000000000000001001",
                "mem-b",
                ts(0),
                Some(pack_item_evidence(PACK)),
            ))
            .map_err(|error| error.to_string())?;
        let extraction = LabelExtractionConfig::default();
        let cx = Cx::for_testing();
        let labels = extract_labeled_triples(&cx, connection, WORKSPACE, &extraction, ts(0))
            .map_err(|error| error.to_string())?;
        let evaluation = evaluate_fusion_candidates(
            &cx,
            &flip_fixture_replays(),
            &labels.triples,
            TuningWeights::incumbent_for_workspace(workspace).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        let gate = RetrievalTuningGateConfig {
            min_triples: 1,
            min_queries: 1,
            promote_margin: 0.03,
        };
        let generation = generation.unwrap_or(
            connection
                .get_workspace_generation(WORKSPACE)
                .map_err(|error| error.to_string())?
                .unwrap_or(0),
        );
        let mut report =
            assemble_retrieval_tuning_report(labels, Some(evaluation), generation, &gate)
                .map_err(|error| error.to_string())?;
        let mut authority = capture_promotion_authority(
            workspace,
            connection,
            WORKSPACE,
            ts(0),
            &extraction,
            &gate,
        )
        .map_err(|error| error.to_string())?;
        authority.report_hash = report.report_hash.clone();
        report.authority = Some(authority);
        assert!(
            report.promotable,
            "positive fixture must be evaluated as promotable"
        );
        Ok(report)
    }

    #[test]
    fn promote_applies_overlay_and_demote_restores_bytes() -> TestResult {
        let tempdir = tempfile::tempdir_in("/tmp").map_err(|error| error.to_string())?;
        let workspace = tempdir.path();
        let store_dir = workspace.join(".ee");
        std::fs::create_dir_all(&store_dir).map_err(|error| error.to_string())?;
        let database_path = store_dir.join("ee.db");
        let connection =
            DbConnection::open_file(&database_path).map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        connection
            .insert_workspace(
                WORKSPACE,
                &CreateWorkspaceInput {
                    path: workspace.display().to_string(),
                    name: None,
                },
            )
            .map_err(|error| error.to_string())?;

        let report = promotable_report_fixture(workspace, &connection, None)?;
        persist_retrieval_tuning_report(workspace, &report).map_err(|error| error.to_string())?;

        // Dry-run writes nothing.
        let plan = promote_retrieval_weights(workspace, &connection, WORKSPACE, true)
            .map_err(|error| error.to_string())?
            .map_err(|refusal| format!("unexpected refusal: {refusal:?}"))?;
        if plan.applied || workspace.join(".ee").join("config.toml").exists() {
            return Err(format!("dry-run must write nothing: {plan:?}"));
        }

        // Apply: config gains the winner weights; the promotion is audited
        // with the full prior bytes (empty — no config existed).
        let change = promote_retrieval_weights(workspace, &connection, WORKSPACE, false)
            .map_err(|error| error.to_string())?
            .map_err(|refusal| format!("unexpected refusal: {refusal:?}"))?;
        let written = std::fs::read_to_string(workspace.join(".ee").join("config.toml"))
            .map_err(|error| error.to_string())?;
        if !written.contains("lexical_weight") || !written.contains("[search]") {
            return Err(format!("overlay not written: {written}"));
        }
        if !change.prior_config.is_empty() {
            return Err("prior config must be empty for a fresh workspace".to_owned());
        }
        let audits = connection
            .list_audit_by_action(PROMOTE_RETRIEVAL_WEIGHTS_AUDIT_ACTION, None)
            .map_err(|error| error.to_string())?;
        if audits.is_empty() {
            return Err("promotion must be audited".to_owned());
        }

        // Demote: byte-identical restoration of the pre-promotion state.
        let demotion = demote_retrieval_weights(workspace, &connection, WORKSPACE, false)
            .map_err(|error| error.to_string())?
            .map_err(|refusal| format!("unexpected refusal: {refusal:?}"))?;
        let restored = std::fs::read_to_string(workspace.join(".ee").join("config.toml"))
            .map_err(|error| error.to_string())?;
        if !restored.is_empty() || !demotion.new_config.is_empty() {
            return Err(format!(
                "demote must restore the exact prior bytes: {restored:?}"
            ));
        }
        Ok(())
    }

    #[test]
    fn promote_refuses_missing_and_stale_reports() -> TestResult {
        let tempdir = tempfile::tempdir_in("/tmp").map_err(|error| error.to_string())?;
        let workspace = tempdir.path();
        std::fs::create_dir_all(workspace.join(".ee")).map_err(|error| error.to_string())?;
        let connection = DbConnection::open_file(workspace.join(".ee").join("ee.db"))
            .map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;

        match promote_retrieval_weights(workspace, &connection, "ws-missing", false)
            .map_err(|error| error.to_string())?
        {
            Err(PromoteRefusal::ReportMissing { .. }) => {}
            other => return Err(format!("missing report must refuse: {other:?}")),
        }

        // A report evaluated at a different generation is stale.
        connection
            .insert_workspace(
                WORKSPACE,
                &CreateWorkspaceInput {
                    path: workspace.display().to_string(),
                    name: None,
                },
            )
            .map_err(|error| error.to_string())?;
        let stale = promotable_report_fixture(workspace, &connection, Some(5))?;
        persist_retrieval_tuning_report(workspace, &stale).map_err(|error| error.to_string())?;
        match promote_retrieval_weights(workspace, &connection, WORKSPACE, false)
            .map_err(|error| error.to_string())?
        {
            Err(PromoteRefusal::StaleGeneration { report: 5, .. }) => Ok(()),
            other => Err(format!("stale report must refuse: {other:?}")),
        }
    }

    struct OverlayFixture {
        directory: tempfile::TempDir,
        connection: DbConnection,
        report: RetrievalTuningReport,
    }

    fn overlay_fixture(config: &str) -> Result<OverlayFixture, String> {
        let directory = tempfile::tempdir_in("/tmp").map_err(|error| error.to_string())?;
        let workspace = directory.path();
        std::fs::create_dir_all(workspace.join(".ee")).map_err(|error| error.to_string())?;
        std::fs::write(workspace_config_path(workspace), config)
            .map_err(|error| error.to_string())?;
        let connection = DbConnection::open_file(workspace.join(".ee/ee.db"))
            .map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        connection
            .insert_workspace(
                WORKSPACE,
                &CreateWorkspaceInput {
                    path: workspace.display().to_string(),
                    name: None,
                },
            )
            .map_err(|error| error.to_string())?;
        let report = promotable_report_fixture(workspace, &connection, None)?;
        persist_retrieval_tuning_report(workspace, &report).map_err(|error| error.to_string())?;
        Ok(OverlayFixture {
            directory,
            connection,
            report,
        })
    }

    fn no_promotion(fixture: &OverlayFixture) -> TestResult {
        let workspace = fixture.directory.path();
        let before = read_config_bytes(workspace).map_err(|error| error.to_string())?;
        for dry_run in [true, false] {
            assert!(
                promote_retrieval_weights(workspace, &fixture.connection, WORKSPACE, dry_run)
                    .map_err(|error| error.to_string())?
                    .is_err()
            );
            assert_eq!(
                read_config_bytes(workspace).map_err(|error| error.to_string())?,
                before
            );
        }
        assert!(
            overlay_audits(
                &fixture.connection,
                WORKSPACE,
                PROMOTE_RETRIEVAL_WEIGHTS_AUDIT_ACTION
            )
            .map_err(|error| error.to_string())?
            .is_empty()
        );
        assert!(
            overlay_audits(&fixture.connection, WORKSPACE, OVERLAY_INTENT_AUDIT_ACTION)
                .map_err(|error| error.to_string())?
                .is_empty()
        );
        Ok(())
    }

    #[test]
    fn edited_report_never_authorizes_live_weights_or_demotion() -> TestResult {
        let fixture = overlay_fixture("# Original config\n")?;
        let workspace = fixture.directory.path();
        let mut edited: serde_json::Value =
            serde_json::from_str(&render_retrieval_tuning_report_json(&fixture.report))
                .map_err(|error| error.to_string())?;
        edited["winner"]["weights"]["lexical"] = serde_json::json!(0.65);
        std::fs::write(shadow_report_path(workspace), edited.to_string())
            .map_err(|error| error.to_string())?;
        no_promotion(&fixture)?;
        for dry_run in [true, false] {
            assert!(matches!(
                demote_retrieval_weights(workspace, &fixture.connection, WORKSPACE, dry_run)
                    .map_err(|error| error.to_string())?,
                Err(PromoteRefusal::NoPriorPromotion)
            ));
        }
        assert_eq!(
            read_config_bytes(workspace)
                .map_err(|error| error.to_string())?
                .0,
            "# Original config\n"
        );
        Ok(())
    }

    fn rehash_report(value: &mut serde_json::Value, authority: &mut PromotionAuthority) {
        value.as_object_mut().unwrap().remove("reportHash");
        let mut input = Vec::new();
        append_len_prefixed(&mut input, REPORT_HASH_DOMAIN.as_bytes());
        append_len_prefixed(&mut input, value.to_string().as_bytes());
        authority.report_hash = format!("blake3:{}", blake3::hash(&input).to_hex());
        value["reportHash"] = serde_json::Value::String(authority.report_hash.clone());
    }

    #[test]
    fn self_consistent_checksums_do_not_bypass_bounds_gate_or_evaluator() -> TestResult {
        let fixture = overlay_fixture("")?;
        for mutation in ["bounds", "gate", "margin", "evaluation"] {
            let mut forged: serde_json::Value =
                serde_json::from_str(&render_retrieval_tuning_report_json(&fixture.report))
                    .map_err(|error| error.to_string())?;
            let mut authority = fixture.report.authority.clone().ok_or("authority")?;
            match mutation {
                "bounds" => forged["winner"]["weights"]["lexical"] = serde_json::json!(0.9),
                "gate" => authority.gate.min_triples = 0,
                "margin" => authority.gate.promote_margin = 0.0,
                _ => forged["diagnostics"]["evaluationHash"] = serde_json::json!("blake3:forged"),
            }
            rehash_report(&mut forged, &mut authority);
            assert!(
                validated_report(&forged, &authority).is_err(),
                "mutation {mutation}"
            );
            std::fs::write(
                shadow_report_path(fixture.directory.path()),
                forged.to_string(),
            )
            .map_err(|error| error.to_string())?;
            std::fs::write(
                authority_path(fixture.directory.path()),
                serde_json::to_string(&authority).map_err(|error| error.to_string())?,
            )
            .map_err(|error| error.to_string())?;
            no_promotion(&fixture)?;
        }
        Ok(())
    }

    #[test]
    fn rehashed_report_and_sidecar_cannot_invent_an_evaluation() -> TestResult {
        let fixture = overlay_fixture("")?;
        let mut forged: serde_json::Value =
            serde_json::from_str(&render_retrieval_tuning_report_json(&fixture.report))
                .map_err(|error| error.to_string())?;
        let mut authority = fixture.report.authority.clone().ok_or("authority")?;
        // Scale every score together, preserving the candidate walk, winner
        // and relative margin. Both checksum layers remain internally valid.
        for key in ["incumbent", "winner"] {
            forged[key]["score"] =
                serde_json::json!(forged[key]["score"].as_f64().ok_or("score")? / 2.0);
        }
        for candidate in forged["candidates"].as_array_mut().ok_or("candidates")? {
            candidate["score"] =
                serde_json::json!(candidate["score"].as_f64().ok_or("score")? / 2.0);
        }
        let incumbent =
            parsed_candidate(&forged["incumbent"], true).map_err(|error| error.message())?;
        let candidates = forged["candidates"]
            .as_array()
            .ok_or("candidates")?
            .iter()
            .map(|candidate| parsed_candidate(candidate, false))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.message())?;
        forged["diagnostics"]["evaluationHash"] = serde_json::Value::String(evaluation_hash(
            &incumbent,
            &candidates,
            fixture.report.labels.triples.len(),
        ));
        rehash_report(&mut forged, &mut authority);
        assert!(validated_report(&forged, &authority).is_ok());
        std::fs::write(
            shadow_report_path(fixture.directory.path()),
            forged.to_string(),
        )
        .map_err(|error| error.to_string())?;
        std::fs::write(
            authority_path(fixture.directory.path()),
            serde_json::to_string(&authority).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        no_promotion(&fixture)
    }

    #[test]
    fn same_generation_config_and_outcome_edits_invalidate_promotion() -> TestResult {
        let config_fixture = overlay_fixture("")?;
        let workspace = config_fixture.directory.path();
        let before_generation = config_fixture
            .connection
            .get_workspace_generation(WORKSPACE)
            .map_err(|error| error.to_string())?;
        std::fs::write(
            workspace_config_path(workspace),
            "[scoring]\nrecency_tau_days = 45.0\n",
        )
        .map_err(|error| error.to_string())?;
        assert_eq!(
            config_fixture
                .connection
                .get_workspace_generation(WORKSPACE)
                .map_err(|error| error.to_string())?,
            before_generation
        );
        no_promotion(&config_fixture)?;

        let outcome_fixture = overlay_fixture("")?;
        let before_generation = outcome_fixture
            .connection
            .get_workspace_generation(WORKSPACE)
            .map_err(|error| error.to_string())?;
        outcome_fixture.connection.execute_raw("UPDATE feedback_events SET signal = 'harmful' WHERE id = 'fb_00000000000000000000001001'")
            .map_err(|error| error.to_string())?;
        assert_eq!(
            outcome_fixture
                .connection
                .get_workspace_generation(WORKSPACE)
                .map_err(|error| error.to_string())?,
            before_generation
        );
        no_promotion(&outcome_fixture)
    }

    #[test]
    fn receipt_cannot_move_to_an_equal_generation_database() -> TestResult {
        let fixture = overlay_fixture("")?;
        let workspace = fixture.directory.path();
        let other = DbConnection::open_file(workspace.join(".ee/other.db"))
            .map_err(|error| error.to_string())?;
        other.migrate().map_err(|error| error.to_string())?;
        other
            .insert_workspace(
                WORKSPACE,
                &CreateWorkspaceInput {
                    path: workspace.display().to_string(),
                    name: None,
                },
            )
            .map_err(|error| error.to_string())?;
        let other_report = promotable_report_fixture(workspace, &other, None)?;
        assert_eq!(other_report.db_generation, fixture.report.db_generation);
        assert_eq!(other_report.labels, fixture.report.labels);
        for dry_run in [true, false] {
            assert!(matches!(
                promote_retrieval_weights(workspace, &other, WORKSPACE, dry_run)
                    .map_err(|error| error.to_string())?,
                Err(PromoteRefusal::ReportInvalid { .. })
            ));
        }
        assert_eq!(
            read_config_bytes(workspace)
                .map_err(|error| error.to_string())?
                .0,
            ""
        );
        assert!(
            overlay_audits(&other, WORKSPACE, OVERLAY_INTENT_AUDIT_ACTION)
                .map_err(|error| error.to_string())?
                .is_empty()
        );
        Ok(())
    }

    #[test]
    fn audit_failure_rolls_back_and_retry_recovers_published_intent() -> TestResult {
        let fixture = overlay_fixture("# Before tuning\n")?;
        let workspace = fixture.directory.path();
        fixture.connection.execute_raw("CREATE TRIGGER refuse_shadow_completion BEFORE INSERT ON audit_log WHEN NEW.action = 'shadow.promote_retrieval_weights' BEGIN SELECT RAISE(ABORT, 'injected completion failure'); END")
            .map_err(|error| error.to_string())?;
        let error = promote_retrieval_weights(workspace, &fixture.connection, WORKSPACE, false)
            .expect_err("completion audit must fail");
        assert!(error.to_string().contains("prior configuration restored"));
        assert_eq!(
            read_config_bytes(workspace)
                .map_err(|error| error.to_string())?
                .0,
            "# Before tuning\n"
        );
        assert!(
            overlay_audits(
                &fixture.connection,
                WORKSPACE,
                PROMOTE_RETRIEVAL_WEIGHTS_AUDIT_ACTION
            )
            .map_err(|error| error.to_string())?
            .is_empty()
        );
        let intents = overlay_audits(&fixture.connection, WORKSPACE, OVERLAY_INTENT_AUDIT_ACTION)
            .map_err(|error| error.to_string())?;
        assert_eq!(intents.len(), 1);
        // An unapplied intent is not rollback authority.
        assert!(matches!(
            demote_retrieval_weights(workspace, &fixture.connection, WORKSPACE, false)
                .map_err(|error| error.to_string())?,
            Err(PromoteRefusal::NoPriorPromotion)
        ));
        let intent = overlay_details(&intents[0]).ok_or("intent")?;
        let published = intent
            .pointer("/completion/newConfigToml")
            .and_then(serde_json::Value::as_str)
            .ok_or("new config")?;
        fixture
            .connection
            .execute_raw("DROP TRIGGER refuse_shadow_completion")
            .map_err(|error| error.to_string())?;
        // Simulate a crash after publication but before its completion row.
        std::fs::write(workspace_config_path(workspace), published)
            .map_err(|error| error.to_string())?;
        fixture.connection.execute_raw(&format!(
            "INSERT INTO workspace_generations (workspace_id, generation, updated_at) VALUES ('{WORKSPACE}', 20, '2026-08-01T12:01:00Z') ON CONFLICT(workspace_id) DO UPDATE SET generation = 20"
        )).map_err(|error| error.to_string())?;
        fixture.connection.execute_raw("UPDATE feedback_events SET signal = 'harmful' WHERE id = 'fb_00000000000000000000001001'")
            .map_err(|error| error.to_string())?;
        let recovered = promote_retrieval_weights(workspace, &fixture.connection, WORKSPACE, false)
            .map_err(|error| error.to_string())?
            .map_err(|refusal| format!("{refusal:?}"))?;
        assert!(recovered.applied);
        assert_eq!(recovered.prior_config, recovered.new_config);
        assert_eq!(
            read_config_bytes(workspace)
                .map_err(|error| error.to_string())?
                .0,
            published
        );
        let promotions = overlay_audits(
            &fixture.connection,
            WORKSPACE,
            PROMOTE_RETRIEVAL_WEIGHTS_AUDIT_ACTION,
        )
        .map_err(|error| error.to_string())?;
        assert_eq!(promotions.len(), 1);
        assert_eq!(
            overlay_details(&promotions[0]).ok_or("completion")?["intentId"].as_str(),
            Some(intents[0].id.as_str())
        );
        let repeated = promote_retrieval_weights(workspace, &fixture.connection, WORKSPACE, false)
            .map_err(|error| error.to_string())?
            .map_err(|refusal| format!("{refusal:?}"))?;
        assert!(!repeated.applied);
        assert_eq!(
            overlay_audits(
                &fixture.connection,
                WORKSPACE,
                PROMOTE_RETRIEVAL_WEIGHTS_AUDIT_ACTION
            )
            .map_err(|error| error.to_string())?
            .len(),
            1
        );
        Ok(())
    }

    #[test]
    fn demote_recovers_a_published_intent_after_report_replacement() -> TestResult {
        let fixture = overlay_fixture("# Before tuning\n")?;
        let workspace = fixture.directory.path();
        fixture.connection.execute_raw("CREATE TRIGGER interrupt_shadow_completion BEFORE INSERT ON audit_log WHEN NEW.action = 'shadow.promote_retrieval_weights' BEGIN SELECT RAISE(ABORT, 'interrupted completion'); END")
            .map_err(|error| error.to_string())?;
        assert!(
            promote_retrieval_weights(workspace, &fixture.connection, WORKSPACE, false).is_err()
        );
        let intents = overlay_audits(&fixture.connection, WORKSPACE, OVERLAY_INTENT_AUDIT_ACTION)
            .map_err(|error| error.to_string())?;
        assert_eq!(intents.len(), 1);
        let intent = overlay_details(&intents[0]).ok_or("intent")?;
        let published = intent
            .pointer("/completion/newConfigToml")
            .and_then(serde_json::Value::as_str)
            .ok_or("new config")?;
        fixture
            .connection
            .execute_raw("DROP TRIGGER interrupt_shadow_completion")
            .map_err(|error| error.to_string())?;
        std::fs::write(workspace_config_path(workspace), published)
            .map_err(|error| error.to_string())?;
        // The report files no longer provide any authority. Rollback must be
        // driven by the original evaluated intent in this database.
        std::fs::write(shadow_report_path(workspace), "{}").map_err(|error| error.to_string())?;
        std::fs::write(authority_path(workspace), "{}").map_err(|error| error.to_string())?;
        let plan = demote_retrieval_weights(workspace, &fixture.connection, WORKSPACE, true)
            .map_err(|error| error.to_string())?
            .map_err(|refusal| format!("{refusal:?}"))?;
        assert!(!plan.applied);
        assert_eq!(plan.new_config, "# Before tuning\n");
        assert!(
            overlay_audits(
                &fixture.connection,
                WORKSPACE,
                PROMOTE_RETRIEVAL_WEIGHTS_AUDIT_ACTION
            )
            .map_err(|error| error.to_string())?
            .is_empty()
        );
        let demotion = demote_retrieval_weights(workspace, &fixture.connection, WORKSPACE, false)
            .map_err(|error| error.to_string())?
            .map_err(|refusal| format!("{refusal:?}"))?;
        assert!(demotion.applied);
        assert_eq!(
            read_config_bytes(workspace)
                .map_err(|error| error.to_string())?
                .0,
            "# Before tuning\n"
        );
        assert_eq!(
            overlay_audits(
                &fixture.connection,
                WORKSPACE,
                PROMOTE_RETRIEVAL_WEIGHTS_AUDIT_ACTION
            )
            .map_err(|error| error.to_string())?
            .len(),
            1
        );
        assert_eq!(
            overlay_audits(
                &fixture.connection,
                WORKSPACE,
                DEMOTE_RETRIEVAL_WEIGHTS_AUDIT_ACTION
            )
            .map_err(|error| error.to_string())?
            .len(),
            1
        );
        Ok(())
    }

    #[test]
    fn intent_failure_cannot_change_live_configuration() -> TestResult {
        let fixture = overlay_fixture("# Original config\n")?;
        fixture.connection.execute_raw("CREATE TRIGGER refuse_shadow_intent BEFORE INSERT ON audit_log WHEN NEW.action = 'shadow.retrieval_weights_intent' BEGIN SELECT RAISE(ABORT, 'injected intent failure'); END")
            .map_err(|error| error.to_string())?;
        assert!(
            promote_retrieval_weights(
                fixture.directory.path(),
                &fixture.connection,
                WORKSPACE,
                false
            )
            .is_err()
        );
        assert_eq!(
            read_config_bytes(fixture.directory.path())
                .map_err(|error| error.to_string())?
                .0,
            "# Original config\n"
        );
        assert!(
            overlay_audits(&fixture.connection, WORKSPACE, OVERLAY_INTENT_AUDIT_ACTION)
                .map_err(|error| error.to_string())?
                .is_empty()
        );
        assert!(
            overlay_audits(
                &fixture.connection,
                WORKSPACE,
                PROMOTE_RETRIEVAL_WEIGHTS_AUDIT_ACTION
            )
            .map_err(|error| error.to_string())?
            .is_empty()
        );
        assert!(matches!(
            demote_retrieval_weights(
                fixture.directory.path(),
                &fixture.connection,
                WORKSPACE,
                false
            )
            .map_err(|error| error.to_string())?,
            Err(PromoteRefusal::NoPriorPromotion)
        ));
        Ok(())
    }

    #[test]
    fn demotion_preserves_later_settings_and_is_idempotent() -> TestResult {
        let original = "# Keep this comment\n[search]\nlexical_weight = 0.5\nsemantic_weight = 0.5\ngraph_weight = 0.0\n";
        let fixture = overlay_fixture(original)?;
        let workspace = fixture.directory.path();
        promote_retrieval_weights(workspace, &fixture.connection, WORKSPACE, false)
            .map_err(|error| error.to_string())?
            .map_err(|refusal| format!("{refusal:?}"))?;
        let promoted = read_config_bytes(workspace)
            .map_err(|error| error.to_string())?
            .0;
        let edited =
            format!("{promoted}\n# A later unrelated setting\n[pack]\ndefault_max_tokens = 3000\n");
        std::fs::write(workspace_config_path(workspace), &edited)
            .map_err(|error| error.to_string())?;
        fixture.connection.execute_raw("CREATE TRIGGER refuse_shadow_demotion BEFORE INSERT ON audit_log WHEN NEW.action = 'shadow.demote_retrieval_weights' BEGIN SELECT RAISE(ABORT, 'injected demotion audit failure'); END")
            .map_err(|error| error.to_string())?;
        assert!(
            demote_retrieval_weights(workspace, &fixture.connection, WORKSPACE, false).is_err()
        );
        assert_eq!(
            read_config_bytes(workspace)
                .map_err(|error| error.to_string())?
                .0,
            edited
        );
        fixture
            .connection
            .execute_raw("DROP TRIGGER refuse_shadow_demotion")
            .map_err(|error| error.to_string())?;
        let demotion = demote_retrieval_weights(workspace, &fixture.connection, WORKSPACE, false)
            .map_err(|error| error.to_string())?
            .map_err(|refusal| format!("{refusal:?}"))?;
        assert!(demotion.applied);
        let restored = read_config_bytes(workspace)
            .map_err(|error| error.to_string())?
            .0;
        assert!(restored.contains("# Keep this comment"));
        assert!(restored.contains("# A later unrelated setting"));
        let document = restored
            .parse::<toml_edit::DocumentMut>()
            .map_err(|error| error.to_string())?;
        assert_eq!(
            document["pack"]["default_max_tokens"].as_integer(),
            Some(3000)
        );
        assert_eq!(document["search"]["lexical_weight"].as_float(), Some(0.5));
        assert_eq!(document["search"]["semantic_weight"].as_float(), Some(0.5));
        assert_eq!(document["search"]["graph_weight"].as_float(), Some(0.0));
        let repeated = demote_retrieval_weights(workspace, &fixture.connection, WORKSPACE, false)
            .map_err(|error| error.to_string())?
            .map_err(|refusal| format!("{refusal:?}"))?;
        assert!(!repeated.applied);
        assert_eq!(
            read_config_bytes(workspace)
                .map_err(|error| error.to_string())?
                .0,
            restored
        );
        assert_eq!(
            overlay_audits(
                &fixture.connection,
                WORKSPACE,
                DEMOTE_RETRIEVAL_WEIGHTS_AUDIT_ACTION
            )
            .map_err(|error| error.to_string())?
            .len(),
            1
        );
        Ok(())
    }

    #[test]
    fn demotion_refuses_conflicting_weight_edits() -> TestResult {
        let fixture = overlay_fixture("")?;
        let workspace = fixture.directory.path();
        promote_retrieval_weights(workspace, &fixture.connection, WORKSPACE, false)
            .map_err(|error| error.to_string())?
            .map_err(|refusal| format!("{refusal:?}"))?;
        let promoted = read_config_bytes(workspace)
            .map_err(|error| error.to_string())?
            .0;
        let mut document = promoted
            .parse::<toml_edit::DocumentMut>()
            .map_err(|error| error.to_string())?;
        document["search"]["lexical_weight"] = toml_edit::value(0.01);
        let edited = document.to_string();
        std::fs::write(workspace_config_path(workspace), &edited)
            .map_err(|error| error.to_string())?;
        for dry_run in [true, false] {
            assert!(matches!(
                demote_retrieval_weights(workspace, &fixture.connection, WORKSPACE, dry_run)
                    .map_err(|error| error.to_string())?,
                Err(PromoteRefusal::ReportInvalid { .. })
            ));
            assert_eq!(
                read_config_bytes(workspace)
                    .map_err(|error| error.to_string())?
                    .0,
                edited
            );
        }
        assert!(
            overlay_audits(
                &fixture.connection,
                WORKSPACE,
                DEMOTE_RETRIEVAL_WEIGHTS_AUDIT_ACTION
            )
            .map_err(|error| error.to_string())?
            .is_empty()
        );
        Ok(())
    }

    #[test]
    fn atomic_publication_refuses_an_interleaved_config_edit() -> TestResult {
        let directory = tempfile::tempdir_in("/tmp").map_err(|error| error.to_string())?;
        std::fs::create_dir_all(directory.path().join(".ee")).map_err(|error| error.to_string())?;
        let path = workspace_config_path(directory.path());
        std::fs::write(&path, "# original\n").map_err(|error| error.to_string())?;
        let result = atomic_shadow_write_authorized(
            &path,
            "# proposed overlay\n",
            Some("# original\n"),
            None,
            || std::fs::write(&path, "# concurrent edit\n").unwrap(),
        );
        assert!(result.is_err());
        assert_eq!(
            std::fs::read_to_string(&path).map_err(|error| error.to_string())?,
            "# concurrent edit\n"
        );
        Ok(())
    }

    #[test]
    fn evaluator_scores_the_real_unclamped_incumbent() -> TestResult {
        let actual = TuningWeights {
            lexical: 0.9,
            semantic: 0.1,
            graph: 0.0,
        };
        let labels = flip_fixture_label_report()?;
        let replays = flip_fixture_replays();
        let result =
            evaluate_fusion_candidates(&Cx::for_testing(), &replays, &labels.triples, actual)
                .map_err(|error| error.to_string())?;
        assert_eq!(result.incumbent.weights, actual);
        assert_ne!(result.incumbent.weights, actual.clamped());
        assert_eq!(
            result.incumbent.score,
            score_fusion_candidate(&replays, &labels.triples, actual)
        );
        for candidate in result.candidates {
            assert_eq!(candidate.weights, candidate.weights.clamped());
        }
        Ok(())
    }

    #[test]
    fn incumbent_configuration_preserves_explicit_weights() -> TestResult {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        std::fs::create_dir_all(tempdir.path().join(".ee")).map_err(|error| error.to_string())?;
        std::fs::write(
            tempdir.path().join(".ee/config.toml"),
            "[search]\nlexical_weight = 0.7\nsemantic_weight = 0.2\ngraph_weight = 0.1\n",
        )
        .map_err(|error| error.to_string())?;
        let weights = TuningWeights::incumbent_for_workspace(tempdir.path())
            .map_err(|error| error.to_string())?;
        assert!((weights.lexical - 0.7).abs() < f32::EPSILON);
        assert!((weights.semantic - 0.2).abs() < f32::EPSILON);
        assert!((weights.graph - 0.1).abs() < f32::EPSILON);
        Ok(())
    }

    #[test]
    fn invalid_incumbent_configuration_cannot_become_empty_evidence_abstention() -> TestResult {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let workspace = tempdir.path();
        std::fs::create_dir_all(workspace.join(".ee")).map_err(|error| error.to_string())?;
        std::fs::write(
            workspace.join(".ee/config.toml"),
            "[search]\nlexical_weight = \"not-a-weight\"\n",
        )
        .map_err(|error| error.to_string())?;
        let database = workspace.join(".ee/ee.db");
        let connection = DbConnection::open_file(&database).map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        let extraction = LabelExtractionConfig {
            label_window_minutes: DEFAULT_LABEL_WINDOW_MINUTES,
        };
        let gate = RetrievalTuningGateConfig::default();
        let result =
            crate::core::run_cli_with_cx(std::time::Duration::from_secs(30), |cx| async move {
                run_retrieval_tuning_with_cx(
                    &cx,
                    &connection,
                    workspace,
                    &database,
                    WORKSPACE,
                    ts(0),
                    &extraction,
                    &gate,
                )
                .await
            })
            .map_err(|error| error.to_string())?;
        match result {
            Err(ShadowTuningError::Configuration { .. }) => Ok(()),
            other => Err(format!(
                "invalid incumbent config must fail before the empty evidence gate: {other:?}"
            )),
        }
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
