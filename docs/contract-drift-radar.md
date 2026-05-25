# Contract Drift Radar

> **Audience:** maintainers and agents triaging stale contract documentation.
> This page documents the workflow for `scripts/contract-drift-radar.sh`
> (the static gate) and the matching Cargo-backed proof
> (`tests/contracts/schema_drift.rs`). Background lives at bd-31nul (the
> parent radar epic) and bd-31nul.5 (this gate slice).

## 1. What the radar detects

The radar protects four contract surfaces from silent drift:

| Surface | Stored in | How the radar checks it |
|---|---|---|
| JSON schema files | `docs/schemas/ee.*.json` | Enumerated; canonical schema-id inventory. |
| Agent-facing documentation references | `AGENTS.md`, `README.md`, `CLAUDE.md`, `docs/agent-ux/**/*.md`, `docs/external-derivation-operator.md`, `docs/agent_integration.md`, `docs/contract-drift-radar.md`, `docs/migration-guide.md` | Greps for stale envelope versions (`ee.response.v1`, `ee.error.v1`) when v2 ships, with allow-marker overrides for legacy prose. |
| Embedded JSONC envelope examples | Same docs as above | Extracts ```json / ```jsonc fenced blocks containing `"schema":"ee.…"` and verifies the pinned schema id exists in the inventory. |
| Degraded-code taxonomy | `docs/degraded_codes.md` vs `tests/fixtures/failure_modes/*.json` | Cross-checks every H2-headed code in the catalog against a per-code fixture. |

It does **not** replicate full JSON-schema validation — that is the
Cargo-backed contract test
(`tests/contracts/schema_drift.rs`, see §5). The static radar is a
no-Cargo prefilter that runs in the existing advisory-gate slot of
`scripts/verify.sh`.

## 2. When to run it

Run the radar after editing any of these surfaces:

- A `docs/schemas/ee.*.json` schema file (added, renamed, bumped, or removed).
- A current-facing doc that pins a schema id in a fenced JSON/JSONC example.
- `docs/degraded_codes.md` or any `tests/fixtures/failure_modes/<code>.json`.
- An `AGENTS.md` / `README.md` / `CLAUDE.md` block that describes envelope
  shape, exit codes, or schema versions.
- `docs/migration-guide.md` when the migration plan changes.
- A `.beads/issues.jsonl` migration that retires a degraded code.

In addition, the radar runs automatically as Gate 3.8 of
`scripts/verify.sh` (advisory; non-blocking).

## 3. Running the static gate

```bash
# default — human summary on stderr, report at .contract-drift-radar-report.json
scripts/contract-drift-radar.sh

# quiet (CI / logs)
scripts/contract-drift-radar.sh --quiet

# emit the report on stdout (still writes to disk)
scripts/contract-drift-radar.sh --json > radar.json

# fail-closed on any violation (default is advisory exit 0)
scripts/contract-drift-radar.sh --strict

# capture per-phase events to an ee.test_event.v1 JSONL log
scripts/contract-drift-radar.sh --events-out /tmp/radar.events.jsonl
```

Report shape (schema `ee.contract_drift_radar.v1`):

```jsonc
{
  "schema": "ee.contract_drift_radar.v1",
  "generatedAt": "2026-05-25T21:50:00Z",
  "verdict": "violations",          // "ok" | "violations"
  "summary": {
    "docsScanned": 25,
    "schemasLoaded": 119,
    "staleEnvelopeRefs": 0,
    "envelopeExamplesScanned": 27,
    "schemaIdViolations": 2,
    "documentedCodes": 419,
    "fixtureCodes": 419,
    "documentedMissingFixture": 0
  },
  "schemaInventory": ["ee.response.v2", "ee.error.v2", "..."],
  "violations": {
    "docsScan": [],                 // stale_envelope_version_reference[]
    "jsonExampleCheck": [],         // json_example_schema_id_unknown[]
    "taxonomyXcheck": []            // documented_code_missing_fixture[]
  }
}
```

Per-phase JSONL on stderr (also `--events-out`) uses
`schema = "ee.test_event.v1"`, `surface = "contract_drift_radar"`,
`beadId = "bd-31nul.5"`, and phases
`inventory_load → docs_scan → json_example_check → taxonomy_xcheck → summary`.

## 4. Interpreting violations

### 4.1 `stale_envelope_version_reference`

Triggered when a current-facing doc references the deprecated v1 envelope <!-- contract-drift-allow: this file documents the radar -->
schemas while the live envelope is v2. The match line is <!-- contract-drift-allow: this file documents the radar -->
captured under `violations.docsScan[].context`.

**Fixes (in order):**

1. **Update the doc.** Replace the deprecated v1 schema id with v2, <!-- contract-drift-allow: this file documents the radar -->
   adjust accompanying field examples (`recovery[]` shape, etc.). This
   is the right fix when the doc was simply not refreshed for v2.
2. **Mark the line as historical.** Append a same-line HTML comment:
   ```text
   The pre-0.2 envelope shape: `{"schema":"ee.response.v1",...}` <!-- contract-drift-allow: pre-0.2 migration prose -->
   ```
   The radar's `docs_scan` phase skips lines containing
   `<!-- contract-drift-allow:...` and lines containing
   `archived`, `historical`, `deprecated`, or `legacy`.

### 4.2 `json_example_schema_id_unknown`

Triggered when a fenced ```json / ```jsonc block in a current-facing
doc carries `"schema":"ee.<something>.vN"` but no
`docs/schemas/ee.<something>.vN.json` file exists.

**Fixes:**

1. **Real drift — schema file missing.** The doc references a schema
   that was renamed, removed, or never landed. Either ship the
   missing `docs/schemas/ee.<something>.vN.json` file or update the
   doc to a live schema id.
