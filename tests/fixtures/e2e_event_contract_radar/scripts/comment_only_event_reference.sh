#!/usr/bin/env bash
set -euo pipefail

# This fixture intentionally mentions ee.test_event.v1, command_start,
# command_end, assert_result, schema_validation_status, redaction_status,
# first_failure_diagnosis, stdout_artifact_path, stderr_artifact_path, and
# sanitized_env only in comments. The radar must not classify comment-only
# references as structured event logging.

printf 'plain helper with documentation-only event references\n'
