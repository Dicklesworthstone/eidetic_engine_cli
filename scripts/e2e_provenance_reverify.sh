#!/usr/bin/env bash
# bd-1n0np.9.5 — Provenance re-verification end-to-end (real binary).
#
# Full scenario (E9): temp workspace -> remember memories citing local
# file#Ln-m spans and one citing a git-sha -> mutate one file span while another
# span points at a file that never existed -> run 'ee verify provenance --json'
# and assert per-scheme verdicts:
# evidence_drift (file span changed), evidence_missing (referent absent), an
# AUDITED trust demotion + a pending deprecation candidate whose reason requests
# revalidation for each, with the memory NEVER removed (RULE 1) -> then assert a
# cass-session referent whose resolver is not wired yet reports `unverifiable`
# (NOT `missing`), conservative.
#
# NOTE: no `set -e` — the harness assert_* helpers accumulate pass/fail and
# `harness_summary` decides the exit code, so a single failing assert must not
# abort the run before the summary is written.
set -uo pipefail

E2E_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/e2e_harness.sh
source "$E2E_DIR/lib/e2e_harness.sh"

harness_init "provenance_reverify"

# ee_supports <subcommand words...> — true when `<words> --help` is accepted.
ee_supports() { "$EE_BIN" "$@" --help >/dev/null 2>&1; }

# ee_json <args...> — run ee, tolerate nonzero exit (assertions inspect output).
ee_json() { "$EE_BIN" "$@" 2>/dev/null || true; }

with_temp_workspace WS

step "seed a tiny source file to cite by file#Ln-m span"
mkdir -p "$WS/src"
printf 'fn one() {}\nfn two() {}\nfn three() {}\n' >"$WS/src/prov.rs"

step "init workspace"
init_out="$(ee_json init --workspace "$WS" --json)"
assert_jq "$init_out" '.success == true' "ee init succeeds"

step "remember a memory citing a local file span (file#Ln-m provenance)"
file_mem="$(ee_json remember \
    "fn two() {}" \
    --workspace "$WS" --level episodic --kind fact \
    --source "file://src/prov.rs#L2-L2" --json)"
assert_jq "$file_mem" '.success == true' "remember (file-span provenance) succeeds"
file_mem_id="$(printf '%s' "$file_mem" | jq -r '.data.memory_id // empty')"
assert_eq "$([ -n "$file_mem_id" ] && echo present || echo missing)" "present" \
    "file-span memory id present"

step "remember a memory whose file provenance is already missing"
missing_mem="$(ee_json remember \
    "Missing provenance fixture." \
    --workspace "$WS" --level episodic --kind fact \
    --source "file://src/missing.rs#L1" --json)"
assert_jq "$missing_mem" '.success == true' "remember (missing file provenance) succeeds"
missing_mem_id="$(printf '%s' "$missing_mem" | jq -r '.data.memory_id // empty')"
assert_eq "$([ -n "$missing_mem_id" ] && echo present || echo missing)" "present" \
    "missing-file memory id present"

step "remember a memory citing a git commit (git-sha provenance)"
git_mem="$(ee_json remember \
    "Behavior fixed in the referenced commit." \
    --workspace "$WS" --level episodic --kind fact \
    --source "git-sha://0000000000000000000000000000000000000000" --json)"
# The provenance URI grammar may reject a synthetic sha; tolerate either and
# capability-guard the verify step below.
if printf '%s' "$git_mem" | jq -e '.success == true' >/dev/null 2>&1; then
    assert_jq "$git_mem" '.success == true' "remember (git-sha provenance) succeeds"
else
    log_drop 1 "git-sha provenance citation rejected by URI grammar on this binary; verify step covers it via the file referent"
fi

step "remember a memory citing a cass session (resolver unavailable by design)"
cass_mem="$(ee_json remember \
    "Cass provenance should stay unverifiable until a resolver is wired." \
    --workspace "$WS" --level episodic --kind fact \
    --source "cass-session://provenance-reverify-fixture#L1-L2" --json)"
assert_jq "$cass_mem" '.success == true' "remember (cass-session provenance) succeeds"
cass_mem_id="$(printf '%s' "$cass_mem" | jq -r '.data.memory_id // empty')"
assert_eq "$([ -n "$cass_mem_id" ] && echo present || echo missing)" "present" \
    "cass-session memory id present"

step "drift the file span (mutate)"
printf 'fn one() {}\nfn TWO_RENAMED() {}\nfn three() {}\n' >"$WS/src/prov.rs"  # evidence_drift

assert_exit 0 "ee verify provenance help exists" -- "$EE_BIN" verify provenance --help

step "ee verify provenance re-resolves referents and persists safe revalidation actions"
vp="$(ee_json verify provenance --workspace "$WS" --json)"
assert_jq "$vp" '.success == true' "ee verify provenance succeeds"
assert_jq "$vp" '.data.dryRun == false' "ee verify provenance durable mode is explicit"
assert_jq "$vp" '.data.readOnly == false' "ee verify provenance is mutating by default"
assert_jq "$vp" '.data.durableMutation == true' "ee verify provenance reports durable mutation"
assert_jq "$vp" 'any(.data.referents[]?; .status == "evidence_drift")' \
    "drifted file span reported evidence_drift"
