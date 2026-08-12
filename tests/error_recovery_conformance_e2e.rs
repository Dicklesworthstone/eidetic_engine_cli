use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};

use ee::core::workspace::stable_workspace_id;
use ee::db::{CreateMemoryInput, CreateWorkspaceInput, DbConnection};

type TestResult = Result<(), String>;

#[derive(Clone, Debug)]
struct ConformanceCase {
    id: &'static str,
    surface: &'static str,
    args: Vec<String>,
}

fn run_ee(args: &[String]) -> Result<Output, String> {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ee"));
    command.env(
        "EE_WORKSPACE_REGISTRY",
        std::env::temp_dir().join(format!(
            "ee-error-recovery-no-registry-{}.db",
            std::process::id()
        )),
    );
    command
        .args(args)
        .output()
        .map_err(|error| format!("failed to run ee {}: {error}", args.join(" ")))
}

fn run_ee_with_registry(args: &[String], registry: &Path) -> Result<Output, String> {
    Command::new(env!("CARGO_BIN_EXE_ee"))
        .env("EE_WORKSPACE_REGISTRY", registry)
        .args(args)
        .output()
        .map_err(|error| format!("failed to run ee {}: {error}", args.join(" ")))
}

fn run_ee_with_stdin(args: &[String], input: &[u8]) -> Result<Output, String> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_ee"))
        .env(
            "EE_WORKSPACE_REGISTRY",
            std::env::temp_dir().join(format!(
                "ee-error-recovery-stdin-registry-{}.db",
                std::process::id()
            )),
        )
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("failed to spawn ee {}: {error}", args.join(" ")))?;
    child
        .stdin
        .take()
        .ok_or("ee stdin pipe was unavailable")?
        .write_all(input)
        .map_err(|error| format!("failed to write ee stdin: {error}"))?;
    child
        .wait_with_output()
        .map_err(|error| format!("failed to collect ee {}: {error}", args.join(" ")))
}

fn run_emitted_ee_command_with_registry(command: &str, registry: &Path) -> Result<Output, String> {
    if !command.starts_with("ee ") {
        return Err(format!("emitted command must start with ee: {command:?}"));
    }
    let binary_dir = Path::new(env!("CARGO_BIN_EXE_ee"))
        .parent()
        .ok_or("compiled ee binary has no parent directory")?;
    let mut path_entries = vec![binary_dir.to_path_buf()];
    if let Some(existing) = std::env::var_os("PATH") {
        path_entries.extend(std::env::split_paths(&existing));
    }
    let shell_path = std::env::join_paths(path_entries)
        .map_err(|error| format!("failed to construct shell PATH: {error}"))?;
    Command::new("/bin/sh")
        .arg("-c")
        .arg(command)
        .env("PATH", shell_path)
        .env("EE_WORKSPACE_REGISTRY", registry)
        .output()
        .map_err(|error| format!("failed to execute emitted command through /bin/sh: {error}"))
}

fn sqlite_sidecar_path(database: &Path, suffix: &str) -> std::path::PathBuf {
    let mut path = database.as_os_str().to_os_string();
    path.push(suffix);
    path.into()
}

fn worker_materialized_path(path: &Path) -> Result<std::path::PathBuf, String> {
    let mut cursor = path;
    let mut missing = Vec::new();
    while !cursor.exists() {
        let component = cursor.file_name().ok_or_else(|| {
            format!(
                "cannot resolve a materialized path ancestor for {}",
                path.display()
            )
        })?;
        missing.push(component.to_os_string());
        cursor = cursor.parent().ok_or_else(|| {
            format!(
                "cannot resolve a materialized path parent for {}",
                path.display()
            )
        })?;
    }
    let mut resolved = cursor
        .canonicalize()
        .map_err(|error| format!("canonicalize {}: {error}", cursor.display()))?;
    for component in missing.iter().rev() {
        resolved.push(component);
    }
    Ok(resolved)
}

#[derive(Debug, Eq, PartialEq)]
struct FileSnapshot {
    bytes: Vec<u8>,
    modified: std::time::SystemTime,
    readonly: bool,
}

fn snapshot_file(path: &Path) -> Result<Option<FileSnapshot>, String> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("failed to snapshot {}: {error}", path.display())),
    };
    let metadata = std::fs::metadata(path)
        .map_err(|error| format!("failed to stat snapshot {}: {error}", path.display()))?;
    Ok(Some(FileSnapshot {
        bytes,
        modified: metadata
            .modified()
            .map_err(|error| format!("failed to read mtime for {}: {error}", path.display()))?,
        readonly: metadata.permissions().readonly(),
    }))
}

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn log_event(case: &ConformanceCase, event: &str, data: serde_json::Value) {
    eprintln!(
        "{}",
        serde_json::json!({
            "schema": "ee.test_event.v1",
            "suite": "error_recovery_conformance_e2e",
            "test": "storage_database_not_found_recovery_contract",
            "requirementId": case.id,
            "surface": case.surface,
            "event": event,
            "data": data,
        })
    );
}

fn stdout_json(output: &Output, label: &str) -> Result<serde_json::Value, String> {
    let stdout = String::from_utf8(output.stdout.clone())
        .map_err(|error| format!("{label}: stdout was not UTF-8: {error}"))?;
    serde_json::from_str(&stdout)
        .map_err(|error| format!("{label}: stdout was not JSON: {error}\n{stdout}"))
}

fn string_at<'a>(
    value: &'a serde_json::Value,
    pointer: &str,
    context: &str,
) -> Result<&'a str, String> {
    value
        .pointer(pointer)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| format!("{context}: missing string at {pointer}"))
}

fn array_at<'a>(
    value: &'a serde_json::Value,
    pointer: &str,
    context: &str,
) -> Result<&'a Vec<serde_json::Value>, String> {
    value
        .pointer(pointer)
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| format!("{context}: missing array at {pointer}"))
}

fn normalize_storeless_snapshot_text(
    text: &str,
    addressed_store: &str,
    storeless_workspace: &str,
    nearby_workspace: &str,
) -> String {
    let mut normalized = text
        .replace(addressed_store, "<ADDRESSED_STORE>")
        .replace(storeless_workspace, "<STORELESS_WORKSPACE>")
        .replace(nearby_workspace, "<NEARBY_WORKSPACE>");

    if let Ok(canonical_storeless) = Path::new(storeless_workspace).canonicalize() {
        normalized = normalized
            .replace(
                canonical_storeless
                    .join(".ee")
                    .join("ee.db")
                    .to_string_lossy()
                    .as_ref(),
                "<ADDRESSED_STORE>",
            )
            .replace(
                canonical_storeless.to_string_lossy().as_ref(),
                "<STORELESS_WORKSPACE>",
            );
    }
    if let Ok(canonical_nearby) = Path::new(nearby_workspace).canonicalize() {
        normalized = normalized.replace(
            canonical_nearby.to_string_lossy().as_ref(),
            "<NEARBY_WORKSPACE>",
        );
    }
    normalized
}

fn storage_error_cases(workspace: &str) -> Vec<ConformanceCase> {
    vec![
        ConformanceCase {
            id: "ERR-RECOVERY-STORAGE-001",
            surface: "context",
            args: vec![
                "--workspace".to_owned(),
                workspace.to_owned(),
                "pack".to_owned(),
                "missing database recovery conformance".to_owned(),
                "--json".to_owned(),
            ],
        },
        ConformanceCase {
            id: "ERR-RECOVERY-STORAGE-002",
            surface: "subscribe poll",
            args: vec![
                "--workspace".to_owned(),
                workspace.to_owned(),
                "subscribe".to_owned(),
                "poll".to_owned(),
                "--cursor".to_owned(),
                "0".to_owned(),
                "--json".to_owned(),
            ],
        },
        ConformanceCase {
            id: "ERR-RECOVERY-STORAGE-003",
            surface: "remember",
            args: vec![
                "--workspace".to_owned(),
                workspace.to_owned(),
                "remember".to_owned(),
                "storeless workspace conformance fact".to_owned(),
                "--json".to_owned(),
            ],
        },
        ConformanceCase {
            id: "ERR-RECOVERY-STORAGE-004",
            surface: "search",
            args: vec![
                "--workspace".to_owned(),
                workspace.to_owned(),
                "search".to_owned(),
                "storeless workspace conformance".to_owned(),
                "--json".to_owned(),
            ],
        },
    ]
}

