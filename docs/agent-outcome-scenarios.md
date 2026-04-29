# Agent Outcome Scenario Matrix

This matrix defines the agent-facing scenarios that make `ee` useful rather
than merely implemented. Each scenario is written as a future fixture contract:
it names the journey, the command sequence, the expected artifacts, degraded
branches, and the beads or gates that own executable coverage.

No scenario may rely on network access, hidden user-global state, a mutable
ambient CASS corpus, paid model APIs, or unredacted secrets. Fixtures must be
self-contained, local-first, deterministic, and safe to replay in temporary
workspaces.

## Matrix Rules

- Every command that emits machine data uses a versioned `schema` field.
- stdout contains only the requested machine format.
- stderr contains diagnostics, progress, and first-failure context.
- Fixture clocks, IDs, rankings, hashes, and workspace paths are deterministic.
- Secrets are redacted before storage, indexing, rendering, and artifacts.
- Degraded paths are first-class branches with stable codes and repair actions.
- Each scenario has an agent-facing success signal: what a fresh coding agent
  can do better because `ee` returned the right memory at the right time.

Artifact dossiers should follow the shape in `docs/testing-strategy.md`:
`target/ee-e2e/<scenario>/<run-id>/` with command, cwd, sanitized environment,
elapsed time, exit code, stdout/stderr artifacts, schema/golden status,
redaction status, degradation status, fixture IDs, and first-failure diagnosis.

## Scenarios

| ID | Journey | Fixture Family | Command Sequence | Expected Artifacts | Degraded / Failure Branches | Owning Beads / Gates | Agent Success Signal |
| --- | --- | --- | --- | --- | --- | --- | --- |
| `usr_pre_task_brief` | Pre-task brief and context pack | `fresh_workspace`, `manual_memory`, `release_failure`, `async_migration` | `ee init --workspace <tmp> --json`; `ee remember --workspace <tmp> --level procedural --kind rule ... --json`; `ee context "prepare release" --workspace <tmp> --max-tokens 4000 --format markdown`; `ee context "prepare release" --workspace <tmp> --json` | Markdown pack golden, JSON pack envelope, pack hash, provenance list, score explanation, redaction report, degraded array | `semantic_disabled`, `graph_snapshot_stale`, empty memory set, oversized token budget reduced deterministically | `eidetic_engine_cli-gbp2` / EE-USR-002, Gate 7 walking-skeleton golden contracts, Gate 14 executable claims when claims land | A fresh agent receives release rules, prior failure evidence, verification commands, and warnings before editing or releasing. |
| `usr_in_task_recovery` | In-task retrieval, `why`, doctor, and tripwire recovery | `stale_index`, `ci_clippy_failure`, `dangerous_cleanup`, `locked_writer` | `ee search "clippy release failure" --workspace <tmp> --explain --json`; `ee why <memory-id> --workspace <tmp> --json`; `ee doctor --json`; future `ee preflight "<action>" --json` | Search result golden, `why` score breakdown, doctor repair plan, tripwire warning artifact, stderr diagnostic log | `search_index_stale`, `lock_contention`, missing memory ID, stale graph, no matching evidence | `eidetic_engine_cli-g2jl` / EE-USR-003, Gate 4 Frankensearch, Gate 15 counterfactual lab, Gate 16 preflight | During a failure, the agent can recover by seeing the relevant past fix, why it was selected, and the safe repair command. |
| `usr_post_task_learning` | Post-task outcome, curation, procedure, and learning | `manual_memory`, `procedure_drift`, `false_alarm`, `causal_confounding` | `ee outcome --memory <id> --helpful --note ... --json`; `ee review session --cass-session <fixture> --propose --json`; `ee curate candidates --workspace <tmp> --json`; future `ee procedure validate <id> --json` | Outcome event JSON, curation candidate JSON, audit entry, confidence/utility delta, procedure validation report | harmful feedback demotion, contradicted evidence, low-confidence candidate, duplicate candidate, stale procedure | `eidetic_engine_cli-1mlo` / EE-USR-004, Gate 18 procedure distillation, Gate 21 active learning, Gate 22 causal credit | Useful advice gains evidence and bad advice is demoted or quarantined without silent memory mutation. |
| `usr_degraded_offline_trust` | Degraded/offline trust and repair plan | `offline_degraded`, `cass/v1`, `semantic_disabled`, `graph_linked_decision` | `ee status --workspace <tmp> --json`; `ee import cass --workspace <tmp> --dry-run --json`; `ee search "format release" --workspace <tmp> --json`; `ee context "debug offline" --workspace <tmp> --json` | Status envelope, import dry-run report, lexical-only search golden, repair commands, trust-class metadata | `cass_unavailable`, `external_adapter_schema_mismatch`, `semantic_disabled`, `agent_detector_unavailable`, `graph_snapshot_stale` | `eidetic_engine_cli-r8r0` / EE-USR-005, Gate 1 dependency tree, Gate 4 search contract, Gate 6 CASS robot fixture | The agent can keep working offline with explicit memory while knowing exactly which evidence sources are missing. |
| `usr_privacy_export` | Privacy, redaction, export, and backup | `secret_redaction`, `privacy_export`, `dangerous_cleanup` | `ee remember --workspace <tmp> --level episodic --kind fact "<secret fixture>" --json`; `ee context "shareable support summary" --workspace <tmp> --json`; `ee export jsonl --workspace <tmp>`; `ee backup create --workspace <tmp> --label fixture --json` | Redaction report, context golden with placeholders, JSONL export, backup manifest, secret absence assertions | `redaction_applied`, blocked secret storage, unsupported shareable export, backup verification failure | `eidetic_engine_cli-9sd5` / EE-USR-006, Gate 19 situation model where privacy affects routing, future backup/export beads | The agent can produce useful support or handoff artifacts without leaking secrets or private evidence. |
| `usr_workspace_continuity` | Multi-workspace and session continuity | `multi_workspace`, `fresh_workspace`, `manual_memory`, `cass/v1` | `ee init --workspace <tmp/a> --json`; `ee init --workspace <tmp/b> --json`; `ee remember --workspace <tmp/a> ... --json`; `ee context "same task" --workspace <tmp/a> --json`; `ee context "same task" --workspace <tmp/b> --json`; `ee workspace list --json` | Workspace registry JSON, two context pack goldens, provenance scoped to the selected workspace, workspace identity hashes | ambiguous workspace, symlink isolation, moved workspace, missing CASS connector, conflicting project-local config | `eidetic_engine_cli-jqhn` / EE-USR-007, Gate 2 SQLModel/FrankenSQLite, Gate 7 walking skeleton, future workspace/config beads | The agent gets project-specific memory for the selected workspace and does not cross-contaminate unrelated repositories. |

