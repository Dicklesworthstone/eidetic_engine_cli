# ee doctor first-aid precedence

> **bd-1c8ga.** Agents repairing local environment problems should call
> `ee doctor --fix-plan` first and only fall back to the manual skill
> playbooks (fixing-beads-problems / rch / br-retry) when the
> deterministic auto-fixer cannot resolve the situation. The skills
> remain available as fallback resources for cases the auto-fixer does
> not cover; they should no longer be the first thing an agent reaches
> for.

## Canonical first-aid order

1. **`ee doctor --fix-plan --json`** — dry-run; reports the auto-fixable
   findings the doctor would apply along with their blast radius and
   the deterministic `mutate()` log. Pure read; no mutation.
2. **`ee doctor --fix --json`** — applies the auto-fixable findings
   through the single `mutate()` chokepoint with reversible/undo
   metadata. Honors `EE_DOCTOR_FIX_DRY_RUN=1` for dry-run override and
   `EE_DOCTOR_FIX_STRICT=1` to fail closed on findings the fixer
   cannot handle.
3. **`ee doctor --json` (read-only)** — full diagnostic report. Use to
   inspect findings that the auto-fixer flagged as not-yet-implemented
   or that require human approval (e.g. anything that would touch the
   work tree's tracked files, anything mutating shared infrastructure).
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

Run `ee doctor --capabilities --json`. The capability descriptor lists
each fixer kind the binary supports, the situations it auto-resolves,
and its `mutate()` reversibility class. The 12 P0/P1 fixers landed by
bd-tu4s8 are listed there; anything not on the list still needs the
manual skill content.

If `ee doctor --fix-plan` reports `degradedCodes: ["doctor_no_auto_fix_available"]`
for your situation, that is the explicit hand-off signal to the
fallback skill.

## Why this precedence

- **Determinism** — `ee doctor --fix` routes every mutation through a
  single `mutate()` chokepoint with backup, undo, and audit metadata.
  Manual skill execution depends on the operator following the steps
  in the right order; the auto-fixer enforces ordering and
  reversibility by construction.
- **Verifiable evidence** — the `ee.doctor.fix.v1` response carries a
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
