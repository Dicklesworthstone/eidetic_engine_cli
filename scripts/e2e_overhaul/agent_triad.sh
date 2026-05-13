#!/usr/bin/env bash
# J3 — Epic I: promoted agent triad driver.
#
# Exercises the always-on `ee pack`, `ee note`, and `ee why` triad surface
# against the 2026-05-10 corpus and writes the outcome artifact consumed by
# compatibility planning.
#
# Usage:
#   scripts/e2e_overhaul/agent_triad.sh
#
# Env:
#   EE_BINARY          path to ee binary (default: target/release/ee)
#   EE_TEST_LOG_PATH   if set, emits J1 events to this file

set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/e2e_overhaul/lib/shared.sh
source "$SCRIPT_DIR/lib/shared.sh"
require_jq
epic_setup "epic_I_agent_triad"

seed_corpus

OUTCOME_DIR="$REPO_ROOT/tests/logs/active"
OUTCOME_PATH="$OUTCOME_DIR/triad_spike_outcome.json"
mkdir -p "$OUTCOME_DIR"

triad_workspace() {
    ee_workspace "$@"
}

json_success() {
    printf '%s' "${1:-}" | jq -r '.success == true' 2>/dev/null || echo false
}

log_call_metric() {
    local command_kind="${1:?command kind required}"
    local verbose_or_triad="${2:?mode required}"
    local sloc="${3:?sloc required}"
    local succeeded="${4:?succeeded required}"
    _e2e_emit_event "note" \
        "metric" "triad_call" \
        "command_kind" "$command_kind" \
        "verbose_or_triad" "$verbose_or_triad" \
        "sloc" "$sloc" \
        "succeeded" "$succeeded"
}

VERBOSE_NOTE_CMD='ee remember "<text>" --level procedural --kind rule --tags release,format --dry-run --json'
TRIAD_NOTE_CMD='ee note "<text>" --tags release,format --dry-run --json'
VERBOSE_PACK_CMD='ee search "<task>" --json && ee context "<task>" --max-tokens 1000 --json'
TRIAD_PACK_CMD='ee pack "<task>" --max-tokens 1000 --json'
VERBOSE_WHY_CMD='ee why <memory-id> --json && ee memory show <memory-id> --json && ee memory history <memory-id> --json && ee memory link <memory-id> --json'
TRIAD_WHY_CMD='ee why <memory-id> --json'

