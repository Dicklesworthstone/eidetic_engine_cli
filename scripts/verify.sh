#!/bin/bash
set -euo pipefail

# EE-TST-LP4P-GAP-001 / EE-TST-LP4P-GAP-004: Central Verification Runner
#
# This script orchestrates the readiness gates for Eidetic Engine (ee).
# It executes standard tests, forbidden dependency checks, and the
# complex E2E/boundary migration pipelines.
#
# Usage:
#   ./scripts/verify.sh                # Run the default profile (all correctness
#                                       # gates; benches/eval are opt-in)
#   ./scripts/verify.sh --ci-smoke      # Fast minimal gate set: forbidden deps,
#                                       # closure linter, drift guards, snapshot
#                                       # proposal guard, advisories, vision
#                                       # coverage, unit/contract/golden tests,
#                                       # Basic E2E. Skips heavy mesh/Tailscale,
#                                       # overhaul integration, advanced E2E,
#                                       # boundary migration, doctor safety
#                                       # harness, benches, and eval. Intended
#                                       # for swarm CI smoke + agent pre-push
#                                       # readiness without paying the full
#                                       # mesh/RCH cost. Documented in
#                                       # docs/operator-swarm-slo.md (bd-2dgn0.5).
#   ./scripts/verify.sh --swarm-heavy   # 64-agent / Swarm-X full verification:
#                                       # default profile PLUS plan-doc-smoke,
#                                       # fuzz-smoke, eval regression, and
#                                       # benches. Intended for large-host
#                                       # opt-in scorecard runs that feed the
#                                       # bd-2dgn0 swarm SLO evidence trail.
#                                       # Documented in docs/operator-swarm-slo.md.
#   ./scripts/verify.sh --plan-doc-smoke # Run plan-sweep verify_cmd smoke checks
#   ./scripts/verify.sh --fuzz-target-audit-self-test # Run only the no-Cargo fuzz audit matcher self-test
#   ./scripts/verify.sh --fuzz-smoke   # Include 30s cargo-fuzz query parser smoke
#   ./scripts/verify.sh --include-bench # Include performance benchmarks
#   ./scripts/verify.sh --eval          # Include pack-quality eval regression sweep
#   ./scripts/verify.sh --help         # Show this help
#
# Exit status:
#   0   Complete. Every stage that was attempted passed. Stages deliberately
#       gated off (opt-in flags not set) do not affect this.
#   75  INCOMPLETE. One or more stages did not run because the beads lock was
#       held, so this run does not establish what those stages check. This is
#       EX_TEMPFAIL and matches BEADS_LOCK_SKIP_CODE: the code a contended
#       stage already returns is the code the whole run returns. Retry.
#   *   A stage failed; the code is that stage's own exit code.
#
#   A contended run is NOT a pass. Before bd-5krnm it exited 0 and was
#   indistinguishable from a clean run to anything reading $?.
#
# Gates (in order):
#   0. Plan Doc Smoke        - optional bd-3usjw.23 verify_cmd manifest checks
#   0.9. Forbidden Dependency Contract - no-Cargo metadata scanner self-test
#   1. Forbidden Dependencies  - cargo tree audit for banned crates
#   2. Closure Linter          - prevent abstention-as-implementation closure
#   3. Snapshot Proposal Guard - block unreviewed tracked insta proposals
#   4.48. Untracked Work Audit Contract - no-Cargo FILE SURFACE matcher self-test
#   4. Untracked Work Audit    - advisory Beads FILE SURFACE coverage for dirty paths
#   4.49. Bridge Staleness Contract - no-Cargo bridge fixture scanner self-test
#   4.5. Bridge Staleness      - advisory signal when CLOSE_THE_GAP_PLAN needs refresh
#   4.59. Plan Drift Contract  - no-Cargo plan/bead fixture scanner self-test
#   4.6. Plan Drift Advisory   - advisory plan_doc_section drift hints for Beads triage
#   4.64. Tracing Field Contract - checker self-test AND a baselined audit of
#         the tree; the self-test alone is what bd-c79fk was filed about
#   4.65. Contract Drift Radar - advisory schema/docs/taxonomy drift scanner (bd-31nul.5)
#   4.655. E2E Event Contract Radar Contract - no-Cargo golden report/schema harness
#   4.66. E2E Event Contract Radar - advisory shell evidence coverage scanner (bd-2ljka.4)
#   4.665. Work Packet No-Mutation - shell fixture matrix for claim-gate consumer safety
#   4.666. Agent Mail Snapshot Contract - no-Cargo redaction/coordination self-test
#   4.67. Panic Helper Radar Contract - no-Cargo schema/golden scanner contract gate
#   4.68. Swarm SLO Replay Contract - no-Cargo replay fixture/golden contract gate
#   4.69. CI Proof-Lane Snapshot Contract - no-Cargo proof-lane fixture gate
#   4.70. CI Proof-Lane Hygiene Contract - no-Cargo workflow policy self-test
#   4.705. CI Proof-Lane Hygiene Advisory - no-Cargo workflow policy scanner
#   4.706. Release Provenance Contract - no-Cargo release provenance marker gate
#   4.71. RCH Doc Examples Contract - no-Cargo command classifier self-test
#   4.715. RCH Doc Examples Lint - no-Cargo docs command-shape scanner
#   4.72. Local Cargo Tripwire Contract - no-Cargo guardrail self-test
#   4.73. RCH Portability Diagnostic Contract - no-Cargo Mac-leak self-test
#   4.74. Package Artifact Leak Contract - no-Cargo deny-pattern self-test
#   4.75. Package Artifact Leak - cargo package list gate for generated artifacts
#   4.8. Fuzz Target Audit Contract - no-Cargo cargo-fuzz matcher self-test
#   4.81. Fuzz Target Audit     - static cargo-fuzz target registration/docs check
#   4.9. Fuzz Smoke            - optional 30s search query parser cargo-fuzz sweep
#   5. Vision Coverage         - report documented implemented/stubbed/missing surfaces
#   5.5. Proof Verification    - advisory Lean4/TLA+ proof artifact checks
#   6. Unit/Contract/Golden    - cargo test --workspace --lib --bins --tests --examples
#   6. Basic E2E               - scripts/e2e_test.sh
#   6.05 Output Budget E2E     - scripts/e2e_output_budget.sh
#   6.055 Output Governor E2E  - scripts/e2e_output_governor.sh
#   6.056 Pack Delta E2E        - scripts/e2e_pack_delta.sh
#   6.06 Replay Lab Smoke E2E  - scripts/e2e_overhaul/swarm_replay_lab_smoke.sh
#   6.07 Why-Not E2E          - scripts/e2e_why_not.sh
#   6.075 Primer/AGENTS.md E2E - scripts/e2e_primer_agentsmd.sh
#   6.08 Cross-Cutting E2E     - scripts/e2e_cross_cutting.sh
#   6.09 Evidence Harvester E2E - scripts/e2e_evidence_harvester.sh
#   6.10 LOD Packing E2E       - scripts/e2e_lod_packing.sh
#   6.11 House Rules E2E       - scripts/e2e_house_rules.sh
#   6.12 Ask E2E               - scripts/e2e_ask.sh
#   6.126 Write Contention E2E - scripts/e2e_single_shot_write_contention.sh
#   6.1265 Capture Track E2E   - scripts/e2e_capture.sh
#   6.1266 Shadow Tuning E2E    - scripts/e2e_shadow_retrieval_tuning.sh
#   6.1267 Global Lane E2E      - scripts/e2e_global_lane.sh
#   6.1268 Beads Export Repair  - scripts/beads_export_repair.sh --self-test
#   6.127 Ergonomics E2E       - scripts/e2e_ergonomics.sh
#   6.1293 Consolidation E2E   - scripts/e2e_consolidation.sh
#   6.1295 Embedding Native E2E - scripts/e2e_embedding_native.sh
#   6.1296 Bundled Embeddings E2E - scripts/e2e_bundled_embeddings.sh
#   6.1297 Native Reranker E2E - scripts/e2e_native_reranker.sh
#   6.1. Agent Ergonomics E2E  - scripts/e2e_lib/run_agent_ergonomics_e2e.sh
#   6.5. Overhaul Integration  - scripts/e2e_overhaul.sh  (gated by VERIFY_OVERHAUL)
#   6.6. Fake Tailscale Harness - deterministic SRR6.46 fake tailnet self-test
#   6.7a-c Fake OIDC IdP Harness - protocol, defect, and matrix self-tests (T7.7)
#   7. Advanced E2E            - scripts/e2e_advanced.sh
#   8. Boundary Migration      - scripts/e2e_boundary_migration.sh
#   8.75. Eval Regression Contract - no-Cargo pack-quality threshold self-test
#   8.8. Eval Regression       - scripts/eval_regression.sh (optional)
#   9. Benchmarks (optional)   - scripts/bench_perf_regression.sh --check-regression
#
# Exit codes match AGENTS.md conventions (0=success, 1=usage, 3=storage, etc.)
# Artifacts are written to /tmp/ee-e2e-*/artifacts by E2E scripts.

INCLUDE_BENCH=false
INCLUDE_EVAL=false
INCLUDE_FUZZ_SMOKE=false
FUZZ_TARGET_AUDIT_SELF_TEST=false
PLAN_DOC_SMOKE=false
CI_SMOKE=false
SWARM_HEAVY=false
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
# shellcheck source=scripts/lib/ee_binary_resolution.sh
source "${REPO_ROOT}/scripts/lib/ee_binary_resolution.sh"
DEFAULT_AGENT_BUILD_ROOT="/Volumes/USBNVME16TB/temp_agent_space"
BEADS_LOCK_WAIT_SECONDS="${EE_BEADS_LOCK_WAIT_SECONDS:-30}"
BEADS_LOCK_SKIP_CODE=75
# scripts/closure-lint.sh exits this when its audit baseline lists debt that no
# longer exists. Kept distinct from 1 so the Verification Drift Guard cannot
# excuse it as a tracked violation; see closure_lint_or_tracked_drift.
CLOSURE_LINT_STALE_BASELINE_CODE=3
# scripts/closure-lint.sh exits this when its AUDIT matched zero beads: an
# abstention, not a pass. Kept distinct from 3 and from 75 because a gate that
# read nothing, a gate whose baseline is stale, and a gate blocked by lock
# contention are three different states that were previously one exit code.
CLOSURE_LINT_EMPTY_POPULATION_CODE=4
VERIFY_BUDGET_FILE="${EE_VERIFY_BUDGET_FILE:-${SCRIPT_DIR}/verify-budget.toml}"
VERIFY_BUDGET_FAIL_CODE=6

for arg in "$@"; do
    case "$arg" in
        --help|-h)
            sed -n '3,62p' "$0" | sed 's/^# //' | sed 's/^#//'
            exit 0
            ;;
        --plan-doc-smoke)
            PLAN_DOC_SMOKE=true
            ;;
        --fuzz-target-audit-self-test)
            FUZZ_TARGET_AUDIT_SELF_TEST=true
            ;;
        --fuzz-smoke)
            INCLUDE_FUZZ_SMOKE=true
            ;;
        --include-bench)
            INCLUDE_BENCH=true
            ;;
        --eval)
            INCLUDE_EVAL=true
            ;;
        --ci-smoke)
            CI_SMOKE=true
            ;;
        --swarm-heavy)
            SWARM_HEAVY=true
            INCLUDE_BENCH=true
            INCLUDE_EVAL=true
            INCLUDE_FUZZ_SMOKE=true
            PLAN_DOC_SMOKE=true
            ;;
        *)
            echo "Unknown argument: $arg" >&2
            exit 1
            ;;
    esac
done

if [ "$CI_SMOKE" = "true" ] && [ "$SWARM_HEAVY" = "true" ]; then
    echo "error: --ci-smoke and --swarm-heavy are mutually exclusive" >&2
    echo "       --ci-smoke trims to the fast minimal gate set;" >&2
    echo "       --swarm-heavy adds the heaviest opt-in gates." >&2
    echo "       See docs/operator-swarm-slo.md for guidance." >&2
    exit 1
fi

echo "=== EE Verification Runner ==="
if [ "$CI_SMOKE" = "true" ]; then
    echo "Profile: ci-smoke (fast minimal gate set; see docs/operator-swarm-slo.md)"
elif [ "$SWARM_HEAVY" = "true" ]; then
    echo "Profile: swarm-heavy (includes bench, eval, fuzz-smoke, plan-doc-smoke)"
else
    echo "Profile: default (correctness gates; benches and eval opt-in)"
fi
echo ""

if [ -d "${DEFAULT_AGENT_BUILD_ROOT}" ]; then
    mkdir -p "${DEFAULT_AGENT_BUILD_ROOT}/cargo-target" "${DEFAULT_AGENT_BUILD_ROOT}/tmp" 2>/dev/null || true
    export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-${DEFAULT_AGENT_BUILD_ROOT}/cargo-target}"
    export TMPDIR="${EE_AGENT_TMPDIR:-${DEFAULT_AGENT_BUILD_ROOT}/tmp}"
fi

ARTIFACT_DIRS=""
TRACE_LOG_DIRS=""
STAGE_RESULTS=""

# Record a stage's artifact directory in the end-of-run index, TAGGED WITH THE
# STATUS THAT PRODUCED IT (bd-ovsjv).
#
# This capture used to live inline in run_stage's PASS branch only. Every other
# terminal status -- FAIL, TIMEOUT, INFRA_ERROR, ADVISORY, TRACKED_RED, SKIP --
# reached `rm -f "$output_file"` without it, so the artifact index had a
# PASSES-ONLY POPULATION: it answered "where is the evidence for the things that
# worked", which is the question nobody asks. Nothing was destroyed (run_stage
# tees every stage's output to the log, and the directories are untouched on
# disk) but the structured list a reader, a triage script or a proof capsule
# scans omitted exactly the stages worth looking at.
#
# The status tag is the other half: an index that lists a directory without
# saying the stage FAILED invites it to be read as a passing artifact.
# Print the artifact index, if anything was recorded.
#
# THIS IS CALLED FROM TWO PLACES ON PURPOSE (bd-ovsjv). It used to be an inline
# block at the very end of the file, which meant a run that HARD-FAILED never
# reached it: run_stage exits with the stage's code the moment a required stage
# fails, hundreds of lines earlier. So the index was not merely
# passes-only-populated, it was not PRINTED AT ALL on exactly the runs where
# someone needs to find a failed stage's evidence.
#
# Recording without printing would have been the dead-code shape this repository
# keeps producing: a capture that runs, populates a variable, and is never read
# on the path that matters.
print_artifact_index() {
    [ -n "$ARTIFACT_DIRS" ] || return 0
    echo ""
    echo "Artifact directories:"
    printf "%b" "$ARTIFACT_DIRS"
    return 0
}

record_stage_artifacts() {
    local name="${1:?record_stage_artifacts: stage name required}"
    local status="${2:?record_stage_artifacts: status required}"
    local source_file="${3-}"
    local artifacts

    [ -n "$source_file" ] && [ -f "$source_file" ] || return 0

    artifacts=$(grep -o 'Artifacts:[[:space:]]*[^ ]*' "$source_file" \
        | head -1 | sed 's/Artifacts:[[:space:]]*//' || true)
    if [ -n "$artifacts" ] && [ -d "$artifacts" ]; then
        ARTIFACT_DIRS="${ARTIFACT_DIRS}  [${status}] ${name}: ${artifacts}\n"
    fi
    return 0
}
# Green has to carry its denominator. STAGE_RESULTS is a display string whose
# "\n" stay literal until printf "%b", so it is not something to parse -- the
# counts are recorded here as stages run instead.
#
# The two kinds of not-run are kept apart on purpose:
#   GATED_OFF  - deliberate (profile flag or --ci-smoke). Fine in a green run.
#   CONTENTION - a beads lock was held, so a stage that was SUPPOSED to run did
#                not. Non-fatal by design, but a run containing one has not
#                established what that stage checks, and must not read as clean.
# Exit code for a run that could not attempt every stage. Deliberately the
# same value as BEADS_LOCK_SKIP_CODE (EX_TEMPFAIL): contention is a retryable
# incompleteness, never a verification failure, so it must not collide with a
# real stage failure's exit code.
VERIFY_EXIT_INCOMPLETE="$BEADS_LOCK_SKIP_CODE"
# 70 = EX_SOFTWARE. A run that attempted ZERO stages is a broken invocation, not
# a transient condition, so it must not share 75 (EX_TEMPFAIL) with contention:
# a wrapper retries 75, and retrying a run that attempts nothing attempts
# nothing again. See verification_exit_status.
VERIFY_EXIT_NOTHING_ATTEMPTED=70

