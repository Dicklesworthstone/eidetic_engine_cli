# Agent Mail Fallback

Agent Mail is the preferred coordination channel for identities, inboxes, and
file reservations. When it is unavailable, `ee` work continues through Beads
and explicit handoff notes rather than implicit chat context.

## Known Failure

Observed on May 12, 2026: multi-recipient `am mail send` can panic in
`mcp_agent_mail_rust` with:

```text
RefCell already borrowed
```

The MCP HTTP transport at `http://127.0.0.1:8765/mcp/` has also been observed
unreachable. During that outage, Agent Mail reservations and broadcasts are not
reliable coordination evidence.

## Fallback Policy

Use this order while Agent Mail is degraded:

1. Record task ownership and progress on the Bead with `br comments add` or
   `br update`.
2. Use one Bead per work surface and include the Bead ID in status comments,
   verification notes, and commit messages.
3. If the `am` CLI is usable for single-recipient sends, send direct messages
   one recipient at a time.
4. Treat missing Agent Mail data as unknown, not empty. Do not assume a file is
   unreserved just because the Agent Mail source is unavailable.

Avoid broad edits when Agent Mail is down unless Beads show no active owner for
the same surface or the coordination risk is explicitly documented.

## Health Check

Run:

```bash
scripts/swarm_coordination_health.sh
```

The script emits `ee.swarm.coordination_health.v1` JSON with MCP reachability,
`am agents list`, single-recipient send, multi-recipient send, and fallback
status. It is safe to run during an outage; failures are reported as fields in
the JSON event.

`ee swarm brief` does not call live Agent Mail MCP tools. Without
`--agent-mail-snapshot`, it only performs a tiny bounded probe of
`127.0.0.1:8765/health` so the degraded message can say whether the local health
endpoint appears reachable.

The output from `scripts/swarm_coordination_health.sh` is health evidence only.
It uses schema `ee.swarm.coordination_health.v1` and can explain degraded
transport or semantic-readiness failures, but it does not contain reservations,
agent inventory, unread counts, or thread freshness. Do not treat it as a full
Agent Mail snapshot.

When the brief must include reservations, unread counts, or thread freshness,
pass a redacted Agent Mail snapshot with `--agent-mail-snapshot <path>`. The
snapshot must follow the producer contract in
`docs/swarm/coordination_snapshot.md`: it should include the
`file_reservations`, `agents`, `inbox`, and `threads` arrays, using empty arrays
only for classes the producer actually checked.

Useful environment overrides:

```bash
AGENT_MAIL_PROJECT=/path/to/repo
AGENT_MAIL_FROM=AgentName
AGENT_MAIL_SINGLE_TO=AgentName
AGENT_MAIL_MULTI_TO=AgentA,AgentB
AGENT_MAIL_HEALTH_URL=http://127.0.0.1:8765/health
AGENT_MAIL_AM_BIN=am
```

## Recovery

When the upstream Agent Mail bug is fixed, confirm all checks are green:

```bash
scripts/swarm_coordination_health.sh | jq .
```

Then send a real multi-recipient smoke message:

```bash
am mail send --project "$PWD" --from "$AGENT_NAME" \
  --to "AgentA,AgentB" --subject "Agent Mail smoke" --body "ping" --json
```

Once that succeeds, Agent Mail can again be treated as the primary
coordination channel. Keep Beads comments for durable audit trail even after
Agent Mail recovers.
