//! Narrow coding-artifact registry.
//!
//! The registry stores metadata, hashes, redaction posture, provenance, and
//! optional safe snippets. Raw artifact bytes stay external unless a later
//! design explicitly adds import/copy semantics.

use std::path::{Component, Path, PathBuf};
use std::{fs, str::FromStr};

use chrono::{SecondsFormat, Utc};

use crate::db::{
    CreateArtifactInput, CreateArtifactLinkInput, CreateAuditInput, CreateWorkspaceInput,
    DbConnection, StoredArtifact, StoredArtifactLink, audit_actions, generate_audit_id,
};
use crate::models::{DomainError, ProvenanceUri, WorkspaceId};

pub const ARTIFACT_REGISTER_SCHEMA_V1: &str = "ee.artifact.register.v1";
pub const ARTIFACT_INSPECT_SCHEMA_V1: &str = "ee.artifact.inspect.v1";
pub const ARTIFACT_LIST_SCHEMA_V1: &str = "ee.artifact.list.v1";
const DEFAULT_DB_FILE: &str = "ee.db";
const DEFAULT_MAX_ARTIFACT_BYTES: u64 = 1_048_576;
const DEFAULT_SNIPPET_CHARS: usize = 2_048;
const REDACTED_SNIPPET: &str = "[REDACTED]";

const SECRET_PATTERNS: &[&str] = &[
    "password",
    "secret",
    "api_key",
    "apikey",
    "api-key",
    "token",
    "bearer",
    "authorization",
    "credential",
    "private_key",
    "access_key",
    "secret_key",
    "database_url",
    "connection_string",
    "-----begin",
];

/// Options for `ee artifact register`.
#[derive(Clone, Debug)]
pub struct ArtifactRegisterOptions<'a> {
    pub workspace_path: &'a Path,
    pub database_path: Option<&'a Path>,
    pub path: Option<&'a Path>,
    pub external_ref: Option<&'a str>,
    pub content_hash: Option<&'a str>,
    pub size_bytes: Option<u64>,
    pub artifact_type: &'a str,
    pub title: Option<&'a str>,
    pub provenance_uri: Option<&'a str>,
    pub max_bytes: Option<u64>,
    pub snippet_chars: Option<usize>,
    pub dry_run: bool,
    pub memory_links: Vec<String>,
    pub pack_links: Vec<String>,
}

/// Options for `ee artifact inspect`.
#[derive(Clone, Debug)]
pub struct ArtifactInspectOptions<'a> {
    pub workspace_path: &'a Path,
    pub database_path: Option<&'a Path>,
    pub artifact_id: &'a str,
}

/// Options for `ee artifact list`.
#[derive(Clone, Debug)]
pub struct ArtifactListOptions<'a> {
    pub workspace_path: &'a Path,
    pub database_path: Option<&'a Path>,
    pub limit: Option<u32>,
}

/// Stable non-fatal artifact diagnostic.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArtifactDegradation {
    pub code: String,
    pub severity: String,
    pub message: String,
    pub repair: String,
}

impl ArtifactDegradation {
    fn new(
        code: impl Into<String>,
        severity: impl Into<String>,
        message: impl Into<String>,
        repair: impl Into<String>,
    ) -> Self {
        Self {
            code: code.into(),
            severity: severity.into(),
            message: message.into(),
            repair: repair.into(),
        }
    }

    fn data_json(&self) -> serde_json::Value {
        serde_json::json!({
            "code": self.code,
            "severity": self.severity,
            "message": self.message,
            "repair": self.repair,
        })
    }
}

/// Artifact row plus links in public command shape.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArtifactView {
    pub artifact: StoredArtifact,
    pub links: Vec<StoredArtifactLink>,
}

impl ArtifactView {
    fn data_json(&self) -> serde_json::Value {
        serde_json::json!({
            "id": self.artifact.id,
            "workspaceId": self.artifact.workspace_id,
            "sourceKind": self.artifact.source_kind,
            "artifactType": self.artifact.artifact_type,
            "originalPath": self.artifact.original_path,
            "canonicalPath": self.artifact.canonical_path,
            "externalRef": self.artifact.external_ref,
            "contentHash": self.artifact.content_hash,
            "mediaType": self.artifact.media_type,
            "sizeBytes": self.artifact.size_bytes,
            "redactionStatus": self.artifact.redaction_status,
            "snippet": self.artifact.snippet,
            "snippetHash": self.artifact.snippet_hash,
            "provenanceUri": self.artifact.provenance_uri,
            "metadata": serde_json::from_str::<serde_json::Value>(&self.artifact.metadata_json)
                .unwrap_or_else(|_| serde_json::json!({})),
            "createdAt": self.artifact.created_at,
            "updatedAt": self.artifact.updated_at,
            "links": self.links.iter().map(link_json).collect::<Vec<_>>(),
        })
    }
}

