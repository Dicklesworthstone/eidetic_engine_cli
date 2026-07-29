# RCH Verification Wrapper

Use `scripts/rch_verify.sh` for focused Rust verification in this repository.
It always builds an explicit `rch exec -- cargo ...` invocation and emits a JSON
proof that can be copied into a Beads comment.

At startup, the wrapper re-execs once from an in-memory copy of its own source.
That keeps long-running RCH proofs stable even if another coordinated agent edits
`scripts/rch_verify.sh` in the checkout while the proof is waiting for remote
Cargo to finish.

Examples:

```bash
scripts/rch_verify.sh --pinned-franken-stack --treeish HEAD --dry-run -- \
  cargo test --locked --lib output::streaming -- --nocapture
scripts/rch_verify.sh --pinned-franken-stack --treeish HEAD -- \
  cargo clippy --locked --all-targets -- -D warnings
scripts/rch_verify.sh -- cargo fmt --check
scripts/rch_verify.sh --pinned-franken-stack --treeish HEAD \
  --bead-id bd-123 --ledger .ee/derived/rch/runs.jsonl --summary -- \
  cargo test --locked --test mesh_off_no_network -- --nocapture
scripts/rch_verify.sh --pinned-franken-stack --treeish HEAD \
  --skip-build-admission -- cargo test --locked --lib focused_case -- --nocapture
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
- `RCH_ENV_ALLOWLIST=CARGO_TARGET_DIR,TMPDIR,...` so RCH can rewrite worker
  target/tmp paths without hiding `cargo` behind a leading `env` argv
- RCH binary `/Users/jemanuel/.local/bin/rch-manifestfix-20260605-5` when present, then `/Users/jemanuel/.local/bin/rch-33720a8`, then `/Volumes/USBNVME16TB/temp_agent_space/rch-macos-target/debug/rch`, then any host-runnable source-built fallback, then `rch`
- `RCH_CANONICAL_PROJECT_ROOT=/Users/jemanuel`
- `RCH_ALIAS_PROJECT_ROOT=/data`
- `RCH_WORKER`, `RCH_WORKERS`, and `RCH_SOCKET_PATH` are verifier control-plane
  inputs when set; use `RCH_WORKER=<id>` for a singular worker override and
  `RCH_SOCKET_PATH=<path>` for an alternate local daemon socket.
- remote `TMPDIR` and `CARGO_TARGET_DIR` are worker-scoped by RCH env
  forwarding/rewrite logic
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
replacement for `scripts/rch_verify.sh --pinned-franken-stack --treeish HEAD
-- cargo clippy --locked --all-targets -- -D warnings`, and it must not be
used to claim a Rust proof when code changed.

Proof:

```text
cargo test -p rch manifest_rewrite_rules --quiet -- --nocapture
[RCH] remote vmi1264463 (513.9s)

RCH_BUILD_TIMEOUT_SEC=1200 ... rch-manifestfix-20260605-5 exec -- \
  cargo check --lib --quiet
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
  `command_not_offloaded`, `active_project_exclusion`,
  `remote_marker_missing`, or `no_worker_selected`.
- `workers_vs_selection_contradiction`: true when workers were reported but no
  worker was selected for an applicable Rust command.
- `path_normalization_warning`: a redacted transcript line when RCH reports a
  project-root, alias-root, or path-normalization warning.
- `remote_required` and `local_fallback_refused`: policy posture flags. A true
  `local_fallback_refused` means no local Cargo fallback was accepted.
- `admission_blocker`: a bounded, redacted blocker detail when selector
  admission found a first-class pre-Cargo condition. Today this is populated for
  `active_project_exclusion` with retry guidance to wait for the active build or
  coordinate with its owner. When `rch queue --json` is available at the moment
  the wrapper observes the exclusion, the blocker also carries bounded
  operator-facing fields such as `active_project_exclusion_count`,
  `active_build_id`, `active_command_preview`, `active_command_hash`, `worker_id`,
  heartbeat/progress ages, `worker_posture`, `retry_after_hint`, `next_action`,
  and `owner_escalation`. If the transcript
  names an `active_build` id, queue enrichment prefers that matching build over
  unrelated active queue entries. The raw queue snapshot is not embedded in the
  proof.

