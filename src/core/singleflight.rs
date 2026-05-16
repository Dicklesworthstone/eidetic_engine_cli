//! In-process single-flight coalescing for duplicate read-heavy operations.
//!
//! The coalescer is intentionally process-local and non-durable. It only shares
//! an in-flight result for callers with the same redaction-safe
//! [`SingleFlightKey`](crate::models::SingleFlightKey); it never mutates memory,
//! indexes, Beads, Agent Mail, or cache state.

use std::collections::HashMap;
use std::sync::{
    Arc, Condvar, Mutex, OnceLock,
    atomic::{AtomicU64, AtomicUsize, Ordering},
};
use std::time::{Duration, Instant};

use crate::models::{
    SingleFlightKey, SingleFlightKeyInput, SingleFlightLastKeyPosture, SingleFlightPostureReport,
    SingleFlightSurface, SingleFlightSurfaceCounters, SingleFlightSurfacePosture,
};

pub const SINGLEFLIGHT_FOLLOWER_TIMEOUT_CODE: &str = "singleflight_follower_timeout";
pub const SINGLEFLIGHT_LEADER_FAILED_CODE: &str = "singleflight_leader_failed";
pub const SINGLEFLIGHT_STATE_POISONED_CODE: &str = "singleflight_state_poisoned";
const SINGLEFLIGHT_TRACE_SURFACE: &str = "singleflight";
const SINGLEFLIGHT_TRACE_REQUEST_ID: &str = "singleflight_group_run";
const SINGLEFLIGHT_TRACE_WORKSPACE_ID: &str = "process_local";
const SINGLEFLIGHT_LEADER_START_EVENT: &str = "leader_start";
const SINGLEFLIGHT_FOLLOWER_JOIN_EVENT: &str = "follower_join";
const SINGLEFLIGHT_FOLLOWER_TIMEOUT_EVENT: &str = "follower_timeout";
const SINGLEFLIGHT_LEADER_COMPLETE_EVENT: &str = "leader_complete";
const SINGLEFLIGHT_COALESCED_RESULT_REUSED_EVENT: &str = "coalesced_result_reused";
const SINGLEFLIGHT_LEADER_FAILED_EVENT: &str = "leader_failed";
#[cfg(test)]
const SINGLEFLIGHT_REQUIRED_TELEMETRY_PHASES: [&str; 5] = [
    SINGLEFLIGHT_LEADER_START_EVENT,
    SINGLEFLIGHT_FOLLOWER_JOIN_EVENT,
    SINGLEFLIGHT_FOLLOWER_TIMEOUT_EVENT,
    SINGLEFLIGHT_LEADER_COMPLETE_EVENT,
    SINGLEFLIGHT_COALESCED_RESULT_REUSED_EVENT,
];
const GRAPH_FEATURE_ENRICHMENT_FOLLOWER_TIMEOUT: Duration = Duration::from_secs(30);

static GRAPH_FEATURE_ENRICHMENT_GROUP: OnceLock<
    SingleFlightGroup<crate::graph::GraphFeatureEnrichmentReport>,
> = OnceLock::new();

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SingleFlightRole {
    Leader,
    Follower,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SingleFlightRun<T> {
    pub value: T,
    pub role: SingleFlightRole,
    pub shared: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SingleFlightError {
    FollowerTimeout {
        key_hash: String,
        timeout_ms: u64,
    },
    LeaderFailed {
        key_hash: String,
        role: SingleFlightRole,
        message: String,
    },
    StatePoisoned {
        key_hash: String,
    },
}

impl SingleFlightError {
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::FollowerTimeout { .. } => SINGLEFLIGHT_FOLLOWER_TIMEOUT_CODE,
            Self::LeaderFailed { .. } => SINGLEFLIGHT_LEADER_FAILED_CODE,
            Self::StatePoisoned { .. } => SINGLEFLIGHT_STATE_POISONED_CODE,
        }
    }

    #[must_use]
    pub const fn severity(&self) -> &'static str {
        match self {
            Self::FollowerTimeout { .. } => "medium",
            Self::LeaderFailed { .. } => "medium",
            Self::StatePoisoned { .. } => "high",
        }
    }

    #[must_use]
    pub const fn repair(&self) -> &'static str {
        match self {
            Self::FollowerTimeout { .. } => "Retry the read with a longer wait budget.",
            Self::LeaderFailed { .. } => {
                "Inspect the leader operation error; followers observed the same failure."
            }
            Self::StatePoisoned { .. } => {
                "Restart the process to clear poisoned in-memory single-flight state."
            }
        }
    }
}

