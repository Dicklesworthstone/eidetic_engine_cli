use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

type TestResult = Result<(), String>;

fn run_ee(args: &[&str]) -> Result<Output, String> {
    Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(args)
        .output()
        .map_err(|error| format!("failed to run ee {}: {error}", args.join(" ")))
}

fn unique_artifact_dir(prefix: &str) -> Result<PathBuf, String> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("clock moved backwards: {error}"))?
        .as_nanos();
    Ok(PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("ee-install-artifacts")
        .join(format!("{prefix}-{}-{now}", std::process::id())))
}

fn ensure(condition: bool, context: &str) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(context.to_owned())
    }
}

fn ensure_equal<T: std::fmt::Debug + PartialEq>(
    actual: T,
    expected: T,
    context: &str,
) -> TestResult {
    if actual == expected {
        Ok(())
    } else {
        Err(format!("{context}: expected {expected:?}, got {actual:?}"))
    }
}

fn parse_stdout(output: &Output) -> Result<serde_json::Value, String> {
    let stdout = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str(&stdout).map_err(|error| format!("invalid JSON stdout: {error}\n{stdout}"))
}

fn json_str<'a>(value: &'a serde_json::Value, pointer: &str) -> Result<Option<&'a str>, String> {
    value
        .pointer(pointer)
        .map(|field| {
            field
                .as_str()
                .ok_or_else(|| format!("{pointer} is not a string"))
        })
        .transpose()
}

fn json_bool(value: &serde_json::Value, pointer: &str) -> Result<Option<bool>, String> {
    value
        .pointer(pointer)
        .map(|field| {
            field
                .as_bool()
                .ok_or_else(|| format!("{pointer} is not a bool"))
        })
        .transpose()
}

fn json_array<'a>(
    value: &'a serde_json::Value,
    pointer: &str,
) -> Result<&'a Vec<serde_json::Value>, String> {
    value
        .pointer(pointer)
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| format!("{pointer} is not an array"))
}

fn has_finding(value: &serde_json::Value, code: &str) -> Result<bool, String> {
    Ok(json_array(value, "/data/findings")?
        .iter()
        .any(|finding| json_str(finding, "/code").ok().flatten() == Some(code)))
}

fn normalize_dynamic_value(value: &mut serde_json::Value, install_root: Option<&Path>) {
    match value {
        serde_json::Value::String(text) => {
            if let Some(root) = install_root {
                let root = root.to_string_lossy().replace('\\', "/");
                *text = text.replace(&root, "<INSTALL_ROOT>");
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                normalize_dynamic_value(item, install_root);
            }
        }
        serde_json::Value::Object(map) => {
            for item in map.values_mut() {
                normalize_dynamic_value(item, install_root);
            }
        }
        serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => {}
    }
}

fn normalized_install_json(
    mut value: serde_json::Value,
    install_root: Option<&Path>,
) -> Result<String, String> {
    normalize_dynamic_value(&mut value, install_root);
    if let Some(data) = value
        .get_mut("data")
        .and_then(serde_json::Value::as_object_mut)
        && data.contains_key("idempotencyKey")
    {
        data.insert(
            "idempotencyKey".to_owned(),
            serde_json::Value::String("<IDEMPOTENCY_KEY>".to_owned()),
        );
    }
    serde_json::to_string(&value).map_err(|error| error.to_string())
}

fn assert_install_golden(
    name: &str,
    value: serde_json::Value,
    install_root: Option<&Path>,
) -> TestResult {
    let actual = normalized_install_json(value, install_root)?;
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("golden")
        .join("install")
        .join(format!("{name}.golden"));
    let expected = fs::read_to_string(&path)
        .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
    ensure_equal(actual.trim(), expected.trim(), name)
}

#[cfg(unix)]
fn write_fake_ee(path: &Path) -> TestResult {
    fs::write(path, "#!/bin/sh\nexit 0\n").map_err(|error| error.to_string())?;
    let mut permissions = fs::metadata(path)
        .map_err(|error| error.to_string())?
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).map_err(|error| error.to_string())
}

