//! Trust class taxonomy (EE-260, ADR-0009).
//!
//! Defines the five-class trust taxonomy for memories:
//! - `human_explicit`: Human invoked `ee remember` directly (0.85)
//! - `agent_validated`: Agent assertion + validated outcome (0.65)
//! - `agent_assertion`: Agent assertion, no outcome yet (0.50)
//! - `cass_evidence`: Imported session span from `cass` (0.45)
//! - `legacy_import`: Imported from pre-v1 Eidetic Engine (0.30)
//!
//! Trust class is exposed as `trust_class` on every memory in
//! `ee.memory.v1`. An optional `trust_subclass` qualifier provides
//! project-tunable metadata without affecting scoring.

use std::fmt;
use std::str::FromStr;

use crate::models::memory::MemoryLevel;
use crate::models::rule::RuleMaturity;

/// Stable schema marker for local signing-key policy decisions.
pub const LOCAL_SIGNING_KEY_POLICY_SCHEMA_V1: &str = "ee.local_signing_key_policy.v1";

/// Trust class for a memory, determining initial confidence and
/// scoring weight.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum TrustClass {
    /// Human invoked `ee remember` directly.
    HumanExplicit,
    /// Agent assertion with at least one validated outcome.
    AgentValidated,
    /// Agent assertion, no outcome events yet.
    AgentAssertion,
    /// Imported session span from `cass`.
    CassEvidence,
    /// Imported from a pre-v1 Eidetic Engine artifact.
    LegacyImport,
}

impl TrustClass {
    /// Stable lowercase wire form.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::HumanExplicit => "human_explicit",
            Self::AgentValidated => "agent_validated",
            Self::AgentAssertion => "agent_assertion",
            Self::CassEvidence => "cass_evidence",
            Self::LegacyImport => "legacy_import",
        }
    }

    /// Initial confidence for this trust class per ADR-0009.
    #[must_use]
    pub const fn initial_confidence(self) -> f32 {
        match self {
            Self::HumanExplicit => 0.85,
            Self::AgentValidated => 0.65,
            Self::AgentAssertion => 0.50,
            Self::CassEvidence => 0.45,
            Self::LegacyImport => 0.30,
        }
    }

    /// All variants in a stable order.
    #[must_use]
    pub const fn all() -> [Self; 5] {
        [
            Self::HumanExplicit,
            Self::AgentValidated,
            Self::AgentAssertion,
            Self::CassEvidence,
            Self::LegacyImport,
        ]
    }

    /// Whether validated procedural memories in this class need a
    /// local signature before authoritative use.
    #[must_use]
    pub const fn requires_local_signature_for_validated_procedural(self) -> bool {
        matches!(self, Self::HumanExplicit | Self::AgentValidated)
    }
}

impl fmt::Display for TrustClass {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Error when parsing an invalid trust class string.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseTrustClassError {
    input: String,
}

impl ParseTrustClassError {
    /// The invalid input that was attempted.
    pub fn input(&self) -> &str {
        &self.input
    }
}

impl fmt::Display for ParseTrustClassError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "unknown trust class `{}`; expected one of human_explicit, agent_validated, agent_assertion, cass_evidence, legacy_import",
            self.input
        )
    }
}

impl std::error::Error for ParseTrustClassError {}

impl FromStr for TrustClass {
    type Err = ParseTrustClassError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        match input {
            "human_explicit" => Ok(Self::HumanExplicit),
            "agent_validated" => Ok(Self::AgentValidated),
            "agent_assertion" => Ok(Self::AgentAssertion),
            "cass_evidence" => Ok(Self::CassEvidence),
            "legacy_import" => Ok(Self::LegacyImport),
            _ => Err(ParseTrustClassError {
                input: input.to_owned(),
            }),
        }
    }
}

/// Local signing-key posture for a procedural memory.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum LocalSigningKeyPosture {
    /// The memory is outside the high-trust procedural policy boundary.
    NotRequired,
    /// A signature should be attached before promotion to validated authority.
    Recommended,
    /// A signature is required before authoritative procedural use.
    Required,
    /// The policy applies and the local signature is present.
    Satisfied,
}

