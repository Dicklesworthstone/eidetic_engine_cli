// The artwork on each share card (lib/og-card.tsx): one drawing per chapter, plus the home page's.
//
// Each drawing is a plain SVG string that the card embeds as an <img>. Satori draws JSX SVG only in
// a narrow subset (no loops, no transforms, no dashes); an embedded image goes straight to resvg,
// which draws the full language, so the art can be computed. resvg has no fonts here, so the words
// on a drawing come back as `labels` and the card lays them over it as text.
//
// Everything is deterministic: the same build draws the same cards. The figures drawn are the
// site's own: half-lives from components/explain/decay.tsx, quotas from components/hero.tsx, the
// trust classes from components/explain/trust.tsx.

import { LEVEL_COLOUR, SECTION_COLOUR } from "@/lib/palette";
import { seeded } from "@/lib/utils";

export type Label = {
  x: number;
  y: number;
  text: string;
  colour: string;
  size?: number;
  mono?: boolean;
  weight?: 500 | 800;
  /** Which point of the text sits at (x, y). */
  anchor?: "start" | "middle" | "end";
  opacity?: number;
};

export type Art = { svg: string; labels: Label[] };

/** A drawing `w × h`, with its definitions. */
function svg(w: number, h: number, body: string, defs = ""): string {
  return `<svg xmlns="http://www.w3.org/2000/svg" width="${w}" height="${h}" viewBox="0 0 ${w} ${h}"><defs>${GLOW}${defs}</defs>${body}</svg>`;
}

const GLOW =
  '<filter id="glow" x="-50%" y="-50%" width="200%" height="200%"><feGaussianBlur stdDeviation="6" result="b"/><feMerge><feMergeNode in="b"/><feMergeNode in="SourceGraphic"/></feMerge></filter>' +
  '<filter id="haze" x="-100%" y="-100%" width="300%" height="300%"><feGaussianBlur stdDeviation="18"/></filter>';

const n = (value: number) => value.toFixed(1);

/** A data URI resvg can read. */
export function dataUri(markup: string): string {
  return `data:image/svg+xml;base64,${Buffer.from(markup).toString("base64")}`;
}

// ─── Home: the constellation, packed ─────────────────────────────────────────

export function homeArt(w: number, h: number): Art {
  const random = seeded(7);
  const colours = Object.values(LEVEL_COLOUR);
  const centres: [number, number][] = [
    [0.22, 0.22],
    [0.55, 0.16],
    [0.86, 0.24],
    [0.3, 0.52],
    [0.68, 0.44],
  ];
  // The pack card's top-left corner, where lib/og-card.tsx's PackCorner puts it (312 wide, 52 from the right,
  // its top 312 above the foot), so every beam ends under the card rather than below it.
  const card = { x: w - 364, y: h - 312 };
  let haze = "";
  let beams = "";
  let dots = "";
  for (let index = 0; index < 64; index += 1) {
    const [cx, cy] = centres[index % centres.length] as [number, number];
    const angle = random() * Math.PI * 2;
    const radius = 0.03 + random() * 0.11;
    const x = (cx + Math.cos(angle) * radius) * w;
    const y = (cy + Math.sin(angle) * radius * 1.3) * h;
    const r = 2.5 + random() * 5.5;
    const colour = colours[Math.floor(random() * colours.length)] ?? "#a78bfa";
    const lit = random() > 0.7;
    if (lit) {
      haze += `<circle cx="${n(x)}" cy="${n(y)}" r="${n(r * 4)}" fill="${colour}" opacity="0.35" filter="url(#haze)"/>`;
      beams += `<path d="M ${n(x)} ${n(y)} Q ${n((x + card.x) / 2)} ${n(Math.max(y, card.y) + 40)} ${n(card.x + 8)} ${n(card.y + 70 + (index % 5) * 30)}" stroke="url(#beam)" stroke-width="1.4" fill="none" opacity="0.75"/>`;
      dots += `<circle cx="${n(x)}" cy="${n(y)}" r="${n(r + 5)}" fill="none" stroke="#ffffff" stroke-opacity="0.55" stroke-width="1.2"/>`;
    }
    dots += `<circle cx="${n(x)}" cy="${n(y)}" r="${n(r)}" fill="${colour}" opacity="${lit ? 1 : 0.32}"${lit ? ' filter="url(#glow)"' : ""}/>`;
  }
  const defs = `<linearGradient id="beam" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="#f5d0fe" stop-opacity="0.1"/><stop offset="1" stop-color="#a78bfa" stop-opacity="0.95"/></linearGradient>`;
  return { svg: svg(w, h, haze + beams + dots, defs), labels: [] };
}

