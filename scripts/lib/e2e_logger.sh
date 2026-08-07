#!/usr/bin/env bash
# J1 — bash side of the structured test logging harness.
# Companion to src/obs/test_log.rs. Both follow docs/schemas/test_event_v1.json.
#
# Usage:
#   source "$(dirname "$0")/../../scripts/lib/e2e_logger.sh"
#   e2e_log_start "epic_a_pack_format"
#   e2e_log_command "$EE" remember "hello world" --workspace . --json
#   e2e_log_assert_eq "$ITEM_COUNT" "13" "item_count"
#   e2e_log_end
#
# The harness is opt-in: when EE_TEST_LOG_PATH is unset (and no -start call set
# it), every helper no-ops silently. This lets shared scripts run both inside
# and outside the per-epic driver harness.

set -o pipefail

# ============================================================================
# Globals
# ============================================================================

EE_TEST_LOG_TEST_ID="${EE_TEST_LOG_TEST_ID:-}"
EE_TEST_LOG_LEVEL="${EE_TEST_LOG_LEVEL:-normal}"
EE_TEST_LOG_STDERR_CAP="${EE_TEST_LOG_STDERR_CAP:-4096}"
EE_TEST_LOG_ASSERTS_PASS=0
EE_TEST_LOG_ASSERTS_FAIL=0
EE_TEST_LOG_SCHEMA="ee.test_event.v1"

# ============================================================================
# Internals
# ============================================================================

_e2e_now_iso() {
    # Subsecond RFC 3339 UTC. Coreutils date doesn't support %N on macOS, so we
    # use python3 (always present on the platforms we target).
    python3 -c "from datetime import datetime, timezone; print(datetime.now(timezone.utc).isoformat(timespec='microseconds').replace('+00:00','Z'))"
}

# Use Python for BLAKE3 if available; otherwise fall back to a SHA-256-prefixed
# placeholder (we still mark it with a `sha256:` prefix so consumers can tell).
_e2e_hash_file() {
    local file="$1"
    if command -v b3sum >/dev/null 2>&1; then
        printf 'blake3:%s' "$(b3sum "$file" | awk '{print $1}')"
    elif python3 -c "import blake3" >/dev/null 2>&1; then
        python3 -c "import sys,blake3; print('blake3:'+blake3.blake3(open(sys.argv[1],'rb').read()).hexdigest())" "$file"
    else
        printf 'sha256:%s' "$(shasum -a 256 "$file" | awk '{print $1}')"
    fi
}

_e2e_sha256_file() {
    local file="$1"
    if command -v shasum >/dev/null 2>&1; then
        printf 'sha256:%s' "$(shasum -a 256 "$file" | awk '{print $1}')"
    elif command -v sha256sum >/dev/null 2>&1; then
        printf 'sha256:%s' "$(sha256sum "$file" | awk '{print $1}')"
    else
        python3 -c "import hashlib,sys; print('sha256:'+hashlib.sha256(open(sys.argv[1],'rb').read()).hexdigest())" "$file"
    fi
}

_e2e_hash_string() {
    local str="$1"
    if command -v b3sum >/dev/null 2>&1; then
        printf 'blake3:%s' "$(printf '%s' "$str" | b3sum | awk '{print $1}')"
    elif python3 -c "import blake3" >/dev/null 2>&1; then
        printf '%s' "$str" | python3 -c "import sys,blake3; print('blake3:'+blake3.blake3(sys.stdin.buffer.read()).hexdigest())"
    else
        printf 'sha256:%s' "$(printf '%s' "$str" | shasum -a 256 | awk '{print $1}')"
    fi
}

_e2e_source_hash() {
    local root="${REPO_ROOT:-$(pwd)}"
    local payload=""
    local rel
    for rel in Cargo.lock Cargo.toml rust-toolchain.toml; do
        if [ -f "$root/$rel" ]; then
            payload="${payload}${rel}=$(_e2e_hash_file "$root/$rel")"$'\n'
        fi
    done
    if [ -z "$payload" ]; then
        printf 'unavailable'
    else
        _e2e_hash_string "$payload"
    fi
}

