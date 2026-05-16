//! Context profile domain model (EE-290).
//!
//! Context profiles are stable, versioned policy records that describe the
//! retrieval objective and section quota mix used by context packing.

use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::{Value as JsonValue, json};

/// Schema identifier for context profile records.
pub const CONTEXT_PROFILE_SCHEMA_V1: &str = "ee.context.profile.v1";

/// Schema identifier for the context profile schema catalog.
pub const CONTEXT_PROFILE_SCHEMA_CATALOG_V1: &str = "ee.context.profile.schemas.v1";
pub const AGENT_CONTEXT_PROFILE_SCHEMA_V1: &str = "ee.context.agent_profile.v1";

const JSON_SCHEMA_DRAFT_2020_12: &str = "https://json-schema.org/draft/2020-12/schema";
const TOTAL_BASIS_POINTS: u16 = 10_000;
pub const AGENT_PROFILE_BIAS_CAP: f64 = 0.05;
pub const AGENT_PROFILE_COLD_START_OUTCOMES: u32 = 10;

/// Outcome counts learned for one agent/memory pair.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentContextProfileCounts {
    pub helpful_count: u32,
    pub harmful_count: u32,
    pub ignored_count: u32,
}

impl AgentContextProfileCounts {
    #[must_use]
    pub const fn new(helpful_count: u32, harmful_count: u32, ignored_count: u32) -> Self {
        Self {
            helpful_count,
            harmful_count,
            ignored_count,
        }
    }

    #[must_use]
    pub const fn observed_outcomes(self) -> u32 {
        self.helpful_count
            .saturating_add(self.harmful_count)
            .saturating_add(self.ignored_count)
    }

    #[must_use]
    pub fn bias(self) -> AgentContextProfileBias {
        let observed_outcomes = self.observed_outcomes();
        if observed_outcomes < AGENT_PROFILE_COLD_START_OUTCOMES {
            return AgentContextProfileBias {
                observed_outcomes,
                weight: 0.0,
                cold_start: true,
            };
        }

        let helpful = f64::from(self.helpful_count).ln_1p();
        let harmful = f64::from(self.harmful_count).ln_1p();
        let ignored_damping = f64::from(self.ignored_count).ln_1p().max(1.0);
        let raw = AGENT_PROFILE_BIAS_CAP * (helpful - harmful) / ignored_damping;

        AgentContextProfileBias {
            observed_outcomes,
            weight: raw.clamp(-AGENT_PROFILE_BIAS_CAP, AGENT_PROFILE_BIAS_CAP),
            cold_start: false,
        }
    }

