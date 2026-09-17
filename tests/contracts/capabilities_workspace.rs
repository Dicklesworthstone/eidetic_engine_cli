//! `ee capabilities` must report the workspace it was handed (bd-7p933).
//!
//! `capabilities` is a machine-facing surface: an agent reads it to decide what
//! this store can do. If it probed the process directory instead of the
//! `--workspace` it was given, an agent would be handed the capabilities of
//! whatever directory the process happened to start in, while an argument it
//! passed was silently dropped.
//!
//! The reason this is pinned rather than measured: bd-7p933 originally used peak
//! RSS as the discriminator (~2 GB meaning "it opened the repo store", ~53 MB
//! meaning "it opened nothing"). That observable did not measure workspace
//! selection at all -- it measured whether the embedding model got loaded, which
//! is process-global and happens for any registered workspace. `7b9729767`
//! stopped status and doctor loading that model, so both arms now read alike and
//! the RSS gap is gone. An observable that a memory fix can erase was never
//! measuring the argument.
//!
//! This asserts the thing itself: two workspaces in different states, and the
//! report must follow the path it was given. If the argument were ignored, both
//! arms would describe the same directory and the statuses could not differ.

use ee::core::capabilities::CapabilitiesReport;
use ee::core::init::{InitOptions, InitStatus, init_workspace};
use ee::models::CapabilityStatus;
use std::path::{Path, PathBuf};

type TestResult = Result<(), String>;

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

/// Canonicalized so the probe and the assertion agree on one spelling: on macOS
/// the temp root is reached through a `/var` -> `/private/var` symlink, and ee
/// carries a guard that rejects store paths traversing one.
fn temp_workspace(prefix: &str) -> Result<(tempfile::TempDir, PathBuf), String> {
    let dir = tempfile::Builder::new()
        .prefix(prefix)
        .tempdir()
        .map_err(|error| format!("create temp workspace: {error}"))?;
    let path = dir
        .path()
        .canonicalize()
        .map_err(|error| format!("canonicalize temp workspace: {error}"))?;
    Ok((dir, path))
}

/// `ee init` owns store creation (91cf7bcbd); `create_dir_all` is not enough, and
/// a fixture that skips it addresses a store that does not exist.
fn initialize(path: &Path) -> TestResult {
    let report = init_workspace(&InitOptions {
        workspace_path: path.to_path_buf(),
        dry_run: false,
        repair_plan: false,
        force: false,
        allow_symlink: false,
        skip_boilerplate: true,
    });
    ensure(
        matches!(
            report.status,
            InitStatus::Created | InitStatus::AlreadyExists
        ),
        format!(
            "init_workspace must persist the workspace row: status={:?} errors={:?}",
            report.status, report.action_errors
        ),
    )
}

fn subsystem_status(report: &CapabilitiesReport, name: &str) -> Result<CapabilityStatus, String> {
    report
        .subsystems
        .iter()
        .find(|entry| entry.name == name)
        .map(|entry| entry.status)
        .ok_or_else(|| format!("capabilities report has no `{name}` subsystem"))
}

#[test]
fn capabilities_probes_the_requested_workspace_not_the_process_directory() -> TestResult {
    let (_initialized_dir, initialized) = temp_workspace("ee-capabilities-initialized-")?;
    initialize(&initialized)?;
    let (_bare_dir, bare) = temp_workspace("ee-capabilities-bare-")?;

    let initialized_report = CapabilitiesReport::gather_for_workspace(&initialized, Vec::new());
    let bare_report = CapabilitiesReport::gather_for_workspace(&bare, Vec::new());

    let initialized_storage = subsystem_status(&initialized_report, "storage")?;
    let bare_storage = subsystem_status(&bare_report, "storage")?;

    // An initialized workspace has a store to open.
    ensure(
        matches!(initialized_storage, CapabilityStatus::Ready),
        format!(
            "capabilities on an initialized workspace must report storage Ready, got {initialized_storage:?}"
        ),
    )?;
    // A bare directory has none, and the probe must say so rather than describing
    // some other directory that does.
    ensure(
        matches!(bare_storage, CapabilityStatus::Pending),
        format!(
            "capabilities on a workspace with no store must report storage Pending, got {bare_storage:?}"
        ),
    )?;

    // The load-bearing assertion. Both reports were produced by the same process,
    // in the same working directory, differing only in the path passed. If that
    // path were ignored, both would describe the same directory and these two
    // statuses would be equal.
    ensure(
        initialized_storage != bare_storage,
        "capabilities returned the same storage posture for an initialized and a \
         storeless workspace, which means it is not reading the workspace it was given",
    )
}

#[test]
fn capabilities_without_a_workspace_reports_pending() -> TestResult {
    // The no-argument arm must report Pending rather than inventing a posture.
    //
    // This does NOT detect a cwd probe, and an earlier version of this comment
    // claimed it did. The claim rested on the test binary running inside a
    // checkout that has a store -- true on a dev Mac, false where this actually
    // runs: `.ee/` is gitignored (.gitignore:17), so an RCH worker's clean
    // overlay has no store in the process directory. There, a cwd probe would
    // also report Pending and this assertion would pass while proving nothing.
    //
    // Kept because Pending IS the contract for "no workspace given" and a
    // regression to Ready or Degraded would be wrong on any host. The
    // discriminating assertion lives in the test above, which compares two
    // different paths inside one process and cannot be satisfied by a probe
    // that ignores the argument.
    let report = CapabilitiesReport::gather(Vec::new());
    let storage = subsystem_status(&report, "storage")?;
    ensure(
        matches!(storage, CapabilityStatus::Pending),
        format!("capabilities with no workspace must report storage Pending, got {storage:?}"),
    )
}