# Read the consumer-side verification report for a packaged remote artifact.
# Output uses ASCII record separators because JSON string escaping guarantees
# that the delimiter cannot occur literally inside the compact report value.
_e2e_remote_artifact_attestation_fields() {
    local report_path="$1"
    python3 - "$report_path" <<'PYEOF'
import json
import hashlib
import re
import sys

report_path = sys.argv[1]
try:
    with open(report_path, "r", encoding="utf-8") as handle:
        report = json.load(handle)
except (OSError, json.JSONDecodeError) as error:
    raise SystemExit(f"remote artifact verification report is unreadable: {error}")

if not isinstance(report, dict):
    raise SystemExit("remote artifact verification report must be a JSON object")
if report.get("schema") != "ee.remote_build_artifact_manifest.verification.v1":
    raise SystemExit("remote artifact verification report has an unsupported schema")

required_keys = (
    "schema",
    "status",
    "accepted",
    "artifactName",
    "artifactId",
    "repository",
    "workflow",
    "runId",
    "runAttempt",
    "sourceCommit",
    "gitTree",
    "manifestHash",
    "buildCommandHash",
    "effectiveInputHash",
    "provenanceHash",
    "target",
    "profile",
    "binaryHash",
    "archiveHash",
    "archiveSizeBytes",
    "checksumStatus",
    "probeStatus",
    "probes",
    "rejections",
    "rawOutputIncluded",
    "verificationHash",
)
missing_keys = [key for key in required_keys if key not in report]
if missing_keys:
    raise SystemExit(
        "remote artifact verification report is incomplete: " + ",".join(missing_keys)
    )
unexpected_keys = sorted(set(report) - set(required_keys))
if unexpected_keys:
    raise SystemExit(
        "remote artifact verification report has unexpected fields: "
        + ",".join(unexpected_keys)
    )
if report["status"] != "verified":
    raise SystemExit("remote artifact verification report status is not verified")
if report["accepted"] is not True:
    raise SystemExit("remote artifact verification report is not accepted")
if report["checksumStatus"] != "verified":
    raise SystemExit("remote artifact verification report checksum is not verified")
if report["probeStatus"] != "passed":
    raise SystemExit("remote artifact verification report probes did not pass")
if report["rawOutputIncluded"] is not False:
    raise SystemExit("remote artifact verification report must exclude raw output")
if report["rejections"] != []:
    raise SystemExit("remote artifact verification report contains rejections")
if (
    not isinstance(report["probes"], list)
    or not 2 <= len(report["probes"]) <= 16
):
    raise SystemExit("remote artifact verification report has invalid probes")

probe_keys = {
    "id",
    "argvHash",
    "exitCode",
    "stdoutHash",
    "stderrHash",
    "status",
    "semanticAssertions",
}
assertion_keys = {"path", "expected", "observed", "matched"}
probe_ids = set()
probes_by_id = {}
for probe in report["probes"]:
    if not isinstance(probe, dict) or set(probe) != probe_keys:
        raise SystemExit("remote artifact verification report has an invalid probe shape")
    probe_id = probe["id"]
    if not isinstance(probe_id, str) or not probe_id:
        raise SystemExit("remote artifact verification report has an invalid probe id")
    if probe_id in probe_ids:
        raise SystemExit("remote artifact verification report has duplicate probe ids")
    probe_ids.add(probe_id)
    probes_by_id[probe_id] = probe
    assertions = probe["semanticAssertions"]
    if not isinstance(assertions, list):
        raise SystemExit("remote artifact verification report has invalid probe assertions")
    if any(
        not isinstance(assertion, dict) or set(assertion) != assertion_keys
        for assertion in assertions
    ):
        raise SystemExit("remote artifact verification report has an invalid assertion shape")
    assertion_paths = set()
    for assertion in assertions:
        path = assertion["path"]
        if not isinstance(path, str) or not path.startswith("/"):
            raise SystemExit("remote artifact verification report has an invalid assertion path")
        if path in assertion_paths:
            raise SystemExit("remote artifact verification report has duplicate assertion paths")
        assertion_paths.add(path)
        if assertion["matched"] is not True or assertion["expected"] != assertion["observed"]:
            raise SystemExit("remote artifact verification report has unmatched probe evidence")

git_object = re.compile(r"^[0-9a-f]{40}$")
sha256 = re.compile(r"^sha256:[0-9a-f]{64}$")
positive_decimal = re.compile(r"^[1-9][0-9]*$")
for key in ("artifactName", "repository", "workflow", "target", "profile"):
    value = report[key]
    if not isinstance(value, str) or not value.strip():
        raise SystemExit(f"remote artifact verification report has invalid {key}")
for key in ("artifactId", "runId"):
    value = report[key]
    if not isinstance(value, str) or positive_decimal.fullmatch(value) is None:
        raise SystemExit(f"remote artifact verification report has invalid {key}")
if type(report["runAttempt"]) is not int or report["runAttempt"] < 1:
    raise SystemExit("remote artifact verification report has invalid runAttempt")
if type(report["archiveSizeBytes"]) is not int or report["archiveSizeBytes"] < 1:
    raise SystemExit("remote artifact verification report has invalid archiveSizeBytes")
for key in ("sourceCommit", "gitTree"):
    value = report.get(key)
    if not isinstance(value, str) or git_object.fullmatch(value) is None:
        raise SystemExit(f"remote artifact verification report has invalid {key}")
for key in (
    "manifestHash",
    "buildCommandHash",
    "effectiveInputHash",
    "provenanceHash",
    "binaryHash",
    "archiveHash",
    "verificationHash",
):
    value = report.get(key)
    if not isinstance(value, str) or sha256.fullmatch(value) is None:
        raise SystemExit(f"remote artifact verification report has invalid {key}")

for probe in report["probes"]:
    if (
        type(probe["exitCode"]) is not int
        or probe["exitCode"] != 0
        or probe["status"] != "passed"
    ):
        raise SystemExit("remote artifact verification report contains a failed probe")
    for key in ("argvHash", "stdoutHash", "stderrHash"):
        value = probe[key]
        if not isinstance(value, str) or sha256.fullmatch(value) is None:
            raise SystemExit(
                f"remote artifact verification report probe has invalid {key}"
            )

required_probes = {
    "version_json": {
        "argvHash": "sha256:4af47b5e027e1686b124d8c7f986fe44a60b0b8a73c9cf1a32a8d8a592b39b48",
        "assertions": {
            "/schema": "ee.response.v2",
            "/success": True,
            "/data/command": "version",
            "/data/schema": "ee.version.provenance.v1",
            "/data/source/gitCommit": report["sourceCommit"],
            "/data/source/gitDirty": True,
            "/data/source/state": "dirty",
            "/data/build/targetTriple": report["target"],
            "/data/build/profile": report["profile"],
            "/data/provenance/available": True,
        },
    },
    "environment_attestation_help": {
        "argvHash": "sha256:37fa79e205cce2dafd8ac4da2075f73f74ed03beeca38cc6d741fcf54be21558",
        "assertions": {},
    },
}
for probe_id, expected in required_probes.items():
    probe = probes_by_id.get(probe_id)
    if probe is None:
        raise SystemExit(
            f"remote artifact verification report is missing required probe {probe_id}"
        )
    if probe["argvHash"] != expected["argvHash"]:
        raise SystemExit(
            f"remote artifact verification report has invalid argvHash for {probe_id}"
        )
    assertions = {
        assertion["path"]: assertion
        for assertion in probe["semanticAssertions"]
    }
    if probe_id == "environment_attestation_help" and assertions:
        raise SystemExit(
            "remote artifact verification report help probe has unexpected assertions"
        )
    for path, expected_value in expected["assertions"].items():
        assertion = assertions.get(path)
        if (
            assertion is None
            or assertion["expected"] != expected_value
            or assertion["observed"] != expected_value
            or assertion["matched"] is not True
        ):
            raise SystemExit(
                f"remote artifact verification report lacks required assertion {path}"
            )

verification_body = dict(report)
claimed_verification_hash = verification_body.pop("verificationHash")
canonical_body = json.dumps(
    verification_body,
    ensure_ascii=False,
    sort_keys=True,
    separators=(",", ":"),
).encode("utf-8")
expected_verification_hash = "sha256:" + hashlib.sha256(canonical_body).hexdigest()
if claimed_verification_hash != expected_verification_hash:
    raise SystemExit("remote artifact verification report self-hash does not match")

values = [
    json.dumps(report, ensure_ascii=False, sort_keys=True, separators=(",", ":")),
    report["binaryHash"],
    report["gitTree"],
    report["sourceCommit"],
    report["manifestHash"],
    report["buildCommandHash"],
    report["effectiveInputHash"],
    report["provenanceHash"],
    report["archiveHash"],
    report["verificationHash"],
]
sys.stdout.write("\x1e".join(values))
PYEOF
}

