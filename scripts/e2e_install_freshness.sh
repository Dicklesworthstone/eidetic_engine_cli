#!/usr/bin/env bash
# bd-3utv2.7: public E2E coverage for install freshness in the swarm claim gate.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/lib/e2e_harness.sh"

harness_init "install_freshness"

ARTIFACT_DIR="$LOG_DIR/artifacts"
mkdir -p "$ARTIFACT_DIR"

now_ns() {
  python3 - <<'PY'
import time
print(time.time_ns())
PY
}

sanitize_label() {
  printf '%s' "$1" | tr -c 'A-Za-z0-9_.-' '_'
}

json_field_or_unavailable() {
  local file="$1"
  local filter="$2"

  if jq -er "$filter" "$file" >/dev/null 2>&1; then
    jq -er "$filter" "$file"
  else
    printf 'unavailable'
  fi
}

emit_command_event() {
  local label="$1"
  local exit_code="$2"
  local elapsed_ms="$3"
  local stdout_path="$4"
  local stderr_path="$5"
  local freshness_verdict="$6"
  local claim_gate_verdict="$7"
  local first_failure="$8"
  local path_label="$9"
  shift 9

  python3 - "$EE_TEST_LOG_PATH" "$HARNESS_TEST_NAME" "$label" "$exit_code" "$elapsed_ms" \
    "$stdout_path" "$stderr_path" "$freshness_verdict" "$claim_gate_verdict" \
    "$first_failure" "$path_label" "$REPO_ROOT" "$WS" "$@" <<'PY'
import datetime
import json
import os
import sys

(
    log_path,
    test_id,
    label,
    exit_code,
    elapsed_ms,
    stdout_path,
    stderr_path,
    freshness_verdict,
    claim_gate_verdict,
    first_failure,
    path_label,
    repo_root,
    workspace,
    *args,
) = sys.argv[1:]

def scrub(value: str) -> str:
    return value.replace(workspace, "[WORKSPACE]").replace(repo_root, "[REPO]")

event = {
    "schema": "ee.test_event.v1",
    "timestamp": datetime.datetime.now(datetime.UTC).isoformat().replace("+00:00", "Z"),
    "test_id": test_id,
    "kind": "command_end",
    "command": "ee",
    "args": [scrub(arg) for arg in args],
    "exit_code": int(exit_code),
    "elapsed_ms": int(elapsed_ms),
    "fields": {
        "label": label,
        "cwd": scrub(os.getcwd()),
        "workspace": "[WORKSPACE]",
        "sanitized_env": {
            "PATH": f"[fixture:{path_label}]",
            "EE_DATABASE_PATH": "[WORKSPACE]/.ee/e2e-install-freshness.db",
            "EE_INDEX_DIR": "[WORKSPACE]/.ee/e2e-install-freshness-index",
            "TMPDIR": "[redacted]",
        },
        "stdout_artifact_path": scrub(stdout_path),
        "stderr_artifact_path": scrub(stderr_path),
        "freshness_verdict": freshness_verdict,
        "claim_gate_verdict": claim_gate_verdict,
        "first_failure_diagnosis": first_failure,
    },
}

with open(log_path, "a", encoding="utf-8") as handle:
    handle.write(json.dumps(event, sort_keys=True, separators=(",", ":")) + "\n")
PY
}

run_ee_capture() {
  local label="$1"
  local path_label="$2"
  local path_value="$3"
  shift 3

  local safe_label
  safe_label="$(sanitize_label "$label")"
  LAST_STDOUT="$ARTIFACT_DIR/${safe_label}.stdout.json"
  LAST_STDERR="$ARTIFACT_DIR/${safe_label}.stderr.txt"

  local started finished elapsed_ms
  started="$(now_ns)"
  PATH="$path_value" \
    EE_DATABASE_PATH="$WS/.ee/e2e-install-freshness.db" \
    EE_INDEX_DIR="$WS/.ee/e2e-install-freshness-index" \
    "$EE_BIN" "$@" >"$LAST_STDOUT" 2>"$LAST_STDERR"
  LAST_RC=$?
  finished="$(now_ns)"
  elapsed_ms="$(((finished - started) / 1000000))"

  local freshness_verdict claim_gate_verdict first_failure
  freshness_verdict="$(json_field_or_unavailable "$LAST_STDOUT" '.data.sourceAuthority.installFreshnessVerdict // .data.freshness.verdict')"
  claim_gate_verdict="$(json_field_or_unavailable "$LAST_STDOUT" '.data.verdict')"
  first_failure="$(json_field_or_unavailable "$LAST_STDOUT" '.data.degradedCodes[0] // .degraded[0].code // .data.unsafeReasons[0]')"

  emit_command_event "$label" "$LAST_RC" "$elapsed_ms" "$LAST_STDOUT" "$LAST_STDERR" \
    "$freshness_verdict" "$claim_gate_verdict" "$first_failure" "$path_label" "$@"
}

