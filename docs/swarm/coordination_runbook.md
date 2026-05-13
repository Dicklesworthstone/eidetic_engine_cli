# Swarm Coordination Runbook

Use this runbook when Agent Mail is down, partially unavailable, or returning
panic output from the `am` CLI.

## Triage

1. Run `scripts/swarm_coordination_health.sh`.
2. If `fallback_active` is `true`, coordinate through Beads.
3. If only `am_send_multi_recipient_ok` is false, direct one-recipient sends may
   still be usable.
4. If `mcp_http_reachable` is false, do not rely on MCP Agent Mail tools,
   resource reads, or file reservations.

## Fallback Workflow

For each active task:

1. Set the Bead to `in_progress`.
2. Add a Beads comment naming the files or modules you intend to touch.
3. Keep progress and blockers in the same Bead thread.
4. Before editing a surface another active Bead mentions, add a comment and wait
   for a reply when practical.
5. Close the Bead with verification evidence and run `br sync --flush-only`.

When you need to broadcast a coordination change:

```bash
br comments add <bead-id> --message "Coordination: <state change>"
```

For durable handoff between agents, prefer:

```bash
br show <bead-id> --json
git log --oneline --decorate -n 20
git status --short
```

## Interpreting Missing Sources

Missing Agent Mail data means the coordination source is unavailable. It does
not mean there are no reservations, no messages, or no active owners.

When a context pack or swarm brief reports `agent_mail_unavailable`, treat the
coordination confidence as degraded and verify with Beads comments before
making overlapping edits.

## Returning To Normal

Agent Mail is considered healthy when:

- `mcp_http_reachable` is true.
- `am_agents_list_ok` is true.
- `am_send_single_recipient_ok` is true.
- `am_send_multi_recipient_ok` is true.
- `fallback_active` is false.

After recovery, keep using Beads as the durable task ledger and Agent Mail as
the fast coordination channel.
