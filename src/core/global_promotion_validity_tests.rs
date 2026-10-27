//! Capture-to-promotion tests for independently learned exact twins.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::{Path, PathBuf};

use super::super::{PromoteGlobalOptions, admission, persist_global_promotion, promote_global};
use super::*;
use crate::core::global_store::{GlobalStorePaths, open_or_create_global_store};
use crate::core::memory::{
    RememberMemoryOptions, prepare_remember_txn_write_for_connection,
    record_prepared_remember_txn_write_in_txn,
};
use crate::db::{CreateAuditInput, CreateWorkspaceInput, generate_audit_id};

const BODY: &str = "Always run the schema drift gate before altering public JSON.";
const AUTHORED_START: &str = "2000-01-01T00:00:00Z";

struct Capture {
    path: PathBuf,
    db: DbConnection,
    memory: StoredMemory,
}

fn remember(
    db: &DbConnection,
    workspace: &Path,
    database: &Path,
    valid_from: Option<&str>,
    valid_to: Option<&str>,
) -> StoredMemory {
    std::fs::create_dir_all(workspace.join(".ee")).unwrap();
    std::fs::write(
        workspace.join(".ee/config.toml"),
        "[memory]\ninclude_global = false\n",
    )
    .unwrap();
    let prepared = prepare_remember_txn_write_for_connection(
        db,
        &RememberMemoryOptions {
            workspace_path: workspace,
            database_path: Some(database),
            content: BODY,
            workflow_id: None,
            level: "procedural",
            kind: "rule",
            tags: None,
            confidence: 0.9,
            source: None,
            allow_secret_mention: false,
            valid_from,
            valid_to,
            dry_run: false,
            auto_link: false,
            propose_candidates: false,
        },
        true,
    )
    .unwrap();
    db.with_transaction(|| record_prepared_remember_txn_write_in_txn(db, &prepared))
        .unwrap();
    db.get_memory(prepared.memory_id()).unwrap().unwrap()
}

impl Capture {
    fn new(root: &Path, name: &str, from: Option<&str>, to: Option<&str>) -> Self {
        let workspace = root.join(name);
        std::fs::create_dir_all(workspace.join(".ee")).unwrap();
        let workspace = workspace.canonicalize().unwrap();
        let path = workspace.join(".ee/ee.db");
        let db = DbConnection::open_file(&path).unwrap();
        db.migrate().unwrap();
        db.insert_workspace(
            &crate::core::workspace::stable_workspace_id(&workspace),
            &CreateWorkspaceInput {
                path: workspace.display().to_string(),
                name: None,
            },
        )
        .unwrap();
        let memory = remember(&db, &workspace, &path, from, to);
        Self { path, db, memory }
    }

    fn options<'a>(
        &'a self,
        global: &'a GlobalStorePaths,
        dry_run: bool,
    ) -> PromoteGlobalOptions<'a> {
        PromoteGlobalOptions {
            workspace_database_path: &self.path,
            memory_id: &self.memory.id,
            global_paths: global,
            global_lane_available: true,
            actor: Some("capture-validity-test"),
            dry_run,
        }
    }

    fn publish(
        &self,
        global: &GlobalStorePaths,
        db: &DbConnection,
        workspace: &str,
    ) -> (String, bool, Option<String>) {
        let (source, payload, plan) =
            admission::load_source(&self.options(global, true), Utc::now()).unwrap();
        assert!(plan.allowed(), "{}", plan.data_json());
        persist_global_promotion(db, workspace, &source, &payload, None, Utc::now()).unwrap()
    }
}

struct Fixture {
    _root: tempfile::TempDir,
    first: Capture,
    second: Capture,
    paths: GlobalStorePaths,
    db: DbConnection,
    workspace: String,
    target: StoredMemory,
}

impl Fixture {
    fn new(first_end: Option<&str>, second_end: Option<&str>) -> Self {
        let root = tempfile::tempdir().unwrap();
        let physical = root.path().canonicalize().unwrap();
        let first = Capture::new(&physical, "first", None, first_end);
        let paths = GlobalStorePaths::from_root(&physical.join("global"));
        let (db, workspace) = open_or_create_global_store(&paths).unwrap();
        let (id, duplicate, _) = first.publish(&paths, &db, &workspace);
        assert!(!duplicate);
        let target = db.get_memory(&id).unwrap().unwrap();
        let second = Capture::new(&physical, "second", None, second_end);
        assert_ne!(
            instant(first.memory.valid_from.as_deref().unwrap()),
            instant(second.memory.valid_from.as_deref().unwrap()),
            "the regression needs independently captured starts"
        );
        Self {
            _root: root,
            first,
            second,
            paths,
            db,
            workspace,
            target,
        }
    }

