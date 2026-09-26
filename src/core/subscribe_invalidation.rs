//! Conservative invalidation when a memory leaves a subscription's filter.
//!
//! Audit rows do not contain complete event-time tags, levels, kinds or trust.
//! Testing only the live projection loses tag removals and trust downgrades.
//! Do not invent historical metadata or widen the ordinary matching delta set:
//! emit a separate identity-only notice, explicitly uncertain about old filter
//! membership. Scope, time and changed-field routing still apply exactly.

use chrono::{DateTime, Utc};
use serde::Serialize;

use super::super::{MemoryDelta, SubscribeFilter};

pub const MEMORY_INVALIDATION_SCHEMA_V1: &str = "ee.memory.invalidation.v1";

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryInvalidation {
    pub schema: &'static str,
    pub cursor: u64,
    pub memory_id: String,
    pub workspace_id: Option<String>,
    pub audit_id: String,
    pub occurred_at: String,
    pub changed_fields: Vec<String>,
    /// Filter dimensions whose prior membership cannot be reconstructed.
    pub affected_filters: Vec<&'static str>,
    pub reason: &'static str,
}

pub(super) fn filtered_invalidation(
    filter: &SubscribeFilter,
    delta: &MemoryDelta,
    since_cutoff: Option<DateTime<Utc>>,
) -> Option<MemoryInvalidation> {
    if delta.kind == "created" || filter.matches_delta(delta, since_cutoff) {
        return None;
    }
    let routing = SubscribeFilter {
        workspace_ids: filter.workspace_ids.clone(),
        changed_fields: filter.changed_fields.clone(),
        since_ms: filter.since_ms,
        ..SubscribeFilter::default()
    };
    if !routing.matches_delta(delta, since_cutoff) {
        return None;
    }
    let mut affected_filters = Vec::new();
    if !filter.levels.is_empty() {
        affected_filters.push("levels");
    }
    if !filter.kinds.is_empty() {
        affected_filters.push("kinds");
    }
    if !filter.tags.is_empty() {
        affected_filters.push("tags");
    }
    if filter.min_trust_class.is_some() {
        affected_filters.push("trustClass");
    }
    if affected_filters.is_empty() {
        return None;
    }
    // Deliberately omit actor, tags, level, kind, trust and source bodies.
    // This notice authorizes no content read and asserts no prior eligibility.
    // The poll owner has already checked workspace and typed memory identity.
    Some(MemoryInvalidation {
        schema: MEMORY_INVALIDATION_SCHEMA_V1,
        cursor: delta.cursor,
        memory_id: delta.memory_id.clone(),
        workspace_id: delta.workspace_id.clone(),
        audit_id: delta.audit_id.clone(),
        occurred_at: delta.occurred_at.clone(),
        changed_fields: delta.changed_fields.clone(),
        affected_filters,
        reason: "prior_filter_membership_unknown",
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::subscribe::parse_subscribe_filter;

    fn delta(kind: &str) -> MemoryDelta {
        MemoryDelta {
            schema: crate::core::subscribe::MEMORY_DELTA_SCHEMA_V1,
            cursor: 7,
            kind: kind.to_owned(),
            memory_id: crate::models::MemoryId::from_uuid(uuid::Uuid::from_u128(1)).to_string(),
            levels: vec!["semantic".to_owned()],
            kinds: vec!["decision".to_owned()],
            tags: vec!["new-label".to_owned()],
            workspace_id: Some("workspace-one".to_owned()),
            trust_class: Some("legacy_import".to_owned()),
            agent_name: Some("private-actor".to_owned()),
            changed_fields: vec!["tags".to_owned(), "trust_class".to_owned()],
            audit_id: "audit-one".to_owned(),
            occurred_at: "2026-09-26T00:00:00Z".to_owned(),
        }
    }

    #[test]
    fn tag_removal_emits_identity_without_current_or_historical_metadata() {
        let filter = parse_subscribe_filter(Some("TAG=release")).expect("filter");
        let event = delta("updated");
        let notice = filtered_invalidation(&filter, &event, None).expect("invalidation");
        assert_eq!(notice.memory_id, event.memory_id);
        assert_eq!(notice.affected_filters, vec!["tags"]);
        let value = serde_json::to_value(&notice).expect("serialize");
        assert_eq!(value["reason"], "prior_filter_membership_unknown");
        for field in [
            "tags",
            "levels",
            "kinds",
            "trustClass",
            "agentName",
            "content",
        ] {
            assert!(value.get(field).is_none(), "unexpected metadata: {field}");
        }
        assert!(!value.to_string().contains("new-label"));
        assert!(!value.to_string().contains("private-actor"));
    }

    #[test]
    fn trust_downgrade_is_an_invalidation_not_a_trust_eligible_delta() {
        let filter = parse_subscribe_filter(Some("TRUST_CLASS=human_explicit")).expect("filter");
        let event = delta("updated");
        assert!(!filter.matches_delta(&event, None));
        let notice = filtered_invalidation(&filter, &event, None).expect("revocation");
        assert_eq!(notice.affected_filters, vec!["trustClass"]);
        assert!(filtered_invalidation(&filter, &delta("created"), None).is_none());
    }

    #[test]
    fn mixed_membership_dimensions_are_not_fabricated_from_partial_history() {
        let filter = parse_subscribe_filter(Some(
            "LEVEL=procedural,KIND=rule,TAG=release,TRUST_CLASS=human_explicit",
        ))
        .expect("filter");
        let notice = filtered_invalidation(&filter, &delta("updated"), None).expect("notice");
        assert_eq!(
            notice.affected_filters,
            vec!["levels", "kinds", "tags", "trustClass"]
        );
    }

    #[test]
    fn scope_time_and_changed_field_routing_are_never_broadened() {
        let event = delta("updated");
        for text in [
            "TAG=release,WORKSPACE_ID=workspace-two",
            "TAG=release,CHANGED_FIELDS=level",
        ] {
            let filter = parse_subscribe_filter(Some(text)).expect("filter");
            assert!(filtered_invalidation(&filter, &event, None).is_none());
        }
        let filter = parse_subscribe_filter(Some("TAG=release")).expect("filter");
        let later = DateTime::parse_from_rfc3339("2026-09-27T00:00:00Z")
            .expect("timestamp")
            .with_timezone(&Utc);
        assert!(filtered_invalidation(&filter, &event, Some(later)).is_none());
    }

    #[test]
    fn creations_and_current_matches_do_not_produce_duplicate_invalidations() {
        let filter = parse_subscribe_filter(Some("TAG=new-label")).expect("filter");
        assert!(filtered_invalidation(&filter, &delta("updated"), None).is_none());
        let other = parse_subscribe_filter(Some("TAG=release")).expect("filter");
        assert!(filtered_invalidation(&other, &delta("created"), None).is_none());
        assert!(
            filtered_invalidation(&SubscribeFilter::default(), &delta("updated"), None).is_none()
        );
    }

    #[test]
    fn filtered_tombstones_survive_missing_current_memory_metadata() {
        let filter = parse_subscribe_filter(Some("LEVEL=procedural,TAG=release")).expect("filter");
        let mut event = delta("tombstoned");
        event.tags.clear();
        event.levels.clear();
        event.kinds.clear();
        event.trust_class = None;
        event.changed_fields = vec!["tombstoned_at".to_owned()];
        let notice = filtered_invalidation(&filter, &event, None).expect("eviction notice");
        assert_eq!(notice.changed_fields, vec!["tombstoned_at".to_owned()]);
    }
}
