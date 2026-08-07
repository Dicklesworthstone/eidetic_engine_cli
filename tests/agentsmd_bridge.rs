//! bd-39tzu.4 — golden + contract + behavior tests for the AGENTS.md
//! bridge surfaces (`ee export agentsmd`, `ee import agentsmd`,
//! `ee diag agentsmd-drift`).
//!
//! Mirrors tests/primer_cli_golden.rs: each test seeds a fresh workspace
//! DB with FIXED memory ids through the library API, then runs the real
//! binary, so output is byte-deterministic across machines — the bridge
//! payloads carry no wall-clock timestamps, no absolute paths, and no
//! binary version. Goldens live under tests/fixtures/golden/agentsmd/.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::process::{Command, Output};

use serde_json::Value;

type TestResult = Result<(), String>;

const FIXTURE_WORKSPACE_ID: &str = "wsp_00000000000000000000000072";

fn run_ee(args: &[&str]) -> Result<Output, String> {
    Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(args)
        .env_remove("EE_WORKSPACE")
        .output()
        .map_err(|error| format!("failed to run ee {args:?}: {error}"))
}

fn run_ee_json(args: &[&str]) -> Result<Value, String> {
    let output = run_ee(args)?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    if !output.status.success() {
        return Err(format!(
            "ee {args:?} failed: {stdout}\n{}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    serde_json::from_str(stdout.trim()).map_err(|error| format!("parse ee {args:?}: {error}"))
}

fn insert_rule_memory(
    connection: &ee::db::DbConnection,
    id: &str,
    level: &str,
    kind: &str,
    content: &str,
    confidence: f32,
) -> Result<(), String> {
    connection
        .insert_memory(
            id,
            &ee::db::CreateMemoryInput {
                workspace_id: FIXTURE_WORKSPACE_ID.to_owned(),
                level: level.to_owned(),
                kind: kind.to_owned(),
                content: content.to_owned(),
                workflow_id: None,
                confidence,
                utility: 0.8,
                importance: 0.7,
                provenance_uri: Some("test://agentsmd-bridge".to_owned()),
                trust_class: "human_explicit".to_owned(),
                trust_subclass: None,
                tags: vec!["agentsmd-bridge-test".to_owned()],
                valid_from: None,
                valid_to: None,
            },
        )
        .map_err(|error| format!("insert memory {id}: {error}"))
}

/// Workspace with one procedural rule, one episodic failure, and one
/// decision — the rule and failure render into the managed block.
fn seed_bridge_workspace() -> Result<tempfile::TempDir, String> {
    let temp = tempfile::tempdir().map_err(|error| format!("tempdir: {error}"))?;
    let db_dir = temp.path().join(".ee");
    std::fs::create_dir_all(&db_dir).map_err(|error| format!("mkdir .ee: {error}"))?;
    let connection = ee::db::DbConnection::open_file(&db_dir.join("ee.db"))
        .map_err(|error| format!("open db: {error}"))?;
    connection
        .migrate()
        .map_err(|error| format!("migrate: {error}"))?;
    connection
        .insert_workspace(
            FIXTURE_WORKSPACE_ID,
            &ee::db::CreateWorkspaceInput {
                path: temp.path().to_string_lossy().into_owned(),
                name: Some("agentsmd-bridge".to_owned()),
            },
        )
        .map_err(|error| format!("insert workspace: {error}"))?;
    insert_rule_memory(
        &connection,
        "mem_00000000000000000000000011",
        "procedural",
        "rule",
        "Always run the verify script before pushing changes to main.",
        0.9,
    )?;
    insert_rule_memory(
        &connection,
        "mem_00000000000000000000000012",
        "episodic",
        "failure",
        "Release broke when goldens were regenerated on the wrong host.",
        0.8,
    )?;
    insert_rule_memory(
        &connection,
        "mem_00000000000000000000000013",
        "semantic",
        "decision",
        "Keep the async runtime on asupersync; tokio is forbidden.",
        0.85,
    )?;
    connection
        .close()
        .map_err(|error| format!("close db: {error}"))?;
    Ok(temp)
}

fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("agentsmd")
        .join(name)
}

/// Self-contained golden compare/update (same UPDATE_GOLDEN contract as
/// tests/golden.rs, without pulling that file's test module in).
fn assert_agentsmd_golden(file_name: &str, actual: &str) -> TestResult {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("golden")
        .join("agentsmd")
        .join(file_name);
    if std::env::var("UPDATE_GOLDEN").is_ok() {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("create {}: {error}", parent.display()))?;
        }
        std::fs::write(&path, actual)
            .map_err(|error| format!("write {}: {error}", path.display()))?;
        return Ok(());
    }
    let expected = std::fs::read_to_string(&path).map_err(|error| {
        format!(
            "read {}: {error} (run with UPDATE_GOLDEN=1)",
            path.display()
        )
    })?;
    if expected == actual {
        Ok(())
    } else {
        Err(format!(
            "golden mismatch for {}.\n--- expected\n{expected}\n+++ actual\n{actual}",
            path.display()
        ))
    }
}

