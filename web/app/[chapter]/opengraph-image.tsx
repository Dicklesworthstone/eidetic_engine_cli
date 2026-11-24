// Each chapter page's Open Graph share image, made at build time (lib/og-card.tsx).
import { chapters } from "@/lib/content";
import { shareCard } from "@/lib/og-card";
import { SHARE_IMAGE_TYPE } from "@/lib/share";

export const dynamic = "force-static";
export const dynamicParams = false;
export const alt = "Eidetic Engine (ee): durable, explainable memory for AI agents. One chapter of the story, with its diagram.";
export const size = { width: 1200, height: 630 };
export const contentType = SHARE_IMAGE_TYPE;

export function generateStaticParams() {
  return chapters.map((chapter) => ({ chapter: chapter.slug }));
}

export default async function Image({ params }: { params: Promise<{ chapter: string }> }) {
  const { chapter: slug } = await params;
  const chapter = chapters.find((item) => item.slug === slug);
  if (!chapter) throw new Error(`no chapter /${slug}/`);
  return shareCard(size.width, size.height, chapter.id);
}
