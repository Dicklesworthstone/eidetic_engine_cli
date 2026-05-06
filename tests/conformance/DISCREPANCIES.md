# CASS Contract Discrepancies

## cass 0.4.1 command names

The ea80 bead requests coverage for `memory`, `audit`, `tags`, and `workspace` robot/JSON surfaces. In the installed cass 0.4.1 contract, those are not top-level cass commands. `cass introspect --json` exposes the consumed top-level surfaces as `search`, `status`, `sessions`, `view`, `expand`, `capabilities`, `api-version`, and `introspect`.

This harness maps the requested surfaces to the current consumed contracts:

- `memory`: `robot_memory_v1.golden`, backed by the CASS `search --robot --robot-meta` fixture that ee parses into memory evidence.
- `audit`: `robot_audit_v1.golden`, a normalized `cass status --json` readiness/audit payload.
- `tags`: `json_contract.golden`, using `ee.export.tag.v1`.
- `workspace`: `json_contract.golden`, using `ee.export.workspace.v1`.

If a future cass release adds top-level `memory`, `audit`, `tags`, or `workspace` commands, this discrepancy should be removed and the harness should pin those direct outputs.

## JSONL non-dry-run persistence

The conformance harness currently asserts the `ee import jsonl --dry-run` contract for the CASS-derived JSONL fixture. A non-dry-run persistence assertion is blocked in this shared dirty checkout by an unrelated DB constraint drift: the audit table currently checks `id GLOB 'audit_*' AND length(id) = 32`, while `generate_audit_id()` returns `audit_` plus a 32-hex UUID. `src/db/mod.rs` is reserved by another agent, so this harness does not edit that storage surface. Once that constraint is reconciled, the dry-run test should be extended to assert persisted memory fields and duplicate idempotency.
