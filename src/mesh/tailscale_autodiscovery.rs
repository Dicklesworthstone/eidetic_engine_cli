//! SRR6.46.2 — deterministic Tailscale peer autodiscovery.
//!
//! The orchestration layer is deliberately transport-independent: callers
//! provide a local Tailscale report, SRR6.46.7 discovery-policy state, and a
//! tiny hello-probe adapter. Unit tests can prove policy, budget, and
//! workspace matching without a real tailnet; the foreground CLI can use the
//! same report contract when fake-Tailscale fixtures advertise hello metadata.

use std::collections::BTreeSet;

use serde::Serialize;

use crate::core::tailscale_probe::{
    TailscaleLocalReport, TailscalePeerEeCapability, TailscalePeerReport,
};
use crate::mesh::discovery_policy::{
    DiscoveryDecision, DiscoveryDecisionInput, DiscoveryMode, decide_discovery,
};
use crate::mesh::hello::{
    HELLO_RESPONSE_SCHEMA_V1, HelloErrorCode, HelloResponse,
    classify_decline_for_caller_skip_reason,
};

pub const TAILSCALE_AUTODISCOVERY_SCHEMA_V1: &str = "ee.tailscale.autodiscovery.v1";

pub const TAILSCALE_PEER_PROBE_TIMEOUT_CODE: &str = "tailscale_peer_probe_timeout";
pub const NO_EE_PEERS_ON_TAILNET_CODE: &str = "no_ee_peers_on_tailnet";
pub const TAILSCALE_PEER_LIST_UNAVAILABLE_CODE: &str = "tailscale_peer_list_unavailable";
pub const PEER_DISCOVERY_WORKSPACE_MISMATCH_CODE: &str = "peer_discovery_workspace_mismatch";
pub const PEER_DISCOVERY_BUDGET_EXHAUSTED_CODE: &str = "peer_discovery_budget_exhausted";

pub const DEFAULT_TAILSCALE_PEER_PROBE_TIMEOUT_MS: u64 = 750;
pub const DEFAULT_TAILSCALE_DISCOVERY_BUDGET_MS: u64 = 5_000;
pub const DETERMINISTIC_LAST_PROBED_AT: &str = "1970-01-01T00:00:00Z";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TailscaleAutodiscoveryConfig<'a> {
    pub mesh_enabled: bool,
    pub workspace_id: &'a str,
    pub discovery_mode: DiscoveryMode,
    pub allowlist: &'a BTreeSet<String>,
    pub denylist: &'a BTreeSet<String>,
    pub peer_probe_timeout_ms: u64,
    pub total_budget_ms: u64,
    pub last_probed_at: &'a str,
}

