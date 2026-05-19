#!/usr/bin/env bash
# bd-3d6ko.7 - no-build e2e for `ee hook git-readiness`.
#
# This driver creates real temporary Git repositories and inspects their hook
# chains through the public CLI. It never runs Cargo, never stages or unstages
# files, and intentionally retains temporary artifacts for audit.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
EVENT_ROOT="${EE_HOOK_GIT_READINESS_EVENT_DIR:-${TMPDIR:-/tmp}/ee-hook-git-readiness-e2e}"
EVENT_LOG="$EVENT_ROOT/events.jsonl"
ASSERTS_PASS=0
ASSERTS_FAIL=0

now_ms() {
    python3 - <<'PY'
import time
print(int(time.time() * 1000))
PY
}

STARTED_MS="$(now_ms)"
mkdir -p "$EVENT_ROOT"
: > "$EVENT_LOG"

emit_event() {
    local scenario="${1:?scenario required}"
    local phase="${2:?phase required}"
    local status="${3:?status required}"
    local exit_code="${4:-0}"
    local command_text="${5:-}"
    local workspace="${6:-}"
    local stdout_artifact="${7:-}"
    local stderr_artifact="${8:-}"
    local first_failure="${9:-}"
    local degraded_codes="${10:-[]}"
    local elapsed_ms
    elapsed_ms="$(( $(now_ms) - STARTED_MS ))"

    jq -cn \
        --arg schema "ee.test_event.v1" \
        --arg bead_id "bd-3d6ko.7" \
        --arg surface "hook_git_readiness" \
        --arg scenario "$scenario" \
        --arg phase "$phase" \
        --arg status "$status" \
        --arg command "$command_text" \
        --arg workspace "$workspace" \
        --arg stdout_artifact "$stdout_artifact" \
        --arg stderr_artifact "$stderr_artifact" \
        --arg first_failure "$first_failure" \
        --arg event_log "$EVENT_LOG" \
        --arg ee_binary "${EE_BINARY:-}" \
        --argjson exit_code "$exit_code" \
        --argjson elapsed_ms "$elapsed_ms" \
        --argjson degraded_codes "$degraded_codes" \
        '{
          schema: $schema,
          beadId: $bead_id,
          surface: $surface,
          scenario: $scenario,
          phase: $phase,
          status: $status,
          elapsedMs: $elapsed_ms,
          exitCode: $exit_code,
          command: (if $command == "" then null else $command end),
          workspace: (if $workspace == "" then null else $workspace end),
          stdoutArtifact: (if $stdout_artifact == "" then null else $stdout_artifact end),
          stderrArtifact: (if $stderr_artifact == "" then null else $stderr_artifact end),
          firstFailureDiagnosis: (if $first_failure == "" then null else $first_failure end),
          degradedCodes: $degraded_codes,
          eventLog: $event_log,
          sanitizedEnv: {
            eeBinary: (if $ee_binary == "" then null else $ee_binary end)
          }
        }' | tee -a "$EVENT_LOG" >&2
}

require_tool() {
    local tool="${1:?tool required}"
    if ! command -v "$tool" >/dev/null 2>&1; then
        emit_event "preflight" "setup" "blocked" 2 "command -v $tool" "" "" "" "missing required tool: $tool" '["tool_unavailable"]'
        exit 2
    fi
}

resolve_ee_binary() {
    if [ -n "${EE_BINARY:-}" ]; then
        printf '%s\n' "$EE_BINARY"
        return 0
    fi
    if [ -n "${EE_BIN:-}" ]; then
        printf '%s\n' "$EE_BIN"
        return 0
    fi
    if [ -n "${CARGO_TARGET_DIR:-}" ]; then
        if [ -x "${CARGO_TARGET_DIR%/}/release/ee" ]; then
            printf '%s\n' "${CARGO_TARGET_DIR%/}/release/ee"
            return 0
        fi
        if [ -x "${CARGO_TARGET_DIR%/}/debug/ee" ]; then
            printf '%s\n' "${CARGO_TARGET_DIR%/}/debug/ee"
            return 0
        fi
    fi
    if [ -x "$REPO_ROOT/target/release/ee" ]; then
        printf '%s\n' "$REPO_ROOT/target/release/ee"
        return 0
    fi
    if [ -x "$REPO_ROOT/target/debug/ee" ]; then
        printf '%s\n' "$REPO_ROOT/target/debug/ee"
        return 0
    fi
    return 1
}

require_tool jq
require_tool git
require_tool mktemp
require_tool shasum

if ! EE_BINARY="$(resolve_ee_binary)"; then
    emit_event "preflight" "setup" "blocked" 2 "locate ee binary" "$REPO_ROOT" "" "" "set EE_BINARY to an existing ee binary; this no-build harness will not run cargo" '["ee_binary_unavailable"]'
    printf 'hook_git_readiness: set EE_BINARY to an existing ee binary; events=%s\n' "$EVENT_LOG" >&2
    exit 2
