// The share cards: one for the home page (app/opengraph-image.tsx, app/twitter-image.tsx) and one
// per chapter page (app/[chapter]/…), drawn at build time by next/og. That is Satori, not a
// browser: inline styles only, flex on every element, no CSS filters. The drawings come from
// lib/og-art.ts as embedded SVG, with their words laid over them here.

import { ImageResponse } from "next/og";

import { type Art, amnesiaArt, campaignsArt, dataUri, graphArt, homeArt, type Label, latticeArt, learningArt, memoryArt, packArt } from "@/lib/og-art";
import { type Chapter, chapters, heroStats, rigor, siteConfig } from "@/lib/content";
import { SECTION_COLOUR } from "@/lib/palette";

const INK = "#05040d";
const HOST = new URL(siteConfig.url).host;

/** "home", or a chapter's id. */
export type CardId = "home" | Chapter["id"];

const HOME = {
  kicker: "Memory for AI agents · Rust · local-first",
  headline: "Your agents forget.",
  accent: "ee remembers.",
  line: "Durable, explainable memory for agents. Every line of context says where it came from and why it is there.",
  colour: "#a78bfa",
};

function chapterFor(id: CardId): Chapter | undefined {
  return chapters.find((chapter) => chapter.id === id);
}

/** The alt text for a card. */
export function shareAlt(id: CardId = "home"): string {
  const chapter = chapterFor(id);
  if (!chapter) return `${siteConfig.name} (ee): your agents forget, ee remembers. Durable, local-first, explainable memory for AI agents.`;
  return `${siteConfig.name} (ee), chapter ${chapter.number}: ${chapter.share.headline} ${chapter.share.accent} ${chapter.share.line}`;
}

// ─── Fonts ──────────────────────────────────────────────────────────────────

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

let fontsOnce: Promise<[ArrayBuffer, ArrayBuffer, ArrayBuffer]> | undefined;
/** The three faces, fetched once per build however many cards are drawn. */
function fonts() {
  fontsOnce ??= Promise.all([googleFont("Inter", 800), googleFont("Inter", 500), googleFont("JetBrains Mono", 500)]);
  return fontsOnce;
}

// ─── Colour ─────────────────────────────────────────────────────────────────

function rgb(hex: string): [number, number, number] {
  const value = Number.parseInt(hex.slice(1), 16);
  return [(value >> 16) & 255, (value >> 8) & 255, value & 255];
}

function rgba(hex: string, alpha: number): string {
  const [r, g, b] = rgb(hex);
  return `rgba(${r},${g},${b},${alpha})`;
}

/** `hex` mixed toward white by `amount`. */
function tint(hex: string, amount: number): string {
  const [r, g, b] = rgb(hex).map((c) => Math.round(c + (255 - c) * amount)) as [number, number, number];
  return `#${((r << 16) | (g << 8) | b).toString(16).padStart(6, "0")}`;
}

// ─── Pieces ─────────────────────────────────────────────────────────────────

/** A drawing's words, laid over it. */
function Labels({ labels }: { labels: Label[] }) {
  return (
    <>
      {labels.map((label, index) => {
        const anchor = label.anchor ?? "start";
        const box = 460;
        const left = anchor === "start" ? label.x : anchor === "end" ? label.x - box : label.x - box / 2;
        return (
          <div
            key={index}
            style={{
              position: "absolute",
              left,
              top: label.y,
              width: box,
              display: "flex",
              justifyContent: anchor === "start" ? "flex-start" : anchor === "end" ? "flex-end" : "center",
              fontFamily: label.mono ? "JetBrains Mono" : "Inter",
              fontSize: label.size ?? 14,
              fontWeight: label.weight ?? 500,
              letterSpacing: label.mono ? 0.5 : -0.5,
              color: label.colour,
              opacity: label.opacity ?? 1,
              whiteSpace: "nowrap",
            }}
          >
            {label.text}
          </div>
        );
      })}
    </>
  );
}

/** The drawing on the card's right, faded into the ink on its left edge. */
function Drawing({ art, width, height }: { art: Art; width: number; height: number }) {
  return (
    <div style={{ position: "absolute", right: 0, top: 0, width, height, display: "flex" }}>
      {/* biome-ignore lint/performance/noImgElement: Satori draws <img>, not next/image. */}
      <img src={dataUri(art.svg)} width={width} height={height} style={{ position: "absolute", left: 0, top: 0 }} alt="" />
      <Labels labels={art.labels} />
    </div>
  );
}