    fn preview(&self) -> Option<String> {
        let report = promote_global(&self.second.options(&self.paths, true)).unwrap();
        assert!(report.plan.allowed() && !report.executed);
        report.global_memory_id
    }
}

fn audit_rows(db: &DbConnection, memory: &StoredMemory, action: &str) -> Vec<(String, Value)> {
    use sqlmodel_core::Value as DbValue;
    db.query(
        "SELECT id, details FROM audit_log WHERE workspace_id = ?1 AND action = ?2 AND target_type = 'memory' AND target_id = ?3 ORDER BY id ASC",
        &[
            DbValue::Text(memory.workspace_id.clone()),
            DbValue::Text(action.to_owned()),
            DbValue::Text(memory.id.clone()),
        ],
    )
    .unwrap()
    .iter()
    .map(|row| {
        (
            row.get(0).unwrap().as_str().unwrap().to_owned(),
            serde_json::from_str(row.get(1).unwrap().as_str().unwrap()).unwrap(),
        )
    })
    .collect()
}

fn replace_details(db: &DbConnection, audit_id: &str, details: &Value) {
    replace_raw_details(db, audit_id, &details.to_string());
}

/// Simulate imported legacy or corrupt evidence only in the private fixture.
/// Normal audit rows are append-only; restore the exact trigger in the same
/// transaction so every promotion still runs with normal storage protections.
fn replace_raw_details(db: &DbConnection, audit_id: &str, details: &str) {
    let trigger = db
        .query(
            "SELECT sql FROM sqlite_master WHERE type = 'trigger' AND name = 'audit_log_no_update'",
            &[],
        )
        .unwrap();
    let trigger = trigger[0].get(0).unwrap().as_str().unwrap();
    db.with_transaction(|| {
        db.execute_raw("DROP TRIGGER audit_log_no_update")?;
        db.execute_raw(&format!(
            "UPDATE audit_log SET details = '{}' WHERE id = '{}'",
            details.replace('\'', "''"),
            audit_id.replace('\'', "''")
        ))?;
        db.execute_raw(trigger)?;
        Ok(())
    })
    .unwrap();
}

#[test]
fn independent_remember_captures_merge_without_rewriting_rows_or_losing_audits() {
    let root = tempfile::tempdir().unwrap();
    let physical = root.path().canonicalize().unwrap();
    let paths = GlobalStorePaths::from_root(&physical.join("global"));
    let first = Capture::new(&physical, "first", None, None);
    let initial = promote_global(&first.options(&paths, false)).unwrap();
    assert!(initial.executed && !initial.already_promoted);
    let global_id = initial.global_memory_id.unwrap();
    let (db, workspace) = open_or_create_global_store(&paths).unwrap();
    let original = db.get_memory(&global_id).unwrap().unwrap();
    let second = Capture::new(&physical, "second", None, None);
    assert_ne!(first.memory.valid_from, second.memory.valid_from);
    assert_ne!(original.created_at, first.memory.created_at);

    let preview = promote_global(&second.options(&paths, true)).unwrap();
    assert!(!preview.executed && preview.already_promoted);
    assert_eq!(
        preview.global_memory_id.as_deref(),
        Some(global_id.as_str())
    );
    for _ in 0..2 {
        let report = promote_global(&second.options(&paths, false)).unwrap();
        assert!(report.executed && report.already_promoted);
        assert_eq!(report.global_memory_id.as_deref(), Some(global_id.as_str()));
    }
    assert_eq!(db.get_memory(&global_id).unwrap(), Some(original.clone()));
    assert_eq!(
        first.db.get_memory(&first.memory.id).unwrap(),
        Some(first.memory.clone())
    );
    assert_eq!(
        second.db.get_memory(&second.memory.id).unwrap(),
        Some(second.memory.clone())
    );
    let rows = db
        .query(
            "SELECT id FROM memories WHERE workspace_id = ?1",
            &[sqlmodel_core::Value::Text(workspace)],
        )
        .unwrap();
    assert_eq!(rows.len(), 1);
    let global_receipts = audit_rows(&db, &original, "memory.promote_global");
    assert_eq!(global_receipts.len(), 3);
    for capture in [&first, &second] {
        assert!(global_receipts.iter().any(|(_, details)| {
            details["originWorkspaceId"] == capture.memory.workspace_id
                && details["originMemoryId"] == capture.memory.id
        }));
        let origin_receipts = audit_rows(&capture.db, &capture.memory, "memory.promote_global");
        assert!(!origin_receipts.is_empty());
        assert!(origin_receipts.iter().all(|(_, details)| {
            details["globalMemoryId"] == global_id && details["originMemoryId"] == capture.memory.id
        }));
    }
    assert_eq!(
        original.provenance_uri,
        Some(super::super::promotion_provenance_uri(
            &first.memory.workspace_id,
            &first.memory.id
        ))
    );
}