impl<'a> TailscaleAutodiscoveryConfig<'a> {
    #[must_use]
    pub fn new(
        mesh_enabled: bool,
        workspace_id: &'a str,
        discovery_mode: DiscoveryMode,
        allowlist: &'a BTreeSet<String>,
        denylist: &'a BTreeSet<String>,
    ) -> Self {
        Self {
            mesh_enabled,
            workspace_id,
            discovery_mode,
            allowlist,
            denylist,
            peer_probe_timeout_ms: DEFAULT_TAILSCALE_PEER_PROBE_TIMEOUT_MS,
            total_budget_ms: DEFAULT_TAILSCALE_DISCOVERY_BUDGET_MS,
            last_probed_at: DETERMINISTIC_LAST_PROBED_AT,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TailscaleAutodiscoveryReport {
    pub schema: &'static str,
    pub tailnet_id: Option<String>,
    pub tailnet_display_name: Option<String>,
    pub self_node_key: Option<String>,
    pub probed_peer_count: u32,
    pub eligible_peer_count: u32,
    pub ee_capable_peers: Vec<TailscaleAutodiscoveryPeer>,
    pub skipped_peers: Vec<TailscaleAutodiscoverySkippedPeer>,
    pub degraded: Vec<TailscaleAutodiscoveryDegradation>,
}

impl TailscaleAutodiscoveryReport {
    #[must_use]
    fn empty() -> Self {
        Self {
            schema: TAILSCALE_AUTODISCOVERY_SCHEMA_V1,
            tailnet_id: None,
            tailnet_display_name: None,
            self_node_key: None,
            probed_peer_count: 0,
            eligible_peer_count: 0,
            ee_capable_peers: Vec::new(),
            skipped_peers: Vec::new(),
            degraded: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TailscaleAutodiscoveryPeer {
    pub node_key: String,
    pub tailscale_ip: String,
    pub magic_dns_name: Option<String>,
    pub hostname: Option<String>,
    pub ee_protocol_version: String,
    pub workspace_match_set: Vec<String>,
    pub last_probed_at: String,
    pub latency_ms: u64,
    pub discovery_policy_decision: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TailscaleAutodiscoverySkippedPeer {
    pub node_key: String,
    pub reason: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TailscaleAutodiscoveryDegradation {
    pub code: &'static str,
    pub severity: &'static str,
    pub message: String,
    pub repair: &'static str,
}

impl TailscaleAutodiscoveryDegradation {
    #[must_use]
    pub fn new(
        code: &'static str,
        severity: &'static str,
        message: impl Into<String>,
        repair: &'static str,
    ) -> Self {
        Self {
            code,
            severity,
            message: message.into(),
            repair,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TailscalePeerHelloProbe {
    Granted {
        response: HelloResponse,
        latency_ms: u64,
    },
    Declined {
        code: HelloErrorCode,
        latency_ms: u64,
    },
    Timeout {
        elapsed_ms: u64,
    },
    Malformed {
        elapsed_ms: u64,
    },
    NonEe {
        elapsed_ms: u64,
    },
}

impl TailscalePeerHelloProbe {
    #[must_use]
    fn elapsed_ms(&self) -> u64 {
        match self {
            Self::Granted { latency_ms, .. } | Self::Declined { latency_ms, .. } => *latency_ms,
            Self::Timeout { elapsed_ms }
            | Self::Malformed { elapsed_ms }
            | Self::NonEe { elapsed_ms } => *elapsed_ms,
        }
    }
}

pub trait TailscaleHelloProbe {
    fn probe(
        &mut self,
        peer: &TailscalePeerReport,
        timeout_ms: u64,
        remaining_budget_ms: u64,
    ) -> TailscalePeerHelloProbe;

    fn cancellation_requested(&self) -> bool {
        false
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct TailscaleStatusCapabilityHelloProbe;

impl TailscaleHelloProbe for TailscaleStatusCapabilityHelloProbe {
    fn probe(
        &mut self,
        peer: &TailscalePeerReport,
        timeout_ms: u64,
        remaining_budget_ms: u64,
    ) -> TailscalePeerHelloProbe {
        if peer.online == Some(false) {
            return TailscalePeerHelloProbe::NonEe { elapsed_ms: 0 };
        }
        let Some(capability) = peer.ee_capability.as_ref() else {
            return TailscalePeerHelloProbe::NonEe { elapsed_ms: 0 };
        };
        let effective_timeout_ms = timeout_ms.min(remaining_budget_ms);
        if capability.latency_ms > effective_timeout_ms {
            return TailscalePeerHelloProbe::Timeout {
                elapsed_ms: effective_timeout_ms,
            };
        }
        if !capability.respond || !capability.looks_like_ee() {
            return TailscalePeerHelloProbe::NonEe {
                elapsed_ms: capability.latency_ms,
            };
        }
        TailscalePeerHelloProbe::Granted {
            response: HelloResponse {
                schema: HELLO_RESPONSE_SCHEMA_V1,
                request_id: format!("autodiscovery:{}", peer.node_key),
                responder_node_key: peer.node_key.clone(),
                responder_ee_version: capability.ee_version.clone(),
                responder_ee_protocol_version: capability.ee_protocol_version.clone(),
                responder_workspace_ids: capability.workspace_ids.clone(),
                responder_capabilities: Vec::new(),
                responder_advertised_tags: peer.advertised_tags.clone(),
                discovery_consent: true,
                response_elapsed_micros: capability.latency_ms.saturating_mul(1_000),
            },
            latency_ms: capability.latency_ms,
        }
    }
}

#[must_use]
pub fn tailscale_peer_probe_timeout_ms_from_env_value(value: Option<&str>) -> u64 {
    positive_u64_or_default(value, DEFAULT_TAILSCALE_PEER_PROBE_TIMEOUT_MS)
}

#[must_use]
pub fn tailscale_discovery_budget_ms_from_env_value(value: Option<&str>) -> u64 {
    positive_u64_or_default(value, DEFAULT_TAILSCALE_DISCOVERY_BUDGET_MS)
}

fn positive_u64_or_default(value: Option<&str>, default_value: u64) -> u64 {
    value
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default_value)
}

#[must_use]
pub fn autodiscover_tailscale_peers<P: TailscaleHelloProbe>(
    local: Option<&TailscaleLocalReport>,
    config: &TailscaleAutodiscoveryConfig<'_>,
    probe: &mut P,
) -> TailscaleAutodiscoveryReport {
    let mut report = local.map_or_else(TailscaleAutodiscoveryReport::empty, |local| {
        TailscaleAutodiscoveryReport {
            schema: TAILSCALE_AUTODISCOVERY_SCHEMA_V1,
            tailnet_id: local.tailnet_id.clone(),
            tailnet_display_name: local.tailnet_display_name.clone(),
            self_node_key: local.self_node_key.clone(),
            probed_peer_count: 0,
            eligible_peer_count: 0,
            ee_capable_peers: Vec::new(),
            skipped_peers: Vec::new(),
            degraded: Vec::new(),
        }
    });

    let Some(local) = local else {
        push_degradation_once(
            &mut report,
            TailscaleAutodiscoveryDegradation::new(
                TAILSCALE_PEER_LIST_UNAVAILABLE_CODE,
                "warning",
                "Tailscale peer list was unavailable because the local probe did not run.",
                "Enable mesh probing with EE_MESH_ENABLED=1 and re-run `ee mesh status --json`.",
            ),
        );
        return report;
    };

    if !config.mesh_enabled {
        push_degradation_once(
            &mut report,
            TailscaleAutodiscoveryDegradation::new(
                TAILSCALE_PEER_LIST_UNAVAILABLE_CODE,
                "warning",
                "Tailscale peer autodiscovery was skipped because mesh is disabled.",
                "Set EE_MESH_ENABLED=1 or configure [mesh].enabled when this workspace should join a mesh.",
            ),
        );
        return report;
    }

    if !local.daemon_reachable || !local.authenticated {
        push_degradation_once(
            &mut report,
            TailscaleAutodiscoveryDegradation::new(
                TAILSCALE_PEER_LIST_UNAVAILABLE_CODE,
                "warning",
                "Tailscale peer list was unavailable because the local daemon is not reachable or not authenticated.",
                "Run `tailscale status` and authenticate with `tailscale up` before mesh discovery.",
            ),
        );
        return report;
    }

    if local.peers.is_empty() {
        push_no_ee_peers(&mut report);
        return report;
    }

    let self_node_key = local.self_node_key.as_deref().unwrap_or_default();
    let mut peers = local.peers.clone();
    peers.sort_by(|left, right| left.node_key.cmp(&right.node_key));

    let mut elapsed_budget_ms = 0_u64;
    let mut budget_exhausted = false;
    let mut saw_policy_probe_candidate = false;

    for peer in &peers {
        let (decision, reason) = decide_discovery(&DiscoveryDecisionInput {
            mode: config.discovery_mode,
            peer_node_key: &peer.node_key,
            peer_advertised_tags: &peer.advertised_tags,
            self_node_key,
            allowlist: config.allowlist,
            denylist: config.denylist,
        });
        let Some(policy_decision) = reason.autodiscovery_policy_decision() else {
            let skip_reason = reason
                .autodiscovery_skip_reason()
                .unwrap_or("denied_by_policy");
            report.skipped_peers.push(skipped(peer, skip_reason));
            continue;
        };
        if decision == DiscoveryDecision::Skip {
            report.skipped_peers.push(skipped(peer, "denied_by_policy"));
            continue;
        }

        saw_policy_probe_candidate = true;
        if peer.online == Some(false) {
            report.skipped_peers.push(skipped(peer, "non_ee"));
            continue;
        }
        if elapsed_budget_ms >= config.total_budget_ms || probe.cancellation_requested() {
            budget_exhausted = true;
            report.skipped_peers.push(skipped(peer, "probe_timeout"));
            continue;
        }

        let remaining_budget_ms = config.total_budget_ms - elapsed_budget_ms;
        let probe_timeout_ms = config.peer_probe_timeout_ms.min(remaining_budget_ms);
        report.probed_peer_count = report.probed_peer_count.saturating_add(1);
        let outcome = probe.probe(peer, probe_timeout_ms, remaining_budget_ms);
        elapsed_budget_ms = elapsed_budget_ms.saturating_add(outcome.elapsed_ms());

        match outcome {
            TailscalePeerHelloProbe::Granted {
                response,
                latency_ms,
            } => {
                if response.schema != HELLO_RESPONSE_SCHEMA_V1
                    || response.responder_node_key != peer.node_key
                    || response.responder_ee_protocol_version.trim().is_empty()
                {
                    report.skipped_peers.push(skipped(peer, "probe_malformed"));
                    continue;
                }
                let workspace_match_set =
                    workspace_intersection(&response.responder_workspace_ids, config.workspace_id);
                if workspace_match_set.is_empty() {
                    report
                        .skipped_peers
                        .push(skipped(peer, "workspace_mismatch"));
                    push_workspace_mismatch(&mut report);
                    continue;
                }
                let Some(tailscale_ip) = peer.tailscale_ips.first().cloned() else {
                    report.skipped_peers.push(skipped(peer, "probe_malformed"));
                    continue;
                };
                report.ee_capable_peers.push(TailscaleAutodiscoveryPeer {
                    node_key: peer.node_key.clone(),
                    tailscale_ip,
                    magic_dns_name: peer.magic_dns_name.clone(),
                    hostname: peer.hostname.clone(),
                    ee_protocol_version: response.responder_ee_protocol_version,
                    workspace_match_set,
                    last_probed_at: config.last_probed_at.to_owned(),
                    latency_ms,
                    discovery_policy_decision: policy_decision.to_owned(),
                });
            }
            TailscalePeerHelloProbe::Declined { code, .. } => {
                report
                    .skipped_peers
                    .push(skipped(peer, classify_decline_for_caller_skip_reason(code)));
            }
            TailscalePeerHelloProbe::Timeout { .. } => {
                report.skipped_peers.push(skipped(peer, "probe_timeout"));
                push_probe_timeout(&mut report);
            }
            TailscalePeerHelloProbe::Malformed { .. } => {
                report.skipped_peers.push(skipped(peer, "probe_malformed"));
            }
            TailscalePeerHelloProbe::NonEe { .. } => {
                report.skipped_peers.push(skipped(peer, "non_ee"));
            }
        }

        if elapsed_budget_ms >= config.total_budget_ms
            && Some(peer.node_key.as_str()) != peers.last().map(|p| p.node_key.as_str())
        {
            budget_exhausted = true;
        }
    }

    if budget_exhausted {
        push_budget_exhausted(&mut report, config.total_budget_ms);
    }
    if report.ee_capable_peers.is_empty()
        && !budget_exhausted
        && (local.peers.is_empty() || saw_policy_probe_candidate)
    {
        push_no_ee_peers(&mut report);
    }

    report
        .ee_capable_peers
        .sort_by(|left, right| left.node_key.cmp(&right.node_key));
    report
        .skipped_peers
        .sort_by(|left, right| left.node_key.cmp(&right.node_key));
    report.eligible_peer_count = report.ee_capable_peers.len() as u32;
    report
}

fn skipped(
    peer: &TailscalePeerReport,
    reason: impl Into<String>,
) -> TailscaleAutodiscoverySkippedPeer {
    TailscaleAutodiscoverySkippedPeer {
        node_key: peer.node_key.clone(),
        reason: reason.into(),
    }
}

fn workspace_intersection(peer_workspace_ids: &[String], workspace_id: &str) -> Vec<String> {
    let workspace_ids: BTreeSet<_> = peer_workspace_ids.iter().map(String::as_str).collect();
    workspace_ids
        .contains(workspace_id)
        .then(|| workspace_id.to_owned())
        .into_iter()
        .collect()
}

fn push_probe_timeout(report: &mut TailscaleAutodiscoveryReport) {
    push_degradation_once(
        report,
        TailscaleAutodiscoveryDegradation::new(
            TAILSCALE_PEER_PROBE_TIMEOUT_CODE,
            "warning",
            "At least one Tailscale peer did not answer the ee hello probe within budget.",
            "Increase EE_TAILSCALE_PEER_PROBE_TIMEOUT_MS or retry discovery later.",
        ),
    );
}

fn push_no_ee_peers(report: &mut TailscaleAutodiscoveryReport) {
    push_degradation_once(
        report,
        TailscaleAutodiscoveryDegradation::new(
            NO_EE_PEERS_ON_TAILNET_CODE,
            "info",
            "Tailscale is healthy, but no eligible ee peers were discovered on this tailnet.",
            "Run ee with mesh enabled on another tailnet machine.",
        ),
    );
}

fn push_workspace_mismatch(report: &mut TailscaleAutodiscoveryReport) {
    push_degradation_once(
        report,
        TailscaleAutodiscoveryDegradation::new(
            PEER_DISCOVERY_WORKSPACE_MISMATCH_CODE,
            "info",
            "At least one ee peer advertised only different workspace IDs and was not enrolled.",
            "Use explicit `ee mesh peer add` or auto-enroll only when cross-workspace sharing is intended.",
        ),
    );
}

fn push_budget_exhausted(report: &mut TailscaleAutodiscoveryReport, total_budget_ms: u64) {
    push_degradation_once(
        report,
        TailscaleAutodiscoveryDegradation::new(
            PEER_DISCOVERY_BUDGET_EXHAUSTED_CODE,
            "warning",
            format!(
                "Tailscale peer autodiscovery exhausted the {total_budget_ms}ms total discovery budget."
            ),
            "Raise EE_TAILSCALE_DISCOVERY_BUDGET_MS or split discovery across multiple passes.",
        ),
    );
}

fn push_degradation_once(
    report: &mut TailscaleAutodiscoveryReport,
    degradation: TailscaleAutodiscoveryDegradation,
) {
    if !report
        .degraded
        .iter()
        .any(|existing| existing.code == degradation.code)
    {
        report.degraded.push(degradation);
    }
}

impl TailscalePeerEeCapability {
    #[must_use]
    pub(crate) fn looks_like_ee(&self) -> bool {
        !self.ee_version.trim().is_empty()
            && self.ee_version != "0.0.0"
            && !self.ee_protocol_version.trim().is_empty()
            && self.ee_protocol_version != "0.0"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::tailscale_probe::{TAILSCALE_LOCAL_SCHEMA_V1, TailscaleProbeMethod};
    use crate::mesh::discovery_policy::EE_MESH_SERVICE_TAG;
    use std::collections::{BTreeMap, BTreeSet};

    fn local_report(peers: Vec<TailscalePeerReport>) -> TailscaleLocalReport {
        TailscaleLocalReport {
            schema: TAILSCALE_LOCAL_SCHEMA_V1,
            installed: true,
            daemon_reachable: true,
            authenticated: true,
            binary_authentic: true,
            binary_version_raw: None,
            binary_absolute_path: None,
            shields_up: Some(false),
            tailnet_id: Some("tailnet-alpha".to_owned()),
            tailnet_display_name: Some("alpha.example".to_owned()),
            self_node_key: Some("nodekey:self".to_owned()),
            self_tailscale_ip: Some("100.64.0.1".to_owned()),
            self_magic_dns_name: Some("self.tailnet.test.".to_owned()),
            self_advertised_tags: vec![EE_MESH_SERVICE_TAG.to_owned()],
            peers,
            version: Some("1.66.0".to_owned()),
            probe_method: TailscaleProbeMethod::Cli,
            probe_elapsed_ms: 12,
            platform: crate::core::tailscale_probe::TailscalePlatform::Linux,
            degradations: Vec::new(),
        }
    }

    fn peer(node_key: &str, tags: &[&str]) -> TailscalePeerReport {
        TailscalePeerReport {
            node_key: node_key.to_owned(),
            tailscale_ips: vec!["100.64.0.2".to_owned()],
            magic_dns_name: Some(format!("{node_key}.tailnet.test.")),
            hostname: Some(node_key.replace("nodekey:", "host-")),
            advertised_tags: tags.iter().map(|tag| (*tag).to_owned()).collect(),
            online: Some(true),
            ee_capability: None,
        }
    }

    fn response(node_key: &str, workspaces: &[&str]) -> HelloResponse {
        HelloResponse {
            schema: HELLO_RESPONSE_SCHEMA_V1,
            request_id: format!("req-{node_key}"),
            responder_node_key: node_key.to_owned(),
            responder_ee_version: "0.2.0".to_owned(),
            responder_ee_protocol_version: "1.0".to_owned(),
            responder_workspace_ids: workspaces.iter().map(|id| (*id).to_owned()).collect(),
            responder_capabilities: Vec::new(),
            responder_advertised_tags: vec![EE_MESH_SERVICE_TAG.to_owned()],
            discovery_consent: true,
            response_elapsed_micros: 1_000,
        }
    }

    fn config<'a>(
        workspace_id: &'a str,
        mode: DiscoveryMode,
        allowlist: &'a BTreeSet<String>,
        denylist: &'a BTreeSet<String>,
    ) -> TailscaleAutodiscoveryConfig<'a> {
        TailscaleAutodiscoveryConfig::new(true, workspace_id, mode, allowlist, denylist)
    }

    #[derive(Default)]
    struct FakeProbe {
        outcomes: BTreeMap<String, TailscalePeerHelloProbe>,
        calls: Vec<String>,
        cancel_after_calls: Option<usize>,
    }

    impl FakeProbe {
        fn with(mut self, node_key: &str, outcome: TailscalePeerHelloProbe) -> Self {
            self.outcomes.insert(node_key.to_owned(), outcome);
            self
        }
    }

    #[test]
    fn status_capability_probe_timeout_consumes_effective_timeout_not_advertised_latency() {
        let mut peer = peer("nodekey:alpha", &[EE_MESH_SERVICE_TAG]);
        peer.ee_capability = Some(TailscalePeerEeCapability {
            ee_version: "0.2.0".to_owned(),
            ee_protocol_version: "1.0".to_owned(),
            workspace_ids: vec!["workspace-alpha".to_owned()],
            respond: true,
            latency_ms: 1_000,
        });

        let mut probe = TailscaleStatusCapabilityHelloProbe;
        let outcome = probe.probe(&peer, 750, 5_000);

        assert_eq!(
            outcome,
            TailscalePeerHelloProbe::Timeout { elapsed_ms: 750 }
        );
    }

    impl TailscaleHelloProbe for FakeProbe {
        fn probe(
            &mut self,
            peer: &TailscalePeerReport,
            _timeout_ms: u64,
            _remaining_budget_ms: u64,
        ) -> TailscalePeerHelloProbe {
            self.calls.push(peer.node_key.clone());
            self.outcomes
                .remove(&peer.node_key)
                .unwrap_or(TailscalePeerHelloProbe::NonEe { elapsed_ms: 0 })
        }

        fn cancellation_requested(&self) -> bool {
            self.cancel_after_calls
                .is_some_and(|limit| self.calls.len() >= limit)
        }
    }

    #[test]
    fn autodiscovery_returns_empty_list_when_local_probe_reports_no_peers() {
        let allowlist = BTreeSet::new();
        let denylist = BTreeSet::new();
        let mut probe = FakeProbe::default();
        let report = autodiscover_tailscale_peers(
            Some(&local_report(Vec::new())),
            &config(
                "workspace-alpha",
                DiscoveryMode::ServiceTag,
                &allowlist,
                &denylist,
            ),
            &mut probe,
        );

        assert!(report.ee_capable_peers.is_empty());
        assert!(report.skipped_peers.is_empty());
        assert_eq!(report.degraded[0].code, NO_EE_PEERS_ON_TAILNET_CODE);
    }

    #[test]
    fn autodiscovery_includes_peer_when_handshake_returns_matching_workspace_id() {
        let allowlist = BTreeSet::new();
        let denylist = BTreeSet::new();
        let peer_a = peer("nodekey:alpha", &[EE_MESH_SERVICE_TAG]);
        let mut probe = FakeProbe::default().with(
            "nodekey:alpha",
            TailscalePeerHelloProbe::Granted {
                response: response("nodekey:alpha", &["workspace-alpha"]),
                latency_ms: 17,
            },
        );

        let report = autodiscover_tailscale_peers(
            Some(&local_report(vec![peer_a])),
            &config(
                "workspace-alpha",
                DiscoveryMode::ServiceTag,
                &allowlist,
                &denylist,
            ),
            &mut probe,
        );

        assert_eq!(report.probed_peer_count, 1);
        assert_eq!(report.eligible_peer_count, 1);
        assert_eq!(report.ee_capable_peers[0].node_key, "nodekey:alpha");
        assert_eq!(
            report.ee_capable_peers[0].discovery_policy_decision,
            "service_tag_match"
        );
        assert_eq!(
            report.ee_capable_peers[0].workspace_match_set,
            vec!["workspace-alpha".to_owned()]
        );
    }

    #[test]
    fn autodiscovery_excludes_peer_when_handshake_response_is_malformed() {
        let allowlist = BTreeSet::new();
        let denylist = BTreeSet::new();
        let mut probe = FakeProbe::default().with(
            "nodekey:alpha",
            TailscalePeerHelloProbe::Malformed { elapsed_ms: 2 },
        );

        let report = autodiscover_tailscale_peers(
            Some(&local_report(vec![peer(
                "nodekey:alpha",
                &[EE_MESH_SERVICE_TAG],
            )])),
            &config(
                "workspace-alpha",
                DiscoveryMode::ServiceTag,
                &allowlist,
                &denylist,
            ),
            &mut probe,
        );

        assert!(report.ee_capable_peers.is_empty());
        assert_eq!(report.skipped_peers[0].reason, "probe_malformed");
    }

    #[test]
    fn autodiscovery_excludes_peer_when_workspace_ids_do_not_intersect() {
        let allowlist = BTreeSet::new();
        let denylist = BTreeSet::new();
        let mut probe = FakeProbe::default().with(
            "nodekey:alpha",
            TailscalePeerHelloProbe::Granted {
                response: response("nodekey:alpha", &["workspace-beta"]),
                latency_ms: 3,
            },
        );

        let report = autodiscover_tailscale_peers(
            Some(&local_report(vec![peer(
                "nodekey:alpha",
                &[EE_MESH_SERVICE_TAG],
            )])),
            &config(
                "workspace-alpha",
                DiscoveryMode::ServiceTag,
                &allowlist,
                &denylist,
            ),
            &mut probe,
        );

        assert!(report.ee_capable_peers.is_empty());
        assert_eq!(report.skipped_peers[0].reason, "workspace_mismatch");
        assert_eq!(
            report.degraded[0].code,
            PEER_DISCOVERY_WORKSPACE_MISMATCH_CODE
        );
    }

    #[test]
    fn autodiscovery_excludes_peer_when_service_tag_mode_and_no_ee_mesh_tag_advertised() {
        let allowlist = BTreeSet::new();
        let denylist = BTreeSet::new();
        let mut probe = FakeProbe::default();

        let report = autodiscover_tailscale_peers(
            Some(&local_report(vec![peer("nodekey:alpha", &[])])),
            &config(
                "workspace-alpha",
                DiscoveryMode::ServiceTag,
                &allowlist,
                &denylist,
            ),
            &mut probe,
        );

        assert!(probe.calls.is_empty());
        assert_eq!(report.skipped_peers[0].reason, "no_discovery_consent");
    }

    #[test]
    fn autodiscovery_includes_peer_when_allowlist_mode_and_node_key_in_allowlist() {
        let allowlist = BTreeSet::from(["nodekey:alpha".to_owned()]);
        let denylist = BTreeSet::new();
        let mut probe = FakeProbe::default().with(
            "nodekey:alpha",
            TailscalePeerHelloProbe::Granted {
                response: response("nodekey:alpha", &["workspace-alpha"]),
                latency_ms: 1,
            },
        );

        let report = autodiscover_tailscale_peers(
            Some(&local_report(vec![peer("nodekey:alpha", &[])])),
            &config(
                "workspace-alpha",
                DiscoveryMode::Allowlist,
                &allowlist,
                &denylist,
            ),
            &mut probe,
        );

        assert_eq!(report.eligible_peer_count, 1);
        assert_eq!(
            report.ee_capable_peers[0].discovery_policy_decision,
            "allowlisted"
        );
    }

    #[test]
    fn autodiscovery_skips_explicitly_offline_peers_without_probing() {
        let allowlist = BTreeSet::new();
        let denylist = BTreeSet::new();
        let mut offline_peer = peer("nodekey:alpha", &[EE_MESH_SERVICE_TAG]);
        offline_peer.online = Some(false);
        offline_peer.ee_capability = Some(TailscalePeerEeCapability {
            ee_version: "0.2.0".to_owned(),
            ee_protocol_version: "1.0".to_owned(),
            workspace_ids: vec!["workspace-alpha".to_owned()],
            respond: true,
            latency_ms: 1,
        });
        let mut probe = FakeProbe::default().with(
            "nodekey:alpha",
            TailscalePeerHelloProbe::Granted {
                response: response("nodekey:alpha", &["workspace-alpha"]),
                latency_ms: 1,
            },
        );

        let report = autodiscover_tailscale_peers(
            Some(&local_report(vec![offline_peer])),
            &config(
                "workspace-alpha",
                DiscoveryMode::ServiceTag,
                &allowlist,
                &denylist,
            ),
            &mut probe,
        );

        assert!(probe.calls.is_empty());
        assert_eq!(report.probed_peer_count, 0);
        assert!(report.ee_capable_peers.is_empty());
        assert_eq!(report.skipped_peers[0].node_key, "nodekey:alpha");
        assert_eq!(report.skipped_peers[0].reason, "non_ee");
    }

    #[test]
    fn autodiscovery_honors_per_peer_750ms_budget_and_5s_total_budget() {
        let allowlist = BTreeSet::new();
        let denylist = BTreeSet::new();
        let peers: Vec<_> = (0..8)
            .map(|idx| peer(&format!("nodekey:{idx:02}"), &[EE_MESH_SERVICE_TAG]))
            .collect();
        let mut probe = FakeProbe::default();
        for idx in 0..8 {
            probe = probe.with(
                &format!("nodekey:{idx:02}"),
                TailscalePeerHelloProbe::Timeout { elapsed_ms: 750 },
            );
        }

        let report = autodiscover_tailscale_peers(
            Some(&local_report(peers)),
            &config(
                "workspace-alpha",
                DiscoveryMode::ServiceTag,
                &allowlist,
                &denylist,
            ),
            &mut probe,
        );

        assert_eq!(report.probed_peer_count, 7);
        assert!(
            report
                .degraded
                .iter()
                .any(|item| item.code == TAILSCALE_PEER_PROBE_TIMEOUT_CODE)
        );
        assert!(
            report
                .degraded
                .iter()
                .any(|item| item.code == PEER_DISCOVERY_BUDGET_EXHAUSTED_CODE)
        );
    }

    #[test]
    fn autodiscovery_skipped_when_ee_mesh_enabled_is_zero() {
        let allowlist = BTreeSet::new();
        let denylist = BTreeSet::new();
        let mut cfg = config(
            "workspace-alpha",
            DiscoveryMode::ServiceTag,
            &allowlist,
            &denylist,
        );
        cfg.mesh_enabled = false;
        let mut probe = FakeProbe::default();

        let report = autodiscover_tailscale_peers(
            Some(&local_report(vec![peer(
                "nodekey:alpha",
                &[EE_MESH_SERVICE_TAG],
            )])),
            &cfg,
            &mut probe,
        );

        assert!(probe.calls.is_empty());
        assert_eq!(
            report.degraded[0].code,
            TAILSCALE_PEER_LIST_UNAVAILABLE_CODE
        );
    }

    #[test]
    fn autodiscovery_eligible_peers_sorted_lexicographically_by_node_key() {
        let allowlist = BTreeSet::new();
        let denylist = BTreeSet::new();
        let mut probe = FakeProbe::default()
            .with(
                "nodekey:zulu",
                TailscalePeerHelloProbe::Granted {
                    response: response("nodekey:zulu", &["workspace-alpha"]),
                    latency_ms: 1,
                },
            )
            .with(
                "nodekey:alpha",
                TailscalePeerHelloProbe::Granted {
                    response: response("nodekey:alpha", &["workspace-alpha"]),
                    latency_ms: 1,
                },
            );

        let report = autodiscover_tailscale_peers(
            Some(&local_report(vec![
                peer("nodekey:zulu", &[EE_MESH_SERVICE_TAG]),
                peer("nodekey:alpha", &[EE_MESH_SERVICE_TAG]),
            ])),
            &config(
                "workspace-alpha",
                DiscoveryMode::ServiceTag,
                &allowlist,
                &denylist,
            ),
            &mut probe,
        );

        let keys: Vec<_> = report
            .ee_capable_peers
            .iter()
            .map(|peer| peer.node_key.as_str())
            .collect();
        assert_eq!(keys, ["nodekey:alpha", "nodekey:zulu"]);
    }

    #[test]
    fn autodiscovery_cancellation_drops_in_flight_probes_within_budget() {
        let allowlist = BTreeSet::new();
        let denylist = BTreeSet::new();
        let mut probe = FakeProbe {
            cancel_after_calls: Some(1),
            ..FakeProbe::default()
        }
        .with(
            "nodekey:alpha",
            TailscalePeerHelloProbe::Granted {
                response: response("nodekey:alpha", &["workspace-alpha"]),
                latency_ms: 10,
            },
        )
        .with(
            "nodekey:beta",
            TailscalePeerHelloProbe::Granted {
                response: response("nodekey:beta", &["workspace-alpha"]),
                latency_ms: 10,
            },
        );

        let report = autodiscover_tailscale_peers(
            Some(&local_report(vec![
                peer("nodekey:alpha", &[EE_MESH_SERVICE_TAG]),
                peer("nodekey:beta", &[EE_MESH_SERVICE_TAG]),
            ])),
            &config(
                "workspace-alpha",
                DiscoveryMode::ServiceTag,
                &allowlist,
                &denylist,
            ),
            &mut probe,
        );

        assert_eq!(probe.calls, ["nodekey:alpha"]);
        assert_eq!(report.eligible_peer_count, 1);
        assert_eq!(report.skipped_peers[0].node_key, "nodekey:beta");
        assert!(
            report
                .degraded
                .iter()
                .any(|item| item.code == PEER_DISCOVERY_BUDGET_EXHAUSTED_CODE)
        );
    }
}
