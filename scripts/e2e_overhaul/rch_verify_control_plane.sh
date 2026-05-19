#!/usr/bin/env bash
# bd-1h8ji.6 — CI-safe RCH verification control-plane e2e driver.
#
# Default mode uses the verifier's deterministic fake-transcript hook so the
# e2e proves JSON/log contracts without starting an expensive remote build.
# Set RCH_VERIFY_CONTROL_PLANE_LONG_BENCH=1 to run the optional heavy bench lane.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
RCH_VERIFY="$REPO_ROOT/scripts/rch_verify.sh"
WORK_DIR="$(mktemp -d /tmp/ee-rch-control-plane.XXXXXX)"
EVENT_BEAD_ID="${EVENT_BEAD_ID:-bd-1h8ji.6}"

emit_event() {
    local phase="${1:?phase required}"
    local status="${2:?status required}"
    local elapsed_ms="${3:-0}"
    local command_hash="${4:-}"
    local worker_id="${5:-}"
    local degraded_codes_json="${6:-[]}"
    local note="${7:-}"
    local case_id="${8:-}"
    PHASE="$phase" \
    STATUS="$status" \
    ELAPSED_MS="$elapsed_ms" \
    COMMAND_HASH="$command_hash" \
    WORKER_ID="$worker_id" \
    DEGRADED_CODES_JSON="$degraded_codes_json" \
    NOTE="$note" \
    CASE_ID="$case_id" \
    EVENT_BEAD_ID="$EVENT_BEAD_ID" \
    python3 - <<'PY'
import json
import os

print(json.dumps({
    "schema": "ee.test_event.v1",
    "surface": "rch_verification_control_plane",
    "bead_id": os.environ["EVENT_BEAD_ID"],
    "case_id": os.environ["CASE_ID"],
    "phase": os.environ["PHASE"],
    "status": os.environ["STATUS"],
    "elapsed_ms": int(os.environ["ELAPSED_MS"]),
    "command_hash": os.environ["COMMAND_HASH"],
    "worker_id": os.environ["WORKER_ID"],
    "degraded_codes": json.loads(os.environ["DEGRADED_CODES_JSON"]),
    "note": os.environ["NOTE"],
}, sort_keys=True, separators=(",", ":")))
PY
}

assert_json() {
    local path="${1:?path required}"
    local expected_status="${2:?expected status required}"
    local expected_worker="${3:-}"
    python3 - "$path" "$expected_status" "$expected_worker" <<'PY'
import json
import sys

path, expected_status, expected_worker = sys.argv[1:4]
with open(path, encoding="utf-8") as handle:
    report = json.load(handle)
if report.get("schema") != "ee.rch.verify.v1":
    raise SystemExit(f"unexpected schema: {report}")
if report.get("status") != expected_status:
    raise SystemExit(f"expected status {expected_status}, got {report.get('status')}: {report}")
if expected_worker and report.get("worker_id") != expected_worker:
    raise SystemExit(f"expected worker {expected_worker}, got {report.get('worker_id')}: {report}")
invocation = report.get("rch_invocation") or []
if "exec" not in invocation:
    raise SystemExit(f"missing explicit rch exec invocation: {report}")
if "/Users/jemanuel" in json.dumps(report):
    raise SystemExit("private user path leaked into proof")
serialized = json.dumps(report).lower()
leak_markers = [f"{name}=" for name in ("token", "secret")]
if any(marker in serialized for marker in leak_markers):
    raise SystemExit("secret-shaped text leaked into proof")
print(json.dumps({
    "command_hash": report.get("command_hash", ""),
    "worker_id": report.get("worker_id") or "",
    "degraded_codes": report.get("degraded_codes") or [],
}, sort_keys=True, separators=(",", ":")))
PY
}