fn assert_storage_recovery_contract(case: &ConformanceCase, output: &Output) -> TestResult {
    // bd-workspace-miss-init-suggestion-sfjvq: a storeless addressing miss
    // carries its own stable identity and exit code, distinct from ordinary
    // storage failures (which stay exit 3 / code "storage").
    ensure(
        output.status.code() == Some(10),
        format!(
            "{}: expected workspace-store-missing exit code 10, got {:?}\nstdout:\n{}\nstderr:\n{}",
            case.id,
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    ensure(
        output.stderr.is_empty(),
        format!("{}: JSON errors must not write stderr", case.id),
    )?;

    let json = stdout_json(output, case.id)?;
    ensure(
        string_at(&json, "/schema", case.id)? == "ee.error.v2",
        format!("{}: error envelope schema must be ee.error.v2", case.id),
    )?;
    ensure(
        string_at(&json, "/error/code", case.id)? == "workspace_store_missing",
        format!("{}: code must be workspace_store_missing", case.id),
    )?;
    ensure(
        string_at(&json, "/error/severity", case.id)? == "high",
        format!("{}: workspace-miss errors must be high severity", case.id),
    )?;
    ensure(
        string_at(&json, "/error/message", case.id)?
            .to_ascii_lowercase()
            .contains("database not found"),
        format!("{}: message must identify missing database", case.id),
    )?;

    let recovery = array_at(&json, "/error/details/recovery", case.id)?;
    ensure(
        recovery.len() == 3,
        format!(
            "{}: expected exactly 3 recovery actions, got {}",
            case.id,
            recovery.len()
        ),
    )?;
    ensure(
        recovery
            .windows(2)
            .all(|window| window[0]["priority"].as_u64() < window[1]["priority"].as_u64()),
        format!("{}: recovery priorities must be strictly ordered", case.id),
    )?;

    ensure(
        recovery[0]["priority"] == serde_json::json!(1)
            && recovery[0]["kind"] == serde_json::json!("flag")
            && recovery[0]["flagName"] == serde_json::json!("--workspace")
            && recovery[0]["valueHint"] == serde_json::json!("<path>"),
        format!(
            "{}: first recovery action must correct workspace addressing",
            case.id
        ),
    )?;
    ensure(
        recovery[1]["priority"] == serde_json::json!(2)
            && recovery[1]["kind"] == serde_json::json!("env")
            && recovery[1]["envName"] == serde_json::json!("EE_DATABASE_PATH"),
        format!(
            "{}: second recovery action must expose EE_DATABASE_PATH",
            case.id
        ),
    )?;
    ensure(
        recovery[2]["priority"] == serde_json::json!(3)
            && recovery[2]["kind"] == serde_json::json!("seed")
            && recovery[2]["command"] == serde_json::json!("ee init --workspace ."),
        format!(
            "{}: ee init must be the final conditional recovery action",
            case.id
        ),
    )?;

    let repair = string_at(&json, "/error/repair", case.id)?;
    ensure(
        repair.contains("Re-check --workspace addressing (looked for"),
        format!(
            "{}: freetext repair must lead with addressing and the looked-for path, got: {repair}",
            case.id
        ),
    )?;
    let recheck_at = repair.find("Re-check --workspace addressing");
    let init_at = repair.find("ee init --workspace .");
    ensure(
        matches!((recheck_at, init_at), (Some(recheck), Some(init)) if recheck < init)
            && repair.contains("Only if you intended to create a NEW store here"),
        format!(
            "{}: ee init must appear last and conditionally framed in the repair, got: {repair}",
            case.id
        ),
    )?;

    log_event(
        case,
        "pass",
        serde_json::json!({
            "schema": json["schema"],
            "code": json["error"]["code"],
            "severity": json["error"]["severity"],
            "recoveryCount": recovery.len(),
        }),
    );
    Ok(())
}

#[test]
fn storage_database_not_found_recovery_contract() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
    let workspace = tempdir.path().to_string_lossy().to_string();
    let cases = storage_error_cases(&workspace);

    ensure(
        cases.len() == 4,
        "coverage matrix should exercise four independent storage-backed surfaces",
    )?;

    for case in &cases {
        log_event(
            case,
            "run",
            serde_json::json!({
                "requirementLevel": "MUST",
                "workspaceInitialized": false,
            }),
        );
        let output = run_ee(&case.args)?;
        assert_storage_recovery_contract(case, &output)?;
    }

    Ok(())
}

/// Remember and search must preflight the exact addressed database before
/// any create-capable open. Cover both an entirely nonexistent workspace and
/// a pre-existing `.ee/` directory whose database is absent; neither miss may
/// create directories, a database, or SQLite sidecars.
#[test]
fn remember_and_search_storeless_address_variants_do_not_create_state() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
    let nonexistent_workspace = tempdir.path().join("never-created-workspace-sfjvq");
    let empty_ee_workspace = tempdir.path().join("existing-empty-ee-sfjvq");
    let empty_ee_dir = empty_ee_workspace.join(".ee");
    std::fs::create_dir_all(&empty_ee_dir).map_err(|error| error.to_string())?;
    let invalid_database_workspace = tempdir.path().join("directory-shaped-db-sfjvq");
    let invalid_database = invalid_database_workspace.join(".ee").join("ee.db");
    std::fs::create_dir_all(&invalid_database).map_err(|error| error.to_string())?;

    for workspace in [&nonexistent_workspace, &empty_ee_workspace] {
        let workspace_text = workspace.to_string_lossy().to_string();
        let expected_database = worker_materialized_path(&workspace.join(".ee").join("ee.db"))?;
        for (surface, args) in [
            (
                "remember",
                vec![
                    "--workspace".to_owned(),
                    workspace_text.clone(),
                    "remember".to_owned(),
                    "storeless preflight must not create state".to_owned(),
                    "--json".to_owned(),
                ],
            ),
            (
                "search",
                vec![
                    "--workspace".to_owned(),
                    workspace_text.clone(),
                    "search".to_owned(),
                    "storeless preflight".to_owned(),
                    "--json".to_owned(),
                ],
            ),
            (
                "search family",
                vec![
                    "--workspace".to_owned(),
                    workspace_text.clone(),
                    "search".to_owned(),
                    "--family".to_owned(),
                    "storeless-family-sfjvq".to_owned(),
                    "--json".to_owned(),
                ],
            ),
            (
                "search all workspaces",
                vec![
                    "--workspace".to_owned(),
                    workspace_text.clone(),
                    "search".to_owned(),
                    "storeless fanout".to_owned(),
                    "--all-workspaces".to_owned(),
                    "--json".to_owned(),
                ],
            ),
        ] {
            let output = run_ee(&args)?;
            ensure(
                output.status.code() == Some(10),
                format!(
                    "{surface} against {} must exit 10, got {:?}: stdout={} stderr={}",
                    workspace.display(),
                    output.status.code(),
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr),
                ),
            )?;
            let json = stdout_json(&output, surface)?;
            ensure(
                string_at(&json, "/error/code", surface)? == "workspace_store_missing",
                format!("{surface}: missing store must retain the canonical error code"),
            )?;
            ensure(
                string_at(&json, "/error/message", surface)?
                    == format!("Database not found at {}", expected_database.display()),
                format!(
                    "{surface}: error must name the exact absolute database path {}",
                    expected_database.display()
                ),
            )?;
            ensure(
                string_at(&json, "/error/details/addressedStorePath", surface)?
                    == expected_database.to_string_lossy(),
                format!(
                    "{surface}: structured addressedStorePath must equal the worker-materialized path {}",
                    expected_database.display()
                ),
            )?;
        }

        let batch = run_ee_with_stdin(
            &[
                "--workspace".to_owned(),
                workspace_text.clone(),
                "remember".to_owned(),
                "--batch".to_owned(),
                "--stdin".to_owned(),
                "--json".to_owned(),
            ],
            br#"{"content":"storeless batch preflight must not create state"}
"#,
        )?;
        ensure(
            batch.status.code() == Some(10),
            format!(
                "remember batch against {} must preserve workspace-store-missing exit 10, got {:?}: stdout={} stderr={}",
                workspace.display(),
                batch.status.code(),
                String::from_utf8_lossy(&batch.stdout),
                String::from_utf8_lossy(&batch.stderr),
            ),
        )?;
        let batch_json = stdout_json(&batch, "remember batch")?;
        ensure(
            string_at(&batch_json, "/error/code", "remember batch")? == "workspace_store_missing",
            "remember batch must not collapse a storeless address into an import error".to_owned(),
        )?;
    }

    let custom_database = tempdir.path().join("custom-store-sfjvq.db");
    let custom_database_text = custom_database.to_string_lossy().to_string();
    let custom_search = run_ee(&[
        "--workspace".to_owned(),
        empty_ee_workspace.to_string_lossy().to_string(),
        "search".to_owned(),
        "custom addressed database".to_owned(),
        "--database".to_owned(),
        custom_database_text.clone(),
        "--json".to_owned(),
    ])?;
    ensure(
        custom_search.status.code() == Some(10),
        format!(
            "a missing explicit search database must exit 10, got {:?}",
            custom_search.status.code()
        ),
    )?;
    let custom_search_json = stdout_json(&custom_search, "custom search database")?;
    let custom_database_materialized = worker_materialized_path(&custom_database)?;
    ensure(
        string_at(
            &custom_search_json,
            "/error/message",
            "custom search database",
        )? == format!(
            "Database not found at {}",
            custom_database_materialized.display()
        ),
        "a custom search miss must print the exact absolute database path".to_owned(),
    )?;
    ensure(
        string_at(
            &custom_search_json,
            "/error/details/addressedStorePath",
            "custom search database",
        )? == custom_database_materialized.to_string_lossy(),
        "custom search structured path must match the exact checked database".to_owned(),
    )?;
    ensure(
        !custom_database.exists(),
        "a custom search miss must not create the addressed database".to_owned(),
    )?;

    let invalid_workspace_text = invalid_database_workspace.to_string_lossy().to_string();
    for (surface, args) in [
        (
            "remember invalid database",
            vec![
                "--workspace".to_owned(),
                invalid_workspace_text.clone(),
                "remember".to_owned(),
                "present invalid stores are not addressing misses".to_owned(),
                "--json".to_owned(),
            ],
        ),
        (
            "search invalid database",
            vec![
                "--workspace".to_owned(),
                invalid_workspace_text.clone(),
                "search".to_owned(),
                "present invalid store".to_owned(),
                "--json".to_owned(),
            ],
        ),
    ] {
        let output = run_ee(&args)?;
        ensure(
            output.status.code() == Some(3),
            format!(
                "{surface}: a present directory-shaped database must remain a storage error, got {:?}: stdout={} stderr={}",
                output.status.code(),
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr),
            ),
        )?;
        let json = stdout_json(&output, surface)?;
        ensure(
            string_at(&json, "/error/code", surface)? == "storage",
            format!("{surface}: present-invalid stores must not be mislabeled as absent"),
        )?;
    }

    ensure(
        !nonexistent_workspace.exists(),
        "a nonexistent addressed workspace must remain nonexistent".to_owned(),
    )?;
    ensure(
        !empty_ee_dir.join("ee.db").exists()
            && !sqlite_sidecar_path(&empty_ee_dir.join("ee.db"), "-wal").exists()
            && !sqlite_sidecar_path(&empty_ee_dir.join("ee.db"), "-shm").exists()
            && std::fs::read_dir(&empty_ee_dir)
                .map_err(|error| error.to_string())?
                .next()
                .is_none(),
        "an existing empty .ee directory must remain byte-for-byte empty after misses".to_owned(),
    )?;
    ensure(
        invalid_database.is_dir()
            && std::fs::read_dir(&invalid_database)
                .map_err(|error| error.to_string())?
                .next()
                .is_none(),
        "present-invalid database directories must not be populated or replaced".to_owned(),
    )
}

