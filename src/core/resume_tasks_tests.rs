#![allow(clippy::unwrap_used)]

use super::*;
use crate::core::resume::{ResumeOptions, build_resume_report};
use crate::core::task_frame::{
    TASK_FRAME_STORE_SCHEMA_V1, TaskFrameCreateOptions, TaskFrameStoreDocument,
    TaskSubgoalAddOptions, add_task_subgoal, create_task_frame,
};

const TIME: &str = "2026-09-01T12:00:00Z";
const ROOT: &str = "/recorded/workspace";

fn frame(index: usize, status: TaskFrameStatus) -> TaskFrameRecord {
    TaskFrameRecord {
        schema: TASK_FRAME_SCHEMA_V1.to_owned(),
        id: format!("tf_{index:026x}"),
        workspace_root: ROOT.to_owned(),
        root_goal: format!("Resume task {index}"),
        status,
        actor: "Recorder".to_owned(),
        source: "test".to_owned(),
        current_focus: Some("Investigate failing tests".to_owned()),
        blockers: vec!["Waiting for upstream evidence".to_owned()],
        subgoals: Vec::new(),
        evidence_links: Vec::new(),
        suggested_commands: vec!["NEVER_EXECUTE_OR_RENDER_STORED_COMMAND".to_owned()],
        redaction_status: "none".to_owned(),
        non_executing_contract: "UNTRUSTED_STORED_CONTRACT".to_owned(),
        created_at: TIME.to_owned(),
        updated_at: TIME.to_owned(),
        closed_at: None,
        close_reason: None,
    }
}

fn subgoal(index: usize) -> TaskSubgoal {
    TaskSubgoal {
        id: format!("tg_{index:026x}"),
        parent_id: None,
        title: format!("Verify subgoal {index}"),
        status: TaskFrameStatus::Open,
        blockers: Vec::new(),
        created_at: TIME.to_owned(),
        updated_at: TIME.to_owned(),
        closed_at: None,
    }
}

fn projected(frames: Vec<TaskFrameRecord>) -> ResumeTaskFrames {
    project(frames, &BTreeSet::from([ROOT.to_owned()]))
}

fn write_frames(workspace: &Path, mut frames: Vec<TaskFrameRecord>) -> Vec<u8> {
    for frame in &mut frames {
        frame.workspace_root = workspace.display().to_string();
    }
    std::fs::create_dir_all(workspace.join(".ee")).unwrap();
    let bytes = serde_json::to_vec(&TaskFrameStoreDocument {
        schema: TASK_FRAME_STORE_SCHEMA_V1.to_owned(),
        frames,
    })
    .unwrap();
    std::fs::write(workspace.join(".ee/task_frames.json"), &bytes).unwrap();
    bytes
}

#[test]
fn empty_task_resume_never_initializes_a_workspace() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().canonicalize().unwrap();
    let report = load(&workspace);
    assert_eq!(report, ResumeTaskFrames::default());
    assert!(!workspace.join(".ee").exists());
}

#[test]
fn unfinished_goals_survive_public_resume_without_a_database() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().canonicalize().unwrap();
    let created = create_task_frame(&TaskFrameCreateOptions {
        workspace_path: workspace.clone(),
        goal: "Finish the interrupted release investigation".to_owned(),
        actor: "ResumeAgent".to_owned(),
        status: TaskFrameStatus::Blocked,
        current_focus: Some("Check the release artifacts".to_owned()),
        blockers: vec!["Await the upstream fix".to_owned()],
        evidence_links: Vec::new(),
        created_at: Some(TIME.to_owned()),
        dry_run: false,
    })
    .unwrap();
    let id = created.frame.unwrap().id;
    let added = add_task_subgoal(&TaskSubgoalAddOptions {
        workspace_path: workspace.clone(),
        frame_id: id.clone(),
        parent_id: None,
        title: "Re-run the failed release test".to_owned(),
        status: TaskFrameStatus::Open,
        blockers: Vec::new(),
        created_at: Some(TIME.to_owned()),
        dry_run: false,
    })
    .unwrap();
    let subgoal_id = added.selected_subgoal.unwrap().id;
    let store = workspace.join(".ee/task_frames.json");
    let before = std::fs::read(&store).unwrap();
    let database = workspace.join(".ee/ee.db");
    for _ in 0..2 {
        let report = build_resume_report(&ResumeOptions {
            workspace_path: &workspace,
            database_path: &database,
            sessions: 3,
        })
        .unwrap();
        assert_eq!(report.task_frames.status, ResumeTaskStatus::Available);
        assert_eq!(report.task_frames.active_total, Some(1));
        assert_eq!(report.task_frames.frames[0].id, id);
        assert_eq!(
            report.task_frames.frames[0].status,
            TaskFrameStatus::Blocked
        );
        assert_eq!(report.task_frames.frames[0].subgoals[0].id, subgoal_id);
        assert_eq!(report.episodic_total, 0, "do not synthesize memories");
        assert!(
            report.nearby_stores.is_none(),
            "the addressed store has resumable work"
        );
        assert!(report.next_commands[0].contains(&format!("ee task-frame show {id} ")));
        assert!(report.next_commands[0].contains("--workspace "));
        assert_eq!(std::fs::read(&store).unwrap(), before);
        assert!(!database.exists(), "resume must not initialize a database");
    }
}

