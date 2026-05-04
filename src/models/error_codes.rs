//! Error Code Registry (EE-035)
//!
//! Stable error codes for programmatic error handling. Each error has:
//! - A stable identifier (e.g., EE-E001)
//! - A category matching DomainError variants
//! - A human-readable description
//! - A default repair command when applicable

/// Stable error code with associated metadata.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ErrorCode {
    /// Stable identifier, e.g., "EE-E001"
    pub id: &'static str,
    /// Category matching DomainError variant
    pub category: ErrorCategory,
    /// Human-readable description
    pub description: &'static str,
    /// Default repair command, if applicable
    pub default_repair: Option<&'static str>,
}

impl ErrorCode {
    /// Returns the numeric portion of the error code (e.g., 1 for EE-E001).
    #[must_use]
    pub const fn number(&self) -> u16 {
        // Parse EE-EXXX format
        let bytes = self.id.as_bytes();
        if bytes.len() >= 6 {
            let d1 = (bytes[4] as u16).wrapping_sub(b'0' as u16);
            let d2 = (bytes[5] as u16).wrapping_sub(b'0' as u16);
            let d3 = if bytes.len() > 6 {
                (bytes[6] as u16).wrapping_sub(b'0' as u16)
            } else {
                0
            };
            d1 * 100 + d2 * 10 + d3
        } else {
            0
        }
    }
}

/// Error categories matching DomainError variants.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ErrorCategory {
    Usage,
    Configuration,
    Storage,
    SearchIndex,
    Import,
    UnsatisfiedDegradedMode,
    PolicyDenied,
    MigrationRequired,
}

impl ErrorCategory {
    /// Returns the wire name used in JSON output.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Usage => "usage",
            Self::Configuration => "configuration",
            Self::Storage => "storage",
            Self::SearchIndex => "search_index",
            Self::Import => "import",
            Self::UnsatisfiedDegradedMode => "unsatisfied_degraded_mode",
            Self::PolicyDenied => "policy_denied",
            Self::MigrationRequired => "migration_required",
        }
    }
}

// ============================================================================
// Error Code Registry
//
// Codes are grouped by category. Each code is stable and should not be reused
// once assigned. Add new codes at the end of each category section.
// ============================================================================

// Usage errors (EE-E001 - EE-E099)
pub const UNKNOWN_COMMAND: ErrorCode = ErrorCode {
    id: "EE-E001",
    category: ErrorCategory::Usage,
    description: "Unknown command or subcommand",
    default_repair: Some("ee --help"),
};

pub const INVALID_ARGUMENT: ErrorCode = ErrorCode {
    id: "EE-E002",
    category: ErrorCategory::Usage,
    description: "Invalid argument value",
    default_repair: Some("ee --help"),
};

pub const MISSING_REQUIRED_ARG: ErrorCode = ErrorCode {
    id: "EE-E003",
    category: ErrorCategory::Usage,
    description: "Required argument missing",
    default_repair: Some("ee --help"),
};

pub const WORKSPACE_NOT_SPECIFIED: ErrorCode = ErrorCode {
    id: "EE-E004",
    category: ErrorCategory::Usage,
    description: "Workspace path required but not specified",
    default_repair: Some("ee --workspace . <command>"),
};

// Configuration errors (EE-E100 - EE-E199)
pub const CONFIG_FILE_NOT_FOUND: ErrorCode = ErrorCode {
    id: "EE-E100",
    category: ErrorCategory::Configuration,
    description: "Configuration file not found",
    default_repair: Some("ee init --workspace ."),
};

pub const CONFIG_PARSE_ERROR: ErrorCode = ErrorCode {
    id: "EE-E101",
    category: ErrorCategory::Configuration,
    description: "Failed to parse configuration file",
    default_repair: Some("ee doctor --fix-plan --json"),
};

pub const CONFIG_INVALID_VALUE: ErrorCode = ErrorCode {
    id: "EE-E102",
    category: ErrorCategory::Configuration,
    description: "Invalid configuration value",
    default_repair: Some("ee doctor --fix-plan --json"),
};