# --- stage status vocabulary (bd-reality-core-convergence-1azkt.5) -----------
#
# Every stage outcome must name WHICH of these it is. Before this existed the
# script emitted two tokens, PASS and SKIP, for five distinguishable outcomes:
# a passing stage, a stage skipped by lock contention, a stage deliberately
# gated off, and any failure at all -- where "any failure" collapsed a real
# assertion failure, a timeout and an OOM kill into one line reading
# "FAIL: <name> (Exit code: 124)".
#
# That collapse is not cosmetic. A 124 is GNU timeout firing, which says the
# stage did not finish and therefore established nothing; a 1 says the stage ran
# and the thing it checks is broken. Reporting both as FAIL means a reader
# cannot tell "this is red" from "this never ran", which is the same confusion
# the contention counter already exists to prevent.
#
# Measured evidence for the mapping, from this repo's own RCH lane on
# 2026-09-17: an integration_s_z run returned 124 with no `test result:` line
# anywhere in its log, having died mid-compile. Re-run with a longer wrapper and
# unchanged code, the identical command returned 0 with 16 passed. The exit code
# was the only thing that distinguished "no verdict" from "verdict".
#
# ADVISORY and TRACKED_RED WERE declared here without being routed -- no
# declaration surface on a stage -- and that gap was recorded rather than hidden
# behind a constant nothing emits. IT IS CLOSED: both are declared per stage in
# scripts/verify-budget.toml (`requirement = "advisory" | "tracked_red"`, with
# `tracked_red_bead` mandatory for the latter) and both are emitted into
# STAGE_RESULTS at :1117 and :1129.
#
# The note is corrected rather than deleted because a comment asserting a gap
# that has since been closed teaches a reader to distrust a vocabulary that now
# enforces itself -- and because the two states are exactly the ones a
# classifier test cannot reach, being policy-derived rather than
# exit-code-derived. Their aggregation is held by
# `advisory_and_tracked_red_are_counted_apart_from_passed` in
# tests/verification_drift_guard.rs (1azkt.5 bullet 8, clause 2).
STAGE_STATUS_VOCABULARY="PASS FAIL NOT_APPLICABLE SKIP ADVISORY TRACKED_RED INFRA_ERROR TIMEOUT CANCELLED"

# Classify a stage's exit code into the vocabulary above.
#
# Only PASS and SKIP are success-shaped; every other status is a non-success and
# keeps the script's existing fail-fast exit. This function ADDS information to
# a failure, it never converts one into a pass.
stage_status_for_exit_code() {
    case "$1" in
        0) printf '%s\n' "PASS" ;;
        "$BEADS_LOCK_SKIP_CODE") printf '%s\n' "SKIP" ;;
        # GNU timeout(1). The stage did not finish, so it established nothing.
        124) printf '%s\n' "TIMEOUT" ;;
        # 128+9 SIGKILL: the OOM killer and `kill -9` both land here. The stage
        # was destroyed from outside rather than deciding anything.
        137) printf '%s\n' "INFRA_ERROR" ;;
        # 128+2 SIGINT, 128+15 SIGTERM: operator or supervisor cancellation.
        130|143) printf '%s\n' "CANCELLED" ;;
        *) printf '%s\n' "FAIL" ;;
    esac
}

STAGE_PASSED=0
STAGE_SKIPPED_CONTENTION=0
STAGE_SKIPPED_CONTENTION_NAMES=""
STAGE_GATED_OFF=0
STAGE_GATED_OFF_NAMES=""
# Declared-non-required outcomes. Counted SEPARATELY from STAGE_PASSED on
# purpose: an advisory or tracked-red stage did not pass, and folding it into
# the passed count is precisely how an excuse becomes invisible. See the
# requirement-policy block in scripts/verify-budget.toml.
STAGE_ADVISORY=0
STAGE_ADVISORY_NAMES=""
STAGE_TRACKED_RED=0
STAGE_TRACKED_RED_NAMES=""
TOTAL_START=$(date +%s)

CURRENT_SOURCE_TARGET_DIR="$(ee_cargo_target_directory || true)"
if [ -n "${CURRENT_SOURCE_TARGET_DIR}" ]; then
    CURRENT_SOURCE_EE_BINARY="${CURRENT_SOURCE_TARGET_DIR%/}/debug/ee"
else
    CURRENT_SOURCE_EE_BINARY="${REPO_ROOT}/target/debug/ee"
fi
if [ -z "${EE_BINARY:-}" ]; then
    export EE_BINARY="${CURRENT_SOURCE_EE_BINARY}"
fi

# EE_BINARY IDENTITY, NOT MERELY ITS PATH (bd-reality-core-convergence-1azkt.5,
# acceptance bullet 5).
#
# Everything above this point is PATH SELECTION: it picks a string and falls
# back to ${REPO_ROOT}/target/debug/ee if cargo cannot be asked. Nothing checked
# that the file at that path exists, can execute on this host, or was built from
# this source. Stages from :1266 onward consume CURRENT_SOURCE_EE_BINARY and the
# only `cargo build --bin ee` in this file is at :1923, so the binary those
# earlier stages run is whatever happened to be on disk.
#
# This repo has already been bitten by precisely that: an RCH run can exit 0
# having written a LINUX ELF over target/debug/ee on a macOS checkout. Every
# assertion afterwards then fails for one reason that has nothing to do with the
# code under test, and a reader sees a wall of product failures.
#
# ee_require_current_binary already implements the check -- existence, executes
# on THIS host, and binary version == source version, printing provenance BEFORE
# it decides. It lived in scripts/lib/ee_binary_resolution.sh, sourced at :133,
# and this runner never called it. No new mechanism is added here; the existing
# one is invoked.
#
# WHAT THIS COVERS, KEPT CURRENT -- bullet 5 names five properties and this
# comment has twice described fewer than are actually checked:
#   existence / executes here / version   ee_require_current_binary, below.
#   FEATURE SET                           checked at :391 against Cargo.toml's
#                                         `default = [...]`, and the invariant
#                                         that build_features() reports every
#                                         default feature is held by
#                                         tests/verification_drift_guard.rs.
#   HASH                                  recorded below AND compared, by the
#                                         "Candidate Binary Identity Stable"
#                                         stage near the end of this file. The
#                                         comparison is a second OBSERVATION of
#                                         the same binary, because no declared
#                                         expected digest exists or can: the
#                                         binary is built per run.
#   TARGET TRIPLE                         checked below, against `rustc -vV`'s
#                                         host line, via
#                                         ee_assert_target_triple_matches_host.
#                                         "unknown" is kept distinct from a
#                                         mismatch: it is a missing fact, not a
#                                         conflict.
#
# All five properties bullet 5 names are now verified. This list is the sentence
# the next reader believes, so it enumerates rather than generalises -- and it
# has already been wrong twice by describing FEWER checks than exist. If you add
# or remove one, change this list in the same commit.
#
# The hash check has to sit after the last stage that can rebuild -- "Write
# Contention E2E" at :1923 runs `cargo build --locked --bin ee` -- or it compares
# the binary to itself and cannot fail. Recording the hash here is also what lets
# the proof capsule bind the binary that produced a verdict.
if ! ee_require_current_binary "${EE_BINARY}" "verify"; then
    printf 'verify: refusing to run stages against an unverified binary.\n' >&2
    printf 'verify: build it first (cargo build --locked --bin ee) or export\n' >&2
    printf 'verify: EE_BINARY to one built from this source tree.\n' >&2
    exit 1
fi
EE_BINARY_SHA256="$(
    { shasum -a 256 "${EE_BINARY}" 2>/dev/null || sha256sum "${EE_BINARY}" 2>/dev/null; } \
        | awk 'NR==1 {print $1}'
)"
export EE_BINARY_SHA256
printf 'verify: ee_binary_sha256=%s target_hint=%s\n' \
    "${EE_BINARY_SHA256:-unavailable}" \
    "$(file -b "${EE_BINARY}" 2>/dev/null | cut -c1-60 || printf 'unavailable')" >&2

# THE CANDIDATE BINARY'S FEATURE SET, ASSERTED RATHER THAN RECORDED
# (bd-reality-core-convergence-1azkt.5, acceptance bullet 5).
#
# Bullet 5 names five properties -- hash, version, source, target, features --
# and before this the check verified three. Version and source are compared by
# ee_require_current_binary above; target is established behaviourally, because
# a binary that could not run on this host would have failed
# ee_binary_executes_here. FEATURES were not checked at all, and the hash is
# still only RECORDED (see the limit below).
#
# WHY IT MATTERS HERE SPECIFICALLY: a stage that needs an optional feature and
# runs against a binary built without it does not error at the feature boundary
# -- it takes whatever fallback path the product provides and then asserts
# against the fallback. That is a green produced by a different code path than
# the one the stage names, which is the failure shape this whole bead exists to
# remove.
#
# THE EXPECTATION IS NOT INVENTED HERE. Cargo.toml's `default = [...]` is
# already the declaration of what a plain `cargo build --bin ee` produces, and
# that is what the stages below run. Introducing a second list in this file
# would create two sources of truth that could disagree silently, which is
# exactly the drift this bullet is about.
ee_expected_default_features() {
    python3 - "$REPO_ROOT/Cargo.toml" <<'PY'
import re, sys
text = open(sys.argv[1], encoding="utf-8").read()
m = re.search(r'^default\s*=\s*\[(.*?)\]', text, re.M | re.S)
if not m:
    raise SystemExit(1)
print(" ".join(sorted(re.findall(r'"([^"]+)"', m.group(1)))))
PY
}

ee_binary_enabled_features() {
    "${EE_BINARY}" version --json 2>/dev/null | python3 -c '
import json, sys
try:
    doc = json.load(sys.stdin)
except Exception:
    raise SystemExit(1)
node = doc.get("data", doc)
feats = node.get("features")
if not isinstance(feats, list):
    raise SystemExit(1)
print(" ".join(sorted(f["name"] for f in feats if f.get("enabled"))))
'
}

if ! EE_EXPECTED_FEATURES="$(ee_expected_default_features)"; then
    printf 'verify: refusing: could not read `default = [...]` from Cargo.toml.\n' >&2
    printf 'verify: the feature expectation has no source, so the check below\n' >&2
    printf 'verify: would compare against nothing and pass regardless.\n' >&2
    exit 1
fi
if ! EE_ACTUAL_FEATURES="$(ee_binary_enabled_features)"; then
    printf 'verify: refusing: %s could not report its build features.\n' "${EE_BINARY}" >&2
    printf 'verify: `ee version --json` must carry features[] (src/core/mod.rs:274).\n' >&2
    printf 'verify: an unreadable feature set is NOT a pass -- it is the same\n' >&2
    printf 'verify: unknown the recorded-but-uncompared hash already is.\n' >&2
    exit 1
fi
printf 'verify: ee_binary_features=%s\n' "${EE_ACTUAL_FEATURES}" >&2
if [ "${EE_ACTUAL_FEATURES}" != "${EE_EXPECTED_FEATURES}" ]; then
    printf 'verify: refusing: candidate binary feature set does not match Cargo.toml.\n' >&2
    printf 'verify:   expected (Cargo.toml default): %s\n' "${EE_EXPECTED_FEATURES}" >&2
    printf 'verify:   actual   (%s): %s\n' "${EE_BINARY}" "${EE_ACTUAL_FEATURES}" >&2
    printf 'verify: stages would run against a binary built differently from the\n' >&2
    printf 'verify: one this manifest describes.\n' >&2
    exit 1
fi
export EE_BINARY_FEATURES="${EE_ACTUAL_FEATURES}"

# TARGET TRIPLE (bd-reality-core-convergence-1azkt.5, bullet 5). The last of the
# five properties that bullet names. `ee_binary_executes_here` already refuses a
# binary whose FORMAT is foreign to this host, but "can execute here" and "was
# built for here" are different questions: a binary cross-built for a sibling
# triple can still load and run, and then every stage reports on a build nobody
# asked for.
#
# THE BINARY ALREADY CARRIES THE ANSWER and nothing read it:
# `ee version --json` -> data.build.targetTriple, from build.rs putting cargo's
# TARGET into EE_BUILD_TARGET (cargo sets TARGET for build scripts only, which
# is why a build script is the only way to obtain it). The reference value is
# `rustc -vV`'s host line, because that is by definition what a plain
# `cargo build` on this host produces.
#
# THREE OUTCOMES, KEPT APART. "unknown" is not a mismatch -- it means the binary
# was built without EE_BUILD_TARGET, which src/core/mod.rs:432 already models as
# the `target_triple_unavailable` degradation. Reporting it as a mismatch would
# send a reader hunting a cross-compile that never happened.
ee_host_target_triple() {
    rustc -vV 2>/dev/null | awk '/^host:/ {print $2}'
}

ee_binary_target_triple() {
    "${EE_BINARY}" version --json 2>/dev/null | python3 -c '
import json, sys
try:
    doc = json.load(sys.stdin)
except Exception:
    raise SystemExit(1)
node = doc.get("data", doc)
build = node.get("build")
if not isinstance(build, dict):
    raise SystemExit(1)
triple = build.get("targetTriple")
if not isinstance(triple, str) or not triple:
    raise SystemExit(1)
print(triple)
'
}

if ! EE_HOST_TRIPLE="$(ee_host_target_triple)" || [ -z "${EE_HOST_TRIPLE}" ]; then
    printf 'verify: refusing: could not read the host triple from `rustc -vV`.\n' >&2
    printf 'verify: the target expectation has no source, so the check below\n' >&2
    printf 'verify: would compare against nothing and pass regardless.\n' >&2
    exit 1
fi
if ! EE_ACTUAL_TRIPLE="$(ee_binary_target_triple)"; then
    printf 'verify: refusing: %s could not report its target triple.\n' "${EE_BINARY}" >&2
    printf 'verify: `ee version --json` must carry data.build.targetTriple.\n' >&2
    printf 'verify: an unreadable target is NOT a pass.\n' >&2
    exit 1
fi
printf 'verify: ee_binary_target=%s host_target=%s\n' \
    "${EE_ACTUAL_TRIPLE}" "${EE_HOST_TRIPLE}" >&2
# The comparison itself lives in the resolution lib so it has self-test arms;
# an inline chain here could only be exercised by running the whole script.
if ! ee_assert_target_triple_matches_host \
        "${EE_ACTUAL_TRIPLE}" "${EE_HOST_TRIPLE}" "${EE_BINARY}" "verify"; then
    printf 'verify: refusing to run stages against that binary.\n' >&2
    exit 1
fi
export EE_BINARY_TARGET_TRIPLE="${EE_ACTUAL_TRIPLE}"

