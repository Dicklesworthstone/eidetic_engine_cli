# ee doctor first-aid precedence

> **bd-1c8ga.** Agents repairing local environment problems should call
> `ee doctor --fix-plan` first and only fall back to the manual skill
> playbooks (fixing-beads-problems / rch / br-retry) when the
> deterministic auto-fixer cannot resolve the situation. The skills
> remain available as fallback resources for cases the auto-fixer does
> not cover; they should no longer be the first thing an agent reaches
> for.

## Canonical first-aid order

1. **`ee doctor --fix-plan --json`** — pure read; no mutation. Lists, in
   order, every failing check that carries a repair hint, with the hint's
   command. Each step's `fixMode` says what `ee doctor --fix` does for it:
   `auto_repair` (it repairs it), `auto_guidance` (it records guidance and
   repairs nothing) or `manual` (it does not act); `fixFinding` names the
   dispatched finding. `fixableIssues` counts only `auto_repair` steps.
2. **`ee doctor --fix --json`** — applies the findings `--fix` can
   dispatch (listed below) through the single `mutate()` chokepoint with
   undo metadata; `ee doctor --undo <runId> --json` reverses a run.
   It always runs every dispatchable finding: `--fix --only <id>` is a
   usage error, because `--fix` declares a conflict with `--only`. A
   failing check with no dispatch is left untouched and gets no
   `fixerResults` entry.
3. **`ee doctor --json` (read-only)** — the diagnostic report. Use it to
   inspect findings `--fix` does not dispatch, or that require human
   approval (e.g. anything that would touch the work tree's tracked
   files, anything mutating shared infrastructure).
4. **Manual skill playbooks** — fall through to these only after
   steps 1–3 fail to converge.

## When the manual skills are still the right call

The auto-fixer is intentionally conservative. Reach for the manual
skills (and their references / agents / scripts subtrees) when:

| Situation | Skill |
| --- | --- |
| `.beads/issues.jsonl` is corrupt enough that `br doctor` / `br show` fail, both stores have diverged, or DB-only changes must be harvested before rebuilding `.beads/beads.db`. | [`fixing-beads-problems`](../../../.claude/skills/fixing-beads-problems/SKILL.md) |
| RCH workers fail preflight or the daemon is silent / version-skewed / disk-full, and the `bd-17c65.10.17.*` topology lane has not yet shipped a fix. | [`rch`](../../../.claude/skills/rch/SKILL.md) |
| `br sync` retries are needed across a multi-agent commit window or a previous `br` operation aborted partway through. | `br-retry` (if present) or the [`br`](../../../.claude/skills/br/SKILL.md) skill itself. |

A doctor fixer that lights up the same diagnostic the manual skill
covers should be considered the primary path; the skill content is
authoritative reference material for the cases the fixer cannot
auto-resolve safely.

## How to know if `ee doctor --fix` already handles your situation

`ee doctor --fix` dispatches six findings, keyed on the failing check's
error code, and only the two index findings repair anything. The other
fixers in `src/core/doctor_fixers.rs` are not dispatched by `--fix`:

| Finding (`findingCode`) | Failing check | Operation | Effect |
| --- | --- | --- | --- |
| `database_empty` | `database` `EE-E206` (0-byte store) | `manual` | guidance only (`guidance_recorded`): recover from backups |
| `database_corrupted` | `database` `EE-E202` (store cannot be opened) | `manual` | guidance only (`guidance_recorded`): recover from backups |
| `search_index_missing` | `EE-E300` | `run_index_rebuild` | repairs (`applied`) |
| `search_index_stale` | `EE-E301`, or any other failing `search_index` check | `run_index_rebuild` | repairs (`applied`) |
| `schema_migration_pending` | `EE-E700` | `run_migration` | guidance only (`guidance_recorded`): records `ee migrate run`, migrates nothing |
| `cass_integration_drift` | `EE-E507` | `manual` | guidance only (`guidance_recorded`) |

While the store is empty or cannot be opened (`EE-E206` or `EE-E202` on
the `database` check), `--fix` skips the index and migration findings:
they read the damaged store (bd-xa6ud).

Anything else still needs the manual skill content. The table is the
dispatch in `fix_finding_for_check` (`src/core/doctor_fixers.rs`), which
`--fix` and `--fix-plan` both read; `--fix-plan` reports it per step as
`fixMode`. `ee doctor --capabilities --json` lists the same table as
`fix_dispatch` (`finding`, `op_kind`, `effect`: `repair` or `guidance`).
Its `op_kinds` is different: that is every op the `mutate()` chokepoint
accepts, most of which no dispatched fixer produces (bd-223vl M5).

If `ee doctor --fix` leaves a failing check without a `fixerResults`
entry, or records it with outcome `guidance_recorded`, that is the
hand-off signal to the fallback skill.

## Why this precedence

- **Determinism** — `ee doctor --fix` routes every mutation through a
  single `mutate()` chokepoint with backup, undo, and audit metadata.
  Manual skill execution depends on the operator following the steps
  in the right order; the auto-fixer enforces ordering and
  reversibility by construction.
- **Verifiable evidence** — the `ee.doctor.fix_summary.v1` response carries a
  structured record of what was done, what was backed up, and what
  remains. Pasting that into a bead is a clearer audit trail than
  pasting the output of an interactive skill run.
- **Concurrency** — the `mutate()` chokepoint acquires the same
  capability locks the rest of `ee` already uses; running the fixer
  while another agent is mid-commit is safer than running the manual
  skill steps in parallel. The `.ee/.doctor.lock` file is deliberately
  persistent: an active fix or undo run is represented by the OS advisory
  lock held on that exact file handle, not by the pathname's existence.
  Teardown unlocks the retained handle and never removes or replaces the
  public path, so a peer process cannot have its replacement lock unlinked.
  Harnesses should attempt the advisory lock rather than deleting the file.
- **Discoverability** — `ee doctor --help` and `ee doctor
  --capabilities` are the canonical surfaces agents will check; the
  external skill playbooks are easy to miss until an agent already
  knows their name. Demoting them to fallback aligns discoverability
  with the deterministic path.

## Migration note for skill authors

When updating the three skill SKILL.md files (`fixing-beads-problems`,
`rch`, `br-retry` if/when it lands) to reflect this precedence,
prepend a callout that points operators at this document first. The
skill body remains the canonical fallback reference for cases the
auto-fixer does not cover, so do not delete content — only relocate
the "first thing to try" anchor to `ee doctor --fix-plan`.
