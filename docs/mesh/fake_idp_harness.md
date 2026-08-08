# Fake OIDC IdP Harness (tier-2 SSO tests)

Status: implemented offline stimulus/oracle matrix; production consumers remain separate
Bead: bd-tc-epic-qzk7o.8.7 (T7.7)
Consumers: T7.4 (device-flow client), T7.5 (token verification), T7.6 (join attestation)

## Purpose

A deterministic, fully-offline OpenID Connect identity provider and executable
reference oracle for team-confederation tier-2 SSO acceptance tests. It needs
**no real IdP account and no outbound network**: the server binds loopback TLS,
the helper disables ambient curl configuration/proxies/netrc, and scenarios
mint protocol stimuli locally. Per ADR 0086 TC-D13, tier-2 uses the RFC 8628
device authorization grant only; this harness serves that flow.

T7.7 owns stimuli and expected dispositions. T7.4 owns production device-flow
and network enforcement, T7.5 owns production JSON/JOSE/claim/replay
enforcement, and T7.6 owns production frame, privacy, lease, bootstrap, and
grace behavior. A passing harness self-check is not a claim that those client
beads already accept or reject the stimuli.

Dependencies: `python3`, `openssl`, and `curl` — the same tools already
required by the fake-Tailscale harness. No third-party Python packages; all
key generation/signing and EC verification use the system `openssl` binary;
the self-tests also perform exact-input RSA verification with Python stdlib.
The server is stdlib `http.server` + `ssl` behind an ephemeral CA per run.

## Files

| Path | Role |
|---|---|
| `scripts/e2e_overhaul/lib/fake_idp.py` | Loopback-TLS discovery/JWKS/device/token server plus capability, poll, identity-floor, lease, bootstrap, frame, and artifact reference oracles |
| `scripts/e2e_overhaul/lib/fake_idp.sh` | Hardened Bash launcher/control client, actual same-state process restart, and retained evidence paths |
| `scripts/e2e_overhaul/fake_idp_harness_smoke.sh` | Happy path, exact RS256 verification, key rotation, and ES256 token-shape smoke |
| `scripts/e2e_overhaul/fake_idp_defects_smoke.sh` | Adversarial token/protocol minting smoke, including negative signature verification |
| `scripts/e2e_overhaul/fake_idp_selfcheck.sh` | Canonical live capability/privacy/time/lease/bootstrap matrix; preserves and extends the original two-line bead artifact |

## Harness contract

```bash
. scripts/e2e_overhaul/lib/fake_idp.sh
fake_idp_start scenario.json      # exports FAKE_IDP_BASE, FAKE_IDP_CA, FAKE_IDP_DIR
fake_idp_curl /.well-known/openid-configuration
fake_idp_control '{"action":"set_status","status":"granted"}'   # mutate at runtime
fake_idp_state                    # inspect devices, minted jtis, key generations
fake_idp_restart                  # reap/relaunch; keep durable floor, lose ceremony state
fake_idp_stop                     # terminate + reap; retain the state dir as evidence
```

The client under test must trust `FAKE_IDP_CA`, pin the configured expected
issuer, and require discovery plus signed `iss` to match it exactly.
`issuer_path` changes both served values.
`fake_idp_restart` is real process loss: the persisted identity-time floor and
non-secret outer `identity_pending` state remain, while device codes, poll
state, and the transient frame disappear and a new ceremony gets new IDs.

`GET /_state` is private, secret-bearing test introspection. It deliberately
shows device/poll ephemera and must never be treated as durable product output.
`GET /_artifact`, `GET /_artifact_views`, and the on-disk
`identity-artifact.json` are the independently scrubbed durable projections.

