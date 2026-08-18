# SRR6 Mesh Verification Matrix

Status: SRR6 contract active; team-confed Unix live ledger current as of 2026-08-17
Bead: bd-26d7w (SRR6) / bd-tc-epic-qzk7o (team-confed)
ADR: docs/adr/0037-optional-mesh-memory.md / docs/adr/0086-team-memory-confederation.md

## Purpose

SRR6 mesh work is optional, local-first, and policy-gated. This matrix defines
the shared proof and logging contract for all SRR6 implementation and test
beads so each slice produces comparable evidence instead of one-off scripts.

Every mesh verifier must prove two things:

- Mesh-off behavior is indistinguishable from ordinary local `ee` behavior
  unless mesh is explicitly enabled.
- Mesh-on behavior is deterministic, provenance-preserving, privacy-safe, and
  scoped to the active workspace and peer group.

## Evidence Matrix

| Evidence | Required scope | Normal verify | RCH friendly | Optional long-running |
| --- | --- | --- | --- | --- |
| Unit | Pure parsers, policy decisions, namespace binding, retry math, redaction decisions | Yes, through `cargo test --lib` | Yes | No |
| Integration | CLI JSON contracts, DB/repository rows, import ledgers, cache rows | Yes, focused test targets | Yes | No |
| E2E | Mesh-off, two-node local fixture, three-node replay/partition fixtures | Yes for mesh-off and no-live-service fixtures | Yes when expressed as `cargo test` or shell with an existing binary | Real Tailscale smoke only |
| Golden | Stable JSON, support bundle, handoff, status/doctor, event envelopes | Yes | Yes | No |
| Perf | Search freshness probes, two-tier latency budgets, cache hit paths | No by default | Yes for compare-only benches | Full benchmarks |
| Privacy | Redaction, body/embedding denial, workspace isolation, support bundle leaks | Yes | Yes | No |
| Failure mode | Missing peer binding, stale revision, partition, authorization denied, cache quota | Yes for synthetic fixtures | Yes | Fault-injection soak |
| Model checks | Anti-entropy, stale-read bounds, convergence/idempotence invariants | No by default | Yes for bounded deterministic harnesses | Larger state spaces |

`scripts/verify.sh` should run the normal-verify rows once their backing
features exist. Optional long-running checks must be explicit opt-in and must
not gate local-first non-mesh work.

## SRR6.20 Two-Tier Budgets

`tests/mesh_two_tier_budget.rs` is the RCH-friendly structured proof for
`bd-3url9`. It exercises the pure `ee.mesh.two_tier_budget.v1` report and
records these default foreground budgets:

| Budget | Default |
| --- | --- |
| Tier 1 local answer p50 | 75 ms |
| Tier 1 local answer p99 | 250 ms |
| Async peer probe timeout | 750 ms |
| Stale-read window under normal sync cadence | 5000 ms |
| Peer freshness fanout | 32 peers |
| Lazy body-cache growth per foreground read | 512 KiB |
| Sync batch size | 512 events |
| Index-job amplification per round | 16 jobs |

The report must keep `localAnswerBlocking=false`, `networkOnTier1=false`, and
`bodyTransferAllowedOnTier1=false`. Cache-hit evidence is reported explicitly
through `cacheHitPathObserved`; missing cache-hit evidence is treated as a
budget degradation for the SRR6.20 proof row.

## Fixture Layout

Use these paths for new SRR6 tests:

```text
tests/fixtures/mesh/
  <scenario>.json                  # static event/config fixtures
  <scenario>/node01/               # retained no-live-service node fixture
  <scenario>/node02/
  <scenario>/node03/

tests/fixtures/golden/mesh/
  <scenario>.<surface>.json.golden # stable JSON contracts

scripts/e2e_overhaul/
  mesh_<scenario>.sh               # shell e2e using J1/J3 logging

tests/
  mesh_<scenario>.rs               # RCH-friendly Cargo companion when needed
```

Node ids are always `node01`, `node02`, and `node03`. Use role labels only in
test descriptions (`primary`, `peer`, `relay`, `partitioned`); machine output
uses the stable node id. Workspaces created by shell e2e scripts live under
`$EPIC_WORKSPACE/mesh/<scenario>/<nodeId>/workspace`.

## Temp And Clock Rules

- Shell e2e scripts use `epic_setup` from
  `scripts/e2e_overhaul/lib/shared.sh`. Cargo integration tests that run on
  RCH workers use `/tmp` for temporary workspaces instead of inheriting a
  host-specific `TMPDIR`.
