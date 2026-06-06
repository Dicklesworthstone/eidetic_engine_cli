# RCH Verification Wrapper

Use `scripts/rch_verify.sh` for focused Rust verification in this repository.
It always builds an explicit `rch exec -- env TMPDIR=/tmp ...` invocation and
emits a JSON proof that can be copied into a Beads comment.

Examples:

```bash
scripts/rch_verify.sh --dry-run -- cargo test --lib output::streaming -- --nocapture
scripts/rch_verify.sh -- cargo clippy --all-targets -- -D warnings
scripts/rch_verify.sh -- cargo fmt --check
scripts/rch_verify.sh --bead-id bd-123 --ledger .ee/derived/rch/runs.jsonl --summary -- cargo test --test mesh_off_no_network -- --nocapture
scripts/rch_verify.sh --skip-build-admission -- cargo test --lib focused_case -- --nocapture
```

Accepted verifier shapes are `cargo check`, `cargo test`, `cargo bench`,
`cargo clippy`, and `cargo fmt --check`. `cargo fmt --check` is accepted for
contract consistency, but current RCH may classify it as non-compilation; its
proof therefore reports `would_offload=false`. Other commands are refused unless
`--allow-raw` is passed. Raw commands still run through explicit `rch exec`, but
the JSON marks `would_offload=false` because RCH may decline non-compilation
commands.

The wrapper sets these remote-safe defaults:

- `RCH_REQUIRE_REMOTE=1`
- `RCH_QUEUE_WHEN_BUSY=1`
- `RCH_COMPRESSION=0`
- RCH binary `/Users/jemanuel/.local/bin/rch-manifestfix-20260605-5` when present, then `/Users/jemanuel/.local/bin/rch-33720a8`, then `/Users/jemanuel/projects/remote_compilation_helper/target-local/release/rch`, then `rch`
- `RCH_CANONICAL_PROJECT_ROOT=/Users/jemanuel`
- `RCH_ALIAS_PROJECT_ROOT=/data`
- remote command `TMPDIR=/tmp`
- remote command `CARGO_TARGET_DIR=/tmp/ee-rch-verify-target`
- local build-admission preflight enabled when a host-runnable `ee` binary is
  available from `--build-admission-ee-bin`, `RCH_VERIFY_EE_BIN`, `EE_BIN`,
  `EE_BINARY`, or the current target directory. Automatic target-directory
  discovery skips candidates whose `--version` output is empty.

bd-3opmx and bd-3tmeg unblock, proven 2026-06-05: release RCH binaries still
fail the Mac dependency topology in this checkout, but the current-source
sidecar `/Users/jemanuel/.local/bin/rch-manifestfix-20260605-5` gets past
`RCH-E327`, rewrites manifest-declared path dependencies to their synced remote
roots, and handles dependency roots discovered only from local manifests. Use
the widened local topology above. The worker preflight remains fixed to
`/dp -> /data/projects`, so `/data` is only the local alias root used by the
dependency planner.

## Cargo-Free Panic Helper Radar

Before spending an RCH or CI Clippy slot on touched Rust tests or benches, run
the panic-helper radar over the files you changed:

```bash
scripts/panic-helper-radar.sh --json tests/new_contract.rs benches/new_bench.rs
scripts/panic-helper-radar.sh --json
```

The no-argument form scans only dirty, staged, or untracked `*.rs` files. It
does not scan the whole repository unless `--all` is passed, and it never runs
Cargo or rewrites code. The report schema is `ee.panic_helper_radar.v1`; a
failure points to unallowed `.expect()`, `.expect_err()`, `.unwrap()`, or
`.unwrap_err()` calls with the corresponding Clippy lint family
(`expect_used` or `unwrap_used`). Explicit file-level or nearby
`#[allow(clippy::expect_used)]` / `#[allow(clippy::unwrap_used)]` annotations
are treated as intentional posture, not as scanner failures.

The schema is pinned in `docs/schemas/ee.panic_helper_radar.v1.json`, with
compact fixtures under `tests/fixtures/panic_helper_radar/`. Run the no-Cargo
golden harness when changing the scanner contract:

```bash
scripts/panic_helper_radar_golden.sh
```

The central `scripts/verify.sh` runner also executes this harness as the
`Panic Helper Radar Contract` stage. That stage validates only the scanner
schema and compact fixtures; it does not scan the entire Rust tree and does not
run Cargo.

The same runner executes `scripts/e2e_overhaul/swarm_slo_replay.sh` early as
the `Swarm SLO Replay Contract` stage. That stage replays the compact
`tests/fixtures/swarm_slo_replay/` trace/golden pair, checks deterministic
event-index tie ordering, validates the summary schema and mutation flags, and
does not run Cargo. Treat it as a shell replay-contract check, not as evidence
that Rust-backed swarm replay code compiled or passed.

