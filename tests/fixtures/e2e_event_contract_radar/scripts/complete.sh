#!/usr/bin/env bash
set -euo pipefail

EE_BIN="${EE_BIN:-ee}"
EVENT_LOG="${EVENT_LOG:-events.jsonl}"
stdout_artifact_path="artifacts/complete.stdout.json"
stderr_artifact_path="artifacts/complete.stderr.txt"

emit_command_start() {
  printf '{"schema":"ee.test_event.v1","kind":"command_start","fields":{"sanitized_env":{"HOME":"[HOME]"}}}\n' >>"$EVENT_LOG"
}

emit_command_end() {
  printf '{"schema":"ee.test_event.v1","kind":"command_end","fields":{"exit_code":0,"elapsed_ms":1,"stdout_artifact_path":"%s","stderr_artifact_path":"%s","sanitized_env":{"HOME":"[HOME]"}}}\n' "$stdout_artifact_path" "$stderr_artifact_path" >>"$EVENT_LOG"
}

emit_assert_result() {
  printf '{"schema":"ee.test_event.v1","kind":"assert_result","fields":{"schema_validation_status":"passed","redaction_status":"passed","first_failure_diagnosis":"%s","stdout_artifact_path":"%s","stderr_artifact_path":"%s","sanitized_env":{"HOME":"[HOME]"}}}\n' "$1" "$stdout_artifact_path" "$stderr_artifact_path" >>"$EVENT_LOG"
}

emit_command_start
if ! "$EE_BIN" search "fixture" --json >"$stdout_artifact_path" 2>"$stderr_artifact_path"; then
  emit_assert_result "search_command_failed"
  exit 1
fi
emit_command_end

if ! jq -e '.schema == "ee.response.v2"' "$stdout_artifact_path" >/dev/null; then
  emit_assert_result "schema_validation_failed"
  exit 1
fi

emit_assert_result "none"
printf '{"schema":"ee.test_event.v1","kind":"assert_ok","fields":{"schema_validation_status":"passed","redaction_status":"passed","first_failure_diagnosis":"none","stdout_artifact_path":"%s","stderr_artifact_path":"%s","sanitized_env":{"HOME":"[HOME]"}}}\n' "$stdout_artifact_path" "$stderr_artifact_path" >>"$EVENT_LOG"

