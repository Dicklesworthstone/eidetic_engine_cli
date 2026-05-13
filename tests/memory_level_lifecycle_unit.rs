#![allow(clippy::expect_used, clippy::unwrap_used)]

use ee::core::curate::{CurateApplyOptions, apply_curation_candidate};
use ee::core::memory::{
    ExpireMemoryOptions, MemoryLevelOptions, RememberMemoryOptions, expire_memory, remember_memory,
    update_memory_level,
};
use ee::core::memory_lifecycle::{
    LEVEL_TRANSITION_CONCURRENT_CONFLICT_CODE, LEVEL_TRANSITION_REQUIRES_EVIDENCE_CODE,
    LEVEL_TRANSITION_TOMBSTONED_REJECTED_CODE, MemoryLevelTransitionAudit, MemoryLifecycleState,
    TRANSITIONS, level_transition_audit_details, transition_for,
};
use ee::db::{
    ApplyMemoryDecayDemotionInput, CreateCurationCandidateInput, CreateMemoryInput,
    CreateWorkspaceInput, DbConnection, MEMORY_LEVEL_TRANSITION_AUDIT_SCHEMA_V1, audit_actions,
};
use ee::output::error_response_json;
use serde_json::Value;

const WORKSPACE_ID: &str = "wsp_lifecycle00000000000000000";

fn open_db() -> DbConnection {
    let connection = DbConnection::open_memory().expect("memory database opens");
    connection.migrate().expect("migrations apply");
    connection
        .insert_workspace(
            WORKSPACE_ID,
            &CreateWorkspaceInput {
                path: "/tmp/ee-memory-lifecycle".to_owned(),
                name: Some("memory-lifecycle".to_owned()),
            },
        )
        .expect("workspace inserts");
    connection
}

fn insert_memory(
    connection: &DbConnection,
    memory_id: &str,
    level: &str,
    workflow_id: Option<&str>,
) {
    connection
        .insert_memory(
            memory_id,
            &CreateMemoryInput {
                workspace_id: WORKSPACE_ID.to_owned(),
                level: level.to_owned(),
                kind: "fact".to_owned(),
                content: format!("memory lifecycle fixture {memory_id}"),
                workflow_id: workflow_id.map(str::to_owned),
                confidence: 0.8,
                utility: 0.8,
                importance: 0.8,
                provenance_uri: Some("test://memory-lifecycle".to_owned()),
                trust_class: "human_explicit".to_owned(),
                trust_subclass: None,
                tags: vec!["lifecycle".to_owned()],
                valid_from: None,
                valid_to: None,
            },
        )
        .expect("memory inserts");
}

#[test]
fn transition_table_is_total_over_known_events_and_states() {
    let events = [
        "workflow.completed",
        "manual.promote_to_episodic",
        "repeated_observation",
        "manual.promote_to_semantic",
        "curate.apply",
        "manual.promote_to_procedural",
        "feedback.harmful_decay",
        "manual.demote_to_semantic",
        "valid_to.set",
        "decay.l3",
        "manual.tombstone",
        "unknown.event",
    ];
    let mut checked = 0_u32;
    for state in MemoryLifecycleState::all() {
        for event in events {
            let expected = TRANSITIONS
                .iter()
                .any(|transition| transition.from == state && transition.event == event);
            assert_eq!(
                transition_for(state, event).is_some(),
                expected,
                "{} + {event}",
                state.as_str()
            );
            checked = checked.saturating_add(1);
        }
    }

    assert_eq!(checked, 60);
    assert!(
        TRANSITIONS
            .iter()
            .all(|transition| { !transition.reason.is_empty() && !transition.evidence.is_empty() })
    );
    assert!(transition_for(MemoryLifecycleState::Tombstoned, "manual.tombstone").is_none());
}

#[test]
fn level_transition_audit_details_include_hash_and_evidence() {
    let details = level_transition_audit_details(&MemoryLevelTransitionAudit {
        memory_id: "mem_lifecycleaudit000000000001",
        previous_level: "working",
        new_level: "episodic",
        reason: "workflow_close",
        automatic: true,
        event: "workflow.completed",
        evidence_refs: &["wf-release"],
        source_action: Some("ee workflow close"),
    });
    let parsed: Value = serde_json::from_str(&details).expect("details parse as JSON");

    assert_eq!(
        parsed["schema"],
        Value::String(MEMORY_LEVEL_TRANSITION_AUDIT_SCHEMA_V1.to_owned())
    );
    assert_eq!(parsed["previousLevel"], Value::String("working".to_owned()));
    assert_eq!(parsed["newLevel"], Value::String("episodic".to_owned()));
    assert_eq!(parsed["automatic"], Value::Bool(true));
    assert_eq!(
        parsed["evidenceRefs"][0],
        Value::String("wf-release".to_owned())
    );
    assert!(
        parsed["detailsHash"]
            .as_str()
            .is_some_and(|hash| { hash.starts_with("blake3:") && hash.len() > "blake3:".len() })
    );
}

