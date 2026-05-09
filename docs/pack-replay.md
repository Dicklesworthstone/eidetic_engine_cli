# Context Pack Replay and Evidence Freshness

This document explains when to use pack replay versus live re-retrieval, how to
interpret stale evidence warnings, and how to attach safe support artifacts.

## Overview

Context packs are the primary `ee` user experience (ADR 0007). A pack includes:
- Selected memories with provenance and explanations
- A selection certificate proving deterministic ranking
- Score components, token estimates, and diversity keys
- Degradation records when dependencies are unavailable

ADR 0025 adds **selection ledgers** so packs can be replayed and compared after
the system changes. The ledger is an audit artifact, not a second source of
truth for memories.

## Pack Replay vs Live Re-Retrieval

| Mode | Question | Uses |
|------|----------|------|
| **Replay** | "What did this pack see when it was created?" | Stored ledger from `pack_records` |
| **Live re-retrieval** | "What would the same query produce now?" | Current indexes, caches, trust state |

Replay is reconstruction from persisted evidence. Live re-retrieval is a new
query that happens to share parameters with an old pack. They answer different
questions.

### When to use replay

- Debugging why a memory appeared or disappeared in a specific pack
- Proving that the selection explanation matches the pack you shipped
- Generating support artifacts that avoid re-running retrieval on production
- Comparing two historical packs without current system noise

### When to use live re-retrieval

- Seeing what the system would produce today
- Measuring whether index rebuilds, graph refreshes, or trust changes affected
  ranking
- A/B testing different profiles, budgets, or query formulations

## Evidence Freshness States

Each memory's provenance can have a freshness state:

| State | Meaning | Repair |
|-------|---------|--------|
| `fresh` | Source evidence matches the stored hash/metadata | None |
| `missing_source` | Source file or session no longer exists | Re-import or re-remember with current evidence |
| `changed_source` | Source exists but content changed | Inspect the change, re-import if intentional |
| `unreachable_source` | Source path exists but cannot be read | Check permissions, re-import |
| `unsupported_source` | Provenance scheme cannot be verified | Manual inspection or re-remember with verifiable source |
| `unknown` | Freshness check did not run or data unavailable | Trigger explicit freshness check |

Freshness states **warn and explain**. They do not silently delete, demote, or
rewrite memories. Any lifecycle mutation goes through existing curation and
audit pathways.

### Interpreting freshness in packs

A `context` or `pack` response may include freshness states per item:

```json
{
  "memoryId": "mem_...",
  "freshness": "changed_source",
  "freshnessRepair": "ee remember --update --source <path>"
}
```

When freshness is not `fresh`, consider:
1. Is the change intentional (file edited, ADR superseded)?
2. Should the memory be re-imported to reflect the new source state?
3. Is this a false alarm from a transient read failure?

## Support Bundle Artifacts

Support bundles include redaction-safe replay/freshness metadata. The key
artifact is `pack_replay_summary.json`:

```json
{
  "schema": "ee.support_bundle.pack_replay_summary.v1",
  "status": "available",
  "redactionStatus": "ids_hashes_counts_codes_only_no_query_text_no_memory_content",
  "database": { ... },
  "packs": [
    {
      "packId": "pack_...",
      "packHash": "blake3:...",
      "ledger": {
        "status": "available",
        "hashVerified": true,
        "schema": "ee.pack_replay_ledger.v1",
        "selectedItemCount": 5,
        "omittedItemCount": 2,
        "freshnessStates": { "fresh": 4, "changed_source": 1 },
        "redactionClasses": [],
        "degradationCodes": [],
        "derivedAssets": { ... },
        "candidateCounts": { ... }
      }
    }
  ]
}
```

### What support bundles include

- Pack IDs and hashes (identifiers, not content)
- Ledger status and integrity (hash verified, schema version)
- Aggregate counts (items, omissions, candidates)
- Freshness state distribution (no raw provenance text)
- Redaction class presence (no raw secret content)
- Degradation codes (stable machine codes)
- Derived asset references (generations, not full indexes)

### What support bundles exclude

- Raw query text or query-file content
- Memory content or raw evidence
- Unredacted provenance payloads
- Secret-like spans from any source

This ensures bundles are safe to share without exposing sensitive data.

## Commands

### Pack replay

```bash
ee pack replay <pack-id> --json
```

Reconstructs the selection explanation from the stored ledger. Reports
`ledger_unavailable` as a degradation if the pack predates ledger storage.

### Pack diff

```bash
ee pack diff <old-pack-id> <new-pack-id> --json
```

Compares two ledgers and reports:
- Added, removed, changed, and unchanged items
- Score deltas
- Degradation deltas
- Redaction deltas
- Freshness deltas
- Likely owner hints (`request`, `retrieval`, `packing`, `trust`, `redaction`,
  `freshness`, `profile`, `unknown`)

Owner hints are explanations, not automatic assignments.

### Support bundle creation

```bash
ee support bundle --output <path>
```

Creates a redaction-safe bundle including `pack_replay_summary.json`.

### Support bundle inspection

```bash
ee support inspect <bundle-path> --json
```

Validates bundle integrity and reports summary without exposing raw content.

## Pack Quality Sentinel

Use the pack-quality sentinel when the question is whether a canonical task still
gets the evidence it needs. It is an `eval` command over deterministic fixtures,
not a performance benchmark, a support bundle, or a replacement for `ee pack
diff`.

```bash
ee eval run release_failure --pack-quality --scenario usr_pre_task_brief --json
```

