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

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::{ParseTrustClassError, TrustClass};

    #[test]
    fn trust_class_round_trip_for_every_variant() {
        for class in TrustClass::all() {
            let rendered = class.to_string();
            let parsed = match TrustClass::from_str(&rendered) {
                Ok(value) => value,
                Err(error) => panic!("trust class {class:?} failed to round-trip: {error}"),
            };
            assert_eq!(parsed, class);
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
        let err = match TrustClass::from_str("unknown_class") {
            Ok(value) => panic!("expected error, got Ok({value:?})"),
            Err(error) => error,
        };
        assert_eq!(
            err,
            ParseTrustClassError {
                input: "unknown_class".to_owned(),
            }
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
}