#[test]
fn workflow_close_writes_canonical_level_transition_audit() {
    let connection = open_db();
    let memory_id = "mem_lifecycleworkflow000000010";
    insert_memory(&connection, memory_id, "working", Some("wf-lifecycle"));

    let promotions = connection
        .promote_workflow_working_memories_audited(
            WORKSPACE_ID,
            "wf-lifecycle",
            "test",
            "2026-05-13T00:00:00Z",
        )
        .expect("workflow close promotions succeed");

    assert_eq!(promotions.len(), 1);
    assert_eq!(promotions[0].memory_id, memory_id);
    let audit = connection
        .get_audit(&promotions[0].audit_id)
        .expect("audit query succeeds")
        .expect("audit row exists");
    assert_eq!(audit.action, audit_actions::MEMORY_LEVEL_TRANSITION);
    let details: Value =
        serde_json::from_str(audit.details.as_deref().expect("details exist")).expect("json");
    assert_eq!(
        details["previousLevel"],
        Value::String("working".to_owned())
    );
    assert_eq!(details["newLevel"], Value::String("episodic".to_owned()));
    assert_eq!(
        details["event"],
        Value::String("workflow.completed".to_owned())
    );
}

#[test]
fn manual_level_command_applies_all_adjacent_transitions_with_transition_audit() {
    let temp = tempfile::tempdir().expect("tempdir creates");
    let database_path = temp.path().join("ee.db");
    let cases = [
        ("working", "episodic", "manual.promote_to_episodic"),
        ("episodic", "semantic", "manual.promote_to_semantic"),
        ("semantic", "procedural", "manual.promote_to_procedural"),
        ("procedural", "semantic", "manual.demote_to_semantic"),
    ];

    for (initial_level, target_level, expected_event) in cases {
        let remembered = remember_memory(&RememberMemoryOptions {
            workspace_path: temp.path(),
            database_path: Some(&database_path),
            content: "A memory that should move through a manual lifecycle edge.",
            workflow_id: None,
            level: initial_level,
            kind: "fact",
            tags: Some("lifecycle"),
            confidence: 0.8,
            source: Some("file:///tmp/manual-level-lifecycle"),
            allow_secret_mention: false,
            valid_from: None,
            valid_to: None,
            dry_run: false,
            auto_link: true,
            propose_candidates: false,
        })
        .expect("memory persists");
        let memory_id = remembered.memory_id.to_string();

        let report = update_memory_level(&MemoryLevelOptions {
            workspace_path: temp.path(),
            database_path: &remembered.database_path,
            memory_id: &memory_id,
            level: target_level,
            expected_level: None,
            reason: Some("task completed with durable evidence"),
            actor: Some("lifecycle-test"),
            dry_run: false,
            include_tombstoned: false,
        })
        .expect("manual level transition succeeds");
        assert_eq!(report.status, "transitioned");
        assert_eq!(report.previous_level, initial_level);
        assert_eq!(report.level, target_level);
        assert_eq!(report.event.as_deref(), Some(expected_event));
        assert!(report.audit_id.is_some());
        assert!(report.index_job_id.is_some());

        let connection = DbConnection::open_file(&remembered.database_path).expect("db opens");
        let memory = connection
            .get_memory(&memory_id)
            .expect("memory query succeeds")
            .expect("memory exists");
        assert_eq!(memory.level, target_level);
        let audits = connection
            .list_audit_by_target("memory", &memory_id, None)
            .expect("target audit query succeeds");
        let transition = audits
            .iter()
            .find(|entry| entry.action == audit_actions::MEMORY_LEVEL_TRANSITION)
            .expect("canonical transition audit exists");
        let details: Value = serde_json::from_str(
            transition
                .details
                .as_deref()
                .expect("transition details exist"),
        )
        .expect("transition details parse");
        assert_eq!(
            details["previousLevel"],
            Value::String(initial_level.to_owned())
        );
        assert_eq!(details["newLevel"], Value::String(target_level.to_owned()));
        assert_eq!(details["event"], Value::String(expected_event.to_owned()));
        assert_eq!(
            details["sourceAction"],
            Value::String("memory.level".to_owned())
        );
    }
}