// ---------------------------------------------------------------------------
// Export
// ---------------------------------------------------------------------------

#[test]
fn export_missing_file_reports_file_missing_golden() -> TestResult {
    let workspace = seed_bridge_workspace()?;
    let workspace_arg = workspace.path().to_str().unwrap().to_owned();
    let output = run_ee(&[
        "export",
        "agentsmd",
        "--workspace",
        &workspace_arg,
        "--json",
    ])?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    if !output.status.success() {
        return Err(format!("export agentsmd failed: {stdout}"));
    }
    assert_agentsmd_golden(
        "export_file_missing.json.golden",
        &(stdout.trim().to_string() + "\n"),
    )?;
    if workspace.path().join("AGENTS.md").exists() {
        return Err("export without --create must not materialize the file".to_owned());
    }
    Ok(())
}

#[test]
fn export_create_matches_golden_and_reexport_is_byte_identical() -> TestResult {
    let workspace = seed_bridge_workspace()?;
    let workspace_arg = workspace.path().to_str().unwrap().to_owned();
    let response = run_ee_json(&[
        "export",
        "agentsmd",
        "--create",
        "--workspace",
        &workspace_arg,
        "--json",
    ])?;
    assert_agentsmd_golden("export_create.json.golden", &(response.to_string() + "\n"))?;
    if response.pointer("/data/created") != Some(&Value::Bool(true)) {
        return Err("first export with --create must report created=true".to_owned());
    }
    let file_path = workspace.path().join("AGENTS.md");
    let first_bytes =
        std::fs::read_to_string(&file_path).map_err(|error| format!("read AGENTS.md: {error}"))?;
    if !first_bytes.contains("<!-- ee:agentsmd:begin generation=") {
        return Err("managed block begin marker missing from created file".to_owned());
    }

    // Idempotency (ADR 0065 §5): unchanged memory ⇒ byte-identical block,
    // no write, no backup.
    let second = run_ee_json(&[
        "export",
        "agentsmd",
        "--workspace",
        &workspace_arg,
        "--json",
    ])?;
    if second.pointer("/data/status").and_then(Value::as_str) != Some("ok") {
        return Err(format!(
            "no-op re-export must succeed (not refuse), got {second}"
        ));
    }
    if second.pointer("/data/changed") != Some(&Value::Bool(false)) {
        return Err("no-op re-export must report changed=false".to_owned());
    }
    if second.pointer("/data/backupPath") != Some(&Value::Null) {
        return Err("no-op re-export must not write a backup".to_owned());
    }
    let second_bytes =
        std::fs::read_to_string(&file_path).map_err(|error| format!("re-read: {error}"))?;
    if first_bytes != second_bytes {
        return Err("re-export must leave the file byte-identical".to_owned());
    }
    if file_path.with_file_name("AGENTS.md.ee-backup").exists() {
        return Err("no-op re-export must not create a backup file".to_owned());
    }
    Ok(())
}

