//! bd-1n0np.21.4 — What-If Sandbox overlay-evaluator contract tests.
//!
//! Pins the *public* contracts of the 21.1 sandbox overlay evaluator
//! (`core::sandbox`) from a consumer's view: a sandbox is a READ-ONLY hypothesis
//! over a baseline memory set — it must never mutate the baseline (no durable
//! writes), its diff must be deterministic + correctly classified, and its
//! `overlay_hash` must be order-independent yet change-sensitive (so the same
//! hypothetical always identifies the same scratch namespace, 21.2).
//!
//! Goldens for the `SandboxDiffReport` JSON body are RCH-remote-regen only and
//! owed there (bd-17c65.10.17). The CLI-routing + temp-index assertions
//! (sandbox_approximation marker / faithful retrieval) live in the 21.5 e2e once
//! the 21.2/21.3 surfaces land.

use std::collections::BTreeMap;

use ee::core::sandbox::{SandboxOverlay, diff_overlay};

fn baseline() -> BTreeMap<String, String> {
    let mut base = BTreeMap::new();
    base.insert("mem_a".to_string(), "hash_a".to_string());
    base.insert("mem_b".to_string(), "hash_b".to_string());
    base
}

#[test]
fn overlay_is_read_only_never_mutates_the_baseline() {
    let base = baseline();
    let snapshot = base.clone();
    let mut overlay = SandboxOverlay::new();
    overlay.upsert("mem_a", "hash_a2"); // modify
    overlay.upsert("mem_c", "hash_c"); // add
    overlay.remove("mem_b"); // remove

    let overlaid = overlay.apply(&base);
    // The baseline is untouched — the core no-durable-write guarantee.
    assert_eq!(base, snapshot, "apply() must not mutate the baseline");
    // The overlaid view reflects all three changes.
    assert_eq!(overlaid.get("mem_a").map(String::as_str), Some("hash_a2"));
    assert_eq!(overlaid.get("mem_c").map(String::as_str), Some("hash_c"));
    assert!(!overlaid.contains_key("mem_b"));
}

#[test]
fn diff_classifies_added_modified_removed_unchanged_deterministically() {
    let mut base = baseline();
    base.insert("mem_keep".to_string(), "hash_keep".to_string());
    let mut overlay = SandboxOverlay::new();
    overlay.upsert("mem_a", "hash_a2"); // modify
    overlay.upsert("mem_c", "hash_c"); // add
    overlay.remove("mem_b"); // remove

    let report = diff_overlay(&base, &overlay);
    assert_eq!(report.added, vec!["mem_c".to_string()]);
    assert_eq!(report.modified, vec!["mem_a".to_string()]);
    assert_eq!(report.removed, vec!["mem_b".to_string()]);
    assert_eq!(report.unchanged, 1); // mem_keep
    assert!(report.overlay_hash.starts_with("blake3:"));

    // Deterministic: re-diffing the same baseline+overlay yields an equal report.
    assert_eq!(report, diff_overlay(&base, &overlay));
}

#[test]
fn overlay_hash_is_order_independent_but_change_sensitive() {
    let mut forward = SandboxOverlay::new();
    forward.upsert("mem_a", "hash_a2");
    forward.remove("mem_b");

    let mut reversed = SandboxOverlay::new();
    reversed.remove("mem_b");
    reversed.upsert("mem_a", "hash_a2");
    assert_eq!(
        forward.overlay_hash(),
        reversed.overlay_hash(),
        "overlay hash must be independent of change insertion order (stable scratch key)"
    );

    let mut different = SandboxOverlay::new();
    different.upsert("mem_a", "hash_a3"); // different content
    different.remove("mem_b");
    assert_ne!(
        forward.overlay_hash(),
        different.overlay_hash(),
        "a different proposed change must yield a different overlay hash"
    );
}

#[test]
fn empty_overlay_is_a_faithful_no_op() {
    let base = baseline();
    let overlay = SandboxOverlay::new();
    assert!(overlay.is_empty());
    let report = diff_overlay(&base, &overlay);
    assert!(report.added.is_empty());
    assert!(report.modified.is_empty());
    assert!(report.removed.is_empty());
    assert_eq!(report.unchanged, base.len());
}
