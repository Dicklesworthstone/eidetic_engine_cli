# CASS JSON Contract Stability Spike

Bead: `eidetic_engine_cli-851n` / EE-283

## Recommendation

Go, with a narrow subprocess adapter and contract gates before any user-visible CASS import command ships.

`ee` should consume CASS through robot/JSON CLI surfaces, not through the CASS SQLite database or internal Rust modules. The current CASS installation exposes explicit discovery commands, a contract version, response schemas for core robot surfaces, stdout/stderr separation rules, and golden/metamorphic tests that make the contract stable enough for an `ee` adapter.

The adapter should not assume every CASS command has a complete `introspect.response_schemas` entry. It should start with the surfaces that are already schema-backed or easy to fixture: `api-version`, `capabilities`, `introspect`, `health`, `status`/`state`, `search --robot`, `sessions --json`, and `view --json`. `expand` and `timeline` can be supported after `ee` owns fixtures for their observed payloads.

## Evidence Inspected

- `/dp/coding_agent_session_search/README.md`
- `/dp/coding_agent_session_search/docs/ROBOT_MODE.md`
- `/dp/coding_agent_session_search/src/lib.rs`
- `/dp/coding_agent_session_search/tests/golden/PROVENANCE.md`
- `/dp/coding_agent_session_search/tests/golden/robot/*.golden`
- `/dp/coding_agent_session_search/tests/golden/robot_docs/*.golden`
- `/dp/coding_agent_session_search/tests/golden_robot_json.rs`
- `/dp/coding_agent_session_search/tests/metamorphic_introspect_schema.rs`

Non-mutating probes run from this repo:

```bash
cass api-version --json
cass capabilities --json
cass introspect --json
cass health --json
cass search "format before release" --robot --limit 1 --fields minimal --robot-meta --request-id ee-283-stream-probe
cass sessions --current --json
cass robot-docs contracts
```

No CASS index rebuild, model install, or Cargo build was run for this spike.

## Observed Contract Surface

The installed CASS binary reports:

- `crate_version`: `0.3.0`
- `api_version`: `1`
- `contract_version`: `"1"`

`cass capabilities --json` advertises the features `json_output`, `jsonl_output`, `robot_meta`, `time_filters`, `field_selection`, `content_truncation`, `aggregations`, `wildcard_fallback`, `timeout`, `cursor_pagination`, `request_id`, `dry_run`, `query_explain`, `view_command`, `status_command`, `state_command`, `api_version_command`, `introspect_command`, `export_command`, `expand_command`, `timeline_command`, and `highlight_matches`.

`cass introspect --json` currently advertises command schemas for `tui`, `index`, `completions`, `man`, `robot-docs`, `search`, `stats`, `diag`, `status`, `capabilities`, `state`, `api-version`, `introspect`, `view`, `health`, `doctor`, `context`, `sessions`, `resume`, `export`, `export-html`, `expand`, `timeline`, `pages`, `sources`, `models`, `import`, `analytics`, and `daemon`.

The response schema registry is narrower. It currently includes schemas for `api-version`, `capabilities`, `diag`, `health`, `index`, `introspect`, `search`, `sessions`, `state`, `stats`, `status`, and `view`.

## Stability Signals

CASS has several useful contract controls:

- `api-version` and `capabilities` expose version and feature discovery.
- `introspect` exposes argument metadata and JSON response schemas for the primary robot surfaces.
- `tests/golden/robot/` freezes robot JSON payloads and shape payloads.
- `tests/golden/robot_docs/` freezes robot documentation text.
- `tests/golden/PROVENANCE.md` documents regeneration, review expectations, and scrub rules.
- `tests/metamorphic_introspect_schema.rs` runs live commands and verifies their runtime JSON shapes are covered by the advertised `introspect.response_schemas`.
- `tests/golden/robot/error_envelope_kinds.json.golden` freezes the error-kind taxonomy and exit-code mapping.

These are strong enough for `ee` to treat CASS as a versioned CLI dependency, provided `ee` adds its own fixture layer around the subset it consumes.

## Integration Risks

Nonzero exit does not mean stdout is unusable. `cass health --json` can exit `1`, emit a valid health payload to stdout, and emit a JSON error envelope to stderr. The `ee` adapter must capture both streams, parse stdout when present, and classify stderr separately.

Diagnostics may appear on stderr for successful commands. A stale-index `cass search --robot` probe exited `0`, returned JSON on stdout, and emitted an index warning on stderr. The contract is data-only stdout and diagnostics/progress on stderr, not stderr-empty success.

Degraded search is normal. The local probe reported stale lexical state and missing semantic assets while still returning a valid robot JSON search response with `_meta`, `index_freshness`, `_warning`, and semantic fallback fields. `ee` should preserve this degraded evidence instead of converting it to a hard failure when lexical results are still available.