impl std::fmt::Display for SingleFlightError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::FollowerTimeout {
                key_hash,
                timeout_ms,
            } => write!(
                formatter,
                "follower timed out waiting for key {key_hash} after {timeout_ms}ms"
            ),
            Self::LeaderFailed {
                key_hash,
                role,
                message,
            } => write!(
                formatter,
                "{role:?} observed leader failure for key {key_hash}: {message}"
            ),
            Self::StatePoisoned { key_hash } => {
                write!(
                    formatter,
                    "single-flight state was poisoned for key {key_hash}"
                )
            }
        }
    }
}

impl std::error::Error for SingleFlightError {}

#[derive(Debug)]
pub struct SingleFlightGroup<T> {
    entries: Mutex<HashMap<String, Arc<SingleFlightEntry<T>>>>,
    counters: SingleFlightCounters,
    last_key: Mutex<Option<SingleFlightLastKeyPosture>>,
}

impl<T> Default for SingleFlightGroup<T> {
    fn default() -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            counters: SingleFlightCounters::default(),
            last_key: Mutex::new(None),
        }
    }
}

impl<T> SingleFlightGroup<T>
where
    T: Clone,
{
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn run<F>(
        &self,
        key: &SingleFlightKey,
        follower_timeout: Duration,
        operation: F,
    ) -> Result<SingleFlightRun<T>, SingleFlightError>
    where
        F: FnOnce() -> Result<T, String>,
    {
        self.record_last_key(key);
        let key_hash = key.key_hash.clone();
        let (entry, is_leader) = self.entry_for(&key_hash)?;

        if is_leader {
            let leader_started = Instant::now();
            
            struct LeaderGuard<'a, T> {
                group: &'a SingleFlightGroup<T>,
                key_hash: String,
                entry: Arc<SingleFlightEntry<T>>,
                completed: bool,
            }
            
            impl<T> Drop for LeaderGuard<'_, T> {
                fn drop(&mut self) {
                    if !self.completed {
                        let _ = self.group.remove_entry(&self.key_hash, &self.entry);
                        if let Ok(mut state) = self.entry.state.lock() {
                            *state = SingleFlightState::Completed(Err("leader panicked".to_owned()));
                            self.entry.ready.notify_all();
                        }
                    }
                }
            }
            
            let mut guard = LeaderGuard {
                group: self,
                key_hash: key_hash.clone(),
                entry: Arc::clone(&entry),
                completed: false,
            };
            
            let result = operation();
            guard.completed = true;
            
            self.complete_leader(&key_hash, &entry, leader_started, result)
        } else {
            self.counters.follower_joins.fetch_add(1, Ordering::SeqCst);
            entry.followers.fetch_add(1, Ordering::SeqCst);
            trace_singleflight_checkpoint(SINGLEFLIGHT_FOLLOWER_JOIN_EVENT, &key_hash, 0, &[]);
            self.wait_for_leader(&key_hash, &entry, follower_timeout)
        }
    }

    pub fn active_len(&self) -> Result<usize, SingleFlightError> {
        let entries = self
            .entries
            .lock()
            .map_err(|_| SingleFlightError::StatePoisoned {
                key_hash: "<group>".to_owned(),
            })?;
        Ok(entries.len())
    }

    pub fn posture(
        &self,
        surface: SingleFlightSurface,
        configured: bool,
        follower_timeout: Duration,
    ) -> SingleFlightSurfacePosture {
        let last_key = self.last_key_snapshot();
        let active_leader_count = match self.active_len() {
            Ok(count) => capped_u32(count),
            Err(_) => {
                self.counters.state_poisoned.fetch_add(1, Ordering::SeqCst);
                0
            }
        };
        SingleFlightSurfacePosture::new(
            surface,
            configured,
            active_leader_count,
            self.counters.snapshot(),
            duration_ms(follower_timeout),
            last_key,
        )
    }

    fn record_last_key(&self, key: &SingleFlightKey) {
        match self.last_key.lock() {
            Ok(mut last_key) => {
                *last_key = Some(SingleFlightLastKeyPosture::from_key(key));
            }
            Err(_) => {
                self.counters.state_poisoned.fetch_add(1, Ordering::SeqCst);
            }
        }
    }

    fn last_key_snapshot(&self) -> Option<SingleFlightLastKeyPosture> {
        match self.last_key.lock() {
            Ok(last_key) => last_key.clone(),
            Err(_) => {
                self.counters.state_poisoned.fetch_add(1, Ordering::SeqCst);
                None
            }
        }
    }

    fn entry_for(
        &self,
        key_hash: &str,
    ) -> Result<(Arc<SingleFlightEntry<T>>, bool), SingleFlightError> {
        let mut entries = self
            .entries
            .lock()
            .map_err(|_| SingleFlightError::StatePoisoned {
                key_hash: key_hash.to_owned(),
            })?;
        if let Some(entry) = entries.get(key_hash) {
            return Ok((Arc::clone(entry), false));
        }

        let entry = Arc::new(SingleFlightEntry::default());
        entries.insert(key_hash.to_owned(), Arc::clone(&entry));
        self.counters.leader_starts.fetch_add(1, Ordering::SeqCst);
        trace_singleflight_checkpoint(SINGLEFLIGHT_LEADER_START_EVENT, key_hash, 0, &[]);
        Ok((entry, true))
    }

    fn complete_leader(
        &self,
        key_hash: &str,
        entry: &Arc<SingleFlightEntry<T>>,
        leader_started: Instant,
        result: Result<T, String>,
    ) -> Result<SingleFlightRun<T>, SingleFlightError> {
        {
            let mut state = entry
                .state
                .lock()
                .map_err(|_| SingleFlightError::StatePoisoned {
                    key_hash: key_hash.to_owned(),
                })?;
            *state = SingleFlightState::Completed(result.clone());
            entry.ready.notify_all();
        }
        self.remove_entry(key_hash, entry)?;

        match result {
            Ok(value) => {
                self.counters
                    .completed_leaders
                    .fetch_add(1, Ordering::SeqCst);
                trace_singleflight_checkpoint(
                    SINGLEFLIGHT_LEADER_COMPLETE_EVENT,
                    key_hash,
                    duration_ms(leader_started.elapsed()),
                    &[],
                );
                Ok(SingleFlightRun {
                    value,
                    role: SingleFlightRole::Leader,
                    shared: entry.followers.load(Ordering::SeqCst) > 0,
                })
            }
            Err(message) => {
                self.counters.leader_failures.fetch_add(1, Ordering::SeqCst);
                trace_singleflight_checkpoint(
                    SINGLEFLIGHT_LEADER_FAILED_EVENT,
                    key_hash,
                    duration_ms(leader_started.elapsed()),
                    &[SINGLEFLIGHT_LEADER_FAILED_CODE],
                );
                Err(SingleFlightError::LeaderFailed {
                    key_hash: key_hash.to_owned(),
                    role: SingleFlightRole::Leader,
                    message,
                })
            }
        }
    }

    fn wait_for_leader(
        &self,
        key_hash: &str,
        entry: &SingleFlightEntry<T>,
        follower_timeout: Duration,
    ) -> Result<SingleFlightRun<T>, SingleFlightError> {
        let started = Instant::now();
        let deadline = match started.checked_add(follower_timeout) {
            Some(deadline) => deadline,
            None => {
                self.counters
                    .follower_timeouts
                    .fetch_add(1, Ordering::SeqCst);
                trace_singleflight_checkpoint(
                    SINGLEFLIGHT_FOLLOWER_TIMEOUT_EVENT,
                    key_hash,
                    0,
                    &[SINGLEFLIGHT_FOLLOWER_TIMEOUT_CODE],
                );
                return Err(SingleFlightError::FollowerTimeout {
                    key_hash: key_hash.to_owned(),
                    timeout_ms: duration_ms(follower_timeout),
                });
            }
        };
        let mut state = entry
            .state
            .lock()
            .map_err(|_| SingleFlightError::StatePoisoned {
                key_hash: key_hash.to_owned(),
            })?;

        loop {
            match &*state {
                SingleFlightState::Pending => {
                    let remaining = match deadline.checked_duration_since(Instant::now()) {
                        Some(remaining) if !remaining.is_zero() => remaining,
                        _ => {
                            self.counters
                                .follower_timeouts
                                .fetch_add(1, Ordering::SeqCst);
                            trace_singleflight_checkpoint(
                                SINGLEFLIGHT_FOLLOWER_TIMEOUT_EVENT,
                                key_hash,
                                duration_ms(started.elapsed()),
                                &[SINGLEFLIGHT_FOLLOWER_TIMEOUT_CODE],
                            );
                            return Err(SingleFlightError::FollowerTimeout {
                                key_hash: key_hash.to_owned(),
                                timeout_ms: duration_ms(follower_timeout),
                            });
                        }
                    };
                    let (next_state, wait) =
                        entry.ready.wait_timeout(state, remaining).map_err(|_| {
                            SingleFlightError::StatePoisoned {
                                key_hash: key_hash.to_owned(),
                            }
                        })?;
                    state = next_state;
                    if wait.timed_out() && matches!(*state, SingleFlightState::Pending) {
                        self.counters
                            .follower_timeouts
                            .fetch_add(1, Ordering::SeqCst);
                        trace_singleflight_checkpoint(
                            SINGLEFLIGHT_FOLLOWER_TIMEOUT_EVENT,
                            key_hash,
                            duration_ms(started.elapsed()),
                            &[SINGLEFLIGHT_FOLLOWER_TIMEOUT_CODE],
                        );
                        return Err(SingleFlightError::FollowerTimeout {
                            key_hash: key_hash.to_owned(),
                            timeout_ms: duration_ms(follower_timeout),
                        });
                    }
                }
                SingleFlightState::Completed(Ok(value)) => {
                    self.counters.reused_results.fetch_add(1, Ordering::SeqCst);
                    trace_singleflight_checkpoint(
                        SINGLEFLIGHT_COALESCED_RESULT_REUSED_EVENT,
                        key_hash,
                        duration_ms(started.elapsed()),
                        &[],
                    );
                    return Ok(SingleFlightRun {
                        value: value.clone(),
                        role: SingleFlightRole::Follower,
                        shared: true,
                    });
                }
                SingleFlightState::Completed(Err(message)) => {
                    trace_singleflight_checkpoint(
                        SINGLEFLIGHT_LEADER_FAILED_EVENT,
                        key_hash,
                        duration_ms(started.elapsed()),
                        &[SINGLEFLIGHT_LEADER_FAILED_CODE],
                    );
                    return Err(SingleFlightError::LeaderFailed {
                        key_hash: key_hash.to_owned(),
                        role: SingleFlightRole::Follower,
                        message: message.clone(),
                    });
                }
            }
        }
    }

    fn remove_entry(
        &self,
        key_hash: &str,
        entry: &Arc<SingleFlightEntry<T>>,
    ) -> Result<(), SingleFlightError> {
        let mut entries = self
            .entries
            .lock()
            .map_err(|_| SingleFlightError::StatePoisoned {
                key_hash: key_hash.to_owned(),
            })?;
        if entries
            .get(key_hash)
            .is_some_and(|current| Arc::ptr_eq(current, entry))
        {
            entries.remove(key_hash);
        }
        Ok(())
    }
}

