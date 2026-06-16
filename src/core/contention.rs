//! Deterministic aggregation/posture/ranking for the read-only
//! `ee.diag.contention.v1` diagnostic (ADR 0079, bd-d67os.11).
//!
//! [`build_contention_report`] is a pure function of [`ContentionInputs`]:
//! given the same metric snapshots it produces a byte-identical
//! [`ContentionDiagReport`]. It embeds no wall-clock and performs no I/O, so it
//! is fully unit-testable with injected fixtures. The live snapshot gathering
//! (calling `WriteOwner::status()`, `ReadPool::stats()`,
//! `singleflight_posture_report()`, …) and the `ee diag contention` CLI surface
//! land in bd-d67os.12; this module is the read-only collector core.
//!
//! "Collector" here means *aggregation*: it folds the scattered, already-cheap
//! per-subsystem snapshots into one posture report with a ranked
//! `topContention` list. It never mutates any subsystem.

use crate::core::write_owner::WriteOwnerStatus;
use crate::db::read_pool::PoolStats;
use crate::models::contention::{
    CONTENTION_DIAG_SCHEMA_V1, ContentionDiagReport, ContentionFinding, ContentionPosture,
    ContentionSourceGap, GroupCommitContention, IndexIntakeContention, L2CacheContention,
    ReadPoolContention, SingleflightContention, WriteLockContention,
};
use crate::models::singleflight::SingleFlightPostureReport;

// --- Posture thresholds (documented in ADR 0079; advisory v1 heuristics) ---

/// Any pending write request is at least mild pressure.
const WRITE_QUEUE_WARM: usize = 1;
/// Queue depth at/above this is hot.
const WRITE_QUEUE_HOT: usize = 8;
/// Queue depth at/above this is contended.
const WRITE_QUEUE_CONTENDED: usize = 32;
/// Max queue wait (ms) at/above this is hot.
const WRITE_WAIT_HOT_MS: u64 = 250;
/// Max queue wait (ms) at/above this is contended.
const WRITE_WAIT_CONTENDED_MS: u64 = 2_000;
/// Persisted `lock_wait_ms` p99 at/above this is contended.
const LOCK_WAIT_P99_CONTENDED_MS: u64 = 1_000;

/// Read-pool acquire-wait p99 (ns) at/above this is warm (1 ms).
const ACQUIRE_WAIT_P99_WARM_NS: u128 = 1_000_000;
/// Read-pool acquire-wait p99 (ns) at/above this is hot (50 ms).
const ACQUIRE_WAIT_P99_HOT_NS: u128 = 50_000_000;
/// Read-pool acquire-wait p99 (ns) at/above this is contended (1 s).
const ACQUIRE_WAIT_P99_CONTENDED_NS: u128 = 1_000_000_000;

/// Optional future-feature input: group-commit write-intake counters
/// (Track B, bd-d67os.1..4). Absent until that lands.
#[derive(Clone, Debug, Default)]
pub struct GroupCommitInput {
    pub enabled: bool,
    pub batches: u64,
    pub writes_coalesced: u64,
    pub fsync_saved: u64,
}

/// Optional future-feature input: incremental-index intake counters
/// (Track C, bd-d67os.5..8). Absent until that lands.
#[derive(Clone, Debug, Default)]
pub struct IndexIntakeInput {
    pub intake_mode: String,
    pub rebuilds: u64,
    pub swap_stalls: u64,
    pub avg_swap_ms: f64,
}

/// Optional future-feature input: L2 pack-cache counters. Absent until an
/// aggregate `pack_l2` metrics accessor exists.
#[derive(Clone, Debug, Default)]
pub struct L2CacheInput {
    pub hits: u64,
    pub misses: u64,
    pub evictions: u64,
    pub inserts: u64,
}

