#!/usr/bin/env bash
set -euo pipefail

EVENT_LOG="${EVENT_LOG:-events.jsonl}"

emit_cleanup_event() {
  printf '{"schema":"ee.test_event.v1","kind":"cleanup","fields":{"redaction_status":"not_checked"}}\n' >>"$EVENT_LOG"
}

trap emit_cleanup_event EXIT

if ! command -v jq >/dev/null 2>&1; then
  printf 'jq missing\n' >&2
  exit 1
fi