Use this as a cheap hygiene check before remote verification. It is not a
replacement for `scripts/rch_verify.sh -- cargo clippy --all-targets -- -D
warnings`, and it must not be used to claim a Rust proof when code changed.

Proof:

```text
cargo test -p rch manifest_rewrite_rules --quiet -- --nocapture
[RCH] remote vmi1264463 (513.9s)

RCH_BUILD_TIMEOUT_SEC=1200 ... rch-manifestfix-20260605-5 exec -- \
  env TMPDIR=/tmp CARGO_TARGET_DIR=/tmp/ee-rch-verify-target cargo check --lib --quiet
[RCH] remote vmi1264463 (839.6s)
```

The 1200s build timeout is intentional for large `cargo check` proofs. The
default 300s build timeout reached remote Cargo and then failed closed with
`RCH-E104` (`SSH command timed out after 300s`), which is a runtime budget
blocker, not permission to use local Cargo fallback.

The JSON proof schema is `ee.rch.verify.v1` and includes the command kind,
remote-required flag, planned or actual RCH invocation, worker id when observed,
remote project root, remote target dir, exit code, elapsed time, command hash,
first Rust compiler error location, output tail, source attribution, dirty-state
hashes, local build-admission summary, degradation codes, and the
source-state/worker-state degraded-code partitions described below. It also
includes `selector_admission_probe`, a read-only
`ee.rch.selector_admission_probe.v1` block that summarizes whether worker
selection reached a concrete remote worker before Cargo could start.

`selector_admission_probe` is intended for Beads comments, proof capsules, and
future replay ledgers. Its standalone contract fixture is
`docs/schemas/ee.rch.selector_admission_probe.v1.json`. Stable fields include:

- `required_runtime`: currently `Rust` for Cargo-shaped verifier commands.
- `workers_reported` and `daemon_workers_reported`: bounded worker IDs already
  reported by RCH metadata/status probes.
- `selected_worker`: the worker observed in the transcript, or `null` when no
  worker was selected.
- `selection_failure_reason`: one of the coarse pre-Cargo reasons such as
  `no_workers_with_rust_installed`, `topology_blocked`,
  `capacity_or_timeout`, `all_workers_preflight_failed`,
  `command_not_offloaded`, `remote_marker_missing`, or
  `no_worker_selected`.
- `workers_vs_selection_contradiction`: true when workers were reported but no
  worker was selected for a Rust command.
- `path_normalization_warning`: a redacted transcript line when RCH reports a
  project-root, alias-root, or path-normalization warning.
- `remote_required` and `local_fallback_refused`: policy posture flags. A true
  `local_fallback_refused` means no local Cargo fallback was accepted.

Rust consumers that ingest `ee.rch.verify.v1` through
`verification_evidence_record_from_rch_verify` expose this block as
`selectorAdmission` on `ee.verification.evidence.v1`, so support bundles and
closeout summaries can classify selector/admission failures without scraping
`stderr_tail` or `summary_markdown`.

## Local Build-Admission Preflight

Before launching RCH for a real run, the wrapper attempts a side-effect-free
`ee diag build-admission --json` check. The preflight checks the local
workspace, `CARGO_TARGET_DIR`, `TMPDIR`, and any explicit
`--artifact-destination` paths. This catches the Mac failure mode where Cargo
target and temp scratch are correctly on `/Volumes/USBNVME16TB/...`, but the
repo checkout on the internal APFS volume is still too full for reliable
verification or artifact handling.

The preflight never runs local Cargo and never deletes, moves, truncates, or
repairs files. If admission is denied, the wrapper refuses before invoking RCH
and emits `status=build_admission_refused` with
`rch_verify_build_admission_denied`. The proof also includes
`build_admission.status`, `build_admission.admitted`,
`build_admission.checks`, and the degraded codes reported by
`ee diag build-admission`.

If no usable `ee` binary is available, the wrapper records
`build_admission.status=unavailable` and continues with
`rch_verify_build_admission_unavailable`; do not treat that as a clean
admission pass. A target-directory `ee` file must be executable on this host
and produce non-empty `--version` output before automatic discovery trusts it.
Use `--build-admission-ee-bin <path>` when you have a known current binary. Use
`--skip-build-admission` only when you intentionally need to bypass this local
admission guard; the proof records `rch_verify_build_admission_skipped`.

## Source Attribution Modes

The wrapper has four source-attribution modes. Pick the weakest mode that is
honest for the claim you need to make.

