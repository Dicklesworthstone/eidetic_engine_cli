//! bd-1n0np.21.1 — What-If Memory Sandbox: read-only overlay evaluator.
//!
//! A [`SandboxOverlay`] is a set of proposed, NON-DURABLE memory changes
//! (upsert/remove) layered over a baseline memory set. Nothing here mutates the
//! store: `apply` returns a fresh overlaid view, and [`diff_overlay`] reports
//! baseline-vs-overlay changes. The `overlay_hash` pins determinism so the same
//! overlay always identifies the same hypothetical.
//!
//! The deterministic pack selection/omission/quality comparison ("run assembly
//! twice — baseline vs overlay") is the consumer (bd-1n0np.21.2/21.3): it builds
//! a baseline view and an overlaid view from this model and diffs the two pack
//! results. This module is pure and deterministic.

use std::collections::BTreeMap;

/// One proposed, non-durable change to a memory in a sandbox overlay.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SandboxChange {
    /// Add a new memory or modify an existing one to this content hash.
    Upsert { content_hash: String },
    /// Hypothetically tombstone the memory (drop it from the overlaid view).
    Remove,
}

/// A read-only overlay of proposed memory changes, keyed by memory id (at most
/// one change per memory). Deterministic by construction (sorted map).
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SandboxOverlay {
    changes: BTreeMap<String, SandboxChange>,
}

impl SandboxOverlay {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Propose adding/modifying `memory_id` to `content_hash`. Blank ids or
    /// hashes are ignored (defensive; the overlay carries no empty changes).
    pub fn upsert(&mut self, memory_id: &str, content_hash: &str) {
        let id = memory_id.trim();
        let hash = content_hash.trim();
        if id.is_empty() || hash.is_empty() {
            return;
        }
        self.changes.insert(
            id.to_string(),
            SandboxChange::Upsert {
                content_hash: hash.to_string(),
            },
        );
    }

    /// Propose hypothetically removing `memory_id`. Blank ids are ignored.
    pub fn remove(&mut self, memory_id: &str) {
        let id = memory_id.trim();
        if !id.is_empty() {
            self.changes.insert(id.to_string(), SandboxChange::Remove);
        }
    }

    #[must_use]
    pub fn changes(&self) -> &BTreeMap<String, SandboxChange> {
        &self.changes
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.changes.len()
    }

    /// Apply the overlay to a baseline `memory_id -> content_hash` view, returning
    /// a fresh overlaid view. Pure: the baseline is never mutated.
    #[must_use]
    pub fn apply(&self, baseline: &BTreeMap<String, String>) -> BTreeMap<String, String> {
        let mut overlaid = baseline.clone();
        for (id, change) in &self.changes {
            match change {
                SandboxChange::Upsert { content_hash } => {
                    overlaid.insert(id.clone(), content_hash.clone());
                }
                SandboxChange::Remove => {
                    overlaid.remove(id);
                }
            }
        }
        overlaid
    }

    /// Stable `blake3:`-prefixed hash of the canonical overlay (sorted changes),
    /// so the same set of proposed changes always identifies the same overlay
    /// regardless of insertion order.
    #[must_use]
    pub fn overlay_hash(&self) -> String {
        let mut canonical = String::from("ee.sandbox_overlay.v1");
        for (id, change) in &self.changes {
            match change {
                SandboxChange::Upsert { content_hash } => {
                    canonical.push_str(&format!("\u{0}upsert\u{0}{id}\u{0}{content_hash}"));
                }
                SandboxChange::Remove => {
                    canonical.push_str(&format!("\u{0}remove\u{0}{id}"));
                }
            }
        }
        format!("blake3:{}", blake3::hash(canonical.as_bytes()).to_hex())
    }
}

/// Baseline-vs-overlay diff report (bd-1n0np.21.1). Deterministic: each id list
/// is sorted, and `overlay_hash` ties the report to its overlay.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SandboxDiffReport {
    pub overlay_hash: String,
    pub added: Vec<String>,
    pub modified: Vec<String>,
    pub removed: Vec<String>,
    pub unchanged: usize,
}

