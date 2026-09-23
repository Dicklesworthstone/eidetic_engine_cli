//! Reversible, derived-only index repair through the existing mutation log.
//!
//! Build and validate away from the live index first. Under the ordinary
//! generation lease, retire the old admission marker, journal each file change,
//! then publish the new marker last. Undo takes the same lease and replays the
//! existing hash-checked primitive inverses; no new trusted action kind is added.

use std::collections::BTreeSet;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::{
    ActionLine, DoctorRuntimeError, Op, RunContext, configure_doctor_inspect_open_no_follow,
    ensure_doctor_lifecycle_bindings, is_mutating_action_kind, mutate,
    validate_doctor_lifecycle_paths,
};
use crate::core::index::{IndexGenerationLease, doctor_repair};

const MANIFEST: &str = "meta.json";

#[derive(Default)]
struct Inventory {
    files: BTreeSet<PathBuf>,
    directories: BTreeSet<PathBuf>,
}

impl Inventory {
    fn copy_budget(&self, root: &Path) -> Result<(u64, u64), DoctorRuntimeError> {
        let overflow = || repair_error("estimate doctor repair copy", "size or entry count overflow");
        let mut bytes = 0_u64;
        let mut entries = u64::try_from(self.directories.len()).map_err(|_| overflow())?;
        for relative in &self.files {
            let path = root.join(relative);
            validate_doctor_lifecycle_paths([path.as_path()])?;
            let metadata = fs::symlink_metadata(&path)?;
            if !metadata.is_file() {
                return Err(repair_error("estimate doctor repair copy", "file changed type"));
            }
            bytes = bytes.checked_add(metadata.len()).ok_or_else(&overflow)?;
            // Each backup has a sequence directory plus a sanitized absolute
            // path. Budget those directories and possible quarantine parents,
            // not just one inode per copied file.
            let path_entries = path.components().count()
                .checked_add(relative.components().count())
                .and_then(|value| value.checked_add(4))
                .ok_or_else(&overflow)?;
            entries = entries.checked_add(u64::try_from(path_entries).map_err(|_| overflow())?)
                .ok_or_else(&overflow)?;
        }
        Ok((bytes, entries))
    }
}

fn repair_error(context: &str, error: impl std::fmt::Display) -> DoctorRuntimeError {
    DoctorRuntimeError::Io {
        context: context.to_owned(),
        source: io::Error::other(error.to_string()),
    }
}

fn inventory(root: &Path) -> Result<Inventory, DoctorRuntimeError> {
    validate_doctor_lifecycle_paths([root])?;
    let mut result = Inventory::default();
    match fs::symlink_metadata(root) {
        Ok(metadata) if metadata.is_dir() => {}
        Ok(_) => return Err(repair_error("inspect index repair root", "expected a directory")),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(result),
        Err(error) => return Err(repair_error("inspect index repair root", error)),
    }
    let mut pending = vec![PathBuf::new()];
    while let Some(relative) = pending.pop() {
        let directory = root.join(&relative);
        validate_doctor_lifecycle_paths([directory.as_path()])?;
        result.directories.insert(relative.clone());
        for entry in fs::read_dir(&directory)? {
            let entry = entry?;
            let path = relative.join(entry.file_name());
            let kind = entry.file_type()?;
            if kind.is_dir() {
                pending.push(path);
            } else if kind.is_file() {
                result.files.insert(path);
            } else {
                return Err(repair_error(
                    "inspect index repair tree",
                    format!("refusing symlink or special entry {}", entry.path().display()),
                ));
            }
        }
    }
    Ok(result)
}