`robot-docs` is a plain-text documentation surface, not a JSON data surface. It is useful for humans and drift review, but should not be parsed as an operational API.

Ambient CASS state is not deterministic. Tests must pin `HOME`, `XDG_DATA_HOME`, and `CASS_IGNORE_SOURCES_CONFIG=1`, and should disable update prompts with `CODING_AGENT_SEARCH_NO_UPDATE_PROMPT=1`. Golden scrubbing must handle versions, timestamps, absolute paths, UUIDs, latency, and host load metrics.

Some advertised commands lack response schemas. `expand` and `timeline` are advertised capabilities, but are not currently in `response_schemas`. They should be considered fixture-backed but not schema-backed until CASS expands the registry or `ee` owns local shape tests for those surfaces.

## Adapter Requirements For `ee`

The first CASS adapter should:

- Run CASS via `std::process::Command` or the project runtime wrapper around it; do not link to CASS internals.
- Pass `--json` or `--robot` on every automated command.
- Prefer `--robot-meta`, `--fields minimal`, `--max-tokens`, and `--request-id` for search probes.
- Treat stdout as the only machine-data channel.
- Treat stderr as diagnostic data that can contain JSON error envelopes or human warnings.
- Persist command, argv, cwd/workspace, sanitized env overrides, exit code, parsed stdout schema name, stderr classification, elapsed time, and request id for provenance.
- Branch on stderr error `kind`, not numeric `code`.
- Check `api_version`, `contract_version`, and required capabilities before running import/search flows.
- Preserve degraded CASS state in `ee status` and context provenance instead of hiding it.
- Return a stable `ee` error or degradation code when CASS is missing, too old, has a contract mismatch, emits invalid stdout JSON, or reports unavailable lexical search.

## Fixture Plan

Gate 6 (`eidetic_engine_cli-s67f`, CASS Robot Contract Fixture) should add the executable coverage for this spike.

Recommended fixture families:

| Fixture | CASS command | Assertions |
| --- | --- | --- |
| Discovery contract | `cass api-version --json`, `cass capabilities --json`, `cass introspect --json` | Version fields parse; `contract_version` is recorded; required features and response schemas exist. |
| Health degraded contract | `cass health --json --data-dir <isolated-dir>` | Nonzero exit can still have valid stdout JSON; stderr error envelope has stable `error.kind`; stdout remains machine data. |
| Search robot contract | `cass search "hello" --robot --robot-meta --fields minimal --request-id <stable> --data-dir <seeded-fixture>` | Search JSON shape, request id echo, `_meta`, cursor fields, warning/degraded fields, deterministic hit ordering. |
| Sessions contract | `cass sessions --json --data-dir <seeded-fixture>` | Stable session list shape and deterministic ordering. |
| View contract | `cass view <fixture-session.jsonl> -n <line> --json` | Source-line provenance, message shape, and invalid-line error behavior. |
| Expand contract | `cass expand <fixture-session.jsonl> -n <line> -C 3 --json` | Fixture-backed shape until CASS adds an `introspect.response_schemas.expand` entry. |
| Error envelope contract | Invalid command input that exits nonzero | stderr JSON parses as `{ "error": { "kind", "message", "hint", "retryable" } }`; stdout is empty unless the command explicitly has a partial stdout contract. |
| Robot docs drift snapshot | `cass robot-docs contracts` and `cass robot-docs schemas` | Text snapshot only; not parsed as runtime JSON. |

Each fixture should log command, cwd, sanitized env, elapsed time, exit code, stdout/stderr artifact paths, schema or golden validation status, redaction status, and a concise first-failure diagnosis, matching `docs/testing-strategy.md`.

## Go / No-Go Boundaries

Go for:

- CASS-backed import/search/context provenance implemented as a subprocess adapter.
- Version/capability preflight before CASS-backed commands.
- Fixture-backed contract tests around the exact CASS surfaces `ee` consumes.
- Degraded mode that records stale indexes, missing semantic assets, and CASS health recommendations.

No-go for:

- Direct reads from the CASS SQLite database.
- Parsing `robot-docs` as an operational API.
- Depending on non-schema-backed `expand` or `timeline` fields without local `ee` fixtures.
- Treating nonzero process exit as automatic loss of stdout payload.
- Running `cass index --full` or `cass models install` implicitly from normal `ee` read paths.

## Closure Note

This spike is documentation-only. It does not add executable tests because the follow-up executable work is already represented by Gate 6 (`eidetic_engine_cli-s67f`) and related contract-test beads. The nearest verification hook is the proposed CASS robot fixture suite above.
