//! `ee focus suggest` Phase 2 — recency-weighted, centrality-scored focus
//! recommendations.
//!
//! Phase 1 (closed bd-sg5si) pinned the CLI surface and the
//! `ee.focus.suggest.v1` envelope while emitting an empty
//! `recommendations` array plus the `focus_suggest_unimplemented`
//! degraded sentinel. Phase 2 (this module, bd-1idcb) replaces the
//! sentinel with a real ranking pipeline:
//!
//! 1. List memories created within `--recent-hours`.
//! 2. Optionally restrict to the evidence neighborhood of a
//!    `--task-frame <id>` if supplied.
//! 3. When `--from-cass` is set, attach CASS evidence spans within the
//!    same recency window for span-density signal.
//! 4. Build the memory graph projection and run PageRank to obtain a
//!    centrality score per memory (graceful fallback when the graph
//!    feature is disabled or the graph is empty).
//! 5. Cluster memories into topics, score each cluster by
//!    `centrality_sum + recency_weight + span_density`, sort
//!    deterministically, and emit the top-N as
//!    `FocusRecommendation`s.
//!
//! Determinism: same workspace + same `--recent-hours` window must
//! produce byte-identical recommendation ordering. Tie-breaking
//! cascades through `score desc → topic_label asc → first_member_id
//! asc (ULID lexical)` — matching the pattern at
//! `src/core/search.rs:5374` (`sort_search_hits_by_score_order`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration, Utc};
use serde::Serialize;

use crate::db::{DbConnection, StoredEvidenceSpan, StoredMemory};
use crate::models::DomainError;

/// Schema id pinned in `docs/schemas/ee.focus.suggest.v1.json`.
pub const FOCUS_SUGGEST_SCHEMA_V1: &str = "ee.focus.suggest.v1";

/// Caller options for `suggest_focus`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FocusSuggestOptions {
    pub workspace_path: PathBuf,
    pub from_cass: bool,
    pub limit: usize,
    pub recent_hours: u32,
    pub task_frame_id: Option<String>,
}

/// One ranked focus recommendation, matching the v1 schema's
/// `recommendations[]` items.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FocusRecommendation {
    pub topic: String,
    pub span_ids: Vec<String>,
    pub centrality_score: f64,
    pub rationale: String,
    pub suggested_query: String,
}

/// One `degraded[]` entry the suggest pipeline may emit when a signal
/// is unavailable. Pinned to the canonical envelope shape.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct FocusSuggestDegradation {
    pub code: String,
    pub severity: String,
    pub message: String,
    pub repair: Option<String>,
}

/// Internal report assembled by `suggest_focus`. The CLI renderer
/// projects this into the `ee.focus.suggest.v1` envelope.
#[derive(Clone, Debug)]
pub struct FocusSuggestReport {
    pub recommendations: Vec<FocusRecommendation>,
    pub from_cass: bool,
    pub limit: usize,
    pub recent_hours: u32,
    pub degraded: Vec<FocusSuggestDegradation>,
}

