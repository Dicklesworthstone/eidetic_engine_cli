//! Shadow-run gates for comparing policy candidates against incumbents (EE-367).
//!
//! Shadow-run gates execute both an incumbent and candidate policy on the same
//! input, comparing outputs without committing changes. This enables:
//!
//! - Safe A/B comparison of pack selection strategies
//! - Curation policy candidate evaluation
//! - Cache admission policy benchmarking
//! - Verification/proof admission policy checks
//!
//! All shadow runs are dry-run by default and emit comparison reports.

use std::fmt;

pub const SUBSYSTEM: &str = "shadow";
pub const SHADOW_REPORT_SCHEMA_V1: &str = "ee.shadow_report.v1";
pub const SHADOW_POLICY_INVENTORY_SCHEMA_V1: &str = "ee.shadow_policy_inventory.v1";
pub const SHADOW_POLICY_SCORE_SCHEMA_V1: &str = "ee.shadow_policy_score.v1";
pub const SHADOW_POLICY_VERDICT_SCHEMA_V1: &str = "ee.shadow_policy_verdict.v1";
pub const SHADOW_POLICY_OPERATOR_WARNING: &str =
    "shadow_verdict_is_advisory_only_no_policy_mutation";

#[must_use]
pub const fn subsystem_name() -> &'static str {
    SUBSYSTEM
}

/// Shadow-run mode controlling what actions are taken.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShadowMode {
    /// Compare only, no side effects.
    Compare,
    /// Compare and log metrics.
    CompareAndLog,
    /// Compare, log, and emit alerts on divergence.
    CompareLogAlert,
}

impl ShadowMode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Compare => "compare",
            Self::CompareAndLog => "compare_and_log",
            Self::CompareLogAlert => "compare_log_alert",
        }
    }

    #[must_use]
    pub const fn all() -> [Self; 3] {
        [Self::Compare, Self::CompareAndLog, Self::CompareLogAlert]
    }
}

impl fmt::Display for ShadowMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Policy domain being shadow-run.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum PolicyDomain {
    /// Pack selection policy (MMR, facility-location, etc.).
    PackSelection,
    /// Curation candidate filtering policy.
    CurationFilter,
    /// Cache admission/eviction policy.
    CacheAdmission,
    /// Verification/proof admission policy.
    VerificationAdmission,
}

impl PolicyDomain {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PackSelection => "pack_selection",
            Self::CurationFilter => "curation_filter",
            Self::CacheAdmission => "cache_admission",
            Self::VerificationAdmission => "verification_admission",
        }
    }

    #[must_use]
    pub const fn all() -> [Self; 4] {
        [
            Self::PackSelection,
            Self::CurationFilter,
            Self::CacheAdmission,
            Self::VerificationAdmission,
        ]
    }
}

impl fmt::Display for PolicyDomain {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Whether an inventoried policy is the default, a candidate, or not yet supported.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PolicyInventoryStatus {
    /// The currently selected/default policy for the decision surface.
    Incumbent,
    /// A policy that can be compared against the incumbent without mutation.
    Candidate,
    /// A known decision surface with no supported shadow policy yet.
    Unsupported,
}

impl PolicyInventoryStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Incumbent => "incumbent",
            Self::Candidate => "candidate",
            Self::Unsupported => "unsupported",
        }
    }
}

impl fmt::Display for PolicyInventoryStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Maturity of an inventoried shadow policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PolicyMaturity {
    /// Stable enough to act as a deterministic incumbent.
    Stable,
    /// Experimental and only eligible for side-effect-free comparison.
    Experimental,
    /// Available only through fixtures/goldens until live wiring lands.
    FixtureOnly,
    /// The surface is known but not yet shadowable.
    Unsupported,
}

impl PolicyMaturity {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Stable => "stable",
            Self::Experimental => "experimental",
            Self::FixtureOnly => "fixture_only",
            Self::Unsupported => "unsupported",
        }
    }
}

impl fmt::Display for PolicyMaturity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Static entry in the shadow policy inventory.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ShadowPolicyInventoryEntry {
    /// Stable policy identifier used by `--policy`, reports, and schemas.
    pub policy_id: &'static str,
    /// Domain string. Unsupported domains are listed here even before an enum variant lands.
    pub policy_domain: &'static str,
    /// Whether this is the incumbent/default, a candidate, or unsupported.
    pub status: PolicyInventoryStatus,
    /// Current maturity of the policy surface.
    pub maturity: PolicyMaturity,
    /// Inputs required before this policy can be evaluated.
    pub required_inputs: &'static [&'static str],
    /// Cohorts where this policy can be shadowed.
    pub supported_cohorts: &'static [&'static str],
    /// Degraded/admission codes a caller should expect while evaluating this policy.
    pub known_degraded_modes: &'static [&'static str],
    /// Inventory entries never mutate state while being inspected.
    pub side_effect_free: bool,
    /// Whether the policy can run beside the incumbent without changing user-visible output.
    pub shadowable_without_mutation: bool,
    /// Why a known surface must abstain instead of running.
    pub abstention_reason: Option<&'static str>,
}

const PACK_SELECTION_INPUTS: &[&str] = &["memory_candidates", "task_query", "token_budget"];
const CACHE_ADMISSION_INPUTS: &[&str] = &["cache_workload", "capacity", "key_trace"];
const CURATION_FILTER_INPUTS: &[&str] = &["candidate_memories", "risk_scores"];
const VERIFICATION_ADMISSION_INPUTS: &[&str] =
    &["source_authority", "rch_posture", "local_cargo_tripwire"];
const RESOURCE_ADMISSION_INPUTS: &[&str] = &[
    "host_profile",
    "resource_budget",
    "rch_posture",
    "local_cargo_tripwire",
    "source_authority",
];

const ALL_COHORTS: &[&str] = &["ci_smoke", "small", "standard", "swarm_heavy"];
const FIXTURE_COHORTS: &[&str] = &["ci_smoke", "small"];
const NO_COHORTS: &[&str] = &[];

const PACK_DEGRADED_MODES: &[&str] = &["no_relevant_results", "weak_query_recall"];
const CACHE_DEGRADED_MODES: &[&str] = &["cache_admission_unavailable"];
const CURATION_DEGRADED_MODES: &[&str] = &["insufficient_evidence"];
const VERIFICATION_DEGRADED_MODES: &[&str] = &[
    "rch_blocked",
    "unsafe_local_cargo_posture",
    "stale_source_authority",
];
const RESOURCE_ADMISSION_DEGRADED_MODES: &[&str] = &[
    "rch_blocked",
    "unsafe_local_cargo_posture",
    "stale_source_authority",
];
const UNSUPPORTED_DEGRADED_MODES: &[&str] = &["unsupported_policy_domain"];

pub const SHADOW_POLICY_INVENTORY: &[ShadowPolicyInventoryEntry] = &[
    ShadowPolicyInventoryEntry {
        policy_id: "incumbent.pack.mmr_redundancy",
        policy_domain: "pack_selection",
        status: PolicyInventoryStatus::Incumbent,
        maturity: PolicyMaturity::Stable,
        required_inputs: PACK_SELECTION_INPUTS,
        supported_cohorts: ALL_COHORTS,
        known_degraded_modes: PACK_DEGRADED_MODES,
        side_effect_free: true,
        shadowable_without_mutation: true,
        abstention_reason: None,
    },
    ShadowPolicyInventoryEntry {
        policy_id: "candidate.pack.facility_location",
        policy_domain: "pack_selection",
        status: PolicyInventoryStatus::Candidate,
        maturity: PolicyMaturity::Experimental,
        required_inputs: PACK_SELECTION_INPUTS,
        supported_cohorts: ALL_COHORTS,
        known_degraded_modes: PACK_DEGRADED_MODES,
        side_effect_free: true,
        shadowable_without_mutation: true,
        abstention_reason: None,
    },
    ShadowPolicyInventoryEntry {
        policy_id: "incumbent.cache.no_cache",
        policy_domain: "cache_admission",
        status: PolicyInventoryStatus::Incumbent,
        maturity: PolicyMaturity::Stable,
        required_inputs: CACHE_ADMISSION_INPUTS,
        supported_cohorts: ALL_COHORTS,
        known_degraded_modes: CACHE_DEGRADED_MODES,
        side_effect_free: true,
        shadowable_without_mutation: true,
        abstention_reason: None,
    },
    ShadowPolicyInventoryEntry {
        policy_id: "candidate.cache.s3_fifo",
        policy_domain: "cache_admission",
        status: PolicyInventoryStatus::Candidate,
        maturity: PolicyMaturity::Experimental,
        required_inputs: CACHE_ADMISSION_INPUTS,
        supported_cohorts: ALL_COHORTS,
        known_degraded_modes: CACHE_DEGRADED_MODES,
        side_effect_free: true,
        shadowable_without_mutation: true,
        abstention_reason: None,
    },
    ShadowPolicyInventoryEntry {
        policy_id: "candidate.curation.risk_filter",
        policy_domain: "curation_filter",
        status: PolicyInventoryStatus::Candidate,
        maturity: PolicyMaturity::FixtureOnly,
        required_inputs: CURATION_FILTER_INPUTS,
        supported_cohorts: FIXTURE_COHORTS,
        known_degraded_modes: CURATION_DEGRADED_MODES,
        side_effect_free: true,
        shadowable_without_mutation: true,
        abstention_reason: None,
    },
    ShadowPolicyInventoryEntry {
        policy_id: "incumbent.verification.rch_only",
        policy_domain: "verification_admission",
        status: PolicyInventoryStatus::Incumbent,
        maturity: PolicyMaturity::Stable,
        required_inputs: VERIFICATION_ADMISSION_INPUTS,
        supported_cohorts: ALL_COHORTS,
        known_degraded_modes: VERIFICATION_DEGRADED_MODES,
        side_effect_free: true,
        shadowable_without_mutation: true,
        abstention_reason: None,
    },
    ShadowPolicyInventoryEntry {
        policy_id: "candidate.verification.environment_attestation",
        policy_domain: "verification_admission",
        status: PolicyInventoryStatus::Candidate,
        maturity: PolicyMaturity::Experimental,
        required_inputs: VERIFICATION_ADMISSION_INPUTS,
        supported_cohorts: FIXTURE_COHORTS,
        known_degraded_modes: VERIFICATION_DEGRADED_MODES,
        side_effect_free: true,
        shadowable_without_mutation: true,
        abstention_reason: None,
    },
    ShadowPolicyInventoryEntry {
        policy_id: "candidate.resource_profile_budget_admission",
        policy_domain: "resource_profile_budget_admission",
        status: PolicyInventoryStatus::Candidate,
        maturity: PolicyMaturity::Experimental,
        required_inputs: RESOURCE_ADMISSION_INPUTS,
        supported_cohorts: ALL_COHORTS,
        known_degraded_modes: RESOURCE_ADMISSION_DEGRADED_MODES,
        side_effect_free: true,
        shadowable_without_mutation: true,
        abstention_reason: None,
    },
    ShadowPolicyInventoryEntry {
        policy_id: "unsupported.resource_profile_budget_admission",
        policy_domain: "resource_profile_budget_admission",
        status: PolicyInventoryStatus::Unsupported,
        maturity: PolicyMaturity::Unsupported,
        required_inputs: RESOURCE_ADMISSION_INPUTS,
        supported_cohorts: NO_COHORTS,
        known_degraded_modes: UNSUPPORTED_DEGRADED_MODES,
        side_effect_free: true,
        shadowable_without_mutation: false,
        abstention_reason: Some("unsupported_policy_domain"),
    },
];

#[must_use]
pub const fn shadow_policy_inventory() -> &'static [ShadowPolicyInventoryEntry] {
    SHADOW_POLICY_INVENTORY
}

#[must_use]
pub fn find_shadow_policy_inventory_entry(
    policy_id: &str,
) -> Option<&'static ShadowPolicyInventoryEntry> {
    SHADOW_POLICY_INVENTORY
        .iter()
        .find(|entry| entry.policy_id == policy_id)
}

pub const RESOURCE_ADMISSION_SCHEMA_V1: &str = "ee.resource_admission.v1";
pub const RESOURCE_ADMISSION_POLICY_DOMAIN: &str = "resource_profile_budget_admission";
pub const RESOURCE_ADMISSION_CANDIDATE_POLICY_ID: &str =
    "candidate.resource_profile_budget_admission";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResourceOperatingProfile {
    Constrained,
    Portable,
    Workstation,
    Swarm,
}