fi
export EE_BINARY

if [ ! -x "$EE_BINARY" ]; then
    emit_event "preflight" "setup" "blocked" 2 "$EE_BINARY --version" "$REPO_ROOT" "" "" "resolved EE_BINARY is not executable" '["ee_binary_unavailable"]'
    exit 2
fi

if [ -z "$("$EE_BINARY" --version 2>/dev/null || true)" ]; then
    emit_event "preflight" "setup" "blocked" 2 "$EE_BINARY --version" "$REPO_ROOT" "" "" "resolved EE_BINARY did not emit a version; set EE_BINARY to a current host-runnable binary" '["ee_binary_unavailable"]'
    exit 2
fi

WORK_ROOT="${EE_HOOK_GIT_READINESS_TMPROOT:-${TMPDIR:-/tmp}}"
case "$WORK_ROOT" in
    /Volumes/*) WORK_ROOT="/tmp" ;;
esac
mkdir -p "$WORK_ROOT"
WORK_DIR="$(mktemp -d "${WORK_ROOT%/}/ee-hook-git-readiness.XXXXXX")"
emit_event "setup" "workspace" "ok" 0 "mktemp -d" "$WORK_DIR" "" "" "temporary repositories retained by no-delete policy"

repo_status_hash() {
    git -C "$REPO_ROOT" status --porcelain=v2 --branch --untracked-files=all |
        shasum -a 256 |
        awk '{ print $1 }'
}

write_file() {
    local path="${1:?path required}"
    local body="${2:-}"
    mkdir -p "$(dirname "$path")"
    printf '%b' "$body" > "$path"
}

init_fixture_repo() {
    local name="${1:?name required}"
    local repo="$WORK_DIR/$name"
    mkdir -p "$repo"
    git -C "$repo" init -q --initial-branch=main
    printf '%s\n' "$repo"
}

run_readiness() {
    local scenario="${1:?scenario required}"
    local repo="${2:?repo required}"
    local agent_name="${3:-}"
    local stdout_artifact="$WORK_DIR/${scenario}.stdout.json"
    local stderr_artifact="$WORK_DIR/${scenario}.stderr.txt"
    local rc=0
    local command_text

    if [ -n "$agent_name" ]; then
        command_text="$EE_BINARY hook git-readiness --repository-root $repo --agent-name $agent_name --json"
        "$EE_BINARY" hook git-readiness \
            --repository-root "$repo" \
            --agent-name "$agent_name" \
            --json >"$stdout_artifact" 2>"$stderr_artifact" || rc=$?
    else
        command_text="AGENT_NAME= $EE_BINARY hook git-readiness --repository-root $repo --json"
        AGENT_NAME="" "$EE_BINARY" hook git-readiness \
            --repository-root "$repo" \
            --json >"$stdout_artifact" 2>"$stderr_artifact" || rc=$?
    fi

    if [ "$rc" -eq 0 ]; then
        emit_event "$scenario" "command" "ok" 0 "$command_text" "$repo" "$stdout_artifact" "$stderr_artifact"
    else
        emit_event "$scenario" "command" "failed" "$rc" "$command_text" "$repo" "$stdout_artifact" "$stderr_artifact" "ee hook git-readiness exited non-zero" '["command_failed"]'
    fi
    printf '%s\n' "$stdout_artifact"
    return "$rc"
}

assert_jq() {
    local json_file="${1:?json file required}"
    local filter="${2:?jq filter required}"
    local expected="${3:?expected value required}"
    local label="${4:?label required}"
    local got
    got="$(jq -r "$filter" "$json_file" 2>/dev/null || true)"
    if [ "$got" = "$expected" ]; then
        ASSERTS_PASS=$((ASSERTS_PASS + 1))
        emit_event "$label" "assert" "ok" 0 "jq -r $filter $json_file"
    else
        ASSERTS_FAIL=$((ASSERTS_FAIL + 1))
        emit_event "$label" "assert" "failed" 1 "jq -r $filter $json_file" "" "$json_file" "" "expected '$expected' got '${got:-<empty>}'" '["assertion_failed"]'
    fi
}

BEFORE_REPO_HASH="$(repo_status_hash)"

legacy_repo="$(init_fixture_repo legacy_beads_missing_agent)"
write_file "$legacy_repo/.git/hooks/pre-commit" '#!/usr/bin/env python3
HOOK_DIR = Path(__file__).parent
RUN_DIR = HOOK_DIR / "hooks.d" / "pre-commit"
ORIG = HOOK_DIR / "pre-commit.orig"
'
# shellcheck disable=SC2016 # Fixture content must preserve hook-time variables.
write_file "$legacy_repo/.git/hooks/pre-commit.orig" '#!/bin/sh
bd sync --flush-only
git add "$BEADS_DIR/issues.jsonl"
'
write_file "$legacy_repo/.git/hooks/pre-push" '#!/usr/bin/env python3
import os
AGENT_NAME = os.environ.get("AGENT_NAME", "").strip()
if not AGENT_NAME:
    print("mcp-agent-mail: AGENT_NAME environment variable is required.")
'
legacy_json="$(run_readiness legacy_beads_missing_agent "$legacy_repo" "")"
assert_jq "$legacy_json" '.schema' "ee.response.v2" "legacy_response_schema"
assert_jq "$legacy_json" '.data.report.schema' "ee.hooks.git_readiness.v1" "legacy_report_schema"
assert_jq "$legacy_json" '.data.report.readOnly' "true" "legacy_read_only"
assert_jq "$legacy_json" '.data.report.summary.posture' "blocked" "legacy_posture_blocked"
assert_jq "$legacy_json" '[.data.report.findings[].code] | index("agent_name_required") != null' "true" "legacy_agent_name_finding"
assert_jq "$legacy_json" '[.data.report.findings[].code] | index("beads_metadata_mutation_risk") != null' "true" "legacy_beads_finding"
assert_jq "$legacy_json" '.data.report.hooks[] | select(.name == "pre-commit") | (.chainTargets | any(endswith("pre-commit.orig")))' "true" "legacy_orig_chain_target"

clean_repo="$(init_fixture_repo clean_configured_chain)"
write_file "$clean_repo/.git/hooks/pre-commit" '#!/bin/sh
/usr/local/bin/ee preflight check --cmd "$*" --json
'
write_file "$clean_repo/.git/hooks/pre-push" '#!/usr/bin/env python3
import os
AGENT_NAME = os.environ.get("AGENT_NAME", "").strip()
'
clean_json="$(run_readiness clean_configured_chain "$clean_repo" "LilacLake")"
assert_jq "$clean_json" '.data.report.summary.posture' "ready" "clean_posture_ready"
assert_jq "$clean_json" '.data.report.summary.agentNameReady' "true" "clean_agent_name_ready"
assert_jq "$clean_json" '.data.report.summary.preflightGuardReachable' "true" "clean_preflight_reachable"
assert_jq "$clean_json" '.data.report.findings | length' "0" "clean_no_findings"

rch_repo="$(init_fixture_repo rch_mismatch)"
write_file "$rch_repo/.git/hooks/pre-commit" '#!/bin/sh
cargo check --all-targets
'
rch_json="$(run_readiness rch_mismatch "$rch_repo" "LilacLake")"
assert_jq "$rch_json" '.data.report.summary.posture' "blocked" "rch_mismatch_posture_blocked"
assert_jq "$rch_json" '.data.report.hooks[] | select(.name == "pre-commit") | .invokesLocalRustToolchain' "true" "rch_mismatch_local_rust_detected"
assert_jq "$rch_json" '.data.report.hooks[] | select(.name == "pre-commit") | .invokesRch' "false" "rch_mismatch_no_rch"
assert_jq "$rch_json" '[.data.report.findings[].code] | index("rch_hook_mismatch") != null' "true" "rch_mismatch_finding"

AFTER_REPO_HASH="$(repo_status_hash)"
if [ "$BEFORE_REPO_HASH" = "$AFTER_REPO_HASH" ]; then
    ASSERTS_PASS=$((ASSERTS_PASS + 1))
    emit_event "caller_checkout" "assert" "ok" 0 "git status hash unchanged" "$REPO_ROOT"
else
    ASSERTS_FAIL=$((ASSERTS_FAIL + 1))
    emit_event "caller_checkout" "assert" "failed" 1 "git status hash unchanged" "$REPO_ROOT" "" "" "caller checkout status hash changed" '["caller_checkout_mutated"]'
fi

if [ "$ASSERTS_FAIL" -ne 0 ]; then
    emit_event "summary" "complete" "failed" 3 "" "$WORK_DIR" "" "" "$ASSERTS_FAIL assertion(s) failed" '["assertion_failed"]'
    printf 'hook_git_readiness: %s passed, %s failed; artifacts retained at %s; events=%s\n' "$ASSERTS_PASS" "$ASSERTS_FAIL" "$WORK_DIR" "$EVENT_LOG" >&2
    exit 3
fi

emit_event "summary" "complete" "ok" 0 "" "$WORK_DIR"
printf 'hook_git_readiness: %s assertions passed; artifacts retained at %s; events=%s\n' "$ASSERTS_PASS" "$WORK_DIR" "$EVENT_LOG" >&2
