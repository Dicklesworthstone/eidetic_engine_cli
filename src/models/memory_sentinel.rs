//! Verifiable memory sentinel models.
//!
//! A sentinel is a user-declared, deterministic validity predicate attached to
//! a durable memory. The model layer owns the stable wire strings, safety
//! classes, parser repair hints, and hash inputs; execution remains a later
//! core concern.

use std::fmt;
use std::str::FromStr;

use crate::models::id::MemoryId;

pub const MEMORY_SENTINEL_SPEC_SCHEMA_V1: &str = "ee.memory_sentinel.spec.v1";
pub const MEMORY_SENTINEL_RESULT_SCHEMA_V1: &str = "ee.memory_sentinel.result.v1";
pub const MEMORY_SENTINEL_SPEC_HASH_SCHEMA_V1: &str = "ee.memory_sentinel.spec_hash.v1";
pub const MEMORY_SENTINEL_RESULT_HASH_SCHEMA_V1: &str = "ee.memory_sentinel.result_hash.v1";

pub const MAX_MEMORY_SENTINEL_TARGET_BYTES: usize = 1_024;
pub const MAX_MEMORY_SENTINEL_PREDICATE_BYTES: usize = 512;
pub const MAX_MEMORY_SENTINEL_PROVENANCE_BYTES: usize = 512;
pub const MAX_MEMORY_SENTINEL_EVIDENCE_BYTES: usize = 4_096;

const BLAKE3_PREFIX: &str = "blake3:";
const BLAKE3_HEX_LEN: usize = 64;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum MemorySentinelKind {
    PathExists,
    FileHashOrMarker,
    JsonSchemaContainsField,
    ConfigKeyExists,
    EnvVarRegistered,
    DegradedCodeFixtureExists,
    DependencyCapabilityPresent,
    CommandHelpContainsFlag,
}

impl MemorySentinelKind {
    #[must_use]
    pub const fn all() -> [Self; 8] {
        [
            Self::PathExists,
            Self::FileHashOrMarker,
            Self::JsonSchemaContainsField,
            Self::ConfigKeyExists,
            Self::EnvVarRegistered,
            Self::DegradedCodeFixtureExists,
            Self::DependencyCapabilityPresent,
            Self::CommandHelpContainsFlag,
        ]
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PathExists => "path_exists",
            Self::FileHashOrMarker => "file_hash_or_marker",
            Self::JsonSchemaContainsField => "json_schema_contains_field",
            Self::ConfigKeyExists => "config_key_exists",
            Self::EnvVarRegistered => "env_var_registered",
            Self::DegradedCodeFixtureExists => "degraded_code_fixture_exists",
            Self::DependencyCapabilityPresent => "dependency_capability_present",
            Self::CommandHelpContainsFlag => "command_help_contains_flag",
        }
    }

    #[must_use]
    pub const fn default_predicate(self) -> &'static str {
        match self {
            Self::PathExists | Self::ConfigKeyExists => "exists",
            Self::FileHashOrMarker => "hash_or_marker_present",
            Self::JsonSchemaContainsField => "contains_field",
            Self::EnvVarRegistered => "registered",
            Self::DegradedCodeFixtureExists => "fixture_exists",
            Self::DependencyCapabilityPresent => "present",
            Self::CommandHelpContainsFlag => "help_contains_flag",
        }
    }

    #[must_use]
    pub const fn safety_class(self) -> MemorySentinelSafetyClass {
        match self {
            Self::CommandHelpContainsFlag => MemorySentinelSafetyClass::AllowlistedIntrospection,
            Self::PathExists
            | Self::FileHashOrMarker
            | Self::JsonSchemaContainsField
            | Self::ConfigKeyExists
            | Self::EnvVarRegistered
            | Self::DegradedCodeFixtureExists
            | Self::DependencyCapabilityPresent => MemorySentinelSafetyClass::PurePredicate,
        }
    }

    #[must_use]
    pub fn parse(input: &str) -> Option<Self> {
        match normalize_kind_token(input).as_str() {
            "path_exists" => Some(Self::PathExists),
            "file_hash_or_marker" => Some(Self::FileHashOrMarker),
            "json_schema_contains_field" => Some(Self::JsonSchemaContainsField),
            "config_key_exists" => Some(Self::ConfigKeyExists),
            "env_var_registered" => Some(Self::EnvVarRegistered),
            "degraded_code_fixture_exists" => Some(Self::DegradedCodeFixtureExists),
            "dependency_capability_present" => Some(Self::DependencyCapabilityPresent),
            "command_help_contains_flag" => Some(Self::CommandHelpContainsFlag),
            _ => None,
        }
    }
}

