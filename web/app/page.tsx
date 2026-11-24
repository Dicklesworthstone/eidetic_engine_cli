import { ArrowDown } from "lucide-react";
import type { CSSProperties } from "react";

import { ChapterContent } from "@/components/chapters";
import { Hero } from "@/components/hero";
import { RecallTitle } from "@/components/instrument";
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
        Your agents forget.
        <br />
        <RecallTitle lit text="[ee remembers.]" />
      </h1>
      <p className="lede mt-7 max-w-[37rem]">
        Eidetic Engine is a durable memory for AI agents. It keeps the rules, decisions, failures and evidence your
        agents learn, and hands every new session a compact context pack: fitted to a token budget, free of
        near-duplicates, and with every line saying where it came from and why it is there. Built for campaigns that run
        for weeks, not chats that end tonight. One Rust binary. No cloud, no paid API, no daemon required.
      </p>
      <p className="mt-4 max-w-[37rem] text-[15px] leading-relaxed text-slate-400">
        <span className="font-semibold text-slate-200">New in {siteConfig.version}:</span> a fix your agents found once
        is recalled by the error&apos;s fingerprint, across rustc, Go, Vitest and Jest output, and a warm daemon serves
        the same pack, hash for hash, in a fraction of the time.{" "}
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
          Install in a minute
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

        {/* The specification strip: four facts, each with its own qualification. */}
        <section aria-label="In four figures" className="relative border-y border-white/[0.06] bg-black/20">
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

        {/* The story in eight chapters: a map of the page, each tile also a way to its own page. */}
        <nav aria-label="The eight chapters" className="relative mx-auto max-w-7xl px-4 pt-16 sm:px-6 md:pt-20">
          <p className="font-mono text-[11px] tracking-[0.22em] text-slate-500 uppercase">The story in eight chapters</p>
          <ol className="mt-5 grid grid-cols-1 gap-3 sm:grid-cols-2 lg:grid-cols-4">
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
                  <span className="mt-3 block text-[1.35rem] leading-tight font-extrabold tracking-[-0.03em] text-white">
                    <RecallTitle text={chapter.title} />
                  </span>
                  <span className="mt-2 block text-[13.5px] leading-relaxed text-slate-400">{chapter.share.line}</span>
                </a>
              </li>
            ))}
          </ol>
        </nav>

        {chapters.map((chapter) => (
          <ChapterContent key={chapter.id} id={chapter.id} />
        ))}
      </main>

      <SiteFooter />
    </>
  );
}
