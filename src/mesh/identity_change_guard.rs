//! SRR6.46.8 — Identity-change guard for cross-tailnet / cross-machine refusal.
//!
//! When a workspace has a materialized auto-enrollment peer-group bound to
//! a specific tailscale identity, this module detects whether the current
//! `tailscaled` posture still matches that identity in two dimensions:
//!
//! 1. **Tailnet change**: bound `tailnetId` differs from current
//!    `tailnetId` (user logged out of tailnet A and into tailnet B on the
//!    same machine).
//! 2. **Node-key change**: bound `materializedOnNodeKey` differs from
//!    current `selfNodeKey` (db restored from a backup taken on machine
//!    A onto machine B; or tailscale was reinstalled and reissued a node
//!    key).
//!
//! Either detection produces a `medium`-severity refusal classification
//! that SRR6.46.3 auto-enrollment consumes to abort cleanly without
//! touching the peer-group table, and that SRR6.46.4 status view
//! surfaces as `drift.tailnetChanged` / `drift.nodeKeyChanged` with a
//! literal `ee mesh disable && ee mesh auto-enroll` repair command.
//!
//! Tailnet DISPLAY-NAME drift (admin renamed the tailnet but the
//! stable id is unchanged) is informational, NOT a refusal: a
//! `warning`-severity classification with a rename-notice message.
//!
//! This module is pure decision logic — no I/O, no DB writes. Callers
//! provide the bound + current values; the module returns a verdict
//! enum. The actual probe (SRR6.46.1) and the peer-group bound-state
//! reader (SRR6.30) live in their own modules.

use serde::{Deserialize, Serialize};

/// Degraded code emitted on tailnet-id mismatch. Severity: `medium`.
/// SRR6.46.3 auto-enrollment refuses to materialize when this fires.
/// Symmetric on the SRR6.46.4 status surface and the SRR6.46.14
/// steward drift reconciliation.
pub const AUTO_ENROLLMENT_TAILNET_CHANGED_CODE: &str = "auto_enrollment_tailnet_changed";

/// Degraded code emitted on node-key mismatch (backup-restored-to-different-
/// machine class). Severity: `medium`. Same caller semantics as the tailnet
/// code — SRR6.46.3 refuses; SRR6.46.4 surfaces; SRR6.46.14 honors.
pub const AUTO_ENROLLMENT_NODE_KEY_CHANGED_CODE: &str = "auto_enrollment_node_key_changed";

/// Degraded code emitted by the SRR6.46.12 hello-responder daemon job
/// at bind time when the materialized peer-group's
/// `materializedOnNodeKey` does not match the current `selfNodeKey`.
/// Severity: `high`. The daemon refuses to bind, preventing peers from
/// reaching a stale-identity responder.
pub const HELLO_RESPONDER_NODE_KEY_MISMATCH_CODE: &str = "hello_responder_node_key_mismatch";

/// Workspace-config opt-in for surfacing the identity-change check on
/// every `ee status --json` (default off; v4 design decision per
/// ADR 0038 / `bd-36bbk.1.8`).
pub const EE_MESH_CHANGE_GUARD_CHECK_ON_STATUS_ENV: &str =
    "EE_MESH_CHANGE_GUARD_CHECK_ON_STATUS";

/// The bound identity that a materialized auto-enrollment peer-group
/// captured at materialization time. All fields are owned strings so
/// the caller can construct this from a DB row read without lifetime
/// gymnastics.
///
/// When the materialized peer-group is absent (clean state), pass
/// `None` to the verdict functions — the guard returns a no-change
/// verdict and SRR6.46.3 proceeds.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct BoundIdentity {
    pub tailnet_id: String,
    pub tailnet_display_name: Option<String>,
    pub materialized_on_node_key: String,
}

/// The current identity reported by SRR6.46.1's local probe. Same
/// owned-string contract.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CurrentIdentity {
    pub tailnet_id: String,
    pub tailnet_display_name: Option<String>,
    pub self_node_key: String,
}

