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
emit. It records host-profile admission, per-command exit code, normalized
`elapsedMs`, stdout/stderr byte counts, degraded codes, command hash, redacted
artifact path tails, optional RSS/CPU measurements, aggregate latency
percentiles, first-failure diagnosis, redaction posture, and verification
posture. The `verification.proofCapsule` block records replay-local static
checks plus an optional compact `ee.rch.verify.v1` summary.

The host-profile admission block classifies replay evidence as `smoke`,
`standard`, or `large-host` from declared workload requirements and observed
host posture. It may include CPU count, available memory, target/TMPDIR posture,
RCH availability, NUMA availability, lexical RAM-tier availability, and hashed
path-tail references. It must not include absolute target paths, raw `TMPDIR`
values, environment dumps, or local machine-specific drive paths.

Result ledgers remain side-effect-free. They can record that RCH was required,
passed, failed, or blocked before Cargo, but a `local Cargo fallback` is not
acceptable proof. RCH proof capsules may include command hash, worker id, remote
marker presence, whether Cargo started, selector failure reason, known blocker
fingerprint, local fallback refusal, and local Cargo contamination count. They
must not include raw stdout/stderr tails, remote project roots, target
directories, environment dumps, or private absolute host paths.

Replay ledgers do not include replay execution timestamps; volatile measurement
fields are normalized and named under `volatileFieldsStripped` when they are
excluded from replay hashes.

## Verification Tiers

Smoke verification is the no-Cargo path:

```bash
./scripts/e2e_overhaul/swarm_replay_lab_smoke.sh
```

It generates a small workload, runs `ee lab swarm replay --dry-run`, and logs
`ee.test_event.v1` command/assertion rows with sanitized environment posture,
elapsed time, exit code, stdout/stderr artifact references, schema validation
status, redaction status, and first-failure diagnosis. A smoke run proves that
the public workflow, redaction gates, and result ledger shape are wired. It
does not prove standard or large-host replay performance.

Standard verification stays RCH-only:

```bash
RCH_REQUIRE_REMOTE=1 ./scripts/rch_verify.sh \
  --bead-id bd-ppbue.8 \
  --summary \
  --no-write \
  --known-blocker-override \
  -- cargo test --test e2e_lab_swarm_workload_generator \
  lab_swarm_replay_executes_small_generated_fixture_with_artifact_ledger \
  -- --nocapture
```

Large-host fixture coverage also stays RCH-only:

```bash
RCH_REQUIRE_REMOTE=1 ./scripts/rch_verify.sh \
  --bead-id bd-ppbue.8 \
  --summary \
  --no-write \
  --known-blocker-override \
  -- cargo test --test e2e_lab_swarm_workload_generator \
  lab_generate_workload_emits_all_profiles_as_redaction_safe_json \
  -- --nocapture
```

Closeout checklist:

1. `scripts/e2e_overhaul/swarm_replay_lab_smoke.sh` passes locally without
   invoking Cargo.
2. `shellcheck` or `bash -n` covers touched shell scripts.
3. Markdown docs name `ee lab swarm replay`, not the older `ee lab replay`
   trace form.
4. RCH proof is attached with a remote pass, or the tracker records the exact
   selector/blocker fingerprint and local Cargo count `0`.
5. `jq empty .beads/issues.jsonl` and `br dep cycles --json` pass before the
   Beads closeout commit.

These contracts are intentionally smaller than a runner API. The implementation
plan is tracked under `bd-ppbue`; inspect live Beads state before claiming,
closing, or changing dependencies because tracker authority can be degraded in
crowded checkouts.

Current orientation map:

- `bd-ppbue.1`: define `ee.swarm_workload.v1` and
  `ee.swarm_replay_result.v1`.
- `bd-ppbue.2`: build deterministic workload fixture generation.
- `bd-ppbue.3`: add the `ee lab swarm replay` runner and machine-readable
  ledgers.
- `bd-ppbue.4`: add SLO budgets and output-size guardrails to replay results.
- `bd-ppbue.5`: add host-profile admission for large-machine replay runs.
- `bd-ppbue.6`: emit compact verification proof capsules from replay and RCH
  gates.
- `bd-ppbue.7`: export replay summaries through support bundles and handoffs.
- `bd-ppbue.8`: wire docs, examples, smoke verification, and RCH-only proof
  instructions.
- `bd-ppbue.9`: add generator evidence ledgers for fixture reproducibility.
- `bd-ppbue.10` and `bd-ppbue.11`: expose reservation pressure and stalled-work
  liveness signals for crowded swarms.
- `bd-ppbue.12` and `bd-ppbue.13`: harden malformed-trace handling and promote
  recorded agent workload traces into fixtures.
- `bd-ppbue.14` through `bd-ppbue.18`: classify RCH selector/admission and
  remote-source materialization failures so replay evidence is not mistaken for
  a source verdict.
- `bd-ppbue.19` through `bd-ppbue.27`: pin schema, public registry, and SLO
  conformance coverage as the contracts mature.

Do not claim the `bd-ppbue` epic as implementation work. Claim a concrete child
only when the tracker is authoritative, dependencies and reservations are clear,
and required Rust verification can stay RCH-only.