/// Diff a baseline `memory_id -> content_hash` view against `overlay`. Pure and
/// deterministic; performs no durable mutation.
#[must_use]
pub fn diff_overlay(
    baseline: &BTreeMap<String, String>,
    overlay: &SandboxOverlay,
) -> SandboxDiffReport {
    let overlaid = overlay.apply(baseline);

    let removed: Vec<String> = baseline
        .keys()
        .filter(|id| !overlaid.contains_key(*id))
        .cloned()
        .collect();

    let mut added = Vec::new();
    let mut modified = Vec::new();
    let mut unchanged = 0_usize;
    for (id, hash) in &overlaid {
        match baseline.get(id) {
            None => added.push(id.clone()),
            Some(base_hash) if base_hash != hash => modified.push(id.clone()),
            Some(_) => unchanged += 1,
        }
    }

    SandboxDiffReport {
        overlay_hash: overlay.overlay_hash(),
        added,
        modified,
        removed,
        unchanged,
    }
}

// ---------------------------------------------------------------------------
// Sandbox session + diff surface (bd-1n0np.21.2/21.3): the `ee sandbox`
// remember/import/curate/diff CLI consumes these. A session is SCRATCH overlay
// state persisted under `<workspace>/.ee/sandbox/`, never the truth store — no
// durable memory mutation happens here.
// ---------------------------------------------------------------------------

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Schema id for the read-only sandbox diff surface.
pub const SANDBOX_DIFF_SCHEMA_V1: &str = "ee.sandbox.diff.v1";

/// A `blake3:`-prefixed content hash, used consistently for baseline memories and
/// proposed synthetic memories so the overlay diff classifies changes correctly.
#[must_use]
pub fn content_hash(content: &str) -> String {
    format!("blake3:{}", blake3::hash(content.as_bytes()).to_hex())
}

/// A synthetic, non-durable memory id for a sandbox-proposed `remember`/`import`.
#[must_use]
pub fn synthetic_memory_id(content: &str) -> String {
    let digest = blake3::hash(format!("sandbox:{content}").as_bytes()).to_hex();
    format!("sandbox_mem_{}", &digest[..20])
}

/// One proposed, non-durable change accumulated in a sandbox session.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SandboxProposal {
    /// Propose a new synthetic memory (`ee sandbox remember`). Carries the full
    /// content + level/kind so `ee sandbox apply` can promote it through the
    /// normal audited remember path.
    Remember {
        memory_id: String,
        content: String,
        content_hash: String,
        level: String,
        kind: String,
    },
    /// Propose importing a memory into the overlay (`ee sandbox import`).
    Import {
        memory_id: String,
        content: String,
        content_hash: String,
        level: String,
        kind: String,
    },
    /// Propose hypothetically retiring an existing memory (`ee sandbox curate --retire`).
    Retire { memory_id: String },
}

/// Accumulated scratch overlay state for one sandbox session.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct SandboxSession {
    #[serde(default)]
    pub proposals: Vec<SandboxProposal>,
}

impl SandboxSession {
    /// Scratch session file path (NOT the truth DB).
    #[must_use]
    pub fn session_path(workspace: &Path, name: &str) -> PathBuf {
        workspace
            .join(".ee")
            .join("sandbox")
            .join(format!("{name}.json"))
    }

    /// Load a session from its scratch file, or a fresh empty session if absent
    /// or unparseable (defensive: a corrupt scratch file never aborts a sandbox).
    #[must_use]
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default()
    }

    /// Persist the session to its scratch file (creating `.ee/sandbox/`).
    ///
    /// This writes only the non-durable overlay scratch, never a memory row.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let body = serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".to_owned());
        std::fs::write(path, body)
    }

    /// Build the [`SandboxOverlay`] this session's proposals describe.
    #[must_use]
    pub fn overlay(&self) -> SandboxOverlay {
        let mut overlay = SandboxOverlay::new();
        for proposal in &self.proposals {
            match proposal {
                SandboxProposal::Remember {
                    memory_id,
                    content_hash,
                    ..
                }
                | SandboxProposal::Import {
                    memory_id,
                    content_hash,
                    ..
                } => overlay.upsert(memory_id, content_hash),
                SandboxProposal::Retire { memory_id } => overlay.remove(memory_id),
            }
        }
        overlay
    }
}