impl ResourceOperatingProfile {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Constrained => "constrained",
            Self::Portable => "portable",
            Self::Workstation => "workstation",
            Self::Swarm => "swarm",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResourceCostClass {
    Tiny,
    Small,
    Standard,
    SwarmHeavy,
    Unknown,
}

impl ResourceCostClass {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Tiny => "tiny",
            Self::Small => "small",
            Self::Standard => "standard",
            Self::SwarmHeavy => "swarm_heavy",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResourceAdmissionDecision {
    Admit,
    DegradeToLean,
    Queue,
    WaitForRch,
    SplitWorkload,
    RefuseLocalCargo,
    Abstain,
}

impl ResourceAdmissionDecision {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Admit => "admit",
            Self::DegradeToLean => "degrade_to_lean",
            Self::Queue => "queue",
            Self::WaitForRch => "wait_for_rch",
            Self::SplitWorkload => "split_workload",
            Self::RefuseLocalCargo => "refuse_local_cargo",
            Self::Abstain => "abstain",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResourceHostCalibrationPosture {
    Fresh,
    Stale,
    Partial,
    SyntheticOnly,
    Contradictory,
    Missing,
    Unavailable,
    NotApplicable,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResourceBudgetPosture {
    WithinBudget,
    RecommendDecrease,
    RecommendIncrease,
    OverrideClamped,
    Missing,
    Contradictory,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResourceRchPosture {
    RemoteReady,
    ActiveProjectExclusion,
    ProgressStale,
    Blocked,
    NotRequired,
    Unknown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResourceLocalCargoPosture {
    Clean,
    Refused,
    Unsafe,
    Unknown,
    NotRequired,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResourceLanePressurePosture {
    Clear,
    ForegroundPressure,
    BackgroundPressure,
    VerificationPressure,
    MaintenancePressure,
    MixedPressure,
    Unknown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResourceWorkloadPressurePosture {
    WithinBudget,
    CachePressure,
    WriteSpoolPressure,
    ReadPoolPressure,
    PackSloPressure,
    IndexPressure,
    GraphPressure,
    MixedPressure,
    Unknown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResourceDaemonPosture {
    Available,
    Unavailable,
    Degraded,
    NotRequired,
    Unknown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResourceReplayPosture {
    Healthy,
    Regression,
    Stale,
    Missing,
    NotRequired,
    Unknown,
}

pub const RESOURCE_QUEUE_PRESSURE_REDACTION_POSTURE: &str =
    "counts_ids_statuses_hashes_only_no_mail_body_no_command_argv_no_absolute_paths";
pub const RESOURCE_QUEUE_PRESSURE_MAX_SOURCE_REFS: usize = 12;
pub const RESOURCE_QUEUE_PRESSURE_BOUNDED_PREVIEW_MAX_CHARS: usize = 160;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResourceQueuePressureLevel {
    Idle,
    Low,
    Moderate,
    Saturated,
    Unknown,
}

impl ResourceQueuePressureLevel {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Low => "low",
            Self::Moderate => "moderate",
            Self::Saturated => "saturated",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResourceQueuePressureReasonCode {
    RchLaneBusy,
    RchTelemetryGap,
    ActiveBuildSlotExhausted,
    StaleInProgressBead,
    AgentMailUnavailable,
    AgentMailRecoveryCorrupt,
    DirtyCheckoutSaturated,
    LocalCargoRefused,
    OutputBudgetPressure,
    HostCalibrationMissing,
    ContradictorySourceState,
}

impl ResourceQueuePressureReasonCode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::RchLaneBusy => "rch_lane_busy",
            Self::RchTelemetryGap => "rch_telemetry_gap",
            Self::ActiveBuildSlotExhausted => "active_build_slot_exhausted",
            Self::StaleInProgressBead => "stale_in_progress_bead",
            Self::AgentMailUnavailable => "agent_mail_unavailable",
            Self::AgentMailRecoveryCorrupt => "agent_mail_recovery_corrupt",
            Self::DirtyCheckoutSaturated => "dirty_checkout_saturated",
            Self::LocalCargoRefused => "local_cargo_refused",
            Self::OutputBudgetPressure => "output_budget_pressure",
            Self::HostCalibrationMissing => "host_calibration_missing",
            Self::ContradictorySourceState => "contradictory_source_state",
        }
    }

    #[must_use]
    pub const fn all() -> [Self; 11] {
        [
            Self::RchLaneBusy,
            Self::RchTelemetryGap,
            Self::ActiveBuildSlotExhausted,
            Self::StaleInProgressBead,
            Self::AgentMailUnavailable,
            Self::AgentMailRecoveryCorrupt,
            Self::DirtyCheckoutSaturated,
            Self::LocalCargoRefused,
            Self::OutputBudgetPressure,
            Self::HostCalibrationMissing,
            Self::ContradictorySourceState,
        ]
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResourceQueuePressureSourceKind {
    RchStatus,
    RchSelectorAdmissionProbe,
    BuildSlotLease,
    BeadsInProgressSummary,
    AgentMailHealth,
    AgentMailRecoveryProbe,
    GitDirtySummary,
    LocalCargoTripwire,
    OutputBudgetGovernor,
    HostCalibrationPosture,
    SourceAuthoritySnapshot,
    ManualFixture,
}

impl ResourceQueuePressureSourceKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::RchStatus => "rch_status",
            Self::RchSelectorAdmissionProbe => "rch_selector_admission_probe",
            Self::BuildSlotLease => "build_slot_lease",
            Self::BeadsInProgressSummary => "beads_in_progress_summary",
            Self::AgentMailHealth => "agent_mail_health",
            Self::AgentMailRecoveryProbe => "agent_mail_recovery_probe",
            Self::GitDirtySummary => "git_dirty_summary",
            Self::LocalCargoTripwire => "local_cargo_tripwire",
            Self::OutputBudgetGovernor => "output_budget_governor",
            Self::HostCalibrationPosture => "host_calibration_posture",
            Self::SourceAuthoritySnapshot => "source_authority_snapshot",
            Self::ManualFixture => "manual_fixture",
        }
    }

    #[must_use]
    pub const fn all() -> [Self; 12] {
        [
            Self::RchStatus,
            Self::RchSelectorAdmissionProbe,
            Self::BuildSlotLease,
            Self::BeadsInProgressSummary,
            Self::AgentMailHealth,
            Self::AgentMailRecoveryProbe,
            Self::GitDirtySummary,
            Self::LocalCargoTripwire,
            Self::OutputBudgetGovernor,
            Self::HostCalibrationPosture,
            Self::SourceAuthoritySnapshot,
            Self::ManualFixture,
        ]
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResourceQueuePressureSourceState {
    Fresh,
    Partial,
    Degraded,
    Unavailable,
    Corrupt,
    Stale,
    Contradictory,
}

impl ResourceQueuePressureSourceState {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Fresh => "fresh",
            Self::Partial => "partial",
            Self::Degraded => "degraded",
            Self::Unavailable => "unavailable",
            Self::Corrupt => "corrupt",
            Self::Stale => "stale",
            Self::Contradictory => "contradictory",
        }
    }

    #[must_use]
    pub const fn freshness(self) -> &'static str {
        match self {
            Self::Fresh | Self::Partial | Self::Degraded => "fresh",
            Self::Unavailable | Self::Corrupt => "unavailable",
            Self::Stale => "stale",
            Self::Contradictory => "contradictory",
        }
    }

    #[must_use]
    pub const fn evidence_state(self) -> &'static str {
        match self {
            Self::Fresh | Self::Partial | Self::Degraded | Self::Stale => "observed",
            Self::Unavailable => "unavailable",
            Self::Corrupt => "corrupt",
            Self::Contradictory => "contradictory",
        }
    }

    #[must_use]
    pub const fn confidence(self) -> &'static str {
        match self {
            Self::Fresh => "high",
            Self::Partial | Self::Degraded => "medium",
            Self::Unavailable | Self::Corrupt | Self::Stale | Self::Contradictory => "low",
        }
    }

    #[must_use]
    pub const fn is_untrusted(self) -> bool {
        matches!(
            self,
            Self::Unavailable | Self::Corrupt | Self::Stale | Self::Contradictory
        )
    }

    #[must_use]
    pub const fn all() -> [Self; 7] {
        [
            Self::Fresh,
            Self::Partial,
            Self::Degraded,
            Self::Unavailable,
            Self::Corrupt,
            Self::Stale,
            Self::Contradictory,
        ]
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResourceQueuePressureSourceRef {
    pub kind: ResourceQueuePressureSourceKind,
    pub source_schema: Option<&'static str>,
    pub state: ResourceQueuePressureSourceState,
    pub reason_code: Option<ResourceQueuePressureReasonCode>,
    pub hash: Option<String>,
    pub bounded_preview: Option<String>,
}

impl ResourceQueuePressureSourceRef {
    #[must_use]
    pub const fn new(
        kind: ResourceQueuePressureSourceKind,
        state: ResourceQueuePressureSourceState,
    ) -> Self {
        Self {
            kind,
            source_schema: None,
            state,
            reason_code: None,
            hash: None,
            bounded_preview: None,
        }
    }

    #[must_use]
    pub const fn with_source_schema(mut self, source_schema: &'static str) -> Self {
        self.source_schema = Some(source_schema);
        self
    }

    #[must_use]
    pub const fn with_reason_code(mut self, reason_code: ResourceQueuePressureReasonCode) -> Self {
        self.reason_code = Some(reason_code);
        self
    }

    #[must_use]
    pub fn with_hash(mut self, hash: impl Into<String>) -> Self {
        self.hash = Some(hash.into());
        self
    }

    #[must_use]
    pub fn with_bounded_preview(mut self, preview: impl AsRef<str>) -> Self {
        self.bounded_preview = Some(truncate_bounded_preview(preview.as_ref()));
        self
    }

    #[must_use]
    pub const fn source_label(&self) -> &'static str {
        self.kind.as_str()
    }

    #[must_use]
    pub const fn freshness(&self) -> &'static str {
        self.state.freshness()
    }

    #[must_use]
    pub const fn evidence_state(&self) -> &'static str {
        self.state.evidence_state()
    }

    #[must_use]
    pub const fn confidence(&self) -> &'static str {
        self.state.confidence()
    }

