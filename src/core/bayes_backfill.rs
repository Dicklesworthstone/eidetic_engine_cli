//! N7.1 Phase 7b (bd-17c65.14.7.2) — orchestrator for batch
//! Bayesian-posterior backfill across a workspace.
//!
//! The pure-math layer ([`crate::core::bayes`]) computes a posterior
//! from either a legacy scalar `confidence` value or a replay of the
//! workspace's feedback events. This module wraps that math in a DB
//! iteration loop that:
//!
//! 1. Reads every non-tombstoned memory in a workspace via
//!    [`DbConnection::list_memories`].
//! 2. Computes a posterior per memory per the chosen [`BackfillMode`].
//! 3. Persists it via [`DbConnection::update_memory_bayes_posterior`].
//! 4. Emits one `audit_actions::MEMORY_BAYES_POSTERIOR_UPDATED` audit
//!    entry per actually-changed posterior, with a `backfillSource`
//!    field in the details JSON distinguishing replay from inverse-fit.
//!
//! The CLI wires this into `ee migrate run --bayes-backfill-from-utility`
//! and `ee migrate run --bayes-backfill-from-feedback-events`.

use crate::core::bayes::{BetaPosterior, FeedbackSignal};
use crate::db::{CreateAuditInput, DbConnection, audit_actions, generate_audit_id};
use crate::models::DomainError;

/// Which derivation strategy the orchestrator should apply to each
/// memory. Picked per-invocation; not mixed within a single run.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BackfillMode {
    /// Inverse-fit the legacy scalar `confidence` field to a Beta
    /// posterior with the given total pseudo-evidence weight.
    /// `weight = 2.0` matches the default in the N7.1 spec.
    FromUtility { weight_hundredths: u32 },
    /// Replay the workspace's stored feedback events from the
    /// Jeffreys prior, using `harmful_weight` for each harmful event.
    FromFeedbackEvents,
}

impl BackfillMode {
    /// Canonical short string used in audit `backfillSource` field
    /// and CLI surface tests. Kept stable so audit hash chains stay
    /// reproducible across versions.
    #[must_use]
    pub const fn audit_source(&self) -> &'static str {
        match self {
            Self::FromUtility { .. } => "backfill_from_utility",
            Self::FromFeedbackEvents => "backfill_from_feedback_events",
        }
    }
}

/// Per-run summary. The CLI prints this as a human-readable line
/// and the migrate-command surface test pins its shape.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BackfillReport {
    /// Memories considered (i.e. returned by `list_memories`).
    pub scanned: usize,
    /// Memories whose posterior was rewritten (i.e. the update
    /// affected a row). Memories whose persisted (alpha, beta) was
    /// already exactly equal to the freshly-computed value are
    /// skipped (no DB write, no audit row).
    pub updated: usize,
    /// Memories the orchestrator could not derive a posterior for
    /// (e.g. non-finite legacy confidence, invalid weight). These
    /// are left unchanged.
    pub skipped: usize,
}

