//! Bounded, opt-in context admission around the pure per-agent policy.
//!
//! Delayed workers keep a resident permit but release foreground capacity.
//! Configuration, observations, and pending slots are generation checked so
//! a stale request cannot restore disabled policy or charge a new identity.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::config::SwarmAdaptiveConfig;
use crate::core::adaptive_scheduler::{
    AdaptiveScheduler, AdaptiveSchedulerConfig, DEFAULT_ADAPTIVE_BACKOFF_MS,
    DEFAULT_ADAPTIVE_NOISY_P99_MS, DEFAULT_ADAPTIVE_SAMPLE_WINDOW,
};
use crate::core::cass_prefetch::{
    CassPrefetchCoordinator, CassPrefetchPolicy, CassPrefetchPolicyRefresh,
    CassPrefetchPolicyTicket, RecencyWeightedFrequencyPredictor,
};

use super::{DAEMON_DEFAULT_RPC_TIMEOUT, InflightPermit};

const MAX_TRACKED_AGENTS: usize = 256;
const MAX_AGENT_ID_BYTES: usize = 1024;
const MAX_BACKOFF_MS: u64 = 1000;
const BACKOFF_POLL_INTERVAL: Duration = Duration::from_millis(5);

type PrefetchCoordinator = Mutex<CassPrefetchCoordinator<RecencyWeightedFrequencyPredictor>>;

#[derive(Debug)]
pub(super) struct DaemonAdaptiveScheduler {
    state: Mutex<RuntimeState>,
}

#[derive(Debug)]
struct RuntimeState {
    refresh_sequence: u64,
    epoch: u64,
    next_identity: u64,
    next_pending: u64,
    workspace: Option<PathBuf>,
    scheduler: AdaptiveScheduler,
    identities: BTreeMap<String, TrackedIdentity>,
    // Retain reservations across policy resets until their owners drop them.
    // Clearing this map would permit two waiting requests for the same agent.
    pending: BTreeMap<(PathBuf, String), u64>,
}

#[derive(Debug)]
struct TrackedIdentity {
    token: u64,
    last_used: u64,
}

#[derive(Clone, Debug)]
pub(super) struct AdaptiveRequestTicket {
    epoch: u64,
    identity: u64,
    agent_id: String,
}

#[derive(Debug, Default)]
pub(super) struct AdaptivePolicyTickets {
    pub(super) scheduler: Option<AdaptiveRequestTicket>,
    pub(super) prefetch: Option<CassPrefetchPolicyTicket>,
}

struct PolicyRefresh {
    sequence: u64,
    prefetch: CassPrefetchPolicyRefresh,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum AdaptiveAdmissionError {
    Deadline,
    Shutdown,
    Overloaded,
}

impl Default for DaemonAdaptiveScheduler {
    fn default() -> Self {
        Self {
            state: Mutex::new(RuntimeState {
                refresh_sequence: 0,
                epoch: 0,
                next_identity: 0,
                next_pending: 0,
                workspace: None,
                scheduler: AdaptiveScheduler::new(runtime_config(&SwarmAdaptiveConfig::default())),
                identities: BTreeMap::new(),
                pending: BTreeMap::new(),
            }),
        }
    }
}

impl DaemonAdaptiveScheduler {
    pub(super) fn configure_workspace(
        &self,
        prefetch: &PrefetchCoordinator,
        workspace: &Path,
        agent_id: &str,
    ) -> AdaptivePolicyTickets {
        self.refresh_policy(prefetch, workspace, agent_id, || {
            match crate::core::config_surface::merged_workspace_config(workspace) {
                Ok(config) => config.values.swarm.adaptive,
                Err(_) => {
                    // Parser errors can include configuration literals. Optional
                    // adaptation fails closed without reflecting those contents.
                    tracing::debug!(
                        target: "ee::adaptive_scheduler",
                        "adaptive behavior disabled: workspace configuration unavailable"
                    );
                    SwarmAdaptiveConfig::default()
                }
            }
        })
    }

    fn refresh_policy(
        &self,
        prefetch: &PrefetchCoordinator,
        workspace: &Path,
        agent_id: &str,
        load: impl FnOnce() -> SwarmAdaptiveConfig,
    ) -> AdaptivePolicyTickets {
        let Some(refresh) = self.begin_refresh(prefetch) else {
            return AdaptivePolicyTickets::default();
        };
        // One merged load drives both adaptive features. Neither mutex is held
        // during filesystem I/O, and a newer reservation supersedes this read.
        let config = load();
        self.finish_refresh(prefetch, refresh, workspace, agent_id, &config)
    }

    fn begin_refresh(&self, prefetch: &PrefetchCoordinator) -> Option<PolicyRefresh> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let sequence = state.refresh_sequence.checked_add(1)?;
        let prefetch = prefetch
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .begin_runtime_policy_refresh()?;
        state.refresh_sequence = sequence;
        Some(PolicyRefresh { sequence, prefetch })
    }

