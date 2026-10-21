//! Replay graph changes as paired, identity-only cache invalidations.
//!
//! A link audit addresses an edge, not a memory. Resolve both endpoints from
//! the durable link table in the poll owner's snapshot. Never trust free-form
//! audit annotations as endpoint authority or split one audit across pages.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use sqlmodel_core::Value;

use super::super::RawAuditDelta;
use super::{MEMORY_INVALIDATION_SCHEMA_V1, MemoryDelta, MemoryInvalidation, SubscribeFilter};
use crate::db::DbConnection;
use crate::models::{DomainError, MemoryId, MemoryLinkId};

const LINK_BATCH: usize = 128;
const MAX_METADATA_BYTES: usize = 16 * 1024;

fn invalid() -> DomainError {
    DomainError::Storage {
        message: "Could not replay a complete memory-link change; no cursor was acknowledged"
            .to_owned(),
        repair: Some(
            "Keep the previous cursor. Run ee doctor --json and restore or repair the retained link history before retrying."
                .to_owned(),
        ),
    }
}

fn checkpoint() -> Result<(), DomainError> {
    if let Some(cx) = asupersync::Cx::current() {
        cx.checkpoint().map_err(|_| invalid())?;
    }
    Ok(())
}

pub(super) struct PendingLink {
    link_id: String,
    event: MemoryDelta,
}

pub(super) fn collect(
    raw: RawAuditDelta,
    pending: &mut Vec<PendingLink>,
) -> Result<(), DomainError> {
    if !raw
        .memory_id
        .parse::<MemoryLinkId>()
        .is_ok_and(|id| id.to_string() == raw.memory_id)
        || raw.workspace_id.is_none()
    {
        return Err(invalid());
    }
    let link_id = raw.memory_id.clone();
    // This event is internal routing metadata, never a public memory delta.
    // Do not copy actor, memory attributes, link annotations or source bodies.
    let event = MemoryDelta {
        schema: super::super::MEMORY_DELTA_SCHEMA_V1,
        cursor: raw.cursor,
        kind: "updated".to_owned(),
        memory_id: String::new(),
        levels: Vec::new(),
        kinds: Vec::new(),
        tags: Vec::new(),
        workspace_id: raw.workspace_id,
        trust_class: None,
        agent_name: None,
        changed_fields: vec!["links".to_owned()],
        audit_id: raw.audit_id,
        occurred_at: raw.occurred_at,
    };
    pending.push(PendingLink { link_id, event });
    Ok(())
}

fn text(row: &sqlmodel_core::Row, index: usize) -> Result<&str, DomainError> {
    row.get(index).and_then(Value::as_str).ok_or_else(invalid)
}

/// Read only bounded relationship projections, never complete memories or
/// arbitrary audit details. Repeated events share a lookup but retain their
/// own cursors. Missing/foreign endpoints cannot turn into an empty success.
fn endpoints(
    db: &DbConnection,
    workspace: &str,
    pending: &[PendingLink],
) -> Result<BTreeMap<String, Option<[String; 2]>>, DomainError> {
    let ids: BTreeSet<_> = pending.iter().map(|entry| entry.link_id.as_str()).collect();
    let ids: Vec<_> = ids.into_iter().collect();
    let mut result = BTreeMap::new();
    for batch in ids.chunks(LINK_BATCH) {
        checkpoint()?;
        let slots = (3..3 + batch.len())
            .map(|index| format!("?{index}"))
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT l.id, \
                CASE WHEN length(CAST(l.src_memory_id AS BLOB)) <= 128 THEN l.src_memory_id ELSE NULL END, \
                CASE WHEN length(CAST(l.dst_memory_id AS BLOB)) <= 128 THEN l.dst_memory_id ELSE NULL END, \
                CASE WHEN s.workspace_id = ?1 AND d.workspace_id = ?1 THEN 1 ELSE 0 END, \
                CASE WHEN l.metadata_json IS NULL THEN NULL \
                     WHEN length(CAST(l.metadata_json AS BLOB)) <= ?2 THEN l.metadata_json ELSE '' END \
             FROM memory_links l \
             LEFT JOIN memories s ON s.id = l.src_memory_id \
             LEFT JOIN memories d ON d.id = l.dst_memory_id \
             WHERE l.id IN ({slots})"
        );
        let mut parameters = vec![
            Value::Text(workspace.to_owned()),
            Value::BigInt(MAX_METADATA_BYTES as i64),
        ];
        parameters.extend(batch.iter().map(|id| Value::Text((*id).to_owned())));
        let rows = db.query(&sql, &parameters).map_err(|_| invalid())?;
        checkpoint()?;
        if rows.len() != batch.len() {
            return Err(invalid());
        }
        for row in rows {
            let id = text(&row, 0)?;
            let source = text(&row, 1)?;
            let target = text(&row, 2)?;
            if !batch.contains(&id)
                || result.contains_key(id)
                || row.get(3).and_then(Value::as_i64) != Some(1)
                || source == target
                || [source, target].into_iter().any(|value| {
                    !value
                        .parse::<MemoryId>()
                        .is_ok_and(|parsed| parsed.to_string() == value)
                })
            {
                return Err(invalid());
            }
            let metadata = match row.get(4) {
                Some(Value::Null) => None,
                Some(Value::Text(value)) if !value.is_empty() => Some(value.as_str()),
                _ => return Err(invalid()),
            };
            // The same mesh visibility rule owns graph/list admission. A
            // hidden edge cannot expose a new relationship through notices.
            let pair = crate::graph::memory_link_mesh_metadata_visible(metadata)
                .then(|| [source.to_owned(), target.to_owned()]);
            result.insert(id.to_owned(), pair);
        }
    }
    Ok(result)
}

