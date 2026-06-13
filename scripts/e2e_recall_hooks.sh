#!/usr/bin/env bash
# bd-u875s.5 — code-anchored recall + harness hook E2E (real binary).
#
# Scenario:
#   * scratch workspace with a real git repository and real ee database
#   * anchored memories for rule/failure/decision surfaces
#   * `ee recall --path` and `ee recall --diff` find the ranked anchors
#   * `ee hook claude-code --install` merges managed hooks into settings JSON
#   * generated PreToolUse command emits additionalContext from `ee recall`
#   * generated PostToolUse Bash-failure command appends a hook journal row
#
# NOTE: no `set -e`; the shared harness accumulates failures and emits a
# structured ee.test_event.v1 log plus summary.
set -uo pipefail

E2E_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
if [ -z "${EE_BIN:-}" ] && [ -n "${EE_BINARY:-}" ]; then
    export EE_BIN="$EE_BINARY"
fi
# shellcheck source=scripts/e2e_lib.sh
# shellcheck disable=SC1091
source "$E2E_DIR/e2e_lib.sh"

harness_init "recall_hooks"

if ! command -v jq >/dev/null 2>&1; then
    echo "recall_hooks: jq is required" >&2
    exit 3
fi

ee_json() { "$EE_BIN" "$@" 2>/dev/null || true; }
now_ms() { python3 -c 'import time; print(int(time.time()*1000))'; }

run_json_hook() {
    local label="$1" command="$2" payload="$3" stdout_path="$4" stderr_path="$5"
    local start rc elapsed
    start="$(now_ms)"
    printf '%s' "$payload" | bash -lc "$command" >"$stdout_path" 2>"$stderr_path"
    rc=$?
    elapsed=$(( $(now_ms) - start ))
    log_event "recall_hooks_step" step "$label" command "$label hook command" exit_code "$rc" elapsed_ms "$elapsed" \
        assertion "hook command completed"
    return "$rc"
}

with_temp_workspace WS

step "initialize scratch git workspace and ee database"
mkdir -p "$WS/src/db" "$WS/src/graph" "$WS/.claude"
printf '%s\n' 'pub struct DbConnection;' 'pub fn open_db() {}' >"$WS/src/db/mod.rs"
printf '%s\n' 'pub fn refresh_graph_snapshot() {}' >"$WS/src/graph/mod.rs"
git -C "$WS" init -b main >/dev/null 2>&1
git_init_rc=$?
assert_eq "$git_init_rc" "0" "scratch git repo initializes on main"
git -C "$WS" config user.email "recall-hooks@example.invalid" >/dev/null 2>&1
git -C "$WS" config user.name "Recall Hooks E2E" >/dev/null 2>&1
git -C "$WS" add src/db/mod.rs src/graph/mod.rs >/dev/null 2>&1
git -C "$WS" commit -m "seed recall hook fixture" >/dev/null 2>&1
git_commit_rc=$?
assert_eq "$git_commit_rc" "0" "scratch git seed commit succeeds"
init_out="$(ee_json init --workspace "$WS" --json)"
assert_jq "$init_out" '.success == true' "ee init succeeds"
log_event "recall_hooks_step" step init command "ee init" exit_code 0 elapsed_ms 0 \
    assertion "workspace initialized"

step "remember anchored memories"
rule_out="$(ee_json remember \
    "Always route storage-layer edits through RCH proof. anchor:path:src/db/mod.rs" \
    --workspace "$WS" --level procedural --kind rule --json)"
failure_out="$(ee_json remember \
    "A failed DB migration surfaced as linker pressure. anchor:path:src/db/mod.rs anchor:symbol:DbConnection" \
    --workspace "$WS" --level episodic --kind failure --json)"
decision_out="$(ee_json remember \
    "Graph snapshots own centrality derivation decisions. anchor:path:src/graph/mod.rs" \
    --workspace "$WS" --level semantic --kind decision --json)"
assert_jq "$rule_out" '.success == true' "anchored rule remembered"
assert_jq "$failure_out" '.success == true' "anchored failure remembered"
assert_jq "$decision_out" '.success == true' "anchored decision remembered"
log_event "recall_hooks_step" step remember command "ee remember x3" exit_code 0 elapsed_ms 0 \
    assertion "three anchored memories stored"

step "recall by path ranks storage anchors and excludes graph-only decision"
path_recall="$(ee_json recall --path "src/db/mod.rs" --workspace "$WS" --json)"
assert_jq "$path_recall" '.success == true' "recall --path succeeds"
assert_jq "$path_recall" '(.data.recall.items | length) >= 2' "recall --path returns DB anchors"
assert_jq "$path_recall" 'any(.data.recall.items[]; .contentPreview | contains("storage-layer"))' \
    "recall --path includes storage rule"
assert_jq "$path_recall" 'any(.data.recall.items[]; .contentPreview | contains("linker pressure"))' \
    "recall --path includes failure"
assert_jq "$path_recall" 'all(.data.recall.items[]; (.anchor.path // "") != "src/graph/mod.rs")' \
    "recall --path excludes graph-only memory"
log_event "recall_hooks_step" step recall_path command "ee recall --path" exit_code 0 elapsed_ms 0 \
    assertion "path selector ranked expected anchors"