#[derive(Debug, Default)]
struct SingleFlightCounters {
    leader_starts: AtomicU64,
    completed_leaders: AtomicU64,
    follower_joins: AtomicU64,
    follower_timeouts: AtomicU64,
    leader_failures: AtomicU64,
    reused_results: AtomicU64,
    state_poisoned: AtomicU64,
}

impl SingleFlightCounters {
    fn snapshot(&self) -> SingleFlightSurfaceCounters {
        SingleFlightSurfaceCounters {
            leader_start_count: self.leader_starts.load(Ordering::SeqCst),
            completed_leader_count: self.completed_leaders.load(Ordering::SeqCst),
            follower_join_count: self.follower_joins.load(Ordering::SeqCst),
            follower_timeout_count: self.follower_timeouts.load(Ordering::SeqCst),
            leader_failure_count: self.leader_failures.load(Ordering::SeqCst),
            reused_result_count: self.reused_results.load(Ordering::SeqCst),
            state_poisoned_count: self.state_poisoned.load(Ordering::SeqCst),
        }
    }
}

#[must_use]
pub fn singleflight_posture_report() -> SingleFlightPostureReport {
    let graph_feature_enrichment = GRAPH_FEATURE_ENRICHMENT_GROUP.get().map_or_else(
        || {
            SingleFlightSurfacePosture::new(
                SingleFlightSurface::GraphFeatureEnrichment,
                true,
                0,
                SingleFlightSurfaceCounters::default(),
                duration_ms(GRAPH_FEATURE_ENRICHMENT_FOLLOWER_TIMEOUT),
                None,
            )
        },
        |group| {
            group.posture(
                SingleFlightSurface::GraphFeatureEnrichment,
                true,
                GRAPH_FEATURE_ENRICHMENT_FOLLOWER_TIMEOUT,
            )
        },
    );

    SingleFlightPostureReport::from_surfaces(vec![graph_feature_enrichment])
}

