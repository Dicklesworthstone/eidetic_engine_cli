//! bd-1n0np.21.3 — `ee sandbox remember|import|curate|diff` What-If Sandbox CLI.
//!
//! A read-only overlay session over the live memory store: `remember`/`import`/
//! `curate` accumulate proposed, NON-DURABLE changes into a scratch session file
//! (`<workspace>/.ee/sandbox/<name>.json`, never the truth DB), and `diff` shows
//! the baseline-vs-overlay change set keyed by the deterministic overlay hash
//! (reusing the bd-1n0np.21.1 overlay evaluator). No durable memory mutation
//! happens here; the diff is explicitly marked `sandboxApproximation`
//! (bd-1n0np.21.2) so an agent never mistakes the change set for faithful
//! retrieval. Promotion to durable memory is the separate `ee remember`/`ee
//! curate`/`ee import` audited path.

use std::path::Path;

use clap::{Args, Subcommand};

use crate::core::memory::{RememberMemoryOptions, remember_memory};
use crate::core::sandbox::{
    SandboxDiffSurface, SandboxProposal, SandboxSession, assemble_sandbox_diff, content_hash,
    synthetic_memory_id,
};
use crate::db::DbConnection;
use crate::models::{DomainError, RESPONSE_SCHEMA_V2};

/// Subcommands for `ee sandbox` (no durable mutation).
#[derive(Clone, Debug, Eq, PartialEq, Subcommand)]
pub enum SandboxCommand {
    /// Propose a new synthetic memory in the overlay (no durable write).
    Remember(SandboxRememberArgs),
    /// Propose importing a memory into the overlay (no durable write).
    Import(SandboxImportArgs),
    /// Propose hypothetically retiring existing memories (no durable write).
    Curate(SandboxCurateArgs),
    /// Show the baseline-vs-overlay diff for the session (no durable write).
    Diff(SandboxDiffArgs),
    /// Promote the session's additive proposals to durable memory through the
    /// normal audited remember path (the only durable-write subcommand).
    Apply(SandboxApplyArgs),
}

/// Shared `--session` selector (defaults to `default`).
fn session_name(explicit: &Option<String>) -> Result<String, DomainError> {
    let name = explicit
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .unwrap_or("default")
        .to_owned();
    if is_safe_session_name(&name) {
        Ok(name)
    } else {
        Err(DomainError::Usage {
            message: "Invalid sandbox session name: session names must be plain file-safe names."
                .to_owned(),
            repair: Some(
                "Use only ASCII letters, digits, '.', '_' or '-' and do not pass path separators."
                    .to_owned(),
            ),
        })
    }
}

fn is_safe_session_name(name: &str) -> bool {
    !matches!(name, "." | "..")
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

#[derive(Clone, Debug, Eq, PartialEq, Args)]
pub struct SandboxRememberArgs {
    /// Proposed memory content.
    #[arg(value_name = "CONTENT")]
    pub content: String,
    /// Memory level (accepted for `ee remember` parity; the overlay is
    /// content-keyed, so it does not change the diff).
    #[arg(long, short = 'l', value_name = "LEVEL")]
    pub level: Option<String>,
    /// Memory kind (accepted for `ee remember` parity).
    #[arg(long, short = 'k', value_name = "KIND")]
    pub kind: Option<String>,
    /// Overlay session name.
    #[arg(long, value_name = "NAME")]
    pub session: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Args)]
pub struct SandboxImportArgs {
    /// Proposed imported memory content.
    #[arg(value_name = "CONTENT")]
    pub content: String,
    /// Memory level (accepted for `ee remember` parity).
    #[arg(long, short = 'l', value_name = "LEVEL")]
    pub level: Option<String>,
    /// Memory kind (accepted for `ee remember` parity).
    #[arg(long, short = 'k', value_name = "KIND")]
    pub kind: Option<String>,
    /// Overlay session name.
    #[arg(long, value_name = "NAME")]
    pub session: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Args)]
pub struct SandboxCurateArgs {
    /// Memory id(s) to hypothetically retire (repeatable).
    #[arg(long = "retire", value_name = "MEMORY_ID")]
    pub retire: Vec<String>,
    /// Overlay session name.
    #[arg(long, value_name = "NAME")]
    pub session: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Args)]
