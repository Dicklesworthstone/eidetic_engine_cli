//! bd-36bbk.1.19 — SRR6.46.19 implements-surface:
//! auto_enroll_ci_integration. CI gate coverage contract.
//!
//! The bead's integration point #1 says: "Every SRR6.46.* sub-bead
//! with an e2e script must be wired into ./scripts/verify.sh in
//! dependency-graph topological order, with the same fail-fast
//! semantics as the existing verify.sh stages."
//!
//! This contract pins the CURRENT canonical wiring so a regression
//! that drops the fake-tailscale or tailscale-local-probe stage is
//! caught immediately, and so that any new SRR6.46.* e2e script that
//! lands on disk fails the contract until it gets a `run_stage`
//! invocation in verify.sh AND a corresponding row in the closed-set
//! map below. That coupling enforces the ordering and discoverability
//! contract the bead acceptance demands.
//!
//! Asserts:
//!
//! 1. scripts/verify.sh exists at the canonical path.
//! 2. Each currently-wired SRR6.46.* stage is invoked exactly once
//!    via `run_stage` with the documented label + script path.
//! 3. The fake-tailscale harness self-test runs strictly before the
//!    tailscale-local-probe stage (the bead's documented ordering
//!    requirement: harness primitives first, then surfaces that
//!    import them).
//! 4. Every SRR6.46-headed e2e script on disk is either (a) wired
//!    into verify.sh with a matching `run_stage` line, or (b)
//!    explicitly listed in the PENDING_SRR6_46_SCRIPTS closed set
//!    below. New scripts that land without being added to either
//!    list fail the contract — that is the implements-surface gate.

use std::fs;
use std::path::PathBuf;

type TestResult = Result<(), String>;

const VERIFY_PATH: &str = "scripts/verify.sh";
const E2E_DIR: &str = "scripts/e2e_overhaul";

#[derive(Clone, Copy)]
struct WiredStage {
    label: &'static str,
    script_rel: &'static str,
}

/// SRR6.46.* e2e stages currently wired into scripts/verify.sh.
/// Extend this list (and the verify.sh `run_stage` call) together
/// whenever a new SRR6.46.* sub-bead ships its e2e harness.
const WIRED_SRR6_46_STAGES: &[WiredStage] = &[
    WiredStage {
        label: "Fake Tailscale Harness E2E (SRR6.46.10)",
        script_rel: "./scripts/e2e_overhaul/lib/test_fake_tailscale.sh",
    },
    WiredStage {
        label: "Tailscale Local Probe E2E (SRR6.46.1)",
        script_rel: "./scripts/e2e_overhaul/tailscale_local_probe.sh",
    },
];

/// SRR6.46.* e2e scripts that exist on disk but are intentionally
/// deferred from verify.sh wiring (e.g. opt-in real-Tailscale tests
/// behind `EE_E2E_REAL_TAILSCALE=1`, or smoke tests gated on their
/// owning sub-bead landing first). Extend this list when a script
/// lands but its `run_stage` wiring is deliberately deferred to a
/// later sub-bead — never as a workaround for "the wiring isn't
/// ready yet".
const PENDING_SRR6_46_SCRIPTS: &[&str] = &[
    "scripts/e2e_overhaul/auto_enrollment_safety_snapshot.sh",
    "scripts/e2e_overhaul/auto_enroll_documentation_set.sh",
    "scripts/e2e_overhaul/auto_enroll_perf_gate.sh",
    "scripts/e2e_overhaul/auto_enroll_idle_24h.sh",
    "scripts/e2e_overhaul/peer_discovery_policy.sh",
    // bd-36bbk.1.11 — opt-in real-Tailscale smoke, gated on
    // EE_E2E_REAL_TAILSCALE=1; exits 78 without the opt-in flag and
    // is deliberately outside normal CI per the script header.
    "scripts/e2e_overhaul/auto_enroll_real_tailscale.sh",
    // bd-21xbi.3 — host-class p99 benchmark gate for the lexical
    // RAM-tier optimization, gated on EE_HUGE_HOST=1 (only meaningful
    // on 256GB+ / 64-core hosts); exits 78 otherwise.
    "scripts/e2e_overhaul/lexical_ram_tier_p99_proof.sh",
    // bd-36bbk.2 — opt-in real-Tailscale `ee mesh sync --once` smoke,
    // gated on EE_E2E_REAL_TAILSCALE=1; exits 78 otherwise. Parallels
    // auto_enroll_real_tailscale.sh on the mesh-sync surface.
    "scripts/e2e_overhaul/mesh_sync_once_real_tailscale.sh",
];

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn verify_body() -> Result<String, String> {
    let path = repo_root().join(VERIFY_PATH);
    fs::read_to_string(&path).map_err(|error| format!("read {}: {error}", path.display()))
}

