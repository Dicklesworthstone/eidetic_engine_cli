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
anything to disk the operator did not ask for. The recorder log is
`agent-workload-trace.jsonl` under the workspace's `obs/flight_recorder/`
subtree (or the `EE_FLIGHT_RECORDER_DIR` override). While it is on, every
command appends there, read-only commands included.

## Env registry

| Variable | Default | Effect |
| --- | --- | --- |
| `EE_FLIGHT_RECORDER` | `0` | When `1`, every `ee` invocation that parses appends one `ee.agent_workload_trace.v1` row after it finishes, unless `ee status` would not report the recorder as writing. `ee recorder flight` is skipped: append writes its own row and replay reads the log. |
| `EE_FLIGHT_RECORDER_DIR` | `<workspace>/obs/flight_recorder/` | Override the log directory. Useful for read-only test workspaces. |
| `EE_FLIGHT_RECORDER_RETENTION_DAYS` | `7` | Retention window reported by `ee status` and `ee doctor`. Nothing prunes the log: the recorder never deletes trace files. An append that would pass the fixed 256 MiB quota is refused. |

An automatically captured row records the verb chain, the names of the
flags given, the positional count, the output format, the exit code, the
elapsed time and the stdout byte count (with a bytes/4 token estimate). It
carries no memory references or degraded codes, and its harness program is
`unknown`; a harness that knows those can write richer rows with
`ee recorder flight append`. A failed append never changes the command's
output or exit code.

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
# Per-invocation
EE_FLIGHT_RECORDER=1 ee pack "prepare release" --json

# Persistent: set these in the environment that runs ee (or ee daemon)
export EE_FLIGHT_RECORDER=1
export EE_FLIGHT_RECORDER_RETENTION_DAYS=7     # the default
export EE_FLIGHT_RECORDER_DIR=/path/to/traces  # optional; overrides the default directory
```

The recorder reads only these environment variables; there are no
`obs.flight_recorder.*` config keys. The 256 MiB quota and the `strict`
redaction level are fixed.

`ee status --json` and `ee doctor --json` surface recorder posture
(enabled / retention / quota / redaction level / current log
bytes) so an agent can confirm the recorder is running without
inspecting the workspace directly.

## How to read traces

```bash
# Replay a trace file into deterministic aggregates (AFR2)
ee recorder flight replay --trace ./obs/flight_recorder/agent-workload-trace.jsonl --json

# Pack-diet report from a captured workload (AFR3)
ee perf prompt-budget --trace ./obs/flight_recorder/agent-workload-trace.jsonl --json
```

There is no tail command; read the JSONL file directly. Automatic capture,
`ee recorder flight append` and replay share `record_workload()` /
`append_workload_trace()` / `replay_workload_trace()` in
`src/obs/flight_recorder.rs`.

## Failure modes

| Condition | Degraded code | Recovery |
| --- | --- | --- |
| Trace directory not writable (read-only mount, permission denied). | `flight_recorder_dir_unwritable` | Set `EE_FLIGHT_RECORDER_DIR` to a writable path, or disable the recorder. |
| Quota exceeded (current bytes >= `[obs.flight_recorder] max_bytes`). | `flight_recorder_quota_exceeded` | Raise `max_bytes`, lower `retention_days`, or run `ee obs flight-recorder prune`. |
| Recorder disabled at startup but env override set at runtime. | `flight_recorder_env_override_ignored` | Restart the daemon to pick up the env override, or use `ee config set`. |

All three are advisory; the recorder never blocks the foreground
command path. A failure to append a trace row is logged via the
existing `tracing` surface and surfaces in `ee status`, but the
underlying `ee pack` / `ee search` / `ee why` call returns its
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