/// bd-sfjvq: a storeless miss next to a populated store must point the
/// caller at that store through the canonical workspace-miss error instead
/// of leading with `ee init`, including `orient` in JSON and human modes.
#[test]
fn storeless_miss_surfaces_nearby_populated_store() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
    let store_root = tempdir.path().join("nearby_store_root_sfjvq");
    let leaf = store_root.join("sub").join("storeless_leaf_sfjvq");
    std::fs::create_dir_all(&leaf).map_err(|error| error.to_string())?;
    // Pin the parent walk: discovery stops at the first .git it sees.
    std::fs::create_dir_all(store_root.join(".git")).map_err(|error| error.to_string())?;
    let store_root_str = store_root.to_string_lossy().to_string();
    let leaf_str = leaf.to_string_lossy().to_string();
    let canonical_store_root = store_root
        .canonicalize()
        .map_err(|error| format!("canonicalize nearby store root: {error}"))?;
    let addressed_store = leaf
        .canonicalize()
        .map_err(|error| format!("canonicalize storeless leaf: {error}"))?
        .join(".ee")
        .join("ee.db");
    let addressed_store_str = addressed_store.to_string_lossy().to_string();

    let init = run_ee(&[
        "init".to_owned(),
        "--workspace".to_owned(),
        store_root_str.clone(),
        "--json".to_owned(),
    ])?;
    ensure(
        init.status.success(),
        format!(
            "init of the nearby store failed: {}",
            String::from_utf8_lossy(&init.stdout)
        ),
    )?;
    let seeded = run_ee(&[
        "--workspace".to_owned(),
        store_root_str.clone(),
        "remember".to_owned(),
        "nearby store seed fact".to_owned(),
        "--json".to_owned(),
    ])?;
    ensure(
        seeded.status.success(),
        format!(
            "seeding the nearby store failed: {}",
            String::from_utf8_lossy(&seeded.stdout)
        ),
    )?;

    let mut error_snapshots = serde_json::Map::new();
    for (surface, verb) in [
        (
            "remember",
            vec![
                "--workspace".to_owned(),
                leaf_str.clone(),
                "remember".to_owned(),
                "storeless leaf fact".to_owned(),
                "--json".to_owned(),
            ],
        ),
        (
            "search",
            vec![
                "--workspace".to_owned(),
                leaf_str.clone(),
                "search".to_owned(),
                "storeless leaf".to_owned(),
                "--json".to_owned(),
            ],
        ),
        (
            "orient",
            vec![
                "--workspace".to_owned(),
                leaf_str.clone(),
                "orient".to_owned(),
                "storeless orientation snapshot".to_owned(),
                "--fast".to_owned(),
                "--json".to_owned(),
            ],
        ),
    ] {
        let output = run_ee(&verb)?;
        ensure(
            !output.status.success(),
            format!("{}: storeless miss must fail", verb.join(" ")),
        )?;
        ensure(
            output.status.code() == Some(10),
            format!(
                "{}: storeless miss must exit with the dedicated workspace-store-missing code 10, got {:?}",
                verb.join(" "),
                output.status.code()
            ),
        )?;
        let json = stdout_json(&output, &verb.join(" "))?;
        ensure(
            string_at(&json, "/error/code", &verb.join(" "))? == "workspace_store_missing",
            format!(
                "{}: error code must be workspace_store_missing",
                verb.join(" ")
            ),
        )?;
        let repair = string_at(&json, "/error/repair", &verb.join(" "))?;
        ensure(
            repair.contains("a populated store exists at")
                && repair.contains("nearby_store_root_sfjvq")
                && repair.contains("retarget with --workspace"),
            format!(
                "{}: repair must surface the nearby populated store, got: {repair}",
                verb.join(" ")
            ),
        )?;
        ensure(
            repair.contains("storeless_leaf_sfjvq") && repair.contains("looked for"),
            format!(
                "{}: repair must print the exact looked-for path, got: {repair}",
                verb.join(" ")
            ),
        )?;
        ensure(
            repair.ends_with(
                "Only if you intended to create a NEW store here: ee init --workspace .",
            ),
            format!(
                "{}: conditional init guidance must be last, got: {repair}",
                verb.join(" ")
            ),
        )?;
        ensure(
            string_at(&json, "/error/details/addressedStorePath", &verb.join(" "))?
                == addressed_store_str,
            format!("{}: structured addressed path drifted", verb.join(" ")),
        )?;
        let nearby = array_at(
            &json,
            "/error/details/storeDiscovery/nearbyStores",
            &verb.join(" "),
        )?;
        ensure(
            json.pointer("/error/details/storeDiscovery/scanned") == Some(&serde_json::json!(true))
                && nearby.len() == 1
                && string_at(&nearby[0], "/workspaceRoot", &verb.join(" "))?
                    == canonical_store_root.to_string_lossy()
                && nearby[0]["documents"].as_u64() == Some(1),
            format!(
                "{}: structured discovery evidence drifted: {nearby:?}",
                verb.join(" ")
            ),
        )?;
        let recovery = array_at(&json, "/error/details/recovery", &verb.join(" "))?;
        ensure(
            recovery.len() == 3
                && recovery[0]["priority"].as_u64() == Some(1)
                && recovery[0]["flagName"].as_str() == Some("--workspace")
                && recovery[1]["valueHint"].as_str()
                    == Some(canonical_store_root.to_string_lossy().as_ref())
                && recovery[2]["kind"].as_str() == Some("seed")
                && recovery[2]["command"]
                    .as_str()
                    .is_some_and(|command| command.contains("storeless_leaf_sfjvq")),
            format!(
                "{}: exact ranked recovery actions drifted: {recovery:?}",
                verb.join(" ")
            ),
        )?;
        error_snapshots.insert(
            surface.to_owned(),
            serde_json::json!({
                "surface": surface,
                "exitCode": output.status.code(),
                "schema": json["schema"],
                "code": json["error"]["code"],
                "severity": json["error"]["severity"],
                "message": normalize_storeless_snapshot_text(
                    string_at(&json, "/error/message", &verb.join(" "))?,
                    &addressed_store_str,
                    &leaf_str,
                    &store_root_str,
                ),
                "repair": normalize_storeless_snapshot_text(
                    repair,
                    &addressed_store_str,
                    &leaf_str,
                    &store_root_str,
                ),
            }),
        );
    }

    let remember_snapshot = error_snapshots
        .remove("remember")
        .ok_or("remember storeless snapshot missing")?;
    insta::assert_json_snapshot!(remember_snapshot, @r###"
    {
      "surface": "remember",
      "exitCode": 10,
      "schema": "ee.error.v2",
      "code": "workspace_store_missing",
      "severity": "high",
      "message": "Database not found at <ADDRESSED_STORE>",
      "repair": "Re-check --workspace addressing (looked for <ADDRESSED_STORE>); a populated store exists at <NEARBY_WORKSPACE> (1 docs) — retarget with --workspace <NEARBY_WORKSPACE>. Only if you intended to create a NEW store here: ee init --workspace ."
    }
    "###);
    let search_snapshot = error_snapshots
        .remove("search")
        .ok_or("search storeless snapshot missing")?;
    insta::assert_json_snapshot!(search_snapshot, @r###"
    {
      "surface": "search",
      "exitCode": 10,
      "schema": "ee.error.v2",
      "code": "workspace_store_missing",
      "severity": "high",
      "message": "Database not found at <ADDRESSED_STORE>",
      "repair": "Re-check --workspace addressing (looked for <ADDRESSED_STORE>); a populated store exists at <NEARBY_WORKSPACE> (1 docs) — retarget with --workspace <NEARBY_WORKSPACE>. Only if you intended to create a NEW store here: ee init --workspace ."
    }
    "###);
    let orient_snapshot = error_snapshots
        .remove("orient")
        .ok_or("orient storeless snapshot missing")?;
    insta::assert_json_snapshot!(orient_snapshot, @r###"
    {
      "surface": "orient",
      "exitCode": 10,
      "schema": "ee.error.v2",
      "code": "workspace_store_missing",
      "severity": "high",
      "message": "Database not found at <ADDRESSED_STORE>",
      "repair": "Re-check --workspace addressing (looked for <ADDRESSED_STORE>); a populated store exists at <NEARBY_WORKSPACE> (1 docs) — retarget with --workspace <NEARBY_WORKSPACE>. Only if you intended to create a NEW store here: ee init --workspace ."
    }
    "###);

    for (surface, argv) in [
        (
            "remember",
            vec![
                "--workspace".to_owned(),
                leaf_str.clone(),
                "remember".to_owned(),
                "human storeless leaf fact".to_owned(),
            ],
        ),
        (
            "search",
            vec![
                "--workspace".to_owned(),
                leaf_str.clone(),
                "search".to_owned(),
                "human storeless leaf".to_owned(),
            ],
        ),
        (
            "orient",
            vec![
                "--workspace".to_owned(),
                leaf_str.clone(),
                "orient".to_owned(),
                "human storeless orientation".to_owned(),
                "--fast".to_owned(),
            ],
        ),
    ] {
        let output = run_ee(&argv)?;
        ensure(
            output.status.code() == Some(10),
            format!(
                "human {surface}: storeless miss must exit 10, got {:?}",
                output.status.code()
            ),
        )?;
        ensure(
            output.stdout.is_empty(),
            format!(
                "human {surface}: workspace miss must reserve stdout, got {}",
                String::from_utf8_lossy(&output.stdout)
            ),
        )?;
        let human = String::from_utf8(output.stderr)
            .map_err(|error| format!("human {surface} stderr was not UTF-8: {error}"))?;
        let addressing = human
            .find("Re-check --workspace addressing")
            .ok_or_else(|| format!("human {surface}: addressing recovery missing: {human}"))?;
        let nearby = human
            .find("a populated store exists at")
            .ok_or_else(|| format!("human {surface}: nearby recovery missing: {human}"))?;
        let conditional_init = human
            .find("Only if you intended to create a NEW store here: ee init --workspace .")
            .ok_or_else(|| format!("human {surface}: conditional init missing: {human}"))?;
        ensure(
            human.contains(&addressed_store_str)
                && human.contains(canonical_store_root.to_string_lossy().as_ref())
                && addressing < nearby
                && nearby < conditional_init,
            format!(
                "human {surface}: exact addressed path and nearby-first/conditional-init-last recovery drifted: {human}"
            ),
        )?;
    }

    // Same-class converted branches (bd-workspace-miss-init-suggestion-sfjvq
    // follow-up): diag quarantine show, economy report, and maintenance
    // wal-checkpoint must carry the identical canonical storeless identity —
    // exit 10, exact looked-for path, nearby-first retarget, init last.
    for (surface, argv) in [
        (
            "diag quarantine show",
            vec![
                "--workspace".to_owned(),
                leaf_str.clone(),
                "diag".to_owned(),
                "quarantine".to_owned(),
                "show".to_owned(),
                "cass://storeless/leaf.jsonl".to_owned(),
                "--json".to_owned(),
            ],
        ),
        (
            "economy report",
            vec![
                "--workspace".to_owned(),
                leaf_str.clone(),
                "economy".to_owned(),
                "report".to_owned(),
                "--json".to_owned(),
            ],
        ),
        (
            "maintenance wal-checkpoint",
            vec![
                "--workspace".to_owned(),
                leaf_str.clone(),
                "maintenance".to_owned(),
                "wal-checkpoint".to_owned(),
                "--dry-run".to_owned(),
                "--json".to_owned(),
            ],
        ),
    ] {
        let output = run_ee(&argv)?;
        ensure(
            output.status.code() == Some(10),
            format!(
                "{surface}: storeless miss must exit 10, got {:?}",
                output.status.code()
            ),
        )?;
        let json = stdout_json(&output, surface)?;
        ensure(
            string_at(&json, "/error/code", surface)? == "workspace_store_missing",
            format!("{surface}: error code must be workspace_store_missing"),
        )?;
        ensure(
            string_at(&json, "/error/message", surface)?.contains("storeless_leaf_sfjvq"),
            format!("{surface}: message must name the exact looked-for path"),
        )?;
        let repair = string_at(&json, "/error/repair", surface)?;
        ensure(
            repair.contains("looked for")
                && repair.contains("a populated store exists at")
                && repair.contains("retarget with --workspace")
                && repair.ends_with(
                    "Only if you intended to create a NEW store here: ee init --workspace .",
                ),
            format!(
                "{surface}: repair must be nearby-first with conditional init last, got: {repair}"
            ),
        )?;
    }

    // A lookup miss must never create state: after the failed remember and
    // search, both orient renders, and the converted same-class surfaces,
    // the addressed store directory must still not exist at the storeless
    // leaf.
    ensure(
        !leaf.join(".ee").exists() && !addressed_store.exists(),
        format!(
            "storeless lookups must not create the addressed store; {} must stay absent",
            leaf.join(".ee").display()
        ),
    )?;
    Ok(())
}

/// A custom missing database address must remain the exact excluded identity,
/// while every bounded nearby populated store is reported in rank order with
/// an independently shell-safe retarget and conditional initialization last.
#[test]
fn storeless_custom_address_reports_all_ranked_nearby_stores() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
    let root = tempdir.path().join("multi nearby's sfjvq");
    let middle = root.join("zzz two-doc child sfjvq");
    let lowest = root.join("aaa one-doc child sfjvq");
    let missing_database = root.join("missing addressed store.db");
    let registry = tempdir.path().join("multi-nearby-registry.db");
    std::fs::create_dir_all(root.join(".git")).map_err(|error| error.to_string())?;

    for store in [&root, &middle, &lowest] {
        let init = run_ee_with_registry(
            &[
                "init".to_owned(),
                "--workspace".to_owned(),
                store.to_string_lossy().to_string(),
                "--json".to_owned(),
            ],
            &registry,
        )?;
        ensure(
            init.status.success(),
            format!(
                "init of nearby store {} failed: {}",
                store.display(),
                String::from_utf8_lossy(&init.stdout)
            ),
        )?;
    }

    let root_contents = [
        "root default nearby fact one",
        "root default nearby fact two",
        "root default nearby fact three",
    ];
    let middle_contents = ["middle nearby fact one", "middle nearby fact two"];
    let lowest_contents = ["lowest nearby fact one"];
    for (store, contents) in [
        (root.as_path(), root_contents.as_slice()),
        (middle.as_path(), middle_contents.as_slice()),
        (lowest.as_path(), lowest_contents.as_slice()),
    ] {
        for content in contents {
            let remember = run_ee_with_registry(
                &[
                    "--workspace".to_owned(),
                    store.to_string_lossy().to_string(),
                    "remember".to_owned(),
                    (*content).to_owned(),
                    "--json".to_owned(),
                ],
                &registry,
            )?;
            ensure(
                remember.status.success(),
                format!(
                    "seeding nearby store {} failed: {}",
                    store.display(),
                    String::from_utf8_lossy(&remember.stdout)
                ),
            )?;
        }
    }
    ensure(
        root.join(".ee").join("ee.db").is_file() && !missing_database.exists(),
        format!(
            "fixture must populate the conventional root store while leaving the custom address absent: root_db={} custom_db={}",
            root.join(".ee").join("ee.db").display(),
            missing_database.display(),
        ),
    )?;
    let missing_database_materialized = worker_materialized_path(&missing_database)?;

    let miss = run_ee_with_registry(
        &[
            "--workspace".to_owned(),
            root.to_string_lossy().to_string(),
            "search".to_owned(),
            "nearby facts".to_owned(),
            "--database".to_owned(),
            missing_database.to_string_lossy().to_string(),
            "--json".to_owned(),
        ],
        &registry,
    )?;
    ensure(
        miss.status.code() == Some(10),
        format!(
            "custom storeless address must exit 10, got {:?}",
            miss.status.code()
        ),
    )?;
    let json = stdout_json(&miss, "custom storeless address")?;
    ensure(
        string_at(&json, "/error/code", "custom storeless address")? == "workspace_store_missing"
            && string_at(&json, "/error/message", "custom storeless address")?
                == format!(
                    "Database not found at {}",
                    missing_database_materialized.display()
                ),
        "custom storeless miss must retain its code and name the exact addressed database"
            .to_owned(),
    )?;

    let canonical_root = root.canonicalize().map_err(|error| error.to_string())?;
    let canonical_middle = middle.canonicalize().map_err(|error| error.to_string())?;
    let canonical_lowest = lowest.canonicalize().map_err(|error| error.to_string())?;
    let shell_quote = |path: &Path| format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"));
    let quoted_root = shell_quote(&canonical_root);
    let quoted_middle = shell_quote(&canonical_middle);
    let quoted_lowest = shell_quote(&canonical_lowest);
    let repair = string_at(&json, "/error/repair", "custom storeless address")?;
    let looked_for = format!(
        "Re-check --workspace addressing (looked for {})",
        missing_database_materialized.display()
    );
    let init_at = repair
        .find("Only if you intended to create a NEW store here: ee init --workspace .")
        .ok_or_else(|| format!("conditional init missing from repair: {repair}"))?;
    let ranked = [
        (&canonical_root, 3_u64, &quoted_root),
        (&canonical_middle, 2_u64, &quoted_middle),
        (&canonical_lowest, 1_u64, &quoted_lowest),
    ];
    let mut previous_at = 0_usize;
    for (store, documents, quoted) in ranked {
        let listing = format!("{} ({documents} docs)", store.display());
        let listing_count = repair.matches(&listing).count();
        let listing_at = repair.find(&listing).ok_or_else(|| {
            format!("ranked nearby store missing from repair: {listing}: {repair}")
        })?;
        let retarget = format!("retarget with --workspace {quoted}");
        let retarget_count = repair.matches(&retarget).count();
        ensure(
            listing_count == 1
                && retarget_count == 1
                && previous_at < listing_at
                && listing_at < init_at,
            format!(
                "nearby store must appear once in strict rank order with one quoted retarget: listing={listing:?} listing_count={listing_count} retarget={retarget:?} retarget_count={retarget_count} repair={repair}"
            ),
        )?;
        previous_at = listing_at;
    }
    ensure(
        repair.starts_with(&looked_for)
            && repair.matches("retarget with --workspace ").count() == 3
            && repair.ends_with(
                "Only if you intended to create a NEW store here: ee init --workspace .",
            ),
        format!(
            "repair must contain the exact custom address, exactly three unique retargets, and conditional init last: {repair}"
        ),
    )?;
    ensure(
        string_at(
            &json,
            "/error/details/addressedStorePath",
            "custom storeless address",
        )? == missing_database_materialized.to_string_lossy(),
        "custom structured details must retain the exact addressed database".to_owned(),
    )?;
    let structured_stores = array_at(
        &json,
        "/error/details/storeDiscovery/nearbyStores",
        "custom storeless address",
    )?;
    let structured_recovery =
        array_at(&json, "/error/details/recovery", "custom storeless address")?;
    let expected_paths = [&canonical_root, &canonical_middle, &canonical_lowest];
    ensure(
        structured_stores.len() == 3
            && structured_recovery.len() == 5
            && expected_paths.iter().enumerate().all(|(index, expected)| {
                structured_stores[index]["workspaceRoot"].as_str()
                    == Some(expected.to_string_lossy().as_ref())
                    && structured_recovery[index + 1]["valueHint"].as_str()
                        == Some(expected.to_string_lossy().as_ref())
                    && structured_recovery[index + 1]["priority"].as_u64()
                        == Some(u64::try_from(index + 2).unwrap_or(u64::MAX))
            })
            && structured_recovery.last().is_some_and(|action| {
                action["kind"].as_str() == Some("seed") && action["priority"].as_u64() == Some(5)
            }),
        format!(
            "custom details must expose all ranked stores once and init last: stores={structured_stores:?} recovery={structured_recovery:?}"
        ),
    )?;
    ensure(
        !missing_database.exists(),
        "custom storeless search must not create the addressed database".to_owned(),
    )
}

/// bd-workspace-miss-init-suggestion-sfjvq quoting follow-up: when the
/// nearby populated store lives at a path with spaces AND an apostrophe,
/// the storeless repair hint must shell-quote (and escape) its retarget
/// argument. After the leaf is explicitly initialized, empty-store orient's
/// retargeted next command must stay executable against the exact store.
#[test]
fn storeless_miss_quotes_spaced_nearby_workspace_and_command_executes() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
    let store_root = tempdir.path().join("nearby store's sfjvq");
    let leaf = store_root.join("sub").join("storeless leaf's sfjvq");
    std::fs::create_dir_all(&leaf).map_err(|error| error.to_string())?;
    // Pin the parent walk: discovery stops at the first .git it sees.
    std::fs::create_dir_all(store_root.join(".git")).map_err(|error| error.to_string())?;
    let store_root_str = store_root.to_string_lossy().to_string();
    let leaf_str = leaf.to_string_lossy().to_string();

    let init = run_ee(&[
        "init".to_owned(),
        "--workspace".to_owned(),
        store_root_str.clone(),
        "--json".to_owned(),
    ])?;
    ensure(
        init.status.success(),
        format!(
            "init of the spaced nearby store failed: {}",
            String::from_utf8_lossy(&init.stdout)
        ),
    )?;
    let seed_content = "spaced quoting seed fact for retarget execution";
    let seeded = run_ee(&[
        "--workspace".to_owned(),
        store_root_str.clone(),
        "remember".to_owned(),
        seed_content.to_owned(),
        "--json".to_owned(),
    ])?;
    ensure(
        seeded.status.success(),
        format!(
            "seeding the spaced nearby store failed: {}",
            String::from_utf8_lossy(&seeded.stdout)
        ),
    )?;
    let canonical_root = store_root
        .canonicalize()
        .map_err(|error| format!("canonicalize spaced store root: {error}"))?;
    // Mirror the production quoting rules: single-quote wrap with the
    // POSIX `'\''` escape for the embedded apostrophe.
    let quoted_root = format!(
        "'{}'",
        canonical_root.to_string_lossy().replace('\'', "'\\''")
    );

    let miss = run_ee(&[
        "--workspace".to_owned(),
        leaf_str.clone(),
        "remember".to_owned(),
        "spaced storeless leaf fact".to_owned(),
        "--json".to_owned(),
    ])?;
    ensure(
        miss.status.code() == Some(10),
        format!(
            "spaced storeless remember must exit 10, got {:?}",
            miss.status.code()
        ),
    )?;
    let miss_json = stdout_json(&miss, "spaced storeless remember")?;
    ensure(
        string_at(&miss_json, "/error/code", "spaced storeless remember")?
            == "workspace_store_missing",
        "spaced storeless remember must carry the workspace_store_missing code".to_owned(),
    )?;
    let repair = string_at(&miss_json, "/error/repair", "spaced storeless remember")?;
    ensure(
        repair.contains(&format!("retarget with --workspace {quoted_root}")),
        format!(
            "repair must shell-quote the spaced nearby workspace so the hint is executable, got: {repair}"
        ),
    )?;
    ensure(
        !leaf.join(".ee").exists(),
        "the storeless miss must not initialize the leaf before explicit init".to_owned(),
    )?;

    let retarget_fragment = repair
        .split_once("retarget with ")
        .and_then(|(_, rest)| rest.split_once(". Only if"))
        .map(|(fragment, _)| fragment)
        .ok_or_else(|| {
            format!("repair did not contain an executable retarget fragment: {repair}")
        })?;
    let emitted_retarget = format!("ee {retarget_fragment} search \"{seed_content}\" --json");
    let retargeted = run_emitted_ee_command_with_registry(
        &emitted_retarget,
        &tempdir.path().join("repair-retarget-registry.db"),
    )?;
    ensure(
        retargeted.status.success(),
        format!(
            "the workspace fragment taken from the actual repair must execute: stdout={} stderr={}",
            String::from_utf8_lossy(&retargeted.stdout),
            String::from_utf8_lossy(&retargeted.stderr),
        ),
    )?;
    let retargeted_json = stdout_json(&retargeted, "repair-derived retarget search")?;
    let retargeted_results = array_at(
        &retargeted_json,
        "/data/results",
        "repair-derived retarget search",
    )?;
    ensure(
        retargeted_results.iter().any(|result| {
            result.get("content").and_then(serde_json::Value::as_str) == Some(seed_content)
        }),
        format!(
            "repair-derived search must read the nearby store's seeded content: {retargeted_results:?}"
        ),
    )?;
    ensure(
        !leaf.join(".ee").exists(),
        "executing the repair-derived retarget must not initialize the misspelled leaf".to_owned(),
    )?;

    let leaf_init = run_ee(&[
        "init".to_owned(),
        "--workspace".to_owned(),
        leaf_str.clone(),
        "--json".to_owned(),
    ])?;
    ensure(
        leaf_init.status.success(),
        format!(
            "explicit init of the spaced leaf failed: {}",
            String::from_utf8_lossy(&leaf_init.stdout)
        ),
    )?;

    let orient = run_ee(&[
        "--workspace".to_owned(),
        leaf_str.clone(),
        "orient".to_owned(),
        seed_content.to_owned(),
        "--fast".to_owned(),
        "--json".to_owned(),
    ])?;
    ensure(
        orient.status.success(),
        format!(
            "spaced storeless orient failed: {}",
            String::from_utf8_lossy(&orient.stderr)
        ),
    )?;
    let orient_json = stdout_json(&orient, "spaced storeless orient")?;
    let first_next_command = string_at(
        &orient_json,
        "/data/nextCommands/0",
        "spaced orient retargeted next command",
    )?;
    ensure(
        first_next_command.contains(&quoted_root),
        format!("orient must quote the spaced retarget workspace, got: {first_next_command:?}"),
    )?;
    let emitted = run_emitted_ee_command_with_registry(
        first_next_command,
        &tempdir.path().join("emitted-spaced-registry.db"),
    )?;
    ensure(
        emitted.status.success(),
        format!(
            "quoted spaced retarget command must execute: stdout={} stderr={}",
            String::from_utf8_lossy(&emitted.stdout),
            String::from_utf8_lossy(&emitted.stderr)
        ),
    )?;
    let emitted_json = stdout_json(&emitted, "spaced emitted pack command")?;
    let emitted_items = array_at(
        &emitted_json,
        "/data/pack/items",
        "spaced emitted pack command",
    )?;
    ensure(
        emitted_items.iter().any(|item| {
            item.pointer("/content").and_then(serde_json::Value::as_str) == Some(seed_content)
        }),
        format!(
            "executing the quoted retarget must return content from the spaced store: {emitted_items:?}"
        ),
    )?;
    ensure(
        leaf.join(".ee").join("ee.db").is_file(),
        format!(
            "initialized-empty orient must preserve the explicitly created store; {} must exist",
            leaf.join(".ee").join("ee.db").display()
        ),
    )
}