/// Report returned by artifact registration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArtifactRegisterReport {
    pub schema: &'static str,
    pub status: String,
    pub dry_run: bool,
    pub persisted: bool,
    pub artifact: ArtifactView,
    pub audit_id: Option<String>,
    pub index_status: String,
    pub degraded: Vec<ArtifactDegradation>,
}

impl ArtifactRegisterReport {
    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        serde_json::json!({
            "schema": self.schema,
            "command": "artifact register",
            "status": self.status,
            "dryRun": self.dry_run,
            "persisted": self.persisted,
            "artifact": self.artifact.data_json(),
            "auditId": self.audit_id,
            "indexStatus": self.index_status,
            "degraded": self.degraded.iter().map(ArtifactDegradation::data_json).collect::<Vec<_>>(),
        })
    }

    #[must_use]
    pub fn human_summary(&self) -> String {
        let mode = if self.dry_run {
            "DRY RUN"
        } else {
            "REGISTERED"
        };
        let mut output = format!(
            "Artifact {mode}: {}\n  Type: {}\n  Hash: {}\n  Redaction: {}\n  Index: {}\n",
            self.artifact.artifact.id,
            self.artifact.artifact.artifact_type,
            self.artifact.artifact.content_hash,
            self.artifact.artifact.redaction_status,
            self.index_status
        );
        if let Some(path) = &self.artifact.artifact.original_path {
            output.push_str(&format!("  Path: {path}\n"));
        }
        if let Some(external_ref) = &self.artifact.artifact.external_ref {
            output.push_str(&format!("  External ref: {external_ref}\n"));
        }
        if !self.degraded.is_empty() {
            output.push_str("\nDegraded:\n");
            for degraded in &self.degraded {
                output.push_str(&format!("  - {}: {}\n", degraded.code, degraded.message));
            }
        }
        output
    }
}

/// Report returned by artifact inspection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArtifactInspectReport {
    pub schema: &'static str,
    pub artifact: Option<ArtifactView>,
}

impl ArtifactInspectReport {
    #[must_use]
    pub fn found(&self) -> bool {
        self.artifact.is_some()
    }

    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        serde_json::json!({
            "schema": self.schema,
            "command": "artifact inspect",
            "found": self.found(),
            "artifact": self.artifact.as_ref().map(ArtifactView::data_json),
        })
    }

    #[must_use]
    pub fn human_summary(&self) -> String {
        match &self.artifact {
            Some(view) => format!(
                "Artifact {}\n  Type: {}\n  Hash: {}\n  Redaction: {}\n",
                view.artifact.id,
                view.artifact.artifact_type,
                view.artifact.content_hash,
                view.artifact.redaction_status
            ),
            None => "Artifact not found\n".to_string(),
        }
    }
}

/// Report returned by artifact listing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArtifactListReport {
    pub schema: &'static str,
    pub workspace_id: String,
    pub artifacts: Vec<ArtifactView>,
    pub total_count: u32,
}

impl ArtifactListReport {
    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        serde_json::json!({
            "schema": self.schema,
            "command": "artifact list",
            "workspaceId": self.workspace_id,
            "totalCount": self.total_count,
            "artifacts": self.artifacts.iter().map(ArtifactView::data_json).collect::<Vec<_>>(),
        })
    }

    #[must_use]
    pub fn human_summary(&self) -> String {
        let mut output = format!("Artifacts: {}\n", self.total_count);
        for view in &self.artifacts {
            output.push_str(&format!(
                "  {} [{}] {}\n",
                view.artifact.id, view.artifact.redaction_status, view.artifact.content_hash
            ));
        }
        output
    }
}

#[derive(Clone, Debug)]
struct PreparedArtifact {
    artifact_id: String,
    workspace_id: String,
    workspace_path: PathBuf,
    database_path: PathBuf,
    input: CreateArtifactInput,
    links: Vec<CreateArtifactLinkInput>,
    degraded: Vec<ArtifactDegradation>,
}

