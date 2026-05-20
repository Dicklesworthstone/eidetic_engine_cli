#!/usr/bin/env bash
# J11.4 — Non-destructive bead closeout audit (bd-17c65.10.11.4).
#
# Run BEFORE marking a bead closed to summarize verification
# evidence, retained artifacts, dependencies, and known caveats.
# Reports closure readiness as `ready`, `ready_with_caveats`, or
# `blocked`, with structured reasons an agent can act on or print
# verbatim into a `br close --reason` invocation.
#
# **Non-destructive contract:** this script NEVER mutates beads,
# git, files, agent-mail reservations, or the cargo target.
# It reads, classifies, and reports. Closing the bead is still an
# explicit operator action (`br close <id> --reason ...`).
#
# Usage:
#   scripts/closeout_audit.sh --bead <id> [--json] [--workspace-root <path>]
#
# Exit codes:
#   0  success — readiness emitted (could be any of ready/caveats/blocked)
#   2  usage error (bad args)
#   3  bead not found
#   4  required tool missing (jq, git)
#
# JSON schema: ee.closeout_audit.v1
#
# Wired by tests/closeout_audit_runner_unit.rs which invokes the
# script against three fixture scenarios (ready, ready_with_caveats,
# blocked) and asserts the readiness classification + structural
# shape of the JSON output.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT_DEFAULT="$(cd "$SCRIPT_DIR/.." && pwd)"
ORIGINAL_CWD="$(pwd)"
SCHEMA_ID="ee.closeout_audit.v1"

usage() {
    cat <<'USAGE'
usage: scripts/closeout_audit.sh --bead <id> [--json] [--workspace-root <path>]

Examples:
  scripts/closeout_audit.sh --bead bd-17c65.4.9 --json
  scripts/closeout_audit.sh --bead bd-17c65.11.3 --workspace-root /tmp/test-ws --json
USAGE
}

# Argument parsing.
BEAD_ID=""
JSON_OUTPUT=0
WORKSPACE_ROOT="$REPO_ROOT_DEFAULT"

while [ "$#" -gt 0 ]; do
    case "$1" in
        --bead)
            BEAD_ID="${2:-}"
            if [ -z "$BEAD_ID" ]; then
                echo "closeout_audit: --bead requires a value" >&2
                usage >&2
                exit 2
            fi
            shift 2
            ;;
        --json)
            JSON_OUTPUT=1
            shift
            ;;
        --workspace-root)
            WORKSPACE_ROOT="${2:-}"
            if [ -z "$WORKSPACE_ROOT" ]; then
                echo "closeout_audit: --workspace-root requires a path" >&2
                usage >&2
                exit 2
            fi
            shift 2
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            echo "closeout_audit: unknown argument: $1" >&2
            usage >&2
            exit 2
            ;;
    esac
done

if [ -z "$BEAD_ID" ]; then
    echo "closeout_audit: --bead is required" >&2
    usage >&2
    exit 2
fi

