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
LIVE_VERDICT="skipped"
LIVE_REASON="env_disabled"
LIVE_PRE_HASH=""
LIVE_POST_HASH=""
LIVE_SNAPSHOT_HASH=""
LIVE_BRIEF_HASH=""
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
    }
  }'

exit "$FINAL_EXIT"