#[test]
fn terminal_and_draft_frames_are_not_adopted_and_multiple_goals_stay_explicit() {
    let report = projected(vec![
        frame(1, TaskFrameStatus::Completed),
        frame(2, TaskFrameStatus::Abandoned),
        frame(3, TaskFrameStatus::Draft),
        frame(4, TaskFrameStatus::Open),
        frame(5, TaskFrameStatus::Blocked),
        frame(6, TaskFrameStatus::Active),
    ]);
    assert_eq!(report.active_total, Some(3));
    assert_eq!(report.excluded_total, Some(0));
    assert!(report.selection_required);
    assert_eq!(report.frames[0].status, TaskFrameStatus::Active);
    assert_eq!(report.frames[1].status, TaskFrameStatus::Blocked);
    assert_eq!(report.frames[2].status, TaskFrameStatus::Open);
    let wire = serde_json::to_string(&report).unwrap();
    assert!(!wire.contains("UNTRUSTED_STORED_CONTRACT"));
    assert!(!wire.contains("NEVER_EXECUTE_OR_RENDER_STORED_COMMAND"));
    assert_eq!(report.non_executing_contract, NON_EXECUTING_CONTRACT);
}

#[test]
fn ordering_uses_instants_and_stable_identity_not_file_order() {
    let mut newer = frame(1, TaskFrameStatus::Open);
    newer.updated_at = "2026-09-01T10:00:00-04:00".to_owned();
    let mut older = frame(2, TaskFrameStatus::Open);
    older.updated_at = "2026-09-01T13:00:00Z".to_owned();
    let tied = frame(3, TaskFrameStatus::Open);
    let earlier = frame(4, TaskFrameStatus::Open);
    let mut rows = vec![older, earlier, tied, newer.clone()];
    let first = projected(rows.clone());
    rows.reverse();
    assert_eq!(projected(rows), first);
    assert_eq!(first.frames[0].id, newer.id);
    assert!(first.frames[2].id < first.frames[3].id);
}

#[test]
fn bounded_projection_keeps_exact_counts_and_redacts_before_truncation() {
    let secret = format!("sk_live_{}", "1234567890abcdef1234567890abcdef");
    let mut rows = Vec::new();
    for index in 0..FRAME_CAP + 3 {
        let mut row = frame(index, TaskFrameStatus::Open);
        // The complete token crosses the visible text boundary.
        row.root_goal = format!("{} {secret}", "x".repeat(TEXT_CHAR_CAP - 8));
        row.current_focus = Some(secret.clone());
        row.blockers = vec![secret.clone(); BLOCKER_CAP + 2];
        row.subgoals = (0..SUBGOAL_CAP + 2)
            .map(|n| {
                let mut child = subgoal(n);
                // Test truncation on ordinary text separately from redaction:
                // an unsafe field can be replaced with a short opaque marker.
                child.title = "Review unit results. ".repeat(20);
                child.blockers = vec![secret.clone(); BLOCKER_CAP + 1];
                child
            })
            .collect();
        rows.push(row);
    }
    let report = projected(rows);
    assert_eq!(report.active_total, Some(FRAME_CAP + 3));
    assert_eq!(report.frames.len(), FRAME_CAP);
    assert!(report.truncated);
    for row in &report.frames {
        assert!(row.root_goal.chars().count() <= TEXT_CHAR_CAP);
        assert!(row.redaction.applied);
        assert_eq!(row.blockers.total, BLOCKER_CAP + 2);
        assert!(row.blockers.truncated);
        assert_eq!(row.blockers.items.len(), BLOCKER_CAP);
        assert_eq!(row.active_subgoals_total, SUBGOAL_CAP + 2);
        assert!(row.subgoals_truncated);
        assert_eq!(row.subgoals.len(), SUBGOAL_CAP);
        assert!(row.subgoals.iter().all(|s| s.text_truncated
            && s.redaction.applied
            && s.title.chars().count() == TEXT_CHAR_CAP));
    }
    let wire = serde_json::to_string(&report).unwrap();
    assert!(!wire.contains(&secret));
    assert!(
        !wire.contains("sk_liv"),
        "do not emit a truncated secret prefix"
    );
}

