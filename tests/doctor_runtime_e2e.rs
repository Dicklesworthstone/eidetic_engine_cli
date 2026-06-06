//! Integration tests for `ee::core::doctor_runtime`.
//!
//! Exercises the chokepoint from outside `src/`, validating the public API
//! composes correctly as a downstream consumer (the Phase-4 CLI wiring at
//! `bd-3boan`) would invoke it. Complements the inline unit tests inside
//! `src/core/doctor_runtime.rs::tests`.
//!
//! Source: doctor_workspace pass 1 (world-class-doctor-mode skill).
//! Polish-bar coverage these tests assert:
//!
//! - 🩺 Detect-then-fix: the runtime exposes only mutate(); no detector
//!   functions reach in here.
//! - 🚪 Single chokepoint: every disk write in this test routes through
//!   `mutate()` — never `std::fs::write` directly to a tracked path.
//! - 💾 Verbatim backup: we hash the file before+after and compare against
//!   what's written to the backups/ tree.
//! - ↩ Inverse pair: full corrupt → fix → undo → byte-identical cycle.
//! - 🔁 Idempotent twice: re-run a fixer; second call returns NoOpIdempotent.
//! - 🔒 Lock-or-refuse: second start() raises ConcurrencyLost.
//! - 🛡 Refuse-on-unsafe: writes outside the declared blast radius refuse.
//! - 📜 Self-describing: CapabilitiesReport serializes with stable schema.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::fs;
use std::path::PathBuf;

use ee::core::doctor_runtime::{
    ACTION_LINE_SCHEMA_V1, CAPABILITIES_SCHEMA_V1, CapabilitiesReport, DoctorRuntimeError, Op,
    RUN_STATE_SCHEMA_V1, RunContext, RunStatus, default_blast_radius_roots, mutate, replay_undo,
};
use tempfile::TempDir;

fn fresh_workspace() -> TempDir {
    TempDir::new().expect("tempdir")
}

/// Helper: start a run with a blast radius that includes the workspace root
/// itself, so we can mutate files at `<ws>/data.txt` for testing convenience.
/// (Production code uses the narrower default_blast_radius_roots.)
fn start_test_run(ws_path: &std::path::Path) -> RunContext {
    let mut roots = default_blast_radius_roots(ws_path);
    roots.push(ws_path.to_path_buf());
    RunContext::start(ws_path, "e2e_sha", roots, false).expect("start run")
}

#[test]
fn full_corrupt_fix_diagnose_undo_roundtrip_is_byte_identical() {
    // The canonical Polish-Bar round-trip: take a known-good file, "fix"
    // it through mutate() (with a backup), then replay_undo and assert
    // the file matches the original byte-for-byte.
    let ws = fresh_workspace();
    let target = ws.path().join("config.toml");
    let original = b"[storage]\ndatabase_path = \"~/.local/share/ee/ee.db\"\n";
    fs::write(&target, original).unwrap();

    let original_hash_input = blake3::hash(original).to_hex().to_string();

    let run_dir;
    {
        let mut ctx = start_test_run(ws.path());
        run_dir = ctx.run_dir().to_path_buf();
        let modified = b"[storage]\ndatabase_path = \"/tmp/ee.db\"\n";
        let line = mutate(
            &mut ctx,
            &target,
            Op::WriteFile {
                bytes: modified.to_vec(),
            },
        )
        .expect("mutate WriteFile");
        assert_eq!(line.schema, ACTION_LINE_SCHEMA_V1);
        assert_eq!(line.kind, "write_file");
        assert!(line.before_hash.is_some());
        assert!(line.after_hash.is_some());

        let backup_rel = line.backup_rel_path.expect("backup recorded");
        let backup_full = ctx.run_dir().join("backups").join(&backup_rel);
        // The backup is byte-identical to the original.
        assert_eq!(fs::read(&backup_full).unwrap(), original);

        ctx.finish(RunStatus::CompletedOk).expect("finish");
    }

    // Verify the live file is the modified version.
    assert_ne!(fs::read(&target).unwrap(), original);

    // Undo.
    let summary = replay_undo(&run_dir).expect("replay_undo");
    assert!(matches!(summary.status, RunStatus::Undone));
    assert_eq!(summary.actions_undone, 1);

    // Byte-identical restoration.
    let restored = fs::read(&target).unwrap();
    assert_eq!(restored, original);
    let restored_hash = blake3::hash(&restored).to_hex().to_string();
    assert_eq!(restored_hash, original_hash_input);
}

