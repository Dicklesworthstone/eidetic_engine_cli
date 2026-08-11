use std::path::Path;
use std::process::{Command, Output};

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

fn split_emitted_command(command: &str) -> Result<Vec<String>, String> {
    #[derive(Copy, Clone, Eq, PartialEq)]
    enum Quote {
        None,
        Single,
        Double,
    }

    let mut args = Vec::new();
    let mut current = String::new();
    let mut quote = Quote::None;
    let mut chars = command.chars();
    while let Some(ch) = chars.next() {
        match quote {
            Quote::None => match ch {
                '\'' => quote = Quote::Single,
                '"' => quote = Quote::Double,
                '\\' => {
                    let next = chars.next().ok_or("emitted command ends with an escape")?;
                    current.push(next);
                }
                ch if ch.is_whitespace() => {
                    if !current.is_empty() {
                        args.push(std::mem::take(&mut current));
                    }
                }
                _ => current.push(ch),
            },
            Quote::Single => {
                if ch == '\'' {
                    quote = Quote::None;
                } else {
                    current.push(ch);
                }
            }
            Quote::Double => match ch {
                '"' => quote = Quote::None,
                '\\' => {
                    let next = chars
                        .next()
                        .ok_or("emitted double-quoted command ends with an escape")?;
                    current.push(next);
                }
                _ => current.push(ch),
            },
        }
    }
    if quote != Quote::None {
        return Err("emitted command has an unterminated quote".to_owned());
    }
    if !current.is_empty() {
        args.push(current);
    }
    Ok(args)
}

fn run_emitted_ee_command_with_registry(command: &str, registry: &Path) -> Result<Output, String> {
    let args = split_emitted_command(command)?;
    if args.first().map(String::as_str) != Some("ee") {
        return Err(format!("emitted command must start with ee: {command:?}"));
    }
    run_ee_with_registry(&args[1..], registry)
}