case "$WORKSPACE_ROOT" in
    /*) ;;
    *) WORKSPACE_ROOT="$ORIGINAL_CWD/$WORKSPACE_ROOT" ;;
esac

RCH_QUEUE_JSON_RESOLVED="${RCH_QUEUE_JSON:-}"
if [ -n "$RCH_QUEUE_JSON_RESOLVED" ]; then
    case "$RCH_QUEUE_JSON_RESOLVED" in
        /*) ;;
        *) RCH_QUEUE_JSON_RESOLVED="$ORIGINAL_CWD/$RCH_QUEUE_JSON_RESOLVED" ;;
    esac
fi
RCH_PROBE_TIMEOUT_SECONDS="${RCH_PROBE_TIMEOUT_SECONDS:-4}"
case "$RCH_PROBE_TIMEOUT_SECONDS" in
    ''|*[!0-9]*|0) RCH_PROBE_TIMEOUT_SECONDS=4 ;;
esac

run_bounded_command() {
    local seconds="${1:?seconds required}"
    shift
    if command -v timeout >/dev/null 2>&1; then
        timeout "${seconds}s" bash -c '"$@"' closeout-audit "$@"
        return $?
    fi
    if command -v gtimeout >/dev/null 2>&1; then
        gtimeout "${seconds}s" bash -c '"$@"' closeout-audit "$@"
        return $?
    fi
    "$@" &
    local command_pid=$!
    (
        sleep "$seconds"
        kill "$command_pid" 2>/dev/null || true
    ) &
    local watchdog_pid=$!
    local status=0
    wait "$command_pid" || status=$?
    kill "$watchdog_pid" 2>/dev/null || true
    wait "$watchdog_pid" 2>/dev/null || true
    return "$status"
}

is_timeout_status() {
    local status="${1:?status required}"
    [ "$status" -eq 124 ] || [ "$status" -ge 128 ]
}

# Tool preflight. jq is required for JSONL parsing; git is required
# for uncommitted-references scan.
for tool in jq git; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        echo "closeout_audit: required tool missing: $tool" >&2
        exit 4
    fi
done

ISSUES_JSONL="$WORKSPACE_ROOT/.beads/issues.jsonl"
if [ ! -f "$ISSUES_JSONL" ]; then
    echo "closeout_audit: no beads JSONL at $ISSUES_JSONL" >&2
    exit 3
fi

# Extract the bead's JSON line. The JSONL has one issue per line so a
# fixed-string grep + jq filter is reliable + fast.
BEAD_JSON="$(grep -F "\"id\":\"$BEAD_ID\"" "$ISSUES_JSONL" | head -1 || true)"
if [ -z "$BEAD_JSON" ]; then
    echo "closeout_audit: bead not found in $ISSUES_JSONL: $BEAD_ID" >&2
    exit 3
fi

BEAD_STATUS="$(printf '%s' "$BEAD_JSON" | jq -r '.status // "unknown"')"
BEAD_ASSIGNEE="$(printf '%s' "$BEAD_JSON" | jq -r '.assignee // ""')"
BEAD_TITLE="$(printf '%s' "$BEAD_JSON" | jq -r '.title // ""')"
SRR6_CLOSEOUT_ENABLED=false
if [ "$BEAD_ID" = "bd-2vu8m" ]; then
    SRR6_CLOSEOUT_ENABLED=true
fi

# Collect open dependencies (the bead is blocked by these).
DEPS_JSON="$(printf '%s' "$BEAD_JSON" | jq -c '[.dependencies // [] | .[] | select(.type == "blocks") | .depends_on_id]')"

# For each dep, look up its status; open deps go into a blocker list.
OPEN_DEPS_JSON="[]"
if [ "$DEPS_JSON" != "[]" ] && [ "$DEPS_JSON" != "null" ]; then
    OPEN_DEPS_BUFFER="$(mktemp)"
    trap 'rm -f "$OPEN_DEPS_BUFFER"' EXIT
    printf '%s\n' "$DEPS_JSON" | jq -r '.[]' | while IFS= read -r dep_id; do
        [ -z "$dep_id" ] && continue
        dep_line="$(grep -F "\"id\":\"$dep_id\"" "$ISSUES_JSONL" | head -1 || true)"
        if [ -z "$dep_line" ]; then
            printf '{"id":"%s","status":"missing"}\n' "$dep_id" >> "$OPEN_DEPS_BUFFER"
            continue
        fi
        dep_status="$(printf '%s' "$dep_line" | jq -r '.status // "unknown"')"
        if [ "$dep_status" != "closed" ] && [ "$dep_status" != "deferred" ]; then
            jq -nc --arg id "$dep_id" --arg s "$dep_status" '{id:$id,status:$s}' >> "$OPEN_DEPS_BUFFER"
        fi
    done
    if [ -s "$OPEN_DEPS_BUFFER" ]; then
        OPEN_DEPS_JSON="$(jq -s '.' < "$OPEN_DEPS_BUFFER")"
    fi
fi

# Check the dependency graph directly from JSONL so closeout does not rely on a
# live br daemon. This mirrors the `br dep cycles` gate at the evidence level:
# any cycle in the tracked bead graph blocks closure until the graph is fixed.
DEPENDENCY_CYCLES_JSON="$(jq -s -c '
    def dependency_targets($issue):
        [($issue.dependencies // [])[]?
            | (.depends_on_id // .id // empty)
            | select(type == "string" and length > 0)];
    (map(select(.id? != null))
        | map({key: .id, value: dependency_targets(.)})
        | from_entries) as $edges
    | def walk($id; $path):
        if ($path | index($id)) then
            [($path + [$id])]
        else
            ([($edges[$id] // [])[]? as $next | walk($next; $path + [$id])] | add) // []
        end;
    [$edges | keys[] as $root | walk($root; [])[]]
    | map(select(length > 1))
    | map({path: ., cycle: join(" -> ")})
    | unique_by(.cycle)
' "$ISSUES_JSONL" 2>/dev/null || printf '[]')"
DEPENDENCY_CYCLE_COUNT="$(printf '%s' "$DEPENDENCY_CYCLES_JSON" | jq 'length')"

# SRR6 final closeout has a stricter contract than ordinary bead closure:
# every child must be closed/deferred and the optional mesh-off proof paths must
# still exist. This keeps bd-2vu8m from being marked ready by a generic audit
# while SRR6 rows are still unresolved.
SRR6_CLOSEOUT_STATUS="not_applicable"
SRR6_MATRIX_PATH=""
SRR6_MATRIX_PRESENT=false
SRR6_MATRIX_ROW_PRESENT=false
SRR6_REQUIRED_PROOFS_JSON="[]"
SRR6_MISSING_PROOFS_JSON="[]"
SRR6_MISSING_PROOF_MARKERS_JSON="[]"
SRR6_UNRESOLVED_DEPS_JSON="[]"
if [ "$SRR6_CLOSEOUT_ENABLED" = "true" ]; then
    SRR6_MATRIX_PATH="docs/mesh/verification_matrix.md"
    SRR6_MATRIX_ABS="$WORKSPACE_ROOT/$SRR6_MATRIX_PATH"
    if [ -f "$SRR6_MATRIX_ABS" ]; then
        SRR6_MATRIX_PRESENT=true
        if grep -F "| bd-2vu8m |" "$SRR6_MATRIX_ABS" >/dev/null 2>&1; then
            SRR6_MATRIX_ROW_PRESENT=true
        fi
    fi

    SRR6_REQUIRED_PROOFS=(
        "docs/mesh/verification_matrix.md"
        "scripts/e2e_overhaul/mesh_off_no_network.sh"
        "tests/mesh_off_no_network.rs"
        "tests/fixtures/golden/mesh/mesh_off_no_network.commands.json.golden"
    )
    SRR6_REQUIRED_PROOFS_JSON="$(printf '%s\n' "${SRR6_REQUIRED_PROOFS[@]}" \
        | jq -Rn '[inputs | select(length > 0)]')"
    SRR6_MISSING_PROOFS_LINES=""
    for proof_path in "${SRR6_REQUIRED_PROOFS[@]}"; do
        if [ ! -f "$WORKSPACE_ROOT/$proof_path" ]; then
            SRR6_MISSING_PROOFS_LINES="${SRR6_MISSING_PROOFS_LINES}${proof_path}"$'\n'
        fi
    done
    if [ -n "$SRR6_MISSING_PROOFS_LINES" ]; then
        SRR6_MISSING_PROOFS_JSON="$(printf '%s' "$SRR6_MISSING_PROOFS_LINES" \
            | jq -Rn '[inputs | select(length > 0)]')"
    fi

    SRR6_MISSING_PROOF_MARKERS_LINES=""
    srr6_require_marker() {
        local proof_path="${1:?proof_path required}"
        local marker="${2:?marker required}"
        if [ ! -f "$WORKSPACE_ROOT/$proof_path" ]; then
            return
        fi
        if ! grep -F "$marker" "$WORKSPACE_ROOT/$proof_path" >/dev/null 2>&1; then
            SRR6_MISSING_PROOF_MARKERS_LINES="${SRR6_MISSING_PROOF_MARKERS_LINES}${proof_path}"$'\t'"${marker}"$'\n'
        fi
    }
    srr6_require_marker "docs/mesh/verification_matrix.md" "Tests must not require real Tailscale"
    srr6_require_marker "docs/mesh/verification_matrix.md" "| bd-2vu8m |"
    srr6_require_marker "scripts/e2e_overhaul/mesh_off_no_network.sh" "export EE_MESH_ENABLED=0"
    srr6_require_marker "scripts/e2e_overhaul/mesh_off_no_network.sh" "mesh_off_status_opens_no_mesh_listener"
    srr6_require_marker "tests/mesh_off_no_network.rs" ".env(\"EE_MESH_ENABLED\", \"0\")"
    srr6_require_marker "tests/mesh_off_no_network.rs" "assert_no_new_mesh_listener"
    srr6_require_marker "tests/mesh_off_no_network.rs" "mesh_off_no_network.commands.json.golden"
    srr6_require_marker "tests/fixtures/golden/mesh/mesh_off_no_network.commands.json.golden" "\"meshOrPeerCodes\""
    srr6_require_marker "tests/fixtures/golden/mesh/mesh_off_no_network.commands.json.golden" "\"meshOrPeerDataKeys\""
    if [ -n "$SRR6_MISSING_PROOF_MARKERS_LINES" ]; then
        SRR6_MISSING_PROOF_MARKERS_JSON="$(printf '%s' "$SRR6_MISSING_PROOF_MARKERS_LINES" \
            | jq -R -s 'split("\n") | map(select(length > 0) | split("\t") | {path: .[0], marker: .[1]})')"
    fi

    SRR6_UNRESOLVED_DEPS_JSON="$OPEN_DEPS_JSON"
    SRR6_MISSING_PROOFS_COUNT="$(printf '%s' "$SRR6_MISSING_PROOFS_JSON" | jq 'length')"
    SRR6_MISSING_PROOF_MARKERS_COUNT="$(printf '%s' "$SRR6_MISSING_PROOF_MARKERS_JSON" | jq 'length')"
    SRR6_UNRESOLVED_DEPS_COUNT="$(printf '%s' "$SRR6_UNRESOLVED_DEPS_JSON" | jq 'length')"
    if [ "$SRR6_MATRIX_PRESENT" != "true" ] \
        || [ "$SRR6_MATRIX_ROW_PRESENT" != "true" ] \
        || [ "$SRR6_MISSING_PROOFS_COUNT" -gt 0 ] \
        || [ "$SRR6_MISSING_PROOF_MARKERS_COUNT" -gt 0 ] \
        || [ "$SRR6_UNRESOLVED_DEPS_COUNT" -gt 0 ]; then
        SRR6_CLOSEOUT_STATUS="blocked"
    else
        SRR6_CLOSEOUT_STATUS="ready"
    fi
fi

# Count uncommitted files in git that reference the bead id. Many
# beads are referenced in commit messages on closure; if there are
# uncommitted files still mentioning the bead, the work may not be
# finished.
UNCOMMITTED_REFS_JSON="[]"
if cd "$WORKSPACE_ROOT" 2>/dev/null; then
    UNCOMMITTED_REFS_RAW="$(git status --porcelain 2>/dev/null \
        | awk 'NF >= 2 && $1 != "??" { sub(/^...\W*/, ""); print }' \
        | grep -vE '^\.beads/issues\.jsonl$' \
        | xargs -I{} sh -c 'grep -lF "'"$BEAD_ID"'" "{}" 2>/dev/null || true' \
        | sort -u || true)"
    if [ -n "$UNCOMMITTED_REFS_RAW" ]; then
        UNCOMMITTED_REFS_JSON="$(printf '%s\n' "$UNCOMMITTED_REFS_RAW" \
            | jq -Rn '[inputs | select(length > 0)]')"
    fi
