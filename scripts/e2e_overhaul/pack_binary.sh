#!/usr/bin/env bash
# N1 - binary context pack smoke harness.

set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/e2e_overhaul/lib/shared.sh
source "$SCRIPT_DIR/lib/shared.sh"

require_jq
START_SECONDS="$(python3 -c 'import time; print(time.monotonic())')"
epic_setup "pack_binary"

ee_workspace remember \
    --level procedural \
    --kind rule \
    "Binary pack smoke item alpha should be retrievable by the binary pack harness." \
    --json >/dev/null 2>&1

PACK_ONE="$EPIC_WORKSPACE/pack_binary_one.eepk"
PACK_TWO="$EPIC_WORKSPACE/pack_binary_two.eepk"

"$EE_BINARY" --format binary context "binary pack smoke item alpha" \
    --workspace "$EPIC_WORKSPACE" \
    --max-tokens 600 \
    --output "$PACK_ONE" >/dev/null 2>&1
"$EE_BINARY" --format binary context "binary pack smoke item alpha" \
    --workspace "$EPIC_WORKSPACE" \
    --max-tokens 600 \
    --output "$PACK_TWO" >/dev/null 2>&1

if cmp -s "$PACK_ONE" "$PACK_TWO"; then
    e2e_log_assert_eq "identical" "identical" "pack_binary_deterministic_bytes"
else
    e2e_log_assert_eq "different" "identical" "pack_binary_deterministic_bytes" || true
fi

READER_JSON="$(python3 "$REPO_ROOT/examples/pack_binary_reader.py" "$PACK_ONE" --item 0)"
assert_jq "$READER_JSON" ".schema" "ee.pack.bin.v1" "pack_binary_reader_schema" || true
assert_jq "$READER_JSON" ".version" "1" "pack_binary_reader_version" || true
assert_jq "$READER_JSON" ".items > 0" "true" "pack_binary_reader_nonempty_items" || true
assert_jq_nonempty "$READER_JSON" ".slices[0].content" "pack_binary_reader_zero_copy_slice" || true

ELAPSED_MS="$(python3 -c "import time; print(int((time.monotonic() - float('$START_SECONDS')) * 1000))")"
e2e_log_note "pack_binary_summary passed=${EE_TEST_LOG_ASSERTS_PASS} failed=${EE_TEST_LOG_ASSERTS_FAIL} elapsed_ms=${ELAPSED_MS}"

if [ "${EE_TEST_LOG_ASSERTS_FAIL:-0}" -gt 0 ]; then
    exit 1
fi