/** The agent's reading of an error, for chapter 06: the envelope and the exit codes. */
function Envelope({ width, height, colour }: { width: number; height: number; colour: string }) {
  const key = "#bef264";
  const text = "#fde68a";
  const plain = "#94a3b8";
  const lines: [number, string, string, string][] = [
    [0, "{", "", ""],
    [1, '"schema": ', '"ee.error.v2"', text],
    [1, '"error": {', "", ""],
    [2, '"code": ', '"search_index_stale"', text],
    [2, '"severity": ', '"medium"', "#fdba74"],
    [2, '"repair": ', '"ee index rebuild --workspace ."', text],
    [2, '"details": { "recovery": [{', "", ""],
    [3, '"priority": ', "0", "#c4b5fd"],
    [3, '"riskClass": ', '"idempotent_refresh"', "#86efac"],
    [3, '"requiresHumanApproval": ', "false", "#fda4af"],
    [3, '"command": ', '"ee index rebuild …"', text],
    [2, "}] } } }", "", ""],
  ];
  const codes = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 130];
  return (
    <div style={{ position: "absolute", right: 44, top: 44, width: width - 70, height: height - 120, display: "flex", flexDirection: "column" }}>
      <div
        style={{
          display: "flex",
          flexDirection: "column",
          padding: "20px 26px",
          borderRadius: 22,
          border: `1px solid ${rgba(colour, 0.35)}`,
          background: "rgba(10,14,8,0.92)",
          boxShadow: `0 0 80px ${rgba(colour, 0.18)}`,
          fontFamily: "JetBrains Mono",
          fontSize: 16,
          lineHeight: 1.5,
        }}
      >
        <div style={{ display: "flex", alignItems: "center", gap: 10, marginBottom: 10 }}>
          <div style={{ width: 10, height: 10, borderRadius: 9999, background: "#fb7185", display: "flex" }} />
          <div style={{ width: 10, height: 10, borderRadius: 9999, background: "#fbbf24", display: "flex" }} />
          <div style={{ width: 10, height: 10, borderRadius: 9999, background: colour, display: "flex" }} />
          <span style={{ marginLeft: 10, fontSize: 14, color: plain }}>$ ee search "release" --json · exit 4</span>
        </div>
        {lines.map(([depth, head, value, valueColour], index) => (
          <div key={index} style={{ display: "flex", paddingLeft: depth * 20, whiteSpace: "nowrap" }}>
            {/* Satori trims a span's trailing space, so the gap after a colon is a no-break space. */}
            <span style={{ color: value ? key : plain }}>{head.replace(/ $/, " ")}</span>
            {value ? <span style={{ color: valueColour }}>{value}</span> : null}
          </div>
        ))}
      </div>
      <div style={{ marginTop: 16, display: "flex", gap: 6 }}>
        {codes.map((code) => (
          <div
            key={code}
            style={{
              flex: 1,
              height: 40,
              display: "flex",
              alignItems: "center",
              justifyContent: "center",
              borderRadius: 10,
              fontFamily: "JetBrains Mono",
              fontSize: 16,
              fontWeight: 500,
              color: code === 4 ? "#0b0f05" : code === 0 ? "#d9f99d" : "#94a3b8",
              background: code === 4 ? colour : "rgba(255,255,255,0.05)",
              border: `1px solid ${code === 4 ? colour : "rgba(255,255,255,0.08)"}`,
            }}
          >
            {code}
          </div>
        ))}
      </div>
      <div style={{ marginTop: 12, display: "flex", fontFamily: "JetBrains Mono", fontSize: 14, color: tint(colour, 0.35) }}>
        exit code → recovery[0] → idempotent_refresh → safe to retry
      </div>
    </div>
  );
}

/** The figures wall, for chapter 08. */
function Figures({ width, height, colour }: { width: number; height: number; colour: string }) {
  const tests = heroStats.find((stat) => stat.label === "Tests")?.value ?? "";
  const failures = heroStats.find((stat) => stat.label === "Failure modes")?.value ?? "";
  const schemas = heroStats.find((stat) => stat.label === "JSON contracts")?.value ?? "";
  const adrs = rigor.find((item) => item.label === "architecture decisions")?.value ?? "";
  const migrations = rigor.find((item) => item.label === "forward-only migrations")?.value ?? "";
  const tiles: [string, string][] = [
    [tests, "tests"],
    [failures, "failure-mode fixtures"],
    [schemas, "JSON schemas"],
    [adrs, "decision records"],
    [migrations, "forward-only migrations"],
    ["27", "fuzz targets"],
  ];
  return (
    <div style={{ position: "absolute", right: 48, top: 64, width: width - 72, display: "flex", flexWrap: "wrap", gap: 14 }}>
      {tiles.map(([value, label], index) => (
        <div
          key={label}
          style={{
            width: (width - 72 - 14) / 2,
            height: (height - 250) / 3,
            display: "flex",
            flexDirection: "column",
            justifyContent: "center",
            padding: "0 24px",
            borderRadius: 20,
            border: `1px solid ${rgba(colour, index === 0 ? 0.5 : 0.18)}`,
            background: index === 0 ? rgba(colour, 0.12) : "rgba(255,255,255,0.03)",
          }}
        >
          <span style={{ fontSize: 50, fontWeight: 800, letterSpacing: -2, color: "#ffffff" }}>{value}</span>
          <span style={{ fontFamily: "JetBrains Mono", fontSize: 15, letterSpacing: 1, color: tint(colour, 0.3) }}>{label}</span>
        </div>
      ))}
    </div>
  );
}

