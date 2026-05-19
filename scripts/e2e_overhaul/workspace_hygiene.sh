#!/usr/bin/env bash
# Logged e2e driver for bd-1eq3l.8.
#
# This script exercises the public `ee workspace hygiene` surface against
# isolated temporary git workspaces. It never builds `ee`, never invokes Cargo,
# and never mutates the caller checkout beyond reading its git state.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
EVENT_ROOT="${EE_WORKSPACE_HYGIENE_EVENT_DIR:-${TMPDIR:-/tmp}/ee-workspace-hygiene-e2e}"
EVENT_LOG="$EVENT_ROOT/events.jsonl"
SYNTHETIC_RAW_SECRET="sk-proj-$(printf 'B%.0s' {1..40})"

now_ns() {
    local seconds
    seconds="$(date +%s)"
    printf '%s000000000\n' "$seconds"
}

STARTED_NS="$(now_ns)"

mkdir -p "$EVENT_ROOT"
: > "$EVENT_LOG"

emit_event() {
    local scenario="${1:?scenario required}"
    local phase="${2:?phase required}"
    local status="${3:?status required}"
    local exit_code="${4:?exit code required}"
    local command_text="${5:-}"
    local workspace="${6:-}"
    local stdout_artifact="${7:-}"
    local stderr_artifact="${8:-}"
    local schema_status="${9:-not_run}"
    local first_failure="${10:-}"
    local degraded_codes="${11:-[]}"
    local before_hash="${12:-}"
    local after_hash="${13:-}"
    local before_artifact="${14:-}"
    local after_artifact="${15:-}"
    local finished_ns elapsed_ms
    finished_ns="$(now_ns)"
    elapsed_ms="$(( (finished_ns - STARTED_NS) / 1000000 ))"

    jq -cn \
        --arg schema "ee.test_event.v1" \
        --arg bead_id "bd-1eq3l.8" \
        --arg surface "workspace_hygiene" \
        --arg scenario "$scenario" \
        --arg phase "$phase" \
        --arg status "$status" \
        --arg command "$command_text" \
        --arg workspace "$workspace" \
        --arg stdout_artifact "$stdout_artifact" \
        --arg stderr_artifact "$stderr_artifact" \
        --arg schema_status "$schema_status" \
        --arg first_failure "$first_failure" \
        --arg before_hash "$before_hash" \
        --arg after_hash "$after_hash" \
        --arg before_artifact "$before_artifact" \
        --arg after_artifact "$after_artifact" \
        --arg tmp_root "$EVENT_ROOT" \
        --arg cargo_target_dir "${CARGO_TARGET_DIR:-}" \
        --arg tmpdir "${TMPDIR:-}" \
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
          command: (if $command == "" then null else $command end),
          workspace: (if $workspace == "" then null else $workspace end),
          elapsedMs: $elapsed_ms,
          exitCode: $exit_code,
          schemaValidationStatus: $schema_status,
          stdoutArtifact: (if $stdout_artifact == "" then null else $stdout_artifact end),
          stderrArtifact: (if $stderr_artifact == "" then null else $stderr_artifact end),
          firstFailureDiagnosis: (if $first_failure == "" then null else $first_failure end),
          degradedCodes: $degraded_codes,
          beforeMutationHash: (if $before_hash == "" then null else $before_hash end),
          afterMutationHash: (if $after_hash == "" then null else $after_hash end),
          beforeMutationArtifact: (if $before_artifact == "" then null else $before_artifact end),
          afterMutationArtifact: (if $after_artifact == "" then null else $after_artifact end),
          sanitizedEnv: {
            tmpRoot: $tmp_root,
            tmpdir: (if $tmpdir == "" then null else $tmpdir end),
            cargoTargetDir: (if $cargo_target_dir == "" then null else $cargo_target_dir end),
            eeBinary: (if $ee_binary == "" then null else $ee_binary end)
          }
        }' | tee -a "$EVENT_LOG" >&2
}

require_tool() {
    local tool="${1:?tool required}"
    if ! command -v "$tool" >/dev/null 2>&1; then
        emit_event "preflight" "setup" "blocked" 2 "command -v $tool" "" "" "" "not_run" "missing required tool: $tool" '["tool_unavailable"]'
        exit 2
    fi
}

require_tool jq
require_tool git
require_tool shasum
require_tool mktemp

