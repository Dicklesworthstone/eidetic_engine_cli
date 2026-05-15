# ADR 0023: Host-Adaptive Operating Profiles

Status: proposed
Date: 2026-05-08

## Context

ADR 0017 defines how `ee` should handle swarm-scale resource pressure: bounded
caches, explicit write backpressure, derived-asset reports, support-bundle
evidence, and RCH-friendly scale verification. The `eidetic_engine_cli-fcq1`
track covers that resource-governance implementation surface.

The next gap is not another cache governor or write spool. Agents run `ee` on
very different hosts: small laptops, ordinary workstations, and large swarm
machines. Today the safe defaults for cache size, search candidate pools, pack
budgets, write-spool behavior, steward jobs, and verification intensity are
implicit. That makes high-capacity hosts underused and constrained hosts
surprising when a default assumes more CPU, memory, or temporary disk than is
available.

The project needs a small contract that names host operating profiles, records
the local probe evidence used to choose them, maps that evidence to explicit
budgets, and reports degradation honestly. This must remain a configuration and
reporting layer over the existing CLI-first product. It must not turn `ee` into
an agent scheduler, daemon requirement, package manager, profiler, or duplicate
implementation of the resource-governance work from ADR 0017.

## Decision

`ee` will define host-adaptive operating profiles as deterministic local
recommendations. A profile has four parts:

1. A named profile ID.
2. A redaction-safe local resource probe.
3. Deterministic recommendation rules that map probe evidence plus explicit
   config overrides to budget fields.
4. A stable report that explains the selected profile, applied overrides,
   degraded capabilities, and verification recipe.

The initial profile IDs are:

- `constrained`: safe fallback for missing probe data, low memory, low temporary
  disk, restricted environments, or invalid/conflicting config.
- `portable`: low-resource local development machines. Prefer small bounded
  caches, low concurrency, smaller candidate pools, and short verification
  recipes.
- `workstation`: the default balanced profile for ordinary developer machines.
  Use moderate derived caches, candidate pools, write-spool batches, and local
  verification recipes.
- `swarm`: high-core, high-memory machines used by many agents. Use larger
  derived caches, wider search and pack budgets, larger write-spool and steward
  windows, and RCH-safe verification recipes.

Users may explicitly request a profile or override individual budgets in config.
Overrides do not hide the base recommendation. The report must preserve both
the automatic recommendation and the effective profile after overrides.

The local probe contract includes only data needed for resource decisions:

- CPU count and, when available, physical-core count.
- Total memory and process/cgroup memory limit when the platform exposes it.
- Free bytes for the workspace, database directory, index/cache directory, and
  temporary build/cache directory.
- Whether the workspace, database, cache, and temporary directories resolve to
  distinct paths or compete for the same filesystem budget.
- Availability of required local tools for the selected verification recipe,
  including `cargo`, `rustfmt`, `clippy`, `br`, `bv`, `rch`, and `gh` only when
  a recipe needs them.
- Configured profile, budget overrides, and redacted config source paths.
- Optional environment hints that are already visible to the process, such as
  `TMPDIR`, `CARGO_TARGET_DIR`, and an RCH availability hint.

Probe output must not include secrets, raw environment dumps, command output,
home-directory contents, or arbitrary path listings. Path evidence should use
workspace-relative paths, stable labels, or redacted absolute paths according to
the policy module.

Recommendation rules are deterministic:

- The same probe JSON, config, and `ee` version produce the same selected
  profile and budget report.
- Rules use explicit thresholds and stable tie-breaking. They do not depend on
  volatile load averages, current process count, network latency, benchmark
  timing, or wall-clock time.
- Missing optional probe fields lower confidence and may select
  `constrained`, but they must not cause silent fallback.
- More available CPU, memory, and disk must not select a more constrained
  profile unless an explicit override, hard policy limit, or degraded condition
  explains the result.
- All overrides carry provenance: source, field, accepted/rejected status,
  reason code, and repair hint for invalid values.

The budget report is the public contract for later implementation beads. It
will expose at least these fields:

- `search`: candidate limit, concurrent index readers, semantic/lexical
  fallback posture, and stale-index tolerance.
- `pack`: max tokens, max candidate memories, context-pack pruning limit, and
  explanation verbosity.
- `cache`: derived-cache memory cap, entry cap, hotset prewarm limit, and
  generation-check behavior.
- `writeSpool`: queue cap, batch cap, coalescing window, retry budget, and
  backpressure severity threshold.
- `steward`: maintenance job window, graph/index refresh budget, and optional
  daemon/prewarm allowance.
