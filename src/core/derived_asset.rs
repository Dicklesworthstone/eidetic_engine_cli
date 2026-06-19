//! Content-addressed derived-asset store.
//!
//! This store is for rebuildable artifacts whose source of truth remains the
//! database plus the command/config manifest that produced them. It deliberately
//! differs from live caches: writes are create-only, reuse is read-only, and
//! cleanup output only proposes human-reviewed actions.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::config::{
    PlatformDataDirError, resolve_dir_unix_xdg, resolve_dir_windows_appdata,
    resolve_dir_windows_localappdata,
};

pub const DERIVED_ASSET_STORE_SUMMARY_SCHEMA_V1: &str = "ee.derived_asset_store.summary.v1";
pub const DERIVED_ASSET_OBJECT_SCHEMA_V1: &str = "ee.derived_asset_store.object.v1";
pub const DERIVED_ASSET_REF_SCHEMA_V1: &str = "ee.derived_asset_store.ref.v1";

pub const DERIVED_ASSET_HASH_MISMATCH_CODE: &str = "derived_asset_hash_mismatch";
pub const DERIVED_ASSET_SCHEMA_MISMATCH_CODE: &str = "derived_asset_schema_mismatch";

const CLEANUP_POLICY: &str = "human_review_only_no_automatic_delete";
const BODY_FILE: &str = "body.bin";
const METADATA_FILE: &str = "metadata.json";
const OBJECTS_DIR: &str = "objects";
const REFS_DIR: &str = "refs";
const DERIVED_ASSET_BODY_MAX_BYTES: u64 = 256 * 1024 * 1024;
const DERIVED_ASSET_MANIFEST_MAX_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DerivedAssetDescriptor {
    pub asset_kind: String,
    pub schema_version: String,
    pub source_manifest_hash: String,
    pub config_hash: String,
    pub binary_capability_hash: String,
    pub body_hash: String,
}

impl DerivedAssetDescriptor {
    #[must_use]
    pub fn new(
        asset_kind: impl Into<String>,
        schema_version: impl Into<String>,
        source_manifest_hash: impl Into<String>,
        config_hash: impl Into<String>,
        binary_capability_hash: impl Into<String>,
        body_hash: impl Into<String>,
    ) -> Self {
        Self {
            asset_kind: asset_kind.into(),
            schema_version: schema_version.into(),
            source_manifest_hash: source_manifest_hash.into(),
            config_hash: config_hash.into(),
            binary_capability_hash: binary_capability_hash.into(),
            body_hash: body_hash.into(),
        }
    }

    #[must_use]
    pub fn key(&self) -> String {
        let mut hasher = blake3::Hasher::new();
        for (field, value) in [
            ("store_schema", DERIVED_ASSET_OBJECT_SCHEMA_V1),
            ("asset_kind", self.asset_kind.as_str()),
            ("schema_version", self.schema_version.as_str()),
            ("source_manifest_hash", self.source_manifest_hash.as_str()),
            ("config_hash", self.config_hash.as_str()),
            (
                "binary_capability_hash",
                self.binary_capability_hash.as_str(),
            ),
            ("body_hash", self.body_hash.as_str()),
        ] {
            hasher.update(field.as_bytes());
            hasher.update(b"\0");
            hasher.update(value.as_bytes());
            hasher.update(b"\0");
        }
        format!("da_{}", hasher.finalize().to_hex())
    }

