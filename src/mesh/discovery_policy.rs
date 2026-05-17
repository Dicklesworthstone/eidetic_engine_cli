//! SRR6.46.7 — Per-peer discovery policy (privacy).
//!
//! Three discovery modes select who SRR6.46.2 autodiscovery probes
//! (`discoveryMode`) and who the SRR6.46.12 hello responder admits
//! (`respondMode`):
//!
//! - `service_tag` (default): only peers advertising the `tag:ee-mesh`
//!   Tailscale ACL tag are probed; the responder only admits peers we
//!   also advertise the tag to. Privacy-by-default — peers on the same
//!   tailnet who haven't opted in to ee discovery never receive (or
//!   respond to) a hello probe.
//! - `auto_admit`: probe every reachable peer; respond to anyone. Simple
//!   but leaks "ee is here" to every tailnet host. Suitable for trusted
//!   corporate tailnets.
//! - `allowlist`: probe only peers whose `nodeKey` appears in
//!   `.ee/discovery_allowlist.toml`; respond only to peers whose
//!   `nodeKey` appears in `.ee/respond_allowlist.toml`.
//!
//! A workspace-scoped denylist (`.ee/discovery_denylist.toml`) overrides
//! all three modes — denylisted peers are never probed and never
//! receive a response.
//!
//! This module owns the **pure decision logic** and the TOML
//! allowlist/denylist file loaders. The CLI surface (`ee mesh
//! discovery-policy [set|allow|deny|--explain]`) and the env-var
//! plumbing (`EE_TAILSCALE_DISCOVERY_MODE`, `EE_TAILSCALE_RESPOND_MODE`)
//! land in follow-up slices to avoid touching `src/cli/mod.rs` and
//! `src/config/env_registry.rs` while other agents hold reservations.
//!
//! The SRR6.46.6 hello-handshake responder integration and the
//! SRR6.46.2 autodiscovery integration both consume this module's
//! `decide_discovery` / `decide_respond` functions; their wiring lands
//! in those beads.

use std::collections::BTreeSet;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// JSON schema identifier for the `ee mesh discovery-policy --json`
/// surface. Held here as the source-of-truth constant so the renderer
/// and the schema-lifecycle drift gate agree.
pub const DISCOVERY_POLICY_SCHEMA_V1: &str = "ee.mesh.discovery_policy.v1";

/// The well-known Tailscale ACL tag opt-in marker. A peer advertises
/// `tag:ee-mesh` via `tailscale up --advertise-tags=tag:ee-mesh` to
/// signal that it is an ee participant willing to be discovered.
pub const EE_MESH_SERVICE_TAG: &str = "tag:ee-mesh";

/// Degraded code emitted when `respondMode=service_tag` is set but the
/// host does not advertise `tag:ee-mesh`. Nobody on the tailnet will be
/// able to discover us; the user almost certainly didn't mean this.
pub const DISCOVERY_POLICY_NO_EE_MESH_TAG_CODE: &str = "discovery_policy_no_ee_mesh_tag";

/// Degraded code emitted when the caller is in `allowlist` mode but
/// the allowlist file is empty. Nothing will be probed.
pub const DISCOVERY_POLICY_EMPTY_ALLOWLIST_CODE: &str = "discovery_policy_empty_allowlist";

/// Default file name for the workspace allowlist (`<workspace>/.ee/discovery_allowlist.toml`).
pub const DISCOVERY_ALLOWLIST_FILE: &str = "discovery_allowlist.toml";

/// Default file name for the workspace denylist (`<workspace>/.ee/discovery_denylist.toml`).
pub const DISCOVERY_DENYLIST_FILE: &str = "discovery_denylist.toml";

/// Default file name for the responder-side allowlist (`<workspace>/.ee/respond_allowlist.toml`).
pub const RESPOND_ALLOWLIST_FILE: &str = "respond_allowlist.toml";

