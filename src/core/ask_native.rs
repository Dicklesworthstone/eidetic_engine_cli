//! Native source identity and lineage for extractive answers.
//!
//! The span engine treats its historical `memory_id` slot as an opaque source
//! key. Native rules retain their RuleId there; this module projects the real
//! kind/revision at every public boundary instead of minting a MemoryId.

use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;

use crate::models::{EvidenceId, MemoryId, ProvenanceUri, RuleId};
use crate::pack::PackEntityRef;

use super::{AskCandidate, AskReport, AskRequest, AskSpan};

/// Source-of-truth metadata captured in the same read snapshot as the body.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AskNativeSource {
    pub entity: PackEntityRef,
    pub entity_revision: String,
    /// Provenance-only inputs. These are never substituted for the entity ID
    /// or serialized as independently supporting citations.
    pub source_memory_ids: Vec<String>,
}

pub(super) fn validate_sources(request: &AskRequest, candidates: &[AskCandidate]) -> bool {
    candidates.iter().all(|candidate| {
        let Some(source) = request.native_sources.get(&candidate.memory_id) else {
            return RuleId::from_str(&candidate.memory_id).is_err()
                && EvidenceId::from_str(&candidate.memory_id).is_err();
        };
        !matches!(source.entity, PackEntityRef::Memory(_))
            && source.entity.id_string() == candidate.memory_id
            && source
                .entity_revision
                .strip_prefix("blake3:")
                .is_some_and(|hash| {
                    hash.len() == 64
                        && hash
                            .bytes()
                            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
                })
            && source
                .source_memory_ids
                .iter()
                .all(|id| MemoryId::from_str(id).is_ok())
    })
}

pub(super) fn attach_sources(report: &mut AskReport, request: &AskRequest) {
    if report.extractiveness_violated {
        return;
    }
    let visible: BTreeSet<_> = report
        .citations
        .iter()
        .map(|item| item.memory_id.as_str())
        .chain(
            report
                .sides
                .iter()
                .flatten()
                .flat_map(|side| &side.citations)
                .map(|item| item.memory_id.as_str()),
        )
        .chain(
            report
                .nearest_evidence
                .iter()
                .flatten()
                .map(|item| item.memory_id.as_str()),
        )
        .collect();
    report.native_sources = request
        .native_sources
        .iter()
        .filter(|(id, _)| visible.contains(id.as_str()))
        .map(|(id, source)| (id.clone(), source.clone()))
        .collect();
}

pub(super) fn insert_identity(
    value: &mut serde_json::Value,
    id: &str,
    sources: &BTreeMap<String, AskNativeSource>,
    matched: bool,
) {
    let Some(source) = sources.get(id) else {
        return;
    };
    let Some(object) = value.as_object_mut() else {
        return;
    };
    let (old, kind, entity, revision, rule, evidence) = if matched {
        (
            "matchedMemoryId",
            "matchedEntityKind",
            "matchedEntityId",
            "matchedEntityRevision",
            "matchedRuleId",
            "matchedEvidenceId",
        )
    } else {
        (
            "memoryId",
            "entityKind",
            "entityId",
            "entityRevision",
            "ruleId",
            "evidenceId",
        )
    };
    object.remove(old);
    object.insert(kind.to_owned(), source.entity.kind_str().into());
    object.insert(entity.to_owned(), id.into());
    object.insert(revision.to_owned(), source.entity_revision.clone().into());
    match source.entity {
        PackEntityRef::Rule(_) => {
            object.insert(rule.to_owned(), id.into());
        }
        PackEntityRef::EvidenceSpan(_) => {
            object.insert(evidence.to_owned(), id.into());
        }
        PackEntityRef::Memory(_) => {}
    }
}

pub(super) fn audit_target(source: Option<&AskNativeSource>) -> &'static str {
    match source.map(|source| source.entity) {
        Some(PackEntityRef::Rule(_)) => "rule",
        Some(PackEntityRef::EvidenceSpan(_)) => "evidence",
        _ => "memory",
    }
}

pub(super) fn markdown_identity(report: &AskReport, id: &str) -> String {
    report
        .native_sources
        .get(id)
        .map_or_else(String::new, |source| {
            format!(
                " [{} `{}` @ `{}`]",
                source.entity.kind_str(),
                id,
                source.entity_revision
            )
        })
}