impl fmt::Display for MemorySentinelKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for MemorySentinelKind {
    type Err = MemorySentinelValidationError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        Self::parse(input).ok_or_else(|| MemorySentinelValidationError::UnknownKind {
            input: input.to_owned(),
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum MemorySentinelSafetyClass {
    PurePredicate,
    AllowlistedIntrospection,
}

impl MemorySentinelSafetyClass {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PurePredicate => "pure_predicate",
            Self::AllowlistedIntrospection => "allowlisted_introspection",
        }
    }

    #[must_use]
    pub fn parse(input: &str) -> Option<Self> {
        match input {
            "pure_predicate" => Some(Self::PurePredicate),
            "allowlisted_introspection" => Some(Self::AllowlistedIntrospection),
            _ => None,
        }
    }
}

impl fmt::Display for MemorySentinelSafetyClass {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum MemorySentinelResultStatus {
    Pass,
    Fail,
    Unknown,
    Degraded,
}

impl MemorySentinelResultStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Fail => "fail",
            Self::Unknown => "unknown",
            Self::Degraded => "degraded",
        }
    }

    #[must_use]
    pub fn parse(input: &str) -> Option<Self> {
        match input.trim().to_ascii_lowercase().as_str() {
            "pass" => Some(Self::Pass),
            "fail" => Some(Self::Fail),
            "unknown" => Some(Self::Unknown),
            "degraded" => Some(Self::Degraded),
            _ => None,
        }
    }
}

impl fmt::Display for MemorySentinelResultStatus {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Outcome of resolving a sentinel's target against current local state, before
/// it is mapped to a [`MemorySentinelResultStatus`] (bd-1n0np.16.3).
///
/// The pure-predicate checker's I/O layer (filesystem / config / schema / env /
/// fixture / allowlisted-introspection probes) produces one of these; the
/// mapping below applies the conservatism rule so the decision stays separate
/// from the I/O and is exhaustively testable.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum SentinelObservation {
    /// The predicate holds — e.g. the path/schema field/config key/env var/
    /// fixture exists, or the allowlisted help text contains the flag.
    Satisfied,
    /// The predicate is definitively false — the referent is gone or absent.
    Unsatisfied,
    /// The check could not run or its result is ambiguous (e.g. an
    /// introspection surface was unavailable, or the workspace could not be
    /// resolved). Conservative: this is NEVER reported as a failure.
    Unverifiable,
}