/// The three discovery modes that gate both probing (caller side) and
/// admission (responder side). The same enum is reused for both surfaces
/// so the discovery-policy schema is symmetric.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiscoveryMode {
    /// Privacy-by-default: only peers advertising `tag:ee-mesh` are
    /// admitted. Recommended for shared / personal tailnets where ee
    /// participation should be opt-in per host.
    ServiceTag,
    /// Trusted-tailnet mode: probe every reachable peer; respond to
    /// anyone. Simpler but leaks "ee is here" to every tailnet member.
    AutoAdmit,
    /// Most restrictive: explicit allowlist of nodeKeys.
    Allowlist,
}

impl DiscoveryMode {
    /// Canonical schema representation (matches the `discoveryMode` /
    /// `respondMode` enum values in `ee.mesh.discovery_policy.v1`).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ServiceTag => "service_tag",
            Self::AutoAdmit => "auto_admit",
            Self::Allowlist => "allowlist",
        }
    }

    /// Default discovery mode: privacy-by-default `service_tag`.
    #[must_use]
    pub const fn default_mode() -> Self {
        Self::ServiceTag
    }
}

impl Default for DiscoveryMode {
    fn default() -> Self {
        Self::default_mode()
    }
}

/// The decision SRR6.46.2 reaches on a per-peer basis when deciding
/// whether to issue a hello probe.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiscoveryDecision {
    Probe,
    Skip,
}

impl DiscoveryDecision {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Probe => "probe",
            Self::Skip => "skip",
        }
    }
}

/// The decision SRR6.46.12 hello responder reaches on a per-incoming-request
/// basis. The decline path uses `DiscoveryConsent::Denied` so the hello
/// error response in SRR6.46.6 returns `discovery_consent_denied` with
/// no metadata leak.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiscoveryConsent {
    Granted,
    Denied,
}

impl DiscoveryConsent {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Granted => "granted",
            Self::Denied => "denied",
        }
    }
}

/// Reason code attached to a [`DiscoveryDecision`] for forensic logging
/// and the `--explain` decision tree.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiscoveryReason {
    /// `auto_admit` mode + peer not denylisted.
    AutoAdmit,
    /// `service_tag` mode + peer advertises `tag:ee-mesh` + not denylisted.
    ServiceTagMatch,
    /// `allowlist` mode + peer in `discovery_allowlist.toml` + not denylisted.
    Allowlisted,
    /// `service_tag` mode + peer does not advertise the tag.
    SkipNoTag,
    /// `allowlist` mode + peer not in `discovery_allowlist.toml`.
    SkipNotAllowlisted,
    /// Peer appears in `discovery_denylist.toml`. Takes priority over
    /// `auto_admit` / `service_tag` / `allowlist`.
    SkipDenylisted,
    /// Peer's nodeKey matches the caller's `selfNodeKey`. We never
    /// probe ourselves; this is a defensive guard.
    SkipSelf,
}

impl DiscoveryReason {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AutoAdmit => "auto_admit",
            Self::ServiceTagMatch => "service_tag_match",
            Self::Allowlisted => "allowlisted",
            Self::SkipNoTag => "skip_no_tag",
            Self::SkipNotAllowlisted => "skip_not_allowlisted",
            Self::SkipDenylisted => "skip_denylisted",
            Self::SkipSelf => "skip_self",
        }
    }
}

/// Caller-side inputs for [`decide_discovery`]. Borrowed-only so the
/// decision function is a pure read with no allocations beyond the
/// returned enum.
#[derive(Clone, Copy, Debug)]
pub struct DiscoveryDecisionInput<'a> {
    pub mode: DiscoveryMode,
    pub peer_node_key: &'a str,
    pub peer_advertised_tags: &'a [String],
    pub self_node_key: &'a str,
    pub allowlist: &'a BTreeSet<String>,
    pub denylist: &'a BTreeSet<String>,
}

/// Responder-side inputs for [`decide_respond`]. Mirrors
/// [`DiscoveryDecisionInput`] but flipped — `requester_*` is the peer
/// that just sent us a hello.
#[derive(Clone, Copy, Debug)]
pub struct RespondDecisionInput<'a> {
    pub mode: DiscoveryMode,
    pub requester_node_key: &'a str,
    pub requester_advertised_tags: &'a [String],
    pub self_advertised_tags: &'a [String],
    pub respond_allowlist: &'a BTreeSet<String>,
    pub denylist: &'a BTreeSet<String>,
}

