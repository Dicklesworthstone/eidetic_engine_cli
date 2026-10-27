//! Requested creation guards commit with their source, including keyed retries.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;
use crate::models::{
    MemorySentinelPolarity, MemorySentinelResult, MemorySentinelResultInput,
    MemorySentinelResultStatus, MemorySentinelSpec,
};

struct Fixture {
    _root: tempfile::TempDir,
    workspace: PathBuf,
    database: PathBuf,
    workspace_id: String,
    db: DbConnection,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().canonicalize().unwrap();
        fs::create_dir(workspace.join(".ee")).unwrap();
        let database = workspace.join(".ee/ee.db");
        let db = DbConnection::open_file(&database).unwrap();
        db.migrate().unwrap();
        let workspace_id = stable_workspace_id(&workspace);
        db.insert_workspace(
            &workspace_id,
            &CreateWorkspaceInput {
                path: workspace.to_string_lossy().into_owned(),
                name: None,
            },
        )
        .unwrap();
        Self {
            _root: root,
            workspace,
            database,
            workspace_id,
            db,
        }
    }

    fn options<'a>(&'a self, content: &'a str) -> RememberMemoryOptions<'a> {
        RememberMemoryOptions {
            workspace_path: &self.workspace,
            database_path: None,
            content,
            workflow_id: None,
            level: "semantic",
            kind: "decision",
            tags: Some("creation-guard"),
            confidence: 0.8,
            source: Some("manual://creation-companion-test"),
            allow_secret_mention: false,
            valid_from: None,
            valid_to: None,
            dry_run: false,
            auto_link: false,
            propose_candidates: false,
        }
    }
}

fn controls(key: Option<&str>) -> RememberWriteControls<'_> {
    RememberWriteControls {
        idempotency_key: key,
        defer_index_processing: true,
        ..RememberWriteControls::default()
    }
}

fn source_snapshot(db: &DbConnection) -> Vec<(String, String)> {
    [
        "memories",
        "memory_tags",
        "audit_log",
        "search_index_jobs",
        "remember_idempotency_keys",
        "attempt_families",
        "attempt_family_members",
        "memory_sentinel_specs",
        "memory_sentinel_results",
        "memory_seals",
    ]
    .into_iter()
    .map(|table| {
        let rows = db
            .query(&format!("SELECT * FROM {table} ORDER BY 1, 2"), &[])
            .unwrap();
        (table.to_owned(), format!("{rows:?}"))
    })
    .collect()
}

fn created(outcome: RememberOutcome) -> Box<RememberMemoryReport> {
    match outcome {
        RememberOutcome::Created(report) => report,
        other => panic!("expected a fresh creation, got {other:?}"),
    }
}

fn replayed(outcome: RememberOutcome, expected_id: &str) {
    match outcome {
        RememberOutcome::AlreadyRecorded(report) => assert_eq!(report.memory_id, expected_id),
        other => panic!("expected a stored replay, got {other:?}"),
    }
}

