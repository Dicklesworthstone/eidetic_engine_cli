//! Source-backed command advice, independent of the built-in pattern catalog.
//!
//! A native rule retains its own RuleId and never masquerades as a MemoryId.
//! All selection is request-local and read-only; this module never publishes
//! an index, loads a model, records feedback, or changes execution authority.

use chrono::{DateTime, Utc};

use crate::db::DbConnection;
use crate::models::DomainError;
use crate::pack::PackEntityRef;

use super::{
    GuardAction, GuardMatch, MatchResolution, PreflightMemoryMatch, RuleSource,
    trauma_guard_command_terms, trauma_guard_content_match,
};

/// Matches from one coherent source snapshot. Native rules and memories keep
/// their distinct identities in the existing advisory response fields.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PreflightAdvice {
    pub memories: Vec<PreflightMemoryMatch>,
    pub rules: Vec<GuardMatch>,
}

/// Retrieve explicit command advice even when no built-in pattern matches.
///
/// The caller resolves the addressed workspace/database and owns the optional
/// store degradation policy. Errors never grant or revoke command authority.
/// A successful empty result says only that no eligible advice matched, not
/// that the command is safe. No raw session transcript is interpreted as a rule.
pub fn load_preflight_advice(
    connection: &DbConnection,
    workspace_id: &str,
    command: &str,
    reference_time: DateTime<Utc>,
) -> Result<PreflightAdvice, DomainError> {
    let terms = trauma_guard_command_terms(command);
    if terms.is_empty() {
        return Ok(PreflightAdvice::default());
    }
    let corpus =
        crate::core::ask::load_command_advice_corpus(connection, workspace_id, reference_time)?;
    let mut advice = PreflightAdvice::default();
    let mut rules = Vec::new();
    for candidate in corpus.candidates {
        let Some(matched) = trauma_guard_content_match(
            &candidate.memory_id,
            &candidate.kind,
            &candidate.content,
            candidate.provenance_uri.as_deref(),
            &terms,
        ) else {
            continue;
        };
        if let Some(source) = corpus.native_sources.get(&candidate.memory_id) {
            let PackEntityRef::Rule(rule_id) = &source.entity else {
                // The command corpus admits only memory and rule identities.
                // Never let another native source type become advice by accident.
                continue;
            };
            let rule_id = rule_id.to_string();
            rules.push((
                matched.score,
                GuardMatch {
                    rule_id: rule_id.clone(),
                    // A lexical evidence match is not a configured shell glob.
                    pattern: format!("text-overlap:{}", matched.matched_terms.join(",")),
                    action: GuardAction::Warn,
                    message: matched.content,
                    source: RuleSource::ProceduralRule { rule_id },
                    resolution: MatchResolution::Matched,
                },
            ));
        } else {
            advice.memories.push(matched);
        }
    }
    advice.memories.sort_by(|left, right| {
        right
            .score
            .total_cmp(&left.score)
            .then_with(|| left.memory_id.cmp(&right.memory_id))
    });
    rules.sort_by(|(left_score, left), (right_score, right)| {
        right_score
            .total_cmp(left_score)
            .then_with(|| left.rule_id.cmp(&right.rule_id))
    });
    advice.rules = rules.into_iter().map(|(_, rule)| rule).collect();
    Ok(advice)
}

#[cfg(test)]
#[path = "preflight_advice_tests.rs"]
mod tests;
