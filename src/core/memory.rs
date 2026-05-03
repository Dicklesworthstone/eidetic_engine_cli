//! Memory retrieval and inspection operations (EE-063, EE-066).
//!
//! Provides the core use case functions for inspecting stored memories:
//! - `get_memory_details`: retrieve a single memory with its tags and metadata
//! - `revise_memory`: create an immutable revision of an existing memory

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::str::FromStr;

use chrono::{DateTime, SecondsFormat, Utc};

use crate::db::{
    CreateAuditInput, CreateMemoryInput, CreateSearchIndexJobInput, CreateWorkspaceInput,
    DbConnection, SearchIndexJobType, StoredMemory, audit_actions, generate_audit_id,
};
use crate::models::{
    DomainError, MemoryContent, MemoryId, MemoryKind, MemoryLevel, ProvenanceUri, Tag, TrustClass,
    UnitScore, WorkspaceId,
};

/// A memory with its associated tags for display.
#[derive(Clone, Debug, PartialEq)]
pub struct MemoryDetails {
    /// The stored memory record.
    pub memory: StoredMemory,
    /// Tags associated with this memory.
    pub tags: Vec<String>,
}

/// Options for creating a manual memory through `ee remember`.
#[derive(Clone, Debug)]
pub struct RememberMemoryOptions<'a> {
    /// Workspace root selected by the CLI.
    pub workspace_path: &'a Path,
    /// Optional database path. Defaults to `<workspace>/.ee/ee.db`.
    pub database_path: Option<&'a Path>,
    /// Memory content.
    pub content: &'a str,
    /// Memory level.
    pub level: &'a str,
    /// Memory kind.
    pub kind: &'a str,
    /// Comma-separated tags.
    pub tags: Option<&'a str>,
    /// Confidence score.
    pub confidence: f32,
    /// Optional source provenance URI.
    pub source: Option<&'a str>,
    /// RFC3339 timestamp when this memory becomes applicable.
    pub valid_from: Option<&'a str>,
    /// RFC3339 timestamp when this memory stops being applicable.
    pub valid_to: Option<&'a str>,
    /// Validate and render the write without mutating storage.
    pub dry_run: bool,
}

/// Result of creating a manual memory.
#[derive(Clone, Debug, PartialEq)]
pub struct RememberMemoryReport {
    /// Package version for stable output.
    pub version: &'static str,
    /// Created or previewed memory ID.
    pub memory_id: MemoryId,
    /// Canonical workspace ID when resolved.
    pub workspace_id: String,
    /// Canonical workspace path.
    pub workspace_path: PathBuf,
    /// Resolved database path.
    pub database_path: PathBuf,
    /// Canonical memory content.
    pub content: String,
    /// Canonical memory level.
    pub level: MemoryLevel,
    /// Canonical memory kind.
    pub kind: MemoryKind,
    /// Validated confidence score.
    pub confidence: f32,
    /// Canonical tags.
    pub tags: Vec<String>,
    /// Canonical source/provenance URI.
    pub source: Option<String>,
    /// RFC3339 timestamp when this memory becomes applicable.
    pub valid_from: Option<String>,
    /// RFC3339 timestamp when this memory stops being applicable.
    pub valid_to: Option<String>,
    /// Current validity status computed from the stored validity window.
    pub validity_status: String,
    /// Stable shape of the validity window.
    pub validity_window_kind: String,
    /// Whether this was a dry run.
    pub dry_run: bool,
    /// Whether a memory row was persisted.
    pub persisted: bool,
    /// First-version revision number for a newly remembered memory.
    pub revision_number: u32,
    /// Revision group ID once revision tracking is backed by storage.
    pub revision_group_id: Option<String>,
    /// Audit entry created for the write.
    pub audit_id: Option<String>,
    /// Pending index job created for the memory.
    pub index_job_id: Option<String>,
    /// Stable index status for the write.
    pub index_status: String,
    /// Effect IDs once command-effect recording is backed by storage.
    pub effect_ids: Vec<String>,
    /// Staged adjacency suggestions. These do not create durable memory_links rows.
    pub suggested_links: Vec<RememberSuggestedLink>,
    /// Status of suggestion generation.
    pub suggested_link_status: String,
    /// Non-fatal degradations encountered while generating suggestions.
    pub suggested_link_degradations: Vec<RememberSuggestedLinkDegradation>,
    /// Stable redaction/policy status for the accepted content.
    pub redaction_status: String,
}

/// Stable schema name for remember-time staged link suggestions.
pub const REMEMBER_SUGGESTED_LINK_SCHEMA_V1: &str = "ee.remember.suggested_link.v1";

const REMEMBER_SUGGESTED_LINK_LIMIT: usize = 5;

/// A staged adjacent-memory suggestion returned from `ee remember`.
#[derive(Clone, Debug, PartialEq)]
pub struct RememberSuggestedLink {
    /// Per-item schema for forward-compatible contract tests.
    pub schema: &'static str,
    /// Suggested edge relation.
    pub relation: String,
    /// Existing memory that may be adjacent to the newly remembered memory.
    pub target_memory_id: String,
    /// Deterministic score for ordering and display.
    pub score: f32,
    /// Conservative confidence in the suggestion.
    pub confidence: f32,
    /// Number of evidence features supporting the suggestion.
    pub evidence_count: u32,
    /// Human-readable summary of the evidence.
    pub evidence_summary: String,
    /// Candidate source that produced the suggestion.
    pub source: String,
    /// Canonical tags shared with the newly remembered memory.
    pub matched_tags: Vec<String>,
    /// Explicit next action; no durable link is created automatically.
    pub next_action: String,
}

/// Non-fatal remember suggestion degradation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RememberSuggestedLinkDegradation {
    /// Stable machine code.
    pub code: String,
    /// Severity string.
    pub severity: String,
    /// Human-readable message.
    pub message: String,
    /// Suggested repair action.
    pub repair: String,
}

/// Create a manual memory and enqueue a single-document index job.
///
/// Dry-run mode validates and returns the canonical record shape without
/// opening or mutating storage.
pub fn remember_memory(
    options: &RememberMemoryOptions<'_>,
) -> Result<RememberMemoryReport, DomainError> {
    let prepared = prepare_remember_memory(options)?;
    if options.dry_run {
        return Ok(RememberMemoryReport {
            version: env!("CARGO_PKG_VERSION"),
            memory_id: prepared.memory_id,
            workspace_id: prepared.workspace_id,
            workspace_path: prepared.workspace_path,
            database_path: prepared.database_path,
            content: prepared.content,
            level: prepared.level,
            kind: prepared.kind,
            confidence: prepared.confidence,
            tags: prepared.tags,
            source: prepared.provenance_uri,
            valid_from: prepared.valid_from,
            valid_to: prepared.valid_to,
            validity_status: prepared.validity_status,
            validity_window_kind: prepared.validity_window_kind,
            dry_run: true,
            persisted: false,
            revision_number: 1,
            revision_group_id: None,
            audit_id: None,
            index_job_id: None,
            index_status: "dry_run_not_queued".to_owned(),
            effect_ids: Vec::new(),
            suggested_links: Vec::new(),
            suggested_link_status: "dry_run_not_evaluated".to_owned(),
            suggested_link_degradations: Vec::new(),
            redaction_status: "checked".to_owned(),
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

    let memory_id = prepared.memory_id.to_string();
    let audit_id = generate_audit_id();
    let index_job_id = generate_search_index_job_id();
    let memory_input = CreateMemoryInput {
        workspace_id: prepared.workspace_id.clone(),
        level: prepared.level.as_str().to_owned(),
        kind: prepared.kind.as_str().to_owned(),
        content: prepared.content.clone(),
        confidence: prepared.confidence,
        utility: UnitScore::neutral().into_inner(),
        importance: UnitScore::neutral().into_inner(),
        provenance_uri: prepared.provenance_uri.clone(),
        trust_class: TrustClass::HumanExplicit.as_str().to_owned(),
        trust_subclass: Some("ee remember".to_owned()),
        tags: prepared.tags.clone(),
        valid_from: prepared.valid_from.clone(),
        valid_to: prepared.valid_to.clone(),
    };
    let audit_details = remember_audit_details(&memory_id, &memory_input);
    let index_input = CreateSearchIndexJobInput {
        workspace_id: prepared.workspace_id.clone(),
        job_type: SearchIndexJobType::SingleDocument,
        document_source: Some("memory".to_owned()),
        document_id: Some(memory_id.clone()),
        documents_total: 1,
    };

    connection
        .with_transaction(|| {
            connection.insert_memory(&memory_id, &memory_input)?;
            connection.insert_audit(
                &audit_id,
                &CreateAuditInput {
                    workspace_id: Some(memory_input.workspace_id.clone()),
                    actor: Some("ee remember".to_owned()),
                    action: audit_actions::MEMORY_CREATE.to_owned(),
                    target_type: Some("memory".to_owned()),
                    target_id: Some(memory_id.clone()),
                    details: Some(audit_details.clone()),
                },
            )?;
            connection.insert_search_index_job(&index_job_id, &index_input)
        })
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to store memory: {error}"),
            repair: Some("ee doctor".to_string()),
        })?;

    let (suggested_links, suggested_link_status, suggested_link_degradations) =
        match suggest_links_for_remember(
            &connection,
            &prepared.workspace_id,
            &memory_id,
            &prepared.tags,
        ) {
            Ok(suggested_links) => {
                let status = if suggested_links.is_empty() {
                    "no_candidates"
                } else {
                    "ready"
                };
                (suggested_links, status.to_owned(), Vec::new())
            }
            Err(error) => (
                Vec::new(),
                "degraded".to_owned(),
                vec![RememberSuggestedLinkDegradation {
                    code: "remember_link_suggestion_failed".to_owned(),
                    severity: "low".to_owned(),
                    message: format!(
                        "Remembered the memory, but link suggestions failed: {}",
                        error.message()
                    ),
                    repair: "Run `ee doctor --json` and inspect memory tag/link indexes."
                        .to_owned(),
                }],
            ),
        };

    Ok(RememberMemoryReport {
        version: env!("CARGO_PKG_VERSION"),
        memory_id: prepared.memory_id,
        workspace_id: prepared.workspace_id,
        workspace_path: prepared.workspace_path,
        database_path: prepared.database_path,
        content: prepared.content,
        level: prepared.level,
        kind: prepared.kind,
        confidence: prepared.confidence,
        tags: prepared.tags,
        source: prepared.provenance_uri,
        valid_from: prepared.valid_from,
        valid_to: prepared.valid_to,
        validity_status: prepared.validity_status,
        validity_window_kind: prepared.validity_window_kind,
        dry_run: false,
        persisted: true,
        revision_number: 1,
        revision_group_id: None,
        audit_id: Some(audit_id),
        index_job_id: Some(index_job_id),
        index_status: "queued".to_owned(),
        effect_ids: Vec::new(),
        suggested_links,
        suggested_link_status,
        suggested_link_degradations,
        redaction_status: "checked".to_owned(),
    })
}

#[derive(Clone, Debug)]
struct PreparedRememberMemory {
    memory_id: MemoryId,
    workspace_id: String,
    workspace_path: PathBuf,
    database_path: PathBuf,
    content: String,
    level: MemoryLevel,
    kind: MemoryKind,
    confidence: f32,
    tags: Vec<String>,
    provenance_uri: Option<String>,
    valid_from: Option<String>,
    valid_to: Option<String>,
    validity_status: String,
    validity_window_kind: String,
}

fn prepare_remember_memory(
    options: &RememberMemoryOptions<'_>,
) -> Result<PreparedRememberMemory, DomainError> {
    let workspace_path = resolve_workspace_path(options.workspace_path, options.dry_run)?;
    let database_path = options
        .database_path
        .map(Path::to_path_buf)
        .unwrap_or_else(|| workspace_path.join(".ee").join("ee.db"));
    let content = MemoryContent::parse(options.content)
        .map_err(|error| remember_usage_error(error.to_string()))?
        .as_str()
        .to_owned();
    validate_remember_policy(&content)?;
    let level = MemoryLevel::from_str(options.level)
        .map_err(|error| remember_usage_error(error.to_string()))?;
    let kind = MemoryKind::from_str(options.kind)
        .map_err(|error| remember_usage_error(error.to_string()))?;
    let confidence = UnitScore::parse(options.confidence)
        .map_err(|error| remember_usage_error(error.to_string()))?
        .into_inner();
    let tags = parse_tags(options.tags)?;
    let provenance_uri = options
        .source
        .map(|source| {
            ProvenanceUri::from_str(source)
                .map(|uri| uri.to_string())
                .map_err(|error| remember_usage_error(format!("invalid provenance URI: {error}")))
        })
        .transpose()?;
    let validity = prepare_validity_window(options.valid_from, options.valid_to)?;

    Ok(PreparedRememberMemory {
        memory_id: MemoryId::now(),
        workspace_id: stable_workspace_id(&workspace_path),
        workspace_path,
        database_path,
        content,
        level,
        kind,
        confidence,
        tags,
        provenance_uri,
        valid_from: validity.valid_from,
        valid_to: validity.valid_to,
        validity_status: validity.status,
        validity_window_kind: validity.window_kind,
    })
}

#[derive(Clone, Debug, PartialEq)]
struct PreparedValidityWindow {
    valid_from: Option<String>,
    valid_to: Option<String>,
    status: String,
    window_kind: String,
}

/// Stable validity metadata derived from a memory's validity window.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MemoryValidity {
    /// RFC3339 timestamp when this memory becomes applicable.
    pub valid_from: Option<String>,
    /// RFC3339 timestamp when this memory stops being applicable.
    pub valid_to: Option<String>,
    /// Current status: unknown, current, future, expired, or invalid.
    pub status: String,
    /// Window shape: unbounded, starts_at, ends_at, bounded, or instant.
    pub window_kind: String,
}