// ─── 01 · Amnesia: a notes file against a pack ───────────────────────────────

export function amnesiaArt(w: number, h: number): Art {
  // Counted in lessons, not tokens: ten a week (components/explain/scale-model.ts's rate). The file is
  // read whole and is trimmed by hand at 200 lines back to 120, deleting lessons; ee's store keeps every
  // one (stale ones are demoted or tombstoned with an audit entry); a session reads a fixed-size pack.
  const left = 56;
  const right = w - 44;
  const top = 168;
  const bottom = h - 132;
  const weeks = 26;
  const ceiling = 280;
  const x = (week: number) => left + ((right - left) * week) / weeks;
  const y = (lessons: number) => bottom - ((bottom - top) * Math.min(ceiling, lessons)) / ceiling;
  const store = (week: number) => 10 * week;
  const trimAt = 20;
  const file = (week: number) => (week < trimAt ? 10 * week : 120 + 10 * (week - trimAt));
  const pack = (week: number) => Math.min(80, store(week));

  let storeLine = "";
  let fileLine = "";
  let fileArea = `M ${n(x(0))} ${n(bottom)}`;
  let packTop = "";
  for (let week = 0; week <= weeks; week += 0.5) {
    if (week === trimAt) {
      fileLine += ` L ${n(x(trimAt))} ${n(y(200))}`;
      fileArea += ` L ${n(x(trimAt))} ${n(y(200))}`;
    }
    storeLine += `${storeLine ? " L" : "M"} ${n(x(week))} ${n(y(store(week)))}`;
    fileLine += `${fileLine ? " L" : "M"} ${n(x(week))} ${n(y(file(week)))}`;
    fileArea += ` L ${n(x(week))} ${n(y(file(week)))}`;
    packTop += ` L ${n(x(week))} ${n(y(pack(week)))}`;
  }
  fileArea += ` L ${n(x(weeks))} ${n(bottom)} Z`;
  const storeArea = `M ${n(x(0))} ${n(bottom)} L${storeLine.slice(1)} L ${n(x(weeks))} ${n(bottom)} Z`;

  // The pack: what a session reads, in the five section colours, stacked under its top.
  const sections = Object.values(SECTION_COLOUR);
  const shares = [0.3, 0.2, 0.2, 0.2, 0.1];
  let band = "";
  let under = 0;
  shares.forEach((share, at) => {
    let d = "";
    for (let week = 0; week <= weeks; week += 0.5) d += `${d ? " L" : "M"} ${n(x(week))} ${n(y(pack(week) * (under + share)))}`;
    for (let week = weeks; week >= 0; week -= 0.5) d += ` L ${n(x(week))} ${n(y(pack(week) * under))}`;
    band += `<path d="${d} Z" fill="${sections[at]}" opacity="${0.3 + at * 0.04}"/>`;
    under += share;
  });

  // The legend, above the plot.
  const legend: [string, string, string, boolean][] = [
    ["#67e8f9", "ee store", "every lesson kept; stale ones demoted", false],
    ["#fb7185", "CLAUDE.md", "read whole every session; trimmed by hand", false],
    ["#c4b5fd", "ee pack", "what a session reads: a fixed budget", true],
  ];
  let swatches = "";
  const labels: Label[] = [];
  legend.forEach(([colour, name, text, isPack], at) => {
    const ly = 34 + at * 34;
    swatches += isPack
      ? `<rect x="${left}" y="${ly + 3}" width="24" height="14" rx="3" fill="${colour}" opacity="0.8"/>`
      : `<line x1="${left}" y1="${ly + 10}" x2="${left + 24}" y2="${ly + 10}" stroke="${colour}" stroke-width="3.5" stroke-linecap="round"/>`;
    labels.push({ x: left + 34, y: ly - 3, text: name, colour, size: 18, weight: 800 });
    labels.push({ x: left + 150, y: ly, text, colour: "#94a3b8", size: 14, mono: true });
  });

  const body =
    swatches +
    `<line x1="${left}" y1="${n(bottom)}" x2="${right}" y2="${n(bottom)}" stroke="#334155" stroke-width="1.2"/>` +
    `<path d="${storeArea}" fill="url(#storeFill)"/>` +
    `<path d="${fileArea}" fill="url(#fileFill)"/>` +
    band +
    `<path d="M${packTop.slice(2)}" stroke="#c4b5fd" stroke-width="3" fill="none" filter="url(#glow)"/>` +
    `<path d="${fileLine}" stroke="#fb7185" stroke-width="3.5" fill="none" filter="url(#glow)"/>` +
    `<path d="${storeLine}" stroke="#67e8f9" stroke-width="3.5" fill="none" filter="url(#glow)"/>` +
    // The trim: eighty lessons cut away by hand.
    `<rect x="${n(x(trimAt) - 5)}" y="${n(y(200))}" width="10" height="${n(y(120) - y(200))}" rx="3" fill="#fb7185" fill-opacity="0.18" stroke="#fb7185" stroke-opacity="0.6" stroke-dasharray="3 3"/>` +
    `<circle cx="${n(x(trimAt))}" cy="${n(y(200))}" r="6" fill="#05040d" stroke="#fb7185" stroke-width="2.5"/>` +
    `<circle cx="${n(x(weeks))}" cy="${n(y(store(weeks)))}" r="6" fill="#67e8f9" filter="url(#glow)"/>` +
    `<circle cx="${n(x(weeks))}" cy="${n(y(file(weeks)))}" r="6" fill="#fb7185" filter="url(#glow)"/>` +
    `<circle cx="${n(x(weeks))}" cy="${n(y(pack(weeks)))}" r="6" fill="#c4b5fd" filter="url(#glow)"/>`;
  const defs =
    `<linearGradient id="fileFill" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#fb7185" stop-opacity="0.3"/><stop offset="1" stop-color="#fb7185" stop-opacity="0.02"/></linearGradient>` +
    `<linearGradient id="storeFill" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#67e8f9" stop-opacity="0.16"/><stop offset="1" stop-color="#67e8f9" stop-opacity="0"/></linearGradient>`;
  labels.push(
    // Above the store line, beside the cut's top: at the cut's middle the rising line runs through the words.
    { x: x(trimAt) - 12, y: y(212) - 8, text: "80 lessons deleted", colour: "#fda4af", size: 13, mono: true, anchor: "end" },
    { x: left, y: bottom + 20, text: "week 1", colour: "#64748b", size: 13, mono: true },
    { x: right, y: bottom + 20, text: "week 26 · lessons", colour: "#64748b", size: 13, mono: true, anchor: "end" },
  );
  return { svg: svg(w, h, body, defs), labels };
}

