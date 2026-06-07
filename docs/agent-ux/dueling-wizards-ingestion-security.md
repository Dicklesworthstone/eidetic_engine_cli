# Dueling Wizards Ingestion Security Contract

This document is the `bd-1n0np.23.3` cross-cutting contract for external-text
ingestion in the dueling-wizards initiative. It applies before new text from
docs-bootstrap, error-log diagnosis, or sandbox imports can become memories,
candidate rows, overlays, fingerprints, or derived evidence.

The executable manifest is
`tests/fixtures/contracts/dueling_wizards_ingestion_security.json`; the CI guard
is `tests/contracts/dueling_wizards_ingestion_security.rs`.

## Required Pipeline

Every covered ingestion surface must run the same ordered gates:

1. `source_classification`
2. `secret_redaction`
3. `prompt_injection_guard`
4. `quarantine_not_store`
5. `audit_event`
6. `regression_corpus`

The manifest's `guardOrderMatrix` pins that order for each surface. Each row
must keep `redactionBeforePromptGuard` and `promptGuardBeforeStorage` true,
`rawStorageBeforeGuards` set to `forbidden`, and
`flaggedStorageDisposition` set to `quarantine_not_store`. The matrix mirrors
the per-surface `requiredPipeline` arrays so a future implementation cannot
claim complete coverage by listing the right gates in the wrong order.

`secret_redaction` means the content is passed through
`crate::policy::redact_secret_like_content` before persistence or output.
`prompt_injection_guard` means the redacted content is passed through
`crate::policy::detect_instruction_like_content` before persistence.

If the prompt-injection guard flags content, the surface must route the item to
curate quarantine and must not store the original external text as authoritative
memory. The accepted behavior is `quarantine_not_store`; raw external text
storage is forbidden by default.

## Covered Surfaces

### docs_bootstrap

Docs bootstrap imports external or workspace documentation into the memory
system. It must classify the source, redact secrets, screen instruction-like
payloads, emit an audit event, and route flagged documents to quarantine before
they can become candidates or memories.

### error_log_diagnosis

Error-log diagnosis consumes stderr, build logs, RCH blocker tails, and other
diagnostic text. These logs may include copied instructions or credentials, so
the surface stores fingerprints and redacted spans by default. Flagged text
cannot become authoritative error-recall evidence without quarantine review.

### sandbox_import

Sandbox import consumes artifacts produced by isolated or experimental runs.
Sandbox output is external text even when it came from this repository. The
surface must redact, guard, audit, and quarantine before any imported text is
linked into memories or graph-derived explanations.

## Current Source Anchors

The existing reusable primitives are in `src/policy/mod.rs`:

- `detect_instruction_like_content`
- `redact_secret_like_content`
- `InstructionLikeReport`
- `SecretRedactionReport`

Existing candidate-validation wiring is in `src/curate/mod.rs`:

- `validate_candidate`
- `PromptInjectionFlagged`
- `redact_secret_like_content`
- `detect_instruction_like_content`

Existing outcome-target protection is in `src/core/outcome.rs`:

- `outcome_prompt_injection_guarded_memory`
- `Review or quarantine the memory before recording outcome feedback.`

The runtime work for docs-bootstrap, error-log diagnosis, and sandbox import may
land in separate beads. Those implementations must update the manifest in the
same change if they add a new covered surface, new degraded code, new schema, or
new storage path.

## Regression Corpus Requirements

Each surface must carry regression payload classes for:

- role markup
- ignore-previous-instructions wording
- destructive command coercion
- secret-like token material
- mixed benign evidence plus malicious instruction

## Regression Payload Examples

The manifest's `regressionPayloadExamples` array pins one concrete sample for
each class. Every example must run `prompt_injection_guard`, quarantine flagged
content, forbid raw storage, and emit the audit event for its source surface.

| Payload class | Sample name | Surface | Requires secret redaction | Audit event |
| --- | --- | --- | --- | --- |
| `role_markup` | `chat_role_block` | `docs_bootstrap` | no | `docs_bootstrap_ingestion_security` |
| `ignore_previous_instructions` | `ignore_previous_instructions` | `docs_bootstrap` | no | `docs_bootstrap_ingestion_security` |
| `destructive_command_coercion` | `rm_rf_instruction` | `sandbox_import` | no | `sandbox_import_ingestion_security` |
| `secret_like_token` | `api_key_literal` | `error_log_diagnosis` | yes | `error_log_ingestion_security` |
| `mixed_benign_and_malicious` | `build_error_plus_instruction` | `error_log_diagnosis` | yes | `error_log_ingestion_security` |

Tests should prove quarantine-not-store behavior. Local Cargo fallback is not valid proof; any Rust test proof must run through RCH.
