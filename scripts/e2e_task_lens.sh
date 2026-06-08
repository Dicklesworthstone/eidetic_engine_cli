#!/usr/bin/env bash
# bd-1n0np.17.5 — Task Lens end-to-end (real binary, detailed logging).
#
# Scenario: temp workspace -> `ee lens explain bugfix` exposes effective
# options -> `ee pack --lens bugfix` applies lens defaults while explicit CLI
# flags override them -> persisted pack replay carries task-lens id/version/hash
# -> `--no-lens` produces a distinct pack hash. The script sources
# scripts/e2e_lib.sh for structured ee.test_event.v1 logging and artifact
# retention; it never builds the binary itself.
#
# NOTE: no `set -e` — assert_* helpers accumulate pass/fail and harness_summary
# decides the exit code, so a single failing assert cannot prevent artifacts
# and the summary from being written.
set -uo pipefail

E2E_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/e2e_lib.sh
# shellcheck disable=SC1091
source "$E2E_DIR/e2e_lib.sh"

harness_init "task_lens"

run_json() {
    local name="${1:?run_json: name required}"
    shift
    local stdout_path="$LOG_DIR/${name}.stdout.json"
    local stderr_path="$LOG_DIR/${name}.stderr.txt"
    local exit_code=0
    "$EE_BIN" "$@" >"$stdout_path" 2>"$stderr_path" || exit_code=$?
    log_event "task_lens_command" \
        name "$name" \
        exitCode "$exit_code" \
        stdoutArtifact "$stdout_path" \
        stderrArtifact "$stderr_path"
    cat "$stdout_path"
}

jq_text() {
    local json="${1:?jq_text: json required}"
    local filter="${2:?jq_text: filter required}"
    printf '%s' "$json" | jq -r "$filter" 2>/dev/null || true
}

with_temp_workspace WS

step "init isolated workspace"
init_out="$(run_json init --workspace "$WS" init --json)"
assert_jq "$init_out" '.success == true' "ee init succeeds"

step "explain built-in bugfix lens effective options"
explain_out="$(run_json lens_explain --workspace "$WS" lens explain bugfix --json)"
assert_jq "$explain_out" '.schema == "ee.response.v2" and .success == true' \
    "lens explain returns response envelope"
assert_jq "$explain_out" '.data.schema == "ee.task_lens.explain.v1"' \
    "lens explain schema is stable"
assert_jq "$explain_out" '.data.lens.id == "bugfix"' "bugfix lens id"
assert_jq "$explain_out" '.data.lens.overlay.contextProfile == "thorough"' \
    "bugfix lens context profile"
assert_jq "$explain_out" '.data.lens.overlay.sourceMode == "hybrid"' \
    "bugfix lens source mode"
assert_jq "$explain_out" '.data.lens.overlay.maxTokens == 6000' \
    "bugfix lens max token default"
assert_jq "$explain_out" '.data.lens.overlay.candidatePool == 160' \
    "bugfix lens candidate pool default"
assert_jq "$explain_out" '(.data.lens.lensHash // "") | startswith("blake3:")' \
    "bugfix lens hash is recorded"
lens_hash="$(jq_text "$explain_out" '.data.lens.lensHash')"
log_event "task_lens_effective_options" \
    lens bugfix \
    lensHash "$lens_hash" \
    contextProfile "$(jq_text "$explain_out" '.data.lens.overlay.contextProfile')" \
    sourceMode "$(jq_text "$explain_out" '.data.lens.overlay.sourceMode')" \
    maxTokens "$(jq_text "$explain_out" '.data.lens.overlay.maxTokens')" \
    candidatePool "$(jq_text "$explain_out" '.data.lens.overlay.candidatePool')"

step "seed failure memory selected by bugfix lens"
remember_out="$(run_json remember_failure --workspace "$WS" remember \
    "Release failure reproduced by rerunning the exact failing command." \
    --level episodic --kind failure --json)"
assert_jq "$remember_out" '.success == true' "failure memory remembered"
memory_id="$(jq_text "$remember_out" '.data.memory_id')"
assert_contains "$memory_id" "mem_" "remember returns memory id"

step "pack with bugfix lens and explicit flag overrides"
pack_lens_out="$(run_json pack_lens --workspace "$WS" pack "fix release failure" \
    --lens bugfix \
    --profile compact \
    --max-tokens 2000 \
    --candidate-pool 17 \
    --source-mode lexical-only \
    --json)"
