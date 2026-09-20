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

**Re-measured 2026-09-20 per commit, and it is worse than the line above says.**
Over the last 24h of `origin/main`: 41 delivery-lane commits, and **0 of 41
received a push-triggered workflow run of ANY kind** — not CI Static, and not
any of the path-filtered workflows either. 0 unclassifiable; the run-fetch
window (2026-09-17 → 2026-09-20) covers every commit classified, and the same
join over 12 human commits returned 11 with one run and 1 with five, so the
zeros are the repo's answer and not the query's.

One commit carries a single run at `event=workflow_run` — a chained trigger that
inherits another run's head sha, not a gate on the push.

**It is not a paths problem.** 30 of those 41 commits touch `src/` and the lane
wrote 103 `src` files in that window. Path-filtered `src` workflows would have
fired. Nothing fired, uniformly, whatever the commit touched — which is what a
credential that does not trigger workflows predicts, and what a paths mismatch
does not.

**Do not quote the 90%.** It counts a run that *existed*, and 42% of runs in
this repo are cancelled. A cancelled run checked nothing, so it is coverage in
a status line and not in fact. The honest human number is **33%**.

The likely mechanism for the cancellations is this workflow's own concurrency:
push/PR runs share a `github.ref`-keyed group with `cancel-in-progress: true`,
so under a multi-agent swarm each push cancels the run before it. That is
deliberate — the latest commit should win — but it means **most human pushes are
also ungated**, just less visibly than the bot's. This was measured, not
isolated: I did not confirm each cancellation was a supersession.

### The mechanism: OBSERVED 2026-09-20, no longer a hypothesis

GitHub does not trigger workflow runs for pushes authenticated with the default
`GITHUB_TOKEN`, to prevent recursive workflow loops. The delivery workflows here
push with exactly that token, and the step is citable:

```yaml
# resume-coherence-20260919.yml
37:  - uses: actions/checkout@11d5960a...
38:    with:
39:      ref: main
40:      fetch-depth: 0        # no `token:`, no `persist-credentials:`
...
98:      git push origin HEAD:main
```

`actions/checkout` defaults `persist-credentials` to **true**, so it writes the
job's default `GITHUB_TOKEN` into `.git/config` as the auth header for `origin`,
and the plain push at `:98` uses it.

**Why this is uniform and not one anecdote.** Every credential *spelling* was
enumerated across all 49 workflow files — `secrets.*`, `github.token`,
`GITHUB_TOKEN`, `ssh-key`, `deploy-key`, `app-id`, `private-key`:

```
secrets.GITHUB_TOKEN        1   release.yml:862
secrets.HOMEBREW_TAP_TOKEN  1   release.yml:981
github.token                2   ci.yml — GH_TOKEN for the gh CLI, not a push
GITHUB_TOKEN                6   the two above, plus ci-static.yml COMMENTS
```

Both real secrets live in `release.yml`, which is tag-triggered and is not a
delivery workflow. No ssh-key, deploy-key, app-id or private-key exists anywhere.
So **there is no non-default credential in this repository** for a delivery
workflow to push with — whatever a checkout persists can only be the default
token, because nothing else exists to persist. That argument does not depend on
matching checkout steps to jobs, which is where a file-level census gets it
wrong: a `persist-credentials: false` in a delivery workflow generally belongs to
a *later verification job* pinned to a source sha, not to the job that pushes.

There is still **no positive control** — no workflow pushes with a PAT, so no
case exists where the mechanism predicts a run and one appears. What changed is
that the leading alternative is now measured false (it is not paths) and the
credential is directly read rather than assumed.

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
