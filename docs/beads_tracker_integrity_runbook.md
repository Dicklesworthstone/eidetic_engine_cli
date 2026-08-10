# Beads Tracker Integrity Runbook

bd-2p297 family. What to do when `br`/`bd` commands fail with tracker
export errors (`Invalid JSON at line N`, `Failed to flush bd changes to
JSONL`, `refusing to export empty database over non-empty JSONL`). The
tracker has TWO stores — the SQLite DB and the `.beads/issues.jsonl`
export — and every failure here is some divergence between them. The
repair direction depends entirely on WHICH side is authoritative, so
never improvise: classify first.

Hard rules (unchanged from AGENTS.md): never delete tracker files, never
hand-edit `issues.jsonl`, no worktrees/stash/reset, no local Cargo, and a
tracker repair claims **no source or test verdicts**.

## Step 1 — stop and classify (read-only)

```bash
scripts/beads_export_repair.sh --classify
```

This emits `beads.export_integrity.v1` with bounded, body-free evidence
(valid-record count, invalid line numbers and shape classes, merge-marker
count, DB health from `br doctor`'s `workspace_health` field, DB issue
count). States and what they mean:

| State | Meaning | Mutation safe? |
|---|---|---|
| `healthy` | Everything parses; counts corroborate. | yes |
| `transient_partial_write` | Export modified seconds ago; likely mid-flight. Re-classify after a short wait. | wait |
| `invalid_trailing_line` | DB healthy, counts corroborate within tolerance, unparseable line(s) only at the tail — the classic interrupted export (`}]}` after N valid records). | stop; safe repair candidate |
| `invalid_interior_lines` | Unparseable line(s) with valid records AFTER them. Something wrote into the middle; hand inspection, no automated repair. | stop |
| `count_divergence_db_behind` | JSONL holds more valid records than the DB (e.g. empty DB next to a full export). **A forced export here destroys the tracker** — repair direction is import, not export. | stop |
| `count_divergence_jsonl_behind` | Valid records trail the DB beyond tail tolerance; ambiguous. | stop |
| `merge_markers` | Conflict markers in the export; resolve the collision by hand. | stop |
| `db_unhealthy` / `unknown` | No safe direction without DB integrity evidence. | stop |

Ordinary `scripts/br_retry.sh` retries are appropriate ONLY while the
classification is `healthy` or `transient_partial_write`. Any other state
means stop mutating the tracker (no `br create/update/close`, no `bd
sync`) until repaired.

## Step 2 — preview the repair

```bash
scripts/beads_export_repair.sh --dry-run
```

Prints the exact command it would run and why
(`beads.export_repair_plan.v1`), or a typed refusal. The only
auto-repairable state is `invalid_trailing_line`, and only because DB
integrity plus corroborating counts make the DB authoritative.

## Step 3 — apply with evidence

```bash
scripts/beads_export_repair.sh --apply
```

Fail-closed guards: refuses on every non-candidate state and when a
`.beads/*.lock` suggests another mutation is in flight. On success it
runs `br sync --flush-only --force --json`, re-runs doctor, and appends a
`beads.export_repair_report.v1` record (pre/post sha256, exit code,
post-doctor result, both classifications) to
`.beads/export-repair-evidence.jsonl`.

## Step 4 — record it

Paste this template into the relevant bead (and Agent Mail if peers are
mid-claim), filling from the `--apply` report:

```text
TRACKER REPAIR (beads.export_repair_report.v1):
state before: <pre.state> (<pre.reason>)
invalid lines: <pre.evidence.invalidLineNumbers> of <pre.evidence.jsonlTotalLines>
counts: db=<pre.evidence.dbIssueCount> jsonlValid=<pre.evidence.jsonlValidRecords>
command: br sync --flush-only --force --json (exit <exitCode>)
export sha256: <preExportSha256> -> <postExportSha256>
post-doctor: <postDoctorOk>; state after: <post.state>
No source or test verdicts are claimed by this repair.
```

## Regression coverage

`scripts/beads_export_repair.sh --self-test` (inline cases) and
`--fixture-suite tests/fixtures/beads_export` (committed fixtures, incl.
the literal incident shape) both run as verify.sh stages; extend the
fixture set when a new divergence shape appears in the wild.
