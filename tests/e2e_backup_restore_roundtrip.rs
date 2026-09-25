//! End-to-end backup → restore round-trip test for eidetic_engine_cli-534m.
//!
//! Seeds a workspace with diverse memories + tags, runs `ee backup create`
//! and `ee backup restore --side-path`, then opens both SQLite databases
//! through `DbConnection` and diffs every content-bearing memory + tag row
//! between the source workspace and the restored side-path.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use ee::db::{
    CreateGraphAlgorithmResultInput, CreateGraphAlgorithmWitnessInput, CreateGraphSnapshotInput,
    DbConnection, GraphSnapshotType, StoredMemory,
};
use serde_json::Value as JsonValue;

#[path = "support/test_tracing.rs"]
mod test_tracing;

type TestResult = Result<(), String>;
const CONTEXT_QUERY: &str = "Always run cargo fmt --check before release";
const SECRET_CANARY: &str = "api_key=cli-backup-secret-canary";
const JSONL_GRAPH_FIELDS: &[&str] = &[
    "pagerank_score",
    "betweenness_score",
    "hits_authority",
    "hits_hub",
    "onion_layer",
    "k_truss_max",
    "articulation_point",
    "bayes_alpha",
    "bayes_beta",
];

fn trace_backup_restore_roundtrip(phase: &'static str, elapsed_ms: u64, degraded_codes: &[&str]) {
    tracing::info!(
        workspace_id = "repo",
        request_id = "e2e_backup_restore_roundtrip_contract",
        bead_id = option_env!("EE_TRACE_BEAD_ID").unwrap_or("bd-bife.21"),
        surface = "e2e_backup_restore_roundtrip",
        phase,
        elapsed_ms,
        degraded_codes = ?degraded_codes,
        "backup/restore roundtrip checkpoint"
    );
}

fn ee_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ee")
}

fn run_ee_raw(args: &[&str]) -> Result<(JsonValue, Vec<u8>), String> {
    trace_backup_restore_roundtrip("input", 0, &[]);
    let output = Command::new(ee_bin())
        .args(args)
        .output()
        .map_err(|error| format!("spawn ee {}: {error}", args.join(" ")))?;

    if !output.status.success() {
        return Err(format!(
            "ee {} failed (exit {:?})\nstdout:\n{}\nstderr:\n{}",
            args.join(" "),
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        ));
    }
    let stdout = output.stdout;
    let parsed = serde_json::from_slice(&stdout)
        .map_err(|error| format!("parse JSON from ee {}: {error}", args.join(" ")))?;
    trace_backup_restore_roundtrip("response", 0, &[]);
    Ok((parsed, stdout))
}

fn run_ee(args: &[&str]) -> Result<JsonValue, String> {
    run_ee_raw(args).map(|(json, _stdout)| json)
}

fn run_ee_output(args: &[&str]) -> Result<std::process::Output, String> {
    Command::new(ee_bin())
        .args(args)
        .output()
        .map_err(|error| format!("spawn ee {}: {error}", args.join(" ")))
}

fn copy_backup_tree(src: &Path, dst: &Path) -> Result<(), String> {
    fs::create_dir_all(dst).map_err(|error| format!("mkdir {}: {error}", dst.display()))?;
    for entry in
        fs::read_dir(src).map_err(|error| format!("read_dir {}: {error}", src.display()))?
    {
        let entry = entry.map_err(|error| format!("read_dir {}: {error}", src.display()))?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        let metadata = fs::symlink_metadata(&from)
            .map_err(|error| format!("symlink_metadata {}: {error}", from.display()))?;
        if metadata.is_dir() {
            copy_backup_tree(&from, &to)?;
        } else {
            fs::copy(&from, &to)
                .map_err(|error| format!("copy {} -> {}: {error}", from.display(), to.display()))?;
        }
    }
    Ok(())
}

fn tree_must_not_contain(root: &Path, canary: &str) -> TestResult {
    fn walk(path: &Path, canary: &str, hits: &mut Vec<String>) -> TestResult {
        let metadata = fs::symlink_metadata(path)
            .map_err(|error| format!("symlink_metadata {}: {error}", path.display()))?;
        if metadata.is_dir() {
            for entry in fs::read_dir(path)
                .map_err(|error| format!("read_dir {}: {error}", path.display()))?
            {
                let entry =
                    entry.map_err(|error| format!("read_dir {}: {error}", path.display()))?;
                walk(&entry.path(), canary, hits)?;
            }
            return Ok(());
        }
        if metadata.file_type().is_symlink() {
            return Ok(());
        }
        let bytes = fs::read(path).map_err(|error| format!("read {}: {error}", path.display()))?;
        if String::from_utf8_lossy(&bytes).contains(canary) {
            hits.push(path.display().to_string());
        }
        Ok(())
    }
    let mut hits = Vec::new();
    walk(root, canary, &mut hits)?;
    ensure(
        hits.is_empty(),
        format!("secret canary leaked into {}", hits.join(", ")),
    )
}

fn verify_issue_codes(report: &JsonValue) -> Vec<String> {
    report
        .pointer("/data/issues")
        .and_then(JsonValue::as_array)
        .map(|issues| {
            issues
                .iter()
                .filter_map(|issue| issue["code"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

fn run_ee_with_passphrase(
    args: &[&str],
    passphrase: &str,
    success: bool,
) -> Result<JsonValue, String> {
    let mut child = Command::new(ee_bin())
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| error.to_string())?;
    child
        .stdin
        .take()
        .ok_or_else(|| "missing stdin pipe".to_owned())?
        .write_all(format!("{passphrase}\n").as_bytes())
        .map_err(|error| error.to_string())?;
    let output = child
        .wait_with_output()
        .map_err(|error| error.to_string())?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    ensure(
        !stdout.contains(passphrase) && !stderr.contains(passphrase),
        "passphrase leaked into command output",
    )?;
    ensure(
        output.status.success() == success,
        format!(
            "key command {args:?}: status {}\nstdout: {stdout}\nstderr: {stderr}",
            output.status
        ),
    )?;
    let json: JsonValue = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("key command JSON: {error}\n{stdout}"))?;
    ensure_equal(
        &json["schema"],
        &serde_json::json!(if success {
            "ee.response.v2"
        } else {
            "ee.error.v2"
        }),
        "key command envelope",
    )?;
    if success {
        ensure_equal(
            &json["success"],
            &serde_json::json!(true),
            "key command success",
        )?;
    }
    Ok(json)
}

#[test]
fn encrypted_key_recovery_restores_backup_without_source_workspace() -> TestResult {
    use ee::policy::store_auth::{StoreAuthRoot, workspace_keys_dir};
    let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
    let base = temp
        .path()
        .canonicalize()
        .map_err(|error| error.to_string())?;
    let source = base.join("source");
    let auth = base.join("recovered-auth");
    let restored = base.join("restored");
    let exported = base.join("encrypted-keys");
    let envelope_path = exported.join("store-auth.recovery.json");
    let source_arg = source.to_string_lossy();
    let auth_arg = auth.to_string_lossy();
    let output_arg = exported.to_string_lossy();
    let input_arg = envelope_path.to_string_lossy();
    let passphrase = "synthetic portable keys passphrase 123";
    let content =
        "For the tangerine compass release, verify the signed artifact before deployment.";
    fs::create_dir(&source).map_err(|error| error.to_string())?;
    run_ee(&["init", "--workspace", &source_arg, "--json"])?;
    let remembered = run_ee(&[
        "remember",
        content,
        "--level",
        "procedural",
        "--kind",
        "rule",
        "--workspace",
        &source_arg,
        "--json",
    ])?;
    let original_id = json_str(&remembered, "/data/memory_id", "remember")?;
    let backup = run_ee(&[
        "backup",
        "create",
        "--output-dir",
        &base.join("data-backup").to_string_lossy(),
        "--redaction",
        "none",
        "--include-graph-cache=false",
        "--workspace",
        &source_arg,
        "--json",
    ])?;
    let backup_path = json_str(&backup, "/data/backupPath", "backup")?;
    // The backup's signing key must survive as a retired key in the envelope.
    let mut root =
        StoreAuthRoot::open(workspace_keys_dir(&source)).map_err(|error| error.to_string())?;
    root.rotate().map_err(|error| error.to_string())?;
    let key_ids: Vec<_> = root.window_key_ids().iter().map(|id| id.to_hex()).collect();
    ensure_equal(&key_ids.len(), &2, "rotated window")?;
    let source_keys = fs::read(workspace_keys_dir(&source).join("store_auth_root.json"))
        .map_err(|error| error.to_string())?;
    let preview = run_ee_with_passphrase(
        &[
            "backup",
            "keys",
            "export",
            "--output-dir",
            &output_arg,
            "--passphrase-stdin",
            "--dry-run",
            "--workspace",
            &source_arg,
            "--json",
        ],
        passphrase,
        true,
    )?;
    ensure_equal(
        &preview["data"]["persisted"],
        &serde_json::json!(false),
        "export preview",
    )?;
    ensure(!exported.exists(), "export preview created its destination")?;
    let exported_json = run_ee_with_passphrase(
        &[
            "backup",
            "keys",
            "export",
            "--output-dir",
            &output_arg,
            "--passphrase-stdin",
            "--workspace",
            &source_arg,
            "--json",
        ],
        passphrase,
        true,
    )?;
    ensure_equal(
        &exported_json["data"]["keyIds"],
        &serde_json::json!(key_ids),
        "export key window",
    )?;
    run_ee_with_passphrase(
        &[
            "backup",
            "keys",
            "export",
            "--output-dir",
            &output_arg,
            "--passphrase-stdin",
            "--workspace",
            &source_arg,
            "--json",
        ],
        passphrase,
        false,
    )?;
    ensure_equal(
        &fs::read(workspace_keys_dir(&source).join("store_auth_root.json"))
            .map_err(|error| error.to_string())?,
        &source_keys,
        "export leaves keys unchanged",
    )?;
    let envelope = fs::read(&envelope_path).map_err(|error| error.to_string())?;
    let key_doc: JsonValue =
        serde_json::from_slice(&source_keys).map_err(|error| error.to_string())?;
    let public_output = format!("{} {exported_json}", String::from_utf8_lossy(&envelope));
    for key in ["/current/root", "/retired/0/root"] {
        ensure(
            !public_output.contains(json_str(&key_doc, key, "source key")?),
            "plaintext key leaked into encrypted export or report",
        )?;
    }
    ensure(
        !public_output.contains(passphrase),
        "passphrase leaked into envelope",
    )?;
    drop(root);
    fs::rename(&source, base.join("offline-source")).map_err(|error| error.to_string())?;
    let absent = Command::new(ee_bin())
        .args([
            "backup",
            "verify",
            backup_path,
            "--workspace",
            &auth_arg,
            "--json",
        ])
        .output()
        .map_err(|error| error.to_string())?;
    ensure(
        !absent.status.success(),
        "backup verified without trusted keys",
    )?;
    ensure(
        !auth.exists(),
        "failed verification created recovery workspace",
    )?;
    let import_args = [
        "backup",
        "keys",
        "import",
        "--input",
        &input_arg,
        "--passphrase-stdin",
        "--workspace",
        &auth_arg,
        "--json",
    ];
    run_ee_with_passphrase(&import_args, "incorrect portable keys passphrase", false)?;
    ensure(!auth.exists(), "wrong passphrase mutated destination")?;
    let mut preview_args = import_args.to_vec();
    preview_args.push("--dry-run");
    let preview = run_ee_with_passphrase(&preview_args, passphrase, true)?;
    ensure_equal(
        &preview["data"]["persisted"],
        &serde_json::json!(false),
        "import preview",
    )?;
    ensure(!auth.exists(), "import preview created destination")?;
    let imported = run_ee_with_passphrase(&import_args, passphrase, true)?;
    ensure_equal(
        &imported["data"]["keyIds"],
        &serde_json::json!(key_ids),
        "import key window",
    )?;
    let key_path = workspace_keys_dir(&auth).join("store_auth_root.json");
    ensure_equal(
        &fs::read(&key_path).map_err(|error| error.to_string())?,
        &source_keys,
        "recovered exact authentication keys",
    )?;
    ensure(
        !auth.join(".ee/ee.db").exists(),
        "key recovery must not initialize a database",
    )?;
    run_ee_with_passphrase(&import_args, passphrase, false)?;
    ensure_equal(
        &fs::read(&key_path).map_err(|error| error.to_string())?,
        &source_keys,
        "repeated import preserves keys",
    )?;
    ensure_equal(
        &fs::read(&envelope_path).map_err(|error| error.to_string())?,
        &envelope,
        "import preserves envelope",
    )?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        ensure_equal(
            &(fs::metadata(&key_path)
                .map_err(|error| error.to_string())?
                .permissions()
                .mode()
                & 0o777),
            &0o600,
            "key file mode",
        )?;
        ensure_equal(
            &(fs::metadata(workspace_keys_dir(&auth))
                .map_err(|error| error.to_string())?
                .permissions()
                .mode()
                & 0o777),
            &0o700,
            "key directory mode",
        )?;
    }
    run_ee(&[
        "backup",
        "verify",
        backup_path,
        "--workspace",
        &auth_arg,
        "--json",
    ])?;
    run_ee(&[
        "backup",
        "restore",
        backup_path,
        "--workspace",
        &auth_arg,
        "--side-path",
        &restored.to_string_lossy(),
        "--json",
    ])?;
    let search = run_ee(&[
        "search",
        "tangerine compass release",
        "--source-mode",
        "lexical_only",
        "--strict-source-mode",
        "--workspace",
        &restored.to_string_lossy(),
        "--json",
    ])?;
    ensure(
        search
            .pointer("/data/results")
            .and_then(JsonValue::as_array)
            .is_some_and(|hits| hits.iter().any(|hit| hit["docId"] == original_id)),
        format!("recovered memory not searchable (original memory {original_id}): {search}"),
    )?;
    let connection =
        DbConnection::open_file(restored.join(".ee/ee.db")).map_err(|error| error.to_string())?;
    let memory = connection
        .get_memory(original_id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "restored memory missing".to_owned())?;
    ensure_equal(&memory.content.as_str(), &content, "restored content")?;
    ensure(
        !source.exists(),
        "restoration unexpectedly recreated original workspace",
    )
}

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn ensure_equal<T: std::fmt::Debug + PartialEq>(actual: &T, expected: &T, ctx: &str) -> TestResult {
    if actual == expected {
        Ok(())
    } else {
        Err(format!("{ctx}: expected {expected:?}, got {actual:?}"))
    }
}

/// Content-addressed inventory of every file and empty directory under `root`.
///
/// Dry-run must not create a marker, lock, WAL sidecar, key file, or empty
/// output directory. A digest over relative paths plus contents is the
/// acceptance instrument (bd-reality-core-convergence-1azkt.13).
fn workspace_file_digests(root: &Path) -> Result<BTreeMap<String, String>, String> {
    let mut out = BTreeMap::new();
    collect_workspace_file_digests(root, root, &mut out)?;
    Ok(out)
}

fn collect_workspace_file_digests(
    root: &Path,
    dir: &Path,
    out: &mut BTreeMap<String, String>,
) -> Result<(), String> {
    let mut entries = fs::read_dir(dir)
        .map_err(|error| format!("read_dir {}: {error}", dir.display()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("read_dir {}: {error}", dir.display()))?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        let rel = path
            .strip_prefix(root)
            .map_err(|error| format!("strip prefix {}: {error}", path.display()))?
            .to_string_lossy()
            .replace('\\', "/");
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| format!("symlink_metadata {}: {error}", path.display()))?;
        if metadata.file_type().is_symlink() {
            let target = fs::read_link(&path)
                .map_err(|error| format!("read_link {}: {error}", path.display()))?;
            out.insert(
                rel,
                format!("symlink:{}", target.to_string_lossy().replace('\\', "/")),
            );
        } else if metadata.is_dir() {
            out.insert(format!("{rel}/"), "dir".to_owned());
            collect_workspace_file_digests(root, &path, out)?;
        } else {
            let bytes =
                fs::read(&path).map_err(|error| format!("read {}: {error}", path.display()))?;
            out.insert(rel, format!("blake3:{}", blake3::hash(&bytes).to_hex()));
        }
    }
    Ok(())
}

fn workspace_store_digest(files: &BTreeMap<String, String>) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"ee.backup.real_store_digest.v1\0");
    for (path, digest) in files {
        hasher.update(&(path.len() as u64).to_le_bytes());
        hasher.update(path.as_bytes());
        hasher.update(&(digest.len() as u64).to_le_bytes());
        hasher.update(digest.as_bytes());
    }
    format!("blake3:{}", hasher.finalize().to_hex())
}

