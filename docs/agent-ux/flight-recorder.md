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
| `ee mcp serve-stdio` | **ON via `EE_FLIGHT_RECORDER=1`** in the server's environment | Each MCP tool call runs through the CLI path, so each is recorded. |
| `ee serve` / `ee daemon` | Not covered | Their requests call handlers directly, so none is recorded. The server process records one row when it exits. |
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

`ee status --json` and `ee doctor --json` report the recorder's posture
(enabled, writing, directory, retention, quota and redaction level), so
an agent can confirm the recorder can write without inspecting the
workspace. They do not report the log's current size.

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

| Condition | What happens | Recovery |
| --- | --- | --- |
| Trace directory not writable (read-only mount, permission denied). | `ee status` and `ee doctor` report the recorder as not writing, with degraded code `flight_recorder_directory_unwritable`. No rows are appended. | Set `EE_FLIGHT_RECORDER_DIR` to a writable path, or disable the recorder. |
| Trace directory under the workspace's `.git/`. | Not writing, with `flight_recorder_directory_inside_git`. No rows are appended. | Point `EE_FLIGHT_RECORDER_DIR` outside `.git/`. |
| `EE_FLIGHT_RECORDER_RETENTION_DAYS` outside 1 to 30. | Not writing, with `flight_recorder_retention_out_of_range`. No rows are appended. | Set it to a value from 1 to 30, or unset it. |
| An append would pass the quota: a fixed 256 MiB for automatic capture, `--max-bytes` for `ee recorder flight append`. | Automatic capture drops the row. `ee recorder flight append` fails with a storage error. | Move `agent-workload-trace.jsonl` aside, since nothing rotates or prunes it, or point `EE_FLIGHT_RECORDER_DIR` at a fresh directory. |

Recording never blocks or changes a command. A failed automatic append
is dropped, and the command's output and exit code are what they would
have been with the recorder off. `ee status` reports whether the
recorder can write, not individual append failures.

## Privacy and retention

- The recorder log lives only on the local host. No `ee` export path
  reads `obs/flight_recorder/`: `ee mesh`, `ee export`, `ee backup` and
  `ee support bundle` all leave it out. Rows carry shape only, never
  raw values.
- Nothing prunes the log. `EE_FLIGHT_RECORDER_RETENTION_DAYS` (1 to 30,
  default 7) is validated and reported by `ee status` and `ee doctor`,
  but rows stay in the file until you move or delete it.

## When to disable

- A workspace under disk-pressure where the recorder log is large
  enough to matter (see `ee diag artifacts --workspace .` for the
  per-subtree breakdown).
- A multi-tenant host where the operator does not control all
  agents and cannot guarantee they trust the redaction contract.
- A short-lived CLI workflow that never replays traces.

Unset `EE_FLIGHT_RECORDER` or set it to `0`. It is read on every
invocation, so the next command stops recording. A running
`ee mcp serve-stdio` keeps the environment it started with, so restart
it too. Existing rows stay in the trace file until you move or delete it.

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
