# CASS Contract Fixtures

Pinned cass binary: `cass` crate_version `0.4.1`, api_version `1`, contract_version `1`.

Repository git ref when this fixture set was added: `3289a6d`.

Commands used to verify the live contract:

- `cass api-version --json`
- `cass capabilities --json`
- `cass introspect --json`
- `cass status --json`

Fixture files:

- `robot_memory_v1.golden` pins the CASS search robot JSON consumed as memory evidence.
- `robot_audit_v1.golden` pins a normalized CASS status/audit readiness payload.
- `json_contract.golden` pins the EE JSONL memory, audit, tag, and workspace records derived from CASS evidence.

Regeneration:

Run `UPDATE_GOLDENS=1 cargo test conformance_cass_contracts` after confirming the installed cass binary still reports crate_version `0.4.1`, api_version `1`, and contract_version `1`.

Manual review required before committing regenerated goldens: inspect every diff under `tests/fixtures/cass/`, confirm no bare interactive CASS output was introduced, and update `tests/conformance/DISCREPANCIES.md` if the command surface changes.
