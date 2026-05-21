//! SRR6.25 executable model for mesh anti-entropy and stale-read semantics.
//!
//! This is deliberately not transport code. It is a small, pure model used to
//! pin the invariants ADR 0041 requires future SRR6 anti-entropy, replay, and
//! revision-notice implementations to preserve:
//!
//! - per-origin streams are append-only by `(origin_node_id, seq)`;
//! - duplicate and out-of-order delivery are idempotent;
//! - cursors advance only after durable, contiguous replay;
//! - Tier 1 reads return local state immediately and can later be revised;
//! - conflicting logical revisions remain visible instead of overwriting.

use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

/// ADR that owns this model's assumptions.
pub const ANTI_ENTROPY_MODEL_ADR: &str = "docs/adr/0041-mesh-anti-entropy-model.md";

/// Stable scenario names logged by tests and referenced by ADR 0041.
pub const ANTI_ENTROPY_MODEL_SCENARIOS: &[&str] = &[
    "cursor_advances_only_after_contiguous_replay",
    "partition_rejoin_duplicate_out_of_order_delivery",
    "conflicting_revisions_are_visible",
    "stale_tier1_read_gets_revision_notice",
    "deterministic_replay_order_independent",
    "withdrawal_propagates_as_provenance_tombstone",
    "withdrawn_remote_material_renders_search_context_why_contract",
    "validity_expiry_filters_without_peer_cache_purge",
    "tombstone_hides_from_search_without_body_purge",
    "withdrawal_wins_over_tombstone_and_validity_expiry",
    "malformed_hash_body_policy_schema_events_enter_quarantine",
    "crash_after_insert_before_cursor_requires_repair",
    "quarantine_repair_actions_are_audited",
];

pub const WITHDRAWAL_VISIBILITY_REASON: &str =
    "withdrawal_preserves_metadata_tombstone_and_requests_peer_cache_purge";
pub const TOMBSTONE_VISIBILITY_REASON: &str =
    "tombstone_preserves_provenance_without_peer_cache_purge";
pub const VALIDITY_EXPIRY_VISIBILITY_REASON: &str =
    "validity_expiry_filters_reads_without_peer_cache_purge";
pub const MESH_LOGICAL_VISIBILITY_RENDER_SCHEMA_V1: &str = "ee.mesh.logical_visibility_render.v1";
pub const MESH_EVENT_QUARANTINED_CODE: &str = "mesh_event_quarantined";
pub const MESH_CURSOR_REPAIR_REQUIRED_CODE: &str = "mesh_cursor_repair_required";
pub const MESH_REPLAY_RECOVERY_SCHEMA_V1: &str = "ee.mesh.replay_recovery.v1";
pub const QUARANTINE_ENTERED_LOG: &str = "quarantine_entered";
pub const CURSOR_NOT_ADVANCED_LOG: &str = "cursor_not_advanced";
pub const REPAIR_ACTION_LOG: &str = "repair_action";
pub const REPLAY_RECOVERED_LOG: &str = "replay_recovered";

/// Stream position for one origin node.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct EventKey {
    pub origin_node_id: String,
    pub seq: u64,
}

impl EventKey {
    #[must_use]
    pub fn new(origin_node_id: impl Into<String>, seq: u64) -> Self {
        Self {
            origin_node_id: origin_node_id.into(),
            seq,
        }
    }
}

/// Mesh event kinds that affect deterministic replay visibility.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ModelEventKind {
    Create,
    Revise,
    Tombstone,
    Trust,
    Validity,
    BodyAvailable,
    ShareWithdraw,
}

impl ModelEventKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::Revise => "revise",
            Self::Tombstone => "tombstone",
            Self::Trust => "trust",
            Self::Validity => "validity",
            Self::BodyAvailable => "bodyAvailable",
            Self::ShareWithdraw => "shareWithdraw",
        }
    }
}

/// Append-only mesh event facts relevant to the SRR6.25 model.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelEvent {
    pub key: EventKey,
    pub event_id: String,
    pub kind: ModelEventKind,
    pub logical_memory_id: String,
    pub base_event_id: Option<String>,
    pub content_hash: String,
    pub valid_until_epoch_ms: Option<u64>,
}

impl ModelEvent {
    #[must_use]
    pub fn new(
        origin_node_id: impl Into<String>,
        seq: u64,
        logical_memory_id: impl Into<String>,
        base_event_id: Option<impl Into<String>>,
        content_hash: impl Into<String>,
    ) -> Self {
        Self::new_with_kind(
            origin_node_id,
            seq,
            logical_memory_id,
            base_event_id,
            content_hash,
            ModelEventKind::Create,
        )
    }

    #[must_use]
    pub fn new_with_kind(
        origin_node_id: impl Into<String>,
        seq: u64,
        logical_memory_id: impl Into<String>,
        base_event_id: Option<impl Into<String>>,
        content_hash: impl Into<String>,
        kind: ModelEventKind,
    ) -> Self {
        let origin_node_id = origin_node_id.into();
        let logical_memory_id = logical_memory_id.into();
        let content_hash = content_hash.into();
        let event_id = format!(
            "evt:{origin_node_id}:{seq}:{logical_memory_id}:{}:{content_hash}",
            kind.as_str()
        );
        Self {
            key: EventKey::new(origin_node_id, seq),
            event_id,
            kind,
            logical_memory_id,
            base_event_id: base_event_id.map(Into::into),
            content_hash,
            valid_until_epoch_ms: None,
        }
    }

    #[must_use]
    pub fn with_valid_until_epoch_ms(mut self, valid_until_epoch_ms: u64) -> Self {
        self.valid_until_epoch_ms = Some(valid_until_epoch_ms);
        self
    }
}

/// Effective visibility of one logical memory after replay.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LogicalVisibilityStatus {
    Active,
    Withdrawn,
    Tombstoned,
    Expired,
}

impl LogicalVisibilityStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Withdrawn => "withdrawn",
            Self::Tombstoned => "tombstoned",
            Self::Expired => "expired",
        }
    }
}

/// Read surface that consumes mesh logical visibility decisions.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MeshReadSurface {
    Search,
    Context,
    Why,
}

impl MeshReadSurface {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Search => "search",
            Self::Context => "context",
            Self::Why => "why",
        }
    }
}

/// Context/search/why rendering contract for replayed mesh material.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LogicalVisibility {
    pub logical_memory_id: String,
    pub status: LogicalVisibilityStatus,
    pub active_head_event_ids: Vec<String>,
    pub provenance_event_ids: Vec<String>,
    pub search_visible: bool,
    pub context_visible: bool,
    pub why_provenance_visible: bool,
    pub body_cache_purge_required: bool,
    pub residual_metadata_reason: Option<&'static str>,
}