# PLATFORM (bd-reality-core-convergence-1azkt.5, bullet 1). The manifest now
# declares WHERE it is valid, in `[requirements].supported_target_os`, and this
# reads it. Without this read the key would be decoration -- a declaration
# nothing consults, which is the defect the whole bullet is about.
#
# The manifest's list is the SOURCE of this fact, not a copy: nothing else in the
# tree states which platforms verify.sh is meant to run on. The binary's side
# comes from `ee version --json` -> data.build.targetOs, the same document the
# triple and feature checks read.
ee_manifest_supported_target_os() {
    awk '
        /^\[requirements\]/ { in_req = 1; next }
        in_req && /^\[/     { exit }
        in_req && /^supported_target_os[[:space:]]*=/ {
            line = $0
            sub(/^[^=]*=[[:space:]]*/, "", line)
            gsub(/[][",]/, " ", line)
            print line
            exit
        }
    ' "$VERIFY_BUDGET_FILE"
}

ee_binary_target_os() {
    "${EE_BINARY}" version --json 2>/dev/null | python3 -c '
import json, sys
try:
    doc = json.load(sys.stdin)
except Exception:
    raise SystemExit(1)
build = doc.get("data", doc).get("build")
if not isinstance(build, dict):
    raise SystemExit(1)
value = build.get("targetOs")
if not isinstance(value, str) or not value:
    raise SystemExit(1)
print(value)
'
}

if ! EE_SUPPORTED_OS="$(ee_manifest_supported_target_os)" || [ -z "${EE_SUPPORTED_OS// /}" ]; then
    printf 'verify: refusing: verify-budget.toml declares no supported_target_os.\n' >&2
    printf 'verify: [requirements].supported_target_os is where this manifest says\n' >&2
    printf 'verify: which platforms it is valid on. Absent, the check below would\n' >&2
    printf 'verify: accept every platform and pass regardless.\n' >&2
    exit 1
fi
if ! EE_ACTUAL_OS="$(ee_binary_target_os)"; then
    printf 'verify: refusing: %s could not report its target OS.\n' "${EE_BINARY}" >&2
    printf 'verify: `ee version --json` must carry data.build.targetOs.\n' >&2
    exit 1
fi
if ! ee_assert_target_os_supported "${EE_ACTUAL_OS}" "${EE_SUPPORTED_OS}" "verify"; then
    printf 'verify: refusing to run stages on an undeclared platform.\n' >&2
    exit 1
fi
export EE_BINARY_TARGET_OS="${EE_ACTUAL_OS}"

# E2E TEMP ROOT: DERIVED PER HOST, AND PROVEN WRITABLE BEFORE ANY STAGE RUNS
# (bd-13y74).
#
# This file used to hand the macOS path /private/tmp to THIRTY e2e stages as a
# hardcoded EE_E2E_TMPDIR literal, one per stage. That path is a macOS
# convention; on the Linux fleet where all RCH
# verification actually runs it exists but is root-owned and unwritable. Those
# stages therefore died at their first `mktemp -d` having executed ZERO
# assertions, and exited nonzero -- so a reader saw what looked like thirty
# product failures rather than a harness that never started.
#
# The scripts already offer EE_E2E_TMPDIR as the escape hatch from that macOS
# default, and this runner was using the hatch to RE-ASSERT the assumption. The
# constraint behind the default is real but it is a MACOS constraint (an ExFAT
# TMPDIR breaks DB opens on the dev host, bd-2vq2z), so it is expressed here as
# one instead of as a literal that is wrong on every other platform.
#
# WHY THIS REFUSES RATHER THAN MINTING A NEW EXIT CODE: a fresh code that the
# callers of this script do not handle gets excused by them, which is worse
# than the ambiguity it replaces (bd-success-shaped-signal-on-failure-l3pa4
# documents exactly that trap). The distinction is carried by the message,
# which names the harness as the thing that could not start, and by refusing
# HERE -- once, before any stage -- rather than thirty times inside stages
# where it is indistinguishable from a test failure.
# PROVE IT, do not assume it. Existence is not writability: /private/tmp EXISTS
# on the Linux workers -- it is merely root-owned -- which is exactly why this
# failed so late and read so much like a product defect. The probe performs the
# same operation the stages perform first, so a pass here means they can start.
e2e_tmpdir_writable() {
    local base="$1" probe
    [ -n "$base" ] || return 1
    [ -d "$base" ] || return 1
    probe="$(mktemp -d "${base}/ee-verify-tmpdir-probe.XXXXXX" 2>/dev/null)" || return 1
    rmdir "$probe" 2>/dev/null || true
    return 0
}

e2e_tmpdir_refuse() {
    printf 'verify: %s\n' "$1" >&2
    printf 'verify: this is the e2e HARNESS root, not a test failure --\n' >&2
    printf 'verify: every e2e stage would die at its first mktemp having\n' >&2
    printf 'verify: executed zero assertions. Refusing before that happens.\n' >&2
    exit 1
}

E2E_TMPDIR_UNAME="$(uname -s 2>/dev/null || printf 'unknown')"
if [ -n "${EE_E2E_TMPDIR:-}" ]; then
    # An EXPLICIT caller choice is never silently relocated: quietly running the
    # suite somewhere other than where the operator pointed it is its own kind
    # of dishonest gate. Refuse and let them repoint it.
    E2E_TMPDIR_BASE="${EE_E2E_TMPDIR%/}"
    E2E_TMPDIR_ORIGIN="caller"
    e2e_tmpdir_writable "${E2E_TMPDIR_BASE}" || e2e_tmpdir_refuse \
        "EE_E2E_TMPDIR=${E2E_TMPDIR_BASE} is not writable on this host."
else
    # ORDERED, DEDUPED CANDIDATES. The fallback must differ from what already
    # failed: an earlier draft of this block fell back to ${TMPDIR:-/tmp} after
    # the platform default failed, which on a host where TMPDIR IS that default
    # retried the identical path and reported it as two attempts. The repo-local
    # last resort is the one the bd-13y74 measurement itself used.
    # An ARRAY, not a space-joined string: a temp path containing a space would
    # otherwise word-split into two bogus candidates and could silently select
    # the wrong directory.
    case "${E2E_TMPDIR_UNAME}" in
        Darwin)
            # Keep the ExFAT-avoidance default this repo relies on locally
            # (bd-2vq2z), but as a macOS-only PREFERENCE, not a global literal.
            E2E_TMPDIR_CANDIDATES=("/private/tmp" "${TMPDIR:-}" "/tmp" "${REPO_ROOT}/target/e2e-tmp")
            ;;
        *)
            E2E_TMPDIR_CANDIDATES=("${TMPDIR:-}" "/tmp" "${REPO_ROOT}/target/e2e-tmp")
            ;;
    esac

    E2E_TMPDIR_BASE=""
    E2E_TMPDIR_ORIGIN=""
    E2E_TMPDIR_TRIED=""
    E2E_TMPDIR_SEEN=" "
    for e2e_candidate in "${E2E_TMPDIR_CANDIDATES[@]}"; do
        e2e_candidate="${e2e_candidate%/}"
        [ -n "${e2e_candidate}" ] || continue
        case "${E2E_TMPDIR_SEEN}" in *" ${e2e_candidate} "*) continue ;; esac
        E2E_TMPDIR_SEEN="${E2E_TMPDIR_SEEN}${e2e_candidate} "
        E2E_TMPDIR_TRIED="${E2E_TMPDIR_TRIED}${E2E_TMPDIR_TRIED:+, }${e2e_candidate}"
        # The repo-local last resort is the only candidate we may create, and it
        # gets a FRESH SUBDIRECTORY PER RUN (bd-xykxz).
        #
        # WHY, and the correlation is the point: unlike /tmp and TMPDIR this
        # path persists between runs, and it is reached ONLY when both of those
        # are unwritable -- which is exactly the wedged-host state measured on
        # bd-8iwkm, where `mkdir -p /tmp/...` was denied across three workers
        # and twenty-one dispatches. So a shared base would be reused precisely
        # on the machines whose brokenness makes this branch necessary.
        #
        # A run that is killed or times out seeds the directory; the next run
        # resolves to the same base and can measure something different without
        # anything saying the environment was not fresh. That does not corrupt a
        # run -- it makes two runs LOOK COMPARABLE WHILE THEY ARE NOT, which is
        # the harder failure to notice.
        #
        # Uniqueness rather than cleanup is deliberate: AGENTS.md forbids
        # removing files without explicit permission, so emptying a shared base
        # is not available. A fresh mktemp -d needs no deletion at all.
        case "${e2e_candidate}" in
            "${REPO_ROOT}/target/e2e-tmp")
                mkdir -p "${e2e_candidate}" 2>/dev/null || true
                if e2e_unique="$(mktemp -d "${e2e_candidate}/run.XXXXXX" 2>/dev/null)"; then
                    e2e_candidate="${e2e_unique}"
                fi
                ;;
        esac
        if e2e_tmpdir_writable "${e2e_candidate}"; then
            E2E_TMPDIR_BASE="${e2e_candidate}"
            E2E_TMPDIR_ORIGIN="auto"
            break
        fi
    done
    [ -n "${E2E_TMPDIR_BASE}" ] || e2e_tmpdir_refuse \
        "no writable e2e temp root on this host (tried: ${E2E_TMPDIR_TRIED})."
fi
# DELIBERATELY NOT EXPORTED. The thirty stages below set EE_E2E_TMPDIR inline,
# so exporting adds nothing for them -- but it WOULD newly expose the variable
# to stages that previously saw none, including Rust tests that read it and
# otherwise fall back through TMPDIR (e.g. tests/agent_profile_e2e.rs:23).
# Relocating their scratch space is a behaviour change this bead did not
# measure and does not need.
printf 'verify: e2e_tmpdir=%s origin=%s uname=%s tried=%s\n' \
    "${E2E_TMPDIR_BASE}" "${E2E_TMPDIR_ORIGIN}" "${E2E_TMPDIR_UNAME}" \
    "${E2E_TMPDIR_TRIED:-n/a}" >&2

# shellcheck disable=SC2329
beads_lock_wait_seconds() {
    case "$BEADS_LOCK_WAIT_SECONDS" in
        ''|*[!0-9]*)
            echo "error: EE_BEADS_LOCK_WAIT_SECONDS must be a non-negative integer" >&2
            exit 1
            ;;
        *)
            printf "%s" "$BEADS_LOCK_WAIT_SECONDS"
            ;;
    esac
}

# shellcheck disable=SC2329
with_beads_read_locks() {
    local beads_dir="${REPO_ROOT}/.beads"
    [ -d "$beads_dir" ] || {
        "$@"
        return $?
    }

    if ! command -v flock >/dev/null 2>&1; then
        echo "warning: flock not found; running Beads-reading gate without lock coordination" >&2
        "$@"
        return $?
    fi

    local wait_seconds
    wait_seconds=$(beads_lock_wait_seconds)

    local write_lock="${beads_dir}/.write.lock"
    local sync_lock="${beads_dir}/.sync.lock"

    if ! exec 8<>"$write_lock"; then
        echo "[!] SKIP: could not open Beads write lock $write_lock" >&2
        return "$BEADS_LOCK_SKIP_CODE"
    fi
    if ! flock -s -w "$wait_seconds" 8; then
        echo "[!] SKIP: Beads write lock is held: $write_lock" >&2
        return "$BEADS_LOCK_SKIP_CODE"
    fi

    if ! exec 9<>"$sync_lock"; then
        echo "[!] SKIP: could not open Beads sync lock $sync_lock" >&2
        flock -u 8 2>/dev/null || true
        exec 8>&- || true
        return "$BEADS_LOCK_SKIP_CODE"
    fi
    if ! flock -s -w "$wait_seconds" 9; then
        echo "[!] SKIP: Beads sync lock is held: $sync_lock" >&2
        flock -u 9 2>/dev/null || true
        exec 9>&- || true
        flock -u 8 2>/dev/null || true
        exec 8>&- || true
        return "$BEADS_LOCK_SKIP_CODE"
    fi

    set +e
    "$@"
    local status=$?
    set -e
    flock -u 9 2>/dev/null || true
    exec 9>&- || true
    flock -u 8 2>/dev/null || true
    exec 8>&- || true
    return "$status"
}

# shellcheck disable=SC2329
snapshot_proposal_guard() {
    if ! git -C "$REPO_ROOT" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
        echo "ok: not in a git worktree; snapshot proposal guard skipped"
        return 0
    fi

    local proposals
    proposals=$(git -C "$REPO_ROOT" ls-files | grep -E '\.snap\.new$' || true)
    if [ -z "$proposals" ]; then
        echo "ok: no tracked insta proposal snapshots"
        return 0
    fi

    local failures=0
    local count=0
    local proposal
    local accepted
    while IFS= read -r proposal; do
        [ -n "$proposal" ] || continue
        count=$((count + 1))
        accepted="${proposal%.new}"
        if ! git -C "$REPO_ROOT" ls-files --error-unmatch "$accepted" >/dev/null 2>&1; then
            echo "error: tracked insta proposal has no accepted snapshot: $proposal" >&2
            echo "       expected accepted snapshot: $accepted" >&2
            failures=1
            continue
        fi
        if ! cmp -s "$REPO_ROOT/$accepted" "$REPO_ROOT/$proposal"; then
            echo "error: tracked insta proposal differs from accepted snapshot: $proposal" >&2
            echo "       review with cargo insta and commit only accepted .snap files" >&2
            failures=1
        fi
    done <<< "$proposals"

    if [ "$failures" -ne 0 ]; then
        return 1
    fi
    echo "ok: $count tracked insta proposal snapshot(s) match accepted snapshots"
    echo "    removal of redundant .snap.new files still requires explicit approval"
}

# shellcheck disable=SC2329
fuzz_target_names() {
    printf '%s\n' \
        insights_section_dispatch \
        proximity_arg_parser \
        ppr_weight_clamp \
        insights_json_decode \
        search_query_parser
}

# shellcheck disable=SC2329
fuzz_manifest_has_registration() {
    local manifest="$1"
    local target="$2"
    local target_path="$3"

    awk -v name="name = \"${target}\"" -v path="path = \"${target_path}\"" '
        index($0, name) { has_name = 1 }
        index($0, path) { has_path = 1 }
        END { exit !(has_name && has_path) }
    ' "$manifest"
}

# shellcheck disable=SC2329
fuzz_target_file_has_shape() {
    local target_file="$1"

    awk '
        index($0, "#![no_main]") { has_no_main = 1 }
        index($0, "fuzz_target!") { has_fuzz_target = 1 }
        END { exit !(has_no_main && has_fuzz_target) }
    ' "$target_file"
}

# shellcheck disable=SC2329
fuzz_readme_has_sweep() {
    local readme="$1"
    local target="$2"
    local sweep_command="cargo fuzz run ${target} -- -max_total_time=300 -print_final_stats=1"

    grep -Fq "$sweep_command" "$readme"
}

# shellcheck disable=SC2329
fuzz_readme_has_global_proofs() {
    local readme="$1"

    awk '
        index($0, "Deliberate-panic proof") { has_deliberate_panic = 1 }
        index($0, "-max_total_time=900") { has_nightly_duration = 1 }
        END { exit !(has_deliberate_panic && has_nightly_duration) }
    ' "$readme"
}

# shellcheck disable=SC2329
fuzz_target_audit_self_test() {
    local manifest_good
    manifest_good='
[[bin]]
name = "insights_section_dispatch"
path = "fuzz_targets/insights_section_dispatch.rs"
[[bin]]
name = "proximity_arg_parser"
path = "fuzz_targets/proximity_arg_parser.rs"
[[bin]]
name = "ppr_weight_clamp"
path = "fuzz_targets/ppr_weight_clamp.rs"
[[bin]]
name = "insights_json_decode"
path = "fuzz_targets/insights_json_decode.rs"
[[bin]]
name = "search_query_parser"
path = "fuzz_targets/search_query_parser.rs"
'

    local readme_good
    readme_good='
cargo fuzz run insights_section_dispatch -- -max_total_time=300 -print_final_stats=1
cargo fuzz run proximity_arg_parser -- -max_total_time=300 -print_final_stats=1
cargo fuzz run ppr_weight_clamp -- -max_total_time=300 -print_final_stats=1
cargo fuzz run insights_json_decode -- -max_total_time=300 -print_final_stats=1
cargo fuzz run search_query_parser -- -max_total_time=300 -print_final_stats=1
Deliberate-panic proof
cargo fuzz run search_query_parser -- -max_total_time=900 -print_final_stats=1
'

    local source_good='#![no_main]
use libfuzzer_sys::fuzz_target;
fuzz_target!(|data: &[u8]| {
    let _ = data;
});
'
    local source_bad='#![no_main]
pub fn placeholder() {}
'

    local target
    while IFS= read -r target; do
        [ -n "$target" ] || continue
        local target_path="fuzz_targets/${target}.rs"
        if ! fuzz_manifest_has_registration <(printf '%s\n' "$manifest_good") "$target" "$target_path"; then
            echo "error: fuzz target self-test expected manifest registration for ${target}" >&2
            return 1
        fi
        if ! fuzz_readme_has_sweep <(printf '%s\n' "$readme_good") "$target"; then
            echo "error: fuzz target self-test expected README sweep for ${target}" >&2
            return 1
        fi
        if ! fuzz_target_file_has_shape <(printf '%s\n' "$source_good"); then
            echo "error: fuzz target self-test expected valid target source shape" >&2
            return 1
        fi
    done < <(fuzz_target_names)

    if fuzz_manifest_has_registration <(printf '%s\n' "$manifest_good") search_query_parser "fuzz_targets/wrong.rs"; then
        echo "error: fuzz target self-test should reject mismatched manifest path" >&2
        return 1
    fi
    if fuzz_readme_has_sweep <(printf '%s\n' "cargo fuzz run search_query_parser") search_query_parser; then
        echo "error: fuzz target self-test should reject incomplete sweep command" >&2
        return 1
    fi
    if fuzz_target_file_has_shape <(printf '%s\n' "$source_bad"); then
        echo "error: fuzz target self-test should reject missing fuzz_target entrypoint" >&2
        return 1
    fi
    if ! fuzz_readme_has_global_proofs <(printf '%s\n' "$readme_good"); then
        echo "error: fuzz target self-test expected global README proof markers" >&2
        return 1
    fi

    echo "ok: fuzz target audit self-test passed"
}

# shellcheck disable=SC2329
fuzz_target_audit() {
    local manifest="${REPO_ROOT}/fuzz/Cargo.toml"
    local readme="${REPO_ROOT}/fuzz/README.md"
    local failures=0
    local target

    if [ ! -f "$manifest" ]; then
        echo "error: missing fuzz manifest: fuzz/Cargo.toml" >&2
        return 1
    fi
    if [ ! -f "$readme" ]; then
        echo "error: missing fuzz README: fuzz/README.md" >&2
        return 1
    fi

    while IFS= read -r target; do
        [ -n "$target" ] || continue
        local target_path="fuzz_targets/${target}.rs"
        local target_file="${REPO_ROOT}/fuzz/${target_path}"
        if ! fuzz_manifest_has_registration "$manifest" "$target" "$target_path"; then
            echo "error: fuzz/Cargo.toml missing bin/path registration for ${target}" >&2
            failures=1
        fi
        if [ ! -f "$target_file" ]; then
            echo "error: missing fuzz target file: fuzz/${target_path}" >&2
            failures=1
        elif ! fuzz_target_file_has_shape "$target_file"; then
            echo "error: fuzz/${target_path} is missing #![no_main] or fuzz_target! entrypoint" >&2
            failures=1
        fi
        if ! fuzz_readme_has_sweep "$readme" "$target"; then
            echo "error: fuzz/README.md missing 5-minute logged cargo-fuzz sweep command for ${target}" >&2
            failures=1
        fi
    done < <(fuzz_target_names)

    if ! fuzz_readme_has_global_proofs "$readme"; then
        echo "error: fuzz/README.md missing deliberate-panic proof instructions or 15-minute nightly cargo-fuzz sweep duration" >&2
        failures=1
    fi

    if [ "$failures" -ne 0 ]; then
        return 1
    fi

    echo "ok: bd-bife.10 fuzz targets are registered, present, documented with 5-minute logged sweeps plus nightly duration, and shaped as cargo-fuzz harnesses"
}

# shellcheck disable=SC2329
fuzz_smoke() {
    if ! command -v cargo >/dev/null 2>&1; then
        echo "error: cargo is required for fuzz smoke" >&2
        return 1
    fi
    if ! cargo fuzz --help >/dev/null 2>&1; then
        echo "error: cargo-fuzz is required for fuzz smoke" >&2
        return 1
    fi

    (
        cd "$REPO_ROOT"
        cargo +nightly fuzz run search_query_parser -- -max_total_time=30 -print_final_stats=1
    )
}

test_trace_root() {
    if [ -n "${CARGO_TARGET_DIR:-}" ]; then
        printf "%s/ee-test-tracing" "${CARGO_TARGET_DIR%/}"
    else
        printf "%s/target/ee-test-tracing" "$REPO_ROOT"
    fi
}

capture_test_trace_artifacts() {
    local name="$1"
    local trace_root
    trace_root="$(test_trace_root)"

    if [ -d "$trace_root" ] &&
        find "$trace_root" -type f -name '*.jsonl' -print -quit 2>/dev/null | grep -q .; then
        TRACE_LOG_DIRS="${TRACE_LOG_DIRS}  ${name}: ${trace_root}\n"
    fi
}