#[test]
fn second_sentinel_failure_rolls_back_source_sidecars_audit_job_and_key() {
    let fixture = Fixture::new();
    let gates = vec!["path_exists:src/first.rs".to_owned()];
    let revivals = vec!["path_exists:src/second.rs".to_owned()];
    let companions = RememberCreationCompanions {
        sentinels: &gates,
        revive_when: &revivals,
        stale_threshold_seconds: Some(300),
        seal: None,
    };
    let options = fixture.options("The guarded configuration is the chosen decision.");
    let controls = controls(Some("atomic-guards"));
    let fields = vec!["chosen=guarded configuration".to_owned()];
    let family = RememberAttemptFamily {
        family_id: "atomic-guard-family",
        declared_size: Some(2),
        attempt_index: Some(1),
        disposition: Some("selected"),
    };
    fixture.db.execute_raw(
        "CREATE TRIGGER reject_second_creation_sentinel BEFORE INSERT ON memory_sentinel_specs
         WHEN (SELECT COUNT(*) FROM memory_sentinel_specs WHERE memory_id = NEW.memory_id) = 1
         BEGIN
           SELECT CASE WHEN
             EXISTS (SELECT 1 FROM memories WHERE id = NEW.memory_id
                     AND typed_fields_json IS NOT NULL AND attempt_family_id IS NOT NULL)
             AND EXISTS (SELECT 1 FROM audit_log WHERE target_id = NEW.memory_id)
             AND EXISTS (SELECT 1 FROM search_index_jobs WHERE document_id = NEW.memory_id)
             AND EXISTS (SELECT 1 FROM remember_idempotency_keys WHERE memory_id = NEW.memory_id)
             AND EXISTS (SELECT 1 FROM attempt_family_members WHERE memory_logical_id = NEW.memory_id)
           THEN RAISE(ABORT, 'late_second_creation_sentinel')
           ELSE RAISE(ABORT, 'wrong_companion_transaction_order') END;
         END"
    ).unwrap();
    let before = source_snapshot(&fixture.db);
    let error = remember_memory_with_creation_companions(
        &options,
        &controls,
        &fields,
        Some(&family),
        companions,
    )
    .unwrap_err();
    assert!(
        error.message().contains("late_second_creation_sentinel"),
        "{error:?}"
    );
    assert_eq!(source_snapshot(&fixture.db), before);

    fixture
        .db
        .execute_raw("DROP TRIGGER reject_second_creation_sentinel")
        .unwrap();
    let report = created(
        remember_memory_with_creation_companions(
            &options,
            &controls,
            &fields,
            Some(&family),
            companions,
        )
        .unwrap(),
    );
    let id = report.memory_id.to_string();
    let specs = fixture.db.list_memory_sentinel_specs(&id).unwrap();
    assert_eq!(specs.len(), 2);
    assert!(
        specs
            .iter()
            .any(|spec| spec.polarity == MemorySentinelPolarity::Gate)
    );
    assert!(
        specs
            .iter()
            .any(|spec| spec.polarity == MemorySentinelPolarity::Revive)
    );
    assert!(
        fixture
            .db
            .get_memory_typed_fields_json(&id)
            .unwrap()
            .is_some()
    );
    assert!(fixture.db.get_memory_attempt_family(&id).unwrap().is_some());
    assert_eq!(
        fixture
            .db
            .list_search_index_jobs(&fixture.workspace_id, None)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        fixture
            .db
            .get_remember_idempotency_key(&fixture.workspace_id, "atomic-guards")
            .unwrap()
            .unwrap()
            .memory_id,
        id
    );
    let committed = source_snapshot(&fixture.db);
    replayed(
        remember_memory_with_creation_companions(
            &options,
            &controls,
            &fields,
            Some(&family),
            companions,
        )
        .unwrap(),
        &id,
    );
    assert_eq!(source_snapshot(&fixture.db), committed);
}

#[test]
fn failed_seal_insert_never_commits_an_unsealed_placeholder() {
    let fixture = Fixture::new();
    let commitment = crate::models::seal_commitment_for_content(
        b"Pre-registered outcome before opening the measurements.",
    )
    .unwrap();
    let seal = RememberSealInput {
        content_commitment: &commitment,
        sealed_at: "2026-01-01T00:00:00Z",
    };
    let companions = RememberCreationCompanions {
        seal: Some(seal),
        ..Default::default()
    };
    let options = fixture.options(crate::models::MEMORY_SEAL_PLACEHOLDER_CONTENT);
    fixture
        .db
        .execute_raw(
            "CREATE TRIGGER reject_creation_seal BEFORE INSERT ON memory_seals BEGIN
         SELECT CASE WHEN
           EXISTS (SELECT 1 FROM audit_log WHERE target_id = NEW.memory_id)
           AND EXISTS (SELECT 1 FROM search_index_jobs WHERE document_id = NEW.memory_id)
         THEN RAISE(ABORT, 'late_creation_seal')
         ELSE RAISE(ABORT, 'wrong_seal_transaction_order') END;
         END",
        )
        .unwrap();
    let before = source_snapshot(&fixture.db);
    let error =
        remember_memory_with_creation_companions(&options, &controls(None), &[], None, companions)
            .unwrap_err();
    assert!(error.message().contains("late_creation_seal"), "{error:?}");
    assert_eq!(source_snapshot(&fixture.db), before);
    fixture
        .db
        .execute_raw("DROP TRIGGER reject_creation_seal")
        .unwrap();
    let report = created(
        remember_memory_with_creation_companions(&options, &controls(None), &[], None, companions)
            .unwrap(),
    );
    let id = report.memory_id.to_string();
    assert_eq!(
        fixture.db.get_memory(&id).unwrap().unwrap().content,
        crate::models::MEMORY_SEAL_PLACEHOLDER_CONTENT
    );
    assert_eq!(
        fixture.db.get_memory_seal(&id).unwrap().unwrap(),
        MemorySeal {
            memory_id: id,
            content_commitment: commitment.clone(),
            sealed_at: seal.sealed_at.to_owned(),
            revealed_at: None,
            reveal_verified: None,
        }
    );
}