# Emit a single JSON-line event. Uses python3 for JSON encoding so embedded
# quotes/newlines/UTF-8 are handled correctly. No-op when log path unset.
_e2e_emit_event() {
    [ -z "${EE_TEST_LOG_PATH:-}" ] && return 0
    # Filter by level: quiet drops everything except command_end / assert_fail /
    # golden_compare; normal drops timer_lap; verbose keeps all.
    local kind="$1"
    case "$EE_TEST_LOG_LEVEL" in
        quiet)
            case "$kind" in command_end|assert_fail|golden_compare|artifact_manifest|lint_determinism|proptest_run) :;; *) return 0;; esac ;;
        normal)
            case "$kind" in timer_lap) return 0;; esac ;;
    esac
    shift
    local json_args=()
    while [ $# -gt 0 ]; do
        json_args+=("$1" "$2")
        shift 2
    done
    python3 - "$EE_TEST_LOG_PATH" "$EE_TEST_LOG_SCHEMA" "$(_e2e_now_iso)" "$EE_TEST_LOG_TEST_ID" "$kind" "${json_args[@]}" <<'PYEOF'
import json, sys, os
log_path = sys.argv[1]
event = {
    "schema": sys.argv[2],
    "ts": sys.argv[3],
    "test_id": sys.argv[4],
    "kind": sys.argv[5],
}
fields = {}
i = 6
while i + 1 < len(sys.argv):
    k = sys.argv[i]
    v = sys.argv[i+1]
    # Top-level columns vs free-form fields.
    if k in ("command", "stdin_hash", "stdout_hash", "stderr_hash", "stderr_excerpt"):
        event[k] = v
    elif k == "exit_code":
        try: event[k] = int(v)
        except ValueError: pass
    elif k == "elapsed_ms":
        try: event[k] = float(v)
        except ValueError: pass
    elif k == "args":
        # Comma-separated arg list -> JSON array
        event[k] = [s for s in v.split("") if s != ""]
    elif k == "remote_artifact_attestation_json":
        try:
            attestation = json.loads(v)
        except json.JSONDecodeError as error:
            raise SystemExit(f"remote artifact attestation is invalid JSON: {error}")
        if not isinstance(attestation, dict):
            raise SystemExit("remote artifact attestation must be a JSON object")
        fields["remote_artifact_attestation"] = attestation
    else:
        fields[k] = v
    i += 2
if fields:
    event["fields"] = fields
os.makedirs(os.path.dirname(log_path) or ".", exist_ok=True)
with open(log_path, "a", encoding="utf-8") as f:
    f.write(json.dumps(event) + "\n")
PYEOF
}