init_fixture_repo() {
    local repo="${1:?repo path required}"
    mkdir -p "$repo"
    git -C "$repo" init -q
    git -C "$repo" config user.name "RCH Verify E2E"
    git -C "$repo" config user.email "rch-verify-e2e@example.invalid"
    printf '%s\n' "seed" > "$repo/tracked.txt"
    printf '%s\n' "._*" > "$repo/.gitignore"
    git -C "$repo" add .gitignore tracked.txt
    git -C "$repo" commit -q -m "seed"
}

git_status_v2() {
    local repo="${1:?repo path required}"
    git -C "$repo" status --porcelain=v2 --untracked-files=all --ignored=no
}

assert_status_unchanged() {
    local repo="${1:?repo path required}"
    local before_file="${2:?before status path required}"
    local context="${3:?context required}"
    local after_file="$WORK_DIR/${context}.after-status"
    git_status_v2 "$repo" > "$after_file"
    if ! cmp -s "$before_file" "$after_file"; then
        printf 'git status changed for %s\nbefore:\n' "$context" >&2
        sed -n '1,120p' "$before_file" >&2
        printf 'after:\n' >&2
        sed -n '1,120p' "$after_file" >&2
        exit 1
    fi
}

write_fake_rch() {
    local path="${1:?fake rch path required}"
    cat > "$path" <<'FAKERCH'
#!/usr/bin/env bash
set -euo pipefail
case "${1:-}" in
  --version)
    printf 'rch 0.1.3\n'
    exit 0
    ;;
esac
printf '%s\n' "$*" >> "${FAKE_RCH_INVOCATIONS:?}"
if [ "${FAKE_RCH_MODE:-pass}" = "committed-tree" ]; then
  printf 'tracked=%s\n' "$(cat tracked.txt)"
  test ! -e token-draft.txt
fi
printf '[RCH] remote css (0.1s)\n'
FAKERCH
    chmod +x "$path"
}

assert_source_refusal_json() {
    local path="${1:?json path required}"
    local expected_staged="${2:?expected tracked staged count required}"
    local expected_unstaged="${3:?expected tracked unstaged count required}"
    local expected_untracked="${4:?expected untracked count required}"
    local expected_secret_risk="${5:?expected secret risk count required}"
    local expected_beads="${6:?expected beads metadata count required}"
    local expected_scratch="${7:?expected scratch artifact count required}"
    shift 7
    python3 - "$path" "$expected_staged" "$expected_unstaged" "$expected_untracked" "$expected_secret_risk" "$expected_beads" "$expected_scratch" "$@" <<'PY'
import json
import sys

path = sys.argv[1]
expected_staged = int(sys.argv[2])
expected_unstaged = int(sys.argv[3])
expected_untracked = int(sys.argv[4])
expected_secret_risk = int(sys.argv[5])
expected_beads = int(sys.argv[6])
expected_scratch = int(sys.argv[7])
required = {"rch_verify_dirty_tree_refused", *sys.argv[8:]}

with open(path, encoding="utf-8") as handle:
    report = json.load(handle)
codes = set(report.get("degraded_codes") or [])
source_codes = set(report.get("source_state_degraded_codes") or [])
if report.get("schema") != "ee.rch.verify.v1":
    raise SystemExit(f"unexpected schema: {report}")
if report.get("status") != "source_state_refused":
    raise SystemExit(f"expected source_state_refused: {report}")
if report.get("verification_attribution") != "live_dirty_checkout":
    raise SystemExit(f"expected live_dirty_checkout: {report}")
if report.get("exit_code") != 1:
    raise SystemExit(f"expected exit_code=1: {report}")
if report.get("rch_invocation") != []:
    raise SystemExit(f"source refusal must not plan RCH invocation: {report}")
summary = report.get("dirty_summary") or {}
if summary.get("tracked_staged") != expected_staged:
    raise SystemExit(f"tracked_staged drifted: {report}")
if summary.get("tracked_unstaged") != expected_unstaged:
    raise SystemExit(f"tracked_unstaged drifted: {report}")
if summary.get("untracked") != expected_untracked:
    raise SystemExit(f"untracked count drifted: {report}")
if summary.get("secret_risk") != expected_secret_risk:
    raise SystemExit(f"secret_risk count drifted: {report}")
if summary.get("beads") != expected_beads:
    raise SystemExit(f"beads count drifted: {report}")
if summary.get("scratch") != expected_scratch:
    raise SystemExit(f"scratch count drifted: {report}")
if not required.issubset(codes) or not required.issubset(source_codes):
    raise SystemExit(f"missing dirty-source degraded codes: {report}")
if expected_secret_risk:
    serialized = json.dumps(report)
    if "SYNTHETIC_TOKEN_VALUE" in serialized:
        raise SystemExit(f"secret-risk fixture leaked raw content: {report}")
    if not any(item.get("kind") == "secret_risk" for item in report.get("dirty_paths_sample") or []):
        raise SystemExit(f"secret-risk fixture did not tag path sample: {report}")
if expected_beads and not any(item.get("kind") == "beads" for item in report.get("dirty_paths_sample") or []):
    raise SystemExit(f"beads fixture did not tag path sample: {report}")
if expected_scratch and not any(item.get("kind") == "scratch" for item in report.get("dirty_paths_sample") or []):
    raise SystemExit(f"scratch fixture did not tag path sample: {report}")
print(json.dumps({
    "command_hash": report.get("command_hash", ""),
    "degraded_codes": sorted(codes),
}, sort_keys=True, separators=(",", ":")))
PY
}

