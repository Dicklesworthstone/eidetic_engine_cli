#!/usr/bin/env bash
# bd-16imy — the chain this project's README promises and nothing proved:
# import a CASS session, then retrieve and PACK a phrase that exists only
# inside the imported transcript. Real `ee` binary, no mocks of the system
# under test.
#
# WHY THIS SCRIPT EXISTS. bd-16imy's acceptance asks for "a public no-mock
# E2E: fake/real CASS fixture -> import -> process/rebuild -> search unique
# transcript phrase -> pack -> why/provenance assertions". The machinery is
# all present -- `evidence_span_to_document` (src/search/mod.rs:1097), the
# bounded evidence visitors feeding both collection paths, and the direct
# evidence pack lane (`direct_evidence_pack_candidate`,
# src/core/context.rs:14943) which hydrates an UNDISTILLED span with real
# provenance and no synthetic MemoryId. What was missing was a run proving
# the pieces compose. The closest existing coverage,
# tests/e2e_appendix_c_agent_flow.rs, imports a fixture session and then
# asserts that search returns content the test REMEMBERED itself, never a
# phrase that exists only in the transcript.
#
# WHAT THE FAKE IS, AND WHAT IT IS NOT. The fake binary here is `cass`, the
# UPSTREAM tool whose robot/JSON contract `ee` consumes -- AGENTS.md requires
# that contract to be pinned by fixtures rather than depending on a live cass.
# Everything under test is the real `ee`: its import, its index, its search and
# its packer. No part of `ee` is stubbed.
#
# SCOPE, stated rather than implied: this proves the chain for a fixture
# session whose shape matches the pinned cass contract. It says nothing about
# real-world transcript variety, and nothing about embedding quality -- the
# phrase is lexically distinctive on purpose and EE_EMBED_DOWNLOAD is off, so
# a pass here is a retrieval-plumbing result, not a ranking-quality one.
#
# Environment:
#   EE_BIN / EE_BINARY  Path to the ee binary. With neither set, the binary
#                       built from this source tree is preferred over PATH.
#   EE_E2E_TMPDIR       Temp base (default /private/tmp)

set -uo pipefail

TEST_ID="imported_transcript_retrieval_e2e"
FAILURES=0
STEP=0

if ! command -v jq >/dev/null 2>&1; then
    echo "jq missing" >&2
    exit 3
fi

E2E_SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "${E2E_SCRIPT_DIR}/.." && pwd)}"
# shellcheck source=scripts/lib/ee_binary_resolution.sh
# shellcheck disable=SC1091
source "${REPO_ROOT}/scripts/lib/ee_binary_resolution.sh"

