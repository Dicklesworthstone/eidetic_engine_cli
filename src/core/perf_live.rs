//! Read-only live performance snapshots for swarm observability.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;

use crate::core::status::{DerivedAssetStatus, StatusOptions, StatusReport};
use crate::core::swarm_brief::{SwarmBriefCommandError, SwarmBriefCommandRunner};
use crate::models::DomainError;

pub const PERF_LIVE_SCHEMA_V1: &str = "ee.perf.live.v1";
pub const PERF_LIVE_BEAD_ID: &str = "bd-1zwi4";

const DEFAULT_INTERVAL_MS: u64 = 1_000;
const DEFAULT_COMMAND_TIMEOUT_MS: u64 = 500;
const READ_ONLY_REDACTION_STATUS: &str = "counts_metrics_codes_only_no_content";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PerfLiveOptions {
    pub workspace: PathBuf,
    pub interval_ms: u64,
    pub window_ms: Option<u64>,
    pub command_timeout_ms: u64,
    pub timestamp_override: Option<String>,
}

impl PerfLiveOptions {
    #[must_use]
    pub fn for_workspace(workspace: impl Into<PathBuf>) -> Self {
        Self {
            workspace: workspace.into(),
            interval_ms: DEFAULT_INTERVAL_MS,
            window_ms: None,
            command_timeout_ms: DEFAULT_COMMAND_TIMEOUT_MS,
            timestamp_override: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PerfLiveSnapshot {
    pub schema: &'static str,
    pub ts: String,
    pub interval_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window_ms: Option<u64>,
    pub side_effect_free: bool,
    pub redaction_status: &'static str,
    pub bead_id: &'static str,
    pub surfaces: PerfLiveSurfaces,
    pub read_pool: PerfLiveReadPool,
    pub audit_lane: PerfLiveAuditLane,
    pub l2_cache: PerfLiveL2Cache,
    pub rch: PerfLiveRch,
    pub graph_snapshot: PerfLiveGraphSnapshot,
    pub host_pressure: PerfLiveHostPressure,
    pub bead_activity: PerfLiveBeadActivity,
    pub degraded: Vec<PerfLiveDegradation>,
}

impl PerfLiveSnapshot {
    #[must_use]
    pub fn to_json(&self) -> String {
        crate::core::serialize_or_error(self)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PerfLiveSurfaces {
    pub context: PerfLiveSurfaceStats,
    pub search: PerfLiveSurfaceStats,
    pub remember: PerfLiveSurfaceStats,
    pub why: PerfLiveSurfaceStats,
    pub pack_build: PerfLiveSurfaceStats,
}

impl Default for PerfLiveSurfaces {
    fn default() -> Self {
        Self {
            context: PerfLiveSurfaceStats::for_surface("context"),
            search: PerfLiveSurfaceStats::for_surface("search"),
            remember: PerfLiveSurfaceStats::for_surface("remember"),
            why: PerfLiveSurfaceStats::for_surface("why"),
            pack_build: PerfLiveSurfaceStats::for_surface("pack_build"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PerfLiveSurfaceStats {
    pub surface: &'static str,
    pub p50_ms: u64,
    pub p95_ms: u64,
    pub p99_ms: u64,
    pub p999_ms: u64,
    pub qps: f64,
    pub inflight: u64,
    pub qos_class_counts: BTreeMap<String, u64>,
}

impl PerfLiveSurfaceStats {
    #[must_use]
    pub fn for_surface(surface: &'static str) -> Self {
        Self {
            surface,
            p50_ms: 0,
            p95_ms: 0,
            p99_ms: 0,
            p999_ms: 0,
            qps: 0.0,
            inflight: 0,
            qos_class_counts: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PerfLiveReadPool {
    pub active_pins: u64,
    pub expired_pins: u64,
    pub release_failures: u64,
    pub queue_depth: u64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PerfLiveAuditLane {
    pub batch_count: u64,
    pub batch_size_p50: u64,
    pub batch_size_p99: u64,
    pub backpressure_events: u64,
    pub channel_depth: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PerfLiveL2Cache {
    pub status: String,
    pub hits: u64,
    pub misses: u64,
    pub hit_rate_basis_points: u16,
    pub byte_size: u64,
    pub evictions: u64,
}

impl Default for PerfLiveL2Cache {
    fn default() -> Self {
        Self {
            status: "not_inspected".to_owned(),
            hits: 0,
            misses: 0,
            hit_rate_basis_points: 0,
            byte_size: 0,
            evictions: 0,
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PerfLiveRch {
    pub workers_healthy: u64,
    pub slots_available: Option<u64>,
    pub queue_depth: u64,
    pub head_of_line_age_ms: Option<u64>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PerfLiveGraphSnapshot {
    pub age_ms: Option<u64>,
    pub refreshed_count: u64,
    pub refresh_lock_wait_ms_p99: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PerfLiveHostPressure {
    pub cpu_user_pct: Option<f64>,
    pub cpu_iowait_pct: Option<f64>,
    pub memory_rss_mb: Option<u64>,
    pub page_cache_mb: Option<u64>,
    pub fsync_latency_p99_ms: Option<u64>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PerfLiveBeadActivity {
    pub active_agents: u64,
    pub ready_beads: u64,
    pub in_progress_beads: u64,
    pub blocked_beads: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PerfLiveDegradation {
    pub code: &'static str,
    pub source: &'static str,
    pub severity: &'static str,
    pub message: String,
    pub repair: Option<String>,
}

impl PerfLiveDegradation {
    #[must_use]
    pub fn warning(
        code: &'static str,
        source: &'static str,
        message: impl Into<String>,
        repair: impl Into<Option<String>>,
    ) -> Self {
        Self {
            code,
            source,
            severity: "warning",
            message: message.into(),
            repair: repair.into(),
        }
    }
}

#[must_use]
pub fn default_perf_live_interval_ms() -> u64 {
    DEFAULT_INTERVAL_MS
}

#[must_use]
pub fn default_perf_live_command_timeout_ms() -> u64 {
    DEFAULT_COMMAND_TIMEOUT_MS
}

pub fn parse_perf_live_duration_ms(value: &str) -> Result<u64, DomainError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(perf_live_duration_error(value));
    }
    let normalized = trimmed.to_ascii_lowercase();
    let (number, multiplier) = if normalized.ends_with("ms") {
        (&trimmed[..trimmed.len() - 2], 1)
    } else if normalized.ends_with('s') {
        (&trimmed[..trimmed.len() - 1], 1_000)
    } else if normalized.ends_with('m') {
        (&trimmed[..trimmed.len() - 1], 60_000)
    } else {
        (trimmed, 1)
    };
    let parsed = number
        .trim()
        .parse::<u64>()
        .map_err(|_| perf_live_duration_error(value))?;
    parsed
        .checked_mul(multiplier)
        .filter(|duration| *duration > 0)
        .ok_or_else(|| perf_live_duration_error(value))
}

fn perf_live_duration_error(value: &str) -> DomainError {
    DomainError::Usage {
        message: format!("Invalid perf live duration `{value}`."),
        repair: Some("Use a positive duration such as 1000ms, 1s, or 30s.".to_owned()),
    }
}

pub fn collect_perf_live_snapshot<R: SwarmBriefCommandRunner>(
    options: &PerfLiveOptions,
    runner: &R,
) -> PerfLiveSnapshot {
    let status = StatusReport::gather_with_options(&StatusOptions {
        workspace_path: Some(options.workspace.clone()),
    });
    let mut degraded = status_degradations(&status);
    let surfaces = PerfLiveSurfaces::default();
    degraded.push(PerfLiveDegradation::warning(
        "perf_live_surface_metrics_unavailable",
        "surfaces",
        "Live per-surface span counters are not yet wired; latency and QPS fields are zero-filled.",
        Some("Wire tracing span counters for context/search/remember/why/pack_build.".to_owned()),
    ));

    let read_pool = PerfLiveReadPool {
        active_pins: usize_to_u64(status.read_pool.active_pins),
        expired_pins: usize_to_u64(status.read_pool.expired_pins),
        release_failures: status.read_pool.release_failures,
        queue_depth: usize_to_u64(status.read_pool.acquire_wait.samples),
    };
    let audit_lane = PerfLiveAuditLane::default();
    degraded.push(PerfLiveDegradation::warning(
        "perf_live_audit_lane_counters_unavailable",
        "auditLane",
        "Audit-lane global counters are not yet published by the source bead; audit lane fields are zero-filled.",
        Some("Finish bd-wp5ac counter publication and route it into perf live.".to_owned()),
    ));

    let l2_cache = l2_cache_snapshot(&status, &mut degraded);
    let rch = rch_snapshot(
        &options.workspace,
        options.command_timeout_ms,
        runner,
        &mut degraded,
    );
    let graph_snapshot = graph_snapshot(&status);
    let host_pressure = host_pressure(&mut degraded);
    let bead_activity = bead_activity(
        &options.workspace,
        options.command_timeout_ms,
        runner,
        &mut degraded,
    );

    degraded.sort_by(|left, right| {
        left.code
            .cmp(right.code)
            .then_with(|| left.source.cmp(right.source))
            .then_with(|| left.message.cmp(&right.message))
    });
    degraded.dedup();

    PerfLiveSnapshot {
        schema: PERF_LIVE_SCHEMA_V1,
        ts: options
            .timestamp_override
            .clone()
            .unwrap_or_else(|| Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)),
        interval_ms: options.interval_ms,
        window_ms: options.window_ms,
        side_effect_free: true,
        redaction_status: READ_ONLY_REDACTION_STATUS,
        bead_id: PERF_LIVE_BEAD_ID,
        surfaces,
        read_pool,
        audit_lane,
        l2_cache,
        rch,
        graph_snapshot,
        host_pressure,
        bead_activity,
        degraded,
    }
}

fn status_degradations(status: &StatusReport) -> Vec<PerfLiveDegradation> {
    status
        .degradations
        .iter()
        .map(|degradation| {
            PerfLiveDegradation::warning(
                "perf_live_status_degraded",
                "status",
                format!("{}: {}", degradation.code, degradation.message),
                Some(degradation.repair.to_owned()),
            )
        })
        .collect()
}

fn l2_cache_snapshot(
    status: &StatusReport,
    degraded: &mut Vec<PerfLiveDegradation>,
) -> PerfLiveL2Cache {
    let Some(asset) = status
        .derived_assets
        .iter()
        .find(|asset| asset.name == "pack_l2_cache")
    else {
        degraded.push(PerfLiveDegradation::warning(
            "perf_live_l2_cache_source_degraded",
            "l2Cache",
            "L2 pack cache status was not present in the status report.",
            Some("Run ee status --json and inspect derivedAssets.".to_owned()),
        ));
        return PerfLiveL2Cache::default();
    };

    if asset.status != DerivedAssetStatus::Current {
        degraded.push(PerfLiveDegradation::warning(
            "perf_live_l2_cache_source_degraded",
            "l2Cache",
            format!("L2 pack cache status is {}.", asset.status.as_str()),
            asset.repair.map(str::to_owned),
        ));
    }

    PerfLiveL2Cache {
        status: asset.status.as_str().to_owned(),
        ..PerfLiveL2Cache::default()
    }
}

fn graph_snapshot(status: &StatusReport) -> PerfLiveGraphSnapshot {
    let refreshed_count = if matches!(
        status.graph_snapshot_artifact.status,
        DerivedAssetStatus::Current
    ) {
        1
    } else {
        0
    };
    let age_ms = status
        .graph_snapshot_artifact
        .last_built_at
        .as_deref()
        .and_then(age_ms_since_rfc3339);
    PerfLiveGraphSnapshot {
        age_ms,
        refreshed_count,
        refresh_lock_wait_ms_p99: 0,
    }
}

fn age_ms_since_rfc3339(ts: &str) -> Option<u64> {
    let observed = DateTime::parse_from_rfc3339(ts).ok()?.with_timezone(&Utc);
    let elapsed = Utc::now().signed_duration_since(observed);
    elapsed.num_milliseconds().try_into().ok()
}

fn rch_snapshot<R: SwarmBriefCommandRunner>(
    workspace: &Path,
    timeout_ms: u64,
    runner: &R,
    degraded: &mut Vec<PerfLiveDegradation>,
) -> PerfLiveRch {
    match runner.run(
        "rch",
        &["status", "--workers", "--jobs", "--json"],
        workspace,
        timeout_ms,
    ) {
        Ok(output) => parse_rch_snapshot_json(&output.stdout).unwrap_or_else(|message| {
            degraded.push(PerfLiveDegradation::warning(
                "perf_live_rch_source_degraded",
                "rch",
                message,
                Some("Run rch status --workers --jobs --json.".to_owned()),
            ));
            PerfLiveRch::default()
        }),
        Err(error) => {
            degraded.push(command_degradation(
                "perf_live_rch_source_degraded",
                "rch",
                error,
                "Run rch status --workers --jobs --json.",
            ));
            PerfLiveRch::default()
        }
    }
}

fn parse_rch_snapshot_json(input: &str) -> Result<PerfLiveRch, String> {
    let value = serde_json::from_str::<Value>(input)
        .map_err(|error| format!("RCH JSON parse error: {error}"))?;
    let workers_healthy = numeric_field_any(
        &value,
        &[
            "workers_healthy",
            "workersHealthy",
            "healthy_workers",
            "healthyWorkers",
        ],
    )
    .or_else(|| infer_healthy_workers(&value))
    .unwrap_or_default();
    let slots_available = numeric_field_any(&value, &["slots_available", "slotsAvailable"]);
    let queue_depth = numeric_field_any(
        &value,
        &[
            "queue_depth",
            "queueDepth",
            "queued",
            "queued_count",
            "queuedCount",
        ],
    )
    .unwrap_or_default();
    let head_of_line_age_ms = numeric_field_any(
        &value,
        &[
            "head_of_line_age_ms",
            "headOfLineAgeMs",
            "queue_head_age_ms",
            "queueHeadAgeMs",
        ],
    );
    Ok(PerfLiveRch {
        workers_healthy,
        slots_available,
        queue_depth,
        head_of_line_age_ms,
    })
}

fn infer_healthy_workers(value: &Value) -> Option<u64> {
    let workers = value
        .get("workers")
        .or_else(|| value.pointer("/data/workers"))
        .and_then(Value::as_array)?;
    Some(
        workers
            .iter()
            .filter(|worker| {
                string_field_any(worker, &["status", "health", "state"])
                    .is_some_and(is_healthy_worker_status)
            })
            .count() as u64,
    )
}

fn is_healthy_worker_status(status: &str) -> bool {
    matches!(
        status.trim().to_ascii_lowercase().as_str(),
        "healthy" | "ready" | "ok" | "available"
    )
}

fn bead_activity<R: SwarmBriefCommandRunner>(
    workspace: &Path,
    timeout_ms: u64,
    runner: &R,
    degraded: &mut Vec<PerfLiveDegradation>,
) -> PerfLiveBeadActivity {
    let ready = br_count(
        workspace,
        timeout_ms,
        runner,
        &["ready", "--json"],
        degraded,
    );
    let in_progress = br_count(
        workspace,
        timeout_ms,
        runner,
        &["list", "--status", "in_progress", "--json"],
        degraded,
    );
    let blocked = br_count(
        workspace,
        timeout_ms,
        runner,
        &["blocked", "--json"],
        degraded,
    );
    PerfLiveBeadActivity {
        active_agents: in_progress,
        ready_beads: ready,
        in_progress_beads: in_progress,
        blocked_beads: blocked,
    }
}

fn br_count<R: SwarmBriefCommandRunner>(
    workspace: &Path,
    timeout_ms: u64,
    runner: &R,
    args: &[&str],
    degraded: &mut Vec<PerfLiveDegradation>,
) -> u64 {
    match runner.run("br", args, workspace, timeout_ms) {
        Ok(output) => parse_json_collection_len(&output.stdout).unwrap_or_else(|message| {
            degraded.push(PerfLiveDegradation::warning(
                "perf_live_beads_source_degraded",
                "beadActivity",
                message,
                Some(format!("Run br {}.", args.join(" "))),
            ));
            0
        }),
        Err(error) => {
            degraded.push(command_degradation(
                "perf_live_beads_source_degraded",
                "beadActivity",
                error,
                &format!("Run br {}.", args.join(" ")),
            ));
            0
        }
    }
}

fn parse_json_collection_len(input: &str) -> Result<u64, String> {
    let value = serde_json::from_str::<Value>(input)
        .map_err(|error| format!("br JSON parse error: {error}"))?;
    json_collection_len(&value)
        .map(|count| count as u64)
        .ok_or_else(|| "br JSON did not contain a top-level collection.".to_owned())
}

fn json_collection_len(value: &Value) -> Option<usize> {
    value.as_array().map(Vec::len).or_else(|| {
        ["issues", "items", "data", "result"]
            .iter()
            .find_map(|key| value.get(*key).and_then(Value::as_array).map(Vec::len))
    })
}

fn host_pressure(degraded: &mut Vec<PerfLiveDegradation>) -> PerfLiveHostPressure {
    let mut pressure = PerfLiveHostPressure::default();
    pressure.memory_rss_mb = current_rss_mb();
    pressure.page_cache_mb = page_cache_mb();
    pressure.fsync_latency_p99_ms = Some(0);
    if pressure.memory_rss_mb.is_none()
        || pressure.page_cache_mb.is_none()
        || pressure.cpu_user_pct.is_none()
        || pressure.cpu_iowait_pct.is_none()
    {
        degraded.push(PerfLiveDegradation::warning(
            "perf_live_host_pressure_partial",
            "hostPressure",
            "Host-pressure probe is partially unavailable on this platform; unavailable fields are null.",
            Some("Wire platform-specific CPU and memory counters into perf live.".to_owned()),
        ));
    }
    pressure
}

#[cfg(target_os = "linux")]
fn current_rss_mb() -> Option<u64> {
    let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
    let rss_pages = statm.split_whitespace().nth(1)?.parse::<u64>().ok()?;
    Some(rss_pages.saturating_mul(4096) / (1024 * 1024))
}

#[cfg(not(target_os = "linux"))]
fn current_rss_mb() -> Option<u64> {
    None
}

#[cfg(target_os = "linux")]
fn page_cache_mb() -> Option<u64> {
    let meminfo = std::fs::read_to_string("/proc/meminfo").ok()?;
    meminfo.lines().find_map(|line| {
        let value = line.strip_prefix("Cached:")?;
        let kib = value.split_whitespace().next()?.parse::<u64>().ok()?;
        Some(kib / 1024)
    })
}

#[cfg(not(target_os = "linux"))]
fn page_cache_mb() -> Option<u64> {
    None
}

fn command_degradation(
    code: &'static str,
    source: &'static str,
    error: SwarmBriefCommandError,
    repair: &str,
) -> PerfLiveDegradation {
    let message = match error {
        SwarmBriefCommandError::Unavailable(_) => "Read-only source command is unavailable.",
        SwarmBriefCommandError::Failed { .. } => "Read-only source command failed.",
        SwarmBriefCommandError::TimedOut { .. } => "Read-only source command timed out.",
        SwarmBriefCommandError::InvalidUtf8(_) => "Read-only source command emitted invalid UTF-8.",
    };
    PerfLiveDegradation::warning(code, source, message, Some(repair.to_owned()))
}

fn numeric_field_any(value: &Value, keys: &[&str]) -> Option<u64> {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(value_to_u64))
        .or_else(|| {
            value.as_object().and_then(|object| {
                object.values().find_map(|nested| {
                    if nested.is_object() {
                        numeric_field_any(nested, keys)
                    } else {
                        None
                    }
                })
            })
        })
}

fn string_field_any<'a>(value: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter().find_map(|key| value.get(*key)?.as_str())
}

fn value_to_u64(value: &Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_i64().and_then(|number| number.try_into().ok()))
        .or_else(|| value.as_str()?.trim().parse::<u64>().ok())
}

fn usize_to_u64(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::core::swarm_brief::{SwarmBriefCommandOutput, SwarmBriefCommandRunner};

    #[derive(Default)]
    struct FakeRunner {
        rch: Option<&'static str>,
        ready: Option<&'static str>,
        in_progress: Option<&'static str>,
        blocked: Option<&'static str>,
    }

    impl SwarmBriefCommandRunner for FakeRunner {
        fn run(
            &self,
            program: &str,
            args: &[&str],
            _cwd: &Path,
            _timeout_ms: u64,
        ) -> Result<SwarmBriefCommandOutput, SwarmBriefCommandError> {
            let stdout = match (program, args) {
                ("rch", ["status", "--workers", "--jobs", "--json"]) => self.rch,
                ("br", ["ready", "--json"]) => self.ready,
                ("br", ["list", "--status", "in_progress", "--json"]) => self.in_progress,
                ("br", ["blocked", "--json"]) => self.blocked,
                _ => None,
            }
            .ok_or_else(|| SwarmBriefCommandError::Unavailable("missing fixture".to_owned()))?;
            Ok(SwarmBriefCommandOutput {
                stdout: stdout.to_owned(),
                stderr: String::new(),
            })
        }
    }

    #[test]
    fn parses_perf_live_duration_suffixes() -> Result<(), String> {
        assert_eq!(
            parse_perf_live_duration_ms("250ms").map_err(|e| e.message())?,
            250
        );
        assert_eq!(
            parse_perf_live_duration_ms("2s").map_err(|e| e.message())?,
            2_000
        );
        assert_eq!(
            parse_perf_live_duration_ms("3m").map_err(|e| e.message())?,
            180_000
        );
        assert_eq!(
            parse_perf_live_duration_ms(" 250MS ").map_err(|e| e.message())?,
            250
        );
        assert_eq!(
            parse_perf_live_duration_ms("2S").map_err(|e| e.message())?,
            2_000
        );
        assert_eq!(
            parse_perf_live_duration_ms("3M").map_err(|e| e.message())?,
            180_000
        );
        assert!(parse_perf_live_duration_ms("0s").is_err());
        Ok(())
    }

    #[test]
    fn infers_healthy_workers_from_status_variants() -> Result<(), String> {
        let rch = parse_rch_snapshot_json(
            r#"{
                "workers": [
                    {"status": "READY"},
                    {"health": " Healthy "},
                    {"state": "available"},
                    {"status": "degraded"}
                ]
            }"#,
        )?;
        assert_eq!(rch.workers_healthy, 3);
        Ok(())
    }

    #[test]
    fn parses_rch_and_bead_activity_sources() -> Result<(), String> {
        let rch = parse_rch_snapshot_json(
            r#"{"workersHealthy":" 5 ","slotsAvailable":32,"queueDepth":"2","headOfLineAgeMs":17}"#,
        )?;
        assert_eq!(rch.workers_healthy, 5);
        assert_eq!(rch.slots_available, Some(32));
        assert_eq!(rch.queue_depth, 2);
        assert_eq!(rch.head_of_line_age_ms, Some(17));

        let mut degraded = Vec::new();
        let runner = FakeRunner {
            ready: Some(r#"[{"id":"a"},{"id":"b"}]"#),
            in_progress: Some(r#"{"issues":[{"id":"c"}]}"#),
            blocked: Some(r#"{"data":[{"id":"d"},{"id":"e"},{"id":"f"}]}"#),
            ..FakeRunner::default()
        };
        let activity = bead_activity(Path::new("."), 100, &runner, &mut degraded);
        assert_eq!(activity.ready_beads, 2);
        assert_eq!(activity.in_progress_beads, 1);
        assert_eq!(activity.blocked_beads, 3);
        assert!(degraded.is_empty());
        Ok(())
    }

    #[test]
    fn source_failures_degrade_instead_of_blocking_snapshot() {
        let mut degraded = Vec::new();
        let runner = FakeRunner::default();
        let rch = rch_snapshot(Path::new("."), 1, &runner, &mut degraded);
        let activity = bead_activity(Path::new("."), 1, &runner, &mut degraded);
        assert_eq!(rch.queue_depth, 0);
        assert_eq!(activity.ready_beads, 0);
        assert!(
            degraded
                .iter()
                .any(|entry| entry.code == "perf_live_rch_source_degraded")
        );
        assert!(
            degraded
                .iter()
                .any(|entry| entry.code == "perf_live_beads_source_degraded")
        );
    }
}