Known-blocker refusals still include the probe for schema stability, but set
`status=not_applicable`, `selection_failure_reason=null`, and
`workers_vs_selection_contradiction=false` because RCH selection was not run.

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
| Strict clean checkout | `scripts/rch_verify.sh --require-clean-tree -- cargo test ...` | only if clean and remotely attributable | You need proof that no tracked, Beads, scratch, or unsafe untracked paths influenced a project without unresolved sibling-source authority. | Clean self-contained tree: `strict_clean_tree`; dirty or remotely unverified Franken graph: `source_state_refused` before RCH. |
| Committed tree export | `scripts/rch_verify.sh --committed-tree --treeish HEAD -- cargo test ...` | yes for safe trees | You need to verify committed source while the shared checkout is dirty. | Safe no-path-dependency trees run from a generated export with `verification_attribution=committed_tree`; unsupported refs/path dependencies refuse before RCH. |
| Pinned Franken-stack bundle | `scripts/rch_verify.sh --pinned-franken-stack --treeish HEAD -- cargo test --locked ...` | only after the complete bundle is attested | The committed `ee` tree has sibling path dependencies and the proof must use the exact `franken-stack.lock` graph without changing live sibling checkouts. | Clean bundle: `verification_attribution=pinned_franken_stack`, `franken_stack.status=pinned`, and `remote_source_verified=true`; any lock, origin, revision, dirty-state, version, or materialization failure refuses before RCH. |

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
`rch_verify_committed_tree_path_deps_unsupported`. The only supported
path-dependency exception is `--pinned-franken-stack`, which requires a valid
committed `franken-stack.lock` and Cargo `--locked`; do not reinterpret any
other committed-tree refusal as successful verification of the live checkout.

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

# Strongest ee proof. Archives the committed ee tree and all seven exact sibling
# revisions into one fresh bundle without changing the live sibling checkouts.
scripts/rch_verify.sh --bead-id bd-XXXX --summary \
  --pinned-franken-stack --treeish HEAD -- \
  cargo test --locked --test rch_verify_contract franken_stack_ -- --nocapture
```

### Franken-stack source authority

`ee` resolves core dependencies through sibling paths, while
`franken-stack.lock` records the revisions known to compose with `Cargo.lock`.
A clean `eidetic_engine_cli` checkout alone is therefore not a complete source
proof. Before any Cargo RCH dispatch, the default verifier performs a read-only
inventory of all seven locked repositories:

- the canonical GitHub origin and a privacy-safe canonical-path hash;
- expected and observed 40-character revisions plus the observed tree;
- dirty state, dirty-entry count, and a content-free status hash;
- package versions for every path-dependent crate compared with `Cargo.lock`;
- the lock-file and Cargo-lock hashes; and
- a dependency `manifest_hash` folded into the proof's
  `source_bundle_hash`.

The object uses schema `ee.rch.franken_stack.v1`. A missing repository, dirty
checkout, unexpected origin, revision mismatch, package-version mismatch,
malformed lock, or unreadable input returns `status=source_state_refused` before
the RCH runtime or worker selector is touched. Stable codes include:

- `rch_verify_franken_stack_repository_missing`
- `rch_verify_franken_stack_repository_dirty`
- `rch_verify_franken_stack_origin_mismatch`
- `rch_verify_franken_stack_revision_mismatch`
- `rch_verify_franken_stack_version_mismatch`
- `rch_verify_franken_stack_lock_invalid`
- `rch_verify_franken_stack_cargo_lock_invalid`

A fully matching live graph is still
`franken_stack.status=clean_remote_unverified`: local Git state cannot prove
which sibling trees already exist on a worker. The proof carries
`rch_verify_franken_stack_remote_source_unverified`; use the pinned mode for
closeout-grade source attribution.

`--pinned-franken-stack` implies `--committed-tree` and refuses unless the
Cargo command contains `--locked`. The first invocation for a commit creates a
fresh content-addressed retained bundle with `eidetic_engine_cli` and its seven
siblings as peers. Later gates reuse that exact source identity only after a
full path, executable-bit, symlink-target, and file-content hash validates.
`franken_stack.bundle_cache` reports `created` or `reused`, the content hash,
and `validation=full_content_hash`; an incomplete or changed entry is never
trusted as the requested commit. This stable identity also lets RCH reuse its
remote build cache across focused tests, check, and Clippy.

Pinned bundles default to `.ee-rch-committed-tree/` beside the canonical
checkout, not below `TMPDIR`. Keeping the bundle beneath the writable project
parent gives root and non-root workers the same narrow path namespace without
syncing all of the developer's home directory. For each physical bundle root,
the wrapper derives a privacy-safe `/tmp/ee-rch-pinned-<path-hash>` alias.
RCH's topology preflight creates that missing alias on the selected worker;
commit- and path-specific aliases prevent concurrent pinned proofs from
retargeting one another. `RCH_VERIFY_COMMITTED_TREE_BASE`,
`RCH_CANONICAL_PROJECT_ROOT`, and `RCH_ALIAS_PROJECT_ROOT` remain explicit
diagnostic overrides.

Each dependency is exported by the locked commit ID from a canonical sibling
object when available, otherwise from a verifier-managed bare cache populated
from the canonical origin. The lane uses `git archive`; it never changes a live
sibling, creates a worktree, switches a branch, or runs cleanup against an
existing checkout. An unknown or mismatched cache path is refused instead of
repaired in place.

After materialization, the verifier reads package versions from the archived
trees, recomputes the complete dependency manifest, and records
`cargo_lock_hash_after` plus `cargo_lock_unchanged`. RCH starts only when every
archived revision and package version matches, the dependency manifest is
complete, and `Cargo.lock` is byte-identical. Focused, all-target, and Clippy
proofs use the same lane:

```bash
scripts/rch_verify.sh --bead-id bd-XXXX --summary \
  --pinned-franken-stack --treeish HEAD -- \
  cargo test --locked --test property_response_envelope

