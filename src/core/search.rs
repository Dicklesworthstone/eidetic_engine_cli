#[cfg(test)]
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::{Duration, Instant, SystemTime};

use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::config::MeshCommandMode;
use crate::config::env_registry::{EnvVar, read};
use crate::core::why::{DedupLinkEvidence, find_embed_dedup_link};
#[cfg(test)]
use crate::db::generate_audit_id_seeded;
use crate::db::{
    CreateAuditInput, DatabaseConfig, DbConnection, DbError, DbOperation,
    FeedbackEventsFingerprint, StoredFeedbackEvent, StoredMemory, StoredModelRegistryEntry,
    audit_actions, generate_audit_id,
    read_pool::{PoolConfig, ReadConnectionPool, registered_process_read_pool},
};
use crate::models::degradation::{
    CONFORMAL_CALIBRATION_INSUFFICIENT_CODE, SEARCH_SCORE_CALIBRATION_FILE_TOO_LARGE_CODE,
    SEARCH_SCORE_CALIBRATION_ROWS_CORRUPT_CODE, SEARCH_SCORE_CALIBRATION_UNREADABLE_CODE,
};
use crate::models::model_registry::{EmbedBackend, ModelPurpose, ModelRegistryStatus};
use crate::models::query::{EqlQuery, EqlSpeedMode, EqlTagsMode};
use crate::models::{
    DomainError, EvidenceId, GLOBAL_MEMORY_SCOPE_TAG, MemoryId, MemoryScope, MemoryScopeStats,
    ProvenanceUri, TrustClass, UnitScore, degraded_recovery_actions,
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
    EmbeddingPosture, IndexHealth, IndexProcessingOptions, IndexProcessingStatus, IndexStatusError,
    IndexStatusOptions, IndexStatusReport, current_embedding_posture,
    embedder_reports_pending_model2vec_download, get_index_status_with_connection,
    prepare_search_embedder_for_workspace, process_index_jobs_coalesced_with_cx_bounded,
};
use super::memory_drift::{
    MemoryDriftSelectionHint, memory_drift_selection_hint_from_provenance_status,
};
use super::memory_scope::{MemoryScopeContext, MeshQueryVisibility, mesh_query_visibility};
use super::profile::{RuntimeProfileReport, runtime_profile_for_workspace};
#[cfg(feature = "lexical-bm25")]
use crate::search::TantivyIndex;
use crate::search::lexical_ram_tier::{
    LEXICAL_HUGEPAGES_UNAVAILABLE_CODE, LEXICAL_RAM_TIER_HEAP_WARMLOAD_CODE,
    LEXICAL_RAM_TIER_HUGEPAGES_ENV, LEXICAL_RAM_TIER_PIN_RAM_ENV,
    LEXICAL_RAM_UNAVAILABLE_ON_MACOS_CODE, LexicalRamTierConfig, LexicalRamTierResult,
    pin_lexical_index_files, trace_lexical_ram_tier,
};
use crate::search::plan_cache::{
    CompiledPlan, DEFAULT_PLAN_CACHE_ENTRIES, PlanCacheKey, compute_eql_hash,
    compute_search_config_hash, lookup_or_insert_process_plan,
};
use crate::search::{
    NativeReranker, Reranker, SpeedMode, TwoTierConfig, TwoTierIndex, TwoTierSearcher,
};
// Test-only: used by the inline `#[cfg(test)]` suites further down.
#[cfg(test)]
use crate::search::HashEmbedder;
use crate::util::radix_ulid_sort::sort_by_ulid_payload_or_lexical;
use frankensearch::LexicalRead;

pub const DEFAULT_INDEX_SUBDIR: &str = "index";
pub const DIAG_SEARCH_SCHEMA_V1: &str = "ee.diag.search.v1";
pub const PERFORMANCE_EXPLAIN_SCHEMA_V1: &str = "ee.explain.performance.v1";
/// Fixed message used by performance fallbacks. Canonical degradation prose may
/// contain request-specific context, so performance diagnostics expose only the
/// internal code, severity, source, and this query/content-independent message.
pub const PERFORMANCE_FALLBACK_REDACTED_MESSAGE: &str =
    "Fallback details are redacted; inspect the degradation code and canonical response.";
pub const SEARCH_REVISION_TOKEN_SCHEMA_V1: &str = "ee.search.revision_token.v1";
pub const SEARCH_SCORE_INTERVAL_SCHEMA_V1: &str = "ee.search.score_interval.v1";
pub const SEARCH_SCORE_CALIBRATION_SCHEMA_V1: &str = "ee.search.score_calibration.v1";
pub const SEARCH_SCORE_RECALIBRATION_SCHEMA_V1: &str = "ee.search.score_recalibration.v1";
pub const QUERY_ASSIST_SCHEMA_V1: &str = "ee.query_assist.v1";
/// Generation lag above which a stale index is classified as a large gap.
///
/// Common synchronous write paths are expected to publish before returning.
/// Other transactional writers may leave queued jobs, so reads use this
/// boundary to decide whether a bounded automatic reconciliation is eligible.
pub const SEARCH_INDEX_LARGE_GAP_THRESHOLD: u64 = 50;
/// Maximum source-of-truth corpus size eligible for synchronous read repair.
///
/// Coalesced index publication rebuilds a complete, writer-fenced corpus even
/// for a one-generation gap. Bounding only the number of queued jobs would
/// therefore be misleading: a single job in a very large workspace can still
/// be expensive. Larger stores remain truthfully stale and use the explicit
/// rebuild repair instead of blocking an interactive read.
const SEARCH_INDEX_AUTO_RECONCILE_MAX_DOCUMENTS: u64 = 64;
const SEARCH_INDEX_AUTO_RECONCILE_TIMEOUT: Duration = Duration::from_millis(500);
const SEARCH_QUERY_MISS_AUDIT_SCHEMA_V1: &str = "ee.search.query_miss.v1";
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
const DEFAULT_SEARCH_RERANK_TOP_K: usize = 50;
pub const RERANK_MODEL_UNAVAILABLE_ADVISORY: &str = "No usable local reranker is registered. Search is using fusion-only ranking. Network download is unavailable, but a verified offline reranker artifact can be imported explicitly.";
/// Leading warning prose of the stale-index advisory. The episode-suppressed
/// compact form strips exactly this prefix, so the builder and the transform
/// must stay in lockstep through these constants.
const SEARCH_INDEX_STALE_WARNING_PREFIX: &str =
    "Search index is stale; returning lexical fallback results from the current index.";
/// Trailing advice prose of the stale-index advisory (see prefix above).
const SEARCH_INDEX_STALE_WARNING_SUFFIX: &str =
    " Newer memories may be omitted until the index is rebuilt.";
/// Stable wire token for permanent advisories emitted at most once during the
/// lifetime of one process. Transient degradations retain their own response-
/// or episode-scoped contracts.
pub const SEARCH_ADVISORY_SCOPE_PROCESS: &str = "process";
/// Stable wire token for a one-shot renderer that owns no retained advisory
/// state. A normal CLI invocation renders once, while long-lived owners must
/// use [`SearchReport::data_json_with_advisory_session`] and report `process`.
pub const SEARCH_ADVISORY_SCOPE_INVOCATION: &str = "invocation";
const RERANK_MODEL_TOKENIZER: &str = "tokenizer.json";
const RERANK_MODEL_SAFETENSORS_PRIMARY: &str = "model_f32.safetensors";
const RERANK_MODEL_SAFETENSORS_FALLBACK: &str = "model.safetensors";
const EMBED_MODEL_UNAVAILABLE_FEATURE_FLAG: &str = "embed-fast";
const HASH_FALLBACK_SEMANTIC_UNAVAILABLE_REASON: &str =
    "active embedder source frankensearch_hash_fallback reports semantic=false";
const SEARCH_MI_DEDUP_MIN_COSINE_SIMILARITY: f64 = 0.85;
const SEARCH_MI_DEDUP_MIN_NORMALIZED_MI: f64 = 0.72;
const QUERY_MISS_AUDIT_TTL_SECONDS: u64 = 7 * 24 * 60 * 60;
const QUERY_MISS_AUDIT_SAMPLE_RATE: f64 = 1.0;
const QUERY_ASSIST_COMPACT_DID_YOU_MEAN_LIMIT: usize = 1;
const QUERY_ASSIST_EXPLAIN_DID_YOU_MEAN_LIMIT: usize = 3;
const QUERY_ASSIST_COMPACT_REFORMULATION_LIMIT: usize = 1;
const QUERY_ASSIST_EXPLAIN_REFORMULATION_LIMIT: usize = 3;
const QUERY_ASSIST_TERM_LIMIT: usize = 4;

/// Character cap for the top-level `contentPreview` field added to each search
/// result. Agents previously had to dig into `metadata.content` (or make an
/// `ee memory show`/`ee why` follow-up call) to learn what a hit actually said;
/// a short preview at the top level removes that round-trip. (agent-UX item 1)
const SEARCH_CONTENT_PREVIEW_MAX_CHARS: usize = 240;

/// Extract the memory body text from a search hit's metadata, checking the
/// public `content` field first and falling back to the analysis-side keys.
fn search_hit_content_text(meta: &serde_json::Value) -> Option<String> {
    for key in [
        "content",
        SEARCH_ANALYSIS_CONTENT_KEY,
        "contentPreview",
        "content_preview",
    ] {
        if let Some(text) = meta.get(key).and_then(serde_json::Value::as_str) {
            let trimmed = text.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    None
}

/// Truncate body text to a single-line preview on a char boundary, collapsing
/// interior newlines so a multi-line memory still renders as one tidy line.
fn search_content_preview(content: &str, max_chars: usize) -> String {
    let collapsed = content.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= max_chars {
        return collapsed;
    }
    let mut preview = collapsed.chars().take(max_chars).collect::<String>();
    preview.push('…');
    preview
}

static SEARCH_INDEX_STATUS_CACHE: OnceLock<Mutex<HashMap<IndexStatusCacheKey, CachedIndexStatus>>> =
    OnceLock::new();
#[cfg(feature = "lexical-bm25")]
static PROCESS_LEXICAL_SEARCHER_CACHE: OnceLock<Mutex<Option<CachedLexicalSearcher>>> =
    OnceLock::new();
#[cfg(feature = "lexical-bm25")]
#[derive(Clone, Debug, Eq, PartialEq)]
struct LexicalSearcherCacheKey {
    index_dir: PathBuf,
    index_manifest_version: u64,
}
#[cfg(feature = "lexical-bm25")]
struct CachedLexicalSearcher {
    key: LexicalSearcherCacheKey,
    searcher: Arc<dyn LexicalRead>,
}
#[cfg(test)]
type BeforeSearchCollectHook = Box<dyn FnOnce(&asupersync::Cx)>;
#[cfg(test)]
type AfterSearchCollectHook = Box<dyn FnOnce(&asupersync::Cx, bool)>;
#[cfg(test)]
std::thread_local! {
    static BEFORE_SEARCH_COLLECT_HOOK: RefCell<Option<BeforeSearchCollectHook>> =
        const { RefCell::new(None) };
    static AFTER_SEARCH_COLLECT_HOOK: RefCell<Option<AfterSearchCollectHook>> =
        const { RefCell::new(None) };
}

#[cfg(test)]
fn install_before_search_collect_hook(hook: impl FnOnce(&asupersync::Cx) + 'static) {
    BEFORE_SEARCH_COLLECT_HOOK.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(hook));
    });
}

#[cfg(test)]
fn run_before_search_collect_hook(cx: &asupersync::Cx) {
    BEFORE_SEARCH_COLLECT_HOOK.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook(cx);
        }
    });
}

#[cfg(test)]
fn install_after_search_collect_hook(hook: impl FnOnce(&asupersync::Cx, bool) + 'static) {
    AFTER_SEARCH_COLLECT_HOOK.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(hook));
    });
}

#[cfg(test)]
fn run_after_search_collect_hook(cx: &asupersync::Cx, succeeded: bool) {
    AFTER_SEARCH_COLLECT_HOOK.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook(cx, succeeded);
        }
    });
}

#[cfg(not(test))]
fn run_before_search_collect_hook(_cx: &asupersync::Cx) {}
#[cfg(not(test))]
fn run_after_search_collect_hook(_cx: &asupersync::Cx, _succeeded: bool) {}
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
static SEARCH_SCORE_CALIBRATION_CACHE: OnceLock<
    RwLock<HashMap<SearchScoreCalibrationCacheKey, SearchScoreCalibration>>,
> = OnceLock::new();
#[cfg(test)]
static SEARCH_SCORE_CALIBRATION_FEEDBACK_EVENT_FULL_LOADS: OnceLock<
    Mutex<HashMap<PathBuf, usize>>,
> = OnceLock::new();
static LEXICAL_RAM_TIER_SEARCH_CONFIG_CACHE: OnceLock<
    Mutex<HashMap<PathBuf, CachedLexicalRamTierSearchConfig>>,
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
            .unwrap_or_else(|| default_workspace_database_path(&options.workspace_path));
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct LexicalRamTierSearchConfigFingerprint {
    exists: bool,
    len: u64,
    modified: Option<SystemTime>,
    content_hash: Option<[u8; 32]>,
}

#[derive(Clone, Copy, Debug)]
struct CachedLexicalRamTierSearchConfig {
    fingerprint: LexicalRamTierSearchConfigFingerprint,
    config: LexicalRamTierConfig,
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

#[derive(Clone, Debug)]
pub struct SimilarOptions {
    pub workspace_path: PathBuf,
    pub database_path: Option<PathBuf>,
    pub index_dir: Option<PathBuf>,
    pub memory_id: String,
    pub limit: u32,
    pub min_score: Option<f32>,
    pub speed: SpeedMode,
    pub explain: bool,
    /// Evaluate validity windows at this timestamp. Defaults to now.
    pub as_of: Option<DateTime<Utc>>,
    /// Include tombstoned memories in neighbor results.
    pub include_tombstoned: bool,
    /// Include memories whose valid_to is before the validity reference time.
    pub include_expired: bool,
    /// Include memories whose valid_from is after the validity reference time.
    pub include_future: bool,
    /// Include search hits whose indexed validity metadata is stale.
    pub include_stale: bool,
    /// Trust lane applied to retrieved memories.
    pub memory_scope: MemoryScope,
    /// Fail closed when relevant evidence exists outside the requested scope.
    pub strict_scope: bool,
}

impl SimilarOptions {
    fn resolve_database_path(&self) -> PathBuf {
        self.database_path
            .clone()
            .unwrap_or_else(|| default_workspace_database_path(&self.workspace_path))
    }

    fn resolve_index_dir(&self) -> PathBuf {
        self.index_dir
            .clone()
            .unwrap_or_else(|| default_workspace_index_dir(&self.workspace_path))
    }
}

#[derive(Clone, Debug)]
pub struct SimilarReport {
    pub target_memory_id: String,
    pub target_level: String,
    pub target_kind: String,
    pub semantic_available: bool,
    pub lexical_fallback: bool,
    pub embedding_posture: EmbeddingPosture,
    pub report: SearchReport,
}

impl SimilarReport {
    #[must_use]
    pub fn human_summary(&self) -> String {
        let mut summary = self.report.human_summary();
        if summary.starts_with("Search results for ") {
            summary = summary.replacen("Search results for ", "Similar memories for ", 1);
        } else if summary.starts_with("No results for ") {
            summary = summary.replacen("No results for ", "No similar memories for ", 1);
        }
        summary
    }

    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        let mut data = self.report.data_json();
        if let Some(data_object) = data.as_object_mut() {
            data_object.insert("command".to_string(), serde_json::json!("similar"));
            data_object.insert(
                "targetMemoryId".to_string(),
                serde_json::json!(&self.target_memory_id),
            );
            data_object.insert(
                "targetMemory".to_string(),
                serde_json::json!({
                    "memoryId": &self.target_memory_id,
                    "level": &self.target_level,
                    "kind": &self.target_kind,
                }),
            );
            data_object.insert(
                "semanticAvailable".to_string(),
                serde_json::json!(self.semantic_available),
            );
            data_object.insert(
                "lexicalFallback".to_string(),
                serde_json::json!(self.lexical_fallback),
            );
            data_object.insert(
                "embeddingPosture".to_string(),
                self.embedding_posture.data_json(),
            );
            if let Some(request) = data_object
                .get_mut("request")
                .and_then(serde_json::Value::as_object_mut)
            {
                request.insert(
                    "targetMemoryId".to_string(),
                    serde_json::json!(&self.target_memory_id),
                );
                request.insert(
                    "similarityMode".to_string(),
                    serde_json::json!(similarity_mode_for_posture(&self.embedding_posture)),
                );
            }
        }
        data
    }
}

#[derive(Clone, Debug)]
pub struct SearchWorkspaceProbeState {
    pub runtime_profile: RuntimeProfileReport,
    pub output_redaction_enabled: bool,
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

    #[must_use]
    pub const fn uses_embeddings(self) -> bool {
        matches!(self, Self::SemanticOnly | Self::Hybrid)
    }
}

/// Live fusion arm weights. Crate-visible so the shadow-tuning evaluator
/// (bd-2tehh.2 S2) can re-fuse a replayed hit pool with candidate weight
/// vectors using exactly the production adjustment math.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct SearchFusionWeights {
    pub(crate) lexical: f32,
    pub(crate) semantic: f32,
    pub(crate) graph: f32,
}

impl Default for SearchFusionWeights {
    fn default() -> Self {
        Self {
            lexical: DEFAULT_SEARCH_LEXICAL_WEIGHT,
            semantic: DEFAULT_SEARCH_SEMANTIC_WEIGHT,
            graph: DEFAULT_SEARCH_GRAPH_WEIGHT,
        }
    }
}

impl SearchFusionWeights {
    fn from_config(config: &crate::config::SearchConfig) -> Self {
        let defaults = Self::default();
        Self {
            lexical: unit_weight_or(config.lexical_weight, defaults.lexical),
            semantic: unit_weight_or(config.semantic_weight, defaults.semantic),
            graph: unit_weight_or(config.graph_weight, defaults.graph),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct SearchFusionAdjustment {
    pub(crate) multiplier: f32,
    lexical_component: f32,
    semantic_component: f32,
    graph_component: f32,
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
    semantic_embedder_pending: Option<&'a str>,
    semantic_embedder_degraded: Option<&'a str>,
}

/// Default relevance floor for the public `relevanceScore` projection (bead
/// bd-17c65.2.1 / B1).
///
/// Calibrated against the 2026-05-10 corpus where junk semantic_fast hits
/// scored `< 0.03` and meaningful hits scored `0.10..=0.50`. Every retrieval
/// scale is projected into this `0.0..=1.0` domain before the floor is applied.
/// Configurable per-call via `--relevance-floor` and per-workspace via
/// `search.relevance_floor` config.
pub const DEFAULT_RELEVANCE_FLOOR: f32 = 0.05;

const DEFAULT_SEARCH_LEXICAL_WEIGHT: f32 = 0.45;
const DEFAULT_SEARCH_SEMANTIC_WEIGHT: f32 = 0.45;
const DEFAULT_SEARCH_GRAPH_WEIGHT: f32 = 0.10;

/// Reference maximum magnitude for an RRF-fused `Hybrid` score (bd-1et0v.11).
///
/// Two arms each contributing the rank-1 reciprocal `1/(k+1)` at the canonical
/// `k=60` give `2/61 ≈ 0.0328`. A top hybrid hit lands at this magnitude, so
/// dividing a raw hybrid score by it yields a self-describing `relevanceScore`
/// near `1.0` — the fix for an agent misreading the inherent `~0.03` RRF
/// magnitude as "no match". Scores above it (three contributing arms) clamp to
/// `1.0`; this is an interpretability reference, not a ranking input.
pub const RRF_HYBRID_TYPICAL_MAX: f32 = 2.0 / 61.0;

/// Deterministic normalized relevance score in `0.0..=1.0` (bd-1et0v.11).
///
/// Maps a [`SearchHit::score`] to a self-describing `0..1` value an agent can
/// read as "how relevant". Lexical-only results are batch-normalized at the
/// Frankensearch adapter boundary while retaining raw BM25 in `lexicalScore`.
/// Cosine-domain sources are clamped to positive similarity, while `Hybrid`
/// carries an RRF-fused magnitude topping out near
/// [`RRF_HYBRID_TYPICAL_MAX`] (`~0.033`). Dividing the hybrid magnitude by that
/// reference rescales a top hit to `~1.0` so an agent does not misread the
/// inherent `~0.03` RRF score as "no match". The projection is pure and
/// order-preserving within a scale. Native engine scores still rank one search
/// execution; the projection only compares hits when independently scored
/// result sets are merged across scales.
#[must_use]
pub fn normalized_relevance_score(source: ScoreSource, score: f32) -> f32 {
    let normalized = match source {
        ScoreSource::Hybrid => score / RRF_HYBRID_TYPICAL_MAX,
        ScoreSource::Lexical
        | ScoreSource::SemanticFast
        | ScoreSource::SemanticQuality
        | ScoreSource::Reranked
        | ScoreSource::HashControl => score,
    };
    normalized.clamp(0.0, 1.0)
}

pub(crate) fn search_hit_meets_relevance_floor(
    hit: &SearchHit,
    user_floor_override: Option<f32>,
) -> bool {
    let floor = user_floor_override.unwrap_or(DEFAULT_RELEVANCE_FLOOR);
    let relevance_score = hit.relevance_score();
    relevance_score.is_finite() && relevance_score >= floor
}

impl SearchOptions {
    fn resolve_database_path(&self) -> PathBuf {
        self.database_path
            .clone()
            .unwrap_or_else(|| default_workspace_database_path(&self.workspace_path))
    }

    fn resolve_index_dir(&self) -> PathBuf {
        self.index_dir
            .clone()
            .unwrap_or_else(|| default_workspace_index_dir(&self.workspace_path))
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

fn default_workspace_root(workspace_path: &Path) -> PathBuf {
    crate::config::workspace::canonical_workspace_root_or_lexical(workspace_path)
}

fn default_workspace_database_path(workspace_path: &Path) -> PathBuf {
    default_workspace_root(workspace_path)
        .join(".ee")
        .join("ee.db")
}

fn bound_search_workspace_id(
    workspace_path: &Path,
    database_path: Option<&Path>,
    connection: Option<&DbConnection>,
) -> String {
    let workspace_root = default_workspace_root(workspace_path);
    let requested = crate::core::curate::stable_workspace_id(&workspace_root);
    if let Some(connection) = connection {
        return crate::core::workspace::bound_workspace_id_or_hash(
            connection,
            &requested,
            &[workspace_path, workspace_root.as_path()],
        )
        .unwrap_or(requested);
    }
    let default_database_path = default_workspace_database_path(workspace_path);
    let database_path = database_path.unwrap_or(&default_database_path);
    if !database_path.exists() {
        return requested;
    }
    match DbConnection::open_file_read_only(database_path) {
        Ok(connection) => crate::core::workspace::bound_workspace_id_or_hash(
            &connection,
            &requested,
            &[workspace_path, workspace_root.as_path()],
        )
        .unwrap_or(requested),
        Err(_) => requested,
    }
}

fn default_workspace_index_dir(workspace_path: &Path) -> PathBuf {
    default_workspace_root(workspace_path)
        .join(".ee")
        .join(DEFAULT_INDEX_SUBDIR)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum QueryAssistMode {
    Compact,
    Explain,
}

impl QueryAssistMode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Compact => "compact",
            Self::Explain => "explain",
        }
    }

    #[must_use]
    const fn did_you_mean_limit(self) -> usize {
        match self {
            Self::Compact => QUERY_ASSIST_COMPACT_DID_YOU_MEAN_LIMIT,
            Self::Explain => QUERY_ASSIST_EXPLAIN_DID_YOU_MEAN_LIMIT,
        }
    }

    #[must_use]
    const fn reformulation_limit(self) -> usize {
        match self {
            Self::Compact => QUERY_ASSIST_COMPACT_REFORMULATION_LIMIT,
            Self::Explain => QUERY_ASSIST_EXPLAIN_REFORMULATION_LIMIT,
        }
    }
}

#[derive(Clone, Debug)]
pub struct QueryAssistReport {
    pub weak_result_reason: String,
    pub mode: QueryAssistMode,
    pub reformulations: Vec<QueryAssistReformulation>,
    pub did_you_mean: Vec<SearchHit>,
    pub capture_template: QueryAssistCaptureTemplate,
    pub candidate_count: usize,
    pub dropped_below_floor: usize,
    pub relevance_floor: Option<f32>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueryAssistReformulation {
    pub query: String,
    pub strategy: &'static str,
    pub rationale: String,
    pub matched_doc_id: String,
    pub matched_memory_id: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueryAssistCaptureTemplate {
    pub level: &'static str,
    pub kind: &'static str,
    pub tags: Vec<&'static str>,
    pub content: String,
    pub command: String,
}

impl QueryAssistCaptureTemplate {
    #[must_use]
    pub fn for_query(query: &str) -> Self {
        let clean_query = query.split_whitespace().collect::<Vec<_>>().join(" ");
        let content = if clean_query.is_empty() {
            "TODO: capture the missing memory this search needs.".to_owned()
        } else {
            format!("TODO: capture memory needed for search query: {clean_query}")
        };
        let command = format!(
            "ee remember --level semantic --kind note --tags query-gap,search-miss --json {}",
            shell_quote(&content)
        );
        Self {
            level: "semantic",
            kind: "note",
            tags: vec!["query-gap", "search-miss"],
            content,
            command,
        }
    }

    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        serde_json::json!({
            "level": self.level,
            "kind": self.kind,
            "tags": &self.tags,
            "content": &self.content,
            "command": &self.command,
            "rationale": "Capture this missing demand explicitly so ee learn gaps can cluster repeated misses.",
        })
    }
}

impl QueryAssistReport {
    #[must_use]
    pub fn data_json(&self, output_redaction_enabled: bool) -> serde_json::Value {
        serde_json::json!({
            "schema": QUERY_ASSIST_SCHEMA_V1,
            "mode": self.mode.as_str(),
            "weakResultReason": &self.weak_result_reason,
            "candidateCount": self.candidate_count,
            "droppedBelowFloor": self.dropped_below_floor,
            "relevanceFloor": optional_score_json(self.relevance_floor),
            "reformulations": self.reformulations.iter().map(QueryAssistReformulation::data_json).collect::<Vec<_>>(),
            "didYouMean": self.did_you_mean.iter().map(|hit| query_assist_did_you_mean_json(hit, output_redaction_enabled)).collect::<Vec<_>>(),
            "captureTemplate": self.capture_template.data_json(),
        })
    }
}

impl QueryAssistReformulation {
    #[must_use]
    fn data_json(&self) -> serde_json::Value {
        serde_json::json!({
            "query": &self.query,
            "strategy": self.strategy,
            "rationale": &self.rationale,
            "matchedDocId": &self.matched_doc_id,
            "matchedMemoryId": &self.matched_memory_id,
        })
    }
}

/// Authoritative per-response index freshness, populated from the same
/// index-status probe that raises the stale/large-gap advisories and emitted
/// independently of advisory-episode dedupe, so a suppressed warning never
/// hides stale/generation/gap truth (bd-index-auto-freshness-m5kwf). Never
/// reconstructed from advisory prose.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchIndexFreshness {
    pub stale: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub db_generation: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub index_generation: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generation_gap: Option<u64>,
    pub large_gap: bool,
}

#[derive(Clone, Debug)]
pub struct SearchReport {
    pub status: SearchStatus,
    pub embed_backend: EmbedBackend,
    pub query: String,
    pub requested_limit: u32,
    pub results: Vec<SearchHit>,
    pub elapsed_ms: f64,
    pub errors: Vec<String>,
    pub degraded: Vec<SearchDegradation>,
    pub runtime_profile: RuntimeProfileReport,
    pub rerank_configured_mode: crate::config::SearchRerankMode,
    pub rerank_configured_top_k: usize,
    /// Whether runtime resolution loaded a usable reranker for this search.
    /// This is capability state and is independent of candidate/result count.
    pub rerank_runtime_available: bool,
    /// Relevance floor that was applied to this search (B1 bd-17c65.2.1).
    /// `None` only for error cases where no search ran.
    pub relevance_floor_applied: Option<f32>,
    /// Number of candidates dropped because they scored below the floor
    /// (B1). Informational; agents can use this to decide whether to
    /// retry with a lower floor or different query.
    pub candidates_below_floor: usize,
    pub query_assist: Option<QueryAssistReport>,
    pub source_mode_requested: SearchSourceMode,
    pub source_mode_applied: SearchSourceMode,
    pub source_mode_fallback: bool,
    pub strict_source_mode: bool,
    pub memory_scope: MemoryScope,
    pub strict_scope: bool,
    pub scope_stats: MemoryScopeStats,
    /// `Some` whenever the index-status probe observed a stale index for this
    /// response; carries the authoritative generations and gap classification.
    pub index_freshness: Option<SearchIndexFreshness>,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct SearchAdvisoryIdentity {
    code: String,
    severity: String,
    message: String,
    repair: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SearchAdvisoryObservation {
    emitted: bool,
    occurrence_count: u64,
    distinct_count: usize,
    session_suppressed_count: u64,
}

const DEFAULT_SEARCH_ADVISORY_WORKSPACE: &str = "process-default";
pub(crate) const MAX_SEARCH_ADVISORY_WORKSPACES: usize = 64;
const MAX_SEARCH_ADVISORY_CAPACITY_BUSY_DELIVERIES: usize = 64;

/// Result of consulting the bounded transient-advisory ledger.
///
/// `CapacityBusy` is deliberately distinct from `Suppress`: the advisory must
/// remain on the response while every tracked workspace has an in-flight
/// delivery reservation. Socket settlement can then retry admission after a
/// reservation is released without ever turning capacity pressure into silent
/// warning loss.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SearchAdvisoryEmission {
    Emit,
    Suppress,
    CapacityBusy,
}

impl SearchAdvisoryEmission {
    #[must_use]
    pub(crate) const fn should_emit(self) -> bool {
        !matches!(self, Self::Suppress)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SearchAdvisorySettlement {
    Complete,
    RetryCapacityBusy,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct SearchAdvisoryPermanentState {
    occurrence_count: u64,
    delivered: bool,
    reserved_delivery: Option<u64>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct SearchAdvisoryWorkspaceState {
    large_gap_occurrence_count: u64,
    large_gap_delivered: bool,
    large_gap_reserved_delivery: Option<u64>,
    last_observed: u64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct SearchAdvisoryDeliveryAccounting {
    /// Deltas owned by one response. They are invisible to every other
    /// reservation until successful socket settlement commits them.
    workspace_id: String,
    permanent_occurrences: BTreeMap<SearchAdvisoryIdentity, u64>,
    permanent_suppressed_count: u64,
    large_gap_occurrence_count: u64,
}

/// One daemon response's provisional claim on advisories that must not be
/// consumed until its socket frame is delivered successfully. Counter deltas
/// remain token-local for the same reason: another concurrent response renders
/// committed state plus only its own delta, never another in-flight attempt.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SearchAdvisoryDeliveryReservation {
    workspace_id: String,
    token: u64,
    emitted: bool,
    large_gap_capacity_busy: bool,
    provisional_accounting: bool,
}

impl SearchAdvisoryDeliveryReservation {
    #[must_use]
    pub(crate) fn workspace_id(&self) -> &str {
        &self.workspace_id
    }

    #[must_use]
    pub(crate) const fn token(&self) -> u64 {
        self.token
    }

    #[must_use]
    pub(crate) const fn emitted(&self) -> bool {
        self.emitted
    }

    #[must_use]
    pub(crate) const fn large_gap_capacity_busy(&self) -> bool {
        self.large_gap_capacity_busy
    }

    #[must_use]
    pub(crate) const fn has_provisional_accounting(&self) -> bool {
        self.provisional_accounting
    }

    #[must_use]
    pub(crate) const fn requires_settlement(&self) -> bool {
        self.emitted() || self.has_provisional_accounting()
    }
}

/// Process-lifetime permanent-advisory ledger plus bounded, workspace-
/// partitioned state for transient large-index-gap episodes.
///
/// A permanent advisory identity is retained after authoritative recovery and
/// across workspace eviction, so it cannot reappear later in the same process.
/// Permanent identities are source-controlled capability codes rather than
/// caller-controlled workspace IDs. Canonical transient degradations remain
/// visible in every affected response; only the companion
/// `search_index_large_gap` repair notice is once per active workspace episode.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SearchAdvisorySession {
    permanent_occurrences: BTreeMap<SearchAdvisoryIdentity, SearchAdvisoryPermanentState>,
    permanent_suppressed_count: u64,
    workspaces: BTreeMap<String, SearchAdvisoryWorkspaceState>,
    capacity_busy_deliveries: BTreeMap<u64, String>,
    delivery_accounting: BTreeMap<u64, SearchAdvisoryDeliveryAccounting>,
    observation_clock: u64,
    delivery_clock: u64,
}

impl SearchAdvisorySession {
    fn workspace_mut(&mut self, workspace_id: &str) -> Option<&mut SearchAdvisoryWorkspaceState> {
        self.observation_clock = self.observation_clock.saturating_add(1);
        let observed_at = self.observation_clock;
        if !self.workspaces.contains_key(workspace_id)
            && self.workspaces.len() >= MAX_SEARCH_ADVISORY_WORKSPACES
        {
            let oldest_workspace = self
                .workspaces
                .iter()
                .filter(|(workspace_id, state)| {
                    state.large_gap_reserved_delivery.is_none()
                        && !self.delivery_accounting.values().any(|accounting| {
                            accounting.workspace_id.as_str() == workspace_id.as_str()
                                && accounting.large_gap_occurrence_count > 0
                        })
                })
                .min_by(|(left_id, left), (right_id, right)| {
                    left.last_observed
                        .cmp(&right.last_observed)
                        .then_with(|| left_id.cmp(right_id))
                })
                .map(|(workspace_id, _)| workspace_id.clone());
            let Some(oldest_workspace) = oldest_workspace else {
                return None;
            };
            self.workspaces.remove(&oldest_workspace);
        }
        let state = self.workspaces.entry(workspace_id.to_owned()).or_default();
        state.last_observed = observed_at;
        Some(state)
    }

    fn delivery_token_is_reserved(&self, token: u64) -> bool {
        self.permanent_occurrences
            .values()
            .any(|entry| entry.reserved_delivery == Some(token))
            || self
                .workspaces
                .values()
                .any(|state| state.large_gap_reserved_delivery == Some(token))
            || self.capacity_busy_deliveries.contains_key(&token)
            || self.delivery_accounting.contains_key(&token)
    }

    fn next_delivery_token(&mut self) -> u64 {
        loop {
            self.delivery_clock = self.delivery_clock.wrapping_add(1);
            if !self.delivery_token_is_reserved(self.delivery_clock) {
                return self.delivery_clock;
            }
        }
    }

    pub(crate) fn reserve_delivery(
        &mut self,
        workspace_id: &str,
    ) -> SearchAdvisoryDeliveryReservation {
        SearchAdvisoryDeliveryReservation {
            workspace_id: workspace_id.to_owned(),
            token: self.next_delivery_token(),
            emitted: false,
            large_gap_capacity_busy: false,
            provisional_accounting: false,
        }
    }

    fn delivery_accounting_mut(
        &mut self,
        reservation: &mut SearchAdvisoryDeliveryReservation,
    ) -> &mut SearchAdvisoryDeliveryAccounting {
        reservation.provisional_accounting = true;
        self.delivery_accounting
            .entry(reservation.token)
            .or_insert_with(|| SearchAdvisoryDeliveryAccounting {
                workspace_id: reservation.workspace_id.clone(),
                ..SearchAdvisoryDeliveryAccounting::default()
            })
    }

    fn observe_permanent_inner(
        &mut self,
        workspace_id: &str,
        degradation: &SearchDegradation,
        mut reservation: Option<&mut SearchAdvisoryDeliveryReservation>,
    ) -> SearchAdvisoryObservation {
        let identity = SearchAdvisoryIdentity {
            code: degradation.code.clone(),
            severity: degradation.severity.clone(),
            message: degradation.message.clone(),
            repair: degradation.repair.clone(),
        };
        let emitted = {
            let entry = self
                .permanent_occurrences
                .entry(identity.clone())
                .or_default();
            let emitted = !entry.delivered && entry.reserved_delivery.is_none();
            if emitted && let Some(reservation) = reservation.as_deref_mut() {
                debug_assert_eq!(reservation.workspace_id, workspace_id);
                entry.reserved_delivery = Some(reservation.token);
            } else if emitted {
                entry.delivered = true;
            }
            emitted
        };
        let mut local_occurrence_count = 0;
        let mut local_suppressed_count = 0;
        if let Some(reservation) = reservation.as_deref_mut() {
            if emitted {
                reservation.emitted = true;
            }
            let accounting = self.delivery_accounting_mut(reservation);
            let occurrence_delta = accounting
                .permanent_occurrences
                .entry(identity.clone())
                .or_default();
            *occurrence_delta = occurrence_delta.saturating_add(1);
            local_occurrence_count = *occurrence_delta;
            if !emitted {
                accounting.permanent_suppressed_count =
                    accounting.permanent_suppressed_count.saturating_add(1);
            }
            local_suppressed_count = accounting.permanent_suppressed_count;
        } else {
            let entry = self
                .permanent_occurrences
                .entry(identity.clone())
                .or_default();
            entry.occurrence_count = entry.occurrence_count.saturating_add(1);
            if !emitted {
                self.permanent_suppressed_count = self.permanent_suppressed_count.saturating_add(1);
            }
        }
        let occurrence_count =
            self.permanent_occurrences
                .get(&identity)
                .map_or(local_occurrence_count, |entry| {
                    entry
                        .occurrence_count
                        .saturating_add(local_occurrence_count)
                });
        let mut distinct_identities = self
            .permanent_occurrences
            .iter()
            .filter(|(_, entry)| entry.occurrence_count > 0)
            .map(|(identity, _)| identity.clone())
            .collect::<BTreeSet<_>>();
        if let Some(reservation) = reservation
            && let Some(accounting) = self.delivery_accounting.get(&reservation.token)
        {
            distinct_identities.extend(accounting.permanent_occurrences.keys().cloned());
        }
        SearchAdvisoryObservation {
            emitted,
            occurrence_count,
            distinct_count: distinct_identities.len(),
            session_suppressed_count: self
                .permanent_suppressed_count
                .saturating_add(local_suppressed_count),
        }
    }

    fn observe_permanent(
        &mut self,
        workspace_id: &str,
        degradation: &SearchDegradation,
    ) -> SearchAdvisoryObservation {
        self.observe_permanent_inner(workspace_id, degradation, None)
    }

    fn observe_permanent_for_delivery(
        &mut self,
        workspace_id: &str,
        degradation: &SearchDegradation,
        reservation: &mut SearchAdvisoryDeliveryReservation,
    ) -> SearchAdvisoryObservation {
        self.observe_permanent_inner(workspace_id, degradation, Some(reservation))
    }

    fn recover_permanent(&mut self, _workspace_id: &str, _code: &str) {
        // Recovery changes current capability posture, but a permanent advisory
        // that was already delivered stays consumed for this process lifetime.
    }

    pub(crate) fn emit_large_gap_while_active(
        &mut self,
        workspace_id: &str,
        active: bool,
    ) -> SearchAdvisoryEmission {
        self.emit_large_gap_while_active_inner(workspace_id, active, None)
    }

    pub(crate) fn emit_large_gap_while_active_for_delivery(
        &mut self,
        workspace_id: &str,
        active: bool,
        reservation: &mut SearchAdvisoryDeliveryReservation,
    ) -> SearchAdvisoryEmission {
        self.emit_large_gap_while_active_inner(workspace_id, active, Some(reservation))
    }

    fn emit_large_gap_while_active_inner(
        &mut self,
        workspace_id: &str,
        active: bool,
        mut reservation: Option<&mut SearchAdvisoryDeliveryReservation>,
    ) -> SearchAdvisoryEmission {
        if !active {
            self.capacity_busy_deliveries
                .retain(|_, pending_workspace_id| pending_workspace_id != workspace_id);
            for accounting in self.delivery_accounting.values_mut() {
                if accounting.workspace_id == workspace_id {
                    accounting.large_gap_occurrence_count = 0;
                }
            }
            if let Some(state) = self.workspaces.get_mut(workspace_id) {
                state.large_gap_occurrence_count = 0;
                state.large_gap_delivered = false;
                state.large_gap_reserved_delivery = None;
            }
            return SearchAdvisoryEmission::Suppress;
        }
        let Some(state) = self.workspace_mut(workspace_id) else {
            if let Some(reservation) = reservation.as_deref_mut() {
                debug_assert_eq!(reservation.workspace_id, workspace_id);
                reservation.emitted = true;
                reservation.large_gap_capacity_busy = true;
                if self.capacity_busy_deliveries.len()
                    < MAX_SEARCH_ADVISORY_CAPACITY_BUSY_DELIVERIES
                {
                    self.capacity_busy_deliveries
                        .insert(reservation.token, workspace_id.to_owned());
                }
            }
            return SearchAdvisoryEmission::CapacityBusy;
        };
        let emitted = !state.large_gap_delivered && state.large_gap_reserved_delivery.is_none();
        if emitted {
            if let Some(reservation) = reservation.as_deref_mut() {
                debug_assert_eq!(reservation.workspace_id, workspace_id);
                state.large_gap_reserved_delivery = Some(reservation.token);
                reservation.emitted = true;
            } else {
                state.large_gap_occurrence_count =
                    state.large_gap_occurrence_count.saturating_add(1);
                state.large_gap_delivered = true;
            }
        }
        if let Some(reservation) = reservation.as_deref_mut() {
            let accounting = self.delivery_accounting_mut(reservation);
            accounting.large_gap_occurrence_count =
                accounting.large_gap_occurrence_count.saturating_add(1);
        } else if !emitted {
            state.large_gap_occurrence_count = state.large_gap_occurrence_count.saturating_add(1);
        }
        if emitted {
            SearchAdvisoryEmission::Emit
        } else {
            SearchAdvisoryEmission::Suppress
        }
    }

    fn suppressed_count(&self, workspace_id: &str) -> u64 {
        let _ = workspace_id;
        self.permanent_suppressed_count
    }

    pub(crate) fn settle_delivery(
        &mut self,
        workspace_id: &str,
        token: u64,
        delivered: bool,
        large_gap_capacity_busy: bool,
    ) -> SearchAdvisorySettlement {
        let accounting = self.delivery_accounting.remove(&token);
        if delivered && let Some(accounting) = accounting {
            self.commit_delivery_accounting(accounting);
        }
        for entry in self.permanent_occurrences.values_mut() {
            if entry.reserved_delivery == Some(token) {
                entry.reserved_delivery = None;
                if delivered {
                    entry.delivered = true;
                }
            }
        }
        if let Some(state) = self.workspaces.get_mut(workspace_id)
            && state.large_gap_reserved_delivery == Some(token)
        {
            state.large_gap_reserved_delivery = None;
            if delivered {
                state.large_gap_delivered = true;
            }
        }
        self.remove_uncommitted_advisory_identities();
        if large_gap_capacity_busy && !delivered {
            self.capacity_busy_deliveries.remove(&token);
        }
        if delivered
            && large_gap_capacity_busy
            && self
                .capacity_busy_deliveries
                .get(&token)
                .is_some_and(|pending_workspace_id| pending_workspace_id == workspace_id)
        {
            if self.workspace_mut(workspace_id).is_none() {
                return SearchAdvisorySettlement::RetryCapacityBusy;
            }
            self.capacity_busy_deliveries.remove(&token);
            let Some(state) = self.workspaces.get_mut(workspace_id) else {
                return SearchAdvisorySettlement::RetryCapacityBusy;
            };
            state.large_gap_occurrence_count = state.large_gap_occurrence_count.saturating_add(1);
            state.large_gap_delivered = true;
            state.large_gap_reserved_delivery = None;
        }
        SearchAdvisorySettlement::Complete
    }

    fn commit_delivery_accounting(&mut self, accounting: SearchAdvisoryDeliveryAccounting) {
        for (identity, occurrence_count) in accounting.permanent_occurrences {
            let entry = self.permanent_occurrences.entry(identity).or_default();
            entry.occurrence_count = entry.occurrence_count.saturating_add(occurrence_count);
        }
        self.permanent_suppressed_count = self
            .permanent_suppressed_count
            .saturating_add(accounting.permanent_suppressed_count);
        if let Some(state) = self.workspaces.get_mut(&accounting.workspace_id) {
            state.large_gap_occurrence_count = state
                .large_gap_occurrence_count
                .saturating_add(accounting.large_gap_occurrence_count);
        }
    }

    fn remove_uncommitted_advisory_identities(&mut self) {
        let pending_identities = self
            .delivery_accounting
            .values()
            .flat_map(|accounting| accounting.permanent_occurrences.keys().cloned())
            .collect::<BTreeSet<_>>();
        self.permanent_occurrences.retain(|identity, entry| {
            entry.occurrence_count > 0
                || entry.delivered
                || entry.reserved_delivery.is_some()
                || pending_identities.contains(identity)
        });
    }

    #[must_use]
    #[cfg(test)]
    pub(crate) fn tracked_workspace_count(&self) -> usize {
        self.workspaces.len()
    }
}

#[derive(Clone, Debug)]
pub struct SearchPerformanceRun {
    pub report: SearchReport,
    pub performance: SearchPerformanceTrace,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SearchPerformanceTrace {
    timings: Vec<SearchPerformanceTiming>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SearchPerformanceTiming {
    name: &'static str,
    elapsed: Duration,
}

impl SearchPerformanceTrace {
    fn record_elapsed(&mut self, name: &'static str, start: Instant) {
        self.record_duration(name, start.elapsed());
    }

    fn record_duration(&mut self, name: &'static str, elapsed: Duration) {
        self.timings.push(SearchPerformanceTiming { name, elapsed });
    }

    pub(crate) fn timings(&self) -> impl Iterator<Item = (&'static str, Duration)> + '_ {
        self.timings
            .iter()
            .map(|timing| (timing.name, timing.elapsed))
    }

    #[must_use]
    pub(crate) fn timings_json(&self) -> Vec<serde_json::Value> {
        self.timings
            .iter()
            .map(search_performance_timing_json)
            .collect()
    }
}

#[derive(Clone, Debug)]
pub struct ContextSearchReport {
    pub report: SearchReport,
    pub preloaded_memories: BTreeMap<String, StoredMemory>,
    pub performance: SearchPerformanceTrace,
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
    pub hash_control: usize,
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

impl SearchHit {
    /// Deterministic normalized relevance score in `0.0..=1.0` (bd-1et0v.11).
    ///
    /// See [`normalized_relevance_score`]. Surfaced as `relevanceScore` so an
    /// agent reads relevance from a uniform `0..1` scale instead of the raw,
    /// source-dependent `score`.
    #[must_use]
    pub fn relevance_score(&self) -> f32 {
        normalized_relevance_score(self.source, self.score)
    }

    /// Interpretation tag for the `score` scale (bd-1et0v.11).
    ///
    /// See [`ScoreSource::score_kind`]. Surfaced as `scoreKind`.
    #[must_use]
    pub const fn score_kind(&self) -> &'static str {
        self.source.score_kind()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TypedMemoryFieldOperator {
    Exact,
    Contains,
    Prefix,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TypedMemoryFieldFilter {
    pub field: String,
    pub value: String,
    pub operator: TypedMemoryFieldOperator,
}

impl TypedMemoryFieldFilter {
    /// Parse `field=value`, `field~substring`, or `field^prefix` filter syntax.
    ///
    /// Field names normalize `-` to `_` so CLI callers can use either
    /// `reverted-at-sha=...` or `reverted_at_sha=...`.
    pub fn parse(raw: &str) -> Result<Self, String> {
        let Some((separator_index, operator)) = typed_memory_filter_separator(raw) else {
            return Err(
                "typed memory field filters must use NAME=VALUE, NAME~VALUE, or NAME^VALUE"
                    .to_owned(),
            );
        };
        let field = &raw[..separator_index];
        let value = &raw[separator_index + 1..];
        let field = normalize_typed_memory_filter_field(field)?;
        let value = value.trim();
        if value.is_empty() {
            return Err("typed memory field filter value must not be empty".to_owned());
        }
        Ok(Self {
            field,
            value: value.to_owned(),
            operator,
        })
    }
}

fn typed_memory_filter_separator(raw: &str) -> Option<(usize, TypedMemoryFieldOperator)> {
    raw.char_indices()
        .filter_map(|(index, ch)| {
            let operator = match ch {
                '=' => TypedMemoryFieldOperator::Exact,
                '~' => TypedMemoryFieldOperator::Contains,
                '^' => TypedMemoryFieldOperator::Prefix,
                _ => return None,
            };
            Some((index, operator))
        })
        .min_by_key(|(index, _)| *index)
}

fn normalize_typed_memory_filter_field(raw: &str) -> Result<String, String> {
    crate::models::memory::normalize_typed_memory_field_name(raw)
        .map_err(|error| error.replace("typed memory field", "typed memory field filter"))
}

pub fn normalize_memory_kind_filter(raw: &str) -> Result<String, String> {
    let kind = crate::models::MemoryKind::from_str(raw).map_err(|error| error.to_string())?;
    Ok(kind.as_str().to_owned())
}

pub fn apply_memory_kind_and_typed_field_filters_to_report(
    options: &SearchOptions,
    report: &mut SearchReport,
    kind_filter: Option<&str>,
    typed_field_filters: &[TypedMemoryFieldFilter],
) -> Result<(), SearchError> {
    if kind_filter.is_none() && typed_field_filters.is_empty() {
        return Ok(());
    }

    let database_path = options.resolve_database_path();
    let connection = DbConnection::open_file_read_only(&database_path).map_err(|error| {
        SearchError::Index(format!(
            "Failed to open database for typed memory filters: {error}"
        ))
    })?;
    let result = apply_memory_kind_and_typed_field_filters_to_report_with_connection(
        &connection,
        report,
        kind_filter,
        typed_field_filters,
    );
    if let Err(error) = connection.close() {
        tracing::warn!(
            target: "ee::search",
            event = "typed_memory_filter_connection_close_failed",
            database_path = %database_path.display(),
            error = %error,
        );
    }
    result
}

fn apply_memory_kind_and_typed_field_filters_to_report_with_connection(
    connection: &DbConnection,
    report: &mut SearchReport,
    kind_filter: Option<&str>,
    typed_field_filters: &[TypedMemoryFieldFilter],
) -> Result<(), SearchError> {
    if kind_filter.is_none() && typed_field_filters.is_empty() {
        return Ok(());
    }

    let mut filtered = Vec::with_capacity(report.results.len());
    for hit in std::mem::take(&mut report.results) {
        if typed_memory_hit_matches(connection, &hit, kind_filter, typed_field_filters)? {
            filtered.push(hit);
        }
    }
    report.results = filtered;
    if report.results.is_empty() && report.status == SearchStatus::Success {
        report.status = SearchStatus::NoResults;
    }
    Ok(())
}

fn typed_memory_hit_matches(
    connection: &DbConnection,
    hit: &SearchHit,
    kind_filter: Option<&str>,
    typed_field_filters: &[TypedMemoryFieldFilter],
) -> Result<bool, SearchError> {
    if !hit.doc_id.starts_with("mem_") {
        return Ok(false);
    }
    let Some(memory) = connection.get_memory(&hit.doc_id).map_err(|error| {
        SearchError::Index(format!(
            "Failed to load memory for typed search filter: {error}"
        ))
    })?
    else {
        return Ok(false);
    };
    if let Some(kind_filter) = kind_filter
        && memory.kind != kind_filter
    {
        return Ok(false);
    }
    let kind = crate::models::MemoryKind::from_str(&memory.kind).map_err(|error| {
        SearchError::Index(format!(
            "Failed to parse memory kind for typed search filter: {error}"
        ))
    })?;
    if typed_field_filters.is_empty() {
        return Ok(true);
    }
    let typed_fields_json = connection
        .get_memory_typed_fields_json(&hit.doc_id)
        .map_err(|error| {
            SearchError::Index(format!(
                "Failed to load typed memory fields for search filter: {error}"
            ))
        })?;
    Ok(typed_fields_match_filters(
        &kind,
        typed_fields_json.as_deref(),
        typed_field_filters,
    ))
}

fn typed_fields_match_filters(
    kind: &crate::models::MemoryKind,
    typed_fields_json: Option<&str>,
    filters: &[TypedMemoryFieldFilter],
) -> bool {
    let Some(typed_fields_json) = typed_fields_json else {
        return filters.is_empty();
    };
    let Ok(fields) = crate::models::memory::typed_memory_fields_from_json(kind, typed_fields_json)
    else {
        return false;
    };
    filters.iter().all(|filter| {
        fields
            .get(&filter.field)
            .is_some_and(|value| typed_field_value_matches(value, filter))
    })
}

fn typed_field_value_matches(value: &serde_json::Value, filter: &TypedMemoryFieldFilter) -> bool {
    match value {
        serde_json::Value::String(actual) => typed_field_string_matches(actual, filter),
        serde_json::Value::Array(values) => values.iter().any(|item| {
            item.as_str()
                .is_some_and(|actual| typed_field_string_matches(actual, filter))
        }),
        _ => false,
    }
}

fn typed_field_string_matches(actual: &str, filter: &TypedMemoryFieldFilter) -> bool {
    match filter.operator {
        TypedMemoryFieldOperator::Exact => actual == filter.value.as_str(),
        TypedMemoryFieldOperator::Contains => actual.contains(filter.value.as_str()),
        TypedMemoryFieldOperator::Prefix => actual.starts_with(filter.value.as_str()),
    }
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
    /// Whether this entry describes a stable capability posture rather than a
    /// transient degradation that belongs in every query response.
    #[must_use]
    pub fn is_permanent(&self) -> bool {
        self.code == "rerank_model_unavailable" && self.message == RERANK_MODEL_UNAVAILABLE_ADVISORY
    }

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
    fn from_model_lifecycle(degradation: &crate::core::model::ModelLifecycleDegradation) -> Self {
        Self {
            code: degradation.code.to_string(),
            severity: degradation.severity.to_string(),
            message: degradation.message.clone(),
            repair: degradation.repair.clone(),
        }
    }

    #[must_use]
    pub(crate) fn rerank_model_absent() -> Self {
        Self {
            code: "rerank_model_unavailable".to_string(),
            severity: "low".to_string(),
            message: RERANK_MODEL_UNAVAILABLE_ADVISORY.to_string(),
            repair: None,
        }
    }

    #[must_use]
    fn rerank_model_unavailable(reason: &str) -> Self {
        Self {
            code: "rerank_model_unavailable".to_string(),
            severity: "low".to_string(),
            message: format!(
                "Search rerank is in auto mode, but the local reranker is unavailable; returning fusion-only ranking. {reason}"
            ),
            repair: None,
        }
    }

    #[must_use]
    pub(crate) fn stale_index(db_generation: Option<u64>, index_generation: Option<u64>) -> Self {
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
                "{SEARCH_INDEX_STALE_WARNING_PREFIX}{generation_detail}{SEARCH_INDEX_STALE_WARNING_SUFFIX}"
            ),
            repair: Some("ee index rebuild --workspace .".to_string()),
        }
    }

    #[must_use]
    pub(crate) fn large_index_gap(db_generation: u64, index_generation: u64) -> Self {
        let generation_gap = db_generation.saturating_sub(index_generation);
        Self {
            code: "search_index_large_gap".to_owned(),
            severity: "medium".to_owned(),
            message: format!(
                "Search index generation gap is {generation_gap}, above the automatic reconciliation limit of {SEARCH_INDEX_LARGE_GAP_THRESHOLD}; automatic read repair was skipped."
            ),
            repair: Some("ee index rebuild --workspace .".to_owned()),
        }
    }

    #[must_use]
    fn index_status_probe_failed() -> Self {
        Self {
            code: "search_index_degraded".to_owned(),
            severity: "medium".to_owned(),
            message: "Search is compiled, but the index is missing from the freshness status because its status probe failed; retrieval continued without claiming that the index is current."
                .to_owned(),
            repair: Some("Run `ee index status --workspace . --json`.".to_owned()),
        }
    }

    #[must_use]
    fn evidence_live_admission_filtered(filtered: usize) -> Self {
        Self {
            code: "evidence_live_admission_filtered".to_string(),
            severity: "warning".to_string(),
            message: format!(
                "Filtered {filtered} indexed evidence candidate{} because current source-of-truth admission could not be verified.",
                if filtered == 1 { "" } else { "s" },
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
    fn no_relevant_results(floor: f32, considered: usize, top_score: Option<f32>) -> Self {
        let top_note = top_score
            .map(|score| format!(" Top candidate scored {score:.4}."))
            .unwrap_or_default();
        Self {
            code: "no_relevant_results".to_string(),
            severity: "medium".to_string(),
            message: format!(
                "No memories scored above relevance floor {floor:.4} (considered {considered} candidate{plural}).{top_note}",
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
                "Embedding model unavailable ({reason}); semantic similarity is disabled and lexical search remains available."
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
    fn lexical_ram_tier_heap_warmload() -> Self {
        Self {
            code: LEXICAL_RAM_TIER_HEAP_WARMLOAD_CODE.to_string(),
            severity: "info".to_string(),
            message: "Lexical RAM-tier pinning is enabled; ee retained lexical index bytes in process heap memory but did not claim OS-level mmap/mlock pinning.".to_string(),
            repair: Some("Use the heap warmload path as an advisory optimization, or land the audited mmap/mlock adapter before requiring OS-level pinning.".to_string()),
        }
    }

    #[must_use]
    fn lexical_ram_unavailable_on_macos() -> Self {
        Self {
            code: LEXICAL_RAM_UNAVAILABLE_ON_MACOS_CODE.to_string(),
            severity: "info".to_string(),
            message: "Lexical RAM-tier pinning is enabled on macOS, where the Linux RAM-tier optimization is unavailable; search results are unchanged.".to_string(),
            repair: Some("Run ee on a Linux 256GB+ host for lexical posting-list RAM-tier pinning.".to_string()),
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
    fn global_memory_disabled(reason: super::global_store::GlobalInclusionReason) -> Self {
        Self {
            code: super::global_store::GLOBAL_MEMORY_DISABLED_CODE.to_string(),
            severity: "info".to_string(),
            message: format!(
                "Global memory lane did not contribute to this search: {}.",
                reason.as_str()
            ),
            repair: Some(match reason {
                super::global_store::GlobalInclusionReason::StoreAbsent => {
                    "Create a global memory with `ee remember --global ...`.".to_string()
                }
                super::global_store::GlobalInclusionReason::NotParticipating => {
                    "Enable this workspace's global-lane participation before global retrieval."
                        .to_string()
                }
                super::global_store::GlobalInclusionReason::DisabledByConfig => {
                    "Set `[memory] include_global = true` in .ee/config.toml.".to_string()
                }
                super::global_store::GlobalInclusionReason::DisabledByFlag => {
                    "Retry without --no-global.".to_string()
                }
                super::global_store::GlobalInclusionReason::Included => {
                    "No repair needed.".to_string()
                }
            }),
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

fn search_index_gap_is_large(db_generation: u64, index_generation: u64) -> bool {
    db_generation.saturating_sub(index_generation) > SEARCH_INDEX_LARGE_GAP_THRESHOLD
}

fn search_index_gap_is_auto_reconcilable(db_generation: u64, index_generation: u64) -> bool {
    let generation_gap = db_generation.saturating_sub(index_generation);
    generation_gap > 0 && generation_gap <= SEARCH_INDEX_LARGE_GAP_THRESHOLD
}

fn search_index_corpus_is_auto_reconcilable(corpus_documents: u64) -> bool {
    corpus_documents <= SEARCH_INDEX_AUTO_RECONCILE_MAX_DOCUMENTS
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
    HashControl,
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
            Self::HashControl => "hash_control",
        }
    }

    /// Interpretation tag for the `score` scale (bd-1et0v.11).
    ///
    /// Lexical-only results expose a unit-normalized ranking score while
    /// retaining raw BM25 in `lexicalScore`. Semantic and hash-control results
    /// expose raw cosine similarity. `Hybrid` carries an RRF-fused magnitude
    /// that tops out near [`RRF_HYBRID_TYPICAL_MAX`] (`~0.033`). `Reranked`
    /// means the score is the cross-encoder score carried in `rerankScore`.
    #[must_use]
    pub const fn score_kind(self) -> &'static str {
        match self {
            Self::Hybrid => "rrf_fused",
            Self::Reranked => "reranked",
            Self::Lexical => "unit_normalized",
            Self::SemanticFast | Self::SemanticQuality | Self::HashControl => "cosine_similarity",
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

        output.push_str(&format!("embed_backend: {}\n\n", self.embed_backend));

        for (i, hit) in visible_results.iter().enumerate() {
            let attribution = team_search_attribution_suffix(hit).unwrap_or_default();
            output.push_str(&format!(
                "  {}. {}{} (relevance: {:.4}, score: {:.4} {}, source: {})\n",
                i + 1,
                hit.doc_id,
                attribution,
                hit.relevance_score(),
                hit.score,
                hit.score_kind(),
                hit.source.as_str()
            ));
            // Show a one-line body preview so a human/agent can tell what each
            // hit says without a follow-up `ee memory show`/`ee why`. (item 1)
            if let Some(text) = hit.metadata.as_ref().and_then(search_hit_content_text) {
                output.push_str(&format!(
                    "     {}\n",
                    search_content_preview(&text, SEARCH_CONTENT_PREVIEW_MAX_CHARS)
                ));
            }
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

        let transient_degraded = self
            .degraded
            .iter()
            .filter(|degradation| !degradation.is_permanent())
            .collect::<Vec<_>>();
        if !transient_degraded.is_empty() {
            output.push_str("\nDegraded:\n");
            for degraded in transient_degraded {
                output.push_str(&format!("  - {}: {}\n", degraded.code, degraded.message));
            }
        }

        output
    }

    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        let mut session = SearchAdvisorySession::default();
        self.data_json_with_advisory_session_inner(
            &mut session,
            DEFAULT_SEARCH_ADVISORY_WORKSPACE,
            SEARCH_ADVISORY_SCOPE_INVOCATION,
            None,
        )
    }

    /// Render JSON while recording permanent advisories in the supplied
    /// bounded active-episode ledger.
    ///
    /// The ordinary renderer intentionally uses a fresh session so repeated
    /// serialization stays pure and deterministic. Long-lived owners such as
    /// the daemon supply and retain their own session explicitly.
    #[must_use]
    pub fn data_json_with_advisory_session(
        &self,
        session: &mut SearchAdvisorySession,
    ) -> serde_json::Value {
        self.data_json_with_advisory_session_for_workspace(
            session,
            DEFAULT_SEARCH_ADVISORY_WORKSPACE,
        )
    }

    /// Render JSON against one workspace partition of a long-lived advisory
    /// ledger. Daemon callers must pass the authorized request workspace so
    /// advisory episodes cannot cross-suppress unrelated workspaces.
    #[must_use]
    pub fn data_json_with_advisory_session_for_workspace(
        &self,
        session: &mut SearchAdvisorySession,
        workspace_id: &str,
    ) -> serde_json::Value {
        self.data_json_with_advisory_session_inner(
            session,
            workspace_id,
            SEARCH_ADVISORY_SCOPE_PROCESS,
            None,
        )
    }

    #[must_use]
    pub(crate) fn data_json_with_advisory_delivery_reservation(
        &self,
        session: &mut SearchAdvisorySession,
        workspace_id: &str,
        reservation: &mut SearchAdvisoryDeliveryReservation,
    ) -> serde_json::Value {
        self.data_json_with_advisory_session_inner(
            session,
            workspace_id,
            SEARCH_ADVISORY_SCOPE_PROCESS,
            Some(reservation),
        )
    }

    fn data_json_with_advisory_session_inner(
        &self,
        session: &mut SearchAdvisorySession,
        workspace_id: &str,
        advisory_scope: &'static str,
        mut reservation: Option<&mut SearchAdvisoryDeliveryReservation>,
    ) -> serde_json::Value {
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
                    "relevanceScore": round_metric_f32(hit.relevance_score()),
                    "scoreKind": hit.score_kind(),
                    "scoreInterval": search_hit_score_interval_json(hit),
                    "coverageGuarantee": search_hit_coverage_guarantee_json(hit),
                    "calibrated": search_hit_calibrated_json(hit),
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
                        // Promote a short, redaction-safe body preview to the top
                        // level so agents don't have to reach into `metadata.content`
                        // (or make a follow-up `ee memory show`). Derived from the
                        // already-redacted public metadata. (agent-UX item 1)
                        // Canonical field policy (D1, bd-17c65.4.1): the key is
                        // `content` — `contentPreview` is a forbidden synonym —
                        // with `content_truncated` marking elided values.
                        if let Some(text) = search_hit_content_text(&metadata) {
                            let preview =
                                search_content_preview(&text, SEARCH_CONTENT_PREVIEW_MAX_CHARS);
                            let truncated =
                                preview.chars().count() > SEARCH_CONTENT_PREVIEW_MAX_CHARS;
                            obj_map.insert("content".to_string(), serde_json::json!(preview));
                            if truncated {
                                obj_map.insert(
                                    "content_truncated".to_string(),
                                    serde_json::json!(true),
                                );
                            }
                        }
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
        let rerank = search_rerank_posture_json_inner(
            visible_results
                .iter()
                .filter(|hit| hit.rerank_score.is_some())
                .count(),
            &self.degraded,
            self.rerank_configured_mode,
            self.rerank_configured_top_k,
            self.rerank_runtime_available,
            session,
            workspace_id,
            advisory_scope,
            reservation.as_deref_mut(),
        );
        let degraded = search_degraded_data_json_with_advisory_session_inner(
            "search",
            &self.degraded,
            session,
            workspace_id,
            reservation,
        );

        let mut data = serde_json::json!({
            "command": "search",
            "status": self.status.as_str(),
            "embed_backend": self.embed_backend.as_str(),
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
            "rerank": rerank,
            "profileRuntime": self.runtime_profile.data_json(),
            "errors": self.errors,
            "degraded": degraded,
        });
        if let Some(query_assist) = &self.query_assist
            && let Some(data_object) = data.as_object_mut()
        {
            data_object.insert(
                "queryAssist".to_owned(),
                query_assist.data_json(output_redaction_enabled),
            );
        }
        // Authoritative freshness truth is emitted on every affected response,
        // independent of advisory-episode dedupe, so suppressed warnings never
        // hide stale/generation/gap state (bd-index-auto-freshness-m5kwf).
        if let Some(index_freshness) = self.index_freshness
            && let Some(data_object) = data.as_object_mut()
        {
            data_object.insert(
                "indexFreshness".to_owned(),
                serde_json::json!(index_freshness),
            );
        }
        data
    }

    #[must_use]
    pub fn performance_explain_json(
        &self,
        speed: SpeedMode,
        score_explanations_requested: bool,
    ) -> serde_json::Value {
        self.performance_explain_json_with_trace(
            speed,
            score_explanations_requested,
            &SearchPerformanceTrace::default(),
        )
    }

    #[must_use]
    pub fn performance_explain_json_with_trace(
        &self,
        speed: SpeedMode,
        score_explanations_requested: bool,
        performance: &SearchPerformanceTrace,
    ) -> serde_json::Value {
        serde_json::json!({
            "schema": PERFORMANCE_EXPLAIN_SCHEMA_V1,
            "success": true,
            "data": self.performance_explain_data_json_with_trace(
                speed,
                score_explanations_requested,
                performance,
            ),
        })
    }

    #[must_use]
    pub fn performance_explain_data_json(
        &self,
        speed: SpeedMode,
        score_explanations_requested: bool,
    ) -> serde_json::Value {
        self.performance_explain_data_json_with_trace(
            speed,
            score_explanations_requested,
            &SearchPerformanceTrace::default(),
        )
    }

    #[must_use]
    pub fn performance_explain_data_json_with_trace(
        &self,
        speed: SpeedMode,
        score_explanations_requested: bool,
        performance: &SearchPerformanceTrace,
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
                "timings": performance.timings_json(),
            },
            "timings": performance.timings_json(),
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
            "fallbacks": search_performance_fallbacks_json("search", &self.degraded),
            "redaction": performance_redaction_json(),
        })
    }
}

fn search_performance_fallbacks_json(
    source: &'static str,
    degraded: &[SearchDegradation],
) -> Vec<serde_json::Value> {
    aggregate_degraded_entries(
        degraded
            .iter()
            .filter(|entry| !entry.is_permanent())
            .map(|entry| {
                DegradationAggregationInput::new(
                    source,
                    entry.code.clone(),
                    entry.severity.clone(),
                    PERFORMANCE_FALLBACK_REDACTED_MESSAGE.to_owned(),
                    String::new(),
                )
            }),
    )
    .into_iter()
    .map(|entry| {
        serde_json::json!({
            "code": entry.code,
            "severity": entry.severity,
            "message": PERFORMANCE_FALLBACK_REDACTED_MESSAGE,
            "sources": entry.sources,
        })
    })
    .collect()
}

pub(crate) fn search_degraded_data_json(
    source: &'static str,
    degraded: &[SearchDegradation],
) -> Vec<serde_json::Value> {
    aggregate_degraded_entries(
        degraded
            .iter()
            .filter(|entry| !entry.is_permanent())
            .map(|entry| {
                DegradationAggregationInput::new(
                    source,
                    entry.code.clone(),
                    entry.severity.clone(),
                    entry.message.clone(),
                    entry.repair.clone().unwrap_or_default(),
                )
            }),
    )
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

fn search_degraded_data_json_with_advisory_session_inner(
    source: &'static str,
    degraded: &[SearchDegradation],
    session: &mut SearchAdvisorySession,
    workspace_id: &str,
    reservation: Option<&mut SearchAdvisoryDeliveryReservation>,
) -> Vec<serde_json::Value> {
    let mut aggregated = search_degraded_data_json(source, degraded);
    let has_large_gap = aggregated.iter().any(|entry| {
        entry.get("code").and_then(serde_json::Value::as_str) == Some("search_index_large_gap")
    });
    let emit_large_gap = match reservation {
        Some(reservation) => session.emit_large_gap_while_active_for_delivery(
            workspace_id,
            has_large_gap,
            reservation,
        ),
        None => session.emit_large_gap_while_active(workspace_id, has_large_gap),
    }
    .should_emit();
    aggregated.retain(|entry| {
        let Some(code) = entry.get("code").and_then(serde_json::Value::as_str) else {
            return true;
        };
        match code {
            // The large-gap stale warning is once per active workspace
            // episode: the first affected response carries both the stale
            // warning and the large-gap repair notice, and every later
            // response in the same episode carries neither. Authoritative
            // stale/generation/gap truth stays per-response in the structured
            // `indexFreshness` field, built from the index-status probe
            // rather than advisory prose. Small-gap staleness keeps the
            // canonical per-response warning (bd-index-auto-freshness-m5kwf).
            "search_index_large_gap" => emit_large_gap,
            "search_index_stale" => !has_large_gap || emit_large_gap,
            _ => true,
        }
    });
    aggregated
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
                    "Selected by {} retrieval with relevance {:.4} from score {:.4} ({}).",
                    self.source.as_str(),
                    self.relevance_score(),
                    self.score,
                    self.score_kind(),
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
            for key in [
                SEARCH_ANALYSIS_PROVENANCE_URI_KEY,
                "provenanceUri",
                "provenance_uri",
            ] {
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

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct SearchScoreCalibrationCacheKey {
    workspace_path: PathBuf,
    database_path: PathBuf,
    workspace_id: String,
    feedback_fingerprint: SearchScoreCalibrationFeedbackFingerprint,
    jsonl_fingerprint: SearchScoreCalibrationJsonlCacheFingerprint,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
enum SearchScoreCalibrationFeedbackFingerprint {
    NoDatabase,
    Available(FeedbackEventsFingerprint),
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
enum SearchScoreCalibrationJsonlCacheFingerprint {
    Absent,
    Unreadable(String),
    Present {
        len: u64,
        modified: Option<(u64, u32)>,
    },
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

fn search_score_calibration_jsonl_path(workspace_path: &Path) -> PathBuf {
    workspace_path
        .join(".ee")
        .join("search")
        .join("calibration.jsonl")
}

fn search_score_calibration_jsonl_cache_fingerprint(
    workspace_path: &Path,
) -> SearchScoreCalibrationJsonlCacheFingerprint {
    let path = search_score_calibration_jsonl_path(workspace_path);
    match std::fs::metadata(path) {
        Ok(metadata) => SearchScoreCalibrationJsonlCacheFingerprint::Present {
            len: metadata.len(),
            modified: metadata
                .modified()
                .ok()
                .and_then(|modified| modified.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|duration| (duration.as_secs(), duration.subsec_nanos())),
        },
        Err(error) => match classify_calibration_io_error(&error) {
            None => SearchScoreCalibrationJsonlCacheFingerprint::Absent,
            Some(reason) => {
                SearchScoreCalibrationJsonlCacheFingerprint::Unreadable(reason.to_owned())
            }
        },
    }
}

fn search_score_calibration_feedback_fingerprint(
    workspace_path: &Path,
    database_path: &Path,
    read_connection: Option<&DbConnection>,
    workspace_id: &str,
) -> Option<SearchScoreCalibrationFeedbackFingerprint> {
    if let Some(connection) = read_connection {
        return connection
            .feedback_events_fingerprint(workspace_id)
            .ok()
            .map(SearchScoreCalibrationFeedbackFingerprint::Available);
    }

    if !database_path.exists() {
        return Some(SearchScoreCalibrationFeedbackFingerprint::NoDatabase);
    }

    DbConnection::open_file_read_only(database_path)
        .ok()
        .and_then(|connection| {
            let fingerprint = connection.feedback_events_fingerprint(workspace_id).ok();
            let _ = connection.close();
            fingerprint
        })
        .map(SearchScoreCalibrationFeedbackFingerprint::Available)
        .or_else(|| {
            tracing::debug!(
                target: "ee::search::score_calibration",
                workspace = %workspace_path.display(),
                database_path = %database_path.display(),
                "feedback-events fingerprint unavailable; falling back to uncached calibration"
            );
            None
        })
}

fn search_score_calibration_for_workspace_cached(
    workspace_path: &Path,
    database_path: Option<&Path>,
    read_connection: Option<&DbConnection>,
) -> SearchScoreCalibration {
    let workspace_id = bound_search_workspace_id(workspace_path, database_path, read_connection);
    let default_database_path = default_workspace_database_path(workspace_path);
    let resolved_database_path = database_path.unwrap_or(&default_database_path);
    let feedback_fingerprint = search_score_calibration_feedback_fingerprint(
        workspace_path,
        resolved_database_path,
        read_connection,
        &workspace_id,
    );
    let Some(feedback_fingerprint) = feedback_fingerprint else {
        let feedback_events = search_score_calibration_feedback_events_with_workspace_id(
            workspace_path,
            Some(resolved_database_path),
            read_connection,
            &workspace_id,
        );
        return SearchScoreCalibration::for_workspace_with_feedback_event_status(
            workspace_path,
            &feedback_events.events,
            feedback_events.unavailable_reason,
        );
    };

    let cache_key = SearchScoreCalibrationCacheKey {
        workspace_path: workspace_path.to_path_buf(),
        database_path: resolved_database_path.to_path_buf(),
        workspace_id: workspace_id.clone(),
        feedback_fingerprint: feedback_fingerprint.clone(),
        jsonl_fingerprint: search_score_calibration_jsonl_cache_fingerprint(workspace_path),
    };
    let cache = SEARCH_SCORE_CALIBRATION_CACHE.get_or_init(|| RwLock::new(HashMap::new()));
    if let Ok(cache_guard) = cache.read()
        && let Some(calibration) = cache_guard.get(&cache_key)
    {
        return calibration.clone();
    }

    #[cfg(test)]
    if let Ok(mut full_loads) = SEARCH_SCORE_CALIBRATION_FEEDBACK_EVENT_FULL_LOADS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
    {
        let count = full_loads.entry(workspace_path.to_path_buf()).or_default();
        *count = count.saturating_add(1);
    }

    let feedback_events = match feedback_fingerprint {
        SearchScoreCalibrationFeedbackFingerprint::NoDatabase => {
            SearchScoreCalibrationFeedbackEvents::available(Vec::new())
        }
        SearchScoreCalibrationFeedbackFingerprint::Available(_) => {
            search_score_calibration_feedback_events_with_workspace_id(
                workspace_path,
                Some(resolved_database_path),
                read_connection,
                &workspace_id,
            )
        }
    };
    let cacheable = feedback_events.unavailable_reason.is_none();
    let calibration = SearchScoreCalibration::for_workspace_with_feedback_event_status(
        workspace_path,
        &feedback_events.events,
        feedback_events.unavailable_reason,
    );
    if cacheable && let Ok(mut cache_guard) = cache.write() {
        cache_guard.insert(cache_key, calibration.clone());
    }
    calibration
}

impl SearchScoreCalibration {
    #[cfg(test)]
    fn for_workspace(workspace_path: &Path) -> Self {
        Self::for_workspace_with_feedback_events(workspace_path, &[])
    }

    #[cfg(test)]
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
        let path = search_score_calibration_jsonl_path(workspace_path);
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
    let calibration = search_score_calibration_for_workspace_cached(
        workspace_path,
        database_path,
        read_connection,
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

    // bd-1h4nu: only advertise a calibrated coverage probability when a
    // conformal residual quantile actually exists. Without one,
    // `interval_for_score` returns the trivial [0,1] band; pairing that with a
    // hardcoded 0.95 "coverageGuarantee" misleads agents into reading the raw
    // RRF score as a 95%-calibrated [0,1] relevance. When uncalibrated we emit
    // `coverageGuarantee: null` and `calibrated: false` so [0,1] reads as
    // "unknown", not "95% sure".
    let calibrated = calibration.residual_quantile.is_some();
    for hit in hits {
        let interval = calibration.interval_for_score(hit.relevance_score());
        let mut metadata = hit
            .metadata
            .take()
            .and_then(|value| value.as_object().cloned())
            .unwrap_or_default();
        metadata.insert(
            "scoreInterval".to_string(),
            serde_json::json!([interval[0], interval[1]]),
        );
        metadata.insert("calibrated".to_string(), serde_json::json!(calibrated));
        metadata.insert(
            "coverageGuarantee".to_string(),
            if calibrated {
                serde_json::json!(round_metric_f32(SEARCH_SCORE_COVERAGE_GUARANTEE))
            } else {
                serde_json::Value::Null
            },
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
    let workspace_id = bound_search_workspace_id(workspace_path, database_path, read_connection);
    search_score_calibration_feedback_events_with_workspace_id(
        workspace_path,
        database_path,
        read_connection,
        &workspace_id,
    )
}

fn search_score_calibration_feedback_events_with_workspace_id(
    workspace_path: &Path,
    database_path: Option<&Path>,
    read_connection: Option<&DbConnection>,
    workspace_id: &str,
) -> SearchScoreCalibrationFeedbackEvents {
    if let Some(connection) = read_connection {
        return match connection.list_feedback_events(workspace_id) {
            Ok(events) => SearchScoreCalibrationFeedbackEvents::available(events),
            Err(_error) => {
                SearchScoreCalibrationFeedbackEvents::unavailable("feedback_events_read_failed")
            }
        };
    }

    let default_database_path = default_workspace_database_path(workspace_path);
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
    match connection.list_feedback_events(workspace_id) {
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
    // bd-1h4nu: absence of a `coverageGuarantee` annotation means calibration
    // never ran for this hit, so the honest default is `null` ("unknown"), not
    // a 0.95 coverage claim over the trivial [0,1] interval.
    hit.metadata
        .as_ref()
        .and_then(|metadata| metadata.get("coverageGuarantee"))
        .cloned()
        .unwrap_or(serde_json::Value::Null)
}

fn search_hit_calibrated_json(hit: &SearchHit) -> serde_json::Value {
    // bd-1h4nu: `calibrated` is the explicit flag agents should branch on. A
    // hit with no calibration metadata is uncalibrated (false), so the
    // scoreInterval/coverageGuarantee pair must be read as "unknown".
    hit.metadata
        .as_ref()
        .and_then(|metadata| metadata.get("calibrated"))
        .cloned()
        .unwrap_or_else(|| serde_json::json!(false))
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

fn search_hit_metadata_tags(hit: &SearchHit) -> Option<Vec<String>> {
    let metadata = hit.metadata.as_ref()?;
    let value = metadata.get("tags")?;
    if let Some(tags) = value.as_array() {
        let parsed = tags
            .iter()
            .filter_map(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|tag| !tag.is_empty())
            .map(str::to_owned)
            .collect::<Vec<_>>();
        return (!parsed.is_empty()).then_some(parsed);
    }
    value.as_str().and_then(|raw| {
        let parsed = raw
            .split(',')
            .map(str::trim)
            .filter(|tag| !tag.is_empty())
            .map(str::to_owned)
            .collect::<Vec<_>>();
        (!parsed.is_empty()).then_some(parsed)
    })
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
        evidence_items: Vec::new(),
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

fn query_assist_visible_candidates(options: &SearchOptions, hits: &[SearchHit]) -> Vec<SearchHit> {
    let mut visible = hits
        .iter()
        .filter(|hit| query_assist_hit_visible(options, hit))
        .cloned()
        .collect::<Vec<_>>();
    sort_search_hits_by_score_order(&mut visible);
    visible
}

fn query_assist_hit_visible(options: &SearchOptions, hit: &SearchHit) -> bool {
    if let Some(metadata) = hit.metadata.as_ref()
        && metadata_bool(metadata, "tombstoned").unwrap_or(false)
        && !options.include_tombstoned
    {
        return false;
    }
    match hit_indexed_validity_status(hit) {
        Some("expired") if !options.include_expired => return false,
        Some("future") if !options.include_future => return false,
        Some("stale") if !options.include_stale => return false,
        _ => {}
    }
    !matches!(
        mesh_query_visibility(hit.metadata.as_ref()),
        MeshQueryVisibility::Blocked
    )
}

fn build_query_assist(
    query: &str,
    explain: bool,
    kept: usize,
    floor_counts: RelevanceFloorCounts,
    floor: f32,
    top_score_after_floor: Option<f32>,
    dropped_below_floor: usize,
    below_floor_candidates: &[SearchHit],
) -> Option<QueryAssistReport> {
    let weak_result_reason = classify_search_query_miss(floor_counts, floor, top_score_after_floor)
        .map(SearchQueryMissReason::as_str)
        .or_else(|| (kept == 0).then_some("empty_results"))?;
    let mode = if explain {
        QueryAssistMode::Explain
    } else {
        QueryAssistMode::Compact
    };
    let did_you_mean = below_floor_candidates
        .iter()
        .take(mode.did_you_mean_limit())
        .cloned()
        .collect::<Vec<_>>();
    let reformulations =
        query_assist_reformulations(query, &did_you_mean, mode.reformulation_limit());
    Some(QueryAssistReport {
        weak_result_reason: weak_result_reason.to_owned(),
        mode,
        reformulations,
        did_you_mean,
        capture_template: QueryAssistCaptureTemplate::for_query(query),
        candidate_count: floor_counts.considered,
        dropped_below_floor,
        relevance_floor: Some(floor),
    })
}

fn query_assist_reformulations(
    query: &str,
    candidates: &[SearchHit],
    limit: usize,
) -> Vec<QueryAssistReformulation> {
    if limit == 0 {
        return Vec::new();
    }
    let query_terms = query_assist_terms(query)
        .into_iter()
        .collect::<BTreeSet<_>>();
    let normalized_query = query.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut seen = BTreeSet::new();
    let mut reformulations = Vec::new();
    for candidate in candidates {
        let terms = query_assist_candidate_terms(candidate, &query_terms);
        if terms.is_empty() {
            continue;
        }
        let joined_terms = terms.join(" ");
        let query_with_terms = if normalized_query.is_empty() {
            joined_terms.clone()
        } else {
            format!("{normalized_query} {joined_terms}")
        };
        push_query_assist_reformulation(
            &mut reformulations,
            &mut seen,
            limit,
            candidate,
            query_with_terms,
            "nearest_memory_terms",
            "Adds salient terms from a semantically near memory that was below the relevance floor.",
        );
        if reformulations.len() >= limit {
            break;
        }

        if let Some(memory_kind_query) = query_assist_memory_kind_query(candidate, &terms) {
            push_query_assist_reformulation(
                &mut reformulations,
                &mut seen,
                limit,
                candidate,
                memory_kind_query,
                "broader_memory_kind",
                "Broadens the query toward the nearest matching memory's level or kind.",
            );
        }
        if reformulations.len() >= limit {
            break;
        }

        push_query_assist_reformulation(
            &mut reformulations,
            &mut seen,
            limit,
            candidate,
            joined_terms,
            "content_terms_only",
            "Drops unmatched wording and searches only terms found in nearby memory content.",
        );
        if reformulations.len() >= limit {
            break;
        }
    }
    reformulations
}

fn push_query_assist_reformulation(
    reformulations: &mut Vec<QueryAssistReformulation>,
    seen: &mut BTreeSet<String>,
    limit: usize,
    candidate: &SearchHit,
    query: String,
    strategy: &'static str,
    rationale: &str,
) {
    if reformulations.len() >= limit {
        return;
    }
    let normalized = query.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty() || !seen.insert(normalized.clone()) {
        return;
    }
    reformulations.push(QueryAssistReformulation {
        query: normalized,
        strategy,
        rationale: rationale.to_owned(),
        matched_doc_id: candidate.doc_id.clone(),
        matched_memory_id: candidate.memory_id().map(str::to_owned),
    });
}

fn query_assist_candidate_terms(hit: &SearchHit, query_terms: &BTreeSet<String>) -> Vec<String> {
    let Some(content) = hit.metadata.as_ref().and_then(search_hit_content_text) else {
        return Vec::new();
    };
    query_assist_terms(&content)
        .into_iter()
        .filter(|term| !query_terms.contains(term))
        .take(QUERY_ASSIST_TERM_LIMIT)
        .collect()
}

fn query_assist_memory_kind_query(hit: &SearchHit, terms: &[String]) -> Option<String> {
    let metadata = hit.metadata.as_ref()?;
    let prefix = metadata_string(metadata, "kind")
        .or_else(|| metadata_string(metadata, "level"))?
        .replace('-', " ");
    let mut parts = vec![prefix];
    parts.extend(terms.iter().take(2).cloned());
    Some(parts.join(" "))
}

fn query_assist_terms(text: &str) -> Vec<String> {
    let normalized = text
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_lowercase()
            } else {
                ' '
            }
        })
        .collect::<String>();
    let mut seen = BTreeSet::new();
    let mut terms = Vec::new();
    for token in normalized.split_whitespace() {
        if token.len() < 3 || query_assist_stopword(token) {
            continue;
        }
        if seen.insert(token.to_owned()) {
            terms.push(token.to_owned());
        }
    }
    terms
}

fn query_assist_stopword(token: &str) -> bool {
    matches!(
        token,
        "the"
            | "and"
            | "for"
            | "with"
            | "that"
            | "this"
            | "from"
            | "into"
            | "your"
            | "you"
            | "are"
            | "was"
            | "were"
            | "has"
            | "have"
            | "had"
            | "not"
            | "but"
            | "use"
            | "run"
            | "memory"
            | "query"
            | "search"
    )
}

fn query_assist_did_you_mean_json(
    hit: &SearchHit,
    output_redaction_enabled: bool,
) -> serde_json::Value {
    let (provenance, provenance_redacted_patterns) = hit.provenance_json(output_redaction_enabled);
    let mut obj = serde_json::json!({
        "docId": hit.doc_id,
        "score": hit.score,
        "relevanceScore": round_metric_f32(hit.relevance_score()),
        "scoreKind": hit.score_kind(),
        "source": hit.source.as_str(),
        "candidateStatus": "below_relevance_floor",
        "why": hit.why(),
        "provenance": provenance,
    });
    if let Some(obj_map) = obj.as_object_mut() {
        if let Some(memory_id) = hit.memory_id() {
            obj_map.insert("memoryId".to_owned(), serde_json::json!(memory_id));
        }
        if let Some(ref meta) = hit.metadata {
            let (metadata, mut redacted_patterns) =
                public_search_metadata(meta, output_redaction_enabled);
            redacted_patterns.extend(provenance_redacted_patterns);
            if let Some(text) = search_hit_content_text(&metadata) {
                let preview = search_content_preview(&text, SEARCH_CONTENT_PREVIEW_MAX_CHARS);
                let truncated = preview.chars().count() > SEARCH_CONTENT_PREVIEW_MAX_CHARS;
                obj_map.insert("content".to_owned(), serde_json::json!(preview));
                if truncated {
                    obj_map.insert("content_truncated".to_owned(), serde_json::json!(true));
                }
            }
            obj_map.insert("metadata".to_owned(), metadata);
            if !redacted_patterns.is_empty() {
                obj_map.insert("contentRedacted".to_owned(), serde_json::json!(true));
                obj_map.insert(
                    "redactions".to_owned(),
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
        }
    }
    obj
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
        relevance: UnitScore::parse(hit.relevance_score()).unwrap_or_else(|_| UnitScore::neutral()),
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
        freshness_facets: Vec::new(),
        attempt_family_multiplicity: None,
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
    let producer = metadata_string(metadata, "trust_subclass")
        .or_else(|| metadata_string(metadata, "producerAgent"))
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
            let relevance_score = hit.relevance_score();
            min_score = Some(min_score.map_or(relevance_score, |score| score.min(relevance_score)));
            max_score = Some(max_score.map_or(relevance_score, |score| score.max(relevance_score)));
            score_sum += relevance_score;
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
                top: hits.first().map(SearchHit::relevance_score),
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
            ScoreSource::HashControl => self.hash_control += 1,
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

#[cfg(test)]
fn search_rerank_posture_json(
    hits: &[&SearchHit],
    degraded: &[SearchDegradation],
    configured_mode: crate::config::SearchRerankMode,
    configured_top_k: usize,
    runtime_available: bool,
    advisory_session: &mut SearchAdvisorySession,
    workspace_id: &str,
) -> serde_json::Value {
    search_rerank_posture_json_inner(
        hits.iter().filter(|hit| hit.rerank_score.is_some()).count(),
        degraded,
        configured_mode,
        configured_top_k,
        runtime_available,
        advisory_session,
        workspace_id,
        SEARCH_ADVISORY_SCOPE_PROCESS,
        None,
    )
}

fn search_rerank_posture_json_inner(
    rerank_score_count: usize,
    degraded: &[SearchDegradation],
    configured_mode: crate::config::SearchRerankMode,
    configured_top_k: usize,
    runtime_available: bool,
    advisory_session: &mut SearchAdvisorySession,
    workspace_id: &str,
    advisory_scope: &'static str,
    reservation: Option<&mut SearchAdvisoryDeliveryReservation>,
) -> serde_json::Value {
    let unavailable = degraded
        .iter()
        .find(|degradation| degradation.code == "rerank_model_unavailable");
    let unavailable_is_permanent = unavailable.is_some_and(SearchDegradation::is_permanent);
    let mode = if rerank_score_count > 0 {
        "reranked"
    } else if unavailable.is_some() {
        "fusion_only_degraded"
    } else {
        "fusion_only"
    };
    let (advisory, advisory_summary) = match unavailable {
        Some(degradation) if unavailable_is_permanent => {
            let observation = match reservation {
                Some(reservation) => advisory_session.observe_permanent_for_delivery(
                    workspace_id,
                    degradation,
                    reservation,
                ),
                None => advisory_session.observe_permanent(workspace_id, degradation),
            };
            let advisory = observation.emitted.then(|| {
                serde_json::json!({
                    "code": degradation.code,
                    "severity": degradation.severity,
                    "permanent": true,
                    "message": degradation.message,
                    "repair": degradation.repair,
                    "resolution": "automatic_repair_unavailable",
                })
            });
            let summary = serde_json::json!({
                "scope": advisory_scope,
                "permanent": true,
                "distinctCount": observation.distinct_count,
                "emittedCount": if observation.emitted { 1 } else { 0 },
                "suppressedCount": if observation.emitted { 0 } else { 1 },
                "sessionOccurrenceCount": observation.occurrence_count,
                "sessionSuppressedCount": observation.session_suppressed_count,
            });
            (advisory, summary)
        }
        Some(degradation) => {
            advisory_session.recover_permanent(workspace_id, "rerank_model_unavailable");
            (
                Some(serde_json::json!({
                "code": degradation.code,
                "severity": degradation.severity,
                "permanent": false,
                "message": degradation.message,
                "repair": degradation.repair,
                "resolution": "retry_or_inspect_local_registry",
                })),
                serde_json::json!({
                    "scope": "response",
                    "permanent": false,
                    "distinctCount": 1,
                    "emittedCount": 1,
                    "suppressedCount": 0,
                    "sessionOccurrenceCount": 1,
                    "sessionSuppressedCount": advisory_session.suppressed_count(workspace_id),
                }),
            )
        }
        None => {
            if runtime_available {
                advisory_session.recover_permanent(workspace_id, "rerank_model_unavailable");
            }
            (
                None,
                serde_json::json!({
                    "scope": "response",
                    "permanent": serde_json::Value::Null,
                    "distinctCount": 0,
                    "emittedCount": 0,
                    "suppressedCount": 0,
                    "sessionOccurrenceCount": 0,
                    "sessionSuppressedCount": advisory_session.suppressed_count(workspace_id),
                }),
            )
        }
    };
    serde_json::json!({
        "schema": "ee.rerank_posture.v1",
        "mode": mode,
        "configured": configured_mode.as_str(),
        "topK": configured_top_k,
        "rerankScoreCount": rerank_score_count,
        "scoreKind": if rerank_score_count > 0 { "reranked" } else { "rrf_fused" },
        "available": runtime_available,
        "degradedCode": unavailable.map(|degradation| degradation.code.as_str()),
        "advisory": advisory,
        "advisorySummary": advisory_summary,
    })
}

/// Render only the delivery-sensitive search fields needed by a cached
/// context pack. The caller supplies the authoritative inputs captured with
/// the fresh search; this deliberately does not infer posture from cached
/// advisory prose.
pub(crate) fn search_advisory_snapshot_data_json_with_delivery_reservation(
    rerank_score_count: usize,
    degraded: &[SearchDegradation],
    configured_mode: crate::config::SearchRerankMode,
    configured_top_k: usize,
    runtime_available: bool,
    session: &mut SearchAdvisorySession,
    workspace_id: &str,
    mut reservation: Option<&mut SearchAdvisoryDeliveryReservation>,
) -> serde_json::Value {
    let rerank = search_rerank_posture_json_inner(
        rerank_score_count,
        degraded,
        configured_mode,
        configured_top_k,
        runtime_available,
        session,
        workspace_id,
        SEARCH_ADVISORY_SCOPE_PROCESS,
        reservation.as_deref_mut(),
    );
    let degraded = search_degraded_data_json_with_advisory_session_inner(
        "search",
        degraded,
        session,
        workspace_id,
        reservation,
    );
    serde_json::json!({
        "rerank": rerank,
        "degraded": degraded,
    })
}

fn search_performance_timing_json(timing: &SearchPerformanceTiming) -> serde_json::Value {
    let mut value = elapsed_timing_json(timing.elapsed.as_secs_f64() * 1000.0);
    if let Some(object) = value.as_object_mut() {
        object.insert(
            "name".to_string(),
            serde_json::Value::String(timing.name.to_string()),
        );
    }
    value
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
    Cancelled(asupersync::CancelReason),
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
            Self::Cancelled(_) => None,
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
            Self::Cancelled(reason) => f.write_str(&crate::core::outcome::cancel_message(reason)),
            Self::SourceModeUnavailable { requested, reason } => write!(
                f,
                "Requested source mode {} is unavailable: {reason}",
                requested.as_str()
            ),
        }
    }
}

impl std::error::Error for SearchError {}

#[derive(Debug)]
pub enum SimilarError {
    Search(SearchError),
    Storage(DbError),
    MemoryNotFound { memory_id: String },
}

impl SimilarError {
    #[must_use]
    pub fn repair_hint(&self) -> Option<&str> {
        match self {
            Self::Search(error) => error.repair_hint(),
            Self::Storage(_) => Some("ee doctor --json"),
            Self::MemoryNotFound { .. } => Some("ee memory list --json"),
        }
    }
}

impl From<SearchError> for SimilarError {
    fn from(error: SearchError) -> Self {
        Self::Search(error)
    }
}

impl From<DbError> for SimilarError {
    fn from(error: DbError) -> Self {
        Self::Storage(error)
    }
}

impl std::fmt::Display for SimilarError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Search(error) => write!(f, "{error}"),
            Self::Storage(error) => write!(f, "Storage error: {error}"),
            Self::MemoryNotFound { memory_id } => {
                write!(f, "Target memory not found: {memory_id}")
            }
        }
    }
}

impl std::error::Error for SimilarError {}

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
                        "raw BM25 term matching before returned-pool normalization",
                        "lexical_score",
                        "score = minmax(lexical_score, returned_pool)",
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
            ScoreSource::HashControl => {
                if let Some(fast) = hit.fast_score {
                    factors.push(ScoreFactor::new(
                        "hash_control",
                        fast,
                        "hash/FNV/JL control vector similarity",
                        "fast_score",
                        "score = fast_score",
                    ));
                    summary_parts.push(format!("hash control ({:.2})", fast));
                }
            }
        }

        let summary = if summary_parts.is_empty() {
            format!(
                "Relevance {:.4} from {} score {:.4} ({}).",
                hit.relevance_score(),
                hit.source.as_str(),
                hit.score,
                hit.score_kind()
            )
        } else {
            format!(
                "Relevance {:.4} from score {:.4} ({}) via {}.",
                hit.relevance_score(),
                hit.score,
                hit.score_kind(),
                summary_parts.join(", ")
            )
        };

        Self { summary, factors }
    }
}

/// Dedupe a hit list on `docId`, keeping the highest-relevance occurrence
/// of each distinct id. Stable: the position of the first occurrence is
/// preserved; the complete hit is upgraded in place when a more relevant
/// duplicate is found later in the list.
///
/// Returns `(deduped, collapsed_count)`. Bead bd-17c65.2.3 (B3).
fn dedupe_hits_on_doc_id(hits: Vec<SearchHit>) -> (Vec<SearchHit>, usize) {
    // Use a HashMap to track first-seen index per doc_id. Iterate in
    // input order so the first occurrence's index is stable. For each
    // duplicate, compare the cross-source relevance projection and (only if
    // strictly higher) overwrite the stored hit in place — preserving
    // ordering. Raw BM25, cosine, and RRF scores are not directly comparable.
    let mut seen: std::collections::HashMap<String, usize> =
        std::collections::HashMap::with_capacity(hits.len());
    let mut deduped: Vec<SearchHit> = Vec::with_capacity(hits.len());
    let mut collapsed = 0_usize;
    for hit in hits {
        if let Some(&index) = seen.get(&hit.doc_id) {
            collapsed += 1;
            // Upgrade only on strictly higher finite relevance so ties keep
            // the first-seen entry (deterministic). A later finite relevance
            // must still displace an earlier non-finite relevance; otherwise one
            // malformed arm can cause the relevance floor to drop a valid
            // duplicate result.
            let existing_relevance = deduped[index].relevance_score();
            let candidate_relevance = hit.relevance_score();
            let should_replace = match (
                existing_relevance.is_finite(),
                candidate_relevance.is_finite(),
            ) {
                (false, true) => true,
                (true, true) => candidate_relevance > existing_relevance,
                _ => false,
            };
            if should_replace {
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
        && let Ok(connection) = DbConnection::open_file_read_only(&database_path)
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

/// Pre-batch best-effort audit append retained as a parity oracle for
/// `SearchAuditBatch` tests. Production read surfaces use the batched path.
#[cfg(test)]
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SearchQueryMissReason {
    NoRelevantResults,
    WeakQueryRecall,
    LowRecallAfterFloor,
}

impl SearchQueryMissReason {
    const fn as_str(self) -> &'static str {
        match self {
            Self::NoRelevantResults => "no_relevant_results",
            Self::WeakQueryRecall => "weak_query_recall",
            Self::LowRecallAfterFloor => "low_recall_after_floor",
        }
    }
}

fn classify_search_query_miss(
    floor_counts: RelevanceFloorCounts,
    floor: f32,
    top_score_after_floor: Option<f32>,
) -> Option<SearchQueryMissReason> {
    if floor_counts.has_no_relevant_results() {
        return Some(SearchQueryMissReason::NoRelevantResults);
    }
    if let Some(top) = top_score_after_floor
        && top.is_finite()
        && top >= floor
        && top < floor * 2.0
    {
        return Some(SearchQueryMissReason::WeakQueryRecall);
    }
    if floor_counts.is_low_recall() {
        return Some(SearchQueryMissReason::LowRecallAfterFloor);
    }
    None
}

struct SearchQueryMissAuditDetails<'a> {
    query_hash: &'a str,
    reason: SearchQueryMissReason,
    status: SearchStatus,
    kept: usize,
    considered: usize,
    dropped_below_floor: usize,
    floor: f32,
    top_score_before_floor: Option<f32>,
    top_score_after_floor: Option<f32>,
}

fn search_query_miss_audit_details(details: SearchQueryMissAuditDetails<'_>) -> String {
    serde_json::json!({
        "schema": SEARCH_QUERY_MISS_AUDIT_SCHEMA_V1,
        "queryHash": details.query_hash,
        "reason": details.reason.as_str(),
        "status": details.status.as_str(),
        "resultCount": details.kept,
        "candidateCount": details.considered,
        "droppedBelowFloor": details.dropped_below_floor,
        "relevanceFloor": round_metric_f32(details.floor),
        "topScoreBeforeFloor": optional_score_json(details.top_score_before_floor),
        "topScoreAfterFloor": optional_score_json(details.top_score_after_floor),
        "ttlSeconds": QUERY_MISS_AUDIT_TTL_SECONDS,
        "sampling": {
            "strategy": "all_low_utility_searches_v1",
            "sampleRate": QUERY_MISS_AUDIT_SAMPLE_RATE,
            "sampled": true,
            "maxRowsPerSearch": 1,
        },
        "redaction": {
            "strategy": "query_hash_only_v1",
            "rawQueryStored": false,
            "queryTextStored": false,
            "queryVectorStored": false,
        },
    })
    .to_string()
}

/// bd-21gya: per-search audit batch.
///
/// Buffers `search.executed` / `search.miss_recorded` /
/// `search.returned_mem` / `redact_at_output` rows for an enclosing mutating
/// operation that explicitly supplies its write connection. Canonical search
/// never constructs or flushes this batch.
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
    run_search_with_filters(options, None, &[])
}

pub fn run_search_with_filters(
    options: &SearchOptions,
    kind_filter: Option<&str>,
    typed_field_filters: &[TypedMemoryFieldFilter],
) -> Result<SearchReport, SearchError> {
    run_search_with_performance_and_filters(options, kind_filter, typed_field_filters)
        .map(|run| run.report)
}

pub fn run_search_with_performance(
    options: &SearchOptions,
) -> Result<SearchPerformanceRun, SearchError> {
    run_search_with_performance_and_filters(options, None, &[])
}

pub fn run_search_with_performance_and_filters(
    options: &SearchOptions,
    kind_filter: Option<&str>,
    typed_field_filters: &[TypedMemoryFieldFilter],
) -> Result<SearchPerformanceRun, SearchError> {
    crate::core::run_cli_with_cx(Duration::from_secs(30), |cx| async move {
        run_search_with_performance_and_filters_with_cx(
            &cx,
            options,
            kind_filter,
            typed_field_filters,
        )
        .await
    })
    .map_err(|error| SearchError::Index(format!("Failed to start search runtime: {error}")))?
}

/// Execute search with the caller-owned Asupersync request context.
///
/// This is the canonical production seam for CLI handlers, context packing,
/// and deterministic LabRuntime cancellation tests. The synchronous wrapper
/// above only mints a bounded root context and delegates here.
pub async fn run_search_with_performance_and_filters_with_cx(
    cx: &asupersync::Cx,
    options: &SearchOptions,
    kind_filter: Option<&str>,
    typed_field_filters: &[TypedMemoryFieldFilter],
) -> Result<SearchPerformanceRun, SearchError> {
    let total_start = Instant::now();
    search_checkpoint(cx)?;
    let determinism = Deterministic::from_seed(0);
    let mut audit_ids = SearchAuditIdSource::Ambient;

    let index_dir = options.resolve_index_dir();
    let database_path = options.resolve_database_path();
    reconcile_search_index_before_read_with_cx(cx, options).await;
    let embedder_preparation = if options.source_mode.uses_embeddings()
        && index_dir.exists()
        && crate::core::index::index_corpus_compatibility_is_current(&index_dir)
    {
        Some(
            prepare_search_embedder_for_workspace(cx, &options.workspace_path, &database_path)
                .await
                .map_err(|error| {
                    map_frankensearch_error(cx, "search embedder preparation", error)
                })?,
        )
    } else {
        None
    };
    let fast_embedder_override = embedder_preparation
        .as_ref()
        .map(|preparation| Arc::clone(&preparation.fast_embedder));

    if database_path.exists() {
        let read_pool = registered_process_read_pool(
            DatabaseConfig::file(database_path.clone()),
            PoolConfig::default_single(),
        );
        let read_snapshot = read_pool.pin_snapshot().map_err(|error| {
            SearchError::Index(format!(
                "Failed to acquire search read snapshot for {}: {error}",
                database_path.display()
            ))
        })?;
        let result = {
            let connection = read_snapshot.checked_connection().map_err(|error| {
                SearchError::Index(format!(
                    "Search read snapshot became unavailable for {}: {error}",
                    database_path.display()
                ))
            })?;
            if connection.needs_migration().map_err(|error| {
                SearchError::Index(format!(
                    "Failed to inspect database migration state before search: {error}"
                ))
            })? {
                return Err(SearchError::Index(
                    "Database migration is required before read-only search; run `ee migrate run --workspace .`."
                        .to_owned(),
                ));
            }
            run_search_inner_with_performance(
                cx,
                options,
                Some(connection),
                determinism.shared_child("search.rerank"),
                &mut audit_ids,
                None,
                None,
                true,
                None,
                fast_embedder_override.clone(),
            )
            .await
            .and_then(|mut run| {
                apply_memory_kind_and_typed_field_filters_to_report_with_connection(
                    connection,
                    &mut run.report,
                    kind_filter,
                    typed_field_filters,
                )?;
                Ok(run)
            })
        };
        read_snapshot.commit().map_err(|error| {
            SearchError::Index(format!(
                "Failed to release search read snapshot for {}: {error}",
                database_path.display()
            ))
        })?;
        let mut run = result?;
        if let Some(preparation) = embedder_preparation {
            run.performance
                .record_duration("search::embedderPrepare", preparation.elapsed);
            run.report.embed_backend = preparation.backend;
        }
        run.report.elapsed_ms = total_start.elapsed().as_secs_f64() * 1000.0;
        return Ok(run);
    }

    let mut run = run_search_inner_with_performance(
        cx,
        options,
        None,
        determinism.shared_child("search.rerank"),
        &mut audit_ids,
        None,
        None,
        true,
        None,
        fast_embedder_override,
    )
    .await?;
    if kind_filter.is_some() || !typed_field_filters.is_empty() {
        return Err(SearchError::Index(format!(
            "Database not found at {}; typed memory filters require an initialized workspace",
            database_path.display()
        )));
    }
    if let Some(preparation) = embedder_preparation {
        run.performance
            .record_duration("search::embedderPrepare", preparation.elapsed);
        run.report.embed_backend = preparation.backend;
    }
    run.report.elapsed_ms = total_start.elapsed().as_secs_f64() * 1000.0;
    Ok(run)
}

fn search_checkpoint(cx: &asupersync::Cx) -> Result<(), SearchError> {
    cx.checkpoint().map_err(|_| {
        SearchError::Cancelled(cx.cancel_reason().unwrap_or_else(|| {
            crate::core::outcome::attributed_cancel_reason(
                cx,
                asupersync::CancelKind::User,
                "search checkpoint cancelled without a recorded reason",
            )
        }))
    })
}

fn with_search_root<F, Fut, T>(operation: F) -> Result<T, SearchError>
where
    F: FnOnce(asupersync::Cx) -> Fut,
    Fut: std::future::Future<Output = Result<T, SearchError>>,
{
    crate::core::run_cli_with_cx(Duration::from_secs(30), operation)
        .map_err(|error| SearchError::Index(format!("Failed to start search runtime: {error}")))?
}

pub fn run_search_seeded(
    options: &SearchOptions,
    determinism: &Deterministic<Seed>,
) -> Result<SearchReport, SearchError> {
    let rerank_seed = determinism.shared_child("search.rerank");
    with_search_root(|cx| async move { run_search_seeded_with_cx(&cx, options, rerank_seed).await })
}

pub async fn run_search_seeded_with_cx(
    cx: &asupersync::Cx,
    options: &SearchOptions,
    rerank_seed: Deterministic<Seed>,
) -> Result<SearchReport, SearchError> {
    // Search determinism controls ranking/output replay. Audit rows are durable
    // side effects, so they must remain unique across repeated seeded calls.
    let mut audit_ids = SearchAuditIdSource::Ambient;
    run_search_inner(
        cx,
        options,
        None,
        rerank_seed,
        &mut audit_ids,
        None,
        true,
        None,
    )
    .await
}

pub fn run_search_with_read_connection(
    options: &SearchOptions,
    read_connection: &DbConnection,
) -> Result<SearchReport, SearchError> {
    with_search_root(|cx| async move {
        run_search_with_read_connection_with_cx(&cx, options, read_connection).await
    })
}

pub async fn run_search_with_read_connection_with_cx(
    cx: &asupersync::Cx,
    options: &SearchOptions,
    read_connection: &DbConnection,
) -> Result<SearchReport, SearchError> {
    let determinism = Deterministic::from_seed(0);
    let mut audit_ids = SearchAuditIdSource::Ambient;
    run_search_inner(
        cx,
        options,
        Some(read_connection),
        determinism.shared_child("search.rerank"),
        &mut audit_ids,
        None,
        true,
        None,
    )
    .await
}

pub fn run_search_with_read_connection_seeded(
    options: &SearchOptions,
    read_connection: &DbConnection,
    determinism: &Deterministic<Seed>,
) -> Result<SearchReport, SearchError> {
    let rerank_seed = determinism.shared_child("search.rerank");
    with_search_root(|cx| async move {
        run_search_with_read_connection_seeded_with_cx(&cx, options, read_connection, rerank_seed)
            .await
    })
}

pub async fn run_search_with_read_connection_seeded_with_cx(
    cx: &asupersync::Cx,
    options: &SearchOptions,
    read_connection: &DbConnection,
    rerank_seed: Deterministic<Seed>,
) -> Result<SearchReport, SearchError> {
    // Search determinism controls ranking/output replay. Audit rows are durable
    // side effects, so they must remain unique across repeated seeded calls.
    let mut audit_ids = SearchAuditIdSource::Ambient;
    run_search_inner(
        cx,
        options,
        Some(read_connection),
        rerank_seed,
        &mut audit_ids,
        None,
        true,
        None,
    )
    .await
}

pub fn run_search_with_read_connection_seeded_and_audit_connection(
    options: &SearchOptions,
    read_connection: &DbConnection,
    audit_connection: Option<&DbConnection>,
    determinism: &Deterministic<Seed>,
) -> Result<SearchReport, SearchError> {
    let rerank_seed = determinism.shared_child("search.rerank");
    with_search_root(|cx| async move {
        run_search_with_read_connection_seeded_and_audit_connection_with_cx(
            &cx,
            options,
            read_connection,
            audit_connection,
            rerank_seed,
        )
        .await
    })
}

pub async fn run_search_with_read_connection_seeded_and_audit_connection_with_cx(
    cx: &asupersync::Cx,
    options: &SearchOptions,
    read_connection: &DbConnection,
    audit_connection: Option<&DbConnection>,
    rerank_seed: Deterministic<Seed>,
) -> Result<SearchReport, SearchError> {
    // Search determinism controls ranking/output replay. Audit rows are durable
    // side effects, so they must remain unique across repeated seeded calls.
    let mut audit_ids = SearchAuditIdSource::Ambient;
    run_search_inner(
        cx,
        options,
        Some(read_connection),
        rerank_seed,
        &mut audit_ids,
        audit_connection,
        true,
        None,
    )
    .await
}

pub fn run_context_search_with_read_connection_seeded_and_audit_connection(
    options: &SearchOptions,
    read_connection: &DbConnection,
    audit_connection: Option<&DbConnection>,
    determinism: &Deterministic<Seed>,
) -> Result<SearchReport, SearchError> {
    let rerank_seed = determinism.shared_child("search.rerank");
    with_search_root(|cx| async move {
        run_context_search_with_preloaded_memories_with_cx(
            &cx,
            options,
            read_connection,
            audit_connection,
            rerank_seed,
            None,
        )
        .await
        .map(|run| run.report)
    })
}

pub fn run_context_search_with_preloaded_memories(
    options: &SearchOptions,
    read_connection: &DbConnection,
    audit_connection: Option<&DbConnection>,
    determinism: &Deterministic<Seed>,
    fast_embedder_override: Option<Arc<dyn crate::search::Embedder>>,
) -> Result<ContextSearchReport, SearchError> {
    let rerank_seed = determinism.shared_child("search.rerank");
    with_search_root(|cx| async move {
        run_context_search_with_preloaded_memories_with_cx(
            &cx,
            options,
            read_connection,
            audit_connection,
            rerank_seed,
            fast_embedder_override,
        )
        .await
    })
}

pub async fn run_context_search_with_preloaded_memories_with_cx(
    cx: &asupersync::Cx,
    options: &SearchOptions,
    read_connection: &DbConnection,
    audit_connection: Option<&DbConnection>,
    rerank_seed: Deterministic<Seed>,
    fast_embedder_override: Option<Arc<dyn crate::search::Embedder>>,
) -> Result<ContextSearchReport, SearchError> {
    run_context_search_with_preloaded_memories_and_workspace_state_with_cx(
        cx,
        options,
        read_connection,
        audit_connection,
        None,
        rerank_seed,
        fast_embedder_override,
    )
    .await
}

pub fn run_context_search_with_preloaded_memories_and_workspace_state(
    options: &SearchOptions,
    read_connection: &DbConnection,
    audit_connection: Option<&DbConnection>,
    workspace_state: Option<&SearchWorkspaceProbeState>,
    determinism: &Deterministic<Seed>,
) -> Result<ContextSearchReport, SearchError> {
    let rerank_seed = determinism.shared_child("search.rerank");
    with_search_root(|cx| async move {
        run_context_search_with_preloaded_memories_and_workspace_state_with_cx(
            &cx,
            options,
            read_connection,
            audit_connection,
            workspace_state,
            rerank_seed,
            None,
        )
        .await
    })
}

pub async fn run_context_search_with_preloaded_memories_and_workspace_state_with_cx(
    cx: &asupersync::Cx,
    options: &SearchOptions,
    read_connection: &DbConnection,
    audit_connection: Option<&DbConnection>,
    workspace_state: Option<&SearchWorkspaceProbeState>,
    rerank_seed: Deterministic<Seed>,
    fast_embedder_override: Option<Arc<dyn crate::search::Embedder>>,
) -> Result<ContextSearchReport, SearchError> {
    // Context candidate conversion batch-loads memories itself, so passthrough
    // swarm/workspace scopes do not need search-analysis metadata on every hit.
    let mut audit_ids = SearchAuditIdSource::Ambient;
    let mut preloaded_memories = BTreeMap::new();
    let run = run_search_inner_with_performance(
        cx,
        options,
        Some(read_connection),
        rerank_seed,
        &mut audit_ids,
        audit_connection,
        workspace_state,
        false,
        Some(&mut preloaded_memories),
        fast_embedder_override,
    )
    .await?;
    Ok(ContextSearchReport {
        report: run.report,
        preloaded_memories,
        performance: run.performance,
    })
}

/// Resolve the `ee similar` seed memory through the same workspace,
/// `--memory-scope` trust lane, and validity gating search applies to every
/// other candidate (bd-2vq2z.25; same bypass class as bd-2vq2z.4). A raw
/// `get_memory(id)` lets a seed from another workspace, an out-of-scope trust
/// lane, or a tombstoned / expired / not-yet-valid row silently drive the
/// query. Mirrors the search candidate-admission path
/// ([`MemoryScopeContext::memory_in_scope_with_tags`] + tombstone/validity
/// windows) and returns `MemoryNotFound` when the seed is not admissible under
/// the requested workspace and scope. The seed body is never emitted (only its
/// id), so this closes the lookup-side bypass without leaking redacted bodies.
fn resolve_similar_seed_memory(
    connection: &DbConnection,
    options: &SimilarOptions,
    workspace_id: &str,
    scope_context: &MemoryScopeContext,
) -> Result<StoredMemory, SimilarError> {
    let memory =
        connection
            .get_memory(&options.memory_id)?
            .ok_or_else(|| SimilarError::MemoryNotFound {
                memory_id: options.memory_id.clone(),
            })?;
    let tags = connection.get_memory_tags(&options.memory_id)?;

    // Workspace + scope trust lane: the seed must belong to the requested
    // workspace (or be globally scoped) and pass the active `--memory-scope`,
    // exactly like every other search candidate.
    let workspace_candidate = memory.workspace_id == workspace_id
        || crate::models::memory_tags_include_global_scope(&tags);
    let admissible = workspace_candidate && scope_context.memory_in_scope_with_tags(&memory, &tags);

    // Validity: honor tombstone / expiry / future windows the same way the
    // search candidate path does, so an excluded seed cannot drive retrieval.
    let validity_ok = if memory.tombstoned_at.is_some() && !options.include_tombstoned {
        false
    } else {
        let reference_time = options.as_of.unwrap_or_else(Utc::now);
        matches!(
            memory_validity_visibility(
                memory.valid_from.as_deref(),
                memory.valid_to.as_deref(),
                reference_time,
                options.include_expired,
                options.include_future,
            ),
            MemoryValidityVisibility::Visible
        )
    };

    if admissible && validity_ok {
        Ok(memory)
    } else {
        Err(SimilarError::MemoryNotFound {
            memory_id: options.memory_id.clone(),
        })
    }
}

pub fn run_similar(options: &SimilarOptions) -> Result<SimilarReport, SimilarError> {
    crate::core::run_cli_with_cx(Duration::from_secs(30), |cx| async move {
        run_similar_with_cx(&cx, options).await
    })
    .map_err(|error| {
        SimilarError::Search(SearchError::Index(format!(
            "Failed to start similar-search runtime: {error}"
        )))
    })?
}

pub async fn run_similar_with_cx(
    cx: &asupersync::Cx,
    options: &SimilarOptions,
) -> Result<SimilarReport, SimilarError> {
    // Production always resolves the live workspace posture. The optional
    // posture below is a narrow diagnostic seam for hermetic hash-only tests;
    // it must never change the neural-by-default production path.
    run_similar_with_cx_and_posture(cx, options, None).await
}

async fn run_similar_with_cx_and_posture(
    cx: &asupersync::Cx,
    options: &SimilarOptions,
    embedding_posture_override: Option<EmbeddingPosture>,
) -> Result<SimilarReport, SimilarError> {
    search_checkpoint(cx)?;
    let database_path = options.resolve_database_path();
    let connection = DbConnection::open_file_read_only(&database_path)?;
    // Resolve the requested workspace and memory scope BEFORE the seed lookup so
    // the seed is admitted through the same workspace / `--memory-scope` /
    // validity gating search uses for every other candidate (bd-2vq2z.25).
    // Workspaces are keyed by the raw `workspace_path` everywhere they are
    // written (init/remember) and where search builds its scope context, so the
    // lookup uses the same raw path rather than a canonical root.
    let requested_workspace_id = crate::core::curate::stable_workspace_id(&options.workspace_path);
    let workspace_id = crate::core::workspace::select_existing_workspace_row(
        &connection,
        &requested_workspace_id,
        &[&options.workspace_path],
    )
    .map_err(|error| DbError::MalformedRow {
        operation: DbOperation::Query,
        message: error.message(),
    })?
    .map_or(requested_workspace_id, |workspace| workspace.id);
    let scope_context = MemoryScopeContext::for_workspace(
        &options.workspace_path,
        options.memory_scope,
        options.strict_scope,
    );
    let target = resolve_similar_seed_memory(&connection, options, &workspace_id, &scope_context)?;
    let index_dir = options.resolve_index_dir();
    let posture_is_overridden = embedding_posture_override.is_some();
    let mut embedding_posture = match embedding_posture_override {
        Some(posture) => posture,
        None => current_embedding_posture(&connection, &target.workspace_id, &index_dir)?,
    };
    let initial_semantic_request_capable = similar_semantic_request_capable(&embedding_posture);
    let lexical_fallback = !initial_semantic_request_capable;
    let requested_limit = options.limit;
    let retrieval_limit = requested_limit.saturating_add(1).max(1);
    let source_mode = if initial_semantic_request_capable {
        SearchSourceMode::SemanticOnly
    } else {
        SearchSourceMode::LexicalOnly
    };
    let search_options = SearchOptions {
        workspace_path: options.workspace_path.clone(),
        database_path: Some(database_path.clone()),
        index_dir: Some(index_dir.clone()),
        query: target.content.clone(),
        limit: retrieval_limit,
        speed: options.speed,
        explain: options.explain,
        as_of: options.as_of,
        include_tombstoned: options.include_tombstoned,
        include_expired: options.include_expired,
        include_future: options.include_future,
        include_stale: options.include_stale,
        relevance_floor: options.min_score,
        dedup_mode: SearchDedupMode::DocId,
        source_mode,
        strict_source_mode: false,
        memory_scope: options.memory_scope,
        strict_scope: options.strict_scope,
    };
    let determinism = Deterministic::from_seed(0);
    let mut audit_ids = SearchAuditIdSource::Ambient;
    let mut report = run_search_inner(
        cx,
        &search_options,
        Some(&connection),
        determinism.shared_child("search.rerank"),
        &mut audit_ids,
        Some(&connection),
        true,
        None,
    )
    .await?;
    if !posture_is_overridden
        && initial_semantic_request_capable
        && !embedding_posture.semantic
        && let Ok(updated) =
            current_embedding_posture(&connection, &target.workspace_id, &index_dir)
    {
        embedding_posture = updated;
    }
    let semantic_available = embedding_posture.semantic;
    report.query = target.id.clone();
    report.requested_limit = requested_limit;
    remove_similar_target_and_truncate(&mut report, &target.id, requested_limit);
    sanitize_similar_target_query_degradations(&mut report, &target.id);
    if lexical_fallback || (!semantic_available && !embedding_posture.semantic_pending()) {
        let reason = similar_semantic_unavailable_reason(&embedding_posture);
        report
            .degraded
            .push(SearchDegradation::embed_model_unavailable(&reason));
        report.source_mode_requested = SearchSourceMode::SemanticOnly;
        if lexical_fallback {
            report.source_mode_applied = SearchSourceMode::LexicalOnly;
            report.source_mode_fallback = true;
        }
    }
    if let Err(error) = connection.close() {
        tracing::warn!(
            target: "ee::search::similar",
            event = "similar_connection_close_failed",
            database_path = %database_path.display(),
            error = %error,
        );
    }

    Ok(SimilarReport {
        target_memory_id: target.id,
        target_level: target.level,
        target_kind: target.kind,
        semantic_available,
        lexical_fallback,
        embedding_posture,
        report,
    })
}

fn remove_similar_target_and_truncate(
    report: &mut SearchReport,
    target_memory_id: &str,
    limit: u32,
) {
    report
        .results
        .retain(|hit| hit.memory_id() != Some(target_memory_id));
    let limit = usize::try_from(limit).unwrap_or(usize::MAX);
    if report.results.len() > limit {
        report.results.truncate(limit);
    }
    if report.results.is_empty() && report.status == SearchStatus::Success {
        report.status = SearchStatus::NoResults;
    }
}

fn sanitize_similar_target_query_degradations(report: &mut SearchReport, target_memory_id: &str) {
    let floor = report
        .relevance_floor_applied
        .unwrap_or(DEFAULT_RELEVANCE_FLOOR);
    for degradation in &mut report.degraded {
        if degradation.code == "no_relevant_results" {
            degradation.message = format!(
                "No memories scored above similarity floor {floor:.4} for target memory `{target_memory_id}`."
            );
            degradation.repair = Some(
                "Lower --min-score or rebuild the index after adding related memories.".to_string(),
            );
        }
    }
}

fn similar_semantic_request_capable(posture: &EmbeddingPosture) -> bool {
    posture.semantic || posture.semantic_pending()
}

fn similarity_mode_for_posture(posture: &EmbeddingPosture) -> &'static str {
    if posture.semantic {
        "semantic_knn"
    } else if posture.semantic_pending() {
        "semantic_pending"
    } else {
        "lexical_fallback"
    }
}

fn similar_semantic_unavailable_reason(posture: &EmbeddingPosture) -> String {
    format!(
        "embedding posture mode={} source={} semantic=false",
        posture.mode, posture.source
    )
}

#[derive(Clone)]
struct SearchRerankRuntime {
    reranker: Option<Arc<dyn Reranker>>,
    text_provider: Option<SearchRerankTextProvider>,
    model_id: Option<String>,
    top_k: usize,
}

const SEARCH_RERANK_MIN_CANDIDATES: usize = 5;

async fn apply_search_rerank(
    cx: &asupersync::Cx,
    query: &str,
    candidates: &mut [crate::search::ScoredResult],
    reranker: &dyn Reranker,
    text_fn: impl Fn(&str) -> Option<String> + Send + Sync,
    top_k: usize,
) -> frankensearch::SearchResult<()> {
    // Frankensearch currently nests TwoTierSearcher's Phase 3 inside a
    // successful quality-embedding Phase 2. EE deliberately ships Model2Vec
    // without the forbidden-dependency FastEmbed quality tier, so invoke the
    // library's public rerank pipeline over Phase 1 candidates directly. This
    // preserves Frankensearch's score/explanation semantics without inventing
    // a fake quality embedder or changing hash-fallback retrieval behavior.
    frankensearch::rerank_step(
        cx,
        reranker,
        query,
        candidates,
        text_fn,
        top_k,
        SEARCH_RERANK_MIN_CANDIDATES,
    )
    .await
}

impl SearchRerankRuntime {
    fn disabled() -> Self {
        Self {
            reranker: None,
            text_provider: None,
            model_id: None,
            top_k: DEFAULT_SEARCH_RERANK_TOP_K,
        }
    }

    fn enabled(
        reranker: Arc<dyn Reranker>,
        text_provider: SearchRerankTextProvider,
        model_id: String,
        top_k: usize,
    ) -> Self {
        Self {
            reranker: Some(reranker),
            text_provider: Some(text_provider),
            model_id: Some(model_id),
            top_k,
        }
    }

    fn is_enabled(&self) -> bool {
        self.reranker.is_some() && self.text_provider.is_some()
    }

    fn collect_limit(&self, requested_limit: usize) -> usize {
        if self.is_enabled() {
            requested_limit.max(self.top_k)
        } else {
            requested_limit
        }
    }
}

#[derive(Clone)]
struct SearchRerankTextProvider {
    database_path: PathBuf,
    scope_context: MemoryScopeContext,
    cache: Arc<Mutex<HashMap<String, Option<String>>>>,
}

impl SearchRerankTextProvider {
    fn new(database_path: PathBuf, workspace_path: &Path, options: &SearchOptions) -> Self {
        Self {
            database_path,
            scope_context: MemoryScopeContext::for_workspace(
                workspace_path,
                options.memory_scope,
                options.strict_scope,
            ),
            cache: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn text_for_doc(&self, doc_id: &str) -> Option<String> {
        if !doc_id.starts_with("mem_") {
            return None;
        }
        if let Ok(cache) = self.cache.lock()
            && let Some(cached) = cache.get(doc_id)
        {
            return cached.clone();
        }

        let resolved = self.load_scoped_memory_text(doc_id);
        if let Ok(mut cache) = self.cache.lock() {
            cache.insert(doc_id.to_string(), resolved.clone());
        }
        resolved
    }

    fn load_scoped_memory_text(&self, doc_id: &str) -> Option<String> {
        let connection = DbConnection::open_file_read_only(&self.database_path).ok()?;
        let memory = connection.get_memory(doc_id).ok().flatten()?;
        let tags = if matches!(self.scope_context.scope, MemoryScope::Global) {
            connection.get_memory_tags(doc_id).unwrap_or_default()
        } else {
            Vec::new()
        };
        if self.scope_context.memory_in_scope_with_tags(&memory, &tags) {
            Some(memory.content)
        } else {
            None
        }
    }
}

fn resolve_search_rerank_runtime(
    options: &SearchOptions,
    explicit_lexical_only: bool,
    connection: Option<&DbConnection>,
    configured_mode: crate::config::SearchRerankMode,
    configured_top_k: usize,
    degraded: &mut Vec<SearchDegradation>,
) -> SearchRerankRuntime {
    // Only an explicitly requested lexical-only mode skips the registry.
    // A hybrid request that FELL BACK to lexical (semantic embedder
    // unavailable) must still consult it so a present-but-unusable
    // reranker surfaces its permanent capability advisory instead of
    // silently vanishing whenever retrieval degraded (bd-degraded-
    // advisory-noise).
    if explicit_lexical_only {
        return SearchRerankRuntime::disabled();
    }

    if configured_mode == crate::config::SearchRerankMode::Off {
        // `[search] rerank = "off"` (bd-2vq2z.23): the operator opted out of the
        // local cross-encoder, so do not look up or load a reranker model and do
        // not emit a `rerank_model_unavailable` degradation.
        return SearchRerankRuntime::disabled();
    }

    let database_path = options.resolve_database_path();
    let workspace_root = default_workspace_root(&options.workspace_path);
    let requested = crate::core::curate::stable_workspace_id(&workspace_root);
    let resolution_result = match connection {
        Some(connection) => {
            let workspace_id = crate::core::workspace::bound_workspace_id_or_hash(
                connection,
                &requested,
                &[&options.workspace_path, workspace_root.as_path()],
            )
            .unwrap_or(requested);
            resolve_registered_reranker(connection, &workspace_id)
        }
        None => {
            if !database_path.exists() {
                Err(format!(
                    "model registry database {} does not exist",
                    database_path.display()
                ))
            } else {
                DbConnection::open_file_read_only(&database_path)
                    .map_err(|error| error.to_string())
                    .and_then(|connection| {
                        let workspace_id = crate::core::workspace::bound_workspace_id_or_hash(
                            &connection,
                            &requested,
                            &[&options.workspace_path, workspace_root.as_path()],
                        )
                        .unwrap_or(requested);
                        resolve_registered_reranker(&connection, &workspace_id)
                    })
            }
        }
    };
    let resolved = match resolution_result {
        Ok(RegisteredRerankerResolution::Loaded(resolved)) => resolved,
        Ok(RegisteredRerankerResolution::Absent | RegisteredRerankerResolution::HashRejected) => {
            // Absent: nothing registered. HashRejected: a reranker row is
            // present but its stored hash cannot match this build's bundled
            // manifest — deterministic, not retryable, so it shares the
            // permanent canonical advisory lifecycle.
            degraded.push(SearchDegradation::rerank_model_absent());
            return SearchRerankRuntime::disabled();
        }
        Ok(RegisteredRerankerResolution::Unloadable) => {
            tracing::warn!(
                target: "ee::search::rerank",
                event = "rerank_model_load_failed",
            );
            degraded.push(SearchDegradation::rerank_model_unavailable(
                "The registered local reranker could not be loaded; retry the query or inspect local model status.",
            ));
            return SearchRerankRuntime::disabled();
        }
        Err(_error) => {
            tracing::warn!(
                target: "ee::search::rerank",
                event = "rerank_registry_inspection_failed",
            );
            degraded.push(SearchDegradation::rerank_model_unavailable(
                "The local reranker registry could not be inspected; retry the query or inspect local model status.",
            ));
            return SearchRerankRuntime::disabled();
        }
    };

    tracing::info!(
        target: "ee::search::rerank",
        event = "rerank_model_resolved",
        model_id = %resolved.entry.model_name,
        top_k = configured_top_k,
    );
    SearchRerankRuntime::enabled(
        resolved.reranker,
        SearchRerankTextProvider::new(database_path, &options.workspace_path, options),
        resolved.entry.model_name,
        configured_top_k,
    )
}

#[derive(Clone, Debug)]
pub(crate) struct SearchRerankRuntimePosture {
    pub(crate) configured_mode: crate::config::SearchRerankMode,
    pub(crate) configured_top_k: usize,
    pub(crate) runtime_available: bool,
    pub(crate) degraded: Vec<SearchDegradation>,
}

/// Re-resolve only the reranker state whose truth can change independently of
/// a cached pack. L2 callers use this after a confirmed hit so the cached
/// selection remains reusable while delivery-time advisory posture stays
/// current.
pub(crate) fn resolve_search_rerank_runtime_posture(
    options: &SearchOptions,
    explicit_lexical_only: bool,
    connection: Option<&DbConnection>,
) -> SearchRerankRuntimePosture {
    let (configured_mode, configured_top_k) = resolve_search_rerank_config(&options.workspace_path);
    let mut degraded = Vec::new();
    let runtime = resolve_search_rerank_runtime(
        options,
        explicit_lexical_only,
        connection,
        configured_mode,
        configured_top_k,
        &mut degraded,
    );
    SearchRerankRuntimePosture {
        configured_mode,
        configured_top_k,
        runtime_available: runtime.is_enabled(),
        degraded,
    }
}

pub(crate) enum RegisteredRerankerResolution {
    Absent,
    Loaded(ResolvedRegisteredReranker),
    Unloadable,
    /// A reranker row is registered as available, but its stored hash cannot
    /// match this build's bundled manifest. Deterministic and not retryable,
    /// so it drives the permanent advisory lifecycle.
    HashRejected,
}

pub(crate) struct ResolvedRegisteredReranker {
    entry: StoredModelRegistryEntry,
    reranker: Arc<dyn Reranker>,
}

pub(crate) fn resolve_registered_reranker(
    connection: &DbConnection,
    workspace_id: &str,
) -> Result<RegisteredRerankerResolution, String> {
    let entries = connection
        .list_model_registry_entries(workspace_id)
        .map_err(|error| error.to_string())?;
    let entries = sorted_available_reranker_entries(entries);
    if entries.is_empty() {
        return Ok(RegisteredRerankerResolution::Absent);
    }
    // `Err(hash_rejected)` classifies an all-failed pass: a rejected stored
    // hash is deterministic and not retryable, while a plain load failure is
    // the retryable unavailable posture.
    match first_loadable_registered_reranker(&entries, |entry| {
        verify_reranker_registry_hash(entry).ok()?;
        load_verified_search_reranker(entry).ok()
    }) {
        Ok((entry, reranker)) => Ok(RegisteredRerankerResolution::Loaded(
            ResolvedRegisteredReranker { entry, reranker },
        )),
        Err(hash_rejected) => Ok(if hash_rejected {
            RegisteredRerankerResolution::HashRejected
        } else {
            RegisteredRerankerResolution::Unloadable
        }),
    }
}

/// Deterministic skip-broken-first pass over sorted available reranker rows.
/// The loader receives every surviving row in order; the first `Some` wins.
/// `Err(true)` means at least one row failed its bundled-manifest hash gate
/// before the pass exhausted, which callers surface as HashRejected.
fn first_loadable_registered_reranker<T>(
    entries: &[StoredModelRegistryEntry],
    mut load: impl FnMut(&StoredModelRegistryEntry) -> Option<T>,
) -> Result<(StoredModelRegistryEntry, T), bool> {
    let mut hash_rejected = false;
    for entry in entries {
        if verify_reranker_registry_hash(entry).is_err() {
            // Keep scanning: a later row may still load, but a rejected hash
            // is remembered so an all-failed pass classifies deterministically.
            hash_rejected = true;
            continue;
        }
        if let Some(loaded) = load(entry) {
            return Ok((entry.clone(), loaded));
        }
    }
    Err(hash_rejected)
}

fn sorted_available_reranker_entries(
    mut entries: Vec<StoredModelRegistryEntry>,
) -> Vec<StoredModelRegistryEntry> {
    entries.retain(|entry| {
        entry.purpose == ModelPurpose::Reranker && entry.status == ModelRegistryStatus::Available
    });
    entries.sort_by(|left, right| {
        left.model_name
            .cmp(&right.model_name)
            .then_with(|| left.id.cmp(&right.id))
    });
    entries
}

fn load_verified_search_reranker(
    entry: &StoredModelRegistryEntry,
) -> Result<Arc<dyn Reranker>, String> {
    let source_path = reranker_entry_source_path(entry)?;
    let model_dir = unpacked_rerank_model_dir(&source_path)?;
    let reranker = NativeReranker::load(&model_dir).map_err(|error| {
        format!(
            "Failed to load rerank model {} from {}: {error}",
            entry.model_name,
            model_dir.display()
        )
    })?;
    Ok(Arc::new(frankensearch::SyncRerankerAdapter(reranker)))
}

fn verify_reranker_registry_hash(entry: &StoredModelRegistryEntry) -> Result<(), String> {
    let manifest = crate::core::model::bundled_rerank_model_manifest()
        .map_err(|error| format!("Could not read bundled rerank model manifest: {error}"))?;
    let expected = format!("blake3:{}", manifest.hash_blake3);
    match entry.content_hash.as_deref() {
        Some(hash) if hash == expected => Ok(()),
        Some(hash) => Err(format!(
            "Registered reranker hash {hash} does not match bundled manifest hash {expected}."
        )),
        None => Err("Registered reranker is missing a content hash.".to_string()),
    }
}

fn reranker_entry_source_path(entry: &StoredModelRegistryEntry) -> Result<PathBuf, String> {
    if let Some(path) = entry
        .metadata_json
        .as_deref()
        .and_then(reranker_stored_path_from_metadata)
    {
        return Ok(path);
    }
    let source = entry
        .source_uri
        .as_deref()
        .ok_or_else(|| "Registered reranker has no source path.".to_string())?;
    if source.contains("://") {
        return Err(format!(
            "Registered reranker source `{source}` is remote; fetch/cache it locally first."
        ));
    }
    Ok(PathBuf::from(source))
}

fn reranker_stored_path_from_metadata(raw: &str) -> Option<PathBuf> {
    let value: serde_json::Value = serde_json::from_str(raw).ok()?;
    value
        .get("storedPath")
        .and_then(serde_json::Value::as_str)
        .filter(|path| !path.trim().is_empty())
        .map(PathBuf::from)
}

fn unpacked_rerank_model_dir(source_path: &Path) -> Result<PathBuf, String> {
    if source_path.is_dir() {
        return ensure_rerank_model_dir_ready(source_path);
    }
    if !source_path.exists() {
        return Err(format!(
            "Registered reranker artifact {} does not exist.",
            source_path.display()
        ));
    }
    if let Some(unpacked) = rerank_archive_sibling_dir(source_path)
        && unpacked.is_dir()
    {
        return ensure_rerank_model_dir_ready(&unpacked);
    }
    Err(format!(
        "Registered reranker artifact {} is cached but not unpacked into a loadable model directory.",
        source_path.display()
    ))
}

fn rerank_archive_sibling_dir(source_path: &Path) -> Option<PathBuf> {
    let file_name = source_path.file_name()?.to_str()?;
    let stem = file_name.strip_suffix(".tar.zst")?;
    Some(source_path.parent()?.join(stem))
}

fn ensure_rerank_model_dir_ready(model_dir: &Path) -> Result<PathBuf, String> {
    let has_model = model_dir.join(RERANK_MODEL_SAFETENSORS_PRIMARY).is_file()
        || model_dir.join(RERANK_MODEL_SAFETENSORS_FALLBACK).is_file();
    let has_tokenizer = model_dir.join(RERANK_MODEL_TOKENIZER).is_file();
    if has_model && has_tokenizer {
        Ok(model_dir.to_path_buf())
    } else {
        Err(format!(
            "Rerank model directory {} is missing tokenizer.json or safetensors model files.",
            model_dir.display()
        ))
    }
}

fn truncate_hits_to_limit(hits: &mut Vec<SearchHit>, limit: u32) {
    let limit = usize::try_from(limit).unwrap_or(usize::MAX);
    if hits.len() > limit {
        hits.truncate(limit);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RelevanceFloorCounts {
    considered: usize,
    passed: usize,
}

impl RelevanceFloorCounts {
    fn new(considered: usize, passed: usize) -> Self {
        Self { considered, passed }
    }

    fn has_no_relevant_results(self) -> bool {
        self.considered > 0 && self.passed == 0
    }

    fn is_low_recall(self) -> bool {
        self.considered >= 3 && (self.passed * 10) < (self.considered * 3)
    }

    fn append_degradations(
        self,
        degraded: &mut Vec<SearchDegradation>,
        floor: f32,
        top_score_before_floor: Option<f32>,
    ) {
        if self.has_no_relevant_results() {
            degraded.push(SearchDegradation::no_relevant_results(
                floor,
                self.considered,
                top_score_before_floor,
            ));
        }
        if self.is_low_recall() {
            degraded.push(SearchDegradation::low_recall_after_floor(
                floor,
                self.passed,
                self.considered,
            ));
        }
    }
}

async fn run_search_inner(
    cx: &asupersync::Cx,
    options: &SearchOptions,
    read_connection: Option<&DbConnection>,
    rerank_seed: Deterministic<Seed>,
    audit_ids: &mut SearchAuditIdSource,
    audit_connection: Option<&DbConnection>,
    include_passthrough_scope_analysis_metadata: bool,
    preloaded_memories: Option<&mut BTreeMap<String, StoredMemory>>,
) -> Result<SearchReport, SearchError> {
    run_search_inner_with_performance(
        cx,
        options,
        read_connection,
        rerank_seed,
        audit_ids,
        audit_connection,
        None,
        include_passthrough_scope_analysis_metadata,
        preloaded_memories,
        None,
    )
    .await
    .map(|run| run.report)
}

async fn run_search_inner_with_performance(
    cx: &asupersync::Cx,
    options: &SearchOptions,
    read_connection: Option<&DbConnection>,
    rerank_seed: Deterministic<Seed>,
    audit_ids: &mut SearchAuditIdSource,
    audit_connection: Option<&DbConnection>,
    workspace_state: Option<&SearchWorkspaceProbeState>,
    include_passthrough_scope_analysis_metadata: bool,
    mut preloaded_memories: Option<&mut BTreeMap<String, StoredMemory>>,
    fast_embedder_override: Option<Arc<dyn crate::search::Embedder>>,
) -> Result<SearchPerformanceRun, SearchError> {
    search_checkpoint(cx)?;
    let start = Instant::now();
    let mut trace = SearchPerformanceTrace::default();
    let setup_start = Instant::now();
    let index_dir = options.resolve_index_dir();
    trace.record_elapsed("search::setup", setup_start);
    // Runtime-profile resolution gets its own stable phase so a regression
    // that reintroduces blocking host probes on this shared pre-query path
    // is attributable from `--explain-performance` output alone
    // (bd-search-warm-latency-0bh05).
    let runtime_profile_start = Instant::now();
    let runtime_profile = workspace_state
        .map(|state| state.runtime_profile.clone())
        .unwrap_or_else(|| runtime_profile_for_workspace(&options.workspace_path));
    trace.record_elapsed("search::runtimeProfile", runtime_profile_start);
    let (effective_limit, limit_capped) = runtime_profile.cap_search_limit(options.limit);

    let index_exists_start = Instant::now();
    if !index_dir.exists() {
        trace.record_elapsed("search::indexExists", index_exists_start);
        return Err(SearchError::NoIndex);
    }
    if !crate::core::index::index_corpus_compatibility_is_current(&index_dir) {
        trace.record_elapsed("search::indexExists", index_exists_start);
        return Err(SearchError::NoIndex);
    }
    trace.record_elapsed("search::indexExists", index_exists_start);

    let degradation_start = Instant::now();
    let output_redaction_enabled = workspace_state.map_or_else(
        || crate::config::workspace_output_redaction_enabled(&options.workspace_path),
        |state| state.output_redaction_enabled,
    );
    let (mut degraded, index_freshness) =
        search_degradations_with_connection(options, &index_dir, read_connection);
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
    trace.record_elapsed("search::degradationSetup", degradation_start);

    let source_mode_start = Instant::now();
    let source_mode = resolve_source_mode(
        options,
        &index_dir,
        &mut degraded,
        fast_embedder_override.as_deref(),
    )?;
    let embed_backend = if source_mode.applied.uses_embeddings() {
        fast_embedder_override.as_ref().map_or_else(
            crate::core::index::active_embed_backend,
            |embedder| {
                if embedder.is_semantic() {
                    EmbedBackend::NeuralLocal
                } else {
                    EmbedBackend::HashFallback
                }
            },
        )
    } else {
        // `embed_backend` describes this response's execution, not semantic
        // capability left warm by an earlier request in the same process.
        // Lexical-only retrieval executes no neural backend.
        EmbedBackend::HashFallback
    };
    push_model_lifecycle_search_degradation(options, read_connection, &mut degraded);
    trace.record_elapsed("search::sourceModeResolve", source_mode_start);
    let rerank_resolve_start = Instant::now();
    let (rerank_configured_mode, rerank_configured_top_k) =
        resolve_search_rerank_config(&options.workspace_path);
    let rerank_runtime = resolve_search_rerank_runtime(
        options,
        // Applied lexical-only WITHOUT a fallback means the operator asked
        // for lexical; a fallback means hybrid was requested and degraded.
        source_mode.applied == SearchSourceMode::LexicalOnly && !source_mode.fallback_applied,
        read_connection,
        rerank_configured_mode,
        rerank_configured_top_k,
        &mut degraded,
    );
    let rerank_runtime_available = rerank_runtime.is_enabled();
    trace.record_elapsed("search::rerankResolve", rerank_resolve_start);
    let fusion_weights = resolved_search_fusion_weights(&options.workspace_path);
    if source_mode.unavailable_no_results {
        search_checkpoint(cx)?;
        let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;
        trace.record_elapsed("search::total", start);
        return Ok(SearchPerformanceRun {
            report: SearchReport {
                index_freshness,
                status: SearchStatus::NoResults,
                embed_backend,
                query: options.query.clone(),
                requested_limit: options.limit,
                results: Vec::new(),
                elapsed_ms,
                errors: Vec::new(),
                degraded,
                runtime_profile,
                rerank_configured_mode,
                rerank_configured_top_k,
                rerank_runtime_available,
                relevance_floor_applied: None,
                candidates_below_floor: 0,
                query_assist: None,
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
            },
            performance: trace,
        });
    }
    let retrieve_start = Instant::now();
    let search_result = search_sync_with_performance(
        cx,
        &index_dir,
        &options.query,
        effective_limit as usize,
        options.two_tier_config_for_limit(effective_limit),
        options.explain,
        source_mode.applied,
        rerank_seed,
        rerank_runtime,
        fusion_weights,
        fast_embedder_override,
        &mut trace,
    )
    .await;
    trace.record_elapsed("search::retrieve", retrieve_start);
    search_checkpoint(cx)?;

    let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;

    match search_result {
        Ok((mut raw_hits, errors)) => {
            // Bead bd-17c65.2.3 (B3): dedupe on docId BEFORE the floor
            // filter so the floor metrics reflect the deduped pool.
            // After fusion, the same docId can appear multiple times
            // (different arms promoting it, MMR rescoring tied
            // candidates, or a global-memory lane overlapping the local
            // result set). Keep the highest-relevance occurrence and discard
            // the rest. Stable ordering is preserved (first occurrence's
            // position wins among ties).
            let global_hits = global_store_lexical_hits(
                options,
                effective_limit,
                &mut degraded,
                preloaded_memories.as_deref_mut(),
            );
            if !global_hits.is_empty() {
                raw_hits.extend(global_hits);
                sort_search_hits_by_score_order(&mut raw_hits);
            }
            let dedupe_doc_id_start = Instant::now();
            let (raw_hits, duplicates_collapsed) = dedupe_hits_on_doc_id(raw_hits);
            let evidence_visibility_start = Instant::now();
            let raw_hits =
                apply_live_evidence_visibility(options, raw_hits, &mut degraded, read_connection);
            trace.record_elapsed(
                "search::evidenceLiveAdmissionVisibility",
                evidence_visibility_start,
            );
            trace.record_elapsed("search::dedupeDocId", dedupe_doc_id_start);
            let dedupe_mi_start = Instant::now();
            let (raw_hits, mi_duplicates_collapsed, mi_eligible_count) =
                if options.dedup_mode == SearchDedupMode::MutualInformation {
                    dedupe_hits_on_mutual_information(raw_hits, options, read_connection)
                } else {
                    (raw_hits, 0, 0)
                };
            trace.record_elapsed("search::dedupeMutualInformation", dedupe_mi_start);

            // Bead bd-17c65.2.1 (B1): apply the floor to the comparable
            // `relevanceScore` projection, never to source-dependent raw
            // BM25, cosine, or RRF magnitudes.
            let user_floor_override = options.relevance_floor;
            let pre_floor_count = raw_hits.len();
            let pre_floor_top_score = raw_hits.first().map(SearchHit::relevance_score);

            // Partition into above-floor (kept) and below-floor (dropped).
            // Floor of 0.0 is "disabled" — keep everything. Non-finite
            // relevance projections are always dropped.
            let relevance_floor_start = Instant::now();
            let (above_floor, below_floor): (Vec<_>, Vec<_>) = raw_hits
                .into_iter()
                .partition(|hit| search_hit_meets_relevance_floor(hit, user_floor_override));
            let floor_counts = RelevanceFloorCounts::new(pre_floor_count, above_floor.len());
            let dropped = below_floor.len();
            let query_assist_visibility_start = Instant::now();
            let query_assist_candidates = query_assist_visible_candidates(options, &below_floor);
            trace.record_elapsed(
                "search::queryAssistVisibility",
                query_assist_visibility_start,
            );
            trace.record_elapsed("search::relevanceFloor", relevance_floor_start);
            let tombstone_start = Instant::now();
            let above_floor = apply_tombstone_visibility_collecting(
                options,
                above_floor,
                &mut degraded,
                read_connection,
                preloaded_memories.as_deref_mut(),
            );
            trace.record_elapsed("search::tombstoneVisibility", tombstone_start);
            let scope_start = Instant::now();
            let (above_floor, scope_stats) =
                apply_memory_scope_visibility_with_metadata_mode_collecting(
                    options,
                    above_floor,
                    &mut degraded,
                    read_connection,
                    include_passthrough_scope_analysis_metadata,
                    preloaded_memories.as_deref_mut(),
                );
            trace.record_elapsed("search::scopeVisibility", scope_start);
            let mesh_start = Instant::now();
            let mut above_floor = apply_mesh_query_visibility(above_floor, &mut degraded);
            trace.record_elapsed("search::meshVisibility", mesh_start);
            let calibration_start = Instant::now();
            annotate_hits_with_score_calibration(
                &options.workspace_path,
                options.database_path.as_deref(),
                read_connection,
                &mut above_floor,
                &mut degraded,
            );
            trace.record_elapsed("search::scoreCalibration", calibration_start);
            let truncate_start = Instant::now();
            truncate_hits_to_limit(&mut above_floor, effective_limit);
            trace.record_elapsed("search::truncate", truncate_start);
            let preload_start = Instant::now();
            preload_returned_search_memories(
                &above_floor,
                read_connection,
                preloaded_memories.as_deref_mut(),
                &mut degraded,
            );
            trace.record_elapsed("search::preloadReturnedMemories", preload_start);
            let kept = above_floor.len();

            let floor = user_floor_override.unwrap_or(DEFAULT_RELEVANCE_FLOOR);

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
            if let Some(top) = above_floor.first().map(SearchHit::relevance_score) {
                if top.is_finite() && top >= floor && top < floor * 2.0 {
                    degraded.push(SearchDegradation::weak_query_recall(floor, top));
                }
            }

            // Emit floor degradations from the immutable partition snapshot.
            // Empty workspaces stay plain `NoResults`; significant floor loss
            // is <30% passed with at least three candidates. Later visibility
            // and result-limit drops have their own diagnostics.
            floor_counts.append_degradations(&mut degraded, floor, pre_floor_top_score);

            let status = if above_floor.is_empty() {
                SearchStatus::NoResults
            } else {
                SearchStatus::Success
            };
            let query_assist = build_query_assist(
                &options.query,
                options.explain,
                kept,
                floor_counts,
                floor,
                above_floor.first().map(SearchHit::relevance_score),
                dropped,
                &query_assist_candidates,
            );
            if let Some(assist) = query_assist.as_ref() {
                tracing::info!(
                    target: "ee::search",
                    event = "query_assist_generated",
                    schema = QUERY_ASSIST_SCHEMA_V1,
                    mode = assist.mode.as_str(),
                    weak_result_reason = assist.weak_result_reason.as_str(),
                    candidate_count = assist.candidate_count,
                    did_you_mean_count = assist.did_you_mean.len(),
                    reformulation_count = assist.reformulations.len(),
                    dropped_below_floor = assist.dropped_below_floor,
                    query_hash = %audit_query_hash(&options.query),
                );
            }

            search_checkpoint(cx)?;

            // Search is a canonical no-write surface. Audit rows are only
            // emitted when an enclosing mutating operation supplies its
            // already-owned write connection (for example, persisted pack
            // assembly). A missing audit connection means "do not audit";
            // it must never trigger an implicit read-write reopen.
            if let Some(audit_connection) = audit_connection {
                let audit_workspace_start = Instant::now();
                // Bind to the stored workspace row so path-keyed ids join
                // the same audit workspace as remember/init writes.
                // Canonicalize first so /tmp -> /private/tmp on macOS.
                let canonical_workspace = default_workspace_root(&options.workspace_path);
                let requested = crate::core::curate::stable_workspace_id(&canonical_workspace);
                let workspace_id = crate::core::workspace::bound_workspace_id_or_hash(
                    audit_connection,
                    &requested,
                    &[&options.workspace_path, canonical_workspace.as_path()],
                )
                .unwrap_or(requested);
                let audit_workspace_persisted =
                    search_audit_workspace_persisted(Some(audit_connection), &workspace_id);
                trace.record_elapsed("search::auditWorkspaceCheck", audit_workspace_start);
                if audit_workspace_persisted {
                    let audit_payload_start = Instant::now();
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
                    // optional miss row + 1 returned_mem per hit + redaction
                    // overhead).
                    let mut audit_batch =
                        SearchAuditBatch::new(2 + above_floor.len().saturating_mul(2));
                    audit_batch.push(
                        audit_ids,
                        Some(&workspace_id),
                        audit_actions::SEARCH_EXECUTED,
                        Some("workspace"),
                        Some(&workspace_id),
                        Some(executed_details),
                    );
                    if let Some(miss_reason) = classify_search_query_miss(
                        floor_counts,
                        floor,
                        above_floor.first().map(SearchHit::relevance_score),
                    ) {
                        let miss_details =
                            search_query_miss_audit_details(SearchQueryMissAuditDetails {
                                query_hash: &q_hash,
                                reason: miss_reason,
                                status,
                                kept,
                                considered: floor_counts.considered,
                                dropped_below_floor: dropped,
                                floor,
                                top_score_before_floor: pre_floor_top_score,
                                top_score_after_floor: above_floor
                                    .first()
                                    .map(SearchHit::relevance_score),
                            });
                        audit_batch.push(
                            audit_ids,
                            Some(&workspace_id),
                            audit_actions::SEARCH_MISS_RECORDED,
                            Some("query_hash"),
                            Some(&q_hash),
                            Some(miss_details),
                        );
                    }
                    for (rank, hit) in above_floor.iter().enumerate() {
                        let returned_details = serde_json::json!({
                            "queryHash": &q_hash,
                            "rank": (rank + 1) as u32,
                            "score": hit.score,
                            "relevanceScore": hit.relevance_score(),
                            "scoreKind": hit.score_kind(),
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
                    trace.record_elapsed("search::auditPayloadBuild", audit_payload_start);
                    let audit_flush_start = Instant::now();
                    audit_batch.flush_best_effort_with_connection(audit_connection);
                    trace.record_elapsed("search::auditFlush", audit_flush_start);
                }
            }

            search_checkpoint(cx)?;
            trace.record_elapsed("search::total", start);
            Ok(SearchPerformanceRun {
                report: SearchReport {
                    status,
                    embed_backend,
                    query: options.query.clone(),
                    requested_limit: options.limit,
                    results: above_floor,
                    elapsed_ms,
                    errors,
                    degraded,
                    runtime_profile,
                    rerank_configured_mode,
                    rerank_configured_top_k,
                    rerank_runtime_available,
                    relevance_floor_applied: Some(floor),
                    candidates_below_floor: dropped,
                    query_assist,
                    source_mode_requested: options.source_mode,
                    source_mode_applied: source_mode.applied,
                    source_mode_fallback: source_mode.fallback_applied,
                    strict_source_mode: options.strict_source_mode,
                    memory_scope: options.memory_scope,
                    strict_scope: options.strict_scope,
                    scope_stats,
                    index_freshness,
                },
                performance: trace,
            })
        }
        Err(SearchError::Cancelled(reason)) => Err(SearchError::Cancelled(reason)),
        Err(error) => {
            let e = error.to_string();
            let mut degraded = degraded;
            let index_error_already_explained = degraded.iter().any(|degradation| {
                matches!(degradation.code.as_str(), "index_corrupt" | "index_missing")
            });
            if !index_error_already_explained {
                degraded.push(SearchDegradation::corrupt_index(Some(&e)));
            }

            trace.record_elapsed("search::total", start);
            Ok(SearchPerformanceRun {
                report: SearchReport {
                    status: SearchStatus::IndexError,
                    embed_backend,
                    query: options.query.clone(),
                    requested_limit: options.limit,
                    results: Vec::new(),
                    elapsed_ms,
                    errors: vec![e],
                    degraded,
                    runtime_profile,
                    rerank_configured_mode,
                    rerank_configured_top_k,
                    rerank_runtime_available,
                    relevance_floor_applied: None,
                    candidates_below_floor: 0,
                    query_assist: None,
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
                    index_freshness,
                },
                performance: trace,
            })
        }
    }
}

pub fn run_diag_search(options: &SearchOptions) -> Result<SearchDiagnosticReport, SearchError> {
    crate::core::run_cli_with_cx(Duration::from_secs(30), |cx| async move {
        run_diag_search_with_cx(&cx, options).await
    })
    .map_err(|error| {
        SearchError::Index(format!(
            "Failed to start diagnostic-search runtime: {error}"
        ))
    })?
}

pub async fn run_diag_search_with_cx(
    cx: &asupersync::Cx,
    options: &SearchOptions,
) -> Result<SearchDiagnosticReport, SearchError> {
    let index_dir = options.resolve_index_dir();
    let fast_embedder = if options.source_mode.uses_embeddings()
        && index_dir.exists()
        && crate::core::index::index_corpus_compatibility_is_current(&index_dir)
    {
        Some(
            prepare_search_embedder_for_workspace(
                cx,
                &options.workspace_path,
                &options.resolve_database_path(),
            )
            .await
            .map_err(|error| map_frankensearch_error(cx, "diagnostic embedder preparation", error))?
            .fast_embedder,
        )
    } else {
        None
    };
    run_diag_search_with_cx_and_workspace_embedder(cx, options, fast_embedder).await
}

#[cfg(test)]
async fn run_diag_search_with_cx_and_embedder(
    cx: &asupersync::Cx,
    options: &SearchOptions,
    fast_embedder_override: Option<Arc<dyn crate::search::Embedder>>,
) -> Result<SearchDiagnosticReport, SearchError> {
    run_diag_search_with_cx_and_embedder_policy(cx, options, fast_embedder_override, false).await
}

async fn run_diag_search_with_cx_and_workspace_embedder(
    cx: &asupersync::Cx,
    options: &SearchOptions,
    fast_embedder_override: Option<Arc<dyn crate::search::Embedder>>,
) -> Result<SearchDiagnosticReport, SearchError> {
    run_diag_search_with_cx_and_embedder_policy(cx, options, fast_embedder_override, true).await
}

async fn run_diag_search_with_cx_and_embedder_policy(
    cx: &asupersync::Cx,
    options: &SearchOptions,
    fast_embedder_override: Option<Arc<dyn crate::search::Embedder>>,
    resolve_runtime_source_mode: bool,
) -> Result<SearchDiagnosticReport, SearchError> {
    search_checkpoint(cx)?;
    let start = Instant::now();
    let index_dir = options.resolve_index_dir();
    let runtime_profile = runtime_profile_for_workspace(&options.workspace_path);
    let (effective_limit, limit_capped) = runtime_profile.cap_search_limit(options.limit);

    if !index_dir.exists() {
        return Err(SearchError::NoIndex);
    }
    if !crate::core::index::index_corpus_compatibility_is_current(&index_dir) {
        return Err(SearchError::NoIndex);
    }

    let (mut degraded, index_freshness) = search_degradations(options, &index_dir);
    let source_mode = if resolve_runtime_source_mode {
        resolve_source_mode(
            options,
            &index_dir,
            &mut degraded,
            fast_embedder_override.as_deref(),
        )?
    } else {
        // Unit tests that provide the exact embedder used to build a synthetic
        // diagnostic index exercise arm/fusion math, not production model
        // discovery. The public path above always enables runtime resolution.
        SourceModeResolution {
            applied: options.source_mode,
            fallback_applied: false,
            unavailable_no_results: false,
        }
    };
    let embed_backend = if source_mode.applied.uses_embeddings() {
        fast_embedder_override.as_ref().map_or_else(
            crate::core::index::active_embed_backend,
            |embedder| {
                if embedder.is_semantic() {
                    EmbedBackend::NeuralLocal
                } else {
                    EmbedBackend::HashFallback
                }
            },
        )
    } else {
        EmbedBackend::HashFallback
    };
    push_model_lifecycle_search_degradation(options, None, &mut degraded);
    if limit_capped {
        degraded.push(SearchDegradation::profile_search_limit_capped(
            options.limit,
            effective_limit,
            runtime_profile.active_profile.as_str(),
        ));
    }

    let config = options.two_tier_config_for_limit(effective_limit);
    let fusion_weights = resolved_search_fusion_weights(&options.workspace_path);
    let mut diag_result = diag_search_sync(
        cx,
        &index_dir,
        &options.query,
        effective_limit as usize,
        config,
        options.explain,
        source_mode.applied,
        fusion_weights,
        fast_embedder_override,
    )
    .await?;
    apply_live_evidence_visibility_to_diag(options, &mut diag_result, &mut degraded);

    let (raw_hits, duplicates_collapsed) = dedupe_hits_on_doc_id(diag_result.final_hits);
    let (raw_hits, mi_duplicates_collapsed, mi_eligible_count) =
        if options.dedup_mode == SearchDedupMode::MutualInformation {
            dedupe_hits_on_mutual_information(raw_hits, options, None)
        } else {
            (raw_hits, 0, 0)
        };
    // Mirror `run_search`'s relevance projection and shared floor so
    // `ee diag search` cannot silently disagree with the live path.
    let user_floor_override = options.relevance_floor;
    let pre_floor_count = raw_hits.len();
    let pre_floor_top_score = raw_hits.first().map(SearchHit::relevance_score);
    let (above_floor, below_floor): (Vec<_>, Vec<_>) = raw_hits
        .into_iter()
        .partition(|hit| search_hit_meets_relevance_floor(hit, user_floor_override));
    let floor_counts = RelevanceFloorCounts::new(pre_floor_count, above_floor.len());
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
    let query_assist_candidates = query_assist_visible_candidates(options, &below_floor);
    let floor = user_floor_override.unwrap_or(DEFAULT_RELEVANCE_FLOOR);

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
    if let Some(top) = above_floor.first().map(SearchHit::relevance_score) {
        if top.is_finite() && top >= floor && top < floor * 2.0 {
            degraded.push(SearchDegradation::weak_query_recall(floor, top));
        }
    }
    // Keep diagnostic search aligned with the live path by deriving floor
    // degradations from the same pre-visibility partition snapshot.
    floor_counts.append_degradations(&mut degraded, floor, pre_floor_top_score);

    let status = if above_floor.is_empty() {
        SearchStatus::NoResults
    } else {
        SearchStatus::Success
    };
    let query_assist = build_query_assist(
        &options.query,
        options.explain,
        kept,
        floor_counts,
        floor,
        above_floor.first().map(SearchHit::relevance_score),
        dropped,
        &query_assist_candidates,
    );
    let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;
    let (rerank_configured_mode, rerank_configured_top_k) =
        resolve_search_rerank_config(&options.workspace_path);
    let final_report = SearchReport {
        status,
        embed_backend,
        query: options.query.clone(),
        requested_limit: options.limit,
        results: above_floor,
        elapsed_ms: diag_result.final_elapsed_ms,
        errors: diag_result.errors.clone(),
        degraded,
        runtime_profile,
        rerank_configured_mode,
        rerank_configured_top_k,
        rerank_runtime_available: false,
        relevance_floor_applied: Some(floor),
        candidates_below_floor: dropped,
        query_assist,
        source_mode_requested: options.source_mode,
        source_mode_applied: source_mode.applied,
        source_mode_fallback: source_mode.fallback_applied,
        strict_source_mode: options.strict_source_mode,
        memory_scope: options.memory_scope,
        strict_scope: options.strict_scope,
        scope_stats,
        index_freshness,
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

fn search_degradations(
    options: &SearchOptions,
    index_dir: &Path,
) -> (Vec<SearchDegradation>, Option<SearchIndexFreshness>) {
    search_degradations_with_connection(options, index_dir, None)
}

/// Connection-reusing variant of [`search_degradations`].
///
/// Threads the caller's open read connection into the index-status probe so
/// the staleness/health check does not open a redundant file database
/// connection. Behavior is identical to [`search_degradations`]; see
/// [`cached_index_status_for_search`].
fn search_degradations_with_connection(
    options: &SearchOptions,
    index_dir: &Path,
    connection: Option<&DbConnection>,
) -> (Vec<SearchDegradation>, Option<SearchIndexFreshness>) {
    let Ok(index_status) = cached_index_status_for_search(options, index_dir, connection) else {
        return (vec![SearchDegradation::index_status_probe_failed()], None);
    };

    match index_status.health {
        IndexHealth::Ready => (Vec::new(), None),
        IndexHealth::Stale => {
            let mut degraded = vec![SearchDegradation::stale_index(
                index_status.db_generation,
                index_status.index_generation,
            )];
            let generation_gap = match (index_status.db_generation, index_status.index_generation) {
                (Some(db_generation), Some(index_generation)) => {
                    Some(db_generation.saturating_sub(index_generation))
                }
                _ => None,
            };
            let large_gap = matches!(
                (index_status.db_generation, index_status.index_generation),
                (Some(db_generation), Some(index_generation))
                    if search_index_gap_is_large(db_generation, index_generation)
            );
            if large_gap
                && let (Some(db_generation), Some(index_generation)) =
                    (index_status.db_generation, index_status.index_generation)
            {
                degraded.push(SearchDegradation::large_index_gap(
                    db_generation,
                    index_generation,
                ));
            }
            let freshness = SearchIndexFreshness {
                stale: true,
                db_generation: index_status.db_generation,
                index_generation: index_status.index_generation,
                generation_gap,
                large_gap,
            };
            (degraded, Some(freshness))
        }
        IndexHealth::Missing => (vec![SearchDegradation::missing_index()], None),
        IndexHealth::Corrupt => (
            vec![SearchDegradation::corrupt_index(
                index_status.last_check_error.as_deref(),
            )],
            None,
        ),
    }
}

fn push_model_lifecycle_search_degradation(
    options: &SearchOptions,
    connection: Option<&DbConnection>,
    degraded: &mut Vec<SearchDegradation>,
) {
    if options.source_mode == SearchSourceMode::LexicalOnly {
        return;
    }
    let Ok(report) = crate::core::model::build_model_lifecycle_report_for_workspace(
        &options.workspace_path,
        options.database_path.as_deref(),
        connection,
    ) else {
        return;
    };
    let Some(degradation) = report.semantic_surface_degradation("search") else {
        return;
    };
    if matches!(
        degradation.code,
        "index_stale" | "index_missing" | "index_corrupt" | "search_index_degraded"
    ) {
        return;
    }
    // Dedup by code only: resolve_source_mode may already have pushed the same
    // code with a different message (different call path, same root cause).
    if degraded
        .iter()
        .any(|existing| existing.code == degradation.code)
    {
        return;
    }
    degraded.push(SearchDegradation::from_model_lifecycle(&degradation));
}

fn pin_lexical_ram_tier_for_search(
    workspace_path: &Path,
    index_dir: &Path,
) -> LexicalRamTierResult {
    let started = Instant::now();
    let config = lexical_ram_tier_config_for_search(workspace_path);
    let result = pin_lexical_index_files(&index_dir.join("lexical"), &config);
    trace_lexical_ram_tier(
        &crate::core::workspace::bound_workspace_id_from_path(workspace_path),
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
    let fingerprint = lexical_ram_tier_search_config_fingerprint(workspace_path);
    let cache = LEXICAL_RAM_TIER_SEARCH_CONFIG_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(cache_guard) = cache.lock()
        && let Some(cached) = cache_guard.get(workspace_path)
        && cached.fingerprint == fingerprint
    {
        return cached.config;
    }

    let config = resolve_lexical_ram_tier_config_for_search(workspace_path);
    if let Ok(mut cache_guard) = cache.lock() {
        cache_guard.insert(
            workspace_path.to_path_buf(),
            CachedLexicalRamTierSearchConfig {
                fingerprint,
                config,
            },
        );
    }
    config
}

fn resolve_lexical_ram_tier_config_for_search(workspace_path: &Path) -> LexicalRamTierConfig {
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

/// Resolve the effective `[search] rerank` mode and candidate-pool
/// `rerank_top_k` for `workspace_path`, defaulting to auto /
/// [`DEFAULT_SEARCH_RERANK_TOP_K`] when config is unreadable or unset
/// (bd-2vq2z.23). `rerank = "off"` suppresses the reranker entirely; a
/// positive `rerank_top_k` overrides the candidate-pool collect limit.
fn resolve_search_rerank_config(workspace_path: &Path) -> (crate::config::SearchRerankMode, usize) {
    if let Ok(merged) = crate::core::config_surface::merged_workspace_config(workspace_path) {
        let mode = merged
            .values
            .search
            .rerank
            .unwrap_or(crate::config::SearchRerankMode::Auto);
        let top_k = merged
            .values
            .search
            .rerank_top_k
            .and_then(|value| usize::try_from(value).ok())
            .filter(|&candidate| candidate > 0)
            .unwrap_or(DEFAULT_SEARCH_RERANK_TOP_K);
        return (mode, top_k);
    }
    (
        crate::config::SearchRerankMode::Auto,
        DEFAULT_SEARCH_RERANK_TOP_K,
    )
}

fn lexical_ram_tier_search_config_fingerprint(
    workspace_path: &Path,
) -> LexicalRamTierSearchConfigFingerprint {
    let config_path = workspace_path.join(".ee").join("config.toml");
    match std::fs::symlink_metadata(&config_path) {
        Ok(metadata) => LexicalRamTierSearchConfigFingerprint {
            exists: true,
            len: metadata.len(),
            modified: metadata.modified().ok(),
            content_hash: std::fs::read(&config_path)
                .ok()
                .map(|bytes| *blake3::hash(&bytes).as_bytes()),
        },
        Err(_) => LexicalRamTierSearchConfigFingerprint {
            exists: false,
            len: 0,
            modified: None,
            content_hash: None,
        },
    }
}

fn push_lexical_ram_tier_search_degradations(
    degraded: &mut Vec<SearchDegradation>,
    report: &LexicalRamTierResult,
) {
    if !report.enabled {
        return;
    }
    for code in &report.degraded_codes {
        if let Some(degradation) = lexical_ram_tier_search_degradation_for_code(code) {
            degraded.push(degradation);
        }
    }
}

fn lexical_ram_tier_search_degradation_for_code(code: &str) -> Option<SearchDegradation> {
    match code {
        LEXICAL_HUGEPAGES_UNAVAILABLE_CODE => {
            Some(SearchDegradation::lexical_hugepages_unavailable())
        }
        LEXICAL_RAM_TIER_HEAP_WARMLOAD_CODE => {
            Some(SearchDegradation::lexical_ram_tier_heap_warmload())
        }
        LEXICAL_RAM_UNAVAILABLE_ON_MACOS_CODE => {
            Some(SearchDegradation::lexical_ram_unavailable_on_macos())
        }
        _ => None,
    }
}

fn resolve_source_mode(
    options: &SearchOptions,
    index_dir: &Path,
    degraded: &mut Vec<SearchDegradation>,
    fast_embedder_override: Option<&dyn crate::search::Embedder>,
) -> Result<SourceModeResolution, SearchError> {
    let lexical_available = lexical_search_available(index_dir);
    if !options.source_mode.uses_embeddings() {
        return resolve_source_mode_with_tiers(
            options,
            degraded,
            SearchTierState {
                lexical_available,
                embed_model_unavailable: None,
                semantic_embedder_pending: None,
                semantic_embedder_degraded: None,
            },
        );
    }

    let prepared_semantic = fast_embedder_override.is_some_and(|embedder| embedder.is_semantic());
    let prepared_hash_fallback =
        fast_embedder_override.is_some_and(|embedder| !embedder.is_semantic());
    let embed_model_unavailable = (!prepared_hash_fallback)
        .then(embed_model_unavailable_reason_from_env)
        .flatten();
    let semantic_unavailable = if prepared_hash_fallback {
        Some(HASH_FALLBACK_SEMANTIC_UNAVAILABLE_REASON.to_owned())
    } else {
        (!prepared_semantic && embed_model_unavailable.is_none())
            .then(semantic_retrieval_unavailable_reason)
            .flatten()
    };
    let semantic_pending =
        (!prepared_semantic && !prepared_hash_fallback && embed_model_unavailable.is_none())
            .then(semantic_retrieval_pending_reason)
            .flatten();
    let tiers = SearchTierState {
        lexical_available,
        embed_model_unavailable: embed_model_unavailable.as_deref(),
        semantic_embedder_pending: semantic_pending.as_deref(),
        semantic_embedder_degraded: semantic_unavailable.as_deref(),
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

    if let Some(reason) = tiers.semantic_embedder_pending
        && matches!(
            requested,
            SearchSourceMode::Hybrid | SearchSourceMode::SemanticOnly
        )
    {
        tracing::info!(
            target: "ee::search::embedder_pending",
            model_id = EMBED_MODEL_UNAVAILABLE_MODEL_ID,
            feature_flag = EMBED_MODEL_UNAVAILABLE_FEATURE_FLAG,
            lexical_available,
            reason,
            "semantic embedder is pending first-use download; keeping semantic path enabled"
        );
    }

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

    if let Some(reason) = tiers.semantic_embedder_degraded
        && matches!(
            requested,
            SearchSourceMode::Hybrid | SearchSourceMode::SemanticOnly
        )
    {
        if options.strict_source_mode {
            return Err(SearchError::SourceModeUnavailable {
                requested,
                reason: format!("semantic similarity unavailable: {reason}"),
            });
        }
        degraded.push(SearchDegradation::embed_model_unavailable(reason));
        tracing::warn!(
            target: "ee::search::embedder_down",
            code = "embed_model_unavailable",
            model_id = EMBED_MODEL_UNAVAILABLE_MODEL_ID,
            feature_flag = EMBED_MODEL_UNAVAILABLE_FEATURE_FLAG,
            lexical_available,
            reason,
            "active embedder is deterministic hash fallback; semantic similarity unavailable"
        );
        if lexical_available {
            return Ok(SourceModeResolution {
                applied: SearchSourceMode::LexicalOnly,
                fallback_applied: true,
                unavailable_no_results: false,
            });
        }
        return Ok(SourceModeResolution {
            applied: SearchSourceMode::LexicalOnly,
            fallback_applied: true,
            unavailable_no_results: true,
        });
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ActiveSearchEmbedderState {
    ReadySemantic,
    PendingLocalDownload,
    HashFallback,
}

fn active_search_embedder_state() -> ActiveSearchEmbedderState {
    let stack = crate::core::index::default_search_embedder_stack();
    let fast = stack.fast();
    if fast.is_semantic() {
        ActiveSearchEmbedderState::ReadySemantic
    } else if embedder_reports_pending_model2vec_download(fast) {
        ActiveSearchEmbedderState::PendingLocalDownload
    } else {
        ActiveSearchEmbedderState::HashFallback
    }
}

pub(crate) fn active_search_embedder_is_semantic() -> bool {
    active_search_embedder_state() == ActiveSearchEmbedderState::ReadySemantic
}

/// One-line, agent-actionable hint for turning on semantic retrieval.
pub(crate) const SEMANTIC_ENABLE_HINT: &str = "run `ee index reembed --workspace .`; default builds use Frankensearch model2vec download unless FRANKENSEARCH_OFFLINE blocks it";

/// Posture probe for onboarding/diagnostic surfaces (e.g. `ee init`).
/// Returns `None` when semantic retrieval is active or pending first-use
/// download, or `Some(reason)` when retrieval is degraded to lexical-only. The
/// same degraded condition surfaces per-query as `embed_model_unavailable`;
/// this lets the one-time onboarding path nudge the agent before search.
/// (agent-UX item 6)
pub(crate) fn semantic_retrieval_unavailable_reason() -> Option<String> {
    if active_search_embedder_is_semantic() {
        return None;
    }

    match active_search_embedder_state() {
        ActiveSearchEmbedderState::ReadySemantic
        | ActiveSearchEmbedderState::PendingLocalDownload => None,
        ActiveSearchEmbedderState::HashFallback => Some(
            embed_model_unavailable_reason_from_env()
                .unwrap_or_else(|| HASH_FALLBACK_SEMANTIC_UNAVAILABLE_REASON.to_string()),
        ),
    }
}

fn semantic_retrieval_pending_reason() -> Option<String> {
    (active_search_embedder_state() == ActiveSearchEmbedderState::PendingLocalDownload).then(
        || {
            "ee-managed bundled model2vec download is pending; the first embedding operation will download and load the local model".to_string()
        },
    )
}

fn embed_model_unavailable_reason_from_env() -> Option<String> {
    embed_model_unavailable_reason_from_path_value(read(EnvVar::EmbedModelPath).as_deref())
}

fn embed_model_unavailable_reason_from_path_value(raw: Option<&str>) -> Option<String> {
    let trimmed = raw?.trim();
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

/// Index-status probe for the search hot path, with process-local TTL caching.
///
/// When `connection` is `Some`, the underlying [`get_index_status_with_connection`]
/// probe reuses the caller's already-open read connection for its `COUNT(*)`
/// generation/stat reads instead of opening a fresh file database connection.
/// On this host a fresh `DbConnection::open_file` is a fixed ~250-300ms cost,
/// and the search hot path already holds an open read connection, so reusing
/// it removes a redundant open from the `search::degradationSetup` span. The
/// cached report and every returned field are identical regardless of
/// `connection` — only the connection used for the DB-stats read differs.
fn cached_index_status_for_search(
    options: &SearchOptions,
    index_dir: &Path,
    connection: Option<&DbConnection>,
) -> Result<IndexStatusReport, IndexStatusError> {
    let status_options = IndexStatusOptions {
        workspace_path: options.workspace_path.clone(),
        database_path: options.database_path.clone(),
        index_dir: Some(index_dir.to_path_buf()),
    };

    // A caller-held snapshot is authoritative for this response. Never let a
    // process-local Ready observation mask a writer that committed after the
    // cache entry was populated, and never let this request repopulate the
    // cache with an observation older than a concurrent writer's invalidation.
    if let Some(connection) = connection {
        return get_index_status_with_connection(&status_options, Some(connection));
    }

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

    let index_status = get_index_status_with_connection(&status_options, None)?;

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

/// Reconcile a small, known generation gap before opening a read snapshot.
///
/// The status read intentionally bypasses the one-second hot-path cache so a
/// newly queued writer cannot inherit an earlier `Ready` observation. A failed
/// reconciliation is non-fatal: the subsequent canonical status probe emits
/// `search_index_stale` and preserves the explicit rebuild repair.
pub(crate) async fn reconcile_search_index_before_read_with_cx(
    cx: &asupersync::Cx,
    options: &SearchOptions,
) {
    let child_scope =
        cx.scope_with_budget(cx.budget_for_timeout(SEARCH_INDEX_AUTO_RECONCILE_TIMEOUT));
    let owned_options = options.clone();
    let mut task = match cx.spawn_in(&child_scope, move |child_cx| async move {
        reconcile_search_index_within_budget(&child_cx, &owned_options).await;
    }) {
        Ok(task) => task,
        Err(error) => {
            tracing::warn!(
                target: "ee::search::index_freshness",
                error = %error,
                "could not start bounded pre-retrieval index repair; continuing with truthful stale-index reporting"
            );
            return;
        }
    };
    if let Err(error) = task.join(cx).await {
        tracing::warn!(
            target: "ee::search::index_freshness",
            error = %error,
            "bounded pre-retrieval index reconciliation ended before convergence; continuing with truthful stale-index reporting"
        );
    }
}

async fn reconcile_search_index_within_budget(cx: &asupersync::Cx, options: &SearchOptions) {
    if cx.checkpoint().is_err() {
        return;
    }
    let index_dir = options.resolve_index_dir();
    invalidate_cached_index_status_for_search(options, &index_dir);
    let status_options = IndexStatusOptions {
        workspace_path: options.workspace_path.clone(),
        database_path: options.database_path.clone(),
        index_dir: Some(index_dir.clone()),
    };
    let database_path = options.resolve_database_path();
    // This preflight must not inherit the process pool's five-second acquire
    // wait. A dedicated one-slot pool opens immediately, and the pinned read
    // transaction makes corpus counts and generation one atomic observation.
    let read_pool = ReadConnectionPool::new(
        DatabaseConfig::file(database_path),
        PoolConfig::default_single()
            .with_acquire_timeout(Duration::ZERO)
            .with_max_pin_duration(SEARCH_INDEX_AUTO_RECONCILE_TIMEOUT),
    );
    let Ok(read_snapshot) = read_pool.acquire_snapshot(true) else {
        return;
    };
    let Ok(connection) = read_snapshot.checked_connection() else {
        return;
    };
    let Ok(status) = get_index_status_with_connection(&status_options, Some(connection)) else {
        return;
    };
    let Some((db_generation, index_generation)) = status.db_generation.zip(status.index_generation)
    else {
        return;
    };
    let generation_gap = db_generation.saturating_sub(index_generation);
    if status.health != IndexHealth::Stale
        || !search_index_gap_is_auto_reconcilable(db_generation, index_generation)
    {
        return;
    }
    let corpus_documents = u64::from(status.db_memory_count)
        .saturating_add(u64::from(status.db_session_count))
        .saturating_add(u64::from(status.db_artifact_count))
        .saturating_add(u64::from(status.db_rule_count))
        .saturating_add(u64::from(status.db_evidence_admitted_count));
    if !search_index_corpus_is_auto_reconcilable(corpus_documents) {
        tracing::info!(
            target: "ee::search::index_freshness",
            generation_gap,
            corpus_documents,
            corpus_document_limit = SEARCH_INDEX_AUTO_RECONCILE_MAX_DOCUMENTS,
            "skipped synchronous search-index repair because the complete corpus exceeds the interactive read bound"
        );
        return;
    }
    drop(read_snapshot);
    drop(read_pool);

    if cx.checkpoint().is_err() {
        return;
    }

    let processing_options = IndexProcessingOptions {
        workspace_path: options.workspace_path.clone(),
        database_path: options.database_path.clone(),
        index_dir: Some(index_dir.clone()),
        dry_run: false,
        job_limit: Some(
            u32::try_from(generation_gap)
                .unwrap_or(u32::MAX)
                .min(u32::try_from(SEARCH_INDEX_LARGE_GAP_THRESHOLD).unwrap_or(u32::MAX)),
        ),
    };
    match process_index_jobs_coalesced_with_cx_bounded(
        cx,
        &processing_options,
        u32::try_from(SEARCH_INDEX_AUTO_RECONCILE_MAX_DOCUMENTS).unwrap_or(u32::MAX),
    )
    .await
    {
        Ok(report)
            if matches!(
                report.status,
                IndexProcessingStatus::Success | IndexProcessingStatus::NoPendingJobs
            ) =>
        {
            tracing::info!(
                target: "ee::search::index_freshness",
                generation_gap,
                completed_jobs = report.completed_jobs,
                "reconciled queued search-index work before retrieval"
            );
        }
        Ok(report) => {
            tracing::warn!(
                target: "ee::search::index_freshness",
                generation_gap,
                status = report.status.as_str(),
                failed_jobs = report.failed_jobs,
                "pre-retrieval search-index reconciliation did not converge"
            );
        }
        Err(error) => {
            tracing::warn!(
                target: "ee::search::index_freshness",
                generation_gap,
                error = %error,
                "pre-retrieval search-index reconciliation failed; continuing with truthful stale-index reporting"
            );
        }
    }
}

fn invalidate_cached_index_status_for_search(options: &SearchOptions, index_dir: &Path) {
    let cache_key = IndexStatusCacheKey::from_search_options(options, index_dir);
    let cache = SEARCH_INDEX_STATUS_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(mut guard) = cache.lock() {
        guard.remove(&cache_key);
    }
}

struct DiagSearchSyncResult {
    pre_fusion: PreFusionDiagnostics,
    fusion: FusionDiagnostics,
    final_hits: Vec<SearchHit>,
    final_elapsed_ms: f64,
    errors: Vec<String>,
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
async fn diag_search_sync(
    cx: &asupersync::Cx,
    index_dir: &Path,
    query: &str,
    limit: usize,
    config: TwoTierConfig,
    explain: bool,
    source_mode: SearchSourceMode,
    fusion_weights: SearchFusionWeights,
    fast_embedder_override: Option<Arc<dyn crate::search::Embedder>>,
) -> Result<DiagSearchSyncResult, SearchError> {
    search_checkpoint(cx)?;
    let index_dir_owned = index_dir.to_path_buf();
    let query_owned = query.to_string();
    #[allow(clippy::type_complexity)]
    let result_holder: Arc<Mutex<Option<Result<DiagSearchSyncResult, SearchError>>>> =
        Arc::new(Mutex::new(None));
    let task_result = Arc::clone(&result_holder);
    let operation_cx = cx.clone();

    async move {
        let cx = operation_cx;
        if let Err(error) = search_checkpoint(&cx) {
            if let Ok(mut guard) = task_result.lock() {
                *guard = Some(Err(error));
            }
            return;
        }
        let index = match TwoTierIndex::open(&index_dir_owned, config.clone()) {
            Ok(idx) => Arc::new(idx),
            Err(error) => {
                if let Ok(mut guard) = task_result.lock() {
                    *guard = Some(Err(map_frankensearch_error(
                        &cx,
                        "Failed to open diagnostic index",
                        error,
                    )));
                }
                return;
            }
        };

        let candidate_limit = limit
            .max(1)
            .saturating_mul(config.candidate_multiplier.max(1));
        let fast_embedder =
            if source_mode == SearchSourceMode::LexicalOnly {
                None
            } else {
                Some(fast_embedder_override.unwrap_or_else(|| {
                    crate::core::index::default_search_embedder_stack().fast_arc()
                }))
            };
        let lexical = match open_lexical_searcher_for_diag(&index_dir_owned) {
            Ok(lexical) => lexical,
            Err(error) => {
                if let Ok(mut guard) = task_result.lock() {
                    *guard = Some(Err(SearchError::Index(error)));
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
                Err(error @ frankensearch::SearchError::Cancelled { .. }) => {
                    if let Ok(mut guard) = task_result.lock() {
                        *guard = Some(Err(map_frankensearch_error(
                            &cx,
                            "Diagnostic lexical arm cancelled",
                            error,
                        )));
                    }
                    return;
                }
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
        if let Err(error) = search_checkpoint(&cx) {
            if let Ok(mut guard) = task_result.lock() {
                *guard = Some(Err(error));
            }
            return;
        }

        let semantic_start = Instant::now();
        let semantic_result = match fast_embedder.as_ref() {
            Some(fast_embedder) => match fast_embedder.embed(&cx, &query_owned).await {
                Ok(query_vec) => match index.search_fast(&query_vec, candidate_limit) {
                    Ok(results) => SearchArmDiagnostics {
                        available: true,
                        score_scale: "cosine_similarity",
                        elapsed_ms: semantic_start.elapsed().as_secs_f64() * 1000.0,
                        results: vector_hits_to_arm_hits(&results),
                        error: None,
                    },
                    Err(error @ frankensearch::SearchError::Cancelled { .. }) => {
                        if let Ok(mut guard) = task_result.lock() {
                            *guard = Some(Err(map_frankensearch_error(
                                &cx,
                                "Diagnostic vector arm cancelled",
                                error,
                            )));
                        }
                        return;
                    }
                    Err(error) => SearchArmDiagnostics {
                        available: true,
                        score_scale: "cosine_similarity",
                        elapsed_ms: semantic_start.elapsed().as_secs_f64() * 1000.0,
                        results: Vec::new(),
                        error: Some(error.to_string()),
                    },
                },
                Err(error @ frankensearch::SearchError::Cancelled { .. }) => {
                    if let Ok(mut guard) = task_result.lock() {
                        *guard = Some(Err(map_frankensearch_error(
                            &cx,
                            "Diagnostic embedding arm cancelled",
                            error,
                        )));
                    }
                    return;
                }
                Err(error) => SearchArmDiagnostics {
                    available: true,
                    score_scale: "cosine_similarity",
                    elapsed_ms: semantic_start.elapsed().as_secs_f64() * 1000.0,
                    results: Vec::new(),
                    error: Some(error.to_string()),
                },
            },
            None => SearchArmDiagnostics {
                available: false,
                score_scale: "cosine_similarity",
                elapsed_ms: semantic_start.elapsed().as_secs_f64() * 1000.0,
                results: Vec::new(),
                error: Some(
                    "semantic arm not executed because source mode resolved to lexical_only"
                        .to_owned(),
                ),
            },
        };
        if let Err(error) = search_checkpoint(&cx) {
            if let Ok(mut guard) = task_result.lock() {
                *guard = Some(Err(error));
            }
            return;
        }

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
        let final_result: Result<Vec<SearchHit>, SearchError> = if source_mode
            == SearchSourceMode::LexicalOnly
        {
            match lexical.as_ref() {
                Some(lexical) => lexical
                    .search(&cx, &query_owned, limit)
                    .await
                    .map(|results| {
                        search_hits_from_scored_results(
                            results,
                            explain,
                            FrankensearchFinalScoreScale::Native,
                            fusion_weights,
                        )
                    })
                    .map_err(|error| {
                        map_frankensearch_error(&cx, "Diagnostic lexical search failed", error)
                    }),
                None => Err(SearchError::Index("Lexical index not found".to_owned())),
            }
        } else {
            match fast_embedder.as_ref() {
                Some(fast_embedder) => {
                    let searcher =
                        TwoTierSearcher::new(Arc::clone(&index), Arc::clone(fast_embedder), config);
                    let searcher = if source_mode == SearchSourceMode::Hybrid {
                        if let Some(lexical) = lexical {
                            searcher.with_lexical(lexical)
                        } else {
                            searcher
                        }
                    } else {
                        searcher
                    };
                    searcher
                        .search_collect(&cx, &query_owned, limit)
                        .await
                        .map(|(results, metrics)| {
                            let final_score_scale =
                                two_tier_final_score_scale(source_mode, &results, &metrics);
                            search_hits_from_scored_results(
                                results,
                                explain,
                                final_score_scale,
                                fusion_weights,
                            )
                        })
                        .map_err(|error| {
                            map_frankensearch_error(&cx, "Diagnostic search failed", error)
                        })
                }
                None => Err(SearchError::Index(
                    "Diagnostic semantic search requires a prepared embedder".to_owned(),
                )),
            }
        };
        if let Err(error) = search_checkpoint(&cx) {
            if let Ok(mut guard) = task_result.lock() {
                *guard = Some(Err(error));
            }
            return;
        }
        let converted = match final_result {
            Ok(mut hits) => {
                let rerank_seed = Deterministic::from_seed(0).shared_child("search.rerank");
                canonicalize_equivalent_component_scores(&mut hits, &rerank_seed);
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
            Err(error) => Err(error),
        };

        if let Ok(mut guard) = task_result.lock() {
            *guard = Some(converted);
        }
    }
    .await;

    result_holder
        .lock()
        .ok()
        .and_then(|mut guard| guard.take())
        .unwrap_or_else(|| {
            Err(SearchError::Index(
                "Diagnostic search result not captured".to_owned(),
            ))
        })
}

#[cfg(feature = "lexical-bm25")]
fn open_lexical_searcher_for_diag(
    index_dir: &Path,
) -> Result<Option<Arc<dyn LexicalRead>>, String> {
    open_lexical_searcher(index_dir)
}

#[cfg(not(feature = "lexical-bm25"))]
#[allow(
    clippy::unnecessary_wraps,
    reason = "signature mirrors the lexical-bm25 implementation"
)]
fn open_lexical_searcher_for_diag(
    _index_dir: &Path,
) -> Result<Option<Arc<dyn LexicalRead>>, String> {
    Ok(None)
}

fn scored_results_to_arm_hits(results: &[crate::search::ScoredResult]) -> Vec<SearchArmHit> {
    results
        .iter()
        .enumerate()
        .map(|(index, result)| SearchArmHit {
            doc_id: result.doc_id.to_string(),
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
            doc_id: result.doc_id.to_string(),
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
        crate::search::ScoreSource::HashControl => ScoreSource::HashControl,
    }
}

fn unit_weight_or(value: Option<f64>, default: f32) -> f32 {
    value
        .filter(|weight| weight.is_finite() && (0.0..=1.0).contains(weight))
        .map_or(default, |weight| weight as f32)
}

pub(crate) fn resolved_search_fusion_weights(workspace_path: &Path) -> SearchFusionWeights {
    crate::core::config_surface::merged_workspace_config(workspace_path)
        .map(|config| SearchFusionWeights::from_config(&config.values.search))
        .unwrap_or_default()
}

fn non_negative_component_score(score: Option<f32>) -> f32 {
    score
        .filter(|score| score.is_finite())
        .map_or(0.0, |score| score.max(0.0))
}

fn semantic_component_score(hit: &SearchHit) -> f32 {
    non_negative_component_score(hit.quality_score.or(hit.fast_score))
}

fn weighted_component_mix(
    weights: SearchFusionWeights,
    lexical_component: f32,
    semantic_component: f32,
    graph_component: f32,
) -> f32 {
    (weights.lexical * lexical_component)
        + (weights.semantic * semantic_component)
        + (weights.graph * graph_component)
}

fn weighted_available_component_mix(
    weights: SearchFusionWeights,
    lexical_component: f32,
    semantic_component: f32,
    graph_component: f32,
) -> f32 {
    if graph_component > 0.0 {
        return weighted_component_mix(
            weights,
            lexical_component,
            semantic_component,
            graph_component,
        );
    }

    let wired_weight_sum = weights.lexical + weights.semantic;
    if !wired_weight_sum.is_finite() || wired_weight_sum <= f32::EPSILON {
        return weighted_available_component_mix(
            SearchFusionWeights::default(),
            lexical_component,
            semantic_component,
            graph_component,
        );
    }
    let total_weight = weights.lexical + weights.semantic + weights.graph;
    let scale = if total_weight.is_finite() && total_weight > wired_weight_sum {
        total_weight / wired_weight_sum
    } else {
        1.0
    };
    (weights.lexical * scale * lexical_component) + (weights.semantic * scale * semantic_component)
}

pub(crate) fn configured_fusion_adjustment(
    hit: &SearchHit,
    score_is_rrf_fused: bool,
    weights: SearchFusionWeights,
) -> Option<SearchFusionAdjustment> {
    if !score_is_rrf_fused || hit.source == ScoreSource::Reranked || !hit.score.is_finite() {
        return None;
    }

    let lexical_component = non_negative_component_score(hit.lexical_score);
    let semantic_component = semantic_component_score(hit);
    let graph_component = 0.0;
    if lexical_component == 0.0 && semantic_component == 0.0 && graph_component == 0.0 {
        return None;
    }

    let default_weights = SearchFusionWeights::default();
    let default_mix = weighted_available_component_mix(
        default_weights,
        lexical_component,
        semantic_component,
        graph_component,
    );
    if !default_mix.is_finite() || default_mix <= f32::EPSILON {
        return None;
    }

    let configured_mix = weighted_available_component_mix(
        weights,
        lexical_component,
        semantic_component,
        graph_component,
    );
    if !configured_mix.is_finite() {
        return None;
    }
    let multiplier = (configured_mix / default_mix).max(0.0);
    if !multiplier.is_finite() {
        return None;
    }

    Some(SearchFusionAdjustment {
        multiplier,
        lexical_component,
        semantic_component,
        graph_component,
    })
}

fn append_configured_fusion_weight_factor(
    explanation: &mut ScoreExplanation,
    weights: SearchFusionWeights,
    adjustment: SearchFusionAdjustment,
) {
    let base = explanation.summary.trim_end_matches('.');
    explanation.summary = format!(
        "{base}. Configured fusion weights adjusted score by {:.4}x (lexical={:.2}, semantic={:.2}, graph={:.2}).",
        adjustment.multiplier, weights.lexical, weights.semantic, weights.graph
    );
    explanation.factors.push(ScoreFactor::new(
        "configured_fusion_weight",
        adjustment.multiplier,
        "workspace search.lexical_weight/search.semantic_weight/search.graph_weight multiplier",
        "search.lexical_weight/search.semantic_weight/search.graph_weight",
        "adjusted_score = raw_fusion_score * (configured_available_component_mix / default_available_component_mix)",
    ));
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FrankensearchFinalScoreScale {
    Native,
    RrfFused,
}

fn two_tier_final_score_scale(
    source_mode: SearchSourceMode,
    results: &[crate::search::ScoredResult],
    metrics: &frankensearch::TwoTierMetrics,
) -> FrankensearchFinalScoreScale {
    if source_mode != SearchSourceMode::Hybrid {
        return FrankensearchFinalScoreScale::Native;
    }

    // `ScoreSource` identifies contributing lanes, not the final score scale:
    // an RRF result with only one contributing lane is labelled Lexical or
    // SemanticFast upstream. Conversely, the hybrid orchestrator can return
    // native BM25 directly when it deliberately short-circuits or degrades.
    // Frankensearch records time only when the fusion call actually ran; the
    // explicit Hybrid source is a second semantic witness for synthetic and
    // zero-duration executions.
    if metrics.rrf_fusion_ms > 0.0
        || results
            .iter()
            .any(|result| result.source == crate::search::ScoreSource::Hybrid)
    {
        FrankensearchFinalScoreScale::RrfFused
    } else {
        FrankensearchFinalScoreScale::Native
    }
}

fn search_hit_from_scored_result(
    result: crate::search::ScoredResult,
    explain: bool,
    final_score_scale: FrankensearchFinalScoreScale,
    fusion_weights: SearchFusionWeights,
) -> SearchHit {
    let upstream_source = score_source_from_frankensearch(result.source);
    // Frankensearch reports which arm(s) contributed to a hit in `source`.
    // When fusion actually ran, even a one-arm contribution is ranked on the
    // RRF scale; when the orchestrator short-circuited to native lexical or
    // semantic retrieval, the upstream source remains authoritative. Keep the
    // component scores as arm-level provenance and classify the final score by
    // the execution evidence captured above.
    let source = if final_score_scale == FrankensearchFinalScoreScale::RrfFused
        && upstream_source != ScoreSource::Reranked
    {
        ScoreSource::Hybrid
    } else {
        upstream_source
    };
    let rerank_score = result.rerank_score.filter(|score| score.is_finite());
    // Frankensearch intentionally retains the pre-rerank retrieval score in
    // `ScoredResult::score` and writes the cross-encoder's final score to
    // `rerank_score`. EE's public `score`, relevance floor, explanation, and
    // pack ranking all consume `SearchHit::score`, so promote the validated
    // reranker score at this adapter boundary. This also makes the existing
    // `score = rerank_score` explanation truthful.
    let score = if source == ScoreSource::Reranked {
        rerank_score.unwrap_or(result.score)
    } else {
        result.score
    };
    let lexical_score = if upstream_source == ScoreSource::Lexical {
        result.lexical_score.or(Some(result.score))
    } else {
        result.lexical_score
    };
    let mut hit = SearchHit {
        doc_id: result.doc_id.to_string(),
        score,
        source,
        fast_score: result.fast_score,
        quality_score: result.quality_score,
        lexical_score,
        rerank_score,
        metadata: result.metadata.map(|m| (*m).clone()),
        explanation: None,
    };
    let fusion_adjustment = configured_fusion_adjustment(
        &hit,
        final_score_scale == FrankensearchFinalScoreScale::RrfFused,
        fusion_weights,
    );
    if let Some(adjustment) = fusion_adjustment {
        hit.score = (hit.score * adjustment.multiplier).max(0.0);
    }
    if explain {
        let mut explanation = ScoreExplanation::generate(&hit);
        if let Some(adjustment) = fusion_adjustment {
            append_configured_fusion_weight_factor(&mut explanation, fusion_weights, adjustment);
        }
        hit.explanation = Some(explanation);
    }
    hit
}

/// Convert one complete Frankensearch result pool into EE search hits.
///
/// Pure lexical retrieval carries unbounded, query-dependent BM25 values.
/// Normalize that complete pool with Frankensearch's canonical min-max
/// primitive before any relevance-floor, quality, calibration, or pack
/// consumer sees it. The original engine value remains available in
/// `lexicalScore`; ordering is unchanged because min-max normalization is
/// monotone. Other execution modes retain their raw final ranking score.
fn search_hits_from_scored_results(
    results: Vec<crate::search::ScoredResult>,
    explain: bool,
    final_score_scale: FrankensearchFinalScoreScale,
    fusion_weights: SearchFusionWeights,
) -> Vec<SearchHit> {
    let mut hits = results
        .into_iter()
        .map(|result| {
            search_hit_from_scored_result(result, explain, final_score_scale, fusion_weights)
        })
        .collect::<Vec<_>>();

    if final_score_scale == FrankensearchFinalScoreScale::Native
        && hits.iter().all(|hit| hit.source == ScoreSource::Lexical)
    {
        let raw_scores = hits.iter().map(|hit| hit.score).collect::<Vec<_>>();
        let normalized_scores = frankensearch::fusion::normalize_scores(&raw_scores);
        for (hit, normalized_score) in hits.iter_mut().zip(normalized_scores) {
            hit.score = normalized_score;
            if explain {
                hit.explanation = Some(ScoreExplanation::generate(hit));
            }
        }
    }

    hits
}

pub(crate) fn sort_search_hits_by_score_order(hits: &mut [SearchHit]) {
    // bd-2vq2z.30: when a reranker ran, its order must survive to the output.
    // Frankensearch returns reranked candidates carrying `rerank_score`;
    // `search_hit_from_scored_result` promotes that value into the public
    // `.score`. The marker still determines that reranked hits sort ahead of
    // fusion-only hits and amongst themselves by descending rerank score;
    // non-reranked hits use the cross-scale relevance projection and retain
    // native score ordering within one score kind.
    hits.sort_by(rerank_aware_hit_order);
    let mut run_start = 0_usize;
    while run_start < hits.len() {
        let mut run_end = run_start + 1;
        while run_end < hits.len()
            && rerank_aware_hit_order(&hits[run_start], &hits[run_end]) == std::cmp::Ordering::Equal
        {
            run_end += 1;
        }
        if run_end - run_start > 1 {
            sort_search_hit_score_tie_by_doc_id(&mut hits[run_start..run_end]);
        }
        run_start = run_end;
    }
}

fn global_store_lexical_hits(
    options: &SearchOptions,
    effective_limit: u32,
    degraded: &mut Vec<SearchDegradation>,
    preloaded_memories: Option<&mut BTreeMap<String, StoredMemory>>,
) -> Vec<SearchHit> {
    if !global_store_participates_in_scope(options.memory_scope) {
        return Vec::new();
    }
    let query_terms = search_lexical_terms(&options.query);
    if query_terms.is_empty() {
        return Vec::new();
    }
    let paths = match super::global_store::default_global_store_paths_from_env() {
        Ok(paths) => paths,
        Err(error) => {
            if matches!(options.memory_scope, MemoryScope::Global) {
                degraded.push(SearchDegradation::scope_metadata_unavailable(&format!(
                    "global store path resolution failed: {error}"
                )));
            }
            return Vec::new();
        }
    };
    // `[memory] include_global` / `participate` from the workspace config
    // gate the lane (bd-1bfwa.3 slice C); both default to true so the
    // opt-in-by-presence behavior is unchanged for unconfigured workspaces.
    let memory_config = crate::config::workspace_config(&options.workspace_path)
        .map(|config| config.memory)
        .unwrap_or_default();
    let inclusion =
        super::global_store::resolve_global_inclusion(&super::global_store::GlobalInclusionInput {
            store_present: paths.database_path.exists(),
            participating: memory_config.participate.unwrap_or(true),
            config_enabled: memory_config.include_global.unwrap_or(true),
            no_global_flag: false,
        });
    if !inclusion.included {
        if matches!(options.memory_scope, MemoryScope::Global) {
            degraded.push(SearchDegradation::global_memory_disabled(inclusion.reason));
        }
        return Vec::new();
    }
    let memories =
        match super::global_store::read_global_store_memories(&paths, options.include_tombstoned) {
            Ok(memories) => memories,
            Err(error) => {
                degraded.push(SearchDegradation::scope_metadata_unavailable(&format!(
                    "global store read failed: {error}"
                )));
                return Vec::new();
            }
        };
    if memories.is_empty() {
        return Vec::new();
    }

    let reference_time = options.as_of.unwrap_or_else(Utc::now);
    let mut scored = Vec::new();
    let mut preloaded_memories = preloaded_memories;
    for memory in memories {
        if memory.tombstoned_at.is_some() && !options.include_tombstoned {
            continue;
        }
        if !matches!(
            memory_validity_visibility(
                memory.valid_from.as_deref(),
                memory.valid_to.as_deref(),
                reference_time,
                options.include_expired,
                options.include_future,
            ),
            MemoryValidityVisibility::Visible
        ) {
            continue;
        }
        let Some(score) = search_lexical_memory_score(&memory, &query_terms) else {
            continue;
        };
        if let Some(preloaded) = preloaded_memories.as_deref_mut() {
            preloaded
                .entry(memory.id.clone())
                .or_insert_with(|| memory.clone());
        }
        scored.push((memory, score));
    }

    scored.sort_by(|(left_memory, left_score), (right_memory, right_score)| {
        right_score
            .total_cmp(left_score)
            .then_with(|| left_memory.workspace_id.cmp(&right_memory.workspace_id))
            .then_with(|| left_memory.id.cmp(&right_memory.id))
    });

    scored
        .into_iter()
        .take(usize::try_from(effective_limit).unwrap_or(usize::MAX))
        .map(|(memory, score)| SearchHit {
            doc_id: memory.id.clone(),
            score,
            source: ScoreSource::Lexical,
            fast_score: None,
            quality_score: None,
            lexical_score: Some(score),
            rerank_score: None,
            metadata: Some(global_store_search_metadata(&memory, reference_time)),
            explanation: None,
        })
        .collect()
}

fn global_store_participates_in_scope(scope: MemoryScope) -> bool {
    matches!(
        scope,
        MemoryScope::Swarm | MemoryScope::Workspace | MemoryScope::Global
    )
}

fn search_lexical_terms(text: &str) -> BTreeSet<String> {
    text.split(|ch: char| !ch.is_alphanumeric())
        .map(str::trim)
        .filter(|term| term.len() >= 2)
        .map(str::to_ascii_lowercase)
        .collect()
}

fn search_lexical_memory_score(
    memory: &StoredMemory,
    query_terms: &BTreeSet<String>,
) -> Option<f32> {
    let haystack =
        format!("{} {} {}", memory.level, memory.kind, memory.content).to_ascii_lowercase();
    let matched = query_terms
        .iter()
        .filter(|term| haystack.contains(term.as_str()))
        .count();
    (matched > 0).then_some(matched as f32 / query_terms.len() as f32)
}

fn global_store_search_metadata(
    memory: &StoredMemory,
    reference_time: DateTime<Utc>,
) -> serde_json::Value {
    let mut metadata = serde_json::json!({
        "source": "memory",
        "storeLane": super::global_store::GLOBAL_PROVENANCE_LANE,
        "memoryId": &memory.id,
        "memory_id": &memory.id,
        "workspaceId": &memory.workspace_id,
        "workspace_id": &memory.workspace_id,
        "level": &memory.level,
        "kind": &memory.kind,
        "confidence": memory.confidence,
        "utility": memory.utility,
        "importance": memory.importance,
        "provenanceUri": &memory.provenance_uri,
        "createdAt": &memory.created_at,
        "updatedAt": &memory.updated_at,
        "valid_from": &memory.valid_from,
        "valid_to": &memory.valid_to,
        "validity_status": search_validity_status_for_memory(memory, reference_time),
        "memory_scope": MemoryScope::Global.as_str(),
        "tags": [GLOBAL_MEMORY_SCOPE_TAG],
    });
    if let Some(object) = metadata.as_object_mut() {
        object.insert(
            SEARCH_ANALYSIS_CONTENT_KEY.to_owned(),
            serde_json::json!(&memory.content),
        );
        object.insert(
            SEARCH_ANALYSIS_CONFIDENCE_KEY.to_owned(),
            serde_json::json!(memory.confidence),
        );
        object.insert(
            SEARCH_ANALYSIS_UTILITY_KEY.to_owned(),
            serde_json::json!(memory.utility),
        );
        object.insert(
            SEARCH_ANALYSIS_CREATED_AT_KEY.to_owned(),
            serde_json::json!(&memory.created_at),
        );
    }
    metadata
}

fn search_validity_status_for_memory(
    memory: &StoredMemory,
    reference_time: DateTime<Utc>,
) -> &'static str {
    match memory_validity_visibility(
        memory.valid_from.as_deref(),
        memory.valid_to.as_deref(),
        reference_time,
        true,
        true,
    ) {
        MemoryValidityVisibility::Visible => "current",
        MemoryValidityVisibility::Expired => "expired",
        MemoryValidityVisibility::Future => "future",
        MemoryValidityVisibility::Malformed => "invalid",
    }
}

fn preload_returned_search_memories(
    hits: &[SearchHit],
    read_connection: Option<&DbConnection>,
    preloaded_memories: Option<&mut BTreeMap<String, StoredMemory>>,
    degraded: &mut Vec<SearchDegradation>,
) {
    let Some(preloaded) = preloaded_memories else {
        return;
    };
    let missing = hits
        .iter()
        .filter(|hit| hit.doc_id.starts_with("mem_") && !preloaded.contains_key(&hit.doc_id))
        .map(|hit| hit.doc_id.as_str())
        .collect::<Vec<_>>();
    if missing.is_empty() {
        return;
    }
    let Some(connection) = read_connection else {
        return;
    };
    match connection.get_memories_batch(&missing) {
        Ok(memories) => {
            for (memory_id, memory) in memories {
                preloaded.entry(memory_id).or_insert(memory);
            }
        }
        Err(error) => degraded.push(SearchDegradation::scope_metadata_unavailable(&format!(
            "search result memory preload failed: {error}"
        ))),
    }
}

/// Ranking order for final search hits (bd-2vq2z.30). Reranked hits (those
/// carrying a `rerank_score`) outrank fusion-only hits and order amongst
/// themselves by descending rerank score, so a reranker's decision survives to
/// the output instead of being discarded by a fusion-score re-sort. Non-reranked
/// hits use `relevanceScore`, the only comparable cross-source projection. The
/// native score breaks relevance ties within a score kind, preserving engine
/// ordering where the projection clamps multiple values to the same endpoint.
fn rerank_aware_hit_order(left: &SearchHit, right: &SearchHit) -> std::cmp::Ordering {
    match (left.rerank_score, right.rerank_score) {
        (Some(left_rerank), Some(right_rerank)) => right_rerank.total_cmp(&left_rerank),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => right
            .relevance_score()
            .total_cmp(&left.relevance_score())
            .then_with(|| left.score_kind().cmp(right.score_kind()))
            .then_with(|| right.score.total_cmp(&left.score)),
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
                .and_then(|mesh| metadata_string(mesh, "origin_workspace_id"))
        })
        .or_else(|| {
            metadata
                .get("mesh")
                .and_then(|mesh| metadata_string(mesh, "originWorkspaceId"))
        })
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

fn search_backend_cancelled(
    cx: &asupersync::Cx,
    caller_phase: &str,
    backend_phase: &str,
    backend_reason: &str,
) -> SearchError {
    let reason = cx.cancel_reason().unwrap_or_else(|| {
        let kind = crate::core::outcome::cancel_kind_from_backend_reason(backend_reason);
        crate::core::outcome::attributed_cancel_reason(
            cx,
            kind,
            format!(
                "{caller_phase}: Frankensearch cancelled during {backend_phase}: {backend_reason}"
            ),
        )
    });
    SearchError::Cancelled(reason)
}

pub(crate) fn map_frankensearch_error(
    cx: &asupersync::Cx,
    phase: &str,
    error: frankensearch::SearchError,
) -> SearchError {
    match error {
        frankensearch::SearchError::Cancelled {
            phase: backend_phase,
            reason,
        } => search_backend_cancelled(cx, phase, &backend_phase, &reason),
        error => SearchError::Index(format!("{phase}: {error}")),
    }
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
fn search_sync(
    index_dir: &Path,
    query: &str,
    limit: usize,
    config: TwoTierConfig,
    explain: bool,
    source_mode: SearchSourceMode,
    determinism: &Deterministic<Seed>,
    fast_embedder_override: Option<Arc<dyn crate::search::Embedder>>,
) -> Result<(Vec<SearchHit>, Vec<String>), String> {
    let mut trace = SearchPerformanceTrace::default();
    crate::core::run_cli_with_cx(Duration::from_secs(30), |cx| async move {
        search_sync_with_performance(
            &cx,
            index_dir,
            query,
            limit,
            config,
            explain,
            source_mode,
            determinism.shared_child("search.rerank"),
            SearchRerankRuntime::disabled(),
            SearchFusionWeights::default(),
            fast_embedder_override,
            &mut trace,
        )
        .await
    })
    .map_err(|error| error.to_string())?
    .map_err(|error| error.to_string())
}

#[allow(clippy::too_many_arguments)]
async fn search_sync_with_performance(
    cx: &asupersync::Cx,
    index_dir: &Path,
    query: &str,
    limit: usize,
    config: TwoTierConfig,
    explain: bool,
    source_mode: SearchSourceMode,
    rerank_seed: Deterministic<Seed>,
    rerank_runtime: SearchRerankRuntime,
    fusion_weights: SearchFusionWeights,
    fast_embedder_override: Option<Arc<dyn crate::search::Embedder>>,
    trace: &mut SearchPerformanceTrace,
) -> Result<(Vec<SearchHit>, Vec<String>), SearchError> {
    search_checkpoint(cx)?;
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
    trace.record_duration("searchSync::planCache", plan_cache_start.elapsed());

    let index_dir_owned = index_dir.to_path_buf();
    let query_owned = plan_lookup.plan.parsed_query.q.clone();
    let rerank_runtime_owned = rerank_runtime.clone();
    tracing::debug!(
        target: "ee::search::determinism",
        seed_scope = %rerank_seed.scope(),
        seed_hash = %rerank_seed.seed_hash_prefix(),
        "threaded deterministic token through search_sync"
    );
    #[allow(clippy::type_complexity)]
    let result_holder: Arc<Mutex<Option<Result<(Vec<SearchHit>, Vec<String>), SearchError>>>> =
        Arc::new(Mutex::new(None));
    let task_result = Arc::clone(&result_holder);
    let sync_timings: Arc<Mutex<Vec<SearchPerformanceTiming>>> = Arc::new(Mutex::new(Vec::new()));
    let async_timings = Arc::clone(&sync_timings);
    let operation_cx = cx.clone();

    let runtime_start = Instant::now();
    async move {
        let cx = operation_cx;
        if let Err(error) = search_checkpoint(&cx) {
            if let Ok(mut guard) = task_result.lock() {
                *guard = Some(Err(error));
            }
            return;
        }
        if source_mode == SearchSourceMode::LexicalOnly {
            let lexical_open_start = Instant::now();
            let lexical = match open_lexical_searcher(&index_dir_owned) {
                Ok(Some(lexical)) => {
                    push_search_performance_timing(
                        &async_timings,
                        "searchSync::lexicalOpen",
                        lexical_open_start.elapsed(),
                    );
                    lexical
                }
                Ok(None) => {
                    push_search_performance_timing(
                        &async_timings,
                        "searchSync::lexicalOpen",
                        lexical_open_start.elapsed(),
                    );
                    if let Ok(mut guard) = task_result.lock() {
                        *guard = Some(Err(SearchError::Index(
                            "Lexical index not found".to_owned(),
                        )));
                    }
                    return;
                }
                Err(error) => {
                    push_search_performance_timing(
                        &async_timings,
                        "searchSync::lexicalOpen",
                        lexical_open_start.elapsed(),
                    );
                    if let Ok(mut guard) = task_result.lock() {
                        *guard = Some(Err(SearchError::Index(error)));
                    }
                    return;
                }
            };

            let lexical_search_start = Instant::now();
            let search_result = lexical.search(&cx, &query_owned, limit).await;
            push_search_performance_timing(
                &async_timings,
                "searchSync::lexicalSearch",
                lexical_search_start.elapsed(),
            );
            if let Err(error) = search_checkpoint(&cx) {
                if let Ok(mut guard) = task_result.lock() {
                    *guard = Some(Err(error));
                }
                return;
            }
            let convert_start = Instant::now();
            let converted = match search_result {
                Ok(results) => {
                    let mut hits = search_hits_from_scored_results(
                        results,
                        explain,
                        FrankensearchFinalScoreScale::Native,
                        fusion_weights,
                    );
                    canonicalize_equivalent_component_scores(&mut hits, &rerank_seed);
                    sort_search_hits_by_score_order(&mut hits);
                    Ok((hits, Vec::new()))
                }
                Err(error) => Err(map_frankensearch_error(&cx, "Lexical search failed", error)),
            };
            push_search_performance_timing(
                &async_timings,
                "searchSync::hitConversion",
                convert_start.elapsed(),
            );

            if let Ok(mut guard) = task_result.lock() {
                *guard = Some(converted);
            }
            return;
        }

        let two_tier_open_start = Instant::now();
        let index = match TwoTierIndex::open(&index_dir_owned, config.clone()) {
            Ok(idx) => {
                push_search_performance_timing(
                    &async_timings,
                    "searchSync::twoTierOpen",
                    two_tier_open_start.elapsed(),
                );
                Arc::new(idx)
            }
            Err(e) => {
                push_search_performance_timing(
                    &async_timings,
                    "searchSync::twoTierOpen",
                    two_tier_open_start.elapsed(),
                );
                if let Ok(mut guard) = task_result.lock() {
                    *guard = Some(Err(map_frankensearch_error(&cx, "Failed to open index", e)));
                }
                return;
            }
        };

        let embedder_start = Instant::now();
        let fast_embedder = fast_embedder_override
            .unwrap_or_else(|| crate::core::index::default_search_embedder_stack().fast_arc());
        push_search_performance_timing(
            &async_timings,
            "searchSync::embedderInit",
            embedder_start.elapsed(),
        );
        let searcher_build_start = Instant::now();
        let mut searcher = TwoTierSearcher::new(index, fast_embedder, config);
        push_search_performance_timing(
            &async_timings,
            "searchSync::searcherBuild",
            searcher_build_start.elapsed(),
        );
        searcher = if source_mode == SearchSourceMode::Hybrid {
            let attach_start = Instant::now();
            match attach_lexical_searcher(searcher, &index_dir_owned) {
                Ok(searcher) => {
                    push_search_performance_timing(
                        &async_timings,
                        "searchSync::attachLexical",
                        attach_start.elapsed(),
                    );
                    searcher
                }
                Err(error) => {
                    push_search_performance_timing(
                        &async_timings,
                        "searchSync::attachLexical",
                        attach_start.elapsed(),
                    );
                    if let Ok(mut guard) = task_result.lock() {
                        *guard = Some(Err(SearchError::Index(error)));
                    }
                    return;
                }
            }
        } else {
            searcher
        };

        let collect_start = Instant::now();
        let collect_limit = rerank_runtime_owned.collect_limit(limit);
        run_before_search_collect_hook(&cx);
        let search_result = if let Some(text_provider) = rerank_runtime_owned.text_provider.clone()
        {
            searcher
                .search_collect_with_text(&cx, &query_owned, collect_limit, move |doc_id| {
                    text_provider.text_for_doc(doc_id)
                })
                .await
        } else {
            searcher
                .search_collect(&cx, &query_owned, collect_limit)
                .await
        };
        run_after_search_collect_hook(&cx, search_result.is_ok());
        if let Err(error) = search_checkpoint(&cx) {
            if let Ok(mut guard) = task_result.lock() {
                *guard = Some(Err(error));
            }
            return;
        }
        let search_result = match search_result {
            Ok((mut results, metrics)) => {
                if let (Some(reranker), Some(text_provider)) = (
                    rerank_runtime_owned.reranker.clone(),
                    rerank_runtime_owned.text_provider.clone(),
                ) {
                    let rerank_start = Instant::now();
                    let rerank_result = apply_search_rerank(
                        &cx,
                        &query_owned,
                        &mut results,
                        reranker.as_ref(),
                        |doc_id| text_provider.text_for_doc(doc_id),
                        rerank_runtime_owned.top_k,
                    )
                    .await;
                    push_search_performance_timing(
                        &async_timings,
                        "searchSync::rerank",
                        rerank_start.elapsed(),
                    );
                    if let Err(error) = search_checkpoint(&cx) {
                        if let Ok(mut guard) = task_result.lock() {
                            *guard = Some(Err(error));
                        }
                        return;
                    }
                    match rerank_result {
                        Ok(()) => Ok((results, metrics)),
                        Err(error) => Err(error),
                    }
                } else {
                    Ok((results, metrics))
                }
            }
            Err(error) => Err(error),
        };
        push_search_performance_timing(
            &async_timings,
            "searchSync::searchCollect",
            collect_start.elapsed(),
        );

        let convert_start = Instant::now();
        let converted = match search_result {
            Ok((results, metrics)) => {
                let final_score_scale = two_tier_final_score_scale(source_mode, &results, &metrics);
                let reranked_count = results
                    .iter()
                    .filter(|result| result.rerank_score.is_some())
                    .count();
                if rerank_runtime_owned.is_enabled() {
                    tracing::info!(
                        target: "ee::search::rerank",
                        event = "rerank_stage_completed",
                        model_id = rerank_runtime_owned
                            .model_id
                            .as_deref()
                            .unwrap_or("unknown"),
                        requested_top_k = rerank_runtime_owned.top_k,
                        collect_limit,
                        reranked_count,
                    );
                }
                let mut hits = search_hits_from_scored_results(
                    results,
                    explain,
                    final_score_scale,
                    fusion_weights,
                );
                canonicalize_equivalent_component_scores(&mut hits, &rerank_seed);
                sort_search_hits_by_score_order(&mut hits);
                Ok((hits, Vec::new()))
            }
            Err(error) => Err(map_frankensearch_error(&cx, "Search failed", error)),
        };
        push_search_performance_timing(
            &async_timings,
            "searchSync::hitConversion",
            convert_start.elapsed(),
        );

        if let Ok(mut guard) = task_result.lock() {
            *guard = Some(converted);
        }
    }
    .await;
    trace.record_duration("searchSync::runtime", runtime_start.elapsed());
    if let Ok(mut guard) = sync_timings.lock() {
        for timing in guard.drain(..) {
            trace.record_duration(timing.name, timing.elapsed);
        }
    }

    result_holder
        .lock()
        .ok()
        .and_then(|mut guard| guard.take())
        .unwrap_or_else(|| Err(SearchError::Index("Search result not captured".to_owned())))
}

fn push_search_performance_timing(
    timings: &Arc<Mutex<Vec<SearchPerformanceTiming>>>,
    name: &'static str,
    elapsed: Duration,
) {
    if let Ok(mut guard) = timings.lock() {
        guard.push(SearchPerformanceTiming { name, elapsed });
    }
}

/// Fail closed for evidence documents against the live source of truth.
///
/// The derived index is only a cache: neither its document id nor any indexed
/// metadata can authorize an evidence hit. This filter intentionally runs
/// before the relevance floor and query-assist construction so a row whose
/// posture changed after indexing cannot survive through a below-floor hint.
fn apply_live_evidence_visibility(
    options: &SearchOptions,
    hits: Vec<SearchHit>,
    degraded: &mut Vec<SearchDegradation>,
    read_connection: Option<&DbConnection>,
) -> Vec<SearchHit> {
    let evidence_ids = hits
        .iter()
        .filter(|hit| hit.doc_id.starts_with("ev_"))
        .map(|hit| hit.doc_id.clone())
        .collect::<BTreeSet<_>>();
    if evidence_ids.is_empty() {
        return hits;
    }

    let admitted = live_admitted_evidence_doc_ids(options, &evidence_ids, read_connection);
    let mut filtered = 0usize;
    let visible = hits
        .into_iter()
        .filter(|hit| {
            let visible = !hit.doc_id.starts_with("ev_") || admitted.contains(hit.doc_id.as_str());
            if !visible {
                filtered = filtered.saturating_add(1);
            }
            visible
        })
        .collect();
    if filtered > 0 {
        degraded.push(SearchDegradation::evidence_live_admission_filtered(
            filtered,
        ));
    }
    visible
}

fn live_admitted_evidence_doc_ids(
    options: &SearchOptions,
    evidence_ids: &BTreeSet<String>,
    read_connection: Option<&DbConnection>,
) -> BTreeSet<String> {
    if let Some(connection) = read_connection {
        return live_admitted_evidence_doc_ids_with_connection(options, evidence_ids, connection);
    }

    let database_path = options.resolve_database_path();
    let Ok(connection) = DbConnection::open_file_read_only(&database_path) else {
        return BTreeSet::new();
    };
    live_admitted_evidence_doc_ids_with_connection(options, evidence_ids, &connection)
}

fn live_admitted_evidence_doc_ids_with_connection(
    options: &SearchOptions,
    evidence_ids: &BTreeSet<String>,
    connection: &DbConnection,
) -> BTreeSet<String> {
    let canonical_workspace = default_workspace_root(&options.workspace_path);
    let requested = crate::core::curate::stable_workspace_id(&canonical_workspace);
    let Some(workspace_id) = crate::core::workspace::select_existing_workspace_row(
        connection,
        &requested,
        &[&options.workspace_path, canonical_workspace.as_path()],
    )
    .ok()
    .flatten()
    .map(|workspace| workspace.id) else {
        return BTreeSet::new();
    };
    evidence_ids
        .iter()
        .filter_map(|doc_id| {
            let evidence_id = EvidenceId::from_str(doc_id).ok()?;
            if evidence_id.to_string() != *doc_id {
                return None;
            }
            let span = connection.get_evidence_span(doc_id).ok()??;
            let session = connection.get_session(&span.session_id).ok()??;
            span.is_search_admitted_for_session(&workspace_id, &session)
                .then(|| doc_id.clone())
        })
        .collect()
}

fn apply_live_evidence_visibility_to_diag(
    options: &SearchOptions,
    diag: &mut DiagSearchSyncResult,
    degraded: &mut Vec<SearchDegradation>,
) {
    let evidence_ids = diag
        .pre_fusion
        .lexical
        .results
        .iter()
        .chain(&diag.pre_fusion.semantic_fast.results)
        .map(|hit| hit.doc_id.as_str())
        .chain(
            diag.fusion
                .per_doc_contribution
                .iter()
                .map(|hit| hit.doc_id.as_str()),
        )
        .chain(diag.final_hits.iter().map(|hit| hit.doc_id.as_str()))
        .filter(|doc_id| doc_id.starts_with("ev_"))
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    if evidence_ids.is_empty() {
        return;
    }

    let admitted = live_admitted_evidence_doc_ids(options, &evidence_ids, None);
    let is_visible = |doc_id: &str| !doc_id.starts_with("ev_") || admitted.contains(doc_id);
    diag.pre_fusion
        .lexical
        .results
        .retain(|hit| is_visible(&hit.doc_id));
    diag.pre_fusion
        .semantic_fast
        .results
        .retain(|hit| is_visible(&hit.doc_id));
    diag.fusion
        .per_doc_contribution
        .retain(|hit| is_visible(&hit.doc_id));
    diag.final_hits.retain(|hit| is_visible(&hit.doc_id));

    let filtered = evidence_ids.len().saturating_sub(admitted.len());
    if filtered > 0 {
        degraded.push(SearchDegradation::evidence_live_admission_filtered(
            filtered,
        ));
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
    let database_path = options.resolve_database_path();
    if !explicit_database_path && !database_path.exists() {
        return hits;
    }
    let connection = match DbConnection::open_file_read_only(&database_path) {
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
    let mut sealed_filtered = 0usize;
    let mut included = 0usize;
    let mut drift_hints = Vec::new();
    let mut seal_lookup_error = None;
    let reference_time = options.as_of.unwrap_or_else(Utc::now);

    {
        let mut handle_loaded_memory =
            |mut hit: SearchHit, memory: &crate::db::StoredMemory| -> Option<SearchHit> {
                // The canonical index projection omits sealed-unrevealed
                // rows. Keep this read-side guard as defense in depth for
                // indexes published before that eligibility revision. Seal
                // sidecar state is authoritative: ordinary content equal to
                // the placeholder text remains searchable.
                if memory.content == crate::models::MEMORY_SEAL_PLACEHOLDER_CONTENT {
                    match connection.get_memory_seal(&memory.id) {
                        Ok(Some(_)) => {
                            sealed_filtered = sealed_filtered.saturating_add(1);
                            return None;
                        }
                        Ok(_) => {}
                        Err(error) => {
                            seal_lookup_error.get_or_insert_with(|| error.to_string());
                        }
                    }
                }
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

    if let Some(error) = seal_lookup_error {
        degraded.push(SearchDegradation::tombstone_visibility_unavailable(&error));
    }

    let total_before = visible_hits
        .len()
        .saturating_add(filtered)
        .saturating_add(expired_filtered)
        .saturating_add(future_filtered)
        .saturating_add(stale_filtered)
        .saturating_add(malformed_filtered)
        .saturating_add(sealed_filtered);
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
        sealed_filtered_count = sealed_filtered,
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
    let database_path = options.resolve_database_path();
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

    let connection = match DbConnection::open_file_read_only(&database_path) {
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
    let (mut scope_memories, read_error): (
        BTreeMap<String, crate::db::StoredMemory>,
        Option<String>,
    ) = match connection.get_memories_batch(&hit_doc_refs) {
        Ok(memories) => (memories, None),
        Err(error) => (BTreeMap::new(), Some(error.to_string())),
    };
    if let Some(preloaded) = preloaded_memories.as_deref() {
        for memory_id in &hit_doc_ids {
            if let Some(memory) = preloaded.get(memory_id) {
                scope_memories
                    .entry(memory_id.clone())
                    .or_insert_with(|| memory.clone());
            }
        }
    }
    let (scope_tags, tag_read_error): (BTreeMap<String, Vec<String>>, Option<String>) =
        if matches!(scope_context.scope, MemoryScope::Global) {
            match connection.get_memory_tags_batch(&hit_doc_refs) {
                Ok(tags) => (tags, None),
                Err(error) => (BTreeMap::new(), Some(error.to_string())),
            }
        } else {
            (BTreeMap::new(), None)
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
        let metadata_tags = search_hit_metadata_tags(&hit);
        match scope_memories.get(&hit.doc_id) {
            Some(memory) => {
                let tags = scope_tags
                    .get(&hit.doc_id)
                    .or(metadata_tags.as_ref())
                    .map(Vec::as_slice)
                    .unwrap_or(&[]);
                let in_scope = scope_context.memory_in_scope_with_tags(memory, tags);
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
    if let Some(error) = tag_read_error {
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
        if let Some(provenance) = super::memory_scope::team_provenance_from_memory(memory) {
            object.insert("teamProvenance".to_string(), provenance.to_json());
        }
    }
    hit.metadata = Some(metadata);
}

/// Compact teammate suffix already stored on the hit as `teamProvenance`.
/// Human search uses the same `· from Priya / project · time` shape pack
/// markdown and ask citations render; missing/empty fields stay silent.
fn team_search_attribution_suffix(hit: &SearchHit) -> Option<String> {
    let provenance = hit.metadata.as_ref()?.get("teamProvenance")?;
    let member = provenance
        .get("memberDisplayName")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty())?;
    let produced_at = provenance
        .get("producedAt")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|when| !when.is_empty())?;
    let project = provenance
        .get("projectName")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty());
    Some(match project {
        Some(project) => format!(" · from {member} / {project} · {produced_at}"),
        None => format!(" · from {member} · {produced_at}"),
    })
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
fn open_lexical_searcher(index_dir: &Path) -> Result<Option<Arc<dyn LexicalRead>>, String> {
    let lexical_dir = index_dir.join("lexical");
    if !lexical_dir.exists() {
        return Ok(None);
    }

    let key = LexicalSearcherCacheKey {
        index_dir: index_dir.to_path_buf(),
        index_manifest_version: index_manifest_version_for_plan_cache(index_dir),
    };
    let cache = PROCESS_LEXICAL_SEARCHER_CACHE.get_or_init(|| Mutex::new(None));
    let Ok(mut cached) = cache.lock() else {
        return open_lexical_searcher_uncached(&lexical_dir).map(Some);
    };
    if let Some(entry) = cached.as_ref()
        && entry.key == key
    {
        tracing::debug!(
            target: "ee::search::index_cache",
            index_dir = %index_dir.display(),
            index_manifest_version = key.index_manifest_version,
            cache_decision = "hit",
            "reused process-scoped lexical searcher"
        );
        return Ok(Some(Arc::clone(&entry.searcher)));
    }

    // Index publication swaps the complete generation atomically. Drop the
    // cached handle before opening the replacement so Tantivy's writer lock
    // remains attached only to the published generation being served.
    cached.take();
    let searcher = open_lexical_searcher_uncached(&lexical_dir)?;
    tracing::debug!(
        target: "ee::search::index_cache",
        index_dir = %index_dir.display(),
        index_manifest_version = key.index_manifest_version,
        cache_decision = "miss",
        "opened and cached process-scoped lexical searcher"
    );
    *cached = Some(CachedLexicalSearcher {
        key,
        searcher: Arc::clone(&searcher),
    });
    Ok(Some(searcher))
}

#[cfg(feature = "lexical-bm25")]
fn open_lexical_searcher_uncached(lexical_dir: &Path) -> Result<Arc<dyn LexicalRead>, String> {
    TantivyIndex::open(lexical_dir)
        .map(|lexical| Arc::new(lexical) as Arc<dyn LexicalRead>)
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
fn open_lexical_searcher(_index_dir: &Path) -> Result<Option<Arc<dyn LexicalRead>>, String> {
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{
        CreateEvidenceSpanInput, CreateFeedbackEventInput, CreateMemoryInput, CreateSessionInput,
        CreateWorkspaceInput, DbConnection, EvidenceProducerKind,
    };
    use crate::search::{Embedder, EmbedderStack, IndexBuilder, IndexableDocument};

    type TestResult = Result<(), String>;

    fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
        if condition {
            Ok(())
        } else {
            Err(message.into())
        }
    }

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

    fn write_current_index_metadata(index_dir: &Path, documents_total: u32) -> TestResult {
        crate::core::index::write_memory_eval_index_metadata(index_dir, documents_total)
            .map_err(|error| error.to_string())
    }

    fn run_similar_with_posture(
        options: &SimilarOptions,
        posture: EmbeddingPosture,
    ) -> Result<SimilarReport, SimilarError> {
        crate::core::run_cli_with_cx(Duration::from_secs(30), |cx| async move {
            run_similar_with_cx_and_posture(&cx, options, Some(posture)).await
        })
        .map_err(|error| {
            SimilarError::Search(SearchError::Index(format!(
                "Failed to start isolated similar-search runtime: {error}"
            )))
        })?
    }

    fn run_diag_search_with_embedder(
        options: &SearchOptions,
        fast_embedder: Arc<dyn Embedder>,
    ) -> Result<SearchDiagnosticReport, SearchError> {
        crate::core::run_cli_with_cx(Duration::from_secs(30), |cx| async move {
            run_diag_search_with_cx_and_embedder(&cx, options, Some(fast_embedder)).await
        })
        .map_err(|error| {
            SearchError::Index(format!(
                "Failed to start isolated diagnostic-search runtime: {error}"
            ))
        })?
    }

    fn collection_cancellation_index(label: &str) -> Result<PathBuf, String> {
        let index_dir = unique_test_dir(label);
        let build_index_dir = index_dir.clone();
        crate::core::run_cli_future(async move {
            let cx = asupersync::Cx::for_testing();
            let stack = EmbedderStack::from_parts(
                Arc::new(HashEmbedder::default_256()) as Arc<dyn Embedder>,
                None,
            );
            IndexBuilder::new(&build_index_dir)
                .with_embedder_stack(stack)
                .add_documents(vec![IndexableDocument::new(
                    "mem_collection_cancel",
                    "Preserve exact caller cancellation during Frankensearch collection.",
                )])
                .build(&cx)
                .await
                .map_err(|error| error.to_string())?;
            Ok::<(), String>(())
        })
        .map_err(|error| error.to_string())??;
        Ok(index_dir)
    }

    fn run_collection_cancellation_probe(
        index_dir: PathBuf,
        seed: u64,
    ) -> Result<asupersync::CancelReason, String> {
        let observation: Arc<Mutex<Option<Result<asupersync::CancelReason, String>>>> =
            Arc::new(Mutex::new(None));
        let task_observation = Arc::clone(&observation);
        let mut lab = asupersync::LabRuntime::new(asupersync::LabConfig::new(seed).max_steps(256));
        let root = lab.state.create_root_region(asupersync::Budget::INFINITE);
        let (task_id, _handle) = lab
            .state
            .create_task(root, asupersync::Budget::INFINITE, async move {
                let result = if let Some(cx) = asupersync::Cx::current() {
                    let mut trace = SearchPerformanceTrace::default();
                    let determinism = Deterministic::from_seed(seed);
                    match search_sync_with_performance(
                        &cx,
                        &index_dir,
                        "cancellation collection",
                        5,
                        TwoTierConfig::default(),
                        false,
                        SearchSourceMode::SemanticOnly,
                        determinism.shared_child("search.rerank"),
                        SearchRerankRuntime::disabled(),
                        SearchFusionWeights::default(),
                        Some(Arc::new(HashEmbedder::default_256()) as Arc<dyn Embedder>),
                        &mut trace,
                    )
                    .await
                    {
                        Err(SearchError::Cancelled(reason)) => Ok(reason),
                        Err(error) => Err(format!(
                            "collection cancellation must remain typed, got {error:?}"
                        )),
                        Ok((hits, errors)) => Err(format!(
                            "cancelled collection returned hits={hits:?}, errors={errors:?}"
                        )),
                    }
                } else {
                    Err("LabRuntime search task did not install a Cx".to_owned())
                };
                if let Ok(mut slot) = task_observation.lock() {
                    *slot = Some(result);
                }
                asupersync::Outcome::<(), String>::Ok(())
            })
            .map_err(|error| format!("create collection cancellation task: {error}"))?;
        lab.scheduler.lock().schedule(task_id, 0);

        let report = lab.run_until_quiescent_with_report();
        ensure(
            report.quiescent,
            "collection cancellation LabRuntime must quiesce",
        )?;
        ensure(
            report.invariant_violations.is_empty(),
            format!(
                "collection cancellation must preserve LabRuntime invariants: {:?}",
                report.invariant_violations
            ),
        )?;
        let observed = observation
            .lock()
            .map_err(|_| "collection cancellation observation poisoned".to_owned())?
            .take()
            .ok_or_else(|| "collection cancellation observation missing".to_owned())?;
        observed
    }

    #[test]
    fn lab_runtime_cancellation_at_search_collection_preserves_reason() -> TestResult {
        let index_dir = collection_cancellation_index("collection-cancellation")?;
        let expected_message = "caller stopped search during collection";
        install_before_search_collect_hook(move |cx| {
            cx.set_cancel_reason(
                asupersync::CancelReason::deadline().with_message(expected_message),
            );
        });

        let reason = run_collection_cancellation_probe(index_dir, 0xEE_90C)?;
        ensure(
            reason.kind == asupersync::CancelKind::Deadline,
            format!("unexpected collection cancellation kind: {:?}", reason.kind),
        )?;
        ensure(
            reason.message.as_deref() == Some(expected_message),
            format!(
                "unexpected collection cancellation message: {:?}",
                reason.message
            ),
        )
    }

    #[test]
    fn lab_runtime_cancellation_after_search_collection_cannot_escape_as_success() -> TestResult {
        let index_dir = collection_cancellation_index("post-collection-cancellation")?;
        let expected_message = "caller stopped search immediately after collection";
        let collection_succeeded = Arc::new(Mutex::new(None));
        let collection_succeeded_for_hook = Arc::clone(&collection_succeeded);
        install_after_search_collect_hook(move |cx, succeeded| {
            if let Ok(mut observed) = collection_succeeded_for_hook.lock() {
                *observed = Some(succeeded);
            }
            cx.set_cancel_reason(asupersync::CancelReason::user(expected_message));
        });

        let reason = run_collection_cancellation_probe(index_dir, 0xEE_90F)?;
        ensure(
            collection_succeeded
                .lock()
                .map_err(|_| "post-collection hook observation poisoned".to_owned())?
                .take()
                == Some(true),
            "post-collection hook must run after a successful Frankensearch collection",
        )?;
        ensure(
            reason.kind == asupersync::CancelKind::User,
            format!(
                "unexpected post-collection cancellation kind: {:?}",
                reason.kind
            ),
        )?;
        ensure(
            reason.message.as_deref() == Some(expected_message),
            format!(
                "unexpected post-collection cancellation message: {:?}",
                reason.message
            ),
        )
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
        batch.flush_best_effort_with_connection(&connection);

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
        let connection = DbConnection::open_memory().expect("open in-memory audit fixture");
        let batch = SearchAuditBatch::new(0);
        batch.flush_best_effort_with_connection(&connection);
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

    fn test_memory_input(workspace_id: &str, content: &str) -> CreateMemoryInput {
        CreateMemoryInput {
            workspace_id: workspace_id.to_owned(),
            level: "procedural".to_owned(),
            kind: "rule".to_owned(),
            content: content.to_owned(),
            workflow_id: None,
            confidence: 0.9,
            utility: 0.7,
            importance: 0.6,
            provenance_uri: Some("test://similar".to_owned()),
            trust_class: "human_explicit".to_owned(),
            trust_subclass: None,
            tags: Vec::new(),
            valid_from: None,
            valid_to: None,
        }
    }

    fn fixture_hash_embedding_posture_for_search() -> EmbeddingPosture {
        EmbeddingPosture {
            schema: crate::models::EMBEDDING_POSTURE_SCHEMA_V1,
            mode: crate::models::EMBEDDING_POSTURE_MODE_DETERMINISTIC_HASH,
            semantic: false,
            source: "frankensearch_hash_fallback".to_owned(),
            fast_model_id: "hash-256".to_owned(),
            fast_dimension: 256,
            quality_model_id: None,
            quality_dimension: None,
            deterministic: true,
            registered_model_count: 0,
            available_model_count: 0,
            selected_registry_model: None,
            vector_coverage: crate::core::index::EmbeddingVectorCoverage {
                embedded: 3,
                total: 3,
            },
        }
    }

    fn fixture_pending_embedding_posture_for_search() -> EmbeddingPosture {
        EmbeddingPosture {
            schema: crate::models::EMBEDDING_POSTURE_SCHEMA_V1,
            mode: crate::models::EMBEDDING_POSTURE_MODE_NEURAL_LOCAL_PENDING,
            semantic: false,
            source: "ee_model2vec_download_pending".to_owned(),
            fast_model_id: "potion-multilingual-128M".to_owned(),
            fast_dimension: 256,
            quality_model_id: None,
            quality_dimension: None,
            deterministic: true,
            registered_model_count: 0,
            available_model_count: 0,
            selected_registry_model: None,
            vector_coverage: crate::core::index::EmbeddingVectorCoverage {
                embedded: 0,
                total: 3,
            },
        }
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
    fn similar_report_data_json_reuses_search_result_shape() {
        let mut report = SearchReport {
            index_freshness: None,
            status: SearchStatus::Success,
            embed_backend: EmbedBackend::HashFallback,
            query: "mem_00000000000000000000000001".to_string(),
            requested_limit: 1,
            results: vec![SearchHit {
                doc_id: "mem_00000000000000000000000002".to_string(),
                score: 0.91,
                source: ScoreSource::SemanticFast,
                fast_score: Some(0.91),
                quality_score: None,
                lexical_score: None,
                rerank_score: None,
                metadata: None,
                explanation: None,
            }],
            elapsed_ms: 2.0,
            errors: Vec::new(),
            degraded: Vec::new(),
            runtime_profile: test_runtime_profile(),
            rerank_configured_mode: crate::config::SearchRerankMode::Auto,
            rerank_configured_top_k: DEFAULT_SEARCH_RERANK_TOP_K,
            rerank_runtime_available: false,
            relevance_floor_applied: Some(0.05),
            candidates_below_floor: 0,
            query_assist: None,
            source_mode_requested: SearchSourceMode::SemanticOnly,
            source_mode_applied: SearchSourceMode::SemanticOnly,
            source_mode_fallback: false,
            strict_source_mode: false,
            memory_scope: MemoryScope::Swarm,
            strict_scope: false,
            scope_stats: test_scope_stats(),
        };
        remove_similar_target_and_truncate(&mut report, "mem_00000000000000000000000001", 1);
        let similar = SimilarReport {
            target_memory_id: "mem_00000000000000000000000001".to_string(),
            target_level: "procedural".to_string(),
            target_kind: "rule".to_string(),
            semantic_available: true,
            lexical_fallback: false,
            embedding_posture: fixture_hash_embedding_posture_for_search(),
            report,
        };

        let json = similar.data_json();

        assert_eq!(json["command"], "similar");
        assert_eq!(
            json["embed_backend"],
            serde_json::json!(EmbedBackend::HashFallback.as_str())
        );
        assert!(
            similar
                .human_summary()
                .contains("embed_backend: hash_fallback"),
            "human retrieval output must expose the same backend token"
        );
        assert_eq!(
            json["targetMemoryId"],
            serde_json::json!("mem_00000000000000000000000001")
        );
        assert_eq!(
            json["results"][0]["memoryId"],
            "mem_00000000000000000000000002"
        );
        assert_eq!(json["results"][0]["schema"], serde_json::Value::Null);
        assert_eq!(
            json["request"]["similarityMode"],
            serde_json::json!("lexical_fallback")
        );
    }

    #[test]
    fn similar_removes_seed_memory_and_truncates_neighbors() {
        let mut report = SearchReport {
            index_freshness: None,
            status: SearchStatus::Success,
            embed_backend: EmbedBackend::HashFallback,
            query: "release checks".to_string(),
            requested_limit: 2,
            results: vec![
                SearchHit {
                    doc_id: "mem_seed".to_string(),
                    score: 1.0,
                    source: ScoreSource::SemanticFast,
                    fast_score: Some(1.0),
                    quality_score: None,
                    lexical_score: None,
                    rerank_score: None,
                    metadata: None,
                    explanation: None,
                },
                SearchHit {
                    doc_id: "mem_related_a".to_string(),
                    score: 0.8,
                    source: ScoreSource::SemanticFast,
                    fast_score: Some(0.8),
                    quality_score: None,
                    lexical_score: None,
                    rerank_score: None,
                    metadata: None,
                    explanation: None,
                },
                SearchHit {
                    doc_id: "mem_related_b".to_string(),
                    score: 0.7,
                    source: ScoreSource::SemanticFast,
                    fast_score: Some(0.7),
                    quality_score: None,
                    lexical_score: None,
                    rerank_score: None,
                    metadata: None,
                    explanation: None,
                },
            ],
            elapsed_ms: 1.0,
            errors: Vec::new(),
            degraded: Vec::new(),
            runtime_profile: test_runtime_profile(),
            rerank_configured_mode: crate::config::SearchRerankMode::Auto,
            rerank_configured_top_k: DEFAULT_SEARCH_RERANK_TOP_K,
            rerank_runtime_available: false,
            relevance_floor_applied: Some(0.0),
            candidates_below_floor: 0,
            query_assist: None,
            source_mode_requested: SearchSourceMode::SemanticOnly,
            source_mode_applied: SearchSourceMode::SemanticOnly,
            source_mode_fallback: false,
            strict_source_mode: false,
            memory_scope: MemoryScope::Swarm,
            strict_scope: false,
            scope_stats: test_scope_stats(),
        };

        remove_similar_target_and_truncate(&mut report, "mem_seed", 1);

        assert_eq!(report.status, SearchStatus::Success);
        assert_eq!(report.results.len(), 1);
        assert_eq!(report.results[0].doc_id, "mem_related_a");
    }

    #[test]
    fn similar_hash_posture_reason_is_stable() {
        let posture = fixture_hash_embedding_posture_for_search();

        assert_eq!(
            similar_semantic_unavailable_reason(&posture),
            "embedding posture mode=deterministic_hash source=frankensearch_hash_fallback semantic=false"
        );
    }

    #[test]
    fn similar_pending_posture_is_distinct_from_hash_fallback() {
        let posture = fixture_pending_embedding_posture_for_search();

        assert!(!posture.semantic);
        assert!(posture.semantic_pending());
        assert!(similar_semantic_request_capable(&posture));
        assert_eq!(similarity_mode_for_posture(&posture), "semantic_pending");
        assert_eq!(
            similar_semantic_unavailable_reason(&posture),
            "embedding posture mode=neural_local_pending source=ee_model2vec_download_pending semantic=false"
        );
    }

    #[test]
    fn active_search_embedder_is_semantic_matches_state_probe() {
        assert_eq!(
            active_search_embedder_is_semantic(),
            active_search_embedder_state() == ActiveSearchEmbedderState::ReadySemantic
        );
    }

    #[test]
    fn embed_model_path_fault_reason_is_pure_and_stable() {
        assert!(embed_model_unavailable_reason_from_path_value(None).is_none());
        assert!(embed_model_unavailable_reason_from_path_value(Some("  ")).is_none());

        let reason = embed_model_unavailable_reason_from_path_value(Some(
            "/definitely/missing/ee-embed-model-path-fixture",
        ))
        .expect("missing path should produce fault-injection reason");

        assert!(reason.contains("EE_EMBED_MODEL_PATH"));
        assert!(reason.contains("/definitely/missing/ee-embed-model-path-fixture"));
    }

    #[test]
    fn run_similar_hash_embedder_falls_back_to_lexical_and_excludes_seed() -> TestResult {
        let temp = tempfile::Builder::new()
            .prefix("ee-similar-hash")
            .tempdir()
            .map_err(|error| error.to_string())?;
        let workspace = temp.path().to_path_buf();
        let database_path = workspace.join("ee.db");
        let index_dir = workspace.join("index");
        let connection =
            DbConnection::open_file(&database_path).map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        let workspace_id = "wsp_00000000000000000000001013";
        connection
            .insert_workspace(
                workspace_id,
                &CreateWorkspaceInput {
                    path: workspace.display().to_string(),
                    name: Some("similar-hash".to_string()),
                },
            )
            .map_err(|error| error.to_string())?;
        let memories = [
            (
                "mem_00000000000000000000000001",
                "Before release run cargo fmt and cargo clippy checks.",
            ),
            (
                "mem_00000000000000000000000002",
                "Release checklist requires cargo fmt before cargo clippy.",
            ),
            (
                "mem_00000000000000000000000003",
                "Design review notes for onboarding copy and screenshots.",
            ),
        ];
        for (memory_id, content) in memories {
            connection
                .insert_memory(memory_id, &test_memory_input(workspace_id, content))
                .map_err(|error| error.to_string())?;
        }
        let documents = memories
            .into_iter()
            .map(|(memory_id, content)| IndexableDocument::new(memory_id, content))
            .collect::<Vec<_>>();
        let build_index_dir = index_dir.clone();
        crate::core::run_cli_future(async move {
            let cx = asupersync::Cx::for_testing();
            let stack = EmbedderStack::from_parts(
                Arc::new(HashEmbedder::default_256()) as Arc<dyn Embedder>,
                None,
            );
            // Mirrors the production rebuild: frankensearch's IndexBuilder no
            // longer writes a Tantivy arm on the cass-compat lane, so ee's
            // own lexical arm runs after the vector build.
            #[cfg(feature = "lexical-bm25")]
            let lexical_documents = documents.clone();
            IndexBuilder::new(&build_index_dir)
                .with_embedder_stack(stack)
                .add_documents(documents)
                .build(&cx)
                .await
                .map_err(|error| error.to_string())?;
            #[cfg(feature = "lexical-bm25")]
            crate::core::index::build_lexical_tier(&cx, &build_index_dir, &lexical_documents)
                .await
                .map_err(|error| error.to_string())?;
            Ok::<(), String>(())
        })
        .map_err(|error| error.to_string())??;
        write_current_index_metadata(&index_dir, 3)?;

        let similar = run_similar_with_posture(
            &SimilarOptions {
                workspace_path: workspace.clone(),
                database_path: Some(database_path),
                index_dir: Some(index_dir),
                memory_id: "mem_00000000000000000000000001".to_string(),
                limit: 2,
                min_score: Some(0.0),
                speed: SpeedMode::Default,
                explain: true,
                as_of: None,
                include_tombstoned: false,
                include_expired: false,
                include_future: false,
                include_stale: false,
                memory_scope: MemoryScope::Swarm,
                strict_scope: false,
            },
            fixture_hash_embedding_posture_for_search(),
        )
        .map_err(|error| error.to_string())?;

        assert!(!similar.semantic_available);
        assert!(similar.lexical_fallback);
        assert_eq!(
            similar.report.source_mode_requested,
            SearchSourceMode::SemanticOnly
        );
        assert_eq!(
            similar.report.source_mode_applied,
            SearchSourceMode::LexicalOnly
        );
        assert!(similar.report.source_mode_fallback);
        assert!(
            similar
                .report
                .degraded
                .iter()
                .any(|degradation| degradation.code == "embed_model_unavailable")
        );
        assert!(
            similar
                .report
                .results
                .iter()
                .all(|hit| hit.doc_id != "mem_00000000000000000000000001")
        );
        #[cfg(feature = "lexical-bm25")]
        {
            let neighbor_ids = similar
                .report
                .results
                .iter()
                .map(|hit| hit.doc_id.as_str())
                .collect::<Vec<_>>();
            assert!(
                neighbor_ids.contains(&"mem_00000000000000000000000002"),
                "related release-check memory should be a lexical fallback neighbor: {neighbor_ids:?}"
            );
            assert!(
                !neighbor_ids.contains(&"mem_00000000000000000000000003")
                    || neighbor_ids
                        .iter()
                        .position(|id| *id == "mem_00000000000000000000000002")
                        <= neighbor_ids
                            .iter()
                            .position(|id| *id == "mem_00000000000000000000000003"),
                "related release-check memory should not rank after unrelated design copy: {neighbor_ids:?}"
            );
        }
        Ok(())
    }

    #[test]
    fn search_revision_metadata_is_absent_by_default_and_stable_when_revisable() {
        let report = SearchReport {
            index_freshness: None,
            status: SearchStatus::Success,
            embed_backend: EmbedBackend::HashFallback,
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
            rerank_configured_mode: crate::config::SearchRerankMode::Auto,
            rerank_configured_top_k: DEFAULT_SEARCH_RERANK_TOP_K,
            rerank_runtime_available: false,
            relevance_floor_applied: None,
            candidates_below_floor: 0,
            query_assist: None,
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
            index_freshness: None,
            status: SearchStatus::Success,
            embed_backend: EmbedBackend::HashFallback,
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
                metadata: Some(serde_json::json!({
                    (SEARCH_ANALYSIS_PROVENANCE_URI_KEY):
                        "journal://jrn_018ff8dc-3d85-7cc0-a6fd-a2d479e6d401",
                })),
                explanation: None,
            }],
            elapsed_ms: 12.3,
            errors: Vec::new(),
            degraded: Vec::new(),
            runtime_profile: test_runtime_profile(),
            rerank_configured_mode: crate::config::SearchRerankMode::Auto,
            rerank_configured_top_k: DEFAULT_SEARCH_RERANK_TOP_K,
            rerank_runtime_available: false,
            relevance_floor_applied: None,
            candidates_below_floor: 0,
            query_assist: None,
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
        // bd-1h4nu: this hit carries no calibration metadata, so the trivial
        // [0,1] interval must read as "unknown" — coverageGuarantee null and
        // calibrated false — not a misleading 0.95 coverage claim.
        assert_eq!(
            json["results"][0]["coverageGuarantee"],
            serde_json::Value::Null
        );
        assert_eq!(json["results"][0]["calibrated"], serde_json::json!(false));
        assert!(json["results"][0]["why"].is_string());
        assert!(json["results"][0]["provenance"].is_array());
        assert_eq!(
            json["results"][0]["provenance"][0],
            serde_json::json!({
                "kind": "provenance_uri",
                "uri": "journal://jrn_018ff8dc-3d85-7cc0-a6fd-a2d479e6d401",
            }),
            "public search provenance must include the pinned memory provenance",
        );
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
        // ExFAT/FAT temp mounts (agent shells on the Mac dev host point
        // TMPDIR at an ExFAT volume) silently ignore POSIX mode bits.
        // When the strip does not take effect, the PermissionDenied
        // branch under test is unreachable on this host — skip like the
        // root-user guard above (bd-29o1y env-sensitivity class).
        if std::fs::File::open(&path).is_ok() {
            return Ok(());
        }

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

    fn feedback_event_full_loads_for_test(workspace_path: &Path) -> usize {
        let workspace_root = default_workspace_root(workspace_path);
        SEARCH_SCORE_CALIBRATION_FEEDBACK_EVENT_FULL_LOADS
            .get()
            .and_then(|full_loads| full_loads.lock().ok())
            .and_then(|full_loads| full_loads.get(&workspace_root).copied())
            .unwrap_or(0)
    }

    fn insert_calibration_feedback_event(
        connection: &DbConnection,
        workspace_id: &str,
        index: usize,
        score: f32,
        truth: f32,
    ) -> TestResult {
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
                &format!("fb_cache_{index:020}"),
                &CreateFeedbackEventInput {
                    workspace_id: workspace_id.to_owned(),
                    target_type: "candidate".to_owned(),
                    target_id: format!("cand_cache_{index:02}"),
                    signal: "confirmation".to_owned(),
                    weight: 1.0,
                    source_type: "outcome_observed".to_owned(),
                    source_id: Some("curate-validation".to_owned()),
                    reason: Some("curate validation accepted relevance label".to_owned()),
                    evidence_json: Some(evidence),
                    session_id: None,
                },
            )
            .map_err(|error| error.to_string())
    }

    fn hit_score_calibration_sample_count(hit: &SearchHit) -> Result<u64, String> {
        hit.metadata
            .as_ref()
            .and_then(|metadata| metadata.pointer("/scoreCalibration/sampleCount"))
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| "scoreCalibration.sampleCount missing".to_string())
    }

    #[test]
    fn search_score_calibration_cache_reuses_feedback_until_fingerprint_changes() -> TestResult {
        let workspace = unique_test_dir("score-calibration-feedback-cache");
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
                    name: Some("score-calibration-feedback-cache".to_owned()),
                },
            )
            .map_err(|error| error.to_string())?;

        for index in 0..MIN_SEARCH_SCORE_CALIBRATION_SAMPLES {
            let score = 0.50 + (index as f32 * 0.01);
            insert_calibration_feedback_event(
                &connection,
                &workspace_id,
                index,
                score,
                score - 0.04,
            )?;
        }

        let mut first_hits = vec![synthetic_hit("mem_feedback_calibration_cache_1", 0.8)];
        let mut first_degraded = Vec::new();
        annotate_hits_with_score_calibration(
            &workspace,
            Some(&database_path),
            Some(&connection),
            &mut first_hits,
            &mut first_degraded,
        );
        assert!(first_degraded.is_empty());
        assert_eq!(feedback_event_full_loads_for_test(&workspace), 1);
        assert_eq!(
            hit_score_calibration_sample_count(&first_hits[0])?,
            MIN_SEARCH_SCORE_CALIBRATION_SAMPLES as u64
        );

        let mut second_hits = vec![synthetic_hit("mem_feedback_calibration_cache_2", 0.8)];
        let mut second_degraded = Vec::new();
        annotate_hits_with_score_calibration(
            &workspace,
            Some(&database_path),
            Some(&connection),
            &mut second_hits,
            &mut second_degraded,
        );
        assert!(second_degraded.is_empty());
        assert_eq!(
            feedback_event_full_loads_for_test(&workspace),
            1,
            "unchanged feedback fingerprint must reuse derived calibration"
        );
        assert_eq!(
            hit_score_calibration_sample_count(&second_hits[0])?,
            MIN_SEARCH_SCORE_CALIBRATION_SAMPLES as u64
        );

        insert_calibration_feedback_event(
            &connection,
            &workspace_id,
            MIN_SEARCH_SCORE_CALIBRATION_SAMPLES,
            0.90,
            0.85,
        )?;
        let mut third_hits = vec![synthetic_hit("mem_feedback_calibration_cache_3", 0.8)];
        let mut third_degraded = Vec::new();
        annotate_hits_with_score_calibration(
            &workspace,
            Some(&database_path),
            Some(&connection),
            &mut third_hits,
            &mut third_degraded,
        );
        assert!(third_degraded.is_empty());
        assert_eq!(
            feedback_event_full_loads_for_test(&workspace),
            2,
            "new feedback row must invalidate the derived calibration cache"
        );
        assert_eq!(
            hit_score_calibration_sample_count(&third_hits[0])?,
            (MIN_SEARCH_SCORE_CALIBRATION_SAMPLES + 1) as u64
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
        let workspace_id = crate::core::curate::stable_workspace_id(&workspace);
        let feedback_events = vec![StoredFeedbackEvent {
            id: "fb_malformed_calibration_0001".to_owned(),
            workspace_id,
            target_type: "candidate".to_owned(),
            target_id: "cand_malformed".to_owned(),
            signal: "confirmation".to_owned(),
            weight: 1.0,
            source_type: "outcome_observed".to_owned(),
            source_id: Some("curate-validation".to_owned()),
            reason: Some("bad calibration evidence".to_owned()),
            evidence_json: Some("{not json".to_owned()),
            session_id: None,
            applied_at: None,
            created_at: "2026-06-06T00:00:00Z".to_owned(),
        }];

        let calibration = SearchScoreCalibration::for_workspace_with_feedback_events(
            &workspace,
            &feedback_events,
        );
        let data = calibration.data_json();

        assert_eq!(
            calibration.status,
            SearchScoreCalibrationStatus::Insufficient
        );
        assert_eq!(calibration.feedback_event_malformed_count, 1);
        assert_eq!(calibration.feedback_event_sample_count, 0);
        assert_eq!(
            data.pointer("/sourceBreakdown/feedbackEventsMalformed")
                .and_then(serde_json::Value::as_u64),
            Some(1)
        );
        assert_eq!(
            data.pointer("/sourceBreakdown/feedbackEvents")
                .and_then(serde_json::Value::as_u64),
            Some(0)
        );
        assert_eq!(
            data.pointer("/sourceBreakdown/feedbackEventsReadStatus")
                .and_then(serde_json::Value::as_str),
            Some("ok")
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
            index_freshness: None,
            status: SearchStatus::Success,
            embed_backend: EmbedBackend::HashFallback,
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
            rerank_configured_mode: crate::config::SearchRerankMode::Auto,
            rerank_configured_top_k: DEFAULT_SEARCH_RERANK_TOP_K,
            rerank_runtime_available: false,
            relevance_floor_applied: None,
            candidates_below_floor: 0,
            query_assist: None,
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
        // 83600051: a present-but-unsafe producerPeerLabel must alias the
        // peer instead of falling back to the raw producer_peer_id, so an
        // unsafe label can never leak the underlying peer identity.
        let producer_peer = provenance["producerPeer"]
            .as_str()
            .expect("producerPeer must be a string");
        assert!(
            producer_peer.starts_with("mesh_peer_"),
            "unsafe producerPeerLabel must yield a stable mesh_peer_ alias, got {producer_peer}"
        );
        assert_ne!(
            producer_peer, "peer_builder_one",
            "raw producer peer id must not leak when the label is unsafe"
        );
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
            index_freshness: None,
            status: SearchStatus::Success,
            embed_backend: EmbedBackend::HashFallback,
            query: "mesh query".to_string(),
            requested_limit: 10,
            results: visible,
            elapsed_ms: 12.3,
            errors: Vec::new(),
            degraded: Vec::new(),
            runtime_profile: test_runtime_profile(),
            rerank_configured_mode: crate::config::SearchRerankMode::Auto,
            rerank_configured_top_k: DEFAULT_SEARCH_RERANK_TOP_K,
            rerank_runtime_available: false,
            relevance_floor_applied: None,
            candidates_below_floor: 0,
            query_assist: None,
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
            index_freshness: None,
            status: SearchStatus::Success,
            embed_backend: EmbedBackend::HashFallback,
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
            rerank_configured_mode: crate::config::SearchRerankMode::Auto,
            rerank_configured_top_k: DEFAULT_SEARCH_RERANK_TOP_K,
            rerank_runtime_available: false,
            relevance_floor_applied: None,
            candidates_below_floor: 0,
            query_assist: None,
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

        let resolution = resolve_source_mode(&options, &index_dir, &mut degraded, None)
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

        let error = match resolve_source_mode(&options, &index_dir, &mut degraded, None) {
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

        let resolution = resolve_source_mode(&options, &index_dir, &mut degraded, None)
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

        let resolution = resolve_source_mode(&options, &index_dir, &mut degraded, None)
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
                semantic_embedder_pending: None,
                semantic_embedder_degraded: None,
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
                semantic_embedder_pending: None,
                semantic_embedder_degraded: None,
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
                semantic_embedder_pending: None,
                semantic_embedder_degraded: None,
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
    fn hash_fallback_forces_lexical_only_without_fast_scoring() -> TestResult {
        let options = source_mode_test_options(SearchSourceMode::Hybrid, false);
        let mut degraded = Vec::new();
        let resolution = resolve_source_mode_with_tiers(
            &options,
            &mut degraded,
            SearchTierState {
                lexical_available: true,
                embed_model_unavailable: None,
                semantic_embedder_pending: None,
                semantic_embedder_degraded: Some(HASH_FALLBACK_SEMANTIC_UNAVAILABLE_REASON),
            },
        )
        .map_err(|error| error.to_string())?;

        assert_eq!(resolution.applied, SearchSourceMode::LexicalOnly);
        assert!(resolution.fallback_applied);
        assert!(!resolution.unavailable_no_results);
        assert_eq!(degraded.len(), 1);
        assert_eq!(degraded[0].code, "embed_model_unavailable");
        assert!(
            degraded[0].message.contains("frankensearch_hash_fallback"),
            "hash fallback reason should reach the degraded message: {}",
            degraded[0].message
        );
        Ok(())
    }

    #[test]
    fn hash_fallback_never_reenters_hybrid_when_lexical_is_unavailable() -> TestResult {
        let options = source_mode_test_options(SearchSourceMode::Hybrid, false);
        let mut degraded = Vec::new();
        let resolution = resolve_source_mode_with_tiers(
            &options,
            &mut degraded,
            SearchTierState {
                lexical_available: false,
                embed_model_unavailable: None,
                semantic_embedder_pending: None,
                semantic_embedder_degraded: Some(HASH_FALLBACK_SEMANTIC_UNAVAILABLE_REASON),
            },
        )
        .map_err(|error| error.to_string())?;

        assert_eq!(resolution.applied, SearchSourceMode::LexicalOnly);
        assert!(resolution.fallback_applied);
        assert!(resolution.unavailable_no_results);
        assert_eq!(degraded.len(), 1);
        assert_eq!(degraded[0].code, "embed_model_unavailable");
        Ok(())
    }

    #[test]
    fn strict_semantic_only_errors_when_hash_fallback_is_active() -> TestResult {
        let options = source_mode_test_options(SearchSourceMode::SemanticOnly, true);
        let mut degraded = Vec::new();

        let error = match resolve_source_mode_with_tiers(
            &options,
            &mut degraded,
            SearchTierState {
                lexical_available: true,
                embed_model_unavailable: None,
                semantic_embedder_pending: None,
                semantic_embedder_degraded: Some(HASH_FALLBACK_SEMANTIC_UNAVAILABLE_REASON),
            },
        ) {
            Ok(resolution) => {
                return Err(format!(
                    "strict semantic-only mode should fail when hash fallback is active: {resolution:?}"
                ));
            }
            Err(error) => error,
        };

        match error {
            SearchError::SourceModeUnavailable { requested, reason } => {
                assert_eq!(requested, SearchSourceMode::SemanticOnly);
                assert!(reason.contains("semantic similarity unavailable"));
                assert!(reason.contains("frankensearch_hash_fallback"));
            }
            other => return Err(format!("unexpected source mode error: {other}")),
        }
        assert!(degraded.is_empty());
        Ok(())
    }

    #[test]
    fn pending_auto_download_keeps_semantic_source_mode_enabled() -> TestResult {
        let options = source_mode_test_options(SearchSourceMode::Hybrid, false);
        let mut degraded = Vec::new();
        let resolution = resolve_source_mode_with_tiers(
            &options,
            &mut degraded,
            SearchTierState {
                lexical_available: true,
                embed_model_unavailable: None,
                semantic_embedder_pending: Some("pending first-use download"),
                semantic_embedder_degraded: None,
            },
        )
        .map_err(|error| error.to_string())?;

        assert_eq!(resolution.applied, SearchSourceMode::Hybrid);
        assert!(!resolution.fallback_applied);
        assert!(!resolution.unavailable_no_results);
        assert!(degraded.is_empty());
        Ok(())
    }

    #[test]
    fn real_semantic_embedder_posture_does_not_emit_hash_fallback_warning() -> TestResult {
        let options = source_mode_test_options(SearchSourceMode::Hybrid, false);
        let mut degraded = Vec::new();
        let resolution = resolve_source_mode_with_tiers(
            &options,
            &mut degraded,
            SearchTierState {
                lexical_available: true,
                embed_model_unavailable: None,
                semantic_embedder_pending: None,
                semantic_embedder_degraded: None,
            },
        )
        .map_err(|error| error.to_string())?;

        assert_eq!(resolution.applied, SearchSourceMode::Hybrid);
        assert!(degraded.is_empty());
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
        assert_eq!(recovery[1]["command"], "cargo build --features embed-fast");
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
            index_freshness: None,
            status: SearchStatus::Success,
            embed_backend: EmbedBackend::HashFallback,
            query: "cargo fmt".to_string(),
            requested_limit: 10,
            results: visible,
            elapsed_ms: 1.0,
            errors: Vec::new(),
            degraded,
            runtime_profile: test_runtime_profile(),
            rerank_configured_mode: crate::config::SearchRerankMode::Auto,
            rerank_configured_top_k: DEFAULT_SEARCH_RERANK_TOP_K,
            rerank_runtime_available: false,
            relevance_floor_applied: None,
            candidates_below_floor: 0,
            query_assist: None,
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
            index_freshness: None,
            status: SearchStatus::Success,
            embed_backend: EmbedBackend::HashFallback,
            query: "stale provenance".to_string(),
            requested_limit: 10,
            results: visible,
            elapsed_ms: 1.0,
            errors: Vec::new(),
            degraded,
            runtime_profile: test_runtime_profile(),
            rerank_configured_mode: crate::config::SearchRerankMode::Auto,
            rerank_configured_top_k: DEFAULT_SEARCH_RERANK_TOP_K,
            rerank_runtime_available: false,
            relevance_floor_applied: None,
            candidates_below_floor: 0,
            query_assist: None,
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
            index_freshness: None,
            status: SearchStatus::Success,
            embed_backend: EmbedBackend::HashFallback,
            query: "provenance drift".to_string(),
            requested_limit: 10,
            results: visible,
            elapsed_ms: 1.0,
            errors: Vec::new(),
            degraded,
            runtime_profile: test_runtime_profile(),
            rerank_configured_mode: crate::config::SearchRerankMode::Auto,
            rerank_configured_top_k: DEFAULT_SEARCH_RERANK_TOP_K,
            rerank_runtime_available: false,
            relevance_floor_applied: None,
            candidates_below_floor: 0,
            query_assist: None,
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
            index_freshness: None,
            status: SearchStatus::Success,
            embed_backend: EmbedBackend::HashFallback,
            query: "stale validity".to_string(),
            requested_limit: 10,
            results: visible,
            elapsed_ms: 1.0,
            errors: Vec::new(),
            degraded,
            runtime_profile: test_runtime_profile(),
            rerank_configured_mode: crate::config::SearchRerankMode::Auto,
            rerank_configured_top_k: DEFAULT_SEARCH_RERANK_TOP_K,
            rerank_runtime_available: false,
            relevance_floor_applied: None,
            candidates_below_floor: 0,
            query_assist: None,
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
            index_freshness: None,
            status: SearchStatus::Success,
            embed_backend: EmbedBackend::HashFallback,
            query: "stale validity window".to_string(),
            requested_limit: 10,
            results: visible,
            elapsed_ms: 1.0,
            errors: Vec::new(),
            degraded,
            runtime_profile: test_runtime_profile(),
            rerank_configured_mode: crate::config::SearchRerankMode::Auto,
            rerank_configured_top_k: DEFAULT_SEARCH_RERANK_TOP_K,
            rerank_runtime_available: false,
            relevance_floor_applied: None,
            candidates_below_floor: 0,
            query_assist: None,
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
        const SECRET_QUERY: &str = "rotate secret sk_live_do_not_emit";
        let report = SearchReport {
            index_freshness: None,
            status: SearchStatus::Success,
            embed_backend: EmbedBackend::HashFallback,
            query: SECRET_QUERY.to_string(),
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
            degraded: vec![
                SearchDegradation::stale_index(Some(12), Some(9)),
                SearchDegradation::no_relevant_results(0.95, 1, Some(0.42)),
            ],
            runtime_profile: test_runtime_profile(),
            rerank_configured_mode: crate::config::SearchRerankMode::Auto,
            rerank_configured_top_k: DEFAULT_SEARCH_RERANK_TOP_K,
            rerank_runtime_available: false,
            relevance_floor_applied: None,
            candidates_below_floor: 0,
            query_assist: None,
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
        assert!(
            json["data"]["fallbacks"]
                .as_array()
                .is_some_and(|fallbacks| {
                    let codes = fallbacks
                        .iter()
                        .filter_map(|fallback| fallback["code"].as_str())
                        .collect::<Vec<_>>();
                    codes.contains(&"no_relevant_results")
                        && codes.contains(&"search_index_stale")
                        && fallbacks.iter().all(|fallback| {
                            fallback["message"] == PERFORMANCE_FALLBACK_REDACTED_MESSAGE
                                && fallback.get("repair").is_none()
                                && fallback.as_object().is_some_and(|object| object.len() == 4)
                        })
                })
        );
        assert_eq!(json["data"]["redaction"]["memoryContentIncluded"], false);
        assert_eq!(json["data"]["redaction"]["queryTextIncluded"], false);
        assert!(!rendered.contains(SECRET_QUERY));
        assert!(!rendered.contains("sk_live_do_not_emit"));
        assert!(!rendered.contains("mem-secret-doc"));
        assert!(!rendered.contains("token should not leave"));
    }

    #[test]
    fn search_report_degraded_entries_are_aggregated() {
        let report = SearchReport {
            index_freshness: None,
            status: SearchStatus::Success,
            embed_backend: EmbedBackend::HashFallback,
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
            rerank_configured_mode: crate::config::SearchRerankMode::Auto,
            rerank_configured_top_k: DEFAULT_SEARCH_RERANK_TOP_K,
            rerank_runtime_available: false,
            relevance_floor_applied: None,
            candidates_below_floor: 0,
            query_assist: None,
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
    fn stale_index_large_gap_boundary_is_stable() {
        assert!(!search_index_gap_is_large(
            SEARCH_INDEX_LARGE_GAP_THRESHOLD - 1,
            0
        ));
        assert!(!search_index_gap_is_large(
            SEARCH_INDEX_LARGE_GAP_THRESHOLD,
            0
        ));
        assert!(search_index_gap_is_large(
            SEARCH_INDEX_LARGE_GAP_THRESHOLD + 1,
            0
        ));
        assert!(!search_index_gap_is_auto_reconcilable(0, 0));
        assert!(search_index_gap_is_auto_reconcilable(1, 0));
        assert!(search_index_gap_is_auto_reconcilable(
            SEARCH_INDEX_LARGE_GAP_THRESHOLD,
            0
        ));
        assert!(!search_index_gap_is_auto_reconcilable(
            SEARCH_INDEX_LARGE_GAP_THRESHOLD + 1,
            0
        ));
        assert!(search_index_corpus_is_auto_reconcilable(
            SEARCH_INDEX_AUTO_RECONCILE_MAX_DOCUMENTS
        ));
        assert!(!search_index_corpus_is_auto_reconcilable(
            SEARCH_INDEX_AUTO_RECONCILE_MAX_DOCUMENTS + 1
        ));

        let above = SearchDegradation::large_index_gap(SEARCH_INDEX_LARGE_GAP_THRESHOLD + 1, 0);
        assert_eq!(above.code, "search_index_large_gap");
        assert!(above.message.contains(&format!(
            "above the automatic reconciliation limit of {SEARCH_INDEX_LARGE_GAP_THRESHOLD}"
        )));
        assert_eq!(
            above.repair.as_deref(),
            Some("ee index rebuild --workspace .")
        );
    }

    #[test]
    fn live_large_gap_search_emits_repair_once_without_hiding_stale_truth() -> TestResult {
        let workspace = unique_test_dir("live-large-index-gap");
        std::fs::create_dir_all(&workspace).map_err(|error| error.to_string())?;
        let database_path = workspace.join("ee.db");
        let index_dir = workspace.join("index");
        let workspace_id = "wsp_live_large_gap_00000000000";
        let seed_id = "mem_live_large_gap_seed_000000";
        let seed_content = "Live large-gap seed remains in the old lexical index.";

        let connection =
            DbConnection::open_file(&database_path).map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        connection
            .insert_workspace(
                workspace_id,
                &CreateWorkspaceInput {
                    path: workspace.display().to_string(),
                    name: Some("live large index gap".to_owned()),
                },
            )
            .map_err(|error| error.to_string())?;
        connection
            .insert_memory(seed_id, &test_memory_input(workspace_id, seed_content))
            .map_err(|error| error.to_string())?;
        connection.close().map_err(|error| error.to_string())?;

        let build_index_dir = index_dir.clone();
        let seed_id_for_index = seed_id.to_owned();
        let seed_content_for_index = seed_content.to_owned();
        crate::core::run_cli_future(async move {
            let cx = asupersync::Cx::for_testing();
            let stack = EmbedderStack::from_parts(
                Arc::new(HashEmbedder::default_256()) as Arc<dyn Embedder>,
                None,
            );
            let documents = vec![IndexableDocument::new(
                seed_id_for_index,
                seed_content_for_index,
            )];
            let lexical_documents = documents.clone();
            IndexBuilder::new(&build_index_dir)
                .with_embedder_stack(stack)
                .add_documents(documents)
                .build(&cx)
                .await
                .map_err(|error| error.to_string())?;
            crate::core::index::build_lexical_tier(&cx, &build_index_dir, &lexical_documents)
                .await
                .map_err(|error| error.to_string())?;
            Ok::<(), String>(())
        })
        .map_err(|error| error.to_string())??;
        write_current_index_metadata(&index_dir, 1)?;

        let connection =
            DbConnection::open_file(&database_path).map_err(|error| error.to_string())?;
        for offset in 0..=SEARCH_INDEX_LARGE_GAP_THRESHOLD {
            let memory_id = format!("mem_live_large_gap_{offset:011}");
            let content = format!("Unindexed live large-gap memory number {offset}.");
            connection
                .insert_memory(&memory_id, &test_memory_input(workspace_id, &content))
                .map_err(|error| error.to_string())?;
        }
        connection.close().map_err(|error| error.to_string())?;

        let status =
            crate::core::index::get_index_status(&crate::core::index::IndexStatusOptions {
                workspace_path: workspace.clone(),
                database_path: Some(database_path.clone()),
                index_dir: Some(index_dir.clone()),
            })
            .map_err(|error| error.to_string())?;
        let (db_generation, index_generation) =
            status
                .db_generation
                .zip(status.index_generation)
                .ok_or_else(|| format!("large-gap status omitted generations: {status:?}"))?;
        ensure(
            db_generation.saturating_sub(index_generation) > SEARCH_INDEX_LARGE_GAP_THRESHOLD,
            format!(
                "fixture must create a real generation gap above {}: db={db_generation}, index={index_generation}",
                SEARCH_INDEX_LARGE_GAP_THRESHOLD
            ),
        )?;

        let report = run_search(&SearchOptions {
            workspace_path: workspace,
            database_path: Some(database_path),
            index_dir: Some(index_dir),
            query: "live large-gap seed".to_owned(),
            limit: 10,
            speed: SpeedMode::Instant,
            explain: false,
            as_of: None,
            include_tombstoned: false,
            include_expired: false,
            include_future: false,
            include_stale: false,
            relevance_floor: Some(0.0),
            dedup_mode: SearchDedupMode::DocId,
            source_mode: SearchSourceMode::LexicalOnly,
            strict_source_mode: true,
            memory_scope: MemoryScope::Workspace,
            strict_scope: false,
        })
        .map_err(|error| error.to_string())?;
        ensure(
            report.results.iter().any(|hit| hit.doc_id == seed_id),
            "the real stale-index search must still execute against the old lexical index",
        )?;

        let mut session = SearchAdvisorySession::default();
        let first = report.data_json_with_advisory_session(&mut session);
        let repeated = report.data_json_with_advisory_session(&mut session);
        let first_codes = first["degraded"]
            .as_array()
            .ok_or_else(|| format!("first large-gap response omitted degraded[]: {first}"))?
            .iter()
            .filter_map(|entry| entry["code"].as_str())
            .collect::<Vec<_>>();
        ensure(
            first_codes.contains(&"search_index_stale")
                && first_codes.contains(&"search_index_large_gap"),
            format!("first real large-gap response omitted required truth: {first}"),
        )?;
        ensure(
            first["degraded"].as_array().is_some_and(|entries| {
                entries.iter().any(|entry| {
                    entry["code"] == "search_index_large_gap"
                        && entry["repair"] == "ee index rebuild --workspace ."
                })
            }),
            format!("large-gap response omitted explicit rebuild repair: {first}"),
        )?;
        ensure(
            repeated["degraded"] == serde_json::json!([]),
            format!("repeat rendering must suppress both warning prose entries: {repeated}"),
        )?;
        ensure(
            repeated["indexFreshness"]
                == serde_json::json!({
                    "stale": true,
                    "dbGeneration": db_generation,
                    "indexGeneration": index_generation,
                    "generationGap": db_generation.saturating_sub(index_generation),
                    "largeGap": true,
                }),
            format!("repeat rendering lost structured freshness truth: {repeated}"),
        )
    }

    #[test]
    fn search_data_json_redacts_public_content_metadata() {
        let raw_value = concat!("sk", "_", "search", "_", "secret", "_", "123");
        let report = SearchReport {
            index_freshness: None,
            status: SearchStatus::Success,
            embed_backend: EmbedBackend::HashFallback,
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
            rerank_configured_mode: crate::config::SearchRerankMode::Auto,
            rerank_configured_top_k: DEFAULT_SEARCH_RERANK_TOP_K,
            rerank_runtime_available: false,
            relevance_floor_applied: None,
            candidates_below_floor: 0,
            query_assist: None,
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
            index_freshness: None,
            status: SearchStatus::Success,
            embed_backend: EmbedBackend::HashFallback,
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
            rerank_configured_mode: crate::config::SearchRerankMode::Auto,
            rerank_configured_top_k: DEFAULT_SEARCH_RERANK_TOP_K,
            rerank_runtime_available: false,
            relevance_floor_applied: None,
            candidates_below_floor: 0,
            query_assist: None,
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
            index_freshness: None,
            status: SearchStatus::Success,
            embed_backend: EmbedBackend::HashFallback,
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
            rerank_configured_mode: crate::config::SearchRerankMode::Auto,
            rerank_configured_top_k: DEFAULT_SEARCH_RERANK_TOP_K,
            rerank_runtime_available: false,
            relevance_floor_applied: None,
            candidates_below_floor: 0,
            query_assist: None,
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
            index_freshness: None,
            status: SearchStatus::Success,
            embed_backend: EmbedBackend::HashFallback,
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
            rerank_configured_mode: crate::config::SearchRerankMode::Auto,
            rerank_configured_top_k: DEFAULT_SEARCH_RERANK_TOP_K,
            rerank_runtime_available: false,
            relevance_floor_applied: None,
            candidates_below_floor: 0,
            query_assist: None,
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

        let (degraded, _freshness) = search_degradations(&options, &index_dir);

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

        let (degraded, _freshness) = search_degradations(&options, &index_dir);

        assert_eq!(degraded.len(), 1);
        assert_eq!(degraded[0].code, "index_corrupt");
        assert_eq!(degraded[0].severity, "high");
        assert!(degraded[0].message.contains("Last check error"));
        assert!(degraded[0].message.contains("meta.json"));
        Ok(())
    }

    #[test]
    fn search_refuses_pre_security_epoch_index_before_open() -> TestResult {
        let workspace = unique_test_dir("pre-security-epoch-index");
        let index_dir = workspace.join("index");
        std::fs::create_dir_all(&index_dir).map_err(|error| error.to_string())?;
        std::fs::write(
            index_dir.join("meta.json"),
            r#"{"schema":"ee.index_metadata.v1","generation":0,"sourceGeneration":0}"#,
        )
        .map_err(|error| error.to_string())?;
        let options = SearchOptions {
            workspace_path: workspace.clone(),
            database_path: Some(workspace.join("missing.db")),
            index_dir: Some(index_dir),
            query: "must not open stale evidence".to_owned(),
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

        assert!(
            matches!(run_search(&options), Err(SearchError::NoIndex)),
            "pre-security-epoch index must fail closed before retrieval"
        );
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

        let (degraded, _freshness) = search_degradations(&options, &index_dir);
        assert_eq!(degraded.len(), 1);
        assert_eq!(degraded[0].code, "index_missing");

        std::fs::write(index_dir.join("meta.json"), "{ not-json")
            .map_err(|error| error.to_string())?;
        let (cached_degraded, _freshness) = search_degradations(&options, &index_dir);

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
            // Mirrors the production rebuild: frankensearch's IndexBuilder no
            // longer writes a Tantivy arm on the cass-compat lane, so ee's
            // own lexical arm runs after the vector build.
            let lexical_documents = documents.clone();
            IndexBuilder::new(&build_index_dir)
                .with_embedder_stack(stack)
                .add_documents(documents)
                .build(&cx)
                .await
                .map_err(|error| error.to_string())?;
            crate::core::index::build_lexical_tier(&cx, &build_index_dir, &lexical_documents)
                .await
                .map_err(|error| error.to_string())?;
            Ok::<(), String>(())
        })
        .map_err(|error| error.to_string())??;

        let first_lexical = open_lexical_searcher(&index_dir)?
            .ok_or_else(|| "rebuilt lexical index did not open".to_owned())?;
        let second_lexical = open_lexical_searcher(&index_dir)?
            .ok_or_else(|| "rebuilt lexical index disappeared".to_owned())?;
        assert!(
            Arc::ptr_eq(&first_lexical, &second_lexical),
            "same published generation must reuse its process-scoped lexical reader"
        );

        let first_key = LexicalSearcherCacheKey {
            index_dir: index_dir.clone(),
            index_manifest_version: index_manifest_version_for_plan_cache(&index_dir),
        };
        std::fs::write(
            index_dir.join("meta.json"),
            r#"{"schema":"ee.index_metadata.v1","generation":2,"sourceGeneration":2}"#,
        )
        .map_err(|error| error.to_string())?;
        let next_key = LexicalSearcherCacheKey {
            index_dir: index_dir.clone(),
            index_manifest_version: index_manifest_version_for_plan_cache(&index_dir),
        };
        assert_ne!(
            first_key, next_key,
            "a published-generation change must invalidate lexical reader reuse"
        );
        let stale_lexical = Arc::downgrade(&first_lexical);
        drop(first_lexical);
        drop(second_lexical);
        let refreshed_lexical = open_lexical_searcher(&index_dir)?
            .ok_or_else(|| "refreshed lexical generation did not open".to_owned())?;
        assert!(
            stale_lexical.upgrade().is_none(),
            "the cache must release the prior published generation before reopening"
        );
        drop(refreshed_lexical);

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
            None,
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
    fn live_evidence_admission_filters_stale_index_from_search_assist_and_diag() -> TestResult {
        let workspace = unique_test_dir("evidence-live-admission");
        std::fs::create_dir_all(&workspace).map_err(|error| error.to_string())?;
        let database_path = workspace.join("ee.db");
        let index_dir = workspace.join("index");
        let workspace_id = "wsp_49000000000000000000000001".to_owned();
        let session_id =
            crate::models::SessionId::from_uuid(uuid::Uuid::from_u128(0x4f_001)).to_string();
        let evidence_id = EvidenceId::from_uuid(uuid::Uuid::from_u128(0x4f_002)).to_string();
        let memory_id = "mem_40000000000000000000000001";
        let denied_phrase = "forbidden old evidence phrase visibility canary";
        let safe_phrase = "safe memory visibility canary remains available";

        let connection =
            DbConnection::open_file(&database_path).map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        connection
            .insert_workspace(
                &workspace_id,
                &CreateWorkspaceInput {
                    path: default_workspace_root(&workspace).display().to_string(),
                    name: Some("evidence-live-admission".to_owned()),
                },
            )
            .map_err(|error| error.to_string())?;
        connection
            .insert_session(
                &session_id,
                &CreateSessionInput {
                    workspace_id: workspace_id.clone(),
                    cass_session_id: "upstream-evidence-live-admission".to_owned(),
                    source_path: None,
                    agent_name: Some("codex".to_owned()),
                    model: Some("gpt-5".to_owned()),
                    started_at: Some("2026-07-28T12:00:00Z".to_owned()),
                    ended_at: Some("2026-07-28T12:01:00Z".to_owned()),
                    message_count: 1,
                    token_count: Some(8),
                    content_hash: format!(
                        "blake3:{}",
                        blake3::hash(b"evidence-live-admission-session").to_hex()
                    ),
                    metadata_json: None,
                },
            )
            .map_err(|error| error.to_string())?;
        connection
            .insert_memory(memory_id, &test_memory_input(&workspace_id, safe_phrase))
            .map_err(|error| error.to_string())?;
        connection
            .insert_evidence_span(
                &evidence_id,
                &CreateEvidenceSpanInput {
                    workspace_id: workspace_id.clone(),
                    session_id,
                    memory_id: None,
                    producer_kind: EvidenceProducerKind::CassImport,
                    cass_span_id: "upstream-evidence-live-admission-span".to_owned(),
                    span_kind: "message".to_owned(),
                    start_line: 1,
                    end_line: 1,
                    start_byte: None,
                    end_byte: None,
                    role: Some("assistant".to_owned()),
                    excerpt: denied_phrase.to_owned(),
                    content_hash: format!(
                        "blake3:{}",
                        blake3::hash(denied_phrase.as_bytes()).to_hex()
                    ),
                    metadata_json: None,
                    inherited_redaction_classes: Vec::new(),
                },
            )
            .map_err(|error| error.to_string())?;
        let indexed_generation = connection
            .get_workspace_generation(&workspace_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "evidence fixture omitted workspace generation".to_owned())?;
        // Public search commands acquire their own read-only snapshot. Close
        // the setup writer before crossing that boundary so the regression
        // observes durable storage rather than a backend-specific live
        // connection view.
        connection.close().map_err(|error| error.to_string())?;

        let build_index_dir = index_dir.clone();
        let documents = vec![
            IndexableDocument::new(evidence_id.clone(), denied_phrase),
            IndexableDocument::new(memory_id, safe_phrase),
        ];
        crate::core::run_cli_future(async move {
            let cx = asupersync::Cx::for_testing();
            let stack = EmbedderStack::from_parts(
                Arc::new(HashEmbedder::default_256()) as Arc<dyn Embedder>,
                None,
            );
            // Mirrors the production rebuild: frankensearch's IndexBuilder no
            // longer writes a Tantivy arm on the cass-compat lane, so ee's
            // own lexical arm runs after the vector build.
            let lexical_documents = documents.clone();
            IndexBuilder::new(&build_index_dir)
                .with_embedder_stack(stack)
                .add_documents(documents)
                .build(&cx)
                .await
                .map_err(|error| error.to_string())?;
            crate::core::index::build_lexical_tier(&cx, &build_index_dir, &lexical_documents)
                .await
                .map_err(|error| error.to_string())?;
            Ok::<(), String>(())
        })
        .map_err(|error| error.to_string())??;
        crate::core::index::write_memory_eval_index_metadata_for_generation(
            &index_dir,
            indexed_generation,
            2,
        )
        .map_err(|error| error.to_string())?;

        let initially_admitted = run_search(&SearchOptions {
            workspace_path: workspace.clone(),
            database_path: Some(database_path.clone()),
            index_dir: Some(index_dir.clone()),
            query: "forbidden old evidence phrase".to_owned(),
            limit: 10,
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
        assert!(
            initially_admitted
                .results
                .iter()
                .any(|hit| hit.doc_id == evidence_id),
            "live admission must resolve the actual canonical workspace row rather than synthesize its id"
        );

        let mutation_connection =
            DbConnection::open_file(&database_path).map_err(|error| error.to_string())?;
        mutation_connection
            .execute_raw(&format!(
                "UPDATE evidence_spans SET search_eligibility = 'denied' WHERE id = '{}'",
                evidence_id
            ))
            .map_err(|error| error.to_string())?;
        // The stale-index checks below are public command invocations, each
        // with an independent read snapshot. Make the denial durable before
        // any of them revalidates the indexed evidence row.
        mutation_connection
            .close()
            .map_err(|error| error.to_string())?;

        let base_options = SearchOptions {
            workspace_path: workspace.clone(),
            database_path: Some(database_path),
            index_dir: Some(index_dir),
            query: "visibility canary".to_owned(),
            limit: 10,
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
        };

        let search = run_search(&base_options).map_err(|error| error.to_string())?;
        assert!(
            search.results.iter().all(|hit| hit.doc_id != evidence_id),
            "live-denied evidence must not survive normal search"
        );
        assert!(
            search.results.iter().any(|hit| hit.doc_id == memory_id),
            "non-evidence hits remain usable"
        );
        assert!(
            search
                .degraded
                .iter()
                .any(|entry| entry.code == "evidence_live_admission_filtered")
        );

        let assist = run_search(&SearchOptions {
            relevance_floor: Some(f32::MAX),
            ..base_options.clone()
        })
        .map_err(|error| error.to_string())?;
        let assist_json = assist.data_json().to_string();
        assert!(!assist_json.contains(&evidence_id));
        assert!(!assist_json.contains(denied_phrase));

        let diag = run_diag_search(&base_options).map_err(|error| error.to_string())?;
        let diag_json = diag.data_json().to_string();
        assert!(!diag_json.contains(&evidence_id));
        assert!(!diag_json.contains(denied_phrase));
        assert!(diag_json.contains(memory_id));
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
            // Mirrors the production rebuild: frankensearch's IndexBuilder no
            // longer writes a Tantivy arm on the cass-compat lane, so ee's
            // own lexical arm runs after the vector build.
            let lexical_documents = documents.clone();
            IndexBuilder::new(&build_index_dir)
                .with_embedder_stack(stack)
                .add_documents(documents)
                .build(&cx)
                .await
                .map_err(|error| error.to_string())?;
            crate::core::index::build_lexical_tier(&cx, &build_index_dir, &lexical_documents)
                .await
                .map_err(|error| error.to_string())?;
            Ok::<(), String>(())
        })
        .map_err(|error| error.to_string())??;
        write_current_index_metadata(&index_dir, 3)?;

        let hash_embedder =
            Arc::new(HashEmbedder::default_256()) as Arc<dyn crate::search::Embedder>;
        let report = run_diag_search_with_embedder(
            &SearchOptions {
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
            },
            Arc::clone(&hash_embedder),
        )
        .map_err(|error| error.to_string())?;
        let json = report.data_json();

        assert_eq!(json["schema"], DIAG_SEARCH_SCHEMA_V1);
        assert_eq!(json["command"], "diag search");
        assert_eq!(json["final"]["embed_backend"], "hash_fallback");
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
            Some(hash_embedder),
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

    #[cfg(unix)]
    #[test]
    fn search_default_paths_canonicalize_existing_workspace_root() -> TestResult {
        use std::os::unix::fs::symlink;

        let root = unique_test_dir("canonical-default-paths");
        let target = root.join("real-workspace");
        let alias = root.join("alias-workspace");
        std::fs::create_dir_all(&target).map_err(|error| error.to_string())?;
        symlink(&target, &alias).map_err(|error| error.to_string())?;

        let canonical = target.canonicalize().map_err(|error| error.to_string())?;
        let expected_database = canonical.join(".ee").join("ee.db");
        let expected_index = canonical.join(".ee").join(DEFAULT_INDEX_SUBDIR);
        let options = SearchOptions {
            workspace_path: alias,
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

        assert_eq!(options.resolve_database_path(), expected_database);
        assert_eq!(options.resolve_index_dir(), expected_index);

        let key = IndexStatusCacheKey::from_search_options(&options, &expected_index);
        assert_eq!(key.database_path, expected_database);
        assert_eq!(key.index_dir, expected_index);

        Ok(())
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
            index_freshness: None,
            status: SearchStatus::Success,
            embed_backend: EmbedBackend::HashFallback,
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
            rerank_configured_mode: crate::config::SearchRerankMode::Auto,
            rerank_configured_top_k: DEFAULT_SEARCH_RERANK_TOP_K,
            rerank_runtime_available: false,
            relevance_floor_applied: None,
            candidates_below_floor: 0,
            query_assist: None,
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
            index_freshness: None,
            status: SearchStatus::Success,
            embed_backend: EmbedBackend::HashFallback,
            query: "provenance".to_string(),
            requested_limit: 1,
            results: vec![hit],
            elapsed_ms: 1.0,
            errors: Vec::new(),
            degraded: Vec::new(),
            runtime_profile: test_runtime_profile(),
            rerank_configured_mode: crate::config::SearchRerankMode::Auto,
            rerank_configured_top_k: DEFAULT_SEARCH_RERANK_TOP_K,
            rerank_runtime_available: false,
            relevance_floor_applied: None,
            candidates_below_floor: 0,
            query_assist: None,
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
            index_freshness: None,
            status: SearchStatus::Success,
            embed_backend: EmbedBackend::HashFallback,
            query: "provenance".to_string(),
            requested_limit: 1,
            results: vec![hit],
            elapsed_ms: 1.0,
            errors: Vec::new(),
            degraded: Vec::new(),
            runtime_profile: test_runtime_profile(),
            rerank_configured_mode: crate::config::SearchRerankMode::Auto,
            rerank_configured_top_k: DEFAULT_SEARCH_RERANK_TOP_K,
            rerank_runtime_available: false,
            relevance_floor_applied: None,
            candidates_below_floor: 0,
            query_assist: None,
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
            index_freshness: None,
            status: SearchStatus::Success,
            embed_backend: EmbedBackend::HashFallback,
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
            rerank_configured_mode: crate::config::SearchRerankMode::Auto,
            rerank_configured_top_k: DEFAULT_SEARCH_RERANK_TOP_K,
            rerank_runtime_available: false,
            relevance_floor_applied: None,
            candidates_below_floor: 0,
            query_assist: None,
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
            score: RRF_HYBRID_TYPICAL_MAX * 0.9,
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
            index_freshness: None,
            status: SearchStatus::Success,
            embed_backend: EmbedBackend::HashFallback,
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
            rerank_configured_mode: crate::config::SearchRerankMode::Auto,
            rerank_configured_top_k: DEFAULT_SEARCH_RERANK_TOP_K,
            rerank_runtime_available: false,
            relevance_floor_applied: None,
            candidates_below_floor: 0,
            query_assist: None,
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
            index_freshness: None,
            status: SearchStatus::NoResults,
            embed_backend: EmbedBackend::HashFallback,
            query: "empty".to_string(),
            requested_limit: 7,
            results: Vec::new(),
            elapsed_ms: 0.0,
            errors: Vec::new(),
            degraded: Vec::new(),
            runtime_profile: test_runtime_profile(),
            rerank_configured_mode: crate::config::SearchRerankMode::Auto,
            rerank_configured_top_k: DEFAULT_SEARCH_RERANK_TOP_K,
            rerank_runtime_available: false,
            relevance_floor_applied: None,
            candidates_below_floor: 0,
            query_assist: None,
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
        assert_eq!(
            explanation.factors[0].formula,
            "score = minmax(lexical_score, returned_pool)"
        );
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
            index_freshness: None,
            status: SearchStatus::Success,
            embed_backend: EmbedBackend::HashFallback,
            query: "explained".to_string(),
            requested_limit: 1,
            results: vec![hit],
            elapsed_ms: 2.0,
            errors: Vec::new(),
            degraded: Vec::new(),
            runtime_profile: test_runtime_profile(),
            rerank_configured_mode: crate::config::SearchRerankMode::Auto,
            rerank_configured_top_k: DEFAULT_SEARCH_RERANK_TOP_K,
            rerank_runtime_available: false,
            relevance_floor_applied: None,
            candidates_below_floor: 0,
            query_assist: None,
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
            index_freshness: None,
            status: SearchStatus::Success,
            embed_backend: EmbedBackend::HashFallback,
            query: "human test".to_string(),
            requested_limit: 1,
            results: vec![hit],
            elapsed_ms: 1.5,
            errors: Vec::new(),
            degraded: Vec::new(),
            runtime_profile: test_runtime_profile(),
            rerank_configured_mode: crate::config::SearchRerankMode::Auto,
            rerank_configured_top_k: DEFAULT_SEARCH_RERANK_TOP_K,
            rerank_runtime_available: false,
            relevance_floor_applied: None,
            candidates_below_floor: 0,
            query_assist: None,
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
    fn human_summary_includes_team_attribution_suffix() {
        let mut hit = SearchHit {
            doc_id: "mem_teamhit".to_string(),
            score: 0.70,
            source: ScoreSource::Lexical,
            fast_score: None,
            quality_score: None,
            lexical_score: Some(0.70),
            rerank_score: None,
            metadata: Some(serde_json::json!({
                "teamProvenance": {
                    "schema": "ee.team.provenance.v1",
                    "memberDisplayName": "Priya",
                    "projectName": "acme-analysis",
                    "originTrustClass": "human_explicit",
                    "producedAt": "2026-07-30T14:02:00Z",
                    "originTimeAssurance": "member_attested"
                }
            })),
            explanation: None,
        };

        let report = SearchReport {
            index_freshness: None,
            status: SearchStatus::Success,
            embed_backend: EmbedBackend::HashFallback,
            query: "Acme Corp".to_string(),
            requested_limit: 1,
            results: vec![hit.clone()],
            elapsed_ms: 1.5,
            errors: Vec::new(),
            degraded: Vec::new(),
            runtime_profile: test_runtime_profile(),
            rerank_configured_mode: crate::config::SearchRerankMode::Auto,
            rerank_configured_top_k: DEFAULT_SEARCH_RERANK_TOP_K,
            rerank_runtime_available: false,
            relevance_floor_applied: None,
            candidates_below_floor: 0,
            query_assist: None,
            source_mode_requested: SearchSourceMode::Hybrid,
            source_mode_applied: SearchSourceMode::Hybrid,
            source_mode_fallback: false,
            strict_source_mode: false,
            memory_scope: MemoryScope::Team,
            strict_scope: false,
            scope_stats: test_scope_stats(),
        };

        let summary = report.human_summary();
        assert!(
            summary.contains(
                "1. mem_teamhit · from Priya / acme-analysis · 2026-07-30T14:02:00Z (relevance: 0.7000, score: 0.7000 unit_normalized, source: lexical)"
            ),
            "human search must attribute teammate hits: {summary}"
        );

        if let Some(metadata) = hit.metadata.as_mut() {
            metadata["teamProvenance"]["projectName"] = serde_json::Value::Null;
        }
        let mut unbound = report;
        unbound.results = vec![hit];
        let unbound_summary = unbound.human_summary();
        assert!(
            unbound_summary.contains(
                "1. mem_teamhit · from Priya · 2026-07-30T14:02:00Z (relevance: 0.7000, score: 0.7000 unit_normalized, source: lexical)"
            ),
            "missing project must still name the teammate: {unbound_summary}"
        );
        assert!(
            !unbound_summary.contains("acme-analysis"),
            "null projectName must not invent a project label: {unbound_summary}"
        );
    }

    #[test]
    fn search_report_delayed_render_uses_captured_embed_backend() {
        let active_backend = crate::core::index::active_embed_backend();
        let captured_backend = match active_backend {
            EmbedBackend::NeuralLocal => EmbedBackend::HashFallback,
            EmbedBackend::HashFallback => EmbedBackend::NeuralLocal,
        };
        let mut report = rerank_test_report(Vec::new(), Vec::new(), false);
        report.embed_backend = captured_backend;

        std::thread::yield_now();

        assert_ne!(
            captured_backend,
            crate::core::index::active_embed_backend(),
            "fixture must distinguish the captured backend from ambient render-time state"
        );
        assert_eq!(
            report.data_json()["embed_backend"],
            serde_json::json!(captured_backend.as_str())
        );
        assert!(
            report
                .human_summary()
                .contains(&format!("embed_backend: {captured_backend}"))
        );
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
        assert!(json.get("permanent").is_none());
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
        assert_eq!(recovery[1]["command"], "cargo build --features embed-fast");
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

    #[test]
    fn global_memory_scope_filters_hits_by_global_tags() -> TestResult {
        let workspace = tempfile::Builder::new()
            .prefix("ee-search-global-scope")
            .tempdir()
            .map_err(|error| error.to_string())?;
        let connection = DbConnection::open_memory().map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        let workspace_id = "wsp_01234567890123456789012345";
        connection
            .insert_workspace(
                workspace_id,
                &CreateWorkspaceInput {
                    path: workspace.path().display().to_string(),
                    name: Some("global-scope-search".to_owned()),
                },
            )
            .map_err(|error| error.to_string())?;

        let base_memory = CreateMemoryInput {
            workspace_id: workspace_id.to_owned(),
            level: "procedural".to_owned(),
            kind: "rule".to_owned(),
            content: "Base rule".to_owned(),
            workflow_id: None,
            confidence: 0.9,
            utility: 0.7,
            importance: 0.8,
            provenance_uri: None,
            trust_class: "agent_assertion".to_owned(),
            trust_subclass: None,
            tags: vec![],
            valid_from: None,
            valid_to: None,
        };
        connection
            .insert_memory(
                "mem_00000000000000000000000001",
                &CreateMemoryInput {
                    content: "Global rule".to_owned(),
                    tags: vec![crate::models::GLOBAL_MEMORY_SCOPE_TAG.to_owned()],
                    ..base_memory.clone()
                },
            )
            .map_err(|error| error.to_string())?;
        connection
            .insert_memory(
                "mem_00000000000000000000000002",
                &CreateMemoryInput {
                    content: "Workspace-only rule".to_owned(),
                    ..base_memory
                },
            )
            .map_err(|error| error.to_string())?;

        let mut options = source_mode_test_options(SearchSourceMode::Hybrid, false);
        options.workspace_path = workspace.path().to_path_buf();
        options.memory_scope = MemoryScope::Global;
        let hits = vec![
            synthetic_hit("mem_00000000000000000000000001", 0.9),
            synthetic_hit("mem_00000000000000000000000002", 0.8),
        ];
        let mut degraded = Vec::new();

        let (scoped, stats) = apply_memory_scope_visibility_with_metadata_mode(
            &options,
            hits.clone(),
            &mut degraded,
            Some(&connection),
            true,
        );

        assert_eq!(scoped.len(), 1);
        assert_eq!(scoped[0].doc_id, "mem_00000000000000000000000001");
        assert_eq!(
            scoped[0].metadata.as_ref().unwrap()["memory_scope"],
            "global"
        );
        assert_eq!(stats.candidates_total, 2);
        assert_eq!(stats.candidates_in_scope, 1);
        assert_eq!(stats.candidates_excluded_by_scope, 1);
        assert!(
            degraded
                .iter()
                .any(|entry| entry.code == "scope_excluded_evidence")
        );

        options.strict_scope = true;
        let floor_counts = RelevanceFloorCounts::new(hits.len(), hits.len());
        let mut strict_degraded = Vec::new();
        let (strict_scoped, strict_stats) = apply_memory_scope_visibility_with_metadata_mode(
            &options,
            hits,
            &mut strict_degraded,
            Some(&connection),
            true,
        );
        assert!(strict_scoped.is_empty());
        assert_eq!(strict_stats.strict_violations, 1);
        assert!(
            strict_degraded
                .iter()
                .any(|entry| entry.code == "scope_strict_excluded_evidence")
        );

        floor_counts.append_degradations(&mut strict_degraded, 0.05, Some(0.9));
        assert!(strict_degraded.iter().all(|entry| !matches!(
            entry.code.as_str(),
            "no_relevant_results" | "low_recall_after_floor"
        )));

        connection.close().map_err(|error| error.to_string())
    }

    #[test]
    fn typed_memory_field_filter_parse_normalizes_hyphenated_fields() -> TestResult {
        let filter = TypedMemoryFieldFilter::parse(" reverted-at-sha = 9af3c21 ")?;
        assert_eq!(filter.field, "reverted_at_sha");
        assert_eq!(filter.value, "9af3c21");
        assert_eq!(filter.operator, TypedMemoryFieldOperator::Exact);

        let contains = TypedMemoryFieldFilter::parse("command~cargo test -- --nocapture=1")?;
        assert_eq!(contains.field, "command");
        assert_eq!(contains.operator, TypedMemoryFieldOperator::Contains);
        assert_eq!(contains.value, "cargo test -- --nocapture=1");

        let prefix = TypedMemoryFieldFilter::parse("family^aggressive~prefetch=literal")?;
        assert_eq!(prefix.field, "family");
        assert_eq!(prefix.operator, TypedMemoryFieldOperator::Prefix);
        assert_eq!(prefix.value, "aggressive~prefetch=literal");

        assert_eq!(normalize_memory_kind_filter(" failure ")?, "failure");
        assert!(TypedMemoryFieldFilter::parse("Family=cache").is_err());
        assert!(TypedMemoryFieldFilter::parse("family=").is_err());
        Ok(())
    }

    #[test]
    fn typed_memory_filter_noop_preserves_evidence_results() -> TestResult {
        let connection = DbConnection::open_memory().map_err(|error| error.to_string())?;
        let evidence_id = "ev_00000000000000000000009W02";
        let memory_id = "mem_11000000000000000000000001";
        let mut report = rerank_test_report(
            vec![
                synthetic_hit(evidence_id, 0.9),
                synthetic_hit(memory_id, 0.8),
            ],
            Vec::new(),
            false,
        );

        apply_memory_kind_and_typed_field_filters_to_report_with_connection(
            &connection,
            &mut report,
            None,
            &[],
        )
        .map_err(|error| error.to_string())?;

        assert_eq!(
            report
                .results
                .iter()
                .map(|hit| hit.doc_id.as_str())
                .collect::<Vec<_>>(),
            vec![evidence_id, memory_id]
        );
        connection.close().map_err(|error| error.to_string())
    }

    #[test]
    fn typed_memory_filters_use_db_kind_and_sidecar_fields() -> TestResult {
        let workspace = tempfile::Builder::new()
            .prefix("ee-search-typed-fields")
            .tempdir()
            .map_err(|error| error.to_string())?;
        let connection = DbConnection::open_memory().map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        let workspace_id = "wsp_11234567890123456789012345";
        connection
            .insert_workspace(
                workspace_id,
                &CreateWorkspaceInput {
                    path: workspace.path().display().to_string(),
                    name: Some("typed-field-search".to_owned()),
                },
            )
            .map_err(|error| error.to_string())?;

        let base_memory = CreateMemoryInput {
            workspace_id: workspace_id.to_owned(),
            level: "episodic".to_owned(),
            kind: "failure".to_owned(),
            content: "Failure family was reverted.".to_owned(),
            workflow_id: None,
            confidence: 0.9,
            utility: 0.7,
            importance: 0.8,
            provenance_uri: None,
            trust_class: "human_explicit".to_owned(),
            trust_subclass: None,
            tags: vec![],
            valid_from: None,
            valid_to: None,
        };
        connection
            .insert_memory(
                "mem_11000000000000000000000001",
                &CreateMemoryInput {
                    content: "Aggressive prefetch regressed small-N reads.".to_owned(),
                    ..base_memory.clone()
                },
            )
            .map_err(|error| error.to_string())?;
        connection
            .set_memory_typed_fields_json(
                "mem_11000000000000000000000001",
                Some(r#"{"family":"aggressive prefetch","reverted_at_sha":"9af3c21"}"#),
            )
            .map_err(|error| error.to_string())?;
        connection
            .insert_memory(
                "mem_11000000000000000000000002",
                &CreateMemoryInput {
                    content: "Branch predictor attempt regressed tail latency.".to_owned(),
                    ..base_memory.clone()
                },
            )
            .map_err(|error| error.to_string())?;
        connection
            .set_memory_typed_fields_json(
                "mem_11000000000000000000000002",
                Some(r#"{"family":"branch predictor","reverted_at_sha":"abc1234"}"#),
            )
            .map_err(|error| error.to_string())?;
        connection
            .insert_memory(
                "mem_11000000000000000000000003",
                &CreateMemoryInput {
                    kind: "decision".to_owned(),
                    content: "Chose the simpler cache invalidation path.".to_owned(),
                    ..base_memory
                },
            )
            .map_err(|error| error.to_string())?;
        connection
            .set_memory_typed_fields_json(
                "mem_11000000000000000000000003",
                Some(r#"{"options":["local cache","RCH remote"],"chosen":"RCH remote"}"#),
            )
            .map_err(|error| error.to_string())?;

        let mut report = SearchReport {
            index_freshness: None,
            status: SearchStatus::Success,
            embed_backend: EmbedBackend::HashFallback,
            query: "prefetch regression".to_owned(),
            requested_limit: 10,
            results: vec![
                synthetic_hit("mem_11000000000000000000000001", 0.9),
                synthetic_hit("mem_11000000000000000000000002", 0.8),
                synthetic_hit("mem_11000000000000000000000003", 0.7),
            ],
            elapsed_ms: 1.0,
            errors: Vec::new(),
            degraded: Vec::new(),
            runtime_profile: test_runtime_profile(),
            rerank_configured_mode: crate::config::SearchRerankMode::Auto,
            rerank_configured_top_k: DEFAULT_SEARCH_RERANK_TOP_K,
            rerank_runtime_available: false,
            relevance_floor_applied: None,
            candidates_below_floor: 0,
            query_assist: None,
            source_mode_requested: SearchSourceMode::Hybrid,
            source_mode_applied: SearchSourceMode::Hybrid,
            source_mode_fallback: false,
            strict_source_mode: false,
            memory_scope: MemoryScope::Swarm,
            strict_scope: false,
            scope_stats: test_scope_stats(),
        };
        let filters = [TypedMemoryFieldFilter::parse("family=aggressive prefetch")?];
        apply_memory_kind_and_typed_field_filters_to_report_with_connection(
            &connection,
            &mut report,
            Some("failure"),
            &filters,
        )
        .map_err(|error| error.to_string())?;
        assert_eq!(report.status, SearchStatus::Success);
        assert_eq!(report.results.len(), 1);
        assert_eq!(report.results[0].doc_id, "mem_11000000000000000000000001");

        let mut contains_report = SearchReport {
            index_freshness: None,
            status: SearchStatus::Success,
            embed_backend: EmbedBackend::HashFallback,
            query: "contains operator".to_owned(),
            requested_limit: 10,
            results: vec![
                synthetic_hit("mem_11000000000000000000000001", 0.9),
                synthetic_hit("mem_11000000000000000000000002", 0.8),
            ],
            elapsed_ms: 1.0,
            errors: Vec::new(),
            degraded: Vec::new(),
            runtime_profile: test_runtime_profile(),
            rerank_configured_mode: crate::config::SearchRerankMode::Auto,
            rerank_configured_top_k: DEFAULT_SEARCH_RERANK_TOP_K,
            rerank_runtime_available: false,
            relevance_floor_applied: None,
            candidates_below_floor: 0,
            query_assist: None,
            source_mode_requested: SearchSourceMode::Hybrid,
            source_mode_applied: SearchSourceMode::Hybrid,
            source_mode_fallback: false,
            strict_source_mode: false,
            memory_scope: MemoryScope::Swarm,
            strict_scope: false,
            scope_stats: test_scope_stats(),
        };
        let filters = [TypedMemoryFieldFilter::parse("family~prefetch")?];
        apply_memory_kind_and_typed_field_filters_to_report_with_connection(
            &connection,
            &mut contains_report,
            Some("failure"),
            &filters,
        )
        .map_err(|error| error.to_string())?;
        assert_eq!(contains_report.results.len(), 1);
        assert_eq!(
            contains_report.results[0].doc_id,
            "mem_11000000000000000000000001"
        );

        let mut prefix_report = SearchReport {
            index_freshness: None,
            status: SearchStatus::Success,
            embed_backend: EmbedBackend::HashFallback,
            query: "prefix operator".to_owned(),
            requested_limit: 10,
            results: vec![
                synthetic_hit("mem_11000000000000000000000001", 0.9),
                synthetic_hit("mem_11000000000000000000000002", 0.8),
            ],
            elapsed_ms: 1.0,
            errors: Vec::new(),
            degraded: Vec::new(),
            runtime_profile: test_runtime_profile(),
            rerank_configured_mode: crate::config::SearchRerankMode::Auto,
            rerank_configured_top_k: DEFAULT_SEARCH_RERANK_TOP_K,
            rerank_runtime_available: false,
            relevance_floor_applied: None,
            candidates_below_floor: 0,
            query_assist: None,
            source_mode_requested: SearchSourceMode::Hybrid,
            source_mode_applied: SearchSourceMode::Hybrid,
            source_mode_fallback: false,
            strict_source_mode: false,
            memory_scope: MemoryScope::Swarm,
            strict_scope: false,
            scope_stats: test_scope_stats(),
        };
        let filters = [TypedMemoryFieldFilter::parse("family^branch")?];
        apply_memory_kind_and_typed_field_filters_to_report_with_connection(
            &connection,
            &mut prefix_report,
            Some("failure"),
            &filters,
        )
        .map_err(|error| error.to_string())?;
        assert_eq!(prefix_report.results.len(), 1);
        assert_eq!(
            prefix_report.results[0].doc_id,
            "mem_11000000000000000000000002"
        );

        let mut empty_report = SearchReport {
            index_freshness: None,
            status: SearchStatus::Success,
            embed_backend: EmbedBackend::HashFallback,
            query: "missing family".to_owned(),
            requested_limit: 10,
            results: vec![synthetic_hit("mem_11000000000000000000000001", 0.9)],
            elapsed_ms: 1.0,
            errors: Vec::new(),
            degraded: Vec::new(),
            runtime_profile: test_runtime_profile(),
            rerank_configured_mode: crate::config::SearchRerankMode::Auto,
            rerank_configured_top_k: DEFAULT_SEARCH_RERANK_TOP_K,
            rerank_runtime_available: false,
            relevance_floor_applied: None,
            candidates_below_floor: 0,
            query_assist: None,
            source_mode_requested: SearchSourceMode::Hybrid,
            source_mode_applied: SearchSourceMode::Hybrid,
            source_mode_fallback: false,
            strict_source_mode: false,
            memory_scope: MemoryScope::Swarm,
            strict_scope: false,
            scope_stats: test_scope_stats(),
        };
        let filters = [TypedMemoryFieldFilter::parse("family=missing")?];
        apply_memory_kind_and_typed_field_filters_to_report_with_connection(
            &connection,
            &mut empty_report,
            Some("failure"),
            &filters,
        )
        .map_err(|error| error.to_string())?;
        assert_eq!(empty_report.status, SearchStatus::NoResults);
        assert!(empty_report.results.is_empty());

        let mut list_value_report = SearchReport {
            index_freshness: None,
            status: SearchStatus::Success,
            embed_backend: EmbedBackend::HashFallback,
            query: "decision options".to_owned(),
            requested_limit: 10,
            results: vec![
                synthetic_hit("mem_11000000000000000000000001", 0.9),
                synthetic_hit("mem_11000000000000000000000003", 0.8),
            ],
            elapsed_ms: 1.0,
            errors: Vec::new(),
            degraded: Vec::new(),
            runtime_profile: test_runtime_profile(),
            rerank_configured_mode: crate::config::SearchRerankMode::Auto,
            rerank_configured_top_k: DEFAULT_SEARCH_RERANK_TOP_K,
            rerank_runtime_available: false,
            relevance_floor_applied: None,
            candidates_below_floor: 0,
            query_assist: None,
            source_mode_requested: SearchSourceMode::Hybrid,
            source_mode_applied: SearchSourceMode::Hybrid,
            source_mode_fallback: false,
            strict_source_mode: false,
            memory_scope: MemoryScope::Swarm,
            strict_scope: false,
            scope_stats: test_scope_stats(),
        };
        let filters = [TypedMemoryFieldFilter::parse("options=RCH remote")?];
        apply_memory_kind_and_typed_field_filters_to_report_with_connection(
            &connection,
            &mut list_value_report,
            Some("decision"),
            &filters,
        )
        .map_err(|error| error.to_string())?;
        assert_eq!(list_value_report.status, SearchStatus::Success);
        assert_eq!(list_value_report.results.len(), 1);
        assert_eq!(
            list_value_report.results[0].doc_id,
            "mem_11000000000000000000000003"
        );

        connection.close().map_err(|error| error.to_string())
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
    fn search_hit_sort_compares_mixed_score_scales_by_relevance() {
        let lexical = SearchHit {
            doc_id: "mem_lexical".to_owned(),
            score: 0.40,
            source: ScoreSource::Lexical,
            fast_score: None,
            quality_score: None,
            lexical_score: Some(8.25),
            rerank_score: None,
            metadata: None,
            explanation: None,
        };
        let hybrid = SearchHit {
            doc_id: "mem_hybrid".to_owned(),
            score: RRF_HYBRID_TYPICAL_MAX * 0.80,
            source: ScoreSource::Hybrid,
            fast_score: Some(0.72),
            quality_score: None,
            lexical_score: Some(5.50),
            rerank_score: None,
            metadata: None,
            explanation: None,
        };
        let mut hits = vec![lexical, hybrid];

        sort_search_hits_by_score_order(&mut hits);

        assert_eq!(hits[0].doc_id, "mem_hybrid");
        assert!((hits[0].relevance_score() - 0.80).abs() < 1e-6);
        assert_eq!(hits[1].doc_id, "mem_lexical");
        assert!((hits[1].relevance_score() - 0.40).abs() < 1e-6);
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

    #[test]
    fn normalized_relevance_score_rescales_rrf_and_tags_kind() {
        // bd-1et0v.11: a top hybrid hit scores at the RRF-fused magnitude
        // (~0.0328), which an agent misreads as "no match". The normalized
        // relevanceScore must rescale that to ~1.0 and tag it `rrf_fused`,
        // while unit-normalized sources pass through unchanged and tag
        // `unit_normalized`.
        let top_hybrid = normalized_relevance_score(ScoreSource::Hybrid, RRF_HYBRID_TYPICAL_MAX);
        assert!(
            (top_hybrid - 1.0).abs() < 1e-6,
            "top hybrid RRF score should normalize to ~1.0, got {top_hybrid}",
        );
        assert_eq!(ScoreSource::Hybrid.score_kind(), "rrf_fused");

        // Hybrid magnitudes above the reference (3+ contributing arms) clamp.
        assert!((normalized_relevance_score(ScoreSource::Hybrid, 0.06) - 1.0).abs() < f32::EPSILON);

        assert_eq!(ScoreSource::Lexical.score_kind(), "unit_normalized");
        assert_eq!(ScoreSource::SemanticFast.score_kind(), "cosine_similarity");
        assert_eq!(
            ScoreSource::SemanticQuality.score_kind(),
            "cosine_similarity"
        );
        assert_eq!(ScoreSource::HashControl.score_kind(), "cosine_similarity");
        assert!(
            (normalized_relevance_score(ScoreSource::SemanticFast, 0.42) - 0.42).abs()
                < f32::EPSILON
        );
        assert_eq!(
            normalized_relevance_score(ScoreSource::SemanticFast, -0.3),
            0.0
        );
        assert_eq!(ScoreSource::Reranked.score_kind(), "reranked");
        assert!(
            (normalized_relevance_score(ScoreSource::Reranked, 0.42) - 0.42).abs() < f32::EPSILON
        );
    }

    #[test]
    fn lexical_batch_normalizes_raw_bm25_without_saturating_distinct_hits() {
        let results = [9.0_f32, 5.0, 2.0]
            .into_iter()
            .enumerate()
            .map(|(index, score)| crate::search::ScoredResult {
                doc_id: format!("mem_bm25_{index}").into(),
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
            .collect();
        let hits = search_hits_from_scored_results(
            results,
            true,
            FrankensearchFinalScoreScale::Native,
            SearchFusionWeights::default(),
        );

        assert_eq!(hits.len(), 3);
        assert_eq!(hits[0].score, 1.0);
        assert!((hits[1].score - (3.0 / 7.0)).abs() < 1e-6);
        assert_eq!(hits[2].score, 0.0);
        assert_eq!(hits[0].lexical_score, Some(9.0));
        assert_eq!(hits[1].lexical_score, Some(5.0));
        assert_eq!(hits[2].lexical_score, Some(2.0));
        assert_eq!(hits[0].score_kind(), "unit_normalized");
        assert_eq!(hits[0].relevance_score(), 1.0);
        assert!(hits[0].relevance_score() > hits[1].relevance_score());
        assert!(hits[1].relevance_score() > hits[2].relevance_score());
        assert!(
            hits[0]
                .explanation
                .as_ref()
                .is_some_and(|explanation| explanation.summary.contains("Relevance 1.0000"))
        );
    }

    #[test]
    fn lexical_batch_maps_a_degenerate_bm25_pool_to_midpoint() {
        let results = [4.25_f32, 4.25]
            .into_iter()
            .enumerate()
            .map(|(index, score)| crate::search::ScoredResult {
                doc_id: format!("mem_bm25_tie_{index}").into(),
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
            .collect();
        let hits = search_hits_from_scored_results(
            results,
            false,
            FrankensearchFinalScoreScale::Native,
            SearchFusionWeights::default(),
        );

        assert!(hits.iter().all(|hit| (hit.score - 0.5).abs() < 1e-6));
        assert!(hits.iter().all(|hit| hit.lexical_score == Some(4.25)));
    }

    #[test]
    fn hybrid_single_arm_hit_is_tagged_by_rrf_score_scale() {
        let raw_rrf_score = 1.0 / 61.0;
        let hits = search_hits_from_scored_results(
            vec![crate::search::ScoredResult {
                doc_id: "mem_hybrid_single_arm".into(),
                score: raw_rrf_score,
                source: crate::search::ScoreSource::Lexical,
                index: None,
                fast_score: None,
                quality_score: None,
                lexical_score: Some(7.5),
                rerank_score: None,
                explanation: None,
                metadata: None,
            }],
            false,
            FrankensearchFinalScoreScale::RrfFused,
            SearchFusionWeights::default(),
        );

        assert_eq!(hits[0].source, ScoreSource::Hybrid);
        assert_eq!(hits[0].score_kind(), "rrf_fused");
        assert!((hits[0].score - raw_rrf_score).abs() < f32::EPSILON);
        assert!((hits[0].relevance_score() - 0.5).abs() < 1e-6);
        assert_eq!(hits[0].lexical_score, Some(7.5));
    }

    #[test]
    fn hybrid_lexical_short_circuit_keeps_native_scale_then_normalizes_bm25() {
        let result = crate::search::ScoredResult {
            doc_id: "mem_hybrid_lexical_short_circuit".into(),
            score: 4.25,
            source: crate::search::ScoreSource::Lexical,
            index: None,
            fast_score: None,
            quality_score: None,
            lexical_score: Some(4.25),
            rerank_score: None,
            explanation: None,
            metadata: None,
        };
        let metrics = frankensearch::TwoTierMetrics {
            skip_reason: Some("non_semantic_fast_embedder_lexical_short_circuit".to_owned()),
            ..frankensearch::TwoTierMetrics::default()
        };

        let scale = two_tier_final_score_scale(
            SearchSourceMode::Hybrid,
            std::slice::from_ref(&result),
            &metrics,
        );
        let hits = search_hits_from_scored_results(
            vec![result],
            false,
            scale,
            SearchFusionWeights::default(),
        );

        assert_eq!(scale, FrankensearchFinalScoreScale::Native);
        assert_eq!(hits[0].source, ScoreSource::Lexical);
        assert_eq!(hits[0].score_kind(), "unit_normalized");
        assert!((hits[0].score - 0.5).abs() < 1e-6);
        assert_eq!(hits[0].lexical_score, Some(4.25));
    }

    #[test]
    fn hybrid_execution_metrics_identify_single_arm_rrf_scale() {
        let result = crate::search::ScoredResult {
            doc_id: "mem_hybrid_single_arm_metrics".into(),
            score: 1.0 / 61.0,
            source: crate::search::ScoreSource::Lexical,
            index: None,
            fast_score: None,
            quality_score: None,
            lexical_score: Some(4.25),
            rerank_score: None,
            explanation: None,
            metadata: None,
        };
        let metrics = frankensearch::TwoTierMetrics {
            rrf_fusion_ms: 0.01,
            ..frankensearch::TwoTierMetrics::default()
        };

        assert_eq!(
            two_tier_final_score_scale(
                SearchSourceMode::Hybrid,
                std::slice::from_ref(&result),
                &metrics,
            ),
            FrankensearchFinalScoreScale::RrfFused
        );
    }

    #[test]
    fn reranked_adapter_promotes_final_score_before_default_floor() {
        // bd-1zltr: Frankensearch preserves the pre-rerank RRF value in
        // `ScoredResult::score`. That value is below EE's unit-domain rerank
        // floor even when the cross-encoder returned a strong match.
        let fusion_score = RRF_HYBRID_TYPICAL_MAX;
        let rerank_score = 0.91;
        assert!(fusion_score < DEFAULT_RELEVANCE_FLOOR);

        let hit = search_hit_from_scored_result(
            crate::search::ScoredResult {
                doc_id: "mem_reranked_score_domain".into(),
                score: fusion_score,
                source: crate::search::ScoreSource::Reranked,
                index: None,
                fast_score: Some(0.70),
                quality_score: None,
                lexical_score: Some(0.60),
                rerank_score: Some(rerank_score),
                explanation: None,
                metadata: None,
            },
            true,
            FrankensearchFinalScoreScale::RrfFused,
            SearchFusionWeights::default(),
        );

        assert_eq!(hit.source, ScoreSource::Reranked);
        assert!((hit.score - rerank_score).abs() < f32::EPSILON);
        assert!((hit.relevance_score() - rerank_score).abs() < f32::EPSILON);
        assert!(search_hit_meets_relevance_floor(&hit, None));
        let explanation = hit
            .explanation
            .as_ref()
            .expect("explain=true should produce a score explanation");
        assert!(explanation.summary.contains("0.9100"));
        assert!(
            explanation
                .factors
                .iter()
                .any(|factor| factor.name == "rerank" && factor.formula == "score = rerank_score")
        );
    }

    #[test]
    fn configured_fusion_weights_do_not_rescale_reranker_score() {
        // Fusion weights apply to the candidate-retrieval score, not the
        // cross-encoder's final score. With these components the old path
        // would multiply 0.80 by 2x and report a fictitious 1.60 score.
        let rerank_score = 0.80;
        let hit = search_hit_from_scored_result(
            crate::search::ScoredResult {
                doc_id: "mem_reranked_weight_invariant".into(),
                score: 0.02,
                source: crate::search::ScoreSource::Reranked,
                index: None,
                fast_score: Some(0.10),
                quality_score: None,
                lexical_score: Some(0.90),
                rerank_score: Some(rerank_score),
                explanation: None,
                metadata: None,
            },
            true,
            FrankensearchFinalScoreScale::RrfFused,
            SearchFusionWeights {
                lexical: 1.0,
                semantic: 0.0,
                graph: 0.0,
            },
        );

        assert!((hit.score - rerank_score).abs() < f32::EPSILON);
        let explanation = hit
            .explanation
            .as_ref()
            .expect("explain=true should produce a score explanation");
        assert!(
            explanation
                .factors
                .iter()
                .all(|factor| factor.name != "configured_fusion_weight")
        );
    }

    #[test]
    fn graph_only_config_does_not_zero_hybrid_recall_bd_d67os_19() {
        let hit = search_hit_from_scored_result(
            crate::search::ScoredResult {
                doc_id: "mem_graph_weight_recall".into(),
                score: 0.02,
                source: crate::search::ScoreSource::Hybrid,
                index: None,
                fast_score: Some(0.70),
                quality_score: None,
                lexical_score: Some(0.40),
                rerank_score: None,
                explanation: None,
                metadata: None,
            },
            true,
            FrankensearchFinalScoreScale::RrfFused,
            SearchFusionWeights {
                lexical: 0.0,
                semantic: 0.0,
                graph: 1.0,
            },
        );

        assert!(
            hit.relevance_score() >= DEFAULT_RELEVANCE_FLOOR,
            "graph-only config must not erase an otherwise relevant hybrid hit"
        );
        assert!((hit.score - 0.02).abs() < 1e-6);
        let explanation = hit
            .explanation
            .as_ref()
            .expect("explain=true should preserve configured-weight explanation");
        assert!(
            explanation
                .factors
                .iter()
                .any(|factor| factor.name == "configured_fusion_weight"
                    && (factor.value - 1.0).abs() < 1e-6)
        );
    }

    /// Helper: synthesize a hybrid `SearchHit` for adaptive-floor tests.
    fn synthetic_hybrid_hit(doc_id: &str, score: f32) -> SearchHit {
        let mut hit = synthetic_hit(doc_id, score);
        hit.source = ScoreSource::Hybrid;
        hit
    }

    fn synthetic_reranked_hit(doc_id: &str, score: f32) -> SearchHit {
        let mut hit = synthetic_hit(doc_id, score);
        hit.source = ScoreSource::Reranked;
        hit.rerank_score = Some(score);
        hit
    }

    fn rerank_test_report(
        results: Vec<SearchHit>,
        degraded: Vec<SearchDegradation>,
        rerank_runtime_available: bool,
    ) -> SearchReport {
        SearchReport {
            index_freshness: None,
            status: SearchStatus::Success,
            embed_backend: EmbedBackend::HashFallback,
            query: "release formatting policy".to_string(),
            requested_limit: 10,
            results,
            elapsed_ms: 12.3,
            errors: Vec::new(),
            degraded,
            runtime_profile: test_runtime_profile(),
            rerank_configured_mode: crate::config::SearchRerankMode::Auto,
            rerank_configured_top_k: DEFAULT_SEARCH_RERANK_TOP_K,
            rerank_runtime_available,
            relevance_floor_applied: None,
            candidates_below_floor: 0,
            query_assist: None,
            source_mode_requested: SearchSourceMode::Hybrid,
            source_mode_applied: SearchSourceMode::Hybrid,
            source_mode_fallback: false,
            strict_source_mode: false,
            memory_scope: MemoryScope::Swarm,
            strict_scope: false,
            scope_stats: test_scope_stats(),
        }
    }

    fn registered_reranker_entry(
        id: &str,
        model_name: &str,
        purpose: ModelPurpose,
        status: ModelRegistryStatus,
    ) -> StoredModelRegistryEntry {
        StoredModelRegistryEntry {
            id: id.to_owned(),
            workspace_id: "wsp_reranker_resolution".to_owned(),
            provider: crate::models::ModelProvider::FastEmbed,
            model_name: model_name.to_owned(),
            purpose,
            dimension: None,
            distance_metric: None,
            status,
            version: Some("v1".to_owned()),
            source_uri: None,
            content_hash: None,
            metadata_json: None,
            created_at: "2026-08-12T00:00:00Z".to_owned(),
            updated_at: "2026-08-12T00:00:00Z".to_owned(),
            last_checked_at: None,
        }
    }

    #[test]
    fn doctor_and_search_resolution_skip_broken_first_for_valid_second() {
        let bundled = crate::core::model::bundled_rerank_model_manifest()
            .expect("bundled rerank manifest must be readable");
        let valid_hash = format!("blake3:{}", bundled.hash_blake3);

        let mut broken_first = registered_reranker_entry(
            "mdl_broken_first",
            "a-broken",
            ModelPurpose::Reranker,
            ModelRegistryStatus::Available,
        );
        broken_first.content_hash = Some("blake3:00000000000000000000000000000000".to_owned());
        let mut valid_second = registered_reranker_entry(
            "mdl_valid_second",
            "b-valid",
            ModelPurpose::Reranker,
            ModelRegistryStatus::Available,
        );
        valid_second.content_hash = Some(valid_hash);

        let entries = vec![
            valid_second,
            broken_first,
            registered_reranker_entry(
                "mdl_unavailable",
                "0-unavailable",
                ModelPurpose::Reranker,
                ModelRegistryStatus::Unavailable,
            ),
            registered_reranker_entry(
                "mdl_embedding",
                "0-embedding",
                ModelPurpose::Embedding,
                ModelRegistryStatus::Available,
            ),
        ];
        let entries = sorted_available_reranker_entries(entries);
        assert_eq!(
            entries
                .iter()
                .map(|entry| entry.model_name.as_str())
                .collect::<Vec<_>>(),
            vec!["a-broken", "b-valid"],
            "only available reranker rows survive, sorted deterministically"
        );
        assert!(verify_reranker_registry_hash(&entries[0]).is_err());
        assert!(verify_reranker_registry_hash(&entries[1]).is_ok());

        let mut attempts = Vec::new();
        let (selected, loaded) = first_loadable_registered_reranker(&entries, |entry| {
            attempts.push(entry.model_name.clone());
            if entry.model_name == "b-valid" {
                Some("loaded-valid-second")
            } else {
                None
            }
        })
        .expect("the deterministic resolver must continue after the hash-rejected first entry");

        assert_eq!(
            attempts,
            vec!["b-valid"],
            "hash-rejected first row is skipped before the loader runs"
        );
        assert_eq!(selected.model_name, "b-valid");
        assert_eq!(loaded, "loaded-valid-second");
    }

    #[test]
    fn reranked_score_kind_is_stable() {
        assert_eq!(ScoreSource::Reranked.score_kind(), "reranked");
        assert_eq!(
            synthetic_reranked_hit("mem_rerank", 0.91).score_kind(),
            "reranked"
        );
    }

    #[test]
    fn search_report_data_json_builds_rerank_posture_from_visible_results() {
        let report = rerank_test_report(
            vec![
                synthetic_reranked_hit("mem_reranked", 0.93),
                synthetic_hybrid_hit("mem_fusion", 2.0 / 61.0),
            ],
            Vec::new(),
            true,
        );

        let json = report.data_json();

        assert_eq!(json["resultCount"], 2);
        assert_eq!(json["rerank"]["schema"], "ee.rerank_posture.v1");
        assert_eq!(json["rerank"]["mode"], "reranked");
        assert_eq!(json["rerank"]["configured"], "auto");
        assert_eq!(json["rerank"]["topK"], DEFAULT_SEARCH_RERANK_TOP_K);
        assert_eq!(json["rerank"]["rerankScoreCount"], 1);
        assert_eq!(json["rerank"]["scoreKind"], "reranked");
        assert_eq!(json["rerank"]["available"], true);
        assert!(json["rerank"]["degradedCode"].is_null());
        assert_eq!(json["results"][0]["scoreKind"], "reranked");
        assert!(json["results"][0].get("rerankScore").is_some());
        assert_eq!(json["results"][1]["scoreKind"], "rrf_fused");
        assert!(json["results"][1].get("rerankScore").is_none());
    }

    #[test]
    fn resolve_search_rerank_config_reads_off_and_top_k() {
        // bd-2vq2z.23: `[search] rerank = "off"` + `rerank_top_k` resolve from
        // the workspace config (not the hard-coded auto/DEFAULT).
        let tempdir = tempfile::tempdir().expect("tempdir");
        let workspace = tempdir.path();
        std::fs::create_dir_all(workspace.join(".ee")).expect("create .ee");
        std::fs::write(
            workspace.join(".ee").join("config.toml"),
            "[search]\nrerank = \"off\"\nrerank_top_k = 12\n",
        )
        .expect("write config.toml");

        let (mode, top_k) = resolve_search_rerank_config(workspace);
        assert_eq!(mode, crate::config::SearchRerankMode::Off);
        assert_eq!(top_k, 12);
    }

    #[test]
    fn resolve_search_rerank_config_defaults_to_auto_when_unset() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let (mode, top_k) = resolve_search_rerank_config(tempdir.path());
        assert_eq!(mode, crate::config::SearchRerankMode::Auto);
        assert_eq!(top_k, DEFAULT_SEARCH_RERANK_TOP_K);
    }

    #[test]
    fn search_report_data_json_uses_report_rerank_config() {
        let mut report = rerank_test_report(Vec::new(), Vec::new(), false);
        report.rerank_configured_mode = crate::config::SearchRerankMode::Off;
        report.rerank_configured_top_k = 12;

        let json = report.data_json();

        assert_eq!(json["rerank"]["mode"], "fusion_only");
        assert_eq!(json["rerank"]["configured"], "off");
        assert_eq!(json["rerank"]["topK"], 12);
    }

    #[test]
    fn rerank_posture_reports_reranked_scores() {
        let hit = synthetic_reranked_hit("mem_reranked", 0.91);
        let mut advisory_session = SearchAdvisorySession::default();
        let posture = search_rerank_posture_json(
            &[&hit],
            &[],
            crate::config::SearchRerankMode::Auto,
            DEFAULT_SEARCH_RERANK_TOP_K,
            true,
            &mut advisory_session,
            DEFAULT_SEARCH_ADVISORY_WORKSPACE,
        );

        assert_eq!(posture["schema"], "ee.rerank_posture.v1");
        assert_eq!(posture["mode"], "reranked");
        assert_eq!(posture["configured"], "auto");
        assert_eq!(posture["topK"], DEFAULT_SEARCH_RERANK_TOP_K);
        assert_eq!(posture["rerankScoreCount"], 1);
        assert_eq!(posture["scoreKind"], "reranked");
        assert_eq!(posture["available"], true);
        assert!(posture["degradedCode"].is_null());
        assert!(posture["advisory"].is_null());
    }

    #[test]
    fn rerank_posture_reports_empty_fusion_only_without_degradation() {
        let mut advisory_session = SearchAdvisorySession::default();
        let posture = search_rerank_posture_json(
            &[],
            &[],
            crate::config::SearchRerankMode::Off,
            12,
            false,
            &mut advisory_session,
            DEFAULT_SEARCH_ADVISORY_WORKSPACE,
        );

        assert_eq!(posture["schema"], "ee.rerank_posture.v1");
        assert_eq!(posture["mode"], "fusion_only");
        assert_eq!(posture["configured"], "off");
        assert_eq!(posture["topK"], 12);
        assert_eq!(posture["rerankScoreCount"], 0);
        assert_eq!(posture["scoreKind"], "rrf_fused");
        assert_eq!(posture["available"], false);
        assert!(posture["degradedCode"].is_null());
    }

    #[test]
    fn rerank_posture_reports_loaded_runtime_available_without_hits() {
        let mut advisory_session = SearchAdvisorySession::default();
        let posture = search_rerank_posture_json(
            &[],
            &[],
            crate::config::SearchRerankMode::Auto,
            DEFAULT_SEARCH_RERANK_TOP_K,
            true,
            &mut advisory_session,
            DEFAULT_SEARCH_ADVISORY_WORKSPACE,
        );

        assert_eq!(posture["mode"], "fusion_only");
        assert_eq!(posture["rerankScoreCount"], 0);
        assert_eq!(posture["available"], true);
        assert!(posture["advisory"].is_null());
    }

    #[test]
    fn rerank_posture_counts_mixed_reranked_hits_only() {
        let reranked_a = synthetic_reranked_hit("mem_reranked_a", 0.98);
        let fusion = synthetic_hybrid_hit("mem_fusion", 2.0 / 61.0);
        let reranked_b = synthetic_reranked_hit("mem_reranked_b", 0.74);
        let hits = vec![&reranked_a, &fusion, &reranked_b];
        let mut advisory_session = SearchAdvisorySession::default();

        let posture = search_rerank_posture_json(
            &hits,
            &[],
            crate::config::SearchRerankMode::Auto,
            DEFAULT_SEARCH_RERANK_TOP_K,
            true,
            &mut advisory_session,
            DEFAULT_SEARCH_ADVISORY_WORKSPACE,
        );

        assert_eq!(posture["mode"], "reranked");
        assert_eq!(posture["rerankScoreCount"], 2);
        assert_eq!(posture["scoreKind"], "reranked");
        assert_eq!(posture["available"], true);
        assert!(posture["degradedCode"].is_null());
    }

    #[test]
    fn rerank_posture_ignores_unrelated_degradation() {
        let hit = synthetic_hybrid_hit("mem_fusion", 2.0 / 61.0);
        let degraded = vec![SearchDegradation {
            code: "index_stale".to_string(),
            severity: "warning".to_string(),
            message: "Index generation is behind the database.".to_string(),
            repair: Some("ee index rebuild --workspace .".to_string()),
        }];
        let mut advisory_session = SearchAdvisorySession::default();

        let posture = search_rerank_posture_json(
            &[&hit],
            &degraded,
            crate::config::SearchRerankMode::Auto,
            DEFAULT_SEARCH_RERANK_TOP_K,
            true,
            &mut advisory_session,
            DEFAULT_SEARCH_ADVISORY_WORKSPACE,
        );

        assert_eq!(posture["mode"], "fusion_only");
        assert_eq!(posture["rerankScoreCount"], 0);
        assert_eq!(posture["scoreKind"], "rrf_fused");
        assert_eq!(posture["available"], true);
        assert!(posture["degradedCode"].is_null());
    }

    #[test]
    fn rerank_posture_reports_fusion_only_degraded() {
        let hit = synthetic_hybrid_hit("mem_fusion", 2.0 / 61.0);
        let degraded = vec![SearchDegradation::rerank_model_absent()];
        let mut advisory_session = SearchAdvisorySession::default();
        let posture = search_rerank_posture_json(
            &[&hit],
            &degraded,
            crate::config::SearchRerankMode::Auto,
            DEFAULT_SEARCH_RERANK_TOP_K,
            false,
            &mut advisory_session,
            DEFAULT_SEARCH_ADVISORY_WORKSPACE,
        );

        assert_eq!(posture["mode"], "fusion_only_degraded");
        assert_eq!(posture["scoreKind"], "rrf_fused");
        assert_eq!(posture["available"], false);
        assert_eq!(posture["degradedCode"], "rerank_model_unavailable");
        assert_eq!(posture["advisory"]["code"], "rerank_model_unavailable");
        assert_eq!(posture["advisory"]["permanent"], true);
        assert!(posture["advisory"]["repair"].is_null());
        assert_eq!(
            posture["advisory"]["resolution"],
            "automatic_repair_unavailable"
        );
        assert_eq!(posture["advisorySummary"]["emittedCount"], 1);
        assert_eq!(posture["advisorySummary"]["suppressedCount"], 0);
    }

    #[test]
    fn search_report_data_json_reports_degraded_rerank_on_empty_results() {
        let report = rerank_test_report(
            Vec::new(),
            vec![SearchDegradation::rerank_model_absent()],
            false,
        );
        let mut advisory_session = SearchAdvisorySession::default();

        let json = report.data_json_with_advisory_session(&mut advisory_session);

        assert_eq!(json["resultCount"], 0);
        assert_eq!(json["rerank"]["mode"], "fusion_only_degraded");
        assert_eq!(json["rerank"]["rerankScoreCount"], 0);
        assert_eq!(json["rerank"]["scoreKind"], "rrf_fused");
        assert_eq!(json["rerank"]["available"], false);
        assert_eq!(json["rerank"]["degradedCode"], "rerank_model_unavailable");
        assert_eq!(json["rerank"]["advisory"]["permanent"], true);
        assert!(json["rerank"]["advisory"]["repair"].is_null());
        assert_eq!(json["rerank"]["advisorySummary"]["emittedCount"], 1);
        assert_eq!(json["degraded"], serde_json::json!([]));
    }

    #[test]
    fn rerank_model_unavailable_degradation_is_stable() {
        let degraded = SearchDegradation::rerank_model_absent();

        assert_eq!(degraded.code, "rerank_model_unavailable");
        assert_eq!(degraded.severity, "low");
        assert_eq!(degraded.message, RERANK_MODEL_UNAVAILABLE_ADVISORY);
        assert!(degraded.repair.is_none());
        assert!(degraded.is_permanent());
        assert!(degraded.data_json().get("permanent").is_none());
    }

    #[test]
    fn transient_reranker_load_failure_remains_in_query_degraded() {
        let report = rerank_test_report(
            Vec::new(),
            vec![SearchDegradation::rerank_model_unavailable(
                "The registered local reranker could not be loaded; retry the query or inspect local model status.",
            )],
            false,
        );
        let mut advisory_session = SearchAdvisorySession::default();

        let json = report.data_json_with_advisory_session(&mut advisory_session);

        assert_eq!(json["degraded"].as_array().map(Vec::len), Some(1));
        assert_eq!(json["degraded"][0]["code"], "rerank_model_unavailable");
        assert_eq!(json["rerank"]["advisory"]["permanent"], false);
        assert_eq!(
            json["rerank"]["advisory"]["resolution"],
            "retry_or_inspect_local_registry"
        );
    }

    #[test]
    fn permanent_rerank_advisory_is_omitted_from_default_degraded_prose() {
        let mut report = rerank_test_report(
            Vec::new(),
            vec![SearchDegradation::rerank_model_absent()],
            false,
        );
        report
            .degraded
            .push(SearchDegradation::stale_index(Some(2), Some(1)));
        let mut advisory_session = SearchAdvisorySession::default();

        let human = report.human_summary();
        let json = report.data_json_with_advisory_session(&mut advisory_session);

        assert!(!human.contains("rerank_model_unavailable"));
        assert!(!human.contains(RERANK_MODEL_UNAVAILABLE_ADVISORY));
        assert!(!human.contains("--from-file /path/to/"));
        assert!(human.contains("search_index_stale"));
        assert_eq!(json["degraded"].as_array().map(Vec::len), Some(1));
        assert_eq!(json["degraded"][0]["code"], "search_index_stale");
        assert_eq!(json["rerank"]["advisory"]["permanent"], true);
    }

    #[test]
    fn large_gap_repair_advisory_is_once_while_stale_truth_remains() {
        let mut first = rerank_test_report(
            Vec::new(),
            vec![
                SearchDegradation::stale_index(Some(102), Some(1)),
                SearchDegradation::large_index_gap(102, 1),
            ],
            false,
        );
        first.rerank_configured_mode = crate::config::SearchRerankMode::Off;
        first.index_freshness = Some(SearchIndexFreshness {
            stale: true,
            db_generation: Some(102),
            index_generation: Some(1),
            generation_gap: Some(101),
            large_gap: true,
        });
        let mut changed_generation = first.clone();
        changed_generation.degraded = vec![
            SearchDegradation::stale_index(Some(108), Some(2)),
            SearchDegradation::large_index_gap(108, 2),
        ];
        changed_generation.index_freshness = Some(SearchIndexFreshness {
            stale: true,
            db_generation: Some(108),
            index_generation: Some(2),
            generation_gap: Some(106),
            large_gap: true,
        });
        let mut session = SearchAdvisorySession::default();

        let first_json = first.data_json_with_advisory_session(&mut session);
        let repeated_json = changed_generation.data_json_with_advisory_session(&mut session);

        let first_degraded = first_json["degraded"]
            .as_array()
            .expect("first response degraded array");
        assert!(first_degraded.iter().any(|entry| {
            entry["code"] == "search_index_stale"
                && entry["repair"] == "ee index rebuild --workspace ."
        }));
        assert!(
            first_degraded
                .iter()
                .any(|entry| entry["code"] == "search_index_large_gap")
        );
        assert_eq!(repeated_json["degraded"], serde_json::json!([]));
        assert_eq!(repeated_json["indexFreshness"]["stale"], true);
        assert_eq!(repeated_json["indexFreshness"]["dbGeneration"], 108);
        assert_eq!(repeated_json["indexFreshness"]["indexGeneration"], 2);
        assert_eq!(repeated_json["indexFreshness"]["generationGap"], 106);
        assert_eq!(repeated_json["indexFreshness"]["largeGap"], true);
    }

    #[test]
    fn large_gap_episode_emits_warning_once_and_keeps_structured_truth() {
        let freshness = |db: u64, index: u64| SearchIndexFreshness {
            stale: true,
            db_generation: Some(db),
            index_generation: Some(index),
            generation_gap: Some(db.saturating_sub(index)),
            large_gap: db.saturating_sub(index) > SEARCH_INDEX_LARGE_GAP_THRESHOLD,
        };
        let stale_report = |db: u64, index: u64, with_gap: bool| {
            let mut degraded = vec![SearchDegradation::stale_index(Some(db), Some(index))];
            if with_gap {
                degraded.push(SearchDegradation::large_index_gap(db, index));
            }
            let mut report = rerank_test_report(Vec::new(), degraded, false);
            report.rerank_configured_mode = crate::config::SearchRerankMode::Off;
            report.index_freshness = Some(freshness(db, index));
            report
        };
        let codes = |json: &serde_json::Value| -> Vec<String> {
            json["degraded"]
                .as_array()
                .expect("degraded array")
                .iter()
                .filter_map(|entry| entry["code"].as_str().map(str::to_owned))
                .collect()
        };
        let assert_full_warning = |json: &serde_json::Value, db: u64, index: u64| {
            let entries = json["degraded"].as_array().expect("degraded array");
            let stale = entries
                .iter()
                .find(|entry| entry["code"] == "search_index_stale")
                .expect("stale warning present");
            let message = stale["message"].as_str().expect("stale message");
            assert!(message.starts_with(SEARCH_INDEX_STALE_WARNING_PREFIX));
            assert!(message.contains(&format!(
                "Database generation is {db}; index generation is {index}."
            )));
            assert_eq!(stale["repair"], "ee index rebuild --workspace .");
            assert!(
                entries
                    .iter()
                    .any(|entry| entry["code"] == "search_index_large_gap")
            );
        };
        let assert_freshness = |json: &serde_json::Value, db: u64, index: u64, large: bool| {
            assert_eq!(json["indexFreshness"]["stale"], true);
            assert_eq!(json["indexFreshness"]["dbGeneration"], db);
            assert_eq!(json["indexFreshness"]["indexGeneration"], index);
            assert_eq!(json["indexFreshness"]["largeGap"], large);
        };

        let mut session = SearchAdvisorySession::default();

        // First affected response: both warnings, full prose, rebuild repair,
        // plus the structured truth.
        let first = stale_report(120, 4, true)
            .data_json_with_advisory_session_for_workspace(&mut session, "wsp-a");
        assert_full_warning(&first, 120, 4);
        assert_freshness(&first, 120, 4, true);

        // Same active episode: neither warning appears, while fresh
        // authoritative generations stay structurally visible per-response.
        let second = stale_report(130, 4, true)
            .data_json_with_advisory_session_for_workspace(&mut session, "wsp-a");
        let second_codes = codes(&second);
        assert!(!second_codes.iter().any(|code| code == "search_index_stale"));
        assert!(
            !second_codes
                .iter()
                .any(|code| code == "search_index_large_gap")
        );
        assert_freshness(&second, 130, 4, true);

        // Another workspace is isolated from wsp-a's episode.
        let other = stale_report(90, 2, true)
            .data_json_with_advisory_session_for_workspace(&mut session, "wsp-b");
        assert_full_warning(&other, 90, 2);

        // Small-gap staleness keeps the canonical per-response warning and
        // ends wsp-a's large-gap episode.
        let small = stale_report(14, 13, false)
            .data_json_with_advisory_session_for_workspace(&mut session, "wsp-a");
        let small_entries = small["degraded"].as_array().expect("degraded array");
        assert!(
            small_entries
                .iter()
                .any(|entry| entry["code"] == "search_index_stale")
        );
        assert_freshness(&small, 14, 13, false);

        // A later large gap rearms the full warning pair.
        let rearmed = stale_report(400, 2, true)
            .data_json_with_advisory_session_for_workspace(&mut session, "wsp-a");
        assert_full_warning(&rearmed, 400, 2);

        // One-shot invocation-scoped renders always get their one warning.
        for _ in 0..2 {
            let one_shot = stale_report(120, 4, true).data_json();
            assert_full_warning(&one_shot, 120, 4);
            assert_freshness(&one_shot, 120, 4, true);
        }
    }

    #[test]
    fn successful_auto_reconcile_ready_rearms_later_large_gap_episode() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let workspace = temp.path();
        let init = crate::core::init::init_workspace(&crate::core::init::InitOptions {
            workspace_path: workspace.to_path_buf(),
            dry_run: false,
            repair_plan: false,
            force: false,
            allow_symlink: false,
            skip_boilerplate: true,
        });
        ensure(
            !matches!(init.status, crate::core::init::InitStatus::Failed),
            format!(
                "initialize auto-reconcile fixture: {:?}",
                init.action_errors
            ),
        )?;
        let remember = |content: &str, defer_index_processing: bool| {
            let options = crate::core::memory::RememberMemoryOptions {
                workspace_path: workspace,
                database_path: None,
                content,
                workflow_id: None,
                level: "procedural",
                kind: "rule",
                tags: Some("auto-reconcile-rearm"),
                confidence: 0.9,
                source: Some("manual://auto-reconcile-rearm"),
                allow_secret_mention: false,
                valid_from: None,
                valid_to: None,
                dry_run: false,
                auto_link: false,
                propose_candidates: false,
            };
            if defer_index_processing {
                crate::core::memory::remember_memory_with_controls(
                    &options,
                    &crate::core::memory::RememberWriteControls {
                        reinforce: false,
                        idempotency_key: None,
                        defer_index_processing: true,
                    },
                )
                .map(|_| ())
                .map_err(|error| error.to_string())
            } else {
                crate::core::memory::remember_memory(&options)
                    .map(|_| ())
                    .map_err(|error| error.to_string())
            }
        };
        let database_path = init.database_path.clone();
        let connection =
            DbConnection::open_file(&database_path).map_err(|error| error.to_string())?;
        let workspace_record = connection
            .get_workspace_by_path(&init.workspace.to_string_lossy())
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "initialized workspace must be bound in the database".to_owned())?;
        let registry_entries = connection
            .list_model_registry_entries(&workspace_record.id)
            .map_err(|error| error.to_string())?;
        ensure(
            registry_entries.len() == 1,
            format!(
                "auto-reconcile fixture must start with one bundled model declaration: {registry_entries:?}"
            ),
        )?;
        let registry_entry = &registry_entries[0];
        let registry_updated = connection
            .update_model_registry_entry(
                &registry_entry.id,
                &crate::db::CreateModelRegistryInput {
                    workspace_id: registry_entry.workspace_id.clone(),
                    provider: registry_entry.provider,
                    model_name: "test-unavailable-model".to_owned(),
                    purpose: registry_entry.purpose,
                    dimension: registry_entry.dimension,
                    distance_metric: registry_entry.distance_metric,
                    status: crate::models::ModelRegistryStatus::Unavailable,
                    version: registry_entry.version.clone(),
                    source_uri: Some("test://unavailable-model".to_owned()),
                    content_hash: None,
                    metadata_json: registry_entry.metadata_json.clone(),
                    last_checked_at: registry_entry.last_checked_at.clone(),
                },
            )
            .map_err(|error| error.to_string())?;
        ensure(
            registry_updated,
            "auto-reconcile fixture must force deterministic hash fallback",
        )?;
        connection.close().map_err(|error| error.to_string())?;

        remember("Initial indexed auto-reconcile rearm rule.", false)?;
        let rebuild = crate::core::index::rebuild_index(&crate::core::index::IndexRebuildOptions {
            workspace_path: workspace.to_path_buf(),
            database_path: Some(database_path.clone()),
            index_dir: None,
            dry_run: false,
        })
        .map_err(|error| error.to_string())?;
        ensure(
            rebuild.status == crate::core::index::IndexRebuildStatus::Success,
            format!("initial auto-reconcile fixture rebuild failed: {rebuild:?}"),
        )?;
        remember(
            "Queued auto-reconcile update becomes searchable before the read.",
            true,
        )?;

        let status_options = crate::core::index::IndexStatusOptions {
            workspace_path: workspace.to_path_buf(),
            database_path: Some(database_path.clone()),
            index_dir: None,
        };
        let stale = crate::core::index::get_index_status(&status_options)
            .map_err(|error| error.to_string())?;
        ensure(
            stale.health == crate::core::index::IndexHealth::Stale,
            format!("queued update must make the pre-search index stale: {stale:?}"),
        )?;

        let ready_report = run_search(&SearchOptions {
            workspace_path: workspace.to_path_buf(),
            database_path: Some(database_path),
            index_dir: None,
            query: "auto-reconcile update".to_owned(),
            limit: 10,
            speed: SpeedMode::Instant,
            explain: false,
            as_of: None,
            include_tombstoned: false,
            include_expired: false,
            include_future: false,
            include_stale: false,
            relevance_floor: Some(0.0),
            dedup_mode: SearchDedupMode::DocId,
            source_mode: SearchSourceMode::LexicalOnly,
            strict_source_mode: true,
            memory_scope: MemoryScope::Workspace,
            strict_scope: false,
        })
        .map_err(|error| error.to_string())?;
        let ready = crate::core::index::get_index_status(&status_options)
            .map_err(|error| error.to_string())?;
        ensure(
            ready.health == crate::core::index::IndexHealth::Ready,
            format!("bounded pre-read reconciliation must converge to ready: {ready:?}"),
        )?;
        ensure(
            ready_report.index_freshness.is_none(),
            format!("ready search must not claim stale freshness: {ready_report:?}"),
        )?;

        let large_gap_report = |db_generation: u64, index_generation: u64| {
            let mut report = rerank_test_report(
                Vec::new(),
                vec![
                    SearchDegradation::stale_index(Some(db_generation), Some(index_generation)),
                    SearchDegradation::large_index_gap(db_generation, index_generation),
                ],
                false,
            );
            report.rerank_configured_mode = crate::config::SearchRerankMode::Off;
            report.index_freshness = Some(SearchIndexFreshness {
                stale: true,
                db_generation: Some(db_generation),
                index_generation: Some(index_generation),
                generation_gap: Some(db_generation.saturating_sub(index_generation)),
                large_gap: true,
            });
            report
        };
        let contains_large_gap = |json: &serde_json::Value| {
            json["degraded"].as_array().is_some_and(|entries| {
                entries
                    .iter()
                    .any(|entry| entry["code"] == "search_index_large_gap")
            })
        };

        let mut session = SearchAdvisorySession::default();
        let first = large_gap_report(150, 1)
            .data_json_with_advisory_session_for_workspace(&mut session, "auto-reconcile-wsp");
        ensure(
            contains_large_gap(&first),
            format!("first large-gap episode must emit its advisory: {first}"),
        )?;
        let ready_json = ready_report
            .data_json_with_advisory_session_for_workspace(&mut session, "auto-reconcile-wsp");
        ensure(
            !contains_large_gap(&ready_json),
            format!("ready response must clear the active large-gap episode: {ready_json}"),
        )?;
        let rearmed = large_gap_report(300, 2)
            .data_json_with_advisory_session_for_workspace(&mut session, "auto-reconcile-wsp");
        ensure(
            contains_large_gap(&rearmed),
            format!("later large gap must rearm after ready reconciliation: {rearmed}"),
        )
    }

    #[test]
    fn staged_permanent_advisory_does_not_mutate_source_session() {
        let report = rerank_test_report(
            Vec::new(),
            vec![SearchDegradation::rerank_model_absent()],
            false,
        );
        let source = SearchAdvisorySession::default();
        let mut staged = source.clone();

        let json = report.data_json_with_advisory_session(&mut staged);

        assert_eq!(source, SearchAdvisorySession::default());
        assert_ne!(staged, source);
        assert_eq!(json["rerank"]["advisorySummary"]["emittedCount"], 1);
    }

    #[test]
    fn failed_delivery_rolls_back_only_its_provisional_advisory_counters() {
        let degradation = SearchDegradation::rerank_model_absent();
        let workspace_id = "concurrent-delivery-workspace";
        let mut session = SearchAdvisorySession::default();

        let mut winner = session.reserve_delivery(workspace_id);
        let winner_observation =
            session.observe_permanent_for_delivery(workspace_id, &degradation, &mut winner);
        assert!(winner_observation.emitted);
        assert!(winner.emitted());
        assert!(winner.has_provisional_accounting());

        let mut suppressed = session.reserve_delivery(workspace_id);
        let suppressed_observation =
            session.observe_permanent_for_delivery(workspace_id, &degradation, &mut suppressed);
        assert!(!suppressed_observation.emitted);
        assert!(!suppressed.emitted());
        assert!(suppressed.has_provisional_accounting());
        assert_eq!(suppressed_observation.occurrence_count, 1);
        assert_eq!(suppressed_observation.session_suppressed_count, 1);

        assert_eq!(
            session.settle_delivery(
                suppressed.workspace_id(),
                suppressed.token(),
                false,
                suppressed.large_gap_capacity_busy(),
            ),
            SearchAdvisorySettlement::Complete
        );
        let state = session
            .permanent_occurrences
            .values()
            .next()
            .expect("winner reservation must remain after competing rollback");
        assert_eq!(state.occurrence_count, 0);
        assert_eq!(state.reserved_delivery, Some(winner.token()));
        assert_eq!(session.permanent_suppressed_count, 0);

        assert_eq!(
            session.settle_delivery(
                winner.workspace_id(),
                winner.token(),
                false,
                winner.large_gap_capacity_busy(),
            ),
            SearchAdvisorySettlement::Complete
        );
        assert!(session.permanent_occurrences.is_empty());
        assert_eq!(session.permanent_suppressed_count, 0);

        let mut retry = session.reserve_delivery(workspace_id);
        let retry_observation =
            session.observe_permanent_for_delivery(workspace_id, &degradation, &mut retry);
        assert!(retry_observation.emitted);
        assert_eq!(retry_observation.occurrence_count, 1);
        assert_eq!(retry_observation.distinct_count, 1);
        assert_eq!(retry_observation.session_suppressed_count, 0);
    }

    #[test]
    fn concurrent_delivery_counters_never_include_another_pending_reservation() {
        let degradation = SearchDegradation::rerank_model_absent();
        let workspace_id = "three-way-delivery-workspace";
        let mut session = SearchAdvisorySession::default();

        let mut reservation_a = session.reserve_delivery(workspace_id);
        let observation_a =
            session.observe_permanent_for_delivery(workspace_id, &degradation, &mut reservation_a);
        assert!(observation_a.emitted);
        assert_eq!(observation_a.occurrence_count, 1);
        assert_eq!(observation_a.session_suppressed_count, 0);

        let mut reservation_b = session.reserve_delivery(workspace_id);
        let observation_b =
            session.observe_permanent_for_delivery(workspace_id, &degradation, &mut reservation_b);
        assert!(!observation_b.emitted);
        assert_eq!(observation_b.occurrence_count, 1);
        assert_eq!(observation_b.session_suppressed_count, 1);

        let mut reservation_c = session.reserve_delivery(workspace_id);
        let observation_c =
            session.observe_permanent_for_delivery(workspace_id, &degradation, &mut reservation_c);
        assert!(!observation_c.emitted);
        assert_eq!(observation_c.occurrence_count, 1);
        assert_eq!(observation_c.session_suppressed_count, 1);

        assert_eq!(
            session.settle_delivery(
                reservation_c.workspace_id(),
                reservation_c.token(),
                true,
                reservation_c.large_gap_capacity_busy(),
            ),
            SearchAdvisorySettlement::Complete
        );
        assert_eq!(
            session
                .permanent_occurrences
                .values()
                .next()
                .expect("C settlement commits the identity")
                .occurrence_count,
            1
        );
        assert_eq!(session.permanent_suppressed_count, 1);

        assert_eq!(
            session.settle_delivery(
                reservation_b.workspace_id(),
                reservation_b.token(),
                false,
                reservation_b.large_gap_capacity_busy(),
            ),
            SearchAdvisorySettlement::Complete
        );
        assert_eq!(
            session
                .permanent_occurrences
                .values()
                .next()
                .expect("C's committed identity must survive B rollback")
                .occurrence_count,
            1
        );
        assert_eq!(session.permanent_suppressed_count, 1);
        assert_eq!(
            session
                .permanent_occurrences
                .values()
                .next()
                .and_then(|state| state.reserved_delivery),
            Some(reservation_a.token()),
            "B failure must not release A's ownership reservation"
        );
    }

    #[test]
    fn advisory_session_bounds_workspace_partitions() {
        let mut session = SearchAdvisorySession::default();

        for index in 0..=MAX_SEARCH_ADVISORY_WORKSPACES {
            let workspace_id = format!("workspace-{index:03}");
            let _ = session.emit_large_gap_while_active(&workspace_id, true);
        }

        assert_eq!(session.workspaces.len(), MAX_SEARCH_ADVISORY_WORKSPACES);
        assert!(!session.workspaces.contains_key("workspace-000"));
        assert!(
            session
                .workspaces
                .contains_key(&format!("workspace-{MAX_SEARCH_ADVISORY_WORKSPACES:03}"))
        );
    }

    #[test]
    fn permanent_advisory_stays_consumed_across_recovery_and_workspace_eviction() {
        let report = rerank_test_report(
            Vec::new(),
            vec![SearchDegradation::rerank_model_absent()],
            false,
        );
        let available = rerank_test_report(Vec::new(), Vec::new(), true);
        let mut session = SearchAdvisorySession::default();

        let first =
            report.data_json_with_advisory_session_for_workspace(&mut session, "workspace-first");
        let _ = available
            .data_json_with_advisory_session_for_workspace(&mut session, "workspace-first");
        for index in 0..=MAX_SEARCH_ADVISORY_WORKSPACES {
            let workspace_id = format!("workspace-transient-{index:03}");
            let _ = session.emit_large_gap_while_active(&workspace_id, true);
        }
        let later =
            report.data_json_with_advisory_session_for_workspace(&mut session, "workspace-later");

        assert_eq!(
            first["rerank"]["advisory"]["code"],
            "rerank_model_unavailable"
        );
        assert_eq!(first["rerank"]["advisorySummary"]["scope"], "process");
        assert!(later["rerank"]["advisory"].is_null());
        assert_eq!(later["rerank"]["advisorySummary"]["emittedCount"], 0);
        assert_eq!(
            later["rerank"]["advisorySummary"]["sessionOccurrenceCount"],
            2
        );
    }

    #[test]
    fn advisory_session_fails_open_and_retries_when_every_partition_is_reserved() {
        let mut session = SearchAdvisorySession::default();
        let mut reservations = Vec::new();

        for index in 0..MAX_SEARCH_ADVISORY_WORKSPACES {
            let workspace_id = format!("reserved-workspace-{index:03}");
            let mut reservation = session.reserve_delivery(&workspace_id);
            assert_eq!(
                session.emit_large_gap_while_active_for_delivery(
                    &workspace_id,
                    true,
                    &mut reservation,
                ),
                SearchAdvisoryEmission::Emit
            );
            reservations.push(reservation);
        }
        assert_eq!(session.workspaces.len(), MAX_SEARCH_ADVISORY_WORKSPACES);

        let overflow_workspace = "reserved-workspace-overflow";
        let mut overflow = session.reserve_delivery(overflow_workspace);
        assert_eq!(
            session.emit_large_gap_while_active_for_delivery(
                overflow_workspace,
                true,
                &mut overflow,
            ),
            SearchAdvisoryEmission::CapacityBusy
        );
        assert_eq!(session.workspaces.len(), MAX_SEARCH_ADVISORY_WORKSPACES);
        assert!(overflow.emitted());
        assert!(overflow.large_gap_capacity_busy());
        assert_eq!(
            session.settle_delivery(
                overflow.workspace_id(),
                overflow.token(),
                true,
                overflow.large_gap_capacity_busy(),
            ),
            SearchAdvisorySettlement::RetryCapacityBusy
        );

        let released = reservations.pop().expect("one reservation to release");
        assert_eq!(
            session.settle_delivery(
                released.workspace_id(),
                released.token(),
                true,
                released.large_gap_capacity_busy(),
            ),
            SearchAdvisorySettlement::Complete
        );
        assert_eq!(
            session.settle_delivery(
                overflow.workspace_id(),
                overflow.token(),
                true,
                overflow.large_gap_capacity_busy(),
            ),
            SearchAdvisorySettlement::Complete
        );
        assert_eq!(session.workspaces.len(), MAX_SEARCH_ADVISORY_WORKSPACES);
        assert!(session.workspaces.contains_key(overflow_workspace));
        assert_eq!(
            session.emit_large_gap_while_active(overflow_workspace, true),
            SearchAdvisoryEmission::Suppress
        );
    }

    #[test]
    fn ready_observation_invalidates_capacity_busy_delivery_before_settlement() {
        let mut session = SearchAdvisorySession::default();
        let mut reservations = Vec::new();
        for index in 0..MAX_SEARCH_ADVISORY_WORKSPACES {
            let workspace_id = format!("ready-race-workspace-{index:03}");
            let mut reservation = session.reserve_delivery(&workspace_id);
            assert_eq!(
                session.emit_large_gap_while_active_for_delivery(
                    &workspace_id,
                    true,
                    &mut reservation,
                ),
                SearchAdvisoryEmission::Emit
            );
            reservations.push(reservation);
        }

        let overflow_workspace = "ready-race-overflow";
        let mut overflow = session.reserve_delivery(overflow_workspace);
        assert_eq!(
            session.emit_large_gap_while_active_for_delivery(
                overflow_workspace,
                true,
                &mut overflow,
            ),
            SearchAdvisoryEmission::CapacityBusy
        );
        assert_eq!(session.capacity_busy_deliveries.len(), 1);
        assert_eq!(
            session.emit_large_gap_while_active(overflow_workspace, false),
            SearchAdvisoryEmission::Suppress
        );
        assert!(session.capacity_busy_deliveries.is_empty());
        assert_eq!(
            session.settle_delivery(
                overflow.workspace_id(),
                overflow.token(),
                true,
                overflow.large_gap_capacity_busy(),
            ),
            SearchAdvisorySettlement::Complete
        );
        assert!(!session.workspaces.contains_key(overflow_workspace));

        let released = reservations.pop().expect("one reservation to release");
        assert_eq!(
            session.settle_delivery(
                released.workspace_id(),
                released.token(),
                true,
                released.large_gap_capacity_busy(),
            ),
            SearchAdvisorySettlement::Complete
        );
        assert_eq!(
            session.emit_large_gap_while_active(overflow_workspace, true),
            SearchAdvisoryEmission::Emit
        );
    }

    #[test]
    fn default_reranker_fetch_without_from_file_reports_network_unavailable() {
        // The reranker has no network/bundled fetch. Registration is an
        // explicit mutating command, so read-only search must never attempt it.
        let tempdir = tempfile::tempdir().expect("tempdir");
        let workspace = tempdir.path();
        let database_path = workspace.join("registry.db");
        let manifest = crate::core::model::bundled_rerank_model_manifest().expect("manifest");
        let fetch_options = crate::core::model::ModelFetchOptions {
            workspace_path: workspace,
            database_path: Some(&database_path),
            model_id: manifest.model_id.as_str(),
            from_file: None,
            model_store_root: None,
        };
        let error = crate::core::model::fetch_rerank_model(&fetch_options)
            .expect_err("rerank fetch without --from-file must not succeed");
        assert!(
            error
                .to_string()
                .contains("Network model fetch is not available"),
            "expected offline network-unavailable error, got: {error}"
        );
    }

    #[test]
    #[allow(clippy::cast_precision_loss)]
    fn rerank_step_reorders_top_k_when_reranker_available() -> TestResult {
        // bd-2vq2z.29 / bd-1nl13.13: positive-path proof that EE's production
        // rerank helper actually REORDERS the top-K when a reranker is available,
        // not just that search degrades when the model is absent. A deterministic
        // stub reranker scores candidates so the LAST one ranks highest.
        //
        // bd-1zltr extends the proof across EE's adapter and relevance floor:
        // realistic RRF-magnitude candidate scores are all below the reranked
        // unit-domain floor, so only promotion of the final rerank score keeps
        // the reranked winner visible. A real cross-encoder model fixture is
        // exercised by scripts/e2e_rerank.sh, not unit tests.
        struct StubReverseReranker;
        impl frankensearch::SyncRerank for StubReverseReranker {
            fn rerank_sync(
                &self,
                _query: &str,
                documents: &[frankensearch::RerankDocument],
            ) -> frankensearch::SearchResult<Vec<frankensearch::RerankScore>> {
                // Later candidates score higher -> the reranker reverses order.
                Ok(documents
                    .iter()
                    .enumerate()
                    .map(|(rank, document)| frankensearch::RerankScore {
                        doc_id: document.doc_id.clone(),
                        score: (rank + 1) as f32 / documents.len() as f32,
                        original_rank: rank,
                        raw_logit: None,
                    })
                    .collect())
            }
            fn id(&self) -> &str {
                "stub-reverse-reranker"
            }
            fn model_name(&self) -> &str {
                "stub-reverse-reranker"
            }
        }

        // Fusion order: mem_0000 (highest fusion score) .. mem_0005 (lowest).
        let mut candidates: Vec<crate::search::ScoredResult> = (0..6)
            .map(|i| {
                let fusion_score = 2.0 / (61.0 + i as f32);
                crate::search::ScoredResult {
                    doc_id: format!("mem_{i:04}").into(),
                    score: fusion_score,
                    source: crate::search::ScoreSource::Hybrid,
                    index: None,
                    fast_score: None,
                    quality_score: None,
                    lexical_score: Some(fusion_score),
                    rerank_score: None,
                    explanation: None,
                    metadata: None,
                }
            })
            .collect();
        let fusion_order: Vec<String> = candidates
            .iter()
            .map(|hit| hit.doc_id.to_string())
            .collect();
        let candidate_count = candidates.len();

        let reranker: Arc<dyn Reranker> =
            Arc::new(frankensearch::SyncRerankerAdapter(StubReverseReranker));

        let (reranked_order, top_candidate) = crate::core::run_cli_future(async move {
            let cx = asupersync::Cx::for_testing();
            apply_search_rerank(
                &cx,
                "release format checklist",
                &mut candidates,
                reranker.as_ref(),
                |doc_id: &str| Some(format!("text body for {doc_id}")),
                candidate_count,
            )
            .await
            .map_err(|error| error.to_string())?;
            let order: Vec<String> = candidates
                .iter()
                .map(|hit| hit.doc_id.to_string())
                .collect();
            let top_candidate = candidates
                .into_iter()
                .next()
                .ok_or_else(|| "rerank unexpectedly returned no candidates".to_string())?;
            Ok::<(Vec<String>, crate::search::ScoredResult), String>((order, top_candidate))
        })
        .map_err(|error| error.to_string())??;

        assert_ne!(
            reranked_order, fusion_order,
            "rerank must reorder the top-K (got the fusion order unchanged)"
        );
        assert_eq!(
            reranked_order.first().map(String::as_str),
            Some("mem_0005"),
            "stub scored the last candidate highest, so it must rank first after rerank"
        );
        assert_eq!(
            reranked_order.last().map(String::as_str),
            Some("mem_0000"),
            "stub scored the first candidate lowest, so it must rank last after rerank"
        );
        let top_rerank_score = top_candidate
            .rerank_score
            .ok_or_else(|| "reranked top candidate has no rerank score".to_string())?;
        assert!(
            (top_rerank_score - 1.0).abs() < f32::EPSILON,
            "the reranked top candidate must carry the highest unit-domain score"
        );
        assert!(
            top_candidate.score < DEFAULT_RELEVANCE_FLOOR,
            "the retained pre-rerank RRF score should reproduce the original floor bug"
        );

        let top_hit = search_hit_from_scored_result(
            top_candidate,
            true,
            FrankensearchFinalScoreScale::RrfFused,
            SearchFusionWeights::default(),
        );
        assert_eq!(top_hit.source, ScoreSource::Reranked);
        assert!((top_hit.score - 1.0).abs() < f32::EPSILON);
        assert!(
            search_hit_meets_relevance_floor(&top_hit, None),
            "the reranked winner must survive EE's default relevance floor"
        );
        Ok(())
    }

    #[test]
    fn rerank_stored_path_metadata_reads_stable_field() -> TestResult {
        let path = reranker_stored_path_from_metadata(r#"{"storedPath":"/tmp/reranker"}"#)
            .ok_or_else(|| "storedPath should be parsed".to_string())?;

        assert_eq!(path, PathBuf::from("/tmp/reranker"));
        assert!(reranker_stored_path_from_metadata(r#"{"storedPath":""}"#).is_none());
        assert!(reranker_stored_path_from_metadata("not-json").is_none());
        Ok(())
    }

    #[test]
    fn rerank_archive_sibling_dir_strips_tar_zst() -> TestResult {
        let archive = PathBuf::from("/tmp/ee-models/rerank-default-v1.tar.zst");
        let sibling = rerank_archive_sibling_dir(&archive)
            .ok_or_else(|| "tar.zst archive should resolve to sibling dir".to_string())?;

        assert_eq!(sibling, PathBuf::from("/tmp/ee-models/rerank-default-v1"));
        assert!(rerank_archive_sibling_dir(Path::new("/tmp/model.onnx")).is_none());
        Ok(())
    }

    #[test]
    fn unpacked_rerank_model_dir_requires_tokenizer_and_model() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let model_dir = temp.path().join("rerank-default-v1");
        std::fs::create_dir_all(&model_dir).map_err(|error| error.to_string())?;
        std::fs::write(model_dir.join(RERANK_MODEL_TOKENIZER), "{}")
            .map_err(|error| error.to_string())?;
        std::fs::write(
            model_dir.join(RERANK_MODEL_SAFETENSORS_PRIMARY),
            b"safetensors",
        )
        .map_err(|error| error.to_string())?;

        assert_eq!(unpacked_rerank_model_dir(&model_dir)?, model_dir);
        Ok(())
    }

    #[test]
    fn truncate_hits_to_limit_caps_expanded_rerank_pool() {
        let mut hits = vec![
            synthetic_reranked_hit("mem_a", 0.91),
            synthetic_reranked_hit("mem_b", 0.82),
            synthetic_reranked_hit("mem_c", 0.73),
        ];

        truncate_hits_to_limit(&mut hits, 2);

        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].doc_id, "mem_a");
        assert_eq!(hits[1].doc_id, "mem_b");
    }

    #[test]
    fn relevance_floor_counts_ignore_later_visibility_and_limit_drops() {
        let all_passed = RelevanceFloorCounts::new(50, 50);

        assert!(!all_passed.has_no_relevant_results());
        assert!(!all_passed.is_low_recall());
    }

    #[test]
    fn relevance_floor_counts_classify_actual_floor_drops() {
        let empty = RelevanceFloorCounts::new(0, 0);
        assert!(!empty.has_no_relevant_results());
        assert!(!empty.is_low_recall());

        let all_dropped = RelevanceFloorCounts::new(5, 0);
        assert!(all_dropped.has_no_relevant_results());
        assert!(all_dropped.is_low_recall());

        let tiny_miss = RelevanceFloorCounts::new(2, 0);
        assert!(tiny_miss.has_no_relevant_results());
        assert!(!tiny_miss.is_low_recall());

        let partial_recall = RelevanceFloorCounts::new(10, 2);
        assert!(!partial_recall.has_no_relevant_results());
        assert!(partial_recall.is_low_recall());

        let threshold = RelevanceFloorCounts::new(10, 3);
        assert!(!threshold.has_no_relevant_results());
        assert!(!threshold.is_low_recall());
    }

    #[test]
    fn relevance_floor_degradations_ignore_later_visibility_and_limit_drops() {
        let mut hits = (0..50)
            .map(|index| synthetic_reranked_hit(&format!("mem_{index:02}"), 0.90))
            .collect::<Vec<_>>();
        let floor_counts = RelevanceFloorCounts::new(hits.len(), hits.len());

        truncate_hits_to_limit(&mut hits, 10);
        assert_eq!(hits.len(), 10);

        let mut after_limit = Vec::new();
        floor_counts.append_degradations(&mut after_limit, 0.05, Some(0.90));
        assert!(
            after_limit.is_empty(),
            "truncating 50 floor-passing candidates later must not report floor loss"
        );

        let mut after_scope_visibility = Vec::new();
        RelevanceFloorCounts::new(5, 5).append_degradations(
            &mut after_scope_visibility,
            0.05,
            Some(0.80),
        );
        assert!(
            after_scope_visibility.is_empty(),
            "hiding floor-passing candidates later must not report no relevant results"
        );

        let mut actual_floor_loss = Vec::new();
        RelevanceFloorCounts::new(10, 2).append_degradations(
            &mut actual_floor_loss,
            0.05,
            Some(0.20),
        );
        assert_eq!(actual_floor_loss.len(), 1);
        assert_eq!(actual_floor_loss[0].code, "low_recall_after_floor");
        assert!(actual_floor_loss[0].message.contains("2 of 10"));
    }

    #[test]
    fn adaptive_partition_keeps_typical_hybrid_hit_with_no_override() {
        // A hybrid hit at the typical 2-arm RRF top of ≈0.0328 projects to
        // relevance 1.0 and must survive the shared relevance floor.
        let hits = vec![synthetic_hybrid_hit("mem_canonical", 2.0 / 61.0)];
        let kept: Vec<_> = hits
            .into_iter()
            .filter(|hit| search_hit_meets_relevance_floor(hit, None))
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
            .filter(|hit| search_hit_meets_relevance_floor(hit, None))
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
            synthetic_hybrid_hit("mem_hybrid_strong", RRF_HYBRID_TYPICAL_MAX * 0.20),
            synthetic_hybrid_hit("mem_hybrid_weak", RRF_HYBRID_TYPICAL_MAX * 0.02),
            synthetic_hit("mem_semantic_strong", 0.20),
            synthetic_hit("mem_semantic_weak", 0.02),
        ];
        let kept: Vec<_> = hits
            .into_iter()
            .filter(|hit| search_hit_meets_relevance_floor(hit, Some(0.10)))
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
        let degradation = SearchDegradation::no_relevant_results(0.05, 12, Some(0.02));
        assert_eq!(degradation.code, "no_relevant_results");
        assert_eq!(degradation.severity, "medium");
        assert!(degradation.message.contains("0.0500"));
        assert!(!degradation.message.contains("query `"));
        assert!(degradation.message.contains("12 candidate"));
        assert!(degradation.message.contains("0.0200"));
        assert!(degradation.repair.is_some());
    }

    #[test]
    fn no_relevant_results_handles_singular_candidate() {
        let degradation = SearchDegradation::no_relevant_results(0.05, 1, Some(0.01));
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
        let degradation = SearchDegradation::no_relevant_results(0.05, 5, Some(0.0));
        let json = degradation.data_json();
        assert_eq!(json["code"], "no_relevant_results");
        assert_eq!(json["severity"], "medium");
        assert!(json["repair"].is_string());
    }

    #[test]
    fn search_query_miss_classifies_no_relevant_results_first() {
        assert_eq!(
            classify_search_query_miss(RelevanceFloorCounts::new(5, 0), 0.05, None),
            Some(SearchQueryMissReason::NoRelevantResults)
        );
    }

    #[test]
    fn search_query_miss_classifies_weak_recall_above_floor() {
        assert_eq!(
            classify_search_query_miss(RelevanceFloorCounts::new(10, 10), 0.05, Some(0.07)),
            Some(SearchQueryMissReason::WeakQueryRecall)
        );
    }

    #[test]
    fn search_query_miss_classifies_low_recall_after_floor() {
        assert_eq!(
            classify_search_query_miss(RelevanceFloorCounts::new(10, 2), 0.05, Some(0.30)),
            Some(SearchQueryMissReason::LowRecallAfterFloor)
        );
        assert_eq!(
            classify_search_query_miss(RelevanceFloorCounts::new(6, 2), 0.05, Some(0.30)),
            None
        );
    }

    #[test]
    fn search_query_miss_ignores_later_visibility_and_limit_drops() {
        assert_eq!(
            classify_search_query_miss(RelevanceFloorCounts::new(50, 50), 0.05, Some(0.30)),
            None
        );
        assert_eq!(
            classify_search_query_miss(RelevanceFloorCounts::new(5, 5), 0.05, None),
            None
        );
    }

    #[test]
    fn query_assist_triggers_on_below_floor_candidate() {
        let mut candidate = synthetic_hit("mem_semantic_release", 0.02);
        candidate.metadata = Some(serde_json::json!({
            "content": "Use installer live smoke validation before publishing release artifacts.",
            "level": "procedural",
            "kind": "rule",
        }));
        let assist = build_query_assist(
            "installer validation",
            true,
            0,
            RelevanceFloorCounts::new(1, 0),
            0.05,
            None,
            1,
            &[candidate],
        )
        .expect("below-floor candidate should produce query assist");
        let json = assist.data_json(true);

        assert_eq!(json["schema"], QUERY_ASSIST_SCHEMA_V1);
        assert_eq!(json["weakResultReason"], "no_relevant_results");
        assert_eq!(json["mode"], "explain");
        assert_eq!(json["didYouMean"][0]["memoryId"], "mem_semantic_release");
        assert_eq!(
            json["didYouMean"][0]["candidateStatus"],
            "below_relevance_floor"
        );
        assert!(
            json["reformulations"]
                .as_array()
                .is_some_and(|items| !items.is_empty()),
            "near-memory content terms should produce at least one reformulation"
        );
    }

    #[test]
    fn query_assist_capture_template_is_deterministic_for_empty_results() {
        let empty_counts = RelevanceFloorCounts::new(0, 0);
        let first = build_query_assist(
            "orbital stapler protocol",
            false,
            0,
            empty_counts,
            0.05,
            None,
            0,
            &[],
        )
        .expect("empty result should offer capture template");
        let second = build_query_assist(
            "orbital stapler protocol",
            false,
            0,
            empty_counts,
            0.05,
            None,
            0,
            &[],
        )
        .expect("empty result should offer capture template");
        let first_json = first.data_json(true);
        let second_json = second.data_json(true);

        assert_eq!(first_json, second_json);
        assert_eq!(first_json["weakResultReason"], "empty_results");
        assert_eq!(first_json["didYouMean"].as_array().map(Vec::len), Some(0));
        assert_eq!(first_json["captureTemplate"]["level"], "semantic");
        assert!(
            first_json["captureTemplate"]["command"]
                .as_str()
                .is_some_and(|command| command.contains("ee remember"))
        );
    }

    #[test]
    fn query_assist_does_not_emit_for_good_results() {
        assert!(
            build_query_assist(
                "release checklist",
                false,
                3,
                RelevanceFloorCounts::new(3, 3),
                0.05,
                Some(0.30),
                0,
                &[]
            )
            .is_none()
        );
    }

    #[test]
    fn query_assist_reports_visibility_only_empty_results_honestly() {
        let assist = build_query_assist(
            "strictly scoped query",
            false,
            0,
            RelevanceFloorCounts::new(5, 5),
            0.05,
            None,
            0,
            &[],
        )
        .expect("an empty visible result set should still offer capture guidance");

        assert_eq!(assist.weak_result_reason, "empty_results");
    }

    #[test]
    fn search_query_miss_audit_details_are_hash_only_and_ttl_bounded() -> TestResult {
        let details = search_query_miss_audit_details(SearchQueryMissAuditDetails {
            query_hash: "blake3:abcdef1234567890",
            reason: SearchQueryMissReason::NoRelevantResults,
            status: SearchStatus::NoResults,
            kept: 0,
            considered: 4,
            dropped_below_floor: 4,
            floor: 0.05,
            top_score_before_floor: Some(0.01),
            top_score_after_floor: None,
        });
        let value: serde_json::Value =
            serde_json::from_str(&details).map_err(|error| error.to_string())?;

        assert_eq!(value["schema"], SEARCH_QUERY_MISS_AUDIT_SCHEMA_V1);
        assert_eq!(value["queryHash"], "blake3:abcdef1234567890");
        assert_eq!(value["reason"], "no_relevant_results");
        assert_eq!(value["status"], "no_results");
        assert_eq!(value["resultCount"], 0);
        assert_eq!(value["candidateCount"], 4);
        assert_eq!(value["droppedBelowFloor"], 4);
        assert_eq!(value["ttlSeconds"], QUERY_MISS_AUDIT_TTL_SECONDS);
        assert_eq!(value["sampling"]["sampled"], true);
        assert_eq!(value["sampling"]["maxRowsPerSearch"], 1);
        assert_eq!(value["redaction"]["rawQueryStored"], false);
        assert_eq!(value["redaction"]["queryTextStored"], false);
        assert_eq!(value["redaction"]["queryVectorStored"], false);
        assert!(
            !details.contains("secret project name"),
            "audit details must not contain raw query text"
        );
        Ok(())
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
    fn dedupe_compares_duplicate_hits_on_cross_source_relevance() {
        let hybrid = synthetic_hybrid_hit("a", RRF_HYBRID_TYPICAL_MAX * 0.80);
        let lexical = synthetic_hit("a", 0.60);

        let (deduped, collapsed) = dedupe_hits_on_doc_id(vec![hybrid, lexical]);

        assert_eq!(collapsed, 1);
        assert_eq!(deduped.len(), 1);
        assert_eq!(deduped[0].source, ScoreSource::Hybrid);
        assert!((deduped[0].relevance_score() - 0.80).abs() < 1e-6);
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
    fn dedupe_finite_score_replaces_prior_non_finite_score() {
        let hits = vec![synthetic_hit("a", f32::NAN), synthetic_hit("a", 0.4)];
        let (deduped, collapsed) = dedupe_hits_on_doc_id(hits);
        assert_eq!(deduped.len(), 1);
        assert_eq!(collapsed, 1);
        assert!(
            (deduped[0].score - 0.4).abs() < 1e-5,
            "finite duplicate must survive a prior malformed non-finite score"
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
            index_freshness: None,
            status: SearchStatus::Success,
            embed_backend: EmbedBackend::HashFallback,
            query: "cargo fmt release".to_string(),
            requested_limit: 10,
            results: deduped,
            elapsed_ms: 1.0,
            errors: Vec::new(),
            degraded: vec![SearchDegradation::mi_dedup_candidate_proposed(1)],
            runtime_profile: test_runtime_profile(),
            rerank_configured_mode: crate::config::SearchRerankMode::Auto,
            rerank_configured_top_k: DEFAULT_SEARCH_RERANK_TOP_K,
            rerank_runtime_available: false,
            relevance_floor_applied: None,
            candidates_below_floor: 0,
            query_assist: None,
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

    #[test]
    fn lexical_ram_tier_search_routes_macos_unavailable_code() {
        let degradation =
            lexical_ram_tier_search_degradation_for_code(LEXICAL_RAM_UNAVAILABLE_ON_MACOS_CODE)
                .expect("macOS lexical RAM-tier code must become a search degradation");

        assert_eq!(degradation.code, LEXICAL_RAM_UNAVAILABLE_ON_MACOS_CODE);
        assert_eq!(degradation.severity, "info");
        assert!(degradation.message.contains("Lexical RAM-tier"));
        assert!(degradation.message.contains("macOS"));
        assert!(
            degradation
                .repair
                .as_deref()
                .is_some_and(|repair| repair.contains("Linux"))
        );
    }

    #[test]
    fn lexical_ram_tier_search_config_cache_refreshes_when_workspace_config_changes() -> TestResult
    {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let config_dir = temp.path().join(".ee");
        std::fs::create_dir_all(&config_dir).map_err(|error| error.to_string())?;
        let config_path = config_dir.join("config.toml");
        std::fs::write(
            &config_path,
            "[search.lexical_ram_tier]\nenabled = true\npopulate_on_open = false\n",
        )
        .map_err(|error| error.to_string())?;

        let first = lexical_ram_tier_config_for_search(temp.path());
        if !first.enabled || first.populate_on_open {
            return Err(format!(
                "initial workspace config must drive enabled=true/populate=false; got {:?}",
                first
            ));
        }

        std::fs::write(
            &config_path,
            "[search.lexical_ram_tier]\nenabled = false\npopulate_on_open = true\nrequest_hugepages = true\n",
        )
        .map_err(|error| error.to_string())?;

        let second = lexical_ram_tier_config_for_search(temp.path());
        if second.enabled {
            return Err(format!(
                "changed workspace config must refresh cached enabled=false; got {:?}",
                second
            ));
        }
        if !second.populate_on_open {
            return Err(format!(
                "changed workspace config must refresh cached populate_on_open=true; got {:?}",
                second
            ));
        }
        if second.request_hugepages {
            return Err(format!(
                "disabled RAM tier must normalize hugepages off after cache refresh; got {:?}",
                second
            ));
        }
        Ok(())
    }

    #[test]
    fn lexical_ram_tier_search_config_cache_refreshes_same_length_preserved_mtime() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let config_dir = temp.path().join(".ee");
        std::fs::create_dir_all(&config_dir).map_err(|error| error.to_string())?;
        let config_path = config_dir.join("config.toml");
        let first_body = "[search.lexical_ram_tier]\nenabled = true\npopulate_on_open = false\n";
        let second_body = "[search.lexical_ram_tier]\nenabled = false\npopulate_on_open = true\n";
        if first_body.len() != second_body.len() {
            return Err(format!(
                "test fixtures must stay the same length: {} != {}",
                first_body.len(),
                second_body.len()
            ));
        }
        let fixed_mtime = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);

        std::fs::write(&config_path, first_body).map_err(|error| error.to_string())?;
        std::fs::OpenOptions::new()
            .read(true)
            .open(&config_path)
            .map_err(|error| error.to_string())?
            .set_times(std::fs::FileTimes::new().set_modified(fixed_mtime))
            .map_err(|error| error.to_string())?;
        let first = lexical_ram_tier_config_for_search(temp.path());
        if !first.enabled || first.populate_on_open {
            return Err(format!(
                "initial workspace config must drive enabled=true/populate=false; got {:?}",
                first
            ));
        }

        std::fs::write(&config_path, second_body).map_err(|error| error.to_string())?;
        std::fs::OpenOptions::new()
            .read(true)
            .open(&config_path)
            .map_err(|error| error.to_string())?
            .set_times(std::fs::FileTimes::new().set_modified(fixed_mtime))
            .map_err(|error| error.to_string())?;
        let second = lexical_ram_tier_config_for_search(temp.path());
        if second.enabled {
            return Err(format!(
                "same-length config rewrite with preserved mtime must refresh enabled=false; got {:?}",
                second
            ));
        }
        if !second.populate_on_open {
            return Err(format!(
                "same-length config rewrite with preserved mtime must refresh populate_on_open=true; got {:?}",
                second
            ));
        }
        Ok(())
    }
}

/// Queryless attempt-family retrieval (bd-multiplicity-aware-trust-p0u7g).
///
/// `ee search --family <id>` without a positional query is a deterministic,
/// workspace-scoped DB retrieval of every recorded family member — including
/// the rejected failures — through the normal admission pipeline rather than
/// a raw table listing: members surface only through their CURRENT live
/// revision (tombstoned and superseded rows are skipped and counted), each
/// candidate passes the same memory-scope context normal search applies
/// (scope-filtered members are counted, never leaked), and member content
/// runs through the secret redaction policy before rendering. Frankensearch
/// stays the owner of query search; this surface never touches the index.
#[derive(Clone, Copy, Debug)]
pub struct FamilyRetrievalOptions<'a> {
    pub workspace_path: &'a Path,
    pub database_path: Option<&'a Path>,
    pub family_id: &'a str,
    pub memory_scope: MemoryScope,
    pub strict_scope: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SearchFamilyMember {
    pub memory_id: String,
    pub logical_id: String,
    pub attempt_index: Option<u32>,
    pub disposition: Option<String>,
    pub discount_factor: f32,
    pub promotion_posture: String,
    pub promotion_reason: String,
    pub membership_family_aliases: Vec<String>,
    pub kind: String,
    pub level: String,
    pub trust_class: String,
    pub content: String,
    pub content_redacted: bool,
    pub provenance_uri: Option<String>,
    pub provenance_redacted: bool,
    pub created_at: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SearchFamilyReport {
    pub workspace_id: String,
    pub family_alias: String,
    pub declared_size: Option<u32>,
    pub origin: Option<String>,
    pub recorded_slots: u32,
    pub selected_count: u32,
    pub rejected_count: u32,
    pub unslotted_count: u32,
    pub duplicate_member_count: u32,
    pub unrecorded_count: u32,
    pub discount_factor: f32,
    pub survivor_only: bool,
    pub promotion_eligible: bool,
    pub promotion_posture: String,
    pub promotion_reason: String,
    pub posture_summary: String,
    pub members: Vec<SearchFamilyMember>,
    pub scope_filtered_count: u32,
    pub missing_current_revision_count: u32,
    pub memory_scope: MemoryScope,
    pub strict_scope: bool,
}

impl SearchFamilyReport {
    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        serde_json::json!({
            "schema": crate::models::schema::SEARCH_FAMILY_SCHEMA_V1,
            "workspaceId": self.workspace_id,
            "familyAlias": self.family_alias,
            "declaredSize": self.declared_size,
            "origin": self.origin,
            "recordedSlots": self.recorded_slots,
            "selectedCount": self.selected_count,
            "rejectedCount": self.rejected_count,
            "unslottedCount": self.unslotted_count,
            "duplicateMemberCount": self.duplicate_member_count,
            "unrecordedCount": self.unrecorded_count,
            "discountFactor": self.discount_factor,
            "survivorOnly": self.survivor_only,
            "promotionEligible": self.promotion_eligible,
            "promotionPosture": self.promotion_posture,
            "promotionReason": self.promotion_reason,
            "postureSummary": self.posture_summary,
            "memoryScope": self.memory_scope.as_str(),
            "strictScope": self.strict_scope,
            "scopeFilteredCount": self.scope_filtered_count,
            "missingCurrentRevisionCount": self.missing_current_revision_count,
            "members": self
                .members
                .iter()
                .map(|member| {
                    serde_json::json!({
                        "memoryId": member.memory_id,
                        "logicalId": member.logical_id,
                        "attemptIndex": member.attempt_index,
                        "disposition": member.disposition,
                        "discountFactor": member.discount_factor,
                        "promotionPosture": member.promotion_posture,
                        "promotionReason": member.promotion_reason,
                        "membershipFamilyAliases": member.membership_family_aliases,
                        "kind": member.kind,
                        "level": member.level,
                        "trustClass": member.trust_class,
                        "content": member.content,
                        "contentRedacted": member.content_redacted,
                        "provenanceUri": member.provenance_uri,
                        "provenanceRedacted": member.provenance_redacted,
                        "createdAt": member.created_at,
                    })
                })
                .collect::<Vec<_>>(),
        })
    }

    #[must_use]
    pub fn human_summary(&self) -> String {
        let mut output = format!(
            "Attempt family {}\n\n  Posture: {}\n  Promotion posture: {} ({})\n  Declared: {}\n  Recorded slots: {} \
             ({} selected, {} rejected, {} unslotted, {} unrecorded)\n  Promotion eligible: {}\n  \
             Memory scope: {}{}\n  Visible members: {}\n  Scope-filtered members: {}\n  \
             Missing live revisions: {}\n",
            self.family_alias,
            self.posture_summary,
            self.promotion_posture,
            self.promotion_reason,
            self.declared_size
                .map_or_else(|| "unknown".to_owned(), |size| size.to_string()),
            self.recorded_slots,
            self.selected_count,
            self.rejected_count,
            self.unslotted_count,
            self.unrecorded_count,
            self.promotion_eligible,
            self.memory_scope.as_str(),
            if self.strict_scope { " (strict)" } else { "" },
            self.members.len(),
            self.scope_filtered_count,
            self.missing_current_revision_count,
        );
        if self.members.is_empty() {
            output.push_str("\n  (no admitted members)\n");
            return output;
        }
        output.push_str("\nMembers:\n");
        for member in &self.members {
            output.push_str(&format!(
                "\n  {}. slot {} [{}] {}\n     Memory: {}\n     Discount: {}\n     Promotion posture: {} ({})\n     Kind/level/trust: {}/{}/{}\n     \
                 Content: {}\n",
                member.attempt_index.map_or_else(|| "?".to_owned(), |index| index.to_string()),
                member.attempt_index.map_or_else(|| "unslotted".to_owned(), |index| index.to_string()),
                member.disposition.as_deref().unwrap_or("unknown"),
                member.logical_id,
                member.memory_id,
                member.discount_factor,
                member.promotion_posture,
                member.promotion_reason,
                member.kind,
                member.level,
                member.trust_class,
                member.content.replace('\n', "\n              "),
            ));
            if let Some(provenance_uri) = member.provenance_uri.as_deref() {
                output.push_str(&format!("     Provenance: {provenance_uri}\n"));
            }
        }
        output
    }
}

/// Run the queryless family retrieval described on
/// [`FamilyRetrievalOptions`].
///
/// # Errors
///
/// Returns `DomainError::Storage` when the workspace database cannot be
/// opened or read.
pub fn run_family_retrieval(
    options: &FamilyRetrievalOptions<'_>,
) -> Result<SearchFamilyReport, DomainError> {
    let family_id = options.family_id.trim();
    if family_id.is_empty() {
        return Err(DomainError::Usage {
            message: "--family requires a non-empty family id".to_owned(),
            repair: Some("ee search --family <id> --json".to_owned()),
        });
    }
    let family_alias = crate::models::public_attempt_family_alias(family_id);
    let workspace_root = default_workspace_root(options.workspace_path);
    let requested_workspace_id = crate::core::curate::stable_workspace_id(&workspace_root);
    let default_database_path = default_workspace_database_path(options.workspace_path);
    let database_path = options.database_path.unwrap_or(&default_database_path);
    let storage_error = |message: String| DomainError::Storage {
        message,
        repair: Some("ee doctor --workspace . --json".to_owned()),
    };
    let migration_connection = DbConnection::open_file(database_path)
        .map_err(|error| storage_error(format!("Failed to open database: {error}")))?;
    migration_connection
        .migrate()
        .map_err(|error| storage_error(format!("Failed to migrate database: {error}")))?;
    drop(migration_connection);

    let read_pool = registered_process_read_pool(
        DatabaseConfig::file(database_path.to_path_buf()),
        PoolConfig::default_single(),
    );
    let read_snapshot = read_pool.pin_snapshot().map_err(|error| {
        storage_error(format!(
            "Failed to acquire attempt-family read snapshot: {error}"
        ))
    })?;
    let connection = read_snapshot.checked_connection().map_err(|error| {
        storage_error(format!(
            "Attempt-family read snapshot became unavailable: {error}"
        ))
    })?;
    let workspace_id = crate::core::workspace::select_existing_workspace_row(
        connection,
        &requested_workspace_id,
        &[options.workspace_path, workspace_root.as_path()],
    )
    .map_err(|error| storage_error(error.message()))?
    .map_or(requested_workspace_id, |workspace| workspace.id);

    let logical_ids = connection
        .list_attempt_family_membership_logical_ids(&workspace_id, family_id)
        .map_err(|error| storage_error(format!("Failed to list attempt family: {error}")))?;
    let declaration = connection
        .get_attempt_family_declaration(&workspace_id, family_id)
        .map_err(|error| {
            storage_error(format!(
                "Failed to read attempt-family declaration: {error}"
            ))
        })?;
    let (fallback_declared_size, fallback_origin) = match declaration {
        Some((declared, origin)) => (declared, Some(origin)),
        None => (None, None),
    };

    let current_ids_by_logical = connection
        .get_current_memory_ids_for_ledger_keys(&workspace_id, &logical_ids)
        .map_err(|error| {
            storage_error(format!("Failed to resolve live family revisions: {error}"))
        })?;
    let current_ids = current_ids_by_logical.values().cloned().collect::<Vec<_>>();
    let snapshot_batch = connection
        .get_attempt_family_membership_snapshots_for_memory_ids(&current_ids)
        .map_err(|error| {
            storage_error(format!(
                "Failed to load batched attempt-family membership snapshots: {error}"
            ))
        })?;
    let mut snapshots = Vec::with_capacity(current_ids.len());
    let mut target_family_snapshot = None;
    let mut multi_family_membership = false;
    for (logical_id, current_id) in &current_ids_by_logical {
        let snapshot = snapshot_batch.by_memory_id.get(current_id).ok_or_else(|| {
            storage_error(format!(
                "Batched attempt-family snapshot omitted live memory `{current_id}`"
            ))
        })?;
        let Some(target_family) = snapshot.family(family_id) else {
            return Err(storage_error(format!(
                "Attempt-family membership snapshot omitted requested family `{family_alias}` for logical memory `{logical_id}`"
            )));
        };
        if target_family_snapshot.is_none() {
            target_family_snapshot = Some(target_family.clone());
        }
        multi_family_membership |= snapshot.families.len() > 1;
        snapshots.push((logical_id.clone(), current_id.clone(), snapshot.clone()));
    }

    let multiplicity = target_family_snapshot.as_ref().map_or_else(
        || {
            crate::models::AttemptFamilyMultiplicity::from_members(
                family_id.to_owned(),
                fallback_declared_size,
                std::iter::empty(),
            )
        },
        crate::db::AttemptFamilySnapshot::multiplicity,
    );
    let origin = target_family_snapshot
        .as_ref()
        .and_then(|family| family.origin.clone())
        .or(fallback_origin);

    let scope_context = crate::core::memory_scope::MemoryScopeContext::for_workspace(
        &workspace_root,
        options.memory_scope,
        options.strict_scope,
    );
    let mut admitted = Vec::new();
    let mut scope_filtered_count = 0_u32;
    let missing_current_revision_count = u32::try_from(
        logical_ids
            .len()
            .saturating_sub(current_ids_by_logical.len()),
    )
    .unwrap_or(u32::MAX);
    let mut memories_by_id = BTreeMap::new();
    let mut tags_by_id = BTreeMap::new();
    for chunk in current_ids.chunks(crate::db::ATTEMPT_FAMILY_MEMBERSHIP_BATCH_SIZE) {
        let refs = chunk.iter().map(String::as_str).collect::<Vec<_>>();
        memories_by_id.extend(connection.get_memories_batch(&refs).map_err(|error| {
            storage_error(format!("Failed to load batched family members: {error}"))
        })?);
        tags_by_id.extend(connection.get_memory_tags_batch(&refs).map_err(|error| {
            storage_error(format!(
                "Failed to load batched family member tags: {error}"
            ))
        })?);
    }
    for (logical_id, current_id, snapshot) in &snapshots {
        let target_family = snapshot.family(family_id).ok_or_else(|| {
            storage_error(format!(
                "Attempt-family membership snapshot omitted requested family `{family_alias}` for logical memory `{logical_id}`"
            ))
        })?;
        let memory = memories_by_id.get(current_id).ok_or_else(|| {
            storage_error(format!(
                "Batched family member load omitted live memory `{current_id}`"
            ))
        })?;
        let tags = tags_by_id.get(current_id).map_or(&[][..], Vec::as_slice);
        if !scope_context.memory_in_scope_with_tags(&memory, &tags) {
            scope_filtered_count = scope_filtered_count.saturating_add(1);
            continue;
        }
        let redaction = crate::policy::redact_secret_like_content(&memory.content);
        let (provenance_uri, provenance_redacted) =
            memory.provenance_uri.as_ref().map_or((None, false), |uri| {
                let mut redacted_patterns = BTreeSet::new();
                let redacted_uri = redact_search_provenance_uri(uri, true, &mut redacted_patterns);
                let redacted = redacted_uri.as_str() != uri;
                (Some(redacted_uri), redacted)
            });
        let membership_posture = snapshot
            .promotion_posture()
            .unwrap_or(crate::models::AttemptFamilyPromotionPosture::BlockedUndeclared);
        let mut membership_records = target_family
            .ledger_members
            .iter()
            .filter(|member| member.memory_logical_id == *logical_id)
            .map(|member| (Some(member.attempt_index), Some(member.disposition.clone())))
            .collect::<Vec<_>>();
        if target_family
            .pointer_only_logical_ids
            .iter()
            .any(|pointer_id| pointer_id == logical_id)
        {
            membership_records.push((None, None));
        }
        for (attempt_index, disposition) in membership_records {
            admitted.push(SearchFamilyMember {
                memory_id: memory.id.clone(),
                logical_id: logical_id.clone(),
                attempt_index,
                discount_factor: snapshot.member_discount_factor(family_id, disposition.as_deref()),
                disposition,
                promotion_posture: membership_posture.as_str().to_owned(),
                promotion_reason: membership_posture.reason().to_owned(),
                membership_family_aliases: snapshot
                    .family_ids()
                    .iter()
                    .map(|family_id| crate::models::public_attempt_family_alias(family_id))
                    .collect(),
                kind: memory.kind.clone(),
                level: memory.level.clone(),
                trust_class: memory.trust_class.clone(),
                content: redaction.content.clone(),
                content_redacted: redaction.redacted,
                provenance_uri: provenance_uri.clone(),
                provenance_redacted,
                created_at: memory.created_at.clone(),
            });
        }
    }

    admitted.sort_by(|left, right| {
        left.attempt_index
            .unwrap_or(u32::MAX)
            .cmp(&right.attempt_index.unwrap_or(u32::MAX))
            .then_with(|| left.logical_id.cmp(&right.logical_id))
            .then_with(|| left.memory_id.cmp(&right.memory_id))
    });

    if options.strict_scope && scope_filtered_count > 0 {
        // Mirror normal search's strict-scope contract: once any relevant
        // family member falls outside the requested trust lane, disclose no
        // family members from the mixed-scope result.
        admitted.clear();
    }

    let promotion_posture = if multi_family_membership {
        crate::models::AttemptFamilyPromotionPosture::BlockedMultipleFamilies
    } else {
        multiplicity.promotion_posture()
    };

    let report = SearchFamilyReport {
        workspace_id,
        family_alias,
        declared_size: multiplicity.declared_size,
        origin,
        recorded_slots: multiplicity.recorded_slots,
        selected_count: multiplicity.selected_count,
        rejected_count: multiplicity.rejected_count,
        unslotted_count: multiplicity.unslotted_count,
        duplicate_member_count: multiplicity.duplicate_member_count,
        unrecorded_count: multiplicity.unrecorded_count(),
        discount_factor: multiplicity.discount_factor(),
        survivor_only: multiplicity.is_survivor_only(),
        promotion_eligible: matches!(
            promotion_posture,
            crate::models::AttemptFamilyPromotionPosture::Eligible
        ),
        promotion_posture: promotion_posture.as_str().to_owned(),
        promotion_reason: promotion_posture.reason().to_owned(),
        posture_summary: multiplicity.summary(),
        members: admitted,
        scope_filtered_count,
        missing_current_revision_count,
        memory_scope: options.memory_scope,
        strict_scope: options.strict_scope,
    };
    read_snapshot.commit().map_err(|error| {
        storage_error(format!(
            "Failed to release attempt-family read snapshot: {error}"
        ))
    })?;
    Ok(report)
}
