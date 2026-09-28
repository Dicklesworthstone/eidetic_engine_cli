//! Replay both identities changed by an immutable memory revision.
//!
//! The producer targets the successor in its one durable audit row. Replaying
//! that row alone leaves the predecessor cached forever. Keep the retirement
//! notice in the same cursor unit, and verify the referenced lineage against
//! the poll owner's snapshot before allowing either half to be acknowledged.

use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;

use chrono::{DateTime, Utc};
use serde::Deserialize;
use sqlmodel_core::Value;

use super::{MEMORY_INVALIDATION_SCHEMA_V1, MemoryDelta, MemoryInvalidation, SubscribeFilter};
use crate::db::DbConnection;
use crate::models::{DomainError, MemoryId};

pub(super) const MAX_DETAILS_BYTES: usize = 16 * 1024;
const SOURCE_BATCH_SIZE: usize = 256;

fn invalid() -> DomainError {
    DomainError::Storage {
        message: "Could not replay a complete immutable revision; no cursor was acknowledged"
            .to_owned(),
        repair: Some(
            "Keep the previous cursor. Run ee doctor --json and restore or repair the retained revision history before retrying; do not skip the failed event."
                .to_owned(),
        ),
    }
}

fn instant(value: &str) -> Result<DateTime<Utc>, DomainError> {
    DateTime::parse_from_rfc3339(value)
        .map(|at| at.with_timezone(&Utc))
        .map_err(|_| invalid())
}

// Deriving this typed projection rejects duplicate known fields, including
// escaped spellings. Unknown producer annotations (reason, actor, etc.) are
// ignored, never copied into a retirement notice or a parsing diagnostic.
#[derive(Deserialize)]
struct RevisionDetails {
    from_id: String,
    to_id: String,
    logical_id: String,
    revised_at: String,
    changed_fields: Vec<String>,
}

pub(super) struct PendingRevision {
    details: RevisionDetails,
    at: DateTime<Utc>,
    event: MemoryDelta,
}

/// Decode only a bounded, unchanged audit value. The SQL reader also caps the
/// value before materialization; truncating JSON would change its meaning.
/// Native revision fields, rather than the generic update approximation, own
/// routing for tag-only, level-only, typed-data and seal-state transitions.
pub(super) fn collect(
    event: &mut MemoryDelta,
    details: Option<&str>,
    pending: &mut Vec<PendingRevision>,
) -> Result<(), DomainError> {
    let details = details
        .filter(|raw| raw.len() <= MAX_DETAILS_BYTES)
        .ok_or_else(invalid)?;
    let details: RevisionDetails = serde_json::from_str(details).map_err(|_| invalid())?;
    if details.to_id != event.memory_id
        || details.from_id == details.to_id
        || [&details.from_id, &details.to_id, &details.logical_id]
            .into_iter()
            .any(|id| MemoryId::from_str(id).is_err())
        || details.changed_fields.is_empty()
        || details.changed_fields.len() > 16
    {
        return Err(invalid());
    }
    let at = instant(&details.revised_at)?;
    let mut fields = BTreeSet::new();
    for field in &details.changed_fields {
        let field = match field.as_str() {
            "content" => "content_hash",
            "level" | "kind" | "confidence" | "tags" | "provenance_uri" | "typed_fields"
            | "seal_state" | "trust_class" => field.as_str(),
            _ => return Err(invalid()),
        };
        fields.insert(field.to_owned());
    }
    event.changed_fields = fields.into_iter().collect();
    pending.push(PendingRevision {
        details,
        at,
        event: event.clone(),
    });
    Ok(())
}

struct RevisionSource {
    workspace: String,
    logical_id: String,
    superseded_at: Option<String>,
    valid_from: Option<String>,
}

fn text(row: &sqlmodel_core::Row, index: usize) -> Result<String, DomainError> {
    row.get(index)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(invalid)
}

fn optional_text(row: &sqlmodel_core::Row, index: usize) -> Result<Option<String>, DomainError> {
    match row.get(index) {
        Some(Value::Null) => Ok(None),
        Some(_) => text(row, index).map(Some),
        None => Err(invalid()),
    }
}