pub struct SandboxDiffArgs {
    /// Optional task/query context for the diff. Recorded for the agent; the
    /// change set is content-keyed, so retrieval impact stays a
    /// `sandboxApproximation` (bd-1n0np.21.2) rather than a live re-ranking.
    #[arg(value_name = "QUERY")]
    pub query: Option<String>,
    /// Overlay session name.
    #[arg(long, value_name = "NAME")]
    pub session: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Args)]
pub struct SandboxApplyArgs {
    /// Overlay session name.
    #[arg(long, value_name = "NAME")]
    pub session: Option<String>,
}

/// Outcome of a proposal command: the updated session + which session it is.
pub struct ProposeOutcome {
    pub session_name: String,
    pub session: SandboxSession,
    /// Non-fatal notes (e.g. the import injection-guard caveat).
    pub notes: Vec<String>,
}

fn load_session(workspace: &Path, name: &str) -> SandboxSession {
    SandboxSession::load(&SandboxSession::session_path(workspace, name))
}

fn save_session(workspace: &Path, name: &str, session: &SandboxSession) -> Result<(), DomainError> {
    session
        .save(&SandboxSession::session_path(workspace, name))
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to persist sandbox session `{name}`: {error}"),
            repair: Some("Check that <workspace>/.ee/sandbox is writable.".to_owned()),
        })
}

/// `ee sandbox remember` — append a synthetic-memory proposal (no durable write).
pub fn propose_remember(
    workspace: &Path,
    args: &SandboxRememberArgs,
) -> Result<ProposeOutcome, DomainError> {
    let name = session_name(&args.session)?;
    let mut session = load_session(workspace, &name);
    session.proposals.push(SandboxProposal::Remember {
        memory_id: synthetic_memory_id(&args.content),
        content: args.content.clone(),
        content_hash: content_hash(&args.content),
        level: args.level.clone().unwrap_or_else(default_level),
        kind: args.kind.clone().unwrap_or_else(default_kind),
    });
    save_session(workspace, &name, &session)?;
    Ok(ProposeOutcome {
        session_name: name,
        session,
        notes: Vec::new(),
    })
}

fn default_level() -> String {
    "episodic".to_owned()
}

fn default_kind() -> String {
    "fact".to_owned()
}

/// `ee sandbox import` — append an import proposal (no durable write).
pub fn propose_import(
    workspace: &Path,
    args: &SandboxImportArgs,
) -> Result<ProposeOutcome, DomainError> {
    let name = session_name(&args.session)?;
    let mut session = load_session(workspace, &name);
    session.proposals.push(SandboxProposal::Import {
        memory_id: synthetic_memory_id(&args.content),
        content: args.content.clone(),
        content_hash: content_hash(&args.content),
        level: args.level.clone().unwrap_or_else(default_level),
        kind: args.kind.clone().unwrap_or_else(default_kind),
    });
    save_session(workspace, &name, &session)?;
    // bd-1n0np.21.3: imports must pass the injection guard (bd-1n0np.23.3) before
    // entering the overlay. That guard is not yet wired into the sandbox path, so
    // the proposal is recorded with a visible caveat rather than a silent pass.
    Ok(ProposeOutcome {
        session_name: name,
        session,
        notes: vec![
            "import injection guard (bd-1n0np.23.3) is not yet wired into the sandbox path; review imported content before promotion".to_owned(),
        ],
    })
}

/// `ee sandbox curate --retire` — append retire proposals (no durable write).
pub fn propose_curate(
    workspace: &Path,
    args: &SandboxCurateArgs,
) -> Result<ProposeOutcome, DomainError> {
    if args.retire.is_empty() {
        return Err(DomainError::Usage {
            message: "ee sandbox curate requires at least one --retire <MEMORY_ID>.".to_owned(),
            repair: Some("Pass --retire <id> (repeatable) to propose retirements.".to_owned()),
        });
    }
    let name = session_name(&args.session)?;
    let mut session = load_session(workspace, &name);
    let mut added_retire_proposals = 0_usize;
    for memory_id in &args.retire {
        let trimmed = memory_id.trim();
        if !trimmed.is_empty() {
            session.proposals.push(SandboxProposal::Retire {
                memory_id: trimmed.to_owned(),
            });
            added_retire_proposals += 1;
        }
    }
    if added_retire_proposals == 0 {
        return Err(DomainError::Usage {
            message: "ee sandbox curate requires at least one non-empty --retire <MEMORY_ID>."
                .to_owned(),
            repair: Some("Pass --retire <id> (repeatable) to propose retirements.".to_owned()),
        });
    }
    save_session(workspace, &name, &session)?;
    Ok(ProposeOutcome {
        session_name: name,
        session,
        notes: Vec::new(),
    })
}

