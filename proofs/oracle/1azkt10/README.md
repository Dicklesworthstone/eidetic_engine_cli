# Retrieval/index regression oracle evidence — bd-reality-core-convergence-1azkt.10

Content-addressed evidence from one live run of the retrieval/index regression
oracle (`tests/retrieval_index_regression_oracle.rs`) against an attested
candidate. Every file is named for the BLAKE3 of the `ee.test_event.v1` event
stream it holds or describes.

## Files

| File | What it is |
| --- | --- |
| `28cf9693f0ab261b6e237f43ace8f4636b9d0e6095e7ab01f5a6756afd887266.ee-test-event.jsonl` | The oracle's `ee.test_event.v1` stream, 11 events. `b3sum` of this file equals its name. |
| `28cf9693f0ab261b6e237f43ace8f4636b9d0e6095e7ab01f5a6756afd887266.bundle.json` | The run's context: the exact command, the rch receipt line, the worker, the `Cargo.lock` and candidate-binary SHA-256, the lane, and the NOT-ATTESTED list. |

Check the evidence with `b3sum --no-names <file>.ee-test-event.jsonl`. It must
print the file's own name. On the worker, the oracle echoed the body with its
BLAKE3 before the job ended. The retained copy was accepted only because its
digest matched both that echoed digest and the name of the worker's proof file.

## Producing command

Run from a canonical-root clone. The base is the pushed commit
`f1cbd6327a4bf066535099ab900e0c57e83b1ca0`.

```text
RCH_ENV_ALLOWLIST=VERGEN_GIT_SHA,VERGEN_GIT_DIRTY,ORACLE_REQUIRE_ATTESTATION,ORACLE_EXPECTED_COMMIT \
VERGEN_GIT_SHA=f1cbd6327a4bf066535099ab900e0c57e83b1ca0 VERGEN_GIT_DIRTY=false \
ORACLE_REQUIRE_ATTESTATION=1 ORACLE_EXPECTED_COMMIT=f1cbd6327a4bf066535099ab900e0c57e83b1ca0 \
RCH_REQUIRE_REMOTE=1 RCH_TEST_TIMEOUT_SEC=7200 RCH_BUILD_TIMEOUT_SEC=4500 \
rch exec --clean-overlay --no-overlay --base f1cbd6327a4bf066535099ab900e0c57e83b1ca0 -- \
  cargo test --locked --test integration_n_r -- \
  retrieval_index_regression_oracle::concurrent_retrieval_over_one_generation_is_classified \
  --exact --ignored --test-threads=1 --nocapture
```

The run went to worker vmi1227854, 2026-09-23T16:22:00Z to 16:38:24Z. Receipt:
`[RCH] clean-overlay receipt: base=f1cbd6327a4bf066535099ab900e0c57e83b1ca0`.
Verdict: `RACE_ABSENT`, from 3 rounds × {search, pack}, 8 probes each, quorum 7.

## Lane

This is the **substitute lane**, not `scripts/rch_verify.sh`. It was accepted
conditionally by GraniteKite in bead comments c9964 and c9970 (option C). Under
that ruling:

- The candidate is the `ee` binary built by the same `cargo test` invocation.
  Its `gitCommit` must equal the `--base` sha.
- In the same process, the oracle scans the worker's cargo config and
  environment for resolution redirects, and treats any hit as INFRA_ERROR.
  This run probed 12 config locations, found no files, and had no hits.

## What these files are NOT

- **Not a proof that divergence is absent.** This is one run under shared
  fleet load: 6 concurrent rounds, each of which found no disagreement among
  completed probes. It does not show that no interleaving can diverge.
- **Not `rch_verify.sh` attestation.** No `ee.rch.verify` envelope was produced
  (no source, bundle or dependency-manifest hash, no git tree id). The crates.io
  tarballs were **not** verified against the `franken-stack.lock` source
  revisions. `frankentorch-*` has no `franken-stack.lock` revision at all. There
  was no build admission, local-Cargo tripwire or known-blocker bookkeeping.
- **Not a statement about the released 0.14.2 binary.** That historical
  evidence stays historical; these files describe commit `f1cbd6327a4b`.

## Durable-mutation attribution and the rows check

