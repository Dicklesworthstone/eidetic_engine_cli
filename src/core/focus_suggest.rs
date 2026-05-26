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
    let cutoff = now - Duration::hours(i64::from(options.recent_hours));
    let recent: Vec<StoredMemory> = memories
        .into_iter()
        .filter(|memory| memory_created_at(memory).is_some_and(|ts| ts >= cutoff))
        .collect();

    let scoped = match options.task_frame_id.as_deref() {
        Some(frame_id) => match load_task_frame_evidence(&options.workspace_path, frame_id) {
            Ok(evidence_ids) if !evidence_ids.is_empty() => recent
                .into_iter()
                .filter(|memory| evidence_ids.iter().any(|id| id == &memory.id))
                .collect(),
            Ok(_) => {
                degraded.push(FocusSuggestDegradation {
                    code: "task_frame_no_evidence".to_owned(),
                    severity: "info".to_owned(),
                    message: format!(
                        "Task frame {frame_id} has no evidence_links; no neighborhood restriction applied."
                    ),
                    repair: None,
                });
                recent
            }
            Err(error) => {
                degraded.push(FocusSuggestDegradation {
                    code: "task_frame_unavailable".to_owned(),
                    severity: "warning".to_owned(),
                    message: format!("Failed to load task frame {frame_id}: {error}"),
                    repair: Some("Verify the frame id with `ee task-frame show --all`.".to_owned()),
                });
                recent
            }
        },
        None => recent,
    };

    if scoped.is_empty() {
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
                degraded.push(FocusSuggestDegradation {
                    code: "cass_unavailable".to_owned(),
                    severity: "warning".to_owned(),
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
    let mut ids: Vec<String> = report
        .frame
        .as_ref()
        .map(|frame| {
            frame
                .evidence_links
                .iter()
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
    degraded.push(FocusSuggestDegradation {
        code: "graph_unavailable".to_owned(),
        severity: "warning".to_owned(),
        message: "Graph feature disabled at build time; falling back to recency-only scoring."
            .to_owned(),
        repair: Some("Rebuild ee with --features graph to enable centrality scoring.".to_owned()),
    });
    BTreeMap::new()
}

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
            cluster.centrality_sum += *rank;
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
                    let age_hours = (now - ts).num_seconds() as f64 / 3600.0;
                    (-age_hours / recent_hours_f).exp()
                })
                .unwrap_or(0.0);
            #[allow(clippy::cast_precision_loss)]
            let span_density = (1.0 + cluster.span_ids.len() as f64).ln();
            let score = cluster.centrality_sum + recency_weight + span_density;
            (score, cluster)
        })
        .collect();

    scored.sort_by(|left, right| {
        right
            .0
            .partial_cmp(&left.0)
            .unwrap_or(std::cmp::Ordering::Equal)
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
        .map(|(score, cluster)| {
            let recency_str = cluster
                .most_recent_at
                .map(|ts| ts.to_rfc3339())
                .unwrap_or_else(|| "unknown".to_owned());
            let rationale = format!(
                "Centrality {:.4} aggregated over {} memory(ies); {} CASS span(s); most recent evidence at {}.",
                cluster.centrality_sum,
                cluster.member_ids.len(),
                cluster.span_ids.len(),
                recency_str,
            );
            let suggested_query = format!(
                "ee context \"{}\" --workspace . --max-tokens 4000 --json",
                escape_topic_for_query(&cluster.topic_label),
            );
            FocusRecommendation {
                topic: cluster.topic_label,
                span_ids: cluster.span_ids,
                centrality_score: score,
                rationale,
                suggested_query,
            }
        })
        .collect()
}

fn topic_key_for_memory(memory: &StoredMemory) -> String {
    let preview = content_preview_tokens(&memory.content, 32);
    format!("{}::{}", memory.kind, preview)
}

fn derive_topic_label(memory: &StoredMemory) -> String {
    let preview = content_preview_tokens(&memory.content, 48);
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
    topic.replace('\\', "\\\\").replace('"', "\\\"")
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

    #[test]
    fn recency_weight_decays_within_window() {
        // Two clusters, equal centrality, equal span density, different
        // age. Newer must outrank older.
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
        // First (highest score) must be the newer one.
        assert!(recs[0].topic.starts_with("release"));
        assert!(recs[0].centrality_score > recs[1].centrality_score);
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
}