fn sources(
    db: &DbConnection,
    workspace: &str,
    pending: &[PendingRevision],
) -> Result<BTreeMap<String, RevisionSource>, DomainError> {
    let ids: BTreeSet<&str> = pending
        .iter()
        .flat_map(|revision| {
            [
                revision.details.from_id.as_str(),
                revision.details.to_id.as_str(),
            ]
        })
        .collect();
    let ids: Vec<_> = ids.into_iter().collect();
    let mut result = BTreeMap::new();
    for batch in ids.chunks(SOURCE_BATCH_SIZE) {
        let slots = (2..=batch.len() + 1)
            .map(|index| format!("?{index}"))
            .collect::<Vec<_>>();
        let mut parameters = vec![Value::Text(workspace.to_owned())];
        parameters.extend(batch.iter().map(|id| Value::Text((*id).to_owned())));
        let sql = format!(
            "SELECT id, workspace_id, COALESCE(logical_id, id), superseded_at, valid_from \
             FROM memories WHERE workspace_id = ?1 AND id IN ({})",
            slots.join(",")
        );
        for row in db.query(&sql, &parameters).map_err(|_| invalid())? {
            let id = text(&row, 0)?;
            let source = RevisionSource {
                workspace: text(&row, 1)?,
                logical_id: text(&row, 2)?,
                superseded_at: optional_text(&row, 3)?,
                valid_from: optional_text(&row, 4)?,
            };
            if result.insert(id, source).is_some() {
                return Err(invalid());
            }
        }
    }
    Ok(result)
}