// ─── 02 · Memory: five half-lives ────────────────────────────────────────────

const HALF_LIVES = [
  { half: 1, colour: LEVEL_COLOUR.working, label: "working · 1 d", dash: false },
  { half: 30, colour: LEVEL_COLOUR.episodic, label: "episodic event · 30 d", dash: false },
  { half: 90, colour: "#fda4af", label: "episodic failure · 90 d", dash: true },
  { half: 180, colour: LEVEL_COLOUR.semantic, label: "semantic fact · 180 d", dash: false },
  { half: 365, colour: LEVEL_COLOUR.procedural, label: "procedural rule · 365 d", dash: false },
] as const;

export function memoryArt(w: number, h: number): Art {
  const left = 48;
  const right = w - 40;
  const top = 196;
  const bottom = h - 120;
  const max = Math.log1p(730);
  const x = (days: number) => left + ((right - left) * Math.log1p(days)) / max;
  const y = (value: number) => bottom - (bottom - top) * value;
  let body = `<line x1="${left}" y1="${n(bottom)}" x2="${right}" y2="${n(bottom)}" stroke="#334155" stroke-width="1.2"/>`;
  body += `<line x1="${left}" y1="${n(y(0.5))}" x2="${right}" y2="${n(y(0.5))}" stroke="#475569" stroke-width="1" stroke-dasharray="3 6"/>`;
  for (const days of [1, 7, 30, 90, 180, 365, 730]) {
    body += `<line x1="${n(x(days))}" y1="${n(bottom)}" x2="${n(x(days))}" y2="${n(bottom + 8)}" stroke="#475569"/>`;
  }
  for (const curve of HALF_LIVES) {
    let d = "";
    for (let step = 0; step <= 160; step += 1) {
      const days = Math.expm1((max * step) / 160);
      const value = Math.exp((-Math.LN2 * days) / curve.half);
      d += `${d ? " L" : "M"} ${n(x(days))} ${n(y(value))}`;
    }
    body += `<path d="${d}" stroke="${curve.colour}" stroke-width="3.5" fill="none" filter="url(#glow)"${curve.dash ? ' stroke-dasharray="9 7"' : ""}/>`;
    body += `<circle cx="${n(x(curve.half))}" cy="${n(y(0.5))}" r="6" fill="#05040d" stroke="${curve.colour}" stroke-width="3"/>`;
  }
  // The legend, above the plot in two columns, each entry with a swatch of its line.
  const labels: Label[] = [];
  HALF_LIVES.forEach((curve, at) => {
    const lx = at < 3 ? 64 : 316;
    const ly = 44 + (at % 3) * 32;
    body += `<line x1="${lx}" y1="${ly + 10}" x2="${lx + 24}" y2="${ly + 10}" stroke="${curve.colour}" stroke-width="3.5" stroke-linecap="round"${curve.dash ? ' stroke-dasharray="6 5"' : ""}/>`;
    labels.push({ x: lx + 32, y: ly, text: curve.label, colour: curve.colour, size: 15, mono: true });
  });
  labels.push(
    { x: left + 6, y: y(0.5) - 24, text: "half", colour: "#94a3b8", size: 13, mono: true },
    { x: left, y: bottom + 18, text: "today", colour: "#64748b", size: 13, mono: true },
    { x: x(30), y: bottom + 18, text: "1 mo", colour: "#64748b", size: 13, mono: true, anchor: "middle" },
    { x: x(365), y: bottom + 18, text: "1 y", colour: "#64748b", size: 13, mono: true, anchor: "middle" },
    { x: right, y: bottom + 18, text: "2 y", colour: "#64748b", size: 13, mono: true, anchor: "end" },
  );
  return { svg: svg(w, h, body), labels };
}

