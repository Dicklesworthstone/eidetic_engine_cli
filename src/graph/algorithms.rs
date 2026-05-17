use std::any::Any;
use std::collections::{BTreeMap, HashMap};
use std::panic::AssertUnwindSafe;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use asupersync::Cx;
use chrono::{DateTime, Utc};
use fnx_algorithms::{PageRankResult, pagerank_with_params};
use fnx_classes::digraph::DiGraph;
use fnx_runtime::{CgsePolicyEngine, CgseValue, CompatibilityMode};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::core::graph_telemetry::{
    AlgorithmCancelledEvent, AlgorithmComputeEvent, AlgorithmTimeoutEvent, CacheEvictEvent,
    CacheEvictReason, CacheOutcomeEvent, emit_algorithm_cancelled, emit_algorithm_compute,
    emit_algorithm_timeout, emit_cache_evict, emit_cache_hit, emit_cache_miss,
};
use crate::db::{CreateGraphAlgorithmResultInput, DbConnection, StoredGraphAlgorithmResult};
use crate::graph::{GraphError, GraphResult, graph_algorithm_params_hash};

pub const DEFAULT_PPR_ALPHA: f64 = 0.30;
pub const DEFAULT_PAGERANK_MAX_ITERATIONS: usize = 100;
pub const DEFAULT_PAGERANK_TOLERANCE: f64 = 1.0e-6;
pub const DEFAULT_SAMPLE_THRESHOLD: usize = 500;
pub const DEFAULT_SAMPLE_SIZE: usize = 100;
pub const DEFAULT_FOREGROUND_BUDGET: Duration = Duration::from_millis(250);
pub const DEFAULT_BACKGROUND_BUDGET: Duration = Duration::from_millis(2_000);
pub const DEFAULT_CGSE_MODE: CompatibilityMode = CompatibilityMode::Strict;
const CANCELLATION_POLL_INTERVAL: Duration = Duration::from_millis(10);
const UNTRACKED_GRAPH_SNAPSHOT_ID: &str = "untracked";
const UNTRACKED_GRAPH_PARAMS_HASH: &str = "untracked";

#[must_use]
pub fn current_or_testing_cx() -> Cx {
    Cx::current().unwrap_or_else(Cx::for_testing)
}

pub fn run_with_budget<R, F>(cx: &Cx, name: &'static str, budget: Duration, f: F) -> GraphResult<R>
where
    R: Send + 'static,
    F: FnOnce() -> R + Send + 'static,
{
    run_with_budget_observed(
        cx,
        name,
        budget,
        BudgetTelemetry {
            snapshot_id: UNTRACKED_GRAPH_SNAPSHOT_ID,
            params_hash: UNTRACKED_GRAPH_PARAMS_HASH,
            emit_compute: true,
            cache_hit: false,
            sampling_used: false,
        },
        f,
    )
}

fn run_with_budget_observed<R, F>(
    cx: &Cx,
    name: &'static str,
    budget: Duration,
    telemetry: BudgetTelemetry<'_>,
    f: F,
) -> GraphResult<R>
where
    R: Send + 'static,
    F: FnOnce() -> R + Send + 'static,
{
    let started = Instant::now();
    if let Err(error) = check_cancelled(cx, name) {
        emit_budget_failure_telemetry(name, budget, started, telemetry, &error);
        return Err(error);
    }

    let runtime = asupersync::runtime::RuntimeBuilder::current_thread()
        .thread_name_prefix("ee-graph-budget")
        .build()
        .map_err(|error| GraphError::GraphEngine {
            operation: "start graph budget runtime",
            source: error.to_string(),
        })?;

    let outcome = runtime.block_on(async {
        let mut worker = std::pin::pin!(asupersync::runtime::spawn_blocking(move || {
            std::panic::catch_unwind(AssertUnwindSafe(f))
        }));
        loop {
            check_cancelled(cx, name)?;
            let Some(remaining) = budget.checked_sub(started.elapsed()) else {
                return Err(GraphError::AlgorithmTimeout {
                    algorithm: name.to_owned(),
                    timeout_ms: duration_millis_saturating(budget),
                });
            };
            if remaining.is_zero() {
                return Err(GraphError::AlgorithmTimeout {
                    algorithm: name.to_owned(),
                    timeout_ms: duration_millis_saturating(budget),
                });
            }

            let poll_budget = remaining.min(CANCELLATION_POLL_INTERVAL);
            if let Ok(result) = asupersync::time::timeout(
                asupersync::time::wall_now(),
                poll_budget,
                worker.as_mut(),
            )
            .await
            {
                return Ok(result);
            }
        }
    });

    let result = match outcome {
        Ok(Ok(result)) => Ok(result),
        Ok(Err(payload)) => Err(GraphError::GraphEngine {
            operation: name,
            source: format!(
                "graph algorithm worker panicked: {}",
                panic_payload_to_string(payload)
            ),
        }),
        Err(error) => Err(error),
    };

    match &result {
        Ok(_) if telemetry.emit_compute => {
            emit_algorithm_compute(AlgorithmComputeEvent {
                algorithm: name,
                snapshot_id: telemetry.snapshot_id,
                params_hash: telemetry.params_hash,
                elapsed_ms: duration_millis_saturating(started.elapsed()),
                cache_hit: telemetry.cache_hit,
                sampling_used: telemetry.sampling_used,
            });
        }
        Err(error) => emit_budget_failure_telemetry(name, budget, started, telemetry, error),
        Ok(_) => {}
    }

    result
}

#[derive(Clone, Copy, Debug)]
struct BudgetTelemetry<'a> {
    snapshot_id: &'a str,
    params_hash: &'a str,
    emit_compute: bool,
    cache_hit: bool,
    sampling_used: bool,
}

fn emit_budget_failure_telemetry(
    name: &'static str,
    budget: Duration,
    started: Instant,
    telemetry: BudgetTelemetry<'_>,
    error: &GraphError,
) {
    match error {
        GraphError::AlgorithmTimeout { .. } => emit_algorithm_timeout(AlgorithmTimeoutEvent {
            algorithm: name,
            snapshot_id: telemetry.snapshot_id,
            budget_ms: duration_millis_saturating(budget),
            elapsed_ms: duration_millis_saturating(started.elapsed()),
        }),
        GraphError::AlgorithmCancelled { .. } => {
            emit_algorithm_cancelled(AlgorithmCancelledEvent {
                algorithm: name,
                elapsed_ms: duration_millis_saturating(started.elapsed()),
            });
        }
        _ => {}
    }
}

