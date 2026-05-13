//! bd-2gill — schema validator for `ee.audit.install_pipeline.v1`.
//!
//! Runs `scripts/audit_install_pipeline.sh` (or uses an existing
//! `tests/audit_artifacts/latest_install_pipeline.json` symlink),
//! parses the envelope, and asserts:
//!
//! 1. Schema field equals `ee.audit.install_pipeline.v1`.
//! 2. `decided_path` is one of {path_a_post_release, path_b_pre_release}.
//! 3. `decision_inputs` is a JSON object (not a string), with the
//!    boolean signal fields.
//! 4. Each probe sub-object has a `probe_status` string.
//! 5. Crates.io owner provenance is structured when the crate is claimed.
//! 6. Release workflow inventory proves tag dispatch, Sigstore bundles,
//!    clean-machine smoke coverage, rerunnable publication, and Homebrew
//!    update ordering.
//! 7. README installation claims are explicit about planned vs available
//!    paths while the project remains pre-release.
//! 8. CI exposes a scheduled/manual install-pipeline smoke gate.
//! 9. `next_actions` is a non-empty array of strings.
//!
//! The test does not require network access — it reads whatever the
//! latest audit run produced. If no audit artifact exists yet, the
//! test is skipped (so a clean checkout doesn't fail CI).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::path::PathBuf;

use serde_json::Value;

type TestResult = Result<(), String>;

fn audit_artifact_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("audit_artifacts")
        .join("latest_install_pipeline.json")
}

fn load_audit() -> Result<Option<Value>, String> {
    let path = audit_artifact_path();
    if !path.exists() {
        return Ok(None);
    }
    let bytes =
        std::fs::read(&path).map_err(|error| format!("read {}: {error}", path.display()))?;
    let value: Value = serde_json::from_slice(&bytes)
        .map_err(|error| format!("parse {}: {error}", path.display()))?;
    Ok(Some(value))
}

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

#[test]
fn schema_pin_is_v1() -> TestResult {
    let Some(audit) = load_audit()? else {
        eprintln!(
            "skipped: no audit artifact at {}",
            audit_artifact_path().display()
        );
        return Ok(());
    };
    let schema = audit
        .get("schema")
        .and_then(Value::as_str)
        .ok_or_else(|| "audit: missing top-level `schema`".to_string())?;
    ensure(
        schema == "ee.audit.install_pipeline.v1",
        format!(
            "audit: schema is `{schema}`; expected `ee.audit.install_pipeline.v1`. \
             Re-run `scripts/audit_install_pipeline.sh`."
        ),
    )
}

#[test]
fn decided_path_is_known() -> TestResult {
    let Some(audit) = load_audit()? else {
        return Ok(());
    };
    let allowed: BTreeSet<&str> = ["path_a_post_release", "path_b_pre_release"]
        .into_iter()
        .collect();
    let decided = audit
        .get("decided_path")
        .and_then(Value::as_str)
        .ok_or_else(|| "audit: missing `decided_path`".to_string())?;
    ensure(
        allowed.contains(decided),
        format!(
            "audit: decided_path `{decided}` not in {:?}. Update the script or this test \
             in the same PR if a new path is introduced.",
            allowed
        ),
    )
}

#[test]
fn decision_inputs_is_object_with_boolean_signals() -> TestResult {
    let Some(audit) = load_audit()? else {
        return Ok(());
    };
    let inputs = audit
        .get("decision_inputs")
        .ok_or_else(|| "audit: missing `decision_inputs`".to_string())?;
    let obj = inputs
        .as_object()
        .ok_or_else(|| "audit: `decision_inputs` is not a JSON object".to_string())?;
    for required in [
        "gh_releases",
        "latest_release_published",
        "release_assets_complete",
        "crates_name_claimed",
        "crates_points_at_project",
        "crates_version_matches_release",
        "tap_formula_present",
        "tap_version_matches_release",
    ] {
        let v = obj
            .get(required)
            .ok_or_else(|| format!("audit: decision_inputs missing `{required}`"))?;
        if !v.is_boolean() {
            return Err(format!(
                "audit: decision_inputs.`{required}` is not a boolean; got `{}`",
                v
            ));
        }
    }
    for required in ["release_tag", "release_version"] {
        let v = obj
            .get(required)
            .ok_or_else(|| format!("audit: decision_inputs missing `{required}`"))?;
        if !v.is_string() {
            return Err(format!(
                "audit: decision_inputs.`{required}` is not a string; got `{}`",
                v
            ));
        }
    }
    Ok(())
}