/// Register a file or external artifact reference.
pub fn register_artifact(
    options: &ArtifactRegisterOptions<'_>,
) -> Result<ArtifactRegisterReport, DomainError> {
    let prepared = prepare_artifact(options)?;
    if options.dry_run {
        let view = ArtifactView {
            artifact: prepared.to_stored_preview(),
            links: prepared.to_stored_link_previews(),
        };
        return Ok(ArtifactRegisterReport {
            schema: ARTIFACT_REGISTER_SCHEMA_V1,
            status: status_for(options.dry_run, &prepared.degraded),
            dry_run: true,
            persisted: false,
            artifact: view,
            audit_id: None,
            index_status: "dry_run_not_indexed".to_string(),
            degraded: prepared.degraded,
        });
    }

    ensure_database_parent_exists(&prepared.database_path)?;
    let connection =
        DbConnection::open_file(&prepared.database_path).map_err(|error| DomainError::Storage {
            message: format!("Failed to open database: {error}"),
            repair: Some("ee doctor".to_string()),
        })?;
    connection.migrate().map_err(|error| DomainError::Storage {
        message: format!("Failed to migrate database: {error}"),
        repair: Some("ee doctor".to_string()),
    })?;
    ensure_workspace(
        &connection,
        &prepared.workspace_id,
        &prepared.workspace_path,
    )?;

    let audit_id = generate_audit_id();
    let audit_details = artifact_audit_details(&prepared.artifact_id, &prepared.input);
    connection
        .with_transaction(|| {
            connection.upsert_artifact(&prepared.artifact_id, &prepared.input)?;
            for link in &prepared.links {
                connection.insert_artifact_link(link)?;
            }
            connection.insert_audit(
                &audit_id,
                &CreateAuditInput {
                    workspace_id: Some(prepared.workspace_id.clone()),
                    actor: Some("ee artifact register".to_string()),
                    action: audit_actions::ARTIFACT_REGISTER.to_string(),
                    target_type: Some("artifact".to_string()),
                    target_id: Some(prepared.artifact_id.clone()),
                    details: Some(audit_details.clone()),
                },
            )
        })
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to register artifact: {error}"),
            repair: Some("ee doctor".to_string()),
        })?;

    let artifact = connection
        .get_artifact(&prepared.artifact_id)
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to reload artifact: {error}"),
            repair: Some("ee artifact inspect <artifact-id> --json".to_string()),
        })?
        .ok_or_else(|| DomainError::Storage {
            message: format!(
                "Artifact {} was not found after registration.",
                prepared.artifact_id
            ),
            repair: Some("ee doctor".to_string()),
        })?;
    let links = connection
        .list_artifact_links(&artifact.id)
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to load artifact links: {error}"),
            repair: Some("ee artifact inspect <artifact-id> --json".to_string()),
        })?;

    Ok(ArtifactRegisterReport {
        schema: ARTIFACT_REGISTER_SCHEMA_V1,
        status: status_for(false, &prepared.degraded),
        dry_run: false,
        persisted: true,
        artifact: ArtifactView { artifact, links },
        audit_id: Some(audit_id),
        index_status: "rebuild_required".to_string(),
        degraded: prepared.degraded,
    })
}

/// Inspect one artifact by ID.
pub fn inspect_artifact(
    options: &ArtifactInspectOptions<'_>,
) -> Result<ArtifactInspectReport, DomainError> {
    let database_path =
        resolved_database_path(options.workspace_path, options.database_path, false)?;
    let connection =
        DbConnection::open_file(&database_path).map_err(|error| DomainError::Storage {
            message: format!("Failed to open database: {error}"),
            repair: Some("ee init --workspace .".to_string()),
        })?;
    let artifact = connection
        .get_artifact(options.artifact_id)
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to query artifact: {error}"),
            repair: Some("ee doctor".to_string()),
        })?;
    let artifact = match artifact {
        Some(artifact) => {
            let links = connection
                .list_artifact_links(&artifact.id)
                .map_err(|error| DomainError::Storage {
                    message: format!("Failed to query artifact links: {error}"),
                    repair: Some("ee doctor".to_string()),
                })?;
            Some(ArtifactView { artifact, links })
        }
        None => None,
    };

    Ok(ArtifactInspectReport {
        schema: ARTIFACT_INSPECT_SCHEMA_V1,
        artifact,
    })
}

/// List artifacts for the selected workspace.
pub fn list_artifacts(
    options: &ArtifactListOptions<'_>,
) -> Result<ArtifactListReport, DomainError> {
    let workspace_path = resolve_workspace_path(options.workspace_path, false)?;
    let database_path = resolved_database_path(&workspace_path, options.database_path, false)?;
    let workspace_id = stable_workspace_id(&workspace_path);
    let connection =
        DbConnection::open_file(&database_path).map_err(|error| DomainError::Storage {
            message: format!("Failed to open database: {error}"),
            repair: Some("ee init --workspace .".to_string()),
        })?;
    let workspace_id = connection
        .get_workspace_by_path(&workspace_path.to_string_lossy())
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to query workspace: {error}"),
            repair: Some("ee doctor".to_string()),
        })?
        .map_or(workspace_id, |workspace| workspace.id);
    let artifacts = connection
        .list_artifacts(&workspace_id, options.limit)
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to list artifacts: {error}"),
            repair: Some("ee doctor".to_string()),
        })?;
    let total_count =
        connection
            .count_artifacts(&workspace_id)
            .map_err(|error| DomainError::Storage {
                message: format!("Failed to count artifacts: {error}"),
                repair: Some("ee doctor".to_string()),
            })?;
    let mut views = Vec::with_capacity(artifacts.len());
    for artifact in artifacts {
        let links = connection
            .list_artifact_links(&artifact.id)
            .map_err(|error| DomainError::Storage {
                message: format!("Failed to query artifact links: {error}"),
                repair: Some("ee doctor".to_string()),
            })?;
        views.push(ArtifactView { artifact, links });
    }

    Ok(ArtifactListReport {
        schema: ARTIFACT_LIST_SCHEMA_V1,
        workspace_id,
        artifacts: views,
        total_count,
    })
}

