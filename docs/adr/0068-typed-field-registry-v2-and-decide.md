# ADR 0068: Typed-Field Registry v2 and the Decide Workflow

Status: proposed
Date: 2026-06-10
Bead: bd-3mgkw.1 (epic bd-3mgkw, 2026-06 idea-wizard wave)

## Context

Typed memory fields (`ee.memory.typed_fields.v1`, `src/models/memory.rs`)
already cover FIVE kinds — Failure `{cause, regression_surface,
reverted_at_sha, family}`, Decision `{options, chosen, rationale,
supersedes}`, Command `{command, when_to_use, exit_meaning}`, Risk and
AntiPattern `{trigger, blast_radius, safer_alternative}` — with
`MAX_TYPED_MEMORY_FIELDS = 4`. (Early planning believed v1 was
failure-only; the 2026-06-10 fact-check corrected this, and the corrected
scope is what this ADR records.) Two gaps remain: Rule and Convention kinds
have no fields, decisions have no revisit horizon, and the search-side
`--field` filter supports only `NAME=VALUE` exact matching. On top of the
registry, `ee decide` adds a micro-ADR workflow over decision-kind
memories — record/list/revisit with supersede chains and fork refusal — so
settled questions stop being re-litigated. This ADR fixes the v2 registry,
operator semantics, indexing coverage, and the decide contract before
implementation (bd-3mgkw.2/.3).

## Decision

### 1. Registry v2 (additive; static code table)

`ee.memory.typed_fields.v2` extends v1 — **every v1 sidecar validates
unchanged under v2** (contract-tested against existing fixtures):

| Kind | Fields (NEW in bold) |
|---|---|
| failure | cause, regression_surface, reverted_at_sha, family — unchanged |
| decision | options, chosen, rationale, supersedes, **revisit_by** (rfc3339) |
| command | command, when_to_use, exit_meaning — unchanged |
| risk / anti-pattern | trigger, blast_radius, safer_alternative — unchanged |
| **rule** | **condition** (text), **action** (text), **exceptions** (text_list) |
| **convention** | **scope** (text), **pattern** (text) |

- Bounds: `MAX_TYPED_MEMORY_FIELDS` raises 4 → 8 (decision now needs 5;
  headroom is deliberate and bounded); per-field and total-JSON byte caps
  re-derived proportionally and documented with the migration note
  (bd-3mgkw.5 migration-guide obligation). This is a deliberate bound
  change, not drift — goldens update with it.
- The registry stays a **static code table** (field name, value type
  `text | text_list | rfc3339`, bounds, search-indexed flag). User-defined
  fields are explicitly out of scope for v2 (rejected below).
- Validation errors are per-field and actionable: `typed_field_unknown` /
  `typed_field_invalid` (usage errors, exit 1) carry the offending field,
  the reason, and the valid field list for that kind.

### 2. Search `--field` operator generalization

Today the CLI surface is `NAME=VALUE` exact. v2 adds two operators,
chosen for unambiguous one-character syntax after the name:

| Syntax | Semantics |
|---|---|
| `--field name=value` | exact (unchanged) |
| `--field name~substr` | contains (case-sensitive byte substring) |
| `--field 'name^prefix'` | prefix |

Escaping: a literal `=`, `~`, or `^` in the VALUE needs no escaping (the
first separator after the name wins); field NAMES are registry identifiers
and never contain operator characters. Filter compilation integrates with
the query plan cache keyed on the full operator expression.

### 3. Search-document indexing coverage

The registry's `search-indexed` flag governs which fields land in canonical
search-document metadata (the existing `family` pattern generalized).
Indexed in v2: failure.family (existing), failure.cause, decision.chosen,
decision.supersedes, command.command, rule.condition, convention.scope.
Non-indexed fields remain filterable only via post-retrieval row checks
(documented; the planner chooses based on the flag). Index coverage changes
ride the normal index-generation machinery (`ee index rebuild` refreshes).

### 4. `ee decide` (a veneer — zero new storage concepts)

- `ee decide record "<topic>" --chosen "<x>" --option "<y>"…
  --rationale "<why>" [--revisit-by <rfc3339|+90d>] [--supersedes <id>]`
  creates a decision-kind memory whose typed fields are the REAL registry
  names: `options` (all alternatives incl. the chosen one), `chosen`,
  `rationale`, `supersedes`, `revisit_by`. `--supersedes` ALSO wires the
  standard supersede link + validity-window close on the predecessor
  (field and link carry the same fact; the link drives graph/lifecycle,
  the field makes it extractable).