fn ensure_store_unchanged(
    label: &str,
    before: &BTreeMap<String, String>,
    after: &BTreeMap<String, String>,
) -> TestResult {
    if before == after {
        return Ok(());
    }
    let mut diffs = Vec::new();
    for path in before.keys().chain(after.keys()) {
        if diffs.len() >= 12 {
            break;
        }
        if before.get(path) == after.get(path) {
            continue;
        }
        diffs.push(format!(
            "{path}: before={} after={}",
            before.get(path).map_or("-", String::as_str),
            after.get(path).map_or("-", String::as_str)
        ));
    }
    Err(format!(
        "{label} mutated the real store (digest {} -> {}); named paths: {}",
        workspace_store_digest(before),
        workspace_store_digest(after),
        diffs.join("; ")
    ))
}

fn json_brief(value: &JsonValue) -> String {
    let rendered = value.to_string();
    if rendered.len() <= 160 {
        rendered
    } else {
        format!("{}...", &rendered[..160])
    }
}

fn collect_json_differences(
    expected: &JsonValue,
    actual: &JsonValue,
    pointer: &str,
    diffs: &mut Vec<String>,
) {
    if diffs.len() >= 8 {
        return;
    }
    match (expected, actual) {
        (JsonValue::Object(expected_object), JsonValue::Object(actual_object)) => {
            let mut keys = std::collections::BTreeSet::new();
            keys.extend(expected_object.keys());
            keys.extend(actual_object.keys());
            for key in keys {
                if diffs.len() >= 8 {
                    return;
                }
                let child_pointer = format!("{pointer}/{key}");
                match (expected_object.get(key), actual_object.get(key)) {
                    (Some(expected_child), Some(actual_child)) => collect_json_differences(
                        expected_child,
                        actual_child,
                        &child_pointer,
                        diffs,
                    ),
                    (Some(expected_child), None) => diffs.push(format!(
                        "{child_pointer}: expected {}, got <missing>",
                        json_brief(expected_child)
                    )),
                    (None, Some(actual_child)) => diffs.push(format!(
                        "{child_pointer}: expected <missing>, got {}",
                        json_brief(actual_child)
                    )),
                    (None, None) => {}
                }
            }
        }
        (JsonValue::Array(expected_array), JsonValue::Array(actual_array)) => {
            if expected_array.len() != actual_array.len() {
                diffs.push(format!(
                    "{pointer}: expected array len {}, got {}",
                    expected_array.len(),
                    actual_array.len()
                ));
            }
            for (index, (expected_child, actual_child)) in
                expected_array.iter().zip(actual_array.iter()).enumerate()
            {
                if diffs.len() >= 8 {
                    return;
                }
                collect_json_differences(
                    expected_child,
                    actual_child,
                    &format!("{pointer}/{index}"),
                    diffs,
                );
            }
        }
        _ if expected != actual => diffs.push(format!(
            "{pointer}: expected {}, got {}",
            json_brief(expected),
            json_brief(actual)
        )),
        _ => {}
    }
}

fn ensure_context_json_bytes_equal(
    actual: &JsonValue,
    expected: &JsonValue,
    actual_stdout: &[u8],
    expected_stdout: &[u8],
    ctx: &str,
) -> TestResult {
    if actual_stdout == expected_stdout {
        return Ok(());
    }
    let mut diffs = Vec::new();
    collect_json_differences(expected, actual, "", &mut diffs);
    Err(format!(
        "{ctx}: expected {} bytes blake3:{}, got {} bytes blake3:{}; first JSON differences: {}",
        expected_stdout.len(),
        blake3::hash(expected_stdout).to_hex(),
        actual_stdout.len(),
        blake3::hash(actual_stdout).to_hex(),
        diffs.join(" | "),
    ))
}

fn remove_json_pointer(value: &mut JsonValue, pointer: &str) {
    let Some((parent_pointer, field)) = pointer.rsplit_once('/') else {
        return;
    };
    if field.is_empty() {
        return;
    }
    if let Some(parent) = value.pointer_mut(parent_pointer)
        && let Some(object) = parent.as_object_mut()
    {
        object.remove(field);
    }
}

fn canonical_context_stdout(mut value: JsonValue) -> Result<Vec<u8>, String> {
    if !ee::obs::normalize_pack_slo_measurements(&mut value)? {
        return Err("backup context output missing producer SLO measurements".to_owned());
    }
    // A side-path restore is a new store. Trust is deliberately capped at the
    // transport boundary, index publication may select a different available
    // embedding backend, and graph source generations are store-local. Those
    // values legitimately change the rendered text/hash and degradation prose;
    // the stable selection, provenance, explanations, and pack structure must
    // still match.
    for pointer in [
        "/data/degraded",
        "/degraded",
        "/data/embed_backend",
        "/data/pack/hash",
        "/data/pack/text",
    ] {
        remove_json_pointer(&mut value, pointer);
    }
    if let Some(items) = value
        .pointer_mut("/data/pack/items")
        .and_then(JsonValue::as_array_mut)
    {
        for item in items {
            if let Some(object) = item.as_object_mut() {
                object.remove("trust");
            }
        }
    }
    serde_json::to_vec(&value).map_err(|error| format!("canonicalize context JSON: {error}"))
}

#[test]
fn context_comparison_requires_producer_slo() {
    let missing_slo = serde_json::json!({
        "schema": "ee.response.v2",
        "success": true,
        "data": {"pack": {"schema": "ee.pack.v2", "items": []}}
    });
    assert_eq!(
        canonical_context_stdout(missing_slo),
        Err("backup context output missing producer SLO measurements".to_owned())
    );
}

fn json_str<'a>(value: &'a JsonValue, pointer: &str, context: &str) -> Result<&'a str, String> {
    value
        .pointer(pointer)
        .and_then(JsonValue::as_str)
        .ok_or_else(|| format!("{context}: missing string at {pointer}"))
}

fn json_u64(value: &JsonValue, pointer: &str, context: &str) -> Result<u64, String> {
    value
        .pointer(pointer)
        .and_then(JsonValue::as_u64)
        .ok_or_else(|| format!("{context}: missing integer at {pointer}"))
}

fn context_item_contents(report: &JsonValue, context: &str) -> Result<Vec<String>, String> {
    report
        .pointer("/data/pack/items")
        .and_then(JsonValue::as_array)
        .ok_or_else(|| format!("{context}: missing context pack items"))?
        .iter()
        .map(|item| {
            item.pointer("/content")
                .and_then(JsonValue::as_str)
                .map(str::to_owned)
                .ok_or_else(|| format!("{context}: item missing content"))
        })
        .collect()
}

fn enable_ppr_context_feature(workspace_path: &Path) -> TestResult {
    let metadata_dir = workspace_path.join(".ee");
    fs::create_dir_all(&metadata_dir)
        .map_err(|error| format!("create PPR fixture config dir: {error}"))?;
    fs::write(
        metadata_dir.join("config.toml"),
        "[graph.feature.ppr]\nenabled = true\n",
    )
    .map_err(|error| format!("write PPR fixture config: {error}"))
}

fn artifact_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("e2e_backup_restore_roundtrip_artifacts")
}

fn persist_json_artifact(name: &str, value: &JsonValue) -> TestResult {
    trace_backup_restore_roundtrip("persistence", 0, &[]);
    let dir = artifact_dir();
    fs::create_dir_all(&dir).map_err(|error| format!("mkdir artifact dir: {error}"))?;
    let path = dir.join(format!("{name}.json"));
    let mut bytes = serde_json::to_vec_pretty(value)
        .map_err(|error| format!("render artifact {name}: {error}"))?;
    bytes.push(b'\n');
    fs::write(&path, bytes).map_err(|error| format!("write artifact {}: {error}", path.display()))
}

fn read_jsonl_records(path: &Path) -> Result<Vec<JsonValue>, String> {
    let input = fs::read_to_string(path)
        .map_err(|error| format!("read JSONL {}: {error}", path.display()))?;
    input
        .lines()
        .enumerate()
        .filter_map(|(index, line)| {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(serde_json::from_str::<JsonValue>(trimmed).map_err(|error| {
                    format!("parse JSONL {} line {}: {error}", path.display(), index + 1)
                }))
            }
        })
        .collect()
}

fn records_with_schema(records: &[JsonValue], schema: &str) -> Vec<JsonValue> {
    records
        .iter()
        .filter(|record| record.get("schema").and_then(JsonValue::as_str) == Some(schema))
        .cloned()
        .collect()
}

fn ensure_memory_records_include_graph_fields(records: &[JsonValue], context: &str) -> TestResult {
    let memories = records_with_schema(records, "ee.export.memory.v1");
    ensure(
        !memories.is_empty(),
        format!("{context}: no memory records"),
    )?;
    let mut observed_fields = std::collections::BTreeSet::new();
    for record in &memories {
        let memory_id = record
            .get("memory_id")
            .and_then(JsonValue::as_str)
            .unwrap_or("<missing-memory-id>");
        for field in JSONL_GRAPH_FIELDS {
            let Some(value) = record.get(*field) else {
                if *field == "k_truss_max" {
                    continue;
                }
                return Err(format!(
                    "{context}: memory {memory_id} missing graph-derived field {field}"
                ));
            };
            observed_fields.insert(*field);
            let valid_type = if *field == "articulation_point" {
                value.as_bool().is_some()
            } else {
                value.is_number()
            };
            ensure(
                valid_type,
                format!(
                    "{context}: memory {memory_id} graph-derived field {field} has invalid type: {value}"
                ),
            )?;
        }
    }
    for field in JSONL_GRAPH_FIELDS {
        ensure(
            observed_fields.contains(field),
            format!("{context}: no memory carried evidence-backed graph field {field}"),
        )?;
    }
    Ok(())
}

fn normalized_records_with_schema(
    records: &[JsonValue],
    schema: &str,
    ignored_fields: &[&str],
) -> Vec<JsonValue> {
    let mut normalized = records_with_schema(records, schema)
        .into_iter()
        .map(|mut record| {
            if let JsonValue::Object(object) = &mut record {
                for field in ignored_fields {
                    object.remove(*field);
                }
            }
            record
        })
        .collect::<Vec<_>>();
    normalized.sort_by_key(JsonValue::to_string);
    normalized
}

fn records_path_from_report(report: &JsonValue, context: &str) -> Result<PathBuf, String> {
    json_str(report, "/data/recordsPath", context).map(PathBuf::from)
}

fn workspace_id_from_db(conn: &DbConnection, workspace_path: &Path) -> Result<String, String> {
    let canonical = workspace_path
        .canonicalize()
        .unwrap_or_else(|_| workspace_path.to_path_buf());
    let path_str = canonical.to_string_lossy().into_owned();
    if let Some(workspace) = conn
        .get_workspace_by_path(&path_str)
        .map_err(|error| format!("get_workspace_by_path: {error}"))?
    {
        return Ok(workspace.id);
    }
    // Fall back to whichever workspace lives in the DB; backup/restore both
    // emit at most one workspace row.
    let workspaces = conn
        .list_workspaces()
        .map_err(|error| format!("list_workspaces: {error}"))?;
    workspaces
        .into_iter()
        .next()
        .map(|w| w.id)
        .ok_or_else(|| "no workspace row present in database".to_owned())
}

/// Memory fields that must survive authenticated, unredacted backup recovery,
/// including the original chronology, provenance and trust decisions. The
/// recovery audit is separate evidence and must not rewrite these fields.
#[derive(Clone, Debug, PartialEq)]
struct MemoryContent {
    level: String,
    kind: String,
    content: String,
    confidence_milli: i64, // milli units to compare floats deterministically
    utility_milli: i64,
    importance_milli: i64,
    created_at: String,
    updated_at: String,
    provenance_uri: Option<String>,
    trust_class: String,
    trust_subclass: Option<String>,
    tombstoned: bool,
    tombstoned_at: Option<String>,
    valid_from: Option<String>,
    valid_to: Option<String>,
}

