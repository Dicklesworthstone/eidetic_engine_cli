use std::collections::{BTreeSet, VecDeque};
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use serde_json::json;

use crate::db::{DbConnection, DbError, StoredMemory};
use crate::models::{LineSpan, PROVENANCE_HEALTH_SCHEMA_V1, ProvenanceUri};

const MAX_PROVENANCE_FILE_BYTES: u64 = 256 * 1024;
const MAX_MOVED_SCAN_FILES: usize = 512;
const MAX_MOVED_SCAN_BYTES: u64 = 64 * 1024;
const MAX_MOVED_SCAN_TOTAL_BYTES: u64 = 8 * 1024 * 1024;

/// Options for the read-only provenance health diagnostic.
#[derive(Clone, Debug)]
pub struct ProvenanceHealthOptions {
    pub workspace_path: PathBuf,
    pub database_path: Option<PathBuf>,
    pub limit: usize,
}

impl ProvenanceHealthOptions {
    #[must_use]
    pub fn new(workspace_path: PathBuf) -> Self {
        Self {
            workspace_path,
            database_path: None,
            limit: 10_000,
        }
    }
}

/// Error returned by provenance-health diagnostics.
#[derive(Debug)]
pub enum ProvenanceHealthError {
    Storage(DbError),
}

impl fmt::Display for ProvenanceHealthError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Storage(error) => write!(formatter, "{error}"),
        }
    }
}

impl std::error::Error for ProvenanceHealthError {}

impl From<DbError> for ProvenanceHealthError {
    fn from(error: DbError) -> Self {
        Self::Storage(error)
    }
}

/// Workspace-wide provenance freshness diagnostic.
#[derive(Clone, Debug, PartialEq)]
pub struct ProvenanceHealthReport {
    pub schema: &'static str,
    pub workspace_id: String,
    pub summary: ProvenanceHealthSummary,
    pub entries: Vec<MemoryProvenanceHealth>,
    pub degraded: Vec<ProvenanceHealthDegradation>,
}

impl ProvenanceHealthReport {
    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        json!({
            "schema": self.schema,
            "workspaceId": self.workspace_id,
            "summary": self.summary.data_json(),
            "entries": self
                .entries
                .iter()
                .map(MemoryProvenanceHealth::data_json)
                .collect::<Vec<_>>(),
            "degraded": self
                .degraded
                .iter()
                .map(ProvenanceHealthDegradation::data_json)
                .collect::<Vec<_>>(),
        })
    }
}

/// Aggregate counts for provenance classifications.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ProvenanceHealthSummary {
    pub memory_count: usize,
    pub pointer_count: usize,
    pub present_count: usize,
    pub moved_count: usize,
    pub missing_count: usize,
    pub unverifiable_count: usize,
    pub memory_with_issue_count: usize,
}

impl ProvenanceHealthSummary {
    #[must_use]
    fn data_json(&self) -> serde_json::Value {
        json!({
            "memoryCount": self.memory_count,
            "pointerCount": self.pointer_count,
            "presentCount": self.present_count,
            "movedCount": self.moved_count,
            "missingCount": self.missing_count,
            "unverifiableCount": self.unverifiable_count,
            "memoryWithIssueCount": self.memory_with_issue_count,
        })
    }
}

/// Provenance status for one memory.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MemoryProvenanceHealth {
    pub memory_id: String,
    pub health: ProvenancePointerStatus,
    pub pointers: Vec<ProvenancePointerHealth>,
}

impl MemoryProvenanceHealth {
    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        json!({
            "memoryId": self.memory_id,
            "health": self.health.as_str(),
            "pointers": self
                .pointers
                .iter()
                .map(ProvenancePointerHealth::data_json)
                .collect::<Vec<_>>(),
        })
    }
}

/// Status vocabulary required by the trust-freshness provenance bead.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum ProvenancePointerStatus {
    Present,
    Moved,
    Missing,
    Unverifiable,
}

impl ProvenancePointerStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Present => "present",
            Self::Moved => "moved",
            Self::Missing => "missing",
            Self::Unverifiable => "unverifiable",
        }
    }

    #[must_use]
    pub const fn is_issue(self) -> bool {
        !matches!(self, Self::Present)
    }
}