#[test]
fn orient_explicit_external_database_uses_recorded_workspace_identity() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
    let workspace = tempdir.path().join("external db workspace's root-ft1z5");
    let external_store = tempdir.path().join("selected external store's files");
    std::fs::create_dir_all(workspace.join(".git")).map_err(|error| error.to_string())?;
    std::fs::create_dir_all(&external_store).map_err(|error| error.to_string())?;
    let workspace = workspace
        .canonicalize()
        .map_err(|error| error.to_string())?;
    let database = external_store.join("custom.db");
    let workspace_id = stable_workspace_id(&workspace);
    let connection = DbConnection::open_file(&database)
        .map_err(|error| format!("open external orient database: {error}"))?;
    connection
        .migrate()
        .map_err(|error| format!("migrate external orient database: {error}"))?;
    connection
        .insert_workspace(
            &workspace_id,
            &CreateWorkspaceInput {
                path: workspace.display().to_string(),
                name: Some("external-orient-address".to_owned()),
            },
        )
        .map_err(|error| format!("insert external orient workspace: {error}"))?;
    let expected_content = "External custom database is the addressed orient store.";
    for (index, content) in [
        expected_content,
        "External custom database second admitted memory.",
        "External custom database third admitted memory.",
    ]
    .into_iter()
    .enumerate()
    {
        connection
            .insert_memory(
                &format!("mem_{index:026}"),
                &CreateMemoryInput {
                    workspace_id: workspace_id.clone(),
                    level: "episodic".to_owned(),
                    kind: "fact".to_owned(),
                    content: content.to_owned(),
                    workflow_id: None,
                    confidence: 0.8,
                    utility: 0.5,
                    importance: 0.5,
                    provenance_uri: Some("test://orient/external-database".to_owned()),
                    trust_class: "agent_assertion".to_owned(),
                    trust_subclass: None,
                    tags: vec!["external-orient".to_owned()],
                    valid_from: None,
                    valid_to: None,
                },
            )
            .map_err(|error| format!("insert external orient memory {index}: {error}"))?;
    }
    drop(connection);
    let index_dir = external_store.join("index");
    let rebuilt = run_ee(&[
        "--workspace".to_owned(),
        workspace.display().to_string(),
        "index".to_owned(),
        "rebuild".to_owned(),
        "--database".to_owned(),
        database.display().to_string(),
        "--index-dir".to_owned(),
        index_dir.display().to_string(),
        "--json".to_owned(),
    ])?;
    ensure(
        rebuilt.status.success(),
        format!(
            "rebuilding the explicit external index failed: stdout={} stderr={}",
            String::from_utf8_lossy(&rebuilt.stdout),
            String::from_utf8_lossy(&rebuilt.stderr)
        ),
    )?;

    let output = run_ee(&[
        "--workspace".to_owned(),
        workspace.display().to_string(),
        "orient".to_owned(),
        "external custom database".to_owned(),
        "--database".to_owned(),
        database.display().to_string(),
        "--fast".to_owned(),
        "--json".to_owned(),
    ])?;
    ensure(
        output.status.success(),
        format!(
            "orient with an explicit external database failed: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    let response = stdout_json(&output, "orient explicit external database")?;
    ensure(
        response
            .pointer("/success")
            .and_then(serde_json::Value::as_bool)
            == Some(true),
        format!("external database orient must return success: {response}"),
    )?;
    let data = response
        .pointer("/data")
        .ok_or("external database orient response is missing data")?;
    ensure(
        data.pointer("/schema").and_then(serde_json::Value::as_str)
            == Some(ee::models::ORIENT_SCHEMA_V1),
        format!("external database orient must emit ee.orient.v1: {data}"),
    )?;
    ensure(
        data.pointer("/storeDiscovery").is_none(),
        format!(
            "three live memories in the explicitly addressed external store must suppress thin-store discovery: {data}"
        ),
    )?;
    let recent = array_at(
        &response,
        "/data/fastContent/recent",
        "external database orient recent content",
    )?;
    ensure(
        recent.iter().any(|item| {
            item.pointer("/snippet").and_then(serde_json::Value::as_str) == Some(expected_content)
        }),
        format!("orient must read admitted content from the external database: {recent:?}"),
    )?;
    ensure(
        response
            .pointer("/data/fastContent/posture")
            .and_then(serde_json::Value::as_str)
            != Some("unavailable")
            && response
                .pointer("/degraded")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|degraded| {
                    degraded.iter().all(|entry| {
                        entry.pointer("/code").and_then(serde_json::Value::as_str)
                            != Some("orient_fast_recent_unavailable")
                    })
                }),
        format!("explicit external database must not be reported unavailable: {response}"),
    )?;
    let schema_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/schemas/ee.orient.v1.json");
    let schema: serde_json::Value = serde_json::from_slice(
        &std::fs::read(&schema_path)
            .map_err(|error| format!("read {}: {error}", schema_path.display()))?,
    )
    .map_err(|error| format!("parse {}: {error}", schema_path.display()))?;
    ee::testing::validate_json_schema_instance(data, &schema)?;
    let next_commands = array_at(
        &response,
        "/data/nextCommands",
        "external database orient next commands",
    )?;
    let database_text = database.to_string_lossy().to_string();
    let index_text = index_dir.to_string_lossy().to_string();
    for (index, command) in next_commands.iter().enumerate() {
        let command = command
            .as_str()
            .ok_or_else(|| format!("nextCommands[{index}] must be a string"))?;
        if command.starts_with("ee pack ") || command.starts_with("ee search ") {
            ensure(
                command.contains("--database")
                    && command.contains("--index-dir")
                    && command.contains(&database_text.replace('\'', "'\\''"))
                    && command.contains(&index_text.replace('\'', "'\\''")),
                format!(
                    "store-reading nextCommands[{index}] must preserve the shell-quoted external database and index: {command:?}"
                ),
            )?;
        }
        if command.starts_with("ee decide revisit ") {
            ensure(
                command.contains("--database")
                    && command.contains(&database_text.replace('\'', "'\\''")),
                format!(
                    "decision nextCommands[{index}] must preserve the shell-quoted external database: {command:?}"
                ),
            )?;
        }
    }
    let follow_up = string_at(
        &response,
        "/data/nextCommands/0",
        "external database pack follow-up",
    )?;
    let followed = run_emitted_ee_command_with_registry(
        follow_up,
        &tempdir.path().join("external-follow-up-registry.db"),
    )?;
    ensure(
        followed.status.success(),
        format!(
            "external database follow-up failed: stdout={} stderr={}",
            String::from_utf8_lossy(&followed.stdout),
            String::from_utf8_lossy(&followed.stderr)
        ),
    )?;
    let followed_json = stdout_json(&followed, "external database pack follow-up")?;
    let followed_items = array_at(
        &followed_json,
        "/data/pack/items",
        "external database pack follow-up",
    )?;
    ensure(
        followed_items.iter().any(|item| {
            item.pointer("/content").and_then(serde_json::Value::as_str) == Some(expected_content)
        }),
        format!(
            "executing the emitted follow-up must read the intended external store: {followed_items:?}"
        ),
    )?;
    ensure(
        !workspace.join(".ee").join("ee.db").exists(),
        "orient must not create or fall back to the workspace-default database",
    )
}