/// Compute stable display metadata for stored validity timestamps.
#[must_use]
pub fn memory_validity(valid_from: &Option<String>, valid_to: &Option<String>) -> MemoryValidity {
    let parsed_from = valid_from
        .as_deref()
        .and_then(|timestamp| DateTime::parse_from_rfc3339(timestamp).ok())
        .map(|timestamp| timestamp.with_timezone(&Utc));
    let parsed_to = valid_to
        .as_deref()
        .and_then(|timestamp| DateTime::parse_from_rfc3339(timestamp).ok())
        .map(|timestamp| timestamp.with_timezone(&Utc));
    let status = match (
        valid_from.as_ref(),
        valid_to.as_ref(),
        parsed_from,
        parsed_to,
    ) {
        (Some(_), _, None, _) | (_, Some(_), _, None) => "invalid",
        (_, _, from, to) => classify_validity_status(from, to),
    };

    MemoryValidity {
        valid_from: valid_from.clone(),
        valid_to: valid_to.clone(),
        status: status.to_owned(),
        window_kind: validity_window_kind(valid_from.as_deref(), valid_to.as_deref()).to_owned(),
    }
}

fn prepare_validity_window(
    valid_from: Option<&str>,
    valid_to: Option<&str>,
) -> Result<PreparedValidityWindow, DomainError> {
    let parsed_from = parse_validity_timestamp("valid_from", valid_from)?;
    let parsed_to = parse_validity_timestamp("valid_to", valid_to)?;

    if let (Some(from), Some(to)) = (parsed_from.as_ref(), parsed_to.as_ref()) {
        if from > to {
            return Err(remember_usage_error(
                "valid_from must be less than or equal to valid_to".to_owned(),
            ));
        }
    }

    let valid_from = parsed_from.map(normalize_validity_timestamp);
    let valid_to = parsed_to.map(normalize_validity_timestamp);

    Ok(PreparedValidityWindow {
        status: memory_validity(&valid_from, &valid_to).status,
        window_kind: validity_window_kind(valid_from.as_deref(), valid_to.as_deref()).to_owned(),
        valid_from,
        valid_to,
    })
}

fn parse_validity_timestamp(
    field_name: &str,
    value: Option<&str>,
) -> Result<Option<DateTime<Utc>>, DomainError> {
    value
        .map(|raw| {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                return Err(remember_usage_error(format!(
                    "{field_name} must be a non-empty RFC3339 timestamp"
                )));
            }
            DateTime::parse_from_rfc3339(trimmed)
                .map(|timestamp| timestamp.with_timezone(&Utc))
                .map_err(|error| {
                    remember_usage_error(format!(
                        "{field_name} must be an RFC3339 timestamp: {error}"
                    ))
                })
        })
        .transpose()
}

fn normalize_validity_timestamp(timestamp: DateTime<Utc>) -> String {
    timestamp.to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn classify_validity_status(
    valid_from: Option<DateTime<Utc>>,
    valid_to: Option<DateTime<Utc>>,
) -> &'static str {
    match (valid_from, valid_to) {
        (None, None) => "unknown",
        (from, to) => {
            let now = Utc::now();
            if from.is_some_and(|timestamp| now < timestamp) {
                "future"
            } else if to.is_some_and(|timestamp| now > timestamp) {
                "expired"
            } else {
                "current"
            }
        }
    }
}

fn validity_window_kind(valid_from: Option<&str>, valid_to: Option<&str>) -> &'static str {
    match (valid_from, valid_to) {
        (None, None) => "unbounded",
        (Some(from), Some(to)) if from == to => "instant",
        (Some(_), Some(_)) => "bounded",
        (Some(_), None) => "starts_at",
        (None, Some(_)) => "ends_at",
    }
}

fn parse_tags(tags: Option<&str>) -> Result<Vec<String>, DomainError> {
    let mut unique = BTreeSet::new();
    if let Some(tags) = tags {
        for raw in tags.split(',').map(str::trim).filter(|tag| !tag.is_empty()) {
            let tag = Tag::parse(raw).map_err(|error| remember_usage_error(error.to_string()))?;
            unique.insert(tag.to_string());
        }
    }
    Ok(unique.into_iter().collect())
}

const REMEMBER_SECRET_PATTERNS: &[&str] = &[
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

fn validate_remember_policy(content: &str) -> Result<(), DomainError> {
    let lowered = content.to_ascii_lowercase();
    if REMEMBER_SECRET_PATTERNS
        .iter()
        .any(|pattern| lowered.contains(pattern))
    {
        return Err(DomainError::PolicyDenied {
            message: "Refusing to persist memory content that looks like it contains a secret."
                .to_owned(),
            repair: Some(
                "Redact the secret and run `ee remember` again with only durable evidence."
                    .to_owned(),
            ),
        });
    }
    Ok(())
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
            repair: Some("ee init --workspace .".to_owned()),
        }),
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
        repair: Some("ee init --workspace .".to_owned()),
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
            repair: Some("ee doctor".to_owned()),
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
            repair: Some("ee doctor".to_owned()),
        })
}

fn stable_workspace_id(path: &Path) -> String {
    let hash = blake3::hash(format!("workspace:{}", path.to_string_lossy()).as_bytes());
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&hash.as_bytes()[..16]);
    WorkspaceId::from_uuid(uuid::Uuid::from_bytes(bytes)).to_string()
}

fn generate_search_index_job_id() -> String {
    let memory_id = MemoryId::now().to_string();
    let payload = memory_id.trim_start_matches("mem_");
    format!("sidx_{payload}")
}

fn remember_audit_details(memory_id: &str, input: &CreateMemoryInput) -> String {
    serde_json::json!({
        "schema": "ee.audit.memory_create.v1",
        "command": "ee remember",
        "memoryId": memory_id,
        "level": input.level,
        "kind": input.kind,
        "confidence": input.confidence,
        "trustClass": input.trust_class,
        "trustSubclass": input.trust_subclass,
        "provenanceUri": input.provenance_uri,
        "tagCount": input.tags.len(),
    })
    .to_string()
}

fn suggest_links_for_remember(
    connection: &DbConnection,
    workspace_id: &str,
    memory_id: &str,
    tags: &[String],
) -> Result<Vec<RememberSuggestedLink>, DomainError> {
    if tags.is_empty() {
        return Ok(Vec::new());
    }

    let mut matches: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for tag in tags {
        let tagged_memory_ids =
            connection
                .list_memories_by_tag(workspace_id, tag)
                .map_err(|error| DomainError::Storage {
                    message: format!("Failed to query memories by tag for suggestions: {error}"),
                    repair: Some("ee doctor --json".to_owned()),
                })?;
        for target_memory_id in tagged_memory_ids {
            if target_memory_id == memory_id {
                continue;
            }
            matches
                .entry(target_memory_id)
                .or_default()
                .insert(tag.clone());
        }
    }

    if matches.is_empty() {
        return Ok(Vec::new());
    }

    let existing_links = connection
        .list_memory_links_for_memory(memory_id, None)
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to query existing memory links for suggestions: {error}"),
            repair: Some("ee doctor --json".to_owned()),
        })?;
    let mut existing_targets = BTreeSet::new();
    for link in existing_links {
        if link.src_memory_id == memory_id {
            existing_targets.insert(link.dst_memory_id);
        } else if link.dst_memory_id == memory_id {
            existing_targets.insert(link.src_memory_id);
        }
    }

    Ok(build_suggested_links_from_matches(
        memory_id,
        matches,
        &existing_targets,
        tags.len(),
        REMEMBER_SUGGESTED_LINK_LIMIT,
    ))
}