#[test]
fn export_appends_block_without_touching_existing_content() -> TestResult {
    let workspace = seed_bridge_workspace()?;
    let workspace_arg = workspace.path().to_str().unwrap().to_owned();
    let file_path = workspace.path().join("AGENTS.md");
    let hand_written = "# My AGENTS.md\n\nHand-written guidance stays untouched.\n";
    std::fs::write(&file_path, hand_written).map_err(|error| format!("seed file: {error}"))?;

    let response = run_ee_json(&[
        "export",
        "agentsmd",
        "--workspace",
        &workspace_arg,
        "--json",
    ])?;
    let degraded_codes: Vec<&str> = response
        .pointer("/data/degraded")
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| entry.pointer("/code").and_then(Value::as_str))
                .collect()
        })
        .unwrap_or_default();
    if !degraded_codes.contains(&"agentsmd_markers_missing") {
        return Err(format!(
            "first export into a markerless file must emit agentsmd_markers_missing, got {degraded_codes:?}"
        ));
    }
    let content = std::fs::read_to_string(&file_path).map_err(|error| format!("read: {error}"))?;
    if !content.starts_with(hand_written) {
        return Err("export must never edit outside its markers".to_owned());
    }
    if !content.contains("<!-- ee:agentsmd:end -->") {
        return Err("managed block must be appended".to_owned());
    }
    let backup = std::fs::read_to_string(file_path.with_file_name("AGENTS.md.ee-backup"))
        .map_err(|error| format!("backup must exist before first mutation: {error}"))?;
    if backup != hand_written {
        return Err("backup must hold the pre-mutation content".to_owned());
    }
    Ok(())
}

#[test]
fn export_refuses_hand_edit_then_force_preserves_edit_in_backup() -> TestResult {
    let workspace = seed_bridge_workspace()?;
    let workspace_arg = workspace.path().to_str().unwrap().to_owned();
    run_ee_json(&[
        "export",
        "agentsmd",
        "--create",
        "--workspace",
        &workspace_arg,
        "--json",
    ])?;
    let file_path = workspace.path().join("AGENTS.md");
    let exported = std::fs::read_to_string(&file_path).map_err(|error| format!("read: {error}"))?;
    let tampered = exported.replace(
        "<!-- ee:agentsmd:end -->",
        "sneaky hand edit inside the managed block\n<!-- ee:agentsmd:end -->",
    );
    if tampered == exported {
        return Err("tamper replacement did not apply".to_owned());
    }
    std::fs::write(&file_path, &tampered).map_err(|error| format!("tamper: {error}"))?;

    // Refusal path: no mutation, warning degraded code.
    let refused = run_ee_json(&[
        "export",
        "agentsmd",
        "--workspace",
        &workspace_arg,
        "--json",
    ])?;
    if refused.pointer("/data/status").and_then(Value::as_str) != Some("refused_unmanaged_edit") {
        return Err(format!("hand-edited block must refuse, got {refused}"));
    }
    let after_refusal =
        std::fs::read_to_string(&file_path).map_err(|error| format!("read: {error}"))?;
    if after_refusal != tampered {
        return Err("refused export must not touch the file".to_owned());
    }

    // Force path: hand edit preserved in the backup, block restored.
    let forced = run_ee_json(&[
        "export",
        "agentsmd",
        "--force-managed-block",
        "--workspace",
        &workspace_arg,
        "--json",
    ])?;
    if forced.pointer("/data/status").and_then(Value::as_str) != Some("ok") {
        return Err(format!("forced export must succeed, got {forced}"));
    }
    let backup = std::fs::read_to_string(file_path.with_file_name("AGENTS.md.ee-backup"))
        .map_err(|error| format!("backup: {error}"))?;
    if !backup.contains("sneaky hand edit inside the managed block") {
        return Err("forced export must preserve the hand edit in the backup".to_owned());
    }
    let restored = std::fs::read_to_string(&file_path).map_err(|error| format!("read: {error}"))?;
    if restored.contains("sneaky hand edit") {
        return Err("forced export must replace the hand-edited block".to_owned());
    }
    Ok(())
}

