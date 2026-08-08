#!/usr/bin/env bash
# bd-tc-epic-qzk7o.8.7 — adversarial-variant self-test for the fake OIDC IdP.
#
# Proves the harness can deterministically MINT each token defect / protocol
# attack the tier-2 verification client (T7.5) must reject: secret-required
# rejection, expiry, algorithm confusion, unknown kid, unsigned (alg=none),
# tampered signature, noncanonical base64url, wrong issuer/audience, and
# missing verified-email. This test asserts the harness PRODUCES each bad
# shape; the ee client's rejection of them is T7.5's contract. Fully offline
# (python3 + openssl + curl); no ee binary, no network.

# Assertions are intentionally passed as single-quoted programs and evaluated
# inside run_defect so each live server stays in the parent shell. The named
# helpers and response/JWKS locals are therefore references through eval.
# shellcheck disable=SC2016,SC2034,SC2329

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/e2e_overhaul/lib/fake_idp.sh
. "$SCRIPT_DIR/lib/fake_idp.sh"

FAILS=0

emit() {
    python3 -c '
import datetime, json, sys
print(json.dumps({
    "schema": "ee.test_event.v1",
    "ts": datetime.datetime.now(datetime.timezone.utc).isoformat(timespec="microseconds").replace("+00:00", "Z"),
    "test_id": "fake_idp_defects_smoke",
    "kind": "assert_ok" if sys.argv[2] == "pass" else "assert_fail",
    "fields": {"label": sys.argv[1], "expected": "pass", "actual": sys.argv[2]},
}, separators=(",", ":")))
' "$1" "$2"
}

check() {
    if [ "$2" = "1" ]; then
        emit "$1" "pass"
    else
        emit "$1" "fail"
        echo "FAIL: $1" >&2
        FAILS=$((FAILS + 1))
    fi
}

# jwt_field <segment-index 0|1> <dotted-key> <token>
jwt_field() {
    python3 -c '
import base64, json, sys
seg = sys.argv[3].split(".")[int(sys.argv[1])]
data = json.loads(base64.urlsafe_b64decode(seg + "=" * (-len(seg) % 4)))
value = data
for part in sys.argv[2].split("."):
    value = value.get(part) if isinstance(value, dict) else None
    if value is None:
        break
print("" if value is None else value)
' "$1" "$2" "$3"
}

