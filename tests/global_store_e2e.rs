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
        trust_class: "agent_assertion".to_owned(),
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
                "mem_00000000000000000000000001",
                &global_memory_input(&workspace_id, "prefer X over Y across all repos"),
            )
            .expect("insert global memory");
    }

    // Invocation 2: a fresh open of the same on-disk store reads it back.
    let memories = read_global_store_memories(&paths, false).expect("read global store");
    assert_eq!(memories.len(), 1, "global memory persisted across opens");
    assert_eq!(memories[0].id, "mem_00000000000000000000000001");
    assert_eq!(memories[0].trust_class, "agent_assertion");
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

#[test]
fn global_memory_policy_errors_never_enable_promotion_or_retrieval() -> TestResult {
    let retained = tempfile::Builder::new()
        .prefix("ee-global-policy-")
        .tempdir()
        .map_err(|error| error.to_string())?
        .keep();
    let root = std::fs::canonicalize(retained).map_err(|error| error.to_string())?;
    let workspace = root.join("workspace");
    let data = root.join("data");
    std::fs::create_dir_all(&workspace).map_err(|error| error.to_string())?;
    std::fs::create_dir_all(&data).map_err(|error| error.to_string())?;
    eprintln!("retained global privacy policy fixture: {}", root.display());
    let run = |label: &str, args: &[&str]| -> Result<(i32, serde_json::Value), String> {
        let output = Command::new(env!("CARGO_BIN_EXE_ee"))
            .current_dir(&workspace)
            .arg("--workspace")
            .arg(&workspace)
            .arg("--json")
            .args(args)
            .env("XDG_DATA_HOME", &data)
            .env("XDG_CONFIG_HOME", root.join("config"))
            .env("XDG_CACHE_HOME", root.join("cache"))
            .env("EE_EMBED_DOWNLOAD", "off")
            .env_remove("EE_WORKSPACE")
            .env_remove("EE_WORKSPACE_REGISTRY")
            .env_remove("EE_DATABASE_PATH")
            .env_remove("EE_INDEX_DIR")
            .env_remove("EE_EMBED_MODEL_DIR")
            .env_remove("EE_EMBED_MODEL_PATH")
            .env_remove("FRANKENSEARCH_MODEL_DIR")
            .output()
            .map_err(|error| format!("{label}: {error}"))?;
        std::fs::write(root.join(format!("{label}.stdout.json")), &output.stdout)
            .map_err(|error| error.to_string())?;
        std::fs::write(root.join(format!("{label}.stderr")), &output.stderr)
            .map_err(|error| error.to_string())?;
        assert!(output.stderr.is_empty(), "{label}: {:?}", output);
        let value = serde_json::from_slice(&output.stdout)
            .map_err(|error| format!("{label}: {error}; output={output:?}"))?;
        Ok((output.status.code().ok_or("CLI killed by signal")?, value))
    };
    let (code, initialized) = run("init", &["init"])?;
    assert_eq!(code, 0, "{initialized}");
    let (code, local) = run(
        "remember-local",
        &[
            "remember",
            "Local promotion policy fixture.",
            "--level",
            "procedural",
            "--kind",
            "rule",
        ],
    )?;
    assert_eq!(code, 0, "{local}");
    let local_id = local["data"]["memory_id"]
        .as_str()
        .ok_or("missing local ID")?;
    let (code, global) = run(
        "remember-global",
        &[
            "remember",
            "Globalpolicysentinel applies across projects.",
            "--global",
            "--level",
            "procedural",
            "--kind",
            "rule",
        ],
    )?;
    assert_eq!(code, 0, "{global}");
    let global_id = global["data"]["memory_id"]
        .as_str()
        .ok_or("missing global ID")?;
    let (code, indexed) = run("index-local", &["index", "rebuild"])?;
    assert_eq!(code, 0, "{indexed}");

    let promotion = ["memory", "promote-global", local_id, "--dry-run"];
    let search = [
        "search",
        "Globalpolicysentinel",
        "--source-mode",
        "lexical-only",
    ];
    let pack = [
        "pack",
        "Local promotion policy fixture",
        "--source-mode",
        "lexical-only",
        "--read-only",
        "--max-tokens",
        "4000",
    ];
    let why_not = [
        "why-not",
        local_id,
        "--task",
        "Local promotion policy fixture",
        "--max-tokens",
        "4000",
    ];
    let check_local_retrieval = |label: &str| -> TestResult {
        let (code, packed) = run(&format!("{label}-pack"), &pack)?;
        assert_eq!(code, 0, "{packed}");
        assert_eq!(packed["success"], true, "{packed}");
        let item = packed
            .pointer("/data/pack/items")
            .and_then(serde_json::Value::as_array)
            .and_then(|items| items.iter().find(|item| item["memoryId"] == local_id))
            .ok_or_else(|| format!("local memory missing from pack: {packed}"))?;
        assert_eq!(item["content"], "Local promotion policy fixture.");
        assert!(
            item["why"]
                .as_str()
                .is_some_and(|why| !why.trim().is_empty()),
            "{item}"
        );
        let (code, explained) = run(&format!("{label}-why-not"), &why_not)?;
        assert_eq!(code, 0, "{explained}");
        assert_eq!(explained["success"], true, "{explained}");
        assert_eq!(explained["data"]["memoryId"], local_id);
        assert_eq!(explained["data"]["selected"], true, "{explained}");
        assert_eq!(explained["data"]["reasonSource"], "authoritative");
        Ok(())
    };
    let config = workspace.join(".ee/config.toml");
    if config.exists() {
        std::fs::rename(&config, root.join("initial-config.retained.toml"))
            .map_err(|error| error.to_string())?;
    }
    let (code, allowed) = run("missing-policy-promotion", &promotion)?;
    assert_eq!(code, 0, "{allowed}");
    assert_eq!(allowed["data"]["report"]["plan"]["verdict"], "allow");
    assert_eq!(allowed["data"]["report"]["executed"], false);
    let (code, found) = run("missing-policy-search", &search)?;
    assert_eq!(code, 0, "{found}");
    assert!(
        found["data"]["results"]
            .as_array()
            .ok_or("results missing")?
            .iter()
            .any(|hit| hit["memoryId"] == global_id),
        "{found}"
    );
    check_local_retrieval("missing-policy")?;

    std::fs::write(
        &config,
        "[memory]\ninclude_global = false\nparticipate = false\n",
    )
    .map_err(|error| error.to_string())?;
    let (code, denied) = run("disabled-policy-promotion", &promotion)?;
    assert_eq!(code, 7, "{denied}");
    assert_eq!(denied["error"]["code"], "policy_denied");
    assert_eq!(
        denied["error"]["details"]["plan"]["detail"]["code"],
        "global_lane_unavailable"
    );
    assert_eq!(denied["error"]["details"]["executed"], false);
    let (code, excluded) = run("disabled-policy-search", &search)?;
    assert_eq!(code, 0, "{excluded}");
    assert!(
        excluded["data"]["results"]
            .as_array()
            .ok_or("results missing")?
            .iter()
            .all(|hit| hit["memoryId"] != global_id),
        "{excluded}"
    );
    check_local_retrieval("disabled-policy")?;
    std::fs::rename(&config, root.join("disabled-config.retained.toml"))
        .map_err(|error| error.to_string())?;

    let check_refusal = |label: &str| -> TestResult {
        for (operation, args) in [
            ("promotion", promotion.as_slice()),
            ("search", search.as_slice()),
            ("pack", pack.as_slice()),
            ("why-not", why_not.as_slice()),
        ] {
            let (code, error) = run(&format!("{label}-{operation}"), args)?;
            assert_eq!(code, 2, "{error}");
            assert_eq!(error["schema"], "ee.error.v2");
            assert_eq!(error["error"]["code"], "configuration");
            assert!(
                error["error"]["message"]
                    .as_str()
                    .is_some_and(|message| message.contains("memory privacy policy")),
                "{error}"
            );
            assert!(
                error["error"]["repair"]
                    .as_str()
                    .is_some_and(|repair| repair.contains("config.toml")),
                "{error}"
            );
            assert!(error.get("data").is_none(), "{error}");
            assert!(!error.to_string().contains(local_id), "{error}");
            assert!(!error.to_string().contains(global_id), "{error}");
            assert!(
                !error.to_string().contains("private-policy-value"),
                "{error}"
            );
        }
        Ok(())
    };
    std::fs::write(&config, "[memory]\ninclude_global = false\nparticipate = false\ninvalid = [\"private-policy-value\"\n")
        .map_err(|error| error.to_string())?;
    check_refusal("malformed-policy")?;
    std::fs::rename(&config, root.join("malformed-config.retained.toml"))
        .map_err(|error| error.to_string())?;
    // A real bounded file read fails for invalid UTF-8 even when tests run as
    // root, unlike permission-bit fixtures that root can still read.
    std::fs::write(&config, [0xff, 0xfe]).map_err(|error| error.to_string())?;
    check_refusal("unreadable-policy")?;
    std::fs::rename(&config, root.join("unreadable-config.retained.toml"))
        .map_err(|error| error.to_string())?;
    std::fs::create_dir(&config).map_err(|error| error.to_string())?;
    check_refusal("nonfile-policy")?;
    #[cfg(unix)]
    {
        std::fs::rename(&config, root.join("nonfile-config.retained"))
            .map_err(|error| error.to_string())?;
        std::os::unix::fs::symlink(root.join("disabled-config.retained.toml"), &config)
            .map_err(|error| error.to_string())?;
        check_refusal("symlink-policy")?;
    }

    let paths = GlobalStorePaths::from_data_root(&data.join("ee"));
    let memories = read_global_store_memories(&paths, false)?;
    assert_eq!(
        memories.len(),
        1,
        "dry-run must never promote the local memory"
    );
    assert_eq!(memories[0].id, global_id);
    Ok(())
}