    #[must_use]
    pub const fn resolved_reason_code(&self) -> Option<ResourceQueuePressureReasonCode> {
        match self.reason_code {
            Some(reason_code) => Some(reason_code),
            None => default_queue_pressure_reason(self.kind, self.state),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResourceQueuePressureInventory {
    source_refs: Vec<ResourceQueuePressureSourceRef>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResourceQueuePressureReport {
    pub level: ResourceQueuePressureLevel,
    pub can_authorize_claim: bool,
    pub reason_codes: Vec<String>,
    pub abstained_sources: Vec<String>,
    pub source_refs: Vec<ResourceQueuePressureSourceRef>,
    pub redaction_posture: &'static str,
}

impl ResourceQueuePressureInventory {
    #[must_use]
    pub fn new(source_refs: Vec<ResourceQueuePressureSourceRef>) -> Self {
        Self {
            source_refs: source_refs
                .into_iter()
                .take(RESOURCE_QUEUE_PRESSURE_MAX_SOURCE_REFS)
                .collect(),
        }
    }

    #[must_use]
    pub fn source_refs(&self) -> &[ResourceQueuePressureSourceRef] {
        &self.source_refs
    }

    #[must_use]
    pub fn report(&self) -> ResourceQueuePressureReport {
        ResourceQueuePressureReport {
            level: self.level(),
            can_authorize_claim: false,
            reason_codes: self.reason_codes(),
            abstained_sources: self.abstained_sources(),
            source_refs: self.source_refs.clone(),
            redaction_posture: self.redaction_posture(),
        }
    }

    #[must_use]
    pub fn level(&self) -> ResourceQueuePressureLevel {
        if self.source_refs.is_empty()
            || self
                .source_refs
                .iter()
                .any(|source_ref| source_ref.state.is_untrusted())
        {
            return ResourceQueuePressureLevel::Unknown;
        }

        let reason_codes = self.resolved_reason_code_values();
        if reason_codes.iter().any(|reason_code| {
            matches!(
                reason_code,
                ResourceQueuePressureReasonCode::ActiveBuildSlotExhausted
                    | ResourceQueuePressureReasonCode::DirtyCheckoutSaturated
                    | ResourceQueuePressureReasonCode::RchLaneBusy
            )
        }) {
            ResourceQueuePressureLevel::Saturated
        } else if reason_codes.iter().any(|reason_code| {
            matches!(
                reason_code,
                ResourceQueuePressureReasonCode::LocalCargoRefused
                    | ResourceQueuePressureReasonCode::OutputBudgetPressure
                    | ResourceQueuePressureReasonCode::StaleInProgressBead
            )
        }) {
            ResourceQueuePressureLevel::Moderate
        } else if self.source_refs.iter().any(|source_ref| {
            matches!(
                source_ref.state,
                ResourceQueuePressureSourceState::Partial
                    | ResourceQueuePressureSourceState::Degraded
            )
        }) {
            ResourceQueuePressureLevel::Low
        } else {
            ResourceQueuePressureLevel::Idle
        }
    }

    #[must_use]
    pub fn reason_codes(&self) -> Vec<String> {
        let mut reason_codes = Vec::new();
        for reason_code in self.resolved_reason_code_values() {
            push_unique(&mut reason_codes, reason_code.as_str());
        }
        reason_codes
    }

    #[must_use]
    pub fn abstained_sources(&self) -> Vec<String> {
        let mut sources = Vec::new();
        for source_ref in &self.source_refs {
            if source_ref.state.is_untrusted() {
                push_unique(&mut sources, source_ref.kind.as_str());
            }
        }
        sources
    }

    #[must_use]
    pub const fn redaction_posture(&self) -> &'static str {
        RESOURCE_QUEUE_PRESSURE_REDACTION_POSTURE
    }

    fn resolved_reason_code_values(&self) -> Vec<ResourceQueuePressureReasonCode> {
        let mut reason_codes = Vec::new();
        for source_ref in &self.source_refs {
            if let Some(reason_code) = source_ref.resolved_reason_code() {
                push_reason_unique(&mut reason_codes, reason_code);
            }
            if source_ref.state == ResourceQueuePressureSourceState::Contradictory {
                push_reason_unique(
                    &mut reason_codes,
                    ResourceQueuePressureReasonCode::ContradictorySourceState,
                );
            }
        }
        reason_codes
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResourceQueuePressureHysteresisHint {
    pub state_key: &'static str,
    pub stable_after_observations: u8,
    pub cooldown_observations: u8,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResourceQueuePressureBackoffInput {
    pub queue_pressure: ResourceQueuePressureReport,
    pub estimated_cost_class: ResourceCostClass,
    pub claim_gate_safe_to_claim: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResourceQueuePressureBackoffAdvice {
    pub decision: ResourceAdmissionDecision,
    pub can_authorize_claim: bool,
    pub primary_reason: String,
    pub contributing_reasons: Vec<String>,
    pub blocked_by: Vec<String>,
    pub next_safe_action: &'static str,
    pub what_would_change: &'static str,
    pub hysteresis: ResourceQueuePressureHysteresisHint,
}

#[must_use]
pub fn evaluate_resource_queue_pressure_backoff(
    input: &ResourceQueuePressureBackoffInput,
) -> ResourceQueuePressureBackoffAdvice {
    let contributing_reasons = sorted_queue_pressure_reasons(&input.queue_pressure.reason_codes);
    let primary_reason =
        queue_pressure_primary_reason(input.queue_pressure.level, &contributing_reasons);
    let decision = queue_pressure_backoff_decision(
        input.queue_pressure.level,
        input.estimated_cost_class,
        &contributing_reasons,
    );
    let mut blocked_by = queue_pressure_blockers(&contributing_reasons);
    if !input.claim_gate_safe_to_claim {
        push_unique(&mut blocked_by, "claim_gate_authority");
    }

    ResourceQueuePressureBackoffAdvice {
        decision,
        can_authorize_claim: false,
        primary_reason,
        contributing_reasons,
        blocked_by,
        next_safe_action: queue_pressure_next_safe_action(decision),
        what_would_change: queue_pressure_what_would_change(decision),
        hysteresis: queue_pressure_hysteresis_hint(decision),
    }
}

fn truncate_bounded_preview(preview: &str) -> String {
    preview
        .chars()
        .take(RESOURCE_QUEUE_PRESSURE_BOUNDED_PREVIEW_MAX_CHARS)
        .collect()
}

const fn default_queue_pressure_reason(
    kind: ResourceQueuePressureSourceKind,
    state: ResourceQueuePressureSourceState,
) -> Option<ResourceQueuePressureReasonCode> {
    use ResourceQueuePressureReasonCode as Reason;
    use ResourceQueuePressureSourceKind as Kind;
    use ResourceQueuePressureSourceState as State;

    match (kind, state) {
        (_, State::Contradictory) => Some(Reason::ContradictorySourceState),
        (Kind::AgentMailRecoveryProbe, State::Corrupt) => Some(Reason::AgentMailRecoveryCorrupt),
        (Kind::AgentMailHealth, State::Unavailable | State::Degraded) => {
            Some(Reason::AgentMailUnavailable)
        }
        (Kind::RchStatus | Kind::RchSelectorAdmissionProbe, State::Partial | State::Degraded)
        | (Kind::RchStatus | Kind::RchSelectorAdmissionProbe, State::Unavailable | State::Stale)
        | (Kind::BuildSlotLease, State::Unavailable) => Some(Reason::RchTelemetryGap),
        (Kind::BuildSlotLease, State::Degraded) => Some(Reason::ActiveBuildSlotExhausted),
        (Kind::BeadsInProgressSummary, State::Stale) => Some(Reason::StaleInProgressBead),
        (Kind::GitDirtySummary, State::Degraded) => Some(Reason::DirtyCheckoutSaturated),
        (Kind::LocalCargoTripwire, State::Degraded | State::Unavailable | State::Stale) => {
            Some(Reason::LocalCargoRefused)
        }
        (Kind::OutputBudgetGovernor, State::Degraded | State::Partial) => {
            Some(Reason::OutputBudgetPressure)
        }
        (Kind::HostCalibrationPosture, State::Unavailable) => Some(Reason::HostCalibrationMissing),
        _ => None,
    }
}

fn push_reason_unique(
    values: &mut Vec<ResourceQueuePressureReasonCode>,
    value: ResourceQueuePressureReasonCode,
) {
    if !values.contains(&value) {
        values.push(value);
    }
}

fn sorted_queue_pressure_reasons(reason_codes: &[String]) -> Vec<String> {
    let mut sorted = reason_codes.to_vec();
    sorted.sort_by(|left, right| {
        queue_pressure_reason_rank(left)
            .cmp(&queue_pressure_reason_rank(right))
            .then_with(|| left.cmp(right))
    });
    sorted.dedup();
    sorted
}

fn queue_pressure_primary_reason(
    level: ResourceQueuePressureLevel,
    contributing_reasons: &[String],
) -> String {
    contributing_reasons
        .first()
        .cloned()
        .unwrap_or_else(|| match level {
            ResourceQueuePressureLevel::Idle => "queue_pressure_idle".to_owned(),
            ResourceQueuePressureLevel::Low => "queue_pressure_low".to_owned(),
            ResourceQueuePressureLevel::Moderate => "queue_pressure_moderate".to_owned(),
            ResourceQueuePressureLevel::Saturated => "queue_pressure_saturated".to_owned(),
            ResourceQueuePressureLevel::Unknown => "queue_pressure_unknown".to_owned(),
        })
}

fn queue_pressure_backoff_decision(
    level: ResourceQueuePressureLevel,
    estimated_cost_class: ResourceCostClass,
    contributing_reasons: &[String],
) -> ResourceAdmissionDecision {
    if has_queue_pressure_reason(
        contributing_reasons,
        ResourceQueuePressureReasonCode::LocalCargoRefused,
    ) {
        return ResourceAdmissionDecision::RefuseLocalCargo;
    }
    if has_any_queue_pressure_reason(
        contributing_reasons,
        &[
            ResourceQueuePressureReasonCode::ContradictorySourceState,
            ResourceQueuePressureReasonCode::AgentMailRecoveryCorrupt,
            ResourceQueuePressureReasonCode::AgentMailUnavailable,
            ResourceQueuePressureReasonCode::HostCalibrationMissing,
        ],
    ) || level == ResourceQueuePressureLevel::Unknown
    {
        return ResourceAdmissionDecision::Abstain;
    }
    if has_any_queue_pressure_reason(
        contributing_reasons,
        &[
            ResourceQueuePressureReasonCode::RchTelemetryGap,
            ResourceQueuePressureReasonCode::ActiveBuildSlotExhausted,
            ResourceQueuePressureReasonCode::RchLaneBusy,
        ],
    ) {
        return ResourceAdmissionDecision::WaitForRch;
    }
    if estimated_cost_class == ResourceCostClass::SwarmHeavy
        && matches!(
            level,
            ResourceQueuePressureLevel::Moderate | ResourceQueuePressureLevel::Saturated
        )
    {
        return ResourceAdmissionDecision::SplitWorkload;
    }
    if level == ResourceQueuePressureLevel::Saturated
        || has_queue_pressure_reason(
            contributing_reasons,
            ResourceQueuePressureReasonCode::StaleInProgressBead,
        )
    {
        return ResourceAdmissionDecision::Queue;
    }
    if level == ResourceQueuePressureLevel::Moderate
        || has_any_queue_pressure_reason(
            contributing_reasons,
            &[
                ResourceQueuePressureReasonCode::DirtyCheckoutSaturated,
                ResourceQueuePressureReasonCode::OutputBudgetPressure,
            ],
        )
    {
        return ResourceAdmissionDecision::DegradeToLean;
    }
    ResourceAdmissionDecision::Admit
}

fn queue_pressure_blockers(contributing_reasons: &[String]) -> Vec<String> {
    let mut blocked_by = Vec::new();
    for reason in contributing_reasons {
        match reason.as_str() {
            "agent_mail_unavailable" | "agent_mail_recovery_corrupt" => {
                push_unique(&mut blocked_by, "agent_mail");
            }
            "contradictory_source_state" | "host_calibration_missing" => {
                push_unique(&mut blocked_by, "source_authority");
            }
            "rch_lane_busy" | "rch_telemetry_gap" | "active_build_slot_exhausted" => {
                push_unique(&mut blocked_by, "rch_lane");
            }
            "local_cargo_refused" => {
                push_unique(&mut blocked_by, "local_cargo_tripwire");
            }
            "stale_in_progress_bead" => {
                push_unique(&mut blocked_by, "beads_in_progress");
            }
            _ => {}
        }
    }
    blocked_by
}

fn queue_pressure_next_safe_action(decision: ResourceAdmissionDecision) -> &'static str {
    match decision {
        ResourceAdmissionDecision::Admit => "continue_with_existing_claim_gate",
        ResourceAdmissionDecision::DegradeToLean => {
            "ee pack <task> --resource-profile constrained --json"
        }
        ResourceAdmissionDecision::Queue => "wait_for_lane_capacity_then_refresh_swarm_brief",
        ResourceAdmissionDecision::WaitForRch => "rch status --json",
        ResourceAdmissionDecision::SplitWorkload => {
            "split_workload_or_create_narrower_bead_before_claim"
        }
        ResourceAdmissionDecision::RefuseLocalCargo => {
            "scripts/check-local-cargo-tripwire.sh --probe-processes --json"
        }
        ResourceAdmissionDecision::Abstain => {
            "collect_bounded_support_bundle_or_agent_mail_snapshot"
        }
    }
}

fn queue_pressure_what_would_change(decision: ResourceAdmissionDecision) -> &'static str {
    match decision {
        ResourceAdmissionDecision::Admit => "stronger_pressure_evidence_appears",
        ResourceAdmissionDecision::DegradeToLean => "output_budget_or_checkout_pressure_clears",
        ResourceAdmissionDecision::Queue => "queue_pressure_drops_below_saturated",
        ResourceAdmissionDecision::WaitForRch => "rch_lane_has_capacity_and_fresh_telemetry",
        ResourceAdmissionDecision::SplitWorkload => "workload_becomes_standard_or_narrower",
        ResourceAdmissionDecision::RefuseLocalCargo => {
            "remote_proof_path_succeeds_without_local_cargo"
        }
        ResourceAdmissionDecision::Abstain => "untrusted_source_becomes_fresh_and_consistent",
    }
}

fn queue_pressure_hysteresis_hint(
    decision: ResourceAdmissionDecision,
) -> ResourceQueuePressureHysteresisHint {
    match decision {
        ResourceAdmissionDecision::Admit => ResourceQueuePressureHysteresisHint {
            state_key: "admit",
            stable_after_observations: 1,
            cooldown_observations: 0,
        },
        ResourceAdmissionDecision::DegradeToLean => ResourceQueuePressureHysteresisHint {
            state_key: "degrade_to_lean",
            stable_after_observations: 2,
            cooldown_observations: 1,
        },
        ResourceAdmissionDecision::Queue => ResourceQueuePressureHysteresisHint {
            state_key: "queue",
            stable_after_observations: 2,
            cooldown_observations: 2,
        },
        ResourceAdmissionDecision::WaitForRch => ResourceQueuePressureHysteresisHint {
            state_key: "wait_for_rch",
            stable_after_observations: 2,
            cooldown_observations: 2,
        },
        ResourceAdmissionDecision::SplitWorkload => ResourceQueuePressureHysteresisHint {
            state_key: "split_workload",
            stable_after_observations: 2,
            cooldown_observations: 1,
        },
        ResourceAdmissionDecision::RefuseLocalCargo => ResourceQueuePressureHysteresisHint {
            state_key: "refuse_local_cargo",
            stable_after_observations: 1,
            cooldown_observations: 1,
        },
        ResourceAdmissionDecision::Abstain => ResourceQueuePressureHysteresisHint {
            state_key: "abstain",
            stable_after_observations: 1,
            cooldown_observations: 1,
        },
    }
}

fn has_queue_pressure_reason(
    contributing_reasons: &[String],
    reason_code: ResourceQueuePressureReasonCode,
) -> bool {
    contributing_reasons
        .iter()
        .any(|reason| reason == reason_code.as_str())
}

fn has_any_queue_pressure_reason(
    contributing_reasons: &[String],
    reason_codes: &[ResourceQueuePressureReasonCode],
) -> bool {
    reason_codes
        .iter()
        .any(|reason_code| has_queue_pressure_reason(contributing_reasons, *reason_code))
}

fn queue_pressure_reason_rank(reason: &str) -> u8 {
    match reason {
        "local_cargo_refused" => 0,
        "contradictory_source_state" => 1,
        "agent_mail_recovery_corrupt" => 2,
        "agent_mail_unavailable" => 3,
        "host_calibration_missing" => 4,
        "rch_telemetry_gap" => 5,
        "active_build_slot_exhausted" => 6,
        "rch_lane_busy" => 7,
        "stale_in_progress_bead" => 8,
        "dirty_checkout_saturated" => 9,
        "output_budget_pressure" => 10,
        _ => 100,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResourceAdmissionInput {
    pub requested_profile: Option<ResourceOperatingProfile>,
    pub effective_profile: ResourceOperatingProfile,
    pub estimated_cost_class: ResourceCostClass,
    pub host_calibration: ResourceHostCalibrationPosture,
    pub resource_budget: ResourceBudgetPosture,
    pub rch: ResourceRchPosture,
    pub local_cargo: ResourceLocalCargoPosture,
    pub lane_pressure: ResourceLanePressurePosture,
    pub workload_pressure: ResourceWorkloadPressurePosture,
    pub daemon: ResourceDaemonPosture,
    pub replay: ResourceReplayPosture,
    pub redaction_posture_verified: bool,
}

impl Default for ResourceAdmissionInput {
    fn default() -> Self {
        Self {
            requested_profile: Some(ResourceOperatingProfile::Workstation),
            effective_profile: ResourceOperatingProfile::Workstation,
            estimated_cost_class: ResourceCostClass::Standard,
            host_calibration: ResourceHostCalibrationPosture::Fresh,
            resource_budget: ResourceBudgetPosture::WithinBudget,
            rch: ResourceRchPosture::NotRequired,
            local_cargo: ResourceLocalCargoPosture::Clean,
            lane_pressure: ResourceLanePressurePosture::Clear,
            workload_pressure: ResourceWorkloadPressurePosture::WithinBudget,
            daemon: ResourceDaemonPosture::Available,
            replay: ResourceReplayPosture::Healthy,
            redaction_posture_verified: true,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResourceAdmissionReport {
    pub schema: &'static str,
    pub policy_domain: &'static str,
    pub policy_id: &'static str,
    pub side_effect_free: bool,
    pub advisory_only: bool,
    pub decision: ResourceAdmissionDecision,
    pub requested_profile: Option<ResourceOperatingProfile>,
    pub effective_profile: ResourceOperatingProfile,
    pub recommended_profile: ResourceOperatingProfile,
    pub reason_codes: Vec<String>,
    pub abstention_reasons: Vec<String>,
    pub next_commands: Vec<String>,
}

/// Evaluate resource-profile admission as a pure shadow candidate.
#[must_use]
pub fn evaluate_resource_profile_budget_admission(
    input: ResourceAdmissionInput,
) -> ResourceAdmissionReport {
    let mut reason_codes = resource_admission_reason_codes(&input);
    let abstention_reasons = resource_admission_abstention_reasons(&input);
    let mut next_commands = Vec::new();

    let decision = if !abstention_reasons.is_empty() {
        push_unique(
            &mut next_commands,
            "ee support bundle --include resource-admission --json",
        );
        ResourceAdmissionDecision::Abstain
    } else if input.local_cargo == ResourceLocalCargoPosture::Unsafe {
        push_unique(&mut reason_codes, "local_cargo_refused");
        push_unique(
            &mut next_commands,
            "scripts/check-local-cargo-tripwire.sh --probe-processes --json",
        );
        if input.rch == ResourceRchPosture::RemoteReady {
            push_unique(&mut next_commands, "scripts/rch_verify.sh -- <command>");
        }
        ResourceAdmissionDecision::RefuseLocalCargo
    } else if rch_requires_wait(input.rch) {
        push_unique(&mut next_commands, "rch status --json");
        ResourceAdmissionDecision::WaitForRch
    } else if should_degrade_to_lean(&input) {
        push_unique(
            &mut next_commands,
            "ee pack <task> --resource-profile constrained --json",
        );
        ResourceAdmissionDecision::DegradeToLean
    } else if should_split_workload(&input) {
        push_unique(&mut reason_codes, "conservative_profile_ceiling");
        push_unique(
            &mut next_commands,
            "ee lab swarm replay --split-workload --dry-run --json",
        );
        ResourceAdmissionDecision::SplitWorkload
    } else if should_queue(&input) {
        push_unique(&mut next_commands, "ee swarm brief --json");
        ResourceAdmissionDecision::Queue
    } else {
        ResourceAdmissionDecision::Admit
    };

    let recommended_profile = match decision {
        ResourceAdmissionDecision::DegradeToLean => ResourceOperatingProfile::Constrained,
        _ => input.effective_profile,
    };

    ResourceAdmissionReport {
        schema: RESOURCE_ADMISSION_SCHEMA_V1,
        policy_domain: RESOURCE_ADMISSION_POLICY_DOMAIN,
        policy_id: RESOURCE_ADMISSION_CANDIDATE_POLICY_ID,
        side_effect_free: true,
        advisory_only: true,
        decision,
        requested_profile: input.requested_profile,
        effective_profile: input.effective_profile,
        recommended_profile,
        reason_codes,
        abstention_reasons,
        next_commands,
    }
}

fn resource_admission_reason_codes(input: &ResourceAdmissionInput) -> Vec<String> {
    let mut reason_codes = Vec::new();
    push_unique(
        &mut reason_codes,
        host_calibration_reason(input.host_calibration),
    );
    push_unique(
        &mut reason_codes,
        resource_budget_reason(input.resource_budget),
    );
    push_unique(&mut reason_codes, rch_reason(input.rch));
    push_unique(&mut reason_codes, local_cargo_reason(input.local_cargo));
    push_unique(&mut reason_codes, lane_pressure_reason(input.lane_pressure));
    push_unique(
        &mut reason_codes,
        workload_pressure_reason(input.workload_pressure),
    );
    push_unique(&mut reason_codes, daemon_reason(input.daemon));
    push_unique(&mut reason_codes, replay_reason(input.replay));
    push_unique(
        &mut reason_codes,
        if input.redaction_posture_verified {
            "redaction_posture_verified"
        } else {
            "redaction_posture_unknown"
        },
    );
    reason_codes
}

fn resource_admission_abstention_reasons(input: &ResourceAdmissionInput) -> Vec<String> {
    let mut reasons = Vec::new();

    match input.host_calibration {
        ResourceHostCalibrationPosture::Missing | ResourceHostCalibrationPosture::Unavailable => {
            push_unique(&mut reasons, "missing_required_signal");
        }
        ResourceHostCalibrationPosture::Stale => {
            push_unique(&mut reasons, "stale_source_authority");
        }
        ResourceHostCalibrationPosture::Contradictory => {
            push_unique(&mut reasons, "contradictory_evidence");
        }
        ResourceHostCalibrationPosture::Fresh
        | ResourceHostCalibrationPosture::Partial
        | ResourceHostCalibrationPosture::SyntheticOnly
        | ResourceHostCalibrationPosture::NotApplicable => {}
    }

    match input.resource_budget {
        ResourceBudgetPosture::Missing => {
            push_unique(&mut reasons, "missing_required_signal");
        }
        ResourceBudgetPosture::Contradictory => {
            push_unique(&mut reasons, "contradictory_evidence");
        }
        ResourceBudgetPosture::WithinBudget
        | ResourceBudgetPosture::RecommendDecrease
        | ResourceBudgetPosture::RecommendIncrease
        | ResourceBudgetPosture::OverrideClamped => {}
    }

    if input.rch == ResourceRchPosture::Unknown {
        push_unique(&mut reasons, "missing_required_signal");
    }

    if input.local_cargo == ResourceLocalCargoPosture::Unknown {
        push_unique(&mut reasons, "missing_required_signal");
    }

    match input.replay {
        ResourceReplayPosture::Stale => {
            push_unique(&mut reasons, "stale_source_authority");
        }
        ResourceReplayPosture::Missing
            if input.estimated_cost_class == ResourceCostClass::Unknown =>
        {
            push_unique(&mut reasons, "missing_required_signal");
        }
        ResourceReplayPosture::Healthy
        | ResourceReplayPosture::Regression
        | ResourceReplayPosture::Missing
        | ResourceReplayPosture::NotRequired
        | ResourceReplayPosture::Unknown => {}
    }

    if !input.redaction_posture_verified {
        push_unique(&mut reasons, "redaction_posture_unknown");
    }

    reasons
}

fn should_degrade_to_lean(input: &ResourceAdmissionInput) -> bool {
    matches!(
        input.resource_budget,
        ResourceBudgetPosture::RecommendDecrease | ResourceBudgetPosture::OverrideClamped
    ) || matches!(
        input.host_calibration,
        ResourceHostCalibrationPosture::Partial | ResourceHostCalibrationPosture::SyntheticOnly
    ) || matches!(
        input.workload_pressure,
        ResourceWorkloadPressurePosture::CachePressure
            | ResourceWorkloadPressurePosture::WriteSpoolPressure
            | ResourceWorkloadPressurePosture::ReadPoolPressure
            | ResourceWorkloadPressurePosture::PackSloPressure
            | ResourceWorkloadPressurePosture::IndexPressure
            | ResourceWorkloadPressurePosture::GraphPressure
            | ResourceWorkloadPressurePosture::MixedPressure
    ) || input.replay == ResourceReplayPosture::Regression
}

fn should_split_workload(input: &ResourceAdmissionInput) -> bool {
    input.estimated_cost_class == ResourceCostClass::SwarmHeavy
        && input.effective_profile != ResourceOperatingProfile::Swarm
        && input.rch == ResourceRchPosture::NotRequired
}

fn should_queue(input: &ResourceAdmissionInput) -> bool {
    matches!(
        input.lane_pressure,
        ResourceLanePressurePosture::BackgroundPressure
            | ResourceLanePressurePosture::MaintenancePressure
            | ResourceLanePressurePosture::MixedPressure
    )
}

fn rch_requires_wait(posture: ResourceRchPosture) -> bool {
    matches!(
        posture,
        ResourceRchPosture::ActiveProjectExclusion
            | ResourceRchPosture::ProgressStale
            | ResourceRchPosture::Blocked
    )
}

fn host_calibration_reason(posture: ResourceHostCalibrationPosture) -> &'static str {
    match posture {
        ResourceHostCalibrationPosture::Fresh => "host_calibration_fresh",
        ResourceHostCalibrationPosture::Stale => "host_calibration_stale",
        ResourceHostCalibrationPosture::Partial => "host_calibration_partial",
        ResourceHostCalibrationPosture::SyntheticOnly => "host_calibration_synthetic_only",
        ResourceHostCalibrationPosture::Contradictory => "host_calibration_contradictory",
        ResourceHostCalibrationPosture::Missing => "host_calibration_missing",
        ResourceHostCalibrationPosture::Unavailable => "host_calibration_unavailable",
        ResourceHostCalibrationPosture::NotApplicable => "host_calibration_unavailable",
    }
}

fn resource_budget_reason(posture: ResourceBudgetPosture) -> &'static str {
    match posture {
        ResourceBudgetPosture::WithinBudget => "budget_within_profile",
        ResourceBudgetPosture::RecommendDecrease => "budget_delta_recommends_decrease",
        ResourceBudgetPosture::RecommendIncrease => "budget_delta_recommends_increase",
        ResourceBudgetPosture::OverrideClamped => "budget_override_clamped",
        ResourceBudgetPosture::Missing => "missing_required_signal",
        ResourceBudgetPosture::Contradictory => "contradictory_evidence",
    }
}

fn rch_reason(posture: ResourceRchPosture) -> &'static str {
    match posture {
        ResourceRchPosture::RemoteReady => "rch_remote_ready",
        ResourceRchPosture::ActiveProjectExclusion => "rch_active_project_exclusion",
        ResourceRchPosture::ProgressStale => "rch_progress_stale",
        ResourceRchPosture::Blocked => "rch_blocked",
        ResourceRchPosture::NotRequired => "rch_not_required",
        ResourceRchPosture::Unknown => "rch_blocked",
    }
}

fn local_cargo_reason(posture: ResourceLocalCargoPosture) -> &'static str {
    match posture {
        ResourceLocalCargoPosture::Clean => "local_cargo_clean",
        ResourceLocalCargoPosture::Refused | ResourceLocalCargoPosture::Unsafe => {
            "local_cargo_refused"
        }
        ResourceLocalCargoPosture::Unknown => "local_cargo_unsafe",
        ResourceLocalCargoPosture::NotRequired => "local_cargo_clean",
    }
}

fn lane_pressure_reason(posture: ResourceLanePressurePosture) -> &'static str {
    match posture {
        ResourceLanePressurePosture::Clear => "lane_pressure_clear",
        ResourceLanePressurePosture::ForegroundPressure => "lane_pressure_foreground",
        ResourceLanePressurePosture::BackgroundPressure => "lane_pressure_background",
        ResourceLanePressurePosture::VerificationPressure => "lane_pressure_verification",
        ResourceLanePressurePosture::MaintenancePressure => "lane_pressure_maintenance",
        ResourceLanePressurePosture::MixedPressure => "lane_pressure_background",
        ResourceLanePressurePosture::Unknown => "lane_pressure_background",
    }
}

fn workload_pressure_reason(posture: ResourceWorkloadPressurePosture) -> &'static str {
    match posture {
        ResourceWorkloadPressurePosture::WithinBudget => "budget_within_profile",
        ResourceWorkloadPressurePosture::CachePressure => "cache_pressure",
        ResourceWorkloadPressurePosture::WriteSpoolPressure => "write_spool_pressure",
        ResourceWorkloadPressurePosture::ReadPoolPressure => "read_pool_pressure",
        ResourceWorkloadPressurePosture::PackSloPressure => "pack_slo_pressure",
        ResourceWorkloadPressurePosture::IndexPressure => "index_pressure",
        ResourceWorkloadPressurePosture::GraphPressure => "graph_pressure",
        ResourceWorkloadPressurePosture::MixedPressure => "cache_pressure",
        ResourceWorkloadPressurePosture::Unknown => "missing_required_signal",
    }
}

fn daemon_reason(posture: ResourceDaemonPosture) -> &'static str {
    match posture {
        ResourceDaemonPosture::Available => "daemon_available",
        ResourceDaemonPosture::Unavailable => "daemon_degraded",
        ResourceDaemonPosture::Degraded => "daemon_degraded",
        ResourceDaemonPosture::NotRequired => "daemon_not_required",
        ResourceDaemonPosture::Unknown => "daemon_degraded",
    }
}

fn replay_reason(posture: ResourceReplayPosture) -> &'static str {
    match posture {
        ResourceReplayPosture::Healthy => "replay_slo_healthy",
        ResourceReplayPosture::Regression => "replay_slo_regression",
        ResourceReplayPosture::Stale => "replay_slo_stale",
        ResourceReplayPosture::Missing => "missing_required_signal",
        ResourceReplayPosture::NotRequired => "replay_slo_healthy",
        ResourceReplayPosture::Unknown => "missing_required_signal",
    }
}

/// Verdict from comparing incumbent and candidate outputs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShadowVerdict {
    /// Outputs are equivalent.
    Equivalent,
    /// Candidate produces better results.
    CandidateBetter,
    /// Incumbent produces better results.
    IncumbentBetter,
    /// Outputs differ but neither is clearly better.
    Divergent,
    /// Comparison could not be performed.
    Inconclusive,
}

impl ShadowVerdict {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Equivalent => "equivalent",
            Self::CandidateBetter => "candidate_better",
            Self::IncumbentBetter => "incumbent_better",
            Self::Divergent => "divergent",
            Self::Inconclusive => "inconclusive",
        }
    }

