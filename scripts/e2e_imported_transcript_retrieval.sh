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
# Distinctive single words from each session, used to identify WHICH session a
# hit came from without needing span ids.
TARGET_MARKER="jitter"
DISTRACTOR_MARKER="descaling"
DISTRACTOR_PHRASE="the descaling cycle stalled because the boiler thermistor was reading in fahrenheit"
# Leak canary. bd-16imy's second acceptance bullet requires that denied or
# redacted material must not leak, and the first version of this fixture
# contained no secrets at all -- so every assertion could pass while leakage
# went unobserved. This token lives on line 5 of the DISTRACTOR transcript and
# must never appear in a search result or a pack item, including when the
# query is aimed straight at it.
SECRET_CANARY="sk-proj-transcriptleak-e2e-canary-000000000000000000"

# WHY TWO SESSIONS, AND WHY NOT AN ABSENCE ASSERTION.
#
# The first version of this script imported ONE session and asserted that a
# phrase absent from the transcript returned zero evidence hits. Measured
# 2026-10-14 on an RCH worker, that planted negative FAILED and saved the
# script from being vacuous: the target phrase returned 5 evidence hits and
# the absent phrase ("marsupial telemetry quorum rebalancing") returned the
# SAME 5, plus a session document. On a single-topic corpus this search
# returns everything, so "the transcript phrase returns imported evidence"
# would have passed for any string whatsoever.
#
# An absence assertion cannot be repaired by rewording, because the response
# is not wrong to offer low-ranked candidates. The discriminating question is
# whether retrieval puts the RIGHT span first. So the corpus now holds two
# unrelated sessions and the assertions are symmetric: each query must rank
# its own session's content at the top evidence hit. A search that returns
# everything in a fixed order fails one of the two by construction, which an
# absence check could never detect.
CASS_DIR="${ROOT}/fakebin"
CASS_BIN="${CASS_DIR}/cass"
SESSION_PATH="${ROOT}/cass-session-transcript.jsonl"
DISTRACTOR_PATH="${ROOT}/cass-session-distractor.jsonl"
mkdir -p "${CASS_DIR}"
: >"${SESSION_PATH}"
: >"${DISTRACTOR_PATH}"

