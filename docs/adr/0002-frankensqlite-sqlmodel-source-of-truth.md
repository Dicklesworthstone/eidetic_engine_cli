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

The live DDL contract is the ordered migration stream in `src/db/mod.rs`,
recorded at runtime in `ee_schema_migrations`. Appendix A DDL in planning
documents is design-source material until it is reconciled into migrations and
the schema conformance tests. Generated SQLModel DDL may become an input to
future migration authoring, but release correctness is judged against the
migrated FrankenSQLite schema.

## Consequences

Durable state has one authority. Recovery is simpler because losing derived
assets requires a rebuild, not data reconstruction. Schema migrations and
repository tests become early gates for product work.

Planning-document schema drift must be explicit. Any table, index, or critical
column change updates the migration stream and the schema conformance tests in
the same change.

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
- Schema drift contract tests inspect a freshly migrated database, pin the live
  table set, critical indexes, critical columns, and known Appendix A
  divergences.
- Index and graph tests prove derived assets can rebuild from durable records.