/// bd-orient-store-discovery-ft1z5 literal acceptance, empty/thin-root flavor:
/// an INITIALIZED root store below the live-memory threshold must discover a populated child
/// store below it, surface it in `--json` `data.storeDiscovery`
/// (workspaceRoot/documents/lastWrite plus the bounded-scan `truncated` flag)
/// and in the human render, and retarget `nextCommands[0]` at the best
/// candidate — while a populated root must omit `storeDiscovery` entirely and
/// keep its Next commands addressed to itself.
#[test]
fn empty_initialized_root_discovers_populated_child_and_populated_root_skips() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
    let root = tempdir.path().join("empty_root_ft1z5");
    let child = root.join("campaign").join("copulattice_ft1z5");
    let poorer_child = root.join("campaign").join("smaller_ft1z5");
    std::fs::create_dir_all(&child).map_err(|error| error.to_string())?;
    std::fs::create_dir_all(&poorer_child).map_err(|error| error.to_string())?;
    // Bound the parent walk so discovery never escapes the fixture.
    std::fs::create_dir_all(root.join(".git")).map_err(|error| error.to_string())?;
    let root_str = root.to_string_lossy().to_string();
    let child_str = child.to_string_lossy().to_string();
    let poorer_child_str = poorer_child.to_string_lossy().to_string();
    let candidate_content = "campaign seed fact for nearby discovery";
    let candidate_query = "campaign seed nearby discovery";

    for workspace in [&root_str, &child_str, &poorer_child_str] {
        let init = run_ee(&[
            "init".to_owned(),
            "--workspace".to_owned(),
            workspace.clone(),
            "--json".to_owned(),
        ])?;
        ensure(
            init.status.success(),
            format!(
                "init of {workspace} failed: {}",
                String::from_utf8_lossy(&init.stdout)
            ),
        )?;
    }
    let poorer_seed = run_ee(&[
        "--workspace".to_owned(),
        poorer_child_str,
        "remember".to_owned(),
        "single poorer nearby fact".to_owned(),
        "--json".to_owned(),
    ])?;
    ensure(
        poorer_seed.status.success(),
        format!(
            "seeding the poorer child store failed: {}",
            String::from_utf8_lossy(&poorer_seed.stdout)
        ),
    )?;
    for content in [
        candidate_content,
        "campaign continuation fact for nearby discovery",
        "campaign recovery fact for nearby discovery",
    ] {
        let seeded = run_ee(&[
            "--workspace".to_owned(),
            child_str.clone(),
            "remember".to_owned(),
            content.to_owned(),
            "--json".to_owned(),
        ])?;
        ensure(
            seeded.status.success(),
            format!(
                "seeding the child store failed: {}",
                String::from_utf8_lossy(&seeded.stdout)
            ),
        )?;
    }
    std::fs::rename(child.join(".ee"), child.join(".ee-campaign"))
        .map_err(|error| error.to_string())?;
    let campaign_store = child
        .join(".ee-campaign")
        .canonicalize()
        .map_err(|error| error.to_string())?;
    let campaign_database = campaign_store.join("ee.db");
    let campaign_index = campaign_store.join("index");
    let campaign_workspace = campaign_store
        .parent()
        .ok_or("campaign store must have a workspace parent")?
        .to_path_buf();

    // Exercise the macOS `/var` -> `/private/var` class of identity mismatch
    // deterministically on every Unix host: the CLI receives a workspace
    // through a symlinked prefix, while the store and emitted command must use
    // the one canonical resolved path.
    #[cfg(unix)]
    let addressed_campaign_workspace = {
        use std::os::unix::fs::symlink;

        let system_prefix = tempdir.path().join("system-prefix-ft1z5");
        symlink(
            root.canonicalize().map_err(|error| error.to_string())?,
            &system_prefix,
        )
        .map_err(|error| error.to_string())?;
        let aliased = system_prefix.join("campaign").join("copulattice_ft1z5");
        ensure(
            aliased.canonicalize().map_err(|error| error.to_string())?
                == child.canonicalize().map_err(|error| error.to_string())?,
            "symlinked system-prefix workspace must resolve to the campaign workspace",
        )?;
        aliased
    };
    #[cfg(not(unix))]
    let addressed_campaign_workspace = child.clone();
    let addressed_campaign_workspace_str =
        addressed_campaign_workspace.to_string_lossy().to_string();

    let orient = run_ee(&[
        "--workspace".to_owned(),
        root_str.clone(),
        "orient".to_owned(),
        candidate_query.to_owned(),
        "--fast".to_owned(),
        "--json".to_owned(),
    ])?;
    let orient_json = stdout_json(&orient, "orient empty initialized root")?;
    let discovery = orient_json
        .pointer("/data/storeDiscovery")
        .ok_or("orient empty root: missing storeDiscovery block")?;
    ensure(
        discovery["storeEmpty"] == serde_json::json!(true)
            && discovery["addressedState"] == serde_json::json!("empty")
            && discovery["addressedDocuments"] == serde_json::json!(0)
            && discovery["thinStoreThreshold"] == serde_json::json!(3)
            && discovery["scanned"] == serde_json::json!(true),
        format!("orient must scan on an initialized empty root, got: {discovery}"),
    )?;
    ensure(
        discovery["truncated"].as_bool().is_some(),
        format!(
            "orient storeDiscovery must report the bounded-scan truncated flag, got: {discovery}"
        ),
    )?;
    let nearby = discovery["nearbyStores"]
        .as_array()
        .ok_or("orient empty root: nearbyStores missing")?;
    let best = nearby
        .first()
        .ok_or("orient nearbyStores must include the populated child store")?;
    let best_path = string_at(best, "/workspaceRoot", "orient best nearby child")?;
    let best_documents = best["documents"]
        .as_u64()
        .ok_or("orient best nearby child: documents must be an integer")?;
    let best_last_write = string_at(best, "/lastWrite", "orient best nearby child")?;
    ensure(
        best_path == campaign_workspace.to_string_lossy(),
        format!(
            "orient best nearby workspace must be the exact canonical campaign workspace: {best:?}"
        ),
    )?;
    ensure(
        string_at(best, "/storeDir", "orient best nearby store directory")?
            == campaign_store.to_string_lossy(),
        format!("orient must preserve the exact .ee-campaign store directory: {best:?}"),
    )?;
    ensure(
        best_documents == 3,
        format!("orient best nearby child must report all three seeded documents, got: {best:?}"),
    )?;
    ensure(
        !best_last_write.is_empty(),
        format!("orient best nearby child must report lastWrite, got: {best:?}"),
    )?;
    let first_next_command = string_at(
        &orient_json,
        "/data/nextCommands/0",
        "orient retargeted next command",
    )?;
    ensure(
        first_next_command.starts_with("ee pack ")
            && first_next_command.contains("--workspace")
            && first_next_command.contains("copulattice_ft1z5")
            && first_next_command.contains("--database")
            && first_next_command.contains(&campaign_database.to_string_lossy().to_string())
            && first_next_command.contains("--index-dir")
            && first_next_command.contains(&campaign_index.to_string_lossy().to_string()),
        format!(
            "orient nextCommands[0] must retarget pack at the exact populated .ee-campaign database and index, got: {first_next_command:?}"
        ),
    )?;
    let emitted = run_emitted_ee_command_with_registry(
        first_next_command,
        &tempdir.path().join("emitted-command-registry.db"),
    )?;
    ensure(
        emitted.status.success(),
        format!(
            "emitted .ee-campaign pack command failed: stdout={} stderr={}",
            String::from_utf8_lossy(&emitted.stdout),
            String::from_utf8_lossy(&emitted.stderr)
        ),
    )?;
    let emitted_json = stdout_json(&emitted, "emitted .ee-campaign pack command")?;
    let emitted_items = array_at(
        &emitted_json,
        "/data/pack/items",
        "emitted .ee-campaign pack command",
    )?;
    ensure(
        emitted_items.iter().any(|item| {
            item.pointer("/content").and_then(serde_json::Value::as_str) == Some(candidate_content)
        }),
        format!(
            "executing the emitted command must return content from the exact .ee-campaign store: {emitted_items:?}"
        ),
    )?;
    let addressed_campaign = run_ee(&[
        "--workspace".to_owned(),
        addressed_campaign_workspace_str.clone(),
        "orient".to_owned(),
        candidate_query.to_owned(),
        "--fast".to_owned(),
        "--json".to_owned(),
    ])?;
    let addressed_campaign_json =
        stdout_json(&addressed_campaign, "orient addressed .ee-campaign store")?;
    let addressed_recent = array_at(
        &addressed_campaign_json,
        "/data/fastContent/recent",
        "orient addressed .ee-campaign recent content",
    )?;
    let addressed_relevant = array_at(
        &addressed_campaign_json,
        "/data/fastContent/relevant",
        "orient addressed .ee-campaign relevant content",
    )?;
    ensure(
        addressed_campaign_json
            .pointer("/data/storeDiscovery")
            .is_none()
            && addressed_recent.iter().any(|item| {
                item.pointer("/snippet").and_then(serde_json::Value::as_str)
                    == Some(candidate_content)
            })
            && addressed_relevant.iter().any(|item| {
                item.pointer("/snippet").and_then(serde_json::Value::as_str)
                    == Some(candidate_content)
            }),
        format!(
            "a populated addressed .ee-campaign store may suppress discovery only when fast recent and relevant providers read its content: {}",
            addressed_campaign_json["data"]
        ),
    )?;
    let addressed_next_commands = array_at(
        &addressed_campaign_json,
        "/data/nextCommands",
        "addressed .ee-campaign next commands",
    )?;
    for (index, command) in addressed_next_commands.iter().enumerate() {
        let command = command
            .as_str()
            .ok_or_else(|| format!("nextCommands[{index}] must be a string"))?;
        if command.starts_with("ee pack ") || command.starts_with("ee search ") {
            ensure(
                command.contains("--database")
                    && command.contains("--index-dir")
                    && command.contains(&campaign_database.to_string_lossy().to_string())
                    && command.contains(&campaign_index.to_string_lossy().to_string()),
                format!(
                    "store-reading nextCommands[{index}] must preserve the selected .ee-campaign database and index: {command:?}"
                ),
            )?;
        }
        if command.starts_with("ee decide revisit ") {
            ensure(
                command.contains("--database")
                    && command.contains(&campaign_database.to_string_lossy().to_string()),
                format!(
                    "decision nextCommands[{index}] must preserve the selected .ee-campaign database: {command:?}"
                ),
            )?;
        }
    }
    let addressed_follow_up = string_at(
        &addressed_campaign_json,
        "/data/nextCommands/0",
        "addressed .ee-campaign pack follow-up",
    )?;
    let addressed_followed = run_emitted_ee_command_with_registry(
        addressed_follow_up,
        &tempdir
            .path()
            .join("addressed-campaign-follow-up-registry.db"),
    )?;
    ensure(
        addressed_followed.status.success(),
        format!(
            "addressed .ee-campaign follow-up failed: stdout={} stderr={}",
            String::from_utf8_lossy(&addressed_followed.stdout),
            String::from_utf8_lossy(&addressed_followed.stderr)
        ),
    )?;
    let addressed_followed_json =
        stdout_json(&addressed_followed, "addressed .ee-campaign pack follow-up")?;
    let addressed_followed_items = array_at(
        &addressed_followed_json,
        "/data/pack/items",
        "addressed .ee-campaign pack follow-up",
    )?;
    ensure(
        addressed_followed_items.iter().any(|item| {
            item.pointer("/content").and_then(serde_json::Value::as_str) == Some(candidate_content)
        }),
        format!(
            "executing the emitted follow-up must read the selected .ee-campaign store: {addressed_followed_items:?}"
        ),
    )?;

    let addressed_campaign_full = run_ee(&[
        "--workspace".to_owned(),
        addressed_campaign_workspace_str,
        "orient".to_owned(),
        candidate_query.to_owned(),
        "--json".to_owned(),
    ])?;
    let addressed_campaign_full_json = stdout_json(
        &addressed_campaign_full,
        "full orient addressed .ee-campaign store",
    )?;
    let addressed_full_items = array_at(
        &addressed_campaign_full_json,
        "/data/pack/pack/items",
        "full orient addressed .ee-campaign pack content",
    )?;
    ensure(
        addressed_campaign_full_json
            .pointer("/data/storeDiscovery")
            .is_none()
            && addressed_full_items.iter().any(|item| {
                item.pointer("/content").and_then(serde_json::Value::as_str)
                    == Some(candidate_content)
            }),
        format!(
            "a populated addressed .ee-campaign store may suppress discovery only when the full pack provider reads its content: {}",
            addressed_campaign_full_json["data"]
        ),
    )?;

    // Once an explicit default store exists, it is the addressed source of
    // truth. The populated campaign store at the same workspace root must no
    // longer suppress discovery; it must be reported as the retarget option.
    let default_init = run_ee(&[
        "init".to_owned(),
        "--workspace".to_owned(),
        child_str.clone(),
        "--json".to_owned(),
    ])?;
    ensure(
        default_init.status.success(),
        format!(
            "initializing the explicit empty default child store failed: stdout={} stderr={}",
            String::from_utf8_lossy(&default_init.stdout),
            String::from_utf8_lossy(&default_init.stderr)
        ),
    )?;
    let child_default_database = child
        .canonicalize()
        .map_err(|error| error.to_string())?
        .join(".ee")
        .join("ee.db");
    let addressed_default = run_ee(&[
        "--workspace".to_owned(),
        child_str.clone(),
        "orient".to_owned(),
        candidate_query.to_owned(),
        "--fast".to_owned(),
        "--json".to_owned(),
    ])?;
    let addressed_default_json = stdout_json(
        &addressed_default,
        "orient empty default beside populated .ee-campaign",
    )?;
    let addressed_default_discovery = addressed_default_json
        .pointer("/data/storeDiscovery")
        .ok_or("empty addressed default must discover its populated sibling campaign store")?;
    ensure(
        string_at(
            addressed_default_discovery,
            "/addressedStorePath",
            "same-root addressed default database",
        )? == child_default_database.to_string_lossy()
            && string_at(
                addressed_default_discovery,
                "/nearbyStores/0/storeDir",
                "same-root campaign discovery",
            )? == campaign_store.to_string_lossy(),
        format!(
            "the exact empty .ee database must remain addressed while .ee-campaign is reported separately: {addressed_default_discovery}"
        ),
    )?;

    let orient_human = run_ee(&[
        "--workspace".to_owned(),
        root_str.clone(),
        "orient".to_owned(),
        candidate_query.to_owned(),
        "--fast".to_owned(),
    ])?;
    ensure(
        orient_human.status.success(),
        format!(
            "human orient at empty root failed: {}",
            String::from_utf8_lossy(&orient_human.stderr)
        ),
    )?;
    let human = String::from_utf8(orient_human.stdout)
        .map_err(|error| format!("human orient stdout was not UTF-8: {error}"))?;
    ensure(
        human.contains("copulattice_ft1z5")
            && human.contains(&format!("{best_documents} docs"))
            && human.contains(&format!("last write {best_last_write}"))
            && human.contains(first_next_command),
        format!(
            "human orient at the empty root must print the child path/documents/last-write and exact retargeted command; \
             documents={best_documents}, lastWrite={best_last_write:?}, command={first_next_command:?}\n{human}"
        ),
    )?;

    // Full mode must expose the same discovery contract and retargeting as
    // fast mode when the addressed store is empty.
    let orient_full = run_ee(&[
        "--workspace".to_owned(),
        root_str.clone(),
        "orient".to_owned(),
        candidate_query.to_owned(),
        "--json".to_owned(),
    ])?;
    let orient_full_json = stdout_json(&orient_full, "full orient empty initialized root")?;
    let full_discovery = orient_full_json
        .pointer("/data/storeDiscovery")
        .ok_or("full orient empty root: missing storeDiscovery block")?;
    let full_best = full_discovery
        .pointer("/nearbyStores/0")
        .ok_or("full orient empty root: missing best nearby store")?;
    let full_next_command = string_at(
        &orient_full_json,
        "/data/nextCommands/0",
        "full orient retargeted next command",
    )?;
    ensure(
        full_discovery["storeEmpty"] == serde_json::json!(true)
            && full_discovery["scanned"] == serde_json::json!(true)
            && string_at(full_best, "/workspaceRoot", "full orient best nearby child")?
                == best_path
            && full_best["documents"].as_u64() == Some(best_documents)
            && string_at(full_best, "/lastWrite", "full orient best nearby child")?
                == best_last_write
            && full_next_command == first_next_command,
        format!(
            "full orient must match fast-mode discovery and retargeting; fast={discovery}, full={full_discovery}, fastCommand={first_next_command:?}, fullCommand={full_next_command:?}"
        ),
    )?;

    // Two unrelated live memories leave the addressed root truthfully thin.
    // Discovery must still report and retarget the richer child rather than
    // treating any positive row count as sufficient orientation state.
    let root_seed = run_ee(&[
        "--workspace".to_owned(),
        root_str.clone(),
        "remember".to_owned(),
        "root store resume fact".to_owned(),
        "--json".to_owned(),
    ])?;
    ensure(
        root_seed.status.success(),
        format!(
            "seeding the root store failed: {}",
            String::from_utf8_lossy(&root_seed.stdout)
        ),
    )?;
    let second_root_seed = run_ee(&[
        "--workspace".to_owned(),
        root_str.clone(),
        "remember".to_owned(),
        "second root store unrelated fact".to_owned(),
        "--json".to_owned(),
    ])?;
    ensure(
        second_root_seed.status.success(),
        format!(
            "seeding the second root fact failed: {}",
            String::from_utf8_lossy(&second_root_seed.stdout)
        ),
    )?;
    let nonmatching_task = "zzzz_ft1z5_no_matching_orient_content_zzzz";
    let thin = run_ee(&[
        "--workspace".to_owned(),
        root_str.clone(),
        "orient".to_owned(),
        nonmatching_task.to_owned(),
        "--json".to_owned(),
    ])?;
    let thin_json = stdout_json(&thin, "nonmatching orient against thin root")?;
    let thin_items = thin_json
        .pointer("/data/pack/pack/items")
        .and_then(serde_json::Value::as_array)
        .ok_or("thin nonmatching orient: missing pack items")?;
    ensure(
        thin_items.is_empty(),
        format!(
            "planted thin-store case requires empty/nonmatching orient content, got: {thin_items:?}"
        ),
    )?;
    let thin_discovery = thin_json
        .pointer("/data/storeDiscovery")
        .ok_or("a two-memory root must retain thin-store discovery")?;
    let thin_nearby = thin_discovery["nearbyStores"]
        .as_array()
        .ok_or("thin-store nearbyStores must be an array")?;
    ensure(
        thin_discovery["addressedState"] == serde_json::json!("thin")
            && thin_discovery["addressedDocuments"] == serde_json::json!(2)
            && thin_discovery["thinStoreThreshold"] == serde_json::json!(3)
            && thin_discovery["storeEmpty"] == serde_json::json!(false)
            && thin_nearby.len() == 1
            && thin_discovery["nearbyStores"][0]["workspaceRoot"] == serde_json::json!(best_path)
            && thin_discovery["nearbyStores"][0]["documents"] == serde_json::json!(best_documents)
            && thin_json["data"]["nextCommands"][0] == serde_json::json!(first_next_command),
        format!(
            "a two-memory root must exclude the one-document child and retarget only the three-document child: {thin_discovery}"
        ),
    )?;

    let thin_human = run_ee(&[
        "--workspace".to_owned(),
        root_str.clone(),
        "orient".to_owned(),
        nonmatching_task.to_owned(),
    ])?;
    ensure(
        thin_human.status.success(),
        format!(
            "human orient against thin root failed: {}",
            String::from_utf8_lossy(&thin_human.stderr)
        ),
    )?;
    let thin_human = String::from_utf8(thin_human.stdout)
        .map_err(|error| format!("thin-root human orient stdout was not UTF-8: {error}"))?;
    ensure(
        thin_human.contains("thin: 2 live memories; discovery threshold 3")
            && thin_human.contains("This store is thin, and richer stores exist nearby")
            && !thin_human.contains("smaller_ft1z5")
            && thin_human.contains(first_next_command),
        format!("thin-root human output must retain posture and retarget: {thin_human}"),
    )?;

    // At the explicit threshold the root is genuinely populated. It must omit
    // discovery even when the task has no matching content and keep its Next
    // commands addressed to itself.
    let threshold_seed = run_ee(&[
        "--workspace".to_owned(),
        root_str.clone(),
        "remember".to_owned(),
        "third root store unrelated fact".to_owned(),
        "--json".to_owned(),
    ])?;
    ensure(
        threshold_seed.status.success(),
        format!(
            "seeding the threshold root fact failed: {}",
            String::from_utf8_lossy(&threshold_seed.stdout)
        ),
    )?;
    let populated = run_ee(&[
        "--workspace".to_owned(),
        root_str,
        "orient".to_owned(),
        nonmatching_task.to_owned(),
        "--json".to_owned(),
    ])?;
    let populated_json = stdout_json(&populated, "nonmatching orient against populated root")?;
    let populated_items = populated_json
        .pointer("/data/pack/pack/items")
        .and_then(serde_json::Value::as_array)
        .ok_or("populated nonmatching orient: missing pack items")?;
    ensure(
        populated_items.is_empty() && populated_json.pointer("/data/storeDiscovery").is_none(),
        format!(
            "a threshold-populated root with no matching content must omit storeDiscovery entirely, got: {}",
            populated_json["data"]
        ),
    )?;
    let populated_first_next = string_at(
        &populated_json,
        "/data/nextCommands/0",
        "orient populated-root next command",
    )?;
    ensure(
        !populated_first_next.contains("copulattice_ft1z5"),
        format!(
            "a populated root's next commands must stay on the root workspace, got: {populated_first_next:?}"
        ),
    )?;
    Ok(())
}