| Mode | Command shape | Runs RCH? | Use when | Proof status to expect |
|---|---|---:|---|---|
| Local checkout observed; remote source unknown | `scripts/rch_verify.sh -- cargo test ...` | yes | You need a remote signal but have not exported source through the wrapper. Dirty paths are fingerprinted locally, not proven to have run remotely. | `verification_attribution=local_checkout_observed_remote_source_unknown`; dirty runs include `remote_source_materialized=false` and `rch_verify_dirty_source_not_materialized`. |
| Strict clean checkout | `scripts/rch_verify.sh --require-clean-tree -- cargo test ...` | only if clean | You need proof that no tracked, Beads, scratch, or unsafe untracked paths influenced the run. | Clean: `strict_clean_tree`; dirty: `source_state_refused` before RCH. |
| Committed tree export | `scripts/rch_verify.sh --committed-tree --treeish HEAD -- cargo test ...` | yes for safe trees | You need to verify committed source while the shared checkout is dirty. | Safe no-path-dependency trees run from a generated export with `verification_attribution=committed_tree`; unsupported refs/path dependencies refuse before RCH. |

Strict clean mode is the closeout mode for "this exact checkout is clean and
remote-verified." Gitignored files (matched by `.gitignore` patterns and not
under `git status --ignored=no`) are the explicit allowlist: local-machine
artifacts such as `._mac_finder_metadata`, editor scratch files, and other
patterns the repo's `.gitignore` already excludes do not count against the
strict-clean check. Use `.gitignore` to declare "these local-only patterns are
allowed to coexist with a strict-clean proof" rather than asking the wrapper for
an ad-hoc allowlist flag. Committed-tree mode resolves the requested treeish, records
`git_tree`, `resolved_commit`, `source_manifest_hash`,
`source_manifest_file_count`, and `source_manifest_byte_count`, then
materializes that committed tree into a generated source export when the tree is
safe to represent without path dependencies. If the ref is unresolved or
`Cargo.toml` contains path dependencies, the proof refuses before RCH with
`rch_verify_committed_tree_unsupported` and, for path dependencies,
`rch_verify_committed_tree_path_deps_unsupported`; do not reinterpret that as a
successful verification of the live checkout.

Copy-paste examples:

```bash
# Default live-checkout proof. Honest if dirty source is intentionally in scope.
scripts/rch_verify.sh --bead-id bd-XXXX --summary -- \
  cargo test --lib focused_case_name -- --nocapture

# Strict proof. Refuses before RCH if .beads, source, or scratch paths are dirty.
scripts/rch_verify.sh --bead-id bd-XXXX --summary --require-clean-tree -- \
  cargo test --test rch_verify_contract strict_clean_tree -- --nocapture

# Committed-tree proof. Safe no-path-dependency trees run from a generated
# source export; path-dependency trees refuse before RCH with an unsupported code.
scripts/rch_verify.sh --bead-id bd-XXXX --summary --committed-tree --treeish HEAD -- \
  cargo test --test rch_verify_contract committed_tree_ -- --nocapture
```

These modes never authorize `git worktree`, `git stash`, `git reset`,
`git checkout`, destructive cleanup, or local Cargo fallback. If the proof is
blocked by dirty state, record that state and coordinate; do not "clean it up"
by deleting files.

For bead closeout evidence, pass `--ledger <path>` to append one derived
JSONL row with schema `ee.rch.verify.ledger.v1`, and pass `--summary` to include
a Markdown summary in the proof. Use `--no-write` with `--ledger` when doing a
read-only investigation that should render the proof without writing the
derived ledger.

Ledger rows include `verifier_id`, optional `bead_id`, `command`,
`command_hash`, `started_at`, `completed_at`, `elapsed_ms`, `worker_id`,
`remote_project_root`, `remote_target_dir`, `rch_location`, `exit_code`,
`status`, `first_error_file`, `first_error_line`, `stdout_tail`, `stderr_tail`,
`transcript_path`, `source_state_degraded_codes`, `worker_state_degraded_codes`,
and known-blocker fields when a circuit-breaker refusal applies. Retained tails
redact private `/Users/<name>` prefixes and obvious `token=...` / `secret=...` /
`password=...` fragments while preserving remote `/data/projects/...` and local
`/Volumes/...` evidence.

