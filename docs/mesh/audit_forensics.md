# Mesh Audit Forensics Ledger

The mesh audit ledger records redaction-safe evidence for cross-peer behavior:
peer enrollment, share preview consent, policy decisions, export, import, body
fetch, withdrawal, quarantine, and revision events. Its purpose is forensic
explainability, not duplicating every local audit row.

Ledger rows must answer these questions without storing body text or secrets:

- What left this machine or arrived from a peer.
- Which peer, origin workspace, target workspace, and workspace scope were
  involved.
- Which policy decision allowed, denied, quarantined, or redacted the event.
- Which local rows or cached bodies were affected, summarized by stable refs or
  support-bundle-safe counts.
- Which previous event hash the row linked to for chain continuity.

Support bundle projections use `ee.mesh.audit_support_bundle_entry.v1`. They keep
event ids, action names, peer and workspace scope labels, row/body counts, and
event hashes, while excluding event details, raw local row refs, cached body refs,
and representative body text.

The focused SRR6.39 e2e driver is `scripts/e2e_mesh_audit_forensics.sh`. It emits
`ee.test_event.v1` schedule rows for the forensic scenario matrix and then runs
`cargo test --test mesh_audit_forensics` through RCH with `RCH_REQUIRE_REMOTE=1`.
The wrapper refuses to run Cargo locally.

Failure-mode coverage lives in:

- `tests/fixtures/failure_modes/mesh_audit_ledger_missing.json`
- `tests/fixtures/failure_modes/mesh_audit_ledger_corrupt.json`