fn milli(value: f32) -> i64 {
    (f64::from(value) * 1000.0).round() as i64
}

impl From<&StoredMemory> for MemoryContent {
    fn from(memory: &StoredMemory) -> Self {
        Self {
            level: memory.level.clone(),
            kind: memory.kind.clone(),
            content: memory.content.clone(),
            confidence_milli: milli(memory.confidence),
            utility_milli: milli(memory.utility),
            importance_milli: milli(memory.importance),
            created_at: memory.created_at.clone(),
            updated_at: memory.updated_at.clone(),
            provenance_uri: memory.provenance_uri.clone(),
            trust_class: memory.trust_class.clone(),
            trust_subclass: memory.trust_subclass.clone(),
            tombstoned: memory.tombstoned_at.is_some(),
            tombstoned_at: memory.tombstoned_at.clone(),
            valid_from: memory.valid_from.clone(),
            valid_to: memory.valid_to.clone(),
        }
    }
}

fn memory_with_tags(
    conn: &DbConnection,
    memory: &StoredMemory,
) -> Result<(MemoryContent, Vec<String>), String> {
    let tags = conn
        .get_memory_tags(&memory.id)
        .map_err(|error| format!("get_memory_tags({}): {error}", memory.id))?;
    Ok((MemoryContent::from(memory), tags))
}

#[test]
fn backup_verify_rejects_modified_inventory_with_nonzero_exit() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
    let workspace = tempdir.path().join("workspace");
    fs::create_dir(&workspace).map_err(|error| error.to_string())?;
    let ws = workspace.to_string_lossy();
    run_ee(&["init", "--workspace", &ws, "--json"])?;
    run_ee(&[
        "remember",
        "Keep release verification evidence.",
        "--workspace",
        &ws,
        "--json",
    ])?;
    let created = run_ee(&[
        "backup",
        "create",
        "--include-graph-cache=false",
        "--workspace",
        &ws,
        "--json",
    ])?;
    let backup_path = json_str(&created, "/data/backupPath", "created backup")?;
    let verified = run_ee(&[
        "backup",
        "verify",
        backup_path,
        "--workspace",
        &ws,
        "--json",
    ])?;
    ensure_equal(
        &verified.pointer("/data/status").and_then(JsonValue::as_str),
        &Some("verified"),
        "unaltered signed backup passes public verification",
    )?;
    let manifest_path = Path::new(backup_path).join("manifest.json");
    let mut manifest: JsonValue =
        serde_json::from_slice(&fs::read(&manifest_path).map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())?;
    manifest["artifacts"] = serde_json::json!([]);
    fs::write(
        &manifest_path,
        serde_json::to_vec(&manifest).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    let rejected = Command::new(ee_bin())
        .args([
            "backup",
            "verify",
            backup_path,
            "--workspace",
            &ws,
            "--json",
        ])
        .output()
        .map_err(|error| error.to_string())?;
    eprintln!(
        "verify stdout: {}\nverify stderr: {}",
        String::from_utf8_lossy(&rejected.stdout),
        String::from_utf8_lossy(&rejected.stderr)
    );
    ensure_equal(
        &rejected.status.code(),
        &Some(5),
        "invalid inventory must fail shell verification",
    )?;
    let report: JsonValue =
        serde_json::from_slice(&rejected.stdout).map_err(|error| error.to_string())?;
    ensure_equal(
        &report["success"].as_bool(),
        &Some(false),
        "failed verification envelope",
    )?;
    ensure_equal(
        &report.pointer("/data/status").and_then(JsonValue::as_str),
        &Some("failed"),
        "failed verification status",
    )?;
    ensure(
        report
            .pointer("/data/issues")
            .and_then(JsonValue::as_array)
            .is_some_and(|issues| {
                issues
                    .iter()
                    .any(|issue| issue["code"] == "manifest_authentication_failed")
            }),
        "public verification reports the manifest authentication defect",
    )?;
    let side_path = tempdir.path().join("rejected-restore");
    let restored = Command::new(ee_bin())
        .args([
            "backup",
            "restore",
            backup_path,
            "--side-path",
            &side_path.to_string_lossy(),
            "--workspace",
            &ws,
            "--json",
        ])
        .output()
        .map_err(|error| error.to_string())?;
    eprintln!(
        "restore stdout: {}\nrestore stderr: {}",
        String::from_utf8_lossy(&restored.stdout),
        String::from_utf8_lossy(&restored.stderr)
    );
    ensure_equal(
        &restored.status.code(),
        &Some(5),
        "invalid inventory must fail restore",
    )?;
    ensure(
        !side_path.exists(),
        "rejected restore must not create its destination",
    )
}

#[test]
fn backup_verify_and_restore_reject_real_store_tamper_matrix() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
    let workspace = tempdir.path().join("workspace");
    fs::create_dir(&workspace).map_err(|error| error.to_string())?;
    let ws = workspace.to_string_lossy().into_owned();
    run_ee(&["init", "--workspace", &ws, "--json"])?;
    run_ee(&[
        "remember",
        "Keep release verification evidence.",
        "--workspace",
        &ws,
        "--json",
    ])?;
    run_ee(&[
        "remember",
        "Second memory so records.jsonl has more than one body line.",
        "--workspace",
        &ws,
        "--json",
    ])?;
    let created = run_ee(&[
        "backup",
        "create",
        "--include-graph-cache=false",
        "--workspace",
        &ws,
        "--json",
    ])?;
    let original_backup = PathBuf::from(json_str(&created, "/data/backupPath", "created backup")?);
    let other = tempdir.path().join("other-workspace");
    fs::create_dir(&other).map_err(|error| error.to_string())?;
    let other_ws = other.to_string_lossy().into_owned();
    run_ee(&["init", "--workspace", &other_ws, "--json"])?;

    struct Case {
        name: &'static str,
        expected_code: &'static str,
        verify_workspace: String,
        tamper: fn(&Path) -> Result<(), String>,
    }

    let cases = [
        Case {
            name: "missing-records",
            expected_code: "artifact_missing",
            verify_workspace: ws.clone(),
            tamper: |backup| {
                fs::remove_file(backup.join("records.jsonl"))
                    .map_err(|error| format!("remove records.jsonl: {error}"))
            },
        },
        Case {
            name: "truncated-records",
            expected_code: "artifact_size_mismatch",
            verify_workspace: ws.clone(),
            tamper: |backup| {
                let path = backup.join("records.jsonl");
                let bytes = fs::read(&path).map_err(|error| format!("read records: {error}"))?;
                ensure(bytes.len() > 16, "records.jsonl too small to truncate")?;
                fs::write(&path, &bytes[..bytes.len() / 2])
                    .map_err(|error| format!("truncate records: {error}"))
            },
        },
        Case {
            name: "substituted-records",
            expected_code: "artifact_hash_mismatch",
            verify_workspace: ws.clone(),
            tamper: |backup| {
                let path = backup.join("records.jsonl");
                let mut bytes =
                    fs::read(&path).map_err(|error| format!("read records: {error}"))?;
                let last = bytes
                    .last_mut()
                    .ok_or_else(|| "records.jsonl is empty".to_owned())?;
                *last ^= 0x5a;
                fs::write(&path, bytes).map_err(|error| format!("substitute records: {error}"))
            },
        },
        Case {
            name: "reordered-records",
            expected_code: "artifact_hash_mismatch",
            verify_workspace: ws.clone(),
            tamper: |backup| {
                let path = backup.join("records.jsonl");
                let text = fs::read_to_string(&path)
                    .map_err(|error| format!("read records text: {error}"))?;
                let mut lines: Vec<&str> = text.lines().collect();
                ensure(
                    lines.len() >= 2,
                    "records.jsonl must have at least two lines to reorder",
                )?;
                lines.swap(0, 1);
                let mut reordered = lines.join("\n");
                if text.ends_with('\n') {
                    reordered.push('\n');
                }
                fs::write(&path, reordered).map_err(|error| format!("reorder records: {error}"))
            },
        },
        Case {
            name: "wrong-workspace",
            expected_code: "manifest_authentication_failed",
            verify_workspace: other_ws,
            tamper: |_| Ok(()),
        },
    ];

    for case in cases {
        let backup = tempdir.path().join(case.name);
        copy_backup_tree(&original_backup, &backup)?;
        (case.tamper)(&backup)?;
        let backup_arg = backup.to_string_lossy().into_owned();
        let verified = run_ee_output(&[
            "backup",
            "verify",
            &backup_arg,
            "--workspace",
            &case.verify_workspace,
            "--json",
        ])?;
        ensure_equal(
            &verified.status.code(),
            &Some(5),
            &format!("{} verify exit", case.name),
        )?;
        let report: JsonValue = serde_json::from_slice(&verified.stdout).map_err(|error| {
            format!(
                "{} verify JSON: {error}\n{}",
                case.name,
                String::from_utf8_lossy(&verified.stdout)
            )
        })?;
        ensure_equal(
            &report["success"].as_bool(),
            &Some(false),
            &format!("{} verify envelope", case.name),
        )?;
        let codes = verify_issue_codes(&report);
        ensure(
            codes.iter().any(|code| {
                if case.expected_code == "manifest_authentication_failed" {
                    code.starts_with("manifest_authentication")
                } else {
                    code == case.expected_code
                }
            }),
            format!(
                "{} expected issue {}, got {codes:?}",
                case.name, case.expected_code
            ),
        )?;
        let side_path = tempdir.path().join(format!("{}-restore", case.name));
        let restored = run_ee_output(&[
            "backup",
            "restore",
            &backup_arg,
            "--side-path",
            &side_path.to_string_lossy(),
            "--workspace",
            &case.verify_workspace,
            "--json",
        ])?;
        ensure(
            restored.status.code() != Some(0),
            format!("{} restore must fail", case.name),
        )?;
        ensure(
            !side_path.exists(),
            format!("{} restore must not create its destination", case.name),
        )?;
    }
    Ok(())
}

#[test]
fn backup_list_does_not_accept_create_staging_debris() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
    let workspace = tempdir.path().join("workspace");
    fs::create_dir(&workspace).map_err(|error| error.to_string())?;
    let backup_dir = tempdir.path().join("backups");
    let ws = workspace.to_string_lossy().into_owned();
    let backup_dir_arg = backup_dir.to_string_lossy().into_owned();
    run_ee(&["init", "--workspace", &ws, "--json"])?;
    run_ee(&[
        "remember",
        "Atomic create must not list unpublished staging.",
        "--workspace",
        &ws,
        "--json",
    ])?;
    let created = run_ee(&[
        "backup",
        "create",
        "--include-graph-cache=false",
        "--output-dir",
        &backup_dir_arg,
        "--workspace",
        &ws,
        "--json",
    ])?;
    let backup_id = json_str(&created, "/data/backupId", "created backup")?;
    let backup_path = PathBuf::from(json_str(&created, "/data/backupPath", "created backup")?);
    let debris = backup_dir.join(".ee-backup-debris-fixture");
    copy_backup_tree(&backup_path, &debris)?;
    ensure(
        debris.join("manifest.json").is_file(),
        "planted staging debris has a complete manifest",
    )?;
    let listed = run_ee(&[
        "backup",
        "list",
        "--output-dir",
        &backup_dir_arg,
        "--workspace",
        &ws,
        "--json",
    ])?;
    let backups = listed
        .pointer("/data/backups")
        .and_then(JsonValue::as_array)
        .ok_or_else(|| "backup list missing data.backups".to_owned())?;
    ensure_equal(
        &backups.len(),
        &1usize,
        "list accepts only the published backup",
    )?;
    let listed_backup = backups
        .first()
        .ok_or_else(|| "backup list missing published entry".to_owned())?;
    ensure_equal(
        &listed_backup.get("backupId").and_then(JsonValue::as_str),
        &Some(backup_id),
        "listed backup id",
    )?;
    let listed_path = listed_backup
        .get("backupPath")
        .and_then(JsonValue::as_str)
        .unwrap_or_default();
    ensure(
        !listed_path.contains(".ee-backup-"),
        format!("list accepted staging path {listed_path}"),
    )?;
    let names: Vec<_> = fs::read_dir(&backup_dir)
        .map_err(|error| error.to_string())?
        .map(|entry| entry.map(|entry| entry.file_name().to_string_lossy().into_owned()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    ensure(
        names.iter().any(|name| name == ".ee-backup-debris-fixture"),
        "planted staging debris remains on disk",
    )?;
    Ok(())
}

#[test]
fn backup_restore_roundtrips_pack_history_and_query_surfaces() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
    let workspace = tempdir.path().join("workspace");
    let backup_dir = tempdir.path().join("backups");
    let side_path = tempdir.path().join("restored");
    fs::create_dir(&workspace).map_err(|error| error.to_string())?;
    let ws = workspace.to_string_lossy().into_owned();
    let backup_dir_arg = backup_dir.to_string_lossy().into_owned();
    let side_path_arg = side_path.to_string_lossy().into_owned();
    run_ee(&["init", "--workspace", &ws, "--json"])?;
    let remembered = run_ee(&[
        "remember",
        CONTEXT_QUERY,
        "--level",
        "procedural",
        "--kind",
        "rule",
        "--workspace",
        &ws,
        "--json",
    ])?;
    let memory_id = json_str(&remembered, "/data/memory_id", "remember")?;
    run_ee(&[
        "remember",
        "FrankenSQLite is the durable source of truth.",
        "--level",
        "semantic",
        "--kind",
        "fact",
        "--workspace",
        &ws,
        "--json",
    ])?;
    let packed = run_ee(&["pack", CONTEXT_QUERY, "--workspace", &ws, "--json"])?;
    ensure_equal(
        &packed.pointer("/success").and_then(JsonValue::as_bool),
        &Some(true),
        "source pack succeeded",
    )?;
    let created = run_ee(&[
        "backup",
        "create",
        "--include-graph-cache=false",
        "--redaction",
        "none",
        "--output-dir",
        &backup_dir_arg,
        "--workspace",
        &ws,
        "--json",
    ])?;
    let backup_id = json_str(&created, "/data/backupId", "created backup")?;
    let restored = run_ee(&[
        "backup",
        "restore",
        backup_id,
        "--output-dir",
        &backup_dir_arg,
        "--side-path",
        &side_path_arg,
        "--workspace",
        &ws,
        "--json",
    ])?;
    ensure_equal(
        &restored
            .pointer("/data/counts/memoriesImported")
            .and_then(JsonValue::as_u64),
        &Some(2),
        "restored both memories",
    )?;
    let pack_records = restored
        .pointer("/data/counts/packHistoryRestored/records")
        .and_then(JsonValue::as_u64)
        .unwrap_or(0);
    ensure(
        pack_records >= 1,
        format!("expected restored pack history, got {pack_records} records"),
    )?;
    let searched = run_ee(&[
        "search",
        CONTEXT_QUERY,
        "--workspace",
        &side_path_arg,
        "--json",
    ])?;
    ensure_equal(
        &searched.pointer("/success").and_then(JsonValue::as_bool),
        &Some(true),
        "restored search succeeded",
    )?;
    let why = run_ee(&["why", memory_id, "--workspace", &side_path_arg, "--json"])?;
    ensure_equal(
        &why.pointer("/success").and_then(JsonValue::as_bool),
        &Some(true),
        "restored why succeeded",
    )?;
    let restored_pack = run_ee(&[
        "pack",
        CONTEXT_QUERY,
        "--workspace",
        &side_path_arg,
        "--json",
    ])?;
    ensure_equal(
        &restored_pack
            .pointer("/success")
            .and_then(JsonValue::as_bool),
        &Some(true),
        "restored pack succeeded",
    )?;
    Ok(())
}

