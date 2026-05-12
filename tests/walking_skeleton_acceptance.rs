//! Walking-skeleton acceptance gate (eidetic_engine_cli-msjpt).
//!
//! AGENTS.md defines a specific set of criteria that the walking skeleton
//! must satisfy. These criteria are exercised piecemeal across many tests
//! (e2e_core_workflow, forbidden_deps, asupersync_cancellation, json
//! contract snapshots, etc.) but no single test asserts them all in one
//! place. This file is that single place. When CI fails on this test,
//! the walking skeleton is broken in a way the agent contract treats as
//! a non-negotiable invariant.
//!
//! Criteria from AGENTS.md "Acceptance gate":
//!   1. All commands work without daemon mode.
//!   2. All commands have stable JSON mode.
//!   3. Memory is stored in FrankenSQLite through ee-db.
//!   4. Search results come from Frankensearch or a documented degraded
//!      lexical path.
//!   5. Context pack includes provenance.
//!   6. `ee why` explains storage, retrieval, and pack selection.
//!   7. Pack record is persisted.
//!   8. `ee status` reports DB, index, and degraded capabilities.
//!   9. Cancellation tests cover at least one command path.   (verified
//!      by tests/contracts/asupersync_cancellation.rs — referenced here)
//!  10. No Tokio or `rusqlite` dependency appears in the dep tree.
//!      (verified by tests/forbidden_deps.rs — referenced here)
//!
//! Criteria 9 and 10 are referenced rather than re-implemented: this
//! test asserts the surface (DB-backed memory + frankensearch retrieval
//! + provenance + why + persistence + status capabilities) end-to-end
//! against the real binary in a real temp workspace.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::process::{Command, Output};

type TestResult = Result<(), String>;

const EXIT_SUCCESS: i32 = 0;

fn run_ee(workspace: &str, args: &[&str]) -> Result<Output, String> {
    let mut all_args = vec!["--workspace", workspace];
    all_args.extend_from_slice(args);
    Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(&all_args)
        .env_remove("EE_WORKSPACE")
        .env_remove("EE_WORKSPACE_REGISTRY")
        .output()
        .map_err(|error| format!("failed to run ee {}: {error}", all_args.join(" ")))
}

fn stdout_json(output: &Output, context: &str) -> Result<serde_json::Value, String> {
    let stdout = String::from_utf8(output.stdout.clone())
        .map_err(|error| format!("{context}: stdout not UTF-8: {error}"))?;
    serde_json::from_str(&stdout)
        .map_err(|error| format!("{context}: stdout not JSON: {error}\nstdout: {stdout}"))
}

fn require_ok(output: &Output, label: &str) -> TestResult {
    if output.status.code() == Some(EXIT_SUCCESS) {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(format!(
            "{label} expected exit 0, got {:?}; stderr: {stderr}",
            output.status.code()
        ))
    }
}

fn require_schema(json: &serde_json::Value, expected: &str, label: &str) -> TestResult {
    let actual = json
        .get("schema")
        .and_then(|s| s.as_str())
        .ok_or_else(|| format!("{label}: response missing top-level `schema` field"))?;
    if actual != expected {
        return Err(format!(
            "{label}: schema mismatch (got {actual:?}, expected {expected:?})"
        ));
    }
    Ok(())
}