    fn finish_refresh(
        &self,
        prefetch: &PrefetchCoordinator,
        refresh: PolicyRefresh,
        workspace: &Path,
        agent_id: &str,
        config: &SwarmAdaptiveConfig,
    ) -> AdaptivePolicyTickets {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.refresh_sequence != refresh.sequence {
            return AdaptivePolicyTickets::default();
        }
        let policy = runtime_config(config);
        if state.scheduler.config() != &policy || state.workspace.as_deref() != Some(workspace) {
            state.scheduler = AdaptiveScheduler::new(policy);
            state.identities.clear();
            state.workspace = Some(workspace.to_owned());
            state.epoch = refresh.sequence;
        }
        let prefetch = prefetch
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .finish_runtime_policy_refresh(
                refresh.prefetch,
                CassPrefetchPolicy::from_config(config),
            );
        let scheduler = state.ticket_for(agent_id);
        AdaptivePolicyTickets {
            scheduler,
            prefetch,
        }
    }

    pub(super) fn observe_execution(
        &self,
        ticket: Option<&AdaptiveRequestTicket>,
        elapsed: Duration,
    ) {
        let Some(ticket) = ticket else { return };
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.ticket_is_current(ticket) {
            // Observe only execution, never our own delay or capacity reacquisition.
            // Disabled requests create neither a model history nor an identity.
            let milliseconds = u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX);
            state
                .scheduler
                .observe_latency_ms(&ticket.agent_id, milliseconds);
        }
    }

    pub(super) fn admit(
        &self,
        ticket: Option<&AdaptiveRequestTicket>,
        permit: Option<&mut InflightPermit>,
        shutdown: &AtomicBool,
        started: Instant,
        timeout_ms: Option<u64>,
    ) -> Result<bool, AdaptiveAdmissionError> {
        self.admit_with_wait(ticket, permit, |pending| {
            let waiting_started = Instant::now();
            wait_for_backoff(
                pending.delay,
                || waiting_started.elapsed(),
                || {
                    if shutdown.load(Ordering::SeqCst) {
                        return BackoffPoll::Stop(AdaptiveAdmissionError::Shutdown);
                    }
                    let Some(remaining) = remaining_context_budget(timeout_ms, started.elapsed())
                    else {
                        return BackoffPoll::Stop(AdaptiveAdmissionError::Deadline);
                    };
                    if !self.ticket_is_current(&pending.ticket) {
                        return BackoffPoll::PolicyChanged;
                    }
                    BackoffPoll::Continue(remaining)
                },
                std::thread::sleep,
            )
        })
    }

    fn admit_with_wait(
        &self,
        ticket: Option<&AdaptiveRequestTicket>,
        permit: Option<&mut InflightPermit>,
        wait: impl FnOnce(&PendingBackoff<'_>) -> Result<bool, AdaptiveAdmissionError>,
    ) -> Result<bool, AdaptiveAdmissionError> {
        let (Some(ticket), Some(permit)) = (ticket, permit) else {
            return Ok(false);
        };
        let Some(pending) = self.reserve_backoff(ticket, permit.pool.capacity)? else {
            return Ok(false);
        };
        if !permit.suspend() {
            return Err(AdaptiveAdmissionError::Overloaded);
        }
        // No mutex is held here. The resident bound remains in force while
        // another agent can acquire the foreground capacity we released.
        let applied = wait(&pending)?;
        // Rejoin once through the same active cap. A busy foreground pool is
        // refused instead of becoming a second, repeatedly sleeping queue.
        if !permit.resume() {
            return Err(AdaptiveAdmissionError::Overloaded);
        }
        Ok(applied)
    }

    fn ticket_is_current(&self, ticket: &AdaptiveRequestTicket) -> bool {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .ticket_is_current(ticket)
    }

    fn reserve_backoff(
        &self,
        ticket: &AdaptiveRequestTicket,
        capacity: usize,
    ) -> Result<Option<PendingBackoff<'_>>, AdaptiveAdmissionError> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !state.ticket_is_current(ticket) {
            return Ok(None);
        }
        let Some(workspace) = state.workspace.clone() else {
            return Ok(None);
        };
        let key = (workspace, ticket.agent_id.clone());
        if state.pending.contains_key(&key) {
            return Err(AdaptiveAdmissionError::Overloaded);
        }
        let delay =
            Duration::from_millis(state.scheduler.decision_for(&ticket.agent_id).backoff_ms);
        if delay.is_zero() {
            return Ok(None);
        }
        if state.pending.len() >= capacity.min(MAX_TRACKED_AGENTS) {
            return Err(AdaptiveAdmissionError::Overloaded);
        }
        let token = state
            .next_pending
            .checked_add(1)
            .ok_or(AdaptiveAdmissionError::Overloaded)?;
        state.next_pending = token;
        state.pending.insert(key.clone(), token);
        Ok(Some(PendingBackoff {
            owner: self,
            key,
            token,
            ticket: ticket.clone(),
            delay,
        }))
    }
}

impl RuntimeState {
    fn ticket_is_current(&self, ticket: &AdaptiveRequestTicket) -> bool {
        self.scheduler.config().enabled
            && self.epoch == ticket.epoch
            && self
                .identities
                .get(&ticket.agent_id)
                .is_some_and(|identity| identity.token == ticket.identity)
    }