- Timestamps in fixtures are fixed RFC 3339 UTC values. Runtime logs may use
  the J1 logger timestamp, but assertions must not depend on wall-clock order
  beyond monotonic phase ordering within one log.
- Node ids, peer ids, workspace ids, event ids, and revision tokens are stable
  fixture strings unless the test is specifically about id generation.
- Tests must not require real Tailscale unless the bead is an explicit
  opt-in transport smoke test.
- The opt-in real transport smoke is
  `scripts/e2e_overhaul/mesh_tailscale_smoke.sh`. It exits `78` by default and
  only runs against a live tailnet when `EE_E2E_REAL_TAILSCALE=1` and
  `EE_REAL_TAILSCALE_PEER` are both set.

## Structured E2E Log Contract

All shell mesh e2e scripts source `scripts/e2e_overhaul/lib/shared.sh` and emit
`ee.test_event.v1` JSONL through `scripts/lib/e2e_logger.sh`.

Every script emits phases in this order:

1. `setup`
2. `action`
3. `assert`
4. `cleanup`

Use `mesh_phase_log <phase> <nodeId|scenario> <message>` for phase notes. The
helper stores these machine fields under `fields`:

```json
{
  "phase": "setup",
  "meshScenario": "mesh_off_no_network",
  "meshNode": "node01",
  "message": "node_workspace path=..."
}
```

Each script ends with a summary note containing:

- scenario name
- pass/fail assertion counters
- node count
- fixture root or retained workspace manifest

Raw command stdout, raw stderr, memory bodies, peer secrets, and full remote
workspace paths do not belong in mesh logs. Use hashes, fixture names, node ids,
redaction-safe aliases, and retained artifact paths.

## Scenario Helpers

`scripts/e2e_overhaul/lib/shared.sh` provides the common shell helpers:

- `mesh_scenario_setup <scenario> <node-count>` creates
  `$EPIC_WORKSPACE/mesh/<scenario>/nodeNN/{workspace,config,logs,goldens}` and
  emits `setup` phase rows.
- `mesh_node_workspace <nodeId>` prints the workspace path for a node and
  creates it if missing.
- `mesh_phase_log <phase> <nodeId|scenario> <message>` emits one structured
  `ee.test_event.v1` note with mesh phase fields.

Shell scripts may still call `ee_workspace` for single-workspace mesh-off
checks. Multi-node scripts call `mesh_node_workspace node01` and pass
`--workspace "$path"` explicitly.

## SRR6 Bead Mapping

| Bead | Required proof from this matrix |
| --- | --- |
| bd-x4hn7 | Mesh disabled by default; ordinary JSON is not polluted; no listener appears |
| bd-162sk | Byte-stability, no-network regression, and golden output parity |
| bd-3k16v | Replay convergence and partition/rejoin invariants |
| bd-3i5q7 | Privacy, redaction, body/embedding denial, and support-bundle leak checks |
| bd-3url9 | Latency, freshness, resource-budget, and cache-hit evidence |
| bd-ghey6 | Local two-node fixture without real Tailscale |
| bd-1crtj | Explicit opt-in real Tailscale smoke via `scripts/e2e_overhaul/mesh_tailscale_smoke.sh`, quarantined outside normal verify |
| bd-3omr5 | Agent-facing command modes and JSON contracts |
| bd-2irom | Embedding/search-surrogate privacy and compatibility |
| bd-2vu8m | Final matrix audit proving every SRR6 shipped surface has unit proof plus e2e or golden proof |

New SRR6 test beads must reference this document in their description or first
tracker comment and must state which matrix rows they satisfy.

## Closeout Checklist

Before closing an SRR6 implementation bead, record:

- Unit or integration test command and result.
- E2E, golden, privacy, failure-mode, perf, or model-check evidence required by
  the bead mapping.
- Whether verification ran under RCH, local shell with an existing binary, or
  optional real transport smoke.
- The structured log path or artifact manifest path when a shell e2e ran.
- Any matrix rows intentionally deferred to a child bead.

Before closing `bd-2vu8m`, record the final rollup in the bead thread:

- `br dep cycles --json --no-db` result and the command timestamp.
- `scripts/closeout_audit.sh --bead bd-2vu8m --json` readiness, blockers,
  caveats, and artifact path when retained.
- Every SRR6 child bead that is not closed, with explicit status, deferral
  rationale, owner, and the proof row that remains unverified.
- Mesh-disabled proof status for
  `scripts/e2e_overhaul/mesh_off_no_network.sh`, `tests/mesh_off_no_network.rs`,
  and `tests/fixtures/golden/mesh/mesh_off_no_network.commands.json.golden`.
