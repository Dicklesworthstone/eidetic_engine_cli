//! Contract test for bd-3boan slice 9 and bd-3ak9b: pin the
//! `scripts/e2e_overhaul/doctor_undo_replay.sh` harness so it can't
//! regress out of the tree without a corresponding contract update.
//!
//! The script is the round-trip proof for the `ee doctor --fix +
//! --undo` chokepoint scaffold; it is the only test surface today
//! that covers the public CLI lifecycle without invoking Cargo, which
//! is critical while the per-FM fixer dispatch table (bd-tu4s8) is
//! still pending and full e2e via Cargo is RCH-environment-blocked
//! per bd-17c65.10.19.
//!
//! This contract asserts:
//!
//! 1. The script exists at the canonical path.
//! 2. It begins with a bash shebang and `set -euo pipefail`.
//! 3. It declares the bd-3boan and bd-3ak9b bead ids in a header comment.
//! 4. It invokes both `ee doctor --fix` and `ee doctor --undo`.
//! 5. It asserts the canonical `ee.response.v2` success envelope for
//!    `ee.doctor.fix_summary.v1`, plus the canonical
//!    `ee.doctor.undo_summary.v1` undo surface.
//! 6. It forces a finalization failure and asserts nonzero `ee.error.v2`,
//!    failed persisted state, lock cleanup, peer-file preservation, and undo.
//! 7. It refuses Cargo/git/destructive shortcuts (no `cargo `, no
//!    `git reset`, no `rm -rf` literal — the harness lives outside
//!    the per-FM fixture suite and uses a temp workspace it leaves
//!    behind for operator inspection per AGENTS.md RULE 1).

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::fs;
use std::path::PathBuf;

fn script_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts/e2e_overhaul/doctor_undo_replay.sh")
}

#[test]
fn doctor_undo_replay_script_present_and_executable() {
    let path = script_path();
    let metadata = fs::metadata(&path).unwrap_or_else(|error| {
        panic!(
            "expected doctor_undo_replay.sh at {}: {error}",
            path.display()
        );
    });
    assert!(
        metadata.is_file(),
        "doctor_undo_replay.sh must be a regular file"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = metadata.permissions().mode();
        assert!(
            mode & 0o111 != 0,
            "doctor_undo_replay.sh must be executable (mode={mode:o})"
        );
    }
}

#[test]
fn doctor_undo_replay_script_declares_bash_shebang_and_strict_mode() {
    let body = fs::read_to_string(script_path()).expect("read script");
    let first = body.lines().next().unwrap_or("");
    assert!(
        first.starts_with("#!"),
        "doctor_undo_replay.sh must start with a shebang"
    );
    assert!(
        first.contains("bash"),
        "doctor_undo_replay.sh shebang must select bash"
    );
    assert!(
        body.contains("set -euo pipefail"),
        "doctor_undo_replay.sh must enable strict mode (`set -euo pipefail`)"
    );
}

#[test]
fn doctor_undo_replay_script_declares_owning_beads_in_header() {
    let body = fs::read_to_string(script_path()).expect("read script");
    let header: String = body.lines().take(40).collect::<Vec<_>>().join("\n");
    for bead in ["bd-3boan", "bd-3ak9b"] {
        assert!(
            header.contains(bead),
            "header must name {bead} so future audits can locate the owning bead from the script itself"
        );
    }
}

#[test]
fn doctor_undo_replay_script_invokes_fix_and_undo() {
    let body = fs::read_to_string(script_path()).expect("read script");
    assert!(
        body.contains("doctor --fix"),
        "script must invoke `ee ... doctor --fix` so the chokepoint runs"
    );
    assert!(
        body.contains("doctor --undo"),
        "script must invoke `ee ... doctor --undo` so the round-trip closes"
    );
}

#[test]
fn doctor_undo_replay_script_asserts_canonical_schemas() {
    let body = fs::read_to_string(script_path()).expect("read script");
    for needle in [
        "ee.response.v2",
        "ee.error.v2",
        "ee.doctor.fix_summary.v1",
        "ee.doctor.undo_summary.v1",
        "ee.doctor.run_state.v1",
    ] {
        assert!(
            body.contains(needle),
            "script must assert the {needle} schema"
        );
    }
}

#[test]
fn doctor_undo_replay_script_asserts_truthful_failure_lifecycle() {
    let body = fs::read_to_string(script_path()).expect("read script");
    for needle in [
        "failure_exit",
        "doctor_latest_entry_unsafe",
        "fixerResults",
        "structured recovery",
        "machine error leaked to stderr",
        "doctor lock survived failed finalization",
        "assert_state_json \"$run_id\" \"failed\"",
        "run_undo \"$run_id\" \"$FAILURE_WORKSPACE\"",
        "assert_state_json \"$run_id\" \"undone\"",
    ] {
        assert!(
            body.contains(needle),
            "doctor failure/undo harness must assert {needle:?}"
        );
    }
}

#[test]
fn doctor_undo_replay_script_refuses_cargo_and_destructive_shortcuts() {
    let body = fs::read_to_string(script_path()).expect("read script");
    for forbidden in ["cargo ", "rustc ", "rustdoc ", "git reset", "rm -rf"] {
        assert!(
            !body.contains(forbidden),
            "doctor_undo_replay.sh contains forbidden token {forbidden:?}; harness must stay non-Cargo and non-destructive per AGENTS.md"
        );
    }
}
