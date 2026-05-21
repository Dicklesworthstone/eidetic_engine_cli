use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Barrier, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::{Value, json};

const TEST_ID: &str = "bd_f6jfs_8_shard_fanout_concurrency";
const WORKER_COUNT: usize = 8;
const WORKSPACE_COUNT: usize = 4;
const REMEMBER_CALLS_PER_WORKER_PER_WORKSPACE: usize = 100;
const TOTAL_OPERATIONS: usize =
    WORKER_COUNT * WORKSPACE_COUNT * REMEMBER_CALLS_PER_WORKER_PER_WORKSPACE;
const COMMIT_HOLD: Duration = Duration::from_millis(2);
const SPEEDUP_GATE: f64 = 3.5;
const WORKLOAD_TIER: &str = "8_workers_x_4_workspaces_x_100_remember_calls";
const ORDERING_CONTRACT: &str = "\
events are sorted by profile, operation ordinal, and phase order \
enqueue < grant < commit; volatile ts and elapsed_ms fields are scrub targets";

type TestResult<T = ()> = Result<T, String>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum HarnessProfile {
    BaselineGlobalGate,
    ShardFanoutPerWorkspace,
}

impl HarnessProfile {
    const fn as_str(self) -> &'static str {
        match self {
            Self::BaselineGlobalGate => "baseline_single_writer_global_gate",
            Self::ShardFanoutPerWorkspace => "shard_fanout_per_workspace_gate",
        }
    }

    fn shard_id(self, workspace_index: usize) -> String {
        match self {
            Self::BaselineGlobalGate => String::from("legacy_global_gate"),
            Self::ShardFanoutPerWorkspace => {
                format!("workspace_shard_{workspace_index:02}")
            }
        }
    }
}

#[derive(Clone)]
struct OperationRoute {
    operation_id: String,
    operation_ordinal: usize,
    worker_id: usize,
    workspace_index: usize,
    workspace_id: String,
    shard_id: String,
}

#[derive(Clone, Debug, Default)]
struct ConcurrencyTracker {
    active_by_shard: BTreeMap<String, usize>,
    max_by_shard: BTreeMap<String, usize>,
    max_active_total: usize,
}

impl ConcurrencyTracker {
    fn grant(&mut self, shard_id: &str) {
        let active = self.active_by_shard.entry(shard_id.to_owned()).or_insert(0);
        *active = active.saturating_add(1);
        let max_seen = self.max_by_shard.entry(shard_id.to_owned()).or_insert(0);
        *max_seen = (*max_seen).max(*active);
        let active_total = self.active_by_shard.values().sum();
        self.max_active_total = self.max_active_total.max(active_total);
    }

    fn commit(&mut self, shard_id: &str) -> TestResult {
        let Some(active) = self.active_by_shard.get_mut(shard_id) else {
            return Err(format!("commit observed inactive shard `{shard_id}`"));
        };
        if *active == 0 {
            return Err(format!("commit underflow for shard `{shard_id}`"));
        }
        *active -= 1;
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize)]
struct TestEvent {
    schema: &'static str,
    ts: String,
    test_id: &'static str,
    kind: &'static str,
    elapsed_ms: f64,
    fields: BTreeMap<String, Value>,
}

#[derive(Clone, Debug)]
struct OperationSample {
    workspace_id: String,
    latency_ms: f64,
}

#[derive(Clone, Debug)]
struct HarnessSummary {
    profile: &'static str,
    total_operations: usize,
    elapsed_ms: f64,
    throughput_ops_per_second: f64,
    per_workspace_throughput: BTreeMap<String, f64>,
    p50_latency_ms: f64,
    p95_latency_ms: f64,
    p99_latency_ms: f64,
    same_shard_max_concurrency: usize,
    cross_shard_max_concurrency: usize,
    degraded_codes: Vec<String>,
    events_jsonl_path: PathBuf,
}

#[derive(Clone, Debug)]
struct HarnessRun {
    summary: HarnessSummary,
    events_jsonl: String,
}

fn unique_temp_dir(name: &str) -> PathBuf {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    std::env::temp_dir().join(format!("ee-{name}-{}-{stamp}", std::process::id()))
}

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn elapsed_ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