#[test]
fn backup_restore_roundtrips_cli_families_and_redacts_secrets() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
    let workspace = tempdir.path().join("workspace");
    let backup_dir = tempdir.path().join("backups");
    let side_path = tempdir.path().join("restored");
    fs::create_dir(&workspace).map_err(|error| error.to_string())?;
    let ws = workspace.to_string_lossy().into_owned();
    let backup_dir_arg = backup_dir.to_string_lossy().into_owned();
    let side_path_arg = side_path.to_string_lossy().into_owned();
    run_ee(&["init", "--workspace", &ws, "--json"])?;
    let remembered = run_ee(&[
        "remember",
        CONTEXT_QUERY,
        "--level",
        "procedural",
        "--kind",
        "rule",
        "--workspace",
        &ws,
        "--json",
    ])?;
    let memory_id = json_str(&remembered, "/data/memory_id", "remember rule")?;
    run_ee(&[
        "remember",
        "FrankenSQLite is the durable source of truth.",
        "--level",
        "semantic",
        "--kind",
        "fact",
        "--workspace",
        &ws,
        "--json",
    ])?;
    let secret_content = format!("Recover using {SECRET_CANARY}");
    run_ee(&[
        "remember",
        &secret_content,
        "--level",
        "semantic",
        "--kind",
        "note",
        "--allow-secret-mention",
        "--workspace",
        &ws,
        "--json",
    ])?;
    let packed = run_ee(&["pack", CONTEXT_QUERY, "--workspace", &ws, "--json"])?;
    ensure_equal(
        &packed.pointer("/success").and_then(JsonValue::as_bool),
        &Some(true),
        "source pack succeeded",
    )?;
    let outcome = run_ee(&[
        "outcome",
        memory_id,
        "--signal",
        "helpful",
        "--reason",
        "The release rule caught a clippy regression",
        "--workspace",
        &ws,
        "--json",
    ])?;
    ensure_equal(
        &outcome.pointer("/success").and_then(JsonValue::as_bool),
        &Some(true),
        "source outcome succeeded",
    )?;
    let proposed = run_ee(&[
        "curate",
        "propose-derived",
        "--source-memory",
        memory_id,
        "--level",
        "semantic",
        "--kind",
        "insight",
        "--content",
        "Derived insight: format before release.",
        "--producer-kind",
        "e2e_test",
        "--workspace",
        &ws,
        "--json",
    ])?;
    ensure_equal(
        &proposed.pointer("/success").and_then(JsonValue::as_bool),
        &Some(true),
        "source curate propose-derived succeeded",
    )?;
    let created = run_ee(&[
        "backup",
        "create",
        "--include-graph-cache=false",
        "--output-dir",
        &backup_dir_arg,
        "--workspace",
        &ws,
        "--json",
    ])?;
    ensure(
        !created.to_string().contains(SECRET_CANARY),
        "backup create JSON leaked the secret canary",
    )?;
    let backup_id = json_str(&created, "/data/backupId", "created backup")?;
    let backup_path = PathBuf::from(json_str(&created, "/data/backupPath", "created backup")?);
    tree_must_not_contain(&backup_path, SECRET_CANARY)?;
    let restored = run_ee(&[
        "backup",
        "restore",
        backup_id,
        "--output-dir",
        &backup_dir_arg,
        "--side-path",
        &side_path_arg,
        "--workspace",
        &ws,
        "--json",
    ])?;
    ensure(
        !restored.to_string().contains(SECRET_CANARY),
        "backup restore JSON leaked the secret canary",
    )?;
    ensure_equal(
        &restored
            .pointer("/data/counts/memoriesImported")
            .and_then(JsonValue::as_u64),
        &Some(3),
        "restored all planted memories",
    )?;
    let pack_records = restored
        .pointer("/data/counts/packHistoryRestored/records")
        .and_then(JsonValue::as_u64)
        .unwrap_or(0);
    ensure(
        pack_records >= 1,
        format!("expected restored pack history, got {pack_records} records"),
    )?;
    let feedback = restored
        .pointer("/data/counts/feedbackEventsRestored")
        .and_then(JsonValue::as_u64)
        .unwrap_or(0);
    ensure(
        feedback >= 1,
        format!("expected restored feedback events, got {feedback}"),
    )?;
    let curation = restored
        .pointer("/data/counts/curationCandidatesRestored")
        .and_then(JsonValue::as_u64)
        .unwrap_or(0);
    ensure(
        curation >= 1,
        format!("expected restored curation candidates, got {curation}"),
    )?;

    let searched = run_ee(&[
        "search",
        CONTEXT_QUERY,
        "--workspace",
        &side_path_arg,
        "--json",
    ])?;
    ensure_equal(
        &searched.pointer("/success").and_then(JsonValue::as_bool),
        &Some(true),
        "restored search succeeded",
    )?;
    ensure(
        !searched.to_string().contains(SECRET_CANARY),
        "restored search leaked the secret canary",
    )?;
    let restored_memory_id = searched
        .pointer("/data/results")
        .and_then(JsonValue::as_array)
        .and_then(|hits| {
            hits.iter().find_map(|hit| {
                hit.get("docId")
                    .or_else(|| hit.get("memoryId"))
                    .or_else(|| hit.get("memory_id"))
                    .and_then(JsonValue::as_str)
                    .map(str::to_owned)
            })
        })
        .ok_or_else(|| format!("restored search missing memory id: {searched}"))?;
    let why = run_ee(&[
        "why",
        &restored_memory_id,
        "--workspace",
        &side_path_arg,
        "--json",
    ])?;
    ensure_equal(
        &why.pointer("/success").and_then(JsonValue::as_bool),
        &Some(true),
        "restored why succeeded",
    )?;

    // bd-reality-core-convergence-1azkt.13. `feedbackEventsRestored >= 1` and
    // `curationCandidatesRestored >= 1` above are counts, and a count cannot
    // distinguish "the planted feedback survived" from "a feedback row
    // survived". The acceptance asks for outcomes/feedback and curation
    // lineage to be PRESERVED, so the content is read back from the restored
    // database.
    //
    // 6e7b24c5c put these two blocks in
    // backup_restore_roundtrips_pack_history_and_query_surfaces, which plants
    // neither fixture -- the reason string and the proposed content are both
    // created in THIS test. There they could only ever report `[]`, which is
    // what they did, in a target two verify.sh stages execute.
    //
    // Keyed to `restored_memory_id`, NOT the source `memory_id`: this test
    // exercises Standard redaction, which remaps identifiers across the
    // archive boundary, which is why the `why` call above resolves the id from
    // a restored-side search rather than reusing the source one. These
    // assertions therefore have to follow that search.
    let restored_db = side_path.join(".ee").join("ee.db");
    let restored_conn = DbConnection::open_file(&restored_db)
        .map_err(|error| format!("open restored db: {error}"))?;
    let restored_workspace_id = workspace_id_from_db(&restored_conn, &side_path)?;
    let restored_feedback = restored_conn
        .list_feedback_events_for_target("memory", &restored_memory_id)
        .map_err(|error| format!("list restored outcome feedback: {error}"))?;
    ensure(
        restored_feedback.iter().any(|event| {
            event.signal == "helpful"
                && event
                    .reason
                    .as_deref()
                    .is_some_and(|reason| reason.contains("caught a clippy regression"))
        }),
        format!("restored outcome feedback lost exact durable semantics: {restored_feedback:?}"),
    )?;
    let restored_candidates = restored_conn
        .list_curation_candidates(&restored_workspace_id, None, None, None)
        .map_err(|error| format!("list restored curation candidates: {error}"))?;
    ensure(
        restored_candidates.iter().any(|candidate| {
            candidate
                .proposed_content
                .as_deref()
                .is_some_and(|content| content == "Derived insight: format before release.")
        }),
        format!("restored curation lineage lost planted candidate: {restored_candidates:?}"),
    )?;
    let restored_pack = run_ee(&[
        "pack",
        CONTEXT_QUERY,
        "--workspace",
        &side_path_arg,
        "--json",
    ])?;
    ensure_equal(
        &restored_pack
            .pointer("/success")
            .and_then(JsonValue::as_bool),
        &Some(true),
        "restored pack succeeded",
    )?;
    ensure(
        !restored_pack.to_string().contains(SECRET_CANARY),
        "restored pack leaked the secret canary",
    )?;
    let curated = run_ee(&[
        "curate",
        "candidates",
        "--all",
        "--workspace",
        &side_path_arg,
        "--json",
    ])?;
    ensure_equal(
        &curated.pointer("/success").and_then(JsonValue::as_bool),
        &Some(true),
        "restored curate candidates succeeded",
    )?;
    let maintain = run_ee(&[
        "maintenance",
        "status",
        "--workspace",
        &side_path_arg,
        "--json",
    ])?;
    ensure_equal(
        &maintain.pointer("/success").and_then(JsonValue::as_bool),
        &Some(true),
        "restored maintenance status succeeded",
    )?;
    Ok(())
}

