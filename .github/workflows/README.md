# Workflows in this directory

## IF YOUR WORKFLOW PUSHES TO `main`, ITS COMMITS ARE NOT GATED BY CI

Read this before adding another delivery workflow. It is the hole bd-fy92m was
filed for, and the next one added here will fall into it by default.

**Measured 2026-09-20T00:5xZ over a 6h window of `origin/main`, counted two
ways — because the two ways disagree by a factor of three:**

```
                      commits   has ANY run      run that SUCCEEDED
bot (github-actions)     28       0   (0%)          0   (0%)
human                    70      63  (90%)         23  (33%)

cancelled CI Static runs in the sample: 84 of 200 (42%)
```

Zero of twenty-eight for bots, by either count. `ci-static.yml` has **no paths
filter** — its trigger is `push: branches: [main]` — so every push to `main`
should run it. Pushes made by workflows in this directory never have.

**Do not quote the 90%.** It counts a run that *existed*, and 42% of runs in
this repo are cancelled. A cancelled run checked nothing, so it is coverage in
a status line and not in fact. The honest human number is **33%**.

The likely mechanism for the cancellations is this workflow's own concurrency:
push/PR runs share a `github.ref`-keyed group with `cancel-in-progress: true`,
so under a multi-agent swarm each push cancels the run before it. That is
deliberate — the latest commit should win — but it means **most human pushes are
also ungated**, just less visibly than the bot's. This was measured, not
isolated: I did not confirm each cancellation was a supersession.

### The mechanism, stated as the hypothesis it is

GitHub does not trigger workflow runs for pushes authenticated with the default
`GITHUB_TOKEN`, to prevent recursive workflow loops. All 35 dated delivery
workflows here push with that token:

```
grep -rnE 'secrets\.[A-Z_]+' .github/workflows/*.yml   # nothing but GITHUB_TOKEN
```

So **there is no control in this repository.** No workflow pushes with a PAT or
deploy key, which means there is no case where the hypothesis predicts a run and
one appears. The 0-of-29 is measured; the token is inferred and has simply never
been contradicted.

### Why this is worse than an untested commit

1. **Ungated source lands on `main`.** Two delivery commits added 948 insertions
   of Rust with no `fmt`, no `clippy`, no forbidden-dependency audit, no closure
   linter, no contract drift radar.
2. **It manufactures misattribution.** The bot commit is never tested. The next
   *human* push triggers CI, which checks out a tree containing the bot's change
   and fails — and the human commit is blamed for a defect it did not introduce.
   Three red-`main` events in one night were `fmt` failures reconstructed this
   way, each costing repeated `git` archaeology.
3. **It fails in the reassuring direction.** An ungated commit produces no red.
   The gate looks green because it never ran, and in a status line that is
   indistinguishable from passing.

### What exists today, and what it does not do

`ci-static.yml` now carries a `schedule:` sweep of `main` HEAD (`c43b49a3f`), so
ungated commits are caught within the interval instead of accumulating
invisibly. Its concurrency group keys scheduled runs on `run_id` rather than
`github.ref`, because otherwise the next push would cancel the sweep — losing
the only coverage those commits get.

That choice turns out to matter more than it looked. With 42% of push runs
cancelled, the sweep is not just cover for bot commits — it is the only CI
Static run in this repo that **cannot** be cancelled by the next push. It is
therefore the coverage floor for human commits too.

**The sweep does not gate a commit before it lands**, and when several land
between sweeps only the newest HEAD is evaluated. The tree is still checked, so
a defect from an earlier unswept commit fails the sweep — but attribution to the
commit that introduced it is not restored. Misattribution is reduced in
frequency, not eliminated.

### If you are adding a delivery workflow

Assume your commits will not be gated on push. Either:

- push with credentials that trigger workflows (a PAT or deploy key, which is a
  repository-secret decision, not a workflow-file one), or
- run the verification your payload needs *inside your own workflow*, before or
  immediately after it pushes, or
- rely on the scheduled sweep and accept that your commit is gated at HEAD
  rather than at authorship.

Do not assume `ci-static.yml`'s `push` trigger covers you. It does not.
