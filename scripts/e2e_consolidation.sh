#!/usr/bin/env bash
# bd-1oep7 - Consolidation Maintain-loop e2e (real binary, no mocks).
#
# Proves the fifth core job closes the consolidation loop through public
# surfaces only: duplicate fixture -> steward consolidation_pass (dry-run
# non-mutation proven by a durable-state snapshot, zero-item-budget
# cancellation, real run with bounded budget accounting, dedupe determinism)
# -> ee curate validate/apply (consolidate-absorb: derived_from lineage,
# tombstoned duplicate, preserved survivor, append-only audit chain under the
# canonical ee.response.v2 envelope) -> workflow-emitted index_coalesce (no
# manual rebuild) -> truthful generation/per-kind counts -> structured
# exactly-once search AND pack identity -> idempotent re-runs. Every emitted
# ee.test_event.v1 line is parsed and schema-validated at the end.
# The script intentionally does not build; central RCH verify provides the
# binary.
set -uo pipefail

E2E_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

if [ -d /private/tmp ]; then
    EE_E2E_TMPDIR="${EE_E2E_TMPDIR:-/private/tmp}"
    export EE_E2E_TMPDIR
fi
export EE_EMBED_DOWNLOAD="${EE_EMBED_DOWNLOAD:-off}"

# shellcheck source=scripts/e2e_lib.sh
# shellcheck disable=SC1091
source "$E2E_DIR/e2e_lib.sh"

harness_init "consolidation"

# Evidence command runner: never silences stderr (the logger records the
# stderr hash + excerpt) and never masks a nonzero exit. Because ee_json runs
# inside command substitutions (subshells), rc failures are relayed through a
# file and folded into the harness FAIL counter in the parent shell before
# the summary — a nonzero evidence command can never green the run.
EE_JSON_RC_FAILURES="${LOG_DIR:-${EE_E2E_TMPDIR:-/tmp}}/ee_json_rc_failures.txt"
: >"$EE_JSON_RC_FAILURES"
ee_json() {
    local out rc
    out="$(e2e_log_command "$EE_BIN" "$@")"
    rc=$?
    if [ "$rc" -ne 0 ]; then
        printf 'ee %s exited nonzero (rc=%s)\n' "${1:-<none>}" "$rc" >>"$EE_JSON_RC_FAILURES"
        printf '  [FAIL-rc] ee %s rc=%s\n' "${1:-}" "$rc" >&2
    fi
    printf '%s' "$out"
}

# Variant for the one step whose nonzero exit is part of the contract under
# test (budget cancellation); the outcome assertion carries the proof.
ee_json_tolerant() {
    local out
    out="$(e2e_log_command "$EE_BIN" "$@")" || true
    printf '%s' "$out"
}

json_scalar() {
    local json="${1:?json required}"
    local filter="${2:?jq filter required}"
    printf '%s' "$json" | jq -r "$filter" 2>/dev/null || true
}

# Every machine-facing success response must be a canonical ee.response.v2
# envelope; bare reports are a contract defect.
assert_envelope() {
    local json="${1:?json required}"
    local label="${2:?label required}"
    assert_jq "$json" '.schema == "ee.response.v2" and .success == true and (.data | type == "object")' \
        "$label emits a canonical ee.response.v2 success envelope"
}

# Failure-propagating numeric assertion: logs the ee.test_event.v1 assert AND
# routes through the harness FAIL counter, so a red numeric check can never
# green the summary (harness_summary exits nonzero on any _harness_fail).
assert_num() {
    local actual="$1" op="$2" expected="$3" label="$4"
    if e2e_log_assert_num "$actual" "$op" "$expected" "$label"; then
        _harness_pass "$label ($actual $op $expected)"
    else
        _harness_fail "$label: expected $actual $op $expected"
    fi
}

with_temp_workspace WS

GROUP_PHRASE="Zephyr quill consolidation gate: run cargo fmt --check before release."
DUPLICATE_PHRASE="  zephyr   QUILL consolidation gate: run cargo fmt --check before release. "
WORDING_CONTROL_PHRASE="Zephyr quill consolidation gate: run cargo fmt --check before publishing."
SEARCH_QUERY="zephyr quill consolidation gate"

step "init isolated workspace"
init_out="$(ee_json init --workspace "$WS" --json)"
assert_envelope "$init_out" "ee init"
log_event "consolidation_workspace" \
    workspaceHash "$(printf '%s' "$WS" | shasum -a 256 | awk '{print $1}')" \
    bead "bd-1oep7"

