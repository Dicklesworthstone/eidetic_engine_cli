//! Stable degradation codes (EE-240).
//!
//! Degradation codes describe conditions where `ee` continues to operate
//! but with reduced capabilities. Unlike error codes (which halt commands),
//! degradation codes explain *what's missing* and *how behavior changes*.
//!
//! Each degradation has:
//! - A stable identifier (e.g., `D001`)
//! - A subsystem that's affected
//! - A description of the degraded behavior
//! - Severity level (advisory, warning, critical)
//! - Whether it's recoverable without user intervention
//!
//! These codes appear in:
//! - `ee status --json` under the `degraded[]` array
//! - The `_meta.degraded` field in any response envelope
//! - Diagnostic output from `ee doctor`

use std::{convert::Infallible, fmt};

/// Degradation severity levels.
///
/// Determines UI treatment and whether the condition should block agents.
#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
pub enum DegradationSeverity {
    /// Informational - operation continues normally with minor limitations.
    #[default]
    Advisory,
    /// Warning - some features unavailable but core functionality works.
    Warning,
    /// Critical - significant functionality impaired; user action advised.
    Critical,
}

impl DegradationSeverity {
    /// Stable string representation for JSON output.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Advisory => "advisory",
            Self::Warning => "warning",
            Self::Critical => "critical",
        }
    }

    /// Parse from string.
    #[must_use]
    pub fn parse_lossy(s: &str) -> Self {
        match s {
            "advisory" => Self::Advisory,
            "warning" => Self::Warning,
            "critical" => Self::Critical,
            _ => Self::Advisory,
        }
    }
}

impl std::str::FromStr for DegradationSeverity {
    type Err = Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self::parse_lossy(s))
    }
}

impl fmt::Display for DegradationSeverity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Subsystem affected by degradation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DegradedSubsystem {
    /// Search and retrieval (Frankensearch).
    Search,
    /// Database storage (FrankenSQLite).
    Storage,
    /// CASS session import.
    Cass,
    /// Graph analytics (FrankenNetworkX).
    Graph,
    /// Context packing.
    Pack,
    /// Memory curation.
    Curate,
    /// Policy enforcement.
    Policy,
    /// External network access.
    Network,
}

impl DegradedSubsystem {
    /// Stable string representation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Search => "search",
            Self::Storage => "storage",
            Self::Cass => "cass",
            Self::Graph => "graph",
            Self::Pack => "pack",
            Self::Curate => "curate",
            Self::Policy => "policy",
            Self::Network => "network",
        }
    }
}

impl fmt::Display for DegradedSubsystem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A stable degradation code.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DegradationCode {
    /// Stable identifier, e.g., "D001"
    pub id: &'static str,
    /// Affected subsystem
    pub subsystem: DegradedSubsystem,
    /// Severity level
    pub severity: DegradationSeverity,
    /// Human-readable description
    pub description: &'static str,
    /// What behavior changes in this degraded state
    pub behavior_change: &'static str,
    /// Whether auto-recovery is possible
    pub auto_recoverable: bool,
    /// Suggested repair command, if applicable
    pub repair: Option<&'static str>,
}

impl DegradationCode {
    /// Numeric portion of the code (e.g., 1 for D001).
    #[must_use]
    pub const fn number(&self) -> u16 {
        let bytes = self.id.as_bytes();
        if bytes.len() >= 4 {
            let d1 = (bytes[1] as u16).wrapping_sub(b'0' as u16);
            let d2 = (bytes[2] as u16).wrapping_sub(b'0' as u16);
            let d3 = (bytes[3] as u16).wrapping_sub(b'0' as u16);
            d1 * 100 + d2 * 10 + d3
        } else {
            0
        }
    }
}

// ============================================================================
// Degradation Code Registry
//
// Codes are grouped by subsystem. Each code is stable and should not be
// reused once assigned. Add new codes at the end of each section.
// ============================================================================

// Search degradations (D001 - D099)
pub const SEMANTIC_SEARCH_UNAVAILABLE: DegradationCode = DegradationCode {
    id: "D001",
    subsystem: DegradedSubsystem::Search,
    severity: DegradationSeverity::Warning,
    description: "Semantic search unavailable",
    behavior_change: "Falling back to lexical (BM25) search only",
    auto_recoverable: true,
    repair: Some("ee search --lexical-only"),
};

pub const EMBEDDING_MODEL_MISSING: DegradationCode = DegradationCode {
    id: "D002",
    subsystem: DegradedSubsystem::Search,
    severity: DegradationSeverity::Warning,
    description: "Embedding model not loaded",
    behavior_change: "Semantic similarity disabled; lexical matching only",
    auto_recoverable: false,
    repair: Some("ee index rebuild"),
};