fi

# Check rch readiness. `rch check` returns 0 when workers reachable.
# We don't probe whether a specific build was offloaded — that's a
# per-invocation property — only whether rch as a system is healthy.
RCH_STATUS="unknown"
RCH_QUEUE_STATUS="unknown"
RCH_ACTIVE_BUILDS=0
RCH_STALE_ACTIVE_BUILDS=0
RCH_QUEUED_BUILDS=0
if command -v rch >/dev/null 2>&1; then
    RCH_CHECK_EXIT=0
    if run_bounded_command "$RCH_PROBE_TIMEOUT_SECONDS" rch check >/dev/null 2>&1; then
        RCH_STATUS="ready"
    else
        RCH_CHECK_EXIT=$?
        if is_timeout_status "$RCH_CHECK_EXIT"; then
            RCH_STATUS="timeout"
        else
            RCH_STATUS="local_fallback_likely"
        fi
    fi
fi

RCH_QUEUE_TIMED_OUT=false
RCH_QUEUE_RAW=""
if [ -n "$RCH_QUEUE_JSON_RESOLVED" ] && [ -f "$RCH_QUEUE_JSON_RESOLVED" ]; then
    RCH_QUEUE_RAW="$(cat "$RCH_QUEUE_JSON_RESOLVED" 2>/dev/null || true)"
