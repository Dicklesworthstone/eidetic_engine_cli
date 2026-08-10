//! E2E: the user-global memory tier (bd-1pq3c, ADR 0083) is a REAL separate
//! on-disk store, not policy-only.
//!
//! Proves that `open_or_create_global_store` + `read_global_store_memories`
//! persist a memory and read it back across **independent** connections
//! (simulating separate `ee` process invocations against
//! `~/.local/share/ee/global`), and that two different store roots are
//! isolated — the core contract a `remember --global` write / global-tier read
//! depends on.

#![allow(clippy::unwrap_used, clippy::expect_used)] // test code may unwrap/expect
use ee::core::global_store::{
    GlobalStorePaths, global_workspace_id, open_or_create_global_store, read_global_store_memories,
};
use ee::db::CreateMemoryInput;
use std::path::Path;
use std::process::{Command, Output};

type TestResult = Result<(), String>;

fn global_memory_input(workspace_id: &str, content: &str) -> CreateMemoryInput {
    CreateMemoryInput {
        workspace_id: workspace_id.to_owned(),
        level: "semantic".to_owned(),
        kind: "rule".to_owned(),
        content: content.to_owned(),
        workflow_id: None,
        confidence: 0.95,
        utility: 0.0,
        importance: 0.0,
        provenance_uri: None,
        trust_class: "self".to_owned(),
        trust_subclass: None,
        tags: vec!["global".to_owned()],
        valid_from: None,
        valid_to: None,
    }
}

#[test]
fn global_store_persists_across_independent_opens_and_isolates_roots() {
    let tempdir = tempfile::tempdir().expect("tempdir");
    let paths = GlobalStorePaths::from_root(&tempdir.path().join("global"));

    // A read before any write returns empty (the store does not exist yet),
    // so a global-tier read needs no separate pre-existence check.
    assert!(
        read_global_store_memories(&paths, false)
            .expect("read empty global store")
            .is_empty()
    );

    // Invocation 1: create the separate store and write a user-global memory.
    {
        let (connection, workspace_id) =
            open_or_create_global_store(&paths).expect("create global store");
        assert!(paths.database_path.exists(), "global ee.db materialized");
        assert_eq!(workspace_id, global_workspace_id(&paths));
        connection
            .insert_memory(
                "mem_e2e_global0000000000000000001",
                &global_memory_input(&workspace_id, "prefer X over Y across all repos"),
            )
            .expect("insert global memory");
    }

    // Invocation 2: a fresh open of the same on-disk store reads it back.
    let memories = read_global_store_memories(&paths, false).expect("read global store");
    assert_eq!(memories.len(), 1, "global memory persisted across opens");
    assert_eq!(memories[0].content, "prefer X over Y across all repos");

    // Re-opening is idempotent: the stable global workspace id is unchanged.
    let (_again, workspace_id_again) =
        open_or_create_global_store(&paths).expect("reopen global store");
    assert_eq!(workspace_id_again, global_workspace_id(&paths));

    // Separate-store isolation: a different root is its own empty store.
    let other = GlobalStorePaths::from_root(&tempdir.path().join("other-global"));
    assert!(
        read_global_store_memories(&other, false)
            .expect("read isolated store")
            .is_empty(),
        "separate global store roots are isolated"
    );
}

fn run_ee(workspace: &Path, xdg_data_home: &Path, args: &[&str]) -> Result<Output, String> {
    Command::new(env!("CARGO_BIN_EXE_ee"))
        .arg("--workspace")
        .arg(workspace)
        .args(args)
        .env("XDG_DATA_HOME", xdg_data_home)
        .env("HOME", xdg_data_home.join("home"))
        .env_remove("EE_WORKSPACE")
        .env_remove("EE_WORKSPACE_REGISTRY")
        .output()
        .map_err(|error| format!("failed to run ee {}: {error}", args.join(" ")))
}