/// Health result for one provenance pointer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProvenancePointerHealth {
    pub uri: String,
    pub scheme: String,
    pub status: ProvenancePointerStatus,
    pub reason_code: &'static str,
    pub checked_path: Option<String>,
    pub moved_to: Option<String>,
    pub detail: String,
}

impl ProvenancePointerHealth {
    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        json!({
            "uri": self.uri,
            "scheme": self.scheme,
            "status": self.status.as_str(),
            "reasonCode": self.reason_code,
            "checkedPath": self.checked_path,
            "movedTo": self.moved_to,
            "detail": self.detail,
        })
    }
}

/// Non-fatal diagnostic degradation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProvenanceHealthDegradation {
    pub code: &'static str,
    pub severity: &'static str,
    pub message: String,
    pub repair: Option<String>,
}

impl ProvenanceHealthDegradation {
    #[must_use]
    fn data_json(&self) -> serde_json::Value {
        json!({
            "code": self.code,
            "severity": self.severity,
            "message": self.message,
            "repair": self.repair,
        })
    }
}

/// Generate a workspace-wide provenance health report.
pub fn generate_provenance_health_report(
    options: ProvenanceHealthOptions,
) -> Result<ProvenanceHealthReport, ProvenanceHealthError> {
    let workspace_root = default_workspace_root(&options.workspace_path);
    let database_path = options
        .database_path
        .unwrap_or_else(|| default_workspace_database_path(&workspace_root));
    let connection = DbConnection::open_file(&database_path)?;
    let workspace_id = resolve_workspace_id(&connection, &workspace_root)?;
    let mut memories = connection.list_memories(&workspace_id, None, false)?;
    memories.truncate(options.limit);
    Ok(build_provenance_health_report(
        workspace_id,
        memories
            .iter()
            .map(|memory| assess_memory_provenance_health(memory, Some(&workspace_root)))
            .collect(),
    ))
}

/// Assess one memory's provenance pointer health.
#[must_use]
pub fn assess_memory_provenance_health(
    memory: &StoredMemory,
    workspace_path: Option<&Path>,
) -> MemoryProvenanceHealth {
    let pointers = match memory.provenance_uri.as_deref() {
        Some(raw) => vec![assess_provenance_uri(raw, &memory.content, workspace_path)],
        None => vec![ProvenancePointerHealth {
            uri: String::new(),
            scheme: "none".to_string(),
            status: ProvenancePointerStatus::Unverifiable,
            reason_code: "no_provenance_uri",
            checked_path: None,
            moved_to: None,
            detail: "Memory has no explicit provenance URI to verify.".to_string(),
        }],
    };
    let health = rollup_pointer_health(&pointers);
    MemoryProvenanceHealth {
        memory_id: memory.id.clone(),
        health,
        pointers,
    }
}

fn build_provenance_health_report(
    workspace_id: String,
    entries: Vec<MemoryProvenanceHealth>,
) -> ProvenanceHealthReport {
    let mut summary = ProvenanceHealthSummary {
        memory_count: entries.len(),
        ..ProvenanceHealthSummary::default()
    };
    for entry in &entries {
        if entry.health.is_issue() {
            summary.memory_with_issue_count += 1;
        }
        for pointer in &entry.pointers {
            summary.pointer_count += 1;
            match pointer.status {
                ProvenancePointerStatus::Present => summary.present_count += 1,
                ProvenancePointerStatus::Moved => summary.moved_count += 1,
                ProvenancePointerStatus::Missing => summary.missing_count += 1,
                ProvenancePointerStatus::Unverifiable => summary.unverifiable_count += 1,
            }
        }
    }
    let degraded = if summary.missing_count > 0 || summary.moved_count > 0 {
        vec![ProvenanceHealthDegradation {
            code: "provenance_health_downgraded",
            severity: "warning",
            message: "One or more memories cite moved or missing provenance; trust should be downgraded until reviewed.".to_string(),
            repair: Some("Run `ee why <memory-id> --json` and revise or re-remember affected memories.".to_string()),
        }]
    } else {
        Vec::new()
    };

    ProvenanceHealthReport {
        schema: PROVENANCE_HEALTH_SCHEMA_V1,
        workspace_id,
        summary,
        entries,
        degraded,
    }
}

