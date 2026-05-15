# closure-lint defer-to-v2 fixtures

These JSONL fixtures cover the `bd-3usjw.26` defer-to-v2 extension in
`scripts/closure-lint.sh`.

- `valid_defer/issues.jsonl`: closed `implements-surface:*` bead with defer-to-v2
  language, a closed sibling `adr:<surface>_v2_design` bead, an open sibling
  `v2:<surface>` bead, and an unexpired `defer_until_iso8601` date.
- `invalid_missing_adr/issues.jsonl`: defer-to-v2 language and open v2 bead,
  but no closed sibling ADR bead.
- `expired_defer/issues.jsonl`: valid carve-out metadata, but an expired
  `defer_until_iso8601` date that should reopen the closed bead with the
  standard expiry comment.