/// `ee sandbox diff` — assemble the baseline-vs-overlay diff surface (read-only).
pub fn build_diff(
    workspace: &Path,
    args: &SandboxDiffArgs,
) -> Result<(String, SandboxDiffSurface), DomainError> {
    let name = session_name(&args.session)?;
    let session = load_session(workspace, &name);
    let baseline = baseline_memories(workspace)?;
    Ok((name, assemble_sandbox_diff(&baseline, &session)))
}

/// Read the baseline `(memory_id, content)` pairs from the workspace, read-only.
fn baseline_memories(workspace: &Path) -> Result<Vec<(String, String)>, DomainError> {
    let database_path = workspace.join(".ee").join("ee.db");
    if !database_path.exists() {
        return Ok(Vec::new());
    }
    let connection =
        DbConnection::open_file(&database_path).map_err(|error| DomainError::Storage {
            message: format!("Failed to open workspace database: {error}"),
            repair: Some("Run `ee doctor --workspace . --json`.".to_owned()),
        })?;
    let canonical = workspace
        .canonicalize()
        .unwrap_or_else(|_| workspace.to_path_buf());
    let workspace_id = crate::core::workspace::bound_workspace_id_or_hash(
        &connection,
        &crate::core::workspace::stable_workspace_id(&canonical),
        &[workspace, canonical.as_path()],
    )?;
    let memories = connection
        .list_memories(&workspace_id, None, false)
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to list workspace memories: {error}"),
            repair: Some("Run `ee doctor --workspace . --json`.".to_owned()),
        })?;
    Ok(memories
        .into_iter()
        .map(|memory| (memory.id, memory.content))
        .collect())
}

/// Outcome of `ee sandbox apply`: which proposals were persisted vs deferred.
pub struct ApplyOutcome {
    pub session_name: String,
    /// Memory ids persisted through the normal audited remember path.
    pub persisted: Vec<String>,
    /// Retire proposals that require explicit `ee curate` promotion (apply does
    /// not auto-tombstone existing memories).
    pub retire_pending: Vec<String>,
    pub notes: Vec<String>,
}

/// `ee sandbox apply` — promote the session's additive proposals to durable
/// memory through the NORMAL audited remember path (bd-1n0np.21.3). Retire
/// proposals are reported for explicit `ee curate` promotion, never silently
/// auto-applied. Applied additive proposals are cleared from the session.
pub fn apply_session(
    workspace: &Path,
    args: &SandboxApplyArgs,
) -> Result<ApplyOutcome, DomainError> {
    let name = session_name(&args.session)?;
    let session = load_session(workspace, &name);

    let mut persisted = Vec::new();
    let mut retire_pending = Vec::new();
    let mut applied_additive_count = 0_usize;
    for proposal in &session.proposals {
        match proposal {
            SandboxProposal::Remember {
                content,
                level,
                kind,
                ..
            }
            | SandboxProposal::Import {
                content,
                level,
                kind,
                ..
            } => {
                let options = RememberMemoryOptions {
                    workspace_path: workspace,
                    database_path: None,
                    content,
                    workflow_id: None,
                    level,
                    kind,
                    tags: None,
                    confidence: 0.8,
                    source: None,
                    allow_secret_mention: false,
                    valid_from: None,
                    valid_to: None,
                    dry_run: false,
                    auto_link: false,
                    propose_candidates: false,
                };
                let report = match remember_memory(&options) {
                    Ok(report) => report,
                    Err(error) => {
                        return Err(record_partial_apply_failure(
                            workspace,
                            &name,
                            &session,
                            applied_additive_count,
                            error,
                        ));
                    }
                };
                persisted.push(report.memory_id.to_string());
                applied_additive_count += 1;
            }
            SandboxProposal::Retire { memory_id } => retire_pending.push(memory_id.clone()),
        }
    }

    let mut notes = Vec::new();
    if !retire_pending.is_empty() {
        notes.push(format!(
            "{} retire proposal(s) require explicit `ee curate` promotion; sandbox apply persists additive proposals through the audited remember path and never auto-tombstones existing memories",
            retire_pending.len()
        ));
    }

    // Clear the applied additive proposals; keep un-promoted retires.
    let remaining = remaining_after_applied_additives(&session, applied_additive_count);
    save_session(workspace, &name, &remaining)?;

    Ok(ApplyOutcome {
        session_name: name,
        persisted,
        retire_pending,
        notes,
    })
}