if [ -z "${EE_BINARY:-}" ]; then
    if [ -n "${EE_BIN:-}" ]; then
        EE_BINARY="$EE_BIN"
    elif [ -n "${CARGO_TARGET_DIR:-}" ] && [ -x "${CARGO_TARGET_DIR%/}/debug/ee" ]; then
        EE_BINARY="${CARGO_TARGET_DIR%/}/debug/ee"
    elif [ -n "${CARGO_TARGET_DIR:-}" ] && [ -x "${CARGO_TARGET_DIR%/}/release/ee" ]; then
        EE_BINARY="${CARGO_TARGET_DIR%/}/release/ee"
    elif [ -x "$REPO_ROOT/target/debug/ee" ]; then
        EE_BINARY="$REPO_ROOT/target/debug/ee"
    fi
fi
export EE_BINARY

if [ -z "${EE_BINARY:-}" ] || [ ! -x "$EE_BINARY" ]; then
    emit_event "preflight" "setup" "blocked" 2 "locate ee binary" "$REPO_ROOT" "" "" "not_run" "set EE_BINARY to an existing ee binary; this script will not run cargo" '["ee_binary_unavailable"]'
    printf 'workspace_hygiene: set EE_BINARY to an existing ee binary; events=%s\n' "$EVENT_LOG" >&2
    exit 2
fi

