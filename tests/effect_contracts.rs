//! Effect contract integration tests (EE-TST-009).
//!
//! Verifies that read-only commands do not mutate workspace state,
//! and that the effect manifest accurately reflects command behavior.

use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::process::Command;

const CLI_SOURCE: &str = include_str!("../src/cli/mod.rs");

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

fn command_paths_from_cli_extract_function() -> Result<Vec<String>, String> {
    let start_marker = "fn extract_command_path(cli: &Cli) -> String {";
    let end_marker = "\n    /// Returns a stable identifier";
    let start = CLI_SOURCE
        .find(start_marker)
        .ok_or_else(|| "extract_command_path function must exist".to_owned())?;
    let rest = &CLI_SOURCE[start..];
    let end = rest
        .find(end_marker)
        .ok_or_else(|| "extract_command_path function end marker must exist".to_owned())?;
    let function = &rest[..end];
    let mut commands = Vec::new();

    for line in function.lines() {
        let Some(to_string_at) = line.find(".to_string()") else {
            continue;
        };
        let prefix = &line[..to_string_at];
        let Some(last_quote) = prefix.rfind('"') else {
            continue;
        };
        let Some(first_quote) = prefix[..last_quote].rfind('"') else {
            continue;
        };
        commands.push(prefix[first_quote + 1..last_quote].to_owned());
    }

    commands.sort();
    commands.dedup();
    Ok(commands)
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
fn effect_manifest_distinguishes_append_only_writes() -> TestResult {
    use ee::core::effect::{EffectManifest, IdempotencyClass, SideEffectClass};

    let manifest = EffectManifest::build();

    for command in [
        "artifact register",
        "import cass",
        "import jsonl",
        "import eidetic-legacy",
    ] {
        let effect = manifest
            .get(command)
            .ok_or_else(|| format!("{command} not in manifest"))?;
        ensure(
            effect.mutation_contract.side_effect_class,
            SideEffectClass::AppendOnly,
            &format!("{command} is append-only"),
        )?;
        ensure(
            effect.idempotency,
            IdempotencyClass::Idempotent,
            &format!("{command} retries by idempotency key"),
        )?;
        ensure(
            effect.requires_audit,
            true,
            &format!("{command} writes audit"),
        )?;
    }
    Ok(())
}

#[test]
fn effect_manifest_imports_are_duplicate_safe_append_only() -> TestResult {
    use ee::core::effect::{EffectManifest, IdempotencyClass, SideEffectClass};

    let manifest = EffectManifest::build();

    for command in ["import cass", "import jsonl", "import eidetic-legacy"] {
        let effect = manifest
            .get(command)
            .ok_or_else(|| format!("{command} not in manifest"))?;
        let contract = &effect.mutation_contract;

        ensure(
            contract.side_effect_class,
            SideEffectClass::AppendOnly,
            &format!("{command} append-only contract"),
        )?;
        ensure(
            effect.idempotency,
            IdempotencyClass::Idempotent,
            &format!("{command} retries are idempotent"),
        )?;
        ensure(
            contract.idempotency_key,
            Some("source hash"),
            &format!("{command} duplicate key"),
        )?;
        ensure(
            contract
                .db_generation_effect
                .contains("unchanged when idempotency key matches"),
            true,
            &format!("{command} duplicate import leaves DB generation unchanged"),
        )?;
        ensure(
            contract
                .dry_run_behavior
                .is_some_and(|behavior| behavior.contains("no DB rows")),
            true,
            &format!("{command} dry-run is a storage no-op"),
        )?;
        ensure(
            contract.recovery_behavior.contains("partial append"),
            true,
            &format!("{command} failed transaction rolls back partial append"),
        )?;
        ensure(
            effect.requires_audit,
            true,
            &format!("{command} audit required"),
        )?;
        ensure(
            effect.write_surfaces.db_tables.contains(&"audit_log"),
            true,
            &format!("{command} writes audit_log"),
        )?;
    }

    Ok(())
}

#[test]
fn effect_manifest_covers_all_normalized_cli_command_paths() -> TestResult {
    use ee::core::effect::EffectManifest;

    let commands = command_paths_from_cli_extract_function()?;
    ensure(commands.len(), 144, "normalized CLI command count")?;

    let manifest = EffectManifest::build();
    let missing = commands
        .iter()
        .filter(|command| manifest.get(command).is_none())
        .cloned()
        .collect::<Vec<_>>();

    if missing.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "effect manifest missing command paths: {missing:?}"
        ))
    }
}