fn stdout_json(output: Output, context: &str) -> Result<serde_json::Value, String> {
    if !output.status.success() {
        return Err(format!(
            "{context} failed: exit={:?} stderr={}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let stdout = String::from_utf8(output.stdout)
        .map_err(|error| format!("{context}: stdout was not UTF-8: {error}"))?;
    serde_json::from_str(&stdout)
        .map_err(|error| format!("{context}: stdout was not JSON: {error}\nstdout: {stdout}"))
}

fn pack_item_memory_ids(envelope: &serde_json::Value) -> Vec<String> {
    envelope
        .pointer("/data/pack/items")
        .and_then(serde_json::Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    item.pointer("/memoryId")
                        .and_then(serde_json::Value::as_str)
                })
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn global_cross_wire_validation_precedes_bootstrap_and_keyed_replay() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
    let workspace = tempdir.path().join("workspace");
    let xdg_data_home = tempdir.path().join("xdg-data");
    std::fs::create_dir_all(&workspace).map_err(|error| error.to_string())?;
    std::fs::create_dir_all(&xdg_data_home).map_err(|error| error.to_string())?;
    let global_paths = GlobalStorePaths::from_data_root(&xdg_data_home.join("ee"));

    let invalid = run_ee(
        &workspace,
        &xdg_data_home,
        &[
            "remember",
            "--global",
            "Global keyed replay must validate first.",
            "--kind",
            "episodic",
            "--idempotency-key",
            "global-cross-wire",
            "--json",
        ],
    )?;
    if invalid.status.code() != Some(1) {
        return Err(format!(
            "initial global cross-wire exit={:?}, stdout={}, stderr={}",
            invalid.status.code(),
            String::from_utf8_lossy(&invalid.stdout),
            String::from_utf8_lossy(&invalid.stderr)
        ));
    }
    let invalid_json: serde_json::Value = serde_json::from_slice(&invalid.stdout)
        .map_err(|error| format!("initial global cross-wire stdout was not JSON: {error}"))?;
    if invalid_json
        .pointer("/error/code")
        .and_then(serde_json::Value::as_str)
        != Some("remember_kind_is_level")
    {
        return Err(format!(
            "initial global cross-wire returned wrong envelope: {invalid_json}"
        ));
    }
    if global_paths.root.exists() {
        return Err(format!(
            "invalid global remember bootstrapped {}",
            global_paths.root.display()
        ));
    }

    stdout_json(
        run_ee(&workspace, &xdg_data_home, &["init", "--json"])?,
        "ee init",
    )?;
    let content = "Global keyed replay must validate first.";
    stdout_json(
        run_ee(
            &workspace,
            &xdg_data_home,
            &[
                "remember",
                "--global",
                content,
                "--kind",
                "fact",
                "--idempotency-key",
                "global-cross-wire",
                "--json",
            ],
        )?,
        "valid keyed global remember",
    )?;

    let replay = run_ee(
        &workspace,
        &xdg_data_home,
        &[
            "remember",
            "--global",
            content,
            "--kind",
            "semantic",
            "--idempotency-key",
            "global-cross-wire",
            "--json",
        ],
    )?;
    if replay.status.code() != Some(1) {
        return Err(format!(
            "cross-wired global replay exit={:?}, stdout={}, stderr={}",
            replay.status.code(),
            String::from_utf8_lossy(&replay.stdout),
            String::from_utf8_lossy(&replay.stderr)
        ));
    }
    let replay_json: serde_json::Value = serde_json::from_slice(&replay.stdout)
        .map_err(|error| format!("global replay stdout was not JSON: {error}"))?;
    if replay_json
        .pointer("/error/code")
        .and_then(serde_json::Value::as_str)
        != Some("remember_kind_is_level")
    {
        return Err(format!(
            "cross-wired global replay returned wrong envelope: {replay_json}"
        ));
    }
    let memories = read_global_store_memories(&global_paths, false)
        .map_err(|error| format!("read global store after cross-wired replay: {error}"))?;
    if memories.len() != 1 {
        return Err(format!(
            "cross-wired global replay mutated row count: {}",
            memories.len()
        ));
    }

    Ok(())
}

#[test]
fn remember_global_then_pack_reads_from_global_store() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
    let workspace = tempdir.path().join("workspace");
    let xdg_data_home = tempdir.path().join("xdg-data");
    std::fs::create_dir_all(&workspace).map_err(|error| error.to_string())?;
    std::fs::create_dir_all(&xdg_data_home).map_err(|error| error.to_string())?;

    stdout_json(
        run_ee(&workspace, &xdg_data_home, &["init", "--json"])?,
        "ee init",
    )?;
    let remembered = stdout_json(
        run_ee(
            &workspace,
            &xdg_data_home,
            &[
                "remember",
                "--global",
                "Always include the bd-29xmb global caller regression rule in packs.",
                "--level",
                "procedural",
                "--kind",
                "rule",
                "--json",
            ],
        )?,
        "ee remember --global",
    )?;
    let memory_id = remembered
        .pointer("/data/memory_id")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| format!("remember response missing memory_id: {remembered}"))?
        .to_owned();

    let global_paths = GlobalStorePaths::from_data_root(&xdg_data_home.join("ee"));
    let global_memories = read_global_store_memories(&global_paths, false)
        .map_err(|error| format!("read global store after CLI remember: {error}"))?;
    if !global_memories.iter().any(|memory| memory.id == memory_id) {
        return Err(format!(
            "remember --global did not persist {memory_id} in {}",
            global_paths.database_path.display()
        ));
    }

    let pack = stdout_json(
        run_ee(
            &workspace,
            &xdg_data_home,
            &[
                "pack",
                "bd-29xmb global caller regression",
                "--read-only",
                "--candidate-pool",
                "20",
                "--max-tokens",
                "1000",
                "--json",
            ],
        )?,
        "ee pack",
    )?;
    let item_ids = pack_item_memory_ids(&pack);
    if !item_ids.iter().any(|id| id == &memory_id) {
        return Err(format!(
            "pack did not include global memory {memory_id}; item ids: {item_ids:?}; envelope: {pack}"
        ));
    }

    Ok(())
}

