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
use std::path::{Path, PathBuf};

use ee::core::doctor_runtime::{
    ACTION_LINE_SCHEMA_V1, CAPABILITIES_SCHEMA_V1, CapabilitiesReport, DoctorRuntimeError, Op,
    RUN_STATE_SCHEMA_V2, RunContext, RunStatus, default_blast_radius_roots, mutate,
    replay_undo_with_authorized_roots,
};
use fs4::fs_std::FileExt as Fs4FileExt;
use tempfile::TempDir;

fn fresh_workspace() -> TempDir {
    TempDir::new().expect("tempdir")
}

#[cfg(unix)]
fn directory_snapshot(path: &Path) -> Vec<(std::ffi::OsString, Vec<u8>)> {
    let mut entries = fs::read_dir(path)
        .expect("read external directory")
        .map(|entry| {
            let entry = entry.expect("read external entry");
            let metadata = fs::symlink_metadata(entry.path()).expect("inspect external entry");
            let bytes = if metadata.is_file() {
                fs::read(entry.path()).expect("read external sentinel")
            } else {
                Vec::new()
            };
            (entry.file_name(), bytes)
        })
        .collect::<Vec<_>>();
    entries.sort_by(|left, right| left.0.cmp(&right.0));
    entries
}

/// Helper: start a run with a blast radius that includes the workspace root
/// itself, so we can mutate files at `<ws>/data.txt` for testing convenience.
/// (Production code uses the narrower default_blast_radius_roots.)
fn start_test_run(ws_path: &std::path::Path) -> RunContext {
    let mut roots = default_blast_radius_roots(ws_path);
    roots.push(ws_path.to_path_buf());
    RunContext::start(ws_path, "e2e_sha", roots, false).expect("start run")
}

fn replay_test_undo(
    run_dir: &Path,
) -> Result<ee::core::doctor_runtime::UndoSummary, DoctorRuntimeError> {
    let run_id = run_dir
        .file_name()
        .and_then(|name| name.to_str())
        .expect("test run id");
    let workspace = run_dir
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .expect("test workspace");
    let mut roots = default_blast_radius_roots(workspace);
    roots.push(workspace.to_path_buf());
    replay_undo_with_authorized_roots(workspace, run_id, &roots)
}

fn assert_persistent_doctor_lock_released(workspace: &Path) {
    let lock_path = workspace.join(".ee").join(".doctor.lock");
    assert!(
        lock_path.is_file(),
        "persistent doctor lock file is missing"
    );
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&lock_path)
        .expect("open persistent doctor lock");
    assert!(
        Fs4FileExt::try_lock_exclusive(&lock)
            .expect("probe released persistent doctor advisory lock"),
        "persistent doctor advisory lock should be released"
    );
    Fs4FileExt::unlock(&lock).expect("unlock test doctor lock");
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
    let summary = replay_test_undo(&run_dir).expect("replay_undo");
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

#[cfg(unix)]
#[test]
fn symlinked_doctor_root_refuses_before_lock_or_external_mutation() {
    use std::os::unix::fs::symlink;

    let root = fresh_workspace();
    let workspace = root.path().join("workspace");
    let external = root.path().join("external-doctor");
    fs::create_dir(&workspace).expect("create workspace");
    fs::create_dir(&external).expect("create external doctor target");
    fs::write(
        external.join("sentinel"),
        b"outside must remain byte-identical",
    )
    .expect("write sentinel");
    let before = directory_snapshot(&external);
    symlink(&external, workspace.join(".doctor")).expect("redirect .doctor");

    let result = RunContext::start(
        &workspace,
        "symlinked-doctor",
        default_blast_radius_roots(&workspace),
        false,
    );

    assert!(matches!(
        result,
        Err(DoctorRuntimeError::SymlinkedRunRoot { ref path })
            if path.ends_with(".doctor")
    ));
    assert!(
        !workspace.join(".ee").exists(),
        "refusal must happen before creating the lock directory"
    );
    assert_eq!(
        directory_snapshot(&external),
        before,
        "external directory listing and sentinel bytes must remain identical"
    );
}