step "seed duplicates plus near-but-not-eligible controls"
# Asserts run in the parent shell (never inside command substitutions) so
# every failure reaches the harness counter.
remember_memory_json() {
    local content="$1" kind="$2" confidence="$3"
    ee_json remember "$content" --workspace "$WS" \
        --level semantic --kind "$kind" --confidence "$confidence" \
        --no-propose-candidates --no-auto-link --json
}
SURVIVOR_JSON="$(remember_memory_json "$GROUP_PHRASE" fact 0.9)"
assert_envelope "$SURVIVOR_JSON" "remember survivor"
SURVIVOR_ID="$(json_scalar "$SURVIVOR_JSON" '.data.memory_id // .data.memoryId // empty')"
DUPLICATE_JSON="$(remember_memory_json "$DUPLICATE_PHRASE" fact 0.4)"
assert_envelope "$DUPLICATE_JSON" "remember duplicate"
DUPLICATE_ID="$(json_scalar "$DUPLICATE_JSON" '.data.memory_id // .data.memoryId // empty')"
WORDING_CONTROL_JSON="$(remember_memory_json "$WORDING_CONTROL_PHRASE" fact 0.4)"
assert_envelope "$WORDING_CONTROL_JSON" "remember wording control"
WORDING_CONTROL_ID="$(json_scalar "$WORDING_CONTROL_JSON" '.data.memory_id // .data.memoryId // empty')"
KIND_CONTROL_JSON="$(remember_memory_json "$GROUP_PHRASE" decision 0.4)"
assert_envelope "$KIND_CONTROL_JSON" "remember kind control"
KIND_CONTROL_ID="$(json_scalar "$KIND_CONTROL_JSON" '.data.memory_id // .data.memoryId // empty')"
if [ -z "$SURVIVOR_ID" ] || [ -z "$DUPLICATE_ID" ] || [ -z "$WORDING_CONTROL_ID" ] || [ -z "$KIND_CONTROL_ID" ]; then
    assert_eq "missing" "present" "consolidation fixture memory ids created"
    harness_summary
    exit 1
fi
log_event "consolidation_fixture" \
    survivorId "$SURVIVOR_ID" duplicateId "$DUPLICATE_ID" \
    wordingControlId "$WORDING_CONTROL_ID" kindControlId "$KIND_CONTROL_ID"

step "workflow-emitted indexing establishes a truthful baseline"
coalesce_out="$(ee_json daemon --workspace "$WS" --foreground --once --job index_coalesce --json)"
assert_envelope "$coalesce_out" "baseline index_coalesce"
baseline_status="$(ee_json index status --workspace "$WS" --json)"
assert_jq "$baseline_status" '.data.dbGeneration != null and .data.dbGeneration == .data.indexGeneration' \
    "baseline generation is truthful"
assert_jq "$baseline_status" '.data.indexDocumentCounts.memories == 4 and .data.health == "ready"' \
    "baseline index holds all four fixture memories"

# Durable-state snapshot used to prove dry-run non-mutation: memory rows,
# tombstoned rows, consolidate candidates across all statuses, total audit
# rows, workspace generation, index generation, per-kind memory documents.
durable_snapshot() {
    local memories candidates audits status
    memories="$(ee_json memory list --workspace "$WS" --json)"
    candidates="$(ee_json curate candidates --workspace "$WS" --type consolidate --all --json)"
    audits="$(ee_json audit timeline --workspace "$WS" --limit 1 --json)"
    status="$(ee_json index status --workspace "$WS" --json)"
    printf 'memories=%s tombstoned=%s candidates=%s audits=%s dbGen=%s idxGen=%s idxMem=%s' \
        "$(json_scalar "$memories" '.data.memories | length')" \
        "$(json_scalar "$memories" '[.data.memories[]? | select((.is_tombstoned // .isTombstoned // false))] | length')" \
        "$(json_scalar "$candidates" '.data.candidates | length')" \
        "$(json_scalar "$audits" '.data.pagination.total_count')" \
        "$(json_scalar "$status" '.data.dbGeneration')" \
        "$(json_scalar "$status" '.data.indexGeneration')" \
        "$(json_scalar "$status" '.data.indexDocumentCounts.memories')"
}

