//! Native source identity and lineage for extractive answers.
//!
//! The span engine treats its historical `memory_id` slot as an opaque source
//! key. Native rules retain their RuleId there; this module projects the real
//! kind/revision at every public boundary instead of minting a MemoryId.

use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;

use crate::models::{EvidenceId, MemoryId, RuleId};
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
/// Collapse intersecting lineages, including a shared source that is not itself
/// in the answer corpus. Root choice and path compression are deterministic.
pub(super) fn support_groups(
    spans: &[AskSpan],
    sources: &BTreeMap<String, AskNativeSource>,
) -> BTreeMap<String, String> {
    if sources.is_empty() {
        return BTreeMap::new();
    }
    let ids: BTreeSet<_> = spans.iter().map(|span| span.memory_id.as_str()).collect();
    let mut parents: BTreeMap<String, String> = BTreeMap::new();
    for (id, source) in sources.iter().filter(|(id, _)| ids.contains(id.as_str())) {
        for parent in &source.source_memory_ids {
            let left = root(&mut parents, id);
            let right = root(&mut parents, parent);
            if left < right {
                parents.insert(right, left);
            } else if right < left {
                parents.insert(left, right);
            }
        }
    }
    ids.into_iter()
        .map(|id| (id.to_owned(), root(&mut parents, id)))
        .collect()
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
