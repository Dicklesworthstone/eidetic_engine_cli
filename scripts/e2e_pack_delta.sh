#!/usr/bin/env bash
# bd-7lvbg.6 — pack delta ergonomics E2E (real binary).
#
# Proves the `--since last` per-agent baseline ledger and the markdown
# delta rendering end to end:
#
#   * A persisted pack records this agent's baseline automatically
#     (EE_AGENT_NAME identity).
#   * After one remember (added) and one memory expire (removed),
#     `--since last --json` emits an ee.context.delta.v2 envelope whose
#     priorPackHash is exactly the recorded baseline, with the new
#     memory in items.added and the expired one in items.removed.
#   * `--since last --format markdown` emits the delta document:
#     added/changed/removed sections present, and an unchanged seeded
#     memory's content is NOT re-emitted.
#   * A different EE_AGENT_NAME has an independent (empty) baseline and
#     degrades honestly with context_delta_no_baseline, as does a run
#     with no agent identity at all.
#   * `--no-baseline-write` persists the pack but leaves the ledger
#     untouched (the next `--since last` still resolves the old
#     baseline).
#
# Delta-asserting runs use --read-only so they never advance the
# baseline mid-scenario. Every step logs ee.test_event.v1 events; the
# harness owns the exit code.
#
# NOTE: no `set -e` — assert_* accumulate and harness_summary decides.
set -uo pipefail

E2E_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/e2e_lib.sh
# shellcheck disable=SC1091
source "$E2E_DIR/e2e_lib.sh"

harness_init "pack_delta"

if ! command -v jq >/dev/null 2>&1; then
    echo "pack_delta: jq is required" >&2
    exit 3
fi

ee_json() { "$EE_BIN" "$@" 2>/dev/null || true; }
now_ms() { python3 -c 'import time; print(int(time.time()*1000))'; }

QUERY="release workflow clippy gates"

with_temp_workspace WS

step "init + seed a small corpus"
init_out="$(ee_json --workspace "$WS" init --json)"
assert_jq "$init_out" '.success == true' "ee init succeeds"
for i in 1 2 3 4; do
    seed_out="$(ee_json --workspace "$WS" remember \
        "Pack delta seed row $i: the release workflow runs clippy gates before any tag." \
        --level semantic --kind fact --tags delta,e2e --json)"
    assert_jq "$seed_out" '.success == true' "seed memory $i remembered"
done

step "AgentOne pack persists and records the baseline"
t0="$(now_ms)"
pack1="$(EE_AGENT_NAME=AgentOne ee_json --workspace "$WS" pack "$QUERY" --max-tokens 2000 --json)"
pack1_ms=$(( $(now_ms) - t0 ))
assert_jq "$pack1" '.success == true' "pack 1 succeeds"
pack1_hash="$(printf '%s' "$pack1" | jq -r '.data.pack.hash // empty')"
assert_eq "$([ -n "$pack1_hash" ] && echo present || echo missing)" "present" \
    "pack 1 carries data.pack.hash"
log_event "pack_delta_baseline" step baseline cmd "pack" exit 0 ms "$pack1_ms" \
    assertion "baseline recorded for AgentOne" hash "${pack1_hash:0:18}"

step "mutate the corpus: one added memory, one expired memory"
added_out="$(ee_json --workspace "$WS" remember \
    "Pack delta added row: clippy gate waivers now require a signed release ticket." \
    --level semantic --kind fact --tags delta,e2e --json)"
assert_jq "$added_out" '.success == true' "added memory remembered"
expire_id="$(ee_json --workspace "$WS" memory list --limit 1 --json | jq -r '.data.memories[0].id // empty')"
assert_eq "$([ -n "$expire_id" ] && echo present || echo missing)" "present" \
    "found a seeded memory id to expire"
expire_out="$(ee_json --workspace "$WS" memory expire "$expire_id" --json)"
assert_jq "$expire_out" '.success == true' "memory expire succeeds"
log_event "pack_delta_mutate" step mutate cmd "remember+expire" exit 0 ms 0 \
    assertion "corpus changed" expired "$expire_id"

step "--since last --json emits the delta against the recorded baseline"
t1="$(now_ms)"
delta_json="$(EE_AGENT_NAME=AgentOne ee_json --workspace "$WS" pack "$QUERY" --max-tokens 2000 --since last --read-only --json)"
delta_ms=$(( $(now_ms) - t1 ))
assert_jq "$delta_json" '.schema == "ee.context.delta.v2"' \
    "since-last response is an ee.context.delta.v2 envelope"
