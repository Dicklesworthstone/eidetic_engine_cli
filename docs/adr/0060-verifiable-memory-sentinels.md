# ADR 0060: Verifiable Memory Sentinels

Status: proposed
Date: 2026-06-07
Bead: bd-1n0np.16.1

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

- `MemorySentinelSpec { memory_id, sentinel_kind, target, expected_predicate,
  safety_class, provenance }`; `MemorySentinelResult { pass | fail | unknown |
  degraded, checked_at, evidence_summary, result_hash, stale_threshold }`.
- Conservative kinds (pure/read-only first): `path_exists`, `file_hash_or_marker`,
  `json_schema_contains_field`, `config_key_exists`, `env_var_registered`,
  `degraded_code_fixture_exists`, `dependency_capability_present`, and
  `command_help_contains_flag` (**allowlisted local commands only**, run **only**
  on an explicit `ee sentinel check`).
- Surfaces: `ee remember --sentinel <kind>:<target>`, `ee sentinel check`,
  `ee why --include-sentinel`, `ee pack --require-fresh-sentinels`. Sentinel state
  feeds why (last-verified), pack (downgrade/flag failed/stale), curate
  (refresh/retire candidates), doctor/status (stale posture).
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
  failure.

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
  candidate + pack downgrade → `--require-fresh-sentinels` enforcement.
