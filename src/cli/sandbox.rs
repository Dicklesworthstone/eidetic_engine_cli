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
fn session_name(explicit: &Option<String>) -> String {
    explicit
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .unwrap_or("default")
        .to_owned()
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
    let name = session_name(&args.session);
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
    let name = session_name(&args.session);
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
    let name = session_name(&args.session);
    let mut session = load_session(workspace, &name);
    for memory_id in &args.retire {
        let trimmed = memory_id.trim();
        if !trimmed.is_empty() {
            session.proposals.push(SandboxProposal::Retire {
                memory_id: trimmed.to_owned(),
            });
        }
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
    let name = session_name(&args.session);
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
    let workspace_row = connection
        .get_workspace_by_path(workspace.to_string_lossy().as_ref())
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to query workspace row: {error}"),
            repair: Some("Run `ee doctor --workspace . --json`.".to_owned()),
        })?;
    let Some(workspace_row) = workspace_row else {
        return Ok(Vec::new());
    };
    let memories = connection
        .list_memories(&workspace_row.id, None, false)
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
    let name = session_name(&args.session);
    let session = load_session(workspace, &name);

    let mut persisted = Vec::new();
    let mut retire_pending = Vec::new();
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
                let report = remember_memory(&options)?;
                persisted.push(report.memory_id.to_string());
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
    let mut remaining = SandboxSession::default();
    for proposal in &session.proposals {
        if matches!(proposal, SandboxProposal::Retire { .. }) {
            remaining.proposals.push(proposal.clone());
        }
    }
    save_session(workspace, &name, &remaining)?;

    Ok(ApplyOutcome {
        session_name: name,
        persisted,
        retire_pending,
        notes,
    })
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
    use super::session_name;

    #[test]
    fn session_name_defaults_and_trims() {
        assert_eq!(session_name(&None), "default");
        assert_eq!(session_name(&Some("  ".to_owned())), "default");
        assert_eq!(session_name(&Some(" feature-x ".to_owned())), "feature-x");
    }
}