fn record_partial_apply_failure(
    workspace: &Path,
    name: &str,
    session: &SandboxSession,
    applied_additive_count: usize,
    apply_error: DomainError,
) -> DomainError {
    if applied_additive_count == 0 {
        return apply_error;
    }

    let remaining = remaining_after_applied_additives(session, applied_additive_count);
    match save_session(workspace, name, &remaining) {
        Ok(()) => DomainError::Storage {
            message: format!(
                "Sandbox apply persisted {applied_additive_count} additive proposal(s), then stopped before completing: {apply_error}. The scratch session was updated to keep only unapplied proposals."
            ),
            repair: Some(format!(
                "Review `ee sandbox diff --session {name}` and rerun after fixing the remaining proposal."
            )),
        },
        Err(cleanup_error) => DomainError::Storage {
            message: format!(
                "Sandbox apply failed after {applied_additive_count} additive proposal(s) persisted; also failed to update the scratch session: {cleanup_error}. Original apply error: {apply_error}"
            ),
            repair: Some(
                "Inspect <workspace>/.ee/sandbox and avoid retrying the same session until the persisted proposals are reconciled."
                    .to_owned(),
            ),
        },
    }
}

fn remaining_after_applied_additives(
    session: &SandboxSession,
    applied_additive_count: usize,
) -> SandboxSession {
    let mut remaining = SandboxSession::default();
    let mut seen_additives = 0_usize;
    for proposal in &session.proposals {
        match proposal {
            SandboxProposal::Remember { .. } | SandboxProposal::Import { .. } => {
                if seen_additives >= applied_additive_count {
                    remaining.proposals.push(proposal.clone());
                }
                seen_additives += 1;
            }
            SandboxProposal::Retire { .. } => remaining.proposals.push(proposal.clone()),
        }
    }
    remaining
}

/// Render an `ee.response.v2` envelope for an apply outcome.
#[must_use]
pub fn render_apply_json(outcome: &ApplyOutcome) -> String {
    serde_json::json!({
        "schema": RESPONSE_SCHEMA_V2,
        "success": true,
        "data": {
            "command": "sandbox apply",
            "sessionName": outcome.session_name,
            "persistedMemoryIds": outcome.persisted,
            "persistedCount": outcome.persisted.len(),
            "retireProposalsPending": outcome.retire_pending,
            "notes": outcome.notes,
        },
        "degraded": [],
    })
    .to_string()
}

/// Compact human summary for an apply outcome.
#[must_use]
pub fn render_apply_human(outcome: &ApplyOutcome) -> String {
    let mut out = format!(
        "sandbox apply (session `{}`): persisted {} memory(ies) via the audited remember path\n",
        outcome.session_name,
        outcome.persisted.len(),
    );
    for id in &outcome.persisted {
        out.push_str(&format!("  + {id}\n"));
    }
    for note in &outcome.notes {
        out.push_str(&format!("  note: {note}\n"));
    }
    out
}