`status` is one of `dry_run`, `remote_pass`, `pass_without_remote_marker`,
`remote_failure`, `rch_environment_failure`, `capacity_or_timeout`,
`build_admission_refused`, or `refused`, `source_state_refused`, or
`committed_tree_unsupported`. The planned known-blocker circuit-breaker status
is `known_blocker_refused`; it means RCH was not launched because an active,
matching environmental blocker already exists. Use
`rch_environment_failure` for topology/local-fallback blockers and
`capacity_or_timeout` for worker capacity, timeout, or all-workers-offline
signals; those are not code failures. Use `build_admission_refused` when local
workspace, target, temp, or artifact destination admission failed before RCH.
Use `source_state_refused` for dirty checkout ambiguity and
`committed_tree_unsupported` for an intentionally non-executing committed-tree
proof. Use `known_blocker_refused` only for the fail-fast non-proof path
described below.

## Durable RCH Verify Ledger CLI

`scripts/rch_verify.sh` produces the proof. `ee verify rch ...` stores and
queries that proof so later agents do not spend another RCH slot rediscovering
the same topology, capacity, or local-fallback blocker.

The ledger commands are storage surfaces only. They never run Cargo, never
invoke RCH, never mutate Beads, and never reinterpret a refused local fallback
as a successful proof.

Typical agent workflow:

```bash
# 1. Generate or capture one ee.rch.verify.v1 proof.
scripts/rch_verify.sh --bead-id bd-XXXX --summary --no-write -- \
  cargo test --lib focused_case_name -- --nocapture > proof.json

# 2. Store the proof in the local verification ledger.
ee verify rch ingest --workspace . --from-json proof.json --json

# 3. Before retrying the same bead, inspect active blockers first.
ee verify rch blockers --workspace . --bead-id bd-XXXX --json

# 4. Inspect historical runs when deciding whether source, command, or
#    topology changed enough to justify another remote attempt.
ee verify rch runs --workspace . --bead-id bd-XXXX --json
```

Use `--from-json -` when piping a proof from another harness. Query commands are
read-only; an uninitialized workspace ledger returns an empty report instead of
creating a database or workspace row. Active blockers are bounded by
`retry_after`; expired blockers remain historical run evidence but should not
stop a new remote attempt when source or topology has changed.

`ee status --json` and `ee doctor --json` expose `data.verificationLedger`, a
bounded summary of active blocker counts, oldest/newest `retry_after`, whether
local fallback was refused, and up to eight blocker references. `ee swarm
work-packet --json` folds active ledger blockers into
`rchProofPosture.knownBlockers`, so packet consumers can prefer static checks
or waiting instead of launching a duplicate RCH attempt.

When a blocker is active, cite these fields in Beads and Agent Mail:

```text
RCH verifier ledger blocker for <bead>:
- command_hash: <run.commandHash>
- status: <run.status>
- blocker_fingerprint: <run.blockerFingerprint>
- degraded_codes: <run.degradedCodes>
- remediation_bead: <run.remediationBead>
- retry_after: <run.retryAfter>
- verification_attribution: <run.verificationAttribution>
- note: Query only; no Cargo or RCH command was launched by `ee verify rch`.
```

For a successful remote proof, keep the closeout short and explicit:

```text
RCH proof stored for <bead>:
- command_hash: <run.commandHash>
- status: <run.status, normally passed>
- worker_id: <run.workerId>
- git_head: <run.gitHead>
- git_tree: <run.gitTree>
- source_state_hash: <run.sourceStateHash>
- verification_attribution: <run.verificationAttribution>
- note: Proof was ingested from `ee.rch.verify.v1`; consult `ee verify rch runs --bead-id <bead> --json` for the full durable row.
```

For topology or capacity blockers, do not say the code was verified:

```text
RCH proof attempt blocked for <bead>:
- command_hash: <run.commandHash>
- status: blocked
- degraded_codes: <run.degradedCodes>
- blocker_fingerprint: <run.blockerFingerprint>
- remediation_bead: <run.remediationBead>
- retry_after: <run.retryAfter>
- exact_blocker: <bounded exact blocker string from proof/error_codes>
- note: Remote Cargo did not reach the test runner, and no local Cargo fallback was run.
```

Concrete examples:

```text
RCH proof stored for bd-123:
- command_hash: sha256:<hash>
- status: passed
- worker_id: trj
- git_head: <commit>
- git_tree: <tree>
- source_state_hash: sha256:<hash>
- verification_attribution: local_checkout_observed_remote_source_unknown
- note: Proof was ingested from `ee.rch.verify.v1`; consult `ee verify rch runs --bead-id bd-123 --json` for the full durable row.
```

```text
RCH proof attempt blocked for bd-123:
- command_hash: sha256:<hash>
- status: rch_environment_failure
- degraded_codes: rch_verify_remote_command_failed, rch_verify_topology_blocked, rch_verify_local_fallback_refused, rch_verify_remote_marker_missing
- blocker_fingerprint: sha256:<hash>
- remediation_bead: bd-17c65.10.17.1.2
- retry_after: <timestamp>
- exact_blocker: RCH-E327 / Path dependency topology policy failed; remote required; refusing local fallback
- note: Remote Cargo did not reach the test runner, and no local Cargo fallback was run.
```

