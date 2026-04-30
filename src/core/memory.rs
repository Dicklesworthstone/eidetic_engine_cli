//! Memory retrieval and inspection operations (EE-063).
//!
//! Provides the core use case functions for inspecting stored memories:
//! - `get_memory_details`: retrieve a single memory with its tags and metadata

use std::path::Path;

use crate::db::{DbConnection, StoredMemory};

/// A memory with its associated tags for display.
#[derive(Clone, Debug, PartialEq)]
pub struct MemoryDetails {
    /// The stored memory record.
    pub memory: StoredMemory,
    /// Tags associated with this memory.
    pub tags: Vec<String>,
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

    // Get workspace ID - for now use default
    let workspace_id = "default";

    // If filtering by tag, get memory IDs first
    let memory_ids: Option<Vec<String>> = if let Some(tag) = options.tag {
        match conn.list_memories_by_tag(workspace_id, tag) {
            Ok(ids) => Some(ids),
            Err(e) => return MemoryListReport::error(format!("Failed to query by tag: {e}")),
        }
    } else {
        None
    };

    // Get memories
    let stored = match conn.list_memories(workspace_id, options.level, options.include_tombstoned) {
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
        .map(|m| MemorySummary {
            id: m.id,
            level: m.level,
            kind: m.kind,
            content_preview: truncate_content(&m.content),
            confidence: m.confidence,
            provenance_uri: m.provenance_uri,
            is_tombstoned: m.tombstoned_at.is_some(),
            created_at: m.created_at,
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
}