pub fn run_graph_feature_enrichment<F>(
    workspace_identity: &str,
    workspace_generation: u64,
    graph_generation: Option<u64>,
    source_mode: &str,
    options: &crate::graph::GraphFeatureEnrichmentOptions,
    operation: F,
) -> Result<SingleFlightRun<crate::graph::GraphFeatureEnrichmentReport>, SingleFlightError>
where
    F: FnOnce() -> crate::graph::GraphFeatureEnrichmentReport,
{
    run_graph_feature_enrichment_with_group(
        GraphFeatureEnrichmentSingleFlightInput {
            group: GRAPH_FEATURE_ENRICHMENT_GROUP.get_or_init(SingleFlightGroup::new),
            follower_timeout: GRAPH_FEATURE_ENRICHMENT_FOLLOWER_TIMEOUT,
            workspace_identity,
            workspace_generation,
            graph_generation,
            source_mode,
            options,
        },
        operation,
    )
}

struct GraphFeatureEnrichmentSingleFlightInput<'a> {
    group: &'a SingleFlightGroup<crate::graph::GraphFeatureEnrichmentReport>,
    follower_timeout: Duration,
    workspace_identity: &'a str,
    workspace_generation: u64,
    graph_generation: Option<u64>,
    source_mode: &'a str,
    options: &'a crate::graph::GraphFeatureEnrichmentOptions,
}

