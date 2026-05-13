//! Memory freshness decay policy.
//!
//! The policy is intentionally pure: callers provide timestamps and thresholds,
//! and the evaluator returns a deterministic lifecycle decision without touching
//! storage. Maintenance jobs decide whether to persist the decision.

use chrono::{DateTime, Utc};

use crate::db::StoredMemory;

/// Machine-readable source label for memory freshness scores.
pub const MEMORY_DECAY_SOURCE: &str = "decay_v1";

/// Default demotion threshold for freshness * confidence * utility.
pub const DEFAULT_DECAY_DEMOTE_THRESHOLD: f32 = 0.05;

/// Default tombstone threshold for freshness * confidence * utility.
pub const DEFAULT_DECAY_FORGET_THRESHOLD: f32 = 0.01;

/// Lifecycle action selected by the decay policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MemoryDecayAction {
    /// Memory remains unchanged.
    Preserve,
    /// Memory remains active but is moved to a lower-trust lifecycle state.
    Demote,
    /// Memory should be tombstoned, not deleted.
    Tombstone,
}

impl MemoryDecayAction {
    /// Stable string for JSON and audit details.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Preserve => "preserve",
            Self::Demote => "demote",
            Self::Tombstone => "tombstone",
        }
    }
}

/// Thresholds used by the decay classifier.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MemoryDecayThresholds {
    pub demote: f32,
    pub forget: f32,
}

impl Default for MemoryDecayThresholds {
    fn default() -> Self {
        Self {
            demote: DEFAULT_DECAY_DEMOTE_THRESHOLD,
            forget: DEFAULT_DECAY_FORGET_THRESHOLD,
        }
    }
}

/// Half-life table used by the memory lifecycle decay policy.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MemoryDecayHalfLives {
    pub working: f32,
    pub episodic_event: f32,
    pub episodic_failure: f32,
    pub semantic_fact: f32,
    pub procedural_rule: f32,
    pub default: f32,
}

impl Default for MemoryDecayHalfLives {
    fn default() -> Self {
        Self {
            working: 1.0,
            episodic_event: 30.0,
            episodic_failure: 90.0,
            semantic_fact: 180.0,
            procedural_rule: 365.0,
            default: 30.0,
        }
    }
}

impl MemoryDecayHalfLives {
    /// Return the half-life in days for a memory level/kind pair.
    #[must_use]
    pub fn for_memory(self, level: &str, kind: &str) -> f32 {
        match (level, kind) {
            ("working", _) => self.working,
            ("episodic", "failure") => self.episodic_failure,
            ("episodic", "event") => self.episodic_event,
            ("episodic", _) => self.episodic_event,
            ("semantic", "fact") => self.semantic_fact,
            ("semantic", _) => self.semantic_fact,
            ("procedural", "rule") => self.procedural_rule,
            ("procedural", _) => self.procedural_rule,
            _ => self.default,
        }
    }

    /// Whether all configured half-lives are finite positive values.
    #[must_use]
    pub fn is_valid(self) -> bool {
        [
            self.working,
            self.episodic_event,
            self.episodic_failure,
            self.semantic_fact,
            self.procedural_rule,
            self.default,
        ]
        .into_iter()
        .all(|value| value.is_finite() && value > 0.0)
    }
}

/// Complete memory decay settings after config/default resolution.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MemoryDecaySettings {
    pub thresholds: MemoryDecayThresholds,
    pub half_lives: MemoryDecayHalfLives,
}

/// Pure decay evaluation for one memory.
#[derive(Clone, Debug, PartialEq)]
pub struct MemoryDecayEvaluation {
    pub action: MemoryDecayAction,
    pub freshness: f32,
    pub lifecycle_score: f32,
    pub half_life_days: f32,
    pub age_days: u32,
    pub previous_level: String,
    pub new_level: String,
    pub previous_importance: f32,
    pub new_importance: f32,
    pub demote_threshold: f32,
    pub forget_threshold: f32,
}

/// Default half-life in days for a memory's level/kind pair.
#[must_use]
pub fn memory_decay_half_life_days(level: &str, kind: &str) -> f64 {
    f64::from(MemoryDecayHalfLives::default().for_memory(level, kind))
}

