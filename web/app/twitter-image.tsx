// The home page's X (Twitter) share image, made at build time (lib/og-card.tsx).
import { shareCard } from "@/lib/og-card";
import { SHARE_IMAGE_SIZE, SHARE_IMAGE_TYPE, shareAlt } from "@/lib/share";

export const dynamic = "force-static";
export const alt = shareAlt();
export const size = SHARE_IMAGE_SIZE.twitter;
export const contentType = SHARE_IMAGE_TYPE;

export default function Image() {
  return shareCard(size.width, size.height);
}
