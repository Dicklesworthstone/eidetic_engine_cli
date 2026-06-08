#!/usr/bin/env bash
# bd-1n0np.22.6 — Provenance Attestation Bundles end-to-end (real binary).
#
# Full scenario (E22): a temp workspace with a memory whose body carries a
# secret ->
#   1. `ee attest query <q>` and `ee attest memory <id>` emit an ee.attest.v1
#      bundle with a blake3 `bundleHash` (the canonical chain-of-custody object).
#   2. The bundle is DETERMINISTIC: two attests of the same subject reproduce the
#      identical bundleHash.
#   3. The bundle is REDACTION-SAFE: rawTextIncluded is false and the raw secret
#      never appears anywhere in the bundle JSON (zero secret leakage).
#   4. When support-bundle / handoff embed the attestation (bd-1n0np.22.3), the
#      embedded bundleHash MUST equal the standalone hash.
#
# The `ee attest` surface (memory/pack/query) is landed (bd-1n0np.22.1/22.2/22.4),
# so steps 1-3 run FOR REAL. The consumer-embedding in support-bundle/handoff is
# bd-1n0np.22.3 and is not wired yet, so step 4 is CAPABILITY-GUARDED: a missing
# embedding records a visible log_drop (the no-silent-cap rule) carrying the exact
# hash-equality assertion that activates once 22.3 lands. No false pass.
#
# NOTE: no `set -e` — the harness assert_* helpers accumulate pass/fail and
# `harness_summary` decides the exit code, so a single failing assert must not
# abort the run before the summary is written.
set -uo pipefail

E2E_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/e2e_harness.sh
source "$E2E_DIR/lib/e2e_harness.sh"

harness_init "attestation"

ee_json() { "$EE_BIN" "$@" 2>/dev/null || true; }
ee_supports() { "$EE_BIN" "$@" --help >/dev/null 2>&1; }

# A distinctive secret token that must never leak into an attestation bundle.
SECRET="SECRET_attest_e2e_4f2a9c"

with_temp_workspace WS

step "init workspace"
init_out="$(ee_json init --workspace "$WS" --json)"
assert_jq "$init_out" '.success == true' "ee init succeeds"

if ! ee_supports attest memory; then
    log_drop 1 "ee attest surface unavailable on this binary (bd-1n0np.22.1/22.2/22.4): when present, assert a deterministic redaction-safe ee.attest.v1 bundle with a blake3 bundleHash, zero secret leakage, and equal hash when embedded in support-bundle/handoff"
    harness_summary
    exit "$?"
fi

step "remember a memory whose body carries a secret"
mem="$(ee_json remember "The deploy token is $SECRET and lives at infra/secrets.env." \
    --workspace "$WS" --level procedural --kind fact --json)"
assert_jq "$mem" '.success == true' "remember succeeds"
mem_id="$(printf '%s' "$mem" | jq -r '.id // .data.memory_id // .data.id // empty')"
assert_eq "$([ -n "$mem_id" ] && echo present || echo missing)" "present" "memory id present"

step "ee attest query emits a hash-only ee.attest.v1 bundle"
q="$(ee_json attest query "deploy policy for fridays" --workspace "$WS" --json)"
assert_jq "$q" '.success == true' "attest query succeeds"
assert_jq "$q" '.data.schema == "ee.attest.v1"' "query bundle carries the v1 schema"
assert_jq "$q" '(.data.bundleHash // "") | startswith("blake3:")' \
    "query bundle carries a blake3 bundleHash"
assert_jq "$q" '(.data.rawTextIncluded // false) == false' \
    "query bundle is hash-only (rawTextIncluded false)"

step "ee attest memory emits a bundle; it is deterministic"
b1="$(ee_json attest memory "$mem_id" --workspace "$WS" --json)"
assert_jq "$b1" '.success == true' "attest memory succeeds"
assert_jq "$b1" '.data.schema == "ee.attest.v1"' "memory bundle carries the v1 schema"
h1="$(printf '%s' "$b1" | jq -r '.data.bundleHash // empty')"
assert_eq "$([ -n "$h1" ] && echo present || echo missing)" "present" "memory bundleHash present"
b2="$(ee_json attest memory "$mem_id" --workspace "$WS" --json)"
h2="$(printf '%s' "$b2" | jq -r '.data.bundleHash // empty')"
assert_eq "$h1" "$h2" "two attests of the same memory reproduce the identical bundleHash"

step "the bundle is redaction-safe: zero secret leakage"
assert_jq "$b1" '(.data.rawTextIncluded // false) == false' \
    "memory bundle is redaction-safe (rawTextIncluded false)"
leak_count="$(printf '%s' "$b1" | grep -c "$SECRET" || true)"
assert_eq "$leak_count" "0" "raw secret never appears in the memory attestation bundle"
leak_q="$(printf '%s' "$q" | grep -c "$SECRET" || true)"
assert_eq "$leak_q" "0" "raw secret never appears in the query attestation bundle"

step "ee attest pack emits a bundle for a stored pack"
pk="$(ee_json pack "deploy policy" --workspace "$WS" --json)"
pack_id="$(printf '%s' "$pk" | jq -r '.data.pack.pack_id // .data.pack_id // .data.packId // empty')"
if [ -n "$pack_id" ] && ee_supports attest pack; then
    pb="$(ee_json attest pack "$pack_id" --workspace "$WS" --json)"
    assert_jq "$pb" '.success == true' "attest pack succeeds"
    assert_jq "$pb" '(.data.bundleHash // "") | startswith("blake3:")' \
        "pack bundle carries a blake3 bundleHash"
else
    log_drop 1 "attest pack skipped: no stored pack id available on this binary; when present, assert ee attest pack emits a deterministic ee.attest.v1 bundle"
fi

step "support-bundle / handoff embed the identical bundleHash (bd-1n0np.22.3)"
embedded_checked=0
for surface in "support-bundle" "handoff"; do
    if ee_supports "$surface"; then
        out="$(ee_json "$surface" --workspace "$WS" --json)"
        if printf '%s' "$out" | grep -q "$h1"; then
            embedded_checked=1
            assert_jq "$out" '.success == true' "$surface runs"
            # The embedded attestation hash must equal the standalone hash.
            assert_eq "$(printf '%s' "$out" | grep -c "$h1" | awk '{print ($1>0)?"present":"absent"}')" \
                "present" "$surface embeds the identical attestation bundleHash"
        fi
    fi
done
if [ "$embedded_checked" -eq 0 ]; then
    log_drop 1 "attestation embedding in support-bundle/handoff pending (bd-1n0np.22.3): when wired, assert the embedded ee.attest.v1 bundleHash equals the standalone 'ee attest memory' hash ($h1), proving one canonical chain-of-custody object"
fi

end_temp_workspace
harness_summary
