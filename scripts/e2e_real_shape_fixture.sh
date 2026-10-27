#!/usr/bin/env bash
# bd-reality-core-convergence-1azkt.43 - real-shape CASS corpus end-to-end.
#
# "scripts/e2e_real_shape_fixture.sh imports through real cass and emits
# ee.test_event.v1 lines: sessions, spans, admitted/quarantined, durations."
#
# This is the whole chain on real tools: index the committed corpus with a REAL
# cass binary, import it through `ee import cass` with explicit trust, then
# assert the numbers the bead wants as a baseline. Nothing here uses
# scripts/real_shape_corpus/cass_fixture.py -- that emulator prefixes every line
# `stub` precisely so a run like this cannot be confused with one, and it is the
# right default for unit-level work and useless for this question.
#
# NOT WIRED INTO verify.sh, deliberately. The import alone takes minutes (308
# sessions, each span captured through a `cass view` subprocess) and the
# non-benchmark p50 budget has roughly 31s of headroom at 608s. A stage this
# size has to be an opt-in gate or a separate job, which is a budget decision
# for the operator, not something to smuggle in with a script. bd-2di61 made
# opt-in-flag gating a general budget exclusion if that is the chosen route.
#
# THREE GATES MUST BE ALIGNED OR THIS REPORTS SUCCESS HAVING IMPORTED NOTHING,
# and two of them fail silently. Each cost a dispatch to find:
#   1. TRUST. `ee import cass` refuses a cass outside its allowlist with exit 5
#      and a `cass_unavailable` degraded entry. There is no --trust flag; the
#      mechanism is EE_CASS_BINARY (one process) or `[cass] binary` in config.
#   2. --limit DEFAULTS TO 10. "Import every session" without it covers 10.
#   3. DISCOVERY FILTERS BY WORKSPACE. `sourceId` is
#      cass://sessions?workspace=...&limit=..., and every transcript records
#      `cwd: /work/ledger`, so the ee workspace path must EQUAL that cwd or
#      sessionsDiscovered is 0 with success:true.
#
# NOTE: no `set -e` -- the harness assert_* helpers accumulate failures and
# harness_summary decides the exit code.
set -uo pipefail

E2E_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/e2e_lib.sh
# shellcheck disable=SC1091
source "$E2E_DIR/e2e_lib.sh"

harness_init "real_shape_fixture"

CORPUS="$REPO_ROOT/tests/fixtures/real_shape_cass"
MANIFEST="$CORPUS/manifest.json"
JUDGMENTS="$CORPUS/judgments.json"

# The ee workspace path is NOT free: it must equal the cwd recorded in the
# transcripts, which the generator writes from its --workspace default.
CORPUS_CWD="$(jq -r '.sessions[0].path' "$MANIFEST" 2>/dev/null | sed -n 's#^home/\.claude/projects/-\([^/]*\)/.*#/\1#p' | tr '-' '/')"
WORKSPACE="${EE_REAL_SHAPE_WORKSPACE:-$CORPUS_CWD}"

step "preflight: corpus, judgments and a real cass"

if [ ! -f "$MANIFEST" ] || [ ! -f "$JUDGMENTS" ]; then
    log_drop 1 "corpus not present at $CORPUS; generate it with scripts/real_shape_corpus/generate.py"
    harness_summary
    exit "$?"
fi

EXPECTED_SESSIONS="$(jq -r '.sessions | length' "$MANIFEST")"
EXPECTED_RECORDS="$(jq -r '.total_records' "$MANIFEST")"
CASS_PINNED="$(jq -r '.claude_code_version' "$MANIFEST")"
e2e_log_note "real_shape_corpus sessions=$EXPECTED_SESSIONS records=$EXPECTED_RECORDS claude_code_version=$CASS_PINNED workspace=$WORKSPACE"

if ! command -v cass >/dev/null 2>&1; then
    # A DROP, not a failure: a host without cass cannot answer this question,
    # and pretending otherwise by reaching for the emulator is the exact
    # substitution this script exists to refuse.
    log_drop 1 "cass is not on PATH; the real-indexer arms cannot run and this script will NOT fall back to cass_fixture.py"
    harness_summary
    exit "$?"
fi
CASS_BIN="$(command -v cass)"
CASS_VERSION="$(cass --version 2>&1 | head -1 | awk '{print $NF}')"
e2e_log_note "real_cass binary=$CASS_BIN version=$CASS_VERSION"

DATA_DIR="$LOG_DIR/cass-data"
mkdir -p "$DATA_DIR" || { log_drop 1 "cannot create $DATA_DIR"; harness_summary; exit "$?"; }

step "index the corpus with real cass"

INDEX_START="$(_harness_now_ns)"
INDEX_JSON="$LOG_DIR/real-index.json"
if "$E2E_DIR/real_shape_corpus/index_with_real_cass.sh" \
        --corpus "$CORPUS" --data-dir "$DATA_DIR" --allow-version-drift \
        >"$INDEX_JSON" 2>"$LOG_DIR/real-index.err"; then
    INDEX_MS=$(( ( $(_harness_now_ns) - INDEX_START ) / 1000000 ))
    INDEX_PAYLOAD="$(cat "$INDEX_JSON")"
    e2e_log_note "real_cass_index elapsed_ms=$INDEX_MS payload=$INDEX_JSON"
    assert_jq "$INDEX_PAYLOAD" \
        "(.conversations == $EXPECTED_SESSIONS) and (.indexer == \"real\")" \
        "real cass indexed every session in the corpus"
    assert_jq "$INDEX_PAYLOAD" '.messages >= 3000' \
        "indexing through real cass yields at least 3k spans"
    assert_jq "$INDEX_PAYLOAD" '.hits > 0' \
        "the index answers a smoke query, so it is retrievable and not merely populated"