    #[must_use]
    pub const fn is_safe_to_promote(self) -> bool {
        matches!(self, Self::Equivalent | Self::CandidateBetter)
    }
}

impl fmt::Display for ShadowVerdict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Safety guards that block policy promotion even when aggregate metrics improve.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ShadowPromotionGuards {
    /// Candidate dropped a critical warning the incumbent kept.
    pub dropped_critical_warnings: bool,
    /// Candidate changed redaction posture or leaked a previously redacted field.
    pub redaction_differences: bool,
    /// Candidate regressed p99/tail latency beyond tolerance.
    pub p99_regression: bool,
    /// Candidate regressed catastrophic/tail-risk handling.
    pub tail_risk_regression: bool,
    /// Candidate mismatch exceeded the configured semantic tolerance.
    pub shadow_mismatch_above_tolerance: bool,
}

impl ShadowPromotionGuards {
    #[must_use]
    pub const fn blocks_promotion(&self) -> bool {
        self.dropped_critical_warnings
            || self.redaction_differences
            || self.p99_regression
            || self.tail_risk_regression
            || self.shadow_mismatch_above_tolerance
    }

    #[must_use]
    pub fn blocker_codes(&self) -> Vec<&'static str> {
        let mut codes = Vec::new();
        if self.dropped_critical_warnings {
            codes.push("dropped_critical_warnings");
        }
        if self.redaction_differences {
            codes.push("redaction_differences");
        }
        if self.p99_regression {
            codes.push("p99_regression");
        }
        if self.tail_risk_regression {
            codes.push("tail_risk_regression");
        }
        if self.shadow_mismatch_above_tolerance {
            codes.push("shadow_mismatch_above_tolerance");
        }
        codes
    }
}

#[must_use]
pub fn candidate_promotion_allowed(verdict: ShadowVerdict, guards: &ShadowPromotionGuards) -> bool {
    verdict.is_safe_to_promote() && !guards.blocks_promotion()
}

/// Comparison metrics from a shadow run.
#[derive(Clone, Debug, Default)]
pub struct ShadowMetrics {
    /// Size of incumbent output (items, bytes, etc.).
    pub incumbent_size: u64,
    /// Size of candidate output.
    pub candidate_size: u64,
    /// Quality score for incumbent (domain-specific).
    pub incumbent_quality: f64,
    /// Quality score for candidate.
    pub candidate_quality: f64,
    /// Overlap ratio between outputs (0.0 to 1.0).
    pub overlap_ratio: f64,
    /// Incumbent execution time in microseconds.
    pub incumbent_time_us: u64,
    /// Candidate execution time in microseconds.
    pub candidate_time_us: u64,
}

impl ShadowMetrics {
    #[must_use]
    pub fn quality_delta(&self) -> f64 {
        self.candidate_quality - self.incumbent_quality
    }

    #[must_use]
    pub fn size_delta(&self) -> i64 {
        signed_u64_delta(self.candidate_size, self.incumbent_size)
    }

    #[must_use]
    pub fn time_delta_us(&self) -> i64 {
        signed_u64_delta(self.candidate_time_us, self.incumbent_time_us)
    }
}

/// Fixture or replay artifact kind used by a shadow-policy score.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShadowEvidenceKind {
    /// Deterministic workload replay trace.
    ReplayTrace,
    /// Checked-in golden fixture.
    GoldenFixture,
    /// Operator-provided manual fixture.
    ManualFixture,
}

impl ShadowEvidenceKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ReplayTrace => "replay_trace",
            Self::GoldenFixture => "golden_fixture",
            Self::ManualFixture => "manual_fixture",
        }
    }
}

/// Evidence availability for one artifact in a shadow-policy score.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShadowEvidencePosture {
    /// Evidence is current and usable for scoring.
    Present,
    /// Required evidence is absent.
    Missing,
    /// Evidence exists but is stale relative to the source authority.
    Stale,
    /// The policy domain cannot consume this artifact.
    Unsupported,
}

impl ShadowEvidencePosture {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Present => "present",
            Self::Missing => "missing",
            Self::Stale => "stale",
            Self::Unsupported => "unsupported",
        }
    }

    #[must_use]
    pub const fn is_usable(self) -> bool {
        matches!(self, Self::Present)
    }
}

/// Aggregate scoring verdict over a deterministic evidence cohort.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShadowPolicyScoreVerdict {
    /// Candidate improves the utility proxy without safety regressions.
    Improves,
    /// Candidate regresses utility, degraded codes, redaction, or required evidence.
    Regresses,
    /// Candidate is materially flat against the incumbent.
    Flat,
    /// Available evidence is not sufficient to score the candidate.
    NeedsMoreEvidence,
}

impl ShadowPolicyScoreVerdict {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Improves => "improves",
            Self::Regresses => "regresses",
            Self::Flat => "flat",
            Self::NeedsMoreEvidence => "needs_more_evidence",
        }
    }
}

/// Conservative promotion decision derived from a scored shadow-policy cohort.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShadowPolicyPromotionVerdict {
    /// Candidate is eligible for a later explicit apply step.
    Promote,
    /// Candidate is not worse, but evidence or SLO posture is too weak to promote.
    Hold,
    /// Candidate regressed a hard safety or utility requirement.
    Reject,
    /// Evidence/source authority is not sufficient to judge the candidate.
    Abstain,
}

impl ShadowPolicyPromotionVerdict {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Promote => "promote",
            Self::Hold => "hold",
            Self::Reject => "reject",
            Self::Abstain => "abstain",
        }
    }
}

/// One deterministic replay/golden artifact scored for a candidate policy.
#[derive(Clone, Debug)]
pub struct ShadowPolicyEvidencePoint {
    pub artifact_id: String,
    pub kind: ShadowEvidenceKind,
    pub cohort: String,
    pub posture: ShadowEvidencePosture,
    pub incumbent_utility: f64,
    pub candidate_utility: f64,
    pub incumbent_output_bytes: u64,
    pub candidate_output_bytes: u64,
    pub candidate_latency_ms: u64,
    pub incumbent_degraded_count: u32,
    pub candidate_degraded_count: u32,
    pub dropped_required_evidence_count: u32,
    pub resource_bytes_delta: i64,
    pub redaction_safe: bool,
}

impl ShadowPolicyEvidencePoint {
    #[must_use]
    pub fn utility_delta(&self) -> f64 {
        self.candidate_utility - self.incumbent_utility
    }

    #[must_use]
    pub fn output_bytes_delta(&self) -> i64 {
        signed_u64_delta(self.candidate_output_bytes, self.incumbent_output_bytes)
    }

    #[must_use]
    pub fn degraded_delta(&self) -> i64 {
        i64::from(self.candidate_degraded_count) - i64::from(self.incumbent_degraded_count)
    }

    #[must_use]
    pub fn diverged(&self) -> bool {
        self.utility_delta().abs() >= 0.0001
            || self.output_bytes_delta() != 0
            || self.degraded_delta() != 0
            || self.dropped_required_evidence_count > 0
            || !self.redaction_safe
    }
}

