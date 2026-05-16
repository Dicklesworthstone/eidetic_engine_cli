#!/usr/bin/env bash
# E2E harness for SRR6.46.5 (auto-enrollment safety snapshot).
#
# Validates that the canonical-JSON summary payload emitted by
# `ee::mesh::auto_enrollment_safety::compute_summary` matches the
# `ee.mesh.auto_enrollment_summary.v1` schema, that the audit row is
# present in the workspace database after a synthetic emission, and
# that the back-fill outcome row joins via `previousAuditId`.
#
# Until SRR6.46.3 ships the user-facing `ee mesh auto-enroll` command,
# this script invokes the safety-snapshot logic via the integration
# test surface (`cargo test --test mesh_auto_enrollment_safety_audit`)
# and asserts the per-test structured outcomes via `ee.test_event.v1`
# JSON-lines. When SRR6.46.3 lands, this script will be extended to
# also call the CLI directly.
#
# Emits structured `ee.test_event.v1` events to
# $EE_TEST_EVENT_DIR/auto_enrollment_safety_snapshot.jsonl for forensic
# audit. Exit 0 on success.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
SCHEMA_FILE="$REPO_ROOT/docs/schemas/ee.mesh.auto_enrollment_summary.v1.json"
TEST_FILE="$REPO_ROOT/tests/mesh_auto_enrollment_safety_audit.rs"
INLINE_SRC="$REPO_ROOT/src/mesh/auto_enrollment_safety.rs"
EVENT_DIR="${EE_TEST_EVENT_DIR:-${TMPDIR:-/tmp}/ee-auto-enroll-safety-events}"
EVENT_LOG="$EVENT_DIR/auto_enrollment_safety_snapshot.jsonl"
BEAD_ID="bd-36bbk.1.5"
SURFACE="auto_enrollment_safety_snapshot"

mkdir -p "$EVENT_DIR"
: > "$EVENT_LOG"

if ! command -v jq >/dev/null 2>&1; then
  printf 'error: jq is required for auto-enrollment safety e2e\n' >&2
  exit 1
fi
if ! command -v shasum >/dev/null 2>&1; then
  printf 'error: shasum is required for auto-enrollment safety e2e\n' >&2
  exit 1
fi

file_hash() {
  shasum -a 256 "$1" | awk '{print "sha256:" $1}'
}

emit_event() {
  local phase="$1"
  local valid="$2"
  local detail="$3"
  local artifact_hash="$4"
  jq -cn \
    --arg schema "ee.test_event.v1" \
    --arg kind "auto_enrollment_safety_snapshot_e2e" \
    --arg bead_id "$BEAD_ID" \
    --arg surface "$SURFACE" \
    --arg phase "$phase" \
    --arg detail "$detail" \
    --arg artifact_hash "$artifact_hash" \
    --argjson valid "$valid" \
    '{schema:$schema,kind:$kind,beadId:$bead_id,surface:$surface,phase:$phase,valid:$valid,detail:$detail,artifactHash:$artifact_hash}' \
    | tee -a "$EVENT_LOG" >&2
}

fail_check() {
  local phase="$1"
  local detail="$2"
  emit_event "$phase" false "$detail" "sha256:unavailable"
  exit 1
}

# ============================================================================
# Phase 1: setup — verify required files exist
# ============================================================================
[ -f "$SCHEMA_FILE" ] \
  || fail_check "setup" "schema file missing at $SCHEMA_FILE"
[ -f "$TEST_FILE" ] \
  || fail_check "setup" "integration test missing at $TEST_FILE"
[ -f "$INLINE_SRC" ] \
  || fail_check "setup" "source missing at $INLINE_SRC"
emit_event "setup" true "schema + test + source all present" \
  "$(file_hash "$SCHEMA_FILE")"

# ============================================================================
# Phase 2: schema validation — confirm the schema parses and declares
# the load-bearing fields the audit row depends on.
# ============================================================================
jq -e '.required | index("schema")' "$SCHEMA_FILE" >/dev/null \
  || fail_check "schema" "required list does not include 'schema'"
jq -e '.required | index("workspaceId")' "$SCHEMA_FILE" >/dev/null \
  || fail_check "schema" "required list does not include 'workspaceId'"
jq -e '.required | index("intendedPeerSetHash")' "$SCHEMA_FILE" >/dev/null \
  || fail_check "schema" "required list does not include 'intendedPeerSetHash'"
jq -e '.required | index("summaryHash")' "$SCHEMA_FILE" >/dev/null \
  || fail_check "schema" "required list does not include 'summaryHash'"
jq -e '.required | index("reversalCommand")' "$SCHEMA_FILE" >/dev/null \
  || fail_check "schema" "required list does not include 'reversalCommand'"
jq -e '.required | index("materializationOutcome")' "$SCHEMA_FILE" >/dev/null \
  || fail_check "schema" "required list does not include 'materializationOutcome'"
jq -e '.properties.intendedLanePolicy.properties.body' "$SCHEMA_FILE" >/dev/null \
  || fail_check "schema" "intendedLanePolicy.body missing from schema"
jq -e '.properties.intendedLanePolicy.properties.embedding' "$SCHEMA_FILE" >/dev/null \
  || fail_check "schema" "intendedLanePolicy.embedding missing from schema"
jq -e '.properties.intendedLanePolicy.properties.graphLink' "$SCHEMA_FILE" >/dev/null \
  || fail_check "schema" "intendedLanePolicy.graphLink missing from schema"