The oracle's first durable-mutation check compared workspace file bytes. At
`42c8408` it went red on `ee.db`, its WAL files and `ee.write.lock` while
every probe agreed. The `read_only_window_attribution` test found the cause
arm by arm on one workspace:

| Arm | What ran |
| --- | --- |
| N1, N2 | Nothing (null controls) |
| S | `ee index status --json` |
| PS | 8 concurrent `ee search` probes |
| PP | 8 concurrent `ee pack --read-only` probes |
| W | One `ee remember` (positive control for the row digest) |

The finding: search appends to `audit_log`, and it is *declared* to
(`append_only_write("search", vec!["audit_log"], ...)` in
`src/core/effect.rs`). `index status` and `pack --read-only` changed no rows.
So the byte check was treating a declared write as a mutation.

Under GraniteKite's 20:40Z ruling (bead comment 10042), the check now judges
rows:

- The allowlist comes from `EffectManifest` via the real CLI parse, never a
  hand-list.
- Declared tables are append-only by rowid.
- Undeclared tables must stay row-equal.
- `ee.write.lock` follows the bd-xa6ud precedent.
- The database files' byte churn is classified, with any `ee.db` byte change
  recorded as UNEXPLAINED.

Every run below used `rch exec --clean-overlay --base <sha>` with the oracle
file and `src/db/mod.rs` as overlays. The overlays make these runs
**unattested**: they verify the instrument, not a product verdict.

| File | Run | What it holds |
| --- | --- | --- |
| `577dae27da9a343fbd4b4a6d3da864426c2d9887f4ee8956d0e7f2558da8178b.ee-test-event.jsonl` | Attribution run 1. Base `70c459bca8028e5871482931be7e24389ff1d6c9`, worker hz4, 2026-09-23T19:37-20:07Z. | Byte and WAL results per arm. The row digest FAILED in every arm: a read-only open of a copied WAL database is refused ("recovery in progress"). No row classification. |
| `c5dbda5eff2c36a3da292b5288d9364ae85979f72d0e7f4c4808b0397cb98f57.ee-test-event.jsonl` | Attribution run 2. Base `4510aeee3f1a2a97a88626af70338afe13da77e5`, hz4, 20:08-20:35Z. | The attribution the ruling rests on. PS changed only `audit_log` (8 -> 56 rows = 8 probes × 6 audit rows). S and PP changed no rows. W changed `audit_log`, `memories`, `search_index_jobs` and `workspace_generations`. |
| `999b1a27f48546f7769410bcbed5b789e1b22f670b774322e024a2866829bdf9.ee-test-event.jsonl` | Live oracle, default config, rows check in place. Base `8952b4bfa923bf8d594a9acb80e91578c8113e06`, worker vmi1227854, 21:14-21:31Z. Receipt overlay fingerprint `8ec1db2aed691628242700e8eebb404ca680f7155243a268d97c8369a2342cc5`. | `RACE_ABSENT`. The derived allowlist is `[audit_log, context_packs, pack_items]`. Only `audit_log` changed, with no append-only finding. The lock epoch went 357 -> 436. `ee.db` bytes changed and are recorded as `db-bytes-UNEXPLAINED`. |
| `a36718c8cca175bfc948e7274f3d4acdf7819d7a9f33f33cfcd69beb8a8db944.ee-test-event.jsonl` | Attribution, same job as `999b1a27`. | The per-arm row judgment is empty in N1, S, PS, PP and N2. W is not judged. |

**Retention disclosure for `577dae27` and `c5dbda5e`.** These two bodies were
echoed on stderr. rch relays the remote stdout into its stderr without ordering
the two streams, so libtest's own stdout lines landed inside the echoed block:

- `577dae27`: 2 lines.
- `c5dbda5e`: 4 lines.

The extractor refused both. The retained files are the echoed block with
exactly those whole, non-JSON lines removed. Each file's `b3sum` equals the
digest the worker announced and the name of its proof file, and every line
parses as JSON.

The echo now goes to stdout. `999b1a27` and `a36718c8` came through the
extractor unmodified.

**What these files are NOT:**

- They are not attested verdicts. The attested run of the landed oracle comes
  after these files.
- They do not explain the `ee.db` byte change. It stays UNEXPLAINED (ruling
  condition 6); the row judgment only covers its consequence.
- `context_packs` in the derived allowlist is not a table that exists. The
  `pack build` declaration at `src/core/effect.rs:2533` names it, but the
  schema's pack table is `pack_records`. That declaration drift is reported on
  the bead and is not fixed here.