_e2e_tmp_root() {
    printf '%s\n' "${EE_E2E_ARTIFACT_TMPDIR:-${EE_E2E_TMPDIR:-${TMPDIR:-/tmp}}}"
}

_e2e_mktemp_file() {
    local label="${1:-artifact}"
    local root
    root="$(_e2e_tmp_root)"
    mkdir -p "$root"
    mktemp "${root%/}/ee-e2e-${label}.XXXXXX"
}

# ============================================================================
# Public API
# ============================================================================

# Start a test scenario. Sets test_id + opens the log file.
# Usage: e2e_log_start <test_id> [log_path]
e2e_log_start() {
    EE_TEST_LOG_TEST_ID="${1:?test_id required}"
    if [ -n "${2:-}" ]; then
        export EE_TEST_LOG_PATH="$2"
    elif [ -z "${EE_TEST_LOG_PATH:-}" ]; then
        export EE_TEST_LOG_PATH="${TMPDIR:-/tmp}/ee-test-log.jsonl"
    fi
    EE_TEST_LOG_ASSERTS_PASS=0
    EE_TEST_LOG_ASSERTS_FAIL=0
    _e2e_emit_event "note" "message" "test_start: $EE_TEST_LOG_TEST_ID"
}

# Free-form note event.
# Usage: e2e_log_note "<message>"
e2e_log_note() {
    _e2e_emit_event "note" "message" "${1:-}"
}