step "dry-run consolidation_pass plans without mutating any durable object"
SNAPSHOT_BEFORE_DRY="$(durable_snapshot)"
assert_eq "$(printf '%s' "$SNAPSHOT_BEFORE_DRY" | grep -o 'memories=4 tombstoned=0 candidates=0' || echo mismatch)" \
    "memories=4 tombstoned=0 candidates=0" \
    "pre-dry-run fixture is four live memories with no candidates"
dry_out="$(ee_json daemon --workspace "$WS" --foreground --once --job consolidation_pass --dry-run --json)"
assert_jq "$dry_out" '.data.ticks[0].runner.results[0].details.dryRun == true' \
    "dry-run reports dryRun"
assert_jq "$dry_out" '.data.ticks[0].runner.results[0].details.plannedCandidates == 1' \
    "dry-run plans exactly one candidate"
assert_jq "$dry_out" '.data.ticks[0].runner.results[0].details.insertedCandidates == 0 and .data.ticks[0].runner.results[0].details.durableMutation == false' \
    "dry-run inserts nothing"
assert_jq "$dry_out" '.data.ticks[0].runner.results[0].itemsProcessed == 1 and .data.ticks[0].runner.results[0].details.selector.maxCandidates == 64' \
    "dry-run reports an actual bounded selector budget"
SNAPSHOT_AFTER_DRY="$(durable_snapshot)"
assert_eq "$SNAPSHOT_AFTER_DRY" "$SNAPSHOT_BEFORE_DRY" \
    "dry-run leaves the full durable-state snapshot unchanged"

step "zero item budget cancels before any mutation"
cancel_out="$(ee_json_tolerant daemon --workspace "$WS" --foreground --once --job consolidation_pass --item-limit 0 --json)"
assert_jq "$cancel_out" '.data.ticks[0].runner.results[0].outcome == "cancelled"' \
    "zero item budget cancels the steward job"
SNAPSHOT_AFTER_CANCEL="$(durable_snapshot)"
assert_eq "$SNAPSHOT_AFTER_CANCEL" "$SNAPSHOT_BEFORE_DRY" \
    "cancelled run leaves the full durable-state snapshot unchanged"

step "real consolidation_pass inserts one deterministic deduplicated candidate"
real_out="$(ee_json daemon --workspace "$WS" --foreground --once --job consolidation_pass --json)"
assert_jq "$real_out" '.data.ticks[0].runner.results[0].details.insertedCandidates == 1 and .data.ticks[0].runner.results[0].details.durableMutation == true' \
    "real run inserts exactly one candidate"
assert_jq "$real_out" '.data.ticks[0].runner.results[0].budgetUsed.violations == 0 and .data.ticks[0].runner.results[0].itemsProcessed == 1' \
    "real run reports zero budget violations with bounded accounting"
CANDIDATE_ID="$(json_scalar "$real_out" '.data.ticks[0].runner.results[0].details.candidateIds[0] // empty')"
assert_eq "$([ -n "$CANDIDATE_ID" ] && echo present || echo missing)" "present" \
    "real run emits the candidate id"
rerun_out="$(ee_json daemon --workspace "$WS" --foreground --once --job consolidation_pass --json)"
assert_jq "$rerun_out" '.data.ticks[0].runner.results[0].details.insertedCandidates == 0 and .data.ticks[0].runner.results[0].details.alreadyPendingCandidates == 1' \
    "re-run dedupes instead of duplicating"
RERUN_CANDIDATE_ID="$(json_scalar "$rerun_out" '.data.ticks[0].runner.results[0].details.candidateIds[0] // empty')"
assert_eq "$RERUN_CANDIDATE_ID" "$CANDIDATE_ID" \
    "re-run plans the same deterministic candidate id"
create_audit="$(ee_json audit timeline --workspace "$WS" --action curation_candidate.create --json)"
assert_envelope "$create_audit" "audit timeline (create)"
assert_jq "$create_audit" '.data.schema == "ee.audit.timeline.v1" and .data.pagination.total_count == 1' \
    "exactly one creation audit row exists"

step "validate and apply through the public curation commands"
validate_out="$(ee_json curate validate "$CANDIDATE_ID" --workspace "$WS" --json)"
assert_envelope "$validate_out" "curate validate"
assert_jq "$validate_out" '.data.mutation.toStatus == "approved"' \
    "validate approves the candidate"
apply_out="$(ee_json curate apply "$CANDIDATE_ID" --workspace "$WS" --json)"
assert_envelope "$apply_out" "curate apply"
assert_jq "$apply_out" '.data.application.decision == "consolidate_absorb" and .data.application.status == "applied"' \
    "apply runs the consolidate-absorb decision"