#[cfg(unix)]
#[test]
fn symlinked_ee_root_refuses_without_touching_external_lock_target() {
    use std::os::unix::fs::symlink;

    let root = fresh_workspace();
    let workspace = root.path().join("workspace");
    let external = root.path().join("external-ee");
    fs::create_dir(&workspace).expect("create workspace");
    fs::create_dir(&external).expect("create external ee target");
    fs::write(
        external.join("sentinel"),
        b"external lock target is immutable",
    )
    .expect("write sentinel");
    let before = directory_snapshot(&external);
    symlink(&external, workspace.join(".ee")).expect("redirect .ee");

    let result = RunContext::start(
        &workspace,
        "symlinked-ee",
        default_blast_radius_roots(&workspace),
        false,
    );

    assert!(matches!(
        result,
        Err(DoctorRuntimeError::SymlinkedRunRoot { ref path })
            if path.ends_with(".ee")
    ));
    assert!(
        !workspace.join(".doctor").exists(),
        "refusal must not allocate doctor run artifacts"
    );
    assert_eq!(
        directory_snapshot(&external),
        before,
        "external directory listing and sentinel bytes must remain identical"
    );
}

#[cfg(any(target_os = "linux", target_os = "android", target_vendor = "apple"))]
#[test]
fn finish_refuses_after_parent_symlink_substitution_without_external_mutation() {
    use std::os::unix::fs::symlink;

    let root = fresh_workspace();
    let workspace = root.path().join("workspace");
    let external = root.path().join("external-latest");
    fs::create_dir(&workspace).expect("create workspace");
    fs::create_dir(&external).expect("create external latest target");
    fs::write(
        external.join("sentinel"),
        b"latest must not escape workspace",
    )
    .expect("write sentinel");

    let ctx = RunContext::start(
        &workspace,
        "latest-parent-substitution",
        default_blast_radius_roots(&workspace),
        false,
    )
    .expect("start run");
    let before = directory_snapshot(&external);
    let detached_doctor = workspace.join(".doctor-detached");
    fs::rename(workspace.join(".doctor"), &detached_doctor).expect("move owned doctor root");
    symlink(&external, workspace.join(".doctor")).expect("substitute external .doctor");

    let result = ctx.finish(RunStatus::CompletedOk);

    assert!(matches!(
        result,
        Err(DoctorRuntimeError::LifecycleRootChanged { ref path })
            if path.ends_with(".doctor")
    ));
    assert_eq!(
        directory_snapshot(&external),
        before,
        "finish must not create, replace, or remove anything through substituted .doctor"
    );
    assert!(
        !detached_doctor.join("latest").exists(),
        "fail-closed finish must not publish latest or return a misleading lexical run path"
    );
}

#[cfg(any(target_os = "linux", target_os = "android", target_vendor = "apple"))]
#[test]
fn mutate_refuses_after_parent_symlink_substitution_before_backup_or_target_write() {
    use std::os::unix::fs::symlink;

    let root = fresh_workspace();
    let workspace = root.path().join("workspace");
    let external = root.path().join("external-backup");
    fs::create_dir(&workspace).expect("create workspace");
    fs::create_dir(&external).expect("create external backup target");
    fs::write(
        external.join("sentinel"),
        b"backup must not escape workspace",
    )
    .expect("write sentinel");
    let target = workspace.join("config.toml");
    fs::write(&target, b"original").expect("write target");

    let mut roots = default_blast_radius_roots(&workspace);
    roots.push(workspace.clone());
    let mut ctx = RunContext::start(&workspace, "backup-parent-substitution", roots, false)
        .expect("start run");
    let run_name = ctx
        .run_dir()
        .file_name()
        .expect("run directory name")
        .to_os_string();
    let before = directory_snapshot(&external);
    let detached_doctor = workspace.join(".doctor-detached");
    fs::rename(workspace.join(".doctor"), &detached_doctor).expect("move owned doctor root");
    symlink(&external, workspace.join(".doctor")).expect("substitute external .doctor");

    let result = mutate(
        &mut ctx,
        &target,
        Op::WriteFile {
            bytes: b"changed".to_vec(),
        },
    );

    assert!(matches!(
        result,
        Err(DoctorRuntimeError::LifecycleRootChanged { ref path })
            if path.ends_with(".doctor")
    ));
    assert_eq!(
        fs::read(&target).expect("read unchanged target"),
        b"original"
    );
    assert_eq!(
        directory_snapshot(&external),
        before,
        "mutate must not create, replace, or remove anything through substituted .doctor"
    );
    let backups = detached_doctor.join("runs").join(run_name).join("backups");
    assert_eq!(
        fs::read_dir(backups)
            .expect("read anchored backups")
            .count(),
        0,
        "refusal must happen before staging any backup"
    );
}

