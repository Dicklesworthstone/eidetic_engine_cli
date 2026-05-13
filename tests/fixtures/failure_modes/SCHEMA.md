# Failure-mode fixture schema (`ee.failure_mode_fixture.v1`)

Each JSON file in this directory documents exactly one `degraded[]` entry
that some `ee` command can emit. The catalog has three jobs:

1. **Discoverability** — an agent (human or AI) browsing this directory
   can enumerate every honesty signal the system can raise and read its
   shape, severity, and repair path without grepping the source.
2. **Drift detection** — `tests/contracts/failure_mode_fixtures.rs`
   walks the catalog, asserts each fixture is well-formed, and verifies
   that every documented `code` corresponds to a real string in `src/`
   so a code removed from production cannot leave a stale fixture
   behind.
3. **Per-epic landing target** — when a new epic (`B`, `C`, `E`, `G`,
   `L`, …) introduces a new degraded code, the implementing PR is
   expected to land its fixture here. That keeps the catalog complete
   without J6 needing to rewrite it.

## File layout

```
tests/fixtures/failure_modes/
├── SCHEMA.md            ← this document
├── README.md            ← human-friendly catalog index
├── no_relevant_results.json
├── weak_query_recall.json
├── duplicates_collapsed.json
├── …                    ← one file per code
```

The filename is the bare `code` (no `.degraded.` prefix) suffixed with
`.json`. Filenames must match `[a-z][a-z0-9_]*` so the catalog directory
lists alphabetically and so the contract test can cross-check
`filename_stem == fixture.code`.

## Fixture JSON shape

```jsonc
{
  // Required. Schema version pin for the fixture catalog itself; bump
  // when this document changes shape. v1 is the initial version filed
  // alongside J6 (bd-17c65.10.6).
  "schema": "ee.failure_mode_fixture.v1",

  // Required. The exact `code` string the production response emits.
  // Must match a literal in src/ (the contract test greps for it).
  "code": "no_relevant_results",

  // Required. Bead reference for the bead that introduced this code.
  // Format: { "bead": "bd-…", "epic_letter": "B|C|E|…" }
  "introduced_by": { "bead": "bd-17c65.2.1", "epic_letter": "B" },

  // Required. The CLI surfaces that emit this code. At least one entry.
  // Surfaces are the `command` field of the response envelope (e.g.
  // "search", "context", "doctor", "status", "memory show", "why").
  "surfaces": ["search"],

  // Required. Severity classification matching the production
  // SearchDegradation / ContextResponseDegradation factory: one of
  // "info" | "low" | "medium" | "high" | "critical".
  "severity": "medium",

  // Required. Whether the production factory always populates `repair`
  // (true) or leaves it null (false, e.g. duplicates_collapsed which
  // is purely informational).
  "repair_present": true,

  // Required. Human-readable trigger description and the minimal
  // setup that should produce the emission. The setup commands are
  // documentation, not necessarily executed by the contract test —
  // per-epic e2e drivers (scripts/e2e_overhaul/*) exercise them
  // end-to-end.
  "trigger": {
    "description": "ee search where every candidate scores below the relevance floor.",
    "setup_commands": [
      "ee init --workspace .",
      "ee remember 'Forbidden deps: tokio, rusqlite, petgraph' --level procedural --kind rule"
    ],
    "invocation": "ee search 'completely unrelated query' --workspace . --json"
  },

  // Required. The shape an agent will see when this code fires.
  // `message_contains` is a list of substrings that must appear in the
  // emitted message. Use this instead of an exact-match message so
  // template values (floor, query, counts) can vary without breaking
  // the fixture. `repair_contains` (when `repair_present` is true) is
  // a single substring asserted on the repair hint.
  "expected_emission": {
    "code": "no_relevant_results",
    "severity": "medium",
    "message_contains": [
      "No memories scored above relevance floor",
      "considered"
    ],
    "repair_contains": "lower --relevance-floor"
  }
}
```

## Adding a fixture

1. Land your epic's degraded code in `src/`.
2. Create `tests/fixtures/failure_modes/<your_code>.json` matching the
   schema above.
3. Run `cargo test --test contracts failure_mode_fixtures` — the
   contract test validates structure and cross-references the code
   string against `src/`.
4. Mention the new fixture in `README.md` so the catalog index stays
   readable.

## Why a static catalog (not E2E)

Per-epic e2e drivers under `scripts/e2e_overhaul/` already exercise
each degraded path end-to-end against the real binary. This catalog is
a **structural reference** — it documents what the codes look like,
which surfaces emit them, and what an agent should do when they see
one. The contract test enforces that the structural reference cannot
drift away from the production source.

Bead: `bd-17c65.10.6` (J6).
