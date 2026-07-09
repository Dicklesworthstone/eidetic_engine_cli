use std::any::Any;
use std::collections::{BTreeMap, HashMap};
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use asupersync::Cx;
use asupersync::cx::NoCaps;
use chrono::{DateTime, Utc};
use fnx_algorithms::{PageRankResult, pagerank_with_params};
use fnx_classes::digraph::DiGraph;
use fnx_runtime::{CgsePolicyEngine, CgseValue, CompatibilityMode};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::core::graph_audit::{
    AlgorithmDegradedInputs, ResultCachedInputs, ResultEvictedInputs, ResultEvictedReason,
    build_algorithm_degraded_payload, build_result_cached_payload, build_result_evicted_payload,
    graph_algorithm_result_audit_target_id, insert_graph_audit_payload,
};
use crate::core::graph_memory_budget::{
    AlgorithmAdmissionDecision, MemoryBudgetPolicy, MemoryBudgetRefusal, check_algorithm_admission,
};
use crate::core::graph_telemetry::{
    AlgorithmCancelledEvent, AlgorithmComputeEvent, AlgorithmTimeoutEvent, CacheEvictEvent,
    CacheEvictReason, CacheOutcomeEvent, emit_algorithm_cancelled, emit_algorithm_compute,
    emit_algorithm_timeout, emit_cache_evict, emit_cache_hit, emit_cache_miss,
};
use crate::db::{CreateGraphAlgorithmResultInput, DbConnection, StoredGraphAlgorithmResult};
use crate::graph::{
    GraphError, GraphResult, SUBSYSTEM as GRAPH_SUBSYSTEM, graph_algorithm_legacy_json_params_hash,
    graph_algorithm_params_hash,
};

pub const DEFAULT_PPR_ALPHA: f64 = 0.30;
pub const DEFAULT_PAGERANK_MAX_ITERATIONS: usize = 100;
pub const DEFAULT_PAGERANK_TOLERANCE: f64 = 1.0e-6;
pub const DEFAULT_SAMPLE_THRESHOLD: usize = 500;
pub const DEFAULT_SAMPLE_SIZE: usize = 100;
pub const DEFAULT_FOREGROUND_BUDGET: Duration = Duration::from_millis(250);
pub const DEFAULT_BACKGROUND_BUDGET: Duration = Duration::from_millis(2_000);
pub const DEFAULT_CGSE_MODE: CompatibilityMode = CompatibilityMode::Strict;
const CANCELLATION_POLL_INTERVAL: Duration = Duration::from_millis(10);
// Blocking-worker cancellation is soft: after timeout the
// closure may continue running. Keep a bounded process-local slot until the
// blocking closure actually exits so repeated graph timeouts cannot spawn an
// unbounded orphan tail.
const MAX_CONCURRENT_GRAPH_BUDGET_WORKERS: usize = 8;
const UNTRACKED_GRAPH_SNAPSHOT_ID: &str = "untracked";
const UNTRACKED_GRAPH_PARAMS_HASH: &str = "untracked";

static GRAPH_BUDGET_WORKER_LIMITER: OnceLock<Arc<GraphBudgetWorkerLimiter>> = OnceLock::new();

#[derive(Debug)]
struct GraphBudgetWorkerLimiter {
    active: AtomicUsize,
    cap: usize,
}

impl GraphBudgetWorkerLimiter {
    fn new(cap: usize) -> Self {
        Self {
            active: AtomicUsize::new(0),
            cap: cap.max(1),
        }
    }

    fn try_acquire(limiter: &Arc<Self>) -> Option<GraphBudgetWorkerSlot> {
        let mut current = limiter.active.load(Ordering::Acquire);
        loop {
            if current >= limiter.cap {
                return None;
            }
            match limiter.active.compare_exchange_weak(
                current,
                current + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    return Some(GraphBudgetWorkerSlot {
                        limiter: Arc::clone(limiter),
                    });
                }
                Err(actual) => current = actual,
            }
        }
    }

    #[cfg(test)]
    fn active_count(&self) -> usize {
        self.active.load(Ordering::Acquire)
    }
}

#[derive(Debug)]
struct GraphBudgetWorkerSlot {
    limiter: Arc<GraphBudgetWorkerLimiter>,
}

impl Drop for GraphBudgetWorkerSlot {
    fn drop(&mut self) {
        let previous = self.limiter.active.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(previous > 0, "graph budget worker slot underflow");
    }
}

fn graph_budget_worker_limiter() -> Arc<GraphBudgetWorkerLimiter> {
    Arc::clone(GRAPH_BUDGET_WORKER_LIMITER.get_or_init(|| {
        Arc::new(GraphBudgetWorkerLimiter::new(
            MAX_CONCURRENT_GRAPH_BUDGET_WORKERS,
        ))
    }))
}

/// Returns a detached, capability-less [`Cx`] for driving the synchronous
/// graph-budget algorithms outside a running Asupersync task.
///
/// These algorithms run on a dedicated OS thread (see
/// [`run_with_budget_observed_with_limiter`]) and only ever consult the `Cx`
/// for cancellation and budget state — never for spawn/timer/IO/entropy/remote
/// capabilities. [`Cx::detached_cancel_context`] is Asupersync's sanctioned
/// production constructor for exactly this "cancellation-aware primitives
/// outside a running task" use case, and it returns a `Cx<NoCaps>` so it cannot
/// leak ambient authority.
///
/// This deliberately does **not** fall back to `Cx::for_testing()`. As of
/// asupersync 0.3.6 (br-asupersync-2x6hbi) `for_testing()` is gated behind
/// `cfg(any(test, feature = "test-internals"))` precisely so external crates
/// cannot mint a full-capability `Cx` in production; the graph budget helpers
/// below are generic over the capability set, so a `Cx<NoCaps>` flows through
/// them unchanged while test call sites that build a `Cx::for_testing()`
/// (`Cx<All>`) still type-check.
#[must_use]
pub fn current_or_testing_cx() -> Cx<NoCaps> {
    Cx::detached_cancel_context()
}