#[cfg(any(target_os = "linux", target_os = "android", target_vendor = "apple"))]
#[test]
fn backup_refuses_nested_symlink_without_touching_external_destination() {
    use std::os::unix::fs::symlink;

    let root = fresh_workspace();
    let workspace = root.path().join("workspace");
    let external = root.path().join("external-backup-slot");
    fs::create_dir(&workspace).expect("create workspace");
    fs::create_dir(&external).expect("create external backup target");
    fs::write(external.join("sentinel"), b"backup slot is immutable").expect("write sentinel");
    let target = workspace.join("config.toml");
    fs::write(&target, b"original").expect("write target");

    let mut roots = default_blast_radius_roots(&workspace);
    roots.push(workspace.clone());
    let mut ctx =
        RunContext::start(&workspace, "nested-backup-symlink", roots, false).expect("start run");
    symlink(&external, ctx.run_dir().join("backups").join("000001"))
        .expect("plant backup sequence redirect");
    let before = directory_snapshot(&external);

    let result = mutate(
        &mut ctx,
        &target,
        Op::WriteFile {
            bytes: b"changed".to_vec(),
        },
    );

    assert!(matches!(
        result,
        Err(DoctorRuntimeError::SymlinkedRunRoot { ref path })
            if path.ends_with("backups/000001")
    ));
    assert_eq!(
        fs::read(&target).expect("read preserved target"),
        b"original"
    );
    assert_eq!(
        directory_snapshot(&external),
        before,
        "descriptor-relative backup setup must not follow a nested symlink"
    );
}

#[cfg(any(target_os = "linux", target_os = "android", target_vendor = "apple"))]
#[test]
fn finish_refuses_regular_latest_without_overwriting_or_removing_it() {
    let ws = fresh_workspace();
    let ctx = start_test_run(ws.path());
    let run_dir = ctx.run_dir().to_path_buf();
    let latest = ws.path().join(".doctor").join("latest");
    let sentinel = b"peer-owned regular latest file";
    fs::write(&latest, sentinel).expect("plant regular latest file");
    let canonical_latest = latest.canonicalize().expect("canonical latest path");

    let result = ctx.finish(RunStatus::CompletedOk);

    assert!(
        matches!(
            &result,
            Err(DoctorRuntimeError::UnsafeLatestEntry {
                path,
                observed_kind,
            }) if path == &canonical_latest && observed_kind == "regular file"
        ),
        "unexpected finish result: {result:?}"
    );
    assert_eq!(
        fs::read(&latest).expect("read preserved latest"),
        sentinel,
        "doctor must never overwrite or remove a regular latest entry"
    );
    let state: serde_json::Value =
        serde_json::from_slice(&fs::read(run_dir.join("state.json")).expect("read run state"))
            .expect("parse run state");
    assert_eq!(
        state["status"], "failed",
        "a finalization failure must not leave completed_ok in state.json"
    );
    assert!(
        state["finished_at"].is_string(),
        "a finalization failure must persist a terminal timestamp"
    );
    assert_persistent_doctor_lock_released(ws.path());
}

#[cfg(any(target_os = "linux", target_os = "android", target_vendor = "apple"))]
#[test]
fn finish_atomically_replaces_symlink_latest_and_preserves_prior_pointer() {
    let ws = fresh_workspace();
    let first = start_test_run(ws.path());
    let first_run_id = first.run_id().to_owned();
    first
        .finish(RunStatus::CompletedOk)
        .expect("finish first run");
    let latest = ws.path().join(".doctor").join("latest");
    let first_target = fs::read_link(&latest).expect("read first latest");
    assert_eq!(first_target, Path::new("runs").join(first_run_id));

    let second = start_test_run(ws.path());
    let second_run_id = second.run_id().to_owned();
    let second_run_dir = second.run_dir().to_path_buf();
    second
        .finish(RunStatus::CompletedOk)
        .expect("finish second run");

    assert_eq!(
        fs::read_link(&latest).expect("read replaced latest"),
        Path::new("runs").join(second_run_id)
    );
    assert_eq!(
        fs::read_link(second_run_dir.join("previous-latest"))
            .expect("read preserved prior pointer"),
        first_target,
        "latest exchange must retain the old symlink as a run artifact"
    );
}

