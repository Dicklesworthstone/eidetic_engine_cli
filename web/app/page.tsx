import { ArrowDown } from "lucide-react";
import type { CSSProperties } from "react";

import { ChapterContent } from "@/components/chapters";
import { Hero } from "@/components/hero";
import { Cmd, RecallTitle } from "@/components/instrument";
import { RevealController } from "@/components/reveal-controller";
import { SiteFooter } from "@/components/site-footer";
import { SiteHeader } from "@/components/site-header";
import { SkipOffscreen } from "@/components/skip-offscreen";
import { COUNTED, chapters, heroStats } from "@/lib/content";
import { cn } from "@/lib/utils";

const HERO_FACTS = ["one Rust binary", "no cloud or API key", "local store", "versioned JSON"];

/** The short version, as a loop: where lessons come from, where they are kept, and what a task gets back. */
const FLOW = [
  {
    step: "Record",
    command: "ee remember · ee outcome",
    note: "Agents add lessons as they work and say which ones helped. Recorded sessions can be imported.",
  },
  {
    step: "Keep",
    command: ".ee/ee.db",
    note: "A local database in the workspace, beside a user-global store under ~/.local/share/ee.",
  },
  {
    step: "Pack",
    command: 'ee pack "<task>"',
    note: "The memories that match the task, cut to a token budget, each with its source, score and trust level.",
  },
];

/** A notes file loaded into every session, against ee, on the four points the summary turns on. */
const VERSUS = [
  { point: "What a session loads", notes: "the whole file", ee: "only what bears on the task" },
  { point: "As lessons pile up", notes: "grows, and costs more", ee: "stays inside your budget" },
  { point: "A line that stopped helping", notes: "nothing marks it", ee: "the decay pass demotes, then retires it" },
  { point: "Each line says", notes: "nothing about itself", ee: "its source and score" },
];

const HARNESSES = ["Claude Code", "Codex", "shell scripts", "MCP, in an mcp-feature build"];

