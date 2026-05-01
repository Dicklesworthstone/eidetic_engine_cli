//! Memory selection explanation (EE-150).
//!
//! Provides the `ee why <memory-id>` command which explains:
//! - How a memory was stored (provenance, trust class)
//! - How it would be retrieved (scoring factors)
//! - How it would be selected for packs (relevance, utility, importance)
//! - Related memory links (supports, contradicts, derived_from, etc.)
//!
//! This makes the system explainable and auditable.

use std::path::Path;

use crate::db::DbConnection;
use sqlmodel_core::{Row, Value};

/// Why a memory was stored with certain characteristics.
#[derive(Clone, Debug, PartialEq)]
pub struct StorageExplanation {
    /// How the memory was created (import, remember, curate).
    pub origin: String,
    /// Trust class assigned at creation.
    pub trust_class: String,
    /// Trust subclass if applicable.
    pub trust_subclass: Option<String>,
    /// Original provenance URI.
    pub provenance_uri: Option<String>,
    /// When the memory was created.
    pub created_at: String,
}

/// Why a memory would be retrieved by search.
#[derive(Clone, Debug, PartialEq)]
pub struct RetrievalExplanation {
    /// Base confidence score (0.0-1.0).
    pub confidence: f32,
    /// Utility score for retrieval ranking.
    pub utility: f32,
    /// Importance score for priority.
    pub importance: f32,
    /// Tags that improve retrieval.
    pub tags: Vec<String>,
    /// Memory level (procedural, episodic, semantic).
    pub level: String,
    /// Memory kind (rule, decision, failure, etc.).
    pub kind: String,
}

/// Why a memory would be selected for a context pack.
#[derive(Clone, Debug, PartialEq)]
pub struct SelectionExplanation {
    /// Combined selection score.
    pub selection_score: f32,
    /// Whether this memory would pass the confidence threshold.
    pub above_confidence_threshold: bool,
    /// Whether the memory is active (not tombstoned).
    pub is_active: bool,
    /// Explanation of how scores combine.
    pub score_breakdown: String,
    /// Most recent persisted context-pack selection for this memory, if any.
    pub latest_pack_selection: Option<PackSelectionExplanation>,
}

/// A persisted context-pack selection involving the memory.
#[derive(Clone, Debug, PartialEq)]
pub struct PackSelectionExplanation {
    /// Pack record ID.
    pub pack_id: String,
    /// Query that produced the pack.
    pub query: String,
    /// Pack profile.
    pub profile: String,
    /// One-based rank inside the pack.
    pub rank: u32,
    /// Context pack section.
    pub section: String,
    /// Estimated token cost recorded for the item.
    pub estimated_tokens: u32,
    /// Relevance score recorded when the pack was assembled.
    pub relevance: f32,
    /// Utility score recorded when the pack was assembled.
    pub utility: f32,
    /// Pack item's persisted why text.
    pub why: String,
    /// Persisted pack hash.
    pub pack_hash: String,
    /// Pack creation timestamp.
    pub selected_at: String,
}

/// Contradiction feedback recorded against this memory (EE-263).
#[derive(Clone, Debug, PartialEq)]
pub struct ContradictionMetadata {
    /// Feedback event ID.
    pub event_id: String,
    /// Weight of the contradiction signal.
    pub weight: f32,
    /// Source type (agent_inference, human_request, etc.).
    pub source_type: String,
    /// Reason for the contradiction.
    pub reason: Option<String>,
    /// When the contradiction was recorded.
    pub created_at: String,
    /// Whether the contradiction has been applied to scores.
    pub applied: bool,
}

/// Summary of a memory link for why output (EE-LINK-USAGE-001).
#[derive(Clone, Debug, PartialEq)]
pub struct MemoryLinkSummary {
    /// Link ID.
    pub link_id: String,
    /// The related memory ID (the "other" side of the link).
    pub linked_memory_id: String,
    /// Relation type (supports, contradicts, derived_from, etc.).
    pub relation: String,
    /// Direction relative to the queried memory: "outgoing" (this memory -> linked),
    /// "incoming" (linked -> this memory), or "undirected".
    pub direction: String,
    /// Confidence score for this link (0.0-1.0).
    pub confidence: f32,
    /// Weight of the link edge.
    pub weight: f32,
    /// Number of evidence instances supporting this link.
    pub evidence_count: u32,
    /// Source that created the link (agent, auto, import, human).
    pub source: String,
    /// When the link was created.
    pub created_at: String,
}

