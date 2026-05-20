# Agent workload flight recorder

> **bd-1zb7k.19.1 (AFR1).** Opt-in, local-only telemetry that captures
> the *shape* of `ee` command invocations — command verbs, flag names,
> timing, response byte counts, degraded codes, hashed memory refs —
> and never captures the raw task strings, raw query text, raw memory
> bodies, raw provenance text, raw mail bodies, secrets, environment
> dumps, or full file listings that those commands consumed. The
> redaction contract is encoded structurally in
> `ee.agent_workload_trace.v1` (every `*Present` boolean in
> `redactionPosture` is `const: false`), so a serializer that ever
> claims raw content is present cannot validate against the schema.
>
> The recorder is **off by default**. AFR2 replay, AFR3 prompt-budget
> diet report, and AFR5 64-agent playback compose on top of the
> traces this module appends.

## When to enable

| Profile | Recommendation | Why |
| --- | --- | --- |
| Single-developer laptop | **OFF** | One agent, no fan-out, no swarm workload to analyse. The disk + CPU cost is not amortised. |
| Daemon / `ee serve` mode | **OFF by default; ON via `EE_FLIGHT_RECORDER=1`** | Stable daemon process is the right substrate for the recorder's append-only log. |
| 64-agent swarm host | **ON** | The whole point — workload analysis, prompt-budget diet, 64-agent playback all need the trace. |

The default is off so a stock developer install does not write
anything to disk the operator did not ask for. The recorder log
lives under the workspace's `obs/flight_recorder/` subtree (or the
`EE_FLIGHT_RECORDER_DIR` override).

## Env registry

| Variable | Default | Effect |
| --- | --- | --- |
| `EE_FLIGHT_RECORDER` | `0` | When `1`, every agent-facing `ee` command appends one `ee.agent_workload_trace.v1` row to the recorder log. |
| `EE_FLIGHT_RECORDER_DIR` | `<workspace>/obs/flight_recorder/` | Override the log directory. Useful for read-only test workspaces. |
| `EE_FLIGHT_RECORDER_RETENTION_DAYS` | `7` | Maximum age of trace rows before the daemon prunes them. The prune pass also enforces `[obs.flight_recorder] max_bytes`. |

## Trace row shape

Every row carries:

```json
{
  "schema": "ee.agent_workload_trace.v1",
  "sideEffectFree": true,
  "redactionLevel": "strict",
  "traceId": "trc_<blake3-hash>",
  "recordedAt": "2026-05-20T08:42:09Z",
  "command": {
    "verbs": ["context"],
    "flagNames": ["--profile", "--ppr-weight"],
    "positionalArity": 1,
    "outputFormat": "json"
  },
  "exitCode": 0,
  "elapsedMs": 142,
  "responseByteCount": 8123,
  "responseTokenEstimate": 2031,
  "tokenEstimatorId": "bytes_div_4",
  "harnessIdentity": {
    "program": "claude-code",
    "modelFamily": "claude-opus"
  },
  "memoryReferences": [
    {"hash": "blake3:abc...", "kind": "fact"}
  ],
  "degradedCodes": [],
  "redactionPosture": {
    "rawTaskStringPresent": false,
    "rawQueryTextPresent": false,
    "rawMemoryBodyPresent": false
  },
  "retentionPosture": {
    "retainedUntil": "2026-05-27T08:42:09Z",
    "retentionDays": 7
  }
}
```

The contract is:

- **Shape, not content.** `command.verbs` and `command.flagNames`
  describe what was invoked; the raw values of those flags and any
  positional arguments are NEVER captured.
- **Hashed refs.** `memoryReferences[].hash` is a BLAKE3 hash of the
  memory ID; the raw ID, the body, and the provenance are not
  represented in the row. A downstream consumer that wants to join
  hashes against a memory table must export that mapping separately
  through a deliberate API call.
- **High-level identity.** `harnessIdentity.program` and
  `harnessIdentity.modelFamily` name a known agent program family;
  the operator's username, hostname, IP, or any other identifying
  context is never captured.
- **Structural redaction proof.** Every `*Present` boolean in
  `redactionPosture` is `const: false` in the schema, so a serializer
  that ever sets one true cannot validate. AFR2 replay and AFR3 diet
  report can rely on the redaction contract by construction.