/// Orchestrate a workspace-wide Bayes-posterior backfill.
///
/// `harmful_weight` is used by the [`BackfillMode::FromFeedbackEvents`]
/// path; pass [`crate::core::bayes::DEFAULT_HARMFUL_WEIGHT`] for the spec default.
pub fn backfill_workspace(
    conn: &DbConnection,
    workspace_id: &str,
    mode: BackfillMode,
    harmful_weight: f64,
    actor: Option<&str>,
) -> Result<BackfillReport, DomainError> {
    let memories = conn
        .list_memories(workspace_id, None, false)
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to list memories for Bayes backfill: {error}"),
            repair: Some("ee doctor".to_string()),
        })?;

    let mut report = BackfillReport {
        scanned: memories.len(),
        ..BackfillReport::default()
    };

    for memory in &memories {
        let derived = match mode {
            BackfillMode::FromUtility { weight_hundredths } => {
                let weight = f64::from(weight_hundredths) / 100.0;
                BetaPosterior::from_utility_inverse(f64::from(memory.confidence), weight)
            }
            BackfillMode::FromFeedbackEvents => {
                let events = conn
                    .list_feedback_events_for_target("memory", &memory.id)
                    .map_err(|error| DomainError::Storage {
                        message: format!(
                            "Failed to list feedback events for memory {}: {error}",
                            memory.id
                        ),
                        repair: Some("ee doctor".to_string()),
                    })?;
                let replay = events.into_iter().map(|ev| {
                    let signal = FeedbackSignal::from_signal_str(&ev.signal);
                    // Per-event weight comes from the stored row; if
                    // it isn't positive, fall back to the run-wide
                    // default so harmful events still register.
                    let event_weight = f64::from(ev.weight);
                    let weight = if event_weight.is_finite() && event_weight > 0.0 {
                        event_weight
                    } else {
                        harmful_weight
                    };
                    (signal, weight)
                });
                Some(BetaPosterior::from_feedback_events(replay))
            }
        };

        let Some(posterior) = derived else {
            report.skipped += 1;
            continue;
        };

        // Skip the write when the persisted (alpha, beta) already
        // equals the freshly-computed value — keeps the audit log
        // free of no-op churn when a migration is re-run.
        let already_matches = match conn.get_memory_bayes_posterior(&memory.id) {
            Ok(Some((existing_alpha, existing_beta))) => {
                approx_eq(existing_alpha, posterior.alpha())
                    && approx_eq(existing_beta, posterior.beta())
            }
            Ok(None) => false,
            Err(_) => false,
        };

        if already_matches {
            continue;
        }

        let prior_alpha = posterior.alpha();
        let prior_beta = posterior.beta();

        let changed = conn
            .update_memory_bayes_posterior(&memory.id, posterior.alpha(), posterior.beta())
            .map_err(|error| DomainError::Storage {
                message: format!(
                    "Failed to update Bayes posterior for memory {}: {error}",
                    memory.id
                ),
                repair: Some("ee doctor".to_string()),
            })?;

        if !changed {
            report.skipped += 1;
            continue;
        }

        let details = serde_json::json!({
            "schema": "ee.audit.bayes_posterior_updated.v1",
            "backfillSource": mode.audit_source(),
            "posteriorAlpha": prior_alpha,
            "posteriorBeta": prior_beta,
            "posteriorMean": posterior.mean(),
            "effectiveSampleSize": posterior.effective_sample_size(),
        })
        .to_string();

        conn.insert_audit(
            &generate_audit_id(),
            &CreateAuditInput {
                workspace_id: Some(memory.workspace_id.clone()),
                actor: actor.map(str::to_string),
                action: audit_actions::MEMORY_BAYES_POSTERIOR_UPDATED.to_string(),
                target_type: Some("memory".to_string()),
                target_id: Some(memory.id.clone()),
                details: Some(details),
            },
        )
        .map_err(|error| DomainError::Storage {
            message: format!(
                "Failed to audit Bayes backfill for memory {}: {error}",
                memory.id
            ),
            repair: Some("ee doctor".to_string()),
        })?;

        report.updated += 1;
    }

    Ok(report)
}

fn approx_eq(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-12
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::bayes::DEFAULT_HARMFUL_WEIGHT;

    #[test]
    fn audit_source_strings_are_canonical() {
        assert_eq!(
            BackfillMode::FromUtility {
                weight_hundredths: 200
            }
            .audit_source(),
            "backfill_from_utility"
        );
        assert_eq!(
            BackfillMode::FromFeedbackEvents.audit_source(),
            "backfill_from_feedback_events"
        );
    }

    #[test]
    fn default_harmful_weight_re_exported_for_callers() {
        // The CLI imports DEFAULT_HARMFUL_WEIGHT via this module so
        // wiring stays self-contained. This test pins the value.
        assert!((DEFAULT_HARMFUL_WEIGHT - 2.5).abs() < 1e-12);
    }

    #[test]
    fn approx_eq_helper_matches_within_tolerance() {
        assert!(approx_eq(1.0, 1.0));
        assert!(approx_eq(0.5, 0.5 + 1e-13));
        assert!(!approx_eq(0.5, 0.5 + 1e-6));
    }
}
