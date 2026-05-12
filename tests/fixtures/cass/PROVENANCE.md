# CASS Contract Fixtures

Pinned cass binary: `cass` crate_version `0.4.1`, api_version `1`, contract_version `1`.

Repository git ref when this fixture set was added: `3289a6d`.

Commands used to verify the live contract:

- `cass api-version --json`
- `cass capabilities --json`
- `cass introspect --json`
- `cass status --json`
- `cass search --robot`
- `cass view --json`
- `cass expand --json`
- `cass doctor --json`
- `cass health --json`

Fixture files:

- `api_version.v1.json` and `capabilities.v1.json` pin the original CASS version/capability parser fixtures used by unit tests.
- `health.v1.json` pins the `cass health --json` shape parsed by `ee::cass::CassHealth`.
- `robot_memory_v1.golden` pins the CASS search robot JSON consumed as memory evidence.
- `robot_audit_v1.golden` pins a normalized CASS status/audit readiness payload.
- `json_contract.golden` pins the EE JSONL memory, audit, tag, and workspace records derived from CASS evidence.
- `v1/api_version.json`, `v1/capabilities.json`, `v1/search_robot.json`, `v1/view.json`, `v1/expand.json`, `v1/sessions.json`, and `v1/doctor.json` pin the dedicated CASS conformance surfaces consumed by `tests/contracts/cass_robot.rs` and `tests/conformance/cass_contracts.rs`.

Regeneration:

Run `UPDATE_GOLDENS=1 cargo test conformance_cass_contracts` after confirming the installed cass binary still reports crate_version `0.4.1`, api_version `1`, and contract_version `1`.

Manual review required before committing regenerated goldens: inspect every diff under `tests/fixtures/cass/`, confirm no bare interactive CASS output was introduced, and update `tests/conformance/DISCREPANCIES.md` if the command surface changes.