elif command -v rch >/dev/null 2>&1; then
    RCH_QUEUE_EXIT=0
    RCH_QUEUE_RAW="$(run_bounded_command "$RCH_PROBE_TIMEOUT_SECONDS" rch queue --json 2>/dev/null)" || RCH_QUEUE_EXIT=$?
    if is_timeout_status "$RCH_QUEUE_EXIT"; then
        RCH_QUEUE_TIMED_OUT=true
        RCH_QUEUE_RAW=""
    fi
fi
if [ "$RCH_QUEUE_TIMED_OUT" = "true" ]; then
    RCH_QUEUE_STATUS="timeout"
elif [ -n "$RCH_QUEUE_RAW" ] && printf '%s' "$RCH_QUEUE_RAW" | jq -e '.success == true and (.data | type == "object")' >/dev/null 2>&1; then
    RCH_ACTIVE_BUILDS="$(printf '%s' "$RCH_QUEUE_RAW" | jq '[.data.active_builds[]?] | length')"
    RCH_STALE_ACTIVE_BUILDS="$(printf '%s' "$RCH_QUEUE_RAW" | jq '[.data.active_builds[]? | select((.last_heartbeat_at == null) and (.last_progress_at == null))] | length')"
    RCH_QUEUED_BUILDS="$(printf '%s' "$RCH_QUEUE_RAW" | jq '[.data.queued_builds[]?] | length')"
    if [ "$RCH_STALE_ACTIVE_BUILDS" -gt 0 ]; then
        RCH_QUEUE_STATUS="stale_active_records"
    elif [ "$RCH_QUEUED_BUILDS" -gt 0 ]; then
        RCH_QUEUE_STATUS="queued"
    elif [ "$RCH_ACTIVE_BUILDS" -gt 0 ]; then
        RCH_QUEUE_STATUS="active"
    else
        RCH_QUEUE_STATUS="idle"
    fi