#[test]
fn backup_then_restore_preserves_every_memory_and_tag() -> TestResult {
    let _trace = test_tracing::init_test_tracing(
        "bd-3usjw.53",
        "backup_then_restore_preserves_every_memory_and_tag",
    );
    let staging = tempfile::Builder::new()
        .prefix("ee-534m-roundtrip-")
        .tempdir()
        .map_err(|error| format!("create temp dir: {error}"))?;

    let workspace = staging.path().join("ws");
    let backup_dir = staging.path().join("backups");
    let side_path = staging.path().join("restored");
    let source_context_index = staging.path().join("source-empty-index");
    let restored_context_index = staging.path().join("restored-empty-index");

    std::fs::create_dir_all(&workspace).map_err(|error| format!("mkdir ws: {error}"))?;

    let workspace_arg = workspace.to_string_lossy().into_owned();
    let backup_dir_arg = backup_dir.to_string_lossy().into_owned();
    let side_path_arg = side_path.to_string_lossy().into_owned();
    let source_context_index_arg = source_context_index.to_string_lossy().into_owned();
    let restored_context_index_arg = restored_context_index.to_string_lossy().into_owned();

    // 1. Initialize the source workspace.
    let init = run_ee(&["--workspace", &workspace_arg, "--json", "init"])?;
    ensure_equal(
        &init.pointer("/data/status").and_then(JsonValue::as_str),
        &Some("created"),
        "init status",
    )?;
    enable_ppr_context_feature(&workspace)?;

    // 2. Seed with diverse memories: distinct levels, kinds, tag sets, scores.
    let seeds: &[(&str, &str, &str, &str, &str)] = &[
        (
            "procedural",
            "rule",
            "Always run cargo fmt --check before release",
            "alpha,backup-test",
            "0.95",
        ),
        (
            "semantic",
            "fact",
            "FrankenSQLite is the storage layer",
            "beta,backup-test",
            "0.90",
        ),
        (
            "episodic",
            "observation",
            "Saw the build pass after pinning the toolchain",
            "alpha,observation",
            "0.55",
        ),
        (
            "procedural",
            "anti-pattern",
            "Never invoke git reset --hard on dirty trees",
            "policy,backup-test",
            "0.99",
        ),
    ];

    let mut seeded_memory_ids = Vec::new();
    for (level, kind, content, tags, confidence) in seeds {
        let json = run_ee(&[
            "--workspace",
            &workspace_arg,
            "--json",
            "remember",
            "--level",
            level,
            "--kind",
            kind,
            "--tags",
            tags,
            "--confidence",
            confidence,
            content,
        ])?;
        ensure_equal(
            &json.pointer("/success").and_then(JsonValue::as_bool),
            &Some(true),
            &format!("remember `{content}` succeeded"),
        )?;
        seeded_memory_ids.push(json_str(&json, "/data/memory_id", "remember")?.to_owned());
    }

    run_ee(&[
        "--workspace",
        &workspace_arg,
        "--json",
        "memory",
        "link",
        &seeded_memory_ids[0],
        &seeded_memory_ids[2],
        "--relation",
        "supports",
        "--weight",
        "0.75",
        "--confidence",
        "0.90",
        "--evidence-count",
        "2",
    ])?;

    let tombstoned_memory_id = seeded_memory_ids
        .get(1)
        .ok_or_else(|| "missing memory id to tombstone".to_owned())?;
    let tombstone = run_ee(&[
        "--workspace",
        &workspace_arg,
        "--json",
        "curate",
        "tombstone",
        tombstoned_memory_id,
        "--reason",
        "backup-roundtrip-tombstone-fixture",
        "--actor",
        "e2e_backup_restore_roundtrip",
    ])?;
    ensure_equal(
        &tombstone.pointer("/persisted").and_then(JsonValue::as_bool),
        &Some(true),
        "tombstone persisted",
    )?;

    // 3. Capture source-side ground truth from the SQLite database directly,
    // including the tombstoned row so backup/restore must preserve it.
    let src_db = workspace.join(".ee").join("ee.db");
    ensure(src_db.exists(), "source database exists")?;
    let src_conn =
        DbConnection::open_file(&src_db).map_err(|error| format!("open src db: {error}"))?;
    let src_workspace_id = workspace_id_from_db(&src_conn, &workspace)?;
    let graph_snapshot_id = "gsnap_0000000000000000000000001";
    src_conn
        .insert_graph_snapshot(
            graph_snapshot_id,
            &CreateGraphSnapshotInput {
                workspace_id: src_workspace_id.clone(),
                snapshot_version: 1,
                schema_version: "ee.graph.snapshot.v1".to_owned(),
                graph_type: GraphSnapshotType::MemoryLinks,
                node_count: 4,
                edge_count: 3,
                metrics_json: serde_json::json!({"pagerank": {"backup": 0.42}}).to_string(),
                content_hash: "blake3:e2e-backup-graph-cache".to_owned(),
                source_generation: 1,
                expires_at: None,
            },
        )
        .map_err(|error| format!("insert graph snapshot: {error}"))?;
    src_conn
        .insert_graph_algorithm_witness(&CreateGraphAlgorithmWitnessInput {
            workspace_id: src_workspace_id.clone(),
            snapshot_id: graph_snapshot_id.to_owned(),
            algorithm: "pagerank".to_owned(),
            params_json: serde_json::json!({"alpha": 0.85}).to_string(),
            witness_json: serde_json::json!({"fixture": "backup-restore"}).to_string(),
        })
        .map_err(|error| format!("insert graph witness: {error}"))?;
    src_conn
        .upsert_graph_algorithm_result(&CreateGraphAlgorithmResultInput {
            workspace_id: src_workspace_id.clone(),
            snapshot_id: graph_snapshot_id.to_owned(),
            algorithm: "personalized_pagerank".to_owned(),
            params_hash: "blake3:e2e-backup-ppr-params".to_owned(),
            result_json: serde_json::json!({
                "scores": [{
                    "node": seeded_memory_ids[0],
                    "score": 1.0,
                }],
                "converged": true,
                "witness": {
                    "algorithm": "personalized_pagerank_acl_push",
                    "complexity_claim": "fixture",
                    "nodes_touched": 1,
                    "edges_scanned": 0,
                    "queue_peak": 1,
                },
            })
            .to_string(),
            ttl_seconds: 3600,
        })
        .map_err(|error| format!("insert graph result cache: {error}"))?;
    let src_memories = src_conn
        .list_memories(&src_workspace_id, None, true)
        .map_err(|error| format!("src list_memories: {error}"))?;
    ensure_equal(
        &src_memories.len(),
        &seeds.len(),
        "seeded memory count matches list_memories with tombstones",
    )?;
    ensure_equal(
        &src_memories
            .iter()
            .filter(|memory| memory.tombstoned_at.is_some())
            .count(),
        &1,
        "source has one tombstoned memory",
    )?;

    let mut src_pairs: Vec<(MemoryContent, Vec<String>)> = src_memories
        .iter()
        .map(|memory| memory_with_tags(&src_conn, memory))
        .collect::<Result<_, _>>()?;
    src_pairs.sort_by(|a, b| a.0.content.cmp(&b.0.content));
    let src_links = src_conn
        .list_all_memory_links(None)
        .map_err(|error| format!("source links: {error}"))?;
    ensure(
        !src_links.is_empty(),
        "source contains a real memory relationship",
    )?;
    drop(src_conn);
    let src_db_arg = src_db.to_string_lossy().into_owned();
    // Let the current context pipeline derive the seed-aware PPR params hash;
    // Frankensearch scores are part of the seed weights and should not be
    // guessed by this backup fixture.
    // The former `--ppr-weight` source/restored context probes (and the
    // `pprScore` cache-restoration assertions) were removed with the deprecated
    // `ee context` command: `ee pack`, the replacement surface, does not expose
    // `--ppr-weight` and hardcodes the PPR blend to disabled. The graph-cache
    // restoration is still verified directly against the SQLite store below;
    // here we keep a byte-identity check that the same `ee pack` query produces
    // identical output before and after the backup/restore round-trip.
    let (source_context, _source_context_stdout) = run_ee_raw(&[
        "--workspace",
        &workspace_arg,
        "--json",
        "pack",
        CONTEXT_QUERY,
        "--database",
        &src_db_arg,
        "--index-dir",
        &source_context_index_arg,
        "--candidate-pool",
        "1",
    ])?;
    let source_context_stdout = canonical_context_stdout(source_context.clone())?;

    // 4. Create the backup with redaction = none so content survives intact.
    let backup = run_ee(&[
        "--workspace",
        &workspace_arg,
        "--json",
        "backup",
        "create",
        "--output-dir",
        &backup_dir_arg,
        "--redaction",
        "none",
        "--label",
        "534m-roundtrip",
    ])?;
    persist_json_artifact("534m_01_backup_create", &backup)?;
    ensure_equal(
        &backup.pointer("/data/schema").and_then(JsonValue::as_str),
        &Some("ee.backup.create.v1"),
        "backup create schema",
    )?;
    let backup_id = backup
        .pointer("/data/backupId")
        .and_then(JsonValue::as_str)
        .ok_or_else(|| "missing backupId".to_owned())?
        .to_owned();
    ensure(backup_id.starts_with("bk_"), "backupId has bk_ prefix")?;
    let memory_records = backup
        .pointer("/data/counts/memoryRecords")
        .and_then(JsonValue::as_u64)
        .ok_or_else(|| "missing memoryRecords count".to_owned())?;
    ensure_equal(
        &usize::try_from(memory_records).unwrap_or(0),
        &seeds.len(),
        "backup memoryRecords matches seeded count",
    )?;
    ensure_equal(
        &backup
            .pointer("/data/includeGraphCache")
            .and_then(JsonValue::as_bool),
        &Some(true),
        "backup includes graph cache by default",
    )?;
    ensure_equal(
        &backup
            .pointer("/data/graphCache/assetCounts/graphAlgorithmResults")
            .and_then(JsonValue::as_u64),
        &Some(1),
        "backup graph result cache asset count",
    )?;
    let backup_records_path = records_path_from_report(&backup, "backup create")?;
    let backup_records = read_jsonl_records(&backup_records_path)?;
    let tombstoned_record = records_with_schema(&backup_records, "ee.export.memory.v1")
        .into_iter()
        .find(|record| {
            record.get("memory_id").and_then(JsonValue::as_str)
                == Some(tombstoned_memory_id.as_str())
        })
        .ok_or_else(|| "backup export did not include tombstoned memory record".to_owned())?;
    ensure(
        tombstoned_record
            .get("tombstoned_at")
            .and_then(JsonValue::as_str)
            .is_some_and(|value| !value.is_empty()),
        "backup export records tombstoned_at on tombstoned memory",
    )?;
    ensure_equal(
        &tombstoned_record
            .get("tombstoned_reason")
            .and_then(JsonValue::as_str),
        &Some("backup-roundtrip-tombstone-fixture"),
        "backup export records tombstoned reason",
    )?;

    // 5. Restore to the side path.
    let restore = run_ee(&[
        "--workspace",
        &workspace_arg,
        "--json",
        "backup",
        "restore",
        &backup_id,
        "--output-dir",
        &backup_dir_arg,
        "--side-path",
        &side_path_arg,
    ])?;
    persist_json_artifact("534m_02_backup_restore", &restore)?;
    ensure_equal(
        &restore.pointer("/data/schema").and_then(JsonValue::as_str),
        &Some("ee.backup.restore.v1"),
        "restore schema",
    )?;
    ensure_equal(
        &restore.pointer("/data/dryRun").and_then(JsonValue::as_bool),
        &Some(false),
        "restore was not a dry run",
    )?;
    let import_status = restore
        .pointer("/data/importStatus")
        .and_then(JsonValue::as_str)
        .ok_or_else(|| "missing importStatus".to_owned())?;
    ensure(
        matches!(import_status, "imported" | "completed"),
        format!("import status was {import_status:?}, expected imported|completed"),
    )?;
    let imported = restore
        .pointer("/data/counts/memoriesImported")
        .and_then(JsonValue::as_u64)
        .ok_or_else(|| "missing memoriesImported".to_owned())?;
    ensure_equal(
        &usize::try_from(imported).unwrap_or(0),
        &seeds.len(),
        "memoriesImported matches seeded count",
    )?;
    ensure_equal(
        &restore
            .pointer("/data/counts/graphCacheRowsRestored")
            .and_then(JsonValue::as_u64),
        &Some(3),
        "restore replays graph cache rows by default",
    )?;

    // 6. Source database must still exist after restore (restore is non-destructive).
    ensure(
        src_db.exists(),
        "source database survives restore (no in-place mutation)",
    )?;

    // 7. Restored DB now has the full content. Diff every content-bearing field.
    let restored_db_path = restore
        .pointer("/data/restoredDatabasePath")
        .and_then(JsonValue::as_str)
        .map(PathBuf::from)
        .ok_or_else(|| "missing restoredDatabasePath".to_owned())?;
    ensure(
        restored_db_path.exists(),
        format!("restored database exists at {}", restored_db_path.display()),
    )?;
    let restored_db_path_arg = restored_db_path.to_string_lossy().into_owned();
    let restored_why = run_ee(&[
        "--workspace",
        &side_path_arg,
        "--json",
        "why",
        tombstoned_memory_id,
        "--database",
        &restored_db_path_arg,
    ])?;
    ensure_equal(
        &restored_why
            .pointer("/data/lifecycle/status")
            .and_then(JsonValue::as_str),
        &Some("tombstoned"),
        "restored why lifecycle status",
    )?;
    ensure_equal(
        &restored_why
            .pointer("/data/lifecycle/tombstoned_reason")
            .and_then(JsonValue::as_str),
        &Some("backup-roundtrip-tombstone-fixture"),
        "restored why lifecycle tombstone reason",
    )?;

    let restored_conn = DbConnection::open_file(&restored_db_path)
        .map_err(|error| format!("open restored db: {error}"))?;
    let restored_workspace_id = workspace_id_from_db(&restored_conn, &side_path)?;
    let restored_graph_snapshots = restored_conn
        .list_graph_snapshots(
            &restored_workspace_id,
            Some(GraphSnapshotType::MemoryLinks),
            10,
        )
        .map_err(|error| format!("restored graph snapshots: {error}"))?;
    ensure_equal(
        &restored_graph_snapshots.len(),
        &1,
        "restored graph snapshot count",
    )?;
    ensure_equal(
        &restored_graph_snapshots[0].content_hash.as_str(),
        &"blake3:e2e-backup-graph-cache",
        "restored graph snapshot content hash",
    )?;
    let restored_witnesses = restored_conn
        .list_graph_algorithm_witnesses(&restored_workspace_id, graph_snapshot_id, Some("pagerank"))
        .map_err(|error| format!("restored graph witnesses: {error}"))?;
    ensure_equal(
        &restored_witnesses.len(),
        &1,
        "restored graph witness count",
    )?;
    let restored_results = restored_conn
        .list_graph_algorithm_results(
            &restored_workspace_id,
            graph_snapshot_id,
            Some("personalized_pagerank"),
        )
        .map_err(|error| format!("restored graph result cache: {error}"))?;
    ensure_equal(
        &restored_results.len(),
        &1,
        "restored graph result cache count",
    )?;
    drop(restored_conn);
    enable_ppr_context_feature(&side_path)?;
    let (restored_context, _restored_context_stdout) = run_ee_raw(&[
        "--workspace",
        &side_path_arg,
        "--json",
        "pack",
        CONTEXT_QUERY,
        "--database",
        &restored_db_path_arg,
        "--index-dir",
        &restored_context_index_arg,
        "--candidate-pool",
        "1",
    ])?;
    let restored_context_stdout = canonical_context_stdout(restored_context.clone())?;
    ensure_context_json_bytes_equal(
        &restored_context,
        &source_context,
        &restored_context_stdout,
        &source_context_stdout,
        "restored canonical context selection matches source context byte-for-byte",
    )?;
    ensure_equal(
        &context_item_contents(&restored_context, "restored context")?,
        &context_item_contents(&source_context, "source context")?,
        "restored context item contents match source context",
    )?;

    let restored_conn = DbConnection::open_file(&restored_db_path)
        .map_err(|error| format!("re-open restored db after context: {error}"))?;
    let restored_memories = restored_conn
        .list_memories(&restored_workspace_id, None, true)
        .map_err(|error| format!("restored list_memories: {error}"))?;
    ensure_equal(
        &restored_memories.len(),
        &src_pairs.len(),
        "restored memory count matches source",
    )?;
    ensure_equal(
        &restored_memories
            .iter()
            .filter(|memory| memory.tombstoned_at.is_some())
            .count(),
        &1,
        "restored has one tombstoned memory",
    )?;

    let mut restored_pairs: Vec<(MemoryContent, Vec<String>)> = restored_memories
        .iter()
        .map(|memory| memory_with_tags(&restored_conn, memory))
        .collect::<Result<_, _>>()?;
    restored_pairs.sort_by(|a, b| a.0.content.cmp(&b.0.content));
    ensure_equal(
        &restored_conn
            .list_all_memory_links(None)
            .map_err(|error| format!("restored links: {error}"))?,
        &src_links,
        "backup restore preserves every link field, ID and timestamp",
    )?;

    // 8. Row-by-row diff. Content + tag set must match exactly per pair.
    for (index, (src_pair, restored_pair)) in
        src_pairs.iter().zip(restored_pairs.iter()).enumerate()
    {
        ensure_equal(
            &restored_pair.0,
            &src_pair.0,
            &format!("memory[{index}] content fields"),
        )?;
        ensure_equal(
            &restored_pair.1,
            &src_pair.1,
            &format!("memory[{index}] tag set"),
        )?;
    }

    // 9. Restore is idempotent for verification: re-running list_memories on
    //    the restored DB after a fresh open returns the same content.
    drop(restored_conn);
    let restored_conn2 = DbConnection::open_file(&restored_db_path)
        .map_err(|error| format!("re-open restored db: {error}"))?;
    let restored_again = restored_conn2
        .list_memories(&restored_workspace_id, None, true)
        .map_err(|error| format!("restored re-list: {error}"))?;
    ensure_equal(
        &restored_again.len(),
        &restored_pairs.len(),
        "restored count stable across reopen",
    )?;

    Ok(())
}