/// The caller owns the read snapshot and the raw page limit. This helper never
/// starts/releases a transaction, reads lookahead, or acknowledges a cursor.
/// Later revisions may have retired the successor too: validate this edge's
/// boundary, not whether the successor is still today's head.
pub(super) fn retirement_notices(
    db: &DbConnection,
    workspace: &str,
    pending: &[PendingRevision],
    filter: &SubscribeFilter,
    since_cutoff: Option<DateTime<Utc>>,
) -> Result<Vec<MemoryInvalidation>, DomainError> {
    let sources = sources(db, workspace, pending)?;
    let routing = SubscribeFilter {
        workspace_ids: filter.workspace_ids.clone(),
        changed_fields: filter.changed_fields.clone(),
        since_ms: filter.since_ms,
        ..SubscribeFilter::default()
    };
    let mut notices = Vec::new();
    for revision in pending {
        let prior = sources.get(&revision.details.from_id).ok_or_else(invalid)?;
        let next = sources.get(&revision.details.to_id).ok_or_else(invalid)?;
        if prior.workspace != workspace
            || next.workspace != workspace
            || prior.logical_id != revision.details.logical_id
            || next.logical_id != revision.details.logical_id
            || instant(prior.superseded_at.as_deref().ok_or_else(invalid)?)? != revision.at
            || instant(next.valid_from.as_deref().ok_or_else(invalid)?)? != revision.at
        {
            return Err(invalid());
        }
        let mut predecessor = revision.event.clone();
        predecessor.memory_id.clone_from(&revision.details.from_id);
        predecessor.changed_fields.push("superseded_at".to_owned());
        predecessor.changed_fields.sort();
        // Old filter membership is not reconstructible from today's metadata.
        // Conservatively evict the old identity without revealing its metadata.
        // Explicit workspace/time/changed-field routing is still honored.
        if !routing.matches_delta(&predecessor, since_cutoff) {
            continue;
        }
        notices.push(MemoryInvalidation {
            schema: MEMORY_INVALIDATION_SCHEMA_V1,
            cursor: predecessor.cursor,
            memory_id: predecessor.memory_id,
            workspace_id: predecessor.workspace_id,
            audit_id: predecessor.audit_id,
            occurred_at: predecessor.occurred_at,
            changed_fields: predecessor.changed_fields,
            affected_filters: Vec::new(),
            reason: "revision_superseded",
        });
    }
    Ok(notices)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::*;
    use crate::core::memory::{ReviseMemoryOptions, ReviseReason, revise_memory};
    use crate::core::subscribe::{
        MEMORY_DELTA_SCHEMA_V1, SubscribePollOptions, SubscribePollReport, parse_subscribe_filter,
        poll_memory_deltas,
    };
    use crate::core::workspace::stable_workspace_id;
    use crate::db::{CreateAuditInput, CreateMemoryInput, CreateWorkspaceInput, DatabaseConfig};
    use serde_json::json;
    use std::path::PathBuf;

    fn id(number: u128) -> String {
        MemoryId::from_uuid(uuid::Uuid::from_u128(number)).to_string()
    }

    fn details(from: &str, to: &str, group: &str, at: &str, fields: &[&str]) -> serde_json::Value {
        json!({
            "from_id": from, "to_id": to, "logical_id": group,
            "revised_at": at, "changed_fields": fields,
            "reason": "PRIVATE_REVISION_REASON", "actor": "PRIVATE_REVISION_ACTOR"
        })
    }

    fn event() -> MemoryDelta {
        MemoryDelta {
            schema: MEMORY_DELTA_SCHEMA_V1,
            cursor: 7,
            kind: "updated".to_owned(),
            memory_id: id(2),
            levels: vec!["procedural".to_owned()],
            kinds: vec!["rule".to_owned()],
            tags: Vec::new(),
            workspace_id: Some("workspace".to_owned()),
            trust_class: Some("agent_validated".to_owned()),
            agent_name: Some("PRIVATE_REVISION_ACTOR".to_owned()),
            changed_fields: vec!["content_hash".to_owned()],
            audit_id: "audit-fixture".to_owned(),
            occurred_at: "2026-09-01T00:00:00Z".to_owned(),
        }
    }

    #[test]
    fn revision_fields_come_from_the_recorded_transition_not_generic_update_defaults() {
        let mut event = event();
        let mut pending = Vec::new();
        let value = details(
            &id(1),
            &id(2),
            &id(1),
            "2026-09-01T00:00:00Z",
            &["tags", "level", "typed_fields", "seal_state", "tags"],
        );
        collect(&mut event, Some(&value.to_string()), &mut pending).unwrap();
        assert_eq!(
            event.changed_fields,
            ["level", "seal_state", "tags", "typed_fields"]
        );
        assert_eq!(pending.len(), 1);
        assert!(
            !event
                .changed_fields
                .iter()
                .any(|field| field == "content_hash")
        );
        let value = details(&id(1), &id(2), &id(1), "2026-09-01T00:00:00Z", &["content"]);
        collect(&mut event, Some(&value.to_string()), &mut pending).unwrap();
        assert_eq!(event.changed_fields, ["content_hash"]);
    }

    #[test]
    fn malformed_ambiguous_and_oversized_revision_details_never_acknowledge_or_leak() {
        let valid = details(&id(1), &id(2), &id(1), "2026-09-01T00:00:00Z", &["tags"]);
        let raw = valid.to_string();
        let mut bad = vec![
            None,
            Some("{}".to_owned()),
            Some(format!("{raw} trailing")),
            Some(format!("{raw}\0")),
            Some(format!("{raw}{}", " ".repeat(MAX_DETAILS_BYTES))),
            Some(format!(r#"{{"from_id":"{}",{}"#, id(1), &raw[1..])),
            Some(format!(r#"{{"from\u005fid":"{}",{}"#, id(1), &raw[1..])),
        ];
        for (key, value) in [
            ("from_id", json!(id(2))),
            ("to_id", json!(id(3))),
            ("logical_id", json!("PRIVATE_INVALID_ID")),
            ("revised_at", json!("PRIVATE_INVALID_TIME")),
            ("changed_fields", json!([])),
            ("changed_fields", json!(["PRIVATE_INVALID_FIELD"])),
            ("changed_fields", json!(vec!["tags"; 17])),
        ] {
            let mut candidate = valid.clone();
            candidate[key] = value;
            bad.push(Some(candidate.to_string()));
        }
        for raw in bad {
            let mut event = event();
            let before = event.clone();
            let mut pending = Vec::new();
            let error = collect(&mut event, raw.as_deref(), &mut pending).unwrap_err();
            assert!(error.to_string().contains("no cursor was acknowledged"));
            assert!(!error.to_string().contains("PRIVATE_"));
            assert!(pending.is_empty());
            assert_eq!(event, before);
        }
    }

    struct Fixture {
        _root: tempfile::TempDir,
        workspace: PathBuf,
        path: PathBuf,
        own: String,
        db: DbConnection,
    }

    impl Fixture {
        fn new() -> Self {
            let root = tempfile::tempdir().unwrap();
            let workspace = root.path().canonicalize().unwrap();
            std::fs::create_dir(workspace.join(".ee")).unwrap();
            let path = workspace.join(".ee/ee.db");
            let db = DbConnection::open_file(&path).unwrap();
            db.migrate().unwrap();
            let own = stable_workspace_id(&workspace);
            db.insert_workspace(
                &own,
                &CreateWorkspaceInput {
                    path: workspace.to_string_lossy().into_owned(),
                    name: None,
                },
            )
            .unwrap();
            Self {
                _root: root,
                workspace,
                path,
                own,
                db,
            }
        }

        fn input(&self, level: &str, tag: &str) -> CreateMemoryInput {
            CreateMemoryInput {
                workspace_id: self.own.clone(),
                level: level.to_owned(),
                kind: "rule".to_owned(),
                content: "Run the release checks before publishing.".to_owned(),
                workflow_id: None,
                confidence: 0.8,
                utility: 0.5,
                importance: 0.5,
                provenance_uri: Some("manual://subscribe-revision".to_owned()),
                trust_class: "agent_validated".to_owned(),
                trust_subclass: None,
                tags: vec![tag.to_owned()],
                valid_from: None,
                valid_to: None,
            }
        }

        fn seed(&self, number: u128) -> String {
            let memory = id(number);
            self.db
                .insert_memory_revision_at(
                    &memory,
                    &memory,
                    &self.input("procedural", "release"),
                    instant("2026-08-31T00:00:00Z").unwrap(),
                )
                .unwrap();
            memory
        }

        fn head(&self) -> u64 {
            let rows = self
                .db
                .query("SELECT COALESCE(MAX(rowid), 0) FROM audit_log", &[])
                .unwrap();
            u64::try_from(rows[0].get(0).and_then(Value::as_i64).unwrap()).unwrap()
        }

        fn audit(&self, to: &str, value: &serde_json::Value) -> u64 {
            self.db
                .insert_audit(
                    &crate::db::generate_audit_id(),
                    &CreateAuditInput {
                        workspace_id: Some(self.own.clone()),
                        actor: Some("PRIVATE_REVISION_ACTOR".to_owned()),
                        action: crate::db::audit_actions::MEMORY_REVISE.to_owned(),
                        target_type: Some("memory".to_owned()),
                        target_id: Some(to.to_owned()),
                        details: Some(value.to_string()),
                    },
                )
                .unwrap();
            self.head()
        }

        fn transition(&self, from: &str, number: u128, level: &str) -> (String, u64) {
            let to = id(number);
            let at = instant("2026-09-01T00:00:00Z").unwrap()
                + chrono::TimeDelta::seconds(i64::try_from(number).unwrap());
            let group = self.db.get_memory_logical_id(from).unwrap().unwrap();
            let original = self.db.get_memory(from).unwrap().unwrap();
            let mut fields = vec!["tags"];
            if original.level != level {
                fields.push("level");
            }
            let mut input = self.input(level, "new-tag");
            input.valid_from = Some(at.to_rfc3339());
            self.db
                .with_transaction(|| {
                    self.db.insert_memory_revision_at(&to, &group, &input, at)?;
                    assert!(self.db.mark_memory_superseded(from, &at.to_rfc3339())?);
                    Ok(())
                })
                .unwrap();
            let cursor = self.audit(&to, &details(from, &to, &group, &at.to_rfc3339(), &fields));
            (to, cursor)
        }

        fn poll(
            &self,
            cursor: u64,
            limit: u32,
            filter: Option<&str>,
        ) -> Result<SubscribePollReport, DomainError> {
            poll_memory_deltas(&SubscribePollOptions {
                workspace_path: &self.workspace,
                database_path: Some(&self.path),
                cursor,
                limit,
                filter: parse_subscribe_filter(filter).unwrap(),
            })
        }
    }

    #[test]
    fn revision_replay_retires_the_prior_identity_and_routes_the_successor_together() {
        let f = Fixture::new();
        let from = f.seed(1);
        let (to, cursor) = f.transition(&from, 2, "semantic");
        let report = f.poll(0, 1, None).unwrap();
        assert_eq!(report.delta_count, 1);
        assert_eq!(report.deltas[0].memory_id, to);
        assert_eq!(report.deltas[0].changed_fields, ["level", "tags"]);
        assert_eq!(report.invalidations.len(), 1);
        let notice = &report.invalidations[0];
        assert_eq!(notice.memory_id, from);
        assert_eq!(notice.reason, "revision_superseded");
        assert_eq!(notice.changed_fields, ["level", "superseded_at", "tags"]);
        assert_eq!(notice.cursor, cursor);
        assert_eq!(report.deltas[0].cursor, cursor);
        assert_eq!(report.next_cursor, cursor);
        assert!(!report.has_more);
        let output = serde_json::to_value(notice).unwrap();
        assert!(!output.to_string().contains("PRIVATE_"));
        for field in [
            "tags",
            "levels",
            "kinds",
            "agentName",
            "trustClass",
            "content",
        ] {
            assert!(output.get(field).is_none());
        }
        let filtered = f
            .poll(0, 1, Some("TAG=release,CHANGED_FIELDS=tags"))
            .unwrap();
        assert!(filtered.deltas.is_empty());
        assert_eq!(filtered.invalidations.len(), 2);
        assert!(
            filtered
                .invalidations
                .iter()
                .any(|entry| entry.memory_id == from)
        );
        assert!(
            filtered
                .invalidations
                .iter()
                .any(|entry| entry.memory_id == to)
        );
        assert_eq!(filtered.next_cursor, cursor);
        assert!(f.poll(cursor, 1, None).unwrap().invalidations.is_empty());
    }

    #[test]
    fn filtered_retirements_do_not_need_tags_or_broaden_explicit_routing() {
        let f = Fixture::new();
        let from = f.seed(1);
        f.transition(&from, 2, "semantic");
        f.db.execute_raw("ALTER TABLE memory_tags RENAME TO temporarily_hidden_tags")
            .unwrap();
        let report = f
            .poll(
                0,
                10,
                Some("LEVEL=procedural,TAG=release,CHANGED_FIELDS=tags"),
            )
            .unwrap();
        assert!(report.deltas.is_empty());
        assert_eq!(report.invalidations.len(), 2);
        for filter in [
            "WORKSPACE_ID=not-this-workspace",
            "CHANGED_FIELDS=confidence",
        ] {
            let report = f.poll(0, 10, Some(filter)).unwrap();
            assert!(report.deltas.is_empty());
            assert!(report.invalidations.is_empty());
        }
        f.db.execute_raw("ALTER TABLE temporarily_hidden_tags RENAME TO memory_tags")
            .unwrap();
        let reader = DbConnection::open(DatabaseConfig::read_only_file(f.path.clone())).unwrap();
        let snapshot = super::super::SubscriptionSnapshot::begin(&reader).unwrap();
        let page = snapshot
            .page(
                &f.own,
                0,
                10,
                &SubscribeFilter::default(),
                Some(instant("9999-01-01T00:00:00Z").unwrap()),
            )
            .unwrap();
        assert!(page.deltas.is_empty());
        assert!(page.invalidations.is_empty());
        snapshot.finish().unwrap();
    }

    #[test]
    fn later_revisions_and_one_row_pages_do_not_split_or_invalidate_earlier_cursor_units() {
        let f = Fixture::new();
        let original = f.seed(1);
        let (middle, first) = f.transition(&original, 2, "procedural");
        let (latest, second) = f.transition(&middle, 3, "semantic");
        let page = f.poll(0, 1, None).unwrap();
        assert!(page.has_more);
        assert_eq!(page.next_cursor, first);
        assert_eq!(page.high_watermark, second);
        assert_eq!(page.deltas[0].memory_id, middle);
        assert_eq!(page.invalidations[0].memory_id, original);
        let page = f.poll(first, 1, None).unwrap();
        assert!(!page.has_more);
        assert_eq!(page.next_cursor, second);
        assert_eq!(page.deltas[0].memory_id, latest);
        assert_eq!(page.invalidations[0].memory_id, middle);
    }

    #[test]
    fn a_failed_tag_read_withholds_both_halves_and_retry_replays_the_same_revision() {
        let f = Fixture::new();
        let from = f.seed(1);
        let (to, cursor) = f.transition(&from, 2, "procedural");
        f.db.execute_raw("ALTER TABLE memory_tags RENAME TO temporarily_hidden_tags")
            .unwrap();
        let error = f.poll(0, 10, None).unwrap_err();
        assert!(error.to_string().contains("no cursor was acknowledged"));
        f.db.execute_raw("ALTER TABLE temporarily_hidden_tags RENAME TO memory_tags")
            .unwrap();
        let report = f.poll(0, 10, None).unwrap();
        assert_eq!(report.deltas[0].memory_id, to);
        assert_eq!(report.invalidations[0].memory_id, from);
        assert_eq!(report.next_cursor, cursor);
    }

    #[test]
    fn false_lineage_and_foreign_predecessors_cannot_authorize_retirement() {
        for foreign in [false, true] {
            let f = Fixture::new();
            let from = f.seed(1);
            let (to, cursor) = f.transition(&from, 2, "procedural");
            let forged = f.seed(3);
            if foreign {
                let elsewhere = f.workspace.join("elsewhere");
                let owner = stable_workspace_id(&elsewhere);
                f.db.insert_workspace(
                    &owner,
                    &CreateWorkspaceInput {
                        path: elsewhere.to_string_lossy().into_owned(),
                        name: None,
                    },
                )
                .unwrap();
                f.db.execute_raw(&format!(
                    "UPDATE memories SET workspace_id = '{owner}' WHERE id = '{forged}'"
                ))
                .unwrap();
            }
            f.audit(
                &to,
                &details(&forged, &to, &from, "2026-09-01T00:00:02Z", &["tags"]),
            );
            let error = f.poll(cursor, 1, None).unwrap_err();
            assert!(error.to_string().contains("no cursor was acknowledged"));
            assert!(!error.to_string().contains(&forged));
            assert!(!error.to_string().contains("PRIVATE_"));
        }
    }

    #[test]
    fn revision_lineage_is_read_from_the_same_snapshot_as_its_audit_page() {
        let f = Fixture::new();
        let from = f.seed(1);
        let (to, cursor) = f.transition(&from, 2, "procedural");
        let reader = DbConnection::open(DatabaseConfig::read_only_file(f.path.clone())).unwrap();
        let snapshot = super::super::SubscriptionSnapshot::begin(&reader).unwrap();
        assert_eq!(snapshot.high_watermark(&f.own).unwrap(), cursor);
        f.db.execute_raw(&format!(
            "UPDATE memories SET logical_id = id WHERE id = '{to}'"
        ))
        .unwrap();
        let page = snapshot
            .page(&f.own, 0, 10, &SubscribeFilter::default(), None)
            .unwrap();
        assert_eq!(page.invalidations[0].memory_id, from);
        snapshot.finish().unwrap();
        assert!(
            f.poll(0, 10, None).is_err(),
            "fresh reads must reject detached lineage"
        );
    }

    #[test]
    fn real_memory_revise_publishes_a_replayable_pair_with_precise_changed_fields() {
        let f = Fixture::new();
        let from = f.seed(1);
        let cursor = f.head();
        let revision = revise_memory(&ReviseMemoryOptions {
            database_path: &f.path,
            original_memory_id: &from,
            content: None,
            level: None,
            kind: None,
            confidence: Some(0.6),
            tags: Some(vec!["release-v2".to_owned()]),
            provenance_uri: None,
            reason: ReviseReason::Correction,
            actor: Some("revision-test"),
            dry_run: false,
        });
        assert!(revision.success, "{:?}", revision.error);
        let to = revision.new_id.unwrap();
        let page = f
            .poll(cursor, 100, Some("CHANGED_FIELDS=tags,TAG=release-v2"))
            .unwrap();
        let delta = page
            .deltas
            .iter()
            .find(|delta| delta.memory_id == to)
            .unwrap();
        let notice = page
            .invalidations
            .iter()
            .find(|notice| notice.memory_id == from)
            .unwrap();
        assert_eq!(delta.changed_fields, ["confidence", "tags"]);
        assert_eq!(notice.reason, "revision_superseded");
        assert_eq!(delta.cursor, notice.cursor);
        assert!(
            f.db.get_memory(&from).unwrap().is_some(),
            "history is not deleted"
        );
    }

    #[test]
    fn revision_source_hydration_crosses_a_batch_boundary_without_losing_pairs() {
        let f = Fixture::new();
        for pair in 0..129 {
            let from = f.seed(1000 + pair * 2);
            f.transition(&from, 1001 + pair * 2, "procedural");
        }
        let page = f.poll(0, 129, None).unwrap();
        assert_eq!(page.deltas.len(), 129);
        assert_eq!(page.invalidations.len(), 129);
        assert!(!page.has_more);
        assert_eq!(page.next_cursor, page.high_watermark);
        for (delta, notice) in page.deltas.iter().zip(&page.invalidations) {
            assert_eq!(delta.cursor, notice.cursor);
            assert_ne!(delta.memory_id, notice.memory_id);
        }
    }
}