```text
RCH proof attempt blocked for bd-123:
- command_hash: sha256:<hash>
- status: capacity_or_timeout
- degraded_codes: rch_verify_remote_command_failed, rch_verify_capacity_or_timeout
- blocker_fingerprint: sha256:<hash>
- remediation_bead: <bead or none>
- retry_after: <timestamp>
- exact_blocker: all workers failed preflight checks / no healthy worker capacity
- note: Remote Cargo did not reach the test runner, and no local Cargo fallback was run.
```

## Degraded-Code Taxonomy

`degraded_codes` remains the complete ordered list of verifier degradations.
Consumers that need to route action should use the two narrower lists:

- `source_state_degraded_codes`: source attribution blockers, such as dirty
  tracked files, Beads metadata churn, scratch artifacts, unsafe untracked paths,
  or committed-tree manifest limitations. These mean the source proof is weaker
  than the closeout claim needs.
- `worker_state_degraded_codes`: RCH worker, topology, capacity, remote-checkout,
  or local-fallback blockers. These mean the same source may verify after the
  worker fleet, root mapping, or queue state is fixed.
  `rch_verify_cargo_workspace_inheritance_blocked` is the worker-topology
  classifier for Cargo errors where a path dependency inherits
  `workspace.package.*` fields from the wrong or incomplete workspace root.
  When the Cargo transcript includes the dependency name, parsed manifest path,
  inherited package field, and missing `workspace.package.*` key, the proof also
  includes `cargo_workspace_inheritance` with structured `dependency`,
  `manifest_path`, `inherited_field`, `workspace_field`, and
  `missing_workspace_field` fields for routing the topology or dependency
  workspace sync follow-up.
  `rch_verify_cargo_path_dependency_version_blocked` classifies Cargo
  path-dependency version-resolution failures where a remote
  `/data/projects/<dependency>` checkout is stale relative to the verifying
  project. When the Cargo transcript includes the dependency name, requested
  version requirement, candidate versions, and searched path, the proof also
  includes `cargo_path_dependency_version` with structured `crate`, `required`,
  `candidate_versions`, and `location_searched` fields for routing the worker
  refresh or dependency publication follow-up.
  `rch_verify_client_daemon_version_skew` is emitted before remote Cargo when
  the selected `rch` client and the live daemon behind `rch status --json`
  report different major/minor compatibility prefixes. The proof includes
  `rch_runtime` with the selected client path, client version, daemon version,
  compatibility prefixes, and daemon socket path. This fails closed by default
  so a repo-local verifier client cannot silently talk to an older launchd
  daemon; set `RCH_VERIFY_FAIL_FAST_VERSION_SKEW=0` only for an explicitly
  documented diagnostic run.
  When an RCH transcript includes a line such as
  `Prepared dependency sync manifest for N roots`, the proof also includes
  `sync_closure.source = "rch_transcript"`,
  `sync_closure.last_root_count`, and `sync_closure.root_counts[]` entries
  with the parsed root count and redacted source line. This is evidence for
  dependency-closure topology debugging; it is not by itself a degraded code.
- `build_admission.status`: local admission result before RCH. A denied result
  means no remote verifier ran; unavailable or skipped results mean proof
  quality is weaker than an admitted run.

The two lists are intentionally disjoint. Generic command failure codes such as
`rch_verify_remote_command_failed` stay only in `degraded_codes`; they provide
overall status context but are not enough by themselves to tell an agent whether
to fix source state or wait for RCH capacity.

These are script proof fields, not new `ee degraded[]` emissions. Do not add
failure-mode catalog fixtures under `tests/fixtures/failure_modes/` only because
a code appears in `source_state_degraded_codes` or
`worker_state_degraded_codes`. Fixture registration is required only if the code
is emitted through an `ee` command response envelope. For the RCH wrapper, the
owned contract is the `ee.rch.verify.v1` proof plus
`tests/rch_verify_contract.rs`.

## Known-Blocker Circuit-Breaker Contract

Known-blocker evidence is an admission-control negative cache for repeated RCH
environment failures. It is not an implementation proof. When a prior remote
proof has already failed for the same worker/topology condition, a later matching
invocation may refuse before launching RCH and emit `status=known_blocker_refused`
with `verification_attribution=not_run_known_blocker`.