#[test]
fn malformed_or_foreign_tasks_are_withheld_without_hiding_valid_goals() {
    let good = frame(0, TaskFrameStatus::Open);
    let mut foreign = frame(1, TaskFrameStatus::Open);
    foreign.workspace_root = "/PRIVATE_FOREIGN_WORKSPACE".to_owned();
    let mut command_id = frame(2, TaskFrameStatus::Open);
    command_id.id = "tf_;PRIVATE_COMMAND".to_owned();
    let mut future = frame(3, TaskFrameStatus::Open);
    future.schema = "future".to_owned();
    let mut invalid_time = frame(4, TaskFrameStatus::Open);
    invalid_time.updated_at = "PRIVATE_TIME".to_owned();
    let mut closed = frame(5, TaskFrameStatus::Open);
    closed.closed_at = Some(TIME.to_owned());
    let report = projected(vec![
        good.clone(),
        foreign,
        command_id,
        future,
        invalid_time,
        closed,
    ]);
    assert_eq!(report.status, ResumeTaskStatus::Partial);
    assert_eq!(report.active_total, Some(1));
    assert_eq!(report.excluded_total, Some(5));
    assert_eq!(report.frames[0].id, good.id);
    assert!(!serde_json::to_string(&report).unwrap().contains("PRIVATE_"));
}

#[test]
fn duplicate_ids_or_cyclic_and_orphan_subgoals_cannot_choose_a_task() {
    let duplicate = frame(1, TaskFrameStatus::Active);
    let mut cycle = frame(2, TaskFrameStatus::Open);
    let mut a = subgoal(1);
    let mut b = subgoal(2);
    a.parent_id = Some(b.id.clone());
    b.parent_id = Some(a.id.clone());
    cycle.subgoals = vec![a, b];
    let mut orphan = frame(3, TaskFrameStatus::Open);
    let mut child = subgoal(3);
    child.parent_id = Some(subgoal(9).id);
    orphan.subgoals.push(child);
    let mut duplicate_child = frame(4, TaskFrameStatus::Open);
    duplicate_child.subgoals = vec![subgoal(1), subgoal(1)];
    let report = projected(vec![
        duplicate.clone(),
        duplicate,
        cycle,
        orphan,
        duplicate_child,
    ]);
    assert_eq!(report.status, ResumeTaskStatus::Partial);
    assert_eq!(report.active_total, Some(0));
    assert_eq!(report.excluded_total, Some(5));
    assert!(report.frames.is_empty());
}

#[test]
fn deep_goal_stacks_are_iterative_and_preserve_parent_identity() {
    let mut row = frame(1, TaskFrameStatus::Open);
    row.subgoals = (0..2000)
        .map(|n| {
            let mut child = subgoal(n);
            child.parent_id = (n > 0).then(|| subgoal(n - 1).id);
            child
        })
        .collect();
    let report = projected(vec![row]);
    assert_eq!(report.active_total, Some(1));
    assert_eq!(report.frames[0].active_subgoals_total, 2000);
    assert_eq!(
        report.frames[0].subgoals[1].parent_id.as_deref(),
        Some(subgoal(0).id.as_str())
    );
}

#[test]
fn unreadable_or_future_task_stores_report_unknown_not_zero_without_echoing_input() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().canonicalize().unwrap();
    std::fs::create_dir_all(workspace.join(".ee")).unwrap();
    for body in [
        br#"{"schema":"PRIVATE_PARSE_INPUT""#.as_slice(),
        br#"{"schema":"future","frames":[]}"#.as_slice(),
    ] {
        std::fs::write(workspace.join(".ee/task_frames.json"), body).unwrap();
        let report = load(&workspace);
        assert_eq!(report.status, ResumeTaskStatus::Unavailable);
        assert_eq!(report.active_total, None);
        assert_eq!(report.excluded_total, None);
        assert_eq!(report.degraded_code, Some("resume_task_frames_unavailable"));
        assert!(
            !serde_json::to_string(&report)
                .unwrap()
                .contains("PRIVATE_PARSE_INPUT")
        );
        assert_eq!(
            std::fs::read(workspace.join(".ee/task_frames.json")).unwrap(),
            body
        );
    }
}