fn sqlite_sidecar_path(database: &Path, suffix: &str) -> std::path::PathBuf {
    let mut path = database.as_os_str().to_os_string();
    path.push(suffix);
    path.into()
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

/// bd-sfjvq: a storeless miss next to a populated store must point the
/// caller at that store (remember/search via the freetext repair, orient
/// via the storeDiscovery block) instead of leading with `ee init`.
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
    let addressed_store = leaf.join(".ee").join("ee.db");
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

    let orient = run_ee(&[
        "--workspace".to_owned(),
        leaf_str.clone(),
        "orient".to_owned(),
        "storeless orientation snapshot".to_owned(),
        "--fast".to_owned(),
        "--json".to_owned(),
    ])?;
    ensure(
        orient.status.success(),
        format!(
            "orient storeless leaf must succeed with diagnostic discovery: {}",
            String::from_utf8_lossy(&orient.stderr)
        ),
    )?;
    let orient_json = stdout_json(&orient, "orient storeless leaf")?;
    let discovery = orient_json
        .pointer("/data/storeDiscovery")
        .ok_or("orient storeless leaf: missing storeDiscovery block")?;
    ensure(
        discovery["storeEmpty"] == serde_json::json!(true)
            && discovery["scanned"] == serde_json::json!(true),
        format!("orient must scan on a storeless workspace, got: {discovery}"),
    )?;
    ensure(
        discovery["addressedStorePath"] == serde_json::json!(addressed_store_str),
        format!(
            "orient must report the exact addressed store path {}; got: {discovery}",
            addressed_store.display()
        ),
    )?;
    let nearby = discovery["nearbyStores"]
        .as_array()
        .ok_or("orient storeless leaf: nearbyStores missing")?;
    let best = nearby
        .first()
        .ok_or("orient nearbyStores must include the populated store")?;
    let best_path = string_at(best, "/workspaceRoot", "orient best nearby store")?;
    let best_documents = best["documents"]
        .as_u64()
        .ok_or("orient best nearby store: documents must be an integer")?;
    let best_last_write = string_at(best, "/lastWrite", "orient best nearby store")?;
    ensure(
        best_path.contains("nearby_store_root_sfjvq"),
        format!("orient best nearby store must be the populated store, got: {best:?}"),
    )?;
    ensure(
        best_documents > 0,
        format!("orient best nearby store must report documents > 0, got: {best:?}"),
    )?;
    ensure(
        !best_last_write.is_empty(),
        format!("orient best nearby store must report lastWrite, got: {best:?}"),
    )?;

    let first_next_command = string_at(
        &orient_json,
        "/data/nextCommands/0",
        "orient retargeted next command",
    )?;
    ensure(
        first_next_command.starts_with("ee pack ")
            && first_next_command.contains("--workspace")
            && first_next_command.contains(best_path),
        format!(
            "orient nextCommands[0] must retarget the pack command to the best candidate; \
             best={best_path:?}, command={first_next_command:?}"
        ),
    )?;
    let next_commands = array_at(
        &orient_json,
        "/data/nextCommands",
        "orient storeless next commands",
    )?;
    ensure(
        next_commands.iter().all(|command| {
            command
                .as_str()
                .is_some_and(|command| !command.contains("ee init"))
        }),
        format!(
            "wrong-cwd orient must retarget toward discovered content without suggesting init: {next_commands:?}"
        ),
    )?;

    let orient_snapshot = serde_json::json!({
        "surface": "orient",
        "exitCode": orient.status.code(),
        "schema": orient_json["schema"],
        "success": orient_json["success"],
        "storeDiscovery": {
            "addressedStorePath": normalize_storeless_snapshot_text(
                string_at(discovery, "/addressedStorePath", "orient store discovery")?,
                &addressed_store_str,
                &leaf_str,
                &store_root_str,
            ),
            "storeEmpty": discovery["storeEmpty"],
            "scanned": discovery["scanned"],
            "truncatedIsBoolean": discovery["truncated"].is_boolean(),
            "bestNearbyStore": {
                "workspaceRoot": normalize_storeless_snapshot_text(
                    best_path,
                    &addressed_store_str,
                    &leaf_str,
                    &store_root_str,
                ),
                "documents": best_documents,
                "lastWrite": if best_last_write.is_empty() {
                    "<EMPTY>"
                } else {
                    "<NONEMPTY_TIMESTAMP>"
                },
            },
        },
        "firstNextCommand": normalize_storeless_snapshot_text(
            first_next_command,
            &addressed_store_str,
            &leaf_str,
            &store_root_str,
        ),
    });
    insta::assert_json_snapshot!(orient_snapshot, @r###"
    {
      "surface": "orient",
      "exitCode": 0,
      "schema": "ee.response.v2",
      "success": true,
      "storeDiscovery": {
        "addressedStorePath": "<ADDRESSED_STORE>",
        "storeEmpty": true,
        "scanned": true,
        "truncatedIsBoolean": true,
        "bestNearbyStore": {
          "workspaceRoot": "<NEARBY_WORKSPACE>",
          "documents": 1,
          "lastWrite": "<NONEMPTY_TIMESTAMP>"
        }
      },
      "firstNextCommand": "ee pack --workspace <NEARBY_WORKSPACE> --database <NEARBY_WORKSPACE>/.ee/ee.db --index-dir <NEARBY_WORKSPACE>/.ee/index --read-only --source-mode lexical_only --max-tokens 4000 --json -- 'storeless orientation snapshot'"
    }
    "###);

    let orient_human = run_ee(&[
        "--workspace".to_owned(),
        leaf_str,
        "orient".to_owned(),
        "storeless orientation snapshot".to_owned(),
        "--fast".to_owned(),
    ])?;
    ensure(
        orient_human.status.success(),
        format!(
            "human orient failed: {}",
            String::from_utf8_lossy(&orient_human.stderr)
        ),
    )?;
    let human = String::from_utf8(orient_human.stdout)
        .map_err(|error| format!("human orient stdout was not UTF-8: {error}"))?;
    ensure(
        human.contains(&addressed_store_str)
            && human.contains(best_path)
            && human.contains(&format!("{best_documents} docs"))
            && human.contains(&format!("last write {best_last_write}"))
            && human.contains(first_next_command)
            && !human.contains("ee init"),
        format!(
            "human orient must print the addressed store, nearby path/documents/last-write, and exact retargeted command without suggesting init; \
             addressed={}, path={best_path:?}, documents={best_documents}, lastWrite={best_last_write:?}, command={first_next_command:?}\n{human}",
            addressed_store.display()
        ),
    )?;

    // A lookup miss must never create state: after the failed remember and
    // search plus both orient renders, the addressed store directory must
    // still not exist at the storeless leaf.
    ensure(
        !leaf.join(".ee").exists() && !addressed_store.exists(),
        format!(
            "storeless lookups must not create the addressed store; {} must stay absent",
            leaf.join(".ee").display()
        ),
    )?;
    Ok(())
}

/// bd-workspace-miss-init-suggestion-sfjvq quoting follow-up: when the
/// nearby populated store lives at a path with spaces, the storeless repair
/// hint must shell-quote its retarget argument and orient's retargeted next
/// command must stay executable end-to-end against the exact store.
#[test]
fn storeless_miss_quotes_spaced_nearby_workspace_and_command_executes() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
    let store_root = tempdir.path().join("nearby store sfjvq");
    let leaf = store_root.join("sub").join("storeless leaf sfjvq");
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
    let quoted_root = format!("'{}'", canonical_root.to_string_lossy());

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
        !leaf.join(".ee").exists(),
        format!(
            "spaced storeless lookups must not create the addressed store; {} must stay absent",
            leaf.join(".ee").display()
        ),
    )
}