/// Deterministic half-life freshness score.
#[must_use]
pub fn memory_decay_freshness_score(age_days: f64, half_life_days: f64) -> f32 {
    if !age_days.is_finite() || !half_life_days.is_finite() || half_life_days <= 0.0 {
        return 0.0;
    }
    let freshness = (-std::f64::consts::LN_2 * age_days.max(0.0) / half_life_days).exp();
    round_policy_score(freshness.clamp(0.0, 1.0))
}

/// Evaluate decay for one memory using a caller-provided last-access reference.
#[must_use]
pub fn evaluate_memory_decay(
    memory: &StoredMemory,
    reference: DateTime<Utc>,
    as_of: DateTime<Utc>,
    thresholds: MemoryDecayThresholds,
) -> MemoryDecayEvaluation {
    evaluate_memory_decay_with_settings(
        memory,
        reference,
        as_of,
        MemoryDecaySettings {
            thresholds,
            half_lives: MemoryDecayHalfLives::default(),
        },
    )
}

/// Evaluate decay for one memory using fully resolved settings.
#[must_use]
pub fn evaluate_memory_decay_with_settings(
    memory: &StoredMemory,
    reference: DateTime<Utc>,
    as_of: DateTime<Utc>,
    settings: MemoryDecaySettings,
) -> MemoryDecayEvaluation {
    let half_life_days = f64::from(settings.half_lives.for_memory(&memory.level, &memory.kind));
    let age_days_f64 =
        as_of.signed_duration_since(reference).num_seconds().max(0) as f64 / 86_400.0;
    let age_days = if age_days_f64.is_finite() {
        age_days_f64.floor().min(f64::from(u32::MAX)) as u32
    } else {
        u32::MAX
    };
    let freshness = memory_decay_freshness_score(age_days_f64, half_life_days);
    let confidence = finite_unit(memory.confidence);
    let utility = finite_unit(memory.utility);
    let lifecycle_score = round_policy_score(f64::from(freshness * confidence * utility));
    let demote_threshold = finite_unit(settings.thresholds.demote);
    let forget_threshold = finite_unit(settings.thresholds.forget).min(demote_threshold);

    let action = if lifecycle_score < forget_threshold {
        MemoryDecayAction::Tombstone
    } else if lifecycle_score < demote_threshold {
        MemoryDecayAction::Demote
    } else {
        MemoryDecayAction::Preserve
    };
    let new_level = if action == MemoryDecayAction::Demote {
        demoted_memory_level(&memory.level).unwrap_or(&memory.level)
    } else {
        &memory.level
    };
    let new_importance = if action == MemoryDecayAction::Demote {
        round_policy_score(f64::from(memory.importance) * 0.5)
    } else {
        finite_unit(memory.importance)
    };

    MemoryDecayEvaluation {
        action,
        freshness,
        lifecycle_score,
        half_life_days: round_policy_score(half_life_days),
        age_days,
        previous_level: memory.level.clone(),
        new_level: new_level.to_owned(),
        previous_importance: finite_unit(memory.importance),
        new_importance,
        demote_threshold,
        forget_threshold,
    }
}

fn demoted_memory_level(level: &str) -> Option<&'static str> {
    match level {
        "procedural" => Some("semantic"),
        "semantic" => Some("episodic"),
        _ => None,
    }
}