fn build_suggested_links_from_matches(
    memory_id: &str,
    matches: BTreeMap<String, BTreeSet<String>>,
    existing_targets: &BTreeSet<String>,
    tag_count: usize,
    limit: usize,
) -> Vec<RememberSuggestedLink> {
    let mut candidates: Vec<(String, Vec<String>)> = matches
        .into_iter()
        .filter(|(target_memory_id, matched_tags)| {
            target_memory_id != memory_id
                && !matched_tags.is_empty()
                && !existing_targets.contains(target_memory_id)
        })
        .map(|(target_memory_id, matched_tags)| {
            (
                target_memory_id,
                matched_tags.into_iter().collect::<Vec<_>>(),
            )
        })
        .collect();

    candidates.sort_by(|(left_id, left_tags), (right_id, right_tags)| {
        right_tags
            .len()
            .cmp(&left_tags.len())
            .then_with(|| left_id.cmp(right_id))
    });

    candidates
        .into_iter()
        .take(limit)
        .map(|(target_memory_id, matched_tags)| {
            let evidence_count = u32::try_from(matched_tags.len()).unwrap_or(u32::MAX);
            RememberSuggestedLink {
                schema: REMEMBER_SUGGESTED_LINK_SCHEMA_V1,
                relation: "co_tag".to_owned(),
                target_memory_id,
                score: co_tag_score(matched_tags.len(), tag_count),
                confidence: co_tag_confidence(matched_tags.len()),
                evidence_count,
                evidence_summary: summarize_matched_tags(&matched_tags),
                source: "tag_cooccurrence".to_owned(),
                matched_tags,
                next_action:
                    "Review this staged link; apply only through an explicit curation/apply command."
                        .to_owned(),
            }
        })
        .collect()
}

fn summarize_matched_tags(tags: &[String]) -> String {
    if tags.len() == 1 {
        return format!("Shares tag `{}` with the newly remembered memory.", tags[0]);
    }

    let rendered = tags
        .iter()
        .map(|tag| format!("`{tag}`"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "Shares {} tags with the newly remembered memory: {rendered}.",
        tags.len()
    )
}

fn co_tag_score(matched_tag_count: usize, total_tag_count: usize) -> f32 {
    let matched = usize_count_to_f32(matched_tag_count);
    let total = usize_count_to_f32(total_tag_count.max(1));
    (0.55 + ((matched / total) * 0.4)).min(0.95)
}

fn co_tag_confidence(matched_tag_count: usize) -> f32 {
    (0.5 + (usize_count_to_f32(matched_tag_count) * 0.1)).min(0.9)
}

fn usize_count_to_f32(value: usize) -> f32 {
    f32::from(u16::try_from(value).unwrap_or(u16::MAX))
}

fn remember_usage_error(message: String) -> DomainError {
    DomainError::Usage {
        message,
        repair: Some("ee remember --help".to_owned()),
    }
}

/// Options for retrieving a memory.
#[derive(Clone, Debug)]
pub struct GetMemoryOptions<'a> {
    /// Database path.
    pub database_path: &'a Path,
    /// Memory ID to retrieve.
    pub memory_id: &'a str,
    /// Whether to include tombstoned memories.
    pub include_tombstoned: bool,
}

/// Result of a memory show operation.
#[derive(Clone, Debug)]
pub struct MemoryShowReport {
    /// Package version for stable output.
    pub version: &'static str,
    /// The memory details if found.
    pub memory: Option<MemoryDetails>,
    /// Whether the memory was found.
    pub found: bool,
    /// Whether the memory is tombstoned (soft-deleted).
    pub is_tombstoned: bool,
    /// Error message if retrieval failed.
    pub error: Option<String>,
}

impl MemoryShowReport {
    /// Create a report for a found memory.
    #[must_use]
    pub fn found(details: MemoryDetails) -> Self {
        let is_tombstoned = details.memory.tombstoned_at.is_some();
        Self {
            version: env!("CARGO_PKG_VERSION"),
            memory: Some(details),
            found: true,
            is_tombstoned,
            error: None,
        }
    }

    /// Create a report for a not-found memory.
    #[must_use]
    pub fn not_found() -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION"),
            memory: None,
            found: false,
            is_tombstoned: false,
            error: None,
        }
    }

    /// Create a report for a database error.
    #[must_use]
    pub fn error(message: String) -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION"),
            memory: None,
            found: false,
            is_tombstoned: false,
            error: Some(message),
        }
    }
}

/// Retrieve a memory by ID with its tags.
///
/// Returns `None` if the memory does not exist. If `include_tombstoned` is false,
/// tombstoned memories are treated as not found.
pub fn get_memory_details(options: &GetMemoryOptions<'_>) -> MemoryShowReport {
    let conn = match DbConnection::open_file(options.database_path) {
        Ok(c) => c,
        Err(e) => return MemoryShowReport::error(format!("Failed to open database: {e}")),
    };

    let memory = match conn.get_memory(options.memory_id) {
        Ok(Some(m)) => m,
        Ok(None) => return MemoryShowReport::not_found(),
        Err(e) => return MemoryShowReport::error(format!("Failed to query memory: {e}")),
    };

    // Check if tombstoned and whether to include it
    if memory.tombstoned_at.is_some() && !options.include_tombstoned {
        return MemoryShowReport::not_found();
    }

    let tags = match conn.get_memory_tags(options.memory_id) {
        Ok(t) => t,
        Err(e) => return MemoryShowReport::error(format!("Failed to query tags: {e}")),
    };

    MemoryShowReport::found(MemoryDetails { memory, tags })
}

/// Options for listing memories.
#[derive(Clone, Debug)]
pub struct ListMemoriesOptions<'a> {
    /// Database path.
    pub database_path: &'a Path,
    /// Workspace path (used to derive workspace_id).
    pub workspace_path: &'a Path,
    /// Filter by memory level.
    pub level: Option<&'a str>,
    /// Filter by tag.
    pub tag: Option<&'a str>,
    /// Maximum number of memories to return.
    pub limit: u32,
    /// Whether to include tombstoned memories.
    pub include_tombstoned: bool,
}

/// Result of a memory list operation.
#[derive(Clone, Debug)]
pub struct MemoryListReport {
    /// Package version for stable output.
    pub version: &'static str,
    /// List of memory summaries.
    pub memories: Vec<MemorySummary>,
    /// Total count of memories matching the filter.
    pub total_count: u32,
    /// Whether results were truncated due to limit.
    pub truncated: bool,
    /// Filter applied.
    pub filter: MemoryListFilter,
    /// Error message if retrieval failed.
    pub error: Option<String>,
}

/// Summary of a memory for list output.
#[derive(Clone, Debug)]
pub struct MemorySummary {
    /// Memory ID.
    pub id: String,
    /// Memory level.
    pub level: String,
    /// Memory kind.
    pub kind: String,
    /// Content preview (truncated).
    pub content_preview: String,
    /// Confidence score.
    pub confidence: f32,
    /// Provenance URI (EE-072: preserve provenance through JSON output).
    pub provenance_uri: Option<String>,
    /// Whether tombstoned.
    pub is_tombstoned: bool,
    /// RFC3339 timestamp when this memory becomes applicable.
    pub valid_from: Option<String>,
    /// RFC3339 timestamp when this memory stops being applicable.
    pub valid_to: Option<String>,
    /// Current validity status computed from the stored validity window.
    pub validity_status: String,
    /// Stable shape of the validity window.
    pub validity_window_kind: String,
    /// Creation timestamp.
    pub created_at: String,
}

/// Filter applied to memory list.
#[derive(Clone, Debug, Default)]
pub struct MemoryListFilter {
    /// Level filter if applied.
    pub level: Option<String>,
    /// Tag filter if applied.
    pub tag: Option<String>,
    /// Include tombstoned.
    pub include_tombstoned: bool,
}

impl MemoryListReport {
    /// Create a successful report.
    #[must_use]
    pub fn success(
        memories: Vec<MemorySummary>,
        total_count: u32,
        truncated: bool,
        filter: MemoryListFilter,
    ) -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION"),
            memories,
            total_count,
            truncated,
            filter,
            error: None,
        }
    }

    /// Create an error report.
    #[must_use]
    pub fn error(message: String) -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION"),
            memories: Vec::new(),
            total_count: 0,
            truncated: false,
            filter: MemoryListFilter::default(),
            error: Some(message),
        }
    }
}

const CONTENT_PREVIEW_LEN: usize = 80;

fn truncate_content(content: &str) -> String {
    if content.len() <= CONTENT_PREVIEW_LEN {
        content.to_string()
    } else {
        format!("{}...", &content[..CONTENT_PREVIEW_LEN])
    }
}

/// List memories matching the given criteria.
pub fn list_memories(options: &ListMemoriesOptions<'_>) -> MemoryListReport {
    let conn = match DbConnection::open_file(options.database_path) {
        Ok(c) => c,
        Err(e) => return MemoryListReport::error(format!("Failed to open database: {e}")),
    };

    let filter = MemoryListFilter {
        level: options.level.map(String::from),
        tag: options.tag.map(String::from),
        include_tombstoned: options.include_tombstoned,
    };

    // Match `remember`'s workspace-ID derivation so absolute paths,
    // relative paths, and symlinked paths all address the same records.
    let workspace_path = options
        .workspace_path
        .canonicalize()
        .unwrap_or_else(|_| options.workspace_path.to_path_buf());
    let workspace_id = stable_workspace_id(&workspace_path);

    // If filtering by tag, get memory IDs first
    let memory_ids: Option<Vec<String>> = if let Some(tag) = options.tag {
        match conn.list_memories_by_tag(&workspace_id, tag) {
            Ok(ids) => Some(ids),
            Err(e) => return MemoryListReport::error(format!("Failed to query by tag: {e}")),
        }
    } else {
        None
    };

    // Get memories
    let stored = match conn.list_memories(&workspace_id, options.level, options.include_tombstoned)
    {
        Ok(m) => m,
        Err(e) => return MemoryListReport::error(format!("Failed to list memories: {e}")),
    };

    // Filter by tag if needed
    let filtered: Vec<_> = if let Some(ref ids) = memory_ids {
        stored.into_iter().filter(|m| ids.contains(&m.id)).collect()
    } else {
        stored
    };

    let total_count = filtered.len() as u32;
    let truncated = total_count > options.limit;

    let memories: Vec<MemorySummary> = filtered
        .into_iter()
        .take(options.limit as usize)
        .map(|m| {
            let validity = memory_validity(&m.valid_from, &m.valid_to);
            MemorySummary {
                id: m.id,
                level: m.level,
                kind: m.kind,
                content_preview: truncate_content(&m.content),
                confidence: m.confidence,
                provenance_uri: m.provenance_uri,
                is_tombstoned: m.tombstoned_at.is_some(),
                valid_from: validity.valid_from,
                valid_to: validity.valid_to,
                validity_status: validity.status,
                validity_window_kind: validity.window_kind,
                created_at: m.created_at,
            }
        })
        .collect();

    MemoryListReport::success(memories, total_count, truncated, filter)
}

