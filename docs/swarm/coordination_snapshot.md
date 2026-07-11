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
| `project_key` | `sha256:<64 lowercase hex>` binding to the physical canonical UTF-8 workspace identity. It is redaction-safe and lets consumers reject a snapshot produced for another workspace. The producer recovers macOS filesystem casing so `/users/...` aliases bind identically to Rust `realpath`; Windows identities use `/`, omit the extended-length prefix, and lowercase the drive letter. Non-UTF-8 canonical paths fail closed. |
| `agent_name` | Agent mailbox used for inbox and thread summaries. |
| `summary` | Counts for agents, reservations, inbox mailboxes, threads, commands, and degraded records. |
| `source_commands` | Redacted list of read-only commands/resources used, including `am status` plus the local Agent Mail `/health` and `/health/durability` resources. |
| `redaction_status` | Producer redaction posture, for example `paths_counts_subjects_only_no_content`. |
| `fallback_active` | Combined fail-closed posture across command validity, readiness, recovery, and durability. |
| `producer_status` | `ok` or `degraded`. |
| `command_statuses` | Redacted status for each read-only Agent Mail command. |
| `degraded` | Source-command failures and invalid command responses; reported health posture alone does not manufacture entries. |

A declared v1 snapshot always records exactly six ordered sources: agents,
reservations, body-free inbox rows, `am status`, `/health`, and
`/health/durability`. `command_statuses` has the same length and each status
repeats the command at the same index. Summary counts must match the normalized
arrays, and `degraded[]` must correspond one-for-one with failed statuses.

Optional bounded health metadata:

| Field | Purpose |
| --- | --- |
| `semantic_readiness` | Object describing semantic-readiness status. |
| `health_level` | Health classification used in semantic-readiness diagnostics. |
| `recovery` | Bounded recovery posture. Non-ok modes such as `corrupt` make reservation and inbox evidence non-authoritative. |
| `durability_state` | Bounded durability posture. `corrupt` is equivalent to recovery corruption and must not leak raw storage details. |

Authority precedence:

1. A failed Agent Mail source command, malformed response, or invalid count
   makes that source unavailable. Counts must fit the unsigned 64-bit wire
   range; booleans, negative values, and oversized integers are not accepted.
   Agent, reservation, and inbox inventories must also use one unambiguous
   recognized list shape, and every returned row must carry consistent required
   identity fields. Conflicting aliases or a partially malformed reservation
   list fail the whole source closed rather than silently dropping or
   downgrading a possible exclusive reservation.
2. `semantic_readiness.status=fail` makes reservation and inbox reads
   non-authoritative even when health is green.
3. `recovery.mode=corrupt`, non-ok `recovery.status`, or
   `durability_state=corrupt` also makes reservation and inbox reads
   non-authoritative, including when `semantic_readiness.status=ok`.
4. The `/health` readiness response and `/health/durability` response are
   independent. The dedicated durability response must contain a non-empty
   `durability_state` plus boolean `allows_reads` and `allows_writes`. The
   stricter bounded posture wins when they disagree. The readiness endpoint's
   intentional `durability_state=not_probed` is neutral only when the dedicated
   durability probe is valid. The readiness response must still contain a valid
   service `status` or health-level signal; semantic-readiness, recovery, and
   durability fields may refine or worsen that signal but cannot establish it.
5. Green transport health is only a transport signal; it does not override
   semantic, recovery, durability, or read-API evidence.

Recovery and durability snapshots must emit only bounded reason classes such as
`archive_corruption` or `storage_recovery_required`. They must not include raw
database paths, SQLite filenames, B-tree/page offsets, recovery bundle paths,
raw next-action text, mail bodies, or stack traces.

Unknown metadata is tolerated only on legacy, schema-less snapshots for
compatibility with older producers. A snapshot that declares
`ee.agent_mail.snapshot.v1` is validated strictly: unknown or contradictory
fields make its reservation and inbox evidence unavailable for claim decisions.

## Agent Mail Snapshot Fields

`file_reservations[]` entries:

