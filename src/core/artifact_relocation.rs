//! Preservation-only artifact relocation manifests.
//!
//! This workflow copies artifacts to a destination root and records a manifest.
//! It never removes originals and never overwrites an existing destination.

use std::fs;
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::time::UNIX_EPOCH;

use chrono::{SecondsFormat, Utc};
use serde::{Deserialize, Serialize};

use crate::models::DomainError;

pub const ARTIFACT_RELOCATION_SCHEMA_V1: &str = "ee.artifact.relocation.v1";

/// Hard upper bound on the byte length of an `ee.artifact.relocation.v1`
/// manifest file read by `read_relocation_manifest_file_no_follow`. The
/// manifest is a list of (source path, destination path, expected hash)
/// triples; realistic files are kilobytes, never megabytes. 4 MiB is the
/// parallel ceiling used by the round-2 cap pass on workspace metadata
/// files (`WORKSPACE_CONFIG_MAX_BYTES`, `CURATE_CONFIG_MAX_BYTES`,
/// `CONFIG_SURFACE_MAX_BYTES`, `PREFLIGHT_RULES_MAX_BYTES`,
/// `MAX_CLAIM_METADATA_BYTES`, `QOS_ACTIVE_LANE_REGISTRY_MAX_BYTES`).
///
/// Without this cap, `File::read_to_string` pre-sizes its destination
/// `String` from the file's metadata length on every supported platform,
/// so a user-supplied or accidentally-inflated multi-GiB manifest would
/// force a matching allocation on every `ee artifact relocate --apply`
/// invocation. The `read_manifest` pre-check at the caller only verifies
/// `is_file()` — no size guard.
const ARTIFACT_RELOCATION_MANIFEST_MAX_BYTES: u64 = 4 * 1024 * 1024;
const RELOCATION_DIR: &str = "ee-relocated-artifacts";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ArtifactRelocationMode {
    Plan,
    Apply,
    Restore,
}

impl ArtifactRelocationMode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Plan => "plan",
            Self::Apply => "apply",
            Self::Restore => "restore",
        }
    }
}

