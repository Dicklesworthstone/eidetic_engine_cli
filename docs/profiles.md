# Operating Profiles

Host-adaptive operating profiles let `ee` automatically tune resource budgets
(cache sizes, search limits, verification timeouts) based on detected hardware.
No daemon required.

## Profile Tiers

| Profile | Cores | Memory | Use Case |
|---------|-------|--------|----------|
| `constrained` | <2 | <8 GiB | CI runners, small VMs, containers |
| `portable` | 2-5 | 8-15 GiB | Laptops, dev containers |
| `workstation` | 6-11 | 16-31 GiB | Desktop workstations |
| `swarm` | 12+ | 32+ GiB | Build servers, large RCH hosts |

## Workflow: Probe, Plan, Apply, Verify

### Step 1: Probe host resources

The probe is **side-effect-free** and runs automatically when you plan a config:

```bash
# See what the probe detects (embedded in plan output)
ee profile config plan --json | jq '.data.probe'

# Or inspect the read-only host profile surface directly
ee diag host-profile --workspace . --json
```

Output includes: CPU cores, memory totals, path capacities, tool availability,
and RCH posture. Absolute paths are redacted by default. When
`ee diag host-profile --full-paths --json` is used, the response changes its
redaction markers to record that local paths were explicitly requested.

### Step 2: Plan config changes

View exact TOML edits before writing:

```bash
# Show planned changes without writing
ee profile config plan

# JSON output for machine parsing
ee profile config plan --json
```

Example human output:
```
profile config plan: planned
  profile: swarm (recommended: swarm, confidence: high)
  config:  .ee/config.toml (would create)

  edits:
    + profile.selected = "swarm"
    + profile.budgets.search_candidate_limit = 240
    + profile.budgets.pack_max_tokens = 8000
    ...

  repair: Review plannedToml, then run `ee profile config apply` without `--dry-run`.
```

### Step 3: Apply config

```bash
# Dry-run (no write, same as plan)
ee profile config apply --dry-run

# Actually write .ee/config.toml
ee profile config apply
```

The command preserves existing TOML formatting where possible.

### Step 4: Verify with profile-aware recipes

After applying, verification commands respect the profile budgets:

```bash
# Run verification with profile-aware timeouts and targets
./scripts/verify.sh
```

On constrained hosts, heavy gates may be skipped or use shorter timeouts.

## Override the Recommendation

Force a specific profile instead of the auto-detected one:

```bash
# Plan with explicit profile
ee profile config plan --profile portable

# Apply with explicit profile
ee profile config apply --profile constrained
```

Valid profiles: `constrained`, `portable`, `workstation`, `swarm`.

## Calibrated Recommendation Contract

H3 recommender output uses a standalone schema:
`ee.host_calibration.recommendation.v1`. It is not a replacement for
`ee.host_profile.v1` or `ee.profile.runtime.v1`:

- `ee.host_profile.v1` is the read-only probe input.
- `ee.host_calibration.recommendation.v1` explains the recommended profile and
  budget deltas.
- `ee.profile.runtime.v1` reports the effective profile a command actually
  used.

Status, doctor, and support-bundle surfaces should embed the recommendation
report instead of copying its fields into surface-specific shapes. The report
must include:

| Field | Meaning |
|-------|---------|
| `configuredProfile` | Explicit config or CLI override, or `null` when absent |
| `recommendedProfile` | Deterministic recommendation from host probe and calibration evidence |
| `effectiveProfile` | Profile used after explicit overrides are applied |
| `budgetDeltas[]` | Per-budget comparison from baseline to recommended/effective value |
| `reasonCodes[]` | Stable explanation codes, sorted by deterministic implementation order |
| `calibrationFreshness` | Fresh/stale/missing/unavailable evidence status with repair hint |
| `topologyWarnings[]` | RCH, target-dir, and path-topology warnings that affect budgets |
| `degraded[]` | Response-time degradations that changed the recommendation |

