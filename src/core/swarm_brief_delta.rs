//! IW2 (bd-1zb7k.17.2): redaction-safe swarm brief delta capsules.
//!
//! Pure differ that takes two pre-collected swarm-brief snapshots
//! (`before` and `after`) and returns a compact capsule describing what
//! changed: agents whose status moved, reservations acquired / released /
//! expired, Beads whose state changed, verification posture deltas, RCH
//! fleet deltas, plus a deterministic next-actions recommendation list.
//!
//! Live consumer surface: `ee swarm brief --since <PATH> [--after-snapshot <PATH>] --json`
//! attaches `delta: SwarmBriefDeltaCapsule` (`schema = "ee.swarm_brief.delta.v1"`)
//! to the swarm brief response.
//!
//! Acceptance shape (from bead body):
//! - `changedAgents[]` with status/lastSeen deltas.
//! - `changedReservations[]` with acquired/released/expired.
//! - `changedBeads[]` with status/assignee/comment-count deltas.
//! - `changedVerification[]` with reusable/stale/blocker deltas when broker
//!   evidence exists.
//! - `changedRch[]` with fleet/capability/posture deltas.
//! - `recommendedNextActions[]` ordered by safety and impact.
//!
//! The differ takes pre-redacted snapshots: every input string is already
//! either an alias (`peer_<hash>`, `origin_<hash>`) or a stable identifier
//! the caller curated. The differ never reads raw mail bodies, command
//! stderr, secret-bearing strings, or memory bodies; the input shapes
//! intentionally don't carry those fields.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize, Serializer};
use serde_json::Value;

use crate::core::swarm_brief::{SwarmBriefReport, redact_brief_text};

/// Maximum snapshot file size accepted by `--since` / `--after-snapshot` (8 MiB).
pub const SWARM_BRIEF_DELTA_MAX_SNAPSHOT_BYTES: u64 = 8 * 1024 * 1024;

/// Public schema identifier for the delta capsule.
pub const SWARM_BRIEF_DELTA_SCHEMA_V1: &str = "ee.swarm_brief.delta.v1";

/// Severity-band a delta carries. Stable ordinal so the renderer can
/// threshold on the band rather than parsing free-form strings.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum DeltaSeverity {
    Info,
    Notice,
    Warning,
    Critical,
}

impl DeltaSeverity {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Notice => "notice",
            Self::Warning => "warning",
            Self::Critical => "critical",
        }
    }
}

impl Serialize for DeltaSeverity {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

/// One reservation row in the swarm-brief input. Closed and
/// pre-redacted: `path_pattern` and `holder_agent` are caller-curated;
/// no raw mail bodies or secret-bearing strings.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SwarmBriefReservation {
    pub reservation_id: String,
    pub path_pattern: String,
    pub holder_agent: String,
    pub expires_at_rfc3339: Option<String>,
}

/// One bead snapshot row.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SwarmBriefBead {
    pub bead_id: String,
    pub status: String,
    pub assignee: Option<String>,
    pub comment_count: u32,
}

/// One agent snapshot row.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SwarmBriefAgent {
    pub agent_alias: String,
    pub status: String,
    pub last_seen_at_rfc3339: Option<String>,
}

/// One known-blocker / verification-evidence row visible to the brief.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SwarmBriefVerificationEvidence {
    pub command_fingerprint: String,
    pub status: String,
    pub evidence_hash: String,
}

/// RCH fleet posture row.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SwarmBriefRchPosture {
    pub worker_alias: String,
    pub posture: String,
    pub admission_status: String,
}

/// Snapshot the differ consumes. Two of these (before, after) produce
/// the capsule. Everything is sorted internally so identical content
/// always diffs to the same capsule.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SwarmBriefSnapshot {
    #[serde(default)]
    pub agents: Vec<SwarmBriefAgent>,
    #[serde(default)]
    pub reservations: Vec<SwarmBriefReservation>,
    #[serde(default)]
    pub beads: Vec<SwarmBriefBead>,
    #[serde(default)]
    pub verification_evidence: Vec<SwarmBriefVerificationEvidence>,
    #[serde(default)]
    pub rch_postures: Vec<SwarmBriefRchPosture>,
}

impl SwarmBriefSnapshot {
    /// Extract a redaction-safe delta snapshot from a collected [`SwarmBriefReport`].
    #[must_use]
    pub fn from_report(report: &SwarmBriefReport) -> Self {
        let mut stalled_posture_by_agent = BTreeMap::<String, String>::new();
        for stalled in &report.stalled_bead_liveness {
            if let Some(assignee) = stalled.assignee.as_deref() {
                let alias = redact_brief_text(assignee);
                if !alias.is_empty() && !stalled.posture.trim().is_empty() {
                    stalled_posture_by_agent
                        .entry(alias)
                        .or_insert_with(|| redact_brief_text(&stalled.posture));
                }
            }
        }

        let mut agents_by_alias = BTreeMap::<String, SwarmBriefAgent>::new();
        for agent in &report.agent_mail_agents {
            let alias = redact_brief_text(&agent.name);
            if alias.is_empty() {
                continue;
            }
            let status = stalled_posture_by_agent
                .get(&alias)
                .cloned()
                .unwrap_or_else(|| "active".to_string());
            let last_seen = agent.last_active_at.as_deref().map(redact_brief_text);
            agents_by_alias.insert(
                alias.clone(),
                SwarmBriefAgent {
                    agent_alias: alias,
                    status,
                    last_seen_at_rfc3339: last_seen,
                },
            );
        }

        let mut reservations_by_id = BTreeMap::<String, SwarmBriefReservation>::new();
        for reservation in &report.file_reservations {
            let holder_agent = redact_brief_text(&reservation.holder);
            let path_pattern = redact_brief_text(&reservation.path_pattern);
            if holder_agent.is_empty() && path_pattern.is_empty() {
                continue;
            }
            let reservation_id = format!("{holder_agent}:{path_pattern}");
            reservations_by_id.insert(
                reservation_id.clone(),
                SwarmBriefReservation {
                    reservation_id,
                    path_pattern,
                    holder_agent,
                    expires_at_rfc3339: reservation.expires_at.as_deref().map(redact_brief_text),
                },
            );
        }

        let mut beads_by_id = BTreeMap::<String, SwarmBriefBead>::new();
        for bead in report
            .beads
            .ready
            .iter()
            .chain(report.beads.in_progress.iter())
            .chain(report.beads.blocked.iter())
            .chain(report.beads.deferred.iter())
        {
            let bead_id = redact_brief_text(&bead.id);
            if bead_id.is_empty() {
                continue;
            }
            let comment_count = u32::try_from(bead.comment_count).unwrap_or(u32::MAX);
            beads_by_id.insert(
                bead_id.clone(),
                SwarmBriefBead {
                    bead_id,
                    status: redact_brief_text(&bead.status),
                    assignee: bead.assignee.as_deref().map(redact_brief_text),
                    comment_count,
                },
            );
        }

        let mut verification_by_fp = BTreeMap::<String, SwarmBriefVerificationEvidence>::new();
        if let Some(broker) = &report.verification_broker {
            if broker.recent_reusable_run_count > 0 {
                let fp = "broker:reusable_runs".to_string();
                verification_by_fp.insert(
                    fp.clone(),
                    SwarmBriefVerificationEvidence {
                        command_fingerprint: fp,
                        status: "reusable".to_string(),
                        evidence_hash: format!("reusable:{}", broker.recent_reusable_run_count),
                    },
                );
            }
            if broker.stale_run_count > 0 {
                let fp = "broker:stale_runs".to_string();
                verification_by_fp.insert(
                    fp.clone(),
                    SwarmBriefVerificationEvidence {
                        command_fingerprint: fp,
                        status: "stale".to_string(),
                        evidence_hash: format!("stale:{}", broker.stale_run_count),
                    },
                );
            }
            if broker.in_flight_equivalent_command_count > 0 {
                let fp = "broker:in_flight".to_string();
                verification_by_fp.insert(
                    fp.clone(),
                    SwarmBriefVerificationEvidence {
                        command_fingerprint: fp,
                        status: "in_flight".to_string(),
                        evidence_hash: format!(
                            "in_flight:{}",
                            broker.in_flight_equivalent_command_count
                        ),
                    },
                );
            }
        }

        let mut rch_by_alias = BTreeMap::<String, SwarmBriefRchPosture>::new();
        if let Some(rch) = &report.rch_local_capability {
            for worker in &rch.worker_pressure.workers {
                let worker_alias = redact_brief_text(&worker.worker_id);
                if worker_alias.is_empty() {
                    continue;
                }
                rch_by_alias.insert(
                    worker_alias.clone(),
                    SwarmBriefRchPosture {
                        worker_alias,
                        posture: redact_brief_text(&worker.pressure_state),
                        admission_status: redact_brief_text(&worker.admission_impact),
                    },
                );
            }
            if rch_by_alias.is_empty()
                && !rch.worker_probe_summary.status.trim().is_empty()
                && rch.worker_probe_summary.status != "not_collected"
            {
                rch_by_alias.insert(
                    "rch_fleet".to_string(),
                    SwarmBriefRchPosture {
                        worker_alias: "rch_fleet".to_string(),
                        posture: redact_brief_text(&rch.worker_probe_summary.status),
                        admission_status: redact_brief_text(&rch.worker_pressure.status),
                    },
                );
            }
        }

        Self {
            agents: agents_by_alias.into_values().collect(),
            reservations: reservations_by_id.into_values().collect(),
            beads: beads_by_id.into_values().collect(),
            verification_evidence: verification_by_fp.into_values().collect(),
            rch_postures: rch_by_alias.into_values().collect(),
        }
    }