    #[must_use]
    pub fn decayed(self, age_days: f64, half_life_days: f64) -> AgentContextProfileDecayedCounts {
        let factor = decay_factor(age_days, half_life_days);
        AgentContextProfileDecayedCounts {
            helpful_count: f64::from(self.helpful_count) * factor,
            harmful_count: f64::from(self.harmful_count) * factor,
            ignored_count: f64::from(self.ignored_count) * factor,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentContextProfileDecayedCounts {
    pub helpful_count: f64,
    pub harmful_count: f64,
    pub ignored_count: f64,
}

impl AgentContextProfileDecayedCounts {
    #[must_use]
    pub fn observed_outcomes(self) -> f64 {
        self.helpful_count + self.harmful_count + self.ignored_count
    }

    #[must_use]
    pub fn bias(self) -> AgentContextProfileBias {
        let observed_outcomes = self.observed_outcomes();
        if observed_outcomes < f64::from(AGENT_PROFILE_COLD_START_OUTCOMES) {
            return AgentContextProfileBias {
                observed_outcomes: observed_outcomes.floor() as u32,
                weight: 0.0,
                cold_start: true,
            };
        }

        let helpful = self.helpful_count.ln_1p();
        let harmful = self.harmful_count.ln_1p();
        let ignored_damping = self.ignored_count.ln_1p().max(1.0);
        let raw = AGENT_PROFILE_BIAS_CAP * (helpful - harmful) / ignored_damping;

        AgentContextProfileBias {
            observed_outcomes: observed_outcomes.floor() as u32,
            weight: raw.clamp(-AGENT_PROFILE_BIAS_CAP, AGENT_PROFILE_BIAS_CAP),
            cold_start: false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentContextProfileBias {
    pub observed_outcomes: u32,
    pub weight: f64,
    pub cold_start: bool,
}

#[must_use]
pub fn decay_factor(age_days: f64, half_life_days: f64) -> f64 {
    if !age_days.is_finite() || age_days <= 0.0 {
        return 1.0;
    }
    if !half_life_days.is_finite() || half_life_days <= 0.0 {
        return 0.0;
    }
    0.5_f64.powf(age_days / half_life_days)
}

/// Built-in context profile name.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextProfileName {
    Compact,
    Balanced,
    Thorough,
    Submodular,
}

impl ContextProfileName {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Compact => "compact",
            Self::Balanced => "balanced",
            Self::Thorough => "thorough",
            Self::Submodular => "submodular",
        }
    }

    #[must_use]
    pub const fn all() -> [Self; 4] {
        [
            Self::Compact,
            Self::Balanced,
            Self::Thorough,
            Self::Submodular,
        ]
    }

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "compact" => Some(Self::Compact),
            "balanced" => Some(Self::Balanced),
            "thorough" => Some(Self::Thorough),
            "submodular" => Some(Self::Submodular),
            _ => None,
        }
    }
}

impl fmt::Display for ContextProfileName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Selection objective used by a context profile.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextProfileObjective {
    MmrRedundancy,
    FacilityLocation,
}

impl ContextProfileObjective {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MmrRedundancy => "mmr_redundancy",
            Self::FacilityLocation => "facility_location",
        }
    }

    #[must_use]
    pub const fn all() -> [Self; 2] {
        [Self::MmrRedundancy, Self::FacilityLocation]
    }

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "mmr_redundancy" | "mmr" => Some(Self::MmrRedundancy),
            "facility_location" | "submodular" => Some(Self::FacilityLocation),
            _ => None,
        }
    }
}

impl fmt::Display for ContextProfileObjective {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Context pack section addressed by profile quotas.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextProfileSection {
    ProceduralRules,
    Decisions,
    Failures,
    Evidence,
    Artifacts,
}

impl ContextProfileSection {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ProceduralRules => "procedural_rules",
            Self::Decisions => "decisions",
            Self::Failures => "failures",
            Self::Evidence => "evidence",
            Self::Artifacts => "artifacts",
        }
    }

    #[must_use]
    pub const fn all() -> [Self; 5] {
        [
            Self::ProceduralRules,
            Self::Decisions,
            Self::Failures,
            Self::Evidence,
            Self::Artifacts,
        ]
    }

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        let trimmed = value.trim();
        if trimmed.eq_ignore_ascii_case("procedural_rules") {
            Some(Self::ProceduralRules)
        } else if trimmed.eq_ignore_ascii_case("decisions") {
            Some(Self::Decisions)
        } else if trimmed.eq_ignore_ascii_case("failures") {
            Some(Self::Failures)
        } else if trimmed.eq_ignore_ascii_case("evidence") {
            Some(Self::Evidence)
        } else if trimmed.eq_ignore_ascii_case("artifacts") {
            Some(Self::Artifacts)
        } else {
            None
        }
    }
}

impl fmt::Display for ContextProfileSection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Deterministic section quota mix expressed in basis points.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ContextProfileSectionMix {
    pub procedural_rules_bps: u16,
    pub decisions_bps: u16,
    pub failures_bps: u16,
    pub evidence_bps: u16,
    pub artifacts_bps: u16,
}

impl ContextProfileSectionMix {
    #[must_use]
    pub const fn new(
        procedural_rules_bps: u16,
        decisions_bps: u16,
        failures_bps: u16,
        evidence_bps: u16,
        artifacts_bps: u16,
    ) -> Self {
        Self {
            procedural_rules_bps,
            decisions_bps,
            failures_bps,
            evidence_bps,
            artifacts_bps,
        }
    }

