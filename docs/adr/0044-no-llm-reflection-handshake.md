# ADR 0044: No-LLM Reflection Handshake

Status: proposed
Date: 2026-05-23
Bead: bd-3kqr0

## Context

The external-derivation candidate design in ADR 0043 gives ee a generic way to
propose a new memory from existing memories or evidence spans:
`create_derived_memory`. Reflection should build on that primitive without
turning ee into an agent loop, workflow engine, planner, or LLM client.

The useful reflection shapes are typed metacognitive consolidations:
summaries, insights, knowledge gaps, strengths, plans, questions, procedural
extractions, and contradiction resolution. Those outputs may come from a human
or from an agent harness that calls an LLM, but the transport and model choice
belong outside ee. ee's job is to package deterministic source material, verify
a structured result, and route accepted results into the existing curation
pipeline.

This boundary preserves the project rules:

- ee remains local-first and CLI-first.
- The harness owns deliberation, tool use, and LLM transport.
- Durable mutations still go through `ee curate candidates`, `ee curate
  validate`, and explicit `ee curate apply`.
- No raw private chain-of-thought is stored.
- No LLM SDK, HTTP stack, Tokio runtime, or parallel curation store is added.

## Decision

Introduce a reflection handshake with two stable artifacts:

1. `ee.reflect.request.v1`
2. `ee.reflect.result.v1`

`ee reflect propose` emits a request artifact. The external harness may send
that request to a model or a human reviewer. `ee reflect ingest` reads a result
artifact, validates it against the original request and current source hashes,
then creates ordinary curation candidates. In v1, accepted low-risk reflection
results create `create_derived_memory` candidates. They do not create memories
directly and do not auto-apply.

Reflection kind is producer metadata, not a candidate mutation shape.
`CandidateType` should not grow `reflection_gaps`, `reflection_summary`, or
similar variants. The mutation shape is still `create_derived_memory`; the
semantic kind lives in derivation metadata.

## Request Artifact

An `ee.reflect.request.v1` artifact contains:

- `requestId`: UUIDv7 for this request instance.
- `requestHash`: deterministic hash over canonical request inputs.
- `workspaceId`: the workspace identity.
- `reflectionKind`: one of the supported kind strings.
- `sources`: memory and evidence-span refs with ids, content hashes, and
  redacted excerpts.
- `promptTemplate`: deterministic template id, version, and hash.
- `responseSchema`: schema id and hash for the expected result.
- `challenge`: HMAC challenge with key id and signature material.
- `createdAt` and `expiresAt`: request lifetime.
- `callerHints`: non-binding hints for the harness.

`requestHash` excludes volatile fields such as `requestId`, timestamps,
expiration, and challenge bytes. It includes the workspace id, reflection kind,
canonical source package, prompt-template identity, response-schema hash, and
any deterministic policy knobs that affect the expected answer. JSON map order
must not change the hash.

Request artifacts are local files or stdout JSON. They are not jobs, workflows,
or daemon tasks. Emitting a request does not imply that ee will run a model.

## Result Artifact

An `ee.reflect.result.v1` artifact contains:

- `requestId`: copied from the request.
- `requestHash`: copied from the request and verified.
- `challenge`: response over the original challenge, including key id.
- `producer`: harness or human identity metadata.
- `reflectionKind`: copied from the request.
- `citedSourceIds`: subset of request source ids.
- `body`: distilled output only.
- `kindFields`: kind-specific structured fields.
- `selfReportedConfidence`: informational only.

The result must cite only request sources. It may not introduce new source ids,
may not claim hidden evidence, and may not ask ee to perform follow-up actions.
The result body is content for curation review, not an instruction stream.

## Candidate Routing

The initial low-risk kinds route to `create_derived_memory`:

| Reflection kind | Candidate route |
| --- | --- |
| `summary` | `create_derived_memory` |
| `insight` | `create_derived_memory` |
| `gaps` | `create_derived_memory` |
| `strengths` | `create_derived_memory` |
| `question` | `create_derived_memory` |
| `plan` | `create_derived_memory` |

The following kinds are deferred because they can mutate procedural or
contradiction state:

| Reflection kind | Deferred route |
| --- | --- |
| `procedural_extract` | Needs a later ADR before using rule/procedure candidates |
| `contradiction_resolve` | Needs a later ADR before using supersede/retract paths |

`ee reflect ingest` creates a pending curation candidate with:

- `candidateType = "create_derived_memory"`.
- `targetMemoryId = null`.
- `derivation_source_refs_json` copied from the cited request sources.
- `derivation_metadata_json` carrying `reflectionKind`, request identity,
  request hash, prompt-template hash, response-schema hash, producer metadata,
  and kind-specific result fields.
- proposed content equal to the redacted distilled body.
- trust class behavior equivalent to agent assertion until normal outcome
  evidence matures it.

Accepted reflection results enter the same queue as any other curation
candidate. Users or agents inspect them with `ee curate candidates`, validate
them with `ee curate validate`, and apply them explicitly with `ee curate
apply`.

## Validation And Failure Semantics

Ingest fails closed before creating any candidate when:

- the result schema is invalid;
- the request id is unknown;
- the request has expired;
- the HMAC challenge does not verify;
- the result request hash does not match the stored request;
- current source content hashes drifted from the request;
- `citedSourceIds` is not a subset of request sources;
- the reflection kind is unsupported or routed to a deferred high-risk path;
- kind-specific fields are missing or vague;
- the body contains raw chain-of-thought markers instead of distilled output;
- prompt-injection, secret, scope, or redaction policy checks fail.

Failure output uses the normal `ee.error.v2` envelope and structured recovery
actions. If a new degraded or error code is added, it needs the standard
failure-mode fixture and taxonomy entry in the same implementation slice.

Rejected results leave no curation candidate behind. Accepted results create
only pending candidates. Applying a candidate remains a separate audited
transaction owned by the curation pipeline.

## Security And Privacy Boundaries

ee does not include an LLM SDK or network client for reflection. A harness may
call a model, but that call is outside ee's dependency tree and audit surface.

Request excerpts must be redacted according to the same memory and provenance
policy used by context packs. Result bodies are redacted and policy-checked
before candidate creation. Private chain-of-thought is not accepted as source
material or result content; only distilled summaries, claims, gaps, questions,
or plans may be stored.

The HMAC challenge proves that the result corresponds to a request generated by
this ee installation. It is not an authorization to apply the candidate, and it
does not promote trust. Trust still comes from source hashes, validation, and
later outcome evidence.

## Consequences

- Reflection can improve memory quality without making ee autonomous.
- The same create-derived candidate path serves deterministic review-session
  proposals and external reflection outputs.
- The public contract cleanly separates request packaging, external reasoning,
  result ingest, candidate validation, and candidate apply.
- Future reflection kinds can add validators and templates without adding
  candidate types when they still create derived memories.
- High-risk procedural and contradiction paths remain deliberately unshipped
  until their mutation rules are separately designed.

## Rejected Alternatives

1. **Calling an LLM directly from ee.** Rejected because it would add model
   transport, credentials, retries, network failure modes, and dependency risk
   to a local memory CLI.
2. **Auto-applying reflection results.** Rejected because reflection output is
   agent assertion until validated and reviewed through the existing curation
   pipeline.
3. **Adding per-kind candidate types.** Rejected because candidate type should
   describe mutation shape. Reflection kind is producer metadata.
4. **Storing raw chain-of-thought.** Rejected because ee stores durable,
   auditable memory content, not private deliberation transcripts.
5. **Using reflection as a workflow engine.** Rejected because plans and gaps
   are curation candidates or recommendations, not scheduled actions.

## Verification Hooks

Implementation slices should prove:

- identical canonical inputs produce identical `requestHash` values;
- volatile fields are present but excluded from `requestHash`;
- invalid HMAC, expired request, request-hash mismatch, source drift, unknown
  cited source ids, raw chain-of-thought, prompt injection, and secret material
  all fail before candidate creation;
- accepted low-risk results create pending `create_derived_memory` candidates
  with `targetMemoryId = null`;
- no accepted result auto-applies;
- no forbidden LLM SDK, HTTP stack, Tokio, or other banned dependency enters
  the dependency tree.