/// Thresholds for deterministic policy scoring.
#[derive(Clone, Copy, Debug)]
pub struct ShadowPolicyScoringConfig {
    pub min_utility_delta_for_improvement: f64,
    pub max_utility_delta_for_flat: f64,
}

impl Default for ShadowPolicyScoringConfig {
    fn default() -> Self {
        Self {
            min_utility_delta_for_improvement: 0.05,
            max_utility_delta_for_flat: 0.01,
        }
    }
}

/// Conservative thresholds for converting a shadow score into a promotion verdict.
#[derive(Clone, Copy, Debug)]
pub struct ShadowPolicyPromotionConfig {
    pub min_usable_evidence_for_promotion: u32,
    pub max_p99_latency_ms_for_promotion: u64,
    pub max_resource_bytes_regression_for_promotion: i64,
}

impl Default for ShadowPolicyPromotionConfig {
    fn default() -> Self {
        Self {
            min_usable_evidence_for_promotion: 2,
            max_p99_latency_ms_for_promotion: 250,
            max_resource_bytes_regression_for_promotion: 0,
        }
    }
}

/// Source and verification posture required before a shadow score can promote.
#[derive(Clone, Copy, Debug)]
pub struct ShadowPolicyPromotionPosture {
    pub source_authority_current: bool,
    pub rch_admitted: bool,
    pub local_cargo_clean: bool,
}

impl Default for ShadowPolicyPromotionPosture {
    fn default() -> Self {
        Self {
            source_authority_current: true,
            rch_admitted: true,
            local_cargo_clean: true,
        }
    }
}

/// Stable summary for a scored policy cohort.
#[derive(Clone, Debug, Default)]
pub struct ShadowPolicyScoreSummary {
    pub total_evidence: u32,
    pub usable_evidence: u32,
    pub missing_evidence: u32,
    pub stale_evidence: u32,
    pub unsupported_evidence: u32,
    pub diverged_evidence: u32,
    pub divergence_rate: f64,
    pub utility_delta: f64,
    pub dropped_required_evidence_count: u32,
    pub output_bytes_delta: i64,
    pub p50_latency_ms: u64,
    pub p95_latency_ms: u64,
    pub p99_latency_ms: u64,
    pub degraded_delta: i64,
    pub resource_bytes_delta: i64,
    pub redaction_safe: bool,
}

/// Read-only deterministic score for one candidate policy over one cohort.
#[derive(Clone, Debug)]
pub struct ShadowPolicyScoreReport {
    pub schema: &'static str,
    pub policy_domain: String,
    pub incumbent_policy_id: String,
    pub candidate_policy_id: String,
    pub cohort_profile: String,
    pub side_effect_free: bool,
    pub summary: ShadowPolicyScoreSummary,
    pub verdict: ShadowPolicyScoreVerdict,
    pub abstention_reasons: Vec<String>,
    pub evidence: Vec<ShadowPolicyEvidencePoint>,
}

/// Read-only promotion verdict for a scored shadow-policy cohort.
#[derive(Clone, Debug)]
pub struct ShadowPolicyPromotionReport {
    pub schema: &'static str,
    pub score_schema: &'static str,
    pub policy_domain: String,
    pub incumbent_policy_id: String,
    pub candidate_policy_id: String,
    pub cohort_profile: String,
    pub side_effect_free: bool,
    pub operator_warning: &'static str,
    pub summary: ShadowPolicyScoreSummary,
    pub score_verdict: ShadowPolicyScoreVerdict,
    pub verdict: ShadowPolicyPromotionVerdict,
    pub confidence: f64,
    pub reason_codes: Vec<String>,
    pub abstention_reasons: Vec<String>,
    pub safety_guards_triggered: Vec<String>,
    pub counter_evidence: Vec<String>,
    pub next_commands: Vec<String>,
}

#[must_use]
pub fn score_shadow_policy_cohort(
    policy_domain: &str,
    incumbent_policy_id: &str,
    candidate_policy_id: &str,
    cohort_profile: &str,
    evidence: Vec<ShadowPolicyEvidencePoint>,
    config: ShadowPolicyScoringConfig,
) -> ShadowPolicyScoreReport {
    let mut summary = ShadowPolicyScoreSummary {
        total_evidence: saturating_u32(evidence.len()),
        redaction_safe: true,
        ..ShadowPolicyScoreSummary::default()
    };

    let mut utility_delta_sum = 0.0;
    let mut latencies = Vec::new();

    for point in &evidence {
        match point.posture {
            ShadowEvidencePosture::Present => {
                summary.usable_evidence = summary.usable_evidence.saturating_add(1);
                if point.diverged() {
                    summary.diverged_evidence = summary.diverged_evidence.saturating_add(1);
                }
                utility_delta_sum += point.utility_delta();
                summary.dropped_required_evidence_count = summary
                    .dropped_required_evidence_count
                    .saturating_add(point.dropped_required_evidence_count);
                summary.output_bytes_delta = summary
                    .output_bytes_delta
                    .saturating_add(point.output_bytes_delta());
                summary.degraded_delta = summary
                    .degraded_delta
                    .saturating_add(point.degraded_delta());
                summary.resource_bytes_delta = summary
                    .resource_bytes_delta
                    .saturating_add(point.resource_bytes_delta);
                summary.redaction_safe &= point.redaction_safe;
                latencies.push(point.candidate_latency_ms);
            }
            ShadowEvidencePosture::Missing => {
                summary.missing_evidence = summary.missing_evidence.saturating_add(1);
            }
            ShadowEvidencePosture::Stale => {
                summary.stale_evidence = summary.stale_evidence.saturating_add(1);
            }
            ShadowEvidencePosture::Unsupported => {
                summary.unsupported_evidence = summary.unsupported_evidence.saturating_add(1);
            }
        }
    }

    if summary.usable_evidence > 0 {
        let usable = f64::from(summary.usable_evidence);
        summary.utility_delta = utility_delta_sum / usable;
        summary.divergence_rate = f64::from(summary.diverged_evidence) / usable;
        latencies.sort_unstable();
        summary.p50_latency_ms = percentile_nearest_rank(&latencies, 50);
        summary.p95_latency_ms = percentile_nearest_rank(&latencies, 95);
        summary.p99_latency_ms = percentile_nearest_rank(&latencies, 99);
    }

    let mut abstention_reasons = Vec::new();
    let verdict = if summary.usable_evidence == 0 {
        if summary.missing_evidence > 0 {
            abstention_reasons.push("missing_replay_evidence".to_string());
        }
        if summary.stale_evidence > 0 {
            abstention_reasons.push("stale_source_authority".to_string());
        }
        if summary.unsupported_evidence > 0 {
            abstention_reasons.push("unsupported_policy_domain".to_string());
        }
        ShadowPolicyScoreVerdict::NeedsMoreEvidence
    } else if summary.dropped_required_evidence_count > 0
        || summary.degraded_delta > 0
        || !summary.redaction_safe
        || summary.utility_delta < -config.max_utility_delta_for_flat
    {
        ShadowPolicyScoreVerdict::Regresses
    } else if summary.utility_delta >= config.min_utility_delta_for_improvement {
        ShadowPolicyScoreVerdict::Improves
    } else {
        ShadowPolicyScoreVerdict::Flat
    };

    ShadowPolicyScoreReport {
        schema: SHADOW_POLICY_SCORE_SCHEMA_V1,
        policy_domain: policy_domain.to_string(),
        incumbent_policy_id: incumbent_policy_id.to_string(),
        candidate_policy_id: candidate_policy_id.to_string(),
        cohort_profile: cohort_profile.to_string(),
        side_effect_free: true,
        summary,
        verdict,
        abstention_reasons,
        evidence,
    }
}

#[must_use]
pub fn promote_shadow_policy_from_score(
    score: &ShadowPolicyScoreReport,
    config: ShadowPolicyPromotionConfig,
    posture: ShadowPolicyPromotionPosture,
) -> ShadowPolicyPromotionReport {
    let mut reason_codes = Vec::new();
    let mut abstention_reasons = Vec::new();
    let mut safety_guards_triggered = Vec::new();
    let mut counter_evidence = Vec::new();
    let mut next_commands = Vec::new();

    if !posture.source_authority_current {
        push_unique(&mut reason_codes, "source_authority_not_current");
        push_unique(&mut abstention_reasons, "stale_source_authority");
        push_unique(&mut next_commands, "ee diag environment-attestation --json");
    }
    if !posture.rch_admitted {
        push_unique(&mut reason_codes, "rch_not_admitted");
        push_unique(&mut abstention_reasons, "rch_blocked");
        push_unique(
            &mut next_commands,
            "scripts/rch_verify.sh -- cargo test --test contracts shadow_run -- --nocapture",
        );
    }
    if !posture.local_cargo_clean {
        push_unique(&mut reason_codes, "local_cargo_tripwire_not_clean");
        push_unique(&mut abstention_reasons, "unsafe_local_cargo_posture");
        push_unique(
            &mut next_commands,
            "scripts/check-local-cargo-tripwire.sh --probe-processes --json",
        );
    }

    if score.verdict == ShadowPolicyScoreVerdict::NeedsMoreEvidence {
        push_unique(&mut reason_codes, "score_needs_more_evidence");
        if score.abstention_reasons.is_empty() {
            push_unique(&mut abstention_reasons, "missing_replay_evidence");
        } else {
            for reason in &score.abstention_reasons {
                push_unique(&mut abstention_reasons, reason);
            }
        }
        push_unique(
            &mut next_commands,
            "ee lab swarm replay --trace <workload.json> --dry-run --json",
        );
    }

    let verdict = if !abstention_reasons.is_empty() {
        ShadowPolicyPromotionVerdict::Abstain
    } else {
        match score.verdict {
            ShadowPolicyScoreVerdict::Improves => promotion_verdict_for_improving_score(
                score,
                config,
                &mut reason_codes,
                &mut safety_guards_triggered,
                &mut counter_evidence,
                &mut next_commands,
            ),
            ShadowPolicyScoreVerdict::Regresses => {
                push_regression_reasons(score, &mut reason_codes, &mut safety_guards_triggered);
                push_regressing_evidence(score, &mut counter_evidence);
                push_unique(&mut next_commands, "ee why candidate-policy --json");
                ShadowPolicyPromotionVerdict::Reject
            }
            ShadowPolicyScoreVerdict::Flat => {
                push_unique(&mut reason_codes, "candidate_flat");
                push_unique(
                    &mut next_commands,
                    "ee pack diff <old-pack-id> <new-pack-id> --json",
                );
                ShadowPolicyPromotionVerdict::Hold
            }
            ShadowPolicyScoreVerdict::NeedsMoreEvidence => ShadowPolicyPromotionVerdict::Abstain,
        }
    };

    let confidence = promotion_confidence(score, verdict, config);

    ShadowPolicyPromotionReport {
        schema: SHADOW_POLICY_VERDICT_SCHEMA_V1,
        score_schema: score.schema,
        policy_domain: score.policy_domain.clone(),
        incumbent_policy_id: score.incumbent_policy_id.clone(),
        candidate_policy_id: score.candidate_policy_id.clone(),
        cohort_profile: score.cohort_profile.clone(),
        side_effect_free: true,
        operator_warning: SHADOW_POLICY_OPERATOR_WARNING,
        summary: score.summary.clone(),
        score_verdict: score.verdict,
        verdict,
        confidence,
        reason_codes,
        abstention_reasons,
        safety_guards_triggered,
        counter_evidence,
        next_commands,
    }
}

fn promotion_verdict_for_improving_score(
    score: &ShadowPolicyScoreReport,
    config: ShadowPolicyPromotionConfig,
    reason_codes: &mut Vec<String>,
    safety_guards_triggered: &mut Vec<String>,
    counter_evidence: &mut Vec<String>,
    next_commands: &mut Vec<String>,
) -> ShadowPolicyPromotionVerdict {
    if score.summary.usable_evidence < config.min_usable_evidence_for_promotion {
        push_unique(reason_codes, "insufficient_evidence_count");
        push_unique(
            next_commands,
            "ee lab swarm replay --trace <workload.json> --dry-run --json",
        );
        return ShadowPolicyPromotionVerdict::Hold;
    }
    if score.summary.p99_latency_ms > config.max_p99_latency_ms_for_promotion {
        push_unique(reason_codes, "p99_regression");
        push_unique(safety_guards_triggered, "p99_regression");
        push_slow_evidence(score, counter_evidence);
        push_unique(
            next_commands,
            "ee lab swarm replay --trace <workload.json> --dry-run --json",
        );
        return ShadowPolicyPromotionVerdict::Hold;
    }
    if score.summary.resource_bytes_delta > config.max_resource_bytes_regression_for_promotion {
        push_unique(reason_codes, "resource_regression");
        push_unique(safety_guards_triggered, "resource_regression");
        push_resource_regressing_evidence(score, counter_evidence);
        push_unique(next_commands, "ee support bundle --out <dir> --json");
        return ShadowPolicyPromotionVerdict::Hold;
    }

    push_unique(reason_codes, "score_improves");
    push_unique(reason_codes, "promotion_thresholds_satisfied");
    ShadowPolicyPromotionVerdict::Promote
}

fn push_regression_reasons(
    score: &ShadowPolicyScoreReport,
    reason_codes: &mut Vec<String>,
    safety_guards_triggered: &mut Vec<String>,
) {
    if score.summary.dropped_required_evidence_count > 0 {
        push_unique(reason_codes, "required_evidence_dropped");
        push_unique(safety_guards_triggered, "dropped_required_evidence");
    }
    if score.summary.degraded_delta > 0 {
        push_unique(reason_codes, "degraded_delta_regression");
        push_unique(safety_guards_triggered, "degraded_delta_regression");
    }
    if !score.summary.redaction_safe {
        push_unique(reason_codes, "redaction_regression");
        push_unique(safety_guards_triggered, "redaction_regression");
    }
    if score.summary.utility_delta < 0.0 {
        push_unique(reason_codes, "utility_regression");
    }
}

fn push_regressing_evidence(score: &ShadowPolicyScoreReport, counter_evidence: &mut Vec<String>) {
    for point in &score.evidence {
        if point.posture.is_usable()
            && (point.utility_delta() < 0.0
                || point.degraded_delta() > 0
                || point.dropped_required_evidence_count > 0
                || !point.redaction_safe)
        {
            push_unique(counter_evidence, &point.artifact_id);
        }
    }
}

fn push_slow_evidence(score: &ShadowPolicyScoreReport, counter_evidence: &mut Vec<String>) {
    for point in &score.evidence {
        if point.posture.is_usable()
            && point.candidate_latency_ms >= score.summary.p99_latency_ms
            && score.summary.p99_latency_ms > 0
        {
            push_unique(counter_evidence, &point.artifact_id);
        }
    }
}

fn push_resource_regressing_evidence(
    score: &ShadowPolicyScoreReport,
    counter_evidence: &mut Vec<String>,
) {
    for point in &score.evidence {
        if point.posture.is_usable() && point.resource_bytes_delta > 0 {
            push_unique(counter_evidence, &point.artifact_id);
        }
    }
}

fn push_unique(values: &mut Vec<String>, value: &str) {
    if !values.iter().any(|existing| existing == value) {
        values.push(value.to_string());
    }
}