scripts/rch_verify.sh --bead-id bd-XXXX --summary \
  --pinned-franken-stack --treeish HEAD -- \
  cargo check --locked --all-targets

scripts/rch_verify.sh --bead-id bd-XXXX --summary \
  --pinned-franken-stack --treeish HEAD -- \
  cargo clippy --locked --all-targets -- -D warnings
```

### Cargo config provenance for locked source proofs

Cargo merges configuration discovered from the invocation directory through
all parent directories, then from `CARGO_HOME`; an extensionless
`.cargo/config` wins over `.cargo/config.toml` when both exist. Config files may
also recursively include other TOML files. This means a source archive plus
`Cargo.lock` is not, by itself, proof of the dependency graph: a host-global
patch, path override, or source replacement can still change resolution.

After any committed-tree materialization, `scripts/rch_verify.sh` fingerprints
the effective Cargo config search path without invoking Cargo. The top-level
`cargo_config_provenance` object uses schema
`ee.rch.cargo_config_provenance.v1` and records:

- `status`: `not_computed`, `not_applicable`, `clean`, `observed`,
  `indeterminate`, or `blocked`;
- whether the command is source-attested and contains `--locked`;
- each discovered or included source with privacy-safe path and content hashes,
  precedence, legacy-file shadowing, parse status, and project/external origin;
- `external_resolution_sources` that contain a patch, `paths`, replacement, or
  source definition capable of changing dependency resolution;
- `blocking_sources`, the subset that caused the current proof to refuse; and
- one stable `provenance_hash` for the complete observation.

Physical paths below the project, Cargo home, or user home are rendered as
`<project>`, `<cargo_home>`, or `<home>` rather than exposing the account name.
The detector recognizes `paths`, `[patch.*]`, `[replace]`, and source
replacement/registry/directory/Git controls. A required external include that
is missing, unreadable, or invalid is indeterminate and therefore also fails
closed when the proof boundary requires certainty.

The refusal boundary is deliberately narrow. Both of these conditions must be
true:

1. the verifier is using `--require-clean-tree` or `--committed-tree`; and
2. the Cargo verifier command contains `--locked`.

An ordinary non-attested run reports an external resolution override as
`status=observed` and continues. A committed project-owned config is recorded
but does not count as an external source. When the boundary is crossed,
verification stops before any RCH client/daemon probe or remote dispatch,
returns `status=rch_environment_failure`, and emits
`rch_verify_cargo_config_provenance_blocked`. This code is verifier-environment
state, not worker state, and is not persisted as a reusable worker
known-blocker.

The repair is an isolated `CARGO_HOME` plus a project or export ancestry that
contains no resolution-altering Cargo config. Registry and Git cache
directories may remain accessible or be linked into that isolated home; the
config file itself must not be copied or linked. Remember that a checkout below
the user home may still discover `<home>/.cargo/config.toml` through Cargo's
parent-directory walk even when `CARGO_HOME` points elsewhere. In that case,
committed-tree mode can materialize the source under the verifier's temporary
export root outside the home hierarchy.

Proof ledgers retain the complete `cargo_config_provenance` and
`franken_stack` objects. Test-event rows retain both hashes and compact status
fields so later automation can distinguish a source-graph refusal, Cargo
configuration refusal, and worker outage without replaying raw output.

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
`cargo_config_provenance`, and known-blocker fields when a circuit-breaker
refusal applies. Retained tails redact private `/Users/<name>` prefixes and
obvious `token=...` / `secret=...` / `password=...` fragments while preserving
remote `/data/projects/...` and local `/Volumes/...` evidence.

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
scripts/rch_verify.sh --pinned-franken-stack --treeish HEAD \
  --bead-id bd-XXXX --summary --no-write -- \
  cargo test --locked --lib focused_case_name -- --nocapture > proof.json

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

On this fail-fast path, `selector_admission_probe.status` is `not_applicable`;
the wrapper has not run RCH selection, so no selector failure or worker-selection
contradiction should be inferred from the cached blocker.

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
  `active_project_exclusion`, or `capacity_or_timeout`.
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
- `known_blocker.active_project_exclusion`: optional bounded selector-admission
  details copied from `selector_admission_probe.admission_blocker` when the
  blocker kind is `active_project_exclusion`. This may include
  `active_project_exclusion_count`, `active_build_id`, `active_command_hash`,
  bounded `active_command_preview`, worker posture, and owner-coordination
  guidance. Volatile age fields may be present for operator context but are not
  required for matching future refusals.
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

## Pressure-Telemetry Gap Blocker Contract (bd-1n3x1.14)

This section is the conformance contract for bd-1n3x1.14: making RCH pressure
telemetry gaps and capabilities-refresh timeouts first-class proof-broker
evidence. It is a specification; fixtures land under bd-1n3x1.14.2 and the
implementation must not ship until every MUST row below has fixture and test
coverage.

The live failure mode this contract captures is **not** a selection failure and
**not** a source verdict. RCH can simultaneously report `posture=remote_ready`,
`active_build_count=0`, `queued_build_count=0`, and a healthy worker probe while
the worker carries `pressure_state=telemetry_gap`,
`pressure_reason_code=telemetry_unavailable`, `pressure_telemetry_fresh=false`,
and recent stuck-detector cancellations. A capabilities refresh can also hang
long enough to require bounded operator termination. Two recorded instances:
the 2026-06-09 trj capabilities-refresh hang (~2 minutes) and the 2026-06-10
daemon slot-accounting leak (`slots_available=0` with an empty queue;
selector `queue_timeout` after 300s; cleared by an owner daemon restart).

### Carrying surfaces

- The `ee.rch.verify.v1` proof emitted by `scripts/rch_verify.sh` is the
  **canonical carrier**. Pressure evidence rides in
  `worker_state_degraded_codes[]` plus a bounded `pressure_telemetry` detail
  object mirroring the matrix fields below.
- The proof broker and the work-packet claim gate (`sourceAuthority`) are
  **consumers**: they must surface the blocker reason without re-deriving it
  from raw `rch status` output.
- The support bundle may embed the same bounded object; it must never embed raw
  worker logs or unredacted host paths.

### Stable reason codes

- `rch_pressure_telemetry_unavailable` — primary bounded code: worker pressure
  telemetry is absent, stale, or self-contradictory while selection is
  otherwise possible.
- `capabilities_refresh_timeout` — a capabilities refresh exceeded its bounded
  budget and was terminated by the wrapper or a supervising operator. Use
  `refresh_exit_class=operator_terminated_refresh` to distinguish manual
  termination from a budget timeout (`refresh_exit_class=budget_timeout`).
- Detail reason (inside the bounded detail object, never a top-level code):
  `slot_accounting_inconsistent` — daemon/worker slot accounting contradicts
  build counts (for example `slots_available=0` while
  `active_build_count=0` and the queue is empty).
- Known-blocker family: entries persisted for this class use
  `known_blocker.blocker_kind=pressure_telemetry_gap`.

### Conformance matrix

An implementation is **not conformant** if any MUST field is absent — in
particular, output that omits the `source_verdict` or the redaction fields
must score as non-conformant even when every telemetry field is present.

| Field | Req | Allowed values / shape | Fixture must pin | Test surface |
| --- | --- | --- | --- | --- |
| `posture` | MUST | rch posture string, e.g. `remote_ready` | `remote_ready` with gap active | contract test on proof JSON |
| `worker_id` | MUST | bounded worker id or null | named worker | same |
| `active_build_count` | MUST | integer >= 0 | `0` | same |
| `queued_build_count` | MUST | integer >= 0 | `0` | same |
| `worker_probe_status` | MUST | `ok` \| `failed` \| `skipped` | `ok` | same |
| `pressure_state` | MUST | `ok` \| `telemetry_gap` \| `overloaded` | `telemetry_gap` | same |
| `pressure_reason_code` | MUST | bounded code, e.g. `telemetry_unavailable` | `telemetry_unavailable` | same |
| `pressure_policy_rule` | MUST | bounded rule id, e.g. `fail_open_telemetry_gap` | `fail_open_telemetry_gap` | same |
| `telemetry_fresh` | MUST | boolean | `false` | same |
| `telemetry_age_secs` | SHOULD | integer or null when unknown | non-null stale age | same |
| `refresh_elapsed_ms` | MUST when refresh attempted | integer | hung-refresh elapsed | refresh-timeout fixture |
| `refresh_exit_class` | MUST when refresh attempted | `completed` \| `budget_timeout` \| `operator_terminated_refresh` | `budget_timeout` and `operator_terminated_refresh` variants | refresh-timeout fixture |
| `retry_after` | MUST | RFC 3339 timestamp | bounded retry hold | contract test |
| `next_action` | MUST | bounded enum: `refresh_telemetry` \| `probe_worker` \| `coordinate_with_owner` | `coordinate_with_owner` | contract test |
| `owner_routing` | MUST | bounded owner/escalation hint; never a raw host path | owner hint present | contract test |
| `source_verdict` | MUST | `no_rust_verdict` (or equivalent constant): a telemetry gap is never a source outcome | `no_rust_verdict` | contract test + non-conformance test when absent |
| redaction status | MUST | explicit flag(s) proving no host-private paths or raw worker logs are embedded | redaction flags true | leak test mirroring `fixtures_do_not_leak_pids_paths_or_secrets` |

Slot-accounting inconsistency row (required input shape, from live
2026-06-09/10 evidence): `posture=remote_ready`, `daemon.slots_total=4`,
`daemon.slots_available=0`, worker `used_slots=4`, `total_slots=4`,
`active_build_count=0`, `queued_build_count=0`, `worker.status=healthy`,
`circuit_state=closed`, `pressure_state=telemetry_gap`,
`pressure_reason_code=telemetry_unavailable`, `pressure_telemetry_fresh=false`,
`pressure_policy_rule=fail_open_telemetry_gap`. Expected contract output:
`rch_pressure_telemetry_unavailable` with detail
`slot_accounting_inconsistent`, `source_verdict=no_rust_verdict`, and
`next_action`/`owner_routing` that say refresh, probe, or coordinate with the
RCH owner — never "cancel the active build" (there is none) and never local
Cargo. Daemon restarts are an owner action, not an agent remediation; the
contract output must route, not instruct agents to restart.

### Precedence

Pressure-telemetry blockers, active-build admission blockers (bd-1n3x1.13,
`active_project_exclusion`), and topology recurrence evidence (bd-b1e4v,
RCH-E327 / `classify_rch_verify_recurrence`) are **distinct environment-proof
families that must never overwrite each other**:

- If selection already failed (`selector_admission_probe.status=selection_failed`
  with `active_project_exclusion` or `topology_blocked`), that admission or
  topology blocker is the primary reason; a concurrent telemetry gap is
  reported alongside it, not instead of it.
- A pressure-telemetry blocker is reportable even when selection succeeds or
  posture is `remote_ready` — that asymmetry is the whole point of this
  contract.
- Each family keeps its own `blocker_fingerprint` inputs; fingerprints must not
  mix families, or recurrence detection
  (`recursClosedRemediation` keyed on closed remediation beads) produces false
  joins across unrelated causes.

### Non-goals

The implementation must not: fall back to local Cargo, mutate workers, restart
daemons, perform destructive cleanup, or create worktrees, stashes, resets, or
checkouts. Evidence is read-only; remediation is routed to owners.

## Mac Lane Doctor: USB-Detached Dual Blocker (bd-2qpgn)

On the Mac checkout, the external build drive being detached produces a
host-environment state where the verifier refuses before Cargo for **every**
command, in two different ways depending on the Cargo.toml path form:

- an absolute `/data/projects/<dep>` patch path passes the dependency
  planner's textual allowed-root check but dies in sibling rsync (`/data`
  dangles, ENOENT);
- a relative `../<dep>` path whose `projects/<dep>` entry is a symlink into
  the user dp dir canonicalizes outside the default canonical root and the
  planner refuses with `RCH-E327`.

`scripts/rch_lane_doctor.sh` is the read-only detector for this state. It
classifies the lane (`healthy` / `usb_detached_dual_blocker` /
`indeterminate`), reports every `[patch.crates-io]` sibling root with its
canonical path, and recommends the dispatch-local env override that was
empirically verified on 2026-06-12 (bd-2qpgn): broaden the topology roots so
both subtrees sit under one canonical root.

```bash
scripts/rch_lane_doctor.sh --json
eval "$(scripts/rch_lane_doctor.sh --emit-env)" && \
  TMPDIR=/private/tmp RCH_VISIBILITY=summary RCH_TEST_TIMEOUT_SEC=3600 \
  scripts/rch_verify.sh --pinned-franken-stack --treeish HEAD --summary -- \
  cargo test --locked --lib