step "recall by git diff resolves changed source paths"
printf '%s\n' 'pub fn changed_for_recall_hooks() {}' >>"$WS/src/db/mod.rs"
diff_recall="$(ee_json recall --diff HEAD --workspace "$WS" --json)"
assert_jq "$diff_recall" '.success == true' "recall --diff succeeds"
assert_jq "$diff_recall" 'any(.data.recall.items[]; .anchor.path == "src/db/mod.rs")' \
    "recall --diff finds changed DB path"
log_event "recall_hooks_step" step recall_diff command "ee recall --diff HEAD" exit_code 0 elapsed_ms 0 \
    assertion "diff selector maps to changed path"

step "install Claude Code hooks into fixture settings JSON"
HOOK_SETTINGS="$WS/.claude/settings.json"
printf '%s\n' '{"hooks":{"PreToolUse":[{"matcher":"Read","hooks":[{"type":"command","command":"echo external"}]}]}}' >"$HOOK_SETTINGS"
install_out="$(ee_json hook claude-code --settings-path "$HOOK_SETTINGS" --ee-binary "$EE_BIN" --install --workspace "$WS" --json)"
assert_jq "$install_out" '.success == true' "hook install succeeds"
assert_jq "$install_out" '.data.harnessInstall.schema == "ee.hook.harness_install.v1"' \
    "hook install report schema"
assert_jq "$install_out" '.data.harnessInstall.mode == "install"' "hook install mode"
assert_jq "$install_out" '.data.harnessInstall.writtenPaths | length == 1' \
    "hook install records written settings path"
assert_jq "$(cat "$HOOK_SETTINGS")" '.hooks.PreToolUse | length == 2' \
    "hook install preserves external PreToolUse entry"
assert_jq "$(cat "$HOOK_SETTINGS")" 'any(.hooks.PreToolUse[]; .eeManaged | contains("bd-u875s.4"))' \
    "hook install writes managed PreToolUse entry"
assert_jq "$(cat "$HOOK_SETTINGS")" 'any(.hooks.PostToolUse[]; .matcher == "Bash")' \
    "hook install writes Bash failure capture entry"
log_event "recall_hooks_step" step hook_install command "ee hook claude-code --install" exit_code 0 elapsed_ms 0 \
    assertion "settings JSON merged"

pre_cmd="$(jq -r '.hooks.PreToolUse[] | select((.eeManaged // "") | contains("bd-u875s.4")) | .hooks[0].command' "$HOOK_SETTINGS" | head -n 1)"
post_cmd="$(jq -r '.hooks.PostToolUse[] | select((.eeManaged // "") | contains("bd-u875s.4")) | .hooks[0].command' "$HOOK_SETTINGS" | head -n 1)"
assert_eq "$([ -n "$pre_cmd" ] && echo present || echo missing)" "present" \
    "managed PreToolUse command extracted"
assert_eq "$([ -n "$post_cmd" ] && echo present || echo missing)" "present" \
    "managed PostToolUse command extracted"

step "simulate Claude Code PreToolUse edit payload"
pre_payload="$(jq -nc --arg cwd "$WS" '{hook_event_name:"PreToolUse",tool_name:"Edit",cwd:$cwd,tool_input:{file_path:"src/db/mod.rs"}}')"
pre_stdout="$LOG_DIR/pre_tool_use.stdout.json"
pre_stderr="$LOG_DIR/pre_tool_use.stderr.txt"
run_json_hook "pre_tool_use" "$pre_cmd" "$pre_payload" "$pre_stdout" "$pre_stderr"
pre_rc=$?
pre_out="$(cat "$pre_stdout")"
assert_eq "$pre_rc" "0" "PreToolUse hook exits zero"
assert_jq "$pre_out" '.hookSpecificOutput.hookEventName == "PreToolUse"' \
    "PreToolUse hook emits Claude hookSpecificOutput"
assert_jq "$pre_out" '.hookSpecificOutput.additionalContext | contains("storage-layer")' \
    "PreToolUse hook injects recall markdown"
assert_jq "$pre_out" '.hookSpecificOutput.additionalContext | contains("linker pressure")' \
    "PreToolUse hook includes failure memory"

step "simulate Claude Code PostToolUse Bash failure payload"
post_payload="$(jq -nc --arg cwd "$WS" '{
    tool_name:"Bash",
    cwd:$cwd,
    tool_input:{command:"cargo test --lib"},
    tool_response:{exit_code:101, stderr:"compile failed in recall hook e2e"}
}')"
post_stdout="$LOG_DIR/post_tool_use.stdout.txt"
post_stderr="$LOG_DIR/post_tool_use.stderr.txt"
run_json_hook "post_tool_use" "$post_cmd" "$post_payload" "$post_stdout" "$post_stderr"
post_rc=$?
assert_eq "$post_rc" "0" "PostToolUse hook exits zero"
journal_out="$(ee_json journal list --workspace "$WS" --json)"
assert_jq "$journal_out" '.success == true' "journal list succeeds"
assert_jq "$journal_out" 'any(.data.entries[]; .source == "hook" and .kind == "command_failure")' \
    "PostToolUse hook records command_failure journal entry"
assert_jq "$journal_out" 'any(.data.entries[]; .structured.exitCode == 101)' \
    "journal sidecar records Bash exit code"
log_event "recall_hooks_step" step post_tool_use command "generated PostToolUse command" exit_code "$post_rc" elapsed_ms 0 \
    assertion "journal capture recorded"

harness_summary