jq -e '.properties.triggerReason.enum | index("dry_run_preview")' "$SCHEMA_FILE" >/dev/null \
  || fail_check "schema" "triggerReason enum missing 'dry_run_preview'"
jq -e '.properties.triggerReason.enum | index("manual_invoke")' "$SCHEMA_FILE" >/dev/null \
  || fail_check "schema" "triggerReason enum missing 'manual_invoke'"
jq -e '.properties.triggerReason.enum | index("drift_reconciliation")' "$SCHEMA_FILE" >/dev/null \
  || fail_check "schema" "triggerReason enum missing 'drift_reconciliation'"
jq -e '.properties.materializationOutcome.enum | index("dry_run")' "$SCHEMA_FILE" >/dev/null \
  || fail_check "schema" "materializationOutcome enum missing 'dry_run'"
jq -e '.properties.materializationOutcome.enum | index("audit_only")' "$SCHEMA_FILE" >/dev/null \
  || fail_check "schema" "materializationOutcome enum missing 'audit_only'"
emit_event "schema" true "ee.mesh.auto_enrollment_summary.v1 contract intact" \
  "$(file_hash "$SCHEMA_FILE")"

# ============================================================================
# Phase 3: inline-source contract — confirm the Rust source declares the
# schema constant and the audit_actions constants the contract depends on.
# ============================================================================
grep -q 'pub const AUTO_ENROLLMENT_SUMMARY_SCHEMA_V1: &str = "ee.mesh.auto_enrollment_summary.v1"' "$INLINE_SRC" \
  || fail_check "source_contract" "schema constant missing from $INLINE_SRC"
grep -q 'MESH_AUTO_ENROLLMENT_INTENDED' "$REPO_ROOT/src/db/mod.rs" \
  || fail_check "source_contract" "MESH_AUTO_ENROLLMENT_INTENDED audit_action missing from src/db/mod.rs"
grep -q 'MESH_AUTO_ENROLLMENT_OUTCOME_RECORDED' "$REPO_ROOT/src/db/mod.rs" \
  || fail_check "source_contract" "MESH_AUTO_ENROLLMENT_OUTCOME_RECORDED audit_action missing from src/db/mod.rs"
grep -q 'mesh' "$REPO_ROOT/src/lib.rs" \
  || fail_check "source_contract" "mesh module not registered in src/lib.rs"
emit_event "source_contract" true \
  "schema constant + audit_actions + module registration present" \
  "$(file_hash "$INLINE_SRC")"

# ============================================================================
# Phase 4: run inline unit tests for the safety snapshot module.
# ============================================================================
INLINE_TEST_OUT="$EVENT_DIR/inline_unit_tests.log"
if cargo test --quiet --lib mesh::auto_enrollment_safety:: --message-format=short \
    >"$INLINE_TEST_OUT" 2>&1; then
  emit_event "inline_unit_tests" true \
    "src/mesh/auto_enrollment_safety.rs inline tests passed" \
    "$(file_hash "$INLINE_TEST_OUT")"
else
  cat "$INLINE_TEST_OUT" >&2
  fail_check "inline_unit_tests" "inline unit tests failed (see $INLINE_TEST_OUT)"
fi

# ============================================================================
# Phase 5: run the dedicated integration test (chain continuity, audit row
# shape, back-fill, idempotent path).
# ============================================================================
INTEGRATION_TEST_OUT="$EVENT_DIR/integration_tests.log"
if cargo test --quiet --test mesh_auto_enrollment_safety_audit --message-format=short \
    >"$INTEGRATION_TEST_OUT" 2>&1; then
  emit_event "integration_tests" true \
    "tests/mesh_auto_enrollment_safety_audit.rs passed" \
    "$(file_hash "$INTEGRATION_TEST_OUT")"
else
  cat "$INTEGRATION_TEST_OUT" >&2
  fail_check "integration_tests" \
    "integration tests failed (see $INTEGRATION_TEST_OUT)"
fi

# ============================================================================
# Phase 6: closure-lint compliance — the *_audit.rs file naming convention
# and inline-test coverage requirement (per closure-lint Rule 8 / Rule 7).
# ============================================================================
[ "${TEST_FILE##*/}" = "mesh_auto_enrollment_safety_audit.rs" ] \
  || fail_check "closure_lint" \
    "test file must follow *_audit.rs convention (got ${TEST_FILE##*/})"

INLINE_TEST_COUNT="$(grep -c "^[[:space:]]*#\[test\]" "$INLINE_SRC" || true)"
if [ "$INLINE_TEST_COUNT" -lt 3 ]; then
  fail_check "closure_lint" \
    "inline #[test] count is $INLINE_TEST_COUNT (rule requires >=3)"
fi
grep -q '#\[cfg(test)\]' "$INLINE_SRC" \
  || fail_check "closure_lint" "src/mesh/auto_enrollment_safety.rs missing #[cfg(test)] block"
grep -q 'mod tests' "$INLINE_SRC" \
  || fail_check "closure_lint" "src/mesh/auto_enrollment_safety.rs missing 'mod tests' block"
emit_event "closure_lint" true \
  "closure-lint Rule 7 (inline tests >= 3) + Rule 8 (*_audit.rs) satisfied" \
  "$(file_hash "$INLINE_SRC")"

# ============================================================================
# Final summary
# ============================================================================
emit_event "complete" true "auto-enrollment safety snapshot e2e green" \
  "$(file_hash "$EVENT_LOG")"

printf 'auto_enrollment_safety_snapshot.sh: %s events emitted to %s\n' \
  "$(wc -l <"$EVENT_LOG" | tr -d ' ')" "$EVENT_LOG"