with_temp_workspace WS

mkdir -p "$WS/.ee" "$WS/fixtures/path/stale" "$WS/fixtures/path/current"

cat >"$WS/fixtures/path/stale/ee" <<'EOF'
#!/usr/bin/env sh
case "${1:-}" in
  --version|version)
    printf 'ee 0.1.0\n'
    ;;
  *)
    printf 'stale ee fixture only supports version probes\n' >&2
    exit 64
    ;;
esac
EOF
chmod +x "$WS/fixtures/path/stale/ee"

ln -s "$EE_BIN" "$WS/fixtures/path/current/ee"

FRESH_PATH="$WS/fixtures/path/current:${PATH:-}"
STALE_DUPLICATE_PATH="$WS/fixtures/path/stale:$WS/fixtures/path/current:${PATH:-}"

run_ee_capture "init" "fresh" "$FRESH_PATH" init --workspace "$WS" --json
init_json="$(cat "$LAST_STDOUT")"
assert_jq "$init_json" '.success == true' "temporary workspace initializes"

run_ee_capture "stale_install_check" "stale-duplicate" "$STALE_DUPLICATE_PATH" \
  install check --workspace "$WS" --offline --json
stale_install_json="$(cat "$LAST_STDOUT")"
assert_jq "$stale_install_json" '.success == true' "stale install check succeeds as a diagnostic command"
assert_jq "$stale_install_json" '.data.path.status == "duplicate"' "duplicate PATH entries are reported"
assert_jq "$stale_install_json" 'any(.data.findings[]?; .code == "duplicate_path_binary")' "duplicate PATH finding is present"
assert_jq "$stale_install_json" 'any(.data.findings[]?; .code == "current_binary_shadowed")' "shadowed current binary finding is present"
assert_jq "$stale_install_json" 'any(.data.findings[]?; .code == "offline_no_manifest")' "offline manifest absence is reported"

run_ee_capture "stale_claim_gate" "stale-duplicate" "$STALE_DUPLICATE_PATH" \
  swarm work-packet --workspace "$WS" --sources none --claim-gate --candidate bd-3utv2.7 --json
stale_gate_json="$(cat "$LAST_STDOUT")"
assert_jq "$stale_gate_json" '.success == true' "stale claim-gate command returns an envelope"
assert_jq "$stale_gate_json" '.data.safeToClaim == false' "stale install freshness blocks the claim gate"
assert_jq "$stale_gate_json" '.data.sourceAuthority.installFreshnessAuthoritative == false' "stale install freshness is not authoritative"
assert_jq "$stale_gate_json" '.data.sourceAuthority.installFreshnessVerdict == "shadowed_binary"' "shadowed binary is the claim-gate freshness verdict"
assert_jq "$stale_gate_json" '(.data.degradedCodes // []) | index("stale_binary_suspected") != null' "stale binary degradation code is emitted"
assert_jq "$stale_gate_json" 'any(.degraded[]?; ((.sources // []) | index("install-freshness")) != null)' "install-freshness degradation is attached"
assert_jq "$stale_gate_json" '.data.claimCommandAction == null' "stale claim gate never returns a claim command"
assert_jq "$stale_gate_json" 'any(.data.recoveryActions[]?; .kind == "verify_source_version")' "claim gate suggests non-mutating install verification"

run_ee_capture "fresh_claim_gate" "fresh" "$FRESH_PATH" \
  swarm work-packet --workspace "$WS" --sources none --claim-gate --candidate bd-3utv2.7 --json
fresh_gate_json="$(cat "$LAST_STDOUT")"
assert_jq "$fresh_gate_json" '.success == true' "fresh claim-gate command returns an envelope"
assert_jq "$fresh_gate_json" '.data.sourceAuthority.installFreshnessAuthoritative == true' "fresh install freshness is authoritative"
assert_jq "$fresh_gate_json" '.data.sourceAuthority.installFreshnessVerdict == "fresh"' "fresh control path reports a fresh binary"
assert_jq "$fresh_gate_json" '((.data.degradedCodes // []) | index("stale_binary_suspected")) == null' "fresh path does not emit stale binary degradation"
assert_jq "$fresh_gate_json" 'all(.degraded[]?; ((.sources // []) | index("install-freshness")) == null)' "fresh path has no install-freshness degradation"
assert_jq "$fresh_gate_json" '((.data.unsafeReasons // []) | index("claim_gate_install_freshness_not_authoritative")) == null' "fresh path leaves downstream gates to decide"

harness_summary
