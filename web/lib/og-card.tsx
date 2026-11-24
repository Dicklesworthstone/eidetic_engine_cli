// The site's share card (app/opengraph-image.tsx, app/twitter-image.tsx), drawn
// at build time by next/og (Satori, not a browser: inline styles, flex on every
// element). The constellation is the hero's toy, the figures are the hero strip's.

import { ImageResponse } from "next/og";

import { heroStats, siteConfig } from "@/lib/content";
import { LEVEL_COLOUR, SECTION_COLOUR } from "@/lib/palette";
import { seeded } from "@/lib/utils";

const INK = "#05040d";

export const shareAlt =
  "Eidetic Engine (ee): your agents forget, ee remembers. Durable, local-first, explainable memory for coding agents.";

/** A Google font as TrueType, which Satori reads (it cannot read WOFF2). */
async function googleFont(family: string, weight: number): Promise<ArrayBuffer> {
  const query = `${family.replace(/ /g, "+")}:wght@${weight}`;
  const sheet = await fetch(`https://fonts.googleapis.com/css2?family=${query}`, {
    signal: AbortSignal.timeout(60_000),
  });
  if (!sheet.ok) throw new Error(`og-card: Google Fonts answered ${sheet.status} for ${family} ${weight}`);
  const text = await sheet.text();
  const url = /src: url\((\S+?)\) format\('(?:truetype|opentype)'\)/.exec(text)?.[1];
  if (!url) throw new Error(`og-card: no TrueType source for ${family} ${weight}`);
  const font = await fetch(url, { signal: AbortSignal.timeout(60_000) });
  if (!font.ok) throw new Error(`og-card: ${url} answered ${font.status}`);
  return font.arrayBuffer();
}

const COLOURS = Object.values(LEVEL_COLOUR);

/** The constellation's points: deterministic, so every build draws the same card. */
function points(width: number, height: number) {
  const random = seeded(7);
  const centres: [number, number][] = [
    [0.66, 0.24],
    [0.84, 0.3],
    [0.74, 0.55],
    [0.92, 0.66],
    [0.6, 0.78],
  ];
  return Array.from({ length: 46 }, (_, index) => {
    const [cx, cy] = centres[index % centres.length] as [number, number];
    const angle = random() * Math.PI * 2;
    const radius = 0.02 + random() * 0.085;
    return {
      x: (cx + Math.cos(angle) * radius) * width,
      y: (cy + Math.sin(angle) * radius * 1.6) * height,
      r: 3 + random() * 6,
      colour: COLOURS[Math.floor(random() * COLOURS.length)] ?? "#a78bfa",
      lit: random() > 0.62,
    };
  });
}

