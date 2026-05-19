#!/usr/bin/env bash
# Logged e2e driver for bd-1eq3l.8.
#
# This script exercises the public `ee workspace hygiene` surface against
# isolated temporary git workspaces. It never builds `ee`, never invokes Cargo,
# and never mutates the caller checkout beyond reading its git state.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
EVENT_ROOT_DEFAULT="${TMPDIR:-/tmp}/ee-workspace-hygiene-e2e"
if [ -n "${EE_WORKSPACE_HYGIENE_EVENT_DIR:-}" ]; then
    EVENT_ROOT="$EE_WORKSPACE_HYGIENE_EVENT_DIR"
else
    case "$EVENT_ROOT_DEFAULT" in
        /Volumes/*) EVENT_ROOT="/tmp/ee-workspace-hygiene-e2e" ;;
        *) EVENT_ROOT="$EVENT_ROOT_DEFAULT" ;;
    esac
fi
EVENT_LOG="$EVENT_ROOT/events.jsonl"
SYNTHETIC_RAW_VALUE="$(printf 'sk-%s-%s' "proj" "$(printf 'B%.0s' {1..40})")"
SELF_TEST_CONTRACTS=false

case "${1:-}" in
    --self-test-contracts)
        SELF_TEST_CONTRACTS=true
        shift
        ;;
    --help | -h)
        printf 'usage: %s [--self-test-contracts]\n' "$0"
        exit 0
        ;;
    "")
        ;;
    *)
        printf 'workspace_hygiene: unknown argument: %s\n' "$1" >&2
        printf 'usage: %s [--self-test-contracts]\n' "$0" >&2
        exit 2
        ;;
esac

if [ "$#" -ne 0 ]; then
    printf 'workspace_hygiene: unexpected extra arguments: %s\n' "$*" >&2
    printf 'usage: %s [--self-test-contracts]\n' "$0" >&2
    exit 2
fi

now_ns() {
    local seconds
    seconds="$(date +%s)"
    printf '%s000000000\n' "$seconds"
}

STARTED_NS="$(now_ns)"

mkdir -p "$EVENT_ROOT"
: > "$EVENT_LOG"

redact_synthetic_value() {
    local value="${1:-}"
    printf '%s\n' "${value//$SYNTHETIC_RAW_VALUE/[redacted-synthetic-secret]}"
}

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
    first_failure="$(redact_synthetic_value "$first_failure")"
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

write_self_test_ee_binary() {
    local fake_binary="$EVENT_ROOT/fake-ee-workspace-hygiene"

    cat > "$fake_binary" <<'FAKE_EE'
#!/usr/bin/env bash
set -euo pipefail

for arg in "$@"; do
    if [ "$arg" = "--help" ] || [ "$arg" = "-h" ]; then
        printf 'workspace hygiene --workspace <path>\n'
        exit 0
    fi
done

json=false
if [ "${1:-}" = "--json" ]; then
    json=true
    shift
fi

if [ "${1:-}" != "workspace" ] || [ "${2:-}" != "hygiene" ]; then
    printf 'fake ee: expected workspace hygiene command\n' >&2
    exit 2
fi
shift 2

workspace=""
while [ "$#" -gt 0 ]; do
    case "$1" in
        --workspace)
            workspace="${2:-}"
            shift 2
            ;;
        --agent-name | --agent-mail-snapshot)
            shift 2
            ;;
        *)
            shift
            ;;
    esac
done

if [ -z "$workspace" ]; then
    printf 'fake ee: missing --workspace\n' >&2
    exit 2
fi

scenario="$(basename "$workspace")"
scenario="${scenario#ee-workspace-hygiene-}"
scenario="${scenario%.*}"

if [ "$json" != true ]; then
    case "$scenario" in
        human_source_and_test)
            printf 'Workspace hygiene:\nStage candidates:\nsource: 1 paths\ntests: 1 paths\n'
            ;;
        human_secret_no_leak)
            printf 'Workspace hygiene:\nDo not commit:\n.env.local\n'
            ;;
        *)
            printf 'Workspace hygiene:\n'
            ;;
    esac
    exit 0
fi

jq -cn --arg scenario "$scenario" '
    def base:
        {
          schema: "ee.response.v2",
          success: true,
          data: {
            schema: "ee.workspace_hygiene.v1",
            readOnly: true,
            dirtyPathCount: 0,
            degraded: [],
            stagingRecommendations: [],
            doNotCommit: [],
            pathClassifications: [],
            bucketCounts: [],
            kindCounts: [],
            secretScan: {
              readOnly: true,
              skippedContentScanCount: 0,
              maxFileBytes: 65536
            },
            coordinationState: {
              agentMailAvailable: false,
              blockedByCoordination: [],
              activeAgents: []
            },
            beadsState: {
              classification: "not_present",
              metadataSignal: "none",
              degradedCodes: [],
              parseErrorLine: null,
              conflictMarkersFound: false,
              jsonlPosture: {untracked: false}
            }
          }
        };
    base
    | if $scenario == "clean" then
        (.data.dirtyPathCount = 0)
      elif $scenario == "source_and_test" then
        (.data.dirtyPathCount = 2)
        | (.data.stagingRecommendations = [
            {name: "source", paths: ["src/lib.rs"]},
            {name: "tests", paths: ["tests/workspace_hygiene.rs"]}
          ])
      elif $scenario == "scratch_only" then
        (.data.dirtyPathCount = 2)
        | (.data.doNotCommit = ["drift-report.txt", "ubs.json"])
        | (.data.pathClassifications = [
            {path: "drift-report.txt", bucket: "do_not_commit", kind: "scratch"},
            {path: "ubs.json", bucket: "do_not_commit", kind: "scratch"}
          ])
        | (.data.bucketCounts = [{name: "do_not_commit", count: 2}])
        | (.data.kindCounts = [{name: "scratch", count: 2}])
      elif $scenario == "generated_only" then
        (.data.dirtyPathCount = 3)
        | (.data.doNotCommit = ["Cargo.lock", "target/debug/ee", "target/release/deps/foo.rlib"])
        | (.data.pathClassifications = [
            {path: "Cargo.lock", bucket: "do_not_commit", kind: "generated"},
            {path: "target/debug/ee", bucket: "do_not_commit", kind: "generated"},
            {path: "target/release/deps/foo.rlib", bucket: "do_not_commit", kind: "generated"}
          ])
        | (.data.bucketCounts = [{name: "do_not_commit", count: 3}])
        | (.data.kindCounts = [{name: "generated", count: 3}])
      elif $scenario == "scratch_generated_secret" then
        (.data.dirtyPathCount = 3)
        | (.data.doNotCommit = [".env.local", "Cargo.lock", "drift-report.txt"])
        | (.data.pathClassifications = [
            {path: ".env.local", bucket: "do_not_commit", kind: "secret_risk"},
            {path: "Cargo.lock", bucket: "do_not_commit", kind: "generated"},
            {path: "drift-report.txt", bucket: "do_not_commit", kind: "scratch"}
          ])
        | (.data.bucketCounts = [{name: "do_not_commit", count: 3}])
        | (.data.kindCounts = [
            {name: "generated", count: 1},
            {name: "scratch", count: 1},
            {name: "secret_risk", count: 1}
          ])
      elif $scenario == "large_binary_scan_skip" then
        (.data.dirtyPathCount = 2)
        | (.data.secretScan = {
            readOnly: true,
            skippedContentScanCount: 2,
            maxFileBytes: 65536
          })
        | (.data.degraded = ["workspace_hygiene_secret_scan_skipped"])
      elif $scenario == "active_reservation" then
        (.data.dirtyPathCount = 1)
        | (.data.coordinationState = {
            agentMailAvailable: true,
            blockedByCoordination: [
              {
                path: "src/lib.rs",
                holderAgent: "OtherAgent",
                pathPattern: "src/lib.rs",
                exclusive: true
              }
            ],
            activeAgents: [{name: "OtherAgent"}]
          })
      elif $scenario == "agent_mail_empty_snapshot" then
        (.data.dirtyPathCount = 1)
        | (.data.stagingRecommendations = [
            {name: "source", paths: ["src/lib.rs"]}
          ])
        | (.data.coordinationState = {
            agentMailAvailable: true,
            blockedByCoordination: [],
            activeAgents: []
          })
      elif $scenario == "agent_mail_unavailable" then
        (.data.dirtyPathCount = 1)
        | (.data.coordinationState = {
            agentMailAvailable: false,
            blockedByCoordination: [],
            activeAgents: []
          })
        | (.data.degraded = [
            "workspace_hygiene_agent_mail_unavailable",
            "workspace_hygiene_partial_metadata"
          ])
      elif $scenario == "beads_pending_flush" then
        (.data.beadsState.classification = "beads_db_dirty_pending_flush")
        | (.data.beadsState.metadataSignal = "db_dirty_pending_flush")
      elif $scenario == "beads_export_only" then
        (.data.beadsState.classification = "beads_export_only")
        | (.data.beadsState.metadataSignal = "unknown")
        | (.data.beadsState.degradedCodes = ["workspace_hygiene_beads_db_divergence_unknown"])
        | (.data.degraded = ["workspace_hygiene_beads_db_divergence_unknown"])
      elif $scenario == "beads_parse_failure" then
        (.data.beadsState.classification = "beads_conflict_or_parse_error")
        | (.data.beadsState.degradedCodes = ["workspace_hygiene_beads_parse_error"])
        | (.data.beadsState.parseErrorLine = 2)
        | (.data.beadsState.conflictMarkersFound = false)
        | (.data.beadsState.jsonlPosture.untracked = true)
        | (.data.degraded = ["workspace_hygiene_beads_parse_error"])
      else
        error("unknown self-test scenario: " + $scenario)
      end
'
FAKE_EE
    chmod +x "$fake_binary"
    EE_BINARY="$fake_binary"
    export EE_BINARY
}

if [ "$SELF_TEST_CONTRACTS" = true ]; then
    write_self_test_ee_binary
fi

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

validate_ee_binary_surface() {
    local help_artifact="$EVENT_ROOT/ee_binary_workspace_hygiene_help.txt"
    local help_error="$EVENT_ROOT/ee_binary_workspace_hygiene_help.stderr.log"
    local exit_code first_failure

    set +e
    "$EE_BINARY" workspace hygiene --help >"$help_artifact" 2>"$help_error"
    exit_code=$?
    set -e

    if [ "$exit_code" -ne 0 ] || ! grep -E '(workspace|hygiene|--workspace)' "$help_artifact" "$help_error" >/dev/null 2>&1; then
        first_failure="$(
            {
                printf 'workspace hygiene help probe failed with exit=%s; ' "$exit_code"
                tail -n 20 "$help_error" "$help_artifact" 2>/dev/null
            } | tr '\n' ' ' | cut -c 1-500
        )"
        emit_event "preflight" "setup" "blocked" 2 "$EE_BINARY workspace hygiene --help" "$REPO_ROOT" "$help_artifact" "$help_error" "failed" "$first_failure" '["ee_binary_unusable"]'
        printf 'workspace_hygiene: ee binary does not expose workspace hygiene help; events=%s\n' "$EVENT_LOG" >&2
        exit 2
    fi
}

validate_ee_binary_surface

WORK_ROOT="${EE_WORKSPACE_HYGIENE_TMPROOT:-${TMPDIR:-/tmp}}"
case "$WORK_ROOT" in
    /Volumes/*) WORK_ROOT="/tmp" ;;
esac
mkdir -p "$WORK_ROOT"

hash_file() {
    shasum -a 256 "$1" | awk '{ print $1 }'
}

file_size_bytes() {
    local path="${1:?path required}"
    wc -c < "$path" | tr -d '[:space:]'
}

file_mtime_seconds() {
    local path="${1:?path required}"
    local mtime
    if mtime="$(stat -f '%m' "$path" 2>/dev/null)" && [[ "$mtime" =~ ^[0-9]+$ ]]; then
        printf '%s\n' "$mtime"
        return 0
    fi
    if mtime="$(stat -c '%Y' "$path" 2>/dev/null)" && [[ "$mtime" =~ ^[0-9]+$ ]]; then
        printf '%s\n' "$mtime"
        return 0
    fi
    printf 'unavailable\n'
}

emit_workspace_file_fingerprints() {
    local workspace="${1:?workspace required}"
    local relative fingerprint_path size_bytes mtime_seconds sha256
    (
        cd "$workspace"
        find . -type f ! -path './.git/*' -print | sed 's#^\./##' | LC_ALL=C sort |
            while IFS= read -r relative; do
                [ -n "$relative" ] || continue
                fingerprint_path="./$relative"
                size_bytes="$(file_size_bytes "$fingerprint_path")"
                mtime_seconds="$(file_mtime_seconds "$fingerprint_path")"
                sha256="$(hash_file "$fingerprint_path")"
                printf '%s\t%s\t%s\t%s\n' "$relative" "$size_bytes" "$mtime_seconds" "$sha256"
            done
    )
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
        printf '\n## file fingerprints (path, size_bytes, mtime_seconds, sha256)\n'
        emit_workspace_file_fingerprints "$workspace"
    ) > "$artifact"
    printf '%s\t%s\n' "$(hash_file "$artifact")" "$artifact"
}

write_file() {
    local path="${1:?path required}"
    local body="${2:-}"
    mkdir -p "$(dirname "$path")"
    printf '%b' "$body" > "$path"
}

write_large_text_file() {
    local path="${1:?path required}"
    local index
    local chunk="aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
    mkdir -p "$(dirname "$path")"
    : > "$path"
    for ((index = 0; index < 600; index++)); do
        printf '%s\n' "$chunk" >> "$path"
    done
}

write_binary_file() {
    local path="${1:?path required}"
    mkdir -p "$(dirname "$path")"
    printf '\377\376\375binary\000payload\n' > "$path"
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

assert_no_raw_value() {
    local label="${1:?label required}"
    local file="${2:?file required}"
    local raw_value="${3:?raw value required}"
    if grep -F "$raw_value" "$file" >/dev/null 2>&1; then
        printf '%s leaked raw synthetic secret\n' "$label"
        return 1
    fi
}

assert_contains() {
    local file="${1:?file required}"
    local needle="${2:?needle required}"
    local message="${3:?message required}"
    if ! grep -F "$needle" "$file" >/dev/null 2>&1; then
        printf '%s\n' "$message"
        return 1
    fi
}

assert_not_json() {
    local file="${1:?file required}"
    local message="${2:?message required}"
    if jq -e . "$file" >/dev/null 2>&1; then
        printf '%s\n' "$message"
        return 1
    fi
}

scenario_plan_json() {
    printf '%s\n' "${SCENARIOS[@]}" | jq -R . | jq -s -c .
}

validate_scenario_plan() {
    local plan_artifact="$EVENT_ROOT/scenario_plan.json"
    local diagnostics="$EVENT_ROOT/scenario_plan_diagnostics.txt"
    local scenario_count unique_count duplicates_artifact
    : > "$diagnostics"

    if [ "${#SCENARIOS[@]}" -eq 0 ]; then
        printf 'scenario plan is empty\n' | tee -a "$diagnostics"
        return 1
    fi

    if ! scenario_plan_json > "$plan_artifact" 2>"$diagnostics"; then
        printf 'scenario plan failed to serialize as JSON\n' | tee -a "$diagnostics"
        return 1
    fi

    scenario_count="$(jq 'length' "$plan_artifact")"
    unique_count="$(jq 'unique | length' "$plan_artifact")"
    if [ "$scenario_count" -ne "$unique_count" ]; then
        duplicates_artifact="$EVENT_ROOT/scenario_plan_duplicates.txt"
        jq -r 'group_by(.)[] | select(length > 1) | .[0]' "$plan_artifact" > "$duplicates_artifact"
        printf 'scenario plan contains duplicate scenario names; duplicates=%s\n' "$duplicates_artifact" | tee -a "$diagnostics"
        return 1
    fi
}

validate_event_log_contract() {
    local event_log="${1:-$EVENT_LOG}"
    local diagnostics="${2:-$EVENT_ROOT/event_log_contract_diagnostics.txt}"
    local require_negative_check="${3:-true}"
    local expected_scenarios_json
    expected_scenarios_json="$(scenario_plan_json)"
    if ! jq -s -e --argjson expected_scenarios "$expected_scenarios_json" --arg require_negative_check "$require_negative_check" '
        . as $events
        | def has_phase($phase): any($events[]; .phase == $phase);
        def phase_event_count($phase; $scenario): [
            $events[] | select(.phase == $phase and .scenario == $scenario)
        ] | length;
        def has_single_pass($phase; $scenario):
            phase_event_count($phase; $scenario) == 1
            and any($events[]; .phase == $phase and .scenario == $scenario and .status == "pass");
        def only_expected_scenarios($phase): [
            $events[] | select(.phase == $phase) | .scenario
        ] | all(. as $scenario | $expected_scenarios | index($scenario));
        def optional_string($field): ($field == null or ($field | type == "string"));
        def sanitized_env_ok:
            (.sanitizedEnv | type == "object")
            and (.sanitizedEnv | keys_unsorted | sort == ["cargoTargetDir", "eeBinary", "tmpRoot", "tmpdir"])
            and (.sanitizedEnv.tmpRoot | type == "string" and length > 0)
            and optional_string(.sanitizedEnv.tmpdir)
            and optional_string(.sanitizedEnv.cargoTargetDir)
            and optional_string(.sanitizedEnv.eeBinary);
        all($events[];
            .schema == "ee.test_event.v1"
            and .beadId == "bd-1eq3l.8"
            and .surface == "workspace_hygiene"
            and (.scenario | type == "string" and length > 0)
            and (.phase | type == "string" and length > 0)
            and (.status | IN("pass", "failed", "blocked"))
            and (.exitCode | type == "number")
            and (.exitCode >= 0)
            and (.exitCode == (.exitCode | floor))
            and (
                (.status == "pass" and .exitCode == 0)
                or (.status != "pass" and .exitCode != 0)
            )
            and (.elapsedMs | type == "number")
            and (.elapsedMs >= 0)
            and (.elapsedMs == (.elapsedMs | floor))
            and (.schemaValidationStatus | IN("not_run", "passed", "failed", "human_output"))
            and (.degradedCodes | type == "array" and all(.[]; type == "string" and length > 0))
            and sanitized_env_ok
            and (
                .status == "pass"
                or (.firstFailureDiagnosis | type == "string" and length > 0)
            )
            and (
                .phase != "scenario"
                or (
                    (.schemaValidationStatus | IN("passed", "human_output"))
                    and
                    (.command | type == "string" and length > 0)
                    and (.workspace | type == "string" and length > 0)
                    and (.stdoutArtifact | type == "string" and length > 0)
                    and (.stderrArtifact | type == "string" and length > 0)
                    and (.beforeMutationHash | type == "string" and length > 0)
                    and (.afterMutationHash | type == "string" and length > 0)
                    and (.beforeMutationArtifact | type == "string" and length > 0)
                    and (.afterMutationArtifact | type == "string" and length > 0)
                )
            )
        )
        and has_phase("setup")
        and has_phase("scenario_plan")
        and has_phase("schema_validation")
        and has_phase("redaction_check")
        and has_phase("artifact_redaction_check")
        and has_phase("stdout_stderr_isolation")
        and has_phase("artifact_reference_contract")
        and has_phase("mutation_artifact_contract")
        and has_phase("local_cargo_guard")
        and has_phase("mutation_check")
        and has_phase("teardown")
        and (
            $require_negative_check != "true"
            or (
                has_phase("negative_contract_check")
                and any($events[];
                    .phase == "negative_contract_check"
                    and .scenario == "event_log_negative_contracts"
                    and .status == "pass"
                    and .schemaValidationStatus == "passed"
                    and (.stdoutArtifact | type == "string" and length > 0)
                )
            )
        )
        and any($events[];
            .phase == "scenario_plan"
            and .scenario == "scenario_plan"
            and .status == "pass"
            and .schemaValidationStatus == "passed"
            and (.stdoutArtifact | type == "string" and length > 0)
        )
        and ($expected_scenarios | all(. as $scenario | has_single_pass("scenario"; $scenario)))
        and ($expected_scenarios | all(. as $scenario | has_single_pass("schema_validation"; $scenario)))
        and only_expected_scenarios("scenario")
        and only_expected_scenarios("schema_validation")
    ' "$event_log" >/dev/null 2>"$diagnostics"; then
        printf 'event log contract check failed; diagnostics=%s\n' "$diagnostics"
        return 1
    fi
}

expect_event_log_contract_rejected() {
    local label="${1:?label required}"
    local event_log="${2:?event log required}"
    local diagnostics="$EVENT_ROOT/${label}_diagnostics.txt"

    if validate_event_log_contract "$event_log" "$diagnostics" false >/dev/null 2>&1; then
        printf '%s malformed event log was accepted: %s\n' "$label" "$event_log"
        return 1
    fi
}

expect_mutation_artifact_contract_rejected() {
    local label="${1:?label required}"
    local event_log="${2:?event log required}"
    local diagnostics="$EVENT_ROOT/${label}_diagnostics.txt"

    if validate_mutation_artifact_contract "$event_log" "$diagnostics" >/dev/null 2>&1; then
        printf '%s malformed mutation artifacts were accepted: %s\n' "$label" "$event_log"
        return 1
    fi
}

expect_local_cargo_guard_rejected() {
    local label="${1:?label required}"
    local event_log="${2:?event log required}"
    local diagnostics="$EVENT_ROOT/${label}_diagnostics.txt"

    if validate_no_local_cargo_commands "$event_log" "$diagnostics" >/dev/null 2>&1; then
        printf '%s malformed local Cargo command evidence was accepted: %s\n' "$label" "$event_log"
        return 1
    fi
}

expect_event_artifact_references_rejected() {
    local label="${1:?label required}"
    local event_log="${2:?event log required}"
    local diagnostics="$EVENT_ROOT/${label}_diagnostics.txt"

    if validate_event_artifact_references "$event_log" "$diagnostics" >/dev/null 2>&1; then
        printf '%s malformed event artifact references were accepted: %s\n' "$label" "$event_log"
        return 1
    fi
}

expect_event_artifact_redaction_rejected() {
    local label="${1:?label required}"
    local event_log="${2:?event log required}"
    local diagnostics="$EVENT_ROOT/${label}_diagnostics.txt"

    if validate_event_artifact_redaction "$event_log" "$diagnostics" >/dev/null 2>&1; then
        printf '%s malformed event artifact redaction was accepted: %s\n' "$label" "$event_log"
        return 1
    fi
}

validate_event_log_negative_contracts() {
    local diagnostics="$EVENT_ROOT/event_log_negative_contracts_diagnostics.txt"
    local failure_count=0
    local missing_plan bad_schema_status unexpected_scenario bad_degraded_code missing_failure_diagnosis
    local bad_pass_exit_code bad_failed_exit_code negative_exit_code fractional_exit_code
    local negative_elapsed_ms fractional_elapsed_ms
    local missing_sanitized_env bad_sanitized_env_shape
    local raw_secret_artifact raw_secret_artifact_path raw_secret_late_artifact raw_secret_late_artifact_path
    local bad_artifact_reference missing_artifact_path external_artifact_reference external_artifact_path
    local traversal_artifact_reference traversal_artifact_path symlink_artifact_reference symlink_target_path symlink_artifact_path
    local missing_mutation_contract bad_before_artifact bad_before_hash missing_fingerprint_artifact local_cargo_command
    : > "$diagnostics"

    missing_plan="$EVENT_ROOT/event_log_negative_missing_scenario_plan.jsonl"
    jq -c 'select(.phase != "scenario_plan")' "$EVENT_LOG" > "$missing_plan"
    if ! expect_event_log_contract_rejected "missing_scenario_plan" "$missing_plan" >> "$diagnostics"; then
        failure_count=$((failure_count + 1))
    fi

    bad_schema_status="$EVENT_ROOT/event_log_negative_bad_schema_status.jsonl"
    jq -c 'if .phase == "scenario" and .scenario == "clean" then .schemaValidationStatus = "failed" else . end' "$EVENT_LOG" > "$bad_schema_status"
    if ! expect_event_log_contract_rejected "bad_schema_status" "$bad_schema_status" >> "$diagnostics"; then
        failure_count=$((failure_count + 1))
    fi

    unexpected_scenario="$EVENT_ROOT/event_log_negative_unexpected_scenario.jsonl"
    jq -c 'if (.phase == "scenario" or .phase == "schema_validation") and .scenario == "clean" then .scenario = "unexpected_clean_alias" else . end' "$EVENT_LOG" > "$unexpected_scenario"
    if ! expect_event_log_contract_rejected "unexpected_scenario" "$unexpected_scenario" >> "$diagnostics"; then
        failure_count=$((failure_count + 1))
    fi

    bad_degraded_code="$EVENT_ROOT/event_log_negative_bad_degraded_code.jsonl"
    jq -c 'if .phase == "scenario_plan" then .degradedCodes = [42] else . end' "$EVENT_LOG" > "$bad_degraded_code"
    if ! expect_event_log_contract_rejected "bad_degraded_code" "$bad_degraded_code" >> "$diagnostics"; then
        failure_count=$((failure_count + 1))
    fi

    missing_failure_diagnosis="$EVENT_ROOT/event_log_negative_missing_failure_diagnosis.jsonl"
    jq -s -c '. + [(.[] | select(.phase == "setup") | .status = "failed" | .exitCode = 1 | .firstFailureDiagnosis = null)] | .[]' "$EVENT_LOG" > "$missing_failure_diagnosis"
    if ! expect_event_log_contract_rejected "missing_failure_diagnosis" "$missing_failure_diagnosis" >> "$diagnostics"; then
        failure_count=$((failure_count + 1))
    fi

    bad_pass_exit_code="$EVENT_ROOT/event_log_negative_bad_pass_exit_code.jsonl"
    jq -c 'if .phase == "scenario_plan" then .exitCode = 7 else . end' "$EVENT_LOG" > "$bad_pass_exit_code"
    if ! expect_event_log_contract_rejected "bad_pass_exit_code" "$bad_pass_exit_code" >> "$diagnostics"; then
        failure_count=$((failure_count + 1))
    fi

    bad_failed_exit_code="$EVENT_ROOT/event_log_negative_bad_failed_exit_code.jsonl"
    jq -s -c '. + [(.[] | select(.phase == "setup") | .status = "failed" | .exitCode = 0 | .firstFailureDiagnosis = "synthetic failed event with zero exit code")] | .[]' "$EVENT_LOG" > "$bad_failed_exit_code"
    if ! expect_event_log_contract_rejected "bad_failed_exit_code" "$bad_failed_exit_code" >> "$diagnostics"; then
        failure_count=$((failure_count + 1))
    fi

    negative_exit_code="$EVENT_ROOT/event_log_negative_negative_exit_code.jsonl"
    jq -c 'if .phase == "scenario_plan" then .exitCode = -1 else . end' "$EVENT_LOG" > "$negative_exit_code"
    if ! expect_event_log_contract_rejected "negative_exit_code" "$negative_exit_code" >> "$diagnostics"; then
        failure_count=$((failure_count + 1))
    fi

    fractional_exit_code="$EVENT_ROOT/event_log_negative_fractional_exit_code.jsonl"
    jq -c 'if .phase == "scenario_plan" then .exitCode = 1.5 else . end' "$EVENT_LOG" > "$fractional_exit_code"
    if ! expect_event_log_contract_rejected "fractional_exit_code" "$fractional_exit_code" >> "$diagnostics"; then
        failure_count=$((failure_count + 1))
    fi

    negative_elapsed_ms="$EVENT_ROOT/event_log_negative_negative_elapsed_ms.jsonl"
    jq -c 'if .phase == "scenario_plan" then .elapsedMs = -1 else . end' "$EVENT_LOG" > "$negative_elapsed_ms"
    if ! expect_event_log_contract_rejected "negative_elapsed_ms" "$negative_elapsed_ms" >> "$diagnostics"; then
        failure_count=$((failure_count + 1))
    fi

    fractional_elapsed_ms="$EVENT_ROOT/event_log_negative_fractional_elapsed_ms.jsonl"
    jq -c 'if .phase == "scenario_plan" then .elapsedMs = 1.5 else . end' "$EVENT_LOG" > "$fractional_elapsed_ms"
    if ! expect_event_log_contract_rejected "fractional_elapsed_ms" "$fractional_elapsed_ms" >> "$diagnostics"; then
        failure_count=$((failure_count + 1))
    fi

    missing_sanitized_env="$EVENT_ROOT/event_log_negative_missing_sanitized_env.jsonl"
    jq -c 'if .phase == "scenario_plan" then del(.sanitizedEnv) else . end' "$EVENT_LOG" > "$missing_sanitized_env"
    if ! expect_event_log_contract_rejected "missing_sanitized_env" "$missing_sanitized_env" >> "$diagnostics"; then
        failure_count=$((failure_count + 1))
    fi

    bad_sanitized_env_shape="$EVENT_ROOT/event_log_negative_bad_sanitized_env_shape.jsonl"
    jq -c 'if .phase == "scenario_plan" then .sanitizedEnv = {"PATH": "/tmp/bin", "tmpRoot": ""} else . end' "$EVENT_LOG" > "$bad_sanitized_env_shape"
    if ! expect_event_log_contract_rejected "bad_sanitized_env_shape" "$bad_sanitized_env_shape" >> "$diagnostics"; then
        failure_count=$((failure_count + 1))
    fi

    raw_secret_artifact="$EVENT_ROOT/event_log_negative_raw_secret_artifact.jsonl"
    raw_secret_artifact_path="$EVENT_ROOT/event_log_negative_raw_secret_artifact.txt"
    printf '%s\n' "$SYNTHETIC_RAW_VALUE" > "$raw_secret_artifact_path"
    jq -c --arg artifact "$raw_secret_artifact_path" 'if .phase == "scenario" and .scenario == "clean" then .stdoutArtifact = $artifact else . end' "$EVENT_LOG" > "$raw_secret_artifact"
    if ! expect_event_artifact_redaction_rejected "raw_secret_artifact" "$raw_secret_artifact" >> "$diagnostics"; then
        failure_count=$((failure_count + 1))
    fi

    raw_secret_late_artifact="$EVENT_ROOT/event_log_negative_raw_secret_late_artifact.jsonl"
    raw_secret_late_artifact_path="$EVENT_ROOT/event_log_negative_raw_secret_late_artifact.txt"
    printf '%s\n' "$SYNTHETIC_RAW_VALUE" > "$raw_secret_late_artifact_path"
    jq -c --arg artifact "$raw_secret_late_artifact_path" 'if .phase == "local_cargo_guard" then .stdoutArtifact = $artifact else . end' "$EVENT_LOG" > "$raw_secret_late_artifact"
    if ! expect_event_artifact_redaction_rejected "raw_secret_late_artifact" "$raw_secret_late_artifact" >> "$diagnostics"; then
        failure_count=$((failure_count + 1))
    fi

    bad_artifact_reference="$EVENT_ROOT/event_log_negative_missing_artifact_reference.jsonl"
    missing_artifact_path="$EVENT_ROOT/event_log_negative_missing_artifact_reference.$$.missing"
    jq -c --arg artifact "$missing_artifact_path" 'if .phase == "scenario_plan" then .stdoutArtifact = $artifact else . end' "$EVENT_LOG" > "$bad_artifact_reference"
    if ! expect_event_artifact_references_rejected "missing_artifact_reference" "$bad_artifact_reference" >> "$diagnostics"; then
        failure_count=$((failure_count + 1))
    fi

    external_artifact_reference="$EVENT_ROOT/event_log_negative_external_artifact_reference.jsonl"
    external_artifact_path="$EVENT_ROOT.external_artifact_reference.$$.txt"
    printf 'external artifact reference should be rejected\n' > "$external_artifact_path"
    jq -c --arg artifact "$external_artifact_path" 'if .phase == "scenario_plan" then .stdoutArtifact = $artifact else . end' "$EVENT_LOG" > "$external_artifact_reference"
    if ! expect_event_artifact_references_rejected "external_artifact_reference" "$external_artifact_reference" >> "$diagnostics"; then
        failure_count=$((failure_count + 1))
    fi

    traversal_artifact_reference="$EVENT_ROOT/event_log_negative_traversal_artifact_reference.jsonl"
    traversal_artifact_path="$EVENT_ROOT/../event_log_negative_traversal_artifact_reference.$$.txt"
    printf 'traversal artifact reference should be rejected\n' > "$traversal_artifact_path"
    jq -c --arg artifact "$traversal_artifact_path" 'if .phase == "scenario_plan" then .stdoutArtifact = $artifact else . end' "$EVENT_LOG" > "$traversal_artifact_reference"
    if ! expect_event_artifact_references_rejected "traversal_artifact_reference" "$traversal_artifact_reference" >> "$diagnostics"; then
        failure_count=$((failure_count + 1))
    fi

    symlink_artifact_reference="$EVENT_ROOT/event_log_negative_symlink_artifact_reference.jsonl"
    symlink_target_path="$EVENT_ROOT.symlink_artifact_target.$$.txt"
    symlink_artifact_path="$EVENT_ROOT/event_log_negative_symlink_artifact_reference.$$.txt"
    printf 'symlink artifact target should be rejected\n' > "$symlink_target_path"
    ln -s "$symlink_target_path" "$symlink_artifact_path"
    jq -c --arg artifact "$symlink_artifact_path" 'if .phase == "scenario_plan" then .stdoutArtifact = $artifact else . end' "$EVENT_LOG" > "$symlink_artifact_reference"
    if ! expect_event_artifact_references_rejected "symlink_artifact_reference" "$symlink_artifact_reference" >> "$diagnostics"; then
        failure_count=$((failure_count + 1))
    fi

    missing_mutation_contract="$EVENT_ROOT/event_log_negative_missing_mutation_artifact_contract.jsonl"
    jq -c 'select(.phase != "mutation_artifact_contract")' "$EVENT_LOG" > "$missing_mutation_contract"
    if ! expect_event_log_contract_rejected "missing_mutation_artifact_contract" "$missing_mutation_contract" >> "$diagnostics"; then
        failure_count=$((failure_count + 1))
    fi

    bad_before_artifact="$EVENT_ROOT/event_log_negative_bad_before_mutation_artifact.jsonl"
    missing_fingerprint_artifact="$EVENT_ROOT/event_log_negative_missing_fingerprint_rows.txt"
    printf '## git status --short\n' > "$missing_fingerprint_artifact"
    jq -c --arg artifact "$missing_fingerprint_artifact" 'if .phase == "scenario" and .scenario == "clean" then .beforeMutationArtifact = $artifact else . end' "$EVENT_LOG" > "$bad_before_artifact"
    if ! expect_mutation_artifact_contract_rejected "bad_before_mutation_artifact" "$bad_before_artifact" >> "$diagnostics"; then
        failure_count=$((failure_count + 1))
    fi

    bad_before_hash="$EVENT_ROOT/event_log_negative_bad_before_mutation_hash.jsonl"
    jq -c 'if .phase == "scenario" and .scenario == "clean" then .beforeMutationHash = "not-the-artifact-hash" else . end' "$EVENT_LOG" > "$bad_before_hash"
    if ! expect_mutation_artifact_contract_rejected "bad_before_mutation_hash" "$bad_before_hash" >> "$diagnostics"; then
        failure_count=$((failure_count + 1))
    fi

    local_cargo_command="$EVENT_ROOT/event_log_negative_local_cargo_command.jsonl"
    jq -c 'if .phase == "scenario" and .scenario == "clean" then .command = "cargo test --lib workspace_hygiene -- --nocapture" else . end' "$EVENT_LOG" > "$local_cargo_command"
    if ! expect_local_cargo_guard_rejected "local_cargo_command" "$local_cargo_command" >> "$diagnostics"; then
        failure_count=$((failure_count + 1))
    fi

    if [ "$failure_count" -ne 0 ]; then
        printf 'event log negative contract checks failed; diagnostics=%s\n' "$diagnostics"
        return 1
    fi
}

validate_stdout_stderr_isolation() {
    local diagnostics="$EVENT_ROOT/stdout_stderr_isolation_diagnostics.txt"
    local scenario_count=0
    local failure_count=0
    : > "$diagnostics"

    local scenario schema_status stdout_artifact stderr_artifact
    while IFS=$'\t' read -r scenario schema_status stdout_artifact stderr_artifact; do
        scenario_count=$((scenario_count + 1))

        if [ ! -s "$stdout_artifact" ]; then
            printf '%s stdout artifact missing or empty: %s\n' "$scenario" "$stdout_artifact" >> "$diagnostics"
            failure_count=$((failure_count + 1))
            continue
        fi

        if [ ! -e "$stderr_artifact" ]; then
            printf '%s stderr artifact missing: %s\n' "$scenario" "$stderr_artifact" >> "$diagnostics"
            failure_count=$((failure_count + 1))
            continue
        fi

        if grep -E '"schema"[[:space:]]*:[[:space:]]*"ee\.(response|workspace_hygiene)' "$stderr_artifact" >/dev/null 2>&1; then
            printf '%s stderr artifact contains a response/workspace schema payload: %s\n' "$scenario" "$stderr_artifact" >> "$diagnostics"
            failure_count=$((failure_count + 1))
        fi

        case "$schema_status" in
            passed)
                if ! jq -e '.schema == "ee.response.v2" and .success == true and .data.schema == "ee.workspace_hygiene.v1"' "$stdout_artifact" >/dev/null 2>&1; then
                    printf '%s JSON stdout artifact does not contain the expected response envelope: %s\n' "$scenario" "$stdout_artifact" >> "$diagnostics"
                    failure_count=$((failure_count + 1))
                fi
                ;;
            human_output)
                if jq -e . "$stdout_artifact" >/dev/null 2>&1; then
                    printf '%s human stdout artifact unexpectedly parses as JSON: %s\n' "$scenario" "$stdout_artifact" >> "$diagnostics"
                    failure_count=$((failure_count + 1))
                fi
                ;;
            *)
                printf '%s scenario did not pass schema/output classification: %s\n' "$scenario" "$schema_status" >> "$diagnostics"
                failure_count=$((failure_count + 1))
                ;;
        esac
    done < <(jq -r '. | select(.phase == "scenario") | [.scenario, .schemaValidationStatus, .stdoutArtifact, .stderrArtifact] | @tsv' "$EVENT_LOG")

    if [ "$scenario_count" -eq 0 ]; then
        printf 'no scenario events found in %s\n' "$EVENT_LOG" >> "$diagnostics"
        printf 'stdout/stderr isolation check failed; diagnostics=%s\n' "$diagnostics"
        return 1
    fi

    if [ "$failure_count" -ne 0 ]; then
        printf 'stdout/stderr isolation check failed; diagnostics=%s\n' "$diagnostics"
        return 1
    fi
}

validate_event_artifact_redaction() {
    local event_log="${1:-$EVENT_LOG}"
    local diagnostics="${2:-$EVENT_ROOT/event_artifact_redaction_diagnostics.txt}"
    local artifact_count=0
    local failure_count=0
    : > "$diagnostics"

    local scenario phase field artifact
    while IFS=$'\t' read -r scenario phase field artifact; do
        artifact_count=$((artifact_count + 1))
        if [ -z "$artifact" ] || [ ! -f "$artifact" ]; then
            printf '%s %s %s artifact missing before redaction scan: %s\n' "$scenario" "$phase" "$field" "$artifact" >> "$diagnostics"
            failure_count=$((failure_count + 1))
            continue
        fi
        if grep -F "$SYNTHETIC_RAW_VALUE" "$artifact" >/dev/null 2>&1; then
            printf '%s %s %s artifact leaked raw synthetic secret: %s\n' "$scenario" "$phase" "$field" "$artifact" >> "$diagnostics"
            failure_count=$((failure_count + 1))
        fi
    done < <(jq -r '
        . as $event
        | ["stdoutArtifact", "stderrArtifact", "beforeMutationArtifact", "afterMutationArtifact"][] as $field
        | select($event[$field] != null)
        | [$event.scenario, $event.phase, $field, $event[$field]]
        | @tsv
    ' "$event_log")

    if [ "$artifact_count" -eq 0 ]; then
        printf 'no event artifact references found in %s\n' "$event_log" >> "$diagnostics"
        printf 'event artifact redaction check failed; diagnostics=%s\n' "$diagnostics"
        return 1
    fi

    if [ "$failure_count" -ne 0 ]; then
        printf 'event artifact redaction check failed; diagnostics=%s\n' "$diagnostics"
        return 1
    fi
}

validate_event_artifact_references() {
    local event_log="${1:-$EVENT_LOG}"
    local diagnostics="${2:-$EVENT_ROOT/event_artifact_reference_diagnostics.txt}"
    local artifact_count=0
    local failure_count=0
    local event_root_physical
    : > "$diagnostics"

    if ! event_root_physical="$(cd -P "$EVENT_ROOT" 2>/dev/null && pwd -P)"; then
        printf 'event root cannot be resolved: %s\n' "$EVENT_ROOT" >> "$diagnostics"
        printf 'event artifact reference check failed; diagnostics=%s\n' "$diagnostics"
        return 1
    fi

    local scenario phase field artifact artifact_dir artifact_name artifact_dir_physical artifact_physical
    while IFS=$'\t' read -r scenario phase field artifact; do
        artifact_count=$((artifact_count + 1))
        if [ -z "$artifact" ]; then
            printf '%s %s %s artifact reference is empty\n' "$scenario" "$phase" "$field" >> "$diagnostics"
            failure_count=$((failure_count + 1))
            continue
        fi
        case "$artifact" in
            "$EVENT_ROOT"/*) ;;
            *)
                printf '%s %s %s artifact reference escapes event root: %s\n' "$scenario" "$phase" "$field" "$artifact" >> "$diagnostics"
                failure_count=$((failure_count + 1))
                continue
                ;;
        esac
        artifact_dir="$(dirname "$artifact")"
        artifact_name="$(basename "$artifact")"
        if ! artifact_dir_physical="$(cd -P "$artifact_dir" 2>/dev/null && pwd -P)"; then
            printf '%s %s %s artifact directory cannot be resolved: %s\n' "$scenario" "$phase" "$field" "$artifact_dir" >> "$diagnostics"
            failure_count=$((failure_count + 1))
            continue
        fi
        artifact_physical="$artifact_dir_physical/$artifact_name"
        case "$artifact_physical" in
            "$event_root_physical"/*) ;;
            *)
                printf '%s %s %s artifact physical path escapes event root: %s -> %s\n' "$scenario" "$phase" "$field" "$artifact" "$artifact_physical" >> "$diagnostics"
                failure_count=$((failure_count + 1))
                continue
                ;;
        esac
        if [ ! -e "$artifact" ]; then
            printf '%s %s %s artifact reference does not exist: %s\n' "$scenario" "$phase" "$field" "$artifact" >> "$diagnostics"
            failure_count=$((failure_count + 1))
            continue
        fi
        if [ -L "$artifact" ] || [ ! -f "$artifact" ]; then
            printf '%s %s %s artifact reference is not a regular file: %s\n' "$scenario" "$phase" "$field" "$artifact" >> "$diagnostics"
            failure_count=$((failure_count + 1))
        fi
    done < <(jq -r '
        . as $event
        | ["stdoutArtifact", "stderrArtifact", "beforeMutationArtifact", "afterMutationArtifact"][] as $field
        | select($event[$field] != null)
        | [$event.scenario, $event.phase, $field, $event[$field]]
        | @tsv
    ' "$event_log")

    if [ "$artifact_count" -eq 0 ]; then
        printf 'no event artifact references found in %s\n' "$event_log" >> "$diagnostics"
        printf 'event artifact reference check failed; diagnostics=%s\n' "$diagnostics"
        return 1
    fi

    if [ "$failure_count" -ne 0 ]; then
        printf 'event artifact reference check failed; diagnostics=%s\n' "$diagnostics"
        return 1
    fi
}

fingerprint_artifact_has_rows() {
    local artifact="${1:?artifact required}"
    awk '
        /^## file fingerprints \(path, size_bytes, mtime_seconds, sha256\)$/ {
            seen_header = 1
            next
        }
        seen_header && NF >= 4 {
            row_count += 1
        }
        END {
            exit(seen_header && row_count > 0 ? 0 : 1)
        }
    ' "$artifact"
}

validate_mutation_artifact_contract() {
    local event_log="${1:-$EVENT_LOG}"
    local diagnostics="${2:-$EVENT_ROOT/mutation_artifact_contract_diagnostics.txt}"
    local scenario_count=0
    local failure_count=0
    : > "$diagnostics"

    local scenario before_hash after_hash before_artifact after_artifact
    while IFS=$'\t' read -r scenario before_hash after_hash before_artifact after_artifact; do
        scenario_count=$((scenario_count + 1))
        for artifact_label in before after; do
            local artifact expected_hash actual_hash
            if [ "$artifact_label" = "before" ]; then
                artifact="$before_artifact"
                expected_hash="$before_hash"
            else
                artifact="$after_artifact"
                expected_hash="$after_hash"
            fi

            if [ ! -s "$artifact" ]; then
                printf '%s %s mutation artifact missing or empty: %s\n' "$scenario" "$artifact_label" "$artifact" >> "$diagnostics"
                failure_count=$((failure_count + 1))
                continue
            fi
            if ! fingerprint_artifact_has_rows "$artifact"; then
                printf '%s %s mutation artifact lacks file fingerprint rows: %s\n' "$scenario" "$artifact_label" "$artifact" >> "$diagnostics"
                failure_count=$((failure_count + 1))
            fi
            actual_hash="$(hash_file "$artifact")"
            if [ "$expected_hash" != "$actual_hash" ]; then
                printf '%s %s mutation hash mismatch: expected=%s actual=%s artifact=%s\n' "$scenario" "$artifact_label" "$expected_hash" "$actual_hash" "$artifact" >> "$diagnostics"
                failure_count=$((failure_count + 1))
            fi
        done
    done < <(jq -r '. | select(.phase == "scenario") | [.scenario, .beforeMutationHash, .afterMutationHash, .beforeMutationArtifact, .afterMutationArtifact] | @tsv' "$event_log")

    if [ "$scenario_count" -eq 0 ]; then
        printf 'no scenario events found in %s\n' "$event_log" >> "$diagnostics"
        printf 'mutation artifact contract check failed; diagnostics=%s\n' "$diagnostics"
        return 1
    fi

    if [ "$failure_count" -ne 0 ]; then
        printf 'mutation artifact contract check failed; diagnostics=%s\n' "$diagnostics"
        return 1
    fi
}

validate_no_local_cargo_commands() {
    local event_log="${1:-$EVENT_LOG}"
    local diagnostics="${2:-$EVENT_ROOT/local_cargo_command_guard_diagnostics.txt}"
    : > "$diagnostics"

    if ! jq -s -e '
        all(.[]; ((.command // "") | test("(^|[[:space:]\"\\x27])(cargo|rustc|rustdoc)([[:space:]\"\\x27]|$)") | not))
    ' "$event_log" >/dev/null 2>"$diagnostics"; then
        jq -r '
            select((.command // "") | test("(^|[[:space:]\"\\x27])(cargo|rustc|rustdoc)([[:space:]\"\\x27]|$)"))
            | "scenario=\(.scenario) phase=\(.phase) command=\(.command)"
        ' "$event_log" >> "$diagnostics" 2>/dev/null || true
        printf 'event log contains direct local Cargo/rustdoc/rustc command evidence; diagnostics=%s\n' "$diagnostics"
        return 1
    fi
}

run_hygiene_human() {
    local scenario="${1:?scenario required}"
    local workspace="${2:?workspace required}"
    local stdout_artifact="$EVENT_ROOT/${scenario}_stdout.txt"
    local stderr_artifact="$EVENT_ROOT/${scenario}_stderr.log"
    local command_text
    local -a args
    args=(workspace hygiene --agent-name SapphireElk --workspace "$workspace")
    command_text="$EE_BINARY ${args[*]}"

    set +e
    "$EE_BINARY" "${args[@]}" >"$stdout_artifact" 2>"$stderr_artifact"
    local exit_code=$?
    set -e

    printf '%s\t%s\t%s\t%s\n' "$exit_code" "$stdout_artifact" "$stderr_artifact" "$command_text"
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
        human_source_and_test)
            write_file "$workspace/src/lib.rs" "pub fn changed() -> bool { true }\n"
            write_file "$workspace/tests/workspace_hygiene.rs" "#[test]\nfn fixture() {}\n"
            ;;
        human_secret_no_leak)
            write_file "$workspace/.env.local" "OPENAI_""API_KEY=$SYNTHETIC_RAW_VALUE\n"
            ;;
        scratch_only)
            write_file "$workspace/drift-report.txt" "local diagnostic output\n"
            write_file "$workspace/ubs.json" "{\"status\":\"local-only\"}\n"
            ;;
        generated_only)
            write_file "$workspace/Cargo.lock" "generated lockfile placeholder\n"
            write_file "$workspace/target/debug/ee" "generated debug binary placeholder\n"
            write_file "$workspace/target/release/deps/foo.rlib" "generated rust library placeholder\n"
            ;;
        scratch_generated_secret)
            write_file "$workspace/drift-report.txt" "local diagnostic output\n"
            write_file "$workspace/Cargo.lock" "generated lockfile placeholder\n"
            write_file "$workspace/.env.local" "OPENAI_""API_KEY=$SYNTHETIC_RAW_VALUE\n"
            ;;
        large_binary_scan_skip)
            write_large_text_file "$workspace/logs/large-output.log"
            write_binary_file "$workspace/blobs/capture.bin"
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
        agent_mail_empty_snapshot)
            write_file "$workspace/src/lib.rs" "pub fn changed() -> bool { true }\n"
            snapshot="$workspace/agent-mail-empty.json"
            write_file "$snapshot" '{
  "file_reservations": [],
  "active_agents": [],
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
        beads_export_only)
            mkdir -p "$workspace/.beads"
            write_file "$workspace/.beads/.gitignore" "*.db\nlast-touched\n"
            write_file "$workspace/.beads/issues.jsonl" '{"id":"bd-public","title":"seed"}\n'
            git -C "$workspace" add .beads/.gitignore .beads/issues.jsonl
            git -C "$workspace" -c user.email=ee-test@example.invalid -c user.name="ee test" commit -m "seed beads metadata" >/dev/null
            write_file "$workspace/.beads/issues.jsonl" '{"id":"bd-public","title":"seed"}\n{"id":"bd-export-only","title":"exported update"}\n'
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
    schema_status="failed"
    first_failure=""
    degraded_codes="[]"

    if [[ "$scenario" == human_* ]]; then
        read -r exit_code stdout_artifact stderr_artifact command_text < <(run_hygiene_human "$scenario" "$workspace")
        schema_status="human_output"
    else
        read -r exit_code stdout_artifact stderr_artifact command_text < <(run_hygiene "$scenario" "$workspace" "$snapshot")
    fi

    if [ "$exit_code" -ne 0 ]; then
        first_failure="$(tail -n 20 "$stderr_artifact" "$stdout_artifact" 2>/dev/null | tr '\n' ' ' | cut -c 1-500)"
        read -r after_hash after_artifact < <(capture_workspace_state "$workspace" "${scenario}_after")
        emit_event "$scenario" "scenario" "failed" "$exit_code" "$command_text" "$workspace" "$stdout_artifact" "$stderr_artifact" "$schema_status" "$first_failure" "$degraded_codes" "$before_hash" "$after_hash" "$before_artifact" "$after_artifact"
        return "$exit_code"
    fi

    if [[ "$scenario" != human_* ]] && jq -e '.success == true and .data.schema == "ee.workspace_hygiene.v1" and .data.readOnly == true' "$stdout_artifact" >/dev/null; then
        schema_status="passed"
        degraded_codes="$(jq -c '(.data.degraded // [])' "$stdout_artifact")"
    fi

    if [ "$schema_status" = "passed" ] || [ "$schema_status" = "human_output" ]; then
        emit_event "$scenario" "schema_validation" "pass" 0 "$command_text" "$workspace" "$stdout_artifact" "$stderr_artifact" "$schema_status" "" "$degraded_codes" "$before_hash" "" "$before_artifact" ""
    else
        first_failure="workspace hygiene response failed the envelope/schema smoke check"
        emit_event "$scenario" "schema_validation" "failed" 1 "$command_text" "$workspace" "$stdout_artifact" "$stderr_artifact" "$schema_status" "$first_failure" "$degraded_codes" "$before_hash" "" "$before_artifact" ""
    fi

    if [ -z "$first_failure" ]; then
        case "$scenario" in
            clean)
                first_failure="$(assert_jq "$stdout_artifact" '.data.dirtyPathCount == 0' "clean workspace should have zero dirty paths" || true)"
                ;;
            source_and_test)
                first_failure="$(assert_jq "$stdout_artifact" '([.data.stagingRecommendations[].name] == ["source", "tests"]) and (.data.stagingRecommendations[0].paths == ["src/lib.rs"]) and (.data.stagingRecommendations[1].paths == ["tests/workspace_hygiene.rs"])' "source_and_test should recommend source and tests groups in deterministic order" || true)"
                ;;
            human_source_and_test)
                first_failure="$(assert_contains "$stdout_artifact" "Workspace hygiene:" "human output should include the workspace hygiene heading" || true)"
                if [ -z "$first_failure" ]; then
                    first_failure="$(assert_contains "$stdout_artifact" "Stage candidates:" "human output should include stage candidate summary" || true)"
                fi
                if [ -z "$first_failure" ]; then
                    first_failure="$(assert_contains "$stdout_artifact" "source: 1 paths" "human output should summarize the source staging group" || true)"
                fi
                if [ -z "$first_failure" ]; then
                    first_failure="$(assert_contains "$stdout_artifact" "tests: 1 paths" "human output should summarize the tests staging group" || true)"
                fi
                if [ -z "$first_failure" ]; then
                    first_failure="$(assert_not_json "$stdout_artifact" "human output should not be a JSON envelope" || true)"
                fi
                ;;
            human_secret_no_leak)
                first_failure="$(assert_contains "$stdout_artifact" "Workspace hygiene:" "human secret output should include the workspace hygiene heading" || true)"
                if [ -z "$first_failure" ]; then
                    first_failure="$(assert_contains "$stdout_artifact" "Do not commit:" "human secret output should include do-not-commit summary" || true)"
                fi
                if [ -z "$first_failure" ]; then
                    first_failure="$(assert_contains "$stdout_artifact" ".env.local" "human secret output should identify the risky path without its value" || true)"
                fi
                if [ -z "$first_failure" ]; then
                    first_failure="$(assert_no_raw_value "$scenario human stdout" "$stdout_artifact" "$SYNTHETIC_RAW_VALUE" || true)"
                fi
                if [ -z "$first_failure" ]; then
                    first_failure="$(assert_no_raw_value "$scenario human stderr" "$stderr_artifact" "$SYNTHETIC_RAW_VALUE" || true)"
                fi
                if [ -z "$first_failure" ]; then
                    first_failure="$(assert_not_json "$stdout_artifact" "human secret output should not be a JSON envelope" || true)"
                fi
                ;;
            scratch_only)
                first_failure="$(assert_jq "$stdout_artifact" '(.data.dirtyPathCount == 2) and (.data.stagingRecommendations | length == 0) and (.data.doNotCommit == ["drift-report.txt", "ubs.json"]) and ([.data.pathClassifications[].path] == ["drift-report.txt", "ubs.json"]) and ([.data.bucketCounts[] | select(.name == "do_not_commit") | .count] == [2]) and ([.data.kindCounts[] | select(.name == "scratch") | .count] == [2]) and all(.data.pathClassifications[]; .bucket == "do_not_commit" and .kind == "scratch")' "scratch-only paths should stay doNotCommit in deterministic order and out of staging" || true)"
                ;;
            generated_only)
                first_failure="$(assert_jq "$stdout_artifact" '(.data.dirtyPathCount == 3) and (.data.stagingRecommendations | length == 0) and (.data.doNotCommit == ["Cargo.lock", "target/debug/ee", "target/release/deps/foo.rlib"]) and ([.data.pathClassifications[].path] == ["Cargo.lock", "target/debug/ee", "target/release/deps/foo.rlib"]) and ([.data.bucketCounts[] | select(.name == "do_not_commit") | .count] == [3]) and ([.data.kindCounts[] | select(.name == "generated") | .count] == [3]) and all(.data.pathClassifications[]; .bucket == "do_not_commit" and .kind == "generated")' "generated-only paths should stay doNotCommit in deterministic order and out of staging" || true)"
                ;;
            scratch_generated_secret)
                first_failure="$(assert_jq "$stdout_artifact" '(.data.dirtyPathCount == 3) and (.data.stagingRecommendations | length == 0) and (.data.doNotCommit == [".env.local", "Cargo.lock", "drift-report.txt"]) and ([.data.pathClassifications[].path] == [".env.local", "Cargo.lock", "drift-report.txt"]) and ([.data.bucketCounts[] | select(.name == "do_not_commit") | .count] == [3]) and ([.data.kindCounts[] | select(.name == "generated") | .count] == [1]) and ([.data.kindCounts[] | select(.name == "scratch") | .count] == [1]) and ([.data.kindCounts[] | select(.name == "secret_risk") | .count] == [1]) and all(.data.pathClassifications[]; .bucket == "do_not_commit")' "scratch/generated/secret paths should stay doNotCommit in deterministic order and out of staging" || true)"
                if [ -z "$first_failure" ]; then
                    first_failure="$(assert_no_raw_value "$scenario JSON" "$stdout_artifact" "$SYNTHETIC_RAW_VALUE" || true)"
                fi
                if [ -z "$first_failure" ]; then
                    first_failure="$(assert_no_raw_value "$scenario stderr" "$stderr_artifact" "$SYNTHETIC_RAW_VALUE" || true)"
                fi
                ;;
            large_binary_scan_skip)
                first_failure="$(assert_jq "$stdout_artifact" '(.data.dirtyPathCount == 2) and (.data.secretScan.readOnly == true) and (.data.secretScan.skippedContentScanCount >= 2) and (.data.secretScan.maxFileBytes == 65536) and (.data.degraded | index("workspace_hygiene_secret_scan_skipped"))' "large and binary dirty files should report skipped secret scans" || true)"
                ;;
            active_reservation)
                first_failure="$(assert_jq "$stdout_artifact" '.data.coordinationState.agentMailAvailable == true and (.data.coordinationState.blockedByCoordination[0].path == "src/lib.rs") and (.data.coordinationState.blockedByCoordination[0].holderAgent == "OtherAgent") and (.data.coordinationState.blockedByCoordination[0].pathPattern == "src/lib.rs") and (.data.coordinationState.blockedByCoordination[0].exclusive == true) and ([.data.stagingRecommendations[].paths[]?] | index("src/lib.rs") | not) and (.data.degraded | index("workspace_hygiene_agent_mail_unavailable") | not)' "active reservation should block src/lib.rs and keep it out of staging" || true)"
                ;;
            agent_mail_empty_snapshot)
                first_failure="$(assert_jq "$stdout_artifact" '.data.coordinationState.agentMailAvailable == true and (.data.coordinationState.blockedByCoordination | length == 0) and (.data.coordinationState.activeAgents | length == 0) and ([.data.stagingRecommendations[].paths[]?] | index("src/lib.rs")) and (.data.degraded | index("workspace_hygiene_agent_mail_unavailable") | not)' "empty Agent Mail snapshot should be available and leave src/lib.rs stageable" || true)"
                ;;
            agent_mail_unavailable)
                first_failure="$(assert_jq "$stdout_artifact" '(.data.coordinationState.agentMailAvailable == false) and (.data.coordinationState.blockedByCoordination | length == 0) and (.data.coordinationState.activeAgents | length == 0) and (.data.degraded | index("workspace_hygiene_agent_mail_unavailable")) and (.data.degraded | index("workspace_hygiene_partial_metadata"))' "missing snapshot should emit Agent Mail unavailable posture and degraded codes" || true)"
                ;;
            beads_pending_flush)
                first_failure="$(assert_jq "$stdout_artifact" '.data.beadsState.classification == "beads_db_dirty_pending_flush" and .data.beadsState.metadataSignal == "db_dirty_pending_flush"' "beads DB marker should report pending flush" || true)"
                ;;
            beads_export_only)
                first_failure="$(assert_jq "$stdout_artifact" '.data.beadsState.classification == "beads_export_only" and .data.beadsState.metadataSignal == "unknown" and (.data.beadsState.degradedCodes | index("workspace_hygiene_beads_db_divergence_unknown")) and (.data.degraded | index("workspace_hygiene_beads_db_divergence_unknown"))' "dirty Beads JSONL without DB signal should report export-only with divergence-unknown degradation" || true)"
                ;;
            beads_parse_failure)
                first_failure="$(assert_jq "$stdout_artifact" '(.data.degraded | index("workspace_hygiene_beads_parse_error")) and (.data.beadsState.degradedCodes | index("workspace_hygiene_beads_parse_error")) and .data.beadsState.classification == "beads_conflict_or_parse_error" and .data.beadsState.parseErrorLine == 2 and .data.beadsState.conflictMarkersFound == false and .data.beadsState.jsonlPosture.untracked == true' "invalid Beads JSONL should report parse-error classification and line 2" || true)"
                ;;
        esac
    fi

    read -r after_hash after_artifact < <(capture_workspace_state "$workspace" "${scenario}_after")
    if [ "$before_hash" != "$after_hash" ] && [ -z "$first_failure" ]; then
        first_failure="workspace hygiene mutated git-visible state for $scenario"
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
    human_source_and_test
    human_secret_no_leak
    scratch_only
    generated_only
    scratch_generated_secret
    large_binary_scan_skip
    active_reservation
    agent_mail_empty_snapshot
    agent_mail_unavailable
    beads_pending_flush
    beads_export_only
    beads_parse_failure
)

SCENARIO_PLAN_FAILURE="$(validate_scenario_plan || true)"
if [ -n "$SCENARIO_PLAN_FAILURE" ]; then
    emit_event "scenario_plan" "scenario_plan" "failed" 1 "validate workspace hygiene scenario plan" "$REPO_ROOT" "$EVENT_ROOT/scenario_plan.json" "$EVENT_ROOT/scenario_plan_diagnostics.txt" "failed" "$SCENARIO_PLAN_FAILURE" '["workspace_hygiene_scenario_plan_invalid"]' "$REPO_BEFORE_HASH" "" "$REPO_BEFORE_ARTIFACT" ""
    printf '%s\n' "$SCENARIO_PLAN_FAILURE" >&2
    exit 1
fi
emit_event "scenario_plan" "scenario_plan" "pass" 0 "validate workspace hygiene scenario plan" "$REPO_ROOT" "$EVENT_ROOT/scenario_plan.json" "$EVENT_ROOT/scenario_plan_diagnostics.txt" "passed" "" "[]" "$REPO_BEFORE_HASH" "" "$REPO_BEFORE_ARTIFACT" ""

for scenario in "${SCENARIOS[@]}"; do
    run_scenario "$scenario"
done

EVENT_LOG_REDACTION_FAILURE="$(assert_no_raw_value "event log" "$EVENT_LOG" "$SYNTHETIC_RAW_VALUE" || true)"
if [ -n "$EVENT_LOG_REDACTION_FAILURE" ]; then
    emit_event "event_log_redaction" "redaction_check" "failed" 1 "grep event log for raw synthetic secret" "$REPO_ROOT" "$EVENT_LOG" "" "failed" "$EVENT_LOG_REDACTION_FAILURE" '["workspace_hygiene_redaction_check_failed"]' "$REPO_BEFORE_HASH" "" "$REPO_BEFORE_ARTIFACT" ""
    printf '%s\n' "$EVENT_LOG_REDACTION_FAILURE" >&2
    exit 1
fi
emit_event "event_log_redaction" "redaction_check" "pass" 0 "grep event log for raw synthetic secret" "$REPO_ROOT" "$EVENT_LOG" "" "passed" "" "[]" "$REPO_BEFORE_HASH" "" "$REPO_BEFORE_ARTIFACT" ""

EVENT_ARTIFACT_REDACTION_FAILURE="$(validate_event_artifact_redaction || true)"
if [ -n "$EVENT_ARTIFACT_REDACTION_FAILURE" ]; then
    emit_event "event_artifact_redaction" "artifact_redaction_check" "failed" 1 "grep event artifacts for raw synthetic secret" "$REPO_ROOT" "$EVENT_ROOT/event_artifact_redaction_diagnostics.txt" "" "failed" "$EVENT_ARTIFACT_REDACTION_FAILURE" '["workspace_hygiene_redaction_check_failed"]' "$REPO_BEFORE_HASH" "" "$REPO_BEFORE_ARTIFACT" ""
    printf '%s\n' "$EVENT_ARTIFACT_REDACTION_FAILURE" >&2
    exit 1
fi
emit_event "event_artifact_redaction" "artifact_redaction_check" "pass" 0 "grep event artifacts for raw synthetic secret" "$REPO_ROOT" "$EVENT_ROOT/event_artifact_redaction_diagnostics.txt" "" "passed" "" "[]" "$REPO_BEFORE_HASH" "" "$REPO_BEFORE_ARTIFACT" ""

STDIO_FAILURE="$(validate_stdout_stderr_isolation || true)"
if [ -n "$STDIO_FAILURE" ]; then
    emit_event "stdout_stderr_isolation" "stdout_stderr_isolation" "failed" 1 "validate stdout/stderr artifact separation" "$REPO_ROOT" "$EVENT_ROOT/stdout_stderr_isolation_diagnostics.txt" "" "failed" "$STDIO_FAILURE" '["workspace_hygiene_stdout_stderr_isolation_failed"]' "$REPO_BEFORE_HASH" "" "$REPO_BEFORE_ARTIFACT" ""
    printf '%s\n' "$STDIO_FAILURE" >&2
    exit 1
fi
emit_event "stdout_stderr_isolation" "stdout_stderr_isolation" "pass" 0 "validate stdout/stderr artifact separation" "$REPO_ROOT" "$EVENT_ROOT/stdout_stderr_isolation_diagnostics.txt" "" "passed" "" "[]" "$REPO_BEFORE_HASH" "" "$REPO_BEFORE_ARTIFACT" ""

ARTIFACT_REFERENCE_FAILURE="$(validate_event_artifact_references || true)"
if [ -n "$ARTIFACT_REFERENCE_FAILURE" ]; then
    emit_event "artifact_reference_contract" "artifact_reference_contract" "failed" 1 "validate event artifact references exist" "$REPO_ROOT" "$EVENT_ROOT/event_artifact_reference_diagnostics.txt" "" "failed" "$ARTIFACT_REFERENCE_FAILURE" '["workspace_hygiene_artifact_reference_contract_failed"]' "$REPO_BEFORE_HASH" "" "$REPO_BEFORE_ARTIFACT" ""
    printf '%s\n' "$ARTIFACT_REFERENCE_FAILURE" >&2
    exit 1
fi
emit_event "artifact_reference_contract" "artifact_reference_contract" "pass" 0 "validate event artifact references exist" "$REPO_ROOT" "$EVENT_ROOT/event_artifact_reference_diagnostics.txt" "" "passed" "" "[]" "$REPO_BEFORE_HASH" "" "$REPO_BEFORE_ARTIFACT" ""

MUTATION_ARTIFACT_FAILURE="$(validate_mutation_artifact_contract || true)"
if [ -n "$MUTATION_ARTIFACT_FAILURE" ]; then
    emit_event "mutation_artifact_contract" "mutation_artifact_contract" "failed" 1 "validate scenario mutation artifacts include file fingerprints" "$REPO_ROOT" "$EVENT_ROOT/mutation_artifact_contract_diagnostics.txt" "" "failed" "$MUTATION_ARTIFACT_FAILURE" '["workspace_hygiene_mutation_artifact_contract_failed"]' "$REPO_BEFORE_HASH" "" "$REPO_BEFORE_ARTIFACT" ""
    printf '%s\n' "$MUTATION_ARTIFACT_FAILURE" >&2
    exit 1
fi
emit_event "mutation_artifact_contract" "mutation_artifact_contract" "pass" 0 "validate scenario mutation artifacts include file fingerprints" "$REPO_ROOT" "$EVENT_ROOT/mutation_artifact_contract_diagnostics.txt" "" "passed" "" "[]" "$REPO_BEFORE_HASH" "" "$REPO_BEFORE_ARTIFACT" ""

LOCAL_CARGO_FAILURE="$(validate_no_local_cargo_commands || true)"
if [ -n "$LOCAL_CARGO_FAILURE" ]; then
    emit_event "local_cargo_guard" "local_cargo_guard" "failed" 1 "validate no direct local Cargo/rustc commands in event log" "$REPO_ROOT" "$EVENT_ROOT/local_cargo_command_guard_diagnostics.txt" "" "failed" "$LOCAL_CARGO_FAILURE" '["workspace_hygiene_local_cargo_command_logged"]' "$REPO_BEFORE_HASH" "" "$REPO_BEFORE_ARTIFACT" ""
    printf '%s\n' "$LOCAL_CARGO_FAILURE" >&2
    exit 1
fi
emit_event "local_cargo_guard" "local_cargo_guard" "pass" 0 "validate no direct local Cargo/rustc commands in event log" "$REPO_ROOT" "$EVENT_ROOT/local_cargo_command_guard_diagnostics.txt" "" "passed" "" "[]" "$REPO_BEFORE_HASH" "" "$REPO_BEFORE_ARTIFACT" ""

read -r REPO_AFTER_HASH REPO_AFTER_ARTIFACT < <(capture_repo_state "after")
if [ "$REPO_BEFORE_HASH" != "$REPO_AFTER_HASH" ]; then
    emit_event "teardown" "mutation_check" "failed" 1 "compare caller checkout state" "$REPO_ROOT" "" "" "not_run" "caller checkout git-visible state changed" '["workspace_hygiene_read_only_violation"]' "$REPO_BEFORE_HASH" "$REPO_AFTER_HASH" "$REPO_BEFORE_ARTIFACT" "$REPO_AFTER_ARTIFACT"
    exit 1
fi

emit_event "teardown" "mutation_check" "pass" 0 "compare caller checkout state" "$REPO_ROOT" "" "" "not_run" "" "[]" "$REPO_BEFORE_HASH" "$REPO_AFTER_HASH" "$REPO_BEFORE_ARTIFACT" "$REPO_AFTER_ARTIFACT"
emit_event "teardown" "teardown" "pass" 0 "complete workspace hygiene e2e run" "$REPO_ROOT" "" "" "not_run" "" "[]" "$REPO_BEFORE_HASH" "$REPO_AFTER_HASH" "$REPO_BEFORE_ARTIFACT" "$REPO_AFTER_ARTIFACT"

NEGATIVE_CONTRACT_FAILURE="$(validate_event_log_negative_contracts || true)"
if [ -n "$NEGATIVE_CONTRACT_FAILURE" ]; then
    emit_event "event_log_negative_contracts" "negative_contract_check" "failed" 1 "validate malformed event logs are rejected" "$REPO_ROOT" "$EVENT_ROOT/event_log_negative_contracts_diagnostics.txt" "" "failed" "$NEGATIVE_CONTRACT_FAILURE" '["workspace_hygiene_event_log_contract_failed"]' "$REPO_BEFORE_HASH" "$REPO_AFTER_HASH" "$REPO_BEFORE_ARTIFACT" "$REPO_AFTER_ARTIFACT"
    printf '%s\n' "$NEGATIVE_CONTRACT_FAILURE" >&2
    exit 1
fi
emit_event "event_log_negative_contracts" "negative_contract_check" "pass" 0 "validate malformed event logs are rejected" "$REPO_ROOT" "$EVENT_ROOT/event_log_negative_contracts_diagnostics.txt" "" "passed" "" "[]" "$REPO_BEFORE_HASH" "$REPO_AFTER_HASH" "$REPO_BEFORE_ARTIFACT" "$REPO_AFTER_ARTIFACT"

EVENT_LOG_FAILURE="$(validate_event_log_contract || true)"
if [ -n "$EVENT_LOG_FAILURE" ]; then
    emit_event "event_log_contract" "schema_check" "failed" 1 "jq validate ee.test_event.v1 events" "$REPO_ROOT" "$EVENT_LOG" "" "failed" "$EVENT_LOG_FAILURE" '["workspace_hygiene_event_log_contract_failed"]' "$REPO_BEFORE_HASH" "$REPO_AFTER_HASH" "$REPO_BEFORE_ARTIFACT" "$REPO_AFTER_ARTIFACT"
    printf '%s\n' "$EVENT_LOG_FAILURE" >&2
    exit 1
fi
emit_event "event_log_contract" "schema_check" "pass" 0 "jq validate ee.test_event.v1 events" "$REPO_ROOT" "$EVENT_LOG" "" "passed" "" "[]" "$REPO_BEFORE_HASH" "$REPO_AFTER_HASH" "$REPO_BEFORE_ARTIFACT" "$REPO_AFTER_ARTIFACT"

FINAL_ARTIFACT_REDACTION_FAILURE="$(validate_event_artifact_redaction "$EVENT_LOG" "$EVENT_ROOT/final_event_artifact_redaction_diagnostics.txt" || true)"
if [ -n "$FINAL_ARTIFACT_REDACTION_FAILURE" ]; then
    emit_event "event_artifact_redaction_final" "artifact_redaction_check" "failed" 1 "grep complete event artifacts for raw synthetic secret" "$REPO_ROOT" "" "" "failed" "$FINAL_ARTIFACT_REDACTION_FAILURE" '["workspace_hygiene_redaction_check_failed"]' "" "" "" ""
    printf '%s\n' "$FINAL_ARTIFACT_REDACTION_FAILURE" >&2
    exit 1
fi
emit_event "event_artifact_redaction_final" "artifact_redaction_check" "pass" 0 "grep complete event artifacts for raw synthetic secret" "$REPO_ROOT" "" "" "passed" "" "[]" "" "" "" ""

FINAL_ARTIFACT_REFERENCE_FAILURE="$(validate_event_artifact_references "$EVENT_LOG" "$EVENT_ROOT/final_event_artifact_reference_diagnostics.txt" || true)"
if [ -n "$FINAL_ARTIFACT_REFERENCE_FAILURE" ]; then
    emit_event "artifact_reference_contract_final" "artifact_reference_contract" "failed" 1 "validate complete event artifact references" "$REPO_ROOT" "" "" "failed" "$FINAL_ARTIFACT_REFERENCE_FAILURE" '["workspace_hygiene_artifact_reference_contract_failed"]' "" "" "" ""
    printf '%s\n' "$FINAL_ARTIFACT_REFERENCE_FAILURE" >&2
    exit 1
fi
emit_event "artifact_reference_contract_final" "artifact_reference_contract" "pass" 0 "validate complete event artifact references" "$REPO_ROOT" "" "" "passed" "" "[]" "" "" "" ""

FINAL_EVENT_LOG_CONTRACT_FAILURE="$(validate_event_log_contract "$EVENT_LOG" "$EVENT_ROOT/final_event_log_contract_diagnostics.txt" || true)"
if [ -n "$FINAL_EVENT_LOG_CONTRACT_FAILURE" ]; then
    printf '%s\n' "$FINAL_EVENT_LOG_CONTRACT_FAILURE" >&2
    exit 1
fi

if [ "$SELF_TEST_CONTRACTS" = true ]; then
    printf 'workspace_hygiene: self-test contracts passed; events=%s\n' "$EVENT_LOG" >&2
else
    printf 'workspace_hygiene: all scenarios passed; events=%s\n' "$EVENT_LOG" >&2
fi
