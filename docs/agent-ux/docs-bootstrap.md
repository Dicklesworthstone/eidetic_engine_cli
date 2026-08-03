# Docs Bootstrap

`ee bootstrap docs` is the cold-start path for turning this repository's own
allowlisted docs into curation candidates. It is not a memory importer.

## Workflow

1. Run `ee bootstrap docs --dry-run --json`.
2. Inspect `data.parserVersion`, `data.runId`, `data.candidates`,
   `data.curateQuarantine`, and both `data.degraded[]` and top-level
   `degraded[]`.
3. Review the proposed candidates through the normal curation flow.
4. Run `ee bootstrap apply <run-id> --approved-only --json` only after the
   relevant curation candidates have been approved.

By default, bootstrap discovery remains deliberately narrow: root policy and
README files plus the built-in ADR, schema, environment-variable, and
failure-mode fixture directories. Add a reference corpus explicitly with the
repeatable `--include <GLOB>` option:

```bash
ee bootstrap docs --dry-run \
  --include SKILL.md \
  --include 'references/**/*.md' \
  --json
```

Include globs are workspace-relative. `*` and `?` match within one path
component; a whole-component `**` matches zero or more components. The first
component must be literal, so `references/**/*.md` is valid while
`**/*.md`, absolute paths, backslashes, and `..` traversal are rejected at
argument parsing. Exact files and recursive matches are sorted and deduplicated
with the built-in classification winning if an include overlaps a default
source.

Apply recompiles the reviewed source set before accepting its run ID. Repeat
the same include selectors and non-default byte limits on apply:

```bash
ee bootstrap apply <run-id> --approved-only \
  --include SKILL.md \
  --include 'references/**/*.md' \
  --json
```

Selector order and duplicates do not affect the run ID. Omitting or changing a
selector does. The normalized selector set is returned as `data.includeGlobs`
so a saved dry-run response contains the exact apply recipe. A glob that matches nothing produces a visible
`docs_bootstrap_source_missing` entry, so an opted-in corpus is never skipped
silently.

The dry-run payload uses `ee.bootstrap.docs.run.v1` inside the standard
`ee.response.v2` envelope. It reports `durableMutation: false`; if that ever
changes, consumers must stop treating the command as a safe preview.

The apply payload uses `ee.bootstrap.docs.apply.v1` and reports materialized,
approved, applied, skipped, unchanged, and blocked counts. Applying can mutate
curation state or approved memories, so automation must require
`--approved-only` and should keep the run ID tied to the exact dry-run it
reviewed.

## Candidate Contract

Bootstrap candidates are proposals, not memories. They carry source paths,
content hashes, line and byte spans, anchors, trust class, parser version, and
redaction/quarantine metadata so a reviewer can decide whether a docs excerpt
belongs in memory.

Explicitly included files use `sourceKind: "reference_doc"`. Their candidates
remain conservative `agent_assertion` inputs, and approved derived memories
carry the `source_kind:reference_doc` tag plus the source kind in evidence and
producer metadata. Packs and lenses can therefore distinguish curated
reference material from root policy and session-derived memories without
discarding byte-span provenance.

The parser version is currently `docs-bootstrap-v1`. Agent consumers should
branch on that value rather than assuming every future parser extracts the same
candidate classes.

## Degraded Handling

Source handling issues are explicit degraded entries. Common codes include
symlink rejection, oversized sources, total-byte limits, non-UTF-8 files, and
read failures. These entries mean the run is incomplete, not that the command
silently ignored a source.

Discovery never follows included symlink files or directories. The existing
per-source and aggregate byte limits apply after deterministic glob expansion,
just as they do for default sources. Expansion also stops at 128 directories
of recursion, 16,384 inspected entries, or 4,096 unique included sources and
surfaces `docs_bootstrap_total_limit_reached` instead of returning a silent
partial crawl.

Prompt-injection-like or unsafe candidate text goes to `curateQuarantine`
instead of becoming a candidate. Quarantined docs text must not be converted to
memory without curation review.
