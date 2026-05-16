# ee.verification.run.v1

`ee.verification.run.v1` is the retained, redaction-safe record for one verification run. It was introduced by `bd-1zb7k.15.1` so agents can cite what ran, where it ran, and which hashes identify the source, command, output, and retained artifacts without copying raw logs.

Required uses:

- Preserve remote-vs-local execution substrate, worker host, exit code, and command/source hashes.
- Store hashes and bounded references, not raw stdout, stderr, local paths, or secret-bearing output.
- Treat the record as evidence only. Reuse decisions belong to `ee.verification.reuse_advisory.v1`.

Non-goals:

- It does not start builds, reserve RCH slots, or decide whether a run is reusable.
- It does not replace Beads, Agent Mail, J1 logs, or support-bundle artifacts.
- It does not make local Cargo acceptable for repositories that require remote verification.