/// Called only after the parent mutation entry point validates the blast radius.
pub(super) fn rebuild(
    ctx: &mut RunContext,
    index: &Path,
) -> Result<ActionLine, DoctorRuntimeError> {
    let expected = crate::config::workspace::resolve_store_index_dir(&ctx.workspace, None, None);
    if index != expected {
        return Err(repair_error("select doctor index target", "target is not this workspace's index"));
    }
    // Reject planted redirects/special files before even building scratch data.
    inventory(index)?;
    let workspace = ctx.workspace.clone();
    let staging = ctx.run_dir.join(format!("index-stage-{:06}", ctx.state.action_count + 1));
    validate_doctor_lifecycle_paths([staging.as_path()])?;
    crate::core::run_cli_with_cx(Duration::from_secs(300), |cx| async move {
        let prepared = doctor_repair::stage(&cx, &workspace, &staging)
            .await
            .map_err(|error| repair_error("build doctor index repair", error))?;
        let staged = inventory(&staging)?;
        let _lease = doctor_repair::publication_lease(&cx, index)
            .await
            .map_err(|error| repair_error("fence doctor index repair", error))?;
        prepared.check_source_generation()
            .map_err(|error| repair_error("validate doctor index source", error))?;
        ensure_doctor_lifecycle_bindings(&ctx.lifecycle, &ctx.workspace, &ctx.run_dir)?;
        // Rescan after fencing: a normal publisher may have completed during
        // staging. Only this current tree is the before-state for our journal.
        let previous = inventory(index)?;
        let (new_bytes, new_entries) = staged.copy_budget(&staging)?;
        let (old_bytes, old_entries) = previous.copy_budget(index)?;
        let bytes = new_bytes.checked_add(old_bytes)
            .ok_or_else(|| repair_error("estimate doctor index publication", "byte count overflow"))?;
        let entries = new_entries.checked_add(old_entries)
            .ok_or_else(|| repair_error("estimate doctor index publication", "entry count overflow"))?;
        // Both roots may share a filesystem; checking the combined budget at
        // each is conservative even when the audit trail is on another mount.
        for destination in [index, ctx.run_dir.as_path()] {
            doctor_repair::admit_repair_copy(&cx, destination, bytes, entries)
                .map_err(|error| repair_error("reserve live index copy and undo backups", error))?;
        }
        let receipt = publish_files(ctx, index, &staging, &staged, &previous)?;
        prepared.check_source_generation()
            .map_err(|error| repair_error("source advanced during doctor index publication", error))?;
        Ok(receipt)
    })
    .map_err(|error| repair_error("start doctor index repair runtime", error))?
}

fn quarantine_file(ctx: &mut RunContext, index: &Path, relative: &Path) -> Result<(), DoctorRuntimeError> {
    let target = index.join(relative);
    validate_doctor_lifecycle_paths([target.as_path()])?;
    let destination = PathBuf::from("index-rebuild")
        .join(format!("{:06}", ctx.state.action_count + 1))
        .join(relative);
    mutate(ctx, &target, Op::QuarantineByRename { dest_under_quarantine: destination })?;
    Ok(())
}