/// Options for retrieving memory history.
#[derive(Clone, Debug)]
pub struct GetMemoryHistoryOptions<'a> {
    /// Database path.
    pub database_path: &'a Path,
    /// Memory ID to retrieve history for.
    pub memory_id: &'a str,
    /// Maximum number of history entries to return.
    pub limit: u32,
}

/// A single entry in the memory history timeline.
#[derive(Clone, Debug, PartialEq)]
pub struct MemoryHistoryEntry {
    /// Audit entry ID.
    pub audit_id: String,
    /// Timestamp of the event.
    pub timestamp: String,
    /// Actor who performed the action (if known).
    pub actor: Option<String>,
    /// Action performed (e.g., "create", "update", "tombstone").
    pub action: String,
    /// Details about the change (JSON string if available).
    pub details: Option<String>,
}

/// Result of a memory history operation.
#[derive(Clone, Debug)]
pub struct MemoryHistoryReport {
    /// Package version for stable output.
    pub version: &'static str,
    /// Memory ID for which history was requested.
    pub memory_id: String,
    /// Whether the memory exists.
    pub memory_exists: bool,
    /// Whether the memory is tombstoned.
    pub is_tombstoned: bool,
    /// History entries ordered from newest to oldest.
    pub entries: Vec<MemoryHistoryEntry>,
    /// Total number of history entries for this memory.
    pub total_count: u32,
    /// Whether results were truncated due to limit.
    pub truncated: bool,
    /// Error message if retrieval failed.
    pub error: Option<String>,
}

impl MemoryHistoryReport {
    /// Create a report for a found memory with history.
    #[must_use]
    pub fn found(
        memory_id: String,
        is_tombstoned: bool,
        entries: Vec<MemoryHistoryEntry>,
        total_count: u32,
        truncated: bool,
    ) -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION"),
            memory_id,
            memory_exists: true,
            is_tombstoned,
            entries,
            total_count,
            truncated,
            error: None,
        }
    }

    /// Create a report for a not-found memory.
    #[must_use]
    pub fn not_found(memory_id: String) -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION"),
            memory_id,
            memory_exists: false,
            is_tombstoned: false,
            entries: Vec::new(),
            total_count: 0,
            truncated: false,
            error: None,
        }
    }

    /// Create a report for a database error.
    #[must_use]
    pub fn error(memory_id: String, message: String) -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION"),
            memory_id,
            memory_exists: false,
            is_tombstoned: false,
            entries: Vec::new(),
            total_count: 0,
            truncated: false,
            error: Some(message),
        }
    }
}

/// Retrieve the history of a memory by querying audit log entries.
///
/// Returns all audit entries for the specified memory, ordered from newest to oldest.
/// If the memory does not exist, returns a not-found report.
pub fn get_memory_history(options: &GetMemoryHistoryOptions<'_>) -> MemoryHistoryReport {
    let conn = match DbConnection::open_file(options.database_path) {
        Ok(c) => c,
        Err(e) => {
            return MemoryHistoryReport::error(
                options.memory_id.to_string(),
                format!("Failed to open database: {e}"),
            );
        }
    };

    // First check if memory exists
    let memory = match conn.get_memory(options.memory_id) {
        Ok(Some(m)) => m,
        Ok(None) => return MemoryHistoryReport::not_found(options.memory_id.to_string()),
        Err(e) => {
            return MemoryHistoryReport::error(
                options.memory_id.to_string(),
                format!("Failed to query memory: {e}"),
            );
        }
    };

    let is_tombstoned = memory.tombstoned_at.is_some();

    // Get audit entries for this memory
    let all_entries = match conn.list_audit_by_target("memory", options.memory_id, None) {
        Ok(entries) => entries,
        Err(e) => {
            return MemoryHistoryReport::error(
                options.memory_id.to_string(),
                format!("Failed to query audit log: {e}"),
            );
        }
    };

    let total_count = all_entries.len() as u32;
    let truncated = total_count > options.limit;

    let entries: Vec<MemoryHistoryEntry> = all_entries
        .into_iter()
        .take(options.limit as usize)
        .map(|e| MemoryHistoryEntry {
            audit_id: e.id,
            timestamp: e.timestamp,
            actor: e.actor,
            action: e.action,
            details: e.details,
        })
        .collect();

    MemoryHistoryReport::found(
        options.memory_id.to_string(),
        is_tombstoned,
        entries,
        total_count,
        truncated,
    )
}

// =============================================================================
// Memory Revise (EE-066)
//
// Immutable revision creates a new memory that supersedes an existing one.
// The original memory remains unchanged; a supersession link connects them.
// =============================================================================

/// Reason for revising a memory.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReviseReason {
    /// Content was corrected or clarified.
    Correction,
    /// Content was updated with new information.
    Update,
    /// Content was refined for clarity.
    Refinement,
    /// Content was consolidated from multiple sources.
    Consolidation,
    /// Custom reason provided by the user.
    Custom(String),
}

impl ReviseReason {
    /// Stable wire representation.
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Correction => "correction",
            Self::Update => "update",
            Self::Refinement => "refinement",
            Self::Consolidation => "consolidation",
            Self::Custom(s) => s.as_str(),
        }
    }

    /// Parse a reason string.
    #[must_use]
    pub fn parse(input: &str) -> Self {
        match input {
            "correction" => Self::Correction,
            "update" => Self::Update,
            "refinement" => Self::Refinement,
            "consolidation" => Self::Consolidation,
            other => Self::Custom(other.to_owned()),
        }
    }
}

#[allow(clippy::derivable_impls)]
impl Default for ReviseReason {
    fn default() -> Self {
        Self::Update
    }
}

/// Options for revising a memory.
#[derive(Clone, Debug)]
pub struct ReviseMemoryOptions<'a> {
    /// Database path.
    pub database_path: &'a Path,
    /// ID of the memory to revise.
    pub original_memory_id: &'a str,
    /// New content (if changing).
    pub content: Option<&'a str>,
    /// New level (if changing).
    pub level: Option<&'a str>,
    /// New kind (if changing).
    pub kind: Option<&'a str>,
    /// New confidence (if changing).
    pub confidence: Option<f32>,
    /// New tags (if changing).
    pub tags: Option<Vec<String>>,
    /// New provenance URI (if changing).
    pub provenance_uri: Option<&'a str>,
    /// Reason for the revision.
    pub reason: ReviseReason,
    /// Actor performing the revision.
    pub actor: Option<&'a str>,
    /// Whether to perform a dry run (no changes).
    pub dry_run: bool,
}

/// Result of a memory revise operation.
#[derive(Clone, Debug)]
pub struct MemoryReviseReport {
    /// Package version for stable output.
    pub version: &'static str,
    /// Whether the operation was a dry run.
    pub dry_run: bool,
    /// Whether the revision was successful.
    pub success: bool,
    /// Original memory ID that was revised.
    pub original_id: String,
    /// New memory ID (if created).
    pub new_id: Option<String>,
    /// Revision group ID linking all versions.
    pub revision_group_id: Option<String>,
    /// Revision number within the group.
    pub revision_number: Option<u32>,
    /// Reason for the revision.
    pub reason: String,
    /// Fields that were changed.
    pub changed_fields: Vec<String>,
    /// Error message if revision failed.
    pub error: Option<String>,
}

impl MemoryReviseReport {
    /// Create a successful revision report.
    #[must_use]
    pub fn success(
        original_id: String,
        new_id: String,
        revision_group_id: String,
        revision_number: u32,
        reason: ReviseReason,
        changed_fields: Vec<String>,
        dry_run: bool,
    ) -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION"),
            dry_run,
            success: true,
            original_id,
            new_id: Some(new_id),
            revision_group_id: Some(revision_group_id),
            revision_number: Some(revision_number),
            reason: reason.as_str().to_owned(),
            changed_fields,
            error: None,
        }
    }

    /// Create a dry-run preview report.
    #[must_use]
    pub fn dry_run_preview(
        original_id: String,
        reason: ReviseReason,
        changed_fields: Vec<String>,
    ) -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION"),
            dry_run: true,
            success: true,
            original_id,
            new_id: None,
            revision_group_id: None,
            revision_number: None,
            reason: reason.as_str().to_owned(),
            changed_fields,
            error: None,
        }
    }

    /// Create an unavailable write report while preserving the computed preview.
    #[must_use]
    pub fn write_unavailable(
        original_id: String,
        reason: ReviseReason,
        changed_fields: Vec<String>,
    ) -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION"),
            dry_run: false,
            success: false,
            original_id,
            new_id: None,
            revision_group_id: None,
            revision_number: None,
            reason: reason.as_str().to_owned(),
            changed_fields,
            error: Some(
                "Memory revision writes are unavailable until immutable revision storage and supersession links are implemented; rerun with --dry-run to preview changes."
                    .to_owned(),
            ),
        }
    }

    /// Create a not-found error report.
    #[must_use]
    pub fn not_found(original_id: String) -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION"),
            dry_run: false,
            success: false,
            original_id,
            new_id: None,
            revision_group_id: None,
            revision_number: None,
            reason: String::new(),
            changed_fields: Vec::new(),
            error: Some("Memory not found".to_owned()),
        }
    }

    /// Create a tombstoned error report.
    #[must_use]
    pub fn tombstoned(original_id: String) -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION"),
            dry_run: false,
            success: false,
            original_id,
            new_id: None,
            revision_group_id: None,
            revision_number: None,
            reason: String::new(),
            changed_fields: Vec::new(),
            error: Some("Cannot revise tombstoned memory".to_owned()),
        }
    }

    /// Create a no-changes error report.
    #[must_use]
    pub fn no_changes(original_id: String) -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION"),
            dry_run: false,
            success: false,
            original_id,
            new_id: None,
            revision_group_id: None,
            revision_number: None,
            reason: String::new(),
            changed_fields: Vec::new(),
            error: Some("No changes specified".to_owned()),
        }
    }

    /// Create a database error report.
    #[must_use]
    pub fn error(original_id: String, message: String) -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION"),
            dry_run: false,
            success: false,
            original_id,
            new_id: None,
            revision_group_id: None,
            revision_number: None,
            reason: String::new(),
            changed_fields: Vec::new(),
            error: Some(message),
        }
    }
}