#[test]
fn path_a_requires_project_owned_install_surfaces() -> TestResult {
    let Some(audit) = load_audit()? else {
        return Ok(());
    };
    if audit.get("decided_path").and_then(Value::as_str) != Some("path_a_post_release") {
        return Ok(());
    }
    let inputs = audit
        .get("decision_inputs")
        .and_then(Value::as_object)
        .ok_or_else(|| "audit: missing `decision_inputs` object".to_string())?;
    for required in [
        "gh_releases",
        "latest_release_published",
        "release_assets_complete",
        "crates_name_claimed",
        "crates_points_at_project",
        "crates_version_matches_release",
        "tap_formula_present",
        "tap_version_matches_release",
    ] {
        ensure(
            inputs.get(required).and_then(Value::as_bool) == Some(true),
            format!("audit: path_a_post_release cannot be selected while `{required}` is false"),
        )?;
    }
    Ok(())
}

#[test]
fn crates_io_claimed_name_reports_repository_and_owners() -> TestResult {
    let Some(audit) = load_audit()? else {
        return Ok(());
    };
    let crates = audit
        .get("crates_io")
        .ok_or_else(|| "audit: missing `crates_io` object".to_string())?;
    if crates.get("name_claimed").and_then(Value::as_bool) != Some(true) {
        return Ok(());
    }

    let repository = crates
        .get("repository")
        .and_then(Value::as_str)
        .ok_or_else(|| "audit: claimed crate missing `repository` string".to_string())?;
    ensure(
        !repository.is_empty(),
        "audit: claimed crate repository is empty".to_string(),
    )?;

    let owners = crates
        .get("owners")
        .and_then(Value::as_array)
        .ok_or_else(|| "audit: claimed crate missing `owners` array".to_string())?;
    ensure(!owners.is_empty(), "audit: claimed crate owners is empty")?;
    for (index, owner) in owners.iter().enumerate() {
        let login = owner
            .get("login")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("audit: crates_io.owners[{index}] missing login"))?;
        ensure(
            !login.is_empty(),
            format!("audit: crates_io.owners[{index}].login is empty"),
        )?;
    }
    Ok(())
}

#[test]
fn github_release_assets_are_structured() -> TestResult {
    let Some(audit) = load_audit()? else {
        return Ok(());
    };
    let assets = audit
        .get("github_release_assets")
        .ok_or_else(|| "audit: missing `github_release_assets` object".to_string())?;
    let assets = assets
        .as_object()
        .ok_or_else(|| "audit: `github_release_assets` is not a JSON object".to_string())?;
    for required in ["probe_status", "tag"] {
        let value = assets
            .get(required)
            .ok_or_else(|| format!("audit: github_release_assets missing `{required}`"))?;
        if !value.is_string() {
            return Err(format!(
                "audit: github_release_assets.`{required}` is not a string; got `{}`",
                value
            ));
        }
    }
    for required in ["expected_assets", "asset_names", "missing_assets"] {
        let value = assets
            .get(required)
            .ok_or_else(|| format!("audit: github_release_assets missing `{required}`"))?;
        if !value.is_array() {
            return Err(format!(
                "audit: github_release_assets.`{required}` is not an array; got `{}`",
                value
            ));
        }
    }
    let complete = assets.get("asset_matrix_complete").ok_or_else(|| {
        "audit: github_release_assets missing `asset_matrix_complete`".to_string()
    })?;
    if !complete.is_boolean() {
        return Err(format!(
            "audit: github_release_assets.asset_matrix_complete is not a boolean; got `{}`",
            complete
        ));
    }
    Ok(())
}