pub fn run_with_budget<R, F, Caps>(
    cx: &Cx<Caps>,
    name: &'static str,
    budget: Duration,
    f: F,
) -> GraphResult<R>
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

/// Outcome of [`run_with_memory_admission`].
///
/// When the memory budget refuses the algorithm before it allocates,
/// callers receive the refusal payload and MUST NOT invoke the underlying
/// algorithm. When the admission succeeds, the caller gets back the
/// algorithm result and the running combined-bytes total the
/// [`AlgorithmAdmissionDecision::Admit`] arm produced so the call site can
/// update its process-local accounting.
#[derive(Clone, Debug)]
pub enum MemoryAdmitted<R> {
    /// Algorithm ran inside the budget and returned a result. The
    /// `combined_bytes` value is the total active resident bytes after
    /// the algorithm landed; the caller can subtract `requested_working_set`
    /// when the algorithm releases its allocation.
    Admitted { result: R, combined_bytes: u64 },
    /// Algorithm was refused before the budget runtime spun up. Caller
    /// should surface the refusal in the response envelope's `degraded[]`
    /// row and decline to invoke the underlying algorithm.
    Refused(MemoryBudgetRefusal),
}

impl<R> MemoryAdmitted<R> {
    /// Convenience: true iff the algorithm was admitted.
    #[must_use]
    pub fn is_admitted(&self) -> bool {
        matches!(self, Self::Admitted { .. })
    }

    /// Map the admitted-result variant in place. No-op on refused.
    #[must_use]
    pub fn map<T, M>(self, mapper: M) -> MemoryAdmitted<T>
    where
        M: FnOnce(R) -> T,
    {
        match self {
            Self::Admitted {
                result,
                combined_bytes,
            } => MemoryAdmitted::Admitted {
                result: mapper(result),
                combined_bytes,
            },
            Self::Refused(refusal) => MemoryAdmitted::Refused(refusal),
        }
    }
}

/// F2 admission wrapper around [`run_with_budget`] (bd-ryzpw).
///
/// Consults [`check_algorithm_admission`] BEFORE spinning up the budget
/// runtime: if the requested working set or combined load would cross
/// the per-algorithm cap or the snapshot pressure ceiling, the algorithm
/// closure is NOT invoked and the caller receives a
/// [`MemoryAdmitted::Refused`] payload carrying the
/// [`MemoryBudgetRefusal`] (code/severity/message/repair/observed/limit).
///
/// On admission success the closure runs inside [`run_with_budget`] with
/// the same cancellation / time-budget semantics as direct callers; the
/// returned `combined_bytes` value lets the caller update its
/// process-local active-resident counter.
pub fn run_with_memory_admission<R, F, Caps>(
    cx: &Cx<Caps>,
    name: &'static str,
    budget: Duration,
    policy: &MemoryBudgetPolicy,
    active_resident_bytes: u64,
    requested_working_set: u64,
    f: F,
) -> GraphResult<MemoryAdmitted<R>>
where
    R: Send + 'static,
    F: FnOnce() -> R + Send + 'static,
{
    match check_algorithm_admission(active_resident_bytes, requested_working_set, policy) {
        AlgorithmAdmissionDecision::Refuse(refusal) => Ok(MemoryAdmitted::Refused(refusal)),
        AlgorithmAdmissionDecision::Admit { combined_bytes } => {
            let result = run_with_budget(cx, name, budget, f)?;
            Ok(MemoryAdmitted::Admitted {
                result,
                combined_bytes,
            })
        }
    }
}

fn run_with_budget_observed<R, F, Caps>(
    cx: &Cx<Caps>,
    name: &'static str,
    budget: Duration,
    telemetry: BudgetTelemetry<'_>,
    f: F,
) -> GraphResult<R>
where
    R: Send + 'static,
    F: FnOnce() -> R + Send + 'static,
{
    run_with_budget_observed_with_limiter(
        cx,
        name,
        budget,
        telemetry,
        graph_budget_worker_limiter(),
        f,
    )
}

fn run_with_budget_observed_with_limiter<R, F, Caps>(
    cx: &Cx<Caps>,
    name: &'static str,
    budget: Duration,
    telemetry: BudgetTelemetry<'_>,
    worker_limiter: Arc<GraphBudgetWorkerLimiter>,
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

    let Some(worker_slot) = GraphBudgetWorkerLimiter::try_acquire(&worker_limiter) else {
        let error = GraphError::AlgorithmTimeout {
            algorithm: name.to_owned(),
            timeout_ms: duration_millis_saturating(budget),
        };
        emit_budget_failure_telemetry(name, budget, started, telemetry, &error);
        return Err(error);
    };

    let (sender, receiver) = mpsc::sync_channel(1);
    thread::Builder::new()
        .name("ee-graph-budget".to_owned())
        .spawn(move || {
            let _worker_slot = worker_slot;
            let _ = sender.send(std::panic::catch_unwind(AssertUnwindSafe(f)));
        })
        .map_err(|error| GraphError::GraphEngine {
            operation: "start graph budget worker",
            source: error.to_string(),
        })?;

    let outcome = loop {
        if let Err(error) = check_cancelled(cx, name) {
            break Err(error);
        }
        let Some(remaining) = budget.checked_sub(started.elapsed()) else {
            break Err(GraphError::AlgorithmTimeout {
                algorithm: name.to_owned(),
                timeout_ms: duration_millis_saturating(budget),
            });
        };
        if remaining.is_zero() {
            break Err(GraphError::AlgorithmTimeout {
                algorithm: name.to_owned(),
                timeout_ms: duration_millis_saturating(budget),
            });
        }

        let poll_budget = remaining.min(CANCELLATION_POLL_INTERVAL);
        match receiver.recv_timeout(poll_budget) {
            Ok(result) => break Ok(result),
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                break Err(GraphError::GraphEngine {
                    operation: name,
                    source: "graph algorithm worker exited without result".to_owned(),
                });
            }
        }
    };

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