impl PreparedArtifact {
    fn to_stored_preview(&self) -> StoredArtifact {
        let now = current_timestamp();
        StoredArtifact {
            id: self.artifact_id.clone(),
            workspace_id: self.workspace_id.clone(),
            source_kind: self.input.source_kind.clone(),
            artifact_type: self.input.artifact_type.clone(),
            original_path: self.input.original_path.clone(),
            canonical_path: self.input.canonical_path.clone(),
            external_ref: self.input.external_ref.clone(),
            content_hash: self.input.content_hash.clone(),
            media_type: self.input.media_type.clone(),
            size_bytes: self.input.size_bytes,
            redaction_status: self.input.redaction_status.clone(),
            snippet: self.input.snippet.clone(),
            snippet_hash: self.input.snippet_hash.clone(),
            provenance_uri: self.input.provenance_uri.clone(),
            metadata_json: self
                .input
                .metadata_json
                .clone()
                .unwrap_or_else(|| "{}".to_string()),
            created_at: now.clone(),
            updated_at: now,
        }
    }

    fn to_stored_link_previews(&self) -> Vec<StoredArtifactLink> {
        let now = current_timestamp();
        self.links
            .iter()
            .map(|link| StoredArtifactLink {
                artifact_id: link.artifact_id.clone(),
                target_type: link.target_type.clone(),
                target_id: link.target_id.clone(),
                relation: link.relation.clone(),
                created_at: now.clone(),
                metadata_json: link.metadata_json.clone(),
            })
            .collect()
    }
}

fn prepare_artifact(
    options: &ArtifactRegisterOptions<'_>,
) -> Result<PreparedArtifact, DomainError> {
    let workspace_path = resolve_workspace_path(options.workspace_path, options.dry_run)?;
    let database_path =
        resolved_database_path(&workspace_path, options.database_path, options.dry_run)?;
    let workspace_id = stable_workspace_id(&workspace_path);
    let artifact_type = normalize_artifact_type(options.artifact_type)?;
    let mut degraded = Vec::new();

    let (
        source_kind,
        original_path,
        canonical_path,
        external_ref,
        content_hash,
        media_type,
        size_bytes,
        redaction_status,
        snippet,
        snippet_hash,
    ) = match (options.path, options.external_ref) {
        (Some(path), None) => prepare_file_artifact(
            &workspace_path,
            path,
            options.max_bytes.unwrap_or(DEFAULT_MAX_ARTIFACT_BYTES),
            options.snippet_chars.unwrap_or(DEFAULT_SNIPPET_CHARS),
            &mut degraded,
        )?,
        (None, Some(external_ref)) => {
            prepare_external_artifact(external_ref, options.content_hash, options.size_bytes)?
        }
        (Some(_), Some(_)) => {
            return Err(artifact_usage_error(
                "Pass either a local artifact path or --external-ref, not both.".to_string(),
            ));
        }
        (None, None) => {
            return Err(artifact_usage_error(
                "Artifact registration requires a path or --external-ref.".to_string(),
            ));
        }
    };

    let locator = canonical_path
        .as_deref()
        .or(external_ref.as_deref())
        .unwrap_or("");
    let artifact_id = stable_artifact_id(&workspace_id, &source_kind, locator, &content_hash);
    let provenance_uri = artifact_provenance_uri(
        options.provenance_uri,
        canonical_path.as_deref(),
        external_ref.as_deref(),
    )?;
    let metadata_json = artifact_metadata_json(options.title, snippet.as_deref(), &degraded)?;
    let links = prepare_links(&artifact_id, &options.memory_links, &options.pack_links)?;

    Ok(PreparedArtifact {
        artifact_id,
        workspace_id: workspace_id.clone(),
        workspace_path,
        database_path,
        input: CreateArtifactInput {
            workspace_id,
            source_kind,
            artifact_type,
            original_path,
            canonical_path,
            external_ref,
            content_hash,
            media_type,
            size_bytes,
            redaction_status,
            snippet,
            snippet_hash,
            provenance_uri,
            metadata_json: Some(metadata_json),
        },
        links,
        degraded,
    })
}

type PreparedArtifactFields = (
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    String,
    String,
    u64,
    String,
    Option<String>,
    Option<String>,
);

