#!/usr/bin/env bash
# bd-tc-epic-qzk7o.8.7 — self-check for the fake O
# IDC protocol harness. The two lines above are preserved prior bead work.
#
# Drives the live loopback-TLS server and its executable reference oracles.
# This proves deterministic stimuli and expected dispositions for T7.4–T7.6;
# it does not claim that the production client already enforces them.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/e2e_overhaul/lib/fake_idp.sh
. "$SCRIPT_DIR/lib/fake_idp.sh"

export EE_E2E_KEEP_WORKSPACE=1

SCENARIO="$(mktemp "${TMPDIR:-/tmp}/fake-idp-matrix-XXXXXX")"
RESTART_EXPECTATION="$(mktemp "${TMPDIR:-/tmp}/fake-idp-restart-XXXXXX")"
VERIFICATION_SCRATCH="$(mktemp -d "${TMPDIR:-/tmp}/fake-idp-verify-XXXXXX")"
cat > "$SCENARIO" <<'JSON'
{
  "issuer_path": "tenant-a",
  "deterministic_seed": "bd-tc-epic-qzk7o.8.7",
  "logical_clock": {"wall": 1000, "monotonic": 0},
  "identity_floor": 1000,
  "client_id": "CLIENT_SENTINEL_T77",
  "project_verified_artifact": true,
  "capability_profile": "absent",
  "alg": "RS256",
  "flow": {
    "initial_status": "authorization_pending",
    "expires_in": 900,
    "frame_ttl": 30
  },
  "claims": {
    "aud": "CLIENT_SENTINEL_T77",
    "sub": "SUBJECT_SENTINEL_T77",
    "email": "PREVIEW_EMAIL_SENTINEL_T77@example.test",
    "email_verified": true,
    "groups": [
      "ALLOWED_ALPHA_SENTINEL_T77",
      "ALLOWED_BETA_SENTINEL_T77",
      "FULL_GROUP_SENTINEL_T77"
    ],
    "lifetime_seconds": 300,
    "extra": {
      "phone_number": "UNRELATED_PII_SENTINEL_T77",
      "address": {"street": "PRIVATE_STREET_SENTINEL_T77"}
    }
  },
  "privacy_policy": {
    "preview_email": true,
    "allowed_groups": [
      "ALLOWED_ALPHA_SENTINEL_T77",
      "ALLOWED_BETA_SENTINEL_T77"
    ],
    "max_allowed_group_matches": 1
  },
  "token_response": {
    "access_token": "ACCESS_TOKEN_SENTINEL_T77",
    "refresh_token": "REFRESH_TOKEN_SENTINEL_T77"
  }
}
JSON

cleanup() {
    fake_idp_stop
    echo "fake_idp_selfcheck: retained scenario at $SCENARIO" >&2
    echo "fake_idp_selfcheck: retained harness evidence at $FAKE_IDP_RETAINED_DIR" >&2
    echo "fake_idp_selfcheck: retained restart expectation at $RESTART_EXPECTATION" >&2
    echo "fake_idp_selfcheck: retained signature scratch at $VERIFICATION_SCRATCH" >&2
}
trap cleanup EXIT

fake_idp_start "$SCENARIO"

# Exercise the hardened curl wrapper with deliberately hostile ambient state.
AMBIENT_DIR="$(mktemp -d "${TMPDIR:-/tmp}/fake-idp-ambient-XXXXXX")"
printf 'proxy = http://127.0.0.1:9\nnetrc\n' > "$AMBIENT_DIR/.curlrc"
printf 'machine 127.0.0.1 login NETRC_USER_SENTINEL_T77 password NETRC_PASSWORD_SENTINEL_T77\n' \
    > "$AMBIENT_DIR/.netrc"
chmod 600 "$AMBIENT_DIR/.netrc"
HOSTILE_KEYLOG="$AMBIENT_DIR/should-not-exist.keys"
set +e
HOSTILE_DISCOVERY="$(
    ALL_PROXY=http://127.0.0.1:9 \
    HTTPS_PROXY=http://127.0.0.1:9 \
    CURL_CA_BUNDLE="$AMBIENT_DIR/missing-ca.pem" \
    CURL_HOME="$AMBIENT_DIR" \
    HOME="$AMBIENT_DIR" \
    NETRC="$AMBIENT_DIR/.netrc" \
    CURL_SSL_BACKEND=definitely-invalid \
    SSLKEYLOGFILE="$HOSTILE_KEYLOG" \
    fake_idp_curl "/.well-known/openid-configuration"
)"
HOSTILE_CURL_RC=$?
fake_idp_curl "/jwks" --proxy http://127.0.0.1:9 >/dev/null 2>&1
UNSAFE_CURL_RC=$?
fake_idp_curl "/jwks" --insecure >/dev/null 2>&1
UNSAFE_TLS_CURL_RC=$?
fake_idp_curl "/jwks" example.invalid >/dev/null 2>&1
ALTERNATE_URL_CURL_RC=$?
CONTROL_URL_BODY="$(fake_idp_control '{"action":"network_evaluate","spec":{"url":"https://127.0.0.1/resource","validation_ips":["127.0.0.1"],"presentation_ips":["127.0.0.1"],"private_approved":true}}')"
CONTROL_URL_BODY_RC=$?
set -e
HOSTILE_KEYLOG_PRESENT=0
if [ -e "$HOSTILE_KEYLOG" ]; then
    HOSTILE_KEYLOG_PRESENT=1
fi