fn promotion_confidence(
    score: &ShadowPolicyScoreReport,
    verdict: ShadowPolicyPromotionVerdict,
    config: ShadowPolicyPromotionConfig,
) -> f64 {
    let required = config.min_usable_evidence_for_promotion.max(1);
    let evidence_confidence =
        (f64::from(score.summary.usable_evidence) / f64::from(required)).min(1.0);
    let utility_confidence = score.summary.utility_delta.abs().min(1.0);
    match verdict {
        ShadowPolicyPromotionVerdict::Promote => {
            (0.70 + (0.20 * evidence_confidence) + (0.10 * utility_confidence)).min(1.0)
        }
        ShadowPolicyPromotionVerdict::Hold => 0.55 * evidence_confidence,
        ShadowPolicyPromotionVerdict::Reject => {
            (0.75 + (0.10 * evidence_confidence) + (0.05 * utility_confidence)).min(1.0)
        }
        ShadowPolicyPromotionVerdict::Abstain => 0.0,
    }
}

fn saturating_u32(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

fn percentile_nearest_rank(sorted_values: &[u64], percentile: u32) -> u64 {
    if sorted_values.is_empty() {
        return 0;
    }
    let len = sorted_values.len();
    let numerator = (len * percentile as usize).saturating_add(99);
    let mut index = numerator / 100;
    index = index.saturating_sub(1);
    sorted_values[index.min(len - 1)]
}

#[must_use]
pub fn render_shadow_policy_score_json(report: &ShadowPolicyScoreReport) -> String {
    let mut out = String::new();
    out.push('{');
    json_str_field(&mut out, "schema", report.schema);
    out.push(',');
    json_str_field(&mut out, "policyDomain", &report.policy_domain);
    out.push(',');
    json_str_field(&mut out, "incumbentPolicyId", &report.incumbent_policy_id);
    out.push(',');
    json_str_field(&mut out, "candidatePolicyId", &report.candidate_policy_id);
    out.push(',');
    json_str_field(&mut out, "cohortProfile", &report.cohort_profile);
    out.push_str(",\"sideEffectFree\":true,");
    out.push_str("\"verdict\":");
    push_json_string(&mut out, report.verdict.as_str());
    out.push_str(",\"abstentionReasons\":[");
    for (idx, reason) in report.abstention_reasons.iter().enumerate() {
        if idx > 0 {
            out.push(',');
        }
        push_json_string(&mut out, reason);
    }
    out.push_str("],\"summary\":{");
    push_summary_json(&mut out, &report.summary);
    out.push_str("},\"redactionPosture\":{");
    out.push_str("\"rawMemoryBodyPresent\":false,");
    out.push_str("\"rawMailBodyPresent\":false,");
    out.push_str("\"rawPolicyPayloadPresent\":false,");
    out.push_str("\"absoluteHostPathPresent\":false,");
    out.push_str("\"secretsPresent\":false");
    out.push_str("},\"evidence\":[");
    for (idx, point) in report.evidence.iter().enumerate() {
        if idx > 0 {
            out.push(',');
        }
        push_evidence_point_json(&mut out, point);
    }
    out.push_str("]}");
    out
}

#[must_use]
pub fn render_shadow_policy_promotion_json(report: &ShadowPolicyPromotionReport) -> String {
    let mut out = String::new();
    out.push('{');
    json_str_field(&mut out, "schema", report.schema);
    out.push(',');
    json_str_field(&mut out, "scoreSchema", report.score_schema);
    out.push(',');
    json_str_field(&mut out, "policyDomain", &report.policy_domain);
    out.push(',');
    json_str_field(&mut out, "incumbentPolicyId", &report.incumbent_policy_id);
    out.push(',');
    json_str_field(&mut out, "candidatePolicyId", &report.candidate_policy_id);
    out.push(',');
    json_str_field(&mut out, "cohortProfile", &report.cohort_profile);
    out.push_str(",\"sideEffectFree\":true,");
    json_str_field(&mut out, "operatorWarning", report.operator_warning);
    out.push_str(",\"scoreVerdict\":");
    push_json_string(&mut out, report.score_verdict.as_str());
    out.push_str(",\"verdict\":");
    push_json_string(&mut out, report.verdict.as_str());
    out.push(',');
    push_f64_field(&mut out, "confidence", report.confidence);
    out.push_str(",\"reasonCodes\":[");
    push_string_array(&mut out, &report.reason_codes);
    out.push_str("],\"abstentionReasons\":[");
    push_string_array(&mut out, &report.abstention_reasons);
    out.push_str("],\"safetyGuardsTriggered\":[");
    push_string_array(&mut out, &report.safety_guards_triggered);
    out.push_str("],\"counterEvidence\":[");
    push_string_array(&mut out, &report.counter_evidence);
    out.push_str("],\"nextCommands\":[");
    push_string_array(&mut out, &report.next_commands);
    out.push_str("],\"summary\":{");
    push_summary_json(&mut out, &report.summary);
    out.push_str("},\"redactionPosture\":{");
    out.push_str("\"rawMemoryBodyPresent\":false,");
    out.push_str("\"rawMailBodyPresent\":false,");
    out.push_str("\"rawPolicyPayloadPresent\":false,");
    out.push_str("\"absoluteHostPathPresent\":false,");
    out.push_str("\"secretsPresent\":false");
    out.push_str("}}");
    out
}

fn push_summary_json(out: &mut String, summary: &ShadowPolicyScoreSummary) {
    push_u32_field(out, "totalEvidence", summary.total_evidence);
    out.push(',');
    push_u32_field(out, "usableEvidence", summary.usable_evidence);
    out.push(',');
    push_u32_field(out, "missingEvidence", summary.missing_evidence);
    out.push(',');
    push_u32_field(out, "staleEvidence", summary.stale_evidence);
    out.push(',');
    push_u32_field(out, "unsupportedEvidence", summary.unsupported_evidence);
    out.push(',');
    push_u32_field(out, "divergedEvidence", summary.diverged_evidence);
    out.push(',');
    push_f64_field(out, "divergenceRate", summary.divergence_rate);
    out.push(',');
    push_f64_field(out, "utilityDelta", summary.utility_delta);
    out.push(',');
    push_u32_field(
        out,
        "droppedRequiredEvidenceCount",
        summary.dropped_required_evidence_count,
    );
    out.push(',');
    push_i64_field(out, "outputBytesDelta", summary.output_bytes_delta);
    out.push(',');
    push_u64_field(out, "p50LatencyMs", summary.p50_latency_ms);
    out.push(',');
    push_u64_field(out, "p95LatencyMs", summary.p95_latency_ms);
    out.push(',');
    push_u64_field(out, "p99LatencyMs", summary.p99_latency_ms);
    out.push(',');
    push_i64_field(out, "degradedDelta", summary.degraded_delta);
    out.push(',');
    push_i64_field(out, "resourceBytesDelta", summary.resource_bytes_delta);
    out.push_str(",\"redactionSafe\":");
    out.push_str(if summary.redaction_safe {
        "true"
    } else {
        "false"
    });
}

fn push_evidence_point_json(out: &mut String, point: &ShadowPolicyEvidencePoint) {
    out.push('{');
    json_str_field(out, "artifactId", &point.artifact_id);
    out.push(',');
    json_str_field(out, "kind", point.kind.as_str());
    out.push(',');
    json_str_field(out, "cohort", &point.cohort);
    out.push(',');
    json_str_field(out, "posture", point.posture.as_str());
    out.push(',');
    push_f64_field(out, "utilityDelta", point.utility_delta());
    out.push(',');
    push_i64_field(out, "outputBytesDelta", point.output_bytes_delta());
    out.push(',');
    push_u64_field(out, "candidateLatencyMs", point.candidate_latency_ms);
    out.push(',');
    push_i64_field(out, "degradedDelta", point.degraded_delta());
    out.push(',');
    push_u32_field(
        out,
        "droppedRequiredEvidenceCount",
        point.dropped_required_evidence_count,
    );
    out.push(',');
    push_i64_field(out, "resourceBytesDelta", point.resource_bytes_delta);
    out.push_str(",\"redactionSafe\":");
    out.push_str(if point.redaction_safe {
        "true"
    } else {
        "false"
    });
    out.push('}');
}

fn json_str_field(out: &mut String, field: &str, value: &str) {
    out.push('"');
    out.push_str(field);
    out.push_str("\":");
    push_json_string(out, value);
}

fn push_json_string(out: &mut String, value: &str) {
    match serde_json::to_string(value) {
        Ok(encoded) => out.push_str(&encoded),
        Err(_) => out.push_str("\"<invalid>\""),
    }
}

fn push_string_array(out: &mut String, values: &[String]) {
    for (idx, value) in values.iter().enumerate() {
        if idx > 0 {
            out.push(',');
        }
        push_json_string(out, value);
    }
}

fn push_u32_field(out: &mut String, field: &str, value: u32) {
    out.push('"');
    out.push_str(field);
    out.push_str("\":");
    out.push_str(&value.to_string());
}

fn push_u64_field(out: &mut String, field: &str, value: u64) {
    out.push('"');
    out.push_str(field);
    out.push_str("\":");
    out.push_str(&value.to_string());
}

fn push_i64_field(out: &mut String, field: &str, value: i64) {
    out.push('"');
    out.push_str(field);
    out.push_str("\":");
    out.push_str(&value.to_string());
}

fn push_f64_field(out: &mut String, field: &str, value: f64) {
    out.push('"');
    out.push_str(field);
    out.push_str("\":");
    out.push_str(&format!("{value:.4}"));
}

fn signed_u64_delta(candidate: u64, incumbent: u64) -> i64 {
    if candidate >= incumbent {
        i64::try_from(candidate - incumbent).unwrap_or(i64::MAX)
    } else {
        let delta = incumbent - candidate;
        if delta > i64::MAX as u64 {
            i64::MIN
        } else {
            -(delta as i64)
        }
    }
}

/// Configuration for shadow-run gates.
#[derive(Clone, Debug)]
pub struct ShadowGateConfig {
    /// Mode controlling side effects.
    pub mode: ShadowMode,
    /// Minimum quality improvement to consider candidate better.
    pub quality_threshold: f64,
    /// Maximum acceptable slowdown ratio (candidate_time / incumbent_time).
    pub max_slowdown_ratio: f64,
    /// Minimum overlap ratio required for equivalence verdict.
    pub min_overlap_for_equivalent: f64,
}

impl Default for ShadowGateConfig {
    fn default() -> Self {
        Self {
            mode: ShadowMode::Compare,
            quality_threshold: 0.05,
            max_slowdown_ratio: 2.0,
            min_overlap_for_equivalent: 0.95,
        }
    }
}

impl ShadowGateConfig {
    #[must_use]
    pub fn with_mode(mut self, mode: ShadowMode) -> Self {
        self.mode = mode;
        self
    }

    #[must_use]
    pub fn with_quality_threshold(mut self, threshold: f64) -> Self {
        self.quality_threshold = threshold;
        self
    }
}

/// Report from a shadow-run comparison.
#[derive(Clone, Debug)]
pub struct ShadowReport {
    /// Schema version.
    pub schema: &'static str,
    /// Unique ID for this report.
    pub id: String,
    /// Policy domain.
    pub domain: PolicyDomain,
    /// Incumbent policy name.
    pub incumbent_name: String,
    /// Candidate policy name.
    pub candidate_name: String,
    /// Comparison verdict.
    pub verdict: ShadowVerdict,
    /// Comparison metrics.
    pub metrics: ShadowMetrics,
    /// Mode used.
    pub mode: ShadowMode,
    /// Timestamp.
    pub timestamp: String,
    /// Optional explanation.
    pub explanation: Option<String>,
}

impl ShadowReport {
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: &str,
        domain: PolicyDomain,
        incumbent_name: &str,
        candidate_name: &str,
        verdict: ShadowVerdict,
        metrics: ShadowMetrics,
        mode: ShadowMode,
        timestamp: &str,
    ) -> Self {
        Self {
            schema: SHADOW_REPORT_SCHEMA_V1,
            id: id.to_string(),
            domain,
            incumbent_name: incumbent_name.to_string(),
            candidate_name: candidate_name.to_string(),
            verdict,
            metrics,
            mode,
            timestamp: timestamp.to_string(),
            explanation: None,
        }
    }

    pub fn with_explanation(mut self, explanation: &str) -> Self {
        self.explanation = Some(explanation.to_string());
        self
    }
}

/// Determine verdict from metrics using config thresholds.
#[must_use]
pub fn determine_verdict(metrics: &ShadowMetrics, config: &ShadowGateConfig) -> ShadowVerdict {
    let quality_delta = metrics.quality_delta();

    if metrics.overlap_ratio >= config.min_overlap_for_equivalent
        && quality_delta.abs() < config.quality_threshold
    {
        return ShadowVerdict::Equivalent;
    }

    let baseline_threshold_us = 1000.0;
    let too_slow = (metrics.candidate_time_us as f64)
        > ((metrics.incumbent_time_us as f64).max(baseline_threshold_us)
            * config.max_slowdown_ratio);

    if quality_delta >= config.quality_threshold && !too_slow {
        ShadowVerdict::CandidateBetter
    } else if quality_delta <= -config.quality_threshold {
        ShadowVerdict::IncumbentBetter
    } else if metrics.overlap_ratio < 0.5 {
        ShadowVerdict::Divergent
    } else {
        ShadowVerdict::Inconclusive
    }
}

/// Build an explanation for the verdict.
#[must_use]
pub fn build_explanation(metrics: &ShadowMetrics, verdict: ShadowVerdict) -> String {
    let quality_delta = metrics.quality_delta();
    let time_delta = metrics.time_delta_us();

    match verdict {
        ShadowVerdict::Equivalent => format!(
            "Outputs are equivalent: {:.1}% overlap, quality delta {:.3}.",
            metrics.overlap_ratio * 100.0,
            quality_delta
        ),
        ShadowVerdict::CandidateBetter => format!(
            "Candidate outperforms incumbent: quality +{:.3}, time delta {}us.",
            quality_delta, time_delta
        ),
        ShadowVerdict::IncumbentBetter => format!(
            "Incumbent outperforms candidate: quality delta {:.3}.",
            quality_delta
        ),
        ShadowVerdict::Divergent => format!(
            "Outputs diverge significantly: only {:.1}% overlap.",
            metrics.overlap_ratio * 100.0
        ),
        ShadowVerdict::Inconclusive => {
            "Comparison inconclusive: insufficient data to determine winner.".to_string()
        }
    }
}

/// Gate for pack selection shadow runs.
pub mod pack {
    use super::*;
    use crate::pack::PackSelectionObjective;

    /// Input for pack selection shadow comparison.
    #[derive(Clone, Debug)]
    pub struct PackShadowInput {
        /// Candidate pool size.
        pub candidate_count: usize,
        /// Token budget.
        pub token_budget: u32,
        /// Incumbent objective.
        pub incumbent_objective: PackSelectionObjective,
        /// Candidate objective.
        pub candidate_objective: PackSelectionObjective,
    }

    /// Output from one pack selection run.
    #[derive(Clone, Debug)]
    pub struct PackShadowOutput {
        /// Memory IDs selected.
        pub selected_ids: Vec<String>,
        /// Total tokens used.
        pub tokens_used: u32,
        /// Quality score (aggregate relevance).
        pub quality_score: f64,
        /// Execution time in microseconds.
        pub time_us: u64,
    }