/// The verdict the guard returns. Variants are sorted by severity in
/// the documented evaluation order (tailnet > node-key > rename >
/// no_change), so a caller switching on this enum can handle the most
/// severe class first and fall through to less-severe.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum IdentityGuardVerdict {
    /// Bound `tailnetId` differs from current. Refuse.
    TailnetChanged {
        bound_tailnet_id: String,
        bound_tailnet_display_name: Option<String>,
        current_tailnet_id: String,
        current_tailnet_display_name: Option<String>,
    },
    /// Bound `materializedOnNodeKey` differs from current `selfNodeKey`.
    /// Refuse. (The bound `tailnetId` matches at this point.)
    NodeKeyChanged {
        bound_node_key: String,
        current_node_key: String,
    },
    /// Tailnet id matches; display name changed (admin renamed the
    /// tailnet). Informational; auto-enrollment proceeds and status
    /// view surfaces with severity `warning`.
    TailnetRenamed {
        tailnet_id: String,
        bound_display_name: Option<String>,
        current_display_name: Option<String>,
    },
    /// All checks pass. Auto-enrollment proceeds normally.
    NoChange,
    /// No materialized peer-group exists yet; the guard has nothing to
    /// check. Auto-enrollment proceeds normally.
    NoBoundIdentity,
}

impl IdentityGuardVerdict {
    /// Whether SRR6.46.3 should REFUSE auto-enrollment based on this
    /// verdict. Only `TailnetChanged` and `NodeKeyChanged` are refusal
    /// classes; rename + no-change + no-bound-identity all proceed.
    #[must_use]
    pub fn refuses_auto_enrollment(&self) -> bool {
        matches!(
            self,
            Self::TailnetChanged { .. } | Self::NodeKeyChanged { .. }
        )
    }

    /// Maps the verdict to the canonical degraded code, when applicable.
    /// Returns `None` for non-refusal verdicts.
    #[must_use]
    pub fn refusal_code(&self) -> Option<&'static str> {
        match self {
            Self::TailnetChanged { .. } => Some(AUTO_ENROLLMENT_TAILNET_CHANGED_CODE),
            Self::NodeKeyChanged { .. } => Some(AUTO_ENROLLMENT_NODE_KEY_CHANGED_CODE),
            _ => None,
        }
    }

    /// Build the canonical repair command string for the verdict. The
    /// command is the literal copy-paste an operator can run to clear
    /// the refusal — `ee mesh disable && ee mesh auto-enroll`, with
    /// the `--workspace` flag substituted and a `--reason` annotation
    /// for the node-key case so the disable audit row records the
    /// trigger.
    ///
    /// Returns `None` for non-refusal verdicts.
    #[must_use]
    pub fn repair_command(&self, workspace_path: &str) -> Option<String> {
        match self {
            Self::TailnetChanged { .. } => Some(format!(
                "ee mesh disable --workspace \"{workspace_path}\" && ee mesh auto-enroll --workspace \"{workspace_path}\""
            )),
            Self::NodeKeyChanged { .. } => Some(format!(
                "ee mesh disable --workspace \"{workspace_path}\" --reason \"restored from different machine\" && ee mesh auto-enroll --workspace \"{workspace_path}\""
            )),
            _ => None,
        }
    }

    /// Stable string tag for log/audit emission. Matches the serde
    /// `kind` field's value for each variant.
    #[must_use]
    pub fn kind_str(&self) -> &'static str {
        match self {
            Self::TailnetChanged { .. } => "tailnet_changed",
            Self::NodeKeyChanged { .. } => "node_key_changed",
            Self::TailnetRenamed { .. } => "tailnet_renamed",
            Self::NoChange => "no_change",
            Self::NoBoundIdentity => "no_bound_identity",
        }
    }
}