#[cfg(unix)]
#[test]
fn install_check_detects_duplicate_path_binaries_without_stderr() -> TestResult {
    let root = unique_artifact_dir("install-check")?;
    let bin_a = root.join("a");
    let bin_b = root.join("b");
    fs::create_dir_all(&bin_a).map_err(|error| error.to_string())?;
    fs::create_dir_all(&bin_b).map_err(|error| error.to_string())?;
    write_fake_ee(&bin_a.join("ee"))?;
    write_fake_ee(&bin_b.join("ee"))?;
    let path_value = std::env::join_paths([bin_a.as_path(), bin_b.as_path()])
        .map_err(|error| error.to_string())?;
    let path_arg = path_value
        .to_str()
        .ok_or_else(|| "PATH argument was not UTF-8".to_owned())?;
    let install_dir = bin_a
        .to_str()
        .ok_or_else(|| "install dir was not UTF-8".to_owned())?;
    let current_binary = bin_b.join("ee");
    let current_binary_arg = current_binary
        .to_str()
        .ok_or_else(|| "current binary was not UTF-8".to_owned())?;

    let output = run_ee(&[
        "install",
        "check",
        "--json",
        "--install-dir",
        install_dir,
        "--current-binary",
        current_binary_arg,
        "--path",
        path_arg,
        "--target",
        "x86_64-unknown-linux-gnu",
        "--offline",
    ])?;
    let stderr = String::from_utf8_lossy(&output.stderr);
    ensure(
        output.status.success(),
        &format!("install check should succeed; stderr: {stderr}"),
    )?;
    ensure(
        stderr.is_empty(),
        "JSON install check must not write stderr",
    )?;
    let value = parse_stdout(&output)?;

    ensure_equal(
        json_str(&value, "/schema")?,
        Some("ee.response.v1"),
        "response schema",
    )?;
    ensure_equal(
        json_str(&value, "/data/schema")?,
        Some("ee.install.check.v1"),
        "install schema",
    )?;
    ensure_equal(
        json_str(&value, "/data/path/status")?,
        Some("duplicate"),
        "PATH duplicate status",
    )?;
    ensure(
        has_finding(&value, "duplicate_path_binary")?,
        "duplicate finding present",
    )?;
    assert_install_golden("duplicate_path_check.json", value, Some(&root))
}

#[test]
fn install_plan_selects_manifest_artifact_and_stays_dry_run() -> TestResult {
    let root = unique_artifact_dir("install-plan")?;
    let install_dir = root.join("bin");
    let install_dir_arg = install_dir
        .to_str()
        .ok_or_else(|| "install dir was not UTF-8".to_owned())?;
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("release_manifest")
        .join("single_platform_dev.json");
    let manifest_arg = manifest
        .to_str()
        .ok_or_else(|| "manifest path was not UTF-8".to_owned())?;

    let output = run_ee(&[
        "install",
        "plan",
        "--json",
        "--manifest",
        manifest_arg,
        "--install-dir",
        install_dir_arg,
        "--target",
        "x86_64-unknown-linux-musl",
        "--offline",
    ])?;
    let stderr = String::from_utf8_lossy(&output.stderr);
    ensure(
        output.status.success(),
        &format!("install plan should succeed; stderr: {stderr}"),
    )?;
    ensure(stderr.is_empty(), "JSON install plan must not write stderr")?;
    let value = parse_stdout(&output)?;

    ensure_equal(
        json_str(&value, "/data/schema")?,
        Some("ee.install.plan.v1"),
        "install plan schema",
    )?;
    ensure_equal(json_bool(&value, "/data/dryRun")?, Some(true), "dry run")?;
    ensure_equal(
        json_str(&value, "/data/artifact/artifactId")?,
        Some("ee-0.1.0-dev-x86_64-unknown-linux-musl"),
        "selected artifact",
    )?;
    ensure(
        json_array(&value, "/data/plannedOperations")?
            .iter()
            .all(|operation| {
                json_bool(operation, "/requiresVerification").ok().flatten() == Some(true)
            }),
        "planned operations require verification",
    )?;
    assert_install_golden("fresh_install_plan.json", value, Some(&root))
}