/// bd-orient-store-discovery-ft1z5 registry acceptance: a populated workspace
/// outside the bounded parent/child walk is still discoverable through the
/// real machine registry. Empty, broken, addressed, and duplicate local rows
/// must not create false or repeated candidates.
#[test]
fn empty_workspace_discovers_registered_remote_store_and_skips_bad_rows() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
    let registry = tempdir.path().join("registry").join("workspaces.db");
    let addressed = tempdir.path().join("addressed-empty-ft1z5");
    let registered = tempdir.path().join("remote-populated-ft1z5");
    let local_duplicate = addressed.join("local-registered-duplicate-ft1z5");
    let empty_registered = tempdir.path().join("remote-empty-ft1z5");
    let broken_registered = tempdir.path().join("remote-broken-ft1z5");

    std::fs::create_dir_all(addressed.join(".git")).map_err(|error| error.to_string())?;
    for workspace in [&addressed, &registered, &local_duplicate, &empty_registered] {
        let workspace_text = workspace.to_string_lossy().to_string();
        let init = run_ee(&[
            "init".to_owned(),
            "--workspace".to_owned(),
            workspace_text.clone(),
            "--json".to_owned(),
        ])?;
        ensure(
            init.status.success(),
            format!(
                "init of {workspace_text} failed: {}",
                String::from_utf8_lossy(&init.stdout)
            ),
        )?;
    }

    for index in 1..=3 {
        let seeded = run_ee(&[
            "--workspace".to_owned(),
            registered.to_string_lossy().to_string(),
            "remember".to_owned(),
            format!("registered remote discovery fact {index}"),
            "--json".to_owned(),
        ])?;
        ensure(
            seeded.status.success(),
            format!(
                "seeding registered remote store failed: {}",
                String::from_utf8_lossy(&seeded.stdout)
            ),
        )?;
    }
    let local_seed = run_ee(&[
        "--workspace".to_owned(),
        local_duplicate.to_string_lossy().to_string(),
        "remember".to_owned(),
        "local duplicate discovery fact".to_owned(),
        "--json".to_owned(),
    ])?;
    ensure(
        local_seed.status.success(),
        format!(
            "seeding local duplicate store failed: {}",
            String::from_utf8_lossy(&local_seed.stdout)
        ),
    )?;

    // A registry row can legitimately outlive its store. Create that row via
    // the public alias surface with a directory-shaped database so discovery
    // proves it skips the broken source rather than treating it as empty.
    std::fs::create_dir_all(broken_registered.join(".ee").join("ee.db"))
        .map_err(|error| error.to_string())?;

    for (workspace, alias) in [
        (&addressed, "addressed-empty-ft1z5"),
        (&registered, "remote-populated-ft1z5"),
        (&local_duplicate, "local-duplicate-ft1z5"),
        (&empty_registered, "remote-empty-ft1z5"),
        (&broken_registered, "remote-broken-ft1z5"),
    ] {
        let alias_output = run_ee_with_registry(
            &[
                "--workspace".to_owned(),
                workspace.to_string_lossy().to_string(),
                "workspace".to_owned(),
                "alias".to_owned(),
                "--as".to_owned(),
                alias.to_owned(),
                "--json".to_owned(),
            ],
            &registry,
        )?;
        ensure(
            alias_output.status.success(),
            format!(
                "registering {alias} through workspace alias failed: stdout={} stderr={}",
                String::from_utf8_lossy(&alias_output.stdout),
                String::from_utf8_lossy(&alias_output.stderr)
            ),
        )?;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;

        let linked_registered = tempdir.path().join("remote-linked-db-ft1z5");
        std::fs::create_dir_all(linked_registered.join(".ee"))
            .map_err(|error| error.to_string())?;
        let alias_output = run_ee_with_registry(
            &[
                "--workspace".to_owned(),
                linked_registered.to_string_lossy().to_string(),
                "workspace".to_owned(),
                "alias".to_owned(),
                "--as".to_owned(),
                "remote-linked-db-ft1z5".to_owned(),
                "--json".to_owned(),
            ],
            &registry,
        )?;
        ensure(
            alias_output.status.success(),
            format!(
                "registering symlink negative failed: {}",
                String::from_utf8_lossy(&alias_output.stdout)
            ),
        )?;
        symlink(
            registered.join(".ee").join("ee.db"),
            linked_registered.join(".ee").join("ee.db"),
        )
        .map_err(|error| error.to_string())?;
    }

    let registered_canonical_path = registered
        .canonicalize()
        .map_err(|error| error.to_string())?;
    let registered_canonical = registered_canonical_path.to_string_lossy().to_string();
    let local_canonical = local_duplicate
        .canonicalize()
        .map_err(|error| error.to_string())?
        .to_string_lossy()
        .to_string();
    let database = registered_canonical_path.join(".ee").join("ee.db");
    let mut wal = database.as_os_str().to_os_string();
    wal.push("-wal");
    let expected_last_write = [database.as_path(), Path::new(&wal)]
        .into_iter()
        .filter_map(|path| {
            let metadata = std::fs::symlink_metadata(path).ok()?;
            if !metadata.file_type().is_file() {
                return None;
            }
            metadata.modified().ok()
        })
        .max()
        .map(|modified| chrono::DateTime::<chrono::Utc>::from(modified).to_rfc3339())
        .ok_or("registered store must have a durable database mtime")?;
    let registry_wal = sqlite_sidecar_path(&registry, "-wal");
    let registry_shm = sqlite_sidecar_path(&registry, "-shm");
    let registry_before = snapshot_file(&registry)?.ok_or("workspace registry must exist")?;
    let registry_wal_before = snapshot_file(&registry_wal)?;
    let registry_shm_before = snapshot_file(&registry_shm)?;

    let orient = run_ee_with_registry(
        &[
            "--workspace".to_owned(),
            addressed.to_string_lossy().to_string(),
            "orient".to_owned(),
            "registered store discovery".to_owned(),
            "--fast".to_owned(),
            "--json".to_owned(),
        ],
        &registry,
    )?;
    ensure(
        orient.status.success(),
        format!(
            "orient with workspace registry failed: stdout={} stderr={}",
            String::from_utf8_lossy(&orient.stdout),
            String::from_utf8_lossy(&orient.stderr)
        ),
    )?;
    let registry_after = snapshot_file(&registry)?.ok_or("workspace registry disappeared")?;
    let registry_wal_after = snapshot_file(&registry_wal)?;
    let registry_shm_after = snapshot_file(&registry_shm)?;
    ensure(
        registry_after == registry_before
            && registry_wal_after == registry_wal_before
            && registry_shm_after == registry_shm_before,
        format!(
            "registry discovery must preserve registry/WAL/SHM presence, bytes, mtime, and permissions: registryEqual={} walEqual={} shmEqual={}",
            registry_after == registry_before,
            registry_wal_after == registry_wal_before,
            registry_shm_after == registry_shm_before,
        ),
    )?;
    let json = stdout_json(&orient, "orient registered remote store")?;
    let nearby = array_at(
        &json,
        "/data/storeDiscovery/nearbyStores",
        "orient registered remote store",
    )?;
    ensure(
        nearby.len() == 2,
        format!(
            "only the remote populated store and deduped local populated store may surface: {nearby:?}"
        ),
    )?;
    let best = &nearby[0];
    ensure(
        string_at(best, "/workspaceRoot", "registered best workspace")? == registered_canonical
            && best["documents"].as_u64() == Some(3)
            && string_at(best, "/lastWrite", "registered best last write")? == expected_last_write,
        format!(
            "registered best store must expose its exact canonical path, three rows, and durable last-write; expectedPath={registered_canonical:?}, expectedLastWrite={expected_last_write:?}, best={best:?}"
        ),
    )?;
    ensure(
        string_at(&nearby[1], "/workspaceRoot", "deduped local workspace")? == local_canonical
            && nearby[1]["documents"].as_u64() == Some(1)
            && nearby
                .iter()
                .filter(|store| {
                    store
                        .pointer("/workspaceRoot")
                        .and_then(serde_json::Value::as_str)
                        == Some(local_canonical.as_str())
                })
                .count()
                == 1,
        format!("the local/registry overlap must appear exactly once with one row: {nearby:?}"),
    )?;
    ensure(
        nearby.iter().all(|store| {
            store
                .pointer("/workspaceRoot")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|path| {
                    !path.contains("remote-empty-ft1z5")
                        && !path.contains("remote-broken-ft1z5")
                        && !path.contains("remote-linked-db-ft1z5")
                })
        }),
        format!(
            "empty, broken, and symlinked registry candidates must all stay excluded: {nearby:?}"
        ),
    )?;
    let next_command = string_at(&json, "/data/nextCommands/0", "registered retarget command")?;
    let registered_index = registered_canonical_path.join(".ee").join("index");
    ensure(
        next_command.starts_with("ee pack ")
            && next_command.contains(&registered_canonical)
            && next_command.contains("--database")
            && next_command.contains(&database.to_string_lossy().to_string())
            && next_command.contains("--index-dir")
            && next_command.contains(&registered_index.to_string_lossy().to_string()),
        format!(
            "orient must retarget the first next command to the registered best store's exact database and index: {next_command:?}"
        ),
    )?;
    let emitted = run_emitted_ee_command_with_registry(
        next_command,
        &tempdir.path().join("emitted-registered-registry.db"),
    )?;
    ensure(
        emitted.status.success(),
        format!(
            "registered-store emitted pack command failed: stdout={} stderr={}",
            String::from_utf8_lossy(&emitted.stdout),
            String::from_utf8_lossy(&emitted.stderr)
        ),
    )?;
    let emitted_json = stdout_json(&emitted, "registered-store emitted pack command")?;
    let emitted_items = array_at(
        &emitted_json,
        "/data/pack/items",
        "registered-store emitted pack command",
    )?;
    ensure(
        emitted_items.iter().any(|item| {
            item.pointer("/content")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|content| content.starts_with("registered remote discovery fact"))
        }),
        format!(
            "executing the registered-store command must read content from its exact database/index: {emitted_items:?}"
        ),
    )?;
    Ok(())
}

