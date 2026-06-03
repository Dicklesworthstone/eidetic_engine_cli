//! Pure SRR5 per-agent adaptive scheduler model.
//!
//! The daemon wiring lives in follow-up slices. This module is deliberately
//! side-effect free: callers feed observed latencies in, and the model returns
//! an advisory pass/backoff decision for that agent only.

use std::collections::{BTreeMap, VecDeque};

use serde::Serialize;

use super::cass_prefetch::ADAPTIVE_BACKOFF_APPLIED_CODE;

pub const ADAPTIVE_SCHEDULER_SCHEMA_V1: &str = "ee.swarm_adaptive.scheduler.v1";
pub const DEFAULT_ADAPTIVE_SAMPLE_WINDOW: usize = 64;
pub const DEFAULT_ADAPTIVE_NOISY_P99_MS: u64 = 200;
pub const DEFAULT_ADAPTIVE_BACKOFF_MS: u64 = 25;
pub const DEFAULT_ADAPTIVE_MAX_BACKOFF_MULTIPLIER: u64 = 4;

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdaptiveSchedulerConfig {
    pub enabled: bool,
    pub sample_window: usize,
    pub noisy_neighbor_p99_ms: u64,
    pub noisy_neighbor_release_p99_ms: u64,
    pub noisy_neighbor_backoff_ms: u64,
    pub max_backoff_ms: u64,
}

impl Default for AdaptiveSchedulerConfig {
    fn default() -> Self {
        Self::new(
            true,
            DEFAULT_ADAPTIVE_SAMPLE_WINDOW,
            DEFAULT_ADAPTIVE_NOISY_P99_MS,
            DEFAULT_ADAPTIVE_BACKOFF_MS,
        )
    }
}