# Resolve the binary built from THIS tree before PATH, paired with an
# executability probe: a shared Cargo target dir can hold a foreign-platform
# artifact that satisfies `[ -x ]` and then dies with `exec format error`
# (bd-z6g1s). An explicit EE_BIN/EE_BINARY still wins.
EE_BIN="$(ee_resolve_fixture_binary e2e_imported_transcript "${EE_BIN:-${EE_BINARY:-}}")"
if [[ "${EE_BIN}" == */* ]]; then
    REAL_EE="${EE_BIN}"
else
    REAL_EE="$(command -v "${EE_BIN}" 2>/dev/null || true)"
fi
if [[ -z "${REAL_EE}" || ! -x "${REAL_EE}" ]]; then
    echo "ee binary missing (set EE_BIN)" >&2
    exit 3
fi

# Staleness guard: this suite asserts on the direct-evidence pack lane, which
# is recent. An older binary would fail confusingly rather than honestly.
if ! ee_require_current_binary "${REAL_EE}" "e2e_imported_transcript"; then
    exit 3
fi

# shellcheck source=scripts/lib/e2e_tmproot.sh
# shellcheck disable=SC1091
source "${E2E_SCRIPT_DIR}/lib/e2e_tmproot.sh"
ROOT_BASE="$(e2e_temp_root)"
ROOT="$(mktemp -d "${ROOT_BASE%/}/ee-imported-transcript-e2e.XXXXXX")"
WS="${ROOT}/ws"
LOG_DIR="${ROOT}/logs"
EVENT_LOG="${LOG_DIR}/events.jsonl"
mkdir -p "${WS}" "${LOG_DIR}"
: >"${EVENT_LOG}"

# Keep the model download out of a verification run; the assertions below are
# lexical by design.
export EE_EMBED_DOWNLOAD="${EE_EMBED_DOWNLOAD:-off}"

event() {
    local label="$1" status="$2" diagnosis="${3:-}"
    jq -cn --arg test_id "${TEST_ID}" --arg label "${label}" --arg status "${status}" \
        --arg diagnosis "${diagnosis}" \
        '{schema:"ee.test_event.v1",test_id:$test_id,kind:"assert_result",fields:{label:$label,status:$status,first_failure_diagnosis:$diagnosis}}' \
        | tee -a "${EVENT_LOG}"
    if [[ "${status}" == "fail" ]]; then
        FAILURES=$((FAILURES + 1))
    fi
}

run_ee() {
    STEP=$((STEP + 1))
    local out="${LOG_DIR}/step${STEP}.stdout" err="${LOG_DIR}/step${STEP}.stderr"
    "${REAL_EE}" "$@" >"${out}" 2>"${err}"
    LAST_EXIT=$?
    LAST_STDOUT="${out}"
    LAST_STDERR="${err}"
    return 0
}

# The phrase under test exists ONLY in the transcript below. It is never
# written through `ee remember`, which is the whole point: a pass means
# imported content became retrievable, not that an explicit memory did.
TRANSCRIPT_PHRASE="the retry budget collapsed because the backoff jitter seed was shared across shards"
# The planted negative. A suite that returned imported evidence for ANY query
# would satisfy the positive assertion while proving nothing, so this phrase
# -- absent from the transcript and from the store -- must NOT surface it.
ABSENT_PHRASE="marsupial telemetry quorum rebalancing"

# `ee` rejects any EE_CASS_BINARY whose file name is not exactly `cass`.
CASS_DIR="${ROOT}/fakebin"
CASS_BIN="${CASS_DIR}/cass"
SESSION_PATH="${ROOT}/cass-session-transcript.jsonl"
mkdir -p "${CASS_DIR}"
: >"${SESSION_PATH}"

cat >"${CASS_BIN}" <<FAKECASS
#!/bin/sh
set -eu
cmd="\${1:-}"
case "\$cmd" in
  sessions)
    printf '%s' '{"sessions":['
    printf '%s' '{"path":"${SESSION_PATH}","workspace":"${WS}","agent":"codex","started_at":"2026-06-18T09:00:00Z","ended_at":"2026-06-18T09:25:00Z","message_count":5,"token_count":480,"content_hash":"blake3:imported-transcript-e2e-session"}'
    printf '%s\n' ']}'
    ;;
  view)
    source_path=""
    for arg in "\$@"; do source_path="\$arg"; done
    if [ "\$source_path" = "${SESSION_PATH}" ]; then
      cat <<'JSON'
{"path":"SESSION_PATH_PLACEHOLDER","target_line":2,"context":3,"lines":[
  {"line":1,"content":"{\"role\":\"user\",\"content\":\"The nightly shard rebalance kept stalling after the third retry.\"}","highlighted":false},
  {"line":2,"content":"{\"role\":\"assistant\",\"content\":\"Root cause: the retry budget collapsed because the backoff jitter seed was shared across shards.\"}","highlighted":true},
  {"line":3,"content":"{\"role\":\"assistant\",\"content\":\"Fix: derive the jitter seed per shard from the shard id before scheduling any retry.\"}","highlighted":false},
  {"line":4,"content":"{\"role\":\"user\",\"content\":\"Verified: the rebalance completed in one pass once the per-shard seed landed.\"}","highlighted":false},
  {"line":5,"content":"{\"role\":\"user\",\"content\":\"Nothing sensitive in this transcript.\"}","highlighted":false}
],"total_lines":5}
JSON
    else
      echo "unexpected cass view path: \$source_path" >&2
      exit 64
    fi
    ;;
  *)
    echo "unexpected cass command: \$cmd" >&2
    exit 64
    ;;
esac
FAKECASS
# The heredoc above is quoted so the transcript JSON survives verbatim; put the
# real session path into the one line that needs it.
sed -i.bak "s#SESSION_PATH_PLACEHOLDER#${SESSION_PATH}#" "${CASS_BIN}" && rm -f "${CASS_BIN}.bak"
chmod 755 "${CASS_BIN}"

# --- 1. workspace ---------------------------------------------------------
run_ee init --workspace "${WS}" --json
if [[ "${LAST_EXIT}" -eq 0 ]]; then
    event init_ok pass
else
    event init_ok fail "init exit ${LAST_EXIT}: $(head -c 200 "${LAST_STDOUT}")"
fi

# --- 2. import the fixture session ----------------------------------------
EE_CASS_BINARY="${CASS_BIN}" run_ee --workspace "${WS}" import cass --limit 1 --json
IMPORT_OUT="${LAST_STDOUT}"
if [[ "${LAST_EXIT}" -eq 0 ]] \
    && jq -e '(.data.sessionsImported // 0) >= 1 and (.data.spansImported // 0) >= 1' \
        "${IMPORT_OUT}" >/dev/null 2>&1; then
    event import_persists_sessions_and_spans pass
else
    # stderr is included because an import that cannot reach the fake cass
    # fails there, not in the JSON envelope.
    event import_persists_sessions_and_spans fail "exit ${LAST_EXIT}; sessions=$(jq -c '.data.sessionsImported' "${IMPORT_OUT}" 2>/dev/null) spans=$(jq -c '.data.spansImported' "${IMPORT_OUT}" 2>/dev/null) degraded=$(jq -c '[(.degraded // [])[] | .code]' "${IMPORT_OUT}" 2>/dev/null) stderr=$(head -c 240 "${LAST_STDERR}" 2>/dev/null | tr '\n' ' ')"
fi

# --- 3. publish the derived index -----------------------------------------
run_ee index rebuild --workspace "${WS}" --json
if [[ "${LAST_EXIT}" -eq 0 ]]; then
    event index_rebuild_ok pass "evidence=$(jq -c '.data.evidenceIndexed // .data.evidence_indexed' "${LAST_STDOUT}" 2>/dev/null)"
else
    event index_rebuild_ok fail "index rebuild exit ${LAST_EXIT}: $(head -c 200 "${LAST_STDOUT}")"
fi

# --- 4. THE HEADLINE: a transcript-only phrase is retrievable -------------
run_ee search "${TRANSCRIPT_PHRASE}" --workspace "${WS}" --json
SEARCH_OUT="${LAST_STDOUT}"
EVIDENCE_HITS="$(jq -r '[(.data.results // [])[]? | select((.docId // "") | startswith("ev_"))] | length' "${SEARCH_OUT}" 2>/dev/null || echo 0)"
if [[ "${LAST_EXIT}" -eq 0 && "${EVIDENCE_HITS}" != "0" ]]; then
    event transcript_phrase_returns_imported_evidence pass "evidence_hits=${EVIDENCE_HITS}"
else
    event transcript_phrase_returns_imported_evidence fail "exit ${LAST_EXIT}; evidence_hits=${EVIDENCE_HITS}; docIds=$(jq -c '[(.data.results // [])[]? | .docId]' "${SEARCH_OUT}" 2>/dev/null); degraded=$(jq -c '[(.degraded // [])[] | .code]' "${SEARCH_OUT}" 2>/dev/null)"
fi

# --- 5. the planted negative ---------------------------------------------
run_ee search "${ABSENT_PHRASE}" --workspace "${WS}" --json
ABSENT_OUT="${LAST_STDOUT}"
ABSENT_HITS="$(jq -r '[(.data.results // [])[]? | select((.docId // "") | startswith("ev_"))] | length' "${ABSENT_OUT}" 2>/dev/null || echo -1)"
if [[ "${ABSENT_HITS}" == "0" ]]; then
    event absent_phrase_returns_no_evidence pass
else
    event absent_phrase_returns_no_evidence fail "a phrase absent from the transcript surfaced ${ABSENT_HITS} evidence hit(s), so the positive assertion above is not discriminating; docIds=$(jq -c '[(.data.results // [])[]? | .docId]' "${ABSENT_OUT}" 2>/dev/null)"
fi

# --- 6. pack hydration with provenance and no invented MemoryId ----------
run_ee pack "${TRANSCRIPT_PHRASE}" --workspace "${WS}" --json
PACK_OUT="${LAST_STDOUT}"
PACK_EVIDENCE="$(jq -r '[(.data.pack.items // [])[]? | select((.entityKind // "") == "evidence_span" or ((.evidenceSpanId // "") | startswith("ev_")))] | length' "${PACK_OUT}" 2>/dev/null || echo 0)"
if [[ "${LAST_EXIT}" -eq 0 && "${PACK_EVIDENCE}" != "0" ]]; then
    event pack_hydrates_imported_evidence pass "evidence_items=${PACK_EVIDENCE}"
else
    event pack_hydrates_imported_evidence fail "exit ${LAST_EXIT}; evidence_items=${PACK_EVIDENCE}; itemKinds=$(jq -c '[(.data.pack.items // [])[]? | {entityKind, evidenceSpanId, memoryId}]' "${PACK_OUT}" 2>/dev/null); degraded=$(jq -c '[(.degraded // [])[] | .code]' "${PACK_OUT}" 2>/dev/null)"
fi

# bd-16imy's acceptance says the evidence must hydrate "without inventing
# MemoryIds", and src/pack/binary.rs:872 pins that an evidence item carries no
# memoryId. Assert it rather than trusting it.
SYNTHETIC_IDS="$(jq -r '[(.data.pack.items // [])[]? | select((.entityKind // "") == "evidence_span") | select(has("memoryId"))] | length' "${PACK_OUT}" 2>/dev/null || echo -1)"
if [[ "${SYNTHETIC_IDS}" == "0" ]]; then
    event evidence_items_carry_no_synthetic_memory_id pass
else
    event evidence_items_carry_no_synthetic_memory_id fail "an evidence_span pack item carried a memoryId (count=${SYNTHETIC_IDS})"
fi

# Provenance has to NAME the stored transcript lines; a hydrated item with no
# traceable source would satisfy the count assertion while defeating the point.
PROVENANCE_OK="$(jq -r '[(.data.pack.items // [])[]? | select((.entityKind // "") == "evidence_span") | select(((.provenanceUri // .provenance.uri // "") | length) > 0)] | length' "${PACK_OUT}" 2>/dev/null || echo 0)"
if [[ "${PROVENANCE_OK}" != "0" ]]; then
    event evidence_items_carry_provenance pass "with_provenance=${PROVENANCE_OK}"
else
    event evidence_items_carry_provenance fail "no evidence_span item exposed a provenance uri; items=$(jq -c '[(.data.pack.items // [])[]? | select((.entityKind // "") == "evidence_span")]' "${PACK_OUT}" 2>/dev/null | head -c 400)"
fi

printf 'imported transcript retrieval e2e: %s steps, %s failures; root %s\n' \
    "${STEP}" "${FAILURES}" "${ROOT}" >&2
if [[ "${FAILURES}" -gt 0 ]]; then
    exit 2
fi
exit 0
