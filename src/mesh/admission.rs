//! SRR6.41 peer admission control, rate limits, and resource isolation.
//!
//! The mesh transport and supervisor hand this module an already-authenticated
//! peer request plus local accounting. The module returns a deterministic
//! allow/reject/throttle decision without touching the network, database, or
//! scheduler. Local Tier-1 memory work is treated as reserved capacity: noisy
//! peers are throttled before their work can consume local-only budgets.

use std::collections::BTreeMap;

use serde::Serialize;

pub const MESH_ADMISSION_SCHEMA_V1: &str = "ee.mesh.admission.v1";
pub const MESH_ADMISSION_STATUS_SCHEMA_V1: &str = "ee.mesh.admission_status.v1";
pub const MESH_ADMISSION_DOCTOR_SCHEMA_V1: &str = "ee.mesh.admission_doctor.v1";
pub const MESH_ADMISSION_E2E_SURFACE: &str = "mesh_admission_control";
pub use crate::models::TEST_EVENT_SCHEMA_V1;

pub mod degraded_codes {
    pub const PEER_THROTTLED: &str = "mesh_peer_throttled";
    pub const PAYLOAD_REJECTED: &str = "mesh_payload_rejected";
    pub const BUDGET_EXHAUSTED: &str = "mesh_peer_budget_exhausted";
    pub const BACKOFF_ACTIVE: &str = "mesh_peer_backoff_active";
    pub const LOCAL_TIER1_UNAFFECTED: &str = "mesh_local_tier1_unaffected";
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MeshAdmissionRequestKind {
    Hello,
    TipAdvertise,
    RangeRequest,
    EventBatch,
    BodyFetch,
    IndexJobs,
}

impl MeshAdmissionRequestKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Hello => "hello",
            Self::TipAdvertise => "tip_advertise",
            Self::RangeRequest => "range_request",
            Self::EventBatch => "event_batch",
            Self::BodyFetch => "body_fetch",
            Self::IndexJobs => "index_jobs",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MeshAdmissionAction {
    Allow,
    Reject,
    Throttle,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Ord, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MeshAdmissionReason {
    WithinBudget,
    PeerThrottled,
    PayloadRejected,
    BudgetExhausted,
    BackoffActive,
}

impl MeshAdmissionReason {
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::WithinBudget => "mesh_admission_allowed",
            Self::PeerThrottled => degraded_codes::PEER_THROTTLED,
            Self::PayloadRejected => degraded_codes::PAYLOAD_REJECTED,
            Self::BudgetExhausted => degraded_codes::BUDGET_EXHAUSTED,
            Self::BackoffActive => degraded_codes::BACKOFF_ACTIVE,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MeshAdmissionLimits {
    pub max_concurrent_requests_per_peer: u32,
    pub max_event_batch_count: u32,
    pub max_event_batch_bytes: u64,
    pub max_body_fetch_bytes: u64,
    pub max_index_jobs_per_round: u32,
    pub max_malformed_frames_before_backoff: u32,
    pub max_policy_denials_before_backoff: u32,
    pub initial_backoff_ms: u64,
    pub max_backoff_ms: u64,
}

impl MeshAdmissionLimits {
    #[must_use]
    pub const fn conservative_default() -> Self {
        Self {
            max_concurrent_requests_per_peer: 4,
            max_event_batch_count: 512,
            max_event_batch_bytes: 4 * 1024 * 1024,
            max_body_fetch_bytes: 512 * 1024,
            max_index_jobs_per_round: 16,
            max_malformed_frames_before_backoff: 3,
            max_policy_denials_before_backoff: 5,
            initial_backoff_ms: 1_000,
            max_backoff_ms: 60_000,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MeshPeerAdmissionState {
    pub peer_id: String,
    pub in_flight_requests: u32,
    pub queued_index_jobs: u32,
    pub malformed_frame_count: u32,
    pub policy_denial_count: u32,
    pub backoff_until_epoch_ms: Option<u64>,
    pub local_tier1_reserved: bool,
}

impl MeshPeerAdmissionState {
    #[must_use]
    pub fn new(peer_id: impl Into<String>) -> Self {
        Self {
            peer_id: peer_id.into(),
            in_flight_requests: 0,
            queued_index_jobs: 0,
            malformed_frame_count: 0,
            policy_denial_count: 0,
            backoff_until_epoch_ms: None,
            local_tier1_reserved: true,
        }
    }

    #[must_use]
    pub fn with_in_flight_requests(mut self, in_flight_requests: u32) -> Self {
        self.in_flight_requests = in_flight_requests;
        self
    }

    #[must_use]
    pub fn with_queued_index_jobs(mut self, queued_index_jobs: u32) -> Self {
        self.queued_index_jobs = queued_index_jobs;
        self
    }

    #[must_use]
    pub fn with_malformed_frame_count(mut self, malformed_frame_count: u32) -> Self {
        self.malformed_frame_count = malformed_frame_count;
        self
    }

    #[must_use]
    pub fn with_policy_denial_count(mut self, policy_denial_count: u32) -> Self {
        self.policy_denial_count = policy_denial_count;
        self
    }

    #[must_use]
    pub fn with_backoff_until(mut self, backoff_until_epoch_ms: u64) -> Self {
        self.backoff_until_epoch_ms = Some(backoff_until_epoch_ms);
        self
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MeshAdmissionRequest {
    pub peer_id: String,
    pub kind: MeshAdmissionRequestKind,
    pub payload_bytes: u64,
    pub event_count: u32,
    pub body_fetch_bytes: u64,
    pub requested_index_jobs: u32,
    pub now_epoch_ms: u64,
}

impl MeshAdmissionRequest {
    #[must_use]
    pub fn new(
        peer_id: impl Into<String>,
        kind: MeshAdmissionRequestKind,
        now_epoch_ms: u64,
    ) -> Self {
        Self {
            peer_id: peer_id.into(),
            kind,
            payload_bytes: 0,
            event_count: 0,
            body_fetch_bytes: 0,
            requested_index_jobs: 0,
            now_epoch_ms,
        }
    }

    #[must_use]
    pub fn with_payload(mut self, payload_bytes: u64) -> Self {
        self.payload_bytes = payload_bytes;
        self
    }

    #[must_use]
    pub fn with_event_count(mut self, event_count: u32) -> Self {
        self.event_count = event_count;
        self
    }

    #[must_use]
    pub fn with_body_fetch_bytes(mut self, body_fetch_bytes: u64) -> Self {
        self.body_fetch_bytes = body_fetch_bytes;
        self
    }

    #[must_use]
    pub fn with_requested_index_jobs(mut self, requested_index_jobs: u32) -> Self {
        self.requested_index_jobs = requested_index_jobs;
        self
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeshAdmissionDecision {
    pub schema: &'static str,
    pub peer_id: String,
    pub request_kind: MeshAdmissionRequestKind,
    pub action: MeshAdmissionAction,
    pub reason: MeshAdmissionReason,
    pub code: &'static str,
    pub payload_bytes: u64,
    pub event_count: u32,
    pub body_fetch_bytes: u64,
    pub requested_index_jobs: u32,
    pub in_flight_requests: u32,
    pub queued_index_jobs: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backoff_until_epoch_ms: Option<u64>,
    pub local_tier1_unaffected: bool,
}

impl MeshAdmissionDecision {
    #[must_use]
    pub const fn allowed(&self) -> bool {
        matches!(self.action, MeshAdmissionAction::Allow)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeshAdmissionStatus {
    pub schema: &'static str,
    pub peer_count: usize,
    pub throttled_peer_count: usize,
    pub rejected_peer_count: usize,
    pub budget_exhausted_peer_count: usize,
    pub local_tier1_unaffected: bool,
    pub degraded: Vec<&'static str>,
    pub per_peer: Vec<MeshAdmissionPeerStatus>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeshAdmissionPeerStatus {
    pub peer_alias: String,
    pub action: MeshAdmissionAction,
    pub reason: MeshAdmissionReason,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backoff_until_epoch_ms: Option<u64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MeshAdmissionDoctorPosture {
    Ok,
    DegradedRecoverable,
}

impl MeshAdmissionDoctorPosture {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::DegradedRecoverable => "degraded_recoverable",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeshAdmissionDoctorSignal {
    pub code: &'static str,
    pub severity: &'static str,
    pub peer_count: usize,
    pub message: &'static str,
    pub repair: &'static str,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeshAdmissionDoctorReport {
    pub schema: &'static str,
    pub posture: MeshAdmissionDoctorPosture,
    pub peer_count: usize,
    pub local_tier1_unaffected: bool,
    pub signals: Vec<MeshAdmissionDoctorSignal>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MeshAdmissionScenario {
    PeerThrottled,
    PayloadRejected,
    BudgetExhausted,
    BackoffUntil,
    LocalTier1Unaffected,
}

impl MeshAdmissionScenario {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PeerThrottled => "peer_throttled",
            Self::PayloadRejected => "payload_rejected",
            Self::BudgetExhausted => "budget_exhausted",
            Self::BackoffUntil => "backoff_until",
            Self::LocalTier1Unaffected => "local_tier1_unaffected",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeshAdmissionTestEvent {
    pub schema: &'static str,
    pub surface: &'static str,
    pub scenario: MeshAdmissionScenario,
    pub phase: &'static str,
    pub peer_id: String,
    pub request_kind: MeshAdmissionRequestKind,
    pub action: MeshAdmissionAction,
    pub code: &'static str,
    pub backoff_until_epoch_ms: Option<u64>,
    pub local_tier1_unaffected: bool,
}

/// Authenticated-session wrapper used by the production responder. Pre-auth
/// concurrency still lives in the broker; this applies the post-handshake
/// payload/index caps from [`MeshAdmissionLimits::conservative_default`].
#[must_use]
pub fn admit_authenticated_mesh_capability(
    peer_id: &str,
    kind: MeshAdmissionRequestKind,
    payload_bytes: u64,
    event_count: u32,
    body_fetch_bytes: u64,
    now_epoch_ms: u64,
) -> MeshAdmissionDecision {
    admit_authenticated_mesh_capability_with_state(
        &MeshPeerAdmissionState::new(peer_id),
        kind,
        payload_bytes,
        event_count,
        body_fetch_bytes,
        now_epoch_ms,
    )
}

/// Same caps as [`admit_authenticated_mesh_capability`], but uses the
/// caller's persisted peer state so backoff and malformed counts survive
/// across authenticated requests.
#[must_use]
pub fn admit_authenticated_mesh_capability_with_state(
    state: &MeshPeerAdmissionState,
    kind: MeshAdmissionRequestKind,
    payload_bytes: u64,
    event_count: u32,
    body_fetch_bytes: u64,
    now_epoch_ms: u64,
) -> MeshAdmissionDecision {
    let request = MeshAdmissionRequest::new(state.peer_id.as_str(), kind, now_epoch_ms)
        .with_payload(payload_bytes)
        .with_event_count(event_count)
        .with_body_fetch_bytes(body_fetch_bytes);
    decide_admission(MeshAdmissionLimits::conservative_default(), state, &request)
}

/// Fold a decision into durable per-peer counters. Allowed requests raise
/// in-flight; rejected payloads/index jobs raise the backoff clocks.
pub fn record_authenticated_admission(
    state: &mut MeshPeerAdmissionState,
    decision: &MeshAdmissionDecision,
    now_epoch_ms: u64,
) {
    if decision.allowed() {
        state.in_flight_requests = state.in_flight_requests.saturating_add(1);
        return;
    }
    match decision.reason {
        MeshAdmissionReason::PayloadRejected => {
            state.malformed_frame_count = state.malformed_frame_count.saturating_add(1);
        }
        MeshAdmissionReason::BudgetExhausted => {
            state.policy_denial_count = state.policy_denial_count.saturating_add(1);
        }
        MeshAdmissionReason::PeerThrottled
        | MeshAdmissionReason::BackoffActive
        | MeshAdmissionReason::WithinBudget => {}
    }
    state.backoff_until_epoch_ms = computed_backoff_until_epoch_ms(
        MeshAdmissionLimits::conservative_default(),
        state,
        now_epoch_ms,
    );
}

/// Release one previously allowed in-flight authenticated request.
pub fn release_authenticated_admission(state: &mut MeshPeerAdmissionState) {
    state.in_flight_requests = state.in_flight_requests.saturating_sub(1);
}

#[must_use]
pub fn decide_admission(
    limits: MeshAdmissionLimits,
    state: &MeshPeerAdmissionState,
    request: &MeshAdmissionRequest,
) -> MeshAdmissionDecision {
    if let Some(backoff_until) = state.backoff_until_epoch_ms
        && backoff_until > request.now_epoch_ms
    {
        return decision(
            state,
            request,
            MeshAdmissionAction::Throttle,
            MeshAdmissionReason::BackoffActive,
            Some(backoff_until),
        );
    }

    let computed_backoff = computed_backoff_until_epoch_ms(limits, state, request.now_epoch_ms);
    if computed_backoff.is_some() {
        return decision(
            state,
            request,
            MeshAdmissionAction::Throttle,
            MeshAdmissionReason::PeerThrottled,
            computed_backoff,
        );
    }

    if state.in_flight_requests >= limits.max_concurrent_requests_per_peer {
        return decision(
            state,
            request,
            MeshAdmissionAction::Throttle,
            MeshAdmissionReason::PeerThrottled,
            None,
        );
    }

    if request.kind == MeshAdmissionRequestKind::EventBatch
        && (request.event_count > limits.max_event_batch_count
            || request.payload_bytes > limits.max_event_batch_bytes)
    {
        return decision(
            state,
            request,
            MeshAdmissionAction::Reject,
            MeshAdmissionReason::PayloadRejected,
            None,
        );
    }

    if request.kind == MeshAdmissionRequestKind::BodyFetch
        && request.body_fetch_bytes > limits.max_body_fetch_bytes
    {
        return decision(
            state,
            request,
            MeshAdmissionAction::Reject,
            MeshAdmissionReason::PayloadRejected,
            None,
        );
    }

    if request.kind == MeshAdmissionRequestKind::IndexJobs
        && state
            .queued_index_jobs
            .saturating_add(request.requested_index_jobs)
            > limits.max_index_jobs_per_round
    {
        return decision(
            state,
            request,
            MeshAdmissionAction::Reject,
            MeshAdmissionReason::BudgetExhausted,
            None,
        );
    }

    decision(
        state,
        request,
        MeshAdmissionAction::Allow,
        MeshAdmissionReason::WithinBudget,
        None,
    )
}

#[must_use]
pub fn computed_backoff_until_epoch_ms(
    limits: MeshAdmissionLimits,
    state: &MeshPeerAdmissionState,
    now_epoch_ms: u64,
) -> Option<u64> {
    let malformed_excess = state
        .malformed_frame_count
        .saturating_sub(limits.max_malformed_frames_before_backoff);
    let policy_excess = state
        .policy_denial_count
        .saturating_sub(limits.max_policy_denials_before_backoff);
    let excess = malformed_excess.max(policy_excess);
    if excess == 0 {
        return None;
    }

    let shift = excess.saturating_sub(1).min(31);
    let multiplier = 1_u64.checked_shl(shift).unwrap_or(u64::MAX);
    let delay = limits
        .initial_backoff_ms
        .saturating_mul(multiplier)
        .min(limits.max_backoff_ms);
    Some(now_epoch_ms.saturating_add(delay))
}

#[must_use]
pub fn admission_status(decisions: &[MeshAdmissionDecision]) -> MeshAdmissionStatus {
    let mut degraded = BTreeMap::<&'static str, ()>::new();
    let mut per_peer_by_alias = BTreeMap::<String, MeshAdmissionPeerStatus>::new();
    for decision in decisions {
        if decision.reason != MeshAdmissionReason::WithinBudget {
            degraded.insert(decision.code, ());
        }

        let alias = peer_alias(&decision.peer_id);
        let candidate = MeshAdmissionPeerStatus {
            peer_alias: alias.clone(),
            action: decision.action,
            reason: decision.reason,
            backoff_until_epoch_ms: decision.backoff_until_epoch_ms,
        };
        per_peer_by_alias
            .entry(alias)
            .and_modify(|current| *current = stronger_peer_status(current, &candidate))
            .or_insert(candidate);
    }

    let per_peer = per_peer_by_alias.into_values().collect::<Vec<_>>();
    let throttled_peer_count = per_peer
        .iter()
        .filter(|peer| peer.action == MeshAdmissionAction::Throttle)
        .count();
    let rejected_peer_count = per_peer
        .iter()
        .filter(|peer| peer.action == MeshAdmissionAction::Reject)
        .count();
    let budget_exhausted_peer_count = per_peer
        .iter()
        .filter(|peer| peer.reason == MeshAdmissionReason::BudgetExhausted)
        .count();
    let peer_count = per_peer.len();
    let has_peer_pressure =
        throttled_peer_count > 0 || rejected_peer_count > 0 || budget_exhausted_peer_count > 0;
    let local_tier1_unaffected = has_peer_pressure
        && decisions
            .iter()
            .all(|decision| decision.local_tier1_unaffected);
    if local_tier1_unaffected {
        degraded.insert(degraded_codes::LOCAL_TIER1_UNAFFECTED, ());
    }

    MeshAdmissionStatus {
        schema: MESH_ADMISSION_STATUS_SCHEMA_V1,
        peer_count,
        throttled_peer_count,
        rejected_peer_count,
        budget_exhausted_peer_count,
        local_tier1_unaffected,
        degraded: degraded.into_keys().collect(),
        per_peer,
    }
}

#[must_use]
pub fn admission_doctor_report(status: &MeshAdmissionStatus) -> MeshAdmissionDoctorReport {
    let mut signals = Vec::new();
    let throttled = peer_count_for_reason(status, MeshAdmissionReason::PeerThrottled)
        + peer_count_for_reason(status, MeshAdmissionReason::BackoffActive);
    if throttled > 0 {
        signals.push(MeshAdmissionDoctorSignal {
            code: degraded_codes::PEER_THROTTLED,
            severity: "warning",
            peer_count: throttled,
            message: "mesh admission throttled one or more peers before local work was affected",
            repair: "Inspect mesh admission status and wait for peer backoff windows before accepting more peer work.",
        });
    }

    let payload_rejected = peer_count_for_reason(status, MeshAdmissionReason::PayloadRejected);
    if payload_rejected > 0 {
        signals.push(MeshAdmissionDoctorSignal {
            code: degraded_codes::PAYLOAD_REJECTED,
            severity: "medium",
            peer_count: payload_rejected,
            message: "mesh admission rejected oversized peer payloads",
            repair: "Reduce event batch or body fetch size for the affected peer.",
        });
    }

    let budget_exhausted = peer_count_for_reason(status, MeshAdmissionReason::BudgetExhausted);
    if budget_exhausted > 0 {
        signals.push(MeshAdmissionDoctorSignal {
            code: degraded_codes::BUDGET_EXHAUSTED,
            severity: "medium",
            peer_count: budget_exhausted,
            message: "mesh admission rejected peer work that would exceed local resource budgets",
            repair: "Drain queued peer index work or lower requested index jobs before retrying.",
        });
    }

    if status.local_tier1_unaffected {
        signals.push(MeshAdmissionDoctorSignal {
            code: degraded_codes::LOCAL_TIER1_UNAFFECTED,
            severity: "info",
            peer_count: status.peer_count,
            message: "local Tier-1 memory capacity remained reserved despite peer pressure",
            repair: "No repair needed; this signal confirms peer isolation worked.",
        });
    }

    MeshAdmissionDoctorReport {
        schema: MESH_ADMISSION_DOCTOR_SCHEMA_V1,
        posture: if status.throttled_peer_count == 0
            && status.rejected_peer_count == 0
            && status.budget_exhausted_peer_count == 0
        {
            MeshAdmissionDoctorPosture::Ok
        } else {
            MeshAdmissionDoctorPosture::DegradedRecoverable
        },
        peer_count: status.peer_count,
        local_tier1_unaffected: status.local_tier1_unaffected,
        signals,
    }
}

#[must_use]
pub fn admission_test_event(
    scenario: MeshAdmissionScenario,
    decision: &MeshAdmissionDecision,
) -> MeshAdmissionTestEvent {
    MeshAdmissionTestEvent {
        schema: TEST_EVENT_SCHEMA_V1,
        surface: MESH_ADMISSION_E2E_SURFACE,
        scenario,
        phase: "assert",
        peer_id: decision.peer_id.clone(),
        request_kind: decision.request_kind,
        action: decision.action,
        code: decision.code,
        backoff_until_epoch_ms: decision.backoff_until_epoch_ms,
        local_tier1_unaffected: decision.local_tier1_unaffected,
    }
}

fn stronger_peer_status(
    current: &MeshAdmissionPeerStatus,
    candidate: &MeshAdmissionPeerStatus,
) -> MeshAdmissionPeerStatus {
    let current_rank = admission_reporting_rank(current.action, current.reason);
    let candidate_rank = admission_reporting_rank(candidate.action, candidate.reason);
    if candidate_rank > current_rank
        || (candidate_rank == current_rank
            && candidate.backoff_until_epoch_ms > current.backoff_until_epoch_ms)
    {
        candidate.clone()
    } else {
        current.clone()
    }
}

const fn admission_reporting_rank(action: MeshAdmissionAction, reason: MeshAdmissionReason) -> u8 {
    match (action, reason) {
        (MeshAdmissionAction::Reject, MeshAdmissionReason::BudgetExhausted) => 50,
        (MeshAdmissionAction::Reject, MeshAdmissionReason::PayloadRejected) => 40,
        (MeshAdmissionAction::Throttle, MeshAdmissionReason::BackoffActive) => 30,
        (MeshAdmissionAction::Throttle, MeshAdmissionReason::PeerThrottled) => 20,
        _ => 0,
    }
}

fn peer_count_for_reason(status: &MeshAdmissionStatus, reason: MeshAdmissionReason) -> usize {
    status
        .per_peer
        .iter()
        .filter(|peer| peer.reason == reason)
        .count()
}

fn decision(
    state: &MeshPeerAdmissionState,
    request: &MeshAdmissionRequest,
    action: MeshAdmissionAction,
    reason: MeshAdmissionReason,
    backoff_until_epoch_ms: Option<u64>,
) -> MeshAdmissionDecision {
    MeshAdmissionDecision {
        schema: MESH_ADMISSION_SCHEMA_V1,
        peer_id: request.peer_id.clone(),
        request_kind: request.kind,
        action,
        reason,
        code: reason.code(),
        payload_bytes: request.payload_bytes,
        event_count: request.event_count,
        body_fetch_bytes: request.body_fetch_bytes,
        requested_index_jobs: request.requested_index_jobs,
        in_flight_requests: state.in_flight_requests,
        queued_index_jobs: state.queued_index_jobs,
        backoff_until_epoch_ms,
        local_tier1_unaffected: state.local_tier1_reserved,
    }
}

fn peer_alias(peer_id: &str) -> String {
    format!("peer_{}", stable_hash_hex(peer_id, 12))
}

fn stable_hash_hex(value: &str, width: usize) -> String {
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

    let mut hash = FNV_OFFSET;
    for byte in value.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }

    let hex = format!("{hash:016x}");
    hex.chars().take(width.min(hex.len())).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_774_112_000_000;

    fn request(kind: MeshAdmissionRequestKind) -> MeshAdmissionRequest {
        MeshAdmissionRequest::new("peer-noisy", kind, NOW)
    }

    #[test]
    fn conservative_limits_match_published_team_confed_budgets() {
        let limits = MeshAdmissionLimits::conservative_default();
        assert_eq!(limits.max_event_batch_count, 512);
        assert_eq!(limits.max_event_batch_bytes, 4 * 1024 * 1024);
        assert_eq!(limits.max_body_fetch_bytes, 512 * 1024);
        assert_eq!(limits.max_index_jobs_per_round, 16);
        assert_eq!(limits.max_concurrent_requests_per_peer, 4);
    }

    #[test]
    fn persisted_peer_state_backs_off_after_repeated_payload_rejects() {
        let limits = MeshAdmissionLimits::conservative_default();
        let mut state = MeshPeerAdmissionState::new("peer-noisy");
        let oversize = limits.max_body_fetch_bytes.saturating_add(1);
        for _ in 0..limits.max_malformed_frames_before_backoff {
            let decision = admit_authenticated_mesh_capability_with_state(
                &state,
                MeshAdmissionRequestKind::BodyFetch,
                oversize,
                0,
                oversize,
                NOW,
            );
            assert_eq!(decision.reason, MeshAdmissionReason::PayloadRejected);
            record_authenticated_admission(&mut state, &decision, NOW);
        }
        let throttled = admit_authenticated_mesh_capability_with_state(
            &state,
            MeshAdmissionRequestKind::Summary,
            8,
            0,
            0,
            NOW,
        );
        assert!(!throttled.allowed());
        assert_eq!(throttled.reason, MeshAdmissionReason::PeerThrottled);
        assert!(state.backoff_until_epoch_ms.is_some());
        release_authenticated_admission(&mut state);
        assert_eq!(state.in_flight_requests, 0);
    }

    #[test]
    fn authenticated_wrapper_rejects_oversized_body_fetch() {
        let limits = MeshAdmissionLimits::conservative_default();
        let allowed = admit_authenticated_mesh_capability(
            "peer-ok",
            MeshAdmissionRequestKind::BodyFetch,
            32,
            0,
            32,
            NOW,
        );
        assert!(allowed.allowed());
        let rejected = admit_authenticated_mesh_capability(
            "peer-noisy",
            MeshAdmissionRequestKind::BodyFetch,
            limits.max_body_fetch_bytes + 1,
            0,
            limits.max_body_fetch_bytes + 1,
            NOW,
        );
        assert!(!rejected.allowed());
        assert_eq!(rejected.reason, MeshAdmissionReason::PayloadRejected);
    }

    #[test]
    fn concurrent_requests_are_throttled_before_local_tier1_is_touched() {
        let limits = MeshAdmissionLimits::conservative_default();
        let state = MeshPeerAdmissionState::new("peer-noisy")
            .with_in_flight_requests(limits.max_concurrent_requests_per_peer);
        let decision = decide_admission(limits, &state, &request(MeshAdmissionRequestKind::Hello));

        assert!(!decision.allowed());
        assert_eq!(decision.action, MeshAdmissionAction::Throttle);
        assert_eq!(decision.reason, MeshAdmissionReason::PeerThrottled);
        assert_eq!(decision.code, degraded_codes::PEER_THROTTLED);
        assert!(decision.local_tier1_unaffected);
    }

    #[test]
    fn oversized_batches_and_body_fetches_are_rejected() {
        let limits = MeshAdmissionLimits::conservative_default();
        let state = MeshPeerAdmissionState::new("peer-noisy");

        let batch = decide_admission(
            limits,
            &state,
            &request(MeshAdmissionRequestKind::EventBatch)
                .with_event_count(limits.max_event_batch_count + 1)
                .with_payload(limits.max_event_batch_bytes),
        );
        assert_eq!(batch.action, MeshAdmissionAction::Reject);
        assert_eq!(batch.reason, MeshAdmissionReason::PayloadRejected);
        assert_eq!(batch.code, degraded_codes::PAYLOAD_REJECTED);

        let body = decide_admission(
            limits,
            &state,
            &request(MeshAdmissionRequestKind::BodyFetch)
                .with_body_fetch_bytes(limits.max_body_fetch_bytes + 1),
        );
        assert_eq!(body.action, MeshAdmissionAction::Reject);
        assert_eq!(body.reason, MeshAdmissionReason::PayloadRejected);
    }

    #[test]
    fn index_amplification_is_budget_exhausted() {
        let limits = MeshAdmissionLimits::conservative_default();
        let state = MeshPeerAdmissionState::new("peer-noisy")
            .with_queued_index_jobs(limits.max_index_jobs_per_round);
        let decision = decide_admission(
            limits,
            &state,
            &request(MeshAdmissionRequestKind::IndexJobs).with_requested_index_jobs(1),
        );

        assert_eq!(decision.action, MeshAdmissionAction::Reject);
        assert_eq!(decision.reason, MeshAdmissionReason::BudgetExhausted);
        assert_eq!(decision.code, degraded_codes::BUDGET_EXHAUSTED);
    }

    #[test]
    fn malformed_frames_and_policy_denials_compute_deterministic_backoff() {
        let limits = MeshAdmissionLimits::conservative_default();
        let state = MeshPeerAdmissionState::new("peer-noisy")
            .with_malformed_frame_count(limits.max_malformed_frames_before_backoff + 2)
            .with_policy_denial_count(limits.max_policy_denials_before_backoff + 1);
        let decision = decide_admission(
            limits,
            &state,
            &request(MeshAdmissionRequestKind::RangeRequest),
        );

        assert_eq!(decision.action, MeshAdmissionAction::Throttle);
        assert_eq!(decision.reason, MeshAdmissionReason::PeerThrottled);
        assert_eq!(decision.backoff_until_epoch_ms, Some(NOW + 2_000));

        let active = MeshPeerAdmissionState::new("peer-noisy").with_backoff_until(NOW + 9_000);
        let active_decision = decide_admission(
            limits,
            &active,
            &request(MeshAdmissionRequestKind::TipAdvertise),
        );
        assert_eq!(active_decision.reason, MeshAdmissionReason::BackoffActive);
        assert_eq!(active_decision.code, degraded_codes::BACKOFF_ACTIVE);
    }

    #[test]
    fn status_and_e2e_events_are_redaction_safe_and_deterministic() {
        let limits = MeshAdmissionLimits::conservative_default();
        let state = MeshPeerAdmissionState::new("peer-secret-node-key")
            .with_in_flight_requests(limits.max_concurrent_requests_per_peer);
        let throttled = decide_admission(
            limits,
            &state,
            &MeshAdmissionRequest::new(
                "peer-secret-node-key",
                MeshAdmissionRequestKind::Hello,
                NOW,
            ),
        );
        let allowed = decide_admission(
            limits,
            &MeshPeerAdmissionState::new("peer-ok"),
            &MeshAdmissionRequest::new("peer-ok", MeshAdmissionRequestKind::TipAdvertise, NOW),
        );
        let status = admission_status(&[allowed.clone(), throttled.clone()]);
        let event = admission_test_event(MeshAdmissionScenario::PeerThrottled, &throttled);

        assert_eq!(status.schema, MESH_ADMISSION_STATUS_SCHEMA_V1);
        assert_eq!(status.throttled_peer_count, 1);
        assert!(status.local_tier1_unaffected);
        assert!(status.degraded.contains(&degraded_codes::PEER_THROTTLED));
        assert!(
            status
                .degraded
                .contains(&degraded_codes::LOCAL_TIER1_UNAFFECTED)
        );
        assert_eq!(event.schema, TEST_EVENT_SCHEMA_V1);
        assert_eq!(event.scenario.as_str(), "peer_throttled");

        let json = serde_json::to_string(&status).expect("serialize status");
        assert!(json.contains("peer_"));
        assert!(!json.contains("peer-secret-node-key"));
    }

    #[test]
    fn empty_status_reports_no_peer_pressure() {
        let status = admission_status(&[]);
        let doctor = admission_doctor_report(&status);

        assert_eq!(status.peer_count, 0);
        assert_eq!(status.throttled_peer_count, 0);
        assert_eq!(status.rejected_peer_count, 0);
        assert_eq!(status.budget_exhausted_peer_count, 0);
        assert!(!status.local_tier1_unaffected);
        assert!(status.degraded.is_empty());
        assert_eq!(doctor.peer_count, 0);
        assert_eq!(doctor.posture, MeshAdmissionDoctorPosture::Ok);
        assert!(doctor.signals.is_empty());
    }

    #[test]
    fn allowed_only_status_does_not_emit_peer_pressure_signal() {
        let limits = MeshAdmissionLimits::conservative_default();
        let allowed = decide_admission(
            limits,
            &MeshPeerAdmissionState::new("peer-ok"),
            &MeshAdmissionRequest::new("peer-ok", MeshAdmissionRequestKind::TipAdvertise, NOW),
        );
        let status = admission_status(&[allowed]);
        let doctor = admission_doctor_report(&status);

        assert_eq!(status.peer_count, 1);
        assert_eq!(status.throttled_peer_count, 0);
        assert_eq!(status.rejected_peer_count, 0);
        assert_eq!(status.budget_exhausted_peer_count, 0);
        assert!(!status.local_tier1_unaffected);
        assert!(status.degraded.is_empty());
        assert_eq!(doctor.posture, MeshAdmissionDoctorPosture::Ok);
        assert!(doctor.signals.is_empty());
    }

    #[test]
    fn status_reports_unique_peers_and_strongest_admission_reason() {
        let limits = MeshAdmissionLimits::conservative_default();
        let peer = MeshPeerAdmissionState::new("peer-noisy")
            .with_in_flight_requests(limits.max_concurrent_requests_per_peer);
        let throttled = decide_admission(
            limits,
            &peer,
            &MeshAdmissionRequest::new("peer-noisy", MeshAdmissionRequestKind::Hello, NOW),
        );
        let rejected = decide_admission(
            limits,
            &MeshPeerAdmissionState::new("peer-noisy"),
            &MeshAdmissionRequest::new("peer-noisy", MeshAdmissionRequestKind::EventBatch, NOW)
                .with_event_count(limits.max_event_batch_count + 1),
        );
        let ok = decide_admission(
            limits,
            &MeshPeerAdmissionState::new("peer-ok"),
            &MeshAdmissionRequest::new("peer-ok", MeshAdmissionRequestKind::TipAdvertise, NOW),
        );

        let status = admission_status(&[throttled, rejected, ok]);

        assert_eq!(status.peer_count, 2);
        assert_eq!(status.throttled_peer_count, 0);
        assert_eq!(status.rejected_peer_count, 1);
        assert_eq!(status.per_peer.len(), 2);
        let noisy = status
            .per_peer
            .iter()
            .find(|peer| peer.reason == MeshAdmissionReason::PayloadRejected)
            .expect("noisy peer should be represented by strongest rejection");
        assert_eq!(noisy.action, MeshAdmissionAction::Reject);
        assert_eq!(noisy.reason, MeshAdmissionReason::PayloadRejected);
    }

    #[test]
    fn doctor_report_summarizes_peer_pressure_without_peer_ids() {
        let limits = MeshAdmissionLimits::conservative_default();
        let throttled = decide_admission(
            limits,
            &MeshPeerAdmissionState::new("peer-secret-node-key")
                .with_backoff_until(NOW.saturating_add(30_000)),
            &MeshAdmissionRequest::new(
                "peer-secret-node-key",
                MeshAdmissionRequestKind::RangeRequest,
                NOW,
            ),
        );
        let budget_exhausted = decide_admission(
            limits,
            &MeshPeerAdmissionState::new("peer-index")
                .with_queued_index_jobs(limits.max_index_jobs_per_round),
            &MeshAdmissionRequest::new("peer-index", MeshAdmissionRequestKind::IndexJobs, NOW)
                .with_requested_index_jobs(1),
        );
        let status = admission_status(&[throttled, budget_exhausted]);
        let doctor = admission_doctor_report(&status);

        assert_eq!(doctor.schema, MESH_ADMISSION_DOCTOR_SCHEMA_V1);
        assert_eq!(
            doctor.posture,
            MeshAdmissionDoctorPosture::DegradedRecoverable
        );
        assert_eq!(doctor.peer_count, 2);
        assert!(doctor.local_tier1_unaffected);
        assert_eq!(
            doctor
                .signals
                .iter()
                .map(|signal| signal.code)
                .collect::<Vec<_>>(),
            vec![
                degraded_codes::PEER_THROTTLED,
                degraded_codes::BUDGET_EXHAUSTED,
                degraded_codes::LOCAL_TIER1_UNAFFECTED,
            ]
        );

        let json = serde_json::to_string(&doctor).expect("serialize doctor report");
        assert!(!json.contains("peer-secret-node-key"));
        assert!(!json.contains("peer-index"));
    }
}