/// Rules and their derivation inputs are correlated, not independent votes.
/// Excerpts from one CASS session, file or web document are likewise one source
/// even when they have different entity IDs or line/fragment windows. Explicit
/// memory references join that same lineage rather than creating new votes.
/// Join before counting support, including a shared parent outside the corpus.
/// Root choice and path compression are deterministic.
pub(super) fn support_groups(
    spans: &[AskSpan],
    sources: &BTreeMap<String, AskNativeSource>,
) -> BTreeMap<String, String> {
    let ids: BTreeSet<_> = spans.iter().map(|span| span.memory_id.as_str()).collect();
    let mut parents: BTreeMap<String, String> = BTreeMap::new();
    for (id, source) in sources.iter().filter(|(id, _)| ids.contains(id.as_str())) {
        for parent in &source.source_memory_ids {
            join(&mut parents, id, parent);
        }
    }
    for span in spans {
        let Some(uri) = span.provenance_uri.as_deref() else {
            continue;
        };
        let Ok(uri) = ProvenanceUri::from_str(uri) else {
            continue;
        };
        let source_key = match uri {
            ProvenanceUri::CassSession { session, .. } => {
                format!("cass-session:{session}")
            }
            ProvenanceUri::File { path, .. } => format!("file-source:{path}"),
            ProvenanceUri::Web { url } => {
                // A fragment selects a position in one document, not another
                // independent observation. Keep the query and scheme: they
                // can identify different documents. Do not fetch/canonicalize
                // resources or guess that different URLs are equivalent.
                let document = url.split_once('#').map_or(url.as_str(), |(base, _)| base);
                format!("web-document:{document}")
            }
            ProvenanceUri::EeMemory(parent) => parent.to_string(),
            // Opaque capture labels (especially manual://cli) need not name
            // an individual source. Treating them as lineage would collapse
            // unrelated observations just because they used the same tool.
            ProvenanceUri::AgentMail { .. } | ProvenanceUri::External { .. } => continue,
        };
        // Private document keys cannot collide with typed entity IDs and are
        // never exported as citations or as source_memory_ids. EeMemory uses
        // the actual parent ID so memory/rule/document lineage is transitive.
        join(&mut parents, &span.memory_id, &source_key);
    }
    if sources.is_empty() && parents.is_empty() {
        return BTreeMap::new();
    }
    ids.into_iter()
        .map(|id| (id.to_owned(), root(&mut parents, id)))
        .collect()
}

fn join(parents: &mut BTreeMap<String, String>, left: &str, right: &str) {
    let left = root(parents, left);
    let right = root(parents, right);
    if left < right {
        parents.insert(right, left);
    } else if right < left {
        parents.insert(left, right);
    }
}

fn root(parents: &mut BTreeMap<String, String>, id: &str) -> String {
    let mut current = id.to_owned();
    let mut path = Vec::new();
    while let Some(parent) = parents.get(&current) {
        path.push(current.clone());
        current = parent.clone();
    }
    for child in path {
        parents.insert(child, current.clone());
    }
    current
}

#[cfg(test)]
mod session_support_tests {
    use super::*;

    fn span(id: &str, provenance: &str) -> AskSpan {
        AskSpan {
            memory_id: id.to_owned(),
            byte_start: 0,
            byte_end: 5,
            text: "Fact.".to_owned(),
            score: 0.8,
            trust_class: "cass_evidence".to_owned(),
            memory_confidence: 0.5,
            provenance_uri: Some(provenance.to_owned()),
            team_provenance: None,
        }
    }

    fn rule_source(parent: &str) -> AskNativeSource {
        AskNativeSource {
            entity: PackEntityRef::Rule(RuleId::from_uuid(uuid::Uuid::from_u128(1))),
            entity_revision: format!("blake3:{}", "0".repeat(64)),
            source_memory_ids: vec![parent.to_owned()],
        }
    }

    #[test]
    fn separate_line_windows_in_one_session_are_not_independent_votes() {
        let spans = vec![
            span("first", "cass-session://conversation#L1-3"),
            span("second", "cass-session://conversation#L20-22"),
            span("third", "cass-session://conversation"),
        ];
        let groups = support_groups(&spans, &BTreeMap::new());
        assert_eq!(groups.get("first"), groups.get("second"));
        assert_eq!(groups.get("second"), groups.get("third"));
        assert_eq!(groups.len(), 3);
    }

