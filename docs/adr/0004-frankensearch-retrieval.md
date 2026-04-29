# ADR 0004: Frankensearch Owns Retrieval

Status: accepted
Date: 2026-04-29

## Context

`ee` needs hybrid retrieval over memories, sessions, rules, artifacts, and
decisions. Reimplementing BM25, vector storage, reciprocal-rank fusion, or model
selection would create avoidable search infrastructure inside a memory product.

## Decision

Frankensearch is the retrieval engine for lexical and semantic search. `ee`
integrates it through a narrow search module and uses documented degraded
lexical behavior when semantic capabilities are unavailable. `ee` does not
hand-roll BM25, vector stores, or custom fusion as the core path.

## Consequences

Search implementation stays focused on memory-specific filtering, provenance,
index manifests, degraded-state reporting, and explanation. Model selection and
hybrid retrieval mechanics remain upstream responsibilities.

Any fallback must be explicit, deterministic, and reported as degraded. Search
indexes are derived assets, not the source of truth.

## Rejected Alternatives

- Custom BM25 implementation in `ee`.
- Custom vector store or embedding registry in `ee`.
- Hand-rolled reciprocal-rank fusion as the default.
- Requiring semantic search for explicit memory workflows.

## Verification

- Search contract tests index fixed fixture documents and assert stable result
  ordering.
- Forbidden dependency audits cover the selected Frankensearch feature profile.
- Degradation tests prove lexical/manual memory workflows continue when semantic
  search is disabled or unavailable.