#[cfg(any(target_os = "linux", target_os = "android", target_vendor = "apple"))]
#[test]
fn finish_reports_prior_pointer_preservation_failure_without_losing_old_symlink() {
    let ws = fresh_workspace();
    let first = start_test_run(ws.path());
    first
        .finish(RunStatus::CompletedOk)
        .expect("finish first run");
    let latest = ws.path().join(".doctor").join("latest");
    let first_target = fs::read_link(&latest).expect("read first latest");

    let second = start_test_run(ws.path());
    let second_run_id = second.run_id().to_owned();
    let second_run_dir = second.run_dir().to_path_buf();
    let previous = second_run_dir.join("previous-latest");
    fs::write(&previous, b"peer-owned preservation slot").expect("occupy preservation slot");

    let result = second.finish(RunStatus::CompletedOk);

    assert!(matches!(
        result,
        Err(DoctorRuntimeError::Io { ref context, .. })
            if context.contains("preserve prior latest")
    ));
    assert_eq!(
        fs::read(&previous).expect("read preserved peer entry"),
        b"peer-owned preservation slot"
    );
    assert_eq!(
        fs::read_link(second_run_dir.join("latest-candidate"))
            .expect("read retained displaced latest"),
        first_target,
        "failed preservation must leave the old symlink retained as the candidate"
    );
    assert_eq!(
        fs::read_link(&latest).expect("read newly published latest"),
        Path::new("runs").join(second_run_id)
    );
}

#[cfg(any(target_os = "linux", target_os = "android", target_vendor = "apple"))]
#[test]
fn rustix_exposes_atomic_latest_rename_flags_on_supported_targets() {
    let no_replace = rustix::fs::RenameFlags::NOREPLACE;
    let exchange = rustix::fs::RenameFlags::EXCHANGE;

    assert_ne!(no_replace, exchange);
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
        Some(RUN_STATE_SCHEMA_V2)
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

#[cfg(any(target_os = "linux", target_os = "android", target_vendor = "apple"))]
#[test]
fn quarantine_refuses_nested_symlink_without_touching_external_destination() {
    use std::os::unix::fs::symlink;

    let root = fresh_workspace();
    let workspace = root.path().join("workspace");
    let external = root.path().join("external-quarantine");
    fs::create_dir(&workspace).expect("create workspace");
    fs::create_dir(&external).expect("create external quarantine target");
    fs::write(
        external.join("sentinel"),
        b"quarantine destination is immutable",
    )
    .expect("write sentinel");
    let target = workspace.join("orphan.wal");
    fs::write(&target, b"original wal").expect("write target");

    let mut roots = default_blast_radius_roots(&workspace);
    roots.push(workspace.clone());
    let mut ctx = RunContext::start(&workspace, "nested-quarantine-symlink", roots, false)
        .expect("start run");
    symlink(&external, ctx.run_dir().join("quarantine").join("redirect"))
        .expect("plant nested quarantine redirect");
    let before = directory_snapshot(&external);

    let result = mutate(
        &mut ctx,
        &target,
        Op::QuarantineByRename {
            dest_under_quarantine: PathBuf::from("redirect/orphan.wal"),
        },
    );

    assert!(matches!(
        result,
        Err(DoctorRuntimeError::SymlinkedRunRoot { ref path })
            if path.ends_with("quarantine/redirect")
    ));
    assert_eq!(
        fs::read(&target).expect("read preserved target"),
        b"original wal"
    );
    assert_eq!(
        directory_snapshot(&external),
        before,
        "descriptor-relative quarantine setup must not follow a nested symlink"
    );
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
    let first = replay_test_undo(&run_dir).expect("replay 1");
    assert_eq!(first.actions_undone, 1);
    assert_eq!(fs::read(&target).unwrap(), b"a");

    // Second replay: 0 actions undone, 1 skipped (already done).
    let second = replay_test_undo(&run_dir).expect("replay 2");
    assert_eq!(second.actions_undone, 0);
    assert_eq!(second.actions_skipped, 1);
    // File still byte-identical to the original.
    assert_eq!(fs::read(&target).unwrap(), b"a");
}

#[test]
fn undo_create_dir_all_reports_partial_when_created_dir_gains_content() {
    let ws = fresh_workspace();
    let target = ws.path().join("doctor-created-dir");

    let run_dir;
    {
        let mut ctx = start_test_run(ws.path());
        run_dir = ctx.run_dir().to_path_buf();
        mutate(&mut ctx, &target, Op::CreateDirAll { mode: 0o755 }).unwrap();
        ctx.finish(RunStatus::CompletedOk).unwrap();
    }

    fs::write(target.join("peer-owned.txt"), b"do not hide this").unwrap();

    let summary =
        replay_test_undo(&run_dir).expect("undo should report partial instead of erroring");

    assert!(matches!(summary.status, RunStatus::UndonePartial));
    assert_eq!(summary.actions_undone, 0);
    assert!(summary.first_error.as_deref().is_some_and(|error| {
        error.contains("drifted") && error.contains("<non-empty directory>")
    }));
    assert!(
        target.join("peer-owned.txt").exists(),
        "undo must leave peer-created directory contents in place"
    );
}
