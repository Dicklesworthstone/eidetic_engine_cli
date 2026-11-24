// The share images: the home page's (app/opengraph-image.tsx, app/twitter-image.tsx) and each
// chapter page's (app/[chapter]/…). The static export writes them without an extension, so
// worker.ts gives them their type.

// Relative, not "@/": worker.ts imports this module, and the Worker's bundler is not Next's.
import { chapters } from "./content";

export const SHARE_IMAGE_TYPE = "image/png";

const IMAGES = ["opengraph-image", "twitter-image"] as const;

export const SHARE_IMAGE_PATHS: readonly string[] = [
  ...IMAGES.map((image) => `/${image}`),
  ...chapters.flatMap((chapter) => IMAGES.map((image) => `/${chapter.slug}/${image}`)),
];