impl LogicalVisibility {
    #[must_use]
    pub fn render_for_surface(&self, surface: MeshReadSurface) -> MeshLogicalVisibilityRender {
        let included = match surface {
            MeshReadSurface::Search => self.search_visible,
            MeshReadSurface::Context => self.context_visible,
            MeshReadSurface::Why => self.why_provenance_visible,
        };
        let body_visible = matches!(surface, MeshReadSurface::Search | MeshReadSurface::Context)
            && matches!(self.status, LogicalVisibilityStatus::Active)
            && included;
        let provenance_visible = matches!(surface, MeshReadSurface::Why) && included;
        let selected_event_ids = if provenance_visible && !self.provenance_event_ids.is_empty() {
            self.provenance_event_ids.clone()
        } else if included && matches!(self.status, LogicalVisibilityStatus::Active) {
            self.active_head_event_ids.clone()
        } else {
            Vec::new()
        };

        MeshLogicalVisibilityRender {
            schema: MESH_LOGICAL_VISIBILITY_RENDER_SCHEMA_V1,
            surface,
            logical_memory_id: self.logical_memory_id.clone(),
            status: self.status,
            included,
            body_visible,
            provenance_visible,
            body_cache_purge_required: self.body_cache_purge_required,
            selected_event_ids,
            residual_metadata_reason: self.residual_metadata_reason,
            render_text: render_visibility_text(self, surface, included, body_visible),
        }
    }

    #[must_use]
    pub fn render_all_surfaces(&self) -> Vec<MeshLogicalVisibilityRender> {
        [
            MeshReadSurface::Search,
            MeshReadSurface::Context,
            MeshReadSurface::Why,
        ]
        .into_iter()
        .map(|surface| self.render_for_surface(surface))
        .collect()
    }
}

/// Deterministic, redaction-safe render contract for one mesh visibility
/// decision on one read surface.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeshLogicalVisibilityRender {
    pub schema: &'static str,
    pub surface: MeshReadSurface,
    pub logical_memory_id: String,
    pub status: LogicalVisibilityStatus,
    pub included: bool,
    pub body_visible: bool,
    pub provenance_visible: bool,
    pub body_cache_purge_required: bool,
    pub selected_event_ids: Vec<String>,
    pub residual_metadata_reason: Option<&'static str>,
    pub render_text: String,
}

/// Replay result for one delivered event.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReplayOutcome {
    Accepted,
    Duplicate,
    Quarantined(ReplayQuarantineRecord),
    RejectedForkedStream {
        origin_node_id: String,
        seq: u64,
        existing_event_id: String,
        incoming_event_id: String,
    },
}

/// Fail-closed reason for an event that must be kept out of local memory.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum ReplayQuarantineReason {
    MalformedEvent,
    HashChainMismatch,
    BodyHashMismatch,
    PolicyDenied,
    IncompatibleSchema,
}

impl ReplayQuarantineReason {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MalformedEvent => "malformed_event",
            Self::HashChainMismatch => "hash_chain_mismatch",
            Self::BodyHashMismatch => "body_hash_mismatch",
            Self::PolicyDenied => "policy_denied",
            Self::IncompatibleSchema => "incompatible_schema",
        }
    }
}

/// Validation outcome supplied by the import/policy/hash-check layer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReplayValidation {
    Accept,
    Quarantine(ReplayQuarantineReason),
}

/// Repair commands the future CLI may expose for a quarantined event.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum ReplayRepairAction {
    Retry,
    SkipWithAudit,
    RevokePeer,
    ResetCache,
}

impl ReplayRepairAction {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Retry => "retry",
            Self::SkipWithAudit => "skip_with_audit",
            Self::RevokePeer => "revoke_peer",
            Self::ResetCache => "reset_cache",
        }
    }

    #[must_use]
    pub const fn as_cli_arg(self) -> &'static str {
        match self {
            Self::Retry => "retry",
            Self::SkipWithAudit => "skip-with-audit",
            Self::RevokePeer => "revoke-peer",
            Self::ResetCache => "reset-cache",
        }
    }
}

/// Redaction-safe quarantine record for one rejected replay event.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReplayQuarantineRecord {
    pub key: EventKey,
    pub incoming_event_id: String,
    pub reason: ReplayQuarantineReason,
    pub degraded_code: &'static str,
    pub cursor_before: u64,
    pub cursor_after: u64,
    pub repair_actions: Vec<ReplayRepairAction>,
    pub structured_log_events: Vec<&'static str>,
}

/// Audit record emitted by a deterministic repair action.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReplayRepairAuditRecord {
    pub key: EventKey,
    pub action: ReplayRepairAction,
    pub degraded_code: &'static str,
    pub cursor_before: u64,
    pub cursor_after: u64,
    pub structured_log_events: Vec<&'static str>,
}

/// Cursor state that can be repaired after a crash between event insert and
/// cursor update.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CursorRepairRecord {
    pub origin_node_id: String,
    pub cursor_before: u64,
    pub repaired_cursor: u64,
    pub degraded_code: &'static str,
    pub structured_log_events: Vec<&'static str>,
}

/// Status/doctor-ready summary of replay recovery work that must complete
/// before peer sync can be trusted again.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplayRecoveryStatus {
    pub schema: &'static str,
    pub status: &'static str,
    pub quarantined_event_count: usize,
    pub cursor_repair_required_count: usize,
    pub latest_quarantined_event_id: Option<String>,
    pub repair_audit_count: usize,
    pub repair_actions: Vec<String>,
    pub sync_blocked: bool,
    pub degraded: Vec<&'static str>,
    pub structured_log_events: Vec<&'static str>,
}

/// Missing contiguous range requested from a peer during anti-entropy.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EventRange {
    pub origin_node_id: String,
    pub start_seq: u64,
    pub end_seq: u64,
}

/// A logical memory has multiple visible heads. This is conflict evidence, not
/// a resolver that hides one branch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LogicalConflict {
    pub logical_memory_id: String,
    pub head_event_ids: Vec<String>,
}

/// Snapshot returned by the local Tier 1 read path.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Tier1Read {
    pub frontier: BTreeMap<String, u64>,
    pub logical_heads: BTreeMap<String, Vec<String>>,
}

/// Explicit notice that a previously returned Tier 1 read is stale relative to
/// later durable replay.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RevisionNotice {
    pub advanced_origins: Vec<FrontierAdvance>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FrontierAdvance {
    pub origin_node_id: String,
    pub from_seq: u64,
    pub to_seq: u64,
}

/// Pure node-local model. `accepted` is the durable event set; `frontier` is
/// derived from contiguous accepted events and is what peers may safely use as
/// an import cursor.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ModelNode {
    accepted: BTreeMap<EventKey, ModelEvent>,
    frontier: BTreeMap<String, u64>,
    quarantined: BTreeMap<EventKey, ReplayQuarantineRecord>,
    audited_skips: BTreeSet<EventKey>,
    repair_audit: Vec<ReplayRepairAuditRecord>,
}

impl ModelNode {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn accepted_event_ids(&self) -> Vec<String> {
        self.accepted
            .values()
            .map(|event| event.event_id.clone())
            .collect()
    }

    #[must_use]
    pub fn frontier(&self) -> BTreeMap<String, u64> {
        self.frontier.clone()
    }