    #[must_use]
    pub const fn total_bps(self) -> u16 {
        self.procedural_rules_bps
            .saturating_add(self.decisions_bps)
            .saturating_add(self.failures_bps)
            .saturating_add(self.evidence_bps)
            .saturating_add(self.artifacts_bps)
    }

    #[must_use]
    pub const fn is_normalized(self) -> bool {
        self.total_bps() == TOTAL_BASIS_POINTS
    }

    #[must_use]
    pub const fn weight_bps(self, section: ContextProfileSection) -> u16 {
        match section {
            ContextProfileSection::ProceduralRules => self.procedural_rules_bps,
            ContextProfileSection::Decisions => self.decisions_bps,
            ContextProfileSection::Failures => self.failures_bps,
            ContextProfileSection::Evidence => self.evidence_bps,
            ContextProfileSection::Artifacts => self.artifacts_bps,
        }
    }

    #[must_use]
    pub fn data_json(self) -> JsonValue {
        json!({
            "proceduralRulesBps": self.procedural_rules_bps,
            "decisionsBps": self.decisions_bps,
            "failuresBps": self.failures_bps,
            "evidenceBps": self.evidence_bps,
            "artifactsBps": self.artifacts_bps,
            "totalBps": self.total_bps(),
        })
    }
}

/// Built-in context profile definition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ContextProfile {
    pub schema: &'static str,
    pub name: ContextProfileName,
    pub display_name: &'static str,
    pub objective: ContextProfileObjective,
    pub section_mix: ContextProfileSectionMix,
    pub default_candidate_pool: u32,
    pub description: &'static str,
}

impl ContextProfile {
    #[must_use]
    pub const fn builtin(name: ContextProfileName) -> Self {
        match name {
            ContextProfileName::Compact => Self {
                schema: CONTEXT_PROFILE_SCHEMA_V1,
                name,
                display_name: "Compact",
                objective: ContextProfileObjective::MmrRedundancy,
                section_mix: ContextProfileSectionMix::new(5_000, 1_500, 2_000, 1_000, 500),
                default_candidate_pool: 64,
                description: "Prioritizes compact procedural guidance and known failure modes.",
            },
            ContextProfileName::Balanced => Self {
                schema: CONTEXT_PROFILE_SCHEMA_V1,
                name,
                display_name: "Balanced",
                objective: ContextProfileObjective::MmrRedundancy,
                section_mix: ContextProfileSectionMix::new(3_000, 2_000, 2_000, 2_000, 1_000),
                default_candidate_pool: 64,
                description: "Balances rules, decisions, failures, evidence, and artifacts.",
            },
            ContextProfileName::Thorough => Self {
                schema: CONTEXT_PROFILE_SCHEMA_V1,
                name,
                display_name: "Thorough",
                objective: ContextProfileObjective::MmrRedundancy,
                section_mix: ContextProfileSectionMix::new(2_000, 2_000, 2_000, 2_500, 1_500),
                default_candidate_pool: 64,
                description: "Expands evidence and artifact coverage for higher-recall work.",
            },
            ContextProfileName::Submodular => Self {
                schema: CONTEXT_PROFILE_SCHEMA_V1,
                name,
                display_name: "Submodular",
                objective: ContextProfileObjective::FacilityLocation,
                section_mix: ContextProfileSectionMix::new(2_000, 2_000, 2_000, 2_500, 1_500),
                default_candidate_pool: 64,
                description: "Uses the facility-location objective with thorough section quotas.",
            },
        }
    }

    #[must_use]
    pub const fn builtins() -> [Self; 4] {
        [
            Self::builtin(ContextProfileName::Compact),
            Self::builtin(ContextProfileName::Balanced),
            Self::builtin(ContextProfileName::Thorough),
            Self::builtin(ContextProfileName::Submodular),
        ]
    }