function SummaryFigure() {
  return (
    <div className="mt-7 space-y-6">
      <ol aria-label="How ee works" className="space-y-3">
        {FLOW.map((item, at) => (
          <li key={item.step} className="relative flex gap-4">
            {/* The connector runs from this step's number to the next one's, and stops at the last. */}
            {at < FLOW.length - 1 ? (
              <span aria-hidden="true" className="absolute top-8 -bottom-4 left-[15px] w-px bg-violet-400/40" />
            ) : null}
            <span className="relative z-10 mt-1 flex h-[31px] w-[31px] shrink-0 items-center justify-center rounded-full border border-violet-300/40 bg-[#0d0a1a] font-mono text-[12px] font-bold text-violet-200">
              {at + 1}
            </span>
            <div className="panel min-w-0 flex-1 rounded-xl px-4 py-3">
              <p className="flex flex-wrap items-baseline gap-x-3 gap-y-1">
                <span className="font-bold text-white">{item.step}</span>
                <Cmd>{item.command}</Cmd>
              </p>
              <p className="mt-1.5 text-[13.5px] leading-snug text-slate-400">{item.note}</p>
            </div>
          </li>
        ))}
      </ol>
      <div className="panel overflow-hidden rounded-xl">
        <table className="w-full text-left text-[13px]">
          <caption className="sr-only">A notes file such as CLAUDE.md, compared with ee</caption>
          <thead>
            <tr className="border-b border-white/[0.06] font-mono text-[10.5px] tracking-[0.14em] uppercase">
              <th scope="col" className="px-4 py-2.5 font-medium text-slate-500">
                <span className="sr-only">Point</span>
              </th>
              <th scope="col" className="px-3 py-2.5 font-medium text-amber-300/80">
                CLAUDE.md
              </th>
              <th scope="col" className="px-3 py-2.5 font-medium text-violet-300">
                ee
              </th>
            </tr>
          </thead>
          <tbody>
            {VERSUS.map((row) => (
              <tr key={row.point} className="border-b border-white/[0.04] last:border-0">
                <th scope="row" className="px-4 py-2.5 align-top font-normal text-slate-500">
                  {row.point}
                </th>
                <td className="px-3 py-2.5 align-top text-slate-400">
                  <span aria-hidden="true" className="mr-1.5 text-rose-300/80">
                    ×
                  </span>
                  {row.notes}
                </td>
                <td className="px-3 py-2.5 align-top text-slate-200">
                  <span aria-hidden="true" className="mr-1.5 text-emerald-300">
                    ✓
                  </span>
                  {row.ee}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
      <div className="flex flex-wrap items-center gap-2 text-[12.5px] text-slate-500">
        <span>The same JSON contracts for</span>
        {HARNESSES.map((name) => (
          <span key={name} className="rounded-full border border-white/10 bg-white/[0.03] px-2.5 py-0.5 text-slate-300">
            {name}
          </span>
        ))}
      </div>
    </div>
  );
}

function HeroWords() {
  return (
    <>
      <p className="inline-flex items-center gap-3 rounded-full border border-violet-300/20 bg-violet-400/[0.06] py-1.5 pr-4 pl-2">
        <span aria-hidden="true" className="sphere h-3 w-3 [--sphere:#a78bfa]" />
        <span className="font-mono text-[10.5px] font-medium tracking-[0.22em] text-violet-200/90 uppercase sm:text-[11px]">
          Memory for AI agents · Rust · local-first
        </span>
      </p>
      <h1 id="hero-heading" className="mt-7 text-[clamp(3rem,5.4vw,5.7rem)] leading-[0.94] tracking-[-0.05em] text-white">
        Long-term memory for
        <br />
        <RecallTitle lit text="[coding agents.]" />
      </h1>
      <p className="lede mt-7 max-w-[37rem]">
        Eidetic Engine (ee) records what your agents learn as they work. At the start of each task it hands the agent the
        part of that record that bears on it, cut to a token budget, with the source of every line.
      </p>
      <ul aria-label="At a glance" className="mt-6 flex max-w-[37rem] flex-wrap gap-2">
        {HERO_FACTS.map((fact) => (
          <li
            key={fact}
            className="rounded-full border border-white/10 bg-white/[0.03] px-3 py-1 font-mono text-[11.5px] text-slate-300"
          >
            {fact}
          </li>
        ))}
      </ul>
      <div className="mt-9 flex flex-wrap gap-3">
        <a
          href="#builder"
          className="inline-flex min-h-12 items-center gap-2 rounded-2xl bg-violet-400 px-5 text-[15px] font-extrabold text-black shadow-[0_0_40px_-10px_rgba(139,92,246,0.9)] transition-colors hover:bg-violet-300"
        >
          Build a pack yourself <ArrowDown className="h-4 w-4" aria-hidden="true" />
        </a>
        <a
          href="#scale"
          className="inline-flex min-h-12 items-center gap-2 rounded-2xl border border-white/15 bg-white/[0.02] px-5 text-[15px] font-bold text-white transition-colors hover:border-violet-400/50 hover:text-violet-200"
        >
          Why not just CLAUDE.md?
        </a>
        <a
          href="#install"
          className="inline-flex min-h-12 items-center gap-2 rounded-2xl border border-white/15 bg-white/[0.02] px-5 text-[15px] font-bold text-white transition-colors hover:border-violet-400/50 hover:text-violet-200"
        >
          Install
        </a>
      </div>
    </>
  );
}

export default function Home() {
  return (
    <>
      <SiteHeader />
      <RevealController />
      <SkipOffscreen />
      <main id="main" className="relative isolate">
        <span id="top" aria-hidden="true" className="absolute top-0" />

        {/* Hero: the specimen and the words, then the strip that drives the specimen. */}
        <section data-hero aria-labelledby="hero-heading" className="relative isolate overflow-hidden">
          <div aria-hidden="true" className="field-light absolute inset-0 -z-10" />
          <div aria-hidden="true" className="lattice absolute inset-0 -z-10" />
          <Hero words={<HeroWords />} />
        </section>

        {/* The specification strip: four facts, each with its own qualification. It and the map below are
            `data-outside`, so the header marks no chapter while they are in view. */}
        <section data-outside aria-label="In four figures" className="relative border-y border-white/[0.06] bg-black/20">
          <dl className="mx-auto grid max-w-7xl grid-cols-2 lg:grid-cols-4">
            {heroStats.map((stat, at) => (
              <div
                key={stat.label}
                className={cn(
                  "min-w-0 border-white/[0.06] px-5 py-7 sm:px-7 lg:py-9",
                  at % 2 === 1 && "border-l",
                  at >= 2 && "border-t lg:border-t-0",
                  at === 2 && "lg:border-l",
                )}
              >
                <dt className="font-mono text-[10px] font-medium tracking-[0.18em] text-violet-300/80 uppercase">{stat.label}</dt>
                <dd className="mt-2">
                  <span className="block text-[1.7rem] font-extrabold tracking-[-0.035em] text-white tabular-nums sm:text-[2.1rem]">
                    {stat.value}
                  </span>
                  <span className="mt-2 block text-[13px] leading-snug text-slate-400">{stat.note}</span>
                </dd>
              </div>
            ))}
          </dl>
          <p className="mx-auto max-w-7xl border-t border-white/[0.06] px-5 py-2.5 font-mono text-[10.5px] text-slate-500 sm:px-7">
            Counted in the repository at {COUNTED.commit}, {COUNTED.date}.
          </p>
        </section>

        {/* The short version: the problem, what ee does, and what each chapter's part of it gives an agent. Each
            tile is a jump to its chapter. `data-outside`, like the strip, so the header marks no chapter here. */}
        <section
          id="summary"
          data-outside
          aria-labelledby="summary-heading"
          className="relative mx-auto max-w-7xl px-4 pt-16 sm:px-6 md:pt-24"
        >
          <div className="grid grid-cols-1 gap-10 lg:grid-cols-12 lg:gap-14">
            <div className="lg:sticky lg:top-28 lg:col-span-5 lg:self-start">
              <p className="font-mono text-[11px] tracking-[0.22em] text-slate-500 uppercase">The short version</p>
              <h2 id="summary-heading" className="display-3 mt-4 text-white">
                <RecallTitle text="What ee does for an [agent]." />
              </h2>
              <p className="mt-6 text-[15.5px] leading-relaxed text-slate-300">
                A coding agent starts every session with an empty context window, so whatever earlier sessions learned is
                gone unless someone wrote it down.
              </p>
              <SummaryFigure />
            </div>
            <nav aria-label="The eight chapters" className="lg:col-span-7">
              <p className="font-mono text-[11px] tracking-[0.22em] text-slate-500 uppercase">What you get, chapter by chapter</p>
              <ol className="mt-4 grid grid-cols-1 gap-3 sm:grid-cols-2">
                {chapters.map((chapter) => (
                  <li key={chapter.id} style={{ "--chapter": chapter.colour, "--sphere": chapter.colour } as CSSProperties}>
                    <a
                      href={`#ch-${chapter.id}`}
                      className="panel lift group relative flex h-full flex-col overflow-hidden rounded-2xl p-5"
                    >
                      <span
                        aria-hidden="true"
                        className="pointer-events-none absolute -top-14 -right-14 h-36 w-36 rounded-full opacity-[0.16] blur-2xl transition-opacity duration-500 group-hover:opacity-40"
                        style={{ background: "var(--chapter)" }}
                      />
                      <span className="flex items-center gap-2.5">
                        <span aria-hidden="true" className="sphere h-2.5 w-2.5" />
                        <span className="font-mono text-[11px] tracking-[0.18em] text-[color:var(--chapter)] uppercase">
                          {chapter.number} · {chapter.label}
                        </span>
                      </span>
                      <span className="mt-3 block text-[1.08rem] leading-snug font-bold tracking-[-0.015em] text-white">
                        {chapter.point.claim}
                      </span>
                      <span className="mt-2 block text-[13.5px] leading-relaxed text-slate-400">{chapter.point.detail}</span>
                    </a>
                  </li>
                ))}
              </ol>
            </nav>
          </div>
        </section>

        {chapters.map((chapter) => (
          <ChapterContent key={chapter.id} id={chapter.id} />
        ))}
      </main>

      <SiteFooter />
    </>
  );
}