The repo wrapper keeps this cache in verifier evidence state, defaulting to
`.ee/derived/rch/known_blockers.jsonl` under the final `--project-root` for real
RCH runs. Tests and harnesses can override it with `--known-blocker-store <path>`
or `RCH_VERIFY_KNOWN_BLOCKER_STORE=<path>`; both forms count as explicit stores
for fake-output contract runs. Use `--skip-known-blocker` to disable the cache
for one run, and `--known-blocker-override` to force a fresh remote attempt while
preserving the matched blocker fingerprint in the proof. `--no-write` suppresses
cache updates just as it suppresses ledger writes.

The proof and any retained ledger row must include these stable fields when the
known-blocker path applies:

- `known_blocker.blocker_fingerprint`: stable hash of the normalized blocker
  inputs listed below.
- `known_blocker.blocker_kind`: coarse blocker family, such as
  `cargo_workspace_inheritance`, `cargo_path_dependency_version`,
  `client_daemon_version_skew`, `remote_checkout_incomplete`, `worker_disk_full`,
  or `capacity_or_timeout`.
- `known_blocker.degraded_codes`: the normalized degraded-code family that made
  the earlier run an environmental blocker.
- `known_blocker.source_state_hash`: the dirty-state hash, committed-tree
  manifest hash, or other source identity used to scope the blocker.
- `known_blocker.source_manifest_hash`: optional committed-tree or workspace
  manifest hash when the verifier can produce one.
- `known_blocker.command_kind` and `known_blocker.command_hash`: command family
  and normalized command fingerprint for the refused verification.
- `known_blocker.normalized_argv_hash`: hash of normalized argv when it differs
  from `command_hash`.
- `known_blocker.dependency` and `known_blocker.manifest_path`: optional
  dependency or manifest identity, redacted to stable path components, that made
  the topology blocker specific.
- `known_blocker.first_seen`, `known_blocker.last_seen`, `known_blocker.expires_at`,
  and `known_blocker.retry_after`: RFC 3339 timestamps that bound the refusal.
- `known_blocker.remediation_bead`: Bead ID that owns the root remediation, for
  example `bd-17c65.10.17.1.3` for path-dependency workspace classification.
- `known_blocker.override_used`: `true` only when an explicit override launched
  a new remote run despite the active blocker.

Fingerprint inputs must be narrow enough that fixed or meaningfully different
work can still verify. At minimum include blocker kind, normalized degraded code
or stderr family, command kind, verifier policy/source mode, source-state
identity, environment fingerprint class, and dependency or manifest identity
when present. Do not include volatile timestamps, worker-selected-at time, raw
absolute home paths, raw stderr tails, or full environment dumps.

Retention is bounded. A known-blocker store may retain only the compact fields
above, capped by count and TTL. Expired entries must not block a run. Changed
source-state identity, changed command kind, changed dependency/manifest identity,
or explicit override must allow a new remote attempt. Override output must carry
both `override_used=true` and the matched `blocker_fingerprint`; it still must
run through RCH and must never fall back to local Cargo.

The wrapper default cap is `RCH_VERIFY_KNOWN_BLOCKER_MAX_ENTRIES=128` and the
default TTL is `RCH_VERIFY_KNOWN_BLOCKER_TTL_SECONDS=21600`. A matching active
blocker must have the same command hash, command kind, source-state hash,
source mode, requested/configured worker set, and RCH runtime compatibility
fingerprint. This intentionally lets different commands, changed source state,
worker-scope changes, and client/daemon version changes try remote verification
again.

Known-blocker output is deliberately weaker than a failed remote run. It proves
only that the verifier refused to consume another RCH slot because an active,
matching environmental blocker was already recorded. Use the remediation bead
and `retry_after` fields to decide whether to wait, dry-run, or coordinate with
the topology owner.

Decision table:

| Situation | Use | Beads/Agent Mail wording |
| --- | --- | --- |
| Real remote retry | Normal `scripts/rch_verify.sh --summary -- cargo ...` after source state, command shape, worker scope, RCH runtime, or dependency topology changed. | "Remote verifier ran. Interpret `status`, `verification_attribution`, `degraded_codes`, and first error normally." |
| Dry-run proof | `scripts/rch_verify.sh --dry-run --summary -- cargo ...` when queue pressure, topology, or policy makes a real run wasteful but you still need command shape and command hash. | "Dry-run only: command shape and hash were produced; no remote Cargo ran." |
| Strict clean refusal | `--require-clean-tree` when the closeout claim requires a clean checkout and the wrapper reports `source_state_refused`. | "Clean proof is blocked by dirty state; this is not a remote run." |
| Committed-tree unsupported | `--committed-tree --treeish <ref>` when a committed-source proof is requested but path dependencies or unsafe materialization prevent export. | "Committed source was identified, but no safe remote materialization ran." |
| Build-admission refusal | Default build admission when local target/temp/artifact posture is unsafe and the wrapper reports `build_admission_refused`. | "Remote Cargo did not run because local build admission refused the workspace posture." |
| Known-blocker refusal | Default known-blocker check when an active matching blocker exists and the wrapper reports `known_blocker_refused`. | "Remote Cargo did not run because a matching RCH environmental blocker is active; cite fingerprint, remediation bead, and retry_after." |
| Explicit override | `--known-blocker-override` only after topology remediation, changed source state, expired or suspect TTL, changed command/dependency scope, or direct owner instruction. | "Override launched a fresh RCH attempt despite blocker `<fingerprint>`; do not claim success unless the new run passed remotely." |