#[test]
fn mutate_twice_with_same_bytes_returns_no_op_idempotent() {
    let ws = fresh_workspace();
    let target = ws.path().join("data.toml");
    let mut ctx = start_test_run(ws.path());

    let first = mutate(
        &mut ctx,
        &target,
        Op::WriteFile {
            bytes: b"same".to_vec(),
        },
    );
    assert!(first.is_ok());

    let second = mutate(
        &mut ctx,
        &target,
        Op::WriteFile {
            bytes: b"same".to_vec(),
        },
    );
    assert!(matches!(second, Err(DoctorRuntimeError::NoOpIdempotent)));
}

#[test]
fn second_run_against_held_lock_refuses_concurrency_lost() {
    let ws = fresh_workspace();
    let _first = start_test_run(ws.path());

    let second = RunContext::start(
        ws.path(),
        "different_sha",
        vec![ws.path().to_path_buf()],
        false,
    );
    assert!(matches!(
        second,
        Err(DoctorRuntimeError::ConcurrencyLost { .. })
    ));
}

#[test]
fn write_outside_blast_radius_refuses_with_blast_radius_exceeded() {
    let ws = fresh_workspace();
    // Restricted radius: only .ee under workspace.
    let restricted = vec![ws.path().join(".ee")];
    let mut ctx = RunContext::start(ws.path(), "sha", restricted, false).expect("start");

    let outside = ws.path().parent().unwrap().join("evil.txt");
    let result = mutate(
        &mut ctx,
        &outside,
        Op::WriteFile {
            bytes: b"x".to_vec(),
        },
    );
    assert!(matches!(
        result,
        Err(DoctorRuntimeError::BlastRadiusExceeded { .. })
    ));
}

#[test]
fn write_with_parent_components_under_missing_tail_refuses_blast_radius_escape() {
    let ws = fresh_workspace();
    let restricted = vec![ws.path().join(".ee")];
    let mut ctx = RunContext::start(ws.path(), "sha", restricted, true).expect("start");

    let target = ws
        .path()
        .join(".ee")
        .join("missing")
        .join("..")
        .join("..")
        .join("blast_radius_escape.txt");
    let escaped = ws.path().join("blast_radius_escape.txt");

    let result = mutate(
        &mut ctx,
        &target,
        Op::WriteFile {
            bytes: b"x".to_vec(),
        },
    );

    assert!(matches!(
        result,
        Err(DoctorRuntimeError::BlastRadiusExceeded { .. })
    ));
    assert!(!escaped.exists());
}

#[test]
fn capabilities_report_serializes_with_stable_schema() {
    let ws = fresh_workspace();
    let report = CapabilitiesReport::build("0.1.0", ws.path());
    let json = serde_json::to_value(&report).expect("serialize");

    // Schema pin.
    assert_eq!(
        json.get("schema").and_then(|v| v.as_str()),
        Some(CAPABILITIES_SCHEMA_V1)
    );
    assert_eq!(
        json.get("action_line_schema").and_then(|v| v.as_str()),
        Some(ACTION_LINE_SCHEMA_V1)
    );
    assert_eq!(
        json.get("run_artifact_schema").and_then(|v| v.as_str()),
        Some(RUN_STATE_SCHEMA_V1)
    );

    // Exit-code dictionary is complete (0..=8 documented).
    let codes: Vec<i64> = json
        .get("exit_codes")
        .and_then(|v| v.as_array())
        .expect("exit_codes array")
        .iter()
        .filter_map(|c| c.get("code").and_then(|v| v.as_i64()))
        .collect();
    assert!(codes.contains(&0));
    assert!(codes.contains(&5));
    assert!(codes.contains(&8));

    // Op kinds enumerated.
    let kinds: Vec<String> = json
        .get("op_kinds")
        .and_then(|v| v.as_array())
        .expect("op_kinds array")
        .iter()
        .filter_map(|k| k.as_str().map(String::from))
        .collect();
    assert!(kinds.contains(&"write_file".to_string()));
    assert!(kinds.contains(&"quarantine_by_rename".to_string()));
    assert!(kinds.contains(&"manual".to_string()));
}

