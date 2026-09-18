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
/// Excerpts from one CASS session are likewise one source even when they have
/// different evidence IDs or line windows. Join both kinds of lineage before
/// counting support, including a shared memory outside the answer corpus.
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
        if let Some(uri) = span.provenance_uri.as_deref()
            && let Ok(ProvenanceUri::CassSession { session, .. }) = ProvenanceUri::from_str(uri)
        {
            // This private union key is not an entity or a citation. It cannot
            // collide with a typed memory/rule/evidence ID, and is never put
            // into source_memory_ids or public source metadata.
            join(
                &mut parents,
                &span.memory_id,
                &format!("cass-session:{session}"),
            );
        }
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
    fn ordinary_non_cass_sources_keep_the_default_grouping() {
        let spans = vec![
            span("first", "manual://note"),
            span("second", "file://src/lib.rs#L1"),
        ];
        assert!(support_groups(&spans, &BTreeMap::new()).is_empty());
    }
}
