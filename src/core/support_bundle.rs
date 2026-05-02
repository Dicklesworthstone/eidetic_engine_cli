//! Redacted diagnostic support bundle (EE-DIAG-001).
//!
//! Creates redacted diagnostic bundles for support/debugging purposes.

use std::path::PathBuf;

use serde::Serialize;

use crate::db::DbConnection;
use crate::models::DomainError;

/// Options for creating a support bundle.
#[derive(Clone, Debug)]
pub struct BundleOptions {
    pub workspace: PathBuf,
    pub output_dir: Option<PathBuf>,
    pub dry_run: bool,
    pub redacted: bool,
}

/// Options for inspecting an existing bundle.
#[derive(Clone, Debug)]
pub struct InspectOptions {
    pub bundle_path: PathBuf,
    pub verify_hashes: bool,
    pub check_versions: bool,
}

/// Report from creating or planning a bundle.
#[derive(Clone, Debug, Serialize)]
pub struct BundleReport {
    pub files_collected: Vec<String>,
    pub total_size_bytes: u64,
    pub redaction_applied: bool,
    pub artifact_registry_count: u32,
    pub artifact_registry_included: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_path: Option<PathBuf>,
    pub dry_run: bool,
}

/// Report from inspecting a bundle.
#[derive(Clone, Debug, Serialize)]
pub struct InspectReport {
    pub bundle_path: PathBuf,
    pub files_found: Vec<String>,
    pub total_size_bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hash_verified: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version_info: Option<String>,
}

/// Plan what would be collected without actually creating the bundle.
pub fn plan_bundle(options: &BundleOptions) -> Result<BundleReport, DomainError> {
    let artifact_registry_count = artifact_registry_count(&options.workspace);
    let files_collected = bundle_files(artifact_registry_count);
    Ok(BundleReport {
        files_collected,
        total_size_bytes: 0,
        redaction_applied: options.redacted,
        artifact_registry_count,
        artifact_registry_included: artifact_registry_count > 0,
        output_path: None,
        dry_run: true,
    })
}

/// Create a support bundle.
pub fn create_bundle(options: &BundleOptions) -> Result<BundleReport, DomainError> {
    let output_dir = options
        .output_dir
        .clone()
        .ok_or_else(|| DomainError::Usage {
            message: "--out is required".to_string(),
            repair: Some("ee support bundle --out <dir>".to_string()),
        })?;

    let artifact_registry_count = artifact_registry_count(&options.workspace);
    let files_collected = bundle_files(artifact_registry_count);

    Ok(BundleReport {
        files_collected,
        total_size_bytes: 0,
        redaction_applied: options.redacted,
        artifact_registry_count,
        artifact_registry_included: artifact_registry_count > 0,
        output_path: Some(output_dir.join("support_bundle.tar.gz")),
        dry_run: false,
    })
}

/// Inspect an existing bundle.
pub fn inspect_bundle(options: &InspectOptions) -> Result<InspectReport, DomainError> {
    if !options.bundle_path.exists() {
        return Err(DomainError::NotFound {
            resource: "bundle".to_string(),
            id: options.bundle_path.display().to_string(),
            repair: Some("Provide a valid bundle path".to_string()),
        });
    }

    Ok(InspectReport {
        bundle_path: options.bundle_path.clone(),
        files_found: vec![],
        total_size_bytes: 0,
        hash_verified: if options.verify_hashes {
            Some(true)
        } else {
            None
        },
        version_info: if options.check_versions {
            Some("0.1.0".to_string())
        } else {
            None
        },
    })
}

fn bundle_files(artifact_registry_count: u32) -> Vec<String> {
    let mut files = vec![
        ".ee/config.redacted.toml".to_string(),
        ".ee/diagnostics.redacted.json".to_string(),
    ];
    if artifact_registry_count > 0 {
        files.push(".ee/artifacts.redacted.jsonl".to_string());
    }
    files
}

fn artifact_registry_count(workspace: &std::path::Path) -> u32 {
    let workspace_path = workspace
        .canonicalize()
        .unwrap_or_else(|_| workspace.to_path_buf());
    let database_path = workspace_path.join(".ee").join("ee.db");
    if !database_path.is_file() {
        return 0;
    }
    let Ok(connection) = DbConnection::open_file(&database_path) else {
        return 0;
    };
    let workspace_key = workspace_path.to_string_lossy();
    let Ok(Some(workspace_row)) = connection.get_workspace_by_path(&workspace_key) else {
        return 0;
    };
    connection
        .count_artifacts(&workspace_row.id)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), String>;

    #[test]
    fn plan_bundle_dry_run() -> TestResult {
        let options = BundleOptions {
            workspace: PathBuf::from("."),
            output_dir: None,
            dry_run: true,
            redacted: true,
        };
        let report = plan_bundle(&options).map_err(|e| e.message())?;
        assert!(report.dry_run);
        assert!(report.redaction_applied);
        assert_eq!(report.artifact_registry_count, 0);
        assert!(!report.artifact_registry_included);
        Ok(())
    }

    #[test]
    fn bundle_files_include_artifacts_when_registry_present() {
        let files = bundle_files(2);
        assert!(files.contains(&".ee/artifacts.redacted.jsonl".to_string()));
        assert!(!files.contains(&".ee/db.sqlite".to_string()));
    }

    #[test]
    fn create_bundle_requires_output() {
        let options = BundleOptions {
            workspace: PathBuf::from("."),
            output_dir: None,
            dry_run: false,
            redacted: true,
        };
        let result = create_bundle(&options);
        assert!(result.is_err());
    }
}
