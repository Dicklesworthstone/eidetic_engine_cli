//! Trust class taxonomy (EE-260, ADR-0009, ADR-0086).
//!
//! Defines the six-class trust taxonomy for memories:
//! - `human_explicit`: Human invoked `ee remember` directly (0.85)
//! - `peer_human_attested`: Active member's signed origin declared `human_explicit` (0.75)
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

fn normalized_trust_token(input: &str) -> String {
    let trimmed = input.trim();
    let mut normalized = String::with_capacity(trimmed.len());
    let mut previous_was_lowercase = false;
    let mut previous_was_separator = false;

    for character in trimmed.chars() {
        match character {
            '-' | '_' => {
                if !normalized.is_empty() && !previous_was_separator {
                    normalized.push('_');
                }
                previous_was_lowercase = false;
                previous_was_separator = true;
            }
            character if character.is_ascii_uppercase() => {
                if previous_was_lowercase && !previous_was_separator {
                    normalized.push('_');
                }
                normalized.push(character.to_ascii_lowercase());
                previous_was_lowercase = false;
                previous_was_separator = false;
            }
            character => {
                normalized.push(character.to_ascii_lowercase());
                previous_was_lowercase = character.is_ascii_lowercase();
                previous_was_separator = false;
            }
        }
    }

    normalized
}

/// Stable schema marker for local signing-key policy decisions.
pub const LOCAL_SIGNING_KEY_POLICY_SCHEMA_V1: &str = "ee.local_signing_key_policy.v1";

/// Trust class for a memory, determining initial confidence and
/// scoring weight.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum TrustClass {
    /// Human invoked `ee remember` directly.
    HumanExplicit,
    /// Signed peer origin from an active member declared `human_explicit`.
    PeerHumanAttested,
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
            Self::PeerHumanAttested => "peer_human_attested",
            Self::AgentValidated => "agent_validated",
            Self::AgentAssertion => "agent_assertion",
            Self::CassEvidence => "cass_evidence",
            Self::LegacyImport => "legacy_import",
        }
    }

    /// Initial confidence for this trust class per ADR-0009 and ADR-0086 TC-D7.
    #[must_use]
    pub const fn initial_confidence(self) -> f32 {
        match self {
            Self::HumanExplicit => 0.85,
            Self::PeerHumanAttested => 0.75,
            Self::AgentValidated => 0.65,
            Self::AgentAssertion => 0.50,
            Self::CassEvidence => 0.45,
            Self::LegacyImport => 0.30,
        }
    }

    /// All variants in a stable order.
    #[must_use]
    pub const fn all() -> [Self; 6] {
        [
            Self::HumanExplicit,
            Self::PeerHumanAttested,
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
        matches!(
            self,
            Self::HumanExplicit | Self::PeerHumanAttested | Self::AgentValidated
        )
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
            "unknown trust class `{}`; expected one of human_explicit, peer_human_attested, agent_validated, agent_assertion, cass_evidence, legacy_import",
            self.input
        )
    }
}

impl std::error::Error for ParseTrustClassError {}