fn remember_global(
    workspace: &Path,
    xdg_data_home: &Path,
    content: &str,
) -> Result<String, String> {
    let envelope = stdout_json(
        run_ee(
            workspace,
            xdg_data_home,
            &[
                "remember", "--global", content, "--level", "semantic", "--kind", "rule", "--json",
            ],
        )?,
        "ee remember --global",
    )?;
    envelope
        .pointer("/data/memory_id")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| format!("remember response missing memory_id: {envelope}"))
}

/// GH#23: the memory curation verbs (`list`, `show`, `expire`, `revise`,
/// `history`, ...) must reach the user-global store via `--global`, from any
/// workspace — previously the store was write-only through the normal verbs
/// (`remember --global` worked, but list/expire/revise could not resolve the
/// global workspace and reported "memory not found" / 0 memories).
#[test]
fn memory_curation_verbs_reach_global_store() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
    let workspace_a = tempdir.path().join("workspace-a");
    let workspace_b = tempdir.path().join("workspace-b");
    let xdg_data_home = tempdir.path().join("xdg-data");
    for dir in [&workspace_a, &workspace_b, &xdg_data_home] {
        std::fs::create_dir_all(dir).map_err(|error| error.to_string())?;
    }

    stdout_json(
        run_ee(&workspace_a, &xdg_data_home, &["init", "--json"])?,
        "ee init",
    )?;
    let expire_target = remember_global(
        &workspace_a,
        &xdg_data_home,
        "GH-23 expire target: global curation must reach this memory.",
    )?;
    let revise_target = remember_global(
        &workspace_a,
        &xdg_data_home,
        "GH-23 revise target: global curation must reach this memory.",
    )?;

    // `--global` list reaches the global store even from an unrelated,
    // never-initialized workspace.
    let list = stdout_json(
        run_ee(
            &workspace_b,
            &xdg_data_home,
            &["memory", "list", "--global", "--json"],
        )?,
        "ee memory list --global",
    )?;
    let listed_ids: Vec<String> = list
        .pointer("/data/memories")
        .and_then(serde_json::Value::as_array)
        .map(|memories| {
            memories
                .iter()
                .filter_map(|memory| {
                    memory
                        .pointer("/id")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned)
                })
                .collect()
        })
        .unwrap_or_default();
    for id in [&expire_target, &revise_target] {
        if !listed_ids.iter().any(|listed| listed == id) {
            return Err(format!(
                "memory list --global did not return {id}; listed ids: {listed_ids:?}; envelope: {list}"
            ));
        }
    }

    // The workspace lane stays isolated: a plain workspace list must NOT
    // contain the global memories ...
    let workspace_list = stdout_json(
        run_ee(&workspace_a, &xdg_data_home, &["memory", "list", "--json"])?,
        "ee memory list (workspace lane)",
    )?;
    let workspace_listed = workspace_list.to_string();
    if workspace_listed.contains(&expire_target) || workspace_listed.contains(&revise_target) {
        return Err(format!(
            "workspace memory list leaked global memories: {workspace_list}"
        ));
    }

    // ... and a workspace-scoped expire still cannot reach a global memory
    // (the workspace-id guard is intact).
    let guarded = run_ee(
        &workspace_a,
        &xdg_data_home,
        &["memory", "expire", &expire_target, "--json"],
    )?;
    if guarded.status.success() {
        return Err(format!(
            "workspace-scoped expire unexpectedly reached global memory {expire_target}: {}",
            String::from_utf8_lossy(&guarded.stdout)
        ));
    }

    // `show --global` resolves the memory.
    let shown = stdout_json(
        run_ee(
            &workspace_b,
            &xdg_data_home,
            &["memory", "show", &expire_target, "--global", "--json"],
        )?,
        "ee memory show --global",
    )?;
    if !shown.to_string().contains(&expire_target) {
        return Err(format!(
            "memory show --global did not return {expire_target}: {shown}"
        ));
    }

    // `revise --global` writes an immutable revision into the global store.
    let revised = stdout_json(
        run_ee(
            &workspace_b,
            &xdg_data_home,
            &[
                "memory",
                "revise",
                &revise_target,
                "--content",
                "GH-23 revise target: revised in the global store.",
                "--global",
                "--json",
            ],
        )?,
        "ee memory revise --global",
    )?;
    let revised_id = revised
        .pointer("/data/new_id")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| format!("memory revise --global returned no new_id: {revised}"))?
        .to_owned();
    let global_paths = GlobalStorePaths::from_data_root(&xdg_data_home.join("ee"));
    let global_memories = read_global_store_memories(&global_paths, true)
        .map_err(|error| format!("read global store after revise: {error}"))?;
    if !global_memories.iter().any(|memory| memory.id == revised_id) {
        return Err(format!(
            "revision {revised_id} did not land in the global store {}",
            global_paths.database_path.display()
        ));
    }

    // `expire --global` tombstone-expires the global memory.
    let expired = stdout_json(
        run_ee(
            &workspace_b,
            &xdg_data_home,
            &[
                "memory",
                "expire",
                &expire_target,
                "--global",
                "--reason",
                "GH-23 e2e curation",
                "--json",
            ],
        )?,
        "ee memory expire --global",
    )?;
    let status = expired
        .pointer("/data/status")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    if status != "expired" {
        return Err(format!(
            "memory expire --global reported status {status:?}, expected \"expired\": {expired}"
        ));
    }

    // `history --global` reads the audit trail of a global memory.
    stdout_json(
        run_ee(
            &workspace_b,
            &xdg_data_home,
            &["memory", "history", &expire_target, "--global", "--json"],
        )?,
        "ee memory history --global",
    )?;

    Ok(())
}
