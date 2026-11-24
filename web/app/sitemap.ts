import type { MetadataRoute } from "next";

import { siteConfig } from "@/lib/content";

export const dynamic = "force-static";

export default function sitemap(): MetadataRoute.Sitemap {
  return [{ url: new URL("/", siteConfig.url).toString(), changeFrequency: "monthly", priority: 1 }];
}