    /// Validate deterministic profile invariants.
    ///
    /// # Errors
    ///
    /// Returns [`ContextProfileValidationError`] if the profile is not usable
    /// for deterministic context packing.
    pub fn validate(self) -> Result<(), ContextProfileValidationError> {
        if self.display_name.trim().is_empty() {
            return Err(ContextProfileValidationError::EmptyDisplayName);
        }
        if self.default_candidate_pool == 0 {
            return Err(ContextProfileValidationError::ZeroCandidatePool);
        }
        if !self.section_mix.is_normalized() {
            return Err(ContextProfileValidationError::UnnormalizedSectionMix {
                total_bps: self.section_mix.total_bps(),
            });
        }
        Ok(())
    }

    #[must_use]
    pub fn data_json(self) -> JsonValue {
        json!({
            "schema": self.schema,
            "name": self.name.as_str(),
            "displayName": self.display_name,
            "objective": self.objective.as_str(),
            "sectionMix": self.section_mix.data_json(),
            "defaultCandidatePool": self.default_candidate_pool,
            "builtin": true,
            "description": self.description,
        })
    }
}

/// Context profile validation error.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContextProfileValidationError {
    EmptyDisplayName,
    ZeroCandidatePool,
    UnnormalizedSectionMix { total_bps: u16 },
}

impl ContextProfileValidationError {
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::EmptyDisplayName => "context_profile_empty_display_name",
            Self::ZeroCandidatePool => "context_profile_zero_candidate_pool",
            Self::UnnormalizedSectionMix { .. } => "context_profile_unnormalized_section_mix",
        }
    }
}

impl fmt::Display for ContextProfileValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyDisplayName => formatter.write_str("context profile display name is empty"),
            Self::ZeroCandidatePool => {
                formatter.write_str("context profile candidate pool must be non-zero")
            }
            Self::UnnormalizedSectionMix { total_bps } => write!(
                formatter,
                "context profile section mix totals {total_bps} basis points, expected {TOTAL_BASIS_POINTS}"
            ),
        }
    }
}

impl std::error::Error for ContextProfileValidationError {}

/// Field descriptor used by the context profile schema catalog.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ContextProfileFieldSchema {
    pub name: &'static str,
    pub type_name: &'static str,
    pub required: bool,
    pub description: &'static str,
}

impl ContextProfileFieldSchema {
    #[must_use]
    pub const fn new(
        name: &'static str,
        type_name: &'static str,
        required: bool,
        description: &'static str,
    ) -> Self {
        Self {
            name,
            type_name,
            required,
            description,
        }
    }
}

/// Stable JSON-schema-like catalog entry for context profile records.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ContextProfileObjectSchema {
    pub schema_name: &'static str,
    pub schema_uri: &'static str,
    pub kind: &'static str,
    pub title: &'static str,
    pub description: &'static str,
    pub fields: &'static [ContextProfileFieldSchema],
}

impl ContextProfileObjectSchema {
    #[must_use]
    pub fn required_count(&self) -> usize {
        self.fields.iter().filter(|field| field.required).count()
    }
}

const CONTEXT_PROFILE_FIELDS: &[ContextProfileFieldSchema] = &[
    ContextProfileFieldSchema::new("schema", "string", true, "Schema identifier."),
    ContextProfileFieldSchema::new("name", "string", true, "Stable profile name."),
    ContextProfileFieldSchema::new(
        "displayName",
        "string",
        true,
        "Human-readable profile name.",
    ),
    ContextProfileFieldSchema::new(
        "objective",
        "string",
        true,
        "Selection objective used by the profile.",
    ),
    ContextProfileFieldSchema::new(
        "sectionMix",
        "object",
        true,
        "Section quota mix expressed in basis points.",
    ),
    ContextProfileFieldSchema::new(
        "defaultCandidatePool",
        "integer",
        true,
        "Default candidate pool size for profile-aware request builders.",
    ),
    ContextProfileFieldSchema::new(
        "builtin",
        "boolean",
        true,
        "Whether the profile is shipped by ee.",
    ),
    ContextProfileFieldSchema::new("description", "string", true, "Profile purpose."),
];