#[test]
fn source_proof_requires_positive_omission_and_current_identity_content_and_bounds() {
    let f = Fixture::new(None, None);
    assert_eq!(f.preview(), Some(f.target.id.clone()));
    let (audit_id, original) =
        audit_rows(&f.second.db, &f.second.memory, "memory.create").remove(0);
    let original_input = &original["captureValidityInput"];
    assert!(original_input.get("validFromInput").unwrap().is_null());
    assert_eq!(original_input["contentHash"], content_hash(BODY));
    assert_eq!(original_input["createdAt"], f.second.memory.created_at);

    for case in [
        "legacy",
        "missing_start",
        "explicit_creation_start",
        "bad_hash",
        "bad_creation",
        "missing_end",
        "changed_end",
        "wrong_memory",
    ] {
        let mut changed = original.clone();
        match case {
            "legacy" => {
                changed
                    .as_object_mut()
                    .unwrap()
                    .remove("captureValidityInput");
            }
            "missing_start" => {
                changed["captureValidityInput"]
                    .as_object_mut()
                    .unwrap()
                    .remove("validFromInput");
            }
            "explicit_creation_start" => {
                changed["captureValidityInput"]["validFromInput"] =
                    Value::String(f.second.memory.created_at.clone())
            }
            "bad_hash" => {
                changed["captureValidityInput"]["contentHash"] =
                    Value::String(content_hash("different content"))
            }
            "bad_creation" => {
                changed["captureValidityInput"]["createdAt"] =
                    Value::String(AUTHORED_START.to_owned())
            }
            "missing_end" => {
                changed["captureValidityInput"]
                    .as_object_mut()
                    .unwrap()
                    .remove("validToInput");
            }
            "changed_end" => {
                changed["captureValidityInput"]["validToInput"] =
                    Value::String("2099-01-01T00:00:00Z".to_owned())
            }
            "wrong_memory" => changed["memoryId"] = Value::String(f.first.memory.id.clone()),
            _ => unreachable!(),
        }
        replace_details(&f.second.db, &audit_id, &changed);
        assert_eq!(f.preview(), None, "{case}");
    }
    replace_details(&f.second.db, &audit_id, &original);
    assert_eq!(f.preview(), Some(f.target.id.clone()));
    // Validation may legitimately upgrade trust after capture. Current trust
    // still has to match, but omitted validity remains an omitted validity.
    f.second
        .db
        .execute_raw(&format!(
            "UPDATE memories SET trust_class = 'agent_validated' WHERE id = '{}'",
            f.second.memory.id
        ))
        .unwrap();
    assert_eq!(f.preview(), None);
    f.db.execute_raw(&format!(
        "UPDATE memories SET trust_class = 'agent_validated' WHERE id = '{}'",
        f.target.id
    ))
    .unwrap();
    assert_eq!(f.preview(), Some(f.target.id.clone()));
}

#[test]
fn destination_proof_requires_its_unique_original_insertion_and_provenance() {
    let f = Fixture::new(None, None);
    assert_eq!(f.preview(), Some(f.target.id.clone()));
    let (audit_id, original) = audit_rows(&f.db, &f.target, "memory.promote_global").remove(0);
    // A valid corroboration receipt carries exactly the same source evidence,
    // but must never become a substitute for the original insertion receipt.
    let (_, duplicate, _) = f.first.publish(&f.paths, &f.db, &f.workspace);
    assert!(duplicate);
    for case in [
        "merge_only",
        "legacy",
        "missing_end",
        "wrong_destination_creation",
        "wrong_origin",
        "wrong_hash",
    ] {
        let mut changed = original.clone();
        match case {
            "merge_only" => changed["alreadyPromoted"] = Value::Bool(true),
            "legacy" => {
                changed["sourcePayload"]
                    .as_object_mut()
                    .unwrap()
                    .remove("captureDefaultStart");
            }
            "missing_end" => {
                changed["sourcePayload"]["captureDefaultStart"]
                    .as_object_mut()
                    .unwrap()
                    .remove("validTo");
            }
            "wrong_destination_creation" => {
                changed["destinationCreatedAt"] = Value::String(AUTHORED_START.to_owned())
            }
            "wrong_origin" => changed["originMemoryId"] = Value::String(f.second.memory.id.clone()),
            "wrong_hash" => {
                changed["sourcePayload"]["captureDefaultStart"]["contentHash"] =
                    Value::String(content_hash("different content"))
            }
            _ => unreachable!(),
        }
        replace_details(&f.db, &audit_id, &changed);
        assert_eq!(f.preview(), None, "{case}");
    }
    replace_details(&f.db, &audit_id, &original);
    assert_eq!(f.preview(), Some(f.target.id.clone()));
    f.db.execute_raw(&format!(
        "UPDATE memories SET provenance_uri = 'ee-mem://{}/{}' WHERE id = '{}'",
        f.second.memory.workspace_id, f.second.memory.id, f.target.id
    ))
    .unwrap();
    assert_eq!(f.preview(), None);
    f.db.execute_raw(&format!(
        "UPDATE memories SET provenance_uri = '{}' WHERE id = '{}'",
        f.target.provenance_uri.as_deref().unwrap(),
        f.target.id
    ))
    .unwrap();
    assert_eq!(f.preview(), Some(f.target.id.clone()));
    let extra = generate_audit_id();
    f.db.insert_audit(
        &extra,
        &CreateAuditInput {
            workspace_id: Some(f.workspace.clone()),
            actor: None,
            action: "memory.promote_global".to_owned(),
            target_type: Some("memory".to_owned()),
            target_id: Some(f.target.id.clone()),
            details: Some(original.to_string()),
        },
    )
    .unwrap();
    assert_eq!(
        f.preview(),
        None,
        "two claimed original insertions are ambiguous"
    );
    replace_raw_details(&f.db, &extra, "{invalid");
    assert_eq!(
        f.preview(),
        None,
        "malformed receipt cannot be assumed to be a merge"
    );
}