assert_committed_tree_json() {
    local path="${1:?json path required}"
    python3 - "$path" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as handle:
    report = json.load(handle)
if report.get("schema") != "ee.rch.verify.v1":
    raise SystemExit(f"unexpected schema: {report}")
if report.get("status") != "remote_pass":
    raise SystemExit(f"expected remote_pass: {report}")
if report.get("verification_attribution") != "committed_tree":
    raise SystemExit(f"expected committed_tree attribution: {report}")
if not str(report.get("source_manifest_hash") or "").startswith("sha256:"):
    raise SystemExit(f"missing source manifest hash: {report}")
if report.get("dirty_summary", {}).get("total") != 0:
    raise SystemExit(f"committed-tree proof must exclude live dirty paths: {report}")
serialized = json.dumps(report)
if "SYNTHETIC_TOKEN_VALUE" in serialized or "token-draft" in serialized:
    raise SystemExit(f"committed-tree proof leaked live untracked token fixture: {report}")
tail = report.get("stdout_tail") or ""
if "tracked=seed" not in tail or "token-draft" in tail:
    raise SystemExit(f"committed-tree fake RCH did not run from clean export: {report}")
print(json.dumps({
    "command_hash": report.get("command_hash", ""),
    "degraded_codes": report.get("degraded_codes") or [],
}, sort_keys=True, separators=(",", ":")))
PY
}

started_ms() {
    python3 - <<'PY'
import time
print(int(time.time() * 1000))
PY
}

elapsed_since() {
    local start="${1:?start ms required}"
    python3 - "$start" <<'PY'
import sys
import time
print(max(0, int(time.time() * 1000) - int(sys.argv[1])))
PY
}

emit_event "setup" "ok" 0 "" "" "[]" "fixture work dir allocated without cleanup deletion"

start="$(started_ms)"
dry_run_json="$WORK_DIR/dry-run.json"
RCH_BIN="${RCH_BIN:-rch}" \
RCH_VERIFY_NOW="2026-05-16T06:40:00.000000Z" \
bash "$RCH_VERIFY" \
    --dry-run \
    --bead-id bd-1h8ji.6 \
    --summary \
    -- \
    cargo test --test rch_verify_contract -- --nocapture > "$dry_run_json"