rsa_signature_verdict() {
    python3 -c '
import base64, hashlib, hmac, json, sys

def decode(value):
    return base64.urlsafe_b64decode(value + "=" * (-len(value) % 4))

try:
    protected, payload, signature = sys.argv[1].split(".")
    header = json.loads(decode(protected))
    jwks = json.loads(sys.argv[2])
    jwk = next(key for key in jwks["keys"] if key.get("kid") == header.get("kid"))
    n = int.from_bytes(decode(jwk["n"]), "big")
    e = int.from_bytes(decode(jwk["e"]), "big")
    raw_signature = decode(signature)
    encoded = pow(int.from_bytes(raw_signature, "big"), e, n).to_bytes((n.bit_length() + 7) // 8, "big")
    prefix = bytes.fromhex("3031300d060960864801650304020105000420")
    digest_info = prefix + hashlib.sha256(f"{protected}.{payload}".encode("ascii")).digest()
    expected = b"\x00\x01" + b"\xff" * (len(encoded) - len(digest_info) - 3) + b"\x00" + digest_info
    print("valid" if hmac.compare_digest(encoded, expected) else "invalid")
except Exception:
    print("invalid")
' "$1" "$2"
}

# run_defect starts the server (in the SCRIPT body, never inside a $() capture
# — a backgrounded server there would hold the substitution's stdout pipe open
# and hang), mints one granted token, exposes $response, evaluates the
# assertion, then reaps the server and retains its evidence.
run_defect() {
    local name="$1" scenario_json="$2" assertion="$3" token_suffix="${4:-}"
    local sc
    sc="$(mktemp "${TMPDIR:-/tmp}/idp-defect-XXXXXX")"
    printf '%s' "$scenario_json" > "$sc"

    fake_idp_start "$sc"
    local device code response jwks artifact_present
    device="$(fake_idp_curl "/device" -X POST --data '')"
    code="$(python3 -c 'import json,sys; print(json.loads(sys.argv[1])["device_code"])' "$device")"
    jwks="$(fake_idp_curl "/jwks")"
    response="$(fake_idp_curl "/token" -X POST \
        --data "grant_type=urn:ietf:params:oauth:grant-type:device_code&device_code=${code}${token_suffix}")"
    eval "$assertion"
    artifact_present="$(fake_idp_curl "/_artifact" | python3 -c 'import json,sys; print("1" if json.load(sys.stdin).get("present") else "0")')"
    check "${name}_does_not_project_unverified_artifact" \
        "$([ "$artifact_present" = "0" ] && echo 1 || echo 0)"
    fake_idp_stop
    echo "fake_idp_defects_smoke: retained $name scenario at $sc" >&2
}

trap 'fake_idp_stop' EXIT

# 1. secret_required: polling the token endpoint without client_secret is 401.
run_defect "secret_required_rejects_public_client" \
    '{"project_verified_artifact":true,"secret_required":true,"flow":{"initial_status":"granted"}}' \
    'ERR="$(python3 -c "import json,sys; print(json.loads(sys.argv[1]).get(\"error\",\"\"))" "$response" 2>/dev/null || echo parse_error)"
     check "secret_required_rejects_public_client" "$([ "$ERR" = "invalid_client" ] && echo 1 || echo 0)"'

# 2. alg_none: unsigned token, empty third segment.
run_defect "alg_none_mints_unsigned_token" \
    '{"project_verified_artifact":true,"alg":"RS256","flow":{"initial_status":"granted"},"defects":{"alg_none":true}}' \
    'id_token="$(python3 -c "import json,sys; print(json.loads(sys.argv[1]).get(\"id_token\",\"\"))" "$response")"
     ALG="$(jwt_field 0 alg "$id_token")"
     SIG="$(printf "%s" "$id_token" | cut -d. -f3)"
     check "alg_none_header_is_none" "$([ "$ALG" = "none" ] && echo 1 || echo 0)"
     check "alg_none_signature_is_empty" "$([ -z "$SIG" ] && echo 1 || echo 0)"'

# 3. wrong_kid: header kid not present in JWKS.
run_defect "wrong_kid_references_absent_key" \
    '{"project_verified_artifact":true,"alg":"RS256","flow":{"initial_status":"granted"},"defects":{"wrong_kid":true}}' \
    'id_token="$(python3 -c "import json,sys; print(json.loads(sys.argv[1]).get(\"id_token\",\"\"))" "$response")"
     KID="$(jwt_field 0 kid "$id_token")"
     check "wrong_kid_is_not_a_jwks_key" "$([ "$KID" = "kid-not-in-jwks" ] && echo 1 || echo 0)"'

# 4. bad_signature: last signature byte flipped (verification must fail).
run_defect "bad_signature_is_tampered" \
    '{"project_verified_artifact":true,"alg":"RS256","flow":{"initial_status":"granted"},"defects":{"bad_signature":true}}' \
    'id_token="$(python3 -c "import json,sys; print(json.loads(sys.argv[1]).get(\"id_token\",\"\"))" "$response")"
     SEG="$(printf "%s" "$id_token" | awk -F. "{print NF}")"
     SIGLEN="$(printf "%s" "$id_token" | cut -d. -f3 | python3 -c "import base64,sys; s=sys.stdin.read().strip(); print(len(base64.urlsafe_b64decode(s+chr(61)*(-len(s)%4))))")"
     VERDICT="$(rsa_signature_verdict "$id_token" "$jwks")"
     check "bad_signature_keeps_three_segments" "$([ "$SEG" = "3" ] && echo 1 || echo 0)"
     check "bad_signature_keeps_256_byte_rsa_sig" "$([ "$SIGLEN" = "256" ] && echo 1 || echo 0)"
     check "bad_signature_fails_exact_input_verification" "$([ "$VERDICT" = "invalid" ] && echo 1 || echo 0)"'

# 5. header_alg confusion: header advertises ES256 while signed RS256.
run_defect "alg_confusion_header_mismatches_signature" \
    '{"project_verified_artifact":true,"alg":"RS256","flow":{"initial_status":"granted"},"defects":{"header_alg":"ES256"}}' \
    'id_token="$(python3 -c "import json,sys; print(json.loads(sys.argv[1]).get(\"id_token\",\"\"))" "$response")"
     ALG="$(jwt_field 0 alg "$id_token")"
     SIGLEN="$(printf "%s" "$id_token" | cut -d. -f3 | python3 -c "import base64,sys; s=sys.stdin.read().strip(); print(len(base64.urlsafe_b64decode(s+chr(61)*(-len(s)%4))))")"
     check "alg_confusion_advertises_es256" "$([ "$ALG" = "ES256" ] && echo 1 || echo 0)"
     check "alg_confusion_signature_is_actually_rsa_256_bytes" "$([ "$SIGLEN" = "256" ] && echo 1 || echo 0)"'

# 6. noncanonical base64url: standard base64 with padding (invalid for JOSE).
run_defect "noncanonical_base64url_uses_padding" \
    '{"project_verified_artifact":true,"alg":"RS256","flow":{"initial_status":"granted"},"defects":{"noncanonical_base64url":true}}' \
    'id_token="$(python3 -c "import json,sys; print(json.loads(sys.argv[1]).get(\"id_token\",\"\"))" "$response")"
     HAS_PAD="$(case "$id_token" in *=*) echo 1;; *) echo 0;; esac)"
     VERDICT="$(rsa_signature_verdict "$id_token" "$jwks")"
     check "noncanonical_base64url_contains_padding" "$HAS_PAD"
     check "noncanonical_base64url_signature_matches_transmitted_input" "$([ "$VERDICT" = "valid" ] && echo 1 || echo 0)"'

# 7. expired token via negative lifetime.
run_defect "expired_lifetime_produces_past_exp" \
    '{"project_verified_artifact":true,"alg":"RS256","flow":{"initial_status":"granted"},"claims":{"lifetime_seconds":-3600}}' \
    'id_token="$(python3 -c "import json,sys; print(json.loads(sys.argv[1]).get(\"id_token\",\"\"))" "$response")"
     EXP="$(jwt_field 1 exp "$id_token")"
     IAT="$(jwt_field 1 iat "$id_token")"
     check "expired_exp_precedes_iat" "$([ -n "$EXP" ] && [ -n "$IAT" ] && [ "$EXP" -lt "$IAT" ] && echo 1 || echo 0)"'

# 8. Wrong issuer with every other claim valid.
run_defect "claim_override_wrong_issuer" \
    '{"project_verified_artifact":true,"alg":"RS256","flow":{"initial_status":"granted"},"claims":{"iss":"https://attacker.invalid"}}' \
    'id_token="$(python3 -c "import json,sys; print(json.loads(sys.argv[1]).get(\"id_token\",\"\"))" "$response")"
     ISS="$(jwt_field 1 iss "$id_token")"
     check "claim_attack_iss_is_attacker" "$([ "$ISS" = "https://attacker.invalid" ] && echo 1 || echo 0)"'

# 9. Wrong audience with issuer/signature/lifetime otherwise valid.
run_defect "claim_override_wrong_audience" \
    '{"project_verified_artifact":true,"alg":"RS256","flow":{"initial_status":"granted"},"claims":{"aud":"attacker-client"}}' \
    'id_token="$(python3 -c "import json,sys; print(json.loads(sys.argv[1]).get(\"id_token\",\"\"))" "$response")"
     AUD="$(jwt_field 1 aud "$id_token")"
     check "claim_attack_aud_is_attacker" "$([ "$AUD" = "attacker-client" ] && echo 1 || echo 0)"'

# 10. Previewed email must be positively verified.
run_defect "claim_override_unverified_email" \
    '{"project_verified_artifact":true,"alg":"RS256","flow":{"initial_status":"granted"},"privacy_policy":{"preview_email":true,"allowed_groups":[],"max_allowed_group_matches":0},"claims":{"email_verified":false}}' \
    'id_token="$(python3 -c "import json,sys; print(json.loads(sys.argv[1]).get(\"id_token\",\"\"))" "$response")"
     EV="$(jwt_field 1 email_verified "$id_token")"
     check "claim_attack_email_unverified" "$([ "$EV" = "False" ] && echo 1 || echo 0)"'

if [ "$FAILS" -eq 0 ]; then
    emit "defects_smoke_overall" "pass"
    echo "fake_idp_defects_smoke: all checks passed" >&2
    exit 0
fi
emit "defects_smoke_overall" "fail"
echo "fake_idp_defects_smoke: $FAILS check(s) failed" >&2
exit 1
