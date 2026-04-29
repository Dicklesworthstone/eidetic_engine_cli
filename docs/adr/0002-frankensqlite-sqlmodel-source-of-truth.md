# ADR 0002: FrankenSQLite And SQLModel Are The Source Of Truth

Status: accepted
Date: 2026-04-29

## Context

`ee` stores durable memories, evidence, decisions, links, pack records, and
curation history. Search indexes, embeddings, graph snapshots, caches, and
rendered packs are useful but rebuildable. The project must avoid duplicate
storage stacks and forbidden SQLite wrappers.

## Decision

FrankenSQLite through SQLModel is the durable source of truth for `ee`.
Repositories own database access and return domain types. Search indexes, graph
views, cache entries, manifests, and rendered artifacts are derived assets that
can be rebuilt from the database plus configuration.

## Consequences

Durable state has one authority. Recovery is simpler because losing derived
assets requires a rebuild, not data reconstruction. Schema migrations and
repository tests become early gates for product work.

Storage code must not bypass SQLModel/FrankenSQLite. The default dependency tree
must not include `rusqlite`, SQLx, Diesel, or SeaORM.

## Rejected Alternatives

- `rusqlite` for quick local SQLite access.
- SQLx, Diesel, or SeaORM as alternate ORM layers.
- Custom JSONL or file stores as the primary memory database.
- Search or graph indexes as authoritative storage.

## Verification

- Dependency audits fail on `rusqlite`, SQLx, Diesel, and SeaORM.
- Storage contract tests open a temporary FrankenSQLite database through
  SQLModel, run migrations, and round-trip memory-shaped rows.
- Index and graph tests prove derived assets can rebuild from durable records.