else
    RCH_QUEUE_STATUS="unavailable"
fi

# Check agent mail reachability. Same liveness probe used in other
# scripts. Note: an unreachable agent-mail server does NOT block
# closure — many beads have no agent-mail evidence — it just feeds
# into the caveat list.
AGENT_MAIL_STATUS="unknown"
AGENT_MAIL_HOST_PORT="${AGENT_MAIL_HOST:-127.0.0.1}:${AGENT_MAIL_PORT:-8765}"
if command -v curl >/dev/null 2>&1; then
    if curl -fsS --connect-timeout 2 --max-time 4 \
            "http://${AGENT_MAIL_HOST_PORT}/api/health" >/dev/null 2>&1 \
        || curl -fsS --connect-timeout 2 --max-time 4 \
            "http://${AGENT_MAIL_HOST_PORT}/health" >/dev/null 2>&1; then
        AGENT_MAIL_STATUS="reachable"
    else
        AGENT_MAIL_STATUS="unreachable"
    fi
fi

# Check J1 log presence — the structured-test-log path agents emit
# evidence into. Optional but useful if the bead's verification
# captured timed events.
J1_LOG_PRESENT=false
J1_LOG_PATH=""
if [ -d "$WORKSPACE_ROOT/tests/logs/active" ]; then
    if compgen -G "$WORKSPACE_ROOT/tests/logs/active/*.jsonl" > /dev/null; then
        J1_LOG_PRESENT=true
        J1_LOG_PATH="$WORKSPACE_ROOT/tests/logs/active"
    fi
fi

# Aggregate readiness.
BLOCKERS=()
CAVEATS=()
NEXT_ACTIONS=()

# Blockers
OPEN_DEPS_COUNT="$(printf '%s' "$OPEN_DEPS_JSON" | jq 'length')"
if [ "$OPEN_DEPS_COUNT" -gt 0 ]; then
    BLOCKERS+=("open_dependencies: ${OPEN_DEPS_COUNT} dep(s) not yet closed")
    NEXT_ACTIONS+=("close or force-close the open dependencies; review each via 'br show <id>'")
fi
if [ "$DEPENDENCY_CYCLE_COUNT" -gt 0 ]; then
    BLOCKERS+=("dependency_cycles: ${DEPENDENCY_CYCLE_COUNT} cycle(s) detected in the bead graph")
    NEXT_ACTIONS+=("break dependency cycles before closeout; verify with 'br dep cycles --json'")