/** The share card at `width × height` (1200 × 630 for Open Graph, 1200 × 600 for X), as PNG. */
export async function shareCard(width: number, height: number): Promise<Response> {
  const [display, body, mono] = await Promise.all([
    googleFont("Inter", 800),
    googleFont("Inter", 500),
    googleFont("JetBrains Mono", 500),
  ]);
  const dots = points(width, height);
  const sections = Object.entries(SECTION_COLOUR);

  return new ImageResponse(
    <div
      style={{
        width,
        height,
        display: "flex",
        position: "relative",
        background: INK,
        fontFamily: "Inter",
        overflow: "hidden",
      }}
    >
      {/* The field of light. */}
      <div
        style={{
          position: "absolute",
          inset: 0,
          display: "flex",
          backgroundImage:
            "radial-gradient(circle at 12% -10%, rgba(139,92,246,0.42), rgba(5,4,13,0) 55%), radial-gradient(circle at 78% 52%, rgba(76,29,149,0.45), rgba(5,4,13,0) 48%)",
        }}
      />
      {/* The constellation. */}
      {dots.map((dot, index) => (
        <div
          key={index}
          style={{
            position: "absolute",
            left: dot.x - dot.r,
            top: dot.y - dot.r,
            width: dot.r * 2,
            height: dot.r * 2,
            borderRadius: 9999,
            background: dot.colour,
            opacity: dot.lit ? 1 : 0.35,
            boxShadow: dot.lit ? `0 0 ${dot.r * 4}px ${dot.colour}` : "none",
            border: dot.lit ? "2px solid rgba(255,255,255,0.85)" : "none",
            display: "flex",
          }}
        />
      ))}

      <div style={{ display: "flex", flexDirection: "column", padding: "64px 72px", width: 760, position: "relative" }}>
        <div
          style={{
            display: "flex",
            alignItems: "center",
            alignSelf: "flex-start",
            gap: 12,
            border: "1px solid rgba(196,181,253,0.3)",
            background: "rgba(167,139,250,0.08)",
            borderRadius: 9999,
            padding: "8px 18px 8px 10px",
          }}
        >
          <div
            style={{
              width: 16,
              height: 16,
              borderRadius: 9999,
              background: "radial-gradient(circle at 34% 30%, #ffffff, #a78bfa 40%, #2e1065 85%)",
              display: "flex",
            }}
          />
          <span style={{ fontFamily: "JetBrains Mono", fontSize: 17, letterSpacing: 3, color: "#ddd6fe" }}>
            MEMORY FOR CODING AGENTS · RUST · LOCAL-FIRST
          </span>
        </div>
        <div style={{ display: "flex", flexDirection: "column", marginTop: 34, fontWeight: 800, letterSpacing: -4, lineHeight: 0.98 }}>
          <span style={{ fontSize: 88, color: "#ffffff" }}>Your agents forget.</span>
          <span
            style={{
              fontSize: 88,
              backgroundImage: "linear-gradient(100deg, #f5d0fe 0%, #c4b5fd 35%, #a78bfa 60%, #67e8f9 100%)",
              backgroundClip: "text",
              color: "transparent",
            }}
          >
            ee remembers.
          </span>
        </div>
        <span style={{ marginTop: 28, fontSize: 25, lineHeight: 1.4, color: "#cbd5e1", maxWidth: 640 }}>
          Durable, explainable memory for coding agents. Every line of context says where it came from and why it is
          there.
        </span>
        <div style={{ display: "flex", gap: 34, marginTop: "auto" }}>
          {heroStats.map((stat) => (
            <div key={stat.label} style={{ display: "flex", flexDirection: "column" }}>
              <span style={{ fontSize: 34, fontWeight: 800, color: "#ffffff", letterSpacing: -1 }}>{stat.value}</span>
              <span style={{ fontFamily: "JetBrains Mono", fontSize: 14, letterSpacing: 2, color: "#c4b5fd" }}>
                {stat.label.toUpperCase()}
              </span>
            </div>
          ))}
        </div>
      </div>

      {/* The pack, in the corner. */}
      <div
        style={{
          position: "absolute",
          right: 56,
          bottom: 52,
          width: 330,
          display: "flex",
          flexDirection: "column",
          gap: 10,
          padding: "20px 22px",
          borderRadius: 22,
          border: "1px solid rgba(196,181,253,0.28)",
          background: "rgba(9,7,26,0.9)",
          fontFamily: "JetBrains Mono",
        }}
      >
        <span style={{ fontSize: 14, letterSpacing: 3, color: "#c4b5fd" }}>CONTEXT PACK · {siteConfig.short}</span>
        {sections.map(([section, colour]) => (
          <div key={section} style={{ display: "flex", alignItems: "center", gap: 10 }}>
            <div style={{ width: 10, height: 10, borderRadius: 9999, background: colour, display: "flex" }} />
            <span style={{ fontSize: 16, color: "#e2e8f0" }}>## {section}</span>
          </div>
        ))}
        <span style={{ fontSize: 13, color: "#8b94ab" }}>provenance · score · why</span>
      </div>

      <span
        style={{
          position: "absolute",
          right: 60,
          top: 50,
          fontFamily: "JetBrains Mono",
          fontSize: 18,
          color: "#a5b4fc",
        }}
      >
        eidetic-engine.org
      </span>
    </div>,
    {
      width,
      height,
      fonts: [
        { name: "Inter", data: display, weight: 800, style: "normal" },
        { name: "Inter", data: body, weight: 500, style: "normal" },
        { name: "JetBrains Mono", data: mono, weight: 500, style: "normal" },
      ],
    },
  );
}
