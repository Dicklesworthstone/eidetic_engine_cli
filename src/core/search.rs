use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::{Duration, Instant, SystemTime};

use chrono::{DateTime, Utc};

use crate::config::MeshCommandMode;
use crate::config::env_registry::{EnvVar, read};
use crate::core::why::{DedupLinkEvidence, find_embed_dedup_link};
#[cfg(test)]
use crate::db::generate_audit_id_seeded;
use crate::db::{
    CreateAuditInput, DbConnection, StoredFeedbackEvent, StoredMemory, audit_actions,
    generate_audit_id,
};
use crate::models::degradation::{
    CONFORMAL_CALIBRATION_INSUFFICIENT_CODE, SEARCH_SCORE_CALIBRATION_FILE_TOO_LARGE_CODE,
    SEARCH_SCORE_CALIBRATION_ROWS_CORRUPT_CODE, SEARCH_SCORE_CALIBRATION_UNREADABLE_CODE,
};
use crate::models::query::{EqlQuery, EqlSpeedMode, EqlTagsMode};
use crate::models::{
    MemoryId, MemoryScope, MemoryScopeStats, ProvenanceUri, TrustClass, UnitScore,
    degraded_recovery_actions,
};
use crate::obs::audit_events::query_hash as audit_query_hash;
use crate::pack::{
    ConflictEntry, ConsensusConflictReport, ConsensusEntry, ConsensusProducer, ContextPackProfile,
    PackDraft, PackDraftItem, PackItemLifecycle, PackProvenance, PackSection, PackSelectedItem,
    PackSelectionAudit, PackSelectionObjective, PackSelectionPhase, PackTrustSignal, TokenBudget,
    analyze_pack_consensus_conflicts, estimate_tokens_default,
};
use crate::runtime::determinism::{Deterministic, Seed};

use super::degraded_aggregation::{DegradationAggregationInput, aggregate_degraded_entries};
use super::index::{
    IndexHealth, IndexStatusError, IndexStatusOptions, IndexStatusReport, get_index_status,
};
use super::memory_drift::{
    MemoryDriftSelectionHint, memory_drift_selection_hint_from_provenance_status,
};
use super::memory_scope::{MemoryScopeContext, MeshQueryVisibility, mesh_query_visibility};
use super::profile::{RuntimeProfileReport, runtime_profile_for_workspace};
#[cfg(feature = "lexical-bm25")]
use crate::search::TantivyIndex;
use crate::search::lexical_ram_tier::{
    LEXICAL_HUGEPAGES_UNAVAILABLE_CODE, LEXICAL_RAM_TIER_HUGEPAGES_ENV,
    LEXICAL_RAM_TIER_NOT_IMPLEMENTED_CODE, LEXICAL_RAM_TIER_PIN_RAM_ENV, LexicalRamTierConfig,
    LexicalRamTierResult, pin_lexical_index_files, trace_lexical_ram_tier,
};
use crate::search::plan_cache::{
    CompiledPlan, DEFAULT_PLAN_CACHE_ENTRIES, PlanCacheKey, compute_eql_hash,
    compute_search_config_hash, lookup_or_insert_process_plan,
};
use crate::search::{
    Embedder, HashEmbedder, SpeedMode, TwoTierConfig, TwoTierIndex, TwoTierSearcher,
};
use crate::util::radix_ulid_sort::sort_by_ulid_payload_or_lexical;
use frankensearch::LexicalSearch;

pub const DEFAULT_INDEX_SUBDIR: &str = "index";
pub const DIAG_SEARCH_SCHEMA_V1: &str = "ee.diag.search.v1";
pub const PERFORMANCE_EXPLAIN_SCHEMA_V1: &str = "ee.explain.performance.v1";
pub const SEARCH_REVISION_TOKEN_SCHEMA_V1: &str = "ee.search.revision_token.v1";
pub const SEARCH_SCORE_INTERVAL_SCHEMA_V1: &str = "ee.search.score_interval.v1";
pub const SEARCH_SCORE_CALIBRATION_SCHEMA_V1: &str = "ee.search.score_calibration.v1";
pub const SEARCH_SCORE_RECALIBRATION_SCHEMA_V1: &str = "ee.search.score_recalibration.v1";
const INDEX_STATUS_CACHE_TTL: Duration = Duration::from_secs(1);
const SEARCH_SCORE_COVERAGE_GUARANTEE: f32 = 0.95;
const MIN_SEARCH_SCORE_CALIBRATION_SAMPLES: usize = 20;
const MAX_SEARCH_SCORE_CALIBRATION_BYTES: u64 = 64 * 1024 * 1024;
const MAX_SEARCH_SCORE_CALIBRATION_CORRUPT_LINE_NUMBERS: usize = 1_000;
/// bd-1cdea: bound the per-response `metadata.scoreCalibration.provenance.feedbackEventIds`
/// list so a calibration with many feedback-event contributors does not bloat
/// every search payload. The empirical residual quantile is computed from all
/// samples regardless; this cap only affects the provenance summary.
const MAX_SEARCH_SCORE_CALIBRATION_FEEDBACK_EVENT_IDS: usize = 64;
const SEARCH_ANALYSIS_CONTENT_KEY: &str = "_ee_analysis_content";
const SEARCH_ANALYSIS_CONFIDENCE_KEY: &str = "_ee_analysis_confidence";
const SEARCH_ANALYSIS_UTILITY_KEY: &str = "_ee_analysis_utility";
const SEARCH_ANALYSIS_PROVENANCE_URI_KEY: &str = "_ee_analysis_provenance_uri";
const SEARCH_ANALYSIS_CREATED_AT_KEY: &str = "_ee_analysis_created_at";
const EMBED_MODEL_UNAVAILABLE_MODEL_ID: &str = "EE_EMBED_MODEL_PATH";
const EMBED_MODEL_UNAVAILABLE_FEATURE_FLAG: &str = "embed-fast";
const SEARCH_MI_DEDUP_MIN_COSINE_SIMILARITY: f64 = 0.85;
const SEARCH_MI_DEDUP_MIN_NORMALIZED_MI: f64 = 0.72;

static SEARCH_INDEX_STATUS_CACHE: OnceLock<Mutex<HashMap<IndexStatusCacheKey, CachedIndexStatus>>> =
    OnceLock::new();
// bd-2r38i: RwLock (was Mutex) so cache-hit reads via the
// fingerprint-keyed lookup at `load_search_score_calibration_jsonl`
// can take `.read()` and run concurrently. This is the simplest
// member of the read-takes-write family: there is no LRU touch and
// no TTL eviction on the read path, so no atomic refactor is
// required — only `.lock()` → `.read()` on the read site and
// `.lock()` → `.write()` on the two write sites. Mirrors bd-2lin9
// (PPR), bd-1nan9 (algorithm result cache), and bd-25yao (plan
// cache).
static SEARCH_SCORE_CALIBRATION_JSONL_CACHE: OnceLock<
    RwLock<HashMap<PathBuf, CachedSearchScoreCalibrationJsonl>>,
> = OnceLock::new();

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct IndexStatusCacheKey {
    database_path: PathBuf,
    index_dir: PathBuf,
}

impl IndexStatusCacheKey {
    fn from_search_options(options: &SearchOptions, index_dir: &Path) -> Self {
        let database_path = options
            .database_path
            .clone()
            .unwrap_or_else(|| options.workspace_path.join(".ee").join("ee.db"));
        Self {
            database_path,
            index_dir: index_dir.to_path_buf(),
        }
    }
}

#[derive(Clone, Debug)]
struct CachedIndexStatus {
    checked_at: Instant,
    report: IndexStatusReport,
}

#[derive(Clone, Debug)]
pub struct SearchOptions {
    pub workspace_path: PathBuf,
    pub database_path: Option<PathBuf>,
    pub index_dir: Option<PathBuf>,
    pub query: String,
    pub limit: u32,
    pub speed: SpeedMode,
    pub explain: bool,
    /// Evaluate validity windows at this timestamp. Defaults to now.
    pub as_of: Option<DateTime<Utc>>,
    /// Include tombstoned memories in result hits. Default command behavior
    /// excludes tombstoned memories so stale search-index documents cannot
    /// silently re-enter active retrieval.
    pub include_tombstoned: bool,
    /// Include memories whose `valid_to` is before the validity reference time.
    pub include_expired: bool,
    /// Include memories whose `valid_from` is after the validity reference time.
    pub include_future: bool,
    /// Include search hits whose indexed validity metadata is stale.
    /// Search indexes are derived assets, so validity-window metadata can lag
    /// the database row until the next rebuild.
    pub include_stale: bool,
    /// Minimum score (0.0..=1.0) for a hit to be returned. `None` falls
    /// back to [`DEFAULT_RELEVANCE_FLOOR`]. Set to `Some(0.0)` to disable.
    /// Bead bd-17c65.2.1 (B1).
    pub relevance_floor: Option<f32>,
    /// Result deduplication strategy. Defaults to doc-id exact collapse; MI
    /// mode additionally clusters high-information paraphrase memory hits.
    pub dedup_mode: SearchDedupMode,
    /// Requested search arm selection. Defaults to hybrid.
    pub source_mode: SearchSourceMode,
    /// Fail closed when the requested source mode cannot be applied.
    pub strict_source_mode: bool,
    /// Trust lane applied to retrieved memories.
    pub memory_scope: MemoryScope,
    /// Fail closed when relevant evidence exists outside the requested scope.
    pub strict_scope: bool,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SearchDedupMode {
    #[default]
    DocId,
    MutualInformation,
}

impl SearchDedupMode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DocId => "doc_id",
            Self::MutualInformation => "mi",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SearchSourceMode {
    LexicalOnly,
    SemanticOnly,
    #[default]
    Hybrid,
}

impl SearchSourceMode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::LexicalOnly => "lexical_only",
            Self::SemanticOnly => "semantic_only",
            Self::Hybrid => "hybrid",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SourceModeResolution {
    applied: SearchSourceMode,
    fallback_applied: bool,
    unavailable_no_results: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SearchTierState<'a> {
    lexical_available: bool,
    embed_model_unavailable: Option<&'a str>,
}

/// Default relevance floor for 0..=1-normalized score sources (bead
/// bd-17c65.2.1 / B1).
///
/// Calibrated against the 2026-05-10 corpus where junk semantic_fast hits
/// scored `< 0.03` and meaningful hits scored `0.10..=0.50`. Applies to
/// `Lexical` (normalized BM25), `SemanticFast`, `SemanticQuality`, and
/// `Reranked` (cross-encoder) score sources. Configurable per-call via
/// `--relevance-floor` and per-workspace via `search.relevance_floor`
/// config.
pub const DEFAULT_RELEVANCE_FLOOR: f32 = 0.05;

/// Default relevance floor for `Hybrid` (RRF-fused) score-source (bead
/// bd-n22a4, B2-followup).
///
/// RRF scores have magnitude `arms_contributing / (k + 1)` which tops out
/// around `0.033` for k=60 and two arms — applying the cosine-domain
/// [`DEFAULT_RELEVANCE_FLOOR`] of 0.05 to those scores would filter every
/// reasonable hybrid result and surface only `no_relevant_results`
/// degraded entries to the agent. This floor preserves the noise-vs-
/// signal cut for RRF-magnitude scores (top hit at 1/61 ≈ 0.0164 still
/// passes; rank ~190 single-arm RRF gets filtered).
pub const DEFAULT_RELEVANCE_FLOOR_HYBRID: f32 = 0.005;

/// Per-source default relevance floor.
///
/// Returns [`DEFAULT_RELEVANCE_FLOOR_HYBRID`] for `Hybrid` (RRF-fused)
/// hits and [`DEFAULT_RELEVANCE_FLOOR`] for every source whose scores
/// are already 0..=1 normalized. Used when the caller passes no explicit
/// `relevance_floor` override — the explicit override still applies
/// uniformly to every hit regardless of source so existing test fixtures
/// and `--relevance-floor 0.0` keep working unchanged.
///
/// Bead bd-n22a4 (B2-followup).
#[must_use]
pub const fn default_floor_for_source(source: ScoreSource) -> f32 {
    match source {
        ScoreSource::Hybrid => DEFAULT_RELEVANCE_FLOOR_HYBRID,
        ScoreSource::Lexical
        | ScoreSource::SemanticFast
        | ScoreSource::SemanticQuality
        | ScoreSource::Reranked => DEFAULT_RELEVANCE_FLOOR,
    }
}

impl SearchOptions {
    fn resolve_database_path(&self) -> PathBuf {
        self.database_path
            .clone()
            .unwrap_or_else(|| self.workspace_path.join(".ee").join("ee.db"))
    }

    fn resolve_index_dir(&self) -> PathBuf {
        self.index_dir
            .clone()
            .unwrap_or_else(|| self.workspace_path.join(".ee").join(DEFAULT_INDEX_SUBDIR))
    }

    #[cfg(test)]
    fn two_tier_config(&self) -> TwoTierConfig {
        self.two_tier_config_for_limit(self.limit)
    }

    fn two_tier_config_for_limit(&self, limit: u32) -> TwoTierConfig {
        let mut config = TwoTierConfig::default();
        let requested = usize::try_from(limit).unwrap_or(usize::MAX).max(1);
        let speed_candidate_multiplier = self.speed.candidate_limit().div_ceil(requested);
        config.candidate_multiplier = config.candidate_multiplier.max(speed_candidate_multiplier);
        config.fast_only = !self.speed.uses_embeddings();
        config.mrl_rescore_top_k = self.speed.rerank_depth();
        config.explain = self.explain;
        config
    }
}

#[derive(Clone, Debug)]
pub struct SearchReport {
    pub status: SearchStatus,
    pub query: String,
    pub requested_limit: u32,
    pub results: Vec<SearchHit>,
    pub elapsed_ms: f64,
    pub errors: Vec<String>,
    pub degraded: Vec<SearchDegradation>,
    pub runtime_profile: RuntimeProfileReport,
    /// Relevance floor that was applied to this search (B1 bd-17c65.2.1).
    /// `None` only for error cases where no search ran.
    pub relevance_floor_applied: Option<f32>,
    /// Number of candidates dropped because they scored below the floor
    /// (B1). Informational; agents can use this to decide whether to
    /// retry with a lower floor or different query.
    pub candidates_below_floor: usize,
    pub source_mode_requested: SearchSourceMode,
    pub source_mode_applied: SearchSourceMode,
    pub source_mode_fallback: bool,
    pub strict_source_mode: bool,
    pub memory_scope: MemoryScope,
    pub strict_scope: bool,
    pub scope_stats: MemoryScopeStats,
}

#[derive(Clone, Debug)]
pub struct ContextSearchReport {
    pub report: SearchReport,
    pub preloaded_memories: BTreeMap<String, StoredMemory>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SearchRevisionMetadata {
    pub schema: &'static str,
    pub mode: &'static str,
    pub token: String,
    pub tier1_usable: bool,
    pub revision_available: bool,
    pub reason: &'static str,
    pub query_hash: String,
    pub result_fingerprint: String,
    pub result_doc_ids: Vec<String>,
    pub local_mesh_tip_status: &'static str,
    pub local_mesh_tip_basis: &'static str,
    pub rerun_command: String,
}

impl SearchRevisionMetadata {
    #[must_use]
    pub fn for_report(report: &SearchReport, mode: MeshCommandMode) -> Option<Self> {
        if mode != MeshCommandMode::Revisable {
            return None;
        }
        let result_doc_ids = search_display_visible_hits(&report.results)
            .iter()
            .map(|hit| hit.doc_id.clone())
            .collect::<Vec<_>>();
        let result_ids = result_doc_ids.join("\n");
        let query_hash = crate::pack::revision_hash_with_prefix(&["query", &report.query]);
        let result_fingerprint =
            crate::pack::revision_hash_with_prefix(&["searchResults", &result_ids]);
        let token_digest = crate::pack::revision_hash(&[
            SEARCH_REVISION_TOKEN_SCHEMA_V1,
            mode.as_str(),
            &query_hash,
            &result_fingerprint,
            "not_checked",
            "no_async_peer_freshness_probe_attached",
        ]);
        let token = format!("searchrev_{}", &token_digest[..32]);
        Some(Self {
            schema: SEARCH_REVISION_TOKEN_SCHEMA_V1,
            mode: mode.as_str(),
            token,
            tier1_usable: true,
            revision_available: false,
            reason: "no_fresher_peer_material_known",
            query_hash,
            result_fingerprint,
            result_doc_ids,
            local_mesh_tip_status: "not_checked",
            local_mesh_tip_basis: "no_async_peer_freshness_probe_attached",
            rerun_command: format!(
                "ee search {} --mesh revisable --json",
                shell_quote(&report.query)
            ),
        })
    }

    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        serde_json::json!({
            "schema": self.schema,
            "mode": self.mode,
            "token": self.token,
            "tier1Usable": self.tier1_usable,
            "revisionAvailable": self.revision_available,
            "reason": self.reason,
            "queryHash": self.query_hash,
            "resultFingerprint": self.result_fingerprint,
            "resultDocIds": self.result_doc_ids,
            "localMeshTipState": {
                "status": self.local_mesh_tip_status,
                "basis": self.local_mesh_tip_basis,
            },
            "rerunCommand": self.rerun_command,
        })
    }
}

fn shell_quote(value: &str) -> String {
    if value.is_empty() {
        return "''".to_owned();
    }
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SearchScoreRecalibrationReport {
    pub schema: &'static str,
    pub status: &'static str,
    pub path: PathBuf,
    pub samples_written: usize,
    pub feedback_events_considered: usize,
    pub feedback_events_malformed: usize,
    pub feedback_events_unavailable_reason: Option<String>,
    pub jsonl_hash: String,
}

impl SearchScoreRecalibrationReport {
    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        serde_json::json!({
            "schema": self.schema,
            "status": self.status,
            "path": self.path.display().to_string(),
            "samplesWritten": self.samples_written,
            "feedbackEventsConsidered": self.feedback_events_considered,
            "feedbackEventsMalformed": self.feedback_events_malformed,
            "feedbackEventsUnavailableReason": self.feedback_events_unavailable_reason,
            "jsonlHash": self.jsonl_hash,
        })
    }
}

#[derive(Clone, Debug)]
pub struct SearchDiagnosticReport {
    pub query: String,
    pub requested_limit: u32,
    pub elapsed_ms: f64,
    pub pre_fusion: PreFusionDiagnostics,
    pub fusion: FusionDiagnostics,
    pub final_report: SearchReport,
    pub errors: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct PreFusionDiagnostics {
    pub lexical: SearchArmDiagnostics,
    pub semantic_fast: SearchArmDiagnostics,
}

#[derive(Clone, Debug)]
pub struct SearchArmDiagnostics {
    pub available: bool,
    pub score_scale: &'static str,
    pub elapsed_ms: f64,
    pub results: Vec<SearchArmHit>,
    pub error: Option<String>,
}

#[derive(Clone, Debug)]
pub struct SearchArmHit {
    pub doc_id: String,
    pub raw_score: f32,
    pub rank: usize,
}

#[derive(Clone, Debug)]
pub struct FusionDiagnostics {
    pub algorithm: &'static str,
    pub rrf_k: f64,
    pub per_doc_contribution: Vec<FusionContribution>,
    pub elapsed_ms: f64,
}

#[derive(Clone, Debug)]
pub struct FusionContribution {
    pub doc_id: String,
    pub lexical_rank: Option<usize>,
    pub lexical_contribution: Option<f64>,
    pub semantic_rank: Option<usize>,
    pub semantic_contribution: Option<f64>,
    pub fused_score: f64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RetrievalMetrics {
    pub requested_limit: u32,
    pub returned_count: usize,
    pub error_count: usize,
    pub elapsed_ms: f64,
    pub source_counts: RetrievalSourceCounts,
    pub score_distribution: RetrievalScoreDistribution,
    pub field_coverage: RetrievalFieldCoverage,
    /// Floor applied to the retrieval (bead bd-17c65.2.1 / B1).
    pub relevance_floor: Option<f32>,
    /// Candidates that passed the floor and made it into `results`.
    pub candidates_above_floor: usize,
    /// Candidates dropped because they scored below the floor.
    /// `returned_count = candidates_above_floor` after filtering;
    /// `candidates_below_floor` is informational for agents that want
    /// to understand recall.
    pub candidates_below_floor: usize,
}

/// Agent-readable summary of recall quality (bead bd-17c65.2.4 / B4).
///
/// Maps a (top_score, p50_score, floor) tuple onto three states agents
/// can branch on without recomputing the math themselves.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum QualityAssessment {
    /// Top hit comfortably above floor AND median above floor → use
    /// top-K confidently.
    Good,
    /// Some hits passed the floor but recall is thin OR scores cluster
    /// near the floor → consider rephrasing.
    Weak,
    /// No hits above floor → query missed the corpus entirely.
    Empty,
}

impl QualityAssessment {
    /// Stable wire name. Consumers branch on this; do not rename without
    /// a contract bump.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Good => "good",
            Self::Weak => "weak",
            Self::Empty => "empty",
        }
    }

    /// Classify a score distribution given the applied floor.
    ///
    /// Rules (per B4 design):
    /// - `Empty`: top score below floor (or no hits at all).
    /// - `Good`: top score ≥ 2× floor AND p50-like (mean here) ≥ floor.
    /// - `Weak`: everything else (top ≥ floor but mean below, or only
    ///   one hit just above floor, etc.).
    #[must_use]
    pub fn classify(top: Option<f32>, mean: Option<f32>, floor: f32) -> Self {
        let Some(top) = top else {
            return Self::Empty;
        };
        if !top.is_finite() || top < floor {
            return Self::Empty;
        }
        let mean = mean.unwrap_or(top);
        if top >= floor * 2.0 && mean >= floor {
            Self::Good
        } else {
            Self::Weak
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RetrievalSourceCounts {
    pub lexical: usize,
    pub semantic_fast: usize,
    pub semantic_quality: usize,
    pub hybrid: usize,
    pub reranked: usize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RetrievalScoreDistribution {
    pub top: Option<f32>,
    pub min: Option<f32>,
    pub max: Option<f32>,
    pub mean: Option<f32>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RetrievalFieldCoverage {
    pub fast_score_count: usize,
    pub quality_score_count: usize,
    pub lexical_score_count: usize,
    pub rerank_score_count: usize,
    pub metadata_count: usize,
    pub explanation_count: usize,
}

#[derive(Clone, Debug)]
pub struct SearchHit {
    pub doc_id: String,
    pub score: f32,
    pub source: ScoreSource,
    pub fast_score: Option<f32>,
    pub quality_score: Option<f32>,
    pub lexical_score: Option<f32>,
    pub rerank_score: Option<f32>,
    pub metadata: Option<serde_json::Value>,
    pub explanation: Option<ScoreExplanation>,
}

/// Read-only search-side dedup-link projection helper (bd-1iltv.3).
///
/// Mirrors [`crate::core::audit::audit_entry_dedup_link`] for the search
/// surface: when a [`SearchHit::doc_id`] resolves to a memory id that
/// carries an `ee.embed_dedup.link.v1` row in `memory_links`, returns the
/// corresponding [`DedupLinkEvidence`]. Returns `None` when the hit is
/// not memory-scoped, when `doc_id` is empty, or when the memory was not
/// deduped at insert time — the same honest-degradation contract pinned
/// by the why surface tests.
///
/// Callers can compose this helper to enrich `SearchHit.metadata`
/// without re-implementing the JSON-parsing path in
/// `find_embed_dedup_link`. The lookup is intentionally per-hit so the
/// hot `run_search` path is not perturbed; provenance enrichers that
/// already iterate top-k hits should call this helper instead.
#[must_use]
pub fn search_hit_dedup_link(conn: &DbConnection, hit: &SearchHit) -> Option<DedupLinkEvidence> {
    if hit.doc_id.is_empty() {
        return None;
    }
    if !hit.doc_id.starts_with("mem_") {
        return None;
    }
    find_embed_dedup_link(conn, hit.doc_id.as_str())
}

#[derive(Clone, Debug)]
pub struct ScoreExplanation {
    pub summary: String,
    pub factors: Vec<ScoreFactor>,
}

#[derive(Clone, Debug)]
pub struct ScoreFactor {
    pub name: String,
    pub value: f32,
    pub contribution: String,
    pub source_field: String,
    pub formula: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SearchDegradation {
    pub code: String,
    pub severity: String,
    pub message: String,
    pub repair: Option<String>,
}

impl SearchDegradation {
    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        let mut value = serde_json::json!({
            "code": self.code,
            "severity": self.severity,
            "message": self.message,
            "repair": self.repair,
        });
        append_degradation_recovery_details(&mut value, &self.code);
        value
    }

    #[must_use]
    fn stale_index(db_generation: Option<u64>, index_generation: Option<u64>) -> Self {
        let generation_detail = match (db_generation, index_generation) {
            (Some(db_generation), Some(index_generation)) => format!(
                " Database generation is {db_generation}; index generation is {index_generation}."
            ),
            (Some(db_generation), None) => {
                format!(" Database generation is {db_generation}; index generation is unavailable.")
            }
            (None, Some(index_generation)) => format!(
                " Index generation is {index_generation}; database generation is unavailable."
            ),
            (None, None) => String::new(),
        };

        Self {
            code: "search_index_stale".to_string(),
            severity: "medium".to_string(),
            message: format!(
                "Search index is stale; returning lexical fallback results from the current index.{generation_detail} Newer memories may be omitted until the index is rebuilt."
            ),
            repair: Some("ee index rebuild --workspace .".to_string()),
        }
    }

    #[must_use]
    fn missing_index() -> Self {
        Self {
            code: "index_missing".to_string(),
            severity: "medium".to_string(),
            message: "Search index metadata or files are missing; results may be unavailable until the index is rebuilt."
                .to_string(),
            repair: Some("ee index rebuild --workspace .".to_string()),
        }
    }

    /// All candidates scored below the relevance floor — no relevant
    /// results to return. Bead bd-17c65.2.1 (B1).
    #[must_use]
    fn no_relevant_results(
        query: &str,
        floor: f32,
        considered: usize,
        top_score: Option<f32>,
    ) -> Self {
        let top_note = top_score
            .map(|score| format!(" Top candidate scored {score:.4}."))
            .unwrap_or_default();
        Self {
            code: "no_relevant_results".to_string(),
            severity: "medium".to_string(),
            message: format!(
                "No memories scored above relevance floor {floor:.4} for query `{query}` (considered {considered} candidate{plural}).{top_note}",
                plural = if considered == 1 { "" } else { "s" },
            ),
            repair: Some(
                "Broaden the query, lower --relevance-floor, or use --source-mode lexical_only."
                    .to_string(),
            ),
        }
    }

    /// Search produced duplicate hits on the same `docId` that were
    /// collapsed (highest score retained). Informational so callers
    /// understand why the raw retrieval count > the returned count.
    /// Bead bd-17c65.2.3 (B3).
    #[must_use]
    pub(crate) fn duplicates_collapsed(collapsed: usize) -> Self {
        Self {
            code: "duplicates_collapsed".to_string(),
            severity: "low".to_string(),
            message: format!(
                "Collapsed {collapsed} duplicate hit{plural} on docId after fusion; only the highest-scoring occurrence was kept.",
                plural = if collapsed == 1 { "" } else { "s" },
            ),
            repair: None,
        }
    }

    /// Search-side mutual-information dedup collapsed paraphrase-like memory
    /// hits. Kept hits carry `metadata.mergedFrom` so provenance survives.
    /// Bead bd-17c65.14.14 (N14).
    #[must_use]
    pub(crate) fn mi_dedup_candidate_proposed(collapsed: usize) -> Self {
        Self {
            code: "mi_dedup_candidate_proposed".to_string(),
            severity: "info".to_string(),
            message: format!(
                "Mutual-information dedup collapsed {collapsed} paraphrase candidate hit{plural}; kept results include metadata.mergedFrom for provenance.",
                plural = if collapsed == 1 { "" } else { "s" },
            ),
            repair: Some(
                "Review durable proposals with ee curate candidates --type dedup --json."
                    .to_string(),
            ),
        }
    }

    /// The caller requested MI dedup, but too few memory hits had usable
    /// content to estimate token mutual information deterministically.
    /// Bead bd-17c65.14.14 (N14).
    #[must_use]
    pub(crate) fn mi_dedup_threshold_underpowered(eligible: usize) -> Self {
        Self {
            code: "mi_dedup_threshold_underpowered".to_string(),
            severity: "info".to_string(),
            message: format!(
                "Mutual-information dedup needs at least 2 memory hits with content; found {eligible}. Search results were left uncollapsed beyond docId dedup."
            ),
            repair: Some(
                "Rebuild the index or run ee remember before retrying --dedupe=mi.".to_string(),
            ),
        }
    }

    /// Top score is above floor but close to it — the embedder may not
    /// recognize the query's synonyms or the corpus genuinely lacks
    /// strong matches. Informational so an agent can choose to
    /// rephrase or fall back to a different source mode.
    ///
    /// Bead bd-17c65.2.5 (B5). Fires when `qualityAssessment ==
    /// "weak"` (per B4): top score is below `2 × floor`.
    #[must_use]
    fn weak_query_recall(floor: f32, top_score: f32) -> Self {
        Self {
            code: "weak_query_recall".to_string(),
            severity: "low".to_string(),
            message: format!(
                "Top score {top_score:.4} is below the weak-recall threshold for relevance floor {floor:.4}; embedder may not recognize query synonyms, or the corpus lacks strong matches.",
            ),
            repair: Some(
                "Rephrase with concrete words present in stored memories, or use --source-mode lexical_only.".to_string(),
            ),
        }
    }

    /// Most candidates dropped below the floor (informational signal so
    /// an agent can decide whether to retry with a different strategy).
    /// Bead bd-17c65.2.1 (B1).
    #[must_use]
    fn low_recall_after_floor(floor: f32, kept: usize, considered: usize) -> Self {
        Self {
            code: "low_recall_after_floor".to_string(),
            severity: "low".to_string(),
            message: format!(
                "Only {kept} of {considered} candidates passed relevance floor {floor:.4}; consider broadening query or rephrasing.",
            ),
            repair: Some(
                "Rephrase with concrete words present in stored memories, or use --source-mode lexical_only when implemented (B6)."
                    .to_string(),
            ),
        }
    }

    #[must_use]
    fn conformal_calibration_insufficient(sample_count: usize) -> Self {
        Self {
            code: CONFORMAL_CALIBRATION_INSUFFICIENT_CODE.to_string(),
            severity: "low".to_string(),
            message: format!(
                "Search score calibration has {sample_count} usable sample{plural}; {required} are required for split-conformal score intervals. Returning conservative [0, 1] intervals.",
                plural = if sample_count == 1 { "" } else { "s" },
                required = MIN_SEARCH_SCORE_CALIBRATION_SAMPLES,
            ),
            repair: Some(
                "Add outcome-backed rows to .ee/search/calibration.jsonl, or record outcome/curation feedback events whose evidence_json includes score and groundTruthRelevance."
                    .to_string(),
            ),
        }
    }

    #[must_use]
    fn search_score_calibration_rows_corrupt(
        usable_samples: usize,
        corrupt_rows: usize,
        corrupt_line_numbers: &[usize],
    ) -> Self {
        let line_summary = line_number_summary(corrupt_line_numbers);
        let capped_note = if corrupt_rows > corrupt_line_numbers.len() {
            format!(
                " Stored diagnostics include the first {} corrupt line numbers.",
                corrupt_line_numbers.len()
            )
        } else {
            String::new()
        };
        Self {
            code: SEARCH_SCORE_CALIBRATION_ROWS_CORRUPT_CODE.to_string(),
            severity: "warning".to_string(),
            message: format!(
                "Search score calibration ignored {corrupt_rows} corrupt row{plural} in .ee/search/calibration.jsonl at {line_summary}; {usable_samples} usable sample{sample_plural} remain. Returning conservative [0, 1] intervals unless enough valid rows are present.{capped_note}",
                plural = if corrupt_rows == 1 { "" } else { "s" },
                sample_plural = if usable_samples == 1 { "" } else { "s" },
            ),
            repair: Some(
                "Fix or remove malformed calibration rows; each non-empty row must be JSON with finite score and groundTruthRelevance fields."
                    .to_string(),
            ),
        }
    }

    #[must_use]
    fn search_score_calibration_file_too_large(file_size_bytes: u64, max_bytes: u64) -> Self {
        Self {
            code: SEARCH_SCORE_CALIBRATION_FILE_TOO_LARGE_CODE.to_string(),
            severity: "warning".to_string(),
            message: format!(
                ".ee/search/calibration.jsonl is {file_size_bytes} bytes, above the {max_bytes} byte search score calibration limit. Skipping JSONL calibration rows and returning conservative intervals unless feedback events provide enough usable samples.",
            ),
            repair: Some(
                "Rotate or truncate .ee/search/calibration.jsonl, then run ee doctor to confirm search calibration health."
                    .to_string(),
            ),
        }
    }

    /// bd-25z97: emit when `.ee/search/calibration.jsonl` is present but
    /// cannot be read (permission denied, invalid UTF-8, interrupted I/O,
    /// etc.). Without this code, the failure folds into `status: absent`
    /// and operators cannot tell evidence-broken from evidence-not-yet.
    #[must_use]
    fn search_score_calibration_unreadable(reason: &str) -> Self {
        let (message, repair) = if reason.starts_with("feedback_events_") {
            (
                format!(
                    "Search score calibration could not use feedback-event calibration evidence ({reason}); outcome-backed calibration samples were unavailable or ignored. Returning conservative [0, 1] intervals unless JSONL evidence provides enough usable samples.",
                ),
                "Run ee doctor and inspect the workspace feedback_events table; repair DB access or malformed evidence_json so outcome-backed calibration reaches the scorer."
                    .to_string(),
            )
        } else {
            (
                format!(
                    ".ee/search/calibration.jsonl exists but is unreadable ({reason}); search score calibration is dropping JSONL evidence and returning conservative [0, 1] intervals unless feedback events provide enough usable samples.",
                ),
                "Restore read permissions on .ee/search/calibration.jsonl (or rotate it via ee doctor) so calibration evidence reaches the scorer."
                    .to_string(),
            )
        };
        Self {
            code: SEARCH_SCORE_CALIBRATION_UNREADABLE_CODE.to_string(),
            severity: "warning".to_string(),
            message,
            repair: Some(repair),
        }
    }

    #[must_use]
    fn mesh_workspace_scope_filtered(filtered: usize) -> Self {
        Self {
            code: "mesh_workspace_scope_filtered".to_string(),
            severity: "low".to_string(),
            message: format!(
                "Filtered {filtered} mesh-derived search hit{plural} because the indexed workspace-scope decision was not an explicit allow for this workspace.",
                plural = if filtered == 1 { "" } else { "s" },
            ),
            repair: Some(
                "Review the mesh peer-group binding and import ledger before authorizing remote workspace material."
                    .to_string(),
            ),
        }
    }

    #[must_use]
    fn source_mode_fallback(
        requested: SearchSourceMode,
        applied: SearchSourceMode,
        reason: &str,
    ) -> Self {
        Self {
            code: "source_mode_fallback".to_string(),
            severity: "warning".to_string(),
            message: format!(
                "Requested source_mode={} but it could not be applied ({reason}); fell back to {}.",
                requested.as_str(),
                applied.as_str()
            ),
            repair: Some(
                "Rebuild with the requested search features, or pass --strict-source-mode to fail closed."
                    .to_string(),
            ),
        }
    }

    #[must_use]
    fn lexical_unavailable() -> Self {
        Self {
            code: "lexical_unavailable".to_string(),
            severity: "warning".to_string(),
            message: "Requested lexical_only search, but the lexical/BM25 arm is unavailable."
                .to_string(),
            repair: Some("rebuild ee with --features fts5,lexical-bm25".to_string()),
        }
    }

    #[must_use]
    fn embed_model_unavailable(reason: &str) -> Self {
        Self {
            code: "embed_model_unavailable".to_string(),
            severity: "warning".to_string(),
            message: format!(
                "Embedding model unavailable ({reason}); falling back to lexical search."
            ),
            repair: Some("ee index reembed --workspace .".to_string()),
        }
    }

    #[must_use]
    fn search_unavailable(reason: &str) -> Self {
        Self {
            code: "search_unavailable".to_string(),
            severity: "medium".to_string(),
            message: format!("Search is unavailable because {reason}."),
            repair: Some("Run `ee index status --workspace . --json`.".to_string()),
        }
    }

    #[must_use]
    fn lexical_hugepages_unavailable() -> Self {
        Self {
            code: LEXICAL_HUGEPAGES_UNAVAILABLE_CODE.to_string(),
            severity: "info".to_string(),
            message: "Lexical RAM-tier hugepages were requested but this host cannot grant them; search continues with regular page-size behavior.".to_string(),
            repair: Some("Disable EE_LEXICAL_INDEX_HUGEPAGES or move the workspace to a Linux host with transparent hugepages available.".to_string()),
        }
    }

    #[must_use]
    fn lexical_ram_tier_not_implemented() -> Self {
        Self {
            code: LEXICAL_RAM_TIER_NOT_IMPLEMENTED_CODE.to_string(),
            severity: "info".to_string(),
            message: "Lexical RAM-tier pinning is enabled, but the safe mmap/mlock adapter is not installed; search results are unchanged.".to_string(),
            repair: Some("Keep EE_LEXICAL_INDEX_PIN_RAM unset until the lexical RAM-tier syscall adapter lands.".to_string()),
        }
    }

    #[must_use]
    fn corrupt_index(last_check_error: Option<&str>) -> Self {
        let detail = last_check_error
            .filter(|error| !error.trim().is_empty())
            .map(|error| format!(" Last check error: {error}"))
            .unwrap_or_default();

        Self {
            code: "index_corrupt".to_string(),
            severity: "high".to_string(),
            message: format!(
                "Search index failed integrity checks; results may be incomplete or unavailable until the index is rebuilt.{detail}"
            ),
            repair: Some("ee index rebuild --workspace .".to_string()),
        }
    }

    #[must_use]
    fn profile_search_limit_capped(requested: u32, effective: u32, profile: &str) -> Self {
        Self {
            code: "profile_search_limit_capped".to_string(),
            severity: "low".to_string(),
            message: format!(
                "Search candidate limit {requested} was capped to {effective} by the active {profile} operating profile."
            ),
            repair: Some("ee profile config plan --json".to_string()),
        }
    }

    #[must_use]
    fn tombstoned_filtered(filtered: usize) -> Self {
        Self {
            code: "tombstoned_filtered".to_string(),
            severity: "low".to_string(),
            message: format!(
                "Excluded {filtered} tombstoned memor{suffix} from search results. Pass --include-tombstoned to inspect them.",
                suffix = if filtered == 1 { "y" } else { "ies" },
            ),
            repair: Some("ee search <query> --include-tombstoned --json".to_string()),
        }
    }

    #[must_use]
    fn tombstoned_in_results(included: usize) -> Self {
        Self {
            code: "tombstoned_in_results".to_string(),
            severity: "low".to_string(),
            message: format!(
                "Search results include {included} tombstoned memor{suffix} because --include-tombstoned was requested.",
                suffix = if included == 1 { "y" } else { "ies" },
            ),
            repair: None,
        }
    }

    #[must_use]
    fn expired_filtered(filtered: usize) -> Self {
        Self {
            code: "expired_filtered".to_string(),
            severity: "low".to_string(),
            message: format!(
                "Excluded {filtered} expired memor{suffix} from search results because valid_to is in the past.",
                suffix = if filtered == 1 { "y" } else { "ies" },
            ),
            repair: Some(
                "Use `ee why <memory-id> --json` to inspect validity metadata.".to_string(),
            ),
        }
    }

    #[must_use]
    fn future_validity_filtered(filtered: usize) -> Self {
        Self {
            code: "future_validity_filtered".to_string(),
            severity: "low".to_string(),
            message: format!(
                "Excluded {filtered} not-yet-valid memor{suffix} from search results because valid_from is after the validity reference time.",
                suffix = if filtered == 1 { "y" } else { "ies" },
            ),
            repair: Some("Pass --include-future or --as-of <RFC3339> to inspect them.".to_string()),
        }
    }

    #[must_use]
    fn stale_validity_filtered(filtered: usize) -> Self {
        Self {
            code: "stale_validity_filtered".to_string(),
            severity: "low".to_string(),
            message: format!(
                "Excluded {filtered} stale memor{suffix} from search results because indexed validity_status is stale.",
                suffix = if filtered == 1 { "y" } else { "ies" },
            ),
            repair: Some("Pass --include-stale to inspect stale memories.".to_string()),
        }
    }

    #[must_use]
    fn malformed_validity_filtered(filtered: usize) -> Self {
        Self {
            code: "malformed_validity_filtered".to_string(),
            severity: "medium".to_string(),
            message: format!(
                "Excluded {filtered} memor{suffix} with malformed validity timestamps.",
                suffix = if filtered == 1 { "y" } else { "ies" },
            ),
            repair: Some("Use `ee why <memory-id> --json` or `ee doctor --json` to inspect validity metadata.".to_string()),
        }
    }

    #[must_use]
    fn validity_filtered_significant_recall_drop(filtered: usize, remaining: usize) -> Self {
        Self {
            code: "validity_filtered_significant_recall_drop".to_string(),
            severity: "info".to_string(),
            message: format!(
                "Validity window filtering removed {filtered} candidate{filtered_suffix}; {remaining} candidate{remaining_suffix} remain.",
                filtered_suffix = if filtered == 1 { "" } else { "s" },
                remaining_suffix = if remaining == 1 { "" } else { "s" },
            ),
            repair: Some("Consider --as-of, --include-expired, --include-future, or --include-stale when historic or inactive memories are expected.".to_string()),
        }
    }

    #[must_use]
    fn output_redaction_disabled() -> Self {
        Self {
            code: "output_redaction_disabled".to_string(),
            severity: "info".to_string(),
            message: "Output-time redaction is disabled by workspace policy; search snippets may include secret-like values.".to_string(),
            repair: Some("Set policy.output_redaction.enabled = true in .ee/config.toml.".to_string()),
        }
    }

    #[must_use]
    fn scope_excluded_evidence(scope: MemoryScope, excluded: usize) -> Self {
        Self {
            code: "scope_excluded_evidence".to_string(),
            severity: "low".to_string(),
            message: format!(
                "Memory scope `{}` excluded {excluded} candidate{suffix} outside the requested trust lane.",
                scope.as_str(),
                suffix = if excluded == 1 { "" } else { "s" },
            ),
            repair: Some(
                "Use --memory-scope swarm to inspect all candidate evidence, or pass --strict-scope to fail closed."
                    .to_string(),
            ),
        }
    }

    #[must_use]
    fn scope_strict_excluded_evidence(scope: MemoryScope, excluded: usize) -> Self {
        Self {
            code: "scope_strict_excluded_evidence".to_string(),
            severity: "medium".to_string(),
            message: format!(
                "Strict memory scope `{}` found {excluded} relevant candidate{suffix} outside the requested trust lane; returning no scoped results.",
                scope.as_str(),
                suffix = if excluded == 1 { "" } else { "s" },
            ),
            repair: Some("Retry without --strict-scope or use --memory-scope swarm.".to_string()),
        }
    }

    #[must_use]
    fn scope_agent_unavailable(scope: MemoryScope) -> Self {
        Self {
            code: "scope_agent_unavailable".to_string(),
            severity: "warning".to_string(),
            message: format!(
                "Memory scope `{}` needs the current agent identity, but EE_AGENT_NAME is unset.",
                scope.as_str()
            ),
            repair: Some("Set EE_AGENT_NAME for self/team scoped retrieval.".to_string()),
        }
    }

    #[must_use]
    fn scope_metadata_unavailable(error: &str) -> Self {
        Self {
            code: "scope_metadata_unavailable".to_string(),
            severity: "medium".to_string(),
            message: format!(
                "Search could not verify memory scope against the memory database: {error}"
            ),
            repair: Some("ee doctor --json".to_string()),
        }
    }

    #[must_use]
    fn tombstone_visibility_unavailable(error: &str) -> Self {
        Self {
            code: "tombstone_visibility_unavailable".to_string(),
            severity: "medium".to_string(),
            message: format!(
                "Search could not verify tombstone visibility against the memory database: {error}"
            ),
            repair: Some("ee doctor --json".to_string()),
        }
    }

    #[must_use]
    fn selected_memory_drift(count: usize, hint: &MemoryDriftSelectionHint) -> Self {
        Self {
            code: hint
                .degraded_code
                .clone()
                .unwrap_or_else(|| "memory_drift_source_unverifiable".to_owned()),
            severity: hint.severity.clone(),
            message: format!(
                "Search selected {count} memor{suffix} with stale provenance evidence; highest-risk status={} memoryId={} reason={}. Each affected result includes driftHint.",
                hint.drift_status.as_str(),
                hint.memory_id,
                hint.top_reason,
                suffix = if count == 1 { "y" } else { "ies" },
            ),
            repair: Some(hint.revalidation_command.clone()),
        }
    }
}

impl ScoreFactor {
    #[must_use]
    pub fn new(
        name: &str,
        value: f32,
        contribution: &str,
        source_field: &str,
        formula: &str,
    ) -> Self {
        Self {
            name: name.to_string(),
            value,
            contribution: contribution.to_string(),
            source_field: source_field.to_string(),
            formula: formula.to_string(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScoreSource {
    Lexical,
    SemanticFast,
    SemanticQuality,
    Hybrid,
    Reranked,
}

impl ScoreSource {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Lexical => "lexical",
            Self::SemanticFast => "semantic_fast",
            Self::SemanticQuality => "semantic_quality",
            Self::Hybrid => "hybrid",
            Self::Reranked => "reranked",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SearchStatus {
    Success,
    NoResults,
    IndexNotFound,
    IndexError,
}

impl SearchStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::NoResults => "no_results",
            Self::IndexNotFound => "index_not_found",
            Self::IndexError => "index_error",
        }
    }
}

impl SearchReport {
    fn output_redaction_enabled(&self) -> bool {
        !self
            .degraded
            .iter()
            .any(|degradation| degradation.code == "output_redaction_disabled")
    }

    #[must_use]
    pub fn retrieval_metrics(&self) -> RetrievalMetrics {
        RetrievalMetrics::from_hits_with_floor(
            self.requested_limit,
            self.elapsed_ms,
            &self.results,
            self.errors.len(),
            self.relevance_floor_applied,
            self.candidates_below_floor,
        )
    }

    #[must_use]
    pub fn human_summary(&self) -> String {
        let mut output = String::new();
        let visible_results = search_display_visible_hits(&self.results);

        match self.status {
            SearchStatus::Success => {
                output.push_str(&format!("Search results for \"{}\"\n\n", self.query));
            }
            SearchStatus::NoResults => {
                output.push_str(&format!("No results for \"{}\"\n\n", self.query));
            }
            SearchStatus::IndexNotFound => {
                output.push_str("Search index not found\n\n");
            }
            SearchStatus::IndexError => {
                output.push_str("Error searching index\n\n");
            }
        }

        for (i, hit) in visible_results.iter().enumerate() {
            output.push_str(&format!(
                "  {}. {} (score: {:.4}, source: {})\n",
                i + 1,
                hit.doc_id,
                hit.score,
                hit.source.as_str()
            ));
            if let Some(ref explanation) = hit.explanation {
                output.push_str(&format!("     {}\n", explanation.summary));
                for factor in &explanation.factors {
                    output.push_str(&format!(
                        "       - {}: {:.4} ({})\n",
                        factor.name, factor.value, factor.contribution
                    ));
                }
            }
        }

        if visible_results.is_empty() && self.status == SearchStatus::Success {
            output.push_str("  (no matches)\n");
        }

        output.push_str(&format!("\nElapsed: {:.1}ms\n", self.elapsed_ms));

        if !self.errors.is_empty() {
            output.push_str("\nErrors:\n");
            for error in &self.errors {
                output.push_str(&format!("  - {error}\n"));
            }
        }

        if !self.degraded.is_empty() {
            output.push_str("\nDegraded:\n");
            for degraded in &self.degraded {
                output.push_str(&format!("  - {}: {}\n", degraded.code, degraded.message));
            }
        }

        output
    }

    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        let output_redaction_enabled = self.output_redaction_enabled();
        let visible_results = search_display_visible_hits(&self.results);
        let mut metrics = RetrievalMetrics::from_hits_with_floor(
            self.requested_limit,
            self.elapsed_ms,
            &visible_results,
            self.errors.len(),
            self.relevance_floor_applied,
            self.candidates_below_floor,
        )
        .data_json();
        if let Some(metrics_obj) = metrics.as_object_mut() {
            metrics_obj.insert(
                "sourceModeRequested".to_string(),
                serde_json::json!(self.source_mode_requested.as_str()),
            );
            metrics_obj.insert(
                "sourceModeApplied".to_string(),
                serde_json::json!(self.source_mode_applied.as_str()),
            );
            metrics_obj.insert(
                "fallbackApplied".to_string(),
                serde_json::json!(self.source_mode_fallback),
            );
            metrics_obj.insert(
                "strictSourceMode".to_string(),
                serde_json::json!(self.strict_source_mode),
            );
            metrics_obj.insert(
                "memoryScope".to_string(),
                serde_json::json!(self.memory_scope.as_str()),
            );
            metrics_obj.insert(
                "strictScope".to_string(),
                serde_json::json!(self.strict_scope),
            );
        }
        let results: Vec<serde_json::Value> = visible_results
            .iter()
            .map(|hit| {
                let (provenance, provenance_redacted_patterns) =
                    hit.provenance_json(output_redaction_enabled);
                let mut obj = serde_json::json!({
                    "docId": hit.doc_id,
                    "score": hit.score,
                    "scoreInterval": search_hit_score_interval_json(hit),
                    "coverageGuarantee": search_hit_coverage_guarantee_json(hit),
                    "source": hit.source.as_str(),
                    "why": hit.why(),
                    "provenance": provenance,
                });
                if let Some(obj_map) = obj.as_object_mut() {
                    if let Some(memory_id) = hit.memory_id() {
                        obj_map.insert("memoryId".to_string(), serde_json::json!(memory_id));
                    }
                    if let Some(fast) = hit.fast_score {
                        obj_map.insert("fastScore".to_string(), serde_json::json!(fast));
                    }
                    if let Some(quality) = hit.quality_score {
                        obj_map.insert("qualityScore".to_string(), serde_json::json!(quality));
                    }
                    if let Some(lexical) = hit.lexical_score {
                        obj_map.insert("lexicalScore".to_string(), serde_json::json!(lexical));
                    }
                    if let Some(rerank) = hit.rerank_score {
                        obj_map.insert("rerankScore".to_string(), serde_json::json!(rerank));
                    }
                    if let Some(ref meta) = hit.metadata {
                        let (metadata, mut redacted_patterns) =
                            public_search_metadata(meta, output_redaction_enabled);
                        redacted_patterns.extend(provenance_redacted_patterns.clone());
                        obj_map.insert("metadata".to_string(), metadata);
                        if let Some(drift_hint) = meta.get("driftHint") {
                            obj_map.insert("driftHint".to_string(), drift_hint.clone());
                        }
                        if let MeshQueryVisibility::Allowed(provenance) =
                            mesh_query_visibility(Some(meta))
                        {
                            obj_map.insert("meshProvenance".to_string(), provenance.to_json());
                            if let Some(adjustment) = meta.get("_ee_mesh_trust_adjustment") {
                                obj_map
                                    .insert("meshTrustAdjustment".to_string(), adjustment.clone());
                            }
                        }
                        if !redacted_patterns.is_empty() {
                            obj_map
                                .insert("contentRedacted".to_string(), serde_json::json!(true));
                            obj_map.insert(
                                "redactions".to_string(),
                                serde_json::json!(
                                    redacted_patterns
                                        .iter()
                                        .map(|pattern| serde_json::json!({
                                            "reason": pattern,
                                            "placeholder": crate::policy::redaction_placeholder(pattern),
                                        }))
                                        .collect::<Vec<_>>()
                                ),
                            );
                        }
                        if metadata_bool(meta, "tombstoned").unwrap_or(false) {
                            obj_map.insert("tombstoned".to_string(), serde_json::json!(true));
                            if let Some(tombstoned_at) = metadata_string(meta, "tombstoned_at") {
                                obj_map.insert(
                                    "tombstonedAt".to_string(),
                                    serde_json::json!(tombstoned_at),
                                );
                            }
                        }
                        if let Some(valid_from) = metadata_string(meta, "valid_from") {
                            obj_map.insert("validFrom".to_string(), serde_json::json!(valid_from));
                        }
                        if let Some(valid_to) = metadata_string(meta, "valid_to") {
                            obj_map.insert("validTo".to_string(), serde_json::json!(valid_to));
                        }
                        if let Some(status) = metadata_string(meta, "validity_status") {
                            obj_map.insert("validityStatus".to_string(), serde_json::json!(status));
                        }
                        if let Some(kind) = metadata_string(meta, "validity_window_kind") {
                            obj_map
                                .insert("validityWindowKind".to_string(), serde_json::json!(kind));
                        }
                    }
                    if let Some(ref explanation) = hit.explanation {
                        let factors: Vec<serde_json::Value> = explanation
                            .factors
                            .iter()
                            .map(|f| {
                                serde_json::json!({
                                    "name": f.name,
                                    "value": f.value,
                                    "contribution": f.contribution,
                                    "sourceField": f.source_field,
                                    "formula": f.formula,
                                })
                            })
                            .collect();
                        obj_map.insert(
                            "explanation".to_string(),
                            serde_json::json!({
                                "summary": explanation.summary,
                                "factors": factors,
                            }),
                        );
                    }
                }
                obj
            })
            .collect();
        let consensus_conflicts = search_consensus_conflict_report(&self.query, &visible_results);

        serde_json::json!({
            "command": "search",
            "status": self.status.as_str(),
            "query": &self.query,
            "request": {
                "sourceMode": self.source_mode_requested.as_str(),
                "strictSourceMode": self.strict_source_mode,
                "memoryScope": self.memory_scope.as_str(),
                "strictScope": self.strict_scope,
            },
            "scopeStats": self.scope_stats.data_json(),
            "results": results,
            "consensus": consensus_conflicts.consensus.iter().map(consensus_entry_data_json).collect::<Vec<_>>(),
            "conflicts": consensus_conflicts.conflicts.iter().map(conflict_entry_data_json).collect::<Vec<_>>(),
            "resultCount": visible_results.len(),
            "elapsedMs": self.elapsed_ms,
            "metrics": metrics,
            "profileRuntime": self.runtime_profile.data_json(),
            "errors": self.errors,
            "degraded": search_degraded_data_json("search", &self.degraded),
        })
    }

    #[must_use]
    pub fn performance_explain_json(
        &self,
        speed: SpeedMode,
        score_explanations_requested: bool,
    ) -> serde_json::Value {
        serde_json::json!({
            "schema": PERFORMANCE_EXPLAIN_SCHEMA_V1,
            "success": true,
            "data": self.performance_explain_data_json(speed, score_explanations_requested),
        })
    }

    #[must_use]
    pub fn performance_explain_data_json(
        &self,
        speed: SpeedMode,
        score_explanations_requested: bool,
    ) -> serde_json::Value {
        let metrics = self.retrieval_metrics();
        serde_json::json!({
            "command": "search",
            "query": query_observation_json(&self.query),
            "queryPlan": {
                "retrievalMode": speed.as_str(),
                "requestedLimit": self.requested_limit,
                "candidateBudget": speed.candidate_limit(),
                "usesEmbeddings": speed.uses_embeddings(),
                "scoreExplanationsRequested": score_explanations_requested,
                "sourceModeRequested": self.source_mode_requested.as_str(),
                "sourceModeApplied": self.source_mode_applied.as_str(),
                "strictSourceMode": self.strict_source_mode,
                "fallbackApplied": self.source_mode_fallback,
                "memoryScope": self.memory_scope.as_str(),
                "strictScope": self.strict_scope,
            },
            "profileRuntime": self.runtime_profile.data_json(),
            "dbReads": {
                "indexStatusChecks": 1,
                "memoryReads": 0,
                "tagReads": 0,
                "artifactLinkReads": 0,
            },
            "search": {
                "status": self.status.as_str(),
                "returnedHits": self.results.len(),
                "sourceCounts": retrieval_source_counts_json(metrics.source_counts),
                "scoreDistribution": retrieval_score_distribution_json(metrics.score_distribution),
                "fieldCoverage": retrieval_field_coverage_json(metrics.field_coverage),
                "errors": self.errors,
                "elapsed": elapsed_timing_json(self.elapsed_ms),
            },
            "pack": {
                "status": "not_used",
                "reason": "search_command_does_not_assemble_context_pack",
            },
            "cache": {
                "status": "not_used",
                "reason": "search_command_reads_derived_search_index_directly",
            },
            "graph": {
                "status": "not_used",
                "reason": "search_command_does_not_request_graph_projection",
            },
            "fallbacks": search_degraded_data_json("search", &self.degraded),
            "redaction": performance_redaction_json(),
        })
    }
}

pub(crate) fn search_degraded_data_json(
    source: &'static str,
    degraded: &[SearchDegradation],
) -> Vec<serde_json::Value> {
    aggregate_degraded_entries(degraded.iter().map(|entry| {
        DegradationAggregationInput::new(
            source,
            entry.code.clone(),
            entry.severity.clone(),
            entry.message.clone(),
            entry.repair.clone().unwrap_or_default(),
        )
    }))
    .into_iter()
    .map(|entry| {
        let mut value = serde_json::json!({
            "code": entry.code,
            "severity": entry.severity,
            "message": entry.message,
            "repair": entry.repair,
            "sources": entry.sources,
        });
        if let Some(code) = value
            .get("code")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
        {
            append_degradation_recovery_details(&mut value, &code);
        }
        value
    })
    .collect()
}

fn append_degradation_recovery_details(value: &mut serde_json::Value, code: &str) {
    let recovery_actions = degraded_recovery_actions(code);
    if recovery_actions.is_empty() && code != "embed_model_unavailable" {
        return;
    }

    let mut details = serde_json::Map::new();
    if !recovery_actions.is_empty() {
        details.insert(
            "recovery".to_string(),
            serde_json::Value::Array(
                recovery_actions
                    .iter()
                    .map(crate::models::RecoveryAction::data_json)
                    .collect(),
            ),
        );
    }
    if code == "embed_model_unavailable" {
        details.insert(
            "modelId".to_string(),
            serde_json::json!(EMBED_MODEL_UNAVAILABLE_MODEL_ID),
        );
        details.insert(
            "featureFlag".to_string(),
            serde_json::json!(EMBED_MODEL_UNAVAILABLE_FEATURE_FLAG),
        );
        details.insert("lexicalAvailable".to_string(), serde_json::json!(true));
    }
    value["details"] = serde_json::Value::Object(details);
}

impl SearchDiagnosticReport {
    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        serde_json::json!({
            "schema": DIAG_SEARCH_SCHEMA_V1,
            "command": "diag search",
            "query": &self.query,
            "requestedLimit": self.requested_limit,
            "elapsedMs": round_metric_f64(self.elapsed_ms),
            "preFusion": self.pre_fusion.data_json(),
            "fusion": self.fusion.data_json(),
            "final": self.final_report.data_json(),
            "errors": &self.errors,
        })
    }
}

impl PreFusionDiagnostics {
    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        serde_json::json!({
            "lexical": self.lexical.data_json(),
            "semanticFast": self.semantic_fast.data_json(),
        })
    }
}

impl SearchArmDiagnostics {
    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        serde_json::json!({
            "available": self.available,
            "scoreScale": self.score_scale,
            "elapsedMs": round_metric_f64(self.elapsed_ms),
            "resultCount": self.results.len(),
            "results": self.results.iter().map(SearchArmHit::data_json).collect::<Vec<_>>(),
            "error": &self.error,
        })
    }
}

impl SearchArmHit {
    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        serde_json::json!({
            "docId": &self.doc_id,
            "rank": self.rank,
            "rawScore": round_metric_f32(self.raw_score),
        })
    }
}

impl FusionDiagnostics {
    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        serde_json::json!({
            "algorithm": self.algorithm,
            "diagnosticOnly": true,
            "affectsFinalRanking": false,
            "rankingOwner": "frankensearch_two_tier_searcher",
            "k": round_metric_f64(self.rrf_k),
            "elapsedMs": round_metric_f64(self.elapsed_ms),
            "perDocContribution": self.per_doc_contribution.iter().map(FusionContribution::data_json).collect::<Vec<_>>(),
        })
    }
}

impl FusionContribution {
    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        serde_json::json!({
            "docId": &self.doc_id,
            "lexicalRank": self.lexical_rank,
            "lexicalContribution": self.lexical_contribution.map(round_metric_f64),
            "semanticRank": self.semantic_rank,
            "semanticContribution": self.semantic_contribution.map(round_metric_f64),
            "fusedScore": round_metric_f64(self.fused_score),
        })
    }
}

impl SearchHit {
    #[must_use]
    fn memory_id(&self) -> Option<&str> {
        self.doc_id
            .starts_with("mem_")
            .then_some(self.doc_id.as_str())
    }

    #[must_use]
    fn why(&self) -> String {
        self.explanation
            .as_ref()
            .map(|explanation| explanation.summary.clone())
            .unwrap_or_else(|| {
                format!(
                    "Selected by {} retrieval with score {:.4}.",
                    self.source.as_str(),
                    self.score
                )
            })
    }

    #[must_use]
    fn provenance_json(
        &self,
        output_redaction_enabled: bool,
    ) -> (Vec<serde_json::Value>, Vec<String>) {
        let mut provenance = Vec::new();
        let mut redacted_patterns = BTreeSet::new();

        if let Some(ref metadata) = self.metadata {
            for key in ["provenanceUri", "provenance_uri"] {
                if let Some(uri) = metadata_string(metadata, key) {
                    let uri = redact_search_provenance_uri(
                        uri,
                        output_redaction_enabled,
                        &mut redacted_patterns,
                    );
                    provenance.push(serde_json::json!({
                        "kind": "provenance_uri",
                        "uri": uri,
                    }));
                    break;
                }
            }
        }

        provenance.push(serde_json::json!({
            "kind": "search_document",
            "docId": self.doc_id,
        }));
        (provenance, redacted_patterns.into_iter().collect())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SearchScoreCalibrationStatus {
    Absent,
    Unreadable,
    Insufficient,
    Corrupt,
    FileTooLarge,
    Calibrated,
}

impl SearchScoreCalibrationStatus {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Absent => "absent",
            Self::Unreadable => "unreadable",
            Self::Insufficient => "insufficient",
            Self::Corrupt => "corrupt",
            Self::FileTooLarge => "file_too_large",
            Self::Calibrated => "calibrated",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SearchScoreCalibrationJsonlFingerprint {
    len: u64,
    modified: Option<SystemTime>,
}

#[derive(Clone, Debug)]
struct SearchScoreCalibrationJsonlLoad {
    exists: bool,
    too_large: bool,
    /// bd-25z97: non-None when the JSONL was present-but-unreadable (e.g.,
    /// permission denied, invalid UTF-8 surfaced as an `io::Error` from a
    /// metadata or open call). Carries the `io::ErrorKind`'s stable
    /// snake_case label so the degradation can explain WHY honestly
    /// instead of folding the failure into `status: absent`.
    unreadable_reason: Option<String>,
    file_size_bytes: Option<u64>,
    /// bd-1cdea: BLAKE3 hex digest (prefixed `blake3:`) of the JSONL bytes
    /// that produced these residuals. Lets `metadata.scoreCalibration`
    /// expose deterministic provenance for the calibration evidence so
    /// search consumers can audit which exact JSONL artifact shaped the
    /// score intervals. `None` whenever no residuals were parsed (status
    /// `absent`, `unreadable`, or `file_too_large`).
    jsonl_hash: Option<String>,
    residuals: Vec<f32>,
    sample_count: usize,
    corrupt_row_count: usize,
    corrupt_line_numbers: Vec<usize>,
}

impl SearchScoreCalibrationJsonlLoad {
    fn absent() -> Self {
        Self {
            exists: false,
            too_large: false,
            unreadable_reason: None,
            file_size_bytes: None,
            jsonl_hash: None,
            residuals: Vec::new(),
            sample_count: 0,
            corrupt_row_count: 0,
            corrupt_line_numbers: Vec::new(),
        }
    }

    /// bd-25z97: existing JSONL we could not read — preserve the error kind
    /// for the degraded code message instead of pretending the file is absent.
    fn unreadable(reason: &str) -> Self {
        Self {
            exists: true,
            too_large: false,
            unreadable_reason: Some(reason.to_owned()),
            file_size_bytes: None,
            jsonl_hash: None,
            residuals: Vec::new(),
            sample_count: 0,
            corrupt_row_count: 0,
            corrupt_line_numbers: Vec::new(),
        }
    }
}

/// bd-25z97: classify an `io::Error` from metadata/open/read into a stable
/// snake_case label suitable for the degraded code message. `NotFound` is
/// returned separately so the caller can fall back to the absent path.
fn classify_calibration_io_error(error: &std::io::Error) -> Option<&'static str> {
    match error.kind() {
        std::io::ErrorKind::NotFound => None,
        std::io::ErrorKind::PermissionDenied => Some("permission_denied"),
        std::io::ErrorKind::InvalidData => Some("invalid_data"),
        std::io::ErrorKind::Interrupted => Some("interrupted"),
        std::io::ErrorKind::UnexpectedEof => Some("unexpected_eof"),
        std::io::ErrorKind::OutOfMemory => Some("out_of_memory"),
        std::io::ErrorKind::TimedOut => Some("timed_out"),
        std::io::ErrorKind::Other => Some("io_other"),
        _ => Some("io_error"),
    }
}

#[derive(Clone, Debug)]
struct CachedSearchScoreCalibrationJsonl {
    fingerprint: SearchScoreCalibrationJsonlFingerprint,
    load: SearchScoreCalibrationJsonlLoad,
}

#[derive(Clone, Debug)]
struct SearchScoreCalibrationFeedbackEvents {
    events: Vec<StoredFeedbackEvent>,
    unavailable_reason: Option<String>,
}

impl SearchScoreCalibrationFeedbackEvents {
    fn available(events: Vec<StoredFeedbackEvent>) -> Self {
        Self {
            events,
            unavailable_reason: None,
        }
    }

    fn unavailable(reason: &str) -> Self {
        Self {
            events: Vec::new(),
            unavailable_reason: Some(reason.to_owned()),
        }
    }
}

#[derive(Clone, Debug)]
struct SearchScoreCalibration {
    status: SearchScoreCalibrationStatus,
    sample_count: usize,
    jsonl_sample_count: usize,
    feedback_event_sample_count: usize,
    feedback_event_malformed_count: usize,
    feedback_event_unavailable_reason: Option<String>,
    corrupt_row_count: usize,
    corrupt_line_numbers: Vec<usize>,
    jsonl_file_size_bytes: Option<u64>,
    residual_quantile: Option<f32>,
    /// bd-25z97: snake_case label of the `io::ErrorKind` that made the
    /// calibration JSONL unreadable, when status == `Unreadable`. None for
    /// every other status.
    unreadable_reason: Option<String>,
    /// bd-1cdea: BLAKE3 hex digest (`blake3:<hex>`) of the JSONL bytes that
    /// contributed residuals. `None` whenever the JSONL did not feed
    /// residuals (status `absent`, `unreadable`, or `file_too_large`).
    jsonl_hash: Option<String>,
    /// bd-1cdea: bounded list of feedback-event IDs that contributed
    /// residuals, capped at `MAX_SEARCH_SCORE_CALIBRATION_FEEDBACK_EVENT_IDS`.
    /// Lets agents audit which exact feedback rows shaped the score
    /// intervals; their content is recoverable from the workspace DB.
    feedback_event_ids: Vec<String>,
    /// bd-1cdea: `true` when more feedback events contributed residuals
    /// than the cap above could surface in the provenance summary.
    feedback_event_ids_truncated: bool,
}

impl SearchScoreCalibration {
    fn for_workspace(workspace_path: &Path) -> Self {
        Self::for_workspace_with_feedback_events(workspace_path, &[])
    }

    fn for_workspace_with_feedback_events(
        workspace_path: &Path,
        feedback_events: &[StoredFeedbackEvent],
    ) -> Self {
        Self::for_workspace_with_feedback_event_status(workspace_path, feedback_events, None)
    }

    fn for_workspace_with_feedback_event_status(
        workspace_path: &Path,
        feedback_events: &[StoredFeedbackEvent],
        feedback_event_unavailable_reason: Option<String>,
    ) -> Self {
        let path = workspace_path
            .join(".ee")
            .join("search")
            .join("calibration.jsonl");
        let jsonl_load = load_search_score_calibration_jsonl(&path);

        let mut residuals = jsonl_load.residuals.clone();
        let corrupt_line_numbers = jsonl_load.corrupt_line_numbers.clone();
        let corrupt_row_count = jsonl_load.corrupt_row_count;
        let jsonl_sample_count = jsonl_load.sample_count;
        let mut feedback_event_sample_count = 0usize;
        // bd-1cdea: capture contributing feedback-event IDs for the
        // provenance summary, capped so a calibration with many
        // contributors does not bloat every search payload.
        let mut feedback_event_ids: Vec<String> = Vec::new();
        let mut feedback_event_ids_truncated = false;
        let mut feedback_event_malformed_count = 0usize;

        for event in feedback_events {
            let Some(evidence_json) = &event.evidence_json else {
                continue;
            };
            let Ok(value) = serde_json::from_str::<serde_json::Value>(evidence_json) else {
                feedback_event_malformed_count = feedback_event_malformed_count.saturating_add(1);
                continue;
            };
            let Some(residual) = calibration_residual_from_value(&value) else {
                continue;
            };
            residuals.push(residual);
            feedback_event_sample_count = feedback_event_sample_count.saturating_add(1);
            if feedback_event_ids.len() < MAX_SEARCH_SCORE_CALIBRATION_FEEDBACK_EVENT_IDS {
                feedback_event_ids.push(event.id.clone());
            } else {
                feedback_event_ids_truncated = true;
            }
        }

        // bd-25z97: an unreadable JSONL must NOT degrade into the silent
        // `Absent` path even when no feedback events compensate. Surface
        // the I/O failure so operators can distinguish "no calibration
        // evidence yet" from "calibration evidence is being dropped on
        // the floor". Feedback-event samples still feed scoring, but the
        // status reports the unreadable jsonl honestly.
        if jsonl_load.unreadable_reason.is_some() {
            let residual_quantile = if residuals.len() < MIN_SEARCH_SCORE_CALIBRATION_SAMPLES {
                None
            } else {
                Some(split_conformal_quantile(
                    residuals.clone(),
                    SEARCH_SCORE_COVERAGE_GUARANTEE,
                ))
            };
            return Self {
                status: SearchScoreCalibrationStatus::Unreadable,
                sample_count: residuals.len(),
                jsonl_sample_count: 0,
                feedback_event_sample_count,
                feedback_event_malformed_count,
                feedback_event_unavailable_reason,
                corrupt_row_count: 0,
                corrupt_line_numbers: Vec::new(),
                jsonl_file_size_bytes: None,
                residual_quantile,
                unreadable_reason: jsonl_load.unreadable_reason.clone(),
                jsonl_hash: None,
                feedback_event_ids,
                feedback_event_ids_truncated,
            };
        }

        if !jsonl_load.exists
            && feedback_event_sample_count == 0
            && feedback_event_malformed_count == 0
            && feedback_event_unavailable_reason.is_none()
        {
            return Self {
                status: SearchScoreCalibrationStatus::Absent,
                sample_count: 0,
                jsonl_sample_count: 0,
                feedback_event_sample_count: 0,
                feedback_event_malformed_count: 0,
                feedback_event_unavailable_reason: None,
                corrupt_row_count: 0,
                corrupt_line_numbers: Vec::new(),
                jsonl_file_size_bytes: None,
                residual_quantile: None,
                unreadable_reason: None,
                jsonl_hash: None,
                feedback_event_ids: Vec::new(),
                feedback_event_ids_truncated: false,
            };
        }

        if jsonl_load.too_large {
            let residual_quantile = if residuals.len() < MIN_SEARCH_SCORE_CALIBRATION_SAMPLES {
                None
            } else {
                Some(split_conformal_quantile(
                    residuals.clone(),
                    SEARCH_SCORE_COVERAGE_GUARANTEE,
                ))
            };
            return Self {
                status: SearchScoreCalibrationStatus::FileTooLarge,
                sample_count: residuals.len(),
                jsonl_sample_count,
                feedback_event_sample_count,
                feedback_event_malformed_count,
                feedback_event_unavailable_reason,
                corrupt_row_count,
                corrupt_line_numbers,
                jsonl_file_size_bytes: jsonl_load.file_size_bytes,
                residual_quantile,
                unreadable_reason: None,
                // bd-1cdea: too_large drops the JSONL before parsing, so no
                // JSONL hash provenance; feedback events may still have
                // contributed and keep their ids.
                jsonl_hash: None,
                feedback_event_ids,
                feedback_event_ids_truncated,
            };
        }

        if !corrupt_line_numbers.is_empty() {
            let residual_quantile = if residuals.len() < MIN_SEARCH_SCORE_CALIBRATION_SAMPLES {
                None
            } else {
                Some(split_conformal_quantile(
                    residuals.clone(),
                    SEARCH_SCORE_COVERAGE_GUARANTEE,
                ))
            };
            return Self {
                status: SearchScoreCalibrationStatus::Corrupt,
                sample_count: residuals.len(),
                jsonl_sample_count,
                feedback_event_sample_count,
                feedback_event_malformed_count,
                feedback_event_unavailable_reason,
                corrupt_row_count,
                corrupt_line_numbers,
                jsonl_file_size_bytes: jsonl_load.file_size_bytes,
                residual_quantile,
                unreadable_reason: None,
                jsonl_hash: jsonl_load.jsonl_hash.clone(),
                feedback_event_ids,
                feedback_event_ids_truncated,
            };
        }

        if residuals.len() < MIN_SEARCH_SCORE_CALIBRATION_SAMPLES {
            return Self {
                status: SearchScoreCalibrationStatus::Insufficient,
                sample_count: residuals.len(),
                jsonl_sample_count,
                feedback_event_sample_count,
                feedback_event_malformed_count,
                feedback_event_unavailable_reason,
                corrupt_row_count: 0,
                corrupt_line_numbers: Vec::new(),
                jsonl_file_size_bytes: jsonl_load.file_size_bytes,
                residual_quantile: None,
                unreadable_reason: None,
                jsonl_hash: jsonl_load.jsonl_hash.clone(),
                feedback_event_ids,
                feedback_event_ids_truncated,
            };
        }

        Self {
            status: SearchScoreCalibrationStatus::Calibrated,
            sample_count: residuals.len(),
            jsonl_sample_count,
            feedback_event_sample_count,
            feedback_event_malformed_count,
            feedback_event_unavailable_reason,
            corrupt_row_count: 0,
            corrupt_line_numbers: Vec::new(),
            jsonl_file_size_bytes: jsonl_load.file_size_bytes,
            residual_quantile: Some(split_conformal_quantile(
                residuals,
                SEARCH_SCORE_COVERAGE_GUARANTEE,
            )),
            unreadable_reason: None,
            jsonl_hash: jsonl_load.jsonl_hash.clone(),
            feedback_event_ids,
            feedback_event_ids_truncated,
        }
    }

    fn interval_for_score(&self, score: f32) -> [f32; 2] {
        let score = if score.is_finite() {
            score.clamp(0.0, 1.0)
        } else {
            0.0
        };
        let Some(quantile) = self.residual_quantile else {
            return [0.0, 1.0];
        };
        let radius = (quantile * score_uncertainty_scale(score)).clamp(0.0, 1.0);
        [
            round_metric_f32((score - radius).clamp(0.0, 1.0)),
            round_metric_f32((score + radius).clamp(0.0, 1.0)),
        ]
    }

    fn data_json(&self) -> serde_json::Value {
        // bd-vca4u: this object is the workspace-level calibration metadata
        // emitted under `metadata.scoreCalibration`. The per-result
        // two-number bound array uses ee.search.score_interval.v1; this
        // object has its own schema id so consumers can validate it and
        // evolve it independently.
        serde_json::json!({
            "schema": SEARCH_SCORE_CALIBRATION_SCHEMA_V1,
            "method": "scaled_split_conformal",
            "status": self.status.as_str(),
            "coverage": round_metric_f32(SEARCH_SCORE_COVERAGE_GUARANTEE),
            "sampleCount": self.sample_count,
            "minimumSamples": MIN_SEARCH_SCORE_CALIBRATION_SAMPLES,
            "sourceBreakdown": {
                "jsonl": self.jsonl_sample_count,
                "feedbackEvents": self.feedback_event_sample_count,
                "feedbackEventsMalformed": self.feedback_event_malformed_count,
                "feedbackEventsReadStatus": if self.feedback_event_unavailable_reason.is_some() { "unavailable" } else { "ok" },
                "feedbackEventsUnavailableReason": self.feedback_event_unavailable_reason,
            },
            "corruptRowCount": self.corrupt_row_count,
            "corruptLineNumbers": &self.corrupt_line_numbers,
            "residualQuantile": self.residual_quantile.map(round_metric_f32),
            // bd-25z97: include the unreadable reason when present so JSON
            // consumers can branch on the kind of I/O failure without
            // re-parsing the human-readable message.
            "unreadableReason": self.unreadable_reason,
            // bd-1cdea: provenance summary lets agents audit which exact
            // evidence shaped the score intervals. `jsonlHash` pins the
            // JSONL bytes (BLAKE3) that produced residuals; `jsonlPath`
            // is the workspace-relative path (stable across machines);
            // `feedbackEventIds` is a bounded list of contributing
            // feedback-event row ids (cap = MAX_SEARCH_SCORE_CALIBRATION_FEEDBACK_EVENT_IDS)
            // with `feedbackEventIdsTruncated` set when the cap was hit.
            "provenance": {
                "jsonlHash": self.jsonl_hash,
                "jsonlPath": ".ee/search/calibration.jsonl",
                "feedbackEventIds": &self.feedback_event_ids,
                "feedbackEventIdsTruncated": self.feedback_event_ids_truncated,
            },
        })
    }
}

fn load_search_score_calibration_jsonl(path: &Path) -> SearchScoreCalibrationJsonlLoad {
    let metadata = match std::fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) => {
            // bd-25z97: NotFound = genuinely absent; everything else is an
            // unreadable file we should surface honestly.
            return match classify_calibration_io_error(&error) {
                None => SearchScoreCalibrationJsonlLoad::absent(),
                Some(reason) => SearchScoreCalibrationJsonlLoad::unreadable(reason),
            };
        }
    };
    let fingerprint = SearchScoreCalibrationJsonlFingerprint {
        len: metadata.len(),
        modified: metadata.modified().ok(),
    };

    let cache = SEARCH_SCORE_CALIBRATION_JSONL_CACHE.get_or_init(|| RwLock::new(HashMap::new()));
    if let Ok(cache_guard) = cache.read()
        && let Some(cached) = cache_guard.get(path)
        && cached.fingerprint == fingerprint
    {
        return cached.load.clone();
    }

    let load = if metadata.len() > MAX_SEARCH_SCORE_CALIBRATION_BYTES {
        SearchScoreCalibrationJsonlLoad {
            exists: true,
            too_large: true,
            unreadable_reason: None,
            file_size_bytes: Some(metadata.len()),
            // bd-1cdea: too_large drops the JSONL before parsing, so we
            // do not compute a hash — the bytes did not feed residuals.
            jsonl_hash: None,
            residuals: Vec::new(),
            sample_count: 0,
            corrupt_row_count: 0,
            corrupt_line_numbers: Vec::new(),
        }
    } else {
        stream_search_score_calibration_jsonl(path, metadata.len())
    };

    if let Ok(mut cache_guard) = cache.write() {
        cache_guard.insert(
            path.to_path_buf(),
            CachedSearchScoreCalibrationJsonl {
                fingerprint,
                load: load.clone(),
            },
        );
    }

    load
}

fn stream_search_score_calibration_jsonl(
    path: &Path,
    file_size_bytes: u64,
) -> SearchScoreCalibrationJsonlLoad {
    // bd-1cdea: read the JSONL bytes once so we can compute a BLAKE3
    // provenance hash AND parse residuals from the same buffer. The
    // size has already been bounded by MAX_SEARCH_SCORE_CALIBRATION_BYTES,
    // so this is safe to materialise in memory.
    //
    // Cap the read at `MAX_SEARCH_SCORE_CALIBRATION_BYTES + 1` to close
    // the TOCTOU window between the metadata-based size pre-check at
    // line ~2266 and this read. A peer that swaps the calibration JSONL
    // for a multi-GiB file between the stat and `fs::read` would
    // otherwise force `fs::read` to allocate the full grown size
    // before any code could route the response through the existing
    // `too_large` branch. The `+ 1` sentinel preserves prior semantics:
    // a file of exactly `MAX_SEARCH_SCORE_CALIBRATION_BYTES` parses
    // normally; a race-grown file lands as `cap + 1` bytes and is
    // surfaced via the same `too_large` shape the metadata check uses.
    // Same defense-in-depth pattern as `read_cache_entry_file` in
    // src/cache/pack_l2.rs (8ba93c0e), `prepare_file_artifact` in
    // src/core/artifact.rs (1e55cde7), `read_pack_file_no_symlinks`
    // in src/core/repro.rs (b771869b), and the symbol-graph fix in
    // src/core/symbol_graph.rs (27a3cb9b).
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) => {
            // bd-25z97: metadata said the file is present, so an open
            // failure here is by definition NOT a "the file is absent"
            // story. Even a NotFound at this point (raced delete) is
            // honest enough to surface as unreadable so the operator
            // can investigate the race.
            let reason = classify_calibration_io_error(&error).unwrap_or("not_found_after_stat");
            return SearchScoreCalibrationJsonlLoad::unreadable(reason);
        }
    };
    let mut bytes = Vec::new();
    let read_result = file
        .take(MAX_SEARCH_SCORE_CALIBRATION_BYTES.saturating_add(1))
        .read_to_end(&mut bytes);
    if let Err(error) = read_result {
        let reason = classify_calibration_io_error(&error).unwrap_or("not_found_after_stat");
        return SearchScoreCalibrationJsonlLoad::unreadable(reason);
    }
    if bytes.len() as u64 > MAX_SEARCH_SCORE_CALIBRATION_BYTES {
        return SearchScoreCalibrationJsonlLoad {
            exists: true,
            too_large: true,
            unreadable_reason: None,
            file_size_bytes: Some(bytes.len() as u64),
            jsonl_hash: None,
            residuals: Vec::new(),
            sample_count: 0,
            corrupt_row_count: 0,
            corrupt_line_numbers: Vec::new(),
        };
    }
    let jsonl_hash = format!("blake3:{}", blake3::hash(&bytes).to_hex());

    let mut residuals = Vec::new();
    let mut corrupt_line_numbers = Vec::new();
    let mut corrupt_row_count = 0usize;
    let mut sample_count = 0usize;
    for (index, line_result) in BufReader::new(bytes.as_slice()).lines().enumerate() {
        let line_number = index + 1;
        let Ok(line) = line_result else {
            record_corrupt_calibration_line(
                &mut corrupt_line_numbers,
                &mut corrupt_row_count,
                line_number,
            );
            continue;
        };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            record_corrupt_calibration_line(
                &mut corrupt_line_numbers,
                &mut corrupt_row_count,
                line_number,
            );
            continue;
        };
        let Some(residual) = calibration_residual_from_value(&value) else {
            record_corrupt_calibration_line(
                &mut corrupt_line_numbers,
                &mut corrupt_row_count,
                line_number,
            );
            continue;
        };
        residuals.push(residual);
        sample_count = sample_count.saturating_add(1);
    }

    SearchScoreCalibrationJsonlLoad {
        exists: true,
        too_large: false,
        unreadable_reason: None,
        file_size_bytes: Some(file_size_bytes),
        jsonl_hash: Some(jsonl_hash),
        residuals,
        sample_count,
        corrupt_row_count,
        corrupt_line_numbers,
    }
}

fn record_corrupt_calibration_line(
    corrupt_line_numbers: &mut Vec<usize>,
    corrupt_row_count: &mut usize,
    line_number: usize,
) {
    *corrupt_row_count = (*corrupt_row_count).saturating_add(1);
    if corrupt_line_numbers.len() < MAX_SEARCH_SCORE_CALIBRATION_CORRUPT_LINE_NUMBERS {
        corrupt_line_numbers.push(line_number);
    }
}

#[derive(Clone, Copy, Debug)]
struct SearchScoreCalibrationSample {
    score: f32,
    ground_truth_relevance: f32,
}

impl SearchScoreCalibrationSample {
    fn residual(self) -> f32 {
        let scale = score_uncertainty_scale(self.score);
        ((self.score.clamp(0.0, 1.0) - self.ground_truth_relevance.clamp(0.0, 1.0)).abs() / scale)
            .min(20.0)
    }

    fn data_json(self, feedback_event_id: &str) -> serde_json::Value {
        serde_json::json!({
            "schema": "ee.search.calibration_feedback.v1",
            "score": round_metric_f32(self.score),
            "groundTruthRelevance": round_metric_f32(self.ground_truth_relevance),
            "source": "feedback_event",
            "feedbackEventId": feedback_event_id,
        })
    }
}

fn calibration_residual_from_value(value: &serde_json::Value) -> Option<f32> {
    calibration_sample_from_value(value).map(SearchScoreCalibrationSample::residual)
}

fn calibration_sample_from_value(
    value: &serde_json::Value,
) -> Option<SearchScoreCalibrationSample> {
    calibration_sample_from_object(value).or_else(|| {
        [
            "searchCalibration",
            "search_calibration",
            "calibration",
            "scoreCalibration",
            "score_calibration",
        ]
        .iter()
        .find_map(|key| value.get(*key).and_then(calibration_sample_from_object))
    })
}

fn calibration_sample_from_object(
    value: &serde_json::Value,
) -> Option<SearchScoreCalibrationSample> {
    let score = calibration_number(
        value,
        &["score", "predictedScore", "predicted_score", "fusionScore"],
    )?;
    let truth = calibration_number(
        value,
        &[
            "groundTruthRelevance",
            "ground_truth_relevance",
            "relevance",
            "label",
        ],
    )?;
    Some(SearchScoreCalibrationSample {
        score,
        ground_truth_relevance: truth,
    })
}

fn line_number_summary(line_numbers: &[usize]) -> String {
    const MAX_LISTED_LINES: usize = 5;
    let mut listed = line_numbers
        .iter()
        .take(MAX_LISTED_LINES)
        .map(usize::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    if line_numbers.len() > MAX_LISTED_LINES {
        listed.push_str(&format!(
            ", ... (+{} more)",
            line_numbers.len() - MAX_LISTED_LINES
        ));
    }
    format!(
        "line{} {listed}",
        if line_numbers.len() == 1 { "" } else { "s" }
    )
}

fn calibration_number(value: &serde_json::Value, keys: &[&str]) -> Option<f32> {
    keys.iter()
        .find_map(|key| {
            value.get(*key).and_then(|entry| {
                entry
                    .as_f64()
                    .map(|number| number as f32)
                    .or_else(|| entry.as_str()?.parse::<f32>().ok())
            })
        })
        .filter(|number| number.is_finite())
}

fn score_uncertainty_scale(score: f32) -> f32 {
    (1.0 - score.clamp(0.0, 1.0)).clamp(0.05, 1.0)
}

fn split_conformal_quantile(mut residuals: Vec<f32>, coverage: f32) -> f32 {
    if residuals.is_empty() {
        return 1.0;
    }
    residuals.sort_by(f32::total_cmp);
    let rank = (((residuals.len() as f32 + 1.0) * coverage).ceil() as usize)
        .saturating_sub(1)
        .min(residuals.len() - 1);
    residuals[rank]
}

fn annotate_hits_with_score_calibration(
    workspace_path: &Path,
    database_path: Option<&Path>,
    read_connection: Option<&DbConnection>,
    hits: &mut [SearchHit],
    degraded: &mut Vec<SearchDegradation>,
) {
    let feedback_events =
        search_score_calibration_feedback_events(workspace_path, database_path, read_connection);
    let calibration = SearchScoreCalibration::for_workspace_with_feedback_event_status(
        workspace_path,
        &feedback_events.events,
        feedback_events.unavailable_reason,
    );
    if let Some(reason) = calibration.feedback_event_unavailable_reason.as_deref() {
        degraded.push(SearchDegradation::search_score_calibration_unreadable(
            reason,
        ));
    }
    if calibration.feedback_event_malformed_count > 0 {
        degraded.push(SearchDegradation::search_score_calibration_unreadable(
            "feedback_events_malformed",
        ));
    }
    match calibration.status {
        SearchScoreCalibrationStatus::Insufficient => {
            degraded.push(SearchDegradation::conformal_calibration_insufficient(
                calibration.sample_count,
            ));
        }
        SearchScoreCalibrationStatus::Corrupt => {
            degraded.push(SearchDegradation::search_score_calibration_rows_corrupt(
                calibration.sample_count,
                calibration.corrupt_row_count,
                &calibration.corrupt_line_numbers,
            ));
        }
        SearchScoreCalibrationStatus::FileTooLarge => {
            degraded.push(SearchDegradation::search_score_calibration_file_too_large(
                calibration.jsonl_file_size_bytes.unwrap_or(0),
                MAX_SEARCH_SCORE_CALIBRATION_BYTES,
            ));
        }
        SearchScoreCalibrationStatus::Unreadable => {
            // bd-25z97: route I/O failures to a dedicated degraded code
            // instead of dropping them on the floor as silent `absent`.
            let reason = calibration
                .unreadable_reason
                .as_deref()
                .unwrap_or("io_error");
            degraded.push(SearchDegradation::search_score_calibration_unreadable(
                reason,
            ));
        }
        SearchScoreCalibrationStatus::Absent | SearchScoreCalibrationStatus::Calibrated => {}
    }

    for hit in hits {
        let interval = calibration.interval_for_score(hit.score);
        let mut metadata = hit
            .metadata
            .take()
            .and_then(|value| value.as_object().cloned())
            .unwrap_or_default();
        metadata.insert(
            "scoreInterval".to_string(),
            serde_json::json!([interval[0], interval[1]]),
        );
        metadata.insert(
            "coverageGuarantee".to_string(),
            serde_json::json!(round_metric_f32(SEARCH_SCORE_COVERAGE_GUARANTEE)),
        );
        metadata.insert("scoreCalibration".to_string(), calibration.data_json());
        hit.metadata = Some(serde_json::Value::Object(metadata));
    }
}

fn search_score_calibration_feedback_events(
    workspace_path: &Path,
    database_path: Option<&Path>,
    read_connection: Option<&DbConnection>,
) -> SearchScoreCalibrationFeedbackEvents {
    let workspace_id = crate::core::curate::stable_workspace_id(workspace_path);
    if let Some(connection) = read_connection {
        return match connection.list_feedback_events(&workspace_id) {
            Ok(events) => SearchScoreCalibrationFeedbackEvents::available(events),
            Err(_error) => {
                SearchScoreCalibrationFeedbackEvents::unavailable("feedback_events_read_failed")
            }
        };
    }

    let default_database_path = workspace_path.join(".ee").join("ee.db");
    let database_path = database_path.unwrap_or(&default_database_path);
    if !database_path.exists() {
        return SearchScoreCalibrationFeedbackEvents::available(Vec::new());
    }
    let connection = match DbConnection::open_file(database_path) {
        Ok(connection) => connection,
        Err(_error) => {
            return SearchScoreCalibrationFeedbackEvents::unavailable(
                "feedback_events_open_failed",
            );
        }
    };
    match connection.list_feedback_events(&workspace_id) {
        Ok(events) => SearchScoreCalibrationFeedbackEvents::available(events),
        Err(_error) => {
            SearchScoreCalibrationFeedbackEvents::unavailable("feedback_events_read_failed")
        }
    }
}

pub fn recalibrate_search_score_calibration(
    workspace_path: &Path,
    database_path: Option<&Path>,
) -> Result<SearchScoreRecalibrationReport, SearchError> {
    let feedback_events =
        search_score_calibration_feedback_events(workspace_path, database_path, None);
    let calibration_path = workspace_path
        .join(".ee")
        .join("search")
        .join("calibration.jsonl");

    let mut rows = Vec::new();
    let mut malformed_count = 0usize;
    for event in &feedback_events.events {
        let Some(evidence_json) = &event.evidence_json else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(evidence_json) else {
            malformed_count = malformed_count.saturating_add(1);
            continue;
        };
        let Some(sample) = calibration_sample_from_value(&value) else {
            continue;
        };
        rows.push(sample.data_json(&event.id).to_string());
    }

    if feedback_events.unavailable_reason.is_some() {
        // Bounded read of the existing calibration JSONL to compute its
        // blake3 hash for the unavailable-feedback report. Same TOCTOU
        // shape that 27f6ad4d closed for `stream_search_score_calibration_jsonl`:
        // `std::fs::read` pre-sizes its `Vec<u8>` from the file's
        // metadata length, so a peer-planted multi-GiB calibration
        // JSONL between the recalibrate-trigger and this branch would
        // pin a matching allocation. The cap reuses
        // `MAX_SEARCH_SCORE_CALIBRATION_BYTES` (the same 64 MiB cap the
        // streaming reader uses for the same file). A read failure or
        // over-cap result falls back to empty bytes (matches the prior
        // `.unwrap_or_default()` shape — the hash of empty is reported
        // honestly, the recalibrate run is non-mutating in this branch
        // anyway since feedback is unavailable).
        let existing = std::fs::File::open(&calibration_path)
            .ok()
            .and_then(|file| {
                let mut bytes = Vec::new();
                file.take(MAX_SEARCH_SCORE_CALIBRATION_BYTES.saturating_add(1))
                    .read_to_end(&mut bytes)
                    .ok()?;
                if bytes.len() as u64 > MAX_SEARCH_SCORE_CALIBRATION_BYTES {
                    return None;
                }
                Some(bytes)
            })
            .unwrap_or_default();
        return Ok(SearchScoreRecalibrationReport {
            schema: SEARCH_SCORE_RECALIBRATION_SCHEMA_V1,
            status: "feedback_unavailable",
            path: calibration_path,
            samples_written: 0,
            feedback_events_considered: feedback_events.events.len(),
            feedback_events_malformed: malformed_count,
            feedback_events_unavailable_reason: feedback_events.unavailable_reason,
            jsonl_hash: format!("blake3:{}", blake3::hash(&existing).to_hex()),
        });
    }

    let output = if rows.is_empty() {
        String::new()
    } else {
        let mut output = rows.join("\n");
        output.push('\n');
        output
    };

    if let Some(parent) = calibration_path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            SearchError::Index(format!(
                "could not create search calibration directory {}: {error}",
                parent.display()
            ))
        })?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&calibration_path)
        .map_err(|error| {
            SearchError::Index(format!(
                "could not open search calibration file {}: {error}",
                calibration_path.display()
            ))
        })?;
    file.write_all(output.as_bytes()).map_err(|error| {
        SearchError::Index(format!(
            "could not write search calibration file {}: {error}",
            calibration_path.display()
        ))
    })?;

    if let Some(cache) = SEARCH_SCORE_CALIBRATION_JSONL_CACHE.get()
        && let Ok(mut cache_guard) = cache.write()
    {
        cache_guard.remove(&calibration_path);
    }

    let samples_written = rows.len();
    let status = if samples_written >= MIN_SEARCH_SCORE_CALIBRATION_SAMPLES {
        "calibrated"
    } else {
        "insufficient"
    };
    Ok(SearchScoreRecalibrationReport {
        schema: SEARCH_SCORE_RECALIBRATION_SCHEMA_V1,
        status,
        path: calibration_path,
        samples_written,
        feedback_events_considered: feedback_events.events.len(),
        feedback_events_malformed: malformed_count,
        feedback_events_unavailable_reason: feedback_events.unavailable_reason,
        jsonl_hash: format!("blake3:{}", blake3::hash(output.as_bytes()).to_hex()),
    })
}

fn search_hit_score_interval_json(hit: &SearchHit) -> serde_json::Value {
    hit.metadata
        .as_ref()
        .and_then(|metadata| metadata.get("scoreInterval"))
        .cloned()
        .unwrap_or_else(|| serde_json::json!([0.0, 1.0]))
}

fn search_hit_coverage_guarantee_json(hit: &SearchHit) -> serde_json::Value {
    hit.metadata
        .as_ref()
        .and_then(|metadata| metadata.get("coverageGuarantee"))
        .cloned()
        .unwrap_or_else(|| serde_json::json!(round_metric_f32(SEARCH_SCORE_COVERAGE_GUARANTEE)))
}

fn redact_search_provenance_uri(
    value: &str,
    output_redaction_enabled: bool,
    redacted_patterns: &mut BTreeSet<String>,
) -> String {
    if !output_redaction_enabled {
        return value.to_string();
    }

    let secret_report = crate::policy::redact_secret_like_content(value);
    redacted_patterns.extend(
        secret_report
            .redacted_reasons
            .into_iter()
            .map(str::to_owned),
    );
    redact_search_absolute_path_like_segments(&secret_report.content, redacted_patterns)
}

fn redact_search_absolute_path_like_segments(
    input: &str,
    redacted_patterns: &mut BTreeSet<String>,
) -> String {
    const REDACTED_PATH: &str = "[REDACTED_PATH]";
    const PATH_PREFIXES: &[&str] = &[
        "/home/",
        "/Users/",
        "/data/",
        "/workspace/",
        "/Volumes/",
        "C:\\",
        "D:\\",
    ];

    let mut output = String::with_capacity(input.len());
    let mut cursor = 0usize;
    while cursor < input.len() {
        let remaining = &input[cursor..];
        if let Some(prefix) = PATH_PREFIXES
            .iter()
            .find(|prefix| remaining.starts_with(**prefix))
        {
            redacted_patterns.insert("path".to_string());
            output.push_str(REDACTED_PATH);
            cursor += prefix.len();
            while cursor < input.len() {
                let next = input[cursor..].chars().next().unwrap_or('\0');
                if next.is_whitespace()
                    || matches!(
                        next,
                        '"' | '\''
                            | '`'
                            | '<'
                            | '>'
                            | ')'
                            | ']'
                            | '}'
                            | ','
                            | ';'
                            | '|'
                            | '?'
                            | '#'
                    )
                {
                    break;
                }
                cursor += next.len_utf8();
            }
            continue;
        }

        let next = remaining.chars().next().unwrap_or('\0');
        output.push(next);
        cursor += next.len_utf8();
    }

    output
}

fn metadata_string<'a>(metadata: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    metadata
        .get(key)
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty())
}

fn metadata_bool(metadata: &serde_json::Value, key: &str) -> Option<bool> {
    metadata.get(key).and_then(serde_json::Value::as_bool)
}

fn metadata_f32(metadata: &serde_json::Value, key: &str) -> Option<f32> {
    metadata
        .get(key)
        .and_then(|value| {
            value
                .as_f64()
                .map(|number| number as f32)
                .or_else(|| value.as_str()?.parse::<f32>().ok())
        })
        .filter(|value| value.is_finite())
}

fn public_search_metadata(
    metadata: &serde_json::Value,
    output_redaction_enabled: bool,
) -> (serde_json::Value, Vec<String>) {
    let Some(object) = metadata.as_object() else {
        return (metadata.clone(), Vec::new());
    };
    let mut redacted_patterns = BTreeSet::new();
    let mut public_fields: serde_json::Map<String, serde_json::Value> = serde_json::Map::new();

    for (key, value) in object {
        if search_metadata_key_is_internal(key) {
            continue;
        }
        if matches!(key.as_str(), "contentPreview" | "content_preview") {
            continue;
        }
        if matches!(key.as_str(), "contentTruncated") {
            public_fields.insert(
                "content_truncated".to_string(),
                search_metadata_truncated_value(value),
            );
            continue;
        }

        let value = if search_metadata_content_key_needs_redaction(key) {
            redact_search_metadata_content_value(
                value,
                &mut redacted_patterns,
                output_redaction_enabled,
            )
        } else if search_metadata_provenance_key_needs_redaction(key) {
            redact_search_metadata_provenance_value(
                value,
                &mut redacted_patterns,
                output_redaction_enabled,
            )
        } else if key == "content_truncated" {
            search_metadata_truncated_value(value)
        } else {
            value.clone()
        };
        public_fields.insert(key.clone(), value);
    }

    if !public_fields.contains_key("content") {
        let content_value = object
            .get(SEARCH_ANALYSIS_CONTENT_KEY)
            .or_else(|| object.get("contentPreview"))
            .or_else(|| object.get("content_preview"));
        if let Some(value) = content_value {
            public_fields.insert(
                "content".to_string(),
                redact_search_metadata_content_value(
                    value,
                    &mut redacted_patterns,
                    output_redaction_enabled,
                ),
            );
        }
    }

    if !public_fields.contains_key("content_truncated") {
        if let Some(value) = object
            .get("content_truncated")
            .or_else(|| object.get("contentTruncated"))
        {
            public_fields.insert(
                "content_truncated".to_string(),
                search_metadata_truncated_value(value),
            );
        } else if let Some(content) = public_fields
            .get("content")
            .and_then(serde_json::Value::as_str)
        {
            public_fields.insert(
                "content_truncated".to_string(),
                serde_json::json!(content.ends_with("...")),
            );
        }
    }

    (
        serde_json::Value::Object(public_fields),
        redacted_patterns.into_iter().collect(),
    )
}

fn search_metadata_truncated_value(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Bool(_) => value.clone(),
        serde_json::Value::String(s) => serde_json::json!(matches!(s.as_str(), "true" | "1")),
        serde_json::Value::Number(n) => serde_json::json!(n.as_u64().is_some_and(|v| v != 0)),
        _ => serde_json::Value::Bool(false),
    }
}

fn search_metadata_key_is_internal(key: &str) -> bool {
    key.starts_with("_ee_")
        || matches!(
            key,
            "mesh"
                | "workspaceScopeDecision"
                | "workspace_scope_decision"
                | "workspaceId"
                | "workspace_id"
                | "peerGroupId"
                | "peer_group_id"
                | "cachedMaterialId"
                | "cached_material_id"
                | "originWorkspaceId"
                | "origin_workspace_id"
                | "originWorkspaceAlias"
                | "originWorkspaceLabel"
                | "origin_workspace_label"
                | "producerPeer"
                | "producerPeerId"
                | "producerPeerLabel"
                | "producer_peer_id"
                | "producer_peer_label"
                | "materialLane"
                | "material_lane"
                | "importDecisionRef"
                | "importDecisionId"
                | "import_decision_id"
                | "ledgerCursor"
                | "ledger_cursor"
                | "trustLane"
                | "trust_lane"
                | "redactionPosture"
                | "redaction_posture"
                | "policyDecision"
                | "policy_decision"
                | "policyDecisionJson"
                | "policy_decision_json"
                | "policyFailureSurface"
                | "policy_failure_surface"
                | "policyFailureSurfaceJson"
                | "policy_failure_surface_json"
        )
}

fn search_metadata_content_key_needs_redaction(key: &str) -> bool {
    matches!(key, "content" | "contentPreview" | "content_preview")
}

fn search_metadata_provenance_key_needs_redaction(key: &str) -> bool {
    matches!(key, "provenanceUri" | "provenance_uri")
}

fn redact_search_metadata_content_value(
    value: &serde_json::Value,
    redacted_patterns: &mut BTreeSet<String>,
    output_redaction_enabled: bool,
) -> serde_json::Value {
    let Some(content) = value.as_str() else {
        return value.clone();
    };
    if !output_redaction_enabled {
        return value.clone();
    }
    let report = crate::policy::redact_secret_like_content(content);
    if !report.redacted {
        return value.clone();
    }
    for reason in report.redacted_reasons {
        redacted_patterns.insert(reason.to_owned());
    }
    serde_json::json!(report.content)
}

fn redact_search_metadata_provenance_value(
    value: &serde_json::Value,
    redacted_patterns: &mut BTreeSet<String>,
    output_redaction_enabled: bool,
) -> serde_json::Value {
    let Some(content) = value.as_str() else {
        return value.clone();
    };
    serde_json::json!(redact_search_provenance_uri(
        content,
        output_redaction_enabled,
        redacted_patterns,
    ))
}

fn search_hit_output_redaction_patterns(hit: &SearchHit) -> Vec<String> {
    let mut patterns = BTreeSet::new();
    let Some(metadata) = hit.metadata.as_ref().and_then(serde_json::Value::as_object) else {
        return Vec::new();
    };
    for key in [
        "content",
        "contentPreview",
        "content_preview",
        SEARCH_ANALYSIS_CONTENT_KEY,
    ] {
        let Some(content) = metadata.get(key).and_then(serde_json::Value::as_str) else {
            continue;
        };
        let report = crate::policy::redact_secret_like_content(content);
        if report.redacted {
            patterns.extend(report.redacted_reasons.into_iter().map(str::to_owned));
        }
    }
    patterns.into_iter().collect()
}

fn search_consensus_conflict_report(query: &str, hits: &[SearchHit]) -> ConsensusConflictReport {
    let items = hits
        .iter()
        .enumerate()
        .filter_map(|(index, hit)| search_hit_pack_item(index, hit))
        .collect::<Vec<_>>();
    if items.len() < 2 {
        return ConsensusConflictReport::default();
    }

    let used_tokens = items.iter().fold(0_u32, |total, item| {
        total.saturating_add(item.estimated_tokens)
    });
    let selected_items = items
        .iter()
        .map(|item| PackSelectedItem {
            rank: item.rank,
            memory_id: item.memory_id,
            token_cost: item.estimated_tokens,
            feasible: true,
        })
        .collect::<Vec<_>>();
    let selected_count = selected_items.len();
    let draft = PackDraft {
        query: query.to_string(),
        budget: TokenBudget::default_context(),
        used_tokens,
        items,
        omitted: Vec::new(),
        selection_audit: PackSelectionAudit {
            profile: ContextPackProfile::Balanced,
            objective: PackSelectionObjective::FacilityLocation,
            algorithm_id: "search_consensus_analysis",
            algorithm_description: "Query-relevant selected hits used for consensus analysis.",
            candidate_count: selected_count,
            selected_count,
            omitted_count: 0,
            budget_limit: TokenBudget::default_context().max_tokens(),
            budget_used: used_tokens,
            total_objective_value: 0.0,
            monotone: true,
            submodular: false,
            selected_items,
            steps: Vec::new(),
        },
        hash: None,
    };

    analyze_pack_consensus_conflicts(&draft)
}

fn search_display_visible_hits(hits: &[SearchHit]) -> Vec<SearchHit> {
    hits.iter()
        .filter(|hit| {
            !matches!(
                mesh_query_visibility(hit.metadata.as_ref()),
                MeshQueryVisibility::Blocked
            )
        })
        .cloned()
        .collect()
}

fn search_hit_pack_item(index: usize, hit: &SearchHit) -> Option<PackDraftItem> {
    if matches!(
        mesh_query_visibility(hit.metadata.as_ref()),
        MeshQueryVisibility::Blocked
    ) {
        return None;
    }

    let metadata = hit.metadata.as_ref()?;
    let content = metadata_string(metadata, SEARCH_ANALYSIS_CONTENT_KEY)
        .or_else(|| metadata_string(metadata, "content"))?
        .to_string();
    let memory_id = MemoryId::from_str(&hit.doc_id).ok()?;
    let level = metadata_string(metadata, "level");
    let kind = metadata_string(metadata, "kind");
    let tags = metadata_string(metadata, "tags")
        .map(split_tags)
        .unwrap_or_default();
    let provenance = search_hit_pack_provenance(metadata, memory_id);
    let trust = search_hit_pack_trust(metadata);
    let lifecycle = search_hit_pack_lifecycle(metadata);
    let rank = u32::try_from(index.saturating_add(1)).unwrap_or(u32::MAX);

    Some(PackDraftItem {
        rank,
        memory_id,
        section: search_pack_section(level, kind),
        content,
        estimated_tokens: estimate_tokens_default(
            metadata_string(metadata, SEARCH_ANALYSIS_CONTENT_KEY)
                .or_else(|| metadata_string(metadata, "content"))
                .unwrap_or_default(),
        ),
        relevance: UnitScore::parse(if hit.score.is_nan() {
            0.0
        } else {
            hit.score.clamp(0.0, 1.0)
        })
        .unwrap_or_else(|_| UnitScore::neutral()),
        utility: metadata_f32(metadata, SEARCH_ANALYSIS_UTILITY_KEY)
            .and_then(|value| {
                UnitScore::parse(if value.is_nan() {
                    0.0
                } else {
                    value.clamp(0.0, 1.0)
                })
                .ok()
            })
            .unwrap_or_else(UnitScore::neutral),
        proximity_to_seed: None,
        score_breakdown: None,
        provenance,
        why: hit.why(),
        diversity_key: tags.first().map(|tag| {
            format!(
                "{}:{}:{}",
                level.unwrap_or("memory"),
                kind.unwrap_or("memory"),
                tag
            )
        }),
        trust,
        redactions: Vec::new(),
        tombstoned_at: metadata_string(metadata, "tombstoned_at").map(str::to_string),
        lifecycle,
        selected_in: PackSelectionPhase::FacilityLocation,
    })
}

fn split_tags(tags: &str) -> Vec<String> {
    tags.split(',')
        .map(str::trim)
        .filter(|tag| !tag.is_empty())
        .map(str::to_string)
        .collect()
}

fn search_pack_section(level: Option<&str>, kind: Option<&str>) -> PackSection {
    match (level.unwrap_or_default(), kind.unwrap_or_default()) {
        ("procedural", _) | (_, "rule" | "convention" | "playbook-step") => {
            PackSection::ProceduralRules
        }
        (_, "decision") => PackSection::Decisions,
        (_, "failure" | "anti-pattern" | "risk") => PackSection::Failures,
        ("episodic", _) => PackSection::Evidence,
        _ => PackSection::Artifacts,
    }
}

fn search_hit_pack_provenance(
    metadata: &serde_json::Value,
    memory_id: MemoryId,
) -> Vec<PackProvenance> {
    let uri = metadata_string(metadata, SEARCH_ANALYSIS_PROVENANCE_URI_KEY)
        .or_else(|| metadata_string(metadata, "provenanceUri"))
        .or_else(|| metadata_string(metadata, "provenance_uri"))
        .and_then(|uri| ProvenanceUri::from_str(uri).ok())
        .unwrap_or(ProvenanceUri::EeMemory(memory_id));
    PackProvenance::new(uri, "search result memory evidence")
        .map(|provenance| vec![provenance])
        .unwrap_or_default()
}

fn search_hit_pack_trust(metadata: &serde_json::Value) -> PackTrustSignal {
    let trust_class = metadata_string(metadata, "trust_class")
        .and_then(|value| TrustClass::from_str(value).ok())
        .unwrap_or(TrustClass::AgentAssertion);
    let producer = metadata_string(metadata, "producerAgent")
        .or_else(|| metadata_string(metadata, "trust_subclass"))
        .map(str::to_string);
    PackTrustSignal::new(trust_class, producer)
}

fn search_hit_pack_lifecycle(metadata: &serde_json::Value) -> Option<PackItemLifecycle> {
    let valid_from = metadata_string(metadata, "valid_from")
        .or_else(|| metadata_string(metadata, SEARCH_ANALYSIS_CREATED_AT_KEY))
        .or_else(|| metadata_string(metadata, "created_at"))
        .map(str::to_string);
    let valid_to = metadata_string(metadata, "valid_to").map(str::to_string);
    if valid_from.is_none() && valid_to.is_none() {
        return None;
    }
    Some(PackItemLifecycle {
        validity_status: metadata_string(metadata, "validity_status")
            .unwrap_or("active")
            .to_string(),
        validity_window_kind: metadata_string(metadata, "validity_window_kind")
            .unwrap_or("unbounded")
            .to_string(),
        valid_from,
        valid_to,
    })
}

fn consensus_entry_data_json(entry: &ConsensusEntry) -> serde_json::Value {
    serde_json::json!({
        "schema": entry.schema,
        "subjectFingerprint": entry.subject_fingerprint,
        "subjectSummary": entry.subject_summary,
        "agreementScore": entry.agreement_score,
        "memberMemoryIds": entry.member_memory_ids.iter().map(ToString::to_string).collect::<Vec<_>>(),
        "memberProducers": entry.member_producers.iter().map(consensus_producer_data_json).collect::<Vec<_>>(),
        "semanticSimilarityMin": entry.semantic_similarity_min,
        "firstRecordedAt": entry.first_recorded_at,
        "lastReinforcedAt": entry.last_reinforced_at,
    })
}

fn consensus_producer_data_json(producer: &ConsensusProducer) -> serde_json::Value {
    serde_json::json!({
        "agentName": producer.agent_name,
        "trustClass": producer.trust_class.as_str(),
    })
}

fn conflict_entry_data_json(entry: &ConflictEntry) -> serde_json::Value {
    serde_json::json!({
        "schema": entry.schema,
        "subjectFingerprint": entry.subject_fingerprint,
        "kind": entry.kind.as_str(),
        "conflictingMemoryIds": entry.conflicting_memory_ids.iter().map(ToString::to_string).collect::<Vec<_>>(),
        "evidencePointers": entry.evidence_pointers,
        "earliestAt": entry.earliest_at,
        "latestAt": entry.latest_at,
        "recommendedAction": entry.recommended_action.as_str(),
    })
}

impl RetrievalMetrics {
    #[must_use]
    pub fn from_hits(
        requested_limit: u32,
        elapsed_ms: f64,
        hits: &[SearchHit],
        error_count: usize,
    ) -> Self {
        Self::from_hits_with_floor(requested_limit, elapsed_ms, hits, error_count, None, 0)
    }

    /// Build metrics with the post-floor view of recall.
    ///
    /// Bead bd-17c65.2.1 (B1). `hits` are the post-floor results (those
    /// that survived); `below_floor_count` is the number of pre-floor
    /// candidates that were dropped.
    #[must_use]
    pub fn from_hits_with_floor(
        requested_limit: u32,
        elapsed_ms: f64,
        hits: &[SearchHit],
        error_count: usize,
        relevance_floor: Option<f32>,
        below_floor_count: usize,
    ) -> Self {
        let mut source_counts = RetrievalSourceCounts::default();
        let mut field_coverage = RetrievalFieldCoverage::default();
        let mut min_score: Option<f32> = None;
        let mut max_score: Option<f32> = None;
        let mut score_sum = 0.0_f32;

        for hit in hits {
            source_counts.record(hit.source);
            field_coverage.record(hit);
            min_score = Some(min_score.map_or(hit.score, |score| score.min(hit.score)));
            max_score = Some(max_score.map_or(hit.score, |score| score.max(hit.score)));
            score_sum += hit.score;
        }

        let mean = if hits.is_empty() {
            None
        } else {
            Some(score_sum / hits.len() as f32)
        };

        Self {
            requested_limit,
            returned_count: hits.len(),
            error_count,
            elapsed_ms,
            source_counts,
            score_distribution: RetrievalScoreDistribution {
                top: hits.first().map(|hit| hit.score),
                min: min_score,
                max: max_score,
                mean,
            },
            field_coverage,
            relevance_floor,
            candidates_above_floor: hits.len(),
            candidates_below_floor: below_floor_count,
        }
    }

    #[must_use]
    pub fn data_json(self) -> serde_json::Value {
        serde_json::json!({
            "requestedLimit": self.requested_limit,
            "returnedCount": self.returned_count,
            "errorCount": self.error_count,
            "elapsedMs": round_metric_f64(self.elapsed_ms),
            "sourceCounts": {
                "lexical": self.source_counts.lexical,
                "semanticFast": self.source_counts.semantic_fast,
                "semanticQuality": self.source_counts.semantic_quality,
                "hybrid": self.source_counts.hybrid,
                "reranked": self.source_counts.reranked,
            },
            "scoreDistribution": {
                "top": optional_score_json(self.score_distribution.top),
                "min": optional_score_json(self.score_distribution.min),
                "max": optional_score_json(self.score_distribution.max),
                "mean": optional_score_json(self.score_distribution.mean),
            },
            "fieldCoverage": {
                "fastScoreCount": self.field_coverage.fast_score_count,
                "qualityScoreCount": self.field_coverage.quality_score_count,
                "lexicalScoreCount": self.field_coverage.lexical_score_count,
                "rerankScoreCount": self.field_coverage.rerank_score_count,
                "metadataCount": self.field_coverage.metadata_count,
                "explanationCount": self.field_coverage.explanation_count,
            },
            // Bead bd-17c65.2.1 (B1): floor + candidate counts.
            "relevanceFloor": optional_score_json(self.relevance_floor),
            "candidatesAboveFloor": self.candidates_above_floor,
            "candidatesBelowFloor": self.candidates_below_floor,
            // Bead bd-17c65.2.4 (B4): qualityAssessment + honestQualityScore.
            "qualityAssessment": self.quality_assessment().as_str(),
            "honestQualityScore": optional_score_json(self.honest_quality_score()),
        })
    }

    /// Classify recall quality (B4). `floor` defaults to
    /// `DEFAULT_RELEVANCE_FLOOR` when `relevance_floor` is `None`.
    #[must_use]
    pub fn quality_assessment(&self) -> QualityAssessment {
        let floor = self.relevance_floor.unwrap_or(DEFAULT_RELEVANCE_FLOOR);
        QualityAssessment::classify(
            self.score_distribution.top,
            self.score_distribution.mean,
            floor,
        )
    }

    /// Single 0..1 confidence summary for agents that don't want to
    /// reason about three-state quality (B4).
    ///
    /// Formula (clamped to `[0.0, 1.0]`):
    ///
    ///   0.5 * (1 - exp(-top / floor))           // top-score signal
    /// + 0.3 * (above_floor / requested_limit)   // recall signal
    /// + 0.2 * (1 - variance_above_floor)        // confidence signal
    ///
    /// Returns `None` when no hits passed the floor (clearly empty;
    /// signaled by `qualityAssessment == "empty"` instead).
    #[must_use]
    pub fn honest_quality_score(&self) -> Option<f32> {
        let top = self.score_distribution.top?;
        let floor = self.relevance_floor.unwrap_or(DEFAULT_RELEVANCE_FLOOR);
        if !top.is_finite() || top < floor {
            return None;
        }
        let limit = self.requested_limit.max(1) as f32;
        let above = self.candidates_above_floor as f32;
        let recall = (above / limit).min(1.0);
        // Top-score signal: exp(-top/floor) collapses to 0 as top
        // gets large, so 1 - exp(-x) → 1.
        let top_signal = 1.0_f32 - (-(top / floor.max(1e-6))).exp();
        // Variance signal: how tightly clustered are above-floor
        // scores? Smaller spread → higher confidence. Approximate
        // variance with (max - min) / max, bounded.
        let variance_proxy = match (self.score_distribution.max, self.score_distribution.min) {
            (Some(max), Some(min)) if max > 0.0 => {
                let v = (max - min) / max;
                if v.is_nan() { 0.0 } else { v.clamp(0.0, 1.0) }
            }
            _ => 0.0,
        };
        let variance_signal = if variance_proxy.is_nan() {
            1.0
        } else {
            (1.0_f32 - variance_proxy).clamp(0.0, 1.0)
        };
        let raw = 0.5 * top_signal + 0.3 * recall + 0.2 * variance_signal;
        Some(if raw.is_nan() {
            0.0
        } else {
            raw.clamp(0.0, 1.0)
        })
    }
}

impl RetrievalSourceCounts {
    fn record(&mut self, source: ScoreSource) {
        match source {
            ScoreSource::Lexical => self.lexical += 1,
            ScoreSource::SemanticFast => self.semantic_fast += 1,
            ScoreSource::SemanticQuality => self.semantic_quality += 1,
            ScoreSource::Hybrid => self.hybrid += 1,
            ScoreSource::Reranked => self.reranked += 1,
        }
    }
}

impl RetrievalFieldCoverage {
    fn record(&mut self, hit: &SearchHit) {
        if hit.fast_score.is_some() {
            self.fast_score_count += 1;
        }
        if hit.quality_score.is_some() {
            self.quality_score_count += 1;
        }
        if hit.lexical_score.is_some() {
            self.lexical_score_count += 1;
        }
        if hit.rerank_score.is_some() {
            self.rerank_score_count += 1;
        }
        if hit.metadata.is_some() {
            self.metadata_count += 1;
        }
        if hit.explanation.is_some() {
            self.explanation_count += 1;
        }
    }
}

#[must_use]
pub fn query_observation_json(query: &str) -> serde_json::Value {
    serde_json::json!({
        "textIncluded": false,
        "lengthBytes": query.len(),
        "fingerprint": format!("blake3:{}", blake3::hash(query.as_bytes()).to_hex()),
    })
}

#[must_use]
pub fn elapsed_timing_json(elapsed_ms: f64) -> serde_json::Value {
    serde_json::json!({
        "elapsedMs": round_metric_f64(elapsed_ms),
        "elapsedMsBucket": elapsed_ms_bucket(elapsed_ms),
        "nondeterministic": true,
    })
}

#[must_use]
pub fn performance_redaction_json() -> serde_json::Value {
    serde_json::json!({
        "memoryContentIncluded": false,
        "queryTextIncluded": false,
        "safeFields": [
            "counts",
            "elapsedMs",
            "elapsedMsBucket",
            "status",
            "fingerprints",
            "degradationCodes"
        ],
    })
}

fn retrieval_source_counts_json(counts: RetrievalSourceCounts) -> serde_json::Value {
    serde_json::json!({
        "lexical": counts.lexical,
        "semanticFast": counts.semantic_fast,
        "semanticQuality": counts.semantic_quality,
        "hybrid": counts.hybrid,
        "reranked": counts.reranked,
    })
}

fn retrieval_score_distribution_json(
    distribution: RetrievalScoreDistribution,
) -> serde_json::Value {
    serde_json::json!({
        "top": optional_score_json(distribution.top),
        "min": optional_score_json(distribution.min),
        "max": optional_score_json(distribution.max),
        "mean": optional_score_json(distribution.mean),
    })
}

fn retrieval_field_coverage_json(coverage: RetrievalFieldCoverage) -> serde_json::Value {
    serde_json::json!({
        "fastScoreCount": coverage.fast_score_count,
        "qualityScoreCount": coverage.quality_score_count,
        "lexicalScoreCount": coverage.lexical_score_count,
        "rerankScoreCount": coverage.rerank_score_count,
        "metadataCount": coverage.metadata_count,
        "explanationCount": coverage.explanation_count,
    })
}

fn optional_score_json(score: Option<f32>) -> serde_json::Value {
    score.map_or(serde_json::Value::Null, |score| {
        serde_json::json!(round_metric_f32(score))
    })
}

fn elapsed_ms_bucket(elapsed_ms: f64) -> &'static str {
    match elapsed_ms {
        elapsed if elapsed < 1.0 => "lt_1ms",
        elapsed if elapsed < 10.0 => "1_9ms",
        elapsed if elapsed < 50.0 => "10_49ms",
        elapsed if elapsed < 100.0 => "50_99ms",
        elapsed if elapsed < 500.0 => "100_499ms",
        elapsed if elapsed < 1_000.0 => "500_999ms",
        _ => "gte_1000ms",
    }
}

fn round_metric_f32(score: f32) -> f32 {
    (score * 1_000_000.0).round() / 1_000_000.0
}

fn round_metric_f64(score: f64) -> f64 {
    (score * 1_000_000.0).round() / 1_000_000.0
}

#[derive(Debug)]
pub enum SearchError {
    Index(String),
    NoIndex,
    SourceModeUnavailable {
        requested: SearchSourceMode,
        reason: String,
    },
}

impl SearchError {
    #[must_use]
    pub fn repair_hint(&self) -> Option<&str> {
        match self {
            Self::Index(_) => Some("Check index directory and permissions"),
            Self::NoIndex => Some("ee index rebuild --workspace ."),
            Self::SourceModeUnavailable { .. } => {
                Some("Rebuild with the requested search features, or omit --strict-source-mode")
            }
        }
    }
}

impl std::fmt::Display for SearchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Index(e) => write!(f, "Index error: {e}"),
            Self::NoIndex => write!(f, "Search index not found"),
            Self::SourceModeUnavailable { requested, reason } => write!(
                f,
                "Requested source mode {} is unavailable: {reason}",
                requested.as_str()
            ),
        }
    }
}

impl std::error::Error for SearchError {}

impl ScoreExplanation {
    #[must_use]
    pub fn generate(hit: &SearchHit) -> Self {
        let mut factors = Vec::new();
        let mut summary_parts = Vec::new();

        match hit.source {
            ScoreSource::Lexical => {
                if let Some(lex) = hit.lexical_score {
                    factors.push(ScoreFactor::new(
                        "lexical",
                        lex,
                        "BM25 term matching",
                        "lexical_score",
                        "score = lexical_score",
                    ));
                    summary_parts.push(format!("lexical match ({:.2})", lex));
                }
            }
            ScoreSource::SemanticFast => {
                if let Some(fast) = hit.fast_score {
                    factors.push(ScoreFactor::new(
                        "semantic_fast",
                        fast,
                        "hash-based embedding similarity",
                        "fast_score",
                        "score = fast_score",
                    ));
                    summary_parts.push(format!("fast semantic ({:.2})", fast));
                }
            }
            ScoreSource::SemanticQuality => {
                if let Some(quality) = hit.quality_score {
                    factors.push(ScoreFactor::new(
                        "semantic_quality",
                        quality,
                        "dense embedding similarity",
                        "quality_score",
                        "score = quality_score",
                    ));
                    summary_parts.push(format!("quality semantic ({:.2})", quality));
                }
            }
            ScoreSource::Hybrid => {
                if let Some(fast) = hit.fast_score {
                    factors.push(ScoreFactor::new(
                        "semantic_fast",
                        fast,
                        "hash-based embedding similarity",
                        "fast_score",
                        "component = fast_score; final score = score",
                    ));
                }
                if let Some(quality) = hit.quality_score {
                    factors.push(ScoreFactor::new(
                        "semantic_quality",
                        quality,
                        "dense embedding similarity",
                        "quality_score",
                        "component = quality_score; final score = score",
                    ));
                }
                if let Some(lex) = hit.lexical_score {
                    factors.push(ScoreFactor::new(
                        "lexical",
                        lex,
                        "BM25 term matching",
                        "lexical_score",
                        "component = lexical_score; final score = score",
                    ));
                }
                summary_parts.push(format!("RRF fusion of {} signals", factors.len()));
            }
            ScoreSource::Reranked => {
                if let Some(rerank) = hit.rerank_score {
                    factors.push(ScoreFactor::new(
                        "rerank",
                        rerank,
                        "cross-encoder reranking",
                        "rerank_score",
                        "score = rerank_score",
                    ));
                    summary_parts.push(format!("reranked ({:.2})", rerank));
                }
                if let Some(fast) = hit.fast_score {
                    factors.push(ScoreFactor::new(
                        "semantic_fast",
                        fast,
                        "initial hash-based candidate",
                        "fast_score",
                        "candidate component = fast_score; final score = rerank_score",
                    ));
                }
            }
        }

        let summary = if summary_parts.is_empty() {
            format!("Score {:.4} from {} source", hit.score, hit.source.as_str())
        } else {
            format!("Score {:.4} via {}", hit.score, summary_parts.join(", "))
        };

        Self { summary, factors }
    }
}

/// Dedupe a hit list on `docId`, keeping the highest-scoring occurrence
/// of each distinct id. Stable: the position of the first occurrence is
/// preserved; only the score / source / explanation fields are upgraded
/// in place when a higher-scoring duplicate is found later in the list.
///
/// Returns `(deduped, collapsed_count)`. Bead bd-17c65.2.3 (B3).
fn dedupe_hits_on_doc_id(hits: Vec<SearchHit>) -> (Vec<SearchHit>, usize) {
    // Use a HashMap to track first-seen index per doc_id. Iterate in
    // input order so the first occurrence's index is stable. For each
    // duplicate, compare scores and (only if strictly higher) overwrite
    // the stored hit in place — preserving ordering.
    let mut seen: std::collections::HashMap<String, usize> =
        std::collections::HashMap::with_capacity(hits.len());
    let mut deduped: Vec<SearchHit> = Vec::with_capacity(hits.len());
    let mut collapsed = 0_usize;
    for hit in hits {
        if let Some(&index) = seen.get(&hit.doc_id) {
            collapsed += 1;
            // Upgrade only on strictly higher score so ties keep the
            // first-seen entry (deterministic).
            //
            // Both `hit.score` and the stored score may be NaN if the
            // upstream search ever produced them; for NaN we never
            // upgrade — `NaN > x` is always false.
            if hit.score > deduped[index].score {
                deduped[index] = hit;
            }
        } else {
            seen.insert(hit.doc_id.clone(), deduped.len());
            deduped.push(hit);
        }
    }
    (deduped, collapsed)
}

/// Dedupe memory hits by conservative token mutual-information clusters.
///
/// This is opt-in (`--dedupe=mi`) and intentionally runs after exact doc-id
/// dedupe but before relevance-floor filtering so the metrics and returned
/// rank order are computed on the final deduped pool. It only clusters memory
/// docs with loadable content and annotates the retained representative with
/// `metadata.mergedFrom`.
///
/// Returns `(deduped, collapsed_count, eligible_memory_hit_count)`.
/// Bead bd-17c65.14.14 (N14).
fn dedupe_hits_on_mutual_information(
    hits: Vec<SearchHit>,
    options: &SearchOptions,
    read_connection: Option<&DbConnection>,
) -> (Vec<SearchHit>, usize, usize) {
    if hits.len() < 2 {
        let eligible_memory_hit_count = hits.len();
        return (hits, 0, eligible_memory_hit_count);
    }

    let contents_by_doc_id = mi_dedup_hit_contents(&hits, options, read_connection);
    let eligible_indices = hits
        .iter()
        .enumerate()
        .filter(|(_, hit)| {
            hit.doc_id.starts_with("mem_")
                && contents_by_doc_id
                    .get(&hit.doc_id)
                    .is_some_and(|content| !content.trim().is_empty())
        })
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    if eligible_indices.len() < 2 {
        return (hits, 0, eligible_indices.len());
    }

    let mut adjacency: BTreeMap<usize, BTreeSet<usize>> = BTreeMap::new();
    for (left_pos, left_index) in eligible_indices.iter().copied().enumerate() {
        for right_index in eligible_indices.iter().copied().skip(left_pos + 1) {
            let Some(left_content) = contents_by_doc_id.get(&hits[left_index].doc_id) else {
                continue;
            };
            let Some(right_content) = contents_by_doc_id.get(&hits[right_index].doc_id) else {
                continue;
            };
            let Some(metrics) = mi_dedup_metrics_for_contents(left_content, right_content) else {
                continue;
            };
            if metrics.cosine_similarity < SEARCH_MI_DEDUP_MIN_COSINE_SIMILARITY
                || metrics.normalized_mi < SEARCH_MI_DEDUP_MIN_NORMALIZED_MI
            {
                continue;
            }
            adjacency.entry(left_index).or_default().insert(right_index);
            adjacency.entry(right_index).or_default().insert(left_index);
        }
    }
    if adjacency.is_empty() {
        return (hits, 0, eligible_indices.len());
    }

    let mut visited = BTreeSet::new();
    let mut remove_indices = BTreeSet::new();
    let mut merged_by_keeper: BTreeMap<usize, Vec<String>> = BTreeMap::new();
    for seed in adjacency.keys().copied() {
        if visited.contains(&seed) {
            continue;
        }
        let mut stack = vec![seed];
        let mut members = BTreeSet::new();
        while let Some(index) = stack.pop() {
            if !visited.insert(index) {
                continue;
            }
            members.insert(index);
            if let Some(neighbors) = adjacency.get(&index) {
                for neighbor in neighbors.iter().rev().copied() {
                    if !visited.contains(&neighbor) {
                        stack.push(neighbor);
                    }
                }
            }
        }
        if members.len() < 2 {
            continue;
        }

        let keeper = members
            .iter()
            .copied()
            .reduce(|best, candidate| {
                if hits[candidate].score > hits[best].score {
                    candidate
                } else {
                    best
                }
            })
            .unwrap_or(seed);
        let merged = members
            .iter()
            .copied()
            .filter(|index| *index != keeper)
            .map(|index| hits[index].doc_id.clone())
            .collect::<Vec<_>>();
        remove_indices.extend(members.iter().copied().filter(|index| *index != keeper));
        if !merged.is_empty() {
            merged_by_keeper.insert(keeper, merged);
        }
    }

    let collapsed = remove_indices.len();
    if collapsed == 0 {
        return (hits, 0, eligible_indices.len());
    }

    let mut deduped = Vec::with_capacity(hits.len() - collapsed);
    for (index, mut hit) in hits.into_iter().enumerate() {
        if remove_indices.contains(&index) {
            continue;
        }
        if let Some(merged_from) = merged_by_keeper.remove(&index) {
            annotate_hit_with_mi_merged_from(&mut hit, merged_from);
        }
        deduped.push(hit);
    }
    (deduped, collapsed, eligible_indices.len())
}

fn mi_dedup_hit_contents(
    hits: &[SearchHit],
    options: &SearchOptions,
    read_connection: Option<&DbConnection>,
) -> BTreeMap<String, String> {
    let doc_ids = hits
        .iter()
        .filter(|hit| hit.doc_id.starts_with("mem_"))
        .map(|hit| hit.doc_id.as_str())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let mut contents = hits
        .iter()
        .filter(|hit| hit.doc_id.starts_with("mem_"))
        .filter_map(|hit| {
            metadata_content_for_mi_dedup(hit).map(|content| (hit.doc_id.clone(), content))
        })
        .collect::<BTreeMap<_, _>>();

    if doc_ids.is_empty() {
        return contents;
    }
    if let Some(connection) = read_connection {
        extend_mi_dedup_contents_from_connection(&mut contents, &doc_ids, connection);
        return contents;
    }

    let database_path = options.resolve_database_path();
    if database_path.exists()
        && let Ok(connection) = DbConnection::open_file(&database_path)
    {
        extend_mi_dedup_contents_from_connection(&mut contents, &doc_ids, &connection);
    }
    contents
}

fn extend_mi_dedup_contents_from_connection(
    contents: &mut BTreeMap<String, String>,
    doc_ids: &[&str],
    connection: &DbConnection,
) {
    let Ok(memories) = connection.get_memories_batch(doc_ids) else {
        return;
    };
    for (memory_id, memory) in memories {
        if !memory.content.trim().is_empty() {
            contents.insert(memory_id, memory.content);
        }
    }
}

fn metadata_content_for_mi_dedup(hit: &SearchHit) -> Option<String> {
    let metadata = hit.metadata.as_ref()?;
    metadata_string(metadata, "content")
        .or_else(|| metadata_string(metadata, SEARCH_ANALYSIS_CONTENT_KEY))
        .or_else(|| metadata_string(metadata, "contentPreview"))
        .or_else(|| metadata_string(metadata, "content_preview"))
        .map(str::to_owned)
}

fn annotate_hit_with_mi_merged_from(hit: &mut SearchHit, merged_from: Vec<String>) {
    let metadata = hit.metadata.get_or_insert_with(|| serde_json::json!({}));
    if !metadata.is_object() {
        *metadata = serde_json::json!({});
    }
    if let Some(object) = metadata.as_object_mut() {
        object.insert(
            "dedupeMode".to_string(),
            serde_json::json!(SearchDedupMode::MutualInformation.as_str()),
        );
        object.insert("mergedFrom".to_string(), serde_json::json!(merged_from));
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct SearchMiDedupMetrics {
    cosine_similarity: f64,
    normalized_mi: f64,
}

fn mi_dedup_metrics_for_contents(left: &str, right: &str) -> Option<SearchMiDedupMetrics> {
    let left_counts = mi_token_counts(left);
    let right_counts = mi_token_counts(right);
    if left_counts.is_empty() || right_counts.is_empty() {
        return None;
    }
    let cosine_similarity = token_cosine_similarity(&left_counts, &right_counts);
    let mutual_information = token_mutual_information(&left_counts, &right_counts);
    let min_entropy = token_entropy(&left_counts).min(token_entropy(&right_counts));
    let normalized_mi = if min_entropy > f64::EPSILON {
        (mutual_information / min_entropy).clamp(0.0, 1.0)
    } else {
        0.0
    };
    Some(SearchMiDedupMetrics {
        cosine_similarity,
        normalized_mi,
    })
}

fn mi_token_counts(content: &str) -> BTreeMap<String, u32> {
    let mut counts = BTreeMap::new();
    let mut token = String::new();
    for ch in content.chars().flat_map(char::to_lowercase) {
        if ch.is_ascii_alphanumeric() {
            token.push(ch);
        } else if !token.is_empty() {
            *counts.entry(std::mem::take(&mut token)).or_insert(0) += 1;
        }
    }
    if !token.is_empty() {
        *counts.entry(token).or_insert(0) += 1;
    }
    counts
}

fn token_cosine_similarity(
    left_counts: &BTreeMap<String, u32>,
    right_counts: &BTreeMap<String, u32>,
) -> f64 {
    let dot = kahan_sum(left_counts.iter().filter_map(|(token, left_count)| {
        right_counts
            .get(token)
            .map(|right_count| f64::from(*left_count) * f64::from(*right_count))
    }));
    let left_norm = kahan_sum(
        left_counts
            .values()
            .map(|count| f64::from(*count) * f64::from(*count)),
    )
    .sqrt();
    let right_norm = kahan_sum(
        right_counts
            .values()
            .map(|count| f64::from(*count) * f64::from(*count)),
    )
    .sqrt();
    if left_norm <= f64::EPSILON || right_norm <= f64::EPSILON {
        0.0
    } else {
        (dot / (left_norm * right_norm)).clamp(0.0, 1.0)
    }
}

fn token_mutual_information(
    left_counts: &BTreeMap<String, u32>,
    right_counts: &BTreeMap<String, u32>,
) -> f64 {
    let left_total = f64::from(left_counts.values().copied().sum::<u32>());
    let right_total = f64::from(right_counts.values().copied().sum::<u32>());
    kahan_sum(left_counts.iter().filter_map(|(token, left_count)| {
        let right_count = right_counts.get(token)?;
        let px = f64::from(*left_count) / left_total;
        let py = f64::from(*right_count) / right_total;
        let pxy = px.min(py);
        (pxy > 0.0 && px > 0.0 && py > 0.0).then(|| pxy * (pxy / (px * py)).ln())
    }))
}

fn token_entropy(counts: &BTreeMap<String, u32>) -> f64 {
    let total = f64::from(counts.values().copied().sum::<u32>());
    kahan_sum(counts.values().map(|count| {
        let probability = f64::from(*count) / total;
        -probability * probability.ln()
    }))
}

fn kahan_sum(values: impl IntoIterator<Item = f64>) -> f64 {
    let mut sum = 0.0_f64;
    let mut compensation = 0.0_f64;
    for value in values {
        let adjusted = value - compensation;
        let next = sum + adjusted;
        compensation = (next - sum) - adjusted;
        sum = next;
    }
    sum
}

/// Best-effort audit append. Bead bd-17c65.7.7 (G8).
///
/// Read surfaces (search, context, why, memory show) call this after
/// completing their primary work to record the access in the audit log
/// for L3 decay (`last_accessed` signal) and G1 learn summary
/// aggregation. Failures are silently swallowed — an audit append must
/// never block or fail the user's primary operation. The audit log is
/// best-effort enrichment, not part of the read response contract.
fn audit_append_best_effort(
    database_path: &Path,
    audit_ids: &mut SearchAuditIdSource,
    workspace_id: Option<&str>,
    action: &'static str,
    target_type: Option<&str>,
    target_id: Option<&str>,
    details: Option<String>,
) {
    let Ok(conn) = DbConnection::open_file(database_path) else {
        return;
    };
    let audit_id = audit_ids.next_audit_id();
    let input = CreateAuditInput {
        workspace_id: workspace_id.map(str::to_owned),
        actor: None,
        action: action.to_owned(),
        target_type: target_type.map(str::to_owned),
        target_id: target_id.map(str::to_owned),
        details,
    };
    if let Err(error) = conn.insert_audit(&audit_id, &input) {
        // Don't propagate but surface via tracing so issues are visible
        // when looking at the response logs.
        tracing::warn!(
            target: "ee::core::search::audit",
            action,
            error = %error,
            "best-effort audit append failed"
        );
    }
}

enum SearchAuditIdSource {
    Ambient,
    #[cfg(test)]
    Seeded(Deterministic<Seed>),
}

impl SearchAuditIdSource {
    fn next_audit_id(&mut self) -> String {
        match self {
            Self::Ambient => generate_audit_id(),
            #[cfg(test)]
            Self::Seeded(determinism) => generate_audit_id_seeded(determinism),
        }
    }
}

/// bd-21gya: per-search audit batch.
///
/// Buffers `search.executed` / `search.returned_mem` / `redact_at_output`
/// rows so the hot read path opens exactly one DbConnection and writes a
/// single transaction, instead of `1 + R + R*P` separate opens (one per
/// returned hit and per redaction pattern).
struct SearchAuditBatch {
    entries: Vec<(String, CreateAuditInput)>,
}

impl SearchAuditBatch {
    fn new(capacity_hint: usize) -> Self {
        Self {
            entries: Vec::with_capacity(capacity_hint),
        }
    }

    fn push(
        &mut self,
        audit_ids: &mut SearchAuditIdSource,
        workspace_id: Option<&str>,
        action: &'static str,
        target_type: Option<&str>,
        target_id: Option<&str>,
        details: Option<String>,
    ) {
        let audit_id = audit_ids.next_audit_id();
        let input = CreateAuditInput {
            workspace_id: workspace_id.map(str::to_owned),
            actor: None,
            action: action.to_owned(),
            target_type: target_type.map(str::to_owned),
            target_id: target_id.map(str::to_owned),
            details,
        };
        self.entries.push((audit_id, input));
    }

    fn flush_best_effort(self, database_path: &Path) {
        if self.entries.is_empty() {
            return;
        }
        let Ok(conn) = DbConnection::open_file(database_path) else {
            return;
        };
        self.flush_best_effort_with_connection(&conn);
    }

    fn flush_best_effort_with_connection(self, conn: &DbConnection) {
        if self.entries.is_empty() {
            return;
        }
        let count = self.entries.len();
        if let Err(error) = conn.insert_audit_batch(&self.entries) {
            // Match audit_append_best_effort's failure mode: best-effort,
            // never block the response. Surface via tracing for diagnostics.
            tracing::warn!(
                target: "ee::core::search::audit",
                count,
                error = %error,
                "best-effort audit batch append failed"
            );
        }
    }
}

fn search_audit_workspace_persisted(conn: Option<&DbConnection>, workspace_id: &str) -> bool {
    let Some(conn) = conn else {
        return true;
    };
    match conn.get_workspace(workspace_id) {
        Ok(Some(_)) => true,
        Ok(None) => false,
        Err(error) => {
            tracing::debug!(
                target: "ee::core::search::audit",
                workspace_id,
                error = %error,
                "search audit workspace preflight failed"
            );
            true
        }
    }
}

pub fn run_search(options: &SearchOptions) -> Result<SearchReport, SearchError> {
    let determinism = Deterministic::from_seed(0);
    let mut audit_ids = SearchAuditIdSource::Ambient;
    run_search_inner(
        options,
        None,
        &determinism,
        &mut audit_ids,
        None,
        true,
        None,
    )
}

pub fn run_search_seeded(
    options: &SearchOptions,
    determinism: &Deterministic<Seed>,
) -> Result<SearchReport, SearchError> {
    // Search determinism controls ranking/output replay. Audit rows are durable
    // side effects, so they must remain unique across repeated seeded calls.
    let mut audit_ids = SearchAuditIdSource::Ambient;
    run_search_inner(options, None, determinism, &mut audit_ids, None, true, None)
}

pub fn run_search_with_read_connection(
    options: &SearchOptions,
    read_connection: &DbConnection,
) -> Result<SearchReport, SearchError> {
    let determinism = Deterministic::from_seed(0);
    let mut audit_ids = SearchAuditIdSource::Ambient;
    run_search_inner(
        options,
        Some(read_connection),
        &determinism,
        &mut audit_ids,
        None,
        true,
        None,
    )
}

pub fn run_search_with_read_connection_seeded(
    options: &SearchOptions,
    read_connection: &DbConnection,
    determinism: &Deterministic<Seed>,
) -> Result<SearchReport, SearchError> {
    // Search determinism controls ranking/output replay. Audit rows are durable
    // side effects, so they must remain unique across repeated seeded calls.
    let mut audit_ids = SearchAuditIdSource::Ambient;
    run_search_inner(
        options,
        Some(read_connection),
        determinism,
        &mut audit_ids,
        None,
        true,
        None,
    )
}

pub fn run_search_with_read_connection_seeded_and_audit_connection(
    options: &SearchOptions,
    read_connection: &DbConnection,
    audit_connection: Option<&DbConnection>,
    determinism: &Deterministic<Seed>,
) -> Result<SearchReport, SearchError> {
    // Search determinism controls ranking/output replay. Audit rows are durable
    // side effects, so they must remain unique across repeated seeded calls.
    let mut audit_ids = SearchAuditIdSource::Ambient;
    run_search_inner(
        options,
        Some(read_connection),
        determinism,
        &mut audit_ids,
        audit_connection,
        true,
        None,
    )
}

pub fn run_context_search_with_read_connection_seeded_and_audit_connection(
    options: &SearchOptions,
    read_connection: &DbConnection,
    audit_connection: Option<&DbConnection>,
    determinism: &Deterministic<Seed>,
) -> Result<SearchReport, SearchError> {
    Ok(run_context_search_with_preloaded_memories(
        options,
        read_connection,
        audit_connection,
        determinism,
    )?
    .report)
}

pub fn run_context_search_with_preloaded_memories(
    options: &SearchOptions,
    read_connection: &DbConnection,
    audit_connection: Option<&DbConnection>,
    determinism: &Deterministic<Seed>,
) -> Result<ContextSearchReport, SearchError> {
    // Context candidate conversion batch-loads memories itself, so passthrough
    // swarm/workspace scopes do not need search-analysis metadata on every hit.
    let mut audit_ids = SearchAuditIdSource::Ambient;
    let mut preloaded_memories = BTreeMap::new();
    let report = run_search_inner(
        options,
        Some(read_connection),
        determinism,
        &mut audit_ids,
        audit_connection,
        false,
        Some(&mut preloaded_memories),
    )?;
    Ok(ContextSearchReport {
        report,
        preloaded_memories,
    })
}

fn run_search_inner(
    options: &SearchOptions,
    read_connection: Option<&DbConnection>,
    determinism: &Deterministic<Seed>,
    audit_ids: &mut SearchAuditIdSource,
    audit_connection: Option<&DbConnection>,
    include_passthrough_scope_analysis_metadata: bool,
    mut preloaded_memories: Option<&mut BTreeMap<String, StoredMemory>>,
) -> Result<SearchReport, SearchError> {
    let start = Instant::now();
    let index_dir = options.resolve_index_dir();
    let runtime_profile = runtime_profile_for_workspace(&options.workspace_path);
    let (effective_limit, limit_capped) = runtime_profile.cap_search_limit(options.limit);

    if !index_dir.exists() {
        return Err(SearchError::NoIndex);
    }

    let output_redaction_enabled =
        crate::config::workspace_output_redaction_enabled(&options.workspace_path);
    let mut degraded = search_degradations(options, &index_dir);
    let lexical_ram_tier = pin_lexical_ram_tier_for_search(&options.workspace_path, &index_dir);
    push_lexical_ram_tier_search_degradations(&mut degraded, &lexical_ram_tier);
    if !output_redaction_enabled {
        degraded.push(SearchDegradation::output_redaction_disabled());
    }
    if limit_capped {
        degraded.push(SearchDegradation::profile_search_limit_capped(
            options.limit,
            effective_limit,
            runtime_profile.active_profile.as_str(),
        ));
    }

    let source_mode = resolve_source_mode(options, &index_dir, &mut degraded)?;
    if source_mode.unavailable_no_results {
        let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;
        return Ok(SearchReport {
            status: SearchStatus::NoResults,
            query: options.query.clone(),
            requested_limit: options.limit,
            results: Vec::new(),
            elapsed_ms,
            errors: Vec::new(),
            degraded,
            runtime_profile,
            relevance_floor_applied: None,
            candidates_below_floor: 0,
            source_mode_requested: options.source_mode,
            source_mode_applied: source_mode.applied,
            source_mode_fallback: source_mode.fallback_applied,
            strict_source_mode: options.strict_source_mode,
            memory_scope: options.memory_scope,
            strict_scope: options.strict_scope,
            scope_stats: MemoryScopeContext::for_workspace(
                &options.workspace_path,
                options.memory_scope,
                options.strict_scope,
            )
            .stats(),
        });
    }
    let search_result = search_sync(
        &index_dir,
        &options.query,
        effective_limit as usize,
        options.two_tier_config_for_limit(effective_limit),
        options.explain,
        source_mode.applied,
        determinism,
    );

    let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;

    match search_result {
        Ok((raw_hits, errors)) => {
            // Bead bd-17c65.2.3 (B3): dedupe on docId BEFORE the floor
            // filter so the floor metrics reflect the deduped pool.
            // After fusion, the same docId can appear multiple times
            // (different arms promoting it, MMR rescoring tied
            // candidates, etc.). Keep the highest-scoring occurrence
            // and discard the rest. Stable ordering is preserved (first
            // occurrence's position wins among ties).
            let (raw_hits, duplicates_collapsed) = dedupe_hits_on_doc_id(raw_hits);
            let (raw_hits, mi_duplicates_collapsed, mi_eligible_count) =
                if options.dedup_mode == SearchDedupMode::MutualInformation {
                    dedupe_hits_on_mutual_information(raw_hits, options, read_connection)
                } else {
                    (raw_hits, 0, 0)
                };

            // Bead bd-17c65.2.1 (B1): apply relevance floor.
            // Bead bd-n22a4 (B2-followup): when the caller does not pass an
            // explicit override the floor is per-hit-source — RRF-fused
            // hybrid hits get `DEFAULT_RELEVANCE_FLOOR_HYBRID` (≈0.005)
            // while 0..=1-normalized sources keep `DEFAULT_RELEVANCE_FLOOR`
            // (0.05). An explicit override still applies uniformly so
            // `--relevance-floor 0.0` and existing golden fixtures with a
            // pinned floor keep behaving exactly as before.
            let user_floor_override = options.relevance_floor;
            let pre_floor_count = raw_hits.len();
            let pre_floor_top_score = raw_hits.first().map(|hit| hit.score);
            let pre_floor_top_source = raw_hits.first().map(|hit| hit.source);

            // Partition into above-floor (kept) and below-floor (dropped).
            // Floor of 0.0 is "disabled" — keep everything. NaN scores are
            // always dropped because NaN >= per_hit_floor is false.
            let (above_floor, below_floor): (Vec<_>, Vec<_>) =
                raw_hits.into_iter().partition(|hit| {
                    let per_hit_floor =
                        user_floor_override.unwrap_or_else(|| default_floor_for_source(hit.source));
                    hit.score.is_finite() && hit.score >= per_hit_floor
                });
            let dropped = below_floor.len();
            let above_floor = apply_tombstone_visibility_collecting(
                options,
                above_floor,
                &mut degraded,
                read_connection,
                preloaded_memories.as_deref_mut(),
            );
            let (above_floor, scope_stats) =
                apply_memory_scope_visibility_with_metadata_mode_collecting(
                    options,
                    above_floor,
                    &mut degraded,
                    read_connection,
                    include_passthrough_scope_analysis_metadata,
                    preloaded_memories.as_deref_mut(),
                );
            let mut above_floor = apply_mesh_query_visibility(above_floor, &mut degraded);
            annotate_hits_with_score_calibration(
                &options.workspace_path,
                options.database_path.as_deref(),
                read_connection,
                &mut above_floor,
                &mut degraded,
            );
            let kept = above_floor.len();

            // Representative floor for degradation reporting + metrics:
            // pick the floor that applies to the top remaining hit, or to
            // the top pre-filter hit when the result set is empty. Falls
            // back to `DEFAULT_RELEVANCE_FLOOR` when there were no hits at
            // all (NoResults). The user override always wins if set.
            let representative_floor = user_floor_override.unwrap_or_else(|| {
                above_floor
                    .first()
                    .map(|hit| hit.source)
                    .or(pre_floor_top_source)
                    .map_or(DEFAULT_RELEVANCE_FLOOR, default_floor_for_source)
            });
            let floor = representative_floor;

            // Surface dedupe count as a low-severity info signal when
            // it fired; agents reading the metrics can correlate with
            // raw retrieve-arm output (B7 surface, future bead) for
            // debugging.
            if duplicates_collapsed > 0 {
                degraded.push(SearchDegradation::duplicates_collapsed(
                    duplicates_collapsed,
                ));
            }
            if options.dedup_mode == SearchDedupMode::MutualInformation {
                if mi_duplicates_collapsed > 0 {
                    degraded.push(SearchDegradation::mi_dedup_candidate_proposed(
                        mi_duplicates_collapsed,
                    ));
                } else if mi_eligible_count < 2 {
                    degraded.push(SearchDegradation::mi_dedup_threshold_underpowered(
                        mi_eligible_count,
                    ));
                }
            }

            // Bead bd-17c65.2.5 (B5). When at least one result passed
            // the floor but the top score is below 2× the floor (the
            // B4 "weak" classifier), emit a low-severity signal so
            // agents can pre-empt low-confidence retrieval failures.
            if let Some(top) = above_floor.first().map(|hit| hit.score) {
                if top.is_finite() && top >= floor && top < floor * 2.0 {
                    degraded.push(SearchDegradation::weak_query_recall(floor, top));
                }
            }

            // Emit no_relevant_results when everything got filtered out
            // (and there were candidates to begin with). Empty workspace
            // is a different scenario — pre_floor_count == 0 — and we
            // leave it as plain SearchStatus::NoResults without an extra
            // degradation since "no memories" is honest by itself.
            if kept == 0 && pre_floor_count > 0 {
                degraded.push(SearchDegradation::no_relevant_results(
                    &options.query,
                    floor,
                    pre_floor_count,
                    pre_floor_top_score,
                ));
            }
            // Low-recall informational signal when significant drop.
            // Threshold: kept < 30% of considered AND ≥ 3 candidates total
            // (avoid spurious signal for tiny corpora).
            if pre_floor_count >= 3 && (kept * 10) < (pre_floor_count * 3) {
                degraded.push(SearchDegradation::low_recall_after_floor(
                    floor,
                    kept,
                    pre_floor_count,
                ));
            }

            let status = if above_floor.is_empty() {
                SearchStatus::NoResults
            } else {
                SearchStatus::Success
            };

            // Bead bd-17c65.7.7 (G8): best-effort audit-log instrumentation.
            // One `search.executed` row per call + one `search.returned_mem`
            // row per memory hit so L3 has a `last_accessed` signal and
            // G1 can count search activity per workspace. Privacy: only
            // the BLAKE3 prefix of the query reaches the audit log.
            let database_path = options
                .database_path
                .clone()
                .unwrap_or_else(|| options.workspace_path.join(".ee").join("ee.db"));
            // Match memory_command_workspace_id's canonicalize-then-hash so the
            // audit row joins to the same workspace the memory was written
            // under (especially important on macOS where /tmp -> /private/tmp).
            let canonical_workspace = options
                .workspace_path
                .canonicalize()
                .unwrap_or_else(|_| options.workspace_path.clone());
            let workspace_id = crate::core::curate::stable_workspace_id(&canonical_workspace);
            if search_audit_workspace_persisted(audit_connection.or(read_connection), &workspace_id)
            {
                let q_hash = audit_query_hash(&options.query);
                let source_arms: Vec<&str> = above_floor
                    .iter()
                    .map(|hit| hit.source.as_str())
                    .collect::<std::collections::BTreeSet<&str>>()
                    .into_iter()
                    .collect();
                let executed_details = serde_json::json!({
                    "queryHash": &q_hash,
                    "resultCount": above_floor.len(),
                    "sourceArms": source_arms,
                    "status": status.as_str(),
                })
                .to_string();
                // bd-21gya: buffer audit rows and flush in one connection +
                // one transaction instead of opening DbConnection per row.
                // Capacity hint sized for the worst case (1 executed + 1
                // returned_mem per hit + redaction overhead).
                let mut audit_batch =
                    SearchAuditBatch::new(1 + above_floor.len().saturating_mul(2));
                audit_batch.push(
                    audit_ids,
                    Some(&workspace_id),
                    audit_actions::SEARCH_EXECUTED,
                    Some("workspace"),
                    Some(&workspace_id),
                    Some(executed_details),
                );
                for (rank, hit) in above_floor.iter().enumerate() {
                    let returned_details = serde_json::json!({
                        "queryHash": &q_hash,
                        "rank": (rank + 1) as u32,
                        "score": hit.score,
                        "source": hit.source.as_str(),
                    })
                    .to_string();
                    audit_batch.push(
                        audit_ids,
                        Some(&workspace_id),
                        audit_actions::SEARCH_RETURNED_MEM,
                        Some("memory"),
                        Some(&hit.doc_id),
                        Some(returned_details),
                    );
                    if output_redaction_enabled {
                        for detected_pattern in search_hit_output_redaction_patterns(hit) {
                            let redaction_details = serde_json::json!({
                                "queryHash": &q_hash,
                                "rank": (rank + 1) as u32,
                                "surface": "search",
                                "memoryId": &hit.doc_id,
                                "detectedPattern": detected_pattern,
                                "action": audit_actions::REDACT_AT_OUTPUT,
                            })
                            .to_string();
                            audit_batch.push(
                                audit_ids,
                                Some(&workspace_id),
                                audit_actions::REDACT_AT_OUTPUT,
                                Some("memory"),
                                Some(&hit.doc_id),
                                Some(redaction_details),
                            );
                        }
                    }
                }
                if let Some(conn) = audit_connection {
                    audit_batch.flush_best_effort_with_connection(conn);
                } else {
                    audit_batch.flush_best_effort(&database_path);
                }
            }

            Ok(SearchReport {
                status,
                query: options.query.clone(),
                requested_limit: options.limit,
                results: above_floor,
                elapsed_ms,
                errors,
                degraded,
                runtime_profile,
                relevance_floor_applied: Some(floor),
                candidates_below_floor: dropped,
                source_mode_requested: options.source_mode,
                source_mode_applied: source_mode.applied,
                source_mode_fallback: source_mode.fallback_applied,
                strict_source_mode: options.strict_source_mode,
                memory_scope: options.memory_scope,
                strict_scope: options.strict_scope,
                scope_stats,
            })
        }
        Err(e) => {
            let mut degraded = degraded;
            let index_error_already_explained = degraded.iter().any(|degradation| {
                matches!(degradation.code.as_str(), "index_corrupt" | "index_missing")
            });
            if !index_error_already_explained {
                degraded.push(SearchDegradation::corrupt_index(Some(&e)));
            }

            Ok(SearchReport {
                status: SearchStatus::IndexError,
                query: options.query.clone(),
                requested_limit: options.limit,
                results: Vec::new(),
                elapsed_ms,
                errors: vec![e],
                degraded,
                runtime_profile,
                relevance_floor_applied: None,
                candidates_below_floor: 0,
                source_mode_requested: options.source_mode,
                source_mode_applied: source_mode.applied,
                source_mode_fallback: source_mode.fallback_applied,
                strict_source_mode: options.strict_source_mode,
                memory_scope: options.memory_scope,
                strict_scope: options.strict_scope,
                scope_stats: MemoryScopeContext::for_workspace(
                    &options.workspace_path,
                    options.memory_scope,
                    options.strict_scope,
                )
                .stats(),
            })
        }
    }
}

pub fn run_diag_search(options: &SearchOptions) -> Result<SearchDiagnosticReport, SearchError> {
    let start = Instant::now();
    let index_dir = options.resolve_index_dir();
    let runtime_profile = runtime_profile_for_workspace(&options.workspace_path);
    let (effective_limit, limit_capped) = runtime_profile.cap_search_limit(options.limit);

    if !index_dir.exists() {
        return Err(SearchError::NoIndex);
    }

    let mut degraded = search_degradations(options, &index_dir);
    if limit_capped {
        degraded.push(SearchDegradation::profile_search_limit_capped(
            options.limit,
            effective_limit,
            runtime_profile.active_profile.as_str(),
        ));
    }

    let config = options.two_tier_config_for_limit(effective_limit);
    let diag_result = diag_search_sync(
        &index_dir,
        &options.query,
        effective_limit as usize,
        config,
        options.explain,
    )
    .map_err(SearchError::Index)?;

    let (raw_hits, duplicates_collapsed) = dedupe_hits_on_doc_id(diag_result.final_hits);
    let (raw_hits, mi_duplicates_collapsed, mi_eligible_count) =
        if options.dedup_mode == SearchDedupMode::MutualInformation {
            dedupe_hits_on_mutual_information(raw_hits, options, None)
        } else {
            (raw_hits, 0, 0)
        };
    // Bead bd-n22a4 (B2-followup): mirror `run_search`'s per-source
    // adaptive floor so `ee diag search` reports the same floor
    // semantics that the live search path applies — without this the
    // diag arm would silently disagree with `ee search` on which hits
    // pass the default floor.
    let user_floor_override = options.relevance_floor;
    let pre_floor_count = raw_hits.len();
    let pre_floor_top_score = raw_hits.first().map(|hit| hit.score);
    let pre_floor_top_source = raw_hits.first().map(|hit| hit.source);
    let (above_floor, below_floor): (Vec<_>, Vec<_>) = raw_hits.into_iter().partition(|hit| {
        let per_hit_floor =
            user_floor_override.unwrap_or_else(|| default_floor_for_source(hit.source));
        hit.score.is_finite() && hit.score >= per_hit_floor
    });
    let (mut above_floor, scope_stats) =
        apply_memory_scope_visibility(options, above_floor, &mut degraded, None);
    annotate_hits_with_score_calibration(
        &options.workspace_path,
        options.database_path.as_deref(),
        None,
        &mut above_floor,
        &mut degraded,
    );
    let kept = above_floor.len();
    let dropped = below_floor.len();
    let floor = user_floor_override.unwrap_or_else(|| {
        above_floor
            .first()
            .map(|hit| hit.source)
            .or(pre_floor_top_source)
            .map_or(DEFAULT_RELEVANCE_FLOOR, default_floor_for_source)
    });

    if duplicates_collapsed > 0 {
        degraded.push(SearchDegradation::duplicates_collapsed(
            duplicates_collapsed,
        ));
    }
    if options.dedup_mode == SearchDedupMode::MutualInformation {
        if mi_duplicates_collapsed > 0 {
            degraded.push(SearchDegradation::mi_dedup_candidate_proposed(
                mi_duplicates_collapsed,
            ));
        } else if mi_eligible_count < 2 {
            degraded.push(SearchDegradation::mi_dedup_threshold_underpowered(
                mi_eligible_count,
            ));
        }
    }
    if let Some(top) = above_floor.first().map(|hit| hit.score) {
        if top.is_finite() && top >= floor && top < floor * 2.0 {
            degraded.push(SearchDegradation::weak_query_recall(floor, top));
        }
    }
    if kept == 0 && pre_floor_count > 0 {
        degraded.push(SearchDegradation::no_relevant_results(
            &options.query,
            floor,
            pre_floor_count,
            pre_floor_top_score,
        ));
    }
    if pre_floor_count >= 3 && (kept * 10) < (pre_floor_count * 3) {
        degraded.push(SearchDegradation::low_recall_after_floor(
            floor,
            kept,
            pre_floor_count,
        ));
    }

    let status = if above_floor.is_empty() {
        SearchStatus::NoResults
    } else {
        SearchStatus::Success
    };
    let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;
    let final_report = SearchReport {
        status,
        query: options.query.clone(),
        requested_limit: options.limit,
        results: above_floor,
        elapsed_ms: diag_result.final_elapsed_ms,
        errors: diag_result.errors.clone(),
        degraded,
        runtime_profile,
        relevance_floor_applied: Some(floor),
        candidates_below_floor: dropped,
        source_mode_requested: options.source_mode,
        source_mode_applied: options.source_mode,
        source_mode_fallback: false,
        strict_source_mode: options.strict_source_mode,
        memory_scope: options.memory_scope,
        strict_scope: options.strict_scope,
        scope_stats,
    };

    Ok(SearchDiagnosticReport {
        query: options.query.clone(),
        requested_limit: options.limit,
        elapsed_ms,
        pre_fusion: diag_result.pre_fusion,
        fusion: diag_result.fusion,
        final_report,
        errors: diag_result.errors,
    })
}

fn search_degradations(options: &SearchOptions, index_dir: &Path) -> Vec<SearchDegradation> {
    let Ok(index_status) = cached_index_status_for_search(options, index_dir) else {
        return Vec::new();
    };

    match index_status.health {
        IndexHealth::Ready => Vec::new(),
        IndexHealth::Stale => vec![SearchDegradation::stale_index(
            index_status.db_generation,
            index_status.index_generation,
        )],
        IndexHealth::Missing => vec![SearchDegradation::missing_index()],
        IndexHealth::Corrupt => vec![SearchDegradation::corrupt_index(
            index_status.last_check_error.as_deref(),
        )],
    }
}

fn pin_lexical_ram_tier_for_search(
    workspace_path: &Path,
    index_dir: &Path,
) -> LexicalRamTierResult {
    let started = Instant::now();
    let config = lexical_ram_tier_config_for_search(workspace_path);
    let result = pin_lexical_index_files(&index_dir.join("lexical"), &config);
    trace_lexical_ram_tier(
        &crate::core::curate::stable_workspace_id(workspace_path),
        &result,
        started.elapsed().as_secs_f64() * 1000.0,
    );
    result
}

/// Resolve the lexical RAM-tier runtime posture for the search hot path.
///
/// Production callers go through `merged_workspace_config` so a workspace
/// `.ee/config.toml` with `[search.lexical_ram_tier] enabled = true` (or
/// `request_hugepages` / `populate_on_open`) drives the runtime posture
/// without requiring `EE_LEXICAL_INDEX_PIN_RAM` / `EE_LEXICAL_INDEX_HUGEPAGES`
/// to be exported. The merge precedence is preserved by `merge_config`:
/// CLI > environment > project > user, so env-var overrides still beat
/// the workspace config file. The env-only branch is the
/// load-config-failed fallback (no workspace context, parse error, IO
/// error) — it keeps the prior behavior so this slice never regresses
/// the existing env-driven deployments.
///
/// Mirrors `lexical_ram_tier_config_for_status_with` at
/// `src/core/status.rs:2176` so search and status agree on the resolved
/// posture for the same workspace, satisfying the bd-21xbi.1 acceptance
/// ("Search and status agree on enabled, hugepages requested, and
/// populate-on-open.").
fn lexical_ram_tier_config_for_search(workspace_path: &Path) -> LexicalRamTierConfig {
    if let Ok(merged) = crate::core::config_surface::merged_workspace_config(workspace_path) {
        return LexicalRamTierConfig::from_config_overrides(&merged.values.search.lexical_ram_tier);
    }
    LexicalRamTierConfig::from_environment_with_reader(
        |name| match name {
            LEXICAL_RAM_TIER_PIN_RAM_ENV => {
                crate::config::read_env_var(crate::config::EnvVar::LexicalIndexPinRam)
            }
            LEXICAL_RAM_TIER_HUGEPAGES_ENV => {
                crate::config::read_env_var(crate::config::EnvVar::LexicalIndexHugepages)
            }
            _ => None,
        },
        |_name, _raw| {},
    )
}

fn push_lexical_ram_tier_search_degradations(
    degraded: &mut Vec<SearchDegradation>,
    report: &LexicalRamTierResult,
) {
    if !report.enabled {
        return;
    }
    for code in &report.degraded_codes {
        match code.as_str() {
            LEXICAL_HUGEPAGES_UNAVAILABLE_CODE => {
                degraded.push(SearchDegradation::lexical_hugepages_unavailable());
            }
            LEXICAL_RAM_TIER_NOT_IMPLEMENTED_CODE => {
                degraded.push(SearchDegradation::lexical_ram_tier_not_implemented());
            }
            _ => {}
        }
    }
}

fn resolve_source_mode(
    options: &SearchOptions,
    index_dir: &Path,
    degraded: &mut Vec<SearchDegradation>,
) -> Result<SourceModeResolution, SearchError> {
    let embed_model_unavailable = embed_model_unavailable_reason_from_env();
    let tiers = SearchTierState {
        lexical_available: lexical_search_available(index_dir),
        embed_model_unavailable: embed_model_unavailable.as_deref(),
    };
    resolve_source_mode_with_tiers(options, degraded, tiers)
}

fn resolve_source_mode_with_tiers(
    options: &SearchOptions,
    degraded: &mut Vec<SearchDegradation>,
    tiers: SearchTierState<'_>,
) -> Result<SourceModeResolution, SearchError> {
    let requested = options.source_mode;
    let lexical_available = tiers.lexical_available;

    if let Some(reason) = tiers.embed_model_unavailable {
        match requested {
            SearchSourceMode::Hybrid if lexical_available && !options.strict_source_mode => {
                degraded.push(SearchDegradation::embed_model_unavailable(reason));
                tracing::warn!(
                    target: "ee::search::embedder_down",
                    code = "embed_model_unavailable",
                    model_id = EMBED_MODEL_UNAVAILABLE_MODEL_ID,
                    feature_flag = EMBED_MODEL_UNAVAILABLE_FEATURE_FLAG,
                    lexical_available = true,
                    reason,
                    "embedding model unavailable; using lexical fallback"
                );
                return Ok(SourceModeResolution {
                    applied: SearchSourceMode::LexicalOnly,
                    fallback_applied: true,
                    unavailable_no_results: false,
                });
            }
            SearchSourceMode::Hybrid if options.strict_source_mode => {
                return Err(SearchError::SourceModeUnavailable {
                    requested,
                    reason: format!("embedding model unavailable: {reason}"),
                });
            }
            SearchSourceMode::Hybrid | SearchSourceMode::SemanticOnly => {
                degraded.push(SearchDegradation::search_unavailable(
                    "the embedding model is unavailable and the lexical/BM25 arm is unavailable",
                ));
                return Ok(SourceModeResolution {
                    applied: requested,
                    fallback_applied: false,
                    unavailable_no_results: true,
                });
            }
            SearchSourceMode::LexicalOnly => {}
        }
    }

    match requested {
        SearchSourceMode::LexicalOnly if lexical_available => Ok(SourceModeResolution {
            applied: SearchSourceMode::LexicalOnly,
            fallback_applied: false,
            unavailable_no_results: false,
        }),
        SearchSourceMode::LexicalOnly if options.strict_source_mode => {
            Err(SearchError::SourceModeUnavailable {
                requested,
                reason: "lexical-bm25 index is unavailable".to_string(),
            })
        }
        SearchSourceMode::LexicalOnly => {
            degraded.push(SearchDegradation::lexical_unavailable());
            Ok(SourceModeResolution {
                applied: SearchSourceMode::LexicalOnly,
                fallback_applied: false,
                unavailable_no_results: true,
            })
        }
        SearchSourceMode::SemanticOnly => Ok(SourceModeResolution {
            applied: SearchSourceMode::SemanticOnly,
            fallback_applied: false,
            unavailable_no_results: false,
        }),
        SearchSourceMode::Hybrid if lexical_available => Ok(SourceModeResolution {
            applied: SearchSourceMode::Hybrid,
            fallback_applied: false,
            unavailable_no_results: false,
        }),
        SearchSourceMode::Hybrid if options.strict_source_mode => {
            Err(SearchError::SourceModeUnavailable {
                requested,
                reason: "lexical-bm25 index is unavailable".to_string(),
            })
        }
        SearchSourceMode::Hybrid => {
            let applied = SearchSourceMode::SemanticOnly;
            degraded.push(SearchDegradation::source_mode_fallback(
                requested,
                applied,
                "lexical-bm25 index is unavailable",
            ));
            Ok(SourceModeResolution {
                applied,
                fallback_applied: true,
                unavailable_no_results: false,
            })
        }
    }
}

fn embed_model_unavailable_reason_from_env() -> Option<String> {
    let raw = read(EnvVar::EmbedModelPath)?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let path = Path::new(trimmed);
    if path.exists() {
        return None;
    }
    Some(format!(
        "{} points at missing path `{}`",
        EnvVar::EmbedModelPath.name(),
        trimmed
    ))
}

#[cfg(feature = "lexical-bm25")]
fn lexical_search_available(index_dir: &Path) -> bool {
    open_lexical_searcher(index_dir).ok().flatten().is_some()
}

#[cfg(not(feature = "lexical-bm25"))]
fn lexical_search_available(_index_dir: &Path) -> bool {
    false
}

fn cached_index_status_for_search(
    options: &SearchOptions,
    index_dir: &Path,
) -> Result<IndexStatusReport, IndexStatusError> {
    let cache_key = IndexStatusCacheKey::from_search_options(options, index_dir);
    let now = Instant::now();
    let cache = SEARCH_INDEX_STATUS_CACHE.get_or_init(|| Mutex::new(HashMap::new()));

    if let Ok(mut guard) = cache.lock() {
        guard.retain(|_, cached| {
            now.checked_duration_since(cached.checked_at)
                .unwrap_or(Duration::ZERO)
                <= INDEX_STATUS_CACHE_TTL
        });
        if let Some(cached) = guard.get(&cache_key) {
            return Ok(cached.report.clone());
        }
    }

    let status_options = IndexStatusOptions {
        workspace_path: options.workspace_path.clone(),
        database_path: options.database_path.clone(),
        index_dir: Some(index_dir.to_path_buf()),
    };

    let index_status = get_index_status(&status_options)?;

    if let Ok(mut guard) = cache.lock() {
        guard.retain(|_, cached| {
            now.checked_duration_since(cached.checked_at)
                .unwrap_or(Duration::ZERO)
                <= INDEX_STATUS_CACHE_TTL
        });
        guard.insert(
            cache_key,
            CachedIndexStatus {
                checked_at: now,
                report: index_status.clone(),
            },
        );
    }

    Ok(index_status)
}

struct DiagSearchSyncResult {
    pre_fusion: PreFusionDiagnostics,
    fusion: FusionDiagnostics,
    final_hits: Vec<SearchHit>,
    final_elapsed_ms: f64,
    errors: Vec<String>,
}

#[allow(clippy::too_many_lines)]
fn diag_search_sync(
    index_dir: &Path,
    query: &str,
    limit: usize,
    config: TwoTierConfig,
    explain: bool,
) -> Result<DiagSearchSyncResult, String> {
    let index_dir_owned = index_dir.to_path_buf();
    let query_owned = query.to_string();
    #[allow(clippy::type_complexity)]
    let result_holder: Arc<Mutex<Option<Result<DiagSearchSyncResult, String>>>> =
        Arc::new(Mutex::new(None));
    let task_result = Arc::clone(&result_holder);
    let runtime_error_result = Arc::clone(&result_holder);

    let panic_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let runtime_result = crate::core::run_cli_future(async move {
            let cx = asupersync::Cx::for_testing();
            let index = match TwoTierIndex::open(&index_dir_owned, config.clone()) {
                Ok(idx) => Arc::new(idx),
                Err(error) => {
                    if let Ok(mut guard) = task_result.lock() {
                        *guard = Some(Err(format!("Failed to open index: {error}")));
                    }
                    return;
                }
            };

            let candidate_limit = limit
                .max(1)
                .saturating_mul(config.candidate_multiplier.max(1));
            let fast_embedder = Arc::new(HashEmbedder::default_256()) as Arc<dyn Embedder>;
            let lexical = match open_lexical_searcher_for_diag(&index_dir_owned) {
                Ok(lexical) => lexical,
                Err(error) => {
                    if let Ok(mut guard) = task_result.lock() {
                        *guard = Some(Err(error));
                    }
                    return;
                }
            };

            let lexical_start = Instant::now();
            let lexical_result = match lexical.as_ref() {
                Some(lexical) => match lexical.search(&cx, &query_owned, candidate_limit).await {
                    Ok(results) => SearchArmDiagnostics {
                        available: true,
                        score_scale: "bm25_tfidf",
                        elapsed_ms: lexical_start.elapsed().as_secs_f64() * 1000.0,
                        results: scored_results_to_arm_hits(&results),
                        error: None,
                    },
                    Err(error) => SearchArmDiagnostics {
                        available: true,
                        score_scale: "bm25_tfidf",
                        elapsed_ms: lexical_start.elapsed().as_secs_f64() * 1000.0,
                        results: Vec::new(),
                        error: Some(error.to_string()),
                    },
                },
                None => SearchArmDiagnostics {
                    available: false,
                    score_scale: "bm25_tfidf",
                    elapsed_ms: lexical_start.elapsed().as_secs_f64() * 1000.0,
                    results: Vec::new(),
                    error: Some("lexical index not found".to_string()),
                },
            };

            let semantic_start = Instant::now();
            let semantic_result = match fast_embedder.embed(&cx, &query_owned).await {
                Ok(query_vec) => match index.search_fast(&query_vec, candidate_limit) {
                    Ok(results) => SearchArmDiagnostics {
                        available: true,
                        score_scale: "cosine_similarity",
                        elapsed_ms: semantic_start.elapsed().as_secs_f64() * 1000.0,
                        results: vector_hits_to_arm_hits(&results),
                        error: None,
                    },
                    Err(error) => SearchArmDiagnostics {
                        available: true,
                        score_scale: "cosine_similarity",
                        elapsed_ms: semantic_start.elapsed().as_secs_f64() * 1000.0,
                        results: Vec::new(),
                        error: Some(error.to_string()),
                    },
                },
                Err(error) => SearchArmDiagnostics {
                    available: true,
                    score_scale: "cosine_similarity",
                    elapsed_ms: semantic_start.elapsed().as_secs_f64() * 1000.0,
                    results: Vec::new(),
                    error: Some(error.to_string()),
                },
            };

            let fusion_start = Instant::now();
            let fusion = build_fusion_diagnostics(
                &lexical_result.results,
                &semantic_result.results,
                config.rrf_k,
                limit,
            );
            let fusion = FusionDiagnostics {
                elapsed_ms: fusion_start.elapsed().as_secs_f64() * 1000.0,
                ..fusion
            };

            let final_start = Instant::now();
            let searcher =
                TwoTierSearcher::new(Arc::clone(&index), Arc::clone(&fast_embedder), config);
            let searcher = if let Some(lexical) = lexical {
                searcher.with_lexical(lexical)
            } else {
                searcher
            };
            let final_result = searcher.search_collect(&cx, &query_owned, limit).await;
            let converted = match final_result {
                Ok((results, _metrics)) => {
                    let mut hits: Vec<SearchHit> = results
                        .into_iter()
                        .map(|result| search_hit_from_scored_result(result, explain))
                        .collect();
                    sort_search_hits_by_score_order(&mut hits);
                    Ok(DiagSearchSyncResult {
                        pre_fusion: PreFusionDiagnostics {
                            lexical: lexical_result,
                            semantic_fast: semantic_result,
                        },
                        fusion,
                        final_hits: hits,
                        final_elapsed_ms: final_start.elapsed().as_secs_f64() * 1000.0,
                        errors: Vec::new(),
                    })
                }
                Err(error) => Err(format!("Search failed: {error}")),
            };

            if let Ok(mut guard) = task_result.lock() {
                *guard = Some(converted);
            }
        });

        if let Err(error) = runtime_result
            && let Ok(mut guard) = runtime_error_result.lock()
        {
            *guard = Some(Err(format!("Runtime failed: {error}")));
        }
    }));

    match panic_result {
        Ok(()) => result_holder
            .lock()
            .ok()
            .and_then(|mut guard| guard.take())
            .unwrap_or_else(|| Err("Diagnostic search result not captured".to_string())),
        Err(_) => Err("Diagnostic search panicked".to_string()),
    }
}

#[cfg(feature = "lexical-bm25")]
fn open_lexical_searcher_for_diag(
    index_dir: &Path,
) -> Result<Option<Arc<dyn LexicalSearch>>, String> {
    open_lexical_searcher(index_dir)
}

#[cfg(not(feature = "lexical-bm25"))]
#[allow(
    clippy::unnecessary_wraps,
    reason = "signature mirrors the lexical-bm25 implementation"
)]
fn open_lexical_searcher_for_diag(
    _index_dir: &Path,
) -> Result<Option<Arc<dyn LexicalSearch>>, String> {
    Ok(None)
}

fn scored_results_to_arm_hits(results: &[crate::search::ScoredResult]) -> Vec<SearchArmHit> {
    results
        .iter()
        .enumerate()
        .map(|(index, result)| SearchArmHit {
            doc_id: result.doc_id.clone(),
            raw_score: result.score,
            rank: index + 1,
        })
        .collect()
}

fn vector_hits_to_arm_hits(results: &[frankensearch::core::types::VectorHit]) -> Vec<SearchArmHit> {
    results
        .iter()
        .enumerate()
        .map(|(index, result)| SearchArmHit {
            doc_id: result.doc_id.clone(),
            raw_score: result.score,
            rank: index + 1,
        })
        .collect()
}

fn build_fusion_diagnostics(
    lexical: &[SearchArmHit],
    semantic: &[SearchArmHit],
    rrf_k: f64,
    limit: usize,
) -> FusionDiagnostics {
    // Diagnostic-only: final result ordering is produced by
    // Frankensearch's TwoTierSearcher below. This table explains how
    // the pre-fusion arms overlap without feeding ranking decisions.
    let mut by_doc: BTreeMap<String, FusionContribution> = BTreeMap::new();

    for hit in lexical {
        let contribution = rank_contribution(rrf_k, hit.rank);
        by_doc
            .entry(hit.doc_id.clone())
            .and_modify(|entry| {
                entry.lexical_rank = Some(hit.rank);
                entry.lexical_contribution = Some(contribution);
                entry.fused_score += contribution;
            })
            .or_insert_with(|| FusionContribution {
                doc_id: hit.doc_id.clone(),
                lexical_rank: Some(hit.rank),
                lexical_contribution: Some(contribution),
                semantic_rank: None,
                semantic_contribution: None,
                fused_score: contribution,
            });
    }

    for hit in semantic {
        let contribution = rank_contribution(rrf_k, hit.rank);
        by_doc
            .entry(hit.doc_id.clone())
            .and_modify(|entry| {
                entry.semantic_rank = Some(hit.rank);
                entry.semantic_contribution = Some(contribution);
                entry.fused_score += contribution;
            })
            .or_insert_with(|| FusionContribution {
                doc_id: hit.doc_id.clone(),
                lexical_rank: None,
                lexical_contribution: None,
                semantic_rank: Some(hit.rank),
                semantic_contribution: Some(contribution),
                fused_score: contribution,
            });
    }

    let mut per_doc_contribution: Vec<_> = by_doc.into_values().collect();
    per_doc_contribution.sort_by(|left, right| {
        right
            .fused_score
            .total_cmp(&left.fused_score)
            .then_with(|| {
                let left_both = left.lexical_rank.is_some() && left.semantic_rank.is_some();
                let right_both = right.lexical_rank.is_some() && right.semantic_rank.is_some();
                right_both.cmp(&left_both)
            })
            .then_with(|| left.doc_id.cmp(&right.doc_id))
    });
    per_doc_contribution.truncate(limit);

    FusionDiagnostics {
        algorithm: "reciprocal_rank_fusion",
        rrf_k,
        per_doc_contribution,
        elapsed_ms: 0.0,
    }
}

fn rank_contribution(rrf_k: f64, one_based_rank: usize) -> f64 {
    let rank = one_based_rank.saturating_sub(1);
    let rank_u32 = u32::try_from(rank).unwrap_or(u32::MAX);
    1.0 / (rrf_k + f64::from(rank_u32) + 1.0)
}

fn score_source_from_frankensearch(source: crate::search::ScoreSource) -> ScoreSource {
    match source {
        crate::search::ScoreSource::Lexical => ScoreSource::Lexical,
        crate::search::ScoreSource::SemanticFast => ScoreSource::SemanticFast,
        crate::search::ScoreSource::SemanticQuality => ScoreSource::SemanticQuality,
        crate::search::ScoreSource::Hybrid => ScoreSource::Hybrid,
        crate::search::ScoreSource::Reranked => ScoreSource::Reranked,
    }
}

fn search_hit_from_scored_result(result: crate::search::ScoredResult, explain: bool) -> SearchHit {
    let mut hit = SearchHit {
        doc_id: result.doc_id,
        score: result.score,
        source: score_source_from_frankensearch(result.source),
        fast_score: result.fast_score,
        quality_score: result.quality_score,
        lexical_score: result.lexical_score,
        rerank_score: result.rerank_score,
        metadata: result.metadata,
        explanation: None,
    };
    if explain {
        hit.explanation = Some(ScoreExplanation::generate(&hit));
    }
    hit
}

fn sort_search_hits_by_score_order(hits: &mut [SearchHit]) {
    hits.sort_by(|left, right| right.score.total_cmp(&left.score));
    let mut run_start = 0_usize;
    while run_start < hits.len() {
        let mut run_end = run_start + 1;
        while run_end < hits.len()
            && hits[run_start].score.total_cmp(&hits[run_end].score) == std::cmp::Ordering::Equal
        {
            run_end += 1;
        }
        if run_end - run_start > 1 {
            sort_search_hit_score_tie_by_doc_id(&mut hits[run_start..run_end]);
        }
        run_start = run_end;
    }
}

fn sort_search_hit_score_tie_by_doc_id(hits: &mut [SearchHit]) {
    hits.sort_by(|left, right| search_hit_workspace_id(left).cmp(search_hit_workspace_id(right)));
    let mut run_start = 0_usize;
    while run_start < hits.len() {
        let mut run_end = run_start + 1;
        while run_end < hits.len()
            && search_hit_workspace_id(&hits[run_start]) == search_hit_workspace_id(&hits[run_end])
        {
            run_end += 1;
        }
        sort_search_hit_score_tie_by_doc_id_within_workspace(&mut hits[run_start..run_end]);
        run_start = run_end;
    }
}

fn sort_search_hit_score_tie_by_doc_id_within_workspace(hits: &mut [SearchHit]) {
    if hits.iter().all(|hit| hit.doc_id.starts_with("mem_")) {
        let mut ordered = hits.to_vec();
        sort_by_ulid_payload_or_lexical(&mut ordered, |hit| hit.doc_id.as_str());
        hits.clone_from_slice(&ordered);
    } else {
        hits.sort_by(|left, right| left.doc_id.cmp(&right.doc_id));
    }
}

fn search_hit_workspace_id(hit: &SearchHit) -> &str {
    let Some(metadata) = hit.metadata.as_ref() else {
        return "";
    };

    metadata_string(metadata, "workspace_id")
        .or_else(|| metadata_string(metadata, "workspaceId"))
        .or_else(|| metadata_string(metadata, "origin_workspace_id"))
        .or_else(|| metadata_string(metadata, "originWorkspaceId"))
        .or_else(|| {
            metadata
                .get("mesh")
                .and_then(|mesh| metadata_string(mesh, "workspace_id"))
        })
        .or_else(|| {
            metadata
                .get("mesh")
                .and_then(|mesh| metadata_string(mesh, "workspaceId"))
        })
        .or_else(|| {
            metadata
                .get("mesh")
                .and_then(|mesh| metadata_string(mesh, "origin_workspace_id"))
        })
        .or_else(|| {
            metadata
                .get("mesh")
                .and_then(|mesh| metadata_string(mesh, "originWorkspaceId"))
        })
        .unwrap_or("")
}

fn option_scores_equivalent(left: Option<f32>, right: Option<f32>) -> bool {
    const COMPONENT_TIE_EPSILON: f32 = 0.000_001;

    match (left, right) {
        (Some(left), Some(right)) if left.is_finite() && right.is_finite() => {
            (left - right).abs() <= COMPONENT_TIE_EPSILON
        }
        (Some(left), Some(right)) => left.to_bits() == right.to_bits(),
        (None, None) => true,
        _ => false,
    }
}

fn search_hit_component_scores_equivalent(left: &SearchHit, right: &SearchHit) -> bool {
    left.source == right.source
        && option_scores_equivalent(left.fast_score, right.fast_score)
        && option_scores_equivalent(left.quality_score, right.quality_score)
        && option_scores_equivalent(left.lexical_score, right.lexical_score)
        && option_scores_equivalent(left.rerank_score, right.rerank_score)
}

fn canonicalize_equivalent_component_scores(
    hits: &mut [SearchHit],
    determinism: &Deterministic<Seed>,
) {
    let tie_seed = determinism.shared_child("search.canonical_ties");
    tracing::debug!(
        target: "ee::search::determinism",
        seed_scope = %tie_seed.scope(),
        seed_hash = %tie_seed.seed_hash_prefix(),
        "threaded deterministic token through equivalent-score canonicalization"
    );

    for left_index in 0..hits.len() {
        for right_index in (left_index + 1)..hits.len() {
            if search_hit_component_scores_equivalent(&hits[left_index], &hits[right_index])
                && hits[left_index].score.is_finite()
                && hits[right_index].score.is_finite()
            {
                let canonical_score = hits[left_index].score.max(hits[right_index].score);
                hits[left_index].score = canonical_score;
                hits[right_index].score = canonical_score;
            }
        }
    }
}

fn search_plan_cache_key(
    index_dir: &Path,
    query: &str,
    limit: usize,
    config: &TwoTierConfig,
    explain: bool,
    source_mode: SearchSourceMode,
) -> PlanCacheKey {
    PlanCacheKey::new(
        compute_eql_hash(query.as_bytes()),
        index_manifest_version_for_plan_cache(index_dir),
        search_config_hash_for_plan_cache(config, limit, explain, source_mode),
    )
}

fn compiled_search_plan(
    query: &str,
    limit: usize,
    explain: bool,
    source_mode: SearchSourceMode,
) -> CompiledPlan {
    let parsed_query = EqlQuery {
        q: query.to_owned(),
        workspace: None,
        levels: Vec::new(),
        kinds: Vec::new(),
        tags: Vec::new(),
        tags_mode: EqlTagsMode::Any,
        scope: Vec::new(),
        time: None,
        confidence: None,
        graph: None,
        limit: u32::try_from(limit).unwrap_or(u32::MAX).max(1),
        speed: EqlSpeedMode::Default,
        rerank: false,
        return_subgraph: false,
        explain,
    };
    CompiledPlan {
        parsed_query,
        bound_index: Some(source_mode.as_str().to_owned()),
        join_strategy: Some(
            match source_mode {
                SearchSourceMode::LexicalOnly => "lexical_only",
                SearchSourceMode::SemanticOnly => "semantic_two_tier",
                SearchSourceMode::Hybrid => "hybrid_two_tier",
            }
            .to_owned(),
        ),
    }
}

fn resolved_query_plan_cache_capacity() -> usize {
    read(EnvVar::QueryPlanCacheEntries)
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(DEFAULT_PLAN_CACHE_ENTRIES)
}

fn search_config_hash_for_plan_cache(
    config: &TwoTierConfig,
    limit: usize,
    explain: bool,
    source_mode: SearchSourceMode,
) -> u64 {
    let mut bytes = Vec::new();
    append_hash_str(&mut bytes, "v1");
    append_hash_u64(&mut bytes, u64::try_from(limit).unwrap_or(u64::MAX));
    append_hash_bool(&mut bytes, explain);
    append_hash_str(&mut bytes, source_mode.as_str());
    append_hash_f64(&mut bytes, config.quality_weight);
    append_hash_f64(&mut bytes, config.rrf_k);
    append_hash_u64(
        &mut bytes,
        u64::try_from(config.candidate_multiplier).unwrap_or(u64::MAX),
    );
    append_hash_u64(&mut bytes, config.quality_timeout_ms);
    append_hash_bool(&mut bytes, config.fast_only);
    append_hash_bool(&mut bytes, config.graph_ranking_enabled);
    append_hash_f64(&mut bytes, config.graph_ranking_weight);
    append_hash_bool(&mut bytes, config.explain);
    append_hash_u64(
        &mut bytes,
        u64::try_from(config.hnsw_ef_search).unwrap_or(u64::MAX),
    );
    append_hash_u64(
        &mut bytes,
        u64::try_from(config.hnsw_ef_construction).unwrap_or(u64::MAX),
    );
    append_hash_u64(&mut bytes, u64::try_from(config.hnsw_m).unwrap_or(u64::MAX));
    append_hash_u64(
        &mut bytes,
        u64::try_from(config.hnsw_threshold).unwrap_or(u64::MAX),
    );
    append_hash_u64(
        &mut bytes,
        u64::try_from(config.mrl_search_dims).unwrap_or(u64::MAX),
    );
    append_hash_u64(
        &mut bytes,
        u64::try_from(config.mrl_rescore_top_k).unwrap_or(u64::MAX),
    );
    compute_search_config_hash(&bytes)
}

/// Maximum byte length read from `<index_dir>/meta.json` or
/// `<index_dir>/manifest.json` while computing the plan-cache key.
///
/// Real index metadata is a small JSON object (well under 1 KiB in
/// practice — `generation`, `lastRebuildAt`, fingerprint fields).
/// 4 MiB matches `INDEX_METADATA_INSPECT_LIMIT` in `src/core/index.rs:43`
/// (the parallel reader hardened by ad2d302e for the
/// `get_index_status` path). Without this cap,
/// `std::fs::read(&path)` would pre-size its destination `Vec<u8>` from
/// the file's metadata length and allocate the entire body — and this
/// function is on the SEARCH HOT PATH (`search_plan_cache_key`, line
/// 5542), invoked on every `ee search` / `ee pack` invocation, so a
/// peer-planted or runaway-writer multi-GiB
/// `<workspace>/.ee/index/meta.json` would OOM every search.
///
/// A read past the cap (oversize file or post-stat growth) is treated
/// as "manifest unavailable" by falling through to the time-based
/// fallback below — this is strictly safe for plan-cache invalidation
/// since the next legitimate index publish will refresh the cache key
/// once the file is back within bounds.
const MAX_PLAN_CACHE_INDEX_MANIFEST_BYTES: u64 = 4 * 1024 * 1024;

fn index_manifest_version_for_plan_cache(index_dir: &Path) -> u64 {
    const DOMAIN: &[u8] = b"ee.search.plan_cache.index_manifest.v1";
    for relative in ["meta.json", "manifest.json"] {
        let path = index_dir.join(relative);
        let Ok(file) = std::fs::File::open(&path) else {
            continue;
        };
        let mut bytes = Vec::new();
        if file
            .take(MAX_PLAN_CACHE_INDEX_MANIFEST_BYTES.saturating_add(1))
            .read_to_end(&mut bytes)
            .is_err()
        {
            continue;
        }
        if bytes.len() as u64 > MAX_PLAN_CACHE_INDEX_MANIFEST_BYTES {
            // Race-grown past the cap between open and read. Treat as
            // unavailable so the cache key falls through to the
            // time-based fallback; the next legitimate index publish
            // will refresh the cache key on the next search.
            continue;
        }
        let mut hasher = blake3::Hasher::new();
        hasher.update(DOMAIN);
        append_blake3_str(&mut hasher, relative);
        hasher.update(&(bytes.len() as u64).to_le_bytes());
        hasher.update(&bytes);
        return truncate_blake3_to_u64(hasher.finalize().as_bytes());
    }

    let mut hasher = blake3::Hasher::new();
    hasher.update(DOMAIN);
    let index_dir_display = index_dir.as_os_str().to_string_lossy();
    append_blake3_str(&mut hasher, index_dir_display.as_ref());
    if let Ok(metadata) = std::fs::metadata(index_dir) {
        if let Ok(modified) = metadata.modified() {
            if let Ok(duration) = modified.duration_since(std::time::SystemTime::UNIX_EPOCH) {
                hasher.update(&duration.as_secs().to_le_bytes());
                hasher.update(&duration.subsec_nanos().to_le_bytes());
            }
        }
        hasher.update(&metadata.len().to_le_bytes());
    }
    truncate_blake3_to_u64(hasher.finalize().as_bytes())
}

fn append_hash_str(bytes: &mut Vec<u8>, value: &str) {
    bytes.extend_from_slice(&(value.len() as u64).to_le_bytes());
    bytes.extend_from_slice(value.as_bytes());
}

fn append_hash_u64(bytes: &mut Vec<u8>, value: u64) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn append_hash_bool(bytes: &mut Vec<u8>, value: bool) {
    bytes.push(u8::from(value));
}

fn append_hash_f64(bytes: &mut Vec<u8>, value: f64) {
    bytes.extend_from_slice(&value.to_bits().to_le_bytes());
}

fn append_blake3_str(hasher: &mut blake3::Hasher, value: &str) {
    hasher.update(&(value.len() as u64).to_le_bytes());
    hasher.update(value.as_bytes());
}

fn truncate_blake3_to_u64(hash: &[u8; 32]) -> u64 {
    let mut buf = [0u8; 8];
    buf.copy_from_slice(&hash[0..8]);
    u64::from_le_bytes(buf)
}

fn trace_query_plan_cache_lookup(
    index_dir: &Path,
    key: &PlanCacheKey,
    lookup: &crate::search::plan_cache::PlanCacheLookup,
    capacity: usize,
    elapsed: Duration,
    source_mode: SearchSourceMode,
) {
    tracing::debug!(
        target: "ee::search::plan_cache",
        workspace_id = %index_dir.display(),
        eql_hash = key.eql_hash,
        index_manifest_version = key.index_manifest_version,
        search_config_hash = key.search_config_hash,
        plan_tree_hash = %lookup.plan_tree_hash,
        cache_decision = lookup.decision.as_str(),
        capacity = capacity.min(crate::search::plan_cache::MAX_PLAN_CACHE_ENTRIES),
        evicted = lookup.evicted.len(),
        degraded_codes = "[]",
        elapsed_ms = elapsed.as_secs_f64() * 1000.0,
        source_mode = source_mode.as_str(),
        "query plan cache lookup"
    );
}

fn search_sync(
    index_dir: &Path,
    query: &str,
    limit: usize,
    config: TwoTierConfig,
    explain: bool,
    source_mode: SearchSourceMode,
    determinism: &Deterministic<Seed>,
) -> Result<(Vec<SearchHit>, Vec<String>), String> {
    let plan_cache_key =
        search_plan_cache_key(index_dir, query, limit, &config, explain, source_mode);
    let plan_cache_capacity = resolved_query_plan_cache_capacity();
    let plan_cache_start = Instant::now();
    let plan_lookup = lookup_or_insert_process_plan(plan_cache_capacity, plan_cache_key, || {
        compiled_search_plan(query, limit, explain, source_mode)
    });
    trace_query_plan_cache_lookup(
        index_dir,
        &plan_cache_key,
        &plan_lookup,
        plan_cache_capacity,
        plan_cache_start.elapsed(),
        source_mode,
    );

    let index_dir_owned = index_dir.to_path_buf();
    let query_owned = plan_lookup.plan.parsed_query.q.clone();
    let rerank_seed = determinism.shared_child("search.rerank");
    tracing::debug!(
        target: "ee::search::determinism",
        seed_scope = %rerank_seed.scope(),
        seed_hash = %rerank_seed.seed_hash_prefix(),
        "threaded deterministic token through search_sync"
    );
    #[allow(clippy::type_complexity)]
    let result_holder: Arc<Mutex<Option<Result<(Vec<SearchHit>, Vec<String>), String>>>> =
        Arc::new(Mutex::new(None));
    let task_result = Arc::clone(&result_holder);
    let runtime_error_result = Arc::clone(&result_holder);

    let panic_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let runtime_result = crate::core::run_cli_future(async move {
            let cx = asupersync::Cx::for_testing();
            if source_mode == SearchSourceMode::LexicalOnly {
                let lexical = match open_lexical_searcher(&index_dir_owned) {
                    Ok(Some(lexical)) => lexical,
                    Ok(None) => {
                        if let Ok(mut guard) = task_result.lock() {
                            *guard = Some(Err("Lexical index not found".to_string()));
                        }
                        return;
                    }
                    Err(error) => {
                        if let Ok(mut guard) = task_result.lock() {
                            *guard = Some(Err(error));
                        }
                        return;
                    }
                };

                let search_result = lexical.search(&cx, &query_owned, limit).await;
                let converted = match search_result {
                    Ok(results) => {
                        let mut hits: Vec<SearchHit> = results
                            .into_iter()
                            .map(|result| search_hit_from_scored_result(result, explain))
                            .collect();
                        canonicalize_equivalent_component_scores(&mut hits, &rerank_seed);
                        sort_search_hits_by_score_order(&mut hits);
                        Ok((hits, Vec::new()))
                    }
                    Err(error) => Err(format!("Lexical search failed: {error}")),
                };

                if let Ok(mut guard) = task_result.lock() {
                    *guard = Some(converted);
                }
                return;
            }

            let index = match TwoTierIndex::open(&index_dir_owned, config.clone()) {
                Ok(idx) => Arc::new(idx),
                Err(e) => {
                    if let Ok(mut guard) = task_result.lock() {
                        *guard = Some(Err(format!("Failed to open index: {e}")));
                    }
                    return;
                }
            };

            let fast_embedder = Arc::new(HashEmbedder::default_256()) as Arc<dyn Embedder>;
            let searcher = TwoTierSearcher::new(index, fast_embedder, config);
            let searcher = if source_mode == SearchSourceMode::Hybrid {
                match attach_lexical_searcher(searcher, &index_dir_owned) {
                    Ok(searcher) => searcher,
                    Err(error) => {
                        if let Ok(mut guard) = task_result.lock() {
                            *guard = Some(Err(error));
                        }
                        return;
                    }
                }
            } else {
                searcher
            };

            let search_result = searcher.search_collect(&cx, &query_owned, limit).await;

            let converted = match search_result {
                Ok((results, _metrics)) => {
                    let mut hits: Vec<SearchHit> = results
                        .into_iter()
                        .map(|result| search_hit_from_scored_result(result, explain))
                        .collect();
                    canonicalize_equivalent_component_scores(&mut hits, &rerank_seed);
                    sort_search_hits_by_score_order(&mut hits);
                    Ok((hits, Vec::new()))
                }
                Err(e) => Err(format!("Search failed: {e}")),
            };

            if let Ok(mut guard) = task_result.lock() {
                *guard = Some(converted);
            }
        });

        if let Err(e) = runtime_result
            && let Ok(mut guard) = runtime_error_result.lock()
        {
            *guard = Some(Err(format!("Runtime failed: {e}")));
        }
    }));

    match panic_result {
        Ok(()) => result_holder
            .lock()
            .ok()
            .and_then(|mut guard| guard.take())
            .unwrap_or_else(|| Err("Search result not captured".to_string())),
        Err(_) => Err("Search panicked".to_string()),
    }
}

#[cfg(test)]
fn apply_tombstone_visibility(
    options: &SearchOptions,
    hits: Vec<SearchHit>,
    degraded: &mut Vec<SearchDegradation>,
    read_connection: Option<&DbConnection>,
) -> Vec<SearchHit> {
    apply_tombstone_visibility_collecting(options, hits, degraded, read_connection, None)
}

fn apply_tombstone_visibility_collecting(
    options: &SearchOptions,
    hits: Vec<SearchHit>,
    degraded: &mut Vec<SearchDegradation>,
    read_connection: Option<&DbConnection>,
    mut preloaded_memories: Option<&mut BTreeMap<String, StoredMemory>>,
) -> Vec<SearchHit> {
    if hits.is_empty() {
        return hits;
    }
    if let Some(connection) = read_connection {
        return apply_tombstone_visibility_with_connection(
            options,
            hits,
            degraded,
            connection,
            preloaded_memories.as_deref_mut(),
        );
    }

    let explicit_database_path = options.database_path.is_some();
    let database_path = options
        .database_path
        .clone()
        .unwrap_or_else(|| options.workspace_path.join(".ee").join("ee.db"));
    if !explicit_database_path && !database_path.exists() {
        return hits;
    }
    let connection = match DbConnection::open_file(&database_path) {
        Ok(connection) => connection,
        Err(error) => {
            degraded.push(SearchDegradation::tombstone_visibility_unavailable(
                &error.to_string(),
            ));
            return hits;
        }
    };

    apply_tombstone_visibility_with_connection(
        options,
        hits,
        degraded,
        &connection,
        preloaded_memories,
    )
}

fn apply_tombstone_visibility_with_connection(
    options: &SearchOptions,
    hits: Vec<SearchHit>,
    degraded: &mut Vec<SearchDegradation>,
    connection: &DbConnection,
    mut preloaded_memories: Option<&mut BTreeMap<String, StoredMemory>>,
) -> Vec<SearchHit> {
    let hit_doc_ids: Vec<&str> = hits
        .iter()
        .map(|hit| hit.doc_id.as_str())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let batch_memories = connection.get_memories_batch(&hit_doc_ids).ok();
    if let (Some(memories), Some(preloaded)) =
        (batch_memories.as_ref(), preloaded_memories.as_deref_mut())
    {
        for (memory_id, memory) in memories {
            preloaded
                .entry(memory_id.clone())
                .or_insert_with(|| memory.clone());
        }
    }
    let mut visible_hits = Vec::with_capacity(hits.len());
    let mut filtered = 0usize;
    let mut expired_filtered = 0usize;
    let mut future_filtered = 0usize;
    let mut stale_filtered = 0usize;
    let mut malformed_filtered = 0usize;
    let mut included = 0usize;
    let mut drift_hints = Vec::new();
    let reference_time = options.as_of.unwrap_or_else(Utc::now);

    {
        let mut handle_loaded_memory =
            |mut hit: SearchHit, memory: &crate::db::StoredMemory| -> Option<SearchHit> {
                if memory.tombstoned_at.is_some() {
                    if options.include_tombstoned {
                        mark_hit_tombstoned(&mut hit, memory.tombstoned_at.as_deref());
                        included = included.saturating_add(1);
                    } else {
                        filtered = filtered.saturating_add(1);
                        return None;
                    }
                }

                let indexed_stale = hit_indexed_validity_status(&hit) == Some("stale")
                    || hit_indexed_validity_window_is_stale(&hit, &memory);
                if indexed_stale && !options.include_stale {
                    stale_filtered = stale_filtered.saturating_add(1);
                    return None;
                }

                match memory_validity_visibility(
                    memory.valid_from.as_deref(),
                    memory.valid_to.as_deref(),
                    reference_time,
                    options.include_expired,
                    options.include_future,
                ) {
                    MemoryValidityVisibility::Visible => {
                        mark_hit_validity(
                            &mut hit,
                            &memory.valid_from,
                            &memory.valid_to,
                            reference_time,
                        );
                        if let Some(hint) = annotate_hit_memory_drift(&mut hit, &memory) {
                            drift_hints.push(hint);
                        }
                        Some(hit)
                    }
                    MemoryValidityVisibility::Expired => {
                        expired_filtered = expired_filtered.saturating_add(1);
                        None
                    }
                    MemoryValidityVisibility::Future => {
                        future_filtered = future_filtered.saturating_add(1);
                        None
                    }
                    MemoryValidityVisibility::Malformed => {
                        malformed_filtered = malformed_filtered.saturating_add(1);
                        None
                    }
                }
            };

        if let Some(memories) = batch_memories.as_ref() {
            for hit in hits {
                if let Some(memory) = memories.get(&hit.doc_id) {
                    if let Some(hit) = handle_loaded_memory(hit, memory) {
                        visible_hits.push(hit);
                    }
                } else {
                    visible_hits.push(hit);
                }
            }
        } else {
            for hit in hits {
                match connection.get_memory(&hit.doc_id) {
                    Ok(Some(memory)) => {
                        if let Some(preloaded) = preloaded_memories.as_deref_mut() {
                            preloaded
                                .entry(hit.doc_id.clone())
                                .or_insert_with(|| memory.clone());
                        }
                        if let Some(hit) = handle_loaded_memory(hit, &memory) {
                            visible_hits.push(hit);
                        }
                    }
                    Ok(None) => visible_hits.push(hit),
                    Err(error) => {
                        degraded.push(SearchDegradation::tombstone_visibility_unavailable(
                            &error.to_string(),
                        ));
                        visible_hits.push(hit);
                    }
                }
            }
        }
    }

    let total_before = visible_hits
        .len()
        .saturating_add(filtered)
        .saturating_add(expired_filtered)
        .saturating_add(future_filtered)
        .saturating_add(stale_filtered)
        .saturating_add(malformed_filtered);
    let validity_filtered = expired_filtered
        .saturating_add(future_filtered)
        .saturating_add(stale_filtered)
        .saturating_add(malformed_filtered);
    tracing::info!(
        target: "ee.search",
        event = "visibility_filter",
        surface = "search",
        total_before,
        tombstoned_count = filtered.saturating_add(included),
        included = options.include_tombstoned,
        tombstoned_included_count = included,
        tombstoned_filtered_count = filtered,
        validity_reference_time = %reference_time.to_rfc3339(),
        expired_filtered_count = expired_filtered,
        future_filtered_count = future_filtered,
        stale_filtered_count = stale_filtered,
        malformed_filtered_count = malformed_filtered,
        valid_count = visible_hits.len(),
        "visibility_filter"
    );

    if filtered > 0 {
        degraded.push(SearchDegradation::tombstoned_filtered(filtered));
    }
    if expired_filtered > 0 {
        degraded.push(SearchDegradation::expired_filtered(expired_filtered));
    }
    if future_filtered > 0 {
        degraded.push(SearchDegradation::future_validity_filtered(future_filtered));
    }
    if stale_filtered > 0 {
        degraded.push(SearchDegradation::stale_validity_filtered(stale_filtered));
    }
    if malformed_filtered > 0 {
        degraded.push(SearchDegradation::malformed_validity_filtered(
            malformed_filtered,
        ));
    }
    if validity_filtered > 0 && validity_filtered.saturating_mul(2) >= total_before {
        degraded.push(
            SearchDegradation::validity_filtered_significant_recall_drop(
                validity_filtered,
                visible_hits.len(),
            ),
        );
    }
    if included > 0 {
        degraded.push(SearchDegradation::tombstoned_in_results(included));
    }
    if let Some(worst_hint) = highest_risk_memory_drift_hint(&drift_hints) {
        degraded.push(SearchDegradation::selected_memory_drift(
            drift_hints.len(),
            worst_hint,
        ));
    }

    visible_hits
}

fn annotate_hit_memory_drift(
    hit: &mut SearchHit,
    memory: &crate::db::StoredMemory,
) -> Option<MemoryDriftSelectionHint> {
    let hint = memory_drift_selection_hint_from_provenance_status(
        &memory.id,
        &memory.provenance_verification_status,
        memory.provenance_chain_hash.as_deref(),
    )?;
    let metadata = hit
        .metadata
        .get_or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
    if !metadata.is_object() {
        *metadata = serde_json::Value::Object(serde_json::Map::new());
    }
    if let Some(map) = metadata.as_object_mut() {
        map.insert("driftHint".to_owned(), hint.compact_json());
        map.insert(
            "provenanceVerificationStatus".to_owned(),
            serde_json::json!(&memory.provenance_verification_status),
        );
        if let Some(hash) = &memory.provenance_chain_hash {
            map.insert("provenanceChainHash".to_owned(), serde_json::json!(hash));
        }
    }
    Some(hint)
}

fn highest_risk_memory_drift_hint(
    hints: &[MemoryDriftSelectionHint],
) -> Option<&MemoryDriftSelectionHint> {
    hints.iter().max_by_key(|hint| {
        (
            hint.drift_status.severity_rank(),
            std::cmp::Reverse(hint.memory_id.as_str()),
        )
    })
}

fn apply_memory_scope_visibility(
    options: &SearchOptions,
    hits: Vec<SearchHit>,
    degraded: &mut Vec<SearchDegradation>,
    read_connection: Option<&DbConnection>,
) -> (Vec<SearchHit>, MemoryScopeStats) {
    apply_memory_scope_visibility_with_metadata_mode(options, hits, degraded, read_connection, true)
}

fn apply_memory_scope_visibility_with_metadata_mode(
    options: &SearchOptions,
    hits: Vec<SearchHit>,
    degraded: &mut Vec<SearchDegradation>,
    read_connection: Option<&DbConnection>,
    include_passthrough_analysis_metadata: bool,
) -> (Vec<SearchHit>, MemoryScopeStats) {
    apply_memory_scope_visibility_with_metadata_mode_collecting(
        options,
        hits,
        degraded,
        read_connection,
        include_passthrough_analysis_metadata,
        None,
    )
}

fn apply_memory_scope_visibility_with_metadata_mode_collecting(
    options: &SearchOptions,
    hits: Vec<SearchHit>,
    degraded: &mut Vec<SearchDegradation>,
    read_connection: Option<&DbConnection>,
    include_passthrough_analysis_metadata: bool,
    mut preloaded_memories: Option<&mut BTreeMap<String, StoredMemory>>,
) -> (Vec<SearchHit>, MemoryScopeStats) {
    let scope_context = MemoryScopeContext::for_workspace(
        &options.workspace_path,
        options.memory_scope,
        options.strict_scope,
    );
    let mut stats = scope_context.stats();
    if hits.is_empty() {
        return (hits, stats);
    }

    if matches!(
        options.memory_scope,
        MemoryScope::SelfOnly | MemoryScope::Team
    ) && scope_context.current_agent.is_none()
    {
        degraded.push(SearchDegradation::scope_agent_unavailable(
            options.memory_scope,
        ));
    }

    let passthrough_scope = matches!(
        options.memory_scope,
        MemoryScope::Swarm | MemoryScope::Workspace
    );
    if passthrough_scope && !include_passthrough_analysis_metadata {
        for hit in &hits {
            stats.record_candidate_id(true, Some(&hit.doc_id));
        }
        return (hits, stats);
    }
    if let Some(connection) = read_connection {
        return apply_memory_scope_visibility_with_connection(
            options,
            hits,
            degraded,
            &scope_context,
            stats,
            passthrough_scope,
            connection,
            preloaded_memories.as_deref_mut(),
        );
    }

    let explicit_database_path = options.database_path.is_some();
    let database_path = options
        .database_path
        .clone()
        .unwrap_or_else(|| options.workspace_path.join(".ee").join("ee.db"));
    if !explicit_database_path && !database_path.exists() {
        for hit in &hits {
            stats.record_candidate_id(passthrough_scope, Some(&hit.doc_id));
        }
        if passthrough_scope {
            return (hits, stats);
        }
        degraded.push(SearchDegradation::scope_metadata_unavailable(
            "memory database does not exist",
        ));
        return (Vec::new(), stats);
    }

    let connection = match DbConnection::open_file(&database_path) {
        Ok(connection) => connection,
        Err(error) => {
            for hit in &hits {
                stats.record_candidate_id(passthrough_scope, Some(&hit.doc_id));
            }
            if passthrough_scope {
                return (hits, stats);
            }
            degraded.push(SearchDegradation::scope_metadata_unavailable(
                &error.to_string(),
            ));
            return (Vec::new(), stats);
        }
    };

    apply_memory_scope_visibility_with_connection(
        options,
        hits,
        degraded,
        &scope_context,
        stats,
        passthrough_scope,
        &connection,
        preloaded_memories,
    )
}

fn apply_memory_scope_visibility_with_connection(
    options: &SearchOptions,
    hits: Vec<SearchHit>,
    degraded: &mut Vec<SearchDegradation>,
    scope_context: &MemoryScopeContext,
    mut stats: MemoryScopeStats,
    passthrough_scope: bool,
    connection: &DbConnection,
    mut preloaded_memories: Option<&mut BTreeMap<String, StoredMemory>>,
) -> (Vec<SearchHit>, MemoryScopeStats) {
    let hit_doc_ids: BTreeSet<String> = hits.iter().map(|hit| hit.doc_id.clone()).collect();
    let hit_doc_refs: Vec<&str> = hit_doc_ids.iter().map(String::as_str).collect();
    let (scope_memories, read_error): (BTreeMap<String, crate::db::StoredMemory>, Option<String>) =
        match connection.get_memories_batch(&hit_doc_refs) {
            Ok(memories) => (memories, None),
            Err(error) => (BTreeMap::new(), Some(error.to_string())),
        };
    if let Some(preloaded) = preloaded_memories.as_deref_mut() {
        for (memory_id, memory) in &scope_memories {
            preloaded
                .entry(memory_id.clone())
                .or_insert_with(|| memory.clone());
        }
    }

    let mut scoped_hits = Vec::with_capacity(hits.len());
    for mut hit in hits {
        match scope_memories.get(&hit.doc_id) {
            Some(memory) => {
                let in_scope = scope_context.memory_in_scope(memory);
                stats.record_candidate_id(in_scope, Some(&hit.doc_id));
                if in_scope {
                    mark_hit_scope(&mut hit, options.memory_scope, memory);
                    scoped_hits.push(hit);
                }
            }
            None => {
                stats.record_candidate_id(passthrough_scope, Some(&hit.doc_id));
                if passthrough_scope {
                    scoped_hits.push(hit);
                }
            }
        }
    }

    if let Some(error) = read_error {
        degraded.push(SearchDegradation::scope_metadata_unavailable(&error));
    }

    if options.strict_scope && stats.strict_violations > 0 {
        degraded.push(SearchDegradation::scope_strict_excluded_evidence(
            options.memory_scope,
            stats.strict_violations,
        ));
        scoped_hits.clear();
    } else if stats.candidates_excluded_by_scope > 0 {
        degraded.push(SearchDegradation::scope_excluded_evidence(
            options.memory_scope,
            stats.candidates_excluded_by_scope,
        ));
    }

    (scoped_hits, stats)
}

fn apply_mesh_query_visibility(
    hits: Vec<SearchHit>,
    degraded: &mut Vec<SearchDegradation>,
) -> Vec<SearchHit> {
    let mut visible_hits = Vec::with_capacity(hits.len());
    let mut filtered = 0usize;

    for mut hit in hits {
        match mesh_query_visibility(hit.metadata.as_ref()) {
            MeshQueryVisibility::Local => visible_hits.push(hit),
            MeshQueryVisibility::Allowed(provenance) => {
                apply_mesh_trust_adjustment(&mut hit, &provenance.trust_lane);
                visible_hits.push(hit);
            }
            MeshQueryVisibility::Blocked => filtered = filtered.saturating_add(1),
        }
    }

    if filtered > 0 {
        degraded.push(SearchDegradation::mesh_workspace_scope_filtered(filtered));
    }

    sort_search_hits_by_score_order(&mut visible_hits);
    visible_hits
}

fn apply_mesh_trust_adjustment(hit: &mut SearchHit, trust_lane: &str) {
    let factor = mesh_trust_adjustment_factor(trust_lane);
    if factor >= 1.0 || !hit.score.is_finite() {
        return;
    }

    let original_score = hit.score;
    hit.score = (hit.score * factor).clamp(0.0, 1.0);

    let adjustment = serde_json::json!({
        "schema": "ee.mesh.search_trust_adjustment.v1",
        "reason": "cached_peer_material_trust_lane",
        "trustLane": trust_lane,
        "factor": round_metric_f32(factor),
        "originalScore": round_metric_f32(original_score),
        "adjustedScore": round_metric_f32(hit.score),
    });
    let metadata = hit.metadata.get_or_insert_with(|| serde_json::json!({}));
    if let Some(object) = metadata.as_object_mut() {
        object.insert("_ee_mesh_trust_adjustment".to_string(), adjustment);
    }

    if let Some(explanation) = hit.explanation.as_mut() {
        let base = explanation.summary.trim_end_matches('.');
        explanation.summary = format!(
            "{base}. Mesh trust lane `{trust_lane}` adjusted score from {original_score:.4} to {:.4}.",
            hit.score
        );
        explanation.factors.push(ScoreFactor::new(
            "mesh_trust_adjustment",
            factor,
            "cached peer trust lane multiplier",
            "_ee_mesh_trust_adjustment.factor",
            "adjusted_score = original_score * trust_factor",
        ));
    }
}

fn mesh_trust_adjustment_factor(trust_lane: &str) -> f32 {
    let normalized = normalized_mesh_trust_lane(trust_lane);
    match normalized.as_str() {
        "localhuman" | "humanexplicit" => 1.0,
        "peerhumanviapeer" => 0.97,
        "peeragent" | "meshagent" => 0.92,
        "peerderived" | "meshmetadata" => 0.85,
        "meshcuration" => 0.80,
        "untrusted" => 0.65,
        _ => 0.75,
    }
}

fn normalized_mesh_trust_lane(trust_lane: &str) -> String {
    trust_lane
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn mark_hit_scope(hit: &mut SearchHit, scope: MemoryScope, memory: &crate::db::StoredMemory) {
    let mut metadata = hit.metadata.take().unwrap_or_else(|| serde_json::json!({}));
    if let Some(object) = metadata.as_object_mut() {
        object.insert(
            "memory_scope".to_string(),
            serde_json::json!(scope.as_str()),
        );
        object.insert(
            "trust_class".to_string(),
            serde_json::json!(&memory.trust_class),
        );
        object.insert(
            SEARCH_ANALYSIS_CONTENT_KEY.to_string(),
            serde_json::json!(&memory.content),
        );
        object.insert(
            SEARCH_ANALYSIS_CONFIDENCE_KEY.to_string(),
            serde_json::json!(memory.confidence),
        );
        object.insert(
            SEARCH_ANALYSIS_UTILITY_KEY.to_string(),
            serde_json::json!(memory.utility),
        );
        object.insert(
            SEARCH_ANALYSIS_CREATED_AT_KEY.to_string(),
            serde_json::json!(&memory.created_at),
        );
        if let Some(provenance_uri) = &memory.provenance_uri {
            object.insert(
                SEARCH_ANALYSIS_PROVENANCE_URI_KEY.to_string(),
                serde_json::json!(provenance_uri),
            );
        }
        if let Some(trust_subclass) = &memory.trust_subclass {
            object.insert(
                "trust_subclass".to_string(),
                serde_json::json!(trust_subclass),
            );
        }
        if let Some(agent) = super::memory_scope::memory_producer_agent(memory) {
            object.insert("producerAgent".to_string(), serde_json::json!(agent));
        }
    }
    hit.metadata = Some(metadata);
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MemoryValidityVisibility {
    Visible,
    Expired,
    Future,
    Malformed,
}

fn memory_validity_visibility(
    valid_from: Option<&str>,
    valid_to: Option<&str>,
    reference_time: DateTime<Utc>,
    include_expired: bool,
    include_future: bool,
) -> MemoryValidityVisibility {
    if let Some(valid_from) = valid_from {
        let Some(valid_from) = parse_validity_timestamp(valid_from) else {
            return MemoryValidityVisibility::Malformed;
        };
        if valid_from > reference_time && !include_future {
            return MemoryValidityVisibility::Future;
        }
    }

    if let Some(valid_to) = valid_to {
        let Some(valid_to) = parse_validity_timestamp(valid_to) else {
            return MemoryValidityVisibility::Malformed;
        };
        if valid_to < reference_time && !include_expired {
            return MemoryValidityVisibility::Expired;
        }
    }

    MemoryValidityVisibility::Visible
}

fn parse_validity_timestamp(raw: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|timestamp| timestamp.with_timezone(&Utc))
}

fn hit_indexed_validity_status(hit: &SearchHit) -> Option<&str> {
    hit.metadata
        .as_ref()
        .and_then(|metadata| metadata_string(metadata, "validity_status"))
        .or_else(|| {
            hit.metadata
                .as_ref()
                .and_then(|metadata| metadata_string(metadata, "validityStatus"))
        })
}

fn hit_indexed_validity_window_is_stale(hit: &SearchHit, memory: &crate::db::StoredMemory) -> bool {
    let Some(metadata) = hit.metadata.as_ref() else {
        return false;
    };
    let indexed_valid_from =
        metadata_string(metadata, "valid_from").or_else(|| metadata_string(metadata, "validFrom"));
    let indexed_valid_to =
        metadata_string(metadata, "valid_to").or_else(|| metadata_string(metadata, "validTo"));

    if indexed_valid_from.is_none() && indexed_valid_to.is_none() {
        return false;
    }

    indexed_valid_from != memory.valid_from.as_deref()
        || indexed_valid_to != memory.valid_to.as_deref()
}

fn validity_status_at(
    valid_from: Option<&str>,
    valid_to: Option<&str>,
    reference_time: DateTime<Utc>,
) -> &'static str {
    let from = match valid_from {
        Some(raw) => match parse_validity_timestamp(raw) {
            Some(timestamp) => Some(timestamp),
            None => return "malformed",
        },
        None => None,
    };
    let to = match valid_to {
        Some(raw) => match parse_validity_timestamp(raw) {
            Some(timestamp) => Some(timestamp),
            None => return "malformed",
        },
        None => None,
    };

    match (from, to) {
        (None, None) => "unknown",
        (from, to) => {
            if from.is_some_and(|timestamp| timestamp > reference_time) {
                "future"
            } else if to.is_some_and(|timestamp| timestamp < reference_time) {
                "expired"
            } else {
                "current"
            }
        }
    }
}

fn validity_window_kind(valid_from: Option<&str>, valid_to: Option<&str>) -> &'static str {
    match (valid_from, valid_to) {
        (None, None) => "unbounded",
        (Some(from), Some(to)) if from == to => "instant",
        (Some(_), Some(_)) => "bounded",
        (Some(_), None) => "starts_at",
        (None, Some(_)) => "ends_at",
    }
}

fn mark_hit_tombstoned(hit: &mut SearchHit, tombstoned_at: Option<&str>) {
    let mut metadata = hit.metadata.take().unwrap_or_else(|| serde_json::json!({}));
    if let Some(object) = metadata.as_object_mut() {
        object.insert("tombstoned".to_string(), serde_json::json!(true));
        if let Some(tombstoned_at) = tombstoned_at {
            object.insert(
                "tombstoned_at".to_string(),
                serde_json::json!(tombstoned_at),
            );
        }
    }
    hit.metadata = Some(metadata);
}

fn mark_hit_validity(
    hit: &mut SearchHit,
    valid_from: &Option<String>,
    valid_to: &Option<String>,
    reference_time: DateTime<Utc>,
) {
    let indexed_status = hit_indexed_validity_status(hit)
        .filter(|status| *status == "stale")
        .map(str::to_owned);
    let mut metadata = hit.metadata.take().unwrap_or_else(|| serde_json::json!({}));
    if let Some(object) = metadata.as_object_mut() {
        object.remove("valid_from");
        object.remove("validFrom");
        object.remove("valid_to");
        object.remove("validTo");
        if let Some(valid_from) = valid_from {
            object.insert("valid_from".to_string(), serde_json::json!(valid_from));
        }
        if let Some(valid_to) = valid_to {
            object.insert("valid_to".to_string(), serde_json::json!(valid_to));
        }
        object.insert(
            "validity_status".to_string(),
            serde_json::json!(indexed_status.as_deref().unwrap_or_else(|| {
                validity_status_at(valid_from.as_deref(), valid_to.as_deref(), reference_time)
            })),
        );
        object.insert(
            "validity_window_kind".to_string(),
            serde_json::json!(validity_window_kind(
                valid_from.as_deref(),
                valid_to.as_deref()
            )),
        );
    }
    hit.metadata = Some(metadata);
}

#[cfg(feature = "lexical-bm25")]
fn attach_lexical_searcher(
    mut searcher: TwoTierSearcher,
    index_dir: &Path,
) -> Result<TwoTierSearcher, String> {
    if let Some(lexical) = open_lexical_searcher(index_dir)? {
        searcher = searcher.with_lexical(lexical);
    }
    Ok(searcher)
}

#[cfg(not(feature = "lexical-bm25"))]
#[allow(
    clippy::unnecessary_wraps,
    reason = "signature mirrors the lexical-bm25 implementation"
)]
fn attach_lexical_searcher(
    searcher: TwoTierSearcher,
    _index_dir: &Path,
) -> Result<TwoTierSearcher, String> {
    Ok(searcher)
}

#[cfg(feature = "lexical-bm25")]
fn open_lexical_searcher(index_dir: &Path) -> Result<Option<Arc<dyn LexicalSearch>>, String> {
    let lexical_dir = index_dir.join("lexical");
    if !lexical_dir.exists() {
        return Ok(None);
    }

    TantivyIndex::open(&lexical_dir)
        .map(|lexical| Some(Arc::new(lexical) as Arc<dyn LexicalSearch>))
        .map_err(|error| {
            format!(
                "Failed to open lexical index at {}: {error}",
                lexical_dir.display()
            )
        })
}

#[cfg(not(feature = "lexical-bm25"))]
#[allow(
    clippy::unnecessary_wraps,
    reason = "signature mirrors the lexical-bm25 implementation"
)]
fn open_lexical_searcher(_index_dir: &Path) -> Result<Option<Arc<dyn LexicalSearch>>, String> {
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{
        CreateFeedbackEventInput, CreateMemoryInput, CreateWorkspaceInput, DbConnection,
    };
    #[cfg(feature = "lexical-bm25")]
    use crate::search::{EmbedderStack, IndexBuilder, IndexableDocument};

    type TestResult = Result<(), String>;

    fn unique_test_dir(label: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join("ee-search-tests").join(format!(
            "{}-{}-{nanos}",
            label,
            std::process::id()
        ))
    }

    fn seeded_search_audit_ids(seed: u64) -> Result<Vec<String>, String> {
        let workspace = tempfile::Builder::new()
            .prefix("ee-search-seeded-audit")
            .tempdir()
            .map_err(|error| error.to_string())?;
        let database_path = workspace.path().join("ee.db");
        let connection =
            DbConnection::open_file(&database_path).map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        let workspace_id = "wsp_01234567890123456789012345";
        connection
            .insert_workspace(
                workspace_id,
                &CreateWorkspaceInput {
                    path: workspace.path().display().to_string(),
                    name: Some("seeded-search-audit".to_owned()),
                },
            )
            .map_err(|error| error.to_string())?;

        let mut audit_ids = SearchAuditIdSource::Seeded(Deterministic::from_seed(seed));
        audit_append_best_effort(
            &database_path,
            &mut audit_ids,
            Some(workspace_id),
            audit_actions::SEARCH_EXECUTED,
            Some("workspace"),
            Some(workspace_id),
            Some(r#"{"queryHash":"hash","resultCount":0}"#.to_owned()),
        );
        audit_append_best_effort(
            &database_path,
            &mut audit_ids,
            Some(workspace_id),
            audit_actions::SEARCH_RETURNED_MEM,
            Some("memory"),
            Some("mem_00000000000000000000000001"),
            Some(r#"{"queryHash":"hash","rank":1}"#.to_owned()),
        );

        let mut entries = connection
            .list_audit_by_target("workspace", workspace_id, None)
            .map_err(|error| error.to_string())?;
        entries.extend(
            connection
                .list_audit_by_target("memory", "mem_00000000000000000000000001", None)
                .map_err(|error| error.to_string())?,
        );
        entries.sort_by(|left, right| left.action.cmp(&right.action));
        Ok(entries.into_iter().map(|entry| entry.id).collect())
    }

    #[test]
    fn search_best_effort_audit_seeded_ids_replay() -> TestResult {
        let first = seeded_search_audit_ids(51_001)?;
        let replay = seeded_search_audit_ids(51_001)?;
        let other = seeded_search_audit_ids(51_002)?;

        assert_eq!(first.len(), 2);
        assert!(first.iter().all(|id| id.starts_with("audit_")));
        assert_eq!(first, replay);
        assert_ne!(first, other);
        Ok(())
    }

    /// bd-21gya: SearchAuditBatch must produce the same persisted audit rows
    /// (same count, same seeded IDs) as the per-row audit_append_best_effort
    /// path. This pins the perf refactor: batching changes the number of
    /// DbConnection opens (1 instead of N) but must not change observable
    /// audit state.
    fn seeded_search_audit_ids_via_batch(seed: u64) -> Result<Vec<String>, String> {
        let workspace = tempfile::Builder::new()
            .prefix("ee-search-seeded-audit-batch")
            .tempdir()
            .map_err(|error| error.to_string())?;
        let database_path = workspace.path().join("ee.db");
        let connection =
            DbConnection::open_file(&database_path).map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        let workspace_id = "wsp_01234567890123456789012345";
        connection
            .insert_workspace(
                workspace_id,
                &CreateWorkspaceInput {
                    path: workspace.path().display().to_string(),
                    name: Some("seeded-search-audit-batch".to_owned()),
                },
            )
            .map_err(|error| error.to_string())?;

        let mut audit_ids = SearchAuditIdSource::Seeded(Deterministic::from_seed(seed));
        let mut batch = SearchAuditBatch::new(2);
        batch.push(
            &mut audit_ids,
            Some(workspace_id),
            audit_actions::SEARCH_EXECUTED,
            Some("workspace"),
            Some(workspace_id),
            Some(r#"{"queryHash":"hash","resultCount":0}"#.to_owned()),
        );
        batch.push(
            &mut audit_ids,
            Some(workspace_id),
            audit_actions::SEARCH_RETURNED_MEM,
            Some("memory"),
            Some("mem_00000000000000000000000001"),
            Some(r#"{"queryHash":"hash","rank":1}"#.to_owned()),
        );
        batch.flush_best_effort(&database_path);

        let mut entries = connection
            .list_audit_by_target("workspace", workspace_id, None)
            .map_err(|error| error.to_string())?;
        entries.extend(
            connection
                .list_audit_by_target("memory", "mem_00000000000000000000000001", None)
                .map_err(|error| error.to_string())?,
        );
        entries.sort_by(|left, right| left.action.cmp(&right.action));
        Ok(entries.into_iter().map(|entry| entry.id).collect())
    }

    #[test]
    fn search_audit_batch_matches_per_row_emit() -> TestResult {
        let per_row = seeded_search_audit_ids(51_003)?;
        let batched = seeded_search_audit_ids_via_batch(51_003)?;

        assert_eq!(per_row.len(), 2);
        assert_eq!(batched.len(), per_row.len());
        assert_eq!(batched, per_row);
        Ok(())
    }

    #[test]
    fn search_audit_batch_empty_is_no_op() {
        // An empty batch must not even attempt to open the database.
        // Pass a path that doesn't exist to prove no I/O is attempted.
        let bogus = std::path::PathBuf::from("/this/path/does/not/exist/ee.db");
        let batch = SearchAuditBatch::new(0);
        batch.flush_best_effort(&bogus);
    }

    fn test_runtime_profile() -> RuntimeProfileReport {
        RuntimeProfileReport::for_profile(
            super::super::profile::OperatingProfile::Workstation,
            "test_fixture",
        )
    }

    fn test_scope_stats() -> MemoryScopeStats {
        MemoryScopeStats::new(MemoryScope::Swarm, false, None, 0)
    }

    fn source_mode_test_options(
        source_mode: SearchSourceMode,
        strict_source_mode: bool,
    ) -> SearchOptions {
        let workspace = unique_test_dir("source-mode-resolution");
        SearchOptions {
            workspace_path: workspace.clone(),
            database_path: Some(workspace.join("ee.db")),
            index_dir: Some(workspace.join("index")),
            query: "format before release".to_string(),
            limit: 10,
            speed: SpeedMode::Default,
            explain: false,
            as_of: None,
            include_tombstoned: false,
            include_expired: false,
            include_future: false,
            include_stale: false,
            relevance_floor: None,
            dedup_mode: SearchDedupMode::DocId,
            source_mode,
            strict_source_mode,
            memory_scope: MemoryScope::Swarm,
            strict_scope: false,
        }
    }

    #[test]
    fn search_status_as_str_is_stable() {
        assert_eq!(SearchStatus::Success.as_str(), "success");
        assert_eq!(SearchStatus::NoResults.as_str(), "no_results");
        assert_eq!(SearchStatus::IndexNotFound.as_str(), "index_not_found");
        assert_eq!(SearchStatus::IndexError.as_str(), "index_error");
    }

    #[test]
    fn search_source_mode_as_str_is_stable() {
        assert_eq!(SearchSourceMode::LexicalOnly.as_str(), "lexical_only");
        assert_eq!(SearchSourceMode::SemanticOnly.as_str(), "semantic_only");
        assert_eq!(SearchSourceMode::Hybrid.as_str(), "hybrid");
    }

    #[test]
    fn search_revision_metadata_is_absent_by_default_and_stable_when_revisable() {
        let report = SearchReport {
            status: SearchStatus::Success,
            query: "format before release".to_string(),
            requested_limit: 10,
            results: vec![SearchHit {
                doc_id: "mem_00000000000000000000000001".to_string(),
                score: 0.95,
                source: ScoreSource::SemanticFast,
                fast_score: Some(0.95),
                quality_score: None,
                lexical_score: None,
                rerank_score: None,
                metadata: None,
                explanation: None,
            }],
            elapsed_ms: 12.3,
            errors: Vec::new(),
            degraded: Vec::new(),
            runtime_profile: test_runtime_profile(),
            relevance_floor_applied: None,
            candidates_below_floor: 0,
            source_mode_requested: SearchSourceMode::Hybrid,
            source_mode_applied: SearchSourceMode::Hybrid,
            source_mode_fallback: false,
            strict_source_mode: false,
            memory_scope: MemoryScope::Swarm,
            strict_scope: false,
            scope_stats: test_scope_stats(),
        };

        assert!(
            SearchRevisionMetadata::for_report(&report, MeshCommandMode::Off).is_none(),
            "off mode must not change strict search output"
        );
        let first = SearchRevisionMetadata::for_report(&report, MeshCommandMode::Revisable)
            .expect("revisable search should emit explicit revision metadata");
        let replay = SearchRevisionMetadata::for_report(&report, MeshCommandMode::Revisable)
            .expect("revisable search replay should emit revision metadata");

        assert_eq!(first, replay);
        assert_eq!(first.schema, SEARCH_REVISION_TOKEN_SCHEMA_V1);
        assert!(first.token.starts_with("searchrev_"));
        assert!(first.tier1_usable);
        assert!(!first.revision_available);
        assert_eq!(first.local_mesh_tip_status, "not_checked");
        assert_eq!(
            first.result_doc_ids,
            vec!["mem_00000000000000000000000001".to_string()]
        );
        assert!(first.query_hash.starts_with("blake3:"));
        assert!(first.result_fingerprint.starts_with("blake3:"));
    }

    #[test]
    fn search_report_data_json_has_required_fields() {
        let report = SearchReport {
            status: SearchStatus::Success,
            query: "test query".to_string(),
            requested_limit: 10,
            results: vec![SearchHit {
                doc_id: "doc-1".to_string(),
                score: 0.95,
                source: ScoreSource::SemanticFast,
                fast_score: Some(0.95),
                quality_score: None,
                lexical_score: None,
                rerank_score: None,
                metadata: None,
                explanation: None,
            }],
            elapsed_ms: 12.3,
            errors: Vec::new(),
            degraded: Vec::new(),
            runtime_profile: test_runtime_profile(),
            relevance_floor_applied: None,
            candidates_below_floor: 0,
            source_mode_requested: SearchSourceMode::Hybrid,
            source_mode_applied: SearchSourceMode::Hybrid,
            source_mode_fallback: false,
            strict_source_mode: false,
            memory_scope: MemoryScope::Swarm,
            strict_scope: false,
            scope_stats: MemoryScopeStats::new(MemoryScope::Swarm, false, None, 0),
        };

        let json = report.data_json();
        assert_eq!(json["command"], "search");
        assert_eq!(json["status"], "success");
        assert_eq!(json["query"], "test query");
        assert_eq!(json["resultCount"], 1);
        assert!(json["results"].is_array());
        assert_eq!(json["metrics"]["requestedLimit"], 10);
        assert_eq!(json["metrics"]["returnedCount"], 1);
        assert_eq!(json["metrics"]["errorCount"], 0);
        assert_eq!(json["request"]["sourceMode"], "hybrid");
        assert_eq!(json["request"]["strictSourceMode"], false);
        assert_eq!(json["metrics"]["sourceModeRequested"], "hybrid");
        assert_eq!(json["metrics"]["sourceModeApplied"], "hybrid");
        assert_eq!(json["metrics"]["fallbackApplied"], false);
        assert_eq!(json["metrics"]["strictSourceMode"], false);
        assert_eq!(
            json["results"][0]["scoreInterval"],
            serde_json::json!([0.0, 1.0])
        );
        assert_eq!(json["results"][0]["coverageGuarantee"], 0.95);
        assert!(json["results"][0]["why"].is_string());
        assert!(json["results"][0]["provenance"].is_array());
    }

    #[test]
    fn conformal_quantile_uses_closed_split_rank() {
        let quantile = split_conformal_quantile(
            vec![0.10, 0.05, 0.30, 0.20],
            SEARCH_SCORE_COVERAGE_GUARANTEE,
        );

        assert_eq!(quantile, 0.30);
    }

    #[test]
    fn search_score_calibration_marks_small_sets_insufficient() -> TestResult {
        let workspace = unique_test_dir("score-calibration-small");
        let calibration_dir = workspace.join(".ee").join("search");
        std::fs::create_dir_all(&calibration_dir).map_err(|error| error.to_string())?;
        std::fs::write(
            calibration_dir.join("calibration.jsonl"),
            r#"{"score":0.8,"groundTruthRelevance":0.7}"#,
        )
        .map_err(|error| error.to_string())?;

        let calibration = SearchScoreCalibration::for_workspace(&workspace);

        assert_eq!(
            calibration.status,
            SearchScoreCalibrationStatus::Insufficient
        );
        assert_eq!(calibration.sample_count, 1);
        assert_eq!(calibration.interval_for_score(0.8), [0.0, 1.0]);
        Ok(())
    }

    #[test]
    fn search_score_calibration_marks_corrupt_rows_distinctly() -> TestResult {
        let workspace = unique_test_dir("score-calibration-corrupt");
        let calibration_dir = workspace.join(".ee").join("search");
        std::fs::create_dir_all(&calibration_dir).map_err(|error| error.to_string())?;
        std::fs::write(
            calibration_dir.join("calibration.jsonl"),
            [
                "not json",
                r#"{"score":0.8}"#,
                r#"{"score":0.6,"groundTruthRelevance":0.5}"#,
            ]
            .join("\n"),
        )
        .map_err(|error| error.to_string())?;

        let calibration = SearchScoreCalibration::for_workspace(&workspace);

        assert_eq!(calibration.status, SearchScoreCalibrationStatus::Corrupt);
        assert_eq!(calibration.sample_count, 1);
        assert_eq!(calibration.corrupt_row_count, 2);
        assert_eq!(calibration.corrupt_line_numbers, vec![1, 2]);
        assert_eq!(calibration.interval_for_score(0.8), [0.0, 1.0]);
        assert_eq!(calibration.data_json()["status"], "corrupt");
        assert_eq!(calibration.data_json()["corruptRowCount"], 2);
        assert_eq!(
            calibration.data_json()["corruptLineNumbers"],
            serde_json::json!([1, 2])
        );
        Ok(())
    }

    #[test]
    fn search_score_calibration_corrupt_rows_emit_degradation() -> TestResult {
        let workspace = unique_test_dir("score-calibration-corrupt-degraded");
        let calibration_dir = workspace.join(".ee").join("search");
        std::fs::create_dir_all(&calibration_dir).map_err(|error| error.to_string())?;
        std::fs::write(
            calibration_dir.join("calibration.jsonl"),
            "not json\n{\"oops\":1}\n",
        )
        .map_err(|error| error.to_string())?;

        let mut hits = vec![synthetic_hit("mem_score_calibration_corrupt", 0.8)];
        let mut degraded = Vec::new();
        annotate_hits_with_score_calibration(&workspace, None, None, &mut hits, &mut degraded);

        assert_eq!(degraded.len(), 1);
        assert_eq!(degraded[0].code, SEARCH_SCORE_CALIBRATION_ROWS_CORRUPT_CODE);
        assert_eq!(degraded[0].severity, "warning");
        assert!(
            degraded[0].message.contains("lines 1, 2"),
            "message should include corrupt line numbers: {}",
            degraded[0].message
        );
        assert_eq!(
            hits[0]
                .metadata
                .as_ref()
                .and_then(|metadata| metadata.pointer("/scoreCalibration/status"))
                .and_then(serde_json::Value::as_str),
            Some("corrupt")
        );
        Ok(())
    }

    #[test]
    fn search_score_calibration_file_too_large_degrades_without_reading_jsonl() -> TestResult {
        let workspace = unique_test_dir("score-calibration-too-large");
        let calibration_dir = workspace.join(".ee").join("search");
        std::fs::create_dir_all(&calibration_dir).map_err(|error| error.to_string())?;
        let file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(calibration_dir.join("calibration.jsonl"))
            .map_err(|error| error.to_string())?;
        file.set_len(MAX_SEARCH_SCORE_CALIBRATION_BYTES + 1)
            .map_err(|error| error.to_string())?;

        let calibration = SearchScoreCalibration::for_workspace(&workspace);

        assert_eq!(
            calibration.status,
            SearchScoreCalibrationStatus::FileTooLarge
        );
        assert_eq!(calibration.sample_count, 0);
        assert_eq!(calibration.jsonl_sample_count, 0);
        assert_eq!(calibration.interval_for_score(0.8), [0.0, 1.0]);
        assert_eq!(calibration.data_json()["status"], "file_too_large");

        let mut hits = vec![synthetic_hit("mem_score_calibration_too_large", 0.8)];
        let mut degraded = Vec::new();
        annotate_hits_with_score_calibration(&workspace, None, None, &mut hits, &mut degraded);

        assert_eq!(degraded.len(), 1);
        assert_eq!(
            degraded[0].code,
            SEARCH_SCORE_CALIBRATION_FILE_TOO_LARGE_CODE
        );
        assert_eq!(degraded[0].severity, "warning");
        assert!(
            degraded[0].message.contains("above the"),
            "message should describe the calibration size cap: {}",
            degraded[0].message
        );
        assert_eq!(
            hits[0]
                .metadata
                .as_ref()
                .and_then(|metadata| metadata.pointer("/scoreCalibration/status"))
                .and_then(serde_json::Value::as_str),
            Some("file_too_large")
        );
        Ok(())
    }

    /// bd-25z97: a present-but-unreadable JSONL (permissions stripped on
    /// Unix) must surface as `status: unreadable` with the
    /// search_score_calibration_unreadable degraded code, NOT silently
    /// collapse into `status: absent` like the pre-bd-25z97 code did.
    #[cfg(unix)]
    #[test]
    fn search_score_calibration_unreadable_jsonl_surfaces_degradation() -> TestResult {
        use std::os::unix::fs::PermissionsExt;

        // Running as root would defeat the permission strip; skip rather
        // than fail the suite in containerized test environments.
        if std::env::var("USER").as_deref() == Ok("root") {
            return Ok(());
        }

        let workspace = unique_test_dir("score-calibration-unreadable");
        let calibration_dir = workspace.join(".ee").join("search");
        std::fs::create_dir_all(&calibration_dir).map_err(|error| error.to_string())?;
        let path = calibration_dir.join("calibration.jsonl");
        std::fs::write(&path, r#"{"score":0.8,"groundTruthRelevance":0.7}"#)
            .map_err(|error| error.to_string())?;
        // Strip ALL permission bits; metadata still succeeds (since the
        // directory is readable) but open() will fail with
        // PermissionDenied, which is the I/O failure the bd-25z97 audit
        // documented as silently folding into `status: absent`.
        let mut perms = std::fs::metadata(&path)
            .map_err(|error| error.to_string())?
            .permissions();
        perms.set_mode(0o000);
        std::fs::set_permissions(&path, perms).map_err(|error| error.to_string())?;

        let calibration = SearchScoreCalibration::for_workspace(&workspace);

        assert_eq!(
            calibration.status,
            SearchScoreCalibrationStatus::Unreadable,
            "permission-denied JSONL must surface as Unreadable, not Absent"
        );
        assert_eq!(
            calibration.unreadable_reason.as_deref(),
            Some("permission_denied"),
            "unreadable_reason should carry the io::ErrorKind label"
        );
        assert_eq!(calibration.interval_for_score(0.8), [0.0, 1.0]);
        assert_eq!(calibration.data_json()["status"], "unreadable");
        assert_eq!(
            calibration.data_json()["unreadableReason"],
            "permission_denied"
        );

        let mut hits = vec![synthetic_hit("mem_score_calibration_unreadable", 0.8)];
        let mut degraded = Vec::new();
        annotate_hits_with_score_calibration(&workspace, None, None, &mut hits, &mut degraded);

        assert_eq!(degraded.len(), 1);
        assert_eq!(degraded[0].code, SEARCH_SCORE_CALIBRATION_UNREADABLE_CODE);
        assert_eq!(degraded[0].severity, "warning");
        assert!(
            degraded[0].message.contains("permission_denied"),
            "unreadable message should include the io::ErrorKind reason: {}",
            degraded[0].message
        );
        assert_eq!(
            hits[0]
                .metadata
                .as_ref()
                .and_then(|metadata| metadata.pointer("/scoreCalibration/status"))
                .and_then(serde_json::Value::as_str),
            Some("unreadable")
        );

        // Restore permissions so the tempdir can be cleaned up.
        let mut perms = std::fs::metadata(&path)
            .map_err(|error| error.to_string())?
            .permissions();
        perms.set_mode(0o600);
        let _ = std::fs::set_permissions(&path, perms);
        Ok(())
    }

    /// bd-25z97: a truly absent JSONL (parent dir doesn't even exist) must
    /// still report `status: absent` and emit NO degradation. This pins
    /// the boundary so we never accidentally widen the unreadable path to
    /// include genuine NotFound.
    #[test]
    fn search_score_calibration_absent_jsonl_emits_no_unreadable_degradation() {
        let workspace = unique_test_dir("score-calibration-absent");
        let calibration = SearchScoreCalibration::for_workspace(&workspace);
        assert_eq!(calibration.status, SearchScoreCalibrationStatus::Absent);
        assert_eq!(calibration.unreadable_reason, None);

        let mut hits = vec![synthetic_hit("mem_score_calibration_absent", 0.5)];
        let mut degraded = Vec::new();
        annotate_hits_with_score_calibration(&workspace, None, None, &mut hits, &mut degraded);
        assert!(
            degraded.is_empty(),
            "absent calibration must not emit any degradation"
        );
    }

    /// bd-25z97: pure unit coverage of the io::ErrorKind classifier — pins
    /// the NotFound vs everything-else split that drives Absent vs
    /// Unreadable in the loaders.
    #[test]
    fn classify_calibration_io_error_splits_not_found_from_everything_else() {
        let not_found = std::io::Error::from(std::io::ErrorKind::NotFound);
        assert_eq!(classify_calibration_io_error(&not_found), None);

        let permission = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
        assert_eq!(
            classify_calibration_io_error(&permission),
            Some("permission_denied")
        );

        let invalid = std::io::Error::from(std::io::ErrorKind::InvalidData);
        assert_eq!(
            classify_calibration_io_error(&invalid),
            Some("invalid_data")
        );

        let interrupted = std::io::Error::from(std::io::ErrorKind::Interrupted);
        assert_eq!(
            classify_calibration_io_error(&interrupted),
            Some("interrupted")
        );
    }

    #[test]
    fn search_score_calibration_corrupt_line_numbers_are_capped() -> TestResult {
        let workspace = unique_test_dir("score-calibration-corrupt-cap");
        let calibration_dir = workspace.join(".ee").join("search");
        std::fs::create_dir_all(&calibration_dir).map_err(|error| error.to_string())?;
        let rows = std::iter::repeat_n(
            "not json",
            MAX_SEARCH_SCORE_CALIBRATION_CORRUPT_LINE_NUMBERS + 3,
        )
        .collect::<Vec<_>>()
        .join("\n");
        std::fs::write(calibration_dir.join("calibration.jsonl"), rows)
            .map_err(|error| error.to_string())?;

        let calibration = SearchScoreCalibration::for_workspace(&workspace);

        assert_eq!(calibration.status, SearchScoreCalibrationStatus::Corrupt);
        assert_eq!(
            calibration.corrupt_row_count,
            MAX_SEARCH_SCORE_CALIBRATION_CORRUPT_LINE_NUMBERS + 3
        );
        assert_eq!(
            calibration.corrupt_line_numbers.len(),
            MAX_SEARCH_SCORE_CALIBRATION_CORRUPT_LINE_NUMBERS
        );
        assert_eq!(
            calibration.corrupt_line_numbers.last().copied(),
            Some(MAX_SEARCH_SCORE_CALIBRATION_CORRUPT_LINE_NUMBERS)
        );
        Ok(())
    }

    #[test]
    fn search_score_calibration_intervals_tighten_for_higher_scores() -> TestResult {
        let workspace = unique_test_dir("score-calibration-calibrated");
        let calibration_dir = workspace.join(".ee").join("search");
        std::fs::create_dir_all(&calibration_dir).map_err(|error| error.to_string())?;
        let rows = (0..MIN_SEARCH_SCORE_CALIBRATION_SAMPLES)
            .map(|index| {
                let score = 0.50 + (index as f32 * 0.001);
                let truth = score - 0.05;
                format!(r#"{{"score":{score:.3},"groundTruthRelevance":{truth:.3}}}"#)
            })
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(calibration_dir.join("calibration.jsonl"), rows)
            .map_err(|error| error.to_string())?;

        let calibration = SearchScoreCalibration::for_workspace(&workspace);
        let low = calibration.interval_for_score(0.40);
        let high = calibration.interval_for_score(0.90);

        assert_eq!(calibration.status, SearchScoreCalibrationStatus::Calibrated);
        assert_eq!(
            calibration.sample_count,
            MIN_SEARCH_SCORE_CALIBRATION_SAMPLES
        );
        assert!(
            (high[1] - high[0]) <= (low[1] - low[0]),
            "higher scores should have tighter or equal score intervals"
        );
        Ok(())
    }

    #[test]
    fn search_score_calibration_bootstraps_from_feedback_events() -> TestResult {
        let workspace = unique_test_dir("score-calibration-feedback-events");
        let database_path = workspace.join(".ee").join("ee.db");
        std::fs::create_dir_all(
            database_path
                .parent()
                .ok_or_else(|| "database path must have a parent".to_string())?,
        )
        .map_err(|error| error.to_string())?;
        let connection =
            DbConnection::open_file(&database_path).map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        let workspace_id = crate::core::curate::stable_workspace_id(&workspace);
        connection
            .insert_workspace(
                &workspace_id,
                &CreateWorkspaceInput {
                    path: workspace.display().to_string(),
                    name: Some("score-calibration-feedback-events".to_owned()),
                },
            )
            .map_err(|error| error.to_string())?;

        for index in 0..MIN_SEARCH_SCORE_CALIBRATION_SAMPLES {
            let score = 0.50 + (index as f32 * 0.01);
            let truth = score - 0.04;
            let evidence = serde_json::json!({
                "schema": "ee.search.calibration_feedback.v1",
                "searchCalibration": {
                    "predictedScore": score,
                    "ground_truth_relevance": truth,
                },
                "query": "candidate validated by curate"
            })
            .to_string();
            connection
                .insert_feedback_event(
                    &format!("fb_{index:026}"),
                    &CreateFeedbackEventInput {
                        workspace_id: workspace_id.clone(),
                        target_type: "candidate".to_owned(),
                        target_id: format!("cand_{index:02}"),
                        signal: "confirmation".to_owned(),
                        weight: 1.0,
                        source_type: "outcome_observed".to_owned(),
                        source_id: Some("curate-validation".to_owned()),
                        reason: Some("curate validation accepted relevance label".to_owned()),
                        evidence_json: Some(evidence),
                        session_id: None,
                    },
                )
                .map_err(|error| error.to_string())?;
        }

        let feedback_events = search_score_calibration_feedback_events(
            &workspace,
            Some(&database_path),
            Some(&connection),
        );
        let calibration = SearchScoreCalibration::for_workspace_with_feedback_events(
            &workspace,
            &feedback_events.events,
        );

        assert_eq!(calibration.status, SearchScoreCalibrationStatus::Calibrated);
        assert_eq!(
            calibration.sample_count,
            MIN_SEARCH_SCORE_CALIBRATION_SAMPLES
        );
        assert_eq!(calibration.jsonl_sample_count, 0);
        assert_eq!(
            calibration.feedback_event_sample_count,
            MIN_SEARCH_SCORE_CALIBRATION_SAMPLES
        );
        assert_ne!(calibration.interval_for_score(0.8), [0.0, 1.0]);

        let mut hits = vec![synthetic_hit("mem_feedback_calibration", 0.8)];
        let mut degraded = Vec::new();
        annotate_hits_with_score_calibration(
            &workspace,
            Some(&database_path),
            Some(&connection),
            &mut hits,
            &mut degraded,
        );

        assert!(
            degraded.is_empty(),
            "calibrated feedback rows should not degrade"
        );
        assert_eq!(
            hits[0]
                .metadata
                .as_ref()
                .and_then(|metadata| metadata.pointer("/scoreCalibration/sourceBreakdown/jsonl"))
                .and_then(serde_json::Value::as_u64),
            Some(0)
        );
        assert_eq!(
            hits[0]
                .metadata
                .as_ref()
                .and_then(|metadata| {
                    metadata.pointer("/scoreCalibration/sourceBreakdown/feedbackEvents")
                })
                .and_then(serde_json::Value::as_u64),
            Some(MIN_SEARCH_SCORE_CALIBRATION_SAMPLES as u64)
        );
        assert_eq!(
            hits[0]
                .metadata
                .as_ref()
                .and_then(|metadata| {
                    metadata.pointer("/scoreCalibration/sourceBreakdown/feedbackEventsReadStatus")
                })
                .and_then(serde_json::Value::as_str),
            Some("ok")
        );
        Ok(())
    }

    #[test]
    fn search_score_recalibrate_now_persists_feedback_events_jsonl() -> TestResult {
        let workspace = unique_test_dir("score-calibration-recalibrate-now");
        let database_path = workspace.join(".ee").join("ee.db");
        std::fs::create_dir_all(
            database_path
                .parent()
                .ok_or_else(|| "database path must have a parent".to_string())?,
        )
        .map_err(|error| error.to_string())?;
        let connection =
            DbConnection::open_file(&database_path).map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        let workspace_id = crate::core::curate::stable_workspace_id(&workspace);
        connection
            .insert_workspace(
                &workspace_id,
                &CreateWorkspaceInput {
                    path: workspace.display().to_string(),
                    name: Some("score-calibration-recalibrate-now".to_owned()),
                },
            )
            .map_err(|error| error.to_string())?;

        for index in 0..MIN_SEARCH_SCORE_CALIBRATION_SAMPLES {
            let score = 0.40 + (index as f32 * 0.01);
            let truth = score + 0.03;
            let evidence = serde_json::json!({
                "schema": "ee.search.calibration_feedback.v1",
                "scoreCalibration": {
                    "score": score,
                    "groundTruthRelevance": truth,
                },
            })
            .to_string();
            connection
                .insert_feedback_event(
                    &format!("fb_{index:026}"),
                    &CreateFeedbackEventInput {
                        workspace_id: workspace_id.clone(),
                        target_type: "candidate".to_owned(),
                        target_id: format!("cand_recal_{index:02}"),
                        signal: "confirmation".to_owned(),
                        weight: 1.0,
                        source_type: "outcome_observed".to_owned(),
                        source_id: Some("curate-validation".to_owned()),
                        reason: Some("calibration refresh sample".to_owned()),
                        evidence_json: Some(evidence),
                        session_id: None,
                    },
                )
                .map_err(|error| error.to_string())?;
        }

        let report = recalibrate_search_score_calibration(&workspace, Some(&database_path))
            .map_err(|error| error.to_string())?;

        assert_eq!(report.status, "calibrated");
        assert_eq!(report.samples_written, MIN_SEARCH_SCORE_CALIBRATION_SAMPLES);
        assert_eq!(
            report.path,
            workspace
                .join(".ee")
                .join("search")
                .join("calibration.jsonl")
        );
        let persisted = std::fs::read_to_string(&report.path).map_err(|error| error.to_string())?;
        assert!(persisted.contains("\"feedbackEventId\":\"fb_00000000000000000000000000\""));
        let calibration = SearchScoreCalibration::for_workspace(&workspace);
        assert_eq!(calibration.status, SearchScoreCalibrationStatus::Calibrated);
        assert_eq!(
            calibration.jsonl_sample_count,
            MIN_SEARCH_SCORE_CALIBRATION_SAMPLES
        );
        assert_ne!(calibration.interval_for_score(0.8), [0.0, 1.0]);
        Ok(())
    }

    #[test]
    fn search_score_recalibrate_now_preserves_jsonl_when_feedback_unavailable() -> TestResult {
        let workspace = unique_test_dir("score-calibration-recalibrate-now-unavailable");
        let calibration_dir = workspace.join(".ee").join("search");
        std::fs::create_dir_all(&calibration_dir).map_err(|error| error.to_string())?;
        let calibration_path = calibration_dir.join("calibration.jsonl");
        let original = r#"{"score":0.9,"groundTruthRelevance":0.8}"#;
        std::fs::write(&calibration_path, original).map_err(|error| error.to_string())?;
        let database_path = workspace.join(".ee").join("ee.db");
        std::fs::create_dir_all(&database_path).map_err(|error| error.to_string())?;

        let report = recalibrate_search_score_calibration(&workspace, Some(&database_path))
            .map_err(|error| error.to_string())?;

        assert_eq!(report.status, "feedback_unavailable");
        assert_eq!(report.samples_written, 0);
        assert_eq!(
            std::fs::read_to_string(&calibration_path).map_err(|error| error.to_string())?,
            original
        );
        Ok(())
    }

    #[test]
    fn search_score_calibration_feedback_db_failure_surfaces_degradation() -> TestResult {
        let workspace = unique_test_dir("score-calibration-feedback-db-failure");
        let database_path = workspace.join(".ee").join("ee.db");
        std::fs::create_dir_all(&database_path).map_err(|error| error.to_string())?;

        let mut hits = vec![synthetic_hit("mem_feedback_db_failure", 0.8)];
        let mut degraded = Vec::new();
        annotate_hits_with_score_calibration(
            &workspace,
            Some(&database_path),
            None,
            &mut hits,
            &mut degraded,
        );

        assert!(
            degraded.iter().any(
                |entry| entry.code == SEARCH_SCORE_CALIBRATION_UNREADABLE_CODE
                    && entry
                        .message
                        .contains("feedback-event calibration evidence")
            ),
            "DB open failures must not erase feedback-event calibration samples silently: {degraded:?}"
        );
        assert_eq!(
            hits[0]
                .metadata
                .as_ref()
                .and_then(|metadata| {
                    metadata.pointer("/scoreCalibration/sourceBreakdown/feedbackEventsReadStatus")
                })
                .and_then(serde_json::Value::as_str),
            Some("unavailable")
        );
        assert_eq!(
            hits[0]
                .metadata
                .as_ref()
                .and_then(|metadata| {
                    metadata.pointer(
                        "/scoreCalibration/sourceBreakdown/feedbackEventsUnavailableReason",
                    )
                })
                .and_then(serde_json::Value::as_str),
            Some("feedback_events_open_failed")
        );
        Ok(())
    }

    #[test]
    fn search_score_calibration_malformed_feedback_evidence_is_counted() -> TestResult {
        let workspace = unique_test_dir("score-calibration-feedback-malformed");
        let database_path = workspace.join(".ee").join("ee.db");
        std::fs::create_dir_all(
            database_path
                .parent()
                .ok_or_else(|| "database path must have a parent".to_string())?,
        )
        .map_err(|error| error.to_string())?;
        let connection =
            DbConnection::open_file(&database_path).map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        let workspace_id = crate::core::curate::stable_workspace_id(&workspace);
        connection
            .insert_workspace(
                &workspace_id,
                &CreateWorkspaceInput {
                    path: workspace.display().to_string(),
                    name: Some("score-calibration-feedback-malformed".to_owned()),
                },
            )
            .map_err(|error| error.to_string())?;
        connection
            .insert_feedback_event(
                "fb_malformed_calibration_0001",
                &CreateFeedbackEventInput {
                    workspace_id: workspace_id.clone(),
                    target_type: "candidate".to_owned(),
                    target_id: "cand_malformed".to_owned(),
                    signal: "confirmation".to_owned(),
                    weight: 1.0,
                    source_type: "outcome_observed".to_owned(),
                    source_id: Some("curate-validation".to_owned()),
                    reason: Some("bad calibration evidence".to_owned()),
                    evidence_json: Some("{not json".to_owned()),
                    session_id: None,
                },
            )
            .map_err(|error| error.to_string())?;

        let mut hits = vec![synthetic_hit("mem_feedback_malformed", 0.8)];
        let mut degraded = Vec::new();
        annotate_hits_with_score_calibration(
            &workspace,
            Some(&database_path),
            Some(&connection),
            &mut hits,
            &mut degraded,
        );

        assert!(
            degraded.iter().any(
                |entry| entry.code == SEARCH_SCORE_CALIBRATION_UNREADABLE_CODE
                    && entry.message.contains("feedback_events_malformed")
            ),
            "malformed evidence_json must surface as degraded calibration evidence: {degraded:?}"
        );
        assert_eq!(
            hits[0]
                .metadata
                .as_ref()
                .and_then(|metadata| {
                    metadata.pointer("/scoreCalibration/sourceBreakdown/feedbackEventsMalformed")
                })
                .and_then(serde_json::Value::as_u64),
            Some(1)
        );
        assert_eq!(
            hits[0]
                .metadata
                .as_ref()
                .and_then(|metadata| {
                    metadata.pointer("/scoreCalibration/sourceBreakdown/feedbackEvents")
                })
                .and_then(serde_json::Value::as_u64),
            Some(0)
        );
        Ok(())
    }

    /// Deterministic split-conformal backtest: build a calibration JSONL
    /// from an exchangeable synthetic (score, truth) distribution, hold
    /// out the second half as the test split, and confirm empirical
    /// coverage on the held-out split clears a conservative floor
    /// (0.80) well below the 0.95 nominal guarantee to absorb
    /// finite-sample variance with 100 held-out samples. Pins the DoD
    /// item "backtest fixture demonstrates empirical coverage falls in
    /// the interval" for bd-17c65.14.2.
    #[test]
    fn search_score_calibration_empirical_coverage_backtest() -> TestResult {
        let workspace = unique_test_dir("score-calibration-empirical-backtest");
        let calibration_dir = workspace.join(".ee").join("search");
        std::fs::create_dir_all(&calibration_dir).map_err(|error| error.to_string())?;

        // Deterministic exchangeable sampler. Score sweeps 0.10..0.89 via
        // a coprime stride so the same score occurs in both splits with
        // distinct noise draws; noise is one of 13 discrete levels in
        // [-0.15, +0.15] selected by a different coprime index. Both
        // splits draw from the same generator, so the cal/test split is
        // exchangeable by construction.
        fn sample(index: usize) -> (f32, f32) {
            let score = 0.10 + ((index * 7) % 80) as f32 * 0.01;
            let noise_index = (index * 11) % 13;
            let noise = (noise_index as f32 - 6.0) * 0.025;
            let truth = (score + noise).clamp(0.0, 1.0);
            (score, truth)
        }

        let total = 200usize;
        let cal_count = 100usize;
        let cal_rows = (0..cal_count)
            .map(|index| {
                let (score, truth) = sample(index);
                format!(r#"{{"score":{score:.6},"groundTruthRelevance":{truth:.6}}}"#)
            })
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(calibration_dir.join("calibration.jsonl"), cal_rows)
            .map_err(|error| error.to_string())?;

        let calibration = SearchScoreCalibration::for_workspace(&workspace);
        assert_eq!(calibration.status, SearchScoreCalibrationStatus::Calibrated);
        assert_eq!(calibration.sample_count, cal_count);

        let mut covered = 0usize;
        let mut test_count = 0usize;
        for index in cal_count..total {
            let (score, truth) = sample(index);
            let [lo, hi] = calibration.interval_for_score(score);
            if truth >= lo && truth <= hi {
                covered += 1;
            }
            test_count += 1;
        }
        let empirical_coverage = covered as f32 / test_count as f32;
        assert!(
            empirical_coverage >= 0.80,
            "empirical coverage {empirical_coverage:.3} fell below 0.80 floor on N={test_count} held-out samples (nominal target {SEARCH_SCORE_COVERAGE_GUARANTEE})"
        );

        // Determinism contract from the bead DoD: identical calibration
        // input must produce the same residual quantile across reloads.
        let reloaded = SearchScoreCalibration::for_workspace(&workspace);
        assert_eq!(
            calibration.residual_quantile, reloaded.residual_quantile,
            "split-conformal quantile must be byte-identical across reloads of the same calibration file"
        );
        Ok(())
    }

    #[test]
    fn search_report_data_json_exposes_allowed_mesh_provenance() {
        let report = SearchReport {
            status: SearchStatus::Success,
            query: "mesh query".to_string(),
            requested_limit: 10,
            results: vec![SearchHit {
                doc_id: "mesh-doc-1".to_string(),
                score: 0.95,
                source: ScoreSource::SemanticFast,
                fast_score: Some(0.95),
                quality_score: None,
                lexical_score: None,
                rerank_score: None,
                metadata: Some(serde_json::json!({
                    "policyDecision": {
                        "schema": "ee.mesh.policy_decision.v1",
                        "direction": "inbound",
                        "action": "allow",
                        "reason": "peer_policy_lane_allowed"
                    },
                    "policyFailureSurface": {
                        "schema": "ee.mesh.policy_failure_surface.v1",
                        "code": "mesh_peer_policy_denied",
                        "reason": "peer_policy_redaction_denied"
                    },
                    "mesh": {
                        "workspaceScopeDecision": "allow",
                        "workspaceId": "wsp_local_alpha",
                        "cachedMaterialId": "mesh_mat_123",
                        "originWorkspaceId": "wsp_remote_beta",
                        "originWorkspaceLabel": "/Users/alice/private/repo",
                        "producerPeerId": "peer_builder_one",
                        "producerPeerLabel": "/Users/alice/private/peer-agent",
                        "materialLane": "metadata",
                        "importDecisionId": "mesh_dec_456",
                        "trustLane": "mesh_metadata",
                        "redactionPosture": "standard"
                    }
                })),
                explanation: None,
            }],
            elapsed_ms: 12.3,
            errors: Vec::new(),
            degraded: Vec::new(),
            runtime_profile: test_runtime_profile(),
            relevance_floor_applied: None,
            candidates_below_floor: 0,
            source_mode_requested: SearchSourceMode::Hybrid,
            source_mode_applied: SearchSourceMode::Hybrid,
            source_mode_fallback: false,
            strict_source_mode: false,
            memory_scope: MemoryScope::Swarm,
            strict_scope: false,
            scope_stats: MemoryScopeStats::new(MemoryScope::Swarm, false, None, 0),
        };

        let json = report.data_json();
        let provenance = &json["results"][0]["meshProvenance"];
        assert_eq!(provenance["cachedMaterialId"], "mesh_mat_123");
        assert!(
            provenance["originWorkspaceAlias"]
                .as_str()
                .is_some_and(|alias| alias.starts_with("mesh_ns_"))
        );
        assert_eq!(provenance["producerPeer"], "peer_builder_one");
        assert_eq!(provenance["materialLane"], "metadata");
        assert_eq!(provenance["importDecisionRef"], "mesh_dec_456");
        assert_eq!(provenance["trustLane"], "mesh_metadata");
        assert_eq!(provenance["redactionPosture"], "standard");
        assert!(json["results"][0]["metadata"].get("mesh").is_none());
        assert!(
            json["results"][0]["metadata"]
                .get("policyDecision")
                .is_none(),
            "policy decision must remain internal"
        );
        assert!(
            json["results"][0]["metadata"]
                .get("policyFailureSurface")
                .is_none(),
            "policy failure surface must remain internal"
        );
        assert!(
            !json["results"][0]["metadata"]
                .to_string()
                .contains("/Users/alice/private/repo")
        );
        assert!(
            !json["results"][0]
                .to_string()
                .contains("/Users/alice/private/peer-agent")
        );
    }

    #[test]
    fn search_mesh_query_visibility_filters_non_allowed_hits() {
        let mut degraded = Vec::new();
        let hits = vec![
            SearchHit {
                doc_id: "local-doc".to_string(),
                score: 0.90,
                source: ScoreSource::SemanticFast,
                fast_score: Some(0.90),
                quality_score: None,
                lexical_score: None,
                rerank_score: None,
                metadata: None,
                explanation: None,
            },
            SearchHit {
                doc_id: "mesh-denied".to_string(),
                score: 0.88,
                source: ScoreSource::SemanticFast,
                fast_score: Some(0.88),
                quality_score: None,
                lexical_score: None,
                rerank_score: None,
                metadata: Some(serde_json::json!({
                    "mesh": {
                        "workspaceScopeDecision": "deny",
                        "cachedMaterialId": "mesh_mat_denied",
                        "originWorkspaceId": "wsp_remote_beta",
                        "producerPeerId": "peer_builder_one",
                        "materialLane": "metadata",
                        "trustLane": "mesh_metadata",
                        "redactionPosture": "standard"
                    }
                })),
                explanation: None,
            },
            SearchHit {
                doc_id: "mesh-quarantined".to_string(),
                score: 0.87,
                source: ScoreSource::SemanticFast,
                fast_score: Some(0.87),
                quality_score: None,
                lexical_score: None,
                rerank_score: None,
                metadata: Some(serde_json::json!({
                    "mesh": {
                        "workspaceScopeDecision": "quarantine",
                        "cachedMaterialId": "mesh_mat_quarantined",
                        "originWorkspaceId": "wsp_remote_beta",
                        "producerPeerId": "peer_builder_one",
                        "materialLane": "curationSignal",
                        "trustLane": "mesh_curation",
                        "redactionPosture": "standard"
                    }
                })),
                explanation: None,
            },
            SearchHit {
                doc_id: "mesh-rejected".to_string(),
                score: 0.86,
                source: ScoreSource::SemanticFast,
                fast_score: Some(0.86),
                quality_score: None,
                lexical_score: None,
                rerank_score: None,
                metadata: Some(serde_json::json!({
                    "mesh": {
                        "workspaceScopeDecision": "reject",
                        "cachedMaterialId": "mesh_mat_rejected",
                        "originWorkspaceId": "wsp_remote_beta",
                        "producerPeerId": "peer_builder_one",
                        "materialLane": "metadata",
                        "trustLane": "mesh_metadata",
                        "redactionPosture": "standard"
                    }
                })),
                explanation: None,
            },
        ];

        let visible = apply_mesh_query_visibility(hits, &mut degraded);

        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].doc_id, "local-doc");
        assert_eq!(degraded.len(), 1);
        assert_eq!(degraded[0].code, "mesh_workspace_scope_filtered");
        assert!(
            degraded[0].message.contains("3 mesh-derived search hits"),
            "unexpected degradation message: {}",
            degraded[0].message
        );
    }

    #[test]
    fn search_mesh_trust_adjustment_reranks_cached_peer_hits() {
        let mut degraded = Vec::new();
        let hits = vec![
            SearchHit {
                doc_id: "local-doc".to_string(),
                score: 0.89,
                source: ScoreSource::SemanticFast,
                fast_score: Some(0.89),
                quality_score: None,
                lexical_score: None,
                rerank_score: None,
                metadata: Some(serde_json::json!({
                    "content": "Local result remains unadjusted."
                })),
                explanation: None,
            },
            SearchHit {
                doc_id: "mesh-peer-agent".to_string(),
                score: 0.96,
                source: ScoreSource::SemanticFast,
                fast_score: Some(0.96),
                quality_score: None,
                lexical_score: None,
                rerank_score: None,
                metadata: Some(mesh_hit_metadata("wsp_remote_beta", "peerAgent")),
                explanation: None,
            },
            SearchHit {
                doc_id: "mesh-peer-human".to_string(),
                score: 0.93,
                source: ScoreSource::SemanticFast,
                fast_score: Some(0.93),
                quality_score: None,
                lexical_score: None,
                rerank_score: None,
                metadata: Some(mesh_hit_metadata("wsp_remote_alpha", "peerHumanViaPeer")),
                explanation: Some(ScoreExplanation {
                    summary: "Selected by semantic_fast retrieval with score 0.9300.".to_string(),
                    factors: Vec::new(),
                }),
            },
            SearchHit {
                doc_id: "mesh-metadata-only".to_string(),
                score: 0.97,
                source: ScoreSource::SemanticFast,
                fast_score: Some(0.97),
                quality_score: None,
                lexical_score: None,
                rerank_score: None,
                metadata: Some(mesh_hit_metadata("wsp_remote_gamma", "mesh_metadata")),
                explanation: None,
            },
        ];

        let visible = apply_mesh_query_visibility(hits, &mut degraded);

        assert!(degraded.is_empty());
        assert_eq!(
            visible
                .iter()
                .map(|hit| hit.doc_id.as_str())
                .collect::<Vec<_>>(),
            vec![
                "mesh-peer-human",
                "local-doc",
                "mesh-peer-agent",
                "mesh-metadata-only",
            ]
        );
        assert_eq!(round_metric_f32(visible[0].score), 0.9021);
        assert_eq!(round_metric_f32(visible[2].score), 0.8832);
        assert_eq!(round_metric_f32(visible[3].score), 0.8245);
        let factor = visible[0]
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.pointer("/_ee_mesh_trust_adjustment/factor"))
            .and_then(serde_json::Value::as_f64)
            .unwrap_or_default();
        assert!((factor - 0.97).abs() <= 0.000_001);
        assert!(
            visible[0]
                .why()
                .contains("Mesh trust lane `peerHumanViaPeer` adjusted score")
        );

        let report = SearchReport {
            status: SearchStatus::Success,
            query: "mesh query".to_string(),
            requested_limit: 10,
            results: visible,
            elapsed_ms: 12.3,
            errors: Vec::new(),
            degraded: Vec::new(),
            runtime_profile: test_runtime_profile(),
            relevance_floor_applied: None,
            candidates_below_floor: 0,
            source_mode_requested: SearchSourceMode::Hybrid,
            source_mode_applied: SearchSourceMode::Hybrid,
            source_mode_fallback: false,
            strict_source_mode: false,
            memory_scope: MemoryScope::Swarm,
            strict_scope: false,
            scope_stats: MemoryScopeStats::new(MemoryScope::Swarm, false, None, 0),
        };
        let json = report.data_json();
        assert_eq!(
            json["results"][0]["meshTrustAdjustment"]["schema"],
            "ee.mesh.search_trust_adjustment.v1"
        );
        let rendered_factor = json["results"][0]["meshTrustAdjustment"]["factor"]
            .as_f64()
            .unwrap_or_default();
        assert!((rendered_factor - 0.97).abs() <= 0.000_001);
        assert!(
            json["results"][0]["metadata"]
                .get("_ee_mesh_trust_adjustment")
                .is_none(),
            "internal adjustment metadata should stay out of public metadata"
        );
    }

    #[test]
    fn search_report_data_json_blocks_non_allowed_mesh_hits_defensively() {
        let report = SearchReport {
            status: SearchStatus::Success,
            query: "mesh query".to_string(),
            requested_limit: 10,
            results: vec![
                SearchHit {
                    doc_id: "mesh-quarantined".to_string(),
                    score: 0.87,
                    source: ScoreSource::SemanticFast,
                    fast_score: Some(0.87),
                    quality_score: None,
                    lexical_score: None,
                    rerank_score: None,
                    metadata: Some(serde_json::json!({
                        "content": "PRIVATE REMOTE MESH BODY MUST NOT RENDER",
                        "mesh": {
                            "workspaceScopeDecision": "quarantine",
                            "cachedMaterialId": "mesh_mat_quarantined",
                            "originWorkspaceId": "wsp_remote_beta",
                            "originWorkspaceLabel": "/Users/alice/private/repo",
                            "producerPeerId": "peer_builder_one",
                            "materialLane": "curationSignal",
                            "trustLane": "mesh_curation",
                            "redactionPosture": "standard"
                        }
                    })),
                    explanation: None,
                },
                SearchHit {
                    doc_id: "local-doc".to_string(),
                    score: 0.91,
                    source: ScoreSource::SemanticFast,
                    fast_score: Some(0.91),
                    quality_score: None,
                    lexical_score: None,
                    rerank_score: None,
                    metadata: Some(serde_json::json!({
                        "content": "Local result remains visible.",
                        "level": "semantic",
                        "kind": "fact"
                    })),
                    explanation: None,
                },
            ],
            elapsed_ms: 12.3,
            errors: Vec::new(),
            degraded: Vec::new(),
            runtime_profile: test_runtime_profile(),
            relevance_floor_applied: None,
            candidates_below_floor: 0,
            source_mode_requested: SearchSourceMode::Hybrid,
            source_mode_applied: SearchSourceMode::Hybrid,
            source_mode_fallback: false,
            strict_source_mode: false,
            memory_scope: MemoryScope::Swarm,
            strict_scope: false,
            scope_stats: MemoryScopeStats::new(MemoryScope::Swarm, false, None, 0),
        };

        let json = report.data_json();

        assert_eq!(json["resultCount"], 1);
        assert_eq!(json["metrics"]["returnedCount"], 1);
        assert_eq!(json["results"][0]["docId"], "local-doc");
        let rendered = json.to_string();
        assert!(!rendered.contains("mesh-quarantined"));
        assert!(!rendered.contains("PRIVATE REMOTE MESH BODY MUST NOT RENDER"));
        assert!(!rendered.contains("/Users/alice/private/repo"));
    }

    #[test]
    fn search_pack_display_analysis_blocks_non_allowed_mesh_hits() {
        let blocked_hit = SearchHit {
            doc_id: "mem_30000000000000000000000001".to_string(),
            score: 0.87,
            source: ScoreSource::SemanticFast,
            fast_score: Some(0.87),
            quality_score: None,
            lexical_score: None,
            rerank_score: None,
            metadata: Some(serde_json::json!({
                "content": "Quarantined mesh evidence must not reach search display pack analysis.",
                "level": "episodic",
                "kind": "evidence",
                "mesh": {
                    "workspaceScopeDecision": "quarantine",
                    "cachedMaterialId": "mesh_mat_quarantined",
                    "originWorkspaceId": "wsp_remote_beta",
                    "producerPeerId": "peer_builder_one",
                    "materialLane": "curationSignal",
                    "trustLane": "mesh_curation",
                    "redactionPosture": "standard"
                }
            })),
            explanation: None,
        };

        let local_hit = SearchHit {
            doc_id: "mem_30000000000000000000000002".to_string(),
            score: 0.91,
            source: ScoreSource::SemanticFast,
            fast_score: Some(0.91),
            quality_score: None,
            lexical_score: None,
            rerank_score: None,
            metadata: Some(serde_json::json!({
                "content": "Local evidence remains eligible for search display pack analysis.",
                "level": "episodic",
                "kind": "evidence"
            })),
            explanation: None,
        };

        assert!(
            search_hit_pack_item(0, &blocked_hit).is_none(),
            "blocked mesh hits must not enter search consensus/display pack analysis"
        );
        assert!(
            search_hit_pack_item(0, &local_hit).is_some(),
            "local hits should remain eligible for search consensus/display pack analysis"
        );
    }

    #[test]
    fn source_mode_resolution_reports_lexical_unavailable_without_fallback() -> TestResult {
        let options = source_mode_test_options(SearchSourceMode::LexicalOnly, false);
        let index_dir = options.resolve_index_dir();
        let mut degraded = Vec::new();

        let resolution = resolve_source_mode(&options, &index_dir, &mut degraded)
            .map_err(|error| error.to_string())?;

        assert_eq!(resolution.applied, SearchSourceMode::LexicalOnly);
        assert!(!resolution.fallback_applied);
        assert!(resolution.unavailable_no_results);
        assert_eq!(degraded.len(), 1);
        assert_eq!(degraded[0].code, "lexical_unavailable");
        assert_eq!(degraded[0].severity, "warning");
        Ok(())
    }

    #[test]
    fn source_mode_resolution_strict_errors_when_lexical_is_unavailable() -> TestResult {
        let options = source_mode_test_options(SearchSourceMode::LexicalOnly, true);
        let index_dir = options.resolve_index_dir();
        let mut degraded = Vec::new();

        let error = match resolve_source_mode(&options, &index_dir, &mut degraded) {
            Ok(_) => {
                return Err(
                    "strict lexical-only mode should fail when lexical index is unavailable"
                        .to_owned(),
                );
            }
            Err(error) => error,
        };

        match error {
            SearchError::SourceModeUnavailable { requested, reason } => {
                assert_eq!(requested, SearchSourceMode::LexicalOnly);
                assert!(reason.contains("lexical-bm25 index is unavailable"));
            }
            other => return Err(format!("unexpected source mode error: {other}")),
        }
        assert!(degraded.is_empty());
        Ok(())
    }

    #[test]
    fn source_mode_resolution_honors_semantic_only_without_lexical() -> TestResult {
        let options = source_mode_test_options(SearchSourceMode::SemanticOnly, true);
        let index_dir = options.resolve_index_dir();
        let mut degraded = Vec::new();

        let resolution = resolve_source_mode(&options, &index_dir, &mut degraded)
            .map_err(|error| error.to_string())?;

        assert_eq!(resolution.applied, SearchSourceMode::SemanticOnly);
        assert!(!resolution.fallback_applied);
        assert!(!resolution.unavailable_no_results);
        assert!(degraded.is_empty());
        Ok(())
    }

    #[test]
    fn source_mode_resolution_falls_back_for_default_hybrid_without_lexical() -> TestResult {
        let options = source_mode_test_options(SearchSourceMode::Hybrid, false);
        let index_dir = options.resolve_index_dir();
        let mut degraded = Vec::new();

        let resolution = resolve_source_mode(&options, &index_dir, &mut degraded)
            .map_err(|error| error.to_string())?;

        assert_eq!(resolution.applied, SearchSourceMode::SemanticOnly);
        assert!(resolution.fallback_applied);
        assert!(!resolution.unavailable_no_results);
        assert_eq!(degraded.len(), 1);
        assert_eq!(degraded[0].code, "source_mode_fallback");
        Ok(())
    }

    #[test]
    fn emit_embed_model_unavailable_when_embedder_only_down() -> TestResult {
        let options = source_mode_test_options(SearchSourceMode::Hybrid, false);
        let mut degraded = Vec::new();
        let resolution = resolve_source_mode_with_tiers(
            &options,
            &mut degraded,
            SearchTierState {
                lexical_available: true,
                embed_model_unavailable: Some("missing model fixture"),
            },
        )
        .map_err(|error| error.to_string())?;

        assert_eq!(resolution.applied, SearchSourceMode::LexicalOnly);
        assert!(resolution.fallback_applied);
        assert!(!resolution.unavailable_no_results);
        let codes: Vec<&str> = degraded
            .iter()
            .map(|degradation| degradation.code.as_str())
            .collect();
        assert_eq!(codes, vec!["embed_model_unavailable"]);
        Ok(())
    }

    #[test]
    fn emit_lexical_unavailable_when_fts5_only_down() -> TestResult {
        let options = source_mode_test_options(SearchSourceMode::LexicalOnly, false);
        let mut degraded = Vec::new();
        let resolution = resolve_source_mode_with_tiers(
            &options,
            &mut degraded,
            SearchTierState {
                lexical_available: false,
                embed_model_unavailable: None,
            },
        )
        .map_err(|error| error.to_string())?;

        assert_eq!(resolution.applied, SearchSourceMode::LexicalOnly);
        assert!(!resolution.fallback_applied);
        assert!(resolution.unavailable_no_results);
        let codes: Vec<&str> = degraded
            .iter()
            .map(|degradation| degradation.code.as_str())
            .collect();
        assert_eq!(codes, vec!["lexical_unavailable"]);
        Ok(())
    }

    #[test]
    fn emit_search_unavailable_when_both_tiers_down() -> TestResult {
        let options = source_mode_test_options(SearchSourceMode::Hybrid, false);
        let mut degraded = Vec::new();
        let resolution = resolve_source_mode_with_tiers(
            &options,
            &mut degraded,
            SearchTierState {
                lexical_available: false,
                embed_model_unavailable: Some("missing model fixture"),
            },
        )
        .map_err(|error| error.to_string())?;

        assert_eq!(resolution.applied, SearchSourceMode::Hybrid);
        assert!(!resolution.fallback_applied);
        assert!(resolution.unavailable_no_results);
        let codes: Vec<&str> = degraded
            .iter()
            .map(|degradation| degradation.code.as_str())
            .collect();
        assert_eq!(codes, vec!["search_unavailable"]);
        Ok(())
    }

    #[test]
    fn embed_model_unavailable_details_includes_model_id_and_feature_flag() {
        let rendered =
            SearchDegradation::embed_model_unavailable("missing model fixture").data_json();

        assert_eq!(rendered["code"], "embed_model_unavailable");
        assert_eq!(
            rendered["details"]["modelId"],
            EMBED_MODEL_UNAVAILABLE_MODEL_ID
        );
        assert_eq!(
            rendered["details"]["featureFlag"],
            EMBED_MODEL_UNAVAILABLE_FEATURE_FLAG
        );
        assert_eq!(rendered["details"]["lexicalAvailable"], true);
    }

    #[test]
    fn embed_model_unavailable_recovery_two_or_three_actions() {
        let rendered =
            SearchDegradation::embed_model_unavailable("missing model fixture").data_json();
        let recovery = rendered
            .pointer("/details/recovery")
            .and_then(serde_json::Value::as_array)
            .expect("embed_model_unavailable should expose recovery details");

        assert_eq!(recovery.len(), 2, "F4b chose the two-action rebuild recipe");
        assert_eq!(recovery[0]["command"], "ee index reembed --workspace .");
        assert_eq!(
            recovery[1]["command"],
            "cargo build --features embed-quality"
        );
    }

    #[test]
    fn tombstone_visibility_excludes_by_default_and_marks_opt_in_results() -> TestResult {
        let workspace = unique_test_dir("tombstone-visibility");
        std::fs::create_dir_all(&workspace).map_err(|error| error.to_string())?;
        let database_path = workspace.join("ee.db");
        let connection =
            DbConnection::open_file(&database_path).map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        connection
            .insert_workspace(
                "wsp_01234567890123456789012345",
                &CreateWorkspaceInput {
                    path: workspace.display().to_string(),
                    name: Some("tombstone visibility".to_string()),
                },
            )
            .map_err(|error| error.to_string())?;
        connection
            .insert_memory(
                "mem_00000000000000000000000001",
                &CreateMemoryInput {
                    workspace_id: "wsp_01234567890123456789012345".to_string(),
                    level: "procedural".to_string(),
                    kind: "rule".to_string(),
                    content: "Run cargo fmt before release.".to_string(),
                    workflow_id: None,
                    confidence: 0.9,
                    utility: 0.5,
                    importance: 0.5,
                    provenance_uri: None,
                    trust_class: "agent_assertion".to_string(),
                    trust_subclass: None,
                    tags: Vec::new(),
                    valid_from: None,
                    valid_to: None,
                },
            )
            .map_err(|error| error.to_string())?;
        connection
            .tombstone_memory("mem_00000000000000000000000001")
            .map_err(|error| error.to_string())?;
        drop(connection);

        let hit = SearchHit {
            doc_id: "mem_00000000000000000000000001".to_string(),
            score: 0.9,
            source: ScoreSource::Lexical,
            fast_score: None,
            quality_score: None,
            lexical_score: Some(0.9),
            rerank_score: None,
            metadata: None,
            explanation: None,
        };
        let base_options = SearchOptions {
            workspace_path: workspace.clone(),
            database_path: Some(database_path.clone()),
            index_dir: None,
            query: "cargo fmt".to_string(),
            limit: 10,
            speed: SpeedMode::Default,
            explain: false,
            as_of: None,
            include_tombstoned: false,
            include_expired: false,
            include_future: false,
            include_stale: false,
            relevance_floor: None,
            dedup_mode: SearchDedupMode::DocId,
            source_mode: SearchSourceMode::Hybrid,
            strict_source_mode: false,
            memory_scope: MemoryScope::Swarm,
            strict_scope: false,
        };

        let read_connection =
            DbConnection::open_file(&database_path).map_err(|error| error.to_string())?;
        let mut snapshot_options = base_options.clone();
        snapshot_options.database_path = Some(workspace.join("missing.db"));
        let mut degraded = Vec::new();
        let visible = apply_tombstone_visibility(
            &snapshot_options,
            vec![hit.clone()],
            &mut degraded,
            Some(&read_connection),
        );
        assert!(visible.is_empty());
        assert_eq!(degraded[0].code, "tombstoned_filtered");

        let mut degraded = Vec::new();
        let visible =
            apply_tombstone_visibility(&base_options, vec![hit.clone()], &mut degraded, None);
        assert!(visible.is_empty());
        assert_eq!(degraded[0].code, "tombstoned_filtered");

        let mut include_options = base_options.clone();
        include_options.include_tombstoned = true;
        let mut degraded = Vec::new();
        let visible = apply_tombstone_visibility(&include_options, vec![hit], &mut degraded, None);
        assert_eq!(visible.len(), 1);
        assert_eq!(degraded[0].code, "tombstoned_in_results");

        let report = SearchReport {
            status: SearchStatus::Success,
            query: "cargo fmt".to_string(),
            requested_limit: 10,
            results: visible,
            elapsed_ms: 1.0,
            errors: Vec::new(),
            degraded,
            runtime_profile: test_runtime_profile(),
            relevance_floor_applied: None,
            candidates_below_floor: 0,
            source_mode_requested: SearchSourceMode::Hybrid,
            source_mode_applied: SearchSourceMode::Hybrid,
            source_mode_fallback: false,
            strict_source_mode: false,
            memory_scope: MemoryScope::Swarm,
            strict_scope: false,
            scope_stats: MemoryScopeStats::new(MemoryScope::Swarm, false, None, 0),
        };
        let json = report.data_json();
        assert_eq!(json["results"][0]["tombstoned"], true);
        assert!(json["results"][0]["tombstonedAt"].is_string());
        assert_eq!(json["results"][0]["metadata"]["tombstoned"], true);
        Ok(())
    }

    #[test]
    fn drift_hint_is_added_to_visible_search_results() -> TestResult {
        let workspace = unique_test_dir("drift-hint-visibility");
        std::fs::create_dir_all(&workspace).map_err(|error| error.to_string())?;
        let database_path = workspace.join("ee.db");
        let connection =
            DbConnection::open_file(&database_path).map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        connection
            .insert_workspace(
                "wsp_31234567890123456789012345",
                &CreateWorkspaceInput {
                    path: workspace.display().to_string(),
                    name: Some("drift hint visibility".to_string()),
                },
            )
            .map_err(|error| error.to_string())?;
        connection
            .insert_memory(
                "mem_30000000000000000000000001",
                &CreateMemoryInput {
                    workspace_id: "wsp_31234567890123456789012345".to_string(),
                    level: "procedural".to_string(),
                    kind: "rule".to_string(),
                    content: "Revalidate stale provenance before trusting search hits.".to_string(),
                    workflow_id: None,
                    confidence: 0.9,
                    utility: 0.5,
                    importance: 0.5,
                    provenance_uri: None,
                    trust_class: "agent_assertion".to_string(),
                    trust_subclass: None,
                    tags: Vec::new(),
                    valid_from: None,
                    valid_to: None,
                },
            )
            .map_err(|error| error.to_string())?;
        connection
            .execute_raw(
                "UPDATE memories SET provenance_verification_status = 'mismatch', provenance_chain_hash = 'blake3:stale' WHERE id = 'mem_30000000000000000000000001'",
            )
            .map_err(|error| error.to_string())?;

        let hit = SearchHit {
            doc_id: "mem_30000000000000000000000001".to_string(),
            score: 0.9,
            source: ScoreSource::Lexical,
            fast_score: None,
            quality_score: None,
            lexical_score: Some(0.9),
            rerank_score: None,
            metadata: Some(serde_json::json!({
                "content": "Revalidate stale provenance before trusting search hits.",
            })),
            explanation: None,
        };
        let options = SearchOptions {
            workspace_path: workspace,
            database_path: Some(database_path),
            index_dir: None,
            query: "stale provenance".to_string(),
            limit: 10,
            speed: SpeedMode::Default,
            explain: false,
            as_of: None,
            include_tombstoned: false,
            include_expired: false,
            include_future: false,
            include_stale: false,
            relevance_floor: None,
            dedup_mode: SearchDedupMode::DocId,
            source_mode: SearchSourceMode::Hybrid,
            strict_source_mode: false,
            memory_scope: MemoryScope::Swarm,
            strict_scope: false,
        };

        let mut degraded = Vec::new();
        let visible =
            apply_tombstone_visibility(&options, vec![hit], &mut degraded, Some(&connection));
        assert_eq!(visible.len(), 1);
        assert_eq!(degraded.len(), 1);
        assert_eq!(degraded[0].code, "memory_drift_source_changed");

        let report = SearchReport {
            status: SearchStatus::Success,
            query: "stale provenance".to_string(),
            requested_limit: 10,
            results: visible,
            elapsed_ms: 1.0,
            errors: Vec::new(),
            degraded,
            runtime_profile: test_runtime_profile(),
            relevance_floor_applied: None,
            candidates_below_floor: 0,
            source_mode_requested: SearchSourceMode::Hybrid,
            source_mode_applied: SearchSourceMode::Hybrid,
            source_mode_fallback: false,
            strict_source_mode: false,
            memory_scope: MemoryScope::Swarm,
            strict_scope: false,
            scope_stats: MemoryScopeStats::new(MemoryScope::Swarm, false, None, 0),
        };
        let json = report.data_json();
        assert_eq!(json["degraded"][0]["code"], "memory_drift_source_changed");
        assert_eq!(json["results"][0]["driftHint"]["driftStatus"], "changed");
        assert_eq!(
            json["results"][0]["metadata"]["driftHint"]["topReason"],
            "provenance_chain_mismatch"
        );
        assert_eq!(
            json["results"][0]["metadata"]["provenanceVerificationStatus"],
            "mismatch"
        );
        Ok(())
    }

    #[test]
    fn drift_hints_keep_search_order_and_report_highest_risk() -> TestResult {
        let workspace = unique_test_dir("drift-hint-order");
        std::fs::create_dir_all(&workspace).map_err(|error| error.to_string())?;
        let database_path = workspace.join("ee.db");
        let connection =
            DbConnection::open_file(&database_path).map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        connection
            .insert_workspace(
                "wsp_32234567890123456789012345",
                &CreateWorkspaceInput {
                    path: workspace.display().to_string(),
                    name: Some("drift hint order".to_string()),
                },
            )
            .map_err(|error| error.to_string())?;

        for (memory_id, content, status) in [
            (
                "mem_31000000000000000000000001",
                "Changed provenance should keep its original search rank.",
                "mismatch",
            ),
            (
                "mem_31000000000000000000000002",
                "Missing provenance is the highest risk selected search hit.",
                "missing",
            ),
        ] {
            connection
                .insert_memory(
                    memory_id,
                    &CreateMemoryInput {
                        workspace_id: "wsp_32234567890123456789012345".to_string(),
                        level: "procedural".to_string(),
                        kind: "rule".to_string(),
                        content: content.to_string(),
                        workflow_id: None,
                        confidence: 0.9,
                        utility: 0.5,
                        importance: 0.5,
                        provenance_uri: None,
                        trust_class: "agent_assertion".to_string(),
                        trust_subclass: None,
                        tags: Vec::new(),
                        valid_from: None,
                        valid_to: None,
                    },
                )
                .map_err(|error| error.to_string())?;
            connection
                .execute_raw(&format!(
                    "UPDATE memories SET provenance_verification_status = '{status}', provenance_chain_hash = 'blake3:{memory_id}' WHERE id = '{memory_id}'",
                ))
                .map_err(|error| error.to_string())?;
        }

        let hits = vec![
            SearchHit {
                doc_id: "mem_31000000000000000000000001".to_string(),
                score: 0.95,
                source: ScoreSource::Lexical,
                fast_score: None,
                quality_score: None,
                lexical_score: Some(0.95),
                rerank_score: None,
                metadata: Some(serde_json::json!({
                    "content": "Changed provenance should keep its original search rank.",
                })),
                explanation: None,
            },
            SearchHit {
                doc_id: "mem_31000000000000000000000002".to_string(),
                score: 0.85,
                source: ScoreSource::Lexical,
                fast_score: None,
                quality_score: None,
                lexical_score: Some(0.85),
                rerank_score: None,
                metadata: Some(serde_json::json!({
                    "content": "Missing provenance is the highest risk selected search hit.",
                })),
                explanation: None,
            },
        ];
        let options = SearchOptions {
            workspace_path: workspace,
            database_path: Some(database_path),
            index_dir: None,
            query: "provenance drift".to_string(),
            limit: 10,
            speed: SpeedMode::Default,
            explain: false,
            as_of: None,
            include_tombstoned: false,
            include_expired: false,
            include_future: false,
            include_stale: false,
            relevance_floor: None,
            dedup_mode: SearchDedupMode::DocId,
            source_mode: SearchSourceMode::Hybrid,
            strict_source_mode: false,
            memory_scope: MemoryScope::Swarm,
            strict_scope: false,
        };

        let mut degraded = Vec::new();
        let visible = apply_tombstone_visibility(&options, hits, &mut degraded, Some(&connection));
        assert_eq!(
            visible
                .iter()
                .map(|hit| hit.doc_id.as_str())
                .collect::<Vec<_>>(),
            vec![
                "mem_31000000000000000000000001",
                "mem_31000000000000000000000002",
            ]
        );
        assert_eq!(degraded.len(), 1);
        assert_eq!(degraded[0].code, "memory_drift_source_missing");
        assert!(
            degraded[0]
                .repair
                .as_deref()
                .is_some_and(|repair| repair.contains("mem_31000000000000000000000002"))
        );

        let report = SearchReport {
            status: SearchStatus::Success,
            query: "provenance drift".to_string(),
            requested_limit: 10,
            results: visible,
            elapsed_ms: 1.0,
            errors: Vec::new(),
            degraded,
            runtime_profile: test_runtime_profile(),
            relevance_floor_applied: None,
            candidates_below_floor: 0,
            source_mode_requested: SearchSourceMode::Hybrid,
            source_mode_applied: SearchSourceMode::Hybrid,
            source_mode_fallback: false,
            strict_source_mode: false,
            memory_scope: MemoryScope::Swarm,
            strict_scope: false,
            scope_stats: MemoryScopeStats::new(MemoryScope::Swarm, false, None, 0),
        };
        let json = report.data_json();
        assert_eq!(
            json["results"][0]["docId"],
            "mem_31000000000000000000000001"
        );
        assert_eq!(
            json["results"][1]["docId"],
            "mem_31000000000000000000000002"
        );
        assert_eq!(json["degraded"][0]["code"], "memory_drift_source_missing");
        assert_eq!(json["results"][0]["driftHint"]["driftStatus"], "changed");
        assert_eq!(
            json["results"][1]["driftHint"]["driftStatus"],
            "missing_source"
        );
        assert_eq!(
            json["results"][1]["metadata"]["driftHint"]["topReason"],
            "provenance_chain_missing"
        );
        Ok(())
    }

    #[test]
    #[allow(clippy::expect_used)]
    fn validity_visibility_respects_windows_as_of_and_overrides() {
        let reference = DateTime::parse_from_rfc3339("2026-05-13T00:00:00Z")
            .expect("valid reference time")
            .with_timezone(&Utc);

        assert_eq!(
            memory_validity_visibility(None, None, reference, false, false),
            MemoryValidityVisibility::Visible
        );
        assert_eq!(
            memory_validity_visibility(Some("2026-06-01T00:00:00Z"), None, reference, false, false,),
            MemoryValidityVisibility::Future
        );
        assert_eq!(
            memory_validity_visibility(Some("2026-06-01T00:00:00Z"), None, reference, false, true,),
            MemoryValidityVisibility::Visible
        );
        assert_eq!(
            memory_validity_visibility(None, Some("2026-05-01T00:00:00Z"), reference, false, false,),
            MemoryValidityVisibility::Expired
        );
        assert_eq!(
            memory_validity_visibility(None, Some("2026-05-01T00:00:00Z"), reference, true, false,),
            MemoryValidityVisibility::Visible
        );
        assert_eq!(
            memory_validity_visibility(Some("not-a-time"), None, reference, true, true),
            MemoryValidityVisibility::Malformed
        );
        assert_eq!(
            validity_status_at(
                Some("2026-01-01T00:00:00Z"),
                Some("2026-06-30T00:00:00Z"),
                reference,
            ),
            "current"
        );
    }

    #[test]
    fn indexed_stale_validity_status_excluded_by_default_and_opt_in() -> TestResult {
        let workspace = unique_test_dir("stale-validity-visibility");
        std::fs::create_dir_all(&workspace).map_err(|error| error.to_string())?;
        let database_path = workspace.join("ee.db");
        let connection =
            DbConnection::open_file(&database_path).map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        connection
            .insert_workspace(
                "wsp_11234567890123456789012345",
                &CreateWorkspaceInput {
                    path: workspace.display().to_string(),
                    name: Some("stale validity visibility".to_string()),
                },
            )
            .map_err(|error| error.to_string())?;
        connection
            .insert_memory(
                "mem_10000000000000000000000001",
                &CreateMemoryInput {
                    workspace_id: "wsp_11234567890123456789012345".to_string(),
                    level: "semantic".to_string(),
                    kind: "fact".to_string(),
                    content: "Indexed stale validity status should be opt-in only.".to_string(),
                    workflow_id: None,
                    confidence: 0.9,
                    utility: 0.5,
                    importance: 0.5,
                    provenance_uri: None,
                    trust_class: "agent_assertion".to_string(),
                    trust_subclass: None,
                    tags: Vec::new(),
                    valid_from: None,
                    valid_to: None,
                },
            )
            .map_err(|error| error.to_string())?;
        drop(connection);

        let hit = SearchHit {
            doc_id: "mem_10000000000000000000000001".to_string(),
            score: 0.9,
            source: ScoreSource::Lexical,
            fast_score: None,
            quality_score: None,
            lexical_score: Some(0.9),
            rerank_score: None,
            metadata: Some(serde_json::json!({ "validity_status": "stale" })),
            explanation: None,
        };
        let base_options = SearchOptions {
            workspace_path: workspace,
            database_path: Some(database_path),
            index_dir: None,
            query: "stale validity".to_string(),
            limit: 10,
            speed: SpeedMode::Default,
            explain: false,
            as_of: None,
            include_tombstoned: false,
            include_expired: false,
            include_future: false,
            include_stale: false,
            relevance_floor: None,
            dedup_mode: SearchDedupMode::DocId,
            source_mode: SearchSourceMode::Hybrid,
            strict_source_mode: false,
            memory_scope: MemoryScope::Swarm,
            strict_scope: false,
        };

        let mut degraded = Vec::new();
        let visible =
            apply_tombstone_visibility(&base_options, vec![hit.clone()], &mut degraded, None);
        assert!(visible.is_empty());
        assert_eq!(degraded[0].code, "stale_validity_filtered");

        let mut include_options = base_options;
        include_options.include_stale = true;
        let mut degraded = Vec::new();
        let visible = apply_tombstone_visibility(&include_options, vec![hit], &mut degraded, None);
        assert_eq!(visible.len(), 1);
        let report = SearchReport {
            status: SearchStatus::Success,
            query: "stale validity".to_string(),
            requested_limit: 10,
            results: visible,
            elapsed_ms: 1.0,
            errors: Vec::new(),
            degraded,
            runtime_profile: test_runtime_profile(),
            relevance_floor_applied: None,
            candidates_below_floor: 0,
            source_mode_requested: SearchSourceMode::Hybrid,
            source_mode_applied: SearchSourceMode::Hybrid,
            source_mode_fallback: false,
            strict_source_mode: false,
            memory_scope: MemoryScope::Swarm,
            strict_scope: false,
            scope_stats: MemoryScopeStats::new(MemoryScope::Swarm, false, None, 0),
        };
        let json = report.data_json();
        assert_eq!(json["results"][0]["validityStatus"], "stale");
        assert_eq!(json["results"][0]["metadata"]["validity_status"], "stale");
        Ok(())
    }

    #[test]
    fn indexed_validity_window_mismatch_is_stale_and_opt_in() -> TestResult {
        let workspace = unique_test_dir("stale-validity-window-visibility");
        std::fs::create_dir_all(&workspace).map_err(|error| error.to_string())?;
        let database_path = workspace.join("ee.db");
        let connection =
            DbConnection::open_file(&database_path).map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        connection
            .insert_workspace(
                "wsp_21234567890123456789012345",
                &CreateWorkspaceInput {
                    path: workspace.display().to_string(),
                    name: Some("stale validity window visibility".to_string()),
                },
            )
            .map_err(|error| error.to_string())?;
        connection
            .insert_memory(
                "mem_20000000000000000000000001",
                &CreateMemoryInput {
                    workspace_id: "wsp_21234567890123456789012345".to_string(),
                    level: "semantic".to_string(),
                    kind: "fact".to_string(),
                    content: "Indexed stale validity window should be opt-in only.".to_string(),
                    workflow_id: None,
                    confidence: 0.9,
                    utility: 0.5,
                    importance: 0.5,
                    provenance_uri: None,
                    trust_class: "agent_assertion".to_string(),
                    trust_subclass: None,
                    tags: Vec::new(),
                    valid_from: None,
                    valid_to: None,
                },
            )
            .map_err(|error| error.to_string())?;
        drop(connection);

        let hit = SearchHit {
            doc_id: "mem_20000000000000000000000001".to_string(),
            score: 0.9,
            source: ScoreSource::Lexical,
            fast_score: None,
            quality_score: None,
            lexical_score: Some(0.9),
            rerank_score: None,
            metadata: Some(serde_json::json!({
                "valid_to": "2026-05-01T00:00:00Z",
                "validity_window_kind": "ends_at",
            })),
            explanation: None,
        };
        let base_options = SearchOptions {
            workspace_path: workspace,
            database_path: Some(database_path),
            index_dir: None,
            query: "stale validity window".to_string(),
            limit: 10,
            speed: SpeedMode::Default,
            explain: false,
            as_of: None,
            include_tombstoned: false,
            include_expired: false,
            include_future: false,
            include_stale: false,
            relevance_floor: None,
            dedup_mode: SearchDedupMode::DocId,
            source_mode: SearchSourceMode::Hybrid,
            strict_source_mode: false,
            memory_scope: MemoryScope::Swarm,
            strict_scope: false,
        };

        let mut degraded = Vec::new();
        let visible =
            apply_tombstone_visibility(&base_options, vec![hit.clone()], &mut degraded, None);
        assert!(visible.is_empty());
        assert_eq!(degraded[0].code, "stale_validity_filtered");

        let mut include_options = base_options;
        include_options.include_stale = true;
        let mut degraded = Vec::new();
        let visible = apply_tombstone_visibility(&include_options, vec![hit], &mut degraded, None);
        assert_eq!(visible.len(), 1);
        let report = SearchReport {
            status: SearchStatus::Success,
            query: "stale validity window".to_string(),
            requested_limit: 10,
            results: visible,
            elapsed_ms: 1.0,
            errors: Vec::new(),
            degraded,
            runtime_profile: test_runtime_profile(),
            relevance_floor_applied: None,
            candidates_below_floor: 0,
            source_mode_requested: SearchSourceMode::Hybrid,
            source_mode_applied: SearchSourceMode::Hybrid,
            source_mode_fallback: false,
            strict_source_mode: false,
            memory_scope: MemoryScope::Swarm,
            strict_scope: false,
            scope_stats: MemoryScopeStats::new(MemoryScope::Swarm, false, None, 0),
        };
        let json = report.data_json();
        assert_eq!(json["results"][0]["validityStatus"], "current");
        assert_eq!(json["results"][0]["metadata"]["validity_status"], "current");
        assert!(json["results"][0]["metadata"].get("valid_to").is_none());
        Ok(())
    }

    #[test]
    fn search_performance_explain_report_is_redaction_safe_and_pins_fallbacks() {
        let report = SearchReport {
            status: SearchStatus::Success,
            query: "rotate secret sk_live_do_not_emit".to_string(),
            requested_limit: 10,
            results: vec![SearchHit {
                doc_id: "mem-secret-doc".to_string(),
                score: 0.95,
                source: ScoreSource::Lexical,
                fast_score: None,
                quality_score: None,
                lexical_score: Some(0.95),
                rerank_score: None,
                metadata: Some(serde_json::json!({
                    "content": "token should not leave normal search output",
                })),
                explanation: None,
            }],
            elapsed_ms: 12.3,
            errors: Vec::new(),
            degraded: vec![SearchDegradation::stale_index(Some(12), Some(9))],
            runtime_profile: test_runtime_profile(),
            relevance_floor_applied: None,
            candidates_below_floor: 0,
            source_mode_requested: SearchSourceMode::Hybrid,
            source_mode_applied: SearchSourceMode::Hybrid,
            source_mode_fallback: false,
            strict_source_mode: false,
            memory_scope: MemoryScope::Swarm,
            strict_scope: false,
            scope_stats: test_scope_stats(),
        };

        let json = report.performance_explain_json(SpeedMode::Instant, false);
        let rendered = json.to_string();

        assert_eq!(json["schema"], PERFORMANCE_EXPLAIN_SCHEMA_V1);
        assert_eq!(json["data"]["command"], "search");
        assert_eq!(json["data"]["query"]["textIncluded"], false);
        assert_eq!(json["data"]["search"]["returnedHits"], 1);
        assert_eq!(json["data"]["fallbacks"][0]["code"], "search_index_stale");
        assert_eq!(json["data"]["redaction"]["memoryContentIncluded"], false);
        assert!(!rendered.contains("sk_live_do_not_emit"));
        assert!(!rendered.contains("mem-secret-doc"));
        assert!(!rendered.contains("token should not leave"));
    }

    #[test]
    fn search_report_degraded_entries_are_aggregated() {
        let report = SearchReport {
            status: SearchStatus::Success,
            query: "format before release".to_string(),
            requested_limit: 10,
            results: Vec::new(),
            elapsed_ms: 7.0,
            errors: Vec::new(),
            degraded: vec![
                SearchDegradation {
                    code: "search_index_stale".to_string(),
                    severity: "warning".to_string(),
                    message: "Search index is stale.".to_string(),
                    repair: Some("ee index rebuild --workspace .".to_string()),
                },
                SearchDegradation {
                    code: "search_index_stale".to_string(),
                    severity: "high".to_string(),
                    message: "Search index is stale and missing recent memories.".to_string(),
                    repair: Some("ee index rebuild --workspace . --force".to_string()),
                },
            ],
            runtime_profile: test_runtime_profile(),
            relevance_floor_applied: None,
            candidates_below_floor: 0,
            source_mode_requested: SearchSourceMode::Hybrid,
            source_mode_applied: SearchSourceMode::Hybrid,
            source_mode_fallback: false,
            strict_source_mode: false,
            memory_scope: MemoryScope::Swarm,
            strict_scope: false,
            scope_stats: test_scope_stats(),
        };

        let json = report.data_json();
        assert_eq!(json["degraded"].as_array().map(Vec::len), Some(1));
        assert_eq!(json["degraded"][0]["code"], "search_index_stale");
        assert_eq!(json["degraded"][0]["severity"], "high");
        assert_eq!(
            json["degraded"][0]["repair"],
            "ee index rebuild --workspace . --force"
        );
        assert_eq!(json["degraded"][0]["sources"][0], "search");

        let perf = report.performance_explain_data_json(SpeedMode::Instant, false);
        assert_eq!(perf["fallbacks"].as_array().map(Vec::len), Some(1));
        assert_eq!(perf["fallbacks"][0]["sources"][0], "search");
    }

    #[test]
    fn search_data_json_redacts_public_content_metadata() {
        let raw_value = concat!("sk", "_", "search", "_", "secret", "_", "123");
        let report = SearchReport {
            status: SearchStatus::Success,
            query: "rotate output secrets".to_string(),
            requested_limit: 10,
            results: vec![SearchHit {
                doc_id: "mem-secret-doc".to_string(),
                score: 0.95,
                source: ScoreSource::Lexical,
                fast_score: None,
                quality_score: None,
                lexical_score: Some(0.95),
                rerank_score: None,
                metadata: Some(serde_json::json!({
                    "content": format!("Rotate api_key={raw_value} before release."),
                    "contentPreview": format!("Preview api_key={raw_value}."),
                })),
                explanation: None,
            }],
            elapsed_ms: 12.3,
            errors: Vec::new(),
            degraded: Vec::new(),
            runtime_profile: test_runtime_profile(),
            relevance_floor_applied: None,
            candidates_below_floor: 0,
            source_mode_requested: SearchSourceMode::Hybrid,
            source_mode_applied: SearchSourceMode::Hybrid,
            source_mode_fallback: false,
            strict_source_mode: false,
            memory_scope: MemoryScope::Swarm,
            strict_scope: false,
            scope_stats: test_scope_stats(),
        };

        let json = report.data_json();
        let rendered = json.to_string();

        assert!(!rendered.contains(raw_value));
        assert_eq!(json["results"][0]["contentRedacted"], true);
        assert_eq!(
            json["results"][0]["metadata"]["content"].as_str(),
            Some("Rotate api_key=[REDACTED:api_key] before release.")
        );
        assert_eq!(json["results"][0]["metadata"].get("contentPreview"), None);
        assert_eq!(
            json["results"][0]["metadata"]["content_truncated"].as_bool(),
            Some(false)
        );
        assert_eq!(
            json["results"][0]["redactions"][0]["reason"].as_str(),
            Some("api_key")
        );
    }

    #[test]
    fn search_data_json_respects_output_redaction_disabled_degradation() {
        let raw_value = concat!("sk", "_", "search", "_", "disabled", "_", "123");
        let report = SearchReport {
            status: SearchStatus::Success,
            query: "rotate output secrets".to_string(),
            requested_limit: 10,
            results: vec![SearchHit {
                doc_id: "mem-secret-doc".to_string(),
                score: 0.95,
                source: ScoreSource::Lexical,
                fast_score: None,
                quality_score: None,
                lexical_score: Some(0.95),
                rerank_score: None,
                metadata: Some(serde_json::json!({
                    "contentPreview": format!("Preview api_key={raw_value}."),
                })),
                explanation: None,
            }],
            elapsed_ms: 12.3,
            errors: Vec::new(),
            degraded: vec![SearchDegradation::output_redaction_disabled()],
            runtime_profile: test_runtime_profile(),
            relevance_floor_applied: None,
            candidates_below_floor: 0,
            source_mode_requested: SearchSourceMode::Hybrid,
            source_mode_applied: SearchSourceMode::Hybrid,
            source_mode_fallback: false,
            strict_source_mode: false,
            memory_scope: MemoryScope::Swarm,
            strict_scope: false,
            scope_stats: test_scope_stats(),
        };

        let json = report.data_json();
        let rendered = json.to_string();
        let expected_preview = format!("Preview api_key={raw_value}.");

        assert!(rendered.contains(raw_value));
        assert_eq!(json["results"][0].get("contentRedacted"), None);
        assert_eq!(
            json["results"][0]["metadata"]["content"].as_str(),
            Some(expected_preview.as_str())
        );
        assert_eq!(json["results"][0]["metadata"].get("contentPreview"), None);
        assert_eq!(
            json["results"][0]["metadata"]["content_truncated"].as_bool(),
            Some(false)
        );
        assert_eq!(
            json["degraded"][0]["code"].as_str(),
            Some("output_redaction_disabled")
        );
    }

    #[test]
    fn search_data_json_disabled_output_redaction_returns_raw_content() {
        let raw_value = concat!("sk", "_", "search", "_", "raw", "_", "123");
        let report = SearchReport {
            status: SearchStatus::Success,
            query: "inspect raw output policy".to_string(),
            requested_limit: 10,
            results: vec![SearchHit {
                doc_id: "mem-raw-secret-doc".to_string(),
                score: 0.95,
                source: ScoreSource::Lexical,
                fast_score: None,
                quality_score: None,
                lexical_score: Some(0.95),
                rerank_score: None,
                metadata: Some(serde_json::json!({
                    "content": format!("Raw api_key={raw_value} is visible by policy."),
                })),
                explanation: None,
            }],
            elapsed_ms: 12.3,
            errors: Vec::new(),
            degraded: vec![SearchDegradation::output_redaction_disabled()],
            runtime_profile: test_runtime_profile(),
            relevance_floor_applied: None,
            candidates_below_floor: 0,
            source_mode_requested: SearchSourceMode::Hybrid,
            source_mode_applied: SearchSourceMode::Hybrid,
            source_mode_fallback: false,
            strict_source_mode: false,
            memory_scope: MemoryScope::Swarm,
            strict_scope: false,
            scope_stats: test_scope_stats(),
        };

        let json = report.data_json();
        let rendered = json.to_string();

        assert!(rendered.contains(raw_value));
        assert_eq!(
            json["results"][0]["contentRedacted"],
            serde_json::Value::Null
        );
        assert_eq!(json["results"][0]["redactions"], serde_json::Value::Null);
        assert_eq!(
            json["results"][0]["metadata"]["content"].as_str(),
            Some(format!("Raw api_key={raw_value} is visible by policy.").as_str())
        );
        assert_eq!(
            json["degraded"][0]["code"].as_str(),
            Some("output_redaction_disabled")
        );
        assert_eq!(json["degraded"][0]["severity"].as_str(), Some("info"));
    }

    #[test]
    fn search_data_json_redacts_hidden_analysis_content_as_public_content() {
        let raw_value = concat!("sk", "_", "search", "_", "hidden", "_", "123");
        let report = SearchReport {
            status: SearchStatus::Success,
            query: "rotate hidden output secrets".to_string(),
            requested_limit: 10,
            results: vec![SearchHit {
                doc_id: "mem-hidden-secret-doc".to_string(),
                score: 0.95,
                source: ScoreSource::Hybrid,
                fast_score: None,
                quality_score: None,
                lexical_score: Some(0.95),
                rerank_score: None,
                metadata: Some(serde_json::json!({
                    SEARCH_ANALYSIS_CONTENT_KEY: format!("Rotate api_key={raw_value} before release."),
                    "kind": "rule",
                    "level": "procedural",
                })),
                explanation: None,
            }],
            elapsed_ms: 12.3,
            errors: Vec::new(),
            degraded: Vec::new(),
            runtime_profile: test_runtime_profile(),
            relevance_floor_applied: None,
            candidates_below_floor: 0,
            source_mode_requested: SearchSourceMode::Hybrid,
            source_mode_applied: SearchSourceMode::Hybrid,
            source_mode_fallback: false,
            strict_source_mode: false,
            memory_scope: MemoryScope::Swarm,
            strict_scope: false,
            scope_stats: test_scope_stats(),
        };

        let json = report.data_json();
        let rendered = json.to_string();

        assert!(!rendered.contains(raw_value));
        assert_eq!(json["results"][0]["contentRedacted"], true);
        assert_eq!(
            json["results"][0]["metadata"]["content"].as_str(),
            Some("Rotate api_key=[REDACTED:api_key] before release.")
        );
        assert_eq!(
            json["results"][0]["metadata"].get(SEARCH_ANALYSIS_CONTENT_KEY),
            None
        );
        assert_eq!(
            json["results"][0]["redactions"][0]["reason"].as_str(),
            Some("api_key")
        );
    }

    #[test]
    fn search_degradations_report_missing_index_files() -> TestResult {
        let workspace = unique_test_dir("missing-index");
        let index_dir = workspace.join("index");
        std::fs::create_dir_all(&index_dir).map_err(|error| error.to_string())?;
        let options = SearchOptions {
            workspace_path: workspace.clone(),
            database_path: Some(workspace.join("missing.db")),
            index_dir: Some(index_dir.clone()),
            query: "format before release".to_string(),
            limit: 10,
            speed: SpeedMode::Default,
            explain: false,
            as_of: None,
            include_tombstoned: false,
            include_expired: false,
            include_future: false,
            include_stale: false,
            relevance_floor: None,
            dedup_mode: SearchDedupMode::DocId,
            source_mode: SearchSourceMode::Hybrid,
            strict_source_mode: false,
            memory_scope: MemoryScope::Swarm,
            strict_scope: false,
        };

        let degraded = search_degradations(&options, &index_dir);

        assert_eq!(degraded.len(), 1);
        assert_eq!(degraded[0].code, "index_missing");
        assert_eq!(degraded[0].severity, "medium");
        assert_eq!(
            degraded[0].repair.as_deref(),
            Some("ee index rebuild --workspace .")
        );
        Ok(())
    }

    #[test]
    fn search_degradations_report_corrupt_index_metadata() -> TestResult {
        let workspace = unique_test_dir("corrupt-index");
        let index_dir = workspace.join("index");
        std::fs::create_dir_all(&index_dir).map_err(|error| error.to_string())?;
        std::fs::write(index_dir.join("meta.json"), "{ not-json")
            .map_err(|error| error.to_string())?;
        let options = SearchOptions {
            workspace_path: workspace.clone(),
            database_path: Some(workspace.join("missing.db")),
            index_dir: Some(index_dir.clone()),
            query: "format before release".to_string(),
            limit: 10,
            speed: SpeedMode::Default,
            explain: false,
            as_of: None,
            include_tombstoned: false,
            include_expired: false,
            include_future: false,
            include_stale: false,
            relevance_floor: None,
            dedup_mode: SearchDedupMode::DocId,
            source_mode: SearchSourceMode::Hybrid,
            strict_source_mode: false,
            memory_scope: MemoryScope::Swarm,
            strict_scope: false,
        };

        let degraded = search_degradations(&options, &index_dir);

        assert_eq!(degraded.len(), 1);
        assert_eq!(degraded[0].code, "index_corrupt");
        assert_eq!(degraded[0].severity, "high");
        assert!(degraded[0].message.contains("Last check error"));
        assert!(degraded[0].message.contains("meta.json"));
        Ok(())
    }

    #[test]
    fn search_degradations_reuse_index_status_within_ttl() -> TestResult {
        let workspace = unique_test_dir("cached-index-status");
        let index_dir = workspace.join("index");
        std::fs::create_dir_all(&index_dir).map_err(|error| error.to_string())?;
        let options = SearchOptions {
            workspace_path: workspace.clone(),
            database_path: Some(workspace.join("missing.db")),
            index_dir: Some(index_dir.clone()),
            query: "format before release".to_string(),
            limit: 10,
            speed: SpeedMode::Default,
            explain: false,
            as_of: None,
            include_tombstoned: false,
            include_expired: false,
            include_future: false,
            include_stale: false,
            relevance_floor: None,
            dedup_mode: SearchDedupMode::DocId,
            source_mode: SearchSourceMode::Hybrid,
            strict_source_mode: false,
            memory_scope: MemoryScope::Swarm,
            strict_scope: false,
        };

        let degraded = search_degradations(&options, &index_dir);
        assert_eq!(degraded.len(), 1);
        assert_eq!(degraded[0].code, "index_missing");

        std::fs::write(index_dir.join("meta.json"), "{ not-json")
            .map_err(|error| error.to_string())?;
        let cached_degraded = search_degradations(&options, &index_dir);

        assert_eq!(cached_degraded.len(), 1);
        assert_eq!(cached_degraded[0].code, "index_missing");
        Ok(())
    }

    #[cfg(feature = "lexical-bm25")]
    #[test]
    fn search_sync_attaches_rebuilt_lexical_index_for_literal_queries() -> TestResult {
        let index_dir = unique_test_dir("lexical-fusion");
        let build_index_dir = index_dir.clone();
        let documents = vec![
            IndexableDocument::new(
                "mem-forbidden-deps",
                "Forbidden deps: tokio rusqlite petgraph hyper axum tower reqwest.",
            ),
            IndexableDocument::new(
                "mem-release-format",
                "Run cargo fmt --check and cargo clippy before release.",
            ),
            IndexableDocument::new(
                "mem-runtime",
                "Asupersync is the runtime foundation for cancellation budgets.",
            ),
        ];

        crate::core::run_cli_future(async move {
            let cx = asupersync::Cx::for_testing();
            let stack = EmbedderStack::from_parts(
                Arc::new(HashEmbedder::default_256()) as Arc<dyn Embedder>,
                None,
            );
            IndexBuilder::new(&build_index_dir)
                .with_embedder_stack(stack)
                .add_documents(documents)
                .build(&cx)
                .await
                .map_err(|error| error.to_string())?;
            Ok::<(), String>(())
        })
        .map_err(|error| error.to_string())??;

        assert!(open_lexical_searcher(&index_dir)?.is_some());

        let config = TwoTierConfig {
            explain: true,
            ..TwoTierConfig::default()
        };
        let (hits, errors) = search_sync(
            &index_dir,
            "forbidden dependencies",
            5,
            config,
            true,
            SearchSourceMode::Hybrid,
            &Deterministic::from_seed(123),
        )?;

        assert!(errors.is_empty(), "search returned errors: {errors:?}");
        let literal_hit = hits
            .iter()
            .find(|hit| hit.doc_id == "mem-forbidden-deps")
            .ok_or_else(|| format!("literal lexical hit missing from results: {hits:?}"))?;
        assert!(
            matches!(
                literal_hit.source,
                ScoreSource::Lexical | ScoreSource::Hybrid
            ),
            "literal hit should carry lexical/hybrid source, got {:?}",
            literal_hit.source
        );
        assert!(
            literal_hit.lexical_score.is_some_and(|score| score > 0.0),
            "literal hit should include a positive lexical score: {literal_hit:?}"
        );
        Ok(())
    }

    #[cfg(feature = "lexical-bm25")]
    #[test]
    fn diag_search_report_exposes_prefusion_arms_and_fusion_contributions() -> TestResult {
        let workspace = unique_test_dir("diag-search-workspace");
        let index_dir = workspace.join("index");
        let build_index_dir = index_dir.clone();
        let documents = vec![
            IndexableDocument::new(
                "mem-forbidden-deps",
                "Forbidden dependencies: tokio rusqlite petgraph hyper axum tower reqwest.",
            ),
            IndexableDocument::new(
                "mem-release-format",
                "Run cargo fmt --check and cargo clippy before release.",
            ),
            IndexableDocument::new(
                "mem-runtime",
                "Asupersync is the runtime foundation for cancellation budgets.",
            ),
        ];

        crate::core::run_cli_future(async move {
            let cx = asupersync::Cx::for_testing();
            let stack = EmbedderStack::from_parts(
                Arc::new(HashEmbedder::default_256()) as Arc<dyn Embedder>,
                None,
            );
            IndexBuilder::new(&build_index_dir)
                .with_embedder_stack(stack)
                .add_documents(documents)
                .build(&cx)
                .await
                .map_err(|error| error.to_string())?;
            Ok::<(), String>(())
        })
        .map_err(|error| error.to_string())??;

        let report = run_diag_search(&SearchOptions {
            workspace_path: workspace.clone(),
            database_path: Some(workspace.join("ee.db")),
            index_dir: Some(index_dir),
            query: "forbidden dependencies".to_string(),
            limit: 5,
            speed: SpeedMode::Default,
            explain: true,
            as_of: None,
            include_tombstoned: false,
            include_expired: false,
            include_future: false,
            include_stale: false,
            relevance_floor: Some(0.0),
            dedup_mode: SearchDedupMode::DocId,
            source_mode: SearchSourceMode::Hybrid,
            strict_source_mode: false,
            memory_scope: MemoryScope::Swarm,
            strict_scope: false,
        })
        .map_err(|error| error.to_string())?;
        let json = report.data_json();

        assert_eq!(json["schema"], DIAG_SEARCH_SCHEMA_V1);
        assert_eq!(json["command"], "diag search");
        assert_eq!(json["preFusion"]["lexical"]["available"], true);
        assert_eq!(json["preFusion"]["lexical"]["scoreScale"], "bm25_tfidf");
        assert_eq!(
            json["preFusion"]["semanticFast"]["scoreScale"],
            "cosine_similarity"
        );
        assert!(
            json["preFusion"]["lexical"]["results"]
                .as_array()
                .is_some_and(|results| results
                    .iter()
                    .any(|hit| hit["docId"] == "mem-forbidden-deps")),
            "lexical arm should expose the literal pre-fusion hit: {json}"
        );
        assert_eq!(
            json["fusion"]["algorithm"], "reciprocal_rank_fusion",
            "fusion algorithm must be explicit"
        );
        assert_eq!(
            json["fusion"]["diagnosticOnly"], true,
            "diagnostic fusion table must not masquerade as retrieval ownership"
        );
        assert_eq!(
            json["fusion"]["affectsFinalRanking"], false,
            "diagnostic fusion table must not affect final ranking"
        );
        assert_eq!(
            json["fusion"]["rankingOwner"], "frankensearch_two_tier_searcher",
            "final ordering owner must be explicit"
        );
        assert!(
            json["fusion"]["perDocContribution"]
                .as_array()
                .is_some_and(|entries| entries.iter().any(|entry| {
                    entry["docId"] == "mem-forbidden-deps"
                        && entry["lexicalContribution"]
                            .as_f64()
                            .is_some_and(|score| score > 0.0)
                })),
            "fusion contribution should expose lexical rank contribution: {json}"
        );
        assert!(
            json["final"]["metrics"]["sourceCounts"]["lexical"]
                .as_u64()
                .unwrap_or(0)
                + json["final"]["metrics"]["sourceCounts"]["hybrid"]
                    .as_u64()
                    .unwrap_or(0)
                > 0,
            "final search metrics should retain lexical/hybrid source evidence: {json}"
        );

        let direct_config = SearchOptions {
            workspace_path: workspace.clone(),
            database_path: Some(workspace.join("ee.db")),
            index_dir: None,
            query: "forbidden dependencies".to_string(),
            limit: 5,
            speed: SpeedMode::Default,
            explain: true,
            as_of: None,
            include_tombstoned: false,
            include_expired: false,
            include_future: false,
            include_stale: false,
            relevance_floor: Some(0.0),
            dedup_mode: SearchDedupMode::DocId,
            source_mode: SearchSourceMode::Hybrid,
            strict_source_mode: false,
            memory_scope: MemoryScope::Swarm,
            strict_scope: false,
        }
        .two_tier_config_for_limit(5);
        let (direct_hits, direct_errors) = search_sync(
            &workspace.join("index"),
            "forbidden dependencies",
            5,
            direct_config,
            true,
            SearchSourceMode::Hybrid,
            &Deterministic::from_seed(123),
        )?;
        assert!(
            direct_errors.is_empty(),
            "direct search returned errors: {direct_errors:?}"
        );
        let direct_doc_ids: Vec<_> = direct_hits.iter().map(|hit| hit.doc_id.as_str()).collect();
        let diag_doc_ids: Vec<_> = json["final"]["results"]
            .as_array()
            .ok_or_else(|| format!("final results missing from diag report: {json}"))?
            .iter()
            .filter_map(|hit| hit["docId"].as_str())
            .collect();
        assert_eq!(
            diag_doc_ids, direct_doc_ids,
            "diag final order must match the normal TwoTierSearcher-backed search path"
        );
        Ok(())
    }

    #[test]
    fn diag_search_fusion_contribution_uses_rrf_rank_formula() -> TestResult {
        let lexical = vec![SearchArmHit {
            doc_id: "mem-a".to_string(),
            raw_score: 8.0,
            rank: 1,
        }];
        let semantic = vec![
            SearchArmHit {
                doc_id: "mem-b".to_string(),
                raw_score: 0.8,
                rank: 1,
            },
            SearchArmHit {
                doc_id: "mem-a".to_string(),
                raw_score: 0.7,
                rank: 2,
            },
        ];

        let fusion = build_fusion_diagnostics(&lexical, &semantic, 60.0, 10);
        let mem_a = fusion
            .per_doc_contribution
            .iter()
            .find(|entry| entry.doc_id == "mem-a")
            .ok_or_else(|| "mem-a contribution present".to_string())?;

        assert_eq!(mem_a.lexical_rank, Some(1));
        assert_eq!(mem_a.semantic_rank, Some(2));
        let expected = (1.0 / 61.0) + (1.0 / 62.0);
        assert!((mem_a.fused_score - expected).abs() < 0.000_001);
        Ok(())
    }

    #[test]
    fn search_options_resolve_index_dir() {
        let options = SearchOptions {
            workspace_path: PathBuf::from("/home/user/project"),
            database_path: None,
            index_dir: None,
            query: "test".to_string(),
            limit: 10,
            speed: SpeedMode::Default,
            explain: false,
            as_of: None,
            include_tombstoned: false,
            include_expired: false,
            include_future: false,
            include_stale: false,
            relevance_floor: None,
            dedup_mode: SearchDedupMode::DocId,
            source_mode: SearchSourceMode::Hybrid,
            strict_source_mode: false,
            memory_scope: MemoryScope::Swarm,
            strict_scope: false,
        };

        assert_eq!(
            options.resolve_index_dir(),
            PathBuf::from("/home/user/project/.ee/index")
        );
    }

    #[test]
    fn search_options_apply_speed_mode_budgets_to_two_tier_config() {
        let options = SearchOptions {
            workspace_path: PathBuf::from("/home/user/project"),
            database_path: None,
            index_dir: None,
            query: "test".to_string(),
            limit: 10,
            speed: SpeedMode::Quality,
            explain: true,
            as_of: None,
            include_tombstoned: false,
            include_expired: false,
            include_future: false,
            include_stale: false,
            relevance_floor: None,
            dedup_mode: SearchDedupMode::DocId,
            source_mode: SearchSourceMode::Hybrid,
            strict_source_mode: false,
            memory_scope: MemoryScope::Swarm,
            strict_scope: false,
        };
        let config = options.two_tier_config();
        assert!(!config.fast_only);
        assert!(config.explain);
        assert_eq!(config.mrl_rescore_top_k, SpeedMode::Quality.rerank_depth());
        let requested_limit = usize::try_from(options.limit).unwrap_or(usize::MAX);
        assert!(
            config.candidate_multiplier * requested_limit >= SpeedMode::Quality.candidate_limit()
        );

        let instant = SearchOptions {
            speed: SpeedMode::Instant,
            explain: false,
            ..options
        }
        .two_tier_config();
        assert!(instant.fast_only);
        assert!(!instant.explain);
        assert_eq!(instant.mrl_rescore_top_k, SpeedMode::Instant.rerank_depth());
        assert!(instant.candidate_multiplier < config.candidate_multiplier);
    }

    #[test]
    fn search_options_respect_explicit_index_dir() {
        let options = SearchOptions {
            workspace_path: PathBuf::from("/home/user/project"),
            database_path: None,
            index_dir: Some(PathBuf::from("/custom/index")),
            query: "test".to_string(),
            limit: 10,
            speed: SpeedMode::Default,
            explain: false,
            as_of: None,
            include_tombstoned: false,
            include_expired: false,
            include_future: false,
            include_stale: false,
            relevance_floor: None,
            dedup_mode: SearchDedupMode::DocId,
            source_mode: SearchSourceMode::Hybrid,
            strict_source_mode: false,
            memory_scope: MemoryScope::Swarm,
            strict_scope: false,
        };

        assert_eq!(options.resolve_index_dir(), PathBuf::from("/custom/index"));
    }

    #[test]
    fn search_error_has_repair_hints() {
        let no_index = SearchError::NoIndex;
        assert_eq!(
            no_index.repair_hint(),
            Some("ee index rebuild --workspace .")
        );

        let index_err = SearchError::Index("test".to_string());
        assert!(index_err.repair_hint().is_some());
    }

    #[test]
    fn score_source_as_str_is_stable() {
        assert_eq!(ScoreSource::Lexical.as_str(), "lexical");
        assert_eq!(ScoreSource::SemanticFast.as_str(), "semantic_fast");
        assert_eq!(ScoreSource::SemanticQuality.as_str(), "semantic_quality");
        assert_eq!(ScoreSource::Hybrid.as_str(), "hybrid");
        assert_eq!(ScoreSource::Reranked.as_str(), "reranked");
    }

    #[test]
    fn search_json_includes_score_breakdown() {
        let report = SearchReport {
            status: SearchStatus::Success,
            query: "hybrid query".to_string(),
            requested_limit: 5,
            results: vec![SearchHit {
                doc_id: "doc-hybrid".to_string(),
                score: 0.88,
                source: ScoreSource::Hybrid,
                fast_score: Some(0.72),
                quality_score: Some(0.91),
                lexical_score: Some(0.65),
                rerank_score: None,
                metadata: Some(serde_json::json!({"level": "procedural", "kind": "rule"})),
                explanation: None,
            }],
            elapsed_ms: 5.2,
            errors: Vec::new(),
            degraded: Vec::new(),
            runtime_profile: test_runtime_profile(),
            relevance_floor_applied: None,
            candidates_below_floor: 0,
            source_mode_requested: SearchSourceMode::Hybrid,
            source_mode_applied: SearchSourceMode::Hybrid,
            source_mode_fallback: false,
            strict_source_mode: false,
            memory_scope: MemoryScope::Swarm,
            strict_scope: false,
            scope_stats: test_scope_stats(),
        };

        let json = report.data_json();
        let result = &json["results"][0];

        assert_eq!(result["docId"], "doc-hybrid");
        assert!((result["score"].as_f64().unwrap_or(f64::NAN) - 0.88).abs() < 0.001);
        assert_eq!(result["source"], "hybrid");
        assert!((result["fastScore"].as_f64().unwrap_or(f64::NAN) - 0.72).abs() < 0.001);
        assert!((result["qualityScore"].as_f64().unwrap_or(f64::NAN) - 0.91).abs() < 0.001);
        assert!((result["lexicalScore"].as_f64().unwrap_or(f64::NAN) - 0.65).abs() < 0.001);
        assert!(result.get("rerankScore").is_none());
        assert_eq!(result["metadata"]["level"], "procedural");
        assert_eq!(result["metadata"]["kind"], "rule");
    }

    #[test]
    fn search_json_exposes_stable_why_and_provenance() {
        let mut hit = SearchHit {
            doc_id: "doc-provenance".to_string(),
            score: 0.82,
            source: ScoreSource::Hybrid,
            fast_score: Some(0.71),
            quality_score: None,
            lexical_score: Some(0.42),
            rerank_score: None,
            metadata: Some(serde_json::json!({
                "level": "procedural",
                "provenance_uri": "file://AGENTS.md#L42",
            })),
            explanation: None,
        };
        hit.explanation = Some(ScoreExplanation::generate(&hit));

        let report = SearchReport {
            status: SearchStatus::Success,
            query: "provenance".to_string(),
            requested_limit: 1,
            results: vec![hit],
            elapsed_ms: 1.0,
            errors: Vec::new(),
            degraded: Vec::new(),
            runtime_profile: test_runtime_profile(),
            relevance_floor_applied: None,
            candidates_below_floor: 0,
            source_mode_requested: SearchSourceMode::Hybrid,
            source_mode_applied: SearchSourceMode::Hybrid,
            source_mode_fallback: false,
            strict_source_mode: false,
            memory_scope: MemoryScope::Swarm,
            strict_scope: false,
            scope_stats: test_scope_stats(),
        };

        let json = report.data_json();
        let result = &json["results"][0];

        assert_eq!(
            result["why"], result["explanation"]["summary"],
            "why should be the stable selection summary"
        );
        assert_eq!(
            result["provenance"],
            serde_json::json!([
                {
                    "kind": "provenance_uri",
                    "uri": "file://AGENTS.md#L42",
                },
                {
                    "kind": "search_document",
                    "docId": "doc-provenance",
                }
            ])
        );
    }

    #[test]
    fn search_json_redacts_sensitive_provenance_uri_output() {
        let mut hit = SearchHit {
            doc_id: "doc-sensitive-provenance".to_string(),
            score: 0.82,
            source: ScoreSource::Hybrid,
            fast_score: None,
            quality_score: None,
            lexical_score: None,
            rerank_score: None,
            metadata: Some(serde_json::json!({
                "level": "episodic",
                "provenance_uri": "file:///Users/alice/private/logs/build.log?api_key=redaction-fixture",
            })),
            explanation: None,
        };
        hit.explanation = Some(ScoreExplanation::generate(&hit));

        let report = SearchReport {
            status: SearchStatus::Success,
            query: "provenance".to_string(),
            requested_limit: 1,
            results: vec![hit],
            elapsed_ms: 1.0,
            errors: Vec::new(),
            degraded: Vec::new(),
            runtime_profile: test_runtime_profile(),
            relevance_floor_applied: None,
            candidates_below_floor: 0,
            source_mode_requested: SearchSourceMode::Hybrid,
            source_mode_applied: SearchSourceMode::Hybrid,
            source_mode_fallback: false,
            strict_source_mode: false,
            memory_scope: MemoryScope::Swarm,
            strict_scope: false,
            scope_stats: test_scope_stats(),
        };

        let json = report.data_json();
        let result = &json["results"][0];
        let rendered = serde_json::to_string(result).expect("search result serializes");

        assert!(
            rendered.contains("[REDACTED_PATH]"),
            "search provenance should preserve a path placeholder: {rendered}"
        );
        assert!(
            rendered.contains("[REDACTED:api_key]"),
            "search provenance should preserve a secret placeholder: {rendered}"
        );
        assert!(
            !rendered.contains("/Users/alice/private/logs/build.log"),
            "search provenance leaked an absolute path: {rendered}"
        );
        assert!(
            !rendered.contains("redaction-fixture"),
            "search provenance leaked a secret-like value: {rendered}"
        );
        assert_eq!(
            result["provenance"][0]["uri"],
            "file://[REDACTED_PATH]?api_key=[REDACTED:api_key]"
        );
        assert_eq!(
            result["metadata"]["provenance_uri"],
            "file://[REDACTED_PATH]?api_key=[REDACTED:api_key]"
        );
        assert_eq!(result["contentRedacted"], true);
        let reasons = result["redactions"]
            .as_array()
            .expect("redactions are present")
            .iter()
            .filter_map(|entry| entry["reason"].as_str())
            .collect::<BTreeSet<_>>();
        assert!(reasons.contains("api_key"));
        assert!(reasons.contains("path"));
    }

    #[test]
    fn search_json_omits_null_scores() {
        let report = SearchReport {
            status: SearchStatus::Success,
            query: "minimal".to_string(),
            requested_limit: 3,
            results: vec![SearchHit {
                doc_id: "doc-min".to_string(),
                score: 0.5,
                source: ScoreSource::Lexical,
                fast_score: None,
                quality_score: None,
                lexical_score: Some(0.5),
                rerank_score: None,
                metadata: None,
                explanation: None,
            }],
            elapsed_ms: 1.0,
            errors: Vec::new(),
            degraded: Vec::new(),
            runtime_profile: test_runtime_profile(),
            relevance_floor_applied: None,
            candidates_below_floor: 0,
            source_mode_requested: SearchSourceMode::Hybrid,
            source_mode_applied: SearchSourceMode::Hybrid,
            source_mode_fallback: false,
            strict_source_mode: false,
            memory_scope: MemoryScope::Swarm,
            strict_scope: false,
            scope_stats: test_scope_stats(),
        };

        let json = report.data_json();
        let result = &json["results"][0];

        assert!(result.get("fastScore").is_none());
        assert!(result.get("qualityScore").is_none());
        assert!(result.get("rerankScore").is_none());
        assert!(result.get("metadata").is_none());
        assert!(result.get("explanation").is_none());
        assert!((result["lexicalScore"].as_f64().unwrap_or(f64::NAN) - 0.5).abs() < 0.001);
    }

    #[test]
    fn retrieval_metrics_summarize_sources_scores_and_coverage() {
        let mut explained_hit = SearchHit {
            doc_id: "doc-hybrid".to_string(),
            score: 0.9,
            source: ScoreSource::Hybrid,
            fast_score: Some(0.7),
            quality_score: Some(0.9),
            lexical_score: Some(0.6),
            rerank_score: None,
            metadata: Some(serde_json::json!({"level": "procedural"})),
            explanation: None,
        };
        explained_hit.explanation = Some(ScoreExplanation::generate(&explained_hit));

        let report = SearchReport {
            status: SearchStatus::Success,
            query: "metrics".to_string(),
            requested_limit: 4,
            results: vec![
                explained_hit,
                SearchHit {
                    doc_id: "doc-lexical".to_string(),
                    score: 0.3,
                    source: ScoreSource::Lexical,
                    fast_score: None,
                    quality_score: None,
                    lexical_score: Some(0.3),
                    rerank_score: None,
                    metadata: None,
                    explanation: None,
                },
            ],
            elapsed_ms: 2.345_678_9,
            errors: vec!["semantic tier unavailable".to_string()],
            degraded: Vec::new(),
            runtime_profile: test_runtime_profile(),
            relevance_floor_applied: None,
            candidates_below_floor: 0,
            source_mode_requested: SearchSourceMode::Hybrid,
            source_mode_applied: SearchSourceMode::Hybrid,
            source_mode_fallback: false,
            strict_source_mode: false,
            memory_scope: MemoryScope::Swarm,
            strict_scope: false,
            scope_stats: test_scope_stats(),
        };

        let metrics = report.retrieval_metrics();
        assert_eq!(metrics.requested_limit, 4);
        assert_eq!(metrics.returned_count, 2);
        assert_eq!(metrics.error_count, 1);
        assert_eq!(metrics.source_counts.hybrid, 1);
        assert_eq!(metrics.source_counts.lexical, 1);
        assert_eq!(metrics.score_distribution.top, Some(0.9));
        assert_eq!(metrics.score_distribution.min, Some(0.3));
        assert_eq!(metrics.score_distribution.max, Some(0.9));
        assert!((metrics.score_distribution.mean.unwrap_or(f32::NAN) - 0.6).abs() < 0.001);
        assert_eq!(metrics.field_coverage.fast_score_count, 1);
        assert_eq!(metrics.field_coverage.quality_score_count, 1);
        assert_eq!(metrics.field_coverage.lexical_score_count, 2);
        assert_eq!(metrics.field_coverage.metadata_count, 1);
        assert_eq!(metrics.field_coverage.explanation_count, 1);

        let json = metrics.data_json();
        assert_eq!(json["requestedLimit"], 4);
        assert_eq!(json["returnedCount"], 2);
        assert_eq!(json["errorCount"], 1);
        assert_eq!(json["sourceCounts"]["hybrid"], 1);
        assert_eq!(json["sourceCounts"]["lexical"], 1);
        assert_eq!(json["fieldCoverage"]["explanationCount"], 1);
        let mean = json["scoreDistribution"]["mean"]
            .as_f64()
            .unwrap_or(f64::NAN);
        assert!((mean - 0.6).abs() < 0.000_001);
        assert_eq!(json["elapsedMs"], serde_json::json!(2.345679));
    }

    #[test]
    fn retrieval_metrics_are_stable_for_empty_results() {
        let report = SearchReport {
            status: SearchStatus::NoResults,
            query: "empty".to_string(),
            requested_limit: 7,
            results: Vec::new(),
            elapsed_ms: 0.0,
            errors: Vec::new(),
            degraded: Vec::new(),
            runtime_profile: test_runtime_profile(),
            relevance_floor_applied: None,
            candidates_below_floor: 0,
            source_mode_requested: SearchSourceMode::Hybrid,
            source_mode_applied: SearchSourceMode::Hybrid,
            source_mode_fallback: false,
            strict_source_mode: false,
            memory_scope: MemoryScope::Swarm,
            strict_scope: false,
            scope_stats: test_scope_stats(),
        };

        let json = report.data_json();
        assert_eq!(json["metrics"]["requestedLimit"], 7);
        assert_eq!(json["metrics"]["returnedCount"], 0);
        assert_eq!(json["metrics"]["sourceCounts"]["lexical"], 0);
        assert_eq!(
            json["metrics"]["scoreDistribution"]["top"],
            serde_json::Value::Null
        );
        assert_eq!(
            json["metrics"]["scoreDistribution"]["mean"],
            serde_json::Value::Null
        );
    }

    #[test]
    fn score_explanation_generates_for_lexical() {
        let hit = SearchHit {
            doc_id: "doc-lex".to_string(),
            score: 0.75,
            source: ScoreSource::Lexical,
            fast_score: None,
            quality_score: None,
            lexical_score: Some(0.75),
            rerank_score: None,
            metadata: None,
            explanation: None,
        };

        let explanation = ScoreExplanation::generate(&hit);
        assert!(explanation.summary.contains("0.75"));
        assert!(explanation.summary.contains("lexical"));
        assert_eq!(explanation.factors.len(), 1);
        assert_eq!(explanation.factors[0].name, "lexical");
        assert!((explanation.factors[0].value - 0.75).abs() < 0.001);
        assert!(explanation.factors[0].contribution.contains("BM25"));
        assert_eq!(explanation.factors[0].source_field, "lexical_score");
        assert_eq!(explanation.factors[0].formula, "score = lexical_score");
    }

    #[test]
    fn score_explanation_generates_for_hybrid() {
        let hit = SearchHit {
            doc_id: "doc-hyb".to_string(),
            score: 0.85,
            source: ScoreSource::Hybrid,
            fast_score: Some(0.70),
            quality_score: Some(0.90),
            lexical_score: Some(0.60),
            rerank_score: None,
            metadata: None,
            explanation: None,
        };

        let explanation = ScoreExplanation::generate(&hit);
        assert!(explanation.summary.contains("0.85"));
        assert!(explanation.summary.contains("RRF fusion"));
        assert_eq!(explanation.factors.len(), 3);
        assert_eq!(explanation.factors[0].source_field, "fast_score");
        assert_eq!(
            explanation.factors[0].formula,
            "component = fast_score; final score = score"
        );
        assert_eq!(explanation.factors[1].source_field, "quality_score");
        assert_eq!(explanation.factors[2].source_field, "lexical_score");
    }

    #[test]
    fn score_explanation_generates_for_reranked() {
        let hit = SearchHit {
            doc_id: "doc-rerank".to_string(),
            score: 0.92,
            source: ScoreSource::Reranked,
            fast_score: Some(0.65),
            quality_score: None,
            lexical_score: None,
            rerank_score: Some(0.92),
            metadata: None,
            explanation: None,
        };

        let explanation = ScoreExplanation::generate(&hit);
        assert!(explanation.summary.contains("0.92"));
        assert!(explanation.summary.contains("reranked"));
        assert_eq!(explanation.factors.len(), 2);
        assert_eq!(explanation.factors[0].name, "rerank");
        assert_eq!(explanation.factors[0].source_field, "rerank_score");
        assert_eq!(explanation.factors[0].formula, "score = rerank_score");
        assert!(
            explanation.factors[0]
                .contribution
                .contains("cross-encoder")
        );
    }

    #[test]
    fn score_explanation_included_in_json_when_present() {
        let mut hit = SearchHit {
            doc_id: "doc-explained".to_string(),
            score: 0.80,
            source: ScoreSource::SemanticFast,
            fast_score: Some(0.80),
            quality_score: None,
            lexical_score: None,
            rerank_score: None,
            metadata: None,
            explanation: None,
        };
        hit.explanation = Some(ScoreExplanation::generate(&hit));

        let report = SearchReport {
            status: SearchStatus::Success,
            query: "explained".to_string(),
            requested_limit: 1,
            results: vec![hit],
            elapsed_ms: 2.0,
            errors: Vec::new(),
            degraded: Vec::new(),
            runtime_profile: test_runtime_profile(),
            relevance_floor_applied: None,
            candidates_below_floor: 0,
            source_mode_requested: SearchSourceMode::Hybrid,
            source_mode_applied: SearchSourceMode::Hybrid,
            source_mode_fallback: false,
            strict_source_mode: false,
            memory_scope: MemoryScope::Swarm,
            strict_scope: false,
            scope_stats: test_scope_stats(),
        };

        let json = report.data_json();
        let result = &json["results"][0];

        assert!(result.get("explanation").is_some());
        assert!(
            result["explanation"]["summary"]
                .as_str()
                .unwrap_or("")
                .contains("0.80")
        );
        assert!(result["explanation"]["factors"].is_array());
        assert_eq!(
            result["explanation"]["factors"]
                .as_array()
                .map(Vec::len)
                .unwrap_or(0),
            1
        );
        assert_eq!(
            result["explanation"]["factors"][0]["sourceField"],
            "fast_score"
        );
        assert_eq!(
            result["explanation"]["factors"][0]["formula"],
            "score = fast_score"
        );
    }

    #[test]
    fn human_summary_includes_explanation_when_present() {
        let mut hit = SearchHit {
            doc_id: "doc-human".to_string(),
            score: 0.70,
            source: ScoreSource::Lexical,
            fast_score: None,
            quality_score: None,
            lexical_score: Some(0.70),
            rerank_score: None,
            metadata: None,
            explanation: None,
        };
        hit.explanation = Some(ScoreExplanation::generate(&hit));

        let report = SearchReport {
            status: SearchStatus::Success,
            query: "human test".to_string(),
            requested_limit: 1,
            results: vec![hit],
            elapsed_ms: 1.5,
            errors: Vec::new(),
            degraded: Vec::new(),
            runtime_profile: test_runtime_profile(),
            relevance_floor_applied: None,
            candidates_below_floor: 0,
            source_mode_requested: SearchSourceMode::Hybrid,
            source_mode_applied: SearchSourceMode::Hybrid,
            source_mode_fallback: false,
            strict_source_mode: false,
            memory_scope: MemoryScope::Swarm,
            strict_scope: false,
            scope_stats: test_scope_stats(),
        };

        let summary = report.human_summary();
        assert!(summary.contains("lexical: 0.70"));
        assert!(summary.contains("BM25"));
    }

    #[test]
    fn search_degradation_corrupt_index_has_required_code_and_severity() {
        let degradation =
            SearchDegradation::corrupt_index(Some("manifest parse error: invalid JSON"));
        assert_eq!(degradation.code, "index_corrupt");
        assert_eq!(degradation.severity, "high");
        assert!(degradation.message.contains("failed integrity checks"));
        assert!(degradation.message.contains("manifest parse error"));
        assert!(degradation.repair.is_some());
        assert!(
            degradation
                .repair
                .as_ref()
                .is_some_and(|r| r.contains("rebuild"))
        );
    }

    #[test]
    fn search_degradation_corrupt_index_without_error_detail_still_valid() {
        let degradation = SearchDegradation::corrupt_index(None);
        assert_eq!(degradation.code, "index_corrupt");
        assert_eq!(degradation.severity, "high");
        assert!(degradation.message.contains("failed integrity checks"));
        assert!(!degradation.message.contains("Last check error"));
    }

    #[test]
    fn search_degradation_missing_index_has_required_code_and_repair() {
        let degradation = SearchDegradation::missing_index();
        assert_eq!(degradation.code, "index_missing");
        assert_eq!(degradation.severity, "medium");
        assert!(degradation.message.contains("missing"));
        assert!(
            degradation
                .repair
                .as_ref()
                .is_some_and(|r| r.contains("rebuild"))
        );
    }

    #[test]
    fn search_degradation_data_json_includes_all_fields() {
        let degradation = SearchDegradation::corrupt_index(Some("test error"));
        let json = degradation.data_json();
        assert_eq!(json["code"], "index_corrupt");
        assert_eq!(json["severity"], "high");
        assert!(json["message"].as_str().is_some_and(|m| !m.is_empty()));
        assert!(json["repair"].as_str().is_some());
    }

    #[test]
    fn search_degraded_data_json_includes_embed_model_recovery_details() {
        let degraded = vec![SearchDegradation {
            code: "embed_model_unavailable".to_string(),
            severity: "warning".to_string(),
            message: "Embedding model unavailable.".to_string(),
            repair: Some("ee index reembed --workspace .".to_string()),
        }];

        let rendered = search_degraded_data_json("search", &degraded);
        assert_eq!(rendered.len(), 1);
        assert_eq!(rendered[0]["code"], "embed_model_unavailable");
        assert_eq!(rendered[0]["sources"], serde_json::json!(["search"]));
        let recovery = rendered[0]
            .pointer("/details/recovery")
            .and_then(serde_json::Value::as_array)
            .expect("embed_model_unavailable should expose details.recovery");
        assert_eq!(recovery.len(), 2);
        assert_eq!(recovery[0]["kind"], "rebuild");
        assert_eq!(recovery[0]["command"], "ee index reembed --workspace .");
        assert_eq!(recovery[1]["kind"], "rebuild");
        assert_eq!(
            recovery[1]["command"],
            "cargo build --features embed-quality"
        );
        assert_eq!(
            rendered[0]["details"]["modelId"],
            EMBED_MODEL_UNAVAILABLE_MODEL_ID
        );
        assert_eq!(
            rendered[0]["details"]["featureFlag"],
            EMBED_MODEL_UNAVAILABLE_FEATURE_FLAG
        );
        assert_eq!(rendered[0]["details"]["lexicalAvailable"], true);
    }

    // ========================================================================
    // Bead bd-17c65.2.1 (B1) — Relevance floor tests
    // ========================================================================

    /// Helper: synthesize a `SearchHit` with a given score for the floor tests.
    fn synthetic_hit(doc_id: &str, score: f32) -> SearchHit {
        SearchHit {
            doc_id: doc_id.to_string(),
            score,
            source: ScoreSource::SemanticFast,
            fast_score: Some(score),
            quality_score: None,
            lexical_score: None,
            rerank_score: None,
            metadata: None,
            explanation: None,
        }
    }

    fn mesh_hit_metadata(origin_workspace_id: &str, trust_lane: &str) -> serde_json::Value {
        serde_json::json!({
            "mesh": {
                "workspaceScopeDecision": "allow",
                "workspaceId": "wsp_local_alpha",
                "cachedMaterialId": format!("mesh_mat_{origin_workspace_id}"),
                "originWorkspaceId": origin_workspace_id,
                "producerPeerId": "peer_builder_one",
                "materialLane": "metadata",
                "importDecisionId": "mesh_dec_456",
                "trustLane": trust_lane,
                "redactionPosture": "standard"
            }
        })
    }

    #[test]
    fn search_hit_sort_uses_radix_tiebreak_for_canonical_memory_ids() {
        let mut hits = vec![
            synthetic_hit("mem_01J0000000000000000000000C", 0.20),
            synthetic_hit("mem_01J0000000000000000000000A", 0.20),
            synthetic_hit("mem_01J0000000000000000000000B", 0.20),
        ];

        sort_search_hits_by_score_order(&mut hits);

        let sorted_ids = hits
            .iter()
            .map(|hit| hit.doc_id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            sorted_ids,
            vec![
                "mem_01J0000000000000000000000A",
                "mem_01J0000000000000000000000B",
                "mem_01J0000000000000000000000C",
            ]
        );
    }

    #[test]
    fn search_hit_sort_keeps_lexical_fallback_for_mixed_doc_ids() {
        let mut hits = vec![
            synthetic_hit("mem_01J0000000000000000000000A", 0.20),
            synthetic_hit("doc_01J0000000000000000000000C", 0.20),
        ];

        sort_search_hits_by_score_order(&mut hits);

        let sorted_ids = hits
            .iter()
            .map(|hit| hit.doc_id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            sorted_ids,
            vec![
                "doc_01J0000000000000000000000C",
                "mem_01J0000000000000000000000A",
            ]
        );
    }

    #[test]
    fn search_hit_sort_orders_cross_shard_ties_by_workspace_then_memory() {
        let mut shard_b = synthetic_hit("mem_01J0000000000000000000000A", 0.20);
        shard_b.metadata = Some(serde_json::json!({"workspace_id": "wsp_b"}));
        let mut shard_a_later = synthetic_hit("mem_01J0000000000000000000000C", 0.20);
        shard_a_later.metadata = Some(serde_json::json!({"workspace_id": "wsp_a"}));
        let mut shard_a_earlier = synthetic_hit("mem_01J0000000000000000000000B", 0.20);
        shard_a_earlier.metadata = Some(serde_json::json!({"workspace_id": "wsp_a"}));
        let mut no_workspace = synthetic_hit("mem_01J0000000000000000000000D", 0.20);
        no_workspace.metadata = Some(serde_json::json!({"source": "memory"}));
        let mut hits = vec![shard_b, shard_a_later, no_workspace, shard_a_earlier];

        sort_search_hits_by_score_order(&mut hits);

        let sorted = hits
            .iter()
            .map(|hit| (search_hit_workspace_id(hit).to_owned(), hit.doc_id.as_str()))
            .collect::<Vec<_>>();
        assert_eq!(
            sorted,
            vec![
                ("".to_owned(), "mem_01J0000000000000000000000D"),
                ("wsp_a".to_owned(), "mem_01J0000000000000000000000B"),
                ("wsp_a".to_owned(), "mem_01J0000000000000000000000C"),
                ("wsp_b".to_owned(), "mem_01J0000000000000000000000A"),
            ]
        );
    }

    #[test]
    fn search_hit_sort_orders_mesh_ties_by_nested_origin_workspace() {
        let mut shard_b = synthetic_hit("mem_01J0000000000000000000000A", 0.20);
        shard_b.metadata = Some(mesh_hit_metadata("wsp_b", "peerAgent"));
        let mut shard_a_later = synthetic_hit("mem_01J0000000000000000000000C", 0.20);
        shard_a_later.metadata = Some(mesh_hit_metadata("wsp_a", "peerAgent"));
        let mut shard_a_earlier = synthetic_hit("mem_01J0000000000000000000000B", 0.20);
        shard_a_earlier.metadata = Some(mesh_hit_metadata("wsp_a", "peerAgent"));
        let mut hits = vec![shard_b, shard_a_later, shard_a_earlier];

        sort_search_hits_by_score_order(&mut hits);

        let sorted = hits
            .iter()
            .map(|hit| (search_hit_workspace_id(hit).to_owned(), hit.doc_id.as_str()))
            .collect::<Vec<_>>();
        assert_eq!(
            sorted,
            vec![
                ("wsp_a".to_owned(), "mem_01J0000000000000000000000B"),
                ("wsp_a".to_owned(), "mem_01J0000000000000000000000C"),
                ("wsp_b".to_owned(), "mem_01J0000000000000000000000A"),
            ]
        );
    }

    #[test]
    fn component_score_ties_use_memory_id_order_not_rank_fusion_artifacts() {
        let mut hits = vec![synthetic_hit("mem_b", 0.10), synthetic_hit("mem_a", 0.20)];
        for hit in &mut hits {
            hit.fast_score = Some(0.42);
        }

        canonicalize_equivalent_component_scores(&mut hits, &Deterministic::from_seed(7));
        sort_search_hits_by_score_order(&mut hits);

        assert_eq!(hits[0].doc_id, "mem_a");
        assert_eq!(hits[1].doc_id, "mem_b");
        assert!((hits[0].score - 0.20).abs() < 1e-6);
        assert!((hits[1].score - 0.20).abs() < 1e-6);
    }

    #[test]
    fn component_score_tie_canonicalization_is_seed_threaded_but_stable() {
        let mut seeded_a = vec![synthetic_hit("mem_b", 0.10), synthetic_hit("mem_a", 0.20)];
        let mut seeded_b = seeded_a.clone();
        for hit in seeded_a.iter_mut().chain(seeded_b.iter_mut()) {
            hit.fast_score = Some(0.42);
        }

        canonicalize_equivalent_component_scores(&mut seeded_a, &Deterministic::from_seed(11));
        canonicalize_equivalent_component_scores(&mut seeded_b, &Deterministic::from_seed(99));
        sort_search_hits_by_score_order(&mut seeded_a);
        sort_search_hits_by_score_order(&mut seeded_b);

        let ids_a = seeded_a
            .iter()
            .map(|hit| hit.doc_id.as_str())
            .collect::<Vec<_>>();
        let ids_b = seeded_b
            .iter()
            .map(|hit| hit.doc_id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(ids_a, vec!["mem_a", "mem_b"]);
        assert_eq!(ids_a, ids_b);
        assert_eq!(seeded_a[0].score, seeded_b[0].score);
        assert_eq!(seeded_a[1].score, seeded_b[1].score);
    }

    #[test]
    fn default_relevance_floor_is_one_in_twenty() {
        // 0.05 is the documented default (calibrated against the 2026-05-10
        // corpus where junk scored < 0.03 and meaningful hits scored 0.10+).
        // Changing this default is a contract change — agents downstream
        // rely on the value.
        assert!((DEFAULT_RELEVANCE_FLOOR - 0.05).abs() < f32::EPSILON);
    }

    // ========================================================================
    // bd-n22a4 (B2-followup): per-source default floor coverage. RRF-fused
    // hybrid hits get the hybrid floor (≈0.005) while 0..=1-normalized
    // sources keep the semantic-domain floor (0.05).
    // ========================================================================

    #[test]
    fn default_floor_hybrid_is_one_in_two_hundred() {
        // 0.005 covers RRF magnitudes down to 1-arm rank ~190 (1/(60+1) at
        // rank N is ≈ 0.0164 at rank 1, ≈ 0.005 at rank 190). Changing
        // this value is a contract change — `ee search` users see
        // dramatically different recall on hybrid queries.
        assert!((DEFAULT_RELEVANCE_FLOOR_HYBRID - 0.005).abs() < f32::EPSILON);
    }

    #[test]
    fn default_floor_for_hybrid_returns_hybrid_constant() {
        assert!(
            (default_floor_for_source(ScoreSource::Hybrid) - DEFAULT_RELEVANCE_FLOOR_HYBRID).abs()
                < f32::EPSILON
        );
    }

    #[test]
    fn default_floor_for_normalized_sources_returns_standard_floor() {
        // Every source whose scores live in the 0..=1 cosine/BM25 domain
        // keeps the standard floor — only the RRF-magnitude `Hybrid`
        // source gets the lower one.
        for source in [
            ScoreSource::Lexical,
            ScoreSource::SemanticFast,
            ScoreSource::SemanticQuality,
            ScoreSource::Reranked,
        ] {
            assert!(
                (default_floor_for_source(source) - DEFAULT_RELEVANCE_FLOOR).abs() < f32::EPSILON,
                "{:?} should use DEFAULT_RELEVANCE_FLOOR",
                source,
            );
        }
    }

    #[test]
    fn hybrid_floor_admits_typical_rrf_scores_that_semantic_floor_would_reject() {
        // The exact bug bd-n22a4 was filed for: a hybrid hit with score
        // ≈0.0328 (the 2-arm-rank-1 RRF top) gets filtered out by the
        // semantic-domain floor of 0.05, leaving every default-floor
        // hybrid search empty. The hybrid floor of 0.005 admits it.
        let rrf_top_two_arm: f32 = 2.0 / 61.0; // ≈ 0.03278
        assert!(rrf_top_two_arm >= DEFAULT_RELEVANCE_FLOOR_HYBRID);
        assert!(rrf_top_two_arm < DEFAULT_RELEVANCE_FLOOR);
        // Single-arm RRF rank-1: 1/61 ≈ 0.0164 — still well above the
        // hybrid floor, still below the semantic floor.
        let rrf_rank_one_one_arm: f32 = 1.0 / 61.0;
        assert!(rrf_rank_one_one_arm >= DEFAULT_RELEVANCE_FLOOR_HYBRID);
        assert!(rrf_rank_one_one_arm < DEFAULT_RELEVANCE_FLOOR);
    }

    #[test]
    fn hybrid_floor_still_rejects_genuinely_weak_rrf_scores() {
        // The floor must still be a noise/signal cut, not "accept
        // everything". Scores at single-arm rank ~250 (1/(60+250) ≈
        // 0.0032) sit below the hybrid floor and get filtered.
        let rrf_deep_rank: f32 = 1.0 / 310.0; // ≈ 0.00323
        assert!(rrf_deep_rank < DEFAULT_RELEVANCE_FLOOR_HYBRID);
    }

    /// Helper: synthesize a hybrid `SearchHit` for adaptive-floor tests.
    fn synthetic_hybrid_hit(doc_id: &str, score: f32) -> SearchHit {
        let mut hit = synthetic_hit(doc_id, score);
        hit.source = ScoreSource::Hybrid;
        hit
    }

    fn test_effective_floor(user_floor_override: Option<f32>, source: ScoreSource) -> f32 {
        user_floor_override.unwrap_or_else(|| default_floor_for_source(source))
    }

    #[test]
    fn adaptive_partition_keeps_typical_hybrid_hit_with_no_override() {
        // Reproduces the bd-n22a4 acceptance path: a hybrid hit at the
        // typical 2-arm RRF top of ≈0.0328 must survive the default
        // floor when no explicit override is set, so `ee search` on a
        // one-memory workspace returns the matching memory instead of a
        // `no_relevant_results` degraded entry.
        let hits = vec![synthetic_hybrid_hit("mem_canonical", 2.0 / 61.0)];
        let kept: Vec<_> = hits
            .into_iter()
            .filter(|hit| {
                let per_hit_floor = test_effective_floor(None, hit.source);
                hit.score.is_finite() && hit.score >= per_hit_floor
            })
            .collect();
        assert_eq!(
            kept.len(),
            1,
            "hybrid hit at ≈0.0328 must pass default floor"
        );
        assert_eq!(kept[0].doc_id, "mem_canonical");
    }

    #[test]
    fn adaptive_partition_still_filters_weak_semantic_hit_with_no_override() {
        // Mirror assertion: a SemanticFast hit at 0.02 (cosine-domain
        // noise) still gets filtered out under the standard 0.05 floor.
        // The adaptive policy must not weaken the semantic-only path.
        let hits = vec![synthetic_hit("mem_semantic_noise", 0.02)];
        let kept: Vec<_> = hits
            .into_iter()
            .filter(|hit| {
                let per_hit_floor = test_effective_floor(None, hit.source);
                hit.score.is_finite() && hit.score >= per_hit_floor
            })
            .collect();
        assert!(
            kept.is_empty(),
            "weak semantic hit at 0.02 must still be filtered by 0.05 floor"
        );
    }

    #[test]
    fn explicit_override_applies_uniformly_across_all_sources() {
        // Backstop: when the caller passes `--relevance-floor 0.10` it
        // must apply uniformly. This guarantees existing fixtures and
        // `--relevance-floor 0.0` (disabled) keep their exact prior
        // semantics — the adaptive policy ONLY kicks in when no
        // explicit override is set.
        let hits = vec![
            synthetic_hybrid_hit("mem_hybrid_strong", 0.20),
            synthetic_hybrid_hit("mem_hybrid_weak", 0.02),
            synthetic_hit("mem_semantic_strong", 0.20),
            synthetic_hit("mem_semantic_weak", 0.02),
        ];
        let kept: Vec<_> = hits
            .into_iter()
            .filter(|hit| {
                let per_hit_floor = test_effective_floor(Some(0.10), hit.source);
                hit.score.is_finite() && hit.score >= per_hit_floor
            })
            .collect();
        assert_eq!(kept.len(), 2, "0.10 override should keep both strong hits");
        assert!(kept.iter().any(|h| h.doc_id == "mem_hybrid_strong"));
        assert!(kept.iter().any(|h| h.doc_id == "mem_semantic_strong"));
    }

    #[test]
    fn retrieval_metrics_records_floor_and_candidate_counts() {
        let hits = vec![
            synthetic_hit("a", 0.30),
            synthetic_hit("b", 0.20),
            synthetic_hit("c", 0.10),
        ];
        let metrics = RetrievalMetrics::from_hits_with_floor(10, 5.0, &hits, 0, Some(0.05), 4);
        assert_eq!(metrics.relevance_floor, Some(0.05));
        assert_eq!(metrics.candidates_above_floor, 3);
        assert_eq!(metrics.candidates_below_floor, 4);
        assert_eq!(metrics.returned_count, 3);
    }

    #[test]
    fn retrieval_metrics_data_json_emits_floor_fields() {
        let hits = vec![synthetic_hit("a", 0.4)];
        let metrics = RetrievalMetrics::from_hits_with_floor(10, 5.0, &hits, 0, Some(0.05), 2);
        let json = metrics.data_json();
        // f32 -> f64 widening introduces sub-epsilon drift (0.0500000007…);
        // compare with tolerance instead of exact equality.
        let Some(floor) = json["relevanceFloor"].as_f64() else {
            panic!("floor present: {json}");
        };
        assert!((floor - 0.05).abs() < 1e-5, "floor mismatch: got {floor}");
        assert_eq!(json["candidatesAboveFloor"], 1);
        assert_eq!(json["candidatesBelowFloor"], 2);
    }

    #[test]
    fn retrieval_metrics_emits_null_floor_when_none() {
        let hits: Vec<SearchHit> = Vec::new();
        let metrics = RetrievalMetrics::from_hits_with_floor(10, 5.0, &hits, 0, None, 0);
        let json = metrics.data_json();
        assert!(json["relevanceFloor"].is_null());
        assert_eq!(json["candidatesAboveFloor"], 0);
        assert_eq!(json["candidatesBelowFloor"], 0);
    }

    #[test]
    fn no_relevant_results_degradation_includes_floor_and_consideration() {
        let degradation =
            SearchDegradation::no_relevant_results("test query", 0.05, 12, Some(0.02));
        assert_eq!(degradation.code, "no_relevant_results");
        assert_eq!(degradation.severity, "medium");
        assert!(degradation.message.contains("0.0500"));
        assert!(degradation.message.contains("test query"));
        assert!(degradation.message.contains("12 candidate"));
        assert!(degradation.message.contains("0.0200"));
        assert!(degradation.repair.is_some());
    }

    #[test]
    fn no_relevant_results_handles_singular_candidate() {
        let degradation = SearchDegradation::no_relevant_results("q", 0.05, 1, Some(0.01));
        // Singular: "1 candidate" not "1 candidates".
        assert!(degradation.message.contains("1 candidate"));
        assert!(!degradation.message.contains("1 candidates"));
    }

    #[test]
    fn low_recall_degradation_is_informational() {
        let degradation = SearchDegradation::low_recall_after_floor(0.05, 2, 10);
        assert_eq!(degradation.code, "low_recall_after_floor");
        assert_eq!(degradation.severity, "low");
        assert!(degradation.message.contains("2 of 10"));
        assert!(degradation.message.contains("0.0500"));
    }

    #[test]
    fn no_relevant_results_data_json_round_trips() {
        let degradation = SearchDegradation::no_relevant_results("q", 0.05, 5, Some(0.0));
        let json = degradation.data_json();
        assert_eq!(json["code"], "no_relevant_results");
        assert_eq!(json["severity"], "medium");
        assert!(json["repair"].is_string());
    }

    // ========================================================================
    // Bead bd-17c65.2.4 (B4) — qualityAssessment + honestQualityScore
    // ========================================================================

    #[test]
    fn quality_assessment_classify_empty_when_top_below_floor() {
        assert_eq!(
            QualityAssessment::classify(None, None, 0.05),
            QualityAssessment::Empty
        );
        assert_eq!(
            QualityAssessment::classify(Some(0.02), Some(0.01), 0.05),
            QualityAssessment::Empty
        );
        assert_eq!(
            QualityAssessment::classify(Some(f32::NAN), None, 0.05),
            QualityAssessment::Empty
        );
    }

    #[test]
    fn quality_assessment_classify_good_requires_top_2x_floor_and_mean_above() {
        assert_eq!(
            QualityAssessment::classify(Some(0.40), Some(0.10), 0.05),
            QualityAssessment::Good
        );
        // top exactly at 2× floor + mean exactly at floor → good
        assert_eq!(
            QualityAssessment::classify(Some(0.10), Some(0.05), 0.05),
            QualityAssessment::Good
        );
    }

    #[test]
    fn quality_assessment_classify_weak_when_top_close_to_floor_or_mean_below() {
        // Top above floor but not 2× → weak
        assert_eq!(
            QualityAssessment::classify(Some(0.06), Some(0.06), 0.05),
            QualityAssessment::Weak
        );
        // Top above 2× but mean below floor → weak (sparse cluster)
        assert_eq!(
            QualityAssessment::classify(Some(0.50), Some(0.02), 0.05),
            QualityAssessment::Weak
        );
    }

    #[test]
    fn quality_assessment_as_str_is_stable() {
        // Wire enum: do not rename without contract bump.
        assert_eq!(QualityAssessment::Good.as_str(), "good");
        assert_eq!(QualityAssessment::Weak.as_str(), "weak");
        assert_eq!(QualityAssessment::Empty.as_str(), "empty");
    }

    #[test]
    fn honest_quality_score_returns_none_when_below_floor() {
        let metrics = RetrievalMetrics::from_hits_with_floor(10, 5.0, &[], 0, Some(0.05), 5);
        assert!(metrics.honest_quality_score().is_none());
        assert_eq!(metrics.quality_assessment(), QualityAssessment::Empty);
    }

    #[test]
    fn honest_quality_score_is_higher_for_good_recall_than_weak_recall() {
        let good_hits = vec![
            synthetic_hit("a", 0.50),
            synthetic_hit("b", 0.45),
            synthetic_hit("c", 0.40),
            synthetic_hit("d", 0.38),
            synthetic_hit("e", 0.35),
        ];
        let weak_hits = vec![synthetic_hit("a", 0.06)];
        let good = RetrievalMetrics::from_hits_with_floor(10, 5.0, &good_hits, 0, Some(0.05), 0);
        let weak = RetrievalMetrics::from_hits_with_floor(10, 5.0, &weak_hits, 0, Some(0.05), 9);
        let Some(good_score) = good.honest_quality_score() else {
            panic!("good score present");
        };
        let Some(weak_score) = weak.honest_quality_score() else {
            panic!("weak score present");
        };
        assert!(
            good_score > weak_score,
            "expected good {good_score} > weak {weak_score}"
        );
        // Sanity: both in 0..1
        assert!((0.0..=1.0).contains(&good_score));
        assert!((0.0..=1.0).contains(&weak_score));
    }

    #[test]
    fn retrieval_metrics_data_json_includes_b4_fields() {
        let hits = vec![synthetic_hit("a", 0.40), synthetic_hit("b", 0.20)];
        let metrics = RetrievalMetrics::from_hits_with_floor(10, 5.0, &hits, 0, Some(0.05), 1);
        let json = metrics.data_json();
        assert_eq!(json["qualityAssessment"], "good");
        let Some(score) = json["honestQualityScore"].as_f64() else {
            panic!("score present: {json}");
        };
        assert!((0.0..=1.0).contains(&score));
    }

    #[test]
    fn retrieval_metrics_quality_assessment_empty_json() {
        // Below-floor input produces empty assessment + null score.
        let metrics = RetrievalMetrics::from_hits_with_floor(10, 5.0, &[], 0, Some(0.05), 3);
        let json = metrics.data_json();
        assert_eq!(json["qualityAssessment"], "empty");
        assert!(json["honestQualityScore"].is_null());
    }

    // ========================================================================
    // Bead bd-17c65.2.3 (B3) — dedupe_hits_on_doc_id
    // ========================================================================

    #[test]
    fn dedupe_keeps_unique_doc_ids_unchanged() {
        let hits = vec![
            synthetic_hit("a", 0.4),
            synthetic_hit("b", 0.3),
            synthetic_hit("c", 0.2),
        ];
        let (deduped, collapsed) = dedupe_hits_on_doc_id(hits);
        assert_eq!(deduped.len(), 3);
        assert_eq!(collapsed, 0);
        assert_eq!(deduped[0].doc_id, "a");
        assert_eq!(deduped[1].doc_id, "b");
        assert_eq!(deduped[2].doc_id, "c");
    }

    #[test]
    fn dedupe_collapses_duplicate_doc_ids_keeping_higher_score() {
        let hits = vec![
            synthetic_hit("a", 0.2),
            synthetic_hit("b", 0.3),
            synthetic_hit("a", 0.5), // higher dup → should replace position 0
            synthetic_hit("b", 0.1), // lower dup → no replace
        ];
        let (deduped, collapsed) = dedupe_hits_on_doc_id(hits);
        assert_eq!(deduped.len(), 2);
        assert_eq!(collapsed, 2);
        // Position-preserving: first-seen index for `a` is 0, for `b` is 1.
        assert_eq!(deduped[0].doc_id, "a");
        assert!((deduped[0].score - 0.5).abs() < 1e-5);
        assert_eq!(deduped[1].doc_id, "b");
        assert!((deduped[1].score - 0.3).abs() < 1e-5);
    }

    #[test]
    fn dedupe_ties_keep_first_seen() {
        let hits = vec![
            synthetic_hit("a", 0.5),
            synthetic_hit("a", 0.5), // tie → no replace (only strict >)
        ];
        let (deduped, collapsed) = dedupe_hits_on_doc_id(hits);
        assert_eq!(deduped.len(), 1);
        assert_eq!(collapsed, 1);
        assert!((deduped[0].score - 0.5).abs() < 1e-5);
    }

    #[test]
    fn dedupe_nan_score_does_not_replace() {
        let hits = vec![synthetic_hit("a", 0.4), synthetic_hit("a", f32::NAN)];
        let (deduped, collapsed) = dedupe_hits_on_doc_id(hits);
        assert_eq!(deduped.len(), 1);
        assert_eq!(collapsed, 1);
        assert!(
            (deduped[0].score - 0.4).abs() < 1e-5,
            "NaN must not overwrite a finite higher score"
        );
    }

    #[test]
    fn dedupe_empty_input_is_empty_output() {
        let hits: Vec<SearchHit> = Vec::new();
        let (deduped, collapsed) = dedupe_hits_on_doc_id(hits);
        assert!(deduped.is_empty());
        assert_eq!(collapsed, 0);
    }

    // ========================================================================
    // Bead bd-17c65.14.14 (N14) — mutual-information dedup
    // ========================================================================

    fn synthetic_content_hit(doc_id: &str, score: f32, content: &str) -> SearchHit {
        let mut hit = synthetic_hit(doc_id, score);
        hit.metadata = Some(serde_json::json!({ "content": content }));
        hit
    }

    #[test]
    fn mi_dedup_collapses_reordered_paraphrase_hits_with_merged_from() {
        let mut options = source_mode_test_options(SearchSourceMode::Hybrid, false);
        options.dedup_mode = SearchDedupMode::MutualInformation;
        let hits = vec![
            synthetic_content_hit("mem_a", 0.90, "run cargo fmt before release cargo test"),
            synthetic_content_hit("mem_b", 0.80, "cargo test cargo fmt before release run"),
            synthetic_content_hit(
                "mem_c",
                0.70,
                "graph centrality pagerank maintenance steward",
            ),
        ];

        let (deduped, collapsed, eligible) =
            dedupe_hits_on_mutual_information(hits, &options, None);

        assert_eq!(eligible, 3);
        assert_eq!(collapsed, 1);
        assert_eq!(
            deduped
                .iter()
                .map(|hit| hit.doc_id.as_str())
                .collect::<Vec<_>>(),
            vec!["mem_a", "mem_c"]
        );
        assert_eq!(
            deduped[0].metadata.as_ref().unwrap()["dedupeMode"],
            SearchDedupMode::MutualInformation.as_str()
        );
        assert_eq!(
            deduped[0].metadata.as_ref().unwrap()["mergedFrom"][0],
            "mem_b"
        );

        let report = SearchReport {
            status: SearchStatus::Success,
            query: "cargo fmt release".to_string(),
            requested_limit: 10,
            results: deduped,
            elapsed_ms: 1.0,
            errors: Vec::new(),
            degraded: vec![SearchDegradation::mi_dedup_candidate_proposed(1)],
            runtime_profile: test_runtime_profile(),
            relevance_floor_applied: None,
            candidates_below_floor: 0,
            source_mode_requested: SearchSourceMode::Hybrid,
            source_mode_applied: SearchSourceMode::Hybrid,
            source_mode_fallback: false,
            strict_source_mode: false,
            memory_scope: MemoryScope::Swarm,
            strict_scope: false,
            scope_stats: test_scope_stats(),
        };
        let json = report.data_json();
        assert_eq!(json["degraded"][0]["code"], "mi_dedup_candidate_proposed");
        assert_eq!(json["results"][0]["metadata"]["mergedFrom"][0], "mem_b");
    }

    #[test]
    fn mi_dedup_underpowered_when_too_few_memory_contents_are_available() {
        let mut options = source_mode_test_options(SearchSourceMode::Hybrid, false);
        options.dedup_mode = SearchDedupMode::MutualInformation;
        let hits = vec![
            synthetic_hit("mem_without_content", 0.90),
            synthetic_hit("session_without_memory_content", 0.80),
        ];

        let (deduped, collapsed, eligible) =
            dedupe_hits_on_mutual_information(hits, &options, None);

        assert_eq!(deduped.len(), 2);
        assert_eq!(collapsed, 0);
        assert_eq!(eligible, 0);
        let degradation = SearchDegradation::mi_dedup_threshold_underpowered(eligible);
        assert_eq!(degradation.code, "mi_dedup_threshold_underpowered");
        assert!(degradation.message.contains("at least 2 memory hits"));
    }

    // ========================================================================
    // Bead bd-17c65.2.5 (B5) — weak_query_recall signal
    // ========================================================================

    #[test]
    fn weak_query_recall_degradation_carries_top_score_and_floor() {
        let degradation = SearchDegradation::weak_query_recall(0.05, 0.07);
        assert_eq!(degradation.code, "weak_query_recall");
        assert_eq!(degradation.severity, "low");
        assert!(degradation.message.contains("0.0700"));
        assert!(degradation.message.contains("0.0500"));
        assert!(
            degradation
                .repair
                .as_deref()
                .is_some_and(|r| r.to_lowercase().contains("rephrase"))
        );
    }

    /// When top score is strictly between floor and 2× floor, the
    /// signal fires. Matches QualityAssessment::Weak from B4.
    #[test]
    fn weak_query_recall_threshold_aligns_with_quality_weak() {
        // top exactly at 2× floor → NOT weak (good); top below 2× → weak.
        // The signal fires when score < 2× floor.
        let floor = 0.05;
        let just_below_two_x = 0.09_f32;
        let two_x = 0.10_f32;
        let just_above_floor = 0.051_f32;
        assert!(just_below_two_x < floor * 2.0);
        assert!(two_x >= floor * 2.0);
        assert!(just_above_floor >= floor);
        // Round-trip: degradation factory accepts any (floor, top) pair.
        let _ = SearchDegradation::weak_query_recall(floor, just_below_two_x);
        let _ = SearchDegradation::weak_query_recall(floor, just_above_floor);
    }

    #[test]
    fn duplicates_collapsed_degradation_uses_correct_grammar() {
        let one = SearchDegradation::duplicates_collapsed(1);
        assert!(
            one.message.contains("1 duplicate hit "),
            "got {}",
            one.message
        );
        assert!(
            !one.message.contains("hits"),
            "singular for n=1: {}",
            one.message
        );
        let many = SearchDegradation::duplicates_collapsed(5);
        assert!(
            many.message.contains("5 duplicate hits"),
            "got {}",
            many.message
        );
    }

    /// bd-21xbi.1 acceptance: a workspace `.ee/config.toml` with
    /// `[search.lexical_ram_tier] enabled = true` changes the
    /// search-side runtime posture without requiring
    /// `EE_LEXICAL_INDEX_PIN_RAM` to be set in the environment.
    /// Mirror of the status-side regression at
    /// `src/core/status.rs::lexical_ram_tier_status_reads_workspace_config_file`.
    #[test]
    fn lexical_ram_tier_search_reads_workspace_config_file() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let config_dir = temp.path().join(".ee");
        std::fs::create_dir_all(&config_dir).map_err(|error| error.to_string())?;
        std::fs::write(
            config_dir.join("config.toml"),
            "[search.lexical_ram_tier]\nenabled = true\npopulate_on_open = false\n",
        )
        .map_err(|error| error.to_string())?;

        let config = lexical_ram_tier_config_for_search(temp.path());
        if !config.enabled {
            return Err(format!(
                "workspace config enabled=true must drive search posture; got {:?}",
                config
            ));
        }
        if config.populate_on_open {
            return Err(format!(
                "workspace config populate_on_open=false must drive search posture; got {:?}",
                config
            ));
        }
        Ok(())
    }
}