assert_jq "$apply_out" '.data.durableMutation == true' \
    "apply reports a durable mutation"
survivor_change_count="$(printf '%s' "$apply_out" | jq -r --arg survivor "$SURVIVOR_ID" \
    '[.data.application.changes[]? | select(.field == "consolidatedIntoMemoryId" and .after == $survivor)] | length' 2>/dev/null || echo -1)"
assert_eq "$survivor_change_count" "1" \
    "apply changes structurally name the survivor"

step "absorb preserves the source and tombstones only the duplicate"
memories_out="$(ee_json memory list --workspace "$WS" --json)"
duplicate_state="$(printf '%s' "$memories_out" | jq -r --arg id "$DUPLICATE_ID" \
    '.data.memories[]? | select(.id == $id) | if (.is_tombstoned // .isTombstoned // false) then "tombstoned" else "active" end' 2>/dev/null || true)"
assert_eq "$duplicate_state" "tombstoned" \
    "absorbed duplicate is preserved as a tombstoned row"
survivor_state="$(printf '%s' "$memories_out" | jq -r --arg id "$SURVIVOR_ID" --arg content "$GROUP_PHRASE" \
    '.data.memories[]? | select(.id == $id) | if ((.is_tombstoned // .isTombstoned // false) | not) and .content == $content then "intact" else "mutated" end' 2>/dev/null || true)"
assert_eq "$survivor_state" "intact" \
    "survivor keeps its content with no opaque rewrite"

step "why explains lineage and the audit chain stays intact"
why_out="$(ee_json why "$SURVIVOR_ID" --workspace "$WS" --json)"
lineage="$(printf '%s' "$why_out" | jq -r --arg id "$DUPLICATE_ID" \
    '[.data.links[]? | select(.relation == "derived_from" and .linkedMemoryId == $id)] | length' 2>/dev/null || echo 0)"
assert_num "${lineage:-0}" -eq 1 \
    "why survivor explains exactly one derived_from lineage edge"
apply_audit="$(ee_json audit timeline --workspace "$WS" --action curation_candidate.apply --json)"
assert_envelope "$apply_audit" "audit timeline (apply)"
assert_jq "$apply_audit" '.data.pagination.total_count == 1' \
    "exactly one apply audit row exists"
verify_out="$(ee_json audit verify --workspace "$WS" --json)"
assert_envelope "$verify_out" "audit verify"
assert_jq "$verify_out" '.data.schema == "ee.audit.verify.v1" and .data.integrity_ok == true' \
    "append-only audit hash chain verifies"

step "workflow job restores a truthful deduplicated index"
stale_status="$(ee_json index status --workspace "$WS" --json)"
assert_jq "$stale_status" '.data.dbGeneration > .data.indexGeneration and .data.health == "stale"' \
    "apply leaves an honestly stale index until the workflow job runs"
post_coalesce="$(ee_json daemon --workspace "$WS" --foreground --once --job index_coalesce --json)"
assert_envelope "$post_coalesce" "post-apply index_coalesce"
truthful_status="$(ee_json index status --workspace "$WS" --json)"
assert_jq "$truthful_status" '.data.dbGeneration == .data.indexGeneration and .data.health == "ready"' \
    "generation is truthful after the workflow-emitted job"
assert_jq "$truthful_status" '.data.indexDocumentCounts.memories == 3' \
    "per-kind index counts drop the absorbed duplicate exactly once"

step "search returns exactly one structured survivor and distinct controls"
search_out="$(ee_json search "$SEARCH_QUERY" --workspace "$WS" --limit 10 --json)"
assert_envelope "$search_out" "search"
count_search_id() {
    printf '%s' "$search_out" | jq -r --arg id "$1" \
        '[.data.results[]? | select(.memoryId == $id)] | length' 2>/dev/null || echo -1
}
assert_num "$(count_search_id "$SURVIVOR_ID")" -eq 1 \
    "search returns the consolidated survivor exactly once"
assert_num "$(count_search_id "$DUPLICATE_ID")" -eq 0 \
    "search never returns the absorbed duplicate"
assert_num "$(count_search_id "$WORDING_CONTROL_ID")" -eq 1 \
    "wording control remains exactly one distinct result"
assert_num "$(count_search_id "$KIND_CONTROL_ID")" -eq 1 \
    "kind control remains exactly one distinct result"

