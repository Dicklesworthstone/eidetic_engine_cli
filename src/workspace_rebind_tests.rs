use super::*;
use crate::db::{
    CreateEvidenceSpanInput, CreateSessionInput, CreateWorkspaceInput, EvidenceProducerKind,
    WorkspaceScopeFields,
};

const WORKSPACE: &str = "wsp_00000000000000000000000000";
const OTHER_WORKSPACE: &str = "wsp_00000000000000000000000001";
const SESSION: &str = "sess_00000000000000000000000000";
const EVIDENCE: &str = "ev_00000000000000000000000000";

fn destination(path: &str) -> WorkspaceRebindDestination {
    let scope = derive_workspace_scope(Path::new(path));
    WorkspaceRebindDestination {
        path: path.to_owned(),
        scope_kind: scope.kind.as_str().to_owned(),
        repository_root: scope.repository_root.map(|p| p.display().to_string()),
        repository_fingerprint: scope.repository_fingerprint,
        subproject_path: scope.subproject_path.map(|p| p.display().to_string()),
    }
}

fn bind(db: &DbConnection, id: &str, path: &str) {
    let target = destination(path);
    db.upsert_workspace_with_scope(
        id,
        &CreateWorkspaceInput {
            path: path.to_owned(),
            name: Some(format!("alias-{id}")),
        },
        &WorkspaceScopeFields {
            scope_kind: target.scope_kind,
            repository_root: target.repository_root,
            repository_fingerprint: target.repository_fingerprint,
            subproject_path: target.subproject_path,
        },
    )
    .expect("insert fixture workspace");
}

fn fixture() -> (
    DbConnection,
    WorkspaceRebindOptions,
    WorkspaceRebindDestination,
) {
    let db = DbConnection::open_memory().expect("open database");
    db.migrate().expect("migrate database");
    bind(&db, WORKSPACE, "/original/project");
    let options = WorkspaceRebindOptions {
        workspace_path: PathBuf::from("/relocated/project"),
        expected_workspace_id: WORKSPACE.to_owned(),
        expected_source_path: "/original/project".to_owned(),
        apply_plan: None,
    };
    (db, options, destination("/relocated/project"))
}

fn preview(
    db: &DbConnection,
    options: &WorkspaceRebindOptions,
    target: &WorkspaceRebindDestination,
) -> WorkspaceRebindPlan {
    plan_for_connection(db, "/relocated/project/.ee/ee.db", target, options)
        .expect("preview exact single-workspace binding")
}

fn apply(
    db: &DbConnection,
    options: &WorkspaceRebindOptions,
    target: &WorkspaceRebindDestination,
    plan: &WorkspaceRebindPlan,
    audit: &str,
) -> Result<WorkspaceRebindPlan, DbError> {
    db.with_transaction(|| {
        apply_on_connection(
            db,
            &plan.database_path,
            target,
            options,
            &plan.plan_hash,
            audit,
        )
    })
}

#[test]
fn preview_is_repeatable_and_does_not_change_the_binding() {
    let (db, options, target) = fixture();
    let before = single_workspace(&db).expect("binding");
    let first = preview(&db, &options, &target);
    let second = preview(&db, &options, &target);
    assert_eq!(first, second);
    assert_eq!(
        before,
        single_workspace(&db).expect("binding after preview")
    );
    let result = report(first, None);
    assert!(result.dry_run);
    assert!(!result.persisted);
    assert!(result.audit_id.is_none());
    assert!(!result.derived_assets_modified);
}