## Attested runs of the rows-check oracle at `25a9d7f`

These two runs are the oracle's first attested verdicts with the rows check in
place. The base is the pushed commit
`25a9d7f8d47712559059f9daf91402e23891d7b3`, which contains the rows check
(`7e07a91`). The lane is the substitute lane of c9964/c9970, the same as the
first run above: `--no-overlay`, the candidate built by the same `cargo test`,
its `gitCommit` checked against `ORACLE_EXPECTED_COMMIT`, and the build
environment scanned in-process.

```text
RCH_ENV_ALLOWLIST=VERGEN_GIT_SHA,VERGEN_GIT_DIRTY,ORACLE_REQUIRE_ATTESTATION,ORACLE_EXPECTED_COMMIT,ORACLE_COLD_CONCURRENT \
VERGEN_GIT_SHA=25a9d7f8d47712559059f9daf91402e23891d7b3 VERGEN_GIT_DIRTY=false \
ORACLE_REQUIRE_ATTESTATION=1 ORACLE_EXPECTED_COMMIT=25a9d7f8d47712559059f9daf91402e23891d7b3 \
ORACLE_COLD_CONCURRENT=<empty for Run A, 1 for Run B> \
RCH_REQUIRE_REMOTE=1 RCH_TEST_TIMEOUT_SEC=7200 RCH_BUILD_TIMEOUT_SEC=4500 \
rch exec --clean-overlay --no-overlay --base 25a9d7f8d47712559059f9daf91402e23891d7b3 -- \
  cargo test --locked --test integration_n_r -- \
  retrieval_index_regression_oracle::concurrent_retrieval_over_one_generation_is_classified \
  --exact --ignored --test-threads=1 --nocapture
```

Both runs printed the receipt
`[RCH] clean-overlay receipt: base=25a9d7f8d47712559059f9daf91402e23891d7b3 overlay-fingerprint=fe0d5151031be8fda7951fe7fe1f42f7ce344018fdb3ed21e6ada866b230b195`.
In both, `observedCommit` equalled `expectedCommit` (`25a9d7f`), 12 config
locations were probed, and there were no build-environment hits.

| File | Run | What it holds |
| --- | --- | --- |
| `3a216754ed5d565f25dd6fd21635877f4515020558aea42c5100faec2c8ec73e.ee-test-event.jsonl` | Run A, the default order: serial-cold first touch, then 3 concurrent rounds. Worker hz4, 2026-09-23T21:36-22:17Z. | `RACE_ABSENT`. Every round agreed (quorum 7 of 8). |
| `51003314b443b392264996e8b7cf345b821b2a50e20bda501f2cabfd36818091.ee-test-event.jsonl` | Run B, `ORACLE_COLD_CONCURRENT=1`: a concurrent-cold search round and pack round as the first touch, then 3 concurrent rounds. Worker hz3, 22:17-23:28Z. | `RACE_ABSENT`. The cold rounds and every warm round agreed, 8 of 8 each. |

Together, the two runs cover the four no-model cells of ruling 17:12Z item 3
(serial or concurrent, cold or warm) at this base. The rows check gave the
same result in both runs:

- only `audit_log` changed (search's declared append);
- the append-only check found nothing;
- the generations stayed at 8;
- the `ee.write.lock` epoch did not go backwards;
- the `ee.db` byte change is recorded as `db-bytes-UNEXPLAINED`.

Both bodies were echoed on stdout and came through the extractor unmodified.
Each file's `b3sum` equals its name.

**What these files are NOT:**

- **Not a model-backed verdict.** Every probe ran on `hash_fallback`
  (`embed_model_unavailable`, `lexical_only`). The model dimension is item 4.
- **Not index-not-found end to end.** That is item 5 (the corrected plant).
- **Not a statement about #49.** The outside PR at
  `5dfa99a822007780771d34e482ea0d523165ff75` ("perf(drift): resolve git facts
  once per pack and per drift report") changes pack assembly. It landed after
  `25a9d7f`, and these runs do not cover it.
- **Not a proof that no interleaving diverges.** Each run is one run under
  shared fleet load.
- **Not `rch_verify.sh` attestation.** The same NOT-ATTESTED list as the first
  run above applies.
