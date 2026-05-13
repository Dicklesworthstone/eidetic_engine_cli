//! Preservation-only artifact relocation manifests.
//!
//! This workflow copies artifacts to a destination root and records a manifest.
//! It never removes originals and never overwrites an existing destination.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use chrono::{SecondsFormat, Utc};
use serde::{Deserialize, Serialize};

use crate::models::DomainError;

pub const ARTIFACT_RELOCATION_SCHEMA_V1: &str = "ee.artifact.relocation.v1";
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
        serde_json::json!(self)
    }

    #[must_use]
    pub fn human_summary(&self) -> String {
        format!(
            "artifact relocation {mode}\n\nentries: {entries}\nmanifest: {manifest}\napplied: {applied}\nrestored: {restored}\n",
            mode = self.mode,
            entries = self.manifest.entries.len(),
            manifest = self.manifest_path,
            applied = self.applied,
            restored = self.restored
        )
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
    let source = absolutize(source);
    if !source.exists() {
        return Err(DomainError::NotFound {
            resource: "artifact source path".to_owned(),
            id: path_to_string(&source),
            repair: Some("Choose an existing artifact root or file.".to_owned()),
        });
    }
    reject_symlink(&source)?;
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
    let entries = collect_entries(
        options.workspace_path,
        &source,
        &destination_root,
        "planned",
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
    for entry in &manifest.entries {
        let original = Path::new(&entry.original_path);
        let destination = Path::new(&entry.destination_path);
        if original.exists() {
            if let Some(expected) = entry.blake3.as_deref() {
                let actual = hash_file(original)?;
                if actual != expected {
                    return Err(DomainError::Storage {
                        message: format!(
                            "Original path exists with different hash: {}.",
                            original.display()
                        ),
                        repair: Some(
                            "Move the conflicting file aside manually before restore.".to_owned(),
                        ),
                    });
                }
            }
            continue;
        }
        if let Some(parent) = original.parent() {
            fs::create_dir_all(parent).map_err(|error| DomainError::Storage {
                message: format!(
                    "failed to create restore parent {}: {error}",
                    parent.display()
                ),
                repair: Some("Check destination permissions.".to_owned()),
            })?;
        }
        fs::copy(destination, original).map_err(|error| DomainError::Storage {
            message: format!(
                "failed to restore {} from {}: {error}",
                original.display(),
                destination.display()
            ),
            repair: Some(
                "Verify the relocation manifest and destination artifact still exist.".to_owned(),
            ),
        })?;
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
        source_allowed: true,
        preservation_policy: "copy_preserve_no_delete_no_overwrite",
        recovery_actions: Vec::new(),
        manifest,
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
    let metadata = fs::metadata(current).map_err(|error| DomainError::Storage {
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
        let original = Path::new(&entry.original_path);
        let destination = Path::new(&entry.destination_path);
        if destination.exists() {
            if let Some(expected) = entry.blake3.as_deref() {
                let actual = hash_file(destination)?;
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
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent).map_err(|error| DomainError::Storage {
                message: format!(
                    "failed to create destination parent {}: {error}",
                    parent.display()
                ),
                repair: Some("Check destination permissions.".to_owned()),
            })?;
        }
        fs::copy(original, destination).map_err(|error| DomainError::Storage {
            message: format!(
                "failed to copy {} to {}: {error}",
                original.display(),
                destination.display()
            ),
            repair: Some("Check destination free space and permissions.".to_owned()),
        })?;
    }
    Ok(())
}

fn write_manifest_no_overwrite(
    manifest_path: &Path,
    manifest: &ArtifactRelocationManifest,
) -> Result<(), DomainError> {
    if manifest_path.exists() {
        return Err(DomainError::Storage {
            message: format!("manifest already exists: {}", manifest_path.display()),
            repair: Some("Choose a new manifest path; this command will not overwrite.".to_owned()),
        });
    }
    if let Some(parent) = manifest_path.parent() {
        fs::create_dir_all(parent).map_err(|error| DomainError::Storage {
            message: format!(
                "failed to create manifest parent {}: {error}",
                parent.display()
            ),
            repair: Some("Check manifest directory permissions.".to_owned()),
        })?;
    }
    let json = serde_json::to_string_pretty(manifest).map_err(|error| DomainError::Storage {
        message: format!("failed to serialize relocation manifest: {error}"),
        repair: Some("Report the serialization failure.".to_owned()),
    })?;
    fs::write(manifest_path, json).map_err(|error| DomainError::Storage {
        message: format!(
            "failed to write manifest {}: {error}",
            manifest_path.display()
        ),
        repair: Some("Check manifest path permissions.".to_owned()),
    })
}

fn read_manifest(path: &Path) -> Result<ArtifactRelocationManifest, DomainError> {
    let text = fs::read_to_string(path).map_err(|error| DomainError::Storage {
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
    let workspace = absolutize(workspace);
    let relative = current.strip_prefix(&workspace).ok().or_else(|| {
        current
            .strip_prefix(root_source)
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

fn hash_file(path: &Path) -> Result<String, DomainError> {
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

fn absolutize(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    }
}

fn path_to_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let unique = std::time::SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default();
        root.join("ee-artifact-relocation-tests")
            .join(format!("{label}-{}-{unique}", std::process::id()))
    }

    fn parent_dir(path: &Path) -> TestResult<&Path> {
        path.parent()
            .ok_or_else(|| format!("path has no parent: {}", path.display()))
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
        fs::create_dir_all(parent_dir(&manifest_path)?).map_err(|error| error.to_string())?;
        let manifest = ArtifactRelocationManifest {
            schema: ARTIFACT_RELOCATION_SCHEMA_V1.to_owned(),
            command_version: env!("CARGO_PKG_VERSION").to_owned(),
            actor: "test".to_owned(),
            created_at: "2026-05-13T00:00:00Z".to_owned(),
            workspace_path: path_to_string(&workspace),
            source_path: path_to_string(&original),
            destination_root: path_to_string(parent_dir(&destination)?),
            restoration_command: format!(
                "ee artifact relocate --restore --manifest {} --json",
                manifest_path.display()
            ),
            force_with_explicit_path: false,
            entries: vec![ArtifactRelocationEntry {
                original_path: path_to_string(&original),
                destination_path: path_to_string(&destination),
                kind: "file".to_owned(),
                size_bytes: 14,
                mtime_unix_seconds: None,
                blake3: Some(hash_file(&destination).map_err(|error| error.to_string())?),
                status: "copied".to_owned(),
            }],
        };
        let json = serde_json::to_string_pretty(&manifest).map_err(|error| error.to_string())?;
        fs::write(&manifest_path, json).map_err(|error| error.to_string())?;

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
}