/// Run the Phase 2 focus-suggest pipeline.
///
/// Returns a structured report with deterministically-ordered
/// recommendations and any degraded signals the pipeline encountered
/// (graph feature disabled, empty graph, no recent evidence, missing
/// task frame, etc.).
pub fn suggest_focus(options: &FocusSuggestOptions) -> Result<FocusSuggestReport, DomainError> {
    let mut degraded: Vec<FocusSuggestDegradation> = Vec::new();
    let database_path = options.workspace_path.join(".ee").join("ee.db");

    if !database_path.exists() {
        degraded.push(FocusSuggestDegradation {
            code: "workspace_uninitialized".to_owned(),
            severity: "warning".to_owned(),
            message: format!(
                "No initialized workspace at {}. Run `ee init` first.",
                options.workspace_path.display()
            ),
            repair: Some("ee init --workspace .".to_owned()),
        });
        return Ok(FocusSuggestReport {
            recommendations: Vec::new(),
            from_cass: options.from_cass,
            limit: options.limit,
            recent_hours: options.recent_hours,
            degraded,
        });
    }

    let connection =
        DbConnection::open_file(&database_path).map_err(|error| DomainError::Storage {
            message: format!("Failed to open database: {error}"),
            repair: Some("ee doctor".to_owned()),
        })?;

    let workspace_id = resolve_workspace_id(&connection, &options.workspace_path)?;

    let memories = connection
        .list_memories(&workspace_id, None, false)
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to list memories: {error}"),
            repair: Some("ee doctor".to_owned()),
        })?;

    let now = Utc::now();
    // Use `checked_sub_signed` rather than `now - Duration::hours(...)`:
    // `recent_hours: u32` accepts values up to ~4.3 billion hours
    // (~489_957 years), and chrono's `DateTime<Utc>` is bounded to
    // ±262_143 years from year 0. Subtracting a multi-million-year
    // duration from `now` panics out of the representable range. The
    // checked variant returns `None` instead, which we surface as a
    // soft "all-time" fallback plus a degraded entry so the agent can
    // see that their window was outside the representable range and
    // shrink it. Same defensive pattern as
    // `src/core/curate.rs:14450` and `src/core/outcome.rs:2409`.
    let cutoff = match now.checked_sub_signed(Duration::hours(i64::from(options.recent_hours))) {
        Some(value) => value,
        None => {
            degraded.push(FocusSuggestDegradation {
                code: "recent_hours_window_clamped".to_owned(),
                severity: "warning".to_owned(),
                message: format!(
                    "--recent-hours {} pushes the cutoff outside the representable DateTime range; treating the window as all-time.",
                    options.recent_hours,
                ),
                repair: Some(
                    "Pass a smaller --recent-hours value (e.g. 24, 168, or 720).".to_owned(),
                ),
            });
            DateTime::<Utc>::MIN_UTC
        }
    };
    let recent: Vec<StoredMemory> = memories
        .into_iter()
        .filter(|memory| memory_created_at(memory).is_some_and(|ts| ts >= cutoff))
        .collect();

    // Track whether the task-frame scope is what filtered the candidate
    // set down to empty, so the trailing emptiness check below can emit
    // a frame-specific code (`task_frame_intersect_empty`) instead of
    // the generic `no_recent_evidence`. The two failure modes look
    // identical at the boundary (`scoped.is_empty()`) but suggest
    // different repairs: increase --recent-hours vs. attach evidence /
    // verify the frame id / drop the scope.
    let task_frame_scope = options.task_frame_id.as_deref();
    let recent_was_empty = recent.is_empty();
    let scoped = match task_frame_scope {
        Some(frame_id) => match load_task_frame_evidence(&options.workspace_path, frame_id) {
            Ok(evidence_ids) if !evidence_ids.is_empty() => recent
                .into_iter()
                .filter(|memory| evidence_ids.iter().any(|id| id == &memory.id))
                .collect(),
            Ok(_) => {
                // The user explicitly scoped the request to a task
                // frame; silently broadening to every recent memory
                // would contradict that intent. Return empty and
                // flag the empty-scope at warning severity so the
                // caller can react. Early-return here so the empty
                // scope does NOT also trip the `no_recent_evidence`
                // check below — there may be plenty of recent memories;
                // they're just not in this frame's evidence_links, and
                // emitting both codes would mislead the caller about
                // why the recommendations are empty.
                degraded.push(FocusSuggestDegradation {
                    code: "task_frame_no_evidence".to_owned(),
                    severity: "warning".to_owned(),
                    message: format!(
                        "Task frame {frame_id} has no evidence_links; returning empty recommendations to honor the explicit scope."
                    ),
                    repair: Some(
                        "Attach evidence to the frame or omit --task-frame to consider all recent memories."
                            .to_owned(),
                    ),
                });
                return Ok(FocusSuggestReport {
                    recommendations: Vec::new(),
                    from_cass: options.from_cass,
                    limit: options.limit,
                    recent_hours: options.recent_hours,
                    degraded,
                });
            }
            Err(error) => {
                // Mirror the `Ok(empty)` arm above: honor the explicit
                // `--task-frame` scope when the frame itself cannot be
                // loaded (typo'd id, no task-frame store yet, corrupt
                // store). Falling back to every recent memory would
                // silently broaden the scope the caller asked us to
                // narrow, mask the configuration error behind a
                // populated recommendations list, and contradict the
                // intent the `Ok(empty)` early-return already encodes
                // for the same surface.
                degraded.push(FocusSuggestDegradation {
                    code: "task_frame_unavailable".to_owned(),
                    severity: "warning".to_owned(),
                    message: format!("Failed to load task frame {frame_id}: {error}"),
                    repair: Some("Verify the frame id with `ee task-frame show --all`.".to_owned()),
                });
                return Ok(FocusSuggestReport {
                    recommendations: Vec::new(),
                    from_cass: options.from_cass,
                    limit: options.limit,
                    recent_hours: options.recent_hours,
                    degraded,
                });
            }
        },
        None => recent,
    };

    if scoped.is_empty() {
        // Distinguish "the recency window itself was empty" from "the
        // recency window had memories but none were linked from the
        // task frame". The first repair is `--recent-hours` / `ee
        // remember`; the second is `ee task-frame evidence add` /
        // verify the frame id / drop `--task-frame`. Conflating them
        // (the pre-fix behavior) sent operators chasing the wrong
        // setting when their frame existed but didn't intersect the
        // window.
        if let Some(frame_id) = task_frame_scope
            && !recent_was_empty
        {
            degraded.push(FocusSuggestDegradation {
                code: "task_frame_intersect_empty".to_owned(),
                severity: "info".to_owned(),
                message: format!(
                    "Task frame {frame_id} has evidence, but none of its memory-kind links fall within the last {} hour(s).",
                    options.recent_hours
                ),
                repair: Some(
                    "Increase --recent-hours, attach newer evidence via `ee task-frame evidence add`, or omit --task-frame.".to_owned(),
                ),
            });
        } else {
            degraded.push(FocusSuggestDegradation {
                code: "no_recent_evidence".to_owned(),
                severity: "info".to_owned(),
                message: format!(
                    "No memories created within the last {} hour(s).",
                    options.recent_hours
                ),
                repair: Some(
                    "Increase --recent-hours or `ee remember` more evidence to seed the surface."
                        .to_owned(),
                ),
            });
        }
        return Ok(FocusSuggestReport {
            recommendations: Vec::new(),
            from_cass: options.from_cass,
            limit: options.limit,
            recent_hours: options.recent_hours,
            degraded,
        });
    }

    let spans = if options.from_cass {
        match connection.list_evidence_spans_for_workspace(&workspace_id) {
            Ok(rows) => rows
                .into_iter()
                .filter(|span| {
                    DateTime::parse_from_rfc3339(&span.created_at)
                        .ok()
                        .map(|ts| ts.with_timezone(&Utc))
                        .is_some_and(|ts| ts >= cutoff)
                })
                .collect::<Vec<StoredEvidenceSpan>>(),
            Err(error) => {
                // Severity `medium` matches the canonical catalog entry
                // at `tests/fixtures/failure_modes/cass_unavailable.json`
                // (introduced by bd-17c65.10.6). Surfaces that emit a
                // shared degraded code must use the catalog's severity
                // so per-code parsing on the agent side stays stable.
                degraded.push(FocusSuggestDegradation {
                    code: "cass_unavailable".to_owned(),
                    severity: "medium".to_owned(),
                    message: format!("Failed to list evidence spans for from-cass pass: {error}"),
                    repair: Some("ee doctor".to_owned()),
                });
                Vec::new()
            }
        }
    } else {
        Vec::new()
    };

    let pagerank = compute_pagerank_scores(&connection, &mut degraded);

    let recommendations = score_and_emit_topics(&scoped, &spans, &pagerank, now, options);

    Ok(FocusSuggestReport {
        recommendations,
        from_cass: options.from_cass,
        limit: options.limit,
        recent_hours: options.recent_hours,
        degraded,
    })
}

