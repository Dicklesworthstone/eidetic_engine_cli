# Disk Pressure Diagnostics

`ee diag disk-pressure --json` is a read-only diagnostic surface. It reports
capacity, top consumers, preservation-only recovery actions, and oversized
agent-harness logs under `agentHarnessLogs`.

Agent-harness log pressure is intentionally separate from Cargo scratch
pressure. `CARGO_TARGET_DIR` and `TMPDIR` can both point at external storage
while `~/.codex/log` continues to grow on the host filesystem.

Each `agentHarnessLogs[]` entry uses schema
`ee.disk_pressure.agent_harness_log_classifier.v1` and includes:

- `entry.activity`: `active_open`, `closed`, or
  `open_handle_probe_unavailable`
- `entry.owningProcessSummary`: process IDs when discoverable, otherwise an
  unavailable reason
- `repairKind`: one of `preserve_tail_copy`, `rotate_with_manifest`,
  `move_preserve`, `ask_human`, or `noop`
- `mutationPolicy: "preservation_only"` and `sideEffectFree: true`

The surface never recommends deletion or truncation. Active or unprobeable logs
must be preserved first and require explicit human approval before any cleanup.
