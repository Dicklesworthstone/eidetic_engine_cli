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

## Full Snapshot

When the brief must include reservations, unread counts, or thread freshness,
generate a full redacted snapshot first:

```bash
scripts/agent_mail_snapshot.sh \
  --project "$PWD" \
  --agent "$AGENT_NAME" \
  --json | jq .
```

The `--json` flag writes the full snapshot to stdout. To keep a durable file
for consumers, use `--output`; plain `--output` writes only the file, while
`--json --output` writes the same `ee.agent_mail.snapshot.v1` snapshot to both
stdout and the file.

```bash
SNAPSHOT_PATH=/private/tmp/ee-agent-mail-snapshot.json
COORDINATION_PATH=/private/tmp/ee-coordination-snapshot.json
scripts/agent_mail_snapshot.sh \
  --project "$PWD" \
  --agent "$AGENT_NAME" \
  --output "$SNAPSHOT_PATH" \
  --coordination-output "$COORDINATION_PATH"
```

Use a canonical, non-symlink path for `SNAPSHOT_PATH`. On macOS,
`ee swarm brief --agent-mail-snapshot /tmp/...` is refused because `/tmp` is a
symlink; use `/private/tmp/...` or another resolved path.

Then pass each artifact to the matching consumer:

```bash
ee swarm brief --workspace . --agent-mail-snapshot "$SNAPSHOT_PATH" --json
ee workspace hygiene --workspace . --agent-name "$AGENT_NAME" \
  --agent-mail-snapshot "$SNAPSHOT_PATH" --json
ee pack "next bead" --workspace . \
  --coordination-snapshot "$COORDINATION_PATH" --json
```

The snapshot follows the producer contract in
`docs/swarm/coordination_snapshot.md`: it includes the `file_reservations`,
`agents`, `inbox`, and `threads` arrays, using empty arrays only for classes the
producer actually checked, and carries schema `ee.agent_mail.snapshot.v1`. The
producer never sends mail, acknowledges messages, marks read state, creates or
releases reservations, mutates Beads, or runs the health smoke-test script.

The producer uses six bounded read-only sources: agent inventory, active
reservations, body-free inbox rows, `am status`, `/health`, and
`/health/durability`. `am status` is the sole authority for unread and
acknowledgement counts; inbox rows only shape `threads[]`. This avoids treating
already-read messages returned for thread context as unread. Both health
endpoints are required because `/health` intentionally reports readiness with
`durability_state=not_probed`, while `/health/durability` reports the durable
read/write posture.

Status counts must be genuine integers in the unsigned 64-bit wire range.
Boolean, negative, string, missing, and oversized values fail closed instead
of being normalized to zero by a downstream consumer.

### Structural and semantic validation

Passing `docs/schemas/swarm/ee.agent_mail.snapshot.v1.json` is necessary
structural validation, but it is not sufficient authority for a claim or
coordination decision. Likewise, piping producer output through `jq .` only
proves that it is parseable JSON. Authoritative consumers **MUST** also run the
strict declared-v1 semantic validator. The shipped `ee swarm brief`,
`ee swarm work-packet`, and `ee workspace hygiene` snapshot paths do this
automatically before they trust reservation or inbox evidence.

The semantic pass verifies cross-field invariants that draft-07 JSON Schema
cannot fully express:

- the six source commands have the required order and identities, the first
  four share one redacted Agent Mail executable prefix, and the inbox/status
  commands bind to the top-level `agent_name`;
- each `command_statuses[]` entry repeats the corresponding
  `source_commands[]` value, successful commands carry the expected CLI or
  HTTP status, and failed commands carry bounded failure metadata;
- `am_agents_list_ok`, `fallback_active`, `producer_status`, readiness,
  recovery, and durability agree with the command results;
- `summary` counts equal the normalized array lengths, every failed command
  has exactly one matching `degraded[]` entry, and failed sources contribute no
  normalized rows; and
