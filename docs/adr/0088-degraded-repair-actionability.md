# ADR 0088: `degraded[].repair` is prose; actionability is a separate, declared field

Status: accepted
Date: 2026-09-17
Bead: bd-degraded-repair-actionability-4g6cp
Depends-on: ADR 0087 (canonical deterministic response contract)

## Context

`degraded[].repair` is `Option<String>` on `ContextResponseDegradation`
(`src/pack/mod.rs:5173-5178`) and on a dozen sibling types
(`src/core/health.rs`, `src/core/tripwire.rs`, `src/mesh/team.rs`,
`src/core/support_bundle.rs`, …). Agents read it to decide what to do next.

The open question was whether it must name a runnable command.

AGENTS.md:616 does **not** settle it. That line governs
`error.details.recovery[]`, a structured array on the *error* envelope.
Degradations are a different surface and carry no `recovery` field at all, so
the law constrains a field these codes do not have. Conflating the two is how
this nearly rode in under a taxonomy-gate fix (`b766ba417`).

## Measurement

The bead's headline figure of 273 is a **proxy**: it counts fixtures whose
declared `repair_contains` *fragment* lacks `ee `. The bead flags it as unusable
in both directions, and it is — `HOTSET_BEADS_UNAVAILABLE_CODE`'s real repair
names `br sync --flush-only`, a genuine command that an `ee `-based proxy scores
as prose. The actionability property lives on the **emitted** string, which
appears only in `src/` and which no fixture contains.

Census of emitted repair literals, product code only (inline `#[cfg(test)]`
modules excluded by brace counting; extractor validated against a raw-occurrence
count, 2496 = 2496):

| Class | Count | Share |
|---|---:|---:|
| Runnable — contains a complete tool invocation | 1347 | 54.0% |
| Names a flag or env var, but no invocation | 265 | 10.6% |
| Prose only | 884 | 35.4% |
| Empty | 0 | 0% |

Within the 884 prose-only, by leading imperative: ~519 (58%) read as judgement
or multi-step (`check` 173, `use` 146, `inspect` 47, `choose` 35, `verify` 25),
and only ~95 (11%) read as a single invocation (`retry`, `regenerate`, `run`,
`re-run`).

Two corrections were needed before these numbers were trustworthy, both found by
validating the enumerator rather than trusting it: Rust line-continuation
strings (`"text \` + newline) silently dropped 10 literals until the pattern
used `DOTALL`, and the first tool list mis-binned 13 real commands (`cass`,
`tailscale`, `curl`, `ollama`, `scripts/*.sh`).

## Decision

**No. `degraded[].repair` is not required to name a runnable command, and must
not be.** Three parts:

1. **`repair` stays human-facing prose.** Roughly 519 of the 884 prose-only
   repairs describe a judgement or a multi-step action with no single
   invocation — "Explicitly re-enable mesh after containment review", "Choose a
   different workspace". Requiring a command there would force authors to invent
   one. A field that *looks* executable and is not is worse than prose, and it
   is precisely the false-actionability failure this codebase keeps
   rediscovering.

2. **Actionability becomes a declared field, not an inference.** Add
   `repairCommand: Option<String>`, populated only when a complete, runnable
   invocation exists. Consumers stop regex-scraping prose to decide whether they
   can act. This is additive and non-breaking: `repair` keeps its meaning, and
   the 1347 already-runnable sites migrate incrementally rather than in one
   sweep.

3. **The 265 flag/env-only repairs are a defect class and get a gate.** These
   are neither prose nor command: the author intended an action and shipped half
   of one. "Re-run with `--limit <= N`" and "Use `EE_MESH_HELLO_PORT=41888` or
   pass `--port 41888`" name an option with no verb an agent can execute. A
   repair that names a flag or env var while containing no tool token is the
   thing to fail on.

### Rejected alternatives

- **Require every repair to name a command.** Rejected on the measurement: it
  manufactures ~519 fake commands. The bead anticipated this and noted Option 2
  needs a third "advisory" state; part 2 above *is* that state, made explicit
  and machine-readable instead of implied by a vacuous `repair_contains`.
- **Give degradations the error envelope's `recovery[]` array.** Rejected as
  disproportionate: a structured array with `priority`/`kind`/`command` per
  entry is a type change reaching every degradation in the product for a
  surface where 54% of repairs are already a single command and a single
  optional field carries the same information.
- **Do nothing, treat the 273 fixture figure as the debt.** Rejected: that
  number measures fixture fragments, not emitted behavior, and is wrong in both
  directions.

## Verification hook

A gate over emitted repair literals with a **reachable failing arm and a
shrink-only baseline**, matching the pattern already used by
`tests/fixtures/contracts/no_silent_fallback_unclassified_baseline.txt` and
`tests/fixtures/vision_coverage/unexercised_baseline.txt`:

- **New arm:** a repair naming a flag or env var with no tool token, absent from
  the baseline, fails.
- **Stale arm:** a baseline entry that has since acquired a command fails,
  telling the author to delete the line. Without both arms the baseline decays
  into a permanent allowlist.
- The 265 current instances are baselined so the gate can fail today without
  turning `main` red for every agent.

An advisory repair — prose, no flag, no command — is explicitly **valid** and
must never be given a synthetic command to satisfy the gate.

## Consequences

- Consumers gain a field they can execute without parsing prose; nothing breaks,
  because `repair` is unchanged.
- The advisory class is legitimised rather than treated as debt, so the backlog
  is ~265 real defects, not ~1149 "non-actionable repairs".
- `ee.response.v2` gains an optional field. Per AGENTS.md's drift rules the
  addition needs a fixture and taxonomy entry when it lands; it does not need an
  envelope version bump, being additive and optional.