/// Caller-side decision for whether to probe a peer.
///
/// Evaluation order (first-match wins):
///
/// 1. `peer_node_key == self_node_key` → `SkipSelf`. Never probe self.
/// 2. Peer in denylist → `SkipDenylisted`. Denylist overrides modes.
/// 3. `mode == AutoAdmit` → `Probe` with `AutoAdmit` reason.
/// 4. `mode == ServiceTag` + peer advertises `EE_MESH_SERVICE_TAG` →
///    `Probe` with `ServiceTagMatch`.
/// 5. `mode == ServiceTag` + peer does not advertise the tag →
///    `Skip` with `SkipNoTag`.
/// 6. `mode == Allowlist` + peer in allowlist → `Probe` with `Allowlisted`.
/// 7. `mode == Allowlist` + peer not in allowlist → `Skip` with
///    `SkipNotAllowlisted`.
#[must_use]
pub fn decide_discovery(
    input: &DiscoveryDecisionInput<'_>,
) -> (DiscoveryDecision, DiscoveryReason) {
    if input.peer_node_key == input.self_node_key {
        return (DiscoveryDecision::Skip, DiscoveryReason::SkipSelf);
    }
    if input.denylist.contains(input.peer_node_key) {
        return (DiscoveryDecision::Skip, DiscoveryReason::SkipDenylisted);
    }
    match input.mode {
        DiscoveryMode::AutoAdmit => (DiscoveryDecision::Probe, DiscoveryReason::AutoAdmit),
        DiscoveryMode::ServiceTag => {
            if input
                .peer_advertised_tags
                .iter()
                .any(|tag| tag == EE_MESH_SERVICE_TAG)
            {
                (DiscoveryDecision::Probe, DiscoveryReason::ServiceTagMatch)
            } else {
                (DiscoveryDecision::Skip, DiscoveryReason::SkipNoTag)
            }
        }
        DiscoveryMode::Allowlist => {
            if input.allowlist.contains(input.peer_node_key) {
                (DiscoveryDecision::Probe, DiscoveryReason::Allowlisted)
            } else {
                (DiscoveryDecision::Skip, DiscoveryReason::SkipNotAllowlisted)
            }
        }
    }
}

/// Responder-side decision for whether to admit a hello request.
///
/// Evaluation order (first-match wins):
///
/// 1. Requester in denylist → `Denied` with `SkipDenylisted`.
/// 2. `mode == AutoAdmit` → `Granted` with `AutoAdmit`. Respond to anyone.
/// 3. `mode == ServiceTag` + WE advertise `EE_MESH_SERVICE_TAG` →
///    `Granted` with `ServiceTagMatch`. The peer's tags are not
///    consulted on the responder side because the responder's own tag
///    advertisement is what signals opt-in.
/// 4. `mode == ServiceTag` + WE do not advertise the tag → `Denied`
///    with `SkipNoTag`. We told the tailnet we don't participate in
///    ee discovery, so we honor that on the response side.
/// 5. `mode == Allowlist` + requester in respond-allowlist → `Granted`
///    with `Allowlisted`.
/// 6. `mode == Allowlist` + requester not in respond-allowlist →
///    `Denied` with `SkipNotAllowlisted`.
#[must_use]
pub fn decide_respond(input: &RespondDecisionInput<'_>) -> (DiscoveryConsent, DiscoveryReason) {
    if input.denylist.contains(input.requester_node_key) {
        return (DiscoveryConsent::Denied, DiscoveryReason::SkipDenylisted);
    }
    let _ = input.requester_advertised_tags; // caller-side filter only
    match input.mode {
        DiscoveryMode::AutoAdmit => (DiscoveryConsent::Granted, DiscoveryReason::AutoAdmit),
        DiscoveryMode::ServiceTag => {
            if input
                .self_advertised_tags
                .iter()
                .any(|tag| tag == EE_MESH_SERVICE_TAG)
            {
                (DiscoveryConsent::Granted, DiscoveryReason::ServiceTagMatch)
            } else {
                (DiscoveryConsent::Denied, DiscoveryReason::SkipNoTag)
            }
        }
        DiscoveryMode::Allowlist => {
            if input.respond_allowlist.contains(input.requester_node_key) {
                (DiscoveryConsent::Granted, DiscoveryReason::Allowlisted)
            } else {
                (
                    DiscoveryConsent::Denied,
                    DiscoveryReason::SkipNotAllowlisted,
                )
            }
        }
    }
}

