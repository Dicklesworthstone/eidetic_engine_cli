use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

type TestResult = Result<(), String>;

fn repo_file(path: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(path)
}

fn read_repo_file(path: &str) -> Result<String, String> {
    fs::read_to_string(repo_file(path)).map_err(|error| format!("failed to read `{path}`: {error}"))
}

fn target_tmp_dir() -> PathBuf {
    option_env!("CARGO_TARGET_TMPDIR").map_or_else(
        || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/e2e-retention-contract"),
        PathBuf::from,
    )
}

fn write_fake_ee(path: &Path) -> Result<(), String> {
    let source = r#"#!/usr/bin/env bash
set -uo pipefail

if [ "${1:-}" = "--version" ]; then
    printf 'ee 0.0.0-retention-test\n'
    exit 0
fi

if [ "${1:-}" = "init" ]; then
    workspace=""
    while [ "$#" -gt 0 ]; do
        case "${1:-}" in
            --workspace)
                workspace="${2:?missing workspace}"
                shift 2
                ;;
            *)
                shift
                ;;
        esac
    done
    if [ -z "$workspace" ]; then
        echo "missing --workspace" >&2
        exit 2
    fi
    mkdir -p "$workspace/.ee"
    printf '{"schema":"ee.response.v2","success":true,"data":{"workspace":"%s"},"degraded":[]}\n' "$workspace"
    exit 0
fi

echo "unexpected fake ee invocation: $*" >&2
exit 64
"#;
    fs::write(path, source).map_err(|error| format!("failed to write fake ee: {error}"))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(path)
            .map_err(|error| format!("failed to stat fake ee: {error}"))?
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions)
            .map_err(|error| format!("failed to chmod fake ee: {error}"))?;
    }

    Ok(())
}

#[test]
fn shared_helper_defines_retention_manifest_and_guarded_cleanup() -> TestResult {
    let source = read_repo_file("scripts/e2e_overhaul/lib/shared.sh")?;
    for expected in [
        "EPIC_RETENTION_MANIFEST",
        "ee.e2e.retention_manifest.v1",
        "_epic_workspace_owned_by_setup",
        "retained_by_keep_workspace",
        "retained_cleanup_refused_unowned_path",
        "removed_by_default_cleanup",
        "EE_E2E_KEEP_WORKSPACE",
        "e2e_retention_manifest.json",
    ] {
        if !source.contains(expected) {
            return Err(format!("shared e2e helper missing `{expected}`"));
        }
    }

    let keep_pos = source
        .find("_epic_keep_workspace_enabled")
        .ok_or_else(|| "shared helper missing keep-workspace predicate".to_owned())?;
    let rm_pos = source
        .find("rm -rf \"$EPIC_WORKSPACE\"")
        .ok_or_else(|| "shared helper missing expected workspace cleanup command".to_owned())?;
    if keep_pos > rm_pos {
        return Err("workspace cleanup must be gated by keep-workspace handling".to_owned());
    }

    let ownership_pos = source
        .find("! _epic_workspace_owned_by_setup")
        .ok_or_else(|| "shared helper missing ownership proof before cleanup".to_owned())?;
    if ownership_pos > rm_pos {
        return Err("workspace cleanup must check setup ownership before rm -rf".to_owned());
    }

    Ok(())
}

#[test]
fn overhaul_epic_scripts_do_not_delete_workspaces_directly() -> TestResult {
    let root = repo_file("scripts/e2e_overhaul");
    for entry in fs::read_dir(&root).map_err(|error| format!("failed to read {root:?}: {error}"))? {
        let entry = entry.map_err(|error| format!("failed to read dir entry: {error}"))?;
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("sh") {
            continue;
        }
        let text = fs::read_to_string(&path)
            .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
        if text.contains("rm -rf") {
            return Err(format!(
                "{} deletes recursively instead of using shared retention cleanup",
                path.display()
            ));
        }
    }
    Ok(())
}

#[test]
fn testing_strategy_documents_e2e_retention_contract() -> TestResult {
    let docs = read_repo_file("docs/testing-strategy.md")?;
    for expected in [
        "ee.e2e.retention_manifest.v1",
        "EE_E2E_KEEP_WORKSPACE=1",
        "EE_E2E_KEEP_ARTIFACTS=1",
        "e2e_retention_manifest.json",
        "retained workspace",
    ] {
        if !docs.contains(expected) {
            return Err(format!(
                "testing strategy missing retention detail `{expected}`"
            ));
        }
    }
    Ok(())
}

#[test]
fn keep_workspace_runtime_writes_manifest_and_preserves_workspace() -> TestResult {
    let run_dir = target_tmp_dir().join(format!("j11-retention-{}", std::process::id()));
    fs::create_dir_all(&run_dir)
        .map_err(|error| format!("failed to create run dir {}: {error}", run_dir.display()))?;

    let fake_ee = run_dir.join("fake-ee");
    write_fake_ee(&fake_ee)?;

    let test_log = run_dir.join("j1.jsonl");
    let script = format!(
        r#"set -uo pipefail
source "{}/scripts/e2e_overhaul/lib/shared.sh"
epic_setup j11_retention_probe
printf '%s\n' "$EPIC_RETENTION_MANIFEST"
exit 7
"#,
        env!("CARGO_MANIFEST_DIR")
    );

    let output = Command::new("bash")
        .arg("-c")
        .arg(script)
        .env("EE_BINARY", &fake_ee)
        .env("EE_E2E_KEEP_WORKSPACE", "1")
        .env("EE_E2E_TMPDIR", &run_dir)
        .env("EE_TEST_LOG_PATH", &test_log)
        .output()
        .map_err(|error| format!("failed to run retention smoke: {error}"))?;

    let code = output.status.code().unwrap_or(-1);
    if code != 7 {
        return Err(format!(
            "retention smoke should preserve script exit 7, got {code}; stderr={}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let manifest_path = stdout
        .lines()
        .last()
        .ok_or_else(|| "retention smoke did not print manifest path".to_owned())?;
    let manifest_text = fs::read_to_string(manifest_path)
        .map_err(|error| format!("failed to read manifest `{manifest_path}`: {error}"))?;
    let manifest: Value = serde_json::from_str(&manifest_text)
        .map_err(|error| format!("invalid retention manifest JSON: {error}"))?;

    if manifest["schema"] != "ee.e2e.retention_manifest.v1" {
        return Err(format!(
            "unexpected manifest schema: {}",
            manifest["schema"]
        ));
    }
    if manifest["cleanup_policy"] != "retained_by_keep_workspace" {
        return Err(format!(
            "unexpected cleanup policy: {}",
            manifest["cleanup_policy"]
        ));
    }
    if manifest["retained"] != true {
        return Err("manifest must mark retained=true in keep mode".to_owned());
    }

    let workspace = manifest["workspace"]
        .as_str()
        .ok_or_else(|| "manifest workspace must be a string".to_owned())?;
    if !Path::new(workspace).is_dir() {
        return Err(format!("retained workspace missing: {workspace}"));
    }
    if !Path::new(workspace).join(".ee").is_dir() {
        return Err(format!(
            "fake ee init marker missing in retained workspace: {workspace}"
        ));
    }
    if !test_log.is_file() {
        return Err(format!("J1 log was not retained at {}", test_log.display()));
    }

    Ok(())
}