fn finite_unit(value: f32) -> f32 {
    if value.is_finite() {
        value.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

fn round_policy_score(value: f64) -> f32 {
    if value.is_finite() {
        ((value * 1_000_000.0).round() / 1_000_000.0) as f32
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    type TestResult = Result<(), String>;

    fn ensure<T: std::fmt::Debug + PartialEq>(actual: T, expected: T, ctx: &str) -> TestResult {
        if actual == expected {
            Ok(())
        } else {
            Err(format!("{ctx}: expected {expected:?}, got {actual:?}"))
        }
    }

    fn memory_fixture(level: &str, kind: &str, confidence: f32, utility: f32) -> StoredMemory {
        StoredMemory {
            id: "mem_decay0000000000000000001".to_owned(),
            workspace_id: "wsp_decay0000000000000000001".to_owned(),
            level: level.to_owned(),
            kind: kind.to_owned(),
            content: "Decay policy fixture.".to_owned(),
            workflow_id: None,
            confidence,
            utility,
            importance: 0.8,
            provenance_uri: None,
            trust_class: "human_explicit".to_owned(),
            trust_subclass: None,
            provenance_chain_hash: None,
            provenance_chain_hash_version: "ee.memory.provenance_chain.v1".to_owned(),
            provenance_verification_status: "verified".to_owned(),
            provenance_verified_at: None,
            provenance_verification_note: None,
            created_at: "2026-05-13T00:00:00Z".to_owned(),
            updated_at: "2026-05-13T00:00:00Z".to_owned(),
            tombstoned_at: None,
            valid_from: None,
            valid_to: None,
        }
    }

    #[test]
    fn freshness_half_life_is_deterministic() -> TestResult {
        ensure(memory_decay_freshness_score(0.0, 30.0), 1.0, "fresh")?;
        ensure(
            memory_decay_freshness_score(30.0, 30.0),
            0.5,
            "one half-life",
        )?;
        ensure(
            memory_decay_freshness_score(60.0, 30.0),
            0.25,
            "two half-lives",
        )
    }

    #[test]
    fn decay_threshold_classification_is_table_driven() -> TestResult {
        let as_of = "2030-01-01T00:00:00Z"
            .parse::<DateTime<Utc>>()
            .map_err(|error| error.to_string())?;
        let stale_reference = as_of - Duration::days(400);
        let thresholds = MemoryDecayThresholds::default();

        let preserve = evaluate_memory_decay(
            &memory_fixture("procedural", "rule", 0.9, 0.9),
            as_of,
            as_of,
            thresholds,
        );
        ensure(preserve.action, MemoryDecayAction::Preserve, "preserve")?;

        let demote = evaluate_memory_decay(
            &memory_fixture("procedural", "rule", 0.3, 0.2),
            stale_reference,
            as_of,
            thresholds,
        );
        ensure(demote.action, MemoryDecayAction::Demote, "demote")?;
        ensure(demote.new_level.as_str(), "semantic", "procedural demotes")?;

        let tombstone = evaluate_memory_decay(
            &memory_fixture("semantic", "fact", 0.2, 0.2),
            as_of - Duration::days(1000),
            as_of,
            thresholds,
        );
        ensure(tombstone.action, MemoryDecayAction::Tombstone, "tombstone")
    }

    #[test]
    fn decay_level_demotion_is_reversible_not_destructive() -> TestResult {
        let as_of = "2030-01-01T00:00:00Z"
            .parse::<DateTime<Utc>>()
            .map_err(|error| error.to_string())?;
        let reference = as_of - Duration::days(400);

        let semantic = evaluate_memory_decay(
            &memory_fixture("semantic", "fact", 0.4, 0.4),
            reference,
            as_of,
            MemoryDecayThresholds::default(),
        );
        ensure(
            semantic.action,
            MemoryDecayAction::Demote,
            "semantic action",
        )?;
        ensure(semantic.new_level.as_str(), "episodic", "semantic demotes")?;
        ensure(semantic.new_importance, 0.4, "importance halves")?;

        let episodic = evaluate_memory_decay(
            &memory_fixture("episodic", "event", 0.5, 0.5),
            as_of - Duration::days(90),
            as_of,
            MemoryDecayThresholds::default(),
        );
        ensure(
            episodic.action,
            MemoryDecayAction::Demote,
            "episodic action",
        )?;
        ensure(
            episodic.new_level.as_str(),
            "episodic",
            "episodic remains active",
        )
    }

    #[test]
    fn non_finite_decay_inputs_are_clamped_to_tombstone_score() -> TestResult {
        let as_of = "2030-01-01T00:00:00Z"
            .parse::<DateTime<Utc>>()
            .map_err(|error| error.to_string())?;
        let evaluation = evaluate_memory_decay(
            &memory_fixture("working", "scratch", f32::NAN, f32::INFINITY),
            as_of,
            as_of,
            MemoryDecayThresholds::default(),
        );
        ensure(evaluation.lifecycle_score, 0.0, "lifecycle score")?;
        ensure(
            evaluation.action,
            MemoryDecayAction::Tombstone,
            "non-finite action",
        )
    }
}