fn assess_provenance_uri(
    raw_uri: &str,
    memory_content: &str,
    workspace_path: Option<&Path>,
) -> ProvenancePointerHealth {
    let provenance = match ProvenanceUri::from_str(raw_uri) {
        Ok(provenance) => provenance,
        Err(error) => {
            return ProvenancePointerHealth {
                uri: raw_uri.to_string(),
                scheme: "invalid".to_string(),
                status: ProvenancePointerStatus::Unverifiable,
                reason_code: "invalid_provenance_uri",
                checked_path: None,
                moved_to: None,
                detail: format!("Provenance URI could not be parsed: {error}."),
            };
        }
    };
    let canonical_uri = provenance.to_string();
    let scheme = provenance.scheme().to_string();
    match provenance {
        ProvenanceUri::File { path, span } => {
            assess_file_provenance(&canonical_uri, &path, span, memory_content, workspace_path)
        }
        ProvenanceUri::CassSession { .. } => ProvenancePointerHealth {
            uri: canonical_uri,
            scheme,
            status: ProvenancePointerStatus::Unverifiable,
            reason_code: "cass_verifier_unavailable",
            checked_path: None,
            moved_to: None,
            detail: "CASS session provenance requires a local cass robot verifier; absent verifier is unverifiable, not missing.".to_string(),
        },
        ProvenanceUri::EeMemory(_)
        | ProvenanceUri::Web { .. }
        | ProvenanceUri::AgentMail { .. }
        | ProvenanceUri::External { .. } => ProvenancePointerHealth {
            uri: canonical_uri,
            scheme,
            status: ProvenancePointerStatus::Unverifiable,
            reason_code: "unsupported_provenance_scheme",
            checked_path: None,
            moved_to: None,
            detail: "Provenance scheme is preserved but not locally freshness-checkable."
                .to_string(),
        },
    }
}

fn assess_file_provenance(
    uri: &str,
    raw_path: &str,
    span: Option<LineSpan>,
    memory_content: &str,
    workspace_path: Option<&Path>,
) -> ProvenancePointerHealth {
    let Some(workspace_root) = workspace_path else {
        return ProvenancePointerHealth {
            uri: uri.to_string(),
            scheme: "file".to_string(),
            status: ProvenancePointerStatus::Unverifiable,
            reason_code: "workspace_unavailable",
            checked_path: None,
            moved_to: None,
            detail: "Workspace path is unavailable, so relative file provenance cannot be checked."
                .to_string(),
        };
    };
    let checked_path = resolve_file_path(raw_path, workspace_root);
    let checked_path_display = checked_path.display().to_string();
    let moved_to = || find_moved_file(workspace_root, &checked_path, memory_content);
    match read_limited_utf8(&checked_path, MAX_PROVENANCE_FILE_BYTES) {
        Ok(Some(contents)) => {
            let span_text = span.and_then(|span| extract_line_span(&contents, span));
            let evidence_text = span_text.as_deref().unwrap_or(&contents);
            if text_matches(evidence_text, memory_content) {
                ProvenancePointerHealth {
                    uri: uri.to_string(),
                    scheme: "file".to_string(),
                    status: ProvenancePointerStatus::Present,
                    reason_code: "source_matches",
                    checked_path: Some(checked_path_display.clone()),
                    moved_to: None,
                    detail: format!("File provenance still matches at {checked_path_display}."),
                }
            } else if let Some(found) = moved_to() {
                ProvenancePointerHealth {
                    uri: uri.to_string(),
                    scheme: "file".to_string(),
                    status: ProvenancePointerStatus::Moved,
                    reason_code: "content_found_elsewhere",
                    checked_path: Some(checked_path_display.clone()),
                    moved_to: Some(found.display().to_string()),
                    detail: format!(
                        "Original file exists but no longer matches; remembered evidence was found at {}.",
                        found.display()
                    ),
                }
            } else {
                ProvenancePointerHealth {
                    uri: uri.to_string(),
                    scheme: "file".to_string(),
                    status: ProvenancePointerStatus::Missing,
                    reason_code: if span_text.is_none() && span.is_some() {
                        "line_span_missing"
                    } else {
                        "content_mismatch"
                    },
                    checked_path: Some(checked_path_display.clone()),
                    moved_to: None,
                    detail: format!(
                        "File provenance at {checked_path_display} no longer contains the remembered evidence."
                    ),
                }
            }
        }
        Ok(None) => {
            if let Some(found) = moved_to() {
                ProvenancePointerHealth {
                    uri: uri.to_string(),
                    scheme: "file".to_string(),
                    status: ProvenancePointerStatus::Moved,
                    reason_code: "missing_source_found_elsewhere",
                    checked_path: Some(checked_path_display),
                    moved_to: Some(found.display().to_string()),
                    detail: format!(
                        "Original file is missing; remembered evidence was found at {}.",
                        found.display()
                    ),
                }
            } else {
                ProvenancePointerHealth {
                    uri: uri.to_string(),
                    scheme: "file".to_string(),
                    status: ProvenancePointerStatus::Missing,
                    reason_code: "source_file_missing",
                    checked_path: Some(checked_path_display.clone()),
                    moved_to: None,
                    detail: format!("File provenance source {checked_path_display} is missing."),
                }
            }
        }
        Err(message) => ProvenancePointerHealth {
            uri: uri.to_string(),
            scheme: "file".to_string(),
            status: ProvenancePointerStatus::Unverifiable,
            reason_code: "source_unreadable",
            checked_path: Some(checked_path_display),
            moved_to: None,
            detail: message,
        },
    }
}