    /// Compare pack selection outputs.
    #[must_use]
    pub fn compare_outputs(
        incumbent: &PackShadowOutput,
        candidate: &PackShadowOutput,
        config: &ShadowGateConfig,
    ) -> (ShadowVerdict, ShadowMetrics) {
        let incumbent_set: std::collections::BTreeSet<_> = incumbent.selected_ids.iter().collect();
        let candidate_set: std::collections::BTreeSet<_> = candidate.selected_ids.iter().collect();

        let intersection = incumbent_set.intersection(&candidate_set).count();
        let union = incumbent_set.union(&candidate_set).count();
        let overlap_ratio = if union > 0 {
            intersection as f64 / union as f64
        } else {
            1.0
        };

        let metrics = ShadowMetrics {
            incumbent_size: incumbent.selected_ids.len() as u64,
            candidate_size: candidate.selected_ids.len() as u64,
            incumbent_quality: incumbent.quality_score,
            candidate_quality: candidate.quality_score,
            overlap_ratio,
            incumbent_time_us: incumbent.time_us,
            candidate_time_us: candidate.time_us,
        };

        let verdict = determine_verdict(&metrics, config);
        (verdict, metrics)
    }
}

/// Gate for curation filter shadow runs.
pub mod curation {
    use super::*;

    /// Output from one curation filter run.
    #[derive(Clone, Debug)]
    pub struct CurationShadowOutput {
        /// Candidate IDs accepted.
        pub accepted_ids: Vec<String>,
        /// Candidate IDs rejected.
        pub rejected_ids: Vec<String>,
        /// Total risk score.
        pub total_risk: f64,
        /// Execution time in microseconds.
        pub time_us: u64,
    }

    /// Compare curation filter outputs.
    #[must_use]
    pub fn compare_outputs(
        incumbent: &CurationShadowOutput,
        candidate: &CurationShadowOutput,
        config: &ShadowGateConfig,
    ) -> (ShadowVerdict, ShadowMetrics) {
        let incumbent_set: std::collections::BTreeSet<_> = incumbent.accepted_ids.iter().collect();
        let candidate_set: std::collections::BTreeSet<_> = candidate.accepted_ids.iter().collect();

        let intersection = incumbent_set.intersection(&candidate_set).count();
        let union = incumbent_set.union(&candidate_set).count();
        let overlap_ratio = if union > 0 {
            intersection as f64 / union as f64
        } else {
            1.0
        };

        let metrics = ShadowMetrics {
            incumbent_size: incumbent.accepted_ids.len() as u64,
            candidate_size: candidate.accepted_ids.len() as u64,
            incumbent_quality: 1.0 - incumbent.total_risk.min(1.0),
            candidate_quality: 1.0 - candidate.total_risk.min(1.0),
            overlap_ratio,
            incumbent_time_us: incumbent.time_us,
            candidate_time_us: candidate.time_us,
        };

        let verdict = determine_verdict(&metrics, config);
        (verdict, metrics)
    }
}

/// Gate for cache admission shadow runs.
pub mod cache {
    use super::*;
    use crate::cache::CacheStats;

    /// Output from one cache policy run.
    #[derive(Clone, Debug)]
    pub struct CacheShadowOutput {
        /// Cache stats after workload.
        pub stats: CacheStats,
        /// Final cache size.
        pub final_size: usize,
        /// Estimated memory used by the cache.
        pub memory_bytes: u64,
        /// Aggregate miss cost for the workload.
        pub miss_cost: u64,
        /// p95 latency for cache operations.
        pub p95_latency_us: u64,
        /// p99 latency for cache operations.
        pub p99_latency_us: u64,
        /// Execution time in microseconds.
        pub time_us: u64,
    }

    /// Compare cache policy outputs.
    #[must_use]
    pub fn compare_outputs(
        incumbent: &CacheShadowOutput,
        candidate: &CacheShadowOutput,
        config: &ShadowGateConfig,
    ) -> (ShadowVerdict, ShadowMetrics) {
        let incumbent_hit_rate = incumbent.stats.hit_rate();
        let candidate_hit_rate = candidate.stats.hit_rate();

        let metrics = ShadowMetrics {
            incumbent_size: incumbent.final_size as u64,
            candidate_size: candidate.final_size as u64,
            incumbent_quality: incumbent_hit_rate,
            candidate_quality: candidate_hit_rate,
            overlap_ratio: 1.0 - (incumbent_hit_rate - candidate_hit_rate).abs(),
            incumbent_time_us: incumbent.time_us,
            candidate_time_us: candidate.time_us,
        };

        let verdict = determine_verdict(&metrics, config);
        (verdict, metrics)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shadow_mode_strings_are_stable() {
        assert_eq!(ShadowMode::Compare.as_str(), "compare");
        assert_eq!(ShadowMode::CompareAndLog.as_str(), "compare_and_log");
        assert_eq!(ShadowMode::CompareLogAlert.as_str(), "compare_log_alert");
    }

    #[test]
    fn policy_domain_strings_are_stable() {
        assert_eq!(PolicyDomain::PackSelection.as_str(), "pack_selection");
        assert_eq!(PolicyDomain::CurationFilter.as_str(), "curation_filter");
        assert_eq!(PolicyDomain::CacheAdmission.as_str(), "cache_admission");
        assert_eq!(
            PolicyDomain::VerificationAdmission.as_str(),
            "verification_admission"
        );
    }

    #[test]
    fn shadow_verdict_strings_are_stable() {
        assert_eq!(ShadowVerdict::Equivalent.as_str(), "equivalent");
        assert_eq!(ShadowVerdict::CandidateBetter.as_str(), "candidate_better");
        assert_eq!(ShadowVerdict::IncumbentBetter.as_str(), "incumbent_better");
        assert_eq!(ShadowVerdict::Divergent.as_str(), "divergent");
        assert_eq!(ShadowVerdict::Inconclusive.as_str(), "inconclusive");
    }

    #[test]
    fn verdict_safe_to_promote() {
        assert!(ShadowVerdict::Equivalent.is_safe_to_promote());
        assert!(ShadowVerdict::CandidateBetter.is_safe_to_promote());
        assert!(!ShadowVerdict::IncumbentBetter.is_safe_to_promote());
        assert!(!ShadowVerdict::Divergent.is_safe_to_promote());
        assert!(!ShadowVerdict::Inconclusive.is_safe_to_promote());
    }

    #[test]
    fn promotion_guards_block_unsafe_candidate() {
        let guards = ShadowPromotionGuards {
            dropped_critical_warnings: true,
            redaction_differences: false,
            p99_regression: true,
            tail_risk_regression: false,
            shadow_mismatch_above_tolerance: false,
        };

        assert!(guards.blocks_promotion());
        assert_eq!(
            guards.blocker_codes(),
            vec!["dropped_critical_warnings", "p99_regression"]
        );
        assert!(!candidate_promotion_allowed(
            ShadowVerdict::CandidateBetter,
            &guards
        ));
        assert!(candidate_promotion_allowed(
            ShadowVerdict::CandidateBetter,
            &ShadowPromotionGuards::default()
        ));
    }

    #[test]
    fn metrics_quality_delta() {
        let metrics = ShadowMetrics {
            incumbent_quality: 0.75,
            candidate_quality: 0.82,
            ..Default::default()
        };
        let delta = metrics.quality_delta();
        assert!((delta - 0.07).abs() < 0.001);
    }

    #[test]
    fn metrics_size_delta() {
        let metrics = ShadowMetrics {
            incumbent_size: 100,
            candidate_size: 85,
            ..Default::default()
        };
        assert_eq!(metrics.size_delta(), -15);
    }

    #[test]
    fn metric_deltas_saturate_instead_of_wrapping() {
        let positive = ShadowMetrics {
            incumbent_size: 0,
            candidate_size: u64::MAX,
            incumbent_time_us: 1,
            candidate_time_us: u64::MAX,
            ..Default::default()
        };
        assert_eq!(positive.size_delta(), i64::MAX);
        assert_eq!(positive.time_delta_us(), i64::MAX);

        let negative = ShadowMetrics {
            incumbent_size: u64::MAX,
            candidate_size: 0,
            incumbent_time_us: u64::MAX,
            candidate_time_us: 1,
            ..Default::default()
        };
        assert_eq!(negative.size_delta(), i64::MIN);
        assert_eq!(negative.time_delta_us(), i64::MIN);
    }

    #[test]
    fn determine_verdict_equivalent() {
        let metrics = ShadowMetrics {
            overlap_ratio: 0.98,
            incumbent_quality: 0.80,
            candidate_quality: 0.82,
            ..Default::default()
        };
        let config = ShadowGateConfig::default();
        let verdict = determine_verdict(&metrics, &config);
        assert_eq!(verdict, ShadowVerdict::Equivalent);
    }

    #[test]
    fn determine_verdict_candidate_better() {
        let metrics = ShadowMetrics {
            overlap_ratio: 0.70,
            incumbent_quality: 0.75,
            candidate_quality: 0.90,
            incumbent_time_us: 100,
            candidate_time_us: 110,
            ..Default::default()
        };
        let config = ShadowGateConfig::default();
        let verdict = determine_verdict(&metrics, &config);
        assert_eq!(verdict, ShadowVerdict::CandidateBetter);
    }

    #[test]
    fn determine_verdict_incumbent_better() {
        let metrics = ShadowMetrics {
            overlap_ratio: 0.70,
            incumbent_quality: 0.90,
            candidate_quality: 0.75,
            ..Default::default()
        };
        let config = ShadowGateConfig::default();
        let verdict = determine_verdict(&metrics, &config);
        assert_eq!(verdict, ShadowVerdict::IncumbentBetter);
    }

    #[test]
    fn determine_verdict_divergent() {
        let metrics = ShadowMetrics {
            overlap_ratio: 0.30,
            incumbent_quality: 0.80,
            candidate_quality: 0.82,
            ..Default::default()
        };
        let config = ShadowGateConfig::default();
        let verdict = determine_verdict(&metrics, &config);
        assert_eq!(verdict, ShadowVerdict::Divergent);
    }

    #[test]
    fn shadow_report_creation() {
        let metrics = ShadowMetrics::default();
        let report = ShadowReport::new(
            "sr_test_001",
            PolicyDomain::PackSelection,
            "mmr_redundancy",
            "facility_location",
            ShadowVerdict::CandidateBetter,
            metrics,
            ShadowMode::Compare,
            "2026-05-01T12:00:00Z",
        )
        .with_explanation("Facility location improves diversity.");

        assert_eq!(report.id, "sr_test_001");
        assert_eq!(report.domain, PolicyDomain::PackSelection);
        assert_eq!(report.incumbent_name, "mmr_redundancy");
        assert_eq!(report.candidate_name, "facility_location");
        assert_eq!(report.verdict, ShadowVerdict::CandidateBetter);
        assert!(report.explanation.is_some());
    }

    #[test]
    fn build_explanation_for_verdicts() {
        let metrics = ShadowMetrics {
            overlap_ratio: 0.95,
            incumbent_quality: 0.80,
            candidate_quality: 0.82,
            candidate_time_us: 100,
            incumbent_time_us: 90,
            ..Default::default()
        };

        let expl = build_explanation(&metrics, ShadowVerdict::Equivalent);
        assert!(expl.contains("equivalent"));
        assert!(expl.contains("95.0%"));

        let expl2 = build_explanation(&metrics, ShadowVerdict::CandidateBetter);
        assert!(expl2.contains("outperforms"));
    }

    #[test]
    fn config_builder() {
        let config = ShadowGateConfig::default()
            .with_mode(ShadowMode::CompareLogAlert)
            .with_quality_threshold(0.10);

        assert_eq!(config.mode, ShadowMode::CompareLogAlert);
        assert!((config.quality_threshold - 0.10).abs() < 0.001);
    }

    #[test]
    fn queue_pressure_source_states_are_schema_bounded() {
        let cases = [
            (
                ResourceQueuePressureSourceState::Fresh,
                "fresh",
                "fresh",
                "observed",
                "high",
                false,
            ),
            (
                ResourceQueuePressureSourceState::Partial,
                "partial",
                "fresh",
                "observed",
                "medium",
                false,
            ),
            (
                ResourceQueuePressureSourceState::Degraded,
                "degraded",
                "fresh",
                "observed",
                "medium",
                false,
            ),
            (
                ResourceQueuePressureSourceState::Unavailable,
                "unavailable",
                "unavailable",
                "unavailable",
                "low",
                true,
            ),
            (
                ResourceQueuePressureSourceState::Corrupt,
                "corrupt",
                "unavailable",
                "corrupt",
                "low",
                true,
            ),
            (
                ResourceQueuePressureSourceState::Stale,
                "stale",
                "stale",
                "observed",
                "low",
                true,
            ),
            (
                ResourceQueuePressureSourceState::Contradictory,
                "contradictory",
                "contradictory",
                "contradictory",
                "low",
                true,
            ),
        ];

        assert_eq!(ResourceQueuePressureSourceState::all().len(), cases.len());
        for (state, label, freshness, evidence_state, confidence, untrusted) in cases {
            assert_eq!(state.as_str(), label);
            assert_eq!(state.freshness(), freshness);
            assert_eq!(state.evidence_state(), evidence_state);
            assert_eq!(state.confidence(), confidence);
            assert_eq!(state.is_untrusted(), untrusted);
        }
    }

    #[test]
    fn queue_pressure_taxonomy_matches_schema_codes() {
        let reason_codes: Vec<_> = ResourceQueuePressureReasonCode::all()
            .iter()
            .map(|reason_code| reason_code.as_str())
            .collect();
        assert_eq!(
            reason_codes,
            vec![
                "rch_lane_busy",
                "rch_telemetry_gap",
                "active_build_slot_exhausted",
                "stale_in_progress_bead",
                "agent_mail_unavailable",
                "agent_mail_recovery_corrupt",
                "dirty_checkout_saturated",
                "local_cargo_refused",
                "output_budget_pressure",
                "host_calibration_missing",
                "contradictory_source_state",
            ]
        );

        let source_kinds: Vec<_> = ResourceQueuePressureSourceKind::all()
            .iter()
            .map(|kind| kind.as_str())
            .collect();
        assert_eq!(
            source_kinds,
            vec![
                "rch_status",
                "rch_selector_admission_probe",
                "build_slot_lease",
                "beads_in_progress_summary",
                "agent_mail_health",
                "agent_mail_recovery_probe",
                "git_dirty_summary",
                "local_cargo_tripwire",
                "output_budget_governor",
                "host_calibration_posture",
                "source_authority_snapshot",
                "manual_fixture",
            ]
        );
    }

    #[test]
    fn queue_pressure_agent_mail_green_health_does_not_hide_recovery_corrupt() {
        let inventory = ResourceQueuePressureInventory::new(vec![
            ResourceQueuePressureSourceRef::new(
                ResourceQueuePressureSourceKind::AgentMailHealth,
                ResourceQueuePressureSourceState::Fresh,
            )
            .with_source_schema("ee.agent_mail.snapshot.v1")
            .with_hash("blake3:agent-mail-health-green"),
            ResourceQueuePressureSourceRef::new(
                ResourceQueuePressureSourceKind::AgentMailRecoveryProbe,
                ResourceQueuePressureSourceState::Corrupt,
            )
            .with_source_schema("ee.agent_mail.snapshot.v1")
            .with_bounded_preview("recovery.mode=corrupt"),
        ]);

        let report = inventory.report();
        assert_eq!(report.level, ResourceQueuePressureLevel::Unknown);
        assert!(!report.can_authorize_claim);
        assert_eq!(report.reason_codes, vec!["agent_mail_recovery_corrupt"]);
        assert_eq!(report.abstained_sources, vec!["agent_mail_recovery_probe"]);
        assert_eq!(
            report.redaction_posture,
            RESOURCE_QUEUE_PRESSURE_REDACTION_POSTURE
        );
    }

    #[test]
    fn queue_pressure_beads_clean_plus_doctor_external_changes_is_contradictory() {
        let inventory = ResourceQueuePressureInventory::new(vec![
            ResourceQueuePressureSourceRef::new(
                ResourceQueuePressureSourceKind::BeadsInProgressSummary,
                ResourceQueuePressureSourceState::Fresh,
            )
            .with_bounded_preview("br sync status clean"),
            ResourceQueuePressureSourceRef::new(
                ResourceQueuePressureSourceKind::SourceAuthoritySnapshot,
                ResourceQueuePressureSourceState::Contradictory,
            )
            .with_bounded_preview("doctor=external_changes_pending_import"),
        ]);

        let report = inventory.report();
        assert_eq!(report.level, ResourceQueuePressureLevel::Unknown);
        assert_eq!(report.reason_codes, vec!["contradictory_source_state"]);
        assert_eq!(report.abstained_sources, vec!["source_authority_snapshot"]);
    }

    #[test]
    fn queue_pressure_rch_idle_lists_with_exhausted_slots_are_saturated() {
        let inventory = ResourceQueuePressureInventory::new(vec![
            ResourceQueuePressureSourceRef::new(
                ResourceQueuePressureSourceKind::RchStatus,
                ResourceQueuePressureSourceState::Fresh,
            )
            .with_bounded_preview("active_builds=0 queued_builds=0"),
            ResourceQueuePressureSourceRef::new(
                ResourceQueuePressureSourceKind::BuildSlotLease,
                ResourceQueuePressureSourceState::Degraded,
            )
            .with_bounded_preview("available_slots=0 worker_used_slots=4/4"),
        ]);

        let report = inventory.report();
        assert_eq!(report.level, ResourceQueuePressureLevel::Saturated);
        assert_eq!(report.reason_codes, vec!["active_build_slot_exhausted"]);
        assert!(report.abstained_sources.is_empty());
    }

    #[test]
    fn queue_pressure_dirty_checkout_saturation_is_bounded() {
        let long_preview = "x".repeat(RESOURCE_QUEUE_PRESSURE_BOUNDED_PREVIEW_MAX_CHARS + 32);
        let many_sources = (0..(RESOURCE_QUEUE_PRESSURE_MAX_SOURCE_REFS + 3))
            .map(|_| {
                ResourceQueuePressureSourceRef::new(
                    ResourceQueuePressureSourceKind::GitDirtySummary,
                    ResourceQueuePressureSourceState::Degraded,
                )
                .with_bounded_preview(&long_preview)
            })
            .collect();
        let inventory = ResourceQueuePressureInventory::new(many_sources);

        let report = inventory.report();
        assert_eq!(report.level, ResourceQueuePressureLevel::Saturated);
        assert_eq!(report.reason_codes, vec!["dirty_checkout_saturated"]);
        assert_eq!(
            report.source_refs.len(),
            RESOURCE_QUEUE_PRESSURE_MAX_SOURCE_REFS
        );
        assert_eq!(
            report.source_refs[0]
                .bounded_preview
                .as_ref()
                .map(String::len),
            Some(RESOURCE_QUEUE_PRESSURE_BOUNDED_PREVIEW_MAX_CHARS)
        );
    }

    #[test]
    fn queue_pressure_backoff_covers_each_decision() {
        struct Case {
            level: ResourceQueuePressureLevel,
            reasons: &'static [&'static str],
            cost: ResourceCostClass,
            decision: ResourceAdmissionDecision,
            primary_reason: &'static str,
        }

        let cases = [
            Case {
                level: ResourceQueuePressureLevel::Idle,
                reasons: &[],
                cost: ResourceCostClass::Standard,
                decision: ResourceAdmissionDecision::Admit,
                primary_reason: "queue_pressure_idle",
            },
            Case {
                level: ResourceQueuePressureLevel::Moderate,
                reasons: &["output_budget_pressure"],
                cost: ResourceCostClass::Standard,
                decision: ResourceAdmissionDecision::DegradeToLean,
                primary_reason: "output_budget_pressure",
            },
            Case {
                level: ResourceQueuePressureLevel::Saturated,
                reasons: &["dirty_checkout_saturated"],
                cost: ResourceCostClass::Standard,
                decision: ResourceAdmissionDecision::Queue,
                primary_reason: "dirty_checkout_saturated",
            },
            Case {
                level: ResourceQueuePressureLevel::Saturated,
                reasons: &["active_build_slot_exhausted"],
                cost: ResourceCostClass::Standard,
                decision: ResourceAdmissionDecision::WaitForRch,
                primary_reason: "active_build_slot_exhausted",
            },
            Case {
                level: ResourceQueuePressureLevel::Saturated,
                reasons: &["dirty_checkout_saturated"],
                cost: ResourceCostClass::SwarmHeavy,
                decision: ResourceAdmissionDecision::SplitWorkload,
                primary_reason: "dirty_checkout_saturated",
            },
            Case {
                level: ResourceQueuePressureLevel::Low,
                reasons: &["local_cargo_refused"],
                cost: ResourceCostClass::Standard,
                decision: ResourceAdmissionDecision::RefuseLocalCargo,
                primary_reason: "local_cargo_refused",
            },
            Case {
                level: ResourceQueuePressureLevel::Low,
                reasons: &["agent_mail_recovery_corrupt"],
                cost: ResourceCostClass::Standard,
                decision: ResourceAdmissionDecision::Abstain,
                primary_reason: "agent_mail_recovery_corrupt",
            },
        ];

        for case in cases {
            let advice = evaluate_resource_queue_pressure_backoff(&backoff_input(
                case.level,
                case.reasons,
                case.cost,
                true,
            ));
            assert_eq!(advice.decision, case.decision);
            assert_eq!(advice.primary_reason, case.primary_reason);
            assert!(!advice.can_authorize_claim);
            assert!(!advice.next_safe_action.is_empty());
            assert!(!advice.what_would_change.is_empty());
        }
    }