#[test]
fn committed_guards_survive_postcommit_audit_file_failure_in_both_stores() {
    for global in [false, true] {
        let fixture = Fixture::new();
        let paths = super::super::global_store::GlobalStorePaths::from_data_root(
            &fixture.workspace.join("user-data"),
        );
        let database = if global {
            paths.database_path.clone()
        } else {
            fixture.database.clone()
        };
        let audit_path = database.parent().unwrap().join("audit.jsonl");
        fs::create_dir_all(audit_path.parent().unwrap()).unwrap();
        assert!(!audit_path.exists());
        fs::create_dir(&audit_path).unwrap();
        let gates = vec!["path_exists:Cargo.toml".to_owned()];
        let companions = RememberCreationCompanions {
            sentinels: &gates,
            ..Default::default()
        };
        let options = fixture.options("The persisted decision retains its requested guard.");
        let controls = controls(Some("audit-guard-key"));
        let write = || {
            if global {
                remember_global_memory_with_creation_companions_at_paths(
                    &options,
                    &controls,
                    &[],
                    paths.clone(),
                    companions,
                )
            } else {
                remember_memory_with_creation_companions(&options, &controls, &[], None, companions)
            }
        };
        let error = write().unwrap_err();
        assert!(
            error.message().contains("failed to append audit JSONL"),
            "{error:?}"
        );
        let db = DbConnection::open_file(&database).unwrap();
        let workspace_id = db.list_workspaces().unwrap()[0].id.clone();
        let memories = db.list_memories(&workspace_id, None, true).unwrap();
        assert_eq!(memories.len(), 1);
        let id = &memories[0].id;
        let specs = db.list_memory_sentinel_specs(id).unwrap();
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].target, "Cargo.toml");
        assert_eq!(specs[0].polarity, MemorySentinelPolarity::Gate);
        assert_eq!(
            db.list_search_index_jobs(&workspace_id, None)
                .unwrap()
                .len(),
            1
        );
        let snapshot = source_snapshot(&db);
        // The broken audit destination remains in place. Replays prove the
        // already-committed guards and do not attempt companion or file writes.
        replayed(write().unwrap(), id);
        replayed(write().unwrap(), id);
        assert_eq!(source_snapshot(&db), snapshot);
    }

    let fixture = Fixture::new();
    fs::create_dir(fixture.workspace.join(".ee/audit.jsonl")).unwrap();
    let commitment = crate::models::seal_commitment_for_content(b"Unseen prediction.").unwrap();
    let companions = RememberCreationCompanions {
        seal: Some(RememberSealInput {
            content_commitment: &commitment,
            sealed_at: "2026-01-01T00:00:00Z",
        }),
        ..Default::default()
    };
    let error = remember_memory_with_creation_companions(
        &fixture.options(crate::models::MEMORY_SEAL_PLACEHOLDER_CONTENT),
        &controls(None),
        &[],
        None,
        companions,
    )
    .unwrap_err();
    assert!(
        error.message().contains("failed to append audit JSONL"),
        "{error:?}"
    );
    let memories = fixture
        .db
        .list_memories(&fixture.workspace_id, None, true)
        .unwrap();
    assert_eq!(memories.len(), 1);
    assert_eq!(
        memories[0].content,
        crate::models::MEMORY_SEAL_PLACEHOLDER_CONTENT
    );
    let seal = fixture
        .db
        .get_memory_seal(&memories[0].id)
        .unwrap()
        .unwrap();
    assert_eq!(seal.content_commitment, commitment);
    assert_eq!(seal.sealed_at, "2026-01-01T00:00:00Z");
    assert!(seal.revealed_at.is_none());
}