| Field | Aliases | Redaction rule |
| --- | --- | --- |
| `path_pattern` | `path`, `pattern` | Workspace-relative path or glob only. |
| `holder` | `agent_name`, `agent`, `owner` | Agent identity only; no message body. |
| `exclusive` | none | Boolean. |
| `expires_ts` | `expires_at` | RFC 3339 timestamp when available. |

The strict snapshot summary counts the producer's captured reservation rows,
but claim/surface projection re-evaluates `expires_ts` at the consumer decision
clock. Rows whose expiry is less than or equal to that clock remain auditable in
the snapshot and are excluded from active collision risk.

`agents[]` entries:

| Field | Aliases | Redaction rule |
| --- | --- | --- |
| `name` | `agent_name`, `agent`, `mailbox` | Agent identity only. |
| `last_active_at` | `lastActiveAt`, `last_active_ts`, `lastActiveTs` | RFC 3339 timestamp when available. |

`inbox[]` entries:

| Field | Aliases | Redaction rule |
| --- | --- | --- |
| `mailbox` | `agent_name`, `agent` | Mailbox or agent identity only. |
| `unread_count` | `unread` | Authoritative non-negative count from `am status`; no subjects or bodies. |
| `ack_required_count` | `ackRequired` | Authoritative non-negative count from `am status`. |

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
Mail inventory, reservation, status-count, inbox-thread, and health calls.
`am status` is the authority for unread and acknowledgement counts. Inbox rows
shape thread summaries only; their presence and `ack_required` flags are not
count evidence. Inbox calls must be body-free, must not pass `--include-bodies`,
and must not mark messages read.

```bash
am agents list --project "$PWD" --json
am file_reservations list "$PWD" --active-only
am robot reservations --project "$PWD" --all --format json
am status --project "$PWD" --agent "$AGENT_NAME" --json
am mail inbox --project "$PWD" --agent "$AGENT_NAME" --limit 20 --json
curl -fsS http://127.0.0.1:8765/health
curl -fsS http://127.0.0.1:8765/health/durability
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
| `--inbox-limit <n>` | Maximum body-free inbox rows to read for thread projection; it does not cap or define status counts. |
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
am status --project <workspace> --agent <agent> --json
GET http://127.0.0.1:8765/health
GET http://127.0.0.1:8765/health/durability
```