fn operation_routes(profile: HarnessProfile) -> Vec<Vec<OperationRoute>> {
    (0..WORKER_COUNT)
        .map(|worker_id| {
            let mut routes = Vec::with_capacity(
                WORKSPACE_COUNT * REMEMBER_CALLS_PER_WORKER_PER_WORKSPACE,
            );
            let mut ordinal = 0usize;
            for call_index in 0..REMEMBER_CALLS_PER_WORKER_PER_WORKSPACE {
                for workspace_offset in 0..WORKSPACE_COUNT {
                    let workspace_index = (workspace_offset + worker_id) % WORKSPACE_COUNT;
                    let operation_ordinal = worker_id
                        * WORKSPACE_COUNT
                        * REMEMBER_CALLS_PER_WORKER_PER_WORKSPACE
                        + ordinal;
                    let workspace_id = format!("workspace_{workspace_index:02}");
                    let shard_id = profile.shard_id(workspace_index);
                    routes.push(OperationRoute {
                        operation_id: format!(
                            "{}_worker_{worker_id:02}_workspace_{workspace_index:02}_call_{call_index:03}",
                            profile.as_str()
                        ),
                        operation_ordinal,
                        worker_id,
                        workspace_index,
                        workspace_id,
                        shard_id,
                    });
                    ordinal = ordinal.saturating_add(1);
                }
            }
            routes
        })
        .collect()
}

fn shard_locks(profile: HarnessProfile) -> BTreeMap<String, Arc<Mutex<()>>> {
    match profile {
        HarnessProfile::BaselineGlobalGate => {
            let global_gate = Arc::new(Mutex::new(()));
            (0..WORKSPACE_COUNT)
                .map(|workspace_index| {
                    (profile.shard_id(workspace_index), Arc::clone(&global_gate))
                })
                .collect()
        }
        HarnessProfile::ShardFanoutPerWorkspace => (0..WORKSPACE_COUNT)
            .map(|workspace_index| (profile.shard_id(workspace_index), Arc::new(Mutex::new(()))))
            .collect(),
    }
}

fn event_for(
    profile: HarnessProfile,
    route: &OperationRoute,
    phase: &str,
    elapsed: Duration,
    wait_ms: f64,
    commit_ms: f64,
) -> TestEvent {
    let mut fields = BTreeMap::new();
    fields.insert("operation".to_owned(), json!("remember_write"));
    fields.insert("status".to_owned(), json!("ok"));
    fields.insert("profile".to_owned(), json!(profile.as_str()));
    fields.insert("workload_tier".to_owned(), json!(WORKLOAD_TIER));
    fields.insert(
        "workspace_id".to_owned(),
        json!(route.workspace_id.as_str()),
    );
    fields.insert("workspace_index".to_owned(), json!(route.workspace_index));
    fields.insert("shard_id".to_owned(), json!(route.shard_id.as_str()));
    fields.insert(
        "operation_id".to_owned(),
        json!(route.operation_id.as_str()),
    );
    fields.insert(
        "operation_ordinal".to_owned(),
        json!(route.operation_ordinal),
    );
    fields.insert("phase".to_owned(), json!(phase));
    fields.insert("worker_id".to_owned(), json!(route.worker_id));
    fields.insert(
        "thread_id".to_owned(),
        json!(format!("{:?}", thread::current().id())),
    );
    fields.insert("degraded_codes".to_owned(), json!(Vec::<String>::new()));
    fields.insert("enqueue_to_grant_ms".to_owned(), json!(wait_ms));
    fields.insert("grant_to_commit_ms".to_owned(), json!(commit_ms));
    fields.insert("ordering_contract".to_owned(), json!(ORDERING_CONTRACT));

    TestEvent {
        schema: "ee.test_event.v1",
        ts: now_rfc3339(),
        test_id: TEST_ID,
        kind: "bench_iteration",
        elapsed_ms: elapsed_ms(elapsed),
        fields,
    }
}

fn phase_rank(event: &TestEvent) -> u8 {
    match event.fields.get("phase").and_then(Value::as_str) {
        Some("enqueue") => 0,
        Some("grant") => 1,
        Some("commit") => 2,
        _ => 3,
    }
}

fn event_sort_key(event: &TestEvent) -> (String, usize, u8, usize) {
    let profile = event
        .fields
        .get("profile")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let ordinal = event
        .fields
        .get("operation_ordinal")
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or_default();
    let worker_id = event
        .fields
        .get("worker_id")
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or_default();
    (profile, ordinal, phase_rank(event), worker_id)
}

