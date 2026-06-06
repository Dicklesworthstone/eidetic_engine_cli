# Coordination Snapshot

Schema: `ee.coordination_snapshot.v1`

Coordination snapshots let context packs include a deterministic, redacted view
of Beads and Agent Mail state without requiring live services during pack
assembly. Agents provide a JSON snapshot path; `ee` reads it side-effect free.

Example:

```bash
ee pack "next bead" --coordination-snapshot coordination.json --json | jq '.data.pack.coordination'
```

Related schemas: `ee.swarm.recommendation.v1`, `ee.trust_lane.v1`.

Non-goals: the snapshot is not a lock manager and does not send Agent Mail.

Tracking Bead: `bd-1zb7k.4`

## Agent Mail Snapshot Producer Contract

Tracking Bead: `bd-6qcwh.1`
Producer implementation: `scripts/agent_mail_snapshot.sh` (`bd-6qcwh.2`)
Producer schema tracking Bead: `bd-1ur7d.1`

`ee swarm brief --agent-mail-snapshot <path>` consumes a side-effect-free,
redacted Agent Mail snapshot with schema `ee.agent_mail.snapshot.v1`
(`docs/schemas/swarm/ee.agent_mail.snapshot.v1.json`). This is not the same
file as the pack-level `ee.coordination_snapshot.v1` snapshot above. The swarm
brief snapshot is a parser-compatible input that lets the brief report
reservations, active agents, unread mail counts, thread freshness, and Agent
Mail degradation without calling live Agent Mail tools from inside `ee`.

The snapshot producer is an external collector. It may call Agent Mail CLIs or
MCP resources before invoking `ee`, but the produced JSON must already be safe
for `ee` to read directly. `ee` treats the file as read-only evidence and
refuses unsafe inputs such as oversized files and symlinked snapshot paths.

The current consumer accepts these top-level arrays:

| Field | Aliases | Purpose |
| --- | --- | --- |
| `file_reservations` | `reservations` | Active Agent Mail file reservations. |
| `agents` | `agent_inventory`, `agentInventory` | Agent inventory and last-activity summary. |
| `inbox` | `mailboxes` | Per-mailbox unread and acknowledgement counts. |
| `threads` | none | Thread summaries and freshness. |

Full `ee.agent_mail.snapshot.v1` snapshots emit all four arrays. Empty arrays
are meaningful: they say the producer checked that class and found no rows.
Omitted arrays mean the class is unknown or the producer is older than this
contract.

Required top-level metadata:

| Field | Purpose |
| --- | --- |
| `schema` | Must be `ee.agent_mail.snapshot.v1` for the shipped producer. |
| `generated_at` | RFC 3339 production timestamp. |
| `project_key` | Redacted or workspace-relative project identifier. |
| `agent_name` | Agent mailbox used for inbox and thread summaries. |
| `summary` | Counts for agents, reservations, inbox mailboxes, threads, commands, and degraded records. |
| `source_commands` | Redacted list of read-only commands/resources used. |
| `redaction_status` | Producer redaction posture, for example `paths_counts_subjects_only_no_content`. |
| `fallback_active` | Agent Mail fallback state from the health probe. |
| `producer_status` | `ok` or `degraded`. |
| `command_statuses` | Redacted status for each read-only Agent Mail command. |
| `degraded` | Source degradation records when a class could not be checked. |

Optional legacy metadata still tolerated by consumers:

| Field | Purpose |
| --- | --- |
| `semantic_readiness` | Object or string describing semantic-readiness status. |
| `healthLevel` | Health classification used in semantic-readiness diagnostics. |

Unknown metadata is tolerated by current consumers for compatibility with older
snapshots, but the shipped producer emits the versioned schema and required
metadata above.

## Agent Mail Snapshot Fields

`file_reservations[]` entries:

| Field | Aliases | Redaction rule |
| --- | --- | --- |
| `path_pattern` | `path`, `pattern` | Workspace-relative path or glob only. |
| `holder` | `agent_name`, `agent`, `owner` | Agent identity only; no message body. |
| `exclusive` | none | Boolean. |
| `expires_ts` | `expires_at` | RFC 3339 timestamp when available. |

`agents[]` entries:

| Field | Aliases | Redaction rule |
| --- | --- | --- |
| `name` | `agent_name`, `agent`, `mailbox` | Agent identity only. |
| `last_active_at` | `lastActiveAt`, `last_active_ts`, `lastActiveTs` | RFC 3339 timestamp when available. |

`inbox[]` entries:

| Field | Aliases | Redaction rule |
| --- | --- | --- |
| `mailbox` | `agent_name`, `agent` | Mailbox or agent identity only. |
| `unread_count` | `unread` | Count only; no subjects or bodies. |
| `ack_required_count` | `ackRequired` | Count only. |

`threads[]` entries:

| Field | Aliases | Redaction rule |
| --- | --- | --- |
| `thread_id` | `threadId`, `id` | Stable thread identifier. |
| `subject` | none | Optional short subject after redaction. |
| `message_count` | `messageCount` | Count only. |
| `last_activity_at` | `lastActivityAt` | RFC 3339 timestamp when available. |

## Redaction Boundaries

The producer must not emit:

- raw message bodies, markdown bodies, attachment contents, or attachment paths;
- raw Agent Mail archive paths or database paths;
- raw stdout/stderr from Agent Mail commands;
- secrets, tokens, API keys, credentials, or environment dumps;
- absolute filesystem paths outside the current workspace;
- full Beads issue bodies or comments copied through Agent Mail.

Allowed evidence is limited to stable identifiers, counts, bounded timestamps,
workspace-relative path patterns, lock exclusivity, freshness, and short
redacted subjects. If a field cannot be redacted confidently, omit it and add a
degraded note through the health or semantic-readiness fields.

## Mutation Boundary

Snapshot collection must be read-only. Allowed sources include read-only Agent
Mail inventory, reservation, inbox-count, and thread-summary calls. Inbox calls
must be body-free, must not pass `--include-bodies`, and must not mark messages
read.

```bash
am agents list --project "$PWD" --json
am file_reservations list "$PWD" --active-only
am robot reservations --project "$PWD" --all --format json
am robot status --project "$PWD" --format json
am mail inbox --project "$PWD" --agent "$AGENT_NAME" --limit 20 --json
```

`scripts/swarm_coordination_health.sh` is not a full snapshot producer because
it can run smoke checks, including send checks. Its
`ee.swarm.coordination_health.v1` output may be merged into snapshot diagnostics
as health evidence, but the full producer must not rely on that script as the
source for reservations, inbox counts, agent roster, or thread freshness unless
a future no-send snapshot mode is added and tested.

Forbidden during snapshot production:

- sending, replying to, acknowledging, or marking Agent Mail messages read;
- acquiring, renewing, releasing, or force-releasing file reservations;
- running `am doctor repair` or any repair command;
- mutating Beads status, comments, dependencies, or priorities;
- reading raw mailbox archives as a shortcut around redacted APIs;
- deleting, cleaning, stashing, rebasing, or rewriting Git state.

The producer should record the read-only commands or MCP resources it used in
`source_commands`, with sensitive arguments redacted. `ee swarm brief` does not
trust that field for safety; no-mutation tests must prove the producer's
behavior.

## Shipped Producer

Generate a full snapshot with:

```bash
SNAPSHOT_PATH=/private/tmp/ee-agent-mail-snapshot.json
COORDINATION_PATH=/private/tmp/ee-coordination-snapshot.json
scripts/agent_mail_snapshot.sh \
  --project "$PWD" \
  --agent "$AGENT_NAME" \
  --output "$SNAPSHOT_PATH" \
  --coordination-output "$COORDINATION_PATH"
```

Use a canonical, non-symlink snapshot path. On macOS, `/tmp` is usually a
symlink to `/private/tmp`, and `ee swarm brief --agent-mail-snapshot /tmp/...`
refuses that path before reading the file.

`$SNAPSHOT_PATH` is the full Agent Mail snapshot consumed by swarm brief and
workspace hygiene. `$COORDINATION_PATH` is the companion
`ee.coordination_snapshot.v1` artifact consumed by context-pack surfaces.

Useful producer options:

| Option | Purpose |
| --- | --- |
| `--project <path>` | Agent Mail project/workspace path. Defaults to `AGENT_MAIL_PROJECT` or the current directory. |
| `--agent <name>` | Mailbox used for inbox/thread summaries. Defaults to `AGENT_MAIL_AGENT` or `AGENT_NAME`. |
| `--am-bin <path>` | Agent Mail CLI binary. Defaults to `AGENT_MAIL_AM_BIN` or `am`. |
| `--inbox-limit <n>` | Maximum inbox rows to read for count and thread projection. |
| `--thread-limit <n>` | Maximum thread summaries emitted. |
| `--timeout-sec <n>` | Per-command timeout; failures are emitted as degraded source entries. |
| `--json` | Emit full `ee.agent_mail.snapshot.v1` JSON to stdout. |
| `--stdout` | Alias for `--json`. |
| `--output <path>` | Write the full snapshot JSON to this path; quiet unless `--json`/`--stdout` is also set. |
| `--coordination-output <path>` | Also write a pack-compatible `ee.coordination_snapshot.v1` companion JSON file. |

