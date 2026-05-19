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
| Calibration freshness | `calibration_fresh`, `calibration_stale`, `calibration_missing`, `calibration_unavailable` |
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
| `ee.profile.config.plan.v1` | Config plan/apply report |
| `ee.profile.runtime.v1` | Runtime profile status |

All outputs use `--json` for stable machine parsing.