The recommender is side-effect-free. It never writes `.ee/config.toml`, changes
profile settings, starts RCH work, or mutates caches. Operators apply changes
only through the existing `ee profile config plan/apply` workflow.

## Resource Admission Contract

Resource-admission reports use `ee.resource_admission.v1`. They consume the
host-calibration recommendation and other existing posture surfaces, but they do
not apply profile changes. The report is advisory evidence for agents deciding
whether to run, degrade, queue, wait for RCH, split a workload, refuse local
Cargo fallback, or abstain because evidence is missing.

The contract deliberately stays below scheduling authority:

- `sideEffectFree` is always `true`.
- `advisoryOnly` is always `true`.
- `mutationPolicy` is always `never_mutates_state`.
- `policyDomain` is always `resource_profile_budget_admission`.

Allowed decisions are schema-pinned: `admit`, `degrade_to_lean`, `queue`,
`wait_for_rch`, `split_workload`, `refuse_local_cargo`, and `abstain`.
Admission advice must preserve the normal safety gates for Beads freshness,
Agent Mail reservations, RCH ownership, redaction posture, and local Cargo
policy. It cannot convert an unsafe claim into a safe claim.

### Resource-Admission Signal Inventory

The resource-admission policy consumes existing posture reports. It must not
probe new live state from inside the policy body; caller surfaces gather inputs
and pass redacted summaries into the admission report.

| Input signal | Source surface or schema | Normalized field | Freshness and confidence rule | Redaction rule | Reason-code family |
|--------------|--------------------------|------------------|-------------------------------|----------------|--------------------|
| Host calibration posture | `ee.host_calibration.posture.v1` from status, doctor, or support-bundle profile evidence | `sourcePosture.hostCalibration` | Fresh/partial/stale/missing/contradictory/unavailable map directly; missing or unavailable is low confidence | Use redacted profile labels and posture fields only | `host_calibration_*` |
| Budget deltas | `ee.host_calibration.recommendation.v1` and `budgetDeltas[]` | `sourcePosture.resourceBudget` | Current recommendation is high confidence; stale or missing calibration lowers confidence before budget advice is trusted | Include surface, unit, direction, and reason codes; do not include raw host paths | `budget_*`, `swarm_host_headroom`, `conservative_profile_ceiling` |
| Requested and effective profile | CLI/config profile, `ee.profile.runtime.v1`, or `--resource-profile` on pack-like surfaces | `subject.requestedProfile`, `subject.effectiveProfile` | Explicit profile is current for that command; absent request is `null` and must not be treated as swarm intent | Record profile names only | `budget_within_profile`, `budget_override_clamped` |
| RCH worker and selector posture | `rch status --json`, `ee.rch.selector_admission_probe.v1`, verification ledger, and proof-broker summaries | `sourcePosture.rch` | Active-build and worker-pressure data is fresh only for the current status snapshot; stale blockers lower confidence or force wait/abstain | Use active build id, worker id, bounded command preview/hash, heartbeat/progress ages | `rch_*` |
| Local Cargo posture | `ee.rch_local_cargo_tripwire.v1` from `scripts/check-local-cargo-tripwire.sh --probe-processes --json` | `sourcePosture.localCargo` | Clean tripwire is high confidence for the scan instant; observed Cargo metadata is not proof and cannot authorize fallback | Redact paths and record process kind/subcommand only | `local_cargo_*` |
| QoS lane pressure | `ee.qos.active_lane_summary.v1`, swarm brief, or work-packet lane summary | `sourcePosture.lanePressure` | Fresh when generated with the work-packet or status snapshot; stale lane data cannot make a queued workload safe | Counts and lane names only | `lane_pressure_*` |
| Workload pressure | Cache/hotset reports, write-spool reports, read/search/pack SLO reports, graph/index refresh posture | `sourcePosture.workloadPressure` | Caller decides whether the source report is fresh enough for the requested surface; missing optional pressure inputs degrade to `unknown` | Include bounded counts, profile labels, and hashes; no raw memory bodies | `cache_pressure`, `write_spool_pressure`, `read_pool_pressure`, `pack_slo_pressure`, `index_pressure`, `graph_pressure` |
| Daemon posture | Daemon status, daemon RPC schemas, or support-bundle daemon evidence | `sourcePosture.daemon` | `not_required` for one-shot CLI reads; daemon-required surfaces must report unavailable/degraded honestly | Socket paths are labels or redacted paths only | `daemon_*` |
| Replay and SLO evidence | `ee.agent_workload_replay.v1`, `ee.swarm_slo.scorecard.v1`, replay lab artifacts | `sourcePosture.replay` | Fresh only when the replay input hash matches the requested workload class; stale or missing replay evidence cannot prove admission safety | Include scorecard ids, hashes, and summary metrics only | `replay_slo_*` |
| Coordination and claim evidence | `ee.swarm.work_packet.v1`, claim-gate reports, Beads freshness, Agent Mail snapshot status | `evidence[]` and caller-specific safety gates | Admission advice is advisory and never overrides stale tracker state, reservation collisions, or live owner evidence | No raw mail bodies, raw Beads JSONL excerpts, or unbounded command output | `missing_required_signal`, `stale_source_authority`, `contradictory_evidence` |