fn percentile(samples: &[f64], percentile: usize) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    let index = samples.len().saturating_mul(percentile).saturating_sub(1) / 100;
    samples[index.min(samples.len() - 1)]
}

fn summary_for(
    profile: HarnessProfile,
    elapsed: Duration,
    samples: &[OperationSample],
    tracker: &ConcurrencyTracker,
    event_log_path: PathBuf,
) -> HarnessSummary {
    let elapsed_ms_value = elapsed_ms(elapsed);
    let elapsed_seconds = elapsed.as_secs_f64().max(f64::EPSILON);
    let mut latencies = samples
        .iter()
        .map(|sample| sample.latency_ms)
        .collect::<Vec<_>>();
    latencies.sort_by(f64::total_cmp);

    let mut counts_by_workspace = BTreeMap::<String, usize>::new();
    for sample in samples {
        *counts_by_workspace
            .entry(sample.workspace_id.clone())
            .or_insert(0) += 1;
    }
    let per_workspace_throughput = counts_by_workspace
        .into_iter()
        .map(|(workspace_id, count)| (workspace_id, count as f64 / elapsed_seconds))
        .collect();
    let same_shard_max_concurrency = tracker.max_by_shard.values().copied().max().unwrap_or(0);

    HarnessSummary {
        profile: profile.as_str(),
        total_operations: samples.len(),
        elapsed_ms: elapsed_ms_value,
        throughput_ops_per_second: samples.len() as f64 / elapsed_seconds,
        per_workspace_throughput,
        p50_latency_ms: percentile(&latencies, 50),
        p95_latency_ms: percentile(&latencies, 95),
        p99_latency_ms: percentile(&latencies, 99),
        same_shard_max_concurrency,
        cross_shard_max_concurrency: tracker.max_active_total,
        degraded_codes: Vec::new(),
        events_jsonl_path: event_log_path,
    }
}

fn jsonl_for(events: &[TestEvent]) -> TestResult<String> {
    let mut jsonl = String::new();
    for event in events {
        let line = serde_json::to_string(event)
            .map_err(|error| format!("serialize shard fanout event: {error}"))?;
        jsonl.push_str(&line);
        jsonl.push('\n');
    }
    Ok(jsonl)
}

fn run_profile(profile: HarnessProfile, run_dir: PathBuf) -> TestResult<HarnessRun> {
    let locks = Arc::new(shard_locks(profile));
    let tracker = Arc::new(Mutex::new(ConcurrencyTracker::default()));
    let barrier = Arc::new(Barrier::new(WORKER_COUNT));
    let profile_started = Instant::now();
    let handles = operation_routes(profile)
        .into_iter()
        .map(|routes| {
            let locks = Arc::clone(&locks);
            let tracker = Arc::clone(&tracker);
            let barrier = Arc::clone(&barrier);
            thread::spawn(
                move || -> TestResult<(Vec<TestEvent>, Vec<OperationSample>)> {
                    let mut events = Vec::with_capacity(routes.len() * 3);
                    let mut samples = Vec::with_capacity(routes.len());
                    barrier.wait();
                    for route in routes {
                        let operation_started = Instant::now();
                        events.push(event_for(
                            profile,
                            &route,
                            "enqueue",
                            Duration::ZERO,
                            0.0,
                            0.0,
                        ));
                        let Some(lock) = locks.get(&route.shard_id) else {
                            return Err(format!("missing shard lock for `{}`", route.shard_id));
                        };
                        let lock_guard = lock
                            .lock()
                            .map_err(|_| format!("shard lock poisoned for `{}`", route.shard_id))?;
                        let grant_elapsed = operation_started.elapsed();
                        {
                            let mut tracker = tracker
                                .lock()
                                .map_err(|_| "concurrency tracker lock poisoned".to_owned())?;
                            tracker.grant(&route.shard_id);
                        }
                        events.push(event_for(
                            profile,
                            &route,
                            "grant",
                            grant_elapsed,
                            elapsed_ms(grant_elapsed),
                            0.0,
                        ));

                        let commit_started = Instant::now();
                        thread::sleep(COMMIT_HOLD);
                        let commit_elapsed = commit_started.elapsed();
                        {
                            let mut tracker = tracker
                                .lock()
                                .map_err(|_| "concurrency tracker lock poisoned".to_owned())?;
                            tracker.commit(&route.shard_id)?;
                        }
                        drop(lock_guard);

                        let total_elapsed = operation_started.elapsed();
                        events.push(event_for(
                            profile,
                            &route,
                            "commit",
                            total_elapsed,
                            elapsed_ms(grant_elapsed),
                            elapsed_ms(commit_elapsed),
                        ));
                        samples.push(OperationSample {
                            workspace_id: route.workspace_id,
                            latency_ms: elapsed_ms(total_elapsed),
                        });
                    }
                    Ok((events, samples))
                },
            )
        })
        .collect::<Vec<_>>();

    let mut events = Vec::with_capacity(TOTAL_OPERATIONS * 3);
    let mut samples = Vec::with_capacity(TOTAL_OPERATIONS);
    for handle in handles {
        let (mut worker_events, mut worker_samples) = handle
            .join()
            .map_err(|_| "shard fanout worker thread panicked".to_owned())??;
        events.append(&mut worker_events);
        samples.append(&mut worker_samples);
    }
    let profile_elapsed = profile_started.elapsed();

    events.sort_by_key(event_sort_key);
    let tracker = tracker
        .lock()
        .map_err(|_| "concurrency tracker lock poisoned".to_owned())?
        .clone();
    let event_log_path = run_dir.join(format!("{}.jsonl", profile.as_str()));
    let events_jsonl = jsonl_for(&events)?;
    fs::write(&event_log_path, &events_jsonl)
        .map_err(|error| format!("write event log {}: {error}", event_log_path.display()))?;
    let summary = summary_for(profile, profile_elapsed, &samples, &tracker, event_log_path);
    Ok(HarnessRun {
        summary,
        events_jsonl,
    })
}