/// Non-fatal limitations in the why explanation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WhyDegradation {
    /// Stable degradation code.
    pub code: &'static str,
    /// Stable severity.
    pub severity: &'static str,
    /// Human-readable message.
    pub message: String,
    /// Suggested repair command when available.
    pub repair: Option<String>,
}

/// Complete why report for a memory.
#[derive(Clone, Debug)]
pub struct WhyReport {
    /// Package version for stable output.
    pub version: &'static str,
    /// Memory ID that was queried.
    pub memory_id: String,
    /// Whether the memory was found.
    pub found: bool,
    /// Storage explanation.
    pub storage: Option<StorageExplanation>,
    /// Retrieval explanation.
    pub retrieval: Option<RetrievalExplanation>,
    /// Selection explanation.
    pub selection: Option<SelectionExplanation>,
    /// Contradiction feedback recorded against this memory (EE-263).
    pub contradictions: Vec<ContradictionMetadata>,
    /// Memory links: supports, contradicts, derived_from, etc. (EE-LINK-USAGE-001).
    pub links: Vec<MemoryLinkSummary>,
    /// Non-fatal degradation notices.
    pub degraded: Vec<WhyDegradation>,
    /// Error message if query failed.
    pub error: Option<String>,
}

impl WhyReport {
    /// Create a report for a found memory.
    #[must_use]
    pub fn found(
        memory_id: String,
        storage: StorageExplanation,
        retrieval: RetrievalExplanation,
        selection: SelectionExplanation,
    ) -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION"),
            memory_id,
            found: true,
            storage: Some(storage),
            retrieval: Some(retrieval),
            selection: Some(selection),
            contradictions: Vec::new(),
            links: Vec::new(),
            degraded: Vec::new(),
            error: None,
        }
    }

    /// Create a report for a not-found memory.
    #[must_use]
    pub fn not_found(memory_id: String) -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION"),
            memory_id,
            found: false,
            storage: None,
            retrieval: None,
            selection: None,
            contradictions: Vec::new(),
            links: Vec::new(),
            degraded: Vec::new(),
            error: None,
        }
    }

    /// Create a report for an error condition.
    #[must_use]
    pub fn error(memory_id: String, message: String) -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION"),
            memory_id,
            found: false,
            storage: None,
            retrieval: None,
            selection: None,
            contradictions: Vec::new(),
            links: Vec::new(),
            degraded: Vec::new(),
            error: Some(message),
        }
    }

    /// Add a non-fatal degradation notice to the report.
    #[must_use]
    pub fn with_degradation(mut self, degraded: WhyDegradation) -> Self {
        self.degraded.push(degraded);
        self
    }

    /// Add contradiction metadata to the report (EE-263).
    #[must_use]
    pub fn with_contradictions(mut self, contradictions: Vec<ContradictionMetadata>) -> Self {
        self.contradictions = contradictions;
        self
    }

    /// Add memory link summaries to the report (EE-LINK-USAGE-001).
    #[must_use]
    pub fn with_links(mut self, links: Vec<MemoryLinkSummary>) -> Self {
        self.links = links;
        self
    }
}

/// Options for the why query.
#[derive(Clone, Debug)]
pub struct WhyOptions<'a> {
    /// Database path.
    pub database_path: &'a Path,
    /// Memory ID to explain.
    pub memory_id: &'a str,
    /// Confidence threshold for selection (default 0.5).
    pub confidence_threshold: f32,
}

impl<'a> WhyOptions<'a> {
    /// Default confidence threshold for pack selection.
    pub const DEFAULT_CONFIDENCE_THRESHOLD: f32 = 0.5;
}