pub const SEARCH_INDEX_STALE: DegradationCode = DegradationCode {
    id: "D003",
    subsystem: DegradedSubsystem::Search,
    severity: DegradationSeverity::Advisory,
    description: "Search index is behind database",
    behavior_change: "Recent memories may not appear in search results",
    auto_recoverable: true,
    repair: Some("ee index rebuild"),
};

pub const FTS5_UNAVAILABLE: DegradationCode = DegradationCode {
    id: "D004",
    subsystem: DegradedSubsystem::Search,
    severity: DegradationSeverity::Critical,
    description: "FTS5 extension not available",
    behavior_change: "Full-text search disabled; only exact matches work",
    auto_recoverable: false,
    repair: None,
};

// Storage degradations (D100 - D199)
pub const DATABASE_READ_ONLY: DegradationCode = DegradationCode {
    id: "D100",
    subsystem: DegradedSubsystem::Storage,
    severity: DegradationSeverity::Warning,
    description: "Database is in read-only mode",
    behavior_change: "Write operations will fail; reads work normally",
    auto_recoverable: false,
    repair: Some("ee db unlock"),
};

pub const WAL_MODE_DISABLED: DegradationCode = DegradationCode {
    id: "D101",
    subsystem: DegradedSubsystem::Storage,
    severity: DegradationSeverity::Advisory,
    description: "WAL mode not enabled",
    behavior_change: "Reduced concurrent read performance",
    auto_recoverable: false,
    repair: Some("ee db optimize"),
};

pub const LARGE_DATABASE: DegradationCode = DegradationCode {
    id: "D102",
    subsystem: DegradedSubsystem::Storage,
    severity: DegradationSeverity::Advisory,
    description: "Database size exceeds recommended threshold",
    behavior_change: "Some operations may be slower",
    auto_recoverable: false,
    repair: Some("ee steward compact"),
};

// CASS degradations (D200 - D299)
pub const CASS_NOT_FOUND: DegradationCode = DegradationCode {
    id: "D200",
    subsystem: DegradedSubsystem::Cass,
    severity: DegradationSeverity::Warning,
    description: "CASS binary not found",
    behavior_change: "Session import disabled; explicit memories work",
    auto_recoverable: false,
    repair: None,
};

pub const CASS_VERSION_MISMATCH: DegradationCode = DegradationCode {
    id: "D201",
    subsystem: DegradedSubsystem::Cass,
    severity: DegradationSeverity::Warning,
    description: "CASS version incompatible",
    behavior_change: "Session import may fail or produce unexpected results",
    auto_recoverable: false,
    repair: None,
};

pub const CASS_INDEX_STALE: DegradationCode = DegradationCode {
    id: "D202",
    subsystem: DegradedSubsystem::Cass,
    severity: DegradationSeverity::Advisory,
    description: "CASS index is stale",
    behavior_change: "Recent sessions may not be available for import",
    auto_recoverable: true,
    repair: Some("cass index --full"),
};

// Graph degradations (D300 - D399)
pub const GRAPH_SNAPSHOT_STALE: DegradationCode = DegradationCode {
    id: "D300",
    subsystem: DegradedSubsystem::Graph,
    severity: DegradationSeverity::Advisory,
    description: "Graph snapshot is stale",
    behavior_change: "Graph metrics may not reflect recent changes",
    auto_recoverable: true,
    repair: Some("ee graph rebuild"),
};

pub const GRAPH_METRICS_UNAVAILABLE: DegradationCode = DegradationCode {
    id: "D301",
    subsystem: DegradedSubsystem::Graph,
    severity: DegradationSeverity::Warning,
    description: "Graph metrics not computed",
    behavior_change: "Related memories and why explanations limited",
    auto_recoverable: true,
    repair: Some("ee graph rebuild"),
};

// Pack degradations (D400 - D499)
pub const TOKEN_BUDGET_EXCEEDED: DegradationCode = DegradationCode {
    id: "D400",
    subsystem: DegradedSubsystem::Pack,
    severity: DegradationSeverity::Advisory,
    description: "Token budget exceeded",
    behavior_change: "Context pack truncated; some memories omitted",
    auto_recoverable: true,
    repair: None,
};

pub const MMR_FALLBACK: DegradationCode = DegradationCode {
    id: "D401",
    subsystem: DegradedSubsystem::Pack,
    severity: DegradationSeverity::Advisory,
    description: "MMR diversity selection disabled",
    behavior_change: "Pack may contain redundant memories",
    auto_recoverable: true,
    repair: None,
};

// Curate degradations (D500 - D599)
pub const CURATION_QUEUE_FULL: DegradationCode = DegradationCode {
    id: "D500",
    subsystem: DegradedSubsystem::Curate,
    severity: DegradationSeverity::Advisory,
    description: "Curation candidate queue is full",
    behavior_change: "New candidates will be dropped until reviewed",
    auto_recoverable: false,
    repair: Some("ee curate review"),
};