cat >"${CASS_BIN}" <<FAKECASS
#!/bin/sh
set -eu
cmd="\${1:-}"
case "\$cmd" in
  sessions)
    printf '%s' '{"sessions":['
    printf '%s' '{"path":"${SESSION_PATH}","workspace":"${WS}","agent":"codex","started_at":"2026-06-18T09:00:00Z","ended_at":"2026-06-18T09:25:00Z","message_count":5,"token_count":480,"content_hash":"blake3:imported-transcript-e2e-session"}'
    printf '%s' ',{"path":"${DISTRACTOR_PATH}","workspace":"${WS}","agent":"claude","started_at":"2026-06-18T11:00:00Z","ended_at":"2026-06-18T11:20:00Z","message_count":5,"token_count":455,"content_hash":"blake3:imported-transcript-e2e-distractor"}'
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
    elif [ "\$source_path" = "${DISTRACTOR_PATH}" ]; then
      cat <<'JSON'
{"path":"DISTRACTOR_PATH_PLACEHOLDER","target_line":2,"context":3,"lines":[
  {"line":1,"content":"{\"role\":\"user\",\"content\":\"The cafe espresso machine stopped pulling a full cup this morning.\"}","highlighted":false},
  {"line":2,"content":"{\"role\":\"assistant\",\"content\":\"Root cause: the descaling cycle stalled because the boiler thermistor was reading in fahrenheit.\"}","highlighted":true},
  {"line":3,"content":"{\"role\":\"assistant\",\"content\":\"Fix: convert the thermistor reading to celsius before the descaling pump engages.\"}","highlighted":false},
  {"line":4,"content":"{\"role\":\"user\",\"content\":\"Verified: a full descaling cycle completed and the cup volume recovered.\"}","highlighted":false},
  {"line":5,"content":"{\"role\":\"user\",\"content\":\"The machine telemetry token is sk-proj-transcriptleak-e2e-canary-000000000000000000 so keep it out of any summary.\"}","highlighted":false}
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
# The inner heredocs are quoted so the transcript JSON survives verbatim; put
# the real session paths into the two lines that need them.
sed -i.bak -e "s#SESSION_PATH_PLACEHOLDER#${SESSION_PATH}#" \
    -e "s#DISTRACTOR_PATH_PLACEHOLDER#${DISTRACTOR_PATH}#" "${CASS_BIN}" \
    && rm -f "${CASS_BIN}.bak"
chmod 755 "${CASS_BIN}"

# --- 1. workspace ---------------------------------------------------------
run_ee init --workspace "${WS}" --json
if [[ "${LAST_EXIT}" -eq 0 ]]; then
    event init_ok pass
else
    event init_ok fail "init exit ${LAST_EXIT}: $(head -c 200 "${LAST_STDOUT}")"
fi

# --- 2. import both fixture sessions --------------------------------------
EE_CASS_BINARY="${CASS_BIN}" run_ee --workspace "${WS}" import cass --limit 2 --json
IMPORT_OUT="${LAST_STDOUT}"
if [[ "${LAST_EXIT}" -eq 0 ]] \
    && jq -e '(.data.sessionsImported // 0) >= 2 and (.data.spansImported // 0) >= 2' \
        "${IMPORT_OUT}" >/dev/null 2>&1; then
    event import_persists_sessions_and_spans pass
else
    # stderr is included because an import that cannot reach the fake cass
    # fails there, not in the JSON envelope.
    event import_persists_sessions_and_spans fail "exit ${LAST_EXIT}; sessions=$(jq -c '.data.sessionsImported' "${IMPORT_OUT}" 2>/dev/null) spans=$(jq -c '.data.spansImported' "${IMPORT_OUT}" 2>/dev/null) degraded=$(jq -c '[(.degraded // [])[] | .code]' "${IMPORT_OUT}" 2>/dev/null) stderr=$(head -c 240 "${LAST_STDERR}" 2>/dev/null | tr '\n' ' ')"
fi

SPANS_IMPORTED="$(jq -r '.data.spansImported // 0' "${IMPORT_OUT}" 2>/dev/null || echo 0)"

# --- 3. publish the derived index -----------------------------------------
run_ee index rebuild --workspace "${WS}" --json
INDEX_OUT="${LAST_STDOUT}"
EVIDENCE_INDEXED="$(jq -r '.data.evidenceIndexed // .data.evidence_indexed // 0' "${INDEX_OUT}" 2>/dev/null || echo 0)"
if [[ "${LAST_EXIT}" -eq 0 ]]; then
    event index_rebuild_ok pass "evidence=${EVIDENCE_INDEXED}"
else
    event index_rebuild_ok fail "index rebuild exit ${LAST_EXIT}: $(head -c 200 "${INDEX_OUT}")"
fi

# bd-16imy's FIFTH bullet: "Import/index job accounting and index generation
# truthfully cover the new documents." Until now this suite asserted the
# rebuild merely EXITED 0, which is not accounting.
#
# Measured on two dispatched runs before this assertion existed: a
# single-session corpus reported evidence=5 and the two-session corpus
# reported evidence=10, i.e. exactly one indexed document per imported span.
# So equality is the real claim, and a drift either way is informative --
# fewer means the index does not cover every span (the bullet fails), more
# means documents are being counted that no import produced.
#
# The canary span is deliberately included in this count: a secret-bearing
# line is WITHHELD at projection time (its content becomes a withheld marker)
# but it is still a document, so excluding it would be a different defect and
# this assertion would catch that too.
if [[ "${EVIDENCE_INDEXED}" == "${SPANS_IMPORTED}" && "${EVIDENCE_INDEXED}" != "0" ]]; then
    event index_accounting_covers_every_imported_span pass "spans=${SPANS_IMPORTED} indexed=${EVIDENCE_INDEXED}"
else
    event index_accounting_covers_every_imported_span fail "accounting does not cover the corpus: spansImported=${SPANS_IMPORTED} evidenceIndexed=${EVIDENCE_INDEXED}; import=$(jq -c '{sessionsImported: .data.sessionsImported, spansImported: .data.spansImported}' "${IMPORT_OUT}" 2>/dev/null); index=$(jq -c '{evidenceIndexed: (.data.evidenceIndexed // .data.evidence_indexed), generation: (.data.generation // .data.indexGeneration)}' "${INDEX_OUT}" 2>/dev/null)"
fi

# Content of the TOP-ranked evidence hit, which is what a discriminating
# retrieval claim actually rests on. Rank is read from result order, the same
# order a caller consumes.
top_evidence_content() {
    jq -r '[(.data.results // [])[]? | select((.docId // "") | startswith("ev_"))]
           | .[0] | ((.content // .contentPreview // .content_preview // "") | ascii_downcase)' \
        "$1" 2>/dev/null
}
evidence_hit_count() {
    jq -r '[(.data.results // [])[]? | select((.docId // "") | startswith("ev_"))] | length' \
        "$1" 2>/dev/null || echo 0
}

# --- 4. THE HEADLINE: the target phrase ranks ITS OWN span first -----------
run_ee search "${TRANSCRIPT_PHRASE}" --workspace "${WS}" --json
SEARCH_OUT="${LAST_STDOUT}"
TARGET_HITS="$(evidence_hit_count "${SEARCH_OUT}")"
TARGET_TOP="$(top_evidence_content "${SEARCH_OUT}")"
if [[ "${LAST_EXIT}" -eq 0 && "${TARGET_HITS}" != "0" && "${TARGET_TOP}" == *"${TARGET_MARKER}"* ]]; then
    event transcript_phrase_ranks_its_own_span_first pass "evidence_hits=${TARGET_HITS}"
else
    event transcript_phrase_ranks_its_own_span_first fail "exit ${LAST_EXIT}; hits=${TARGET_HITS}; top content lacked '${TARGET_MARKER}'; top=$(printf '%s' "${TARGET_TOP}" | head -c 160); docIds=$(jq -c '[(.data.results // [])[]? | .docId]' "${SEARCH_OUT}" 2>/dev/null); degraded=$(jq -c '[(.degraded // [])[] | .code]' "${SEARCH_OUT}" 2>/dev/null)"
fi

# --- 5. THE DISCRIMINATOR: the other phrase ranks the OTHER span first ----
# This replaces an absence assertion that was measurably vacuous (see the
# comment above the fixture). Two symmetric positives cannot both pass unless
# retrieval actually responds to the query: a search that returns every span
# in a fixed order satisfies at most one of them.
run_ee search "${DISTRACTOR_PHRASE}" --workspace "${WS}" --json
DISTRACTOR_OUT="${LAST_STDOUT}"
DISTRACTOR_TOP="$(top_evidence_content "${DISTRACTOR_OUT}")"
if [[ "${LAST_EXIT}" -eq 0 && "${DISTRACTOR_TOP}" == *"${DISTRACTOR_MARKER}"* ]]; then
    event distractor_phrase_ranks_its_own_span_first pass
else
    event distractor_phrase_ranks_its_own_span_first fail "retrieval does not discriminate between two unrelated imported sessions, so the assertion above proves nothing about query relevance; top content lacked '${DISTRACTOR_MARKER}'; top=$(printf '%s' "${DISTRACTOR_TOP}" | head -c 160); docIds=$(jq -c '[(.data.results // [])[]? | .docId]' "${DISTRACTOR_OUT}" 2>/dev/null)"
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
#
# The fields below are the ACTUAL representation, read off a measured run on
# 2026-10-14 rather than assumed. My first version asserted a
# `provenanceUri`/`provenance.uri` string and failed -- not because provenance
# was missing, but because an evidence item carries it STRUCTURALLY:
#
#   {"rank":1,"entityKind":"evidence_span","evidenceSpanId":"ev_...",
#    "entityRevision":"blake3:...","sessionId":"sess_...","startLine":2,
#    "endLine":2,"section":"evidence","content":"assistant: Root cause: ..."}
#
# which locates the exact stored lines more precisely than a URI would. The
# assertion now requires the span id, the session id, a line range and the
# entity revision together, because any one of them alone would not let a
# reader find the source.
PROVENANCE_OK="$(jq -r '[(.data.pack.items // [])[]?
       | select((.entityKind // "") == "evidence_span")
       | select(((.evidenceSpanId // "") | startswith("ev_"))
                and ((.sessionId // "") | startswith("sess_"))
                and ((.entityRevision // "") | length) > 0
                and (.startLine != null) and (.endLine != null))] | length' \
    "${PACK_OUT}" 2>/dev/null || echo 0)"
if [[ "${PROVENANCE_OK}" != "0" ]]; then
    event evidence_items_carry_provenance pass "with_full_provenance=${PROVENANCE_OK}"
else
    event evidence_items_carry_provenance fail "no evidence_span item carried the full span/session/line-range/revision set; items=$(jq -c '[(.data.pack.items // [])[]? | select((.entityKind // "") == "evidence_span") | {evidenceSpanId, sessionId, entityRevision, startLine, endLine}]' "${PACK_OUT}" 2>/dev/null | head -c 400)"
fi

# The content a reader actually pays tokens for must be the TRANSCRIPT line,
# not an envelope or a placeholder. Measured: the item's content was
# "assistant: Root cause: the retry budget collapsed because the backoff
# jitter seed was shared across shards." -- so assert the marker survives into
# the packed body.
PACKED_BODY_OK="$(jq -r --arg m "${TARGET_MARKER}" '[(.data.pack.items // [])[]?
       | select((.entityKind // "") == "evidence_span")
       | select(((.content // "") | ascii_downcase) | contains($m))] | length' \
    "${PACK_OUT}" 2>/dev/null || echo 0)"
if [[ "${PACKED_BODY_OK}" != "0" ]]; then
    event packed_evidence_body_is_the_transcript_line pass
else
    event packed_evidence_body_is_the_transcript_line fail "no packed evidence item's content contained '${TARGET_MARKER}'; contents=$(jq -c '[(.data.pack.items // [])[]? | select((.entityKind // "") == "evidence_span") | (.content // "")[0:90]]' "${PACK_OUT}" 2>/dev/null | head -c 400)"
fi

# --- 7. LEAK CANARY: bd-16imy's "denied/redacted material must not leak" ---
#
# The canary sits on line 5 of the distractor transcript. Two surfaces are
# checked, and the query is aimed STRAIGHT AT the secret, because a leak that
# only appears when someone searches for the token is still a leak -- and a
# query about espresso would not have exercised it.
run_ee search "${SECRET_CANARY}" --workspace "${WS}" --json
LEAK_SEARCH_OUT="${LAST_STDOUT}"
if ! grep -qF "${SECRET_CANARY}" "${LEAK_SEARCH_OUT}" 2>/dev/null; then
    event secret_canary_absent_from_search pass
else
    event secret_canary_absent_from_search fail "the imported secret token appears verbatim in search output; hits=$(jq -c '[(.data.results // [])[]? | .docId]' "${LEAK_SEARCH_OUT}" 2>/dev/null)"
fi

run_ee pack "${SECRET_CANARY}" --workspace "${WS}" --json
LEAK_PACK_OUT="${LAST_STDOUT}"
if ! grep -qF "${SECRET_CANARY}" "${LEAK_PACK_OUT}" 2>/dev/null; then
    event secret_canary_absent_from_pack pass
else
    event secret_canary_absent_from_pack fail "the imported secret token appears verbatim in pack output; items=$(jq -c '[(.data.pack.items // [])[]? | {entityKind, evidenceSpanId}]' "${LEAK_PACK_OUT}" 2>/dev/null | head -c 300)"
fi

# The canary must be absent because it was WITHHELD, not because the whole
# distractor session failed to index. Without this control the two assertions
# above would pass just as happily on an empty corpus, which is the shape that
# makes an absence claim worthless (measured the hard way earlier in this very
# script's history).
if [[ "${DISTRACTOR_TOP}" == *"${DISTRACTOR_MARKER}"* ]]; then
    event leak_control_distractor_session_is_indexed pass
else
    event leak_control_distractor_session_is_indexed fail "the distractor session is not retrievable, so the canary's absence above is unmeasured rather than proof of redaction"
fi

printf 'imported transcript retrieval e2e: %s steps, %s failures; root %s\n' \
    "${STEP}" "${FAILURES}" "${ROOT}" >&2
if [[ "${FAILURES}" -gt 0 ]]; then
    exit 2
fi
exit 0