impl FromStr for TrustClass {
    type Err = ParseTrustClassError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        match normalized_trust_token(input).as_str() {
            "human_explicit" => Ok(Self::HumanExplicit),
            "peer_human_attested" => Ok(Self::PeerHumanAttested),
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

/// Multiplicity evidence for a memory that was one recorded attempt out of a
/// declared family of sibling attempts (bd-multiplicity-aware-trust-p0u7g).
///
/// The canonical discount and completeness math lives here so the trust
/// report, pack ranking, and the promotion gate cannot drift apart. A family
/// is *complete* when at least the declared number of attempts is recorded;
/// survivor-only evidence (one recorded member drawn from N > 1 declared
/// attempts) receives the strongest discount.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttemptFamilyMultiplicity {
    /// Stable pre-registered family identity.
    pub family_id: String,
    /// Largest declared sibling count among recorded members, when any
    /// member declared one.
    pub declared_size: Option<u32>,
    /// Distinct in-range attempt slots covered by live members. Completion
    /// is measured in slots, never raw rows: re-recording the winner N times
    /// occupies one slot and cannot launder an incomplete family.
    pub recorded_slots: u32,
    /// Live slotted members recorded as `selected` (winners).
    pub selected_count: u32,
    /// Live slotted members recorded as `rejected` (negative siblings).
    pub rejected_count: u32,
    /// Live members without a slot; they are visible evidence but never
    /// count toward completion.
    pub unslotted_count: u32,
}

impl AttemptFamilyMultiplicity {
    /// Aggregate member rows (slot, disposition) into the canonical
    /// multiplicity posture. Slots are deduplicated, and when a declared
    /// size exists only slots in `1..=declared` count toward completion.
    #[must_use]
    pub fn from_members<'a>(
        family_id: String,
        declared_size: Option<u32>,
        members: impl IntoIterator<Item = (Option<u32>, Option<&'a str>)>,
    ) -> Self {
        let mut seen_slots = std::collections::BTreeSet::new();
        let mut selected_count = 0_u32;
        let mut rejected_count = 0_u32;
        let mut unslotted_count = 0_u32;
        for (slot, disposition) in members {
            match slot {
                Some(slot) if declared_size.is_none_or(|declared| slot <= declared) => {
                    if seen_slots.insert(slot) {
                        match disposition {
                            Some("selected") => selected_count += 1,
                            Some("rejected") => rejected_count += 1,
                            _ => {}
                        }
                    }
                }
                Some(_) | None => unslotted_count += 1,
            }
        }
        let recorded_slots = u32::try_from(seen_slots.len()).unwrap_or(u32::MAX);
        Self {
            family_id,
            declared_size,
            recorded_slots,
            selected_count,
            rejected_count,
            unslotted_count,
        }
    }

    /// Declared sibling slots that no live member occupies.
    #[must_use]
    pub fn unrecorded_count(&self) -> u32 {
        self.declared_size
            .map_or(0, |declared| declared.saturating_sub(self.recorded_slots))
    }

    /// True when every declared sibling slot is occupied by a distinct live
    /// member (or no sibling count was ever declared, which leaves nothing
    /// outstanding to require).
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.unrecorded_count() == 0
    }

    /// True for the selection-bias shape the discount exists for: a declared
    /// family of more than one whose recorded slot coverage is a lone
    /// surviving winner.
    #[must_use]
    pub fn is_survivor_only(&self) -> bool {
        self.declared_size.is_some_and(|declared| declared > 1)
            && self.recorded_slots <= 1
            && self.selected_count >= 1
    }

    /// Deterministic multiplicity discount in (0.0, 1.0]: the fraction of
    /// declared attempt slots that are actually recorded. A complete family
    /// (or one that never declared a size) is undiscounted at 1.0; "1 of 18"
    /// yields 1/18. Unslotted members never raise the factor. The factor is
    /// a pure ratio so identical inputs always produce byte-identical
    /// scores.
    #[must_use]
    pub fn discount_factor(&self) -> f32 {
        let Some(declared) = self.declared_size else {
            return 1.0;
        };
        if declared <= 1 || self.recorded_slots >= declared {
            return 1.0;
        }
        #[allow(clippy::cast_possible_truncation)]
        let factor = (f64::from(self.recorded_slots) / f64::from(declared)) as f32;
        factor.clamp(f32::MIN_POSITIVE, 1.0)
    }

    /// True when the family may support a trust promotion: every declared
    /// slot is recorded AND the recorded composition is the canonical
    /// fan-out shape — exactly one `selected` winner with the remaining
    /// slots `rejected`. Mere slot coverage is deliberately insufficient:
    /// N rows all recorded as winners fill every slot while recording zero
    /// negative evidence, which is exactly the all-winners laundering shape.
    /// Families with several genuine winners are not yet modeled and stay
    /// ineligible (an explicit, documented limitation rather than a silent
    /// pass); undeclared or single-attempt families reduce to completeness.
    #[must_use]
    pub fn is_promotion_eligible(&self) -> bool {
        if !self.is_complete() {
            return false;
        }
        match self.declared_size {
            Some(declared) if declared > 1 => {
                self.selected_count == 1 && self.rejected_count == declared - 1
            }
            _ => true,
        }
    }