#[cfg(unix)]
#[test]
fn symlinked_task_stores_are_unavailable_and_not_rewritten() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().canonicalize().unwrap();
    std::fs::create_dir_all(workspace.join(".ee")).unwrap();
    let outside = workspace.join("outside.json");
    std::fs::write(&outside, "PRIVATE_OUTSIDE_INPUT").unwrap();
    std::os::unix::fs::symlink(&outside, workspace.join(".ee/task_frames.json")).unwrap();
    let report = load(&workspace);
    assert_eq!(report.status, ResumeTaskStatus::Unavailable);
    assert_eq!(
        std::fs::read_to_string(&outside).unwrap(),
        "PRIVATE_OUTSIDE_INPUT"
    );
}

#[test]
fn public_resume_preserves_memory_results_when_task_store_is_corrupt() {
    use crate::db::{CreateMemoryInput, CreateWorkspaceInput, DbConnection};
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().canonicalize().unwrap();
    write_frames(&workspace, vec![frame(1, TaskFrameStatus::Open)]);
    let database = workspace.join(".ee/ee.db");
    let db = DbConnection::open_file(&database).unwrap();
    db.migrate().unwrap();
    let workspace_id = crate::core::workspace::stable_workspace_id(&workspace);
    db.insert_workspace(
        &workspace_id,
        &CreateWorkspaceInput {
            path: workspace.display().to_string(),
            name: Some("task resume".to_owned()),
        },
    )
    .unwrap();
    let id = crate::models::MemoryId::from_uuid(uuid::Uuid::from_u128(0x52534d54)).to_string();
    db.insert_memory(
        &id,
        &CreateMemoryInput {
            workspace_id,
            level: "episodic".to_owned(),
            kind: "note".to_owned(),
            content: "The previous session finished the failing-test investigation.".to_owned(),
            workflow_id: None,
            confidence: 0.8,
            utility: 0.5,
            importance: 0.5,
            provenance_uri: None,
            trust_class: "agent_assertion".to_owned(),
            trust_subclass: None,
            tags: vec!["session-task-resume".to_owned()],
            valid_from: None,
            valid_to: None,
        },
    )
    .unwrap();
    db.close().unwrap();
    let options = ResumeOptions {
        workspace_path: &workspace,
        database_path: &database,
        sessions: 3,
    };
    let before = build_resume_report(&options).unwrap();
    assert_eq!(before.task_frames.active_total, Some(1));
    assert_eq!(before.sessions[0].items[0].memory_id, id);
    let fingerprint = || {
        ["", "-wal", "-shm"].map(|suffix| {
            match std::fs::read(format!("{}{suffix}", database.display())) {
                Ok(bytes) => Some(blake3::hash(&bytes)),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) => panic!("cannot fingerprint resume database: {error}"),
            }
        })
    };
    let original = fingerprint();
    std::fs::write(
        workspace.join(".ee/task_frames.json"),
        "PRIVATE_BROKEN_TASK_STORE",
    )
    .unwrap();
    let after = build_resume_report(&options).unwrap();
    assert_eq!(after.sessions, before.sessions);
    assert_eq!(after.task_frames.status, ResumeTaskStatus::Unavailable);
    assert_eq!(fingerprint(), original);
    assert!(after.next_commands[0].contains("inspect incomplete task recovery"));
    assert!(
        !serde_json::to_string(&after)
            .unwrap()
            .contains("PRIVATE_BROKEN_TASK_STORE")
    );
}

#[test]
fn a_non_directory_workspace_marker_is_unavailable_not_empty() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().canonicalize().unwrap();
    std::fs::write(workspace.join(".ee"), "PRIVATE_BROKEN_NAMESPACE").unwrap();
    let report = load(&workspace);
    assert_eq!(report.status, ResumeTaskStatus::Unavailable);
    assert_eq!(report.active_total, None);
    assert!(
        !serde_json::to_string(&report)
            .unwrap()
            .contains("PRIVATE_BROKEN_NAMESPACE")
    );
    assert_eq!(
        std::fs::read_to_string(workspace.join(".ee")).unwrap(),
        "PRIVATE_BROKEN_NAMESPACE"
    );
}