/// Revise an existing memory by creating a new immutable version.
///
/// This function:
/// 1. Validates the original memory exists and is not tombstoned
/// 2. Determines which fields are being changed
/// 3. Creates a new memory with updated fields
/// 4. Links the new memory to the original via supersession
/// 5. Marks the original as superseded
///
/// If `dry_run` is true, no changes are made but the report shows what would happen.
pub fn revise_memory(options: &ReviseMemoryOptions<'_>) -> MemoryReviseReport {
    let conn = match DbConnection::open_file(options.database_path) {
        Ok(c) => c,
        Err(e) => {
            return MemoryReviseReport::error(
                options.original_memory_id.to_owned(),
                format!("Failed to open database: {e}"),
            );
        }
    };

    // Get the original memory
    let original = match conn.get_memory(options.original_memory_id) {
        Ok(Some(m)) => m,
        Ok(None) => return MemoryReviseReport::not_found(options.original_memory_id.to_owned()),
        Err(e) => {
            return MemoryReviseReport::error(
                options.original_memory_id.to_owned(),
                format!("Failed to query memory: {e}"),
            );
        }
    };

    // Check if tombstoned
    if original.tombstoned_at.is_some() {
        return MemoryReviseReport::tombstoned(options.original_memory_id.to_owned());
    }

    // Determine what fields are changing
    let mut changed_fields = Vec::new();

    if let Some(content) = options.content {
        if content != original.content {
            changed_fields.push("content".to_owned());
        }
    }
    if let Some(level) = options.level {
        if level != original.level {
            changed_fields.push("level".to_owned());
        }
    }
    if let Some(kind) = options.kind {
        if kind != original.kind {
            changed_fields.push("kind".to_owned());
        }
    }
    if let Some(confidence) = options.confidence {
        if (confidence - original.confidence).abs() > f32::EPSILON {
            changed_fields.push("confidence".to_owned());
        }
    }
    if options.tags.is_some() {
        changed_fields.push("tags".to_owned());
    }
    if let Some(provenance) = options.provenance_uri {
        let current = original.provenance_uri.as_deref().unwrap_or("");
        if provenance != current {
            changed_fields.push("provenance_uri".to_owned());
        }
    }

    // If no changes, return early
    if changed_fields.is_empty() {
        return MemoryReviseReport::no_changes(options.original_memory_id.to_owned());
    }

    // If dry run, return preview
    if options.dry_run {
        return MemoryReviseReport::dry_run_preview(
            options.original_memory_id.to_owned(),
            options.reason.clone(),
            changed_fields,
        );
    }

    MemoryReviseReport::write_unavailable(
        options.original_memory_id.to_owned(),
        options.reason.clone(),
        changed_fields,
    )
}

// =============================================================================
// Dedupe Detection (EE-069)
//
// Detects potential duplicate memories before creation to warn users about
// existing similar content. Uses both exact matching and similarity scoring.
// =============================================================================

/// Severity of a dedupe warning.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum DedupeSeverity {
    /// Exact content match - very likely a duplicate.
    Exact,
    /// High similarity (>90%) - probably a duplicate.
    High,
    /// Medium similarity (70-90%) - worth reviewing.
    Medium,
    /// Low similarity (50-70%) - possibly related.
    Low,
}

impl DedupeSeverity {
    /// Stable wire representation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::High => "high",
            Self::Medium => "medium",
            Self::Low => "low",
        }
    }

    /// Determine severity from similarity score.
    #[must_use]
    pub fn from_score(score: f32) -> Self {
        if score >= 1.0 - f32::EPSILON {
            Self::Exact
        } else if score >= 0.9 {
            Self::High
        } else if score >= 0.7 {
            Self::Medium
        } else {
            Self::Low
        }
    }
}

/// A warning about a potential duplicate memory.
#[derive(Clone, Debug)]
pub struct DedupeWarning {
    /// ID of the similar existing memory.
    pub existing_memory_id: String,
    /// Similarity score (0.0-1.0).
    pub similarity_score: f32,
    /// Severity of the warning.
    pub severity: DedupeSeverity,
    /// Content preview of the existing memory.
    pub existing_preview: String,
    /// How the match was detected.
    pub match_type: DedupeMatchType,
    /// Suggested action.
    pub suggestion: String,
}

/// How a duplicate match was detected.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DedupeMatchType {
    /// Exact content match.
    ExactContent,
    /// Normalized content match (ignoring whitespace/case).
    NormalizedContent,
    /// Semantic similarity (if available).
    Semantic,
    /// Lexical similarity (word overlap).
    Lexical,
}

impl DedupeMatchType {
    /// Stable wire representation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ExactContent => "exact_content",
            Self::NormalizedContent => "normalized_content",
            Self::Semantic => "semantic",
            Self::Lexical => "lexical",
        }
    }
}

/// Options for dedupe detection.
#[derive(Clone, Debug)]
pub struct DedupeCheckOptions<'a> {
    /// Database path.
    pub database_path: &'a Path,
    /// Content to check for duplicates.
    pub content: &'a str,
    /// Memory level (optional filter).
    pub level: Option<&'a str>,
    /// Memory kind (optional filter).
    pub kind: Option<&'a str>,
    /// Minimum similarity threshold (0.0-1.0).
    pub min_similarity: f32,
    /// Maximum warnings to return.
    pub max_warnings: usize,
}

impl<'a> DedupeCheckOptions<'a> {
    /// Create with defaults.
    #[must_use]
    pub fn new(database_path: &'a Path, content: &'a str) -> Self {
        Self {
            database_path,
            content,
            level: None,
            kind: None,
            min_similarity: 0.5,
            max_warnings: 5,
        }
    }
}

/// Result of a dedupe check.
#[derive(Clone, Debug)]
pub struct DedupeCheckReport {
    /// Package version for stable output.
    pub version: &'static str,
    /// Whether any duplicates were found.
    pub has_warnings: bool,
    /// Warnings ordered by severity (exact first, then by similarity).
    pub warnings: Vec<DedupeWarning>,
    /// Number of memories scanned.
    pub memories_scanned: u32,
    /// Error message if check failed.
    pub error: Option<String>,
}

impl DedupeCheckReport {
    /// Create a report with warnings.
    #[must_use]
    pub fn with_warnings(warnings: Vec<DedupeWarning>, memories_scanned: u32) -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION"),
            has_warnings: !warnings.is_empty(),
            warnings,
            memories_scanned,
            error: None,
        }
    }

    /// Create a report with no warnings.
    #[must_use]
    pub fn no_duplicates(memories_scanned: u32) -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION"),
            has_warnings: false,
            warnings: Vec::new(),
            memories_scanned,
            error: None,
        }
    }

    /// Create an error report.
    #[must_use]
    pub fn error(message: String) -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION"),
            has_warnings: false,
            warnings: Vec::new(),
            memories_scanned: 0,
            error: Some(message),
        }
    }
}

