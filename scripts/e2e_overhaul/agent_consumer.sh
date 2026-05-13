#!/usr/bin/env bash
# J8 - reference agent-consumer contract test.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
DEFAULT_AGENT_BUILD_ROOT="/Volumes/USBNVME16TB/temp_agent_space"

if [ -d "$DEFAULT_AGENT_BUILD_ROOT" ]; then
    mkdir -p "$DEFAULT_AGENT_BUILD_ROOT/cargo-target" "$DEFAULT_AGENT_BUILD_ROOT/tmp" 2>/dev/null || true
    export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$DEFAULT_AGENT_BUILD_ROOT/cargo-target}"
    export TMPDIR="${EE_AGENT_TMPDIR:-$DEFAULT_AGENT_BUILD_ROOT/tmp}"
fi

if [ -n "${CARGO_TARGET_DIR:-}" ]; then
    EE_BINARY="${EE_BINARY:-${CARGO_TARGET_DIR%/}/release/ee}"
else
    EE_BINARY="${EE_BINARY:-$REPO_ROOT/target/release/ee}"
fi

CONSUMER="$REPO_ROOT/scripts/agent_consume_pack.py"
CORPUS_SEED="$REPO_ROOT/tests/fixtures/corpus/corpus_2026_05_10_seed.sh"
WORKSPACE="$(mktemp -d "${TMPDIR:-/tmp}/ee-e2e-agent-consumer.XXXXXX")"
PACK_JSON="$WORKSPACE/pack.json"
PROMPT_FRAGMENT="$WORKSPACE/prompt_fragment.md"

# shellcheck source=scripts/lib/e2e_logger.sh
source "$REPO_ROOT/scripts/lib/e2e_logger.sh"
e2e_log_start "j8_agent_consumer"
e2e_log_note "workspace_left_for_inspection=$WORKSPACE"

fail() {
    e2e_log_assert_eq "$1" "$2" "$3" >/dev/null 2>&1 || true
}

if [ ! -x "$EE_BINARY" ]; then
    echo "j8: ee binary not executable at $EE_BINARY" >&2
    fail "missing_binary" "ok" "j8_ee_binary_executable"
    e2e_log_end
    exit 2
fi

if [ ! -x "$CONSUMER" ]; then
    echo "j8: consumer not executable at $CONSUMER" >&2
    fail "missing_consumer" "ok" "j8_consumer_executable"
    e2e_log_end
    exit 2
fi

"$EE_BINARY" init --workspace "$WORKSPACE" --json >/dev/null
CORPUS_TOLERATE_REJECT=1 EE_BINARY="$EE_BINARY" "$CORPUS_SEED" "$WORKSPACE" >/dev/null 2>&1 || true

if "$EE_BINARY" context "prepare release" \
    --workspace "$WORKSPACE" --max-tokens 1000 --json > "$PACK_JSON" 2>"$WORKSPACE/context.stderr"; then
    e2e_log_assert_eq "0" "0" "j8_context_json_command_succeeded" || true
else
    fail "context_failed" "ok" "j8_context_json_command_succeeded"
fi

if "$CONSUMER" --from-stdin < "$PACK_JSON" > "$PROMPT_FRAGMENT"; then
    e2e_log_assert_eq "0" "0" "j8_consumer_command_succeeded" || true
else
    fail "consumer_failed" "ok" "j8_consumer_command_succeeded"
fi

if [ -s "$PROMPT_FRAGMENT" ]; then
    e2e_log_assert_eq "nonempty" "nonempty" "j8_prompt_fragment_nonempty" || true
else
    fail "empty" "nonempty" "j8_prompt_fragment_nonempty"
fi

if grep -q "# Context Pack:" "$PROMPT_FRAGMENT"; then
    e2e_log_assert_eq "present" "present" "j8_prompt_fragment_has_title" || true
else
    fail "missing" "present" "j8_prompt_fragment_has_title"
fi

BYTES="$(wc -c < "$PROMPT_FRAGMENT" | tr -d ' ')"
e2e_log_assert_num "$BYTES" -le 8000 "j8_prompt_fragment_max_bytes" || true

if grep -q '\\\.' "$PROMPT_FRAGMENT"; then
    fail "escape_leakage" "none" "j8_prompt_fragment_no_dot_escape_leakage"
else
    e2e_log_assert_eq "none" "none" "j8_prompt_fragment_no_dot_escape_leakage" || true
fi

MEANINGFUL_LINES="$(grep -Ev '^[[:space:]]*(#.*)?$' "$CONSUMER" | wc -l | tr -d ' ')"
e2e_log_assert_num "$MEANINGFUL_LINES" -le 50 "j8_consumer_meaningful_lines_le_50" || true

e2e_log_end
if [ "$EE_TEST_LOG_ASSERTS_FAIL" -gt 0 ]; then
    exit 3
fi
exit 0