#[test]
fn explicit_and_legacy_validity_keep_strict_equality() {
    let f = Fixture::new(None, None);
    let physical = f._root.path().canonicalize().unwrap();
    let authored = Capture::new(&physical, "authored", Some(AUTHORED_START), None);
    assert!(
        capture_default_start(&authored.db, &authored.memory)
            .unwrap()
            .is_none()
    );
    let (authored_id, duplicate, _) = authored.publish(&f.paths, &f.db, &f.workspace);
    assert!(!duplicate);
    assert_ne!(authored_id, f.target.id);
    let same_authored = Capture::new(&physical, "same-authored", Some(AUTHORED_START), None);
    let (same_id, duplicate, _) = same_authored.publish(&f.paths, &f.db, &f.workspace);
    assert!(duplicate);
    assert_eq!(same_id, authored_id);

    let (audit_id, mut legacy) =
        audit_rows(&f.second.db, &f.second.memory, "memory.create").remove(0);
    legacy
        .as_object_mut()
        .unwrap()
        .remove("captureValidityInput");
    replace_details(&f.second.db, &audit_id, &legacy);
    let (legacy_id, duplicate, _) = f.second.publish(&f.paths, &f.db, &f.workspace);
    assert!(!duplicate);
    assert_ne!(legacy_id, f.target.id);
    let (again, duplicate, _) = f.second.publish(&f.paths, &f.db, &f.workspace);
    assert!(duplicate);
    assert_eq!(again, legacy_id);
    assert_eq!(
        f.db.get_memory(&f.target.id).unwrap(),
        Some(f.target.clone())
    );
}

#[test]
fn direct_global_capture_has_its_own_default_start_authority() {
    let root = tempfile::tempdir().unwrap();
    let physical = root.path().canonicalize().unwrap();
    let paths = GlobalStorePaths::from_root(&physical.join("global"));
    let (db, workspace) = open_or_create_global_store(&paths).unwrap();
    let target = remember(&db, &paths.root, &paths.database_path, None, None);
    let source = Capture::new(&physical, "source", None, None);
    assert_ne!(source.memory.valid_from, target.valid_from);
    assert!(audit_rows(&db, &target, "memory.promote_global").is_empty());
    let preview = promote_global(&source.options(&paths, true)).unwrap();
    assert_eq!(
        preview.global_memory_id.as_deref(),
        Some(target.id.as_str())
    );
    let (id, duplicate, _) = source.publish(&paths, &db, &workspace);
    assert!(duplicate);
    assert_eq!(id, target.id);
    assert_eq!(db.get_memory(&target.id).unwrap(), Some(target));
}

#[test]
fn independently_defaulted_starts_never_relax_end_bounds() {
    let f = Fixture::new(Some("2099-01-01T00:00:00Z"), Some("2098-01-01T00:00:00Z"));
    assert!(
        capture_default_start(&f.first.db, &f.first.memory)
            .unwrap()
            .is_some()
    );
    assert!(
        capture_default_start(&f.second.db, &f.second.memory)
            .unwrap()
            .is_some()
    );
    assert_eq!(f.preview(), None);
    let (id, duplicate, _) = f.second.publish(&f.paths, &f.db, &f.workspace);
    assert!(!duplicate);
    assert_ne!(id, f.target.id);
    assert_eq!(
        f.db.get_memory(&f.target.id).unwrap(),
        Some(f.target.clone())
    );
}
