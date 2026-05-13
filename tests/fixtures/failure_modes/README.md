# Failure-mode fixture catalog

Catalog of degraded-emission fixtures. Each `*.json` documents one
`degraded[]` code with its surface, severity, trigger scenario, and
expected emission shape.

See [`SCHEMA.md`](./SCHEMA.md) for the fixture format. The contract test
at [`tests/contracts/failure_mode_fixtures.rs`](../../contracts/failure_mode_fixtures.rs)
walks this directory and asserts every fixture is well-formed and that
every documented `code` corresponds to a real string in `src/`.

## Seed catalog (J6 / bd-17c65.10.6)

These are the highest-emission codes the system raises today. Per-epic
PRs that introduce new codes are expected to land their own fixture here
in the same commit, keeping the catalog complete by construction.

| Code | Surface | Severity | Bead |
|---|---|---|---|
| `no_relevant_results` | search | medium | bd-17c65.2.1 (B1) |
| `weak_query_recall` | search | low | bd-17c65.2.5 (B5) |
| `low_recall_after_floor` | search | info | bd-17c65.2.1 (B1) |
| `duplicates_collapsed` | search | low | bd-17c65.2.3 (B3) |
| `index_stale` | search, context | high | bd-17c65.2.1 (B1) |
| `index_missing` | search, context | medium | bd-17c65.2.1 (B1) |
| `index_corrupt` | search, context | high | bd-17c65.2.1 (B1) |
| `tombstoned_in_results` | search | info | bd-17c65.2.8 (B8) |
| `expired_filtered` | search | info | bd-17c65.2.8 (B8) |
| `profile_search_limit_capped` | search, diag search | info | bd-17c65.2.4 (B7) |
| `context_evidence_freshness_changed_source` | context, pack replay | info | bd-17c65.1.2 (A2) |

## Adding a fixture

1. Implement the new degraded emission in `src/`.
2. Drop a fixture here named `<code>.json` matching the schema.
3. `cargo test --test contracts failure_mode_fixtures_validate_catalog`
   to confirm structural validity + cross-reference against `src/`.
4. Add a row to the table above.

## What this catalog is NOT

* It is not a replacement for end-to-end exercise of each degraded
  emission. Per-epic e2e drivers under
  `scripts/e2e_overhaul/` (search_honesty.sh, etc.) run the real
  binary and assert each code fires when expected. The catalog is the
  static reference; the e2e drivers are the executable proof.
* It is not authoritative for message text. Production messages embed
  runtime values (floor, query, counts). Fixtures use
  `message_contains` substrings to stay robust under templating.

Bead: `bd-17c65.10.6` (J6).