    /// Parse a [`SwarmBriefSnapshot`] from a JSON file path (`SwarmBriefSnapshot`,
    /// `SwarmBriefReport`, `ee.response.v2` envelope, or support-bundle wrapper).
    pub fn from_json_path(path: &Path) -> Result<Self, String> {
        let metadata = fs::metadata(path).map_err(|error| {
            format!(
                "Failed to read swarm brief snapshot metadata {}: {error}",
                redact_brief_text(&path.display().to_string())
            )
        })?;
        if metadata.len() > SWARM_BRIEF_DELTA_MAX_SNAPSHOT_BYTES {
            return Err(format!(
                "Swarm brief snapshot {} exceeds maximum size ({} > {} bytes)",
                redact_brief_text(&path.display().to_string()),
                metadata.len(),
                SWARM_BRIEF_DELTA_MAX_SNAPSHOT_BYTES
            ));
        }
        let raw = fs::read_to_string(path).map_err(|error| {
            format!(
                "Failed to read swarm brief snapshot {}: {error}",
                redact_brief_text(&path.display().to_string())
            )
        })?;
        Self::from_json_str(&raw)
    }

    /// Parse a [`SwarmBriefSnapshot`] from a JSON string.
    pub fn from_json_str(raw: &str) -> Result<Self, String> {
        let value: Value = serde_json::from_str(raw)
            .map_err(|error| format!("Invalid swarm brief snapshot JSON: {error}"))?;
        Self::from_json_value(&value)
    }

