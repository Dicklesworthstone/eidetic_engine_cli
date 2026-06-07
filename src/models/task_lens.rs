//! Task lens policy models.
//!
//! A task lens is a named, inspectable policy overlay. It compiles into
//! existing pack/search/redaction/output knobs in later layers; it never
//! plans work or decides the agent's next action.

use std::collections::BTreeSet;
use std::fmt;
use std::str::FromStr;

use crate::models::{ContextProfileName, MemoryKind, RedactionLevel};

pub const TASK_LENS_SCHEMA_V1: &str = "ee.task_lens.v1";
pub const TASK_LENS_VERSION: u32 = 1;
pub const MAX_WORKSPACE_TASK_LENSES: usize = 16;
pub const MAX_TASK_LENS_ID_BYTES: usize = 64;
pub const MAX_TASK_LENS_DESCRIPTION_BYTES: usize = 512;
pub const MAX_TASK_LENS_FACETS: usize = 16;
pub const MAX_TASK_LENS_KINDS: usize = 16;
pub const MAX_TASK_LENS_TOKENS: u32 = 64_000;
pub const MAX_TASK_LENS_CANDIDATE_POOL: u32 = 10_000;
pub const MAX_TASK_LENS_RESULTS: u32 = 1_000;

pub const BUILTIN_TASK_LENS_IDS: &[&str] = &[
    "bugfix",
    "code-review",
    "release-readiness",
    "dependency-update",
    "schema-contract",
    "performance-investigation",
    "coordination-handoff",
];

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TaskLens {
    pub schema: &'static str,
    pub id: String,
    pub version: u32,
    pub description: String,
    pub overlay: TaskLensOverlay,
    pub lens_hash: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TaskLensOverlay {
    pub context_profile: Option<String>,
    pub source_mode: Option<String>,
    pub strict_source_mode: Option<bool>,
    pub pack_profile: Option<String>,
    pub resource_profile: Option<String>,
    pub redaction: Option<RedactionLevel>,
    pub memory_scope: Option<String>,
    pub max_tokens: Option<u32>,
    pub candidate_pool: Option<u32>,
    pub max_results: Option<u32>,
    pub coverage_facets: Vec<String>,
    pub allowed_kinds: Vec<String>,
    pub deprioritized_kinds: Vec<String>,
}

impl TaskLensOverlay {
    #[must_use]
    pub fn normalized(mut self) -> Self {
        self.context_profile = self.context_profile.map(|value| {
            ContextProfileName::parse(&value).map_or_else(
                || normalized_token(&value, '_'),
                |profile| profile.as_str().to_owned(),
            )
        });
        self.source_mode = self.source_mode.map(normalized_source_mode_token);
        self.pack_profile = self.pack_profile.map(|value| normalized_token(&value, '_'));
        self.resource_profile = self
            .resource_profile
            .map(|value| normalized_token(&value, '_'));
        self.memory_scope = self.memory_scope.map(|value| normalized_token(&value, '_'));
        self.coverage_facets = normalized_unique_identifiers(self.coverage_facets, '-');
        self.allowed_kinds = normalized_unique_memory_kinds(self.allowed_kinds);
        self.deprioritized_kinds = normalized_unique_memory_kinds(self.deprioritized_kinds);
        self
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TaskLensInput {
    pub id: String,
    pub version: u32,
    pub description: String,
    pub overlay: TaskLensOverlay,
}

impl TaskLens {
    /// Build a validated task lens and compute its stable hash.
    ///
    /// # Errors
    ///
    /// Returns [`TaskLensValidationError`] when the id, description, version,
    /// or any overlay field is outside the supported schema.
    pub fn new(input: TaskLensInput) -> Result<Self, TaskLensValidationError> {
        let id = validate_lens_id(&input.id)?;
        if input.version == 0 {
            return Err(TaskLensValidationError::InvalidVersion { id });
        }
        let description = validate_description(&input.description, &id)?;
        let overlay = validate_overlay(input.overlay.normalized(), &id)?;
        let lens_hash = stable_lens_hash(&id, input.version, &description, &overlay);
        Ok(Self {
            schema: TASK_LENS_SCHEMA_V1,
            id,
            version: input.version,
            description,
            overlay,
            lens_hash,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TaskLensCatalog {
    pub lenses: Vec<TaskLens>,
}

impl TaskLensCatalog {
    /// Return built-ins plus validated workspace overrides.
    ///
    /// Workspace lenses replace a built-in when they use the same id, and add a
    /// custom lens otherwise. Final ordering is stable by id.
    ///
    /// # Errors
    ///
    /// Returns [`TaskLensValidationError`] if built-ins or overrides are invalid.
    pub fn with_workspace_overrides(
        overrides: Vec<TaskLens>,
    ) -> Result<Self, TaskLensValidationError> {
        if overrides.len() > MAX_WORKSPACE_TASK_LENSES {
            return Err(TaskLensValidationError::TooManyWorkspaceLenses {
                count: overrides.len(),
                max: MAX_WORKSPACE_TASK_LENSES,
            });
        }

        let mut lenses = builtin_task_lenses()?;
        for override_lens in overrides {
            if let Some(existing) = lenses.iter_mut().find(|lens| lens.id == override_lens.id) {
                *existing = override_lens;
            } else {
                lenses.push(override_lens);
            }
        }
        lenses.sort_by(|left, right| left.id.cmp(&right.id));
        Ok(Self { lenses })
    }

    #[must_use]
    pub fn get(&self, id: &str) -> Option<&TaskLens> {
        let normalized = normalize_lens_id(id);
        self.lenses.iter().find(|lens| lens.id == normalized)
    }
}

pub fn builtin_task_lenses() -> Result<Vec<TaskLens>, TaskLensValidationError> {
    BUILTIN_TASK_LENS_IDS
        .iter()
        .map(|id| builtin_task_lens(id))
        .collect()
}

pub fn builtin_task_lens(id: &str) -> Result<TaskLens, TaskLensValidationError> {
    let id = normalize_lens_id(id);
    let input = match id.as_str() {
        "bugfix" => TaskLensInput {
            id,
            version: TASK_LENS_VERSION,
            description:
                "Prioritize failures, risks, commands, and verification evidence for a bug fix."
                    .to_owned(),
            overlay: TaskLensOverlay {
                context_profile: Some("thorough".to_owned()),
                source_mode: Some("hybrid".to_owned()),
                pack_profile: Some("standard".to_owned()),
                resource_profile: Some("standard".to_owned()),
                redaction: Some(RedactionLevel::Minimal),
                memory_scope: Some("workspace".to_owned()),
                max_tokens: Some(6_000),
                candidate_pool: Some(160),
                coverage_facets: strings(&["reproduction", "root-cause", "verification"]),
                allowed_kinds: strings(&["failure", "risk", "anti-pattern", "command", "decision"]),
                deprioritized_kinds: strings(&["fact"]),
                ..TaskLensOverlay::default()
            },
        },
        "code-review" => TaskLensInput {
            id,
            version: TASK_LENS_VERSION,
            description: "Bias toward contracts, tests, regressions, and review-risk memories."
                .to_owned(),
            overlay: TaskLensOverlay {
                context_profile: Some("grounding".to_owned()),
                source_mode: Some("hybrid".to_owned()),
                pack_profile: Some("standard".to_owned()),
                resource_profile: Some("standard".to_owned()),
                redaction: Some(RedactionLevel::Standard),
                memory_scope: Some("workspace".to_owned()),
                max_tokens: Some(5_000),
                candidate_pool: Some(160),
                coverage_facets: strings(&["contracts", "tests", "regression-risk"]),
                allowed_kinds: strings(&[
                    "risk",
                    "anti-pattern",
                    "decision",
                    "convention",
                    "failure",
                ]),
                deprioritized_kinds: strings(&["command"]),
                ..TaskLensOverlay::default()
            },
        },
        "release-readiness" => TaskLensInput {
            id,
            version: TASK_LENS_VERSION,
            description: "Expand release rules, prior failures, commands, and operational risks."
                .to_owned(),
            overlay: TaskLensOverlay {
                context_profile: Some("thorough".to_owned()),
                source_mode: Some("hybrid".to_owned()),
                pack_profile: Some("verbose".to_owned()),
                resource_profile: Some("swarm_heavy".to_owned()),
                redaction: Some(RedactionLevel::Standard),
                memory_scope: Some("workspace".to_owned()),
                max_tokens: Some(8_000),
                candidate_pool: Some(240),
                coverage_facets: strings(&["release-gates", "signing", "verification", "rollback"]),
                allowed_kinds: strings(&["rule", "failure", "command", "convention", "risk"]),
                deprioritized_kinds: Vec::new(),
                ..TaskLensOverlay::default()
            },
        },
        "dependency-update" => TaskLensInput {
            id,
            version: TASK_LENS_VERSION,
            description:
                "Focus on dependency risks, update decisions, compatibility failures, and commands."
                    .to_owned(),
            overlay: TaskLensOverlay {
                context_profile: Some("balanced".to_owned()),
                source_mode: Some("hybrid".to_owned()),
                pack_profile: Some("standard".to_owned()),
                resource_profile: Some("standard".to_owned()),
                redaction: Some(RedactionLevel::Standard),
                memory_scope: Some("workspace".to_owned()),
                max_tokens: Some(5_000),
                candidate_pool: Some(160),
                coverage_facets: strings(&["dependency-risk", "version-policy", "verification"]),
                allowed_kinds: strings(&["failure", "risk", "decision", "command", "convention"]),
                deprioritized_kinds: strings(&["playbook-step"]),
                ..TaskLensOverlay::default()
            },
        },
        "schema-contract" => TaskLensInput {
            id,
            version: TASK_LENS_VERSION,
            description: "Prefer exact schema, contract, migration, and degraded-code evidence."
                .to_owned(),
            overlay: TaskLensOverlay {
                context_profile: Some("grounding".to_owned()),
                source_mode: Some("lexical_only".to_owned()),
                strict_source_mode: Some(false),
                pack_profile: Some("standard".to_owned()),
                resource_profile: Some("standard".to_owned()),
                redaction: Some(RedactionLevel::Standard),
                memory_scope: Some("workspace".to_owned()),
                max_tokens: Some(5_500),
                candidate_pool: Some(180),
                coverage_facets: strings(&["schema", "migration", "degraded-code", "contract"]),
                allowed_kinds: strings(&["decision", "convention", "rule", "failure", "risk"]),
                deprioritized_kinds: strings(&["fact"]),
                ..TaskLensOverlay::default()
            },
        },
        "performance-investigation" => TaskLensInput {
            id,
            version: TASK_LENS_VERSION,
            description:
                "Expand profiling, benchmark, negative-evidence, and optimization memories."
                    .to_owned(),
            overlay: TaskLensOverlay {
                context_profile: Some("thorough".to_owned()),
                source_mode: Some("hybrid".to_owned()),
                pack_profile: Some("verbose".to_owned()),
                resource_profile: Some("swarm_heavy".to_owned()),
                redaction: Some(RedactionLevel::Standard),
                memory_scope: Some("workspace".to_owned()),
                max_tokens: Some(8_000),
                candidate_pool: Some(260),
                coverage_facets: strings(&[
                    "profiling",
                    "benchmark",
                    "negative-evidence",
                    "regression",
                ]),
                allowed_kinds: strings(&["failure", "decision", "command", "risk", "anti-pattern"]),
                deprioritized_kinds: strings(&["fact"]),
                ..TaskLensOverlay::default()
            },
        },
        "coordination-handoff" => TaskLensInput {
            id,
            version: TASK_LENS_VERSION,
            description: "Produce compact, redaction-strict context for crowded checkout handoff."
                .to_owned(),
            overlay: TaskLensOverlay {
                context_profile: Some("orientation".to_owned()),
                source_mode: Some("lexical_only".to_owned()),
                pack_profile: Some("lean".to_owned()),
                resource_profile: Some("lean".to_owned()),
                redaction: Some(RedactionLevel::Strict),
                memory_scope: Some("workspace".to_owned()),
                max_tokens: Some(3_000),
                candidate_pool: Some(80),
                coverage_facets: strings(&[
                    "coordination",
                    "reservations",
                    "handoff",
                    "verification",
                ]),
                allowed_kinds: strings(&["rule", "decision", "failure", "convention", "command"]),
                deprioritized_kinds: strings(&["fact"]),
                ..TaskLensOverlay::default()
            },
        },
        other => {
            return Err(TaskLensValidationError::UnknownBuiltin {
                id: other.to_owned(),
            });
        }
    };
    TaskLens::new(input)
}

fn validate_overlay(
    overlay: TaskLensOverlay,
    id: &str,
) -> Result<TaskLensOverlay, TaskLensValidationError> {
    if let Some(value) = overlay.context_profile.as_deref() {
        ContextProfileName::parse(value).ok_or_else(|| TaskLensValidationError::InvalidField {
            id: id.to_owned(),
            field: "context_profile",
            value: value.to_owned(),
            expected: "compact, balanced, grounding, orientation, thorough, or submodular"
                .to_owned(),
        })?;
    }
    if let Some(value) = overlay.source_mode.as_deref() {
        validate_enum(
            id,
            "source_mode",
            value,
            &["lexical_only", "semantic_only", "hybrid"],
        )?;
    }
    if let Some(value) = overlay.pack_profile.as_deref() {
        validate_enum(id, "pack_profile", value, &["lean", "standard", "verbose"])?;
    }
    if let Some(value) = overlay.resource_profile.as_deref() {
        validate_enum(
            id,
            "resource_profile",
            value,
            &["lean", "standard", "swarm_heavy"],
        )?;
    }
    if let Some(value) = overlay.memory_scope.as_deref() {
        validate_enum(
            id,
            "memory_scope",
            value,
            &["self", "team", "workspace", "verified", "swarm"],
        )?;
    }
    if overlay.redaction == Some(RedactionLevel::Full) {
        return Err(TaskLensValidationError::InvalidField {
            id: id.to_owned(),
            field: "redaction",
            value: RedactionLevel::Full.as_str().to_owned(),
            expected: "none, minimal, standard, strict, or paranoid".to_owned(),
        });
    }
    validate_positive_cap(id, "max_tokens", overlay.max_tokens, MAX_TASK_LENS_TOKENS)?;
    validate_positive_cap(
        id,
        "candidate_pool",
        overlay.candidate_pool,
        MAX_TASK_LENS_CANDIDATE_POOL,
    )?;
    validate_positive_cap(
        id,
        "max_results",
        overlay.max_results,
        MAX_TASK_LENS_RESULTS,
    )?;
    validate_identifier_list(
        id,
        "coverage_facets",
        &overlay.coverage_facets,
        MAX_TASK_LENS_FACETS,
    )?;
    validate_memory_kind_list(id, "allowed_kinds", &overlay.allowed_kinds)?;
    validate_memory_kind_list(id, "deprioritized_kinds", &overlay.deprioritized_kinds)?;
    validate_no_kind_overlap(id, &overlay.allowed_kinds, &overlay.deprioritized_kinds)?;
    Ok(overlay)
}

fn validate_lens_id(input: &str) -> Result<String, TaskLensValidationError> {
    let normalized = normalize_lens_id(input);
    if normalized.is_empty() {
        return Err(TaskLensValidationError::InvalidId {
            input: input.to_owned(),
            reason: "id must not be empty",
        });
    }
    if normalized.len() > MAX_TASK_LENS_ID_BYTES {
        return Err(TaskLensValidationError::InvalidId {
            input: input.to_owned(),
            reason: "id is too long",
        });
    }
    if !is_valid_identifier(&normalized) {
        return Err(TaskLensValidationError::InvalidId {
            input: input.to_owned(),
            reason: "id must contain only lowercase ASCII letters, digits, and hyphens",
        });
    }
    Ok(normalized)
}

fn validate_description(input: &str, id: &str) -> Result<String, TaskLensValidationError> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(TaskLensValidationError::InvalidDescription {
            id: id.to_owned(),
            reason: "description must not be empty",
        });
    }
    if trimmed.len() > MAX_TASK_LENS_DESCRIPTION_BYTES {
        return Err(TaskLensValidationError::InvalidDescription {
            id: id.to_owned(),
            reason: "description is too long",
        });
    }
    Ok(trimmed.to_owned())
}

fn validate_enum(
    id: &str,
    field: &'static str,
    value: &str,
    allowed: &'static [&'static str],
) -> Result<(), TaskLensValidationError> {
    if allowed.contains(&value) {
        Ok(())
    } else {
        Err(TaskLensValidationError::InvalidField {
            id: id.to_owned(),
            field,
            value: value.to_owned(),
            expected: allowed.join(", "),
        })
    }
}

fn validate_positive_cap(
    id: &str,
    field: &'static str,
    value: Option<u32>,
    max: u32,
) -> Result<(), TaskLensValidationError> {
    if let Some(value) = value {
        if value == 0 || value > max {
            return Err(TaskLensValidationError::InvalidNumericField {
                id: id.to_owned(),
                field,
                value,
                min: 1,
                max,
            });
        }
    }
    Ok(())
}

fn validate_identifier_list(
    id: &str,
    field: &'static str,
    values: &[String],
    max: usize,
) -> Result<(), TaskLensValidationError> {
    if values.len() > max {
        return Err(TaskLensValidationError::TooManyValues {
            id: id.to_owned(),
            field,
            count: values.len(),
            max,
        });
    }
    for value in values {
        if !is_valid_identifier(value) {
            return Err(TaskLensValidationError::InvalidField {
                id: id.to_owned(),
                field,
                value: value.to_owned(),
                expected: "lowercase ASCII identifier tokens separated by hyphens".to_owned(),
            });
        }
    }
    Ok(())
}

fn validate_memory_kind_list(
    id: &str,
    field: &'static str,
    values: &[String],
) -> Result<(), TaskLensValidationError> {
    if values.len() > MAX_TASK_LENS_KINDS {
        return Err(TaskLensValidationError::TooManyValues {
            id: id.to_owned(),
            field,
            count: values.len(),
            max: MAX_TASK_LENS_KINDS,
        });
    }
    for value in values {
        MemoryKind::from_str(value).map_err(|_| TaskLensValidationError::InvalidField {
            id: id.to_owned(),
            field,
            value: value.to_owned(),
            expected: "a known or custom memory kind identifier".to_owned(),
        })?;
    }
    Ok(())
}

fn validate_no_kind_overlap(
    id: &str,
    allowed: &[String],
    deprioritized: &[String],
) -> Result<(), TaskLensValidationError> {
    let allowed: BTreeSet<&str> = allowed.iter().map(String::as_str).collect();
    for value in deprioritized {
        if allowed.contains(value.as_str()) {
            return Err(TaskLensValidationError::OverlappingKind {
                id: id.to_owned(),
                kind: value.to_owned(),
            });
        }
    }
    Ok(())
}

fn stable_lens_hash(
    id: &str,
    version: u32,
    description: &str,
    overlay: &TaskLensOverlay,
) -> String {
    let mut hasher = blake3::Hasher::new();
    hash_part(&mut hasher, TASK_LENS_SCHEMA_V1);
    hash_part(&mut hasher, id);
    hash_part(&mut hasher, &version.to_string());
    hash_part(&mut hasher, description);
    hash_opt(&mut hasher, overlay.context_profile.as_deref());
    hash_opt(&mut hasher, overlay.source_mode.as_deref());
    hash_opt_bool(&mut hasher, overlay.strict_source_mode);
    hash_opt(&mut hasher, overlay.pack_profile.as_deref());
    hash_opt(&mut hasher, overlay.resource_profile.as_deref());
    hash_opt(&mut hasher, overlay.redaction.map(RedactionLevel::as_str));
    hash_opt(&mut hasher, overlay.memory_scope.as_deref());
    hash_opt_u32(&mut hasher, overlay.max_tokens);
    hash_opt_u32(&mut hasher, overlay.candidate_pool);
    hash_opt_u32(&mut hasher, overlay.max_results);
    hash_list(&mut hasher, &overlay.coverage_facets);
    hash_list(&mut hasher, &overlay.allowed_kinds);
    hash_list(&mut hasher, &overlay.deprioritized_kinds);
    format!("blake3:{}", hasher.finalize().to_hex())
}

fn hash_part(hasher: &mut blake3::Hasher, value: &str) {
    hasher.update(&(value.len() as u64).to_le_bytes());
    hasher.update(value.as_bytes());
}

fn hash_opt(hasher: &mut blake3::Hasher, value: Option<&str>) {
    match value {
        Some(value) => {
            hasher.update(&[1]);
            hash_part(hasher, value);
        }
        None => {
            hasher.update(&[0]);
        }
    };
}

fn hash_opt_bool(hasher: &mut blake3::Hasher, value: Option<bool>) {
    match value {
        Some(true) => {
            hasher.update(&[1, 1]);
        }
        Some(false) => {
            hasher.update(&[1, 0]);
        }
        None => {
            hasher.update(&[0]);
        }
    };
}

fn hash_opt_u32(hasher: &mut blake3::Hasher, value: Option<u32>) {
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

fn hash_list(hasher: &mut blake3::Hasher, values: &[String]) {
    hasher.update(&(values.len() as u64).to_le_bytes());
    for value in values {
        hash_part(hasher, value);
    }
}

fn normalize_lens_id(input: &str) -> String {
    normalized_token(input, '-')
}

fn normalized_source_mode_token(input: String) -> String {
    normalized_token(&input, '_')
}

fn normalized_token(input: &str, separator: char) -> String {
    let mut normalized = String::with_capacity(input.len());
    let mut previous_was_separator = false;
    let mut previous_was_lowercase_or_digit = false;

    for character in input.trim().chars() {
        match character {
            '-' | '_' | ' ' => {
                if !normalized.is_empty() && !previous_was_separator {
                    normalized.push(separator);
                }
                previous_was_separator = true;
                previous_was_lowercase_or_digit = false;
            }
            ch if ch.is_ascii_uppercase() => {
                if previous_was_lowercase_or_digit && !previous_was_separator {
                    normalized.push(separator);
                }
                normalized.push(ch.to_ascii_lowercase());
                previous_was_separator = false;
                previous_was_lowercase_or_digit = false;
            }
            ch => {
                normalized.push(ch.to_ascii_lowercase());
                previous_was_separator = false;
                previous_was_lowercase_or_digit = ch.is_ascii_lowercase() || ch.is_ascii_digit();
            }
        }
    }

    while normalized.ends_with(separator) {
        normalized.pop();
    }
    normalized
}

fn normalized_unique_identifiers(values: Vec<String>, separator: char) -> Vec<String> {
    let mut seen = BTreeSet::new();
    for value in values {
        let normalized = normalized_token(&value, separator);
        if !normalized.is_empty() {
            seen.insert(normalized);
        }
    }
    seen.into_iter().collect()
}

fn normalized_unique_memory_kinds(values: Vec<String>) -> Vec<String> {
    let mut seen = BTreeSet::new();
    for value in values {
        if let Ok(kind) = MemoryKind::from_str(&value) {
            seen.insert(kind.as_str().to_owned());
        } else {
            let normalized = normalized_token(&value, '-');
            if !normalized.is_empty() {
                seen.insert(normalized);
            }
        }
    }
    seen.into_iter().collect()
}

fn is_valid_identifier(value: &str) -> bool {
    !value.is_empty()
        && !value.starts_with('-')
        && !value.ends_with('-')
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TaskLensValidationError {
    UnknownBuiltin {
        id: String,
    },
    InvalidId {
        input: String,
        reason: &'static str,
    },
    InvalidVersion {
        id: String,
    },
    InvalidDescription {
        id: String,
        reason: &'static str,
    },
    InvalidField {
        id: String,
        field: &'static str,
        value: String,
        expected: String,
    },
    InvalidNumericField {
        id: String,
        field: &'static str,
        value: u32,
        min: u32,
        max: u32,
    },
    TooManyValues {
        id: String,
        field: &'static str,
        count: usize,
        max: usize,
    },
    TooManyWorkspaceLenses {
        count: usize,
        max: usize,
    },
    OverlappingKind {
        id: String,
        kind: String,
    },
}

impl fmt::Display for TaskLensValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownBuiltin { id } => write!(formatter, "unknown built-in task lens `{id}`"),
            Self::InvalidId { input, reason } => {
                write!(formatter, "invalid task lens id `{input}`: {reason}")
            }
            Self::InvalidVersion { id } => {
                write!(
                    formatter,
                    "task lens `{id}` version must be greater than zero"
                )
            }
            Self::InvalidDescription { id, reason } => {
                write!(
                    formatter,
                    "task lens `{id}` description is invalid: {reason}"
                )
            }
            Self::InvalidField {
                id,
                field,
                value,
                expected,
            } => write!(
                formatter,
                "task lens `{id}` field `{field}` has invalid value `{value}`; expected {}",
                expected
            ),
            Self::InvalidNumericField {
                id,
                field,
                value,
                min,
                max,
            } => write!(
                formatter,
                "task lens `{id}` field `{field}` has invalid value `{value}`; expected {min}..={max}"
            ),
            Self::TooManyValues {
                id,
                field,
                count,
                max,
            } => write!(
                formatter,
                "task lens `{id}` field `{field}` has {count} values; max is {max}"
            ),
            Self::TooManyWorkspaceLenses { count, max } => {
                write!(
                    formatter,
                    "workspace defines {count} task lenses; max is {max}"
                )
            }
            Self::OverlappingKind { id, kind } => write!(
                formatter,
                "task lens `{id}` lists memory kind `{kind}` as both allowed and deprioritized"
            ),
        }
    }
}

impl std::error::Error for TaskLensValidationError {}

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

    #[test]
    fn builtins_are_complete_and_have_stable_hashes() -> TestResult {
        let lenses = builtin_task_lenses().map_err(|error| error.to_string())?;
        ensure(
            lenses.len() == BUILTIN_TASK_LENS_IDS.len(),
            "missing built-in lens",
        )?;
        for id in BUILTIN_TASK_LENS_IDS {
            let lens = lenses
                .iter()
                .find(|lens| lens.id == *id)
                .ok_or_else(|| format!("missing built-in {id}"))?;
            ensure(
                lens.lens_hash.starts_with("blake3:"),
                format!("{id} hash must use blake3 prefix"),
            )?;
            let rebuilt = builtin_task_lens(id).map_err(|error| error.to_string())?;
            ensure(
                rebuilt.lens_hash == lens.lens_hash,
                format!("{id} hash is not deterministic"),
            )?;
        }
        Ok(())
    }

    #[test]
    fn workspace_override_replaces_builtin_by_id() -> TestResult {
        let override_lens = TaskLens::new(TaskLensInput {
            id: "BugFix".to_owned(),
            version: 2,
            description: "Local bugfix policy.".to_owned(),
            overlay: TaskLensOverlay {
                context_profile: Some("compact".to_owned()),
                allowed_kinds: strings(&["failure"]),
                ..TaskLensOverlay::default()
            },
        })
        .map_err(|error| error.to_string())?;
        let catalog = TaskLensCatalog::with_workspace_overrides(vec![override_lens.clone()])
            .map_err(|error| error.to_string())?;
        let lens = catalog
            .get("bugfix")
            .ok_or_else(|| "bugfix override missing".to_owned())?;
        ensure(
            lens.version == 2,
            "override version did not replace built-in",
        )?;
        ensure(
            lens.lens_hash == override_lens.lens_hash,
            "override hash did not replace built-in hash",
        )
    }

    #[test]
    fn invalid_override_field_is_rejected() -> TestResult {
        let error = match TaskLens::new(TaskLensInput {
            id: "review".to_owned(),
            version: 1,
            description: "Review.".to_owned(),
            overlay: TaskLensOverlay {
                source_mode: Some("fast".to_owned()),
                ..TaskLensOverlay::default()
            },
        }) {
            Ok(_) => return Err("invalid source mode unexpectedly passed".to_owned()),
            Err(error) => error,
        };
        ensure(
            matches!(
                error,
                TaskLensValidationError::InvalidField {
                    field: "source_mode",
                    ..
                }
            ),
            format!("unexpected error: {error:?}"),
        )
    }
}
