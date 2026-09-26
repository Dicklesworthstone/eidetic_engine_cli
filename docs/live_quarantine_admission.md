# Live feedback-quarantine admission

Pending feedback review is a current source-authority hold, not an indexed
trust label and not a permanent verdict about a memory or rule. Search's live
admission boundary checks these holds before candidates participate in ranking,
relevance thresholds, duplicate suppression or query hints. Context and pack
construction receive the admitted retrieval results rather than resurrecting
held candidates from the old index.

## Exact native ownership

A hold must match all three source fields: target ID, target type and the
owning workspace. A pending `memory` event withholds that memory; a pending
`rule` event withholds that procedural rule. A row belonging to another
workspace, another entity kind or an already-reviewed event is not a hold on
the candidate.

Rules remain independent native entities. Their source-memory IDs describe
lineage, not feedback aliases. Holding a source memory does not implicitly
hold an independently reviewed native rule, and holding a rule does not
implicitly hold its source memories. Existing lineage-identity and workspace
checks still apply. Rule bodies continue to come from canonical rule records,
not from private source-memory bodies or stale indexed metadata.

## Review, snapshots and privacy

Pending holds apply at every requested reference time. `--as-of`, inclusion
flags, protected-rule status, maturity and indexed trust metadata cannot bypass
them. A held memory also cannot seed lexical-query construction or semantic
embedding. A closed seal retains its stronger explanation; reviewing feedback
does not reveal a seal or resurrect an obsolete revision.

All authority reads for a candidate use the caller's source snapshot, or an
owned read-only snapshot when none is supplied. A concurrent hold or review
belongs to the next source snapshot; the admission helper does not commit,
replace or release a caller-owned transaction.

Queries bind candidate IDs in pages of 256 and return distinct held identities.
They read neither feedback reasons nor source identifiers, event payloads or
memory bodies. Multiple pending events keep a target held until no matching
pending event remains. Source rows, trust scores, audit logs, feedback records,
queued index work and persisted indexes are not changed by admission.

## Diagnostics and recovery

`quarantined_memory_filtered` reports the number of withheld memory candidates,
without returning their IDs, bodies or private feedback details. Native rules
use `rule_live_admission_filtered`, whose repair guidance now includes source
review before derived-index repair. Failure to read quarantine authority
withholds the affected native candidates rather than trusting the index;
unrelated evidence candidates remain available.

Inspect the existing queue in the addressed workspace:

```bash
ee outcome quarantine list --workspace /path/to/workspace --json
```

Use the existing explicit review workflow to resolve pending entries. Admission
never releases or rejects feedback automatically. Once all matching holds are
reviewed, the next snapshot can admit the source again without rebuilding its
index, provided the other lifecycle, revision, scope and seal checks still pass.
An index rebuild does not resolve a pending review hold. For unreadable source
authority, inspect `ee doctor --json` before attempting derived-state repair.

## Scope and regressions

This change covers live retrieval admission and similarity seeds. It does not
modify stored-pack replay, quarantine-write policy, feedback scoring or the
broader read-fence/write-immune machinery.

The added real-store Rust regressions cover native ownership, multiple holds,
private diagnostics, historical/inclusion bypass attempts, seal and supersession
precedence, borrowed and owned snapshot transitions, unavailable quarantine
storage, bind-safe candidate pages, independent rule lineage, and public memory
search against an unchanged published index. Run these through the repository's
RCH-gated Rust validation workflow; SQL reference checks are not FrankenSQLite
or application-test evidence.