/** The home card's pack, in the corner. */
function PackCorner({ height }: { height: number }) {
  return (
    <div
      style={{
        position: "absolute",
        right: 52,
        top: height - 312,
        width: 312,
        display: "flex",
        flexDirection: "column",
        gap: 9,
        padding: "20px 22px",
        borderRadius: 22,
        border: "1px solid rgba(196,181,253,0.32)",
        background: "rgba(9,7,26,0.92)",
        boxShadow: "0 0 70px rgba(139,92,246,0.25)",
        fontFamily: "JetBrains Mono",
      }}
    >
      <span style={{ fontSize: 13, letterSpacing: 3, color: "#c4b5fd" }}>CONTEXT PACK · BUDGETED</span>
      {Object.entries(SECTION_COLOUR).map(([section, colour], index) => (
        <div key={section} style={{ display: "flex", alignItems: "center", gap: 10 }}>
          <div style={{ width: 10, height: 10, borderRadius: 9999, background: colour, display: "flex" }} />
          <span style={{ fontSize: 15, color: "#e2e8f0", width: 120 }}>## {section}</span>
          <div style={{ height: 6, width: [120, 82, 82, 82, 40][index], borderRadius: 3, background: colour, opacity: 0.85, display: "flex" }} />
        </div>
      ))}
      <span style={{ fontSize: 12.5, color: "#8b94ab" }}>provenance · score · why</span>
    </div>
  );
}

// ─── The card ───────────────────────────────────────────────────────────────