impl LocalSigningKeyPosture {
    /// Stable lowercase wire form.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotRequired => "not_required",
            Self::Recommended => "recommended",
            Self::Required => "required",
            Self::Satisfied => "satisfied",
        }
    }

    /// All variants in stable wire order.
    #[must_use]
    pub const fn all() -> [Self; 4] {
        [
            Self::NotRequired,
            Self::Recommended,
            Self::Required,
            Self::Satisfied,
        ]
    }
}

impl fmt::Display for LocalSigningKeyPosture {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Deterministic local signing-key policy result for one memory posture.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct LocalSigningKeyDecision {
    /// Stable schema marker for machine consumers.
    pub schema: &'static str,
    /// Policy posture.
    pub posture: LocalSigningKeyPosture,
    /// Stable machine-readable reason code.
    pub code: &'static str,
    /// Human-facing summary that can be rendered on stderr or in reports.
    pub message: &'static str,
    /// Suggested next action when the posture is not already satisfied.
    pub repair: Option<&'static str>,
}

impl LocalSigningKeyDecision {
    const fn new(
        posture: LocalSigningKeyPosture,
        code: &'static str,
        message: &'static str,
        repair: Option<&'static str>,
    ) -> Self {
        Self {
            schema: LOCAL_SIGNING_KEY_POLICY_SCHEMA_V1,
            posture,
            code,
            message,
            repair,
        }
    }

