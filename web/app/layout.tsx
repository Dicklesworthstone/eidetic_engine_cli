import type { Metadata, Viewport } from "next";
import { Inter, JetBrains_Mono } from "next/font/google";
import type { ReactNode } from "react";

import { siteConfig } from "@/lib/content";

// The compiled stylesheet, through a module scripts/build-style.ts writes (it says why).
import "@/.generated/style-import";

const inter = Inter({ subsets: ["latin"], variable: "--font-inter", display: "swap" });
const jetbrains = JetBrains_Mono({ subsets: ["latin"], variable: "--font-jetbrains", display: "swap" });

// The share images are app/opengraph-image.tsx and app/twitter-image.tsx, which Next.js
// links itself (with a hash of each route); naming images here would override them.
export const metadata: Metadata = {
  metadataBase: new URL(siteConfig.url),
  title: siteConfig.title,
  description: siteConfig.description,
  openGraph: {
    title: siteConfig.title,
    description: siteConfig.description,
    url: "/",
    siteName: siteConfig.name,
    type: "website",
  },
  twitter: {
    card: "summary_large_image",
    title: siteConfig.title,
    description: siteConfig.description,
  },
};

export const viewport: Viewport = {
  themeColor: "#05040d",
  colorScheme: "dark",
};

// Before the body is parsed: arms `.first-view` for a reader arriving at the top. Not for one sent to an
// anchor, or coming back to where they were (a reload, the back button), whose place depends on every height above it;
// and a click on an in-page link ends it at once, for the same reason.
const FIRST_VIEW = `try{(function(){var r=document.documentElement,n=performance.getEntriesByType("navigation")[0];if(location.hash||!n||n.type!=="navigate")return;r.classList.add("first-view");addEventListener("click",function(e){if(e.target instanceof Element&&e.target.closest('a[href^="#"]'))r.classList.remove("first-view")},true)})()}catch(e){}`;

export default function RootLayout({ children }: { children: ReactNode }) {
  return (
    <html lang="en" className={`${inter.variable} ${jetbrains.variable}`} suppressHydrationWarning>
      <head>
        <script dangerouslySetInnerHTML={{ __html: FIRST_VIEW }} />
      </head>
      <body className="min-h-screen bg-[#05040d] antialiased">{children}</body>
    </html>
  );
}