/** The share card for `id` at `width × height` (1200 × 630 for Open Graph, 1200 × 600 for X), as PNG. */
export async function shareCard(width: number, height: number, id: CardId = "home"): Promise<Response> {
  const [display, body, mono] = await fonts();
  const chapter = chapterFor(id);
  const words = chapter ? { ...chapter.share, colour: chapter.colour } : HOME;
  const colour = words.colour;
  const artWidth = 600;
  // A drawing in two parts sits in one layer: Satori drops a fragment held in a variable.
  const layer = { position: "absolute", left: 0, top: 0, width, height, display: "flex" } as const;

  let drawing = null;
  switch (id) {
    case "home":
      drawing = (
        <div style={layer}>
          <Drawing art={homeArt(artWidth, height)} width={artWidth} height={height} />
          <PackCorner height={height} />
        </div>
      );
      break;
    case "forget":
      drawing = <Drawing art={amnesiaArt(artWidth, height)} width={artWidth} height={height} />;
      break;
    case "memory":
      drawing = <Drawing art={memoryArt(artWidth, height)} width={artWidth} height={height} />;
      break;
    case "pack":
      drawing = <Drawing art={packArt(artWidth, height)} width={artWidth} height={height} />;
      break;
    case "learn":
      drawing = <Drawing art={learningArt(artWidth, height)} width={artWidth} height={height} />;
      break;
    case "campaigns":
      drawing = <Drawing art={campaignsArt(artWidth, height)} width={artWidth} height={height} />;
      break;
    case "agents":
      drawing = (
        <div style={layer}>
          <Drawing art={latticeArt(artWidth, height, colour)} width={artWidth} height={height} />
          <Envelope width={artWidth} height={height} colour={colour} />
        </div>
      );
      break;
    case "graph":
      drawing = <Drawing art={graphArt(artWidth, height)} width={artWidth} height={height} />;
      break;
    case "proof":
      drawing = (
        <div style={layer}>
          <Drawing art={latticeArt(artWidth, height, colour)} width={artWidth} height={height} />
          <Figures width={artWidth} height={height} colour={colour} />
        </div>
      );
      break;
    default:
      throw new Error(`og-card: no drawing for ${id}`);
  }

  const path = chapter ? `${HOST}/${chapter.slug}` : HOST;

  return new ImageResponse(
    <div style={{ width, height, display: "flex", position: "relative", background: INK, fontFamily: "Inter", overflow: "hidden" }}>
      {/* The field of light, in the chapter's colour. */}
      <div
        style={{
          position: "absolute",
          inset: 0,
          display: "flex",
          backgroundImage: `radial-gradient(circle at 8% -12%, ${rgba(colour, 0.38)}, rgba(5,4,13,0) 52%), radial-gradient(circle at 80% 55%, ${rgba(colour, 0.2)}, rgba(5,4,13,0) 50%), radial-gradient(circle at 40% 120%, rgba(76,29,149,0.35), rgba(5,4,13,0) 55%)`,
        }}
      />
      {drawing}
      {/* The drawing fades out under the words. */}
      <div
        style={{
          position: "absolute",
          left: 0,
          top: 0,
          width: 660,
          height,
          display: "flex",
          backgroundImage: "linear-gradient(90deg, rgba(5,4,13,0.96) 0%, rgba(5,4,13,0.9) 86%, rgba(5,4,13,0) 100%)",
        }}
      />

      <div style={{ position: "absolute", left: 64, top: 52, width: 536, height: height - 104, display: "flex", flexDirection: "column" }}>
        <div style={{ display: "flex", alignItems: "center", gap: 12 }}>
          <div
            style={{
              width: 26,
              height: 26,
              borderRadius: 9999,
              display: "flex",
              alignItems: "center",
              justifyContent: "center",
              background: "radial-gradient(circle at 35% 35%, #f5d0fe, #a78bfa 42%, #1e1147 78%)",
              boxShadow: "0 0 18px rgba(139,92,246,0.6)",
            }}
          >
            <div style={{ width: 11, height: 11, borderRadius: 9999, background: "rgba(5,4,13,0.85)", display: "flex" }} />
          </div>
          <span style={{ fontSize: 22, fontWeight: 800, color: "#ffffff", letterSpacing: -0.5 }}>{siteConfig.name}</span>
          <span style={{ fontFamily: "JetBrains Mono", fontSize: 16, color: "#c4b5fd" }}>ee</span>
        </div>

        <div
          style={{
            display: "flex",
            alignSelf: "flex-start",
            alignItems: "center",
            gap: 10,
            marginTop: 30,
            padding: "7px 16px 7px 10px",
            borderRadius: 9999,
            border: `1px solid ${rgba(colour, 0.4)}`,
            background: rgba(colour, 0.1),
          }}
        >
          <div style={{ width: 12, height: 12, borderRadius: 9999, background: colour, boxShadow: `0 0 12px ${colour}`, display: "flex" }} />
          <span style={{ fontFamily: "JetBrains Mono", fontSize: 15, letterSpacing: 3, color: tint(colour, 0.55) }}>
            {words.kicker.toUpperCase()}
          </span>
        </div>

        <div style={{ display: "flex", flexDirection: "column", marginTop: 26, fontWeight: 800, letterSpacing: -3.2, lineHeight: 1.0 }}>
          <span style={{ fontSize: 66, color: "#ffffff" }}>{words.headline}</span>
          <span
            style={{
              fontSize: 66,
              backgroundImage: `linear-gradient(100deg, ${tint(colour, 0.7)} 0%, ${tint(colour, 0.25)} 40%, ${colour} 75%, #67e8f9 120%)`,
              backgroundClip: "text",
              color: "transparent",
            }}
          >
            {words.accent}
          </span>
        </div>

        <span style={{ marginTop: 22, fontSize: 21, lineHeight: 1.45, color: "#cbd5e1", maxWidth: 500 }}>{words.line}</span>

        <div style={{ display: "flex", marginTop: "auto", alignItems: "flex-end" }}>
          {chapter ? (
            <div style={{ display: "flex", gap: 7 }}>
              {chapters.map((item) => (
                <div
                  key={item.id}
                  style={{
                    display: "flex",
                    alignItems: "center",
                    justifyContent: "center",
                    width: item.id === chapter.id ? 54 : 34,
                    height: 30,
                    borderRadius: 9,
                    fontFamily: "JetBrains Mono",
                    fontSize: 13,
                    color: item.id === chapter.id ? "#05040d" : tint(item.colour, 0.2),
                    background: item.id === chapter.id ? item.colour : rgba(item.colour, 0.1),
                    border: `1px solid ${rgba(item.colour, item.id === chapter.id ? 1 : 0.3)}`,
                  }}
                >
                  {item.number}
                </div>
              ))}
            </div>
          ) : (
            <div style={{ display: "flex", gap: 32 }}>
              {heroStats.map((stat) => (
                <div key={stat.label} style={{ display: "flex", flexDirection: "column" }}>
                  <span style={{ fontSize: 32, fontWeight: 800, color: "#ffffff", letterSpacing: -1 }}>{stat.value}</span>
                  <span style={{ fontFamily: "JetBrains Mono", fontSize: 12.5, letterSpacing: 2, color: "#c4b5fd" }}>
                    {stat.label.toUpperCase()}
                  </span>
                </div>
              ))}
            </div>
          )}
        </div>
      </div>

      <span
        style={{
          position: "absolute",
          right: 52,
          bottom: 40,
          display: "flex",
          fontFamily: "JetBrains Mono",
          fontSize: 18,
          color: tint(colour, 0.45),
        }}
      >
        {path}
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
