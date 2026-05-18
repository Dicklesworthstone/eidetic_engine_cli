//! Aggregation helper for degraded-code arrays in agent-facing
//! responses (bd-bife.27).
//!
//! When `ee context` exercises PPR + Pack DNA + Voronoi + Louvain +
//! Ego at once and any structural snapshot is stale, every algorithm
//! emits its own `snapshot_stale` [`DegradationReport`]. The raw
//! concatenation puts N copies of the same code in `degraded[]`;
//! agents read N copies, learn nothing more from the redundancy, and
//! waste tokens. This module collapses same-code entries into one
//! aggregate, escalates severity to the worst observed across
//! emitters, picks the highest-severity emitter's repair hint as the
//! canonical one, and caps the final array length so a single dirty
//! snapshot can't drown the response.
//!
//! The helper is intentionally output-shape-agnostic: it returns
//! [`AggregatedDegradation`] values that any renderer can lower into
//! its own JSON / Markdown / TOON format. Wiring into individual
//! response renderers is tracked separately so this module can land
//! clean and small.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::core::status::DegradationReport;

/// Maximum number of aggregated entries returned in any single
/// response. Excess entries are folded into a synthetic truncation
/// trailer so agents can detect that the visible array dropped some
/// rows.
pub const DEGRADED_AGGREGATION_MAX_ENTRIES: usize = 20;

/// Synthetic code emitted by the truncation trailer. Agents can
/// match this constant to know that one or more aggregate entries
/// were dropped from the visible array.
pub const DEGRADED_AGGREGATION_TRUNCATED_CODE: &str = "degraded_array_truncated";

/// Severity-tier ordering used for escalation. Lower index = lower
/// severity; aggregate severity is the maximum observed across all
/// emitters of the same code. Mirrors the 6-tier vocabulary
/// documented in `tests/fixtures/failure_modes/SCHEMA.md` and
/// surfaced in `docs/degraded_code_taxonomy.md`.
const SEVERITY_RANK: &[(&str, u8)] = &[
    ("info", 0),
    ("low", 1),
    ("warning", 2),
    ("medium", 3),
    ("high", 4),
    ("critical", 5),
];

/// One entry in the aggregated `degraded[]` array.
///
/// `sources` lists the algorithms or surfaces that emitted the
/// underlying code (for example `"ppr"`, `"louvain"`, `"pack_dna"`).
/// When the input had only one source for a code, `sources` is a
/// one-element vector.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AggregatedDegradation {
    pub code: String,
    pub severity: String,
    pub message: String,
    pub repair: String,
    pub sources: Vec<String>,
}

/// Owned input row for degraded aggregation.
///
/// Most status/graph surfaces use static [`DegradationReport`] rows,
/// but renderers such as `ee context` carry owned strings. This type
/// lets those renderers reuse the same aggregation algorithm without
/// leaking strings or duplicating the rules.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DegradationAggregationInput {
    pub source: String,
    pub code: String,
    pub severity: String,
    pub message: String,
    pub repair: String,
}

impl DegradationAggregationInput {
    #[must_use]
    pub fn new(
        source: impl Into<String>,
        code: impl Into<String>,
        severity: impl Into<String>,
        message: impl Into<String>,
        repair: impl Into<String>,
    ) -> Self {
        Self {
            source: source.into(),
            code: code.into(),
            severity: severity.into(),
            message: message.into(),
            repair: repair.into(),
        }
    }
}

struct AggregatedAccumulator {
    code: String,
    severity: String,
    severity_rank: u8,
    message: String,
    repair: String,
    sources: Vec<String>,
}