2. **Legacy-by-design example.** When the doc deliberately shows a
   historical envelope (e.g. the `docs/migration-guide.md` v0 line),
   add a marker on the line **immediately preceding the fence**:
   ```markdown
   <!-- legacy-example: pre-0.2 envelope, no live schema file by design -->
   ```json
   {"schema":"ee.response.v0","ok":true,"result":{}}
   ```
   ```
   `<!-- legacy-example -->` and `<!-- contract-drift-allow:...` both
   cause the radar to count the example under `skippedLegacyExamples`
   instead of `schemaIdViolations`.

### 4.3 `documented_code_missing_fixture`

Triggered when `docs/degraded_codes.md` has an H2-headed code
(`## \`<code>\``) but `tests/fixtures/failure_modes/<code>.json` is
absent. The taxonomy gate (`bd-3usjw.60`) requires both to land in the
same PR.

**Fixes:**

1. **Doc-only entry.** Either remove the doc entry or land the fixture.
2. **Renamed code.** Update the doc heading to the live code name.
3. **Generated doc.** If `docs/degraded_codes.md` is auto-generated from
   the fixture catalog (K3), regenerate it.

The reverse direction (fixture present but no doc) is checked by
`tests/contracts/failure_mode_fixtures.rs`, not by this radar.

## 5. Cargo-backed proof (RCH-only)

The static radar prefilters; the canonical proof is the schema-drift
contracts test:

```bash
# RCH-only. Do NOT run local cargo on this Mac dev host — the canonical
# build path is remote workers. RCH-E327 path-topology blockers MUST NOT
# be papered over by local Cargo fallback (per AGENTS.md RCH section).
TMPDIR=/tmp \
RCH_REQUIRE_REMOTE=1 \
RCH_VISIBILITY=summary \
RCH_CANONICAL_PROJECT_ROOT=/Users/jemanuel/projects \
RCH_ALIAS_PROJECT_ROOT=/data/projects \
~/.local/bin/rch exec -- \
  env TMPDIR=/tmp CARGO_TARGET_DIR=/Volumes/USBNVME16TB/temp_agent_space/cargo-target \
  cargo test -p ee --test contracts -- schema_drift
```

The Cargo gate (`tests/contracts/schema_drift.rs`) does what the static
gate intentionally **does not** do:

- Deep-validates every emitted response against its declared JSON
  schema using the `jsonschema` crate.
- Normalises JSONC (strips comments and trailing commas) before
  parsing each documented example.
- Asserts that every `CORE_SCHEMAS` entry maps to a present schema
  file with a parseable definition.

The static and Cargo gates are complementary. The static gate runs
under `verify.sh` and any agent shell; the Cargo gate runs only via
RCH and is the authoritative source of truth.

## 6. RCH-blocked environments

If RCH refuses (E327 path topology, worker pressure, transport
timeout, etc.), the contract:

- **Do not** fall back to local Cargo. AGENTS.md is explicit: remote
  refusal is a verification blocker.
- **Do** record the exact blocker text in the Beads bead comment using
  the closeout template (§7).
- **Do** rely on the static radar's `ee.contract_drift_radar.v1`
  report as the local evidence; pair it with the most recent
  successful Cargo proof (commit SHA) in the closeout.

## 7. Beads closeout template

When closing or commenting on a bead that touched contract surfaces,
include:

```markdown
**Contract-drift radar evidence**

- Static gate: `scripts/contract-drift-radar.sh --json > radar.json` →
  `verdict=<ok|violations>`, schemasLoaded=<n>, schemaIdViolations=<n>,
  documentedMissingFixture=<n>.
- Surviving violations (if any): list each `violations.*[]` entry by
  file:line + reason. Either include the fix in the same PR or open a
  follow-up bead with a clear reproduction step.
- Cargo proof: `cargo test -p ee --test contracts -- schema_drift`
  via RCH at commit <sha> (or "RCH blocked: <verbatim blocker text>").
- Allow markers added: list any new `<!-- contract-drift-allow:... -->`
  or `<!-- legacy-example -->` markers, the doc + line they were added
  to, and the historical reason.

Local Cargo fallback was NOT used. (Mandatory line.)
```

The wording "Local Cargo fallback was NOT used." is load-bearing — it
documents that the closure follows the RCH-only verification policy,
even when remote proof is blocked.

## 8. Adding new doc paths to the scan

The current scan set is hard-coded at the top of
`scripts/contract-drift-radar.sh` (`CURRENT_DOCS` array plus a glob
over `docs/agent-ux/*.md`). To extend coverage:

1. Append the new doc path to the explicit list, or extend the
   `find docs/<dir>` glob.
2. Re-run the radar; confirm the file appears in `docsScanned` and
   that no new false-positive violations fire.
3. If the new doc deliberately shows legacy envelopes, attach
   `<!-- legacy-example -->` markers per §4.2.

Do **not** add `docs/archive/**` to the scan. Archived bridge plans
and historical reference docs are intentionally allowed to carry
v1/v0 envelopes; they document past surfaces, not the active
contract.

## 9. Related references

- `tests/contracts/schema_drift.rs` — Cargo-backed JSONC envelope
  validator (c750b1b9 landed JSONC normalisation + envelope checks).
- `tests/contracts/failure_mode_fixtures.rs` — reverse-direction
  fixture↔doc taxonomy validator (J6).
- `docs/degraded_code_taxonomy.md` — degraded-code classification
  (`build_time | mixed | response_time`).
- `docs/degraded_codes.md` — auto-generated catalog from
  `tests/fixtures/failure_modes/` (K3).
- `AGENTS.md` — "Response envelope contract", "Schema versions",
  and "Failure-mode catalog" sections that the radar protects.