fn prepare_file_artifact(
    workspace_path: &Path,
    path: &Path,
    max_bytes: u64,
    snippet_chars: usize,
    degraded: &mut Vec<ArtifactDegradation>,
) -> Result<PreparedArtifactFields, DomainError> {
    if max_bytes == 0 {
        return Err(artifact_usage_error(
            "--max-bytes must be greater than zero.".to_string(),
        ));
    }
    if path_has_parent_dir(path) {
        return Err(DomainError::PolicyDenied {
            message: format!(
                "Artifact path contains parent-directory traversal: {}",
                path.display()
            ),
            repair: Some("Pass a path inside the workspace without `..` components.".to_string()),
        });
    }

    let candidate_path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        workspace_path.join(path)
    };
    let metadata =
        fs::symlink_metadata(&candidate_path).map_err(|error| DomainError::NotFound {
            resource: "artifact".to_string(),
            id: candidate_path.display().to_string(),
            repair: Some(format!("Create the file or pass a valid path: {error}")),
        })?;
    if !metadata.is_file() && !metadata.file_type().is_symlink() {
        return Err(DomainError::PolicyDenied {
            message: format!(
                "Artifact path is not a regular file: {}",
                candidate_path.display()
            ),
            repair: Some(
                "Register a regular file, log, command output, fixture, or bundle artifact."
                    .to_string(),
            ),
        });
    }

    let canonical_path = candidate_path
        .canonicalize()
        .map_err(|error| DomainError::Storage {
            message: format!(
                "Failed to resolve artifact path {}: {error}",
                candidate_path.display()
            ),
            repair: Some("Check artifact file permissions and symlink targets.".to_string()),
        })?;
    if !canonical_path.starts_with(workspace_path) {
        return Err(DomainError::PolicyDenied {
            message: format!(
                "Artifact path resolves outside the workspace: {}",
                candidate_path.display()
            ),
            repair: Some("Register only artifacts under the selected workspace.".to_string()),
        });
    }
    if metadata.file_type().is_symlink() {
        degraded.push(ArtifactDegradation::new(
            "artifact_symlink_resolved",
            "low",
            "Artifact path was a symlink that resolved inside the workspace.",
            "Prefer registering the canonical target path for reproducibility.",
        ));
    }

    let resolved_metadata =
        fs::metadata(&canonical_path).map_err(|error| DomainError::Storage {
            message: format!(
                "Failed to read artifact metadata {}: {error}",
                canonical_path.display()
            ),
            repair: Some("Check artifact file permissions and retry.".to_string()),
        })?;
    if resolved_metadata.len() > max_bytes {
        return Err(DomainError::PolicyDenied {
            message: format!(
                "Artifact is too large: {} bytes exceeds the {} byte limit.",
                resolved_metadata.len(),
                max_bytes
            ),
            repair: Some(
                "Register a smaller log/snippet artifact or raise --max-bytes intentionally."
                    .to_string(),
            ),
        });
    }

    let bytes = fs::read(&canonical_path).map_err(|error| DomainError::Storage {
        message: format!(
            "Failed to read artifact {}: {error}",
            canonical_path.display()
        ),
        repair: Some("Check artifact file permissions and retry.".to_string()),
    })?;
    let content_hash = blake3_hex(&bytes);
    let size_bytes = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    let text = if bytes.contains(&0) {
        None
    } else {
        std::str::from_utf8(&bytes).ok()
    };

    let (redaction_status, snippet, media_type) = match text {
        Some(text) => {
            if contains_secret_pattern(text) {
                degraded.push(ArtifactDegradation::new(
                    "artifact_secret_redacted",
                    "medium",
                    "Artifact text looked secret-bearing; only a redacted placeholder was stored.",
                    "Register a pre-redacted artifact if searchable snippets are needed.",
                ));
                (
                    "redacted".to_string(),
                    Some(REDACTED_SNIPPET.to_string()),
                    media_type_for_path(&canonical_path, true),
                )
            } else {
                (
                    "checked".to_string(),
                    safe_snippet(text, snippet_chars),
                    media_type_for_path(&canonical_path, true),
                )
            }
        }
        None => {
            degraded.push(ArtifactDegradation::new(
                "artifact_binary_not_indexed",
                "low",
                "Artifact content was binary or invalid UTF-8; metadata and hash were registered without a snippet.",
                "If text search is needed, register a UTF-8 log or summary artifact.",
            ));
            (
                "not_text".to_string(),
                None,
                media_type_for_path(&canonical_path, false),
            )
        }
    };
    let snippet_hash = snippet.as_ref().map(|value| blake3_hex(value.as_bytes()));

    Ok((
        "file".to_string(),
        Some(path.to_string_lossy().into_owned()),
        Some(canonical_path.to_string_lossy().into_owned()),
        None,
        content_hash,
        media_type,
        size_bytes,
        redaction_status,
        snippet,
        snippet_hash,
    ))
}

