//! `ee workspace rebind` through the real binary (bd-3s7pq).
//!
//! The relocation surface arrived with 3b8c751d7. Its only test drove the CLI
//! in process from `src/`, which the vision-coverage gate does not count as an
//! invocation, so CI Static reported `workspace rebind` as documented but never
//! exercised. This test spawns the built `ee`, with HOME and XDG isolated so
//! host state cannot change the verdict, and checks the user-visible contract:
//! a preview writes nothing, applying the previewed plan keeps the workspace and
//! memory identity, normal reads resolve the moved store, a used plan cannot
//! replay, and a wrong expected source path is refused.

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use ee::db::{CreateMemoryInput, CreateWorkspaceInput, DbConnection, WorkspaceScopeFields};
use ee::policy::store_auth::StoreAuthRoot;

type TestResult = Result<(), String>;

const WORKSPACE_ID: &str = "wsp_00000000000000000000000391";
const MEMORY_ID: &str = "mem_00000000000000000000000391";
const BODY: &str = "Keep the source signing identity when relocating release memory.";
// src/policy/store_auth.rs KEY_FILE_NAME, which is crate-private.
const KEY_FILE: &str = "store_auth_root.json";

fn run_ee(home: &Path, args: &[&str]) -> Result<Output, String> {
    Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(args)
        .env("HOME", home)
        .env("XDG_DATA_HOME", home.join("data"))
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("XDG_CACHE_HOME", home.join("cache"))
        .env("EE_EMBED_DOWNLOAD", "off")
        .output()
        .map_err(|error| format!("failed to run ee {}: {error}", args.join(" ")))
}

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn json_stdout(output: &Output, context: &str) -> Result<serde_json::Value, String> {
    let stdout = String::from_utf8_lossy(&output.stdout);
    ensure(
        output.status.success(),
        format!(
            "{context}: exit {:?}, stdout {stdout}, stderr {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    ensure(
        output.stderr.is_empty(),
        format!("{context}: JSON stderr must stay clean"),
    )?;
    serde_json::from_str(&stdout).map_err(|error| format!("{context}: stdout is not JSON: {error}"))
}

/// A store written at `original`, with its own store-auth key, then moved to
/// `moved`: the situation `workspace rebind` exists to recover from.
fn moved_store(original: &Path, moved: &Path) -> TestResult {
    fs::create_dir_all(original.join(".ee")).map_err(|error| error.to_string())?;
    let db = DbConnection::open_file(original.join(".ee/ee.db")).map_err(|e| e.to_string())?;
    db.migrate().map_err(|e| e.to_string())?;
    db.upsert_workspace_with_scope(
        WORKSPACE_ID,
        &CreateWorkspaceInput {
            path: original.to_string_lossy().into_owned(),
            name: Some("relocation-e2e".to_owned()),
        },
        &WorkspaceScopeFields::standalone(),
    )
    .map_err(|e| e.to_string())?;
    db.insert_memory(
        MEMORY_ID,
        &CreateMemoryInput {
            workspace_id: WORKSPACE_ID.to_owned(),
            level: "semantic".to_owned(),
            kind: "fact".to_owned(),
            content: BODY.to_owned(),
            workflow_id: None,
            confidence: 0.9,
            utility: 0.7,
            importance: 0.6,
            provenance_uri: Some("manual://source-relocation".to_owned()),
            trust_class: "human_explicit".to_owned(),
            trust_subclass: None,
            tags: vec!["release".to_owned()],
            valid_from: None,
            valid_to: None,
        },
    )
    .map_err(|e| e.to_string())?;
    db.close().map_err(|e| e.to_string())?;
    StoreAuthRoot::create(original.join(".ee/keys")).map_err(|e| e.to_string())?;
    fs::rename(original, moved).map_err(|e| e.to_string())
}

#[test]
fn workspace_rebind_binary_previews_applies_and_refuses_replay() -> TestResult {
    let temporary = tempfile::tempdir().map_err(|e| e.to_string())?;
    let root = temporary.path().canonicalize().map_err(|e| e.to_string())?;
    let home = root.join("home");
    fs::create_dir_all(&home).map_err(|e| e.to_string())?;
    let original = root.join("original");
    let moved = root.join("moved");
    moved_store(&original, &moved)?;

    let database = moved.join(".ee/ee.db");
    let key = moved.join(".ee/keys").join(KEY_FILE);
    let database_before = fs::read(&database).map_err(|e| e.to_string())?;
    let key_before = fs::read(&key).map_err(|e| e.to_string())?;
    let moved_text = moved.to_string_lossy().into_owned();
    let source_text = original.to_string_lossy().into_owned();
    let keys_text = moved.join(".ee/keys").to_string_lossy().into_owned();

    // Preview: a plan, and no byte of the store or its key changes.
    let preview = [
        "--json",
        "--workspace",
        &moved_text,
        "workspace",
        "rebind",
        "--expected-workspace-id",
        WORKSPACE_ID,
        "--expected-source-path",
        &source_text,
        "--source-keys-dir",
        &keys_text,
    ];
    let planned = json_stdout(&run_ee(&home, &preview)?, "rebind preview")?;
    ensure(
        planned["data"]["schema"] == "ee.workspace.rebind.v1",
        format!("preview schema: {}", planned["data"]["schema"]),
    )?;
    ensure(planned["data"]["status"] == "preview", "preview status")?;
    ensure(
        planned["data"]["persisted"] == false,
        "a preview persists nothing",
    )?;
    ensure(
        fs::read(&database).map_err(|e| e.to_string())? == database_before,
        "a preview must not write the store",
    )?;
    let plan = planned["data"]["plan"]["planHash"]
        .as_str()
        .ok_or("preview must commit to a plan hash")?
        .to_owned();

    // Apply exactly the previewed plan.
    let mut apply = preview.to_vec();
    apply.extend(["--apply-plan", plan.as_str()]);
    let applied = json_stdout(&run_ee(&home, &apply)?, "rebind apply")?;
    ensure(applied["data"]["status"] == "rebound", "apply status")?;
    ensure(applied["data"]["persisted"] == true, "apply persists")?;
    ensure(
        applied["data"]["workspaceIdPreserved"] == true,
        "the workspace identity is preserved, not re-keyed",
    )?;
    ensure(
        fs::read(&key).map_err(|e| e.to_string())? == key_before,
        "the source signing key is kept byte for byte",
    )?;

    // A normal read through the binary resolves the moved store and the same memory.
    let listed = json_stdout(
        &run_ee(
            &home,
            &["--json", "--workspace", &moved_text, "memory", "list"],
        )?,
        "memory list after rebind",
    )?;
    ensure(
        listed.to_string().contains(MEMORY_ID),
        "memory list must resolve the preserved memory identity",
    )?;

    // The plan is single-use: it cannot replay against the new binding.
    let replay = run_ee(&home, &apply)?;
    ensure(
        !replay.status.success(),
        "a used relocation plan must not replay",
    )
}

#[test]
fn workspace_rebind_binary_refuses_a_wrong_expected_source() -> TestResult {
    let temporary = tempfile::tempdir().map_err(|e| e.to_string())?;
    let root = temporary.path().canonicalize().map_err(|e| e.to_string())?;
    let home = root.join("home");
    fs::create_dir_all(&home).map_err(|e| e.to_string())?;
    let original = root.join("original");
    let moved = root.join("moved");
    moved_store(&original, &moved)?;
    let database_before = fs::read(moved.join(".ee/ee.db")).map_err(|e| e.to_string())?;

    let moved_text = moved.to_string_lossy().into_owned();
    let wrong_source = root.join("somewhere-else").to_string_lossy().into_owned();
    let keys_text = moved.join(".ee/keys").to_string_lossy().into_owned();
    let refused = run_ee(
        &home,
        &[
            "--json",
            "--workspace",
            &moved_text,
            "workspace",
            "rebind",
            "--expected-workspace-id",
            WORKSPACE_ID,
            "--expected-source-path",
            &wrong_source,
            "--source-keys-dir",
            &keys_text,
        ],
    )?;
    ensure(
        !refused.status.success(),
        format!(
            "a rebind naming the wrong source must be refused; stdout {}",
            String::from_utf8_lossy(&refused.stdout)
        ),
    )?;
    ensure(
        fs::read(moved.join(".ee/ee.db")).map_err(|e| e.to_string())? == database_before,
        "a refused rebind must not write the store",
    )
}