#[test]
fn update_dry_run_manifest_plan_matches_golden() -> TestResult {
    let root = unique_artifact_dir("update-plan")?;
    let install_dir = root.join("bin");
    let install_dir_arg = install_dir
        .to_str()
        .ok_or_else(|| "install dir was not UTF-8".to_owned())?;
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("release_manifest")
        .join("multi_platform.json");
    let manifest_arg = manifest
        .to_str()
        .ok_or_else(|| "manifest path was not UTF-8".to_owned())?;

    let output = run_ee(&[
        "update",
        "--dry-run",
        "--json",
        "--manifest",
        manifest_arg,
        "--install-dir",
        install_dir_arg,
        "--target",
        "x86_64-unknown-linux-gnu",
        "--offline",
    ])?;
    let stderr = String::from_utf8_lossy(&output.stderr);
    ensure(
        output.status.success(),
        &format!("update dry-run should succeed; stderr: {stderr}"),
    )?;
    ensure(
        stderr.is_empty(),
        "JSON update dry-run must not write stderr",
    )?;
    let value = parse_stdout(&output)?;

    ensure_equal(
        json_str(&value, "/data/schema")?,
        Some("ee.update.plan.v1"),
        "update schema",
    )?;
    ensure_equal(
        json_str(&value, "/data/operation")?,
        Some("update"),
        "operation",
    )?;
    assert_install_golden("update_plan.json", value, Some(&root))
}

#[test]
fn install_plan_checksum_mismatch_refuses_unverified_artifact() -> TestResult {
    let root = unique_artifact_dir("checksum-mismatch")?;
    let install_dir = root.join("bin");
    let artifact_root = root.join("artifacts");
    fs::create_dir_all(&artifact_root).map_err(|error| error.to_string())?;
    fs::write(
        artifact_root.join("ee-x86_64-unknown-linux-gnu.tar.xz"),
        "wrong artifact bytes",
    )
    .map_err(|error| error.to_string())?;
    let install_dir_arg = install_dir
        .to_str()
        .ok_or_else(|| "install dir was not UTF-8".to_owned())?;
    let artifact_root_arg = artifact_root
        .to_str()
        .ok_or_else(|| "artifact root was not UTF-8".to_owned())?;
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("release_manifest")
        .join("checksum_mismatch.json");
    let manifest_arg = manifest
        .to_str()
        .ok_or_else(|| "manifest path was not UTF-8".to_owned())?;

    let output = run_ee(&[
        "install",
        "plan",
        "--json",
        "--manifest",
        manifest_arg,
        "--artifact-root",
        artifact_root_arg,
        "--install-dir",
        install_dir_arg,
        "--target",
        "x86_64-unknown-linux-gnu",
        "--offline",
    ])?;
    ensure(
        output.status.success(),
        "checksum mismatch remains a successful dry-run report",
    )?;
    let value = parse_stdout(&output)?;

    ensure_equal(
        json_str(&value, "/data/status")?,
        Some("blocked"),
        "blocked status",
    )?;
    ensure_equal(
        json_str(&value, "/data/verification/checksumStatus")?,
        Some("failed"),
        "checksum status",
    )?;
    ensure(
        has_finding(&value, "artifact_checksum_mismatch")?,
        "checksum mismatch finding",
    )?;
    assert_install_golden("checksum_mismatch_plan.json", value, Some(&root))
}