// ─── 03 · The pack: many candidates, one budget ───────────────────────────────

export function packArt(w: number, h: number): Art {
  const random = seeded(31);
  const sections = Object.entries(SECTION_COLOUR);
  const shares = [0.3, 0.2, 0.2, 0.2, 0.1];
  const stack = { x: w - 300, y: 96, width: 256, row: 56 };
  let body = "";
  // The candidates: a field of them on the left, most grey, the chosen ones coloured and beamed in.
  for (let index = 0; index < 90; index += 1) {
    const cx = 40 + random() * 200;
    const cy = 70 + random() * (h - 220);
    const pick = random();
    const chosen = pick > 0.78;
    const section = Math.floor(random() * sections.length);
    const colour = chosen ? (sections[section]?.[1] ?? "#a78bfa") : "#64748b";
    const r = 2 + random() * 3.5;
    if (chosen) {
      const ty = stack.y + section * stack.row + stack.row / 2 - 4;
      body += `<path d="M ${n(cx)} ${n(cy)} C ${n(cx + 140)} ${n(cy)} ${n(stack.x - 120)} ${n(ty)} ${n(stack.x - 4)} ${n(ty)}" stroke="${colour}" stroke-opacity="0.38" stroke-width="1.2" fill="none"/>`;
    }
    body += `<circle cx="${n(cx)}" cy="${n(cy)}" r="${n(r)}" fill="${colour}" opacity="${chosen ? 1 : 0.28}"${chosen ? ' filter="url(#glow)"' : ""}/>`;
  }
  // The pack: five rows, each a quota of the budget, filled with items.
  sections.forEach(([, colour], at) => {
    const y = stack.y + at * stack.row;
    const quota = stack.width * (shares[at] ?? 0.1) * 3.1;
    body += `<rect x="${stack.x}" y="${n(y + 18)}" width="${stack.width}" height="22" rx="6" fill="#ffffff" fill-opacity="0.04" stroke="#ffffff" stroke-opacity="0.08"/>`;
    let cursor = stack.x + 3;
    const end = stack.x + Math.min(stack.width - 3, quota);
    while (cursor < end - 10) {
      const size = Math.min(end - cursor, 18 + random() * 34);
      body += `<rect x="${n(cursor)}" y="${n(y + 21)}" width="${n(size - 3)}" height="16" rx="4" fill="${colour}" opacity="0.92"/>`;
      cursor += size;
    }
  });
  // The budget gauge under the stack.
  const gauge = stack.y + sections.length * stack.row + 18;
  body += `<rect x="${stack.x}" y="${n(gauge)}" width="${stack.width}" height="8" rx="4" fill="#ffffff" fill-opacity="0.08"/>`;
  body += `<rect x="${stack.x}" y="${n(gauge)}" width="${n(stack.width * 0.96)}" height="8" rx="4" fill="url(#budget)" filter="url(#glow)"/>`;
  const defs = `<linearGradient id="budget" x1="0" y1="0" x2="1" y2="0"><stop offset="0" stop-color="#f5d0fe"/><stop offset="0.6" stop-color="#a78bfa"/><stop offset="1" stop-color="#67e8f9"/></linearGradient>`;
  const labels: Label[] = sections.map(([section, colour], at) => ({
    x: stack.x,
    y: stack.y + at * stack.row - 4,
    text: `## ${section}`,
    colour,
    size: 14,
    mono: true,
  }));
  labels.push(
    { x: stack.x, y: gauge + 16, text: "a token budget · quotas · MMR", colour: "#c4b5fd", size: 13, mono: true },
    { x: stack.x, y: gauge + 38, text: "same inputs → same pack.hash", colour: "#94a3b8", size: 13, mono: true },
    { x: 40, y: 36, text: "CANDIDATES", colour: "#94a3b8", size: 13, mono: true },
    { x: stack.x, y: 50, text: "THE PACK", colour: "#ddd6fe", size: 13, mono: true },
  );
  return { svg: svg(w, h, body, defs), labels };
}

