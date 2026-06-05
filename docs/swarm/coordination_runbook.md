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

The health script is not a full snapshot producer. It can run send smoke checks
and emits `ee.swarm.coordination_health.v1`, which is useful transport evidence
but does not include active reservations, agent roster entries, unread counts,
or thread freshness.

When those fields matter, generate a full read-only snapshot:

```bash
scripts/agent_mail_snapshot.sh \
  --project "$PWD" \
  --agent "$AGENT_NAME" \
  --json | jq .
```

Use `--json` for stdout. Use `--output` to write a snapshot file; plain
`--output` is quiet, and `--json --output` writes the same snapshot to stdout
and to the file.

```bash
SNAPSHOT_PATH=/private/tmp/ee-agent-mail-snapshot.json
COORDINATION_PATH=/private/tmp/ee-coordination-snapshot.json
scripts/agent_mail_snapshot.sh \
  --project "$PWD" \
  --agent "$AGENT_NAME" \
  --output "$SNAPSHOT_PATH" \
  --coordination-output "$COORDINATION_PATH"
```

Use a canonical, non-symlink `SNAPSHOT_PATH`. On macOS, `/tmp` is a symlink and
`ee swarm brief --agent-mail-snapshot /tmp/...` refuses the path.

Then consume each artifact without live Agent Mail mutation:

```bash
ee swarm brief --workspace . --agent-mail-snapshot "$SNAPSHOT_PATH" --json
ee workspace hygiene --workspace . --agent-name "$AGENT_NAME" \
  --agent-mail-snapshot "$SNAPSHOT_PATH" --json
ee pack "next bead" --workspace . \
  --coordination-snapshot "$COORDINATION_PATH" --json
```

Use the full Agent Mail snapshot for `--agent-mail-snapshot` consumers and the
companion `ee.coordination_snapshot.v1` file for `--coordination-snapshot`
consumers. The shapes are intentionally different.

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

## Proof-Lane Coordination

For GitHub Actions artifact lanes, use the CI proof-lane snapshot runbook:
[`docs/ci-proof-lane-snapshot.md`](../ci-proof-lane-snapshot.md). It is the
source for whether agents should reuse an active run, wait, download and verify
an artifact, dispatch exactly one new run after coordination, abstain, or file a
follow-up Bead.

When Agent Mail is available, announce the snapshot verdict before any
`workflow_dispatch`:

```text
[proof-lane] <verdict> for <bead-or-surface>
- workflow: <workflow name> (<workflow path>)
- run_id: <run id or none>
- head_sha: <40-char source SHA>
- artifact: <artifact name or none>
- local_cargo_tripwire: <ok count=0 | bypass_detected count=N | not_checked>
- next_action: <activeRecommendation.nextAction>
```

When Agent Mail is degraded, put the same fields in the Bead comment thread and
avoid dispatching a duplicate run unless the snapshot says `dispatch_new_run`.
Do not cancel workflows, clean artifacts, create worktrees, switch branches, or
use local Cargo as substitute proof without explicit human authorization.

## Interpreting Missing Sources

Missing Agent Mail data means the coordination source is unavailable. It does
not mean there are no reservations, no messages, or no active owners.
`ee swarm brief` may report that `127.0.0.1:8765/health` is reachable when no
snapshot is configured; that only proves the health endpoint answered, not that
the brief consumed live reservations or inbox state.

Similarly, a green `scripts/swarm_coordination_health.sh` event is not proof of
zero reservations or zero unread mail. Use `scripts/agent_mail_snapshot.sh`
when claim decisions need reservation, roster, inbox, or thread evidence.

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
