# Swarm Replay Contracts

`ee.swarm_workload.v1` is the replay-lab input contract for multi-agent
workloads. It is orchestration metadata: fixture seed, workspace fingerprint,
agent count, command-shape sequence, expected degraded posture, redaction
probes, resource-profile hints, and provenance. It composes existing command
outputs and `ee.agent_workload_trace.v1` rows instead of carrying raw command
arguments or duplicating response envelopes.

Workload traces do not contain raw task strings, query text, memory bodies,
mail bodies, command output, secrets, environment dumps, full file listings, or
absolute host paths. Command steps record verb chains, positional arity, flag
names, output format, and a `commandHash`. Workspace identity is a
`workspaceFingerprint` plus path policy, never a local checkout path.
`fixtureSeed` is deterministic fixture identity, not wall-clock time.

`ee.swarm_replay_result.v1` is the compact result ledger future replay runners
emit. It records per-command exit code, normalized `elapsedMs`, stdout/stderr
byte counts, degraded codes, command hash, redacted artifact path tails,
optional RSS/CPU measurements, aggregate latency percentiles, first-failure
diagnosis, redaction posture, and verification posture.

Result ledgers remain side-effect-free. They can record that RCH was required,
passed, failed, or blocked before Cargo, but they do not treat local Cargo
fallback as acceptable proof. They also do not include timestamps; volatile
measurement fields are normalized and named under `volatileFieldsStripped`
when they are excluded from replay hashes.

These contracts are intentionally smaller than a runner API. Later beads can
build fixture generation, admission checks, replay execution, and support
bundle export on top of them without inventing new public fields.
