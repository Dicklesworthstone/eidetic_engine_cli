# Stash janitor — handoff report

**Date:** 2026-05-09
**Project:** /data/projects/eidetic_engine_cli (`ee`)
**Mode:** Quick (full run)
**Branch policy:** main only (per AGENTS.md Rule 2; recovery branch override declined by user)
**HEAD at start:** 0e80f20d (drifted to e59e2de8 during run from concurrent-agent commits — never touched by janitor)
**Branch:** main (never left)

## Verdict summary

| # | Date | Files | +/− | Verdict | Bundle ref |
|---|------|------:|-----|---------|-----------|
| 0 | 2026-05-08 05:15 | 7 | +576 −185 | superseded | refs/stash-backup/000 |
| 1 | 2026-05-08 01:42 | 5 | +255 −201 | superseded | refs/stash-backup/001 |
| 2 | 2026-05-08 01:22 | 2 | +228 −741 | superseded-by-newer-stash | refs/stash-backup/002 |
| 3 | 2026-05-07 23:04 | 5 | +173 −370 | superseded | refs/stash-backup/003 |
| 4 | 2026-05-04 00:52 | 3 | +243 −383 | superseded | refs/stash-backup/004 |
| 5 | 2026-04-29 19:44 | 10 |  +23  −30 | superseded | refs/stash-backup/005 |

**Counts:** 6 stashes triaged · 0 keepers applied · 6 stashes dropped · 0 conflicts · 0 errors.

## What landed on `main`

Nothing. Every triage row was `superseded` (or `superseded-by-newer-stash` for the cache-deletion duplicate of stash 1). The work captured in these stashes had already been committed to main by other means; abandoned proposals (`src/cache/mod.rs` deletion in stash 002, `refresh_centrality` strip-down in stash 003) were correctly **not** re-applied.

## What got dropped

All 6, in highest-index-first order, after per-drop SHA re-verification:

```
git stash drop stash@{5}    # 9806fe80 (EE-002 format changes to review)
git stash drop stash@{4}    # 3775cb81 (causal WIP from 9a09fa4)
git stash drop stash@{3}    # cf183a20 (t8em graph WIP)
git stash drop stash@{2}    # d385d43b (x8y2 pre-6wj5 WIP)
git stash drop stash@{1}    # 7065378e (x8y2 causal promotion WIP)
git stash drop stash@{0}    # 6f07ddbc (other-agent-changes)
```

Per-drop log: `.stash_janitor_workspace/cleanup_log.tsv`. Verbatim authorization captured in `.stash_janitor_workspace/cleanup_authorization.txt`.

## Recovery — three layers, all intact

1. **Backup refs (in `.git/`):**
   ```
   refs/stash-backup/000 → 6f07ddbc
   refs/stash-backup/001 → 7065378e
   refs/stash-backup/002 → d385d43b
   refs/stash-backup/003 → cf183a20
   refs/stash-backup/004 → 3775cb81
   refs/stash-backup/005 → 9806fe80
   ```
2. **Bundle (outside repo, persistent):**
   `/data/projects/eidetic_engine_cli-stash-archive-2026-05-09/`
   - `README.md` — recovery recipes + footgun warnings
   - `index.tsv` — sha/parent/date/shortstat/message per stash
   - `diffs/000..005.diff` — binary-safe stash-aware unified diffs (sha256-verified equal to regenerated output)
   - `meta/000..005.txt` — per-stash recovery commands

### Recovery recipes (any stash, by index N=000..005)

```bash
# Inspect the original stash content
git show refs/stash-backup/NNN
git stash show -p refs/stash-backup/NNN
cat /data/projects/eidetic_engine_cli-stash-archive-2026-05-09/diffs/NNN.diff

# Re-create as a stash list entry
git update-ref --create-reflog refs/stash refs/stash-backup/NNN

# Apply tracked+index changes from the bundled diff (preferred)
git apply --3way --check /data/projects/eidetic_engine_cli-stash-archive-2026-05-09/diffs/NNN.diff
git apply --3way        /data/projects/eidetic_engine_cli-stash-archive-2026-05-09/diffs/NNN.diff

# Cherry-pick HEAD-at-stash side (stash is a merge commit; -m 1 picks HEAD parent)
git cherry-pick -m 1 refs/stash-backup/NNN
```

### When you're sure nothing was lost (suggested wait: 1–4 weeks)

```bash
# Drop the backup refs:
git for-each-ref refs/stash-backup/ --format='%(refname)' | xargs -n1 git update-ref -d

# Drop the bundle (per AGENTS.md NO-DELETION rule — janitor never does this for you):
rm -rf /data/projects/eidetic_engine_cli-stash-archive-2026-05-09
```

## Push

The janitor made **zero commits** on `main`, so there is nothing new to push from this run. `main` advanced four commits during the run from concurrent agents (e59e2de8, e0cf5093, 2e2b1f38, 31545508) — those are theirs to push, not ours.

## Things you should know

- `git status` currently shows `tests/pack_quality_contracts.rs` modified and `.stash_janitor_workspace/` untracked. Both are concurrent-agent or skill artifacts — neither was created or modified by the destructive phase.
- The `.stash_janitor_workspace/` directory is the run's transient workspace. It contains the audit trail for this run (`triage.tsv`, `cleanup_log.tsv`, `cleanup_authorization.txt`, `bundle_verification.log`, `project_profile.json`, etc.). Per AGENTS.md NO-DELETION rule, the janitor does not delete it. Move it under `~/agent-runs/` or `tar -czf` and archive when convenient.
- `stash@{4}` (004.diff) would not have applied cleanly to current HEAD even if it had been a keeper — `git apply --3way --check` flagged conflicts on `.beads/issues.jsonl` (constantly mutated by `br`) and `src/core/causal.rs` (heavily moved). The diff is preserved verbatim in the bundle for forensic reference.
