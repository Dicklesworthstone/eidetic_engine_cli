# The Eidetic Engine website

The project's page at <https://eidetic-engine.org>. Next.js 16 builds it as a static export, and a
Cloudflare Worker serves it from its static assets (`wrangler.jsonc`). The Rust crate neither builds nor
reads this directory. `.rchignore` keeps the installed packages and build output out of remote builds;
the tracked source still syncs, so the fleet identifier guard keeps scanning it.

Every word and figure outside the explainers is in `lib/content.ts`. Each figure comes from the
repository at the commit in `COUNTED`, and the page says which commit that was. When the repository
changes, update the page's claims to match.

## Recounting the figures

Run these from the repository root at the commit you are about to name in `COUNTED`. Use `git grep`
against `HEAD` so that untracked files are not counted. Use `[[:space:]]` rather than `\s`, which
POSIX ERE does not define: with `\s` the test count below came out at a third of its true value.

```bash
git grep -h -E '^[[:space:]]*#\[test\][[:space:]]*$' HEAD -- src tests | wc -l   # tests (20,746 -> "20,700+")
git grep -h -c 'proptest! *{' HEAD -- src tests | awk -F: '{s+=$NF} END {print s}'  # property suites (46)
git ls-files 'fuzz/fuzz_targets/*.rs' | wc -l                                     # fuzz targets (27)
git ls-files 'tests/fixtures/failure_modes/*.json' | wc -l                        # failure modes (630)
git ls-files 'docs/schemas/*.json' | wc -l                                        # JSON contracts (280)
git ls-files 'docs/adr/[0-9]*.md' | grep -vc 0000-template                        # ADRs (91)
git grep -c -E '^(pub )?const V[0-9]{3}[A-Z_0-9]*: Migration' HEAD -- src/db/mod.rs  # migrations (133)
git ls-files -z 'src/*.rs' | xargs -0 cat | wc -l                                 # lines of Rust in src (771 files)
git ls-files -z 'tests/*.rs' | xargs -0 cat | wc -l                               # lines of tests
git rev-list --count HEAD                                                         # commits
```

The release dates come from the tags, and the release notes from `CHANGELOG.md`. The limitations come
from the README's own "Limitations" section and the ADRs it cites.

## The page's design

The design follows FrankenLife's page (`~/projects/frankenlife/web`). It borrows the boxed eyebrow and
the reader-driven hero strip from FrankenRobotics.

`chapters` in `lib/content.ts` defines the site's eight chapters: amnesia, memory, the pack, learning,
campaigns, for agents, the graph and proof. Four things come from that list:
- the header's chapter links (a bottom dock below 1024 px);
- each chapter's index, and every section's number ("03.2");
- each chapter's own page, `/<slug>/`, built by `app/[chapter]/page.tsx`;
- each chapter's share card words (`share`).

So adding a section there is enough to number and link it. `components/chapters.tsx` holds every
chapter's sections. The home page renders all eight in order; a chapter page renders one, with links to
its neighbours, and its header links to the other chapter pages instead of to anchors.

`components/section.tsx` has the two section layouts:
- `wide`: an explainer takes the full width under its heading.
- `split`: a sticky heading sits beside lists and prose.

The colours:
- **Accent:** recall violet, `#a78bfa`.
- **Headline words:** a fuchsia-to-cyan gradient.
- **Chapters:** each chapter's accent is `--chapter`, set from `lib/palette.ts`.
- **Memory levels:** each has a colour (working amber, episodic rose, semantic sky, procedural violet).
- **Pack sections:** each has a colour (rules, decisions, failures, evidence, artifacts).

All of these apply wherever a level or section appears.

The page's furniture:
- Corner ticks (`Reticle` in `components/instrument.tsx`) frame anything that is a specimen or a figure.
- Surfaces are `panel`s.
- Blurred glass is kept for the floating header and the dock.
- Lists are ruled rather than boxed.

### The hero

The hero is a toy memory store of 96 memories in six topics. The reader picks a task and a token
budget, and `pack()` in `components/hero.tsx` runs a scaled-down version of ee's selection rule. That
rule is strict MMR (0.75 relevance, 0.25 redundancy) under the balanced profile's section quotas and
the budget. The constellation lights up the candidates, rings the selected memories in rank order, and
draws a beam from each one to the pack card. The memories are invented. The rule and its constants
come from `src/pack/mod.rs` and `src/models/context_profile.rs`.

## The explainers