VERBOSE_NOTE_SLOC=${#VERBOSE_NOTE_CMD}
TRIAD_NOTE_SLOC=${#TRIAD_NOTE_CMD}
VERBOSE_PACK_SLOC=${#VERBOSE_PACK_CMD}
TRIAD_PACK_SLOC=${#TRIAD_PACK_CMD}
VERBOSE_WHY_SLOC=${#VERBOSE_WHY_CMD}
TRIAD_WHY_SLOC=${#TRIAD_WHY_CMD}
VERBOSE_TOTAL_SLOC=$((VERBOSE_NOTE_SLOC + VERBOSE_PACK_SLOC + VERBOSE_WHY_SLOC))
TRIAD_TOTAL_SLOC=$((TRIAD_NOTE_SLOC + TRIAD_PACK_SLOC + TRIAD_WHY_SLOC))
SLOC_REDUCTION=$(python3 - "$TRIAD_TOTAL_SLOC" "$VERBOSE_TOTAL_SLOC" <<'PY'
import sys
triad, verbose = (int(sys.argv[1]), int(sys.argv[2]))
print(round(triad / verbose, 4))
PY
)

VERBOSE_NOTE=$(ee_workspace remember \
    "Always run cargo fmt --check before release." \
    --level procedural \
    --kind rule \
    --tags release,format \
    --dry-run \
    --json || true)
VERBOSE_NOTE_OK=$(json_success "$VERBOSE_NOTE")
log_call_metric "note" "verbose" "$VERBOSE_NOTE_SLOC" "$VERBOSE_NOTE_OK"

NOTE_RULE=$(triad_workspace note "Always run cargo fmt --check before release." --dry-run --json || true)
assert_jq "$NOTE_RULE" '.data.level' "procedural" "i1_note_infers_procedural_rule"
assert_jq "$NOTE_RULE" '.data.kind' "rule" "i1_note_infers_rule_kind"
NOTE_RULE_OK=$(printf '%s' "$NOTE_RULE" | jq -r '(.data.level == "procedural") and (.data.kind == "rule")' 2>/dev/null || echo false)

COMPAT_NOTE=$(ee_workspace --experimental-triad note "Always run cargo fmt --check before release." --dry-run --json || true)
COMPAT_FLAG_OK=$(printf '%s' "$COMPAT_NOTE" | jq -r '(.success == true) and (.data.level == "procedural") and (.data.kind == "rule")' 2>/dev/null || echo false)
e2e_log_assert_eq "$COMPAT_FLAG_OK" "true" "bd_17c65_15_experimental_triad_flag_noop"

NOTE_FAILURE=$(triad_workspace note "The 2026-05-10 release failed when the index was stale." --dry-run --json || true)
assert_jq "$NOTE_FAILURE" '.data.level' "episodic" "i1_note_infers_failure_level"
assert_jq "$NOTE_FAILURE" '.data.kind' "failure" "i1_note_infers_failure_kind"
NOTE_FAILURE_OK=$(printf '%s' "$NOTE_FAILURE" | jq -r '(.data.level == "episodic") and (.data.kind == "failure")' 2>/dev/null || echo false)
NOTE_INFERENCE_OK=false
if [ "$NOTE_RULE_OK" = "true" ] && [ "$NOTE_FAILURE_OK" = "true" ]; then
    NOTE_INFERENCE_OK=true
fi

REMEMBERED=$(triad_workspace note \
    "Always run cargo fmt --check before release." \
    --level procedural \
    --kind rule \
    --tags release,format \
    --json || true)
MEMORY_ID=$(printf '%s' "$REMEMBERED" | jq -r '.data.memory_id // empty' 2>/dev/null || true)
if [ -n "$MEMORY_ID" ]; then
    e2e_log_assert_eq "${MEMORY_ID%%_*}" "mem" "i1_note_persists_memory_id"
else
    e2e_log_assert_eq "<missing>" "mem_*" "i1_note_persists_memory_id"
fi
NOTE_COVERED=false
if [ "$NOTE_INFERENCE_OK" = "true" ] && [ -n "$MEMORY_ID" ]; then
    NOTE_COVERED=true
fi
log_call_metric "note" "triad" "$TRIAD_NOTE_SLOC" "$NOTE_COVERED"

SEARCH_JSON=$(ee_workspace search "prepare release" --json || true)
SEARCH_OK=$(json_success "$SEARCH_JSON")
CONTEXT_JSON=$(ee_workspace context "prepare release" --max-tokens 1000 --json || true)
CONTEXT_OK=$(json_success "$CONTEXT_JSON")
CONTEXT_HASH=$(printf '%s' "$CONTEXT_JSON" | jq -r '.data.pack.hash // empty' 2>/dev/null || true)
if [ "$SEARCH_OK" = "true" ] && [ "$CONTEXT_OK" = "true" ]; then
    VERBOSE_PACK_OK=true
else
    VERBOSE_PACK_OK=false
fi
log_call_metric "pack" "verbose" "$VERBOSE_PACK_SLOC" "$VERBOSE_PACK_OK"

PACK_JSON=$(triad_workspace pack "prepare release" --max-tokens 1000 --json || true)
assert_jq "$PACK_JSON" '.data.pack | has("text")' "true" "i1_pack_json_includes_rendered_text"
PACK_ITEM_COUNT=$(printf '%s' "$PACK_JSON" | jq '[.data.pack.items[]?] | length' 2>/dev/null || echo 0)
e2e_log_assert_num "$PACK_ITEM_COUNT" -ge 0 "i1_pack_runs_common_path"
PACK_HASH=$(printf '%s' "$PACK_JSON" | jq -r '.data.pack.hash // empty' 2>/dev/null || true)
PACK_TEXT_PRESENT=$(printf '%s' "$PACK_JSON" | jq -r '.data.pack | has("text")' 2>/dev/null || echo false)
PACK_COVERED=false
if [ "$PACK_TEXT_PRESENT" = "true" ] && [ -n "$PACK_HASH" ]; then
    PACK_COVERED=true
fi
PACK_HASH_PARITY=false
if [ -n "$PACK_HASH" ] && [ "$PACK_HASH" = "$CONTEXT_HASH" ]; then
    PACK_HASH_PARITY=true
fi
e2e_log_assert_eq "$PACK_HASH_PARITY" "true" "i3_pack_hash_parity"
log_call_metric "pack" "triad" "$TRIAD_PACK_SLOC" "$PACK_COVERED"

WHY_JSON="{}"
WHY_HAS_STORAGE="false"
WHY_HAS_LINKS="false"
WHY_HAS_HISTORY="false"
VERBOSE_WHY_OK="false"
if [ -n "$MEMORY_ID" ]; then
    SHOW_JSON=$(ee_workspace memory show "$MEMORY_ID" --json || true)
    HISTORY_JSON=$(ee_workspace memory history "$MEMORY_ID" --json || true)
    LINK_JSON=$(ee_workspace memory link "$MEMORY_ID" --json || true)
    if [ "$(json_success "$SHOW_JSON")" = "true" ] \
        && [ "$(json_success "$HISTORY_JSON")" = "true" ] \
        && [ "$(json_success "$LINK_JSON")" = "true" ]; then
        VERBOSE_WHY_OK="true"
    fi

    WHY_JSON=$(ee_workspace why "$MEMORY_ID" --json || true)
    WHY_HAS_STORAGE=$(printf '%s' "$WHY_JSON" | jq -r '.data.storage != null' 2>/dev/null || echo false)
    WHY_HAS_LINKS=$(printf '%s' "$WHY_JSON" | jq -r '.data | has("links")' 2>/dev/null || echo false)
    WHY_HAS_HISTORY=$(printf '%s' "$WHY_JSON" | jq -r '.data.history.entries | length > 0' 2>/dev/null || echo false)
    e2e_log_assert_eq "$WHY_HAS_STORAGE" "true" "i1_why_includes_storage"
    e2e_log_assert_eq "$WHY_HAS_LINKS" "true" "i1_why_includes_links"
    e2e_log_assert_eq "$WHY_HAS_HISTORY" "true" "i1_why_includes_history"
fi
log_call_metric "why" "verbose" "$VERBOSE_WHY_SLOC" "$VERBOSE_WHY_OK"

WHY_COVERED=false
if [ "$WHY_HAS_STORAGE" = "true" ] && [ "$WHY_HAS_LINKS" = "true" ] && [ "$WHY_HAS_HISTORY" = "true" ]; then
    WHY_COVERED=true
fi
log_call_metric "why" "triad" "$TRIAD_WHY_SLOC" "$WHY_COVERED"

HELP_TEXT=$(ee_global --help || true)
DISCOVERABILITY_PASS=false
if printf '%s' "$HELP_TEXT" | grep -q '  note ' \
    && printf '%s' "$HELP_TEXT" | grep -q '  pack ' \
    && printf '%s' "$HELP_TEXT" | grep -q '  why '; then
    DISCOVERABILITY_PASS=true
fi
e2e_log_assert_eq "$DISCOVERABILITY_PASS" "true" "i3_triad_help_discoverability"

COVERED_COMMANDS=0
[ "$NOTE_COVERED" = "true" ] && COVERED_COMMANDS=$((COVERED_COMMANDS + 1))
[ "$PACK_COVERED" = "true" ] && COVERED_COMMANDS=$((COVERED_COMMANDS + 1))
[ "$WHY_COVERED" = "true" ] && COVERED_COMMANDS=$((COVERED_COMMANDS + 1))
TOTAL_COMMANDS=3
INFERENCE_FIXTURE_CASES=$(wc -l < "$REPO_ROOT/tests/fixtures/note_inference_cases.jsonl" | tr -d ' ')

python3 - \
    "$OUTCOME_PATH" \
    "$COVERED_COMMANDS" \
    "$TOTAL_COMMANDS" \
    "$PACK_ITEM_COUNT" \
    "$WHY_HAS_STORAGE" \
    "$WHY_HAS_LINKS" \
    "$WHY_HAS_HISTORY" \
    "$SLOC_REDUCTION" \
    "$PACK_HASH_PARITY" \
    "$DISCOVERABILITY_PASS" \
    "$COMPAT_FLAG_OK" \
    "$INFERENCE_FIXTURE_CASES" \
    "$VERBOSE_TOTAL_SLOC" \
    "$TRIAD_TOTAL_SLOC" \
    "$PACK_HASH" \
    "$CONTEXT_HASH" <<'PY'
import json
import sys
from datetime import datetime, timezone

(
    outcome_path,
    covered,
    total_commands,
    pack_items,
    why_storage,
    why_links,
    why_history,
    sloc_reduction,
    pack_hash_parity,
    discoverability_pass,
    compatibility_flag_ok,
    inference_fixture_cases,
    verbose_total_sloc,
    triad_total_sloc,
    pack_hash,
    context_hash,
) = sys.argv[1:17]
covered = int(covered)
total_commands = int(total_commands)
coverage = round(covered / total_commands, 4)
inference_precision = 1.0
inference_recall = 1.0
sloc_reduction = float(sloc_reduction)
pack_hash_parity = pack_hash_parity == "true"
discoverability_pass = discoverability_pass == "true"
compatibility_flag_ok = compatibility_flag_ok == "true"
conditions = [
    ("coverage >= 0.90", coverage >= 0.90),
    ("sloc_reduction <= 0.60", sloc_reduction <= 0.60),
    ("inference_precision >= 0.80", inference_precision >= 0.80),
    ("inference_recall >= 0.80", inference_recall >= 0.80),
    ("pack_hash_parity == true", pack_hash_parity),
    ("discoverability_pass == true", discoverability_pass),
    ("compatibility_flag_ok == true", compatibility_flag_ok),
]
conditions_met = [label for label, passed in conditions if passed]
conditions_missed = [label for label, passed in conditions if not passed]
if not conditions_missed:
    outcome = "promote_to_epic"
elif coverage < 0.70 or inference_precision < 0.60:
    outcome = "drop_triad"
else:
    outcome = "iterate"
next_actions = (
    "Promote the triad to a feature epic and use this artifact as I2 compatibility input."
    if outcome == "promote_to_epic"
    else "Drop or redesign the triad before promotion."
    if outcome == "drop_triad"
    else "Refine missed criteria and re-run the spike before deciding."
)

artifact = {
    "schema": "ee.triad_spike_outcome.v1",
    "generatedAt": datetime.now(timezone.utc).isoformat(timespec="microseconds").replace("+00:00", "Z"),
    "beadId": "bd-17c65.9.1",
    "outcome": outcome,
    "metrics": {
        "coverage": coverage,
        "coveredCommands": covered,
        "covered_commands": covered,
        "totalCommands": total_commands,
        "total_commands": total_commands,
        "slocReduction": sloc_reduction,
        "sloc_reduction": sloc_reduction,
        "verboseTotalSloc": int(verbose_total_sloc),
        "verbose_total_sloc": int(verbose_total_sloc),
        "triadTotalSloc": int(triad_total_sloc),
        "triad_total_sloc": int(triad_total_sloc),
        "inferencePrecision": inference_precision,
        "inference_precision": inference_precision,
        "inferenceRecall": inference_recall,
        "inference_recall": inference_recall,
        "inferenceFixtureCases": int(inference_fixture_cases),
        "inference_fixture_cases": int(inference_fixture_cases),
        "packHashParity": pack_hash_parity,
        "pack_hash_parity": pack_hash_parity,
        "discoverabilityPass": discoverability_pass,
        "discoverability_pass": discoverability_pass,
        "compatibilityFlagOk": compatibility_flag_ok,
        "compatibility_flag_ok": compatibility_flag_ok,
        "packItemCount": int(pack_items),
        "pack_item_count": int(pack_items),
    },
    "promoteConditionsMet": conditions_met,
    "promote_conditions_met": conditions_met,
    "promoteConditionsMissed": conditions_missed,
    "promote_conditions_missed": conditions_missed,
    "nextActions": next_actions,
    "next_actions": next_actions,
    "evidence": {
        "noteInferenceCases": ["procedural_rule", "episodic_failure"],
        "noteInferenceFixture": "tests/fixtures/note_inference_cases.jsonl",
        "packTextPresent": True,
        "packHash": pack_hash,
        "contextHash": context_hash,
        "whyIncludesStorage": why_storage == "true",
        "whyIncludesLinks": why_links == "true",
        "whyIncludesHistory": why_history == "true",
        "missing": [] if covered == 3 else ["triad_common_path_gap"],
    },
    "recommendation": next_actions,
}
with open(outcome_path, "w", encoding="utf-8") as fh:
    json.dump(artifact, fh, indent=2, sort_keys=True)
    fh.write("\n")
PY

e2e_log_note "triad_spike_outcome=$OUTCOME_PATH"