fn rollup_pointer_health(pointers: &[ProvenancePointerHealth]) -> ProvenancePointerStatus {
    if pointers
        .iter()
        .any(|pointer| pointer.status == ProvenancePointerStatus::Missing)
    {
        ProvenancePointerStatus::Missing
    } else if pointers
        .iter()
        .any(|pointer| pointer.status == ProvenancePointerStatus::Moved)
    {
        ProvenancePointerStatus::Moved
    } else if pointers
        .iter()
        .any(|pointer| pointer.status == ProvenancePointerStatus::Unverifiable)
    {
        ProvenancePointerStatus::Unverifiable
    } else {
        ProvenancePointerStatus::Present
    }
}

fn resolve_file_path(raw_path: &str, workspace_root: &Path) -> PathBuf {
    let path = PathBuf::from(raw_path);
    if path.is_absolute() {
        path
    } else {
        workspace_root.join(path)
    }
}

fn read_limited_utf8(path: &Path, max_bytes: u64) -> Result<Option<String>, String> {
    // bd-6i8gq: reject ANY symlinked path component (parent or leaf), not just a
    // symlinked final file. fs::symlink_metadata only declines to follow the
    // LAST component, so a symlinked PARENT directory was still traversed and its
    // target read as if it were the workspace evidence file. Mirrors the
    // memory-freshness contract, which rejects any symlinked provenance component.
    if let Some(symlink_path) = crate::core::path_safety::first_existing_symlink_component(path)
        .map_err(|error| format!("Could not check provenance source for symlinks: {error}"))?
    {
        return Err(format!(
            "Refusing to follow symlinked provenance path component {}.",
            symlink_path.display()
        ));
    }
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("Could not stat provenance source: {error}")),
    };
    if metadata.file_type().is_symlink() {
        return Err("Refusing to follow symlinked provenance source.".to_string());
    }
    if !metadata.is_file() {
        return Err("Provenance source is not a regular file.".to_string());
    }
    let file = open_provenance_source_no_follow(path)?;
    let opened_metadata = file
        .metadata()
        .map_err(|error| format!("Could not stat opened provenance source: {error}"))?;
    if !opened_metadata.file_type().is_file() {
        return Err("Provenance source is not a regular file after open.".to_string());
    }
    let mut reader = file.take(max_bytes.saturating_add(1));
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes).map_err(|error| {
        format!(
            "Could not read provenance source {}: {error}",
            path.display()
        )
    })?;
    if bytes.len() as u64 > max_bytes {
        return Err(format!(
            "Provenance source {} exceeds the {} byte read cap.",
            path.display(),
            max_bytes
        ));
    }
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|error| format!("Provenance source {} is not UTF-8: {error}", path.display()))
}

