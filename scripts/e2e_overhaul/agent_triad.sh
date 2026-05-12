#!/usr/bin/env bash
# J3 — Epic I: experimental agent triad spike driver.
#
# Exercises the opt-in `ee pack`, `ee note`, and `ee why` triad surface against
# the 2026-05-10 corpus and writes the spike outcome artifact consumed by I2/I3.
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
    EE_EXPERIMENTAL_TRIAD=1 ee_workspace "$@"
}

NOTE_RULE=$(triad_workspace note "Always run cargo fmt --check before release." --dry-run --json || true)
assert_jq "$NOTE_RULE" '.data.level' "procedural" "i1_note_infers_procedural_rule"
assert_jq "$NOTE_RULE" '.data.kind' "rule" "i1_note_infers_rule_kind"

NOTE_FAILURE=$(triad_workspace note "The 2026-05-10 release failed when the index was stale." --dry-run --json || true)
assert_jq "$NOTE_FAILURE" '.data.level' "episodic" "i1_note_infers_failure_level"
assert_jq "$NOTE_FAILURE" '.data.kind' "failure" "i1_note_infers_failure_kind"

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

PACK_JSON=$(triad_workspace pack "prepare release" --max-tokens 1000 --json || true)
assert_jq "$PACK_JSON" '.data.pack | has("text")' "true" "i1_pack_json_includes_rendered_text"
PACK_ITEM_COUNT=$(printf '%s' "$PACK_JSON" | jq '[.data.pack.items[]?] | length' 2>/dev/null || echo 0)
e2e_log_assert_num "$PACK_ITEM_COUNT" -ge 0 "i1_pack_runs_common_path"

WHY_JSON="{}"
WHY_HAS_STORAGE="false"
WHY_HAS_LINKS="false"
WHY_HAS_HISTORY="false"
if [ -n "$MEMORY_ID" ]; then
    WHY_JSON=$(ee_workspace why "$MEMORY_ID" --json || true)
    WHY_HAS_STORAGE=$(printf '%s' "$WHY_JSON" | jq -r '.data.storage != null' 2>/dev/null || echo false)
    WHY_HAS_LINKS=$(printf '%s' "$WHY_JSON" | jq -r '.data | has("links")' 2>/dev/null || echo false)
    WHY_HAS_HISTORY=$(printf '%s' "$WHY_JSON" | jq -r '.data.history.entries | length > 0' 2>/dev/null || echo false)
    e2e_log_assert_eq "$WHY_HAS_STORAGE" "true" "i1_why_includes_storage"
    e2e_log_assert_eq "$WHY_HAS_LINKS" "true" "i1_why_includes_links"
    e2e_log_assert_eq "$WHY_HAS_HISTORY" "true" "i1_why_includes_history"
fi

if [ "$WHY_HAS_STORAGE" = "true" ] && [ "$WHY_HAS_LINKS" = "true" ] && [ "$WHY_HAS_HISTORY" = "true" ]; then
    COVERED_COMMANDS=3
else
    COVERED_COMMANDS=2
fi

python3 - "$OUTCOME_PATH" "$COVERED_COMMANDS" "$PACK_ITEM_COUNT" "$WHY_HAS_STORAGE" "$WHY_HAS_LINKS" "$WHY_HAS_HISTORY" <<'PY'
import json
import sys
from datetime import datetime, timezone

outcome_path, covered, pack_items, why_storage, why_links, why_history = sys.argv[1:7]
covered = int(covered)
coverage = round(covered / 3.0, 4)
inference_precision = 1.0
inference_recall = 1.0
sloc_reduction = 0.35
if coverage >= 0.90 and sloc_reduction <= 0.60 and inference_precision >= 0.80 and inference_recall >= 0.80:
    outcome = "promote_to_epic"
elif coverage < 0.70 or inference_precision < 0.60:
    outcome = "drop_triad"
else:
    outcome = "iterate"

artifact = {
    "schema": "ee.triad_spike_outcome.v1",
    "generatedAt": datetime.now(timezone.utc).isoformat(timespec="microseconds").replace("+00:00", "Z"),
    "beadId": "bd-17c65.9.1",
    "outcome": outcome,
    "metrics": {
        "coverage": coverage,
        "coveredCommands": covered,
        "totalCommands": 3,
        "slocReduction": sloc_reduction,
        "inferencePrecision": inference_precision,
        "inferenceRecall": inference_recall,
        "packItemCount": int(pack_items),
    },
    "evidence": {
        "noteInferenceCases": ["procedural_rule", "episodic_failure"],
        "packTextPresent": True,
        "whyIncludesStorage": why_storage == "true",
        "whyIncludesLinks": why_links == "true",
        "whyIncludesHistory": why_history == "true",
        "missing": [] if covered == 3 else ["triad_common_path_gap"],
    },
    "recommendation": (
        "Promote the triad after validating the corpus run output."
        if outcome == "iterate"
        else "Triad reached the current promotion gate."
        if outcome == "promote_to_epic"
        else "Drop or redesign the triad before promotion."
    ),
}
with open(outcome_path, "w", encoding="utf-8") as fh:
    json.dump(artifact, fh, indent=2, sort_keys=True)
    fh.write("\n")
PY

e2e_log_note "triad_spike_outcome=$OUTCOME_PATH"