#[test]
fn keyed_guard_replays_bind_canonical_config_and_ignore_check_results() {
    let fixture = Fixture::new();
    let gates = vec![
        "path_exists:src/b.rs".to_owned(),
        "path_exists:src/a.rs".to_owned(),
        "path_exists:src/b.rs".to_owned(),
    ];
    let revivals = vec!["path_exists:src/revive.rs".to_owned()];
    let companions = RememberCreationCompanions {
        sentinels: &gates,
        revive_when: &revivals,
        stale_threshold_seconds: Some(300),
        seal: None,
    };
    let options = fixture.options("A replay uses the same decision and complete guard set.");
    let controls = controls(Some("canonical-guards"));
    let original_hash = remember_request_hash(&options, &[], None).unwrap();
    assert_eq!(
        RememberCreationCompanions::default()
            .request_hash(&original_hash)
            .unwrap(),
        original_hash
    );
    let expected_hash = companions.request_hash(&original_hash).unwrap();
    assert_ne!(expected_hash, original_hash);
    let report = created(
        remember_memory_with_creation_companions(&options, &controls, &[], None, companions)
            .unwrap(),
    );
    let id = report.memory_id.to_string();
    assert_eq!(
        fixture
            .db
            .get_remember_idempotency_key(&fixture.workspace_id, "canonical-guards")
            .unwrap()
            .unwrap()
            .content_hash,
        expected_hash
    );
    let specs = fixture.db.list_memory_sentinel_specs(&id).unwrap();
    assert_eq!(specs.len(), 3);
    fixture
        .db
        .execute_raw("UPDATE memory_sentinel_specs SET updated_at = '2026-01-02T00:00:00Z'")
        .unwrap();
    fixture
        .db
        .insert_memory_sentinel_result(
            &MemorySentinelResult::new(MemorySentinelResultInput {
                spec_hash: specs[0].spec_hash.clone(),
                status: MemorySentinelResultStatus::Fail,
                checked_at: "2026-01-02T00:00:00Z".to_owned(),
                evidence_summary: "The requested path is currently absent.".to_owned(),
                stale_threshold_seconds: Some(300),
            })
            .unwrap(),
        )
        .unwrap();
    let reordered_gates = vec![
        "path_exists:src/a.rs".to_owned(),
        "path_exists:src/b.rs".to_owned(),
    ];
    let reordered = RememberCreationCompanions {
        sentinels: &reordered_gates,
        ..companions
    };
    let snapshot = source_snapshot(&fixture.db);
    replayed(
        remember_memory_with_creation_companions(&options, &controls, &[], None, reordered)
            .unwrap(),
        &id,
    );
    assert_eq!(source_snapshot(&fixture.db), snapshot);
    let other_gates = vec!["path_exists:src/other.rs".to_owned()];
    for changed in [
        RememberCreationCompanions {
            stale_threshold_seconds: Some(301),
            ..reordered
        },
        RememberCreationCompanions {
            sentinels: &other_gates,
            ..reordered
        },
        RememberCreationCompanions {
            sentinels: &revivals,
            revive_when: &reordered_gates,
            ..reordered
        },
        RememberCreationCompanions::default(),
    ] {
        let error =
            remember_memory_with_creation_companions(&options, &controls, &[], None, changed)
                .unwrap_err();
        assert_eq!(error.code(), REMEMBER_IDEMPOTENCY_CONFLICT_CODE);
        assert_eq!(source_snapshot(&fixture.db), snapshot);
    }

    // Matching the new request hash is insufficient if the stored guard was
    // subsequently corrupted; replay must never silently recreate it.
    fixture
        .db
        .execute_raw("UPDATE memory_sentinel_specs SET expected_predicate = 'different'")
        .unwrap();
    let corrupted = source_snapshot(&fixture.db);
    let error = remember_memory_with_creation_companions(&options, &controls, &[], None, reordered)
        .unwrap_err();
    assert_eq!(error.code(), REMEMBER_IDEMPOTENCY_CONFLICT_CODE);
    assert_eq!(source_snapshot(&fixture.db), corrupted);
}