fn open_provenance_source_no_follow(path: &Path) -> Result<fs::File, String> {
    let mut options = OpenOptions::new();
    options.read(true);
    configure_provenance_source_open_options(&mut options);
    options.open(path).map_err(|error| {
        format!(
            "Could not read provenance source {}: {error}",
            path.display()
        )
    })
}

#[cfg(all(unix, not(any(target_os = "espidf", target_os = "horizon"))))]
fn configure_provenance_source_open_options(options: &mut OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt;

    options.custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32);
}

#[cfg(not(all(unix, not(any(target_os = "espidf", target_os = "horizon")))))]
fn configure_provenance_source_open_options(_options: &mut OpenOptions) {}

fn extract_line_span(contents: &str, span: LineSpan) -> Option<String> {
    let start = usize::try_from(span.start).ok()?.checked_sub(1)?;
    let end = usize::try_from(span.end.unwrap_or(span.start)).ok()?;
    let lines = contents.lines().collect::<Vec<_>>();
    if start >= lines.len() || end > lines.len() || end <= start {
        return None;
    }
    Some(lines[start..end].join("\n"))
}

fn text_matches(source_text: &str, memory_content: &str) -> bool {
    let source = normalize_text(source_text);
    let memory = normalize_text(memory_content);
    if source.is_empty() || memory.is_empty() {
        return false;
    }
    source.contains(&memory) || memory.contains(&source)
}