- `verification`: local/RCH recipe name, target-dir posture, timeout class, and
  whether heavy verification should be skipped, queued, or offloaded.
- `diagnostics`: support-bundle evidence fields, degraded codes, first-failure
  diagnosis level, and redaction posture.

Profile reports use a stable JSON shape before any command starts depending on
them:

```json
{
  "schema": "ee.profile.report.v1",
  "profile": {
    "recommended": "workstation",
    "effective": "workstation",
    "confidence": "high",
    "reasons": []
  },
  "probe": {
    "schema": "ee.host_profile.v1",
    "complete": true,
    "redaction": "policy_applied"
  },
  "budgets": {},
  "overrides": [],
  "degraded": [],
  "verification": {}
}
```

Specific field names and numeric thresholds belong in the implementation beads
and golden fixtures. This ADR fixes the shape and invariants: profiles are
explicit, evidence-backed, deterministic, override-aware, and honest under
degradation.

## Consequences

The profile system gives operators and agents one place to understand why `ee`
picked small, balanced, or swarm-scale budgets. A support bundle can include the
profile report without including secrets or raw host inventory. A later config
command can dry-run profile changes without mutating durable memory.

Small hosts get safer defaults because missing probe data, tight temporary disk,
or low memory select a conservative profile with visible repair hints. Large
hosts get a way to use bigger cache and verification budgets without editing
many unrelated settings by hand.

The separation from ADR 0017 is intentional:

- ADR 0017 owns cache-governor behavior, write-spool semantics, scale fixtures,
  contention E2E tests, and support-bundle artifact mechanics.
- This ADR owns the host probe, named profile contract, deterministic mapping
  from probe to budgets, profile config dry-run semantics, profile evidence in
  reports, and profile-specific verification recipes.

This makes some implementation work stricter:

- Resource probing must be platform-tolerant and redaction-safe.
- Budget defaults must live behind named profiles instead of being scattered
  across command handlers.
- Tests must freeze report shapes and threshold behavior before profile budgets
  influence core commands.
- Profile selection must be explainable even when the result is conservative.

## Rejected Alternatives

- **Use one fixed default for every host.** That is simple but wastes large
  hosts and creates hidden pressure on constrained hosts.
- **Autotune from recent timings or load averages.** Timings are useful
  diagnostics, but they make profile selection non-deterministic and difficult
  to test.
- **Let each subsystem probe resources independently.** That scatters policy,
  makes reports inconsistent, and encourages silent drift in cache, search,
  pack, write-spool, steward, and verification defaults.
- **Require the daemon to own host profiles.** Profiles are local configuration
  and report data. Ordinary CLI commands must work without a daemon.
- **Duplicate ADR 0017 implementation work.** Host profiles choose and report
  budgets; cache governors, write-spool behavior, support-bundle mechanics, and
  contention tests remain in the resource-governance surface.
- **Expose full host inventory.** The probe must collect only the redaction-safe
  inputs needed for deterministic budget decisions.

## Verification

The decision remains true when the profile track proves all of the following:

1. `br dep cycles --json` reports no cycles for the `eidetic_engine_cli-k8dp`
   planning track.
2. Unit tests cover probe normalization for complete, partial, and unavailable
   host evidence.
3. Unit and property tests cover deterministic profile selection, stable
   tie-breaking, override precedence, invalid override rejection, and the
   monotonicity rule that more available resources do not select a more
   constrained profile without an explicit reason.
4. Golden tests freeze `ee.host_profile.v1` and `ee.profile.report.v1`
   response shapes for `constrained`, `portable`, `workstation`, `swarm`, and
   override cases.
5. Config dry-run tests prove profile changes report planned mutations,
   accepted/rejected overrides, repair hints, and command-effect summaries
   before any durable config write.
6. Runtime report tests prove commands that consume profile budgets preserve
   their existing stdout/stderr discipline and include profile provenance in
   diagnostics or JSON fields where applicable.
7. Verification recipe tests prove `portable`, `workstation`, and `swarm`
   recipes are RCH-safe, set explicit `CARGO_TARGET_DIR` guidance, and never
   require destructive cleanup.
8. Support-bundle tests include redacted profile reports, probe completeness,
   selected budgets, degraded codes, and profile-related first-failure
   diagnosis without leaking secrets.
9. E2E tests exercise at least one complete synthetic probe for each named
   profile and one missing-probe case that selects `constrained` with a stable
   repair hint.
10. Forbidden-dependency audits continue to reject Tokio, rusqlite, petgraph,
    and other banned crates.