#[test]
fn rebind_preserves_durable_session_and_evidence_identity_and_normal_resolution() {
    let (db, options, target) = fixture();
    let excerpt = "Run the complete release checks before publishing.";
    db.insert_session(
        SESSION,
        &CreateSessionInput {
            workspace_id: WORKSPACE.to_owned(),
            cass_session_id: "/original/transcript.jsonl".to_owned(),
            source_path: Some("/original/transcript.jsonl".to_owned()),
            agent_name: Some("codex".to_owned()),
            model: None,
            started_at: None,
            ended_at: None,
            message_count: 1,
            token_count: None,
            content_hash: "session-source-hash".to_owned(),
            metadata_json: None,
        },
    )
    .expect("insert durable session");
    db.insert_evidence_span(
        EVIDENCE,
        &CreateEvidenceSpanInput {
            workspace_id: WORKSPACE.to_owned(),
            session_id: SESSION.to_owned(),
            memory_id: None,
            producer_kind: EvidenceProducerKind::CassImport,
            cass_span_id: "/original/transcript.jsonl:1".to_owned(),
            span_kind: "message".to_owned(),
            start_line: 1,
            end_line: 1,
            start_byte: None,
            end_byte: None,
            role: Some("assistant".to_owned()),
            excerpt: excerpt.to_owned(),
            content_hash: format!("blake3:{}", blake3::hash(excerpt.as_bytes()).to_hex()),
            metadata_json: None,
            inherited_redaction_classes: Vec::new(),
        },
    )
    .expect("insert durable evidence");
    let before = preview(&db, &options, &target);
    let audit = generate_audit_id();
    apply(&db, &options, &target, &before, &audit).expect("apply rebind");
    let after = single_workspace(&db).expect("rebound workspace");
    assert_eq!(after.workspace_id, WORKSPACE);
    assert_eq!(after.path, target.path);
    assert_eq!(after.alias, before.previous.alias);
    assert_eq!(after.created_at, before.previous.created_at);
    let session = db
        .get_session_by_cass_id(WORKSPACE, "/original/transcript.jsonl")
        .expect("read retained session")
        .expect("session still exists");
    assert_eq!(session.id, SESSION);
    assert_eq!(
        session.source_path.as_deref(),
        Some("/original/transcript.jsonl")
    );
    assert_eq!(session.content_hash, "session-source-hash");
    let spans = db
        .list_evidence_spans_for_session(SESSION)
        .expect("retained evidence");
    assert_eq!(spans.len(), 1);
    let span = spans.first().expect("one evidence row");
    assert_eq!(span.id, EVIDENCE);
    assert_eq!(span.workspace_id, WORKSPACE);
    assert_eq!(span.session_id, SESSION);
    assert_eq!(span.cass_span_id, "/original/transcript.jsonl:1");
    assert_eq!(span.excerpt, excerpt);
    assert_eq!(
        span.content_hash,
        format!("blake3:{}", blake3::hash(excerpt.as_bytes()).to_hex())
    );
    let entry = db
        .get_audit(&audit)
        .expect("read audit")
        .expect("audit committed");
    assert_eq!(entry.action, REBIND_ACTION);
    assert_eq!(entry.target_id.as_deref(), Some(WORKSPACE));
    let path = Path::new(&target.path);
    let requested = crate::core::workspace::stable_workspace_id(path);
    let resolved = crate::core::workspace::ensure_bound_workspace(&db, &requested, &[path])
        .expect("normal resolution must reuse the rebound identity");
    assert_eq!(resolved, WORKSPACE);
    assert_eq!(db.list_workspaces().expect("workspace count").len(), 1);
}

#[test]
fn refusal_never_guesses_a_source_or_merges_multiple_workspaces() {
    let (db, mut options, target) = fixture();
    options.expected_workspace_id = OTHER_WORKSPACE.to_owned();
    assert!(plan_for_connection(&db, "store", &target, &options).is_err());
    options.expected_workspace_id = WORKSPACE.to_owned();
    options.expected_source_path = "/different/source".to_owned();
    assert!(plan_for_connection(&db, "store", &target, &options).is_err());
    options.expected_source_path = "/original/project".to_owned();
    bind(&db, OTHER_WORKSPACE, "/second/project");
    assert!(plan_for_connection(&db, "store", &target, &options).is_err());
    let empty = DbConnection::open_memory().expect("empty database");
    empty.migrate().expect("migrate empty database");
    assert!(plan_for_connection(&empty, "store", &target, &options).is_err());
}

#[test]
fn a_stale_preview_cannot_overwrite_new_alias_or_scope_metadata() {
    let (db, options, target) = fixture();
    let original = preview(&db, &options, &target);
    db.update_workspace_name(WORKSPACE, Some("changed-after-preview"))
        .expect("concurrent metadata edit");
    let changed = single_workspace(&db).expect("changed binding");
    assert!(apply(&db, &options, &target, &original, &generate_audit_id()).is_err());
    assert_eq!(
        single_workspace(&db).expect("binding after refusal"),
        changed
    );
    assert_ne!(
        preview(&db, &options, &target).plan_hash,
        original.plan_hash
    );
}

#[test]
fn a_second_workspace_created_after_preview_blocks_apply() {
    let (db, options, target) = fixture();
    let plan = preview(&db, &options, &target);
    bind(&db, OTHER_WORKSPACE, "/second/project");
    assert!(apply(&db, &options, &target, &plan, &generate_audit_id()).is_err());
    assert_eq!(db.list_workspaces().expect("workspaces").len(), 2);
}