fn write_staged_file(
    ctx: &mut RunContext,
    index: &Path,
    staging: &Path,
    relative: &Path,
) -> Result<ActionLine, DoctorRuntimeError> {
    let source = staging.join(relative);
    let target = index.join(relative);
    validate_doctor_lifecycle_paths([source.as_path(), target.as_path()])?;
    let mut options = fs::OpenOptions::new();
    options.read(true);
    configure_doctor_inspect_open_no_follow(&mut options);
    let mut file = options.open(&source)?;
    if !file.metadata()?.is_file() {
        return Err(repair_error("read staged index file", "source is not a regular file"));
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    mutate(ctx, &target, Op::WriteFile { bytes })
}

fn publish_files(
    ctx: &mut RunContext,
    index: &Path,
    staging: &Path,
    staged: &Inventory,
    previous: &Inventory,
) -> Result<ActionLine, DoctorRuntimeError> {
    let manifest = Path::new(MANIFEST);
    if !staged.files.contains(manifest) {
        return Err(repair_error("publish doctor index", "staged generation has no admission marker"));
    }
    if staged.files.iter().any(|path| previous.directories.contains(path))
        || staged.directories.iter().any(|path| previous.files.contains(path))
    {
        return Err(repair_error("publish doctor index", "file/directory type conflict; no repair was published"));
    }
    // Retire and persist the old admission marker BEFORE changing a tier.
    // A process failure must not expose an old marker over new tier files.
    if previous.files.contains(manifest) {
        quarantine_file(ctx, index, manifest)?;
        doctor_repair::flush_directory(index)
            .map_err(|error| repair_error("persist retired index admission marker", error))?;
    }
    for relative in staged.directories.difference(&previous.directories) {
        let path = index.join(relative);
        validate_doctor_lifecycle_paths([path.as_path()])?;
        mutate(ctx, &path, Op::CreateDirAll { mode: 0o700 })?;
    }
    for relative in &staged.files {
        if relative == manifest {
            continue;
        }
        match write_staged_file(ctx, index, staging, relative) {
            Ok(_) | Err(DoctorRuntimeError::NoOpIdempotent) => {}
            Err(error) => return Err(error),
        }
    }
    for relative in previous.files.difference(&staged.files) {
        quarantine_file(ctx, index, relative)?;
    }
    // The generic mutation primitive uses atomic replacement, but atomicity
    // alone does not order writes on persistent storage. Make the tier files,
    // inverse backups, and journal durable before making this generation ready.
    doctor_repair::flush_tree(index)
        .map_err(|error| repair_error("persist repaired index tiers", error))?;
    doctor_repair::flush_tree(&ctx.run_dir)
        .map_err(|error| repair_error("persist index repair inverse journal", error))?;
    // This receipt is a real write_file action, not a RunIndexRebuild promise.
    // All preceding mutations have their own backup/hash/sequence and are
    // undone in reverse before the original marker can become visible again.
    let receipt = write_staged_file(ctx, index, staging, manifest)?;
    doctor_repair::flush_tree(&ctx.run_dir)
        .map_err(|error| repair_error("persist final index repair receipt", error))?;
    doctor_repair::flush_tree(index)
        .map_err(|error| repair_error("persist repaired index admission marker", error))?;
    Ok(receipt)
}

pub(super) fn undo_lease(
    workspace: &Path,
    actions: &[ActionLine],
) -> Result<Option<IndexGenerationLease>, DoctorRuntimeError> {
    let index = crate::config::workspace::resolve_store_index_dir(workspace, None, None);
    if !actions.iter().any(|action| {
        is_mutating_action_kind(&action.kind) && action.path.starts_with(&index)
    }) {
        return Ok(None);
    }
    crate::core::run_cli_with_cx(Duration::from_secs(10), |cx| async move {
        doctor_repair::publication_lease(&cx, &index)
            .await
            .map(Some)
            .map_err(|error| repair_error("fence doctor index undo", error))
    })
    .map_err(|error| repair_error("start doctor index undo runtime", error))?
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;
    use super::super::{RunStatus, default_blast_radius_roots, replay_undo_for_workspace};
    use crate::db::{CreateMemoryInput, CreateWorkspaceInput, DbConnection};

    const WORKSPACE: &str = "wsp_00000000000000000000000081";
    const MEMORY: &str = "mem_00000000000000000000000081";

    fn fixture() -> (tempfile::TempDir, PathBuf) {
        let root = tempfile::tempdir().expect("fixture");
        let workspace = root.path().canonicalize().expect("physical path");
        fs::create_dir(workspace.join(".ee")).expect("store directory");
        let db = DbConnection::open_file(&workspace.join(".ee/ee.db")).expect("store");
        db.migrate().expect("schema");
        db.insert_workspace(WORKSPACE, &CreateWorkspaceInput {
            path: workspace.display().to_string(), name: Some("doctor repair".to_owned()),
        }).expect("workspace");
        db.insert_memory_revision(MEMORY, MEMORY, &CreateMemoryInput {
            workspace_id: WORKSPACE.to_owned(), level: "procedural".to_owned(), kind: "rule".to_owned(),
            content: "Run `cargo fmt --check` before publishing the release.".to_owned(),
            workflow_id: None, confidence: 0.9, utility: 0.5, importance: 0.5,
            provenance_uri: None, trust_class: "human_explicit".to_owned(), trust_subclass: None,
            tags: Vec::new(), valid_from: None, valid_to: None,
        }).expect("unanchored memory");
        drop(db);
        (root, workspace)
    }

    fn start(workspace: &Path) -> RunContext {
        RunContext::start(workspace, "index-repair-test", default_blast_radius_roots(workspace), false)
            .expect("doctor run")
    }

    fn repair(ctx: &mut RunContext, index: &Path) -> Result<ActionLine, DoctorRuntimeError> {
        mutate(ctx, index, Op::RunIndexRebuild { steps: vec!["ee index rebuild".to_owned()] })
    }

    #[test]
    fn missing_index_rebuild_preserves_source_and_undo_restores_absence() {
        let (_root, workspace) = fixture();
        let database = workspace.join(".ee/ee.db");
        let before = fs::read(&database).expect("source bytes");
        let index = workspace.join(".ee/index");
        let mut ctx = start(&workspace);
        let receipt = repair(&mut ctx, &index).expect("actual rebuild");
        assert_eq!(receipt.kind, "write_file");
        assert_eq!(receipt.path, index.join(MANIFEST));
        assert!(index.join("vector.fast.idx").is_file());
        #[cfg(feature = "lexical-bm25")]
        assert!(index.join("lexical/meta.json").is_file());
        let metadata: serde_json::Value = serde_json::from_slice(&fs::read(index.join(MANIFEST)).expect("metadata"))
            .expect("metadata JSON");
        assert_eq!(metadata["documentCounts"]["memories"], 1);
        assert_eq!(fs::read(&database).expect("unchanged source"), before);
        let db = DbConnection::open_file_read_only(&database).expect("read source");
        assert!(db.list_memory_anchors(MEMORY).expect("anchors").is_empty());
        drop(db);
        let summary = ctx.finish(RunStatus::CompletedOk).expect("finish");
        assert!(summary.action_count > 2);
        let undo = replay_undo_for_workspace(&workspace, &summary.run_id).expect("undo");
        assert_eq!(undo.status, RunStatus::Undone, "{:?}", undo.first_error);
        assert!(!index.exists());
        assert_eq!(fs::read(database).expect("source after undo"), before);
        let again = replay_undo_for_workspace(&workspace, &summary.run_id).expect("idempotent undo");
        assert_eq!(again.actions_undone, 0);
    }

    #[test]
    fn stale_files_and_original_manifest_are_restored_byte_for_byte() {
        let (_root, workspace) = fixture();
        let index = workspace.join(".ee/index");
        fs::create_dir(&index).expect("old index");
        fs::write(index.join(MANIFEST), b"old corrupt metadata").expect("old marker");
        fs::write(index.join("vector.fast.idx"), b"old corrupt vector").expect("old vector");
        fs::write(index.join("retired.segment"), b"preserve obsolete bytes").expect("old segment");
        let mut ctx = start(&workspace);
        repair(&mut ctx, &index).expect("repair stale index");
        assert!(!index.join("retired.segment").exists());
        let summary = ctx.finish(RunStatus::CompletedOk).expect("finish");
        let undo = replay_undo_for_workspace(&workspace, &summary.run_id).expect("undo");
        assert_eq!(undo.status, RunStatus::Undone, "{:?}", undo.first_error);
        assert_eq!(fs::read(index.join(MANIFEST)).expect("restored marker"), b"old corrupt metadata");
        assert_eq!(fs::read(index.join("vector.fast.idx")).expect("restored vector"), b"old corrupt vector");
        assert_eq!(fs::read(index.join("retired.segment")).expect("restored segment"), b"preserve obsolete bytes");
        assert_eq!(inventory(&index).expect("restored inventory").files.len(), 3);
    }

    #[test]
    fn repair_refuses_redirects_before_source_or_target_changes() {
        let (_root, workspace) = fixture();
        let outside = tempfile::tempdir().expect("outside");
        let protected = outside.path().join("protected");
        fs::write(&protected, b"never change").expect("sentinel");
        let index = workspace.join(".ee/index");
        fs::create_dir(&index).expect("index directory");
        std::os::unix::fs::symlink(&protected, index.join("vector.fast.idx")).expect("redirect");
        let mut ctx = start(&workspace);
        assert!(repair(&mut ctx, &index).is_err());
        assert_eq!(ctx.state.action_count, 0);
        assert_eq!(fs::read(&protected).expect("sentinel"), b"never change");
        ctx.finish(RunStatus::CompletedPartial).expect("finish");
    }

    #[test]
    fn dry_run_does_not_build_an_index_or_modify_the_store() {
        let (_root, workspace) = fixture();
        let database = workspace.join(".ee/ee.db");
        let before = fs::read(&database).expect("source bytes");
        let mut ctx = RunContext::start(&workspace, "dry-index", default_blast_radius_roots(&workspace), true)
            .expect("dry run");
        let receipt = repair(&mut ctx, &workspace.join(".ee/index")).expect("plan");
        assert_eq!(receipt.kind, "run_index_rebuild");
        assert!(!workspace.join(".ee/index").exists());
        assert!(!ctx.run_dir.join("index-stage-000001").exists());
        assert_eq!(fs::read(database).expect("source unchanged"), before);
        ctx.finish(RunStatus::CompletedOk).expect("finish plan");
    }

    #[test]
    fn empty_corpus_rebuild_preserves_tombstones_and_remains_undoable() {
        let (_root, workspace) = fixture();
        let database = workspace.join(".ee/ee.db");
        let db = DbConnection::open_file(&database).expect("source writer");
        assert!(db.tombstone_memory(MEMORY).expect("tombstone last memory"));
        drop(db);
        let before = fs::read(&database).expect("tombstoned source bytes");
        let index = workspace.join(".ee/index");
        let mut ctx = start(&workspace);
        repair(&mut ctx, &index).expect("empty generation repair");
        let metadata: serde_json::Value = serde_json::from_slice(
            &fs::read(index.join(MANIFEST)).expect("empty metadata"),
        ).expect("metadata JSON");
        assert_eq!(metadata["documentCount"], 0);
        assert!(index.join("vector.fast.idx").is_file());
        assert_eq!(fs::read(&database).expect("source unchanged"), before);
        let summary = ctx.finish(RunStatus::CompletedOk).expect("finish");
        let undo = replay_undo_for_workspace(&workspace, &summary.run_id).expect("undo empty generation");
        assert_eq!(undo.status, RunStatus::Undone, "{:?}", undo.first_error);
        assert!(!index.exists());
        assert_eq!(fs::read(database).expect("tombstone preserved after undo"), before);
    }

    #[test]
    fn undo_refuses_a_newer_index_manifest_without_touching_tiers() {
        let (_root, workspace) = fixture();
        let index = workspace.join(".ee/index");
        let mut ctx = start(&workspace);
        repair(&mut ctx, &index).expect("repair missing index");
        let summary = ctx.finish(RunStatus::CompletedOk).expect("finish");
        let tier_before = fs::read(index.join("vector.fast.idx")).expect("repaired tier");
        let marker = index.join(MANIFEST);
        let mut metadata: serde_json::Value = serde_json::from_slice(
            &fs::read(&marker).expect("original repaired manifest"),
        ).expect("metadata JSON");
        metadata["generation"] = serde_json::json!(999_999);
        metadata["sourceGeneration"] = serde_json::json!(999_999);
        let newer = serde_json::to_vec(&metadata).expect("newer metadata");
        fs::write(&marker, &newer).expect("simulate a later publisher");
        let undo = replay_undo_for_workspace(&workspace, &summary.run_id).expect("bounded undo refusal");
        assert_eq!(undo.status, RunStatus::UndonePartial);
        assert_eq!(undo.actions_undone, 0);
        assert!(undo.first_error.is_some());
        assert_eq!(fs::read(marker).expect("newer manifest preserved"), newer);
        assert_eq!(fs::read(index.join("vector.fast.idx")).expect("tier preserved"), tier_before);
    }

    #[test]
    fn an_unregistered_store_cannot_destroy_an_existing_index() {
        let root = tempfile::tempdir().expect("fixture");
        let workspace = root.path().canonicalize().expect("physical path");
        let index = workspace.join(".ee/index");
        fs::create_dir_all(&index).expect("existing index");
        fs::write(index.join(MANIFEST), b"preserved").expect("existing marker");
        let mut ctx = start(&workspace);
        assert!(repair(&mut ctx, &index).is_err());
        assert_eq!(ctx.state.action_count, 0);
        assert_eq!(fs::read(index.join(MANIFEST)).expect("marker preserved"), b"preserved");
        assert!(!workspace.join(".ee/ee.db").exists());
        ctx.finish(RunStatus::CompletedPartial).expect("finish");
    }
}