#[test]
fn effect_manifest_has_no_undocumented_extra_cli_paths() -> TestResult {
    use std::collections::BTreeSet;

    use ee::core::effect::EffectManifest;

    let commands = command_paths_from_cli_extract_function()?;
    let command_set = commands.iter().map(String::as_str).collect::<BTreeSet<_>>();

    let manifest = EffectManifest::build();
    let manifest_only = manifest
        .command_paths()
        .into_iter()
        .filter(|command| !command_set.contains(command))
        .collect::<Vec<_>>();

    if manifest_only.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "effect manifest has command paths not emitted by CLI normalization: {manifest_only:?}"
        ))
    }
}

#[test]
fn effect_manifest_includes_config_write_commands() -> TestResult {
    use ee::core::effect::{EffectClass, EffectManifest, SideEffectClass};

    let manifest = EffectManifest::build();

    for command in ["init", "workspace alias"] {
        let effect = manifest
            .get(command)
            .ok_or_else(|| format!("{command} not in manifest"))?;
        ensure(
            effect.default_effect,
            EffectClass::ConfigWrite,
            &format!("{command} writes configuration"),
        )?;
        ensure(
            effect.mutation_contract.side_effect_class,
            SideEffectClass::AuditedMutation,
            &format!("{command} has audited config contract"),
        )?;
        ensure(
            effect.requires_audit,
            true,
            &format!("{command} requires audit"),
        )?;
    }
    Ok(())
}

#[test]
fn effect_manifest_tracks_degraded_unavailable_paths_as_non_mutating() -> TestResult {
    use ee::core::effect::{EffectClass, EffectManifest, SideEffectClass};

    let manifest = EffectManifest::build();

    for (command, code) in [
        ("audit timeline", "audit_log_unavailable"),
        ("certificate verify", "certificate_store_unavailable"),
        ("claim verify", "claim_verification_unavailable"),
        ("demo run", "demo_command_execution_unavailable"),
        ("eval list", "eval_fixtures_unavailable"),
        ("handoff create", "handoff_unavailable"),
        ("procedure export", "procedure_store_unavailable"),
        ("rehearse run", "rehearsal_unavailable"),
        ("support bundle", "support_bundle_unavailable"),
        ("tripwire check", "tripwire_store_unavailable"),
    ] {
        let effect = manifest
            .get(command)
            .ok_or_else(|| format!("{command} not in manifest"))?;
        ensure(
            effect.default_effect,
            EffectClass::ReadOnly,
            &format!("{command} has no mutation while unavailable"),
        )?;
        ensure(
            effect.mutation_contract.side_effect_class,
            SideEffectClass::DegradedUnavailable,
            &format!("{command} degraded class"),
        )?;
        ensure(
            effect.write_surfaces.is_empty(),
            true,
            &format!("{command} has no write surfaces"),
        )?;
        ensure(
            effect.mutation_contract.audit_surface,
            None,
            &format!("{command} does not write audit while unavailable"),
        )?;
        ensure(
            effect.mutation_contract.degraded_code,
            Some(code),
            &format!("{command} degraded code"),
        )?;
    }
    Ok(())
}