#[test]
fn global_migration_and_workspace_scope_read_failures_remain_distinct() -> TestResult {
    let root = tempfile::Builder::new()
        .prefix("ee-scope-read-boundaries-")
        .tempdir_in("/tmp")
        .map_err(|error| error.to_string())?
        .keep();
    let workspace = root.join("workspace");
    let data = root.join("data");
    let home = root.join("home");
    for path in [&workspace, &data, &home] {
        std::fs::create_dir_all(path).map_err(|error| error.to_string())?;
    }
    eprintln!("retained scope boundary workspace: {}", root.display());
    let run = |label: &str, args: &[&str]| -> Result<serde_json::Value, String> {
        let output = Command::new(env!("CARGO_BIN_EXE_ee"))
            .current_dir(&workspace)
            .args(args)
            .arg("--workspace")
            .arg(&workspace)
            .arg("--json")
            .env("HOME", &home)
            .env("XDG_DATA_HOME", &data)
            .env("XDG_CONFIG_HOME", home.join(".config"))
            .env("EE_EMBED_DOWNLOAD", "off")
            .env_remove("EE_WORKSPACE")
            .env_remove("EE_WORKSPACE_REGISTRY")
            .env_remove("EE_DATABASE_PATH")
            .env_remove("EE_INDEX_DIR")
            .env_remove("EE_EMBED_MODEL_DIR")
            .env_remove("EE_EMBED_MODEL_PATH")
            .env_remove("FRANKENSEARCH_MODEL_DIR")
            .output()
            .map_err(|error| error.to_string())?;
        std::fs::write(root.join(format!("{label}.stdout.json")), &output.stdout)
            .map_err(|error| error.to_string())?;
        std::fs::write(root.join(format!("{label}.stderr")), &output.stderr)
            .map_err(|error| error.to_string())?;
        let value = stdout_json(output, label)?;
        assert_eq!(value["schema"], "ee.response.v2", "{label}");
        assert_eq!(value["success"], true, "{label}");
        Ok(value)
    };
    let search = [
        "search",
        "Scoped metadata fixture",
        "--source-mode",
        "lexical-only",
    ];
    run("init", &["init"])?;
    let remembered = run(
        "remember",
        &[
            "remember",
            "Scoped metadata fixture.",
            "--level",
            "semantic",
            "--kind",
            "fact",
        ],
    )?;
    let memory_id = remembered["data"]["memoryId"]
        .as_str()
        .ok_or("remember must return a memory ID")?;
    run("index", &["index", "rebuild"])?;
    let baseline = run("baseline", &search)?;
    assert_eq!(baseline["data"]["resultCount"], 1);
    assert_eq!(baseline["data"]["results"][0]["memoryId"], memory_id);

    // A real uninitialized global database needs migration, but must not
    // make the independently verified workspace result look untrustworthy.
    let global = GlobalStorePaths::from_data_root(&data.join("ee"));
    std::fs::create_dir_all(&global.root).map_err(|error| error.to_string())?;
    let connection = ee::db::DbConnection::open_file(&global.database_path)
        .map_err(|error| error.to_string())?;
    connection
        .execute_raw("CREATE TABLE scope_migration_fixture (id INTEGER PRIMARY KEY)")
        .map_err(|error| error.to_string())?;
    assert!(
        connection
            .needs_migration()
            .map_err(|error| error.to_string())?
    );
    connection.close().map_err(|error| error.to_string())?;
    let pending = run("global-pending", &search)?;
    assert_eq!(pending["data"]["resultCount"], 1);
    assert_eq!(pending["data"]["results"][0]["memoryId"], memory_id);
    let codes = pending["degraded"]
        .as_array()
        .ok_or("degraded array missing")?;
    assert!(
        codes
            .iter()
            .all(|entry| entry["code"] != "scope_metadata_unavailable")
    );
    let migration = codes
        .iter()
        .filter(|entry| entry["code"] == "global_lane_migration_required")
        .collect::<Vec<_>>();
    assert_eq!(migration.len(), 1);
    assert_eq!(migration[0]["severity"], "info");
    let database = global
        .database_path
        .to_str()
        .ok_or("global path is not UTF-8")?;
    assert_eq!(
        migration[0]["repair"],
        format!("ee migrate run --database {database}")
    );
    let empty_repair = run("global-repair", &["migrate", "run", "--database", database])?;
    assert_eq!(
        empty_repair["data"]["postMigrationIndexRebuild"]["status"],
        "skipped_no_workspaces"
    );
    assert!(empty_repair["data"]["postMigrationIndexRebuild"]["auditId"].is_string());
    // GH #35: migrations land a batch of frames in the WAL sidecar, and the
    // automatic checkpoint threshold is a flat 64 MB that a small store never
    // reaches. Left there, every later connection open replays them.
    assert!(
        empty_repair["data"]["walCheckpoint"].is_string(),
        "migrate run must report what it did about the WAL: {:?}",
        empty_repair["data"]["walCheckpoint"]
    );
    let wal_path = global.database_path.with_extension("db-wal");
    let wal_bytes = std::fs::metadata(&wal_path).map_or(0, |meta| meta.len());
    let db_bytes = std::fs::metadata(&global.database_path)
        .map_err(|error| error.to_string())?
        .len();
    assert!(
        wal_bytes <= db_bytes,
        "migrate run left a WAL larger than the database it describes: \
         wal={wal_bytes} db={db_bytes}"
    );
    let connection = ee::db::DbConnection::open_file_read_only(&global.database_path)
        .map_err(|error| error.to_string())?;
    let audits = connection
        .list_audit_by_action(ee::db::audit_actions::MIGRATION_INDEX_REBUILD, None)
        .map_err(|error| error.to_string())?;
    assert_eq!(audits.len(), 1);
    assert_eq!(audits[0].workspace_id, None);
    connection.close().map_err(|error| error.to_string())?;
    let repaired = run("global-repaired", &search)?;
    assert_eq!(repaired["data"]["resultCount"], 1);
    assert_eq!(repaired["data"]["results"][0]["memoryId"], memory_id);
    assert!(
        repaired["degraded"]
            .as_array()
            .ok_or("degraded array missing")?
            .iter()
            .all(|entry| entry["code"] != "global_lane_migration_required"
                && entry["code"] != "scope_metadata_unavailable")
    );

    // Also migrate a populated historical store. The repair must target
    // global/indexes even though the invoking workspace has its own index.
    std::fs::rename(&global.root, root.join("migrated-empty-global"))
        .map_err(|error| error.to_string())?;
    std::fs::create_dir_all(&global.root).map_err(|error| error.to_string())?;
    let connection = ee::db::DbConnection::open_file(&global.database_path)
        .map_err(|error| error.to_string())?;
    connection
        .ensure_migration_table()
        .map_err(|error| error.to_string())?;
    for migration in ee::db::MIGRATIONS.iter().take(11) {
        connection
            .execute_raw(migration.sql())
            .map_err(|error| error.to_string())?;
        let record = ee::db::MigrationRecord::new(
            migration.version(),
            migration.name(),
            migration.checksum_label(),
            "2026-05-01T00:00:00Z",
        )
        .map_err(|error| error.to_string())?;
        connection
            .record_migration(&record)
            .map_err(|error| error.to_string())?;
    }
    let global_workspace = global_workspace_id(&global);
    let global_path = global
        .root
        .canonicalize()
        .map_err(|error| error.to_string())?
        .to_string_lossy()
        .replace('\'', "''");
    let global_memory = "mem_00000000000000000000000031";
    connection.execute_raw(&format!(
        "INSERT INTO workspaces (id, path, created_at, updated_at) VALUES ('{global_workspace}', '{global_path}', '2026-05-01T00:00:00Z', '2026-05-01T00:00:00Z')"
    )).map_err(|error| error.to_string())?;
    connection.execute_raw(&format!(
        "INSERT INTO memories (id, workspace_id, level, kind, content, confidence, utility, importance, created_at, updated_at, trust_class, trust_subclass) VALUES ('{global_memory}', '{global_workspace}', 'procedural', 'rule', 'Scoped metadata fixture from legacy global store.', 0.8, 0.7, 0.6, '2026-05-01T00:00:00Z', '2026-05-01T00:00:00Z', 'human_explicit', 'test')"
    )).map_err(|error| error.to_string())?;
    connection.close().map_err(|error| error.to_string())?;
    let legacy_pending = run("legacy-global-pending", &search)?;
    assert_eq!(legacy_pending["data"]["resultCount"], 1);
    assert_eq!(legacy_pending["data"]["results"][0]["memoryId"], memory_id);
    assert!(
        legacy_pending["degraded"]
            .as_array()
            .ok_or("degraded array missing")?
            .iter()
            .any(|entry| entry["code"] == "global_lane_migration_required"
                && entry["severity"] == "info")
    );
    let legacy_repair = run(
        "legacy-global-repair",
        &["migrate", "run", "--database", database],
    )?;
    let rebuild = &legacy_repair["data"]["postMigrationIndexRebuild"];
    assert_eq!(rebuild["status"], "success");
    assert_eq!(rebuild["indexDir"], global.index_dir.display().to_string());
    assert_eq!(rebuild["memoriesIndexed"], 1);
    let connection = ee::db::DbConnection::open_file_read_only(&global.database_path)
        .map_err(|error| error.to_string())?;
    let audits = connection
        .list_audit_by_action(ee::db::audit_actions::MIGRATION_INDEX_REBUILD, None)
        .map_err(|error| error.to_string())?;
    assert_eq!(audits.len(), 1);
    assert_eq!(
        audits[0].workspace_id.as_deref(),
        Some(global_workspace.as_str())
    );
    connection.close().map_err(|error| error.to_string())?;
    let legacy_repaired = run("legacy-global-repaired", &search)?;
    let results = legacy_repaired["data"]["results"]
        .as_array()
        .ok_or("results missing")?;
    assert_eq!(results.len(), 2);
    for id in [memory_id, global_memory] {
        assert!(results.iter().any(|result| result["memoryId"] == id));
    }
    assert!(
        legacy_repaired["degraded"]
            .as_array()
            .ok_or("degraded array missing")?
            .iter()
            .all(|entry| entry["code"] != "global_lane_migration_required"
                && entry["code"] != "scope_metadata_unavailable")
    );

    // Preserve the row and migration history while making its trust column
    // unavailable. An empty database fails earlier, before scope admission.
    let mut scoped_args = search.to_vec();
    scoped_args.extend(["--memory-scope", "verified"]);
    let verified = run("workspace-scope-baseline", &scoped_args)?;
    assert_eq!(verified["data"]["resultCount"], 1);
    assert_eq!(verified["data"]["results"][0]["memoryId"], memory_id);
    let connection = ee::db::DbConnection::open_file(&workspace.join(".ee/ee.db"))
        .map_err(|error| error.to_string())?;
    connection
        .execute_raw("ALTER TABLE memories RENAME COLUMN trust_class TO retained_trust_class_probe")
        .map_err(|error| error.to_string())?;
    connection.close().map_err(|error| error.to_string())?;
    let scoped = run("workspace-scope-unavailable", &scoped_args)?;
    assert_eq!(scoped["data"]["resultCount"], 0);
    assert_eq!(scoped["data"]["scopeStats"]["candidatesTotal"], 1);
    assert_eq!(scoped["data"]["scopeStats"]["candidatesExcludedByScope"], 1);
    assert_eq!(
        scoped["data"]["scopeStats"]["excludedMemoryIds"],
        serde_json::json!([memory_id])
    );
    let codes = scoped["degraded"]
        .as_array()
        .ok_or("degraded array missing")?;
    assert!(
        codes
            .iter()
            .all(|entry| entry["code"] != "global_lane_migration_required")
    );
    let scope = codes
        .iter()
        .filter(|entry| entry["code"] == "scope_metadata_unavailable")
        .collect::<Vec<_>>();
    assert_eq!(scope.len(), 1);
    assert_eq!(scope[0]["severity"], "medium");
    assert_eq!(scope[0]["repair"], "ee doctor --json");
    assert!(
        scope[0]["message"]
            .as_str()
            .ok_or("scope message missing")?
            .contains("trust_class")
    );
    Ok(())
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

/// GH #57: a persisting pack that selects a user-global memory must still be
/// recorded. The global memory has no row in the workspace database, so it
/// stays out of the workspace ledger (reported as
/// `context_pack_global_items_not_persisted`), while the workspace's own item
/// keeps its rank and is gradeable through `ee outcome --pack/--item`.
#[test]
fn persisting_pack_with_global_memory_records_workspace_ledger() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
    let workspace = tempdir.path().join("workspace");
    let xdg_data_home = tempdir.path().join("xdg-data");
    std::fs::create_dir_all(&workspace).map_err(|error| error.to_string())?;
    std::fs::create_dir_all(&xdg_data_home).map_err(|error| error.to_string())?;
    stdout_json(
        run_ee(&workspace, &xdg_data_home, &["init", "--json"])?,
        "ee init",
    )?;

    let global_id = remember_global(
        &workspace,
        &xdg_data_home,
        "Always run cargo fmt check before tagging a gh57 release.",
    )?;
    let local = stdout_json(
        run_ee(
            &workspace,
            &xdg_data_home,
            &[
                "remember",
                "Release checklist for gh57: tag only after the cargo fmt check passes.",
                "--level",
                "procedural",
                "--kind",
                "rule",
                "--json",
            ],
        )?,
        "ee remember (workspace)",
    )?;
    let local_id = local
        .pointer("/data/memory_id")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| format!("remember response missing memory_id: {local}"))?
        .to_owned();

    let pack = stdout_json(
        run_ee(
            &workspace,
            &xdg_data_home,
            &[
                "pack",
                "gh57 cargo fmt check before tagging a release",
                "--candidate-pool",
                "20",
                "--max-tokens",
                "1000",
                "--json",
            ],
        )?,
        "ee pack (persisting)",
    )?;
    let degraded_codes = pack
        .pointer("/data/degraded")
        .and_then(serde_json::Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter_map(|row| row.get("code").and_then(serde_json::Value::as_str))
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if degraded_codes
        .iter()
        .any(|code| code == "context_pack_persist_failed")
    {
        return Err(format!("persisting pack was not recorded: {pack}"));
    }
    let items = pack
        .pointer("/data/pack/items")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| format!("pack response missing items: {pack}"))?;
    let item_for = |id: &str| {
        items
            .iter()
            .find(|item| item.get("memoryId").and_then(serde_json::Value::as_str) == Some(id))
    };
    let global_item =
        item_for(&global_id).ok_or_else(|| format!("pack omitted global {global_id}: {pack}"))?;
    let local_item =
        item_for(&local_id).ok_or_else(|| format!("pack omitted local {local_id}: {pack}"))?;
    let global_notes = global_item
        .get("provenance")
        .and_then(serde_json::Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| entry.get("note").and_then(serde_json::Value::as_str))
                .collect::<Vec<_>>()
                .join(" | ")
        })
        .unwrap_or_default();
    if !global_notes.contains("lane=global") {
        return Err(format!(
            "global pack item must carry lane=global provenance, got notes: {global_notes}"
        ));
    }
    if !degraded_codes
        .iter()
        .any(|code| code == "context_pack_global_items_not_persisted")
    {
        return Err(format!(
            "pack must report that its global items are not in the workspace ledger: {pack}"
        ));
    }

    let hash = pack
        .pointer("/data/pack/hash")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| format!("pack response missing hash: {pack}"))?
        .to_owned();
    let rank_of = |item: &serde_json::Value| {
        item.get("rank")
            .and_then(serde_json::Value::as_u64)
            .map(|rank| rank.to_string())
            .ok_or_else(|| format!("pack item missing rank: {item}"))
    };
    let local_rank = rank_of(local_item)?;
    let global_rank = rank_of(global_item)?;

    let graded = stdout_json(
        run_ee(
            &workspace,
            &xdg_data_home,
            &[
                "outcome",
                "--pack",
                &hash,
                "--item",
                &local_rank,
                "--signal",
                "helpful",
                "--json",
            ],
        )?,
        "ee outcome --pack/--item on the workspace item",
    )?;
    let graded_text = graded.to_string();
    if !graded_text.contains(&local_id) {
        return Err(format!(
            "outcome --pack/--item did not resolve to workspace memory {local_id}: {graded}"
        ));
    }

    let global_grade = run_ee(
        &workspace,
        &xdg_data_home,
        &[
            "outcome",
            "--pack",
            &hash,
            "--item",
            &global_rank,
            "--signal",
            "helpful",
            "--json",
        ],
    )?;
    if global_grade.status.success() {
        return Err(format!(
            "outcome --pack/--item must not resolve the unledgered global rank {global_rank}: exit={:?} stdout={} stderr={}",
            global_grade.status.code(),
            String::from_utf8_lossy(&global_grade.stdout),
            String::from_utf8_lossy(&global_grade.stderr)
        ));
    }
    Ok(())
}