dry_assert="$(assert_json "$dry_run_json" "dry_run" "")"
emit_event \
    "action" \
    "dry_run_proof_generated" \
    "$(elapsed_since "$start")" \
    "$(printf '%s' "$dry_assert" | python3 -c 'import json,sys; print(json.load(sys.stdin)["command_hash"])')" \
    "" \
    "$(printf '%s' "$dry_assert" | python3 -c 'import json,sys; print(json.dumps(json.load(sys.stdin)["degraded_codes"]))')" \
    "explicit rch exec proof rendered"

start="$(started_ms)"
fake_pass_json="$WORK_DIR/fake-pass.json"
RCH_BIN="${RCH_BIN:-rch}" \
RCH_VERIFY_NOW="2026-05-16T06:40:01.000000Z" \
RCH_VERIFY_FAKE_OUTPUT=$'running 1 test\ntest rch_control_plane ... ok\n[RCH] remote css (0.1s)\n' \
RCH_VERIFY_FAKE_EXIT_CODE=0 \
RCH_VERIFY_FAKE_ELAPSED_MS=100 \
bash "$RCH_VERIFY" \
    --bead-id bd-1h8ji.6 \
    --summary \
    -- \
    cargo test --test rch_verify_control_plane -- --nocapture > "$fake_pass_json"
pass_assert="$(assert_json "$fake_pass_json" "remote_pass" "css")"
emit_event \
    "assert" \
    "fake_remote_pass_validated" \
    "$(elapsed_since "$start")" \
    "$(printf '%s' "$pass_assert" | python3 -c 'import json,sys; print(json.load(sys.stdin)["command_hash"])')" \
    "css" \
    "[]" \
    "remote proof and summary contract validated"

start="$(started_ms)"
strict_repo="$WORK_DIR/strict-dirty-repo"
init_fixture_repo "$strict_repo"
printf '%s\n' "dirty live checkout" > "$strict_repo/tracked.txt"
strict_before="$WORK_DIR/strict-dirty.before-status"
git_status_v2 "$strict_repo" > "$strict_before"
strict_fake_rch="$WORK_DIR/fake-rch-strict-dirty"
strict_invocations="$WORK_DIR/strict-dirty-rch-invocations.txt"
strict_json="$WORK_DIR/strict-dirty-refusal.json"
strict_event_log="$WORK_DIR/strict-dirty-events.jsonl"
write_fake_rch "$strict_fake_rch"
set +e
FAKE_RCH_INVOCATIONS="$strict_invocations" \
RCH_VERIFY_NOW="2026-05-16T06:40:02.000000Z" \
RCH_VERIFY_CONFIGURED_WORKERS="css" \
RCH_VERIFY_DAEMON_WORKERS="css" \
RCH_VERIFY_STATUS_JSON='{"data":{"daemon":{"recent_builds":[]}}}' \
bash "$RCH_VERIFY" \
    --bead-id bd-9ygik.3 \
    --require-clean-tree \
    --project-root "$strict_repo" \
    --event-log "$strict_event_log" \
    --rch-bin "$strict_fake_rch" \
    -- \
    cargo test --lib rch_verify_strict_dirty_e2e > "$strict_json"
strict_exit=$?
set -e
if [ "$strict_exit" -eq 0 ]; then
    printf 'strict dirty fixture unexpectedly passed\n' >&2
    exit 1
fi
assert_status_unchanged "$strict_repo" "$strict_before" "strict-dirty"
if [ -s "$strict_invocations" ]; then
    printf 'strict dirty refusal invoked fake RCH:\n' >&2
    sed -n '1,120p' "$strict_invocations" >&2
    exit 1
fi
strict_assert="$(assert_source_refusal_json \
    "$strict_json" \
    0 \
    1 \
    0 \
    0 \
    0 \
    0 \
    rch_verify_dirty_tracked_paths \
    rch_verify_dirty_unstaged_paths)"
