import { ArrowDown } from "lucide-react";
import type { CSSProperties } from "react";

import { ChapterContent } from "@/components/chapters";
import { Hero } from "@/components/hero";
import { Cmd, RecallTitle } from "@/components/instrument";
import { RevealController } from "@/components/reveal-controller";
import { SiteFooter } from "@/components/site-footer";
import { SiteHeader } from "@/components/site-header";
import { SkipOffscreen } from "@/components/skip-offscreen";
import { COUNTED, chapters, heroStats, siteConfig } from "@/lib/content";
import { cn } from "@/lib/utils";

const link = "text-violet-300 underline decoration-violet-300/30 underline-offset-4 transition-colors hover:decoration-violet-300";

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
        Eidetic Engine (ee) records what your agents learn as they work: rules, decisions, failed attempts and the fixes
        that worked. At the start of each task it gives the agent the part of that record that bears on the task, cut to
        a token budget, with the source of every line and the reason it was chosen. It is one Rust binary and needs no
        cloud service or API key.
      </p>
      <p className="mt-4 max-w-[37rem] text-[15px] leading-relaxed text-slate-400">
        <span className="font-semibold text-slate-200">New in {siteConfig.version}:</span> fixes are recalled by error
        fingerprint in Go, Vitest and Jest output as well as rustc, and the optional warm daemon returns the same pack,
        with the same hash, in 708 ms where a cold run took 3,155 ms on one test machine.{" "}
        <a className={link} href={siteConfig.changelog}>
          What changed
        </a>
        .
      </p>
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
      <p className="mt-6 text-sm text-slate-500">
        Open source on{" "}
        <a className={link} href={siteConfig.github}>
          GitHub
        </a>
        . Every figure on this site names where it comes from.
      </p>
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
              <div className="mt-6 space-y-4 text-[15.5px] leading-relaxed text-slate-300">
                <p>
                  A coding agent starts each session with an empty context window. Whatever earlier sessions worked out,
                  such as how the project builds, which approaches failed and why a decision went the way it did, is gone
                  unless someone wrote it down. The usual answer is a notes file like CLAUDE.md or AGENTS.md that the
                  harness loads into every session. That helps at first, but the file grows with every lesson, nothing in
                  it marks a line as out of date, and every line costs tokens in every session whether the task needs it
                  or not.
                </p>
                <p>
                  ee replaces that file with a local database. Agents add to it as they work, with{" "}
                  <Cmd className="whitespace-nowrap">ee remember</Cmd> and <Cmd className="whitespace-nowrap">ee outcome</Cmd>, and ee can import the sessions they have already
                  recorded. When a task starts, <Cmd className="whitespace-nowrap">ee pack</Cmd> returns the memories that match it, fitted to a token
                  budget, with each item&apos;s source, score and trust level. However large the store grows, the pack
                  stays within the budget you set.
                </p>
                <p>
                  It is a single Rust binary that keeps its data in the repository&apos;s <Cmd>.ee</Cmd> directory and
                  needs no cloud service or API key. Claude Code, Codex, MCP clients and shell scripts call the same
                  commands and get the same JSON.
                </p>
              </div>
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
