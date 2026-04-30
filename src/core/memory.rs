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
}