impl AdaptiveSchedulerConfig {
    #[must_use]
    pub const fn new(
        enabled: bool,
        sample_window: usize,
        noisy_neighbor_p99_ms: u64,
        noisy_neighbor_backoff_ms: u64,
    ) -> Self {
        let threshold = if noisy_neighbor_p99_ms == 0 {
            DEFAULT_ADAPTIVE_NOISY_P99_MS
        } else {
            noisy_neighbor_p99_ms
        };
        let base_backoff = if noisy_neighbor_backoff_ms == 0 {
            DEFAULT_ADAPTIVE_BACKOFF_MS
        } else {
            noisy_neighbor_backoff_ms
        };
        let window = if sample_window == 0 {
            DEFAULT_ADAPTIVE_SAMPLE_WINDOW
        } else {
            sample_window
        };
        let release_threshold = threshold.saturating_mul(4).saturating_div(5);
        let max_backoff = base_backoff.saturating_mul(DEFAULT_ADAPTIVE_MAX_BACKOFF_MULTIPLIER);

        Self {
            enabled,
            sample_window: window,
            noisy_neighbor_p99_ms: threshold,
            noisy_neighbor_release_p99_ms: release_threshold,
            noisy_neighbor_backoff_ms: base_backoff,
            max_backoff_ms: max_backoff,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdaptiveScheduler {
    config: AdaptiveSchedulerConfig,
    agents: BTreeMap<String, AgentAdaptiveState>,
}

impl AdaptiveScheduler {
    #[must_use]
    pub fn new(config: AdaptiveSchedulerConfig) -> Self {
        Self {
            config,
            agents: BTreeMap::new(),
        }
    }

    #[must_use]
    pub const fn config(&self) -> &AdaptiveSchedulerConfig {
        &self.config
    }

    #[must_use]
    pub fn agent_count(&self) -> usize {
        self.agents.len()
    }

    #[must_use]
    pub fn state_for(&self, agent_id: &str) -> Option<&AgentAdaptiveState> {
        self.agents.get(agent_id)
    }

    pub fn observe_latency_ms(
        &mut self,
        agent_id: impl Into<String>,
        latency_ms: u64,
    ) -> AdaptiveSchedulerDecision {
        let agent_id = agent_id.into();
        let sample_window = self.config.sample_window;
        let state = self
            .agents
            .entry(agent_id.clone())
            .or_insert_with(|| AgentAdaptiveState::new(sample_window));
        state.record_latency_ms(latency_ms, sample_window);
        let decision = state.decision(&agent_id, &self.config);
        state.backoff_active = decision.outcome == AdaptiveSchedulerOutcome::Backoff;
        decision
    }

    #[must_use]
    pub fn decision_for(&self, agent_id: &str) -> AdaptiveSchedulerDecision {
        match self.agents.get(agent_id) {
            Some(state) => state.decision(agent_id, &self.config),
            None => AdaptiveSchedulerDecision::empty(agent_id, &self.config),
        }
    }
}

impl Default for AdaptiveScheduler {
    fn default() -> Self {
        Self::new(AdaptiveSchedulerConfig::default())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentAdaptiveState {
    latencies_ms: VecDeque<u64>,
    pub backoff_active: bool,
}

impl AgentAdaptiveState {
    #[must_use]
    pub fn new(sample_window: usize) -> Self {
        Self {
            latencies_ms: VecDeque::with_capacity(sample_window.max(1)),
            backoff_active: false,
        }
    }

    pub fn record_latency_ms(&mut self, latency_ms: u64, sample_window: usize) {
        let sample_window = sample_window.max(1);
        self.latencies_ms.push_back(latency_ms);
        while self.latencies_ms.len() > sample_window {
            self.latencies_ms.pop_front();
        }
    }

    #[must_use]
    pub fn sample_count(&self) -> usize {
        self.latencies_ms.len()
    }

    #[must_use]
    pub fn latencies_ms(&self) -> impl Iterator<Item = u64> + '_ {
        self.latencies_ms.iter().copied()
    }

    #[must_use]
    pub fn percentiles(&self) -> LatencyPercentiles {
        LatencyPercentiles::from_samples(self.latencies_ms.iter().copied())
    }

    #[must_use]
    fn decision(
        &self,
        agent_id: &str,
        config: &AdaptiveSchedulerConfig,
    ) -> AdaptiveSchedulerDecision {
        let percentiles = self.percentiles();
        if !config.enabled {
            return AdaptiveSchedulerDecision::new(
                agent_id,
                AdaptiveSchedulerOutcome::Disabled,
                percentiles,
                config,
                0,
            );
        }

        let should_backoff = if self.backoff_active {
            percentiles.p99_ms > config.noisy_neighbor_release_p99_ms
        } else {
            percentiles.p99_ms > config.noisy_neighbor_p99_ms
        };
        let backoff_ms = should_backoff
            .then(|| proportional_backoff_ms(percentiles.p99_ms, config))
            .unwrap_or(0);
        let outcome = if should_backoff {
            AdaptiveSchedulerOutcome::Backoff
        } else {
            AdaptiveSchedulerOutcome::Pass
        };

        AdaptiveSchedulerDecision::new(agent_id, outcome, percentiles, config, backoff_ms)
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LatencyPercentiles {
    pub sample_count: usize,
    pub p50_ms: u64,
    pub p95_ms: u64,
    pub p99_ms: u64,
}

impl LatencyPercentiles {
    #[must_use]
    pub fn from_samples(samples: impl IntoIterator<Item = u64>) -> Self {
        let mut values: Vec<u64> = samples.into_iter().collect();
        if values.is_empty() {
            return Self::default();
        }
        values.sort_unstable();

        Self {
            sample_count: values.len(),
            p50_ms: nearest_rank(&values, 50),
            p95_ms: nearest_rank(&values, 95),
            p99_ms: nearest_rank(&values, 99),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AdaptiveSchedulerOutcome {
    Pass,
    Backoff,
    Disabled,
}

impl AdaptiveSchedulerOutcome {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Backoff => "backoff",
            Self::Disabled => "disabled",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdaptiveSchedulerDecision {
    pub schema: &'static str,
    pub agent_id: String,
    pub outcome: AdaptiveSchedulerOutcome,
    pub percentiles: LatencyPercentiles,
    pub threshold_p99_ms: u64,
    pub release_p99_ms: u64,
    pub backoff_ms: u64,
    pub degraded_code: Option<&'static str>,
}

impl AdaptiveSchedulerDecision {
    #[must_use]
    fn empty(agent_id: &str, config: &AdaptiveSchedulerConfig) -> Self {
        Self::new(
            agent_id,
            if config.enabled {
                AdaptiveSchedulerOutcome::Pass
            } else {
                AdaptiveSchedulerOutcome::Disabled
            },
            LatencyPercentiles::default(),
            config,
            0,
        )
    }

    #[must_use]
    fn new(
        agent_id: &str,
        outcome: AdaptiveSchedulerOutcome,
        percentiles: LatencyPercentiles,
        config: &AdaptiveSchedulerConfig,
        backoff_ms: u64,
    ) -> Self {
        Self {
            schema: ADAPTIVE_SCHEDULER_SCHEMA_V1,
            agent_id: agent_id.to_owned(),
            outcome,
            percentiles,
            threshold_p99_ms: config.noisy_neighbor_p99_ms,
            release_p99_ms: config.noisy_neighbor_release_p99_ms,
            backoff_ms,
            degraded_code: (outcome == AdaptiveSchedulerOutcome::Backoff)
                .then_some(ADAPTIVE_BACKOFF_APPLIED_CODE),
        }
    }
}

#[must_use]
fn nearest_rank(sorted_values: &[u64], percentile: usize) -> u64 {
    debug_assert!(!sorted_values.is_empty());
    let rank = sorted_values
        .len()
        .saturating_mul(percentile)
        .saturating_add(99)
        / 100;
    sorted_values[rank.saturating_sub(1).min(sorted_values.len() - 1)]
}

#[must_use]
fn proportional_backoff_ms(p99_ms: u64, config: &AdaptiveSchedulerConfig) -> u64 {
    if p99_ms <= config.noisy_neighbor_p99_ms {
        return config.noisy_neighbor_backoff_ms.min(config.max_backoff_ms);
    }
    let scaled = config
        .noisy_neighbor_backoff_ms
        .saturating_mul(p99_ms)
        .saturating_add(config.noisy_neighbor_p99_ms.saturating_sub(1))
        / config.noisy_neighbor_p99_ms.max(1);
    scaled
        .max(config.noisy_neighbor_backoff_ms)
        .min(config.max_backoff_ms)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> AdaptiveSchedulerConfig {
        AdaptiveSchedulerConfig::new(true, 4, 200, 25)
    }

    #[test]
    fn rolling_window_percentiles_are_deterministic() {
        let mut scheduler = AdaptiveScheduler::new(test_config());

        for latency in [10, 100, 30, 20, 40] {
            scheduler.observe_latency_ms("agent-a", latency);
        }

        let state = scheduler.state_for("agent-a").expect("state exists");
        assert_eq!(
            state.latencies_ms().collect::<Vec<_>>(),
            vec![100, 30, 20, 40]
        );
        assert_eq!(
            state.percentiles(),
            LatencyPercentiles {
                sample_count: 4,
                p50_ms: 30,
                p95_ms: 100,
                p99_ms: 100,
            }
        );
    }

    #[test]
    fn backoff_scales_with_agent_p99_and_reports_degraded_code() {
        let mut scheduler = AdaptiveScheduler::new(AdaptiveSchedulerConfig::new(true, 2, 100, 10));

        scheduler.observe_latency_ms("mild", 20);
        let mild = scheduler.observe_latency_ms("mild", 125);
        scheduler.observe_latency_ms("hot", 20);
        let hot = scheduler.observe_latency_ms("hot", 350);

        assert_eq!(mild.outcome, AdaptiveSchedulerOutcome::Backoff);
        assert_eq!(hot.outcome, AdaptiveSchedulerOutcome::Backoff);
        assert!(hot.backoff_ms > mild.backoff_ms);
        assert_eq!(hot.degraded_code, Some(ADAPTIVE_BACKOFF_APPLIED_CODE));
    }

    #[test]
    fn hysteresis_keeps_backoff_until_release_threshold_is_clear() {
        let mut scheduler = AdaptiveScheduler::new(AdaptiveSchedulerConfig::new(true, 1, 200, 25));

        let first = scheduler.observe_latency_ms("agent-a", 250);
        let still_hot = scheduler.observe_latency_ms("agent-a", 190);
        let released = scheduler.observe_latency_ms("agent-a", 150);

        assert_eq!(first.outcome, AdaptiveSchedulerOutcome::Backoff);
        assert_eq!(still_hot.outcome, AdaptiveSchedulerOutcome::Backoff);
        assert_eq!(released.outcome, AdaptiveSchedulerOutcome::Pass);
        assert_eq!(released.backoff_ms, 0);
        assert_eq!(released.degraded_code, None);
    }

    #[test]
    fn disabled_mode_observes_without_backoff() {
        let mut scheduler = AdaptiveScheduler::new(AdaptiveSchedulerConfig::new(false, 4, 200, 25));

        let decision = scheduler.observe_latency_ms("agent-a", 2_000);

        assert_eq!(decision.outcome, AdaptiveSchedulerOutcome::Disabled);
        assert_eq!(decision.percentiles.p99_ms, 2_000);
        assert_eq!(decision.backoff_ms, 0);
        assert_eq!(decision.degraded_code, None);
    }

    #[test]
    fn per_agent_state_is_isolated() {
        let mut scheduler = AdaptiveScheduler::new(AdaptiveSchedulerConfig::new(true, 2, 200, 25));

        let hot = scheduler.observe_latency_ms("agent-a", 500);
        let cool = scheduler.observe_latency_ms("agent-b", 50);

        assert_eq!(scheduler.agent_count(), 2);
        assert_eq!(hot.outcome, AdaptiveSchedulerOutcome::Backoff);
        assert_eq!(cool.outcome, AdaptiveSchedulerOutcome::Pass);
        assert_eq!(
            scheduler.decision_for("agent-a").outcome,
            AdaptiveSchedulerOutcome::Backoff
        );
        assert_eq!(
            scheduler.decision_for("agent-b").outcome,
            AdaptiveSchedulerOutcome::Pass
        );
    }

    #[test]
    fn empty_agent_decision_is_deterministic_pass() {
        let scheduler = AdaptiveScheduler::new(test_config());

        let decision = scheduler.decision_for("new-agent");

        assert_eq!(decision.outcome, AdaptiveSchedulerOutcome::Pass);
        assert_eq!(decision.percentiles, LatencyPercentiles::default());
        assert_eq!(decision.schema, ADAPTIVE_SCHEDULER_SCHEMA_V1);
    }

    #[test]
    fn zero_config_values_fall_back_to_safe_defaults() {
        let config = AdaptiveSchedulerConfig::new(true, 0, 0, 0);

        assert_eq!(config.sample_window, DEFAULT_ADAPTIVE_SAMPLE_WINDOW);
        assert_eq!(config.noisy_neighbor_p99_ms, DEFAULT_ADAPTIVE_NOISY_P99_MS);
        assert_eq!(
            config.noisy_neighbor_backoff_ms,
            DEFAULT_ADAPTIVE_BACKOFF_MS
        );
        assert_eq!(config.noisy_neighbor_release_p99_ms, 160);
        assert_eq!(
            config.max_backoff_ms,
            DEFAULT_ADAPTIVE_BACKOFF_MS * DEFAULT_ADAPTIVE_MAX_BACKOFF_MULTIPLIER
        );
    }

    #[test]
    fn proportional_backoff_is_capped() {
        let mut scheduler = AdaptiveScheduler::new(AdaptiveSchedulerConfig::new(true, 1, 100, 10));

        let decision = scheduler.observe_latency_ms("agent-a", 10_000);

        assert_eq!(decision.outcome, AdaptiveSchedulerOutcome::Backoff);
        assert_eq!(decision.backoff_ms, 40);
    }

    #[test]
    fn decision_serializes_stable_camel_case_contract() {
        let mut scheduler = AdaptiveScheduler::new(AdaptiveSchedulerConfig::new(true, 1, 100, 10));

        let value = serde_json::to_value(scheduler.observe_latency_ms("agent-a", 250))
            .expect("decision serializes");

        assert_eq!(
            value,
            serde_json::json!({
                "schema": ADAPTIVE_SCHEDULER_SCHEMA_V1,
                "agentId": "agent-a",
                "outcome": "backoff",
                "percentiles": {
                    "sampleCount": 1,
                    "p50Ms": 250,
                    "p95Ms": 250,
                    "p99Ms": 250,
                },
                "thresholdP99Ms": 100,
                "releaseP99Ms": 80,
                "backoffMs": 25,
                "degradedCode": ADAPTIVE_BACKOFF_APPLIED_CODE,
            })
        );
    }
}