    #[must_use]
    pub fn cursor_for(&self, origin_node_id: &str) -> u64 {
        self.frontier.get(origin_node_id).copied().unwrap_or(0)
    }

    pub fn replay_with_validation(
        &mut self,
        event: ModelEvent,
        validation: ReplayValidation,
    ) -> ReplayOutcome {
        match validation {
            ReplayValidation::Accept => self.replay(event),
            ReplayValidation::Quarantine(reason) => {
                self.quarantine_event(event.key, event.event_id, reason)
            }
        }
    }

    pub fn replay(&mut self, event: ModelEvent) -> ReplayOutcome {
        match self.accepted.get(&event.key) {
            Some(existing) if existing.event_id == event.event_id => ReplayOutcome::Duplicate,
            Some(existing) => ReplayOutcome::RejectedForkedStream {
                origin_node_id: event.key.origin_node_id,
                seq: event.key.seq,
                existing_event_id: existing.event_id.clone(),
                incoming_event_id: event.event_id,
            },
            None => {
                let origin_node_id = event.key.origin_node_id.clone();
                self.accepted.insert(event.key.clone(), event);
                self.advance_frontier_for(&origin_node_id);
                ReplayOutcome::Accepted
            }
        }
    }

    #[must_use]
    pub fn quarantine_records(&self) -> Vec<ReplayQuarantineRecord> {
        self.quarantined.values().cloned().collect()
    }

    #[must_use]
    pub fn repair_audit(&self) -> Vec<ReplayRepairAuditRecord> {
        self.repair_audit.clone()
    }

    #[must_use]
    pub fn replay_recovery_status(&self) -> ReplayRecoveryStatus {
        let quarantined = self.quarantine_records();
        let cursor_repairs = self.cursor_repair_requirements();
        let mut degraded = BTreeSet::new();
        let mut structured_log_events = BTreeSet::new();
        let mut repair_actions = BTreeSet::new();

        for record in &quarantined {
            degraded.insert(record.degraded_code);
            for event in &record.structured_log_events {
                structured_log_events.insert(*event);
            }
            repair_actions.insert(quarantine_repair_command(record));
        }

        for repair in &cursor_repairs {
            degraded.insert(repair.degraded_code);
            for event in &repair.structured_log_events {
                structured_log_events.insert(*event);
            }
            repair_actions.insert(cursor_repair_command(repair));
        }

        let status = if !cursor_repairs.is_empty() {
            "cursor_repair_required"
        } else if !quarantined.is_empty() {
            "quarantine_present"
        } else {
            "clear"
        };

        ReplayRecoveryStatus {
            schema: MESH_REPLAY_RECOVERY_SCHEMA_V1,
            status,
            quarantined_event_count: quarantined.len(),
            cursor_repair_required_count: cursor_repairs.len(),
            latest_quarantined_event_id: quarantined
                .last()
                .map(|record| record.incoming_event_id.clone()),
            repair_audit_count: self.repair_audit.len(),
            repair_actions: repair_actions.into_iter().collect(),
            sync_blocked: !quarantined.is_empty() || !cursor_repairs.is_empty(),
            degraded: degraded.into_iter().collect(),
            structured_log_events: structured_log_events.into_iter().collect(),
        }
    }

    pub fn repair_quarantined_event(
        &mut self,
        key: &EventKey,
        action: ReplayRepairAction,
    ) -> Option<ReplayRepairAuditRecord> {
        let record = self.quarantined.get(key)?.clone();
        let origin_node_id = record.key.origin_node_id.clone();
        let cursor_before = self.cursor_for(&origin_node_id);
        if action == ReplayRepairAction::SkipWithAudit {
            self.audited_skips.insert(record.key.clone());
            self.advance_frontier_for(&origin_node_id);
        }
        let cursor_after = self.cursor_for(&origin_node_id);
        let audit = ReplayRepairAuditRecord {
            key: record.key.clone(),
            action,
            degraded_code: MESH_EVENT_QUARANTINED_CODE,
            cursor_before,
            cursor_after,
            structured_log_events: vec![REPAIR_ACTION_LOG],
        };
        self.repair_audit.push(audit.clone());
        Some(audit)
    }

    #[must_use]
    pub fn cursor_repair_requirements(&self) -> Vec<CursorRepairRecord> {
        let origins = self
            .accepted
            .keys()
            .chain(self.audited_skips.iter())
            .map(|key| key.origin_node_id.clone())
            .collect::<BTreeSet<_>>();

        origins
            .into_iter()
            .filter_map(|origin_node_id| {
                let cursor_before = self.cursor_for(&origin_node_id);
                let repaired_cursor = self.repairable_cursor_for(&origin_node_id);
                (repaired_cursor > cursor_before).then_some(CursorRepairRecord {
                    origin_node_id,
                    cursor_before,
                    repaired_cursor,
                    degraded_code: MESH_CURSOR_REPAIR_REQUIRED_CODE,
                    structured_log_events: vec![CURSOR_NOT_ADVANCED_LOG],
                })
            })
            .collect()
    }

    pub fn repair_cursor_after_crash(
        &mut self,
        origin_node_id: &str,
    ) -> Option<CursorRepairRecord> {
        let cursor_before = self.cursor_for(origin_node_id);
        let repaired_cursor = self.repairable_cursor_for(origin_node_id);
        if repaired_cursor <= cursor_before {
            return None;
        }

        self.frontier
            .insert(origin_node_id.to_owned(), repaired_cursor);
        Some(CursorRepairRecord {
            origin_node_id: origin_node_id.to_owned(),
            cursor_before,
            repaired_cursor,
            degraded_code: MESH_CURSOR_REPAIR_REQUIRED_CODE,
            structured_log_events: vec![REPLAY_RECOVERED_LOG],
        })
    }

    /// Return the ranges this node should request from a peer that advertises
    /// `peer_frontier`. Each range starts after this node's durable cursor for
    /// that origin, so gaps cannot be skipped silently.
    #[must_use]
    pub fn ranges_to_request(&self, peer_frontier: &BTreeMap<String, u64>) -> Vec<EventRange> {
        let mut ranges = Vec::new();
        for (origin_node_id, peer_tip) in peer_frontier {
            let local_tip = self.cursor_for(origin_node_id);
            if *peer_tip > local_tip {
                ranges.push(EventRange {
                    origin_node_id: origin_node_id.clone(),
                    start_seq: local_tip + 1,
                    end_seq: *peer_tip,
                });
            }
        }
        ranges
    }

    #[must_use]
    pub fn events_for_range(&self, range: &EventRange) -> Vec<ModelEvent> {
        (range.start_seq..=range.end_seq)
            .filter_map(|seq| {
                self.accepted
                    .get(&EventKey::new(range.origin_node_id.clone(), seq))
                    .cloned()
            })
            .collect()
    }

    #[must_use]
    pub fn tier1_read(&self) -> Tier1Read {
        Tier1Read {
            frontier: self.frontier(),
            logical_heads: self.logical_heads(),
        }
    }

