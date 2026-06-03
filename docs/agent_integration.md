# Agent Integration

`scripts/agent_consume_pack.py` is the reference consumer for `ee context --json`.
It reads a context response from stdin, prefers `data.pack.text` when present,
and falls back to rendering `data.pack.items[]` into a prompt fragment.

Example:

```bash
ee context "prepare release" --workspace . --max-tokens 1000 --json \
  | scripts/agent_consume_pack.py --from-stdin
```

The contract check lives at `scripts/e2e_overhaul/agent_consumer.sh`.

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