fn prepare_external_artifact(
    external_ref: &str,
    content_hash: Option<&str>,
    size_bytes: Option<u64>,
) -> Result<PreparedArtifactFields, DomainError> {
    let external_ref = external_ref.trim();
    if external_ref.is_empty() {
        return Err(artifact_usage_error(
            "--external-ref must be non-empty.".to_string(),
        ));
    }
    let _ = ProvenanceUri::from_str(external_ref).map_err(|error| {
        artifact_usage_error(format!(
            "--external-ref must be a supported provenance URI: {error}"
        ))
    })?;
    let content_hash = content_hash
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            artifact_usage_error("--content-hash is required with --external-ref.".to_string())
        })?;
    validate_blake3_hash(content_hash)?;
    let size_bytes = size_bytes.ok_or_else(|| {
        artifact_usage_error("--size-bytes is required with --external-ref.".to_string())
    })?;

    Ok((
        "external".to_string(),
        None,
        None,
        Some(external_ref.to_string()),
        content_hash.to_string(),
        "application/octet-stream".to_string(),
        size_bytes,
        "external_reference".to_string(),
        None,
        None,
    ))
}

fn prepare_links(
    artifact_id: &str,
    memory_links: &[String],
    pack_links: &[String],
) -> Result<Vec<CreateArtifactLinkInput>, DomainError> {
    let mut links = Vec::new();
    for memory_id in memory_links {
        let target_id = trim_required(memory_id, "--link-memory")?;
        links.push(CreateArtifactLinkInput {
            artifact_id: artifact_id.to_string(),
            target_type: "memory".to_string(),
            target_id,
            relation: "evidence_for".to_string(),
            metadata_json: None,
        });
    }
    for pack_id in pack_links {
        let target_id = trim_required(pack_id, "--link-pack")?;
        links.push(CreateArtifactLinkInput {
            artifact_id: artifact_id.to_string(),
            target_type: "pack".to_string(),
            target_id,
            relation: "included_by".to_string(),
            metadata_json: None,
        });
    }
    Ok(links)
}

fn trim_required(value: &str, field: &str) -> Result<String, DomainError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        Err(artifact_usage_error(format!("{field} must be non-empty.")))
    } else {
        Ok(trimmed.to_string())
    }
}

fn artifact_provenance_uri(
    raw: Option<&str>,
    canonical_path: Option<&str>,
    external_ref: Option<&str>,
) -> Result<Option<String>, DomainError> {
    let value = raw.or(external_ref).or(canonical_path);
    value
        .map(|value| {
            let candidate = if raw.is_none() && canonical_path.is_some() && !value.contains("://") {
                format!("file://{value}")
            } else {
                value.to_string()
            };
            ProvenanceUri::from_str(&candidate)
                .map(|uri| uri.to_string())
                .map_err(|error| artifact_usage_error(format!("invalid provenance URI: {error}")))
        })
        .transpose()
}

fn artifact_metadata_json(
    title: Option<&str>,
    snippet: Option<&str>,
    degraded: &[ArtifactDegradation],
) -> Result<String, DomainError> {
    let title = title.map(str::trim).filter(|value| !value.is_empty());
    serde_json::to_string(&serde_json::json!({
        "schema": "ee.artifact.metadata.v1",
        "title": title,
        "snippetPresent": snippet.is_some(),
        "degradationCodes": degraded.iter().map(|entry| entry.code.as_str()).collect::<Vec<_>>(),
    }))
    .map_err(|error| DomainError::Storage {
        message: format!("Failed to serialize artifact metadata: {error}"),
        repair: Some("ee artifact register --help".to_string()),
    })
}

fn artifact_audit_details(artifact_id: &str, input: &CreateArtifactInput) -> String {
    serde_json::json!({
        "schema": "ee.audit.artifact_register.v1",
        "command": "ee artifact register",
        "artifactId": artifact_id,
        "sourceKind": input.source_kind,
        "artifactType": input.artifact_type,
        "contentHash": input.content_hash,
        "redactionStatus": input.redaction_status,
        "sizeBytes": input.size_bytes,
        "provenanceUri": input.provenance_uri,
    })
    .to_string()
}

fn resolved_database_path(
    workspace_path: &Path,
    database_path: Option<&Path>,
    dry_run: bool,
) -> Result<PathBuf, DomainError> {
    let path = database_path
        .map(Path::to_path_buf)
        .unwrap_or_else(|| workspace_path.join(".ee").join(DEFAULT_DB_FILE));
    if dry_run || path.exists() {
        Ok(path)
    } else {
        Err(DomainError::Storage {
            message: format!("Database not found at {}", path.display()),
            repair: Some("ee init --workspace .".to_string()),
        })
    }
}