#[test]
fn verify_script_exists_at_canonical_path() -> TestResult {
    let _ = verify_body()?;
    Ok(())
}

#[test]
fn every_wired_srr6_46_stage_appears_exactly_once_in_verify_sh() -> TestResult {
    let body = verify_body()?;
    for stage in WIRED_SRR6_46_STAGES {
        // Count only invocations of `run_stage "<label>"`. Bare label
        // mentions inside ci-smoke `SKIP ...` echos (e.g.
        // `"${STAGE_RESULTS}SKIP Fake Tailscale Harness E2E
        // (SRR6.46.10) (ci-smoke)\n"`) advertise that the stage was
        // skipped — they are not duplicate wirings.
        let label_needle = format!("run_stage \"{}\"", stage.label);
        let label_hits = body.matches(label_needle.as_str()).count();
        let script_hits = body.matches(stage.script_rel).count();
        ensure(
            label_hits == 1,
            format!(
                "expected exactly one `run_stage` invocation with label {:?} in verify.sh; found {label_hits}",
                stage.label
            ),
        )?;
        ensure(
            script_hits == 1,
            format!(
                "expected exactly one reference to {:?} in verify.sh; found {script_hits}",
                stage.script_rel
            ),
        )?;
    }
    Ok(())
}

#[test]
fn fake_tailscale_harness_runs_strictly_before_local_probe() -> TestResult {
    let body = verify_body()?;
    let harness_idx = body
        .find("Fake Tailscale Harness E2E (SRR6.46.10)")
        .ok_or("fake-tailscale harness stage missing from verify.sh")?;
    let probe_idx = body
        .find("Tailscale Local Probe E2E (SRR6.46.1)")
        .ok_or("tailscale-local-probe stage missing from verify.sh")?;
    ensure(
        harness_idx < probe_idx,
        "fake-tailscale harness self-test must run strictly before the tailscale-local-probe stage (the bead's documented dependency ordering: harness primitives first, then surfaces that import them)",
    )
}

#[test]
fn every_srr6_46_script_on_disk_is_wired_or_pending() -> TestResult {
    let body = verify_body()?;
    let e2e_dir = repo_root().join(E2E_DIR);
    let mut srr6_46_scripts: Vec<PathBuf> = Vec::new();
    for entry in fs::read_dir(&e2e_dir)
        .map_err(|error| format!("read_dir {}: {error}", e2e_dir.display()))?
    {
        let entry = entry.map_err(|error| format!("read_dir entry: {error}"))?;
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("sh") {
            continue;
        }
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(_) => continue,
        };
        if text.contains("SRR6.46.") || text.contains("bd-36bbk.1.") {
            srr6_46_scripts.push(path);
        }
    }
    srr6_46_scripts.sort();

    let wired_paths: Vec<String> = WIRED_SRR6_46_STAGES
        .iter()
        .map(|stage| stage.script_rel.trim_start_matches("./").to_owned())
        .collect();
    let pending_paths: Vec<String> = PENDING_SRR6_46_SCRIPTS
        .iter()
        .map(|p| (*p).to_owned())
        .collect();

    for script in &srr6_46_scripts {
        let rel = script
            .strip_prefix(repo_root())
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| script.to_string_lossy().into_owned());
        let wired = wired_paths.iter().any(|w| w == &rel);
        let pending = pending_paths.iter().any(|p| p == &rel);
        let referenced_in_verify = body.contains(&rel);
        ensure(
            wired || pending || referenced_in_verify,
            format!(
                "SRR6.46.* e2e script {rel} is on disk but neither wired into verify.sh via run_stage nor explicitly listed in PENDING_SRR6_46_SCRIPTS. New SRR6.46.* sub-beads must wire their e2e script AND extend this contract together.",
            ),
        )?;
    }
    Ok(())
}

#[test]
fn wired_scripts_exist_on_disk_and_are_executable() -> TestResult {
    for stage in WIRED_SRR6_46_STAGES {
        let rel = stage.script_rel.trim_start_matches("./");
        let path = repo_root().join(rel);
        let metadata =
            fs::metadata(&path).map_err(|error| format!("stat {}: {error}", path.display()))?;
        ensure(metadata.is_file(), format!("{rel} must be a regular file"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = metadata.permissions().mode();
            ensure(
                mode & 0o111 != 0,
                format!("{rel} must be executable (mode={mode:o})"),
            )?;
        }
    }
    Ok(())
}