The producer currently calls only read-only Agent Mail commands:

```bash
am agents list --project <workspace> --json
am robot reservations --project <workspace> --all --format json
am mail inbox --project <workspace> --agent <agent> --limit <n> --json
```

It intentionally does not call `scripts/swarm_coordination_health.sh`, because
that script can run send smoke checks. Health events remain useful as degraded
transport evidence; they are not full reservation, roster, inbox, or thread
snapshots.

Pass the generated file to consumers:

```bash
ee swarm brief --workspace . --agent-mail-snapshot "$SNAPSHOT_PATH" --json
ee workspace hygiene --workspace . --agent-name "$AGENT_NAME" \
  --agent-mail-snapshot "$SNAPSHOT_PATH" --json
ee pack "next bead" --workspace . \
  --coordination-snapshot "$COORDINATION_PATH" --json
```

Do not pass the full Agent Mail snapshot to `--coordination-snapshot`: pack
coordination requires the companion `ee.coordination_snapshot.v1` shape with
`sources[]`. Conversely, do not pass the companion artifact to
`--agent-mail-snapshot`: swarm brief and workspace hygiene expect the full
Agent Mail arrays.

## Snapshot-Backed Claim Gates

`ee swarm work-packet --claim-gate` can also consume the full
`ee.agent_mail.snapshot.v1` file through `--agent-mail-snapshot`. Use this when
the first gate reports `agent_mail_unavailable` or a non-authoritative
`sourceAuthority.agentMailStatus`:

```bash
CANDIDATE=bd-example.1
ee swarm work-packet --workspace . --include-rch \
  --agent-mail-snapshot "$SNAPSHOT_PATH" \
  --claim-gate --candidate "$CANDIDATE" --json \
  | jq '.data | {schema, verdict, safeToClaim, agentMailStatus: .sourceAuthority.agentMailStatus, unsafeReasons, degradedCodes}'
```

The snapshot only upgrades the evidence source from unknown to observed. It is
read-only and never authorizes Beads claim, close, reservation, or Git mutation
on its own. Agents may claim only when the retry still has
`schema=ee.swarm.work_packet.claim_gate.v1`, `safeToClaim=true`,
`verdict=safe_to_claim`, authoritative reservation and inbox flags, and a
structured `claimCommandAction`. Any remaining active reservation, stale
tracker, Beads/BV disagreement, or RCH blocker in `unsafeReasons`,
`staleReasons`, or `degradedCodes` requires coordination instead of claiming.

## Examples

Healthy full snapshot:

```json
{
  "schema": "ee.agent_mail.snapshot.v1",
  "generated_at": "2026-06-04T18:20:00Z",
  "project_key": "<workspace>",
  "agent_name": "BeigeHollow",
  "redaction_status": "paths_counts_subjects_only_no_content",
  "source_commands": [
    "am agents list --project '<workspace>' --json",
    "am robot reservations --project '<workspace>' --all --format json",
    "am mail inbox --project '<workspace>' --agent BeigeHollow --limit 20 --json"
  ],
  "producer_status": "ok",
  "fallback_active": false,
  "summary": {
    "agent_count": 1,
    "file_reservation_count": 1,
    "inbox_mailbox_count": 1,
    "thread_count": 1,
    "source_command_count": 3,
    "degraded_count": 0
  },
  "file_reservations": [
    {
      "path_pattern": "docs/swarm/coordination_snapshot.md",
      "holder": "BeigeHollow",
      "exclusive": true,
      "expires_ts": "2026-06-04T23:34:27Z"
    }
  ],
  "agents": [
    {
      "name": "BeigeHollow",
      "last_active_ts": "2026-06-04T18:19:30Z"
    }
  ],
  "inbox": [
    {
      "mailbox": "BeigeHollow",
      "unread_count": 0,
      "ack_required_count": 0
    }
  ],
  "threads": [
    {
      "thread_id": "bd-6qcwh.1",
      "subject": "Define redacted Agent Mail snapshot producer contract",
      "message_count": 4,
      "last_activity_at": "2026-06-04T18:19:45Z"
    }
  ]
}
```

Health-only fallback event:

```json
{
  "schema": "ee.swarm.coordination_health.v1",
  "mcp_http_reachable": false,
  "am_agents_list_ok": true,
  "am_send_single_recipient_ok": true,
  "am_send_multi_recipient_ok": false,
  "observed_panic": "RefCell already borrowed",
  "fallback_active": true
}
```

