#!/usr/bin/env bash
# Script/static checks for the read-only Agent Mail snapshot producer.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
PRODUCER="$REPO_ROOT/scripts/agent_mail_snapshot.sh"
SNAPSHOT_SCHEMA="$REPO_ROOT/docs/schemas/swarm/ee.agent_mail.snapshot.v1.json"
TMP_BASE="${TMPDIR:-/tmp}"
case "$TMP_BASE" in
  /Volumes/*) TMP_BASE="/tmp" ;;
esac
if [ -d "$TMP_BASE" ]; then
    TMP_BASE="$(cd "$TMP_BASE" && pwd -P)"
fi
TMP_ROOT="$TMP_BASE/ee-agent-mail-snapshot-e2e.$$"
FAKE_BIN="$TMP_ROOT/bin"
PROJECT="$TMP_ROOT/workspace"
COMMAND_LOG="$TMP_ROOT/am-commands.log"
SNAPSHOT_OK="$TMP_ROOT/snapshot-ok.json"
SNAPSHOT_DEGRADED="$TMP_ROOT/snapshot-degraded.json"
SNAPSHOT_STDOUT_OK="$TMP_ROOT/snapshot-stdout-ok.json"
SNAPSHOT_STDOUT_STDERR="$TMP_ROOT/snapshot-stdout.stderr"
SNAPSHOT_DUAL_FILE="$TMP_ROOT/snapshot-dual-file.json"
SNAPSHOT_DUAL_STDOUT="$TMP_ROOT/snapshot-dual-stdout.json"
SNAPSHOT_OUTPUT_ONLY_STDOUT="$TMP_ROOT/snapshot-output-only.stdout"
SNAPSHOT_MISSING_SCHEMA="$TMP_ROOT/snapshot-missing-schema.json"
SNAPSHOT_MALFORMED_DEGRADED="$TMP_ROOT/snapshot-malformed-degraded.json"
SNAPSHOT_BODY_LEAK="$TMP_ROOT/snapshot-body-leak.json"
SNAPSHOT_PATH_LEAK="$TMP_ROOT/snapshot-path-leak.json"
HELP_STDOUT="$TMP_ROOT/help.stdout"
HELP_STDERR="$TMP_ROOT/help.stderr"
UNKNOWN_STDOUT="$TMP_ROOT/unknown.stdout"
UNKNOWN_STDERR="$TMP_ROOT/unknown.stderr"
COORDINATION_OK="$TMP_ROOT/coordination-ok.json"
COORDINATION_DEGRADED="$TMP_ROOT/coordination-degraded.json"
SYMLINK_PARENT="$TMP_ROOT/symlink-parent"
SYMLINK_TARGET="$TMP_ROOT/symlink-target"
SYMLINK_STDOUT="$TMP_ROOT/symlink-refusal.stdout"
SYMLINK_STDERR="$TMP_ROOT/symlink-refusal.stderr"
SYMLINK_OUTPUT="$SYMLINK_PARENT/refused-snapshot.json"
LIVE_MODE="${EE_AGENT_MAIL_SNAPSHOT_LIVE_E2E:-0}"
LIVE_PROJECT="${EE_AGENT_MAIL_SNAPSHOT_LIVE_PROJECT:-$REPO_ROOT}"
LIVE_AGENT="${EE_AGENT_MAIL_SNAPSHOT_LIVE_AGENT:-${AGENT_NAME:-${AGENT_MAIL_AGENT:-}}}"
LIVE_INBOX_LIMIT="${EE_AGENT_MAIL_SNAPSHOT_LIVE_INBOX_LIMIT:-20}"
LIVE_THREAD_LIMIT="${EE_AGENT_MAIL_SNAPSHOT_LIVE_THREAD_LIMIT:-20}"
LIVE_TIMEOUT_SEC="${EE_AGENT_MAIL_SNAPSHOT_LIVE_TIMEOUT_SEC:-5}"
LIVE_AM_BIN="${AGENT_MAIL_AM_BIN:-am}"
LIVE_EE_BIN="${EE_BINARY:-ee}"
LIVE_PRE_STATE="$TMP_ROOT/live-pre-state.json"
LIVE_POST_STATE="$TMP_ROOT/live-post-state.json"
LIVE_SNAPSHOT="$TMP_ROOT/live-snapshot.json"
LIVE_BRIEF="$TMP_ROOT/live-brief.json"
CLAIM_GATE_MODE="${EE_AGENT_MAIL_SNAPSHOT_CLAIM_GATE_E2E:-0}"
CLAIM_GATE_WORKSPACE="$TMP_ROOT/claim-gate-workspace"
CLAIM_GATE_SNAPSHOT="$TMP_ROOT/claim-gate-snapshot.json"
CLAIM_GATE_CLEAN_SNAPSHOT="$TMP_ROOT/claim-gate-clean-snapshot.json"
CLAIM_GATE_NO_SNAPSHOT="$TMP_ROOT/claim-gate-no-snapshot.json"
CLAIM_GATE_NO_SNAPSHOT_STDERR="$TMP_ROOT/claim-gate-no-snapshot.stderr"
CLAIM_GATE_WITH_SNAPSHOT="$TMP_ROOT/claim-gate-with-snapshot.json"
CLAIM_GATE_WITH_SNAPSHOT_STDERR="$TMP_ROOT/claim-gate-with-snapshot.stderr"
CLAIM_GATE_WITH_CLEAN_SNAPSHOT="$TMP_ROOT/claim-gate-with-clean-snapshot.json"
CLAIM_GATE_WITH_CLEAN_SNAPSHOT_STDERR="$TMP_ROOT/claim-gate-with-clean-snapshot.stderr"
LIVE_VERDICT="skipped"
LIVE_REASON="env_disabled"
LIVE_PRE_HASH=""
LIVE_POST_HASH=""
LIVE_SNAPSHOT_HASH=""
LIVE_BRIEF_HASH=""
CLAIM_GATE_VERDICT="skipped"
CLAIM_GATE_REASON="mode_disabled"
CLAIM_GATE_NO_SNAPSHOT_HASH=""
CLAIM_GATE_WITH_SNAPSHOT_HASH=""
CLAIM_GATE_WITH_CLEAN_SNAPSHOT_HASH=""
CLAIM_GATE_SNAPSHOT_HASH=""
CLAIM_GATE_CLEAN_SNAPSHOT_HASH=""
CLAIM_GATE_NO_SNAPSHOT_EXIT=""
CLAIM_GATE_WITH_SNAPSHOT_EXIT=""
CLAIM_GATE_WITH_CLEAN_SNAPSHOT_EXIT=""
CLAIM_GATE_NO_SNAPSHOT_ELAPSED_MS=""
CLAIM_GATE_WITH_SNAPSHOT_ELAPSED_MS=""
CLAIM_GATE_WITH_CLEAN_SNAPSHOT_ELAPSED_MS=""
CLAIM_GATE_NO_SNAPSHOT_DEGRADED_CODES="[]"
CLAIM_GATE_WITH_SNAPSHOT_DEGRADED_CODES="[]"
CLAIM_GATE_WITH_CLEAN_SNAPSHOT_DEGRADED_CODES="[]"
FINAL_EXIT=0

mkdir -p "$FAKE_BIN" "$PROJECT"
: > "$COMMAND_LOG"

if ! command -v jq >/dev/null 2>&1; then
    printf 'agent_mail_snapshot: jq is required\n' >&2
    exit 2
fi

bash -n "$PRODUCER"

jq -e '
  .["$schema"] == "http://json-schema.org/draft-07/schema#"
  and .["$id"] == "https://eidetic-engine/schemas/swarm/ee.agent_mail.snapshot.v1.json"
  and .title == "ee.agent_mail.snapshot.v1"
  and .properties.schema.const == "ee.agent_mail.snapshot.v1"
  and (.required | index("schema") and index("summary") and index("file_reservations") and index("agents") and index("inbox") and index("threads"))
  and .["x-ee-status"].tracking_bead == "bd-1ur7d.1"
  and .["x-ee-status"].shipped == true
  and .["x-ee-status"].available_in_build == true
  and .["x-ee-doc"] == "docs/swarm/coordination_snapshot.md"
  and .examples[0].schema == "ee.agent_mail.snapshot.v1"
' "$SNAPSHOT_SCHEMA" >/dev/null

"$PRODUCER" --help >"$HELP_STDOUT" 2>"$HELP_STDERR"
if [ -s "$HELP_STDERR" ]; then
    printf 'agent_mail_snapshot: --help wrote diagnostics to stderr\n' >&2
    exit 1
fi
for expected_help in \
    "--json" \
    "--stdout" \
    "--output /private/tmp/ee-agent-mail-snapshot.json" \
    "--json --output /private/tmp/ee-agent-mail-snapshot.json"; do
    if ! grep -F -- "$expected_help" "$HELP_STDOUT" >/dev/null; then
        printf 'agent_mail_snapshot: --help missing expected text: %s\n' "$expected_help" >&2
        exit 1
    fi
done

set +e
"$PRODUCER" --definitely-not-valid >"$UNKNOWN_STDOUT" 2>"$UNKNOWN_STDERR"
unknown_status=$?
set -e
if [ "$unknown_status" -eq 0 ]; then
    printf 'agent_mail_snapshot: unknown argument should fail\n' >&2
    exit 1
fi
if [ -s "$UNKNOWN_STDOUT" ]; then
    printf 'agent_mail_snapshot: unknown argument wrote stdout\n' >&2
    exit 1
fi
if ! grep -F -- "unrecognized arguments: --definitely-not-valid" "$UNKNOWN_STDERR" >/dev/null; then
    printf 'agent_mail_snapshot: unknown argument diagnostic missing\n' >&2
    exit 1
fi

assert_snapshot_contract() {
    local snapshot="$1"

    jq -e '
      .schema == "ee.agent_mail.snapshot.v1"
      and (.generated_at | type == "string")
      and .project_key == "<workspace>"
      and (.agent_name | type == "string" and length > 0)
      and .redaction_status == "paths_counts_subjects_only_no_content"
      and (.producer_status == "ok" or .producer_status == "degraded")
      and (.source_commands | type == "array")
      and (.command_statuses | type == "array")
      and (.fallback_active | type == "boolean")
      and (.am_agents_list_ok | type == "boolean")
      and (.summary.agent_count == (.agents | length))
      and (.summary.file_reservation_count == (.file_reservations | length))
      and (.summary.inbox_mailbox_count == (.inbox | length))
      and (.summary.thread_count == (.threads | length))
      and (.summary.source_command_count == (.source_commands | length))
      and (.summary.degraded_count == (.degraded | length))
      and (.degraded | all(
        (.code | type == "string" and length > 0)
        and (.severity | type == "string")
        and (.source | type == "string" and length > 0)
        and (.command | type == "string" and length > 0)
        and (.timed_out | type == "boolean")
      ))
      and ([.. | objects | keys[]? | select(. == "body" or . == "body_md" or . == "bodyMd" or . == "raw_stdout" or . == "raw_stderr")] | length == 0)
      and ((. | tostring) | test("ghp_|sk-[A-Za-z0-9]{20,}|SECRET_TOKEN|raw body|/Users/|/Volumes/|/data/|/tmp/|/private/|/var/folders/") | not)
    ' "$snapshot" >/dev/null
}

sha256_file() {
    shasum -a 256 "$1" | awk '{print "sha256:" $1}'
}

now_ms() {
    printf '%s000\n' "$(date +%s)"
}

mark_live_degraded() {
    LIVE_VERDICT="degraded"
    LIVE_REASON="$1"
    FINAL_EXIT=3
}

run_live_json_capture() {
    local label="$1"
    local output="$2"
    local stderr_output="$3"
    shift 3

    if ! "$@" >"$output" 2>"$stderr_output"; then
        mark_live_degraded "${label}_command_failed"
        return 1
    fi
    if ! jq . "$output" >/dev/null; then
        mark_live_degraded "${label}_invalid_json"
        return 1
    fi
}

mark_claim_gate_degraded() {
    CLAIM_GATE_VERDICT="degraded"
    CLAIM_GATE_REASON="$1"
    FINAL_EXIT=3
}

run_claim_gate_fixture_e2e() {
    case "$CLAIM_GATE_MODE" in
        0|false|no|off)
            CLAIM_GATE_VERDICT="skipped"
            CLAIM_GATE_REASON="mode_disabled"
            return 0
            ;;
        auto)
            if ! command -v "$LIVE_EE_BIN" >/dev/null 2>&1; then
                CLAIM_GATE_VERDICT="skipped"
                CLAIM_GATE_REASON="missing_ee_binary_auto"
                return 0
            fi
            ;;
        1|true|yes|on)
            if ! command -v "$LIVE_EE_BIN" >/dev/null 2>&1; then
                mark_claim_gate_degraded "missing_ee_binary"
                return 0
            fi
            ;;
        *)
            mark_claim_gate_degraded "invalid_claim_gate_mode"
            return 0
            ;;
    esac
    if ! command -v br >/dev/null 2>&1; then
        mark_claim_gate_degraded "missing_br"
        return 0
    fi

    mkdir -p "$CLAIM_GATE_WORKSPACE/.beads"
    cat > "$CLAIM_GATE_WORKSPACE/.beads/issues.jsonl" <<'JSONL'
{"id":"bd-fixture-policy","title":"Policy redaction collision proof","status":"open","priority":1,"issue_type":"test","created_at":"2026-06-06T00:00:00Z","updated_at":"2026-06-06T00:00:00Z"}
JSONL
    cat > "$CLAIM_GATE_SNAPSHOT" <<'JSON'
{
  "schema": "ee.agent_mail.snapshot.v1",
  "generated_at": "2026-06-06T00:00:00Z",
  "project_key": "<workspace>",
  "agent_name": "BlueLake",
  "redaction_status": "paths_counts_subjects_only_no_content",
  "producer_status": "ok",
  "fallback_active": false,
  "am_agents_list_ok": true,
  "summary": {
    "agent_count": 1,
    "degraded_count": 0,
    "file_reservation_count": 1,
    "inbox_mailbox_count": 1,
    "source_command_count": 3,
    "thread_count": 1
  },
  "source_commands": [
    "am agents list --project <workspace> --json",
    "am robot reservations --project <workspace> --all --format json",
    "am mail inbox --project <workspace> --agent BlueLake --limit 5 --json"
  ],
  "command_statuses": [],
  "degraded": [],
  "agents": [
    {"name": "BlueLake", "last_active_ts": "2026-06-06T00:00:00Z"}
  ],
  "file_reservations": [
    {"holder": "BlueLake", "path_pattern": "src/policy/**", "exclusive": true}
  ],
  "inbox": [
    {"mailbox": "BlueLake", "unread_count": 0, "ack_required_count": 0}
  ],
  "threads": [
    {"thread_id": "bd-fixture-policy", "message_count": 1}
  ]
}
JSON
    CLAIM_GATE_SNAPSHOT_HASH="$(sha256_file "$CLAIM_GATE_SNAPSHOT")"
    cat > "$CLAIM_GATE_CLEAN_SNAPSHOT" <<'JSON'
{
  "schema": "ee.agent_mail.snapshot.v1",
  "generated_at": "2026-06-06T00:00:00Z",
  "project_key": "<workspace>",
  "agent_name": "GreenLake",
  "redaction_status": "paths_counts_subjects_only_no_content",
  "producer_status": "ok",
  "fallback_active": false,
  "am_agents_list_ok": true,
  "summary": {
    "agent_count": 1,
    "degraded_count": 0,
    "file_reservation_count": 1,
    "inbox_mailbox_count": 1,
    "source_command_count": 3,
    "thread_count": 1
  },
  "source_commands": [
    "am agents list --project <workspace> --json",
    "am robot reservations --project <workspace> --all --format json",
    "am mail inbox --project <workspace> --agent GreenLake --limit 5 --json"
  ],
  "command_statuses": [],
  "degraded": [],
  "agents": [
    {"name": "GreenLake", "last_active_ts": "2026-06-06T00:00:00Z"}
  ],
  "file_reservations": [
    {"holder": "GreenLake", "path_pattern": "docs/agent-mail-clean/**", "exclusive": true}
  ],
  "inbox": [
    {"mailbox": "GreenLake", "unread_count": 0, "ack_required_count": 0}
  ],
  "threads": [
    {"thread_id": "bd-fixture-policy", "message_count": 1}
  ]
}
JSON
    CLAIM_GATE_CLEAN_SNAPSHOT_HASH="$(sha256_file "$CLAIM_GATE_CLEAN_SNAPSHOT")"

    set +e
    claim_gate_started_ms="$(now_ms)"
    (
      cd "$CLAIM_GATE_WORKSPACE"
      "$LIVE_EE_BIN" swarm work-packet \
        --workspace . \
        --sources beads,agent-mail \
        --claim-gate \
        --candidate bd-fixture-policy \
        --json \
        --command-timeout-ms 1000 \
        > "$CLAIM_GATE_NO_SNAPSHOT" \
        2> "$CLAIM_GATE_NO_SNAPSHOT_STDERR"
    )
    CLAIM_GATE_NO_SNAPSHOT_EXIT=$?
    CLAIM_GATE_NO_SNAPSHOT_ELAPSED_MS=$(( $(now_ms) - claim_gate_started_ms ))
    claim_gate_started_ms="$(now_ms)"
    (
      cd "$CLAIM_GATE_WORKSPACE"
      "$LIVE_EE_BIN" swarm work-packet \
        --workspace . \
        --sources beads,agent-mail \
        --agent-mail-snapshot "$CLAIM_GATE_SNAPSHOT" \
        --claim-gate \
        --candidate bd-fixture-policy \
        --json \
        --command-timeout-ms 1000 \
        > "$CLAIM_GATE_WITH_SNAPSHOT" \
        2> "$CLAIM_GATE_WITH_SNAPSHOT_STDERR"
    )
    CLAIM_GATE_WITH_SNAPSHOT_EXIT=$?
    CLAIM_GATE_WITH_SNAPSHOT_ELAPSED_MS=$(( $(now_ms) - claim_gate_started_ms ))
    claim_gate_started_ms="$(now_ms)"
    (
      cd "$CLAIM_GATE_WORKSPACE"
      "$LIVE_EE_BIN" swarm work-packet \
        --workspace . \
        --sources beads,agent-mail \
        --agent-mail-snapshot "$CLAIM_GATE_CLEAN_SNAPSHOT" \
        --claim-gate \
        --candidate bd-fixture-policy \
        --json \
        --command-timeout-ms 1000 \
        > "$CLAIM_GATE_WITH_CLEAN_SNAPSHOT" \
        2> "$CLAIM_GATE_WITH_CLEAN_SNAPSHOT_STDERR"
    )
    CLAIM_GATE_WITH_CLEAN_SNAPSHOT_EXIT=$?
    CLAIM_GATE_WITH_CLEAN_SNAPSHOT_ELAPSED_MS=$(( $(now_ms) - claim_gate_started_ms ))
    set -e

    if [ "$CLAIM_GATE_NO_SNAPSHOT_EXIT" -ne 0 ]; then
        mark_claim_gate_degraded "no_snapshot_claim_gate_failed"
        return 0
    fi
    if [ "$CLAIM_GATE_WITH_SNAPSHOT_EXIT" -ne 0 ]; then
        mark_claim_gate_degraded "snapshot_claim_gate_failed"
        return 0
    fi
    if [ "$CLAIM_GATE_WITH_CLEAN_SNAPSHOT_EXIT" -ne 0 ]; then
        mark_claim_gate_degraded "clean_snapshot_claim_gate_failed"
        return 0
    fi
    if ! jq . "$CLAIM_GATE_NO_SNAPSHOT" >/dev/null; then
        mark_claim_gate_degraded "no_snapshot_claim_gate_invalid_json"
        return 0
    fi
    if ! jq . "$CLAIM_GATE_WITH_SNAPSHOT" >/dev/null; then
        mark_claim_gate_degraded "snapshot_claim_gate_invalid_json"
        return 0
    fi
    if ! jq . "$CLAIM_GATE_WITH_CLEAN_SNAPSHOT" >/dev/null; then
        mark_claim_gate_degraded "clean_snapshot_claim_gate_invalid_json"
        return 0
    fi
    CLAIM_GATE_NO_SNAPSHOT_DEGRADED_CODES="$(jq -c '.data.degradedCodes // []' "$CLAIM_GATE_NO_SNAPSHOT")"
    CLAIM_GATE_WITH_SNAPSHOT_DEGRADED_CODES="$(jq -c '.data.degradedCodes // []' "$CLAIM_GATE_WITH_SNAPSHOT")"
    CLAIM_GATE_WITH_CLEAN_SNAPSHOT_DEGRADED_CODES="$(jq -c '.data.degradedCodes // []' "$CLAIM_GATE_WITH_CLEAN_SNAPSHOT")"
    if ! jq -e '
      .success == true
      and .data.schema == "ee.swarm.work_packet.claim_gate.v1"
      and .data.safeToClaim == false
      and ((.data.degradedCodes | index("agent_mail_unavailable")) != null)
    ' "$CLAIM_GATE_NO_SNAPSHOT" >/dev/null; then
        mark_claim_gate_degraded "no_snapshot_missing_agent_mail_unavailable"
        return 0
    fi
    if ! jq -e '
      .success == true
      and .data.schema == "ee.swarm.work_packet.claim_gate.v1"
      and .data.safeToClaim == false
      and ((.data.degradedCodes | index("agent_mail_unavailable")) == null)
      and .data.sourceAuthority.agentMailStatus == "fresh"
      and .data.sourceAuthority.reservationAuthoritative == true
      and .data.sourceAuthority.inboxAuthoritative == true
      and .data.selectedCandidate.decision == "unsafe_due_to_conflict"
      and .data.selectedCandidate.collisionRisk != "none"
      and ((.data.unsafeReasons | index("reservation_collision:src/policy/**")) != null)
      and ((.data.unsafeReasons | index("file_collision_owner:BlueLake:src/policy/**")) != null)
      and .data.claimCommandAction == null
    ' "$CLAIM_GATE_WITH_SNAPSHOT" >/dev/null; then
        mark_claim_gate_degraded "snapshot_claim_gate_did_not_preserve_collision"
        return 0
    fi
    if ! jq -e '
      .success == true
      and .data.schema == "ee.swarm.work_packet.claim_gate.v1"
      and .data.safeToClaim == true
      and ((.data.degradedCodes | index("agent_mail_unavailable")) == null)
      and .data.sourceAuthority.agentMailStatus == "fresh"
      and .data.sourceAuthority.reservationAuthoritative == true
      and .data.sourceAuthority.inboxAuthoritative == true
      and .data.selectedCandidate.decision == "safe_to_claim"
      and .data.selectedCandidate.collisionRisk == "none"
      and (.data.unsafeReasons | length) == 0
      and .data.claimCommandAction.commandId == "bead_claim_candidate"
    ' "$CLAIM_GATE_WITH_CLEAN_SNAPSHOT" >/dev/null; then
        mark_claim_gate_degraded "clean_snapshot_claim_gate_not_safe"
        return 0
    fi

    for artifact in "$CLAIM_GATE_SNAPSHOT" "$CLAIM_GATE_CLEAN_SNAPSHOT" "$CLAIM_GATE_NO_SNAPSHOT" "$CLAIM_GATE_WITH_SNAPSHOT" "$CLAIM_GATE_WITH_CLEAN_SNAPSHOT"; do
        if grep -E 'ghp_|raw body|body_md|SECRET_TOKEN|/Users/|/Volumes/|/data/|/tmp/|/private/|/var/folders/' "$artifact" >/dev/null; then
            mark_claim_gate_degraded "claim_gate_redaction_leak"
            return 0
        fi
        if grep -F "$CLAIM_GATE_WORKSPACE" "$artifact" >/dev/null; then
            mark_claim_gate_degraded "claim_gate_workspace_path_leak"
            return 0
        fi
    done

    CLAIM_GATE_NO_SNAPSHOT_HASH="$(sha256_file "$CLAIM_GATE_NO_SNAPSHOT")"
    CLAIM_GATE_WITH_SNAPSHOT_HASH="$(sha256_file "$CLAIM_GATE_WITH_SNAPSHOT")"
    CLAIM_GATE_WITH_CLEAN_SNAPSHOT_HASH="$(sha256_file "$CLAIM_GATE_WITH_CLEAN_SNAPSHOT")"
    CLAIM_GATE_VERDICT="pass"
    CLAIM_GATE_REASON="snapshot_refreshes_agent_mail_evidence_and_preserves_clean_and_conflicting_reservations"
}

capture_live_state() {
    local phase="$1"
    local output="$2"
    local phase_dir="$TMP_ROOT/live-$phase"
    mkdir -p "$phase_dir"

    run_live_json_capture \
        "agents_${phase}" \
        "$phase_dir/agents.json" \
        "$phase_dir/agents.stderr" \
        "$LIVE_AM_BIN" agents list --project "$LIVE_PROJECT" --json || return 1
    run_live_json_capture \
        "reservations_${phase}" \
        "$phase_dir/reservations.json" \
        "$phase_dir/reservations.stderr" \
        "$LIVE_AM_BIN" robot reservations --project "$LIVE_PROJECT" --all --format json || return 1
    run_live_json_capture \
        "inbox_${phase}" \
        "$phase_dir/inbox.json" \
        "$phase_dir/inbox.stderr" \
        "$LIVE_AM_BIN" mail inbox --project "$LIVE_PROJECT" --agent "$LIVE_AGENT" --limit "$LIVE_INBOX_LIMIT" --json || return 1
    run_live_json_capture \
        "beads_${phase}" \
        "$phase_dir/beads-doctor.json" \
        "$phase_dir/beads-doctor.stderr" \
        br doctor --json --no-db || return 1

    jq -S '
      {
        ok,
        checks: [
          .checks[]? | {
            name,
            status,
            records: (.details.records // null),
            dirtyIssues: (.details.dirty_issues // null)
          }
        ]
      }
    ' "$phase_dir/beads-doctor.json" > "$phase_dir/beads-doctor-redacted.json"

    jq -S -n \
      --slurpfile agents "$phase_dir/agents.json" \
      --slurpfile reservations "$phase_dir/reservations.json" \
      --slurpfile inbox "$phase_dir/inbox.json" \
      --slurpfile beads "$phase_dir/beads-doctor-redacted.json" \
      '{
        agents: (
          ($agents[0].agents? // $agents[0].result? // $agents[0].items? // $agents[0])
          | if type == "array" then
              map({name: (.name // .agent_name // .agent // .mailbox // "")})
              | map(select(.name != ""))
              | sort_by(.name)
            else [] end
        ),
        reservations: (
          ($reservations[0].all_active? // $reservations[0].active? // $reservations[0].reservations? // $reservations[0].file_reservations? // $reservations[0].items? // $reservations[0])
          | if type == "array" then
              map({
                path: (.path_pattern // .path // .pattern // ""),
                holder: (.holder // .agent_name // .agent // .owner // ""),
                exclusive: (.exclusive // false)
              })
              | map(select(.path != "" and .holder != ""))
              | sort_by(.path, .holder, .exclusive)
            else [] end
        ),
        inbox: (
          ($inbox[0].inbox? // $inbox[0].messages? // $inbox[0].result? // $inbox[0].items? // $inbox[0])
          | if type == "array" then
              map({
                id: ((.id // .message_id // .messageId // "") | tostring),
                thread_id: (.thread_id // .threadId // ""),
                ack_required: (.ack_required // .ackRequired // false)
              })
              | sort_by(.id, .thread_id, .ack_required)
            else [] end
        ),
        beads: $beads[0]
      }' > "$output"
}

run_live_no_mock_e2e() {
    if [ "$LIVE_MODE" != "1" ]; then
        return 0
    fi

    LIVE_VERDICT="running"
    LIVE_REASON="running"
    for command_name in "$LIVE_AM_BIN" "$LIVE_EE_BIN" br jq; do
        if ! command -v "$command_name" >/dev/null 2>&1; then
            mark_live_degraded "missing_command_${command_name}"
            return 0
        fi
    done
    if [ -z "$LIVE_AGENT" ]; then
        mark_live_degraded "missing_live_agent"
        return 0
    fi

    capture_live_state "pre" "$LIVE_PRE_STATE" || return 0
    LIVE_PRE_HASH="$(sha256_file "$LIVE_PRE_STATE")"

    if ! "$PRODUCER" \
        --am-bin "$LIVE_AM_BIN" \
        --project "$LIVE_PROJECT" \
        --agent "$LIVE_AGENT" \
        --inbox-limit "$LIVE_INBOX_LIMIT" \
        --thread-limit "$LIVE_THREAD_LIMIT" \
        --timeout-sec "$LIVE_TIMEOUT_SEC" \
        --output "$LIVE_SNAPSHOT" \
        >"$TMP_ROOT/live-producer.stdout" \
        2>"$TMP_ROOT/live-producer.stderr"; then
        mark_live_degraded "producer_failed"
        return 0
    fi
    LIVE_SNAPSHOT_HASH="$(sha256_file "$LIVE_SNAPSHOT")"

    if grep -E 'ghp_|raw body|body_md|SECRET_TOKEN|/Users/|/Volumes/|/data/|/tmp/|/private/|/var/folders/' "$LIVE_SNAPSHOT" >/dev/null; then
        mark_live_degraded "live_snapshot_redaction_leak"
        return 0
    fi

    if ! "$LIVE_EE_BIN" swarm brief \
        --json \
        --sources agent-mail \
        --agent-mail-snapshot "$LIVE_SNAPSHOT" \
        --workspace "$LIVE_PROJECT" \
        >"$LIVE_BRIEF" \
        2>"$TMP_ROOT/live-brief.stderr"; then
        mark_live_degraded "swarm_brief_failed"
        return 0
    fi
    if ! jq -e '
        .success == true
        and any(.data.sources[]?; .source == "agent_mail" and .status == "ready")
      ' "$LIVE_BRIEF" >/dev/null; then
        mark_live_degraded "swarm_brief_missing_agent_mail_ready"
        return 0
    fi
    LIVE_BRIEF_HASH="$(sha256_file "$LIVE_BRIEF")"

    capture_live_state "post" "$LIVE_POST_STATE" || return 0
    LIVE_POST_HASH="$(sha256_file "$LIVE_POST_STATE")"
    if [ "$LIVE_PRE_HASH" != "$LIVE_POST_HASH" ]; then
        mark_live_degraded "live_state_changed"
        return 0
    fi

    LIVE_VERDICT="pass"
    LIVE_REASON="state_hash_stable"
}

cat > "$FAKE_BIN/am" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$AM_FAKE_COMMAND_LOG"

if [ "${AM_FAKE_MODE:-ok}" = "fail_agents" ] && [ "$1" = "agents" ] && [ "$2" = "list" ]; then
  printf 'agent list unavailable\n' >&2
  exit 12
fi

if [ "$1" = "agents" ] && [ "$2" = "list" ]; then
cat <<'JSON'
[
  {
    "name": "BlueFortress",
    "last_active_ts": "2026-06-04T22:48:06Z",
    "body_md": "SECRET_TOKEN=ghp_abcdefghijklmnopqrstuvwxyz123456"
  },
  {
    "name": "BeigeHollow",
    "last_active_ts": "2026-06-04T22:45:43Z"
  }
]
JSON
  exit 0
fi

if [ "$1" = "robot" ] && [ "$2" = "reservations" ]; then
cat <<JSON
{
  "_meta": {"command": "robot reservations"},
  "all_active": [
    {
      "agent": "BeigeHollow",
      "path": "$AM_FAKE_PROJECT/scripts/agent_mail_snapshot.sh",
      "exclusive": true
    },
    {
      "agent": "OtherAgent",
      "path": "/Users/example/private/mail/archive.jsonl",
      "exclusive": false
    }
  ]
}
JSON
  exit 0
fi

if [ "$1" = "mail" ] && [ "$2" = "inbox" ]; then
  for arg in "$@"; do
    if [ "$arg" = "--include-bodies" ]; then
      printf 'producer must not request bodies\n' >&2
      exit 64
    fi
  done
cat <<'JSON'
[
  {
    "id": 1,
    "thread_id": "bd-6qcwh.2",
    "subject": "Use token ghp_abcdefghijklmnopqrstuvwxyz123456 in /private/tmp and /private/tmp/secret-case and /var/folders/zz/private-case",
    "created_ts": "2026-06-04T22:48:15Z",
    "ack_required": true,
    "body_md": "raw body must not appear"
  },
  {
    "id": 2,
    "thread_id": "bd-6qcwh.2",
    "subject": "Follow up",
    "created_ts": "2026-06-04T22:49:00Z",
    "ack_required": false
  }
]
JSON
  exit 0
fi

printf 'unexpected am command: %s\n' "$*" >&2
exit 2
EOF
chmod 755 "$FAKE_BIN/am"

mkdir -p "$SYMLINK_TARGET"
ln -s "$SYMLINK_TARGET" "$SYMLINK_PARENT"
if PATH="$FAKE_BIN:$PATH" \
    AM_FAKE_COMMAND_LOG="$COMMAND_LOG" \
    AM_FAKE_PROJECT="$PROJECT" \
    "$PRODUCER" \
      --project "$PROJECT" \
      --agent BeigeHollow \
      --output "$SYMLINK_OUTPUT" \
      >"$SYMLINK_STDOUT" \
      2>"$SYMLINK_STDERR"; then
    printf 'agent_mail_snapshot: producer accepted symlinked output path\n' >&2
    exit 1
fi
if ! grep -F 'path traverses symlink component' "$SYMLINK_STDERR" >/dev/null; then
    printf 'agent_mail_snapshot: missing symlink refusal diagnostic\n' >&2
    exit 1
fi
if [ -e "$SYMLINK_OUTPUT" ]; then
    printf 'agent_mail_snapshot: symlink-refused output was still written\n' >&2
    exit 1
fi

PATH="$FAKE_BIN:$PATH" \
AM_FAKE_COMMAND_LOG="$COMMAND_LOG" \
AM_FAKE_PROJECT="$PROJECT" \
"$PRODUCER" \
  --project "$PROJECT" \
  --agent BeigeHollow \
  --inbox-limit 5 \
  --thread-limit 5 \
  --timeout-sec 3 \
  --json \
  >"$SNAPSHOT_STDOUT_OK" \
  2>"$SNAPSHOT_STDOUT_STDERR"
if [ -s "$SNAPSHOT_STDOUT_STDERR" ]; then
    printf 'agent_mail_snapshot: --json wrote diagnostics to stderr\n' >&2
    exit 1
fi
jq -e '
  .producer_status == "ok"
  and .fallback_active == false
  and any(.threads[]; .thread_id == "bd-6qcwh.2" and .message_count == 2)
' "$SNAPSHOT_STDOUT_OK" >/dev/null
assert_snapshot_contract "$SNAPSHOT_STDOUT_OK"

PATH="$FAKE_BIN:$PATH" \
AM_FAKE_COMMAND_LOG="$COMMAND_LOG" \
AM_FAKE_PROJECT="$PROJECT" \
"$PRODUCER" \
  --project "$PROJECT" \
  --agent BeigeHollow \
  --inbox-limit 5 \
  --thread-limit 5 \
  --timeout-sec 3 \
  --json \
  --output "$SNAPSHOT_DUAL_FILE" \
  >"$SNAPSHOT_DUAL_STDOUT"
if ! cmp -s "$SNAPSHOT_DUAL_FILE" "$SNAPSHOT_DUAL_STDOUT"; then
    printf 'agent_mail_snapshot: --json --output wrote different stdout and file snapshots\n' >&2
    exit 1
fi
assert_snapshot_contract "$SNAPSHOT_DUAL_FILE"

PATH="$FAKE_BIN:$PATH" \
AM_FAKE_COMMAND_LOG="$COMMAND_LOG" \
AM_FAKE_PROJECT="$PROJECT" \
"$PRODUCER" \
  --project "$PROJECT" \
  --agent BeigeHollow \
  --inbox-limit 5 \
  --thread-limit 5 \
  --timeout-sec 3 \
  --coordination-output "$COORDINATION_OK" \
  --output "$SNAPSHOT_OK" \
  >"$SNAPSHOT_OUTPUT_ONLY_STDOUT"
if [ -s "$SNAPSHOT_OUTPUT_ONLY_STDOUT" ]; then
    printf 'agent_mail_snapshot: --output without --json wrote stdout\n' >&2
    exit 1
fi

jq -e '
  .redaction_status == "paths_counts_subjects_only_no_content"
  and .schema == "ee.agent_mail.snapshot.v1"
  and .agent_name == "BeigeHollow"
  and .producer_status == "ok"
  and .fallback_active == false
  and .summary.file_reservation_count == 2
  and .summary.degraded_count == 0
  and (.file_reservations | length) == 2
  and any(.file_reservations[]; .path_pattern == "scripts/agent_mail_snapshot.sh" and .holder == "BeigeHollow" and .exclusive == true)
  and any(.file_reservations[]; .path_pattern == "[REDACTED:absolute_path]")
  and (.agents | length) == 2
  and (.inbox[0].mailbox == "BeigeHollow")
  and (.inbox[0].unread_count == 2)
  and (.inbox[0].ack_required_count == 1)
  and any(.threads[]; .thread_id == "bd-6qcwh.2" and .message_count == 2)
  and (.source_commands | all(contains("--include-bodies") | not))
' "$SNAPSHOT_OK" >/dev/null
assert_snapshot_contract "$SNAPSHOT_OK"

jq 'del(.schema)' "$SNAPSHOT_OK" > "$SNAPSHOT_MISSING_SCHEMA"
if assert_snapshot_contract "$SNAPSHOT_MISSING_SCHEMA"; then
    printf 'agent_mail_snapshot: contract accepted snapshot missing schema\n' >&2
    exit 1
fi

jq '.producer_status = "degraded" | .fallback_active = true | .summary.degraded_count = 1 | .degraded = [{"code":"missing_fields"}]' \
    "$SNAPSHOT_OK" > "$SNAPSHOT_MALFORMED_DEGRADED"
if assert_snapshot_contract "$SNAPSHOT_MALFORMED_DEGRADED"; then
    printf 'agent_mail_snapshot: contract accepted malformed degraded entry\n' >&2
    exit 1
fi

jq '.threads[0].body_md = "raw body must not appear"' "$SNAPSHOT_OK" > "$SNAPSHOT_BODY_LEAK"
if assert_snapshot_contract "$SNAPSHOT_BODY_LEAK"; then
    printf 'agent_mail_snapshot: contract accepted raw body leak\n' >&2
    exit 1
fi

jq '.file_reservations[0].path_pattern = "/Users/example/private/mail/archive.jsonl"' \
    "$SNAPSHOT_OK" > "$SNAPSHOT_PATH_LEAK"
if assert_snapshot_contract "$SNAPSHOT_PATH_LEAK"; then
    printf 'agent_mail_snapshot: contract accepted private path leak\n' >&2
    exit 1
fi

jq -e '
  .schema == "ee.coordination_snapshot.v1"
  and .scope == "workspace"
  and (.sources | length) == 5
  and any(.sources[];
    .source_id == "agent_mail_reservations"
    and .status == "fresh"
    and any(.entries[];
      .kind == "file_reservation"
      and .path_pattern == "scripts/agent_mail_snapshot.sh"
      and .conflict == true
      and .severity == "warning"
    )
  )
  and any(.sources[];
    .source_id == "agent_mail_inbox"
    and any(.entries[];
      .kind == "agent_mail_inbox"
      and .id == "BeigeHollow"
      and .status == "ack_required"
    )
  )
  and any(.sources[];
    .source_id == "agent_mail_threads"
    and any(.entries[];
      .kind == "agent_mail_thread"
      and .id == "bd-6qcwh.2"
    )
  )
  and any(.sources[];
    .source_id == "agent_mail_snapshot_health"
    and .status == "fresh"
  )
' "$COORDINATION_OK" >/dev/null

if grep -E 'mail send|mail ack|mail read|file_reservations reserve|file_reservations release|doctor repair' "$COMMAND_LOG" >/dev/null; then
    printf 'agent_mail_snapshot: producer invoked a forbidden mutating Agent Mail command\n' >&2
    exit 1
fi

PATH="$FAKE_BIN:$PATH" \
AM_FAKE_COMMAND_LOG="$COMMAND_LOG" \
AM_FAKE_PROJECT="$PROJECT" \
AM_FAKE_MODE=fail_agents \
"$PRODUCER" \
  --project "$PROJECT" \
  --agent BeigeHollow \
  --coordination-output "$COORDINATION_DEGRADED" \
  --output "$SNAPSHOT_DEGRADED"

jq -e '
  .producer_status == "degraded"
  and .fallback_active == true
  and .am_agents_list_ok == false
  and .summary.degraded_count == 1
  and (.degraded | length) == 1
  and .file_reservations != null
  and .agents != null
  and .inbox != null
  and .threads != null
' "$SNAPSHOT_DEGRADED" >/dev/null
assert_snapshot_contract "$SNAPSHOT_DEGRADED"

jq -e '
  .schema == "ee.coordination_snapshot.v1"
  and any(.sources[];
    .source_id == "agent_mail_agents"
    and .status == "unavailable"
    and any(.degraded[]; .code == "agent_mail_snapshot_source_unavailable")
  )
  and any(.sources[];
    .source_id == "agent_mail_snapshot_health"
    and .status == "degraded"
    and any(.degraded[]; .code == "agent_mail_snapshot_source_unavailable")
  )
' "$COORDINATION_DEGRADED" >/dev/null

for snapshot in "$SNAPSHOT_OK" "$SNAPSHOT_DEGRADED" "$SNAPSHOT_STDOUT_OK" "$SNAPSHOT_DUAL_FILE" "$SNAPSHOT_DUAL_STDOUT" "$COORDINATION_OK" "$COORDINATION_DEGRADED"; do
    if grep -E 'ghp_|raw body|body_md|SECRET_TOKEN|agent list unavailable|/Users/|/Volumes/|/data/|/tmp/|/private/|/var/folders/' "$snapshot" >/dev/null; then
        printf 'agent_mail_snapshot: redaction leak in %s\n' "$snapshot" >&2
        exit 1
    fi
    if grep -F "$PROJECT" "$snapshot" >/dev/null; then
        printf 'agent_mail_snapshot: raw project path leaked in %s\n' "$snapshot" >&2
        exit 1
    fi
done

if grep -E 'mail (send|ack|read)|file_reservations (reserve|release)|doctor repair' "$PRODUCER" >/dev/null; then
    printf 'agent_mail_snapshot: producer source contains a forbidden mutating command\\n' >&2
    exit 1
fi

run_live_no_mock_e2e
run_claim_gate_fixture_e2e

jq -cn \
  --arg schema "ee.test_event.v1" \
  --arg surface "agent_mail_snapshot" \
  --arg phase "verdict" \
  --arg kind "note" \
  --arg healthy "$(shasum -a 256 "$SNAPSHOT_OK" | awk '{print "sha256:" $1}')" \
  --arg degraded "$(shasum -a 256 "$SNAPSHOT_DEGRADED" | awk '{print "sha256:" $1}')" \
  --arg coordination_healthy "$(shasum -a 256 "$COORDINATION_OK" | awk '{print "sha256:" $1}')" \
  --arg coordination_degraded "$(shasum -a 256 "$COORDINATION_DEGRADED" | awk '{print "sha256:" $1}')" \
  --arg live_mode "$LIVE_MODE" \
  --arg live_verdict "$LIVE_VERDICT" \
  --arg live_reason "$LIVE_REASON" \
  --arg live_pre_hash "$LIVE_PRE_HASH" \
  --arg live_post_hash "$LIVE_POST_HASH" \
  --arg live_snapshot_hash "$LIVE_SNAPSHOT_HASH" \
  --arg live_brief_hash "$LIVE_BRIEF_HASH" \
  --arg claim_gate_mode "$CLAIM_GATE_MODE" \
  --arg claim_gate_verdict "$CLAIM_GATE_VERDICT" \
  --arg claim_gate_reason "$CLAIM_GATE_REASON" \
  --arg claim_gate_snapshot_hash "$CLAIM_GATE_SNAPSHOT_HASH" \
  --arg claim_gate_clean_snapshot_hash "$CLAIM_GATE_CLEAN_SNAPSHOT_HASH" \
  --arg claim_gate_no_snapshot_hash "$CLAIM_GATE_NO_SNAPSHOT_HASH" \
  --arg claim_gate_with_snapshot_hash "$CLAIM_GATE_WITH_SNAPSHOT_HASH" \
  --arg claim_gate_with_clean_snapshot_hash "$CLAIM_GATE_WITH_CLEAN_SNAPSHOT_HASH" \
  --arg claim_gate_no_snapshot_exit "$CLAIM_GATE_NO_SNAPSHOT_EXIT" \
  --arg claim_gate_with_snapshot_exit "$CLAIM_GATE_WITH_SNAPSHOT_EXIT" \
  --arg claim_gate_with_clean_snapshot_exit "$CLAIM_GATE_WITH_CLEAN_SNAPSHOT_EXIT" \
  --arg claim_gate_no_snapshot_elapsed_ms "$CLAIM_GATE_NO_SNAPSHOT_ELAPSED_MS" \
  --arg claim_gate_with_snapshot_elapsed_ms "$CLAIM_GATE_WITH_SNAPSHOT_ELAPSED_MS" \
  --arg claim_gate_with_clean_snapshot_elapsed_ms "$CLAIM_GATE_WITH_CLEAN_SNAPSHOT_ELAPSED_MS" \
  --argjson claim_gate_no_snapshot_degraded_codes "$CLAIM_GATE_NO_SNAPSHOT_DEGRADED_CODES" \
  --argjson claim_gate_with_snapshot_degraded_codes "$CLAIM_GATE_WITH_SNAPSHOT_DEGRADED_CODES" \
  --argjson claim_gate_with_clean_snapshot_degraded_codes "$CLAIM_GATE_WITH_CLEAN_SNAPSHOT_DEGRADED_CODES" \
  '{
    schema: $schema,
    surface: $surface,
    phase: $phase,
    kind: $kind,
    verdict: "pass",
    mutationExecuted: false,
    healthySnapshotHash: $healthy,
    degradedSnapshotHash: $degraded,
    healthyCoordinationSnapshotHash: $coordination_healthy,
    degradedCoordinationSnapshotHash: $coordination_degraded,
    liveNoMock: {
      enabled: ($live_mode == "1"),
      verdict: $live_verdict,
      reason: $live_reason,
      preStateHash: $live_pre_hash,
      postStateHash: $live_post_hash,
      snapshotHash: $live_snapshot_hash,
      swarmBriefHash: $live_brief_hash
    },
    claimGateFixture: {
      enabled: ($claim_gate_mode != "0" and $claim_gate_mode != "false" and $claim_gate_mode != "no" and $claim_gate_mode != "off"),
      verdict: $claim_gate_verdict,
      reason: $claim_gate_reason,
      mutationExecuted: false,
      sanitizedEnvironment: {
        EE_AGENT_MAIL_SNAPSHOT_CLAIM_GATE_E2E: $claim_gate_mode,
        EE_BINARY: "set externally or PATH-resolved",
        TMPDIR: "[TMP]"
      },
      commands: [
        {
          label: "claim_gate_without_snapshot",
          command: "ee swarm work-packet --workspace [WORKSPACE] --sources beads,agent-mail --claim-gate --candidate bd-fixture-policy --json --command-timeout-ms 1000",
          cwd: "[WORKSPACE]",
          elapsedMs: (if $claim_gate_no_snapshot_elapsed_ms == "" then null else ($claim_gate_no_snapshot_elapsed_ms | tonumber) end),
          exitCode: (if $claim_gate_no_snapshot_exit == "" then null else ($claim_gate_no_snapshot_exit | tonumber) end),
          stdoutArtifactPath: "[ARTIFACT]/claim-gate-no-snapshot.json",
          stdoutArtifactHash: $claim_gate_no_snapshot_hash,
          stderrArtifactPath: "[ARTIFACT]/claim-gate-no-snapshot.stderr",
          degradedCodes: $claim_gate_no_snapshot_degraded_codes,
          schemaValidationStatus: (if $claim_gate_verdict == "pass" then "passed" else "not_run_or_degraded" end),
          redactionStatus: (if $claim_gate_verdict == "pass" then "passed" else "not_run_or_degraded" end)
        },
        {
          label: "claim_gate_with_conflict_snapshot",
          command: "ee swarm work-packet --workspace [WORKSPACE] --sources beads,agent-mail --agent-mail-snapshot [ARTIFACT]/claim-gate-snapshot.json --claim-gate --candidate bd-fixture-policy --json --command-timeout-ms 1000",
          cwd: "[WORKSPACE]",
          elapsedMs: (if $claim_gate_with_snapshot_elapsed_ms == "" then null else ($claim_gate_with_snapshot_elapsed_ms | tonumber) end),
          exitCode: (if $claim_gate_with_snapshot_exit == "" then null else ($claim_gate_with_snapshot_exit | tonumber) end),
          stdoutArtifactPath: "[ARTIFACT]/claim-gate-with-snapshot.json",
          stdoutArtifactHash: $claim_gate_with_snapshot_hash,
          stderrArtifactPath: "[ARTIFACT]/claim-gate-with-snapshot.stderr",
          degradedCodes: $claim_gate_with_snapshot_degraded_codes,
          schemaValidationStatus: (if $claim_gate_verdict == "pass" then "passed" else "not_run_or_degraded" end),
          redactionStatus: (if $claim_gate_verdict == "pass" then "passed" else "not_run_or_degraded" end)
        },
        {
          label: "claim_gate_with_clean_snapshot",
          command: "ee swarm work-packet --workspace [WORKSPACE] --sources beads,agent-mail --agent-mail-snapshot [ARTIFACT]/claim-gate-clean-snapshot.json --claim-gate --candidate bd-fixture-policy --json --command-timeout-ms 1000",
          cwd: "[WORKSPACE]",
          elapsedMs: (if $claim_gate_with_clean_snapshot_elapsed_ms == "" then null else ($claim_gate_with_clean_snapshot_elapsed_ms | tonumber) end),
          exitCode: (if $claim_gate_with_clean_snapshot_exit == "" then null else ($claim_gate_with_clean_snapshot_exit | tonumber) end),
          stdoutArtifactPath: "[ARTIFACT]/claim-gate-with-clean-snapshot.json",
          stdoutArtifactHash: $claim_gate_with_clean_snapshot_hash,
          stderrArtifactPath: "[ARTIFACT]/claim-gate-with-clean-snapshot.stderr",
          degradedCodes: $claim_gate_with_clean_snapshot_degraded_codes,
          schemaValidationStatus: (if $claim_gate_verdict == "pass" then "passed" else "not_run_or_degraded" end),
          redactionStatus: (if $claim_gate_verdict == "pass" then "passed" else "not_run_or_degraded" end)
        }
      ],
      snapshotHash: $claim_gate_snapshot_hash,
      cleanSnapshotHash: $claim_gate_clean_snapshot_hash,
      firstFailureDiagnosis: (if $claim_gate_verdict == "pass" then "" else $claim_gate_reason end)
    }
  }'

exit "$FINAL_EXIT"
