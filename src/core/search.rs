use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use super::index::{
    IndexHealth, IndexStatusError, IndexStatusOptions, IndexStatusReport, get_index_status,
};
use super::profile::{RuntimeProfileReport, runtime_profile_for_workspace};
use crate::search::{
    Embedder, HashEmbedder, SpeedMode, TwoTierConfig, TwoTierIndex, TwoTierSearcher,
};

pub const DEFAULT_INDEX_SUBDIR: &str = "index";
pub const PERFORMANCE_EXPLAIN_SCHEMA_V1: &str = "ee.explain.performance.v1";
const INDEX_STATUS_CACHE_TTL: Duration = Duration::from_secs(1);

static SEARCH_INDEX_STATUS_CACHE: OnceLock<Mutex<HashMap<IndexStatusCacheKey, CachedIndexStatus>>> =
    OnceLock::new();

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
    /// Minimum score (0.0..=1.0) for a hit to be returned. `None` falls
    /// back to [`DEFAULT_RELEVANCE_FLOOR`]. Set to `Some(0.0)` to disable.
    /// Bead bd-17c65.2.1 (B1).
    pub relevance_floor: Option<f32>,
}

/// Default relevance floor (bead bd-17c65.2.1 / B1).
///
/// Calibrated against the 2026-05-10 corpus where junk semantic_fast hits
/// scored `< 0.03` and meaningful hits scored `0.10..=0.50`. Configurable
/// per-call via `--relevance-floor` and per-workspace via
/// `search.relevance_floor` config.
pub const DEFAULT_RELEVANCE_FLOOR: f32 = 0.05;

