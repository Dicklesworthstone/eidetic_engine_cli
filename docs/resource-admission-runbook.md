# Resource-admission runbook (operators + swarm agents)

bd-u7f9q / bd-7engw. How to use `ee`'s resource-admission advice on anything
from a constrained laptop to a 256 GB / 64-core swarm host — and exactly what
the surface does NOT do.

## The one fact that shapes everything

The admission policy is **advisory and side-effect-free**. It never schedules
agents, cancels RCH jobs, mutates config, deletes files, or persists its
decision anywhere. `ee diag resource-admission` is a **pure calculator** over
the inputs you hand it, and the swarm work-packet surface runs the same
calculator over its own gathered evidence. If you want the decision recorded,
paste it into the bead/mail yourself (wording below) — there is no decision
ledger yet (see "What does not exist yet").

## The diagnostic surface

```bash
# Evaluate a proposed heavy verification on this host, explicitly:
ee diag resource-admission \
    --surface verification \
    --command-class diagnostic \
    --effective-profile workstation \
    --estimated-cost-class standard \
    --json
```

- `--surface`: `search | pack | cache | write-spool | steward | verification | diagnostics | graph`
- `--effective-profile` / `--requested-profile`: `constrained | portable | workstation | swarm`
- Every input you do not supply is evaluated at its stated default — the
  calculator does not probe the host behind your back. Missing/contradictory
  evidence produces an honest `abstain`, never a guess.

## Reading a decision

`decision` is one of:

| Decision | Meaning | Your move |
|---|---|---|
| `admit` | evidence supports running the workload here | proceed |
| `degrade_to_lean` | run it, but with lean output/budget presets | add `--fields summary`, lower limits |
| `queue` | proof lanes/slots are occupied | wait; re-check with `rch status` |
| `wait_for_rch` | remote verification is the bottleneck, not this host | watch the lane, do not fall back to local Cargo |
| `split_workload` | too big for one pass | shard the target set |
| `refuse_local_cargo` | local Cargo would violate the RCH-only rule | route through `scripts/rch_verify.sh` |
| `abstain` | insufficient or contradictory evidence | refresh evidence (below), rerun |

`reasonCodes` carry the WHY in stable snake_case (e.g.
`stale_source_authority`, `redaction_posture_unknown`,
`missing_required_signal`); `nextCommands` carry exact non-destructive
follow-ups. Trust the codes, not prose.

## Profile examples

- **Constrained (laptop, <16 GB)**: expect `degrade_to_lean` for pack/search
  bulk work and `refuse_local_cargo` for verification. Honor lean presets —
  the 156 KB default swarm brief is exactly what this profile exists to
  prevent.
- **Workstation (default)**: `admit` for standard cost classes; heavy
  verification still routes to RCH (`wait_for_rch`/`queue` when lanes are
  hot).
- **Swarm (256 GB+/64-core shared host)**: high parallelism makes WRITE
  pressure, not CPU, the binding constraint. Expect `queue`/`split_workload`
  advice under checkout churn; cap concurrent editors before trusting
  `admit` (see `docs/rch_runbook.md` for the fleet-side discipline).

## Composing with the rest of the system

- **Work packets**: `ee swarm work-packet --claim-gate ...` embeds the same
  admission evaluation; a claim-gate refusal with admission reasons is NOT a
  source failure — do not "fix" code in response, refresh evidence instead.
- **RCH-only proof**: any Rust verification the advice touches runs remotely
  (`scripts/rch_verify.sh --pinned-franken-stack ...`). If RCH refuses before
  Cargo, preserve the exact blocker strings in the bead; that is evidence,
  not noise.
- **Evidence refresh (all non-destructive)**:
  ```bash
  ee status --workspace . --json          # generations + degraded posture
  ee doctor --workspace . --json          # repair commands, never auto-run
  rch status                              # fleet health, slots, pressure
  ee diag environment-attestation --workspace . --include-rch --json
  ```
- **Handoff wording** (paste into Beads/Agent Mail with the JSON attached):
  > resource-admission: decision=<decision> reasons=<reasonCodes> at
  > <timestamp>; evidence refreshed via status/doctor/rch-status; no local
  > Cargo run; not a source failure.

## What does not exist yet (do not claim it in handoffs)

- **No persisted decision ledger** — every evaluation is fresh; "the latest
  admission decision" is not a queryable thing (bd-2sw3h holds the design
  for persisting one; bd-u7f9q.2 holds the hysteresis controller that would
  consume it).
- **No replay-calibrated thermostat** — recommendations do not yet carry
  stability/confidence over an evidence window; treat borderline decisions
  as one-shot advice, not trends (bd-u7f9q.2).
- **No pressure fixtures** — the conformance matrix for admit/degrade/queue
  under synthetic pressure is bd-2b95k, still open.

The safety rails hold regardless: nothing in this surface performs
destructive cleanup, worktree creation, stashing, or local Cargo fallback.