    fn ticket_for(&mut self, agent_id: &str) -> Option<AdaptiveRequestTicket> {
        if !self.scheduler.config().enabled
            || agent_id.trim().is_empty()
            || agent_id.len() > MAX_AGENT_ID_BYTES
        {
            return None;
        }
        if !self.identities.contains_key(agent_id) {
            let token = self.next_identity.checked_add(1)?;
            if self.identities.len() >= MAX_TRACKED_AGENTS {
                let workspace = self.workspace.as_ref()?;
                // Deterministic least-recently admitted eviction. Pending
                // owners are protected; in-flight completions carry a unique
                // identity token and cannot resurrect an evicted history.
                let evicted = self
                    .identities
                    .iter()
                    .filter(|(agent, _)| {
                        !self
                            .pending
                            .contains_key(&(workspace.clone(), (*agent).clone()))
                    })
                    .min_by_key(|(agent, identity)| (identity.last_used, *agent))
                    .map(|(agent, _)| agent.clone())?;
                self.identities.remove(&evicted);
                self.scheduler.forget_agent(&evicted);
            }
            self.next_identity = token;
            self.identities.insert(
                agent_id.to_owned(),
                TrackedIdentity {
                    token,
                    last_used: self.refresh_sequence,
                },
            );
        }
        let identity = self.identities.get_mut(agent_id)?;
        identity.last_used = self.refresh_sequence;
        Some(AdaptiveRequestTicket {
            epoch: self.epoch,
            identity: identity.token,
            agent_id: agent_id.to_owned(),
        })
    }
}

struct PendingBackoff<'a> {
    owner: &'a DaemonAdaptiveScheduler,
    key: (PathBuf, String),
    token: u64,
    ticket: AdaptiveRequestTicket,
    delay: Duration,
}

impl Drop for PendingBackoff<'_> {
    fn drop(&mut self) {
        let mut state = self
            .owner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.pending.get(&self.key) == Some(&self.token) {
            state.pending.remove(&self.key);
        }
    }
}

fn runtime_config(config: &SwarmAdaptiveConfig) -> AdaptiveSchedulerConfig {
    let mut policy = AdaptiveSchedulerConfig::new(
        config.enabled.unwrap_or(false),
        DEFAULT_ADAPTIVE_SAMPLE_WINDOW,
        config
            .noisy_neighbor_p99_ms
            .unwrap_or(DEFAULT_ADAPTIVE_NOISY_P99_MS),
        config
            .noisy_neighbor_backoff_ms
            .unwrap_or(DEFAULT_ADAPTIVE_BACKOFF_MS)
            .min(MAX_BACKOFF_MS),
    );
    policy.max_backoff_ms = policy.max_backoff_ms.min(MAX_BACKOFF_MS);
    policy
}

/// The socket already uses this default. Apply it to the whole context job,
/// including authorized path/config work and admission, then pass only the
/// remaining budget to core instead of starting a fresh timeout after waiting.
pub(super) fn remaining_context_budget(
    timeout_ms: Option<u64>,
    elapsed: Duration,
) -> Option<Duration> {
    let timeout = timeout_ms
        .map(Duration::from_millis)
        .unwrap_or(DAEMON_DEFAULT_RPC_TIMEOUT);
    timeout
        .checked_sub(elapsed)
        .filter(|remaining| !remaining.is_zero())
}

enum BackoffPoll {
    Continue(Duration),
    PolicyChanged,
    Stop(AdaptiveAdmissionError),
}