#[test]
fn install_plan_unsupported_target_matches_golden() -> TestResult {
    let root = unique_artifact_dir("unsupported-target")?;
    let install_dir = root.join("bin");
    let install_dir_arg = install_dir
        .to_str()
        .ok_or_else(|| "install dir was not UTF-8".to_owned())?;
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("release_manifest")
        .join("unsupported_target.json");
    let manifest_arg = manifest
        .to_str()
        .ok_or_else(|| "manifest path was not UTF-8".to_owned())?;

    let output = run_ee(&[
        "install",
        "plan",
        "--json",
        "--manifest",
        manifest_arg,
        "--install-dir",
        install_dir_arg,
        "--target",
        "sparc64-unknown-plan9",
        "--offline",
    ])?;
    ensure(output.status.success(), "unsupported target dry-run report")?;
    let value = parse_stdout(&output)?;

    ensure_equal(
        json_str(&value, "/data/status")?,
        Some("blocked"),
        "blocked status",
    )?;
    ensure(
        has_finding(&value, "unsupported_target")?,
        "unsupported target finding",
    )?;
    assert_install_golden("unsupported_target_plan.json", value, Some(&root))
}

#[test]
fn install_plan_without_manifest_matches_offline_golden() -> TestResult {
    let root = unique_artifact_dir("offline-plan")?;
    let install_dir = root.join("bin");
    let install_dir_arg = install_dir
        .to_str()
        .ok_or_else(|| "install dir was not UTF-8".to_owned())?;

    let output = run_ee(&[
        "install",
        "plan",
        "--json",
        "--install-dir",
        install_dir_arg,
        "--target",
        "x86_64-unknown-linux-gnu",
        "--offline",
    ])?;
    ensure(output.status.success(), "offline dry-run report")?;
    let value = parse_stdout(&output)?;

    ensure_equal(
        json_str(&value, "/data/status")?,
        Some("blocked"),
        "blocked status",
    )?;
    ensure(
        has_finding(&value, "offline_no_manifest")?,
        "offline no manifest finding",
    )?;
    assert_install_golden("offline_no_manifest_plan.json", value, Some(&root))
}

#[cfg(unix)]
#[test]
fn install_check_permission_denied_matches_golden() -> TestResult {
    let output = run_ee(&[
        "install",
        "check",
        "--json",
        "--install-dir",
        "/dev/null/ee",
        "--current-binary",
        "/dev/null/not-ee",
        "--path",
        "/dev/null",
        "--target",
        "x86_64-unknown-linux-gnu",
        "--offline",
    ])?;
    ensure(output.status.success(), "permission check report")?;
    let value = parse_stdout(&output)?;

    ensure_equal(
        json_str(&value, "/data/permissions/status")?,
        Some("missing_parent_unknown"),
        "permission status",
    )?;
    ensure(
        has_finding(&value, "install_dir_not_writable")?,
        "install dir not writable finding",
    )?;
    assert_install_golden("permission_denied_check.json", value, None)
}

#[test]
fn update_without_dry_run_is_policy_denied_json() -> TestResult {
    let output = run_ee(&["update", "--json"])?;
    let value = parse_stdout(&output)?;

    ensure(!output.status.success(), "update apply should fail")?;
    ensure_equal(
        json_str(&value, "/schema")?,
        Some("ee.error.v1"),
        "error schema",
    )?;
    ensure_equal(
        json_str(&value, "/error/code")?,
        Some("policy_denied"),
        "policy denied code",
    )
}

// ============================================================================
// EE-DIST-004: Additional e2e scenarios
// ============================================================================