`components/explain/` shows what ee does rather than only saying it. Each figure wears a badge
(`DemoFrame`'s `basis`, defined in `components/explain/demo.tsx`) that says what it is:

| Badge | Meaning | Figures |
|---|---|---|
| ee's rule, computed here | ee's own rule and constants, recomputed in the browser on hand-sized inputs | `decay.tsx`, `pack-builder.tsx`, `hash-prefix.tsx`, `trust.tsx`, the recall step of `incident-card.tsx`, the risk-class table in `agent-contract.tsx` |
| Derived · arithmetic, not a benchmark | Arithmetic from assumptions the caption states | `scale.tsx` (a notes file, a vector store and ee's pack over a year) |
| From the record | What ee's code, goldens and failure-mode fixtures say it reports | `degradation.tsx`, `agent-contract.tsx` (six pinned or source-built error records) |
| Diagram · not program output | How the software is organised | `loop.tsx`, `pipeline.tsx`, `incident-card.tsx`, `session-loop.tsx`, `scale-lanes.tsx` |
| A toy in your browser · not ee | A small model written for the page; nothing it shows is a measurement | `amnesia.tsx`, `graph-lens.tsx`, `campaign.tsx`, `ledger.tsx`, the session in `incident-card.tsx`, the hero |

The newer figures keep their models apart from their drawings (`scale-model.ts`, `campaign-model.ts`,
`ledger-model.ts`, `agent-contract-data.ts`, `session-loop-data.ts`), and `tests/` checks each model's
invariants. Three of those tests also read the repository: `scale.test.ts`, `session-loop.test.ts`
and `agent-contract.test.ts` check that the constants, JSON keys and error records their figures cite
still read the same in the Rust source, the schemas and the goldens. So a rename in ee turns them red.
The campaign, ledger and scale-ladder tests check their models only.

Each figure's caption names the source files and constants it follows. Animations stop when off
screen, and never run for a reader who asks for reduced motion.

## The share cards

Every page has its own Open Graph card (1200 × 630) and X card (1200 × 600): the home page's in
`app/opengraph-image.tsx` and `app/twitter-image.tsx`, and each chapter's in `app/[chapter]/`. All of
them come from `shareCard()` in `lib/og-card.tsx`. Each card has the chapter's colour, its headline,
its line and a rail of the eight chapters, next to a drawing of what the chapter is about:
- the notes file against the pack (amnesia);
- the five half-lives (memory);
- candidates beamed into a budgeted pack (the pack);
- moving posteriors and the trust ladder (learning);
- a search space pruned family by family (campaigns);
- an `ee.error.v2` envelope and the exit codes (for agents);
- communities, a hub and an articulation point (graph);
- the counted figures (proof).

next/og draws with Satori, not a browser. So the drawings in `lib/og-art.ts` are SVG strings embedded
as images, which resvg draws in full, and their words are laid over them as text, because resvg has
no fonts there. Two Satori rules bit here: a fragment held in a variable is dropped without an error,
and a span's trailing space is trimmed.

To look at the cards without a full build, render them with a throwaway Bun script that calls
`shareCard(1200, 630, id)` and writes the response's bytes to a PNG.

## The stylesheet and the fleet guard

`tests/fleet_identifier_guard.rs` scans every tracked path and file for a list of banned words. One of
them is the stylesheet language's three-letter extension, written in lower case. So no tracked file
here may have that extension, or contain that word.

The Tailwind source is therefore `styles/site.tailwind`. `scripts/build-style.ts` compiles it with the
Tailwind CLI into the git-ignored `.generated/` directory, and writes `.generated/style-import.ts`, a
one-line module that imports the result. `app/layout.tsx` imports that module, so Next serves the
stylesheet as one hashed, cached file.

`scripts/check-tokens.ts` runs first in `bun run build`. It reads the guard's word lists out of the
Rust test, and fails if any file under `web/` would trip the guard. That matters because the guard
itself is a cargo test, and cargo test cannot run on a developer's Mac here.

## Commands

Bun only:

```bash
cd web
bun install
bun run dev          # http://localhost:3000, with the stylesheet rebuilt on change
bun run typecheck
bun test             # the Worker's routing, the chapters, and every explainer's model and citations
bun run build        # the token check, the stylesheet, then the static export into out/
bun run preview      # build, then serve out/ through the Worker locally (wrangler dev)
bun run deploy       # build, then `wrangler deploy` (needs `wrangler login`)
```

## Deployment

`wrangler.jsonc` deploys a Worker named `eidetic-engine`, with:
- `out/` as its static assets;
- `run_worker_first`, so `worker.ts` sees every request;
- custom domains for `eidetic-engine.org` and `www.eidetic-engine.org`.

Wrangler creates the DNS records and certificates for both domains itself. The zone has to be on the
Cloudflare account you are logged in to. Any existing A, AAAA or CNAME record on either name must be
removed first, or the custom domain will not attach.

`worker.ts` does three things:
- It redirects `www` to the apex with a 301, keeping the path and query.
- It serves the share images (`SHARE_IMAGE_PATHS` in `lib/share.ts`: the home page's two and each
  chapter page's two) as PNG. The static export writes them without an extension.
- It hands everything else to the static assets unchanged.

The static export writes each chapter page as `<slug>.html`, and the assets' `auto-trailing-slash`
handling serves that file at `/<slug>` and redirects `/<slug>/` to it. So every link, canonical URL
and sitemap entry for a chapter page is written without a trailing slash.

`public/_headers` caches `/_next/static/*` immutably, and sets `nosniff` and a referrer policy on every
response.