emit_event \
    "assert" \
    "strict_dirty_refusal_validated" \
    "$(elapsed_since "$start")" \
    "$(printf '%s' "$strict_assert" | python3 -c 'import json,sys; print(json.load(sys.stdin)["command_hash"])')" \
    "" \
    "$(printf '%s' "$strict_assert" | python3 -c 'import json,sys; print(json.dumps(json.load(sys.stdin)["degraded_codes"]))')" \
    "real git dirty fixture refused before fake RCH" \
    "strict_dirty_source"

start="$(started_ms)"
staged_repo="$WORK_DIR/staged-change-repo"
init_fixture_repo "$staged_repo"
printf '%s\n' "staged live checkout" > "$staged_repo/tracked.txt"
git -C "$staged_repo" add tracked.txt
staged_before="$WORK_DIR/staged-change.before-status"
git_status_v2 "$staged_repo" > "$staged_before"
staged_fake_rch="$WORK_DIR/fake-rch-staged-change"
staged_invocations="$WORK_DIR/staged-change-rch-invocations.txt"
staged_json="$WORK_DIR/staged-change-refusal.json"
staged_event_log="$WORK_DIR/staged-change-events.jsonl"
write_fake_rch "$staged_fake_rch"
set +e
FAKE_RCH_INVOCATIONS="$staged_invocations" \
RCH_VERIFY_NOW="2026-05-16T06:40:05.000000Z" \
RCH_VERIFY_CONFIGURED_WORKERS="css" \
RCH_VERIFY_DAEMON_WORKERS="css" \
RCH_VERIFY_STATUS_JSON='{"data":{"daemon":{"recent_builds":[]}}}' \
bash "$RCH_VERIFY" \
    --bead-id bd-9ygik.3 \
    --require-clean-tree \
    --project-root "$staged_repo" \
    --event-log "$staged_event_log" \
    --rch-bin "$staged_fake_rch" \
    -- \
    cargo test --lib rch_verify_staged_dirty_e2e > "$staged_json"
staged_exit=$?
set -e
if [ "$staged_exit" -eq 0 ]; then
    printf 'staged dirty fixture unexpectedly passed\n' >&2
    exit 1
fi
assert_status_unchanged "$staged_repo" "$staged_before" "staged-change"
if [ -s "$staged_invocations" ]; then
    printf 'staged dirty refusal invoked fake RCH:\n' >&2
    sed -n '1,120p' "$staged_invocations" >&2
    exit 1
fi
staged_assert="$(assert_source_refusal_json \
    "$staged_json" \
    1 \
    0 \
    0 \
    0 \
    0 \
    0 \
    rch_verify_dirty_tracked_paths \
    rch_verify_dirty_staged_paths)"
emit_event \
    "assert" \
    "staged_dirty_refusal_validated" \
    "$(elapsed_since "$start")" \
    "$(printf '%s' "$staged_assert" | python3 -c 'import json,sys; print(json.load(sys.stdin)["command_hash"])')" \
    "" \
    "$(printf '%s' "$staged_assert" | python3 -c 'import json,sys; print(json.dumps(json.load(sys.stdin)["degraded_codes"]))')" \
    "staged tracked fixture refused before fake RCH with staged-only counters" \
    "staged_change"

start="$(started_ms)"
secret_repo="$WORK_DIR/secret-risk-repo"
init_fixture_repo "$secret_repo"
printf '%s\n' "SYNTHETIC_TOKEN_VALUE" > "$secret_repo/token-draft.txt"
secret_before="$WORK_DIR/secret-risk.before-status"
git_status_v2 "$secret_repo" > "$secret_before"
secret_fake_rch="$WORK_DIR/fake-rch-secret-risk"
secret_invocations="$WORK_DIR/secret-risk-rch-invocations.txt"
secret_json="$WORK_DIR/secret-risk-refusal.json"
secret_event_log="$WORK_DIR/secret-risk-events.jsonl"
write_fake_rch "$secret_fake_rch"
set +e
FAKE_RCH_INVOCATIONS="$secret_invocations" \
RCH_VERIFY_NOW="2026-05-16T06:40:04.000000Z" \
RCH_VERIFY_CONFIGURED_WORKERS="css" \
RCH_VERIFY_DAEMON_WORKERS="css" \
RCH_VERIFY_STATUS_JSON='{"data":{"daemon":{"recent_builds":[]}}}' \
bash "$RCH_VERIFY" \
    --bead-id bd-9ygik.3 \
    --require-clean-tree \
    --project-root "$secret_repo" \
    --event-log "$secret_event_log" \
    --rch-bin "$secret_fake_rch" \
    -- \
    cargo test --lib rch_verify_secret_risk_e2e > "$secret_json"