## Three harness bugs worth remembering

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
  "capability_profile": "identity_attested", // absent | manifest_only | identity_attested
  "logical_clock": {"wall": 1700000000, "monotonic": 0},
  "identity_floor": 1700000000,
  "project_verified_artifact": true, // invoke projection; only a verified mint may replace evidence
  "flow": {
    "initial_status": "authorization_pending",   // or granted | access_denied | slow_down | expired_token
    // omit "interval" to prove the parser-oracle default is exactly 5
    "expires_in": 900,               // absent/null/zero/overflow are emitted exactly
    "slow_down_after_polls": 3       // start returning slow_down (+5s each) after N polls
  },
  "claims": {                        // ID-token claim overrides
    "aud": "ee-team-client", "sub": "user-priya",
    "email": "priya@example.test", "email_verified": true,
    "groups": ["ee-team"], "lifetime_seconds": 300,
    "extra": {}, "omit": []          // add / remove arbitrary claims
  },
  "privacy_policy": {
    "preview_email": true,
    "allowed_groups": ["ee-team"],
    "max_allowed_group_matches": 8
  },
  "token_response": {                // transient-only sentinels are allowed here
    "access_token": "ACCESS_SENTINEL",
    "refresh_token": "REFRESH_SENTINEL"
  },
  "defects": {
    "alg_none": false,               // unsigned "none" token, empty signature segment
    "wrong_kid": false,              // header kid absent from JWKS
    "bad_signature": false,          // final signature byte flipped
    "header_alg": null,              // algorithm confusion: advertise this alg, sign with `alg`
    "noncanonical_base64url": false  // padded standard base64 (invalid for JOSE)
  }
}
```

Runtime `/_control` actions include flow/status/key/algorithm/provider changes;
the three capability profiles and event-feature disposition; logical-clock
set/advance; poll configure/attempt/repeat; identity reset/path/repair;
lease admission; atomic replay claims; bootstrap enable/verify/tick; and
renewal/grace ticks, lifecycle process traps, and ceremony purge. Every matrix
oracle call traverses the live HTTPS control endpoint.

## Deterministic acceptance matrix

| Matrix | Executable proof |
|---|---|
| Provider/team capability | Public `none` and secret-only discovery/token behavior; exact absent, manifest-only, and identity-attested feature lists; missing mandatory bits quarantine; unknown extras and unsupported receiver variants remain replayable |
| Device parser and poll time | The HTTPS-driven logical reference oracle enforces mandatory positive `expires_in`; omitted `interval` defaults exactly to 5; null/zero/negative/non-integer/overflow termination; no early request; cumulative `slow_down`; checked timeout backoff; provider, 1800-second local, and 300-request bounds with distinct reasons and one expiry class. T7.4 binds these expectations to production `/token` requests |
| Lifecycle | An actual parent/descendant inherited-pipe trap is cancelled and reaped with its partial-token buffer zeroized; terminal polls do not auto-restart; real process loss drops device/poll/frame state and requires new ceremony IDs |
| Privacy | A live token/device exchange first proves every bearer, code, URL, group, PII, and poll sentinel exists; the separate artifact file and database/manifest/audit/log/support views are byte-scanned for only subject, optional preview email, bounded allowed-group decision, and canonical provenance |
| Identity time | Every mutating path advances or atomically rejects rollback; read-only paths leave the persisted oracle byte-identical; untrusted timestamps cannot advance the floor; restart persistence, forward expiry, no backward revival, and confirmed repair suppression are covered |
| Lease/bootstrap/grace | Exact skew/cadence/evidence boundaries, derived distinct-subject checks, canonical per-subject arrival permutations, conflict expiry, monotonic policy generations, finite exact-generation bootstrap leases, post-suspension interactive recovery, and zero background IdP HTTP |

Logical wall/monotonic transitions, request counts, reason codes, projection
shape/redaction decisions, and seeded ceremony IDs are deterministic. Ephemeral
CA/key bytes, OS-assigned ports, token/thumbprint hashes, and ECDSA signatures
intentionally vary; assertions compare their protocol semantics rather than raw
runs. The projection is a reference oracle, not a production token verifier.
The happy matrix proves successful minimal projection. Each projection-capable
defect smoke deliberately invokes the same oracle and asserts that no artifact
is created; the matrix separately seeds valid evidence, then proves an invalid
mint leaves it byte-stable and marks the transient frame `token_rejected`.

## Adversarial corpus the consumers must reject

The defects smoke proves secret-required rejection, unsigned `alg=none`, unknown
`kid`, tampered signature, algorithm confusion, noncanonical base64url, expired
`exp < iat`, wrong issuer/audience, and unverified email. The matrix additionally
serves weak RSA/bad exponent/wrong-curve and ambiguous/metadata-mismatched JWKS;
fresh, validator-backed 304, stale 304, retained, and retired key views; duplicate
members at every JSON/JOSE layer; compact/base64url/size/depth faults; `crit`,
`jku`, `x5u`, `jwk`, and `x5c` headers; exact-input mutation; redirect, oversized
response, inherited-pipe, and raw diagnostic traps; and URL/DNS/replay reference
oracles. Live stall and partial-token endpoints cover timeout/cancellation, and
hostile ambient checks include `.curlrc`, proxy, CA/backend, keylog, insecure,
and netrc inputs. Every raw shape is obtained from the live TLS process and
inspected for its exact bytes, exact signature, or state transition. Production
rejection remains T7.4/T7.5 work.

## Running

```bash
bash scripts/e2e_overhaul/fake_idp_harness_smoke.sh    # happy path
bash scripts/e2e_overhaul/fake_idp_defects_smoke.sh    # defect corpus
bash scripts/e2e_overhaul/fake_idp_selfcheck.sh        # full live matrix
```

All three emit schema-valid `ee.test_event.v1` assertion rows, require no `ee`
binary or Cargo, retain their scenarios/state directories, and exit non-zero on
any failure. `scripts/verify.sh` keeps the original two stages visible and runs
the matrix self-check as a third stage immediately afterward.