- **Fork refusal**: re-deciding the same normalized topic WITHOUT
  `--supersedes` errors with the prior chain head's id (usage error,
  structured payload). Silent decision forks are how settled questions get
  re-litigated; explicitness is the feature. Topic normalization:
  lowercase, whitespace-collapsed, stopword-light — documented and
  deterministic.
- `ee decide list [--about <substr>] [--include-superseded]`: chain heads
  with fields, chain depth, revisit status; deterministic order (most
  recent first, id tie-break); governor truncation point on the list.
- `ee decide revisit`: decisions with `revisit_by ≤ now + [decide]
  revisit_warning_days` (default 7), split due/overdue. Wire-through: `ee
  orient` surfaces a bounded count + ids; the subscribe surface exposes a
  decision-revisit query so polling harnesses see revivals.
- Effects: record = durable memory write (audited, standard remember
  pipeline); list/revisit = read-only.

### 5. Coordination obligations

- `ee conflict resolve` (ADR 0066) writes its rationale memory with these
  registry fields — one vocabulary, no migration later.
- `ee ask` (ADR 0067) gains precision from indexed fields for free once §3
  lands; no coupling needed.
- Batch remember (bd-1pi9m.4) accepts a `fields` object per JSONL line
  validated by the same registry path.

## Consequences

- **Easier**: memories an agent can OPERATE on (`--field command~release`
  → executable; `decide list --about storage` → the decision log with
  alternatives); per-workspace lightweight ADRs with self-resurfacing
  revisit horizons.
- **Guarded**: additive v2 (no v1 migration hazard — the corrected
  fact-check scope removed the one early planning assumed); static
  registry prevents key sprawl; fork refusal preserves chain integrity.
- **Costs accepted**: bound raise updates goldens once; two new operators
  expand the filter-parse surface (property-tested against injection of
  operator chars in values).

## Rejected Alternatives

- **User-defined field schemas**: fragments search semantics, breaks
  golden tests, invites key sprawl ('priority'/'prio'/'p'). If real demand
  emerges, that is a v3 ADR with namespacing — record demand as beads.
- **Invented decide field names** (`alternatives` etc. from early
  planning): superseded by the real registry names; `alternatives` ≡
  existing `options`.
- **decide as a new storage concept**: decision memories + supersede links
  + validity windows all pre-exist; a veneer keeps one lifecycle. Rejected
  building any parallel store.
- **Regex `--field` operator**: unbounded cost + injection surface on a
  hot filter path; contains/prefix cover the observed need. Deferred, not
  precluded.

## Verification

- Unit (bd-3mgkw.2): per-kind validation matrices (valid / unknown /
  wrong-type / oversize / list-overflow); v1 fixture compatibility; filter
  operator semantics incl. values containing `=`/`~`/`^`; index round-trip
  (remember with field → index job → search --field finds it); stable
  render order in show/pack provenance.
- Unit (bd-3mgkw.3): chain semantics (record / supersede / fork-refusal),
  revisit due math incl. timezone edges, list determinism, orient
  integration shape.
- Property (bd-3mgkw.4): field values (unicode, max-bytes, operator chars)
  never panic and round-trip byte-identically; operators never match
  outside their documented semantics.
- E2E (bd-3mgkw.4): `scripts/e2e_typed_fields_decide.sh` — one memory per
  kind with fields; operator matrix; decide record → supersede → fork
  refusal → revisit surfacing in orient; batch JSONL fields object; all
  steps logged as `ee.test_event.v1`.

## Appendix: decide record shapes (normative draft)

Standalone schema files ship with bd-3mgkw.3; drafts are normative.

```text
object ee.decide.record.v1 (under data.decision)
  schema       const "ee.decide.record.v1"
  memoryId     string
  topic        string          (normalized form also returned: topicKey)
  fields       {options: string[], chosen: string, rationale: string,
                supersedes: string|null, revisit_by: rfc3339|null}
  supersededMemoryId string|null
  chainDepth   integer
  auditId      string

object ee.decide.list.v1 (under data.decisions; governor point items[])
  items[]: {memoryId, topic, chosen, chainDepth, revisitStatus:
            "none"|"due_soon"|"overdue", revisitBy: rfc3339|null}

object ee.decide.revisit.v1 (under data.revisit)
  due[]: items as above; overdue[]: items as above
```