fi
if [ "$SRR6_CLOSEOUT_ENABLED" = "true" ]; then
    SRR6_UNRESOLVED_DEPS_COUNT="$(printf '%s' "$SRR6_UNRESOLVED_DEPS_JSON" | jq 'length')"
    SRR6_MISSING_PROOFS_COUNT="$(printf '%s' "$SRR6_MISSING_PROOFS_JSON" | jq 'length')"
    if [ "$SRR6_UNRESOLVED_DEPS_COUNT" -gt 0 ]; then
        BLOCKERS+=("srr6_unresolved_dependencies: ${SRR6_UNRESOLVED_DEPS_COUNT} SRR6 dep(s) are not closed/deferred")
        NEXT_ACTIONS+=("defer or close every unresolved SRR6 dependency before closing bd-2vu8m")
    fi
    if [ "$SRR6_MATRIX_PRESENT" != "true" ]; then
        BLOCKERS+=("srr6_matrix_missing: docs/mesh/verification_matrix.md is required")
        NEXT_ACTIONS+=("restore the SRR6 verification matrix before closeout")
    elif [ "$SRR6_MATRIX_ROW_PRESENT" != "true" ]; then
        BLOCKERS+=("srr6_matrix_row_missing: bd-2vu8m row is required in docs/mesh/verification_matrix.md")
        NEXT_ACTIONS+=("add or restore the bd-2vu8m matrix row before closeout")
    fi
    if [ "$SRR6_MISSING_PROOFS_COUNT" -gt 0 ]; then
        BLOCKERS+=("srr6_mesh_off_proof_missing: ${SRR6_MISSING_PROOFS_COUNT} required proof path(s) missing")
        NEXT_ACTIONS+=("restore the missing mesh-off proof files before closeout")
    fi
    if [ "$SRR6_MISSING_PROOF_MARKERS_COUNT" -gt 0 ]; then
        BLOCKERS+=("srr6_mesh_off_marker_missing: ${SRR6_MISSING_PROOF_MARKERS_COUNT} required proof marker(s) missing")
        NEXT_ACTIONS+=("restore the required mesh-off assertions before closeout")
    fi
fi
UNCOMMITTED_REFS_COUNT="$(printf '%s' "$UNCOMMITTED_REFS_JSON" | jq 'length')"
if [ "$UNCOMMITTED_REFS_COUNT" -gt 0 ]; then
    BLOCKERS+=("uncommitted_files_reference_bead: ${UNCOMMITTED_REFS_COUNT} file(s) still mention ${BEAD_ID}")
    NEXT_ACTIONS+=("commit or revert the uncommitted files that reference ${BEAD_ID}")
fi

# Caveats
if [ "$RCH_STATUS" = "local_fallback_likely" ]; then
    CAVEATS+=("rch_health_check_failed: cargo evidence captured this session may have been local fallback rather than offloaded; verify before closure if the bead required remote builds")
    NEXT_ACTIONS+=("re-run cargo verification with explicit rch routing OR document the local-fallback context in the close_reason")
elif [ "$RCH_STATUS" = "timeout" ]; then
    CAVEATS+=("rch_health_check_timeout: rch check exceeded ${RCH_PROBE_TIMEOUT_SECONDS}s; cargo evidence remains unverified until a bounded remote exec succeeds")
    NEXT_ACTIONS+=("rerun RCH health checks separately before treating Cargo evidence as available")
fi
if [ "$RCH_QUEUE_STATUS" = "stale_active_records" ]; then
    CAVEATS+=("rch_queue_stale_active_records: ${RCH_STALE_ACTIVE_BUILDS} active RCH record(s) have no heartbeat/progress timestamp; cargo submissions may time out querying the daemon and fall back locally")
    NEXT_ACTIONS+=("inspect RCH before cargo: RCH_CANONICAL_PROJECT_ROOT=/Users/jemanuel/projects RCH_ALIAS_PROJECT_ROOT=/data/projects rch queue --json")
    NEXT_ACTIONS+=("if a wrapper reports 'Daemon response timed out' or 'running locally', stop that wrapper and record the failed-offload caveat instead of counting local Cargo output")
    NEXT_ACTIONS+=("if this host's rch CLI rejects 'rch exec' as an unknown subcommand, do not run Cargo locally; record the remote-exec surface mismatch and keep the Cargo gate unverified")