#[test]
fn export_import_export_preserves_memory_and_tag_records() -> TestResult {
    let staging = tempfile::Builder::new()
        .prefix("ee-0n9b5-jsonl-roundtrip-")
        .tempdir()
        .map_err(|error| format!("create temp dir: {error}"))?;

    let source_workspace = staging.path().join("source-ws");
    let imported_workspace = staging.path().join("imported-ws");
    let source_export_dir = staging.path().join("source-export");
    let imported_export_dir = staging.path().join("imported-export");
    let provenance_dir = staging.path().join("provenance");
    fs::create_dir_all(&source_workspace).map_err(|error| format!("mkdir source ws: {error}"))?;
    fs::create_dir_all(&imported_workspace)
        .map_err(|error| format!("mkdir imported ws: {error}"))?;
    fs::create_dir_all(&provenance_dir).map_err(|error| format!("mkdir provenance: {error}"))?;

    let source_workspace_arg = source_workspace.to_string_lossy().into_owned();
    let imported_workspace_arg = imported_workspace.to_string_lossy().into_owned();
    let source_export_dir_arg = source_export_dir.to_string_lossy().into_owned();
    let imported_export_dir_arg = imported_export_dir.to_string_lossy().into_owned();

    let init_source = run_ee(&["--workspace", &source_workspace_arg, "--json", "init"])?;
    persist_json_artifact("0n9b5_01_init_source", &init_source)?;

    let seeds: &[(&str, &str, &str, &str, &str)] = &[
        (
            "procedural",
            "rule",
            "JSONL roundtrip rule: run cargo fmt --check before release",
            "roundtrip,rule,release",
            "0.97",
        ),
        (
            "semantic",
            "fact",
            "JSONL roundtrip fact: FrankenSQLite is the source of truth",
            "roundtrip,fact,storage",
            "0.91",
        ),
        (
            "episodic",
            "decision",
            "JSONL roundtrip decision: compare normalized JSON records",
            "roundtrip,decision,testing",
            "0.86",
        ),
        (
            "working",
            "failure",
            "JSONL roundtrip failure: stale exports hide missing provenance",
            "roundtrip,failure,provenance",
            "0.52",
        ),
        (
            "procedural",
            "command",
            "JSONL roundtrip command: ee export --redaction none",
            "roundtrip,command,cli",
            "0.88",
        ),
        (
            "semantic",
            "convention",
            "JSONL roundtrip convention: stable JSON stays machine-readable",
            "roundtrip,convention,json",
            "0.82",
        ),
        (
            "procedural",
            "anti-pattern",
            "JSONL roundtrip anti-pattern: do not drop tags during import",
            "roundtrip,anti-pattern,tags",
            "0.94",
        ),
        (
            "working",
            "risk",
            "JSONL roundtrip risk: regenerated workspace IDs must be normalized",
            "roundtrip,risk,workspace",
            "0.64",
        ),
        (
            "episodic",
            "playbook-step",
            "JSONL roundtrip playbook step: verify memory IDs survive import",
            "roundtrip,playbook,ids",
            "0.79",
        ),
    ];

    let mut memory_ids = Vec::with_capacity(seeds.len());
    for (index, (level, kind, content, tags, confidence)) in seeds.iter().copied().enumerate() {
        let source_path = provenance_dir.join(format!("source-{index}.md"));
        fs::write(&source_path, content)
            .map_err(|error| format!("write provenance source {index}: {error}"))?;
        let source_uri = format!("file://{}#L1", source_path.display());
        let remembered = run_ee(&[
            "--workspace",
            &source_workspace_arg,
            "--json",
            "remember",
            "--level",
            level,
            "--kind",
            kind,
            "--tags",
            tags,
            "--confidence",
            confidence,
            "--source",
            &source_uri,
            "--no-propose-candidates",
            content,
        ])?;
        persist_json_artifact(&format!("0n9b5_02_remember_{index}"), &remembered)?;
        ensure_equal(
            &remembered.pointer("/success").and_then(JsonValue::as_bool),
            &Some(true),
            &format!("remember {index} succeeded"),
        )?;
        memory_ids.push(json_str(&remembered, "/data/memory_id", "remember")?.to_owned());
    }

    ensure_equal(&memory_ids.len(), &seeds.len(), "remembered memory count")?;

    // This fixture exercises ordinary cross-workspace JSONL import, whose
    // security contract correctly refuses to carry store-local
    // `human_explicit` trust into a foreign store. Seed the portable class the
    // transport is allowed to preserve; backup restore has a separate verified
    // path that tests explicit trust capping below.
    let source_db = source_workspace.join(".ee").join("ee.db");
    let source_conn = DbConnection::open_file(&source_db)
        .map_err(|error| format!("open source JSONL fixture DB: {error}"))?;
    for memory_id in &memory_ids {
        ensure(
            source_conn
                .update_memory_trust_class(memory_id, "agent_validated")
                .map_err(|error| format!("cap source memory trust: {error}"))?,
            format!("source memory {memory_id} trust class updated"),
        )?;
    }
    drop(source_conn);

    let link = run_ee(&[
        "--workspace",
        &source_workspace_arg,
        "--json",
        "memory",
        "link",
        &memory_ids[0],
        &memory_ids[1],
        "--relation",
        "supports",
        "--weight",
        "0.75",
        "--confidence",
        "0.90",
        "--evidence-count",
        "2",
        "--metadata",
        r#"{"reason":"eidetic_engine_cli-0n9b5 source fixture"}"#,
        "--actor",
        "jsonl-roundtrip-e2e",
    ])?;
    persist_json_artifact("0n9b5_03_link_source_memories", &link)?;
    ensure_equal(
        &link.pointer("/data/status").and_then(JsonValue::as_str),
        &Some("created"),
        "source memory link created",
    )?;

    // Build a connected source graph so every exported memory has real
    // centrality and structural evidence. JSONL round-trip assertions then
    // exercise preservation of derived fields without relying on fabricated
    // zero-value baselines.
    for index in 1..memory_ids.len().saturating_sub(1) {
        let link = run_ee(&[
            "--workspace",
            &source_workspace_arg,
            "--json",
            "memory",
            "link",
            &memory_ids[index],
            &memory_ids[index + 1],
            "--relation",
            "supports",
            "--weight",
            "0.75",
            "--confidence",
            "0.90",
            "--evidence-count",
            "2",
            "--actor",
            "jsonl-roundtrip-e2e",
        ])?;
        ensure_equal(
            &link.pointer("/data/status").and_then(JsonValue::as_str),
            &Some("created"),
            &format!("source memory link {index} created"),
        )?;
    }
    let triangle_link = run_ee(&[
        "--workspace",
        &source_workspace_arg,
        "--json",
        "memory",
        "link",
        &memory_ids[0],
        &memory_ids[2],
        "--relation",
        "supports",
        "--weight",
        "0.75",
        "--confidence",
        "0.90",
        "--evidence-count",
        "2",
        "--actor",
        "jsonl-roundtrip-e2e",
    ])?;
    ensure_equal(
        &triangle_link
            .pointer("/data/status")
            .and_then(JsonValue::as_str),
        &Some("created"),
        "source triangle-closing memory link created",
    )?;
    let centrality = run_ee(&[
        "--workspace",
        &source_workspace_arg,
        "--json",
        "graph",
        "centrality-refresh",
    ])?;
    persist_json_artifact("0n9b5_03b_refresh_source_centrality", &centrality)?;
    ensure_equal(
        &centrality
            .pointer("/data/status")
            .and_then(JsonValue::as_str),
        &Some("refreshed"),
        "source graph centrality refreshed",
    )?;

    let source_export = run_ee(&[
        "--workspace",
        &source_workspace_arg,
        "--json",
        "export",
        "--output-dir",
        &source_export_dir_arg,
        "--redaction",
        "none",
        "--label",
        "0n9b5-source",
    ])?;
    persist_json_artifact("0n9b5_04_export_source", &source_export)?;
    ensure_equal(
        &source_export
            .pointer("/data/schema")
            .and_then(JsonValue::as_str),
        &Some("ee.export.report.v1"),
        "source export schema",
    )?;
    ensure_equal(
        &json_u64(
            &source_export,
            "/data/counts/memoryRecords",
            "source export",
        )?,
        &(seeds.len() as u64),
        "source export memory count",
    )?;

    let source_records_path = records_path_from_report(&source_export, "source export")?;
    let source_records = read_jsonl_records(&source_records_path)?;
    let source_records_json = JsonValue::Array(source_records.clone());
    persist_json_artifact("0n9b5_05_source_records", &source_records_json)?;
    ensure_memory_records_include_graph_fields(&source_records, "source export")?;

    let source_link_records = records_with_schema(&source_records, "ee.export.link.v1");
    ensure(
        !source_link_records.is_empty(),
        "source export contains the explicit memory link fixture",
    )?;
    ensure(
        source_link_records.iter().any(|record| {
            record.get("source_memory_id").and_then(JsonValue::as_str)
                == Some(memory_ids[0].as_str())
                && record.get("target_memory_id").and_then(JsonValue::as_str)
                    == Some(memory_ids[1].as_str())
                && record.get("link_type").and_then(JsonValue::as_str) == Some("supports")
        }),
        "source export includes the expected supports link",
    )?;

    let init_imported = run_ee(&["--workspace", &imported_workspace_arg, "--json", "init"])?;
    persist_json_artifact("0n9b5_06_init_imported", &init_imported)?;

    let source_records_path_arg = source_records_path.to_string_lossy().into_owned();
    let import = run_ee(&[
        "--workspace",
        &imported_workspace_arg,
        "--json",
        "import",
        "jsonl",
        "--source",
        &source_records_path_arg,
    ])?;
    persist_json_artifact("0n9b5_07_import_jsonl", &import)?;
    ensure_equal(
        &import.pointer("/data/status").and_then(JsonValue::as_str),
        &Some("completed"),
        "import status",
    )?;
    ensure_equal(
        &json_u64(&import, "/data/memoriesImported", "import report")?,
        &(seeds.len() as u64),
        "imported memory count",
    )?;
    ensure_equal(
        &json_u64(&import, "/data/linksImported", "import report")?,
        &(source_link_records.len() as u64),
        "every exported relationship is imported",
    )?;

    let imported_export = run_ee(&[
        "--workspace",
        &imported_workspace_arg,
        "--json",
        "export",
        "--output-dir",
        &imported_export_dir_arg,
        "--redaction",
        "none",
        "--label",
        "0n9b5-imported",
    ])?;
    persist_json_artifact("0n9b5_08_export_imported", &imported_export)?;
    ensure_equal(
        &json_u64(
            &imported_export,
            "/data/counts/memoryRecords",
            "imported export",
        )?,
        &(seeds.len() as u64),
        "imported export memory count",
    )?;

    let imported_records_path = records_path_from_report(&imported_export, "imported export")?;
    let imported_records = read_jsonl_records(&imported_records_path)?;
    let imported_records_json = JsonValue::Array(imported_records.clone());
    persist_json_artifact("0n9b5_09_imported_records", &imported_records_json)?;
    ensure_memory_records_include_graph_fields(&imported_records, "imported export")?;

    let ignored_memory_fields = ["workspace_id", "created_at", "updated_at"];
    let source_memories = normalized_records_with_schema(
        &source_records,
        "ee.export.memory.v1",
        &ignored_memory_fields,
    );
    let imported_memories = normalized_records_with_schema(
        &imported_records,
        "ee.export.memory.v1",
        &ignored_memory_fields,
    );
    persist_json_artifact(
        "0n9b5_10_normalized_source_memories",
        &JsonValue::Array(source_memories.clone()),
    )?;
    persist_json_artifact(
        "0n9b5_11_normalized_imported_memories",
        &JsonValue::Array(imported_memories.clone()),
    )?;
    ensure_equal(
        &imported_memories,
        &source_memories,
        "normalized memory records survive export/import/export",
    )?;

    let ignored_tag_fields = ["created_at"];
    let source_tags =
        normalized_records_with_schema(&source_records, "ee.export.tag.v1", &ignored_tag_fields);
    let imported_tags =
        normalized_records_with_schema(&imported_records, "ee.export.tag.v1", &ignored_tag_fields);
    ensure_equal(
        &imported_tags,
        &source_tags,
        "normalized tag records survive export/import/export",
    )?;

    let source_links = normalized_records_with_schema(&source_records, "ee.export.link.v1", &[]);
    let imported_links =
        normalized_records_with_schema(&imported_records, "ee.export.link.v1", &[]);
    ensure_equal(
        &imported_links,
        &source_links,
        "complete link records survive export/import/export without normalization",
    )?;

    Ok(())
}