// ─── 04 · Learning: a posterior moving ───────────────────────────────────────

function lnGamma(z: number): number {
  const g = 7;
  const c = [
    0.99999999999980993, 676.5203681218851, -1259.1392167224028, 771.32342877765313, -176.61502916214059,
    12.507343278686905, -0.13857109526572012, 9.9843695780195716e-6, 1.5056327351493116e-7,
  ];
  if (z < 0.5) return Math.log(Math.PI / Math.abs(Math.sin(Math.PI * z))) - lnGamma(1 - z);
  const x = z - 1;
  let sum = c[0] as number;
  for (let i = 1; i < g + 2; i += 1) sum += (c[i] as number) / (x + i);
  const t = x + g + 0.5;
  return 0.5 * Math.log(2 * Math.PI) + (x + 0.5) * Math.log(t) - t + Math.log(sum);
}

const betaPdf = (p: number, a: number, b: number) =>
  Math.exp((a - 1) * Math.log(p) + (b - 1) * Math.log(1 - p) - (lnGamma(a) + lnGamma(b) - lnGamma(a + b)));

export function learningArt(w: number, h: number): Art {
  const left = 40;
  const right = w - 222;
  const top = 80;
  const bottom = h - 130;
  const x = (p: number) => left + (right - left) * p;
  const peak = 6.2;
  const y = (density: number) => bottom - (bottom - top) * Math.min(1, density / peak);
  const curves = [
    { a: 0.5, b: 0.5, colour: "#64748b", width: 2, fill: 0 },
    { a: 2.5, b: 1.5, colour: "#7dd3fc", width: 2.5, fill: 0.1 },
    { a: 6.5, b: 1.5, colour: "#38bdf8", width: 3, fill: 0.14 },
    { a: 14.5, b: 2.5, colour: "#e0f2fe", width: 4, fill: 0.22 },
    { a: 6.5, b: 6.5, colour: "#fb7185", width: 3, fill: 0.12 },
  ];
  let body = `<line x1="${left}" y1="${n(bottom)}" x2="${right}" y2="${n(bottom)}" stroke="#334155" stroke-width="1.2"/>`;
  for (const threshold of [0.35, 0.7]) {
    body += `<line x1="${n(x(threshold))}" y1="${top - 20}" x2="${n(x(threshold))}" y2="${n(bottom)}" stroke="${threshold > 0.5 ? "#38bdf8" : "#fb7185"}" stroke-opacity="0.6" stroke-width="1.5" stroke-dasharray="5 6"/>`;
  }
  for (const curve of curves) {
    let d = "";
    for (let step = 1; step < 200; step += 1) {
      const p = step / 200;
      d += `${d ? " L" : "M"} ${n(x(p))} ${n(y(betaPdf(p, curve.a, curve.b)))}`;
    }
    if (curve.fill > 0) {
      body += `<path d="M ${n(x(0.005))} ${n(bottom)} L${d.slice(1)} L ${n(x(0.995))} ${n(bottom)} Z" fill="${curve.colour}" fill-opacity="${curve.fill}"/>`;
    }
    body += `<path d="${d}" stroke="${curve.colour}" stroke-width="${curve.width}" fill="none"${curve.width > 2 ? ' filter="url(#glow)"' : ""}/>`;
  }
  // The ladder of trust classes, with their starting confidences.
  const ladder = [
    ["human_explicit", 0.85],
    ["peer_human_attested", 0.75],
    ["agent_validated", 0.65],
    ["agent_assertion", 0.5],
    ["cass_evidence", 0.45],
    ["legacy_import", 0.3],
  ] as const;
  const lx = w - 196;
  const labels: Label[] = [];
  ladder.forEach(([name, initial], at) => {
    const ly = top - 10 + at * 52;
    body += `<rect x="${lx}" y="${ly}" width="172" height="40" rx="10" fill="#38bdf8" fill-opacity="${0.04 + (5 - at) * 0.035}" stroke="#7dd3fc" stroke-opacity="${0.15 + (5 - at) * 0.08}"/>`;
    body += `<rect x="${lx + 10}" y="${ly + 28}" width="${n(152 * initial)}" height="4" rx="2" fill="#7dd3fc"/>`;
    labels.push({ x: lx + 10, y: ly + 5, text: name, colour: "#e0f2fe", size: 13, mono: true });
  });
  labels.push({ x: lx, y: top - 36, text: "TRUST CLASSES", colour: "#7dd3fc", size: 12, mono: true });
  labels.push(
    { x: x(0.7) + 8, y: top - 26, text: "promote", colour: "#7dd3fc", size: 13, mono: true },
    { x: x(0.35) - 8, y: top - 26, text: "demote", colour: "#fda4af", size: 13, mono: true, anchor: "end" },
    { x: x(0.5), y: y(betaPdf(0.5, 6.5, 6.5)) - 30, text: "harmful × 2.5", colour: "#fda4af", size: 15, mono: true, anchor: "middle" },
    { x: x(0.8) - 10, y: y(betaPdf(0.86, 14.5, 2.5)) + 6, text: "helpful", colour: "#e0f2fe", size: 15, mono: true, anchor: "end" },
    { x: left, y: bottom + 16, text: "0", colour: "#64748b", size: 13, mono: true },
    { x: right, y: bottom + 16, text: "1", colour: "#64748b", size: 13, mono: true, anchor: "end" },
    { x: (left + right) / 2, y: bottom + 16, text: "confidence", colour: "#64748b", size: 13, mono: true, anchor: "middle" },
  );
  return { svg: svg(w, h, body), labels };
}

