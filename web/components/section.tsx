import type { CSSProperties, ReactNode } from "react";

import { MicroLabel, RecallTitle } from "@/components/instrument";
import { Reveal } from "@/components/reveal";
import type { Chapter } from "@/lib/content";
import { cn } from "@/lib/utils";

/**
 * A chapter: its opener (an outlined numeral, the chapter's sphere, its title
 * and thesis, and its sections as an index), then its sections. The wrapper
 * carries the chapter's colour as `--chapter` and `data-chapter` for the
 * header's scroll spy.
 */
export function ChapterBlock({ chapter, children }: { chapter: Chapter; children: ReactNode }) {
  const style = { "--chapter": chapter.colour } as CSSProperties;
  const headingId = `ch-${chapter.id}-heading`;
  return (
    <div data-chapter={chapter.id} style={style} className="relative">
      <section aria-labelledby={headingId} className="relative overflow-hidden pt-24 md:pt-32 lg:pt-40">
        {/* The membrane: a hairline across the page, lit at the chapter's sphere. */}
        <div aria-hidden="true" className="absolute inset-x-0 top-0 h-px bg-gradient-to-r from-transparent via-[color:var(--chapter)]/40 to-transparent" />
        <div
          aria-hidden="true"
          className="pointer-events-none absolute -top-40 left-1/2 h-80 w-[60rem] -translate-x-1/2 rounded-[100%] opacity-[0.10] blur-3xl"
          style={{ background: "radial-gradient(closest-side, var(--chapter), transparent)" }}
        />
        {/* The anchor sits on the words, so a jump to the chapter lands on its title, not on the space above. */}
        <div id={`ch-${chapter.id}`} className="relative mx-auto max-w-7xl scroll-mt-4 px-4 pb-6 sm:px-6">
          <Reveal className="grid grid-cols-1 items-end gap-8 lg:grid-cols-12 lg:gap-12">
            <div className="lg:col-span-8">
              <div className="flex items-center gap-4">
                <span aria-hidden="true" className="sphere h-3.5 w-3.5" />
                <span className="font-mono text-xs font-medium tracking-[0.25em] text-[color:var(--chapter)] uppercase">
                  Chapter {chapter.number} · {chapter.label}
                </span>
              </div>
              <h2 id={headingId} className="display-2 mt-6 text-white">
                <RecallTitle text={chapter.title} />
              </h2>
              <p className="lede mt-6 max-w-3xl">{chapter.thesis}</p>
            </div>
            <div className="relative hidden lg:col-span-4 lg:block">
              <span aria-hidden="true" className="chapter-numeral absolute -top-44 right-0 text-[13rem] select-none">
                {chapter.number}
              </span>
              <ol className="relative space-y-1 border-l border-white/[0.08] pl-5">
                {chapter.sections.map((section, index) => (
                  <li key={section.id}>
                    <a
                      href={`#${section.id}`}
                      className="group flex items-baseline gap-3 rounded-lg py-1.5 text-sm text-slate-400 transition-colors hover:text-white"
                    >
                      <span className="font-mono text-[11px] text-slate-500 tabular-nums group-hover:text-[color:var(--chapter)]">
                        {chapter.number}.{index + 1}
                      </span>
                      {section.label}
                    </a>
                  </li>
                ))}
              </ol>
            </div>
          </Reveal>
        </div>
      </section>
      {children}
    </div>
  );
}

/**
 * A section of a chapter. `wide` puts the heading above a full-width figure (the explainers);
 * `split` keeps a sticky heading beside its content on wide screens (lists and prose).
 */
export function Section({
  id,
  index,
  eyebrow,
  title,
  kicker,
  children,
  className,
  layout = "split",
}: {
  id?: string;
  /** The section's number within its chapter: "01.2". */
  index?: string;
  eyebrow: string;
  title: string;
  kicker?: ReactNode;
  children: ReactNode;
  className?: string;
  layout?: "split" | "wide";
}) {
  const headingId = id ? `${id}-heading` : undefined;
  const label = (
    <div className="flex items-center gap-3">
      {index ? (
        <span className="font-mono text-[11px] font-medium text-[color:var(--chapter)] tabular-nums">{index}</span>
      ) : null}
      <span aria-hidden="true" className="h-px w-6 bg-[color:var(--chapter)]/50" />
      <MicroLabel className="text-slate-400">{eyebrow}</MicroLabel>
    </div>
  );

  if (layout === "wide") {
    return (
      <section
        id={id}
        data-skip
        aria-labelledby={headingId}
        className={cn("relative mx-auto max-w-7xl px-4 py-14 sm:px-6 md:py-20 lg:py-24", className)}
      >
        <Reveal className="grid grid-cols-1 items-end gap-5 lg:grid-cols-12 lg:gap-12">
          <div className="space-y-5 lg:col-span-7">
            {label}
            <h3 id={headingId} className="display-3 text-white">
              <RecallTitle text={title} />
            </h3>
          </div>
          {kicker ? <div className="text-base leading-relaxed text-slate-400 lg:col-span-5 lg:pb-1.5">{kicker}</div> : null}
        </Reveal>
        <div className="mt-9 md:mt-12">{children}</div>
      </section>
    );
  }

  return (
    <section
      id={id}
      data-skip
      aria-labelledby={headingId}
      className={cn("relative mx-auto max-w-7xl px-4 py-14 sm:px-6 md:py-20 lg:py-24", className)}
    >
      <div className="grid grid-cols-1 items-start gap-9 lg:grid-cols-12 lg:gap-16">
        <Reveal className="space-y-5 lg:sticky lg:top-28 lg:col-span-4">
          {label}
          <h3 id={headingId} className="display-3 text-white">
            <RecallTitle text={title} />
          </h3>
          {kicker ? <div className="max-w-md text-base leading-relaxed text-slate-400">{kicker}</div> : null}
        </Reveal>
        <div className="min-w-0 lg:col-span-8">{children}</div>
      </div>
    </section>
  );
}