pub const AUTO_CURATION_DISABLED: DegradationCode = DegradationCode {
    id: "D501",
    subsystem: DegradedSubsystem::Curate,
    severity: DegradationSeverity::Advisory,
    description: "Automatic curation disabled",
    behavior_change: "Rules will not auto-promote; manual review required",
    auto_recoverable: false,
    repair: Some("ee config set curate.auto_promote true"),
};

// Policy degradations (D600 - D699)
pub const POLICY_NOT_LOADED: DegradationCode = DegradationCode {
    id: "D600",
    subsystem: DegradedSubsystem::Policy,
    severity: DegradationSeverity::Warning,
    description: "Policy file not loaded",
    behavior_change: "Default policies in effect; custom rules ignored",
    auto_recoverable: false,
    repair: Some("ee policy validate"),
};

pub const REDACTION_PATTERNS_STALE: DegradationCode = DegradationCode {
    id: "D601",
    subsystem: DegradedSubsystem::Policy,
    severity: DegradationSeverity::Advisory,
    description: "Redaction patterns may be outdated",
    behavior_change: "Some sensitive data may not be caught",
    auto_recoverable: false,
    repair: Some("ee policy update"),
};

// Network degradations (D700 - D799)
pub const NETWORK_UNAVAILABLE: DegradationCode = DegradationCode {
    id: "D700",
    subsystem: DegradedSubsystem::Network,
    severity: DegradationSeverity::Warning,
    description: "Network access unavailable",
    behavior_change: "Remote operations disabled; local-only mode",
    auto_recoverable: true,
    repair: None,
};

/// All registered degradation codes for enumeration.
pub const ALL_DEGRADATION_CODES: &[DegradationCode] = &[
    // Search
    SEMANTIC_SEARCH_UNAVAILABLE,
    EMBEDDING_MODEL_MISSING,
    SEARCH_INDEX_STALE,
    FTS5_UNAVAILABLE,
    // Storage
    DATABASE_READ_ONLY,
    WAL_MODE_DISABLED,
    LARGE_DATABASE,
    // CASS
    CASS_NOT_FOUND,
    CASS_VERSION_MISMATCH,
    CASS_INDEX_STALE,
    // Graph
    GRAPH_SNAPSHOT_STALE,
    GRAPH_METRICS_UNAVAILABLE,
    // Pack
    TOKEN_BUDGET_EXCEEDED,
    MMR_FALLBACK,
    // Curate
    CURATION_QUEUE_FULL,
    AUTO_CURATION_DISABLED,
    // Policy
    POLICY_NOT_LOADED,
    REDACTION_PATTERNS_STALE,
    // Network
    NETWORK_UNAVAILABLE,
];

/// Look up a degradation code by its stable ID (e.g., "D001").
#[must_use]
pub fn lookup(id: &str) -> Option<DegradationCode> {
    ALL_DEGRADATION_CODES
        .iter()
        .find(|code| code.id == id)
        .copied()
}

/// Look up degradation codes by subsystem.
#[must_use]
pub fn by_subsystem(subsystem: DegradedSubsystem) -> Vec<DegradationCode> {
    ALL_DEGRADATION_CODES
        .iter()
        .filter(|code| code.subsystem == subsystem)
        .copied()
        .collect()
}

/// Look up degradation codes by severity.
#[must_use]
pub fn by_severity(severity: DegradationSeverity) -> Vec<DegradationCode> {
    ALL_DEGRADATION_CODES
        .iter()
        .filter(|code| code.severity == severity)
        .copied()
        .collect()
}

/// Active degradation state.
///
/// Represents a degradation that's currently in effect, with optional
/// context about when it was detected and why.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActiveDegradation {
    /// The degradation code.
    pub code: DegradationCode,
    /// When the degradation was first detected.
    pub detected_at: Option<String>,
    /// Additional context about the degradation.
    pub context: Option<String>,
}

impl ActiveDegradation {
    /// Create a new active degradation.
    #[must_use]
    pub const fn new(code: DegradationCode) -> Self {
        Self {
            code,
            detected_at: None,
            context: None,
        }
    }

    /// Builder: set detection time.
    #[must_use]
    pub fn at(mut self, timestamp: impl Into<String>) -> Self {
        self.detected_at = Some(timestamp.into());
        self
    }