## How to enable

```bash
# Per-invocation override
EE_FLIGHT_RECORDER=1 ee context "prepare release" --json

# Persistent (for ee daemon / ee serve)
ee config set obs.flight_recorder.enabled true
ee config set obs.flight_recorder.retention_days 7
ee config set obs.flight_recorder.max_bytes 268435456  # 256 MiB
ee config set obs.flight_recorder.redaction strict
```

`ee status --json` and `ee doctor --json` surface recorder posture
(enabled / retention / quota / redaction level / current log
bytes) so an agent can confirm the recorder is running without
inspecting the workspace directly.

## How to read traces

```bash
# Stream the most recent traces
ee obs flight-recorder tail --since 1h --json | jq

# Replay a slice into a fresh deterministic harness (AFR2)
ee obs flight-recorder replay --from-trace trc_abc... --json

# Pack-diet report from a captured workload (AFR3)
ee perf prompt-budget --trace ./obs/flight_recorder/2026-05-20.jsonl --json
```

The `ee obs flight-recorder tail` and `replay` surfaces are owned by
follow-up beads bd-1zb7k.19.2 (AFR2) and bd-1zb7k.19.3 (AFR3); the
core `record_workload()` / `append_workload_trace()` / `replay_workload_trace()`
in `src/obs/flight_recorder.rs` is the substrate they all share.

## Failure modes

| Condition | Degraded code | Recovery |
| --- | --- | --- |
| Trace directory not writable (read-only mount, permission denied). | `flight_recorder_dir_unwritable` | Set `EE_FLIGHT_RECORDER_DIR` to a writable path, or disable the recorder. |
| Quota exceeded (current bytes >= `[obs.flight_recorder] max_bytes`). | `flight_recorder_quota_exceeded` | Raise `max_bytes`, lower `retention_days`, or run `ee obs flight-recorder prune`. |
| Recorder disabled at startup but env override set at runtime. | `flight_recorder_env_override_ignored` | Restart the daemon to pick up the env override, or use `ee config set`. |

All three are advisory; the recorder never blocks the foreground
command path. A failure to append a trace row is logged via the
existing `tracing` surface and surfaces in `ee status`, but the
underlying `ee context` / `ee search` / `ee why` call returns its
normal response unchanged.

## Privacy and retention

- The recorder log lives only on the local host. It is never
  exported through `ee mesh`, `ee export`, or `ee backup` by default.
  The `--include-flight-recorder` flag must be passed explicitly,
  and the support-bundle redaction layer scrubs every raw-value
  candidate (which the schema guarantees is absent anyway).
- Retention is enforced by a daemon prune pass. The default 7-day
  window matches the support-bundle retention.
- Each row's `retainedUntil` is `recordedAt + retentionDays`. After
  that timestamp the row is eligible for pruning; nothing in `ee`
  reads past-retention rows from disk.

## When to disable

- A workspace under disk-pressure where the recorder log is large
  enough to matter (see `ee diag artifacts --workspace .` for the
  per-subtree breakdown).
- A multi-tenant host where the operator does not control all
  agents and cannot guarantee they trust the redaction contract.
- A short-lived CLI workflow that never replays traces.

Set `EE_FLIGHT_RECORDER=0` and restart the daemon. Existing trace
rows are retained until their `retainedUntil` timestamp.

## Distinguishability from neighboring features

| Feature | Bead | Scope |
| --- | --- | --- |
| QoS active-lane registry | `bd-1zb7k.20.2` | Live pressure / throttling state, not a historical log. |
| Recorder gate (causal evidence) | `bd-17c65.10.recorder` | Per-action evidence with full content (gated by separate redaction). Not the agent workload shape. |
| Verification evidence ledger | `bd-1nxz4.5` | Build / test / RCH verification rows. Not agent-command rows. |
| Agent workload flight recorder | **bd-1zb7k.19.1 (this doc)** | Append-only shape-only log of agent-facing `ee` command invocations. |

The four compose: QoS shows live state, the recorder gate shows
per-action causal evidence with content, the verification ledger
shows build/test rows, and the flight recorder shows the
redaction-safe shape of every command. Downstream analytical
beads (AFR2 / AFR3 / AFR5) read the flight recorder.