#[test]
fn effect_manifest_backup_restore_have_side_path_no_delete_contracts() -> TestResult {
    use ee::core::effect::{EffectClass, EffectManifest, SideEffectClass};

    let manifest = EffectManifest::build();

    for command in ["backup create", "backup restore"] {
        let effect = manifest
            .get(command)
            .ok_or_else(|| format!("{command} not in manifest"))?;
        let contract = &effect.mutation_contract;

        ensure(
            effect.default_effect,
            EffectClass::WorkspaceFileWrite,
            &format!("{command} writes only side-path workspace files"),
        )?;
        ensure(
            effect.dry_run_effect,
            Some(EffectClass::ReadOnly),
            &format!("{command} dry-run is read-only"),
        )?;
        ensure(
            effect.write_surfaces.workspace_files.is_empty(),
            false,
            &format!("{command} names side-path file surfaces"),
        )?;
        ensure(
            effect.requires_audit,
            true,
            &format!("{command} side-path manifest requires audit"),
        )?;
        ensure(
            contract.side_effect_class,
            SideEffectClass::SidePathArtifact,
            &format!("{command} side-path class"),
        )?;
        ensure(
            contract.audit_surface.is_some(),
            true,
            &format!("{command} names manifest or audit surface"),
        )?;
        ensure(
            contract
                .db_generation_effect
                .contains("source DB generation unchanged"),
            true,
            &format!("{command} leaves source DB generation unchanged"),
        )?;
        ensure(
            contract.index_generation_effect,
            "none",
            &format!("{command} does not rebuild indexes"),
        )?;
        ensure(
            contract
                .dry_run_behavior
                .is_some_and(|behavior| behavior.contains("no files are written")),
            true,
            &format!("{command} dry-run writes no files"),
        )?;
        ensure(
            contract.no_overwrite_behavior.is_some_and(|policy| {
                policy.contains("no-overwrite") && policy.contains("no-delete")
            }),
            true,
            &format!("{command} has no-overwrite/no-delete policy"),
        )?;
        ensure(
            contract.recovery_behavior.contains("never deleted by ee"),
            true,
            &format!("{command} recovery never deletes partial output"),
        )?;
    }

    Ok(())
}