/// Get a why explanation for a memory.
///
/// Explains why a memory was stored, how it would be retrieved,
/// and how it would be selected for context packs.
pub fn explain_memory(options: &WhyOptions<'_>) -> WhyReport {
    let conn = match DbConnection::open_file(options.database_path) {
        Ok(c) => c,
        Err(e) => {
            return WhyReport::error(
                options.memory_id.to_string(),
                format!("Failed to open database: {e}"),
            );
        }
    };

    let memory = match conn.get_memory(options.memory_id) {
        Ok(Some(m)) => m,
        Ok(None) => return WhyReport::not_found(options.memory_id.to_string()),
        Err(e) => {
            return WhyReport::error(
                options.memory_id.to_string(),
                format!("Failed to query memory: {e}"),
            );
        }
    };

    let tags = match conn.get_memory_tags(options.memory_id) {
        Ok(t) => t,
        Err(e) => {
            return WhyReport::error(
                options.memory_id.to_string(),
                format!("Failed to query tags: {e}"),
            );
        }
    };

    // Fetch contradiction feedback events (EE-263)
    let contradictions = fetch_contradictions(&conn, options.memory_id);

    // Fetch memory links (EE-LINK-USAGE-001)
    let links = fetch_links(&conn, options.memory_id);

    let storage = StorageExplanation {
        origin: determine_origin(&memory.trust_class),
        trust_class: memory.trust_class.clone(),
        trust_subclass: memory.trust_subclass.clone(),
        provenance_uri: memory.provenance_uri.clone(),
        created_at: memory.created_at.clone(),
    };

    let retrieval = RetrievalExplanation {
        confidence: memory.confidence,
        utility: memory.utility,
        importance: memory.importance,
        tags,
        level: memory.level.clone(),
        kind: memory.kind.clone(),
    };

    let is_active = memory.tombstoned_at.is_none();
    let selection_score =
        compute_selection_score(memory.confidence, memory.utility, memory.importance);
    let above_threshold = memory.confidence >= options.confidence_threshold;

    let latest_pack_selection = match latest_pack_selection(&conn, options.memory_id) {
        Ok(selection) => selection,
        Err(message) => {
            let report = build_report(
                options,
                storage,
                retrieval,
                ReportSelectionInputs {
                    is_active,
                    selection_score,
                    above_threshold,
                    latest_pack_selection: None,
                    contradictions,
                    links,
                },
            );
            return report.with_degradation(WhyDegradation {
                code: "why_pack_selection_unavailable",
                severity: "low",
                message,
                repair: Some("ee db migrate".to_string()),
            });
        }
    };

    build_report(
        options,
        storage,
        retrieval,
        ReportSelectionInputs {
            is_active,
            selection_score,
            above_threshold,
            latest_pack_selection,
            contradictions,
            links,
        },
    )
}

struct ReportSelectionInputs {
    is_active: bool,
    selection_score: f32,
    above_threshold: bool,
    latest_pack_selection: Option<PackSelectionExplanation>,
    contradictions: Vec<ContradictionMetadata>,
    links: Vec<MemoryLinkSummary>,
}

fn build_report(
    options: &WhyOptions<'_>,
    storage: StorageExplanation,
    retrieval: RetrievalExplanation,
    selection_inputs: ReportSelectionInputs,
) -> WhyReport {
    let selection = SelectionExplanation {
        selection_score: selection_inputs.selection_score,
        above_confidence_threshold: selection_inputs.above_threshold,
        is_active: selection_inputs.is_active,
        score_breakdown: format!(
            "selection_score = 0.5 * confidence({:.2}) + 0.3 * utility({:.2}) + 0.2 * importance({:.2}) = {:.2}",
            retrieval.confidence,
            retrieval.utility,
            retrieval.importance,
            selection_inputs.selection_score
        ),
        latest_pack_selection: selection_inputs.latest_pack_selection,
    };

    WhyReport::found(options.memory_id.to_string(), storage, retrieval, selection)
        .with_contradictions(selection_inputs.contradictions)
        .with_links(selection_inputs.links)
}

fn determine_origin(trust_class: &str) -> String {
    match trust_class {
        "human_explicit" => "Explicitly remembered via `ee remember`".to_string(),
        "agent_validated" => "Agent assertion with validated outcome evidence".to_string(),
        "agent_assertion" => "Agent assertion awaiting validation".to_string(),
        "cass_evidence" => "Imported from CASS session evidence".to_string(),
        "legacy_import" => "Imported from a legacy Eidetic Engine store".to_string(),
        _ => format!("Created with trust class: {trust_class}"),
    }
}