fn ensure_database_parent_exists(database_path: &Path) -> Result<(), DomainError> {
    let Some(parent) = database_path.parent() else {
        return Ok(());
    };
    if parent.exists() {
        return Ok(());
    }
    Err(DomainError::Storage {
        message: format!("Database directory not found at {}", parent.display()),
        repair: Some("ee init --workspace .".to_string()),
    })
}

fn ensure_workspace(
    connection: &DbConnection,
    workspace_id: &str,
    workspace_path: &Path,
) -> Result<(), DomainError> {
    let path = workspace_path.to_string_lossy().into_owned();
    if connection
        .get_workspace_by_path(&path)
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to query workspace: {error}"),
            repair: Some("ee doctor".to_string()),
        })?
        .is_some()
    {
        return Ok(());
    }

    connection
        .insert_workspace(
            workspace_id,
            &CreateWorkspaceInput {
                path,
                name: workspace_path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned()),
            },
        )
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to register workspace: {error}"),
            repair: Some("ee doctor".to_string()),
        })
}

fn resolve_workspace_path(path: &Path, dry_run: bool) -> Result<PathBuf, DomainError> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    };

    match absolute.canonicalize() {
        Ok(canonical) => Ok(canonical),
        Err(_error) if dry_run => Ok(absolute),
        Err(error) => Err(DomainError::Configuration {
            message: format!(
                "Failed to resolve workspace {}: {error}",
                absolute.display()
            ),
            repair: Some("ee init --workspace .".to_string()),
        }),
    }
}

fn stable_workspace_id(path: &Path) -> String {
    let hash = blake3::hash(format!("workspace:{}", path.to_string_lossy()).as_bytes());
    let mut bytes = [0_u8; 16];
    for (target, source) in bytes.iter_mut().zip(hash.as_bytes().iter().copied()) {
        *target = source;
    }
    WorkspaceId::from_uuid(uuid::Uuid::from_bytes(bytes)).to_string()
}

fn stable_artifact_id(
    workspace_id: &str,
    source_kind: &str,
    locator: &str,
    content_hash: &str,
) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"ee.artifact.id.v1\0");
    hasher.update(workspace_id.as_bytes());
    hasher.update(b"\0");
    hasher.update(source_kind.as_bytes());
    hasher.update(b"\0");
    hasher.update(locator.as_bytes());
    hasher.update(b"\0");
    hasher.update(content_hash.as_bytes());
    let digest = hasher.finalize().to_hex().to_string();
    format!("art_{}", &digest[..26])
}

fn current_timestamp() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn status_for(dry_run: bool, degraded: &[ArtifactDegradation]) -> String {
    match (dry_run, degraded.is_empty()) {
        (true, true) => "dry_run".to_string(),
        (true, false) => "dry_run_degraded".to_string(),
        (false, true) => "registered".to_string(),
        (false, false) => "registered_degraded".to_string(),
    }
}

fn path_has_parent_dir(path: &Path) -> bool {
    path.components()
        .any(|component| matches!(component, Component::ParentDir))
}

fn normalize_artifact_type(value: &str) -> Result<String, DomainError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(artifact_usage_error(
            "artifact type must be non-empty.".to_string(),
        ));
    }
    let mut output = String::new();
    for character in trimmed.chars() {
        let normalized = match character {
            'A'..='Z' => character.to_ascii_lowercase(),
            'a'..='z' | '0'..='9' | '_' | '-' | '.' => character,
            ' ' => '_',
            _ => {
                return Err(artifact_usage_error(format!(
                    "artifact type contains unsupported character `{character}`"
                )));
            }
        };
        output.push(normalized);
    }
    Ok(output)
}

fn validate_blake3_hash(value: &str) -> Result<(), DomainError> {
    let valid = value.len() == 71
        && value.starts_with("blake3:")
        && value["blake3:".len()..]
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase());
    if valid {
        Ok(())
    } else {
        Err(artifact_usage_error(
            "content hash must be `blake3:` followed by 64 lowercase hex characters.".to_string(),
        ))
    }
}

fn blake3_hex(bytes: &[u8]) -> String {
    format!("blake3:{}", blake3::hash(bytes).to_hex())
}

fn safe_snippet(text: &str, limit: usize) -> Option<String> {
    if limit == 0 {
        return None;
    }
    let snippet: String = text.chars().take(limit).collect();
    if snippet.is_empty() {
        None
    } else {
        Some(snippet)
    }
}

fn contains_secret_pattern(content: &str) -> bool {
    let lowered = content.to_ascii_lowercase();
    SECRET_PATTERNS
        .iter()
        .any(|pattern| lowered.contains(pattern))
}