#[test]
fn legacy_keys_require_the_complete_exact_guard_set_without_retrofitting() {
    for attached in 0..=2 {
        let fixture = Fixture::new();
        let gates = vec!["path_exists:src/legacy.rs".to_owned()];
        let revivals = vec!["path_exists:src/legacy-revive.rs".to_owned()];
        let companions = RememberCreationCompanions {
            sentinels: &gates,
            revive_when: &revivals,
            stale_threshold_seconds: Some(300),
            seal: None,
        };
        let options = fixture.options("An existing key must prove its original guards.");
        let controls = controls(Some("legacy-guard-key"));
        let report = created(remember_memory_with_controls(&options, &controls).unwrap());
        let id = report.memory_id.to_string();
        let old_key = fixture
            .db
            .get_remember_idempotency_key(&fixture.workspace_id, "legacy-guard-key")
            .unwrap()
            .unwrap();
        assert_eq!(
            old_key.content_hash,
            remember_request_hash(&options, &[], None).unwrap()
        );
        for (raw, polarity, provenance) in [
            (
                &gates[0],
                MemorySentinelPolarity::Gate,
                "ee remember --sentinel",
            ),
            (
                &revivals[0],
                MemorySentinelPolarity::Revive,
                "ee remember --revive-when",
            ),
        ]
        .into_iter()
        .take(attached)
        {
            let spec =
                MemorySentinelSpec::from_raw(&id, raw, polarity, None, provenance, Some(300))
                    .unwrap();
            fixture.db.upsert_memory_sentinel_spec(&spec).unwrap();
        }
        let before = source_snapshot(&fixture.db);
        let result =
            remember_memory_with_creation_companions(&options, &controls, &[], None, companions);
        if attached == 2 {
            replayed(result.unwrap(), &id);
            assert_eq!(
                fixture
                    .db
                    .get_remember_idempotency_key(&fixture.workspace_id, "legacy-guard-key")
                    .unwrap()
                    .unwrap(),
                old_key,
                "legacy proof does not rewrite identity"
            );
        } else {
            assert_eq!(
                result.unwrap_err().code(),
                REMEMBER_IDEMPOTENCY_CONFLICT_CODE
            );
        }
        assert_eq!(
            source_snapshot(&fixture.db),
            before,
            "legacy replay never adds missing guards"
        );

        // A dry run uses the same proof and cannot repair a partial legacy
        // write, even though it does not acquire a writer.
        let dry = RememberMemoryOptions {
            dry_run: true,
            ..options.clone()
        };
        let dry_result =
            remember_memory_with_creation_companions(&dry, &controls, &[], None, companions);
        if attached == 2 {
            replayed(dry_result.unwrap(), &id);
        } else {
            assert_eq!(
                dry_result.unwrap_err().code(),
                REMEMBER_IDEMPOTENCY_CONFLICT_CODE
            );
        }
        assert_eq!(source_snapshot(&fixture.db), before);
    }
}

#[test]
fn concurrent_guarded_key_creates_one_guarded_memory_and_replays_the_winner() {
    let fixture = Fixture::new();
    let gates = vec!["path_exists:src/shared.rs".to_owned()];
    let companions = RememberCreationCompanions {
        sentinels: &gates,
        stale_threshold_seconds: Some(60),
        ..Default::default()
    };
    let options = fixture.options("Concurrent guarded retries share one stored decision.");
    let original_hash = remember_request_hash(&options, &[], None).unwrap();
    let hash = companions.request_hash(&original_hash).unwrap();
    let barrier = std::sync::Barrier::new(2);
    let outcomes = std::thread::scope(|scope| {
        let workers = [0, 1].map(|_| {
            let options = &options;
            let barrier = &barrier;
            let original_hash = &original_hash;
            let hash = &hash;
            scope.spawn(move || {
                barrier.wait();
                // Deliberately bypass the public preflight lookup: both
                // requests reach the source transaction with fresh candidate
                // IDs. The loser must prove guards against the winner's ID.
                remember_memory_outcome_inner_with_store(
                    options,
                    &mut RememberIdSource::Ambient,
                    None,
                    true,
                    None,
                    &[],
                    None,
                    Some(RememberIdempotencyRequest {
                        key: "concurrent-guard-key",
                        request_hash: hash,
                        legacy_request_hash: Some(original_hash),
                        creation_companions: companions,
                    }),
                    companions,
                )
            })
        });
        workers.map(|worker| worker.join().unwrap().unwrap())
    });
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| matches!(outcome, RememberOutcome::Created(_)))
            .count(),
        1
    );
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| matches!(outcome, RememberOutcome::AlreadyRecorded(_)))
            .count(),
        1
    );
    let key = fixture
        .db
        .get_remember_idempotency_key(&fixture.workspace_id, "concurrent-guard-key")
        .unwrap()
        .unwrap();
    for outcome in outcomes {
        match outcome {
            RememberOutcome::Created(report) => {
                assert_eq!(report.memory_id.to_string(), key.memory_id)
            }
            RememberOutcome::AlreadyRecorded(report) => assert_eq!(report.memory_id, key.memory_id),
            other => panic!("unexpected concurrent outcome: {other:?}"),
        }
    }
    assert_eq!(
        fixture
            .db
            .list_memories(&fixture.workspace_id, None, true)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        fixture
            .db
            .list_search_index_jobs(&fixture.workspace_id, None)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        fixture
            .db
            .list_audit_by_action(audit_actions::MEMORY_CREATE, None)
            .unwrap()
            .len(),
        1
    );
    let specs = fixture
        .db
        .list_memory_sentinel_specs(&key.memory_id)
        .unwrap();
    assert_eq!(specs.len(), 1);
    assert_eq!(specs[0].memory_id, key.memory_id);
    assert_eq!(specs[0].target, "src/shared.rs");
    let before = source_snapshot(&fixture.db);
    let changed = RememberCreationCompanions {
        stale_threshold_seconds: Some(61),
        ..companions
    };
    let changed_hash = changed.request_hash(&original_hash).unwrap();
    let error = remember_memory_outcome_inner_with_store(
        &options,
        &mut RememberIdSource::Ambient,
        None,
        true,
        None,
        &[],
        None,
        Some(RememberIdempotencyRequest {
            key: "concurrent-guard-key",
            request_hash: &changed_hash,
            legacy_request_hash: Some(&original_hash),
            creation_companions: changed,
        }),
        changed,
    )
    .unwrap_err();
    assert_eq!(error.code(), REMEMBER_IDEMPOTENCY_CONFLICT_CODE);
    assert_eq!(source_snapshot(&fixture.db), before);
}