- RCH posture for every Cargo command. If RCH refuses remote execution or reports
  local fallback, record the reason and keep the Cargo proof gate unverified.

## Team confederation closeout (ADR 0086 / T6.7)

Campaign: `bd-tc-epic-qzk7o`. Proof host: isolated CSS
`ubuntu@superserver` trees `/home/ubuntu/ee-mesh-campaign-verify` and
`/home/ubuntu/ee-mesh-campaign-verify-pinned` (franken-stack.lock
siblings at pinned revisions; no Mac local Cargo). The earlier isolated
host `ubuntu@38.242.134.66` is pubkey-denied as of 2026-08-17.
This is a live Unix EE-to-EE ledger, not a bead-closing ceremony.

Unix product is shipped. The v1 remainder set is closed. Post-v1
follow-ups live on `bd-tc-followup-oo7d2` and are not children of this
epic:

| Remainder | Owner | Why it stays open |
| --- | --- | --- |
| Two-human Tailscale US-4 soak | `bd-tc-epic-qzk7o.3.8` (T2.6) **closed 2026-08-17** | Live two-host tailnet soak: css origin `100.90.148.85:41888` + hz1 joiner `100.87.220.62`. Invite/join over Tailscale IPs; authenticated EventFetch after pair-key+LocalAPI (`8055b5b1`); origin body-lane grant gen 1; joiner `mesh sync --once` `contactedPeers=true` / `health=synced` with no `mesh_sync_once_network_deferred`. `ee search`/`ee pack --memory-scope team` returned `US4-SOAK-MARKER Acme Corp Q3 analysis teammate text` with `teamProvenance.memberDisplayName=T26-US4-Soak`. Two tailnet hosts, not two distinct human operators. |
| T5.7 publication fence | `bd-d67os.28` **closed 2026-08-17**; `.6.7` **closed 2026-08-17** | NavyLotus protocol (no fence rewrite). Isolated CSS pinned tree `24193e89` 2026-08-17: `single_processor_never_stamps_post_snapshot_commit_current` 111.39s EXIT 0; `limited_coalesced_processor_fences_unclaimed_and_post_snapshot_jobs` 52.49s EXIT 0. Prior RCH lib tests 2/2 in 42.56s on vmi1149989 at the same commit. Multiprocess E2E `multiprocess_write_after_source_snapshot_is_present_or_explicitly_stale` 334.26s EXIT 0 on CSS: 256-doc snapshot, `index status` health=stale with rebuild repair, second writer absent from search, coalesce repair to ready, unique phrase then present. Search may disclose `search_index_degraded` when its freshness probe fails under hash_fallback; that path still refuses to claim Current. Team-scale coalesce remains `inbound_index_jobs_coalesce_under_amplification_cap` (500-row → 1 Incremental job). RCH E2E on vmi1156319 compiled 16m07s then failed as environment (`ee.write.lock` 38s no-progress / worker disk). |
| Windows-host DACL / inbound crash / key-path soak | `bd-tc-epic-qzk7o.12` (`.2.4` **closed 2026-08-17**; `.12` **closed 2026-08-17**) | `.2.4` live: SurfaceBookJE `ee 0.13.1` `14:53` `target\release\ee.exe` `team create` on `C:\Users\jeffr\ee-tc-win-soak5`. `.ee\keys\mesh` DACL `D:P` TokenUser (`SURFACEBOOKJE\jeffr`) + SYSTEM, no inherited ACEs; signing record same. `.ee` ancestors stay inherited so `ee.db` remains writable (`19dcfd8c`). Reparse junction on soak2-reparse refused (`path is (or traverses) a symbolic link`). `.12` live 2026-08-17: soak6 `C:\Users\jeffr\ee-tc-win-soak6` store row `wsp_700J9NPX8BKY4TQF8XJDA2RHSS` (`\\?\C:\...`); invite `66804dc593b6a64ac56eb04549d75595` redeemed by css `/home/ubuntu/ee-tc-win-soak6b-join` over Tailscale `100.90.148.85` → `100.120.65.94:41888`. Enroll writes `mesh_peers` on the store row (`86541697`). `hello-responder run` `05:56` `ee.exe` binds `100.120.65.94:41888` (`c7cd4702` routed listen + `d4a159d2` TeamJoin WhoIs). css `ee mesh sync --once` `contactedPeers=true` / `health=synced` / `importedEventCount=1`. Kill PID 23208 only, restart PID 19864; inbound hello+sync still returns genesis `blake3:f7a3a19d83518780777a8ff075e9f3b30088e7cb885f922759a5c28129b35658`. Post-restart unsigned hello took 4–8s on Tailscale, so default foreground TCP sync timeout is 15s and `--time-budget-ms` defaults to 20000. |
| Production IdP vendor soak | `bd-tc-epic-qzk7o.8.8` **closed 2026-08-17** | Fake-IdP RS256 + live `identity_attest` is the v1 ceiling. No Entra/Okta/Google secretless public client exists in this environment. T7.1–T7.7 (preflight, constrained HTTPS device client, JWKS verify, token-free attestation, fake harness) remain shipped. Later vendor soak is `bd-tc-followup-oo7d2.8`, not a v1 blocker. |
| Program closeout | `bd-tc-epic-qzk7o.7.7` (T6.7) **closed 2026-08-17** | Every tc-* implement child is closed. Post-v1 follow-ups filed as `bd-tc-followup-oo7d2` (.1 port migration, .2 roles/quorum, .3 Windows service + local control, .4 selective-sync, .5 multi-team, .6 rustls, .7 encrypted credential-backup, .8 production IdP). `br dep cycles --json --no-db` 2026-08-17: `active_count=0`. Vision coverage gap 0.74% (missing surface is the documented `"..."` placeholder, not a team verb). Closure-lint fail is pre-existing `bd-3usjw.4` `serve_localhost` deferral, not a team-confed hole. Do not start `bd-1nl13`. |
| Windows local control transport | `bd-tc-followup-oo7d2.9` **closed 2026-08-18** | Loopback TCP + owner-only `%LOCALAPPDATA%\eidetic-engine\mesh-responder.control` endpoint file (`ee.mesh.responder.control.endpoint.v1`). Unix UDS unchanged. Isolated CSS 2026-08-18 `/home/ubuntu/ee-mesh-campaign-verify-pinned`: `cargo test --lib responder_broker` 13 passed in 0.26s. SurfaceBookJE native `CARGO_TARGET_DIR=C:\Users\jeffr\ee-win-task-target` 14 passed in 0.34s including `windows_control_endpoint_publishes_and_status_submits` and `windows_owner_safe_path_accepts_existing_workspace`. Live soak `C:\Users\jeffr\ee-tc-win-control-soak` `WinControlSoak` `team_3c50e4053fd45185e8bdb59bdf`: invite `08f5935b50364acebedf9412b5d6ad3e` redeemed (`peer_8172d940cc571dda65183dd6325abbf2`); debug `ee.exe` `hello-responder run --port 41889` bound `100.120.65.94:41889` `registeredRoutes=1`; endpoint `transport=loopback_tcp host=127.0.0.1 port=62014`; `hello-responder status --json` `running=true` `listenAddress=100.120.65.94:41889` `degraded=[]` after 50ms control poll. Register delivers a control-frame reply; re-resolve of an already-running owner still returns `PlatformUnsupported` (`mesh_transport_unreachable`) and is not required to prove the transport. Named-pipe listen remains a later slice. |
| Encrypted credential backup | `bd-tc-followup-oo7d2.7` **closed 2026-08-18** | First-class `ee team credentials backup\|restore --passphrase-stdin`. Envelope schema `ee.mesh.credentials.backup.v1`; payload sealed with blake3-derive-key + ring CHACHA20-POLY1305. Written through `SecureLocalDir` under `.ee/keys/mesh-credential-backup/` (Unix 0600 / Windows TokenUser+SYSTEM DACL). Isolated CSS 2026-08-18 `/home/ubuntu/ee-mesh-campaign-verify-pinned`: `cargo test --lib credential_backup` 6 passed in 1.32s. Live CLI soak `/home/ubuntu/ee-cred-backup-soak-20260818`: `team create CredBackupSoak` then backup `storePresent=true pairCount=0 signingCount=1`, restore wrote `signing.node_987f14217476edd62346865fa87baacf.current.json` mode 600, envelope 600 / dir 700, wrong passphrase fail-closed `credential backup could not be decrypted`. Ordinary `ee backup` still redacts `peerCredentials`. |

