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

For remote Rust proof handoffs, see [`docs/rch_verification.md`](rch_verification.md)
and [`docs/rch_runbook.md`](rch_runbook.md). Agent-to-agent messages should name
the RCH proof status and source attribution explicitly:

- `strict_clean_tree` means the remote proof came from a clean checkout.
- `live_dirty_checkout` means the remote proof included the current shared
  checkout state; include `dirty_status_hash` and relevant `dirty_paths_sample`.
- `source_state_refused` means the wrapper refused before RCH because strict
  proof would be ambiguous.
- `committed_tree_unsupported` means the committed source manifest was computed,
  but remote Cargo did not run from that manifest yet.

Do not translate these states into "verified" or "failed" without the qualifier.
They are attribution states, and they do not authorize local Cargo fallback,
stash/reset/checkout/worktree operations, or cleanup of another agent's files.