    /// Per-member ranking discount. The multiplicity discount exists to
    /// deflate survivor-selection bias, so it applies to `selected` winners
    /// only; `rejected` siblings are the negative/safety evidence the family
    /// exists to preserve and are never discounted (deflating them would
    /// hide the very failures the denominator is meant to surface).
    #[must_use]
    pub fn member_discount_factor(&self, disposition: Option<&str>) -> f32 {
        match disposition {
            Some("selected") => self.discount_factor(),
            _ => 1.0,
        }
    }

    /// Human-facing summary of the recorded/declared posture, e.g.
    /// `"1 of 18 attempt slots recorded; 17 unrecorded"`.
    #[must_use]
    pub fn summary(&self) -> String {
        let mut summary = match self.declared_size {
            Some(declared) => format!(
                "{} of {declared} attempt slots recorded; {} unrecorded",
                self.recorded_slots,
                self.unrecorded_count()
            ),
            None => format!(
                "{} attempt slots recorded; no declared sibling count",
                self.recorded_slots
            ),
        };
        if self.unslotted_count > 0 {
            summary.push_str(&format!(
                " ({} unslotted member(s) excluded from completion)",
                self.unslotted_count
            ));
        }
        summary
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use crate::models::memory::MemoryLevel;
    use crate::models::rule::RuleMaturity;

    use super::{
        AttemptFamilyMultiplicity, LocalSigningKeyPosture, ParseTrustClassError, TrustClass,
        evaluate_local_signing_key_policy,
    };

    #[test]
    fn multiplicity_completion_counts_distinct_slots_not_raw_rows() {
        let survivor_only = AttemptFamilyMultiplicity::from_members(
            "fam-a".to_owned(),
            Some(18),
            [(Some(1), Some("selected"))],
        );
        assert!(survivor_only.is_survivor_only());
        assert!(!survivor_only.is_complete());
        assert_eq!(survivor_only.recorded_slots, 1);
        assert_eq!(survivor_only.unrecorded_count(), 17);
        assert!((survivor_only.discount_factor() - 1.0 / 18.0).abs() < 1.0e-7);
        assert_eq!(
            survivor_only.summary(),
            "1 of 18 attempt slots recorded; 17 unrecorded"
        );

        // Anti-laundering: recording the same winning slot three times covers
        // one slot, and members without slots never advance completion.
        let laundered = AttemptFamilyMultiplicity::from_members(
            "fam-b".to_owned(),
            Some(3),
            [
                (Some(1), Some("selected")),
                (Some(1), Some("selected")),
                (Some(1), Some("selected")),
                (None, Some("rejected")),
            ],
        );
        assert!(!laundered.is_complete());
        assert_eq!(laundered.recorded_slots, 1);
        assert_eq!(laundered.unslotted_count, 1);
        assert_eq!(laundered.unrecorded_count(), 2);
        assert!((laundered.discount_factor() - 1.0 / 3.0).abs() < 1.0e-7);

        let complete = AttemptFamilyMultiplicity::from_members(
            "fam-c".to_owned(),
            Some(3),
            [
                (Some(1), Some("selected")),
                (Some(2), Some("rejected")),
                (Some(3), Some("rejected")),
            ],
        );
        assert!(complete.is_complete());
        assert!(!complete.is_survivor_only());
        assert_eq!(complete.selected_count, 1);
        assert_eq!(complete.rejected_count, 2);
        assert_eq!(complete.unrecorded_count(), 0);
        assert!((complete.discount_factor() - 1.0).abs() < f32::EPSILON);

        // Slots above the declared size are visible but never complete a
        // family they do not belong to.
        let out_of_range = AttemptFamilyMultiplicity::from_members(
            "fam-d".to_owned(),
            Some(2),
            [(Some(1), Some("selected")), (Some(9), Some("rejected"))],
        );
        assert!(!out_of_range.is_complete());
        assert_eq!(out_of_range.recorded_slots, 1);
        assert_eq!(out_of_range.unslotted_count, 1);

        let undeclared = AttemptFamilyMultiplicity::from_members(
            "fam-e".to_owned(),
            None,
            [(Some(1), Some("selected")), (Some(2), Some("rejected"))],
        );
        assert!(undeclared.is_complete());
        assert!((undeclared.discount_factor() - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn promotion_eligibility_requires_canonical_composition_not_just_slots() {
        // All-winners laundering: every slot filled, zero negative evidence.
        // Complete for discount purposes, but never promotion-eligible.
        let all_selected = AttemptFamilyMultiplicity::from_members(
            "fam-f".to_owned(),
            Some(3),
            [
                (Some(1), Some("selected")),
                (Some(2), Some("selected")),
                (Some(3), Some("selected")),
            ],
        );
        assert!(all_selected.is_complete());
        assert!(!all_selected.is_promotion_eligible());

        let canonical = AttemptFamilyMultiplicity::from_members(
            "fam-g".to_owned(),
            Some(3),
            [
                (Some(1), Some("selected")),
                (Some(2), Some("rejected")),
                (Some(3), Some("rejected")),
            ],
        );
        assert!(canonical.is_promotion_eligible());

        let incomplete = AttemptFamilyMultiplicity::from_members(
            "fam-h".to_owned(),
            Some(3),
            [(Some(1), Some("selected")), (Some(2), Some("rejected"))],
        );
        assert!(!incomplete.is_promotion_eligible());
    }

    #[test]
    fn member_discount_applies_to_selected_survivors_never_rejected_evidence() {
        let survivor_only = AttemptFamilyMultiplicity::from_members(
            "fam-i".to_owned(),
            Some(18),
            [(Some(1), Some("selected")), (Some(2), Some("rejected"))],
        );
        let selected_factor = survivor_only.member_discount_factor(Some("selected"));
        assert!((selected_factor - 2.0 / 18.0).abs() < 1.0e-7);
        let rejected_factor = survivor_only.member_discount_factor(Some("rejected"));
        assert!((rejected_factor - 1.0).abs() < f32::EPSILON);
        let unslotted_factor = survivor_only.member_discount_factor(None);
        assert!((unslotted_factor - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn trust_class_round_trip_for_every_variant() {
        for class in TrustClass::all() {
            let rendered = class.to_string();
            let parsed = TrustClass::from_str(&rendered);
            assert_eq!(parsed, Ok(class));
        }
        assert_eq!(
            TrustClass::from_str(" Agent-Validated "),
            Ok(TrustClass::AgentValidated)
        );
        assert_eq!(
            TrustClass::from_str("humanExplicit"),
            Ok(TrustClass::HumanExplicit)
        );
        assert_eq!(
            TrustClass::from_str("PeerHumanAttested"),
            Ok(TrustClass::PeerHumanAttested)
        );
        assert_eq!(
            TrustClass::from_str("CassEvidence"),
            Ok(TrustClass::CassEvidence)
        );
        assert_eq!(
            TrustClass::from_str("legacyImport"),
            Ok(TrustClass::LegacyImport)
        );
    }

    #[test]
    fn trust_class_initial_confidences_match_adr() {
        assert!((TrustClass::HumanExplicit.initial_confidence() - 0.85).abs() < 0.001);
        assert!((TrustClass::PeerHumanAttested.initial_confidence() - 0.75).abs() < 0.001);
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
        assert_eq!(
            TrustClass::all().map(TrustClass::as_str),
            [
                "human_explicit",
                "peer_human_attested",
                "agent_validated",
                "agent_assertion",
                "cass_evidence",
                "legacy_import",
            ]
        );
        assert_eq!(TrustClass::HumanExplicit.as_str(), "human_explicit");
        assert_eq!(
            TrustClass::PeerHumanAttested.as_str(),
            "peer_human_attested"
        );
        assert_eq!(TrustClass::AgentValidated.as_str(), "agent_validated");
        assert_eq!(TrustClass::AgentAssertion.as_str(), "agent_assertion");
        assert_eq!(TrustClass::CassEvidence.as_str(), "cass_evidence");
        assert_eq!(TrustClass::LegacyImport.as_str(), "legacy_import");
    }

    #[test]
    fn local_signing_policy_requires_validated_high_trust_procedural_signatures() {
        for trust_class in [
            TrustClass::HumanExplicit,
            TrustClass::PeerHumanAttested,
            TrustClass::AgentValidated,
        ] {
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