Run the command without `--scenario` to evaluate every pack-quality case in the
fixture family. JSON stdout uses the `ee.response.v1` envelope and carries
`ee.eval.pack_quality_report.v1`.

### Interpreting Results

The report's `aggregate_verdict` is the first field to inspect:

| Verdict | Meaning | Usual next step |
|---------|---------|-----------------|
| `within` | Selection, provenance, degradation, redaction, and budget expectations matched | Keep the fixture as regression coverage |
| `drift` | Behavior changed without a known critical omission or forbidden leak | Inspect selected/omitted IDs and decide whether to update expectations |
| `regression` | Critical evidence was omitted, forbidden evidence leaked, or an unexpected degradation appeared | Fix retrieval/packing/redaction behavior before updating fixtures |
| `inconclusive` | Required fixture, workspace, ledger, or derived-asset evidence was unavailable or malformed | Repair the fixture or rerun after restoring the missing evidence |

Useful JSON checks:

```bash
ee eval run release_failure --pack-quality --json \
  | jq '.data.report.aggregate_verdict'

ee eval run release_failure --pack-quality --json \
  | jq '.data.report.comparisons[] | {case_id, verdict, failure_reasons}'

ee eval run release_failure --pack-quality --json \
  | jq '.data.degradedBranches[]? | {code, repairAction}'

ee eval run release_failure --pack-quality --json \
  | jq '.data.artifactPaths[] | {scenarioId, stdout, stderr}'
```

Failure triage:

1. Start with `failure_reasons` and the comparison row for the failing case.
2. Check `expected_selected_memory_ids` against `actual_selected_ids` before
   looking at aggregate metrics.
3. Treat `critical_omitted_memory_ids` matches as regressions unless the fixture
   is wrong.
4. Check `unexpected_degradation_codes` and `actual_redaction_leaks` before
   changing expected memory IDs.
5. Use the reported artifact paths to inspect stdout/stderr from the real-binary
   scenario run.
6. Update fixture expectations only after the new behavior is intentional and
   documented.

## Fixture Authoring

Pack-quality cases live with normal eval fixtures under
`tests/fixtures/eval/<fixture-family>/`. A complete fixture family has:

| File | Purpose |
|------|---------|
| `README.md` | Human intent, user workflow, expected signal, artifact location |
| `source_memory.json` | Deterministic synthetic memories and stable memory IDs |
| `scenario.json` | Command sequence, expected stdout contracts, degraded branches, pack-quality expectations |

Add a `pack_quality_expectations` block to `scenario.json`:

```json
{
  "schema": "ee.eval.pack_quality_expectations.v1",
  "cases": [
    {
      "case_id": "pq.release_failure.context.v1",
      "scenario_id": "usr_pre_task_brief",
      "command_step": 4,
      "query_surface": {
        "kind": "inline_query",
        "query": "prepare release"
      },
      "expected_selected_memory_ids": [
        "mem_00000000000000000000000101"
      ],
      "critical_omitted_memory_ids": [],
      "min_provenance_density": 1.0,
      "allowed_degradation_codes": [
        "semantic_disabled"
      ],
      "forbidden_redaction_leaks": [
        "secret",
        "token"
      ],
      "token_budget": {
        "max_tokens": 4000,
        "expected_used_tokens_max": 1200,
        "expect_truncation": false
      },
      "stable_first_failure_label": "missing_release_failure_context"
    }
  ]
}
```

Authoring rules:

1. Use stable fixture IDs, scenario IDs, memory IDs, clocks, and hashes.
2. Keep source memories synthetic and secret-free unless the fixture is
   explicitly testing redaction with safe probes.
3. Put only memories that must appear in `expected_selected_memory_ids`.
4. Put memories whose selection would be harmful in
   `critical_omitted_memory_ids`.
5. List expected degraded modes in `allowed_degradation_codes`; do not leave
   degraded behavior implicit.
6. Require `min_provenance_density` high enough to catch unsupported selection.
7. Give every case a stable, grep-friendly `stable_first_failure_label`.
8. Update the fixture README when the user workflow or expected signal changes.

## Test Coverage

The following test families cover replay, freshness, and egress:

| File | Coverage |
|------|----------|
| `tests/e2e_pack_determinism.rs` | Ledger persistence, deterministic selection |
| `tests/no_mocks_e2e.rs` | Real-binary pack with ledger assertions |
| `tests/redaction_egress_no_mock.rs` | Egress matrix for secret-like spans |
| `tests/freshness_contracts.rs` | Freshness states, changed_source E2E |
| `tests/degraded_honesty.rs` | Support bundle schema validation |
| `tests/support_bundle_perf_compare.rs` | Bundle profile/perf evidence |
| `tests/json_contract_snapshots.rs` | Stable pack ledger JSON contracts |

Run targeted tests:
```bash
rch exec -- cargo test --test e2e_pack_determinism
rch exec -- cargo test --test redaction_egress_no_mock
rch exec -- cargo test --test freshness_contracts
```

## Schema References

| Schema | Purpose |
|--------|---------|
| `ee.pack_replay_ledger.v1` | Full selection ledger stored with pack_records |
| `ee.pack_replay.v1` | Replay command stdout contract |
| `ee.pack_diff.v1` | Diff command stdout contract |
| `ee.support_bundle.pack_replay_summary.v1` | Support bundle artifact |

## ADR References

- ADR 0007: Context Packs Primary UX
- ADR 0025: Replayable Context Pack Selection Ledgers
