#!/usr/bin/env bash
# Index the real-shape CASS corpus with a REAL cass binary, in isolation.
#
# bd-reality-core-convergence-1azkt.43 deliverable 3: "Script how the fixture is
# indexed by an actual `cass` binary into a private CASS_DATA_DIR (pinned cass
# version recorded). Unit-level tests may keep the existing stub path, but must
# log 'stub'. R-gate (.50) must use real cass."
#
# WHY THIS EXISTS SEPARATELY FROM probe.py. probe.py serves the corpus through
# scripts/real_shape_corpus/cass_fixture.py, an emulator whose every line is
# prefixed `stub` so a gate cannot mistake it for the real thing. That is the
# right default for unit-level work and it proves nothing about whether real
# cass can READ these shapes. This script is the opposite trade: slower, needs a
# binary, and answers the only question the emulator cannot.
#
# WHAT IT PROVES, and it is the bead's central claim: that the AUTHORED
# transcripts are schema-faithful enough for the shipped tool. A corpus the
# generator validates against its own table but real cass silently skips would
# be a measuring stick measuring nothing.
#
# ISOLATION. cass discovers sessions from HOME (~/.claude, ~/.codex) and writes
# its database under XDG_DATA_HOME. Both are redirected into --data-dir, and the
# corpus is COPIED there first, because cass writes a raw mirror and we will not
# have it writing into a git-tracked fixture.
#
# NOTE: no `set -e`. Each phase reports and the script decides the exit code, so
# a failed assertion cannot stop the summary from being printed.
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

# The pin the bead asks to be RECORDED. A measuring stick read by a different
# indexer is a different measuring stick, so a mismatch is reported loudly and
# exits nonzero unless the caller opts out deliberately.
EXPECTED_CASS_VERSION="0.8.0"

CORPUS="$REPO_ROOT/tests/fixtures/real_shape_cass"
DATA_DIR=""
ALLOW_VERSION_DRIFT="false"
SMOKE_QUERY="cargo publish"

usage() {
    cat >&2 <<'USAGE'
usage: index_with_real_cass.sh --data-dir DIR [options]

  --data-dir DIR         Private root for the copied corpus and the cass index.
                         Required: it is written to, so it must not be the fixture.
  --corpus DIR           Corpus to index (default: tests/fixtures/real_shape_cass).
  --smoke-query STR      Query that must return >0 hits (default: "cargo publish").
  --allow-version-drift  Report a cass version other than the pin instead of failing.
USAGE
}

while [ $# -gt 0 ]; do
    case "$1" in
        --data-dir) DATA_DIR="${2:?--data-dir requires a path}"; shift 2 ;;
        --corpus) CORPUS="${2:?--corpus requires a path}"; shift 2 ;;
        --smoke-query) SMOKE_QUERY="${2:?--smoke-query requires a string}"; shift 2 ;;
        --allow-version-drift) ALLOW_VERSION_DRIFT="true"; shift ;;
        -h|--help) usage; exit 0 ;;
        *) printf 'index_with_real_cass: unknown argument %s\n' "$1" >&2; usage; exit 2 ;;
    esac
done

fail() { printf 'index_with_real_cass: %s\n' "$1" >&2; exit "${2:-1}"; }

[ -n "$DATA_DIR" ] || { usage; fail "--data-dir is required" 2; }
[ -d "$CORPUS/home" ] || fail "no corpus at $CORPUS (expected a home/ subtree)"
[ -f "$CORPUS/manifest.json" ] || fail "no manifest.json under $CORPUS"

# REFUSE RATHER THAN FALL BACK. A missing cass is exactly the condition that
# would otherwise be papered over by the emulator, which is the thing this
# script exists not to do.
command -v cass >/dev/null 2>&1 || fail "cass is not on PATH; this script will not fall back to the stub emulator" 3

CASS_VERSION="$(cass --version 2>&1 | head -1 | awk '{print $NF}')"
printf 'index_with_real_cass: cass_version=%s pinned=%s\n' "$CASS_VERSION" "$EXPECTED_CASS_VERSION" >&2
if [ "$CASS_VERSION" != "$EXPECTED_CASS_VERSION" ]; then
    if [ "$ALLOW_VERSION_DRIFT" = "true" ]; then
        printf 'index_with_real_cass: version drift ALLOWED by flag; measurements are not comparable to the pin\n' >&2
    else
        fail "cass $CASS_VERSION is not the pinned $EXPECTED_CASS_VERSION; re-pin deliberately or pass --allow-version-drift" 4
    fi
fi

