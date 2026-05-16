//! Pure decision helper for graph algorithm witness retention
//! (bd-bife.25).
//!
//! `graph_algorithm_witnesses` accumulates one row per algorithm
//! invocation. On a busy workspace doing 100 daily `ee context`
//! calls that's ~100 rows/day from PPR alone, ~36k rows/year. This
//! module provides the pure classification used by the upcoming
//! `ee maintenance graph-witnesses-prune` command and any
//! background steward job:
//!
//! - **Default TTL** (configurable via `graph.witnesses.retention_days`).
//! - **Per-algorithm overrides** for witnesses that are more
//!   valuable to keep around (e.g. the cache_results algorithm
//!   produces witnesses that survive longer than ad-hoc PPR runs).
//! - **Active-snapshot guard** — witnesses tied to an active
//!   snapshot are never pruned, regardless of age, until that
//!   snapshot is archived.
//! - **Conservative parse-failure handling** — a witness whose
//!   `recorded_at` cannot be parsed as RFC 3339 is kept (not
//!   silently pruned) and reported via the
//!   `keep_unparseable_recorded_at_count` summary field. The
//!   maintenance command can surface that as a degraded code so an
//!   operator notices schema drift instead of losing rows to it.
//!
//! The function is intentionally pure: it takes a slice of
//! `(StoredGraphAlgorithmWitness, snapshot_active: bool)` plus a
//! policy and a clock, and returns a structured report. Wiring
//! against the live DB plus the CLI subcommand is tracked separately
//! so this slice can land clean against an uncontested file
//! boundary.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::db::StoredGraphAlgorithmWitness;

/// Default retention window in days when neither the policy nor a
/// per-algorithm override specifies one.
pub const DEFAULT_WITNESS_RETENTION_DAYS: u32 = 30;

/// Stable schema tag for the prune-decision report payload that the
/// maintenance command will eventually serialize.
pub const WITNESS_PRUNE_REPORT_SCHEMA_V1: &str = "ee.graph.witness_prune_report.v1";

/// Effective retention policy for a workspace's graph algorithm
/// witness ledger. The default and any per-algorithm overrides come
/// from `[graph.witnesses]` config keys; this type is what the
/// classification function actually consumes so the wiring layer
/// can resolve config in one place.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WitnessRetentionPolicy {
    pub default_ttl_days: u32,
    pub per_algorithm_ttl_days: BTreeMap<String, u32>,
}

impl WitnessRetentionPolicy {
    /// Construct a policy with the documented default TTL and no
    /// per-algorithm overrides.
    #[must_use]
    pub fn defaults() -> Self {
        Self {
            default_ttl_days: DEFAULT_WITNESS_RETENTION_DAYS,
            per_algorithm_ttl_days: BTreeMap::new(),
        }
    }

    /// Effective TTL for the named algorithm, falling back to the
    /// default when no per-algorithm override is set.
    #[must_use]
    pub fn ttl_days_for(&self, algorithm: &str) -> u32 {
        self.per_algorithm_ttl_days
            .get(algorithm)
            .copied()
            .unwrap_or(self.default_ttl_days)
    }
}

impl Default for WitnessRetentionPolicy {
    fn default() -> Self {
        Self::defaults()
    }
}

/// One classification verdict for a single witness row. The
/// classification carries the original identifying fields so the
/// maintenance command can render a stable per-row table without a
/// second join against the witness table.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WitnessClassification {
    pub workspace_id: String,
    pub snapshot_id: String,
    pub algorithm: String,
    pub recorded_at: String,
    pub action: WitnessAction,
}

/// Action the maintenance command should take for a single row.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WitnessAction {
    /// Row is older than the effective TTL and is not tied to an
    /// active snapshot. Safe to delete.
    Prune { age_days: u32, ttl_days: u32 },
    /// Row should be kept; `reason` explains why.
    Keep { reason: WitnessKeepReason },
}

/// Why a witness row is being kept rather than pruned.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum WitnessKeepReason {
    /// The witness is tied to a snapshot that is still active. We
    /// preserve historical evidence as long as the snapshot is
    /// queryable so `ee insights` can show "yesterday's PageRank
    /// scores" without losing rows underneath the query.
    ActiveSnapshot,
    /// The witness is younger than its effective TTL.
    WithinTtl { age_days: u32, ttl_days: u32 },
    /// The witness's `recorded_at` could not be parsed as RFC 3339.
    /// We keep the row (conservative) and surface the count via the
    /// summary so an operator can investigate schema drift.
    UnparseableRecordedAt,
}