fn normalize_text(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn find_moved_file(
    workspace_root: &Path,
    original_path: &Path,
    memory_content: &str,
) -> Option<PathBuf> {
    let needle = normalize_text(memory_content);
    if needle.chars().count() < 12 {
        return None;
    }
    let mut queue = VecDeque::from([workspace_root.to_path_buf()]);
    let mut visited = BTreeSet::new();
    let mut scanned_files = 0_usize;
    let mut scanned_bytes = 0_u64;
    while let Some(dir) = queue.pop_front() {
        if !visited.insert(dir.clone()) {
            continue;
        }
        let Ok(read_dir) = fs::read_dir(&dir) else {
            continue;
        };
        let mut entries = read_dir.filter_map(Result::ok).collect::<Vec<_>>();
        entries.sort_by_key(|entry| entry.path());
        for entry in entries {
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if should_skip_scan_entry(&name) {
                continue;
            }
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_symlink() {
                continue;
            }
            if file_type.is_dir() {
                queue.push_back(path);
                continue;
            }
            if !file_type.is_file() || path == original_path {
                continue;
            }
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            if metadata.len() > MAX_MOVED_SCAN_BYTES {
                continue;
            }
            scanned_files = scanned_files.saturating_add(1);
            scanned_bytes = scanned_bytes.saturating_add(metadata.len());
            if scanned_files > MAX_MOVED_SCAN_FILES || scanned_bytes > MAX_MOVED_SCAN_TOTAL_BYTES {
                return None;
            }
            let Ok(Some(contents)) = read_limited_utf8(&path, MAX_MOVED_SCAN_BYTES) else {
                continue;
            };
            if normalize_text(&contents).contains(&needle) {
                return Some(path);
            }
        }
    }
    None
}

fn should_skip_scan_entry(name: &str) -> bool {
    matches!(
        name,
        ".git" | ".ee" | "target" | "node_modules" | ".next" | "dist" | "vendor"
    )
}

fn default_workspace_root(workspace_path: &Path) -> PathBuf {
    crate::config::workspace::canonical_workspace_root_or_lexical(workspace_path)
}

fn default_workspace_database_path(workspace_path: &Path) -> PathBuf {
    default_workspace_root(workspace_path)
        .join(".ee")
        .join("ee.db")
}

fn resolve_workspace_id(
    connection: &DbConnection,
    workspace_path: &Path,
) -> Result<String, DbError> {
    let requested = crate::core::curate::stable_workspace_id(workspace_path);
    if let Ok(Some(workspace)) = crate::core::workspace::select_existing_workspace_row(
        connection,
        &requested,
        &[workspace_path],
    ) {
        return Ok(workspace.id);
    }
    Ok(connection
        .get_workspace_by_path(&workspace_path.to_string_lossy())?
        .map(|workspace| workspace.id)
        .unwrap_or(requested))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn memory(id: &str, content: &str, provenance_uri: Option<String>) -> StoredMemory {
        StoredMemory {
            id: id.to_string(),
            workspace_id: "wsp_test".to_string(),
            level: "procedural".to_string(),
            kind: "rule".to_string(),
            content: content.to_string(),
            workflow_id: None,
            confidence: 0.8,
            utility: 0.7,
            importance: 0.6,
            provenance_uri,
            trust_class: "human_explicit".to_string(),
            trust_subclass: None,
            provenance_chain_hash: None,
            provenance_chain_hash_version: "v1".to_string(),
            provenance_verification_status: "unchecked".to_string(),
            provenance_verified_at: None,
            provenance_verification_note: None,
            created_at: "2026-06-18T00:00:00Z".to_string(),
            updated_at: "2026-06-18T00:00:00Z".to_string(),
            tombstoned_at: None,
            valid_from: None,
            valid_to: None,
        }
    }

    #[test]
    fn classifies_present_missing_moved_and_cass_unverifiable() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        fs::write(
            temp.path().join("present.md"),
            "release workflow evidence stays here\n",
        )
        .map_err(|error| error.to_string())?;
        fs::create_dir(temp.path().join("docs")).map_err(|error| error.to_string())?;
        fs::write(
            temp.path().join("docs").join("moved.md"),
            "moved provenance evidence survives here\n",
        )
        .map_err(|error| error.to_string())?;

        let present = assess_memory_provenance_health(
            &memory(
                "mem_present",
                "release workflow evidence stays here",
                Some("file://present.md#L1".to_string()),
            ),
            Some(temp.path()),
        );
        assert_eq!(present.health, ProvenancePointerStatus::Present);

        let moved = assess_memory_provenance_health(
            &memory(
                "mem_moved",
                "moved provenance evidence survives here",
                Some("file://old-place.md#L1".to_string()),
            ),
            Some(temp.path()),
        );
        assert_eq!(moved.health, ProvenancePointerStatus::Moved);
        assert_eq!(
            moved.pointers[0].reason_code,
            "missing_source_found_elsewhere"
        );

        let missing = assess_memory_provenance_health(
            &memory(
                "mem_missing",
                "evidence that cannot be found anywhere",
                Some("file://missing.md#L1".to_string()),
            ),
            Some(temp.path()),
        );
        assert_eq!(missing.health, ProvenancePointerStatus::Missing);

        let cass = assess_memory_provenance_health(
            &memory(
                "mem_cass",
                "cass-backed evidence",
                Some("cass-session://session-a#L1".to_string()),
            ),
            Some(temp.path()),
        );
        assert_eq!(cass.health, ProvenancePointerStatus::Unverifiable);
        assert_eq!(cass.pointers[0].reason_code, "cass_verifier_unavailable");
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn moved_scan_does_not_follow_symlinked_workspace_directory() -> Result<(), String> {
        use std::os::unix::fs::symlink;

        let workspace = tempfile::tempdir().map_err(|error| error.to_string())?;
        let outside = tempfile::tempdir().map_err(|error| error.to_string())?;
        fs::write(
            outside.path().join("outside.md"),
            "symlink-only provenance evidence lives outside workspace\n",
        )
        .map_err(|error| error.to_string())?;
        symlink(outside.path(), workspace.path().join("linked-outside"))
            .map_err(|error| error.to_string())?;

        let health = assess_memory_provenance_health(
            &memory(
                "mem_symlink_escape",
                "symlink-only provenance evidence lives outside workspace",
                Some("file://missing.md#L1".to_string()),
            ),
            Some(workspace.path()),
        );

        assert_eq!(health.health, ProvenancePointerStatus::Missing);
        assert_eq!(health.pointers[0].reason_code, "source_file_missing");
        assert_eq!(health.pointers[0].moved_to, None);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn file_provenance_under_symlinked_parent_is_refused_bd_6i8gq() -> Result<(), String> {
        use std::os::unix::fs::symlink;

        let workspace = tempfile::tempdir().map_err(|error| error.to_string())?;
        let outside = tempfile::tempdir().map_err(|error| error.to_string())?;
        let evidence = "provenance evidence reachable only via a symlinked parent directory";
        fs::write(outside.path().join("evidence.md"), format!("{evidence}\n"))
            .map_err(|error| error.to_string())?;
        // workspace/linked-dir is a symlink to `outside`; evidence.md itself is a
        // regular file, so the leaf-only is_symlink check would miss it.
        symlink(outside.path(), workspace.path().join("linked-dir"))
            .map_err(|error| error.to_string())?;

        let health = assess_memory_provenance_health(
            &memory(
                "mem_symlinked_parent",
                evidence,
                Some("file://linked-dir/evidence.md#L1".to_string()),
            ),
            Some(workspace.path()),
        );

        assert_eq!(health.health, ProvenancePointerStatus::Unverifiable);
        assert_eq!(
            health.pointers[0].status,
            ProvenancePointerStatus::Unverifiable
        );
        assert_eq!(health.pointers[0].reason_code, "source_unreadable");
        Ok(())
    }

    #[test]
    fn classifies_invalid_uri_and_missing_workspace_as_unverifiable() {
        let invalid = assess_memory_provenance_health(
            &memory(
                "mem_invalid",
                "invalid provenance",
                Some("not a valid provenance uri".to_string()),
            ),
            Some(Path::new(".")),
        );
        assert_eq!(invalid.health, ProvenancePointerStatus::Unverifiable);
        assert_eq!(invalid.pointers[0].scheme, "invalid");
        assert_eq!(invalid.pointers[0].reason_code, "invalid_provenance_uri");

        let no_workspace = assess_memory_provenance_health(
            &memory(
                "mem_no_workspace",
                "workspace is required for relative file provenance",
                Some("file://src/lib.rs#L1".to_string()),
            ),
            None,
        );
        assert_eq!(no_workspace.health, ProvenancePointerStatus::Unverifiable);
        assert_eq!(no_workspace.pointers[0].scheme, "file");
        assert_eq!(
            no_workspace.pointers[0].reason_code,
            "workspace_unavailable"
        );
    }

    #[test]
    fn report_rolls_up_counts_deterministically() {
        let report = build_provenance_health_report(
            "wsp_test".to_string(),
            vec![
                MemoryProvenanceHealth {
                    memory_id: "mem_a".to_string(),
                    health: ProvenancePointerStatus::Present,
                    pointers: vec![ProvenancePointerHealth {
                        uri: "file://a.md".to_string(),
                        scheme: "file".to_string(),
                        status: ProvenancePointerStatus::Present,
                        reason_code: "source_matches",
                        checked_path: None,
                        moved_to: None,
                        detail: "ok".to_string(),
                    }],
                },
                MemoryProvenanceHealth {
                    memory_id: "mem_b".to_string(),
                    health: ProvenancePointerStatus::Missing,
                    pointers: vec![ProvenancePointerHealth {
                        uri: "file://b.md".to_string(),
                        scheme: "file".to_string(),
                        status: ProvenancePointerStatus::Missing,
                        reason_code: "source_file_missing",
                        checked_path: None,
                        moved_to: None,
                        detail: "missing".to_string(),
                    }],
                },
            ],
        );

        assert_eq!(report.summary.memory_count, 2);
        assert_eq!(report.summary.present_count, 1);
        assert_eq!(report.summary.missing_count, 1);
        assert_eq!(report.summary.memory_with_issue_count, 1);
        assert_eq!(report.degraded[0].code, "provenance_health_downgraded");
    }
}
