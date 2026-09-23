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
