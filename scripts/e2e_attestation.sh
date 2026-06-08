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
#   4. support-bundle / handoff consume the shared pack AttestationBundle surface
#      manifest: the embedded pack bundleHash MUST equal `ee attest pack`.
#
# The `ee attest` surface (memory/pack/query) and the consumer embedding
# (bd-1n0np.22.3) are landed, so all four steps run FOR REAL. No capability drop
# is allowed for support-bundle/handoff hash equality.
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
assert_jq "$pk" '.success == true' "pack for attestation succeeds"
pack_records="$(ee_json --workspace "$WS" db inspect pack_records --limit 20 --json)"
assert_jq "$pack_records" '.success == true' "pack records are inspectable"
pack_id="$(printf '%s' "$pack_records" \
    | jq -r '[.data.report.rows[]?.values | select(.query == "deploy policy")] | sort_by(.created_at) | reverse | .[0].id // empty' \
    2>/dev/null || true)"
assert_eq "$([ -n "$pack_id" ] && echo present || echo missing)" "present" "stored pack id present"
pb="$(ee_json attest pack "$pack_id" --workspace "$WS" --json)"
assert_jq "$pb" '.success == true' "attest pack succeeds"
assert_jq "$pb" '(.data.bundleHash // "") | startswith("blake3:")' \
    "pack bundle carries a blake3 bundleHash"
pack_hash="$(printf '%s' "$pb" | jq -r '.data.bundleHash // empty')"
assert_eq "$([ -n "$pack_hash" ] && echo present || echo missing)" "present" "pack bundleHash present"

step "support-bundle embeds the identical pack bundleHash (bd-1n0np.22.3)"
support_out="$WS/support-bundles"
mkdir -p "$support_out"
support="$(ee_json support bundle --workspace "$WS" --out "$support_out" --json)"
assert_jq "$support" '.success == true' "support bundle succeeds"
support_dir="$(printf '%s' "$support" | jq -r '.data.outputPath // empty')"
assert_eq "$([ -n "$support_dir" ] && echo present || echo missing)" "present" \
    "support bundle output path present"
support_pack_summary="$(cat "$support_dir/pack_replay_summary.json" 2>/dev/null || true)"
assert_jq "$support_pack_summary" '.schema == "ee.support_bundle.pack_replay_summary.v1"' \
    "support bundle pack replay summary schema"
support_hash_count="$(printf '%s' "$support_pack_summary" \
    | jq -r --arg pack "$pack_id" --arg hash "$pack_hash" \
        '[.packs[]? | select(.packId == $pack and .attestationBundle.bundleHash == $hash)] | length' \
    2>/dev/null || echo 0)"
assert_eq "$support_hash_count" "1" \
    "support bundle embeds the identical pack attestation bundleHash"

step "handoff embeds the identical pack bundleHash (bd-1n0np.22.3)"
handoff_path="$WS/handoff.json"
handoff="$(ee_json handoff create --workspace "$WS" --out "$handoff_path" --json)"
assert_jq "$handoff" '.schema == "ee.handoff.create.v1"' "handoff create succeeds"
handoff_hash_count="$(printf '%s' "$handoff" \
    | jq -r --arg pack "$pack_id" --arg hash "$pack_hash" \
        '[.pack_replay_summary.packs[]? | select(.packId == $pack and .attestationBundle.bundleHash == $hash)] | length' \
    2>/dev/null || echo 0)"
assert_eq "$handoff_hash_count" "1" \
    "handoff embeds the identical pack attestation bundleHash"

end_temp_workspace
harness_summary