// ─── 05 · Campaigns: a search space, pruned ───────────────────────────────────

export function campaignsArt(w: number, h: number): Art {
  const random = seeded(5);
  const cols = 21;
  const rows = 12;
  const cell = 22;
  const gap = 4;
  const ox = w - 40 - cols * (cell + gap);
  const oy = 64;
  // Families of dead ends: rectangles of the grid, each pruned once enough of it failed.
  const families = [
    { c0: 1, r0: 1, c1: 6, r1: 5 },
    { c0: 9, r0: 6, c1: 15, r1: 10 },
    { c0: 14, r0: 0, c1: 19, r1: 3 },
    { c0: 2, r0: 8, c1: 6, r1: 11 },
  ];
  const inFamily = (c: number, r: number) => families.some((f) => c >= f.c0 && c <= f.c1 && r >= f.r0 && r <= f.r1);
  const win = { c: 17, r: 8 };
  let body = "";
  for (let r = 0; r < rows; r += 1) {
    for (let c = 0; c < cols; c += 1) {
      const x = ox + c * (cell + gap);
      const y = oy + r * (cell + gap);
      const pruned = inFamily(c, r);
      const tried = pruned ? random() > 0.72 : random() > 0.9;
      if (c === win.c && r === win.r) continue;
      body += `<rect x="${x}" y="${y}" width="${cell}" height="${cell}" rx="5" fill="${pruned ? "url(#hatch)" : "#ffffff"}" fill-opacity="${pruned ? 1 : 0.045}" stroke="${pruned ? "#fb923c" : "#ffffff"}" stroke-opacity="${pruned ? 0.35 : 0.06}"/>`;
      if (tried) {
        body += `<path d="M ${x + 6} ${y + 6} L ${x + cell - 6} ${y + cell - 6} M ${x + cell - 6} ${y + 6} L ${x + 6} ${y + cell - 6}" stroke="${pruned ? "#fdba74" : "#94a3b8"}" stroke-width="2.2" stroke-linecap="round"/>`;
      }
    }
  }
  for (const family of families) {
    const x = ox + family.c0 * (cell + gap) - 3;
    const y = oy + family.r0 * (cell + gap) - 3;
    const fw = (family.c1 - family.c0 + 1) * (cell + gap) + 2;
    const fh = (family.r1 - family.r0 + 1) * (cell + gap) + 2;
    body += `<rect x="${x}" y="${y}" width="${fw}" height="${fh}" rx="9" fill="none" stroke="#fb923c" stroke-width="2" stroke-opacity="0.85"/>`;
  }
  const wx = ox + win.c * (cell + gap);
  const wy = oy + win.r * (cell + gap);
  body += `<circle cx="${wx + cell / 2}" cy="${wy + cell / 2}" r="34" fill="#a3e635" opacity="0.5" filter="url(#haze)"/>`;
  body += `<rect x="${wx}" y="${wy}" width="${cell}" height="${cell}" rx="5" fill="#d9f99d" filter="url(#glow)"/>`;

  // Underneath: rules accrue in steps over the weeks; the pack stays flat.
  const left = ox;
  const right = ox + cols * (cell + gap) - gap;
  const base = h - 104;
  // As tall as fits under the grid's caption: on the 600-high X card a full 92 puts "procedural rules" on it.
  const caption = oy + rows * (cell + gap) + 4;
  const height = Math.min(92, base - caption - 18 - 26 - 8);
  const steps = [0, 0, 1, 1, 1, 2, 2, 3, 3, 3, 4, 5, 5, 5, 6, 6, 7, 7, 7, 8, 8, 8, 9, 9, 10, 10];
  let stair = "";
  steps.forEach((value, week) => {
    const x0 = left + ((right - left) * week) / steps.length;
    const x1 = left + ((right - left) * (week + 1)) / steps.length;
    const y = base - (height * value) / 10;
    stair += `${stair ? " L" : "M"} ${n(x0)} ${n(y)} L ${n(x1)} ${n(y)}`;
  });
  body += `<path d="${stair} L ${n(right)} ${base} L ${n(left)} ${base} Z" fill="#fb923c" fill-opacity="0.14"/>`;
  body += `<path d="${stair}" stroke="#fb923c" stroke-width="3" fill="none" filter="url(#glow)"/>`;
  body += `<line x1="${n(left)}" y1="${n(base - height * 0.32)}" x2="${n(right)}" y2="${n(base - height * 0.32)}" stroke="#c4b5fd" stroke-width="2.5" stroke-dasharray="7 6"/>`;
  body += `<line x1="${n(left)}" y1="${base}" x2="${n(right)}" y2="${base}" stroke="#334155"/>`;
  const defs =
    '<pattern id="hatch" width="7" height="7" patternUnits="userSpaceOnUse" patternTransform="rotate(45)"><rect width="7" height="7" fill="#fb923c" fill-opacity="0.12"/><line x1="0" y1="0" x2="0" y2="7" stroke="#fb923c" stroke-opacity="0.5" stroke-width="2.4"/></pattern>';
  return {
    svg: svg(w, h, body, defs),
    labels: [
      { x: ox, y: oy - 32, text: "THE SEARCH SPACE · 26 WEEKS", colour: "#94a3b8", size: 13, mono: true },
      { x: wx + cell / 2, y: wy + cell + 8, text: "found", colour: "#d9f99d", size: 14, mono: true, anchor: "middle" },
      { x: ox, y: caption, text: "hatched: a family of dead ends, pruned by one anti-pattern", colour: "#fdba74", size: 13, mono: true },
      { x: right, y: base - height - 26, text: "procedural rules", colour: "#fdba74", size: 14, mono: true, anchor: "end" },
      { x: left + 4, y: base - height * 0.32 - 24, text: "pack size: flat", colour: "#c4b5fd", size: 14, mono: true },
      { x: left, y: base + 10, text: "week 1", colour: "#64748b", size: 13, mono: true },
      { x: right, y: base + 10, text: "week 26", colour: "#64748b", size: 13, mono: true, anchor: "end" },
    ],
  };
}