#[derive(Clone, Debug)]
pub struct ArtifactRelocationOptions<'a> {
    pub workspace_path: &'a Path,
    pub source_path: Option<&'a Path>,
    pub destination_root: Option<&'a Path>,
    pub manifest_path: &'a Path,
    pub actor: Option<&'a str>,
    pub mode: ArtifactRelocationMode,
    pub force_with_explicit_path: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactRelocationManifest {
    pub schema: String,
    pub command_version: String,
    pub actor: String,
    pub created_at: String,
    pub workspace_path: String,
    pub source_path: String,
    pub destination_root: String,
    pub restoration_command: String,
    pub force_with_explicit_path: bool,
    pub entries: Vec<ArtifactRelocationEntry>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactRelocationEntry {
    pub original_path: String,
    pub destination_path: String,
    pub kind: String,
    pub size_bytes: u64,
    pub mtime_unix_seconds: Option<u64>,
    pub blake3: Option<String>,
    pub status: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactRelocationReport {
    pub schema: &'static str,
    pub command: &'static str,
    pub mode: &'static str,
    pub applied: bool,
    pub restored: bool,
    pub manifest_path: String,
    pub manifest_hash: Option<String>,
    pub source_allowed: bool,
    pub preservation_policy: &'static str,
    pub manifest: ArtifactRelocationManifest,
    pub recovery_actions: Vec<ArtifactRelocationRecoveryAction>,
}

impl ArtifactRelocationReport {
    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        serde_json::json!(self.redacted_for_public_output())
    }

    #[must_use]
    pub fn human_summary(&self) -> String {
        format!(
            "artifact relocation {mode}\n\nentries: {entries}\nmanifest: {manifest}\napplied: {applied}\nrestored: {restored}\n",
            mode = self.mode,
            entries = self.manifest.entries.len(),
            manifest = redact_artifact_relocation_public_path(&self.manifest_path),
            applied = self.applied,
            restored = self.restored
        )
    }

    fn redacted_for_public_output(&self) -> Self {
        let mut report = self.clone();
        report.manifest_path = redact_artifact_relocation_public_path(&report.manifest_path);
        report.manifest.workspace_path =
            redact_artifact_relocation_public_path(&report.manifest.workspace_path);
        report.manifest.source_path =
            redact_artifact_relocation_public_path(&report.manifest.source_path);
        report.manifest.destination_root =
            redact_artifact_relocation_public_path(&report.manifest.destination_root);
        report.manifest.restoration_command =
            redact_artifact_relocation_public_path(&report.manifest.restoration_command);
        for entry in &mut report.manifest.entries {
            entry.original_path = redact_artifact_relocation_public_path(&entry.original_path);
            entry.destination_path =
                redact_artifact_relocation_public_path(&entry.destination_path);
        }
        for action in &mut report.recovery_actions {
            action.command = redact_artifact_relocation_public_path(&action.command);
        }
        report
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactRelocationRecoveryAction {
    pub priority: u8,
    pub kind: &'static str,
    pub command: String,
    pub reason: String,
}

pub fn relocate_artifacts(
    options: &ArtifactRelocationOptions<'_>,
) -> Result<ArtifactRelocationReport, DomainError> {
    match options.mode {
        ArtifactRelocationMode::Plan | ArtifactRelocationMode::Apply => {
            plan_or_apply_relocation(options)
        }
        ArtifactRelocationMode::Restore => restore_relocation(options),
    }
}

fn plan_or_apply_relocation(
    options: &ArtifactRelocationOptions<'_>,
) -> Result<ArtifactRelocationReport, DomainError> {
    let source = options.source_path.ok_or_else(|| DomainError::Usage {
        message: "--from is required unless --restore is used.".to_owned(),
        repair: Some(
            "ee artifact relocate --from <path> --to <root> --manifest <manifest> --json"
                .to_owned(),
        ),
    })?;
    let destination_root = options.destination_root.ok_or_else(|| DomainError::Usage {
        message: "--to is required unless --restore is used.".to_owned(),
        repair: Some(
            "ee artifact relocate --from <path> --to <root> --manifest <manifest> --apply --json"
                .to_owned(),
        ),
    })?;
    // Normalize lexical parent components before existence and symlink checks.
    // A reviewed path such as `target/../src/main.rs` must resolve to the same
    // canonical source even when the cancelled `target` component does not
    // exist; asking the filesystem about the unnormalized spelling first
    // incorrectly reports NotFound.
    let source = normalize_relocation_manifest_path(&absolutize(source));
    if !source.exists() {
        return Err(DomainError::NotFound {
            resource: "artifact source path".to_owned(),
            id: path_to_string(&source),
            repair: Some("Choose an existing artifact root or file.".to_owned()),
        });
    }
    reject_existing_symlink_component(&source)?;
    let source = canonicalize_existing_relocation_source(&source)?;
    let source_allowed = source_allowed(options.workspace_path, &source);
    if !source_allowed && !options.force_with_explicit_path {
        return Err(DomainError::PolicyDenied {
            message: format!(
                "Refusing to relocate source outside allowlisted artifact roots: {}.",
                source.display()
            ),
            repair: Some(
                "Use target/, tests/audit_artifacts/, tests/logs/, .ee/backups/, or pass --force-with-explicit-path after reviewing the path."
                    .to_owned(),
            ),
        });
    }

    let destination_root = absolutize(destination_root);
    if options.mode == ArtifactRelocationMode::Apply {
        reject_existing_symlink_component(&destination_root)?;
    }
    let destination_root = normalize_relocation_manifest_path(&destination_root);
    let entry_status = if options.mode == ArtifactRelocationMode::Apply {
        "copied"
    } else {
        "planned"
    };
    let entries = collect_entries(
        options.workspace_path,
        &source,
        &destination_root,
        entry_status,
    )?;
    let manifest = ArtifactRelocationManifest {
        schema: ARTIFACT_RELOCATION_SCHEMA_V1.to_owned(),
        command_version: env!("CARGO_PKG_VERSION").to_owned(),
        actor: options.actor.unwrap_or("ee artifact relocate").to_owned(),
        created_at: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
        workspace_path: path_to_string(&absolutize(options.workspace_path)),
        source_path: path_to_string(&source),
        destination_root: path_to_string(&destination_root),
        restoration_command: format!(
            "ee artifact relocate --restore --manifest {} --json",
            options.manifest_path.display()
        ),
        force_with_explicit_path: options.force_with_explicit_path,
        entries,
    };

    if options.mode == ArtifactRelocationMode::Apply {
        reject_existing_symlink_component(options.manifest_path)?;
        prepare_manifest_output_path(options.manifest_path)?;
        apply_manifest_copy(&manifest)?;
        write_manifest_no_overwrite(options.manifest_path, &manifest)?;
    }

    let manifest_hash = if options.mode == ArtifactRelocationMode::Apply {
        Some(hash_file(options.manifest_path)?)
    } else {
        None
    };

    Ok(ArtifactRelocationReport {
        schema: ARTIFACT_RELOCATION_SCHEMA_V1,
        command: "artifact relocate",
        mode: options.mode.as_str(),
        applied: options.mode == ArtifactRelocationMode::Apply,
        restored: false,
        manifest_path: path_to_string(options.manifest_path),
        manifest_hash,
        source_allowed: source_allowed || options.force_with_explicit_path,
        preservation_policy: "copy_preserve_no_delete_no_overwrite",
        recovery_actions: vec![ArtifactRelocationRecoveryAction {
            priority: 1,
            kind: "restore",
            command: manifest.restoration_command.clone(),
            reason: "Use the manifest to copy preserved artifacts back if originals are missing."
                .to_owned(),
        }],
        manifest,
    })
}

fn restore_relocation(
    options: &ArtifactRelocationOptions<'_>,
) -> Result<ArtifactRelocationReport, DomainError> {
    let manifest = read_manifest(options.manifest_path)?;
    let mut restored = false;
    let mut restore_source_allowed = true;
    for entry in &manifest.entries {
        let original = manifest_entry_path(entry, "originalPath", &entry.original_path)?;
        let destination = manifest_entry_path(entry, "destinationPath", &entry.destination_path)?;
        let expected_hash = required_manifest_entry_hash(entry)?;
        reject_existing_symlink_component(&original)?;
        reject_existing_symlink_component(&destination)?;
        let original_allowed = source_allowed(options.workspace_path, &original);
        if !original_allowed && !options.force_with_explicit_path {
            return Err(DomainError::PolicyDenied {
                message: format!(
                    "Refusing to restore artifact outside current workspace artifact roots: {}.",
                    original.display()
                ),
                repair: Some(
                    "Use --force-with-explicit-path only after explicit review of the manifest original paths."
                        .to_owned(),
                ),
            });
        }
        restore_source_allowed &= original_allowed;
        if original.exists() {
            verify_relocation_file_hash(
                &original,
                expected_hash,
                "existing original artifact",
                "Move the conflicting file aside manually before restore.",
            )?;
            continue;
        }
        verify_relocation_file_hash(
            &destination,
            expected_hash,
            "relocated artifact",
            "Verify the relocation manifest and preserved artifact before restore.",
        )?;
        if let Some(parent) = original.parent() {
            reject_existing_symlink_component(parent)?;
            fs::create_dir_all(parent).map_err(|error| DomainError::Storage {
                message: format!(
                    "failed to create restore parent {}: {error}",
                    parent.display()
                ),
                repair: Some("Check destination permissions.".to_owned()),
            })?;
            reject_existing_symlink_component(parent)?;
        }
        reject_existing_symlink_component(&destination)?;
        reject_existing_symlink_component(&original)?;
        copy_relocation_file_no_overwrite(&destination, &original, "restore relocated artifact")?;
        verify_relocation_file_hash(
            &original,
            expected_hash,
            "restored original artifact",
            "Remove the partial restored file manually after reviewing it, then retry.",
        )?;
        restored = true;
    }

    Ok(ArtifactRelocationReport {
        schema: ARTIFACT_RELOCATION_SCHEMA_V1,
        command: "artifact relocate",
        mode: ArtifactRelocationMode::Restore.as_str(),
        applied: false,
        restored,
        manifest_path: path_to_string(options.manifest_path),
        manifest_hash: Some(hash_file(options.manifest_path)?),
        source_allowed: restore_source_allowed || options.force_with_explicit_path,
        preservation_policy: "copy_preserve_no_delete_no_overwrite",
        recovery_actions: Vec::new(),
        manifest,
    })
}

fn required_manifest_entry_hash(entry: &ArtifactRelocationEntry) -> Result<&str, DomainError> {
    entry.blake3.as_deref().ok_or_else(|| DomainError::Usage {
        message: format!(
            "relocation manifest entry for {} is missing required blake3 hash.",
            entry.destination_path
        ),
        repair: Some(
            "Use a relocation manifest created by `ee artifact relocate --apply`.".to_owned(),
        ),
    })
}

fn collect_entries(
    workspace: &Path,
    source: &Path,
    destination_root: &Path,
    status: &str,
) -> Result<Vec<ArtifactRelocationEntry>, DomainError> {
    let mut entries = Vec::new();
    collect_entries_inner(
        workspace,
        source,
        source,
        destination_root,
        status,
        &mut entries,
    )?;
    entries.sort_by(|left, right| left.original_path.cmp(&right.original_path));
    Ok(entries)
}

fn collect_entries_inner(
    workspace: &Path,
    root_source: &Path,
    current: &Path,
    destination_root: &Path,
    status: &str,
    entries: &mut Vec<ArtifactRelocationEntry>,
) -> Result<(), DomainError> {
    reject_symlink(current)?;
    let metadata = fs::symlink_metadata(current).map_err(|error| DomainError::Storage {
        message: format!("failed to inspect {}: {error}", current.display()),
        repair: Some("Check file permissions.".to_owned()),
    })?;
    if metadata.is_dir() {
        for child in fs::read_dir(current).map_err(|error| DomainError::Storage {
            message: format!("failed to read directory {}: {error}", current.display()),
            repair: Some("Check file permissions.".to_owned()),
        })? {
            let child = child.map_err(|error| DomainError::Storage {
                message: format!("failed to inspect directory entry: {error}"),
                repair: Some("Check file permissions.".to_owned()),
            })?;
            collect_entries_inner(
                workspace,
                root_source,
                &child.path(),
                destination_root,
                status,
                entries,
            )?;
        }
        return Ok(());
    }

    if !metadata.is_file() {
        return Ok(());
    }

    let destination = destination_for_source(workspace, root_source, current, destination_root);
    entries.push(ArtifactRelocationEntry {
        original_path: path_to_string(current),
        destination_path: path_to_string(&destination),
        kind: "file".to_owned(),
        size_bytes: metadata.len(),
        mtime_unix_seconds: metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map(|duration| duration.as_secs()),
        blake3: Some(hash_file(current)?),
        status: status.to_owned(),
    });
    Ok(())
}

fn apply_manifest_copy(manifest: &ArtifactRelocationManifest) -> Result<(), DomainError> {
    for entry in &manifest.entries {
        let original = manifest_entry_path(entry, "originalPath", &entry.original_path)?;
        let destination = manifest_entry_path(entry, "destinationPath", &entry.destination_path)?;
        reject_existing_symlink_component(&original)?;
        reject_existing_symlink_component(&destination)?;
        if destination.exists() {
            if let Some(expected) = entry.blake3.as_deref() {
                let actual = hash_file(&destination)?;
                if actual == expected {
                    continue;
                }
            }
            return Err(DomainError::Storage {
                message: format!("destination already exists: {}", destination.display()),
                repair: Some(
                    "Choose an empty destination root; this command will not overwrite.".to_owned(),
                ),
            });
        }
        if let Some(expected) = entry.blake3.as_deref() {
            verify_relocation_file_hash(
                &original,
                expected,
                "source artifact",
                "Regenerate the relocation manifest from the current artifact bytes.",
            )?;
        }
        if let Some(parent) = destination.parent() {
            reject_existing_symlink_component(parent)?;
            fs::create_dir_all(parent).map_err(|error| DomainError::Storage {
                message: format!(
                    "failed to create destination parent {}: {error}",
                    parent.display()
                ),
                repair: Some("Check destination permissions.".to_owned()),
            })?;
            reject_existing_symlink_component(parent)?;
        }
        reject_existing_symlink_component(&original)?;
        reject_existing_symlink_component(&destination)?;
        copy_relocation_file_no_overwrite(&original, &destination, "copy relocated artifact")?;
        if let Some(expected) = entry.blake3.as_deref() {
            verify_relocation_file_hash(
                &destination,
                expected,
                "copied relocated artifact",
                "Inspect the partial destination artifact manually before retrying.",
            )?;
        }
    }
    Ok(())
}

fn verify_relocation_file_hash(
    path: &Path,
    expected: &str,
    role: &str,
    repair: &str,
) -> Result<(), DomainError> {
    let actual = hash_file(path)?;
    if actual == expected {
        return Ok(());
    }
    Err(DomainError::Storage {
        message: format!(
            "{role} hash mismatch at {}: expected {expected}, got {actual}.",
            path.display()
        ),
        repair: Some(repair.to_owned()),
    })
}

fn copy_relocation_file_no_overwrite(
    source: &Path,
    destination: &Path,
    action: &str,
) -> Result<(), DomainError> {
    reject_existing_symlink_component(source)?;
    reject_existing_symlink_component(destination)?;
    match fs::symlink_metadata(source) {
        Ok(metadata) if metadata.file_type().is_file() => {}
        Ok(_) => {
            return Err(DomainError::Storage {
                message: format!(
                    "refusing to {action} from non-regular source: {}",
                    source.display()
                ),
                repair: Some("Relocate only regular artifact files.".to_owned()),
            });
        }
        Err(error) => {
            return Err(DomainError::Storage {
                message: format!(
                    "failed to inspect relocation source {}: {error}",
                    source.display()
                ),
                repair: Some("Verify the relocation manifest and source artifact.".to_owned()),
            });
        }
    }

    let mut source_file = fs::File::open(source).map_err(|error| DomainError::Storage {
        message: format!(
            "failed to open relocation source {}: {error}",
            source.display()
        ),
        repair: Some("Verify the relocation manifest and source artifact.".to_owned()),
    })?;
    let mut destination_file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)
        .map_err(|error| {
            let (message, repair) = if error.kind() == io::ErrorKind::AlreadyExists {
                (
                    format!(
                        "refusing to {action} over existing destination: {}",
                        destination.display()
                    ),
                    "Move the conflicting file aside manually before retrying.".to_owned(),
                )
            } else {
                (
                    format!(
                        "failed to create relocation destination {}: {error}",
                        destination.display()
                    ),
                    "Check destination free space and permissions.".to_owned(),
                )
            };
            DomainError::Storage {
                message,
                repair: Some(repair),
            }
        })?;
    io::copy(&mut source_file, &mut destination_file).map_err(|error| DomainError::Storage {
        message: format!(
            "failed to {action} from {} to {}: {error}",
            source.display(),
            destination.display()
        ),
        repair: Some("Check destination free space and permissions.".to_owned()),
    })?;
    destination_file
        .flush()
        .map_err(|error| DomainError::Storage {
            message: format!(
                "failed to flush relocation destination {}: {error}",
                destination.display()
            ),
            repair: Some("Check destination free space and permissions.".to_owned()),
        })?;
    destination_file
        .sync_all()
        .map_err(|error| DomainError::Storage {
            message: format!(
                "failed to sync relocation destination {}: {error}",
                destination.display()
            ),
            repair: Some("Check destination free space and permissions.".to_owned()),
        })
}

fn write_manifest_no_overwrite(
    manifest_path: &Path,
    manifest: &ArtifactRelocationManifest,
) -> Result<(), DomainError> {
    prepare_manifest_output_path(manifest_path)?;
    let json = serde_json::to_string_pretty(manifest).map_err(|error| DomainError::Storage {
        message: format!("failed to serialize relocation manifest: {error}"),
        repair: Some("Report the serialization failure.".to_owned()),
    })?;

    let temp_path = relocation_manifest_temp_path(manifest_path);

    {
        use std::io::Write;
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)
            .map_err(|error| DomainError::Storage {
                message: format!(
                    "failed to create temporary manifest {}: {error}",
                    temp_path.display()
                ),
                repair: Some("Check manifest path permissions.".to_owned()),
            })?;
        file.write_all(json.as_bytes())
            .map_err(|error| DomainError::Storage {
                message: format!(
                    "failed to write temporary manifest {}: {error}",
                    temp_path.display()
                ),
                repair: Some("Check manifest path permissions.".to_owned()),
            })?;
        file.sync_data().map_err(|error| DomainError::Storage {
            message: format!(
                "failed to sync temporary manifest {}: {error}",
                temp_path.display()
            ),
            repair: Some("Check manifest path permissions.".to_owned()),
        })?;
    }

    publish_relocation_manifest_temp_file(&temp_path, manifest_path)?;

    if let Some(parent) = manifest_path.parent() {
        if let Ok(dir) = fs::File::open(parent) {
            let _ = dir.sync_data();
        }
    }

    Ok(())
}

fn prepare_manifest_output_path(manifest_path: &Path) -> Result<(), DomainError> {
    reject_existing_symlink_component(manifest_path)?;
    ensure_relocation_manifest_final_path_missing(manifest_path)?;
    if let Some(parent) = non_empty_parent(manifest_path) {
        reject_existing_symlink_component(parent)?;
        fs::create_dir_all(parent).map_err(|error| DomainError::Storage {
            message: format!(
                "failed to create manifest parent {}: {error}",
                parent.display()
            ),
            repair: Some("Check manifest directory permissions.".to_owned()),
        })?;
        reject_existing_symlink_component(parent)?;
    }

    let temp_path = relocation_manifest_temp_path(manifest_path);
    reject_existing_symlink_component(&temp_path)?;
    ensure_relocation_manifest_temp_path_regular_or_missing(&temp_path)
}

fn relocation_manifest_temp_path(manifest_path: &Path) -> PathBuf {
    // Append ".tmp" to the FULL file name rather than replacing the extension.
    // set_extension("tmp") yields temp_path == manifest_path when the requested
    // manifest path already ends in ".tmp" (e.g. `--manifest relocation.tmp`):
    // the temp write would then create the final path, and the publish guard
    // (`ensure_relocation_manifest_final_path_missing`) would fail because the
    // final path now exists — leaving the manifest on disk while reporting an
    // error (bd-1whq0). Appending keeps the temp sibling guaranteed distinct
    // from manifest_path for every input.
    let mut file_name = manifest_path
        .file_name()
        .map_or_else(std::ffi::OsString::new, std::ffi::OsString::from);
    file_name.push(".tmp");
    manifest_path.with_file_name(file_name)
}

fn non_empty_parent(path: &Path) -> Option<&Path> {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
}

fn publish_relocation_manifest_temp_file(
    temp_path: &Path,
    manifest_path: &Path,
) -> Result<(), DomainError> {
    reject_existing_symlink_component(manifest_path)?;
    ensure_relocation_manifest_final_path_missing(manifest_path)?;
    reject_existing_symlink_component(temp_path)?;
    ensure_relocation_manifest_temp_path_is_regular(temp_path)?;
    fs::rename(temp_path, manifest_path).map_err(|error| DomainError::Storage {
        message: format!(
            "failed to rename temporary manifest to {}: {error}",
            manifest_path.display()
        ),
        repair: Some("Check manifest path permissions.".to_owned()),
    })
}

fn ensure_relocation_manifest_final_path_missing(manifest_path: &Path) -> Result<(), DomainError> {
    match fs::symlink_metadata(manifest_path) {
        Ok(metadata) if metadata.file_type().is_file() => Err(DomainError::Storage {
            message: format!(
                "relocation manifest path already exists before publish: {}",
                manifest_path.display()
            ),
            repair: Some("Choose a new manifest path; this command will not overwrite.".to_owned()),
        }),
        Ok(_) => Err(DomainError::Storage {
            message: format!(
                "relocation manifest path is not a regular file: {}",
                manifest_path.display()
            ),
            repair: Some("Choose a regular relocation manifest path.".to_owned()),
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(DomainError::Storage {
            message: format!(
                "failed to inspect relocation manifest {} before publish: {error}",
                manifest_path.display()
            ),
            repair: Some("Check manifest path permissions.".to_owned()),
        }),
    }
}

fn ensure_relocation_manifest_temp_path_is_regular(temp_path: &Path) -> Result<(), DomainError> {
    match fs::symlink_metadata(temp_path) {
        Ok(metadata) if metadata.file_type().is_file() => Ok(()),
        Ok(_) => Err(DomainError::Storage {
            message: format!(
                "temporary relocation manifest path is not a regular file: {}",
                temp_path.display()
            ),
            repair: Some("Remove the non-regular temporary manifest path and retry.".to_owned()),
        }),
        Err(error) => Err(DomainError::Storage {
            message: format!(
                "failed to inspect temporary relocation manifest {} before publish: {error}",
                temp_path.display()
            ),
            repair: Some("Check manifest path permissions.".to_owned()),
        }),
    }
}

fn ensure_relocation_manifest_temp_path_regular_or_missing(
    temp_path: &Path,
) -> Result<(), DomainError> {
    match fs::symlink_metadata(temp_path) {
        Ok(metadata) if metadata.file_type().is_file() => Err(DomainError::Storage {
            message: format!(
                "temporary relocation manifest path already exists: {}",
                temp_path.display()
            ),
            repair: Some("Remove the stale temporary manifest path and retry.".to_owned()),
        }),
        Ok(_) => Err(DomainError::Storage {
            message: format!(
                "temporary relocation manifest path is not a regular file: {}",
                temp_path.display()
            ),
            repair: Some("Remove the non-regular temporary manifest path and retry.".to_owned()),
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(DomainError::Storage {
            message: format!(
                "failed to inspect temporary relocation manifest {} before create: {error}",
                temp_path.display()
            ),
            repair: Some("Check manifest path permissions.".to_owned()),
        }),
    }
}

fn read_manifest(path: &Path) -> Result<ArtifactRelocationManifest, DomainError> {
    reject_existing_symlink_component(path)?;
    let metadata = fs::symlink_metadata(path).map_err(|error| DomainError::Storage {
        message: format!("failed to inspect manifest {}: {error}", path.display()),
        repair: Some(
            "Pass a relocation manifest created by `ee artifact relocate --apply`.".to_owned(),
        ),
    })?;
    if !metadata.file_type().is_file() {
        return Err(DomainError::Storage {
            message: format!(
                "refusing to read relocation manifest {} because it is not a regular file",
                path.display()
            ),
            repair: Some("Pass a regular ee.artifact.relocation.v1 manifest file.".to_owned()),
        });
    }
    let text =
        read_relocation_manifest_file_no_follow(path).map_err(|error| DomainError::Storage {
            message: format!("failed to read manifest {}: {error}", path.display()),
            repair: Some(
                "Pass a relocation manifest created by `ee artifact relocate --apply`.".to_owned(),
            ),
        })?;
    let manifest: ArtifactRelocationManifest =
        serde_json::from_str(&text).map_err(|error| DomainError::Usage {
            message: format!(
                "failed to parse relocation manifest {}: {error}",
                path.display()
            ),
            repair: Some("Pass a valid ee.artifact.relocation.v1 manifest.".to_owned()),
        })?;
    if manifest.schema != ARTIFACT_RELOCATION_SCHEMA_V1 {
        return Err(DomainError::Usage {
            message: format!(
                "unsupported relocation manifest schema `{}`.",
                manifest.schema
            ),
            repair: Some(format!("Expected `{ARTIFACT_RELOCATION_SCHEMA_V1}`.")),
        });
    }
    Ok(manifest)
}

fn read_relocation_manifest_file_no_follow(path: &Path) -> io::Result<String> {
    // Bounded read: cap at `ARTIFACT_RELOCATION_MANIFEST_MAX_BYTES + 1`
    // so the post-read size check distinguishes "exactly at cap"
    // (accepted) from "above cap" (rejected) without a separate stat
    // call on the read path. The metadata-only `file_type().is_file()`
    // check at the caller (`read_manifest` line 681) does NOT bound
    // size, so without this cap an unbounded `read_to_string` would
    // pre-size from the on-disk length and OOM the CLI on a multi-GiB
    // manifest. Same defensive pattern as the round-2 caps at
    // `src/core/preflight_guard.rs::read_preflight_rules_file_no_follow`
    // (7f56d89b), `src/core/memory.rs::read_workspace_config_if_present`
    // (e1499deb), `src/core/curate.rs::structural_decay_config_contents`
    // (0fe4a339), `src/cache/pack_l2.rs::read_cache_entry_file` (8ba93c0e),
    // `src/core/handoff.rs::ensure_handoff_key_material_within_cap`
    // (f067c32c), `src/core/claims.rs` (52276a68), and `src/core/qos.rs`
    // (e0b11daa).
    let file = open_relocation_manifest_file_for_read(path)?;
    let mut bytes = Vec::new();
    file.take(ARTIFACT_RELOCATION_MANIFEST_MAX_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > ARTIFACT_RELOCATION_MANIFEST_MAX_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "refusing to read relocation manifest `{}`: exceeded the {ARTIFACT_RELOCATION_MANIFEST_MAX_BYTES}-byte cap after the metadata check (TOCTOU)",
                path.display(),
            ),
        ));
    }
    String::from_utf8(bytes).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

fn open_relocation_manifest_file_for_read(path: &Path) -> io::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    configure_relocation_manifest_open_no_follow(&mut options);
    options.open(path)
}

#[cfg(all(unix, not(any(target_os = "espidf", target_os = "horizon"))))]
fn configure_relocation_manifest_open_no_follow(options: &mut fs::OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt;

    options.custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32);
}

#[cfg(not(all(unix, not(any(target_os = "espidf", target_os = "horizon")))))]
fn configure_relocation_manifest_open_no_follow(_options: &mut fs::OpenOptions) {}

fn manifest_entry_path(
    entry: &ArtifactRelocationEntry,
    field: &str,
    raw_path: &str,
) -> Result<PathBuf, DomainError> {
    let path = PathBuf::from(raw_path);
    if !path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir | std::path::Component::CurDir
            )
        })
    {
        return Err(DomainError::PolicyDenied {
            message: format!(
                "Refusing relocation manifest entry with unsafe {field}: {raw_path}."
            ),
            repair: Some(
                "Use a relocation manifest created by this ee version with absolute normalized paths."
                    .to_owned(),
            ),
        });
    }
    reject_existing_symlink_component(&path)?;
    if entry.kind != "file" {
        return Err(DomainError::Usage {
            message: format!(
                "unsupported relocation manifest entry kind `{}`.",
                entry.kind
            ),
            repair: Some("Only file relocation entries are supported.".to_owned()),
        });
    }
    Ok(path)
}