### Queue-Pressure Block

`queuePressure` is an optional bounded object for queue-pressure fairness in
large swarms. Older emitters may omit it; emitters that include it must use the
schema-pinned fields below and must keep `canAuthorizeClaim = false`.

| Field | Meaning |
|-------|---------|
| `level` | One of `idle`, `low`, `moderate`, `saturated`, or `unknown`. |
| `reasonCodes[]` | Queue-specific reason taxonomy. Current codes are `rch_lane_busy`, `rch_telemetry_gap`, `active_build_slot_exhausted`, `stale_in_progress_bead`, `agent_mail_unavailable`, `agent_mail_recovery_corrupt`, `dirty_checkout_saturated`, `local_cargo_refused`, `output_budget_pressure`, `host_calibration_missing`, and `contradictory_source_state`. |
| `abstainedSources[]` | Source classes that could not be trusted or inspected. Non-empty when `level = unknown`. |
| `sourceRefs[]` | Redacted source references with kind, optional source schema, freshness, evidence state, confidence, optional hash, and bounded preview. |
| `redactionPosture` | Always `counts_ids_statuses_hashes_only_no_mail_body_no_command_argv_no_absolute_paths`. |

Queue-pressure source refs identify only source class and bounded state. They
must not contain raw mail bodies, raw command argv, host-private absolute paths,
full dirty file listings, raw Beads JSONL, stack traces, secrets, or unbounded
command output. Missing telemetry uses `evidenceState = "abstained"` or
`"missing"` and contributes to `level = "unknown"` unless another trusted,
fresh source independently establishes the level. Corrupt Agent Mail recovery
state uses `agent_mail_recovery_corrupt`; unavailable Agent Mail read state uses
`agent_mail_unavailable`. Contradictory sources use
`contradictory_source_state` and should make the advice collect more evidence
rather than authorize work.

Normalization rules:

- Missing required evidence becomes `freshness = missing`, `confidence = low`,
  and reason `missing_required_signal`; it never becomes a healthy default.
- Stale evidence becomes reason `stale_source_authority`. A caller may still
  emit lean advice for optional stale inputs, but safety-critical stale inputs
  force `abstain`.
- Contradictory evidence becomes reason `contradictory_evidence` and should
  force `abstain` unless the decision is only to collect more evidence.
- Unknown redaction posture becomes reason `redaction_posture_unknown` and
  forces `abstain`; support-safe evidence is required before handoff.
- Local Cargo observations can explain `refuse_local_cargo`, but they never
  provide proof that a Rust check passed. Remote-required proof must come from
  RCH.
- Work-packet and claim-gate consumers must apply their existing safety gates
  after reading admission advice. Admission reports shape timing and workload
  size; they do not grant ownership.

### Reason-Code Taxonomy

Reason codes are schema-pinned strings. New codes require a schema update and a
fixture in the implementing bead.