// ─── 07 · Graph: communities, a hub, a bridge ─────────────────────────────────

export function graphArt(w: number, h: number): Art {
  const random = seeded(19);
  const communities = [
    { x: 0.27, y: 0.3, colour: "#34d399", size: 10 },
    { x: 0.73, y: 0.26, colour: "#38bdf8", size: 9 },
    { x: 0.3, y: 0.74, colour: "#a78bfa", size: 9 },
    { x: 0.76, y: 0.72, colour: "#fbbf24", size: 8 },
  ];
  type Node = { x: number; y: number; colour: string; group: number; degree: number };
  const nodes: Node[] = [];
  communities.forEach((group, g) => {
    for (let i = 0; i < group.size; i += 1) {
      const angle = (i / group.size) * Math.PI * 2 + random() * 0.6;
      const radius = 0.05 + random() * 0.09;
      nodes.push({ x: (group.x + Math.cos(angle) * radius) * w, y: (group.y + Math.sin(angle) * radius * 1.2) * h, colour: group.colour, group: g, degree: 0 });
    }
  });
  // The articulation point: the only way between the communities.
  const bridge: Node = { x: w * 0.52, y: h * 0.5, colour: "#ffffff", group: -1, degree: 0 };
  nodes.push(bridge);
  const edges: [number, number][] = [];
  for (let a = 0; a < nodes.length - 1; a += 1) {
    for (let b = a + 1; b < nodes.length - 1; b += 1) {
      const na = nodes[a] as Node;
      const nb = nodes[b] as Node;
      if (na.group === nb.group && random() > 0.62) edges.push([a, b]);
    }
  }
  // One node from each community is tied to the bridge.
  let start = 0;
  communities.forEach((group) => {
    edges.push([start, nodes.length - 1]);
    edges.push([start + 1, nodes.length - 1]);
    start += group.size;
  });
  for (const [a, b] of edges) {
    (nodes[a] as Node).degree += 1;
    (nodes[b] as Node).degree += 1;
  }
  let body = "";
  for (const group of communities) {
    body += `<circle cx="${n(group.x * w)}" cy="${n(group.y * h)}" r="92" fill="${group.colour}" opacity="0.16" filter="url(#haze)"/>`;
  }
  for (const [a, b] of edges) {
    const na = nodes[a] as Node;
    const nb = nodes[b] as Node;
    const toBridge = b === nodes.length - 1;
    body += `<line x1="${n(na.x)}" y1="${n(na.y)}" x2="${n(nb.x)}" y2="${n(nb.y)}" stroke="${toBridge ? "#ffffff" : na.colour}" stroke-opacity="${toBridge ? 0.55 : 0.32}" stroke-width="${toBridge ? 1.8 : 1.2}"/>`;
  }
  let hub = nodes[0] as Node;
  for (const node of nodes) if (node !== bridge && node.degree > hub.degree) hub = node;
  for (const node of nodes) {
    const r = node === bridge ? 11 : 3.5 + node.degree * 0.9;
    body += `<circle cx="${n(node.x)}" cy="${n(node.y)}" r="${n(r)}" fill="${node.colour}"${node === bridge || node === hub ? ' filter="url(#glow)"' : ""}/>`;
  }
  body += `<circle cx="${n(bridge.x)}" cy="${n(bridge.y)}" r="22" fill="none" stroke="#ffffff" stroke-width="2" stroke-opacity="0.8"/>`;
  body += `<circle cx="${n(hub.x)}" cy="${n(hub.y)}" r="${n(4 + hub.degree * 0.9 + 9)}" fill="none" stroke="${hub.colour}" stroke-width="2"/>`;
  return {
    svg: svg(w, h, body),
    labels: [
      { x: bridge.x, y: bridge.y + 28, text: "articulation point", colour: "#ffffff", size: 15, mono: true, anchor: "middle" },
      { x: hub.x, y: hub.y - 52, text: "PageRank hub", colour: hub.colour, size: 15, mono: true, anchor: "middle" },
    ],
  };
}

/** The faint lattice behind the text-led cards (For agents, Proof). */
export function latticeArt(w: number, h: number, colour: string): Art {
  let body = "";
  for (let x = 0; x <= w; x += 40) body += `<line x1="${x}" y1="0" x2="${x}" y2="${h}" stroke="${colour}" stroke-opacity="0.07"/>`;
  for (let y = 0; y <= h; y += 40) body += `<line x1="0" y1="${y}" x2="${w}" y2="${y}" stroke="${colour}" stroke-opacity="0.07"/>`;
  return { svg: svg(w, h, body), labels: [] };
}
