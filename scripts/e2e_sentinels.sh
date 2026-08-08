#!/usr/bin/env bash
# bd-1n0np.16.6 — Verifiable Memory Sentinels end-to-end (real binary).
#
# Full scenario (ADR 0060): temp workspace -> remember facts carrying Gate and
# Revive sentinels -> prove malformed revival input cannot mutate database bytes,
# memory rows, or idempotency state -> evaluate ready Revive specs through the
# literal read-only tripwire surface while excluding Gate specs -> carry a
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
# Stderr remains visible so a real binary failure cannot be hidden.
ee_json() { "$EE_BIN" "$@" || true; }

db_state_hash() {
    {
        for path in "$WS/.ee/ee.db" "$WS/.ee/ee.db-wal"; do
            if [[ -f "$path" ]]; then
                shasum -a 256 "$path"
            else
                printf 'missing %s\n' "$path"
            fi
        done
    } | shasum -a 256 | awk '{print $1}'
}

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

step "reject malformed revival input before database rows, bytes, or idempotency mutate"
before_revival_count="$after_unknown_count"
before_revival_hash="$(db_state_hash)"
bad_revival="$(ee_json remember "revival validation must be atomic" \
    --workspace "$WS" --level episodic --kind failure \
    --idempotency-key sentinel-revival-prevalidation \
    --revive-when "definitely_unknown:target" --json)"
assert_jq "$bad_revival" \
    '.schema == "ee.error.v2"
     and .error.code == "usage"
     and (.error.message | contains("unknown memory sentinel kind"))' \
    "malformed --revive-when returns a usage error"
after_bad_revival_hash="$(db_state_hash)"
assert_eq "$after_bad_revival_hash" "$before_revival_hash" \
    "malformed --revive-when leaves database and WAL bytes unchanged"
after_bad_revival="$(ee_json memory list --workspace "$WS" --json)"
after_bad_revival_count="$(printf '%s' "$after_bad_revival" | jq -r '(.data.memories // []) | length')"
assert_eq "$after_bad_revival_count" "$before_revival_count" \
    "malformed --revive-when leaves memory row count unchanged"

step "valid revival retry with the same idempotency key persists normally"
revive_mem="$(ee_json remember "Retry this route after the marker appears." \
    --workspace "$WS" --level episodic --kind failure \
    --idempotency-key sentinel-revival-prevalidation \
    --revive-when "path_exists:revival-ready.marker" --json)"
assert_jq "$revive_mem" '.success == true and .data.persisted == true' \
    "malformed revival did not consume the idempotency key"
revive_mem_id="$(printf '%s' "$revive_mem" | jq -r '.data.memory_id // empty')"
assert_eq "$([ -n "$revive_mem_id" ] && echo present || echo missing)" "present" \
    "revival memory id present"

blocked_revival="$(ee_json tripwire check --revivals --workspace "$WS" --json)"
assert_jq "$blocked_revival" \
    '.success == true
     and .data.schema == "ee.memory_sentinel.revivals.v1"
     and .data.revivalCount == 0
     and .data.summary.fail >= 1
     and .data.mutationPosture == "read_only_no_result_trust_or_tombstone_mutation"' \
    "revival surface evaluates a blocked Revive spec without reporting it ready"

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
assert_jq "$help_pass" \
    '.data.schema == "ee.memory_sentinel.check.v2"
     and .data.byPolarity.gate.pass >= 1
     and .data.byPolarity.revive.specCount == 0
     and any(.data.results[]?; .status == "pass" and .polarity == "gate")' \
    "allowlisted command-help sentinel passes"

step "explicit revival checks may introspect command help while implicit orient stays process-free"
help_revive_mem="$(ee_json remember "Retry this route when pack advertises fresh sentinels." \
    --workspace "$WS" --level episodic --kind failure \
    --revive-when "command_help_contains_flag:ee pack --require-fresh-sentinels" --json)"
assert_jq "$help_revive_mem" '.success == true and .data.persisted == true' \
    "command-help revival memory persists"
help_revive_id="$(printf '%s' "$help_revive_mem" | jq -r '.data.memory_id // empty')"

env_revive_mem="$(ee_json remember "Retry this route when the output governor env key is registered." \
    --workspace "$WS" --level episodic --kind failure \
    --revive-when "env_var_registered:EE_MAX_OUTPUT_TOKENS" --json)"
assert_jq "$env_revive_mem" '.success == true and .data.persisted == true' \
    "env-registry revival memory persists"
env_revive_id="$(printf '%s' "$env_revive_mem" | jq -r '.data.memory_id // empty')"