pub fn run_with_cached_budget<R, F>(
    cx: &Cx,
    spec: &AlgorithmResultCacheSpec<'_>,
    name: &'static str,
    budget: Duration,
    f: F,
) -> GraphResult<AlgorithmResultCacheRun<R>>
where
    R: Clone + DeserializeOwned + Send + Serialize + Sync + 'static,
    F: FnOnce() -> R + Send + 'static,
{
    let params_hash =
        graph_algorithm_params_hash(spec.algorithm, spec.snapshot_content_hash, spec.params)?;
    let started = Instant::now();
    let run = run_with_result_cache_with_params_hash(spec, &params_hash, || {
        run_with_budget_observed(
            cx,
            name,
            budget,
            BudgetTelemetry {
                snapshot_id: spec.snapshot_id,
                params_hash: &params_hash,
                emit_compute: false,
                cache_hit: false,
                sampling_used: false,
            },
            f,
        )
    })?;
    emit_algorithm_compute(AlgorithmComputeEvent {
        algorithm: name,
        snapshot_id: spec.snapshot_id,
        params_hash: &run.params_hash,
        elapsed_ms: duration_millis_saturating(started.elapsed()),
        cache_hit: run.cache_hit,
        sampling_used: false,
    });
    Ok(run)
}

pub fn with_cgse_mode<R, F>(mode: CompatibilityMode, f: F) -> R
where
    F: FnOnce(CgsePolicyEngine) -> R,
{
    f(CgsePolicyEngine::new(mode))
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PprPolicy {
    pub alpha: f64,
}

impl PprPolicy {
    #[must_use]
    pub fn from_optional_config(alpha: Option<f64>) -> Self {
        Self {
            alpha: alpha.unwrap_or(DEFAULT_PPR_ALPHA),
        }
    }
}

impl Default for PprPolicy {
    fn default() -> Self {
        Self {
            alpha: DEFAULT_PPR_ALPHA,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SamplingPolicy {
    pub sample_threshold: usize,
    pub sample_size: usize,
}

impl SamplingPolicy {
    #[must_use]
    pub const fn new(sample_threshold: usize, sample_size: usize) -> Self {
        Self {
            sample_threshold,
            sample_size,
        }
    }

    #[must_use]
    pub fn from_optional_sample_config(
        sample_threshold: Option<u64>,
        sample_size: Option<u64>,
    ) -> Self {
        Self {
            sample_threshold: sample_threshold
                .map(u64_to_usize_saturating)
                .unwrap_or(DEFAULT_SAMPLE_THRESHOLD),
            sample_size: sample_size
                .map(u64_to_usize_saturating)
                .unwrap_or(DEFAULT_SAMPLE_SIZE),
        }
    }
}

impl Default for SamplingPolicy {
    fn default() -> Self {
        Self {
            sample_threshold: DEFAULT_SAMPLE_THRESHOLD,
            sample_size: DEFAULT_SAMPLE_SIZE,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SamplingChoice {
    Exact,
    Approximate,
}

impl SamplingChoice {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::Approximate => "approximate",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SamplingWitness {
    pub algorithm: String,
    pub snapshot_version: u64,
    pub node_count: usize,
    pub sample_threshold: usize,
    pub requested_sample_size: usize,
    pub effective_sample_size: usize,
    pub choice: SamplingChoice,
    pub seed: u64,
    pub pivots: Vec<usize>,
    pub decision_path_hash: String,
}

impl SamplingWitness {
    #[must_use]
    pub fn to_cgse_value(&self) -> CgseValue {
        let mut fields = BTreeMap::new();
        fields.insert(
            "algorithm".to_owned(),
            CgseValue::String(self.algorithm.clone()),
        );
        fields.insert(
            "choice".to_owned(),
            CgseValue::String(self.choice.as_str().to_owned()),
        );
        fields.insert(
            "decisionPathHash".to_owned(),
            CgseValue::String(self.decision_path_hash.clone()),
        );
        fields.insert(
            "effectiveSampleSize".to_owned(),
            cgse_usize(self.effective_sample_size),
        );
        fields.insert("nodeCount".to_owned(), cgse_usize(self.node_count));
        fields.insert(
            "requestedSampleSize".to_owned(),
            cgse_usize(self.requested_sample_size),
        );
        fields.insert(
            "pivots".to_owned(),
            CgseValue::String(
                self.pivots
                    .iter()
                    .map(|pivot| pivot.to_string())
                    .collect::<Vec<_>>()
                    .join(","),
            ),
        );
        fields.insert(
            "sampleThreshold".to_owned(),
            cgse_usize(self.sample_threshold),
        );
        fields.insert("seed".to_owned(), cgse_u64(self.seed));
        fields.insert(
            "snapshotVersion".to_owned(),
            cgse_u64(self.snapshot_version),
        );
        CgseValue::Map(fields)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SamplingRun<R> {
    pub result: R,
    pub witness: SamplingWitness,
}

impl<R> SamplingRun<R> {
    #[must_use]
    pub fn into_result(self) -> R {
        self.result
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AlgorithmResultCacheRun<R> {
    pub result: R,
    pub params_hash: String,
    pub cache_hit: bool,
}

impl<R> AlgorithmResultCacheRun<R> {
    #[must_use]
    pub fn into_result(self) -> R {
        self.result
    }
}

#[derive(Clone, Copy)]
pub struct AlgorithmResultCacheSpec<'a> {
    pub conn: &'a DbConnection,
    pub workspace_id: &'a str,
    pub snapshot_id: &'a str,
    pub snapshot_content_hash: &'a str,
    pub algorithm: &'a str,
    pub params: &'a serde_json::Value,
    pub ttl_seconds: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct CachedComputation<R> {
    result: R,
    cache_hit: bool,
}

static ALGORITHM_CACHE_LOCKS: OnceLock<Mutex<HashMap<String, Arc<Mutex<()>>>>> = OnceLock::new();
static IN_MEMORY_ALGORITHM_RESULTS: OnceLock<Mutex<HashMap<String, InMemoryAlgorithmResult>>> =
    OnceLock::new();

#[derive(Clone)]
struct InMemoryAlgorithmResult {
    result: Arc<dyn Any + Send + Sync>,
    expires_at: Option<Instant>,
}

pub fn run_with_result_cache<R, Compute>(
    spec: &AlgorithmResultCacheSpec<'_>,
    compute: Compute,
) -> GraphResult<AlgorithmResultCacheRun<R>>
where
    R: Clone + DeserializeOwned + Send + Serialize + Sync + 'static,
    Compute: FnOnce() -> GraphResult<R>,
{
    let params_hash =
        graph_algorithm_params_hash(spec.algorithm, spec.snapshot_content_hash, spec.params)?;
    run_with_result_cache_with_params_hash(spec, &params_hash, compute)
}

pub(crate) fn run_with_result_cache_with_params_hash<R, Compute>(
    spec: &AlgorithmResultCacheSpec<'_>,
    params_hash: &str,
    compute: Compute,
) -> GraphResult<AlgorithmResultCacheRun<R>>
where
    R: Clone + DeserializeOwned + Send + Serialize + Sync + 'static,
    Compute: FnOnce() -> GraphResult<R>,
{
    let cache_key = format!(
        "{}\0{}\0{}\0{}",
        spec.workspace_id, spec.snapshot_id, spec.algorithm, params_hash
    );
    let cached = compute_or_load_algorithm_result(
        &cache_key,
        || {
            let loaded = load_cached_algorithm_result_with_memory(spec, params_hash, &cache_key)?;
            if loaded.is_some() {
                emit_cache_hit(CacheOutcomeEvent {
                    algorithm: spec.algorithm,
                    params_hash,
                });
            }
            Ok(loaded)
        },
        || {
            emit_cache_miss(CacheOutcomeEvent {
                algorithm: spec.algorithm,
                params_hash,
            });
            compute()
        },
        |result| store_cached_algorithm_result_with_memory(spec, params_hash, &cache_key, result),
    )?;

    Ok(AlgorithmResultCacheRun {
        result: cached.result,
        params_hash: params_hash.to_owned(),
        cache_hit: cached.cache_hit,
    })
}

fn compute_or_load_algorithm_result<R, Load, Compute, Store>(
    cache_key: &str,
    mut load: Load,
    compute: Compute,
    mut store: Store,
) -> GraphResult<CachedComputation<R>>
where
    R: Clone,
    Load: FnMut() -> GraphResult<Option<R>>,
    Compute: FnOnce() -> GraphResult<R>,
    Store: FnMut(&R) -> GraphResult<()>,
{
    if let Some(result) = load()? {
        return Ok(CachedComputation {
            result,
            cache_hit: true,
        });
    }

    let lock = algorithm_cache_lock(cache_key);
    let _guard = lock
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    if let Some(result) = load()? {
        return Ok(CachedComputation {
            result,
            cache_hit: true,
        });
    }

    let result = compute()?;
    store(&result)?;
    Ok(CachedComputation {
        result,
        cache_hit: false,
    })
}

fn algorithm_cache_lock(cache_key: &str) -> Arc<Mutex<()>> {
    static CLEANUP_COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

    let mut locks = ALGORITHM_CACHE_LOCKS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    // Periodically clean up unreferenced locks to prevent memory leaks
    if CLEANUP_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed) % 64 == 0 {
        locks.retain(|_, v| Arc::strong_count(v) > 1);
    }

    locks
        .entry(cache_key.to_owned())
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone()
}

fn load_cached_algorithm_result_with_memory<R>(
    spec: &AlgorithmResultCacheSpec<'_>,
    params_hash: &str,
    cache_key: &str,
) -> GraphResult<Option<R>>
where
    R: Clone + DeserializeOwned + Send + Sync + 'static,
{
    if let Some(result) = load_in_memory_algorithm_result(cache_key) {
        return Ok(Some(result));
    }

    let result = load_cached_algorithm_result(spec, params_hash)?;
    if let Some(result) = &result {
        store_in_memory_algorithm_result(cache_key, result, spec.ttl_seconds);
    }
    Ok(result)
}

fn store_cached_algorithm_result_with_memory<R>(
    spec: &AlgorithmResultCacheSpec<'_>,
    params_hash: &str,
    cache_key: &str,
    result: &R,
) -> GraphResult<()>
where
    R: Clone + Send + Serialize + Sync + 'static,
{
    store_cached_algorithm_result(spec, params_hash, result)?;
    store_in_memory_algorithm_result(cache_key, result, spec.ttl_seconds);
    Ok(())
}

fn load_in_memory_algorithm_result<R>(cache_key: &str) -> Option<R>
where
    R: Clone + Send + Sync + 'static,
{
    let mut cache = IN_MEMORY_ALGORITHM_RESULTS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(entry) = cache.get(cache_key) else {
        return None;
    };
    if entry
        .expires_at
        .is_some_and(|expires_at| expires_at <= Instant::now())
    {
        cache.remove(cache_key);
        emit_cache_evict(CacheEvictEvent {
            reason: CacheEvictReason::TtlExpired,
            count: 1,
        });
        return None;
    }
    Arc::clone(&entry.result)
        .downcast::<R>()
        .ok()
        .map(|result| (*result).clone())
}

fn store_in_memory_algorithm_result<R>(cache_key: &str, result: &R, ttl_seconds: u64)
where
    R: Clone + Send + Sync + 'static,
{
    static CLEANUP_COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

    let expires_at = Instant::now().checked_add(Duration::from_secs(ttl_seconds));
    let mut cache = IN_MEMORY_ALGORITHM_RESULTS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    // Periodic garbage collection of expired results
    if CLEANUP_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed) % 64 == 0 {
        let evicted_count = evict_expired_in_memory_algorithm_results(&mut cache, Instant::now());
        if evicted_count > 0 {
            emit_cache_evict(CacheEvictEvent {
                reason: CacheEvictReason::TtlExpired,
                count: usize_to_u32_saturating(evicted_count),
            });
        }
    }

    cache.insert(
        cache_key.to_owned(),
        InMemoryAlgorithmResult {
            result: Arc::new(result.clone()),
            expires_at,
        },
    );
}

fn evict_expired_in_memory_algorithm_results(
    cache: &mut HashMap<String, InMemoryAlgorithmResult>,
    now: Instant,
) -> usize {
    let before = cache.len();
    cache.retain(|_, entry| entry.expires_at.is_none_or(|expires_at| expires_at > now));
    before.saturating_sub(cache.len())
}

fn load_cached_algorithm_result<R>(
    spec: &AlgorithmResultCacheSpec<'_>,
    params_hash: &str,
) -> GraphResult<Option<R>>
where
    R: DeserializeOwned,
{
    let row = spec
        .conn
        .get_graph_algorithm_result(
            spec.workspace_id,
            spec.snapshot_id,
            spec.algorithm,
            params_hash,
        )
        .map_err(|error| GraphError::storage("load graph algorithm result cache", error))?;
    let Some(row) = row else {
        return Ok(None);
    };
    if !cached_algorithm_result_is_fresh(&row) {
        return Ok(None);
    }

    match serde_json::from_str(&row.result_json) {
        Ok(result) => Ok(Some(result)),
        Err(error) => {
            tracing::warn!(
                target: "ee::graph",
                workspace_id = spec.workspace_id,
                snapshot_id = spec.snapshot_id,
                algorithm = spec.algorithm,
                params_hash,
                error = %error,
                "graph algorithm result cache row could not be deserialized"
            );
            Ok(None)
        }
    }
}

fn store_cached_algorithm_result<R>(
    spec: &AlgorithmResultCacheSpec<'_>,
    params_hash: &str,
    result: &R,
) -> GraphResult<()>
where
    R: Serialize,
{
    let result_json = serde_json::to_string(result)
        .map_err(|error| GraphError::json("serialize graph algorithm result cache row", error))?;
    spec.conn
        .upsert_graph_algorithm_result(&CreateGraphAlgorithmResultInput {
            workspace_id: spec.workspace_id.to_owned(),
            snapshot_id: spec.snapshot_id.to_owned(),
            algorithm: spec.algorithm.to_owned(),
            params_hash: params_hash.to_owned(),
            result_json,
            ttl_seconds: spec.ttl_seconds,
        })
        .map_err(|error| GraphError::storage("store graph algorithm result cache", error))
}

fn cached_algorithm_result_is_fresh(row: &StoredGraphAlgorithmResult) -> bool {
    let Ok(computed_at) = DateTime::parse_from_rfc3339(&row.computed_at) else {
        return false;
    };
    let Ok(ttl_seconds) = i64::try_from(row.ttl_seconds) else {
        return true;
    };
    let Some(ttl) = chrono::Duration::try_seconds(ttl_seconds) else {
        return false;
    };
    computed_at
        .with_timezone(&Utc)
        .checked_add_signed(ttl)
        .is_some_and(|expires_at| expires_at > Utc::now())
}

pub fn run_with_sampling<R, Exact, Approx>(
    name: &str,
    node_count: usize,
    sample_threshold: usize,
    sample_size: usize,
    snapshot_version: u64,
    f_exact: Exact,
    f_approx: Approx,
) -> SamplingRun<R>
where
    Exact: FnOnce() -> R,
    Approx: FnOnce(&[usize], u64) -> R,
{
    let seed = deterministic_sampling_seed(
        name,
        snapshot_version,
        node_count,
        sample_threshold,
        sample_size,
    );
    let choice = if node_count < sample_threshold {
        SamplingChoice::Exact
    } else {
        SamplingChoice::Approximate
    };
    let pivots = match choice {
        SamplingChoice::Exact => Vec::new(),
        SamplingChoice::Approximate => deterministic_sample_pivots(node_count, sample_size, seed),
    };
    let effective_sample_size = pivots.len();
    let decision_path_hash = sampling_decision_path_hash(&SamplingDecisionHashInput {
        name,
        snapshot_version,
        node_count,
        sample_threshold,
        sample_size,
        choice,
        seed,
        pivots: &pivots,
    });
    let witness = SamplingWitness {
        algorithm: name.to_owned(),
        snapshot_version,
        node_count,
        sample_threshold,
        requested_sample_size: sample_size,
        effective_sample_size,
        choice,
        seed,
        pivots,
        decision_path_hash,
    };
    let result = match choice {
        SamplingChoice::Exact => f_exact(),
        SamplingChoice::Approximate => f_approx(&witness.pivots, seed),
    };

    SamplingRun { result, witness }
}

pub fn run_with_sampling_policy<R, Exact, Approx>(
    name: &str,
    node_count: usize,
    policy: SamplingPolicy,
    snapshot_version: u64,
    f_exact: Exact,
    f_approx: Approx,
) -> SamplingRun<R>
where
    Exact: FnOnce() -> R,
    Approx: FnOnce(&[usize], u64) -> R,
{
    run_with_sampling(
        name,
        node_count,
        policy.sample_threshold,
        policy.sample_size,
        snapshot_version,
        f_exact,
        f_approx,
    )
}

#[must_use]
pub fn run_pagerank_with_policy(graph: &DiGraph, policy: PprPolicy) -> PageRankResult {
    pagerank_with_params(
        graph,
        policy.alpha,
        DEFAULT_PAGERANK_MAX_ITERATIONS,
        DEFAULT_PAGERANK_TOLERANCE,
    )
}

#[must_use]
pub fn deterministic_sampling_seed(
    name: &str,
    snapshot_version: u64,
    node_count: usize,
    sample_threshold: usize,
    sample_size: usize,
) -> u64 {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"ee.graph.algorithms.sampling.seed.v1");
    hasher.update(name.as_bytes());
    hasher.update(&snapshot_version.to_le_bytes());
    hasher.update(&node_count.to_le_bytes());
    hasher.update(&sample_threshold.to_le_bytes());
    hasher.update(&sample_size.to_le_bytes());
    let digest = hasher.finalize();
    let mut seed_bytes = [0_u8; 8];
    seed_bytes.copy_from_slice(&digest.as_bytes()[..8]);
    u64::from_le_bytes(seed_bytes)
}

#[must_use]
pub fn deterministic_sample_pivots(node_count: usize, sample_size: usize, seed: u64) -> Vec<usize> {
    if node_count == 0 || sample_size == 0 {
        return Vec::new();
    }

    let effective_sample_size = sample_size.min(node_count);
    let mut ranked: Vec<(blake3::Hash, usize)> = (0..node_count)
        .map(|node_index| {
            let mut hasher = blake3::Hasher::new();
            hasher.update(b"ee.graph.algorithms.sampling.pivot.v1");
            hasher.update(&seed.to_le_bytes());
            hasher.update(&node_index.to_le_bytes());
            (hasher.finalize(), node_index)
        })
        .collect();
    ranked.sort_by(|left, right| {
        left.0
            .as_bytes()
            .cmp(right.0.as_bytes())
            .then_with(|| left.1.cmp(&right.1))
    });

    let mut pivots: Vec<_> = ranked
        .into_iter()
        .take(effective_sample_size)
        .map(|(_, node_index)| node_index)
        .collect();
    pivots.sort_unstable();
    pivots
}

struct SamplingDecisionHashInput<'a> {
    name: &'a str,
    snapshot_version: u64,
    node_count: usize,
    sample_threshold: usize,
    sample_size: usize,
    choice: SamplingChoice,
    seed: u64,
    pivots: &'a [usize],
}

fn sampling_decision_path_hash(input: &SamplingDecisionHashInput<'_>) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"ee.graph.algorithms.sampling.decision.v1");
    hasher.update(input.name.as_bytes());
    hasher.update(input.choice.as_str().as_bytes());
    hasher.update(&input.snapshot_version.to_le_bytes());
    hasher.update(&input.node_count.to_le_bytes());
    hasher.update(&input.sample_threshold.to_le_bytes());
    hasher.update(&input.sample_size.to_le_bytes());
    hasher.update(&input.seed.to_le_bytes());
    for pivot in input.pivots {
        hasher.update(&pivot.to_le_bytes());
    }
    format!("blake3:{}", hasher.finalize().to_hex())
}

fn cgse_usize(value: usize) -> CgseValue {
    match i64::try_from(value) {
        Ok(value) => CgseValue::Int(value),
        Err(_) => CgseValue::String(value.to_string()),
    }
}

fn cgse_u64(value: u64) -> CgseValue {
    match i64::try_from(value) {
        Ok(value) => CgseValue::Int(value),
        Err(_) => CgseValue::String(value.to_string()),
    }
}

fn u64_to_usize_saturating(value: u64) -> usize {
    usize::try_from(value).unwrap_or(usize::MAX)
}

fn duration_millis_saturating(duration: Duration) -> u64 {
    duration.as_millis().min(u128::from(u64::MAX)) as u64
}

fn usize_to_u32_saturating(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

pub(crate) fn check_cancelled(cx: &Cx, name: &'static str) -> GraphResult<()> {
    if cx.checkpoint().is_ok() && !cx.is_cancel_requested() {
        return Ok(());
    }

    Err(GraphError::AlgorithmCancelled {
        algorithm: name.to_owned(),
        reason: cx.cancel_reason().map_or_else(
            || "cancellation requested".to_owned(),
            |reason| reason.to_string(),
        ),
    })
}

fn panic_payload_to_string(payload: Box<dyn std::any::Any + Send + 'static>) -> String {
    let payload = match payload.downcast::<String>() {
        Ok(message) => return *message,
        Err(payload) => payload,
    };
    match payload.downcast::<&'static str>() {
        Ok(message) => (*message).to_owned(),
        Err(_) => "non-string panic payload".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Barrier, Mutex};
    use std::thread;
    use std::time::{Duration, Instant};

    use asupersync::CancelReason;
    use tracing::subscriber::with_default;
    use tracing_subscriber::Layer;
    use tracing_subscriber::layer::{Context, SubscriberExt};
    use tracing_subscriber::registry::Registry;

    use crate::core::graph_telemetry::{
        ALGORITHM_CANCELLED_EVENT, ALGORITHM_COMPUTE_EVENT, ALGORITHM_TIMEOUT_EVENT,
        CACHE_EVICT_EVENT, CACHE_HIT_EVENT, CACHE_MISS_EVENT,
    };
    use crate::db::{
        CreateGraphSnapshotInput, CreateWorkspaceInput, DbConnection, GraphSnapshotType,
    };
    use crate::graph::GraphResult;

    type TestResult<T = ()> = Result<T, String>;

    fn graph_result<T>(result: GraphResult<T>) -> Result<T, String> {
        result.map_err(|error| error.to_string())
    }

    #[derive(Clone, Debug)]
    struct CapturedEvent {
        target: String,
        fields: BTreeMap<String, String>,
    }

    #[derive(Default, Clone)]
    struct CaptureLayer {
        events: Arc<Mutex<Vec<CapturedEvent>>>,
    }

    impl<S> Layer<S> for CaptureLayer
    where
        S: tracing::Subscriber,
    {
        fn on_event(&self, event: &tracing::Event<'_>, _: Context<'_, S>) {
            let mut captured = CapturedEvent {
                target: event.metadata().target().to_owned(),
                fields: BTreeMap::new(),
            };
            let mut visitor = CaptureVisitor {
                fields: &mut captured.fields,
            };
            event.record(&mut visitor);
            self.events.lock().expect("capture lock").push(captured);
        }
    }

    struct CaptureVisitor<'a> {
        fields: &'a mut BTreeMap<String, String>,
    }

    impl tracing::field::Visit for CaptureVisitor<'_> {
        fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
            self.fields
                .insert(field.name().to_owned(), format!("{value:?}"));
        }

        fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
            self.fields
                .insert(field.name().to_owned(), value.to_owned());
        }

        fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
            self.fields
                .insert(field.name().to_owned(), value.to_string());
        }

        fn record_i64(&mut self, field: &tracing::field::Field, value: i64) {
            self.fields
                .insert(field.name().to_owned(), value.to_string());
        }

        fn record_bool(&mut self, field: &tracing::field::Field, value: bool) {
            self.fields
                .insert(field.name().to_owned(), value.to_string());
        }
    }

    fn capture_graph_events<F: FnOnce()>(thunk: F) -> Vec<CapturedEvent> {
        let layer = CaptureLayer::default();
        let events = Arc::clone(&layer.events);
        let subscriber = Registry::default()
            .with(layer)
            .with(tracing_subscriber::filter::LevelFilter::TRACE);
        with_default(subscriber, thunk);
        let guard = events.lock().expect("capture lock");
        guard.clone()
    }

    fn events_with_target<'a>(events: &'a [CapturedEvent], target: &str) -> Vec<&'a CapturedEvent> {
        events
            .iter()
            .filter(|event| event.target == target)
            .collect()
    }

    #[test]
    fn run_with_budget_returns_under_budget_result() -> TestResult {
        let cx = Cx::for_testing();
        let result = graph_result(run_with_budget(
            &cx,
            "under_budget_fixture",
            DEFAULT_FOREGROUND_BUDGET,
            || 42_u64,
        ))?;

        assert_eq!(result, 42);
        Ok(())
    }

    #[test]
    fn run_with_budget_emits_algorithm_compute_telemetry() -> TestResult {
        let cx = Cx::for_testing();
        let mut result = Ok(());
        let events = capture_graph_events(|| {
            result = graph_result(run_with_budget(
                &cx,
                "telemetry_compute_fixture",
                DEFAULT_FOREGROUND_BUDGET,
                || 42_u64,
            ))
            .map(|_| ());
        });
        result?;

        let compute = events_with_target(&events, ALGORITHM_COMPUTE_EVENT);
        assert_eq!(compute.len(), 1);
        assert_eq!(
            compute[0].fields.get("algorithm").map(String::as_str),
            Some("telemetry_compute_fixture")
        );
        assert_eq!(
            compute[0].fields.get("snapshot_id").map(String::as_str),
            Some(UNTRACKED_GRAPH_SNAPSHOT_ID)
        );
        assert_eq!(
            compute[0].fields.get("params_hash").map(String::as_str),
            Some(UNTRACKED_GRAPH_PARAMS_HASH)
        );
        assert_eq!(
            compute[0].fields.get("cache_hit").map(String::as_str),
            Some("false")
        );
        Ok(())
    }

    #[test]
    fn run_with_budget_times_out_over_budget_work() -> TestResult {
        let cx = Cx::for_testing();
        let error = match run_with_budget(&cx, "timeout_fixture", Duration::from_millis(10), || {
            thread::sleep(Duration::from_millis(50));
            7_u64
        }) {
            Ok(value) => return Err(format!("expected timeout error, got {value}")),
            Err(error) => error,
        };

        match error {
            GraphError::AlgorithmTimeout {
                algorithm,
                timeout_ms,
            } => {
                assert_eq!(algorithm, "timeout_fixture");
                assert_eq!(timeout_ms, 10);
            }
            other => {
                return Err(format!("expected AlgorithmTimeout, got {other:?}"));
            }
        }
        Ok(())
    }

    #[test]
    fn run_with_budget_emits_timeout_telemetry() -> TestResult {
        let cx = Cx::for_testing();
        let events = capture_graph_events(|| {
            let _ = run_with_budget(
                &cx,
                "telemetry_timeout_fixture",
                Duration::from_millis(5),
                || {
                    thread::sleep(Duration::from_millis(25));
                    7_u64
                },
            );
        });

        let timeout = events_with_target(&events, ALGORITHM_TIMEOUT_EVENT);
        assert_eq!(timeout.len(), 1);
        assert_eq!(
            timeout[0].fields.get("algorithm").map(String::as_str),
            Some("telemetry_timeout_fixture")
        );
        assert_eq!(
            timeout[0].fields.get("budget_ms").map(String::as_str),
            Some("5")
        );
        Ok(())
    }

    #[test]
    fn run_with_budget_emits_cancelled_telemetry() -> TestResult {
        let cx = Cx::for_testing();
        cx.set_cancel_reason(CancelReason::timeout().with_message("telemetry cancellation"));
        let events = capture_graph_events(|| {
            let _ = run_with_budget(
                &cx,
                "telemetry_cancelled_fixture",
                DEFAULT_FOREGROUND_BUDGET,
                || 7_u64,
            );
        });

        let cancelled = events_with_target(&events, ALGORITHM_CANCELLED_EVENT);
        assert_eq!(cancelled.len(), 1);
        assert_eq!(
            cancelled[0].fields.get("algorithm").map(String::as_str),
            Some("telemetry_cancelled_fixture")
        );
        Ok(())
    }

    #[test]
    fn run_with_budget_reports_worker_panic() -> TestResult {
        let cx = Cx::for_testing();
        let error = match run_with_budget(
            &cx,
            "panic_fixture",
            DEFAULT_FOREGROUND_BUDGET,
            || -> u64 { panic!("graph worker exploded") },
        ) {
            Ok(value) => return Err(format!("expected worker panic error, got {value}")),
            Err(error) => error,
        };

        match error {
            GraphError::GraphEngine { operation, source } => {
                assert_eq!(operation, "panic_fixture");
                assert!(
                    source.contains("graph worker exploded"),
                    "panic source should include payload, got {source}"
                );
            }
            other => {
                return Err(format!("expected GraphEngine panic error, got {other:?}"));
            }
        }
        Ok(())
    }

    #[test]
    fn with_cgse_mode_exposes_explicit_policy_engine() {
        let strict = with_cgse_mode(DEFAULT_CGSE_MODE, |engine| engine.mode());
        let hardened = with_cgse_mode(CompatibilityMode::Hardened, |engine| engine.mode());

        assert_eq!(strict, CompatibilityMode::Strict);
        assert_eq!(hardened, CompatibilityMode::Hardened);
    }

    #[test]
    fn run_with_sampling_uses_exact_under_threshold() {
        let run = run_with_sampling(
            "betweenness",
            499,
            DEFAULT_SAMPLE_THRESHOLD,
            DEFAULT_SAMPLE_SIZE,
            7,
            || "exact",
            |_, _| "approx",
        );

        assert_eq!(run.result, "exact");
        assert_eq!(run.witness.choice, SamplingChoice::Exact);
        assert_eq!(run.witness.effective_sample_size, 0);
        assert!(run.witness.pivots.is_empty());
        assert!(run.witness.decision_path_hash.starts_with("blake3:"));
    }

    #[test]
    fn run_with_sampling_uses_approx_at_or_over_threshold() {
        let run = run_with_sampling(
            "betweenness",
            500,
            DEFAULT_SAMPLE_THRESHOLD,
            DEFAULT_SAMPLE_SIZE,
            7,
            || (0, 0),
            |pivots, seed| (pivots.len(), seed),
        );

        assert_eq!(run.witness.choice, SamplingChoice::Approximate);
        assert_eq!(run.result.0, DEFAULT_SAMPLE_SIZE);
        assert_eq!(run.result.1, run.witness.seed);
        assert_eq!(run.witness.pivots.len(), DEFAULT_SAMPLE_SIZE);
        assert!(run.witness.pivots.windows(2).all(|pair| pair[0] < pair[1]));
    }

    #[test]
    fn sampling_policy_uses_graph_config_overrides() {
        let policy = SamplingPolicy::from_optional_sample_config(Some(3), Some(2));
        let run = run_with_sampling_policy(
            "gomory_hu",
            3,
            policy,
            21,
            || "exact",
            |pivots, _| {
                assert_eq!(pivots.len(), 2);
                "approx"
            },
        );

        assert_eq!(policy.sample_threshold, 3);
        assert_eq!(policy.sample_size, 2);
        assert_eq!(run.result, "approx");
        assert_eq!(run.witness.sample_threshold, 3);
        assert_eq!(run.witness.requested_sample_size, 2);
    }

    #[test]
    fn ppr_policy_uses_graph_config_alpha_override() -> TestResult {
        let default_policy = PprPolicy::from_optional_config(None);
        let override_policy = PprPolicy::from_optional_config(Some(0.90));
        let mut graph = DiGraph::strict();
        graph
            .add_edge("a", "b")
            .map_err(|error| format!("edge add a->b should succeed: {error}"))?;
        graph
            .add_edge("b", "c")
            .map_err(|error| format!("edge add b->c should succeed: {error}"))?;

        let default_result = run_pagerank_with_policy(&graph, default_policy);
        let override_result = run_pagerank_with_policy(&graph, override_policy);
        let default_b_score = default_result
            .scores
            .iter()
            .find(|score| score.node == "b")
            .map(|score| score.score)
            .ok_or_else(|| "default PageRank result should include b".to_owned())?;
        let override_b_score = override_result
            .scores
            .iter()
            .find(|score| score.node == "b")
            .map(|score| score.score)
            .ok_or_else(|| "override PageRank result should include b".to_owned())?;

        assert!((default_policy.alpha - DEFAULT_PPR_ALPHA).abs() <= f64::EPSILON);
        assert!((override_policy.alpha - 0.90).abs() <= f64::EPSILON);
        assert!((default_b_score - override_b_score).abs() > 1.0e-6);
        assert!(override_result.converged);
        Ok(())
    }

    #[test]
    fn sampling_witness_is_recorded_as_deterministic_cgse_value() {
        let first = run_with_sampling(
            "k_truss",
            1_000,
            DEFAULT_SAMPLE_THRESHOLD,
            DEFAULT_SAMPLE_SIZE,
            11,
            || "exact",
            |pivots, seed| {
                assert_eq!(pivots.len(), DEFAULT_SAMPLE_SIZE);
                assert_ne!(seed, 0);
                "approx"
            },
        );
        let second = run_with_sampling(
            "k_truss",
            1_000,
            DEFAULT_SAMPLE_THRESHOLD,
            DEFAULT_SAMPLE_SIZE,
            11,
            || "exact",
            |_, _| "approx",
        );
        let different_snapshot = run_with_sampling(
            "k_truss",
            1_000,
            DEFAULT_SAMPLE_THRESHOLD,
            DEFAULT_SAMPLE_SIZE,
            12,
            || "exact",
            |_, _| "approx",
        );

        assert_eq!(first.result, "approx");
        assert_eq!(first.witness, second.witness);
        assert_ne!(first.witness.seed, different_snapshot.witness.seed);
        assert_ne!(
            first.witness.decision_path_hash,
            different_snapshot.witness.decision_path_hash
        );
        assert_eq!(first.witness.pivots, second.witness.pivots);

        let CgseValue::Map(fields) = first.witness.to_cgse_value() else {
            panic!("sampling witness should render as CGSE map");
        };
        assert_eq!(
            fields.get("choice"),
            Some(&CgseValue::String("approximate".to_owned()))
        );
        assert_eq!(fields.get("snapshotVersion"), Some(&CgseValue::Int(11)));
    }

    #[test]
    fn run_with_result_cache_reuses_stored_result() -> TestResult {
        let connection = DbConnection::open_memory().map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        let workspace_id = "wsp_7123456789abcdef0123456789";
        let snapshot_id = "gsnap_7123456789abcdef012345678";
        connection
            .insert_workspace(
                workspace_id,
                &CreateWorkspaceInput {
                    path: "/workspace/algorithm-result-cache".to_owned(),
                    name: Some("algorithm-result-cache".to_owned()),
                },
            )
            .map_err(|error| error.to_string())?;
        connection
            .insert_graph_snapshot(
                snapshot_id,
                &CreateGraphSnapshotInput {
                    workspace_id: workspace_id.to_owned(),
                    snapshot_version: 1,
                    schema_version: "ee.graph.snapshot.v1".to_owned(),
                    graph_type: GraphSnapshotType::MemoryLinks,
                    node_count: 2,
                    edge_count: 1,
                    metrics_json: r#"{"nodes":[],"edges":[]}"#.to_owned(),
                    content_hash: "blake3:algorithm-result-cache-snapshot".to_owned(),
                    source_generation: 0,
                    expires_at: None,
                },
            )
            .map_err(|error| error.to_string())?;

        let params = serde_json::json!({"damping": 0.85});
        let spec = AlgorithmResultCacheSpec {
            conn: &connection,
            workspace_id,
            snapshot_id,
            snapshot_content_hash: "blake3:algorithm-result-cache-snapshot",
            algorithm: "pagerank",
            params: &params,
            ttl_seconds: 300,
        };
        let compute_count = AtomicUsize::new(0);

        let first = graph_result(run_with_result_cache(&spec, || {
            compute_count.fetch_add(1, Ordering::SeqCst);
            Ok(serde_json::json!({"scores":[["mem_a",0.75]]}))
        }))?;
        let second = graph_result(run_with_result_cache(&spec, || {
            compute_count.fetch_add(1, Ordering::SeqCst);
            Ok(serde_json::json!({"scores":[["mem_a",0.25]]}))
        }))?;

        assert!(!first.cache_hit);
        assert!(second.cache_hit);
        assert_eq!(first.params_hash, second.params_hash);
        assert_eq!(first.result, second.result);
        assert_eq!(compute_count.load(Ordering::SeqCst), 1);

        connection.close().map_err(|error| error.to_string())
    }

    #[test]
    fn run_with_cached_budget_skips_worker_on_cache_hit() -> TestResult {
        let connection = DbConnection::open_memory().map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        let workspace_id = "wsp_8123456789abcdef0123456789";
        let snapshot_id = "gsnap_8123456789abcdef012345678";
        connection
            .insert_workspace(
                workspace_id,
                &CreateWorkspaceInput {
                    path: "/workspace/algorithm-cached-budget".to_owned(),
                    name: Some("algorithm-cached-budget".to_owned()),
                },
            )
            .map_err(|error| error.to_string())?;
        connection
            .insert_graph_snapshot(
                snapshot_id,
                &CreateGraphSnapshotInput {
                    workspace_id: workspace_id.to_owned(),
                    snapshot_version: 1,
                    schema_version: "ee.graph.snapshot.v1".to_owned(),
                    graph_type: GraphSnapshotType::MemoryLinks,
                    node_count: 2,
                    edge_count: 1,
                    metrics_json: r#"{"nodes":[],"edges":[]}"#.to_owned(),
                    content_hash: "blake3:algorithm-cached-budget-snapshot".to_owned(),
                    source_generation: 0,
                    expires_at: None,
                },
            )
            .map_err(|error| error.to_string())?;

        let params = serde_json::json!({"algorithm": "pagerank", "alpha": 0.30});
        let spec = AlgorithmResultCacheSpec {
            conn: &connection,
            workspace_id,
            snapshot_id,
            snapshot_content_hash: "blake3:algorithm-cached-budget-snapshot",
            algorithm: "pagerank",
            params: &params,
            ttl_seconds: 300,
        };
        let compute_count = Arc::new(AtomicUsize::new(0));
        let first_compute_count = Arc::clone(&compute_count);
        let second_compute_count = Arc::clone(&compute_count);
        let cx = Cx::for_testing();

        let first = graph_result(run_with_cached_budget(
            &cx,
            &spec,
            "pagerank",
            DEFAULT_FOREGROUND_BUDGET,
            move || {
                first_compute_count.fetch_add(1, Ordering::SeqCst);
                serde_json::json!({"scores":[["mem_a",0.75]]})
            },
        ))?;
        let second = graph_result(run_with_cached_budget(
            &cx,
            &spec,
            "pagerank",
            DEFAULT_FOREGROUND_BUDGET,
            move || -> serde_json::Value {
                second_compute_count.fetch_add(1, Ordering::SeqCst);
                panic!("cached budget worker should not run on cache hit");
            },
        ))?;

        assert!(!first.cache_hit);
        assert!(second.cache_hit);
        assert_eq!(first.result, second.result);
        assert_eq!(compute_count.load(Ordering::SeqCst), 1);

        connection.close().map_err(|error| error.to_string())
    }

    #[test]
    fn run_with_cached_budget_emits_cache_and_compute_telemetry() -> TestResult {
        let connection = DbConnection::open_memory().map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        let workspace_id = "wsp_8123456789abcdef0123456790";
        let snapshot_id = "gsnap_8123456789abcdef012345679";
        connection
            .insert_workspace(
                workspace_id,
                &CreateWorkspaceInput {
                    path: "/workspace/algorithm-cached-budget-telemetry".to_owned(),
                    name: Some("algorithm-cached-budget-telemetry".to_owned()),
                },
            )
            .map_err(|error| error.to_string())?;
        connection
            .insert_graph_snapshot(
                snapshot_id,
                &CreateGraphSnapshotInput {
                    workspace_id: workspace_id.to_owned(),
                    snapshot_version: 1,
                    schema_version: "ee.graph.snapshot.v1".to_owned(),
                    graph_type: GraphSnapshotType::MemoryLinks,
                    node_count: 2,
                    edge_count: 1,
                    metrics_json: r#"{"nodes":[],"edges":[]}"#.to_owned(),
                    content_hash: "blake3:algorithm-cached-budget-telemetry-snapshot".to_owned(),
                    source_generation: 0,
                    expires_at: None,
                },
            )
            .map_err(|error| error.to_string())?;

        let params = serde_json::json!({"algorithm": "pagerank", "alpha": 0.30});
        let spec = AlgorithmResultCacheSpec {
            conn: &connection,
            workspace_id,
            snapshot_id,
            snapshot_content_hash: "blake3:algorithm-cached-budget-telemetry-snapshot",
            algorithm: "pagerank",
            params: &params,
            ttl_seconds: 300,
        };
        let cx = Cx::for_testing();
        let mut first = None;
        let mut second = None;
        let events = capture_graph_events(|| {
            first = Some(graph_result(run_with_cached_budget(
                &cx,
                &spec,
                "pagerank",
                DEFAULT_FOREGROUND_BUDGET,
                || serde_json::json!({"scores":[["mem_a",0.75]]}),
            )));
            second = Some(graph_result(run_with_cached_budget(
                &cx,
                &spec,
                "pagerank",
                DEFAULT_FOREGROUND_BUDGET,
                || serde_json::json!({"scores":[["mem_a",0.25]]}),
            )));
        });

        let first = first.expect("first run recorded")?;
        let second = second.expect("second run recorded")?;
        assert!(!first.cache_hit);
        assert!(second.cache_hit);
        assert_eq!(first.params_hash, second.params_hash);

        let miss = events_with_target(&events, CACHE_MISS_EVENT);
        let hit = events_with_target(&events, CACHE_HIT_EVENT);
        let compute = events_with_target(&events, ALGORITHM_COMPUTE_EVENT);
        assert_eq!(miss.len(), 1);
        assert_eq!(hit.len(), 1);
        assert_eq!(compute.len(), 2);
        assert_eq!(
            miss[0].fields.get("params_hash").map(String::as_str),
            Some(first.params_hash.as_str())
        );
        assert_eq!(
            hit[0].fields.get("params_hash").map(String::as_str),
            Some(first.params_hash.as_str())
        );
        assert_eq!(
            compute[0].fields.get("snapshot_id").map(String::as_str),
            Some(snapshot_id)
        );
        assert_eq!(
            compute[0].fields.get("cache_hit").map(String::as_str),
            Some("false")
        );
        assert_eq!(
            compute[1].fields.get("cache_hit").map(String::as_str),
            Some("true")
        );

        connection.close().map_err(|error| error.to_string())
    }

    #[test]
    fn expired_in_memory_cache_load_emits_cache_evict_telemetry() -> TestResult {
        let cache_key = "expired_in_memory_cache_load_emits_cache_evict_telemetry";
        store_in_memory_algorithm_result(cache_key, &123_u64, 0);

        let mut loaded = Some(123_u64);
        let events = capture_graph_events(|| {
            loaded = load_in_memory_algorithm_result::<u64>(cache_key);
        });

        assert_eq!(loaded, None);
        let evicts = events_with_target(&events, CACHE_EVICT_EVENT);
        assert_eq!(evicts.len(), 1);
        assert_eq!(
            evicts[0].fields.get("reason").map(String::as_str),
            Some("ttl_expired")
        );
        assert_eq!(evicts[0].fields.get("count").map(String::as_str), Some("1"));
        Ok(())
    }

    #[test]
    fn expired_in_memory_cache_cleanup_counts_only_expired_rows() {
        let now = Instant::now();
        let mut cache = HashMap::new();
        cache.insert(
            "expired".to_owned(),
            InMemoryAlgorithmResult {
                result: Arc::new(1_u64),
                expires_at: now.checked_sub(Duration::from_millis(1)),
            },
        );
        cache.insert(
            "fresh".to_owned(),
            InMemoryAlgorithmResult {
                result: Arc::new(2_u64),
                expires_at: now.checked_add(Duration::from_secs(60)),
            },
        );
        cache.insert(
            "persistent".to_owned(),
            InMemoryAlgorithmResult {
                result: Arc::new(3_u64),
                expires_at: None,
            },
        );

        let evicted = evict_expired_in_memory_algorithm_results(&mut cache, now);

        assert_eq!(evicted, 1);
        assert!(!cache.contains_key("expired"));
        assert!(cache.contains_key("fresh"));
        assert!(cache.contains_key("persistent"));
    }

    #[test]
    fn run_with_result_cache_hit_avoids_cold_compute_cost() -> TestResult {
        let connection = DbConnection::open_memory().map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        let workspace_id = "wsp_9123456789abcdef0123456789";
        let snapshot_id = "gsnap_9123456789abcdef012345678";
        connection
            .insert_workspace(
                workspace_id,
                &CreateWorkspaceInput {
                    path: "/workspace/algorithm-cache-perf".to_owned(),
                    name: Some("algorithm-cache-perf".to_owned()),
                },
            )
            .map_err(|error| error.to_string())?;
        connection
            .insert_graph_snapshot(
                snapshot_id,
                &CreateGraphSnapshotInput {
                    workspace_id: workspace_id.to_owned(),
                    snapshot_version: 1,
                    schema_version: "ee.graph.snapshot.v1".to_owned(),
                    graph_type: GraphSnapshotType::MemoryLinks,
                    node_count: 2,
                    edge_count: 1,
                    metrics_json: r#"{"nodes":[],"edges":[]}"#.to_owned(),
                    content_hash: "blake3:algorithm-cache-perf-snapshot".to_owned(),
                    source_generation: 0,
                    expires_at: None,
                },
            )
            .map_err(|error| error.to_string())?;

        let params = serde_json::json!({"algorithm": "pagerank", "alpha": 0.30});
        let spec = AlgorithmResultCacheSpec {
            conn: &connection,
            workspace_id,
            snapshot_id,
            snapshot_content_hash: "blake3:algorithm-cache-perf-snapshot",
            algorithm: "pagerank",
            params: &params,
            ttl_seconds: 300,
        };
        let compute_count = AtomicUsize::new(0);

        let cold_started = Instant::now();
        let cold = graph_result(run_with_result_cache(&spec, || {
            compute_count.fetch_add(1, Ordering::SeqCst);
            thread::sleep(Duration::from_millis(25));
            Ok(serde_json::json!({"scores":[["mem_a",0.75]]}))
        }))?;
        let cold_elapsed = cold_started.elapsed();

        let warm_started = Instant::now();
        let warm = graph_result(run_with_result_cache(&spec, || {
            compute_count.fetch_add(1, Ordering::SeqCst);
            Ok(serde_json::json!({"scores":[["mem_a",0.25]]}))
        }))?;
        let warm_elapsed = warm_started.elapsed();

        assert!(!cold.cache_hit);
        assert!(warm.cache_hit);
        assert_eq!(cold.result, warm.result);
        assert_eq!(compute_count.load(Ordering::SeqCst), 1);
        assert!(
            warm_elapsed < cold_elapsed,
            "cache hit should avoid cold compute cost: warm={warm_elapsed:?} cold={cold_elapsed:?}"
        );

        connection.close().map_err(|error| error.to_string())
    }

    #[test]
    fn compute_or_load_algorithm_result_serializes_same_key_computes() -> TestResult {
        let stored = Arc::new(Mutex::new(None::<u64>));
        let compute_count = Arc::new(AtomicUsize::new(0));
        let barrier = Arc::new(Barrier::new(10));
        let mut handles = Vec::new();

        for _ in 0..10 {
            let stored = Arc::clone(&stored);
            let compute_count = Arc::clone(&compute_count);
            let barrier = Arc::clone(&barrier);
            handles.push(thread::spawn(move || -> TestResult<(u64, bool)> {
                barrier.wait();
                let run = graph_result(compute_or_load_algorithm_result(
                    "test\0same-algorithm-cache-key",
                    || {
                        Ok(*stored
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner))
                    },
                    || {
                        compute_count.fetch_add(1, Ordering::SeqCst);
                        thread::sleep(Duration::from_millis(25));
                        Ok(42_u64)
                    },
                    |result| {
                        *stored
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(*result);
                        Ok(())
                    },
                ))?;
                Ok((run.result, run.cache_hit))
            }));
        }

        let mut cache_hits = 0;
        for handle in handles {
            let (result, cache_hit) = handle
                .join()
                .map_err(|_| "cache thread panicked".to_owned())??;
            assert_eq!(result, 42);
            if cache_hit {
                cache_hits += 1;
            }
        }

        assert_eq!(compute_count.load(Ordering::SeqCst), 1);
        assert_eq!(cache_hits, 9);
        Ok(())
    }
}