    /// Builder: add context.
    #[must_use]
    pub fn with_context(mut self, context: impl Into<String>) -> Self {
        self.context = Some(context.into());
        self
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
    fn degradation_code_ids_are_unique() -> TestResult {
        let mut seen = std::collections::HashSet::new();
        for code in ALL_DEGRADATION_CODES {
            if !seen.insert(code.id) {
                return Err(format!("Duplicate degradation code ID: {}", code.id));
            }
        }
        Ok(())
    }

    #[test]
    fn degradation_code_ids_follow_format() -> TestResult {
        for code in ALL_DEGRADATION_CODES {
            if !code.id.starts_with('D') {
                return Err(format!("Code {} does not start with D", code.id));
            }
            if code.id.len() != 4 {
                return Err(format!("Code {} is not 4 characters", code.id));
            }
        }
        Ok(())
    }

    #[test]
    fn degradation_code_numbers_are_in_range() -> TestResult {
        for code in ALL_DEGRADATION_CODES {
            let num = code.number();
            let expected_range = match code.subsystem {
                DegradedSubsystem::Search => 1..100,
                DegradedSubsystem::Storage => 100..200,
                DegradedSubsystem::Cass => 200..300,
                DegradedSubsystem::Graph => 300..400,
                DegradedSubsystem::Pack => 400..500,
                DegradedSubsystem::Curate => 500..600,
                DegradedSubsystem::Policy => 600..700,
                DegradedSubsystem::Network => 700..800,
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
        let found = lookup("D001");
        ensure(found.is_some(), true, "D001 exists")?;
        ensure(found.map(|c| c.id), Some("D001"), "found correct code")
    }

    #[test]
    fn lookup_returns_none_for_unknown() -> TestResult {
        ensure(lookup("D999"), None, "unknown code returns None")
    }

    #[test]
    fn by_subsystem_returns_correct_codes() -> TestResult {
        let search = by_subsystem(DegradedSubsystem::Search);
        ensure(search.len() >= 3, true, "at least 3 search codes")?;
        for code in &search {
            ensure(
                code.subsystem,
                DegradedSubsystem::Search,
                "correct subsystem",
            )?;
        }
        Ok(())
    }

    #[test]
    fn by_severity_returns_correct_codes() -> TestResult {
        let warnings = by_severity(DegradationSeverity::Warning);
        ensure(warnings.len() >= 3, true, "at least 3 warning codes")?;
        for code in &warnings {
            ensure(
                code.severity,
                DegradationSeverity::Warning,
                "correct severity",
            )?;
        }
        Ok(())
    }

    #[test]
    fn severity_ordering() -> TestResult {
        ensure(
            DegradationSeverity::Advisory < DegradationSeverity::Warning,
            true,
            "advisory < warning",
        )?;
        ensure(
            DegradationSeverity::Warning < DegradationSeverity::Critical,
            true,
            "warning < critical",
        )
    }

    #[test]
    fn severity_strings_are_stable() -> TestResult {
        ensure(
            DegradationSeverity::Advisory.as_str(),
            "advisory",
            "advisory",
        )?;
        ensure(DegradationSeverity::Warning.as_str(), "warning", "warning")?;
        ensure(
            DegradationSeverity::Critical.as_str(),
            "critical",
            "critical",
        )
    }

    #[test]
    fn subsystem_strings_are_stable() -> TestResult {
        ensure(DegradedSubsystem::Search.as_str(), "search", "search")?;
        ensure(DegradedSubsystem::Storage.as_str(), "storage", "storage")?;
        ensure(DegradedSubsystem::Cass.as_str(), "cass", "cass")?;
        ensure(DegradedSubsystem::Graph.as_str(), "graph", "graph")?;
        ensure(DegradedSubsystem::Pack.as_str(), "pack", "pack")?;
        ensure(DegradedSubsystem::Curate.as_str(), "curate", "curate")?;
        ensure(DegradedSubsystem::Policy.as_str(), "policy", "policy")?;
        ensure(DegradedSubsystem::Network.as_str(), "network", "network")
    }

    #[test]
    fn all_subsystems_have_at_least_one_code() -> TestResult {
        let subsystems = [
            DegradedSubsystem::Search,
            DegradedSubsystem::Storage,
            DegradedSubsystem::Cass,
            DegradedSubsystem::Graph,
            DegradedSubsystem::Pack,
            DegradedSubsystem::Curate,
            DegradedSubsystem::Policy,
            DegradedSubsystem::Network,
        ];
        for sub in subsystems {
            let codes = by_subsystem(sub);
            if codes.is_empty() {
                return Err(format!("Subsystem {:?} has no codes", sub));
            }
        }
        Ok(())
    }

    #[test]
    fn active_degradation_builder() {
        let active = ActiveDegradation::new(SEMANTIC_SEARCH_UNAVAILABLE)
            .at("2026-01-01T00:00:00Z")
            .with_context("Model file missing");

        assert_eq!(active.code.id, "D001");
        assert_eq!(active.detected_at, Some("2026-01-01T00:00:00Z".to_string()));
        assert_eq!(active.context, Some("Model file missing".to_string()));
    }
}