fn source_allowed(workspace: &Path, source: &Path) -> bool {
    let workspace = fs::canonicalize(workspace).unwrap_or_else(|_| absolutize(workspace));
    let source = fs::canonicalize(source).unwrap_or_else(|_| absolutize(source));
    [
        workspace.join("target"),
        workspace.join("tests/audit_artifacts"),
        workspace.join("tests/logs"),
        workspace.join(".ee/backups"),
        workspace.join(".ee/index"),
        workspace.join(".ee/cache"),
    ]
    .iter()
    .any(|root| source.starts_with(root))
}

fn destination_for_source(
    workspace: &Path,
    root_source: &Path,
    current: &Path,
    destination_root: &Path,
) -> PathBuf {
    let workspace = normalize_relocation_manifest_path(&absolutize(workspace));
    let root_source = normalize_relocation_manifest_path(root_source);
    let current = normalize_relocation_manifest_path(current);
    let destination_root = normalize_relocation_manifest_path(destination_root);
    let relative = current.strip_prefix(&workspace).ok().or_else(|| {
        current
            .strip_prefix(&root_source)
            .ok()
            .filter(|path| !path.as_os_str().is_empty())
    });
    let relative = relative.map(Path::to_path_buf).unwrap_or_else(|| {
        current
            .file_name()
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("artifact"))
    });
    destination_root.join(RELOCATION_DIR).join(relative)
}

fn reject_symlink(path: &Path) -> Result<(), DomainError> {
    let metadata = fs::symlink_metadata(path).map_err(|error| DomainError::Storage {
        message: format!("failed to inspect {}: {error}", path.display()),
        repair: Some("Check file permissions.".to_owned()),
    })?;
    if metadata.file_type().is_symlink() {
        return Err(DomainError::PolicyDenied {
            message: format!("Refusing to relocate symlink path: {}.", path.display()),
            repair: Some("Relocate the resolved artifact path explicitly.".to_owned()),
        });
    }
    Ok(())
}

fn reject_existing_symlink_component(path: &Path) -> Result<(), DomainError> {
    match super::path_safety::first_existing_symlink_component(path) {
        Ok(Some(symlink_path)) => Err(DomainError::PolicyDenied {
            message: format!(
                "Refusing artifact relocation path with symlink component: {}.",
                symlink_path.display()
            ),
            repair: Some(
                "Use a path whose existing parent components are regular directories.".to_owned(),
            ),
        }),
        Ok(None) => Ok(()),
        Err(error) => Err(DomainError::Storage {
            message: format!("failed to inspect {}: {error}", path.display()),
            repair: Some("Check file permissions.".to_owned()),
        }),
    }
}

fn hash_file(path: &Path) -> Result<String, DomainError> {
    reject_existing_symlink_component(path)?;
    ensure_hash_source_is_regular_file(path)?;
    let mut file = fs::File::open(path).map_err(|error| DomainError::Storage {
        message: format!("failed to open {} for hashing: {error}", path.display()),
        repair: Some("Check file permissions.".to_owned()),
    })?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| DomainError::Storage {
                message: format!("failed to read {} for hashing: {error}", path.display()),
                repair: Some("Check file permissions.".to_owned()),
            })?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("blake3:{}", hasher.finalize().to_hex()))
}

fn ensure_hash_source_is_regular_file(path: &Path) -> Result<(), DomainError> {
    let metadata = fs::symlink_metadata(path).map_err(|error| DomainError::Storage {
        message: format!(
            "failed to inspect {} before hashing: {error}",
            path.display()
        ),
        repair: Some("Check file permissions.".to_owned()),
    })?;
    if metadata.file_type().is_file() {
        return Ok(());
    }
    Err(DomainError::Storage {
        message: format!(
            "refusing to hash non-regular artifact path: {}",
            path.display()
        ),
        repair: Some("Relocate only regular artifact files.".to_owned()),
    })
}