stage_budget_value() {
    local stage_name="$1"
    local field="$2"

    [ -f "$VERIFY_BUDGET_FILE" ] || return 1

    awk -v target="$stage_name" -v field="$field" '
        /^\[\[stage\]\]/ {
            in_stage = 1
            matched = 0
            next
        }
        in_stage && /^name[[:space:]]*=/ {
            value = $0
            sub(/^[^=]*=[[:space:]]*/, "", value)
            gsub(/^"|"$/, "", value)
            matched = (value == target)
            next
        }
        in_stage && matched && $0 ~ ("^" field "[[:space:]]*=") {
            value = $0
            sub(/^[^=]*=[[:space:]]*/, "", value)
            gsub(/#.*/, "", value)
            gsub(/[[:space:]]+$/, "", value)
            gsub(/^"|"$/, "", value)
            print value
            found = 1
            exit
        }
        END {
            if (!found) {
                exit 1
            }
        }
    ' "$VERIFY_BUDGET_FILE"
}

# A stage's declared requirement policy: "required" (the default), "advisory",
# or "tracked_red".
#
# Absence means REQUIRED. A stage cannot become non-required by omission, only
# by a declaration someone wrote and a reviewer saw.
stage_requirement() {
    local stage_name="$1"
    local declared

    declared="$(stage_budget_value "$stage_name" requirement)" || {
        printf '%s\n' "required"
        return 0
    }
    case "$declared" in
        advisory | tracked_red) printf '%s\n' "$declared" ;;
        *) printf '%s\n' "required" ;;
    esac
}

stage_budget_thresholds() {
    local stage_name="$1"
    local p50
    local regression_factor

    p50="$(stage_budget_value "$stage_name" expected_seconds_p50)" || return 1
    regression_factor="$(stage_budget_value "$stage_name" regression_factor)" || return 1

    awk -v p50="$p50" -v regression_factor="$regression_factor" '
        BEGIN {
            advisory = int((p50 * regression_factor) + 0.999999)
            fail = int((p50 * 3) + 0.999999)
            printf "%d %d %d", p50, advisory, fail
        }
    '
}

stage_budget_summary() {
    local stage_name="$1"
    local duration="$2"
    local thresholds

    thresholds="$(stage_budget_thresholds "$stage_name")" || {
        printf "budget=untracked"
        return 0
    }

    local p50
    local advisory
    local fail
    read -r p50 advisory fail <<< "$thresholds"

    if [ "$duration" -gt "$fail" ]; then
        printf "budget=fail elapsed=%ss p50=%ss advisory=%ss fail=%ss" "$duration" "$p50" "$advisory" "$fail"
    elif [ "$duration" -gt "$advisory" ]; then
        printf "budget=advisory elapsed=%ss p50=%ss advisory=%ss fail=%ss" "$duration" "$p50" "$advisory" "$fail"
    else
        printf "budget=ok elapsed=%ss p50=%ss advisory=%ss fail=%ss" "$duration" "$p50" "$advisory" "$fail"
    fi
}

enforce_stage_budget() {
    local stage_name="$1"
    local duration="$2"
    local thresholds

    thresholds="$(stage_budget_thresholds "$stage_name")" || return 0

    local p50
    local advisory
    local fail
    read -r p50 advisory fail <<< "$thresholds"

    if [ "$duration" -gt "$fail" ]; then
        echo "error: verification stage exceeded hard budget: $stage_name" >&2
        echo "       elapsed=${duration}s p50=${p50}s hard_fail=${fail}s" >&2
        echo "       update scripts/verify-budget.toml only after validating the regression is expected" >&2
        return "$VERIFY_BUDGET_FAIL_CODE"
    fi

    if [ "$duration" -gt "$advisory" ]; then
        echo "[!] BUDGET: $stage_name exceeded advisory budget (${duration}s > ${advisory}s; p50=${p50}s)" >&2
    fi
}

# Record a stage that was deliberately not attempted. Emits the same
# STAGE_RESULTS line the call sites used to write by hand, and additionally
# counts and names it so the banner can state its own scope.
record_gated_off() {
    local name="$1"
    local reason="$2"
    # NOT_APPLICABLE, not SKIP. These are two different facts that this ledger
    # used to spell identically: a gated-off stage was DECLARED not to apply to
    # this run (a profile flag, --ci-smoke, an absent optional toolchain), while
    # a SKIP means a stage that was supposed to run did not. Only the first may
    # sit inside a green run, and a reader could not tell them apart while both
    # printed "SKIP <name>".
    STAGE_RESULTS="${STAGE_RESULTS}NOT_APPLICABLE ${name} (${reason})\n"
    STAGE_GATED_OFF=$((STAGE_GATED_OFF + 1))
    STAGE_GATED_OFF_NAMES="${STAGE_GATED_OFF_NAMES}    - ${name} (${reason})\n"
}

# The closing verdict, stating what it covers.
#
# The line this replaces was `echo "=== All verification stages passed ==="`,
# printed unconditionally. It was true only in the sense that run_stage exits
# the script on a real failure, so reaching it meant nothing FAILED -- it said
# nothing about how much was attempted, and it read as a full sweep whether 112
# stages ran or 30.
# Exit status for a completed verification run. Echoes rather than returns so
# callers can use it under `set -e` without the status tripping the shell.
verification_exit_status() {
    # SUCCESS REQUIRES COMPLETENESS, NOT MERELY THE ABSENCE OF A FAILURE
    # (bd-reality-core-convergence-1azkt.5, acceptance bullet 4).
    #
    # This function used to ask exactly one question -- were any stages skipped
    # for lock contention -- and answered 0 otherwise. So a run in which NOTHING
    # EXECUTED exited 0:
    #
    #     passed=0 contended=0 gated_off=0 -> exit=0
    #     passed=0 contended=0 gated_off=9 -> exit=0
    #
    # Both measured against this function before the change. A green there is
    # the absence of a FAIL, not the presence of a completed check, which is the
    # same vacuous-pass shape this repo has been removing from its gates all
    # week: an assertion whose failing case is unreachable, one layer up.
    #
    # A DISTINCT CODE, deliberately. VERIFY_EXIT_INCOMPLETE is 75 = EX_TEMPFAIL,
    # and wrappers RETRY 75 because contention is transient. A run that attempted
    # nothing is not transient -- retrying it attempts nothing again -- so
    # reusing 75 would spin. 70 = EX_SOFTWARE says the invocation itself was
    # wrong. One value cannot express two states.
    local attempted=$((STAGE_PASSED + STAGE_SKIPPED_CONTENTION + STAGE_ADVISORY + STAGE_TRACKED_RED))
    if [ "$attempted" -eq 0 ]; then
        printf '%s\n' "$VERIFY_EXIT_NOTHING_ATTEMPTED"
        return
    fi
    if [ "$STAGE_SKIPPED_CONTENTION" -gt 0 ]; then
        printf '%s\n' "$VERIFY_EXIT_INCOMPLETE"
        return
    fi
    printf '%s\n' "0"
}

verification_summary_banner() {
    local attempted=$((STAGE_PASSED + STAGE_SKIPPED_CONTENTION + STAGE_ADVISORY + STAGE_TRACKED_RED))
    local declared=$((attempted + STAGE_GATED_OFF))
    # A census, never a boolean. The excused population has to appear in the
    # same line that claims success, or an excuse nobody counts is an excuse
    # nobody audits (ruled 2026-09-17).
    # The census EXTENDS the attempted denominator rather than replacing it.
    # "N/M attempted verification stages passed" is its own contract, asserted
    # by a_complete_run_exits_zero, and dropping it to make room for the census
    # would trade one honest number for another instead of reporting both.
    local census="${STAGE_ADVISORY} advisory, ${STAGE_TRACKED_RED} tracked-red, ${STAGE_SKIPPED_CONTENTION} did-not-run, ${STAGE_GATED_OFF} not-applicable"

    if [ "$attempted" -eq 0 ]; then
        # THE TEXT HALF OF bullet 4. Without this branch the banner printed
        # "=== 0/0 attempted verification stages passed; 0 advisory, 0
        # tracked-red, 0 did-not-run, 0 not-applicable ===", which reads as a
        # clean run to every human and every log scraper. Fixing only the exit
        # code would leave the two halves of one contract disagreeing, and the
        # half people actually read would still say success.
        echo "=== NOTHING ATTEMPTED: 0 of ${declared} declared stages ran. This run establishes NOTHING. ==="
        echo ""
        echo "    Not a pass and not a contended skip: no stage was attempted at all."
        if [ "$STAGE_GATED_OFF" -gt 0 ]; then
            echo "    Every declared stage was gated off:"
            printf "%b" "$STAGE_GATED_OFF_NAMES"
        fi
    elif [ "$STAGE_SKIPPED_CONTENTION" -gt 0 ]; then
        echo "=== INCOMPLETE: ${STAGE_PASSED}/${attempted} attempted stages passed; ${STAGE_SKIPPED_CONTENTION} did NOT run (lock contention); ${census} ==="
        echo ""
        echo "    This run does not establish what these stages check:"
        printf "%b" "$STAGE_SKIPPED_CONTENTION_NAMES"
    elif [ "$STAGE_ADVISORY" -gt 0 ] || [ "$STAGE_TRACKED_RED" -gt 0 ]; then
        # Stages that did not pass were excused by declaration, and the headline
        # names them rather than letting a reader infer a clean run from an exit
        # code.
        echo "=== ${STAGE_PASSED}/${attempted} attempted verification stages passed; ${census} ==="
        echo ""
        echo "    Excused by declaration -- these did NOT pass:"
        printf "%b" "$STAGE_ADVISORY_NAMES"
        printf "%b" "$STAGE_TRACKED_RED_NAMES"
    else
        echo "=== ${STAGE_PASSED}/${attempted} attempted verification stages passed; ${census} ==="
    fi

    echo ""
    echo "Stage accounting:"
    echo "  declared                  : ${declared}"
    echo "  passed                    : ${STAGE_PASSED}"
    echo "  advisory (did NOT pass)   : ${STAGE_ADVISORY}"
    echo "  tracked red (did NOT pass): ${STAGE_TRACKED_RED}"
    echo "  did not run (contention)  : ${STAGE_SKIPPED_CONTENTION}"
    echo "  gated off (not attempted) : ${STAGE_GATED_OFF}"
    if [ "$STAGE_GATED_OFF" -gt 0 ]; then
        printf "%b" "$STAGE_GATED_OFF_NAMES"
    fi
    echo "  exit status               : $(verification_exit_status)"
}

run_stage() {
    local name="$1"
    local cmd="$2"
    echo "[*] Running: $name"
    echo "    $cmd"

    local start_time
    start_time=$(date +%s)
    local output_file
    output_file=$(mktemp)

    # `set +e` INSIDE THE GROUP, AND IT IS NOT OPTIONAL (bd-zu099).
    #
    # This line used to be `if eval "$cmd" 2>&1 | tee "$output_file"`. The left
    # side of a pipe runs in a subshell, this file sets `-e` at :2, and when a
    # stage failed errexit fired IN THAT SUBSHELL and exited it with 1 rather
    # than the command's status. So every real stage's exit code arrived here as
    # 1, and stage_status_for_exit_code -- which classifies by code -- could
    # only ever answer FAIL.
    #
    # Four branches were unreachable in production, measured one real
    # --plan-doc-smoke run each, all four now firing:
    #     124 TIMEOUT      137 INFRA_ERROR      130/143 CANCELLED      75 SKIP
    # The 75 one cascaded: STAGE_SKIPPED_CONTENTION could never increment, so
    # verification_exit_status never returned VERIFY_EXIT_INCOMPLETE and the
    # banner's "INCOMPLETE ... did NOT run (lock contention)" branch was dead
    # with it. A contended skip -- designed to be retryable -- was reported as a
    # hard failure.
    #
    # WHY IT LOOKED FINE: a stage string containing a literal `exit 137` sets the
    # status explicitly and SURVIVES, so the classifier demonstrably worked when
    # probed that way. Every real stage is a script or a function, which does
    # not. That inline-vs-script pair is the control for any future change here.
    #
    # The group is not a subshell; the PIPE provides the subshell, so `set +e`
    # is scoped to it. Verified: errexit reads ON in the parent immediately after
    # a stage runs.
    if { set +e; eval "$cmd"; } 2>&1 | tee "$output_file"; then
        local end_time
        end_time=$(date +%s)
        local duration=$((end_time - start_time))
        local budget_summary
        budget_summary="$(stage_budget_summary "$name" "$duration")"
        echo "[+] PASS: $name (${duration}s; ${budget_summary})"
        STAGE_RESULTS="${STAGE_RESULTS}PASS ${name} (${duration}s; ${budget_summary})\n"
        STAGE_PASSED=$((STAGE_PASSED + 1))
        capture_test_trace_artifacts "$name"

        record_stage_artifacts "$name" "PASS" "$output_file"
        rm -f "$output_file"
        enforce_stage_budget "$name" "$duration"
        echo ""
    else
        local exit_code=$?
        local end_time
        end_time=$(date +%s)
        local duration=$((end_time - start_time))
        if [ "$exit_code" -eq "$BEADS_LOCK_SKIP_CODE" ]; then
            local budget_summary
            budget_summary="$(stage_budget_summary "$name" "$duration")"
            echo "[!] SKIP: $name (${duration}s; ${budget_summary})"
            STAGE_RESULTS="${STAGE_RESULTS}SKIP ${name} (${duration}s; ${budget_summary})\n"
            # Non-fatal on purpose: routine lock contention must not break a
            # five-agent swarm. But it is NOT a pass, so it is counted
            # separately and named in the banner.
            STAGE_SKIPPED_CONTENTION=$((STAGE_SKIPPED_CONTENTION + 1))
            STAGE_SKIPPED_CONTENTION_NAMES="${STAGE_SKIPPED_CONTENTION_NAMES}    - ${name} (beads lock held)\n"
            record_stage_artifacts "$name" "SKIP" "$output_file"
            rm -f "$output_file"
            enforce_stage_budget "$name" "$duration"
            echo ""
            return 0
        fi
        # Name WHICH kind of non-success this is. The exit is unchanged -- every
        # status reached here is still fatal and still propagates the original
        # code -- but "TIMEOUT" and "INFRA_ERROR" tell a reader the stage
        # established nothing, where a bare "FAIL" implies it ran and decided.
        local stage_status
        stage_status="$(stage_status_for_exit_code "$exit_code")"

        # A stage the manifest declares non-required reports its OWN terminal
        # status and does not stop the run. It is never recorded as PASS and
        # never counted as one -- ADVISORY and TRACKED_RED are outcomes in their
        # own right, not a softer spelling of green (ruled 2026-09-17).
        #
        # The run continues, which is the entire purpose of the declaration, but
        # the summary census below prints these counts beside the passed count
        # so "success" can never be read without the excused population next to
        # it. That census is what separates a classification from an escape
        # hatch.
        local requirement
        requirement="$(stage_requirement "$name")"
        case "$requirement" in
            advisory)
                echo "[~] ADVISORY: $name (Exit code: $exit_code, ${duration}s; declared advisory)"
                STAGE_RESULTS="${STAGE_RESULTS}ADVISORY ${name} (exit ${exit_code}, ${duration}s)\n"
                STAGE_ADVISORY=$((STAGE_ADVISORY + 1))
                STAGE_ADVISORY_NAMES="${STAGE_ADVISORY_NAMES}    - ${name} (exit ${exit_code})\n"
                record_stage_artifacts "$name" "ADVISORY" "$output_file"
                rm -f "$output_file"
                enforce_stage_budget "$name" "$duration"
                echo ""
                return 0
                ;;
            tracked_red)
                local tracked_bead
                tracked_bead="$(stage_budget_value "$name" tracked_red_bead || printf '%s' 'UNDECLARED')"
                echo "[~] TRACKED_RED: $name (Exit code: $exit_code, ${duration}s; owned by ${tracked_bead})"
                STAGE_RESULTS="${STAGE_RESULTS}TRACKED_RED ${name} (exit ${exit_code}, ${tracked_bead})\n"
                STAGE_TRACKED_RED=$((STAGE_TRACKED_RED + 1))
                STAGE_TRACKED_RED_NAMES="${STAGE_TRACKED_RED_NAMES}    - ${name} (${tracked_bead})\n"
                record_stage_artifacts "$name" "TRACKED_RED" "$output_file"
                rm -f "$output_file"
                enforce_stage_budget "$name" "$duration"
                echo ""
                return 0
                ;;
        esac

        echo "[-] ${stage_status}: $name (Exit code: $exit_code, ${duration}s)"
        # Record it before exiting. Previously a failure left NO trace in
        # STAGE_RESULTS at all, because the script exits here, so the ledger
        # silently described only the stages that had already succeeded.
        STAGE_RESULTS="${STAGE_RESULTS}${stage_status} ${name} (exit ${exit_code}, ${duration}s)\n"
        record_stage_artifacts "$name" "$stage_status" "$output_file"
        rm -f "$output_file"
        # The run stops here, so the end-of-file index is unreachable. Print it
        # now or the failing stage's artifacts are recorded and never shown.
        print_artifact_index
        exit $exit_code
    fi
}