// Storage errors (EE-E200 - EE-E299)
pub const DATABASE_NOT_FOUND: ErrorCode = ErrorCode {
    id: "EE-E200",
    category: ErrorCategory::Storage,
    description: "Database file not found",
    default_repair: Some("ee init --workspace ."),
};

pub const DATABASE_LOCKED: ErrorCode = ErrorCode {
    id: "EE-E201",
    category: ErrorCategory::Storage,
    description: "Database is locked by another process",
    default_repair: None,
};

pub const DATABASE_CORRUPTED: ErrorCode = ErrorCode {
    id: "EE-E202",
    category: ErrorCategory::Storage,
    description: "Database file is corrupted",
    default_repair: Some("ee doctor --fix-plan --json"),
};

pub const WRITE_FAILED: ErrorCode = ErrorCode {
    id: "EE-E203",
    category: ErrorCategory::Storage,
    description: "Failed to write to database",
    default_repair: None,
};

// Search index errors (EE-E300 - EE-E399)
pub const INDEX_NOT_FOUND: ErrorCode = ErrorCode {
    id: "EE-E300",
    category: ErrorCategory::SearchIndex,
    description: "Search index not found",
    default_repair: Some("ee index rebuild"),
};

pub const INDEX_STALE: ErrorCode = ErrorCode {
    id: "EE-E301",
    category: ErrorCategory::SearchIndex,
    description: "Search index is out of sync with database",
    default_repair: Some("ee index rebuild"),
};

pub const INDEX_CORRUPTED: ErrorCode = ErrorCode {
    id: "EE-E302",
    category: ErrorCategory::SearchIndex,
    description: "Search index is corrupted",
    default_repair: Some("ee index rebuild"),
};

pub const EMBEDDING_UNAVAILABLE: ErrorCode = ErrorCode {
    id: "EE-E303",
    category: ErrorCategory::SearchIndex,
    description: "Embedding model not available",
    default_repair: Some("ee index reembed --dry-run"),
};

// Import errors (EE-E400 - EE-E499)
pub const IMPORT_SOURCE_NOT_FOUND: ErrorCode = ErrorCode {
    id: "EE-E400",
    category: ErrorCategory::Import,
    description: "Import source file or directory not found",
    default_repair: None,
};

pub const IMPORT_FORMAT_ERROR: ErrorCode = ErrorCode {
    id: "EE-E401",
    category: ErrorCategory::Import,
    description: "Unrecognized import format",
    default_repair: Some("ee import jsonl --source <file> --dry-run"),
};

pub const IMPORT_DUPLICATE: ErrorCode = ErrorCode {
    id: "EE-E402",
    category: ErrorCategory::Import,
    description: "Import would create duplicate entries",
    default_repair: Some("ee import jsonl --source <file> --dry-run"),
};

// Degraded mode errors (EE-E500 - EE-E599)
pub const REQUIRED_CAPABILITY_UNAVAILABLE: ErrorCode = ErrorCode {
    id: "EE-E500",
    category: ErrorCategory::UnsatisfiedDegradedMode,
    description: "Required capability is not available",
    default_repair: None,
};

pub const SEMANTIC_SEARCH_REQUIRED: ErrorCode = ErrorCode {
    id: "EE-E501",
    category: ErrorCategory::UnsatisfiedDegradedMode,
    description: "Semantic search required but unavailable",
    default_repair: Some("ee index reembed --dry-run"),
};

pub const UNKNOWN_AGENT_CONNECTOR: ErrorCode = ErrorCode {
    id: "EE-E502",
    category: ErrorCategory::UnsatisfiedDegradedMode,
    description: "Agent connector type not recognized",
    default_repair: Some("ee agent detect --json"),
};

pub const AGENT_DETECTOR_UNAVAILABLE: ErrorCode = ErrorCode {
    id: "EE-E503",
    category: ErrorCategory::UnsatisfiedDegradedMode,
    description: "Agent detector subsystem is unavailable",
    default_repair: Some("ee doctor --json"),
};