#[test]
fn manual_level_command_requires_reason_evidence() {
    let temp = tempfile::tempdir().expect("tempdir creates");
    let database_path = temp.path().join("ee.db");
    let remembered = remember_memory(&RememberMemoryOptions {
        workspace_path: temp.path(),
        database_path: Some(&database_path),
        content: "A working memory missing manual transition evidence.",
        workflow_id: None,
        level: "working",
        kind: "fact",
        tags: Some("lifecycle"),
        confidence: 0.8,
        source: Some("file:///tmp/manual-level-requires-reason"),
        allow_secret_mention: false,
        valid_from: None,
        valid_to: None,
        dry_run: false,
        auto_link: true,
        propose_candidates: false,
    })
    .expect("working memory persists");
    let memory_id = remembered.memory_id.to_string();

    let error = update_memory_level(&MemoryLevelOptions {
        workspace_path: temp.path(),
        database_path: &remembered.database_path,
        memory_id: &memory_id,
        level: "episodic",
        expected_level: None,
        reason: None,
        actor: Some("lifecycle-test"),
        dry_run: false,
        include_tombstoned: false,
    })
    .expect_err("manual transition without reason is rejected");

    assert_eq!(error.code(), LEVEL_TRANSITION_REQUIRES_EVIDENCE_CODE);
    assert!(format!("{error}").contains("requires evidence"));
    let error_json: Value =
        serde_json::from_str(&error_response_json(&error)).expect("error JSON parses");
    assert_eq!(
        error_json["error"]["code"],
        Value::String(LEVEL_TRANSITION_REQUIRES_EVIDENCE_CODE.to_owned())
    );
    assert_eq!(
        error_json["error"]["severity"],
        Value::String("medium".to_owned())
    );
    assert_eq!(
        error_json["degraded"][0]["code"],
        Value::String(LEVEL_TRANSITION_REQUIRES_EVIDENCE_CODE.to_owned())
    );
    let connection = DbConnection::open_file(&remembered.database_path).expect("db opens");
    let memory = connection
        .get_memory(&memory_id)
        .expect("memory query succeeds")
        .expect("memory exists");
    assert_eq!(memory.level, "working");
}

#[test]
fn manual_level_command_rejects_stale_expected_level_with_transition_code() {
    let temp = tempfile::tempdir().expect("tempdir creates");
    let database_path = temp.path().join("ee.db");
    let remembered = remember_memory(&RememberMemoryOptions {
        workspace_path: temp.path(),
        database_path: Some(&database_path),
        content: "A working memory that another writer will advance first.",
        workflow_id: None,
        level: "working",
        kind: "fact",
        tags: Some("lifecycle"),
        confidence: 0.8,
        source: Some("file:///tmp/manual-level-concurrent-conflict"),
        allow_secret_mention: false,
        valid_from: None,
        valid_to: None,
        dry_run: false,
        auto_link: true,
        propose_candidates: false,
    })
    .expect("working memory persists");
    let memory_id = remembered.memory_id.to_string();

    update_memory_level(&MemoryLevelOptions {
        workspace_path: temp.path(),
        database_path: &remembered.database_path,
        memory_id: &memory_id,
        level: "episodic",
        expected_level: Some("working"),
        reason: Some("first writer completed the workflow"),
        actor: Some("lifecycle-test"),
        dry_run: false,
        include_tombstoned: false,
    })
    .expect("first transition succeeds");

    let error = update_memory_level(&MemoryLevelOptions {
        workspace_path: temp.path(),
        database_path: &remembered.database_path,
        memory_id: &memory_id,
        level: "episodic",
        expected_level: Some("working"),
        reason: Some("stale worker retries the planned transition"),
        actor: Some("lifecycle-test"),
        dry_run: false,
        include_tombstoned: false,
    })
    .expect_err("stale compare-and-set transition is rejected");

    assert_eq!(error.code(), LEVEL_TRANSITION_CONCURRENT_CONFLICT_CODE);
    assert!(format!("{error}").contains("concurrent"));
    let error_json: Value =
        serde_json::from_str(&error_response_json(&error)).expect("error JSON parses");
    assert_eq!(
        error_json["error"]["code"],
        Value::String(LEVEL_TRANSITION_CONCURRENT_CONFLICT_CODE.to_owned())
    );
    assert_eq!(
        error_json["degraded"][0]["code"],
        Value::String(LEVEL_TRANSITION_CONCURRENT_CONFLICT_CODE.to_owned())
    );
    assert_eq!(
        error_json["error"]["details"]["observedLevel"],
        Value::String("episodic".to_owned())
    );
}