The readiness endpoint may correctly report `durability_state=not_probed`.
That value is completed by the dedicated durability endpoint; it is not itself
proof of degradation. Missing or malformed dedicated durability fields fail
closed. A reported corrupt, repair-required, or read/write-disabled posture
also activates fallback without manufacturing a `degraded[]` command failure.
`degraded[]` remains a record of failed source commands or invalid command
responses.

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
  "project_key": "sha256:1111111111111111111111111111111111111111111111111111111111111111",
  "agent_name": "BeigeHollow",
  "redaction_status": "paths_counts_subjects_only_no_content",
  "source_commands": [
    "am agents list --project '<workspace>' --json",
    "am robot reservations --project '<workspace>' --all --format json",
    "am mail inbox --project '<workspace>' --agent BeigeHollow --limit 20 --json",
    "am status --project '<workspace>' --agent BeigeHollow --json",
    "agent-mail-health http://127.0.0.1:8765/health",
    "agent-mail-health http://127.0.0.1:8765/health/durability"
  ],
  "command_statuses": [
    {"command": "am agents list --project '<workspace>' --json", "ok": true, "exit_code": 0, "timed_out": false, "error_class": null},
    {"command": "am robot reservations --project '<workspace>' --all --format json", "ok": true, "exit_code": 0, "timed_out": false, "error_class": null},
    {"command": "am mail inbox --project '<workspace>' --agent BeigeHollow --limit 20 --json", "ok": true, "exit_code": 0, "timed_out": false, "error_class": null},
    {"command": "am status --project '<workspace>' --agent BeigeHollow --json", "ok": true, "exit_code": 0, "timed_out": false, "error_class": null},
    {"command": "agent-mail-health http://127.0.0.1:8765/health", "ok": true, "exit_code": 200, "timed_out": false, "error_class": null},
    {"command": "agent-mail-health http://127.0.0.1:8765/health/durability", "ok": true, "exit_code": 200, "timed_out": false, "error_class": null}
  ],
  "producer_status": "ok",
  "fallback_active": false,
  "am_agents_list_ok": true,
  "health_level": "green",
  "durability_state": "ok",
  "summary": {
    "agent_count": 1,
    "file_reservation_count": 1,
    "inbox_mailbox_count": 1,
    "thread_count": 1,
    "source_command_count": 6,
    "degraded_count": 0
  },
  "degraded": [],
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
  "project_key": "sha256:1111111111111111111111111111111111111111111111111111111111111111",
  "agent_name": "BeigeHollow",
  "redaction_status": "paths_counts_subjects_only_no_content",
  "producer_status": "ok",
  "source_commands": [
    "am agents list --project '<workspace>' --json",
    "am robot reservations --project '<workspace>' --all --format json",
    "am mail inbox --project '<workspace>' --agent BeigeHollow --limit 20 --json",
    "am status --project '<workspace>' --agent BeigeHollow --json",
    "agent-mail-health http://127.0.0.1:8765/health",
    "agent-mail-health http://127.0.0.1:8765/health/durability"
  ],
  "command_statuses": [
    {"command": "am agents list --project '<workspace>' --json", "ok": true, "exit_code": 0, "timed_out": false, "error_class": null},
    {"command": "am robot reservations --project '<workspace>' --all --format json", "ok": true, "exit_code": 0, "timed_out": false, "error_class": null},
    {"command": "am mail inbox --project '<workspace>' --agent BeigeHollow --limit 20 --json", "ok": true, "exit_code": 0, "timed_out": false, "error_class": null},
    {"command": "am status --project '<workspace>' --agent BeigeHollow --json", "ok": true, "exit_code": 0, "timed_out": false, "error_class": null},
    {"command": "agent-mail-health http://127.0.0.1:8765/health", "ok": true, "exit_code": 200, "timed_out": false, "error_class": null},
    {"command": "agent-mail-health http://127.0.0.1:8765/health/durability", "ok": true, "exit_code": 200, "timed_out": false, "error_class": null}
  ],
  "fallback_active": false,
  "am_agents_list_ok": true,
  "health_level": "green",
  "durability_state": "ok",
  "summary": {
    "agent_count": 0,
    "file_reservation_count": 0,
    "inbox_mailbox_count": 0,
    "thread_count": 0,
    "source_command_count": 6,
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
  "project_key": "sha256:1111111111111111111111111111111111111111111111111111111111111111",
  "agent_name": "BeigeHollow",
  "redaction_status": "paths_counts_subjects_only_no_content",
  "producer_status": "ok",
  "source_commands": [
    "am agents list --project '<workspace>' --json",
    "am robot reservations --project '<workspace>' --all --format json",
    "am mail inbox --project '<workspace>' --agent BeigeHollow --limit 20 --json",
    "am status --project '<workspace>' --agent BeigeHollow --json",
    "agent-mail-health http://127.0.0.1:8765/health",
    "agent-mail-health http://127.0.0.1:8765/health/durability"
  ],
  "command_statuses": [
    {"command": "am agents list --project '<workspace>' --json", "ok": true, "exit_code": 0, "timed_out": false, "error_class": null},
    {"command": "am robot reservations --project '<workspace>' --all --format json", "ok": true, "exit_code": 0, "timed_out": false, "error_class": null},
    {"command": "am mail inbox --project '<workspace>' --agent BeigeHollow --limit 20 --json", "ok": true, "exit_code": 0, "timed_out": false, "error_class": null},
    {"command": "am status --project '<workspace>' --agent BeigeHollow --json", "ok": true, "exit_code": 0, "timed_out": false, "error_class": null},
    {"command": "agent-mail-health http://127.0.0.1:8765/health", "ok": true, "exit_code": 200, "timed_out": false, "error_class": null},
    {"command": "agent-mail-health http://127.0.0.1:8765/health/durability", "ok": true, "exit_code": 200, "timed_out": false, "error_class": null}
  ],
  "fallback_active": false,
  "am_agents_list_ok": true,
  "health_level": "green",
  "durability_state": "ok",
  "summary": {
    "agent_count": 0,
    "file_reservation_count": 1,
    "inbox_mailbox_count": 0,
    "thread_count": 0,
    "source_command_count": 6,
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

Inbox counts unavailable:

```json
{
  "schema": "ee.agent_mail.snapshot.v1",
  "generated_at": "2026-06-04T18:26:00Z",
  "project_key": "sha256:1111111111111111111111111111111111111111111111111111111111111111",
  "agent_name": "BeigeHollow",
  "redaction_status": "paths_counts_subjects_only_no_content",
  "producer_status": "degraded",
  "source_commands": [
    "am agents list --project '<workspace>' --json",
    "am robot reservations --project '<workspace>' --all --format json",
    "am mail inbox --project '<workspace>' --agent BeigeHollow --limit 20 --json",
    "am status --project '<workspace>' --agent BeigeHollow --json",
    "agent-mail-health http://127.0.0.1:8765/health",
    "agent-mail-health http://127.0.0.1:8765/health/durability"
  ],
  "command_statuses": [
    {"command": "am agents list --project '<workspace>' --json", "ok": true, "exit_code": 0, "timed_out": false, "error_class": null},
    {"command": "am robot reservations --project '<workspace>' --all --format json", "ok": true, "exit_code": 0, "timed_out": false, "error_class": null},
    {"command": "am mail inbox --project '<workspace>' --agent BeigeHollow --limit 20 --json", "ok": true, "exit_code": 0, "timed_out": false, "error_class": null},
    {"command": "am status --project '<workspace>' --agent BeigeHollow --json", "ok": false, "exit_code": 0, "timed_out": false, "error_class": "invalid_response"},
    {"command": "agent-mail-health http://127.0.0.1:8765/health", "ok": true, "exit_code": 200, "timed_out": false, "error_class": null},
    {"command": "agent-mail-health http://127.0.0.1:8765/health/durability", "ok": true, "exit_code": 200, "timed_out": false, "error_class": null}
  ],
  "fallback_active": true,
  "am_agents_list_ok": true,
  "health_level": "green",
  "durability_state": "ok",
  "summary": {
    "agent_count": 1,
    "file_reservation_count": 0,
    "inbox_mailbox_count": 0,
    "thread_count": 0,
    "source_command_count": 6,
    "degraded_count": 1
  },
  "degraded": [
    {
      "code": "agent_mail_snapshot_source_unavailable",
      "severity": "warning",
      "source": "agent_mail",
      "command": "am status --project '<workspace>' --agent BeigeHollow --json",
      "error_class": "invalid_response",
      "exit_code": 0,
      "timed_out": false
    }
  ],
  "file_reservations": [],
  "agents": [
    {
      "name": "GoldenGate",
      "last_active_ts": "2026-06-04T18:20:00Z"
    }
  ],
  "inbox": [],
  "threads": []
}
```

The empty `inbox` array is not proof of zero unread messages when
`fallback_active` is true; it means the authoritative `am status` counts were
unavailable. Body-free inbox rows may still produce `threads[]`, but they never
substitute for the missing counts.

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

Recovery or durability corrupt:

```json
{
  "schema": "ee.swarm.coordination_health.v1",
  "healthLevel": "green",
  "semantic_readiness": {
    "status": "ok"
  },
  "recovery": {
    "mode": "corrupt",
    "reason": "archive_corruption"
  },
  "durability_state": "corrupt"
}
```

Recovery and durability corruption are authoritative even when transport health
is green and semantic readiness passes. The consumer must keep reservation and
inbox reads non-authoritative and surface only bounded reason classes such as
`archive_corruption`, never raw database paths, page offsets, or repair bundle
paths.

## Test Requirements

Implementation beads for the producer and parser must include fixtures for:

- absent `--agent-mail-snapshot`;
- health-only `ee.swarm.coordination_health.v1`;
- healthy full snapshot with all four arrays;
- stale snapshot;
- exclusive reservation conflict;
- inbox unavailable while fallback is active;
- inbox rows that look unread while authoritative `am status` reports zero;
- malformed, contradictory, or partially malformed agent, reservation, and inbox collections;
- missing, boolean, negative, oversized, and non-integer status counts;
- split readiness/durability health, including `not_probed` plus healthy,
  contradictory postures, missing durability fields, and disabled reads;
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
