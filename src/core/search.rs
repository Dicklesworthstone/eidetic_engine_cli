use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use super::index::{
    IndexHealth, IndexStatusError, IndexStatusOptions, IndexStatusReport, get_index_status,
};
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
}

impl SearchOptions {
    fn resolve_index_dir(&self) -> PathBuf {
        self.index_dir
            .clone()
            .unwrap_or_else(|| self.workspace_path.join(".ee").join(DEFAULT_INDEX_SUBDIR))
    }

    fn two_tier_config(&self) -> TwoTierConfig {
        let mut config = TwoTierConfig::default();
        let requested = usize::try_from(self.limit).unwrap_or(usize::MAX).max(1);
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
        RetrievalMetrics::from_hits(
            self.requested_limit,
            self.elapsed_ms,
            &self.results,
            self.errors.len(),
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

pub fn run_search(options: &SearchOptions) -> Result<SearchReport, SearchError> {
    let start = Instant::now();
    let index_dir = options.resolve_index_dir();

    if !index_dir.exists() {
        return Err(SearchError::NoIndex);
    }

    let degraded = search_degradations(options, &index_dir);

    let search_result = search_sync(
        &index_dir,
        &options.query,
        options.limit as usize,
        options.two_tier_config(),
        options.explain,
    );

    let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;

    match search_result {
        Ok((hits, errors)) => {
            let status = if hits.is_empty() {
                SearchStatus::NoResults
            } else {
                SearchStatus::Success
            };

            Ok(SearchReport {
                status,
                query: options.query.clone(),
                requested_limit: options.limit,
                results: hits,
                elapsed_ms,
                errors,
                degraded,
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
}