#[test]
fn manual_level_command_rejects_tombstoned_memory_with_transition_code() {
    let temp = tempfile::tempdir().expect("tempdir creates");
    let database_path = temp.path().join("ee.db");
    let remembered = remember_memory(&RememberMemoryOptions {
        workspace_path: temp.path(),
        database_path: Some(&database_path),
        content: "A semantic memory that will be tombstoned.",
        workflow_id: None,
        level: "semantic",
        kind: "fact",
        tags: Some("lifecycle"),
        confidence: 0.8,
        source: Some("file:///tmp/manual-level-tombstoned"),
        allow_secret_mention: false,
        valid_from: None,
        valid_to: None,
        dry_run: false,
        auto_link: true,
        propose_candidates: false,
    })
    .expect("memory persists");
    let remembered_id = remembered.memory_id.to_string();
    let file_connection = DbConnection::open_file(&remembered.database_path).expect("db opens");
    file_connection
        .tombstone_memory_audited(
            &remembered_id,
            &remembered.workspace_id,
            Some("lifecycle-test"),
            Some("retired before transition"),
        )
        .expect("manual tombstone succeeds");

    let error = update_memory_level(&MemoryLevelOptions {
        workspace_path: temp.path(),
        database_path: &remembered.database_path,
        memory_id: &remembered_id,
        level: "procedural",
        expected_level: None,
        reason: Some("manual promotion after tombstone"),
        actor: Some("lifecycle-test"),
        dry_run: false,
        include_tombstoned: false,
    })
    .expect_err("tombstoned memory transition is rejected");

    assert_eq!(error.code(), LEVEL_TRANSITION_TOMBSTONED_REJECTED_CODE);
}

#[test]
fn decay_demotion_writes_legacy_and_canonical_audits() {
    let connection = open_db();
    let memory_id = "mem_lifecycledecaydemote000010";
    insert_memory(&connection, memory_id, "procedural", None);

    let legacy_audit_id = connection
        .apply_memory_decay_demotion_audited(
            memory_id,
            &ApplyMemoryDecayDemotionInput {
                workspace_id: WORKSPACE_ID.to_owned(),
                level: "semantic".to_owned(),
                importance: 0.4,
                updated_at: "2026-05-13T00:00:00Z".to_owned(),
                actor: Some("test".to_owned()),
                details: r#"{"reason":"test_decay"}"#.to_owned(),
            },
        )
        .expect("decay demotion succeeds")
        .expect("legacy audit row is returned");
    let legacy = connection
        .get_audit(&legacy_audit_id)
        .expect("legacy audit query succeeds")
        .expect("legacy audit exists");
    assert_eq!(legacy.action, audit_actions::MEMORY_DECAY_DEMOTE);

    let audits = connection
        .list_audit_by_target("memory", memory_id, None)
        .expect("target audit query succeeds");
    let transition = audits
        .iter()
        .find(|entry| entry.action == audit_actions::MEMORY_LEVEL_TRANSITION)
        .expect("canonical transition audit exists");
    let details: Value = serde_json::from_str(
        transition
            .details
            .as_deref()
            .expect("transition details exist"),
    )
    .expect("transition details parse");
    assert_eq!(
        details["previousLevel"],
        Value::String("procedural".to_owned())
    );
    assert_eq!(details["newLevel"], Value::String("semantic".to_owned()));
    assert_eq!(
        details["sourceAction"],
        Value::String("memory.decay_demote".to_owned())
    );
}

