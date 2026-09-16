//! bd-ns-gate-open-retry-classifier-wg18a: the owed two-process first-open race.
//!
//! The bead's acceptance names two tests. The unit half — that the FrankenSQLite
//! namespace-sidecar `CannotOpen` message classifies retryable — landed in
//! `src/db/mod.rs` with `8068719e9`. This is the other half:
//!
//! > *a concurrency test (two processes racing first open on a fresh workspace)
//! > proving the retry ladder absorbs the race.*
//!
//! # What races
//!
//! FrankenSQLite binds an opened database to its pathname namespace through two
//! persistent sidecars, `<db>-fsqlite-ns-gate` and `<db>-fsqlite-ns-use`. A
//! read-write admission creates them with `O_CREAT|O_EXCL`; a read-only or
//! existing-companion admission probes for them and then opens them. On the very
//! first contact with a workspace those two paths race: a reader can observe the
//! gate a creating peer just published and still lose the following open, or find
//! the companion absent mid-publication. `fsqlite-vfs`'s
//! `open_existing_secure_lock_file` reports that as
//! `FrankenError::CannotOpen { path: <sidecar> }`, which
//! `sqlmodel-frankensqlite` maps verbatim to
//! `"unable to open database file: '<...>-fsqlite-ns-gate'"`.
//!
//! Before `8068719e9` that message matched none of the retryable literals, so the
//! first `ee` invocation of a session could fail outright on a race the next
//! invocation won — the field report behind this bead. The fix routes it into the
//! bounded `FILE_DATABASE_OPEN_MAX_ATTEMPTS` open ladder.
//!
//! # Why this is not a flaky test
//!
//! It borrows the separation rule from the retrieval oracle
//! (`tests/retrieval_index_regression_oracle.rs`): **a probe that fails to
//! complete is a resource signal, never evidence about the race.** A process
//! killed by load, or failing for an unrelated reason, must not be able to
//! masquerade as "the ladder absorbed it" *or* to manufacture a reproduction.
//! So the two are counted separately:
//!
//! - **Any** probe whose output carries the namespace-sidecar signature is a
//!   reproduction, and fails the test immediately — one observation is enough,
//!   because the ladder is supposed to make that message unreachable.
//! - Probes that fail for other reasons are reported verbatim and counted; the
//!   test fails only if too few completed to conclude anything, and it says so
//!   in those words rather than claiming the race is absent.
//!
//! That asymmetry is deliberate. The race is intermittent by nature, so absence
//! of the signature across a quorum is the strongest available evidence, while a
//! single appearance is conclusive the other way.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;
use std::process::{Command, Output, Stdio};

type TestResult = Result<(), String>;

/// Concurrent processes per phase. Both phases race the same fresh workspace.
const PROBES: usize = 8;

/// How many probes must complete before absence of the signature means anything.
/// Below this the verdict is inconclusive, not clean.
const QUORUM: usize = 5;

/// The verbatim rendering of `FrankenError::CannotOpen`, from
/// `fsqlite-error`'s `#[error("unable to open database file: '{path}'")]`.
const CANNOT_OPEN_PREFIX: &str = "unable to open database file";

/// `fsqlite-vfs::namespace`'s `GATE_SUFFIX` and `USE_SUFFIX`.
const NAMESPACE_SIDECAR_SUFFIXES: [&str; 2] = ["-fsqlite-ns-gate", "-fsqlite-ns-use"];