explicit_revivals="$(ee_json tripwire check --revivals --limit 100 --workspace "$WS" --json)"
HELP_REVIVE_ID="$help_revive_id" ENV_REVIVE_ID="$env_revive_id" assert_jq "$explicit_revivals" \
    '.data.observationMode == "explicit"
     and .data.evaluationPosture == "local_read_only_predicates_plus_allowlisted_command_help_process"
     and .data.commandHelpProcessExecution == true
     and any(.data.revivals[]?; .memoryId == env.HELP_REVIVE_ID and .kind == "command_help_contains_flag")
     and any(.data.revivals[]?; .memoryId == env.ENV_REVIVE_ID and .kind == "env_var_registered")' \
    "explicit revival check evaluates command-help and safe env predicates"

limited_revivals="$(ee_json tripwire check --revivals --limit 1 --workspace "$WS" --json)"
assert_jq "$limited_revivals" \
    '.data.limit == 1
     and .data.matchedSpecCount >= 3
     and .data.evaluatedSpecCount == 1
     and .data.unevaluatedSpecCount == (.data.matchedSpecCount - 1)
     and .data.truncatedByLimit == true
     and (.data.limitRepair | startswith("ee tripwire check --revivals --limit "))
     and (.data.revivals | length) <= 1' \
    "revival provider limit is deterministic, bounded, and explicit"
bad_limit="$(ee_json tripwire check --revivals --limit 101 --workspace "$WS" --json)"
assert_jq "$bad_limit" \
    '.schema == "ee.error.v2" and .error.code == "usage"' \
    "revival limit rejects values above the public maximum"

step "read-only revival check reports passing Revive specs and excludes Gate specs"
printf 'ready\n' >"$WS/revival-ready.marker"
before_ready_why="$(ee_json why "$revive_mem_id" --workspace "$WS" --include-sentinel --json)"
assert_jq "$before_ready_why" \
    '.data.sentinel.schema == "ee.memory_sentinel.why.v2"
     and .data.sentinel.summary.latestResultCount == 0
     and .data.sentinel.byPolarity.revive.specCount == 1
     and all(.data.sentinel.specs[]?; .polarity == "revive")' \
    "why exposes revival polarity before any persisted result exists"
before_ready_hash="$(db_state_hash)"
ready_revival="$(ee_json tripwire check --revivals --workspace "$WS" --json)"
after_ready_hash="$(db_state_hash)"
assert_eq "$after_ready_hash" "$before_ready_hash" \
    "tripwire revival check leaves database and WAL bytes unchanged"
REVIVE_ID="$revive_mem_id" GATE_ID="$help_mem_id" assert_jq "$ready_revival" \
    '.success == true
     and .data.schema == "ee.memory_sentinel.revivals.v1"
     and .data.polarity == "revive"
     and .data.observationMode == "explicit"
     and .data.commandHelpProcessExecution == true
     and .data.revivalCount >= 1
     and .data.memoryContentIncluded == false
     and .data.redactionStatus == "metadata_and_domain_separated_target_digest_no_memory_content_provenance_or_raw_target"
     and all(.data.revivals[]?; (.targetDigest | startswith("blake3:")) and (has("target") | not))
     and any(.data.revivals[]?; .memoryId == env.REVIVE_ID and .polarity == "revive" and .status == "pass")
     and all(.data.revivals[]?; .polarity == "revive" and .memoryId != env.GATE_ID)' \
    "revival happy path emits digest-safe Revive rows and excludes passing Gate specs"
after_ready_why="$(ee_json why "$revive_mem_id" --workspace "$WS" --include-sentinel --json)"
assert_jq "$after_ready_why" \
    '.data.sentinel.summary.latestResultCount == 0
     and .data.sentinel.summary.missingResultCount == 1' \
    "read-only revival check does not persist an automatic sentinel result"

implicit_orient="$(ee_json orient "inspect newly unblocked routes" --fast --workspace "$WS" --json)"
REVIVE_ID="$revive_mem_id" HELP_REVIVE_ID="$help_revive_id" ENV_REVIVE_ID="$env_revive_id" assert_jq "$implicit_orient" \
    '.success == true
     and .data.sideEffectFree == true
     and .data.revivals.observationMode == "implicit"
     and .data.revivals.evaluationPosture == "local_read_only_predicates_no_process_execution"
     and .data.revivals.commandHelpProcessExecution == false
     and any(.data.revivals.revivals[]?; .memoryId == env.REVIVE_ID and .kind == "path_exists")
     and any(.data.revivals.revivals[]?; .memoryId == env.ENV_REVIVE_ID and .kind == "env_var_registered")
     and all(.data.revivals.revivals[]?; .memoryId != env.HELP_REVIVE_ID)' \
    "orient keeps safe revival predicates live but never marks command-help revival ready"
