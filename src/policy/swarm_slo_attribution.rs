//! Redaction-safe Swarm SLO attribution adapters (bd-2dgn0.4).
//!
//! These adapters sit at the policy boundary: callers pass already-observed
//! resource usage or coordination posture, and the adapter emits a stable,
//! privacy-preserving event that scorecard/replay code can aggregate later.
//! The output intentionally carries hashes, counts, posture enums, normalized
//! producer ids, and safe evidence codes only. It never preserves raw mail
//! bodies, memory bodies, command output, local paths, or secret-like values.

use serde::Serialize;

use crate::policy::{
    NormalizedProducerId, ProducerIdKind, redact_secret_like_content,
    workspace_secret_risk_evidence,
};

pub const SWARM_SLO_RESOURCE_USAGE_EVENT_SCHEMA_V1: &str = "ee.swarm_slo.resource_usage_event.v1";
pub const SWARM_SLO_COORDINATION_EVENT_SCHEMA_V1: &str = "ee.swarm_slo.coordination_event.v1";

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SwarmSloAttributionBucket {
    Storage,
    Search,
    Graph,
    Pack,
    Output,
    Coordination,
    Rch,
    Tracker,
    ExternalUnavailable,
    UnknownResidual,
}

impl SwarmSloAttributionBucket {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Storage => "storage",
            Self::Search => "search",
            Self::Graph => "graph",
            Self::Pack => "pack",
            Self::Output => "output",
            Self::Coordination => "coordination",
            Self::Rch => "rch",
            Self::Tracker => "tracker",
            Self::ExternalUnavailable => "external_unavailable",
            Self::UnknownResidual => "unknown_residual",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SwarmSloPosture {
    Ok,
    Degraded,
    Unavailable,
    Blocked,
    Unknown,
}

impl SwarmSloPosture {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Degraded => "degraded",
            Self::Unavailable => "unavailable",
            Self::Blocked => "blocked",
            Self::Unknown => "unknown",
        }
    }

    #[must_use]
    pub const fn is_unavailable_or_blocked(self) -> bool {
        matches!(self, Self::Unavailable | Self::Blocked)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SwarmSloProducerAttribution {
    pub kind: &'static str,
    pub attribution_key: String,
    pub canonical_hash: String,
    pub original_hash: String,
    pub redacted: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SwarmSloRedactedEvidence {
    pub field: String,
    pub code: Option<String>,
    pub value_hash: String,
    pub redacted: bool,
    pub redaction_reasons: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SwarmSloResourceUsageEvent {
    pub schema: &'static str,
    pub producer: SwarmSloProducerAttribution,
    pub source: String,
    pub stage: String,
    pub bucket: SwarmSloAttributionBucket,
    pub posture: SwarmSloPosture,
    pub elapsed_ms: u64,
    pub cpu_ms: Option<u64>,
    pub memory_bytes: Option<u64>,
    pub io_read_bytes: Option<u64>,
    pub io_write_bytes: Option<u64>,
    pub evidence: Vec<SwarmSloRedactedEvidence>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SwarmSloResourceUsageInput<'a> {
    pub producer_id: &'a str,
    pub source: &'a str,
    pub stage: &'a str,
    pub posture: SwarmSloPosture,
    pub elapsed_ms: u64,
    pub cpu_ms: Option<u64>,
    pub memory_bytes: Option<u64>,
    pub io_read_bytes: Option<u64>,
    pub io_write_bytes: Option<u64>,
    pub evidence: &'a [(&'a str, &'a str)],
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SwarmSloCoordinationEvent {
    pub schema: &'static str,
    pub producer: SwarmSloProducerAttribution,
    pub source_kind: String,
    pub bucket: SwarmSloAttributionBucket,
    pub posture: SwarmSloPosture,
    pub elapsed_ms: u64,
    pub event_count: u64,
    pub error_count: u64,
    pub degraded_count: u64,
    pub repair_command: Option<String>,
    pub evidence: Vec<SwarmSloRedactedEvidence>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SwarmSloCoordinationInput<'a> {
    pub producer_id: &'a str,
    pub source_kind: &'a str,
    pub posture: SwarmSloPosture,
    pub elapsed_ms: u64,
    pub event_count: u64,
    pub error_count: u64,
    pub degraded_count: u64,
    pub evidence: &'a [(&'a str, &'a str)],
}

#[must_use]
pub fn adapt_swarm_slo_resource_usage_event(
    input: &SwarmSloResourceUsageInput<'_>,
) -> SwarmSloResourceUsageEvent {
    let evidence = redact_evidence(input.evidence);
    let bucket = classify_attribution_bucket(input.source, input.stage, input.posture, &evidence);
    SwarmSloResourceUsageEvent {
        schema: SWARM_SLO_RESOURCE_USAGE_EVENT_SCHEMA_V1,
        producer: producer_attribution(input.producer_id),
        source: safe_label(input.source),
        stage: safe_label(input.stage),
        bucket,
        posture: input.posture,
        elapsed_ms: input.elapsed_ms,
        cpu_ms: input.cpu_ms,
        memory_bytes: input.memory_bytes,
        io_read_bytes: input.io_read_bytes,
        io_write_bytes: input.io_write_bytes,
        evidence,
    }
}

#[must_use]
pub fn adapt_swarm_slo_coordination_event(
    input: &SwarmSloCoordinationInput<'_>,
) -> SwarmSloCoordinationEvent {
    let evidence = redact_evidence(input.evidence);
    let bucket =
        classify_attribution_bucket(input.source_kind, "coordination", input.posture, &evidence);
    let source_kind = safe_label(input.source_kind);
    let repair_command = repair_command_for(&source_kind, input.posture, &evidence);
    SwarmSloCoordinationEvent {
        schema: SWARM_SLO_COORDINATION_EVENT_SCHEMA_V1,
        producer: producer_attribution(input.producer_id),
        source_kind,
        bucket,
        posture: input.posture,
        elapsed_ms: input.elapsed_ms,
        event_count: input.event_count,
        error_count: input.error_count,
        degraded_count: input.degraded_count,
        repair_command,
        evidence,
    }
}

fn producer_attribution(raw: &str) -> SwarmSloProducerAttribution {
    let normalized = crate::policy::normalize_producer_id(raw);
    let canonical_hash = stable_hash(&normalized.canonical);
    let original_hash = stable_hash(&normalized.original);
    let redacted = producer_key_needs_redaction(&normalized);
    let attribution_key = if redacted {
        format!("{}:{canonical_hash}", normalized.kind.as_str())
    } else {
        normalized.attribution_key()
    };
    SwarmSloProducerAttribution {
        kind: normalized.kind.as_str(),
        attribution_key,
        canonical_hash,
        original_hash,
        redacted,
    }
}

fn producer_key_needs_redaction(producer: &NormalizedProducerId) -> bool {
    if matches!(
        producer.kind,
        ProducerIdKind::Human | ProducerIdKind::Unknown
    ) {
        return true;
    }
    let content_redaction = redact_secret_like_content(&producer.canonical);
    let path_report =
        workspace_secret_risk_evidence("producer_id", Some(producer.canonical.as_bytes()), 4096);
    content_redaction.redacted
        || path_report.secret_risk
        || !is_public_producer_key(&producer.canonical)
}

fn redact_evidence(evidence: &[(&str, &str)]) -> Vec<SwarmSloRedactedEvidence> {
    let mut redacted = evidence
        .iter()
        .map(|(field, value)| redact_evidence_value(field, value))
        .collect::<Vec<_>>();
    redacted.sort_by(|left, right| {
        left.field
            .cmp(&right.field)
            .then_with(|| left.code.cmp(&right.code))
            .then_with(|| left.value_hash.cmp(&right.value_hash))
    });
    redacted.dedup();
    redacted
}

fn redact_evidence_value(field: &str, value: &str) -> SwarmSloRedactedEvidence {
    let field = safe_label(field);
    let redaction = redact_secret_like_content(value);
    let path_report = workspace_secret_risk_evidence(&field, Some(value.as_bytes()), 4096);
    let mut reasons = redaction
        .redacted_reasons
        .into_iter()
        .map(str::to_owned)
        .chain(path_report.risk_classes.into_iter().map(str::to_owned))
        .chain(path_report.reasons.into_iter().map(str::to_owned))
        .collect::<Vec<_>>();
    let public_label = is_public_label(value);
    if !public_label {
        reasons.push("unsafe_public_label".to_owned());
    }
    reasons.sort();
    reasons.dedup();
    let redacted = redaction.redacted || path_report.secret_risk || !public_label;
    SwarmSloRedactedEvidence {
        field,
        code: (!redacted).then(|| safe_label(value)),
        value_hash: stable_hash(value),
        redacted,
        redaction_reasons: reasons,
    }
}

fn classify_attribution_bucket(
    source: &str,
    stage: &str,
    posture: SwarmSloPosture,
    evidence: &[SwarmSloRedactedEvidence],
) -> SwarmSloAttributionBucket {
    let haystack = attribution_haystack(source, stage, evidence);
    if haystack.contains("rch") || haystack.contains("e327") {
        return SwarmSloAttributionBucket::Rch;
    }
    if haystack.contains("agent_mail")
        || haystack.contains("mail")
        || haystack.contains("reservation")
        || haystack.contains("coordination")
    {
        return SwarmSloAttributionBucket::Coordination;
    }
    if haystack.contains("beads")
        || haystack.contains("br")
        || haystack.contains("bv")
        || haystack.contains("tracker")
    {
        return SwarmSloAttributionBucket::Tracker;
    }
    if haystack.contains("sqlite") || haystack.contains("db") || haystack.contains("storage") {
        return SwarmSloAttributionBucket::Storage;
    }
    if haystack.contains("search") || haystack.contains("index") {
        return SwarmSloAttributionBucket::Search;
    }
    if haystack.contains("graph") {
        return SwarmSloAttributionBucket::Graph;
    }
    if haystack.contains("pack") || haystack.contains("context") {
        return SwarmSloAttributionBucket::Pack;
    }
    if haystack.contains("output") || haystack.contains("render") {
        return SwarmSloAttributionBucket::Output;
    }
    if posture.is_unavailable_or_blocked() {
        return SwarmSloAttributionBucket::ExternalUnavailable;
    }
    SwarmSloAttributionBucket::UnknownResidual
}

fn attribution_haystack(
    source: &str,
    stage: &str,
    evidence: &[SwarmSloRedactedEvidence],
) -> String {
    let mut parts = vec![source.to_ascii_lowercase(), stage.to_ascii_lowercase()];
    for item in evidence {
        parts.push(item.field.to_ascii_lowercase());
        if let Some(code) = &item.code {
            parts.push(code.to_ascii_lowercase());
        }
        parts.extend(
            item.redaction_reasons
                .iter()
                .map(|reason| reason.to_ascii_lowercase()),
        );
    }
    parts.join(" ")
}

fn repair_command_for(
    source_kind: &str,
    posture: SwarmSloPosture,
    evidence: &[SwarmSloRedactedEvidence],
) -> Option<String> {
    let haystack = attribution_haystack(source_kind, "coordination", evidence);
    if haystack.contains("rch") || haystack.contains("e327") {
        return Some(r#"rch diagnose --dry-run "cargo test --workspace""#.to_owned());
    }
    if haystack.contains("agent_mail") || haystack.contains("mail") || haystack.contains("sqlite") {
        return Some("am doctor check --verbose".to_owned());
    }
    if haystack.contains("bv") {
        return Some("bv --robot-triage".to_owned());
    }
    if haystack.contains("beads") || haystack.contains("br") || haystack.contains("tracker") {
        return Some("br doctor --json".to_owned());
    }
    if posture.is_unavailable_or_blocked() {
        return Some("ee status --json".to_owned());
    }
    None
}

fn safe_label(value: &str) -> String {
    let mut out = value
        .trim()
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.') {
                ch.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect::<String>();
    while out.contains("__") {
        out = out.replace("__", "_");
    }
    out.trim_matches('_').to_owned()
}

fn is_public_label(value: &str) -> bool {
    let trimmed = value.trim();
    !trimmed.is_empty()
        && trimmed.len() <= 96
        && !trimmed.contains('/')
        && !trimmed.contains('\\')
        && !trimmed.contains('@')
        && !trimmed.contains('=')
        && !trimmed.contains(':')
        && trimmed
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.'))
}

fn is_public_producer_key(value: &str) -> bool {
    let trimmed = value.trim();
    !trimmed.is_empty()
        && trimmed.len() <= 128
        && !trimmed.contains('/')
        && !trimmed.contains('\\')
        && !trimmed.contains('@')
        && !trimmed.contains('=')
        && trimmed
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.' | ':'))
}

fn stable_hash(value: &str) -> String {
    let digest = blake3::hash(value.as_bytes());
    let hex = digest.to_hex();
    format!("blake3:{}", &hex[..16])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resource_usage_event_normalizes_producer_and_redacts_secret_evidence() {
        let event = adapt_swarm_slo_resource_usage_event(&SwarmSloResourceUsageInput {
            producer_id: "%4",
            source: "context_pack",
            stage: "pack",
            posture: SwarmSloPosture::Degraded,
            elapsed_ms: 412,
            cpu_ms: Some(51),
            memory_bytes: Some(2_048),
            io_read_bytes: Some(128),
            io_write_bytes: None,
            evidence: &[(
                "stderr",
                "api_key=test-redaction-value-abcdefghijklmnopqrstuvwxyz /tmp/private/id_ed25519",
            )],
        });

        assert_eq!(event.schema, SWARM_SLO_RESOURCE_USAGE_EVENT_SCHEMA_V1);
        assert_eq!(event.producer.kind, "agent_pane");
        assert_eq!(event.producer.attribution_key, "agent_pane:pane_4");
        assert_eq!(event.bucket, SwarmSloAttributionBucket::Pack);
        assert_eq!(event.evidence.len(), 1);
        assert!(event.evidence[0].redacted);
        assert!(event.evidence[0].code.is_none());
        let json = serde_json::to_string(&event).expect("resource event serializes");
        assert!(!json.contains("test-redaction-value"));
        assert!(!json.contains("id_ed25519"));
        assert!(!json.contains("/tmp/private"));
        assert!(json.contains("unsafe_public_label"));
    }

    #[test]
    fn coordination_event_distinguishes_agent_mail_bv_and_rch_buckets() {
        let agent_mail = adapt_swarm_slo_coordination_event(&SwarmSloCoordinationInput {
            producer_id: "PinkOriole",
            source_kind: "agent_mail",
            posture: SwarmSloPosture::Unavailable,
            elapsed_ms: 0,
            event_count: 0,
            error_count: 1,
            degraded_count: 1,
            evidence: &[("code", "sqlite_malformed")],
        });
        let bv = adapt_swarm_slo_coordination_event(&SwarmSloCoordinationInput {
            producer_id: "cod_1",
            source_kind: "bv",
            posture: SwarmSloPosture::Unavailable,
            elapsed_ms: 1200,
            event_count: 1,
            error_count: 1,
            degraded_count: 1,
            evidence: &[("code", "bv_timeout_no_output")],
        });
        let rch = adapt_swarm_slo_coordination_event(&SwarmSloCoordinationInput {
            producer_id: "codex-cli",
            source_kind: "rch",
            posture: SwarmSloPosture::Blocked,
            elapsed_ms: 2200,
            event_count: 1,
            error_count: 1,
            degraded_count: 1,
            evidence: &[("code", "rch_e327_path_topology")],
        });

        assert_eq!(agent_mail.bucket, SwarmSloAttributionBucket::Coordination);
        assert_eq!(
            agent_mail.repair_command.as_deref(),
            Some("am doctor check --verbose")
        );
        assert_eq!(bv.bucket, SwarmSloAttributionBucket::Tracker);
        assert_eq!(bv.repair_command.as_deref(), Some("bv --robot-triage"));
        assert_eq!(rch.bucket, SwarmSloAttributionBucket::Rch);
        assert_eq!(
            rch.repair_command.as_deref(),
            Some(r#"rch diagnose --dry-run "cargo test --workspace""#)
        );
    }

    #[test]
    fn human_producer_email_is_hashed_not_leaked() {
        let event = adapt_swarm_slo_coordination_event(&SwarmSloCoordinationInput {
            producer_id: "person@example.test",
            source_kind: "beads",
            posture: SwarmSloPosture::Ok,
            elapsed_ms: 20,
            event_count: 3,
            error_count: 0,
            degraded_count: 0,
            evidence: &[("code", "ready_json")],
        });

        assert_eq!(event.producer.kind, "human");
        assert!(event.producer.redacted);
        assert!(event.producer.attribution_key.starts_with("human:blake3:"));
        let json = serde_json::to_string(&event).expect("coordination event serializes");
        assert!(!json.contains("person@example.test"));
    }

    #[test]
    fn evidence_order_is_stable_and_deduplicated() {
        let event = adapt_swarm_slo_coordination_event(&SwarmSloCoordinationInput {
            producer_id: "cc_1",
            source_kind: "workspace",
            posture: SwarmSloPosture::Degraded,
            elapsed_ms: 99,
            event_count: 4,
            error_count: 0,
            degraded_count: 2,
            evidence: &[
                ("posture", "peer_dirty_paths"),
                ("code", "reservations_unavailable"),
                ("posture", "peer_dirty_paths"),
            ],
        });

        let keys = event
            .evidence
            .iter()
            .map(|item| (item.field.as_str(), item.code.as_deref()))
            .collect::<Vec<_>>();
        assert_eq!(
            keys,
            vec![
                ("code", Some("reservations_unavailable")),
                ("posture", Some("peer_dirty_paths")),
            ]
        );
    }

    #[test]
    fn unsafe_source_and_stage_labels_are_sanitized() {
        let event = adapt_swarm_slo_resource_usage_event(&SwarmSloResourceUsageInput {
            producer_id: "reflection:gaps:claude-opus-4-7",
            source: "context/search",
            stage: "output:json",
            posture: SwarmSloPosture::Ok,
            elapsed_ms: 5,
            cpu_ms: None,
            memory_bytes: None,
            io_read_bytes: None,
            io_write_bytes: None,
            evidence: &[("code", "render_ok")],
        });

        assert_eq!(event.producer.kind, "reflection_context");
        assert!(!event.producer.redacted);
        assert_eq!(
            event.producer.attribution_key,
            "reflection_context:reflection:gaps:claude-opus-4-7"
        );
        assert_eq!(event.source, "context_search");
        assert_eq!(event.stage, "output_json");
        assert_eq!(event.bucket, SwarmSloAttributionBucket::Search);
    }

    #[test]
    fn redaction_placeholder_remains_scanner_specific() {
        assert_eq!(
            crate::policy::redaction_placeholder("swarm_slo_attribution"),
            "[REDACTED:swarm_slo_attribution]"
        );
    }
}