else
    INDEX_MS=$(( ( $(_harness_now_ns) - INDEX_START ) / 1000000 ))
    e2e_log_note "real_cass_index FAILED elapsed_ms=$INDEX_MS stderr=$LOG_DIR/real-index.err"
    assert_eq "index_failed" "index_ok" "real cass indexing must succeed before importing"
    harness_summary
    exit "$?"
fi

step "import through ee with explicit trust"

mkdir -p "$WORKSPACE" 2>/dev/null
if ! e2e_log_command "$EE_BIN" init --workspace "$WORKSPACE" >"$LOG_DIR/init.txt" 2>&1; then
    e2e_log_note "ee_init_nonzero workspace=$WORKSPACE (continuing: an existing workspace is fine)"
fi

IMPORT_START="$(_harness_now_ns)"
IMPORT_JSON="$LOG_DIR/import.json"
# EE_CASS_BINARY is the explicit trust. Without it this is exit 5 with
# cass_unavailable, which is the product behaving correctly, not a bug.
env HOME="$DATA_DIR/home" \
    XDG_CONFIG_HOME="$DATA_DIR/xdg/config" \
    XDG_DATA_HOME="$DATA_DIR/xdg/data" \
    XDG_CACHE_HOME="$DATA_DIR/xdg/cache" \
    XDG_STATE_HOME="$DATA_DIR/xdg/state" \
    EE_CASS_BINARY="$CASS_BIN" \
    "$EE_BIN" import cass --workspace "$WORKSPACE" \
        --limit "$(( EXPECTED_SESSIONS + 50 ))" --json \
        >"$IMPORT_JSON" 2>"$LOG_DIR/import.err"
IMPORT_RC=$?
IMPORT_MS=$(( ( $(_harness_now_ns) - IMPORT_START ) / 1000000 ))
IMPORT_PAYLOAD="$(cat "$IMPORT_JSON" 2>/dev/null)"
e2e_log_note "ee_import_cass rc=$IMPORT_RC elapsed_ms=$IMPORT_MS payload=$IMPORT_JSON"

# assert_eq, not assert_exit: assert_exit RUNS the command itself, and this rc
# comes from an invocation that had to carry a bespoke env block. Arg order is
# (actual, expected, label).
assert_eq "$IMPORT_RC" "0" "ee import cass succeeds with explicit trust via EE_CASS_BINARY"
assert_jq "$IMPORT_PAYLOAD" '.schema == "ee.response.v2" and .success == true' \
    "the import returns a success envelope"
assert_jq "$IMPORT_PAYLOAD" \
    "(.data.sessionsDiscovered == $EXPECTED_SESSIONS) and (.data.sessionsImported == $EXPECTED_SESSIONS) and (.data.sessionsSkipped == 0)" \
    "every session is discovered and imported, none skipped"
assert_jq "$IMPORT_PAYLOAD" "(.data.spansImported | type == \"number\") and (.data.spansImported >= 3000)" \
    "at least 3k spans are imported"

step "record the admitted and quarantined baseline"

# THE RECONCILIATION IS THE POINT. A quarantine total that does not add up with
# the admitted total against spansImported is how dropped records hide, so
# assert the sum rather than the parts.
assert_jq "$IMPORT_PAYLOAD" \
    '((.data.evidenceAdmission.admitted + .data.evidenceAdmission.quarantined) == .data.spansImported)' \
    "admitted plus quarantined reconciles exactly with spansImported"
assert_jq "$IMPORT_PAYLOAD" '.data.evidenceAdmission.admitted > 0' \
    "some evidence is admitted, so admission is not refusing everything"
assert_jq "$IMPORT_PAYLOAD" \
    '((.data.evidenceAdmission.quarantineReasons | keys | length) > 0)' \
    "quarantine decisions carry reasons rather than a bare count"

ADMITTED="$(printf '%s' "$IMPORT_PAYLOAD" | jq -r '.data.evidenceAdmission.admitted' 2>/dev/null)"
QUARANTINED="$(printf '%s' "$IMPORT_PAYLOAD" | jq -r '.data.evidenceAdmission.quarantined' 2>/dev/null)"
REASONS="$(printf '%s' "$IMPORT_PAYLOAD" | jq -c '.data.evidenceAdmission.quarantineReasons' 2>/dev/null)"
SPANS="$(printf '%s' "$IMPORT_PAYLOAD" | jq -r '.data.spansImported' 2>/dev/null)"
e2e_log_note "evidence_admission_baseline admitted=$ADMITTED quarantined=$QUARANTINED spans=$SPANS reasons=$REASONS"

step "judgments integrity against the manifest"

# Every label a judgment references must resolve to a manifest locator whose
# session file exists. This is the static half of "every referenced span id
# exists after import"; the post-import half needs a store query and is not
# claimed here.
# The manifest path goes in as argv, not as an exported variable: an env
# assignment on the assert line would have arrived after this pipeline had
# already run, and the python would have read an unset MANIFEST.
BROKEN="$(python3 - "$MANIFEST" "$JUDGMENTS" <<'PY'
import json, sys
labels = set(json.load(open(sys.argv[1]))["labels"])
jud = json.load(open(sys.argv[2]))
referenced = set()
for query in jud.get("retrieval") or []:
    referenced.update((query.get("relevant") or {}).keys())
    referenced.update(query.get("distractors") or [])
print(len(referenced - labels))
PY
)"
assert_eq "${BROKEN:-unknown}" "0" \
    "every judgment label resolves to a manifest locator"

harness_summary