pub const AGENT_SOURCE_NOT_IMPORTED: ErrorCode = ErrorCode {
    id: "EE-E504",
    category: ErrorCategory::UnsatisfiedDegradedMode,
    description: "Agent source detected but not yet imported",
    default_repair: Some("ee import cass --dry-run --json"),
};

// Policy errors (EE-E600 - EE-E699)
pub const REDACTION_BLOCKED: ErrorCode = ErrorCode {
    id: "EE-E600",
    category: ErrorCategory::PolicyDenied,
    description: "Operation blocked by redaction policy",
    default_repair: Some("ee doctor --fix-plan --json"),
};

pub const RETENTION_BLOCKED: ErrorCode = ErrorCode {
    id: "EE-E601",
    category: ErrorCategory::PolicyDenied,
    description: "Operation blocked by retention policy",
    default_repair: Some("ee doctor --fix-plan --json"),
};

pub const SCOPE_VIOLATION: ErrorCode = ErrorCode {
    id: "EE-E602",
    category: ErrorCategory::PolicyDenied,
    description: "Operation violates scope boundaries",
    default_repair: None,
};

// Migration errors (EE-E700 - EE-E799)
pub const MIGRATION_REQUIRED: ErrorCode = ErrorCode {
    id: "EE-E700",
    category: ErrorCategory::MigrationRequired,
    description: "Database schema requires migration",
    default_repair: Some("ee init --workspace ."),
};

pub const MIGRATION_FAILED: ErrorCode = ErrorCode {
    id: "EE-E701",
    category: ErrorCategory::MigrationRequired,
    description: "Database migration failed",
    default_repair: Some("ee init --workspace . --repair-plan --json"),
};

pub const RUNTIME_UNAVAILABLE: ErrorCode = ErrorCode {
    id: "EE-E505",
    category: ErrorCategory::UnsatisfiedDegradedMode,
    description: "Asupersync runtime initialization failed",
    default_repair: None,
};

pub const CASS_NOT_FOUND: ErrorCode = ErrorCode {
    id: "EE-E506",
    category: ErrorCategory::UnsatisfiedDegradedMode,
    description: "CASS binary not found in PATH",
    default_repair: Some("Ensure cass is installed and in PATH"),
};

pub const CASS_DEGRADED: ErrorCode = ErrorCode {
    id: "EE-E507",
    category: ErrorCategory::UnsatisfiedDegradedMode,
    description: "CASS binary found but capabilities are limited",
    default_repair: Some("cass health"),
};

pub const CASS_UNAVAILABLE: ErrorCode = ErrorCode {
    id: "EE-E508",
    category: ErrorCategory::UnsatisfiedDegradedMode,
    description: "CASS integration is unavailable",
    default_repair: Some("Check cass installation"),
};

/// All registered error codes for enumeration.
pub const ALL_ERROR_CODES: &[ErrorCode] = &[
    // Usage
    UNKNOWN_COMMAND,
    INVALID_ARGUMENT,
    MISSING_REQUIRED_ARG,
    WORKSPACE_NOT_SPECIFIED,
    // Configuration
    CONFIG_FILE_NOT_FOUND,
    CONFIG_PARSE_ERROR,
    CONFIG_INVALID_VALUE,
    // Storage
    DATABASE_NOT_FOUND,
    DATABASE_LOCKED,
    DATABASE_CORRUPTED,
    WRITE_FAILED,
    // Search index
    INDEX_NOT_FOUND,
    INDEX_STALE,
    INDEX_CORRUPTED,
    EMBEDDING_UNAVAILABLE,
    // Import
    IMPORT_SOURCE_NOT_FOUND,
    IMPORT_FORMAT_ERROR,
    IMPORT_DUPLICATE,
    // Degraded mode
    REQUIRED_CAPABILITY_UNAVAILABLE,
    SEMANTIC_SEARCH_REQUIRED,
    UNKNOWN_AGENT_CONNECTOR,
    AGENT_DETECTOR_UNAVAILABLE,
    AGENT_SOURCE_NOT_IMPORTED,
    // Policy
    REDACTION_BLOCKED,
    RETENTION_BLOCKED,
    SCOPE_VIOLATION,
    // Migration
    MIGRATION_REQUIRED,
    MIGRATION_FAILED,
    // Runtime and CASS
    RUNTIME_UNAVAILABLE,
    CASS_NOT_FOUND,
    CASS_DEGRADED,
    CASS_UNAVAILABLE,
];

