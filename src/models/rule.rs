//! Procedural rule domain types (EE-084).
//!
//! Procedural rules are distilled lessons, patterns, and policies from
//! experience. They have lifecycle (maturity), scope (where they apply),
//! and feedback tracking.

use std::fmt;
use std::str::FromStr;

/// Scope of a procedural rule - where it applies.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum RuleScope {
    /// Applies to all workspaces.
    Global,
    /// Applies to the current workspace.
    Workspace,
    /// Applies to a specific project within workspace.
    Project,
    /// Applies to a specific directory pattern.
    Directory,
    /// Applies to files matching a pattern.
    FilePattern,
}

impl RuleScope {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::Workspace => "workspace",
            Self::Project => "project",
            Self::Directory => "directory",
            Self::FilePattern => "file_pattern",
        }
    }

    #[must_use]
    pub const fn all() -> [Self; 5] {
        [
            Self::Global,
            Self::Workspace,
            Self::Project,
            Self::Directory,
            Self::FilePattern,
        ]
    }

    #[must_use]
    pub const fn requires_pattern(self) -> bool {
        matches!(self, Self::Directory | Self::FilePattern)
    }
}

impl fmt::Display for RuleScope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Error when parsing an invalid rule scope string.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseRuleScopeError {
    input: String,
}

impl ParseRuleScopeError {
    pub fn input(&self) -> &str {
        &self.input
    }
}

impl fmt::Display for ParseRuleScopeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "unknown rule scope `{}`; expected one of global, workspace, project, directory, file_pattern",
            self.input
        )
    }
}

impl std::error::Error for ParseRuleScopeError {}

impl FromStr for RuleScope {
    type Err = ParseRuleScopeError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        match input {
            "global" => Ok(Self::Global),
            "workspace" => Ok(Self::Workspace),
            "project" => Ok(Self::Project),
            "directory" => Ok(Self::Directory),
            "file_pattern" => Ok(Self::FilePattern),
            _ => Err(ParseRuleScopeError {
                input: input.to_owned(),
            }),
        }
    }
}

/// Maturity level of a procedural rule.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum RuleMaturity {
    /// Newly created, not yet reviewed.
    Draft,
    /// Proposed for validation.
    Candidate,
    /// Validated by positive outcomes.
    Validated,
    /// No longer recommended but kept for history.
    Deprecated,
    /// Replaced by another rule.
    Superseded,
}

impl RuleMaturity {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Candidate => "candidate",
            Self::Validated => "validated",
            Self::Deprecated => "deprecated",
            Self::Superseded => "superseded",
        }
    }

    #[must_use]
    pub const fn all() -> [Self; 5] {
        [
            Self::Draft,
            Self::Candidate,
            Self::Validated,
            Self::Deprecated,
            Self::Superseded,
        ]
    }

    #[must_use]
    pub const fn is_active(self) -> bool {
        matches!(self, Self::Draft | Self::Candidate | Self::Validated)
    }

    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Deprecated | Self::Superseded)
    }
}

impl fmt::Display for RuleMaturity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Error when parsing an invalid rule maturity string.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseRuleMaturityError {
    input: String,
}

impl ParseRuleMaturityError {
    pub fn input(&self) -> &str {
        &self.input
    }
}

impl fmt::Display for ParseRuleMaturityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "unknown rule maturity `{}`; expected one of draft, candidate, validated, deprecated, superseded",
            self.input
        )
    }
}

impl std::error::Error for ParseRuleMaturityError {}

impl FromStr for RuleMaturity {
    type Err = ParseRuleMaturityError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        match input {
            "draft" => Ok(Self::Draft),
            "candidate" => Ok(Self::Candidate),
            "validated" => Ok(Self::Validated),
            "deprecated" => Ok(Self::Deprecated),
            "superseded" => Ok(Self::Superseded),
            _ => Err(ParseRuleMaturityError {
                input: input.to_owned(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::{ParseRuleMaturityError, ParseRuleScopeError, RuleMaturity, RuleScope};

    #[test]
    fn rule_scope_round_trip_for_every_variant() {
        for scope in RuleScope::all() {
            let rendered = scope.to_string();
            let parsed = RuleScope::from_str(&rendered)
                .unwrap_or_else(|e| panic!("rule scope {scope:?} failed to round-trip: {e}"));
            assert_eq!(parsed, scope);
        }
    }

    #[test]
    fn rule_scope_rejects_unknown_input() {
        let err = RuleScope::from_str("unknown_scope");
        assert!(matches!(err, Err(ParseRuleScopeError { .. })));
    }

    #[test]
    fn rule_scope_requires_pattern() {
        assert!(!RuleScope::Global.requires_pattern());
        assert!(!RuleScope::Workspace.requires_pattern());
        assert!(!RuleScope::Project.requires_pattern());
        assert!(RuleScope::Directory.requires_pattern());
        assert!(RuleScope::FilePattern.requires_pattern());
    }

    #[test]
    fn rule_maturity_round_trip_for_every_variant() {
        for maturity in RuleMaturity::all() {
            let rendered = maturity.to_string();
            let parsed = RuleMaturity::from_str(&rendered)
                .unwrap_or_else(|e| panic!("rule maturity {maturity:?} failed to round-trip: {e}"));
            assert_eq!(parsed, maturity);
        }
    }

    #[test]
    fn rule_maturity_rejects_unknown_input() {
        let err = RuleMaturity::from_str("unknown_maturity");
        assert!(matches!(err, Err(ParseRuleMaturityError { .. })));
    }

    #[test]
    fn rule_maturity_active_and_terminal_states() {
        assert!(RuleMaturity::Draft.is_active());
        assert!(RuleMaturity::Candidate.is_active());
        assert!(RuleMaturity::Validated.is_active());
        assert!(!RuleMaturity::Deprecated.is_active());
        assert!(!RuleMaturity::Superseded.is_active());

        assert!(!RuleMaturity::Draft.is_terminal());
        assert!(!RuleMaturity::Candidate.is_terminal());
        assert!(!RuleMaturity::Validated.is_terminal());
        assert!(RuleMaturity::Deprecated.is_terminal());
        assert!(RuleMaturity::Superseded.is_terminal());
    }
}