/// Render an `ee.response.v2` envelope for a proposal outcome.
#[must_use]
pub fn render_propose_json(command: &str, outcome: &ProposeOutcome) -> String {
    serde_json::json!({
        "schema": RESPONSE_SCHEMA_V2,
        "success": true,
        "data": {
            "command": command,
            "sessionName": outcome.session_name,
            "overlayHash": outcome.session.overlay().overlay_hash(),
            "proposalCount": outcome.session.proposals.len(),
            "durableMutation": false,
            "notes": outcome.notes,
        },
        "degraded": [],
    })
    .to_string()
}

/// Render an `ee.response.v2` envelope wrapping the `ee.sandbox.diff.v1` surface.
#[must_use]
pub fn render_diff_json(session_name: &str, surface: &SandboxDiffSurface) -> String {
    serde_json::json!({
        "schema": RESPONSE_SCHEMA_V2,
        "success": true,
        "data": {
            "sessionName": session_name,
            "diff": surface,
        },
        "degraded": [],
    })
    .to_string()
}

/// Compact human summary for a proposal outcome.
#[must_use]
pub fn render_propose_human(command: &str, outcome: &ProposeOutcome) -> String {
    let mut out = format!(
        "sandbox {command}: session `{}` now has {} proposal(s) (no durable write)\n  overlay hash: {}\n",
        outcome.session_name,
        outcome.session.proposals.len(),
        outcome.session.overlay().overlay_hash(),
    );
    for note in &outcome.notes {
        out.push_str(&format!("  note: {note}\n"));
    }
    out
}

