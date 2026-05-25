# Swarm SLO Verification Profiles (bd-2dgn0.5)

`scripts/verify.sh` accepts two scope flags that let operators choose
how much of the readiness pipeline to run for a given workflow. The
default profile sits between them: it covers every correctness gate but
leaves benches and the pack-quality eval as opt-ins.

This document is the operator-facing companion to the bd-2dgn0
`swarm-slo` evidence epic. The replay runner (bd-2dgn0.2), redaction
adapters (bd-2dgn0.4), and scorecard budgets (bd-2dgn0.3) feed into the
two profiles described below.

## TL;DR — which flag should I use?

| Situation | Use |
| --- | --- |
| Agent pre-push readiness; per-PR swarm CI smoke; "did I break the type system?" | `./scripts/verify.sh --ci-smoke` |
| Local "is the tree fine?" check before a substantive change | `./scripts/verify.sh` (default) |
| Pre-release / weekly Swarm-X scorecard run on the canonical 64-agent host | `./scripts/verify.sh --swarm-heavy` |
| Following up a `--ci-smoke` PR before tagging a release | `./scripts/verify.sh --swarm-heavy` |

The two scope flags are mutually exclusive — passing both errors out
with usage guidance.

## `--ci-smoke` (fast minimal gate set)

Use when you want fast correctness signal without the multi-minute mesh
/ Tailscale / boundary-migration walk. Designed for swarm CI smoke and
agent pre-push readiness.

### Gates that run

- Forbidden Dependencies (cargo tree audit)
- Closure Linter + Verification Drift Guard
- Snapshot Proposal Guard
- Untracked Work Audit (advisory)
- Bridge Staleness Advisory
- Plan Drift Advisory
- Contract Drift Radar Advisory
- Fuzz Target Audit (static)
- Vision Coverage
- Unit, Contract, and Golden Tests (`cargo test --workspace --lib --bins --tests --examples`)
- Basic E2E Scripts (`scripts/e2e_test.sh`)

### Gates that are skipped (and logged as `SKIP ... (ci-smoke)`)

- Proof Verification (Lean4/TLA+ tooling)
- Agent Ergonomics E2E (F1-F5)
- Overhaul Integration E2E (J4) — already gated by `VERIFY_OVERHAUL`
- Swarm Next-Action Recommendation Cards E2E
- Graph Determinism E2E (F4.a)
- Fake Tailscale Harness (SRR6.46.10) and the SRR6.46.{1,2,6,12} mesh
  / hello / autodiscovery / responder lifecycle drivers
- Advanced E2E Scripts
- Boundary Migration Scripts
- `ee doctor` Safety Harness
- Pack-quality Eval Regression (`--eval` still works as an opt-in)
- Performance Benchmarks (`--include-bench` still works as an opt-in)

### When NOT to use `--ci-smoke`

- You changed mesh code, Tailscale discovery, the hello handshake, or
  the boundary-migration pipeline. Smoke skips exactly the surface
  you would be regressing.
- You touched `ee doctor` repair surfaces. Smoke skips the doctor
  safety harness.
- You are about to cut a release tag. Use the default profile or
  `--swarm-heavy`.

## `--swarm-heavy` (full Swarm-X / 64-agent verification)

Use when you want the full evidence trail required to feed the
bd-2dgn0 scorecard, including the optional benches and eval that the
default profile keeps gated behind explicit flags.

`--swarm-heavy` implies `--include-bench`, `--eval`, `--fuzz-smoke`,
and `--plan-doc-smoke` on top of every default-profile gate. There is
no separate "swarm heavy" set of new gates; the value of the flag is
**the bundle**: a single command that produces a complete scorecard
input artifact set on a large host.

### When to use `--swarm-heavy`

- Cutting a release.
- Closing a Swarm-X performance bead that cites scorecard evidence.
- Investigating an SLO regression report from the replay runner
  (bd-2dgn0.2 / bd-2dgn0.3).
- Periodic (weekly / pre-rotation) cross-feature regression sweeps.

### When NOT to use `--swarm-heavy`

- During inner-loop development. The benches and eval can take many
  minutes; default or `--ci-smoke` is the right answer for "did I
  break this?" feedback.
- On a host that does not match the documented Swarm-X reference
  hardware. Bench thresholds calibrated on the reference host will
  produce noisy advisory output everywhere else.

## Interpreting scorecard failures

The replay runner (bd-2dgn0.2), adapters (bd-2dgn0.4), and budgets
(bd-2dgn0.3) write a `ee.swarm_slo.scorecard.v1` artifact. When a gate
fails inside a `--swarm-heavy` run, the failure surface depends on
which stage produced it:

1. **`Forbidden Dependencies`, `Closure Linter`, `Vision Coverage`**
   — straight regressions. Fix the underlying code or bead, do not
   relax the gate.
2. **`Unit, Contract, and Golden Tests`** — the cargo workspace test
   gate. Investigate as usual; the swarm-heavy bundle does not change
   what cargo reports.
3. **`Performance Benchmarks` / `Eval Regression`** — these are the
   real Swarm-X SLO gates. A failure means a feature shipped that
   regressed an SLO budget. Capture the scorecard artifact path in the
   feature bead's evidence section, do not just retry the run.

A feature bead is **only** "closed with scorecard evidence" when the
full `--swarm-heavy` run is green AND the scorecard artifact is cited
in the bead's close comment. See AGENTS.md "Bead Taxonomy" — scorecard
evidence behaves like the `implements-surface:*` rule: it is a
post-condition for closing, not an alternative to it.

## RCH-only heavy verification

Per AGENTS.md "BUILDS — RCH ONLY", do not run the cargo-heavy gates
(`Unit, Contract, and Golden Tests`, `Performance Benchmarks`, `Eval
Regression`) under local fallback when remote workers are healthy.
`--swarm-heavy` is therefore an explicit "I am on the canonical host
and I want the heavy path" signal, not a license to fall back to
local Cargo when RCH refuses dispatch.

When RCH circuits are open or workers are unreachable, treat the
failure as a verification blocker and surface it in the relevant
bead. Do **not** run `--swarm-heavy` against local Cargo just because
remote dispatch refused.

## Citing scorecard evidence in beads

When closing a Swarm-X performance bead, include in the close comment:

- The exact command that produced the scorecard
  (`scripts/verify.sh --swarm-heavy` plus any env flags).
- The scorecard artifact path (the bd-2dgn0.3 budget summary line in
  the verify output points to it).
- The feature-specific focused proof (perf gate, golden snap, etc.)
  that the scorecard run also satisfied — the scorecard is
  cross-feature evidence, not a substitute for the focused proof.

Beads that try to cite scorecard evidence without a matching focused
proof should be reopened.

## See also

- `scripts/verify.sh --help`
- AGENTS.md → "CI/CD Pipeline" and "BUILDS — RCH ONLY"
- bd-2dgn0 (epic), bd-2dgn0.2/.3/.4/.5 (children)