/// bd-workspace-miss-init-suggestion-sfjvq negatives: an ORDINARY storage
/// failure must keep exit 3 / code `storage` — proving the dedicated
/// workspace-miss exit 10 is a distinction, not a rename — and `ee init`
/// into a directory with existing AGENTS.md/CLAUDE.md must preserve both
/// files byte-for-byte (no silent overwrite).
#[test]
fn ordinary_storage_failure_keeps_exit_three_and_init_preserves_agent_docs() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;

    // Phase 1: `.ee/ee.db` exists but is a DIRECTORY — the store is
    // addressed and present, so this is a genuine storage failure, not an
    // addressing miss.
    let broken = tempdir.path().join("broken_store_sfjvq");
    std::fs::create_dir_all(broken.join(".ee").join("ee.db")).map_err(|error| error.to_string())?;
    let broken_str = broken.to_string_lossy().to_string();
    let output = run_ee(&[
        "--workspace".to_owned(),
        broken_str,
        "remember".to_owned(),
        "ordinary storage failure fact".to_owned(),
        "--json".to_owned(),
    ])?;
    ensure(
        output.status.code() == Some(3),
        format!(
            "ordinary storage failure must keep exit 3, got {:?}\nstdout:\n{}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout)
        ),
    )?;
    let json = stdout_json(&output, "remember against directory-shaped db")?;
    let code = string_at(&json, "/error/code", "remember against directory-shaped db")?;
    ensure(
        code == "storage",
        format!("ordinary storage failure must keep code storage, got: {code}"),
    )?;

    // Phase 2: init preservation re-proof — pre-existing AGENTS.md and
    // CLAUDE.md must survive `ee init` unchanged.
    let docs_root = tempdir.path().join("init_preserve_sfjvq");
    std::fs::create_dir_all(&docs_root).map_err(|error| error.to_string())?;
    let agents_sentinel = "existing AGENTS.md sentinel: ee init must never overwrite me\n";
    let claude_sentinel = "existing CLAUDE.md sentinel: ee init must never overwrite me\n";
    std::fs::write(docs_root.join("AGENTS.md"), agents_sentinel)
        .map_err(|error| error.to_string())?;
    std::fs::write(docs_root.join("CLAUDE.md"), claude_sentinel)
        .map_err(|error| error.to_string())?;
    let agents_path = docs_root.join("AGENTS.md");
    let claude_path = docs_root.join("CLAUDE.md");
    let agents_before = snapshot_file(&agents_path)?.ok_or("AGENTS.md snapshot missing")?;
    let claude_before = snapshot_file(&claude_path)?.ok_or("CLAUDE.md snapshot missing")?;
    for (label, extra) in [("ordinary init", None), ("forced init", Some("--force"))] {
        let mut args = vec![
            "init".to_owned(),
            "--workspace".to_owned(),
            docs_root.to_string_lossy().to_string(),
            "--json".to_owned(),
        ];
        if let Some(flag) = extra {
            args.push(flag.to_owned());
        }
        let init = run_ee(&args)?;
        ensure(
            init.status.success(),
            format!(
                "{label} beside existing agent docs must succeed with extra flag {extra:?}: {}",
                String::from_utf8_lossy(&init.stdout)
            ),
        )?;
        let init_json = stdout_json(&init, label)?;
        let actions = array_at(&init_json, "/data/actions", label)?;
        for expected_path in [&agents_path, &claude_path] {
            let expected_materialized = worker_materialized_path(expected_path)?;
            let expected_materialized_text = expected_materialized.to_string_lossy().to_string();
            ensure(
                actions.iter().any(|action| {
                    action["action"].as_str() == Some("check_file")
                        && action["status"].as_str() == Some("exists")
                        && action["path"].as_str() == Some(expected_materialized_text.as_str())
                }),
                format!(
                    "{label} must report the preserved file as exists: path={} actions={actions:?}",
                    expected_materialized.display()
                ),
            )?;
        }
        let agents_after = snapshot_file(&agents_path)?
            .ok_or_else(|| format!("AGENTS.md disappeared after {label}"))?;
        let claude_after = snapshot_file(&claude_path)?
            .ok_or_else(|| format!("CLAUDE.md disappeared after {label}"))?;
        ensure(
            agents_after == agents_before && claude_after == claude_before,
            format!(
                "{label} must independently preserve AGENTS.md and CLAUDE.md byte-for-byte with metadata"
            ),
        )?;
    }
    Ok(())
}