# shellcheck disable=SC2329
plan_doc_smoke() {
    if ! command -v python3 >/dev/null 2>&1; then
        echo "error: python3 is required for --plan-doc-smoke" >&2
        return 1
    fi

    python3 - "$REPO_ROOT" <<'PY'
import json
import os
import subprocess
import sys
import time
from pathlib import Path

root = Path(sys.argv[1])
report = root / "docs" / "plan-sweep-report.md"
request_id = os.environ.get("EE_REQUEST_ID", "plan-doc-smoke")


def trace(phase, section_id="", elapsed_ms=0, degraded_codes=None):
    print(
        json.dumps(
            {
                "workspace_id": str(root),
                "request_id": request_id,
                "bead_id": "bd-3usjw.23",
                "surface": "plan_doc_verify_cmds",
                "phase": phase,
                "section_id": section_id,
                "elapsed_ms": elapsed_ms,
                "degraded_codes": degraded_codes or [],
            },
            separators=(",", ":"),
        ),
        file=sys.stderr,
    )


if not report.exists():
    trace("input", degraded_codes=["plan_sweep_report_missing"])
    print(f"error: missing plan sweep report: {report}", file=sys.stderr)
    sys.exit(1)

trace("input")
commands = []
in_matrix = False
for line in report.read_text(encoding="utf-8").splitlines():
    stripped = line.strip()
    if stripped == "## Machine-Checked Section Matrix":
        in_matrix = True
        continue
    if in_matrix and stripped.startswith("## "):
        break
    if not in_matrix or not stripped.startswith("|"):
        continue
    if "section_id" in stripped or "------------" in stripped:
        continue

    cells = [cell.strip() for cell in stripped.strip("|").split("|")]
    if len(cells) != 6:
        trace("dependency_check", degraded_codes=["plan_sweep_row_malformed"])
        print(f"error: plan matrix row must have 6 cells: {line}", file=sys.stderr)
        sys.exit(1)

    section_id, _title, _classification, _evidence, _test_bead, verify_cmd = cells
    if verify_cmd and verify_cmd != "-":
        commands.append((section_id, verify_cmd))

if not commands:
    trace("dependency_check", degraded_codes=["plan_sweep_verify_cmds_missing"])
    print("error: no plan sweep verify_cmd entries found", file=sys.stderr)
    sys.exit(1)

failures = 0
for section_id, command in commands:
    start = time.monotonic()
    print(f"[*] {section_id}: {command}")
    trace("dependency_check", section_id=section_id)
    try:
        result = subprocess.run(
            ["bash", "-lc", f"set -euo pipefail; {command}"],
            cwd=root,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=60,
            check=False,
        )
    except subprocess.TimeoutExpired as error:
        elapsed_ms = int((time.monotonic() - start) * 1000)
        trace(
            "response",
            section_id=section_id,
            elapsed_ms=elapsed_ms,
            degraded_codes=["verify_cmd_timeout"],
        )
        print(f"error: {section_id} verify_cmd exceeded 60s: {command}", file=sys.stderr)
        if error.stdout:
            print(error.stdout, end="")
        if error.stderr:
            print(error.stderr, end="", file=sys.stderr)
        failures += 1
        continue

    elapsed_ms = int((time.monotonic() - start) * 1000)
    if result.stdout:
        print(result.stdout, end="")
    if result.stderr:
        print(result.stderr, end="", file=sys.stderr)

    if result.returncode == 0:
        trace("response", section_id=section_id, elapsed_ms=elapsed_ms)
    else:
        trace(
            "response",
            section_id=section_id,
            elapsed_ms=elapsed_ms,
            degraded_codes=["verify_cmd_failed"],
        )
        print(
            f"error: {section_id} verify_cmd exited {result.returncode}: {command}",
            file=sys.stderr,
        )
        failures += 1

if failures:
    sys.exit(1)

trace("response")
print(f"[+] plan-doc-smoke passed {len(commands)} verify commands")
PY
}

# shellcheck disable=SC2329
closure_lint_or_tracked_drift() {
    # The closure-lint audit covers both bead closure discipline and the
    # failure-mode fixture taxonomy (including *_unimplemented honesty-only
    # markers), so verify routes that full gate through drift tracking.
    #
    # Capture the status with `|| closure_exit=$?` rather than reading `$?`
    # after an `if`. A false `if` with no `else` exits 0, so the old
    # `local closure_exit=$?` placed after the `fi` read the COMPOUND's
    # status and never the linter's. closure_exit was therefore always 0 and
    # `return "$closure_exit"` could never be non-zero: this gate's only
    # failure path was dead, and a real violation was recorded as PASS
    # (bd-closure-lint-gate-cannot-fail-6hb5b).
    local closure_exit=0
    with_beads_read_locks ./scripts/closure-lint.sh --audit --json || closure_exit=$?
    if [ "$closure_exit" -eq 0 ]; then
        return 0
    fi

    # Contention is not a linter verdict. Hand the skip code straight back so
    # run_stage records it as a skipped stage and counts it toward the
    # INCOMPLETE banner and exit 75. Previously a contended closure-lint fell
    # through to the drift guard, and a passing guard converted a gate that
    # NEVER EXECUTED into a PASS that no counter ever saw.
    if [ "$closure_exit" -eq "$BEADS_LOCK_SKIP_CODE" ]; then
        return "$closure_exit"
    fi

    # A STALE AUDIT BASELINE is not a violation and must not be excusable.
    # closure-lint.sh exits CLOSURE_LINT_STALE_BASELINE_CODE when its baseline
    # lists debt that no longer exists. The drift guard's job is to excuse
    # TRACKED violations, and it decides by reading `.count` from the report --
    # which is ZERO in this case, because every live violation IS baselined. So
    # routing this through the guard would excuse it every time and make the
    # linter's stale arm inert. The fix for a stale entry is to delete the line,
    # not to open a bead, so there is nothing for the guard to find.
    if [ "$closure_exit" -eq "$CLOSURE_LINT_STALE_BASELINE_CODE" ]; then
        echo "[-] Closure linter: audit baseline lists debt that no longer exists; delete those lines" >&2
        return "$closure_exit"
    fi

    # AN EMPTY AUDIT POPULATION IS NOT EXCUSABLE EITHER, and for the same
    # reason as the stale baseline directly above: the drift guard decides by
    # reading `.count` from the report, which is ZERO when the audit matched no
    # beads at all. Routing an abstention through the guard would excuse it
    # every time and make the arm inert -- the linter would report "I read
    # nothing" and the gate would answer "excused".
    #
    # There is nothing for the guard to find here. The fix is to repair the
    # ledger read, not to open a bead.
    if [ "$closure_exit" -eq "$CLOSURE_LINT_EMPTY_POPULATION_CODE" ]; then
        echo "[-] Closure linter ABSTAINED: the audit matched zero beads, so its pass would have been vacuous" >&2
        return "$closure_exit"
    fi

    # Only a real linter verdict may be excused by tracked drift. If the guard
    # itself cannot run, the excuse is not established, so the linter's own
    # failure stands.
    if with_beads_read_locks ./scripts/verification-drift-guard.sh --gate=closure-lint --json; then
        echo "[!] Closure linter reported tracked violations; continuing via Verification Drift Guard"
        return 0
    fi

    return "$closure_exit"
}

# shellcheck disable=SC2329
untracked_work_audit_advisory() {
    # untracked-work-audit invokes `br list`, which owns its own Beads lock.
    # Holding verify's shared file locks around it makes br wait on the parent
    # process that is synchronously waiting for br: a 30-second self-deadlock.
    # The audit also has no meaningful input in rsync verification mirrors,
    # where the tracked tree is present without Git metadata.
    if ! git -C "$REPO_ROOT" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
        echo "ok: not in a git worktree; untracked work audit skipped"
        return 0
    fi
    ./scripts/untracked-work-audit.sh
}

# NOTE: ruby_gate_or_skip used to live here. It wrapped the command INSIDE
# run_stage and returned 0 when ruby was absent, so a stage that never executed
# was recorded as PASS and counted in STAGE_PASSED
# (bd-ruby-gate-skip-counts-as-passed-jb56c).
#
# It could not be repaired in place. run_stage executes its command as
# `eval "$cmd" 2>&1 | tee "$output_file"` -- a pipeline, therefore a subshell --
# so a wrapper calling record_gated_off from within the command would have its
# counter increments discarded by the subshell. The decision has to be made
# BEFORE run_stage is entered, which is why the three CI Proof-Lane call sites
# now guard on `command -v ruby` themselves.

# shellcheck disable=SC2329
e2e_event_contract_radar_advisory() {
    local report="${EE_E2E_EVENT_CONTRACT_RADAR_REPORT:-${REPO_ROOT}/.e2e-event-contract-radar-report.json}"
    local allowlist="${EE_E2E_EVENT_CONTRACT_RADAR_ALLOWLIST:-}"
    local args=(--json --quiet --output "$report")

    if [ -n "$allowlist" ]; then
        args+=(--allowlist "$allowlist")
    fi

    local json
    json=$("${REPO_ROOT}/scripts/e2e_event_contract_radar.sh" "${args[@]}")

    printf "%s\n" "$json" | jq -r --arg report "$report" '
        "e2e event contract radar: verdict=\(.verdict) scripts=\(.summary.scriptCount) pass=\(.summary.passCount) advisory_gap=\(.summary.advisoryGapCount) known_gap=\(.summary.knownGapCount) fail=\(.summary.failCount) missing_failure_verdicts=\(.summary.missingFailureVerdictCount)",
        "report: \($report)"
    '
}

artifact_retention_summary() {
    echo ""
    echo "Artifact retention:"

    if [ ! -x "${EE_BINARY:-}" ]; then
        echo "  skipped: ee binary not found at ${EE_BINARY:-<unset>}"
        return 0
    fi

    local summary_json
    if ! summary_json=$("$EE_BINARY" --workspace "$REPO_ROOT" diag artifacts --json 2>/dev/null); then
        echo "  skipped: ee diag artifacts failed"
        return 0
    fi

    if command -v jq >/dev/null 2>&1; then
        printf "%s\n" "$summary_json" | jq -r '
            .data.summary
            | "  roots=\(.rootCount) existing=\(.existingRoots) bytes=\(.totalBytes) over_budget=\(.overBudgetRoots) expired=\(.expiredRoots)"
        ' || true
    else
        echo "  report available via: $EE_BINARY --workspace $REPO_ROOT diag artifacts --json"
    fi
}

if [ "$FUZZ_TARGET_AUDIT_SELF_TEST" = "true" ]; then
    fuzz_target_audit_self_test
    exit 0
fi

if [ "$PLAN_DOC_SMOKE" = "true" ]; then
    run_stage "Plan Doc Smoke (bd-3usjw.23)" "plan_doc_smoke"
    # Early-exit mode: this runs ONE stage and stops. It gets the same
    # self-describing banner so a plan-doc-smoke run cannot be mistaken,
    # in a log, for a full sweep.
    verification_summary_banner
    printf "%b" "$STAGE_RESULTS"
    exit "$(verification_exit_status)"
else
    record_gated_off "Plan Doc Smoke (bd-3usjw.23)" "--plan-doc-smoke not set"
fi

# Gate 0.84: e2e invocation audit (bd-smxdr follow-on). No-Cargo. Asserts no
# scripts/e2e_*.sh is invoked by nothing. Six pieces of test machinery were
# found this session that existed and ran nowhere; the Rust side already had
# tests/suites/inventory.rs for exactly this, the shell side had nothing.
# 19 pre-existing orphans are baselined; the audit fails on CHANGE in BOTH
# directions, so the baseline can only shrink.
run_stage "E2E Invocation Audit Contract" "./scripts/e2e_invocation_audit.sh --self-test"
run_stage "E2E Invocation Audit" "./scripts/e2e_invocation_audit.sh"

# Gate 0.845: repo hygiene (bd-udjrq). This suite sat in the orphan baseline --
# written, committed, invoked by nothing -- which is the failure the audit
# immediately above exists to catch, so leaving it unwired next to that audit
# was its own small joke.
#
# Wired rather than merely triaged because it is the one orphan that could be:
# it references NO ee binary (zero EE_BIN / EE_BINARY / ee_resolve hits), so its
# pass does not depend on the stale 0.14.2 build this host is stuck with, unlike
# the other seven non-mesh orphans. It asserts .gitignore and .rchignore carry
# their required patterns, and measured 0 0 1 0 0 seconds over five runs.
#
# Its orphan_baseline.txt row is deleted in the same commit. The audit fails on a
# stale baseline entry as well as a new orphan, so wiring without that deletion
# would trade one red for another.
run_stage "Repo Hygiene E2E (bd-udjrq)" "./scripts/e2e_repo_hygiene.sh"

# Gate 0.846: read-coalescing E2E (bd-udjrq). Orphaned since it was written,
# and until e484da9a0 it could not have passed anywhere: `${2:-{}}` in its own
# emit_event appended a stray brace to every payload, so jq rejected the first
# event and the script exited 2 in under a second having asserted nothing.
# Fixed, it runs 20 assertions and writes 25 valid JSONL records.
#
# EE_BIN/EE_BINARY pinned, as gate 6.12698a-j requires of every suite that
# resolves a binary: unpinned, the harness now REFUSES (817412148) rather than
# silently testing whatever ee is on PATH, so an unpinned call site here would
# be a hard failure instead of a false pass.
#
# MEASURED, not unmeasured: 2.30 2.30 2.34 2.58 2.73 seconds over five runs,
# p50 2.34. Declared 3 so the hard-fail line lands at p50*3 = 9s. The
# unmeasured allowance is exhausted at 27/27, so a new stage must carry a real
# number; the budget for it comes from re-measuring Forbidden Dependencies
# from an over-declared 10 down to 2, not from raising the 600s ceiling.
#
# Its orphan_baseline.txt row is deleted in the same commit, for the same
# reason the Repo Hygiene row was: the audit fails on a stale baseline entry
# as well as on a new orphan.
run_stage "Read Coalescing E2E (bd-udjrq)" "EE_BIN=\"${CURRENT_SOURCE_EE_BINARY}\" EE_BINARY=\"${CURRENT_SOURCE_EE_BINARY}\" EE_E2E_TMPDIR=\"${E2E_TMPDIR_BASE}\" ./scripts/e2e_read_coalescing.sh"

# Gate 0.85: ee binary resolution + staleness contract (bd-smxdr). This
# no-Cargo test proves the shared resolver refuses a stale or missing binary
# BEFORE any e2e stage runs against one. It had existed unwired since May, so
# the guard it now carries would never have executed. Load-bearing arm: it also
# asserts a CURRENT binary is still accepted, because a guard that refuses
# everything would mask the defect rather than fix it.
run_stage "EE Binary Resolution Contract" "./scripts/lib/ee_binary_resolution_test.sh"

# Gate 0.9: Forbidden dependency scanner contract. This no-Cargo self-test
# proves the JSON metadata classifier catches forbidden crates before the live
# cargo-tree audit runs.
run_stage "Forbidden Dependency Contract" "./scripts/check-forbidden-deps.sh --self-test"

# Gate 1: Check Forbidden Dependencies
run_stage "Forbidden Dependencies" "./scripts/check-forbidden-deps.sh"

# Gate 2: Closure Discipline
run_stage "Closure Linter" "closure_lint_or_tracked_drift"

# Gate 2.5: Drift Guard (ensures red gates have tracking beads)
run_stage "Verification Drift Guard" "with_beads_read_locks ./scripts/verification-drift-guard.sh --json"

# Gate 3: Snapshot Proposal Guard
run_stage "Snapshot Proposal Guard" "snapshot_proposal_guard"

# Gate 3.49: Untracked work audit contract. This no-Cargo fixture proves the
# Beads FILE SURFACE matcher and orphan classifier before the live dirty-work
# advisory consumes current Beads/git state.
run_stage "Untracked Work Audit Contract" "./scripts/untracked-work-audit.sh --self-test"

# Gate 3.5: Advisory dirty-work ownership coverage. This remains advisory while
# multi-agent sessions routinely carry unrelated in-flight changes.
# GUARDED AT THE CALL SITE, NOT INSIDE THE WRAPPER (bd-ogtco), for the reason
# the bd-ruby-gate-skip-counts-as-passed-jb56c note above records: run_stage
# executes its command in a pipeline, so a record_gated_off from inside the
# wrapper has its counter increments discarded by the subshell.
#
# WHY THIS STAGE NEEDED IT. The wrapper already detects a tree with no Git
# metadata -- the rsync verification mirror -- and returns 0. That made a
# REQUIRED stage report PASS having executed nothing, which is exactly the
# shape jb56c removed next door. The stage is NAMED "(advisory)" but
# verify-budget.toml declares it required, so the name did not save it either.
#
# On the RCH clean-overlay tree (measured HAS_DOT_GIT=no) this stage cannot
# run. Saying so as NOT-APPLICABLE keeps it out of the passed count instead of
# inflating a green with a stage that was structurally incapable of failing.
if git -C "$REPO_ROOT" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
    run_stage "Untracked Work Audit (advisory)" "untracked_work_audit_advisory"
else
    record_gated_off "Untracked Work Audit (advisory)" \
        "no Git metadata in this tree (rsync verification mirror)"
fi

# Gate 3.59: Bridge staleness contract. This no-Cargo fixture harness proves
# signal classifications before the live advisory scan reads Beads state.
run_stage "Bridge Staleness Contract" "./scripts/bridge-staleness.sh --self-test"

# Gate 3.6: Advisory bridge-plan staleness. This always exits 0 and writes
# .bridge-staleness-report.json so the trailing verify summary includes whether
# Part II appears stale enough to plan the next bridge.
run_stage "Bridge Staleness Advisory" "with_beads_read_locks ./scripts/bridge-staleness.sh --quiet"

# Gate 3.69: Plan/bead drift contract. This no-Cargo fixture harness proves
# warning classifications and BV hints before the live advisory scan reads
# Beads state.
run_stage "Plan Drift Contract" "./scripts/plan-drift.sh --self-test"

# Gate 3.7: Advisory plan/bead drift. This always exits 0 and writes
# .plan-drift-report.json with BV-friendly warning hints for active
# implements-surface beads whose plan_doc_section labels point at evolved text.
run_stage "Plan Drift Advisory" "with_beads_read_locks ./scripts/plan-drift.sh --quiet"