run_matrix_phase() {
    local phase="$1"
    python3 - \
        "$phase" "$FAKE_IDP_BASE" "$FAKE_IDP_CA" "$FAKE_IDP_DIR" \
        "$HOSTILE_CURL_RC" "$HOSTILE_DISCOVERY" \
        "$RESTART_EXPECTATION" "$VERIFICATION_SCRATCH" \
        "$UNSAFE_CURL_RC" "$UNSAFE_TLS_CURL_RC" \
        "$HOSTILE_KEYLOG_PRESENT" "$ALTERNATE_URL_CURL_RC" \
        "$CONTROL_URL_BODY_RC" "$CONTROL_URL_BODY" <<'PY'
import base64
import concurrent.futures
import datetime
import hashlib
import hmac
import http.client
import itertools
import json
import os
import pathlib
import socket
import ssl
import subprocess
import sys
import urllib.error
import urllib.parse
import urllib.request


(
    PHASE,
    BASE,
    CA_FILE,
    STATE_DIR,
    HOSTILE_RC,
    HOSTILE_DISCOVERY,
    RESTART_EXPECTATION,
    VERIFICATION_SCRATCH,
    UNSAFE_CURL_RC,
    UNSAFE_TLS_CURL_RC,
    HOSTILE_KEYLOG_PRESENT,
    ALTERNATE_URL_CURL_RC,
    CONTROL_URL_BODY_RC,
    CONTROL_URL_BODY,
) = sys.argv[1:]
MAX_U64 = (1 << 64) - 1
TEST_ID = "fake_idp_matrix_selfcheck"
failures = 0
labels = set()


def emit(label, passed):
    global failures
    if label in labels:
        passed = False
        label = f"duplicate_label:{label}"
    labels.add(label)
    if not passed:
        failures += 1
    event = {
        "schema": "ee.test_event.v1",
        "ts": datetime.datetime.now(datetime.timezone.utc)
        .isoformat(timespec="microseconds")
        .replace("+00:00", "Z"),
        "test_id": TEST_ID,
        "kind": "assert_ok" if passed else "assert_fail",
        "fields": {
            "label": label,
            "expected": "pass",
            "actual": "pass" if passed else "fail",
        },
    }
    print(json.dumps(event, sort_keys=True, separators=(",", ":")), flush=True)


def check(label, condition):
    emit(label, bool(condition))


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


def opener(pinned=True, follow_redirects=True):
    context = (
        ssl.create_default_context(cafile=CA_FILE)
        if pinned
        else ssl.create_default_context()
    )
    handlers = [
        urllib.request.ProxyHandler({}),
        urllib.request.HTTPSHandler(context=context),
    ]
    if not follow_redirects:
        handlers.append(NoRedirect())
    return urllib.request.build_opener(*handlers)


def request(
    path,
    method="GET",
    payload=None,
    headers=None,
    pinned=True,
    follow_redirects=True,
):
    body = None
    request_headers = dict(headers or {})
    if isinstance(payload, dict):
        body = json.dumps(payload, sort_keys=True, separators=(",", ":")).encode()
        request_headers.setdefault("Content-Type", "application/json")
    elif isinstance(payload, str):
        body = payload.encode()
    req = urllib.request.Request(
        BASE + path, data=body, method=method, headers=request_headers
    )
    try:
        response = opener(pinned, follow_redirects).open(req, timeout=5)
        raw = response.read()
        status = response.status
        response_headers = dict(response.headers.items())
    except urllib.error.HTTPError as error:
        raw = error.read()
        status = error.code
        response_headers = dict(error.headers.items())
    content_type = next(
        (
            value
            for key, value in response_headers.items()
            if key.lower() == "content-type"
        ),
        "",
    )
    decoded = (
        json.loads(raw)
        if raw and content_type.lower().startswith("application/json")
        else None
    )
    return status, decoded, response_headers, raw


def get_json(path):
    status, decoded, _, _ = request(path)
    if status != 200:
        raise AssertionError(f"GET {path} returned {status}")
    return decoded


def control(action, **values):
    command = {"action": action, **values}
    status, decoded, _, _ = request("/_control", "POST", command)
    if status != 200 or not decoded.get("ok"):
        raise AssertionError(f"control {action} failed: {status} {decoded}")
    return decoded.get("result", decoded)


def post_device():
    status, decoded, _, raw = request("/device", "POST", "")
    if status != 200:
        raise AssertionError(f"device endpoint returned {status}")
    return decoded, raw


def post_token(device_code, secret=None):
    form = {
        "grant_type": "urn:ietf:params:oauth:grant-type:device_code",
        "device_code": device_code,
    }
    if secret is not None:
        form["client_secret"] = secret
    encoded = urllib.parse.urlencode(form)
    return request(
        "/token",
        "POST",
        encoded,
        {"Content-Type": "application/x-www-form-urlencoded"},
    )


def b64decode(segment):
    return base64.urlsafe_b64decode(segment + "=" * (-len(segment) % 4))


def decode_jwt(token):
    protected, payload, signature = token.split(".")
    return (
        json.loads(b64decode(protected)),
        json.loads(b64decode(payload)),
        protected,
        payload,
        b64decode(signature),
    )


def verify_rs256_with_jwk(token, jwk):
    _, _, protected, payload, signature = decode_jwt(token)
    n = int.from_bytes(b64decode(jwk["n"]), "big")
    e = int.from_bytes(b64decode(jwk["e"]), "big")
    encoded = pow(int.from_bytes(signature, "big"), e, n).to_bytes(
        (n.bit_length() + 7) // 8, "big"
    )
    prefix = bytes.fromhex("3031300d060960864801650304020105000420")
    digest_info = prefix + hashlib.sha256(
        f"{protected}.{payload}".encode("ascii")
    ).digest()
    expected = (
        b"\x00\x01"
        + b"\xff" * (len(encoded) - len(digest_info) - 3)
        + b"\x00"
        + digest_info
    )
    return hmac.compare_digest(encoded, expected)


def verify_rs256(token, jwks):
    header = decode_jwt(token)[0]
    jwk = next(key for key in jwks["keys"] if key.get("kid") == header.get("kid"))
    return verify_rs256_with_jwk(token, jwk)


def der_length(length):
    if length < 128:
        return bytes([length])
    encoded = length.to_bytes((length.bit_length() + 7) // 8, "big")
    return bytes([0x80 | len(encoded)]) + encoded


def der(tag, body):
    return bytes([tag]) + der_length(len(body)) + body


def der_integer(value):
    body = value.to_bytes(max(1, (value.bit_length() + 7) // 8), "big")
    if body[0] & 0x80:
        body = b"\x00" + body
    return der(0x02, body)


def verify_es256(token, jwks):
    header, _, protected, payload, raw_signature = decode_jwt(token)
    jwk = next(key for key in jwks["keys"] if key.get("kid") == header.get("kid"))
    point = b"\x04" + b64decode(jwk["x"]) + b64decode(jwk["y"])
    algorithm = {
        "P-256": bytes.fromhex("301306072a8648ce3d020106082a8648ce3d030107"),
        "P-384": bytes.fromhex("301006072a8648ce3d020106052b81040022"),
    }[jwk["crv"]]
    public_der = der(0x30, algorithm + der(0x03, b"\x00" + point))
    width = len(b64decode(jwk["x"]))
    r = int.from_bytes(raw_signature[:width], "big")
    s = int.from_bytes(raw_signature[width:], "big")
    signature_der = der(0x30, der_integer(r) + der_integer(s))
    scratch_name = jwk["crv"].lower()
    public_path = os.path.join(VERIFICATION_SCRATCH, f"{scratch_name}-public.der")
    signature_path = os.path.join(VERIFICATION_SCRATCH, f"{scratch_name}-signature.der")
    input_path = os.path.join(VERIFICATION_SCRATCH, f"{scratch_name}-signing-input.bin")
    mutated_path = os.path.join(VERIFICATION_SCRATCH, f"{scratch_name}-mutated-input.bin")
    public_pem = os.path.join(VERIFICATION_SCRATCH, f"{scratch_name}-public.pem")
    signing_input = f"{protected}.{payload}".encode("ascii")
    with open(public_path, "wb") as handle:
        handle.write(public_der)
    with open(signature_path, "wb") as handle:
        handle.write(signature_der)
    with open(input_path, "wb") as handle:
        handle.write(signing_input)
    with open(mutated_path, "wb") as handle:
        handle.write(signing_input + b"x")
    subprocess.run(
        [
            "openssl",
            "pkey",
            "-pubin",
            "-inform",
            "DER",
            "-in",
            public_path,
            "-out",
            public_pem,
        ],
        check=True,
        capture_output=True,
    )
    valid = subprocess.run(
        [
            "openssl",
            "dgst",
            "-sha256",
            "-verify",
            public_pem,
            "-signature",
            signature_path,
            input_path,
        ],
        capture_output=True,
    ).returncode == 0
    mutated_valid = subprocess.run(
        [
            "openssl",
            "dgst",
            "-sha256",
            "-verify",
            public_pem,
            "-signature",
            signature_path,
            mutated_path,
        ],
        capture_output=True,
    ).returncode == 0
    return valid, mutated_valid


def configure_poll(response, start=0):
    control("clock_set", monotonic=start)
    return control("poll_configure", response=response, start=start)


def attempt_poll(event, now):
    control("clock_set", monotonic=now)
    return control("poll_attempt", event=event)


def setup_lease_generation(generation=7):
    control("clock_set", wall=100)
    control("identity_reset", floor=100)
    enabled = control(
        "bootstrap_enable", generation=generation, grace_seconds=50, wall=100
    )
    if enabled["status"] != "enabled":
        raise AssertionError(f"bootstrap setup failed: {enabled}")
    verifier = control(
        "bootstrap_verify",
        subject_member="member-verifier",
        verifier_member="creator",
        subject_issuer="https://issuer.example",
        subject="verifier-subject",
        verifier_issuer="https://issuer.example",
        verifier_subject="creator-subject",
        lease_seconds=900,
        wall=100,
    )
    creator = control(
        "bootstrap_verify",
        subject_member="creator",
        verifier_member="member-verifier",
        subject_issuer="https://issuer.example",
        subject="creator-subject",
        verifier_issuer="https://issuer.example",
        verifier_subject="verifier-subject",
        lease_seconds=900,
        wall=100,
    )
    if (
        verifier["status"] != "verified"
        or creator["status"] != "verified"
        or creator["bootstrap"]["state"] != "active"
    ):
        raise AssertionError(f"verifier bootstrap failed: {verifier} / {creator}")


def lease_spec(**changes):
    value = {
        "id": "lease-base",
        "eventHash": "event-base",
        "subjectMember": "member-target",
        "verifierMember": "member-verifier",
        "issuer": "https://issuer.example",
        "subject": "subject-1",
        "policyGeneration": 7,
        "verifiedAt": 100,
        "validUntil": 200,
        "evidenceExpiry": 200,
        "policyCadence": 100,
        "verifierActive": True,
        "verifierNodeActive": True,
    }
    value.update(changes)
    return value


def bootstrap_attest(
    subject_member,
    verifier_member,
    subject_identity,
    verifier_identity,
    *,
    wall=100,
    lease_seconds=300,
):
    return control(
        "bootstrap_verify",
        subject_member=subject_member,
        verifier_member=verifier_member,
        subject_issuer="https://issuer.example",
        subject=subject_identity,
        verifier_issuer="https://issuer.example",
        verifier_subject=verifier_identity,
        lease_seconds=lease_seconds,
        wall=wall,
    )


def count_idp_requests(state):
    return sum(
        row["path"].split("?", 1)[0] in {"/device", "/token"}
        for row in state["requestTrace"]
    )


def run_pre_restart():
    discovery = json.loads(HOSTILE_DISCOVERY) if HOSTILE_DISCOVERY else {}
    check("offline.hostile_ambient_curl_is_neutralized", HOSTILE_RC == "0")
    check(
        "offline.keylog_and_tls_backend_environment_is_neutralized",
        HOSTILE_KEYLOG_PRESENT == "0",
    )
    check(
        "offline.unsafe_curl_routing_override_is_rejected",
        UNSAFE_CURL_RC == "2",
    )
    check(
        "offline.unsafe_curl_tls_override_is_rejected",
        UNSAFE_TLS_CURL_RC == "2",
    )
    check(
        "offline.scheme_less_alternate_curl_url_is_rejected",
        ALTERNATE_URL_CURL_RC == "2",
    )
    control_url_result = (
        json.loads(CONTROL_URL_BODY).get("result", {})
        if CONTROL_URL_BODY_RC == "0" and CONTROL_URL_BODY
        else {}
    )
    check(
        "offline.https_text_in_control_body_is_not_misclassified_as_routing",
        CONTROL_URL_BODY_RC == "0"
        and control_url_result.get("status") == "allowed"
        and control_url_result.get("pinnedAddresses") == ["127.0.0.1"],
    )
    check("tls.discovery_issuer_honors_path", discovery.get("issuer") == BASE + "/tenant-a")
    check(
        "tls.discovery_endpoints_are_loopback_https",
        all(
            discovery.get(name, "").startswith(BASE + "/")
            for name in (
                "device_authorization_endpoint",
                "token_endpoint",
                "jwks_uri",
            )
        ),
    )
    check(
        "capability.discovery_public_auth_is_none",
        discovery.get("token_endpoint_auth_methods_supported") == ["none"],
    )
    check(
        "capability.discovery_algs_claims_scopes",
        discovery.get("id_token_signing_alg_values_supported") == ["RS256", "ES256"]
        and {"iss", "aud", "sub", "exp", "iat", "email", "groups"}
        <= set(discovery.get("claims_supported", []))
        and {"openid", "email", "profile", "groups"}
        <= set(discovery.get("scopes_supported", [])),
    )
    try:
        request("/.well-known/openid-configuration", pinned=False)
        unpinned_failed = False
    except urllib.error.URLError:
        unpinned_failed = True
    check("tls.unpinned_ephemeral_ca_fails", unpinned_failed)
    initial_trace = get_json("/_state")["requestTrace"]
    hostile_discovery_rows = [
        row
        for row in initial_trace
        if row["path"] == "/.well-known/openid-configuration"
    ]
    check(
        "offline.ambient_netrc_credentials_are_not_sent",
        bool(hostile_discovery_rows)
        and all(not row["authorizationPresent"] for row in hostile_discovery_rows)
        and "NETRC_USER_SENTINEL_T77" not in json.dumps(initial_trace),
    )

    profile_expectations = {
        "absent": [],
        "manifest_only": ["mesh.team.manifest.v1"],
        "identity_attested": [
            "mesh.team.identity_attested.v1",
            "mesh.team.manifest.v1",
        ],
    }
    for profile, expected in profile_expectations.items():
        result = control("set_capability_profile", profile=profile)
        served = get_json("/_capabilities")
        check(f"capability.{profile}.exact_feature_list", served["receiverFeatures"] == expected)
        check(f"capability.{profile}.control_matches_get", result == served)

    mandatory = sorted(
        ["mesh.team.manifest.v1", "mesh.team.identity_attested.v1"]
    )
    control("set_capability_profile", profile="identity_attested")
    missing_manifest = control(
        "feature_disposition",
        required_features=["mesh.team.identity_attested.v1"],
    )
    missing_identity = control(
        "feature_disposition", required_features=["mesh.team.manifest.v1"]
    )
    unknown = control(
        "feature_disposition",
        required_features=sorted(mandatory + ["mesh.future.v99"]),
    )
    eligible = control("feature_disposition", required_features=mandatory)
    check(
        "capability.missing_manifest_feature_quarantines",
        missing_manifest == {
            "disposition": "quarantine",
            "reason": "mesh_event_feature_contract_invalid",
            "missingMandatoryFeatures": ["mesh.team.manifest.v1"],
        },
    )
    check(
        "capability.missing_identity_feature_quarantines",
        missing_identity == {
            "disposition": "quarantine",
            "reason": "mesh_event_feature_contract_invalid",
            "missingMandatoryFeatures": ["mesh.team.identity_attested.v1"],
        },
    )
    check(
        "capability.unknown_extra_is_replayable_unsupported",
        unknown["disposition"] == "replayable_unsupported"
        and unknown["unknownFeatures"] == ["mesh.future.v99"],
    )
    check("capability.identity_profile_exact_event_is_eligible", eligible["disposition"] == "eligible")
    feature_64 = "mesh." + "x" * 59
    feature_65 = "mesh." + "x" * 60
    count_32 = sorted(mandatory + [f"mesh.extra.{index:02d}" for index in range(30)])
    count_33 = sorted(mandatory + [f"mesh.extra.{index:02d}" for index in range(31)])
    byte_64 = control(
        "feature_disposition",
        required_features=sorted(mandatory + [feature_64]),
    )
    at_count_limit = control(
        "feature_disposition", required_features=count_32
    )
    check(
        "capability.required_feature_64_bytes_is_canonical",
        byte_64["disposition"] == "replayable_unsupported"
        and byte_64["unknownFeatures"] == [feature_64],
    )
    check(
        "capability.required_feature_count_32_is_canonical",
        at_count_limit["disposition"] == "replayable_unsupported"
        and len(at_count_limit["unknownFeatures"]) == 30,
    )
    for name, malformed in (
        ("duplicate", mandatory + [mandatory[-1]]),
        ("unsorted", list(reversed(mandatory))),
        ("wrong_shape", "mesh.team.manifest.v1"),
        ("byte_65", sorted(mandatory + [feature_65])),
        ("count_33", count_33),
        ("non_mesh_namespace", sorted(mandatory + ["vendor.feature.v1"])),
    ):
        invalid_contract = control(
            "feature_disposition", required_features=malformed
        )
        check(
            f"capability.required_features_{name}_quarantines",
            invalid_contract["disposition"] == "quarantine"
            and invalid_contract["reason"]
            == "mesh_event_feature_contract_invalid",
        )
    control("set_capability_profile", profile="manifest_only")
    check(
        "capability.manifest_only_identity_event_is_unsupported",
        control("feature_disposition", required_features=mandatory)["disposition"]
        == "replayable_unsupported",
    )
    control("set_capability_profile", profile="absent")
    check(
        "capability.absent_profile_does_not_dispatch",
        control("feature_disposition", required_features=mandatory)["disposition"]
        == "no_dispatch",
    )

    for method in ("client_secret_post", "client_secret_basic"):
        control("set_secret_required", required=True, method=method)
        secret_discovery = get_json("/.well-known/openid-configuration")
        secret_device, _ = post_device()
        status, denied, _, denied_raw = post_token(secret_device["device_code"])
        if method == "client_secret_post":
            accepted_status, accepted, _, accepted_raw = post_token(
                secret_device["device_code"], "CLIENT_SECRET_SENTINEL_T77"
            )
        else:
            form = urllib.parse.urlencode(
                {
                    "grant_type": "urn:ietf:params:oauth:grant-type:device_code",
                    "device_code": secret_device["device_code"],
                }
            )
            basic = base64.b64encode(
                b"client:CLIENT_SECRET_SENTINEL_T77"
            ).decode("ascii")
            accepted_status, accepted, _, accepted_raw = request(
                "/token",
                "POST",
                form,
                {
                    "Content-Type": "application/x-www-form-urlencoded",
                    "Authorization": f"Basic {basic}",
                },
            )
        check(
            f"capability.secret_only_provider_advertises_{method}",
            secret_discovery["token_endpoint_auth_methods_supported"] == [method],
        )
        check(
            f"capability.secret_only_provider_rejects_missing_{method}",
            status == 401 and denied == {"error": "invalid_client"},
        )
        check(
            f"capability.secret_only_provider_accepts_supplied_{method}",
            accepted_status == 400
            and accepted == {"error": "authorization_pending"},
        )
        check(
            f"capability.provider_never_distributes_{method}_secret",
            b"CLIENT_SECRET_SENTINEL_T77" not in denied_raw
            and b"CLIENT_SECRET_SENTINEL_T77" not in accepted_raw
            and "client_secret" not in secret_discovery,
        )
    control("set_secret_required", required=False)

    network_cases = [
        (
            "https_required",
            {
                "url": "http://127.0.0.1/device",
                "validation_ips": ["127.0.0.1"],
                "private_approved": True,
            },
            "https_required",
        ),
        (
            "userinfo_forbidden",
            {
                "url": "https://user:password@127.0.0.1/device",
                "validation_ips": ["127.0.0.1"],
                "private_approved": True,
            },
            "userinfo_forbidden",
        ),
        (
            "fragment_forbidden",
            {
                "url": BASE + "/device#fragment",
                "validation_ips": ["127.0.0.1"],
                "private_approved": True,
            },
            "fragment_forbidden",
        ),
        (
            "private_address_requires_approval",
            {
                "url": BASE + "/device",
                "validation_ips": ["127.0.0.1"],
                "private_approved": False,
            },
            "private_address_unapproved",
        ),
        (
            "invalid_dns_address_even_with_private_approval",
            {
                "url": BASE + "/device",
                "validation_ips": ["not-an-ip"],
                "private_approved": True,
            },
            "invalid_dns_address",
        ),
        (
            "dns_rebinding",
            {
                "url": BASE + "/device",
                "validation_ips": ["127.0.0.1"],
                "presentation_ips": ["127.0.0.2"],
                "private_approved": True,
            },
            "dns_rebinding",
        ),
        (
            "cross_origin_redirect",
            {
                "url": BASE + "/device",
                "validation_ips": ["127.0.0.1"],
                "private_approved": True,
                "redirects": ["https://example.invalid/next"],
            },
            "cross_origin_redirect",
        ),
        (
            "credential_post_redirect",
            {
                "url": BASE + "/token",
                "validation_ips": ["127.0.0.1"],
                "private_approved": True,
                "credentialed_post": True,
                "redirects": [BASE + "/token-2"],
            },
            "credential_post_redirect",
        ),
    ]
    for name, spec, reason in network_cases:
        result = control("network_evaluate", spec=spec)
        check(
            f"network.oracle.{name}",
            result == {"status": "rejected", "reason": reason},
        )
    allowed_network = control(
        "network_evaluate",
        spec={
            "url": BASE + "/device",
            "validation_ips": ["127.0.0.1"],
            "presentation_ips": ["127.0.0.1"],
            "private_approved": True,
            "redirects": [BASE + "/next"],
        },
    )
    check(
        "network.oracle.approved_pinned_same_origin_is_allowed",
        allowed_network["status"] == "allowed"
        and allowed_network["pinnedAddresses"] == ["127.0.0.1"]
        and allowed_network["redirectCount"] == 1,
    )

    jwks_profile_checks = {
        "rsa_1024": lambda keys: len(b64decode(keys[0]["n"])) == 128,
        "rsa_bad_exponent": lambda keys: int.from_bytes(
            b64decode(keys[0]["e"]), "big"
        )
        == 3,
        "ec_wrong_curve": lambda keys: keys[0]["crv"] == "P-384"
        and len(b64decode(keys[0]["x"])) == 48,
        "missing_kid": lambda keys: all("kid" not in key for key in keys),
        "duplicate_same_kid": lambda keys: len(keys) == 2 and keys[0] == keys[1],
        "ambiguous_same_kid": lambda keys: len(keys) == 2
        and keys[0]["kid"] == keys[1]["kid"]
        and keys[0]["kty"] == keys[1]["kty"] == "RSA"
        and keys[0]["alg"] == keys[1]["alg"] == "RS256"
        and keys[0]["n"] != keys[1]["n"],
        "metadata_mismatch": lambda keys: all(
            key.get("use") == "enc"
            and key.get("key_ops") == ["encrypt"]
            and key.get("alg") == "HS256"
            for key in keys
        ),
        "zero_eligible": lambda keys: all(
            key.get("key_ops") == ["deriveKey"] for key in keys
        ),
    }
    for profile, predicate in jwks_profile_checks.items():
        control("set_jwks_profile", profile=profile, mode="fresh")
        served = get_json("/jwks")
        check(f"jwks.live_profile.{profile}", predicate(served["keys"]))
        if profile in {
            "rsa_1024",
            "rsa_bad_exponent",
            "ec_wrong_curve",
            "ambiguous_same_kid",
        }:
            control(
                "replace_flow",
                flow={"initial_status": "granted", "expires_in": 900},
            )
            profile_device, _ = post_device()
            profile_status, profile_body, _, _ = post_token(
                profile_device["device_code"]
            )
            profile_token = profile_body.get("id_token", "")
            if profile in {"rsa_1024", "rsa_bad_exponent"}:
                paired = verify_rs256(profile_token, served)
            elif profile == "ec_wrong_curve":
                paired = (
                    len(decode_jwt(profile_token)[4]) == 96
                    and verify_es256(profile_token, served)[0]
                )
            else:
                paired = (
                    verify_rs256_with_jwk(profile_token, served["keys"][0])
                    and not verify_rs256_with_jwk(
                        profile_token, served["keys"][1]
                    )
                )
            check(
                f"jwks.live_profile.{profile}_has_paired_exact_signature",
                profile_status == 200 and paired,
            )
    control("set_jwks_profile", profile="normal", mode="fresh")
    retained_before = get_json("/jwks")
    control("rotate_keys", retire_previous=False)
    retained_after = get_json("/jwks")
    check(
        "jwks.rotation_retains_previous_generation_when_requested",
        {"rs1", "es1"} <= {key.get("kid") for key in retained_before["keys"]}
        and {"rs1", "es1", "rs2", "es2"}
        <= {key.get("kid") for key in retained_after["keys"]},
    )
    status, _, headers, _ = request("/jwks")
    stale_validator = headers.get("ETag") or headers.get("Etag")
    control("rotate_keys", retire_previous=True)
    control("set_jwks_profile", profile="normal", mode="stale_304")
    stale_status, _, _, stale_raw = request(
        "/jwks", headers={"If-None-Match": stale_validator}
    )
    retired = get_json("/jwks")
    check(
        "jwks.stale_validator_can_present_bodyless_304",
        status == 200 and stale_status == 304 and stale_raw == b"",
    )
    check(
        "jwks.rotation_can_retire_immediately_previous_generation",
        not {"rs2", "es2"}.intersection(
            {key.get("kid") for key in retired["keys"]}
        ),
    )
    control("set_jwks_profile", profile="normal", mode="fresh")
    normal_attack_jwks = get_json("/jwks")

    stimulus_index = get_json("/_stimulus")["names"]
    check(
        "adversarial.live_stimulus_index_is_sorted_unique",
        stimulus_index == sorted(set(stimulus_index)) and len(stimulus_index) >= 27,
    )
    url_stimuli = {
        "url_insecure_http": "http://127.0.0.1/device",
        "url_userinfo": "https://user:password@127.0.0.1/device",
        "url_fragment": BASE + "/device#fragment",
        "url_reserved": "https://192.0.2.1/device",
    }
    for name, expected_url in url_stimuli.items():
        served_discovery = get_json(f"/_stimulus/{name}")
        check(
            f"adversarial.discovery_url.{name}",
            served_discovery["issuer"] == BASE + "/tenant-a"
            and served_discovery["device_authorization_endpoint"] == expected_url
            and {"token_endpoint", "jwks_uri"} <= set(served_discovery),
        )
    duplicate_expectations = {
        "duplicate_discovery": b'"issuer"',
        "duplicate_device": b'"expires_in"',
        "duplicate_token": b'"id_token"',
        "duplicate_jwks": b'"keys"',
        "duplicate_jwk": b'"kid"',
    }
    expected_current_rsa_kid = [
        key["kid"] for key in normal_attack_jwks["keys"] if key["kty"] == "RSA"
    ][-1]
    for name, member in duplicate_expectations.items():
        stimulus_status, _, _, raw = request(f"/_stimulus/{name}")
        parsed_duplicate = json.loads(raw)
        surrounding_valid = {
            "duplicate_discovery": lambda value: {
                "issuer",
                "device_authorization_endpoint",
                "token_endpoint",
                "jwks_uri",
            }
            <= set(value)
            and value["issuer"] == BASE + "/tenant-a",
            "duplicate_device": lambda value: {
                "device_code",
                "user_code",
                "verification_uri",
                "expires_in",
                "interval",
            }
            <= set(value)
            and value["expires_in"] == 900
            and value["interval"] == 5,
            "duplicate_token": lambda value: {
                "access_token",
                "token_type",
                "id_token",
            }
            <= set(value)
            and verify_rs256(value["id_token"], normal_attack_jwks),
            "duplicate_jwks": lambda value: bool(value.get("keys"))
            and {"kty", "kid", "n", "e"} <= set(value["keys"][0]),
            "duplicate_jwk": lambda value: bool(value.get("keys"))
            and {"kty", "kid", "n", "e"} <= set(value["keys"][0])
            and value["keys"][0]["kid"] == expected_current_rsa_kid,
        }[name](parsed_duplicate)
        check(
            f"adversarial.raw_json.{name}",
            stimulus_status == 200
            and raw.count(member) == 2
            and surrounding_valid,
        )
    duplicate_compact_tokens = {}
    for name, member in (
        ("duplicate_jose_header", b'"alg"'),
        ("duplicate_claims", b'"sub"'),
    ):
        _, _, _, raw_token = request(f"/_stimulus/{name}")
        duplicate_compact_tokens[name] = raw_token.decode("ascii")
        segment = raw_token.split(b".")[0 if name.endswith("header") else 1]
        decoded_segment = base64.urlsafe_b64decode(
            segment + b"=" * (-len(segment) % 4)
        )
        check(
            f"adversarial.raw_json.{name}",
            decoded_segment.count(member) == 2
            and verify_rs256(duplicate_compact_tokens[name], normal_attack_jwks),
        )
    _, _, _, depth_raw = request("/_stimulus/json_depth_65")
    _, _, _, oversize_raw = request("/_stimulus/json_oversize")
    check(
        "adversarial.json_depth_and_byte_caps_have_live_overlimit_inputs",
        json.loads(depth_raw)["issuer"] == BASE + "/tenant-a"
        and depth_raw.count(b"[") >= 65
        and json.loads(oversize_raw)["issuer"] == BASE + "/tenant-a"
        and len(oversize_raw) > 1024 * 1024,
    )
    compact_shapes = {}
    for name in (
        "compact_two_segments",
        "compact_four_segments",
        "compact_empty_segment",
        "compact_whitespace",
        "compact_padded",
        "compact_standard_base64",
    ):
        _, _, _, compact_shapes[name] = request(f"/_stimulus/{name}")
    check(
        "adversarial.compact_and_base64url_shapes_are_exact",
        compact_shapes["compact_two_segments"].count(b".") == 1
        and compact_shapes["compact_four_segments"].count(b".") == 3
        and b".." in compact_shapes["compact_empty_segment"]
        and compact_shapes["compact_whitespace"] != compact_shapes[
            "compact_whitespace"
        ].strip()
        and b"=" in compact_shapes["compact_padded"]
        and (b"+" in compact_shapes["compact_standard_base64"]
             or b"/" in compact_shapes["compact_standard_base64"]),
    )
    check(
        "adversarial.noncanonical_base64_variants_sign_exact_transmitted_input",
        verify_rs256(
            compact_shapes["compact_padded"].decode("ascii"), normal_attack_jwks
        )
        and verify_rs256(
            compact_shapes["compact_standard_base64"].decode("ascii"),
            normal_attack_jwks,
        ),
    )
    header_names = (
        "header_alg_none",
        "header_alg_confusion",
        "header_unknown_crit",
        "header_jku",
        "header_x5u",
        "header_jwk",
        "header_x5c",
        "header_missing_kid",
    )
    served_headers = {}
    served_header_tokens = {}
    for name in header_names:
        _, _, _, raw_token = request(f"/_stimulus/{name}")
        served_header_tokens[name] = raw_token.decode("ascii")
        served_headers[name] = json.loads(b64decode(raw_token.decode().split(".")[0]))
    _, _, confusion_protected, confusion_payload, confusion_signature = decode_jwt(
        served_header_tokens["header_alg_confusion"]
    )
    confusion_secret = b64decode(
        [key for key in normal_attack_jwks["keys"] if key["kty"] == "RSA"][-1]["n"]
    )
    confusion_signature_valid = hmac.compare_digest(
        confusion_signature,
        hmac.new(
            confusion_secret,
            f"{confusion_protected}.{confusion_payload}".encode("ascii"),
            hashlib.sha256,
        ).digest(),
    )
    check(
        "adversarial.jose_header_attack_family_is_live",
        served_headers["header_alg_none"]["alg"] == "none"
        and served_header_tokens["header_alg_none"].endswith(".")
        and served_headers["header_alg_confusion"]["alg"] == "HS256"
        and confusion_signature_valid
        and served_headers["header_unknown_crit"]["crit"] == ["future"]
        and all(
            key in served_headers[f"header_{key}"]
            for key in ("jku", "x5u", "jwk", "x5c")
        )
        and "kid" not in served_headers["header_missing_kid"],
    )
    current_rsa_jwk = [
        key for key in normal_attack_jwks["keys"] if key["kty"] == "RSA"
    ][-1]
    check(
        "adversarial.forbidden_headers_have_valid_exact_rsa_signature",
        all(
            verify_rs256(served_header_tokens[name], normal_attack_jwks)
            for name in (
                "header_unknown_crit",
                "header_jku",
                "header_x5u",
                "header_jwk",
                "header_x5c",
            )
        )
        and verify_rs256_with_jwk(
            served_header_tokens["header_missing_kid"], current_rsa_jwk
        ),
    )
    same_redirect_status, _, same_redirect_headers, _ = request(
        "/_trap/redirect/same_origin", follow_redirects=False
    )
    cross_redirect_status, _, cross_redirect_headers, _ = request(
        "/_trap/redirect/cross_origin", follow_redirects=False
    )
    _, _, _, trap_oversize = request("/_trap/oversize")
    diagnostic_status, diagnostic, _, _ = request(
        "/_trap/redaction_diagnostic"
    )
    parsed_base = urllib.parse.urlsplit(BASE)
    trap_context = ssl.create_default_context(cafile=CA_FILE)
    stall_connection = http.client.HTTPSConnection(
        parsed_base.hostname,
        parsed_base.port,
        context=trap_context,
        timeout=0.2,
    )
    stall_timed_out = False
    try:
        stall_connection.request(
            "POST",
            "/_trap/stall",
            "grant_type=urn:ietf:params:oauth:grant-type:device_code",
            {"Content-Type": "application/x-www-form-urlencoded"},
        )
        stall_response = stall_connection.getresponse()
        stall_response.read(1)
    except (TimeoutError, socket.timeout):
        stall_timed_out = True
    finally:
        stall_connection.close()
    partial_connection = http.client.HTTPSConnection(
        parsed_base.hostname,
        parsed_base.port,
        context=trap_context,
        timeout=0.2,
    )
    expected_partial = b'{"access_token":"PARTIAL_TOKEN_SENTINEL_T77'
    partial_timed_out = False
    partial_bytes = b""
    try:
        partial_connection.request(
            "POST",
            "/_trap/partial_token",
            "grant_type=urn:ietf:params:oauth:grant-type:device_code",
            {"Content-Type": "application/x-www-form-urlencoded"},
        )
        partial_response = partial_connection.getresponse()
        partial_bytes = partial_response.read(len(expected_partial))
        partial_response.read(1)
    except (TimeoutError, socket.timeout):
        partial_timed_out = True
    finally:
        partial_connection.close()
    check(
        "network.live_redirect_traps_expose_same_and_cross_origin_hops",
        same_redirect_status == 302
        and same_redirect_headers.get("Location") == BASE + "/jwks"
        and cross_redirect_status == 302
        and cross_redirect_headers.get("Location")
        == "https://127.0.0.1:1/credential-sink",
    )
    check(
        "network.live_output_cap_and_redaction_diagnostic_traps",
        len(trap_oversize) == 1024 * 1024 + 1
        and diagnostic_status == 500
        and diagnostic["diagnostic"] == "ACCESS_TOKEN_SENTINEL_T77",
    )
    check(
        "network.live_stall_and_partial_token_timeout_traps",
        stall_timed_out
        and partial_timed_out
        and partial_bytes == expected_partial,
    )
    output_at_cap = control(
        "output_budget_evaluate", stdout_bytes=32768, stderr_bytes=32768
    )
    output_over_cap = control(
        "output_budget_evaluate", stdout_bytes=32768, stderr_bytes=32769
    )
    output_overflow = control(
        "output_budget_evaluate", stdout_bytes=MAX_U64, stderr_bytes=1
    )
    check(
        "network.stdout_stderr_aggregate_cap_boundaries_are_checked",
        output_at_cap["status"] == "allowed"
        and output_at_cap["aggregateBytes"] == 65536
        and output_over_cap["status"] == "terminated"
        and output_over_cap["reason"] == "aggregate_output_cap_exceeded"
        and output_over_cap["reapRequired"]
        and output_overflow["reason"] == "output_size_invalid_or_overflow",
    )

    raw_flow_cases = [
        (
            "positive_expires",
            {"initial_status": "authorization_pending", "expires_in": 900},
            lambda body: body.get("expires_in") == 900,
        ),
        (
            "missing_expires",
            {"initial_status": "authorization_pending", "device_response_omit": ["expires_in"]},
            lambda body: "expires_in" not in body,
        ),
        (
            "null_expires",
            {"initial_status": "authorization_pending", "expires_in": None},
            lambda body: "expires_in" in body and body["expires_in"] is None,
        ),
        (
            "zero_expires",
            {"initial_status": "authorization_pending", "expires_in": 0},
            lambda body: body.get("expires_in") == 0,
        ),
        (
            "overflow_expires",
            {"initial_status": "authorization_pending", "expires_in": MAX_U64 + 1},
            lambda body: body.get("expires_in") == MAX_U64 + 1,
        ),
        (
            "omitted_interval",
            {"initial_status": "authorization_pending", "expires_in": 900},
            lambda body: "interval" not in body,
        ),
        (
            "positive_interval",
            {
                "initial_status": "authorization_pending",
                "expires_in": 900,
                "interval": 7,
            },
            lambda body: body.get("interval") == 7,
        ),
        (
            "null_interval",
            {"initial_status": "authorization_pending", "expires_in": 900, "interval": None},
            lambda body: "interval" in body and body["interval"] is None,
        ),
        (
            "zero_interval",
            {"initial_status": "authorization_pending", "expires_in": 900, "interval": 0},
            lambda body: body.get("interval") == 0,
        ),
        (
            "overflow_interval",
            {
                "initial_status": "authorization_pending",
                "expires_in": 900,
                "interval": MAX_U64 + 1,
            },
            lambda body: body.get("interval") == MAX_U64 + 1,
        ),
    ]
    for name, flow, predicate in raw_flow_cases:
        control("replace_flow", flow=flow)
        body, _ = post_device()
        check(f"numeric.live_device_stimulus.{name}", predicate(body))

    live_terminal_cases = {
        "authorization_pending": "authorization_pending",
        "slow_down": "slow_down",
        "access_denied": "access_denied",
        "expired_token": "expired_token",
    }
    for initial_status, expected_error in live_terminal_cases.items():
        control(
            "replace_flow",
            flow={
                "initial_status": initial_status,
                "expires_in": 900,
                "interval": 5,
            },
        )
        live_device, _ = post_device()
        live_status, live_body, _, _ = post_token(live_device["device_code"])
        check(
            f"poll.live_token_endpoint.{initial_status}",
            live_status == 400 and live_body == {"error": expected_error},
        )

    control("poll_reset")
    unconfigured_poll = attempt_poll("authorization_pending", 0)
    check(
        "poll.unconfigured_attempt_is_deterministic_invalid_not_exception",
        unconfigured_poll["status"] == "invalid"
        and unconfigured_poll["validationError"] == "poll_not_configured",
    )
    configure_poll({"expires_in": 900, "interval": 1})
    overlarge_repeat = control(
        "poll_repeat_legal", event="authorization_pending", count=301
    )
    check(
        "poll.bulk_repeat_is_bounded",
        overlarge_repeat["status"] == "terminal_error"
        and overlarge_repeat["validationError"] == "repeat_count_out_of_range",
    )

    invalid_expires = [
        ("missing", {}, "expires_in_missing"),
        ("null", {"expires_in": None}, "expires_in_missing"),
        ("zero", {"expires_in": 0}, "expires_in_not_positive"),
        ("negative", {"expires_in": -1}, "expires_in_not_positive"),
        ("non_integer", {"expires_in": "5"}, "expires_in_not_integer"),
        ("overflow", {"expires_in": MAX_U64 + 1}, "expires_in_overflow"),
    ]
    for name, response, reason in invalid_expires:
        result = configure_poll(response)
        check(
            f"numeric.expires.{name}.terminates",
            result["status"] == "invalid"
            and result["validationError"] == reason
            and result["restartRequired"],
        )
    positive = configure_poll({"expires_in": 10})
    check(
        "numeric.expires.positive_is_accepted",
        positive["validationError"] is None and positive["providerDeadline"] == 10,
    )

    interval_cases = [
        ("null", None, "interval_not_integer"),
        ("zero", 0, "interval_not_positive"),
        ("negative", -1, "interval_not_positive"),
        ("non_integer", "5", "interval_not_integer"),
        ("overflow", MAX_U64 + 1, "interval_overflow"),
    ]
    omitted = configure_poll({"expires_in": 20})
    explicit = configure_poll({"expires_in": 20, "interval": 7})
    check("numeric.interval.omission_defaults_exactly_five", omitted["interval"] == 5)
    check("numeric.interval.explicit_positive_is_accepted", explicit["interval"] == 7)
    for name, value, reason in interval_cases:
        result = configure_poll({"expires_in": 20, "interval": value})
        check(
            f"numeric.interval.{name}.terminates",
            result["status"] == "invalid" and result["validationError"] == reason,
        )
    overflow = configure_poll({"expires_in": 2, "interval": 1}, MAX_U64 - 1)
    check("numeric.checked_deadline_overflow_terminates", overflow["validationError"] == "deadline_overflow")

    configure_poll({"expires_in": 100, "interval": 5})
    early = attempt_poll("authorization_pending", 4)
    slow_one = attempt_poll("slow_down", 5)
    slow_two = attempt_poll("slow_down", 15)
    early_after_slow = attempt_poll("authorization_pending", 29)
    on_time = attempt_poll("authorization_pending", 30)
    check(
        "poll.early_request_is_observed_not_sent",
        early["requestCount"] == 0
        and early["trace"][-1]["event"] == "early_poll"
        and not early["trace"][-1]["requestSent"],
    )
    check("poll.slow_down_first_adds_five", slow_one["interval"] == 10 and slow_one["nextPollAt"] == 15)
    check("poll.slow_down_is_cumulative", slow_two["interval"] == 15 and slow_two["nextPollAt"] == 30)
    check("poll.new_interval_blocks_early_request", early_after_slow["requestCount"] == 2)
    check("poll.on_time_after_slow_down_is_sent", on_time["requestCount"] == 3 and on_time["nextPollAt"] == 45)

    configure_poll({"expires_in": 100, "interval": 1})
    timeout_rows = []
    for at in (1, 2, 4, 8):
        timeout_rows.append(attempt_poll("timeout", at))
    check(
        "poll.timeout_backoff_is_checked_exponential",
        [row["timeoutBackoff"] for row in timeout_rows] == [1, 2, 4, 8]
        and [row["nextPollAt"] for row in timeout_rows] == [2, 4, 8, 16],
    )

    terminal_expectations = {
        "granted": "success",
        "access_denied": "terminal_error",
        "expired_token": "expired",
        "malformed": "terminal_error",
        "unknown_error": "terminal_error",
        "cancelled": "cancelled",
    }
    for event, expected_status in terminal_expectations.items():
        configure_poll({"expires_in": 20, "interval": 5})
        result = attempt_poll(event, 5)
        count = result["requestCount"]
        repeated = attempt_poll("authorization_pending", 10)
        check(
            f"poll.terminal.{event}.requires_explicit_restart",
            result["status"] == expected_status
            and repeated["status"] == expected_status
            and repeated["requestCount"] == count,
        )

    configure_poll({"expires_in": 10, "interval": 5})
    provider_expired = attempt_poll("authorization_pending", 10)
    configure_poll({"expires_in": 5000, "interval": 5})
    local_expired = attempt_poll("authorization_pending", 1800)
    configure_poll({"expires_in": 1800, "interval": 1})
    budget_expired = control(
        "poll_repeat_legal", event="authorization_pending", count=300
    )
    after_budget = attempt_poll("authorization_pending", 9999)
    configure_poll({"expires_in": 1800, "interval": 1})
    control("poll_repeat_legal", event="authorization_pending", count=299)
    slow_down_at_ceiling = attempt_poll("slow_down", 300)
    configure_poll({"expires_in": 1800, "interval": 1})
    control("poll_repeat_legal", event="authorization_pending", count=299)
    timeout_at_ceiling = attempt_poll("timeout", 300)
    check(
        "poll.provider_deadline_has_common_expiry_class",
        provider_expired["status"] == "expired"
        and provider_expired["expiryReason"] == "provider_deadline"
        and provider_expired["restartRequired"],
    )
    check(
        "poll.local_1800_deadline_has_distinct_reason",
        local_expired["status"] == "expired"
        and local_expired["expiryReason"] == "local_deadline"
        and local_expired["restartRequired"],
    )
    check(
        "poll.request_300_ceiling_has_no_301st_request",
        budget_expired["status"] == "expired"
        and budget_expired["expiryReason"] == "request_budget"
        and budget_expired["requestCount"] == 300
        and after_budget["requestCount"] == 300,
    )
    check(
        "poll.request_300_ceiling_is_independent_of_continuing_response",
        slow_down_at_ceiling["status"] == "expired"
        and slow_down_at_ceiling["expiryReason"] == "request_budget"
        and slow_down_at_ceiling["requestCount"] == 300
        and timeout_at_ceiling["status"] == "expired"
        and timeout_at_ceiling["expiryReason"] == "request_budget"
        and timeout_at_ceiling["requestCount"] == 300,
    )
    too_long = configure_poll({"expires_in": 4, "interval": 5})
    check(
        "poll.wait_longer_than_remaining_expires_without_shortening",
        too_long["status"] == "expired"
        and too_long["expiryReason"] == "provider_deadline",
    )
    configure_poll({"expires_in": 6, "interval": 1})
    for at in (1, 2, 4):
        timeout_expired = attempt_poll("timeout", at)
    check(
        "poll.timeout_backoff_past_remaining_lifetime_expires",
        timeout_expired["status"] == "expired"
        and timeout_expired["expiryReason"] == "provider_deadline",
    )

    control("clock_set", wall=1000, monotonic=100)
    before_rollback = configure_poll({"expires_in": 10, "interval": 5}, start=100)
    control("clock_set", wall=1, monotonic=100)
    after_rollback = get_json("/_state")["pollOracle"]
    wall_rollback_expired = attempt_poll("authorization_pending", 110)
    check(
        "time.poll_deadline_is_monotonic_not_wall",
        before_rollback["deadline"] == after_rollback["deadline"] == 110
        and wall_rollback_expired["status"] == "expired",
    )

    mutating_paths = [
        "token_verify",
        "idp_set",
        "grant",
        "serve",
        "sync_import",
        "steward",
        "revalidate",
    ]
    for path in mutating_paths:
        control("identity_reset", floor=100)
        authorized = control("identity_path", path=path, wall=101)
        authorized_state = get_json("/_state")["identityOracle"]
        check(
            f"time.path.{path}.advances_floor_before_effect",
            authorized["status"] == "authorized"
            and authorized_state["floor"] == 101
            and authorized_state["auditCount"] == 1
            and authorized_state["effectCount"] == 1,
        )
        control("identity_reset", floor=100)
        rollback = control("identity_path", path=path, wall=99)
        rollback_state = get_json("/_state")["identityOracle"]
        check(
            f"time.path.{path}.rollback_is_atomic",
            rollback["status"] == "team_identity_clock_rollback"
            and rollback_state["floor"] == 100
            and rollback_state["auditCount"] == 0
            and rollback_state["effectCount"] == 0,
        )

    control("identity_reset", floor=100)
    stable_digest = get_json("/_state")["identityOracleSha256"]
    for path in ("status", "doctor", "activity", "audit"):
        result = control("identity_path", path=path, wall=200)
        check(
            f"time.read_only.{path}.does_not_persist_floor_or_audit",
            result["status"] == "read_only"
            and result["effectiveTime"] == 200
            and result["persistedUnchanged"]
            and result["identityOracleSha256"] == stable_digest,
        )
    observation = control(
        "observe_time_evidence",
        peer_produced_at=999999,
        token_timestamp=999998,
        attestation_timestamp=999997,
        receipt_time=999996,
    )
    check(
        "time.untrusted_timestamps_never_advance_floor",
        observation["status"] == "ignored_for_floor"
        and observation["floor"] == 100
        and observation["identityOracleSha256"] == stable_digest,
    )

    control("clock_set", wall=1000, monotonic=50)
    control("identity_reset", floor=1000)
    advanced = control("identity_path", path="token_verify", wall=1100)
    control("clock_set", wall=1000, monotonic=50)
    control(
        "replace_flow",
        flow={"initial_status": "authorization_pending", "expires_in": 900},
    )
    restart_device, _ = post_device()
    restart_frame = get_json("/_frame")
    configure_poll({"expires_in": 900}, start=50)
    restart_state = get_json("/_state")
    restart_expectation = {
        "processGeneration": restart_state["processGeneration"],
        "deviceCode": restart_device["device_code"],
        "ceremonyId": restart_frame["frame"]["ceremonyId"],
        "floor": advanced["floor"],
        "oracleDigest": restart_state["identityOracleSha256"],
    }
    with open(RESTART_EXPECTATION, "w", encoding="utf-8") as handle:
        json.dump(restart_expectation, handle, sort_keys=True, separators=(",", ":"))
        handle.write("\n")
    check(
        "lifecycle.pre_restart_has_only_pending_ceremony",
        restart_state["identityOracle"]["outerState"] == "identity_pending"
        and len(restart_state["devices"]) >= 1
        and restart_state["artifact"]["present"] is False,
    )


def run_post_restart():
    with open(RESTART_EXPECTATION, "r", encoding="utf-8") as handle:
        expected = json.load(handle)
    state = get_json("/_state")
    check(
        "lifecycle.actual_restart_preserves_floor",
        state["identityOracle"]["floor"] == expected["floor"]
        and state["identityOracleSha256"] == expected["oracleDigest"],
    )
    check(
        "lifecycle.actual_restart_drops_device_poll_and_frame_state",
        state["devices"] == {}
        and state["pollOracle"]["status"] == "unconfigured"
        and state["frame"]["present"] is False,
    )
    check(
        "lifecycle.actual_restart_keeps_only_nonsecret_pending_outer_state",
        state["identityOracle"]["outerState"] == "identity_pending"
        and state["artifact"]["present"] is False,
    )
    check(
        "lifecycle.process_generation_changes",
        state["processGeneration"] == expected["processGeneration"] + 1,
    )
    blocked = control("identity_path", path="token_verify", wall=1000)
    check(
        "time.lower_wall_after_restart_remains_blocked",
        blocked["status"] == "team_identity_clock_rollback"
        and blocked["floor"] == expected["floor"],
    )
    old_status, old_body, _, _ = post_token(expected["deviceCode"])
    check(
        "lifecycle.old_device_code_cannot_be_reused",
        old_status == 400 and old_body == {"error": "invalid_grant"},
    )
    fresh_device, _ = post_device()
    fresh_frame = get_json("/_frame")
    check(
        "lifecycle.fresh_explicit_ceremony_has_new_ids",
        fresh_device["device_code"] != expected["deviceCode"]
        and fresh_frame["frame"]["ceremonyId"] != expected["ceremonyId"],
    )

    control("clock_set", wall=1200, monotonic=0)
    control(
        "replace_flow",
        flow={
            "initial_status": "authorization_pending",
            "expires_in": 900,
            "frame_ttl": 30,
        },
    )
    privacy_device, privacy_device_raw = post_device()
    pending_status, pending_body, _, pending_raw = post_token(
        privacy_device["device_code"]
    )
    control(
        "set_status", status="granted", user_code=privacy_device["user_code"]
    )
    granted_status, granted_body, _, granted_raw = post_token(
        privacy_device["device_code"]
    )
    check(
        "privacy.live_flow_reaches_pending_then_granted",
        pending_status == 400
        and pending_body == {"error": "authorization_pending"}
        and granted_status == 200,
    )
    id_token = granted_body["id_token"]
    header, claims, _, _, _ = decode_jwt(id_token)
    jwks_status, jwks, jwks_headers, _ = request("/jwks")
    etag = jwks_headers.get("ETag") or jwks_headers.get("Etag")
    not_modified_status, _, _, not_modified_raw = request(
        "/jwks", headers={"If-None-Match": etag}
    )
    check("jose.rs256_exact_signing_input_verifies", jwks_status == 200 and verify_rs256(id_token, jwks))
    check(
        "jose.jwks_validator_returns_bodyless_304",
        bool(etag) and not_modified_status == 304 and not not_modified_raw,
    )
    check(
        "jose.issuer_path_is_in_signed_claims",
        claims["iss"] == BASE + "/tenant-a" and header["alg"] == "RS256",
    )

    raw_source = b"\n".join(
        [
            privacy_device_raw,
            pending_raw,
            granted_raw,
            json.dumps(claims, sort_keys=True).encode(),
        ]
    )
    required_source_sentinels = {
        "subject": "SUBJECT_SENTINEL_T77",
        "preview_email": "PREVIEW_EMAIL_SENTINEL_T77@example.test",
        "allowed_group": "ALLOWED_ALPHA_SENTINEL_T77",
        "second_allowed_group": "ALLOWED_BETA_SENTINEL_T77",
        "full_group": "FULL_GROUP_SENTINEL_T77",
        "unrelated_pii": "UNRELATED_PII_SENTINEL_T77",
        "nested_pii": "PRIVATE_STREET_SENTINEL_T77",
        "access_token": "ACCESS_TOKEN_SENTINEL_T77",
        "refresh_token": "REFRESH_TOKEN_SENTINEL_T77",
        "device_code": privacy_device["device_code"],
        "user_code": privacy_device["user_code"],
        "verification_url": privacy_device["verification_uri_complete"],
        "poll_state": "authorization_pending",
        "raw_id_token": id_token,
    }
    for name, sentinel in required_source_sentinels.items():
        check(
            f"privacy.source_contains.{name}", sentinel.encode("utf-8") in raw_source
        )

    artifact_response = get_json("/_artifact")
    artifact = artifact_response["artifact"]
    artifact_path = os.path.join(STATE_DIR, "identity-artifact.json")
    with open(artifact_path, "rb") as handle:
        artifact_bytes = handle.read()
    views = get_json("/_artifact_views")
    views_bytes = json.dumps(
        views, sort_keys=True, separators=(",", ":")
    ).encode("utf-8")
    check(
        "privacy.artifact_exact_top_level_allowlist",
        set(artifact) == {
            "schema",
            "subject",
            "previewEmail",
            "groupDecision",
            "provenance",
        },
    )
    check(
        "privacy.artifact_retains_minimal_approved_identity",
        artifact["subject"] == "SUBJECT_SENTINEL_T77"
        and artifact["previewEmail"] == "PREVIEW_EMAIL_SENTINEL_T77@example.test"
        and artifact["groupDecision"]["matchedAllowedGroups"]
        == ["ALLOWED_ALPHA_SENTINEL_T77"]
        and artifact["groupDecision"]["allowed"]
        and artifact["groupDecision"]["truncated"],
    )
    check(
        "privacy.artifact_provenance_is_canonical_not_bearer_data",
        set(artifact["provenance"])
        == {
            "issuer",
            "clientId",
            "tokenHash",
            "replayClaimHash",
            "kid",
            "jwkThumbprint",
            "alg",
            "verifiedAt",
            "expiresAt",
        }
        and artifact["provenance"]["tokenHash"]
        == hashlib.sha256(id_token.encode("ascii")).hexdigest(),
    )
    forbidden_artifact_sentinels = {
        name: sentinel
        for name, sentinel in required_source_sentinels.items()
        if name
        not in {
            "subject",
            "preview_email",
            "allowed_group",
        }
    }
    for name, sentinel in forbidden_artifact_sentinels.items():
        encoded = sentinel.encode("utf-8")
        check(
            f"privacy.artifact_excludes.{name}",
            encoded not in artifact_bytes and encoded not in views_bytes,
        )
    check(
        "privacy.artifact_endpoint_matches_durable_file_hash",
        artifact_response["sha256"] == hashlib.sha256(artifact_bytes).hexdigest(),
    )
    check(
        "privacy.all_named_artifact_views_are_live_and_scrubbed",
        set(views["views"])
        == {"database", "manifest", "audit", "log", "supportBundle"}
        and views["present"],
    )

    frame_response = get_json("/_frame")
    frame = frame_response["frame"]
    frame_bytes = json.dumps(frame, sort_keys=True, separators=(",", ":")).encode()
    check(
        "privacy.identity_attest_frame_is_bounded_and_token_free",
        frame_response["bytes"] <= 8192
        and set(frame)
        == {
            "schema",
            "ceremonyId",
            "ttlSeconds",
            "verificationUrl",
            "userCode",
            "status",
        }
        and id_token.encode() not in frame_bytes
        and privacy_device["device_code"].encode() not in frame_bytes
        and b"ACCESS_TOKEN_SENTINEL_T77" not in frame_bytes
        and b"REFRESH_TOKEN_SENTINEL_T77" not in frame_bytes,
    )

    control(
        "set_privacy_policy",
        privacy_policy={
            "preview_email": False,
            "allowed_groups": ["ALLOWED_ALPHA_SENTINEL_T77"],
            "max_allowed_group_matches": 1,
        },
    )
    control(
        "replace_flow", flow={"initial_status": "granted", "expires_in": 900}
    )
    no_preview_device, _ = post_device()
    no_preview_status, _, _, _ = post_token(no_preview_device["device_code"])
    no_preview_artifact = get_json("/_artifact")["artifact"]
    with open(artifact_path, "rb") as handle:
        no_preview_bytes = handle.read()
    check("privacy.second_live_mint_succeeds", no_preview_status == 200)
    check(
        "privacy.email_is_absent_without_explicit_preview_policy",
        "previewEmail" not in no_preview_artifact
        and b"PREVIEW_EMAIL_SENTINEL_T77@example.test" not in no_preview_bytes,
    )

    no_preview_hash = get_json("/_artifact")["sha256"]
    control(
        "set_claims",
        claims={"groups": [{"malformed": "MALFORMED_GROUP_SENTINEL_T77"}]},
    )
    malformed_device, _ = post_device()
    malformed_status, _, _, _ = post_token(malformed_device["device_code"])
    malformed_artifact = get_json("/_artifact")
    malformed_frame = get_json("/_frame")
    check(
        "privacy.malformed_group_shape_cannot_crash_or_replace_evidence",
        malformed_status == 200
        and malformed_artifact["lastProjectionError"]
        == "identity_claims_out_of_bounds"
        and malformed_artifact["sha256"] == no_preview_hash
        and b"MALFORMED_GROUP_SENTINEL_T77"
        not in pathlib.Path(artifact_path).read_bytes(),
    )
    check(
        "privacy.rejected_projection_never_marks_ceremony_verified",
        malformed_frame["frame"]["status"] == "token_rejected"
        and malformed_frame["frame"]["status"] != "verified",
    )
    large_groups = ["ALLOWED_ALPHA_SENTINEL_T77"] + [
        f"large-group-{index:03d}" for index in range(255)
    ]
    control("set_claims", claims={"groups": large_groups})
    large_device, _ = post_device()
    large_status, large_body, _, _ = post_token(large_device["device_code"])
    large_claims = decode_jwt(large_body["id_token"])[1]
    large_artifact = get_json("/_artifact")
    check(
        "privacy.group_list_256_boundary_is_accepted_and_reduced",
        large_status == 200
        and len(large_claims["groups"]) == 256
        and large_artifact["lastProjectionError"] is None
        and large_artifact["artifact"]["groupDecision"]["matchedAllowedGroups"]
        == ["ALLOWED_ALPHA_SENTINEL_T77"],
    )
    large_artifact_hash = large_artifact["sha256"]
    group_257_sentinel = "GROUP_257_SENTINEL_T77"
    control(
        "set_claims",
        claims={"groups": large_groups + [group_257_sentinel]},
    )
    too_many_device, _ = post_device()
    too_many_status, _, _, _ = post_token(too_many_device["device_code"])
    too_many_artifact = get_json("/_artifact")
    check(
        "privacy.group_list_257_is_rejected_without_replacing_evidence",
        too_many_status == 200
        and too_many_artifact["lastProjectionError"]
        == "identity_claims_out_of_bounds"
        and too_many_artifact["sha256"] == large_artifact_hash
        and group_257_sentinel.encode()
        not in pathlib.Path(artifact_path).read_bytes(),
    )
    oversized_group_sentinel = "OVERSIZED_GROUP_SENTINEL_T77_" + "x" * 257
    control("set_claims", claims={"groups": [oversized_group_sentinel]})
    oversized_group_device, _ = post_device()
    oversized_group_status, _, _, _ = post_token(
        oversized_group_device["device_code"]
    )
    oversized_group_artifact = get_json("/_artifact")
    check(
        "privacy.group_name_257_bytes_is_rejected_without_replacing_evidence",
        oversized_group_status == 200
        and oversized_group_artifact["lastProjectionError"]
        == "identity_claims_out_of_bounds"
        and oversized_group_artifact["sha256"] == large_artifact_hash
        and b"OVERSIZED_GROUP_SENTINEL_T77"
        not in pathlib.Path(artifact_path).read_bytes(),
    )
    original_groups = [
        "ALLOWED_ALPHA_SENTINEL_T77",
        "ALLOWED_BETA_SENTINEL_T77",
        "FULL_GROUP_SENTINEL_T77",
    ]
    control("set_claims", claims={"groups": original_groups})
    provenance_sentinels = {
        "iss": "NESTED_ISSUER_PII_SENTINEL_T77",
        "aud": "NESTED_AUDIENCE_PII_SENTINEL_T77",
        "exp": "NESTED_EXPIRY_PII_SENTINEL_T77",
    }
    for claim_name, sentinel in provenance_sentinels.items():
        control(
            "set_claims",
            claims={"extra": {claim_name: {"pii": sentinel}}},
        )
        nested_device, _ = post_device()
        nested_status, _, _, _ = post_token(nested_device["device_code"])
        nested_artifact = get_json("/_artifact")
        check(
            f"privacy.nested_{claim_name}_cannot_enter_provenance",
            nested_status == 200
            and nested_artifact["lastProjectionError"]
            == "identity_token_not_verified"
            and nested_artifact["sha256"] == large_artifact_hash
            and sentinel.encode() not in pathlib.Path(artifact_path).read_bytes(),
        )
    control(
        "set_claims",
        claims={
            "extra": {
                "phone_number": "UNRELATED_PII_SENTINEL_T77",
                "address": {"street": "PRIVATE_STREET_SENTINEL_T77"},
            }
        },
    )
    control(
        "set_privacy_policy",
        privacy_policy={
            "preview_email": False,
            "allowed_groups": ["ALLOWED_ALPHA_SENTINEL_T77"],
            "max_allowed_group_matches": 257,
        },
    )
    invalid_policy_device, _ = post_device()
    invalid_policy_status, _, _, _ = post_token(invalid_policy_device["device_code"])
    invalid_policy_artifact = get_json("/_artifact")
    check(
        "privacy.match_limit_257_is_rejected_without_replacing_evidence",
        invalid_policy_status == 200
        and invalid_policy_artifact["lastProjectionError"]
        == "identity_policy_out_of_bounds"
        and invalid_policy_artifact["sha256"] == large_artifact_hash,
    )
    control(
        "set_privacy_policy",
        privacy_policy={
            "preview_email": False,
            "allowed_groups": ["ALLOWED_ALPHA_SENTINEL_T77"],
            "max_allowed_group_matches": 1,
        },
    )
    control(
        "set_claims",
        claims={"groups": original_groups},
    )

    control("set_alg", alg="ES256")
    control("set_jwks_profile", profile="normal", mode="fresh")
    es_device, _ = post_device()
    es_status, es_body, _, _ = post_token(es_device["device_code"])
    es_jwks = get_json("/jwks")
    es_valid, es_mutated_valid = verify_es256(es_body["id_token"], es_jwks)
    check("jose.es256_live_token_uses_raw_signature", es_status == 200 and len(decode_jwt(es_body["id_token"])[4]) == 64)
    check("jose.es256_exact_original_input_verifies", es_valid)
    check("jose.es256_reserialized_or_mutated_input_fails", not es_mutated_valid)

    control("clock_set", monotonic=0)
    control(
        "replace_flow",
        flow={
            "initial_status": "authorization_pending",
            "expires_in": 900,
            "frame_ttl": 30,
        },
    )
    ttl_device, _ = post_device()
    ttl_at_start = get_json("/_frame")
    control("clock_set", monotonic=29)
    ttl_before_boundary = get_json("/_frame")
    state_before_boundary = get_json("/_state")
    control("clock_set", monotonic=30)
    ttl_at_boundary = get_json("/_frame")
    ttl_state = get_json("/_state")
    check(
        "privacy.ttl_retains_ephemera_before_boundary",
        ttl_at_start["present"]
        and ttl_before_boundary["present"]
        and ttl_device["device_code"] in state_before_boundary["devices"],
    )
    check(
        "privacy.ttl_boundary_removes_all_ceremony_ephemera",
        ttl_at_boundary["present"] is False
        and ttl_state["devices"] == {}
        and ttl_state["frame"]["present"] is False
        and ttl_state["pollOracle"]["status"] == "unconfigured",
    )
    check(
        "privacy.ttl_expiry_preserves_only_scrubbed_durable_evidence",
        ttl_state["artifact"]["present"],
    )

    control("clock_set", monotonic=31)
    cancel_device, _ = post_device()
    lifecycle_started = control("start_lifecycle_trap")
    lifecycle_running = get_json("/_state")["lifecycleTrap"]
    lifecycle_cancelled = control("cancel_lifecycle_trap")
    lifecycle_state = get_json("/_state")
    check(
        "lifecycle.cancel_kills_reaps_descendants_and_zeroizes_partial_token",
        lifecycle_started["status"] == "running"
        and lifecycle_started["descendantPid"] > 0
        and lifecycle_running["running"]
        and lifecycle_running["descendantPid"]
        == lifecycle_started["descendantPid"]
        and lifecycle_running["partialTokenBytes"] > 0
        and lifecycle_cancelled["status"] == "cancelled"
        and lifecycle_cancelled["reaped"]
        and lifecycle_cancelled["descendantsReaped"]
        and lifecycle_cancelled["descendantObserved"]
        and lifecycle_cancelled["partialTokenZeroized"]
        and lifecycle_state["lifecycleTrap"]
        == {"running": False, "descendantPid": None, "partialTokenBytes": 0},
    )
    check(
        "lifecycle.cancel_requires_fresh_ceremony_and_clears_codes",
        lifecycle_cancelled["freshCeremonyRequired"]
        and lifecycle_state["devices"] == {}
        and lifecycle_state["frame"]["present"] is False
        and cancel_device["device_code"] not in json.dumps(lifecycle_state),
    )

    retained_state_bytes = b"\n".join(
        path.read_bytes()
        for path in sorted(
            pathlib.Path(STATE_DIR).rglob("*"), key=lambda value: str(value)
        )
        if path.is_file()
    )
    retained_forbidden = [
        "ACCESS_TOKEN_SENTINEL_T77",
        "REFRESH_TOKEN_SENTINEL_T77",
        "FULL_GROUP_SENTINEL_T77",
        "ALLOWED_BETA_SENTINEL_T77",
        "UNRELATED_PII_SENTINEL_T77",
        "PRIVATE_STREET_SENTINEL_T77",
        "PREVIEW_EMAIL_SENTINEL_T77@example.test",
        "PARTIAL_TOKEN_SENTINEL_T77",
        "NETRC_USER_SENTINEL_T77",
        "NETRC_PASSWORD_SENTINEL_T77",
        group_257_sentinel,
        "OVERSIZED_GROUP_SENTINEL_T77",
        *provenance_sentinels.values(),
        privacy_device["device_code"],
        privacy_device["user_code"],
        privacy_device["verification_uri_complete"],
        ttl_device["device_code"],
        cancel_device["device_code"],
        id_token,
    ]
    check(
        "privacy.simulated_durable_state_tree_contains_no_forbidden_sentinel",
        all(value.encode("utf-8") not in retained_state_bytes for value in retained_forbidden),
    )

    boundary_cases = [
        (
            "future_skew_equal_600_passes",
            lease_spec(
                id="lease-skew-600",
                eventHash="skew-600",
                verifiedAt=700,
                validUntil=701,
                evidenceExpiry=701,
                policyCadence=1,
            ),
            "accepted",
            None,
        ),
        (
            "future_skew_plus_one_fails",
            lease_spec(
                id="lease-skew-601",
                eventHash="skew-601",
                verifiedAt=701,
                validUntil=702,
                evidenceExpiry=702,
                policyCadence=1,
            ),
            "rejected",
            "verified_at_future_skew",
        ),
        (
            "cadence_and_evidence_equal_pass",
            lease_spec(id="lease-equal", eventHash="equal"),
            "accepted",
            None,
        ),
        (
            "cadence_plus_one_fails",
            lease_spec(
                id="lease-cadence",
                eventHash="cadence",
                validUntil=201,
                evidenceExpiry=300,
            ),
            "rejected",
            "policy_cadence_exceeded",
        ),
        (
            "evidence_plus_one_fails",
            lease_spec(
                id="lease-evidence",
                eventHash="evidence",
                validUntil=201,
                evidenceExpiry=200,
                policyCadence=200,
            ),
            "rejected",
            "evidence_expiry_exceeded",
        ),
        (
            "nonpositive_duration_fails",
            lease_spec(
                id="lease-nonpositive",
                eventHash="nonpositive",
                validUntil=100,
            ),
            "rejected",
            "nonpositive_duration",
        ),
        (
            "late_admission_fails",
            lease_spec(
                id="lease-late",
                eventHash="late",
                verifiedAt=90,
                validUntil=100,
                evidenceExpiry=100,
                policyCadence=10,
            ),
            "rejected",
            "already_expired",
        ),
        (
            "wrong_generation_fails",
            lease_spec(policyGeneration=8),
            "rejected",
            "policy_generation_mismatch",
        ),
        (
            "inactive_verifier_fails",
            lease_spec(verifierActive=False),
            "rejected",
            "verifier_inactive",
        ),
        (
            "inactive_verifier_node_fails",
            lease_spec(verifierNodeActive=False),
            "rejected",
            "verifier_inactive",
        ),
        (
            "self_attestation_fails",
            lease_spec(verifierMember="member-target"),
            "rejected",
            "self_attestation",
        ),
        (
            "same_subject_verifier_fails",
            lease_spec(
                issuer="https://issuer.example",
                subject="verifier-subject",
                sameSubject=False,
            ),
            "rejected",
            "same_subject_verifier",
        ),
        (
            "verifier_without_current_lease_fails",
            lease_spec(verifierMember="member-without-lease"),
            "rejected",
            "verifier_missing_current_lease",
        ),
    ]
    for name, spec, expected_status, expected_reason in boundary_cases:
        setup_lease_generation()
        result = control("identity_admit_lease", lease=spec, wall=100)
        check(
            f"lease.boundary.{name}",
            result["status"] == expected_status
            and (expected_reason is None or result.get("reason") == expected_reason),
        )

    permutation_leases = [
        lease_spec(
            id="lease-a",
            eventHash="a",
            subjectMember="member-target",
            subject="target-subject",
            validUntil=210,
            evidenceExpiry=210,
            policyCadence=110,
        ),
        lease_spec(
            id="lease-b",
            eventHash="b",
            subjectMember="member-target",
            subject="target-subject",
            validUntil=230,
            evidenceExpiry=230,
            policyCadence=130,
        ),
        lease_spec(
            id="lease-c",
            eventHash="c",
            subjectMember="member-target",
            subject="target-subject",
            validUntil=220,
            evidenceExpiry=220,
            policyCadence=120,
        ),
    ]
    permutation_snapshots = []
    for permutation in itertools.permutations(permutation_leases):
        setup_lease_generation()
        for spec in permutation:
            control("identity_admit_lease", lease=spec, wall=100)
        current = get_json("/_state")["identityOracle"]
        permutation_snapshots.append(
            (
                current["eligibleLeaseSets"]["member-target"],
                current["effectiveDeadlines"]["member-target"],
            )
        )
    check(
        "lease.all_arrival_permutations_converge_canonically",
        len({json.dumps(value, sort_keys=True) for value in permutation_snapshots}) == 1
        and permutation_snapshots[0] == (["lease-a", "lease-b", "lease-c"], 230),
    )

    conflict_specs = [
        lease_spec(
            id=f"lease-conflict-{suffix}",
            eventHash=f"conflict-{suffix}",
            subjectMember=f"member-{suffix}",
            issuer="https://issuer.example",
            subject="same-subject",
        )
        for suffix in ("a", "b", "c")
    ]
    conflict_snapshots = []
    for permutation in itertools.permutations(conflict_specs):
        setup_lease_generation()
        for spec in permutation:
            control("identity_admit_lease", lease=spec, wall=100)
        current = get_json("/_state")["identityOracle"]
        conflict_snapshots.append(
            (
                current["conflicts"],
                current["manifestStatus"],
                current["paused"],
                sorted(
                    lease_id
                    for lease_id in current["eligibleLeaseIds"]
                    if lease_id.startswith("lease-conflict-")
                ),
            )
        )
    check(
        "lease.duplicate_identity_is_complete_set_manifest_conflict",
        len({json.dumps(value, sort_keys=True) for value in conflict_snapshots}) == 1
        and conflict_snapshots[0]
        == (
            [["member-a", "member-b", "member-c"]],
            "manifest_conflict",
            True,
            ["lease-conflict-a", "lease-conflict-b", "lease-conflict-c"],
        ),
    )
    control("clock_set", wall=200)
    control("identity_path", path="steward", wall=200)
    conflict_after_expiry = get_json("/_state")["identityOracle"]
    control("clock_set", wall=100)
    conflict_after_backward_correction = get_json("/_state")["identityOracle"]
    check(
        "lease.expired_duplicate_identity_conflict_does_not_pause_forever",
        conflict_after_expiry["conflicts"] == []
        and conflict_after_expiry["manifestStatus"] == "consistent"
        and not conflict_after_expiry["paused"]
        and not any(
            lease_id.startswith("lease-conflict-")
            for lease_id in conflict_after_expiry["eligibleLeaseIds"]
        )
        and conflict_after_backward_correction["conflicts"] == []
        and not conflict_after_backward_correction["paused"],
    )

    setup_lease_generation()
    accepted_once = control("identity_admit_lease", lease=lease_spec(), wall=100)
    before_duplicate = get_json("/_state")
    duplicate = control("identity_admit_lease", lease=lease_spec(), wall=100)
    after_duplicate = get_json("/_state")
    check(
        "lease.duplicate_event_hash_is_idempotent",
        accepted_once["status"] == "accepted"
        and duplicate["status"] == "duplicate"
        and duplicate["effectsUnchanged"]
        and before_duplicate["identityOracleSha256"]
        == after_duplicate["identityOracleSha256"],
    )

    setup_lease_generation()
    before_invalid = get_json("/_state")
    invalid_at_future_wall = control(
        "identity_admit_lease",
        lease=lease_spec(validUntil=201, evidenceExpiry=300),
        wall=150,
    )
    after_invalid = get_json("/_state")
    check(
        "lease.rejected_future_wall_admission_is_atomic",
        invalid_at_future_wall["status"] == "rejected"
        and invalid_at_future_wall["reason"] == "policy_cadence_exceeded"
        and before_invalid["identityOracleSha256"]
        == after_invalid["identityOracleSha256"]
        and after_invalid["identityOracle"]["floor"] == 100,
    )

    setup_lease_generation()
    control("identity_admit_lease", lease=lease_spec(), wall=100)
    floor_before_read = get_json("/_state")["identityOracle"]["floor"]
    control("clock_set", wall=250)
    effective_read = get_json("/_state")["identityOracle"]
    check(
        "lease.read_only_effective_time_hides_expired_without_advancing_floor",
        effective_read["effectiveTime"] == 250
        and effective_read["eligibleLeaseSets"]["member-target"] == []
        and effective_read["floor"] == floor_before_read,
    )

    setup_lease_generation()
    control("identity_admit_lease", lease=lease_spec(), wall=100)
    forward = control("identity_path", path="grant", wall=200)
    forward_state = get_json("/_state")["identityOracle"]
    backward = control("identity_path", path="grant", wall=150)
    backward_state = get_json("/_state")["identityOracle"]
    check(
        "time.forward_jump_expires_lease_early",
        forward["status"] == "authorized"
        and forward_state["eligibleLeaseSets"]["member-target"] == [],
    )
    check(
        "time.backward_correction_never_revives_expired_lease",
        backward["status"] == "team_identity_clock_rollback"
        and backward_state["eligibleLeaseSets"]["member-target"] == [],
    )

    setup_lease_generation()
    control("identity_admit_lease", lease=lease_spec(), wall=100)
    no_confirmation = control(
        "identity_repair", new_floor=90, confirmed=False, corrected_clock=True
    )
    no_correction = control(
        "identity_repair", new_floor=90, confirmed=True, corrected_clock=False
    )
    control("identity_path", path="grant", wall=200)
    repaired = control(
        "identity_repair", new_floor=90, confirmed=True, corrected_clock=True
    )
    repaired_state = get_json("/_state")["identityOracle"]
    check(
        "time.repair_requires_confirmation_and_corrected_clock",
        no_confirmation["status"] == "confirmation_required"
        and no_correction["status"] == "clock_not_corrected",
    )
    check(
        "time.confirmed_repair_suppresses_leases_before_lowering",
        repaired["status"] == "repaired"
        and repaired["suppressedLeaseIds"]
        == [
            "bootstrap-7-creator",
            "bootstrap-7-member-verifier",
            "lease-base",
        ]
        and repaired_state["floor"] == 90
        and repaired_state["eligibleLeaseIds"] == []
        and repaired_state["outerState"] == "identity_pending"
        and repaired_state["bootstrap"]["state"] == "grace"
        and repaired_state["bootstrap"]["graceDeadline"] == 140
        and repaired_state["bootstrap"]["ceremonyEpoch"] == 1
        and repaired_state["bootstrap"]["attested"] == []
        and all(
            renewal["state"] == "pending"
            for renewal in repaired_state["renewals"].values()
        ),
    )
    no_reactivation = control("identity_path", path="grant", wall=90)
    check(
        "time.repaired_old_evidence_never_reactivates",
        no_reactivation["status"] == "authorized"
        and get_json("/_state")["identityOracle"]["eligibleLeaseIds"] == [],
    )
    repair_verifier = bootstrap_attest(
        "member-verifier",
        "creator",
        "verifier-subject",
        "creator-subject",
        wall=90,
        lease_seconds=50,
    )
    repair_creator = bootstrap_attest(
        "creator",
        "member-verifier",
        "creator-subject",
        "verifier-subject",
        wall=90,
        lease_seconds=50,
    )
    recovered_state = get_json("/_state")["identityOracle"]
    check(
        "time.repair_fresh_current_generation_ceremonies_are_accepted",
        repair_verifier["status"] == "verified"
        and repair_creator["status"] == "verified",
    )
    check(
        "time.repair_fresh_ceremonies_restore_active_current_generation",
        recovered_state["outerState"] == "active"
        and recovered_state["bootstrap"]["state"] == "active"
        and set(recovered_state["eligibleLeaseIds"])
        == {
            "bootstrap-7-r1-creator",
            "bootstrap-7-r1-member-verifier",
        },
    )
    check(
        "time.repair_suppressed_evidence_stays_suppressed_after_recovery",
        all(
            lease["suppressed"]
            for lease in recovered_state["leases"]
            if lease["id"]
            in {
                "bootstrap-7-creator",
                "bootstrap-7-member-verifier",
                "lease-base",
            }
        ),
    )

    setup_lease_generation()
    control("identity_path", path="grant", wall=200)
    control("identity_repair", new_floor=90, confirmed=True, corrected_clock=True)
    expired_repair_grace = bootstrap_attest(
        "member-verifier",
        "creator",
        "verifier-subject",
        "creator-subject",
        wall=140,
        lease_seconds=50,
    )
    expired_repair_state = get_json("/_state")["identityOracle"]
    check(
        "time.repair_grace_rejects_at_exact_deadline_without_tick",
        expired_repair_grace
        == {"status": "rejected", "reason": "bootstrap_grace_expired"}
        and expired_repair_state["floor"] == 140
        and expired_repair_state["bootstrap"]["state"] == "suspended"
        and expired_repair_state["eligibleLeaseIds"] == [],
    )

    control("identity_reset", floor=100)
    disabled_verify = bootstrap_attest(
        "joiner", "creator", "joiner-subject", "creator-subject"
    )
    rollback_enable = control(
        "bootstrap_enable", generation=1, grace_seconds=20, wall=99
    )
    zero_grace = control(
        "bootstrap_enable", generation=1, grace_seconds=0, wall=100
    )
    enabled = control(
        "bootstrap_enable", generation=1, grace_seconds=20, wall=100
    )
    check("bootstrap.one_member_zero_grace_refuses", zero_grace["status"] == "zero_grace_refused")
    check(
        "bootstrap.disabled_or_rollback_mutations_are_rejected",
        disabled_verify["reason"] == "tier2_not_enabled"
        and rollback_enable["status"] == "team_identity_clock_rollback"
        and get_json("/_state")["identityOracle"]["floor"] == 100,
    )
    check(
        "bootstrap.enable_enters_nonzero_grace_not_orphaned",
        enabled["status"] == "enabled"
        and enabled["bootstrap"]["state"] == "grace"
        and enabled["bootstrap"]["graceDeadline"] == 120,
    )
    self_reject = bootstrap_attest(
        "creator", "creator", "creator-subject", "creator-subject"
    )
    same_subject_reject = bootstrap_attest(
        "joiner", "creator", "shared-subject", "shared-subject"
    )
    joiner = bootstrap_attest(
        "joiner", "creator", "joiner-subject", "creator-subject"
    )
    creator = bootstrap_attest(
        "creator", "joiner", "creator-subject", "joiner-subject"
    )
    check(
        "bootstrap.self_and_same_subject_never_satisfy_exception",
        self_reject["reason"] == "self_attestation"
        and same_subject_reject["reason"] == "same_subject_verifier",
    )
    check(
        "bootstrap.creator_then_distinct_joiner_completes_cross_verification",
        joiner["bootstrap"]["state"] == "grace"
        and creator["bootstrap"]["state"] == "active"
        and creator["bootstrap"]["attested"] == ["creator", "joiner"],
    )
    bootstrap_active_state = get_json("/_state")["identityOracle"]
    check(
        "bootstrap.activation_is_backed_by_finite_exact_generation_leases",
        bootstrap_active_state["eligibleLeaseSets"]["creator"]
        == ["bootstrap-1-creator"]
        and bootstrap_active_state["eligibleLeaseSets"]["joiner"]
        == ["bootstrap-1-joiner"]
        and bootstrap_active_state["effectiveDeadlines"]["creator"] == 400
        and bootstrap_active_state["effectiveDeadlines"]["joiner"] == 400,
    )
    closed_exception = bootstrap_attest(
        "late-member", "creator", "late-subject", "creator-subject"
    )
    check(
        "bootstrap.exception_closes_after_activation",
        closed_exception == {
            "status": "rejected",
            "reason": "bootstrap_exception_closed",
        },
    )
    tightened = control(
        "bootstrap_enable", generation=2, grace_seconds=20, wall=105
    )
    check(
        "bootstrap.tightening_new_generation_returns_members_to_grace",
        tightened["bootstrap"]["policyGeneration"] == 2
        and tightened["bootstrap"]["state"] == "grace"
        and tightened["bootstrap"]["attested"] == [],
    )
    downgrade = control(
        "bootstrap_enable", generation=1, grace_seconds=20, wall=106
    )
    after_downgrade = get_json("/_state")["identityOracle"]
    check(
        "bootstrap.policy_generation_downgrade_cannot_revive_old_leases",
        downgrade == {
            "status": "rejected",
            "reason": "policy_generation_not_advanced",
        }
        and after_downgrade["bootstrap"]["policyGeneration"] == 2
        and after_downgrade["eligibleLeaseIds"] == [],
    )

    control("identity_reset", floor=100)
    control("bootstrap_enable", generation=1, grace_seconds=10, wall=100)
    before_tick = get_json("/_state")
    still_grace = control("bootstrap_tick", wall=109)
    suspended = control("bootstrap_tick", wall=110)
    backward_tick = control("bootstrap_tick", wall=109)
    after_tick = get_json("/_state")
    check(
        "bootstrap.no_verifier_stays_pending_through_grace_then_suspends",
        still_grace["status"] == "grace"
        and suspended["status"] == "suspended"
        and backward_tick["status"] == "team_identity_clock_rollback"
        and after_tick["identityOracle"]["bootstrap"]["state"] == "suspended",
    )
    check(
        "bootstrap.steward_tick_performs_zero_idp_http",
        count_idp_requests(before_tick) == count_idp_requests(after_tick)
        and suspended["bootstrap"]["backgroundIdpRequests"] == 0,
    )

    setup_lease_generation()
    control("identity_admit_lease", lease=lease_spec(), wall=100)
    before_renewal = get_json("/_state")
    active_before_due = control(
        "renewal_tick",
        subject_member="member-target",
        wall=199,
        grace_seconds=10,
        verifier_available=False,
    )
    grace_without_verifier = control(
        "renewal_tick",
        subject_member="member-target",
        wall=200,
        grace_seconds=10,
        verifier_available=False,
    )
    still_in_grace = control(
        "renewal_tick",
        subject_member="member-target",
        wall=209,
        grace_seconds=10,
        verifier_available=False,
    )
    suspended_after_grace = control(
        "renewal_tick",
        subject_member="member-target",
        wall=210,
        grace_seconds=10,
        verifier_available=False,
    )
    renewal_rollback = control(
        "renewal_tick",
        subject_member="member-target",
        wall=209,
        grace_seconds=10,
        verifier_available=True,
    )
    after_renewal = get_json("/_state")
    check(
        "grace.ordinary_lease_due_overdue_and_suspension_are_finite",
        active_before_due["status"] == "active"
        and grace_without_verifier["status"] == "grace"
        and still_in_grace["status"] == "grace"
        and suspended_after_grace["status"] == "suspended"
        and renewal_rollback["status"] == "team_identity_clock_rollback"
        and after_renewal["identityOracle"]["renewals"]["member-target"]["state"]
        == "suspended",
    )
    check(
        "grace.verifier_loss_and_expiry_perform_zero_background_idp_http",
        count_idp_requests(before_renewal) == count_idp_requests(after_renewal)
        and after_renewal["identityOracle"]["renewals"]["member-target"][
            "backgroundIdpRequests"
        ]
        == 0,
    )
    fresh_after_suspension = control(
        "identity_admit_lease",
        lease=lease_spec(
            id="lease-recovered",
            eventHash="recovered",
            verifiedAt=210,
            validUntil=300,
            evidenceExpiry=300,
            policyCadence=90,
        ),
        wall=210,
    )
    recovered_tick = control(
        "renewal_tick",
        subject_member="member-target",
        wall=211,
        grace_seconds=10,
        verifier_available=True,
    )
    recovered_state = get_json("/_state")["identityOracle"]
    check(
        "grace.fresh_interactive_lease_recovers_suspended_subject",
        fresh_after_suspension["status"] == "accepted"
        and recovered_tick["status"] == "active"
        and recovered_state["renewals"]["member-target"]["state"] == "active"
        and recovered_state["effectiveDeadlines"]["member-target"] == 300,
    )

    control("identity_reset", floor=200)
    before_replay_rollback = get_json("/_state")
    replay_rollback = control(
        "replay_claim",
        issuer="https://issuer-a.example",
        client_id="client-a",
        claim="ROLLBACK_REPLAY_SENTINEL_T77",
        claimant="rollback-claimant",
        expires_at=400,
        wall=199,
    )
    after_replay_rollback = get_json("/_state")
    replay_forward = control(
        "replay_claim",
        issuer="https://issuer-a.example",
        client_id="client-a",
        claim="FORWARD_REPLAY_SENTINEL_T77",
        claimant="forward-claimant",
        expires_at=400,
        wall=250,
    )
    check(
        "replay.rollback_is_atomic_and_forward_claim_advances_floor",
        replay_rollback["status"] == "team_identity_clock_rollback"
        and before_replay_rollback["identityOracleSha256"]
        == after_replay_rollback["identityOracleSha256"]
        and replay_forward["status"] == "accepted"
        and get_json("/_state")["identityOracle"]["floor"] == 250,
    )

    control("identity_reset", floor=100)
    claimants = [f"claimant-{index}" for index in range(16)]
    with concurrent.futures.ThreadPoolExecutor(max_workers=16) as executor:
        replay_results = list(
            executor.map(
                lambda claimant: control(
                    "replay_claim",
                    issuer="https://issuer-a.example",
                    client_id="client-a",
                    claim="TOKEN_HASH_REPLAY_SENTINEL_T77",
                    claimant=claimant,
                    expires_at=400,
                    wall=100,
                ),
                claimants,
            )
        )
    accepted = [row for row in replay_results if row["status"] == "accepted"]
    replayed = [row for row in replay_results if row["status"] == "replay"]
    winners = {row["winner"] for row in replay_results}
    check(
        "replay.concurrent_claim_has_exactly_one_atomic_winner",
        len(accepted) == 1 and len(replayed) == 15 and len(winners) == 1,
    )
    scoped_client = control(
        "replay_claim",
        issuer="https://issuer-a.example",
        client_id="client-b",
        claim="TOKEN_HASH_REPLAY_SENTINEL_T77",
        claimant="client-b-winner",
        expires_at=400,
        wall=100,
    )
    scoped_issuer = control(
        "replay_claim",
        issuer="https://issuer-b.example",
        client_id="client-a",
        claim="TOKEN_HASH_REPLAY_SENTINEL_T77",
        claimant="issuer-b-winner",
        expires_at=400,
        wall=100,
    )
    before_expiry = control(
        "replay_claim",
        issuer="https://issuer-a.example",
        client_id="client-a",
        claim="TOKEN_HASH_REPLAY_SENTINEL_T77",
        claimant="late-before-expiry",
        expires_at=400,
        wall=399,
    )
    after_expiry = control(
        "replay_claim",
        issuer="https://issuer-a.example",
        client_id="client-a",
        claim="TOKEN_HASH_REPLAY_SENTINEL_T77",
        claimant="fresh-after-expiry",
        expires_at=700,
        wall=400,
    )
    expired_input = control(
        "replay_claim",
        issuer="https://issuer-a.example",
        client_id="client-a",
        claim="ALREADY_EXPIRED_SENTINEL_T77",
        claimant="nobody",
        expires_at=400,
        wall=400,
    )
    check(
        "replay.scope_is_issuer_and_client_specific",
        scoped_client["status"] == "accepted"
        and scoped_issuer["status"] == "accepted"
        and len(
            {
                accepted[0]["claimHash"],
                scoped_client["claimHash"],
                scoped_issuer["claimHash"],
            }
        )
        == 3,
    )
    check(
        "replay.claim_is_single_winner_through_expiry_then_released",
        before_expiry["status"] == "replay"
        and after_expiry["status"] == "accepted"
        and after_expiry["winner"] == "fresh-after-expiry"
        and expired_input == {"status": "rejected", "reason": "token_expired"},
    )

    final_state = get_json("/_state")
    check(
        "offline.every_observed_peer_is_loopback",
        bool(final_state["requestTrace"])
        and all(row["clientHost"] == "127.0.0.1" for row in final_state["requestTrace"]),
    )


try:
    if PHASE == "pre":
        run_pre_restart()
    elif PHASE == "post":
        run_post_restart()
    else:
        raise ValueError(f"unknown phase: {PHASE}")
except Exception as error:
    emit(f"{PHASE}.unhandled_exception.{type(error).__name__}", False)
    print(f"fake_idp_selfcheck {PHASE}: {error}", file=sys.stderr)

emit(f"{PHASE}.unique_assertion_labels", len(labels) == len(set(labels)))
emit(f"{PHASE}.overall", failures == 0)
raise SystemExit(0 if failures == 0 else 1)
PY
}

set +e
run_matrix_phase pre
PRE_RC=$?
set -e

fake_idp_restart

set +e
run_matrix_phase post
POST_RC=$?
set -e

if [ "$PRE_RC" -ne 0 ] || [ "$POST_RC" -ne 0 ]; then
    echo "fake_idp_selfcheck: matrix failed (pre=$PRE_RC post=$POST_RC)" >&2
    exit 1
fi

echo "fake_idp_selfcheck: deterministic capability/privacy/time matrix passed" >&2