/// On-disk TOML shape for the workspace allowlist / denylist files.
///
/// Tolerates absent files (empty set) and tolerates unknown fields
/// (forward-compat) — only the `node_keys` array is required.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct NodeKeyListFile {
    #[serde(default)]
    pub node_keys: Vec<String>,
}

/// Error variants for [`load_node_key_list`].
#[derive(Debug)]
pub enum LoadListError {
    Read(std::io::Error),
    Parse(toml::de::Error),
}

impl std::fmt::Display for LoadListError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Read(error) => write!(f, "failed to read discovery list file: {error}"),
            Self::Parse(error) => write!(f, "failed to parse discovery list TOML: {error}"),
        }
    }
}

impl std::error::Error for LoadListError {}

/// Load a node-key list TOML file. Absent file → empty set (this is the
/// expected state for fresh workspaces; `service_tag` and `auto_admit`
/// modes work without any list file present).
///
/// Trims whitespace from each entry and skips empty lines; lowercases
/// nothing (node-keys are case-sensitive). Duplicates are deduplicated
/// via the `BTreeSet` contract.
pub fn load_node_key_list(path: &Path) -> Result<BTreeSet<String>, LoadListError> {
    let body = match std::fs::read_to_string(path) {
        Ok(body) => body,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeSet::new()),
        Err(error) => return Err(LoadListError::Read(error)),
    };
    let parsed: NodeKeyListFile = toml::from_str(&body).map_err(LoadListError::Parse)?;
    Ok(parsed
        .node_keys
        .into_iter()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .collect())
}

/// Convenience: load all three workspace list files (`.ee/discovery_allowlist.toml`,
/// `.ee/discovery_denylist.toml`, `.ee/respond_allowlist.toml`).
///
/// Returns the three sets in order. Any individual missing file is
/// silently treated as empty; only an actual parse error or read error
/// propagates.
pub fn load_workspace_lists(workspace_path: &Path) -> Result<WorkspaceLists, LoadListError> {
    let ee_dir = workspace_path.join(".ee");
    let allowlist = load_node_key_list(&ee_dir.join(DISCOVERY_ALLOWLIST_FILE))?;
    let denylist = load_node_key_list(&ee_dir.join(DISCOVERY_DENYLIST_FILE))?;
    let respond_allowlist = load_node_key_list(&ee_dir.join(RESPOND_ALLOWLIST_FILE))?;
    Ok(WorkspaceLists {
        allowlist,
        denylist,
        respond_allowlist,
    })
}

/// Bundle returned by [`load_workspace_lists`].
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct WorkspaceLists {
    pub allowlist: BTreeSet<String>,
    pub denylist: BTreeSet<String>,
    pub respond_allowlist: BTreeSet<String>,
}

/// Posture-evaluation degradation: detects misconfigurations that the
/// `--json` surface should surface to the operator.
///
/// Empty when the configuration is internally consistent.
#[must_use]
pub fn evaluate_policy_degradations(
    discovery_mode: DiscoveryMode,
    respond_mode: DiscoveryMode,
    self_advertised_tags: &[String],
    discovery_allowlist: &BTreeSet<String>,
) -> Vec<PolicyDegradation> {
    let mut out = Vec::new();
    if respond_mode == DiscoveryMode::ServiceTag
        && !self_advertised_tags
            .iter()
            .any(|tag| tag == EE_MESH_SERVICE_TAG)
    {
        out.push(PolicyDegradation {
            code: DISCOVERY_POLICY_NO_EE_MESH_TAG_CODE,
            severity: "info",
            message: format!(
                "respondMode is {} but this host does not advertise {EE_MESH_SERVICE_TAG}; peers will see decline responses",
                DiscoveryMode::ServiceTag.as_str(),
            ),
            repair: "tailscale up --advertise-tags=tag:ee-mesh",
        });
    }
    if discovery_mode == DiscoveryMode::Allowlist && discovery_allowlist.is_empty() {
        out.push(PolicyDegradation {
            code: DISCOVERY_POLICY_EMPTY_ALLOWLIST_CODE,
            severity: "info",
            message: format!(
                "discoveryMode is {} but the allowlist is empty; no peers will be probed",
                DiscoveryMode::Allowlist.as_str(),
            ),
            repair: "ee mesh discovery-policy allow <node-key>",
        });
    }
    out
}