fn absolutize(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    }
}

fn canonicalize_existing_relocation_source(path: &Path) -> Result<PathBuf, DomainError> {
    fs::canonicalize(path).map_err(|error| DomainError::Storage {
        message: format!(
            "failed to resolve artifact source {}: {error}",
            path.display()
        ),
        repair: Some(
            "Choose an existing artifact path with inspectable parent directories.".to_owned(),
        ),
    })
}

fn normalize_relocation_manifest_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() {
                    normalized.push(component.as_os_str());
                }
            }
            Component::Normal(part) => normalized.push(part),
        }
    }
    if normalized.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        normalized
    }
}

fn path_to_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn redact_artifact_relocation_public_path(value: &str) -> String {
    let secret_redacted = crate::policy::redact_secret_like_content(value).content;
    redact_artifact_relocation_path_segments(&secret_redacted)
}

fn redact_artifact_relocation_path_segments(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut cursor = 0;
    while cursor < value.len() {
        let Some((relative_index, _)) = value[cursor..].char_indices().find(|(_, c)| *c == '/')
        else {
            output.push_str(&value[cursor..]);
            break;
        };
        let start = cursor + relative_index;
        if !artifact_relocation_path_starts_sensitive_segment(&value[start..]) {
            output.push_str(&value[cursor..=start]);
            cursor = start + 1;
            continue;
        }

        output.push_str(&value[cursor..start]);
        output.push_str("[REDACTED_PATH]");
        cursor = value[start..]
            .char_indices()
            .find_map(|(index, c)| artifact_relocation_path_boundary(c).then_some(start + index))
            .unwrap_or(value.len());
    }
    output
}

fn artifact_relocation_path_starts_sensitive_segment(value: &str) -> bool {
    const PREFIXES: &[&str] = &[
        "/Users/",
        "/Volumes/",
        "/private/",
        "/var/",
        "/tmp/",
        "/home/",
        "/data/",
        "/dp/",
        "/workspace/",
        "/repo/",
        "/etc/",
    ];

    PREFIXES.iter().any(|prefix| value.starts_with(prefix))
}