| Category | Codes |
|----------|-------|
| CPU | `cpu_logical_cores_constrained`, `cpu_logical_cores_portable`, `cpu_logical_cores_workstation`, `cpu_logical_cores_swarm` |
| Memory | `memory_available_constrained`, `memory_available_portable`, `memory_available_workstation`, `memory_available_swarm` |
| Disk | `disk_capacity_constrained`, `disk_capacity_sufficient`, `disk_capacity_swarm_ready` |
| Target dir | `target_dir_shared`, `target_dir_isolated`, `target_dir_external` |
| RCH topology | `rch_topology_missing`, `rch_topology_available`, `rch_topology_version_skew`, `rch_topology_workspace_metadata_blocked`, `rch_topology_remote_required` |
| Calibration freshness | `calibration_fresh`, `calibration_stale`, `calibration_partial`, `calibration_synthetic_only`, `calibration_contradictory`, `calibration_missing`, `calibration_unavailable` |
| Synthetic fixtures | `synthetic_fixture_constrained`, `synthetic_fixture_portable`, `synthetic_fixture_workstation`, `synthetic_fixture_swarm` |
| Overrides | `explicit_profile_override` |

## Profile Budgets

Each profile sets default budgets for:

| Category | Budget Keys | Description |
|----------|------------|-------------|
| **Search** | `search_candidate_limit`, `search_concurrent_index_readers` | Limits on search result pool and parallelism |
| **Pack** | `pack_max_tokens`, `pack_max_candidate_memories` | Context pack assembly limits |
| **Cache** | `cache_memory_cap_mb`, `cache_entry_cap` | In-memory cache sizing |
| **Write Spool** | `write_spool_queue_cap`, `write_spool_batch_cap` | Async write queue sizing |
| **Verification** | `verification_recipe`, `verification_timeout_class` | Test/lint gate behavior |

## Examples by Host Type

### Laptop (portable profile)

```bash
# Auto-detects portable for 4-core, 16GB laptop
ee profile config plan
# => profile: portable

ee profile config apply
```

### CI Runner (constrained profile)

```bash
# Force constrained for ephemeral CI
ee profile config apply --profile constrained
```

### Large Build Server (swarm profile)

```bash
# Auto-detects swarm for 128-core, 256GB server
ee profile config plan --json | jq '.data.profile'
# => {"recommended":"swarm","effective":"swarm","confidence":"high",...}

ee profile config apply
```

### Remote RCH Worker

When offloading builds via RCH, the remote worker uses its own profile:

```bash
# Local laptop plans portable
ee profile config plan
# => portable

# RCH worker auto-detects swarm
rch exec -- ee profile config plan
# => swarm
```

## Troubleshooting

### Probe warnings

If the probe cannot read CPU/memory info, it reports `complete: false`:

```bash
ee profile config plan --json | jq '.data.probe.degraded'
```

Fix: Check `/proc/meminfo` access or set explicit `--profile` override.

### Config conflicts

If `.ee/config.toml` has conflicting manual edits:

```bash
ee profile config plan --json | jq '.data.conflicts'
```

Fix: Resolve conflicts manually or remove the conflicting keys.

### Checking active profile

```bash
# Show current config without changes
ee profile config plan --json | jq '{
  profile: .data.profile.effective,
  configExists: .data.configExists,
  wouldWrite: .data.wouldWrite
}'
```

## Machine-Readable Schemas

| Schema | Description |
|--------|-------------|
| `ee.host_profile.v1` | Host resource probe results |
| `ee.host_calibration.host_class.v1` | Pure host-class classifier output for recommender internals |
| `ee.host_calibration.recommendation.v1` | Calibrated profile recommendation and budget deltas |
| `ee.resource_admission.v1` | Advisory resource-profile admission decision for agent workloads |
| `ee.profile.config.plan.v1` | Config plan/apply report |
| `ee.profile.runtime.v1` | Runtime profile status |

All outputs use `--json` for stable machine parsing.