/// bd-orient-store-discovery-ft1z5 literal acceptance, empty-root flavor: an
/// INITIALIZED root store with zero documents must discover a populated child
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
    std::fs::create_dir_all(&child).map_err(|error| error.to_string())?;
    // Bound the parent walk so discovery never escapes the fixture.
    std::fs::create_dir_all(root.join(".git")).map_err(|error| error.to_string())?;
    let root_str = root.to_string_lossy().to_string();
    let child_str = child.to_string_lossy().to_string();
    let candidate_content = "campaign seed fact for nearby discovery";
    let candidate_query = "campaign seed nearby discovery";

    for workspace in [&root_str, &child_str] {
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
    let seeded = run_ee(&[
        "--workspace".to_owned(),
        child_str.clone(),
        "remember".to_owned(),
        candidate_content.to_owned(),
        "--json".to_owned(),
    ])?;
    ensure(
        seeded.status.success(),
        format!(
            "seeding the child store failed: {}",
            String::from_utf8_lossy(&seeded.stdout)
        ),
    )?;
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
        best_documents == 1,
        format!("orient best nearby child must report the one seeded document, got: {best:?}"),
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

    // A populated root must omit discovery entirely even when the task has no
    // matching orient content, and must not retarget a next command away from
    // the root. This is the planted negative for the source-of-truth count:
    // retrieval emptiness must never be mistaken for store emptiness.
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
    let nonmatching_task = "zzzz_ft1z5_no_matching_orient_content_zzzz";
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
        populated_items.is_empty(),
        format!(
            "planted negative requires empty/nonmatching orient content, got: {populated_items:?}"
        ),
    )?;
    ensure(
        populated_json.pointer("/data/storeDiscovery").is_none(),
        format!(
            "a populated root must omit storeDiscovery entirely, got: {}",
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
    let init = run_ee(&[
        "init".to_owned(),
        "--workspace".to_owned(),
        docs_root.to_string_lossy().to_string(),
        "--json".to_owned(),
    ])?;
    ensure(
        init.status.success(),
        format!(
            "init beside existing agent docs must succeed: {}",
            String::from_utf8_lossy(&init.stdout)
        ),
    )?;
    let agents_after =
        std::fs::read_to_string(docs_root.join("AGENTS.md")).map_err(|error| error.to_string())?;
    let claude_after =
        std::fs::read_to_string(docs_root.join("CLAUDE.md")).map_err(|error| error.to_string())?;
    ensure(
        agents_after == agents_sentinel && claude_after == claude_sentinel,
        "ee init must preserve pre-existing AGENTS.md and CLAUDE.md byte-for-byte".to_owned(),
    )
}