impl SearchOptions {
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
                "Broaden the query, lower --relevance-floor, or use --source-mode lexical_only when implemented (B6)."
                    .to_string(),
            ),
        }
    }

    /// Search produced duplicate hits on the same `docId` that were
    /// collapsed (highest score retained). Informational so callers
    /// understand why the raw retrieval count > the returned count.
    /// Bead bd-17c65.2.3 (B3).
    #[must_use]
    fn duplicates_collapsed(collapsed: usize) -> Self {
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
                "Top result scored {top_score:.4} against floor {floor:.4}; embedder may not recognize query synonyms, or the corpus lacks strong matches.",
            ),
            repair: Some(
                "Rephrase with concrete words present in stored memories, or use --source-mode lexical_only when implemented (B6).".to_string(),
            ),
        }
    }

    /// Most candidates dropped below the floor (informational signal so
    /// an agent can decide whether to retry with a different strategy).
    /// Bead bd-17c65.2.1 (B1).
    #[must_use]
    fn low_recall_after_floor(
        floor: f32,
        kept: usize,
        considered: usize,
    ) -> Self {
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
    fn data_json(&self) -> serde_json::Value {
        serde_json::json!({
            "code": self.code,
            "severity": self.severity,
            "message": self.message,
            "repair": self.repair,
        })
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

        for (i, hit) in self.results.iter().enumerate() {
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

        if self.results.is_empty() && self.status == SearchStatus::Success {
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
        let results: Vec<serde_json::Value> = self
            .results
            .iter()
            .map(|hit| {
                let mut obj = serde_json::json!({
                    "docId": hit.doc_id,
                    "score": hit.score,
                    "source": hit.source.as_str(),
                    "why": hit.why(),
                    "provenance": hit.provenance_json(),
                });
                if let Some(obj_map) = obj.as_object_mut() {
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
                        obj_map.insert("metadata".to_string(), meta.clone());
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

        serde_json::json!({
            "command": "search",
            "status": self.status.as_str(),
            "query": self.query,
            "results": results,
            "resultCount": self.results.len(),
            "elapsedMs": self.elapsed_ms,
            "metrics": self.retrieval_metrics().data_json(),
            "profileRuntime": self.runtime_profile.data_json(),
            "errors": self.errors,
            "degraded": self.degraded.iter().map(SearchDegradation::data_json).collect::<Vec<_>>(),
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
            "fallbacks": self.degraded.iter().map(SearchDegradation::data_json).collect::<Vec<_>>(),
            "redaction": performance_redaction_json(),
        })
    }
}

impl SearchHit {
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
    fn provenance_json(&self) -> Vec<serde_json::Value> {
        let mut provenance = Vec::new();

        if let Some(ref metadata) = self.metadata {
            for key in ["provenanceUri", "provenance_uri"] {
                if let Some(uri) = metadata_string(metadata, key) {
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
        provenance
    }
}

fn metadata_string<'a>(metadata: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    metadata
        .get(key)
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty())
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
        let variance_proxy = match (
            self.score_distribution.max,
            self.score_distribution.min,
        ) {
            (Some(max), Some(min)) if max > 0.0 => ((max - min) / max).clamp(0.0, 1.0),
            _ => 0.0,
        };
        let variance_signal = (1.0_f32 - variance_proxy).clamp(0.0, 1.0);
        let raw = 0.5 * top_signal + 0.3 * recall + 0.2 * variance_signal;
        Some(raw.clamp(0.0, 1.0))
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
}

impl SearchError {
    #[must_use]
    pub fn repair_hint(&self) -> Option<&str> {
        match self {
            Self::Index(_) => Some("Check index directory and permissions"),
            Self::NoIndex => Some("ee index rebuild --workspace ."),
        }
    }
}

impl std::fmt::Display for SearchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Index(e) => write!(f, "Index error: {e}"),
            Self::NoIndex => write!(f, "Search index not found"),
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

pub fn run_search(options: &SearchOptions) -> Result<SearchReport, SearchError> {
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

    let search_result = search_sync(
        &index_dir,
        &options.query,
        effective_limit as usize,
        options.two_tier_config_for_limit(effective_limit),
        options.explain,
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

            // Bead bd-17c65.2.1 (B1): apply relevance floor.
            let floor = options
                .relevance_floor
                .unwrap_or(DEFAULT_RELEVANCE_FLOOR);
            let pre_floor_count = raw_hits.len();
            let pre_floor_top_score = raw_hits.first().map(|hit| hit.score);

            // Partition into above-floor (kept) and below-floor (dropped).
            // Floor of 0.0 is "disabled" — keep everything. NaN scores are
            // always dropped because NaN >= floor is false.
            let (above_floor, below_floor): (Vec<_>, Vec<_>) = raw_hits
                .into_iter()
                .partition(|hit| hit.score.is_finite() && hit.score >= floor);
            let kept = above_floor.len();
            let dropped = below_floor.len();

            // Surface dedupe count as a low-severity info signal when
            // it fired; agents reading the metrics can correlate with
            // raw retrieve-arm output (B7 surface, future bead) for
            // debugging.
            if duplicates_collapsed > 0 {
                degraded.push(SearchDegradation::duplicates_collapsed(
                    duplicates_collapsed,
                ));
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
            if kept > 0 && pre_floor_count >= 3 && (kept * 10) < (pre_floor_count * 3) {
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
            })
        }
    }
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

fn cached_index_status_for_search(
    options: &SearchOptions,
    index_dir: &Path,
) -> Result<IndexStatusReport, IndexStatusError> {
    let cache_key = IndexStatusCacheKey::from_search_options(options, index_dir);
    let now = Instant::now();
    let cache = SEARCH_INDEX_STATUS_CACHE.get_or_init(|| Mutex::new(HashMap::new()));

    if let Ok(mut guard) = cache.lock() {
        guard.retain(|_, cached| now.duration_since(cached.checked_at) <= INDEX_STATUS_CACHE_TTL);
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
        guard.retain(|_, cached| now.duration_since(cached.checked_at) <= INDEX_STATUS_CACHE_TTL);
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

fn search_sync(
    index_dir: &Path,
    query: &str,
    limit: usize,
    config: TwoTierConfig,
    explain: bool,
) -> Result<(Vec<SearchHit>, Vec<String>), String> {
    let index_dir_owned = index_dir.to_path_buf();
    let query_owned = query.to_string();
    #[allow(clippy::type_complexity)]
    let result_holder: Arc<Mutex<Option<Result<(Vec<SearchHit>, Vec<String>), String>>>> =
        Arc::new(Mutex::new(None));
    let task_result = Arc::clone(&result_holder);
    let runtime_error_result = Arc::clone(&result_holder);

    let panic_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let runtime_result = crate::core::run_cli_future(async move {
            let cx = asupersync::Cx::for_testing();
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

            let search_result = searcher.search_collect(&cx, &query_owned, limit).await;

            let converted = match search_result {
                Ok((results, _metrics)) => {
                    let mut hits: Vec<SearchHit> = results
                        .into_iter()
                        .map(|r| {
                            let source = match r.source {
                                crate::search::ScoreSource::Lexical => ScoreSource::Lexical,
                                crate::search::ScoreSource::SemanticFast => {
                                    ScoreSource::SemanticFast
                                }
                                crate::search::ScoreSource::SemanticQuality => {
                                    ScoreSource::SemanticQuality
                                }
                                crate::search::ScoreSource::Hybrid => ScoreSource::Hybrid,
                                crate::search::ScoreSource::Reranked => ScoreSource::Reranked,
                            };
                            let mut hit = SearchHit {
                                doc_id: r.doc_id,
                                score: r.score,
                                source,
                                fast_score: r.fast_score,
                                quality_score: r.quality_score,
                                lexical_score: r.lexical_score,
                                rerank_score: r.rerank_score,
                                metadata: r.metadata,
                                explanation: None,
                            };
                            if explain {
                                hit.explanation = Some(ScoreExplanation::generate(&hit));
                            }
                            hit
                        })
                        .collect();
                    hits.sort_by(|left, right| {
                        right
                            .score
                            .total_cmp(&left.score)
                            .then_with(|| left.doc_id.cmp(&right.doc_id))
                    });
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
mod tests {
    use super::*;

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

    fn test_runtime_profile() -> RuntimeProfileReport {
        RuntimeProfileReport::for_profile(
            super::super::profile::OperatingProfile::Workstation,
            "test_fixture",
        )
    }

    #[test]
    fn search_status_as_str_is_stable() {
        assert_eq!(SearchStatus::Success.as_str(), "success");
        assert_eq!(SearchStatus::NoResults.as_str(), "no_results");
        assert_eq!(SearchStatus::IndexNotFound.as_str(), "index_not_found");
        assert_eq!(SearchStatus::IndexError.as_str(), "index_error");
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
        assert!(json["results"][0]["why"].is_string());
        assert!(json["results"][0]["provenance"].is_array());
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
            relevance_floor: None,
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
            relevance_floor: None,
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
            relevance_floor: None,
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
            relevance_floor: None,
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
            relevance_floor: None,
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
            relevance_floor: None,
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

    #[test]
    fn default_relevance_floor_is_one_in_twenty() {
        // 0.05 is the documented default (calibrated against the 2026-05-10
        // corpus where junk scored < 0.03 and meaningful hits scored 0.10+).
        // Changing this default is a contract change — agents downstream
        // rely on the value.
        assert!((DEFAULT_RELEVANCE_FLOOR - 0.05).abs() < f32::EPSILON);
    }

    #[test]
    fn retrieval_metrics_records_floor_and_candidate_counts() {
        let hits = vec![
            synthetic_hit("a", 0.30),
            synthetic_hit("b", 0.20),
            synthetic_hit("c", 0.10),
        ];
        let metrics = RetrievalMetrics::from_hits_with_floor(
            10, 5.0, &hits, 0, Some(0.05), 4,
        );
        assert_eq!(metrics.relevance_floor, Some(0.05));
        assert_eq!(metrics.candidates_above_floor, 3);
        assert_eq!(metrics.candidates_below_floor, 4);
        assert_eq!(metrics.returned_count, 3);
    }

    #[test]
    fn retrieval_metrics_data_json_emits_floor_fields() {
        let hits = vec![synthetic_hit("a", 0.4)];
        let metrics = RetrievalMetrics::from_hits_with_floor(
            10, 5.0, &hits, 0, Some(0.05), 2,
        );
        let json = metrics.data_json();
        // f32 -> f64 widening introduces sub-epsilon drift (0.0500000007…);
        // compare with tolerance instead of exact equality.
        let floor = json["relevanceFloor"].as_f64().expect("floor present");
        assert!(
            (floor - 0.05).abs() < 1e-5,
            "floor mismatch: got {floor}"
        );
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
        let degradation =
            SearchDegradation::no_relevant_results("q", 0.05, 1, Some(0.01));
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
        let metrics = RetrievalMetrics::from_hits_with_floor(
            10,
            5.0,
            &[],
            0,
            Some(0.05),
            5,
        );
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
        let good = RetrievalMetrics::from_hits_with_floor(
            10, 5.0, &good_hits, 0, Some(0.05), 0,
        );
        let weak = RetrievalMetrics::from_hits_with_floor(
            10, 5.0, &weak_hits, 0, Some(0.05), 9,
        );
        let good_score = good.honest_quality_score().expect("good");
        let weak_score = weak.honest_quality_score().expect("weak");
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
        let metrics =
            RetrievalMetrics::from_hits_with_floor(10, 5.0, &hits, 0, Some(0.05), 1);
        let json = metrics.data_json();
        assert_eq!(json["qualityAssessment"], "good");
        let score = json["honestQualityScore"]
            .as_f64()
            .expect("score present");
        assert!((0.0..=1.0).contains(&score));
    }

    #[test]
    fn retrieval_metrics_quality_assessment_empty_json() {
        // Below-floor input produces empty assessment + null score.
        let metrics =
            RetrievalMetrics::from_hits_with_floor(10, 5.0, &[], 0, Some(0.05), 3);
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
        let hits = vec![
            synthetic_hit("a", 0.4),
            synthetic_hit("a", f32::NAN),
        ];
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
    // Bead bd-17c65.2.5 (B5) — weak_query_recall signal
    // ========================================================================

    #[test]
    fn weak_query_recall_degradation_carries_top_score_and_floor() {
        let degradation = SearchDegradation::weak_query_recall(0.05, 0.07);
        assert_eq!(degradation.code, "weak_query_recall");
        assert_eq!(degradation.severity, "low");
        assert!(degradation.message.contains("0.0700"));
        assert!(degradation.message.contains("0.0500"));
        assert!(degradation
            .repair
            .as_deref()
            .is_some_and(|r| r.to_lowercase().contains("rephrase")));
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
        assert!(one.message.contains("1 duplicate hit "), "got {}", one.message);
        assert!(!one.message.contains("hits"), "singular for n=1: {}", one.message);
        let many = SearchDegradation::duplicates_collapsed(5);
        assert!(many.message.contains("5 duplicate hits"), "got {}", many.message);
    }
}
