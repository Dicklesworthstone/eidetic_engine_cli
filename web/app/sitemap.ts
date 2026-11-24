import type { MetadataRoute } from "next";

import { chapters, siteConfig } from "@/lib/content";

export const dynamic = "force-static";

export default function sitemap(): MetadataRoute.Sitemap {
  return [
    { url: new URL("/", siteConfig.url).toString(), changeFrequency: "monthly", priority: 1 },
    ...chapters.map((chapter) => ({
      url: new URL(`/${chapter.slug}`, siteConfig.url).toString(),
      changeFrequency: "monthly" as const,
      priority: 0.8,
    })),
  ];
}