#[must_use]
pub const fn context_profile_schemas() -> [ContextProfileObjectSchema; 1] {
    [ContextProfileObjectSchema {
        schema_name: CONTEXT_PROFILE_SCHEMA_V1,
        schema_uri: "urn:ee:schema:context-profile:v1",
        kind: "context_profile",
        title: "ContextProfile",
        description: "Versioned context packing profile with objective and section quotas.",
        fields: CONTEXT_PROFILE_FIELDS,
    }]
}

#[must_use]
pub fn context_profile_schema_catalog_json() -> String {
    let schemas = context_profile_schemas();
    let mut output = String::from("{\n");
    output.push_str(&format!(
        "  \"schema\": \"{CONTEXT_PROFILE_SCHEMA_CATALOG_V1}\",\n"
    ));
    output.push_str("  \"schemas\": [\n");
    for (schema_index, schema) in schemas.iter().enumerate() {
        output.push_str("    {\n");
        output.push_str(&format!(
            "      \"$schema\": \"{JSON_SCHEMA_DRAFT_2020_12}\",\n"
        ));
        output.push_str("      \"$id\": ");
        push_json_string(&mut output, schema.schema_uri);
        output.push_str(",\n");
        output.push_str("      \"eeSchema\": ");
        push_json_string(&mut output, schema.schema_name);
        output.push_str(",\n");
        output.push_str("      \"kind\": ");
        push_json_string(&mut output, schema.kind);
        output.push_str(",\n");
        output.push_str("      \"title\": ");
        push_json_string(&mut output, schema.title);
        output.push_str(",\n");
        output.push_str("      \"description\": ");
        push_json_string(&mut output, schema.description);
        output.push_str(",\n");
        output.push_str("      \"type\": \"object\",\n");
        output.push_str("      \"required\": [\n");
        let mut emitted_required = 0;
        for field in schema.fields {
            if field.required {
                emitted_required += 1;
                output.push_str("        ");
                push_json_string(&mut output, field.name);
                if emitted_required == schema.required_count() {
                    output.push('\n');
                } else {
                    output.push_str(",\n");
                }
            }
        }
        output.push_str("      ],\n");
        output.push_str("      \"fields\": [\n");
        for (field_index, field) in schema.fields.iter().enumerate() {
            output.push_str("        {\"name\": ");
            push_json_string(&mut output, field.name);
            output.push_str(", \"type\": ");
            push_json_string(&mut output, field.type_name);
            output.push_str(", \"required\": ");
            output.push_str(if field.required { "true" } else { "false" });
            output.push_str(", \"description\": ");
            push_json_string(&mut output, field.description);
            if field_index + 1 == schema.fields.len() {
                output.push_str("}\n");
            } else {
                output.push_str("},\n");
            }
        }
        output.push_str("      ],\n");
        output.push_str("      \"additionalProperties\": false\n");
        if schema_index + 1 == schemas.len() {
            output.push_str("    }\n");
        } else {
            output.push_str("    },\n");
        }
    }
    output.push_str("  ]\n");
    output.push_str("}\n");
    output
}

