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

# bd-16imy's acceptance says the pack must hydrate imported evidence "with
# why/provenance", and until now this script asserted only the provenance half.
# `PackEvidenceItem` carries a `why: String` (src/pack/mod.rs:5988) which the
# native path populates, so the field is not aspirational -- it was simply
# unasserted, which is how an acceptance bullet passes review while half of it
# goes unmeasured.
#
# Requires a NON-EMPTY why on every evidence item rather than on at least one.
# An "at least one" form would pass a pack that explained a single item and
# left the rest bare, which is the failure a reader would actually hit.
EVIDENCE_TOTAL="$(jq -r '[(.data.pack.items // [])[]?
       | select((.entityKind // "") == "evidence_span")] | length' \
    "${PACK_OUT}" 2>/dev/null || echo 0)"
EVIDENCE_WITH_WHY="$(jq -r '[(.data.pack.items // [])[]?
       | select((.entityKind // "") == "evidence_span")
       | select(((.why // "") | length) > 0)] | length' \
    "${PACK_OUT}" 2>/dev/null || echo 0)"
if [[ "${EVIDENCE_TOTAL}" != "0" && "${EVIDENCE_WITH_WHY}" == "${EVIDENCE_TOTAL}" ]]; then
    event evidence_items_carry_why pass "with_why=${EVIDENCE_WITH_WHY}/${EVIDENCE_TOTAL}"
else
    event evidence_items_carry_why fail "expected every evidence item to carry a non-empty why; with_why=${EVIDENCE_WITH_WHY} of ${EVIDENCE_TOTAL}; whys=$(jq -c '[(.data.pack.items // [])[]? | select((.entityKind // "") == "evidence_span") | .why]' "${PACK_OUT}" 2>/dev/null | head -c 400)"
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

# bd-16imy's bullet 4 says the pack hydrates imported evidence into a
# "deterministic section". Nothing asserted the DETERMINISTIC half, so pack the
# same phrase again and require the evidence section to be identical.
#
# Compared field by field rather than as whole envelopes, because the response
# legitimately varies between runs (pack id, timing) while the SECTION must not.
# The fields are the item's real representation, read off a measured run: rank,
# evidenceSpanId, entityRevision, sessionId, startLine, endLine, section and
# content. `jq -S` sorts keys so a key-order change cannot read as a difference.
#
# THE NON-EMPTY GUARD IS THE POINT. An earlier version of this bullet's
# assertion compared run 2 to run 1 and nothing else, and a bead comment
# correctly recorded that it "WOULD HAVE PASSED IDENTICALLY ON AN EMPTY PACK OR
# ON THE WRONG 100" -- a criterion that cannot fail is not evidence. So this
# fails when the section is empty, which makes equality meaningful.
PACK_SECTION_FILTER='[(.data.pack.items // [])[]?
       | select((.entityKind // "") == "evidence_span")
       | {rank, evidenceSpanId, entityRevision, sessionId, startLine, endLine, section, content}]'
PACK_SECTION_1="$(jq -S -c "${PACK_SECTION_FILTER}" "${PACK_OUT}" 2>/dev/null || echo 'ERR1')"
run_ee pack "${TRANSCRIPT_PHRASE}" --workspace "${WS}" --json
PACK_OUT_REPEAT="${LAST_STDOUT}"
PACK_SECTION_2="$(jq -S -c "${PACK_SECTION_FILTER}" "${PACK_OUT_REPEAT}" 2>/dev/null || echo 'ERR2')"
PACK_SECTION_COUNT="$(jq -r 'length' <<<"${PACK_SECTION_1}" 2>/dev/null || echo 0)"
if [[ "${LAST_EXIT}" -eq 0 && "${PACK_SECTION_COUNT}" != "0" && "${PACK_SECTION_1}" == "${PACK_SECTION_2}" ]]; then
    event pack_evidence_section_is_deterministic pass "items=${PACK_SECTION_COUNT} identical across two packs"
else
    event pack_evidence_section_is_deterministic fail "exit ${LAST_EXIT}; items=${PACK_SECTION_COUNT}; first=$(head -c 300 <<<"${PACK_SECTION_1}"); second=$(head -c 300 <<<"${PACK_SECTION_2}")"
fi

# --- 7. LEAK CANARY: bd-16imy's "denied/redacted material must not leak" ---
#
# THE QUERY IS DELIBERATELY BENIGN, and the first version of this check was
# wrong for the opposite reason.
#
# That version searched for the SECRET ITSELF, reasoning that a leak which only
# surfaces when someone searches for the token is still a leak. Measured
# 2026-10-14, both arms "failed" -- and they would have failed on a perfectly
# redacted store, because `ee search` echoes the caller's query back in its
# response (`"query": &self.query`, src/core/search.rs:4043 and :4336) and
# `ee pack` does the same through `query_observation_json`. Putting the secret
# in the query guarantees the secret is in the response. The check could not
# distinguish a leak from an echo, so it proved nothing in either direction.
#
# The question that matters is whether STORED transcript content leaks. So the
# query is the distractor's benign marker, which demonstrably retrieves that
# session (the symmetric assertion above passes on it), and the canary must be
# absent from what comes back.
run_ee search "${DISTRACTOR_MARKER}" --workspace "${WS}" --json
LEAK_SEARCH_OUT="${LAST_STDOUT}"
if ! grep -qF "${SECRET_CANARY}" "${LEAK_SEARCH_OUT}" 2>/dev/null; then
    event secret_canary_absent_from_search pass
else
    event secret_canary_absent_from_search fail "a benign query retrieved the imported secret token verbatim; hits=$(jq -c '[(.data.results // [])[]? | .docId]' "${LEAK_SEARCH_OUT}" 2>/dev/null)"
fi

run_ee pack "${DISTRACTOR_PHRASE}" --workspace "${WS}" --json
LEAK_PACK_OUT="${LAST_STDOUT}"
if ! grep -qF "${SECRET_CANARY}" "${LEAK_PACK_OUT}" 2>/dev/null; then
    event secret_canary_absent_from_pack pass
else
    event secret_canary_absent_from_pack fail "a benign pack request carried the imported secret token verbatim; items=$(jq -c '[(.data.pack.items // [])[]? | {entityKind, evidenceSpanId}]' "${LEAK_PACK_OUT}" 2>/dev/null | head -c 300)"
fi

# Positive control for the canary itself: the benign query must actually reach
# the distractor session's content, or "the secret is absent" is just an
# observation about an empty result set. Asserting the marker IS present in
# the same response that the secret must be absent from makes the pair
# meaningful.
if grep -qF "${DISTRACTOR_MARKER}" "${LEAK_SEARCH_OUT}" 2>/dev/null; then
    event leak_probe_reached_the_distractor_content pass
else
    event leak_probe_reached_the_distractor_content fail "the benign query returned nothing containing '${DISTRACTOR_MARKER}', so the canary's absence above is unmeasured"
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

# --- 8. WORKSPACE ISOLATION: bullet 1's "workspace isolation" clause ------
#
# bd-16imy's bullet 1 requires the canonical representation to retain
# "workspace isolation". Everything above runs in ONE workspace, and the
# distractor probes share it, so nothing exercised a SECOND workspace failing
# to see the first's imported spans.
#
# A ZERO HERE HAS FOUR CAUSES and only one of them is isolation: the span could
# be absent, or search could be broken in a fresh workspace, or degraded, or
# the query could be wrong. So the second workspace gets its OWN positive
# control first -- a memory it can find -- which proves retrieval works THERE
# before its silence about the first workspace's phrase is allowed to mean
# anything. WS1's hits (section 4) do not establish that: they prove search
# works in WS1.
# (This script has no `step` helper -- STEP is incremented inside run_ee -- so
# sections are marked by comment, as above.)
WS2="${ROOT}/ws2"
# ALPHABETIC sentinel. This was `${RANDOM}${RANDOM}`, two concatenated random
# integers, and that made this control fail INTERMITTENTLY -- roughly one run
# in five:
#
#   remember_error=policy_denied: Refusing to persist memory content that
#   contains secrets: phone_number.
#
# `ee` scans content for secrets before a durable write, and a long enough run
# of digits looks like a phone number. The product was right every time; the
# fixture was feeding it a different sentinel on each run and occasionally
# rolled one shaped like a phone number. The failure then surfaced as the
# second workspace being unable to retrieve its own memory, which reads like a
# retrieval or isolation defect and is neither.
#
# That cost five eliminated hypotheses -- retained worker state, the argument
# set, a contract change, an EE_DATABASE_PATH mismatch, determinism itself --
# before the cause field added in 5475da79d named it on the first failing run
# it saw.
#
# Mapping the digits to letters keeps the sentinel unique and searchable while
# making a phone-number shape impossible to roll.
# $RANDOM is expanded in THIS shell and only then handed to the substitution.
# Expanding it inside `$( ... )` yields the subshell's reseeded value, which is
# identical on every call -- the sentinel would be alphabetic but no longer
# unique, and uniqueness is the whole point of a sentinel.
ISO_SENTINEL_DIGITS="${RANDOM}${RANDOM}"
ISO_SENTINEL="isolationsentinel-$(printf '%s' "${ISO_SENTINEL_DIGITS}" | tr '0-9' 'abcdefghij')"
run_ee init --workspace "${WS2}" --json
ISO_INIT_EXIT="${LAST_EXIT}"
# A durable write needs provenance: `remember` without --source is POLICY
# DENIED (exit 7, "policy denied operation"), which the first version of this
# control hit. The arg set mirrors scripts/e2e_ask.sh's remember_memory.
run_ee remember "Workspace isolation probe ${ISO_SENTINEL} stays local to its own store." \
    --workspace "${WS2}" --level semantic --kind fact --confidence 0.70 \
    --source "manual://imported_transcript_e2e/isolation-probe" \
    --no-auto-link --no-propose-candidates --json
ISO_REMEMBER_EXIT="${LAST_EXIT}"
# Capture the CAUSE beside the exit code.
#
# A bare `remember=7` says a durable write was refused and nothing about why,
# and this control has already cost two wrong hypotheses that one error code
# would have killed on sight: retained worker state (refuted by a clean-root
# run that reproduced it) and an EE_DATABASE_PATH mismatch (refuted by
# reproducing the exact env shape, which succeeds). The response body is
# already on disk in ${LAST_STDOUT}; only nobody was reading it.
ISO_REMEMBER_ERR="$(jq -r '((.error.code // "none") + ": " + ((.error.message // "")[0:200]))' "${LAST_STDOUT}" 2>/dev/null || printf 'unreadable')"
# WS2 needs its own published index for the same reason WS1 does (section 3).
run_ee index rebuild --workspace "${WS2}" --json
ISO_INDEX_EXIT="${LAST_EXIT}"
run_ee search "${ISO_SENTINEL}" --workspace "${WS2}" --json
ISO_OWN_HITS="$(jq -r '[(.data.results // [])[]?] | length' "${LAST_STDOUT}" 2>/dev/null || echo 0)"
ISO_OWN_EXIT="${LAST_EXIT}"
ISO_CONTROL_OK=0
if [[ "${ISO_INIT_EXIT}" -eq 0 && "${ISO_REMEMBER_EXIT}" -eq 0 && "${ISO_INDEX_EXIT}" -eq 0 && "${ISO_OWN_EXIT}" -eq 0 && "${ISO_OWN_HITS}" != "0" ]]; then
    ISO_CONTROL_OK=1
    event second_workspace_retrieval_works pass "own_hits=${ISO_OWN_HITS}"
else
    event second_workspace_retrieval_works fail "the second workspace cannot find its OWN memory, so its silence about the first workspace proves nothing; init=${ISO_INIT_EXIT} remember=${ISO_REMEMBER_EXIT} index=${ISO_INDEX_EXIT} search=${ISO_OWN_EXIT} hits=${ISO_OWN_HITS}; remember_error=${ISO_REMEMBER_ERR}"
fi

# The isolation claim DEPENDS on that control. Measured 2026-10-14: with the
# control broken the zero below still "passed", which is a vacuous pass of
# exactly the kind this bead's ledger rejected for bullet 4 -- a criterion that
# cannot fail is not evidence. So an unproven control fails this assertion too.
run_ee search "${TRANSCRIPT_PHRASE}" --workspace "${WS2}" --json
ISO_LEAK_OUT="${LAST_STDOUT}"
ISO_LEAK_EXIT="${LAST_EXIT}"
ISO_LEAK_HITS="$(evidence_hit_count "${ISO_LEAK_OUT}")"
if [[ "${ISO_CONTROL_OK}" -eq 1 && "${ISO_LEAK_EXIT}" -eq 0 && "${ISO_LEAK_HITS}" == "0" ]]; then
    event second_workspace_cannot_see_imported_spans pass "evidence_hits=0 while its own memory IS retrievable (own_hits=${ISO_OWN_HITS})"
elif [[ "${ISO_CONTROL_OK}" -ne 1 ]]; then
    event second_workspace_cannot_see_imported_spans fail "UNMEASURED: evidence_hits=${ISO_LEAK_HITS} but the second workspace cannot retrieve its own memory, so this zero does not distinguish isolation from broken retrieval"
else
    event second_workspace_cannot_see_imported_spans fail "a fresh workspace retrieved the other workspace's imported evidence; exit ${ISO_LEAK_EXIT}; evidence_hits=${ISO_LEAK_HITS}; docIds=$(jq -c '[(.data.results // [])[]? | .docId]' "${ISO_LEAK_OUT}" 2>/dev/null)"
fi

printf 'imported transcript retrieval e2e: %s steps, %s failures; root %s\n' \
    "${STEP}" "${FAILURES}" "${ROOT}" >&2
if [[ "${FAILURES}" -gt 0 ]]; then
    exit 2
fi
exit 0
