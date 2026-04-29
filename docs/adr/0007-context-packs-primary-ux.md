# ADR 0007: Context Packs Are The Primary UX

Status: accepted
Date: 2026-04-29

## Context

The most important `ee` workflow is an agent asking what it should know before a
task. Search results alone are not enough; agents need compact, provenance-backed
context that fits a token budget and explains why each item was selected.

## Decision

Context packs are the primary user experience. `ee context` and related pack
commands receive product priority over daemon, UI, MCP, graph analytics, or
automatic curation work. Packs include provenance, score explanations, degraded
capability notes, stable hashes, and deterministic ordering.

## Consequences

The project optimizes around before-work usefulness. Retrieval, scoring,
redaction, trust, and output rendering are judged by whether they improve
context packs for real agent workflows.

Pack rendering must stay adapter-like: Markdown, JSON, TOON, and future formats
cannot change selection decisions or hide provenance.

## Rejected Alternatives

- Building a web UI before the CLI context loop works.
- Leading with daemon or MCP server implementation.
- Treating search as the whole product.
- Producing packs without provenance or score explanations.

## Verification

- Walking-skeleton tests include `ee context` with JSON and Markdown output.
- Golden tests assert deterministic pack hashes, section ordering, provenance,
  degradation metadata, and stdout/stderr separation.
- Evaluation fixtures such as `release_failure`, `async_migration`, and
  `offline_degraded` measure whether packs surface useful context.