fn compute_selection_score(confidence: f32, utility: f32, importance: f32) -> f32 {
    0.5 * confidence + 0.3 * utility + 0.2 * importance
}

fn latest_pack_selection(
    conn: &DbConnection,
    memory_id: &str,
) -> Result<Option<PackSelectionExplanation>, String> {
    let rows = conn
        .query(
            "SELECT pi.pack_id, pr.query, pr.profile, pi.rank, pi.section, pi.estimated_tokens, pi.relevance, pi.utility, pi.why, pr.pack_hash, pr.created_at \
             FROM pack_items pi \
             JOIN pack_records pr ON pr.id = pi.pack_id \
             WHERE pi.memory_id = ?1 \
             ORDER BY pr.created_at DESC, pi.pack_id DESC, pi.rank ASC \
             LIMIT 1",
            &[Value::Text(memory_id.to_string())],
        )
        .map_err(|error| format!("Failed to query pack selection: {error}"))?;

    rows.first().map(pack_selection_from_row).transpose()
}

fn pack_selection_from_row(row: &Row) -> Result<PackSelectionExplanation, String> {
    Ok(PackSelectionExplanation {
        pack_id: required_text(row, 0, "pack_id")?,
        query: required_text(row, 1, "query")?,
        profile: required_text(row, 2, "profile")?,
        rank: required_u32(row, 3, "rank")?,
        section: required_text(row, 4, "section")?,
        estimated_tokens: required_u32(row, 5, "estimated_tokens")?,
        relevance: required_f32(row, 6, "relevance")?,
        utility: required_f32(row, 7, "utility")?,
        why: required_text(row, 8, "why")?,
        pack_hash: required_text(row, 9, "pack_hash")?,
        selected_at: required_text(row, 10, "created_at")?,
    })
}

fn required_text(row: &Row, index: usize, column: &str) -> Result<String, String> {
    row.get(index)
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .ok_or_else(|| format!("Pack selection column {column} was missing or not text"))
}

fn required_u32(row: &Row, index: usize, column: &str) -> Result<u32, String> {
    let raw = row
        .get(index)
        .and_then(|value| value.as_i64())
        .ok_or_else(|| format!("Pack selection column {column} was missing or not integer"))?;
    u32::try_from(raw)
        .map_err(|_| format!("Pack selection column {column} was out of range: {raw}"))
}

fn required_f32(row: &Row, index: usize, column: &str) -> Result<f32, String> {
    row.get(index)
        .and_then(|value| value.as_f64())
        .map(|value| value as f32)
        .ok_or_else(|| format!("Pack selection column {column} was missing or not numeric"))
}

/// Fetch contradiction feedback events for a memory (EE-263).
fn fetch_contradictions(conn: &DbConnection, memory_id: &str) -> Vec<ContradictionMetadata> {
    let events = match conn.list_feedback_events_for_target("memory", memory_id) {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };

    events
        .into_iter()
        .filter(|e| e.signal == "contradiction")
        .map(|e| ContradictionMetadata {
            event_id: e.id,
            weight: e.weight,
            source_type: e.source_type,
            reason: e.reason,
            created_at: e.created_at,
            applied: e.applied_at.is_some(),
        })
        .collect()
}