/// The read-only sandbox diff surface (`ee.sandbox.diff.v1`): baseline-vs-overlay
/// changes plus the honesty markers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SandboxDiffSurface {
    pub schema: &'static str,
    pub overlay_hash: String,
    pub added: Vec<String>,
    pub modified: Vec<String>,
    pub removed: Vec<String>,
    pub unchanged: usize,
    pub proposal_count: usize,
    /// Always false: the sandbox performs NO durable memory mutation.
    pub durable_mutation: bool,
    /// Always true in this implementation: retrieval impact is shown as a
    /// baseline-vs-overlay change set over content hashes, NOT a live temporary
    /// index (bd-1n0np.21.2). The marker is never omitted when an approximation
    /// is used, so an agent never mistakes the change set for faithful retrieval.
    pub sandbox_approximation: bool,
    pub approximation_reason: &'static str,
}

/// Assemble the sandbox diff surface from a baseline `memory_id -> content` map
/// and a session's proposed overlay (bd-1n0np.21.2/21.3). Pure + deterministic;
/// performs no durable mutation. The caller supplies the baseline memories
/// (read-only), keeping this decoupled from the database.
#[must_use]
pub fn assemble_sandbox_diff(
    baseline_memories: &[(String, String)],
    session: &SandboxSession,
) -> SandboxDiffSurface {
    let mut baseline: BTreeMap<String, String> = BTreeMap::new();
    for (memory_id, content) in baseline_memories {
        baseline.insert(memory_id.clone(), content_hash(content));
    }
    let overlay = session.overlay();
    let report = diff_overlay(&baseline, &overlay);

    SandboxDiffSurface {
        schema: SANDBOX_DIFF_SCHEMA_V1,
        overlay_hash: report.overlay_hash,
        added: report.added,
        modified: report.modified,
        removed: report.removed,
        unchanged: report.unchanged,
        proposal_count: session.proposals.len(),
        durable_mutation: false,
        sandbox_approximation: true,
        approximation_reason: "Retrieval impact is shown as a baseline-vs-overlay change set over memory content hashes, not a live temporary index; new-memory search/pack ranking is approximated, not faithfully retrieved.",
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{
        SandboxOverlay, SandboxProposal, SandboxSession, assemble_sandbox_diff, content_hash,
        diff_overlay, synthetic_memory_id,
    };

    fn baseline() -> BTreeMap<String, String> {
        let mut base = BTreeMap::new();
        base.insert("mem_a".to_string(), "h_a".to_string());
        base.insert("mem_b".to_string(), "h_b".to_string());
        base
    }

    #[test]
    fn apply_overlays_without_mutating_baseline() {
        let base = baseline();
        let mut overlay = SandboxOverlay::new();
        overlay.upsert("mem_a", "h_a2"); // modify
        overlay.upsert("mem_c", "h_c"); // add
        overlay.remove("mem_b"); // remove

        let overlaid = overlay.apply(&base);
        assert_eq!(overlaid.get("mem_a").map(String::as_str), Some("h_a2"));
        assert_eq!(overlaid.get("mem_c").map(String::as_str), Some("h_c"));
        assert!(!overlaid.contains_key("mem_b"));
        // Baseline is untouched (read-only / no durable mutation).
        assert_eq!(base.get("mem_a").map(String::as_str), Some("h_a"));
        assert!(base.contains_key("mem_b"));
    }

    #[test]
    fn diff_classifies_added_modified_removed_unchanged() {
        let mut base = baseline();
        base.insert("mem_keep".to_string(), "h_keep".to_string());
        let mut overlay = SandboxOverlay::new();
        overlay.upsert("mem_a", "h_a2");
        overlay.upsert("mem_c", "h_c");
        overlay.remove("mem_b");

        let report = diff_overlay(&base, &overlay);
        assert_eq!(report.added, vec!["mem_c".to_string()]);
        assert_eq!(report.modified, vec!["mem_a".to_string()]);
        assert_eq!(report.removed, vec!["mem_b".to_string()]);
        assert_eq!(report.unchanged, 1); // mem_keep
        assert!(report.overlay_hash.starts_with("blake3:"));
    }

    #[test]
    fn overlay_hash_is_order_independent_and_change_sensitive() {
        let mut forward = SandboxOverlay::new();
        forward.upsert("mem_a", "h_a2");
        forward.remove("mem_b");

        let mut reversed = SandboxOverlay::new();
        reversed.remove("mem_b");
        reversed.upsert("mem_a", "h_a2");

        assert_eq!(
            forward.overlay_hash(),
            reversed.overlay_hash(),
            "overlay hash is independent of change insertion order"
        );

        let mut different = SandboxOverlay::new();
        different.upsert("mem_a", "h_a3");
        different.remove("mem_b");
        assert_ne!(forward.overlay_hash(), different.overlay_hash());
    }

    #[test]
    fn empty_overlay_is_a_no_op() {
        let base = baseline();
        let overlay = SandboxOverlay::new();
        assert!(overlay.is_empty());
        let report = diff_overlay(&base, &overlay);
        assert!(report.added.is_empty());
        assert!(report.modified.is_empty());
        assert!(report.removed.is_empty());
        assert_eq!(report.unchanged, base.len());
    }

    #[test]
    fn session_proposals_build_the_expected_overlay() {
        let session = SandboxSession {
            proposals: vec![
                SandboxProposal::Remember {
                    memory_id: synthetic_memory_id("a brand new fact"),
                    content: "a brand new fact".to_owned(),
                    content_hash: content_hash("a brand new fact"),
                    level: "episodic".to_owned(),
                    kind: "fact".to_owned(),
                },
                SandboxProposal::Retire {
                    memory_id: "mem_a".to_owned(),
                },
            ],
        };
        let overlay = session.overlay();
        assert_eq!(overlay.len(), 2, "one upsert + one remove");
    }

    #[test]
    fn session_round_trips_through_json() {
        let session = SandboxSession {
            proposals: vec![SandboxProposal::Import {
                memory_id: "mem_x".to_owned(),
                content: "imported".to_owned(),
                content_hash: content_hash("imported"),
                level: "episodic".to_owned(),
                kind: "fact".to_owned(),
            }],
        };
        let json = serde_json::to_string(&session).expect("serialize");
        let restored: SandboxSession = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(session, restored);
    }

    #[test]
    fn assemble_diff_classifies_proposals_and_marks_approximation() {
        // Baseline: two real memories.
        let baseline_memories = vec![
            ("mem_a".to_owned(), "alpha".to_owned()),
            ("mem_b".to_owned(), "beta".to_owned()),
        ];
        let new_id = synthetic_memory_id("gamma fact");
        let session = SandboxSession {
            proposals: vec![
                SandboxProposal::Remember {
                    memory_id: new_id.clone(),
                    content: "gamma fact".to_owned(),
                    content_hash: content_hash("gamma fact"),
                    level: "episodic".to_owned(),
                    kind: "fact".to_owned(),
                },
                SandboxProposal::Retire {
                    memory_id: "mem_b".to_owned(),
                },
            ],
        };

        let surface = assemble_sandbox_diff(&baseline_memories, &session);
        assert_eq!(surface.schema, super::SANDBOX_DIFF_SCHEMA_V1);
        assert_eq!(surface.added, vec![new_id]);
        assert_eq!(surface.removed, vec!["mem_b".to_owned()]);
        assert_eq!(surface.unchanged, 1); // mem_a
        assert_eq!(surface.proposal_count, 2);
        // No durable mutation; approximation honestly marked (bd-1n0np.21.2).
        assert!(!surface.durable_mutation);
        assert!(surface.sandbox_approximation);
        assert!(surface.overlay_hash.starts_with("blake3:"));
    }

    #[test]
    fn assemble_diff_is_deterministic() {
        let baseline_memories = vec![("mem_a".to_owned(), "alpha".to_owned())];
        let session = SandboxSession {
            proposals: vec![SandboxProposal::Remember {
                memory_id: synthetic_memory_id("new"),
                content: "new".to_owned(),
                content_hash: content_hash("new"),
                level: "episodic".to_owned(),
                kind: "fact".to_owned(),
            }],
        };
        let first = assemble_sandbox_diff(&baseline_memories, &session);
        let second = assemble_sandbox_diff(&baseline_memories, &session);
        assert_eq!(first, second, "sandbox diff is deterministic");
    }
}