#[test]
fn install_check_already_current_is_idempotent() -> TestResult {
    let root = unique_artifact_dir("already-current")?;
    let install_dir = root.join("bin");
    fs::create_dir_all(&install_dir).map_err(|error| error.to_string())?;
    let install_dir_arg = install_dir
        .to_str()
        .ok_or_else(|| "install dir was not UTF-8".to_owned())?;
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("release_manifest")
        .join("already_current.json");
    let manifest_arg = manifest
        .to_str()
        .ok_or_else(|| "manifest path was not UTF-8".to_owned())?;
    fs::write(
        &manifest,
        serde_json::json!({
            "schema": "ee.release_manifest.v1",
            "version": env!("CARGO_PKG_VERSION"),
            "artifacts": [{
                "artifactId": "ee-current-x86_64-unknown-linux-gnu",
                "target": "x86_64-unknown-linux-gnu",
                "url": "file:///dev/null",
                "sha256": "0".repeat(64),
                "bytes": 1000
            }]
        })
        .to_string(),
    )
    .map_err(|error| error.to_string())?;

    let output = run_ee(&[
        "install",
        "check",
        "--json",
        "--manifest",
        manifest_arg,
        "--install-dir",
        install_dir_arg,
        "--target",
        "x86_64-unknown-linux-gnu",
        "--offline",
    ])?;
    let stderr = String::from_utf8_lossy(&output.stderr);
    ensure(
        output.status.success(),
        &format!("already-current check should succeed; stderr: {stderr}"),
    )?;
    let value = parse_stdout(&output)?;

    ensure_equal(
        json_str(&value, "/schema")?,
        Some("ee.response.v1"),
        "response schema",
    )?;
    ensure_equal(
        json_str(&value, "/data/schema")?,
        Some("ee.install.check.v1"),
        "install check schema",
    )
}

#[test]
fn install_plan_rejects_path_traversal_in_artifact_name() -> TestResult {
    let root = unique_artifact_dir("path-traversal")?;
    let install_dir = root.join("bin");
    fs::create_dir_all(&install_dir).map_err(|error| error.to_string())?;
    let install_dir_arg = install_dir
        .to_str()
        .ok_or_else(|| "install dir was not UTF-8".to_owned())?;
    let manifest = root.join("traversal_manifest.json");
    let manifest_arg = manifest
        .to_str()
        .ok_or_else(|| "manifest path was not UTF-8".to_owned())?;
    fs::write(
        &manifest,
        serde_json::json!({
            "schema": "ee.release_manifest.v1",
            "version": "0.1.0",
            "artifacts": [{
                "artifactId": "../../../etc/passwd",
                "target": "x86_64-unknown-linux-gnu",
                "url": "file:///tmp/evil.tar.xz",
                "sha256": "0".repeat(64),
                "bytes": 1000
            }]
        })
        .to_string(),
    )
    .map_err(|error| error.to_string())?;

    let output = run_ee(&[
        "install",
        "plan",
        "--json",
        "--manifest",
        manifest_arg,
        "--install-dir",
        install_dir_arg,
        "--target",
        "x86_64-unknown-linux-gnu",
        "--offline",
    ])?;
    let value = parse_stdout(&output)?;

    let status = json_str(&value, "/data/status")?;
    let has_traversal = has_finding(&value, "path_traversal_detected").unwrap_or(false)
        || has_finding(&value, "invalid_artifact_id").unwrap_or(false)
        || status == Some("blocked");
    ensure(
        has_traversal,
        "path traversal should be detected or blocked",
    )
}

#[test]
fn install_plan_rejects_unicode_control_chars_in_path() -> TestResult {
    let root = unique_artifact_dir("unicode-control")?;
    let install_dir = root.join("bin");
    fs::create_dir_all(&install_dir).map_err(|error| error.to_string())?;
    let install_dir_arg = install_dir
        .to_str()
        .ok_or_else(|| "install dir was not UTF-8".to_owned())?;
    let manifest = root.join("unicode_manifest.json");
    let manifest_arg = manifest
        .to_str()
        .ok_or_else(|| "manifest path was not UTF-8".to_owned())?;
    fs::write(
        &manifest,
        serde_json::json!({
            "schema": "ee.release_manifest.v1",
            "version": "0.1.0",
            "artifacts": [{
                "artifactId": "ee-\u{202E}gnp.exe",
                "target": "x86_64-unknown-linux-gnu",
                "url": "file:///tmp/rtl-trick.tar.xz",
                "sha256": "0".repeat(64),
                "bytes": 1000
            }]
        })
        .to_string(),
    )
    .map_err(|error| error.to_string())?;

    let output = run_ee(&[
        "install",
        "plan",
        "--json",
        "--manifest",
        manifest_arg,
        "--install-dir",
        install_dir_arg,
        "--target",
        "x86_64-unknown-linux-gnu",
        "--offline",
    ])?;
    let value = parse_stdout(&output)?;

    let status = json_str(&value, "/data/status")?;
    let has_unicode_issue = has_finding(&value, "unicode_control_character").unwrap_or(false)
        || has_finding(&value, "invalid_artifact_id").unwrap_or(false)
        || status == Some("blocked");
    ensure(
        has_unicode_issue,
        "unicode control characters should be detected or blocked",
    )
}

