# Fake OIDC IdP Harness (tier-2 SSO tests)

Status: implemented
Bead: bd-tc-epic-qzk7o.8.7 (T7.7)
Consumers: T7.4 (device-flow client), T7.5 (token verification), T7.6 (join attestation)

## Purpose

A deterministic, fully-offline OpenID Connect identity provider that the
team-confederation tier-2 SSO client acceptance tests run against. It exists so
those tests need **no real IdP account and no outbound network** — every OIDC
protocol shape and every attack the verification client must reject is minted
locally, reproducibly, from a scenario file. Per ADR 0086 TC-D13, tier-2 uses
the RFC 8628 device authorization grant only; this harness serves exactly that
flow.

Dependencies: `python3`, `openssl`, and `curl` — the same tools already
required by the fake-Tailscale harness. No third-party Python packages; all
cryptography is delegated to the system `openssl` binary, and the server is
stdlib `http.server` + `ssl` behind an ephemeral CA generated per run.

## Files

| Path | Role |
|---|---|
| `scripts/e2e_overhaul/lib/fake_idp.py` | The server: TLS discovery / JWKS / device / token endpoints, RS256+ES256 minting with rotatable+retirable keys, a scriptable device-flow state machine, and a `/_control` + `/_state` surface |
| `scripts/e2e_overhaul/lib/fake_idp.sh` | Bash harness: `fake_idp_start`/`_stop`/`_control`/`_state`/`_curl` helpers exporting `FAKE_IDP_BASE` and the CA path a client must trust |
| `scripts/e2e_overhaul/fake_idp_harness_smoke.sh` | Happy-path self-test (16 checks): discovery, JWKS, device authorization, pending/grant polling, RS256 claim integrity, jti recording, key-rotation retirement, ES256 raw-64-byte signature |
| `scripts/e2e_overhaul/fake_idp_defects_smoke.sh` | Adversarial self-test (12 checks): the JOSE/protocol attack corpus below |

## Harness contract

```bash
. scripts/e2e_overhaul/lib/fake_idp.sh
fake_idp_start scenario.json      # exports FAKE_IDP_BASE, FAKE_IDP_CA, FAKE_IDP_DIR
fake_idp_curl /.well-known/openid-configuration
fake_idp_control '{"action":"set_status","status":"granted"}'   # mutate at runtime
fake_idp_state                    # inspect devices, minted jtis, key generations
fake_idp_stop                     # terminate + reap + clean the state dir
```

The client under test must trust `FAKE_IDP_CA` (e.g. `curl --cacert`) and pin
the issuer to `FAKE_IDP_BASE`. One scenario is served per process lifetime;
restart for a fresh ceremony — process loss preserves nothing, matching the
tier-2 "a lost sub-ceremony requires a fresh explicit ceremony" rule.

## Two harness bugs worth remembering

- `python3 - <<'PY'` makes python read its **script** from the heredoc, which
  consumes stdin; a JSON document piped in is then discarded and
  `sys.stdin.read()` is empty. Pass data via `argv` with `python3 -c` instead.
- On macOS `mktemp`, the `X`s must be **trailing** — `-XXXXXX.json` is not
  substituted and collides on the second call. Drop the suffix.
- Never start the background server inside a `$(...)` capture: command
  substitution waits for the server to close the stdout pipe and hangs. Start
  it in the script body; capture only the `curl` calls.

## Scenario schema

```jsonc
{
  "secret_required": false,          // token endpoint 401s without client_secret when true
  "alg": "RS256",                    // or "ES256" — controls the signing key + header
  "issuer_path": "/idp",
  "flow": {
    "initial_status": "authorization_pending",   // or granted | access_denied | slow_down | expired_token
    "interval": 5,                   // device-response interval; omit -> default 5
    "expires_in": 900,               // device-response expiry; null omits the field
    "slow_down_after_polls": 3       // start returning slow_down (+5s each) after N polls
  },
  "claims": {                        // ID-token claim overrides
    "aud": "ee-team-client", "sub": "user-priya",
    "email": "priya@example.test", "email_verified": true,
    "groups": ["ee-team"], "lifetime_seconds": 300,
    "extra": {}, "omit": []          // add / remove arbitrary claims
  },
  "defects": {                       // token-defect injection (the attack corpus)
    "alg_none": false,               // unsigned "none" token, empty signature segment
    "wrong_kid": false,              // header kid absent from JWKS
    "bad_signature": false,          // final signature byte flipped
    "header_alg": null,              // algorithm confusion: advertise this alg, sign with `alg`
    "noncanonical_base64url": false  // padded standard base64 (invalid for JOSE)
  }
}
```

Runtime `/_control` actions: `set_status` (per-user-code or all), `rotate_keys`
(`retire_previous` optional), and `set_flow` (merge flow overrides mid-run).

## Attack corpus the client must reject (proven mintable by the defects smoke)

secret-required rejection · unsigned `alg=none` · unknown `kid` · tampered
signature · algorithm confusion · noncanonical base64url · expired
(`exp` < `iat`) · wrong audience · unverified email. Client-side network guards
(DNS-rebinding, private-URL, ambient-proxy, redirect-follow) and the
`1800`-second / `300`-request poll budgets are the verification client's
concern (T7.4/T7.5); the harness serves TLS on `127.0.0.1` and never redirects.

## Running

```bash
bash scripts/e2e_overhaul/fake_idp_harness_smoke.sh    # 16 checks, ~10s
bash scripts/e2e_overhaul/fake_idp_defects_smoke.sh    # 12 checks, ~15s
```

Both emit `ee.test_event.v1` outcome lines and exit non-zero on any failure.
They require no `ee` binary and no cargo, so they run outside the RCH lane.