#[test]
fn each_probe_has_probe_status() -> TestResult {
    let Some(audit) = load_audit()? else {
        return Ok(());
    };
    for probe in ["github_releases", "crates_io", "homebrew_tap"] {
        let probe_obj = audit
            .get(probe)
            .ok_or_else(|| format!("audit: missing probe object `{probe}`"))?;
        let status = probe_obj
            .get("probe_status")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("audit: probe `{probe}` missing string `probe_status` field"))?;
        if status.is_empty() {
            return Err(format!("audit: probe `{probe}` has empty probe_status"));
        }
    }
    Ok(())
}

#[test]
fn next_actions_is_non_empty_string_array() -> TestResult {
    let Some(audit) = load_audit()? else {
        return Ok(());
    };
    let actions = audit
        .get("next_actions")
        .and_then(Value::as_array)
        .ok_or_else(|| "audit: missing or non-array `next_actions`".to_string())?;
    ensure(
        !actions.is_empty(),
        "audit: `next_actions` is empty".to_string(),
    )?;
    for (i, action) in actions.iter().enumerate() {
        if !action.is_string() {
            return Err(format!(
                "audit: next_actions[{i}] is not a string; got `{}`",
                action
            ));
        }
    }
    Ok(())
}

#[test]
fn release_workflow_inventory_present() -> TestResult {
    let Some(audit) = load_audit()? else {
        return Ok(());
    };
    let wf = audit
        .get("release_workflow")
        .ok_or_else(|| "audit: missing `release_workflow` object".to_string())?;
    // The script writes this as either a JSON object (when release.yml
    // exists) or the literal "no_release_yml" string fallback.
    if wf.is_string() {
        // Acceptable fallback when no release.yml; nothing else to check.
        return Ok(());
    }
    let workflow = wf
        .as_object()
        .ok_or_else(|| "audit: `release_workflow` is neither object nor string".to_string())?;
    for required in [
        "cosign_installer_present",
        "tag_trigger_present",
        "cosign_bundle_present",
        "macos_smoke_present",
        "smoke_uses_release_tag",
        "smoke_uses_release_installer_asset",
        "release_notes_use_release_assets",
        "release_notes_pin_installer_version",
        "release_notes_no_expanding_heredoc",
        "release_notes_git_log_pipe_safe",
        "release_notes_checkout_full_history",
        "smoke_runs_doctor",
        "release_publish_idempotent",
        "homebrew_after_smoke",
        "homebrew_missing_token_skip_present",
        "homebrew_pr_idempotent",
        "release_concurrency_present",
        "job_timeouts_present",
        "critical_bash_steps_strict",
        "release_requires_downloaded_artifacts",
        "smoke_version_assertions_present",
        "smoke_version_context_available",
    ] {
        let value = workflow
            .get(required)
            .ok_or_else(|| format!("audit: release_workflow missing `{required}`"))?;
        if !value.is_boolean() {
            return Err(format!(
                "audit: release_workflow.`{required}` is not a boolean; got `{}`",
                value
            ));
        }
    }

    let smoke_containers = workflow
        .get("smoke_containers")
        .and_then(Value::as_array)
        .ok_or_else(|| "audit: release_workflow missing `smoke_containers` array".to_string())?;
    let containers = smoke_containers
        .iter()
        .map(|value| {
            value.as_str().ok_or_else(|| {
                format!("audit: smoke_containers entry is not a string; got `{value}`")
            })
        })
        .collect::<Result<BTreeSet<_>, _>>()?;
    for required in ["ubuntu:24.04", "debian:bookworm-slim"] {
        ensure(
            containers.contains(required),
            format!("audit: release_workflow.smoke_containers missing `{required}`"),
        )?;
    }

    ensure(
        workflow.get("macos_smoke_present").and_then(Value::as_bool) == Some(true),
        "audit: release workflow is missing macOS smoke coverage",
    )?;
    ensure(
        workflow
            .get("smoke_uses_release_tag")
            .and_then(Value::as_bool)
            == Some(true),
        "audit: release smoke install is not pinned to the workflow release tag",
    )?;
    ensure(
        workflow
            .get("smoke_uses_release_installer_asset")
            .and_then(Value::as_bool)
            == Some(true),
        "audit: release smoke install is not using the attached release installer asset",
    )?;
    ensure(
        workflow
            .get("release_notes_use_release_assets")
            .and_then(Value::as_bool)
            == Some(true),
        "audit: release notes point installers at mutable branch content instead of release assets",
    )?;
    ensure(
        workflow
            .get("release_notes_pin_installer_version")
            .and_then(Value::as_bool)
            == Some(true),
        "audit: release notes fetch tag-specific installers but do not pin the installed version",
    )?;
    ensure(
        workflow
            .get("release_notes_no_expanding_heredoc")
            .and_then(Value::as_bool)
            == Some(true),
        "audit: release notes are generated with an expanding heredoc that can execute markdown fences",
    )?;
    ensure(
        workflow
            .get("release_notes_git_log_pipe_safe")
            .and_then(Value::as_bool)
            == Some(true),
        "audit: release notes git-log fallback is fragile under pipefail",
    )?;
    ensure(
        workflow
            .get("release_notes_checkout_full_history")
            .and_then(Value::as_bool)
            == Some(true),
        "audit: release notes checkout does not fetch tags/full history for git describe",
    )?;
    ensure(
        workflow.get("smoke_runs_doctor").and_then(Value::as_bool) == Some(true),
        "audit: release smoke coverage does not run ee doctor --json",
    )?;
    ensure(
        workflow
            .get("release_publish_idempotent")
            .and_then(Value::as_bool)
            == Some(true),
        "audit: release publication is not idempotent for reruns",
    )?;
    ensure(
        workflow
            .get("homebrew_after_smoke")
            .and_then(Value::as_bool)
            == Some(true),
        "audit: Homebrew update can run before release smoke tests pass",
    )?;
    ensure(
        workflow
            .get("homebrew_missing_token_skip_present")
            .and_then(Value::as_bool)
            == Some(true),
        "audit: Homebrew tap PR step does not skip cleanly when HOMEBREW_TAP_TOKEN is absent",
    )?;
    ensure(
        workflow
            .get("homebrew_pr_idempotent")
            .and_then(Value::as_bool)
            == Some(true),
        "audit: Homebrew tap PR step is not idempotent for reruns",
    )?;
    ensure(
        workflow
            .get("release_concurrency_present")
            .and_then(Value::as_bool)
            == Some(true),
        "audit: release workflow lacks non-cancelling concurrency protection",
    )?;
    ensure(
        workflow
            .get("job_timeouts_present")
            .and_then(Value::as_bool)
            == Some(true),
        "audit: release workflow jobs are missing explicit timeouts",
    )?;
    ensure(
        workflow
            .get("critical_bash_steps_strict")
            .and_then(Value::as_bool)
            == Some(true),
        "audit: critical release workflow Bash steps do not enable strict mode",
    )?;
    ensure(
        workflow
            .get("release_requires_downloaded_artifacts")
            .and_then(Value::as_bool)
            == Some(true),
        "audit: release publication can proceed without downloaded build artifacts",
    )?;
    ensure(
        workflow
            .get("smoke_version_assertions_present")
            .and_then(Value::as_bool)
            == Some(true),
        "audit: release smoke coverage does not assert installed ee version",
    )?;
    ensure(
        workflow
            .get("smoke_version_context_available")
            .and_then(Value::as_bool)
            == Some(true),
        "audit: release smoke jobs cannot read the check-version output they assert",
    )?;
    Ok(())
}