# Gate 3.79: Tracing field contract. TWO ARMS, deliberately, in one stage.
#
# The self-test proves the checker's own predicate still works. The second
# invocation runs the check against the TREE. Until bd-c79fk this stage ran only
# the self-test, so a stage named "Tracing Field Contract" evaluated the contract
# against zero production surfaces while reporting PASS -- and the only other
# automated invocation passed `--bead __no_such_bead__`, auditing no beads at
# all. Seven real violations had accumulated unseen.
#
# Both arms share ONE stage rather than adding a second, because the verify
# budget currently admits no new stage: the non-benchmark p50s total exactly 600
# against a `<= 600` ceiling, and UNMEASURED_STAGE_ALLOWANCE sits exactly at its
# 27 down-only ratchet. Widening an existing stage needs no budget entry.
#
# The real arm is baselined (tests/fixtures/tracing_field/violation_baseline.txt)
# so it fails on NEW violations and on STALE baseline entries, not on the
# pre-existing debt. That keeps the gate honest without wedging every pane on
# seven surfaces nobody has been asked to instrument yet.
run_stage "Tracing Field Contract" "./scripts/check-tracing-fields.sh --self-test && ./scripts/check-tracing-fields.sh"

# Gate 3.8: Advisory contract-drift radar (bd-31nul.5). Cargo-free static
# scan of current-facing agent docs for stale envelope versions, unknown
# JSONC envelope schema ids, and degraded-code documentation that lacks a
# matching tests/fixtures/failure_modes/<code>.json fixture. Always exits 0
# and writes .contract-drift-radar-report.json with schema
# "ee.contract_drift_radar.v1". The Cargo-backed proof (full JSONC envelope
# validation against jsonschema files) is the schema_drift contracts test
# under cargo test -p ee --test contracts and stays an RCH-only surface.
run_stage "Contract Drift Radar Advisory" "./scripts/contract-drift-radar.sh --quiet"

# Gate 3.81: Deterministic self-test for the static radar's own report/event
# contract. This is shell-only and does not run Cargo or RCH.
run_stage "Contract Drift Radar Self-Test" "./scripts/contract-drift-radar.sh --self-test"

# Gate 3.84: E2E event-contract radar golden contract. This no-Cargo harness
# freezes the scanner report matrix, schema strictness, and negative fixture
# before the live advisory scan reads the full shell E2E surface.
run_stage "E2E Event Contract Radar Contract" "./scripts/e2e_event_contract_radar_golden.sh"

# Gate 3.85: Advisory e2e event-contract radar (bd-2ljka.4). This is a
# no-Cargo static scanner for shell E2E evidence logging. It writes
# .e2e-event-contract-radar-report.json by default and does not fail the
# readiness gate for advisory or known gaps; scanner/runtime errors still fail.
run_stage "E2E Event Contract Radar Advisory" "e2e_event_contract_radar_advisory"

# Gate 3.855: Work-packet no-mutation contract. This shell-only harness proves
# packet generation and the agent-facing claim-gate consumer stay read-only,
# refuse unsafe claim states, and include install-check freshness fixtures.
run_stage "Work Packet No-Mutation Contract" "./scripts/e2e_swarm_work_packet_no_mutation.sh"

# Gate 3.856: Agent Mail snapshot bridge contract. This no-Cargo self-test
# verifies redaction, normalization, degradation, and companion coordination
# output without requiring a live Agent Mail process.
run_stage "Agent Mail Snapshot Contract" "./scripts/agent_mail_snapshot.sh --self-test"

# Gate 3.86: Panic-helper radar contract (bd-ppbue.30). This no-Cargo harness
# validates ee.panic_helper_radar.v1 schema/golden fixtures so scanner drift is
# caught without scanning the entire legacy Rust tree.
run_stage "Panic Helper Radar Contract" "./scripts/panic_helper_radar_golden.sh"

# Gate 3.87: Swarm SLO replay contract (bd-ppbue.31). This no-Cargo harness
# replays the compact swarm trace fixture, checks deterministic tie ordering,
# verifies summary schema/mutation flags, and fails before Cargo-backed gates
# if the shell replay contract drifts.
run_stage "Swarm SLO Replay Contract" "./scripts/e2e_overhaul/swarm_slo_replay.sh"

# Gate 3.88: CI proof-lane snapshot contract (bd-1n3x1.7). This no-Cargo
# harness transforms offline proof-lane fixtures and verifies duplicate-run,
# missing/stale artifact, checksum, surface-probe, unavailable-gh, and invalid
# SHA behavior before agents rely on CI artifact source-authority evidence.
if command -v ruby >/dev/null 2>&1; then
    run_stage "CI Proof-Lane Snapshot Contract" "./scripts/ci_proof_lane_snapshot_fixture_test.sh"
else
    record_gated_off "CI Proof-Lane Snapshot Contract" "ruby unavailable on this host"
fi

# Gate 3.89: CI proof-lane hygiene contract. This no-Cargo synthetic harness
# exercises workflow-dispatch, duplicate-dispatch, cancellable CI artifacts,
# release artifacts, and unclassified artifact-lane policy without reading
# live workflows or invoking Cargo.
if command -v ruby >/dev/null 2>&1; then
    run_stage "CI Proof-Lane Hygiene Contract" "./scripts/ci_proof_lane_hygiene.sh --self-test"
else
    record_gated_off "CI Proof-Lane Hygiene Contract" "ruby unavailable on this host"
fi

# Gate 3.895: CI proof-lane hygiene advisory (bd-1n3x1.8). This no-Cargo,
# network-free workflow scanner emits ee.ci_proof_lane_hygiene.v1 so agents see
# duplicate-dispatch, cancel-in-progress, artifact-retention, release-artifact,
# and unclassified artifact-lane posture before spending CI/RCH proof slots.
if command -v ruby >/dev/null 2>&1; then
    run_stage "CI Proof-Lane Hygiene Advisory" "./scripts/ci_proof_lane_hygiene.sh --json"
else
    record_gated_off "CI Proof-Lane Hygiene Advisory" "ruby unavailable on this host"
fi

# Gate 3.896: Release provenance static contract. This no-Cargo gate verifies
# the release workflow, installer, README, publish checklist, and audit script
# still advertise the SLSA/Sigstore provenance contract before release work.
run_stage "Release Provenance Contract" "./scripts/e2e_release_provenance.sh --static"

# Gate 3.90: RCH doc examples classifier contract. This no-Cargo self-test
# proves the command classifier denies local Cargo examples while accepting
# RCH-wrapped proof recipes before the live docs scan.
run_stage "RCH Doc Examples Contract" "python3 scripts/check-rch-doc-examples.py --self-test"

# Gate 3.905: RCH doc examples lint (bd-1n3x1.9). This no-Cargo docs scanner
# fails before expensive gates if AGENTS.md, README.md, or the RCH runbooks grow
# copy-pasteable local Cargo compile examples that bypass the verifier wrapper.
run_stage "RCH Doc Examples Lint" "python3 scripts/check-rch-doc-examples.py --json"

# Gate 3.91: Local Cargo tripwire contract (bd-1n3x1.10). This deterministic
# self-test validates command-shape denials, JSON repair actions, and fixture
# process classification without scanning live peer processes or running Cargo.
run_stage "Local Cargo Tripwire Contract" "./scripts/check-local-cargo-tripwire.sh --self-test"

# Gate 3.92: RCH portability diagnostic contract (bd-1n3x1.10). This
# deterministic self-test verifies the Mac-leak anomaly detector for remote
# transcripts without mutating workers, launching RCH, or deleting artifacts.
run_stage "RCH Portability Diagnostic Contract" "./scripts/check-rch-portability.sh --self-test"

# Gate 4.74: Package artifact leakage self-test. This proves the manifest
# exclude set and forbidden path classifier before the live cargo package list
# gate, without invoking Cargo.
run_stage "Package Artifact Leak Contract" "./scripts/package-artifact-leak-check.sh --self-test"

# Gate 4.75: Package artifact leakage guard. This is a quick packaging gate:
# it runs cargo package --list without building and fails if local/generated
# tracker, perf, backup, or temp artifact paths would enter the published crate.
run_stage "Package Artifact Leak Check (bd-2ifvx)" "./scripts/package-artifact-leak-check.sh"

# Gate 4.8: Fuzz target audit contract. This no-Cargo self-test proves the
# manifest, target source, README sweep, and nightly-proof matchers before the
# live static cargo-fuzz target registration/docs audit.
run_stage "Fuzz Target Audit Contract" "fuzz_target_audit_self_test"

# Gate 4.81: Static cargo-fuzz target registration/docs audit. This is a
# no-build guard; actual cargo-fuzz sweeps remain explicit RCH-only evidence.
run_stage "Fuzz Target Audit (bd-bife.10)" "fuzz_target_audit"

if [ "$INCLUDE_FUZZ_SMOKE" = "true" ]; then
    run_stage "Fuzz Smoke: search query parser (bd-2j2h0)" "fuzz_smoke"
else
    record_gated_off "Fuzz Smoke: search query parser (bd-2j2h0)" "--include-fuzz-smoke not set"
fi

# Gate 4: Strategic Vision Coverage
run_stage "Vision Coverage" "with_beads_read_locks sh ./scripts/vision-coverage.sh --json"

# Gate 4.5: Mechanized proof artifacts. Missing Lean4/TLA+ tools degrade
# inside the driver instead of blocking the default readiness gate.
# Skipped under --ci-smoke because the Lean4/TLA+ driver depends on
# optional external toolchains that smoke runs should not require.
if [ "$CI_SMOKE" != "true" ]; then
    run_stage "Proof Verification (bd-nnfq4)" "./scripts/e2e_overhaul/proof_verify.sh"
else
    record_gated_off "Proof Verification (bd-nnfq4)" "ci-smoke"
fi

# Gate 5: Core Cargo Tests (Contracts, Logic, Golden). Benchmarks are
# deliberately excluded here and run only through the explicit benchmark gate.
# GRADED, NOT MERELY EXITED (bd-reality-core-convergence-1azkt.5, bullet 4).
#
# Bullet 4 requires that an IGNORED, ZERO or OMITTED test make the run
# non-success. Cargo's exit code cannot express any of them: `cargo test` over a
# filter that matches nothing prints `running 0 tests` / `test result: ok.` and
# EXITS 0, so this stage passed. Nine of the bullet's thirteen states were
# already gated at the stage level; these sat at the TEST level, below where
# run_stage can see.
#
# scripts/lib/grade_test_log.py decides them, and already grades the RCH lane and
# the proof capsule. It could not be pointed here until de65434cf, because this
# invocation reports MANY targets and the grader refused a multi-target log as
# ambiguous. --all-targets grades every reconciled pair together and fails when
# any target failed or when the TOTAL announced across all of them is zero.
#
# THE STAGE IS MODIFIED, NOT ADDED, AND NOT RENAMED -- deliberately. The budget
# has one second of headroom (599 of 600), so a new [[stage]] would spend it; and
# tests/verification_drift_guard.rs pins run_stage labels, so a rename would red
# a guard that would be right to red. p50 stays 120: grading parses a log that
# was captured anyway.
#
# BOTH EXITS ARE READ -- see the capture comment inside the function for why
# that needs `||` rather than a bare statement, and what happened when it did
# not. `${PIPESTATUS[0]}` is cargo's; the grader's is its own. A green grade over
# a failing cargo run, or a green cargo run over an unreadable log, must not
# become a pass -- one value cannot express two states.
unit_contract_golden_tests() {
    local log rc=0 grade=0
    log="$(mktemp "${TMPDIR:-/tmp}/ee-unit-contract-golden.XXXXXX")"
    # CAPTURE THROUGH `||`, NEVER AS A BARE STATEMENT FOLLOWED BY $?.
    #
    # verify.sh:2 is `set -euo pipefail`. A bare `cargo ... | tee` that fails
    # makes the pipeline non-zero and ABORTS THIS FUNCTION AT THAT LINE, so the
    # `rc=${PIPESTATUS[0]}` beneath it never executes -- and the same for a bare
    # grader call followed by `grade=$?`. Both reads, both diagnostics and the
    # `rm -f` were unreachable in the first version of this function.
    #
    # Measured in a mirror of run_stage's exact shape (set -euo pipefail, the
    # function evaluated inside an `if` and piped to tee), because bash's
    # documented "-e is ignored in an if condition" does NOT save this and
    # reading the manual said otherwise:
    #     bare form, inner cmd fails  -> the line after the pipeline NEVER RAN
    #     `||` form,  inner cmd fails -> rc=1 read, grade read, stage fails
    #     `||` form,  all succeed     -> both read, stage passes
    # Commands on the left of `||` are exempt from -e, so the capture survives.
    #
    # This is the SECOND time this shape has shipped dead code in this file
    # family today (4c3678daf was the first). It is not decidable by reading a
    # diff; only by running both arms.
    cargo test --workspace --lib --bins --tests --examples -- --test-threads=1 2>&1 \
        | tee "$log" || rc=${PIPESTATUS[0]}
    python3 "${REPO_ROOT}/scripts/lib/grade_test_log.py" --all-targets "$log" || grade=$?
    # Unconditional, and before any return: in the first version this sat on the
    # unreachable path, so every failing run leaked a temp file.
    rm -f "$log"
    if [ "$rc" -ne 0 ]; then
        printf 'verify: cargo test exited %s.\n' "$rc" >&2
        return "$rc"
    fi
    if [ "$grade" -ne 0 ]; then
        printf 'verify: cargo test exited 0 but the log does NOT grade green.\n' >&2
        printf 'verify: an exit code cannot see a zero-test run, an unreconciled\n' >&2
        printf 'verify: summary, or a target that never reported -- the grader can.\n' >&2
        return 1
    fi
    return 0
}
run_stage "Unit, Contract, and Golden Tests" "unit_contract_golden_tests"

# Gate 5.1: mcp lib unit tests (bd-up1hk). The stage above builds with default
# features, and `mcp` is not among them, so src/mcp.rs is not compiled and its
# 59 #[test] functions execute nowhere. The wrapper carries a vacuity guard:
# without --features mcp the `mcp::` filter matches nothing and cargo exits 0
# having run no tests, which would be a second false green in the exact shape
# this gate exists to remove.
run_stage "MCP Lib Unit Tests Guard (bd-up1hk)" "./scripts/mcp_lib_tests.sh --self-test"
run_stage "MCP Lib Unit Tests (bd-up1hk)" "./scripts/mcp_lib_tests.sh"

# Gate 5.2: Lexical relevance contract (bd-reality-core-convergence-1azkt.11).
#
# The pins fail against the current product on purpose: they document that a
# lexical pool's top hit renders relevanceScore 1.0 whatever its raw BM25 was
# while scoreKind calls that value `unit_normalized`, and that relevance-floor
# admission is decided in the query-relative domain. Gate 5 above runs every
# [[test]] target, so they carry #[ignore] to keep that REQUIRED stage's verdict
# about the product rather than about this bead; `--ignored` is what executes
# them here.
#
# Both arms run the binary Gate 5 just built rather than re-entering cargo.
# Measured on RCH hz4 at 82926d4c5: the pins themselves take 0.00s while the
# cargo wrapper around them took 74s re-walking freshness across ~350 crates
# that Gate 5 had walked moments earlier. Paying for that scan twice is waste in
# the gate independent of anything being added to it.
#
# The guard is REQUIRED and the run arm is TRACKED_RED. That split is the point:
# run_stage excuses ANY nonzero exit from a tracked-red stage, so without a
# required arm a missing binary, a stale binary or a renamed test would be
# recorded as the known red and be indistinguishable from it.
run_stage "Lexical Relevance Contract Harness Guard" "./scripts/lexical_relevance_pins.sh --guard"
run_stage "Lexical Relevance Contract (tracked red)" "./scripts/lexical_relevance_pins.sh"

# Gate 6: Basic End-to-End
run_stage "Basic E2E Scripts" "./scripts/e2e_test.sh"

# Gate 6.05: Agent-facing output budget guard for status and swarm brief.
run_stage "Output Budget E2E (bd-kua65)" "./scripts/e2e_output_budget.sh"

# Gate 6.055: Output-token governor contract — deterministic corpus,
# ceiling sweep across the wired surfaces, exact-count cursor drains,
# mid-pagination staleness, and EE_MAX_OUTPUT_TOKENS equivalence
# (ADR 0063, bd-7lvbg.4). Corpus pinned below the script's 500-row
# default for gate runtime (debug-build seeding; the bd-2efx1 per-line
# index swap is fixed, the residual cost is per-line dedup/connection
# overhead) — the drain/staleness assertions are corpus-size
# independent.
run_stage "Output Governor E2E (bd-7lvbg.4)" "EE_GOVERNOR_E2E_CORPUS=120 ./scripts/e2e_output_governor.sh"

# Gate 6.056: pack delta ergonomics — per-agent --since last baseline
# ledger, markdown delta rendering, per-agent isolation, and the
# --no-baseline-write opt-out (bd-7lvbg.6).
run_stage "Pack Delta E2E (bd-7lvbg.6)" "./scripts/e2e_pack_delta.sh"

# Gate 6.057: install-freshness claim-gate E2E — real binary + fake-PATH
# fixtures (stale shim + current symlink) prove a stale/shadowed install fails
# the swarm work-packet claim gate closed (safeToClaim=false, shadowed_binary,
# no claim command) while a fresh binary stays authoritative and lets
# downstream gates decide; emits ee.test_event.v1 evidence (bd-3utv2.7).
run_stage "Install Freshness Claim-Gate E2E (bd-3utv2.7)" "./scripts/e2e_install_freshness.sh"