    fn validate_body(&self, bytes: &[u8]) -> Result<(), DerivedAssetStoreError> {
        let actual = blake3_body_hash(bytes);
        if self.body_hash == actual {
            Ok(())
        } else {
            Err(DerivedAssetStoreError::HashMismatch {
                expected: self.body_hash.clone(),
                actual,
            })
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DerivedAssetAttachMode {
    HardLink,
    Copy,
    AlreadyPresent,
}

impl DerivedAssetAttachMode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::HardLink => "hard_link",
            Self::Copy => "copy",
            Self::AlreadyPresent => "already_present",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DerivedAssetPutOutcome {
    pub key: String,
    pub object_path: PathBuf,
    pub reference_path: PathBuf,
    pub reused_existing: bool,
    pub reference_count: usize,
    pub bytes: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DerivedAssetAttachOutcome {
    pub key: String,
    pub object_path: PathBuf,
    pub destination_path: PathBuf,
    pub mode: DerivedAssetAttachMode,
    pub bytes: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DerivedAssetStoreSummary {
    pub schema: &'static str,
    pub root: Option<PathBuf>,
    pub status: &'static str,
    pub root_present: bool,
    pub object_count: usize,
    pub reusable_object_count: usize,
    pub reference_count: usize,
    pub total_bytes: u64,
    pub invalid_object_count: usize,
    pub cleanup_candidates: Vec<DerivedAssetCleanupCandidate>,
    pub degraded: Vec<DerivedAssetSummaryDegradation>,
}

impl DerivedAssetStoreSummary {
    #[must_use]
    pub fn data_json(&self) -> Value {
        json!({
            "schema": self.schema,
            "root": self.root.as_ref().map(|root| root.display().to_string()),
            "status": self.status,
            "rootPresent": self.root_present,
            "objectCount": self.object_count,
            "reusableObjectCount": self.reusable_object_count,
            "referenceCount": self.reference_count,
            "totalBytes": self.total_bytes,
            "invalidObjectCount": self.invalid_object_count,
            "reuseMode": "read_only",
            "copyFallback": true,
            "cleanup": {
                "policy": CLEANUP_POLICY,
                "automaticDeletion": false,
                "candidateCount": self.cleanup_candidates.len(),
                "candidates": self
                    .cleanup_candidates
                    .iter()
                    .map(DerivedAssetCleanupCandidate::data_json)
                    .collect::<Vec<_>>(),
            },
            "degraded": self
                .degraded
                .iter()
                .map(DerivedAssetSummaryDegradation::data_json)
                .collect::<Vec<_>>(),
        })
    }

    fn unavailable(root: Option<PathBuf>, message: String) -> Self {
        Self {
            schema: DERIVED_ASSET_STORE_SUMMARY_SCHEMA_V1,
            root,
            status: "unavailable",
            root_present: false,
            object_count: 0,
            reusable_object_count: 0,
            reference_count: 0,
            total_bytes: 0,
            invalid_object_count: 0,
            cleanup_candidates: Vec::new(),
            degraded: vec![DerivedAssetSummaryDegradation {
                code: DERIVED_ASSET_SCHEMA_MISMATCH_CODE,
                severity: "medium",
                message,
                repair: "inspect the derived asset store root and retry support bundle collection"
                    .to_owned(),
            }],
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DerivedAssetCleanupCandidate {
    pub key: String,
    pub reason: &'static str,
    pub bytes: u64,
    pub reference_count: usize,
}

impl DerivedAssetCleanupCandidate {
    fn data_json(&self) -> Value {
        json!({
            "key": self.key,
            "reason": self.reason,
            "bytes": self.bytes,
            "referenceCount": self.reference_count,
            "action": "manual_review_required",
            "deleteCommand": Value::Null,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DerivedAssetSummaryDegradation {
    pub code: &'static str,
    pub severity: &'static str,
    pub message: String,
    pub repair: String,
}

impl DerivedAssetSummaryDegradation {
    fn data_json(&self) -> Value {
        json!({
            "code": self.code,
            "severity": self.severity,
            "message": self.message,
            "repair": self.repair,
        })
    }
}

#[derive(Debug)]
pub enum DerivedAssetStoreError {
    Io {
        path: PathBuf,
        operation: &'static str,
        source: io::Error,
    },
    Serialize {
        path: PathBuf,
        operation: &'static str,
        source: serde_json::Error,
    },
    HashMismatch {
        expected: String,
        actual: String,
    },
    SchemaMismatch {
        path: PathBuf,
        expected: &'static str,
        actual: String,
    },
    DescriptorMismatch {
        path: PathBuf,
        expected_key: String,
        actual_key: String,
    },
    DestinationExists {
        path: PathBuf,
        existing_hash: String,
    },
    DataDir {
        source: PlatformDataDirError,
    },
}

impl DerivedAssetStoreError {
    #[must_use]
    pub const fn code(&self) -> Option<&'static str> {
        match self {
            Self::HashMismatch { .. } => Some(DERIVED_ASSET_HASH_MISMATCH_CODE),
            Self::SchemaMismatch { .. } | Self::DescriptorMismatch { .. } => {
                Some(DERIVED_ASSET_SCHEMA_MISMATCH_CODE)
            }
            _ => None,
        }
    }
}

impl fmt::Display for DerivedAssetStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io {
                path,
                operation,
                source,
            } => write!(
                formatter,
                "derived asset store {operation} failed at {}: {source}",
                path.display()
            ),
            Self::Serialize {
                path,
                operation,
                source,
            } => write!(
                formatter,
                "derived asset store {operation} failed at {}: {source}",
                path.display()
            ),
            Self::HashMismatch { expected, actual } => write!(
                formatter,
                "derived asset body hash mismatch: expected {expected}, actual {actual}"
            ),
            Self::SchemaMismatch {
                path,
                expected,
                actual,
            } => write!(
                formatter,
                "derived asset schema mismatch at {}: expected {expected}, actual {actual}",
                path.display()
            ),
            Self::DescriptorMismatch {
                path,
                expected_key,
                actual_key,
            } => write!(
                formatter,
                "derived asset descriptor mismatch at {}: expected key {expected_key}, actual key {actual_key}",
                path.display()
            ),
            Self::DestinationExists {
                path,
                existing_hash,
            } => write!(
                formatter,
                "derived asset attach destination already exists at {} with hash {existing_hash}",
                path.display()
            ),
            Self::DataDir { source } => write!(
                formatter,
                "derived asset store data directory could not be resolved: {source}"
            ),
        }
    }
}

impl std::error::Error for DerivedAssetStoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Serialize { source, .. } => Some(source),
            Self::DataDir { source } => Some(source),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DerivedAssetStore {
    root: PathBuf,
}

impl DerivedAssetStore {
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn from_default_env() -> Result<Self, DerivedAssetStoreError> {
        Ok(Self::new(default_derived_asset_store_root()?))
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn put_bytes(
        &self,
        descriptor: &DerivedAssetDescriptor,
        workspace_fingerprint: &str,
        bytes: &[u8],
    ) -> Result<DerivedAssetPutOutcome, DerivedAssetStoreError> {
        descriptor.validate_body(bytes)?;
        let key = descriptor.key();
        let object_dir = self.object_dir(&key);
        let refs_dir = self.refs_dir(&key);
        ensure_no_symlink_components(&self.root, "inspect_root")?;
        ensure_directory(&self.root)?;
        ensure_directory(&self.root.join(OBJECTS_DIR))?;
        ensure_directory(&self.root.join(REFS_DIR))?;
        ensure_directory(&object_dir)?;
        ensure_directory(&refs_dir)?;

        let body_path = self.body_path(&key);
        let metadata_path = self.object_metadata_path(&key);
        ensure_bytes_within_cap(
            &body_path,
            u64::try_from(bytes.len()).unwrap_or(u64::MAX),
            DERIVED_ASSET_BODY_MAX_BYTES,
            "write_body",
        )?;
        let reused_existing = match read_body_file(&body_path, "read_body") {
            Ok(existing) => {
                descriptor.validate_body(&existing)?;
                true
            }
            Err(DerivedAssetStoreError::Io { source, .. })
                if source.kind() == io::ErrorKind::NotFound =>
            {
                write_create_only(&body_path, bytes)?;
                set_read_only(&body_path)?;
                false
            }
            Err(error) => return Err(error),
        };

        match read_object_manifest(&metadata_path) {
            Ok(manifest) => validate_object_manifest(&manifest, &key, descriptor, &metadata_path)?,
            Err(DerivedAssetStoreError::Io { source, .. })
                if source.kind() == io::ErrorKind::NotFound =>
            {
                let manifest =
                    DerivedAssetObjectManifest::new(&key, descriptor, bytes.len() as u64);
                write_json_create_only(&metadata_path, &manifest)?;
                set_read_only(&metadata_path)?;
            }
            Err(error) => return Err(error),
        }

        let reference_path = self.reference_path(&key, workspace_fingerprint);
        match read_ref_manifest(&reference_path) {
            Ok(manifest) => validate_ref_manifest(&manifest, &key, &reference_path)?,
            Err(DerivedAssetStoreError::Io { source, .. })
                if source.kind() == io::ErrorKind::NotFound =>
            {
                let manifest = DerivedAssetRefManifest::new(&key, workspace_fingerprint);
                write_json_create_only(&reference_path, &manifest)?;
                set_read_only(&reference_path)?;
            }
            Err(error) => return Err(error),
        }

        Ok(DerivedAssetPutOutcome {
            key,
            object_path: body_path,
            reference_path,
            reused_existing,
            reference_count: count_ref_files(&refs_dir)?,
            bytes: bytes.len() as u64,
        })
    }

    pub fn attach_read_only(
        &self,
        descriptor: &DerivedAssetDescriptor,
        destination_path: &Path,
    ) -> Result<DerivedAssetAttachOutcome, DerivedAssetStoreError> {
        let key = descriptor.key();
        let object_path = self.body_path(&key);
        self.validate_object(descriptor)?;
        ensure_parent_directory(destination_path)?;
        ensure_no_symlink_components(destination_path, "inspect_destination")?;

        match read_body_file(destination_path, "read_destination") {
            Ok(existing) => {
                let existing_hash = blake3_body_hash(&existing);
                if existing_hash != descriptor.body_hash {
                    return Err(DerivedAssetStoreError::DestinationExists {
                        path: destination_path.to_path_buf(),
                        existing_hash,
                    });
                }
                return Ok(DerivedAssetAttachOutcome {
                    key,
                    object_path,
                    destination_path: destination_path.to_path_buf(),
                    mode: DerivedAssetAttachMode::AlreadyPresent,
                    bytes: existing.len() as u64,
                });
            }
            Err(DerivedAssetStoreError::Io { source, .. })
                if source.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }

        let mode = match fs::hard_link(&object_path, destination_path) {
            Ok(()) => DerivedAssetAttachMode::HardLink,
            Err(_) => {
                copy_create_only(&object_path, destination_path)?;
                DerivedAssetAttachMode::Copy
            }
        };
        set_read_only(destination_path)?;
        let bytes = fs::metadata(destination_path)
            .map_err(|source| DerivedAssetStoreError::Io {
                path: destination_path.to_path_buf(),
                operation: "metadata",
                source,
            })?
            .len();
        Ok(DerivedAssetAttachOutcome {
            key,
            object_path,
            destination_path: destination_path.to_path_buf(),
            mode,
            bytes,
        })
    }

    pub fn validate_object(
        &self,
        descriptor: &DerivedAssetDescriptor,
    ) -> Result<(), DerivedAssetStoreError> {
        let key = descriptor.key();
        let body_path = self.body_path(&key);
        let metadata_path = self.object_metadata_path(&key);
        let manifest = read_object_manifest(&metadata_path)?;
        validate_object_manifest(&manifest, &key, descriptor, &metadata_path)?;
        let body = read_body_file(&body_path, "read_body")?;
        descriptor.validate_body(&body)
    }

    pub fn summary(&self) -> DerivedAssetStoreSummary {
        let mut summary = DerivedAssetStoreSummary {
            schema: DERIVED_ASSET_STORE_SUMMARY_SCHEMA_V1,
            root: Some(self.root.clone()),
            status: "ok",
            root_present: self.root.is_dir(),
            object_count: 0,
            reusable_object_count: 0,
            reference_count: 0,
            total_bytes: 0,
            invalid_object_count: 0,
            cleanup_candidates: Vec::new(),
            degraded: Vec::new(),
        };
        if !summary.root_present {
            return summary;
        }
        if let Err(error) = ensure_no_symlink_components(&self.root, "inspect_root") {
            summary.status = "degraded";
            summary.degraded.push(DerivedAssetSummaryDegradation {
                code: DERIVED_ASSET_SCHEMA_MISMATCH_CODE,
                severity: "high",
                message: error.to_string(),
                repair: "move the derived asset store root away from symlinked components"
                    .to_owned(),
            });
            return summary;
        }

        let object_root = self.root.join(OBJECTS_DIR);
        let mut object_dirs = match sorted_directories(&object_root) {
            Ok(paths) => paths,
            Err(error) if error.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(error) => {
                summary.status = "degraded";
                summary.degraded.push(DerivedAssetSummaryDegradation {
                    code: DERIVED_ASSET_SCHEMA_MISMATCH_CODE,
                    severity: "medium",
                    message: format!("failed to read {}: {error}", object_root.display()),
                    repair: "inspect the derived asset object directory permissions".to_owned(),
                });
                return summary;
            }
        };
        object_dirs.sort();
        let mut hash_mismatch_count = 0usize;
        let mut schema_mismatch_count = 0usize;

        for object_dir in object_dirs {
            let Some(key) = object_dir.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            summary.object_count = summary.object_count.saturating_add(1);
            let body_path = object_dir.join(BODY_FILE);
            let metadata_path = object_dir.join(METADATA_FILE);
            let object_bytes = fs::symlink_metadata(&body_path)
                .ok()
                .filter(|metadata| metadata.file_type().is_file())
                .map_or(0, |metadata| metadata.len());
            summary.total_bytes = summary.total_bytes.saturating_add(object_bytes);

            let refs_dir = self.refs_dir(key);
            let refs = count_ref_files(&refs_dir).unwrap_or(0);
            summary.reference_count = summary.reference_count.saturating_add(refs);
            if refs > 0 {
                summary.reusable_object_count = summary.reusable_object_count.saturating_add(1);
            } else {
                summary
                    .cleanup_candidates
                    .push(DerivedAssetCleanupCandidate {
                        key: key.to_owned(),
                        reason: "orphaned_reference_count_zero",
                        bytes: object_bytes,
                        reference_count: refs,
                    });
            }

            let invalid = match read_object_manifest(&metadata_path) {
                Ok(manifest)
                    if manifest.schema != DERIVED_ASSET_OBJECT_SCHEMA_V1 || manifest.key != key =>
                {
                    schema_mismatch_count = schema_mismatch_count.saturating_add(1);
                    true
                }
                Ok(manifest) => match read_body_file(&body_path, "read_body") {
                    Ok(bytes) => match manifest.descriptor.validate_body(&bytes) {
                        Ok(()) => false,
                        Err(DerivedAssetStoreError::HashMismatch { .. }) => {
                            hash_mismatch_count = hash_mismatch_count.saturating_add(1);
                            true
                        }
                        Err(_) => {
                            schema_mismatch_count = schema_mismatch_count.saturating_add(1);
                            true
                        }
                    },
                    Err(_) => {
                        schema_mismatch_count = schema_mismatch_count.saturating_add(1);
                        true
                    }
                },
                Err(_) => {
                    schema_mismatch_count = schema_mismatch_count.saturating_add(1);
                    true
                }
            };
            if invalid {
                summary.invalid_object_count = summary.invalid_object_count.saturating_add(1);
            }
        }
        if hash_mismatch_count > 0 {
            summary.status = "degraded";
            summary.degraded.push(DerivedAssetSummaryDegradation {
                code: DERIVED_ASSET_HASH_MISMATCH_CODE,
                severity: "high",
                message: format!(
                    "{hash_mismatch_count} derived asset object(s) failed body hash validation"
                ),
                repair:
                    "rebuild affected derived assets locally; do not reuse hash-mismatched objects"
                        .to_owned(),
            });
        }
        if schema_mismatch_count > 0 {
            summary.status = "degraded";
            summary.degraded.push(DerivedAssetSummaryDegradation {
                code: DERIVED_ASSET_SCHEMA_MISMATCH_CODE,
                severity: "high",
                message: format!(
                    "{schema_mismatch_count} derived asset object(s) failed metadata/schema validation"
                ),
                repair: "rebuild affected derived assets locally; do not reuse invalid objects"
                    .to_owned(),
            });
        }
        summary
    }

    fn object_dir(&self, key: &str) -> PathBuf {
        self.root.join(OBJECTS_DIR).join(key)
    }

    fn body_path(&self, key: &str) -> PathBuf {
        self.object_dir(key).join(BODY_FILE)
    }

    fn object_metadata_path(&self, key: &str) -> PathBuf {
        self.object_dir(key).join(METADATA_FILE)
    }

    fn refs_dir(&self, key: &str) -> PathBuf {
        self.root.join(REFS_DIR).join(key)
    }

    fn reference_path(&self, key: &str, workspace_fingerprint: &str) -> PathBuf {
        self.refs_dir(key).join(format!(
            "{}.json",
            workspace_ref_file_stem(workspace_fingerprint)
        ))
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DerivedAssetObjectManifest {
    schema: String,
    key: String,
    descriptor: DerivedAssetDescriptor,
    bytes: u64,
    reuse_mode: String,
    cleanup_policy: String,
}

impl DerivedAssetObjectManifest {
    fn new(key: &str, descriptor: &DerivedAssetDescriptor, bytes: u64) -> Self {
        Self {
            schema: DERIVED_ASSET_OBJECT_SCHEMA_V1.to_owned(),
            key: key.to_owned(),
            descriptor: descriptor.clone(),
            bytes,
            reuse_mode: "read_only".to_owned(),
            cleanup_policy: CLEANUP_POLICY.to_owned(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DerivedAssetRefManifest {
    schema: String,
    key: String,
    workspace_fingerprint_hash: String,
    lease_policy: String,
}

impl DerivedAssetRefManifest {
    fn new(key: &str, workspace_fingerprint: &str) -> Self {
        Self {
            schema: DERIVED_ASSET_REF_SCHEMA_V1.to_owned(),
            key: key.to_owned(),
            workspace_fingerprint_hash: blake3_body_hash(workspace_fingerprint.as_bytes()),
            lease_policy: CLEANUP_POLICY.to_owned(),
        }
    }
}

#[must_use]
pub fn blake3_body_hash(bytes: &[u8]) -> String {
    format!("blake3:{}", blake3::hash(bytes).to_hex())
}

pub fn default_derived_asset_store_root() -> Result<PathBuf, DerivedAssetStoreError> {
    default_derived_asset_store_root_from_env(
        std::env::vars_os()
            .filter_map(|(key, value)| key.into_string().ok().map(|key| (key, value)))
            .collect(),
    )
}

pub fn default_derived_asset_store_root_from_env(
    env: BTreeMap<String, OsString>,
) -> Result<PathBuf, DerivedAssetStoreError> {
    let data_dir = if cfg!(windows) {
        resolve_dir_windows_localappdata(&env)
            .or_else(|_| resolve_dir_windows_appdata(&env))
            .map_err(|source| DerivedAssetStoreError::DataDir { source })?
    } else {
        resolve_dir_unix_xdg(&env, "ee")
            .map_err(|source| DerivedAssetStoreError::DataDir { source })?
    };
    Ok(data_dir.join("derived-assets"))
}

#[must_use]
pub fn gather_default_derived_asset_store_summary() -> DerivedAssetStoreSummary {
    match DerivedAssetStore::from_default_env() {
        Ok(store) => store.summary(),
        Err(error) => DerivedAssetStoreSummary::unavailable(None, error.to_string()),
    }
}

fn validate_object_manifest(
    manifest: &DerivedAssetObjectManifest,
    key: &str,
    descriptor: &DerivedAssetDescriptor,
    path: &Path,
) -> Result<(), DerivedAssetStoreError> {
    if manifest.schema != DERIVED_ASSET_OBJECT_SCHEMA_V1 {
        return Err(DerivedAssetStoreError::SchemaMismatch {
            path: path.to_path_buf(),
            expected: DERIVED_ASSET_OBJECT_SCHEMA_V1,
            actual: manifest.schema.clone(),
        });
    }
    if manifest.key != key || manifest.descriptor != *descriptor {
        return Err(DerivedAssetStoreError::DescriptorMismatch {
            path: path.to_path_buf(),
            expected_key: key.to_owned(),
            actual_key: manifest.key.clone(),
        });
    }
    Ok(())
}

fn validate_ref_manifest(
    manifest: &DerivedAssetRefManifest,
    key: &str,
    path: &Path,
) -> Result<(), DerivedAssetStoreError> {
    if manifest.schema != DERIVED_ASSET_REF_SCHEMA_V1 {
        return Err(DerivedAssetStoreError::SchemaMismatch {
            path: path.to_path_buf(),
            expected: DERIVED_ASSET_REF_SCHEMA_V1,
            actual: manifest.schema.clone(),
        });
    }
    if manifest.key != key {
        return Err(DerivedAssetStoreError::DescriptorMismatch {
            path: path.to_path_buf(),
            expected_key: key.to_owned(),
            actual_key: manifest.key.clone(),
        });
    }
    Ok(())
}

fn read_object_manifest(path: &Path) -> Result<DerivedAssetObjectManifest, DerivedAssetStoreError> {
    let bytes = read_manifest_file(path, "read_manifest")?;
    serde_json::from_slice(&bytes).map_err(|source| DerivedAssetStoreError::Serialize {
        path: path.to_path_buf(),
        operation: "parse_manifest",
        source,
    })
}

fn read_ref_manifest(path: &Path) -> Result<DerivedAssetRefManifest, DerivedAssetStoreError> {
    let bytes = read_manifest_file(path, "read_ref")?;
    serde_json::from_slice(&bytes).map_err(|source| DerivedAssetStoreError::Serialize {
        path: path.to_path_buf(),
        operation: "parse_ref",
        source,
    })
}

fn write_json_create_only<T: Serialize>(
    path: &Path,
    value: &T,
) -> Result<(), DerivedAssetStoreError> {
    let bytes =
        serde_json::to_vec_pretty(value).map_err(|source| DerivedAssetStoreError::Serialize {
            path: path.to_path_buf(),
            operation: "serialize",
            source,
        })?;
    write_create_only(path, &bytes)
}

fn write_create_only(path: &Path, bytes: &[u8]) -> Result<(), DerivedAssetStoreError> {
    ensure_parent_directory(path)?;
    ensure_no_symlink_components(path, "inspect_write_path")?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    configure_derived_asset_open_no_follow(&mut options);
    let mut file = options
        .open(path)
        .map_err(|source| DerivedAssetStoreError::Io {
            path: path.to_path_buf(),
            operation: "create_new",
            source,
        })?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|source| DerivedAssetStoreError::Io {
            path: path.to_path_buf(),
            operation: "write_sync",
            source,
        })
}

fn copy_create_only(
    source_path: &Path,
    destination_path: &Path,
) -> Result<(), DerivedAssetStoreError> {
    ensure_parent_directory(destination_path)?;
    ensure_no_symlink_components(source_path, "inspect_copy_source")?;
    ensure_no_symlink_components(destination_path, "inspect_copy_destination")?;
    let bytes = read_body_file(source_path, "read_copy_source")?;
    write_create_only(destination_path, &bytes)
}

fn read_body_file(path: &Path, operation: &'static str) -> Result<Vec<u8>, DerivedAssetStoreError> {
    read_regular_file_bounded_no_follow(path, DERIVED_ASSET_BODY_MAX_BYTES, operation)
}

fn read_manifest_file(
    path: &Path,
    operation: &'static str,
) -> Result<Vec<u8>, DerivedAssetStoreError> {
    read_regular_file_bounded_no_follow(path, DERIVED_ASSET_MANIFEST_MAX_BYTES, operation)
}

fn read_regular_file_bounded_no_follow(
    path: &Path,
    max_bytes: u64,
    operation: &'static str,
) -> Result<Vec<u8>, DerivedAssetStoreError> {
    ensure_no_symlink_components(path, operation)?;
    let metadata = fs::symlink_metadata(path).map_err(|source| DerivedAssetStoreError::Io {
        path: path.to_path_buf(),
        operation,
        source,
    })?;
    if !metadata.file_type().is_file() {
        return Err(DerivedAssetStoreError::Io {
            path: path.to_path_buf(),
            operation,
            source: io::Error::new(
                io::ErrorKind::InvalidData,
                "derived asset path is not a regular file",
            ),
        });
    }
    ensure_bytes_within_cap(path, metadata.len(), max_bytes, operation)?;

    let file = open_derived_asset_file_for_read_no_follow(path).map_err(|source| {
        DerivedAssetStoreError::Io {
            path: path.to_path_buf(),
            operation,
            source,
        }
    })?;
    let opened_metadata = file
        .metadata()
        .map_err(|source| DerivedAssetStoreError::Io {
            path: path.to_path_buf(),
            operation,
            source,
        })?;
    if !opened_metadata.file_type().is_file() {
        return Err(DerivedAssetStoreError::Io {
            path: path.to_path_buf(),
            operation,
            source: io::Error::new(
                io::ErrorKind::InvalidData,
                "opened derived asset path is not a regular file",
            ),
        });
    }
    ensure_bytes_within_cap(path, opened_metadata.len(), max_bytes, operation)?;

    let mut bytes = Vec::new();
    file.take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|source| DerivedAssetStoreError::Io {
            path: path.to_path_buf(),
            operation,
            source,
        })?;
    ensure_bytes_within_cap(
        path,
        u64::try_from(bytes.len()).unwrap_or(u64::MAX),
        max_bytes,
        operation,
    )?;
    Ok(bytes)
}

fn ensure_bytes_within_cap(
    path: &Path,
    byte_len: u64,
    max_bytes: u64,
    operation: &'static str,
) -> Result<(), DerivedAssetStoreError> {
    if byte_len > max_bytes {
        return Err(DerivedAssetStoreError::Io {
            path: path.to_path_buf(),
            operation,
            source: io::Error::new(
                io::ErrorKind::InvalidData,
                format!("derived asset file exceeds the {max_bytes}-byte cap"),
            ),
        });
    }
    Ok(())
}

fn open_derived_asset_file_for_read_no_follow(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    configure_derived_asset_open_no_follow(&mut options);
    options.open(path)
}

#[cfg(all(unix, not(any(target_os = "espidf", target_os = "horizon"))))]
fn configure_derived_asset_open_no_follow(options: &mut OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt;

    options.custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32);
}

#[cfg(not(all(unix, not(any(target_os = "espidf", target_os = "horizon")))))]
fn configure_derived_asset_open_no_follow(_options: &mut OpenOptions) {}

fn ensure_directory(path: &Path) -> Result<(), DerivedAssetStoreError> {
    ensure_no_symlink_components(path, "inspect_directory")?;
    fs::create_dir_all(path).map_err(|source| DerivedAssetStoreError::Io {
        path: path.to_path_buf(),
        operation: "create_dir_all",
        source,
    })?;
    ensure_no_symlink_components(path, "inspect_directory")?;
    #[cfg(unix)]
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|source| {
        DerivedAssetStoreError::Io {
            path: path.to_path_buf(),
            operation: "set_dir_permissions",
            source,
        }
    })?;
    Ok(())
}

fn ensure_parent_directory(path: &Path) -> Result<(), DerivedAssetStoreError> {
    let parent = path.parent().ok_or_else(|| DerivedAssetStoreError::Io {
        path: path.to_path_buf(),
        operation: "parent",
        source: io::Error::new(io::ErrorKind::InvalidInput, "path has no parent"),
    })?;
    ensure_directory(parent)
}

fn set_read_only(path: &Path) -> Result<(), DerivedAssetStoreError> {
    ensure_no_symlink_components(path, "set_read_only")?;
    let file = open_derived_asset_file_for_read_no_follow(path).map_err(|source| {
        DerivedAssetStoreError::Io {
            path: path.to_path_buf(),
            operation: "open_set_read_only",
            source,
        }
    })?;
    let mut permissions = file
        .metadata()
        .map_err(|source| DerivedAssetStoreError::Io {
            path: path.to_path_buf(),
            operation: "metadata_permissions",
            source,
        })?
        .permissions();
    permissions.set_readonly(true);
    #[cfg(unix)]
    permissions.set_mode(0o400);
    file.set_permissions(permissions)
        .map_err(|source| DerivedAssetStoreError::Io {
            path: path.to_path_buf(),
            operation: "set_read_only",
            source,
        })
}

fn ensure_no_symlink_components(
    path: &Path,
    operation: &'static str,
) -> Result<(), DerivedAssetStoreError> {
    if let Some(symlink_path) =
        first_existing_symlink_component(path).map_err(|source| DerivedAssetStoreError::Io {
            path: path.to_path_buf(),
            operation,
            source,
        })?
    {
        return Err(DerivedAssetStoreError::Io {
            path: path.to_path_buf(),
            operation,
            source: io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "derived asset store path traverses symbolic link {}",
                    symlink_path.display()
                ),
            ),
        });
    }
    Ok(())
}

fn first_existing_symlink_component(path: &Path) -> io::Result<Option<PathBuf>> {
    super::path_safety::first_existing_symlink_component(path)
}

fn count_ref_files(refs_dir: &Path) -> Result<usize, DerivedAssetStoreError> {
    match fs::read_dir(refs_dir) {
        Ok(entries) => {
            let mut count = 0usize;
            for entry in entries {
                let entry = entry.map_err(|source| DerivedAssetStoreError::Io {
                    path: refs_dir.to_path_buf(),
                    operation: "read_ref_entry",
                    source,
                })?;
                if entry
                    .file_type()
                    .map_err(|source| DerivedAssetStoreError::Io {
                        path: entry.path(),
                        operation: "ref_file_type",
                        source,
                    })?
                    .is_file()
                {
                    count = count.saturating_add(1);
                }
            }
            Ok(count)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(0),
        Err(source) => Err(DerivedAssetStoreError::Io {
            path: refs_dir.to_path_buf(),
            operation: "read_refs",
            source,
        }),
    }
}

fn sorted_directories(path: &Path) -> io::Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            paths.push(entry.path());
        }
    }
    paths.sort();
    Ok(paths)
}

fn workspace_ref_file_stem(workspace_fingerprint: &str) -> String {
    blake3::hash(workspace_fingerprint.as_bytes())
        .to_hex()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::{
        DERIVED_ASSET_BODY_MAX_BYTES, DERIVED_ASSET_HASH_MISMATCH_CODE,
        DERIVED_ASSET_MANIFEST_MAX_BYTES, DERIVED_ASSET_REF_SCHEMA_V1,
        DERIVED_ASSET_SCHEMA_MISMATCH_CODE, DerivedAssetDescriptor, DerivedAssetObjectManifest,
        DerivedAssetStore, DerivedAssetStoreError, blake3_body_hash,
        default_derived_asset_store_root_from_env, first_existing_symlink_component,
    };
    use serde_json::Value;
    use std::collections::BTreeMap;
    use std::ffi::OsString;
    use std::fs;
    use std::path::PathBuf;

    type TestResult = Result<(), String>;

    #[test]
    fn derived_asset_keys_are_deterministic_and_include_all_identity_parts() -> TestResult {
        let body_hash = blake3_body_hash(b"graph-snapshot");
        let base_descriptor = descriptor(
            "graph_snapshot",
            "schema-a",
            "source-a",
            "config-a",
            "binary-a",
            &body_hash,
        );
        let same = descriptor(
            "graph_snapshot",
            "schema-a",
            "source-a",
            "config-a",
            "binary-a",
            &body_hash,
        );
        let different_config = descriptor(
            "graph_snapshot",
            "schema-a",
            "source-a",
            "config-b",
            "binary-a",
            &body_hash,
        );

        ensure_equal(&base_descriptor.key(), &same.key(), "same descriptor key")?;
        ensure_not_equal(
            &base_descriptor.key(),
            &different_config.key(),
            "config hash participates in key",
        )?;
        ensure(
            base_descriptor.key().starts_with("da_"),
            "key carries derived asset prefix",
        )?;
        ensure_equal(&base_descriptor.key().len(), &67usize, "key length")?;
        Ok(())
    }

    #[test]
    fn put_reuses_object_and_records_two_workspace_refs_without_path_leakage() -> TestResult {
        let root = unique_test_path("reuse-two-workspaces");
        let store = DerivedAssetStore::new(root.join("store"));
        let body = br#"{"schema":"ee.test.graph_snapshot.v1","nodes":2}"#;
        let descriptor = descriptor(
            "graph_snapshot",
            "ee.test.graph_snapshot.v1",
            "blake3:source-manifest",
            "blake3:config",
            "blake3:binary",
            &blake3_body_hash(body),
        );

        let first = store
            .put_bytes(&descriptor, "/tmp/workspace-a-with-private-path", body)
            .map_err(|error| error.to_string())?;
        let second = store
            .put_bytes(&descriptor, "/tmp/workspace-b-with-private-path", body)
            .map_err(|error| error.to_string())?;

        ensure(!first.reused_existing, "first write creates object")?;
        ensure(second.reused_existing, "second write reuses object")?;
        ensure_equal(&second.reference_count, &2usize, "two ref manifests")?;
        ensure_equal(
            &first.object_path,
            &second.object_path,
            "shared object path",
        )?;

        let first_ref = fs::read_to_string(first.reference_path)
            .map_err(|error| format!("read first ref: {error}"))?;
        ensure(
            !first_ref.contains("workspace-a-with-private-path"),
            "ref manifest stores only hashed workspace fingerprint",
        )?;
        ensure(
            first_ref.contains(DERIVED_ASSET_REF_SCHEMA_V1),
            "ref manifest records schema",
        )?;

        let summary = store.summary();
        ensure_equal(&summary.object_count, &1usize, "one object")?;
        ensure_equal(
            &summary.reusable_object_count,
            &1usize,
            "one reusable object",
        )?;
        ensure_equal(&summary.reference_count, &2usize, "summary refcount")?;
        ensure_equal(&summary.invalid_object_count, &0usize, "no invalid objects")?;
        ensure(
            summary.cleanup_candidates.is_empty(),
            "referenced object has no cleanup proposal",
        )?;
        let rendered = summary.data_json();
        let automatic_deletion = rendered.pointer("/cleanup/automaticDeletion");
        ensure_equal(
            &automatic_deletion,
            &Some(&Value::Bool(false)),
            "cleanup is proposal-only",
        )?;
        Ok(())
    }

    #[test]
    fn attach_is_read_only_and_preserves_body_hash() -> TestResult {
        let root = unique_test_path("attach-read-only");
        let store = DerivedAssetStore::new(root.join("store"));
        let body = b"pack-cache-entry";
        let descriptor = descriptor(
            "pack_cache_entry",
            "ee.pack.cache.test.v1",
            "blake3:source",
            "blake3:config",
            "blake3:binary",
            &blake3_body_hash(body),
        );
        store
            .put_bytes(&descriptor, "workspace-a", body)
            .map_err(|error| error.to_string())?;
        let destination = root.join("workspace-b").join("attached.bin");

        let attached = store
            .attach_read_only(&descriptor, &destination)
            .map_err(|error| error.to_string())?;

        ensure_equal(&attached.key, &descriptor.key(), "attached key")?;
        ensure_equal(
            &blake3_body_hash(&fs::read(&destination).map_err(|error| error.to_string())?),
            &descriptor.body_hash,
            "attached body hash",
        )?;
        ensure(
            fs::metadata(&destination)
                .map_err(|error| error.to_string())?
                .permissions()
                .readonly(),
            "attached file is read-only",
        )?;
        ensure(
            matches!(attached.mode.as_str(), "hard_link" | "copy"),
            "attach mode records hardlink or copy",
        )?;
        Ok(())
    }

    #[test]
    fn hash_mismatch_fails_closed_before_store_mutation() -> TestResult {
        let root = unique_test_path("hash-mismatch");
        let store = DerivedAssetStore::new(root.join("store"));
        let descriptor = descriptor(
            "algorithm_witness",
            "ee.test.witness.v1",
            "blake3:source",
            "blake3:config",
            "blake3:binary",
            "blake3:wrong",
        );

        let error = match store.put_bytes(&descriptor, "workspace-a", b"real witness") {
            Ok(outcome) => {
                return Err(format!(
                    "hash mismatch should reject object, got outcome {outcome:?}"
                ));
            }
            Err(error) => error,
        };

        ensure_equal(
            &error.code(),
            &Some(DERIVED_ASSET_HASH_MISMATCH_CODE),
            "hash mismatch code",
        )?;
        ensure(
            !store.root().join("objects").exists(),
            "incoming hash mismatch does not create object directory",
        )?;
        Ok(())
    }

    #[test]
    fn schema_mismatch_fails_closed_without_attach() -> TestResult {
        let root = unique_test_path("schema-mismatch");
        let store = DerivedAssetStore::new(root.join("store"));
        let body = b"restore-derived-asset";
        let descriptor = descriptor(
            "backup_restore_derived_asset",
            "ee.backup.derived.test.v1",
            "blake3:source",
            "blake3:config",
            "blake3:binary",
            &blake3_body_hash(body),
        );
        let key = descriptor.key();
        let object_dir = store.root().join("objects").join(&key);
        fs::create_dir_all(&object_dir).map_err(|error| error.to_string())?;
        fs::write(object_dir.join("body.bin"), body).map_err(|error| error.to_string())?;
        let mut manifest = DerivedAssetObjectManifest::new(&key, &descriptor, body.len() as u64);
        manifest.schema = "ee.derived_asset_store.object.v0".to_owned();
        let manifest_bytes = serde_json::to_vec_pretty(&manifest)
            .map_err(|error| format!("serialize manifest: {error}"))?;
        fs::write(object_dir.join("metadata.json"), manifest_bytes)
            .map_err(|error| error.to_string())?;

        let error = match store
            .attach_read_only(&descriptor, &root.join("workspace-b").join("asset.bin"))
        {
            Ok(outcome) => {
                return Err(format!(
                    "schema mismatch should block attach, got outcome {outcome:?}"
                ));
            }
            Err(error) => error,
        };

        ensure_equal(
            &error.code(),
            &Some(DERIVED_ASSET_SCHEMA_MISMATCH_CODE),
            "schema mismatch code",
        )?;
        ensure(
            matches!(error, DerivedAssetStoreError::SchemaMismatch { .. }),
            "schema mismatch variant",
        )?;
        Ok(())
    }

    #[test]
    fn validate_object_rejects_oversized_body_before_hashing() -> TestResult {
        let root = unique_test_path("oversized-body");
        let store = DerivedAssetStore::new(root.join("store"));
        let body = b"small-body";
        let descriptor = descriptor(
            "graph_snapshot",
            "ee.test.graph_snapshot.v1",
            "blake3:source",
            "blake3:config",
            "blake3:binary",
            &blake3_body_hash(body),
        );
        let key = descriptor.key();
        let object_dir = store.root().join("objects").join(&key);
        fs::create_dir_all(&object_dir).map_err(|error| error.to_string())?;
        let body_path = object_dir.join("body.bin");
        fs::File::create(&body_path)
            .and_then(|file| file.set_len(DERIVED_ASSET_BODY_MAX_BYTES.saturating_add(1)))
            .map_err(|error| error.to_string())?;
        let manifest = DerivedAssetObjectManifest::new(&key, &descriptor, body.len() as u64);
        let manifest_bytes = serde_json::to_vec_pretty(&manifest)
            .map_err(|error| format!("serialize manifest: {error}"))?;
        fs::write(object_dir.join("metadata.json"), manifest_bytes)
            .map_err(|error| error.to_string())?;

        let error = match store.validate_object(&descriptor) {
            Ok(()) => return Err("oversized body should fail validation".to_owned()),
            Err(error) => error,
        };

        ensure(
            error.to_string().contains("exceeds"),
            "oversized body error cites cap",
        )
    }

    #[test]
    fn validate_object_rejects_oversized_manifest_before_parse() -> TestResult {
        let root = unique_test_path("oversized-manifest");
        let store = DerivedAssetStore::new(root.join("store"));
        let body = b"small-body";
        let descriptor = descriptor(
            "graph_snapshot",
            "ee.test.graph_snapshot.v1",
            "blake3:source",
            "blake3:config",
            "blake3:binary",
            &blake3_body_hash(body),
        );
        let key = descriptor.key();
        let object_dir = store.root().join("objects").join(&key);
        fs::create_dir_all(&object_dir).map_err(|error| error.to_string())?;
        fs::write(object_dir.join("body.bin"), body).map_err(|error| error.to_string())?;
        fs::File::create(object_dir.join("metadata.json"))
            .and_then(|file| file.set_len(DERIVED_ASSET_MANIFEST_MAX_BYTES.saturating_add(1)))
            .map_err(|error| error.to_string())?;

        let error = match store.validate_object(&descriptor) {
            Ok(()) => return Err("oversized manifest should fail validation".to_owned()),
            Err(error) => error,
        };

        ensure(
            error.to_string().contains("exceeds"),
            "oversized manifest error cites cap",
        )
    }

    #[cfg(unix)]
    #[test]
    fn validate_object_rejects_symlinked_body() -> TestResult {
        use std::os::unix::fs as unix_fs;

        let root = unique_test_path("symlinked-body");
        let store = DerivedAssetStore::new(root.join("store"));
        let body = b"small-body";
        let descriptor = descriptor(
            "graph_snapshot",
            "ee.test.graph_snapshot.v1",
            "blake3:source",
            "blake3:config",
            "blake3:binary",
            &blake3_body_hash(body),
        );
        let key = descriptor.key();
        let object_dir = store.root().join("objects").join(&key);
        fs::create_dir_all(&object_dir).map_err(|error| error.to_string())?;
        let outside = root.join("outside-body.bin");
        fs::write(&outside, body).map_err(|error| error.to_string())?;
        unix_fs::symlink(&outside, object_dir.join("body.bin"))
            .map_err(|error| error.to_string())?;
        let manifest = DerivedAssetObjectManifest::new(&key, &descriptor, body.len() as u64);
        let manifest_bytes = serde_json::to_vec_pretty(&manifest)
            .map_err(|error| format!("serialize manifest: {error}"))?;
        fs::write(object_dir.join("metadata.json"), manifest_bytes)
            .map_err(|error| error.to_string())?;

        let error = match store.validate_object(&descriptor) {
            Ok(()) => return Err("symlinked body should fail validation".to_owned()),
            Err(error) => error,
        };

        ensure(
            error.to_string().contains("symbolic link"),
            "symlinked body error cites symlink",
        )
    }

    #[test]
    fn derived_asset_symlink_scan_stops_at_non_directory_tail() -> TestResult {
        let root = tempfile::tempdir().map_err(|error| error.to_string())?;
        let file_path = root.path().join("not-a-directory");
        fs::write(&file_path, b"file").map_err(|error| error.to_string())?;
        let child_path = file_path.join("child").join("asset.bin");

        let symlink = first_existing_symlink_component(&child_path)
            .map_err(|error| format!("symlink scan should stop at non-directory tail: {error}"))?;

        ensure_equal(
            &symlink,
            &None,
            "non-directory tail reports no symlink before later filesystem operation fails",
        )
    }

    #[test]
    fn default_root_uses_platform_data_dir_without_project_config() -> TestResult {
        let mut env = BTreeMap::new();
        env.insert("XDG_DATA_HOME".to_owned(), OsString::from("/tmp/xdg-data"));
        let root =
            default_derived_asset_store_root_from_env(env).map_err(|error| error.to_string())?;
        ensure_equal(
            &root,
            &PathBuf::from("/tmp/xdg-data/ee/derived-assets"),
            "xdg derived asset root",
        )
    }

    fn descriptor(
        asset_kind: &str,
        schema_version: &str,
        source_manifest_hash: &str,
        config_hash: &str,
        binary_capability_hash: &str,
        body_hash: &str,
    ) -> DerivedAssetDescriptor {
        DerivedAssetDescriptor::new(
            asset_kind,
            schema_version,
            source_manifest_hash,
            config_hash,
            binary_capability_hash,
            body_hash,
        )
    }

    fn unique_test_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "ee-derived-asset-{name}-{}-{}",
            std::process::id(),
            uuid::Uuid::now_v7()
        ))
    }

    fn ensure(condition: bool, message: &str) -> TestResult {
        if condition {
            Ok(())
        } else {
            Err(message.to_owned())
        }
    }

    fn ensure_equal<T>(actual: &T, expected: &T, label: &str) -> TestResult
    where
        T: std::fmt::Debug + PartialEq,
    {
        if actual == expected {
            Ok(())
        } else {
            Err(format!("{label}: expected {expected:?}, got {actual:?}"))
        }
    }

    fn ensure_not_equal<T>(left: &T, right: &T, label: &str) -> TestResult
    where
        T: std::fmt::Debug + PartialEq,
    {
        if left != right {
            Ok(())
        } else {
            Err(format!("{label}: values unexpectedly equal: {left:?}"))
        }
    }
}
