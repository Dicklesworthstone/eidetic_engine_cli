"use client";

import {
  Bot,
  BrainCircuit,
  ExternalLink,
  Hourglass,
  type LucideIcon,
  Network,
  PackageOpen,
  Scale,
  ShieldCheck,
  TrendingUp,
} from "lucide-react";
import { type CSSProperties, useEffect, useRef, useState } from "react";

import { type Chapter, chapters, siteConfig } from "@/lib/content";
import { cn } from "@/lib/utils";

const ICONS: Record<string, LucideIcon> = {
  forget: BrainCircuit,
  memory: Hourglass,
  pack: PackageOpen,
  learn: Scale,
  campaigns: TrendingUp,
  agents: Bot,
  graph: Network,
  proof: ShieldCheck,
};

/**
 * The page's chrome. Wide screens: a floating pill with the chapters, the one in view marked, and a
 * hairline that fills as the page is read. Phones: the name and the code at the top, and the chapters in a
 * dock at the bottom, where a thumb reaches them.
 *
 * On the home page the chapters are anchors and the one in view is marked. On a chapter's own page
 * (`current`), they are links to the other chapters' pages, and the current one stays marked.
 */
export function SiteHeader({ current }: { current?: string } = {}) {
  const [scrolled, setScrolled] = useState(false);
  const [spied, setActive] = useState<string | null>(null);
  const active = current ?? spied;
  const href = (chapter: Chapter) => (current ? `/${chapter.slug}` : `#ch-${chapter.id}`);
  const progress = useRef<HTMLSpanElement>(null);

  // Scrolled state and reading progress: the progress bar is written directly, never through React.
  useEffect(() => {
    let frame = 0;
    const update = () => {
      frame = 0;
      const doc = document.documentElement;
      const max = doc.scrollHeight - window.innerHeight;
      const done = max > 0 ? Math.min(1, Math.max(0, window.scrollY / max)) : 0;
      if (progress.current) progress.current.style.transform = `scaleX(${done})`;
      setScrolled(window.scrollY > 24);
    };
    const onScroll = () => {
      if (frame === 0) frame = requestAnimationFrame(update);
    };
    update();
    window.addEventListener("scroll", onScroll, { passive: true });
    window.addEventListener("resize", onScroll, { passive: true });
    return () => {
      if (frame) cancelAnimationFrame(frame);
      window.removeEventListener("scroll", onScroll);
      window.removeEventListener("resize", onScroll);
    };
  }, []);

  // The chapter crossing the middle of the screen is the one in view.
  useEffect(() => {
    if (typeof IntersectionObserver === "undefined") return;
    const observer = new IntersectionObserver(
      (entries) => {
        for (const entry of entries) {
          if (entry.isIntersecting) setActive(entry.target.getAttribute("data-chapter"));
        }
      },
      { rootMargin: "-45% 0px -54% 0px" },
    );
    const outside = [...document.querySelectorAll("[data-hero], [data-outside]")];
    const watched = [...document.querySelectorAll("[data-chapter]")];
    for (const element of watched) observer.observe(element);
    // Above the first chapter and below the last (the hero, the figures, the short version, the footer), no
    // chapter is in view. One batch can carry several of them, so every entry is read, not only the first.
    const heroObserver = new IntersectionObserver(
      (entries) => {
        if (entries.some((entry) => entry.isIntersecting)) setActive(null);
      },
      { rootMargin: "-45% 0px -54% 0px" },
    );
    for (const element of outside) heroObserver.observe(element);
    return () => {
      observer.disconnect();
      heroObserver.disconnect();
    };
  }, []);

  return (
    <>
      <a
        href="#main"
        className="sr-only z-[70] rounded-xl bg-violet-400 px-4 py-2 text-sm font-bold text-black focus:not-sr-only focus:fixed focus:top-3 focus:left-3"
      >
        Skip to the page
      </a>
      <header className="fixed inset-x-0 top-0 z-50 px-3 pt-3 sm:px-6 sm:pt-4">
        <div
          className={cn(
            "relative mx-auto max-w-7xl overflow-hidden rounded-2xl border px-3 py-2 transition-[background-color,border-color,box-shadow] duration-500 sm:px-4",
            scrolled ? "glass-modern" : "border-transparent bg-transparent",
          )}
        >
          <div className="flex h-10 items-center gap-3">
            <a href={current ? "/" : "#top"} className="flex shrink-0 items-center gap-2.5 rounded-lg">
              <span
                aria-hidden="true"
                className="relative h-6 w-6 rounded-full bg-[radial-gradient(circle_at_35%_35%,#f5d0fe,#a78bfa_42%,#1e1147_78%)] shadow-[0_0_18px_rgba(139,92,246,0.6)]"
              >
                <span className="absolute inset-[7px] rounded-full bg-[#05040d]/85 ring-1 ring-violet-200/40" />
              </span>
              <span className="text-[15px] font-extrabold tracking-tight text-white">
                {siteConfig.name}
                <span className="ml-1.5 hidden font-mono text-[11px] font-medium text-violet-300/80 sm:inline">ee</span>
              </span>
            </a>
            <nav aria-label="Chapters" className="hidden min-w-0 flex-1 justify-center lg:flex">
              <ul className="flex items-center gap-0.5">
                {chapters.map((chapter, index) => {
                  const on = active === chapter.id;
                  return (
                    <li key={chapter.id}>
                      <a
                        href={href(chapter)}
                        aria-current={on ? (current ? "page" : "location") : undefined}
                        style={{ "--sphere": chapter.colour } as CSSProperties}
                        className={cn(
                          "group flex items-center gap-1.5 rounded-xl px-2 py-2 text-[12.5px] font-semibold whitespace-nowrap transition-colors xl:gap-2 xl:px-2.5 xl:text-[13px]",
                          on ? "bg-white/[0.06] text-white" : "text-slate-400 hover:bg-white/[0.04] hover:text-white",
                        )}
                      >
                        <span
                          aria-hidden="true"
                          className={cn(
                            "sphere h-2 w-2 transition-[opacity,transform] duration-300",
                            on ? "scale-125 opacity-100" : "opacity-40 group-hover:opacity-80",
                          )}
                        />
                        <span className="hidden font-mono text-[11px] text-slate-500 2xl:inline">{String(index + 1).padStart(2, "0")}</span>
                        {chapter.label}
                      </a>
                    </li>
                  );
                })}
              </ul>
            </nav>
            <a
              href={siteConfig.github}
              className="ml-auto inline-flex h-10 shrink-0 items-center gap-1.5 rounded-xl bg-violet-400 px-3.5 text-[13px] font-extrabold text-black transition-colors hover:bg-violet-300 lg:ml-0"
            >
              GitHub <ExternalLink className="h-3.5 w-3.5" aria-hidden="true" />
            </a>
          </div>
          {/* Reading progress, as a hairline along the pill's foot. */}
          <span
            aria-hidden="true"
            className={cn(
              "absolute inset-x-4 bottom-0 h-px bg-white/[0.04] transition-opacity duration-500",
              scrolled ? "opacity-100" : "opacity-0",
            )}
          >
            <span
              ref={progress}
              className="block h-full w-full origin-left bg-gradient-to-r from-fuchsia-300/90 via-violet-300/90 to-cyan-300/80"
              style={{ transform: "scaleX(0)" }}
            />
          </span>
        </div>
      </header>

      {/* Phones: the chapters in a dock at the bottom of the screen. */}
      <nav
        aria-label="Chapters"
        className="fixed inset-x-0 bottom-0 z-50 px-3 pb-[max(0.75rem,env(safe-area-inset-bottom))] lg:hidden"
      >
        <ul
          className="glass-modern mx-auto grid max-w-xl rounded-2xl p-1"
          style={{ gridTemplateColumns: `repeat(${chapters.length}, minmax(0, 1fr))` }}
        >
          {chapters.map((chapter) => {
            const Icon = ICONS[chapter.id] ?? BrainCircuit;
            const on = active === chapter.id;
            return (
              <li key={chapter.id}>
                <a
                  href={href(chapter)}
                  aria-current={on ? (current ? "page" : "location") : undefined}
                  style={{ "--sphere": chapter.colour } as CSSProperties}
                  className={cn(
                    "flex min-h-[3.25rem] flex-col items-center justify-center gap-1 rounded-xl text-[10px] font-bold transition-colors",
                    on ? "bg-white/[0.07] text-white" : "text-slate-400 active:bg-white/[0.06]",
                  )}
                >
                  <span className="relative">
                    <Icon className="h-[18px] w-[18px]" aria-hidden="true" />
                    {on ? <span aria-hidden="true" className="sphere absolute -top-1 -right-1.5 h-1.5 w-1.5" /> : null}
                  </span>
                  {chapter.short}
                </a>
              </li>
            );
          })}
        </ul>
      </nav>
    </>
  );
}
