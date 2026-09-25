use super::*;
use crate::db::{
    CreateEvidenceSpanInput, CreateMemoryInput, CreateSessionInput, CreateWorkspaceInput,
    EvidenceProducerKind, WorkspaceScopeFields,
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
    let source_keys_dir = test_root().join("source-keys");
    StoreAuthRoot::create(&source_keys_dir).expect("source authentication fixture");
    let options = WorkspaceRebindOptions {
        workspace_path: PathBuf::from("/relocated/project"),
        source_keys_dir,
        expected_workspace_id: WORKSPACE.to_owned(),
        expected_source_path: "/original/project".to_owned(),
        apply_plan: None,
    };
    (db, options, destination("/relocated/project"))
}

fn plan_for_connection(
    db: &DbConnection,
    database: &str,
    target: &WorkspaceRebindDestination,
    options: &WorkspaceRebindOptions,
) -> Result<WorkspaceRebindPlan, DbError> {
    let auth = StoreAuthRoot::open(&options.source_keys_dir).expect("source authentication");
    super::plan_for_connection(db, database, target, options, &auth)
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
    let auth = StoreAuthRoot::open(&options.source_keys_dir).expect("source authentication");
    db.with_transaction(|| {
        apply_on_connection(
            db,
            &plan.database_path,
            target,
            options,
            &auth,
            &auth,
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
    let original_span = db
        .list_evidence_spans_for_session(SESSION)
        .expect("read original evidence")
        .into_iter()
        .next()
        .expect("original evidence exists");
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
    assert_eq!(span.cass_span_id, original_span.cass_span_id);
    assert_eq!(span.upstream_ref_hash, original_span.upstream_ref_hash);
    assert_eq!(span, &original_span);
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
        source_keys_dir: root.join("keys"),
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
        source_keys_dir: root.join("keys"),
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

struct CopiedStoreFixture {
    options: WorkspaceRebindOptions,
    original_database: PathBuf,
    copied_database: PathBuf,
    memory: crate::db::StoredMemory,
}

fn copy_closed_database(source: &Path, target: &Path) {
    std::fs::copy(source, target).expect("copy closed database");
    for suffix in ["-wal", "-shm"] {
        let mut source_sidecar = source.as_os_str().to_os_string();
        source_sidecar.push(suffix);
        let source_sidecar = PathBuf::from(source_sidecar);
        if source_sidecar.exists() {
            let mut target_sidecar = target.as_os_str().to_os_string();
            target_sidecar.push(suffix);
            std::fs::copy(source_sidecar, PathBuf::from(target_sidecar))
                .expect("copy closed database sidecar");
        }
    }
}

fn copied_store_fixture() -> CopiedStoreFixture {
    let root = test_root();
    let original = root.join("original workspace");
    let copied = root.join("relocated workspace");
    for workspace in [&original, &copied] {
        std::fs::create_dir_all(workspace.join(WORKSPACE_MARKER)).expect("workspace marker");
    }
    let original_database = original.join(".ee/ee.db");
    let copied_database = copied.join(".ee/ee.db");
    let db = DbConnection::open_file(&original_database).expect("source store");
    db.migrate().expect("source migrations");
    bind(&db, WORKSPACE, original.to_str().expect("source path"));
    let memory_id = "mem_00000000000000000000000000";
    db.insert_memory(
        memory_id,
        &CreateMemoryInput {
            workspace_id: WORKSPACE.to_owned(),
            level: "semantic".to_owned(),
            kind: "fact".to_owned(),
            content: "Durable relocation keeps this authenticated source identity.".to_owned(),
            workflow_id: None,
            confidence: 1.0,
            utility: 0.7,
            importance: 0.8,
            provenance_uri: Some("manual://original-project/relocation".to_owned()),
            trust_class: "human_explicit".to_owned(),
            trust_subclass: None,
            tags: vec!["relocation-proof".to_owned()],
            valid_from: Some("2020-01-01T00:00:00Z".to_owned()),
            valid_to: None,
        },
    )
    .expect("durable source memory");
    let memory = db
        .get_memory(memory_id)
        .expect("memory query")
        .expect("memory exists");
    db.close().expect("close source before copying");
    let source_keys_dir = workspace_keys_dir(&original);
    let mut auth = StoreAuthRoot::create(&source_keys_dir).expect("source authentication");
    auth.rotate().expect("retain historical authentication key");
    let copied_keys_dir = workspace_keys_dir(&copied);
    std::fs::create_dir(&copied_keys_dir).expect("copied key directory");
    std::fs::set_permissions(
        &copied_keys_dir,
        std::fs::metadata(&source_keys_dir)
            .expect("source key mode")
            .permissions(),
    )
    .expect("preserve owner-only key directory");
    std::fs::copy(
        source_keys_dir.join(crate::policy::store_auth::KEY_FILE_NAME),
        copied_keys_dir.join(crate::policy::store_auth::KEY_FILE_NAME),
    )
    .expect("copy existing authentication material");
    copy_closed_database(&original_database, &copied_database);
    std::fs::create_dir(copied.join(".ee/index")).expect("derived index directory");
    std::fs::write(
        copied.join(".ee/index/retained-asset"),
        b"copied derived bytes",
    )
    .expect("retained derived asset");
    CopiedStoreFixture {
        options: WorkspaceRebindOptions {
            workspace_path: copied,
            source_keys_dir,
            expected_workspace_id: WORKSPACE.to_owned(),
            expected_source_path: original.to_str().expect("source path").to_owned(),
            apply_plan: None,
        },
        original_database,
        copied_database,
        memory,
    }
}

fn file_digests(root: &Path) -> std::collections::BTreeMap<PathBuf, String> {
    fn visit(path: &Path, files: &mut std::collections::BTreeMap<PathBuf, String>) {
        for entry in std::fs::read_dir(path).expect("read fixture directory") {
            let entry = entry.expect("fixture entry");
            if entry.file_type().expect("fixture file type").is_dir() {
                visit(&entry.path(), files);
            } else {
                let digest = blake3::hash(&std::fs::read(entry.path()).expect("fixture bytes"));
                files.insert(entry.path(), digest.to_hex().to_string());
            }
        }
    }
    let mut files = std::collections::BTreeMap::new();
    visit(root, &mut files);
    files
}

#[test]
fn authenticated_file_rebind_preserves_keys_memories_and_every_unrelated_table() {
    let mut fixture = copied_store_fixture();
    let source = StoreAuthRoot::open(&fixture.options.source_keys_dir).expect("source root");
    let original_key_ids = source.window_key_ids();
    let native_mac = source
        .mac(
            MacDomain::NativeImportRecordsRoot,
            b"existing signed artifact",
        )
        .expect("existing artifact MAC");
    let copied = DbConnection::open_file_read_only(&fixture.copied_database).expect("read copy");
    let before = copied
        .logical_table_digests()
        .expect("before table digests");
    assert!(matches!(
        crate::core::workspace::addressed_workspace_row(
            &copied,
            &fixture.options.workspace_path,
            &fixture.copied_database,
        ),
        Err(DomainError::WorkspaceIdentityMismatch { .. })
    ));
    drop(copied);
    let unchanged_files = file_digests(&fixture.options.workspace_path);
    let original_files = file_digests(Path::new(&fixture.options.expected_source_path));
    let preview = rebind_workspace(&fixture.options).expect("authenticated preview");
    assert_eq!(
        preview,
        rebind_workspace(&fixture.options).expect("repeat preview")
    );
    assert_eq!(
        file_digests(&fixture.options.workspace_path),
        unchanged_files
    );
    assert_eq!(
        file_digests(Path::new(&fixture.options.expected_source_path)),
        original_files
    );
    assert_eq!(
        preview.plan.source_auth_key_id,
        source.current_key_id().to_hex()
    );
    assert!(preview.plan.content_hash.starts_with("blake3:"));
    fixture.options.apply_plan = Some(preview.plan.plan_hash.clone());
    let applied = rebind_workspace(&fixture.options).expect("apply authenticated rebind");
    assert!(applied.persisted);
    assert!(!applied.dry_run);
    assert!(!applied.derived_assets_modified);

    let copied = DbConnection::open_file_read_only(&fixture.copied_database).expect("reopen copy");
    let bound = crate::core::workspace::addressed_workspace_row(
        &copied,
        &fixture.options.workspace_path,
        &fixture.copied_database,
    )
    .expect("ordinary destination resolution")
    .expect("rebound identity exists");
    assert_eq!(bound.id, WORKSPACE);
    assert_eq!(
        copied
            .get_memory(&fixture.memory.id)
            .expect("retained memory"),
        Some(fixture.memory)
    );
    let after = copied.logical_table_digests().expect("after table digests");
    assert_eq!(
        before.keys().collect::<Vec<_>>(),
        after.keys().collect::<Vec<_>>()
    );
    for (table, digest) in &before {
        if !matches!(table.as_str(), "workspaces" | "audit_log") {
            assert_eq!(
                after.get(table),
                Some(digest),
                "rebind changed table {table}"
            );
        }
    }
    let audit = copied
        .get_audit(applied.audit_id.as_deref().expect("audit id"))
        .expect("audit query")
        .expect("audit exists");
    let details: serde_json::Value =
        serde_json::from_str(audit.details.as_deref().expect("audit details"))
            .expect("audit plan JSON");
    assert_eq!(details["sourceAuthKeyId"], applied.plan.source_auth_key_id);
    assert_eq!(
        details["previous"]["path"],
        fixture.options.expected_source_path
    );
    assert_eq!(
        details["rollbackPreviewCommand"],
        applied.plan.rollback_preview_command
    );
    assert_eq!(
        std::fs::read(
            fixture
                .options
                .workspace_path
                .join(".ee/index/retained-asset")
        )
        .expect("derived bytes retained"),
        b"copied derived bytes"
    );
    let destination_root = StoreAuthRoot::open(workspace_keys_dir(&fixture.options.workspace_path))
        .expect("retained destination authentication");
    assert_eq!(destination_root.window_key_ids(), original_key_ids);
    let copied_key_file = workspace_keys_dir(&fixture.options.workspace_path)
        .join(crate::policy::store_auth::KEY_FILE_NAME);
    assert_eq!(
        file_digests(&fixture.options.workspace_path).get(&copied_key_file),
        unchanged_files.get(&copied_key_file),
        "the complete current and retired key material must remain byte-identical"
    );
    assert!(
        destination_root
            .verify(
                MacDomain::NativeImportRecordsRoot,
                b"existing signed artifact",
                &native_mac
            )
            .expect("old authentication still works")
    );
    assert_eq!(
        file_digests(Path::new(&fixture.options.expected_source_path)),
        original_files
    );
    let original =
        DbConnection::open_file_read_only(&fixture.original_database).expect("original store");
    assert_eq!(
        original.logical_table_digests().expect("original rows"),
        before
    );
    let applied_files = file_digests(&fixture.options.workspace_path);
    assert!(
        rebind_workspace(&fixture.options).is_err(),
        "an applied plan cannot be replayed"
    );
    assert_eq!(file_digests(&fixture.options.workspace_path), applied_files);
}

#[test]
fn public_rebind_refuses_wrong_selection_keys_and_commitments_without_writes() {
    let fixture = copied_store_fixture();
    let preview = rebind_workspace(&fixture.options).expect("valid preview");
    let foreign_keys = test_root().join("foreign-keys");
    StoreAuthRoot::create(&foreign_keys).expect("foreign root");
    let before = file_digests(&fixture.options.workspace_path);
    let source_before = file_digests(Path::new(&fixture.options.expected_source_path));
    let mut invalid = Vec::new();
    let mut wrong_id = fixture.options.clone();
    wrong_id.expected_workspace_id = OTHER_WORKSPACE.to_owned();
    invalid.push(wrong_id);
    let mut wrong_path = fixture.options.clone();
    wrong_path.expected_source_path = "/different/source".to_owned();
    invalid.push(wrong_path);
    let mut wrong_key = fixture.options.clone();
    wrong_key.source_keys_dir = foreign_keys;
    invalid.push(wrong_key);
    let mut missing_key = fixture.options.clone();
    missing_key.source_keys_dir = fixture.options.source_keys_dir.join("missing");
    invalid.push(missing_key);
    for token in [
        preview.plan.content_hash,
        format!("{}00", preview.plan.plan_hash),
    ] {
        let mut unauthenticated = fixture.options.clone();
        unauthenticated.apply_plan = Some(token);
        invalid.push(unauthenticated);
    }
    for options in invalid {
        assert!(rebind_workspace(&options).is_err());
        assert_eq!(file_digests(&fixture.options.workspace_path), before);
        assert_eq!(
            file_digests(Path::new(&fixture.options.expected_source_path)),
            source_before
        );
    }
    assert!(!fixture.options.source_keys_dir.join("missing").exists());
}

#[test]
fn copied_root_must_prove_key_possession_even_when_its_public_key_id_matches() {
    let fixture = copied_store_fixture();
    let expected = StoreAuthRoot::open(&fixture.options.source_keys_dir)
        .expect("source root")
        .current_key_id()
        .to_hex();
    let foreign_keys = test_root().join("foreign-keys");
    StoreAuthRoot::create(&foreign_keys).expect("foreign root");
    let filename = crate::policy::store_auth::KEY_FILE_NAME;
    let mut foreign: serde_json::Value = serde_json::from_slice(
        &std::fs::read(foreign_keys.join(filename)).expect("foreign root bytes"),
    )
    .expect("foreign key JSON");
    foreign["current"]["keyId"] = serde_json::Value::String(expected.clone());
    std::fs::write(
        workspace_keys_dir(&fixture.options.workspace_path).join(filename),
        serde_json::to_vec(&foreign).expect("foreign fixture document"),
    )
    .expect("replace copied root with a key-id collision");
    let copied_root = StoreAuthRoot::open(workspace_keys_dir(&fixture.options.workspace_path))
        .expect("foreign root remains structurally valid");
    assert_eq!(copied_root.current_key_id().to_hex(), expected);
    let before = file_digests(&fixture.options.workspace_path);
    assert!(rebind_workspace(&fixture.options).is_err());
    assert_eq!(file_digests(&fixture.options.workspace_path), before);
}

#[test]
fn rebind_commitment_rejects_import_mac_replay_and_rotation_outside_current_key() {
    let (db, options, target) = fixture();
    let plan = preview(&db, &options, &target);
    let mut auth = StoreAuthRoot::open(&options.source_keys_dir).expect("source root");
    let import_mac = auth
        .mac(
            MacDomain::NativeImportRecordsRoot,
            &commitment_message(&plan).expect("message"),
        )
        .expect("import-domain MAC");
    let replay = format!(
        "{COMMITMENT_PREFIX}:{}:{}",
        plan.source_auth_key_id,
        import_mac.to_hex()
    );
    assert!(verify_plan_commitment(&plan, &replay, &auth).is_err());
    assert!(verify_plan_commitment(&plan, &plan.plan_hash, &auth).is_ok());
    auth.rotate().expect("rotate source root");
    assert!(verify_plan_commitment(&plan, &plan.plan_hash, &auth).is_err());
    assert!(apply(&db, &options, &target, &plan, &generate_audit_id()).is_err());
    assert_eq!(
        single_workspace(&db).expect("unchanged source"),
        plan.previous
    );
}

#[test]
fn authenticated_inverse_plan_restores_addressing_with_a_second_audit() {
    let (db, mut options, target) = fixture();
    let forward = preview(&db, &options, &target);
    apply(&db, &options, &target, &forward, &generate_audit_id()).expect("forward rebind");
    let restored = destination(&forward.previous.path);
    options.workspace_path = PathBuf::from(&restored.path);
    options.expected_source_path = target.path;
    let inverse = plan_for_connection(&db, "/original/project/.ee/ee.db", &restored, &options)
        .expect("inverse preview");
    apply(&db, &options, &restored, &inverse, &generate_audit_id()).expect("inverse rebind");
    let actual = single_workspace(&db).expect("restored binding");
    let mut expected = forward.previous;
    expected.updated_at = actual.updated_at.clone();
    assert_eq!(actual, expected);
    assert_eq!(
        db.list_audit_by_action(REBIND_ACTION, None)
            .expect("rebind history")
            .len(),
        2
    );
}