#[test]
fn export_dry_run_prints_diff_and_writes_nothing() -> TestResult {
    let workspace = seed_bridge_workspace()?;
    let workspace_arg = workspace.path().to_str().unwrap().to_owned();
    let response = run_ee_json(&[
        "export",
        "agentsmd",
        "--create",
        "--dry-run",
        "--workspace",
        &workspace_arg,
        "--json",
    ])?;
    let diff = response
        .pointer("/data/diff")
        .and_then(Value::as_str)
        .ok_or("dry-run must carry a diff")?;
    if !diff.contains("+ <!-- ee:agentsmd:begin generation=") {
        return Err(format!("diff must preview the managed block, got: {diff}"));
    }
    if workspace.path().join("AGENTS.md").exists() {
        return Err("dry run must not write the file".to_owned());
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Import
// ---------------------------------------------------------------------------

fn copy_fixture_into_workspace(workspace: &std::path::Path, fixture: &str) -> TestResult {
    std::fs::copy(fixture_path(fixture), workspace.join("AGENTS.md"))
        .map_err(|error| format!("copy fixture {fixture}: {error}"))?;
    Ok(())
}

#[test]
fn import_dry_run_matches_golden_and_schema() -> TestResult {
    let workspace = seed_bridge_workspace()?;
    let workspace_arg = workspace.path().to_str().unwrap().to_owned();
    copy_fixture_into_workspace(workspace.path(), "sample_agents.md")?;
    let response = run_ee_json(&[
        "import",
        "agentsmd",
        "--workspace",
        &workspace_arg,
        "--json",
    ])?;
    assert_agentsmd_golden("import_dry_run.json.golden", &(response.to_string() + "\n"))?;
    let payload = response.pointer("/data").ok_or("missing data payload")?;
    validate_import_against_schema(payload)?;

    // Precision expectations over the fixture: six statements admitted,
    // and the seeded identical rule dedups to a reinforce proposal.
    let proposals = payload
        .pointer("/proposals")
        .and_then(Value::as_array)
        .ok_or("proposals must be an array")?;
    if proposals.len() != 6 {
        return Err(format!("expected 6 proposals, got {}", proposals.len()));
    }
    let reinforce: Vec<&Value> = proposals
        .iter()
        .filter(|proposal| {
            proposal.pointer("/action").and_then(Value::as_str) == Some("reinforce_existing")
        })
        .collect();
    if reinforce.len() != 1 {
        return Err(format!(
            "exactly the seeded near-duplicate must reinforce, got {}",
            reinforce.len()
        ));
    }
    if reinforce[0]
        .pointer("/targetMemoryId")
        .and_then(Value::as_str)
        != Some("mem_00000000000000000000000011")
    {
        return Err("reinforce proposal must target the seeded rule memory".to_owned());
    }
    if payload.pointer("/applied") != Some(&Value::Null) {
        return Err("dry run must not apply".to_owned());
    }
    Ok(())
}

#[test]
fn import_apply_writes_candidates_and_reruns_abstain() -> TestResult {
    let workspace = seed_bridge_workspace()?;
    let workspace_arg = workspace.path().to_str().unwrap().to_owned();
    copy_fixture_into_workspace(workspace.path(), "sample_agents.md")?;
    let applied = run_ee_json(&[
        "import",
        "agentsmd",
        "--apply",
        "--workspace",
        &workspace_arg,
        "--json",
    ])?;
    let candidate_ids: Vec<String> = applied
        .pointer("/data/applied/candidateIds")
        .and_then(Value::as_array)
        .ok_or("apply must report candidateIds")?
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect();
    if candidate_ids.len() != 6 {
        return Err(format!(
            "expected 6 applied candidates, got {}",
            candidate_ids.len()
        ));
    }

    // The candidates exist as pending curation candidates.
    let connection = ee::db::DbConnection::open_file(&workspace.path().join(".ee").join("ee.db"))
        .map_err(|error| format!("open db: {error}"))?;
    for candidate_id in &candidate_ids {
        let row = connection
            .get_curation_candidate(FIXTURE_WORKSPACE_ID, candidate_id)
            .map_err(|error| format!("get candidate {candidate_id}: {error}"))?
            .ok_or_else(|| format!("candidate {candidate_id} missing after apply"))?;
        if row.source_id.as_deref() != Some("agentsmd_import") {
            return Err(format!(
                "candidate {candidate_id} must carry source_id agentsmd_import"
            ));
        }
    }
    connection
        .close()
        .map_err(|error| format!("close db: {error}"))?;

    // Idempotency: a second apply proposes nothing and abstains per line.
    let second = run_ee_json(&[
        "import",
        "agentsmd",
        "--apply",
        "--workspace",
        &workspace_arg,
        "--json",
    ])?;
    let abstentions = second
        .pointer("/data/abstentions")
        .and_then(Value::as_array)
        .ok_or("abstentions must be an array")?;
    if abstentions.len() != 6 {
        return Err(format!(
            "re-apply must abstain on all six statements, got {}",
            abstentions.len()
        ));
    }
    if !abstentions.iter().all(|abstention| {
        abstention.pointer("/reason").and_then(Value::as_str) == Some("already_imported")
    }) {
        return Err("every re-apply abstention must be already_imported".to_owned());
    }
    let second_applied = second
        .pointer("/data/applied/candidateIds")
        .and_then(Value::as_array)
        .ok_or("re-apply must still report applied")?;
    if !second_applied.is_empty() {
        return Err("re-apply must not double-insert candidates".to_owned());
    }
    Ok(())
}

#[test]
fn import_excludes_managed_block_content() -> TestResult {
    let workspace = seed_bridge_workspace()?;
    let workspace_arg = workspace.path().to_str().unwrap().to_owned();
    run_ee_json(&[
        "export",
        "agentsmd",
        "--create",
        "--workspace",
        &workspace_arg,
        "--json",
    ])?;
    let response = run_ee_json(&[
        "import",
        "agentsmd",
        "--workspace",
        &workspace_arg,
        "--json",
    ])?;
    if response.pointer("/data/managedBlockExcluded") != Some(&Value::Bool(true)) {
        return Err("import must exclude the managed block".to_owned());
    }
    let proposals = response
        .pointer("/data/proposals")
        .and_then(Value::as_array)
        .ok_or("proposals must be an array")?;
    if !proposals.is_empty() {
        return Err(format!(
            "the bridge must never re-import its own export, got {proposals:?}"
        ));
    }
    Ok(())
}

#[test]
fn repo_agents_snapshot_teaches_memory_only_advisory_preflight() -> TestResult {
    let snapshot = include_str!("fixtures/agentsmd/repo_agents_snapshot.md");

    for required in [
        "### Command-Risk Memory Is Advisory Only",
        "`ee` is a memory substrate, not a shell policy-enforcement layer.",
        "Never install `ee` as a command-denial hook",
        "Never translate an `ee` risk-memory match into `permissionDecision: \"deny\"`",
        "A syntactically valid `ee preflight check` stays exit-zero",
        "Harness hooks managed by `ee` are limited to memory-oriented recall",
        "must fail open.",
    ] {
        if !snapshot.contains(required) {
            return Err(format!(
                "repo AGENTS snapshot is missing advisory command-risk contract: {required:?}"
            ));
        }
    }

    for forbidden in [
        "### Wiring Trauma-Guard Into Agent Hooks",
        "Exit code `7` means policy denied",
        "| 7 | policy denied operation |",
        "--override-token",
        "Agent harnesses should call it before running shell commands",
    ] {
        if snapshot.contains(forbidden) {
            return Err(format!(
                "repo AGENTS snapshot still teaches command authority: {forbidden:?}"
            ));
        }
    }

    Ok(())
}

#[test]
fn import_parses_repo_agents_snapshot_with_precision() -> TestResult {
    let workspace = seed_bridge_workspace()?;
    let workspace_arg = workspace.path().to_str().unwrap().to_owned();
    copy_fixture_into_workspace(workspace.path(), "repo_agents_snapshot.md")?;
    let response = run_ee_json(&[
        "import",
        "agentsmd",
        "--workspace",
        &workspace_arg,
        "--json",
    ])?;
    let proposals = response
        .pointer("/data/proposals")
        .and_then(Value::as_array)
        .ok_or("proposals must be an array")?;
    if proposals.is_empty() {
        return Err("the repo's own AGENTS.md must yield rule statements".to_owned());
    }
    let mut imports_no_denial_hook_rule = false;
    let mut imports_no_permission_denial_rule = false;
    // Precision spot checks: nothing structural leaks through the parser.
    for proposal in proposals {
        let text = proposal
            .pointer("/contentDraft")
            .and_then(Value::as_str)
            .ok_or("proposal missing contentDraft")?;
        for forbidden in ["#", "|", "<!--", "```"] {
            if text.starts_with(forbidden) {
                return Err(format!("structural line leaked through parser: {text}"));
            }
        }
        let kind = proposal.pointer("/kind").and_then(Value::as_str);
        if kind != Some("rule") && kind != Some("convention") {
            return Err(format!("unexpected kind {kind:?}"));
        }
        let modality = proposal.pointer("/modality").and_then(Value::as_str);
        if text.starts_with("Never install `ee` as a command-denial hook") {
            imports_no_denial_hook_rule = modality == Some("Never");
        }
        if text.starts_with("Never translate an `ee` risk-memory match")
            && text.contains("permissionDecision: \"deny\"")
        {
            imports_no_permission_denial_rule = modality == Some("Never");
        }
        for forbidden in [
            "Exit code `7` means policy denied",
            "policy denied operation",
            "--override-token",
        ] {
            if text.contains(forbidden) {
                return Err(format!(
                    "AGENTS.md import proposed obsolete execution-authority teaching: {text}"
                ));
            }
        }
    }
    if !imports_no_denial_hook_rule || !imports_no_permission_denial_rule {
        return Err(format!(
            "AGENTS.md import must preserve explicit negative command-authority rules; no_denial_hook={imports_no_denial_hook_rule}, no_permission_denial={imports_no_permission_denial_rule}"
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Drift
// ---------------------------------------------------------------------------

#[test]
fn drift_reports_stale_block_contradiction_and_missing_rule() -> TestResult {
    let workspace = seed_bridge_workspace()?;
    let workspace_arg = workspace.path().to_str().unwrap().to_owned();

    // Second high-confidence rule that never appears in the file.
    let connection = ee::db::DbConnection::open_file(&workspace.path().join(".ee").join("ee.db"))
        .map_err(|error| format!("open db: {error}"))?;
    insert_rule_memory(
        &connection,
        "mem_00000000000000000000000014",
        "procedural",
        "rule",
        "You MUST sign release tags with the project signing key.",
        0.9,
    )?;
    connection
        .close()
        .map_err(|error| format!("close db: {error}"))?;

    // Hand-built file: a stale managed block (generation=-1 is behind any
    // live generation) with a valid hash, plus a hand-written statement
    // contradicting the seeded "Always run the verify script..." rule.
    let body = "managed body placeholder line\n";
    let block = ee::core::agentsmd::render_managed_block(body, -1);
    let content = format!(
        "# AGENTS.md\n\n- Never run the verify script before pushing changes to main.\n\n{block}\n"
    );
    std::fs::write(workspace.path().join("AGENTS.md"), &content)
        .map_err(|error| format!("seed file: {error}"))?;

    let response = run_ee_json(&[
        "diag",
        "agentsmd-drift",
        "--workspace",
        &workspace_arg,
        "--json",
    ])?;
    let payload = response.pointer("/data").ok_or("missing data payload")?;

    if payload.pointer("/managedBlock/stale") != Some(&Value::Bool(true)) {
        return Err(format!("generation=-1 block must be stale, got {payload}"));
    }
    if payload.pointer("/managedBlock/hashMatches") != Some(&Value::Bool(true)) {
        return Err("untampered block must report hashMatches=true".to_owned());
    }

    let contradictions = payload
        .pointer("/contradictions")
        .and_then(Value::as_array)
        .ok_or("contradictions must be an array")?;
    let found = contradictions.iter().any(|finding| {
        finding.pointer("/memoryId").and_then(Value::as_str)
            == Some("mem_00000000000000000000000011")
            && finding.pointer("/filePolarity").and_then(Value::as_str) == Some("negative")
            && finding.pointer("/memoryPolarity").and_then(Value::as_str) == Some("positive")
            && finding.pointer("/signal").and_then(Value::as_str) == Some("contradiction_link")
    });
    if !found {
        return Err(format!(
            "Never-vs-Always pair must surface as contradiction_link, got {contradictions:?}"
        ));
    }

    let missing = payload
        .pointer("/missingRules")
        .and_then(Value::as_array)
        .ok_or("missingRules must be an array")?;
    if !missing.iter().any(|finding| {
        finding.pointer("/memoryId").and_then(Value::as_str)
            == Some("mem_00000000000000000000000014")
    }) {
        return Err(format!(
            "the unexported signing rule must be reported missing, got {missing:?}"
        ));
    }

    let suggested = payload
        .pointer("/suggestedCommands")
        .and_then(Value::as_array)
        .ok_or("suggestedCommands must be an array")?;
    if suggested.is_empty() {
        return Err("drift findings must carry suggested commands".to_owned());
    }
    Ok(())
}

#[test]
fn drift_missing_file_is_honest_and_read_only() -> TestResult {
    let workspace = seed_bridge_workspace()?;
    let workspace_arg = workspace.path().to_str().unwrap().to_owned();
    let response = run_ee_json(&[
        "diag",
        "agentsmd-drift",
        "--workspace",
        &workspace_arg,
        "--json",
    ])?;
    if response.pointer("/data/status").and_then(Value::as_str) != Some("file_missing") {
        return Err(format!(
            "missing file must report file_missing, got {response}"
        ));
    }
    if workspace.path().join("AGENTS.md").exists() {
        return Err("drift is read-only and must not create files".to_owned());
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Schema contract (structural, mirrors primer_cli_golden.rs)
// ---------------------------------------------------------------------------

fn load_schema(name: &str) -> Result<Value, String> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("docs")
        .join("schemas")
        .join(name);
    let bytes =
        std::fs::read(&path).map_err(|error| format!("read {}: {error}", path.display()))?;
    serde_json::from_slice(&bytes).map_err(|error| format!("parse schema {name}: {error}"))
}

fn string_set(value: &Value, pointer: &str) -> Result<Vec<String>, String> {
    Ok(value
        .pointer(pointer)
        .and_then(Value::as_array)
        .ok_or_else(|| format!("schema missing {pointer}"))?
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect())
}

/// Minimal structural validator covering the constructs the import schema
/// uses (required sets, const, enums); the repo has no jsonschema engine.
fn validate_import_against_schema(payload: &Value) -> TestResult {
    let schema = load_schema("ee.agentsmd.import.v1.json")?;
    for field in string_set(&schema, "/required")? {
        if payload.get(&field).is_none() {
            return Err(format!("payload missing required field {field}"));
        }
    }
    if payload.pointer("/schema").and_then(Value::as_str) != Some("ee.agentsmd.import.v1") {
        return Err("payload schema must be ee.agentsmd.import.v1".to_owned());
    }
    let action_enum = string_set(
        &schema,
        "/properties/proposals/items/properties/action/enum",
    )?;
    let kind_enum = string_set(&schema, "/properties/proposals/items/properties/kind/enum")?;
    for proposal in payload
        .pointer("/proposals")
        .and_then(Value::as_array)
        .ok_or("proposals must be an array")?
    {
        for field in string_set(&schema, "/properties/proposals/items/required")? {
            if proposal.get(&field).is_none() {
                return Err(format!("proposal missing required field {field}"));
            }
        }
        let action = proposal
            .pointer("/action")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if !action_enum.iter().any(|allowed| allowed == action) {
            return Err(format!("action {action:?} not in schema enum"));
        }
        let kind = proposal
            .pointer("/kind")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if !kind_enum.iter().any(|allowed| allowed == kind) {
            return Err(format!("kind {kind:?} not in schema enum"));
        }
        let evidence = proposal
            .pointer("/evidence")
            .and_then(Value::as_array)
            .ok_or("evidence must be an array")?;
        for uri in evidence {
            let uri = uri.as_str().unwrap_or_default();
            if !uri.starts_with("file://") || !uri.contains("#L") {
                return Err(format!("evidence uri {uri:?} must be file://<path>#L<n>"));
            }
        }
    }
    Ok(())
}

#[test]
fn export_payload_validates_against_schema() -> TestResult {
    let workspace = seed_bridge_workspace()?;
    let workspace_arg = workspace.path().to_str().unwrap().to_owned();
    let response = run_ee_json(&[
        "export",
        "agentsmd",
        "--create",
        "--workspace",
        &workspace_arg,
        "--json",
    ])?;
    let payload = response.pointer("/data").ok_or("missing data payload")?;
    let schema = load_schema("ee.agentsmd.export.v1.json")?;
    for field in string_set(&schema, "/required")? {
        if payload.get(&field).is_none() {
            return Err(format!("payload missing required field {field}"));
        }
    }
    let status_enum = string_set(&schema, "/properties/status/enum")?;
    let status = payload
        .pointer("/status")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !status_enum.iter().any(|allowed| allowed == status) {
        return Err(format!("status {status:?} not in schema enum"));
    }
    let block_hash = payload
        .pointer("/blockHash")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !block_hash.starts_with("blake3:") || block_hash.len() != "blake3:".len() + 16 {
        return Err(format!("blockHash {block_hash:?} must be blake3:<16 hex>"));
    }
    Ok(())
}

#[test]
fn drift_payload_validates_against_schema() -> TestResult {
    let workspace = seed_bridge_workspace()?;
    let workspace_arg = workspace.path().to_str().unwrap().to_owned();
    std::fs::write(
        workspace.path().join("AGENTS.md"),
        "# AGENTS.md\n\n- Always run the verify script before pushing changes to main.\n",
    )
    .map_err(|error| format!("seed file: {error}"))?;
    let response = run_ee_json(&[
        "diag",
        "agentsmd-drift",
        "--workspace",
        &workspace_arg,
        "--json",
    ])?;
    let payload = response.pointer("/data").ok_or("missing data payload")?;
    let schema = load_schema("ee.agentsmd.drift.v1.json")?;
    for field in string_set(&schema, "/required")? {
        if payload.get(&field).is_none() {
            return Err(format!("payload missing required field {field}"));
        }
    }
    let code_enum = string_set(&schema, "/properties/degraded/items/properties/code/enum")?;
    for entry in payload
        .pointer("/degraded")
        .and_then(Value::as_array)
        .ok_or("degraded must be an array")?
    {
        let code = entry.pointer("/code").and_then(Value::as_str).unwrap_or("");
        if !code_enum.iter().any(|allowed| allowed == code) {
            return Err(format!("degraded code {code:?} not in schema enum"));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Human-format goldens (both-formats parity with primer_cli_golden.rs)
// ---------------------------------------------------------------------------

#[test]
fn export_create_human_summary_matches_golden() -> TestResult {
    let workspace = seed_bridge_workspace()?;
    let workspace_arg = workspace.path().to_str().unwrap().to_owned();
    let output = run_ee(&[
        "export",
        "agentsmd",
        "--create",
        "--workspace",
        &workspace_arg,
    ])?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    if !output.status.success() {
        return Err(format!("export agentsmd (human) failed: {stdout}"));
    }
    assert_agentsmd_golden("export_create.human.golden", &stdout)
}

#[test]
fn import_dry_run_human_summary_matches_golden() -> TestResult {
    let workspace = seed_bridge_workspace()?;
    let workspace_arg = workspace.path().to_str().unwrap().to_owned();
    copy_fixture_into_workspace(workspace.path(), "sample_agents.md")?;
    let output = run_ee(&["import", "agentsmd", "--workspace", &workspace_arg])?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    if !output.status.success() {
        return Err(format!("import agentsmd (human) failed: {stdout}"));
    }
    assert_agentsmd_golden("import_dry_run.human.golden", &stdout)
}
