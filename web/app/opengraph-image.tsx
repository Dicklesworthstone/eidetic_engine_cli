// The home page's Open Graph share image, made at build time (lib/og-card.tsx).
import { shareAlt, shareCard } from "@/lib/og-card";
import { SHARE_IMAGE_TYPE } from "@/lib/share";

export const dynamic = "force-static";
export const alt = shareAlt();
export const size = { width: 1200, height: 630 };
export const contentType = SHARE_IMAGE_TYPE;

export default function Image() {
  return shareCard(size.width, size.height);
}
