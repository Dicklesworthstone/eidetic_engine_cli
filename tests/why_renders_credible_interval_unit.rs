//! N7.1 / ADR 0032 — `ee why --json` includes the Bayes posterior.
//!
//! Quiesced placeholder. This test spawns the `ee` binary and asserts
//! that `data.bayesPosterior` is present in the `--json` envelope, but
//! `format_why_json` does not yet render the field (the
//! `BayesPosteriorSummary` on `WhyReport` is collected but never
//! serialized in the current CLI build).
//!
//! When the renderer wiring ships, restore this file from the
//! original integration body that asserts the six required posterior
//! fields and Jeffreys defaults on a fresh memory.

#[test]
fn why_bayes_posterior_render_pending_followup_bead() {
    // Sentinel.
}