# Gate 6.057: code-anchored recall plus harness hooks — real scratch git
# workspace, anchored memories, recall path/diff selectors, Claude Code hook
# install, PreToolUse context injection, and Bash failure journal capture.
run_stage "Recall Hooks E2E (bd-u875s.5)" "EE_E2E_TMPDIR=\"${E2E_TMPDIR_BASE}\" ./scripts/e2e_recall_hooks.sh"

# Gate 6.06: Replay lab smoke. This is intentionally no-Cargo and exercises
# the public `ee lab swarm replay --dry-run` path plus ee.test_event.v1 logging
# before the heavier replay/RCH proof lanes.
run_stage "Replay Lab Smoke E2E (bd-ppbue.8)" "./scripts/e2e_overhaul/swarm_replay_lab_smoke.sh"

# Gate 6.07: Dueling Wizards why-not real-binary E2E.
run_stage "Dueling Wizards Why-Not E2E" "./scripts/e2e_why_not.sh"

# Gate 6.075: Primer + AGENTS.md bridge real-binary E2E (bd-39tzu.5).
# Real-binary, no-Cargo: cold/warm primer cache contract, generation
# invalidation, managed-block export/backup/hand-edit refusal, candidates-only
# import, and the drift diagnostic, all logging ee.test_event.v1 lines.
run_stage "Primer/AGENTS.md Bridge E2E (bd-39tzu.5)" "./scripts/e2e_primer_agentsmd.sh"

# Gate 6.08: Dueling Wizards cross-cutting static E2E. This is intentionally
# no-Cargo and checks the shared manifests/static gates before the feature
# scripts that depend on them.
run_stage "Dueling Wizards Cross-Cutting Static E2E" "./scripts/e2e_cross_cutting.sh"

# Gate 6.09: Dueling Wizards evidence-harvester real-binary E2E. Real-binary,
# no-Cargo: the script self-guards (log_drop) when the harvest/calibration CLI
# is not yet built into the binary, so it never false-fails the gate.
run_stage "Dueling Wizards Evidence Harvester E2E" "./scripts/e2e_evidence_harvester.sh"

# Gate 6.10: Dueling Wizards LOD packing real-binary E2E. Real-binary, no-Cargo:
# hard-asserts pack budget + hash determinism; condition-guards (log_drop) the
# peripheral-index/link-only tier and the cli-gated --no-lod parity.
run_stage "Dueling Wizards LOD Packing E2E" "./scripts/e2e_lod_packing.sh"

# Gate 6.11: Dueling Wizards House Rules real-binary E2E. Real-binary, no-Cargo:
# hard-asserts the houseRules insights section in its origin workspace (lists the
# global-tagged rule, no workspace-local leak); condition-guards (log_drop) the
# cross-workspace shared-DB read path and the cli-gated `remember --scope global`.
run_stage "Dueling Wizards House Rules E2E" "./scripts/e2e_house_rules.sh"

# Gate 6.12: Dueling Wizards typed-kinds real-binary E2E. Real-binary, no-Cargo:
# proves extraction-first failure fields through --kind/--field searches, typed
# decision supersedes graph projection, and unchanged bare --kind behavior.
run_stage "Dueling Wizards Typed Kinds E2E" "./scripts/e2e_typed_kinds.sh"

# Gate 6.125: Ask direct-answer real-binary E2E (bd-169v0.5). No-Cargo:
# proves extractive answer citations, corroboration confidence lift, conflict
# sides, thresholded abstention, fail-closed --require-confidence, and retained
# ee.test_event.v1 evidence artifacts.
run_stage "Ask E2E (bd-169v0.5)" "./scripts/e2e_ask.sh"

# Gate 6.126: Journal capture/distillation real-binary E2E (bd-1pi9m.6).
# No-Cargo: appends hook/stdin journal entries, proves redaction before
# persistence, distills repeated failures, applies the candidate, then checks
# search, pack-item outcome feedback, and outcome trace with ee.test_event.v1
# evidence.
run_stage "Journal Capture E2E (bd-1pi9m.6)" "./scripts/e2e_journal_capture.sh"

# Gate 6.126: Single-shot write contention E2E (bd-d67os.27 item 1). No-mock:
# N concurrent OS processes interleave `ee journal append` and `ee remember`
# against one workspace DB. Journal appends must never drop (the bd-d67os.26
# flock-classification fix class, proven end-to-end), and progress-aware
# advisory-lock waiting must keep every remember write lossless (bd-rs4cm).
run_stage "Write Contention E2E (bd-d67os.27)" "cargo build --locked --bin ee && EE_BIN=\"${CURRENT_SOURCE_EE_BINARY}\" EE_BINARY=\"${CURRENT_SOURCE_EE_BINARY}\" EE_E2E_TMPDIR=\"${E2E_TMPDIR_BASE}\" ./scripts/e2e_single_shot_write_contention.sh"

# Gate 6.1265: Capture-track real-binary E2E (bd-2vq2z.20). No-Cargo:
# proves ambient capture suggestions are read-only and workspace-stable,
# from-commit/from-diff remember capture is dry-run-first with anchors,
# redaction and audit checks, and session-arc proposals stay curation-gated.
# EE_BIN/EE_BINARY are pinned explicitly here, matching the Write Contention
# stage above. The global export at :200-202 was being shadowed by the script's
# own PATH default, so this stage validated whatever `ee` was installed
# (bd-smxdr). Passing both names keeps the pin visible at the call site rather
# than depending on an export several hundred lines away.
run_stage "Capture Track E2E (bd-2vq2z.20)" "EE_BIN=\"${CURRENT_SOURCE_EE_BINARY}\" EE_BINARY=\"${CURRENT_SOURCE_EE_BINARY}\" EE_E2E_TMPDIR=\"${E2E_TMPDIR_BASE}\" EE_E2E_KEEP=1 ./scripts/e2e_capture.sh"

# Gate 6.1266: Shadow retrieval-tuning E2E (bd-2tehh.4 / ADR 0070). Real
# binary: sparse corpus abstains with the fixture-backed degraded code and
# persists the report; promote refuses abstained reports with exit 7;
# a promotable report dry-runs without writing, applies the [search]
# overlay, and demote restores the prior config bytes exactly.
run_stage "Shadow Retrieval Tuning E2E (bd-2tehh.4)" "EE_E2E_TMPDIR=\"${E2E_TMPDIR_BASE}\" ./scripts/e2e_shadow_retrieval_tuning.sh"

# Gate 6.1267: Global knowledge lane E2E (bd-1bfwa.4). Three real
# workspaces + a hermetic XDG user-global store: evidence-gated promote
# (branch asserted from the actual trust class), cross-workspace
# storeLane=global surfacing, participate=false isolation with the
# honest global_lane_disabled code, and demote-global tombstoning.
run_stage "Global Lane E2E (bd-1bfwa.4)" "EE_E2E_TMPDIR=\"${E2E_TMPDIR_BASE}\" ./scripts/e2e_global_lane.sh"

# Gate 6.1268: Beads export-integrity classifier self-test (bd-2p297.1/.2).
# Fixture-driven: safe-repair candidacy, destructive-export refusal, merge
# markers, unhealthy DB, transient partial write. No live tracker touched.
run_stage "Beads Export Repair Self-Test (bd-2p297.1, bd-2p297.2)" "./scripts/beads_export_repair.sh --self-test"
run_stage "Beads Export Fixture Suite (bd-2p297.3)" "./scripts/beads_export_repair.sh --fixture-suite tests/fixtures/beads_export"

# Gate 6.1269: Graph-intelligence E2E (bd-3a1op.6 / ADR 0066). Real binary:
# empty-graph honesty, hub-pattern suggestion with opposed-polarity
# contradiction typing, --propose emission + re-propose dedup, and the
# curate validate/apply lifecycle creating the typed link.
run_stage "Graph Intel E2E (bd-3a1op.6)" "EE_BIN=\"${CURRENT_SOURCE_EE_BINARY}\" EE_BINARY=\"${CURRENT_SOURCE_EE_BINARY}\" EE_E2E_TMPDIR=\"${E2E_TMPDIR_BASE}\" ./scripts/e2e_graph_intel.sh"

# Gate 6.12695: Session-resume E2E (bd-resume-verb-v0f57). Real binary:
# empty-store no-session-evidence honesty, tagged-session grouping, revisit
# decisions + next-tagged open loops, and the superseded-note stale marker.
run_stage "Resume E2E (bd-resume-verb-v0f57)" "EE_E2E_TMPDIR=\"${E2E_TMPDIR_BASE}\" ./scripts/e2e_resume.sh"

# Gate 6.12696: Memory-debt E2E slice 1 (bd-3ap2m.4). Real binary: planted
# orphan detected with an Actionable suggested command (healthy linked
# control stays clean), resolving the debt strictly shrinks the class count,
# and repeated missed searches form a learn-gaps cluster.
run_stage "Memory Debt E2E (bd-3ap2m.4)" "EE_BIN=\"${CURRENT_SOURCE_EE_BINARY}\" EE_BINARY=\"${CURRENT_SOURCE_EE_BINARY}\" EE_E2E_TMPDIR=\"${E2E_TMPDIR_BASE}\" ./scripts/e2e_memory_debt.sh"

# Gate 6.12697a-d: four e2e suites that were referenced by NOTHING (bd-smxdr).
# (Numbered after 6.12696 memory-debt; 6.1269 is graph-intel.)
#
# These were committed and then wired into no runner at all -- not verify.sh,
# not any script, not any workflow. They are 662 lines of assertions that had
# never executed once.
#
# PRE-REGISTERED EXPECTATION, recorded before the first run so neither a green
# nor a red can be spun afterwards:
#   - All 12 ee subcommands they invoke (agent-docs, why-not, timeline, trust,
#     verify, outcome, diag, pack, remember, init, memory, why) DO exist at
#     HEAD, so they are not obsolete and a first run has a real chance of
#     passing.
#   - They have nonetheless never run, so their assertions may have drifted
#     against surfaces that moved underneath them.
#   - A RED here is therefore most likely a first-execution finding, NOT a
#     regression from this session's commits. Read it as "this suite has
#     finally run" before reading it as "someone broke something".
#
# Wired as BLOCKING rather than advisory on purpose. An advisory stage that
# stays permanently red is the appearance-of-coverage pattern this session has
# been removing; blocking forces the real decision, which is fix or retire.
# Each pins EE_BIN/EE_BINARY: they carried a PATH default until 48b20809f.
run_stage "Agent Docs Env E2E (first execution)" "EE_BIN=\"${CURRENT_SOURCE_EE_BINARY}\" EE_BINARY=\"${CURRENT_SOURCE_EE_BINARY}\" EE_E2E_TMPDIR=\"${E2E_TMPDIR_BASE}\" ./scripts/e2e_agent_docs_env.sh"
run_stage "Coverage Gap E2E (first execution)" "EE_BIN=\"${CURRENT_SOURCE_EE_BINARY}\" EE_BINARY=\"${CURRENT_SOURCE_EE_BINARY}\" EE_E2E_TMPDIR=\"${E2E_TMPDIR_BASE}\" ./scripts/e2e_coverage_gap.sh"
run_stage "Timeline E2E (first execution)" "EE_BIN=\"${CURRENT_SOURCE_EE_BINARY}\" EE_BINARY=\"${CURRENT_SOURCE_EE_BINARY}\" EE_E2E_TMPDIR=\"${E2E_TMPDIR_BASE}\" ./scripts/e2e_timeline.sh"
run_stage "Trust Freshness E2E (first execution)" "EE_BIN=\"${CURRENT_SOURCE_EE_BINARY}\" EE_BINARY=\"${CURRENT_SOURCE_EE_BINARY}\" EE_E2E_TMPDIR=\"${E2E_TMPDIR_BASE}\" ./scripts/e2e_trust_freshness.sh"

# Gate 6.12698a-j: bd-udjrq triage — ten e2e suites that were invoked by
# nothing, now executed. 3,047 lines of assertions across twelve orphans were
# read before wiring; these ten reference only surfaces that exist at HEAD and
# create their own fixtures, so none is stale-by-dependency.
#
# PRE-REGISTERED, same discipline as 6.12697: a RED here is a FIRST-EXECUTION
# finding, not a regression from this session. Reading assertions proves a
# suite is not obsolete; it cannot prove it passes. Stage names say so.
#
# EE_BIN/EE_BINARY pinned at each call site. Unpinned, these resolve through
# _harness_resolve_ee_bin's bare `printf ee` fallback to whatever is installed
# -- the path that made e2e_field_report_suite.sh emit five false assert_fails
# against 0.14.2 at 06:17 today. Wiring without the pin would reproduce that
# ten times over.
#
# NOT wired, with reasons:
#   e2e_backup_roundtrip.sh   RETIRED as superseded by the registered
#                             [[test]] target tests/e2e_backup_restore_roundtrip.rs
#                             (6 tests, updated 2026-09-15). File NOT deleted
#                             per RULE 1; it stays baselined pending an
#                             operator decision on removal.
#   e2e_field_report_suite.sh BLOCKED on the unguarded harness PATH fallback
#                             (scripts/lib/e2e_harness.sh:69). It is PROVEN to
#                             emit false assert_fails against a stale binary;
#                             wiring it now would create exactly the
#                             permanently-red stage this triage exists to avoid.
run_stage "Anchors E2E (first execution)" "EE_BIN=\"${CURRENT_SOURCE_EE_BINARY}\" EE_BINARY=\"${CURRENT_SOURCE_EE_BINARY}\" EE_E2E_TMPDIR=\"${E2E_TMPDIR_BASE}\" ./scripts/e2e_anchors.sh"
run_stage "Bridge Exemption E2E (first execution)" "EE_BIN=\"${CURRENT_SOURCE_EE_BINARY}\" EE_BINARY=\"${CURRENT_SOURCE_EE_BINARY}\" EE_E2E_TMPDIR=\"${E2E_TMPDIR_BASE}\" ./scripts/e2e_bridge_exemption.sh"
run_stage "Command Inventory E2E (first execution)" "EE_BIN=\"${CURRENT_SOURCE_EE_BINARY}\" EE_BINARY=\"${CURRENT_SOURCE_EE_BINARY}\" EE_E2E_TMPDIR=\"${E2E_TMPDIR_BASE}\" ./scripts/e2e_command_inventory.sh"
run_stage "Delivery E2E (first execution)" "EE_BIN=\"${CURRENT_SOURCE_EE_BINARY}\" EE_BINARY=\"${CURRENT_SOURCE_EE_BINARY}\" EE_E2E_TMPDIR=\"${E2E_TMPDIR_BASE}\" ./scripts/e2e_delivery.sh"
run_stage "Provenance Reverify E2E (first execution)" "EE_BIN=\"${CURRENT_SOURCE_EE_BINARY}\" EE_BINARY=\"${CURRENT_SOURCE_EE_BINARY}\" EE_E2E_TMPDIR=\"${E2E_TMPDIR_BASE}\" ./scripts/e2e_provenance_reverify.sh"
run_stage "Reach E2E (first execution)" "EE_BIN=\"${CURRENT_SOURCE_EE_BINARY}\" EE_BINARY=\"${CURRENT_SOURCE_EE_BINARY}\" EE_E2E_TMPDIR=\"${E2E_TMPDIR_BASE}\" ./scripts/e2e_reach.sh"
run_stage "Rerank Precision Gain E2E (first execution)" "EE_BIN=\"${CURRENT_SOURCE_EE_BINARY}\" EE_BINARY=\"${CURRENT_SOURCE_EE_BINARY}\" EE_E2E_TMPDIR=\"${E2E_TMPDIR_BASE}\" ./scripts/e2e_rerank_precision_gain.sh"
run_stage "Reservation Pressure E2E (first execution)" "EE_BIN=\"${CURRENT_SOURCE_EE_BINARY}\" EE_BINARY=\"${CURRENT_SOURCE_EE_BINARY}\" EE_E2E_TMPDIR=\"${E2E_TMPDIR_BASE}\" ./scripts/e2e_reservation_pressure.sh"
run_stage "Search Weight Config E2E (first execution)" "EE_BIN=\"${CURRENT_SOURCE_EE_BINARY}\" EE_BINARY=\"${CURRENT_SOURCE_EE_BINARY}\" EE_E2E_TMPDIR=\"${E2E_TMPDIR_BASE}\" ./scripts/e2e_search_weight_config.sh"
run_stage "Similar Scope E2E (first execution)" "EE_BIN=\"${CURRENT_SOURCE_EE_BINARY}\" EE_BINARY=\"${CURRENT_SOURCE_EE_BINARY}\" EE_E2E_TMPDIR=\"${E2E_TMPDIR_BASE}\" ./scripts/e2e_similar_scope.sh"

# Gate 6.127: Ergonomics real-binary E2E (bd-1et0v.22). No-Cargo:
# proves `ee context` remains an alias for canonical `ee pack` while carrying
# the deprecated_alias info row, and proves PATH-shadow doctor findings are
# advisory-only and offline/no-network.
run_stage "Ergonomics E2E (bd-1et0v.22)" "EE_E2E_TMPDIR=\"${E2E_TMPDIR_BASE}\" ./scripts/e2e_ergonomics.sh"

# Gate 6.128: concise default doctor real-binary E2E (bd-1et0v.15). No-Cargo:
# proves default `ee doctor --json` exposes only the compact core verdict,
# actionable core repairs, and advisory summary while `ee doctor --full --json`
# retains the exhaustive mesh/RCH/verification diagnostic blocks.
run_stage "Doctor Concise E2E (bd-1et0v.15)" "EE_E2E_TMPDIR=\"${E2E_TMPDIR_BASE}\" ./scripts/e2e_doctor_concise.sh"