elif [ "$RCH_QUEUE_STATUS" = "queued" ]; then
    CAVEATS+=("rch_queue_busy: ${RCH_QUEUED_BUILDS} queued RCH job(s); cargo verification may wait behind other agents")
elif [ "$RCH_QUEUE_STATUS" = "timeout" ]; then
    CAVEATS+=("rch_queue_timeout: rch queue exceeded ${RCH_PROBE_TIMEOUT_SECONDS}s; closeout audit kept running but queue evidence is unavailable")
    NEXT_ACTIONS+=("capture RCH queue evidence separately before closing beads that require remote Cargo proof")
fi
if [ "$AGENT_MAIL_STATUS" = "unreachable" ]; then
    CAVEATS+=("agent_mail_unreachable: reservation/inbox evidence could not be captured at audit time; rely on commit-message coordination")
fi
if [ "$J1_LOG_PRESENT" = "false" ]; then
    CAVEATS+=("j1_log_absent: no tests/logs/active/*.jsonl found; this is fine for beads that didn't run e2e drivers but means no structured timing evidence is retained")
fi

# Readiness classification:
#  - blocked: any blocker present (open deps, uncommitted files)
#  - ready_with_caveats: no blockers but ≥1 caveat
#  - ready: clean
if [ "${#BLOCKERS[@]}" -gt 0 ]; then
    READINESS="blocked"
elif [ "${#CAVEATS[@]}" -gt 0 ]; then
    READINESS="ready_with_caveats"
else
    READINESS="ready"
fi
NEXT_ACTIONS+=("review the audit JSON, then run: br close ${BEAD_ID} --reason '<close reason citing this audit>'")

# Build output JSON via jq for safe escaping.
BLOCKERS_JSON="$(printf '%s\n' "${BLOCKERS[@]:-}" | jq -Rn '[inputs | select(length > 0)]')"
CAVEATS_JSON="$(printf '%s\n' "${CAVEATS[@]:-}" | jq -Rn '[inputs | select(length > 0)]')"
NEXT_ACTIONS_JSON="$(printf '%s\n' "${NEXT_ACTIONS[@]:-}" | jq -Rn '[inputs | select(length > 0)]')"

RESULT_JSON="$(jq -nc \
    --arg schema "$SCHEMA_ID" \
    --arg bead_id "$BEAD_ID" \
    --arg readiness "$READINESS" \
    --arg bead_status "$BEAD_STATUS" \
    --arg bead_assignee "$BEAD_ASSIGNEE" \
    --arg bead_title "$BEAD_TITLE" \
    --argjson open_deps "$OPEN_DEPS_JSON" \
    --argjson dependency_cycles "$DEPENDENCY_CYCLES_JSON" \
    --argjson dependency_cycle_count "$DEPENDENCY_CYCLE_COUNT" \
    --argjson uncommitted_refs "$UNCOMMITTED_REFS_JSON" \
    --arg rch_status "$RCH_STATUS" \
    --arg rch_queue_status "$RCH_QUEUE_STATUS" \
    --argjson rch_active_builds "$RCH_ACTIVE_BUILDS" \
    --argjson rch_stale_active_builds "$RCH_STALE_ACTIVE_BUILDS" \
    --argjson rch_queued_builds "$RCH_QUEUED_BUILDS" \
    --arg agent_mail_status "$AGENT_MAIL_STATUS" \
    --argjson j1_log_present "$J1_LOG_PRESENT" \
    --arg j1_log_path "$J1_LOG_PATH" \
    --argjson srr6_closeout "$(jq -nc \
        --argjson enabled "$SRR6_CLOSEOUT_ENABLED" \
        --arg status "$SRR6_CLOSEOUT_STATUS" \
        --arg matrix_path "$SRR6_MATRIX_PATH" \
        --argjson matrix_present "$SRR6_MATRIX_PRESENT" \
        --argjson matrix_row_present "$SRR6_MATRIX_ROW_PRESENT" \
        --argjson required_proofs "$SRR6_REQUIRED_PROOFS_JSON" \
        --argjson missing_proofs "$SRR6_MISSING_PROOFS_JSON" \
        --argjson missing_proof_markers "$SRR6_MISSING_PROOF_MARKERS_JSON" \
        --argjson unresolved_dependencies "$SRR6_UNRESOLVED_DEPS_JSON" \
        '{
            enabled: $enabled,
            status: $status,
            matrix_path: $matrix_path,
            matrix_present: $matrix_present,
            matrix_row_present: $matrix_row_present,
            required_proofs: $required_proofs,
            missing_proofs: $missing_proofs,
            missing_proof_markers: $missing_proof_markers,
            unresolved_dependencies: $unresolved_dependencies
        }')" \
    --argjson blockers "$BLOCKERS_JSON" \
    --argjson caveats "$CAVEATS_JSON" \
    --argjson next_actions "$NEXT_ACTIONS_JSON" \
    '{
        schema: $schema,
        bead_id: $bead_id,
        readiness: $readiness,
        evidence: {
            bead_status: $bead_status,
            bead_assignee: $bead_assignee,
            bead_title: $bead_title,
            open_dependencies: $open_deps,
            dependency_cycles: $dependency_cycles,
            dependency_cycle_count: $dependency_cycle_count,
            uncommitted_files_referencing_bead: $uncommitted_refs,
            rch_status: $rch_status,
            rch_queue_status: $rch_queue_status,
            rch_active_builds: $rch_active_builds,
            rch_stale_active_builds: $rch_stale_active_builds,
            rch_queued_builds: $rch_queued_builds,
            agent_mail_status: $agent_mail_status,
            j1_log_present: $j1_log_present,
            j1_log_path: $j1_log_path,
            srr6_closeout: $srr6_closeout
        },
        blockers: $blockers,
        caveats: $caveats,
        next_actions: $next_actions
    }')"