secret_exit=$?
set -e
if [ "$secret_exit" -eq 0 ]; then
    printf 'secret-risk fixture unexpectedly passed\n' >&2
    exit 1
fi
assert_status_unchanged "$secret_repo" "$secret_before" "secret-risk"
if [ -s "$secret_invocations" ]; then
    printf 'secret-risk refusal invoked fake RCH:\n' >&2
    sed -n '1,120p' "$secret_invocations" >&2
    exit 1
fi
secret_assert="$(assert_source_refusal_json \
    "$secret_json" \
    0 \
    0 \
    0 \
    1 \
    0 \
    0 \
    rch_verify_dirty_untracked_paths)"
emit_event \
    "assert" \
    "secret_risk_refusal_validated" \
    "$(elapsed_since "$start")" \
    "$(printf '%s' "$secret_assert" | python3 -c 'import json,sys; print(json.load(sys.stdin)["command_hash"])')" \
    "" \
    "$(printf '%s' "$secret_assert" | python3 -c 'import json,sys; print(json.dumps(json.load(sys.stdin)["degraded_codes"]))')" \
    "secret-like untracked path refused before fake RCH without raw content leakage" \
    "secret_like_untracked"

start="$(started_ms)"
beads_repo="$WORK_DIR/beads-export-repo"
init_fixture_repo "$beads_repo"
mkdir -p "$beads_repo/.beads"
printf '%s\n' '{"id":"bd-fixture","status":"open"}' > "$beads_repo/.beads/issues.jsonl"
beads_before="$WORK_DIR/beads-export.before-status"
git_status_v2 "$beads_repo" > "$beads_before"
beads_fake_rch="$WORK_DIR/fake-rch-beads-export"
beads_invocations="$WORK_DIR/beads-export-rch-invocations.txt"
beads_json="$WORK_DIR/beads-export-refusal.json"
beads_event_log="$WORK_DIR/beads-export-events.jsonl"
write_fake_rch "$beads_fake_rch"
set +e
FAKE_RCH_INVOCATIONS="$beads_invocations" \
RCH_VERIFY_NOW="2026-05-16T06:40:06.000000Z" \
RCH_VERIFY_CONFIGURED_WORKERS="css" \
RCH_VERIFY_DAEMON_WORKERS="css" \
RCH_VERIFY_STATUS_JSON='{"data":{"daemon":{"recent_builds":[]}}}' \
bash "$RCH_VERIFY" \
    --bead-id bd-9ygik.3 \
    --require-clean-tree \
    --project-root "$beads_repo" \
    --event-log "$beads_event_log" \
    --rch-bin "$beads_fake_rch" \
    -- \
    cargo test --lib rch_verify_beads_export_e2e > "$beads_json"
beads_exit=$?
set -e
if [ "$beads_exit" -eq 0 ]; then
    printf 'beads export fixture unexpectedly passed\n' >&2
    exit 1
fi
assert_status_unchanged "$beads_repo" "$beads_before" "beads-export"
if [ -s "$beads_invocations" ]; then
    printf 'beads export refusal invoked fake RCH:\n' >&2
    sed -n '1,120p' "$beads_invocations" >&2
    exit 1
fi
beads_assert="$(assert_source_refusal_json \
    "$beads_json" \
    0 \
    0 \
    0 \
    0 \
    1 \
    0 \
    rch_verify_dirty_beads_metadata)"