/// Static-string-bearing degradation record used by
/// [`evaluate_policy_degradations`]. The `&'static str` fields keep
/// allocations off the hot path; the `message` field is owned because
/// it embeds mode-specific text.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PolicyDegradation {
    pub code: &'static str,
    pub severity: &'static str,
    pub message: String,
    pub repair: &'static str,
}

// ============================================================================
// Inline tests (AGENTS.md L300-302 / bd-3usjw.62 Rule 7)
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn empty_set() -> BTreeSet<String> {
        BTreeSet::new()
    }

    fn set_of(items: &[&str]) -> BTreeSet<String> {
        items.iter().map(|s| (*s).to_owned()).collect()
    }

    // ---- Mode + enum round-trips -------------------------------------------

    #[test]
    fn discovery_mode_default_is_service_tag() {
        assert_eq!(DiscoveryMode::default(), DiscoveryMode::ServiceTag);
        assert_eq!(DiscoveryMode::default_mode(), DiscoveryMode::ServiceTag);
        assert_eq!(DiscoveryMode::default().as_str(), "service_tag");
    }

    #[test]
    fn discovery_mode_round_trips_through_serde_snake_case() {
        for mode in [
            DiscoveryMode::ServiceTag,
            DiscoveryMode::AutoAdmit,
            DiscoveryMode::Allowlist,
        ] {
            let serialized = serde_json::to_string(&mode).expect("serialize");
            let deserialized: DiscoveryMode =
                serde_json::from_str(&serialized).expect("deserialize");
            assert_eq!(deserialized, mode);
            assert!(serialized.contains(mode.as_str()));
        }
    }

    #[test]
    fn discovery_reason_str_matches_snake_case_serde() {
        for reason in [
            DiscoveryReason::AutoAdmit,
            DiscoveryReason::ServiceTagMatch,
            DiscoveryReason::Allowlisted,
            DiscoveryReason::SkipNoTag,
            DiscoveryReason::SkipNotAllowlisted,
            DiscoveryReason::SkipDenylisted,
            DiscoveryReason::SkipSelf,
        ] {
            let serialized = serde_json::to_string(&reason).expect("serialize");
            assert!(serialized.contains(reason.as_str()));
        }
    }

    // ---- Caller-side discovery decisions -----------------------------------

    #[test]
    fn caller_skips_self_node_key_in_every_mode() {
        let tags = vec![EE_MESH_SERVICE_TAG.to_owned()];
        let allow = set_of(&["nodekey:self"]);
        let deny = empty_set();
        for mode in [
            DiscoveryMode::ServiceTag,
            DiscoveryMode::AutoAdmit,
            DiscoveryMode::Allowlist,
        ] {
            let (decision, reason) = decide_discovery(&DiscoveryDecisionInput {
                mode,
                peer_node_key: "nodekey:self",
                peer_advertised_tags: &tags,
                self_node_key: "nodekey:self",
                allowlist: &allow,
                denylist: &deny,
            });
            assert_eq!(decision, DiscoveryDecision::Skip);
            assert_eq!(reason, DiscoveryReason::SkipSelf);
        }
    }

    #[test]
    fn caller_denylist_overrides_auto_admit() {
        let deny = set_of(&["nodekey:bad"]);
        let (decision, reason) = decide_discovery(&DiscoveryDecisionInput {
            mode: DiscoveryMode::AutoAdmit,
            peer_node_key: "nodekey:bad",
            peer_advertised_tags: &[],
            self_node_key: "nodekey:self",
            allowlist: &empty_set(),
            denylist: &deny,
        });
        assert_eq!(decision, DiscoveryDecision::Skip);
        assert_eq!(reason, DiscoveryReason::SkipDenylisted);
    }

    #[test]
    fn caller_denylist_overrides_allowlist() {
        let allow = set_of(&["nodekey:peer"]);
        let deny = set_of(&["nodekey:peer"]);
        let (decision, reason) = decide_discovery(&DiscoveryDecisionInput {
            mode: DiscoveryMode::Allowlist,
            peer_node_key: "nodekey:peer",
            peer_advertised_tags: &[],
            self_node_key: "nodekey:self",
            allowlist: &allow,
            denylist: &deny,
        });
        assert_eq!(decision, DiscoveryDecision::Skip);
        assert_eq!(reason, DiscoveryReason::SkipDenylisted);
    }

    #[test]
    fn caller_auto_admit_probes_peer_without_tag() {
        let (decision, reason) = decide_discovery(&DiscoveryDecisionInput {
            mode: DiscoveryMode::AutoAdmit,
            peer_node_key: "nodekey:peer",
            peer_advertised_tags: &[],
            self_node_key: "nodekey:self",
            allowlist: &empty_set(),
            denylist: &empty_set(),
        });
        assert_eq!(decision, DiscoveryDecision::Probe);
        assert_eq!(reason, DiscoveryReason::AutoAdmit);
    }

    #[test]
    fn caller_service_tag_probes_peer_with_ee_mesh_tag() {
        let tags = vec![EE_MESH_SERVICE_TAG.to_owned()];
        let (decision, reason) = decide_discovery(&DiscoveryDecisionInput {
            mode: DiscoveryMode::ServiceTag,
            peer_node_key: "nodekey:peer",
            peer_advertised_tags: &tags,
            self_node_key: "nodekey:self",
            allowlist: &empty_set(),
            denylist: &empty_set(),
        });
        assert_eq!(decision, DiscoveryDecision::Probe);
        assert_eq!(reason, DiscoveryReason::ServiceTagMatch);
    }

    #[test]
    fn caller_service_tag_skips_peer_without_ee_mesh_tag() {
        let tags = vec!["tag:something-else".to_owned()];
        let (decision, reason) = decide_discovery(&DiscoveryDecisionInput {
            mode: DiscoveryMode::ServiceTag,
            peer_node_key: "nodekey:peer",
            peer_advertised_tags: &tags,
            self_node_key: "nodekey:self",
            allowlist: &empty_set(),
            denylist: &empty_set(),
        });
        assert_eq!(decision, DiscoveryDecision::Skip);
        assert_eq!(reason, DiscoveryReason::SkipNoTag);
    }

    #[test]
    fn caller_allowlist_probes_listed_node_key() {
        let allow = set_of(&["nodekey:friend"]);
        let (decision, reason) = decide_discovery(&DiscoveryDecisionInput {
            mode: DiscoveryMode::Allowlist,
            peer_node_key: "nodekey:friend",
            peer_advertised_tags: &[],
            self_node_key: "nodekey:self",
            allowlist: &allow,
            denylist: &empty_set(),
        });
        assert_eq!(decision, DiscoveryDecision::Probe);
        assert_eq!(reason, DiscoveryReason::Allowlisted);
    }

    #[test]
    fn caller_allowlist_skips_unlisted_node_key() {
        let allow = set_of(&["nodekey:friend"]);
        let (decision, reason) = decide_discovery(&DiscoveryDecisionInput {
            mode: DiscoveryMode::Allowlist,
            peer_node_key: "nodekey:stranger",
            peer_advertised_tags: &[],
            self_node_key: "nodekey:self",
            allowlist: &allow,
            denylist: &empty_set(),
        });
        assert_eq!(decision, DiscoveryDecision::Skip);
        assert_eq!(reason, DiscoveryReason::SkipNotAllowlisted);
    }

    // ---- Responder-side decisions ------------------------------------------

    #[test]
    fn responder_denylist_overrides_auto_admit() {
        let deny = set_of(&["nodekey:bad"]);
        let (consent, reason) = decide_respond(&RespondDecisionInput {
            mode: DiscoveryMode::AutoAdmit,
            requester_node_key: "nodekey:bad",
            requester_advertised_tags: &[],
            self_advertised_tags: &[],
            respond_allowlist: &empty_set(),
            denylist: &deny,
        });
        assert_eq!(consent, DiscoveryConsent::Denied);
        assert_eq!(reason, DiscoveryReason::SkipDenylisted);
    }

    #[test]
    fn responder_service_tag_grants_when_self_advertises_tag() {
        let self_tags = vec![EE_MESH_SERVICE_TAG.to_owned()];
        let (consent, reason) = decide_respond(&RespondDecisionInput {
            mode: DiscoveryMode::ServiceTag,
            requester_node_key: "nodekey:peer",
            requester_advertised_tags: &[],
            self_advertised_tags: &self_tags,
            respond_allowlist: &empty_set(),
            denylist: &empty_set(),
        });
        assert_eq!(consent, DiscoveryConsent::Granted);
        assert_eq!(reason, DiscoveryReason::ServiceTagMatch);
    }

    #[test]
    fn responder_service_tag_denies_when_self_does_not_advertise_tag() {
        let (consent, reason) = decide_respond(&RespondDecisionInput {
            mode: DiscoveryMode::ServiceTag,
            requester_node_key: "nodekey:peer",
            requester_advertised_tags: &[EE_MESH_SERVICE_TAG.to_owned()],
            self_advertised_tags: &[],
            respond_allowlist: &empty_set(),
            denylist: &empty_set(),
        });
        assert_eq!(consent, DiscoveryConsent::Denied);
        assert_eq!(reason, DiscoveryReason::SkipNoTag);
    }

    #[test]
    fn responder_allowlist_grants_listed_requester() {
        let allow = set_of(&["nodekey:peer"]);
        let (consent, reason) = decide_respond(&RespondDecisionInput {
            mode: DiscoveryMode::Allowlist,
            requester_node_key: "nodekey:peer",
            requester_advertised_tags: &[],
            self_advertised_tags: &[],
            respond_allowlist: &allow,
            denylist: &empty_set(),
        });
        assert_eq!(consent, DiscoveryConsent::Granted);
        assert_eq!(reason, DiscoveryReason::Allowlisted);
    }

    #[test]
    fn responder_allowlist_denies_unlisted_requester() {
        let allow = set_of(&["nodekey:peer"]);
        let (consent, reason) = decide_respond(&RespondDecisionInput {
            mode: DiscoveryMode::Allowlist,
            requester_node_key: "nodekey:stranger",
            requester_advertised_tags: &[],
            self_advertised_tags: &[],
            respond_allowlist: &allow,
            denylist: &empty_set(),
        });
        assert_eq!(consent, DiscoveryConsent::Denied);
        assert_eq!(reason, DiscoveryReason::SkipNotAllowlisted);
    }

    // ---- TOML list loader --------------------------------------------------

    #[test]
    fn load_node_key_list_returns_empty_set_when_file_missing() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let path = tempdir.path().join("missing.toml");
        let result = load_node_key_list(&path).expect("missing file is ok");
        assert!(result.is_empty());
    }

    #[test]
    fn load_node_key_list_parses_simple_toml() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let path = tempdir.path().join("list.toml");
        let mut file = std::fs::File::create(&path).expect("create");
        writeln!(file, "node_keys = [\"nodekey:alpha\", \"nodekey:bravo\"]").expect("write");
        drop(file);
        let result = load_node_key_list(&path).expect("parse");
        assert_eq!(result.len(), 2);
        assert!(result.contains("nodekey:alpha"));
        assert!(result.contains("nodekey:bravo"));
    }

    #[test]
    fn load_node_key_list_deduplicates_and_trims() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let path = tempdir.path().join("list.toml");
        let mut file = std::fs::File::create(&path).expect("create");
        writeln!(
            file,
            "node_keys = [\"nodekey:alpha\", \"  nodekey:alpha  \", \"\", \"   \"]"
        )
        .expect("write");
        drop(file);
        let result = load_node_key_list(&path).expect("parse");
        assert_eq!(
            result.len(),
            1,
            "expected dedup + empty drop, got {result:?}"
        );
        assert!(result.contains("nodekey:alpha"));
    }

    #[test]
    fn load_node_key_list_rejects_invalid_toml() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let path = tempdir.path().join("list.toml");
        let mut file = std::fs::File::create(&path).expect("create");
        writeln!(file, "node_keys = this is not toml [").expect("write");
        drop(file);
        let result = load_node_key_list(&path);
        assert!(matches!(result, Err(LoadListError::Parse(_))));
    }

    #[test]
    fn load_workspace_lists_returns_default_when_ee_dir_missing() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let lists = load_workspace_lists(tempdir.path()).expect("ok");
        assert!(lists.allowlist.is_empty());
        assert!(lists.denylist.is_empty());
        assert!(lists.respond_allowlist.is_empty());
    }

    #[test]
    fn load_workspace_lists_reads_all_three_files_when_present() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let ee_dir = tempdir.path().join(".ee");
        std::fs::create_dir(&ee_dir).expect("mkdir");
        for (name, key) in [
            (DISCOVERY_ALLOWLIST_FILE, "nodekey:allowed"),
            (DISCOVERY_DENYLIST_FILE, "nodekey:denied"),
            (RESPOND_ALLOWLIST_FILE, "nodekey:respond"),
        ] {
            let path = ee_dir.join(name);
            std::fs::write(&path, format!("node_keys = [\"{key}\"]\n")).expect("write");
        }
        let lists = load_workspace_lists(tempdir.path()).expect("ok");
        assert!(lists.allowlist.contains("nodekey:allowed"));
        assert!(lists.denylist.contains("nodekey:denied"));
        assert!(lists.respond_allowlist.contains("nodekey:respond"));
    }

    // ---- Degradation evaluation --------------------------------------------

    #[test]
    fn evaluate_policy_degradations_flags_missing_service_tag() {
        let degradations = evaluate_policy_degradations(
            DiscoveryMode::AutoAdmit,
            DiscoveryMode::ServiceTag,
            &[],
            &empty_set(),
        );
        assert!(
            degradations
                .iter()
                .any(|d| d.code == DISCOVERY_POLICY_NO_EE_MESH_TAG_CODE)
        );
    }

    #[test]
    fn evaluate_policy_degradations_does_not_flag_when_tag_advertised() {
        let tags = vec![EE_MESH_SERVICE_TAG.to_owned()];
        let degradations = evaluate_policy_degradations(
            DiscoveryMode::AutoAdmit,
            DiscoveryMode::ServiceTag,
            &tags,
            &empty_set(),
        );
        assert!(
            degradations
                .iter()
                .all(|d| d.code != DISCOVERY_POLICY_NO_EE_MESH_TAG_CODE)
        );
    }

    #[test]
    fn evaluate_policy_degradations_flags_empty_allowlist() {
        let degradations = evaluate_policy_degradations(
            DiscoveryMode::Allowlist,
            DiscoveryMode::AutoAdmit,
            &[],
            &empty_set(),
        );
        assert!(
            degradations
                .iter()
                .any(|d| d.code == DISCOVERY_POLICY_EMPTY_ALLOWLIST_CODE)
        );
    }

    #[test]
    fn evaluate_policy_degradations_returns_empty_when_well_formed() {
        let tags = vec![EE_MESH_SERVICE_TAG.to_owned()];
        let allow = set_of(&["nodekey:friend"]);
        let degradations = evaluate_policy_degradations(
            DiscoveryMode::Allowlist,
            DiscoveryMode::ServiceTag,
            &tags,
            &allow,
        );
        assert!(degradations.is_empty());
    }

    #[test]
    fn evaluate_policy_degradations_can_return_both_codes_simultaneously() {
        let degradations = evaluate_policy_degradations(
            DiscoveryMode::Allowlist,
            DiscoveryMode::ServiceTag,
            &[],
            &empty_set(),
        );
        assert_eq!(degradations.len(), 2);
    }
}