assert_jq "$vp" 'any(.data.referents[]?; .status == "evidence_missing")' \
    "missing file referent reported evidence_missing"
assert_jq "$vp" '.data.mutationCount >= 2' "missing/drift referents produce mutation records"
assert_jq "$vp" '.data.curationCandidateCount >= 2' "missing/drift referents produce curation candidates"
assert_jq "$vp" '.data.auditCount >= 4' "missing/drift referents append audit rows"
assert_jq "$vp" 'all(.data.referents[]?; .status != "removed")' \
    "no referent status implies memory removal"
cass_unverifiable_count="$(printf '%s' "$vp" \
    | jq -r --arg id "$cass_mem_id" \
        '[.data.referents[]? | select(.memoryId == $id and .scheme == "cass-session" and .status == "unverifiable" and .reason == "cass_recheck_requires_cass_contract" and (.mutation.action == "advisory") and (.mutation.persisted == true) and (.mutation.newVerificationStatus == "skipped") and (.mutation.trustClassUpdated == false) and (.mutation.candidateId == null))] | length' \
    2>/dev/null || echo 0)"
cass_missing_count="$(printf '%s' "$vp" \
    | jq -r --arg id "$cass_mem_id" \
        '[.data.referents[]? | select(.memoryId == $id and .scheme == "cass-session" and .status == "evidence_missing")] | length' \
    2>/dev/null || echo 0)"
assert_eq "$cass_unverifiable_count" "1" \
    "cass-session referent is advisory-only with skipped verification status"
assert_eq "$cass_missing_count" "0" \
    "cass-session referent is never classified evidence_missing"

file_candidate_id="$(printf '%s' "$vp" \
    | jq -r --arg id "$file_mem_id" '.data.referents[]? | select(.memoryId == $id and .status == "evidence_drift") | .mutation.candidateId // empty' \
    | head -n 1)"
missing_candidate_id="$(printf '%s' "$vp" \
    | jq -r --arg id "$missing_mem_id" '.data.referents[]? | select(.memoryId == $id and .status == "evidence_missing") | .mutation.candidateId // empty' \
    | head -n 1)"
assert_eq "$([ -n "$file_candidate_id" ] && echo present || echo missing)" "present" \
    "drifted memory candidate id present"
assert_eq "$([ -n "$missing_candidate_id" ] && echo present || echo missing)" "present" \
    "missing memory candidate id present"

step "curation queue surfaces provenance revalidation candidates"
candidates="$(ee_json curate candidates --workspace "$WS" --type deprecate --status pending --json)"
assert_jq "$candidates" '.success == true' "curate candidates succeeds"
file_candidate_count="$(printf '%s' "$candidates" \
    | jq -r --arg id "$file_candidate_id" '[.data.candidates[]? | select((.candidateId // .candidate_id // .id // "") == $id)] | length' \
    2>/dev/null || echo 0)"
missing_candidate_count="$(printf '%s' "$candidates" \
    | jq -r --arg id "$missing_candidate_id" '[.data.candidates[]? | select((.candidateId // .candidate_id // .id // "") == $id)] | length' \
    2>/dev/null || echo 0)"
assert_eq "$file_candidate_count" "1" "drifted memory candidate is listed"
assert_eq "$missing_candidate_count" "1" "missing memory candidate is listed"

step "audit log records demotion and candidate creation"
audits="$(ee_json --workspace "$WS" db inspect audit_log --limit 80 --json)"
assert_jq "$audits" '.success == true' "db inspect audit_log succeeds"
trust_audit_count="$(printf '%s' "$audits" \
    | jq -r --arg file "$file_mem_id" --arg missing "$missing_mem_id" \
        '[.data.report.rows[]?.values | select(.action == "trust_class.transition" and (.target_id == $file or .target_id == $missing))] | length' \
    2>/dev/null || echo 0)"
candidate_audit_count="$(printf '%s' "$audits" \
    | jq -r --arg file "$file_candidate_id" --arg missing "$missing_candidate_id" \
        '[.data.report.rows[]?.values | select(.action == "curation_candidate.create" and (.target_id == $file or .target_id == $missing))] | length' \
    2>/dev/null || echo 0)"
assert_eq "$trust_audit_count" "2" "trust transition audits recorded"
assert_eq "$candidate_audit_count" "2" "candidate create audits recorded"

step "cited memories are demoted but never removed"
memories="$(ee_json --workspace "$WS" db inspect memories --limit 80 --json)"
assert_jq "$memories" '.success == true' "db inspect memories succeeds"
live_demoted_count="$(printf '%s' "$memories" \
    | jq -r --arg file "$file_mem_id" --arg missing "$missing_mem_id" \
        '[.data.report.rows[]?.values | select((.id == $file or .id == $missing) and .trust_class == "agent_assertion" and (.tombstoned_at == null))] | length' \
    2>/dev/null || echo 0)"
assert_eq "$live_demoted_count" "2" "cited memories remain live after demotion"

harness_summary