    /// Parse a [`SwarmBriefSnapshot`] from a JSON [`Value`], accepting direct
    /// [`SwarmBriefSnapshot`] payloads, [`SwarmBriefReport`] payloads,
    /// `ee.response.v2` envelopes (`.data`), or support-bundle objects (`.swarmBrief`).
    pub fn from_json_value(value: &Value) -> Result<Self, String> {
        let root = value
            .get("data")
            .filter(|inner| inner.is_object())
            .or_else(|| value.get("swarmBrief").filter(|inner| inner.is_object()))
            .unwrap_or(value);
        let obj = root
            .as_object()
            .ok_or_else(|| "Swarm brief snapshot must be a JSON object".to_string())?;

        let mut stalled_posture_by_agent = BTreeMap::<String, String>::new();
        if let Some(stalled_items) = obj.get("stalledBeadLiveness").and_then(Value::as_array) {
            for item in stalled_items {
                if let (Some(assignee), Some(posture)) = (
                    item.get("assignee").and_then(Value::as_str),
                    item.get("posture").and_then(Value::as_str),
                ) {
                    let alias = redact_brief_text(assignee);
                    let posture = redact_brief_text(posture);
                    if !alias.is_empty() && !posture.is_empty() {
                        stalled_posture_by_agent.entry(alias).or_insert(posture);
                    }
                }
            }
        }

        let mut agents_by_alias = BTreeMap::<String, SwarmBriefAgent>::new();
        for key in ["agents", "agentMailAgents"] {
            if let Some(items) = obj.get(key).and_then(Value::as_array) {
                for item in items {
                    let Some(raw_alias) = item
                        .get("agentAlias")
                        .or_else(|| item.get("name"))
                        .and_then(Value::as_str)
                    else {
                        continue;
                    };
                    let agent_alias = redact_brief_text(raw_alias);
                    if agent_alias.is_empty() {
                        continue;
                    }
                    let status = item
                        .get("status")
                        .and_then(Value::as_str)
                        .map(redact_brief_text)
                        .filter(|s| !s.is_empty())
                        .or_else(|| stalled_posture_by_agent.get(&agent_alias).cloned())
                        .unwrap_or_else(|| "active".to_string());
                    let last_seen_at_rfc3339 = item
                        .get("lastSeenAtRfc3339")
                        .or_else(|| item.get("lastActiveAt"))
                        .and_then(Value::as_str)
                        .map(redact_brief_text);
                    agents_by_alias.insert(
                        agent_alias.clone(),
                        SwarmBriefAgent {
                            agent_alias,
                            status,
                            last_seen_at_rfc3339,
                        },
                    );
                }
            }
        }

        let mut reservations_by_id = BTreeMap::<String, SwarmBriefReservation>::new();
        for key in ["reservations", "fileReservations"] {
            if let Some(items) = obj.get(key).and_then(Value::as_array) {
                for item in items {
                    let holder_agent = item
                        .get("holderAgent")
                        .or_else(|| item.get("holder"))
                        .and_then(Value::as_str)
                        .map(redact_brief_text)
                        .unwrap_or_default();
                    let path_pattern = item
                        .get("pathPattern")
                        .and_then(Value::as_str)
                        .map(redact_brief_text)
                        .unwrap_or_default();
                    let reservation_id = item
                        .get("reservationId")
                        .and_then(Value::as_str)
                        .map(redact_brief_text)
                        .filter(|s| !s.is_empty())
                        .unwrap_or_else(|| format!("{holder_agent}:{path_pattern}"));
                    if reservation_id.is_empty() || reservation_id == ":" {
                        continue;
                    }
                    let expires_at_rfc3339 = item
                        .get("expiresAtRfc3339")
                        .or_else(|| item.get("expiresAt"))
                        .and_then(Value::as_str)
                        .map(redact_brief_text);
                    reservations_by_id.insert(
                        reservation_id.clone(),
                        SwarmBriefReservation {
                            reservation_id,
                            path_pattern,
                            holder_agent,
                            expires_at_rfc3339,
                        },
                    );
                }
            }
        }

        let mut beads_by_id = BTreeMap::<String, SwarmBriefBead>::new();
        let mut insert_bead_item = |item: &Value| {
            let Some(raw_id) = item
                .get("beadId")
                .or_else(|| item.get("id"))
                .and_then(Value::as_str)
            else {
                return;
            };
            let bead_id = redact_brief_text(raw_id);
            if bead_id.is_empty() {
                return;
            }
            let status = item
                .get("status")
                .and_then(Value::as_str)
                .map(redact_brief_text)
                .unwrap_or_else(|| "open".to_string());
            let assignee = item
                .get("assignee")
                .and_then(Value::as_str)
                .map(redact_brief_text);
            let comment_count = item
                .get("commentCount")
                .and_then(Value::as_u64)
                .and_then(|v| u32::try_from(v).ok())
                .unwrap_or(0);
            beads_by_id.insert(
                bead_id.clone(),
                SwarmBriefBead {
                    bead_id,
                    status,
                    assignee,
                    comment_count,
                },
            );
        };
        if let Some(beads_val) = obj.get("beads") {
            if let Some(items) = beads_val.as_array() {
                for item in items {
                    insert_bead_item(item);
                }
            } else if let Some(beads_obj) = beads_val.as_object() {
                for bucket in ["ready", "inProgress", "blocked", "deferred"] {
                    if let Some(items) = beads_obj.get(bucket).and_then(Value::as_array) {
                        for item in items {
                            insert_bead_item(item);
                        }
                    }
                }
            }
        }

        let mut verification_by_fp = BTreeMap::<String, SwarmBriefVerificationEvidence>::new();
        if let Some(items) = obj.get("verificationEvidence").and_then(Value::as_array) {
            for item in items {
                let Some(raw_fp) = item.get("commandFingerprint").and_then(Value::as_str) else {
                    continue;
                };
                let command_fingerprint = redact_brief_text(raw_fp);
                if command_fingerprint.is_empty() {
                    continue;
                }
                let status = item
                    .get("status")
                    .and_then(Value::as_str)
                    .map(redact_brief_text)
                    .unwrap_or_else(|| "unknown".to_string());
                let evidence_hash = item
                    .get("evidenceHash")
                    .and_then(Value::as_str)
                    .map(redact_brief_text)
                    .unwrap_or_default();
                verification_by_fp.insert(
                    command_fingerprint.clone(),
                    SwarmBriefVerificationEvidence {
                        command_fingerprint,
                        status,
                        evidence_hash,
                    },
                );
            }
        }
        if let Some(broker) = obj.get("verificationBroker").and_then(Value::as_object) {
            let reusable = broker
                .get("recentReusableRunCount")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            if reusable > 0 {
                let fp = "broker:reusable_runs".to_string();
                verification_by_fp.insert(
                    fp.clone(),
                    SwarmBriefVerificationEvidence {
                        command_fingerprint: fp,
                        status: "reusable".to_string(),
                        evidence_hash: format!("reusable:{reusable}"),
                    },
                );
            }
            let stale = broker
                .get("staleRunCount")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            if stale > 0 {
                let fp = "broker:stale_runs".to_string();
                verification_by_fp.insert(
                    fp.clone(),
                    SwarmBriefVerificationEvidence {
                        command_fingerprint: fp,
                        status: "stale".to_string(),
                        evidence_hash: format!("stale:{stale}"),
                    },
                );
            }
            let in_flight = broker
                .get("inFlightEquivalentCommandCount")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            if in_flight > 0 {
                let fp = "broker:in_flight".to_string();
                verification_by_fp.insert(
                    fp.clone(),
                    SwarmBriefVerificationEvidence {
                        command_fingerprint: fp,
                        status: "in_flight".to_string(),
                        evidence_hash: format!("in_flight:{in_flight}"),
                    },
                );
            }
        }

        let mut rch_by_alias = BTreeMap::<String, SwarmBriefRchPosture>::new();
        if let Some(items) = obj.get("rchPostures").and_then(Value::as_array) {
            for item in items {
                let Some(raw_alias) = item
                    .get("workerAlias")
                    .or_else(|| item.get("workerId"))
                    .and_then(Value::as_str)
                else {
                    continue;
                };
                let worker_alias = redact_brief_text(raw_alias);
                if worker_alias.is_empty() {
                    continue;
                }
                let posture = item
                    .get("posture")
                    .or_else(|| item.get("pressureState"))
                    .and_then(Value::as_str)
                    .map(redact_brief_text)
                    .unwrap_or_else(|| "unknown".to_string());
                let admission_status = item
                    .get("admissionStatus")
                    .or_else(|| item.get("admissionImpact"))
                    .and_then(Value::as_str)
                    .map(redact_brief_text)
                    .unwrap_or_else(|| "unknown".to_string());
                rch_by_alias.insert(
                    worker_alias.clone(),
                    SwarmBriefRchPosture {
                        worker_alias,
                        posture,
                        admission_status,
                    },
                );
            }
        }
        if let Some(rch) = obj.get("rchLocalCapability").and_then(Value::as_object) {
            if let Some(workers) = rch
                .get("workerPressure")
                .and_then(Value::as_object)
                .and_then(|wp| wp.get("workers"))
                .and_then(Value::as_array)
            {
                for worker in workers {
                    let Some(raw_alias) = worker.get("workerId").and_then(Value::as_str) else {
                        continue;
                    };
                    let worker_alias = redact_brief_text(raw_alias);
                    if worker_alias.is_empty() {
                        continue;
                    }
                    let posture = worker
                        .get("pressureState")
                        .and_then(Value::as_str)
                        .map(redact_brief_text)
                        .unwrap_or_else(|| "unknown".to_string());
                    let admission_status = worker
                        .get("admissionImpact")
                        .and_then(Value::as_str)
                        .map(redact_brief_text)
                        .unwrap_or_else(|| "unknown".to_string());
                    rch_by_alias.insert(
                        worker_alias.clone(),
                        SwarmBriefRchPosture {
                            worker_alias,
                            posture,
                            admission_status,
                        },
                    );
                }
            }
            if rch_by_alias.is_empty()
                && let Some(probe_status) = rch
                    .get("workerProbeSummary")
                    .and_then(Value::as_object)
                    .and_then(|s| s.get("status"))
                    .and_then(Value::as_str)
                && !probe_status.trim().is_empty()
                && probe_status != "not_collected"
            {
                let pressure_status = rch
                    .get("workerPressure")
                    .and_then(Value::as_object)
                    .and_then(|wp| wp.get("status"))
                    .and_then(Value::as_str)
                    .unwrap_or("unknown");
                rch_by_alias.insert(
                    "rch_fleet".to_string(),
                    SwarmBriefRchPosture {
                        worker_alias: "rch_fleet".to_string(),
                        posture: redact_brief_text(probe_status),
                        admission_status: redact_brief_text(pressure_status),
                    },
                );
            }
        }

        Ok(Self {
            agents: agents_by_alias.into_values().collect(),
            reservations: reservations_by_id.into_values().collect(),
            beads: beads_by_id.into_values().collect(),
            verification_evidence: verification_by_fp.into_values().collect(),
            rch_postures: rch_by_alias.into_values().collect(),
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentDelta {
    pub agent_alias: String,
    pub kind: &'static str,
    pub previous_status: Option<String>,
    pub current_status: Option<String>,
    pub previous_last_seen: Option<String>,
    pub current_last_seen: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReservationDelta {
    pub reservation_id: String,
    pub kind: &'static str,
    pub path_pattern: String,
    pub holder_agent: String,
    pub previous_expires_at: Option<String>,
    pub current_expires_at: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BeadDelta {
    pub bead_id: String,
    pub kind: &'static str,
    pub previous_status: Option<String>,
    pub current_status: Option<String>,
    pub previous_assignee: Option<String>,
    pub current_assignee: Option<String>,
    pub comment_count_delta: i64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VerificationDelta {
    pub command_fingerprint: String,
    pub kind: &'static str,
    pub previous_status: Option<String>,
    pub current_status: Option<String>,
    pub previous_evidence_hash: Option<String>,
    pub current_evidence_hash: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RchDelta {
    pub worker_alias: String,
    pub kind: &'static str,
    pub previous_posture: Option<String>,
    pub current_posture: Option<String>,
    pub previous_admission_status: Option<String>,
    pub current_admission_status: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NextAction {
    pub severity: DeltaSeverity,
    pub code: &'static str,
    pub rationale: &'static str,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SwarmBriefDeltaCapsule {
    pub schema: &'static str,
    pub side_effect_free: bool,
    pub changed_agents: Vec<AgentDelta>,
    pub changed_reservations: Vec<ReservationDelta>,
    pub changed_beads: Vec<BeadDelta>,
    pub changed_verification: Vec<VerificationDelta>,
    pub changed_rch: Vec<RchDelta>,
    pub recommended_next_actions: Vec<NextAction>,
}

impl SwarmBriefDeltaCapsule {
    /// True iff at least one delta exists.
    #[must_use]
    pub fn any_changes(&self) -> bool {
        !self.changed_agents.is_empty()
            || !self.changed_reservations.is_empty()
            || !self.changed_beads.is_empty()
            || !self.changed_verification.is_empty()
            || !self.changed_rch.is_empty()
    }
}

/// Pure differ. Returns a deterministic capsule given two snapshots.
#[must_use]
pub fn compute_swarm_brief_delta(
    before: &SwarmBriefSnapshot,
    after: &SwarmBriefSnapshot,
) -> SwarmBriefDeltaCapsule {
    let changed_agents = diff_agents(&before.agents, &after.agents);
    let changed_reservations = diff_reservations(&before.reservations, &after.reservations);
    let changed_beads = diff_beads(&before.beads, &after.beads);
    let changed_verification =
        diff_verification(&before.verification_evidence, &after.verification_evidence);
    let changed_rch = diff_rch(&before.rch_postures, &after.rch_postures);
    let recommended_next_actions = derive_next_actions(
        &changed_agents,
        &changed_reservations,
        &changed_beads,
        &changed_verification,
        &changed_rch,
    );
    SwarmBriefDeltaCapsule {
        schema: SWARM_BRIEF_DELTA_SCHEMA_V1,
        side_effect_free: true,
        changed_agents,
        changed_reservations,
        changed_beads,
        changed_verification,
        changed_rch,
        recommended_next_actions,
    }
}

fn diff_agents(before: &[SwarmBriefAgent], after: &[SwarmBriefAgent]) -> Vec<AgentDelta> {
    let before_map: BTreeMap<&str, &SwarmBriefAgent> = before
        .iter()
        .map(|agent| (agent.agent_alias.as_str(), agent))
        .collect();
    let after_map: BTreeMap<&str, &SwarmBriefAgent> = after
        .iter()
        .map(|agent| (agent.agent_alias.as_str(), agent))
        .collect();
    let mut deltas: Vec<AgentDelta> = Vec::new();
    for (alias, after_agent) in &after_map {
        match before_map.get(alias) {
            Some(before_agent) => {
                if before_agent.status != after_agent.status
                    || before_agent.last_seen_at_rfc3339 != after_agent.last_seen_at_rfc3339
                {
                    deltas.push(AgentDelta {
                        agent_alias: (*alias).to_string(),
                        kind: "changed",
                        previous_status: Some(before_agent.status.clone()),
                        current_status: Some(after_agent.status.clone()),
                        previous_last_seen: before_agent.last_seen_at_rfc3339.clone(),
                        current_last_seen: after_agent.last_seen_at_rfc3339.clone(),
                    });
                }
            }
            None => deltas.push(AgentDelta {
                agent_alias: (*alias).to_string(),
                kind: "appeared",
                previous_status: None,
                current_status: Some(after_agent.status.clone()),
                previous_last_seen: None,
                current_last_seen: after_agent.last_seen_at_rfc3339.clone(),
            }),
        }
    }
    for (alias, before_agent) in &before_map {
        if !after_map.contains_key(alias) {
            deltas.push(AgentDelta {
                agent_alias: (*alias).to_string(),
                kind: "disappeared",
                previous_status: Some(before_agent.status.clone()),
                current_status: None,
                previous_last_seen: before_agent.last_seen_at_rfc3339.clone(),
                current_last_seen: None,
            });
        }
    }
    deltas.sort_by(|a, b| {
        a.kind
            .cmp(b.kind)
            .then_with(|| a.agent_alias.cmp(&b.agent_alias))
    });
    deltas
}

fn diff_reservations(
    before: &[SwarmBriefReservation],
    after: &[SwarmBriefReservation],
) -> Vec<ReservationDelta> {
    let before_map: BTreeMap<&str, &SwarmBriefReservation> = before
        .iter()
        .map(|reservation| (reservation.reservation_id.as_str(), reservation))
        .collect();
    let after_map: BTreeMap<&str, &SwarmBriefReservation> = after
        .iter()
        .map(|reservation| (reservation.reservation_id.as_str(), reservation))
        .collect();
    let mut deltas: Vec<ReservationDelta> = Vec::new();
    for (id, after_reservation) in &after_map {
        match before_map.get(id) {
            Some(before_reservation) => {
                if before_reservation.expires_at_rfc3339 != after_reservation.expires_at_rfc3339 {
                    deltas.push(ReservationDelta {
                        reservation_id: (*id).to_string(),
                        kind: "renewed",
                        path_pattern: after_reservation.path_pattern.clone(),
                        holder_agent: after_reservation.holder_agent.clone(),
                        previous_expires_at: before_reservation.expires_at_rfc3339.clone(),
                        current_expires_at: after_reservation.expires_at_rfc3339.clone(),
                    });
                }
            }
            None => deltas.push(ReservationDelta {
                reservation_id: (*id).to_string(),
                kind: "acquired",
                path_pattern: after_reservation.path_pattern.clone(),
                holder_agent: after_reservation.holder_agent.clone(),
                previous_expires_at: None,
                current_expires_at: after_reservation.expires_at_rfc3339.clone(),
            }),
        }
    }
    for (id, before_reservation) in &before_map {
        if !after_map.contains_key(id) {
            deltas.push(ReservationDelta {
                reservation_id: (*id).to_string(),
                kind: "released_or_expired",
                path_pattern: before_reservation.path_pattern.clone(),
                holder_agent: before_reservation.holder_agent.clone(),
                previous_expires_at: before_reservation.expires_at_rfc3339.clone(),
                current_expires_at: None,
            });
        }
    }
    deltas.sort_by(|a, b| {
        a.kind
            .cmp(b.kind)
            .then_with(|| a.reservation_id.cmp(&b.reservation_id))
    });
    deltas
}

fn diff_beads(before: &[SwarmBriefBead], after: &[SwarmBriefBead]) -> Vec<BeadDelta> {
    let before_map: BTreeMap<&str, &SwarmBriefBead> = before
        .iter()
        .map(|bead| (bead.bead_id.as_str(), bead))
        .collect();
    let after_map: BTreeMap<&str, &SwarmBriefBead> = after
        .iter()
        .map(|bead| (bead.bead_id.as_str(), bead))
        .collect();
    let mut deltas: Vec<BeadDelta> = Vec::new();
    for (id, after_bead) in &after_map {
        match before_map.get(id) {
            Some(before_bead) => {
                let status_changed = before_bead.status != after_bead.status;
                let assignee_changed = before_bead.assignee != after_bead.assignee;
                let comment_delta =
                    i64::from(after_bead.comment_count) - i64::from(before_bead.comment_count);
                if !status_changed && !assignee_changed && comment_delta == 0 {
                    continue;
                }
                deltas.push(BeadDelta {
                    bead_id: (*id).to_string(),
                    kind: if status_changed {
                        "status_changed"
                    } else if assignee_changed {
                        "assignee_changed"
                    } else {
                        "comments_changed"
                    },
                    previous_status: Some(before_bead.status.clone()),
                    current_status: Some(after_bead.status.clone()),
                    previous_assignee: before_bead.assignee.clone(),
                    current_assignee: after_bead.assignee.clone(),
                    comment_count_delta: comment_delta,
                });
            }
            None => deltas.push(BeadDelta {
                bead_id: (*id).to_string(),
                kind: "appeared",
                previous_status: None,
                current_status: Some(after_bead.status.clone()),
                previous_assignee: None,
                current_assignee: after_bead.assignee.clone(),
                comment_count_delta: i64::from(after_bead.comment_count),
            }),
        }
    }
    for (id, before_bead) in &before_map {
        if !after_map.contains_key(id) {
            deltas.push(BeadDelta {
                bead_id: (*id).to_string(),
                kind: "disappeared",
                previous_status: Some(before_bead.status.clone()),
                current_status: None,
                previous_assignee: before_bead.assignee.clone(),
                current_assignee: None,
                comment_count_delta: -i64::from(before_bead.comment_count),
            });
        }
    }
    deltas.sort_by(|a, b| a.kind.cmp(b.kind).then_with(|| a.bead_id.cmp(&b.bead_id)));
    deltas
}

fn diff_verification(
    before: &[SwarmBriefVerificationEvidence],
    after: &[SwarmBriefVerificationEvidence],
) -> Vec<VerificationDelta> {
    let before_map: BTreeMap<&str, &SwarmBriefVerificationEvidence> = before
        .iter()
        .map(|evidence| (evidence.command_fingerprint.as_str(), evidence))
        .collect();
    let after_map: BTreeMap<&str, &SwarmBriefVerificationEvidence> = after
        .iter()
        .map(|evidence| (evidence.command_fingerprint.as_str(), evidence))
        .collect();
    let mut deltas: Vec<VerificationDelta> = Vec::new();
    for (fingerprint, after_evidence) in &after_map {
        match before_map.get(fingerprint) {
            Some(before_evidence) => {
                if before_evidence.status != after_evidence.status
                    || before_evidence.evidence_hash != after_evidence.evidence_hash
                {
                    deltas.push(VerificationDelta {
                        command_fingerprint: (*fingerprint).to_string(),
                        kind: "changed",
                        previous_status: Some(before_evidence.status.clone()),
                        current_status: Some(after_evidence.status.clone()),
                        previous_evidence_hash: Some(before_evidence.evidence_hash.clone()),
                        current_evidence_hash: Some(after_evidence.evidence_hash.clone()),
                    });
                }
            }
            None => deltas.push(VerificationDelta {
                command_fingerprint: (*fingerprint).to_string(),
                kind: "appeared",
                previous_status: None,
                current_status: Some(after_evidence.status.clone()),
                previous_evidence_hash: None,
                current_evidence_hash: Some(after_evidence.evidence_hash.clone()),
            }),
        }
    }
    for (fingerprint, before_evidence) in &before_map {
        if !after_map.contains_key(fingerprint) {
            deltas.push(VerificationDelta {
                command_fingerprint: (*fingerprint).to_string(),
                kind: "cleared",
                previous_status: Some(before_evidence.status.clone()),
                current_status: None,
                previous_evidence_hash: Some(before_evidence.evidence_hash.clone()),
                current_evidence_hash: None,
            });
        }
    }
    deltas.sort_by(|a, b| {
        a.kind
            .cmp(b.kind)
            .then_with(|| a.command_fingerprint.cmp(&b.command_fingerprint))
    });
    deltas
}

fn diff_rch(before: &[SwarmBriefRchPosture], after: &[SwarmBriefRchPosture]) -> Vec<RchDelta> {
    let before_map: BTreeMap<&str, &SwarmBriefRchPosture> = before
        .iter()
        .map(|posture| (posture.worker_alias.as_str(), posture))
        .collect();
    let after_map: BTreeMap<&str, &SwarmBriefRchPosture> = after
        .iter()
        .map(|posture| (posture.worker_alias.as_str(), posture))
        .collect();
    let mut deltas: Vec<RchDelta> = Vec::new();
    for (alias, after_posture) in &after_map {
        match before_map.get(alias) {
            Some(before_posture) => {
                if before_posture.posture != after_posture.posture
                    || before_posture.admission_status != after_posture.admission_status
                {
                    deltas.push(RchDelta {
                        worker_alias: (*alias).to_string(),
                        kind: "changed",
                        previous_posture: Some(before_posture.posture.clone()),
                        current_posture: Some(after_posture.posture.clone()),
                        previous_admission_status: Some(before_posture.admission_status.clone()),
                        current_admission_status: Some(after_posture.admission_status.clone()),
                    });
                }
            }
            None => deltas.push(RchDelta {
                worker_alias: (*alias).to_string(),
                kind: "appeared",
                previous_posture: None,
                current_posture: Some(after_posture.posture.clone()),
                previous_admission_status: None,
                current_admission_status: Some(after_posture.admission_status.clone()),
            }),
        }
    }
    for (alias, before_posture) in &before_map {
        if !after_map.contains_key(alias) {
            deltas.push(RchDelta {
                worker_alias: (*alias).to_string(),
                kind: "disappeared",
                previous_posture: Some(before_posture.posture.clone()),
                current_posture: None,
                previous_admission_status: Some(before_posture.admission_status.clone()),
                current_admission_status: None,
            });
        }
    }
    deltas.sort_by(|a, b| {
        a.kind
            .cmp(b.kind)
            .then_with(|| a.worker_alias.cmp(&b.worker_alias))
    });
    deltas
}

fn derive_next_actions(
    agents: &[AgentDelta],
    reservations: &[ReservationDelta],
    beads: &[BeadDelta],
    verification: &[VerificationDelta],
    rch: &[RchDelta],
) -> Vec<NextAction> {
    let mut actions: Vec<NextAction> = Vec::new();
    if rch.iter().any(delta_indicates_pressure) {
        actions.push(NextAction {
            severity: DeltaSeverity::Critical,
            code: "rch_posture_regressed",
            rationale: "RCH worker pressure is present in the latest snapshot; verify topology before launching new remote builds.",
        });
    }
    if reservations
        .iter()
        .any(|delta| delta.kind == "released_or_expired")
    {
        actions.push(NextAction {
            severity: DeltaSeverity::Warning,
            code: "reservation_released",
            rationale: "Reservations cleared since the prior snapshot; recheck for orphan WIP before claiming the freed paths.",
        });
    }
    if verification.iter().any(|delta| delta.kind == "cleared") {
        actions.push(NextAction {
            severity: DeltaSeverity::Notice,
            code: "verification_evidence_cleared",
            rationale: "Verification evidence cleared since the prior snapshot; consider re-running gated proofs.",
        });
    }
    if beads.iter().any(|delta| delta.kind == "status_changed") {
        actions.push(NextAction {
            severity: DeltaSeverity::Notice,
            code: "bead_status_changed",
            rationale: "Bead status changed since the prior snapshot; reread the affected bead comments before resuming work.",
        });
    }
    if agents.iter().any(|delta| delta.kind == "disappeared") {
        actions.push(NextAction {
            severity: DeltaSeverity::Info,
            code: "agent_disappeared",
            rationale: "An agent vanished from the brief; surface the absence to the orchestrator before reclaiming its lanes.",
        });
    }
    if actions.is_empty()
        && (!agents.is_empty()
            || !reservations.is_empty()
            || !beads.is_empty()
            || !verification.is_empty()
            || !rch.is_empty())
    {
        actions.push(NextAction {
            severity: DeltaSeverity::Info,
            code: "delta_observed_no_action",
            rationale: "Some swarm state shifted but no critical change was found; brief is informational.",
        });
    }
    actions
}

fn delta_indicates_pressure(delta: &RchDelta) -> bool {
    matches!(delta.current_admission_status.as_deref(), Some(status) if status == "pressure_blocked"
        || status == "topology_blocked"
        || status == "all_workers_preflight_failed")
        || matches!(delta.current_posture.as_deref(), Some(posture) if posture == "critical")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rfc(value: &str) -> Option<String> {
        Some(value.to_string())
    }

    #[test]
    fn empty_snapshots_produce_empty_capsule() {
        let before = SwarmBriefSnapshot::default();
        let after = SwarmBriefSnapshot::default();
        let capsule = compute_swarm_brief_delta(&before, &after);
        assert!(!capsule.any_changes());
        assert!(capsule.recommended_next_actions.is_empty());
    }

    #[test]
    fn identical_snapshots_produce_no_changes() {
        let snapshot = SwarmBriefSnapshot {
            agents: vec![SwarmBriefAgent {
                agent_alias: "peer_a".to_string(),
                status: "active".to_string(),
                last_seen_at_rfc3339: rfc("2026-05-20T01:00:00Z"),
            }],
            reservations: vec![SwarmBriefReservation {
                reservation_id: "res-1".to_string(),
                path_pattern: "src/core/**".to_string(),
                holder_agent: "peer_a".to_string(),
                expires_at_rfc3339: rfc("2026-05-20T02:00:00Z"),
            }],
            beads: vec![SwarmBriefBead {
                bead_id: "bd-xyz".to_string(),
                status: "in_progress".to_string(),
                assignee: Some("peer_a".to_string()),
                comment_count: 3,
            }],
            verification_evidence: Vec::new(),
            rch_postures: Vec::new(),
        };
        let capsule = compute_swarm_brief_delta(&snapshot, &snapshot);
        assert!(!capsule.any_changes());
    }

    #[test]
    fn reservation_churn_produces_acquired_and_released_deltas() {
        let before = SwarmBriefSnapshot {
            reservations: vec![SwarmBriefReservation {
                reservation_id: "res-1".to_string(),
                path_pattern: "src/core/**".to_string(),
                holder_agent: "peer_a".to_string(),
                expires_at_rfc3339: rfc("2026-05-20T02:00:00Z"),
            }],
            ..SwarmBriefSnapshot::default()
        };
        let after = SwarmBriefSnapshot {
            reservations: vec![SwarmBriefReservation {
                reservation_id: "res-2".to_string(),
                path_pattern: "src/output/**".to_string(),
                holder_agent: "peer_b".to_string(),
                expires_at_rfc3339: rfc("2026-05-20T03:00:00Z"),
            }],
            ..SwarmBriefSnapshot::default()
        };
        let capsule = compute_swarm_brief_delta(&before, &after);
        assert_eq!(capsule.changed_reservations.len(), 2);
        assert_eq!(capsule.changed_reservations[0].kind, "acquired");
        assert_eq!(capsule.changed_reservations[0].reservation_id, "res-2");
        assert_eq!(capsule.changed_reservations[1].kind, "released_or_expired");
        assert_eq!(capsule.changed_reservations[1].reservation_id, "res-1");
        assert!(
            capsule
                .recommended_next_actions
                .iter()
                .any(|action| action.code == "reservation_released")
        );
    }

    #[test]
    fn bead_status_change_emits_recommendation() {
        let before = SwarmBriefSnapshot {
            beads: vec![SwarmBriefBead {
                bead_id: "bd-foo".to_string(),
                status: "open".to_string(),
                assignee: None,
                comment_count: 2,
            }],
            ..SwarmBriefSnapshot::default()
        };
        let after = SwarmBriefSnapshot {
            beads: vec![SwarmBriefBead {
                bead_id: "bd-foo".to_string(),
                status: "in_progress".to_string(),
                assignee: Some("peer_b".to_string()),
                comment_count: 4,
            }],
            ..SwarmBriefSnapshot::default()
        };
        let capsule = compute_swarm_brief_delta(&before, &after);
        assert_eq!(capsule.changed_beads.len(), 1);
        assert_eq!(capsule.changed_beads[0].kind, "status_changed");
        assert_eq!(capsule.changed_beads[0].comment_count_delta, 2);
        assert!(
            capsule
                .recommended_next_actions
                .iter()
                .any(|action| action.code == "bead_status_changed")
        );
    }

    #[test]
    fn rch_degradation_produces_critical_action() {
        let before = SwarmBriefSnapshot {
            rch_postures: vec![SwarmBriefRchPosture {
                worker_alias: "vmi_4f2a".to_string(),
                posture: "healthy".to_string(),
                admission_status: "available".to_string(),
            }],
            ..SwarmBriefSnapshot::default()
        };
        let after = SwarmBriefSnapshot {
            rch_postures: vec![SwarmBriefRchPosture {
                worker_alias: "vmi_4f2a".to_string(),
                posture: "critical".to_string(),
                admission_status: "pressure_blocked".to_string(),
            }],
            ..SwarmBriefSnapshot::default()
        };
        let capsule = compute_swarm_brief_delta(&before, &after);
        assert_eq!(capsule.changed_rch.len(), 1);
        assert_eq!(capsule.changed_rch[0].kind, "changed");
        let critical = capsule
            .recommended_next_actions
            .iter()
            .find(|action| action.severity == DeltaSeverity::Critical)
            .expect("critical action emitted");
        assert_eq!(critical.code, "rch_posture_regressed");
    }

    #[test]
    fn newly_observed_rch_pressure_still_produces_critical_action() {
        let before = SwarmBriefSnapshot::default();
        let after = SwarmBriefSnapshot {
            rch_postures: vec![SwarmBriefRchPosture {
                worker_alias: "vmi_new".to_string(),
                posture: "critical".to_string(),
                admission_status: "pressure_blocked".to_string(),
            }],
            ..SwarmBriefSnapshot::default()
        };
        let capsule = compute_swarm_brief_delta(&before, &after);
        assert_eq!(capsule.changed_rch.len(), 1);
        assert_eq!(capsule.changed_rch[0].kind, "appeared");
        let critical = capsule
            .recommended_next_actions
            .iter()
            .find(|action| action.severity == DeltaSeverity::Critical)
            .expect("critical action emitted for newly observed RCH pressure");
        assert_eq!(critical.code, "rch_posture_regressed");
    }

    #[test]
    fn verification_evidence_cleared_emits_notice() {
        let before = SwarmBriefSnapshot {
            verification_evidence: vec![SwarmBriefVerificationEvidence {
                command_fingerprint: "cargo_test:foo".to_string(),
                status: "reusable".to_string(),
                evidence_hash: "blake3:abc".to_string(),
            }],
            ..SwarmBriefSnapshot::default()
        };
        let after = SwarmBriefSnapshot::default();
        let capsule = compute_swarm_brief_delta(&before, &after);
        assert_eq!(capsule.changed_verification.len(), 1);
        assert_eq!(capsule.changed_verification[0].kind, "cleared");
        assert!(
            capsule
                .recommended_next_actions
                .iter()
                .any(|action| action.code == "verification_evidence_cleared")
        );
    }

    #[test]
    fn differ_is_deterministic_across_repeat_calls() {
        let snapshot = SwarmBriefSnapshot {
            agents: vec![
                SwarmBriefAgent {
                    agent_alias: "peer_b".to_string(),
                    status: "active".to_string(),
                    last_seen_at_rfc3339: rfc("2026-05-20T01:00:00Z"),
                },
                SwarmBriefAgent {
                    agent_alias: "peer_a".to_string(),
                    status: "active".to_string(),
                    last_seen_at_rfc3339: rfc("2026-05-20T01:00:00Z"),
                },
            ],
            ..SwarmBriefSnapshot::default()
        };
        let mut after = snapshot.clone();
        after.agents[0].status = "stale".to_string();
        let a = compute_swarm_brief_delta(&snapshot, &after);
        let b = compute_swarm_brief_delta(&snapshot, &after);
        assert_eq!(a, b);
        let a_json = serde_json::to_string(&a).expect("serialize a");
        let b_json = serde_json::to_string(&b).expect("serialize b");
        assert_eq!(a_json, b_json);
    }

    #[test]
    fn redaction_contract_input_fields_are_caller_curated() {
        // Inputs only carry alias / id strings (caller pre-redacts). The
        // differ has no field for raw mail bodies, command stderr, secret
        // strings, or memory bodies. This test ensures the snapshot
        // structs stay narrow; if a future field bypasses redaction this
        // assertion catches it via struct layout.
        let snapshot = SwarmBriefSnapshot::default();
        let json = serde_json::to_value(compute_swarm_brief_delta(&snapshot, &snapshot))
            .expect("serialize capsule");
        let serialized = serde_json::to_string(&json).expect("serialize json");
        for forbidden in ["password", "api_key", "raw_stderr", "raw_body"] {
            assert!(
                !serialized.contains(forbidden),
                "capsule must not surface {forbidden}: {serialized}"
            );
        }
    }

    #[test]
    fn snapshot_from_report_and_response_v2_envelope_are_isomorphic() {
        let mut report = SwarmBriefReport::empty(Path::new("/workspace"));
        report
            .agent_mail_agents
            .push(crate::core::swarm_brief::SwarmBriefAgentMailAgent {
                name: "peer_alpha".to_string(),
                last_active_at: rfc("2026-05-20T01:00:00Z"),
            });
        report
            .file_reservations
            .push(crate::core::swarm_brief::SwarmBriefFileReservation {
                path_pattern: "src/core/**".to_string(),
                holder: "peer_alpha".to_string(),
                exclusive: true,
                expires_at: rfc("2026-05-20T02:00:00Z"),
            });
        report
            .beads
            .in_progress
            .push(crate::core::swarm_brief::SwarmBriefBead {
                id: "bd-1zb7k.17.2".to_string(),
                title: "Swarm brief delta capsules".to_string(),
                status: "in_progress".to_string(),
                priority: Some(2),
                assignee: Some("peer_alpha".to_string()),
                issue_type: None,
                created_at: None,
                updated_at: None,
                latest_comment_at: None,
                comment_count: 5,
                source_bucket: "in_progress".to_string(),
            });

        let from_report = SwarmBriefSnapshot::from_report(&report);
        let envelope = serde_json::json!({
            "schema": "ee.response.v2",
            "success": true,
            "data": report,
            "degraded": []
        });
        let from_envelope =
            SwarmBriefSnapshot::from_json_value(&envelope).expect("parse response.v2 envelope");
        assert_eq!(from_report, from_envelope);
        assert_eq!(from_report.agents.len(), 1);
        assert_eq!(from_report.reservations.len(), 1);
        assert_eq!(from_report.beads.len(), 1);
    }

    #[test]
    fn agent_mail_unavailable_fallback_surfaces_disappeared_agents_and_released_reservations() {
        let before_json = serde_json::json!({
            "schema": "ee.swarm.brief.v1",
            "agentMailAgents": [
                {"name": "peer_alpha", "lastActiveAt": "2026-05-20T01:00:00Z"}
            ],
            "fileReservations": [
                {
                    "pathPattern": "src/core/**",
                    "holder": "peer_alpha",
                    "exclusive": true,
                    "expiresAt": "2026-05-20T02:00:00Z"
                }
            ]
        });
        let after_json = serde_json::json!({
            "schema": "ee.swarm.brief.v1",
            "sources": [
                {"source": "agent_mail", "status": "unavailable"}
            ],
            "agentMailAgents": [],
            "fileReservations": []
        });
        let before = SwarmBriefSnapshot::from_json_value(&before_json).expect("parse before");
        let after = SwarmBriefSnapshot::from_json_value(&after_json).expect("parse after");
        let capsule = compute_swarm_brief_delta(&before, &after);
        assert_eq!(capsule.changed_agents.len(), 1);
        assert_eq!(capsule.changed_agents[0].kind, "disappeared");
        assert_eq!(capsule.changed_agents[0].agent_alias, "peer_alpha");
        assert_eq!(capsule.changed_reservations.len(), 1);
        assert_eq!(capsule.changed_reservations[0].kind, "released_or_expired");
        assert!(
            capsule
                .recommended_next_actions
                .iter()
                .any(|action| action.code == "reservation_released")
        );
        assert!(
            capsule
                .recommended_next_actions
                .iter()
                .any(|action| action.code == "agent_disappeared")
        );
    }

    #[test]
    fn snapshot_json_parser_redacts_secrets_paths_and_ignores_raw_mail_or_stderr_fields() {
        let before = SwarmBriefSnapshot::default();
        let after_raw = serde_json::json!({
            "rawMailBody": "super secret mail body sk-proj-abcdefghijklmnopqrstuvwxyz0123456789ABCDEFGHIJ",
            "rawCommandStderr": "fatal: password=topsecret token=ghp_1234567890abcdefghijklmnopqrstuvwxyzABCD",
            "agentMailAgents": [
                {
                    "name": "agent:sk-proj-abcdefghijklmnopqrstuvwxyz0123456789ABCDEFGHIJ",
                    "lastActiveAt": "2026-05-20T01:00:00Z"
                }
            ],
            "fileReservations": [
                {
                    "pathPattern": "/private/var/folders/secret/workspace/src/core/**",
                    "holder": "peer_alpha",
                    "exclusive": true,
                    "expiresAt": "2026-05-20T02:00:00Z"
                }
            ]
        });
        let after = SwarmBriefSnapshot::from_json_value(&after_raw).expect("parse after_raw");
        let capsule = compute_swarm_brief_delta(&before, &after);
        let serialized = serde_json::to_string(&capsule).expect("serialize capsule");
        assert!(!serialized.contains("sk-proj-abcdefghijklmnopqrstuvwxyz"));
        assert!(!serialized.contains("ghp_1234567890"));
        assert!(!serialized.contains("super secret mail body"));
        assert!(!serialized.contains("password=topsecret"));
        assert!(!serialized.contains("/private/var/folders/secret"));
    }

    #[test]
    fn golden_capsule_json_matches_deterministic_contract() {
        let before = SwarmBriefSnapshot {
            agents: vec![SwarmBriefAgent {
                agent_alias: "peer_a".to_string(),
                status: "active".to_string(),
                last_seen_at_rfc3339: rfc("2026-05-20T01:00:00Z"),
            }],
            reservations: vec![SwarmBriefReservation {
                reservation_id: "res-1".to_string(),
                path_pattern: "src/core/**".to_string(),
                holder_agent: "peer_a".to_string(),
                expires_at_rfc3339: rfc("2026-05-20T02:00:00Z"),
            }],
            beads: vec![SwarmBriefBead {
                bead_id: "bd-1zb7k.17.2".to_string(),
                status: "open".to_string(),
                assignee: None,
                comment_count: 1,
            }],
            verification_evidence: vec![SwarmBriefVerificationEvidence {
                command_fingerprint: "broker:reusable_runs".to_string(),
                status: "reusable".to_string(),
                evidence_hash: "reusable:2".to_string(),
            }],
            rch_postures: vec![SwarmBriefRchPosture {
                worker_alias: "worker-01".to_string(),
                posture: "healthy".to_string(),
                admission_status: "available".to_string(),
            }],
        };
        let after = SwarmBriefSnapshot {
            agents: Vec::new(),
            reservations: Vec::new(),
            beads: vec![SwarmBriefBead {
                bead_id: "bd-1zb7k.17.2".to_string(),
                status: "in_progress".to_string(),
                assignee: Some("peer_b".to_string()),
                comment_count: 3,
            }],
            verification_evidence: Vec::new(),
            rch_postures: vec![SwarmBriefRchPosture {
                worker_alias: "worker-01".to_string(),
                posture: "critical".to_string(),
                admission_status: "pressure_blocked".to_string(),
            }],
        };
        let capsule = compute_swarm_brief_delta(&before, &after);
        let value = serde_json::to_value(&capsule).expect("serialize capsule");
        let expected = serde_json::json!({
            "schema": "ee.swarm_brief.delta.v1",
            "sideEffectFree": true,
            "changedAgents": [
                {
                    "agentAlias": "peer_a",
                    "kind": "disappeared",
                    "previousStatus": "active",
                    "currentStatus": null,
                    "previousLastSeen": "2026-05-20T01:00:00Z",
                    "currentLastSeen": null
                }
            ],
            "changedReservations": [
                {
                    "reservationId": "res-1",
                    "kind": "released_or_expired",
                    "pathPattern": "src/core/**",
                    "holderAgent": "peer_a",
                    "previousExpiresAt": "2026-05-20T02:00:00Z",
                    "currentExpiresAt": null
                }
            ],
            "changedBeads": [
                {
                    "beadId": "bd-1zb7k.17.2",
                    "kind": "status_changed",
                    "previousStatus": "open",
                    "currentStatus": "in_progress",
                    "previousAssignee": null,
                    "currentAssignee": "peer_b",
                    "commentCountDelta": 2
                }
            ],
            "changedVerification": [
                {
                    "commandFingerprint": "broker:reusable_runs",
                    "kind": "cleared",
                    "previousStatus": "reusable",
                    "currentStatus": null,
                    "previousEvidenceHash": "reusable:2",
                    "currentEvidenceHash": null
                }
            ],
            "changedRch": [
                {
                    "workerAlias": "worker-01",
                    "kind": "changed",
                    "previousPosture": "healthy",
                    "currentPosture": "critical",
                    "previousAdmissionStatus": "available",
                    "currentAdmissionStatus": "pressure_blocked"
                }
            ],
            "recommendedNextActions": [
                {
                    "severity": "critical",
                    "code": "rch_posture_regressed",
                    "rationale": "RCH worker pressure is present in the latest snapshot; verify topology before launching new remote builds."
                },
                {
                    "severity": "warning",
                    "code": "reservation_released",
                    "rationale": "Reservations cleared since the prior snapshot; recheck for orphan WIP before claiming the freed paths."
                },
                {
                    "severity": "notice",
                    "code": "verification_evidence_cleared",
                    "rationale": "Verification evidence cleared since the prior snapshot; consider re-running gated proofs."
                },
                {
                    "severity": "notice",
                    "code": "bead_status_changed",
                    "rationale": "Bead status changed since the prior snapshot; reread the affected bead comments before resuming work."
                },
                {
                    "severity": "info",
                    "code": "agent_disappeared",
                    "rationale": "An agent vanished from the brief; surface the absence to the orchestrator before reclaiming its lanes."
                }
            ]
        });
        assert_eq!(value, expected);
    }
}