## Beads and Agent Mail Templates

For Beads comments, paste the summary plus the fields that make attribution
auditable:

```text
RCH proof for <bead>:
- command_hash: <proof.command_hash>
- status: <proof.status>
- verification_attribution: <proof.verification_attribution>
- git_head: <proof.git_head>
- git_tree: <proof.git_tree>
- dirty_status_hash: <proof.dirty_status_hash>
- source_manifest_hash: <proof.source_manifest_hash or none>
- worker_id: <proof.worker_id or none>
- exit_code: <proof.exit_code>
- degraded_codes: <proof.degraded_codes or none>
- source_state_degraded_codes: <proof.source_state_degraded_codes or none>
- worker_state_degraded_codes: <proof.worker_state_degraded_codes or none>
- known_blocker: <proof.known_blocker.blocker_fingerprint or none>
- remediation_bead: <proof.known_blocker.remediation_bead or none>
- retry_after: <proof.known_blocker.retry_after or none>
- build_admission: <proof.build_admission.status>/<proof.build_admission.admitted>
- first_error: <proof.first_error_file>:<proof.first_error_line or none>
```

For a known-blocker refusal, use a shorter closeout that does not call the result
a proof:

```text
RCH known-blocker refusal for <bead>:
- command_hash: <proof.command_hash>
- status: known_blocker_refused
- verification_attribution: not_run_known_blocker
- blocker_fingerprint: <proof.known_blocker.blocker_fingerprint>
- blocker_kind: <proof.known_blocker.blocker_kind>
- degraded_codes: <proof.known_blocker.degraded_codes>
- remediation_bead: <proof.known_blocker.remediation_bead>
- retry_after: <proof.known_blocker.retry_after>
- override_used: <proof.known_blocker.override_used>
- note: Remote Cargo did not run, so this is not compile proof.
```

Agent Mail handoff phrasing should distinguish proof quality:

- `remote_pass` + `strict_clean_tree`: "Committed implementation verified from
  a clean checkout."
- `remote_pass` + `local_checkout_observed_remote_source_unknown`: "Remote run
  passed, but the wrapper did not materialize local source remotely; inspect
  `dirty_paths_sample` and `remote_source_materialized` before closing."
- `source_state_refused`: "Code may be implemented, but clean proof is blocked
  by dirty source state."
- `committed_tree_unsupported`: "Committed source manifest was computed, but no
  safe remote materialization exists yet; this is not a remote Cargo pass."
- `build_admission_refused`: "Remote Cargo did not run because local
  build-admission denied the workspace/target/temp/artifact path posture."
- `known_blocker_refused`: "Remote Cargo did not run because a matching active
  RCH environmental blocker is already recorded; this is not compile proof."

When Agent Mail is healthy, send the same known-blocker summary to the active
bead owner or topology owner with the bead ID as `thread_id`. When mail reads or
delivery are degraded, record the Beads comment only and state that Agent Mail
handoff was skipped because the coordination channel was unhealthy. Do not paste
raw stderr tails, raw environment dumps, secrets, or shell-substitution text into
either channel.

Dirty `.beads/issues.jsonl` is metadata churn, but it still invalidates strict
clean proof because it changes the shared checkout. If the only dirty path is a
Beads export that you own, run `br doctor --json`, then `br sync --flush-only`,
commit the tracker export, and rerun strict mode. If the Beads file is reserved
or contains another agent's updates, coordinate through Agent Mail and use
live-checkout or committed-tree manifest wording rather than claiming a clean
proof.

## Compile-Blocker Routing

Use `scripts/rch_compile_blocker_router.py` when a remote verifier reaches
Cargo and fails on compiler diagnostics. It is read-only: it consumes a
transcript plus optional Agent Mail reservation JSON and emits
`ee.rch.compile_blocker_route.v1` JSON with an Agent Mail-ready Markdown
summary.

Examples:

```bash
scripts/rch_compile_blocker_router.py failed-rch.txt \
  --command "cargo test --lib why_toon_matches_json_contract -- --nocapture" \
  --bead-id bd-123 \
  --agent-name SilentLark \
  --reservations reservations.json \
  --json
```

Routing decisions:

- `self_fix_allowed`: the first diagnostic file is reserved by the current
  agent.
- `reserved_by_other_agent`: an active exact or glob reservation owns the first
  diagnostic file.
- `no_owner_found`: the first diagnostic is local to this repo, but no active
  reservation matched.
- `upstream_dependency_failure`: the first diagnostic is under a sibling
  `/data/projects/...` dependency rather than this repo.
- `environment_failure`: the transcript is an RCH topology/local-fallback
  blocker or lacks an actionable compiler diagnostic.

The same helper also has a snapshot-only preflight mode for crowded shared
checkouts. It does not run Cargo or inspect live Agent Mail itself; callers pass
dirty-path, reservation, and recent verifier-evidence snapshots they already
collected:

```bash
scripts/rch_compile_blocker_router.py --preflight \
  --dirty-paths dirty-paths.json \
  --reservations reservations.json \
  --verifier-evidence recent-rch-proofs.json \
  --command "cargo test --lib focused_case -- --nocapture" \
  --bead-id bd-123 \
  --agent-name SilentLark \
  --json
```

Preflight emits `ee.swarm_compile_blockers.v1` with `safeToLaunchRch`,
`compileBlockers[]`, `recommendedAlternativeWork[]`, and an optional
`mailTemplate`. A dirty compile-critical path reserved by another active agent,
or a recent RCH first-error file matching a currently dirty path, returns
`safeToLaunchRch=false`. Dirty compile-critical paths without ownership or
recent first-error evidence return `safeToLaunchRch="unknown"` so agents can
prefer static work or coordinate before burning a remote slot.

## Control-Plane Fixtures and E2E

`bd-1h8ji.6` pins the verification control plane with a dedicated fixture
catalog under `tests/fixtures/rch_verify_control_plane/`. Each fixture has
schema `ee.rch.verify_control_plane_fixture.v1`, one `expected_status_class`,
and an exact `summary_markdown` golden. The catalog covers remote Cargo success,
focused test success, Rust compile failure, RCH-E327 topology refusal, worker
capacity waits, daemon timeouts, and local-Cargo hook bypass detection.

Run the focused contract through RCH:

```bash
scripts/rch_verify.sh --bead-id bd-1h8ji.6 --summary -- \
  cargo test --test rch_verify_control_plane -- --nocapture
```

The CI-safe e2e driver is `scripts/e2e_overhaul/rch_verify_control_plane.sh`.
Default mode uses deterministic fake RCH transcripts so it can verify JSON proof
generation, explicit `rch exec` invocation shape, phase logs, and final summary
rows without starting a heavy build:

```bash
scripts/e2e_overhaul/rch_verify_control_plane.sh
```

To run the optional heavyweight lane, opt in explicitly:

```bash
RCH_VERIFY_CONTROL_PLANE_LONG_BENCH=1 \
  scripts/e2e_overhaul/rch_verify_control_plane.sh
```

The e2e emits one JSONL event per phase with schema `ee.test_event.v1`,
`surface=rch_verification_control_plane`, `phase`, `status`, `elapsed_ms`,
`command_hash`, `worker_id`, and `degraded_codes`. The cleanup phase does not
delete files; it records `status=no_delete_by_policy` and leaves the temporary
proof directory in `/tmp`.

## Runbook Command Example Lint

`scripts/check-rch-doc-examples.py` keeps this page and
`docs/rch_runbook.md` from drifting back to copy-pasteable local Cargo
examples. It scans fenced shell blocks, allows commands wrapped through
`scripts/rch_verify.sh` or explicit `rch exec`, and rejects direct
`cargo build/check/test/bench/clippy` examples that would bypass RCH.
RCH-specific fenced blocks in AGENTS.md and README.md are scanned too.

```bash
python3 scripts/check-rch-doc-examples.py --json
```

The copy-paste smoke harness extracts the first documented dry-run verifier
example and executes that exact docs text. It expects an `ee.rch.verify.v1`
proof with `status=dry_run` and an explicit `rch exec` invocation, so the
default lane never starts local Cargo or a remote build.

```bash
scripts/e2e_overhaul/rch_runbook_docs_smoke.sh
```

Both surfaces emit deterministic machine output. The e2e emits
`ee.test_event.v1` lines with `surface=rch_doc_examples`, `source_file`,
`fenced_block_index`, `normalized_command`, `command_hash`, `phase`,
`status`, `elapsed_ms`, `degraded_codes`, and `first_failure_diagnosis`.