# Emit a deterministic manifest for the artifact exercised by a verification
# command. Raw output stays out of the log; paths and hashes are enough for
# closeout tooling to locate retained evidence and detect binary confusion.
# Set EE_REMOTE_ARTIFACT_VERIFICATION_REPORT to the path of a complete
# ee.remote_build_artifact_manifest.verification.v1 report to emit the
# attested ee.test_artifact_manifest.v2 contract instead of the legacy v1.
# Usage: e2e_log_artifact_manifest <phase> <binary_path> [argv...]
e2e_log_artifact_manifest() {
    local phase="${1:-manual}"
    local binary_path="${2:-${EE_BINARY:-}}"
    shift 2 || true

    local args_str=""
    local arg
    for arg in "$@"; do
        if [ -z "$args_str" ]; then args_str="$arg"; else args_str="$args_str"$'\x01'"$arg"; fi
    done

    local binary_hash="unavailable"
    local binary_hash_status="missing"
    if [ -n "$binary_path" ] && [ -f "$binary_path" ]; then
        binary_hash="$(_e2e_hash_file "$binary_path")"
        binary_hash_status="available"
    elif [ -n "$binary_path" ]; then
        binary_hash_status="not_file"
    fi

    local command_hash source_hash manifest_hash execution_substrate host_name
    local manifest_schema="ee.test_artifact_manifest.v1"
    local remote_attestation_fields=()
    command_hash="$(_e2e_hash_string "$binary_path"$'\n'"$args_str")"
    source_hash="$(_e2e_source_hash)"
    execution_substrate="${EE_TEST_EXECUTION_SUBSTRATE:-local}"
    if [ -n "${RCH_WORKER_ID:-}${RCH_WORKER_HOST:-}" ]; then
        execution_substrate="rch"
    fi
    host_name="$(hostname 2>/dev/null || printf 'unknown')"
    manifest_hash="$(_e2e_hash_string "$phase"$'\n'"$binary_path"$'\n'"$binary_hash"$'\n'"$command_hash"$'\n'"${CARGO_TARGET_DIR:-}"$'\n'"${EE_E2E_FIXTURE_FILTER:-${EE_TEST_FILTER:-}}"$'\n'"${EPIC_RETENTION_MANIFEST:-${EE_E2E_RETENTION_MANIFEST:-}}")"

    # A full consumer-side verification report upgrades this event to v2. The
    # report, rather than the ambient checkout, becomes the source of build
    # provenance. The binary hash is independently recomputed here so a report
    # for different bytes can never be attached to the exercised command.
    if [ -n "${EE_REMOTE_ARTIFACT_VERIFICATION_REPORT:-}" ]; then
        local attestation_payload remote_attestation_json report_binary_hash
        local report_git_tree report_source_commit report_manifest_hash
        local report_build_command_hash report_effective_input_hash
        local report_provenance_hash report_archive_hash report_verification_hash
        if ! attestation_payload="$(_e2e_remote_artifact_attestation_fields "$EE_REMOTE_ARTIFACT_VERIFICATION_REPORT")"; then
            printf 'e2e artifact manifest: invalid EE_REMOTE_ARTIFACT_VERIFICATION_REPORT=%s\n' \
                "$EE_REMOTE_ARTIFACT_VERIFICATION_REPORT" >&2
            return 2
        fi
        IFS=$'\x1e' read -r \
            remote_attestation_json \
            report_binary_hash \
            report_git_tree \
            report_source_commit \
            report_manifest_hash \
            report_build_command_hash \
            report_effective_input_hash \
            report_provenance_hash \
            report_archive_hash \
            report_verification_hash <<<"$attestation_payload"
        if [ -z "$binary_path" ] || [ ! -f "$binary_path" ]; then
            printf 'e2e artifact manifest: verified remote artifact binary is not a file: %s\n' \
                "$binary_path" >&2
            return 2
        fi
        local observed_binary_hash
        observed_binary_hash="$(_e2e_sha256_file "$binary_path")"
        if [ "$observed_binary_hash" != "$report_binary_hash" ]; then
            printf 'e2e artifact manifest: exercised binary hash does not match remote artifact verification report\n' >&2
            return 2
        fi

        manifest_schema="ee.test_artifact_manifest.v2"
        binary_hash="$report_binary_hash"
        binary_hash_status="available"
        source_hash="git_tree:$report_git_tree"
        manifest_hash="$report_manifest_hash"
        remote_attestation_fields=(
            "remote_artifact_attestation_json" "$remote_attestation_json"
            "source_commit" "$report_source_commit"
            "git_tree" "$report_git_tree"
            "build_command_hash" "$report_build_command_hash"
            "effective_input_hash" "$report_effective_input_hash"
            "provenance_hash" "$report_provenance_hash"
            "archive_hash" "$report_archive_hash"
            "verification_hash" "$report_verification_hash"
        )
    fi

    _e2e_emit_event "artifact_manifest" \
        "manifest_schema" "$manifest_schema" \
        "phase" "$phase" \
        "binary_path" "$binary_path" \
        "binary_hash" "$binary_hash" \
        "binary_hash_status" "$binary_hash_status" \
        "source_hash" "$source_hash" \
        "command_hash" "$command_hash" \
        "command_arg_count" "$#" \
        "execution_substrate" "$execution_substrate" \
        "local_host" "$host_name" \
        "worker_host" "${RCH_WORKER_HOST:-${RCH_WORKER_ID:-}}" \
        "target_directory" "${CARGO_TARGET_DIR:-}" \
        "fixture_filter" "${EE_E2E_FIXTURE_FILTER:-${EE_TEST_FILTER:-}}" \
        "log_path" "${EE_TEST_LOG_PATH:-}" \
        "retention_manifest_path" "${EPIC_RETENTION_MANIFEST:-${EE_E2E_RETENTION_MANIFEST:-}}" \
        "artifact_manifest_hash" "$manifest_hash" \
        "${remote_attestation_fields[@]}"
}