/// Aggregate `(source, DegradationReport)` pairs into a deterministic
/// vector of [`AggregatedDegradation`].
///
/// Aggregation rules (per the bd-bife.27 acceptance contract):
///
/// 1. **Same code, multiple sources** — merged into one entry with
///    `sources` populated by the emitting algorithm names.
/// 2. **Severity escalation** — aggregate severity is the maximum
///    observed across emitters of the same code, ranked by the
///    6-tier `info < low < warning < medium < high < critical`
///    vocabulary. Unknown severities sort below `info` so a typo'd
///    severity never silently outranks a real one.
/// 3. **Repair-hint deduplication** — the canonical repair hint is
///    the one emitted by the highest-severity source. When two
///    emitters tie on severity the first-seen hint wins, which is
///    deterministic given the input order.
/// 4. **Truncation** — at most
///    [`DEGRADED_AGGREGATION_MAX_ENTRIES`] aggregates are returned.
///    When the input would produce more, the visible array is
///    truncated to `MAX - 1` and a synthetic trailer entry with
///    code [`DEGRADED_AGGREGATION_TRUNCATED_CODE`] reports the
///    dropped count and the dropped codes (sorted, deduplicated).
///
/// Output is sorted by **descending** severity, then by code, so the
/// most urgent aggregates surface first and the order is
/// byte-stable across runs (J7 determinism contract).
#[must_use]
pub fn aggregate_degraded<I>(entries: I) -> Vec<AggregatedDegradation>
where
    I: IntoIterator<Item = (&'static str, DegradationReport)>,
{
    aggregate_degraded_entries(entries.into_iter().map(|(source, report)| {
        DegradationAggregationInput::new(
            source,
            report.code,
            report.severity,
            report.message,
            report.repair,
        )
    }))
}

/// Aggregate owned degraded entries into a deterministic vector of
/// [`AggregatedDegradation`].
#[must_use]
pub fn aggregate_degraded_entries<I>(entries: I) -> Vec<AggregatedDegradation>
where
    I: IntoIterator<Item = DegradationAggregationInput>,
{
    let mut by_code: BTreeMap<String, AggregatedAccumulator> = BTreeMap::new();
    for report in entries {
        let acc = by_code
            .entry(report.code.clone())
            .or_insert_with(|| AggregatedAccumulator {
                code: report.code.clone(),
                severity: report.severity.clone(),
                severity_rank: severity_rank_for(&report.severity),
                message: report.message.clone(),
                repair: report.repair.clone(),
                sources: Vec::new(),
            });
        let report_rank = severity_rank_for(&report.severity);
        if report_rank > acc.severity_rank {
            acc.severity = report.severity.clone();
            acc.severity_rank = report_rank;
            // Use the message and repair from the highest-severity
            // emitter, since that one is the load-bearing one for
            // remediation. Lower-severity hints would otherwise
            // mask the urgent action.
            acc.message = report.message.clone();
            acc.repair = report.repair.clone();
        }
        if !acc.sources.contains(&report.source) {
            acc.sources.push(report.source.clone());
        }
    }

    let mut aggregates: Vec<AggregatedDegradation> = by_code
        .into_values()
        .map(|acc| {
            let mut sources = acc.sources;
            sources.sort_unstable();
            sources.dedup();
            AggregatedDegradation {
                code: acc.code,
                severity: acc.severity,
                message: acc.message,
                repair: acc.repair,
                sources,
            }
        })
        .collect();
    aggregates.sort_by(|a, b| {
        let a_rank = severity_rank_for(&a.severity);
        let b_rank = severity_rank_for(&b.severity);
        b_rank.cmp(&a_rank).then_with(|| a.code.cmp(&b.code))
    });

    if aggregates.len() <= DEGRADED_AGGREGATION_MAX_ENTRIES {
        return aggregates;
    }

    // Truncate to MAX - 1 visible entries plus a synthetic trailer
    // so the caller can detect that the array was dropped from. The
    // trailer carries the dropped count in its message and the
    // dropped codes in `sources` so an operator can still see what
    // got hidden without rerunning.
    let kept = DEGRADED_AGGREGATION_MAX_ENTRIES - 1;
    let dropped: Vec<String> = aggregates[kept..]
        .iter()
        .map(|entry| entry.code.clone())
        .collect();
    let dropped_count = dropped.len();
    aggregates.truncate(kept);
    aggregates.push(AggregatedDegradation {
        code: DEGRADED_AGGREGATION_TRUNCATED_CODE.to_owned(),
        severity: "info".to_owned(),
        message: format!(
            "{dropped_count} additional aggregated degraded entries were truncated; \
             rerun with `--fields full` or check `ee doctor --json` for the full set"
        ),
        repair: "ee doctor --json".to_owned(),
        sources: dropped,
    });
    aggregates
}

fn severity_rank_for(severity: &str) -> u8 {
    SEVERITY_RANK
        .iter()
        .find_map(|(name, rank)| (*name == severity).then_some(*rank))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(
        code: &'static str,
        severity: &'static str,
        message: &'static str,
        repair: &'static str,
    ) -> DegradationReport {
        DegradationReport {
            code,
            severity,
            message,
            repair,
        }
    }

    /// Rule 1: same code from multiple sources collapses into one
    /// aggregate with sources[] populated.
    #[test]
    fn same_code_from_multiple_sources_aggregates_into_one_entry() {
        let entries = vec![
            (
                "ppr",
                report("snapshot_stale", "medium", "stale", "rebuild"),
            ),
            (
                "louvain",
                report("snapshot_stale", "medium", "stale", "rebuild"),
            ),
            (
                "pack_dna",
                report("snapshot_stale", "medium", "stale", "rebuild"),
            ),
        ];

        let aggregates = aggregate_degraded(entries);

        assert_eq!(
            aggregates.len(),
            1,
            "three same-code emitters → 1 aggregate"
        );
        assert_eq!(aggregates[0].code, "snapshot_stale");
        assert_eq!(aggregates[0].sources, vec!["louvain", "pack_dna", "ppr"]);
    }

    /// Rule 2: severity escalates to the maximum observed across
    /// emitters; the load-bearing repair hint travels with it.
    #[test]
    fn severity_escalates_to_max_and_uses_top_severity_repair() {
        let entries = vec![
            (
                "ppr",
                report("snapshot_stale", "low", "low msg", "low repair"),
            ),
            (
                "pack_dna",
                report("snapshot_stale", "high", "high msg", "high repair"),
            ),
            (
                "louvain",
                report("snapshot_stale", "medium", "medium msg", "medium repair"),
            ),
        ];

        let aggregates = aggregate_degraded(entries);

        assert_eq!(aggregates.len(), 1);
        assert_eq!(aggregates[0].severity, "high");
        assert_eq!(aggregates[0].message, "high msg");
        assert_eq!(
            aggregates[0].repair, "high repair",
            "highest-severity repair hint must win"
        );
    }

    /// Rule 3: identical repair hints across emitters of the same
    /// code appear once. (Implicit in the aggregation contract: one
    /// entry → one repair string, regardless of how many sources
    /// emitted that hint.)
    #[test]
    fn duplicate_repair_hints_collapse_with_their_aggregate() {
        let entries = vec![
            (
                "ppr",
                report("idx_cold", "warning", "cold", "ee index warm"),
            ),
            (
                "voronoi",
                report("idx_cold", "warning", "cold", "ee index warm"),
            ),
            (
                "ego",
                report("idx_cold", "warning", "cold", "ee index warm"),
            ),
        ];

        let aggregates = aggregate_degraded(entries);

        assert_eq!(aggregates.len(), 1);
        assert_eq!(aggregates[0].repair, "ee index warm");
        assert_eq!(aggregates[0].sources, vec!["ego", "ppr", "voronoi"]);
    }

    /// Rule 4: the visible array is capped at
    /// DEGRADED_AGGREGATION_MAX_ENTRIES; excess produces a
    /// synthetic trailer carrying the dropped codes.
    #[test]
    fn excess_entries_produce_truncation_trailer_with_dropped_codes() {
        // Build 25 distinct codes, all "low" severity so none
        // outrank each other.
        let codes: Vec<&'static str> = vec![
            "code_aa", "code_ab", "code_ac", "code_ad", "code_ae", "code_af", "code_ag", "code_ah",
            "code_ai", "code_aj", "code_ak", "code_al", "code_am", "code_an", "code_ao", "code_ap",
            "code_aq", "code_ar", "code_as", "code_at", "code_au", "code_av", "code_aw", "code_ax",
            "code_ay",
        ];
        let entries: Vec<(&'static str, DegradationReport)> = codes
            .iter()
            .map(|code| ("emitter_x", report(code, "low", "msg", "repair")))
            .collect();

        let aggregates = aggregate_degraded(entries);

        assert_eq!(
            aggregates.len(),
            DEGRADED_AGGREGATION_MAX_ENTRIES,
            "truncated array length must equal the cap"
        );
        let trailer = aggregates.last().expect("trailer present");
        assert_eq!(trailer.code, DEGRADED_AGGREGATION_TRUNCATED_CODE);
        assert!(
            trailer.message.contains(&format!(
                "{} additional",
                codes.len() - (DEGRADED_AGGREGATION_MAX_ENTRIES - 1)
            )),
            "trailer message must report the dropped count, got: {}",
            trailer.message
        );
        assert!(
            !trailer.sources.is_empty(),
            "trailer must carry the dropped codes in sources"
        );
        // Each dropped code must appear exactly once in the trailer.
        for source in &trailer.sources {
            assert!(codes.contains(&source.as_str()));
        }
    }

    /// Worst-case integration: 8 distinct degradation streams (PPR +
    /// Pack DNA + Voronoi + Louvain + Ego + four others) all
    /// emitting the same `snapshot_stale` code at varying severities
    /// must collapse to one aggregate with the right severity, the
    /// right repair hint, and all 8 sources listed.
    #[test]
    fn worst_case_eight_emitters_one_aggregate_one_clean_entry() {
        let entries = vec![
            ("ppr", report("snapshot_stale", "low", "lo", "lo-repair")),
            (
                "pack_dna",
                report("snapshot_stale", "medium", "med", "med-repair"),
            ),
            (
                "voronoi",
                report("snapshot_stale", "warning", "wn", "wn-repair"),
            ),
            (
                "louvain",
                report("snapshot_stale", "high", "hi", "hi-repair"),
            ),
            ("ego", report("snapshot_stale", "low", "lo2", "lo2-repair")),
            (
                "hits",
                report("snapshot_stale", "medium", "med2", "med2-repair"),
            ),
            (
                "betweenness",
                report("snapshot_stale", "info", "in", "in-repair"),
            ),
            (
                "kcore",
                report("snapshot_stale", "low", "lo3", "lo3-repair"),
            ),
        ];

        let aggregates = aggregate_degraded(entries);

        assert_eq!(aggregates.len(), 1, "one code → one aggregate");
        let entry = &aggregates[0];
        assert_eq!(entry.severity, "high");
        assert_eq!(entry.repair, "hi-repair");
        assert_eq!(entry.sources.len(), 8);
        assert_eq!(
            entry.sources,
            vec![
                "betweenness",
                "ego",
                "hits",
                "kcore",
                "louvain",
                "pack_dna",
                "ppr",
                "voronoi",
            ],
        );
    }

    /// Determinism: the same input set in different orders must
    /// produce byte-identical JSON.
    #[test]
    fn aggregation_output_is_byte_stable_across_input_orders() {
        let mut order_a = vec![
            ("emit_a", report("code_x", "medium", "x", "fix-x")),
            ("emit_b", report("code_y", "high", "y", "fix-y")),
            ("emit_c", report("code_x", "low", "x", "fix-x")),
            ("emit_d", report("code_z", "warning", "z", "fix-z")),
        ];
        let mut order_b = order_a.clone();
        order_b.reverse();

        let agg_a = aggregate_degraded(order_a.drain(..));
        let agg_b = aggregate_degraded(order_b.drain(..));

        let json_a = serde_json::to_string(&agg_a).expect("agg A serializes");
        let json_b = serde_json::to_string(&agg_b).expect("agg B serializes");
        assert_eq!(
            json_a, json_b,
            "aggregate output must be insertion-order-invariant"
        );
        // Spot-check ordering: highest severity (code_y, high) first.
        assert_eq!(agg_a[0].code, "code_y");
        assert_eq!(agg_a[0].severity, "high");
    }

    /// Unknown / typo'd severities must not silently outrank known
    /// ones — they sort below `info`.
    #[test]
    fn unknown_severity_sorts_below_known_severities() {
        let entries = vec![
            ("real", report("real_code", "info", "info-msg", "info-fix")),
            (
                "typo",
                report("typo_code", "criticla", "typo-msg", "typo-fix"),
            ),
        ];

        let aggregates = aggregate_degraded(entries);

        assert_eq!(aggregates.len(), 2);
        assert_eq!(
            aggregates[0].code, "real_code",
            "real info entry must rank above the typo'd severity"
        );
        assert_eq!(aggregates[1].code, "typo_code");
    }

    /// Empty input must produce empty output, not a trailer.
    #[test]
    fn empty_input_produces_empty_aggregate_array() {
        let aggregates = aggregate_degraded(std::iter::empty());
        assert!(aggregates.is_empty());
    }
}