/// Injectable snapshot inputs for [`build_contention_report`]. In production
/// these are gathered live by the future `ee diag contention` handler; in tests
/// they are constructed directly for determinism.
#[derive(Clone, Debug, Default)]
pub struct ContentionInputs {
    /// Write-owner status (daemon write path). `None` when not running.
    pub write_owner: Option<WriteOwnerStatus>,
    /// p50 of persisted per-write `lock_wait_ms` from the session-budget ledger.
    pub lock_wait_ms_p50: Option<u64>,
    /// p99 of persisted per-write `lock_wait_ms` from the session-budget ledger.
    pub lock_wait_ms_p99: Option<u64>,
    /// Read-pool stats. `None` when no pool is initialized.
    pub read_pool: Option<PoolStats>,
    /// Single-flight posture (process-global). `None` only if unavailable.
    pub singleflight: Option<SingleFlightPostureReport>,
    /// Group-commit counters (Track B). `None` until that feature lands.
    pub group_commit: Option<GroupCommitInput>,
    /// Incremental-index counters (Track C). `None` until that feature lands.
    pub index_intake: Option<IndexIntakeInput>,
    /// L2 pack-cache counters. `None` until an aggregate accessor exists.
    pub l2_cache: Option<L2CacheInput>,
}

fn classify_write_lock(status: &WriteOwnerStatus, p99_ms: Option<u64>) -> ContentionPosture {
    let mut posture = ContentionPosture::Ok;
    if status.queue_depth >= WRITE_QUEUE_WARM || status.max_wait_ms > 0 {
        posture = ContentionPosture::Warm;
    }
    if status.queue_depth >= WRITE_QUEUE_HOT || status.max_wait_ms >= WRITE_WAIT_HOT_MS {
        posture = ContentionPosture::Hot;
    }
    if status.queue_depth >= WRITE_QUEUE_CONTENDED
        || status.max_wait_ms >= WRITE_WAIT_CONTENDED_MS
        || p99_ms.is_some_and(|v| v >= LOCK_WAIT_P99_CONTENDED_MS)
    {
        posture = ContentionPosture::Contended;
    }
    posture
}

/// Returns `(posture, underized)`.
fn classify_read_pool(stats: &PoolStats) -> (ContentionPosture, bool) {
    let p99 = stats.acquire_wait.p99_ns;
    let saturated = stats.max_size > 0 && stats.active >= stats.max_size;
    let underized =
        stats.size_was_zero || stats.ad_hoc_bypass_count > 0 || p99 >= ACQUIRE_WAIT_P99_WARM_NS;

    let mut posture = ContentionPosture::Ok;
    if saturated
        || stats.expired_pins > 0
        || stats.release_failures > 0
        || p99 >= ACQUIRE_WAIT_P99_WARM_NS
    {
        posture = ContentionPosture::Warm;
    }
    if stats.ad_hoc_bypass_count > 0 || p99 >= ACQUIRE_WAIT_P99_HOT_NS {
        posture = ContentionPosture::Hot;
    }
    if stats.size_was_zero || stats.drops > 0 || p99 >= ACQUIRE_WAIT_P99_CONTENDED_NS {
        posture = ContentionPosture::Contended;
    }
    (posture, underized)
}

fn classify_singleflight(report: &SingleFlightPostureReport) -> ContentionPosture {
    let mut posture = ContentionPosture::Ok;
    if report.active_leader_count > 0 || report.follower_wait_count > 0 {
        posture = ContentionPosture::Warm;
    }
    if report.follower_timeout_count > 0 || report.leader_failure_count > 0 {
        posture = ContentionPosture::Hot;
    }
    if report.status == "state_poisoned" {
        posture = ContentionPosture::Contended;
    }
    posture
}

fn finding(
    source: &str,
    severity: ContentionPosture,
    reason_code: &str,
    detail: String,
    suggested: &[&str],
) -> ContentionFinding {
    ContentionFinding {
        source: source.to_owned(),
        severity,
        reason_code: reason_code.to_owned(),
        detail,
        suggested_commands: suggested.iter().map(|s| (*s).to_owned()).collect(),
    }
}

