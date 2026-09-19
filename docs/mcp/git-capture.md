# Git-backed memory capture over MCP

`ee_capture_git` delegates to the same `ee remember --from-* --json` path as the CLI.
It collects repository evidence and applies the normal capture redaction, memory
admission, audit, and indexing rules. It never runs a shell command supplied by
the client or bypasses memory policy.

A workspace and mode are required. `commit` captures one commit (default `HEAD`),
`diff` requires a base or revision range in `reference`, and `worktree` captures
tracked working changes and forbids a reference. Worktree capture includes
staged-only tracked files before the first commit; untracked files are excluded.
The source fingerprint covers the complete sanitized diff, not only its bounded
visible excerpt. Working files can still change while Git reads them.

Preview a commit:

```json
{"name":"ee_capture_git","arguments":{"workspace":"/workspace/project","mode":"commit","reference":"HEAD"}}
```

Apply a reviewed working change with an explicit retry key:

```json
{"name":"ee_capture_git","arguments":{"workspace":"/workspace/project","mode":"worktree","dryRun":false,"allowWrite":true,"idempotencyKey":"release-fix-1","tags":"release"}}
```

`allowWrite: true` alone does not apply anything. Durable writes require both
`dryRun: false` and `allowWrite: true`; the adapter then emits the CLI's required
`--apply` flag. Unknown arguments are refused, including raw content, provenance
overrides, and secret-policy bypasses. A retry key uses remember's existing
content-based identity: identical capture content returns the original memory;
changed content under that key is a conflict, not another write. Other metadata
is not part of remember's existing idempotency comparison.

Optional controls include `level`, `kind`, `tags`, `confidence`, `workflow`,
`validFrom`, `validTo`, `noAutoLink`, and `noProposeCandidates`. Tool discovery
reports the potential durable effects even though preview is the default.
Responses are the original CLI response envelopes, including redaction,
idempotent-replay and failure outcomes. `ee_remember` remains unchanged for
manually supplied memory content.
