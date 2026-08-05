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
# NOTE: no `set -e` — the harness assert_* helpers accumulate pass/fail and
# `harness_summary` decides the exit code, so a single failing assert must not
# abort the run before the summary is written.
set -uo pipefail

E2E_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/e2e_harness.sh
source "$E2E_DIR/lib/e2e_harness.sh"

harness_init "sentinels"

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

step "reject unknown sentinel kinds before memory mutation"
before_unknown="$(ee_json memory list --workspace "$WS" --json)"
assert_jq "$before_unknown" '.success == true and ((.data.memories // []) | type == "array")' \
    "baseline memory list succeeds"
before_unknown_count="$(printf '%s' "$before_unknown" | jq -r '(.data.memories // []) | length')"
unknown_kind="$(ee_json remember "unknown sentinel kind must not persist" \
    --workspace "$WS" --level procedural --kind fact \
    --sentinel "definitely_unknown:target" --json)"
assert_jq "$unknown_kind" \
    '.schema == "ee.error.v2"
     and .error.code == "usage"
     and (.error.message | contains("unknown memory sentinel kind"))
     and (.error.message | contains("path_exists"))
     and (.error.repair | contains("ee sentinel explain"))' \
    "unknown sentinel kind returns a repairable usage error"
after_unknown="$(ee_json memory list --workspace "$WS" --json)"
assert_jq "$after_unknown" '.success == true and ((.data.memories // []) | type == "array")' \
    "post-rejection memory list succeeds"
after_unknown_count="$(printf '%s' "$after_unknown" | jq -r '(.data.memories // []) | length')"
assert_eq "$after_unknown_count" "$before_unknown_count" \
    "unknown sentinel kind is rejected before memory mutation"

step "remember a sentinel-backed contract-shaped fact"
mem="$(ee_json remember \
    "The demo.v1 schema defines the memoryId field." \
    --workspace "$WS" --level procedural --kind fact \
    --sentinel "json_schema_contains_field:docs/schemas/demo.v1.json#memoryId" \
    --json)"
assert_jq "$mem" '.success == true' "remember succeeds"
mem_id="$(printf '%s' "$mem" | jq -r '.data.memory_id // empty')"
assert_eq "$([ -n "$mem_id" ] && echo present || echo missing)" "present" \
    "memory id present"

step "reject arbitrary-shell sentinel targets before memory mutation"
bad="$(ee_json remember "bad sentinel target" --workspace "$WS" --level procedural \
    --kind fact --sentinel "command_help_contains_flag:sh -c --version;echo bad" --json)"
assert_jq "$bad" '((.success == false) or (.schema == "ee.error.v2")) and (.error.message | test("sentinel|target|shell|ee"))' \
    "arbitrary-shell sentinel target rejected"

step "allowlisted ee help introspection sentinel passes"
help_mem="$(ee_json remember "pack exposes the fresh-sentinel gate." \
    --workspace "$WS" --level procedural --kind fact \
    --sentinel "command_help_contains_flag:ee pack --require-fresh-sentinels" --json)"
assert_jq "$help_mem" '.success == true' "remember command-help sentinel succeeds"
help_mem_id="$(printf '%s' "$help_mem" | jq -r '.data.memory_id // empty')"
assert_eq "$([ -n "$help_mem_id" ] && echo present || echo missing)" "present" \
    "command-help memory id present"
help_pass="$(ee_json sentinel check "$help_mem_id" --workspace "$WS" --json)"
assert_jq "$help_pass" 'any(.data.results[]?; .status == "pass")' \
    "allowlisted command-help sentinel passes"

step "ee sentinel check passes while the field is present"
pass="$(ee_json sentinel check "$mem_id" --workspace "$WS" --json)"
assert_jq "$pass" 'any(.data.results[]?; .status == "pass" and (.resultHash | startswith("blake3:")))' \
    "sentinel check passes with result hash while memoryId present"

step "remove the schema field, then ee sentinel check FAILS without memory mutation"
printf '{"title":"demo.v1","properties":{}}\n' >"$WS/docs/schemas/demo.v1.json"
fail="$(ee_json sentinel check "$mem_id" --workspace "$WS" --json)"
assert_jq "$fail" 'any(.data.results[]?; .status == "fail" and (.resultHash | startswith("blake3:")))' \
    "sentinel check fails once memoryId is absent"
assert_jq "$fail" 'all(.data.results[]?; .status != "removed")' \
    "no sentinel result implies memory removal (ee never mutates)"

step "ee pack --require-fresh-sentinels omits the failing sentinel-backed memory"
pk="$(ee_json pack "demo schema memoryId" --workspace "$WS" --require-fresh-sentinels --json)"
assert_jq "$pk" '.success == true' "pack --require-fresh-sentinels runs"
SENTINEL_ID="$mem_id" assert_jq "$pk" \
    'any(.data.pack.omitted[]?; ((.memoryId // .memory_id) == env.SENTINEL_ID) and ((.reason // "") == "excluded_by_policy"))' \
    "failing sentinel-backed memory is persisted as a pack omission"

step "ee why --include-sentinel shows the latest failed sentinel result"
why="$(ee_json why "$mem_id" --workspace "$WS" --include-sentinel --json)"
assert_jq "$why" '.success == true' "why --include-sentinel runs"
assert_jq "$why" \
    '.data.sentinel.summary.failCount >= 1 and any(.data.sentinel.specs[]?; .latestResult.status == "fail")' \
    "why reports latest failed sentinel status"


harness_summary