fn artifact_relocation_path_boundary(c: char) -> bool {
    c.is_whitespace() || matches!(c, '?' | '#' | '"' | '\'' | ')' | ']' | '}' | ',' | ';')
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::ensure;
    use std::fs;

    type TestResult = Result<(), String>;

    fn temp_path(label: &str) -> PathBuf {
        let root = std::env::var_os("CARGO_TARGET_TMPDIR")
            .or_else(|| std::env::var_os("CARGO_TARGET_DIR"))
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                std::env::current_dir()
                    .unwrap_or_else(|_| PathBuf::from("."))
                    .join("target")
            });
        let root = fs::canonicalize(&root).unwrap_or(root);
        let unique = std::time::SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default();
        root.join("ee-artifact-relocation-tests")
            .join(format!("{label}-{}-{unique}", std::process::id()))
    }

    fn parent_dir(path: &Path) -> Result<&Path, String> {
        path.parent()
            .ok_or_else(|| format!("path has no parent: {}", path.display()))
    }

    fn relocation_manifest_for(
        workspace: &Path,
        original: &Path,
        destination: &Path,
        manifest_path: &Path,
    ) -> Result<ArtifactRelocationManifest, String> {
        Ok(ArtifactRelocationManifest {
            schema: ARTIFACT_RELOCATION_SCHEMA_V1.to_owned(),
            command_version: env!("CARGO_PKG_VERSION").to_owned(),
            actor: "test".to_owned(),
            created_at: "2026-05-13T00:00:00Z".to_owned(),
            workspace_path: path_to_string(workspace),
            source_path: path_to_string(original),
            destination_root: path_to_string(parent_dir(destination)?),
            restoration_command: format!(
                "ee artifact relocate --restore --manifest {} --json",
                manifest_path.display()
            ),
            force_with_explicit_path: false,
            entries: vec![ArtifactRelocationEntry {
                original_path: path_to_string(original),
                destination_path: path_to_string(destination),
                kind: "file".to_owned(),
                size_bytes: fs::metadata(destination)
                    .map_err(|error| error.to_string())?
                    .len(),
                mtime_unix_seconds: None,
                blake3: Some(hash_file(destination).map_err(|error| error.to_string())?),
                status: "copied".to_owned(),
            }],
        })
    }

    fn write_relocation_manifest(
        manifest_path: &Path,
        manifest: &ArtifactRelocationManifest,
    ) -> TestResult {
        fs::create_dir_all(parent_dir(manifest_path)?).map_err(|error| error.to_string())?;
        let json = serde_json::to_string_pretty(manifest).map_err(|error| error.to_string())?;
        fs::write(manifest_path, json).map_err(|error| error.to_string())
    }

    fn has_dot_path_component(raw_path: &str) -> bool {
        Path::new(raw_path)
            .components()
            .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
    }

    #[test]
    fn relocation_report_json_redacts_sensitive_manifest_paths() -> TestResult {
        let report = ArtifactRelocationReport {
            schema: ARTIFACT_RELOCATION_SCHEMA_V1,
            command: "artifact relocate",
            mode: ArtifactRelocationMode::Plan.as_str(),
            applied: false,
            restored: false,
            manifest_path: concat!(
                "/Users/jemanuel/private/relocation.json?",
                "api",
                "_key=sk-test-12345678901234567890"
            )
            .to_owned(),
            manifest_hash: None,
            source_allowed: true,
            preservation_policy: "copy_preserve_no_delete_no_overwrite",
            manifest: ArtifactRelocationManifest {
                schema: ARTIFACT_RELOCATION_SCHEMA_V1.to_owned(),
                command_version: env!("CARGO_PKG_VERSION").to_owned(),
                actor: "test".to_owned(),
                created_at: "2026-05-13T00:00:00Z".to_owned(),
                workspace_path: "/Users/jemanuel/projects/eidetic_engine_cli".to_owned(),
                source_path: "/Users/jemanuel/projects/eidetic_engine_cli/target/debug/app.o"
                    .to_owned(),
                destination_root: "/Volumes/USBNVME16TB/temp_agent_space/artifacts".to_owned(),
                restoration_command: concat!(
                    "ee artifact relocate --restore --manifest ",
                    "/Users/jemanuel/private/relocation.json?",
                    "api",
                    "_key=sk-test-12345678901234567890 --json"
                )
                .to_owned(),
                force_with_explicit_path: false,
                entries: vec![ArtifactRelocationEntry {
                    original_path: "/Users/jemanuel/projects/eidetic_engine_cli/target/debug/app.o"
                        .to_owned(),
                    destination_path: "/Volumes/USBNVME16TB/temp_agent_space/artifacts/app.o"
                        .to_owned(),
                    kind: "file".to_owned(),
                    size_bytes: 10,
                    mtime_unix_seconds: None,
                    blake3: Some("blake3:test".to_owned()),
                    status: "planned".to_owned(),
                }],
            },
            recovery_actions: vec![ArtifactRelocationRecoveryAction {
                priority: 1,
                kind: "restore",
                command: "ee artifact relocate --restore --manifest /Users/jemanuel/private/relocation.json --json".to_owned(),
                reason: "restore".to_owned(),
            }],
        };

        let rendered = report.data_json().to_string();

        ensure(
            rendered.contains("[REDACTED_PATH]"),
            format!("report JSON should redact path-like fields: {rendered}"),
        )?;
        ensure(
            rendered.contains("[REDACTED:"),
            format!("report JSON should redact secret-like fields: {rendered}"),
        )?;
        ensure(
            !rendered.contains("/Users/jemanuel"),
            format!("report JSON leaked user path: {rendered}"),
        )?;
        ensure(
            !rendered.contains("/Volumes/USBNVME16TB"),
            format!("report JSON leaked volume path: {rendered}"),
        )?;
        ensure(
            !rendered.contains("12345678901234567890"),
            format!("report JSON leaked secret material: {rendered}"),
        )?;
        ensure(
            report.manifest.source_path.contains("/Users/jemanuel"),
            "raw report manifest source_path should stay intact internally",
        )
    }

    #[test]
    fn relocation_report_human_summary_redacts_manifest_path() -> TestResult {
        let manifest_path = concat!(
            "/Users/jemanuel/private/relocation.json?",
            "api",
            "_key=sk-test-abcdefghijklmnop"
        )
        .to_owned();
        let report = ArtifactRelocationReport {
            schema: ARTIFACT_RELOCATION_SCHEMA_V1,
            command: "artifact relocate",
            mode: ArtifactRelocationMode::Plan.as_str(),
            applied: false,
            restored: false,
            manifest_path,
            manifest_hash: None,
            source_allowed: true,
            preservation_policy: "copy_preserve_no_delete_no_overwrite",
            manifest: ArtifactRelocationManifest {
                schema: ARTIFACT_RELOCATION_SCHEMA_V1.to_owned(),
                command_version: env!("CARGO_PKG_VERSION").to_owned(),
                actor: "test".to_owned(),
                created_at: "2026-05-13T00:00:00Z".to_owned(),
                workspace_path: "agent://safe-workspace".to_owned(),
                source_path: "agent://safe-source".to_owned(),
                destination_root: "agent://safe-destination".to_owned(),
                restoration_command:
                    "ee artifact relocate --restore --manifest agent://safe --json".to_owned(),
                force_with_explicit_path: false,
                entries: vec![],
            },
            recovery_actions: vec![],
        };

        let rendered = report.human_summary();

        ensure(
            rendered.contains("[REDACTED_PATH]"),
            format!("human summary should redact path-like manifest path: {rendered}"),
        )?;
        ensure(
            rendered.contains("[REDACTED:"),
            format!("human summary should redact secret-like manifest path: {rendered}"),
        )?;
        ensure(
            !rendered.contains("/Users/jemanuel"),
            format!("human summary leaked manifest path: {rendered}"),
        )?;
        ensure(
            !rendered.contains("abcdefghijklmnop"),
            format!("human summary leaked secret material: {rendered}"),
        )
    }

    #[test]
    fn relocation_plan_refuses_non_artifact_source_without_force() -> TestResult {
        let workspace = temp_path("refuse-workspace");
        let source = workspace.join("src/main.rs");
        fs::create_dir_all(parent_dir(&source)?).map_err(|error| error.to_string())?;
        fs::write(&source, "fn main() {}\n").map_err(|error| error.to_string())?;
        let manifest = workspace.join("manifest.json");
        let destination = temp_path("refuse-destination");
        let result = relocate_artifacts(&ArtifactRelocationOptions {
            workspace_path: &workspace,
            source_path: Some(&source),
            destination_root: Some(&destination),
            manifest_path: &manifest,
            actor: Some("test"),
            mode: ArtifactRelocationMode::Plan,
            force_with_explicit_path: false,
        });
        if matches!(result, Err(DomainError::PolicyDenied { .. })) {
            Ok(())
        } else {
            Err(format!("expected policy denial, got {result:?}"))
        }
    }

    #[test]
    fn relocation_apply_copies_and_writes_manifest_without_removing_original() -> TestResult {
        let workspace = temp_path("apply-workspace");
        let source = workspace.join("target/debug/sample.o");
        fs::create_dir_all(parent_dir(&source)?).map_err(|error| error.to_string())?;
        fs::write(&source, "artifact bytes\n").map_err(|error| error.to_string())?;
        let destination = temp_path("apply-destination");
        let manifest = temp_path("apply-manifest").join("relocation.json");

        let report = relocate_artifacts(&ArtifactRelocationOptions {
            workspace_path: &workspace,
            source_path: Some(&source),
            destination_root: Some(&destination),
            manifest_path: &manifest,
            actor: Some("test"),
            mode: ArtifactRelocationMode::Apply,
            force_with_explicit_path: false,
        })
        .map_err(|error| error.to_string())?;

        if !source.exists() {
            return Err("original source was removed".to_owned());
        }
        if !Path::new(&report.manifest.entries[0].destination_path).exists() {
            return Err("destination copy missing".to_owned());
        }
        if !manifest.exists() {
            return Err("manifest missing".to_owned());
        }
        if report.manifest.entries[0].blake3.is_none() {
            return Err("manifest entry missing blake3".to_owned());
        }
        if report.manifest.entries[0].status != "copied" {
            return Err(format!(
                "applied relocation entry should be copied, got {}",
                report.manifest.entries[0].status
            ));
        }
        Ok(())
    }

    #[test]
    fn relocation_plan_accepts_canonical_absolute_artifact_source() -> TestResult {
        let workspace = temp_path("canonical-plan-workspace");
        let source = workspace.join("target/debug/canonical.o");
        fs::create_dir_all(parent_dir(&source)?).map_err(|error| error.to_string())?;
        fs::write(&source, "artifact bytes\n").map_err(|error| error.to_string())?;
        let canonical_workspace =
            fs::canonicalize(&workspace).map_err(|error| error.to_string())?;
        let canonical_source = fs::canonicalize(&source).map_err(|error| error.to_string())?;
        let destination = temp_path("canonical-plan-destination");
        let manifest = temp_path("canonical-plan-manifest").join("relocation.json");

        let report = relocate_artifacts(&ArtifactRelocationOptions {
            workspace_path: &canonical_workspace,
            source_path: Some(&canonical_source),
            destination_root: Some(&destination),
            manifest_path: &manifest,
            actor: Some("test"),
            mode: ArtifactRelocationMode::Plan,
            force_with_explicit_path: false,
        })
        .map_err(|error| error.to_string())?;

        ensure(
            !report.applied,
            "plan mode should not apply artifact relocation",
        )?;
        ensure(
            report.manifest.entries.len() == 1,
            "canonical source should produce one relocation entry",
        )
    }

    #[test]
    fn relocation_plan_normalizes_force_reviewed_parent_component_source() -> TestResult {
        let workspace = temp_path("force-parent-source-workspace");
        let source = workspace.join("src/main.rs");
        fs::create_dir_all(parent_dir(&source)?).map_err(|error| error.to_string())?;
        fs::write(&source, "fn main() {}\n").map_err(|error| error.to_string())?;
        let reviewed_source = workspace.join("target/../src/main.rs");
        let destination = temp_path("force-parent-source-destination");
        let manifest = temp_path("force-parent-source-manifest").join("relocation.json");

        let report = relocate_artifacts(&ArtifactRelocationOptions {
            workspace_path: &workspace,
            source_path: Some(&reviewed_source),
            destination_root: Some(&destination),
            manifest_path: &manifest,
            actor: Some("test"),
            mode: ArtifactRelocationMode::Plan,
            force_with_explicit_path: true,
        })
        .map_err(|error| error.to_string())?;

        let entry = report
            .manifest
            .entries
            .first()
            .ok_or_else(|| "expected one relocation entry".to_owned())?;
        ensure(
            !has_dot_path_component(&entry.original_path),
            format!(
                "planned original path should be normalized for apply: {}",
                entry.original_path
            ),
        )?;
        ensure(
            !has_dot_path_component(&entry.destination_path),
            format!(
                "planned destination path should be normalized for apply: {}",
                entry.destination_path
            ),
        )?;
        ensure(
            entry
                .destination_path
                .ends_with("ee-relocated-artifacts/src/main.rs"),
            format!(
                "unexpected normalized destination: {}",
                entry.destination_path
            ),
        )
    }

    #[test]
    fn relocation_apply_normalizes_parent_component_destination_root() -> TestResult {
        let workspace = temp_path("apply-parent-destination-workspace");
        let source = workspace.join("target/debug/sample.o");
        fs::create_dir_all(parent_dir(&source)?).map_err(|error| error.to_string())?;
        fs::write(&source, "artifact bytes\n").map_err(|error| error.to_string())?;
        let destination_base = temp_path("apply-parent-destination");
        let destination = destination_base.join("scratch/../archive");
        let manifest = temp_path("apply-parent-destination-manifest").join("relocation.json");

        let report = relocate_artifacts(&ArtifactRelocationOptions {
            workspace_path: &workspace,
            source_path: Some(&source),
            destination_root: Some(&destination),
            manifest_path: &manifest,
            actor: Some("test"),
            mode: ArtifactRelocationMode::Apply,
            force_with_explicit_path: false,
        })
        .map_err(|error| error.to_string())?;

        let entry = report
            .manifest
            .entries
            .first()
            .ok_or_else(|| "expected one relocation entry".to_owned())?;
        ensure(
            !has_dot_path_component(&entry.destination_path),
            format!(
                "applied destination path should be normalized for restore: {}",
                entry.destination_path
            ),
        )?;
        let expected_copy = destination_base
            .join("archive")
            .join(RELOCATION_DIR)
            .join("target/debug/sample.o");
        ensure(
            expected_copy.exists(),
            format!(
                "expected normalized destination copy at {}",
                expected_copy.display()
            ),
        )
    }

    #[cfg(unix)]
    #[test]
    fn relocation_apply_rejects_existing_symlinked_destination_file_before_hash() -> TestResult {
        let workspace = temp_path("symlink-final-destination-workspace");
        let source = workspace.join("target/debug/sample.o");
        fs::create_dir_all(parent_dir(&source)?).map_err(|error| error.to_string())?;
        fs::write(&source, "artifact bytes\n").map_err(|error| error.to_string())?;
        let destination = temp_path("symlink-final-destination");
        let expected_copy = destination
            .join(RELOCATION_DIR)
            .join("target/debug/sample.o");
        fs::create_dir_all(parent_dir(&expected_copy)?).map_err(|error| error.to_string())?;
        let outside_target = temp_path("symlink-final-destination-target").join("sample.o");
        fs::create_dir_all(parent_dir(&outside_target)?).map_err(|error| error.to_string())?;
        fs::write(&outside_target, "artifact bytes\n").map_err(|error| error.to_string())?;
        std::os::unix::fs::symlink(&outside_target, &expected_copy)
            .map_err(|error| error.to_string())?;
        let manifest = temp_path("symlink-final-destination-manifest").join("relocation.json");

        let result = relocate_artifacts(&ArtifactRelocationOptions {
            workspace_path: &workspace,
            source_path: Some(&source),
            destination_root: Some(&destination),
            manifest_path: &manifest,
            actor: Some("test"),
            mode: ArtifactRelocationMode::Apply,
            force_with_explicit_path: false,
        });

        if !matches!(result, Err(DomainError::PolicyDenied { .. })) {
            return Err(format!("expected policy denial, got {result:?}"));
        }
        let outside = fs::read_to_string(&outside_target).map_err(|error| error.to_string())?;
        if outside != "artifact bytes\n" {
            return Err("apply mutated symlink target before rejecting".to_owned());
        }
        if manifest.exists() {
            return Err("manifest was written after symlinked destination rejection".to_owned());
        }
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn relocation_apply_rejects_symlinked_destination_root() -> TestResult {
        let workspace = temp_path("symlink-destination-workspace");
        let source = workspace.join("target/debug/sample.o");
        fs::create_dir_all(parent_dir(&source)?).map_err(|error| error.to_string())?;
        fs::write(&source, "artifact bytes\n").map_err(|error| error.to_string())?;
        let real_destination = temp_path("symlink-destination-real");
        fs::create_dir_all(&real_destination).map_err(|error| error.to_string())?;
        let destination_link = temp_path("symlink-destination-link");
        std::os::unix::fs::symlink(&real_destination, &destination_link)
            .map_err(|error| error.to_string())?;
        let manifest = temp_path("symlink-destination-manifest").join("relocation.json");

        let result = relocate_artifacts(&ArtifactRelocationOptions {
            workspace_path: &workspace,
            source_path: Some(&source),
            destination_root: Some(&destination_link),
            manifest_path: &manifest,
            actor: Some("test"),
            mode: ArtifactRelocationMode::Apply,
            force_with_explicit_path: false,
        });

        if matches!(result, Err(DomainError::PolicyDenied { .. })) {
            Ok(())
        } else {
            Err(format!("expected policy denial, got {result:?}"))
        }
    }

    #[cfg(unix)]
    #[test]
    fn relocation_apply_rejects_symlinked_source_parent_even_with_force() -> TestResult {
        let workspace = temp_path("symlink-source-workspace");
        let real_source_parent = temp_path("symlink-source-real");
        fs::create_dir_all(&real_source_parent).map_err(|error| error.to_string())?;
        let source_parent = workspace.join("target/debug");
        fs::create_dir_all(parent_dir(&source_parent)?).map_err(|error| error.to_string())?;
        std::os::unix::fs::symlink(&real_source_parent, &source_parent)
            .map_err(|error| error.to_string())?;
        let source = source_parent.join("sample.o");
        fs::write(&source, "artifact bytes\n").map_err(|error| error.to_string())?;
        let destination = temp_path("symlink-source-destination");
        let manifest = temp_path("symlink-source-manifest").join("relocation.json");
        let expected_copy = destination
            .join(RELOCATION_DIR)
            .join("target/debug/sample.o");

        let result = relocate_artifacts(&ArtifactRelocationOptions {
            workspace_path: &workspace,
            source_path: Some(&source),
            destination_root: Some(&destination),
            manifest_path: &manifest,
            actor: Some("test"),
            mode: ArtifactRelocationMode::Apply,
            force_with_explicit_path: true,
        });

        if !matches!(result, Err(DomainError::PolicyDenied { .. })) {
            return Err(format!("expected policy denial, got {result:?}"));
        }
        if expected_copy.exists() {
            return Err("copy happened through symlinked source parent".to_owned());
        }
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn relocation_apply_rejects_symlinked_manifest_parent_before_copy() -> TestResult {
        let workspace = temp_path("symlink-manifest-workspace");
        let source = workspace.join("target/debug/sample.o");
        fs::create_dir_all(parent_dir(&source)?).map_err(|error| error.to_string())?;
        fs::write(&source, "artifact bytes\n").map_err(|error| error.to_string())?;
        let destination = temp_path("symlink-manifest-destination");
        let real_manifest_parent = temp_path("symlink-manifest-real");
        fs::create_dir_all(&real_manifest_parent).map_err(|error| error.to_string())?;
        let manifest_parent_link = temp_path("symlink-manifest-link");
        std::os::unix::fs::symlink(&real_manifest_parent, &manifest_parent_link)
            .map_err(|error| error.to_string())?;
        let manifest = manifest_parent_link.join("relocation.json");
        let expected_copy = destination
            .join(RELOCATION_DIR)
            .join("target/debug/sample.o");

        let result = relocate_artifacts(&ArtifactRelocationOptions {
            workspace_path: &workspace,
            source_path: Some(&source),
            destination_root: Some(&destination),
            manifest_path: &manifest,
            actor: Some("test"),
            mode: ArtifactRelocationMode::Apply,
            force_with_explicit_path: false,
        });

        if !matches!(result, Err(DomainError::PolicyDenied { .. })) {
            return Err(format!("expected policy denial, got {result:?}"));
        }
        if expected_copy.exists() {
            return Err("copy happened before symlinked manifest parent was rejected".to_owned());
        }
        Ok(())
    }

    #[test]
    fn relocation_apply_rejects_existing_temp_manifest_without_truncating() -> TestResult {
        let workspace = temp_path("existing-temp-manifest-workspace");
        let source = workspace.join("target/debug/sample.o");
        fs::create_dir_all(parent_dir(&source)?).map_err(|error| error.to_string())?;
        fs::write(&source, "artifact bytes\n").map_err(|error| error.to_string())?;
        let destination = temp_path("existing-temp-manifest-destination");
        let expected_copy = destination
            .join(RELOCATION_DIR)
            .join("target/debug/sample.o");
        let manifest = temp_path("existing-temp-manifest").join("relocation.json");
        let parent = parent_dir(&manifest)?;
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        let temp_manifest = relocation_manifest_temp_path(&manifest);
        fs::write(&temp_manifest, "keep me").map_err(|error| error.to_string())?;

        let result = relocate_artifacts(&ArtifactRelocationOptions {
            workspace_path: &workspace,
            source_path: Some(&source),
            destination_root: Some(&destination),
            manifest_path: &manifest,
            actor: Some("test"),
            mode: ArtifactRelocationMode::Apply,
            force_with_explicit_path: false,
        });

        match result {
            Err(DomainError::Storage { message, repair }) => {
                if !message.contains("temporary relocation manifest path")
                    || !message.contains("already exists")
                {
                    return Err(format!("unexpected storage error message: {message}"));
                }
                if repair.as_deref() != Some("Remove the stale temporary manifest path and retry.")
                {
                    return Err(format!("unexpected repair hint: {repair:?}"));
                }
            }
            other => {
                return Err(format!(
                    "expected temp manifest storage error, got {other:?}"
                ));
            }
        }
        if manifest.exists() {
            return Err("final manifest was written after temp path collision".to_owned());
        }
        if expected_copy.exists() {
            return Err("artifact copy happened before temp manifest rejection".to_owned());
        }
        let temp_content = fs::read_to_string(&temp_manifest).map_err(|error| error.to_string())?;
        if temp_content != "keep me" {
            return Err("existing temp manifest was unexpectedly truncated".to_owned());
        }
        Ok(())
    }

    #[test]
    fn relocation_apply_succeeds_when_manifest_path_ends_in_tmp() -> TestResult {
        // bd-1whq0: a manifest path ending in ".tmp" must not collide with the
        // temporary publish path. Previously `relocation_manifest_temp_path`
        // used set_extension("tmp"), so temp_path == manifest_path; the temp
        // write created the final path and publish then errored (final exists),
        // copying artifacts while reporting failure. The temp sibling is now
        // guaranteed distinct, so apply succeeds and publishes atomically.
        let base = temp_path("bd1whq0-tmp-manifest");
        fs::create_dir_all(&base).map_err(|error| error.to_string())?;
        // Resolve any symlinked path component (e.g. the worker's /Users alias
        // when this is RCH-verified from the /Users checkout) so the relocation
        // symlink-safety guards see a regular-directory path rather than denying
        // the whole operation before the temp-path logic under test runs.
        let base = fs::canonicalize(&base).map_err(|error| error.to_string())?;
        let workspace = base.join("workspace");
        let source = workspace.join("target/debug/sample.o");
        fs::create_dir_all(parent_dir(&source)?).map_err(|error| error.to_string())?;
        fs::write(&source, "artifact bytes\n").map_err(|error| error.to_string())?;
        let destination = base.join("destination");
        let expected_copy = destination
            .join(RELOCATION_DIR)
            .join("target/debug/sample.o");
        let manifest = base.join("manifest").join("relocation.tmp");
        fs::create_dir_all(parent_dir(&manifest)?).map_err(|error| error.to_string())?;

        // The temp sibling must differ from a manifest path that already ends in
        // ".tmp" (the regression this test guards).
        let temp_sibling = relocation_manifest_temp_path(&manifest);
        if temp_sibling == manifest {
            return Err(format!(
                "temp path must differ from a .tmp manifest path; both resolved to {}",
                manifest.display()
            ));
        }

        relocate_artifacts(&ArtifactRelocationOptions {
            workspace_path: &workspace,
            source_path: Some(&source),
            destination_root: Some(&destination),
            manifest_path: &manifest,
            actor: Some("test"),
            mode: ArtifactRelocationMode::Apply,
            force_with_explicit_path: false,
        })
        .map_err(|error| format!("apply with a .tmp manifest path should succeed: {error:?}"))?;

        if !manifest.exists() {
            return Err("manifest was not published at the requested .tmp path".to_owned());
        }
        if !expected_copy.exists() {
            return Err(
                "artifact was not copied during a successful .tmp-manifest apply".to_owned(),
            );
        }
        if temp_sibling.exists() {
            return Err(format!(
                "temporary manifest {} was left on disk after publish",
                temp_sibling.display()
            ));
        }
        Ok(())
    }

    #[test]
    fn relocation_apply_rejects_existing_manifest_before_copying() -> TestResult {
        let workspace = temp_path("existing-final-manifest-workspace");
        let source = workspace.join("target/debug/sample.o");
        fs::create_dir_all(parent_dir(&source)?).map_err(|error| error.to_string())?;
        fs::write(&source, "artifact bytes\n").map_err(|error| error.to_string())?;
        let destination = temp_path("existing-final-manifest-destination");
        let expected_copy = destination
            .join(RELOCATION_DIR)
            .join("target/debug/sample.o");
        let manifest = temp_path("existing-final-manifest").join("relocation.json");
        fs::create_dir_all(parent_dir(&manifest)?).map_err(|error| error.to_string())?;
        fs::write(&manifest, "keep final").map_err(|error| error.to_string())?;

        let result = relocate_artifacts(&ArtifactRelocationOptions {
            workspace_path: &workspace,
            source_path: Some(&source),
            destination_root: Some(&destination),
            manifest_path: &manifest,
            actor: Some("test"),
            mode: ArtifactRelocationMode::Apply,
            force_with_explicit_path: false,
        });

        match result {
            Err(DomainError::Storage { message, repair }) => {
                if !message.contains("relocation manifest path already exists before publish") {
                    return Err(format!("unexpected storage error message: {message}"));
                }
                if repair.as_deref()
                    != Some("Choose a new manifest path; this command will not overwrite.")
                {
                    return Err(format!("unexpected repair hint: {repair:?}"));
                }
            }
            other => {
                return Err(format!(
                    "expected existing manifest storage error, got {other:?}"
                ));
            }
        }
        let final_content = fs::read_to_string(&manifest).map_err(|error| error.to_string())?;
        if final_content != "keep final" {
            return Err("existing final manifest was unexpectedly overwritten".to_owned());
        }
        if expected_copy.exists() {
            return Err("artifact copy happened before existing manifest rejection".to_owned());
        }
        Ok(())
    }

    #[test]
    fn relocation_manifest_publish_rejects_existing_final_file_without_overwriting() -> TestResult {
        let root = temp_path("manifest-existing-final-recheck");
        let manifest = root.join("relocation.json");
        fs::create_dir_all(parent_dir(&manifest)?).map_err(|error| error.to_string())?;
        let temp_manifest = relocation_manifest_temp_path(&manifest);
        fs::write(&temp_manifest, r#"{"schema":"new"}"#).map_err(|error| error.to_string())?;
        fs::write(&manifest, "keep final").map_err(|error| error.to_string())?;

        let result = publish_relocation_manifest_temp_file(&temp_manifest, &manifest);

        match result {
            Err(DomainError::Storage { message, repair }) => {
                if !message.contains("relocation manifest path already exists before publish") {
                    return Err(format!("unexpected storage error message: {message}"));
                }
                if repair.as_deref()
                    != Some("Choose a new manifest path; this command will not overwrite.")
                {
                    return Err(format!("unexpected repair hint: {repair:?}"));
                }
            }
            other => {
                return Err(format!(
                    "expected existing final storage error, got {other:?}"
                ));
            }
        }
        let final_content = fs::read_to_string(&manifest).map_err(|error| error.to_string())?;
        if final_content != "keep final" {
            return Err("existing final manifest was unexpectedly overwritten".to_owned());
        }
        let temp_content = fs::read_to_string(&temp_manifest).map_err(|error| error.to_string())?;
        if temp_content != r#"{"schema":"new"}"# {
            return Err("temporary manifest was unexpectedly removed or mutated".to_owned());
        }
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn relocation_manifest_publish_rechecks_final_symlink_before_rename() -> TestResult {
        let root = temp_path("manifest-final-symlink-recheck");
        let manifest = root.join("relocation.json");
        fs::create_dir_all(parent_dir(&manifest)?).map_err(|error| error.to_string())?;
        let mut temp_manifest = manifest.clone();
        temp_manifest.set_extension("tmp");
        fs::write(&temp_manifest, r#"{"schema":"sentinel"}"#).map_err(|error| error.to_string())?;

        let outside_manifest = root.join("outside-relocation.json");
        fs::write(&outside_manifest, "outside sentinel").map_err(|error| error.to_string())?;
        std::os::unix::fs::symlink(&outside_manifest, &manifest)
            .map_err(|error| error.to_string())?;

        let result = publish_relocation_manifest_temp_file(&temp_manifest, &manifest);
        match result {
            Err(DomainError::PolicyDenied { message, .. }) => {
                if !message.contains("symlink component") {
                    return Err(format!("unexpected policy error message: {message}"));
                }
            }
            other => {
                return Err(format!(
                    "expected final symlink policy denial before publish, got {other:?}"
                ));
            }
        }
        let outside_content =
            fs::read_to_string(&outside_manifest).map_err(|error| error.to_string())?;
        if outside_content != "outside sentinel" {
            return Err("outside manifest target was unexpectedly mutated".to_owned());
        }
        let temp_content = fs::read_to_string(&temp_manifest).map_err(|error| error.to_string())?;
        if temp_content != r#"{"schema":"sentinel"}"# {
            return Err("temporary manifest was unexpectedly removed or mutated".to_owned());
        }
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn relocation_manifest_publish_rechecks_temp_symlink_before_rename() -> TestResult {
        let root = temp_path("manifest-temp-symlink-recheck");
        let manifest = root.join("relocation.json");
        fs::create_dir_all(parent_dir(&manifest)?).map_err(|error| error.to_string())?;
        let mut temp_manifest = manifest.clone();
        temp_manifest.set_extension("tmp");
        let temp_backup = manifest.with_extension("tmp.preserved");
        fs::write(&temp_manifest, r#"{"schema":"sentinel"}"#).map_err(|error| error.to_string())?;
        fs::rename(&temp_manifest, &temp_backup).map_err(|error| error.to_string())?;

        let outside_manifest = root.join("outside-temp-relocation.json");
        fs::write(&outside_manifest, "outside sentinel").map_err(|error| error.to_string())?;
        std::os::unix::fs::symlink(&outside_manifest, &temp_manifest)
            .map_err(|error| error.to_string())?;

        let result = publish_relocation_manifest_temp_file(&temp_manifest, &manifest);
        match result {
            Err(DomainError::PolicyDenied { message, .. }) => {
                if !message.contains("symlink component") {
                    return Err(format!("unexpected policy error message: {message}"));
                }
            }
            other => {
                return Err(format!(
                    "expected temp symlink policy denial before publish, got {other:?}"
                ));
            }
        }
        let outside_content =
            fs::read_to_string(&outside_manifest).map_err(|error| error.to_string())?;
        if outside_content != "outside sentinel" {
            return Err("outside temp manifest target was unexpectedly mutated".to_owned());
        }
        if !fs::symlink_metadata(&temp_manifest)
            .map_err(|error| error.to_string())?
            .file_type()
            .is_symlink()
        {
            return Err("temporary manifest symlink was unexpectedly removed".to_owned());
        }
        if manifest.exists() {
            return Err(
                "relocation manifest was unexpectedly published from temp symlink".to_owned(),
            );
        }
        Ok(())
    }

    #[test]
    fn relocation_apply_rejects_non_regular_temp_manifest_before_create() -> TestResult {
        let workspace = temp_path("directory-temp-manifest-workspace");
        let source = workspace.join("target/debug/sample.o");
        fs::create_dir_all(parent_dir(&source)?).map_err(|error| error.to_string())?;
        fs::write(&source, "artifact bytes\n").map_err(|error| error.to_string())?;
        let destination = temp_path("directory-temp-manifest-destination");
        let manifest = temp_path("directory-temp-manifest").join("relocation.json");
        let temp_manifest = relocation_manifest_temp_path(&manifest);
        fs::create_dir_all(&temp_manifest).map_err(|error| error.to_string())?;

        let result = relocate_artifacts(&ArtifactRelocationOptions {
            workspace_path: &workspace,
            source_path: Some(&source),
            destination_root: Some(&destination),
            manifest_path: &manifest,
            actor: Some("test"),
            mode: ArtifactRelocationMode::Apply,
            force_with_explicit_path: false,
        });

        match result {
            Err(DomainError::Storage { message, repair }) => {
                if !message.contains("temporary relocation manifest path")
                    || !message.contains("not a regular file")
                {
                    return Err(format!("unexpected storage error message: {message}"));
                }
                if repair.as_deref()
                    != Some("Remove the non-regular temporary manifest path and retry.")
                {
                    return Err(format!("unexpected repair hint: {repair:?}"));
                }
            }
            other => {
                return Err(format!(
                    "expected temp manifest storage error, got {other:?}"
                ));
            }
        }
        if manifest.exists() {
            return Err("final manifest was written after temp path rejection".to_owned());
        }
        if !temp_manifest.is_dir() {
            return Err("non-regular temp manifest path was unexpectedly replaced".to_owned());
        }
        Ok(())
    }

    #[test]
    fn hash_file_rejects_non_regular_artifact_path_before_open() -> TestResult {
        let directory = temp_path("hash-directory-artifact");
        fs::create_dir_all(&directory).map_err(|error| error.to_string())?;

        let result = hash_file(&directory);

        match result {
            Err(DomainError::Storage { message, repair }) => {
                if !message.contains("refusing to hash non-regular artifact path") {
                    return Err(format!("unexpected storage error message: {message}"));
                }
                if repair.as_deref() != Some("Relocate only regular artifact files.") {
                    return Err(format!("unexpected repair hint: {repair:?}"));
                }
            }
            other => return Err(format!("expected non-regular hash refusal, got {other:?}")),
        }
        ensure(
            directory.is_dir(),
            "hash refusal should leave directory artifact untouched".to_owned(),
        )
    }

    #[test]
    fn relocation_copy_rejects_existing_destination_without_truncating() -> TestResult {
        let source = temp_path("copy-existing-source").join("source.o");
        let destination = temp_path("copy-existing-destination").join("destination.o");
        fs::create_dir_all(parent_dir(&source)?).map_err(|error| error.to_string())?;
        fs::create_dir_all(parent_dir(&destination)?).map_err(|error| error.to_string())?;
        fs::write(&source, "source bytes\n").map_err(|error| error.to_string())?;
        fs::write(&destination, "keep me\n").map_err(|error| error.to_string())?;

        let result =
            copy_relocation_file_no_overwrite(&source, &destination, "test relocation copy");

        match result {
            Err(DomainError::Storage { message, repair }) => {
                if !message.contains("existing destination") {
                    return Err(format!("unexpected existing destination error: {message}"));
                }
                if repair.as_deref()
                    != Some("Move the conflicting file aside manually before retrying.")
                {
                    return Err(format!(
                        "unexpected existing destination repair: {repair:?}"
                    ));
                }
            }
            other => {
                return Err(format!(
                    "expected existing destination storage error, got {other:?}"
                ));
            }
        }
        let destination_content =
            fs::read_to_string(&destination).map_err(|error| error.to_string())?;
        if destination_content != "keep me\n" {
            return Err("existing relocation destination was unexpectedly truncated".to_owned());
        }
        Ok(())
    }

    #[test]
    fn relocation_apply_rejects_source_hash_mismatch_before_copying() -> TestResult {
        let workspace = temp_path("apply-source-hash-workspace");
        let original = workspace.join("target/debug/source-hash.o");
        fs::create_dir_all(parent_dir(&original)?).map_err(|error| error.to_string())?;
        fs::write(&original, "original bytes\n").map_err(|error| error.to_string())?;
        let expected_hash = hash_file(&original).map_err(|error| error.to_string())?;
        fs::write(&original, "changed bytes\n").map_err(|error| error.to_string())?;

        let destination = temp_path("apply-source-hash-destination")
            .join(RELOCATION_DIR)
            .join("target/debug/source-hash.o");
        let manifest = ArtifactRelocationManifest {
            schema: ARTIFACT_RELOCATION_SCHEMA_V1.to_owned(),
            command_version: env!("CARGO_PKG_VERSION").to_owned(),
            actor: "test".to_owned(),
            created_at: "2026-05-13T00:00:00Z".to_owned(),
            workspace_path: path_to_string(&workspace),
            source_path: path_to_string(&original),
            destination_root: path_to_string(parent_dir(&destination)?),
            restoration_command: "ee artifact relocate --restore --manifest manifest.json --json"
                .to_owned(),
            force_with_explicit_path: false,
            entries: vec![ArtifactRelocationEntry {
                original_path: path_to_string(&original),
                destination_path: path_to_string(&destination),
                kind: "file".to_owned(),
                size_bytes: 15,
                mtime_unix_seconds: None,
                blake3: Some(expected_hash),
                status: "planned".to_owned(),
            }],
        };

        let result = apply_manifest_copy(&manifest);

        match result {
            Err(DomainError::Storage { message, repair }) => {
                if !message.contains("source artifact hash mismatch") {
                    return Err(format!("unexpected hash mismatch message: {message}"));
                }
                if repair.as_deref()
                    != Some("Regenerate the relocation manifest from the current artifact bytes.")
                {
                    return Err(format!("unexpected repair hint: {repair:?}"));
                }
            }
            other => return Err(format!("expected source hash mismatch, got {other:?}")),
        }
        if destination.exists() {
            return Err("destination was copied after source hash mismatch".to_owned());
        }
        Ok(())
    }

    #[test]
    fn relocation_restore_copies_missing_original_from_manifest() -> TestResult {
        let workspace = temp_path("restore-workspace");
        let original = workspace.join("target/debug/restored.o");
        let destination = temp_path("restore-destination")
            .join(RELOCATION_DIR)
            .join("target/debug/restored.o");
        fs::create_dir_all(parent_dir(&destination)?).map_err(|error| error.to_string())?;
        fs::write(&destination, "restore bytes\n").map_err(|error| error.to_string())?;
        let manifest_path = temp_path("restore-manifest").join("relocation.json");
        let manifest =
            relocation_manifest_for(&workspace, &original, &destination, &manifest_path)?;
        write_relocation_manifest(&manifest_path, &manifest)?;

        let report = relocate_artifacts(&ArtifactRelocationOptions {
            workspace_path: &workspace,
            source_path: None,
            destination_root: None,
            manifest_path: &manifest_path,
            actor: Some("test"),
            mode: ArtifactRelocationMode::Restore,
            force_with_explicit_path: false,
        })
        .map_err(|error| error.to_string())?;

        if !original.exists() {
            return Err("restore did not recreate missing original".to_owned());
        }
        let original_hash = hash_file(&original).map_err(|error| error.to_string())?;
        let destination_hash = hash_file(&destination).map_err(|error| error.to_string())?;
        if original_hash != destination_hash {
            return Err("restored file hash mismatch".to_owned());
        }
        if !report.restored {
            return Err("restore report did not mark restored=true".to_owned());
        }
        Ok(())
    }

    #[test]
    fn relocation_restore_rejects_wrong_workspace_original_from_manifest() -> TestResult {
        let active_workspace = temp_path("restore-active-workspace");
        let manifest_workspace = temp_path("restore-manifest-workspace");
        let original = manifest_workspace.join("target/debug/restored.o");
        let destination = temp_path("restore-wrong-workspace-destination")
            .join(RELOCATION_DIR)
            .join("target/debug/restored.o");
        fs::create_dir_all(parent_dir(&destination)?).map_err(|error| error.to_string())?;
        fs::write(&destination, "restore bytes\n").map_err(|error| error.to_string())?;
        let manifest_path = temp_path("restore-wrong-workspace-manifest").join("relocation.json");
        let manifest =
            relocation_manifest_for(&manifest_workspace, &original, &destination, &manifest_path)?;
        write_relocation_manifest(&manifest_path, &manifest)?;

        let result = relocate_artifacts(&ArtifactRelocationOptions {
            workspace_path: &active_workspace,
            source_path: None,
            destination_root: None,
            manifest_path: &manifest_path,
            actor: Some("test"),
            mode: ArtifactRelocationMode::Restore,
            force_with_explicit_path: false,
        });

        match result {
            Err(DomainError::PolicyDenied { message, repair }) => {
                if !message.contains("outside current workspace artifact roots") {
                    return Err(format!("unexpected policy denial message: {message}"));
                }
                if repair
                    .as_deref()
                    .is_none_or(|repair| !repair.contains("--force-with-explicit-path"))
                {
                    return Err(format!("unexpected repair hint: {repair:?}"));
                }
            }
            other => return Err(format!("expected policy denial, got {other:?}")),
        }
        if original.exists() {
            return Err("restore wrote an original from the wrong workspace manifest".to_owned());
        }
        if parent_dir(&original)?.exists() {
            return Err(
                "restore created parent directories outside the active workspace".to_owned(),
            );
        }
        Ok(())
    }

    #[test]
    fn relocation_restore_rejects_relocated_hash_mismatch_before_writing_original() -> TestResult {
        let workspace = temp_path("restore-corrupt-destination-workspace");
        let original = workspace.join("target/debug/restored.o");
        let destination = temp_path("restore-corrupt-destination")
            .join(RELOCATION_DIR)
            .join("target/debug/restored.o");
        fs::create_dir_all(parent_dir(&destination)?).map_err(|error| error.to_string())?;
        fs::write(&destination, "restore bytes\n").map_err(|error| error.to_string())?;
        let manifest_path =
            temp_path("restore-corrupt-destination-manifest").join("relocation.json");
        let manifest =
            relocation_manifest_for(&workspace, &original, &destination, &manifest_path)?;
        write_relocation_manifest(&manifest_path, &manifest)?;
        fs::write(&destination, "corrupted bytes\n").map_err(|error| error.to_string())?;

        let result = relocate_artifacts(&ArtifactRelocationOptions {
            workspace_path: &workspace,
            source_path: None,
            destination_root: None,
            manifest_path: &manifest_path,
            actor: Some("test"),
            mode: ArtifactRelocationMode::Restore,
            force_with_explicit_path: false,
        });

        match result {
            Err(DomainError::Storage { message, repair }) => {
                if !message.contains("relocated artifact hash mismatch") {
                    return Err(format!("unexpected hash mismatch message: {message}"));
                }
                if repair.as_deref()
                    != Some("Verify the relocation manifest and preserved artifact before restore.")
                {
                    return Err(format!("unexpected repair hint: {repair:?}"));
                }
            }
            other => return Err(format!("expected relocated hash mismatch, got {other:?}")),
        }
        if original.exists() {
            return Err("restore wrote original after relocated hash mismatch".to_owned());
        }
        Ok(())
    }

    #[test]
    fn relocation_restore_rejects_missing_manifest_hash_before_writing_original() -> TestResult {
        let workspace = temp_path("restore-missing-hash-workspace");
        let original = workspace.join("target/debug/restored.o");
        let destination = temp_path("restore-missing-hash-destination")
            .join(RELOCATION_DIR)
            .join("target/debug/restored.o");
        fs::create_dir_all(parent_dir(&destination)?).map_err(|error| error.to_string())?;
        fs::write(&destination, "restore bytes\n").map_err(|error| error.to_string())?;
        let manifest_path = temp_path("restore-missing-hash-manifest").join("relocation.json");
        let mut manifest =
            relocation_manifest_for(&workspace, &original, &destination, &manifest_path)?;
        manifest.entries[0].blake3 = None;
        write_relocation_manifest(&manifest_path, &manifest)?;

        let result = relocate_artifacts(&ArtifactRelocationOptions {
            workspace_path: &workspace,
            source_path: None,
            destination_root: None,
            manifest_path: &manifest_path,
            actor: Some("test"),
            mode: ArtifactRelocationMode::Restore,
            force_with_explicit_path: false,
        });

        match result {
            Err(DomainError::Usage { message, repair }) => {
                if !message.contains("missing required blake3 hash") {
                    return Err(format!("unexpected usage error message: {message}"));
                }
                if repair.as_deref()
                    != Some("Use a relocation manifest created by `ee artifact relocate --apply`.")
                {
                    return Err(format!("unexpected repair hint: {repair:?}"));
                }
            }
            other => return Err(format!("expected missing hash usage error, got {other:?}")),
        }
        if original.exists() {
            return Err("restore wrote original from a hashless manifest entry".to_owned());
        }
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn relocation_restore_rejects_symlinked_manifest_path() -> TestResult {
        let workspace = temp_path("restore-symlink-manifest-workspace");
        let original = workspace.join("target/debug/restored.o");
        let destination = temp_path("restore-symlink-manifest-destination")
            .join(RELOCATION_DIR)
            .join("target/debug/restored.o");
        fs::create_dir_all(parent_dir(&destination)?).map_err(|error| error.to_string())?;
        fs::write(&destination, "restore bytes\n").map_err(|error| error.to_string())?;
        let real_manifest = temp_path("restore-symlink-manifest-real").join("relocation.json");
        let manifest =
            relocation_manifest_for(&workspace, &original, &destination, &real_manifest)?;
        write_relocation_manifest(&real_manifest, &manifest)?;
        let manifest_link = temp_path("restore-symlink-manifest-link").join("relocation.json");
        fs::create_dir_all(parent_dir(&manifest_link)?).map_err(|error| error.to_string())?;
        std::os::unix::fs::symlink(&real_manifest, &manifest_link)
            .map_err(|error| error.to_string())?;

        let result = relocate_artifacts(&ArtifactRelocationOptions {
            workspace_path: &workspace,
            source_path: None,
            destination_root: None,
            manifest_path: &manifest_link,
            actor: Some("test"),
            mode: ArtifactRelocationMode::Restore,
            force_with_explicit_path: false,
        });

        if matches!(result, Err(DomainError::PolicyDenied { .. })) {
            Ok(())
        } else {
            Err(format!("expected policy denial, got {result:?}"))
        }
    }

    #[cfg(unix)]
    #[test]
    fn relocation_manifest_final_read_open_rejects_symlinked_path() -> TestResult {
        let root = temp_path("manifest-final-read-symlink");
        let real_manifest = root.join("real-relocation.json");
        fs::create_dir_all(parent_dir(&real_manifest)?).map_err(|error| error.to_string())?;
        let manifest_text = format!(
            r#"{{"schema":"{}","entries":[]}}"#,
            ARTIFACT_RELOCATION_SCHEMA_V1
        );
        fs::write(&real_manifest, &manifest_text).map_err(|error| error.to_string())?;
        let manifest_link = root.join("relocation.json");
        std::os::unix::fs::symlink(&real_manifest, &manifest_link)
            .map_err(|error| error.to_string())?;

        let error = open_relocation_manifest_file_for_read(&manifest_link)
            .expect_err("final manifest read open must reject symlinks");

        if error.kind() == std::io::ErrorKind::NotFound {
            return Err("final symlink read should fail because the path is a symlink".to_owned());
        }
        let real_content = fs::read_to_string(&real_manifest).map_err(|error| error.to_string())?;
        if real_content != manifest_text {
            return Err("manifest read helper unexpectedly mutated the symlink target".to_owned());
        }
        Ok(())
    }

    #[test]
    fn relocation_restore_rejects_non_regular_manifest_path() -> TestResult {
        let workspace = temp_path("restore-directory-manifest-workspace");
        let manifest_path = temp_path("restore-directory-manifest").join("relocation.json");
        fs::create_dir_all(&manifest_path).map_err(|error| error.to_string())?;

        let result = relocate_artifacts(&ArtifactRelocationOptions {
            workspace_path: &workspace,
            source_path: None,
            destination_root: None,
            manifest_path: &manifest_path,
            actor: Some("test"),
            mode: ArtifactRelocationMode::Restore,
            force_with_explicit_path: false,
        });

        match result {
            Err(DomainError::Storage { message, .. }) if message.contains("regular file") => Ok(()),
            other => Err(format!(
                "expected regular-file storage error, got {other:?}"
            )),
        }
    }

    #[cfg(unix)]
    #[test]
    fn relocation_restore_rejects_existing_symlinked_original_file_before_hash() -> TestResult {
        let workspace = temp_path("restore-symlink-final-original-workspace");
        let original = workspace.join("target/debug/restored.o");
        fs::create_dir_all(parent_dir(&original)?).map_err(|error| error.to_string())?;
        let outside_original =
            temp_path("restore-symlink-final-original-target").join("restored.o");
        fs::create_dir_all(parent_dir(&outside_original)?).map_err(|error| error.to_string())?;
        fs::write(&outside_original, "restore bytes\n").map_err(|error| error.to_string())?;
        std::os::unix::fs::symlink(&outside_original, &original)
            .map_err(|error| error.to_string())?;
        let destination = temp_path("restore-symlink-final-original-destination")
            .join(RELOCATION_DIR)
            .join("target/debug/restored.o");
        fs::create_dir_all(parent_dir(&destination)?).map_err(|error| error.to_string())?;
        fs::write(&destination, "restore bytes\n").map_err(|error| error.to_string())?;
        let manifest_path =
            temp_path("restore-symlink-final-original-manifest").join("relocation.json");
        let manifest =
            relocation_manifest_for(&workspace, &original, &destination, &manifest_path)?;
        write_relocation_manifest(&manifest_path, &manifest)?;

        let result = relocate_artifacts(&ArtifactRelocationOptions {
            workspace_path: &workspace,
            source_path: None,
            destination_root: None,
            manifest_path: &manifest_path,
            actor: Some("test"),
            mode: ArtifactRelocationMode::Restore,
            force_with_explicit_path: false,
        });

        if !matches!(result, Err(DomainError::PolicyDenied { .. })) {
            return Err(format!("expected policy denial, got {result:?}"));
        }
        let outside = fs::read_to_string(&outside_original).map_err(|error| error.to_string())?;
        if outside != "restore bytes\n" {
            return Err("restore mutated symlink target before rejecting".to_owned());
        }
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn relocation_restore_rejects_symlinked_original_parent_from_manifest() -> TestResult {
        let workspace = temp_path("restore-symlink-original-workspace");
        let real_original_parent = temp_path("restore-symlink-original-real");
        fs::create_dir_all(&real_original_parent).map_err(|error| error.to_string())?;
        let original_parent_link = temp_path("restore-symlink-original-link");
        std::os::unix::fs::symlink(&real_original_parent, &original_parent_link)
            .map_err(|error| error.to_string())?;
        let original = original_parent_link.join("restored.o");
        let destination = temp_path("restore-symlink-original-destination")
            .join(RELOCATION_DIR)
            .join("restored.o");
        fs::create_dir_all(parent_dir(&destination)?).map_err(|error| error.to_string())?;
        fs::write(&destination, "restore bytes\n").map_err(|error| error.to_string())?;
        let manifest_path = temp_path("restore-symlink-original-manifest").join("relocation.json");
        let manifest =
            relocation_manifest_for(&workspace, &original, &destination, &manifest_path)?;
        write_relocation_manifest(&manifest_path, &manifest)?;

        let result = relocate_artifacts(&ArtifactRelocationOptions {
            workspace_path: &workspace,
            source_path: None,
            destination_root: None,
            manifest_path: &manifest_path,
            actor: Some("test"),
            mode: ArtifactRelocationMode::Restore,
            force_with_explicit_path: false,
        });

        if !matches!(result, Err(DomainError::PolicyDenied { .. })) {
            return Err(format!("expected policy denial, got {result:?}"));
        }
        if real_original_parent.join("restored.o").exists() {
            return Err("restore wrote through symlinked original parent".to_owned());
        }
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn relocation_restore_rejects_symlinked_destination_from_manifest() -> TestResult {
        let workspace = temp_path("restore-symlink-destination-workspace");
        let original = workspace.join("target/debug/restored.o");
        let real_destination = temp_path("restore-symlink-destination-real").join("restored.o");
        fs::create_dir_all(parent_dir(&real_destination)?).map_err(|error| error.to_string())?;
        fs::write(&real_destination, "restore bytes\n").map_err(|error| error.to_string())?;
        let destination_link = temp_path("restore-symlink-destination-link").join("restored.o");
        fs::create_dir_all(parent_dir(&destination_link)?).map_err(|error| error.to_string())?;
        std::os::unix::fs::symlink(&real_destination, &destination_link)
            .map_err(|error| error.to_string())?;
        let manifest_path =
            temp_path("restore-symlink-destination-manifest").join("relocation.json");
        let manifest = ArtifactRelocationManifest {
            schema: ARTIFACT_RELOCATION_SCHEMA_V1.to_owned(),
            command_version: env!("CARGO_PKG_VERSION").to_owned(),
            actor: "test".to_owned(),
            created_at: "2026-05-13T00:00:00Z".to_owned(),
            workspace_path: path_to_string(&workspace),
            source_path: path_to_string(&original),
            destination_root: path_to_string(parent_dir(&destination_link)?),
            restoration_command: format!(
                "ee artifact relocate --restore --manifest {} --json",
                manifest_path.display()
            ),
            force_with_explicit_path: false,
            entries: vec![ArtifactRelocationEntry {
                original_path: path_to_string(&original),
                destination_path: path_to_string(&destination_link),
                kind: "file".to_owned(),
                size_bytes: fs::metadata(&real_destination)
                    .map_err(|error| error.to_string())?
                    .len(),
                mtime_unix_seconds: None,
                blake3: Some(hash_file(&real_destination).map_err(|error| error.to_string())?),
                status: "copied".to_owned(),
            }],
        };
        write_relocation_manifest(&manifest_path, &manifest)?;

        let result = relocate_artifacts(&ArtifactRelocationOptions {
            workspace_path: &workspace,
            source_path: None,
            destination_root: None,
            manifest_path: &manifest_path,
            actor: Some("test"),
            mode: ArtifactRelocationMode::Restore,
            force_with_explicit_path: false,
        });

        if matches!(result, Err(DomainError::PolicyDenied { .. })) {
            Ok(())
        } else {
            Err(format!("expected policy denial, got {result:?}"))
        }
    }
}