/// Compact human summary for the diff surface.
#[must_use]
pub fn render_diff_human(session_name: &str, surface: &SandboxDiffSurface) -> String {
    let mut out = format!(
        "sandbox diff (session `{session_name}`, no durable write)\n  overlay hash: {}\n  added: {}  modified: {}  removed: {}  unchanged: {}\n",
        surface.overlay_hash,
        surface.added.len(),
        surface.modified.len(),
        surface.removed.len(),
        surface.unchanged,
    );
    if surface.sandbox_approximation {
        out.push_str(&format!(
            "  approximation: {}\n",
            surface.approximation_reason
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{
        ApplyOutcome, ProposeOutcome, SandboxCurateArgs, SandboxProposal, SandboxSession,
        load_session, propose_curate, remaining_after_applied_additives, render_apply_json,
        render_diff_json, render_propose_json, session_name,
    };
    use crate::core::sandbox::{SANDBOX_DIFF_SCHEMA_V1, SandboxDiffSurface};

    #[test]
    fn session_name_defaults_and_trims() {
        assert_eq!(session_name(&None).expect("default"), "default");
        assert_eq!(
            session_name(&Some("  ".to_owned())).expect("blank defaults"),
            "default"
        );
        assert_eq!(
            session_name(&Some(" feature-x ".to_owned())).expect("trimmed"),
            "feature-x"
        );
    }

    #[test]
    fn session_name_rejects_path_traversal() {
        for raw in [
            ".",
            "..",
            "../escape",
            "nested/name",
            "/absolute",
            r"nested\name",
            "feature name",
            "feature:name",
            "feature\nname",
            "feature-\u{2603}",
        ] {
            let error = session_name(&Some(raw.to_owned())).expect_err(raw);
            assert!(
                error.to_string().contains("Invalid sandbox session name"),
                "unexpected error for {raw:?}: {error}"
            );
        }
    }

    #[test]
    fn session_name_accepts_file_safe_names() {
        for raw in [
            "default",
            "feature-x",
            "feature_x",
            "feature.2026",
            "A1-b_2.c",
        ] {
            assert_eq!(session_name(&Some(raw.to_owned())).expect(raw), raw);
        }
    }

    #[test]
    fn sandbox_curate_rejects_blank_retire_values() {
        let workspace = tempfile::tempdir().expect("tempdir");
        let args = SandboxCurateArgs {
            retire: vec![" ".to_owned(), "\t".to_owned()],
            session: Some("blank-retire".to_owned()),
        };

        let error = match propose_curate(workspace.path(), &args) {
            Ok(_) => panic!("blank retires must fail"),
            Err(error) => error,
        };
        assert!(
            error
                .to_string()
                .contains("at least one non-empty --retire"),
            "unexpected error: {error}"
        );

        let session_path =
            crate::core::sandbox::SandboxSession::session_path(workspace.path(), "blank-retire");
        assert!(
            !session_path.exists(),
            "invalid curate invocation must not create a scratch session"
        );
    }

    #[test]
    fn sandbox_curate_trims_and_records_non_empty_retire_values() {
        let workspace = tempfile::tempdir().expect("tempdir");
        let args = SandboxCurateArgs {
            retire: vec!["  mem_a  ".to_owned(), "".to_owned(), "mem_b".to_owned()],
            session: Some("trim-retire".to_owned()),
        };

        let outcome = propose_curate(workspace.path(), &args).expect("valid retires");
        assert_eq!(outcome.session.proposals.len(), 2);
        assert_eq!(
            outcome.session.proposals,
            vec![
                SandboxProposal::Retire {
                    memory_id: "mem_a".to_owned()
                },
                SandboxProposal::Retire {
                    memory_id: "mem_b".to_owned()
                },
            ]
        );

        let stored = load_session(workspace.path(), "trim-retire");
        assert_eq!(stored.proposals, outcome.session.proposals);
    }

    #[test]
    fn sandbox_apply_remaining_session_drops_only_confirmed_additives() {
        let first = SandboxProposal::Remember {
            memory_id: "sandbox_mem_first".to_owned(),
            content: "first".to_owned(),
            content_hash: "blake3:first".to_owned(),
            level: "episodic".to_owned(),
            kind: "fact".to_owned(),
        };
        let retire = SandboxProposal::Retire {
            memory_id: "mem_existing".to_owned(),
        };
        let second = SandboxProposal::Import {
            memory_id: "sandbox_mem_second".to_owned(),
            content: "second".to_owned(),
            content_hash: "blake3:second".to_owned(),
            level: "episodic".to_owned(),
            kind: "fact".to_owned(),
        };
        let third = SandboxProposal::Remember {
            memory_id: "sandbox_mem_third".to_owned(),
            content: "third".to_owned(),
            content_hash: "blake3:third".to_owned(),
            level: "episodic".to_owned(),
            kind: "fact".to_owned(),
        };
        let session = SandboxSession {
            proposals: vec![first.clone(), retire.clone(), second.clone(), third.clone()],
        };

        assert_eq!(
            remaining_after_applied_additives(&session, 0).proposals,
            session.proposals
        );
        assert_eq!(
            remaining_after_applied_additives(&session, 2).proposals,
            vec![retire.clone(), third.clone()]
        );
        assert_eq!(
            remaining_after_applied_additives(&session, usize::MAX).proposals,
            vec![retire]
        );
    }

    #[test]
    fn sandbox_success_json_envelopes_include_clean_degraded_array() {
        let session = SandboxSession {
            proposals: vec![SandboxProposal::Remember {
                memory_id: "sandbox_mem_first".to_owned(),
                content: "first".to_owned(),
                content_hash: "blake3:first".to_owned(),
                level: "episodic".to_owned(),
                kind: "fact".to_owned(),
            }],
        };
        let propose = ProposeOutcome {
            session_name: "default".to_owned(),
            session,
            notes: Vec::new(),
        };
        let apply = ApplyOutcome {
            session_name: "default".to_owned(),
            persisted: vec!["mem_a".to_owned()],
            retire_pending: Vec::new(),
            notes: Vec::new(),
        };
        let diff = SandboxDiffSurface {
            schema: SANDBOX_DIFF_SCHEMA_V1,
            overlay_hash: "blake3:overlay".to_owned(),
            added: vec!["sandbox_mem_first".to_owned()],
            modified: Vec::new(),
            removed: Vec::new(),
            unchanged: 0,
            proposal_count: 1,
            durable_mutation: false,
            sandbox_approximation: true,
            approximation_reason: "test approximation",
        };

        for raw in [
            render_propose_json("sandbox remember", &propose),
            render_apply_json(&apply),
            render_diff_json("default", &diff),
        ] {
            let envelope: serde_json::Value =
                serde_json::from_str(&raw).expect("sandbox success envelope");
            assert_eq!(envelope["schema"], crate::models::RESPONSE_SCHEMA_V2);
            assert_eq!(envelope["success"], true);
            assert_eq!(envelope["degraded"], serde_json::json!([]));
        }
    }
}