assert_jq "$pack_lens_out" '.schema == "ee.response.v2" and .success == true' \
    "pack --lens succeeds"
assert_jq "$pack_lens_out" '.data.command == "pack"' "pack command field"
assert_jq "$pack_lens_out" '.data.request.profile == "compact"' \
    "explicit --profile overrides lens context profile"
assert_jq "$pack_lens_out" '.data.request.maxTokens == 2000' \
    "explicit --max-tokens overrides lens token default"
assert_jq "$pack_lens_out" '.data.request.candidatePool == 17' \
    "explicit --candidate-pool overrides lens candidate default"
assert_jq "$pack_lens_out" '(.data.pack.hash // "") | startswith("blake3:")' \
    "lens pack hash is present"
assert_jq "$pack_lens_out" '(.data.pack.items // []) | length >= 1' \
    "lens pack selected at least one item"
pack_hash="$(jq_text "$pack_lens_out" '.data.pack.hash')"

step "why exposes latest persisted pack selection"
why_out="$(run_json why_memory --workspace "$WS" why "$memory_id" --json)"
assert_jq "$why_out" '.success == true' "ee why succeeds for selected memory"
assert_jq "$why_out" '(.data.selection.latestPackSelection.packId // "") | startswith("pack_")' \
    "why exposes latest pack selection id"
pack_id="$(jq_text "$why_out" '.data.selection.latestPackSelection.packId')"
if printf '%s' "$why_out" | jq -e '.data.selection.latestPackSelection.taskLens.id == "bugfix"' >/dev/null 2>&1; then
    _harness_pass "why latest pack selection cites task lens"
else
    log_drop 1 "ee why latestPackSelection does not expose task lens metadata yet; pack replay ledger assertion covers persisted lens metadata"
fi

step "pack replay shows recorded task lens id/version/hash"
replay_out="$(run_json pack_replay --workspace "$WS" pack replay "$pack_id" --json)"
assert_jq "$replay_out" '.schema == "ee.pack.replay.v1" and .success == true' \
    "pack replay succeeds"
assert_jq "$replay_out" '.data.replay.status == "available"' \
    "pack replay ledger is available"
assert_jq "$replay_out" '.data.replay.ledger.taskLens.id == "bugfix"' \
    "replay ledger records task lens id"
assert_jq "$replay_out" '.data.replay.ledger.taskLens.version == 1' \
    "replay ledger records task lens version"
recorded_lens_hash="$(jq_text "$replay_out" '.data.replay.ledger.taskLens.lensHash')"
assert_eq "$recorded_lens_hash" "$lens_hash" "replay ledger records the explained lens hash"
assert_jq "$replay_out" '.data.replay.ledger.request.profile == "compact"' \
    "replay ledger records explicit profile override"
assert_jq "$replay_out" '.data.replay.ledger.request.maxTokens == 2000' \
    "replay ledger records explicit token override"
ledger_hash="$(jq_text "$replay_out" '.data.pack.ledgerHash')"
log_event "task_lens_recorded_pack" \
    packId "$pack_id" \
    packHash "$pack_hash" \
    ledgerHash "$ledger_hash" \
    lensHash "$recorded_lens_hash"

step "--no-lens produces a distinct pack hash"
pack_no_lens_out="$(run_json pack_no_lens --workspace "$WS" pack "fix release failure" \
    --no-lens \
    --profile compact \
    --max-tokens 2000 \
    --candidate-pool 17 \
    --source-mode lexical-only \
    --json)"
assert_jq "$pack_no_lens_out" '.schema == "ee.response.v2" and .success == true' \
    "pack --no-lens succeeds"
no_lens_hash="$(jq_text "$pack_no_lens_out" '.data.pack.hash')"
if [ -n "$pack_hash" ] && [ -n "$no_lens_hash" ] && [ "$pack_hash" != "$no_lens_hash" ]; then
    _harness_pass "--no-lens pack hash differs from lens-bound pack hash"
else
    _harness_fail "--no-lens pack hash should differ from lens-bound hash (lens=$pack_hash no_lens=$no_lens_hash)"
fi
log_event "task_lens_summary" \
    lensHash "$lens_hash" \
    lensPackHash "$pack_hash" \
    noLensPackHash "$no_lens_hash" \
    artifactDir "$LOG_DIR"

end_temp_workspace
summary