## Required Fixture Metadata

Every scenario fixture needs a manifest with:

- scenario ID and owning bead or gate
- fixture family and seed data files
- workspace path policy and whether symlinks are involved
- command sequence with exact argv
- expected schema names and golden files
- expected artifacts under `target/ee-e2e/<scenario>/<run-id>/`
- deterministic clock, ID provider, ranking seed, and feature profile
- degradation codes expected in each branch
- redaction classes expected or explicitly absent
- provenance IDs and evidence line ranges
- first-failure diagnosis template

## Degraded Branch Requirements

Each degraded branch must prove three things:

1. The response is still parseable and schema-valid.
2. The missing capability is named with a stable code and useful repair action.
3. The agent-facing success signal is either preserved in a reduced form or the
   command refuses with an explicit reason.

Silent partial success is a failure. A context pack that omits CASS evidence
must say CASS was unavailable. A search result that skips semantic scoring must
say lexical-only retrieval was used. An export that redacts evidence must name
the redaction class without exposing the secret.

## Closure Guidance

This bead is a docs-only scenario contract. It does not add executable tests
because the repository is still in the foundation slice and the executable
scenario beads listed above own the future real-binary coverage. Closeout should
cite this file and name the nearest executable follow-ups:

- `eidetic_engine_cli-gbp2` / EE-USR-002
- `eidetic_engine_cli-g2jl` / EE-USR-003
- `eidetic_engine_cli-1mlo` / EE-USR-004
- `eidetic_engine_cli-r8r0` / EE-USR-005
- `eidetic_engine_cli-9sd5` / EE-USR-006
- `eidetic_engine_cli-jqhn` / EE-USR-007