/// Evaluate the identity guard.
///
/// Evaluation order (first-match wins):
///
/// 1. `bound` is `None` → [`NoBoundIdentity`]. Clean state, proceed.
/// 2. `bound.tailnet_id != current.tailnet_id` → [`TailnetChanged`].
///    Refuse.
/// 3. `bound.materialized_on_node_key != current.self_node_key` →
///    [`NodeKeyChanged`]. Refuse.
/// 4. `bound.tailnet_display_name != current.tailnet_display_name`
///    (with both non-None) → [`TailnetRenamed`]. Informational; proceed.
/// 5. Otherwise → [`NoChange`]. Proceed normally.
///
/// Note: tailnet rename is only detected when BOTH display names are
/// present and differ. A transition from `Some(x)` to `None` (or vice
/// versa) is silently treated as no-change because absent display names
/// are an artifact of the probe's optional field handling — not a
/// meaningful identity change.
#[must_use]
pub fn evaluate_identity_guard(
    bound: Option<&BoundIdentity>,
    current: &CurrentIdentity,
) -> IdentityGuardVerdict {
    let Some(bound) = bound else {
        return IdentityGuardVerdict::NoBoundIdentity;
    };
    if bound.tailnet_id != current.tailnet_id {
        return IdentityGuardVerdict::TailnetChanged {
            bound_tailnet_id: bound.tailnet_id.clone(),
            bound_tailnet_display_name: bound.tailnet_display_name.clone(),
            current_tailnet_id: current.tailnet_id.clone(),
            current_tailnet_display_name: current.tailnet_display_name.clone(),
        };
    }
    if bound.materialized_on_node_key != current.self_node_key {
        return IdentityGuardVerdict::NodeKeyChanged {
            bound_node_key: bound.materialized_on_node_key.clone(),
            current_node_key: current.self_node_key.clone(),
        };
    }
    match (
        bound.tailnet_display_name.as_deref(),
        current.tailnet_display_name.as_deref(),
    ) {
        (Some(bound_name), Some(current_name)) if bound_name != current_name => {
            IdentityGuardVerdict::TailnetRenamed {
                tailnet_id: bound.tailnet_id.clone(),
                bound_display_name: Some(bound_name.to_owned()),
                current_display_name: Some(current_name.to_owned()),
            }
        }
        _ => IdentityGuardVerdict::NoChange,
    }
}

/// Convenience verdict for the SRR6.46.12 hello responder's bind-time
/// check: a strictly node-key-focused variant of the verdict tree that
/// ignores tailnet + display-name. The responder cares only about
/// "am I the same machine that materialized this binding?" because a
/// peer reaching us under a stale identity is the safety concern.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ResponderBindVerdict {
    /// Bind safely. Bound nodeKey matches current self.
    Bind,
    /// Refuse to bind. Bound nodeKey differs from current.
    RefuseNodeKeyMismatch {
        bound_node_key: String,
        current_node_key: String,
    },
    /// No bound peer-group exists. Bind safely (nothing to protect).
    BindNoBoundIdentity,
}

impl ResponderBindVerdict {
    #[must_use]
    pub fn refuses_bind(&self) -> bool {
        matches!(self, Self::RefuseNodeKeyMismatch { .. })
    }

    /// The literal degraded code SRR6.46.12 emits on refusal.
    #[must_use]
    pub fn refusal_code(&self) -> Option<&'static str> {
        match self {
            Self::RefuseNodeKeyMismatch { .. } => Some(HELLO_RESPONDER_NODE_KEY_MISMATCH_CODE),
            _ => None,
        }
    }
}

/// Hello responder bind-time check. Pure-read; SRR6.46.12 consumes
/// the verdict to decide whether to register the supervised job.
#[must_use]
pub fn evaluate_responder_bind(
    bound: Option<&BoundIdentity>,
    current_self_node_key: &str,
) -> ResponderBindVerdict {
    let Some(bound) = bound else {
        return ResponderBindVerdict::BindNoBoundIdentity;
    };
    if bound.materialized_on_node_key == current_self_node_key {
        ResponderBindVerdict::Bind
    } else {
        ResponderBindVerdict::RefuseNodeKeyMismatch {
            bound_node_key: bound.materialized_on_node_key.clone(),
            current_node_key: current_self_node_key.to_owned(),
        }
    }
}

