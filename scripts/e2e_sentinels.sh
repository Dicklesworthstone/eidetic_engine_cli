#!/usr/bin/env bash
# bd-1n0np.16.6 — Verifiable Memory Sentinels end-to-end (real binary).
#
# Full scenario (ADR 0060): temp workspace -> remember a fact carrying a
# `json_schema_contains_field` sentinel -> `ee sentinel check` PASSES -> mutate
# the target so the field is absent -> `ee sentinel check` FAILS, raising a
# revalidate curation candidate and downgrading the memory in a pack ->
# `ee pack --require-fresh-sentinels` excludes/flags it -> `ee why
# --include-sentinel` shows the last-verified status. Conservative: an
# unverifiable check is `unknown` (advisory), never `fail` (the SentinelObservation
# contract, ab62aa56). No arbitrary shell — pure predicates + an allowlist only.
#
# The sentinel SURFACE is landed only as data model + storage + the decision
# core today (MemorySentinelSpec/Result + db helpers + SentinelObservation,
# commits fe3cf207/b830e0a3/ab62aa56). The `remember --sentinel` flag, the pure-
# predicate checker (bd-1n0np.16.3), and the `ee sentinel check` / pack /
# why integration (bd-1n0np.16.4) are not wired yet, so those assertions are
# CAPABILITY-GUARDED: a missing surface records a visible `log_drop` (no-silent-
# cap) carrying the exact assertion that activates once it lands. The init /
# remember / fixture-mutation path runs for real.
#
# NOTE: no `set -e` — the harness assert_* helpers accumulate pass/fail and
# `harness_summary` decides the exit code, so a single failing assert must not
# abort the run before the summary is written.
set -uo pipefail

E2E_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/e2e_harness.sh
source "$E2E_DIR/lib/e2e_harness.sh"

harness_init "sentinels"

# ee_supports <subcommand words...> — true when `<words> --help` is accepted.
ee_supports() { "$EE_BIN" "$@" --help >/dev/null 2>&1; }

# ee_flag_supported <subcommand> <flag> — true when `<subcommand> --help` lists <flag>.
ee_flag_supported() { "$EE_BIN" "$1" --help 2>&1 | grep -- "$2" >/dev/null 2>&1; }

# ee_json <args...> — run ee, tolerate nonzero exit (assertions inspect output).
ee_json() { "$EE_BIN" "$@" 2>/dev/null || true; }

with_temp_workspace WS

step "seed a JSON schema fixture for the sentinel target"
mkdir -p "$WS/docs/schemas"
printf '{"title":"demo.v1","properties":{"memoryId":{"type":"string"}}}\n' \
    >"$WS/docs/schemas/demo.v1.json"

step "init workspace"
init_out="$(ee_json init --workspace "$WS" --json)"
assert_jq "$init_out" '.success == true' "ee init succeeds"

step "remember a contract-shaped fact (sentinel target: schema field memoryId)"
mem="$(ee_json remember \
    "The demo.v1 schema defines the memoryId field." \
    --workspace "$WS" --level procedural --kind fact --json)"
assert_jq "$mem" '.success == true' "remember succeeds"
mem_id="$(printf '%s' "$mem" | jq -r '.data.memory_id // empty')"
assert_eq "$([ -n "$mem_id" ] && echo present || echo missing)" "present" \
    "memory id present"

if ee_supports remember && ee_flag_supported remember "--sentinel"; then
    step "attach a json_schema_contains_field sentinel via remember --sentinel"
    s="$(ee_json remember "demo.v1 must keep memoryId." --workspace "$WS" \
        --level procedural --kind fact \
        --sentinel "json_schema_contains_field:docs/schemas/demo.v1.json#memoryId" --json)"
    assert_jq "$s" '.success == true' "remember --sentinel succeeds"
else
    log_drop 1 "remember --sentinel flag absent (bd-1n0np.16.2 CLI wiring pending): when wired, assert the spec persists with sentinel_kind=json_schema_contains_field + the stable spec hash"
fi

if ee_supports sentinel check; then
    step "ee sentinel check passes while the field is present"
    pass="$(ee_json sentinel check --workspace "$WS" --json)"
    assert_jq "$pass" 'any(.data.results[]?; .status == "pass")' \
        "sentinel check passes while memoryId present"

    step "remove the schema field, then ee sentinel check FAILS (never mutates)"
    printf '{"title":"demo.v1","properties":{}}\n' >"$WS/docs/schemas/demo.v1.json"
    fail="$(ee_json sentinel check --workspace "$WS" --json)"
    assert_jq "$fail" 'any(.data.results[]?; .status == "fail")' \
        "sentinel check fails once memoryId is absent"
    assert_jq "$fail" 'all(.data.results[]?; .status != "removed")' \
        "no sentinel result implies memory removal (ee never mutates)"
else
    log_drop 1 "ee sentinel check absent (bd-1n0np.16.3/16.4 pending): when wired, assert pass while the schema field is present, then fail after removing it, with a 'revalidate' curation candidate raised and the memory NEVER removed"
    log_drop 1 "conservatism not observable: when wired, assert an unverifiable check (e.g. unreadable target) yields status=unknown (advisory), NEVER fail"
fi

if ee_flag_supported pack "--require-fresh-sentinels"; then
    step "ee pack --require-fresh-sentinels flags the failing sentinel"
    pk="$(ee_json pack "demo schema" --workspace "$WS" --require-fresh-sentinels --json)"
    assert_jq "$pk" '.success == true' "pack --require-fresh-sentinels runs"
else
    log_drop 1 "pack --require-fresh-sentinels absent (bd-1n0np.16.4): when wired, assert a memory with a failing sentinel is excluded/flagged in the pack"
fi

log_drop 1 "why --include-sentinel absent (bd-1n0np.16.4): when wired, assert ee why --include-sentinel shows the memory's last-verified sentinel status"

harness_summary