#[test]
fn effect_manifest_export_paths_do_not_write_side_paths_until_materialized() -> TestResult {
    use ee::core::effect::{EffectClass, EffectManifest};

    let manifest = EffectManifest::build();

    for command in ["graph export", "schema export", "procedure export"] {
        let effect = manifest
            .get(command)
            .ok_or_else(|| format!("{command} not in manifest"))?;

        ensure(
            effect.default_effect,
            EffectClass::ReadOnly,
            &format!("{command} is read-only today"),
        )?;
        ensure(
            effect.write_surfaces.is_empty(),
            true,
            &format!("{command} has no file write surface"),
        )?;
        ensure(
            effect.requires_audit,
            false,
            &format!("{command} writes no audit while read-only"),
        )?;
    }

    Ok(())
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

#[test]
fn effect_manifest_mutating_commands_have_complete_s43e_contracts() -> TestResult {
    use ee::core::effect::EffectManifest;

    let manifest = EffectManifest::build();

    for effect in manifest.mutating_commands() {
        let contract = &effect.mutation_contract;
        if contract.transaction_scope.is_none() {
            return Err(format!(
                "{} must declare transaction scope",
                effect.command_path
            ));
        }
        if contract.idempotency_key.is_none() {
            return Err(format!(
                "{} must declare idempotency behavior",
                effect.command_path
            ));
        }
        if contract.dry_run_behavior.is_none() {
            return Err(format!(
                "{} must declare dry-run no-op behavior",
                effect.command_path
            ));
        }
        if contract.recovery_behavior.is_empty() {
            return Err(format!(
                "{} must declare rollback/recovery behavior",
                effect.command_path
            ));
        }
        if contract.db_generation_effect.is_empty() || contract.index_generation_effect.is_empty() {
            return Err(format!(
                "{} must declare DB/index generation effects",
                effect.command_path
            ));
        }
    }
    Ok(())
}

#[test]
fn effect_manifest_runtime_classifier_covers_v6h4_command_classes() -> TestResult {
    use ee::core::effect::{EffectManifest, RuntimeClass};

    let manifest = EffectManifest::build();

    for (command, runtime_class, budget_required) in [
        ("status", RuntimeClass::Bounded, false),
        ("index rebuild", RuntimeClass::LongRunning, true),
        ("import cass", RuntimeClass::MultiStage, true),
        ("remember", RuntimeClass::MultiStage, true),
        ("backup create", RuntimeClass::MultiStage, true),
        ("daemon", RuntimeClass::Supervised, true),
    ] {
        let effect = manifest
            .get(command)
            .ok_or_else(|| format!("{command} not in manifest"))?;
        ensure(
            effect.runtime_contract.runtime_class,
            runtime_class,
            &format!("{command} runtime class"),
        )?;
        ensure(
            effect.runtime_contract.requires_budget(),
            budget_required,
            &format!("{command} budget requirement"),
        )?;
    }

    Ok(())
}

#[test]
fn effect_manifest_runtime_contracts_have_complete_v6h4_fields() -> TestResult {
    use ee::core::effect::EffectManifest;

    let manifest = EffectManifest::build();

    for command in manifest.command_paths() {
        let effect = manifest
            .get(command)
            .ok_or_else(|| format!("{command} not in manifest"))?;
        let runtime = &effect.runtime_contract;
        if runtime.runtime_class.as_str().is_empty() {
            return Err(format!("{command} has empty runtime class"));
        }
        if runtime.cancellation_points.is_empty() {
            return Err(format!("{command} has no cancellation checkpoints"));
        }
        if runtime.partial_progress_policy.is_empty() {
            return Err(format!("{command} has no partial-progress policy"));
        }
        if runtime.outcome_mapping.is_empty() {
            return Err(format!("{command} has no deterministic outcome mapping"));
        }
        if runtime.requires_budget() && runtime.default_budget_ms.is_none_or(|budget| budget == 0) {
            return Err(format!(
                "{command} requires a positive default runtime budget"
            ));
        }
    }

    Ok(())
}

#[test]
fn effect_manifest_runtime_budget_deadline_math_is_deterministic() -> TestResult {
    use ee::core::effect::EffectManifest;

    let manifest = EffectManifest::build();
    let rebuild = manifest
        .get("index rebuild")
        .ok_or_else(|| "index rebuild not in manifest".to_string())?;
    ensure(
        rebuild.runtime_contract.effective_budget_ms(None),
        Ok(Some(300_000)),
        "index rebuild default budget",
    )?;
    ensure(
        rebuild.runtime_contract.effective_budget_ms(Some(12_345)),
        Ok(Some(12_345)),
        "explicit budget overrides index default",
    )?;
    ensure(
        rebuild.runtime_contract.effective_budget_ms(Some(0)),
        Err("runtime budget must be greater than zero"),
        "zero budget rejected",
    )?;

    let support = manifest
        .get("support bundle")
        .ok_or_else(|| "support bundle not in manifest".to_string())?;
    ensure(
        support.runtime_contract.effective_budget_ms(None),
        Ok(None),
        "degraded unavailable command has no runtime budget by default",
    )
}

#[test]
fn effect_manifest_read_only_contracts_forbid_durable_mutation() -> TestResult {
    use ee::core::effect::EffectManifest;

    let manifest = EffectManifest::build();

    for effect in manifest.safe_mid_task_commands() {
        let contract = &effect.mutation_contract;
        ensure(
            contract.side_effect_class.declares_no_durable_mutation(),
            true,
            &format!("{} declares no durable mutation", effect.command_path),
        )?;
        ensure(
            contract.declares_no_source_mutation(),
            true,
            &format!("{} leaves source DB/index unchanged", effect.command_path),
        )?;
        ensure(
            contract.audit_surface,
            None,
            &format!("{} does not write audit", effect.command_path),
        )?;
    }
    Ok(())
}

#[test]
fn effect_manifest_side_path_artifacts_have_no_overwrite_policy() -> TestResult {
    use ee::core::effect::EffectManifest;

    let manifest = EffectManifest::build();

    for effect in manifest.mutating_commands() {
        let contract = &effect.mutation_contract;
        if contract.side_effect_class.requires_no_overwrite_contract() {
            let Some(policy) = contract.no_overwrite_behavior else {
                return Err(format!(
                    "{} must declare no-overwrite side-path behavior",
                    effect.command_path
                ));
            };
            if !policy.contains("no-overwrite") || !policy.contains("no-delete") {
                return Err(format!(
                    "{} must declare no-overwrite and no-delete side-path behavior",
                    effect.command_path
                ));
            }
            if !contract.recovery_behavior.contains("never deleted by ee") {
                return Err(format!(
                    "{} must not delete partial side-path output during recovery",
                    effect.command_path
                ));
            }
        }
    }
    Ok(())
}