// ============================================================================
// Inline tests (AGENTS.md L300-302 / bd-3usjw.62 Rule 7)
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn bound(tailnet: &str, display: Option<&str>, node: &str) -> BoundIdentity {
        BoundIdentity {
            tailnet_id: tailnet.to_owned(),
            tailnet_display_name: display.map(str::to_owned),
            materialized_on_node_key: node.to_owned(),
        }
    }

    fn current(tailnet: &str, display: Option<&str>, node: &str) -> CurrentIdentity {
        CurrentIdentity {
            tailnet_id: tailnet.to_owned(),
            tailnet_display_name: display.map(str::to_owned),
            self_node_key: node.to_owned(),
        }
    }

    // ---- evaluate_identity_guard -------------------------------------------

    #[test]
    fn guard_no_bound_identity_when_bound_is_none() {
        let verdict = evaluate_identity_guard(None, &current("tn_a", None, "nk_self"));
        assert_eq!(verdict, IdentityGuardVerdict::NoBoundIdentity);
        assert!(!verdict.refuses_auto_enrollment());
        assert_eq!(verdict.kind_str(), "no_bound_identity");
    }

    #[test]
    fn guard_no_change_when_both_identities_match_exactly() {
        let b = bound("tn_a", Some("team-a"), "nk_self");
        let c = current("tn_a", Some("team-a"), "nk_self");
        let verdict = evaluate_identity_guard(Some(&b), &c);
        assert_eq!(verdict, IdentityGuardVerdict::NoChange);
        assert!(!verdict.refuses_auto_enrollment());
        assert_eq!(verdict.kind_str(), "no_change");
    }

    #[test]
    fn guard_no_change_when_bound_display_name_is_none_and_current_is_some() {
        // Optional-field drift on display name alone is NOT a rename
        // detection — display name might just become available after
        // an admin sets it. Lock the behavior here.
        let b = bound("tn_a", None, "nk_self");
        let c = current("tn_a", Some("team-a"), "nk_self");
        let verdict = evaluate_identity_guard(Some(&b), &c);
        assert_eq!(verdict, IdentityGuardVerdict::NoChange);
    }

    #[test]
    fn guard_no_change_when_bound_display_name_is_some_and_current_is_none() {
        let b = bound("tn_a", Some("team-a"), "nk_self");
        let c = current("tn_a", None, "nk_self");
        let verdict = evaluate_identity_guard(Some(&b), &c);
        assert_eq!(verdict, IdentityGuardVerdict::NoChange);
    }

    #[test]
    fn guard_tailnet_changed_takes_priority_over_node_key_change() {
        // When BOTH have changed (user moved to a different tailnet on
        // a different machine — rare but possible), the tailnet check
        // fires first. The repair command for tailnet-change is
        // sufficient: disabling clears the bound state entirely, so a
        // subsequent auto-enroll picks up both new identities.
        let b = bound("tn_old", None, "nk_old");
        let c = current("tn_new", None, "nk_new");
        let verdict = evaluate_identity_guard(Some(&b), &c);
        assert!(matches!(verdict, IdentityGuardVerdict::TailnetChanged { .. }));
        assert!(verdict.refuses_auto_enrollment());
        assert_eq!(
            verdict.refusal_code(),
            Some(AUTO_ENROLLMENT_TAILNET_CHANGED_CODE)
        );
    }

    #[test]
    fn guard_tailnet_changed_captures_both_bound_and_current_ids() {
        let b = bound("tn_old", Some("old-team"), "nk_self");
        let c = current("tn_new", Some("new-team"), "nk_self");
        let verdict = evaluate_identity_guard(Some(&b), &c);
        let IdentityGuardVerdict::TailnetChanged {
            bound_tailnet_id,
            bound_tailnet_display_name,
            current_tailnet_id,
            current_tailnet_display_name,
        } = verdict
        else {
            panic!("expected TailnetChanged");
        };
        assert_eq!(bound_tailnet_id, "tn_old");
        assert_eq!(bound_tailnet_display_name.as_deref(), Some("old-team"));
        assert_eq!(current_tailnet_id, "tn_new");
        assert_eq!(current_tailnet_display_name.as_deref(), Some("new-team"));
    }

    #[test]
    fn guard_node_key_changed_when_only_node_key_differs() {
        let b = bound("tn_a", None, "nk_old");
        let c = current("tn_a", None, "nk_new");
        let verdict = evaluate_identity_guard(Some(&b), &c);
        let IdentityGuardVerdict::NodeKeyChanged {
            bound_node_key,
            current_node_key,
        } = verdict
        else {
            panic!("expected NodeKeyChanged");
        };
        assert_eq!(bound_node_key, "nk_old");
        assert_eq!(current_node_key, "nk_new");
        assert_eq!(
            evaluate_identity_guard(Some(&b), &c).refusal_code(),
            Some(AUTO_ENROLLMENT_NODE_KEY_CHANGED_CODE)
        );
    }

    #[test]
    fn guard_tailnet_renamed_when_id_matches_but_display_name_differs() {
        let b = bound("tn_a", Some("old-name"), "nk_self");
        let c = current("tn_a", Some("new-name"), "nk_self");
        let verdict = evaluate_identity_guard(Some(&b), &c);
        let IdentityGuardVerdict::TailnetRenamed {
            tailnet_id,
            bound_display_name,
            current_display_name,
        } = verdict
        else {
            panic!("expected TailnetRenamed");
        };
        assert_eq!(tailnet_id, "tn_a");
        assert_eq!(bound_display_name.as_deref(), Some("old-name"));
        assert_eq!(current_display_name.as_deref(), Some("new-name"));
        // Rename is informational — does NOT refuse auto-enrollment.
        assert!(!evaluate_identity_guard(Some(&b), &c).refuses_auto_enrollment());
    }

    #[test]
    fn guard_node_key_change_priority_when_tailnet_id_matches_but_node_key_differs() {
        // Confirms the second-tier check fires when tailnet matches.
        let b = bound("tn_a", Some("team-a"), "nk_old");
        let c = current("tn_a", Some("team-a"), "nk_new");
        let verdict = evaluate_identity_guard(Some(&b), &c);
        assert!(matches!(verdict, IdentityGuardVerdict::NodeKeyChanged { .. }));
    }

    // ---- repair_command ----------------------------------------------------

    #[test]
    fn repair_command_for_tailnet_change_includes_disable_and_auto_enroll() {
        let b = bound("tn_old", None, "nk_self");
        let c = current("tn_new", None, "nk_self");
        let verdict = evaluate_identity_guard(Some(&b), &c);
        let cmd = verdict
            .repair_command("/Users/me/projects/foo")
            .expect("repair available");
        assert!(cmd.contains("ee mesh disable --workspace \"/Users/me/projects/foo\""));
        assert!(cmd.contains("ee mesh auto-enroll --workspace \"/Users/me/projects/foo\""));
        assert!(cmd.contains("&&"));
    }

    #[test]
    fn repair_command_for_node_key_change_includes_explicit_reason_flag() {
        let b = bound("tn_a", None, "nk_old");
        let c = current("tn_a", None, "nk_new");
        let verdict = evaluate_identity_guard(Some(&b), &c);
        let cmd = verdict
            .repair_command("/Users/me/projects/foo")
            .expect("repair available");
        assert!(cmd.contains("--reason \"restored from different machine\""));
        assert!(cmd.contains("ee mesh disable"));
        assert!(cmd.contains("ee mesh auto-enroll"));
    }

    #[test]
    fn repair_command_returns_none_for_non_refusal_verdicts() {
        assert!(IdentityGuardVerdict::NoChange.repair_command("/x").is_none());
        assert!(IdentityGuardVerdict::NoBoundIdentity.repair_command("/x").is_none());
        assert!(IdentityGuardVerdict::TailnetRenamed {
            tailnet_id: "tn_a".to_owned(),
            bound_display_name: Some("old".to_owned()),
            current_display_name: Some("new".to_owned()),
        }
        .repair_command("/x")
        .is_none());
    }

    // ---- evaluate_responder_bind -------------------------------------------

    #[test]
    fn responder_bind_no_bound_identity_when_bound_is_none() {
        let verdict = evaluate_responder_bind(None, "nk_self");
        assert_eq!(verdict, ResponderBindVerdict::BindNoBoundIdentity);
        assert!(!verdict.refuses_bind());
        assert!(verdict.refusal_code().is_none());
    }

    #[test]
    fn responder_bind_succeeds_when_bound_node_key_matches_current() {
        let b = bound("tn_a", None, "nk_self");
        let verdict = evaluate_responder_bind(Some(&b), "nk_self");
        assert_eq!(verdict, ResponderBindVerdict::Bind);
        assert!(!verdict.refuses_bind());
    }

    #[test]
    fn responder_bind_refuses_on_node_key_mismatch() {
        let b = bound("tn_a", None, "nk_old");
        let verdict = evaluate_responder_bind(Some(&b), "nk_new");
        let ResponderBindVerdict::RefuseNodeKeyMismatch {
            bound_node_key,
            current_node_key,
        } = verdict.clone()
        else {
            panic!("expected RefuseNodeKeyMismatch");
        };
        assert_eq!(bound_node_key, "nk_old");
        assert_eq!(current_node_key, "nk_new");
        assert!(verdict.refuses_bind());
        assert_eq!(
            verdict.refusal_code(),
            Some(HELLO_RESPONDER_NODE_KEY_MISMATCH_CODE)
        );
    }

    #[test]
    fn responder_bind_ignores_tailnet_id_drift_when_node_key_matches() {
        // The hello responder bind check is strictly node-key focused.
        // A tailnet-only change still produces a Bind verdict here; the
        // SRR6.46.3 caller-side guard handles the tailnet-change refusal.
        let b = bound("tn_old", None, "nk_self");
        let verdict = evaluate_responder_bind(Some(&b), "nk_self");
        assert_eq!(verdict, ResponderBindVerdict::Bind);
    }

    // ---- Verdict serde + kind_str -----------------------------------------

    #[test]
    fn verdict_serializes_with_tagged_kind_field() {
        let v = IdentityGuardVerdict::TailnetChanged {
            bound_tailnet_id: "tn_old".to_owned(),
            bound_tailnet_display_name: None,
            current_tailnet_id: "tn_new".to_owned(),
            current_tailnet_display_name: None,
        };
        let json = serde_json::to_string(&v).expect("serialize");
        assert!(json.contains("\"kind\":\"tailnet_changed\""));
    }

    #[test]
    fn verdict_kind_str_matches_serde_tag_for_all_variants() {
        for v in [
            IdentityGuardVerdict::TailnetChanged {
                bound_tailnet_id: String::new(),
                bound_tailnet_display_name: None,
                current_tailnet_id: String::new(),
                current_tailnet_display_name: None,
            },
            IdentityGuardVerdict::NodeKeyChanged {
                bound_node_key: String::new(),
                current_node_key: String::new(),
            },
            IdentityGuardVerdict::TailnetRenamed {
                tailnet_id: String::new(),
                bound_display_name: None,
                current_display_name: None,
            },
            IdentityGuardVerdict::NoChange,
            IdentityGuardVerdict::NoBoundIdentity,
        ] {
            let json = serde_json::to_string(&v).expect("serialize");
            assert!(
                json.contains(&format!("\"kind\":\"{}\"", v.kind_str())),
                "kind_str ({}) did not match serde tag in JSON: {}",
                v.kind_str(),
                json
            );
        }
    }
}
