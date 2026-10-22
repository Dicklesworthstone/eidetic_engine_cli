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
    endpoints_observed(db, workspace, pending, |_| Ok(()))
}

fn endpoints_observed(
    db: &DbConnection,
    workspace: &str,
    pending: &[PendingLink],
    mut after_read: impl FnMut(&'static str) -> Result<(), DomainError>,
) -> Result<BTreeMap<String, Option<[String; 2]>>, DomainError> {
    // The pinned engine can implement IN batches and joins as full scans.
    // Point-read each distinct edge and endpoint instead. Cache only successful
    // workspace checks in this one source snapshot; no cached body or authority
    // survives a poll. Shared graph hubs are checked once, not once per edge.
    let ids: BTreeSet<_> = pending.iter().map(|entry| entry.link_id.as_str()).collect();
    let mut result = BTreeMap::new();
    let mut checked_memories = BTreeSet::new();
    for expected_id in ids {
        checkpoint()?;
        let rows = db.query(
            "SELECT l.id, \
                CASE WHEN length(CAST(l.src_memory_id AS BLOB)) <= 128 THEN l.src_memory_id ELSE NULL END, \
                CASE WHEN length(CAST(l.dst_memory_id AS BLOB)) <= 128 THEN l.dst_memory_id ELSE NULL END, \
                CASE WHEN l.metadata_json IS NULL THEN NULL \
                     WHEN length(CAST(l.metadata_json AS BLOB)) <= ?2 THEN l.metadata_json ELSE '' END \
             FROM memory_links l WHERE l.id = ?1",
            &[Value::Text(expected_id.to_owned()), Value::BigInt(MAX_METADATA_BYTES as i64)],
        ).map_err(|_| invalid())?;
        checkpoint()?;
        after_read("memory_links")?;
        checkpoint()?;
        if rows.len() != 1 {
            return Err(invalid());
        }
        for row in rows {
            let id = text(&row, 0)?;
            let source = text(&row, 1)?;
            let target = text(&row, 2)?;
            if id != expected_id
                || result.contains_key(id)
                || source == target
                || [source, target].into_iter().any(|value| {
                    !value
                        .parse::<MemoryId>()
                        .is_ok_and(|parsed| parsed.to_string() == value)
                })
            {
                return Err(invalid());
            }
            for memory_id in [source, target] {
                checkpoint()?;
                if checked_memories.contains(memory_id) {
                    continue;
                }
                let memories = db.query(
                    "SELECT CASE WHEN workspace_id = ?2 THEN 1 ELSE 0 END FROM memories WHERE id = ?1",
                    &[Value::Text(memory_id.to_owned()), Value::Text(workspace.to_owned())],
                ).map_err(|_| invalid())?;
                checkpoint()?;
                after_read("memories")?;
                checkpoint()?;
                if memories.len() != 1 || memories[0].get(0).and_then(Value::as_i64) != Some(1) {
                    return Err(invalid());
                }
                checked_memories.insert(memory_id.to_owned());
            }
            let metadata = match row.get(3) {
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
    // A memory-only poll must not consult link storage or acquire a new
    // cancellation boundary merely because this optional replay exists.
    if pending.is_empty() {
        return Ok(Vec::new());
    }
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

    fn pending_edge(f: &Fixture, source: &str, target: &str, number: u128) -> PendingLink {
        let id = MemoryLinkId::from_uuid(uuid::Uuid::from_u128(number)).to_string();
        f.db.insert_memory_link(
            &id,
            &crate::db::CreateMemoryLinkInput {
                src_memory_id: source.to_owned(),
                dst_memory_id: target.to_owned(),
                relation: MemoryLinkRelation::Supports,
                weight: 1.0,
                confidence: 0.9,
                directed: true,
                evidence_count: 1,
                last_reinforced_at: None,
                source: MemoryLinkSource::Human,
                created_by: None,
                metadata_json: None,
            },
        )
        .expect("durable source edge");
        PendingLink {
            link_id: id,
            event: routing_event(f, number as u64),
        }
    }

    #[test]
    fn point_reads_deduplicate_edges_and_shared_hubs_without_caching_across_polls() {
        let f = Fixture::new();
        let anchor = f.memory(1, false);
        let mut pending = Vec::new();
        let mut last = String::new();
        for n in 1..=257_u128 {
            let target = f.memory(1000 + n, false);
            pending.push(pending_edge(&f, &anchor, &target, n));
            last = target;
        }
        for cursor in 258..263 {
            pending.push(PendingLink {
                link_id: pending[0].link_id.clone(),
                event: routing_event(&f, cursor),
            });
        }
        let mut edge_reads = 0;
        let mut memory_reads = 0;
        let pairs = endpoints_observed(&f.db, &f.own, &pending, |table| {
            match table {
                "memory_links" => edge_reads += 1,
                "memories" => memory_reads += 1,
                _ => panic!("unexpected projection"),
            }
            Ok(())
        })
        .unwrap();
        assert_eq!((edge_reads, memory_reads), (257, 258));
        assert_eq!(pairs.len(), 257);
        for pair in pairs.values() {
            assert_eq!(pair.as_ref().unwrap()[0], anchor);
        }
        let notices = notices(&f.db, &f.own, &pending, &SubscribeFilter::default(), None).unwrap();
        assert_eq!(
            notices.len(),
            pending.len() * 2,
            "deduplicated reads do not deduplicate events"
        );
        f.db.execute_raw(&format!(
            "UPDATE memories SET workspace_id = '{}' WHERE id = '{last}'",
            f.other
        ))
        .unwrap();
        assert!(
            endpoints(&f.db, &f.own, &pending).is_err(),
            "a new poll must reload authority"
        );
    }

    #[test]
    fn edge_and_endpoint_point_reads_borrow_one_pinned_snapshot() {
        let f = Fixture::new();
        let first = f.memory(1, false);
        let second = f.memory(2, false);
        let pending = vec![pending_edge(&f, &first, &second, 1)];
        let reader = DbConnection::open(crate::db::DatabaseConfig::read_only_file(
            f.database.clone(),
        ))
        .unwrap();
        let snapshot = super::super::SubscriptionSnapshot::begin(&reader).unwrap();
        reader
            .query("SELECT COUNT(*) FROM memory_links", &[])
            .unwrap();
        let mut writes = 0;
        let pairs = endpoints_observed(&reader, &f.own, &pending, |table| {
            if table == "memory_links" {
                f.db.execute_raw(&format!(
                    "UPDATE memories SET workspace_id = '{}' WHERE id = '{second}'",
                    f.other
                ))
                .map_err(|_| invalid())?;
                writes += 1;
            }
            Ok(())
        })
        .unwrap();
        assert_eq!(writes, 1);
        assert_eq!(pairs[&pending[0].link_id], Some([first, second]));
        assert!(
            reader.begin_read_snapshot().is_err(),
            "the caller retains its view"
        );
        snapshot.finish().unwrap();
        assert!(endpoints(&reader, &f.own, &pending).is_err());
    }

    #[test]
    fn late_point_read_failure_discards_prior_pairs_and_allows_rollback_retry() {
        let f = Fixture::new();
        let anchor = f.memory(1, false);
        let first = f.memory(2, false);
        let second = f.memory(3, false);
        let pending = vec![
            pending_edge(&f, &anchor, &first, 1),
            pending_edge(&f, &anchor, &second, 2),
        ];
        let expected = endpoints(&f.db, &f.own, &pending).unwrap();
        f.db.execute_raw("BEGIN IMMEDIATE").unwrap();
        let mut edges = 0;
        let error = endpoints_observed(&f.db, &f.own, &pending, |table| {
            if table == "memory_links" {
                edges += 1;
                if edges == 2 {
                    f.db.execute_raw(
                        "ALTER TABLE memories RENAME TO private_missing_endpoint_authority",
                    )
                    .map_err(|_| invalid())?;
                }
            }
            Ok(())
        })
        .unwrap_err();
        assert_eq!(edges, 2);
        assert!(
            !error
                .message()
                .contains("private_missing_endpoint_authority")
        );
        assert!(!error.message().contains(&anchor));
        assert!(f.db.begin_read_snapshot().is_err());
        f.db.execute_raw("ROLLBACK").unwrap();
        assert_eq!(endpoints(&f.db, &f.own, &pending).unwrap(), expected);
    }

    #[test]
    fn cancellation_between_edge_and_endpoint_reads_never_returns_a_pair() {
        crate::core::run_cli_with_cx(std::time::Duration::from_secs(30), |cx| async move {
            let f = Fixture::new();
            let first = f.memory(1, false);
            let second = f.memory(2, false);
            let pending = vec![pending_edge(&f, &first, &second, 1)];
            f.db.begin_read_snapshot().unwrap();
            let mut reads = 0;
            let result = endpoints_observed(&f.db, &f.own, &pending, |table| {
                assert_eq!(table, "memory_links");
                reads += 1;
                cx.cancel_with(asupersync::CancelKind::User, Some("cancel endpoint replay"));
                Ok(())
            });
            assert!(result.is_err());
            assert_eq!(reads, 1);
            let _uncancelled = asupersync::Cx::set_current(None);
            assert!(f.db.begin_read_snapshot().is_err());
            f.db.rollback_read_snapshot().unwrap();
            assert_eq!(endpoints(&f.db, &f.own, &pending).unwrap().len(), 1);
        })
        .unwrap();
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
    fn batch_failure_withholds_both_endpoints_and_allows_complete_retry() {
        let f = Fixture::new();
        let anchor = f.memory(1, false);
        let foreign = f.memory(50_000, true);
        let mut expected = Vec::new();
        let mut edges = Vec::new();
        for number in 1..=129_u128 {
            let target = f.memory(1000 + number, false);
            let edge = MemoryLinkId::from_uuid(uuid::Uuid::from_u128(number)).to_string();
            f.db.insert_memory_link(
                &edge,
                &crate::db::CreateMemoryLinkInput {
                    src_memory_id: anchor.clone(),
                    dst_memory_id: target.clone(),
                    relation: MemoryLinkRelation::Supports,
                    weight: 1.0,
                    confidence: 0.9,
                    directed: true,
                    evidence_count: 1,
                    last_reinforced_at: None,
                    source: MemoryLinkSource::Human,
                    created_by: None,
                    metadata_json: None,
                },
            )
            .expect("durable edge");
            let cursor = f.audit(&edge, "memory_link", audit_actions::MEMORY_LINK_CREATE);
            expected.extend([(cursor, anchor.clone()), (cursor, target.clone())]);
            edges.push((edge, target));
        }
        let repeat = f.audit(
            &edges[0].0,
            "memory_link",
            audit_actions::MEMORY_LINK_CREATE,
        );
        expected.extend([(repeat, anchor.clone()), (repeat, edges[0].1.clone())]);
        let original = f.poll(0, 200, None).expect("all batches");
        assert_eq!(
            original
                .invalidations
                .iter()
                .map(|n| (n.cursor, n.memory_id.clone()))
                .collect::<Vec<_>>(),
            expected
        );
        let (last, target) = edges.last().expect("late batch");
        f.db.execute_raw(&format!(
            "UPDATE memory_links SET dst_memory_id = '{foreign}' WHERE id = '{last}'"
        ))
        .expect("foreign endpoint in final batch");
        let error = f
            .poll(0, 200, None)
            .expect_err("no prefix may be acknowledged");
        let error = error.to_string();
        for private in [&anchor, &foreign, last, &f.other] {
            assert!(!error.contains(private));
        }
        f.db.begin_read_snapshot()
            .expect("failed poll released its own view");
        f.db.rollback_read_snapshot().unwrap();
        f.db.execute_raw(&format!(
            "UPDATE memory_links SET dst_memory_id = '{target}' WHERE id = '{last}'"
        ))
        .expect("repair authority");
        let retry = f
            .poll(0, 200, None)
            .expect("same cursor retries the complete page");
        assert_eq!(retry.invalidations, original.invalidations);
        assert_eq!(retry.next_cursor, original.next_cursor);
        assert_eq!(
            f.head(),
            repeat,
            "reads never append or rewrite audit history"
        );
    }

    #[test]
    fn absent_link_authority_refuses_partial_output_and_preserves_borrowed_transactions() {
        let f = Fixture::new();
        let first = f.memory(1, false);
        let second = f.memory(2, false);
        f.link(&first, &second);
        let before = f.poll(0, 100, None).unwrap();
        let transaction: crate::db::Result<()> = f.db.with_transaction(|| {
            f.db.execute_raw("ALTER TABLE memory_links RENAME TO private_missing_link_authority")?;
            // Do not create another snapshot inside the write transaction.
            // Source verification neither commits nor rolls back its caller.
            let row = f.db.query(
                "SELECT target_id FROM audit_log WHERE action = 'memory.link.create'",
                &[],
            )?;
            let edge = row[0].get(0).and_then(Value::as_str).expect("edge");
            let pending = vec![PendingLink {
                link_id: edge.to_owned(),
                event: routing_event(&f, 1),
            }];
            let error = notices(&f.db, &f.own, &pending, &SubscribeFilter::default(), None)
                .expect_err("missing link authority cannot produce empty success");
            assert!(!error.to_string().contains("private_missing_link_authority"));
            assert!(
                f.db.query("SELECT id FROM private_missing_link_authority", &[])
                    .is_ok()
            );
            f.db.execute_raw("INSERT INTO missing_link_transaction_rollback VALUES (1)")?;
            Ok(())
        });
        assert!(transaction.is_err());
        assert_eq!(
            f.poll(0, 100, None).unwrap().invalidations,
            before.invalidations
        );
        assert_eq!(f.head(), before.next_cursor);
    }

    fn routing_event(f: &Fixture, cursor: u64) -> MemoryDelta {
        MemoryDelta {
            schema: crate::core::subscribe::MEMORY_DELTA_SCHEMA_V1,
            cursor,
            kind: "updated".to_owned(),
            memory_id: String::new(),
            levels: Vec::new(),
            kinds: Vec::new(),
            tags: Vec::new(),
            workspace_id: Some(f.own.clone()),
            trust_class: None,
            agent_name: None,
            changed_fields: vec!["links".to_owned()],
            audit_id: "audit-test".to_owned(),
            occurred_at: "2026-10-20T00:00:00Z".to_owned(),
        }
    }

    #[test]
    fn memory_only_poll_does_not_depend_on_unused_link_authority() {
        let f = Fixture::new();
        let first = f.memory(1, false);
        let cursor = f.audit(&first, "memory", audit_actions::MEMORY_CREATE);
        f.db.execute_raw("ALTER TABLE memory_links RENAME TO unrelated_subscription_edges")
            .expect("missing optional source");
        let page = f
            .poll(0, 1, None)
            .expect("ordinary memory feed remains independent");
        assert_eq!(page.deltas.len(), 1);
        assert_eq!(page.deltas[0].memory_id, first);
        assert!(page.invalidations.is_empty());
        assert_eq!(page.next_cursor, cursor);
        assert!(!page.has_more);
    }

    #[test]
    fn hidden_relationships_do_not_leak_and_link_only_pages_never_hydrate_tags() {
        let f = Fixture::new();
        let first = f.memory(1, false);
        let second = f.memory(2, false);
        let report = f.link(&first, &second);
        let edge = report.link.as_ref().unwrap().link_id.as_deref().unwrap();
        let original = f.db.get_memory_link(edge).unwrap().unwrap();
        f.db.execute_raw(&format!("UPDATE memory_links SET metadata_json = '{{\"workspaceScopeDecision\":\"deny\",\"originWorkspaceId\":\"PRIVATE_ORIGIN\"}}' WHERE id = '{edge}'"))
            .expect("hidden mesh relationship");
        f.db.execute_raw("ALTER TABLE memory_tags RENAME TO private_unused_link_tags")
            .unwrap();
        let hidden = f.poll(0, 1, None).expect("no tag hydration for links");
        assert!(hidden.deltas.is_empty() && hidden.invalidations.is_empty());
        assert_eq!(hidden.next_cursor, f.head());
        let oversized = serde_json::json!({"note": "界".repeat(MAX_METADATA_BYTES)}).to_string();
        f.db.execute_raw(&format!(
            "UPDATE memory_links SET metadata_json = '{}' WHERE id = '{edge}'",
            oversized.replace('\'', "''")
        ))
        .unwrap();
        let error = f
            .poll(0, 1, None)
            .expect_err("oversized policy is not absent policy");
        assert!(!error.to_string().contains('界'));
        f.db.execute_raw(&format!(
            "UPDATE memory_links SET metadata_json = '{}' WHERE id = '{edge}'",
            original.metadata_json.unwrap().replace('\'', "''")
        ))
        .unwrap();
        let visible = f
            .poll(0, 1, Some("TAG=unknown,CHANGED_FIELDS=links"))
            .unwrap();
        assert!(visible.deltas.is_empty());
        assert_eq!(visible.invalidations.len(), 2);
        assert_eq!(visible.next_cursor, hidden.next_cursor);
    }

    #[test]
    fn a_pinned_page_cannot_acknowledge_a_link_committed_after_its_watermark() {
        let f = Fixture::new();
        let first = f.memory(1, false);
        let second = f.memory(2, false);
        let third = f.memory(3, false);
        f.link(&first, &second);
        let before = f.head();
        let reader = DbConnection::open(crate::db::DatabaseConfig::read_only_file(
            f.database.clone(),
        ))
        .unwrap();
        let snapshot = super::super::SubscriptionSnapshot::begin(&reader).unwrap();
        assert_eq!(snapshot.high_watermark(&f.own).unwrap(), before);
        f.link(&first, &third);
        let after = f.head();
        let pinned = snapshot
            .page(&f.own, 0, 100, &SubscribeFilter::default(), None)
            .unwrap();
        assert_eq!(pinned.next_cursor, before);
        assert_eq!(pinned.invalidations.len(), 2);
        assert!(!pinned.invalidations.iter().any(|n| n.memory_id == third));
        snapshot.finish().unwrap();
        let fresh = f.poll(before, 1, None).unwrap();
        assert_eq!(fresh.next_cursor, after);
        assert_eq!(fresh.invalidations.len(), 2);
        assert!(fresh.invalidations.iter().all(|n| n.cursor == after));
        assert!(fresh.invalidations.iter().any(|n| n.memory_id == third));
        assert!(f.poll(after, 1, None).unwrap().invalidations.is_empty());
    }

    #[test]
    fn cancellation_withholds_link_notices_without_releasing_a_callers_snapshot() {
        let f = Fixture::new();
        let first = f.memory(1, false);
        let second = f.memory(2, false);
        let report = f.link(&first, &second);
        let pending = vec![PendingLink {
            link_id: report.link.unwrap().link_id.unwrap(),
            event: routing_event(&f, f.head()),
        }];
        f.db.begin_read_snapshot().unwrap();
        let db = &f.db;
        let workspace = f.own.as_str();
        let result =
            crate::core::run_cli_with_cx(std::time::Duration::from_secs(30), |cx| async move {
                cx.cancel_with(
                    asupersync::CancelKind::User,
                    Some("stop subscription replay"),
                );
                notices(db, workspace, &pending, &SubscribeFilter::default(), None)
            })
            .expect("runtime");
        assert!(result.is_err());
        assert!(f.db.begin_read_snapshot().is_err());
        f.db.commit_read_snapshot()
            .expect("caller still owns its snapshot");
        assert_eq!(f.poll(0, 100, None).unwrap().invalidations.len(), 2);
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