pub(super) fn notices(
    db: &DbConnection,
    workspace: &str,
    pending: &[PendingLink],
    filter: &SubscribeFilter,
    since_cutoff: Option<DateTime<Utc>>,
) -> Result<Vec<MemoryInvalidation>, DomainError> {
    let endpoints = endpoints(db, workspace, pending)?;
    let routing = SubscribeFilter {
        workspace_ids: filter.workspace_ids.clone(),
        changed_fields: filter.changed_fields.clone(),
        since_ms: filter.since_ms,
        ..SubscribeFilter::default()
    };
    let mut notices = Vec::new();
    for entry in pending {
        checkpoint()?;
        if entry.event.workspace_id.as_deref() != Some(workspace) {
            return Err(invalid());
        }
        let pair = endpoints.get(&entry.link_id).ok_or_else(invalid)?;
        let Some(pair) = pair else { continue };
        if !routing.matches_delta(&entry.event, since_cutoff) {
            continue;
        }
        // The edge may affect a cached item's relationship-derived relevance
        // even when its current metadata no longer matches the old filter.
        // Identity-only eviction grants no content access or trust promotion.
        for memory_id in pair {
            notices.push(MemoryInvalidation {
                schema: MEMORY_INVALIDATION_SCHEMA_V1,
                cursor: entry.event.cursor,
                memory_id: memory_id.clone(),
                workspace_id: entry.event.workspace_id.clone(),
                audit_id: entry.event.audit_id.clone(),
                occurred_at: entry.event.occurred_at.clone(),
                changed_fields: entry.event.changed_fields.clone(),
                affected_filters: Vec::new(),
                reason: "memory_link_changed",
            });
        }
    }
    checkpoint()?;
    Ok(notices)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::*;
    use crate::core::memory::{
        MemoryLinkMode, MemoryLinkOptions, MemoryLinkReport, update_memory_link,
    };
    use crate::core::subscribe::{
        SubscribePollOptions, SubscribePollReport, parse_subscribe_filter, poll_memory_deltas,
    };
    use crate::core::workspace::stable_workspace_id;
    use crate::db::{
        CreateAuditInput, CreateMemoryInput, CreateWorkspaceInput, MemoryLinkRelation,
        MemoryLinkSource, audit_actions,
    };
    use std::path::PathBuf;

    struct Fixture {
        _root: tempfile::TempDir,
        workspace: PathBuf,
        database: PathBuf,
        db: DbConnection,
        own: String,
        other: String,
    }

    impl Fixture {
        fn new() -> Self {
            let root = tempfile::tempdir().expect("temporary workspace");
            let workspace = root.path().canonicalize().expect("physical root");
            std::fs::create_dir(workspace.join(".ee")).expect("workspace marker");
            let database = workspace.join(".ee/ee.db");
            let db = DbConnection::open_file(&database).expect("database");
            db.migrate().expect("migrations");
            let own = stable_workspace_id(&workspace);
            let other_path = workspace.join("other");
            let other = stable_workspace_id(&other_path);
            for (id, path) in [(&own, &workspace), (&other, &other_path)] {
                db.insert_workspace(
                    id,
                    &CreateWorkspaceInput {
                        path: path.to_string_lossy().into_owned(),
                        name: None,
                    },
                )
                .expect("workspace binding");
            }
            Self {
                _root: root,
                workspace,
                database,
                db,
                own,
                other,
            }
        }

        fn memory(&self, number: u128, foreign: bool) -> String {
            let id = MemoryId::from_uuid(uuid::Uuid::from_u128(number)).to_string();
            self.db
                .insert_memory(
                    &id,
                    &CreateMemoryInput {
                        workspace_id: if foreign { &self.other } else { &self.own }.clone(),
                        level: "procedural".to_owned(),
                        kind: "rule".to_owned(),
                        content: format!("PRIVATE_LINK_BODY_{number}"),
                        workflow_id: None,
                        confidence: 0.8,
                        utility: 0.5,
                        importance: 0.5,
                        provenance_uri: Some("manual://subscription-links".to_owned()),
                        trust_class: "human_explicit".to_owned(),
                        trust_subclass: None,
                        tags: vec!["private-link-tag".to_owned()],
                        valid_from: None,
                        valid_to: None,
                    },
                )
                .expect("memory");
            id
        }

        fn link(&self, source: &str, target: &str) -> MemoryLinkReport {
            update_memory_link(&MemoryLinkOptions {
                workspace_path: &self.workspace,
                database_path: &self.database,
                memory_id: source,
                mode: MemoryLinkMode::Create {
                    target_memory_id: target.to_owned(),
                    relation: MemoryLinkRelation::Supports,
                    weight: 1.0,
                    confidence: 0.9,
                    directed: true,
                    evidence_count: 1,
                    source: MemoryLinkSource::Human,
                    metadata_json: Some("{\"note\":\"PRIVATE_LINK_ANNOTATION\"}".to_owned()),
                },
                actor: Some("PRIVATE_LINK_ACTOR"),
                dry_run: false,
                include_tombstoned: false,
            })
            .expect("public memory-link producer")
        }

        fn audit(&self, id: &str, kind: &str, action: &str) -> u64 {
            self.db
                .insert_audit(
                    &crate::db::generate_audit_id(),
                    &CreateAuditInput {
                        workspace_id: Some(self.own.clone()),
                        actor: Some("PRIVATE_LINK_ACTOR".to_owned()),
                        action: action.to_owned(),
                        target_type: Some(kind.to_owned()),
                        target_id: Some(id.to_owned()),
                        details: None,
                    },
                )
                .expect("audit");
            self.head()
        }

        fn head(&self) -> u64 {
            let rows = self
                .db
                .query("SELECT COALESCE(MAX(rowid), 0) FROM audit_log", &[])
                .expect("head");
            u64::try_from(rows[0].get(0).and_then(Value::as_i64).expect("cursor"))
                .expect("positive cursor")
        }

        fn poll(
            &self,
            cursor: u64,
            limit: u32,
            filter: Option<&str>,
        ) -> Result<SubscribePollReport, DomainError> {
            poll_memory_deltas(&SubscribePollOptions {
                workspace_path: &self.workspace,
                database_path: Some(&self.database),
                cursor,
                limit,
                filter: parse_subscribe_filter(filter).expect("filter"),
            })
        }
    }

    #[test]
    fn public_link_creation_invalidates_both_endpoints_once_without_private_metadata() {
        let f = Fixture::new();
        let low = f.memory(1, false);
        let high = f.memory(2, false);
        let before = f.db.get_memory(&low).unwrap();
        let report = f.link(&high, &low);
        assert!(report.persisted && report.changed);
        let audit_id = report.audit_id.as_deref().expect("durable audit");
        let cursor = f.head();
        let page = f.poll(0, 1, None).expect("paired notice page");
        assert!(page.deltas.is_empty());
        assert_eq!(page.invalidations.len(), 2);
        assert_eq!(
            page.invalidations
                .iter()
                .map(|n| n.memory_id.as_str())
                .collect::<Vec<_>>(),
            [low.as_str(), high.as_str()]
        );
        for notice in &page.invalidations {
            assert_eq!(notice.cursor, cursor);
            assert_eq!(notice.audit_id, audit_id);
            assert_eq!(notice.workspace_id.as_deref(), Some(f.own.as_str()));
            assert_eq!(notice.changed_fields, ["links"]);
            assert_eq!(notice.reason, "memory_link_changed");
            let value = serde_json::to_value(notice).expect("public notice");
            for private in ["PRIVATE_LINK", "private-link-tag", "manual://", "supports"] {
                assert!(!value.to_string().contains(private));
            }
            for field in [
                "content",
                "tags",
                "levels",
                "kinds",
                "trustClass",
                "agentName",
                "linkId",
            ] {
                assert!(value.get(field).is_none(), "{field}");
            }
        }
        assert_eq!(page.next_cursor, cursor);
        assert!(!page.has_more);
        assert_eq!(f.db.get_memory(&low).unwrap(), before);
        let repeated = f.link(&high, &low);
        assert!(!repeated.changed);
        let replay = f
            .poll(page.next_cursor, 1, None)
            .expect("idempotent replay");
        assert!(replay.deltas.is_empty() && replay.invalidations.is_empty());
        assert_eq!(replay.next_cursor, cursor);
    }

    #[test]
    fn link_notices_share_one_page_boundary_with_memory_changes_and_observations() {
        let f = Fixture::new();
        let first = f.memory(1, false);
        let second = f.memory(2, false);
        let created = f.audit(&first, "memory", audit_actions::MEMORY_CREATE);
        f.link(&first, &second);
        let linked = f.head();
        let changed = f.audit(&first, "memory", audit_actions::MEMORY_TAG_SET);
        let tail = f.audit(&first, "memory", audit_actions::MEMORY_SHOW);
        let page = f.poll(0, 1, None).expect("memory creation");
        assert_eq!(page.deltas.len(), 1);
        assert!(page.invalidations.is_empty());
        assert_eq!(page.next_cursor, created);
        assert!(page.has_more);
        let page = f.poll(created, 1, None).expect("whole edge");
        assert!(page.deltas.is_empty());
        assert_eq!(page.invalidations.len(), 2);
        assert!(page.invalidations.iter().all(|n| n.cursor == linked));
        assert_eq!(page.next_cursor, linked);
        assert!(page.has_more);
        let page = f.poll(linked, 1, None).expect("later mutation");
        assert_eq!(page.deltas.len(), 1);
        assert_eq!(page.deltas[0].cursor, changed);
        assert!(page.invalidations.is_empty());
        assert_eq!(page.next_cursor, tail);
        assert!(!page.has_more);
    }

    #[test]
    fn link_routing_honors_fields_workspace_and_time_without_inventing_filter_membership() {
        let f = Fixture::new();
        let first = f.memory(1, false);
        let second = f.memory(2, false);
        f.link(&first, &second);
        let head = f.head();
        let wanted = f
            .poll(
                0,
                1,
                Some("TAG=release,LEVEL=episodic,CHANGED_FIELDS=links"),
            )
            .expect("conservative cache notice");
        assert!(wanted.deltas.is_empty());
        assert_eq!(wanted.invalidations.len(), 2);
        for expression in [
            "CHANGED_FIELDS=content_hash",
            "CHANGED_FIELDS=links+confidence",
        ] {
            let page = f.poll(0, 1, Some(expression)).expect("routing filter");
            assert!(page.deltas.is_empty() && page.invalidations.is_empty());
            assert_eq!(page.next_cursor, head);
        }
        let expression = format!("WORKSPACE_ID={},CHANGED_FIELDS=links", f.other);
        assert!(
            f.poll(0, 1, Some(&expression))
                .unwrap()
                .invalidations
                .is_empty()
        );
        let reader = crate::db::DbConnection::open(crate::db::DatabaseConfig::read_only_file(
            f.database.clone(),
        ))
        .unwrap();
        let snapshot = super::super::SubscriptionSnapshot::begin(&reader).unwrap();
        let future = DateTime::parse_from_rfc3339("2999-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let page = snapshot
            .page(&f.own, 0, 1, &SubscribeFilter::default(), Some(future))
            .unwrap();
        assert!(page.deltas.is_empty() && page.invalidations.is_empty());
        assert_eq!(page.next_cursor, head);
        snapshot.finish().unwrap();
    }
}