    #[test]
    fn different_sessions_remain_independent() {
        let spans = vec![
            span("first", "cass-session://conversation-a#L1"),
            span("second", "cass-session://conversation-b#L1"),
        ];
        let groups = support_groups(&spans, &BTreeMap::new());
        assert_ne!(groups.get("first"), groups.get("second"));
    }

    #[test]
    fn derived_rule_and_shared_session_form_one_transitive_group() {
        let spans = vec![
            span("memory", "cass-session://conversation#L1"),
            span("evidence", "cass-session://conversation#L2"),
            span("rule", "manual://rule"),
        ];
        let sources = BTreeMap::from([("rule".to_owned(), rule_source("memory"))]);
        let groups = support_groups(&spans, &sources);
        assert_eq!(groups.get("rule"), groups.get("memory"));
        assert_eq!(groups.get("rule"), groups.get("evidence"));
        assert_eq!(groups.len(), 3);
    }

    #[test]
    fn support_groups_are_independent_of_span_order() {
        let mut spans = vec![
            span("first", "cass-session://conversation#L1"),
            span("second", "cass-session://conversation#L2"),
            span("third", "cass-session://another#L1"),
        ];
        let before = support_groups(&spans, &BTreeMap::new());
        spans.reverse();
        assert_eq!(before, support_groups(&spans, &BTreeMap::new()));
    }

    #[test]
    fn absent_native_sources_cannot_join_visible_sessions() {
        let spans = vec![
            span("first", "cass-session://conversation-a#L1"),
            span("second", "cass-session://conversation-b#L1"),
        ];
        let mut hidden = rule_source("first");
        hidden.source_memory_ids.push("second".to_owned());
        let sources = BTreeMap::from([("hidden-rule".to_owned(), hidden)]);
        let groups = support_groups(&spans, &sources);
        assert_ne!(groups.get("first"), groups.get("second"));
        assert!(!groups.contains_key("hidden-rule"));
    }

    #[test]
    fn opaque_capture_labels_do_not_invent_shared_lineage() {
        let spans = vec![
            span("first", "manual://cli"),
            span("second", "manual://cli"),
        ];
        assert!(support_groups(&spans, &BTreeMap::new()).is_empty());
    }

    #[test]
    fn file_line_windows_share_support_but_different_files_do_not() {
        let spans = vec![
            span("first", "file://src/cache.rs#L1-3"),
            span("second", "file://src/cache.rs#L20"),
            span("whole", "file://src/cache.rs"),
            span("other", "file://src/other.rs#L1-3"),
        ];
        let groups = support_groups(&spans, &BTreeMap::new());
        assert_eq!(groups.get("first"), groups.get("second"));
        assert_eq!(groups.get("first"), groups.get("whole"));
        assert_ne!(groups.get("first"), groups.get("other"));
        assert_eq!(groups.len(), spans.len());
    }

    #[test]
    fn web_fragments_share_support_without_discarding_query_or_scheme() {
        let spans = vec![
            span("first", "https://example.test/decision?id=1#summary"),
            span("second", "https://example.test/decision?id=1#details"),
            span("whole", "https://example.test/decision?id=1"),
            span("other-query", "https://example.test/decision?id=2#summary"),
            span("other-scheme", "http://example.test/decision?id=1#summary"),
        ];
        let groups = support_groups(&spans, &BTreeMap::new());
        assert_eq!(groups.get("first"), groups.get("second"));
        assert_eq!(groups.get("first"), groups.get("whole"));
        assert_ne!(groups.get("first"), groups.get("other-query"));
        assert_ne!(groups.get("first"), groups.get("other-scheme"));
    }

    #[test]
    fn provenance_schemes_have_separate_private_identity_domains() {
        let spans = vec![
            span("file", "file://conversation#L1"),
            span("session", "cass-session://conversation#L1"),
            span("web", "https://conversation/#L1"),
        ];
        let groups = support_groups(&spans, &BTreeMap::new());
        assert_eq!(groups.values().collect::<BTreeSet<_>>().len(), 3);
    }

