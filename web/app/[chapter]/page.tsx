// Each chapter on a page of its own, /<slug>, so one chapter can be sent on with its own
// share images (opengraph-image.tsx and twitter-image.tsx beside this file).

import { ArrowLeft, ArrowRight } from "lucide-react";
import type { Metadata } from "next";
import { notFound } from "next/navigation";
import type { CSSProperties } from "react";

import { ChapterContent } from "@/components/chapters";
import { RecallTitle } from "@/components/instrument";
import { RevealController } from "@/components/reveal-controller";
import { SiteFooter } from "@/components/site-footer";
import { SiteHeader } from "@/components/site-header";
import { SkipOffscreen } from "@/components/skip-offscreen";
import { type Chapter, chapters, siteConfig } from "@/lib/content";

export const dynamicParams = false;

export function generateStaticParams() {
  return chapters.map((chapter) => ({ chapter: chapter.slug }));
}

type Props = { params: Promise<{ chapter: string }> };

function bySlug(slug: string): Chapter | undefined {
  return chapters.find((chapter) => chapter.slug === slug);
}

export async function generateMetadata({ params }: Props): Promise<Metadata> {
  const chapter = bySlug((await params).chapter);
  if (!chapter) return {};
  const title = `${chapter.share.title} · ${siteConfig.name}`;
  const url = `/${chapter.slug}`;
  // The images are this route's opengraph-image.tsx and twitter-image.tsx, which Next.js links
  // itself; naming images here would override them.
  return {
    title,
    description: chapter.share.description,
    alternates: { canonical: url },
    openGraph: { title, description: chapter.share.description, url, siteName: siteConfig.name, type: "article" },
    twitter: { card: "summary_large_image", title, description: chapter.share.description },
  };
}

export default async function ChapterPage({ params }: Props) {
  const chapter = bySlug((await params).chapter);
  if (!chapter) notFound();
  const at = chapters.indexOf(chapter);
  const previous = chapters[at - 1];
  const next = chapters[at + 1];
  return (
    <>
      <SiteHeader current={chapter.id} />
      <RevealController />
      <SkipOffscreen />
      <main id="main" className="relative isolate">
        <span id="top" aria-hidden="true" className="absolute top-0" />
        <ChapterContent id={chapter.id} mode="page" />

        <nav aria-label="Other chapters" className="mx-auto max-w-7xl px-4 pt-6 pb-24 sm:px-6">
          <div className="grid grid-cols-1 gap-3 md:grid-cols-2">
            {previous ? <Neighbour chapter={previous} direction="previous" /> : <span className="hidden md:block" />}
            {next ? <Neighbour chapter={next} direction="next" /> : <Neighbour chapter={chapters[0] as Chapter} direction="next" />}
          </div>
          <ol className="mt-8 flex flex-wrap gap-2">
            {chapters.map((item) => (
              <li key={item.id}>
                <a
                  href={`/${item.slug}`}
                  aria-current={item.id === chapter.id ? "page" : undefined}
                  style={{ "--sphere": item.colour } as CSSProperties}
                  className="flex items-center gap-2 rounded-full border border-white/[0.08] px-3 py-1.5 font-mono text-[11px] text-slate-400 transition-colors hover:border-white/20 hover:text-white aria-[current=page]:border-white/25 aria-[current=page]:text-white"
                >
                  <span aria-hidden="true" className="sphere h-2 w-2" />
                  {item.number} · {item.label}
                </a>
              </li>
            ))}
          </ol>
        </nav>
      </main>
      {/* On /proof the install section is on this page: an in-page anchor, which the first-view script in app/layout.tsx recognises. */}
      <SiteFooter installHref={chapter.id === "proof" ? "#install" : "/proof#install"} />
    </>
  );
}

function Neighbour({ chapter, direction }: { chapter: Chapter; direction: "previous" | "next" }) {
  const Arrow = direction === "previous" ? ArrowLeft : ArrowRight;
  return (
    <a
      href={`/${chapter.slug}`}
      style={{ "--chapter": chapter.colour, "--sphere": chapter.colour } as CSSProperties}
      className="panel lift group relative block overflow-hidden rounded-2xl p-6"
    >
      <span
        aria-hidden="true"
        className="pointer-events-none absolute -top-16 -right-16 h-44 w-44 rounded-full opacity-20 blur-3xl transition-opacity group-hover:opacity-40"
        style={{ background: "var(--chapter)" }}
      />
      <span className="flex items-center gap-2 font-mono text-[11px] tracking-[0.18em] text-[color:var(--chapter)] uppercase">
        {direction === "previous" ? <Arrow className="h-3.5 w-3.5" aria-hidden="true" /> : null}
        {direction === "previous" ? "Previous" : "Next"} · Chapter {chapter.number}
        {direction === "next" ? <Arrow className="h-3.5 w-3.5" aria-hidden="true" /> : null}
      </span>
      <span className="mt-3 block text-2xl font-extrabold tracking-[-0.03em] text-white">
        <RecallTitle text={chapter.title} />
      </span>
      <span className="mt-2 block text-[14px] leading-relaxed text-slate-400">{chapter.share.line}</span>
    </a>
  );
}