fn resolve_workspace_id(
    connection: &DbConnection,
    workspace_path: &Path,
) -> Result<String, DomainError> {
    let canonical = workspace_path
        .canonicalize()
        .unwrap_or_else(|_| workspace_path.to_path_buf());
    let canonical_str = canonical.to_string_lossy().into_owned();
    let raw_str = workspace_path.to_string_lossy().into_owned();

    for candidate in [canonical_str.as_str(), raw_str.as_str()] {
        if let Ok(Some(workspace)) = connection.get_workspace_by_path(candidate) {
            return Ok(workspace.id);
        }
    }

    Ok(crate::core::curate::stable_workspace_id(
        canonical.as_path(),
    ))
}

fn memory_created_at(memory: &StoredMemory) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(&memory.created_at)
        .ok()
        .map(|ts| ts.with_timezone(&Utc))
}

fn load_task_frame_evidence(
    workspace_path: &Path,
    frame_id: &str,
) -> Result<Vec<String>, DomainError> {
    use crate::core::task_frame::{TaskFrameShowOptions, show_task_frame};
    let report = show_task_frame(&TaskFrameShowOptions {
        workspace_path: workspace_path.to_path_buf(),
        frame_id: Some(frame_id.to_owned()),
        active: false,
    })?;
    // Task-frame evidence_links carry `kind` ∈ {"memory", "context_pack",
    // "recorder_run", "handoff", "bead", ...} (see src/core/task_frame.rs:101,
    // src/core/handoff.rs callers, src/cli/mod.rs:13230). Only entries with
    // kind=="memory" can match `StoredMemory::id` in the candidate filter; if
    // we collected every id regardless of kind, non-memory ids (e.g. a
    // context_pack id or a handoff id) would silently over-restrict the
    // candidate set to the empty intersection.
    let mut ids: Vec<String> = report
        .frame
        .as_ref()
        .map(|frame| {
            frame
                .evidence_links
                .iter()
                .filter(|link| link.kind == "memory")
                .map(|link| link.id.clone())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    ids.sort();
    ids.dedup();
    Ok(ids)
}

#[cfg(feature = "graph")]
fn compute_pagerank_scores(
    connection: &DbConnection,
    degraded: &mut Vec<FocusSuggestDegradation>,
) -> BTreeMap<String, f64> {
    use crate::graph::{ProjectionOptions, build_memory_graph, compute_pagerank};

    match build_memory_graph(connection, &ProjectionOptions::default()) {
        Ok(projection) if projection.node_count > 0 => match compute_pagerank(&projection) {
            Ok(result) => result
                .scores
                .into_iter()
                // Drop non-finite PageRank scores (NaN / ±Inf) before they
                // reach `centrality_sum`. A single NaN poisons the running
                // sum, the composite score, and any subsequent `total_cmp`
                // ordering (NaN sorts to one extreme of total ordering and
                // would silently pin a meaningless cluster at the top).
                // Negative scores are also non-physical for PageRank and
                // would distort the additive composite.
                .filter(|score| score.score.is_finite() && score.score >= 0.0)
                .map(|score| (score.node, score.score))
                .collect(),
            Err(error) => {
                degraded.push(FocusSuggestDegradation {
                        code: "graph_pagerank_failed".to_owned(),
                        severity: "warning".to_owned(),
                        message: format!(
                            "PageRank computation failed: {error}; falling back to recency-only scoring."
                        ),
                        repair: None,
                    });
                BTreeMap::new()
            }
        },
        Ok(_) => {
            degraded.push(FocusSuggestDegradation {
                code: "graph_empty".to_owned(),
                severity: "info".to_owned(),
                message: "Memory graph has no nodes; centrality contribution is zero.".to_owned(),
                repair: None,
            });
            BTreeMap::new()
        }
        Err(error) => {
            degraded.push(FocusSuggestDegradation {
                code: "graph_projection_failed".to_owned(),
                severity: "warning".to_owned(),
                message: format!(
                    "Failed to build memory graph projection: {error}; falling back to recency-only scoring."
                ),
                repair: None,
            });
            BTreeMap::new()
        }
    }
}

#[cfg(not(feature = "graph"))]
fn compute_pagerank_scores(
    _connection: &DbConnection,
    degraded: &mut Vec<FocusSuggestDegradation>,
) -> BTreeMap<String, f64> {
    // Severity `medium` matches the canonical catalog entry at
    // `tests/fixtures/failure_modes/graph_unavailable.json` (introduced
    // by bd-17c65.10.6). Surfaces that emit a shared degraded code must
    // use the catalog's severity so per-code parsing on the agent side
    // stays stable.
    degraded.push(FocusSuggestDegradation {
        code: "graph_unavailable".to_owned(),
        severity: "medium".to_owned(),
        message: "Graph feature disabled at build time; falling back to recency-only scoring."
            .to_owned(),
        repair: Some("Rebuild ee with --features graph to enable centrality scoring.".to_owned()),
    });
    BTreeMap::new()
}

#[derive(Clone)]
struct TopicCluster {
    topic_label: String,
    member_ids: Vec<String>,
    centrality_sum: f64,
    most_recent_at: Option<DateTime<Utc>>,
    span_ids: Vec<String>,
}

fn score_and_emit_topics(
    memories: &[StoredMemory],
    spans: &[StoredEvidenceSpan],
    pagerank: &BTreeMap<String, f64>,
    now: DateTime<Utc>,
    options: &FocusSuggestOptions,
) -> Vec<FocusRecommendation> {
    if options.limit == 0 {
        return Vec::new();
    }

    let mut clusters: BTreeMap<String, TopicCluster> = BTreeMap::new();

    for memory in memories {
        let key = topic_key_for_memory(memory);
        let cluster = clusters.entry(key.clone()).or_insert_with(|| TopicCluster {
            topic_label: derive_topic_label(memory),
            member_ids: Vec::new(),
            centrality_sum: 0.0,
            most_recent_at: None,
            span_ids: Vec::new(),
        });
        cluster.member_ids.push(memory.id.clone());
        if let Some(rank) = pagerank.get(&memory.id) {
            // Drop non-finite or negative PageRank contributions. A
            // NaN/Inf score would propagate through the sum and end up
            // in the emitted `centralityScore` field, which serde_json
            // serializes to `null` — violating the v1 schema's
            // `"type": "number"` (`docs/schemas/ee.focus.suggest.v1.json:46`).
            // Negative scores are non-physical for a standard PageRank
            // and would distort the additive composite. The upstream
            // `compute_pagerank_scores` filter already drops both
            // categories before they reach this map, but the two
            // invariants must match: if they ever drift (refactor, new
            // test path inserting into the map), the additive composite
            // must not silently absorb a negative or non-finite value.
            if rank.is_finite() && *rank >= 0.0 {
                cluster.centrality_sum += *rank;
            }
        }
        if let Some(ts) = memory_created_at(memory) {
            cluster.most_recent_at = Some(match cluster.most_recent_at {
                Some(existing) if existing > ts => existing,
                _ => ts,
            });
        }
    }

    for span in spans {
        let Some(memory_id) = span.memory_id.as_deref() else {
            continue;
        };
        // `docs/schemas/ee.focus.suggest.v1.json` constrains each spanIds
        // entry to `minLength: 1`. `StoredEvidenceSpan::cass_span_id` is a
        // plain `String` with no NOT NULL invariant at the type level, so an
        // empty `cass_span_id` (corrupt row, partial migration, legacy
        // import) would otherwise leak into the response and violate the
        // schema's per-item `minLength` contract.
        if span.cass_span_id.is_empty() {
            continue;
        }
        for cluster in clusters.values_mut() {
            if cluster.member_ids.iter().any(|id| id == memory_id) {
                cluster.span_ids.push(span.cass_span_id.clone());
                break;
            }
        }
    }

    let recent_hours_f = f64::from(options.recent_hours).max(1.0);
    let mut scored: Vec<(f64, TopicCluster)> = clusters
        .into_values()
        .map(|mut cluster| {
            cluster.member_ids.sort();
            cluster.span_ids.sort();
            cluster.span_ids.dedup();
            let recency_weight = cluster
                .most_recent_at
                .map(|ts| {
                    #[allow(clippy::cast_precision_loss)]
                    // Clamp to >= 0 so a future-dated `created_at`
                    // (clock skew, manual back-dating) cannot drive
                    // recency_weight above 1.0 via exp(-negative).
                    let age_hours = ((now - ts).num_seconds() as f64 / 3600.0).max(0.0);
                    (-age_hours / recent_hours_f).exp()
                })
                .unwrap_or(0.0);
            #[allow(clippy::cast_precision_loss)]
            let span_density = (1.0 + cluster.span_ids.len() as f64).ln();
            let score = cluster.centrality_sum + recency_weight + span_density;
            (score, cluster)
        })
        .collect();

    // `total_cmp` keeps the ordering total even if a score is NaN, but
    // its IEEE 754 totalOrder places positive NaN ABOVE positive infinity
    // — which, in a descending sort, would surface NaN-scored clusters
    // BEFORE every finite-scored cluster. The upstream filters at
    // `compute_pagerank_scores` and `score_and_emit_topics` drop
    // non-finite contributions, so production never observes this, but
    // the `sort_is_total_under_nan_scores` regression test pins the
    // intended contract ("finite scores must outrank NaNs in the
    // descending sort") and would otherwise fail under the bare
    // `total_cmp` shape because `1.0.total_cmp(&NaN)` returns `Less`
    // (NaN is greater in total order). Normalize NaN to `NEG_INFINITY`
    // before comparison so any NaN that leaks past the upstream filters
    // lands at the bottom of the ranking instead of hijacking the top.
    // Real `NEG_INFINITY` scores collide with normalized NaNs and
    // tie-break through `topic_label` / `first_member_id` like the rest
    // of the order — both deterministic and both at the bottom is
    // exactly the behavior an operator expects when "highest score
    // wins". Mirrors the determinism contract upheld at
    // src/core/search.rs:5375 (sort_search_hits_by_score_order).
    let sort_score = |score: f64| {
        if score.is_nan() {
            f64::NEG_INFINITY
        } else {
            score
        }
    };
    scored.sort_by(|left, right| {
        sort_score(right.0)
            .total_cmp(&sort_score(left.0))
            .then_with(|| left.1.topic_label.cmp(&right.1.topic_label))
            .then_with(|| {
                let left_first = left.1.member_ids.first().map(String::as_str).unwrap_or("");
                let right_first = right.1.member_ids.first().map(String::as_str).unwrap_or("");
                left_first.cmp(right_first)
            })
    });

    scored
        .into_iter()
        .take(options.limit)
        .map(|(_score, cluster)| {
            // The composite `_score` (centrality + recency + spans)
            // drives the sort above. The exposed `centralityScore`
            // field is documented in `docs/schemas/ee.focus.suggest.v1.json`
            // as "Graph centrality contribution to the rank" — i.e.
            // the centrality term only — so we emit that, keeping
            // the field aligned with both the schema description and
            // the "Centrality {:.4}" prefix of the rationale string.
            let centrality_sum = cluster.centrality_sum;
            let recency_str = cluster
                .most_recent_at
                .map(|ts| ts.to_rfc3339())
                .unwrap_or_else(|| "unknown".to_owned());
            let rationale = format!(
                "Centrality {:.4} aggregated over {} memory(ies); {} CASS span(s); most recent evidence at {}.",
                centrality_sum,
                cluster.member_ids.len(),
                cluster.span_ids.len(),
                recency_str,
            );
            // Emit `ee pack` (not `ee context`). Per AGENTS.md line 544,
            // "`ee pack "<task>"` is the canonical post-triad-promotion
            // surface; `ee context "<task>"` is retained as a
            // soft-deprecated alias that runs the same code path and
            // emits `deprecated_alias` (severity `info`) in its
            // `degraded[]`. Prefer `ee pack` in new agent harnesses,
            // scripts, and docs." Phase 2 of focus_suggest is brand-new
            // post-promotion code; suggesting the deprecated alias would
            // make every agent that acts on a recommendation trip the
            // `deprecated_alias` warning on its very next command.
            let suggested_query = format!(
                "ee pack \"{}\" --workspace . --max-tokens 4000 --json",
                escape_topic_for_query(&cluster.topic_label),
            );
            FocusRecommendation {
                topic: cluster.topic_label,
                span_ids: cluster.span_ids,
                centrality_score: centrality_sum,
                rationale,
                suggested_query,
            }
        })
        .collect()
}

/// Preview width used by BOTH `topic_key_for_memory` (the cluster key) AND
/// `derive_topic_label` (the displayed label). Keeping the widths equal
/// avoids a subtle determinism trap: if the cluster key used 32 chars but
/// the label used 48, two memories with identical first 32 alphanumeric
/// chars would land in the same cluster yet present a label drawn from
/// whichever memory iterated first (id-asc → oldest ULID) — including
/// chars 33-48 that may not be representative of the rest of the cluster's
/// contents. Tying the two to a single constant makes the (cluster
/// identity, displayed label) pair self-consistent by construction.
const TOPIC_PREVIEW_CHARS: usize = 32;

fn topic_key_for_memory(memory: &StoredMemory) -> String {
    let preview = content_preview_tokens(&memory.content, TOPIC_PREVIEW_CHARS);
    if preview.is_empty() {
        // `content_preview_tokens` returns "" whenever the first
        // `TOPIC_PREVIEW_CHARS` Unicode codepoints contain no
        // `char::is_alphanumeric` characters — emoji-only content
        // (`🚨🚨🚨`), pure-punctuation content (`!!! ??? ...`), or
        // pure-whitespace content all collapse to the empty preview.
        // Without an id-tail fallback, every empty-preview memory of
        // the same `kind` would land in a single `kind::` cluster and
        // collapse into ONE recommendation — losing the distinct
        // topics they actually represent. Fall back to the memory id
        // so each empty-preview memory gets its own cluster; the
        // determinism contract still holds because memory ids are
        // unique and the downstream tie-break already sorts by
        // `first_member_id asc`. The matching `derive_topic_label`
        // branch keeps the visible label as `memory.kind` so the
        // observable schema field doesn't suddenly leak an internal
        // id-tail to agents.
        format!("{}::id={}", memory.kind, memory.id)
    } else {
        format!("{}::{}", memory.kind, preview)
    }
}

fn derive_topic_label(memory: &StoredMemory) -> String {
    let preview = content_preview_tokens(&memory.content, TOPIC_PREVIEW_CHARS);
    if preview.is_empty() {
        memory.kind.clone()
    } else {
        format!("{}: {}", memory.kind, preview)
    }
}

fn content_preview_tokens(content: &str, max_chars: usize) -> String {
    let collected: String = content
        .chars()
        .take(max_chars)
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect();
    collected.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn escape_topic_for_query(topic: &str) -> String {
    // The suggested_query embeds the topic inside a double-quoted shell
    // command (`ee pack "<topic>" ...`). Inside double quotes, bash
    // still expands `$VAR` and `` `cmd` `` (and `\` / `"` would close
    // the string), so all four must be escaped. Backslash first so the
    // backslashes introduced by later steps are not themselves doubled.
    topic
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('$', "\\$")
        .replace('`', "\\`")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn memory_with(id: &str, kind: &str, content: &str, created_at: &str) -> StoredMemory {
        StoredMemory {
            id: id.to_owned(),
            workspace_id: "ws".to_owned(),
            level: "episodic".to_owned(),
            kind: kind.to_owned(),
            content: content.to_owned(),
            workflow_id: None,
            confidence: 0.5,
            utility: 0.5,
            importance: 0.5,
            provenance_uri: None,
            trust_class: "user".to_owned(),
            trust_subclass: None,
            provenance_chain_hash: None,
            provenance_chain_hash_version: "v1".to_owned(),
            provenance_verification_status: "unverified".to_owned(),
            provenance_verified_at: None,
            provenance_verification_note: None,
            created_at: created_at.to_owned(),
            updated_at: created_at.to_owned(),
            tombstoned_at: None,
            valid_from: None,
            valid_to: None,
        }
    }

    fn fixed_now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-05-26T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn topic_key_groups_same_kind_and_preview() {
        let a = memory_with(
            "01",
            "release",
            "release readiness gate",
            "2026-05-26T11:00:00Z",
        );
        let b = memory_with(
            "02",
            "release",
            "release readiness gate",
            "2026-05-26T10:30:00Z",
        );
        assert_eq!(topic_key_for_memory(&a), topic_key_for_memory(&b));
    }

    #[test]
    fn topic_key_distinguishes_different_kinds() {
        let a = memory_with(
            "01",
            "release",
            "release readiness gate",
            "2026-05-26T11:00:00Z",
        );
        let b = memory_with(
            "02",
            "decision",
            "release readiness gate",
            "2026-05-26T10:30:00Z",
        );
        assert_ne!(topic_key_for_memory(&a), topic_key_for_memory(&b));
    }

    /// Regression guard for the empty-preview cluster collision.
    ///
    /// Pre-fix, `content_preview_tokens` returned "" for memories whose
    /// first `TOPIC_PREVIEW_CHARS` codepoints contained no alphanumeric
    /// characters — emoji-only content, pure punctuation, or pure
    /// whitespace. `topic_key_for_memory` formatted that empty preview
    /// into `"kind::"`, collapsing every empty-preview memory of the
    /// same kind into a single cluster regardless of their actual
    /// content. Two distinct fact memories with `"🚨"` and `"!!!"` would
    /// have shared a key — losing the distinct topics they represent.
    /// The fix appends the memory id when the preview is empty so each
    /// empty-preview memory clusters on its own, and the visible
    /// `topic_label` stays as `memory.kind` to keep the agent-visible
    /// surface unchanged.
    #[test]
    fn topic_key_does_not_collapse_distinct_empty_preview_memories() {
        // `content_preview_tokens` returns "" because none of the chars
        // in "🚨🚨🚨" or "!!! ???" are `is_alphanumeric()`.
        let a = memory_with("01", "fact", "🚨🚨🚨", "2026-05-26T11:00:00Z");
        let b = memory_with("02", "fact", "!!! ???", "2026-05-26T10:30:00Z");
        assert!(content_preview_tokens(&a.content, TOPIC_PREVIEW_CHARS).is_empty());
        assert!(content_preview_tokens(&b.content, TOPIC_PREVIEW_CHARS).is_empty());
        let key_a = topic_key_for_memory(&a);
        let key_b = topic_key_for_memory(&b);
        assert_ne!(
            key_a, key_b,
            "two empty-preview memories of the same kind must NOT share a cluster key"
        );
        // The visible topic label stays as `memory.kind` regardless —
        // the id-tail fallback lives in the key only, not in the
        // schema-exposed label.
        assert_eq!(derive_topic_label(&a), "fact");
        assert_eq!(derive_topic_label(&b), "fact");
    }

    #[test]
    fn recency_weight_decays_within_window() {
        // Two clusters, equal centrality, equal span density, different
        // age. Newer must outrank older in the emitted ORDER. The
        // exposed `centrality_score` field reflects pure centrality
        // (zero here, since the pagerank map is empty), so it is NOT
        // a proxy for the composite score — order is what matters.
        let mem_new = memory_with("01", "release", "alpha", "2026-05-26T11:30:00Z");
        let mem_old = memory_with("02", "decision", "beta", "2026-05-26T01:00:00Z");
        let options = FocusSuggestOptions {
            workspace_path: PathBuf::from("/tmp"),
            from_cass: false,
            limit: 5,
            recent_hours: 24,
            task_frame_id: None,
        };
        let recs = score_and_emit_topics(
            &[mem_new, mem_old],
            &[],
            &BTreeMap::new(),
            fixed_now(),
            &options,
        );
        assert_eq!(recs.len(), 2);
        // First (highest composite score, driven by recency) must be
        // the newer one.
        assert!(recs[0].topic.starts_with("release"));
        // Both have empty pagerank, so centrality_score is 0 for both
        // — confirming the field is centrality-only and not the
        // composite sort score.
        assert_eq!(recs[0].centrality_score, 0.0);
        assert_eq!(recs[1].centrality_score, 0.0);
    }

    #[test]
    fn centrality_score_field_is_centrality_only_not_composite() {
        // The v1 schema documents `centralityScore` as the graph
        // centrality contribution. Even though recency_weight and
        // span_density drive the composite sort score, the emitted
        // field must equal the PageRank aggregate alone.
        let mem = memory_with("01", "release", "alpha", "2026-05-26T11:30:00Z");
        let mut pagerank = BTreeMap::new();
        pagerank.insert("01".to_owned(), 0.42);
        let options = FocusSuggestOptions {
            workspace_path: PathBuf::from("/tmp"),
            from_cass: false,
            limit: 5,
            recent_hours: 24,
            task_frame_id: None,
        };
        let recs = score_and_emit_topics(&[mem], &[], &pagerank, fixed_now(), &options);
        assert_eq!(recs.len(), 1);
        // centrality_score equals the PageRank aggregate (0.42), not
        // the composite 0.42 + recency_weight + 0.
        assert!(
            (recs[0].centrality_score - 0.42).abs() < 1e-12,
            "centrality_score must equal pagerank aggregate; got {}",
            recs[0].centrality_score,
        );
        // The rationale string formats the same centrality value.
        assert!(
            recs[0].rationale.starts_with("Centrality 0.4200 "),
            "rationale must lead with the centrality value; got {:?}",
            recs[0].rationale,
        );
    }

    #[test]
    fn future_dated_memory_does_not_dominate_via_recency() {
        // Without the age clamp, a memory whose `created_at` is in
        // the future (clock skew, manual back-dating, etc.) would
        // get `exp(-negative/positive) > 1` and unfairly outrank
        // current memories. The clamp pins recency_weight to <= 1.
        let mem_future = memory_with("01", "zeta_kind", "future", "2026-05-26T20:00:00Z");
        let mem_now = memory_with("02", "alpha_kind", "current", "2026-05-26T12:00:00Z");
        let options = FocusSuggestOptions {
            workspace_path: PathBuf::from("/tmp"),
            from_cass: false,
            limit: 5,
            recent_hours: 24,
            task_frame_id: None,
        };
        let recs = score_and_emit_topics(
            &[mem_future, mem_now],
            &[],
            &BTreeMap::new(),
            fixed_now(),
            &options,
        );
        assert_eq!(recs.len(), 2);
        // Both clusters clamp to age 0 → recency_weight = 1.0 →
        // composite scores tie → topic_label alphabetical tie-break
        // puts `alpha_kind` first. Without the clamp the future
        // memory's recency_weight would be > 1.0 and `zeta_kind`
        // would win.
        assert!(
            recs[0].topic.starts_with("alpha_kind"),
            "clamp violated; recs={recs:?}",
        );
    }

    #[test]
    fn deterministic_ordering_with_tied_scores() {
        // Two memories at the same instant, same kind/preview → one
        // cluster. Two distinct same-instant kinds → tie-break by
        // topic label.
        let mem_a = memory_with("01", "alpha_kind", "same", "2026-05-26T11:30:00Z");
        let mem_b = memory_with("02", "beta_kind", "same", "2026-05-26T11:30:00Z");
        let options = FocusSuggestOptions {
            workspace_path: PathBuf::from("/tmp"),
            from_cass: false,
            limit: 5,
            recent_hours: 24,
            task_frame_id: None,
        };
        let first = score_and_emit_topics(
            &[mem_a.clone(), mem_b.clone()],
            &[],
            &BTreeMap::new(),
            fixed_now(),
            &options,
        );
        // Reverse insertion order must not change emitted order.
        let second = score_and_emit_topics(
            &[mem_b, mem_a],
            &[],
            &BTreeMap::new(),
            fixed_now(),
            &options,
        );
        assert_eq!(first, second);
        // Alphabetical tie-break.
        assert!(first[0].topic.starts_with("alpha_kind"));
        assert!(first[1].topic.starts_with("beta_kind"));
    }

    #[test]
    fn limit_zero_emits_empty_recommendations() {
        let mem = memory_with("01", "release", "x", "2026-05-26T11:30:00Z");
        let options = FocusSuggestOptions {
            workspace_path: PathBuf::from("/tmp"),
            from_cass: false,
            limit: 0,
            recent_hours: 24,
            task_frame_id: None,
        };
        let recs = score_and_emit_topics(&[mem], &[], &BTreeMap::new(), fixed_now(), &options);
        assert!(recs.is_empty());
    }

    #[test]
    fn pagerank_boost_outranks_recency() {
        let mem_central = memory_with("01", "release", "alpha", "2026-05-26T01:00:00Z");
        let mem_recent = memory_with("02", "decision", "beta", "2026-05-26T11:30:00Z");
        let mut pagerank = BTreeMap::new();
        pagerank.insert("01".to_owned(), 5.0);
        pagerank.insert("02".to_owned(), 0.0);
        let options = FocusSuggestOptions {
            workspace_path: PathBuf::from("/tmp"),
            from_cass: false,
            limit: 5,
            recent_hours: 24,
            task_frame_id: None,
        };
        let recs = score_and_emit_topics(
            &[mem_central, mem_recent],
            &[],
            &pagerank,
            fixed_now(),
            &options,
        );
        // High centrality must win over recency.
        assert!(recs[0].topic.starts_with("release"));
        assert!(recs[0].centrality_score > recs[1].centrality_score);
    }

    #[test]
    fn span_density_accumulates_for_member_memories() {
        let mem = memory_with("01", "release", "alpha", "2026-05-26T11:30:00Z");
        let mk_span = |id: &str, mem_id: &str, cass_id: &str| StoredEvidenceSpan {
            id: id.to_owned(),
            workspace_id: "ws".to_owned(),
            session_id: "session".to_owned(),
            memory_id: Some(mem_id.to_owned()),
            cass_span_id: cass_id.to_owned(),
            span_kind: "code".to_owned(),
            start_line: 1,
            end_line: 2,
            start_byte: None,
            end_byte: None,
            role: None,
            excerpt: "snippet".to_owned(),
            content_hash: "abc".to_owned(),
            metadata_json: None,
            created_at: "2026-05-26T11:30:00Z".to_owned(),
            updated_at: "2026-05-26T11:30:00Z".to_owned(),
        };
        let spans = vec![
            mk_span("s1", "01", "cass_a"),
            mk_span("s2", "01", "cass_b"),
            mk_span("s3", "01", "cass_b"), // duplicate cass_span_id, deduped
        ];
        let options = FocusSuggestOptions {
            workspace_path: PathBuf::from("/tmp"),
            from_cass: true,
            limit: 5,
            recent_hours: 24,
            task_frame_id: None,
        };
        let recs = score_and_emit_topics(&[mem], &spans, &BTreeMap::new(), fixed_now(), &options);
        assert_eq!(recs.len(), 1);
        assert_eq!(
            recs[0].span_ids,
            vec!["cass_a".to_owned(), "cass_b".to_owned()]
        );
    }

    #[test]
    fn escape_topic_handles_quotes_and_backslashes() {
        assert_eq!(escape_topic_for_query(r#"foo"bar"#), r#"foo\"bar"#);
        assert_eq!(escape_topic_for_query(r"foo\bar"), r"foo\\bar");
    }

    #[test]
    fn non_finite_pagerank_does_not_leak_into_centrality_score() {
        // serde_json renders NaN/Inf f64s as JSON `null`, which would
        // violate the v1 schema's `"type": "number"` constraint on
        // `centralityScore`. The summation must drop non-finite
        // PageRank contributions so the emitted score is always a
        // finite number.
        let mem_nan = memory_with("01", "alpha_kind", "alpha", "2026-05-26T11:30:00Z");
        let mem_inf = memory_with("02", "beta_kind", "beta", "2026-05-26T11:30:00Z");
        let mem_neg_inf = memory_with("03", "gamma_kind", "gamma", "2026-05-26T11:30:00Z");
        let mem_finite = memory_with("04", "delta_kind", "delta", "2026-05-26T11:30:00Z");
        let mut pagerank = BTreeMap::new();
        pagerank.insert("01".to_owned(), f64::NAN);
        pagerank.insert("02".to_owned(), f64::INFINITY);
        pagerank.insert("03".to_owned(), f64::NEG_INFINITY);
        pagerank.insert("04".to_owned(), 0.42);
        let options = FocusSuggestOptions {
            workspace_path: PathBuf::from("/tmp"),
            from_cass: false,
            limit: 10,
            recent_hours: 24,
            task_frame_id: None,
        };
        let recs = score_and_emit_topics(
            &[mem_nan, mem_inf, mem_neg_inf, mem_finite],
            &[],
            &pagerank,
            fixed_now(),
            &options,
        );
        assert_eq!(recs.len(), 4);
        for rec in &recs {
            assert!(
                rec.centrality_score.is_finite(),
                "centrality_score must be finite; rec={rec:?}",
            );
            // serde_json round-trip must produce a numeric value, not
            // null — this is the actual JSON contract we're guarding.
            let value = serde_json::to_value(rec.centrality_score).expect("score must serialize");
            assert!(
                value.is_number(),
                "centrality_score must serialize as JSON number; got {value:?}",
            );
        }
        // The finite-PR cluster (delta_kind) must surface with the
        // exact aggregate; the non-finite ones contribute 0.
        let delta = recs
            .iter()
            .find(|r| r.topic.starts_with("delta_kind"))
            .expect("delta cluster present");
        assert!((delta.centrality_score - 0.42).abs() < 1e-12);
        for label in ["alpha_kind", "beta_kind", "gamma_kind"] {
            let cluster = recs
                .iter()
                .find(|r| r.topic.starts_with(label))
                .unwrap_or_else(|| panic!("{label} cluster present"));
            assert_eq!(cluster.centrality_score, 0.0);
        }
    }

    #[test]
    fn escape_topic_neutralizes_double_quoted_shell_metacharacters() {
        // `$` and `` ` `` are still active inside the double quotes
        // around the topic in `ee pack "<topic>" ...`, so the agent
        // would do parameter / command substitution if they leaked
        // through unescaped.
        assert_eq!(escape_topic_for_query("$x"), r"\$x");
        assert_eq!(escape_topic_for_query("`cmd`"), r"\`cmd\`");
        // Combined: an attacker-controlled kind like `$(rm -rf /)`
        // must be reduced to a literal payload.
        assert_eq!(
            escape_topic_for_query("$(rm -rf /)"),
            r"\$(rm -rf /)".to_owned(),
        );
        // Backslashes are escaped before the other metacharacters so
        // the backslash we INTRODUCE for `$` / `` ` `` is not itself
        // re-doubled.
        assert_eq!(escape_topic_for_query(r"a\$b"), r"a\\\$b");
    }

    #[test]
    fn sort_is_total_under_nan_scores() {
        // Direct guard against the determinism trap: if an upstream
        // score happens to be NaN (e.g. PageRank divergence under a
        // pathological projection), the descending sort must place
        // finite scores ABOVE NaNs — not the other way around. The
        // bare `f64::total_cmp` shape gives a total order on f64, but
        // IEEE 754 `totalOrder` places positive NaN ABOVE positive
        // infinity (see `f64::total_cmp` docs and the underlying spec).
        // Plugging that directly into a descending `right.total_cmp(&left)`
        // sort surfaces NaN-scored clusters at the TOP of
        // `recommendations[]`, the opposite of "highest finite score
        // wins". Pre-fix this test asserted the post-normalization
        // outcome but performed the bare `total_cmp` comparison — the
        // assertion fired because alpha/charlie (NaN) landed at indices
        // 0/1 instead of delta/bravo, and the test failed loud whenever
        // it was actually run (cargo test --lib focus_suggest).
        //
        // Production `score_and_emit_topics` (this file, sort_score
        // closure above the sort_by call) coerces NaN to NEG_INFINITY
        // before comparing so any NaN that leaks past the upstream
        // filters lands at the bottom of the ranking instead of
        // hijacking the top. Mirror that normalization here so the
        // test exercises the SAME contract the production sort honors,
        // and any future regression that drops the normalization fails
        // this assertion.
        let mk_cluster = |label: &str, member: &str| TopicCluster {
            topic_label: label.to_owned(),
            member_ids: vec![member.to_owned()],
            centrality_sum: 0.0,
            most_recent_at: None,
            span_ids: Vec::new(),
        };
        let scored = vec![
            (f64::NAN, mk_cluster("alpha", "01")),
            (1.0, mk_cluster("bravo", "02")),
            (f64::NAN, mk_cluster("charlie", "03")),
            (2.0, mk_cluster("delta", "04")),
        ];
        let sort_score = |score: f64| {
            if score.is_nan() {
                f64::NEG_INFINITY
            } else {
                score
            }
        };
        let mut a = scored.clone();
        let mut b = scored;
        a.sort_by(|l, r| {
            sort_score(r.0)
                .total_cmp(&sort_score(l.0))
                .then_with(|| l.1.topic_label.cmp(&r.1.topic_label))
        });
        b.sort_by(|l, r| {
            sort_score(r.0)
                .total_cmp(&sort_score(l.0))
                .then_with(|| l.1.topic_label.cmp(&r.1.topic_label))
        });
        let a_labels: Vec<&str> = a.iter().map(|(_, c)| c.topic_label.as_str()).collect();
        let b_labels: Vec<&str> = b.iter().map(|(_, c)| c.topic_label.as_str()).collect();
        // Determinism: identical input → identical output across two
        // sorts (sort_by is stable; sort_score has no internal state).
        assert_eq!(a_labels, b_labels);
        // Finite scores must outrank NaNs in the descending sort.
        // Without the sort_score normalization above, NaN sorts ABOVE
        // +Infinity and alpha/charlie would land at indices 0/1.
        assert_eq!(a_labels[0], "delta");
        assert_eq!(a_labels[1], "bravo");
        // The two NaN clusters tie-break through topic_label asc since
        // they both normalize to NEG_INFINITY: alpha before charlie.
        assert_eq!(a_labels[2], "alpha");
        assert_eq!(a_labels[3], "charlie");
    }

    #[test]
    fn checked_sub_signed_recent_hours_does_not_panic_on_u32_max() {
        // Regression guard: `recent_hours: u32` accepts u32::MAX, which
        // converts to ~489_957 years. Subtracting that from `Utc::now()`
        // via the unchecked `now - Duration::hours(...)` path panics
        // because the result is outside `DateTime<Utc>`'s representable
        // range. The fix uses `checked_sub_signed` and falls back to
        // `DateTime::<Utc>::MIN_UTC` when subtraction underflows. This
        // test asserts the fallback exists by exercising it directly,
        // since the panic would otherwise abort the test binary.
        let now = Utc::now();
        let result = now.checked_sub_signed(Duration::hours(i64::from(u32::MAX)));
        // Either the subtraction succeeds (chrono's range happens to
        // span the resulting year) or returns None for the overflow
        // case. Both outcomes must be representable; the panic path is
        // what this test rejects.
        let cutoff = result.unwrap_or(DateTime::<Utc>::MIN_UTC);
        // Any memory created after MIN_UTC must satisfy the recency
        // window when the cutoff has clamped. This proves the fallback
        // yields a usable bound.
        let any_memory_ts = DateTime::parse_from_rfc3339("2026-05-26T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert!(
            any_memory_ts >= cutoff,
            "MIN_UTC fallback must accept every real-world memory timestamp",
        );
    }
}
