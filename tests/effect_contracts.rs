//! Effect contract integration tests (EE-TST-009).
//!
//! Verifies that read-only commands do not mutate workspace state,
//! and that the effect manifest accurately reflects command behavior.

use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::process::Command;

type TestResult = Result<(), String>;

fn ensure<T: std::fmt::Debug + PartialEq>(actual: T, expected: T, ctx: &str) -> TestResult {
    if actual == expected {
        Ok(())
    } else {
        Err(format!("{ctx}: expected {expected:?}, got {actual:?}"))
    }
}

fn hash_file(path: &Path) -> Option<u64> {
    let content = fs::read(path).ok()?;
    let mut hasher = DefaultHasher::new();
    content.hash(&mut hasher);
    Some(hasher.finish())
}

fn hash_directory(path: &Path) -> u64 {
    let mut hasher = DefaultHasher::new();
    if let Ok(entries) = fs::read_dir(path) {
        let mut paths: Vec<_> = entries.filter_map(|e| e.ok()).collect();
        paths.sort_by_key(|e| e.path());
        for entry in paths {
            let p = entry.path();
            p.to_string_lossy().hash(&mut hasher);
            if p.is_file() {
                if let Some(h) = hash_file(&p) {
                    h.hash(&mut hasher);
                }
            } else if p.is_dir() {
                hash_directory(&p).hash(&mut hasher);
            }
        }
    }
    hasher.finish()
}

fn run_ee(args: &[&str]) -> Result<std::process::Output, String> {
    let output = Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(args)
        .output()
        .map_err(|e| format!("Failed to run ee: {e}"))?;
    Ok(output)
}

// ============================================================================
// No-Mutation Contract Tests
// ============================================================================

#[test]
fn status_command_does_not_mutate_workspace() -> TestResult {
    let temp = tempfile::tempdir().map_err(|e| format!("tempdir: {e}"))?;
    let workspace = temp.path();
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be valid UTF-8".to_string())?;

    let before = hash_directory(workspace);
    let output = run_ee(&["status", "--workspace", workspace_arg, "--json"])?;
    let after = hash_directory(workspace);

    ensure(
        output.status.success() || output.status.code() == Some(3),
        true,
        "status exits 0 or 3",
    )?;
    ensure(before, after, "workspace unchanged by status command")
}

#[test]
fn check_command_does_not_mutate_workspace() -> TestResult {
    let temp = tempfile::tempdir().map_err(|e| format!("tempdir: {e}"))?;
    let workspace = temp.path();
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be valid UTF-8".to_string())?;

    let before = hash_directory(workspace);
    let output = run_ee(&["check", "--workspace", workspace_arg, "--json"])?;
    let after = hash_directory(workspace);

    ensure(
        output.status.success() || output.status.code() == Some(3),
        true,
        "check exits 0 or 3",
    )?;
    ensure(before, after, "workspace unchanged by check command")
}

#[test]
fn capabilities_command_does_not_mutate_workspace() -> TestResult {
    let temp = tempfile::tempdir().map_err(|e| format!("tempdir: {e}"))?;
    let workspace = temp.path();

    let before = hash_directory(workspace);
    let output = run_ee(&["capabilities", "--json"])?;
    let after = hash_directory(workspace);

    ensure(output.status.success(), true, "capabilities succeeds")?;
    ensure(before, after, "workspace unchanged by capabilities command")
}

#[test]
fn version_command_does_not_mutate_workspace() -> TestResult {
    let temp = tempfile::tempdir().map_err(|e| format!("tempdir: {e}"))?;
    let workspace = temp.path();

    let before = hash_directory(workspace);
    let output = run_ee(&["version", "--json"])?;
    let after = hash_directory(workspace);

    ensure(output.status.success(), true, "version succeeds")?;
    ensure(before, after, "workspace unchanged by version command")
}

#[test]
fn health_command_does_not_mutate_workspace() -> TestResult {
    let temp = tempfile::tempdir().map_err(|e| format!("tempdir: {e}"))?;
    let workspace = temp.path();

    let before = hash_directory(workspace);
    let output = run_ee(&["health", "--json"])?;
    let after = hash_directory(workspace);

    ensure(
        output.status.success() || output.status.code() == Some(6),
        true,
        "health exits 0 or 6",
    )?;
    ensure(before, after, "workspace unchanged by health command")
}

#[test]
fn introspect_command_does_not_mutate_workspace() -> TestResult {
    let temp = tempfile::tempdir().map_err(|e| format!("tempdir: {e}"))?;
    let workspace = temp.path();

    let before = hash_directory(workspace);
    let output = run_ee(&["introspect", "--json"])?;
    let after = hash_directory(workspace);

    ensure(output.status.success(), true, "introspect succeeds")?;
    ensure(before, after, "workspace unchanged by introspect command")
}

// ============================================================================
// Effect Manifest Contract Tests
// ============================================================================

#[test]
fn effect_manifest_includes_status_as_read_only() -> TestResult {
    use ee::core::effect::{EffectClass, EffectManifest};

    let manifest = EffectManifest::build();
    let status = manifest
        .get("status")
        .ok_or_else(|| "status not in manifest".to_string())?;

    ensure(
        status.default_effect,
        EffectClass::ReadOnly,
        "status is read_only",
    )?;
    ensure(status.is_safe_mid_task(), true, "status is safe mid-task")
}

#[test]
fn effect_manifest_includes_remember_as_durable_write() -> TestResult {
    use ee::core::effect::{EffectClass, EffectManifest};

    let manifest = EffectManifest::build();
    let remember = manifest
        .get("remember")
        .ok_or_else(|| "remember not in manifest".to_string())?;

    ensure(
        remember.default_effect,
        EffectClass::DurableMemoryWrite,
        "remember is durable_memory_write",
    )?;
    ensure(
        remember.is_safe_mid_task(),
        false,
        "remember is not safe mid-task",
    )?;
    ensure(remember.requires_audit, true, "remember requires audit")
}

#[test]
fn effect_manifest_includes_index_rebuild_as_derived_write() -> TestResult {
    use ee::core::effect::{EffectClass, EffectManifest};

    let manifest = EffectManifest::build();
    let rebuild = manifest
        .get("index rebuild")
        .ok_or_else(|| "index rebuild not in manifest".to_string())?;

    ensure(
        rebuild.default_effect,
        EffectClass::DerivedArtifactWrite,
        "index rebuild is derived_artifact_write",
    )?;
    ensure(
        rebuild.write_surfaces.derived_paths.contains(&".ee/index/"),
        true,
        "index rebuild writes to .ee/index/",
    )
}

#[test]
fn effect_manifest_safe_commands_count_matches_read_only() -> TestResult {
    use ee::core::effect::{EffectClass, EffectManifest};

    let manifest = EffectManifest::build();
    let safe = manifest.safe_mid_task_commands();

    for effect in safe {
        ensure(
            effect.default_effect,
            EffectClass::ReadOnly,
            &format!("{} is read_only", effect.command_path),
        )?;
    }
    Ok(())
}

#[test]
fn effect_manifest_mutating_commands_have_non_empty_write_surfaces() -> TestResult {
    use ee::core::effect::{EffectClass, EffectManifest};

    let manifest = EffectManifest::build();

    for effect in manifest.mutating_commands() {
        if effect.default_effect == EffectClass::ReadOnly {
            continue;
        }
        if effect.write_surfaces.is_empty() {
            return Err(format!(
                "Mutating command '{}' has empty write_surfaces",
                effect.command_path
            ));
        }
    }
    Ok(())
}
