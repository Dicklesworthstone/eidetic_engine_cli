#!/usr/bin/env bash
# Script/static checks for the read-only Agent Mail snapshot producer.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
PRODUCER="$REPO_ROOT/scripts/agent_mail_snapshot.sh"
TMP_BASE="${TMPDIR:-/tmp}"
case "$TMP_BASE" in
  /Volumes/*) TMP_BASE="/tmp" ;;
esac
TMP_ROOT="$TMP_BASE/ee-agent-mail-snapshot-e2e.$$"
FAKE_BIN="$TMP_ROOT/bin"
PROJECT="$TMP_ROOT/workspace"
COMMAND_LOG="$TMP_ROOT/am-commands.log"
SNAPSHOT_OK="$TMP_ROOT/snapshot-ok.json"
SNAPSHOT_DEGRADED="$TMP_ROOT/snapshot-degraded.json"

mkdir -p "$FAKE_BIN" "$PROJECT"
: > "$COMMAND_LOG"

if ! command -v jq >/dev/null 2>&1; then
    printf 'agent_mail_snapshot: jq is required\n' >&2
    exit 2
fi

bash -n "$PRODUCER"

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
    "subject": "Use token ghp_abcdefghijklmnopqrstuvwxyz123456 in review",
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

PATH="$FAKE_BIN:$PATH" \
AM_FAKE_COMMAND_LOG="$COMMAND_LOG" \
AM_FAKE_PROJECT="$PROJECT" \
"$PRODUCER" \
  --project "$PROJECT" \
  --agent BeigeHollow \
  --inbox-limit 5 \
  --thread-limit 5 \
  --timeout-sec 3 \
  --output "$SNAPSHOT_OK"

jq -e '
  .redaction_status == "paths_counts_subjects_only_no_content"
  and .producer_status == "ok"
  and .fallback_active == false
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
  --output "$SNAPSHOT_DEGRADED"

jq -e '
  .producer_status == "degraded"
  and .fallback_active == true
  and .am_agents_list_ok == false
  and (.degraded | length) == 1
  and .file_reservations != null
  and .agents != null
  and .inbox != null
  and .threads != null
' "$SNAPSHOT_DEGRADED" >/dev/null

for snapshot in "$SNAPSHOT_OK" "$SNAPSHOT_DEGRADED"; do
    if grep -E 'ghp_|raw body|body_md|SECRET_TOKEN|agent list unavailable|/Users/|/Volumes/|/data/|/tmp/' "$snapshot" >/dev/null; then
        printf 'agent_mail_snapshot: redaction leak in %s\n' "$snapshot" >&2
        exit 1
    fi
    if grep -F "$PROJECT" "$snapshot" >/dev/null; then
        printf 'agent_mail_snapshot: raw project path leaked in %s\n' "$snapshot" >&2
        exit 1
    fi
done

jq -cn \
  --arg schema "ee.test_event.v1" \
  --arg surface "agent_mail_snapshot" \
  --arg phase "verdict" \
  --arg kind "note" \
  --arg healthy "$(shasum -a 256 "$SNAPSHOT_OK" | awk '{print "sha256:" $1}')" \
  --arg degraded "$(shasum -a 256 "$SNAPSHOT_DEGRADED" | awk '{print "sha256:" $1}')" \
  '{
    schema: $schema,
    surface: $surface,
    phase: $phase,
    kind: $kind,
    verdict: "pass",
    mutationExecuted: false,
    healthySnapshotHash: $healthy,
    degradedSnapshotHash: $degraded
  }'