# Wrap a command: capture stdout/stderr/exit, emit start+end events, AND
# write stdout to a temp file so callers can use it after.
# Usage:  e2e_log_command "$EE" remember "hello" ...
# Prints stdout (so $(e2e_log_command ...) captures it). Exit code propagates.
e2e_log_command() {
    local label="${1:?command required}"
    local args_str=""
    local arg
    for arg in "$@"; do
        if [ -z "$args_str" ]; then args_str="$arg"; else args_str="$args_str"$'\x01'"$arg"; fi
    done
    _e2e_emit_event "command_start" "command" "$label" "args" "$args_str"
    local out_file err_file
    out_file=$(_e2e_mktemp_file stdout)
    err_file=$(_e2e_mktemp_file stderr)
    local started
    started=$(python3 -c "import time; print(time.monotonic_ns())")
    "$@" >"$out_file" 2>"$err_file"
    local rc=$?
    local ended
    ended=$(python3 -c "import time; print(time.monotonic_ns())")
    local elapsed_ms
    elapsed_ms=$(python3 -c "print(($ended - $started) / 1_000_000.0)")
    local stdout_hash stderr_hash stderr_excerpt
    stdout_hash=$(_e2e_hash_file "$out_file")
    stderr_hash=$(_e2e_hash_file "$err_file")
    stderr_excerpt=$(head -c "$EE_TEST_LOG_STDERR_CAP" "$err_file")
    _e2e_emit_event "command_end" \
        "command" "$label" \
        "args" "$args_str" \
        "stdout_hash" "$stdout_hash" \
        "stderr_hash" "$stderr_hash" \
        "stderr_excerpt" "$stderr_excerpt" \
        "exit_code" "$rc" \
        "elapsed_ms" "$elapsed_ms"
    e2e_log_artifact_manifest "command_end" "$label" "$@"
    cat "$out_file"
    if [ "${EE_E2E_KEEP_ARTIFACTS:-${EE_E2E_KEEP_WORKSPACE:-0}}" = "1" ]; then
        e2e_log_note "e2e_log_command_keep_artifacts stdout=$out_file stderr=$err_file"
    else
        rm -f "$out_file" "$err_file"
    fi
    return $rc
}

