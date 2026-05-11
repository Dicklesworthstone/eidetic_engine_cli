#!/usr/bin/env bash
# Seed the 2026-05-10 reference corpus into a workspace.
# Bead bd-17c65.10.2 (J2). Used by every per-epic e2e script (J3).
#
# Usage:
#   corpus_2026_05_10_seed.sh <workspace-path>
#
# Env:
#   EE_BINARY              path to ee binary (default: target/release/ee)
#   EE_TEST_LOG_PATH       if set, emits J1 events for each ee remember
#   CORPUS_TOLERATE_REJECT if 1, continues past rejected memories without failing
#
# Exit codes:
#   0 — all 15 memories seeded successfully
#   1 — usage error
#   2 — workspace does not exist
#   3 — at least one memory failed to seed AND CORPUS_TOLERATE_REJECT != 1
#
# Behavior under pre-overhaul binary:
#   Memories 13 and 15 (cancellation token note, secret-policy rule) fail
#   due to the keyword secret detector (bd-17c65.3.1 / C1 fixes this).
#   Memories 9 and 10 (release decisions with v0.1.0/v0.2.0 tags) fail due
#   to the dot-in-tags validator (bd-17c65.3.3 / C3 fixes this).
#   This script reports the failures structurally (via J1 if log path set,
#   via stderr otherwise) so callers can detect "pre-overhaul behavior" vs
#   "fully fixed".

set -u
set -o pipefail

WORKSPACE="${1:-}"
if [ -z "$WORKSPACE" ]; then
    echo "usage: $0 <workspace-path>" >&2
    exit 1
fi
if [ ! -d "$WORKSPACE" ]; then
    echo "workspace not found: $WORKSPACE" >&2
    exit 2
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../../.." && pwd)"
EE_BINARY="${EE_BINARY:-$REPO_ROOT/target/release/ee}"

if [ ! -x "$EE_BINARY" ]; then
    echo "ee binary not executable at $EE_BINARY" >&2
    echo "set EE_BINARY or run: cargo build --release" >&2
    exit 2
fi

# Source the J1 logger if available so per-memory events are recorded.
if [ -f "$REPO_ROOT/scripts/lib/e2e_logger.sh" ]; then
    # shellcheck disable=SC1091
    source "$REPO_ROOT/scripts/lib/e2e_logger.sh"
    e2e_log_start "corpus_seed_2026_05_10"
    LOGGING=1
else
    LOGGING=0
fi

CORPUS_FILE="$SCRIPT_DIR/corpus_2026_05_10.jsonl"
if [ ! -f "$CORPUS_FILE" ]; then
    echo "corpus file missing: $CORPUS_FILE" >&2
    exit 2
fi

SEEDED=0
REJECTED=0
LINE_NO=0

# Read corpus, one memory per line, invoke ee remember.
while IFS= read -r line; do
    LINE_NO=$((LINE_NO + 1))
    [ -z "$line" ] && continue

    # Parse the JSONL record via python3 (jq may not be installed).
    parsed=$(python3 - "$line" <<'PYEOF'
import json, sys, shlex
record = json.loads(sys.argv[1])
content = record["content"]
level = record["level"]
kind = record["kind"]
tags = ",".join(record.get("tags", []))
confidence = str(record.get("confidence", 0.8))
print(shlex.quote(content))
print(level)
print(kind)
print(tags)
print(confidence)
PYEOF
    )
    content=$(echo "$parsed" | sed -n '1p')
    level=$(echo "$parsed" | sed -n '2p')
    kind=$(echo "$parsed" | sed -n '3p')
    tags=$(echo "$parsed" | sed -n '4p')
    confidence=$(echo "$parsed" | sed -n '5p')

    cmd=("$EE_BINARY" remember "$(eval echo "$content")" --workspace "$WORKSPACE" \
        --level "$level" --kind "$kind" --confidence "$confidence" --json)
    if [ -n "$tags" ]; then
        cmd+=(--tags "$tags")
    fi

    # Run remember and capture exit + body.
    body=$("${cmd[@]}" 2>&1)
    rc=$?

    if [ $rc -eq 0 ]; then
        success=$(printf '%s' "$body" | python3 -c "import json,sys; print(json.load(sys.stdin).get('success', False))" 2>/dev/null || echo "False")
    else
        success="False"
    fi

    if [ "$success" = "True" ]; then
        SEEDED=$((SEEDED + 1))
        [ "$LOGGING" = "1" ] && e2e_log_assert_eq "True" "True" "seed_line_${LINE_NO}_ok"
    else
        REJECTED=$((REJECTED + 1))
        err_code=$(printf '%s' "$body" | python3 -c "import json,sys; d=json.loads(sys.stdin.read()); print(d.get('error',{}).get('code','unknown'))" 2>/dev/null || echo "parse_error")
        err_msg=$(printf '%s' "$body" | python3 -c "import json,sys; d=json.loads(sys.stdin.read()); print(d.get('error',{}).get('message',''))" 2>/dev/null || echo "")
        if [ "$LOGGING" = "1" ]; then
            e2e_log_note "seed_line_${LINE_NO}_rejected: code=$err_code msg=$err_msg"
        else
            echo "[line $LINE_NO rejected] code=$err_code msg=$err_msg" >&2
        fi
    fi
done < "$CORPUS_FILE"

if [ "$LOGGING" = "1" ]; then
    e2e_log_note "corpus_seed_summary: seeded=$SEEDED rejected=$REJECTED total=$LINE_NO"
    e2e_log_end
fi

echo "[corpus_seed] seeded=$SEEDED  rejected=$REJECTED  total=$LINE_NO"

if [ "$REJECTED" -gt 0 ] && [ "${CORPUS_TOLERATE_REJECT:-0}" != "1" ]; then
    echo "[corpus_seed] some memories rejected; pass CORPUS_TOLERATE_REJECT=1 to ignore" >&2
    exit 3
fi
exit 0
