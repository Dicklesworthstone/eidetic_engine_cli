#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
RUNNER="$REPO_ROOT/scripts/swarm-slo-replay.sh"
FIXTURE="$REPO_ROOT/tests/fixtures/swarm_slo_replay/read_only_trace.jsonl"
EXPECTED="$REPO_ROOT/tests/fixtures/swarm_slo_replay/read_only_trace.replayed.jsonl"
TMP_ROOT="${TMPDIR:-/tmp}/ee-swarm-slo-replay.$$"
OUT_ONE="$TMP_ROOT/replay-one.jsonl"
OUT_TWO="$TMP_ROOT/replay-two.jsonl"
SUMMARY="$TMP_ROOT/summary.json"

mkdir -p "$TMP_ROOT"

if ! command -v jq >/dev/null 2>&1; then
    printf 'swarm_slo_replay: jq is required\n' >&2
    exit 2
fi

bash -n "$RUNNER"

"$RUNNER" --input "$FIXTURE" --output "$OUT_ONE" --summary "$SUMMARY" --verify-determinism
"$RUNNER" --input "$FIXTURE" --output "$OUT_TWO" --verify-determinism

diff -u "$EXPECTED" "$OUT_ONE"
diff -u "$OUT_ONE" "$OUT_TWO"

TIE_ONE="$TMP_ROOT/event-index-tie-one.jsonl"
TIE_TWO="$TMP_ROOT/event-index-tie-two.jsonl"
TIE_OUT_ONE="$TMP_ROOT/event-index-tie-one.out.jsonl"
TIE_OUT_TWO="$TMP_ROOT/event-index-tie-two.out.jsonl"
cat >"$TIE_ONE" <<'EOF'
{"schema":"ee.test_event.v1","eventIndex":7,"phase":"parallel","kind":"command_end","surface":"swarm_slo_replay","agentId":"cod_a","elapsedMs":11,"degradedCodes":[]}
{"schema":"ee.test_event.v1","eventIndex":7,"phase":"parallel","kind":"command_end","surface":"swarm_slo_replay","agentId":"cod_b","elapsedMs":13,"degradedCodes":[]}
EOF
cat >"$TIE_TWO" <<'EOF'
{"schema":"ee.test_event.v1","eventIndex":7,"phase":"parallel","kind":"command_end","surface":"swarm_slo_replay","agentId":"cod_b","elapsedMs":13,"degradedCodes":[]}
{"schema":"ee.test_event.v1","eventIndex":7,"phase":"parallel","kind":"command_end","surface":"swarm_slo_replay","agentId":"cod_a","elapsedMs":11,"degradedCodes":[]}
EOF
"$RUNNER" --input "$TIE_ONE" --output "$TIE_OUT_ONE" --verify-determinism
"$RUNNER" --input "$TIE_TWO" --output "$TIE_OUT_TWO" --verify-determinism
diff -u "$TIE_OUT_ONE" "$TIE_OUT_TWO"

jq -e '
  .schema == "ee.swarm_slo.replay.v1"
  and .eventCount == 4
  and .dryRunOnly == true
  and .mutationExecuted == false
  and .deterministic == true
  and (.mutatingCommandRows | length) == 0
  and .schemaCounts["ee.test_event.v1"] == 3
  and .schemaCounts["ee.agent_workload_trace.v1"] == 1
' "$SUMMARY" >/dev/null

jq -cn \
    --arg schema "ee.test_event.v1" \
    --arg surface "swarm_slo_replay" \
    --arg phase "verdict" \
    --arg kind "note" \
    --arg summaryHash "$(shasum -a 256 "$SUMMARY" | awk '{print "sha256:" $1}')" \
    '{
      schema: $schema,
      surface: $surface,
      phase: $phase,
      kind: $kind,
      verdict: "pass",
      dryRunOnly: true,
      mutationExecuted: false,
      summaryHash: $summaryHash
    }'