fn run_graph_feature_enrichment_with_group<F>(
    input: GraphFeatureEnrichmentSingleFlightInput<'_>,
    operation: F,
) -> Result<SingleFlightRun<crate::graph::GraphFeatureEnrichmentReport>, SingleFlightError>
where
    F: FnOnce() -> crate::graph::GraphFeatureEnrichmentReport,
{
    let max_features = input.options.max_features.to_string();
    let min_combined_score = stable_f64_option(input.options.min_combined_score);
    let max_selection_boost = stable_f64_option(input.options.max_selection_boost);
    let option_pairs = [
        ("source_mode", input.source_mode),
        ("max_features", max_features.as_str()),
        ("min_combined_score_bits", min_combined_score.as_str()),
        ("max_selection_boost_bits", max_selection_boost.as_str()),
    ];
    let mut key_input = SingleFlightKeyInput::new(
        SingleFlightSurface::GraphFeatureEnrichment,
        input.workspace_identity,
        input.workspace_generation,
        crate::graph::GRAPH_FEATURE_ENRICHMENT_SCHEMA_V1,
    );
    key_input.graph_generation = input.graph_generation;
    key_input.option_pairs = &option_pairs;
    let key = SingleFlightKey::from_input(&key_input);

    input
        .group
        .run(&key, input.follower_timeout, || Ok(operation()))
}

fn stable_f64_option(value: f64) -> String {
    format!("{:016x}", value.to_bits())
}

#[derive(Debug)]
struct SingleFlightEntry<T> {
    state: Mutex<SingleFlightState<T>>,
    ready: Condvar,
    followers: AtomicUsize,
}

impl<T> Default for SingleFlightEntry<T> {
    fn default() -> Self {
        Self {
            state: Mutex::new(SingleFlightState::Pending),
            ready: Condvar::new(),
            followers: AtomicUsize::new(0),
        }
    }
}

#[derive(Clone, Debug)]
enum SingleFlightState<T> {
    Pending,
    Completed(Result<T, String>),
}