/// Fetch memory links for a memory (EE-LINK-USAGE-001).
fn fetch_links(conn: &DbConnection, memory_id: &str) -> Vec<MemoryLinkSummary> {
    let stored_links = match conn.list_memory_links_for_memory(memory_id, None) {
        Ok(links) => links,
        Err(_) => return Vec::new(),
    };

    stored_links
        .into_iter()
        .map(|link| {
            let direction = if !link.directed {
                "undirected".to_string()
            } else if link.src_memory_id == memory_id {
                "outgoing".to_string()
            } else {
                "incoming".to_string()
            };

            let linked_memory_id = if link.src_memory_id == memory_id {
                link.dst_memory_id.clone()
            } else {
                link.src_memory_id.clone()
            };

            MemoryLinkSummary {
                link_id: link.id,
                linked_memory_id,
                relation: link.relation,
                direction,
                confidence: link.confidence,
                weight: link.weight,
                evidence_count: link.evidence_count,
                source: link.source,
                created_at: link.created_at,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), String>;

    fn ensure<T: std::fmt::Debug + PartialEq>(actual: T, expected: T, ctx: &str) -> TestResult {
        if actual == expected {
            Ok(())
        } else {
            Err(format!("{ctx}: expected {expected:?}, got {actual:?}"))
        }
    }

    #[test]
    fn why_report_not_found_is_correct() -> TestResult {
        let report = WhyReport::not_found("mem_test".to_string());

        ensure(report.found, false, "found")?;
        ensure(report.memory_id, "mem_test".to_string(), "memory_id")?;
        ensure(report.storage.is_none(), true, "storage is none")?;
        ensure(report.error.is_none(), true, "no error")
    }

    #[test]
    fn why_report_error_captures_message() -> TestResult {
        let report = WhyReport::error("mem_test".to_string(), "db error".to_string());

        ensure(report.found, false, "found")?;
        ensure(report.error, Some("db error".to_string()), "error message")
    }

    #[test]
    fn why_report_version_matches_package() -> TestResult {
        let report = WhyReport::not_found("mem_test".to_string());
        ensure(report.version, env!("CARGO_PKG_VERSION"), "version")
    }

    #[test]
    fn selection_score_computation() -> TestResult {
        let score = compute_selection_score(0.8, 0.6, 0.7);
        let expected = 0.5 * 0.8 + 0.3 * 0.6 + 0.2 * 0.7;
        ensure((score - expected).abs() < 0.001, true, "score computation")
    }

    #[test]
    fn determine_origin_for_explicit_memory() -> TestResult {
        let origin = determine_origin("human_explicit");
        ensure(
            origin.contains("ee remember"),
            true,
            "human_explicit origin mentions ee remember",
        )
    }

    #[test]
    fn determine_origin_for_cass_import() -> TestResult {
        let origin = determine_origin("cass_evidence");
        ensure(
            origin.contains("CASS"),
            true,
            "cass_evidence origin mentions CASS",
        )
    }

    #[test]
    fn found_report_can_carry_pack_selection() -> TestResult {
        let selection = PackSelectionExplanation {
            pack_id: "pack_00000000000000000000000001".to_string(),
            query: "prepare release".to_string(),
            profile: "compact".to_string(),
            rank: 1,
            section: "procedural_rules".to_string(),
            estimated_tokens: 8,
            relevance: 0.91,
            utility: 0.8,
            why: "selected because it matches the release task".to_string(),
            pack_hash: "hash".to_string(),
            selected_at: "2026-04-29T12:00:00Z".to_string(),
        };
        let report = build_report(
            &WhyOptions {
                database_path: Path::new("/tmp/unused.db"),
                memory_id: "mem_00000000000000000000000001",
                confidence_threshold: WhyOptions::DEFAULT_CONFIDENCE_THRESHOLD,
            },
            StorageExplanation {
                origin: "Explicitly remembered via `ee remember`".to_string(),
                trust_class: "human_explicit".to_string(),
                trust_subclass: None,
                provenance_uri: None,
                created_at: "2026-04-29T12:00:00Z".to_string(),
            },
            RetrievalExplanation {
                confidence: 0.9,
                utility: 0.8,
                importance: 0.7,
                tags: Vec::new(),
                level: "procedural".to_string(),
                kind: "rule".to_string(),
            },
            ReportSelectionInputs {
                is_active: true,
                selection_score: 0.83,
                above_threshold: true,
                latest_pack_selection: Some(selection),
                contradictions: Vec::new(),
                links: Vec::new(),
            },
        );

        ensure(report.found, true, "found")?;
        ensure(
            report
                .selection
                .and_then(|selection| selection.latest_pack_selection)
                .map(|selection| selection.rank),
            Some(1),
            "pack rank",
        )
    }

    #[test]
    fn memory_link_summary_direction_outgoing() -> TestResult {
        let link = MemoryLinkSummary {
            link_id: "link_test".to_string(),
            linked_memory_id: "mem_other".to_string(),
            relation: "supports".to_string(),
            direction: "outgoing".to_string(),
            confidence: 0.9,
            weight: 1.0,
            evidence_count: 3,
            source: "agent".to_string(),
            created_at: "2026-04-30T12:00:00Z".to_string(),
        };
        ensure(link.direction, "outgoing".to_string(), "direction")?;
        ensure(link.relation, "supports".to_string(), "relation")
    }

    #[test]
    fn memory_link_summary_direction_incoming() -> TestResult {
        let link = MemoryLinkSummary {
            link_id: "link_test".to_string(),
            linked_memory_id: "mem_source".to_string(),
            relation: "contradicts".to_string(),
            direction: "incoming".to_string(),
            confidence: 0.85,
            weight: 0.8,
            evidence_count: 1,
            source: "human".to_string(),
            created_at: "2026-04-30T12:00:00Z".to_string(),
        };
        ensure(link.direction, "incoming".to_string(), "direction")?;
        ensure(link.relation, "contradicts".to_string(), "relation")
    }

    #[test]
    fn memory_link_summary_undirected() -> TestResult {
        let link = MemoryLinkSummary {
            link_id: "link_test".to_string(),
            linked_memory_id: "mem_related".to_string(),
            relation: "related".to_string(),
            direction: "undirected".to_string(),
            confidence: 0.7,
            weight: 0.5,
            evidence_count: 2,
            source: "auto".to_string(),
            created_at: "2026-04-30T12:00:00Z".to_string(),
        };
        ensure(link.direction, "undirected".to_string(), "direction")?;
        ensure(link.relation, "related".to_string(), "relation")
    }

    #[test]
    fn why_report_with_links() -> TestResult {
        let links = vec![
            MemoryLinkSummary {
                link_id: "link_01".to_string(),
                linked_memory_id: "mem_support".to_string(),
                relation: "supports".to_string(),
                direction: "outgoing".to_string(),
                confidence: 0.9,
                weight: 1.0,
                evidence_count: 2,
                source: "agent".to_string(),
                created_at: "2026-04-30T12:00:00Z".to_string(),
            },
            MemoryLinkSummary {
                link_id: "link_02".to_string(),
                linked_memory_id: "mem_contradict".to_string(),
                relation: "contradicts".to_string(),
                direction: "incoming".to_string(),
                confidence: 0.8,
                weight: 0.5,
                evidence_count: 1,
                source: "human".to_string(),
                created_at: "2026-04-30T12:01:00Z".to_string(),
            },
        ];

        let report = WhyReport::not_found("mem_test".to_string()).with_links(links);

        ensure(report.links.len(), 2, "link count")?;
        ensure(
            report.links[0].relation.clone(),
            "supports".to_string(),
            "first link relation",
        )?;
        ensure(
            report.links[1].relation.clone(),
            "contradicts".to_string(),
            "second link relation",
        )
    }

    #[test]
    fn why_report_links_default_empty() -> TestResult {
        let report = WhyReport::not_found("mem_test".to_string());
        ensure(
            report.links.is_empty(),
            true,
            "links should be empty by default",
        )
    }

    #[test]
    fn all_link_relation_types_supported() -> TestResult {
        let relations = [
            "supports",
            "contradicts",
            "derived_from",
            "supersedes",
            "related",
            "co_tag",
            "co_mention",
        ];

        for relation in &relations {
            let link = MemoryLinkSummary {
                link_id: format!("link_{relation}"),
                linked_memory_id: "mem_other".to_string(),
                relation: relation.to_string(),
                direction: "outgoing".to_string(),
                confidence: 0.9,
                weight: 1.0,
                evidence_count: 1,
                source: "agent".to_string(),
                created_at: "2026-04-30T12:00:00Z".to_string(),
            };
            ensure(link.relation, relation.to_string(), "relation type")?;
        }
        Ok(())
    }

    #[test]
    fn all_link_sources_supported() -> TestResult {
        let sources = ["agent", "auto", "import", "maintenance", "human"];

        for source in &sources {
            let link = MemoryLinkSummary {
                link_id: format!("link_{source}"),
                linked_memory_id: "mem_other".to_string(),
                relation: "supports".to_string(),
                direction: "outgoing".to_string(),
                confidence: 0.9,
                weight: 1.0,
                evidence_count: 1,
                source: source.to_string(),
                created_at: "2026-04-30T12:00:00Z".to_string(),
            };
            ensure(link.source, source.to_string(), "source type")?;
        }
        Ok(())
    }
}
