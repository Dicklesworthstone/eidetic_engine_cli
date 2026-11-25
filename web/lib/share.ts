// The share images: the home page's (app/opengraph-image.tsx, app/twitter-image.tsx) and each
// chapter page's (app/[chapter]/…), drawn by lib/og-card.tsx. This module holds what is said about
// them (their paths, type, sizes and alt text) without the renderer, so the Worker and the pages
// can import it cheaply. The static export writes the images without an extension, so worker.ts
// gives them their type.

// Relative, not "@/": worker.ts imports this module, and the Worker's bundler is not Next's.
import { type Chapter, chapters, heroStats, rigor, siteConfig } from "./content";

export const SHARE_IMAGE_TYPE = "image/png";

/** Open Graph's card, and X's, which is 30 pixels shorter. */
export const SHARE_IMAGE_SIZE = {
  openGraph: { width: 1200, height: 630 },
  twitter: { width: 1200, height: 600 },
} as const;

const IMAGES = ["opengraph-image", "twitter-image"] as const;

export const SHARE_IMAGE_PATHS: readonly string[] = [
  ...IMAGES.map((image) => `/${image}`),
  ...chapters.flatMap((chapter) => IMAGES.map((image) => `/${chapter.slug}/${image}`)),
];

/** "home", or a chapter's id. */
export type CardId = "home" | Chapter["id"];

/** The home card's words. A chapter's card takes its words from the chapter's `share`. */
export const HOME_CARD = {
  kicker: "Memory for AI agents · Rust · local-first",
  headline: "Long-term memory for",
  accent: "coding agents.",
  line: "Stores what your agents learn and gives each session a context pack within a token budget.",
  colour: "#a78bfa",
};

/** The alt text for a card: what the card says, since its words are most of what it shows. */
export function shareAlt(id: CardId = "home"): string {
  const chapter = chapters.find((item) => item.id === id);
  if (!chapter) {
    return `${siteConfig.name} (ee): long-term memory for coding agents. ee ${HOME_CARD.line.charAt(0).toLowerCase()}${HOME_CARD.line.slice(1)}`;
  }
  return `${siteConfig.name} (ee), chapter ${chapter.number}: ${chapter.share.headline} ${chapter.share.accent} ${chapter.share.line}`;
}

/** FNV-1a, 32 bits, as hex: enough to tell one card's words from the next version of them. */
function fingerprint(text: string): string {
  let hash = 0x811c9dc5;
  for (let at = 0; at < text.length; at += 1) {
    hash ^= text.charCodeAt(at);
    hash = Math.imul(hash, 0x01000193) >>> 0;
  }
  return hash.toString(16).padStart(8, "0");
}

/**
 * A chapter page's share-image tags, each with that chapter's own alt text.
 *
 * Next.js writes these tags itself from app/[chapter]/opengraph-image.tsx, but with the one `alt` that file
 * exports for every chapter. `generateImageMetadata` would give each its own, and cannot be used here: under
 * a static export its generated route only produces the image id, never the `[chapter]` above it. So the
 * page names its images, which stops Next.js from adding the file's (lib/metadata's mergeStaticMetadata
 * skips them once a page sets `images`). The query changes when the card's words or the release change, so a
 * site that cached the old card fetches the new one. (Next's own query hashes only the route file's source.)
 */
export function chapterShareImages(chapter: Chapter) {
  const alt = shareAlt(chapter.id);
  // Everything the card draws from the content: its words, the rail of chapter numbers and colours, and the
  // counted figures the proof card shows. A change to a drawing in lib/og-art.ts alone does not move it.
  const version = fingerprint(
    JSON.stringify([
      siteConfig.version,
      chapter.slug,
      chapter.share,
      chapters.map((item) => [item.number, item.colour]),
      heroStats.map((stat) => stat.value),
      rigor.map((item) => item.value),
    ]),
  );
  const image = (name: (typeof IMAGES)[number], size: { width: number; height: number }) => ({
    url: `/${chapter.slug}/${name}?v=${version}`,
    ...size,
    type: SHARE_IMAGE_TYPE,
    alt,
  });
  return {
    openGraph: [image("opengraph-image", SHARE_IMAGE_SIZE.openGraph)],
    twitter: [image("twitter-image", SHARE_IMAGE_SIZE.twitter)],
  };
}