    #[must_use]
    pub fn revision_notice_since(&self, read: &Tier1Read) -> Option<RevisionNotice> {
        let mut advanced_origins = Vec::new();
        for (origin_node_id, to_seq) in &self.frontier {
            let from_seq = read.frontier.get(origin_node_id).copied().unwrap_or(0);
            if *to_seq > from_seq {
                advanced_origins.push(FrontierAdvance {
                    origin_node_id: origin_node_id.clone(),
                    from_seq,
                    to_seq: *to_seq,
                });
            }
        }

        if advanced_origins.is_empty() {
            None
        } else {
            Some(RevisionNotice { advanced_origins })
        }
    }

    #[must_use]
    pub fn logical_conflicts(&self) -> Vec<LogicalConflict> {
        self.logical_heads()
            .into_iter()
            .filter_map(|(logical_memory_id, head_event_ids)| {
                if head_event_ids.len() > 1 {
                    Some(LogicalConflict {
                        logical_memory_id,
                        head_event_ids,
                    })
                } else {
                    None
                }
            })
            .collect()
    }

    #[must_use]
    pub fn logical_visibility_at(&self, now_epoch_ms: u64) -> Vec<LogicalVisibility> {
        let mut events_by_logical: BTreeMap<String, Vec<&ModelEvent>> = BTreeMap::new();
        for event in self.accepted.values() {
            events_by_logical
                .entry(event.logical_memory_id.clone())
                .or_default()
                .push(event);
        }

        let heads = self.logical_heads();
        events_by_logical
            .into_iter()
            .map(|(logical_memory_id, mut events)| {
                events.sort_by(|left, right| {
                    left.key
                        .cmp(&right.key)
                        .then_with(|| left.event_id.cmp(&right.event_id))
                });
                let withdrawn = events
                    .iter()
                    .filter(|event| event.kind == ModelEventKind::ShareWithdraw)
                    .map(|event| event.event_id.clone())
                    .collect::<Vec<_>>();
                let tombstoned = events
                    .iter()
                    .filter(|event| event.kind == ModelEventKind::Tombstone)
                    .map(|event| event.event_id.clone())
                    .collect::<Vec<_>>();
                let expired = events
                    .iter()
                    .filter(|event| {
                        event
                            .valid_until_epoch_ms
                            .is_some_and(|valid_until| valid_until <= now_epoch_ms)
                    })
                    .map(|event| event.event_id.clone())
                    .collect::<Vec<_>>();

                if !withdrawn.is_empty() {
                    return LogicalVisibility {
                        logical_memory_id,
                        status: LogicalVisibilityStatus::Withdrawn,
                        active_head_event_ids: Vec::new(),
                        provenance_event_ids: withdrawn,
                        search_visible: false,
                        context_visible: false,
                        why_provenance_visible: true,
                        body_cache_purge_required: true,
                        residual_metadata_reason: Some(WITHDRAWAL_VISIBILITY_REASON),
                    };
                }
                if !tombstoned.is_empty() {
                    return LogicalVisibility {
                        logical_memory_id,
                        status: LogicalVisibilityStatus::Tombstoned,
                        active_head_event_ids: Vec::new(),
                        provenance_event_ids: tombstoned,
                        search_visible: false,
                        context_visible: false,
                        why_provenance_visible: true,
                        body_cache_purge_required: false,
                        residual_metadata_reason: Some(TOMBSTONE_VISIBILITY_REASON),
                    };
                }
                if !expired.is_empty() {
                    return LogicalVisibility {
                        logical_memory_id,
                        status: LogicalVisibilityStatus::Expired,
                        active_head_event_ids: Vec::new(),
                        provenance_event_ids: expired,
                        search_visible: false,
                        context_visible: false,
                        why_provenance_visible: true,
                        body_cache_purge_required: false,
                        residual_metadata_reason: Some(VALIDITY_EXPIRY_VISIBILITY_REASON),
                    };
                }

                LogicalVisibility {
                    logical_memory_id: logical_memory_id.clone(),
                    status: LogicalVisibilityStatus::Active,
                    active_head_event_ids: heads
                        .get(&logical_memory_id)
                        .cloned()
                        .unwrap_or_default(),
                    provenance_event_ids: Vec::new(),
                    search_visible: true,
                    context_visible: true,
                    why_provenance_visible: true,
                    body_cache_purge_required: false,
                    residual_metadata_reason: None,
                }
            })
            .collect()
    }