#[test]
fn commitments_are_bound_to_the_store_destination_and_source_metadata() {
    let (db, options, target) = fixture();
    let original = preview(&db, &options, &target);
    let other_database = plan_for_connection(&db, "/other/.ee/ee.db", &target, &options)
        .expect("other addressed store");
    assert_ne!(original.plan_hash, other_database.plan_hash);
    let mut changed_target = target.clone();
    changed_target.path = "/another/project".to_owned();
    let other_target = preview(&db, &options, &changed_target);
    assert_ne!(original.plan_hash, other_target.plan_hash);
    assert!(
        apply(
            &db,
            &options,
            &changed_target,
            &original,
            &generate_audit_id()
        )
        .is_err()
    );
    changed_target = target.clone();
    changed_target.repository_fingerprint = Some("changed-repository-scope".to_owned());
    assert_ne!(
        preview(&db, &options, &changed_target).plan_hash,
        original.plan_hash
    );
}

#[test]
fn an_audit_failure_rolls_back_the_location_update() {
    let (db, options, target) = fixture();
    let plan = preview(&db, &options, &target);
    let audit = generate_audit_id();
    db.insert_audit(
        &audit,
        &CreateAuditInput {
            workspace_id: Some(WORKSPACE.to_owned()),
            actor: Some("fixture".to_owned()),
            action: "fixture.existing_audit".to_owned(),
            target_type: Some("workspace".to_owned()),
            target_id: Some(WORKSPACE.to_owned()),
            details: None,
        },
    )
    .expect("occupy audit identity");
    assert!(apply(&db, &options, &target, &plan, &audit).is_err());
    assert_eq!(
        single_workspace(&db).expect("rolled back binding"),
        plan.previous
    );
    let existing = db
        .get_audit(&audit)
        .expect("read existing audit")
        .expect("audit retained");
    assert_eq!(existing.action, "fixture.existing_audit");
}

#[test]
fn replaying_a_successful_plan_is_refused_without_a_second_rebind() {
    let (db, options, target) = fixture();
    let plan = preview(&db, &options, &target);
    apply(&db, &options, &target, &plan, &generate_audit_id()).expect("first apply");
    let after = single_workspace(&db).expect("rebound workspace");
    assert!(apply(&db, &options, &target, &plan, &generate_audit_id()).is_err());
    assert_eq!(single_workspace(&db).expect("binding after replay"), after);
}

fn test_root() -> PathBuf {
    let root = std::env::temp_dir().join(format!("ee-rebind-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&root).expect("create test root");
    root.canonicalize().expect("physical test root")
}

#[test]
fn previewing_missing_or_non_regular_stores_never_creates_a_database() {
    let root = test_root();
    let options = WorkspaceRebindOptions {
        workspace_path: root.clone(),
        expected_workspace_id: WORKSPACE.to_owned(),
        expected_source_path: "/original/project".to_owned(),
        apply_plan: None,
    };
    assert!(rebind_workspace(&options).is_err());
    assert!(!root.join(".ee").exists());
    std::fs::create_dir(root.join(".ee")).expect("create marker only");
    assert!(rebind_workspace(&options).is_err());
    assert!(!root.join(".ee/ee.db").exists());
    std::fs::create_dir(root.join(".ee/ee.db")).expect("non-regular database fixture");
    assert!(rebind_workspace(&options).is_err());
    assert!(root.join(".ee/ee.db").is_dir());
}

#[cfg(unix)]
#[test]
fn preview_refuses_symlinked_store_files_without_following_them() {
    let root = test_root();
    let outside = root.join("outside.db");
    std::fs::write(&outside, b"not a database").expect("outside fixture");
    std::fs::create_dir(root.join(".ee")).expect("marker");
    std::os::unix::fs::symlink(&outside, root.join(".ee/ee.db")).expect("symlink fixture");
    let options = WorkspaceRebindOptions {
        workspace_path: root,
        expected_workspace_id: WORKSPACE.to_owned(),
        expected_source_path: "/original/project".to_owned(),
        apply_plan: None,
    };
    assert!(rebind_workspace(&options).is_err());
    assert_eq!(
        std::fs::read(outside).expect("outside remains unchanged"),
        b"not a database"
    );
}

#[test]
fn location_update_quotes_apostrophes_and_sql_shaped_path_text() {
    let (db, options, mut target) = fixture();
    target.path = "/relocated/it's a project'; DELETE FROM workspaces; --".to_owned();
    let plan = preview(&db, &options, &target);
    apply(&db, &options, &target, &plan, &generate_audit_id()).expect("quoted location update");
    let after = single_workspace(&db).expect("workspace was not deleted");
    assert_eq!(after.path, target.path);
    assert_eq!(after.repository_root, target.repository_root);
    assert_eq!(after.workspace_id, WORKSPACE);
    assert!(sql_text("bad\0binding").is_err());
    assert_eq!(sql_optional_text(None).expect("SQL NULL"), "NULL");
    assert_eq!(
        sql_optional_text(Some("NULL")).expect("literal NULL"),
        "'NULL'"
    );
}