if [ "$JSON_OUTPUT" -eq 1 ]; then
    printf '%s\n' "$RESULT_JSON"
else
    # Human-readable summary that still surfaces every field the JSON
    # carries, just in flowing prose. Useful when the operator is
    # running ad-hoc without --json.
    printf 'Closeout audit for %s\n' "$BEAD_ID"
    printf '  readiness: %s\n' "$READINESS"
    printf '  status: %s\n' "$BEAD_STATUS"
    if [ -n "$BEAD_ASSIGNEE" ]; then
        printf '  assignee: %s\n' "$BEAD_ASSIGNEE"
    fi
    printf '  rch: %s\n' "$RCH_STATUS"
    printf '  rch_queue: %s (active=%s stale=%s queued=%s)\n' \
        "$RCH_QUEUE_STATUS" \
        "$RCH_ACTIVE_BUILDS" \
        "$RCH_STALE_ACTIVE_BUILDS" \
        "$RCH_QUEUED_BUILDS"
    printf '  agent_mail: %s\n' "$AGENT_MAIL_STATUS"
    printf '  j1_log: %s\n' "$J1_LOG_PRESENT"
    if [ "$OPEN_DEPS_COUNT" -gt 0 ]; then
        printf '  open_dependencies (%d):\n' "$OPEN_DEPS_COUNT"
        printf '%s' "$OPEN_DEPS_JSON" | jq -r '.[] | "    - \(.id) [\(.status)]"'
    fi
    if [ "$DEPENDENCY_CYCLE_COUNT" -gt 0 ]; then
        printf '  dependency_cycles (%d):\n' "$DEPENDENCY_CYCLE_COUNT"
        printf '%s' "$DEPENDENCY_CYCLES_JSON" | jq -r '.[] | "    - \(.cycle)"'
    fi
    if [ "$UNCOMMITTED_REFS_COUNT" -gt 0 ]; then
        printf '  uncommitted files referencing bead (%d):\n' "$UNCOMMITTED_REFS_COUNT"
        printf '%s' "$UNCOMMITTED_REFS_JSON" | jq -r '.[] | "    - \(.)"'
    fi
    if [ "${#BLOCKERS[@]}" -gt 0 ]; then
        printf '  blockers:\n'
        for b in "${BLOCKERS[@]}"; do printf '    - %s\n' "$b"; done
    fi
    if [ "${#CAVEATS[@]}" -gt 0 ]; then
        printf '  caveats:\n'
        for c in "${CAVEATS[@]}"; do printf '    - %s\n' "$c"; done
    fi
    if [ "${#NEXT_ACTIONS[@]}" -gt 0 ]; then
        printf '  next actions:\n'
        for a in "${NEXT_ACTIONS[@]}"; do printf '    - %s\n' "$a"; done
    fi
fi

exit 0