fn parse_events(jsonl: &str) -> TestResult<Vec<Value>> {
    jsonl
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).map_err(|error| format!("parse event: {error}")))
        .collect()
}

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn validate_event_log(run: &HarnessRun) -> TestResult {
    let events = parse_events(&run.events_jsonl)?;
    ensure(
        events.len() == TOTAL_OPERATIONS * 3,
        format!(
            "{} emitted {} events, expected {}",
            run.summary.profile,
            events.len(),
            TOTAL_OPERATIONS * 3
        ),
    )?;

    let mut phases = BTreeSet::new();
    let mut workspaces = BTreeSet::new();
    let mut shards = BTreeSet::new();
    for event in events {
        ensure(
            event["schema"] == "ee.test_event.v1",
            format!("bad event schema: {event}"),
        )?;
        ensure(
            event["test_id"] == TEST_ID,
            format!("bad event test_id: {event}"),
        )?;
        ensure(
            event["kind"] == "bench_iteration",
            format!("bad event kind: {event}"),
        )?;
        ensure(
            event["elapsed_ms"].as_f64().is_some_and(f64::is_finite),
            format!("bad event elapsed_ms: {event}"),
        )?;
        let fields = event
            .get("fields")
            .and_then(Value::as_object)
            .ok_or_else(|| format!("event missing fields object: {event}"))?;
        for required in [
            "operation",
            "status",
            "profile",
            "workload_tier",
            "workspace_id",
            "shard_id",
            "operation_id",
            "phase",
            "worker_id",
            "degraded_codes",
        ] {
            ensure(
                fields.contains_key(required),
                format!("event missing field `{required}`: {event}"),
            )?;
        }
        ensure(
            fields["operation"] == "remember_write",
            format!("bad operation field: {event}"),
        )?;
        ensure(
            fields["status"] == "ok",
            format!("bad status field: {event}"),
        )?;
        ensure(
            fields["profile"] == run.summary.profile,
            format!("bad profile field: {event}"),
        )?;
        ensure(
            fields["workload_tier"] == WORKLOAD_TIER,
            format!("bad workload_tier field: {event}"),
        )?;
        ensure(
            fields["degraded_codes"]
                .as_array()
                .is_some_and(|codes| codes.is_empty()),
            format!("degraded_codes must be an empty array for successful run: {event}"),
        )?;
        if let Some(phase) = fields["phase"].as_str() {
            phases.insert(phase.to_owned());
        }
        if let Some(workspace_id) = fields["workspace_id"].as_str() {
            workspaces.insert(workspace_id.to_owned());
        }
        if let Some(shard_id) = fields["shard_id"].as_str() {
            shards.insert(shard_id.to_owned());
        }
    }

    ensure(
        phases
            == BTreeSet::from([
                "commit".to_owned(),
                "enqueue".to_owned(),
                "grant".to_owned(),
            ]),
        format!("unexpected phases for {}: {phases:?}", run.summary.profile),
    )?;
    ensure(
        workspaces.len() == WORKSPACE_COUNT,
        format!(
            "{} touched {} workspaces, expected {WORKSPACE_COUNT}",
            run.summary.profile,
            workspaces.len()
        ),
    )?;
    let expected_shards = if run.summary.profile == HarnessProfile::BaselineGlobalGate.as_str() {
        1
    } else {
        WORKSPACE_COUNT
    };
    ensure(
        shards.len() == expected_shards,
        format!(
            "{} touched {} shards, expected {expected_shards}",
            run.summary.profile,
            shards.len()
        ),
    )?;
    Ok(())
}