#[test]
fn readme_install_paths_match_audit_posture() -> TestResult {
    let Some(audit) = load_audit()? else {
        return Ok(());
    };
    let readme = audit
        .get("readme_installation")
        .ok_or_else(|| "audit: missing `readme_installation` object".to_string())?;
    let readme = readme
        .as_object()
        .ok_or_else(|| "audit: `readme_installation` is not a JSON object".to_string())?;

    for required in [
        "installation_status_section",
        "release_installer_marked_planned",
        "homebrew_marked_planned",
        "cargo_marked_planned",
        "source_build_marked_available",
        "release_installer_uses_release_assets",
        "release_installer_pins_version",
        "planned_markers_present",
    ] {
        let value = readme
            .get(required)
            .ok_or_else(|| format!("audit: readme_installation missing `{required}`"))?;
        if !value.is_boolean() {
            return Err(format!(
                "audit: readme_installation.`{required}` is not a boolean; got `{}`",
                value
            ));
        }
    }

    ensure(
        readme
            .get("planned_markers_present")
            .and_then(Value::as_bool)
            == Some(true),
        "audit: README install paths are not explicitly marked planned/available",
    )?;
    ensure(
        readme
            .get("release_installer_uses_release_assets")
            .and_then(Value::as_bool)
            == Some(true),
        "audit: README release installer example points at mutable branch content",
    )?;
    ensure(
        readme
            .get("release_installer_pins_version")
            .and_then(Value::as_bool)
            == Some(true),
        "audit: README release installer example does not pin the installed version",
    )
}