WORK_ROOT="${EE_WORKSPACE_HYGIENE_TMPROOT:-${TMPDIR:-/tmp}}"
case "$WORK_ROOT" in
    /Volumes/*) WORK_ROOT="/tmp" ;;
esac
mkdir -p "$WORK_ROOT"

hash_file() {
    shasum -a 256 "$1" | awk '{ print $1 }'
}

capture_repo_state() {
    local label="${1:?label required}"
    local artifact="$EVENT_ROOT/${label}_repo_state.txt"
    (
        cd "$REPO_ROOT"
        printf '## git status --porcelain=v2 --branch --untracked-files=all\n'
        git status --porcelain=v2 --branch --untracked-files=all
        printf '\n## git diff --name-status\n'
        git diff --name-status
        printf '\n## git diff --cached --name-status\n'
        git diff --cached --name-status
        printf '\n## git ls-files --others --exclude-standard\n'
        git ls-files --others --exclude-standard
    ) > "$artifact"
    printf '%s\t%s\n' "$(hash_file "$artifact")" "$artifact"
}

capture_workspace_state() {
    local workspace="${1:?workspace required}"
    local label="${2:?label required}"
    local artifact="$EVENT_ROOT/${label}_workspace_state.txt"
    (
        cd "$workspace"
        printf '## git status --porcelain=v2 --branch --untracked-files=all\n'
        git status --porcelain=v2 --branch --untracked-files=all
        printf '\n## tracked files\n'
        git ls-files --stage
        printf '\n## untracked files\n'
        git ls-files --others --exclude-standard
    ) > "$artifact"
    printf '%s\t%s\n' "$(hash_file "$artifact")" "$artifact"
}

write_file() {
    local path="${1:?path required}"
    local body="${2:-}"
    mkdir -p "$(dirname "$path")"
    printf '%b' "$body" > "$path"
}

init_git_workspace() {
    local scenario="${1:?scenario required}"
    local workspace
    workspace="$(mktemp -d "$WORK_ROOT/ee-workspace-hygiene-${scenario}.XXXXXX")"
    git init -b main "$workspace" >/dev/null
    write_file "$workspace/README.md" "# hygiene fixture\n"
    git -C "$workspace" add README.md
    git -C "$workspace" -c user.email=ee-test@example.invalid -c user.name="ee test" commit -m "seed fixture" >/dev/null
    printf '%s\n' "$workspace"
}

run_hygiene() {
    local scenario="${1:?scenario required}"
    local workspace="${2:?workspace required}"
    local snapshot="${3:-}"
    local stdout_artifact="$EVENT_ROOT/${scenario}_stdout.json"
    local stderr_artifact="$EVENT_ROOT/${scenario}_stderr.log"
    local command_text
    local -a args
    args=(--json workspace hygiene --agent-name SapphireElk --workspace "$workspace")
    if [ -n "$snapshot" ]; then
        args+=(--agent-mail-snapshot "$snapshot")
    fi
    command_text="$EE_BINARY ${args[*]}"

    set +e
    "$EE_BINARY" "${args[@]}" >"$stdout_artifact" 2>"$stderr_artifact"
    local exit_code=$?
    set -e

    printf '%s\t%s\t%s\t%s\n' "$exit_code" "$stdout_artifact" "$stderr_artifact" "$command_text"
}

jq_value() {
    local file="${1:?file required}"
    local filter="${2:?filter required}"
    jq -r "$filter" "$file"
}

assert_jq() {
    local file="${1:?file required}"
    local filter="${2:?filter required}"
    local message="${3:?message required}"
    if ! jq -e "$filter" "$file" >/dev/null; then
        printf '%s\n' "$message"
        return 1
    fi
}

assert_no_secret() {
    local label="${1:?label required}"
    local file="${2:?file required}"
    local secret="${3:?secret required}"
    if grep -F "$secret" "$file" >/dev/null 2>&1; then
        printf '%s leaked raw synthetic secret\n' "$label"
        return 1
    fi
}

run_scenario() {
    local scenario="${1:?scenario required}"
    local workspace snapshot before_hash before_artifact after_hash after_artifact
    workspace="$(init_git_workspace "$scenario")"
    snapshot=""

    case "$scenario" in
        clean)
            ;;
        source_and_test)
            write_file "$workspace/src/lib.rs" "pub fn changed() -> bool { true }\n"
            write_file "$workspace/tests/workspace_hygiene.rs" "#[test]\nfn fixture() {}\n"
            ;;
        scratch_generated_secret)
            write_file "$workspace/drift-report.txt" "local diagnostic output\n"
            write_file "$workspace/Cargo.lock" "generated lockfile placeholder\n"
            write_file "$workspace/.env.local" "OPENAI_API_KEY=$SYNTHETIC_RAW_SECRET\n"
            ;;
        active_reservation)
            write_file "$workspace/src/lib.rs" "pub fn reserved() -> bool { true }\n"
            snapshot="$workspace/agent-mail-snapshot.json"
            write_file "$snapshot" '{
  "file_reservations": [
    {
      "path_pattern": "src/lib.rs",
      "holder": "OtherAgent",
      "exclusive": true,
      "expires_at": "2099-01-01T00:00:00Z"
    }
  ],
  "active_agents": [
    {"name": "OtherAgent", "last_active_at": "2026-05-19T00:00:00Z"}
  ],
  "inbox": [],
  "threads": []
}
'
            ;;
        agent_mail_unavailable)
            write_file "$workspace/src/lib.rs" "pub fn changed() -> bool { true }\n"
            ;;
        beads_pending_flush)
            mkdir -p "$workspace/.beads"
            write_file "$workspace/.beads/.gitignore" "*.db\nlast-touched\n"
            write_file "$workspace/.beads/issues.jsonl" '{"id":"bd-public","title":"seed"}\n'
            git -C "$workspace" add .beads/.gitignore .beads/issues.jsonl
            git -C "$workspace" -c user.email=ee-test@example.invalid -c user.name="ee test" commit -m "seed beads metadata" >/dev/null
            sleep 2
            write_file "$workspace/.beads/beads.db" "db changed after export\n"
            ;;
        beads_parse_failure)
            mkdir -p "$workspace/.beads"
            write_file "$workspace/.beads/issues.jsonl" '{"id":"bd-public"}\n{not valid json\n'
            ;;
        *)
            printf 'unknown scenario %s\n' "$scenario" >&2
            return 2
            ;;
    esac

    read -r before_hash before_artifact < <(capture_workspace_state "$workspace" "${scenario}_before")
    local exit_code stdout_artifact stderr_artifact command_text schema_status first_failure degraded_codes
    read -r exit_code stdout_artifact stderr_artifact command_text < <(run_hygiene "$scenario" "$workspace" "$snapshot")
    schema_status="failed"
    first_failure=""
    degraded_codes="[]"

    if [ "$exit_code" -ne 0 ]; then
        first_failure="$(tail -n 20 "$stderr_artifact" "$stdout_artifact" 2>/dev/null | tr '\n' ' ' | cut -c 1-500)"
        read -r after_hash after_artifact < <(capture_workspace_state "$workspace" "${scenario}_after")
        emit_event "$scenario" "scenario" "failed" "$exit_code" "$command_text" "$workspace" "$stdout_artifact" "$stderr_artifact" "$schema_status" "$first_failure" "$degraded_codes" "$before_hash" "$after_hash" "$before_artifact" "$after_artifact"
        return "$exit_code"
    fi

    if jq -e '.success == true and .data.schema == "ee.workspace_hygiene.v1" and .data.readOnly == true' "$stdout_artifact" >/dev/null; then
        schema_status="passed"
        degraded_codes="$(jq -c '(.data.degraded // [])' "$stdout_artifact")"
    fi

    case "$scenario" in
        clean)
            first_failure="$(assert_jq "$stdout_artifact" '.data.dirtyPathCount == 0' "clean workspace should have zero dirty paths" || true)"
            ;;
        source_and_test)
            first_failure="$(assert_jq "$stdout_artifact" '([.data.stagingRecommendations[].name] | index("source")) and ([.data.stagingRecommendations[].name] | index("tests"))' "source_and_test should recommend source and tests groups" || true)"
            ;;
        scratch_generated_secret)
            first_failure="$(assert_jq "$stdout_artifact" '(.data.doNotCommit | index(".env.local")) and (.data.doNotCommit | index("drift-report.txt")) and (.data.doNotCommit | index("Cargo.lock"))' "scratch/generated/secret paths should be doNotCommit" || true)"
            if [ -z "$first_failure" ]; then
                first_failure="$(assert_no_secret "$scenario JSON" "$stdout_artifact" "$SYNTHETIC_RAW_SECRET" || true)"
            fi
            ;;
        active_reservation)
            first_failure="$(assert_jq "$stdout_artifact" '.data.coordinationState.agentMailAvailable == true and (.data.coordinationState.blockedByCoordination[0].path == "src/lib.rs")' "active reservation should block src/lib.rs" || true)"
            ;;
        agent_mail_unavailable)
            first_failure="$(assert_jq "$stdout_artifact" '(.data.degraded | index("workspace_hygiene_agent_mail_unavailable")) and (.data.degraded | index("workspace_hygiene_partial_metadata"))' "missing snapshot should emit Agent Mail unavailable degraded codes" || true)"
            ;;
        beads_pending_flush)
            first_failure="$(assert_jq "$stdout_artifact" '.data.beadsState.classification == "beads_db_dirty_pending_flush" and .data.beadsState.metadataSignal == "db_dirty_pending_flush"' "beads DB marker should report pending flush" || true)"
            ;;
        beads_parse_failure)
            first_failure="$(assert_jq "$stdout_artifact" '(.data.degraded | index("workspace_hygiene_beads_parse_error")) and .data.beadsState.parseErrorLine == 2' "invalid Beads JSONL should report parse line 2" || true)"
            ;;
    esac

    read -r after_hash after_artifact < <(capture_workspace_state "$workspace" "${scenario}_after")
    if [ "$before_hash" != "$after_hash" ] && [ -z "$first_failure" ]; then
        first_failure="workspace hygiene mutated git-visible state for $scenario"
    fi

    if [ "$schema_status" != "passed" ] && [ -z "$first_failure" ]; then
        first_failure="workspace hygiene response failed the envelope/schema smoke check"
    fi

    if [ -n "$first_failure" ]; then
        emit_event "$scenario" "scenario" "failed" 1 "$command_text" "$workspace" "$stdout_artifact" "$stderr_artifact" "$schema_status" "$first_failure" "$degraded_codes" "$before_hash" "$after_hash" "$before_artifact" "$after_artifact"
        printf '%s\n' "$first_failure" >&2
        return 1
    fi

    emit_event "$scenario" "scenario" "pass" 0 "$command_text" "$workspace" "$stdout_artifact" "$stderr_artifact" "$schema_status" "" "$degraded_codes" "$before_hash" "$after_hash" "$before_artifact" "$after_artifact"
}

read -r REPO_BEFORE_HASH REPO_BEFORE_ARTIFACT < <(capture_repo_state "before")
emit_event "setup" "setup" "pass" 0 "locate ee binary" "$REPO_ROOT" "" "" "not_run" "" "[]" "$REPO_BEFORE_HASH" "" "$REPO_BEFORE_ARTIFACT" ""

SCENARIOS=(
    clean
    source_and_test
    scratch_generated_secret
    active_reservation
    agent_mail_unavailable
    beads_pending_flush
    beads_parse_failure
)

for scenario in "${SCENARIOS[@]}"; do
    run_scenario "$scenario"
done

read -r REPO_AFTER_HASH REPO_AFTER_ARTIFACT < <(capture_repo_state "after")
if [ "$REPO_BEFORE_HASH" != "$REPO_AFTER_HASH" ]; then
    emit_event "teardown" "mutation_check" "failed" 1 "compare caller checkout state" "$REPO_ROOT" "" "" "not_run" "caller checkout git-visible state changed" '["workspace_hygiene_read_only_violation"]' "$REPO_BEFORE_HASH" "$REPO_AFTER_HASH" "$REPO_BEFORE_ARTIFACT" "$REPO_AFTER_ARTIFACT"
    exit 1
fi

emit_event "teardown" "mutation_check" "pass" 0 "compare caller checkout state" "$REPO_ROOT" "" "" "not_run" "" "[]" "$REPO_BEFORE_HASH" "$REPO_AFTER_HASH" "$REPO_BEFORE_ARTIFACT" "$REPO_AFTER_ARTIFACT"
printf 'workspace_hygiene: all scenarios passed; events=%s\n' "$EVENT_LOG" >&2
