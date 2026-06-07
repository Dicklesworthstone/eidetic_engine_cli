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

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{SandboxOverlay, diff_overlay};

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
}