/// Aggregate summary of a single classification pass.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WitnessPruneSummary {
    pub total_count: usize,
    pub prune_count: usize,
    pub keep_active_snapshot_count: usize,
    pub keep_within_ttl_count: usize,
    pub keep_unparseable_recorded_at_count: usize,
    /// Age of the oldest row that would be pruned, in days. `None`
    /// when nothing is being pruned.
    pub oldest_pruned_age_days: Option<u32>,
    /// Age of the oldest row being kept, in days. `None` when no
    /// rows are kept.
    pub oldest_kept_age_days: Option<u32>,
}

/// Top-level result of `classify_witnesses_for_pruning`.
///
/// `classifications` is sorted by `(workspace_id, snapshot_id,
/// algorithm, recorded_at)` so the rendered report is byte-stable
/// across runs (J7 determinism).
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WitnessPruneReport {
    pub schema: &'static str,
    pub now: String,
    pub policy: WitnessRetentionPolicy,
    pub classifications: Vec<WitnessClassification>,
    pub summary: WitnessPruneSummary,
}

/// Classify each `(witness, snapshot_active)` pair against the given
/// retention `policy` and return a deterministic report describing
/// which rows the maintenance command should prune and why every
/// other row was kept.
///
/// `now` is taken as a parameter (rather than reading
/// `Utc::now()`) so callers can drive deterministic tests; the
/// production wiring will pass `Utc::now()`.
#[must_use]
pub fn classify_witnesses_for_pruning(
    witnesses: &[(StoredGraphAlgorithmWitness, bool)],
    policy: &WitnessRetentionPolicy,
    now: DateTime<Utc>,
) -> WitnessPruneReport {
    let mut sorted: Vec<&(StoredGraphAlgorithmWitness, bool)> = witnesses.iter().collect();
    sorted.sort_by(|a, b| {
        let lhs = (
            &a.0.workspace_id,
            &a.0.snapshot_id,
            &a.0.algorithm,
            &a.0.recorded_at,
        );
        let rhs = (
            &b.0.workspace_id,
            &b.0.snapshot_id,
            &b.0.algorithm,
            &b.0.recorded_at,
        );
        lhs.cmp(&rhs)
    });

    let mut classifications: Vec<WitnessClassification> = Vec::with_capacity(sorted.len());
    let mut summary = WitnessPruneSummary {
        total_count: sorted.len(),
        ..WitnessPruneSummary::default()
    };

    for (witness, snapshot_active) in sorted {
        let recorded_at = match DateTime::parse_from_rfc3339(&witness.recorded_at) {
            Ok(parsed) => parsed.with_timezone(&Utc),
            Err(_) => {
                summary.keep_unparseable_recorded_at_count += 1;
                classifications.push(WitnessClassification {
                    workspace_id: witness.workspace_id.clone(),
                    snapshot_id: witness.snapshot_id.clone(),
                    algorithm: witness.algorithm.clone(),
                    recorded_at: witness.recorded_at.clone(),
                    action: WitnessAction::Keep {
                        reason: WitnessKeepReason::UnparseableRecordedAt,
                    },
                });
                continue;
            }
        };

        let age_days = days_between_floor(recorded_at, now);
        let ttl_days = policy.ttl_days_for(&witness.algorithm);

        // Active-snapshot guard wins regardless of age. We surface
        // active rows as Keep::ActiveSnapshot rather than collapsing
        // them into Keep::WithinTtl so the maintenance report can
        // tell an operator that pruning won't free those rows until
        // the snapshot itself is archived.
        if *snapshot_active {
            summary.keep_active_snapshot_count += 1;
            update_oldest(&mut summary.oldest_kept_age_days, age_days);
            classifications.push(WitnessClassification {
                workspace_id: witness.workspace_id.clone(),
                snapshot_id: witness.snapshot_id.clone(),
                algorithm: witness.algorithm.clone(),
                recorded_at: witness.recorded_at.clone(),
                action: WitnessAction::Keep {
                    reason: WitnessKeepReason::ActiveSnapshot,
                },
            });
            continue;
        }

        if age_days >= ttl_days {
            summary.prune_count += 1;
            update_oldest(&mut summary.oldest_pruned_age_days, age_days);
            classifications.push(WitnessClassification {
                workspace_id: witness.workspace_id.clone(),
                snapshot_id: witness.snapshot_id.clone(),
                algorithm: witness.algorithm.clone(),
                recorded_at: witness.recorded_at.clone(),
                action: WitnessAction::Prune { age_days, ttl_days },
            });
        } else {
            summary.keep_within_ttl_count += 1;
            update_oldest(&mut summary.oldest_kept_age_days, age_days);
            classifications.push(WitnessClassification {
                workspace_id: witness.workspace_id.clone(),
                snapshot_id: witness.snapshot_id.clone(),
                algorithm: witness.algorithm.clone(),
                recorded_at: witness.recorded_at.clone(),
                action: WitnessAction::Keep {
                    reason: WitnessKeepReason::WithinTtl { age_days, ttl_days },
                },
            });
        }
    }

    WitnessPruneReport {
        schema: WITNESS_PRUNE_REPORT_SCHEMA_V1,
        now: now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        policy: policy.clone(),
        classifications,
        summary,
    }
}