#[test]
fn backup_create_and_restore_dry_run_leave_real_store_hash_unchanged() -> TestResult {
    let staging = tempfile::Builder::new()
        .prefix("ee-1azkt13-dryrun-")
        .tempdir()
        .map_err(|error| format!("create temp dir: {error}"))?;

    let workspace = staging.path().join("ws");
    let nested_preview_dir = workspace.join("would-be-backup");
    let backup_dir = staging.path().join("backups");
    let side_path = staging.path().join("restored");
    std::fs::create_dir_all(&workspace).map_err(|error| format!("mkdir ws: {error}"))?;

    let workspace_arg = workspace.to_string_lossy().into_owned();
    let nested_preview_arg = nested_preview_dir.to_string_lossy().into_owned();
    let backup_dir_arg = backup_dir.to_string_lossy().into_owned();
    let side_path_arg = side_path.to_string_lossy().into_owned();

    run_ee(&["--workspace", &workspace_arg, "--json", "init"])?;
    run_ee(&[
        "--workspace",
        &workspace_arg,
        "--json",
        "remember",
        "--level",
        "procedural",
        "--kind",
        "rule",
        "Dry-run probe memory",
    ])?;

    let before_create = workspace_file_digests(&workspace)?;
    let create_preview = run_ee(&[
        "--workspace",
        &workspace_arg,
        "--json",
        "backup",
        "create",
        "--output-dir",
        &nested_preview_arg,
        "--redaction",
        "none",
        "--label",
        "1azkt13-create-dry-run",
        "--dry-run",
    ])?;
    ensure_equal(
        &create_preview
            .pointer("/data/dryRun")
            .and_then(JsonValue::as_bool),
        &Some(true),
        "create reports dryRun=true",
    )?;
    ensure(
        !nested_preview_dir.exists(),
        "create --dry-run must not create its output directory",
    )?;
    ensure_store_unchanged(
        "backup create --dry-run",
        &before_create,
        &workspace_file_digests(&workspace)?,
    )?;

    let backup = run_ee(&[
        "--workspace",
        &workspace_arg,
        "--json",
        "backup",
        "create",
        "--output-dir",
        &backup_dir_arg,
        "--redaction",
        "none",
        "--label",
        "1azkt13-dryrun",
    ])?;
    let backup_id = backup
        .pointer("/data/backupId")
        .and_then(JsonValue::as_str)
        .ok_or_else(|| "missing backupId".to_owned())?
        .to_owned();

    let before_restore = workspace_file_digests(&workspace)?;
    let restore = run_ee(&[
        "--workspace",
        &workspace_arg,
        "--json",
        "backup",
        "restore",
        &backup_id,
        "--output-dir",
        &backup_dir_arg,
        "--side-path",
        &side_path_arg,
        "--dry-run",
    ])?;
    ensure_equal(
        &restore.pointer("/data/dryRun").and_then(JsonValue::as_bool),
        &Some(true),
        "restore reports dryRun=true",
    )?;
    ensure(
        !side_path.exists(),
        "dry-run restore must not create the side path",
    )?;
    ensure(
        !side_path.join(".ee").join("ee.db").exists(),
        "dry-run restore must not materialize a side-path database",
    )?;
    ensure_store_unchanged(
        "backup restore --dry-run",
        &before_restore,
        &workspace_file_digests(&workspace)?,
    )?;
    Ok(())
}

fn typed_fields_value(
    conn: &DbConnection,
    memory_id: &str,
    side: &str,
) -> Result<JsonValue, String> {
    let raw = conn
        .get_memory_typed_fields_json(memory_id)
        .map_err(|error| format!("{side} get_memory_typed_fields_json: {error}"))?
        .ok_or_else(|| format!("{side} memory {memory_id} has no typed fields"))?;
    serde_json::from_str(&raw).map_err(|error| format!("{side} typed fields are not JSON: {error}"))
}

/// bd-1n0np.23.2: typed memory fields survive backup -> restore.
///
/// This is the suite's one per-kind round trip at the DEFAULT redaction level
/// (no `--redaction` flag); the other per-kind tests use `--redaction minimal`,
/// so without this one the suite would only ever test a non-default mode. The
/// default re-mints memory IDs (bd-cjt23), so the restored memory is found by
/// its content, not by its ID.
#[test]
fn backup_restore_at_default_redaction_preserves_typed_memory_fields() -> TestResult {
    const CONTENT: &str = "Decision: keep the build cache on the remote workers.";
    let staging = tempfile::Builder::new()
        .prefix("ee-23-2-typed-")
        .tempdir()
        .map_err(|error| format!("create temp dir: {error}"))?;
    let workspace = staging.path().join("ws");
    let backup_dir = staging.path().join("backups");
    let side_path = staging.path().join("restored");
    fs::create_dir_all(&workspace).map_err(|error| format!("mkdir ws: {error}"))?;
    let ws = workspace.to_string_lossy().into_owned();
    let backup_dir_arg = backup_dir.to_string_lossy().into_owned();
    let side_path_arg = side_path.to_string_lossy().into_owned();

    run_ee(&["init", "--workspace", &ws, "--json"])?;
    let remembered = run_ee(&[
        "remember",
        CONTENT,
        "--level",
        "semantic",
        "--kind",
        "decision",
        "--field",
        "options=local",
        "--field",
        "options=remote",
        "--field",
        "chosen=remote",
        "--field",
        "rationale=keeps the SSD cold",
        "--workspace",
        &ws,
        "--json",
    ])?;
    let source_id = json_str(&remembered, "/data/memory_id", "remember")?.to_owned();
    let source_conn = DbConnection::open_file(workspace.join(".ee/ee.db"))
        .map_err(|error| format!("open source db: {error}"))?;
    let source_fields = typed_fields_value(&source_conn, &source_id, "source")?;
    // Empty-world guard: the comparison below proves nothing unless the source
    // really holds the fields it was given.
    ensure_equal(
        &source_fields
            .pointer("/fields/chosen")
            .and_then(JsonValue::as_str),
        &Some("remote"),
        "source memory holds the typed field it was given",
    )?;

    let created = run_ee(&[
        "backup",
        "create",
        "--output-dir",
        &backup_dir_arg,
        "--workspace",
        &ws,
        "--json",
    ])?;
    let backup_id = json_str(&created, "/data/backupId", "backup create")?;
    run_ee(&[
        "backup",
        "restore",
        backup_id,
        "--output-dir",
        &backup_dir_arg,
        "--side-path",
        &side_path_arg,
        "--workspace",
        &ws,
        "--json",
    ])?;

    let restored_conn = DbConnection::open_file(side_path.join(".ee/ee.db"))
        .map_err(|error| format!("open restored db: {error}"))?;
    let restored_workspace_id = workspace_id_from_db(&restored_conn, &side_path)?;
    let restored: Vec<StoredMemory> = restored_conn
        .list_memories(&restored_workspace_id, None, true)
        .map_err(|error| format!("restored list_memories: {error}"))?
        .into_iter()
        .filter(|memory| memory.content == CONTENT)
        .collect();
    ensure_equal(
        &restored.len(),
        &1,
        "exactly one restored memory carries the content",
    )?;
    let restored_fields = typed_fields_value(&restored_conn, &restored[0].id, "restored")?;
    ensure_equal(
        &restored_fields,
        &source_fields,
        "typed fields after a default-redaction backup -> restore",
    )
}

/// A per-kind round-trip world: an initialized workspace plus the paths a
/// `backup create` / `backup restore --side-path` cycle needs.
struct KindRoundTrip {
    _staging: tempfile::TempDir,
    workspace: PathBuf,
    backup_dir: PathBuf,
    side_path: PathBuf,
}

impl KindRoundTrip {
    fn new(prefix: &str) -> Result<Self, String> {
        let staging = tempfile::Builder::new()
            .prefix(prefix)
            .tempdir()
            .map_err(|error| format!("create temp dir: {error}"))?;
        let workspace = staging.path().join("ws");
        fs::create_dir_all(&workspace).map_err(|error| format!("mkdir ws: {error}"))?;
        let world = Self {
            backup_dir: staging.path().join("backups"),
            side_path: staging.path().join("restored"),
            workspace,
            _staging: staging,
        };
        run_ee(&["init", "--workspace", &world.ws(), "--json"])?;
        Ok(world)
    }

    fn ws(&self) -> String {
        self.workspace.to_string_lossy().into_owned()
    }

    fn source_db(&self) -> Result<DbConnection, String> {
        DbConnection::open_file(self.workspace.join(".ee/ee.db"))
            .map_err(|error| format!("open source db: {error}"))
    }

    fn restored_db(&self) -> Result<DbConnection, String> {
        DbConnection::open_file(self.side_path.join(".ee/ee.db"))
            .map_err(|error| format!("open restored db: {error}"))
    }

    /// `backup create --redaction minimal`, then restore to the side path.
    /// Minimal keeps memory IDs, as does the default backup policy. The
    /// explicit level keeps these focused recovery fixtures independent of
    /// configured workspace redaction defaults.
    fn backup_minimal_and_restore(&self) -> Result<JsonValue, String> {
        let ws = self.ws();
        let backup_dir = self.backup_dir.to_string_lossy().into_owned();
        let side_path = self.side_path.to_string_lossy().into_owned();
        let created = run_ee(&[
            "backup",
            "create",
            "--redaction",
            "minimal",
            "--output-dir",
            &backup_dir,
            "--workspace",
            &ws,
            "--json",
        ])?;
        let backup_id = json_str(&created, "/data/backupId", "backup create")?;
        run_ee(&[
            "backup",
            "restore",
            backup_id,
            "--output-dir",
            &backup_dir,
            "--side-path",
            &side_path,
            "--workspace",
            &ws,
            "--json",
        ])
    }
}

/// bd-1n0np.23.2: sentinel specs survive backup -> restore unchanged, every
/// field of every spec, including the memory each belongs to.
#[test]
fn backup_restore_preserves_memory_sentinel_specs() -> TestResult {
    let world = KindRoundTrip::new("ee-23-2-sentinel-specs-")?;
    let remembered = run_ee(&[
        "remember",
        "The workspace keeps a README and a Cargo manifest at its root.",
        "--level",
        "semantic",
        "--kind",
        "fact",
        "--sentinel",
        "path_exists:README.md",
        "--sentinel",
        "path_exists:Cargo.toml",
        "--workspace",
        &world.ws(),
        "--json",
    ])?;
    let memory_id = json_str(&remembered, "/data/memory_id", "remember")?.to_owned();
    let source_specs = world
        .source_db()?
        .list_memory_sentinel_specs(&memory_id)
        .map_err(|error| format!("source list_memory_sentinel_specs: {error}"))?;
    // Empty-world guard: two specs were attached, so two must be stored.
    ensure_equal(&source_specs.len(), &2, "source sentinel specs stored")?;

    world.backup_minimal_and_restore()?;

    let restored_specs = world
        .restored_db()?
        .list_memory_sentinel_specs(&memory_id)
        .map_err(|error| format!("restored list_memory_sentinel_specs: {error}"))?;
    ensure_equal(
        &restored_specs,
        &source_specs,
        "sentinel specs after backup -> restore",
    )
}

/// The query-miss ledger's rows, compared whole except for `workspace_id`,
/// which names the store the row lives in: a side-path restore is a different
/// store.
fn query_miss_rows(
    conn: &DbConnection,
    side: &str,
) -> Result<Vec<ee::db::StoredAuditEntry>, String> {
    let mut rows = conn
        .list_audit_by_action(ee::db::audit_actions::SEARCH_MISS_RECORDED, None)
        .map_err(|error| format!("{side} list_audit_by_action: {error}"))?;
    for row in &mut rows {
        row.workspace_id = None;
    }
    rows.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(rows)
}

/// bd-1n0np.23.2: the query-miss ledger survives backup -> restore, and restore
/// does not invent it.
///
/// The ledger is not a table: it is `audit_log` rows with action
/// `search.miss_recorded`. `audit_log` is append-only (a trigger aborts
/// DELETE), so the "rows removed" control cannot be staged by deleting rows in
/// a source store. Arm 2 therefore uses a store that never recorded a miss:
/// it must restore with none, so the rows arm 1 finds come from the backup and
/// not from anything the restore produced.
#[test]
fn backup_restore_preserves_query_miss_ledger_and_does_not_invent_it() -> TestResult {
    // Arm 1: a store with recorded misses.
    let world = KindRoundTrip::new("ee-23-2-miss-ledger-")?;
    let ws = world.ws();
    run_ee(&[
        "remember",
        "The release checklist runs cargo fmt before tagging.",
        "--level",
        "semantic",
        "--kind",
        "fact",
        "--workspace",
        &ws,
        "--json",
    ])?;
    // An ask that no memory supports abstains, and an abstention is recorded
    // in the ledger (record_ask_query_miss_best_effort). A search only records
    // a miss when it retrieved candidates that all fell below the floor, which
    // depends on scoring; the abstention does not. The ask's own exit status is
    // not asserted: the guard below is what says whether a miss was recorded.
    run_ee_output(&[
        "ask",
        "Which zebra orbits the quantum marmalade moon?",
        "--workspace",
        &ws,
        "--json",
    ])?;
    let source_rows = query_miss_rows(&world.source_db()?, "source")?;
    // Empty-world guard: the ask must have recorded a miss, or the comparison
    // below compares two empty ledgers.
    ensure(
        !source_rows.is_empty(),
        "source ask recorded no query-miss row",
    )?;
    world.backup_minimal_and_restore()?;
    ensure_equal(
        &query_miss_rows(&world.restored_db()?, "restored")?,
        &source_rows,
        "query-miss ledger after backup -> restore",
    )?;

    // Arm 2: a store that never recorded a miss restores with none.
    let quiet = KindRoundTrip::new("ee-23-2-miss-ledger-quiet-")?;
    run_ee(&[
        "remember",
        "The release checklist runs cargo fmt before tagging.",
        "--level",
        "semantic",
        "--kind",
        "fact",
        "--workspace",
        &quiet.ws(),
        "--json",
    ])?;
    ensure(
        query_miss_rows(&quiet.source_db()?, "quiet source")?.is_empty(),
        "quiet source has no query-miss rows",
    )?;
    quiet.backup_minimal_and_restore()?;
    ensure(
        query_miss_rows(&quiet.restored_db()?, "quiet restored")?.is_empty(),
        "restore invented query-miss rows the backup did not carry",
    )
}

/// What anchor extraction produced for a memory, as a sorted set.
///
/// Anchors are not copied by backup: restore re-extracts them from the memory
/// content (backup_table_policy: derived_rebuildable / rebuild_on_restore),
/// once when the import inserts the memory and again when restore's index
/// rebuild backfills any memory left without anchors. So
/// the set compares what extraction yields: kind, value hash, redacted value,
/// captured-span hash, confidence and freshness. Left out on purpose:
/// `generation` and the timestamps (when the row was written), and `source` /
/// `provenance` (which write path ran the extraction; on restore it is import).
fn extracted_anchor_set(
    conn: &DbConnection,
    memory_id: &str,
    side: &str,
) -> Result<Vec<String>, String> {
    let mut anchors: Vec<String> = conn
        .list_memory_anchors(memory_id)
        .map_err(|error| format!("{side} list_memory_anchors: {error}"))?
        .iter()
        .map(|anchor| {
            format!(
                "{:?}|{}|{}|{}|{}|{:?}",
                anchor.anchor_kind,
                anchor.anchor_value_hash,
                anchor.redacted_anchor_value,
                anchor.captured_span_hash,
                anchor.confidence,
                anchor.freshness_state,
            )
        })
        .collect();
    anchors.sort();
    Ok(anchors)
}