pub fn run_with_cached_budget<R, F, Caps>(
    cx: &Cx<Caps>,
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
    let run = match run_with_result_cache_with_params_hash(spec, &params_hash, || {
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
    }) {
        Ok(run) => run,
        Err(error) => {
            insert_graph_algorithm_degraded_audit(spec, name, &error)?;
            return Err(error);
        }
    };
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

// bd-8tsi5: RwLock (was Mutex) so the keyed-lock lookup hot path
// takes `.read()` for cache-hit reads of an existing per-key entry.
// Sibling to bd-1nan9 / bd-2lin9 / bd-25yao / bd-2r38i; serializes
// only on the slow path (key-not-present insert + periodic GC).
static ALGORITHM_CACHE_LOCKS: OnceLock<RwLock<HashMap<String, Arc<Mutex<()>>>>> = OnceLock::new();
// bd-1nan9: RwLock (was Mutex) so cache-hit reads via
// `load_in_memory_algorithm_result` can take `.read()` and run
// concurrently. The lazy-TTL eviction that used to mutate the map
// on every load is now deferred to the next mutating call path
// (`store_in_memory_algorithm_result`'s periodic 64-store cleanup
// and the natural HashMap::insert replacement). Mirrors the PPR
// shared-lock refactor in bd-2lin9.
static IN_MEMORY_ALGORITHM_RESULTS: OnceLock<RwLock<HashMap<String, InMemoryAlgorithmResult>>> =
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
    let mut stale_persistent_eviction_emitted = false;
    let cached = compute_or_load_algorithm_result(
        &cache_key,
        || {
            let loaded = load_cached_algorithm_result_with_memory(
                spec,
                params_hash,
                &cache_key,
                &mut stale_persistent_eviction_emitted,
            )?;
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
        |result, elapsed_ms| {
            store_cached_algorithm_result_with_memory(
                spec,
                params_hash,
                &cache_key,
                result,
                elapsed_ms,
            )
        },
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
    Store: FnMut(&R, u64) -> GraphResult<()>,
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

    let compute_started = Instant::now();
    let result = compute()?;
    let compute_elapsed_ms = duration_millis_saturating(compute_started.elapsed());
    store(&result, compute_elapsed_ms)?;
    Ok(CachedComputation {
        result,
        cache_hit: false,
    })
}

fn algorithm_cache_lock(cache_key: &str) -> Arc<Mutex<()>> {
    static CLEANUP_COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let locks = ALGORITHM_CACHE_LOCKS.get_or_init(|| RwLock::new(HashMap::new()));

    // bd-8tsi5: fast path takes `.read()` for cache-hit reads of an
    // existing per-key entry. Concurrent callers with distinct
    // cache_keys parallelize at this layer. Still increment
    // CLEANUP_COUNTER on the read path so the GC trigger cadence on
    // the next write tracks total call volume, not just write volume.
    let counter_tick = CLEANUP_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    {
        let read_guard = locks
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(existing) = read_guard.get(cache_key) {
            return Arc::clone(existing);
        }
    }

    // Slow path: key missing, take the write lock to insert. The
    // read-then-write window means another thread may have inserted
    // the same key in between; `or_insert_with` handles that race
    // by returning the existing entry's Arc.
    let mut write_guard = locks
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    // Periodic GC of unreferenced inner mutexes runs only on the
    // write path now (bd-8tsi5). Reading paths no longer take the
    // exclusive lock so GC cannot piggyback there. Counter cadence
    // is still incremented on every call so the trigger fires
    // close to its original 1-in-64 frequency once a write does
    // happen.
    if counter_tick % 64 == 0 {
        write_guard.retain(|_, v| Arc::strong_count(v) > 1);
    }

    write_guard
        .entry(cache_key.to_owned())
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone()
}

fn load_cached_algorithm_result_with_memory<R>(
    spec: &AlgorithmResultCacheSpec<'_>,
    params_hash: &str,
    cache_key: &str,
    stale_persistent_eviction_emitted: &mut bool,
) -> GraphResult<Option<R>>
where
    R: Clone + DeserializeOwned + Send + Sync + 'static,
{
    if let Some(result) = load_in_memory_algorithm_result(cache_key) {
        return Ok(Some(result));
    }

    let result =
        load_cached_algorithm_result(spec, params_hash, stale_persistent_eviction_emitted)?;
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
    compute_elapsed_ms: u64,
) -> GraphResult<()>
where
    R: Clone + Send + Serialize + Sync + 'static,
{
    store_cached_algorithm_result(spec, params_hash, result, compute_elapsed_ms)?;
    store_in_memory_algorithm_result(cache_key, result, spec.ttl_seconds);
    Ok(())
}

fn load_in_memory_algorithm_result<R>(cache_key: &str) -> Option<R>
where
    R: Clone + Send + Sync + 'static,
{
    // bd-1nan9: cache-hit hot path now takes RwLock::read() so
    // concurrent algorithm reads against different cache keys
    // parallelize. A TTL-expired entry is treated as a miss
    // (safety-critical: never return a stale result); the actual
    // removal is deferred to the next mutating call path
    // (`store_in_memory_algorithm_result` runs a periodic GC every
    // 64 stores and an `insert` for the same key naturally replaces
    // the stale entry). The deferred `emit_cache_evict` event fires
    // during the next eviction pass instead of on the load that
    // first observed expiry — semantic shift in WHEN the event
    // fires, but it still fires.
    let cache = IN_MEMORY_ALGORITHM_RESULTS
        .get_or_init(|| RwLock::new(HashMap::new()))
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let entry = cache.get(cache_key)?;
    if entry
        .expires_at
        .is_some_and(|expires_at| expires_at <= Instant::now())
    {
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
    // bd-1nan9: write lock; this path mutates (inserts the fresh
    // entry and runs the periodic GC). Counterpart to the .read()
    // path in `load_in_memory_algorithm_result`.
    let mut cache = IN_MEMORY_ALGORITHM_RESULTS
        .get_or_init(|| RwLock::new(HashMap::new()))
        .write()
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
    stale_persistent_eviction_emitted: &mut bool,
) -> GraphResult<Option<R>>
where
    R: DeserializeOwned,
{
    let row = load_cached_algorithm_result_row(spec, params_hash)?;
    let Some(row) = row else {
        return Ok(None);
    };
    if !cached_algorithm_result_is_fresh(&row) {
        if !*stale_persistent_eviction_emitted {
            emit_cache_evict(CacheEvictEvent {
                reason: CacheEvictReason::TtlExpired,
                count: 1,
            });
            insert_graph_algorithm_result_evicted_audit(
                spec,
                &row.params_hash,
                ResultEvictedReason::TtlExpired,
            )?;
            *stale_persistent_eviction_emitted = true;
        }
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
                params_hash = row.params_hash.as_str(),
                error = %error,
                "graph algorithm result cache row could not be deserialized"
            );
            Ok(None)
        }
    }
}

fn load_cached_algorithm_result_row(
    spec: &AlgorithmResultCacheSpec<'_>,
    params_hash: &str,
) -> GraphResult<Option<StoredGraphAlgorithmResult>> {
    let primary = spec
        .conn
        .get_graph_algorithm_result(
            spec.workspace_id,
            spec.snapshot_id,
            spec.algorithm,
            params_hash,
        )
        .map_err(|error| GraphError::storage("load graph algorithm result cache", error))?;
    if primary.is_some() {
        return Ok(primary);
    }

    let legacy_hash = graph_algorithm_legacy_json_params_hash(
        spec.algorithm,
        spec.snapshot_content_hash,
        spec.params,
    )?;
    if legacy_hash == params_hash {
        return Ok(None);
    }

    spec.conn
        .get_graph_algorithm_result(
            spec.workspace_id,
            spec.snapshot_id,
            spec.algorithm,
            &legacy_hash,
        )
        .map_err(|error| GraphError::storage("load legacy graph algorithm result cache", error))
}

fn store_cached_algorithm_result<R>(
    spec: &AlgorithmResultCacheSpec<'_>,
    params_hash: &str,
    result: &R,
    compute_elapsed_ms: u64,
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
        .map_err(|error| GraphError::storage("store graph algorithm result cache", error))?;
    let cache_size_after = spec
        .conn
        .list_graph_algorithm_results(spec.workspace_id, spec.snapshot_id, Some(spec.algorithm))
        .map_err(|error| GraphError::storage("count graph algorithm result cache rows", error))
        .and_then(|rows| {
            u64::try_from(rows.len()).map_err(|_| {
                GraphError::numeric_overflow("graph algorithm result cache size", rows.len())
            })
        })?;
    let witness_id =
        graph_algorithm_result_audit_target_id(spec.snapshot_id, spec.algorithm, params_hash);
    let payload = build_result_cached_payload(ResultCachedInputs {
        witness_id: witness_id.as_str(),
        algorithm: spec.algorithm,
        params_hash,
        elapsed_ms: compute_elapsed_ms,
        cache_size_after,
    });
    insert_graph_audit_payload(spec.conn, spec.workspace_id, GRAPH_SUBSYSTEM, payload)
        .map_err(|error| GraphError::storage("insert graph algorithm result cache audit", error))
}

fn insert_graph_algorithm_result_evicted_audit(
    spec: &AlgorithmResultCacheSpec<'_>,
    params_hash: &str,
    reason: ResultEvictedReason,
) -> GraphResult<()> {
    let witness_id =
        graph_algorithm_result_audit_target_id(spec.snapshot_id, spec.algorithm, params_hash);
    let payload = build_result_evicted_payload(ResultEvictedInputs {
        witness_id: witness_id.as_str(),
        reason,
    });
    insert_graph_audit_payload(spec.conn, spec.workspace_id, GRAPH_SUBSYSTEM, payload)
        .map_err(|error| GraphError::storage("insert graph algorithm result eviction audit", error))
}

fn insert_graph_algorithm_degraded_audit(
    spec: &AlgorithmResultCacheSpec<'_>,
    algorithm: &'static str,
    error: &GraphError,
) -> GraphResult<()> {
    let payload = build_algorithm_degraded_payload(AlgorithmDegradedInputs {
        algorithm,
        code: error.kind_str(),
        severity: graph_error_audit_severity(error),
        repair: error.repair_hint(),
        snapshot_version: None,
    });
    insert_graph_audit_payload(spec.conn, spec.workspace_id, GRAPH_SUBSYSTEM, payload)
        .map_err(|error| GraphError::storage("insert graph algorithm degraded audit", error))
}

const fn graph_error_audit_severity(error: &GraphError) -> &'static str {
    match error {
        GraphError::Storage { .. }
        | GraphError::SnapshotLockHeld { .. }
        | GraphError::SnapshotLockUnavailable { .. } => "warning",
        GraphError::Json { .. }
        | GraphError::GraphEngine { .. }
        | GraphError::AlgorithmCancelled { .. }
        | GraphError::AlgorithmTimeout { .. }
        | GraphError::InvalidSnapshotMetrics { .. } => "medium",
        GraphError::NumericOverflow { .. } | GraphError::SnapshotVersionOverflow => "high",
    }
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

pub(crate) fn check_cancelled<Caps>(cx: &Cx<Caps>, name: &'static str) -> GraphResult<()> {
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
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Barrier, Mutex};
    use std::thread;
    use std::time::{Duration, Instant};

    use asupersync::CancelReason;
    use tracing::subscriber::with_default;
    use tracing_subscriber::Layer;
    use tracing_subscriber::layer::{Context, SubscriberExt};
    use tracing_subscriber::registry::Registry;

    use crate::core::graph_audit::{
        RESULT_CACHED_ACTION, RESULT_EVICTED_ACTION, WITNESS_TARGET_TYPE,
        graph_algorithm_result_audit_target_id,
    };
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

    fn test_budget_telemetry() -> BudgetTelemetry<'static> {
        BudgetTelemetry {
            snapshot_id: UNTRACKED_GRAPH_SNAPSHOT_ID,
            params_hash: UNTRACKED_GRAPH_PARAMS_HASH,
            emit_compute: true,
            cache_hit: false,
            sampling_used: false,
        }
    }

    struct KillOnDrop(Arc<AtomicBool>);

    impl Drop for KillOnDrop {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Release);
        }
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
    fn run_with_memory_admission_admits_under_cap() -> TestResult {
        let cx = Cx::for_testing();
        let policy = MemoryBudgetPolicy::defaults();
        let outcome = graph_result(run_with_memory_admission(
            &cx,
            "memory_admit_under_cap",
            DEFAULT_FOREGROUND_BUDGET,
            &policy,
            0,
            1024,
            || 7_u64,
        ))?;
        match outcome {
            MemoryAdmitted::Admitted {
                result,
                combined_bytes,
            } => {
                assert_eq!(result, 7);
                assert_eq!(combined_bytes, 1024);
            }
            MemoryAdmitted::Refused(refusal) => panic!("expected admitted, got {refusal:?}"),
        }
        Ok(())
    }

    #[test]
    fn run_with_memory_admission_refuses_above_per_algorithm_cap() -> TestResult {
        let cx = Cx::for_testing();
        let policy = MemoryBudgetPolicy::defaults();
        // Per-algorithm cap defaults to 100 MiB = 100 * 1024 * 1024 bytes.
        let requested = policy.per_algorithm_cap_bytes + 1;
        let outcome = graph_result(run_with_memory_admission(
            &cx,
            "memory_admit_refuse_per_algorithm",
            DEFAULT_FOREGROUND_BUDGET,
            &policy,
            0,
            requested,
            || -> u64 { panic!("closure must not run on refusal") },
        ))?;
        match outcome {
            MemoryAdmitted::Refused(refusal) => {
                assert_eq!(
                    refusal.code,
                    crate::core::graph_memory_budget::ALGORITHM_MEMORY_CAP_CODE
                );
                assert_eq!(refusal.observed_bytes, requested);
                assert_eq!(refusal.limit_bytes, policy.per_algorithm_cap_bytes);
            }
            MemoryAdmitted::Admitted { .. } => panic!("expected refusal"),
        }
        Ok(())
    }

    #[test]
    fn run_with_memory_admission_refuses_above_combined_pressure_ceiling() -> TestResult {
        let cx = Cx::for_testing();
        let policy = MemoryBudgetPolicy::defaults();
        // Active load just under cap + requested just under per-algorithm cap
        // adds up past snapshot_cap_bytes, triggering MEMORY_PRESSURE_CODE.
        let active = policy.snapshot_cap_bytes - 1;
        let requested = policy.per_algorithm_cap_bytes - 1;
        let outcome = graph_result(run_with_memory_admission(
            &cx,
            "memory_admit_refuse_pressure",
            DEFAULT_FOREGROUND_BUDGET,
            &policy,
            active,
            requested,
            || -> u64 { panic!("closure must not run on refusal") },
        ))?;
        match outcome {
            MemoryAdmitted::Refused(refusal) => {
                assert_eq!(
                    refusal.code,
                    crate::core::graph_memory_budget::MEMORY_PRESSURE_CODE
                );
            }
            MemoryAdmitted::Admitted { .. } => panic!("expected refusal"),
        }
        Ok(())
    }

    #[test]
    fn memory_admitted_map_preserves_combined_bytes_and_skips_refused() {
        let admitted: MemoryAdmitted<u8> = MemoryAdmitted::Admitted {
            result: 3,
            combined_bytes: 4_096,
        };
        let mapped = admitted.map(|value| value as u64 * 2);
        match mapped {
            MemoryAdmitted::Admitted {
                result,
                combined_bytes,
            } => {
                assert_eq!(result, 6_u64);
                assert_eq!(combined_bytes, 4_096);
            }
            MemoryAdmitted::Refused(refusal) => panic!("expected admitted, got {refusal:?}"),
        }

        let refused: MemoryAdmitted<u8> = MemoryAdmitted::Refused(MemoryBudgetRefusal {
            code: crate::core::graph_memory_budget::ALGORITHM_MEMORY_CAP_CODE,
            severity: "high",
            message: "test refusal",
            repair: "test repair",
            observed_bytes: 1,
            limit_bytes: 0,
        });
        let mapped_refused = refused.map(|v| v as u64);
        assert!(!mapped_refused.is_admitted());
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
    fn run_with_budget_worker_limit_refuses_work_while_timed_out_worker_runs() -> TestResult {
        let limiter = Arc::new(GraphBudgetWorkerLimiter::new(1));
        let kill = Arc::new(AtomicBool::new(false));
        let _kill_guard = KillOnDrop(Arc::clone(&kill));
        let first_started = Arc::new(AtomicBool::new(false));
        let first_kill = Arc::clone(&kill);
        let first_limiter = Arc::clone(&limiter);
        let first_started_for_worker = Arc::clone(&first_started);

        let first_handle = thread::spawn(move || {
            let cx = Cx::for_testing();
            run_with_budget_observed_with_limiter(
                &cx,
                "worker_limit_first_timeout",
                Duration::from_millis(250),
                test_budget_telemetry(),
                first_limiter,
                move || {
                    first_started_for_worker.store(true, Ordering::Release);
                    let hard_stop = Instant::now() + Duration::from_secs(2);
                    while !first_kill.load(Ordering::Acquire) && Instant::now() < hard_stop {
                        thread::sleep(Duration::from_millis(1));
                    }
                    1_u64
                },
            )
        });

        let start_deadline = Instant::now() + Duration::from_secs(1);
        while !first_started.load(Ordering::Acquire) && Instant::now() < start_deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(
            first_started.load(Ordering::Acquire),
            "first workload should have started and held the limiter slot"
        );

        let first = first_handle
            .join()
            .map_err(|_| "first worker-limit test thread panicked".to_owned())?;

        match first {
            Err(GraphError::AlgorithmTimeout {
                algorithm,
                timeout_ms,
            }) => {
                assert_eq!(algorithm, "worker_limit_first_timeout");
                assert_eq!(timeout_ms, 250);
            }
            Err(other) => return Err(format!("expected first timeout, got {other:?}")),
            Ok(value) => return Err(format!("expected first timeout, got Ok({value})")),
        }

        assert_eq!(
            limiter.active_count(),
            1,
            "timed-out worker should keep its slot until the blocking closure exits"
        );

        let cx = Cx::for_testing();
        let second_started = Arc::new(AtomicBool::new(false));
        let second_started_for_worker = Arc::clone(&second_started);
        let second = run_with_budget_observed_with_limiter(
            &cx,
            "worker_limit_second_refused",
            Duration::from_millis(250),
            test_budget_telemetry(),
            Arc::clone(&limiter),
            move || {
                second_started_for_worker.store(true, Ordering::Release);
                2_u64
            },
        );

        match second {
            Err(GraphError::AlgorithmTimeout {
                algorithm,
                timeout_ms,
            }) => {
                assert_eq!(algorithm, "worker_limit_second_refused");
                assert_eq!(timeout_ms, 250);
            }
            Err(other) => return Err(format!("expected cap refusal timeout, got {other:?}")),
            Ok(value) => return Err(format!("expected cap refusal timeout, got Ok({value})")),
        }
        assert!(
            !second_started.load(Ordering::Acquire),
            "cap refusal must happen before spawning another blocking closure"
        );
        assert_eq!(
            limiter.active_count(),
            1,
            "refused work must not consume an extra limiter slot"
        );

        kill.store(true, Ordering::Release);
        let deadline = Instant::now() + Duration::from_secs(1);
        while Instant::now() < deadline {
            if limiter.active_count() == 0 {
                return Ok(());
            }
            thread::sleep(Duration::from_millis(10));
        }

        Err(format!(
            "timed-out worker did not release its limiter slot after kill; active={}",
            limiter.active_count()
        ))
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
    fn run_with_budget_emits_cancelled_telemetry_when_cancelled_after_start() -> TestResult {
        let cx = Cx::for_testing();
        let worker_cx = cx.clone();
        let started = Arc::new(AtomicBool::new(false));
        let started_for_worker = Arc::clone(&started);
        let started_for_canceller = Arc::clone(&started);
        let canceller_cx = cx.clone();
        let canceller = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(1);
            while !started_for_canceller.load(Ordering::Acquire) && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(1));
            }
            canceller_cx
                .set_cancel_reason(CancelReason::timeout().with_message("mid-run cancellation"));
        });

        let mut outcome = None;
        let events = capture_graph_events(|| {
            outcome = Some(run_with_budget(
                &cx,
                "telemetry_midrun_cancelled_fixture",
                DEFAULT_FOREGROUND_BUDGET,
                move || {
                    started_for_worker.store(true, Ordering::Release);
                    while !worker_cx.is_cancel_requested() {
                        thread::sleep(Duration::from_millis(1));
                    }
                    thread::sleep(Duration::from_millis(50));
                    7_u64
                },
            ));
        });

        canceller
            .join()
            .map_err(|_| "mid-run cancellation helper panicked".to_owned())?;

        match outcome.expect("run_with_budget should have returned") {
            Err(GraphError::AlgorithmCancelled { algorithm, reason }) => {
                assert_eq!(algorithm, "telemetry_midrun_cancelled_fixture");
                assert!(reason.contains("mid-run cancellation"));
            }
            Err(other) => return Err(format!("expected AlgorithmCancelled, got {other:?}")),
            Ok(value) => return Err(format!("expected AlgorithmCancelled, got Ok({value})")),
        }

        let cancelled = events_with_target(&events, ALGORITHM_CANCELLED_EVENT);
        assert_eq!(cancelled.len(), 1);
        assert_eq!(
            cancelled[0].fields.get("algorithm").map(String::as_str),
            Some("telemetry_midrun_cancelled_fixture")
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

        let audit_target =
            graph_algorithm_result_audit_target_id(snapshot_id, "pagerank", &first.params_hash);
        let audits = connection
            .list_audit_by_target(WITNESS_TARGET_TYPE, &audit_target, None)
            .map_err(|error| error.to_string())?;
        assert_eq!(audits.len(), 1);
        assert_eq!(audits[0].action, RESULT_CACHED_ACTION);
        let details: serde_json::Value = serde_json::from_str(
            audits[0]
                .details
                .as_deref()
                .ok_or_else(|| "result cached audit details missing".to_owned())?,
        )
        .map_err(|error| error.to_string())?;
        assert_eq!(
            details.get("algorithm"),
            Some(&serde_json::json!("pagerank"))
        );
        assert_eq!(
            details.get("params_hash"),
            Some(&serde_json::json!(first.params_hash))
        );
        assert_eq!(
            details.get("cache_size_after"),
            Some(&serde_json::json!(1_u64))
        );

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
    fn result_cache_reads_legacy_json_keyed_rows_during_transition() -> TestResult {
        let connection = DbConnection::open_memory().map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        let workspace_id = "wsp_8123456789abcdef012345679a";
        let snapshot_id = "gsnap_8123456789abcdef01234568a";
        let snapshot_content_hash = "blake3:algorithm-legacy-json-cache-snapshot";
        connection
            .insert_workspace(
                workspace_id,
                &CreateWorkspaceInput {
                    path: "/workspace/algorithm-legacy-json-cache".to_owned(),
                    name: Some("algorithm-legacy-json-cache".to_owned()),
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
                    content_hash: snapshot_content_hash.to_owned(),
                    source_generation: 0,
                    expires_at: None,
                },
            )
            .map_err(|error| error.to_string())?;

        let params = serde_json::json!({"algorithm": "pagerank", "alpha": 0.30});
        let legacy_hash = graph_result(graph_algorithm_legacy_json_params_hash(
            "pagerank",
            snapshot_content_hash,
            &params,
        ))?;
        connection
            .upsert_graph_algorithm_result(&CreateGraphAlgorithmResultInput {
                workspace_id: workspace_id.to_owned(),
                snapshot_id: snapshot_id.to_owned(),
                algorithm: "pagerank".to_owned(),
                params_hash: legacy_hash.clone(),
                result_json: r#"{"scores":[["mem_legacy",0.75]]}"#.to_owned(),
                ttl_seconds: 300,
            })
            .map_err(|error| error.to_string())?;

        let spec = AlgorithmResultCacheSpec {
            conn: &connection,
            workspace_id,
            snapshot_id,
            snapshot_content_hash,
            algorithm: "pagerank",
            params: &params,
            ttl_seconds: 300,
        };
        let run = graph_result(run_with_result_cache(&spec, || {
            Ok(serde_json::json!({"scores":[["mem_new",0.25]]}))
        }))?;

        assert!(run.cache_hit);
        assert_ne!(run.params_hash, legacy_hash);
        assert_eq!(run.result["scores"][0][0], "mem_legacy");

        let rows = connection
            .list_graph_algorithm_results(workspace_id, snapshot_id, Some("pagerank"))
            .map_err(|error| error.to_string())?;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].params_hash, legacy_hash);

        connection.close().map_err(|error| error.to_string())
    }

    #[test]
    fn expired_persistent_cache_load_emits_cache_evict_telemetry() -> TestResult {
        let connection = DbConnection::open_memory().map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        let workspace_id = "wsp_8123456789abcdef0123456791";
        let snapshot_id = "gsnap_8123456789abcdef012345680";
        connection
            .insert_workspace(
                workspace_id,
                &CreateWorkspaceInput {
                    path: "/workspace/algorithm-expired-persistent-cache".to_owned(),
                    name: Some("algorithm-expired-persistent-cache".to_owned()),
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
                    content_hash: "blake3:algorithm-expired-persistent-cache-snapshot".to_owned(),
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
            snapshot_content_hash: "blake3:algorithm-expired-persistent-cache-snapshot",
            algorithm: "pagerank",
            params: &params,
            ttl_seconds: 1,
        };
        let params_hash = graph_result(graph_algorithm_params_hash(
            spec.algorithm,
            spec.snapshot_content_hash,
            spec.params,
        ))?;
        connection
            .upsert_graph_algorithm_result(&CreateGraphAlgorithmResultInput {
                workspace_id: workspace_id.to_owned(),
                snapshot_id: snapshot_id.to_owned(),
                algorithm: spec.algorithm.to_owned(),
                params_hash: params_hash.clone(),
                result_json: r#"{"scores":[["mem_a",0.75]]}"#.to_owned(),
                ttl_seconds: 1,
            })
            .map_err(|error| error.to_string())?;
        connection
            .execute_raw(&format!(
                "UPDATE graph_algorithm_results SET computed_at = '2026-01-01T00:00:00Z' \
                 WHERE workspace_id = '{workspace_id}' AND snapshot_id = '{snapshot_id}' \
                 AND algorithm = '{}' AND params_hash = '{params_hash}'",
                spec.algorithm
            ))
            .map_err(|error| error.to_string())?;

        let mut first_loaded = Some(serde_json::json!({"scores":[["mem_a",0.75]]}));
        let mut second_loaded = Some(serde_json::json!({"scores":[["mem_a",0.75]]}));
        let events = capture_graph_events(|| {
            let mut stale_persistent_eviction_emitted = false;
            first_loaded = graph_result(load_cached_algorithm_result::<serde_json::Value>(
                &spec,
                &params_hash,
                &mut stale_persistent_eviction_emitted,
            ))
            .expect("first expired persistent cache lookup should not fail");
            second_loaded = graph_result(load_cached_algorithm_result::<serde_json::Value>(
                &spec,
                &params_hash,
                &mut stale_persistent_eviction_emitted,
            ))
            .expect("second expired persistent cache lookup should not fail");
        });

        assert_eq!(first_loaded, None);
        assert_eq!(second_loaded, None);
        let evicts = events_with_target(&events, CACHE_EVICT_EVENT);
        assert_eq!(evicts.len(), 1);
        assert_eq!(
            evicts[0].fields.get("reason").map(String::as_str),
            Some("ttl_expired")
        );
        assert_eq!(evicts[0].fields.get("count").map(String::as_str), Some("1"));

        let audit_target =
            graph_algorithm_result_audit_target_id(snapshot_id, "pagerank", &params_hash);
        let audits = connection
            .list_audit_by_target(WITNESS_TARGET_TYPE, &audit_target, None)
            .map_err(|error| error.to_string())?;
        assert_eq!(audits.len(), 1);
        assert_eq!(audits[0].action, RESULT_EVICTED_ACTION);
        let details: serde_json::Value = serde_json::from_str(
            audits[0]
                .details
                .as_deref()
                .ok_or_else(|| "result evicted audit details missing".to_owned())?,
        )
        .map_err(|error| error.to_string())?;
        assert_eq!(
            details.get("reason"),
            Some(&serde_json::json!("ttl_expired"))
        );

        connection.close().map_err(|error| error.to_string())
    }

    // bd-1nan9: after the RwLock refactor `load_in_memory_algorithm
    // _result` takes `.read()` and CANNOT emit-and-remove on the
    // expiration-observed path. The safety-critical contract (None
    // returned for expired entries — no stale data leaks) is
    // preserved. The telemetry event is deferred: it fires during
    // the next `store_in_memory_algorithm_result` GC pass that
    // reclaims the slot.
    #[test]
    fn expired_in_memory_cache_load_returns_none_with_deferred_evict_event() -> TestResult {
        let cache_key = "expired_in_memory_cache_load_returns_none_with_deferred_evict_event";
        store_in_memory_algorithm_result(cache_key, &123_u64, 0);

        // Read path: returns None, emits NO evict event (deferred).
        let mut loaded = Some(123_u64);
        let read_events = capture_graph_events(|| {
            loaded = load_in_memory_algorithm_result::<u64>(cache_key);
        });
        assert_eq!(loaded, None);
        assert!(
            events_with_target(&read_events, CACHE_EVICT_EVENT).is_empty(),
            "read path must NOT emit evict events under deferred-eviction semantics"
        );

        // The deferred event fires on the next store for the same
        // key, where `HashMap::insert` reclaims the stale slot via
        // the natural overwrite. We force a GC cycle by running
        // sentinel stores until the periodic CLEANUP_COUNTER
        // crosses the 64-store boundary, then assert at least one
        // ttl_expired evict was emitted at some point.
        let mut all_evicts = Vec::new();
        for sentinel in 0..96 {
            let sentinel_key = format!(
                "expired_in_memory_cache_load_returns_none_with_deferred_evict_event::sentinel::{sentinel}"
            );
            let events = capture_graph_events(|| {
                store_in_memory_algorithm_result(&sentinel_key, &0_u64, 600);
            });
            for event in events_with_target(&events, CACHE_EVICT_EVENT) {
                if event.fields.get("reason").map(String::as_str) == Some("ttl_expired") {
                    all_evicts.push(event.clone());
                }
            }
        }
        assert!(
            !all_evicts.is_empty(),
            "ttl_expired evict event must fire during a subsequent store's GC pass",
        );
        Ok(())
    }

    // bd-8tsi5: concurrent algorithm_cache_lock calls against
    // DISTINCT cache_keys must not deadlock and must give each
    // caller a distinct per-key Arc<Mutex<()>>. The fast path
    // takes RwLock::read() so this should parallelize, but the
    // test only asserts the safety contract (no deadlock + correct
    // per-key identity), not the parallelism property.
    #[test]
    fn algorithm_cache_lock_concurrent_distinct_keys_return_distinct_inner_mutexes() -> TestResult {
        use std::thread;
        const THREAD_COUNT: usize = 8;

        let prefix = "bd_8tsi5_distinct";
        let handles: Vec<_> = (0..THREAD_COUNT)
            .map(|tid| {
                let key = format!("{prefix}::{tid}");
                thread::spawn(move || (tid, key.clone(), algorithm_cache_lock(&key)))
            })
            .collect();

        let mut observed: Vec<(usize, String, Arc<Mutex<()>>)> = Vec::new();
        for handle in handles {
            let result = handle.join().map_err(|_| "thread panicked".to_owned())?;
            observed.push(result);
        }
        assert_eq!(observed.len(), THREAD_COUNT, "all threads must complete");

        // Each distinct cache_key must produce a distinct inner
        // Arc<Mutex<()>> by pointer identity. Same cache_key MUST
        // produce the SAME Arc — verified by re-calling each key
        // and comparing pointer-equality.
        for (_, key, first_arc) in &observed {
            let second_arc = algorithm_cache_lock(key);
            assert!(
                Arc::ptr_eq(first_arc, &second_arc),
                "algorithm_cache_lock for the same key must return the same Arc<Mutex<()>>",
            );
        }
        for i in 0..observed.len() {
            for j in (i + 1)..observed.len() {
                assert!(
                    !Arc::ptr_eq(&observed[i].2, &observed[j].2),
                    "algorithm_cache_lock for DISTINCT keys must return DISTINCT Arc<Mutex<()>>: key_i={}, key_j={}",
                    observed[i].1,
                    observed[j].1,
                );
            }
        }
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
                    |result, _elapsed_ms| {
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
