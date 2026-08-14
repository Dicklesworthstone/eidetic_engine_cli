# Team-confed performance budgets (T6.5)

These are **structural caps plus measured isolated-host proofs**, not a
Criterion wall-time gate. The same profile is emitted on
`ee team status --json` as `budgets` (`ee.team.budgets.v1`): join event
batch count, signed-relay batch bytes, body fetch bytes, and index jobs
per round. Full `cargo bench` remains opt-in via
`./scripts/verify.sh --include-bench`.

## Structural caps (enforced)

Source: `MeshAdmissionLimits::conservative_default()` and
`PreAuthAdmissionLimits::default()`, proven by
`conservative_limits_match_published_team_confed_budgets` and the
authenticated responder wrapper.

| Path | Cap | Enforcement |
| --- | --- | --- |
| Event batch count | 512 events | `decide_admission` reject |
| Event batch bytes | 4 MiB | `decide_admission` reject |
| Body fetch bytes | 512 KiB | `decide_admission` reject |
| Index jobs / round | 16 | `decide_admission` reject |
| Concurrent requests / peer | 4 | throttle |
| Pre-auth global inflight | 64 | broker `AdmissionLimited` |
| Pre-auth source inflight | 8 | broker `AdmissionLimited` |
| Unsigned bootstrap envelope | `BOOTSTRAP_MAX_ENVELOPE_BYTES` | decode reject |
| Identity-attest payload | 8192 bytes | `IDENTITY_ATTEST_MAX_PAYLOAD_BYTES` |

## Isolated-host measured proofs (2026-08-13)

Host: `ubuntu@38.242.134.66`, isolated tree
`/tmp/ee-mesh-verify/eidetic_engine_cli`, no Mac local Cargo.

| Proof | Filter | Wall | Exit |
| --- | --- | --- | --- |
| Body share/unshare lifecycle | `share_team_bodies_publishes_then_unshare_stops_serving` | 35m35s (cold crate rebuild) / test itself sub-second | 0 |
| Identity attest rejects bearer | `apply_identity_attest_frame_rejects_bearer` | <2s incremental | 0 |
| JWT RS256/ES256/EdDSA | `jwt_verifies` family | <2s incremental | 0 |
| Constrained fake-IdP HTTPS + RS256 | `constrained_https_fetches_fake_idp_jwks_and_verifies_rs256` | 1.40s | 0 |
| Live TCP identity_attest | `production_broker_applies_authenticated_identity_attest_without_bearer` | 88.42s (includes compile of loopback test) | 0 |
| Inviter enrolls joiner from accept | `enroll_joiner_from_accept_uses_source_ip_and_advertised_port` | 12m 31s compile + 56.40s test | 0 |
| Live TCP join enrolls joiner peer | `serve_one_bootstrap_join_redeems_and_records_the_joiner` | 11m 00s compile + 107.21s test | 0 |
| Plan inbound routes from enroll | `enroll_team_pair_peer_uses_the_pair_key_handle` | 13m 13s compile + 70.39s test | 0 |
| Missing store skips responder spawn | `spawn_team_responder_owner_skips_missing_store` | 16.89s compile + 0.20s test | 0 |

Join, signed relay, and body fetch were previously proven on the same
host in this campaign (live TCP hello, sync_round, join, BodyFetch).
Those wall times were dominated by compile, not by the protocol.

## Amplification rule

Index-intake jobs stay inside the 16-job / round cap. Body publication
never bypasses the 512 KiB hardened record cap
(`MAX_RECORD_BYTES` / `max_body_fetch_bytes`). A rejected peer cannot
consume local Tier-1 write budget (`local_tier1_unaffected`).