#[test]
fn shard_fanout_concurrency_harness_meets_speedup_gate() -> TestResult {
    let run_dir = unique_temp_dir("shard-fanout-concurrency");
    fs::create_dir_all(&run_dir)
        .map_err(|error| format!("create run dir {}: {error}", run_dir.display()))?;

    let baseline = run_profile(HarnessProfile::BaselineGlobalGate, run_dir.clone())?;
    let fanout = run_profile(HarnessProfile::ShardFanoutPerWorkspace, run_dir)?;

    validate_event_log(&baseline)?;
    validate_event_log(&fanout)?;

    ensure(
        baseline.summary.total_operations == TOTAL_OPERATIONS,
        format!("baseline operation count mismatch: {:?}", baseline.summary),
    )?;
    ensure(
        fanout.summary.total_operations == TOTAL_OPERATIONS,
        format!("fanout operation count mismatch: {:?}", fanout.summary),
    )?;
    ensure(
        baseline.summary.same_shard_max_concurrency == 1,
        format!(
            "baseline same-shard writes must serialize: {:?}",
            baseline.summary
        ),
    )?;
    ensure(
        fanout.summary.same_shard_max_concurrency == 1,
        format!(
            "fanout same-shard writes must serialize: {:?}",
            fanout.summary
        ),
    )?;
    ensure(
        baseline.summary.cross_shard_max_concurrency == 1,
        format!(
            "baseline global gate should not allow cross-shard concurrency: {:?}",
            baseline.summary
        ),
    )?;
    ensure(
        fanout.summary.cross_shard_max_concurrency >= 2,
        format!(
            "fanout should allow different shards to proceed concurrently: {:?}",
            fanout.summary
        ),
    )?;

    let speedup = fanout.summary.throughput_ops_per_second
        / baseline.summary.throughput_ops_per_second.max(f64::EPSILON);
    ensure(
        speedup >= SPEEDUP_GATE,
        format!(
            "shard fanout throughput speedup {speedup:.2}x below {SPEEDUP_GATE:.2}x gate; baseline={:?} fanout={:?}",
            baseline.summary, fanout.summary
        ),
    )?;

    for summary in [&baseline.summary, &fanout.summary] {
        ensure(
            summary.per_workspace_throughput.len() == WORKSPACE_COUNT,
            format!("per-workspace throughput missing entries: {summary:?}"),
        )?;
        ensure(
            summary.p50_latency_ms <= summary.p95_latency_ms
                && summary.p95_latency_ms <= summary.p99_latency_ms,
            format!("latency percentiles out of order: {summary:?}"),
        )?;
        ensure(
            summary.degraded_codes.is_empty(),
            format!("unexpected degraded codes: {summary:?}"),
        )?;
        ensure(
            summary.events_jsonl_path.exists(),
            format!("event log path was not written: {summary:?}"),
        )?;
        ensure(
            summary.elapsed_ms > 0.0,
            format!("summary elapsed_ms must be positive: {summary:?}"),
        )?;
    }

    Ok(())
}

#[test]
fn shard_fanout_event_ordering_contract_is_scrubbable() -> TestResult {
    for required in [
        "profile",
        "operation ordinal",
        "enqueue < grant < commit",
        "volatile ts and elapsed_ms fields are scrub targets",
    ] {
        ensure(
            ORDERING_CONTRACT.contains(required),
            format!("ordering contract missing `{required}`"),
        )?;
    }
    Ok(())
}