#[test]
fn manual_tombstone_writes_legacy_and_canonical_audits_for_all_levels() {
    let connection = open_db();
    let cases = [
        ("mem_10000000000000000000000001", "working"),
        ("mem_10000000000000000000000002", "episodic"),
        ("mem_10000000000000000000000003", "semantic"),
        ("mem_10000000000000000000000004", "procedural"),
    ];

    for (memory_id, level) in cases {
        insert_memory(&connection, memory_id, level, None);

        let legacy_audit_id = connection
            .tombstone_memory_audited(memory_id, WORKSPACE_ID, Some("test"), Some("superseded"))
            .expect("manual tombstone succeeds")
            .expect("legacy audit row is returned");
        let legacy = connection
            .get_audit(&legacy_audit_id)
            .expect("legacy audit query succeeds")
            .expect("legacy audit exists");
        assert_eq!(legacy.action, audit_actions::MEMORY_TOMBSTONE);

        let audits = connection
            .list_audit_by_target("memory", memory_id, None)
            .expect("target audit query succeeds");
        let transition = audits
            .iter()
            .find(|entry| entry.action == audit_actions::MEMORY_LEVEL_TRANSITION)
            .expect("canonical transition audit exists");
        let details: Value = serde_json::from_str(
            transition
                .details
                .as_deref()
                .expect("transition details exist"),
        )
        .expect("transition details parse");
        assert_eq!(details["previousLevel"], Value::String(level.to_owned()));
        assert_eq!(details["newLevel"], Value::String("tombstoned".to_owned()));
        assert_eq!(
            details["event"],
            Value::String("manual.tombstone".to_owned())
        );
        assert_eq!(
            details["sourceAction"],
            Value::String(audit_actions::MEMORY_TOMBSTONE.to_owned())
        );
    }
}

#[test]
fn decay_tombstone_writes_legacy_and_canonical_audits_for_all_levels() {
    let connection = open_db();
    let cases = [
        ("mem_20000000000000000000000001", "working"),
        ("mem_20000000000000000000000002", "episodic"),
        ("mem_20000000000000000000000003", "semantic"),
        ("mem_20000000000000000000000004", "procedural"),
    ];

    for (memory_id, level) in cases {
        insert_memory(&connection, memory_id, level, None);

        let legacy_audit_id = connection
            .tombstone_memory_decay_audited(
                memory_id,
                WORKSPACE_ID,
                Some("test"),
                r#"{"reason":"ttl_expired"}"#,
            )
            .expect("decay tombstone succeeds")
            .expect("legacy audit row is returned");
        let legacy = connection
            .get_audit(&legacy_audit_id)
            .expect("legacy audit query succeeds")
            .expect("legacy audit exists");
        assert_eq!(legacy.action, audit_actions::MEMORY_DECAY_TOMBSTONE);

        let audits = connection
            .list_audit_by_target("memory", memory_id, None)
            .expect("target audit query succeeds");
        let transition = audits
            .iter()
            .find(|entry| entry.action == audit_actions::MEMORY_LEVEL_TRANSITION)
            .expect("canonical transition audit exists");
        let details: Value = serde_json::from_str(
            transition
                .details
                .as_deref()
                .expect("transition details exist"),
        )
        .expect("transition details parse");
        assert_eq!(details["previousLevel"], Value::String(level.to_owned()));
        assert_eq!(details["newLevel"], Value::String("tombstoned".to_owned()));
        assert_eq!(details["event"], Value::String("decay.l3".to_owned()));
        assert_eq!(
            details["sourceAction"],
            Value::String(audit_actions::MEMORY_DECAY_TOMBSTONE.to_owned())
        );
    }
}