# Gate 6.129: doctor-health real-binary E2E (bd-1et0v.21). No-Cargo:
# proves initialized workspaces are green by default, concise output stays
# compact, --full retains exhaustive advisory/host-calibration diagnostics, and
# synthetic CASS/RCH advisory failures do not flip the top-line posture.
run_stage "Doctor Health E2E (bd-1et0v.21)" "EE_E2E_TMPDIR=\"${E2E_TMPDIR_BASE}\" ./scripts/e2e_doctor_health.sh"

# Gate 6.1292: memory-health scorecard real-binary E2E (bd-2vq2z.14).
# No-Cargo: proves scorecard schema, debt snapshot trend reads, duplicate/
# provenance debt scoring, top repair actions, and read-only determinism.
run_stage "Health Scorecard E2E (bd-2vq2z.14)" "EE_E2E_TMPDIR=\"${E2E_TMPDIR_BASE}\" ./scripts/e2e_health_scorecard.sh"

# Gate 6.1293: consolidation Maintain-loop real-binary E2E (bd-1oep7).
# No-Cargo: proves steward consolidation_pass dry-run non-mutation, budget
# cancellation, deterministic dedupe, consolidate-absorb apply (lineage,
# tombstone, audit chain), workflow-emitted index refresh truthfulness,
# deduplicated search, and idempotent re-runs.
run_stage "Consolidation E2E (bd-1oep7)" "EE_E2E_TMPDIR=\"${E2E_TMPDIR_BASE}\" ./scripts/e2e_consolidation.sh"

# Gate 6.1295: embedding-native retrieval real-binary E2E (bd-2vq2z.19).
# No-Cargo and no-download: uses EE_EMBED_MODEL_FIXTURE_DIR when a
# pre-provisioned model cache is available, otherwise asserts the explicit
# hash/lexical degradation path. Covers similar, remember-time dedupe,
# curation dedupe proposals, rerank posture, and eval precision metrics.
run_stage "Embedding Native E2E (bd-2vq2z.19)" "EE_E2E_TMPDIR=\"${E2E_TMPDIR_BASE}\" ./scripts/e2e_embedding_native.sh"

# Gate 6.1296: bundled embeddings regression E2E (bd-1et0v.19). No-Cargo:
# proves the analyst paraphrase regression, fresh semantic-ready posture,
# honest hash fallback degradation, eval semantic-recall gain, and the
# opt-in real-download lifecycle without downloading by default.
run_stage "Bundled Embeddings E2E (bd-1et0v.19)" "EE_E2E_TMPDIR=\"${E2E_TMPDIR_BASE}\" ./scripts/e2e_bundled_embeddings.sh"

# Gate 6.1297: pure-Rust native reranker real-binary E2E (bd-1nl13.14).
# Every profile proves dynamic ORT-free linkage plus honest missing/rejected-
# artifact degradation. Default and swarm-heavy verification additionally
# require the cached safetensors archive, a real rerank-induced order change,
# and registered-model withholding fallback. CI smoke is explicitly degradation-
# only so its cost and outcome never depend on an ambient host model cache.
# shellcheck disable=SC2329
native_reranker_full_e2e() {
    local archive_path
    archive_path="${EE_E2E_RERANK_MODEL_ARCHIVE:-${HOME}/.local/share/ee/models/rerank/rerank-default-v1/rerank-default-v1.tar.zst}"
    EE_E2E_RERANK_MODEL_ARCHIVE="${archive_path}" \
        EE_E2E_NATIVE_RERANK_REQUIRE_MODEL=1 \
        EE_E2E_NATIVE_RERANK_DEGRADATION_ONLY=0 \
        ./scripts/e2e_native_reranker.sh
}

# shellcheck disable=SC2329
native_reranker_degradation_e2e() {
    EE_E2E_NATIVE_RERANK_REQUIRE_MODEL=0 \
        EE_E2E_NATIVE_RERANK_DEGRADATION_ONLY=1 \
        ./scripts/e2e_native_reranker.sh
}

if [ "$CI_SMOKE" != "true" ]; then
    run_stage "Native Reranker E2E (bd-1nl13.14)" "native_reranker_full_e2e"
else
    run_stage "Native Reranker E2E (bd-1nl13.14)" "native_reranker_degradation_e2e"
fi

# Gate 6.13: Dueling Wizards docs-bootstrap real-binary E2E (bd-1n0np.11.5).
# Proves ee bootstrap docs --dry-run structural candidates (spans/hashes/anchors/
# specificity), determinism, guard-rail rejection (oversize/symlink/missing as
# structured degraded rows), and apply-through-curation refusing without
# --approved-only (no silent write).
run_stage "Dueling Wizards Docs Bootstrap E2E" "./scripts/e2e_docs_bootstrap.sh"

# Gate 6.14: Dueling Wizards attestation real-binary E2E (bd-1n0np.22.6).
# Proves ee attest query/memory emit a deterministic, redaction-safe ee.attest.v1
# bundle (blake3 bundleHash, rawTextIncluded false) with zero secret leakage; the
# support-bundle/handoff embedding hash-equality is capability-guarded until
# bd-1n0np.22.3 wires it.
run_stage "Dueling Wizards Attestation E2E" "./scripts/e2e_attestation.sh"

# Dueling Wizards Feedback-Gated E2E (bd-1n0np.13.5). Real-binary, no-Cargo:
# hard-asserts the cold-start calibration honesty invariant; condition-guards
# (log_drop) the feedback-gated reporting CLIs not yet wired (roi/calibration/
# regime). Validated PASS against the built binary.
run_stage "Dueling Wizards Feedback-Gated E2E" "./scripts/e2e_feedback_gated.sh"

# Dueling Wizards Trauma-Guard Learn E2E (bd-1n0np.18.4). Real-binary, no-Cargo;
# self-guards via log_drop for the not-yet-wired preflight-learn surfaces.
# Validated PASS against the built binary.
run_stage "Dueling Wizards Trauma-Guard Learn E2E" "./scripts/e2e_trauma_guard_learn.sh"

# Heavy gate block: skipped under --ci-smoke for fast swarm-CI / agent
# pre-push runs. bd-2dgn0.5: see docs/operator-swarm-slo.md for which
# gates are dropped and how to recover coverage in a follow-up
# --swarm-heavy run.
if [ "$CI_SMOKE" != "true" ]; then
    # Gate 6.1: Agent ergonomics F1-F5 e2e library driver. Missing future scripts
    # are reported as skips until their implementation beads land.
    run_stage "Agent Ergonomics E2E (F1-F5)" "./scripts/e2e_lib/run_agent_ergonomics_e2e.sh"

    # Gate 6.5: Overhaul Integration (J4). Gated behind VERIFY_OVERHAUL=1
    # until enough implementation beads ship to make the suite reliably
    # pass across CI. The driver itself respects VERIFY_OVERHAUL=0 and
    # exits 0 without running, so this stage stays fast in default CI.
    run_stage "Overhaul Integration E2E (J4)" "./scripts/e2e_overhaul.sh"

    # Gate 6.5.2: Lightweight swarm next-action recommendation-card contract.
    # This keeps SWA6's golden next-action overlap proof in the default gate
    # without requiring the heavier no-mock multi-agent harness.
    run_stage "Swarm Next-Action Recommendation Cards E2E (bd-3vwx0.6)" "./scripts/e2e_overhaul/swarm_next_action_recommendation_cards.sh"

    # Gate 6.5.3: the five swarm/ownership fixture suites (bd-udjrq). Each was
    # committed and invoked by nothing; each was executed for the FIRST time on
    # 2026-09-18 and passed on its own before being wired here. A suite that has
    # never run has never been right, so running them came before wiring them.
    #
    # One stage for five scripts, not five stages: the budget ceiling admits
    # measured seconds and the unmeasured allowance is exhausted at 27/27, so
    # five 0.4s scripts do not each deserve an entry.
    #
    # MEASURED, not unmeasured: 1.98 2.11 1.99 2.52 2.88 seconds over five runs
    # of the driver on a six-agent-loaded machine, p50 2.11. Declared 3 so the
    # hard-fail line lands at p50*3 = 9s, above the 2.88 worst observed.
    #
    # The driver collects EVERY failure and fails once with the full list,
    # rather than returning on the first -- these suites stayed broken for
    # years because nothing reported them, and a driver that stops at the first
    # failure hides the second in the same way.
    #
    # Their five orphan_baseline.txt rows are deleted in the same commit: the
    # audit fails on a stale baseline entry as well as on a new orphan.
    run_stage "Swarm Fixture Suite E2E (bd-udjrq)" "./scripts/e2e_swarm_fixture_suite.sh"

    # Gate 6.6: Graph determinism harness (F4.a). This is separate from the J4
    # epic registry because it tracks the GraphAccretion surfaces while they are
    # landing incrementally.
    run_stage "Graph Determinism E2E (F4.a)" "./scripts/e2e_overhaul/graph_determinism.sh"

    # Gate 6.7: Fake Tailscale harness (SRR6.46.10). Later SRR6.46 auto-enrollment
    # e2e scripts import this library, so this self-test runs before those surfaces.
    run_stage "Fake Tailscale Harness E2E (SRR6.46.10)" "./scripts/e2e_overhaul/lib/test_fake_tailscale.sh"

    # Gate 6.7a-c: Fake OIDC IdP harness (team-confed T7.7, bd-tc-epic-qzk7o.8.7).
    # The tier-2 SSO client acceptance beads import this harness, so its self-tests
    # run before those surfaces. All three are no-Cargo (python3 + openssl + curl only).
    run_stage "Fake OIDC IdP Harness E2E (T7.7)" "./scripts/e2e_overhaul/fake_idp_harness_smoke.sh"
    run_stage "Fake OIDC IdP Defects E2E (T7.7)" "./scripts/e2e_overhaul/fake_idp_defects_smoke.sh"
    run_stage "Fake OIDC IdP Matrix Self-Check E2E (T7.7)" "./scripts/e2e_overhaul/fake_idp_selfcheck.sh"

    # Gate 6.8: Local Tailscale probe status harness (SRR6.46.1). This keeps the
    # no-network status surface covered by the deterministic fake Tailscale CLI.
    run_stage "Tailscale Local Probe E2E (SRR6.46.1)" "./scripts/e2e_overhaul/tailscale_local_probe.sh"

    # Gate 6.9: Tailscale peer autodiscovery harness (SRR6.46.2). Uses fake
    # Tailscale peer metadata; the script itself never invokes cargo.
    run_stage "Tailscale Peer Autodiscovery E2E (SRR6.46.2)" "./scripts/e2e_overhaul/tailscale_peer_autodiscovery.sh"

    # Gate 6.10: Mesh hello protocol contract (SRR6.46.6). Static/no-Cargo gate
    # covering the bounded hello request/response/error schemas and fixtures.
    run_stage "Mesh Hello Handshake E2E (SRR6.46.6)" "./scripts/e2e_overhaul/mesh_hello_handshake.sh"

    # Gate 6.11: Mesh hello responder lifecycle contract (SRR6.46.12). Static
    # no-Cargo gate covering daemon/status wiring, degraded fixtures, and audit names.
    run_stage "Mesh Hello Responder Lifecycle E2E (SRR6.46.12)" "./scripts/e2e_overhaul/hello_responder_lifecycle.sh"

    # Gate 7: Advanced End-to-End
    run_stage "Advanced E2E Scripts" "./scripts/e2e_advanced.sh"

    # Gate 8: Boundary Migration
    run_stage "Boundary Migration Scripts" "./scripts/e2e_boundary_migration.sh"

    # Gate 8.5: ee doctor safety harness (bd-21joy)
    # Wraps verify-undo.sh, verify-idempotence.sh, verify-crash-recovery.sh,
    # verify-concurrency.sh, verify-metamorphic.sh against the per-FM
    # fixture suite under tests/doctor_fixtures/ (owned by bd-2oh15).
    # Advisory while fixtures or sub-scripts are missing; set
    # EE_SAFETY_HARNESS_STRICT=1 to fail closed.
    run_stage "ee doctor Safety Harness (bd-21joy)" "./scripts/run-safety-harness.sh"
else
    record_gated_off "Agent Ergonomics E2E (F1-F5)" "ci-smoke"
    record_gated_off "Overhaul Integration E2E (J4)" "ci-smoke"
    record_gated_off "Swarm Next-Action Recommendation Cards E2E (bd-3vwx0.6)" "ci-smoke"
    record_gated_off "Swarm Fixture Suite E2E (bd-udjrq)" "ci-smoke"
    record_gated_off "Graph Determinism E2E (F4.a)" "ci-smoke"
    record_gated_off "Fake Tailscale Harness E2E (SRR6.46.10)" "ci-smoke"
    record_gated_off "Fake OIDC IdP Harness E2E (T7.7)" "ci-smoke"
    record_gated_off "Fake OIDC IdP Defects E2E (T7.7)" "ci-smoke"
    record_gated_off "Fake OIDC IdP Matrix Self-Check E2E (T7.7)" "ci-smoke"
    record_gated_off "Tailscale Local Probe E2E (SRR6.46.1)" "ci-smoke"
    record_gated_off "Tailscale Peer Autodiscovery E2E (SRR6.46.2)" "ci-smoke"
    record_gated_off "Mesh Hello Handshake E2E (SRR6.46.6)" "ci-smoke"
    record_gated_off "Mesh Hello Responder Lifecycle E2E (SRR6.46.12)" "ci-smoke"
    record_gated_off "Advanced E2E Scripts" "ci-smoke"
    record_gated_off "Boundary Migration Scripts" "ci-smoke"
    record_gated_off "ee doctor Safety Harness (bd-21joy)" "ci-smoke"
fi

# Gate 8.75: Pack-quality eval threshold contract. This no-Cargo self-test
# proves the committed regression reports' deliberate top-1 miss trips the
# NDCG@10 threshold before the optional full eval sweep runs.
run_stage "Eval Regression Contract (bd-bife.18)" "./scripts/eval_regression.sh --self-test-misrank-top1"

# Gate 8.76: Ask answer-quality fixture contract. Runs the discoverable ask_v1
# fixture through the public eval surface so citation/abstention QA remains a
# committed verification step instead of a manual inspection.
# The binary is pinned to the current source build for the same reason the
# E2E stages pin EE_BIN/EE_BINARY (see 48b20809f): an unqualified ee resolves
# through PATH, so the gate would grade whatever build happens to be installed
# rather than the tree under verification. This stage invokes the binary
# directly instead of through a harness script, so the path is the pin;
# EE_BIN/EE_BINARY are not consulted on this call path.
run_stage "Ask Eval Quality Gate (bd-169v0.4)" "\"${CURRENT_SOURCE_EE_BINARY}\" eval run ask_v1 --json"

# CANDIDATE BINARY IDENTITY (bd-reality-core-convergence-1azkt.5, bullet 5,
# "hash"). EE_BINARY_SHA256 is computed once at :348 and, until this stage,
# NOTHING COMPARED IT -- it was printed into the log and into a proof capsule as
# the identity of "the binary that produced this verdict", with no check that it
# still was.
#
# It can stop being true mid-run. "Write Contention E2E" runs
# `cargo build --locked --bin ee`, so if that build replaces the file, the
# stages before it and the stages after it ran DIFFERENT binaries and the
# recorded digest describes only the first. That is the "wrong binary / stale
# source" case bullet 4 requires to make a run non-successful.
#
# PLACEMENT IS THE POINT: this must sit AFTER the last stage that can rebuild,
# or it compares the binary to itself and cannot fail. Keep it last among the
# non-optional stages.
#
# The comparison is a second OBSERVATION, not a declared constant. Nothing in
# the tree declares an expected ee digest and nothing can -- the binary is built
# per run -- so a pinned value would be fabricated provenance. Same binary, two
# points in time, is the only honest second value.
candidate_binary_identity_stable() {
    ee_assert_binary_identity_unchanged "${EE_BINARY}" "${EE_BINARY_SHA256:-}" "verify"
}
run_stage "Candidate Binary Identity Stable" "candidate_binary_identity_stable"
# The emitter is WIRED by scripts/rch_run.sh (3eb67caa6); this stage is what
# makes it GATED. Those are different things: wiring makes it run when someone
# uses that wrapper, this makes a broken emitter fail verification.
run_stage "Proof Capsule Emitter (1azkt.5 B9)" "python3 ./scripts/lib/emit_proof_capsule.py --self-test"

# Gate 8.8: Pack-quality eval regression sweep. Optional because it validates
# committed report artifacts and intended eval thresholds after feature slices.
if [ "$INCLUDE_EVAL" = "true" ]; then
    run_stage "Eval Regression (bd-bife.18)" "./scripts/eval_regression.sh"
else
    record_gated_off "Eval Regression (bd-bife.18)" "--include-eval not set"
fi

# Gate 9: Performance Benchmarks (optional, gated behind --include-bench)
if [ "$INCLUDE_BENCH" = "true" ]; then
    run_stage "Performance Benchmarks" "./scripts/bench_perf_regression.sh --check-regression"
else
    record_gated_off "Performance Benchmarks" "--include-bench not set"
fi

TOTAL_END=$(date +%s)
TOTAL_DURATION=$((TOTAL_END - TOTAL_START))

verification_summary_banner
echo ""
echo "Summary:"
printf "%b" "$STAGE_RESULTS"
echo ""
echo "Total time: ${TOTAL_DURATION}s"

print_artifact_index

echo ""
echo "Test tracing log paths:"
if [ -n "$TRACE_LOG_DIRS" ]; then
    printf "%b" "$TRACE_LOG_DIRS"
else
    echo "  none recorded"
fi

artifact_retention_summary

exit "$(verification_exit_status)"
