//! Real CLI proof that read-only ask does not need a writer or mutate its store.
use super::*;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;

fn invoke(workspace: &Path, question: &str, flags: &[&str]) -> Result<Output, String> {
    crate::common_spawn::serialized_real_ee_with(|command| {
        command
            .arg("--json")
            .arg("--workspace")
            .arg(workspace)
            .arg("ask")
            .arg(question)
            .args(flags);
    })
    .map_err(|error| error.to_string())
}

fn response(output: &Output) -> Result<Value, String> {
    serde_json::from_slice(&output.stdout).map_err(|error| {
        format!(
            "invalid response ({error}); exit={:?}, stderr={}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

fn seed_workspace() -> Result<(tempfile::TempDir, PathBuf, PathBuf, String, String), String> {
    let (root, workspace, database) = super::super::build_empty_workspace()?;
    let db = DbConnection::open_file(&database).map_err(|error| error.to_string())?;
    let workspace_id = db
        .get_workspace_by_path(&workspace.to_string_lossy())
        .map_err(|error| error.to_string())?
        .ok_or("missing workspace")?
        .id;
    let id = seed(
        &db,
        &workspace_id,
        1,
        "Run cargo fmt before release.",
        "2000-01-01T00:00:00Z",
        None,
    )?;
    drop(db);
    Ok((root, workspace, database, workspace_id, id))
}

// SQLite's shared-memory file is transient reader coordination, not durable
// evidence. Everything else, including WAL and writer-owner epoch, is checked.
fn durable_files(directory: &Path) -> Result<BTreeMap<PathBuf, Vec<u8>>, String> {
    fn visit(path: &Path, files: &mut BTreeMap<PathBuf, Vec<u8>>) -> Result<(), String> {
        for entry in fs::read_dir(path).map_err(|error| error.to_string())? {
            let entry = entry.map_err(|error| error.to_string())?;
            let path = entry.path();
            let kind = entry.file_type().map_err(|error| error.to_string())?;
            if kind.is_dir() {
                visit(&path, files)?;
            } else if kind.is_file() && !entry.file_name().to_string_lossy().ends_with("-shm") {
                files.insert(
                    path.clone(),
                    fs::read(&path).map_err(|error| error.to_string())?,
                );
            }
        }
        Ok(())
    }
    let mut files = BTreeMap::new();
    visit(directory, &mut files)?;
    Ok(files)
}

#[test]
fn read_only_answers_abstentions_and_strict_failures_leave_durable_state_unchanged()
-> Result<(), String> {
    let (_root, workspace, _database, _workspace_id, id) = seed_workspace()?;
    let before = durable_files(&workspace)?;
    for (question, flags, exit) in [
        ("Run cargo fmt before release", vec!["--read-only"], 0),
        ("What is the database port?", vec!["--read-only"], 0),
        (
            "What is the database port?",
            vec!["--read-only", "--require-confidence", "1"],
            6,
        ),
    ] {
        let output = invoke(&workspace, question, &flags)?;
        assert_eq!(output.status.code(), Some(exit));
        let value = response(&output)?;
        if exit == 0 {
            assert_eq!(value["success"], true);
            if question == "Run cargo fmt before release" {
                assert_eq!(value["data"]["abstained"], false);
                assert_eq!(value["data"]["citations"][0]["memoryId"], id);
            } else {
                assert_eq!(value["data"]["abstained"], true);
            }
        } else {
            assert_eq!(value["success"], false);
        }
        assert_eq!(durable_files(&workspace)?, before, "{question} {flags:?}");
    }
    Ok(())
}

#[test]
fn ordinary_ask_keeps_learning_audits_and_matches_read_only_answers() -> Result<(), String> {
    use ee::db::audit_actions;

    let (_root, workspace, database, workspace_id, id) = seed_workspace()?;
    let before = DbConnection::open_file_read_only(&database)
        .map_err(|error| error.to_string())?
        .list_audit_entries(Some(&workspace_id), None)
        .map_err(|error| error.to_string())?;
    for question in ["Run cargo fmt before release", "What is the database port?"] {
        let read_only = invoke(&workspace, question, &["--read-only"])?;
        let ordinary = invoke(&workspace, question, &[])?;
        assert!(read_only.status.success());
        assert!(ordinary.status.success());
        assert_eq!(response(&read_only)?, response(&ordinary)?);
    }
    let db = DbConnection::open_file_read_only(&database).map_err(|error| error.to_string())?;
    let after = db
        .list_audit_entries(Some(&workspace_id), None)
        .map_err(|error| error.to_string())?;
    let added: Vec<_> = after
        .iter()
        .filter(|row| !before.iter().any(|old| old.id == row.id))
        .collect();
    assert!(
        added
            .iter()
            .any(|row| row.action == audit_actions::SEARCH_RETURNED_MEM
                && row.target_id.as_deref() == Some(id.as_str()))
    );
    assert!(
        added
            .iter()
            .any(|row| row.action == audit_actions::SEARCH_MISS_RECORDED)
    );
    assert!(
        added
            .iter()
            .all(|row| row.action == audit_actions::SEARCH_RETURNED_MEM
                || row.action == audit_actions::SEARCH_MISS_RECORDED)
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn read_only_ask_succeeds_while_another_process_owns_the_writer_fence() -> Result<(), String> {
    let (_root, workspace, database, _workspace_id, id) = seed_workspace()?;
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(database.with_extension("write.lock"))
        .map_err(|error| error.to_string())?;
    rustix::fs::flock(&lock, rustix::fs::FlockOperation::NonBlockingLockExclusive)
        .map_err(|error| error.to_string())?;
    let before = durable_files(&workspace)?;
    // The parent retains the real flock until after the child exits. No mock
    // store, timing race or sleep can let a writable implementation pass.
    let output = invoke(&workspace, "Run cargo fmt before release", &["--read-only"])?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(response(&output)?["data"]["citations"][0]["memoryId"], id);
    assert_eq!(durable_files(&workspace)?, before);
    drop(lock);
    Ok(())
}

#[cfg(unix)]
#[test]
fn read_only_ask_accepts_a_nonwritable_database_file() -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;

    let (_root, workspace, database, _workspace_id, id) = seed_workspace()?;
    let original = fs::metadata(&database)
        .map_err(|error| error.to_string())?
        .permissions();
    fs::set_permissions(&database, fs::Permissions::from_mode(0o444))
        .map_err(|error| error.to_string())?;
    let before = durable_files(&workspace)?;
    let result = invoke(&workspace, "Run cargo fmt before release", &["--read-only"]);
    fs::set_permissions(&database, original).map_err(|error| error.to_string())?;
    let output = result?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(response(&output)?["data"]["citations"][0]["memoryId"], id);
    assert_eq!(durable_files(&workspace)?, before);
    Ok(())
}

#[test]
fn ask_requires_explicit_migration_instead_of_upgrading_during_a_query() -> Result<(), String> {
    let (_root, workspace, database, _workspace_id, _id) = seed_workspace()?;
    let db = DbConnection::open_file(&database).map_err(|error| error.to_string())?;
    db.execute_raw("DELETE FROM ee_schema_migrations WHERE version = (SELECT MAX(version) FROM ee_schema_migrations)")
        .map_err(|error| error.to_string())?;
    assert!(db.needs_migration().map_err(|error| error.to_string())?);
    drop(db);
    let before = durable_files(&workspace)?;
    for flags in [vec!["--read-only"], vec![]] {
        let output = invoke(&workspace, "Run cargo fmt before release", &flags)?;
        assert_eq!(output.status.code(), Some(8));
        assert_eq!(response(&output)?["success"], false);
        assert!(String::from_utf8_lossy(&output.stdout).contains("migrate"));
        assert_eq!(durable_files(&workspace)?, before);
    }
    Ok(())
}

#[test]
fn read_only_ask_never_initializes_a_missing_store() -> Result<(), String> {
    let root = tempfile::tempdir().map_err(|error| error.to_string())?;
    let output = invoke(
        root.path(),
        "Run cargo fmt before release",
        &["--read-only"],
    )?;
    assert!(!output.status.success());
    assert_eq!(response(&output)?["success"], false);
    assert!(!root.path().join(".ee").exists());
    Ok(())
}

#[test]
fn read_only_flag_and_effect_manifest_agree_on_actual_audit_behavior() -> Result<(), String> {
    use clap::Parser;
    use ee::cli::{Cli, NormalizedInvocation};
    use ee::core::effect::{EffectClass, EffectManifest, SideEffectClass};

    let manifest = EffectManifest::build();
    for (flags, expected) in [(vec![], "ask"), (vec!["--read-only"], "ask --read-only")] {
        let mut args = vec!["ee", "ask", "release"];
        args.extend(flags);
        let cli = Cli::try_parse_from(args).map_err(|error| error.to_string())?;
        let invocation = NormalizedInvocation::from_cli(&cli, &[]);
        assert_eq!(invocation.command_path, expected);
        let effect = manifest.get(expected).ok_or("missing ask effect")?;
        assert!(effect.requires_read_snapshot);
        assert!(effect.dry_run_effect.is_none());
        assert!(effect.mutation_contract.dry_run_behavior.is_none());
        if expected == "ask" {
            assert_eq!(effect.default_effect, EffectClass::DurableMemoryWrite);
            assert_eq!(
                effect.mutation_contract.side_effect_class,
                SideEffectClass::AppendOnly
            );
            assert_eq!(effect.write_surfaces.db_tables, ["audit_log"]);
            assert!(effect.requires_audit);
        } else {
            assert_eq!(effect.default_effect, EffectClass::ReadOnly);
            assert!(effect.write_surfaces.is_empty());
            assert!(!effect.requires_audit);
        }
    }
    Ok(())
}