/// Look up an error code by its stable ID (e.g., "EE-E001").
#[must_use]
pub fn lookup(id: &str) -> Option<ErrorCode> {
    ALL_ERROR_CODES.iter().find(|code| code.id == id).copied()
}

/// Look up error codes by category.
#[must_use]
pub fn by_category(category: ErrorCategory) -> Vec<ErrorCode> {
    ALL_ERROR_CODES
        .iter()
        .filter(|code| code.category == category)
        .copied()
        .collect()
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
    fn error_code_ids_are_unique() -> TestResult {
        let mut seen = std::collections::HashSet::new();
        for code in ALL_ERROR_CODES {
            if !seen.insert(code.id) {
                return Err(format!("Duplicate error code ID: {}", code.id));
            }
        }
        Ok(())
    }

    #[test]
    fn error_code_ids_follow_format() -> TestResult {
        for code in ALL_ERROR_CODES {
            if !code.id.starts_with("EE-E") {
                return Err(format!("Code {} does not start with EE-E", code.id));
            }
            if code.id.len() < 7 {
                return Err(format!("Code {} is too short", code.id));
            }
        }
        Ok(())
    }

    #[test]
    fn error_code_numbers_are_in_range() -> TestResult {
        for code in ALL_ERROR_CODES {
            let num = code.number();
            let expected_range = match code.category {
                ErrorCategory::Usage => 1..100,
                ErrorCategory::Configuration => 100..200,
                ErrorCategory::Storage => 200..300,
                ErrorCategory::SearchIndex => 300..400,
                ErrorCategory::Import => 400..500,
                ErrorCategory::UnsatisfiedDegradedMode => 500..600,
                ErrorCategory::PolicyDenied => 600..700,
                ErrorCategory::MigrationRequired => 700..800,
            };
            if !expected_range.contains(&num) {
                return Err(format!(
                    "Code {} has number {} outside range {:?}",
                    code.id, num, expected_range
                ));
            }
        }
        Ok(())
    }

    #[test]
    fn lookup_finds_existing_code() -> TestResult {
        let found = lookup("EE-E001");
        ensure(found.is_some(), true, "EE-E001 exists")?;
        ensure(found.map(|c| c.id), Some("EE-E001"), "found correct code")
    }

    #[test]
    fn lookup_returns_none_for_unknown() -> TestResult {
        ensure(lookup("EE-E999"), None, "unknown code returns None")
    }

    #[test]
    fn by_category_returns_correct_codes() -> TestResult {
        let usage = by_category(ErrorCategory::Usage);
        ensure(usage.len() >= 4, true, "at least 4 usage codes")?;
        for code in &usage {
            ensure(code.category, ErrorCategory::Usage, "correct category")?;
        }
        Ok(())
    }

    #[test]
    fn category_as_str_matches_domain_error_codes() -> TestResult {
        ensure(ErrorCategory::Usage.as_str(), "usage", "usage")?;
        ensure(ErrorCategory::Storage.as_str(), "storage", "storage")?;
        ensure(
            ErrorCategory::SearchIndex.as_str(),
            "search_index",
            "search_index",
        )?;
        ensure(
            ErrorCategory::MigrationRequired.as_str(),
            "migration_required",
            "migration",
        )
    }

    #[test]
    fn all_categories_have_at_least_one_code() -> TestResult {
        let categories = [
            ErrorCategory::Usage,
            ErrorCategory::Configuration,
            ErrorCategory::Storage,
            ErrorCategory::SearchIndex,
            ErrorCategory::Import,
            ErrorCategory::UnsatisfiedDegradedMode,
            ErrorCategory::PolicyDenied,
            ErrorCategory::MigrationRequired,
        ];
        for cat in categories {
            let codes = by_category(cat);
            if codes.is_empty() {
                return Err(format!("Category {:?} has no codes", cat));
            }
        }
        Ok(())
    }
}