#[test]
fn companion_validation_and_dry_runs_do_not_create_source_or_guard_state() {
    let fixture = Fixture::new();
    let gates = vec!["path_exists:Cargo.toml".to_owned()];
    let companions = RememberCreationCompanions {
        sentinels: &gates,
        ..Default::default()
    };
    let mut options = fixture.options("A guarded preview is not a persisted decision.");
    options.dry_run = true;
    let before = source_snapshot(&fixture.db);
    let report = created(
        remember_memory_with_creation_companions(
            &options,
            &controls(Some("preview-guards")),
            &[],
            None,
            companions,
        )
        .unwrap(),
    );
    assert!(report.dry_run);
    assert_eq!(source_snapshot(&fixture.db), before);

    for seconds in [0, u64::MAX] {
        let invalid = RememberCreationCompanions {
            stale_threshold_seconds: Some(seconds),
            ..companions
        };
        assert!(matches!(
            remember_memory_with_creation_companions(&options, &controls(None), &[], None, invalid,),
            Err(DomainError::Usage { .. })
        ));
        let paths = super::super::global_store::GlobalStorePaths::from_data_root(
            &fixture.workspace.join(format!("invalid-global-{seconds}")),
        );
        assert!(matches!(
            remember_global_memory_with_creation_companions_at_paths(
                &RememberMemoryOptions {
                    dry_run: false,
                    ..options.clone()
                },
                &controls(None),
                &[],
                paths.clone(),
                invalid,
            ),
            Err(DomainError::Usage { .. })
        ));
        assert!(!paths.database_path.exists());
    }
    let commitment = crate::models::seal_commitment_for_content(b"Preview evidence.").unwrap();
    let sealed = RememberCreationCompanions {
        seal: Some(RememberSealInput {
            content_commitment: &commitment,
            sealed_at: "2026-01-01T00:00:00Z",
        }),
        ..Default::default()
    };
    let seal_options = RememberMemoryOptions {
        content: crate::models::MEMORY_SEAL_PLACEHOLDER_CONTENT,
        ..options.clone()
    };
    assert!(
        created(
            remember_memory_with_creation_companions(
                &seal_options,
                &controls(None),
                &[],
                None,
                sealed,
            )
            .unwrap()
        )
        .dry_run
    );
    assert!(matches!(
        remember_memory_with_creation_companions(
            &seal_options,
            &controls(Some("forbidden-seal-key")),
            &[],
            None,
            sealed,
        ),
        Err(DomainError::Usage { .. })
    ));
    let global_paths = super::super::global_store::GlobalStorePaths::from_data_root(
        &fixture.workspace.join("forbidden-global-seal"),
    );
    assert!(matches!(
        remember_global_memory_with_creation_companions_at_paths(
            &seal_options,
            &controls(None),
            &[],
            global_paths.clone(),
            sealed,
        ),
        Err(DomainError::Usage { .. })
    ));
    assert!(!global_paths.database_path.exists());
    assert_eq!(source_snapshot(&fixture.db), before);
}