implicit_orient_human="$(ee_json orient "inspect newly unblocked routes" --fast --workspace "$WS")"
assert_contains "$implicit_orient_human" "Revival evaluator: mode=implicit" \
    "orient human output identifies implicit revival evaluation"
assert_contains "$implicit_orient_human" "matched=" \
    "orient human output exposes matched revival count"
assert_contains "$implicit_orient_human" "evaluated=" \
    "orient human output exposes evaluated revival count"
assert_contains "$implicit_orient_human" "unevaluated=" \
    "orient human output exposes unevaluated revival count"
assert_contains "$implicit_orient_human" "truncated=" \
    "orient human output exposes revival truncation posture"
assert_contains "$implicit_orient_human" "limitRepair=" \
    "orient human output exposes revival limit repair"

step "revival JSON and human stdout never expose secret-shaped path targets"
sensitive_target="private/AKIAIOSFODNN7EXAMPLE-token.marker"
mkdir -p "$WS/private"
printf 'ready\n' >"$WS/$sensitive_target"
sensitive_mem="$(ee_json remember "A sensitive local marker can revive this route." \
    --workspace "$WS" --level episodic --kind failure \
    --revive-when "path_exists:$sensitive_target" --json)"
assert_jq "$sensitive_mem" '.success == true and .data.persisted == true' \
    "secret-shaped path revival memory persists"
sensitive_mem_id="$(printf '%s' "$sensitive_mem" | jq -r '.data.memory_id // empty')"
sensitive_json="$(ee_json tripwire check --revivals --workspace "$WS" --json)"
RAW_TARGET="$sensitive_target" SENSITIVE_ID="$sensitive_mem_id" assert_jq "$sensitive_json" \
    '([.. | strings | select(contains(env.RAW_TARGET))] | length) == 0
     and any(.data.revivals[]?;
         .memoryId == env.SENSITIVE_ID
         and (.targetDigest | startswith("blake3:"))
         and (has("target") | not))' \
    "revival JSON replaces the raw secret-shaped path with a digest"
sensitive_human="$(ee_json tripwire check --revivals --workspace "$WS")"
assert_eq "$([[ "$sensitive_human" == *"$sensitive_target"* ]] && echo leaked || echo redacted)" \
    "redacted" "revival human stdout omits the raw secret-shaped path"
assert_contains "$sensitive_human" "target=blake3:" \
    "revival human stdout retains digest-based target identity"
assert_contains "$sensitive_human" "Revival evaluator: mode=explicit" \
    "tripwire human output identifies explicit revival evaluation"
assert_contains "$sensitive_human" "matched=" \
    "tripwire human output exposes matched revival count"
assert_contains "$sensitive_human" "unevaluated=" \
    "tripwire human output exposes unevaluated revival count"
assert_contains "$sensitive_human" "truncated=" \
    "tripwire human output exposes revival truncation posture"
assert_contains "$sensitive_human" "limitRepair=" \
    "tripwire human output exposes revival limit repair"

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
    '.data.sentinel.schema == "ee.memory_sentinel.why.v2"
     and .data.sentinel.byPolarity.gate.failCount >= 1
     and .data.sentinel.byPolarity.revive.specCount == 0
     and any(.data.sentinel.specs[]?; .polarity == "gate" and .latestResult.status == "fail")' \
    "why reports latest failed sentinel status"

step "orient revival degradation fixture has a reproducible real filesystem trigger"
BROKEN_REVIVAL_WS=""
with_temp_workspace BROKEN_REVIVAL_WS
BROKEN_REVIVAL_DB="$EE_DATABASE_PATH"
broken_init="$(ee_json init --workspace "$BROKEN_REVIVAL_WS" --json)"
assert_jq "$broken_init" '.success == true' \
    "broken-revival fixture workspace initializes"
mv "$BROKEN_REVIVAL_DB" "$BROKEN_REVIVAL_DB.saved"
mkdir "$BROKEN_REVIVAL_DB"
broken_orient="$(ee_json orient "resume work" --fast --workspace "$BROKEN_REVIVAL_WS" --json)"
assert_jq "$broken_orient" \
    '.success == true
     and any(.degraded[]?;
         .code == "orient_revivals_unavailable"
         and .severity == "info"
         and (.message | contains("Revival sentinel check could not be assembled"))
         and (.repair | contains("ee tripwire check --revivals --json")))' \
    "orient emits the fixture-backed revival degradation when ee.db is a directory"

harness_summary