emit_event \
    "assert" \
    "beads_export_refusal_validated" \
    "$(elapsed_since "$start")" \
    "$(printf '%s' "$beads_assert" | python3 -c 'import json,sys; print(json.load(sys.stdin)["command_hash"])')" \
    "" \
    "$(printf '%s' "$beads_assert" | python3 -c 'import json,sys; print(json.dumps(json.load(sys.stdin)["degraded_codes"]))')" \
    "beads metadata churn refused before fake RCH and kept out of source proof" \
    "beads_export_churn"

start="$(started_ms)"
scratch_repo="$WORK_DIR/scratch-artifacts-repo"
init_fixture_repo "$scratch_repo"
printf '%s\n' '{"warnings":[]}' > "$scratch_repo/ubs.json"
printf '%s\n' '{"drift":[]}' > "$scratch_repo/.plan-drift-report.json"
scratch_before="$WORK_DIR/scratch-artifacts.before-status"
git_status_v2 "$scratch_repo" > "$scratch_before"
scratch_fake_rch="$WORK_DIR/fake-rch-scratch-artifacts"
scratch_invocations="$WORK_DIR/scratch-artifacts-rch-invocations.txt"
scratch_json="$WORK_DIR/scratch-artifacts-refusal.json"
scratch_event_log="$WORK_DIR/scratch-artifacts-events.jsonl"
write_fake_rch "$scratch_fake_rch"
set +e
FAKE_RCH_INVOCATIONS="$scratch_invocations" \
RCH_VERIFY_NOW="2026-05-16T06:40:07.000000Z" \
RCH_VERIFY_CONFIGURED_WORKERS="css" \
RCH_VERIFY_DAEMON_WORKERS="css" \
RCH_VERIFY_STATUS_JSON='{"data":{"daemon":{"recent_builds":[]}}}' \
bash "$RCH_VERIFY" \
    --bead-id bd-9ygik.3 \
    --require-clean-tree \
    --project-root "$scratch_repo" \
    --event-log "$scratch_event_log" \
    --rch-bin "$scratch_fake_rch" \
    -- \
    cargo test --lib rch_verify_scratch_artifacts_e2e > "$scratch_json"
scratch_exit=$?
set -e
if [ "$scratch_exit" -eq 0 ]; then
    printf 'scratch artifacts fixture unexpectedly passed\n' >&2
    exit 1
fi
assert_status_unchanged "$scratch_repo" "$scratch_before" "scratch-artifacts"
if [ -s "$scratch_invocations" ]; then
    printf 'scratch artifacts refusal invoked fake RCH:\n' >&2
    sed -n '1,120p' "$scratch_invocations" >&2
    exit 1
fi
scratch_assert="$(assert_source_refusal_json \
    "$scratch_json" \
    0 \
    0 \
    0 \
    0 \
    0 \
    2 \
    rch_verify_dirty_untracked_scratch)"
emit_event \
    "assert" \
    "scratch_artifacts_refusal_validated" \
    "$(elapsed_since "$start")" \
    "$(printf '%s' "$scratch_assert" | python3 -c 'import json,sys; print(json.load(sys.stdin)["command_hash"])')" \
    "" \
    "$(printf '%s' "$scratch_assert" | python3 -c 'import json,sys; print(json.dumps(json.load(sys.stdin)["degraded_codes"]))')" \
    "scratch artifacts refused before fake RCH and kept out of source proof" \
    "scratch_artifacts"