This event is health evidence only. It can explain degraded Agent Mail
transport, but it does not provide reservations, agent inventory, unread counts,
or thread freshness.

Stale snapshot:

```json
{
  "schema": "ee.agent_mail.snapshot.v1",
  "generated_at": "2026-06-04T12:00:00Z",
  "project_key": "<workspace>",
  "agent_name": "BeigeHollow",
  "redaction_status": "paths_counts_subjects_only_no_content",
  "producer_status": "ok",
  "source_commands": [],
  "command_statuses": [],
  "fallback_active": false,
  "am_agents_list_ok": true,
  "summary": {
    "agent_count": 0,
    "file_reservation_count": 0,
    "inbox_mailbox_count": 0,
    "thread_count": 0,
    "source_command_count": 0,
    "degraded_count": 0
  },
  "degraded": [],
  "file_reservations": [],
  "agents": [],
  "inbox": [],
  "threads": []
}
```

Consumers compare `generated_at` or source freshness against their own staleness
policy. A stale empty snapshot still means "checked earlier and found none",
not "checked just now".

Reservation conflict:

```json
{
  "schema": "ee.agent_mail.snapshot.v1",
  "generated_at": "2026-06-04T18:25:00Z",
  "project_key": "<workspace>",
  "agent_name": "BeigeHollow",
  "redaction_status": "paths_counts_subjects_only_no_content",
  "producer_status": "ok",
  "source_commands": [],
  "command_statuses": [],
  "fallback_active": false,
  "am_agents_list_ok": true,
  "summary": {
    "agent_count": 0,
    "file_reservation_count": 1,
    "inbox_mailbox_count": 0,
    "thread_count": 0,
    "source_command_count": 0,
    "degraded_count": 0
  },
  "degraded": [],
  "file_reservations": [
    {
      "path_pattern": "src/core/swarm_brief.rs",
      "holder": "GoldenGate",
      "exclusive": true,
      "expires_ts": "2026-06-04T23:00:00Z"
    }
  ],
  "agents": [],
  "inbox": [],
  "threads": []
}
```

Inbox unavailable:

```json
{
  "schema": "ee.agent_mail.snapshot.v1",
  "generated_at": "2026-06-04T18:26:00Z",
  "project_key": "<workspace>",
  "agent_name": "BeigeHollow",
  "redaction_status": "paths_counts_subjects_only_no_content",
  "producer_status": "degraded",
  "source_commands": [],
  "command_statuses": [],
  "fallback_active": true,
  "am_agents_list_ok": true,
  "summary": {
    "agent_count": 1,
    "file_reservation_count": 0,
    "inbox_mailbox_count": 0,
    "thread_count": 0,
    "source_command_count": 0,
    "degraded_count": 1
  },
  "degraded": [
    {
      "code": "agent_mail_snapshot_source_unavailable",
      "severity": "warning",
      "source": "agent_mail",
      "command": "am mail inbox --project '<workspace>' --agent BeigeHollow --limit 20 --json",
      "error_class": "timeout",
      "exit_code": null,
      "timed_out": true
    }
  ],
  "file_reservations": [],
  "agents": [
    {
      "name": "GoldenGate",
      "lastActiveAt": "2026-06-04T18:20:00Z"
    }
  ],
  "inbox": [],
  "threads": []
}
```

The empty `inbox` array is not proof of zero unread messages when
`fallback_active` is true; it means inbox freshness is degraded.

Semantic readiness failed:

```json
{
  "schema": "ee.swarm.coordination_health.v1",
  "healthLevel": "green",
  "semantic_readiness": {
    "status": "fail",
    "reason": "database disk image is malformed at page 283 in <redacted>"
  }
}
```

The consumer classifies the reason without surfacing raw database paths or page
details.

## Test Requirements

Implementation beads for the producer and parser must include fixtures for:

- absent `--agent-mail-snapshot`;
- health-only `ee.swarm.coordination_health.v1`;
- healthy full snapshot with all four arrays;
- stale snapshot;
- exclusive reservation conflict;
- inbox unavailable while fallback is active;
- semantic-readiness failure;
- malformed JSON, oversized file, and symlinked snapshot paths;
- redaction of bodies, attachments, secrets, absolute archive paths, and raw
  command output.

Contract tests must also prove:

- stdout is machine JSON only and stderr stays empty for `--json` success paths;
- snapshot collection does not mutate Agent Mail read/ack state, reservations,
  Beads records, Git state, EE DB rows, or support-bundle files;
- health-only input never appears as full reservation, inbox, or thread
  evidence;
- remote Cargo verification is used for Rust parser tests when this repo's RCH
  policy requires it.