- a successful status probe contributes exactly the mailbox named by
  `agent_name`.

Workspace binding and snapshot freshness are additional authority checks, not
substitutes for the semantic pass. If any structural, semantic, binding, or
freshness check fails, treat Agent Mail as unavailable or degraded; never
interpret empty arrays from that snapshot as proof that no reservation or
unread mail exists.

Agent, reservation, and inbox responses must expose one unambiguous recognized
collection shape, and every returned row must carry consistent required
identity fields. The producer rejects conflicting aliases and entire partially
malformed collections instead of silently dropping a row that might represent
an exclusive reservation or active thread.

The dedicated durability response must include a non-empty state and actual
boolean `allows_reads`/`allows_writes` values. Missing or malformed fields fail
closed. When readiness and durability disagree, the stricter bounded posture
wins; a healthy durability response cannot erase a corrupt readiness/recovery
signal. Conversely, readiness `not_probed` plus a valid healthy durability
response is healthy. `fallback_active` and `producer_status` reflect this
combined posture, while `degraded[]` remains limited to source command failures
and invalid command responses.

The companion coordination file is the pack-compatible
`ee.coordination_snapshot.v1` projection over the same redacted Agent Mail
evidence. Use it only with `--coordination-snapshot`; swarm brief and workspace
hygiene still consume the full Agent Mail snapshot.

## Claim-Gate Bridge

Use the snapshot bridge when `ee swarm work-packet --claim-gate` cannot observe
Agent Mail and reports `agent_mail_unavailable`. First run the gate for the
exact candidate without changing Beads:

```bash
CANDIDATE=bd-example.1
ee swarm work-packet --workspace . --include-rch \
  --claim-gate --candidate "$CANDIDATE" --json \
  | jq '.data | {schema, verdict, safeToClaim, agentMailStatus: .sourceAuthority.agentMailStatus, unsafeReasons, degradedCodes}'
```

If the response schema is `ee.swarm.work_packet.claim_gate.v1` and
`safeToClaim` is `false` because `degradedCodes` contains
`agent_mail_unavailable` or `sourceAuthority.agentMailStatus` is
`unavailable`, `skipped`, or `degraded_read_only`, generate a redacted
`ee.agent_mail.snapshot.v1` snapshot and retry the same gate:

```bash
SNAPSHOT_PATH=/private/tmp/ee-agent-mail-snapshot.json
scripts/agent_mail_snapshot.sh \
  --project "$PWD" \
  --agent "$AGENT_NAME" \
  --output "$SNAPSHOT_PATH"

ee swarm work-packet --workspace . --include-rch \
  --agent-mail-snapshot "$SNAPSHOT_PATH" \
  --claim-gate --candidate "$CANDIDATE" --json \
  | jq '.data | {schema, verdict, safeToClaim, agentMailStatus: .sourceAuthority.agentMailStatus, unsafeReasons, degradedCodes}'
```

Interpret `sourceAuthority.agentMailStatus` of `fresh` or `healthy` plus
authoritative reservation/inbox flags as current snapshot evidence. It is not
permission to claim by itself: `safeToClaim=true`, `verdict=safe_to_claim`, and
a non-null `claimCommandAction` must still be present. If `unsafeReasons` still
name active reservations, stale tracker state, BV disagreement, or RCH proof
blockers, coordinate through Agent Mail or Beads comments instead of claiming.
If the snapshot has `fallback_active=true`, do not infer authority from empty
arrays or a transport-green `/health` response. Inspect the bounded recovery or
durability fields and the command statuses, repair the failing source, then
regenerate the snapshot.

Useful full-snapshot environment overrides:

```bash
AGENT_MAIL_PROJECT=/path/to/repo
AGENT_MAIL_AGENT=AgentName
AGENT_NAME=AgentName
AGENT_MAIL_AM_BIN=am
```

Useful health-check environment overrides:

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
