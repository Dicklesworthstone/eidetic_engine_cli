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