fn media_type_for_path(path: &Path, is_text: bool) -> String {
    match path.extension().and_then(|value| value.to_str()) {
        Some("json") => "application/json",
        Some("jsonl") => "application/x-ndjson",
        Some("md") | Some("markdown") => "text/markdown",
        Some("toml") => "application/toml",
        Some("yaml" | "yml") => "application/yaml",
        Some("rs") => "text/rust",
        Some("txt" | "log") => "text/plain",
        _ if is_text => "text/plain",
        _ => "application/octet-stream",
    }
    .to_string()
}

fn link_json(link: &StoredArtifactLink) -> serde_json::Value {
    serde_json::json!({
        "artifactId": link.artifact_id,
        "targetType": link.target_type,
        "targetId": link.target_id,
        "relation": link.relation,
        "createdAt": link.created_at,
        "metadata": link.metadata_json.as_ref().and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok()),
    })
}

fn artifact_usage_error(message: String) -> DomainError {
    DomainError::Usage {
        message,
        repair: Some("ee artifact register --help".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), String>;

    fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
        if condition {
            Ok(())
        } else {
            Err(message.into())
        }
    }

    fn secret_fixture(parts: &[&str]) -> String {
        parts.concat()
    }

    fn secret_assignment(value: &str) -> String {
        format!("{}={value}", secret_fixture(&["api", "_", "key"]))
    }

    #[test]
    fn stable_artifact_id_is_deterministic_and_prefixed() -> TestResult {
        let id = stable_artifact_id(
            "wsp_01234567890123456789012345",
            "file",
            "/workspace/log.txt",
            "blake3:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        );
        ensure(id.starts_with("art_"), "artifact id prefix")?;
        ensure(id.len() == 30, "artifact id length")?;
        let again = stable_artifact_id(
            "wsp_01234567890123456789012345",
            "file",
            "/workspace/log.txt",
            "blake3:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        );
        ensure(id == again, "artifact id deterministic")
    }

    #[test]
    fn rejects_parent_directory_paths() -> TestResult {
        ensure(
            path_has_parent_dir(Path::new("../secret.txt")),
            "parent dir rejected",
        )
    }

    #[test]
    fn redacts_secret_snippet_without_storing_raw_text() -> TestResult {
        let dir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let workspace = dir.path();
        let ee_dir = workspace.join(".ee");
        std::fs::create_dir_all(&ee_dir).map_err(|error| error.to_string())?;
        let file = workspace.join("log.txt");
        let content = format!("{}\nnormal text", secret_assignment("redaction-fixture"));
        std::fs::write(&file, content).map_err(|error| error.to_string())?;
        let options = ArtifactRegisterOptions {
            workspace_path: workspace,
            database_path: Some(&ee_dir.join("ee.db")),
            path: Some(Path::new("log.txt")),
            external_ref: None,
            content_hash: None,
            size_bytes: None,
            artifact_type: "log",
            title: None,
            provenance_uri: None,
            max_bytes: Some(DEFAULT_MAX_ARTIFACT_BYTES),
            snippet_chars: Some(DEFAULT_SNIPPET_CHARS),
            dry_run: true,
            memory_links: Vec::new(),
            pack_links: Vec::new(),
        };
        let report = register_artifact(&options).map_err(|error| error.message())?;
        ensure(
            report.artifact.artifact.redaction_status == "redacted",
            "secret status redacted",
        )?;
        ensure(
            report.artifact.artifact.snippet.as_deref() == Some(REDACTED_SNIPPET),
            "secret snippet placeholder",
        )?;
        ensure(
            report
                .degraded
                .iter()
                .any(|entry| entry.code == "artifact_secret_redacted"),
            "secret degradation",
        )
    }

    #[test]
    fn binary_artifact_registers_metadata_without_snippet() -> TestResult {
        let dir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let workspace = dir.path();
        let ee_dir = workspace.join(".ee");
        std::fs::create_dir_all(&ee_dir).map_err(|error| error.to_string())?;
        let file = workspace.join("blob.bin");
        std::fs::write(&file, [0_u8, 159, 146, 150]).map_err(|error| error.to_string())?;
        let options = ArtifactRegisterOptions {
            workspace_path: workspace,
            database_path: Some(&ee_dir.join("ee.db")),
            path: Some(Path::new("blob.bin")),
            external_ref: None,
            content_hash: None,
            size_bytes: None,
            artifact_type: "file",
            title: None,
            provenance_uri: None,
            max_bytes: Some(DEFAULT_MAX_ARTIFACT_BYTES),
            snippet_chars: Some(DEFAULT_SNIPPET_CHARS),
            dry_run: true,
            memory_links: Vec::new(),
            pack_links: Vec::new(),
        };
        let report = register_artifact(&options).map_err(|error| error.message())?;
        ensure(
            report.artifact.artifact.redaction_status == "not_text",
            "binary status",
        )?;
        ensure(
            report.artifact.artifact.snippet.is_none(),
            "binary snippet omitted",
        )
    }
}