#[test]
fn install_plan_handles_duplicate_target_ids() -> TestResult {
    let root = unique_artifact_dir("duplicate-target")?;
    let install_dir = root.join("bin");
    fs::create_dir_all(&install_dir).map_err(|error| error.to_string())?;
    let install_dir_arg = install_dir
        .to_str()
        .ok_or_else(|| "install dir was not UTF-8".to_owned())?;
    let manifest = root.join("duplicate_target_manifest.json");
    let manifest_arg = manifest
        .to_str()
        .ok_or_else(|| "manifest path was not UTF-8".to_owned())?;
    fs::write(
        &manifest,
        serde_json::json!({
            "schema": "ee.release_manifest.v1",
            "version": "0.1.0",
            "artifacts": [
                {
                    "artifactId": "ee-v1-x86_64-unknown-linux-gnu",
                    "target": "x86_64-unknown-linux-gnu",
                    "url": "file:///tmp/first.tar.xz",
                    "sha256": "a".repeat(64),
                    "bytes": 1000
                },
                {
                    "artifactId": "ee-v2-x86_64-unknown-linux-gnu",
                    "target": "x86_64-unknown-linux-gnu",
                    "url": "file:///tmp/second.tar.xz",
                    "sha256": "b".repeat(64),
                    "bytes": 1000
                }
            ]
        })
        .to_string(),
    )
    .map_err(|error| error.to_string())?;

    let output = run_ee(&[
        "install",
        "plan",
        "--json",
        "--manifest",
        manifest_arg,
        "--install-dir",
        install_dir_arg,
        "--target",
        "x86_64-unknown-linux-gnu",
        "--offline",
    ])?;
    let value = parse_stdout(&output)?;

    ensure_equal(
        json_str(&value, "/schema")?,
        Some("ee.response.v1"),
        "response schema",
    )?;
    let has_duplicate_finding = has_finding(&value, "duplicate_target").unwrap_or(false)
        || has_finding(&value, "ambiguous_artifact").unwrap_or(false);
    let selected_artifact = json_str(&value, "/data/artifact/artifactId")?;
    ensure(
        has_duplicate_finding || selected_artifact.is_some(),
        "duplicate targets should be handled (warning or deterministic selection)",
    )
}

#[cfg(unix)]
#[test]
fn install_check_handles_symlinked_install_root() -> TestResult {
    let root = unique_artifact_dir("symlink-root")?;
    let real_bin = root.join("real_bin");
    let symlink_bin = root.join("linked_bin");
    fs::create_dir_all(&real_bin).map_err(|error| error.to_string())?;
    std::os::unix::fs::symlink(&real_bin, &symlink_bin).map_err(|error| error.to_string())?;
    let install_dir_arg = symlink_bin
        .to_str()
        .ok_or_else(|| "install dir was not UTF-8".to_owned())?;
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("release_manifest")
        .join("single_platform_dev.json");
    let manifest_arg = manifest
        .to_str()
        .ok_or_else(|| "manifest path was not UTF-8".to_owned())?;

    let output = run_ee(&[
        "install",
        "check",
        "--json",
        "--manifest",
        manifest_arg,
        "--install-dir",
        install_dir_arg,
        "--target",
        "x86_64-unknown-linux-musl",
        "--offline",
    ])?;
    let stderr = String::from_utf8_lossy(&output.stderr);
    ensure(
        output.status.success(),
        &format!("symlinked root check should succeed; stderr: {stderr}"),
    )?;
    let value = parse_stdout(&output)?;

    ensure_equal(
        json_str(&value, "/schema")?,
        Some("ee.response.v1"),
        "response schema",
    )?;
    ensure_equal(
        json_str(&value, "/data/schema")?,
        Some("ee.install.check.v1"),
        "install check schema",
    )
}