```

`--emit-env` prints export lines only when the dual blocker is active (exit
2); a healthy lane emits nothing (exit 0). The override changes the RCH
project hash (fresh lane, cold first build — keep `RCH_TEST_TIMEOUT_SEC=3600`
on first dispatches or the remote 1800s test budget times out mid-compile
with `RCH-E104`) and applies to the dispatch environment only — Cargo.toml
keeps the bd-12ps0 sibling-relative convention, and nothing on the workers or
in the repo changes. Reattaching the drive restores the default lane and the
override becomes unnecessary.

### Topology Recurrence Evidence Bundle

When an RCH proof recurs as `RCH-E327`, collect the topology-family evidence
with the lane doctor before spending another remote slot:

```bash
scripts/rch_lane_doctor.sh --recurrence-evidence \
  --recurrence-proof tests/fixtures/verify_ledger/rch_e327_topology_recurrence.json \
  --recurrence-manifest Cargo.toml
```

The report schema is `ee.rch.topology_recurrence_evidence.v1`. It combines the
read-only worker-root canary, the `ee verify rch topology-audit` surface when
available, `scripts/check-local-cargo-tripwire.sh --probe-processes --json`,
and `br dep cycles --json`. The command never runs Cargo, never mutates workers,
and never writes state.

For blocked lanes, cite the compact fields rather than raw stderr:
`status`, `proof.classifier.unresolvedTopologyEdge`, `topologyAudit.status`,
`localCargoTripwire.status`, `beadsCycles.count`, and
`proofDiscipline.sourceBeadClosePolicy`. A valid blocked bundle uses
`sourceVerdict=no_rust_verdict`; topology evidence can route the RCH owner, but
it is not Rust source verification. If the installed `ee` is stale and reports
`topologyAudit.status=ee_surface_unavailable`, keep that in `surfaceGaps[]` and
route the topology-audit surface separately instead of silently treating it as a
passed path-closure audit.

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
scripts/rch_verify.sh --pinned-franken-stack --treeish HEAD \
  --bead-id bd-1h8ji.6 --summary -- \
  cargo test --locked --test rch_verify_control_plane -- --nocapture
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

The central `scripts/verify.sh` runner executes the same static lint as the
`RCH Doc Examples Lint` stage before Cargo-backed gates, so unwrapped
copy-pasteable Cargo examples fail while the fix is still a docs-only change.

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