start="$(started_ms)"
committed_repo="$WORK_DIR/committed-tree-repo"
init_fixture_repo "$committed_repo"
printf '%s\n' "dirty live checkout" > "$committed_repo/tracked.txt"
printf '%s\n' "SYNTHETIC_TOKEN_VALUE" > "$committed_repo/token-draft.txt"
committed_before="$WORK_DIR/committed-tree.before-status"
git_status_v2 "$committed_repo" > "$committed_before"
committed_fake_rch="$WORK_DIR/fake-rch-committed-tree"
committed_invocations="$WORK_DIR/committed-tree-rch-invocations.txt"
committed_json="$WORK_DIR/committed-tree.json"
committed_event_log="$WORK_DIR/committed-tree-events.jsonl"
write_fake_rch "$committed_fake_rch"
FAKE_RCH_INVOCATIONS="$committed_invocations" \
FAKE_RCH_MODE="committed-tree" \
RCH_VERIFY_NOW="2026-05-16T06:40:03.000000Z" \
RCH_VERIFY_CONFIGURED_WORKERS="css" \
RCH_VERIFY_DAEMON_WORKERS="css" \
RCH_VERIFY_STATUS_JSON='{"data":{"daemon":{"recent_builds":[]}}}' \
RCH_VERIFY_COMMITTED_TREE_BASE="$WORK_DIR/committed-tree-export" \
bash "$RCH_VERIFY" \
    --bead-id bd-9ygik.3 \
    --committed-tree \
    --treeish HEAD \
    --project-root "$committed_repo" \
    --event-log "$committed_event_log" \
    --rch-bin "$committed_fake_rch" \
    -- \
    cargo test --lib rch_verify_committed_tree_e2e > "$committed_json"
assert_status_unchanged "$committed_repo" "$committed_before" "committed-tree"
if [ "$(wc -l < "$committed_invocations" | tr -d ' ')" != "1" ]; then
    printf 'committed-tree fixture should invoke fake RCH once:\n' >&2
    sed -n '1,120p' "$committed_invocations" >&2
    exit 1
fi
committed_assert="$(assert_committed_tree_json "$committed_json")"
emit_event \
    "assert" \
    "committed_tree_export_validated" \
    "$(elapsed_since "$start")" \
    "$(printf '%s' "$committed_assert" | python3 -c 'import json,sys; print(json.load(sys.stdin)["command_hash"])')" \
    "css" \
    "$(printf '%s' "$committed_assert" | python3 -c 'import json,sys; print(json.dumps(json.load(sys.stdin)["degraded_codes"]))')" \
    "committed-tree mode ignored live dirty paths and ran fake RCH from export" \
    "committed_tree_ignores_dirty"

if [ "${RCH_VERIFY_CONTROL_PLANE_LONG_BENCH:-0}" = "1" ]; then
    start="$(started_ms)"
    bench_json="$WORK_DIR/optional-bench.json"
    RCH_BIN="${RCH_BIN:-rch}" \
    RCH_VERIFY_NOW="2026-05-16T06:40:06.000000Z" \
    bash "$RCH_VERIFY" \
        --bead-id bd-1h8ji.6 \
        --summary \
        -- \
        cargo bench --bench graph_minhash_rank > "$bench_json"
    bench_assert="$(assert_json "$bench_json" "remote_pass" "")"
    emit_event \
        "action" \
        "optional_bench_validated" \
        "$(elapsed_since "$start")" \
        "$(printf '%s' "$bench_assert" | python3 -c 'import json,sys; print(json.load(sys.stdin)["command_hash"])')" \
        "$(printf '%s' "$bench_assert" | python3 -c 'import json,sys; print(json.load(sys.stdin)["worker_id"])')" \
        "$(printf '%s' "$bench_assert" | python3 -c 'import json,sys; print(json.dumps(json.load(sys.stdin)["degraded_codes"]))')" \
        "optional heavy remote bench lane completed"
else
    emit_event "action" "optional_bench_skipped" 0 "" "" "[\"manual_heavy_strategy\"]" "set RCH_VERIFY_CONTROL_PLANE_LONG_BENCH=1 to run"
fi

emit_event "cleanup" "no_delete_by_policy" 0 "" "" "[]" "left temporary proof directory in /tmp"
emit_event "summary" "pass" 0 "" "" "[]" "rch verification control-plane e2e completed"