fn wait_for_backoff(
    delay: Duration,
    mut elapsed: impl FnMut() -> Duration,
    mut poll: impl FnMut() -> BackoffPoll,
    mut wait: impl FnMut(Duration),
) -> Result<bool, AdaptiveAdmissionError> {
    let mut applied = false;
    loop {
        let budget = match poll() {
            BackoffPoll::Continue(budget) => budget,
            BackoffPoll::PolicyChanged => return Ok(applied),
            BackoffPoll::Stop(error) => return Err(error),
        };
        let remaining = delay.saturating_sub(elapsed());
        if remaining.is_zero() {
            return Ok(applied);
        }
        let step = remaining.min(budget).min(BACKOFF_POLL_INTERVAL);
        if step.is_zero() {
            return Err(AdaptiveAdmissionError::Deadline);
        }
        wait(step);
        applied = true;
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::ffi::OsString;

    use super::super::InflightPool;
    use super::*;
    use crate::config::{ConfigLayers, PathExpander, config_from_env, merge_config};
    use crate::core::adaptive_scheduler::AdaptiveSchedulerOutcome;

    fn enabled() -> SwarmAdaptiveConfig {
        SwarmAdaptiveConfig {
            enabled: Some(true),
            ..SwarmAdaptiveConfig::default()
        }
    }

    fn configure(
        runtime: &DaemonAdaptiveScheduler,
        prefetch: &PrefetchCoordinator,
        agent: &str,
        config: SwarmAdaptiveConfig,
    ) -> AdaptivePolicyTickets {
        runtime.refresh_policy(prefetch, Path::new("/workspace/authorized"), agent, || {
            config
        })
    }

    fn hot_ticket(
        runtime: &DaemonAdaptiveScheduler,
        prefetch: &PrefetchCoordinator,
        agent: &str,
    ) -> AdaptiveRequestTicket {
        let ticket = configure(runtime, prefetch, agent, enabled())
            .scheduler
            .expect("enabled ticket");
        runtime.observe_execution(Some(&ticket), Duration::from_millis(400));
        ticket
    }

    #[test]
    fn default_and_environment_disabled_admission_leave_no_history() {
        let runtime = DaemonAdaptiveScheduler::default();
        let prefetch = Mutex::new(CassPrefetchCoordinator::new());
        let disabled = configure(
            &runtime,
            &prefetch,
            "agent-a",
            SwarmAdaptiveConfig::default(),
        );
        assert!(disabled.scheduler.is_none());
        assert!(disabled.prefetch.is_none());
        runtime.observe_execution(disabled.scheduler.as_ref(), Duration::from_secs(10));
        assert_eq!(runtime.state.lock().unwrap().scheduler.agent_count(), 0);
        assert!(runtime.state.lock().unwrap().identities.is_empty());

        // Exercise the actual typed environment merge without process-global
        // environment mutation (including the false kill-switch no-op).
        for (project_enabled, override_value, expected) in
            [(true, "1", false), (false, "0", false), (true, "0", true)]
        {
            let env = BTreeMap::from([(
                "EE_DISABLE_ADAPTIVE".to_owned(),
                OsString::from(override_value),
            )]);
            let expander = PathExpander::with_env(None, BTreeMap::new());
            let mut layers = ConfigLayers::default();
            layers.project.swarm.adaptive.enabled = Some(project_enabled);
            layers.environment = config_from_env(&env, &expander).expect("typed adaptive override");
            let config = merge_config(&layers).values.swarm.adaptive;
            let tickets = configure(&runtime, &prefetch, "agent-a", config);
            assert_eq!(tickets.scheduler.is_some(), expected);
            assert_eq!(tickets.prefetch.is_some(), expected);
        }
    }

    #[test]
    fn configured_backoff_is_bounded_and_zero_uses_existing_defaults() {
        let policy = runtime_config(&SwarmAdaptiveConfig {
            enabled: Some(true),
            noisy_neighbor_p99_ms: Some(1),
            noisy_neighbor_backoff_ms: Some(u64::MAX),
            ..SwarmAdaptiveConfig::default()
        });
        let mut scheduler = AdaptiveScheduler::new(policy);
        assert_eq!(
            scheduler.observe_latency_ms("agent-a", u64::MAX).backoff_ms,
            MAX_BACKOFF_MS
        );
        let defaults = runtime_config(&SwarmAdaptiveConfig {
            enabled: Some(true),
            noisy_neighbor_p99_ms: Some(0),
            noisy_neighbor_backoff_ms: Some(0),
            ..SwarmAdaptiveConfig::default()
        });
        assert_eq!(defaults.noisy_neighbor_p99_ms, 200);
        assert_eq!(defaults.noisy_neighbor_backoff_ms, 25);
        assert_eq!(defaults.max_backoff_ms, 100);
    }

    #[test]
    fn older_config_reads_cannot_reenable_either_adaptive_feature() {
        let runtime = DaemonAdaptiveScheduler::default();
        let prefetch = Mutex::new(CassPrefetchCoordinator::new());
        let old = configure(&runtime, &prefetch, "agent-a", enabled());
        let old_scheduler = old.scheduler.expect("enabled scheduler");
        let old_prefetch = old.prefetch.expect("enabled prefetch");
        let slow_read = runtime
            .begin_refresh(&prefetch)
            .expect("reserve older read");
        let disabled = configure(
            &runtime,
            &prefetch,
            "agent-a",
            SwarmAdaptiveConfig::default(),
        );
        assert!(disabled.scheduler.is_none());
        let stale = runtime.finish_refresh(
            &prefetch,
            slow_read,
            Path::new("/workspace/authorized"),
            "agent-a",
            &enabled(),
        );
        assert!(stale.scheduler.is_none());
        assert!(stale.prefetch.is_none());
        assert!(!runtime.ticket_is_current(&old_scheduler));
        assert!(
            !prefetch
                .lock()
                .unwrap()
                .policy_ticket_is_current(old_prefetch)
        );
        runtime.observe_execution(Some(&old_scheduler), Duration::from_secs(10));
        assert_eq!(runtime.state.lock().unwrap().scheduler.agent_count(), 0);
    }

    #[test]
    fn malformed_workspace_policy_revokes_both_features_without_observing() {
        let runtime = DaemonAdaptiveScheduler::default();
        let prefetch = Mutex::new(CassPrefetchCoordinator::new());
        let old = configure(&runtime, &prefetch, "agent-a", enabled());
        let scheduler = old.scheduler.unwrap();
        let prefetch_ticket = old.prefetch.unwrap();
        runtime.observe_execution(Some(&scheduler), Duration::from_millis(400));
        let workspace = tempfile::tempdir().unwrap();
        std::fs::create_dir(workspace.path().join(".ee")).unwrap();
        std::fs::write(
            workspace.path().join(".ee/config.toml"),
            "[swarm.adaptive]\nenabled = true\nsimilarity_threshold = 2.0\n",
        )
        .unwrap();
        let disabled = runtime.configure_workspace(&prefetch, workspace.path(), "agent-a");
        assert!(disabled.scheduler.is_none());
        assert!(disabled.prefetch.is_none());
        assert!(
            !prefetch
                .lock()
                .unwrap()
                .policy_ticket_is_current(prefetch_ticket)
        );
        runtime.observe_execution(Some(&scheduler), Duration::from_secs(10));
        let state = runtime.state.lock().unwrap();
        assert!(state.identities.is_empty());
        assert_eq!(state.scheduler.agent_count(), 0);
    }

    #[test]
    fn disable_reenable_preserves_one_pending_owner_and_rejects_old_samples() {
        let runtime = DaemonAdaptiveScheduler::default();
        let prefetch = Mutex::new(CassPrefetchCoordinator::new());
        let old = hot_ticket(&runtime, &prefetch, "agent-a");
        let pending = runtime
            .reserve_backoff(&old, 2)
            .unwrap()
            .expect("hot agent waits");
        configure(
            &runtime,
            &prefetch,
            "agent-a",
            SwarmAdaptiveConfig::default(),
        );
        let new = configure(&runtime, &prefetch, "agent-a", enabled())
            .scheduler
            .unwrap();
        runtime.observe_execution(Some(&old), Duration::from_secs(10));
        assert_eq!(runtime.state.lock().unwrap().scheduler.agent_count(), 0);
        assert!(matches!(
            runtime.reserve_backoff(&new, 2),
            Err(AdaptiveAdmissionError::Overloaded)
        ));
        drop(pending);
        runtime.observe_execution(Some(&new), Duration::from_millis(400));
        let next = runtime
            .reserve_backoff(&new, 2)
            .unwrap()
            .expect("released pending slot");
        assert_ne!(old.epoch, new.epoch);
        drop(next);
        assert!(runtime.state.lock().unwrap().pending.is_empty());
    }

    #[test]
    fn canonical_workspace_change_invalidates_previous_agent_samples() {
        let runtime = DaemonAdaptiveScheduler::default();
        let prefetch = Mutex::new(CassPrefetchCoordinator::new());
        let old = hot_ticket(&runtime, &prefetch, "agent-a");
        let new = runtime
            .refresh_policy(&prefetch, Path::new("/workspace/other"), "agent-a", enabled)
            .scheduler
            .unwrap();
        runtime.observe_execution(Some(&old), Duration::from_secs(10));
        runtime.observe_execution(Some(&new), Duration::from_millis(3));
        let state = runtime.state.lock().unwrap();
        let decision = state.scheduler.decision_for("agent-a");
        assert_eq!(decision.percentiles.sample_count, 1);
        assert_eq!(decision.percentiles.p99_ms, 3);
        assert_eq!(decision.outcome, AdaptiveSchedulerOutcome::Pass);
    }

    #[test]
    fn identity_eviction_is_bounded_protects_waiters_and_revokes_old_completions() {
        let runtime = DaemonAdaptiveScheduler::default();
        let prefetch = Mutex::new(CassPrefetchCoordinator::new());
        let protected = hot_ticket(&runtime, &prefetch, "agent-000");
        let pending = runtime.reserve_backoff(&protected, 1).unwrap().unwrap();
        let evicted = hot_ticket(&runtime, &prefetch, "agent-001");
        for index in 2..=MAX_TRACKED_AGENTS {
            let _ = hot_ticket(&runtime, &prefetch, &format!("agent-{index:03}"));
        }
        assert!(runtime.ticket_is_current(&protected));
        assert!(!runtime.ticket_is_current(&evicted));
        runtime.observe_execution(Some(&evicted), Duration::from_secs(10));
        {
            let state = runtime.state.lock().unwrap();
            assert_eq!(state.identities.len(), MAX_TRACKED_AGENTS);
            assert_eq!(state.scheduler.agent_count(), MAX_TRACKED_AGENTS);
            assert!(state.scheduler.state_for("agent-001").is_none());
        }
        let replacement = configure(&runtime, &prefetch, "agent-001", enabled())
            .scheduler
            .unwrap();
        assert_ne!(evicted.identity, replacement.identity);
        runtime.observe_execution(Some(&evicted), Duration::from_secs(10));
        assert!(
            runtime
                .state
                .lock()
                .unwrap()
                .scheduler
                .state_for("agent-001")
                .is_none()
        );
        assert!(
            configure(
                &runtime,
                &prefetch,
                &"a".repeat(MAX_AGENT_ID_BYTES + 1),
                enabled()
            )
            .scheduler
            .is_none()
        );
        drop(pending);
    }

    #[test]
    fn context_budget_covers_explicit_default_and_expired_deadlines() {
        assert_eq!(
            remaining_context_budget(Some(20), Duration::from_millis(7)),
            Some(Duration::from_millis(13))
        );
        assert_eq!(
            remaining_context_budget(None, Duration::from_secs(4)),
            Some(DAEMON_DEFAULT_RPC_TIMEOUT - Duration::from_secs(4))
        );
        assert_eq!(
            remaining_context_budget(Some(20), Duration::from_millis(20)),
            None
        );
        assert_eq!(remaining_context_budget(Some(0), Duration::ZERO), None);
        assert_eq!(
            remaining_context_budget(None, DAEMON_DEFAULT_RPC_TIMEOUT),
            None
        );
        assert!(remaining_context_budget(Some(u64::MAX), Duration::from_secs(1)).is_some());
    }

    #[test]
    fn injected_wait_obeys_delay_deadline_shutdown_and_policy_revocation() {
        let elapsed = Cell::new(Duration::ZERO);
        let applied = wait_for_backoff(
            Duration::from_millis(12),
            || elapsed.get(),
            || BackoffPoll::Continue(Duration::from_secs(1)),
            |step| {
                assert!(step <= BACKOFF_POLL_INTERVAL);
                elapsed.set(elapsed.get() + step);
            },
        )
        .unwrap();
        assert!(applied);
        assert_eq!(elapsed.get(), Duration::from_millis(12));

        for stop in [
            AdaptiveAdmissionError::Deadline,
            AdaptiveAdmissionError::Shutdown,
        ] {
            elapsed.set(Duration::ZERO);
            let outcome = wait_for_backoff(
                Duration::from_secs(1),
                || elapsed.get(),
                || {
                    if elapsed.get() >= Duration::from_millis(3) {
                        BackoffPoll::Stop(stop)
                    } else {
                        BackoffPoll::Continue(Duration::from_millis(3))
                    }
                },
                |step| elapsed.set(elapsed.get() + step),
            );
            assert_eq!(outcome, Err(stop));
            assert_eq!(elapsed.get(), Duration::from_millis(3));
        }

        elapsed.set(Duration::ZERO);
        assert!(
            wait_for_backoff(
                Duration::from_secs(1),
                || elapsed.get(),
                || {
                    if elapsed.get().is_zero() {
                        BackoffPoll::Continue(Duration::from_secs(1))
                    } else {
                        BackoffPoll::PolicyChanged
                    }
                },
                |step| elapsed.set(elapsed.get() + step)
            )
            .unwrap()
        );
        assert_eq!(elapsed.get(), BACKOFF_POLL_INTERVAL);
        assert!(
            !wait_for_backoff(
                Duration::from_secs(1),
                || Duration::ZERO,
                || BackoffPoll::PolicyChanged,
                |_| panic!("revoked policy must not wait")
            )
            .unwrap()
        );
    }

    #[test]
    fn backoff_releases_foreground_and_retains_bounded_resident_ownership() {
        let runtime = DaemonAdaptiveScheduler::default();
        let prefetch = Mutex::new(CassPrefetchCoordinator::new());
        let ticket = hot_ticket(&runtime, &prefetch, "agent-a");
        let pool = InflightPool::new(1);
        let mut first = pool.try_acquire().unwrap();
        assert!(
            runtime
                .admit_with_wait(Some(&ticket), Some(&mut first), |_| {
                    assert!(pool.foreground_is_idle());
                    assert!(!pool.wait_until_drained(Duration::ZERO));
                    let mut other = pool
                        .try_acquire()
                        .expect("another agent gets foreground capacity");
                    assert!(pool.try_acquire().is_none());
                    assert!(!other.suspend(), "at most N residents may be suspended");
                    assert!(matches!(
                        runtime.reserve_backoff(&ticket, 1),
                        Err(AdaptiveAdmissionError::Overloaded)
                    ));
                    let counts = pool.inflight.lock().unwrap();
                    assert_eq!((counts.active, counts.resident), (1, 2));
                    drop(counts);
                    drop(other);
                    Ok(true)
                })
                .unwrap()
        );
        assert!(!pool.foreground_is_idle());
        assert_eq!(pool.inflight.lock().unwrap().resident, 1);
        drop(first);
        assert!(pool.wait_until_drained(Duration::ZERO));
    }

    #[test]
    fn a_busy_resume_refuses_once_and_timeout_or_unwind_releases_residents() {
        let runtime = DaemonAdaptiveScheduler::default();
        let prefetch = Mutex::new(CassPrefetchCoordinator::new());
        let ticket = hot_ticket(&runtime, &prefetch, "agent-a");
        let pool = InflightPool::new(1);
        let mut first = pool.try_acquire().unwrap();
        let mut other = None;
        let result = runtime.admit_with_wait(Some(&ticket), Some(&mut first), |_| {
            other = pool.try_acquire();
            assert!(other.is_some());
            Ok(true)
        });
        assert_eq!(result, Err(AdaptiveAdmissionError::Overloaded));
        assert!(!first.active);
        drop(first);
        drop(other);
        assert!(pool.wait_until_drained(Duration::ZERO));
        assert!(runtime.state.lock().unwrap().pending.is_empty());

        for refusal in [
            AdaptiveAdmissionError::Deadline,
            AdaptiveAdmissionError::Shutdown,
        ] {
            let mut permit = pool.try_acquire().unwrap();
            assert_eq!(
                runtime.admit_with_wait(Some(&ticket), Some(&mut permit), |_| Err(refusal)),
                Err(refusal)
            );
            assert!(!permit.active);
            assert!(!pool.wait_until_drained(Duration::ZERO));
            drop(permit);
            assert!(pool.wait_until_drained(Duration::ZERO));
        }
        let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut permit = pool.try_acquire().unwrap();
            let _ = runtime.admit_with_wait(Some(&ticket), Some(&mut permit), |_| {
                panic!("injected waiting worker panic")
            });
        }));
        assert!(panicked.is_err());
        assert!(pool.wait_until_drained(Duration::ZERO));
        assert!(runtime.state.lock().unwrap().pending.is_empty());
    }

    #[cfg(feature = "lexical-bm25")]
    #[test]
    fn context_dispatch_applies_backoff_only_after_authority_and_preserves_pack_identity() {
        use super::super::{
            DAEMON_CONTEXT_PARAMS_INVALID_CODE, DAEMON_METHOD_UNAUTHORIZED_CODE,
            DaemonDispatchPolicy, METHOD_CONTEXT, dispatch_context_with_admission,
            dispatch_with_echo_policy_and_workspace_inner,
        };
        use crate::core::cass_prefetch::ADAPTIVE_BACKOFF_APPLIED_CODE;
        use crate::core::index::{IndexRebuildOptions, IndexRebuildStatus};
        use crate::daemon::protocol::DaemonRequest;

        let directory = tempfile::tempdir().unwrap();
        let workspace = directory.path().canonicalize().unwrap();
        let init = crate::core::init::init_workspace(&crate::core::init::InitOptions {
            workspace_path: workspace.clone(),
            dry_run: false,
            repair_plan: false,
            force: false,
            allow_symlink: false,
            skip_boilerplate: true,
        });
        assert!(
            !matches!(init.status, crate::core::init::InitStatus::Failed),
            "{:?}",
            init.action_errors
        );
        let connection = crate::db::DbConnection::open_file(&init.database_path).unwrap();
        let workspace_id = connection
            .get_workspace_by_path(&workspace.to_string_lossy())
            .unwrap()
            .unwrap()
            .id;
        let _embedder = crate::core::index::install_test_hash_workspace_embedder(&workspace_id);
        const MEMORY_ID: &str = "mem_00000000000000000000005901";
        connection
            .insert_memory_with_timestamps(
                MEMORY_ID,
                &crate::db::CreateMemoryInput {
                    workspace_id,
                    level: "procedural".to_owned(),
                    kind: "rule".to_owned(),
                    content: "Check quasar release checksums before publication.".to_owned(),
                    workflow_id: None,
                    confidence: 0.9,
                    utility: 0.8,
                    importance: 0.7,
                    provenance_uri: Some("test://adaptive-context".to_owned()),
                    trust_class: "agent_validated".to_owned(),
                    trust_subclass: None,
                    tags: Vec::new(),
                    valid_from: None,
                    valid_to: None,
                },
                // Pin the existing recency floor instead of letting elapsed
                // test time perturb the canonical score/hash comparison.
                "2020-01-01T00:00:00+00:00",
                "2020-01-01T00:00:00+00:00",
                MEMORY_ID,
            )
            .unwrap();
        connection.close().unwrap();
        let rebuilt = crate::core::index::rebuild_index(&IndexRebuildOptions {
            workspace_path: workspace.clone(),
            database_path: Some(init.database_path.clone()),
            index_dir: None,
            dry_run: false,
        })
        .unwrap();
        assert_eq!(rebuilt.status, IndexRebuildStatus::Success);

        let policy = DaemonDispatchPolicy::for_workspace(workspace.display().to_string());
        let runtime = &policy.adaptive_scheduler;
        let prefetch = policy.cass_prefetch();
        let shutdown = AtomicBool::new(false);
        let pool = InflightPool::new(1);
        let mut request = DaemonRequest::new(
            "req-adaptive-context",
            "agent-context",
            METHOD_CONTEXT,
            serde_json::json!({
                "task": "quasar release checksums", "workspacePath": workspace,
                "databasePath": init.database_path, "sourceMode": "lexical_only", "speed": "instant",
                "readOnly": true, "maxTokens": 800, "noRenderedText": false,
            }),
        );
        request.workspace_id = Some(workspace.display().to_string());

        // The real dispatch gate rejects caller-controlled identity/path input
        // before it can load policy, create history, or reserve a waiting slot.
        let other = tempfile::tempdir().unwrap();
        let mut unauthorized = request.clone();
        unauthorized.workspace_id = Some(other.path().display().to_string());
        let refused = dispatch_with_echo_policy_and_workspace_inner(
            &unauthorized,
            false,
            &policy,
            &shutdown,
            false,
            None,
        );
        assert_eq!(refused.error.unwrap().code, DAEMON_METHOD_UNAUTHORIZED_CODE);
        let mut invalid_path = request.clone();
        invalid_path.params["workspacePath"] = serde_json::json!(other.path());
        let refused = dispatch_with_echo_policy_and_workspace_inner(
            &invalid_path,
            false,
            &policy,
            &shutdown,
            false,
            None,
        );
        assert_eq!(
            refused.error.unwrap().code,
            DAEMON_CONTEXT_PARAMS_INVALID_CODE
        );
        assert!(runtime.state.lock().unwrap().identities.is_empty());
        assert_eq!(runtime.state.lock().unwrap().scheduler.agent_count(), 0);

        let mut baseline = None;
        for hot in [false, true] {
            if hot {
                let ticket = runtime
                    .refresh_policy(prefetch, &workspace, &request.agent_id, enabled)
                    .scheduler
                    .unwrap();
                runtime.observe_execution(Some(&ticket), Duration::from_secs(60));
            }
            let before = runtime
                .state
                .lock()
                .unwrap()
                .scheduler
                .decision_for(&request.agent_id)
                .percentiles
                .sample_count;
            let waited = Cell::new(Duration::ZERO);
            let mut permit = pool.try_acquire().unwrap();
            let response = dispatch_context_with_admission(
                &request,
                &shutdown,
                &policy,
                false,
                Some(&mut permit),
                |authorized_workspace, _, _, permit| {
                    let tickets = runtime.refresh_policy(
                        prefetch,
                        authorized_workspace,
                        &request.agent_id,
                        enabled,
                    );
                    let applied =
                        runtime.admit_with_wait(tickets.scheduler.as_ref(), permit, |pending| {
                            assert!(hot, "the first request has no observed latency");
                            assert!(pool.foreground_is_idle());
                            wait_for_backoff(
                                pending.delay,
                                || waited.get(),
                                || BackoffPoll::Continue(Duration::from_secs(1)),
                                |step| waited.set(waited.get() + step),
                            )
                        })?;
                    // Waiting itself must not enter the execution latency window.
                    assert_eq!(
                        runtime
                            .state
                            .lock()
                            .unwrap()
                            .scheduler
                            .decision_for(&request.agent_id)
                            .percentiles
                            .sample_count,
                        before
                    );
                    Ok((tickets, applied))
                },
            );
            assert!(response.error.is_none(), "{response:?}");
            assert_eq!(
                response
                    .degraded_codes
                    .iter()
                    .any(|code| code == ADAPTIVE_BACKOFF_APPLIED_CODE),
                hot
            );
            assert_eq!(!waited.get().is_zero(), hot);
            let result = response.result.unwrap();
            assert_eq!(result["success"], true);
            assert!(
                result.to_string().contains(MEMORY_ID),
                "the real canonical pack must retrieve the seeded memory"
            );
            assert!(!result.to_string().contains(ADAPTIVE_BACKOFF_APPLIED_CODE));
            assert_eq!(
                runtime
                    .state
                    .lock()
                    .unwrap()
                    .scheduler
                    .decision_for(&request.agent_id)
                    .percentiles
                    .sample_count,
                before + 1
            );
            if let Some(cold) = baseline.as_ref() {
                let cold: &serde_json::Value = cold;
                for pointer in ["/data/pack/text", "/data/pack/hash"] {
                    let expected = cold
                        .pointer(pointer)
                        .and_then(serde_json::Value::as_str)
                        .expect("real canonical field");
                    assert!(!expected.is_empty());
                    assert_eq!(
                        result.pointer(pointer).and_then(serde_json::Value::as_str),
                        Some(expected),
                        "adaptive admission must preserve {pointer}"
                    );
                }
            } else {
                baseline = Some(result);
            }
            drop(permit);
            assert!(pool.wait_until_drained(Duration::ZERO));
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn another_framed_request_completes_while_hot_agent_waits() {
        use super::super::{DaemonDispatchPolicy, METHOD_CAPABILITIES, handle_connection};
        use crate::daemon::protocol::{DaemonRequest, DaemonResponse};
        use std::io::{Read, Write};
        use std::os::unix::net::UnixStream;
        use std::sync::Arc;

        let runtime = DaemonAdaptiveScheduler::default();
        let prefetch = Mutex::new(CassPrefetchCoordinator::new());
        let ticket = hot_ticket(&runtime, &prefetch, "agent-hot");
        let pool = InflightPool::new(1);
        let mut hot = pool.try_acquire().unwrap();
        let result = runtime.admit_with_wait(Some(&ticket), Some(&mut hot), |_| {
            let mut other = pool.try_acquire().expect("released foreground slot");
            let (mut client, server) = UnixStream::pair().unwrap();
            client
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let request = DaemonRequest::new(
                "req-cool",
                "agent-cool",
                METHOD_CAPABILITIES,
                serde_json::json!({}),
            );
            let body = serde_json::to_vec(&request).unwrap();
            client
                .write_all(&u32::try_from(body.len()).unwrap().to_be_bytes())
                .unwrap();
            client.write_all(&body).unwrap();
            handle_connection(
                server,
                Arc::new(AtomicBool::new(false)),
                Arc::new(DaemonDispatchPolicy::default()),
                Arc::new(crate::daemon::metrics::NoopMetricsCollector),
                Some(&mut other),
            );
            let mut header = [0; 4];
            client.read_exact(&mut header).unwrap();
            let mut body = vec![0; u32::from_be_bytes(header) as usize];
            client.read_exact(&mut body).unwrap();
            let response: DaemonResponse = serde_json::from_slice(&body).unwrap();
            assert_eq!(response.request_id, "req-cool");
            assert!(response.error.is_none());
            assert!(
                !response
                    .degraded_codes
                    .iter()
                    .any(|code| code == crate::core::cass_prefetch::ADAPTIVE_BACKOFF_APPLIED_CODE)
            );
            drop(other);
            Ok(true)
        });
        assert_eq!(result, Ok(true));
        drop(hot);
        assert!(pool.wait_until_drained(Duration::ZERO));
    }
}