fn ee_command(workspace: &Path, data_home: &Path, args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ee"));
    command
        .args(args)
        .env_remove("EE_WORKSPACE")
        .env_remove("EE_WORKSPACE_REGISTRY")
        .env_remove("EE_DATABASE_PATH")
        .env_remove("EE_INDEX_DIR")
        // Keep the user-global lane and any model download out of the test.
        .env("HOME", data_home)
        .env("XDG_DATA_HOME", data_home)
        .env("XDG_CONFIG_HOME", data_home)
        .env("EE_EMBED_DOWNLOAD", "off")
        .env("EE_NO_COLOR", "1")
        .arg("--workspace")
        .arg(workspace)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

/// Does this probe's output carry the namespace-sidecar open failure?
///
/// Deliberately narrow, matching the classifier it is proving: the message must
/// be FrankenSQLite's `CannotOpen` rendering **and** name one of the two
/// namespace sidecars. A `CannotOpen` on the database path itself is a genuine
/// fatal error and is not this bead's race.
fn carries_namespace_sidecar_failure(text: &str) -> bool {
    let lowered = text.to_ascii_lowercase();
    lowered.contains(CANNOT_OPEN_PREFIX)
        && NAMESPACE_SIDECAR_SUFFIXES
            .iter()
            .any(|suffix| lowered.contains(suffix))
}

struct ProbeOutcome {
    completed: bool,
    detail: String,
}

/// Run `count` processes concurrently against one workspace and classify each.
fn race(workspace: &Path, data_home: &Path, args: &[&str], count: usize) -> Vec<ProbeOutcome> {
    let children: Vec<_> = (0..count)
        .map(|index| {
            (
                index,
                ee_command(workspace, data_home, args)
                    .spawn()
                    .map_err(|error| format!("spawn failed: {error}")),
            )
        })
        .collect();

    children
        .into_iter()
        .map(|(index, child)| match child {
            Err(error) => ProbeOutcome {
                completed: false,
                detail: format!("#{index}: {error}"),
            },
            Ok(child) => match child.wait_with_output() {
                Err(error) => ProbeOutcome {
                    completed: false,
                    detail: format!("#{index}: wait failed: {error}"),
                },
                Ok(output) => classify(index, &output),
            },
        })
        .collect()
}

fn classify(index: usize, output: &Output) -> ProbeOutcome {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let combined = format!("{stdout}{stderr}");
    if output.status.success() {
        return ProbeOutcome {
            completed: true,
            detail: combined,
        };
    }
    ProbeOutcome {
        completed: false,
        detail: format!(
            "#{index}: exited {:?}\nstdout: {stdout}\nstderr: {stderr}",
            output.status.code()
        ),
    }
}

/// Fail immediately if any probe surfaced the sidecar signature, then require a
/// quorum of completions before treating its absence as meaningful.
fn assert_race_absorbed(phase: &str, outcomes: &[ProbeOutcome]) -> TestResult {
    for outcome in outcomes {
        if carries_namespace_sidecar_failure(&outcome.detail) {
            return Err(format!(
                "{phase}: a process surfaced the FrankenSQLite namespace-sidecar open failure, so the retry ladder did not absorb the first-open race (bd-ns-gate-open-retry-classifier-wg18a):\n{}",
                outcome.detail
            ));
        }
    }

    let completed = outcomes.iter().filter(|outcome| outcome.completed).count();
    if completed < QUORUM {
        let incomplete: Vec<&str> = outcomes
            .iter()
            .filter(|outcome| !outcome.completed)
            .map(|outcome| outcome.detail.as_str())
            .collect();
        return Err(format!(
            "{phase}: only {completed} of {} probes completed (quorum {QUORUM}). This is a resource signal, NOT evidence that the race is absent — none of these carried the sidecar signature:\n{}",
            outcomes.len(),
            incomplete.join("\n---\n")
        ));
    }
    Ok(())
}

#[test]
fn concurrent_first_open_of_a_fresh_workspace_absorbs_the_ns_gate_race() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
    let workspace = tempdir.path().join("workspace");
    let data_home = tempdir.path().join("home");
    std::fs::create_dir_all(&workspace).map_err(|error| error.to_string())?;
    std::fs::create_dir_all(&data_home).map_err(|error| error.to_string())?;

    // Phase 1 — the creation race. The workspace has never been opened, so the
    // namespace sidecars do not exist and these processes race to publish them.
    // `ee init` is idempotent (src/core/init.rs:352 "returns success if already
    // initialized"), so every probe is expected to succeed; a loser of the
    // creation race must retry through the ladder, not fail.
    let init = race(&workspace, &data_home, &["init", "--json"], PROBES);
    assert_race_absorbed("concurrent first init", &init)?;

    // Phase 2 — the read race, which is the shape the field report actually hit:
    // the first read-only open of a store whose sidecars were just published.
    let status = race(&workspace, &data_home, &["status", "--json"], PROBES);
    assert_race_absorbed("concurrent first read", &status)?;

    // Guard against a vacuous pass: if `ee` never actually opened the store,
    // every probe above could "succeed" without exercising the namespace
    // admission at all. A real store on disk is the evidence that it did.
    let database = workspace.join(".ee").join("ee.db");
    if !database.is_file() {
        return Err(format!(
            "no database at {}, so the probes never reached namespace admission and this test proved nothing",
            database.display()
        ));
    }
    Ok(())
}