/// bd-1n0np.23.2: a restored memory's anchors are the same set its source had.
#[test]
fn backup_restore_re_extracts_the_same_memory_anchors() -> TestResult {
    let world = KindRoundTrip::new("ee-23-2-anchors-")?;
    // Extraction is precision-first: it takes explicit `anchor:KIND:VALUE`
    // tokens, schema IDs and code fragments, not bare prose paths.
    let remembered = run_ee(&[
        "remember",
        "Backups carry ee.backup.recovery_inventory.v1; see anchor:path:src/core/backup.rs and anchor:env_var:EE_WORKSPACE.",
        "--level",
        "procedural",
        "--kind",
        "rule",
        "--workspace",
        &world.ws(),
        "--json",
    ])?;
    let memory_id = json_str(&remembered, "/data/memory_id", "remember")?.to_owned();
    let source_anchors = extracted_anchor_set(&world.source_db()?, &memory_id, "source")?;
    // Empty-world guard: the content carries a schema ID and two explicit
    // anchors, so extraction must have produced anchors, or the comparison
    // below compares two empty sets.
    ensure(
        !source_anchors.is_empty(),
        "source memory has no extracted anchors",
    )?;

    world.backup_minimal_and_restore()?;

    ensure_equal(
        &extracted_anchor_set(&world.restored_db()?, &memory_id, "restored")?,
        &source_anchors,
        "anchors re-extracted on restore",
    )
}

/// The latest sentinel status per spec for one memory.
fn sentinel_statuses(
    conn: &DbConnection,
    memory_id: &str,
    side: &str,
) -> Result<BTreeMap<String, String>, String> {
    Ok(conn
        .latest_memory_sentinel_results_for_memory(memory_id)
        .map_err(|error| format!("{side} latest_memory_sentinel_results_for_memory: {error}"))?
        .into_iter()
        .map(|result| (result.spec_hash, format!("{:?}", result.status)))
        .collect())
}

/// Run `ee sentinel check` on a workspace. Its exit status is not asserted: a
/// failing sentinel is a result, not a command error. The caller's guard on
/// the stored results carries this output if nothing was recorded.
fn sentinel_check(workspace: &str) -> Result<String, String> {
    let output = run_ee_output(&["sentinel", "check", "--workspace", workspace, "--json"])?;
    Ok(format!(
        "exit {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    ))
}

/// bd-1n0np.23.2: sentinel results are not copied by backup. Restore records
/// that they "must be checked afresh" (backup_table_policy:
/// derived_rebuildable / rebuild_on_restore), so the oracle is a re-check:
/// `ee sentinel check` on the restored store gives the same status per spec as
/// it gave on the source. Both targets are independent of the working tree,
/// because restore brings back the store, not the workspace's files.
#[test]
fn backup_restore_then_sentinel_recheck_reproduces_statuses() -> TestResult {
    let world = KindRoundTrip::new("ee-23-2-sentinel-results-")?;
    let ws = world.ws();
    let remembered = run_ee(&[
        "remember",
        "No generated file should exist, and EE_WORKSPACE stays a registered variable.",
        "--level",
        "semantic",
        "--kind",
        "fact",
        "--sentinel",
        "path_exists:no/such/generated/file.txt",
        "--sentinel",
        "env_var_registered:EE_WORKSPACE",
        "--workspace",
        &ws,
        "--json",
    ])?;
    let memory_id = json_str(&remembered, "/data/memory_id", "remember")?.to_owned();
    let source_check = sentinel_check(&ws)?;
    let source = sentinel_statuses(&world.source_db()?, &memory_id, "source")?;
    // Empty-world guard: two specs were attached and checked, so there must be
    // a result for each, or the comparison below compares two empty maps.
    ensure(
        source.len() == 2,
        format!("expected a source result per spec, got {source:?}\n{source_check}"),
    )?;

    world.backup_minimal_and_restore()?;

    let restored_check = sentinel_check(&world.side_path.to_string_lossy())?;
    ensure_equal(
        &sentinel_statuses(&world.restored_db()?, &memory_id, "restored")?,
        &source,
        &format!("sentinel statuses after restore and a re-check\n{restored_check}"),
    )
}

/// bd-1n0np.23.2: workspace generations are not copied (backup_table_policy:
/// derived_rebuildable / rebuild_on_restore); triggers on workspace and memory
/// writes rebuild them while restore imports. The value counts writes, so a
/// restored store legitimately differs from its source. The oracle is that the
/// restored store HAS a generation and that it still increases on a new write.
#[test]
fn backup_restore_rebuilds_a_workspace_generation_that_still_increases() -> TestResult {
    let world = KindRoundTrip::new("ee-23-2-generation-")?;
    run_ee(&[
        "remember",
        "Generations advance on every durable write.",
        "--level",
        "semantic",
        "--kind",
        "fact",
        "--workspace",
        &world.ws(),
        "--json",
    ])?;
    let source_conn = world.source_db()?;
    let source_workspace_id = workspace_id_from_db(&source_conn, &world.workspace)?;
    // Empty-world guard: without a source generation there is nothing for the
    // restore to have rebuilt.
    ensure(
        source_conn
            .get_workspace_generation(&source_workspace_id)
            .map_err(|error| format!("source get_workspace_generation: {error}"))?
            .is_some(),
        "source store has no workspace generation",
    )?;
    drop(source_conn);

    world.backup_minimal_and_restore()?;

    let restored_conn = world.restored_db()?;
    let restored_workspace_id = workspace_id_from_db(&restored_conn, &world.side_path)?;
    let restored = restored_conn
        .get_workspace_generation(&restored_workspace_id)
        .map_err(|error| format!("restored get_workspace_generation: {error}"))?
        .ok_or("restored store has no workspace generation")?;
    drop(restored_conn);
    run_ee(&[
        "remember",
        "A write after restore advances the restored generation.",
        "--level",
        "semantic",
        "--kind",
        "fact",
        "--workspace",
        &world.side_path.to_string_lossy(),
        "--json",
    ])?;
    let after_write = world
        .restored_db()?
        .get_workspace_generation(&restored_workspace_id)
        .map_err(|error| format!("restored get_workspace_generation after write: {error}"))?
        .ok_or("restored store lost its workspace generation after a write")?;
    ensure(
        after_write > restored,
        format!("restored generation did not increase on a write: {restored} -> {after_write}"),
    )
}

/// The `hash` of the hash-manifest entry with this `label`.
fn attest_manifest_hash<'a>(attest: &'a JsonValue, label: &str) -> Option<&'a str> {
    attest
        .pointer("/data/bundle/hashManifest/entries")?
        .as_array()?
        .iter()
        .find(|entry| entry.get("label").and_then(JsonValue::as_str) == Some(label))?
        .get("hash")?
        .as_str()
}

/// The `(id, contentHash)` of each evidence entry of this `kind`, sorted.
fn attest_evidence(attest: &JsonValue, kind: &str) -> Vec<(String, String)> {
    let mut entries: Vec<(String, String)> = attest
        .pointer("/data/bundle/evidenceManifest/entries")
        .and_then(JsonValue::as_array)
        .into_iter()
        .flatten()
        .filter(|entry| entry.get("kind").and_then(JsonValue::as_str) == Some(kind))
        .map(|entry| {
            let field = |name: &str| {
                entry
                    .get(name)
                    .and_then(JsonValue::as_str)
                    .unwrap_or_default()
                    .to_owned()
            };
            (field("id"), field("contentHash"))
        })
        .collect();
    entries.sort();
    entries
}

/// The replacement hashes the redaction manifest records for this `field`, sorted.
fn attest_redaction_hashes(attest: &JsonValue, field: &str) -> Vec<String> {
    let mut hashes: Vec<String> = attest
        .pointer("/data/bundle/redactionManifest/entries")
        .and_then(JsonValue::as_array)
        .into_iter()
        .flatten()
        .filter(|entry| entry.get("field").and_then(JsonValue::as_str) == Some(field))
        .filter_map(|entry| entry.get("replacementHash").and_then(JsonValue::as_str))
        .map(str::to_owned)
        .collect();
    hashes.sort();
    hashes
}

/// The `field` of every omission the bundle declares.
fn attest_omissions(attest: &JsonValue) -> Vec<String> {
    attest
        .pointer("/data/bundle/omissions")
        .and_then(JsonValue::as_array)
        .into_iter()
        .flatten()
        .filter_map(|omission| omission.get("field").and_then(JsonValue::as_str))
        .map(str::to_owned)
        .collect()
}

/// bd-1n0np.23.2: attestation bundles are not stored; `ee attest memory` builds
/// one on read from the memory, its links, anchors, audit rows and seal. The
/// bundle includes recovery history as well as the original memory evidence:
///
/// - The subject, content, links, trust/validity and memory provenance chain
///   remain equal. Recovery preserves absent provenance, and the re-extracted
///   anchors retain their original provenance and redacted values.
/// - Recovery adds exactly one audit row for the memory while preserving every
///   source audit. The audit manifest and aggregate bundle therefore change.
/// - Anchor timestamps can reflect re-extraction, so the aggregate anchor
///   hash is not a content-identity oracle.
///
/// `--redaction minimal` keeps the memory ID the bundle names.
#[test]
fn backup_restore_keeps_attestation_content_and_changes_only_custody() -> TestResult {
    let world = KindRoundTrip::new("ee-23-2-attestation-")?;
    let ws = world.ws();
    let remembered = run_ee(&[
        "remember",
        "Backups carry ee.backup.recovery_inventory.v1 and anchor:path:src/core/backup.rs.",
        "--level",
        "semantic",
        "--kind",
        "fact",
        "--workspace",
        &ws,
        "--json",
    ])?;
    let memory_id = json_str(&remembered, "/data/memory_id", "remember")?.to_owned();
    let source = run_ee(&["attest", "memory", &memory_id, "--workspace", &ws, "--json"])?;
    // Empty-world guard: the source attestation must name this memory and
    // carry a bundle hash, or the comparison below compares two empty shapes.
    ensure_equal(
        &source
            .pointer("/data/subjectId")
            .and_then(JsonValue::as_str),
        &Some(memory_id.as_str()),
        "source attestation names the memory",
    )?;
    ensure(
        source
            .pointer("/data/bundleHash")
            .and_then(JsonValue::as_str)
            .is_some_and(|hash| !hash.is_empty()),
        "source attestation has a bundle hash",
    )?;

    world.backup_minimal_and_restore()?;

    let restored = run_ee(&[
        "attest",
        "memory",
        &memory_id,
        "--workspace",
        &world.side_path.to_string_lossy(),
        "--json",
    ])?;

    // CONTENT: equal.
    for pointer in ["/data/subjectKind", "/data/subjectId"] {
        ensure_equal(
            &restored.pointer(pointer),
            &source.pointer(pointer),
            pointer,
        )?;
    }
    for label in [
        "memory.redacted_content",
        "memory.links",
        "memory.trust_validity",
        "memory.provenance_chain",
    ] {
        ensure(
            attest_manifest_hash(&source, label).is_some(),
            format!("source attestation has no {label} hash"),
        )?;
        ensure_equal(
            &attest_manifest_hash(&restored, label),
            &attest_manifest_hash(&source, label),
            label,
        )?;
    }
    ensure_equal(
        &attest_evidence(&restored, "memory"),
        &attest_evidence(&source, "memory"),
        "the memory's own evidence entry (id and content hash)",
    )?;
    let anchor_ids = |attest: &JsonValue| -> Vec<String> {
        attest_evidence(attest, "memory_anchor")
            .into_iter()
            .map(|(id, _)| id)
            .collect()
    };
    ensure(
        !anchor_ids(&source).is_empty(),
        "source attestation has no anchor evidence",
    )?;
    ensure_equal(
        &anchor_ids(&restored),
        &anchor_ids(&source),
        "anchor evidence ids",
    )?;
    for field in ["memory.content", "memoryAnchors[].redactedAnchorValue"] {
        ensure_equal(
            &attest_redaction_hashes(&restored, field),
            &attest_redaction_hashes(&source, field),
            field,
        )?;
    }

    // Recovery does not invent provenance for the memory or its anchors.
    ensure(
        attest_redaction_hashes(&source, "memory.provenanceUri").is_empty(),
        "the source memory has no provenance URI",
    )?;
    ensure_equal(
        &attest_redaction_hashes(&restored, "memory.provenanceUri"),
        &attest_redaction_hashes(&source, "memory.provenanceUri"),
        "the restored memory retains absent provenance",
    )?;
    let omission = "publicProjection.provenanceUri".to_owned();
    ensure(
        !attest_omissions(&source).contains(&omission)
            && !attest_omissions(&restored).contains(&omission),
        "neither bundle invents a provenance URI omission",
    )?;

    ensure(
        !attest_redaction_hashes(&source, "memoryAnchors[].provenance").is_empty(),
        "the source has anchor provenance to compare",
    )?;
    ensure_equal(
        &attest_redaction_hashes(&restored, "memoryAnchors[].provenance"),
        &attest_redaction_hashes(&source, "memoryAnchors[].provenance"),
        "restored anchor provenance matches the original evidence",
    )?;

    // CUSTODY: restore adds exactly one audit row and keeps every source row.
    let source_audit = attest_evidence(&source, "audit_log");
    let restored_audit = attest_evidence(&restored, "audit_log");
    ensure(
        restored_audit.len() == source_audit.len() + 1
            && source_audit.iter().all(|row| restored_audit.contains(row)),
        format!("audit evidence: source {source_audit:?}, restored {restored_audit:?}"),
    )?;

    // The recovery event changes the audit manifest and bundle, while the
    // memory's own provenance chain above remains identical.
    ensure(
        attest_manifest_hash(&restored, "memory.audit")
            != attest_manifest_hash(&source, "memory.audit"),
        "the audit manifest must include the recovery event",
    )?;
    ensure(
        restored.pointer("/data/bundleHash") != source.pointer("/data/bundleHash"),
        "the bundle must distinguish the recovered audit history",
    )
}
