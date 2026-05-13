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
  // "info" | "low" | "warning" | "medium" | "high" | "critical".
  "severity": "medium",

  // Required. Whether the production factory always populates `repair`
  // (true) or leaves it null (false, e.g. duplicates_collapsed which
  // is purely informational).
  "repair_present": true,

  // Optional. True only for a legacy code intentionally kept in the
  // catalog as a tombstone after production stopped emitting it. Retired
  // fixtures keep the historical `code` and `expected_emission` shape so
  // tooling can recognize the code, while the e2e driver asserts the
  // production emission pattern is absent.
  "retired": false,
  "retired_by": {
    "bead": "bd-17c65.5.2",
    "reason": "The meta signal was replaced by concrete degraded[] entries."
  },

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
    "repair_contains": "lower --relevance-floor",

    // Optional. The exact full text of the repair hint emitted by
    // production for this code. Use `repair_string` when the code emits
    // exactly one repair template. Use `repair_strings` (array) when
    // the same code emits one of several repair variants depending on
    // its trigger branch (e.g. agent_mail_unavailable has three).
    // Owned by J6.1 (bd-17c65.10.6.1). Present-state is per-fixture;
    // a code's fixture is "pinned" once either field is populated.
    "repair_string": "Lower --relevance-floor to inspect rejected matches.",

    // Optional. A regex (PCRE2-subset compatible with the Rust `regex`
    // crate) that extracts a runnable shell command from `repair_string`
    // (or any element of `repair_strings`). MUST contain exactly one
    // named capture group called `cmd`. Required when the repair hint
    // is itself a runnable command. Absent when the hint is prose-only
    // (e.g. "Check the configured Agent Mail snapshot path."). Allows
    // an agent to mechanically extract the next action without parsing
    // arbitrary prose. Owned by J6.1.
    "repair_command_regex": "(?P<cmd>ee [a-z][a-z0-9 -]*(?: --[a-z][a-z0-9-]*(?:[= ][^ ]+)?)*)"
  }
}
```

### Pinning repair contents (J6.1 contract)

Beyond the substring assertion (`repair_contains`), fixtures can pin
the *exact text* of the repair hint and a regex for extracting a
runnable command. The schema fields are:

- `repair_string` — single full literal that production emits.
- `repair_strings` — array, when the code has multiple repair
  variants. At most one of `repair_string` / `repair_strings` may be
  set per fixture.
- `repair_command_regex` — extraction regex with named capture `cmd`.
  Optional; only present when the repair text is itself a runnable
  command.

When either pinning field is present, the J6.1 contract test
(`tests/contracts/failure_mode_repair_string.rs`) asserts:

1. The string(s) are non-empty.
2. If `repair_command_regex` is present, it compiles under the Rust
   `regex` crate and contains exactly one named capture named `cmd`.
3. If both a pinning field and `repair_command_regex` are present,
   the regex matches at least one of the pinned strings and the `cmd`
   group is non-empty.
4. If `repair_contains` is set alongside, the pinned string(s) must
   each contain the `repair_contains` substring (consistency).

The fixture-author workflow is:

1. Locate the exact `repair: Some("…")` literal in `src/` for the
   code.
2. Paste it verbatim into `repair_string` (or `repair_strings` for
   multi-variant codes).
3. If the literal IS a runnable command, add `repair_command_regex`
   targeting it.
4. Run `cargo test --test contracts failure_mode_repair_string`.

Backfill across all 138 `repair_present: true` fixtures is tracked
under follow-up bead `bd-17c65.10.6.1.1` (incremental per-PR).


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
many degraded paths end-to-end against the real binary. J6 also ships
`scripts/e2e_overhaul/failure_modes.sh`, which reads this catalog and
exercises every fixture with a public-CLI scenario where one is
available. Fixtures whose triggers still require direct mutation or
unimplemented replay surfaces are logged as explicit TODOs.

This catalog remains the **structural reference** — it documents what
the codes look like, which surfaces emit them, and what an agent should
do when they see one. The contract test enforces that the structural
reference cannot drift away from the production source.

Bead: `bd-17c65.10.6` (J6).