impl SentinelObservation {
    /// Map the observation to a result status, enforcing the sentinel
    /// conservatism rule: an unverifiable check is `Unknown` (advisory), NEVER
    /// `Fail`. `ee` never mutates on a sentinel result — it reports and proposes
    /// a curation candidate — so a false `Fail` would be the costly error.
    #[must_use]
    pub const fn into_status(self) -> MemorySentinelResultStatus {
        match self {
            Self::Satisfied => MemorySentinelResultStatus::Pass,
            Self::Unsatisfied => MemorySentinelResultStatus::Fail,
            Self::Unverifiable => MemorySentinelResultStatus::Unknown,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParsedMemorySentinelSpec {
    pub sentinel_kind: MemorySentinelKind,
    pub target: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CreateMemorySentinelSpecInput {
    pub memory_id: String,
    pub sentinel_kind: MemorySentinelKind,
    pub target: String,
    pub expected_predicate: Option<String>,
    pub provenance: String,
    pub stale_threshold_seconds: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MemorySentinelSpec {
    pub schema: &'static str,
    pub spec_hash: String,
    pub memory_id: String,
    pub sentinel_kind: MemorySentinelKind,
    pub target: String,
    pub expected_predicate: String,
    pub safety_class: MemorySentinelSafetyClass,
    pub provenance: String,
    pub stale_threshold_seconds: Option<u64>,
}

impl MemorySentinelSpec {
    /// Parse `remember --sentinel <kind>:<target>` input and build a validated spec.
    ///
    /// # Errors
    ///
    /// Returns [`MemorySentinelValidationError`] when the memory id, raw sentinel
    /// syntax, target, predicate, provenance, or stale threshold is invalid.
    pub fn from_raw(
        memory_id: &str,
        raw_spec: &str,
        expected_predicate: Option<&str>,
        provenance: &str,
        stale_threshold_seconds: Option<u64>,
    ) -> Result<Self, MemorySentinelValidationError> {
        let parsed = parse_memory_sentinel_spec(raw_spec)?;
        Self::new(CreateMemorySentinelSpecInput {
            memory_id: memory_id.to_owned(),
            sentinel_kind: parsed.sentinel_kind,
            target: parsed.target,
            expected_predicate: expected_predicate.map(str::to_owned),
            provenance: provenance.to_owned(),
            stale_threshold_seconds,
        })
    }

    /// Build a validated sentinel spec and compute its stable hash.
    ///
    /// # Errors
    ///
    /// Returns [`MemorySentinelValidationError`] when any field is outside the
    /// supported sentinel contract.
    pub fn new(
        input: CreateMemorySentinelSpecInput,
    ) -> Result<Self, MemorySentinelValidationError> {
        let memory_id = validate_memory_id(&input.memory_id)?;
        let target = validate_target(input.sentinel_kind, &input.target)?;
        let expected_predicate = validate_predicate(
            input
                .expected_predicate
                .as_deref()
                .unwrap_or_else(|| input.sentinel_kind.default_predicate()),
        )?;
        let provenance = validate_provenance(&input.provenance)?;
        validate_stale_threshold(input.stale_threshold_seconds)?;
        let safety_class = input.sentinel_kind.safety_class();
        let spec_hash = stable_spec_hash(
            &memory_id,
            input.sentinel_kind,
            &target,
            &expected_predicate,
            safety_class,
            &provenance,
            input.stale_threshold_seconds,
        );
        Ok(Self {
            schema: MEMORY_SENTINEL_SPEC_SCHEMA_V1,
            spec_hash,
            memory_id,
            sentinel_kind: input.sentinel_kind,
            target,
            expected_predicate,
            safety_class,
            provenance,
            stale_threshold_seconds: input.stale_threshold_seconds,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredMemorySentinelSpec {
    pub spec_hash: String,
    pub memory_id: String,
    pub sentinel_kind: MemorySentinelKind,
    pub target: String,
    pub expected_predicate: String,
    pub safety_class: MemorySentinelSafetyClass,
    pub provenance: String,
    pub stale_threshold_seconds: Option<u64>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MemorySentinelResultInput {
    pub spec_hash: String,
    pub status: MemorySentinelResultStatus,
    pub checked_at: String,
    pub evidence_summary: String,
    pub stale_threshold_seconds: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MemorySentinelResult {
    pub schema: &'static str,
    pub spec_hash: String,
    pub status: MemorySentinelResultStatus,
    pub checked_at: String,
    pub evidence_summary: String,
    pub result_hash: String,
    pub stale_threshold_seconds: Option<u64>,
}

impl MemorySentinelResult {
    /// Build a validated sentinel result and compute its stable hash.
    ///
    /// # Errors
    ///
    /// Returns [`MemorySentinelValidationError`] when the result references an
    /// invalid spec hash or carries malformed evidence metadata.
    pub fn new(input: MemorySentinelResultInput) -> Result<Self, MemorySentinelValidationError> {
        let spec_hash = validate_blake3_hash("spec_hash", &input.spec_hash)?;
        let checked_at = validate_checked_at(&input.checked_at)?;
        let evidence_summary = validate_evidence_summary(&input.evidence_summary)?;
        validate_stale_threshold(input.stale_threshold_seconds)?;
        let result_hash = stable_result_hash(
            &spec_hash,
            input.status,
            &checked_at,
            &evidence_summary,
            input.stale_threshold_seconds,
        );
        Ok(Self {
            schema: MEMORY_SENTINEL_RESULT_SCHEMA_V1,
            spec_hash,
            status: input.status,
            checked_at,
            evidence_summary,
            result_hash,
            stale_threshold_seconds: input.stale_threshold_seconds,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredMemorySentinelResult {
    pub result_hash: String,
    pub spec_hash: String,
    pub status: MemorySentinelResultStatus,
    pub checked_at: String,
    pub evidence_summary: String,
    pub stale_threshold_seconds: Option<u64>,
    pub created_at: String,
}

/// Parse the stable `<kind>:<target>` sentinel syntax without requiring a memory id.
///
/// # Errors
///
/// Returns [`MemorySentinelValidationError`] when the syntax is malformed, the
/// kind is unknown, or the target violates the selected kind's safety envelope.
pub fn parse_memory_sentinel_spec(
    raw_spec: &str,
) -> Result<ParsedMemorySentinelSpec, MemorySentinelValidationError> {
    let trimmed = raw_spec.trim();
    let Some((raw_kind, raw_target)) = trimmed.split_once(':') else {
        return Err(MemorySentinelValidationError::MalformedSpec {
            input: raw_spec.to_owned(),
            reason: "missing `:` separator",
        });
    };
    if raw_kind.trim().is_empty() {
        return Err(MemorySentinelValidationError::MalformedSpec {
            input: raw_spec.to_owned(),
            reason: "missing sentinel kind",
        });
    }
    let sentinel_kind = MemorySentinelKind::from_str(raw_kind)?;
    let target = validate_target(sentinel_kind, raw_target)?;
    Ok(ParsedMemorySentinelSpec {
        sentinel_kind,
        target,
    })
}

#[must_use]
pub fn memory_sentinel_spec_repair_hint() -> &'static str {
    "Use --sentinel <kind>:<target>; run `ee sentinel explain` for supported kinds and target syntax."
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MemorySentinelValidationError {
    InvalidMemoryId {
        input: String,
        reason: String,
    },
    MalformedSpec {
        input: String,
        reason: &'static str,
    },
    UnknownKind {
        input: String,
    },
    InvalidTarget {
        kind: MemorySentinelKind,
        target: String,
        reason: &'static str,
    },
    InvalidPredicate {
        predicate: String,
        reason: &'static str,
    },
    InvalidProvenance {
        reason: &'static str,
    },
    InvalidStaleThreshold {
        seconds: u64,
    },
    InvalidHash {
        field: &'static str,
        value: String,
    },
    InvalidTimestamp {
        field: &'static str,
        value: String,
    },
    InvalidEvidenceSummary {
        reason: &'static str,
    },
}

impl MemorySentinelValidationError {
    #[must_use]
    pub fn repair(&self) -> &'static str {
        match self {
            Self::InvalidMemoryId { .. } => {
                "Attach sentinels to an existing memory id in mem_<26-char-id> form."
            }
            Self::MalformedSpec { .. } | Self::UnknownKind { .. } | Self::InvalidTarget { .. } => {
                memory_sentinel_spec_repair_hint()
            }
            Self::InvalidPredicate { .. } => {
                "Use a short non-empty predicate token, or omit it to use the kind default."
            }
            Self::InvalidProvenance { .. } => {
                "Provide a short provenance string naming who or what declared the sentinel."
            }
            Self::InvalidStaleThreshold { .. } => {
                "Use a positive stale-threshold duration in seconds, or omit it."
            }
            Self::InvalidHash { .. } => "Use a blake3:<64-hex-character> hash.",
            Self::InvalidTimestamp { .. } => {
                "Use an RFC 3339 timestamp such as 2026-06-07T20:00:00Z."
            }
            Self::InvalidEvidenceSummary { .. } => {
                "Use a short non-empty evidence summary describing the check outcome."
            }
        }
    }
}

impl fmt::Display for MemorySentinelValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidMemoryId { input, reason } => {
                write!(
                    formatter,
                    "invalid memory id `{input}` for sentinel: {reason}"
                )
            }
            Self::MalformedSpec { input, reason } => {
                write!(formatter, "malformed sentinel spec `{input}`: {reason}")
            }
            Self::UnknownKind { input } => write!(
                formatter,
                "unknown memory sentinel kind `{input}`; expected one of {}",
                MemorySentinelKind::all()
                    .iter()
                    .map(|kind| kind.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Self::InvalidTarget {
                kind,
                target,
                reason,
            } => write!(
                formatter,
                "invalid target `{target}` for memory sentinel kind `{kind}`: {reason}"
            ),
            Self::InvalidPredicate { predicate, reason } => {
                write!(
                    formatter,
                    "invalid sentinel predicate `{predicate}`: {reason}"
                )
            }
            Self::InvalidProvenance { reason } => {
                write!(formatter, "invalid sentinel provenance: {reason}")
            }
            Self::InvalidStaleThreshold { seconds } => write!(
                formatter,
                "invalid sentinel stale threshold `{seconds}`; expected a positive value"
            ),
            Self::InvalidHash { field, value } => {
                write!(formatter, "invalid sentinel {field} `{value}`")
            }
            Self::InvalidTimestamp { field, value } => {
                write!(formatter, "invalid sentinel {field} timestamp `{value}`")
            }
            Self::InvalidEvidenceSummary { reason } => {
                write!(formatter, "invalid sentinel evidence summary: {reason}")
            }
        }
    }
}

impl std::error::Error for MemorySentinelValidationError {}

fn validate_memory_id(input: &str) -> Result<String, MemorySentinelValidationError> {
    let trimmed = input.trim();
    MemoryId::from_str(trimmed).map_err(|error| {
        MemorySentinelValidationError::InvalidMemoryId {
            input: input.to_owned(),
            reason: error.to_string(),
        }
    })?;
    Ok(trimmed.to_owned())
}

fn validate_target(
    kind: MemorySentinelKind,
    input: &str,
) -> Result<String, MemorySentinelValidationError> {
    let target = input.trim();
    if target.is_empty() {
        return Err(MemorySentinelValidationError::InvalidTarget {
            kind,
            target: input.to_owned(),
            reason: "target must not be empty",
        });
    }
    if target.len() > MAX_MEMORY_SENTINEL_TARGET_BYTES {
        return Err(MemorySentinelValidationError::InvalidTarget {
            kind,
            target: target.to_owned(),
            reason: "target is too long",
        });
    }
    if has_control_character(target) {
        return Err(MemorySentinelValidationError::InvalidTarget {
            kind,
            target: target.to_owned(),
            reason: "target must not contain control characters",
        });
    }
    match kind {
        MemorySentinelKind::PathExists | MemorySentinelKind::FileHashOrMarker => {
            validate_workspace_target(kind, target)?;
        }
        MemorySentinelKind::EnvVarRegistered => {
            validate_env_var_target(kind, target)?;
        }
        MemorySentinelKind::DegradedCodeFixtureExists => {
            validate_snake_identifier_target(kind, target, "degraded code fixture id")?;
        }
        MemorySentinelKind::ConfigKeyExists => {
            validate_config_key_target(kind, target)?;
        }
        MemorySentinelKind::DependencyCapabilityPresent => {
            validate_dependency_capability_target(kind, target)?;
        }
        MemorySentinelKind::JsonSchemaContainsField => {
            validate_generic_read_only_target(kind, target)?;
        }
        MemorySentinelKind::CommandHelpContainsFlag => {
            validate_command_help_target(kind, target)?;
        }
    }
    Ok(target.to_owned())
}

fn validate_workspace_target(
    kind: MemorySentinelKind,
    target: &str,
) -> Result<(), MemorySentinelValidationError> {
    let path_part = target.split_once('#').map_or(target, |(path, _)| path);
    if path_part.starts_with('/') || path_part.starts_with('~') || path_part.contains("://") {
        return Err(MemorySentinelValidationError::InvalidTarget {
            kind,
            target: target.to_owned(),
            reason: "workspace file targets must be relative paths",
        });
    }
    if path_part.split('/').any(|component| component == "..") {
        return Err(MemorySentinelValidationError::InvalidTarget {
            kind,
            target: target.to_owned(),
            reason: "workspace file targets must not contain parent traversal",
        });
    }
    validate_generic_read_only_target(kind, target)
}

fn validate_env_var_target(
    kind: MemorySentinelKind,
    target: &str,
) -> Result<(), MemorySentinelValidationError> {
    if target.starts_with("EE_")
        && target.len() > 3
        && target
            .chars()
            .all(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit() || ch == '_')
    {
        Ok(())
    } else {
        Err(MemorySentinelValidationError::InvalidTarget {
            kind,
            target: target.to_owned(),
            reason: "env var sentinels must target a registered EE_* variable",
        })
    }
}

fn validate_snake_identifier_target(
    kind: MemorySentinelKind,
    target: &str,
    label: &'static str,
) -> Result<(), MemorySentinelValidationError> {
    if is_snake_identifier(target) {
        Ok(())
    } else {
        Err(MemorySentinelValidationError::InvalidTarget {
            kind,
            target: target.to_owned(),
            reason: label,
        })
    }
}

fn validate_config_key_target(
    kind: MemorySentinelKind,
    target: &str,
) -> Result<(), MemorySentinelValidationError> {
    if target.contains('.')
        && target.chars().all(|ch| {
            ch.is_ascii_lowercase() || ch.is_ascii_digit() || matches!(ch, '.' | '_' | '-')
        })
    {
        Ok(())
    } else {
        Err(MemorySentinelValidationError::InvalidTarget {
            kind,
            target: target.to_owned(),
            reason: "config key targets must be lowercase dotted keys",
        })
    }
}

fn validate_dependency_capability_target(
    kind: MemorySentinelKind,
    target: &str,
) -> Result<(), MemorySentinelValidationError> {
    if target.chars().all(|ch| {
        ch.is_ascii_lowercase() || ch.is_ascii_digit() || matches!(ch, '-' | '_' | ':' | '.')
    }) {
        Ok(())
    } else {
        Err(MemorySentinelValidationError::InvalidTarget {
            kind,
            target: target.to_owned(),
            reason: "dependency capability targets must be lowercase capability tokens",
        })
    }
}

fn validate_generic_read_only_target(
    kind: MemorySentinelKind,
    target: &str,
) -> Result<(), MemorySentinelValidationError> {
    if contains_shell_metacharacter(target) {
        return Err(MemorySentinelValidationError::InvalidTarget {
            kind,
            target: target.to_owned(),
            reason: "target must be declarative and must not contain shell metacharacters",
        });
    }
    Ok(())
}

fn validate_command_help_target(
    kind: MemorySentinelKind,
    target: &str,
) -> Result<(), MemorySentinelValidationError> {
    validate_generic_read_only_target(kind, target)?;
    let mut parts = target.split_whitespace();
    let Some(binary) = parts.next() else {
        return Err(MemorySentinelValidationError::InvalidTarget {
            kind,
            target: target.to_owned(),
            reason: "command-help target must start with ee",
        });
    };
    if binary != "ee" {
        return Err(MemorySentinelValidationError::InvalidTarget {
            kind,
            target: target.to_owned(),
            reason: "command-help sentinels are limited to the ee binary",
        });
    }
    if !target.split_whitespace().any(|part| part.starts_with("--")) {
        return Err(MemorySentinelValidationError::InvalidTarget {
            kind,
            target: target.to_owned(),
            reason: "command-help target must include the expected --flag",
        });
    }
    Ok(())
}

fn validate_predicate(input: &str) -> Result<String, MemorySentinelValidationError> {
    let predicate = input.trim();
    if predicate.is_empty() {
        return Err(MemorySentinelValidationError::InvalidPredicate {
            predicate: input.to_owned(),
            reason: "predicate must not be empty",
        });
    }
    if predicate.len() > MAX_MEMORY_SENTINEL_PREDICATE_BYTES {
        return Err(MemorySentinelValidationError::InvalidPredicate {
            predicate: predicate.to_owned(),
            reason: "predicate is too long",
        });
    }
    if has_control_character(predicate) || contains_shell_metacharacter(predicate) {
        return Err(MemorySentinelValidationError::InvalidPredicate {
            predicate: predicate.to_owned(),
            reason: "predicate must be declarative text",
        });
    }
    Ok(predicate.to_owned())
}

fn validate_provenance(input: &str) -> Result<String, MemorySentinelValidationError> {
    let provenance = input.trim();
    if provenance.is_empty() {
        return Err(MemorySentinelValidationError::InvalidProvenance {
            reason: "provenance must not be empty",
        });
    }
    if provenance.len() > MAX_MEMORY_SENTINEL_PROVENANCE_BYTES {
        return Err(MemorySentinelValidationError::InvalidProvenance {
            reason: "provenance is too long",
        });
    }
    if has_control_character(provenance) {
        return Err(MemorySentinelValidationError::InvalidProvenance {
            reason: "provenance must not contain control characters",
        });
    }
    Ok(provenance.to_owned())
}

fn validate_stale_threshold(value: Option<u64>) -> Result<(), MemorySentinelValidationError> {
    if value == Some(0) {
        return Err(MemorySentinelValidationError::InvalidStaleThreshold { seconds: 0 });
    }
    Ok(())
}

fn validate_blake3_hash(
    field: &'static str,
    input: &str,
) -> Result<String, MemorySentinelValidationError> {
    let trimmed = input.trim();
    let Some(hex) = trimmed.strip_prefix(BLAKE3_PREFIX) else {
        return Err(MemorySentinelValidationError::InvalidHash {
            field,
            value: input.to_owned(),
        });
    };
    if hex.len() != BLAKE3_HEX_LEN || !hex.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return Err(MemorySentinelValidationError::InvalidHash {
            field,
            value: input.to_owned(),
        });
    }
    Ok(trimmed.to_owned())
}

fn validate_checked_at(input: &str) -> Result<String, MemorySentinelValidationError> {
    let trimmed = input.trim();
    if chrono::DateTime::parse_from_rfc3339(trimmed).is_err() {
        return Err(MemorySentinelValidationError::InvalidTimestamp {
            field: "checked_at",
            value: input.to_owned(),
        });
    }
    Ok(trimmed.to_owned())
}

fn validate_evidence_summary(input: &str) -> Result<String, MemorySentinelValidationError> {
    let summary = input.trim();
    if summary.is_empty() {
        return Err(MemorySentinelValidationError::InvalidEvidenceSummary {
            reason: "summary must not be empty",
        });
    }
    if summary.len() > MAX_MEMORY_SENTINEL_EVIDENCE_BYTES {
        return Err(MemorySentinelValidationError::InvalidEvidenceSummary {
            reason: "summary is too long",
        });
    }
    if has_control_character(summary) {
        return Err(MemorySentinelValidationError::InvalidEvidenceSummary {
            reason: "summary must not contain control characters",
        });
    }
    Ok(summary.to_owned())
}

fn stable_spec_hash(
    memory_id: &str,
    kind: MemorySentinelKind,
    target: &str,
    expected_predicate: &str,
    safety_class: MemorySentinelSafetyClass,
    provenance: &str,
    stale_threshold_seconds: Option<u64>,
) -> String {
    let mut hasher = blake3::Hasher::new();
    hash_part(&mut hasher, MEMORY_SENTINEL_SPEC_HASH_SCHEMA_V1);
    hash_part(&mut hasher, memory_id);
    hash_part(&mut hasher, kind.as_str());
    hash_part(&mut hasher, target);
    hash_part(&mut hasher, expected_predicate);
    hash_part(&mut hasher, safety_class.as_str());
    hash_part(&mut hasher, provenance);
    hash_opt_u64(&mut hasher, stale_threshold_seconds);
    format!("blake3:{}", hasher.finalize().to_hex())
}

fn stable_result_hash(
    spec_hash: &str,
    status: MemorySentinelResultStatus,
    checked_at: &str,
    evidence_summary: &str,
    stale_threshold_seconds: Option<u64>,
) -> String {
    let mut hasher = blake3::Hasher::new();
    hash_part(&mut hasher, MEMORY_SENTINEL_RESULT_HASH_SCHEMA_V1);
    hash_part(&mut hasher, spec_hash);
    hash_part(&mut hasher, status.as_str());
    hash_part(&mut hasher, checked_at);
    hash_part(&mut hasher, evidence_summary);
    hash_opt_u64(&mut hasher, stale_threshold_seconds);
    format!("blake3:{}", hasher.finalize().to_hex())
}

fn hash_part(hasher: &mut blake3::Hasher, value: &str) {
    hasher.update(&(value.len() as u64).to_le_bytes());
    hasher.update(value.as_bytes());
}

fn hash_opt_u64(hasher: &mut blake3::Hasher, value: Option<u64>) {
    match value {
        Some(value) => {
            hasher.update(&[1]);
            hasher.update(&value.to_le_bytes());
        }
        None => {
            hasher.update(&[0]);
        }
    };
}

fn normalize_kind_token(input: &str) -> String {
    let mut normalized = String::with_capacity(input.len());
    let mut previous_was_separator = false;
    for character in input.trim().chars() {
        match character {
            '-' | '_' | ' ' => {
                if !normalized.is_empty() && !previous_was_separator {
                    normalized.push('_');
                }
                previous_was_separator = true;
            }
            character if character.is_ascii_uppercase() => {
                normalized.push(character.to_ascii_lowercase());
                previous_was_separator = false;
            }
            character => {
                normalized.push(character.to_ascii_lowercase());
                previous_was_separator = false;
            }
        }
    }
    while normalized.ends_with('_') {
        normalized.pop();
    }
    normalized
}

fn has_control_character(value: &str) -> bool {
    value.chars().any(char::is_control)
}

fn contains_shell_metacharacter(value: &str) -> bool {
    value
        .chars()
        .any(|ch| matches!(ch, ';' | '|' | '&' | '`' | '<' | '>' | '\n' | '\r'))
}

fn is_snake_identifier(value: &str) -> bool {
    !value.is_empty()
        && !value.starts_with('_')
        && !value.ends_with('_')
        && value
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_')
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

    fn memory_id() -> String {
        MemoryId::from_uuid(uuid::Uuid::nil()).to_string()
    }

    #[test]
    fn parses_path_sentinel_and_computes_stable_spec_hash() -> TestResult {
        let spec = MemorySentinelSpec::from_raw(
            &memory_id(),
            "path_exists:docs/adr/0060-verifiable-memory-sentinels.md",
            None,
            "test://sentinel",
            Some(3_600),
        )
        .map_err(|error| error.to_string())?;

        ensure(
            spec.schema == MEMORY_SENTINEL_SPEC_SCHEMA_V1,
            "unexpected schema",
        )?;
        ensure(
            spec.sentinel_kind == MemorySentinelKind::PathExists,
            "wrong kind",
        )?;
        ensure(
            spec.safety_class == MemorySentinelSafetyClass::PurePredicate,
            "path sentinel should be a pure predicate",
        )?;
        ensure(
            spec.expected_predicate == "exists",
            "default predicate should be exists",
        )?;
        ensure(
            spec.spec_hash.starts_with(BLAKE3_PREFIX),
            "spec hash should use blake3 prefix",
        )?;

        let rebuilt = MemorySentinelSpec::from_raw(
            &memory_id(),
            "path-exists:docs/adr/0060-verifiable-memory-sentinels.md",
            None,
            "test://sentinel",
            Some(3_600),
        )
        .map_err(|error| error.to_string())?;
        ensure(
            spec.spec_hash == rebuilt.spec_hash,
            "kind spelling normalization should preserve hash",
        )
    }

    #[test]
    fn command_help_sentinel_is_allowlisted_and_rejects_shell_targets() -> TestResult {
        let spec = MemorySentinelSpec::from_raw(
            &memory_id(),
            "command_help_contains_flag:ee pack --require-fresh-sentinels",
            None,
            "test://sentinel",
            None,
        )
        .map_err(|error| error.to_string())?;
        ensure(
            spec.safety_class == MemorySentinelSafetyClass::AllowlistedIntrospection,
            "command help sentinel should be allowlisted introspection",
        )?;

        let error = match MemorySentinelSpec::from_raw(
            &memory_id(),
            "command_help_contains_flag:sh -c 'echo hi'",
            None,
            "test://sentinel",
            None,
        ) {
            Ok(_) => return Err("arbitrary shell target unexpectedly passed".to_owned()),
            Err(error) => error,
        };
        ensure(
            matches!(error, MemorySentinelValidationError::InvalidTarget { .. }),
            format!("unexpected error: {error:?}"),
        )
    }

    #[test]
    fn malformed_specs_carry_repair_hint() -> TestResult {
        let error = match MemorySentinelSpec::from_raw(
            &memory_id(),
            "path_exists",
            None,
            "test://sentinel",
            None,
        ) {
            Ok(_) => return Err("malformed spec unexpectedly passed".to_owned()),
            Err(error) => error,
        };
        ensure(
            error.repair().contains("<kind>:<target>"),
            format!("repair hint was not actionable: {}", error.repair()),
        )
    }

    #[test]
    fn result_hash_is_stable_and_status_sensitive() -> TestResult {
        let spec = MemorySentinelSpec::from_raw(
            &memory_id(),
            "env_var_registered:EE_PACK_TRACE",
            Some("registered"),
            "test://sentinel",
            None,
        )
        .map_err(|error| error.to_string())?;
        let input = MemorySentinelResultInput {
            spec_hash: spec.spec_hash.clone(),
            status: MemorySentinelResultStatus::Pass,
            checked_at: "2026-06-07T20:00:00Z".to_owned(),
            evidence_summary: "EE_PACK_TRACE is present in the env registry.".to_owned(),
            stale_threshold_seconds: Some(600),
        };
        let first = MemorySentinelResult::new(input.clone()).map_err(|error| error.to_string())?;
        let second = MemorySentinelResult::new(input).map_err(|error| error.to_string())?;
        ensure(
            first.result_hash == second.result_hash,
            "result hash should be deterministic",
        )?;

        let changed = MemorySentinelResult::new(MemorySentinelResultInput {
            spec_hash: second.spec_hash,
            status: MemorySentinelResultStatus::Fail,
            checked_at: second.checked_at,
            evidence_summary: second.evidence_summary,
            stale_threshold_seconds: second.stale_threshold_seconds,
        })
        .map_err(|error| error.to_string())?;
        ensure(
            changed.result_hash != first.result_hash,
            "status should affect result hash",
        )
    }

    #[test]
    fn sentinel_observation_maps_conservatively_to_status() {
        assert_eq!(
            SentinelObservation::Satisfied.into_status(),
            MemorySentinelResultStatus::Pass
        );
        assert_eq!(
            SentinelObservation::Unsatisfied.into_status(),
            MemorySentinelResultStatus::Fail
        );
        // Conservatism: an ambiguous / unverifiable check is Unknown, never Fail.
        assert_eq!(
            SentinelObservation::Unverifiable.into_status(),
            MemorySentinelResultStatus::Unknown
        );
        assert_ne!(
            SentinelObservation::Unverifiable.into_status(),
            MemorySentinelResultStatus::Fail
        );
    }
}