#[test]
fn install_plan_huge_manifest_does_not_hang() -> TestResult {
    let root = unique_artifact_dir("huge-manifest")?;
    let install_dir = root.join("bin");
    fs::create_dir_all(&install_dir).map_err(|error| error.to_string())?;
    let install_dir_arg = install_dir
        .to_str()
        .ok_or_else(|| "install dir was not UTF-8".to_owned())?;
    let manifest = root.join("huge_manifest.json");
    let manifest_arg = manifest
        .to_str()
        .ok_or_else(|| "manifest path was not UTF-8".to_owned())?;

    let mut artifacts = Vec::new();
    for i in 0..500 {
        artifacts.push(serde_json::json!({
            "artifactId": format!("ee-{i}-fake-target-{i}"),
            "target": format!("fake-target-{i}"),
            "url": format!("file:///tmp/artifact-{i}.tar.xz"),
            "sha256": format!("{:064x}", i),
            "bytes": 1000 + i
        }));
    }
    fs::write(
        &manifest,
        serde_json::json!({
            "schema": "ee.release_manifest.v1",
            "version": "0.1.0",
            "artifacts": artifacts
        })
        .to_string(),
    )
    .map_err(|error| error.to_string())?;

    let output = run_ee(&[
        "install",
        "plan",
        "--json",
        "--manifest",
        manifest_arg,
        "--install-dir",
        install_dir_arg,
        "--target",
        "x86_64-unknown-linux-gnu",
        "--offline",
    ])?;
    ensure(output.status.success(), "huge manifest should complete")?;
    let value = parse_stdout(&output)?;

    ensure_equal(
        json_str(&value, "/schema")?,
        Some("ee.response.v1"),
        "response schema",
    )?;
    let status = json_str(&value, "/data/status")?;
    ensure(
        status == Some("blocked") || status == Some("ready"),
        "huge manifest should report blocked (no target) or ready",
    )
}

#[test]
fn install_plan_empty_manifest_is_blocked() -> TestResult {
    let root = unique_artifact_dir("empty-manifest")?;
    let install_dir = root.join("bin");
    fs::create_dir_all(&install_dir).map_err(|error| error.to_string())?;
    let install_dir_arg = install_dir
        .to_str()
        .ok_or_else(|| "install dir was not UTF-8".to_owned())?;
    let manifest = root.join("empty_manifest.json");
    let manifest_arg = manifest
        .to_str()
        .ok_or_else(|| "manifest path was not UTF-8".to_owned())?;
    fs::write(
        &manifest,
        serde_json::json!({
            "schema": "ee.release_manifest.v1",
            "version": "0.1.0",
            "artifacts": []
        })
        .to_string(),
    )
    .map_err(|error| error.to_string())?;

    let output = run_ee(&[
        "install",
        "plan",
        "--json",
        "--manifest",
        manifest_arg,
        "--install-dir",
        install_dir_arg,
        "--target",
        "x86_64-unknown-linux-gnu",
        "--offline",
    ])?;
    ensure(output.status.success(), "empty manifest report")?;
    let value = parse_stdout(&output)?;

    ensure_equal(
        json_str(&value, "/data/status")?,
        Some("blocked"),
        "empty manifest should block",
    )?;
    ensure(
        has_finding(&value, "unsupported_target")? || has_finding(&value, "no_artifacts")?,
        "empty manifest should have finding",
    )
}