step "pack contains exactly one structured survivor item and distinct controls"
pack_out="$(ee_json pack "$SEARCH_QUERY" --workspace "$WS" --max-tokens 2000 --json)"
assert_envelope "$pack_out" "pack"
count_pack_id() {
    printf '%s' "$pack_out" | jq -r --arg id "$1" \
        '[.data.pack.items[]? | select(.memoryId == $id)] | length' 2>/dev/null || echo -1
}
assert_num "$(count_pack_id "$SURVIVOR_ID")" -eq 1 \
    "pack contains the consolidated survivor as exactly one item"
assert_num "$(count_pack_id "$DUPLICATE_ID")" -eq 0 \
    "pack never contains the absorbed duplicate"
assert_num "$(count_pack_id "$WORDING_CONTROL_ID")" -eq 1 \
    "wording control packs as exactly one distinct item"
assert_num "$(count_pack_id "$KIND_CONTROL_ID")" -eq 1 \
    "kind control packs as exactly one distinct item"

step "steward and apply re-runs are idempotent"
SNAPSHOT_BEFORE_REPLAY="$(durable_snapshot)"
idempotent_out="$(ee_json daemon --workspace "$WS" --foreground --once --job consolidation_pass --json)"
assert_jq "$idempotent_out" '.data.ticks[0].runner.results[0].details.plannedCandidates == 0 and .data.ticks[0].runner.results[0].details.insertedCandidates == 0' \
    "steward re-run after absorb has nothing to plan"
replay_out="$(ee_json curate apply "$CANDIDATE_ID" --workspace "$WS" --json)"
assert_jq "$replay_out" '.data.application.status == "already_applied" and .data.durableMutation == false' \
    "apply replay is an idempotent no-op"
SNAPSHOT_AFTER_REPLAY="$(durable_snapshot)"
assert_eq "$SNAPSHOT_AFTER_REPLAY" "$SNAPSHOT_BEFORE_REPLAY" \
    "replays leave the full durable-state snapshot unchanged"
assert_eq "$(printf '%s' "$SNAPSHOT_AFTER_REPLAY" | grep -o 'memories=4 tombstoned=1 candidates=1' || echo mismatch)" \
    "memories=4 tombstoned=1 candidates=1" \
    "closed-loop census holds after replay"

step "every emitted ee.test_event.v1 line parses and validates"
EVENT_VALIDATION="$(python3 - "$EE_TEST_LOG_PATH" <<'PY'
import json
import sys

path = sys.argv[1]
required = {"schema", "ts", "test_id", "kind"}
total = 0
bad = []
try:
    with open(path, encoding="utf-8") as handle:
        for line_number, line in enumerate(handle, start=1):
            line = line.strip()
            if not line:
                continue
            total += 1
            try:
                event = json.loads(line)
            except json.JSONDecodeError as error:
                bad.append(f"line {line_number}: invalid JSON ({error})")
                continue
            if event.get("schema") != "ee.test_event.v1":
                bad.append(f"line {line_number}: schema={event.get('schema')!r}")
                continue
            missing = required - set(event)
            if missing:
                bad.append(f"line {line_number}: missing {sorted(missing)}")
except OSError as error:
    print(f"error opening {path}: {error}")
    sys.exit(0)
if bad:
    print(f"invalid ({len(bad)}/{total}): " + "; ".join(bad[:5]))
else:
    print(f"valid {total}")
PY
)"
case "$EVENT_VALIDATION" in
    valid\ *)
        EVENT_COUNT="${EVENT_VALIDATION#valid }"
        assert_num "${EVENT_COUNT:-0}" -ge 20 \
            "every emitted test event line is valid ee.test_event.v1 (count=$EVENT_COUNT)"
        ;;
    *)
        assert_eq "$EVENT_VALIDATION" "valid" \
            "every emitted test event line is valid ee.test_event.v1"
        ;;
esac

step "no evidence command exited nonzero unexpectedly"
if [ -s "$EE_JSON_RC_FAILURES" ]; then
    while IFS= read -r rc_failure; do
        _harness_fail "$rc_failure"
    done <"$EE_JSON_RC_FAILURES"
else
    _harness_pass "all evidence commands exited zero"
fi

if [ "${EE_GRAPH_E2E_INJECT_FAILURE:-0}" = "1" ]; then
    # Deliberate-negative selftest: this failing assertion routes through the
    # harness FAIL counter, so the summary MUST go red and exit nonzero. A
    # green exit under injection is itself a harness defect.
    assert_eq "actual-consolidation" "expected-consolidation" "consolidation_injected_failure_diff"
fi

harness_summary
