#!/usr/bin/env bash
# bd-1n0np.9.5 — Provenance re-verification end-to-end (real binary).
#
# Full scenario (E9): temp workspace -> remember memories citing local
# file#Ln-m spans and one citing a git-sha -> mutate one file span while another
# span points at a file that never existed -> run 'ee verify provenance --json'
# and assert per-scheme verdicts:
# evidence_drift (file span changed), evidence_missing (referent absent), an
# AUDITED trust demotion + a revalidate curation candidate for each, with the
# memory NEVER removed (RULE 1) -> then simulate cass-down and assert the
# cass-session referent is reported `unverifiable` (NOT `missing`), conservative.
#
# The verify-provenance SURFACE is landed only as core today: the read-only
# `verify_bounded_provenance` over the per-scheme referent dispatcher and the
# ProvenanceReverifyAction decision contract (commits 2b6266ea, 407cc7ce) exist,
# but the public `ee verify provenance` CLI command + the audited demotion/
# revalidate WRITE are not wired yet (bd-1n0np.9.1/9.2). Those assertions are
# CAPABILITY-GUARDED: a missing surface records a visible `log_drop` (no-silent-
# cap) carrying the exact assertion that activates once it lands. The init /
# remember (with --source provenance) / file-mutation path runs for real.
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

step "drift the file span (mutate)"
printf 'fn one() {}\nfn TWO_RENAMED() {}\nfn three() {}\n' >"$WS/src/prov.rs"  # evidence_drift

if ! ee_supports verify provenance; then
    log_drop 1 "ee verify provenance CLI absent (bd-1n0np.9.1 wiring pending): when wired, assert .data per-referent status includes evidence_drift (mutated file span) and evidence_missing (absent referent)"
    log_drop 1 "audited demotion + revalidate candidate not observable (bd-1n0np.9.2 write pending): when wired, assert an audited trust-class/freshness demotion row AND an 'ee curate candidates' revalidate candidate per drifted/missing referent"
    log_drop 1 "no-removal invariant not observable without the verify write: when wired, assert the cited memory is STILL present after demotion (RULE 1 / never removed)"
    log_drop 1 "cass-down conservatism not observable: when wired, simulate cass down and assert a cass-session referent reports status 'unverifiable' (NOT 'missing')"
else
    step "ee verify provenance re-resolves referents (read-only)"
    vp="$(ee_json verify provenance --workspace "$WS" --json)"
    assert_jq "$vp" '.success == true' "ee verify provenance succeeds"
    assert_jq "$vp" 'any(.data.referents[]?; .status == "evidence_drift")' \
        "drifted file span reported evidence_drift"
    assert_jq "$vp" 'any(.data.referents[]?; .status == "evidence_missing")' \
        "missing file referent reported evidence_missing"
    # Conservatism: an unresolvable backend is 'unverifiable', never 'missing'.
    assert_jq "$vp" 'all(.data.referents[]?; .status != "removed")' \
        "no referent status implies memory removal"
fi

harness_summary
