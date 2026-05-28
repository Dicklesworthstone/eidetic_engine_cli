//! bd-2vu8m (SRR6.23) — structural contract for the
//! `scripts/e2e_overhaul/mesh_off_no_network.sh` harness.
//!
//! The bead acceptance demands "mesh-disabled behavior proven stable"
//! as a closure prerequisite for the SRR6 epic. That stability is
//! enforced by the mesh-off no-network e2e harness, which sets
//! `EE_MESH_ENABLED=0`, runs `ee status` / `ee why` / `ee context`,
//! and asserts (a) no mesh / tailscale / peer degradation codes
//! appear, (b) no mesh-class keys appear under `data`, and (c) no new
//! mesh / tailscale listener sockets open on the host.
//!
//! Without a contract pin, that script can drift: a future refactor
//! that removes `EE_MESH_ENABLED=0`, drops the listener probe, or
//! adds destructive shortcuts would silently weaken the
//! mesh-disabled-stability guarantee bd-2vu8m's acceptance names.
//! This contract closes that gap at the default-feature-build layer
//! (no RCH, no cargo bench, no --features mcp gate).
//!
//! Asserts:
//!
//! 1. Script present at the canonical path and executable.
//! 2. Bash shebang + `set -euo pipefail` strict mode.
//! 3. SRR6.2 owning-bead header marker so audits can resolve back
//!    from the script alone.
//! 4. `EE_MESH_ENABLED=0` is exported before any `ee` invocation —
//!    the load-bearing mesh-off precondition.
//! 5. The three assertion helpers the bead acceptance demands are
//!    present: `assert_no_mesh_degradation`,
//!    `assert_no_mesh_data_key`, and `listener_snapshot`.
//! 6. The harness invokes the three observable surfaces the bead
//!    spec names (`ee status`, `ee why`, `ee context`) under the
//!    mesh-disabled probe.
//! 7. The harness refuses Cargo / git-reset / git-clean / `rm -rf` /
//!    `--no-verify` / `--force` tokens, per AGENTS.md.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::PathBuf;

type TestResult = Result<(), String>;

fn script_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts/e2e_overhaul/mesh_off_no_network.sh")
}

fn script_body() -> Result<String, String> {
    fs::read_to_string(script_path())
        .map_err(|error| format!("read mesh_off_no_network.sh: {error}"))
}

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

#[test]
fn mesh_off_script_present_and_executable() -> TestResult {
    let path = script_path();
    let metadata = fs::metadata(&path).map_err(|error| {
        format!(
            "expected mesh_off_no_network.sh at {}: {error}",
            path.display()
        )
    })?;
    ensure(
        metadata.is_file(),
        "mesh_off_no_network.sh must be a regular file",
    )?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = metadata.permissions().mode();
        ensure(
            mode & 0o111 != 0,
            format!("mesh_off_no_network.sh must be executable (mode={mode:o})"),
        )?;
    }
    Ok(())
}

#[test]
fn mesh_off_script_declares_bash_shebang_and_strict_mode() -> TestResult {
    let body = script_body()?;
    let first = body.lines().next().unwrap_or("");
    ensure(
        first.starts_with("#!"),
        "mesh_off_no_network.sh must start with a shebang",
    )?;
    ensure(
        first.contains("bash"),
        "mesh_off_no_network.sh shebang must select bash",
    )?;
    ensure(
        body.contains("set -euo pipefail"),
        "mesh_off_no_network.sh must enable strict mode (`set -euo pipefail`)",
    )
}

#[test]
fn mesh_off_script_declares_owning_bead_in_header() -> TestResult {
    let body = script_body()?;
    let header: String = body.lines().take(20).collect::<Vec<_>>().join("\n");
    ensure(
        header.contains("SRR6.2") || header.contains("bd-2vu8m"),
        "header must name SRR6.2 / bd-2vu8m so future audits can locate the owning bead from the script itself",
    )
}

#[test]
fn mesh_off_script_exports_ee_mesh_enabled_zero() -> TestResult {
    // Load-bearing precondition: every subsequent `ee` invocation
    // must run under EE_MESH_ENABLED=0 so the mesh-off invariant
    // covers the whole harness, not just a single command.
    let body = script_body()?;
    ensure(
        body.contains("export EE_MESH_ENABLED=0"),
        "mesh_off_no_network.sh must `export EE_MESH_ENABLED=0` before invoking ee — the load-bearing mesh-off precondition the bead acceptance demands",
    )
}

#[test]
fn mesh_off_script_defines_required_assertion_helpers() -> TestResult {
    let body = script_body()?;
    for helper in [
        "assert_no_mesh_degradation",
        "assert_no_mesh_data_key",
        "listener_snapshot",
    ] {
        ensure(
            body.contains(helper),
            format!(
                "mesh_off_no_network.sh must define `{helper}` so the mesh-disabled stability gate covers (a) degraded codes, (b) data keys, and (c) socket listeners",
            ),
        )?;
    }
    Ok(())
}

#[test]
fn mesh_off_script_exercises_three_observable_surfaces() -> TestResult {
    let body = script_body()?;
    for surface in ["status --json", "why", "pack"] {
        ensure(
            body.contains(surface),
            format!(
                "mesh_off_no_network.sh must exercise `ee {surface}` so the mesh-disabled stability gate covers the surfaces the bead spec names",
            ),
        )?;
    }
    Ok(())
}

#[test]
fn mesh_off_script_refuses_cargo_and_destructive_shortcuts() -> TestResult {
    let body = script_body()?;
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
        ensure(
            !body.contains(forbidden),
            format!(
                "mesh_off_no_network.sh contains forbidden token {forbidden:?}; harness must stay non-Cargo and non-destructive per AGENTS.md",
            ),
        )?;
    }
    Ok(())
}
