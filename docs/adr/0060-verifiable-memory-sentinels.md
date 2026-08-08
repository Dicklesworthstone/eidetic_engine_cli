# ADR 0060: Verifiable Memory Sentinels

Status: proposed
Date: 2026-06-07
Bead: bd-1n0np.16.1
Extension bead: bd-wake-on-condition-inverse-sentinel-65uci

## Context

Many `ee` facts are **contract-shaped** and therefore deterministically
checkable: schema fields (`ee.response.v2`), env vars (`EE_*`), degraded-code
fixtures, config keys, file/span existence, dependency capabilities, command-help
flags. The most dangerous memories are stale facts that still sound
authoritative. The 2026-06-07 review (score 745) proposed letting a human/agent
attach a small explicit validity check (a *sentinel*) to a memory so `ee` keeps
verifying it. This complements but does not duplicate ADR 0056 (automatic
code-symbol drift) or the provenance re-verification ADR (cited-evidence
referents): a sentinel is a **user-declared predicate** over any memory.

## Decision

- `MemorySentinelSpec { memory_id, sentinel_kind, polarity, target,
  expected_predicate, safety_class, provenance }`; `MemorySentinelResult { pass |
  fail | unknown | degraded, checked_at, evidence_summary, result_hash,
  stale_threshold }`. Polarity is `gate` or `revive`. Existing Gate hash inputs
  remain unchanged; Revive hashes are domain-separated so opposite meanings can
  never alias.
- Conservative kinds (pure/read-only first): `path_exists`, `file_hash_or_marker`,
  `json_schema_contains_field`, `config_key_exists`, `env_var_registered`,
  `degraded_code_fixture_exists`, `dependency_capability_present`, and
  `command_help_contains_flag` (**allowlisted local commands only**, run **only**
  on an explicit `ee sentinel check`).
- Surfaces: `ee remember --sentinel <kind>:<target>`, `ee remember --revive-when
  <kind>:<target>`, `ee sentinel check`, `ee tripwire check --revivals`, `ee why
  --include-sentinel`, `ee pack --require-fresh-sentinels`. Sentinel state
  feeds why (last-verified), pack (downgrade/flag failed/stale), curate
  (refresh/retire candidates), doctor/status (stale posture).
- `--sentinel` and `--revive-when` use the same bounded parser and are both
  completely validated before opening the remember write path. Invalid input
  cannot create a memory, consume an idempotency key, or produce dry-run state.
- Gate is the serving polarity: a fresh failed Gate may withhold its owning
  memory when fresh sentinels are required. Revive is inverse and never gates
  retrieval. `ee tripwire check --revivals` uses a single workspace-scoped join
  to current, non-tombstoned, non-expired owning memories, evaluates only Revive
  specs, and returns only predicates that pass now.
- The revival surface is literal read-only inspection. It emits redaction-safe
  `ee.memory_sentinel.revivals.v1` JSON and useful human output without memory
  content, provenance, or raw sentinel targets. A domain-separated target digest
  preserves stable identity alongside `memoryId`, `specHash`, and kind. The
  command does not persist the ephemeral check result or automatically change
  trust, validity, or tombstone state. Resurfacing remains an explicit future
  curation operation.
- Revival evaluation has two truthful observation modes. Explicit
  `ee tripwire check --revivals` may run the allowlisted `ee ... --help`
  introspection predicate under strict wall-time and redacted captured-output
  caps. The implicit revival-sentinel evaluator used by consumers such as
  `ee orient` calls only `observe_sentinel`, so `command_help_contains_flag`
  stays unknown and that evaluator executes no process; this does not claim
  that unrelated orient components are process-free. Safe path, file, schema,
  and env-registry predicates remain live. The payload identifies
  `observationMode`, `evaluationPosture`, and whether command-help process
  execution is enabled.
- `ee tripwire check --revivals` evaluates at most 25 deterministically ordered
  specs by default and accepts validated `--limit 1..=100`. It reports matched,
  evaluated, and unevaluated counts plus an explicit higher-limit repair so the
  cap is never silent. It deliberately has no continuation cursor: readiness
  depends on live filesystem, environment-registry, and (only in explicit mode)
  command-help observations that can change without a database generation.
- Adding polarity to check and why rows changes those public aggregates, so the
  containing contracts are `ee.memory_sentinel.check.v2` and
  `ee.memory_sentinel.why.v2`; why spec rows are
  `ee.memory_sentinel.spec.v2`. Stored spec/result hash schemas stay v1 because
  the Gate hash contract remains byte-stable and Revive is domain-separated.
- **Safety**: v1 supports **no arbitrary shell** sentinels — only pure
  filesystem/config/schema predicates + the allowlisted, read-only
  command-introspection kind, under asupersync budgets + strict per-check caps. A
  failed sentinel produces **evidence + a curation candidate**, never an automatic
  rewrite (no-silent-mutation).

## Consequences

- **Easier**: high-value contract-shaped memories become self-checking; a human
  invests once (attach a sentinel) and `ee` surfaces freshness forever.
- **Guarded**: arbitrary-command-execution gravity is resisted by the
  pure-predicate v1 + allowlist; brittle UX is mitigated by `ee sentinel explain`
  + good repair messages.
- **Intentionally impossible**: no arbitrary shell execution; no auto-mutation on
  Gate failure or Revive pass.

## Rejected Alternatives

- **Arbitrary shell sentinels**: a back door around the safety model; rejected
  for pure predicates + a tiny allowlist.
- **Auto-retire failed-sentinel memories**: violates no-silent-mutation; rejected
  for curation candidates.
- **Fold into ADR 0056/provenance**: those are *automatic* drift detectors; a
  user-declared predicate over any memory is a distinct mechanism; kept separate.

## Verification

- Unit + golden (bd-1n0np.16.5): per-kind pass/fail/unknown/degraded; malformed
  spec rejection; no-arbitrary-shell guard; failure → candidate (no mutation);
  `result_hash` stability.
- e2e `scripts/e2e_sentinels.sh`: attach → pass → target change → fail → refresh
  candidate + pack downgrade → `--require-fresh-sentinels` enforcement; malformed
  revival byte/row no-mutation proof; read-only revival happy path; Gate exclusion
  from the revival result set; explicit-vs-implicit observation; safe predicate
  parity; provider bounds.