mkdir -p "$DATA_DIR/home" "$DATA_DIR/xdg/config" "$DATA_DIR/xdg/data" \
         "$DATA_DIR/xdg/cache" "$DATA_DIR/xdg/state" \
    || fail "cannot create the private data dir under $DATA_DIR"

cp -R "$CORPUS/home/." "$DATA_DIR/home/" || fail "could not copy the corpus into $DATA_DIR"

EXPECTED_SESSIONS="$(python3 -c 'import json,sys; print(len(json.load(open(sys.argv[1]))["sessions"]))' "$CORPUS/manifest.json")"
COPIED_SESSIONS="$(find "$DATA_DIR/home" -name '*.jsonl' | wc -l | tr -d ' ')"
printf 'index_with_real_cass: sessions manifest=%s copied=%s\n' "$EXPECTED_SESSIONS" "$COPIED_SESSIONS" >&2
[ "$COPIED_SESSIONS" = "$EXPECTED_SESSIONS" ] \
    || fail "copied $COPIED_SESSIONS sessions but the manifest lists $EXPECTED_SESSIONS"

run_cass() {
    env HOME="$DATA_DIR/home" \
        XDG_CONFIG_HOME="$DATA_DIR/xdg/config" \
        XDG_DATA_HOME="$DATA_DIR/xdg/data" \
        XDG_CACHE_HOME="$DATA_DIR/xdg/cache" \
        XDG_STATE_HOME="$DATA_DIR/xdg/state" \
        cass "$@"
}

printf 'index_with_real_cass: indexing with REAL cass (not the stub emulator)\n' >&2
run_cass index --full >"$DATA_DIR/index.log" 2>&1
INDEX_RC=$?
if [ "$INDEX_RC" -ne 0 ]; then
    tail -20 "$DATA_DIR/index.log" >&2
    fail "cass index exited $INDEX_RC; see $DATA_DIR/index.log" 5
fi

# INGEST COUNTS ARE THE LOAD-BEARING ASSERTION. `index completed` with zero
# conversations processed is the silent-zero this corpus is built to expose, so
# read the counts rather than the exit code.
STATS_JSON="$(run_cass stats --json 2>/dev/null)"
CONVERSATIONS="$(printf '%s' "$STATS_JSON" | python3 -c 'import json,sys
try: d=json.load(sys.stdin)
except Exception: print(-1); raise SystemExit
for k in ("conversations","total_conversations"):
    if k in d: print(d[k]); raise SystemExit
t=d.get("totals") or {}
print(t.get("conversations", -1))' 2>/dev/null)"
MESSAGES="$(printf '%s' "$STATS_JSON" | python3 -c 'import json,sys
try: d=json.load(sys.stdin)
except Exception: print(-1); raise SystemExit
for k in ("messages","total_messages"):
    if k in d: print(d[k]); raise SystemExit
t=d.get("totals") or {}
print(t.get("messages", -1))' 2>/dev/null)"

printf 'index_with_real_cass: indexed conversations=%s messages=%s\n' "$CONVERSATIONS" "$MESSAGES" >&2
[ "$CONVERSATIONS" = "$EXPECTED_SESSIONS" ] \
    || fail "real cass indexed $CONVERSATIONS conversations, expected $EXPECTED_SESSIONS -- the corpus shapes are not being read" 6

# And it must be RETRIEVABLE, not merely ingested. An index that accepts every
# record and answers nothing is the other half of the same failure.
HITS="$(run_cass search "$SMOKE_QUERY" --json 2>/dev/null | python3 -c 'import json,sys
try: d=json.load(sys.stdin)
except Exception: print(0); raise SystemExit
print(d.get("total_matches", d.get("count", len(d.get("hits") or []))))' 2>/dev/null)"
printf 'index_with_real_cass: smoke query %s -> hits=%s\n' "$SMOKE_QUERY" "$HITS" >&2
[ "${HITS:-0}" -gt 0 ] 2>/dev/null \
    || fail "smoke query '$SMOKE_QUERY' returned $HITS hits from a populated index" 7

printf '{"schema":"ee.real_shape_cass.real_index.v1","cass_version":"%s","pinned_cass_version":"%s","corpus":"%s","data_dir":"%s","sessions":%s,"conversations":%s,"messages":%s,"smoke_query":"%s","hits":%s,"indexer":"real"}\n' \
    "$CASS_VERSION" "$EXPECTED_CASS_VERSION" "$CORPUS" "$DATA_DIR" \
    "$EXPECTED_SESSIONS" "$CONVERSATIONS" "$MESSAGES" "$SMOKE_QUERY" "$HITS"
