# Agent Journal Capture

`ee journal` is the working-tier capture lane for observations that are useful
to review later but are not yet durable memory. It is append-only by default,
redacted before storage, and intentionally excluded from the search index until
a reviewed distillation or remember command promotes the evidence.

## Decision Table

| Need | Use | Why |
|---|---|---|
| Raw observation during a live session | `ee journal append` | Low-friction capture without claiming the text is durable guidance. |
| Confirmed rule, fact, decision, or failure lesson | `ee remember` | Writes a durable searchable memory with provenance and scoring. |
| Repeated missed demand from search or ask | `ee learn gaps` | Clusters retained query misses into capture templates. |
| Command timing, degraded codes, and run metadata | `ee recorder` / flight recorder | Keeps workload telemetry separate from memory content. |
| Existing memory was corroborated | `ee remember --reinforce` or `ee outcome` | Avoids duplicate memories and records bounded evidence. |

## Hook-Fed Capture

Harness hooks should use the journal for ephemeral observations:

```bash
ee journal append "RCH proof failed with artifact rsync timeout on worker vmi1156319." \
  --workspace . \
  --source hook \
  --session "$SESSION_ID" \
  --agent "$AGENT_NAME" \
  --json
```

For batches, write one JSON object per line and pipe it through stdin:

```bash
printf '%s\n' \
  '{"body":"RCH retry passed after rsync exclusion.","source":"hook","kind":"success"}' \
  '{"body":"Pack item 2 cited stale release policy.","source":"hook","kind":"surprise"}' \
  | ee journal append --workspace . --stdin --json
```

Each line succeeds or fails independently. A retry with the same content and
metadata is safe: journal ids and distillation proposal ids are deterministic
for the reviewed evidence, and later distill/apply steps skip already-consumed
entries instead of creating duplicate candidates.

## End-Of-Session Flush

End a session by reviewing the journal before writing durable memory:

```bash
ee journal distill --workspace . --session "$SESSION_ID" --dry-run --json
```

Read `data.proposals[]`, `data.abstentions[]`, and `data.degraded[]`.
`distill_no_candidates` is an honest empty result, not a failure. If proposals
look grounded, apply them as curation candidates:

```bash
ee journal distill --workspace . --session "$SESSION_ID" --apply --json
ee curate candidates --workspace . --json
```

Candidates still require ordinary curation review. The journal distiller does
not silently promote observations into procedural rules or permanent facts.

## Durable Memory And Reinforcement

Use `ee remember` only after the observation is stable enough to retrieve later:

```bash
ee remember "RCH verification must use scripts/rch_verify.sh, not local cargo." \
  --workspace . \
  --level procedural \
  --kind rule \
  --json
```

When the text corroborates an existing nearby memory, prefer reinforcement:

```bash
ee remember "RCH verification must use scripts/rch_verify.sh, not local cargo." \
  --workspace . \
  --reinforce \
  --json
```

For curated imports, batch durable writes through JSONL:

```bash
printf '%s\n' \
  '{"content":"Use br show before editing a claimed bead.","level":"procedural","kind":"rule"}' \
  '{"content":"RCH sync failures should be retried after checking worker health.","level":"episodic","kind":"failure"}' \
  | ee remember --batch --stdin --workspace . --json
```

## Grade The Pack You Used

When a pack item materially helped or misled the work, grade the specific pack
item rather than the whole session:

```bash
ee outcome --pack <pack-id> --item <n> \
  --workspace . \
  --signal helpful \
  --reason "Included the RCH-only verification rule before edits." \
  --json
```

Use `harmful` when the item caused wasted work or pointed at stale instructions.
Inspect the feedback trail later with:

```bash
ee outcome trace <memory-id> --workspace . --json
ee audit timeline --target <memory-id> --workspace . --json
```

## Config And Retention

Journal capture is controlled by:

```toml
[journal]
enabled = true
retention_days = 14
```

`EE_JOURNAL_ENABLED` and `EE_JOURNAL_RETENTION_DAYS` override those keys.
When capture is disabled, journal commands report `journal_disabled`. Retention
is enforced by the explicit `journal-retention` steward job with audit evidence;
ordinary journal commands do not delete rows in the background.

## Anti-Patterns

| Anti-pattern | Use instead |
|---|---|
| Promoting every note directly to procedural memory | Journal first, then distill and curate repeated evidence. |
| Treating raw journal entries as search results | Remember or apply curation candidates when evidence should be retrievable. |
| Recording pack feedback against a guessed memory id | Use `ee outcome --pack <pack-id> --item <n>`. |
| Running local Cargo just to validate docs or memory capture | Use the project verification lane required by the active task, usually RCH. |
