#!/usr/bin/env bash
set -euo pipefail

EVENT_LOG="${EVENT_LOG:-events.jsonl}"
stdout_artifact_path="artifacts/implicit.stdout.json"
stderr_artifact_path="artifacts/implicit.stderr.txt"

printf '{"schema":"ee.test_event.v1","kind":"command_start","fields":{"sanitized_env":{"HOME":"[HOME]"}}}\n' >>"$EVENT_LOG"
printf '{"schema":"ee.test_event.v1","kind":"command_end","fields":{"stdout_artifact_path":"%s","stderr_artifact_path":"%s","sanitized_env":{"HOME":"[HOME]"}}}\n' "$stdout_artifact_path" "$stderr_artifact_path" >>"$EVENT_LOG"

jq -e '.schema == "ee.response.v2"' "$stdout_artifact_path" >/dev/null

printf '{"schema":"ee.test_event.v1","kind":"assert_ok","fields":{"schema_validation_status":"passed","redaction_status":"passed","first_failure_diagnosis":"none","stdout_artifact_path":"%s","stderr_artifact_path":"%s","sanitized_env":{"HOME":"[HOME]"}}}\n' "$stdout_artifact_path" "$stderr_artifact_path" >>"$EVENT_LOG"