fn days_between_floor(earlier: DateTime<Utc>, later: DateTime<Utc>) -> u32 {
    if later <= earlier {
        return 0;
    }
    let delta = later.signed_duration_since(earlier);
    let days = delta.num_days();
    if days < 0 {
        0
    } else {
        u32::try_from(days).unwrap_or(u32::MAX)
    }
}

fn update_oldest(slot: &mut Option<u32>, age_days: u32) {
    *slot = Some(slot.map_or(age_days, |current| current.max(age_days)));
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn witness(
        workspace_id: &str,
        snapshot_id: &str,
        algorithm: &str,
        recorded_at: &str,
    ) -> StoredGraphAlgorithmWitness {
        StoredGraphAlgorithmWitness {
            workspace_id: workspace_id.to_owned(),
            snapshot_id: snapshot_id.to_owned(),
            algorithm: algorithm.to_owned(),
            params_json: "{}".to_owned(),
            witness_json: "{}".to_owned(),
            recorded_at: recorded_at.to_owned(),
        }
    }

    fn now_at_2026_05_15() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 5, 15, 12, 0, 0).unwrap()
    }

    /// Witness tied to an active snapshot is kept regardless of age.
    /// This is the load-bearing guard: pruning must never remove
    /// rows that `ee insights` could still query through the
    /// snapshot.
    #[test]
    fn witness_tied_to_active_snapshot_is_kept_regardless_of_age() {
        let now = now_at_2026_05_15();
        let policy = WitnessRetentionPolicy::defaults();
        // Recorded a year ago, way past TTL.
        let row = witness("ws1", "snap_active", "ppr", "2025-05-15T12:00:00Z");
        let report = classify_witnesses_for_pruning(&[(row, true)], &policy, now);

        assert_eq!(report.summary.total_count, 1);
        assert_eq!(report.summary.prune_count, 0);
        assert_eq!(report.summary.keep_active_snapshot_count, 1);
        assert!(matches!(
            report.classifications[0].action,
            WitnessAction::Keep {
                reason: WitnessKeepReason::ActiveSnapshot
            }
        ));
    }

    /// Witness past its effective TTL on an inactive snapshot is
    /// pruned. This is the headline behavior the maintenance
    /// command exists to perform.
    #[test]
    fn expired_witness_on_inactive_snapshot_is_pruned() {
        let now = now_at_2026_05_15();
        let policy = WitnessRetentionPolicy::defaults();
        // 31 days old, TTL is 30 days.
        let row = witness("ws1", "snap_old", "ppr", "2026-04-14T12:00:00Z");
        let report = classify_witnesses_for_pruning(&[(row, false)], &policy, now);

        assert_eq!(report.summary.total_count, 1);
        assert_eq!(report.summary.prune_count, 1);
        assert_eq!(report.summary.oldest_pruned_age_days, Some(31));
        assert!(matches!(
            report.classifications[0].action,
            WitnessAction::Prune {
                age_days: 31,
                ttl_days: 30
            }
        ));
    }

    /// Witness within the default TTL on an inactive snapshot is
    /// kept; the report carries the remaining-life math so a
    /// reviewer can see how close it was to expiry.
    #[test]
    fn fresh_witness_on_inactive_snapshot_is_kept_within_ttl() {
        let now = now_at_2026_05_15();
        let policy = WitnessRetentionPolicy::defaults();
        // 5 days old.
        let row = witness("ws1", "snap_recent", "ppr", "2026-05-10T12:00:00Z");
        let report = classify_witnesses_for_pruning(&[(row, false)], &policy, now);

        assert_eq!(report.summary.total_count, 1);
        assert_eq!(report.summary.prune_count, 0);
        assert_eq!(report.summary.keep_within_ttl_count, 1);
        assert_eq!(report.summary.oldest_kept_age_days, Some(5));
        assert!(matches!(
            report.classifications[0].action,
            WitnessAction::Keep {
                reason: WitnessKeepReason::WithinTtl {
                    age_days: 5,
                    ttl_days: 30
                }
            }
        ));
    }

    /// Per-algorithm override longer than the default keeps a row
    /// that the default would have pruned. This is the "cache
    /// results live longer than ad-hoc PPR" case from the bead body.
    #[test]
    fn per_algorithm_override_longer_than_default_keeps_row() {
        let now = now_at_2026_05_15();
        let mut policy = WitnessRetentionPolicy::defaults();
        policy
            .per_algorithm_ttl_days
            .insert("cache_results".to_owned(), 90);

        // 60 days old: past default 30 but within cache_results 90.
        let row = witness("ws1", "snap_old", "cache_results", "2026-03-16T12:00:00Z");
        let report = classify_witnesses_for_pruning(&[(row, false)], &policy, now);

        assert_eq!(report.summary.prune_count, 0);
        assert_eq!(report.summary.keep_within_ttl_count, 1);
        let WitnessAction::Keep {
            reason: WitnessKeepReason::WithinTtl { age_days, ttl_days },
        } = report.classifications[0].action
        else {
            panic!("expected WithinTtl");
        };
        assert_eq!(age_days, 60);
        assert_eq!(ttl_days, 90);
    }

    /// Per-algorithm override shorter than the default prunes a row
    /// that the default would have kept.
    #[test]
    fn per_algorithm_override_shorter_than_default_prunes_row() {
        let now = now_at_2026_05_15();
        let mut policy = WitnessRetentionPolicy::defaults();
        policy
            .per_algorithm_ttl_days
            .insert("ad_hoc_ppr".to_owned(), 7);

        // 10 days old: within default 30 but past ad_hoc_ppr 7.
        let row = witness("ws1", "snap_old", "ad_hoc_ppr", "2026-05-05T12:00:00Z");
        let report = classify_witnesses_for_pruning(&[(row, false)], &policy, now);

        assert_eq!(report.summary.prune_count, 1);
        let WitnessAction::Prune { age_days, ttl_days } = report.classifications[0].action else {
            panic!("expected Prune");
        };
        assert_eq!(age_days, 10);
        assert_eq!(ttl_days, 7);
    }

    /// Malformed `recorded_at` is kept (conservative) and reported
    /// via the unparseable counter so a degraded code can fire.
    #[test]
    fn unparseable_recorded_at_keeps_row_and_increments_counter() {
        let now = now_at_2026_05_15();
        let policy = WitnessRetentionPolicy::defaults();
        let row = witness("ws1", "snap_x", "ppr", "not a timestamp");
        let report = classify_witnesses_for_pruning(&[(row, false)], &policy, now);

        assert_eq!(report.summary.total_count, 1);
        assert_eq!(report.summary.keep_unparseable_recorded_at_count, 1);
        assert_eq!(report.summary.prune_count, 0);
        assert!(matches!(
            report.classifications[0].action,
            WitnessAction::Keep {
                reason: WitnessKeepReason::UnparseableRecordedAt
            }
        ));
    }

    /// Empty input yields an empty report with zero counters and a
    /// schema-tagged envelope.
    #[test]
    fn empty_input_produces_empty_report() {
        let now = now_at_2026_05_15();
        let policy = WitnessRetentionPolicy::defaults();
        let report = classify_witnesses_for_pruning(&[], &policy, now);

        assert_eq!(report.schema, WITNESS_PRUNE_REPORT_SCHEMA_V1);
        assert_eq!(report.summary.total_count, 0);
        assert_eq!(report.summary.prune_count, 0);
        assert_eq!(report.summary.keep_active_snapshot_count, 0);
        assert_eq!(report.summary.keep_within_ttl_count, 0);
        assert_eq!(report.summary.keep_unparseable_recorded_at_count, 0);
        assert!(report.classifications.is_empty());
    }

    /// Determinism: running classification on the same input set in
    /// different orders produces byte-identical JSON.
    #[test]
    fn classification_is_byte_stable_across_input_orders() {
        let now = now_at_2026_05_15();
        let policy = WitnessRetentionPolicy::defaults();
        let rows = vec![
            (
                witness("ws1", "snap_a", "ppr", "2026-04-01T12:00:00Z"),
                false,
            ),
            (
                witness("ws1", "snap_b", "louvain", "2026-05-10T12:00:00Z"),
                true,
            ),
            (
                witness("ws2", "snap_c", "ppr", "2026-05-01T12:00:00Z"),
                false,
            ),
            (
                witness("ws1", "snap_a", "louvain", "2026-04-20T12:00:00Z"),
                false,
            ),
        ];
        let mut reversed = rows.clone();
        reversed.reverse();

        let report_a = classify_witnesses_for_pruning(&rows, &policy, now);
        let report_b = classify_witnesses_for_pruning(&reversed, &policy, now);

        let json_a = serde_json::to_string(&report_a).expect("report A serializes");
        let json_b = serde_json::to_string(&report_b).expect("report B serializes");
        assert_eq!(
            json_a, json_b,
            "classification report must be insertion-order-invariant"
        );
        // Sanity: report includes all four rows.
        assert_eq!(report_a.summary.total_count, 4);
    }

    /// `oldest_pruned_age_days` and `oldest_kept_age_days` track
    /// the maxima across the relevant subsets independently — a
    /// young row that's kept must not lower the kept-max, and a
    /// fresh row that gets pruned must not lower the pruned-max.
    #[test]
    fn summary_oldest_age_fields_track_each_subset_independently() {
        let now = now_at_2026_05_15();
        let policy = WitnessRetentionPolicy::defaults();
        let rows = vec![
            // Pruned: 60d, 31d on inactive snapshots
            (witness("ws1", "old", "ppr", "2026-03-16T12:00:00Z"), false),
            (
                witness("ws1", "old", "louvain", "2026-04-14T12:00:00Z"),
                false,
            ),
            // Kept: 5d on inactive (within TTL) + 100d on active
            // (snapshot guard).
            (
                witness("ws1", "fresh", "ppr", "2026-05-10T12:00:00Z"),
                false,
            ),
            (
                witness("ws1", "active", "ppr", "2026-02-04T12:00:00Z"),
                true,
            ),
        ];
        let report = classify_witnesses_for_pruning(&rows, &policy, now);

        assert_eq!(report.summary.prune_count, 2);
        assert_eq!(report.summary.keep_within_ttl_count, 1);
        assert_eq!(report.summary.keep_active_snapshot_count, 1);
        assert_eq!(report.summary.oldest_pruned_age_days, Some(60));
        // Oldest kept = 100 (the active snapshot row), not 5.
        assert_eq!(report.summary.oldest_kept_age_days, Some(100));
    }

    /// A future-dated `recorded_at` (clock skew) yields age 0 — we
    /// must never produce a negative or wrap-around age that could
    /// classify the row as expired.
    #[test]
    fn future_recorded_at_yields_zero_age_and_keeps_row() {
        let now = now_at_2026_05_15();
        let policy = WitnessRetentionPolicy::defaults();
        // 1 hour in the future.
        let row = witness("ws1", "snap_clockskew", "ppr", "2026-05-15T13:00:00Z");
        let report = classify_witnesses_for_pruning(&[(row, false)], &policy, now);

        assert_eq!(report.summary.prune_count, 0);
        assert_eq!(report.summary.keep_within_ttl_count, 1);
        let WitnessAction::Keep {
            reason: WitnessKeepReason::WithinTtl { age_days, .. },
        } = report.classifications[0].action
        else {
            panic!("expected WithinTtl");
        };
        assert_eq!(age_days, 0);
    }
}
