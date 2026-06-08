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

The parser version is currently `docs-bootstrap-v1`. Agent consumers should
branch on that value rather than assuming every future parser extracts the same
candidate classes.

## Degraded Handling

Source handling issues are explicit degraded entries. Common codes include
symlink rejection, oversized sources, total-byte limits, non-UTF-8 files, and
read failures. These entries mean the run is incomplete, not that the command
silently ignored a source.

Prompt-injection-like or unsafe candidate text goes to `curateQuarantine`
instead of becoming a candidate. Quarantined docs text must not be converted to
memory without curation review.