    #[test]
    fn queue_pressure_backoff_precedence_is_safety_dominant() {
        let local_cargo_wins = evaluate_resource_queue_pressure_backoff(&backoff_input(
            ResourceQueuePressureLevel::Saturated,
            &[
                "output_budget_pressure",
                "active_build_slot_exhausted",
                "local_cargo_refused",
            ],
            ResourceCostClass::SwarmHeavy,
            true,
        ));
        assert_eq!(
            local_cargo_wins.decision,
            ResourceAdmissionDecision::RefuseLocalCargo
        );
        assert_eq!(local_cargo_wins.primary_reason, "local_cargo_refused");
        assert_eq!(
            local_cargo_wins.contributing_reasons,
            vec![
                "local_cargo_refused",
                "active_build_slot_exhausted",
                "output_budget_pressure"
            ]
        );

        let untrusted_mail_wins = evaluate_resource_queue_pressure_backoff(&backoff_input(
            ResourceQueuePressureLevel::Saturated,
            &["rch_lane_busy", "agent_mail_unavailable"],
            ResourceCostClass::Standard,
            true,
        ));
        assert_eq!(
            untrusted_mail_wins.decision,
            ResourceAdmissionDecision::Abstain
        );
        assert_eq!(untrusted_mail_wins.primary_reason, "agent_mail_unavailable");
        assert_eq!(
            untrusted_mail_wins.blocked_by,
            vec!["agent_mail", "rch_lane"]
        );

        let rch_wins_over_split = evaluate_resource_queue_pressure_backoff(&backoff_input(
            ResourceQueuePressureLevel::Saturated,
            &["dirty_checkout_saturated", "rch_telemetry_gap"],
            ResourceCostClass::SwarmHeavy,
            true,
        ));
        assert_eq!(
            rch_wins_over_split.decision,
            ResourceAdmissionDecision::WaitForRch
        );
        assert_eq!(rch_wins_over_split.primary_reason, "rch_telemetry_gap");
    }

    #[test]
    fn queue_pressure_backoff_never_authorizes_claims() {
        let unsafe_claim_gate = evaluate_resource_queue_pressure_backoff(&backoff_input(
            ResourceQueuePressureLevel::Idle,
            &[],
            ResourceCostClass::Standard,
            false,
        ));
        assert_eq!(unsafe_claim_gate.decision, ResourceAdmissionDecision::Admit);
        assert!(!unsafe_claim_gate.can_authorize_claim);
        assert_eq!(unsafe_claim_gate.blocked_by, vec!["claim_gate_authority"]);

        let safe_claim_gate = evaluate_resource_queue_pressure_backoff(&backoff_input(
            ResourceQueuePressureLevel::Idle,
            &[],
            ResourceCostClass::Standard,
            true,
        ));
        assert_eq!(safe_claim_gate.decision, ResourceAdmissionDecision::Admit);
        assert!(!safe_claim_gate.can_authorize_claim);
        assert!(safe_claim_gate.blocked_by.is_empty());
    }

    #[test]
    fn queue_pressure_backoff_summary_is_byte_stable() {
        let advice = evaluate_resource_queue_pressure_backoff(&backoff_input(
            ResourceQueuePressureLevel::Saturated,
            &["dirty_checkout_saturated", "rch_lane_busy"],
            ResourceCostClass::Standard,
            false,
        ));

        assert_eq!(
            render_backoff_advice_golden(&advice),
            "{\"decision\":\"wait_for_rch\",\"canAuthorizeClaim\":false,\"primaryReason\":\"rch_lane_busy\",\"contributingReasons\":[\"rch_lane_busy\",\"dirty_checkout_saturated\"],\"blockedBy\":[\"rch_lane\",\"claim_gate_authority\"],\"nextSafeAction\":\"rch status --json\",\"whatWouldChange\":\"rch_lane_has_capacity_and_fresh_telemetry\",\"hysteresis\":{\"stateKey\":\"wait_for_rch\",\"stableAfterObservations\":2,\"cooldownObservations\":2}}"
        );
    }

    fn backoff_input(
        level: ResourceQueuePressureLevel,
        reasons: &'static [&'static str],
        estimated_cost_class: ResourceCostClass,
        claim_gate_safe_to_claim: bool,
    ) -> ResourceQueuePressureBackoffInput {
        ResourceQueuePressureBackoffInput {
            queue_pressure: ResourceQueuePressureReport {
                level,
                can_authorize_claim: false,
                reason_codes: reasons.iter().map(|reason| (*reason).to_owned()).collect(),
                abstained_sources: Vec::new(),
                source_refs: Vec::new(),
                redaction_posture: RESOURCE_QUEUE_PRESSURE_REDACTION_POSTURE,
            },
            estimated_cost_class,
            claim_gate_safe_to_claim,
        }
    }

    fn render_backoff_advice_golden(advice: &ResourceQueuePressureBackoffAdvice) -> String {
        format!(
            "{{\"decision\":\"{}\",\"canAuthorizeClaim\":{},\"primaryReason\":\"{}\",\"contributingReasons\":{},\"blockedBy\":{},\"nextSafeAction\":\"{}\",\"whatWouldChange\":\"{}\",\"hysteresis\":{{\"stateKey\":\"{}\",\"stableAfterObservations\":{},\"cooldownObservations\":{}}}}}",
            advice.decision.as_str(),
            advice.can_authorize_claim,
            advice.primary_reason,
            quoted_string_list(&advice.contributing_reasons),
            quoted_string_list(&advice.blocked_by),
            advice.next_safe_action,
            advice.what_would_change,
            advice.hysteresis.state_key,
            advice.hysteresis.stable_after_observations,
            advice.hysteresis.cooldown_observations
        )
    }

    fn quoted_string_list(values: &[String]) -> String {
        format!(
            "[{}]",
            values
                .iter()
                .map(|value| format!("\"{value}\""))
                .collect::<Vec<_>>()
                .join(",")
        )
    }

    mod pack_tests {
        use super::super::pack::*;
        use super::super::*;

        #[test]
        fn compare_pack_outputs_equivalent() {
            let incumbent = PackShadowOutput {
                selected_ids: vec!["m1".to_string(), "m2".to_string(), "m3".to_string()],
                tokens_used: 1000,
                quality_score: 0.85,
                time_us: 100,
            };
            let candidate = PackShadowOutput {
                selected_ids: vec!["m1".to_string(), "m2".to_string(), "m3".to_string()],
                tokens_used: 1000,
                quality_score: 0.86,
                time_us: 105,
            };
            let config = ShadowGateConfig::default();
            let (verdict, metrics) = compare_outputs(&incumbent, &candidate, &config);

            assert_eq!(verdict, ShadowVerdict::Equivalent);
            assert!((metrics.overlap_ratio - 1.0).abs() < 0.001);
        }

        #[test]
        fn compare_pack_outputs_divergent() {
            let incumbent = PackShadowOutput {
                selected_ids: vec!["m1".to_string(), "m2".to_string()],
                tokens_used: 500,
                quality_score: 0.80,
                time_us: 100,
            };
            let candidate = PackShadowOutput {
                selected_ids: vec!["m3".to_string(), "m4".to_string()],
                tokens_used: 500,
                quality_score: 0.80,
                time_us: 100,
            };
            let config = ShadowGateConfig::default();
            let (verdict, metrics) = compare_outputs(&incumbent, &candidate, &config);

            assert_eq!(verdict, ShadowVerdict::Divergent);
            assert!((metrics.overlap_ratio - 0.0).abs() < 0.001);
        }
    }

    mod curation_tests {
        use super::super::curation::*;
        use super::super::*;

        #[test]
        fn compare_curation_outputs_candidate_better() {
            let incumbent = CurationShadowOutput {
                accepted_ids: vec!["c1".to_string(), "c2".to_string()],
                rejected_ids: vec!["c3".to_string()],
                total_risk: 0.40,
                time_us: 50,
            };
            let candidate = CurationShadowOutput {
                accepted_ids: vec!["c1".to_string(), "c3".to_string()],
                rejected_ids: vec!["c2".to_string()],
                total_risk: 0.20,
                time_us: 55,
            };
            let config = ShadowGateConfig::default();
            let (verdict, metrics) = compare_outputs(&incumbent, &candidate, &config);

            assert!(metrics.candidate_quality > metrics.incumbent_quality);
            assert!(matches!(
                verdict,
                ShadowVerdict::CandidateBetter | ShadowVerdict::Divergent
            ));
        }
    }

    mod cache_tests {
        use super::super::cache::*;
        use super::super::*;
        use crate::cache::CacheStats;

        #[test]
        fn compare_cache_outputs() {
            let incumbent = CacheShadowOutput {
                stats: CacheStats {
                    hits: 80,
                    misses: 20,
                    evictions: 10,
                    promotions: 5,
                },
                final_size: 50,
                memory_bytes: 4096,
                miss_cost: 20_000,
                p95_latency_us: 120,
                p99_latency_us: 200,
                time_us: 100,
            };
            let candidate = CacheShadowOutput {
                stats: CacheStats {
                    hits: 90,
                    misses: 10,
                    evictions: 8,
                    promotions: 7,
                },
                final_size: 50,
                memory_bytes: 3584,
                miss_cost: 10_000,
                p95_latency_us: 90,
                p99_latency_us: 150,
                time_us: 110,
            };
            let config = ShadowGateConfig::default();
            let (verdict, metrics) = compare_outputs(&incumbent, &candidate, &config);

            assert!(metrics.candidate_quality > metrics.incumbent_quality);
            assert_eq!(verdict, ShadowVerdict::CandidateBetter);
        }
    }
}