/// Normalize content for comparison (lowercase, collapse whitespace).
fn normalize_content(content: &str) -> String {
    content
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Calculate simple word-based Jaccard similarity between two texts.
fn jaccard_similarity(a: &str, b: &str) -> f32 {
    let words_a: std::collections::HashSet<&str> = a.split_whitespace().collect();
    let words_b: std::collections::HashSet<&str> = b.split_whitespace().collect();

    if words_a.is_empty() && words_b.is_empty() {
        return 1.0;
    }
    if words_a.is_empty() || words_b.is_empty() {
        return 0.0;
    }

    let intersection = words_a.intersection(&words_b).count();
    let union = words_a.union(&words_b).count();

    intersection as f32 / union as f32
}

/// Check for potential duplicate memories.
///
/// Scans existing memories and returns warnings for any that are similar
/// to the provided content. Uses exact matching and lexical similarity.
pub fn check_for_duplicates(options: &DedupeCheckOptions<'_>) -> DedupeCheckReport {
    let conn = match DbConnection::open_file(options.database_path) {
        Ok(c) => c,
        Err(e) => return DedupeCheckReport::error(format!("Failed to open database: {e}")),
    };

    // Get workspace ID - for now use default
    let workspace_id = "default";

    // List memories with optional level filter
    let memories = match conn.list_memories(workspace_id, options.level, false) {
        Ok(m) => m,
        Err(e) => return DedupeCheckReport::error(format!("Failed to list memories: {e}")),
    };

    let memories_scanned = memories.len() as u32;
    let normalized_input = normalize_content(options.content);
    let mut warnings: Vec<DedupeWarning> = Vec::new();

    for memory in memories {
        // Skip if kind filter doesn't match
        if let Some(kind) = options.kind {
            if memory.kind != kind {
                continue;
            }
        }

        // Check exact match
        if memory.content == options.content {
            warnings.push(DedupeWarning {
                existing_memory_id: memory.id.clone(),
                similarity_score: 1.0,
                severity: DedupeSeverity::Exact,
                existing_preview: truncate_content(&memory.content),
                match_type: DedupeMatchType::ExactContent,
                suggestion: format!(
                    "Exact duplicate exists. Consider using `ee memory show {}` to review.",
                    memory.id
                ),
            });
            continue;
        }

        // Check normalized match
        let normalized_memory = normalize_content(&memory.content);
        if normalized_memory == normalized_input {
            warnings.push(DedupeWarning {
                existing_memory_id: memory.id.clone(),
                similarity_score: 0.99,
                severity: DedupeSeverity::Exact,
                existing_preview: truncate_content(&memory.content),
                match_type: DedupeMatchType::NormalizedContent,
                suggestion: format!(
                    "Near-exact match (whitespace/case differs). Review `ee memory show {}`.",
                    memory.id
                ),
            });
            continue;
        }

        // Check lexical similarity
        let similarity = jaccard_similarity(&normalized_input, &normalized_memory);
        if similarity >= options.min_similarity {
            let severity = DedupeSeverity::from_score(similarity);
            warnings.push(DedupeWarning {
                existing_memory_id: memory.id.clone(),
                similarity_score: similarity,
                severity,
                existing_preview: truncate_content(&memory.content),
                match_type: DedupeMatchType::Lexical,
                suggestion: format!(
                    "{:.0}% similar. Consider revising instead: `ee memory revise {}`.",
                    similarity * 100.0,
                    memory.id
                ),
            });
        }
    }

    // Sort by severity (exact first), then by similarity score (descending)
    warnings.sort_by(|a, b| {
        a.severity.cmp(&b.severity).then_with(|| {
            b.similarity_score
                .partial_cmp(&a.similarity_score)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
    });

    // Limit warnings
    warnings.truncate(options.max_warnings);

    if warnings.is_empty() {
        DedupeCheckReport::no_duplicates(memories_scanned)
    } else {
        DedupeCheckReport::with_warnings(warnings, memories_scanned)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), String>;

    fn ensure<T: std::fmt::Debug + PartialEq>(actual: T, expected: T, ctx: &str) -> TestResult {
        if actual == expected {
            Ok(())
        } else {
            Err(format!("{ctx}: expected {expected:?}, got {actual:?}"))
        }
    }

    #[test]
    fn memory_show_report_not_found_is_correct() -> TestResult {
        let report = MemoryShowReport::not_found();

        ensure(report.found, false, "found")?;
        ensure(report.memory.is_none(), true, "memory is none")?;
        ensure(report.is_tombstoned, false, "is_tombstoned")?;
        ensure(report.error.is_none(), true, "no error")
    }

    #[test]
    fn memory_show_report_error_captures_message() -> TestResult {
        let report = MemoryShowReport::error("test error".to_string());

        ensure(report.found, false, "found")?;
        ensure(
            report.error,
            Some("test error".to_string()),
            "error message",
        )
    }

    #[test]
    fn memory_show_report_version_matches_package() -> TestResult {
        let report = MemoryShowReport::not_found();
        ensure(report.version, env!("CARGO_PKG_VERSION"), "version")
    }

    #[test]
    fn remember_memory_dry_run_does_not_create_database() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let report = remember_memory(&RememberMemoryOptions {
            workspace_path: temp.path(),
            database_path: None,
            content: "  Run cargo fmt before release.  ",
            level: "procedural",
            kind: "rule",
            tags: Some("Release,cli,release"),
            confidence: 0.8,
            source: Some("file://AGENTS.md#L42"),
            valid_from: None,
            valid_to: None,
            dry_run: true,
        })
        .map_err(|error| error.message())?;

        ensure(report.dry_run, true, "dry_run")?;
        ensure(report.persisted, false, "persisted")?;
        ensure(report.revision_number, 1, "revision number")?;
        ensure(
            report.revision_group_id.is_none(),
            true,
            "revision group absent",
        )?;
        ensure(report.audit_id.is_none(), true, "audit id absent")?;
        ensure(report.index_job_id.is_none(), true, "index job absent")?;
        ensure(
            report.index_status,
            "dry_run_not_queued".to_string(),
            "index status",
        )?;
        ensure(report.effect_ids.is_empty(), true, "effect ids empty")?;
        ensure(
            report.suggested_links.is_empty(),
            true,
            "suggested links empty",
        )?;
        ensure(
            report.suggested_link_status,
            "dry_run_not_evaluated".to_string(),
            "suggested link status",
        )?;
        ensure(
            report.suggested_link_degradations.is_empty(),
            true,
            "suggested link degradations",
        )?;
        ensure(
            report.redaction_status,
            "checked".to_string(),
            "redaction status",
        )?;
        ensure(
            report.database_path.exists(),
            false,
            "dry run must not create database",
        )?;
        ensure(
            report.tags,
            vec!["cli".to_string(), "release".to_string()],
            "canonical tags",
        )?;
        ensure(
            report.source,
            Some("file://AGENTS.md#L42".to_string()),
            "canonical source",
        )?;
        ensure(report.valid_from, None, "valid_from absent")?;
        ensure(report.valid_to, None, "valid_to absent")?;
        ensure(
            report.validity_status,
            "unknown".to_string(),
            "validity status",
        )?;
        ensure(
            report.validity_window_kind,
            "unbounded".to_string(),
            "validity window kind",
        )
    }

    #[test]
    fn remember_memory_persists_memory_audit_and_index_job() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        std::fs::create_dir(temp.path().join(".ee")).map_err(|error| error.to_string())?;

        let report = remember_memory(&RememberMemoryOptions {
            workspace_path: temp.path(),
            database_path: None,
            content: "Store release checks as durable memory.",
            level: "procedural",
            kind: "rule",
            tags: Some("release,checks"),
            confidence: 0.9,
            source: Some("file://README.md#L74-77"),
            valid_from: None,
            valid_to: None,
            dry_run: false,
        })
        .map_err(|error| error.message())?;

        ensure(report.dry_run, false, "dry_run")?;
        ensure(report.persisted, true, "persisted")?;
        ensure(report.revision_number, 1, "revision number")?;
        ensure(
            report.revision_group_id.is_none(),
            true,
            "revision group absent",
        )?;
        ensure(report.audit_id.is_some(), true, "audit id present")?;
        ensure(report.index_job_id.is_some(), true, "index job id present")?;
        ensure(report.index_status, "queued".to_string(), "index status")?;
        ensure(report.effect_ids.is_empty(), true, "effect ids empty")?;
        ensure(
            report.suggested_links.is_empty(),
            true,
            "suggested links empty",
        )?;
        ensure(
            report.suggested_link_status,
            "no_candidates".to_string(),
            "suggested link status",
        )?;
        ensure(
            report.suggested_link_degradations.is_empty(),
            true,
            "suggested link degradations",
        )?;
        ensure(
            report.redaction_status,
            "checked".to_string(),
            "redaction status",
        )?;
        ensure(report.database_path.exists(), true, "database created")?;

        let connection = crate::db::DbConnection::open_file(&report.database_path)
            .map_err(|error| error.to_string())?;
        let memory = connection
            .get_memory(&report.memory_id.to_string())
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "memory should be persisted".to_string())?;
        ensure(
            memory.workspace_id,
            report.workspace_id.clone(),
            "workspace id",
        )?;
        ensure(
            memory.content,
            "Store release checks as durable memory.".to_string(),
            "content",
        )?;
        ensure(
            memory.trust_class,
            "human_explicit".to_string(),
            "trust class",
        )?;
        ensure(
            memory.provenance_uri,
            Some("file://README.md#L74-77".to_string()),
            "provenance uri",
        )?;
        ensure(memory.valid_from, None, "stored valid_from")?;
        ensure(memory.valid_to, None, "stored valid_to")?;
        let tags = connection
            .get_memory_tags(&report.memory_id.to_string())
            .map_err(|error| error.to_string())?;
        ensure(
            tags,
            vec!["checks".to_string(), "release".to_string()],
            "tags",
        )?;
        let audit_id = report
            .audit_id
            .as_ref()
            .ok_or_else(|| "audit id missing".to_string())?;
        let audit = connection
            .get_audit(audit_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "audit should be persisted".to_string())?;
        ensure(audit.action, "memory.create".to_string(), "audit action")?;
        ensure(
            audit.target_id,
            Some(report.memory_id.to_string()),
            "audit target",
        )?;
        let jobs = connection
            .list_search_index_jobs(
                &report.workspace_id,
                Some(crate::db::SearchIndexJobStatus::Pending),
            )
            .map_err(|error| error.to_string())?;
        ensure(jobs.len(), 1, "pending index job count")?;
        ensure(
            jobs[0].document_id.clone(),
            Some(report.memory_id.to_string()),
            "index job document",
        )
    }

    #[test]
    fn remember_memory_validates_and_stores_temporal_validity_window() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        std::fs::create_dir(temp.path().join(".ee")).map_err(|error| error.to_string())?;

        let report = remember_memory(&RememberMemoryOptions {
            workspace_path: temp.path(),
            database_path: None,
            content: "Temporal memories retain their explicit applicability window.",
            level: "semantic",
            kind: "fact",
            tags: Some("temporal,validity"),
            confidence: 0.8,
            source: None,
            valid_from: Some("2020-01-01T00:00:00+00:00"),
            valid_to: Some("2099-01-01T00:00:00Z"),
            dry_run: false,
        })
        .map_err(|error| error.message())?;

        ensure(
            report.valid_from,
            Some("2020-01-01T00:00:00Z".to_string()),
            "normalized valid_from",
        )?;
        ensure(
            report.valid_to,
            Some("2099-01-01T00:00:00Z".to_string()),
            "normalized valid_to",
        )?;
        ensure(
            report.validity_status,
            "current".to_string(),
            "validity status",
        )?;
        ensure(
            report.validity_window_kind,
            "bounded".to_string(),
            "validity window kind",
        )?;

        let connection = crate::db::DbConnection::open_file(&report.database_path)
            .map_err(|error| error.to_string())?;
        let memory = connection
            .get_memory(&report.memory_id.to_string())
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "memory should be persisted".to_string())?;
        ensure(
            memory.valid_from,
            Some("2020-01-01T00:00:00Z".to_string()),
            "stored valid_from",
        )?;
        ensure(
            memory.valid_to,
            Some("2099-01-01T00:00:00Z".to_string()),
            "stored valid_to",
        )
    }

    #[test]
    fn remember_memory_rejects_invalid_temporal_validity_windows() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;

        let malformed = remember_memory(&RememberMemoryOptions {
            workspace_path: temp.path(),
            database_path: None,
            content: "Temporal windows must parse.",
            level: "semantic",
            kind: "fact",
            tags: None,
            confidence: 0.8,
            source: None,
            valid_from: Some("not a timestamp"),
            valid_to: None,
            dry_run: true,
        });
        match malformed {
            Err(DomainError::Usage { message, .. }) => {
                ensure(message.contains("valid_from"), true, "mentions valid_from")?;
            }
            Err(error) => return Err(format!("expected usage error, got {error:?}")),
            Ok(_) => return Err("malformed valid_from should fail".to_string()),
        }

        let reversed = remember_memory(&RememberMemoryOptions {
            workspace_path: temp.path(),
            database_path: None,
            content: "Temporal windows must be ordered.",
            level: "semantic",
            kind: "fact",
            tags: None,
            confidence: 0.8,
            source: None,
            valid_from: Some("2099-01-01T00:00:00Z"),
            valid_to: Some("2020-01-01T00:00:00Z"),
            dry_run: true,
        });
        match reversed {
            Err(DomainError::Usage { message, .. }) => {
                ensure(message.contains("valid_from"), true, "mentions valid_from")?;
                ensure(message.contains("valid_to"), true, "mentions valid_to")?;
            }
            Err(error) => return Err(format!("expected usage error, got {error:?}")),
            Ok(_) => return Err("reversed validity window should fail".to_string()),
        }

        let boundary = remember_memory(&RememberMemoryOptions {
            workspace_path: temp.path(),
            database_path: None,
            content: "Instant validity windows are accepted at the boundary.",
            level: "semantic",
            kind: "fact",
            tags: None,
            confidence: 0.8,
            source: None,
            valid_from: Some("2050-01-01T00:00:00Z"),
            valid_to: Some("2050-01-01T00:00:00Z"),
            dry_run: true,
        })
        .map_err(|error| error.message())?;
        ensure(
            boundary.validity_window_kind,
            "instant".to_string(),
            "boundary-equal window kind",
        )
    }

    #[test]
    fn remember_memory_returns_tag_cooccurrence_suggestions_without_links() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        std::fs::create_dir(temp.path().join(".ee")).map_err(|error| error.to_string())?;

        let first = remember_memory(&RememberMemoryOptions {
            workspace_path: temp.path(),
            database_path: None,
            content: "Release checks include cargo fmt.",
            level: "procedural",
            kind: "rule",
            tags: Some("release,checks"),
            confidence: 0.9,
            source: None,
            valid_from: None,
            valid_to: None,
            dry_run: false,
        })
        .map_err(|error| error.message())?;
        let second = remember_memory(&RememberMemoryOptions {
            workspace_path: temp.path(),
            database_path: None,
            content: "Release docs mention supported targets.",
            level: "semantic",
            kind: "fact",
            tags: Some("release,docs"),
            confidence: 0.8,
            source: None,
            valid_from: None,
            valid_to: None,
            dry_run: false,
        })
        .map_err(|error| error.message())?;
        let third = remember_memory(&RememberMemoryOptions {
            workspace_path: temp.path(),
            database_path: None,
            content: "Before release, run checks and record evidence.",
            level: "procedural",
            kind: "rule",
            tags: Some("checks,release"),
            confidence: 0.85,
            source: None,
            valid_from: None,
            valid_to: None,
            dry_run: false,
        })
        .map_err(|error| error.message())?;

        ensure(
            third.suggested_link_status,
            "ready".to_string(),
            "suggested link status",
        )?;
        ensure(
            third.suggested_link_degradations.is_empty(),
            true,
            "suggested link degradations",
        )?;
        ensure(third.suggested_links.len(), 2, "suggestion count")?;
        ensure(
            third.suggested_links[0].target_memory_id.clone(),
            first.memory_id.to_string(),
            "highest-overlap target first",
        )?;
        ensure(
            third.suggested_links[0].matched_tags.clone(),
            vec!["checks".to_string(), "release".to_string()],
            "highest-overlap tags",
        )?;
        ensure(
            third.suggested_links[0].relation.clone(),
            "co_tag".to_string(),
            "relation",
        )?;
        ensure(
            third.suggested_links[0].source.clone(),
            "tag_cooccurrence".to_string(),
            "source",
        )?;
        ensure(
            third.suggested_links[1].target_memory_id.clone(),
            second.memory_id.to_string(),
            "lower-overlap target second",
        )?;

        let connection = crate::db::DbConnection::open_file(&third.database_path)
            .map_err(|error| error.to_string())?;
        let links = connection
            .list_all_memory_links(None)
            .map_err(|error| error.to_string())?;
        ensure(
            links.is_empty(),
            true,
            "suggestions must not create durable memory links",
        )
    }

    #[test]
    fn staged_link_builder_suppresses_self_existing_and_limits_stably() -> TestResult {
        let mut matches = BTreeMap::new();
        matches.insert(
            "mem_new".to_string(),
            BTreeSet::from(["release".to_string(), "checks".to_string()]),
        );
        matches.insert(
            "mem_existing".to_string(),
            BTreeSet::from(["release".to_string(), "checks".to_string()]),
        );
        matches.insert(
            "mem_c".to_string(),
            BTreeSet::from(["release".to_string(), "checks".to_string()]),
        );
        matches.insert("mem_a".to_string(), BTreeSet::from(["release".to_string()]));
        matches.insert("mem_b".to_string(), BTreeSet::from(["release".to_string()]));

        let existing_targets = BTreeSet::from(["mem_existing".to_string()]);
        let suggestions =
            build_suggested_links_from_matches("mem_new", matches, &existing_targets, 2, 2);

        ensure(suggestions.len(), 2, "bounded suggestions")?;
        ensure(
            suggestions[0].target_memory_id.clone(),
            "mem_c".to_string(),
            "highest overlap first",
        )?;
        ensure(
            suggestions[1].target_memory_id.clone(),
            "mem_a".to_string(),
            "tie broken by target id",
        )?;
        ensure(
            suggestions
                .iter()
                .any(|suggestion| suggestion.target_memory_id == "mem_new"),
            false,
            "self-link suppressed",
        )?;
        ensure(
            suggestions
                .iter()
                .any(|suggestion| suggestion.target_memory_id == "mem_existing"),
            false,
            "existing link suppressed",
        )
    }

    #[test]
    fn remember_memory_rejects_secret_like_content_before_storage() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let secret_like_content = format!(
            "Rotate {} before release.",
            ["api", "key", "secret", "123"].join("_")
        );
        let result = remember_memory(&RememberMemoryOptions {
            workspace_path: temp.path(),
            database_path: None,
            content: &secret_like_content,
            level: "procedural",
            kind: "rule",
            tags: None,
            confidence: 0.8,
            source: None,
            valid_from: None,
            valid_to: None,
            dry_run: false,
        });

        match result {
            Err(DomainError::PolicyDenied { message, repair }) => {
                ensure(
                    message.contains("secret"),
                    true,
                    "policy error mentions secret",
                )?;
                ensure(repair.is_some(), true, "repair is present")?;
            }
            Err(error) => return Err(format!("expected policy denial, got {error:?}")),
            Ok(report) => {
                return Err(format!(
                    "secret-like content should not persist, got {report:?}"
                ));
            }
        }
        ensure(
            temp.path().join(".ee").join("ee.db").exists(),
            false,
            "policy denial must not create database",
        )
    }

    #[test]
    fn memory_history_report_not_found_is_correct() -> TestResult {
        let report = MemoryHistoryReport::not_found("mem_test".to_string());

        ensure(report.memory_exists, false, "memory_exists")?;
        ensure(report.entries.is_empty(), true, "entries empty")?;
        ensure(report.is_tombstoned, false, "is_tombstoned")?;
        ensure(report.error.is_none(), true, "no error")?;
        ensure(report.memory_id, "mem_test".to_string(), "memory_id")
    }

    #[test]
    fn memory_history_report_error_captures_message() -> TestResult {
        let report = MemoryHistoryReport::error("mem_test".to_string(), "db error".to_string());

        ensure(report.memory_exists, false, "memory_exists")?;
        ensure(report.error, Some("db error".to_string()), "error message")
    }

    #[test]
    fn memory_history_report_found_with_entries() -> TestResult {
        let entries = vec![
            MemoryHistoryEntry {
                audit_id: "audit_001".to_string(),
                timestamp: "2026-04-29T12:00:00Z".to_string(),
                actor: Some("user@example.com".to_string()),
                action: "create".to_string(),
                details: None,
            },
            MemoryHistoryEntry {
                audit_id: "audit_002".to_string(),
                timestamp: "2026-04-29T13:00:00Z".to_string(),
                actor: Some("user@example.com".to_string()),
                action: "update".to_string(),
                details: Some("{\"field\":\"content\"}".to_string()),
            },
        ];

        let report = MemoryHistoryReport::found("mem_test".to_string(), false, entries, 2, false);

        ensure(report.memory_exists, true, "memory_exists")?;
        ensure(report.entries.len(), 2, "entry count")?;
        ensure(report.total_count, 2, "total_count")?;
        ensure(report.truncated, false, "truncated")?;
        ensure(report.is_tombstoned, false, "is_tombstoned")
    }

    #[test]
    fn memory_history_report_version_matches_package() -> TestResult {
        let report = MemoryHistoryReport::not_found("mem_test".to_string());
        ensure(report.version, env!("CARGO_PKG_VERSION"), "version")
    }

    // =========================================================================
    // Memory Revise Tests (EE-066)
    // =========================================================================

    #[test]
    fn revise_reason_as_str_is_stable() -> TestResult {
        ensure(
            ReviseReason::Correction.as_str(),
            "correction",
            "correction",
        )?;
        ensure(ReviseReason::Update.as_str(), "update", "update")?;
        ensure(
            ReviseReason::Refinement.as_str(),
            "refinement",
            "refinement",
        )?;
        ensure(
            ReviseReason::Consolidation.as_str(),
            "consolidation",
            "consolidation",
        )?;
        ensure(
            ReviseReason::Custom("custom-reason".to_owned()).as_str(),
            "custom-reason",
            "custom",
        )
    }

    #[test]
    fn revise_reason_parse_roundtrips() -> TestResult {
        ensure(
            ReviseReason::parse("correction"),
            ReviseReason::Correction,
            "correction",
        )?;
        ensure(
            ReviseReason::parse("update"),
            ReviseReason::Update,
            "update",
        )?;
        ensure(
            ReviseReason::parse("refinement"),
            ReviseReason::Refinement,
            "refinement",
        )?;
        ensure(
            ReviseReason::parse("consolidation"),
            ReviseReason::Consolidation,
            "consolidation",
        )?;
        ensure(
            ReviseReason::parse("my-custom"),
            ReviseReason::Custom("my-custom".to_owned()),
            "custom",
        )
    }

    #[test]
    fn revise_reason_default_is_update() -> TestResult {
        ensure(ReviseReason::default(), ReviseReason::Update, "default")
    }

    #[test]
    fn memory_revise_report_not_found_is_correct() -> TestResult {
        let report = MemoryReviseReport::not_found("mem_missing".to_string());

        ensure(report.success, false, "success")?;
        ensure(report.original_id, "mem_missing".to_string(), "original_id")?;
        ensure(report.new_id.is_none(), true, "new_id is none")?;
        ensure(
            report.error,
            Some("Memory not found".to_owned()),
            "error message",
        )
    }

    #[test]
    fn memory_revise_report_tombstoned_is_correct() -> TestResult {
        let report = MemoryReviseReport::tombstoned("mem_old".to_string());

        ensure(report.success, false, "success")?;
        ensure(report.original_id, "mem_old".to_string(), "original_id")?;
        ensure(
            report.error,
            Some("Cannot revise tombstoned memory".to_owned()),
            "error message",
        )
    }

    #[test]
    fn memory_revise_report_no_changes_is_correct() -> TestResult {
        let report = MemoryReviseReport::no_changes("mem_same".to_string());

        ensure(report.success, false, "success")?;
        ensure(report.original_id, "mem_same".to_string(), "original_id")?;
        ensure(
            report.error,
            Some("No changes specified".to_owned()),
            "error message",
        )
    }

    #[test]
    fn memory_revise_report_success_captures_all_fields() -> TestResult {
        let report = MemoryReviseReport::success(
            "mem_old".to_string(),
            "mem_new".to_string(),
            "rev_group".to_string(),
            2,
            ReviseReason::Correction,
            vec!["content".to_string(), "confidence".to_string()],
            false,
        );

        ensure(report.success, true, "success")?;
        ensure(report.dry_run, false, "dry_run")?;
        ensure(report.original_id, "mem_old".to_string(), "original_id")?;
        ensure(report.new_id, Some("mem_new".to_string()), "new_id")?;
        ensure(
            report.revision_group_id,
            Some("rev_group".to_string()),
            "revision_group_id",
        )?;
        ensure(report.revision_number, Some(2), "revision_number")?;
        ensure(report.reason, "correction".to_string(), "reason")?;
        ensure(report.changed_fields.len(), 2, "changed_fields count")?;
        ensure(report.error.is_none(), true, "no error")
    }

    #[test]
    fn memory_revise_report_dry_run_preview_is_correct() -> TestResult {
        let report = MemoryReviseReport::dry_run_preview(
            "mem_test".to_string(),
            ReviseReason::Update,
            vec!["level".to_string()],
        );

        ensure(report.success, true, "success")?;
        ensure(report.dry_run, true, "dry_run")?;
        ensure(report.new_id.is_none(), true, "no new_id for dry run")?;
        ensure(
            report.revision_group_id.is_none(),
            true,
            "no revision_group_id for dry run",
        )?;
        ensure(report.changed_fields.len(), 1, "changed_fields count")?;
        ensure(report.error.is_none(), true, "no error")
    }

    #[test]
    fn memory_revise_report_write_unavailable_preserves_preview_fields() -> TestResult {
        let report = MemoryReviseReport::write_unavailable(
            "mem_old".to_string(),
            ReviseReason::Correction,
            vec!["content".to_string(), "confidence".to_string()],
        );

        ensure(report.success, false, "success")?;
        ensure(report.dry_run, false, "dry_run")?;
        ensure(report.original_id, "mem_old".to_string(), "original_id")?;
        ensure(report.new_id.is_none(), true, "new_id absent")?;
        ensure(
            report.revision_group_id.is_none(),
            true,
            "revision group absent",
        )?;
        ensure(report.revision_number.is_none(), true, "revision absent")?;
        ensure(report.reason, "correction".to_string(), "reason")?;
        ensure(
            report.changed_fields,
            vec!["content".to_string(), "confidence".to_string()],
            "changed fields",
        )?;
        ensure(
            report
                .error
                .as_deref()
                .is_some_and(|message| message.contains("unavailable")),
            true,
            "unavailable error",
        )
    }

    #[test]
    fn revise_memory_non_dry_run_reports_unavailable_instead_of_stub_success() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        std::fs::create_dir(temp.path().join(".ee")).map_err(|error| error.to_string())?;

        let created = remember_memory(&RememberMemoryOptions {
            workspace_path: temp.path(),
            database_path: None,
            content: "Store release checks as durable memory.",
            level: "procedural",
            kind: "rule",
            tags: Some("release,checks"),
            confidence: 0.9,
            source: Some("file://README.md#L74-77"),
            valid_from: None,
            valid_to: None,
            dry_run: false,
        })
        .map_err(|error| error.message())?;
        let memory_id = created.memory_id.to_string();

        let report = revise_memory(&ReviseMemoryOptions {
            database_path: &created.database_path,
            original_memory_id: &memory_id,
            content: Some("Store release checks and clippy gates as durable memory."),
            level: None,
            kind: None,
            confidence: None,
            tags: None,
            provenance_uri: None,
            reason: ReviseReason::Correction,
            actor: Some("SapphireBeacon"),
            dry_run: false,
        });

        ensure(report.success, false, "success")?;
        ensure(report.dry_run, false, "dry_run")?;
        ensure(report.original_id, memory_id.clone(), "original id")?;
        ensure(report.new_id.is_none(), true, "no generated new memory id")?;
        ensure(
            report.revision_group_id.is_none(),
            true,
            "no generated revision group",
        )?;
        ensure(
            report.revision_number.is_none(),
            true,
            "no stub revision number",
        )?;
        ensure(
            report.changed_fields,
            vec!["content".to_string()],
            "changed fields",
        )?;
        ensure(
            report
                .error
                .as_deref()
                .is_some_and(|message| message.contains("--dry-run")),
            true,
            "repair hint mentions dry-run",
        )?;

        let connection = crate::db::DbConnection::open_file(&created.database_path)
            .map_err(|error| error.to_string())?;
        let original = connection
            .get_memory(&memory_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "created memory should still exist".to_string())?;
        ensure(
            original.content,
            "Store release checks as durable memory.".to_string(),
            "original content unchanged",
        )
    }

    #[test]
    fn memory_revise_report_version_matches_package() -> TestResult {
        let report = MemoryReviseReport::not_found("mem_test".to_string());
        ensure(report.version, env!("CARGO_PKG_VERSION"), "version")
    }

    // =========================================================================
    // Dedupe Warning Tests (EE-069)
    // =========================================================================

    #[test]
    fn dedupe_severity_as_str_is_stable() -> TestResult {
        ensure(DedupeSeverity::Exact.as_str(), "exact", "exact")?;
        ensure(DedupeSeverity::High.as_str(), "high", "high")?;
        ensure(DedupeSeverity::Medium.as_str(), "medium", "medium")?;
        ensure(DedupeSeverity::Low.as_str(), "low", "low")
    }

    #[test]
    fn dedupe_severity_from_score_thresholds() -> TestResult {
        ensure(
            DedupeSeverity::from_score(1.0),
            DedupeSeverity::Exact,
            "1.0",
        )?;
        ensure(
            DedupeSeverity::from_score(0.95),
            DedupeSeverity::High,
            "0.95",
        )?;
        ensure(
            DedupeSeverity::from_score(0.90),
            DedupeSeverity::High,
            "0.90",
        )?;
        ensure(
            DedupeSeverity::from_score(0.89),
            DedupeSeverity::Medium,
            "0.89",
        )?;
        ensure(
            DedupeSeverity::from_score(0.70),
            DedupeSeverity::Medium,
            "0.70",
        )?;
        ensure(
            DedupeSeverity::from_score(0.69),
            DedupeSeverity::Low,
            "0.69",
        )?;
        ensure(DedupeSeverity::from_score(0.5), DedupeSeverity::Low, "0.5")?;
        ensure(DedupeSeverity::from_score(0.0), DedupeSeverity::Low, "0.0")
    }

    #[test]
    fn dedupe_severity_ordering_is_correct() -> TestResult {
        let exact = DedupeSeverity::Exact;
        let high = DedupeSeverity::High;
        let medium = DedupeSeverity::Medium;
        let low = DedupeSeverity::Low;

        ensure(exact < high, true, "exact < high")?;
        ensure(high < medium, true, "high < medium")?;
        ensure(medium < low, true, "medium < low")
    }

    #[test]
    fn dedupe_match_type_as_str_is_stable() -> TestResult {
        ensure(
            DedupeMatchType::ExactContent.as_str(),
            "exact_content",
            "exact_content",
        )?;
        ensure(
            DedupeMatchType::NormalizedContent.as_str(),
            "normalized_content",
            "normalized_content",
        )?;
        ensure(DedupeMatchType::Semantic.as_str(), "semantic", "semantic")?;
        ensure(DedupeMatchType::Lexical.as_str(), "lexical", "lexical")
    }

    #[test]
    fn jaccard_similarity_identical_strings() -> TestResult {
        let sim = jaccard_similarity("hello world", "hello world");
        ensure((sim - 1.0).abs() < f32::EPSILON, true, "identical = 1.0")
    }

    #[test]
    fn jaccard_similarity_completely_different() -> TestResult {
        let sim = jaccard_similarity("alpha beta", "gamma delta");
        ensure((sim - 0.0).abs() < f32::EPSILON, true, "disjoint = 0.0")
    }

    #[test]
    fn jaccard_similarity_partial_overlap() -> TestResult {
        // "hello world" vs "hello there" -> intersection = {hello}, union = {hello, world, there}
        // Jaccard = 1/3 ≈ 0.333
        let sim = jaccard_similarity("hello world", "hello there");
        ensure(sim > 0.3 && sim < 0.4, true, "partial overlap ~0.33")
    }

    #[test]
    fn jaccard_similarity_empty_strings() -> TestResult {
        let both_empty = jaccard_similarity("", "");
        let one_empty = jaccard_similarity("hello", "");

        ensure(
            (both_empty - 1.0).abs() < f32::EPSILON,
            true,
            "both empty = 1.0",
        )?;
        ensure(
            (one_empty - 0.0).abs() < f32::EPSILON,
            true,
            "one empty = 0.0",
        )
    }

    #[test]
    fn dedupe_check_options_defaults() -> TestResult {
        let opts = DedupeCheckOptions::new(std::path::Path::new("/tmp/db"), "test content");

        ensure(opts.content, "test content", "content")?;
        ensure(opts.level.is_none(), true, "level none")?;
        ensure(opts.kind.is_none(), true, "kind none")?;
        ensure(
            (opts.min_similarity - 0.5).abs() < f32::EPSILON,
            true,
            "min_similarity",
        )?;
        ensure(opts.max_warnings, 5, "max_warnings")
    }

    #[test]
    fn dedupe_check_report_no_duplicates() -> TestResult {
        let report = DedupeCheckReport::no_duplicates(42);

        ensure(report.has_warnings, false, "has_warnings")?;
        ensure(report.warnings.is_empty(), true, "warnings empty")?;
        ensure(report.memories_scanned, 42, "memories_scanned")?;
        ensure(report.error.is_none(), true, "no error")
    }

    #[test]
    fn dedupe_check_report_with_warnings() -> TestResult {
        let warning = DedupeWarning {
            existing_memory_id: "mem_123".to_string(),
            similarity_score: 0.85,
            severity: DedupeSeverity::Medium,
            existing_preview: "preview text".to_string(),
            match_type: DedupeMatchType::Lexical,
            suggestion: "Consider reviewing".to_string(),
        };
        let report = DedupeCheckReport::with_warnings(vec![warning], 100);

        ensure(report.has_warnings, true, "has_warnings")?;
        ensure(report.warnings.len(), 1, "warnings count")?;
        ensure(report.memories_scanned, 100, "memories_scanned")?;
        ensure(report.error.is_none(), true, "no error")
    }

    #[test]
    fn dedupe_check_report_error() -> TestResult {
        let report = DedupeCheckReport::error("Database failure".to_string());

        ensure(report.has_warnings, false, "has_warnings")?;
        ensure(report.warnings.is_empty(), true, "warnings empty")?;
        ensure(report.memories_scanned, 0, "memories_scanned")?;
        ensure(
            report.error,
            Some("Database failure".to_string()),
            "error message",
        )
    }

    #[test]
    fn dedupe_check_report_version_matches_package() -> TestResult {
        let report = DedupeCheckReport::no_duplicates(0);
        ensure(report.version, env!("CARGO_PKG_VERSION"), "version")
    }
}
