//! Deterministic core for speculative context-pack pre-assembly.
//!
//! This module is intentionally integration-light: the CLI/steward wiring can
//! call into these pure pieces once the L2 pack-cache producer path is clear.

use std::collections::VecDeque;

use serde::Serialize;

pub const SPEC_PACK_SCHEMA_V1: &str = "ee.spec_pack.v1";
pub const DEFAULT_SPEC_PACK_TTL_SECONDS: u64 = 30;
pub const DEFAULT_SPEC_PACK_CONCURRENCY: usize = 4;
pub const DEFAULT_SPEC_PACK_RING_CAPACITY: usize = 64;
pub const DEFAULT_SPEC_PACK_RING_TOKEN_CAPACITY: u64 = 64 * 4_000;
pub const DEFAULT_SPEC_PACK_TOP_K: usize = 4;

pub const SPEC_PACK_RING_EMPTY_CODE: &str = "ring_empty";
pub const SPEC_PACK_QOS_BACK_PRESSURE_CODE: &str = "qos_back_pressure";
pub const SPEC_PACK_CACHE_ADMISSION_DENIED_CODE: &str = "cache_admission_denied";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SpecPackConfig {
    pub ttl_seconds: u64,
    pub per_workspace_concurrency: usize,
    pub ring_capacity: usize,
    pub ring_token_capacity: u64,
    pub top_k: usize,
}

impl Default for SpecPackConfig {
    fn default() -> Self {
        Self {
            ttl_seconds: DEFAULT_SPEC_PACK_TTL_SECONDS,
            per_workspace_concurrency: DEFAULT_SPEC_PACK_CONCURRENCY,
            ring_capacity: DEFAULT_SPEC_PACK_RING_CAPACITY,
            ring_token_capacity: DEFAULT_SPEC_PACK_RING_TOKEN_CAPACITY,
            top_k: DEFAULT_SPEC_PACK_TOP_K,
        }
    }
}

impl SpecPackConfig {
    #[must_use]
    pub const fn new(
        ttl_seconds: u64,
        per_workspace_concurrency: usize,
        ring_capacity: usize,
        ring_token_capacity: u64,
        top_k: usize,
    ) -> Self {
        Self {
            ttl_seconds,
            per_workspace_concurrency,
            ring_capacity,
            ring_token_capacity,
            top_k,
        }
    }

    #[must_use]
    pub fn effective_ttl_seconds(&self) -> u64 {
        self.ttl_seconds.min(3_600)
    }

    #[must_use]
    pub fn effective_concurrency(&self) -> usize {
        self.per_workspace_concurrency.max(1)
    }

    #[must_use]
    pub fn effective_ring_capacity(&self) -> usize {
        self.ring_capacity.max(1)
    }