Do not start `bd-1nl13`. v1 remainder children are closed.

| Slice | Status | Proof | Remainder |
| --- | --- | --- | --- |
| T1–T4 transport, origin, join, authorizer, pair-key, leave/pause | **Proven** | Isolated live TCP hello, sync_round, join, revoke, history share, signed inbound persist. Create/join raise the invite-authorization floor. `ee team projects reconcile` rematerializes origin `teamProjectShared` rows (V116) whose `projectId` is `prj_tm_` plus 26 chars. Isolated 2026-08-14: `reconcile_rematerializes_origin_project_shares` 60.95s exit 0; `resume_pending_invite_omits_the_secret` 51.71s exit 0. | A 29-char fixture id is ignored by reconcile; doctor now uses the same id check |
| T5.1–T5.8 pack scope, activity, attribution | **Landed; search/pack/ask/why/activity recall teammate text; P4.4 precedence/conflict wired** | Authorized BodyFetch hydrates and indexes teammate text. Search hits, pack markdown, `ee pack --json` items, and pack JSONL carry `teamProvenance`. Inbound ids are typed Crockford `mem_*` so pack can parse them. Apply/steward hydrate leftover stubs from already-available cache. `ee team activity` attributes inbound projections with member name, origin `producedAt`, and `bodyAvailable` after hydrate. Isolated 2026-08-16: `inbound_team_memory_id_is_a_parseable_memory_id` 0.02s; `hydrated_team_memory_is_searchable_under_team_scope` (search+pack) 190.29s; `apply_fetched_team_body_hydrates_already_available_cache` 119.34s; `steward_hydrates_leftover_history_stubs_from_available_cache` 127.64s; `list_team_activity_attributes_hydrated_inbound_memory` 58.61s then 59.20s with `--member`; `list_team_activity_filters_by_project_binding` 59.11s; `list_team_activity_since_excludes_earlier_events_and_labels_incompleteness` 50.38s; `list_team_activity_cursor_pages_without_overlap` 59.08s; live TeamJoin BodyFetch then joiner `ee pack --memory-scope team` (`team_join_start_durable_serves_authenticated_body_fetch`) 298.09s; `enroll_team_pair_peer_persists_remote_member_without_config` Finished 53m 33s, test 95.42s EXIT 0 so Team scope admits the enrolled teammate without `trust.team_members`; live BodyFetch then pack without config.toml (`team_join_start_durable_serves_authenticated_body_fetch`) 3m 18s + 345.61s EXIT 0; `join_team_first_sync_imports_origin_genesis` 10m 24s + 108.06s EXIT 0 (join applies origin genesis onto the import ledger); live `serve_one_bootstrap_join_redeems_and_records_the_joiner` after invite waiter first-sync 53m 06s + 96.58s EXIT 0 (`firstSync.complete`, importedEvents >= 1); `team_scope_ignores_unauthenticated_workspace_config_team_members` 39m 19s + 0.01s EXIT 0; P4.4 `team_lane_pack_policy` 30m 19s + 0.00s EXIT 0 (contradiction keeps both sides, overlap records local override, sealed body stays unassessed); `content_across` / `workspace_team_contradiction` / `three_lane_overlap` / `lane_specificity_is_workspace` EXIT 0; T5.3 `team_provenance` (`explain_memory_emits_team_provenance_for_peer_human_attested`, `ask_citation_json_includes_team_provenance`, `team_provenance_from_peer_human_attested_memory`) 3 passed in 65.82s EXIT 0; T5.6 `ee insights --section peerConflicts` wired through `surface_precedence_conflicts` + `detect_peer_memory_conflicts`; isolated `peer_conflicts_section_surfaces_team_lane_contradiction` Finished 51m 39s + 106.34s EXIT 0; `ee why` elevation + T5.8 origin-time invariance: isolated `does_not_rerank_on_origin` 17m 36s + 0.04s EXIT 0; `explain_memory_emits_team_provenance` 7.78s + 101.14s EXIT 0. Isolated 2026-08-17: inbound Incremental jobs persist `document_source=memory` so they pass the search_index_jobs CHECK; `project_inbound_team_memory_persists_project_binding` and `project_inbound_team_memory_writes_a_metadata_stub` Finished 34m 53s + 118.79s, 2 passed EXIT 0 (`trust_subclass` carries `project=acme-analysis`); `inbound_index_jobs_coalesce_under_amplification_cap` 500-row burst Finished 29m 11s + 37.16s EXIT 0; origin `origin_trust_claim` persisted and rendered (`team_provenance_includes_origin_trust_claim`, why elevation `fromTrustClass=agent_assertion`, persist `origin_trust=human_explicit`) 5 passed in 63.49s then persist 36.22s EXIT 0. `trust.team_members` is no longer a closed config key; leftover nickname lists fail as unknown. Rematerialize retries Incremental enqueue for already-projected stubs. Isolated 2026-08-17: T5.7 source-snapshot protocol tests `single_processor_never_stamps_post_snapshot_commit_current` and `limited_coalesced_processor_fences_unclaimed_and_post_snapshot_jobs` 2 passed in 136.31s EXIT 0 on the same isolated tree (inbound drain uses this coalesced publisher). Fence closed 2026-08-17 on `bd-d67os.28` / `.6.7`. | Isolated 2026-08-17: human search attribution suffix + US-5 `ee team status` last-sync/reachability (`synced 4m ago` / `never synced`) on `7cc3678a`. Live two-host Tailscale US-4 closed on `bd-tc-epic-qzk7o.3.8` / `8055b5b1`. |
| T5.9 Unix body product | **Proven** | `share_team_bodies_publishes_then_unshare_stops_serving` exit 0; live BodyFetch; confirm gated on secure-file. Isolated 2026-08-14: `inbound_body_placeholder_verifies_nonce_before_publication` Finished 15m 33s, test 132.74s exit 0. Inbound `exact`/`already_redacted` events persist a producer-keyed `metadata_only` row; authorized serve releases `nonceHex`; `apply_fetched_team_body` recomputes the event commitment before `staging→available`; mismatch quarantines. Omitted history share stays body-free. | Files never deleted; reconcile does not resurrect. Sync and `ee team fetch body` retry granted BodyFetch after EventFetch. |
| T5.9 Windows SID/DACL/reparse | **Live key-path + inbound crash/restart proven on SurfaceBookJE** | Isolated `x86_64-pc-windows-gnu --lib` still compiles. Live 2026-08-17: `56f2cea8` in-place harden, `909a1e7a` TokenUser+writable dir handle, `19dcfd8c` leaf-only DACL. Soak5 `team create` `WinSoak5` `team_540f2c013aee8acd0022ba40eb`; mesh DACL `D:P(A;;FA;;;S-1-5-21-...-1001)(A;;FA;;;SY)`; reparse junction denied. Soak6 enroll + hello-responder crash/restart: css joiner `ee mesh sync --once` then kill/restart Windows PID; inbound hello+sync still serves genesis. | Tailscale WhoIs remains Unix; TeamJoin inbound is the Windows path |
| T5.10 US-6 E2E | **Proven (Unix product harness)** | Token bind/drift/expiry/wrong-store, unlinkable previews, crash lifecycle. Isolated 2026-08-14: `already_redacted` 58.60s, `body_lane_grant_then_revoke_gates_fetch` 61.08s, `inspect_team_health` origin_outbox 52.54s, `substituted_body_cache_bytes_stay_metadata_only` 54.58s. `ee team share bodies --representation`; hash-checked fetch | No Windows-host crash killer |
| T6.1 steward | **Proven** | Isolated 2026-08-14: membership, projects, orphaned Next pair, inbound memory rematerialize, SingleDocument index enqueue, inbound body placeholders, join peer enroll, and grant-gated fetch retry. Invite/grant/enroll now carry `originWorkspaceId`. Isolated 2026-08-14: `enroll_team_pair_peer_uses_the_pair_key_handle` 12m 53s + 52.25s exit 0 (stores remote workspace; `plan_team_body_fetch_binding` is Some only when workspaces and nodes are distinct). Retry calls fetch only when the durable body lane is Allow, then applies nonce-checked bytes. Isolated 2026-08-14: `enroll_joiner_from_accept_uses_source_ip_and_advertised_port` 12m 31s + 56.40s exit 0; live `serve_one_bootstrap_join_redeems_and_records_the_joiner` 11m 00s + 107.21s exit 0 (inviter stores joiner at source IP + advertised hello port + `joinerWorkspaceId`). | Live authenticated BodyFetch runs after the Send supervisor via current-thread `block_on`. `ee team fetch body` retries the same path when local cache is metadata-only. `ephemeral_source_for` binds loopback remotes on 127.0.0.1/[::1] and routed remotes on the UDP-selected local IP. Isolated 2026-08-14: mesh-off sync+fetch 14m 52s + 116.71s; `ephemeral_source_for_loopback_and_routed_remotes` 10m 25s + 0.01s, both exit 0. Still needs pair key, distinct workspaces, and Body-lane Allow. |
| T6.2 daemon install | **Proven on Linux and macOS user supervisors** | Linux: `systemctl --user start ee-team-confed-proof.service` → `ActiveState=active` at 2026-08-14T01:47:44Z. macOS: `launchctl bootstrap gui/501/ai.eideticengine.ee-team-confed-proof` → `active count = 1` / `state = xpcproxy` at 2026-08-14T02:14:58Z. Both units quarantined by rename. Isolated 2026-08-14: `enroll_team_pair_peer_uses_the_pair_key_handle` 13m 13s + 70.39s exit 0 (`plan_team_responder_registrations` emits the pair-key handle; team-join tailnet matches current LocalAPI; ungranted team-join handshake is enough for inbound EventFetch). `spawn_team_responder_owner_skips_missing_store` 16.89s + 0.20s exit 0. `ee mesh hello-responder run --workspace .` auto-loads enrolled peers. `ee daemon --foreground` (not `--once`) starts that owner when mesh is on and peers exist. | Real `ee` binary KeepAlive service was not left running. Windows `ee daemon install` live 2026-08-17 SurfaceBookJE: `kind=windows_user_task`, XML under `%USERPROFILE%\AppData\Local\eidetic-engine\`, `schtasks` create SUCCESS, query Ready/Interactive/logon as `jeffr`, uninstall removes the task and quarantines the XML. Inbound is still TeamJoin TCP. Local control UDS listen remains Unix; Windows default control path is `%LOCALAPPDATA%\eidetic-engine\mesh-responder.control`. launchd/systemd install stays Unix. Missing tailscaled falls back to `TeamJoinLocalApi`. A local team turns mesh on unless `EE_MESH_ENABLED=0` or `mesh.enabled = false`. Isolated 2026-08-14: `local_team_enables_mesh_unless_explicitly_disabled` 13m 57s + 123.79s exit 0; mesh-off snapshot without a team still 11m 52s + 115.11s exit 0. Isolated 2026-08-15: `team_join_local_api_start_durable_binds_loopback` 7m 53s + 96.48s exit 0 — owner binds 127.0.0.1 after join without tailscaled. Isolated 2026-08-15: `enroll_team_pair_peer_uses_the_pair_key_handle` 11m 47s + 61.01s exit 0 — `InboundLocalApi::prefer` selects TeamJoin when every enrolled endpoint is loopback, even if tailscaled is installed. Isolated 2026-08-15: `team_join_start_durable_serves_unsigned_hello_sync` 11m 13s + 94.45s exit 0 — after TeamJoin `start_durable`, `serve_one` answers unsigned hello and returns the genesis origin event, the same path `ee mesh sync --once` uses. Isolated 2026-08-15: `team_join_start_durable_serves_authenticated_event_fetch` 8m 31s + 111.87s exit 0 — pair-key EventFetch through the same owner returns the genesis event. Isolated 2026-08-15: `team_join_start_durable_serves_authenticated_body_fetch` 12m 35s + 170.26s exit 0 — grant-gated BodyFetch returns the published bytes. Isolated 2026-08-15: `team_join_start_durable_denies_ungranted_body_fetch` 10m 08s + 110.73s exit 0 — without Body-lane Allow the same owner answers metadata-only and leaks no bytes. Isolated 2026-08-15: `team_join_start_durable_applies_authenticated_identity_attest` 9m 22s + 116.34s exit 0 — token-free identity_attest persists the member login on the origin store. |
| T6.3 admission | **Proven** | Authenticated serve folds decisions into a broker-owned per-peer map and persists a V118 snapshot. Isolated 2026-08-14: `persisted_admission_snapshot_warns_doctor_and_status` 56.71s exit 0; inspect 56.12s exit 0. Status/doctor report throttled/exhausted counts and coalesced exhaustion after the broker exits. | Tailscale LocalAPI WhoIs remains Unix; TeamJoin inbound is cross-platform |
| T6.4 doctor | **Proven** | Isolated 2026-08-14: `inspect_team_health_reports_no_team_then_ok_then_paused` 65.39s exit 0. `broker_port` now compares genesis hello port to `EE_MESH_HELLO_PORT`; mismatch is a warning. `whois` no longer claims a live Tailscale probe. | Tailscale LocalAPI WhoIs remains Unix; TeamJoin inbound is cross-platform |
| T6.5 budgets | **Proven** | `ee team status` emits `budgets` (`ee.team.budgets.v1`) naming join event-batch count, signed-relay batch bytes, body fetch bytes, and index jobs/round. Isolated 2026-08-14: `team_confed_budget_profile_names_join_relay_body_and_index_caps` 53.51s exit 0. At-cap EventBatch is allowed; +1 is rejected with `local_tier1_unaffected`. Isolated 2026-08-15: `[[bench]] team_confed` compiles (`cargo test --bench team_confed --no-run` Finished 19m 21s). Isolated 2026-08-15: `cargo bench --bench team_confed` Finished 68m 21s EXIT 0 — derive_pair_key 1.99 µs, admission EventBatch/BodyFetch ~80 ns, create_and_enroll 34 s (migrate+genesis). Loopback inbound TCP connect 8m 29s + 99.62s exit 0. | Repeatable via `./scripts/verify.sh --include-bench`; create_and_enroll is migrate-dominated |
| T6.6 docs | **Landed** | `docs/team/quickstart.md`, `trusted_vs_contractor.md`, `docs/agent-ux/team.md`, CHANGELOG | — |
| T7.1–T7.6 IdP | **Proven** | Fake-IdP HTTPS RS256 1.40s; live TCP identity_attest 88.42s compile+test | No production IdP vendor soak |
| T7 Windows / client-only | **Fail-closed** | Same as T5.9 | — |

Campaign is **done as live Unix EE-to-EE capability**, including
inbound-memory rematerialize, SingleDocument index enqueue,
nonce-checked body apply, join peer enroll, and grant-gated
BodyFetch retry. Isolated 2026-08-14T18:14–18:31Z: rematerialize
72.43s, project 78.90s, all exit 0. Isolated 2026-08-14T18:42–19:00Z:
inbound body placeholder 15m 33s compile + 132.74s test, exit 0.
Isolated 2026-08-14T19:20–20:05Z: enroll 13m 58s + 53.11s, retry
18m 44s + 57.20s, both exit 0. Isolated 2026-08-14T20:20–20:40Z:
enroll+binding 12m 53s + 52.25s exit 0. Isolated 2026-08-14T20:45–21:10Z:
after-sync BodyFetch 14m 52s + 116.71s exit 0; mint/redeem
originWorkspaceId 56.97s exit 0. Isolated 2026-08-14T21:15–21:26Z:
`ephemeral_source_for` 10m 25s + 0.01s exit 0. Isolated
2026-08-14T21:40–22:20Z: inviter enrolls the accepted joiner
(`enroll_joiner_from_accept` 12m 31s + 56.40s; live join 11m 00s +
107.21s, both exit 0). Hello now carries `joinerHelloPort` and
`joinerWorkspaceId`. Isolated 2026-08-14T22:30–22:55Z: inbound
auto-load 13m 13s + 70.39s and spawn-skip 16.89s + 0.20s, both
exit 0. Isolated 2026-08-14T23:10–23:35Z: TeamJoin LocalAPI WhoIs
22m 52s + 59.44s exit 0. Missing tailscaled binds loopback from
enrolled endpoints. Isolated 2026-08-14T23:50–00:20Z: a local team
enables mesh (13m 57s + 123.79s); mesh-off without a team still
holds (11m 52s + 115.11s). Isolated 2026-08-15: TeamJoin
`start_durable` binds loopback 7m 53s + 96.48s exit 0. Isolated
2026-08-15: inbound TCP connect 8m 29s + 99.62s; `[[bench]]
team_confed` compiles in 19m 21s. Isolated 2026-08-15: TeamJoin
`serve_one` returns the genesis event over unsigned hello+sync
11m 13s + 94.45s exit 0. Isolated 2026-08-15: authenticated
pair-key EventFetch through TeamJoin inbound 8m 31s + 111.87s
exit 0. Isolated 2026-08-15: grant-gated BodyFetch through the
same owner returns published bytes 12m 35s + 170.26s exit 0.
Isolated 2026-08-15: ungranted BodyFetch stays metadata-only
10m 08s + 110.73s exit 0. Isolated 2026-08-15: TeamJoin inbound
identity_attest persists login 9m 22s + 116.34s exit 0.
Isolated 2026-08-15: `cargo bench --bench
team_confed` Finished 68m 21s EXIT 0 (derive_pair_key ~2 µs,
admission ~80 ns, create_and_enroll ~34 s migrate-dominated).
Windows inbound TeamJoin TCP compiles (`x86_64-pc-windows-gnu --lib`
Finished 25m 06s EXIT 0). Isolated 2026-08-15: authorized BodyFetch
hydrates the team stub (`hydrate_inbound_team_memory_body_replaces_history_stub`
32m 07s + 63.77s EXIT 0) and `hydrated_team_memory_is_searchable_under_team_scope`
10m 53s + 194.00s EXIT 0 so `--memory-scope team` search returns teammate text.
Isolated 2026-08-16: that same searchable test now also packs the hydrated
memory (190.29s EXIT 0) after inbound ids became typed Crockford `mem_*`.
Retry/steward leftover-stub hydrate 119.34s / 127.64s; inbound activity
attribution 58.61s. Tailscale LocalAPI WhoIs stays Unix. Do not self-close
beads from this ledger.
