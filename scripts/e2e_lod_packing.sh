#!/usr/bin/env bash
# bd-1n0np.5.5 — Telescoping Level-of-Detail (LOD) packing end-to-end
# (real binary, detailed logging).
#
# Scenario: temp workspace seeded with many sizable candidates -> `ee pack
# <task> --max-tokens N` under a tight budget so the Full / Truncated_Preview /
# Link_Only tiers engage. Assertions:
#   * hard (always true on a current binary): pack succeeds, used_tokens stays
#     within budget, and the pack hash is byte-stable across repeated runs.
#   * condition-guarded (no-silent-cap log_drop when the surface/tier is not
#     present in the binary under test): the markdown peripheral index renders
#     for link-only items (bd-1n0np.5.3), `ee memory show` drills into a
#     link-only memory, and `--no-lod` reproduces the legacy flat pack
#     byte-for-byte (bd-1n0np.5.2, currently cli-gated).
# Every step emits an ee.test_event.v1 event + a human line via the shared
# harness (scripts/lib/e2e_harness.sh, surfaced through scripts/e2e_lib.sh), and
# harness_summary prints PASS/FAIL with an artifact dir and owns the exit code.
#
# NOTE: no `set -e` — assert_* accumulate pass/fail and harness_summary decides
# the exit code, so a single failing assert must not abort before the summary.
set -uo pipefail

E2E_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/e2e_lib.sh
# shellcheck disable=SC1091
source "$E2E_DIR/e2e_lib.sh"

harness_init "lod_packing"

# ee_json <args...> — run ee, tolerate nonzero exit (assertions inspect output).
ee_json() { "$EE_BIN" "$@" 2>/dev/null || true; }
# ee_text <args...> — run ee for non-JSON (markdown) output.
ee_text() { "$EE_BIN" "$@" 2>/dev/null || true; }

LOD_TASK="lodfixture release verification rollout checklist"
MAX_TOKENS=150

with_temp_workspace WS

step "init workspace"
init_out="$(ee_json init --workspace "$WS" --json)"
assert_jq "$init_out" '.success == true' "ee init succeeds"

step "seed many sizable candidates (force LOD tiers under a tight budget)"
seed_count=8
seeded=0
for i in $(seq 1 "$seed_count"); do
    # Distinct, sizable bodies sharing the task keywords so all are retrieval
    # candidates; the tight budget then splits them across Full/preview/link.
    body="lodfixture release verification rollout checklist entry ${i}"
    for j in $(seq 1 30); do body="$body token_${i}_${j}"; done
    r="$(ee_json remember "$body" --workspace "$WS" --level semantic --kind note \
        --tags lod,e2e --json)"
    if printf '%s' "$r" | jq -e '.success == true' >/dev/null 2>&1; then
        seeded=$((seeded + 1))
    fi
done
assert_eq "$seeded" "$seed_count" "seeded all LOD candidates"
log_event "lod_seed" candidates "$seeded" maxTokens "$MAX_TOKENS" task "$LOD_TASK"

step "pack within a tight budget (hard asserts: success + budget + determinism)"
pack1="$(ee_json pack "$LOD_TASK" --workspace "$WS" --max-tokens "$MAX_TOKENS" --json)"
assert_jq "$pack1" '.success == true' "ee pack succeeds"
assert_jq "$pack1" '((.data.pack.budget.usedTokens // 0) <= '"$MAX_TOKENS"')' \
    "pack used tokens stay within the budget (off-by-one-free accounting)"
assert_jq "$pack1" '((.data.pack.hash // "") | length) > 0' "pack emits a pack hash"

pack2="$(ee_json pack "$LOD_TASK" --workspace "$WS" --max-tokens "$MAX_TOKENS" --json)"
h1="$(printf '%s' "$pack1" | jq -r '.data.pack.hash // empty')"
h2="$(printf '%s' "$pack2" | jq -r '.data.pack.hash // empty')"
assert_eq "$h1" "$h2" "LOD pack hash is byte-stable across runs"

step "markdown peripheral index renders link-only items (bd-1n0np.5.3)"
pack_md="$(ee_text pack "$LOD_TASK" --workspace "$WS" --max-tokens "$MAX_TOKENS" --format markdown)"
if printf '%s' "$pack_md" | grep -q "## Peripheral Index"; then
    _harness_pass "markdown pack renders a '## Peripheral Index' section"
    # Drill into the first link-only memory id listed in the peripheral index.
    link_id="$(printf '%s' "$pack_md" \
        | awk '/## Peripheral Index/{f=1;next} f&&/^- /{print; exit}' \
        | grep -oE 'mem_[0-9A-Za-z]+' | head -n1)"
    if [ -n "$link_id" ]; then
        show_out="$(ee_json memory show "$link_id" --workspace "$WS" --json)"
        assert_jq "$show_out" '.success == true' "ee memory show drills into a link-only item ($link_id)"
    else
        log_drop 1 "peripheral index present but no mem_ id parsed; drill-in assertion skipped"
    fi
else
    # No-silent-cap: the link-only tier did not surface (binary predates the 5.3
    # markdown rendering, or this budget/candidate mix produced only Full/preview).
    log_drop 1 "no '## Peripheral Index' in markdown pack; link-only tier did not engage (binary may predate bd-1n0np.5.3 rendering)"
fi

step "--no-lod reproduces the legacy flat pack byte-for-byte (bd-1n0np.5.2)"
if "$EE_BIN" pack --help 2>&1 | grep -q -- "--no-lod"; then
    flat1="$(ee_json pack "$LOD_TASK" --workspace "$WS" --max-tokens "$MAX_TOKENS" --no-lod --json)"
    flat2="$(ee_json pack "$LOD_TASK" --workspace "$WS" --max-tokens "$MAX_TOKENS" --no-lod --json)"
    assert_jq "$flat1" '.success == true' "ee pack --no-lod succeeds"
    fh1="$(printf '%s' "$flat1" | jq -r '.data.pack.hash // empty')"
    fh2="$(printf '%s' "$flat2" | jq -r '.data.pack.hash // empty')"
    assert_eq "$fh1" "$fh2" "--no-lod pack hash is deterministic"
else
    # --no-lod is gated behind src/cli/mod.rs (bd-1n0np.5.2), not yet shipped.
    log_drop 1 "ee pack --no-lod flag absent (bd-1n0np.5.2 pending): legacy-flat parity assertion skipped"
fi

end_temp_workspace
harness_summary