    #[must_use]
    pub fn effective_top_k(&self) -> usize {
        self.top_k.max(1)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecentQueryShape {
    pub workspace_id: String,
    pub query: String,
    pub bead_id: Option<String>,
    pub max_tokens: u32,
    pub profile: String,
    pub agent_name_when_bias_active: Option<String>,
}

impl RecentQueryShape {
    #[must_use]
    pub fn new(
        workspace_id: impl Into<String>,
        query: impl Into<String>,
        bead_id: Option<impl Into<String>>,
        max_tokens: u32,
        profile: impl Into<String>,
        agent_name_when_bias_active: Option<impl Into<String>>,
    ) -> Self {
        Self {
            workspace_id: workspace_id.into(),
            query: query.into(),
            bead_id: bead_id.map(Into::into),
            max_tokens,
            profile: profile.into(),
            agent_name_when_bias_active: agent_name_when_bias_active.map(Into::into),
        }
    }

    #[must_use]
    pub fn workspace_id_hash(&self) -> String {
        blake3_hex_hash([("workspace_id", self.workspace_id.as_str())])
    }

    #[must_use]
    pub fn query_shape_hash(&self) -> String {
        let max_tokens = self.max_tokens.to_string();
        blake3_hex_hash([
            ("workspace_id", self.workspace_id.as_str()),
            ("query", self.query.as_str()),
            ("bead_id", self.bead_id.as_deref().unwrap_or("")),
            ("max_tokens", max_tokens.as_str()),
            ("profile", self.profile.as_str()),
            (
                "agent_name_when_bias_active",
                self.agent_name_when_bias_active.as_deref().unwrap_or(""),
            ),
        ])
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecentQueryObservation {
    pub shape: RecentQueryShape,
    pub tokens_used: u64,
    pub observed_at_ms: u64,
}

impl RecentQueryObservation {
    #[must_use]
    pub const fn new(shape: RecentQueryShape, tokens_used: u64, observed_at_ms: u64) -> Self {
        Self {
            shape,
            tokens_used,
            observed_at_ms,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecentQueryRing {
    max_entries: usize,
    max_tokens: u64,
    total_tokens: u64,
    entries: VecDeque<RecentQueryObservation>,
}

impl RecentQueryRing {
    #[must_use]
    pub fn new(max_entries: usize, max_tokens: u64) -> Self {
        Self {
            max_entries: max_entries.max(1),
            max_tokens,
            total_tokens: 0,
            entries: VecDeque::new(),
        }
    }

    pub fn record(&mut self, observation: RecentQueryObservation) {
        let query_hash = observation.shape.query_shape_hash();
        if let Some(index) = self
            .entries
            .iter()
            .position(|entry| entry.shape.query_shape_hash() == query_hash)
            && let Some(previous) = self.entries.remove(index)
        {
            self.total_tokens = self.total_tokens.saturating_sub(previous.tokens_used);
        }

        self.total_tokens = self.total_tokens.saturating_add(observation.tokens_used);
        self.entries.push_back(observation);
        self.evict_to_limits();
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[must_use]
    pub const fn total_tokens(&self) -> u64 {
        self.total_tokens
    }

    #[must_use]
    pub fn entries_newest_first(&self) -> Vec<RecentQueryObservation> {
        self.entries.iter().rev().cloned().collect()
    }

    fn evict_to_limits(&mut self) {
        while self.entries.len() > self.max_entries {
            self.pop_oldest();
        }
        while self.entries.len() > 1 && self.total_tokens > self.max_tokens {
            self.pop_oldest();
        }
    }

    fn pop_oldest(&mut self) {
        if let Some(removed) = self.entries.pop_front() {
            self.total_tokens = self.total_tokens.saturating_sub(removed.tokens_used);
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SpecPackCacheFreshness {
    Fresh,
    Missing,
    Stale,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SpecPackCandidate {
    pub observation: RecentQueryObservation,
    pub cache_freshness: SpecPackCacheFreshness,
}

impl SpecPackCandidate {
    #[must_use]
    pub const fn new(
        observation: RecentQueryObservation,
        cache_freshness: SpecPackCacheFreshness,
    ) -> Self {
        Self {
            observation,
            cache_freshness,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SpecPackSelectedCandidate {
    pub workspace_id_hash: String,
    pub query_shape_hash: String,
    pub observed_at_ms: u64,
    pub tokens_used: u64,
    pub cache_freshness: SpecPackCacheFreshness,
}

#[must_use]
pub fn select_spec_pack_candidates(
    candidates: impl IntoIterator<Item = SpecPackCandidate>,
    top_k: usize,
) -> Vec<SpecPackSelectedCandidate> {
    let mut selected = candidates
        .into_iter()
        .filter(|candidate| candidate.cache_freshness != SpecPackCacheFreshness::Fresh)
        .map(|candidate| {
            let query_shape_hash = candidate.observation.shape.query_shape_hash();
            SpecPackSelectedCandidate {
                workspace_id_hash: candidate.observation.shape.workspace_id_hash(),
                query_shape_hash,
                observed_at_ms: candidate.observation.observed_at_ms,
                tokens_used: candidate.observation.tokens_used,
                cache_freshness: candidate.cache_freshness,
            }
        })
        .collect::<Vec<_>>();
    selected.sort_by(|left, right| {
        right
            .observed_at_ms
            .cmp(&left.observed_at_ms)
            .then_with(|| right.tokens_used.cmp(&left.tokens_used))
            .then_with(|| left.query_shape_hash.cmp(&right.query_shape_hash))
    });
    selected.truncate(top_k.max(1));
    selected
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SpecPackAdmissionVerdict {
    Admitted,
    DeniedForegroundRequestIdActive,
    DeniedReadPoolForegroundPinHeld,
    DeniedQosForegroundPressure,
    DeniedPerWorkspaceConcurrencyCap,
}

impl SpecPackAdmissionVerdict {
    #[must_use]
    pub const fn degraded_code(self) -> Option<&'static str> {
        match self {
            Self::Admitted => None,
            Self::DeniedForegroundRequestIdActive
            | Self::DeniedReadPoolForegroundPinHeld
            | Self::DeniedQosForegroundPressure => Some(SPEC_PACK_QOS_BACK_PRESSURE_CODE),
            Self::DeniedPerWorkspaceConcurrencyCap => Some(SPEC_PACK_CACHE_ADMISSION_DENIED_CODE),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SpecPackQosSnapshot {
    pub foreground_request_id_active: bool,
    pub read_pool_foreground_pin_held: bool,
    pub qos_foreground_active: bool,
    pub active_speculations_for_workspace: usize,
}

impl SpecPackQosSnapshot {
    #[must_use]
    pub const fn idle() -> Self {
        Self {
            foreground_request_id_active: false,
            read_pool_foreground_pin_held: false,
            qos_foreground_active: false,
            active_speculations_for_workspace: 0,
        }
    }

    #[must_use]
    pub fn snapshot_hash(&self) -> String {
        let active = self.active_speculations_for_workspace.to_string();
        blake3_hex_hash([
            (
                "foreground_request_id_active",
                bool_str(self.foreground_request_id_active),
            ),
            (
                "read_pool_foreground_pin_held",
                bool_str(self.read_pool_foreground_pin_held),
            ),
            (
                "qos_foreground_active",
                bool_str(self.qos_foreground_active),
            ),
            ("active_speculations_for_workspace", active.as_str()),
        ])
    }
}

#[must_use]
pub fn admit_spec_pack_candidate(
    config: &SpecPackConfig,
    qos: SpecPackQosSnapshot,
) -> SpecPackAdmissionVerdict {
    if qos.foreground_request_id_active {
        return SpecPackAdmissionVerdict::DeniedForegroundRequestIdActive;
    }
    if qos.read_pool_foreground_pin_held {
        return SpecPackAdmissionVerdict::DeniedReadPoolForegroundPinHeld;
    }
    if qos.qos_foreground_active {
        return SpecPackAdmissionVerdict::DeniedQosForegroundPressure;
    }
    if qos.active_speculations_for_workspace >= config.effective_concurrency() {
        return SpecPackAdmissionVerdict::DeniedPerWorkspaceConcurrencyCap;
    }
    SpecPackAdmissionVerdict::Admitted
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SpecPackPhase {
    Admission,
    Prepare,
    Run,
    Abort,
    Store,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SpecPackAbortReason {
    ForegroundRequestArrived,
    Shutdown,
    MemoryPressure,
    TtlExpiredBeforeStore,
    SelectionNoLongerTopK,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SpecPackTelemetryEvent {
    pub schema: &'static str,
    pub side_effect_free: bool,
    pub phase: SpecPackPhase,
    pub ts: String,
    pub workspace_id_hash: String,
    pub query_shape_hash: String,
    pub qos_lane_snapshot_hash: String,
    pub admission_verdict: SpecPackAdmissionVerdict,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub surface_pack_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ttl_seconds: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tokens_used: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub elapsed_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub abort_reason: Option<SpecPackAbortReason>,
    pub degraded_codes: Vec<&'static str>,
}

impl SpecPackTelemetryEvent {
    #[must_use]
    pub fn admission(
        ts: impl Into<String>,
        shape: &RecentQueryShape,
        qos: SpecPackQosSnapshot,
        verdict: SpecPackAdmissionVerdict,
    ) -> Self {
        Self::new(ts, SpecPackPhase::Admission, shape, qos, verdict)
    }

    #[must_use]
    pub fn for_phase(
        ts: impl Into<String>,
        phase: SpecPackPhase,
        shape: &RecentQueryShape,
        qos: SpecPackQosSnapshot,
        verdict: SpecPackAdmissionVerdict,
    ) -> Self {
        Self::new(ts, phase, shape, qos, verdict)
    }

    #[must_use]
    pub fn with_surface_pack_id(mut self, surface_pack_id: impl Into<String>) -> Self {
        self.surface_pack_id = Some(surface_pack_id.into());
        self
    }

    #[must_use]
    pub fn with_ttl_seconds(mut self, ttl_seconds: u64) -> Self {
        self.ttl_seconds = Some(ttl_seconds.min(3_600));
        self
    }

    #[must_use]
    pub const fn with_tokens_used(mut self, tokens_used: u64) -> Self {
        self.tokens_used = Some(tokens_used);
        self
    }

    #[must_use]
    pub const fn with_elapsed_ms(mut self, elapsed_ms: u64) -> Self {
        self.elapsed_ms = Some(elapsed_ms);
        self
    }

    #[must_use]
    pub const fn with_abort_reason(mut self, abort_reason: SpecPackAbortReason) -> Self {
        self.abort_reason = Some(abort_reason);
        self
    }

    fn new(
        ts: impl Into<String>,
        phase: SpecPackPhase,
        shape: &RecentQueryShape,
        qos: SpecPackQosSnapshot,
        verdict: SpecPackAdmissionVerdict,
    ) -> Self {
        let degraded_codes = verdict.degraded_code().into_iter().collect();
        Self {
            schema: SPEC_PACK_SCHEMA_V1,
            side_effect_free: true,
            phase,
            ts: ts.into(),
            workspace_id_hash: shape.workspace_id_hash(),
            query_shape_hash: shape.query_shape_hash(),
            qos_lane_snapshot_hash: qos.snapshot_hash(),
            admission_verdict: verdict,
            surface_pack_id: None,
            ttl_seconds: None,
            tokens_used: None,
            elapsed_ms: None,
            abort_reason: None,
            degraded_codes,
        }
    }
}

fn bool_str(value: bool) -> &'static str {
    if value { "true" } else { "false" }
}

fn blake3_hex_hash<'a>(fields: impl IntoIterator<Item = (&'a str, &'a str)>) -> String {
    let mut hasher = blake3::Hasher::new();
    for (label, value) in fields {
        hash_field(&mut hasher, label, value);
    }
    format!("blake3:{}", hasher.finalize().to_hex())
}

fn hash_field(hasher: &mut blake3::Hasher, label: &str, value: &str) {
    hasher.update(label.as_bytes());
    hasher.update(&[0]);
    let value_len = u64::try_from(value.len()).unwrap_or(u64::MAX);
    hasher.update(&value_len.to_be_bytes());
    hasher.update(value.as_bytes());
    hasher.update(&[0xff]);
}