fn push_json_string(output: &mut String, value: &str) {
    output.push('"');
    for character in value.chars() {
        match character {
            '"' => output.push_str("\\\""),
            '\\' => output.push_str("\\\\"),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            other => output.push(other),
        }
    }
    output.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONTEXT_PROFILE_SCHEMA_GOLDEN: &str =
        include_str!("../../tests/fixtures/golden/models/context_profile_schemas.json.golden");

    type TestResult = Result<(), String>;

    fn ensure<T: std::fmt::Debug + PartialEq>(actual: T, expected: T, ctx: &str) -> TestResult {
        if actual == expected {
            Ok(())
        } else {
            Err(format!("{ctx}: expected {expected:?}, got {actual:?}"))
        }
    }

    #[test]
    fn context_profile_names_parse_stable_wire_names() -> TestResult {
        ensure(
            ContextProfileName::all().map(ContextProfileName::as_str),
            ["compact", "balanced", "thorough", "submodular"],
            "profile names",
        )?;
        ensure(
            ContextProfileName::parse(" BALANCED "),
            Some(ContextProfileName::Balanced),
            "profile parse trims and folds case",
        )?;
        ensure(
            ContextProfileName::parse("wide"),
            None,
            "unsupported profile is rejected by model parser",
        )
    }

    #[test]
    fn context_profile_sections_parse_stable_wire_names() -> TestResult {
        ensure(
            ContextProfileSection::all().map(ContextProfileSection::as_str),
            [
                "procedural_rules",
                "decisions",
                "failures",
                "evidence",
                "artifacts",
            ],
            "section names",
        )?;
        ensure(
            ContextProfileSection::parse("evidence"),
            Some(ContextProfileSection::Evidence),
            "section parse",
        )
    }

    #[test]
    fn agent_profile_bias_cold_starts_until_threshold() -> TestResult {
        let bias = AgentContextProfileCounts::new(9, 0, 0).bias();
        ensure(bias.cold_start, true, "nine outcomes is still cold-start")?;
        ensure(bias.weight, 0.0, "cold-start applies no bias")?;

        let bias = AgentContextProfileCounts::new(10, 0, 0).bias();
        ensure(!bias.cold_start, true, "ten outcomes exits cold-start")?;
        ensure(
            bias.weight > 0.0,
            true,
            "helpful outcomes produce positive bias",
        )
    }

    #[test]
    fn agent_profile_bias_is_capped_for_all_extreme_counts() -> TestResult {
        for counts in [
            AgentContextProfileCounts::new(u32::MAX, 0, 0),
            AgentContextProfileCounts::new(0, u32::MAX, 0),
            AgentContextProfileCounts::new(u32::MAX, u32::MAX / 2, 0),
            AgentContextProfileCounts::new(u32::MAX / 2, u32::MAX, u32::MAX),
        ] {
            let bias = counts.bias();
            ensure(
                bias.weight.abs() <= AGENT_PROFILE_BIAS_CAP,
                true,
                "bias must never exceed cap",
            )?;
        }
        Ok(())
    }

    #[test]
    fn agent_profile_bias_uses_helpful_harmful_and_ignored_counts() -> TestResult {
        let helpful = AgentContextProfileCounts::new(20, 1, 0).bias();
        let harmful = AgentContextProfileCounts::new(1, 20, 0).bias();
        let ignored = AgentContextProfileCounts::new(20, 1, 100).bias();

        ensure(helpful.weight > 0.0, true, "helpful majority is positive")?;
        ensure(harmful.weight < 0.0, true, "harmful majority is negative")?;
        ensure(
            ignored.weight.abs() < helpful.weight.abs(),
            true,
            "ignored outcomes damp useful bias",
        )
    }

    #[test]
    fn agent_profile_decay_halves_counts_at_half_life() -> TestResult {
        let decayed = AgentContextProfileCounts::new(40, 20, 10).decayed(365.0, 365.0);
        ensure(
            (decayed.helpful_count - 20.0).abs() < f64::EPSILON,
            true,
            "helpful count halves",
        )?;
        ensure(
            (decayed.harmful_count - 10.0).abs() < f64::EPSILON,
            true,
            "harmful count halves",
        )?;
        ensure(
            (decayed.ignored_count - 5.0).abs() < f64::EPSILON,
            true,
            "ignored count halves",
        )
    }

    #[test]
    fn agent_profile_decay_handles_invalid_boundaries() -> TestResult {
        ensure(
            decay_factor(-1.0, 365.0),
            1.0,
            "negative age does not amplify counts",
        )?;
        ensure(decay_factor(1.0, 0.0), 0.0, "zero half-life expires counts")
    }

    #[test]
    fn builtin_context_profiles_are_normalized_and_ordered() -> TestResult {
        let profiles = ContextProfile::builtins();
        ensure(profiles.len(), 4, "builtin count")?;
        ensure(
            profiles.map(|profile| profile.name),
            ContextProfileName::all(),
            "builtin order",
        )?;
        for profile in profiles {
            profile
                .validate()
                .map_err(|error| format!("{} failed validation: {error}", profile.name))?;
            ensure(profile.section_mix.total_bps(), 10_000, "section total")?;
        }
        Ok(())
    }

    #[test]
    fn builtin_profiles_capture_existing_quota_mixes() -> TestResult {
        let compact = ContextProfile::builtin(ContextProfileName::Compact);
        ensure(
            compact
                .section_mix
                .weight_bps(ContextProfileSection::ProceduralRules),
            5_000,
            "compact procedural quota",
        )?;
        let thorough = ContextProfile::builtin(ContextProfileName::Thorough);
        ensure(
            thorough
                .section_mix
                .weight_bps(ContextProfileSection::Evidence),
            2_500,
            "thorough evidence quota",
        )?;
        let submodular = ContextProfile::builtin(ContextProfileName::Submodular);
        ensure(
            submodular.objective,
            ContextProfileObjective::FacilityLocation,
            "submodular objective",
        )
    }

    #[test]
    fn context_profile_validation_rejects_invalid_models() -> TestResult {
        let profile = ContextProfile {
            display_name: "",
            ..ContextProfile::builtin(ContextProfileName::Balanced)
        };
        ensure(
            profile.validate(),
            Err(ContextProfileValidationError::EmptyDisplayName),
            "empty display name",
        )?;

        let profile = ContextProfile {
            default_candidate_pool: 0,
            ..ContextProfile::builtin(ContextProfileName::Balanced)
        };
        ensure(
            profile.validate(),
            Err(ContextProfileValidationError::ZeroCandidatePool),
            "zero candidate pool",
        )?;

        let profile = ContextProfile {
            section_mix: ContextProfileSectionMix::new(1, 2, 3, 4, 5),
            ..ContextProfile::builtin(ContextProfileName::Balanced)
        };
        ensure(
            profile.validate(),
            Err(ContextProfileValidationError::UnnormalizedSectionMix { total_bps: 15 }),
            "unnormalized section mix",
        )
    }

    #[test]
    fn context_profile_json_shape_is_stable() -> TestResult {
        let json = ContextProfile::builtin(ContextProfileName::Submodular).data_json();
        ensure(
            json.get("schema").and_then(JsonValue::as_str),
            Some(CONTEXT_PROFILE_SCHEMA_V1),
            "profile schema",
        )?;
        ensure(
            json.get("name").and_then(JsonValue::as_str),
            Some("submodular"),
            "profile name",
        )?;
        ensure(
            json.get("objective").and_then(JsonValue::as_str),
            Some("facility_location"),
            "profile objective",
        )?;
        ensure(
            json.pointer("/sectionMix/totalBps")
                .and_then(JsonValue::as_u64),
            Some(10_000),
            "section mix total",
        )
    }

    #[test]
    fn context_profile_schema_catalog_order_is_stable() -> TestResult {
        let schemas = context_profile_schemas();
        ensure(schemas.len(), 1, "schema count")?;
        ensure(
            schemas.first().map(|schema| schema.schema_name),
            Some(CONTEXT_PROFILE_SCHEMA_V1),
            "context profile schema",
        )
    }

    #[test]
    fn context_profile_schema_catalog_matches_golden_fixture() {
        assert_eq!(
            context_profile_schema_catalog_json(),
            CONTEXT_PROFILE_SCHEMA_GOLDEN
        );
    }

    #[test]
    fn context_profile_schema_catalog_is_valid_json() -> TestResult {
        let parsed: JsonValue =
            serde_json::from_str(CONTEXT_PROFILE_SCHEMA_GOLDEN).map_err(|error| {
                format!("context profile schema golden must be valid JSON: {error}")
            })?;
        ensure(
            parsed.get("schema").and_then(JsonValue::as_str),
            Some(CONTEXT_PROFILE_SCHEMA_CATALOG_V1),
            "catalog schema",
        )?;
        let schemas = parsed
            .get("schemas")
            .and_then(JsonValue::as_array)
            .ok_or_else(|| "schemas must be an array".to_string())?;
        ensure(schemas.len(), 1, "catalog length")
    }
}
