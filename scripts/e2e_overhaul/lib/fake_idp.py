#!/usr/bin/env python3
"""bd-tc-epic-qzk7o.8.7 — deterministic fake OIDC IdP for team-confed tier-2 tests.

Serves TLS discovery/JWKS/device/token endpoints on 127.0.0.1 with an
ephemeral CA, a scriptable RFC 8628 device-flow state machine, and RS256 /
ES256 ID-token minting with rotatable keys. Zero third-party python deps:
all cryptography is delegated to the system `openssl` binary; the server is
http.server + ssl from the stdlib. Never touches the real network.

Usage:
  fake_idp.py --dir <state-dir> --port <port> [--scenario <scenario.json>]

The state dir receives: ca.pem (for clients to trust), server cert/key,
signing keys, ready file (port written once listening), and a control log.
Test drivers mutate behavior at runtime via POST /_control and inspect via
GET /_state. This process serves one scenario per lifetime; restart for a
fresh ceremony (process loss preserves nothing but the outer state dir).
"""

import argparse
import base64
import json
import os
import ssl
import struct
import subprocess
import sys
import threading
import time
import uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


def b64url(data: bytes) -> str:
    return base64.urlsafe_b64encode(data).rstrip(b"=").decode("ascii")


def run_openssl(args, data=None):
    proc = subprocess.run(
        ["openssl", *args], input=data, capture_output=True, check=True
    )
    return proc.stdout