#[test]
fn quarantine_by_rename_moves_file_under_run_quarantine_root() {
    let ws = fresh_workspace();
    let target = ws.path().join("orphan.wal");
    fs::write(&target, b"stale wal contents").unwrap();

    let mut ctx = start_test_run(ws.path());
    let line = mutate(
        &mut ctx,
        &target,
        Op::QuarantineByRename {
            dest_under_quarantine: PathBuf::from("orphan.wal"),
        },
    )
    .expect("mutate QuarantineByRename");

    // Source path is gone.
    assert!(!target.exists());
    // Quarantine destination is present and byte-identical to original.
    let q = ctx.run_dir().join("quarantine").join("orphan.wal");
    assert!(q.exists());
    assert_eq!(fs::read(&q).unwrap(), b"stale wal contents");
    assert_eq!(line.kind, "quarantine_by_rename");
}

#[test]
fn quarantine_refuses_traversal_via_parent_components() {
    // Path-traversal defense added in round-1 fresh-eyes (P0 security fix).
    let ws = fresh_workspace();
    let target = ws.path().join("file.txt");
    fs::write(&target, b"x").unwrap();

    let mut ctx = start_test_run(ws.path());
    let result = mutate(
        &mut ctx,
        &target,
        Op::QuarantineByRename {
            dest_under_quarantine: PathBuf::from("../../../escape"),
        },
    );
    assert!(matches!(
        result,
        Err(DoctorRuntimeError::BlastRadiusExceeded { .. })
    ));
    // Victim file is unaffected.
    assert!(target.exists());
}

#[test]
fn dry_run_records_plan_without_touching_disk() {
    let ws = fresh_workspace();
    let target = ws.path().join("plan.txt");
    fs::write(&target, b"unchanged").unwrap();

    let mut roots = default_blast_radius_roots(ws.path());
    roots.push(ws.path().to_path_buf());
    let mut ctx = RunContext::start(ws.path(), "sha", roots, /*dry_run*/ true).expect("start");

    let line = mutate(
        &mut ctx,
        &target,
        Op::WriteFile {
            bytes: b"would_change".to_vec(),
        },
    )
    .expect("mutate dry-run");

    // Disk is unchanged in dry-run.
    assert_eq!(fs::read(&target).unwrap(), b"unchanged");
    // But the plan IS recorded in actions.jsonl.
    let raw = fs::read_to_string(ctx.run_dir().join("actions.jsonl")).unwrap();
    assert!(raw.contains("write_file"));
    assert_eq!(line.kind, "write_file");
}

#[test]
fn manual_op_records_steps_but_writes_nothing_to_disk() {
    let ws = fresh_workspace();
    let mut ctx = start_test_run(ws.path());

    // Manual op for an external-state finding (e.g., "cass not found").
    let line = mutate(
        &mut ctx,
        &ws.path().join("not_a_real_file"),
        Op::Manual {
            steps: vec![
                "cargo install --path /dp/coding_agent_session_search".to_string(),
                "verify cass --version".to_string(),
            ],
        },
    )
    .expect("mutate Manual");
    assert_eq!(line.kind, "manual");
    let notes = line.notes.expect("notes recorded");
    assert!(notes.contains("cargo install"));
    assert!(notes.contains("verify cass --version"));
}

#[test]
fn full_undo_idempotence_two_replays_safe() {
    let ws = fresh_workspace();
    let target = ws.path().join("idem.txt");
    fs::write(&target, b"a").unwrap();

    let run_dir;
    {
        let mut ctx = start_test_run(ws.path());
        run_dir = ctx.run_dir().to_path_buf();
        mutate(
            &mut ctx,
            &target,
            Op::WriteFile {
                bytes: b"b".to_vec(),
            },
        )
        .unwrap();
        ctx.finish(RunStatus::CompletedOk).unwrap();
    }

    // First replay: 1 action undone.
    let first = replay_undo(&run_dir).expect("replay 1");
    assert_eq!(first.actions_undone, 1);
    assert_eq!(fs::read(&target).unwrap(), b"a");

    // Second replay: 0 actions undone, 1 skipped (already done).
    let second = replay_undo(&run_dir).expect("replay 2");
    assert_eq!(second.actions_undone, 0);
    assert_eq!(second.actions_skipped, 1);
    // File still byte-identical to the original.
    assert_eq!(fs::read(&target).unwrap(), b"a");
}
