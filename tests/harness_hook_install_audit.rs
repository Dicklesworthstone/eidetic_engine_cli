//! bd-i0iiw.3 - read-only harness hook install-audit coverage.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::{Path, PathBuf};

use ee::hooks::{HarnessHookInstallOptions, HarnessHookTarget, generate_harness_hook_install};
use serde_json::Value;
use tempfile::TempDir;

type TestResult = Result<(), String>;

fn options(
    target: HarnessHookTarget,
    settings_path: &Path,
    install: bool,
    undo: bool,
) -> HarnessHookInstallOptions {
    HarnessHookInstallOptions {
        target,
        workspace: settings_path
            .parent()
            .unwrap_or_else(|| Path::new("/tmp"))
            .to_path_buf(),
        settings_path: Some(settings_path.to_path_buf()),
        install,
        undo,
        ee_binary_path: Some(PathBuf::from("/usr/local/bin/ee")),
    }
}

fn audit_status(target: HarnessHookTarget, settings_path: &Path) -> Result<String, String> {
    let report = generate_harness_hook_install(&options(target, settings_path, false, false))
        .map_err(|error| error.message())?;
    Ok(report.install_audit.status)
}

#[test]
fn install_audit_reports_missing_fresh_and_docs() -> TestResult {
    let temp = TempDir::new().map_err(|error| error.to_string())?;
    let settings_path = temp.path().join("codex-hooks.json");

    let missing = generate_harness_hook_install(&options(
        HarnessHookTarget::Codex,
        &settings_path,
        false,
        false,
    ))
    .map_err(|error| error.message())?;
    if missing.install_audit.status != "missing_hook" {
        return Err(format!(
            "missing config should report missing_hook, got {}",
            missing.install_audit.status
        ));
    }
    if missing.install_audit.hook_missing_count == 0 {
        return Err("missing audit should count missing installable hooks".to_owned());
    }
    if missing.written_paths.len() != 0 || settings_path.exists() {
        return Err("read-only audit must not write a missing settings file".to_owned());
    }
    for doc_id in ["recall_hooks", "primer_hooks", "journal_hooks"] {
        if !missing
            .install_audit
            .docs
            .iter()
            .any(|doc| doc.id == doc_id)
        {
            return Err(format!("missing audit doc link {doc_id}"));
        }
    }
    if !missing
        .install_audit
        .repair_plan
        .iter()
        .any(|repair| repair.action == "install_or_refresh_hooks" && repair.mutates_state)
    {
        return Err("missing audit should emit explicit install repair plan".to_owned());
    }

    generate_harness_hook_install(&options(
        HarnessHookTarget::Codex,
        &settings_path,
        true,
        false,
    ))
    .map_err(|error| error.message())?;
    let fresh = generate_harness_hook_install(&options(
        HarnessHookTarget::Codex,
        &settings_path,
        false,
        false,
    ))
    .map_err(|error| error.message())?;
    if fresh.install_audit.status != "fresh" {
        return Err(format!(
            "installed hooks should report fresh, got {}",
            fresh.install_audit.status
        ));
    }
    if fresh.install_audit.hook_fresh_count == 0 || fresh.install_audit.hook_stale_count != 0 {
        return Err("fresh audit should count fresh hooks and no stale hooks".to_owned());
    }
    if !fresh.install_audit.repair_plan.is_empty() {
        return Err("fresh audit should not emit repair actions".to_owned());
    }

    Ok(())
}

#[test]
fn install_audit_reports_stale_managed_hooks() -> TestResult {
    let temp = TempDir::new().map_err(|error| error.to_string())?;
    let settings_path = temp.path().join("claude-settings.json");
    generate_harness_hook_install(&options(
        HarnessHookTarget::ClaudeCode,
        &settings_path,
        true,
        false,
    ))
    .map_err(|error| error.message())?;

    let mut document: Value =
        serde_json::from_slice(&fs::read(&settings_path).map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())?;
    document["hooks"]["PreToolUse"][0]["hooks"][0]["command"] =
        Value::String("python3 -c 'print(\"stale\")'".to_owned());
    fs::write(
        &settings_path,
        serde_json::to_vec_pretty(&document).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;

    let report = generate_harness_hook_install(&options(
        HarnessHookTarget::ClaudeCode,
        &settings_path,
        false,
        false,
    ))
    .map_err(|error| error.message())?;
    if report.install_audit.status != "stale_hook" {
        return Err(format!(
            "modified managed hook should report stale_hook, got {}",
            report.install_audit.status
        ));
    }
    if !report
        .install_audit
        .findings
        .iter()
        .any(|finding| finding.code == "stale_hook")
    {
        return Err("stale audit should include a stale_hook finding".to_owned());
    }
    if !report
        .install_audit
        .repair_plan
        .iter()
        .any(|repair| repair.action == "install_or_refresh_hooks")
    {
        return Err("stale audit should include refresh repair action".to_owned());
    }
    Ok(())
}

#[test]
fn install_audit_reports_unsupported_and_unwritable_config() -> TestResult {
    let temp = TempDir::new().map_err(|error| error.to_string())?;
    let gemini_path = temp.path().join("gemini-settings.json");
    if audit_status(HarnessHookTarget::Gemini, &gemini_path)? != "unsupported_harness_version" {
        return Err("Gemini should report unsupported_harness_version".to_owned());
    }

    let readonly_path = temp.path().join("readonly-codex-hooks.json");
    fs::write(&readonly_path, "{}\n").map_err(|error| error.to_string())?;
    let mut permissions = fs::metadata(&readonly_path)
        .map_err(|error| error.to_string())?
        .permissions();
    permissions.set_readonly(true);
    fs::set_permissions(&readonly_path, permissions).map_err(|error| error.to_string())?;

    let status = audit_status(HarnessHookTarget::Codex, &readonly_path)?;
    let mut permissions = fs::metadata(&readonly_path)
        .map_err(|error| error.to_string())?
        .permissions();
    permissions.set_readonly(false);
    fs::set_permissions(&readonly_path, permissions).map_err(|error| error.to_string())?;

    if status != "config_not_writable" {
        return Err(format!(
            "read-only settings file should report config_not_writable, got {status}"
        ));
    }
    Ok(())
}
