#!/usr/bin/env bash
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=tests/doctor_fixtures/lib.sh
. "$SCRIPT_DIR/../lib.sh"

# PINNED DEFECT bd-ixxzq, re-scoped to the LIVE holder (bd-2oh15 c9984/c9986).
# An orphan as named self-heals: the kernel releases a flock when its process
# dies. The failure that remains is a live process that holds
# .ee/ee.write.lock and makes no progress (a hung or stopped writer). This
# script builds the healthy store and the holder; assert.sh runs the holder
# for exactly as long as it measures, so no process outlives the fixture.
FM="fm-state_files-orphaned-pid-write-lock"
target="$(doctor_fixture_target)"
ee_bin="${EE_DOCTOR_FIXTURE_BINARY:-ee}"
command -v "$ee_bin" >/dev/null
command -v python3 >/dev/null
export EE_EMBED_DOWNLOAD="${EE_EMBED_DOWNLOAD:-off}"
doctor_fixture_prepare_target "$target"
doctor_fixture_healthy_store "$FM" "$target" "$ee_bin"
base="$target/.fixture_baseline"

# Provision the persistent doctor lock through a healthy no-op run, so the
# measured runs below change nothing that a baseline would compare.
"$ee_bin" doctor --workspace "$target" --fix --json > "$base/doctor-initialize-lock.json"
jq -e '.schema == "ee.response.v2" and .success == true and .data.actionCount == 0' \
    "$base/doctor-initialize-lock.json" >/dev/null
test -f "$target/.ee/ee.write.lock"

# The holder takes an exclusive flock(2) on the existing write lock (opened
# for reading, so no byte changes), signals readiness, and then does nothing.
cat > "$base/hold-write-lock.py" <<'PY'
import fcntl
import sys
import time

lock = open(sys.argv[1], "rb")
fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
open(sys.argv[2], "w").close()
time.sleep(float(sys.argv[3]))
PY
cat > "$base/probe-write-lock.py" <<'PY'
import fcntl
import sys

lock = open(sys.argv[1], "rb")
try:
    fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
except BlockingIOError:
    print("held")
    sys.exit(0)
print("free")
PY

# Negative control: with no holder the lock is free and doctor is healthy.
test "$(python3 "$base/probe-write-lock.py" "$target/.ee/ee.write.lock")" = free
"$ee_bin" doctor --workspace "$target" --json > "$base/doctor-unheld.json"
doctor_fixture_assert_health_report "$FM" "$base/doctor-unheld.json"

doctor_fixture_corrupt "$FM" "P1" "state_files"
printf 'live-holder trigger ready: %s holds .ee/ee.write.lock during assert.sh\n' "$base/hold-write-lock.py" >&2