    /// Returns true when authoritative use must be blocked until signed.
    #[must_use]
    pub const fn is_blocking(self) -> bool {
        matches!(self.posture, LocalSigningKeyPosture::Required)
    }
}

/// Evaluate the local signing-key policy for a memory.
///
/// Only validated high-trust procedural memories are blocking when unsigned.
/// Draft or candidate high-trust procedural memories get a recommendation so
/// curation can attach a local signature before promotion. Lower-trust,
/// non-procedural, and terminal memories are not required to carry one.
#[must_use]
pub const fn evaluate_local_signing_key_policy(
    level: MemoryLevel,
    trust_class: TrustClass,
    maturity: RuleMaturity,
    has_local_signature: bool,
) -> LocalSigningKeyDecision {
    if !matches!(level, MemoryLevel::Procedural)
        || maturity.is_terminal()
        || !trust_class.requires_local_signature_for_validated_procedural()
    {
        LocalSigningKeyDecision::new(
            LocalSigningKeyPosture::NotRequired,
            "local_signing_key_not_required",
            "Local signing key is not required for this memory posture.",
            None,
        )
    } else if has_local_signature {
        LocalSigningKeyDecision::new(
            LocalSigningKeyPosture::Satisfied,
            "local_signing_key_satisfied",
            "High-trust procedural memory has a local signature.",
            None,
        )
    } else if matches!(maturity, RuleMaturity::Validated) {
        LocalSigningKeyDecision::new(
            LocalSigningKeyPosture::Required,
            "local_signing_key_required",
            "Validated high-trust procedural memories require a local signature before authoritative use.",
            Some(
                "Keep the memory out of authoritative procedural sections until a local signature is attached.",
            ),
        )
    } else {
        LocalSigningKeyDecision::new(
            LocalSigningKeyPosture::Recommended,
            "local_signing_key_recommended",
            "Attach a local signature before promoting this high-trust procedural memory to validated authority.",
            Some("Keep the memory advisory until a local signature is attached."),
        )
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use crate::models::memory::MemoryLevel;
    use crate::models::rule::RuleMaturity;

    use super::{
        LocalSigningKeyPosture, ParseTrustClassError, TrustClass, evaluate_local_signing_key_policy,
    };

    #[test]
    fn trust_class_round_trip_for_every_variant() {
        for class in TrustClass::all() {
            let rendered = class.to_string();
            let parsed = TrustClass::from_str(&rendered);
            assert_eq!(parsed, Ok(class));
        }
    }

    #[test]
    fn trust_class_initial_confidences_match_adr() {
        assert!((TrustClass::HumanExplicit.initial_confidence() - 0.85).abs() < 0.001);
        assert!((TrustClass::AgentValidated.initial_confidence() - 0.65).abs() < 0.001);
        assert!((TrustClass::AgentAssertion.initial_confidence() - 0.50).abs() < 0.001);
        assert!((TrustClass::CassEvidence.initial_confidence() - 0.45).abs() < 0.001);
        assert!((TrustClass::LegacyImport.initial_confidence() - 0.30).abs() < 0.001);
    }

    #[test]
    fn trust_class_rejects_unknown_input() {
        assert_eq!(
            TrustClass::from_str("unknown_class"),
            Err(ParseTrustClassError {
                input: "unknown_class".to_owned(),
            })
        );
    }

    #[test]
    fn trust_class_as_str_is_stable() {
        assert_eq!(TrustClass::HumanExplicit.as_str(), "human_explicit");
        assert_eq!(TrustClass::AgentValidated.as_str(), "agent_validated");
        assert_eq!(TrustClass::AgentAssertion.as_str(), "agent_assertion");
        assert_eq!(TrustClass::CassEvidence.as_str(), "cass_evidence");
        assert_eq!(TrustClass::LegacyImport.as_str(), "legacy_import");
    }

    #[test]
    fn local_signing_policy_requires_validated_high_trust_procedural_signatures() {
        for trust_class in [TrustClass::HumanExplicit, TrustClass::AgentValidated] {
            let decision = evaluate_local_signing_key_policy(
                MemoryLevel::Procedural,
                trust_class,
                RuleMaturity::Validated,
                false,
            );
            assert_eq!(decision.posture, LocalSigningKeyPosture::Required);
            assert_eq!(decision.code, "local_signing_key_required");
            assert!(decision.is_blocking());
            assert!(decision.repair.is_some());
        }
    }

    #[test]
    fn local_signing_policy_is_satisfied_by_present_signature() {
        let decision = evaluate_local_signing_key_policy(
            MemoryLevel::Procedural,
            TrustClass::HumanExplicit,
            RuleMaturity::Validated,
            true,
        );

        assert_eq!(decision.posture, LocalSigningKeyPosture::Satisfied);
        assert_eq!(decision.code, "local_signing_key_satisfied");
        assert!(!decision.is_blocking());
    }

    #[test]
    fn local_signing_policy_recommends_signature_before_promotion() {
        let decision = evaluate_local_signing_key_policy(
            MemoryLevel::Procedural,
            TrustClass::AgentValidated,
            RuleMaturity::Candidate,
            false,
        );

        assert_eq!(decision.posture, LocalSigningKeyPosture::Recommended);
        assert_eq!(decision.code, "local_signing_key_recommended");
        assert!(!decision.is_blocking());
    }

    #[test]
    fn local_signing_policy_ignores_non_authoritative_postures() {
        for (level, trust_class, maturity) in [
            (
                MemoryLevel::Semantic,
                TrustClass::HumanExplicit,
                RuleMaturity::Validated,
            ),
            (
                MemoryLevel::Procedural,
                TrustClass::AgentAssertion,
                RuleMaturity::Validated,
            ),
            (
                MemoryLevel::Procedural,
                TrustClass::CassEvidence,
                RuleMaturity::Validated,
            ),
            (
                MemoryLevel::Procedural,
                TrustClass::LegacyImport,
                RuleMaturity::Validated,
            ),
            (
                MemoryLevel::Procedural,
                TrustClass::HumanExplicit,
                RuleMaturity::Deprecated,
            ),
            (
                MemoryLevel::Procedural,
                TrustClass::AgentValidated,
                RuleMaturity::Superseded,
            ),
        ] {
            let decision = evaluate_local_signing_key_policy(level, trust_class, maturity, false);
            assert_eq!(decision.posture, LocalSigningKeyPosture::NotRequired);
            assert_eq!(decision.code, "local_signing_key_not_required");
            assert!(!decision.is_blocking());
        }
    }

    #[test]
    fn local_signing_key_posture_wire_order_is_stable() {
        let rendered: Vec<&str> = LocalSigningKeyPosture::all()
            .iter()
            .map(|posture| posture.as_str())
            .collect();

        assert_eq!(
            rendered.as_slice(),
            &["not_required", "recommended", "required", "satisfied"],
        );
    }
}