    #[must_use]
    pub fn convergence_digest(&self) -> String {
        let events = self.accepted_event_ids().join(",");
        let frontier = self
            .frontier
            .iter()
            .map(|(origin, seq)| format!("{origin}:{seq}"))
            .collect::<Vec<_>>()
            .join(",");
        let conflicts = self
            .logical_conflicts()
            .into_iter()
            .map(|conflict| {
                format!(
                    "{}:{}",
                    conflict.logical_memory_id,
                    conflict.head_event_ids.join("+")
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        format!("events=[{events}];frontier=[{frontier}];conflicts=[{conflicts}]")
    }

    #[must_use]
    pub fn replay_deterministically(events: impl IntoIterator<Item = ModelEvent>) -> Self {
        let mut events = events.into_iter().collect::<Vec<_>>();
        events.sort_by(|left, right| {
            left.key
                .cmp(&right.key)
                .then_with(|| left.event_id.cmp(&right.event_id))
        });
        let mut node = Self::new();
        for event in events {
            let _ = node.replay(event);
        }
        node
    }

    fn advance_frontier_for(&mut self, origin_node_id: &str) {
        let repaired_cursor = self.repairable_cursor_for(origin_node_id);
        if repaired_cursor > self.cursor_for(origin_node_id) {
            self.frontier
                .insert(origin_node_id.to_owned(), repaired_cursor);
        }
    }

    fn quarantine_event(
        &mut self,
        key: EventKey,
        incoming_event_id: String,
        reason: ReplayQuarantineReason,
    ) -> ReplayOutcome {
        let cursor_before = self.cursor_for(&key.origin_node_id);
        let record = ReplayQuarantineRecord {
            key: key.clone(),
            incoming_event_id,
            reason,
            degraded_code: MESH_EVENT_QUARANTINED_CODE,
            cursor_before,
            cursor_after: cursor_before,
            repair_actions: vec![
                ReplayRepairAction::Retry,
                ReplayRepairAction::SkipWithAudit,
                ReplayRepairAction::RevokePeer,
                ReplayRepairAction::ResetCache,
            ],
            structured_log_events: vec![QUARANTINE_ENTERED_LOG, CURSOR_NOT_ADVANCED_LOG],
        };
        self.quarantined.insert(key, record.clone());
        ReplayOutcome::Quarantined(record)
    }

    fn repairable_cursor_for(&self, origin_node_id: &str) -> u64 {
        let mut cursor = self.cursor_for(origin_node_id);
        if cursor == u64::MAX {
            return cursor;
        }

        let mut next_seq = cursor.saturating_add(1);
        while self.has_durable_or_audited_skip(origin_node_id, next_seq) {
            cursor = next_seq;
            if cursor == u64::MAX {
                break;
            }
            next_seq = next_seq.saturating_add(1);
        }
        cursor
    }

    fn has_durable_or_audited_skip(&self, origin_node_id: &str, seq: u64) -> bool {
        let key = EventKey::new(origin_node_id.to_owned(), seq);
        self.accepted.contains_key(&key) || self.audited_skips.contains(&key)
    }

    fn logical_heads(&self) -> BTreeMap<String, Vec<String>> {
        let mut events_by_logical: BTreeMap<String, Vec<&ModelEvent>> = BTreeMap::new();
        let mut referenced_bases: BTreeSet<String> = BTreeSet::new();

        for event in self.accepted.values() {
            events_by_logical
                .entry(event.logical_memory_id.clone())
                .or_default()
                .push(event);
            if let Some(base_event_id) = &event.base_event_id {
                referenced_bases.insert(base_event_id.clone());
            }
        }

        events_by_logical
            .into_iter()
            .map(|(logical_memory_id, events)| {
                let mut heads = events
                    .into_iter()
                    .filter(|event| !referenced_bases.contains(&event.event_id))
                    .map(|event| event.event_id.clone())
                    .collect::<Vec<_>>();
                heads.sort();
                (logical_memory_id, heads)
            })
            .collect()
    }
}

fn quarantine_repair_command(record: &ReplayQuarantineRecord) -> String {
    let actions = record
        .repair_actions
        .iter()
        .map(|action| action.as_cli_arg())
        .collect::<Vec<_>>()
        .join("|");
    format!(
        "ee mesh repair --event {} --action {actions} --json",
        record.incoming_event_id
    )
}

fn cursor_repair_command(record: &CursorRepairRecord) -> String {
    format!(
        "ee mesh repair --cursor --origin {} --from {} --to {} --json",
        record.origin_node_id, record.cursor_before, record.repaired_cursor
    )
}

fn render_visibility_text(
    visibility: &LogicalVisibility,
    surface: MeshReadSurface,
    included: bool,
    body_visible: bool,
) -> String {
    let reason = visibility
        .residual_metadata_reason
        .unwrap_or("active_mesh_memory");
    match (surface, visibility.status, included, body_visible) {
        (MeshReadSurface::Search, LogicalVisibilityStatus::Active, true, true) => format!(
            "mesh memory {} is searchable; active heads={}.",
            visibility.logical_memory_id,
            event_list(&visibility.active_head_event_ids)
        ),
        (MeshReadSurface::Context, LogicalVisibilityStatus::Active, true, true) => format!(
            "mesh memory {} may enter context; active heads={}.",
            visibility.logical_memory_id,
            event_list(&visibility.active_head_event_ids)
        ),
        (MeshReadSurface::Why, LogicalVisibilityStatus::Active, true, false) => format!(
            "mesh memory {} remains active; why may cite active heads={}.",
            visibility.logical_memory_id,
            event_list(&visibility.active_head_event_ids)
        ),
        (MeshReadSurface::Search, _, false, false) => format!(
            "mesh memory {} is suppressed from search because status={} reason={reason}.",
            visibility.logical_memory_id,
            visibility.status.as_str()
        ),
        (MeshReadSurface::Context, _, false, false) => format!(
            "mesh memory {} is suppressed from context because status={} reason={reason}.",
            visibility.logical_memory_id,
            visibility.status.as_str()
        ),
        (MeshReadSurface::Why, _, true, false) => format!(
            "mesh memory {} keeps why provenance for status={} via events={}; body_visible=false; purge_required={}.",
            visibility.logical_memory_id,
            visibility.status.as_str(),
            event_list(&visibility.provenance_event_ids),
            visibility.body_cache_purge_required
        ),
        _ => format!(
            "mesh memory {} has surface={} status={} included={} body_visible={} reason={reason}.",
            visibility.logical_memory_id,
            surface.as_str(),
            visibility.status.as_str(),
            included,
            body_visible
        ),
    }
}

fn event_list(events: &[String]) -> String {
    if events.is_empty() {
        "[]".to_owned()
    } else {
        format!("[{}]", events.join(","))
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ANTI_ENTROPY_MODEL_SCENARIOS, CURSOR_NOT_ADVANCED_LOG, EventRange, LogicalVisibilityStatus,
        MESH_CURSOR_REPAIR_REQUIRED_CODE, MESH_EVENT_QUARANTINED_CODE,
        MESH_LOGICAL_VISIBILITY_RENDER_SCHEMA_V1, MESH_REPLAY_RECOVERY_SCHEMA_V1, MeshReadSurface,
        ModelEvent, ModelEventKind, ModelNode, QUARANTINE_ENTERED_LOG, REPAIR_ACTION_LOG,
        REPLAY_RECOVERED_LOG, ReplayOutcome, ReplayQuarantineReason, ReplayRepairAction,
        ReplayValidation, TOMBSTONE_VISIBILITY_REASON, VALIDITY_EXPIRY_VISIBILITY_REASON,
        WITHDRAWAL_VISIBILITY_REASON,
    };

    fn event(
        origin: &str,
        seq: u64,
        logical_memory_id: &str,
        base_event_id: Option<&str>,
        content_hash: &str,
    ) -> ModelEvent {
        ModelEvent::new(origin, seq, logical_memory_id, base_event_id, content_hash)
    }

    fn event_kind(
        origin: &str,
        seq: u64,
        logical_memory_id: &str,
        base_event_id: Option<&str>,
        content_hash: &str,
        kind: ModelEventKind,
    ) -> ModelEvent {
        ModelEvent::new_with_kind(
            origin,
            seq,
            logical_memory_id,
            base_event_id,
            content_hash,
            kind,
        )
    }

    #[test]
    fn cursor_advances_only_after_contiguous_replay() {
        let scenario = "cursor_advances_only_after_contiguous_replay";
        assert!(ANTI_ENTROPY_MODEL_SCENARIOS.contains(&scenario));

        let mut node = ModelNode::new();
        let first = event("node_a", 1, "mem_release_rule", None, "hash_a1");
        let second = event(
            "node_a",
            2,
            "mem_release_rule",
            Some(first.event_id.as_str()),
            "hash_a2",
        );

        assert_eq!(node.replay(second.clone()), ReplayOutcome::Accepted);
        assert_eq!(
            node.cursor_for("node_a"),
            0,
            "{scenario}: cursor cannot skip missing seq=1"
        );

        assert_eq!(node.replay(first.clone()), ReplayOutcome::Accepted);
        assert_eq!(node.cursor_for("node_a"), 2);
        assert_eq!(node.replay(first), ReplayOutcome::Duplicate);
        assert_eq!(node.accepted_event_ids().len(), 2);

        let mut accepted_node = ModelNode::new();
        assert_eq!(
            accepted_node.replay_with_validation(
                event("node_accept", 1, "mem_accept", None, "hash_accept"),
                ReplayValidation::Accept,
            ),
            ReplayOutcome::Accepted
        );
    }

    #[test]
    fn partition_rejoin_duplicate_out_of_order_delivery_converges() {
        let scenario = "partition_rejoin_duplicate_out_of_order_delivery";
        assert!(ANTI_ENTROPY_MODEL_SCENARIOS.contains(&scenario));

        let a1 = event("node_a", 1, "mem_release_rule", None, "hash_a1");
        let a2 = event(
            "node_a",
            2,
            "mem_release_rule",
            Some(a1.event_id.as_str()),
            "hash_a2",
        );
        let b1 = event("node_b", 1, "mem_review_rule", None, "hash_b1");

        let mut node_a = ModelNode::new();
        let mut node_b = ModelNode::new();
        assert_eq!(node_a.replay(a1.clone()), ReplayOutcome::Accepted);
        assert_eq!(node_a.replay(a2.clone()), ReplayOutcome::Accepted);
        assert_eq!(node_b.replay(b1.clone()), ReplayOutcome::Accepted);

        let ranges = node_b.ranges_to_request(&node_a.frontier());
        assert_eq!(
            ranges,
            vec![EventRange {
                origin_node_id: "node_a".to_owned(),
                start_seq: 1,
                end_seq: 2
            }]
        );

        assert_eq!(node_b.replay(a2.clone()), ReplayOutcome::Accepted);
        assert_eq!(node_b.cursor_for("node_a"), 0);
        assert_eq!(node_b.replay(a1.clone()), ReplayOutcome::Accepted);
        assert_eq!(node_b.cursor_for("node_a"), 2);
        assert_eq!(node_b.replay(a1.clone()), ReplayOutcome::Duplicate);

        let oracle = ModelNode::replay_deterministically([a1, a2, b1]);
        assert_eq!(
            node_b.convergence_digest(),
            oracle.convergence_digest(),
            "{scenario}: fixed accepted event set must converge"
        );
    }

    #[test]
    fn conflicting_revisions_are_visible_not_overwritten() {
        let scenario = "conflicting_revisions_are_visible";
        assert!(ANTI_ENTROPY_MODEL_SCENARIOS.contains(&scenario));

        let left = event("node_a", 1, "mem_shared_policy", None, "hash_left");
        let right = event("node_b", 1, "mem_shared_policy", None, "hash_right");
        let mut node = ModelNode::new();

        assert_eq!(node.replay(left.clone()), ReplayOutcome::Accepted);
        assert_eq!(node.replay(right.clone()), ReplayOutcome::Accepted);

        let conflicts = node.logical_conflicts();
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].logical_memory_id, "mem_shared_policy");
        assert_eq!(
            conflicts[0].head_event_ids,
            vec![left.event_id.clone(), right.event_id.clone()]
        );
        assert_eq!(
            node.tier1_read().logical_heads["mem_shared_policy"],
            vec![left.event_id, right.event_id],
            "{scenario}: both heads remain visible to callers"
        );
    }

    #[test]
    fn stale_tier1_read_gets_revision_notice_after_import() {
        let scenario = "stale_tier1_read_gets_revision_notice";
        assert!(ANTI_ENTROPY_MODEL_SCENARIOS.contains(&scenario));

        let first = event("node_a", 1, "mem_release_rule", None, "hash_a1");
        let second = event(
            "node_a",
            2,
            "mem_release_rule",
            Some(first.event_id.as_str()),
            "hash_a2",
        );
        let mut node = ModelNode::new();
        assert_eq!(node.replay(first), ReplayOutcome::Accepted);

        let read = node.tier1_read();
        assert_eq!(read.frontier["node_a"], 1);
        assert_eq!(node.revision_notice_since(&read), None);

        assert_eq!(node.replay(second), ReplayOutcome::Accepted);
        let notice = node
            .revision_notice_since(&read)
            .expect("revision notice after newer durable replay");
        assert_eq!(notice.advanced_origins.len(), 1);
        assert_eq!(notice.advanced_origins[0].origin_node_id, "node_a");
        assert_eq!(notice.advanced_origins[0].from_seq, 1);
        assert_eq!(notice.advanced_origins[0].to_seq, 2);
        assert_eq!(
            read.frontier["node_a"], 1,
            "{scenario}: returned Tier 1 read is not mutated silently"
        );
    }

    #[test]
    fn deterministic_replay_is_order_independent() {
        let scenario = "deterministic_replay_order_independent";
        assert!(ANTI_ENTROPY_MODEL_SCENARIOS.contains(&scenario));

        let a1 = event("node_a", 1, "mem_release_rule", None, "hash_a1");
        let a2 = event(
            "node_a",
            2,
            "mem_release_rule",
            Some(a1.event_id.as_str()),
            "hash_a2",
        );
        let b1 = event("node_b", 1, "mem_shared_policy", None, "hash_b1");
        let c1 = event("node_c", 1, "mem_shared_policy", None, "hash_c1");

        let canonical =
            ModelNode::replay_deterministically([a1.clone(), a2.clone(), b1.clone(), c1.clone()]);
        let shuffled = ModelNode::replay_deterministically([c1, a2, b1, a1]);

        assert_eq!(
            canonical.convergence_digest(),
            shuffled.convergence_digest()
        );
        assert_eq!(canonical.logical_conflicts().len(), 1);
    }

    #[test]
    fn withdrawal_propagates_as_provenance_tombstone() {
        let scenario = "withdrawal_propagates_as_provenance_tombstone";
        assert!(ANTI_ENTROPY_MODEL_SCENARIOS.contains(&scenario));

        let create = event("node_a", 1, "mem_shared_body", None, "hash_body");
        let withdraw = event_kind(
            "node_a",
            2,
            "mem_shared_body",
            Some(create.event_id.as_str()),
            "hash_withdraw",
            ModelEventKind::ShareWithdraw,
        );
        let node = ModelNode::replay_deterministically([withdraw.clone(), create]);

        let visibility = node.logical_visibility_at(10_000);
        assert_eq!(visibility.len(), 1);
        let decision = &visibility[0];
        assert_eq!(decision.status, LogicalVisibilityStatus::Withdrawn);
        assert_eq!(decision.status.as_str(), "withdrawn");
        assert!(decision.active_head_event_ids.is_empty());
        assert_eq!(decision.provenance_event_ids, vec![withdraw.event_id]);
        assert!(!decision.search_visible);
        assert!(!decision.context_visible);
        assert!(decision.why_provenance_visible);
        assert!(decision.body_cache_purge_required);
        assert_eq!(
            decision.residual_metadata_reason,
            Some(WITHDRAWAL_VISIBILITY_REASON)
        );
    }

    #[test]
    fn withdrawn_remote_material_renders_search_context_why_contract() {
        let scenario = "withdrawn_remote_material_renders_search_context_why_contract";
        assert!(ANTI_ENTROPY_MODEL_SCENARIOS.contains(&scenario));

        let create = event("node_a", 1, "mem_shared_body", None, "hash_body");
        let withdraw = event_kind(
            "node_a",
            2,
            "mem_shared_body",
            Some(create.event_id.as_str()),
            "hash_withdraw",
            ModelEventKind::ShareWithdraw,
        );
        let node = ModelNode::replay_deterministically([create, withdraw.clone()]);
        let visibility = node.logical_visibility_at(10_000);
        let renders = visibility[0].render_all_surfaces();

        assert_eq!(renders.len(), 3);
        for render in &renders {
            assert_eq!(render.schema, MESH_LOGICAL_VISIBILITY_RENDER_SCHEMA_V1);
            assert_eq!(render.logical_memory_id, "mem_shared_body");
            assert_eq!(render.status, LogicalVisibilityStatus::Withdrawn);
            assert!(!render.body_visible);
            assert_eq!(
                render.residual_metadata_reason,
                Some(WITHDRAWAL_VISIBILITY_REASON)
            );
        }

        let search = &renders[0];
        assert_eq!(search.surface, MeshReadSurface::Search);
        assert!(!search.included);
        assert!(!search.provenance_visible);
        assert!(search.selected_event_ids.is_empty());
        assert!(search.render_text.contains("suppressed from search"));

        let context = &renders[1];
        assert_eq!(context.surface, MeshReadSurface::Context);
        assert!(!context.included);
        assert!(!context.provenance_visible);
        assert!(context.selected_event_ids.is_empty());
        assert!(context.render_text.contains("suppressed from context"));

        let why = &renders[2];
        assert_eq!(why.surface, MeshReadSurface::Why);
        assert!(why.included);
        assert!(why.provenance_visible);
        assert!(why.body_cache_purge_required);
        assert_eq!(why.selected_event_ids, vec![withdraw.event_id]);
        assert!(why.render_text.contains("keeps why provenance"));
        assert!(why.render_text.contains("body_visible=false"));
        assert!(why.render_text.contains("purge_required=true"));
    }

    #[test]
    fn validity_expiry_filters_without_peer_cache_purge() {
        let scenario = "validity_expiry_filters_without_peer_cache_purge";
        assert!(ANTI_ENTROPY_MODEL_SCENARIOS.contains(&scenario));

        let validity = event_kind(
            "node_a",
            1,
            "mem_timeboxed",
            None,
            "hash_validity",
            ModelEventKind::Validity,
        )
        .with_valid_until_epoch_ms(5_000);
        let node = ModelNode::replay_deterministically([validity.clone()]);

        let active = node.logical_visibility_at(4_999);
        assert_eq!(active[0].status, LogicalVisibilityStatus::Active);
        assert!(active[0].search_visible);
        assert!(!active[0].body_cache_purge_required);

        let expired = node.logical_visibility_at(5_000);
        assert_eq!(expired[0].status, LogicalVisibilityStatus::Expired);
        assert_eq!(expired[0].provenance_event_ids, vec![validity.event_id]);
        assert!(!expired[0].search_visible);
        assert!(!expired[0].context_visible);
        assert!(expired[0].why_provenance_visible);
        assert!(!expired[0].body_cache_purge_required);
        assert_eq!(
            expired[0].residual_metadata_reason,
            Some(VALIDITY_EXPIRY_VISIBILITY_REASON)
        );
    }

    #[test]
    fn tombstone_hides_from_search_without_body_purge() {
        let scenario = "tombstone_hides_from_search_without_body_purge";
        assert!(ANTI_ENTROPY_MODEL_SCENARIOS.contains(&scenario));

        let create = event("node_a", 1, "mem_tombstoned", None, "hash_body");
        let tombstone = event_kind(
            "node_a",
            2,
            "mem_tombstoned",
            Some(create.event_id.as_str()),
            "hash_tombstone",
            ModelEventKind::Tombstone,
        );
        let node = ModelNode::replay_deterministically([create, tombstone.clone()]);

        let visibility = node.logical_visibility_at(10_000);
        assert_eq!(visibility[0].status, LogicalVisibilityStatus::Tombstoned);
        assert_eq!(visibility[0].provenance_event_ids, vec![tombstone.event_id]);
        assert!(!visibility[0].search_visible);
        assert!(!visibility[0].context_visible);
        assert!(visibility[0].why_provenance_visible);
        assert!(!visibility[0].body_cache_purge_required);
        assert_eq!(
            visibility[0].residual_metadata_reason,
            Some(TOMBSTONE_VISIBILITY_REASON)
        );
    }

    #[test]
    fn withdrawal_wins_over_tombstone_and_validity_expiry() {
        let scenario = "withdrawal_wins_over_tombstone_and_validity_expiry";
        assert!(ANTI_ENTROPY_MODEL_SCENARIOS.contains(&scenario));

        let create = event("node_a", 1, "mem_lifecycle", None, "hash_create");
        let expired_validity = event_kind(
            "node_a",
            2,
            "mem_lifecycle",
            Some(create.event_id.as_str()),
            "hash_validity",
            ModelEventKind::Validity,
        )
        .with_valid_until_epoch_ms(5_000);
        let tombstone = event_kind(
            "node_a",
            3,
            "mem_lifecycle",
            Some(expired_validity.event_id.as_str()),
            "hash_tombstone",
            ModelEventKind::Tombstone,
        );
        let withdraw = event_kind(
            "node_a",
            4,
            "mem_lifecycle",
            Some(tombstone.event_id.as_str()),
            "hash_withdraw",
            ModelEventKind::ShareWithdraw,
        );
        let node = ModelNode::replay_deterministically([
            tombstone,
            withdraw.clone(),
            create,
            expired_validity,
        ]);

        let visibility = node.logical_visibility_at(10_000);
        assert_eq!(visibility.len(), 1);
        assert_eq!(visibility[0].status, LogicalVisibilityStatus::Withdrawn);
        assert_eq!(visibility[0].provenance_event_ids, vec![withdraw.event_id]);
        assert!(!visibility[0].search_visible);
        assert!(!visibility[0].context_visible);
        assert!(visibility[0].why_provenance_visible);
        assert!(visibility[0].body_cache_purge_required);
        assert_eq!(
            visibility[0].residual_metadata_reason,
            Some(WITHDRAWAL_VISIBILITY_REASON)
        );
    }

    #[test]
    fn malformed_hash_body_policy_schema_events_enter_quarantine() {
        let scenario = "malformed_hash_body_policy_schema_events_enter_quarantine";
        assert!(ANTI_ENTROPY_MODEL_SCENARIOS.contains(&scenario));

        let cases = [
            (ReplayQuarantineReason::MalformedEvent, "malformed_event"),
            (
                ReplayQuarantineReason::HashChainMismatch,
                "hash_chain_mismatch",
            ),
            (
                ReplayQuarantineReason::BodyHashMismatch,
                "body_hash_mismatch",
            ),
            (ReplayQuarantineReason::PolicyDenied, "policy_denied"),
            (
                ReplayQuarantineReason::IncompatibleSchema,
                "incompatible_schema",
            ),
        ];

        for (idx, (reason, reason_name)) in cases.into_iter().enumerate() {
            let mut node = ModelNode::new();
            let incoming = event(
                "node_a",
                idx as u64 + 1,
                "mem_rejected_peer_event",
                None,
                reason_name,
            );
            let outcome =
                node.replay_with_validation(incoming.clone(), ReplayValidation::Quarantine(reason));

            let ReplayOutcome::Quarantined(record) = outcome else {
                panic!("{scenario}: {reason_name} must fail closed into quarantine");
            };
            assert_eq!(reason.as_str(), reason_name);
            assert_eq!(record.key, incoming.key);
            assert_eq!(record.incoming_event_id, incoming.event_id);
            assert_eq!(record.reason, reason);
            assert_eq!(record.degraded_code, MESH_EVENT_QUARANTINED_CODE);
            assert_eq!(record.cursor_before, 0);
            assert_eq!(record.cursor_after, 0);
            assert_eq!(
                record.structured_log_events,
                vec![QUARANTINE_ENTERED_LOG, CURSOR_NOT_ADVANCED_LOG]
            );
            assert_eq!(node.cursor_for("node_a"), 0);
            assert!(node.accepted_event_ids().is_empty());
            assert_eq!(node.quarantine_records(), vec![record]);
        }
    }

    #[test]
    fn crash_after_insert_before_cursor_requires_repair() {
        let scenario = "crash_after_insert_before_cursor_requires_repair";
        assert!(ANTI_ENTROPY_MODEL_SCENARIOS.contains(&scenario));

        let crashed = event("node_a", 1, "mem_crash_recovery", None, "hash_crashed");
        let mut node = ModelNode::new();
        node.accepted.insert(crashed.key.clone(), crashed);
        assert_eq!(node.cursor_for("node_a"), 0);

        let requirements = node.cursor_repair_requirements();
        assert_eq!(requirements.len(), 1);
        assert_eq!(requirements[0].origin_node_id, "node_a");
        assert_eq!(requirements[0].cursor_before, 0);
        assert_eq!(requirements[0].repaired_cursor, 1);
        assert_eq!(
            requirements[0].degraded_code,
            MESH_CURSOR_REPAIR_REQUIRED_CODE
        );
        assert_eq!(
            requirements[0].structured_log_events,
            vec![CURSOR_NOT_ADVANCED_LOG]
        );

        let repaired = node
            .repair_cursor_after_crash("node_a")
            .expect("repairable cursor after crash");
        assert_eq!(repaired.cursor_before, 0);
        assert_eq!(repaired.repaired_cursor, 1);
        assert_eq!(repaired.structured_log_events, vec![REPLAY_RECOVERED_LOG]);
        assert_eq!(node.cursor_for("node_a"), 1);
        assert!(node.cursor_repair_requirements().is_empty());
    }

    #[test]
    fn replay_recovery_status_reports_quarantine_and_cursor_repair() {
        let mut node = ModelNode::new();
        let rejected = event("node_a", 1, "mem_quarantined", None, "hash_policy_denied");
        let rejected_event_id = rejected.event_id.clone();
        let _ = node.replay_with_validation(
            rejected,
            ReplayValidation::Quarantine(ReplayQuarantineReason::PolicyDenied),
        );

        let crashed = event("node_b", 1, "mem_crash_recovery", None, "hash_crashed");
        node.accepted.insert(crashed.key.clone(), crashed);

        let status = node.replay_recovery_status();

        assert_eq!(status.schema, MESH_REPLAY_RECOVERY_SCHEMA_V1);
        assert_eq!(status.status, "cursor_repair_required");
        assert_eq!(status.quarantined_event_count, 1);
        assert_eq!(status.cursor_repair_required_count, 1);
        assert_eq!(
            status.latest_quarantined_event_id.as_deref(),
            Some(rejected_event_id.as_str())
        );
        assert_eq!(status.repair_audit_count, 0);
        assert!(status.sync_blocked);
        assert_eq!(
            status.degraded,
            vec![
                MESH_CURSOR_REPAIR_REQUIRED_CODE,
                MESH_EVENT_QUARANTINED_CODE
            ]
        );
        assert_eq!(
            status.structured_log_events,
            vec![CURSOR_NOT_ADVANCED_LOG, QUARANTINE_ENTERED_LOG]
        );
        assert!(
            status
                .repair_actions
                .iter()
                .any(|command| command.contains("--event")
                    && command.contains(rejected_event_id.as_str())
                    && command.contains("retry|skip-with-audit|revoke-peer|reset-cache"))
        );
        assert!(
            status.repair_actions.iter().any(|command| command
                == "ee mesh repair --cursor --origin node_b --from 0 --to 1 --json")
        );
    }

    #[test]
    fn quarantine_repair_actions_are_audited() {
        let scenario = "quarantine_repair_actions_are_audited";
        assert!(ANTI_ENTROPY_MODEL_SCENARIOS.contains(&scenario));

        let mut node = ModelNode::new();
        let rejected = event("node_a", 1, "mem_repairable", None, "hash_bad");
        let key = rejected.key.clone();
        let outcome = node.replay_with_validation(
            rejected,
            ReplayValidation::Quarantine(ReplayQuarantineReason::BodyHashMismatch),
        );
        let ReplayOutcome::Quarantined(record) = outcome else {
            panic!("{scenario}: body hash mismatch must quarantine");
        };
        assert_eq!(
            record.repair_actions,
            vec![
                ReplayRepairAction::Retry,
                ReplayRepairAction::SkipWithAudit,
                ReplayRepairAction::RevokePeer,
                ReplayRepairAction::ResetCache,
            ]
        );
        for action in [
            ReplayRepairAction::Retry,
            ReplayRepairAction::RevokePeer,
            ReplayRepairAction::ResetCache,
        ] {
            let audit = node
                .repair_quarantined_event(&key, action)
                .expect("quarantined event can be repaired");
            assert!(!action.as_str().is_empty());
            assert_eq!(audit.action, action);
            assert_eq!(audit.degraded_code, MESH_EVENT_QUARANTINED_CODE);
            assert_eq!(audit.cursor_before, 0);
            assert_eq!(audit.cursor_after, 0);
            assert_eq!(audit.structured_log_events, vec![REPAIR_ACTION_LOG]);
        }

        let skip = node
            .repair_quarantined_event(&key, ReplayRepairAction::SkipWithAudit)
            .expect("quarantined event can be skipped with audit");
        assert_eq!(skip.cursor_before, 0);
        assert_eq!(skip.cursor_after, 1);
        assert_eq!(node.cursor_for("node_a"), 1);
        assert_eq!(
            node.repair_audit()
                .into_iter()
                .map(|audit| audit.action)
                .collect::<Vec<_>>(),
            vec![
                ReplayRepairAction::Retry,
                ReplayRepairAction::RevokePeer,
                ReplayRepairAction::ResetCache,
                ReplayRepairAction::SkipWithAudit,
            ]
        );
    }
}
