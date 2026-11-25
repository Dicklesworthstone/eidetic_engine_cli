// Each chapter page's X (Twitter) share image, made at build time (lib/og-card.tsx). The page's own metadata
// names it, with the chapter's alt text (chapterShareImages in lib/share.ts), so this file exports no `alt`.
import { chapters } from "@/lib/content";
import { shareCard } from "@/lib/og-card";
import { SHARE_IMAGE_SIZE, SHARE_IMAGE_TYPE } from "@/lib/share";

export const dynamic = "force-static";
export const dynamicParams = false;
export const size = SHARE_IMAGE_SIZE.twitter;
export const contentType = SHARE_IMAGE_TYPE;

export function generateStaticParams() {
  return chapters.map((chapter) => ({ chapter: chapter.slug }));
}

export default async function Image({ params }: { params: Promise<{ chapter: string }> }) {
  const { chapter: slug } = await params;
  const chapter = chapters.find((item) => item.slug === slug);
  if (!chapter) throw new Error(`no chapter /${slug}`);
  return shareCard(size.width, size.height, chapter.id);
}