    #[test]
    fn memory_references_join_file_and_rule_lineage_transitively() {
        let parent = MemoryId::from_uuid(uuid::Uuid::from_u128(11)).to_string();
        let spans = vec![
            span(&parent, "file://decisions.md#L1"),
            span("copy", &format!("ee-mem://{parent}")),
            span("excerpt", "file://decisions.md#L20"),
            span("rule", "manual://rule"),
        ];
        let sources = BTreeMap::from([("rule".to_owned(), rule_source("copy"))]);
        let groups = support_groups(&spans, &sources);
        assert_eq!(groups.values().collect::<BTreeSet<_>>().len(), 1);
        assert_eq!(groups.len(), spans.len());
        assert!(
            groups
                .keys()
                .all(|id| spans.iter().any(|span| &span.memory_id == id))
        );
        // Support analysis must not rewrite the public citation or lineage.
        assert_eq!(spans[1].provenance_uri, Some(format!("ee-mem://{parent}")));
        assert_eq!(sources["rule"].source_memory_ids, vec!["copy".to_owned()]);
    }

    #[test]
    fn shared_absent_memory_parent_correlates_only_visible_citations() {
        let parent = MemoryId::from_uuid(uuid::Uuid::from_u128(12)).to_string();
        let spans = vec![
            span("first", &format!("ee-mem://{parent}")),
            span("second", &format!("ee-mem://{parent}")),
        ];
        let groups = support_groups(&spans, &BTreeMap::new());
        assert_eq!(groups.get("first"), groups.get("second"));
        assert!(!groups.contains_key(&parent));
    }

    #[test]
    fn repeated_documents_cannot_lift_weak_evidence_above_the_answer_floor() {
        for prefix in ["file://fact.md#L", "https://example.test/fact#section-"] {
            let mut spans: Vec<_> = (1..=32)
                .map(|index| {
                    let mut item = span(&format!("copy-{index:02}"), &format!("{prefix}{index}"));
                    item.score = 0.54;
                    item
                })
                .collect();
            // The old distinct-ID count alone would authorize this answer.
            let ungrouped = super::super::clustering::cluster_spans(&spans);
            assert!(ungrouped[0].score > super::super::ASK_MIN_CONFIDENCE_DEFAULT);
            let groups = support_groups(&spans, &BTreeMap::new());
            let clustered = super::super::clustering::cluster_spans_with_groups(&spans, &groups);
            assert_eq!(clustered.len(), 1);
            assert_eq!(clustered[0].score, 0.54);
            assert!(clustered[0].score < super::super::ASK_MIN_CONFIDENCE_DEFAULT);

            let mut independent = span("independent", "file://independent-observation.md#L1");
            independent.score = 0.54;
            spans.push(independent);
            let groups = support_groups(&spans, &BTreeMap::new());
            let clustered = super::super::clustering::cluster_spans_with_groups(&spans, &groups);
            assert!(clustered[0].score > super::super::ASK_MIN_CONFIDENCE_DEFAULT);
            assert!((clustered[0].score - 0.54 * (1.0 + 0.1 * 2.0_f32.ln())).abs() < 1e-6);
        }
    }

    #[test]
    fn memory_reference_cycles_are_deterministic_and_do_not_loop() {
        let first = MemoryId::from_uuid(uuid::Uuid::from_u128(13)).to_string();
        let second = MemoryId::from_uuid(uuid::Uuid::from_u128(14)).to_string();
        let mut spans = vec![
            span(&first, &format!("ee-mem://{second}")),
            span(&second, &format!("ee-mem://{first}")),
        ];
        let groups = support_groups(&spans, &BTreeMap::new());
        assert_eq!(groups.get(&first), groups.get(&second));
        spans.reverse();
        assert_eq!(groups, support_groups(&spans, &BTreeMap::new()));
    }

    #[test]
    fn missing_and_malformed_provenance_cannot_invent_shared_support() {
        let mut missing = span("missing", "manual://cli");
        missing.provenance_uri = None;
        let spans = vec![
            missing,
            span("first", "file://"),
            span("second", "file://"),
            span("invalid-memory", "ee-mem://not-a-memory-id"),
        ];
        assert!(support_groups(&spans, &BTreeMap::new()).is_empty());
    }
}