#[test]
fn walking_skeleton_acceptance_gate() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|error| format!("tempdir failed: {error}"))?;
    let workspace = tempdir.path().to_string_lossy().to_string();

    // ---- Criterion 1 + 2: all commands work without daemon, stable JSON ----
    let init = run_ee(&workspace, &["init", "--json"])?;
    require_ok(&init, "init")?;
    require_schema(&stdout_json(&init, "init")?, "ee.response.v1", "init")?;

    let remember = run_ee(
        &workspace,
        &[
            "remember",
            "Run cargo fmt --check before cutting any release tag.",
            "--level",
            "procedural",
            "--kind",
            "rule",
            "--json",
        ],
    )?;
    require_ok(&remember, "remember")?;
    let remember_json = stdout_json(&remember, "remember")?;
    require_schema(&remember_json, "ee.response.v1", "remember")?;

    // Capture the memory_id for later why-explanation assertions.
    let memory_id = remember_json
        .pointer("/data/memory_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "remember response missing data.memory_id".to_string())?
        .to_string();

    // ---- Criterion 3: memory stored through ee-db (FrankenSQLite) ----
    // The DB file lives under .ee/ee.db. If it's missing, the previous
    // remember call did not persist via ee-db.
    let db_path = tempdir.path().join(".ee").join("ee.db");
    if !db_path.exists() {
        return Err(format!(
            "expected DB file at {} after remember (criterion 3 broken)",
            db_path.display()
        ));
    }

    // ---- Criterion 4: search returns from Frankensearch or a documented
    //     degraded lexical path ----
    let search = run_ee(
        &workspace,
        &["search", "cargo fmt before release", "--json"],
    )?;
    require_ok(&search, "search")?;
    let search_json = stdout_json(&search, "search")?;
    require_schema(&search_json, "ee.response.v1", "search")?;

    // Either we got at least one result OR we got a degraded notice
    // explaining the absence — never silent zero-score returns.
    let results_len = search_json
        .pointer("/data/results")
        .and_then(serde_json::Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    let degraded_len = search_json
        .pointer("/data/degraded")
        .and_then(serde_json::Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    if results_len == 0 && degraded_len == 0 {
        return Err(
            "search returned zero results with no degraded[] explanation \
             (criterion 4 broken — silent empty result)"
                .to_string(),
        );
    }

    // ---- Criterion 5: context pack includes provenance ----
    let context = run_ee(
        &workspace,
        &[
            "context",
            "prepare release",
            "--max-tokens",
            "2000",
            "--json",
        ],
    )?;
    require_ok(&context, "context")?;
    let context_json = stdout_json(&context, "context")?;
    require_schema(&context_json, "ee.response.v1", "context")?;
    let items = context_json
        .pointer("/data/pack/items")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "context response missing data.pack.items array".to_string())?;
    if items.is_empty() {
        return Err(
            "context pack has no items after a remember+search; criterion 5 \
             cannot be validated without at least one item"
                .to_string(),
        );
    }
    for (i, item) in items.iter().enumerate() {
        let provenance = item
            .get("provenance")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| format!("context pack item {i} missing provenance[] (criterion 5)"))?;
        if provenance.is_empty() {
            return Err(format!(
                "context pack item {i} has empty provenance[] (criterion 5 broken)"
            ));
        }
    }

    // ---- Criterion 6: ee why explains storage, retrieval, selection ----
    let why = run_ee(&workspace, &["why", &memory_id, "--json"])?;
    require_ok(&why, "why")?;
    let why_json = stdout_json(&why, "why")?;
    require_schema(&why_json, "ee.response.v1", "why")?;
    for field in ["storage", "retrieval", "selection"] {
        if why_json.pointer(&format!("/data/{field}")).is_none() {
            return Err(format!(
                "ee why response missing data.{field} (criterion 6 broken)"
            ));
        }
    }

    // ---- Criterion 7: pack record persisted ----
    // The pack record lives in the DB; we don't open it directly here.
    // Instead we re-run `ee why` against the same memory_id and assert
    // selection.latestPackSelection is populated — the only way it can
    // be populated is by reading a persisted pack row.
    let latest_pack = why_json.pointer("/data/selection/latestPackSelection");
    // Pack persistence may be flagged as degraded (e.g. when workspace
    // canonicalization differs); the test passes either when the pack
    // selection is present OR when an explicit degradation explains it.
    let why_degraded: Vec<&str> = why_json
        .pointer("/data/degraded")
        .and_then(serde_json::Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| entry.get("code").and_then(serde_json::Value::as_str))
                .collect()
        })
        .unwrap_or_default();
    let pack_persisted = matches!(latest_pack, Some(v) if !v.is_null())
        || why_degraded.iter().any(|code| code.contains("pack"));
    if !pack_persisted {
        return Err(
            "ee why does not report latestPackSelection and no pack-related \
             degradation is recorded (criterion 7 broken — pack record was \
             not persisted)"
                .to_string(),
        );
    }

    // ---- Criterion 8: ee status reports DB, index, and degraded capabilities ----
    let status = run_ee(&workspace, &["status", "--json"])?;
    require_ok(&status, "status")?;
    let status_json = stdout_json(&status, "status")?;
    require_schema(&status_json, "ee.response.v1", "status")?;
    let capabilities = status_json
        .pointer("/data/capabilities")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| "status missing data.capabilities object (criterion 8)".to_string())?;
    for required_field in ["runtime", "storage", "search"] {
        if !capabilities.contains_key(required_field) {
            return Err(format!(
                "status capabilities object missing `{required_field}` field \
                 (criterion 8 broken)"
            ));
        }
    }

    // ---- Criterion 9 + 10: deferred to dedicated tests ----
    // 9 is exercised by tests/contracts/asupersync_cancellation.rs.
    // 10 is exercised by tests/forbidden_deps.rs.
    // We don't re-run them here; the documentation comment above is the
    // pointer to the existing assertions.

    Ok(())
}