#[test]
fn installer_targets_match_release_matrix() -> TestResult {
    let Some(audit) = load_audit()? else {
        return Ok(());
    };
    let installer_assets = audit
        .get("installer_assets")
        .ok_or_else(|| "audit: missing `installer_assets` object".to_string())?;
    let installer_assets = installer_assets
        .as_object()
        .ok_or_else(|| "audit: `installer_assets` is not a JSON object".to_string())?;

    for required in [
        "unix_installer_supports_musl_asset",
        "installer_examples_use_release_assets",
        "unix_installer_normalizes_version_tag",
        "unix_installer_fails_on_bad_sigstore",
        "unix_installer_requires_sigstore_bundle_with_cosign",
        "unix_installer_requires_sha256_tool",
        "windows_installer_supports_x64_asset",
        "windows_installer_rejects_unbuilt_i686",
        "windows_installer_rejects_unbuilt_arm64",
        "windows_installer_normalizes_version_tag",
        "windows_installer_fails_on_bad_sigstore",
        "windows_installer_requires_sigstore_bundle_with_cosign",
        "homebrew_formula_tests_doctor_json",
        "homebrew_formula_fetches_sha_strictly",
        "homebrew_formula_normalizes_version_tag",
        "installer_targets_match_release_matrix",
    ] {
        let value = installer_assets
            .get(required)
            .ok_or_else(|| format!("audit: installer_assets missing `{required}`"))?;
        if !value.is_boolean() {
            return Err(format!(
                "audit: installer_assets.`{required}` is not a boolean; got `{}`",
                value
            ));
        }
    }

    ensure(
        installer_assets
            .get("installer_targets_match_release_matrix")
            .and_then(Value::as_bool)
            == Some(true),
        "audit: installer target detection does not match the release asset matrix",
    )
}

#[test]
fn ci_has_install_pipeline_smoke_gate() -> TestResult {
    let Some(audit) = load_audit()? else {
        return Ok(());
    };
    let ci = audit
        .get("ci_workflow")
        .ok_or_else(|| "audit: missing `ci_workflow` object".to_string())?;
    let ci = ci
        .as_object()
        .ok_or_else(|| "audit: `ci_workflow` is not a JSON object".to_string())?;

    for required in [
        "schedule_present",
        "workflow_dispatch_present",
        "verify_not_scheduled",
        "install_pipeline_smoke_present",
        "linux_runner_present",
        "macos_runner_present",
        "path_a_gate_present",
        "linux_docker_smoke_present",
        "pre_release_skip_present",
        "audit_artifact_upload_present",
        "smoke_uses_latest_release_tag",
        "smoke_uses_release_installer_asset",
        "homebrew_smoke_present",
        "cargo_smoke_present",
        "smoke_version_assertions_present",
        "weekly_smoke_ready",
    ] {
        let value = ci
            .get(required)
            .ok_or_else(|| format!("audit: ci_workflow missing `{required}`"))?;
        if !value.is_boolean() {
            return Err(format!(
                "audit: ci_workflow.`{required}` is not a boolean; got `{}`",
                value
            ));
        }
    }

    ensure(
        ci.get("weekly_smoke_ready").and_then(Value::as_bool) == Some(true),
        "audit: CI does not expose a scheduled/manual install-pipeline smoke gate",
    )?;
    ensure(
        ci.get("smoke_version_assertions_present")
            .and_then(Value::as_bool)
            == Some(true),
        "audit: CI install-pipeline smoke does not assert installed ee version",
    )
}