fn duration_ms(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn capped_u32(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

fn trace_singleflight_checkpoint(
    phase: &'static str,
    key_hash: &str,
    elapsed_ms: u64,
    degraded_codes: &[&str],
) {
    tracing::info!(
        target: "ee::singleflight",
        workspace_id = SINGLEFLIGHT_TRACE_WORKSPACE_ID,
        request_id = SINGLEFLIGHT_TRACE_REQUEST_ID,
        bead_id = option_env!("EE_TRACE_BEAD_ID").unwrap_or("bd-gni47.3"),
        surface = SINGLEFLIGHT_TRACE_SURFACE,
        phase,
        elapsed_ms,
        degraded_codes = ?degraded_codes,
        key_hash = %key_hash,
        "single-flight checkpoint"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{
        CentralityRefreshReport, CentralityRefreshStatus, GraphFeatureEnrichmentOptions,
        MemoryCentralityScore, enrich_graph_features,
    };
    use std::sync::{
        Arc, Barrier,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    };
    use std::thread;

    type TestResult = Result<(), String>;

    fn key(label: &str) -> SingleFlightKey {
        let mut input = SingleFlightKeyInput::new(
            SingleFlightSurface::Context,
            "workspace-a",
            7,
            "ee.context.v1",
        );
        input.query_text = Some(label);
        SingleFlightKey::from_input(&input)
    }

    #[test]
    fn identical_concurrent_requests_share_one_leader() -> TestResult {
        let group = Arc::new(SingleFlightGroup::new());
        let key = key("same read");
        let calls = Arc::new(AtomicUsize::new(0));
        let barrier = Arc::new(Barrier::new(6));
        let mut handles = Vec::new();

        for _ in 0..6 {
            let group = Arc::clone(&group);
            let key = key.clone();
            let calls = Arc::clone(&calls);
            let barrier = Arc::clone(&barrier);
            handles.push(thread::spawn(move || {
                barrier.wait();
                group.run(&key, Duration::from_secs(5), || {
                    calls.fetch_add(1, Ordering::SeqCst);
                    thread::sleep(Duration::from_millis(50));
                    Ok("shared-result".to_owned())
                })
            }));
        }

        let mut leader_count = 0;
        let mut follower_count = 0;
        for handle in handles {
            let run = handle
                .join()
                .map_err(|_| "thread panicked".to_owned())?
                .map_err(|error| format!("single-flight run failed: {error:?}"))?;
            assert_eq!(run.value, "shared-result");
            match run.role {
                SingleFlightRole::Leader => leader_count += 1,
                SingleFlightRole::Follower => follower_count += 1,
            }
        }

        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(leader_count, 1);
        assert_eq!(follower_count, 5);
        assert_eq!(group.active_len().map_err(|error| format!("{error:?}"))?, 0);
        let posture = group.posture(
            SingleFlightSurface::Context,
            true,
            Duration::from_millis(250),
        );
        assert_eq!(posture.status, "idle");
        assert_eq!(posture.active_leader_count, 0);
        assert_eq!(posture.leader_start_count, 1);
        assert_eq!(posture.completed_leader_count, 1);
        assert_eq!(posture.follower_join_count, 5);
        assert_eq!(posture.reused_result_count, 5);
        Ok(())
    }

    #[test]
    fn posture_records_only_redaction_safe_last_key_generations() -> TestResult {
        let group = SingleFlightGroup::new();
        let mut input = SingleFlightKeyInput::new(
            SingleFlightSurface::Context,
            "/workspace/secret-path",
            11,
            "ee.context.v2",
        );
        input.index_generation = Some(17);
        input.graph_generation = Some(23);
        input.query_text = Some("raw query with token secret should never surface");
        let key = SingleFlightKey::from_input(&input);

        group
            .run(&key, Duration::from_secs(1), || Ok("ok".to_owned()))
            .map_err(|error| format!("single-flight run failed: {error}"))?;

        let posture = group.posture(SingleFlightSurface::Context, true, Duration::from_secs(1));
        let last_key = posture
            .last_key
            .as_ref()
            .ok_or_else(|| "posture should include last key".to_owned())?;
        assert_eq!(last_key.key_hash, key.key_hash);
        assert_eq!(last_key.workspace_generation, 11);
        assert_eq!(last_key.index_generation, Some(17));
        assert_eq!(last_key.graph_generation, Some(23));

        let serialized = serde_json::to_string(&posture)
            .map_err(|error| format!("serialize single-flight posture: {error}"))?;
        assert!(serialized.contains(&key.key_hash));
        assert!(!serialized.contains("raw query"));
        assert!(!serialized.contains("token secret"));
        assert!(!serialized.contains("/workspace/secret-path"));
        Ok(())
    }

    #[test]
    fn distinct_keys_execute_independently() -> TestResult {
        let group = Arc::new(SingleFlightGroup::new());
        let calls = Arc::new(AtomicUsize::new(0));
        let barrier = Arc::new(Barrier::new(4));
        let mut handles = Vec::new();

        for index in 0..4 {
            let group = Arc::clone(&group);
            let key = key(&format!("query-{index}"));
            let calls = Arc::clone(&calls);
            let barrier = Arc::clone(&barrier);
            handles.push(thread::spawn(move || {
                barrier.wait();
                group.run(&key, Duration::from_secs(5), || {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok(index)
                })
            }));
        }

        for handle in handles {
            let run = handle
                .join()
                .map_err(|_| "thread panicked".to_owned())?
                .map_err(|error| format!("single-flight run failed: {error:?}"))?;
            assert_eq!(run.role, SingleFlightRole::Leader);
            assert!(!run.shared);
        }
        assert_eq!(calls.load(Ordering::SeqCst), 4);
        Ok(())
    }

    #[test]
    fn follower_timeout_is_structured_and_does_not_cancel_leader() -> TestResult {
        let group = Arc::new(SingleFlightGroup::new());
        let key = key("slow read");
        let calls = Arc::new(AtomicUsize::new(0));
        let (leader_started_tx, leader_started_rx) = mpsc::channel();

        let leader_group = Arc::clone(&group);
        let leader_key = key.clone();
        let leader_calls = Arc::clone(&calls);
        let leader = thread::spawn(move || {
            leader_group.run(&leader_key, Duration::from_secs(5), || {
                leader_calls.fetch_add(1, Ordering::SeqCst);
                leader_started_tx
                    .send(())
                    .map_err(|error| format!("failed to signal leader start: {error}"))?;
                thread::sleep(Duration::from_millis(150));
                Ok("leader-finished".to_owned())
            })
        });

        leader_started_rx
            .recv_timeout(Duration::from_secs(1))
            .map_err(|error| format!("leader did not start: {error}"))?;
        let follower_error = match group.run(&key, Duration::from_millis(10), || {
            Ok("should-not-run".to_owned())
        }) {
            Ok(run) => return Err(format!("follower should time out, got {run:?}")),
            Err(error) => error,
        };
        assert_eq!(follower_error.code(), SINGLEFLIGHT_FOLLOWER_TIMEOUT_CODE);
        assert_eq!(follower_error.severity(), "medium");

        let leader_run = leader
            .join()
            .map_err(|_| "thread panicked".to_owned())?
            .map_err(|error| format!("leader failed unexpectedly: {error:?}"))?;
        assert_eq!(leader_run.value, "leader-finished");
        assert_eq!(leader_run.role, SingleFlightRole::Leader);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let posture = group.posture(SingleFlightSurface::Search, true, Duration::from_millis(10));
        assert_eq!(posture.status, "observed_failures");
        assert_eq!(posture.follower_timeout_count, 1);
        assert_eq!(posture.completed_leader_count, 1);
        Ok(())
    }

    #[test]
    fn telemetry_contract_names_required_events() {
        assert_eq!(
            SINGLEFLIGHT_REQUIRED_TELEMETRY_PHASES,
            [
                "leader_start",
                "follower_join",
                "follower_timeout",
                "leader_complete",
                "coalesced_result_reused",
            ]
        );
    }

    #[test]
    fn leader_failure_is_visible_to_waiting_followers() -> TestResult {
        let group = Arc::new(SingleFlightGroup::<String>::new());
        let key = key("failed read");
        let (leader_started_tx, leader_started_rx) = mpsc::channel();

        let leader_group = Arc::clone(&group);
        let leader_key = key.clone();
        let leader = thread::spawn(move || {
            leader_group.run(&leader_key, Duration::from_secs(5), || {
                leader_started_tx
                    .send(())
                    .map_err(|error| format!("failed to signal leader start: {error}"))?;
                thread::sleep(Duration::from_millis(50));
                Err("leader cancelled".to_owned())
            })
        });

        leader_started_rx
            .recv_timeout(Duration::from_secs(1))
            .map_err(|error| format!("leader did not start: {error}"))?;
        let follower_error = match group.run(&key, Duration::from_secs(5), || {
            Ok("should-not-run".to_owned())
        }) {
            Ok(run) => {
                return Err(format!(
                    "follower should observe leader failure, got {run:?}"
                ));
            }
            Err(error) => error,
        };
        assert_eq!(follower_error.code(), SINGLEFLIGHT_LEADER_FAILED_CODE);
        match follower_error {
            SingleFlightError::LeaderFailed { role, message, .. } => {
                assert_eq!(role, SingleFlightRole::Follower);
                assert_eq!(message, "leader cancelled");
            }
            other => return Err(format!("unexpected follower error: {other:?}")),
        }

        let leader_error = match leader.join().map_err(|_| "thread panicked".to_owned())? {
            Ok(run) => {
                return Err(format!(
                    "leader should return operation failure, got {run:?}"
                ));
            }
            Err(error) => error,
        };
        match leader_error {
            SingleFlightError::LeaderFailed { role, message, .. } => {
                assert_eq!(role, SingleFlightRole::Leader);
                assert_eq!(message, "leader cancelled");
            }
            other => return Err(format!("unexpected leader error: {other:?}")),
        }
        let posture = group.posture(
            SingleFlightSurface::GraphFeatureEnrichment,
            true,
            Duration::from_secs(5),
        );
        assert_eq!(posture.status, "observed_failures");
        assert_eq!(posture.leader_failure_count, 1);
        Ok(())
    }

    #[test]
    fn graph_feature_enrichment_wrapper_shares_identical_report() -> TestResult {
        let group = Arc::new(SingleFlightGroup::new());
        let leader_started = Arc::new(Barrier::new(2));
        let release_leader = Arc::new(Barrier::new(2));
        let executions = Arc::new(AtomicUsize::new(0));
        let options = GraphFeatureEnrichmentOptions::default();
        let mut handles = Vec::new();

        for _ in 0..4 {
            let group = Arc::clone(&group);
            let leader_started = Arc::clone(&leader_started);
            let release_leader = Arc::clone(&release_leader);
            let executions = Arc::clone(&executions);
            let options = options.clone();
            handles.push(thread::spawn(move || {
                run_graph_feature_enrichment_with_group(
                    GraphFeatureEnrichmentSingleFlightInput {
                        group: &group,
                        follower_timeout: Duration::from_secs(2),
                        workspace_identity: "/workspace/eidetic_engine_cli",
                        workspace_generation: 12,
                        graph_generation: Some(7),
                        source_mode: "graph_snapshot",
                        options: &options,
                    },
                    || {
                        executions.fetch_add(1, Ordering::SeqCst);
                        leader_started.wait();
                        release_leader.wait();
                        enrich_graph_features(&centrality_report(), &options)
                    },
                )
                .map_err(|error| format!("single-flight run failed: {error}"))
            }));
        }

        leader_started.wait();
        thread::sleep(Duration::from_millis(50));
        release_leader.wait();

        let mut leader_count = 0;
        let mut follower_count = 0;
        let mut reports = Vec::new();
        for handle in handles {
            let run = handle.join().map_err(|_| "thread panicked".to_owned())??;
            match run.role {
                SingleFlightRole::Leader => leader_count += 1,
                SingleFlightRole::Follower => follower_count += 1,
            }
            reports.push(run.value.data_json());
        }

        assert_eq!(executions.load(Ordering::SeqCst), 1);
        assert_eq!(leader_count, 1);
        assert_eq!(follower_count, 3);
        for report in reports.iter().skip(1) {
            assert_eq!(report, &reports[0]);
        }

        Ok(())
    }

    fn centrality_report() -> CentralityRefreshReport {
        let scores = vec![
            MemoryCentralityScore {
                memory_id: "mem_a".to_owned(),
                pagerank: 0.9,
                betweenness: 0.2,
            },
            MemoryCentralityScore {
                memory_id: "mem_b".to_owned(),
                pagerank: 0.3,
                betweenness: 0.8,
            },
        ];
        CentralityRefreshReport {
            version: env!("CARGO_PKG_VERSION"),
            status: CentralityRefreshStatus::Refreshed,
            dry_run: false,
            node_count: scores.len(),
            edge_count: 1,
            projection_ms: 0.0,
            pagerank_ms: 0.0,
            betweenness_ms: 0.0,
            total_ms: 0.0,
            top_pagerank: scores.clone(),
            top_betweenness: scores.clone(),
            scores,
        }
    }
}