# Assert two strings equal. Emits assert_ok or assert_fail.
# Usage: e2e_log_assert_eq "$got" "$want" "label"
e2e_log_assert_eq() {
    local got="${1:-}"
    local want="${2:-}"
    local label="${3:?label required}"
    if [ "$got" = "$want" ]; then
        EE_TEST_LOG_ASSERTS_PASS=$((EE_TEST_LOG_ASSERTS_PASS + 1))
        _e2e_emit_event "assert_ok" "label" "$label"
    else
        EE_TEST_LOG_ASSERTS_FAIL=$((EE_TEST_LOG_ASSERTS_FAIL + 1))
        _e2e_emit_event "assert_fail" "label" "$label" "expected" "$want" "actual" "$got"
        return 1
    fi
}

# Assert numeric comparison. op ∈ {-le, -lt, -ge, -gt, -eq, -ne}.
# Usage: e2e_log_assert_num "$got" -le "$want" "label"
e2e_log_assert_num() {
    local got="${1:?got required}"
    local op="${2:?op required}"
    local want="${3:?want required}"
    local label="${4:?label required}"
    local matched="false"
    case "$op" in
        -le) [ "$got" -le "$want" ] 2>/dev/null && matched="true" ;;
        -lt) [ "$got" -lt "$want" ] 2>/dev/null && matched="true" ;;
        -ge) [ "$got" -ge "$want" ] 2>/dev/null && matched="true" ;;
        -gt) [ "$got" -gt "$want" ] 2>/dev/null && matched="true" ;;
        -eq) [ "$got" -eq "$want" ] 2>/dev/null && matched="true" ;;
        -ne) [ "$got" -ne "$want" ] 2>/dev/null && matched="true" ;;
        *) matched="false" ;;
    esac
    if [ "$matched" = "true" ]; then
        EE_TEST_LOG_ASSERTS_PASS=$((EE_TEST_LOG_ASSERTS_PASS + 1))
        _e2e_emit_event "assert_ok" "label" "$label"
    else
        EE_TEST_LOG_ASSERTS_FAIL=$((EE_TEST_LOG_ASSERTS_FAIL + 1))
        _e2e_emit_event "assert_fail" "label" "$label" "expected" "$op $want" "actual" "$got"
        return 1
    fi
}

# Golden compare two files. Emits golden_compare event with matched=true|false.
# Usage: e2e_log_golden_compare <generated> <expected> <name>
e2e_log_golden_compare() {
    local generated="${1:?generated path required}"
    local expected="${2:?expected path required}"
    local name="${3:?name required}"
    local matched="false"
    if diff -q "$generated" "$expected" >/dev/null 2>&1; then
        matched="true"
        EE_TEST_LOG_ASSERTS_PASS=$((EE_TEST_LOG_ASSERTS_PASS + 1))
    else
        EE_TEST_LOG_ASSERTS_FAIL=$((EE_TEST_LOG_ASSERTS_FAIL + 1))
    fi
    _e2e_emit_event "golden_compare" \
        "name" "$name" \
        "generated_path" "$generated" \
        "expected_path" "$expected" \
        "matched" "$matched"
    [ "$matched" = "true" ]
}

# Close the scenario. Writes a summary note and (if outer script wants) the
# pass/fail counters via globals.
e2e_log_end() {
    _e2e_emit_event "note" \
        "message" "test_end: $EE_TEST_LOG_TEST_ID" \
        "asserts_pass" "$EE_TEST_LOG_ASSERTS_PASS" \
        "asserts_fail" "$EE_TEST_LOG_ASSERTS_FAIL"
}
