# Agent Integration

`scripts/agent_consume_pack.py` is the reference consumer for `ee pack --json`.
It reads a pack response from stdin, prefers `data.pack.text` when present,
and falls back to rendering `data.pack.items[]` into a prompt fragment.

Example:

```bash
ee pack "prepare release" --workspace . --max-tokens 1000 --json \
  | scripts/agent_consume_pack.py --from-stdin
```

The contract check lives at `scripts/e2e_overhaul/agent_consumer.sh`.

## Swarm Brief Field Projection Guard

Use `ee swarm brief` as a read-only preflight before choosing work in a shared
checkout. The compact summary projection is documented in both local and global
flag positions:

```bash
ee swarm brief --fields summary --workspace . --json
ee --fields summary swarm brief --workspace . --json
```

If either command returns an `ee.error.v2` usage failure such as
`usage_unknown_field`, and `error.details.presetsAvailable` still lists
`summary`, treat the installed binary as stale relative to the current
source/docs contract. Fall back to `ee swarm brief --workspace . --json` only
for read-only inspection. That fallback does not authorize Beads mutation:
claim work only after the work-packet claim gate succeeds, and coordinate for
an approved RCH/release-path rebuild if compact field projection is required.

## Work-Packet Claim Gate

In shared checkouts, use Beads and BV to identify candidates, but do not treat
their copy-paste claim commands as authority to mutate state. Before claiming a
bead or reserving edit paths, ask `ee` for the read-only work-packet claim
gate:

```bash
ee swarm work-packet --workspace . --include-rch --claim-gate --candidate <id> --json \
  | scripts/agent_consume_work_packet_gate.py
```

If the installed `ee` rejects `--claim-gate` or `--candidate` as an unexpected
argument, treat that binary as stale relative to the current source/docs
contract. Stop at inspection, coordinate for an approved RCH/release-path
rebuild, and do not run a BV claim command or local Cargo install as a
workaround.

The consumer emits `ee.agent.work_packet_gate_decision.v1` with
`safeToClaim`, `candidateId`, `decision`, `argvActions`,
`mutatingActionsRequireHuman`, `whyNotSafe`, and source/degraded summaries.
Harnesses may only run a mutating claim action when the decision reports
`safeToClaim=true` and the claim action is `runnable=true`. Every other
verdict is inspection-only: coordinate through Agent Mail or Beads comments,
preserve exact RCH blockers, and do not substitute local Cargo proof.

The gate and consumer are intentionally read-only. They must not claim Beads,
reserve files, send Agent Mail, mutate git, run Cargo, or launch RCH. Prefer
structured `argvActions[].argv` over display strings; never shell-parse
`commandTemplate`, `suggestedCommands`, or `displayCommand`.

The fixture-driven consumer check is:

```bash
python3 scripts/agent_consume_work_packet_gate_test.py
```

For shared-checkout commit readiness, see
[`docs/agent-ux/workspace-hygiene.md`](agent-ux/workspace-hygiene.md). The
workspace hygiene surface is read-only and explains dirty-path buckets,
reason codes, and scratch-artifact examples for agent commits.

Before committing or pushing from a shared checkout, run:

```bash
ee hook git-readiness --workspace . --agent-name <AgentName> --json
```

This read-only diagnostic reports schema `ee.hooks.git_readiness.v1` and
checks local Git hooks for agent identity requirements, legacy Beads metadata
mutation, local Cargo hooks that should route through RCH, unreadable hook-chain
targets, and missing preflight-guard coverage.

The no-build e2e harness for this diagnostic is
`scripts/e2e_overhaul/hook_git_readiness.sh`. It creates real temporary Git
repositories, requires `EE_BINARY` or an already-built `ee` binary, writes
`ee.test_event.v1` JSONL, and retains its temporary repositories and event log
for audit instead of deleting them. The harness must not run Cargo.

For remote Rust proof handoffs, see [`docs/rch_verification.md`](rch_verification.md)
and [`docs/rch_runbook.md`](rch_runbook.md). Agent-to-agent messages should name
the RCH proof status and source attribution explicitly:

- `strict_clean_tree` means the remote proof came from a clean checkout.
- `local_checkout_observed_remote_source_unknown` means the wrapper fingerprinted
  the local checkout but did not materialize that source for the remote command;
  include `dirty_status_hash`, `remote_source_materialized`, and relevant
  `dirty_paths_sample`.
- `source_state_refused` means the wrapper refused before RCH because strict
  proof would be ambiguous.
- `committed_tree_unsupported` means the committed source manifest was computed,
  but remote Cargo did not run from that manifest yet.

Do not translate these states into "verified" or "failed" without the qualifier.
They are attribution states, and they do not authorize local Cargo fallback,
stash/reset/checkout/worktree operations, or cleanup of another agent's files.

## Landing producer-derived memories

When an external producer (reflection ingest, `ee review session --propose`,
vendor pipelines) needs to land a *new* memory derived from existing sources,
use the `create_derived_memory` candidate flow:

```
ee curate propose-derived ... → ee curate show <id> → ee curate validate <id>
                              → ee curate apply <id> → ee why <createdMemoryId>
```

See [`docs/external-derivation-operator.md`](external-derivation-operator.md)
for the safe command order, source-ref shapes, trust-class default,
target-mutating vs create-derived contract differences, and the failure-mode
catalog. The operator doc is authoritative for agents writing producer
harnesses; [ADR 0043](adr/0043-external-derivation-candidates.md) covers the
design context.
