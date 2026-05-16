# Mesh Command Modes

Status: proposed
Bead: bd-3omr5
ADR: docs/adr/0037-optional-mesh-memory.md
Matrix: docs/mesh/verification_matrix.md

## Purpose

Mesh-aware commands need an explicit latency and freshness contract. The default
remains local-first: no command contacts peers, opens listeners, probes caches,
or changes response shape unless mesh is enabled by config, environment, or an
explicit command flag.

The shared mode vocabulary is:

| Mode | Contract | Peer network | Cached peer material | Revision surface |
| --- | --- | --- | --- | --- |
| `off` | Strict local-only behavior. Output must be byte-stable with non-mesh runs except for explicit diagnostics that the caller requested. | Never | Ignored | None |
| `cache` | Read only already-authorized cached mesh material for the current workspace. | Never | Allowed after import decision | None |
| `revisable` | Return the local/cache answer promptly and emit explicit revision availability tokens when fresher peer data may arrive later. | No blocking peer wait | Allowed after import decision | Required when stale/future peer material is known |
| `blocking` | Reserved opt-in for future hard-budget peer freshness. Until implemented, commands must fail honestly or report an unsupported degraded code instead of silently waiting. | Only after implementation | Allowed after import decision | Required |

## Surface Matrix

| Command | `off` | `cache` | `revisable` | `blocking` |
| --- | --- | --- | --- | --- |
| `ee search --mesh <mode>` | Local index only. | Local index plus authorized cached mesh documents. | Same as `cache`, plus revision token metadata. | Deferred until a latency-budgeted peer query exists. |
| `ee context --mesh <mode>` | Local pack only. | Pack may include authorized cached mesh material with namespace provenance. | Pack remains immediately usable and includes revision token metadata. | Deferred until a latency-budgeted peer query exists. |
| `ee pack --mesh <mode>` | Same as `context` for triad pack calls and `pack build`. | Same as `context`. | Same as `context`. | Same as `context`. |
| `ee why --mesh <mode>` | Explain only local memory evidence. | Explain authorized cached namespace provenance when present. | Include revision ancestry when present. | Deferred until peer freshness is implemented. |
| `ee status --mesh <mode>` | Report mesh disabled/local-only posture. | Report cache capability and authorized/denied counts only. | Report revision-token capability and stale-revision counts. | Report unsupported unless peer blocking is implemented and enabled. |

## Precedence

The effective mode is selected in this order:

1. Command flag: `--mesh off|cache|revisable|blocking`
2. Environment: `EE_MESH_MODE`
3. Config: `[mesh] command_mode = "..."`
4. Built-in default: `off`

`EE_MESH_ENABLED=false` or `[mesh] enabled = false` keeps ordinary commands
local-first. A command may still parse a non-`off` mode for diagnostics and
planning, but runtime mesh behavior must remain gated by the feature/config
checks owned by the later SRR6 implementation beads.

## Response Envelope Rules

- `off`: no mesh-specific `degraded[]` entry is emitted merely because mesh is
  disabled. That is expected local-first behavior.
- `cache`: denied or quarantined cached material is counted only in mesh/status
  posture; it is never included in search, context, why, graph, or curate
  outputs.
- `revisable`: revision metadata is explicit and redaction-safe. A consumer can
  decide whether to re-query later without treating the first response as
  incomplete.
- `blocking`: while unimplemented, commands must return an honest
  unsupported/degraded response rather than silently falling back to `cache` or
  waiting on peers.

## Verification

The companion fixture `tests/fixtures/mesh/command_modes.v1.json` is the stable
machine-readable matrix for parser, docs, and e2e assertions. Runtime tests that
implement cache/revisable/blocking behavior must cite both this document and the
SRR6 verification matrix in their closeout evidence.
