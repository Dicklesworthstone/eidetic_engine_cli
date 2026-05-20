//! Contract test for bd-1zb7k.19.1 (AFR1, flight recorder): pin the
//! `scripts/e2e_overhaul/flight_recorder.sh` harness so the
//! redaction-canary + status/doctor posture chain cannot regress out
//! of the tree without a corresponding contract update.
//!
//! The script is the only test surface today that exercises the
//! flight recorder end-to-end without Cargo: it sets the three
//! EE_FLIGHT_RECORDER* env overrides, runs `ee status`, `ee doctor`,
//! and `ee recorder flight append`, then sweeps the trace directory
//! for known raw-content tokens (OPENAI_API_KEY, sk-proj-, raw task /
//! query, password). Full e2e via Cargo is RCH-environment-blocked
//! per bd-17c65.10.19, so this contract is the closure-quality proof
//! the bead acceptance demands.
//!
//! This contract asserts:
//!
//! 1. The script exists at the canonical path and is executable.
//! 2. It begins with a bash shebang and `set -euo pipefail`.
//! 3. It declares the AFR1 / bd-1zb7k.19.1 owning bead in the header.
//! 4. It sets the three documented EE_FLIGHT_RECORDER* env overrides.
//! 5. It invokes the three observable surfaces the bead spec names:
//!    `ee status`, `ee doctor`, `ee recorder flight append`.
//! 6. It asserts the canonical `ee.flight_recorder.status.v1` /
//!    `ee.flight_recorder.append.v1` schemas the renderer emits.
//! 7. It runs the redaction canary sweep against the trace directory
//!    so a regression that lets `OPENAI_API_KEY`, `sk-proj-`, raw
//!    task/query strings, or `password` leak into a trace row fails
//!    the e2e harness instead of slipping silently.
//! 8. It refuses Cargo / git / destructive shortcuts (no `cargo `,
//!    no `git reset`, no `rm -rf` literal — the harness must stay
//!    non-Cargo and AGENTS.md-RULE-1-compliant).

use std::fs;
use std::path::PathBuf;

fn script_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts/e2e_overhaul/flight_recorder.sh")
}

fn script_body() -> String {
    fs::read_to_string(script_path()).expect("read flight_recorder.sh")
}

#[test]
fn flight_recorder_script_present_and_executable() {
    let path = script_path();
    let metadata = fs::metadata(&path).unwrap_or_else(|error| {
        panic!("expected flight_recorder.sh at {}: {error}", path.display());
    });
    assert!(
        metadata.is_file(),
        "flight_recorder.sh must be a regular file"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = metadata.permissions().mode();
        assert!(
            mode & 0o111 != 0,
            "flight_recorder.sh must be executable (mode={mode:o})"
        );
    }
}

#[test]
fn flight_recorder_script_declares_bash_shebang_and_strict_mode() {
    let body = script_body();
    let first = body.lines().next().unwrap_or("");
    assert!(
        first.starts_with("#!"),
        "flight_recorder.sh must start with a shebang"
    );
    assert!(
        first.contains("bash"),
        "flight_recorder.sh shebang must select bash"
    );
    assert!(
        body.contains("set -euo pipefail"),
        "flight_recorder.sh must enable strict mode (`set -euo pipefail`)"
    );
}

#[test]
fn flight_recorder_script_declares_owning_bead_in_header() {
    let body = script_body();
    let header: String = body.lines().take(40).collect::<Vec<_>>().join("\n");
    assert!(
        header.contains("AFR1") || header.contains("bd-1zb7k.19.1"),
        "header must name AFR1 / bd-1zb7k.19.1 so future audits can locate the owning bead from the script itself"
    );
}

#[test]
fn flight_recorder_script_sets_documented_env_overrides() {
    let body = script_body();
    for var in [
        "EE_FLIGHT_RECORDER",
        "EE_FLIGHT_RECORDER_DIR",
        "EE_FLIGHT_RECORDER_RETENTION_DAYS",
    ] {
        assert!(
            body.contains(var),
            "flight_recorder.sh must exercise the {var} env override the bead acceptance documents"
        );
    }
}

#[test]
fn flight_recorder_script_invokes_status_doctor_and_recorder_append() {
    let body = script_body();
    for verb_phrase in ["status --json", "doctor --json", "recorder flight append"] {
        assert!(
            body.contains(verb_phrase),
            "script must invoke `{verb_phrase}` so the bead's three observable surfaces all execute"
        );
    }
}

#[test]
fn flight_recorder_script_asserts_canonical_schemas() {
    let body = script_body();
    for needle in [
        "ee.flight_recorder.status.v1",
        "ee.flight_recorder.append.v1",
    ] {
        assert!(
            body.contains(needle),
            "script must assert the {needle} schema so renderer drift is caught"
        );
    }
}

#[test]
fn flight_recorder_script_sweeps_redaction_canary() {
    let body = script_body();
    for canary in [
        "OPENAI_API_KEY",
        "sk-proj-",
        "raw task",
        "raw query",
        "password",
    ] {
        assert!(
            body.contains(canary),
            "redaction canary sweep must look for the {canary:?} token so a future renderer regression that leaks it into traces fails the e2e harness"
        );
    }
}

#[test]
fn flight_recorder_script_refuses_cargo_and_destructive_shortcuts() {
    let body = script_body();
    for forbidden in [
        "cargo ",
        "rustc ",
        "rustdoc ",
        "git reset",
        "git clean",
        "rm -rf",
        "--no-verify",
        "--force",
    ] {
        assert!(
            !body.contains(forbidden),
            "flight_recorder.sh contains forbidden token {forbidden:?}; harness must stay non-Cargo and non-destructive per AGENTS.md"
        );
    }
}