#[test]
fn memory_expire_demotes_semantic_to_episodic_with_transition_audit() {
    let temp = tempfile::tempdir().expect("tempdir creates");
    let database_path = temp.path().join("ee.db");
    let remembered = remember_memory(&RememberMemoryOptions {
        workspace_path: temp.path(),
        database_path: Some(&database_path),
        content: "A semantic fact that became time-bound.",
        workflow_id: None,
        level: "semantic",
        kind: "fact",
        tags: Some("lifecycle"),
        confidence: 0.9,
        source: Some("file:///tmp/memory-expire-lifecycle"),
        allow_secret_mention: false,
        valid_from: None,
        valid_to: None,
        dry_run: false,
        auto_link: true,
        propose_candidates: false,
    })
    .expect("semantic memory persists");
    let memory_id = remembered.memory_id.to_string();

    let report = expire_memory(&ExpireMemoryOptions {
        workspace_path: temp.path(),
        database_path: &remembered.database_path,
        memory_id: &memory_id,
        reason: Some("became time-bound"),
        actor: Some("lifecycle-test"),
        dry_run: false,
        include_tombstoned: false,
    })
    .expect("memory expire succeeds");
    assert_eq!(report.status, "expired");

    let connection = DbConnection::open_file(&remembered.database_path).expect("db opens");
    let memory = connection
        .get_memory(&memory_id)
        .expect("memory query succeeds")
        .expect("memory exists");
    assert_eq!(memory.level, "episodic");
    assert!(memory.valid_to.is_some());

    let audits = connection
        .list_audit_by_target("memory", &memory_id, None)
        .expect("target audit query succeeds");
    let transition = audits
        .iter()
        .find(|entry| entry.action == audit_actions::MEMORY_LEVEL_TRANSITION)
        .expect("canonical transition audit exists");
    let details: Value = serde_json::from_str(
        transition
            .details
            .as_deref()
            .expect("transition details exist"),
    )
    .expect("transition details parse");
    assert_eq!(
        details["previousLevel"],
        Value::String("semantic".to_owned())
    );
    assert_eq!(details["newLevel"], Value::String("episodic".to_owned()));
    assert_eq!(details["event"], Value::String("valid_to.set".to_owned()));
    assert_eq!(
        details["sourceAction"],
        Value::String("memory.expire".to_owned())
    );
}

#[test]
fn curate_apply_promote_moves_episodic_to_semantic_with_transition_audit() {
    let temp = tempfile::tempdir().expect("tempdir creates");
    let database_path = temp.path().join("ee.db");
    let remembered = remember_memory(&RememberMemoryOptions {
        workspace_path: temp.path(),
        database_path: Some(&database_path),
        content: "Repeated observation about the release process.",
        workflow_id: None,
        level: "episodic",
        kind: "observation",
        tags: Some("lifecycle"),
        confidence: 0.8,
        source: Some("file:///tmp/curate-promote-lifecycle"),
        allow_secret_mention: false,
        valid_from: None,
        valid_to: None,
        dry_run: false,
        auto_link: true,
        propose_candidates: false,
    })
    .expect("episodic memory persists");
    let memory_id = remembered.memory_id.to_string();
    let candidate_id = "curate_00000000000000000000000001";

    let connection = DbConnection::open_file(&remembered.database_path).expect("db opens");
    connection
        .insert_curation_candidate(
            candidate_id,
            &CreateCurationCandidateInput {
                workspace_id: remembered.workspace_id.clone(),
                candidate_type: "promote".to_owned(),
                target_memory_id: memory_id.clone(),
                proposed_content: None,
                proposed_confidence: None,
                proposed_trust_class: None,
                source_type: "agent_inference".to_owned(),
                source_id: Some("cluster:release-process:3".to_owned()),
                reason: "Three consistent episodic observations support a semantic fact."
                    .to_owned(),
                confidence: 0.86,
                status: Some("approved".to_owned()),
                created_at: Some("2026-05-13T00:00:00Z".to_owned()),
                ttl_expires_at: None,
            },
        )
        .expect("candidate inserts");

    let report = apply_curation_candidate(&CurateApplyOptions {
        workspace_path: temp.path(),
        database_path: Some(&remembered.database_path),
        candidate_id,
        actor: Some("lifecycle-test"),
        dry_run: false,
    })
    .expect("curate apply succeeds");
    assert_eq!(report.application.status, "applied");
    assert!(
        report
            .application
            .changes
            .iter()
            .any(|change| change.field == "level")
    );

    let memory = connection
        .get_memory(&memory_id)
        .expect("memory query succeeds")
        .expect("memory exists");
    assert_eq!(memory.level, "semantic");

    let audits = connection
        .list_audit_by_target("memory", &memory_id, None)
        .expect("target audit query succeeds");
    let transition = audits
        .iter()
        .find(|entry| entry.action == audit_actions::MEMORY_LEVEL_TRANSITION)
        .expect("canonical transition audit exists");
    let details: Value = serde_json::from_str(
        transition
            .details
            .as_deref()
            .expect("transition details exist"),
    )
    .expect("transition details parse");
    assert_eq!(
        details["previousLevel"],
        Value::String("episodic".to_owned())
    );
    assert_eq!(details["newLevel"], Value::String("semantic".to_owned()));
    assert_eq!(
        details["event"],
        Value::String("repeated_observation".to_owned())
    );
    assert_eq!(
        details["sourceAction"],
        Value::String("curation_candidate.apply".to_owned())
    );
}