class KeyMaterial:
    """One signing key generation: RSA-2048 (RS256) + P-256 (ES256)."""

    def __init__(self, state_dir: str, generation: int):
        self.generation = generation
        self.rsa_kid = f"rs{generation}"
        self.ec_kid = f"es{generation}"
        self.rsa_key = os.path.join(state_dir, f"rsa-{generation}.pem")
        self.ec_key = os.path.join(state_dir, f"ec-{generation}.pem")
        run_openssl(["genrsa", "-out", self.rsa_key, "2048"])
        run_openssl(
            ["ecparam", "-name", "prime256v1", "-genkey", "-noout", "-out", self.ec_key]
        )

    def rsa_public_jwk(self):
        text = run_openssl(
            ["rsa", "-in", self.rsa_key, "-text", "-noout"]
        ).decode("ascii", "replace")
        hex_digits = []
        in_modulus = False
        exponent = 65537
        for line in text.splitlines():
            stripped = line.strip()
            if stripped.startswith("modulus:") or stripped.startswith("Modulus:"):
                in_modulus = True
                continue
            if stripped.startswith("publicExponent:") or stripped.startswith(
                "Exponent:"
            ):
                in_modulus = False
                exponent = int(stripped.split()[1])
                continue
            if in_modulus:
                if all(c in "0123456789abcdefABCDEF:" for c in stripped) and stripped:
                    hex_digits.append(stripped.replace(":", ""))
                else:
                    in_modulus = False
        modulus = bytes.fromhex("".join(hex_digits)).lstrip(b"\x00")
        exp_bytes = exponent.to_bytes((exponent.bit_length() + 7) // 8, "big")
        return {
            "kty": "RSA",
            "kid": self.rsa_kid,
            "use": "sig",
            "alg": "RS256",
            "n": b64url(modulus),
            "e": b64url(exp_bytes),
        }

    def ec_public_jwk(self):
        text = run_openssl(
            ["ec", "-in", self.ec_key, "-text", "-noout"]
        ).decode("ascii", "replace")
        hex_digits = []
        in_pub = False
        for line in text.splitlines():
            stripped = line.strip()
            if stripped.startswith("pub:"):
                in_pub = True
                continue
            if in_pub:
                if all(c in "0123456789abcdefABCDEF:" for c in stripped) and stripped:
                    hex_digits.append(stripped.replace(":", ""))
                else:
                    in_pub = False
        point = bytes.fromhex("".join(hex_digits))
        if point[0] != 0x04 or len(point) != 65:
            raise RuntimeError("unexpected EC public point encoding")
        return {
            "kty": "EC",
            "kid": self.ec_kid,
            "use": "sig",
            "alg": "ES256",
            "crv": "P-256",
            "x": b64url(point[1:33]),
            "y": b64url(point[33:65]),
        }

    def sign(self, alg: str, signing_input: bytes) -> bytes:
        if alg == "RS256":
            return run_openssl(
                ["dgst", "-sha256", "-sign", self.rsa_key], data=signing_input
            )
        if alg == "ES256":
            der = run_openssl(
                ["dgst", "-sha256", "-sign", self.ec_key], data=signing_input
            )
            return der_ecdsa_to_raw(der)
        raise ValueError(f"unsupported alg {alg}")


def der_ecdsa_to_raw(der: bytes) -> bytes:
    """Convert DER SEQUENCE{r INTEGER, s INTEGER} to the raw 64-byte r||s JOSE form."""
    if der[0] != 0x30:
        raise ValueError("not a DER sequence")
    idx = 2
    if der[1] & 0x80:
        idx = 2 + (der[1] & 0x7F)

    def read_int(offset):
        if der[offset] != 0x02:
            raise ValueError("expected DER integer")
        length = der[offset + 1]
        value = der[offset + 2 : offset + 2 + length]
        return value.lstrip(b"\x00"), offset + 2 + length

    r, idx = read_int(idx)
    s, _ = read_int(idx)
    return r.rjust(32, b"\x00") + s.rjust(32, b"\x00")


class IdpState:
    def __init__(self, scenario: dict, state_dir: str):
        self.lock = threading.Lock()
        self.scenario = scenario
        self.state_dir = state_dir
        self.issuer = scenario.get("issuer_path", "/idp")
        self.flow = scenario.get("flow", {})
        self.keys = [KeyMaterial(state_dir, 1)]
        self.retired_kids = []
        self.devices = {}
        self.token_polls = {}
        self.minted_jtis = []
        self.control_log = []

    def current_keys(self):
        return self.keys[-1]

    def rotate_keys(self, retire_previous: bool):
        with self.lock:
            if retire_previous:
                previous = self.keys[-1]
                self.retired_kids.extend([previous.rsa_kid, previous.ec_kid])
            self.keys.append(KeyMaterial(self.state_dir, len(self.keys) + 1))

    def jwks(self):
        with self.lock:
            keys = []
            for material in self.keys:
                if material.rsa_kid not in self.retired_kids:
                    keys.append(material.rsa_public_jwk())
                if material.ec_kid not in self.retired_kids:
                    keys.append(material.ec_public_jwk())
            return {"keys": keys}


def make_handler(state: IdpState, base_url_holder: dict):
    class Handler(BaseHTTPRequestHandler):
        server_version = "FakeIdp/1"

        def log_message(self, *_args):
            pass

        def _send_json(self, status: int, payload: dict, extra_headers=None):
            body = json.dumps(payload).encode("utf-8")
            self.send_response(status)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            for key, value in (extra_headers or {}).items():
                self.send_header(key, value)
            self.end_headers()
            self.wfile.write(body)

        def _read_body(self) -> bytes:
            length = int(self.headers.get("Content-Length", "0"))
            return self.rfile.read(min(length, 65536))

        def do_GET(self):
            base = base_url_holder["base"]
            if self.path == "/.well-known/openid-configuration":
                self._send_json(
                    200,
                    {
                        "issuer": base,
                        "device_authorization_endpoint": f"{base}/device",
                        "token_endpoint": f"{base}/token",
                        "jwks_uri": f"{base}/jwks",
                        "grant_types_supported": [
                            "urn:ietf:params:oauth:grant-type:device_code"
                        ],
                        "token_endpoint_auth_methods_supported": (
                            ["client_secret_post"]
                            if state.scenario.get("secret_required")
                            else ["none"]
                        ),
                        "id_token_signing_alg_values_supported": ["RS256", "ES256"],
                    },
                )
            elif self.path == "/jwks":
                self._send_json(200, state.jwks())
            elif self.path == "/_state":
                with state.lock:
                    self._send_json(
                        200,
                        {
                            "devices": {
                                code: dict(entry, minted_jtis=None)
                                for code, entry in state.devices.items()
                            },
                            "retired_kids": state.retired_kids,
                            "generations": len(state.keys),
                            "minted_jtis": state.minted_jtis,
                            "control_log": state.control_log,
                        },
                    )
            else:
                self._send_json(404, {"error": "not_found"})

        def do_POST(self):
            if self.path == "/device":
                self._handle_device()
            elif self.path == "/token":
                self._handle_token()
            elif self.path == "/_control":
                self._handle_control()
            else:
                self._send_json(404, {"error": "not_found"})

        def _handle_device(self):
            self._read_body()
            flow = state.flow
            device_code = f"dev-{uuid.uuid4().hex[:16]}"
            user_code = f"{uuid.uuid4().hex[:4].upper()}-{uuid.uuid4().hex[:4].upper()}"
            with state.lock:
                state.devices[device_code] = {
                    "status": flow.get("initial_status", "authorization_pending"),
                    "user_code": user_code,
                    "issued_at": time.time(),
                    "polls": 0,
                    "interval": flow.get("interval", 5),
                }
            base = base_url_holder["base"]
            payload = {
                "device_code": device_code,
                "user_code": user_code,
                "verification_uri": f"{base}/activate",
                "verification_uri_complete": f"{base}/activate?user_code={user_code}",
            }
            if "expires_in" in flow:
                if flow["expires_in"] is not None:
                    payload["expires_in"] = flow["expires_in"]
            else:
                payload["expires_in"] = 900
            if "interval" in flow:
                if flow["interval"] is not None:
                    payload["interval"] = flow["interval"]
            else:
                payload["interval"] = 5
            for key in flow.get("device_response_omit", []):
                payload.pop(key, None)
            self._send_json(200, payload)

        def _handle_token(self):
            body = self._read_body().decode("utf-8", "replace")
            params = {}
            for pair in body.split("&"):
                if "=" in pair:
                    key, _, value = pair.partition("=")
                    params[key] = value
            device_code = params.get("device_code", "")
            if state.scenario.get("secret_required") and "client_secret" not in params:
                self._send_json(401, {"error": "invalid_client"})
                return
            with state.lock:
                entry = state.devices.get(device_code)
                if entry is None:
                    self._send_json(400, {"error": "invalid_grant"})
                    return
                entry["polls"] += 1
                flow = state.flow
                expires_in = flow.get("expires_in", 900) or 900
                if time.time() - entry["issued_at"] > expires_in:
                    entry["status"] = "expired_token"
                status = entry["status"]
                if status == "authorization_pending":
                    slow_after = flow.get("slow_down_after_polls")
                    if slow_after is not None and entry["polls"] > slow_after:
                        entry["interval"] += 5
                        self._send_json(400, {"error": "slow_down"})
                        return
                    self._send_json(400, {"error": "authorization_pending"})
                    return
                if status == "slow_down":
                    entry["interval"] += 5
                    self._send_json(400, {"error": "slow_down"})
                    return
                if status == "access_denied":
                    self._send_json(400, {"error": "access_denied"})
                    return
                if status == "expired_token":
                    self._send_json(400, {"error": "expired_token"})
                    return
                if status == "granted":
                    token = self._mint_id_token(entry)
                    self._send_json(
                        200,
                        {
                            "access_token": f"opaque-{uuid.uuid4().hex[:12]}",
                            "token_type": "Bearer",
                            "id_token": token,
                        },
                    )
                    return
            self._send_json(500, {"error": "unhandled_status"})

        def _mint_id_token(self, entry: dict) -> str:
            claims_config = state.scenario.get("claims", {})
            alg = state.scenario.get("alg", "RS256")
            material = state.current_keys()
            kid = material.rsa_kid if alg == "RS256" else material.ec_kid
            now = int(time.time())
            jti = f"jti-{uuid.uuid4().hex[:16]}"
            payload = {
                "iss": base_url_holder["base"],
                "aud": claims_config.get("aud", "ee-team-client"),
                "sub": claims_config.get("sub", "user-priya"),
                "email": claims_config.get("email", "priya@example.test"),
                "email_verified": claims_config.get("email_verified", True),
                "iat": now,
                "auth_time": now,
                "exp": now + claims_config.get("lifetime_seconds", 300),
                "jti": jti,
            }
            if "groups" in claims_config:
                payload["groups"] = claims_config["groups"]
            payload.update(claims_config.get("extra", {}))
            for key in claims_config.get("omit", []):
                payload.pop(key, None)
            header = {"alg": alg, "typ": "JWT", "kid": kid}
            signing_input = (
                b64url(json.dumps(header, separators=(",", ":")).encode())
                + "."
                + b64url(json.dumps(payload, separators=(",", ":")).encode())
            ).encode("ascii")
            signature = material.sign(alg, signing_input)
            state.minted_jtis.append(jti)
            return signing_input.decode("ascii") + "." + b64url(signature)

        def _handle_control(self):
            try:
                command = json.loads(self._read_body().decode("utf-8"))
            except (ValueError, UnicodeDecodeError):
                self._send_json(400, {"error": "bad_control_payload"})
                return
            action = command.get("action")
            with state.lock:
                state.control_log.append(action or "unknown")
            if action == "set_status":
                target_status = command.get("status", "granted")
                user_code = command.get("user_code")
                with state.lock:
                    changed = 0
                    for entry in state.devices.values():
                        if user_code is None or entry["user_code"] == user_code:
                            entry["status"] = target_status
                            changed += 1
                self._send_json(200, {"ok": True, "changed": changed})
            elif action == "rotate_keys":
                state.rotate_keys(bool(command.get("retire_previous", False)))
                self._send_json(200, {"ok": True, "generations": len(state.keys)})
            elif action == "set_flow":
                with state.lock:
                    state.flow.update(command.get("flow", {}))
                self._send_json(200, {"ok": True})
            else:
                self._send_json(400, {"error": "unknown_action"})

    return Handler


def build_tls(state_dir: str) -> ssl.SSLContext:
    ca_key = os.path.join(state_dir, "ca-key.pem")
    ca_pem = os.path.join(state_dir, "ca.pem")
    srv_key = os.path.join(state_dir, "server-key.pem")
    srv_csr = os.path.join(state_dir, "server.csr")
    srv_pem = os.path.join(state_dir, "server.pem")
    ext = os.path.join(state_dir, "san.cnf")
    run_openssl(["genrsa", "-out", ca_key, "2048"])
    run_openssl(
        [
            "req", "-x509", "-new", "-key", ca_key, "-sha256", "-days", "2",
            "-subj", "/CN=fake-idp-ephemeral-ca", "-out", ca_pem,
        ]
    )
    with open(ext, "w", encoding="ascii") as handle:
        handle.write("subjectAltName=IP:127.0.0.1,DNS:localhost\n")
    run_openssl(["genrsa", "-out", srv_key, "2048"])
    run_openssl(
        ["req", "-new", "-key", srv_key, "-subj", "/CN=127.0.0.1", "-out", srv_csr]
    )
    run_openssl(
        [
            "x509", "-req", "-in", srv_csr, "-CA", ca_pem, "-CAkey", ca_key,
            "-CAcreateserial", "-days", "2", "-sha256", "-extfile", ext,
            "-out", srv_pem,
        ]
    )
    context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    context.load_cert_chain(srv_pem, srv_key)
    return context


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--dir", required=True)
    parser.add_argument("--port", type=int, default=0)
    parser.add_argument("--scenario", default=None)
    args = parser.parse_args()

    os.makedirs(args.dir, exist_ok=True)
    scenario = {}
    if args.scenario:
        with open(args.scenario, "r", encoding="utf-8") as handle:
            scenario = json.load(handle)

    state = IdpState(scenario, args.dir)
    base_url_holder = {"base": ""}
    handler = make_handler(state, base_url_holder)
    server = ThreadingHTTPServer(("127.0.0.1", args.port), handler)
    server.socket = build_tls(args.dir).wrap_socket(server.socket, server_side=True)
    port = server.socket.getsockname()[1]
    base_url_holder["base"] = f"https://127.0.0.1:{port}"

    ready_path = os.path.join(args.dir, "ready")
    with open(ready_path + ".tmp", "w", encoding="ascii") as handle:
        handle.write(str(port))
    os.replace(ready_path + ".tmp", ready_path)

    try:
        server.serve_forever(poll_interval=0.2)
    except KeyboardInterrupt:
        pass
    finally:
        server.server_close()
    return 0


if __name__ == "__main__":
    sys.exit(main())