/// Build the deterministic contention report from the supplied snapshots.
#[must_use]
pub fn build_contention_report(inputs: &ContentionInputs) -> ContentionDiagReport {
    let mut gaps: Vec<ContentionSourceGap> = Vec::new();
    let mut findings: Vec<ContentionFinding> = Vec::new();
    let mut overall = ContentionPosture::Ok;

    // --- write lock ---
    let write_lock = if let Some(status) = inputs.write_owner.as_ref() {
        let posture = classify_write_lock(status, inputs.lock_wait_ms_p99);
        overall = overall.worst(posture);
        if posture >= ContentionPosture::Warm {
            let reason = if status.queue_depth >= WRITE_QUEUE_HOT {
                "write_lock_queue_backlog"
            } else if status.max_wait_ms >= WRITE_WAIT_HOT_MS
                || inputs
                    .lock_wait_ms_p99
                    .is_some_and(|v| v >= LOCK_WAIT_P99_CONTENDED_MS)
            {
                "write_lock_high_wait"
            } else {
                "write_lock_pressure"
            };
            findings.push(finding(
                "write_lock",
                posture,
                reason,
                format!(
                    "write-owner queue depth {} (max wait {} ms); concurrent writers are serializing behind the single write lock",
                    status.queue_depth, status.max_wait_ms
                ),
                &[
                    "enable group-commit write batching (bd-d67os.1)",
                    "route heavy writers through the daemon write owner: ee daemon --foreground",
                    "reduce the number of concurrent durable writers",
                ],
            ));
        }
        WriteLockContention {
            running: status.running,
            queue_depth: status.queue_depth,
            total_processed: status.total_processed,
            avg_wait_ms: status.avg_wait_ms,
            max_wait_ms: status.max_wait_ms,
            lock_wait_ms_p50: inputs.lock_wait_ms_p50,
            lock_wait_ms_p99: inputs.lock_wait_ms_p99,
            posture,
        }
    } else {
        gaps.push(ContentionSourceGap {
            source: "write_lock".to_owned(),
            code: "write_owner_unavailable".to_owned(),
        });
        WriteLockContention {
            lock_wait_ms_p50: inputs.lock_wait_ms_p50,
            lock_wait_ms_p99: inputs.lock_wait_ms_p99,
            ..WriteLockContention::default()
        }
    };

    // --- read pool ---
    let read_pool = if let Some(stats) = inputs.read_pool.as_ref() {
        let (posture, underized) = classify_read_pool(stats);
        overall = overall.worst(posture);
        if posture >= ContentionPosture::Warm {
            let reason = if stats.size_was_zero {
                "read_pool_disabled"
            } else if stats.ad_hoc_bypass_count > 0 {
                "read_pool_ad_hoc_bypass"
            } else if stats.acquire_wait.p99_ns >= ACQUIRE_WAIT_P99_HOT_NS {
                "read_pool_high_acquire_wait"
            } else if stats.max_size > 0 && stats.active >= stats.max_size {
                "read_pool_saturated"
            } else {
                "read_pool_pressure"
            };
            findings.push(finding(
                "read_pool",
                posture,
                reason,
                format!(
                    "read pool {}/{} active, {} ad-hoc bypasses, acquire-wait p99 {} ns; readers are waiting or bypassing the pool",
                    stats.active, stats.max_size, stats.ad_hoc_bypass_count, stats.acquire_wait.p99_ns
                ),
                &[
                    "raise EE_READ_POOL_SIZE for this workload",
                    "investigate long-held snapshot pins (EE_READ_POOL_MAX_PIN_SECONDS)",
                    "see bd-d67os.15 for read-pool FIFO fairness under storm",
                ],
            ));
        }
        ReadPoolContention {
            max_size: stats.max_size,
            active: stats.active,
            idle: stats.idle,
            active_pins: stats.active_pins,
            expired_pins: stats.expired_pins,
            max_seen: stats.max_seen,
            drops: stats.drops,
            release_failures: stats.release_failures,
            ad_hoc_bypass_count: stats.ad_hoc_bypass_count,
            acquire_wait_samples: stats.acquire_wait.samples,
            acquire_wait_p50_ns: stats.acquire_wait.p50_ns,
            acquire_wait_p99_ns: stats.acquire_wait.p99_ns,
            size_was_zero: stats.size_was_zero,
            underized,
            posture,
        }
    } else {
        gaps.push(ContentionSourceGap {
            source: "read_pool".to_owned(),
            code: "read_pool_unavailable".to_owned(),
        });
        ReadPoolContention::default()
    };

    // --- single-flight ---
    let singleflight = if let Some(report) = inputs.singleflight.as_ref() {
        let posture = classify_singleflight(report);
        overall = overall.worst(posture);
        let denom = report
            .reused_result_count
            .saturating_add(report.leader_start_count);
        let coalesce_efficiency = if denom > 0 {
            Some(report.reused_result_count as f64 / denom as f64)
        } else {
            None
        };
        if posture >= ContentionPosture::Warm {
            let (reason, suggested): (&str, &[&str]) = if report.status == "state_poisoned" {
                (
                    "singleflight_state_poisoned",
                    &["restart the process to clear poisoned single-flight state"],
                )
            } else if report.follower_timeout_count > 0 {
                (
                    "singleflight_follower_timeouts",
                    &[
                        "lower duplicate read pressure or raise the follower timeout",
                        "inspect degraded entries before rerunning duplicate work",
                    ],
                )
            } else if report.leader_failure_count > 0 {
                (
                    "singleflight_leader_failures",
                    &["inspect degraded entries; leaders failed but followers were woken"],
                )
            } else {
                (
                    "singleflight_active_coalescing",
                    &["informational: read coalescing is absorbing duplicate load"],
                )
            };
            findings.push(finding(
                "singleflight",
                posture,
                reason,
                format!(
                    "single-flight: {} active leaders, {} followers waiting, {} timeouts, {} leader failures",
                    report.active_leader_count,
                    report.follower_wait_count,
                    report.follower_timeout_count,
                    report.leader_failure_count
                ),
                suggested,
            ));
        }
        SingleflightContention {
            status: report.status.clone(),
            configured_surface_count: report.configured_surface_count,
            active_leader_count: report.active_leader_count,
            leader_start_count: report.leader_start_count,
            follower_wait_count: report.follower_wait_count,
            follower_timeout_count: report.follower_timeout_count,
            leader_failure_count: report.leader_failure_count,
            reused_result_count: report.reused_result_count,
            coalesce_efficiency,
            posture,
        }
    } else {
        gaps.push(ContentionSourceGap {
            source: "singleflight".to_owned(),
            code: "singleflight_unavailable".to_owned(),
        });
        SingleflightContention::default()
    };

    // --- optional future-feature sources (omit-safe; no gap when absent) ---
    let group_commit = inputs.group_commit.as_ref().map(|gc| {
        let avg_batch_size = if gc.batches > 0 {
            gc.writes_coalesced as f64 / gc.batches as f64
        } else {
            0.0
        };
        let posture = if !gc.enabled {
            ContentionPosture::Ok
        } else if gc.fsync_saved > 0 {
            ContentionPosture::Warm
        } else {
            ContentionPosture::Ok
        };
        overall = overall.worst(posture);
        GroupCommitContention {
            enabled: gc.enabled,
            batches: gc.batches,
            writes_coalesced: gc.writes_coalesced,
            fsync_saved: gc.fsync_saved,
            avg_batch_size,
            posture,
        }
    });

    let index_intake = inputs.index_intake.as_ref().map(|ix| {
        let posture = if ix.swap_stalls > 0 {
            ContentionPosture::Hot
        } else if ix.intake_mode == "full_rebuild" && ix.rebuilds > 0 {
            ContentionPosture::Warm
        } else {
            ContentionPosture::Ok
        };
        overall = overall.worst(posture);
        if posture >= ContentionPosture::Warm {
            findings.push(finding(
                "index_intake",
                posture,
                if ix.swap_stalls > 0 {
                    "index_swap_stalls"
                } else {
                    "index_full_rebuild_amplification"
                },
                format!(
                    "index intake mode '{}': {} rebuilds, {} swap stalls (avg swap {:.1} ms)",
                    ix.intake_mode, ix.rebuilds, ix.swap_stalls, ix.avg_swap_ms
                ),
                &["adopt incremental index intake (bd-d67os.6)"],
            ));
        }
        IndexIntakeContention {
            intake_mode: ix.intake_mode.clone(),
            rebuilds: ix.rebuilds,
            swap_stalls: ix.swap_stalls,
            avg_swap_ms: ix.avg_swap_ms,
            posture,
        }
    });

    let l2_cache = inputs.l2_cache.as_ref().map(|l2| {
        let lookups = l2.hits.saturating_add(l2.misses);
        let hit_rate = if lookups > 0 {
            Some(l2.hits as f64 / lookups as f64)
        } else {
            None
        };
        let thrash_ratio = if l2.inserts > 0 {
            Some(l2.evictions as f64 / l2.inserts as f64)
        } else {
            None
        };
        let posture = if thrash_ratio.is_some_and(|r| r >= 0.5) {
            ContentionPosture::Warm
        } else {
            ContentionPosture::Ok
        };
        overall = overall.worst(posture);
        if posture >= ContentionPosture::Warm {
            findings.push(finding(
                "l2_cache",
                posture,
                "l2_cache_thrash",
                format!(
                    "L2 pack cache: {} hits / {} misses, {} evictions across {} inserts",
                    l2.hits, l2.misses, l2.evictions, l2.inserts
                ),
                &["raise EE_L2_PACK_CACHE_BYTES or narrow the pack workload"],
            ));
        }
        L2CacheContention {
            hits: l2.hits,
            misses: l2.misses,
            evictions: l2.evictions,
            hit_rate,
            thrash_ratio,
            posture,
        }
    });

    // Deterministic ordering: severity desc, then source asc.
    findings.sort_by(|a, b| {
        b.severity
            .cmp(&a.severity)
            .then_with(|| a.source.cmp(&b.source))
    });
    gaps.sort_by(|a, b| a.source.cmp(&b.source));

    ContentionDiagReport {
        schema_tag: CONTENTION_DIAG_SCHEMA_V1,
        overall_posture: overall,
        write_lock,
        read_pool,
        singleflight,
        group_commit,
        index_intake,
        l2_cache,
        top_contention: findings,
        unavailable_sources: gaps,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::read_pool::AcquireWaitStats;

    fn quiet_singleflight() -> SingleFlightPostureReport {
        SingleFlightPostureReport {
            schema: "ee.singleflight.posture.v1".to_owned(),
            status: "idle".to_owned(),
            configured_surface_count: 1,
            active_leader_count: 0,
            leader_start_count: 0,
            follower_wait_count: 0,
            follower_timeout_count: 0,
            leader_failure_count: 0,
            reused_result_count: 0,
            surfaces: Vec::new(),
        }
    }

    fn quiet_write_owner() -> WriteOwnerStatus {
        WriteOwnerStatus {
            running: true,
            queue_depth: 0,
            total_processed: 100,
            avg_wait_ms: 0.0,
            max_wait_ms: 0,
            ..WriteOwnerStatus::default()
        }
    }

    fn pool_stats(active: usize, max_size: usize, bypass: u64, p99_ns: u128) -> PoolStats {
        PoolStats {
            active,
            max_size,
            ad_hoc_bypass_count: bypass,
            acquire_wait: AcquireWaitStats {
                samples: 4,
                p50_ns: 0,
                p99_ns,
            },
            ..PoolStats::default()
        }
    }

    #[test]
    fn all_quiet_is_ok_with_no_findings() {
        let inputs = ContentionInputs {
            write_owner: Some(quiet_write_owner()),
            read_pool: Some(pool_stats(1, 4, 0, 0)),
            singleflight: Some(quiet_singleflight()),
            ..ContentionInputs::default()
        };
        let report = build_contention_report(&inputs);
        assert_eq!(report.overall_posture, ContentionPosture::Ok);
        assert!(report.top_contention.is_empty());
        assert!(report.unavailable_sources.is_empty());
        assert_eq!(report.schema_tag, CONTENTION_DIAG_SCHEMA_V1);
        assert!(report.group_commit.is_none());
    }

    #[test]
    fn saturated_read_pool_bypass_is_hot_with_finding() {
        let inputs = ContentionInputs {
            write_owner: Some(quiet_write_owner()),
            read_pool: Some(pool_stats(4, 4, 7, 60_000_000)),
            singleflight: Some(quiet_singleflight()),
            ..ContentionInputs::default()
        };
        let report = build_contention_report(&inputs);
        assert_eq!(report.read_pool.posture, ContentionPosture::Hot);
        assert!(report.read_pool.underized);
        assert_eq!(report.overall_posture, ContentionPosture::Hot);
        let f = report
            .top_contention
            .iter()
            .find(|f| f.source == "read_pool")
            .expect("read_pool finding");
        assert_eq!(f.reason_code, "read_pool_ad_hoc_bypass");
        assert!(!f.suggested_commands.is_empty());
    }

    #[test]
    fn write_backlog_escalates_to_contended() {
        let mut owner = quiet_write_owner();
        owner.queue_depth = 40;
        owner.max_wait_ms = 5_000;
        let inputs = ContentionInputs {
            write_owner: Some(owner),
            read_pool: Some(pool_stats(0, 4, 0, 0)),
            singleflight: Some(quiet_singleflight()),
            ..ContentionInputs::default()
        };
        let report = build_contention_report(&inputs);
        assert_eq!(report.write_lock.posture, ContentionPosture::Contended);
        assert_eq!(report.overall_posture, ContentionPosture::Contended);
        assert_eq!(
            report.top_contention.first().map(|f| f.source.as_str()),
            Some("write_lock")
        );
    }

    #[test]
    fn missing_core_sources_record_gaps_and_defaults() {
        let inputs = ContentionInputs::default();
        let report = build_contention_report(&inputs);
        assert_eq!(report.overall_posture, ContentionPosture::Ok);
        let codes: Vec<&str> = report
            .unavailable_sources
            .iter()
            .map(|g| g.source.as_str())
            .collect();
        // Sorted by source asc.
        assert_eq!(codes, vec!["read_pool", "singleflight", "write_lock"]);
    }

    #[test]
    fn findings_sorted_by_severity_then_source() {
        // read_pool Hot, write_lock Warm -> read_pool first.
        let mut owner = quiet_write_owner();
        owner.queue_depth = 2; // warm
        let inputs = ContentionInputs {
            write_owner: Some(owner),
            read_pool: Some(pool_stats(4, 4, 1, 0)), // hot (bypass>0)
            singleflight: Some(quiet_singleflight()),
            ..ContentionInputs::default()
        };
        let report = build_contention_report(&inputs);
        let sources: Vec<&str> = report
            .top_contention
            .iter()
            .map(|f| f.source.as_str())
            .collect();
        assert_eq!(sources, vec!["read_pool", "write_lock"]);
    }

    #[test]
    fn report_is_deterministic() {
        let inputs = ContentionInputs {
            write_owner: Some(quiet_write_owner()),
            read_pool: Some(pool_stats(2, 4, 3, 70_000_000)),
            singleflight: Some(quiet_singleflight()),
            ..ContentionInputs::default()
        };
        let a = serde_json::to_string(&build_contention_report(&inputs)).expect("a");
        let b = serde_json::to_string(&build_contention_report(&inputs)).expect("b");
        assert_eq!(a, b);
    }

    #[test]
    fn serialized_report_uses_camel_case_fields() {
        let inputs = ContentionInputs {
            write_owner: Some(quiet_write_owner()),
            read_pool: Some(pool_stats(1, 4, 0, 0)),
            singleflight: Some(quiet_singleflight()),
            ..ContentionInputs::default()
        };
        let value = serde_json::to_value(build_contention_report(&inputs)).expect("value");
        assert_eq!(value["schemaTag"], CONTENTION_DIAG_SCHEMA_V1);
        assert!(value.get("overallPosture").is_some());
        assert!(value["readPool"].get("adHocBypassCount").is_some());
        assert!(value["readPool"].get("acquireWaitP99Ns").is_some());
        assert!(value.get("topContention").is_some());
        assert!(value.get("unavailableSources").is_some());
        // omit-safe optionals absent
        assert!(value.get("groupCommit").is_none());
        assert!(value.get("indexIntake").is_none());
        assert!(value.get("l2Cache").is_none());
    }
}