assert_jq "$delta_json" '.data.serverDecision.computedFromServerVerifiedPackRecord == true' \
    "real CLI delta marks its centrally verified persisted prior"
assert_json "$delta_json" '.data.priorPackHash' "$pack1_hash" \
    "delta priorPackHash equals the recorded baseline hash"
assert_jq "$delta_json" '(.data.items.added | length) >= 1' \
    "delta reports the added memory"
assert_jq "$delta_json" '(.data.items.removed | length) >= 1' \
    "delta reports the expired memory"
log_event "pack_delta_json" step delta_json cmd "pack --since last --json" exit 0 ms "$delta_ms" \
    assertion "added+removed present" \
    added "$(printf '%s' "$delta_json" | jq -r '.data.items.added | length')" \
    removed "$(printf '%s' "$delta_json" | jq -r '.data.items.removed | length')"

step "--since last --format markdown emits the delta document"
t2="$(now_ms)"
delta_md="$(EE_AGENT_NAME=AgentOne "$EE_BIN" --workspace "$WS" pack "$QUERY" --max-tokens 2000 --since last --read-only --format markdown 2>/dev/null || true)"
md_ms=$(( $(now_ms) - t2 ))
assert_contains "$delta_md" "# context delta" "markdown delta header present"
assert_contains "$delta_md" "## added (" "markdown added section present"
assert_contains "$delta_md" "## removed (" "markdown removed section present"
if printf '%s' "$delta_md" | grep -qF "Pack delta seed row 2"; then
    _harness_fail "markdown delta re-emitted an unchanged memory's content"
else
    _harness_pass "markdown delta does not re-emit unchanged content"
fi
log_event "pack_delta_markdown" step delta_markdown cmd "pack --since last --format markdown" \
    exit 0 ms "$md_ms" assertion "delta document sections present" bytes "${#delta_md}"

step "a different agent has an independent baseline (honest no-baseline fallback)"
other="$(EE_AGENT_NAME=AgentTwo ee_json --workspace "$WS" pack "$QUERY" --max-tokens 2000 --since last --read-only --json)"
assert_jq "$other" '.schema == "ee.response.v2"' "AgentTwo falls back to the full pack"
assert_jq "$other" '[(.degraded[]?, .data.degraded[]?) | .code] | index("context_delta_no_baseline") != null' \
    "AgentTwo reports context_delta_no_baseline"
log_event "pack_delta_isolation" step isolation cmd "pack --since last (AgentTwo)" exit 0 ms 0 \
    assertion "independent per-agent baseline"

step "no agent identity degrades honestly"
anon="$(env -u EE_AGENT_NAME "$EE_BIN" --workspace "$WS" pack "$QUERY" --max-tokens 2000 --since last --read-only --json 2>/dev/null || true)"
assert_jq "$anon" '[(.degraded[]?, .data.degraded[]?) | .code] | index("context_delta_no_baseline") != null' \
    "unset EE_AGENT_NAME reports context_delta_no_baseline"
log_event "pack_delta_anon" step anon cmd "pack --since last (no identity)" exit 0 ms 0 \
    assertion "no-identity fallback honest"

step "--no-baseline-write persists the pack but never touches the ledger"
nbw="$(EE_AGENT_NAME=AgentOne ee_json --workspace "$WS" pack "$QUERY" --max-tokens 2000 --no-baseline-write --json)"
assert_jq "$nbw" '.success == true or .schema == "ee.context.delta.v2"' "no-baseline-write pack succeeds"
after="$(EE_AGENT_NAME=AgentOne ee_json --workspace "$WS" pack "$QUERY" --max-tokens 2000 --since last --read-only --json)"
after_prior="$(printf '%s' "$after" | jq -r '.data.priorPackHash // empty')"
assert_eq "$after_prior" "$pack1_hash" \
    "baseline still resolves to pack 1 after a --no-baseline-write pack"
log_event "pack_delta_no_write" step no_baseline_write cmd "pack --no-baseline-write" exit 0 ms 0 \
    assertion "ledger untouched" prior "${after_prior:0:18}"

end_temp_workspace
harness_summary
