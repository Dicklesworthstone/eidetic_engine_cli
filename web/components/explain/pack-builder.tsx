"use client";

import { type CSSProperties, useMemo, useState } from "react";

import { DemoFrame, Readout, Segmented, Slider } from "@/components/explain/demo";
import { Cmd } from "@/components/instrument";
import { type PackSection, SECTION_COLOUR } from "@/lib/palette";
import { cn, grouped } from "@/lib/utils";

// ─── The toy candidates ──────────────────────────────────────────────────────
// Nineteen memories a search for "prepare the v0.20 release" might return,
// already scored. Three are near-duplicates of a stronger memory, two are
// irrelevant, one is weak. Token counts are toy sizes. Nothing here is ee's data.

type Level = "procedural" | "semantic" | "episodic";

interface Memory {
  id: string;
  section: PackSection;
  level: Level;
  kind: string;
  /** The memory's first tag: with level and kind it makes ee's diversity key. */
  tag: string;
  relevance: number;
  tokens: number;
  text: string;
}

const TASK = "prepare the v0.20 release";

const MEMORIES: readonly Memory[] = [
  { id: "m01", section: "rules", level: "procedural", kind: "rule", tag: "release", relevance: 0.93, tokens: 60, text: "Run cargo fmt --check and the clippy gate before tagging." },
  { id: "m02", section: "failures", level: "episodic", kind: "failure", tag: "changelog", relevance: 0.9, tokens: 110, text: "Tag pushed before CHANGELOG.md was updated; the installer checksum went stale for a day." },
  { id: "m03", section: "rules", level: "procedural", kind: "rule", tag: "changelog", relevance: 0.89, tokens: 70, text: "Bump Cargo.toml and CHANGELOG.md in the same commit as the version tag." },
  { id: "m04", section: "rules", level: "procedural", kind: "rule", tag: "release", relevance: 0.87, tokens: 62, text: "Before tagging, run cargo fmt --check and the clippy gate." },
  { id: "m05", section: "failures", level: "episodic", kind: "failure", tag: "changelog", relevance: 0.85, tokens: 105, text: "Pushing the tag before the CHANGELOG.md update left the installer checksum stale." },
  { id: "m06", section: "decisions", level: "semantic", kind: "decision", tag: "release", relevance: 0.84, tokens: 90, text: "Releases are cut locally with dsr, never with GitHub Actions." },
  { id: "m07", section: "decisions", level: "semantic", kind: "decision", tag: "release", relevance: 0.8, tokens: 88, text: "Cut releases locally with dsr; GitHub Actions is never used for releases." },
  { id: "m08", section: "failures", level: "episodic", kind: "failure", tag: "installer", relevance: 0.78, tokens: 120, text: "The Homebrew tap was refreshed before the assets verified and served a 404 for a day." },
  { id: "m09", section: "rules", level: "procedural", kind: "rule", tag: "crates", relevance: 0.74, tokens: 80, text: "Publish to crates.io only after the GitHub release assets verify." },
  { id: "m10", section: "decisions", level: "semantic", kind: "decision", tag: "installer", relevance: 0.71, tokens: 140, text: "Installers verify SHA-256 sidecars and minisign signatures; Sigstore bundles are not published yet." },
  { id: "m11", section: "evidence", level: "episodic", kind: "evidence", tag: "release", relevance: 0.69, tokens: 180, text: "cass session, v0.19.0 release run: 1,647 commits, cargo audit advisories 12 to 0, all platform archives signed." },
  { id: "m12", section: "artifacts", level: "semantic", kind: "artifact", tag: "release", relevance: 0.66, tokens: 95, text: "scripts/release.sh: the release checklist, step by step, with its verification gates." },
  { id: "m13", section: "evidence", level: "episodic", kind: "evidence", tag: "build", relevance: 0.64, tokens: 160, text: "cass session: dsr build matrix finished five of five targets and uploaded every checksum sidecar." },
  { id: "m14", section: "artifacts", level: "semantic", kind: "artifact", tag: "docs", relevance: 0.62, tokens: 120, text: "docs/releasing.md: the human-readable release guide and its rollback section." },
  { id: "m15", section: "evidence", level: "episodic", kind: "evidence", tag: "tests", relevance: 0.45, tokens: 210, text: "cass session: a flaky daemon test retried twice before the release gate went green." },
  { id: "m16", section: "artifacts", level: "semantic", kind: "artifact", tag: "installer", relevance: 0.41, tokens: 70, text: "install.sh: the installer, with --verify and --easy-mode." },
  { id: "m17", section: "rules", level: "procedural", kind: "rule", tag: "release", relevance: 0.04, tokens: 50, text: "Prefer early returns over nested if-blocks in release handlers." },
  { id: "m18", section: "evidence", level: "episodic", kind: "evidence", tag: "build", relevance: 0.03, tokens: 150, text: "cass session: a long discussion of terminal colour themes for the build dashboard." },
  { id: "m19", section: "evidence", level: "episodic", kind: "evidence", tag: "release", relevance: 0.12, tokens: 40, text: "cass session: the v0.18.0 release notes were drafted in the same session as the tag." },
];

// ─── ee's rule (src/pack/mod.rs), at toy scale ───────────────────────────────

/** DEFAULT_MMR_RELEVANCE_WEIGHT. */
const RELEVANCE_WEIGHT = 0.75;
/** DEFAULT_COVERAGE_FILL_RELEVANCE_FLOOR. */
const FLOOR = 0.05;
/** PackLodBudgetShares::default_70_20_10. */
const LOD_BPS = { full: 7000, preview: 2000, link: 1000 } as const;
/** A link-only item's body is `Memory <id>`: a few tokens whatever the memory's size (toy estimate). */
const LINK_TOKENS = 14;
/** Below this a truncated preview has no room for a word and its " ..." (toy). */
const MIN_PREVIEW = 6;

const SECTIONS: readonly PackSection[] = ["rules", "decisions", "failures", "evidence", "artifacts"];
const SECTION_KEY: Record<PackSection, string> = {
  rules: "procedural_rules",
  decisions: "decisions",
  failures: "failures",
  evidence: "evidence",
  artifacts: "artifacts",
};

/** Built-in profiles' section mixes in basis points (src/models/context_profile.rs). */
const PROFILES = {
  compact: { rules: 5000, decisions: 1500, failures: 2000, evidence: 1000, artifacts: 500 },
  balanced: { rules: 3000, decisions: 2000, failures: 2000, evidence: 2000, artifacts: 1000 },
  thorough: { rules: 2000, decisions: 2000, failures: 2000, evidence: 2500, artifacts: 1500 },
} as const satisfies Record<string, Record<PackSection, number>>;
type Profile = keyof typeof PROFILES;

type Tier = "full" | "preview" | "link";
type Phase = "antiPatternFirst" | "mmr" | "coverageFill";
type Mode = "mmr" | "relevance";

export type Outcome =
  | {
      kind: "selected";
      rank: number;
      phase: Phase;
      gain: number;
      maxSim: number;
      simWith: string | null;
      tier: Tier;
      cost: number;
    }
  | {
      kind: "omitted";
      reason: "token_budget_exceeded" | "redundant_candidate" | "below_relevance_floor";
      gain: number;
      maxSim: number;
      simWith: string | null;
      binding?: "quota" | "budget" | "tiers";
      detail: string;
    };

/** normalized_terms: whitespace-split, trimmed of non-alphanumerics at both ends, lowercased, sorted, deduped. */
function terms(text: string): string[] {
  const out = text
    .split(/\s+/)
    .map((term) => term.replace(/^[^A-Za-z0-9]+|[^A-Za-z0-9]+$/g, "").toLowerCase())
    .filter(Boolean);
  return [...new Set(out)].sort();
}

const TERMS = new Map(MEMORIES.map((memory) => [memory.id, terms(memory.text)]));
const diversityKey = (memory: Memory) => `${memory.level}:${memory.kind}:${memory.tag}`;

/** candidate_signature_similarity. */
function similarity(left: Memory, right: Memory): number {
  if (left.id === right.id) return 1;
  const sameKey = diversityKey(left) === diversityKey(right);
  if (left.text.split(/\s+/).join(" ") === right.text.split(/\s+/).join(" ")) return sameKey ? 1 : 0.5;
  const a = TERMS.get(left.id) ?? [];
  const b = new Set(TERMS.get(right.id) ?? []);
  const intersection = a.filter((term) => b.has(term)).length;
  const union = a.length + b.size - intersection;
  const jaccard = union === 0 ? 0 : intersection / union;
  // A shared diversity key raises similarity into [0.5, 0.95]: related, never a duplicate by tag alone.
  return sameKey ? Math.min(0.95, Math.max(0.5, jaccard)) : jaccard;
}

const SECTION_ORDER: Record<PackSection, number> = { rules: 0, decisions: 1, failures: 2, evidence: 3, artifacts: 4 };

/** compare_candidates: relevance, then (equal toy utility) section, then id. */
const compare = (a: Memory, b: Memory) =>
  b.relevance - a.relevance || SECTION_ORDER[a.section] - SECTION_ORDER[b.section] || a.id.localeCompare(b.id);

export interface Assembly {
  outcomes: Map<string, Outcome>;
  order: string[];
  usedTokens: number;
  sectionUsed: Record<PackSection, number>;
  quotas: Record<PackSection, number>;
  lodLimits: Record<Tier, number>;
  lodUsed: Record<Tier, number>;
}

/** ee's MMR pack assembly (assemble_draft_with_profile_and_options_seeded), on the toy candidates. */
export function assemble(profile: Profile, budget: number, mode: Mode): Assembly {
  const mix = PROFILES[profile];
  // quota_for_basis_points: ceil(budget × bps / 10000), at least 1.
  const quotas = Object.fromEntries(
    SECTIONS.map((section) => [section, Math.max(1, Math.ceil((budget * mix[section]) / 10000))]),
  ) as Record<PackSection, number>;
  // PackLodBudgetShares::limits: floor shares, each bounded by what the earlier tiers left.
  const full = Math.min(budget, Math.max(1, Math.floor((budget * LOD_BPS.full) / 10000)));
  const preview = Math.min(budget - full, Math.max(1, Math.floor((budget * LOD_BPS.preview) / 10000)));
  const link = Math.min(budget - full - preview, Math.max(1, Math.floor((budget * LOD_BPS.link) / 10000)));
  const lodLimits: Record<Tier, number> = { full, preview, link };
  const lodUsed: Record<Tier, number> = { full: 0, preview: 0, link: 0 };
  const sectionUsed: Record<PackSection, number> = { rules: 0, decisions: 0, failures: 0, evidence: 0, artifacts: 0 };
  let used = 0;
  let rank = 0;
  const outcomes = new Map<string, Outcome>();
  const order: string[] = [];
  const selected: Memory[] = [];
  let remaining = [...MEMORIES].sort(compare);

  const maxSimilarity = (memory: Memory): { value: number; with: string | null } => {
    let value = 0;
    let best: string | null = null;
    for (const chosen of selected) {
      const sim = similarity(memory, chosen);
      if (sim > value) {
        value = sim;
        best = chosen.id;
      }
    }
    return { value, with: best };
  };
  const gainOf = (memory: Memory, maxSim: number) =>
    mode === "relevance" ? memory.relevance : maxSim >= 1 ? 0 : RELEVANCE_WEIGHT * memory.relevance - (1 - RELEVANCE_WEIGHT) * maxSim;

  /** pack_lod_candidate_plan: the first tier whose rendering fits every binding limit. */
  const plan = (memory: Memory): { tier: Tier; cost: number } | null => {
    const available = Math.min(budget - used, quotas[memory.section] - sectionUsed[memory.section]);
    for (const tier of ["full", "preview", "link"] as const) {
      const room = Math.min(lodLimits[tier] - lodUsed[tier], available);
      if (room <= 0) continue;
      if (tier === "full" && memory.tokens <= room) return { tier, cost: memory.tokens };
      if (tier === "preview" && room >= MIN_PREVIEW && memory.tokens > 2)
        return { tier, cost: Math.min(memory.tokens - 2, room) };
      if (tier === "link" && LINK_TOKENS <= room) return { tier, cost: LINK_TOKENS };
    }
    return null;
  };
  /**
   * Which limit refused it. ee reports all three as token_budget_exceeded with a minimal-budget hint; the page
   * names the one that bound: the section's quota, the pack's budget, or the level-of-detail tiers (full text,
   * preview and link each have their own share of the budget).
   */
  const binding = (memory: Memory): "quota" | "budget" | "tiers" => {
    const quotaLeft = quotas[memory.section] - sectionUsed[memory.section];
    const budgetLeft = budget - used;
    const available = Math.min(quotaLeft, budgetLeft);
    // Room enough for at least a preview, yet no tier took it: the tiers' own shares were spent.
    if (available >= MIN_PREVIEW) return "tiers";
    return quotaLeft <= budgetLeft ? "quota" : "budget";
  };
  const take = (memory: Memory, phase: Phase, gain: number, sim: { value: number; with: string | null }) => {
    const chosen = plan(memory);
    if (!chosen) return false;
    rank += 1;
    used += chosen.cost;
    sectionUsed[memory.section] += chosen.cost;
    lodUsed[chosen.tier] += chosen.cost;
    selected.push(memory);
    order.push(memory.id);
    outcomes.set(memory.id, { kind: "selected", rank, phase, gain, maxSim: sim.value, simWith: sim.with, tier: chosen.tier, cost: chosen.cost });
    return true;
  };
  /** Each tier's refusal, in words: how much room it had and what limited that room. */
  const tierDetail = (memory: Memory): string => {
    const quotaLeft = quotas[memory.section] - sectionUsed[memory.section];
    const budgetLeft = budget - used;
    const available = Math.min(quotaLeft, budgetLeft);
    const limiter = quotaLeft <= budgetLeft ? `${SECTION_KEY[memory.section]} quota` : "pack budget";
    const fullLeft = lodLimits.full - lodUsed.full;
    const fullRoom = Math.min(fullLeft, available);
    return [
      `full: room for ${Math.max(0, fullRoom)} of its ${memory.tokens} (${fullLeft < available ? "full-text tier" : limiter})`,
      `preview tier ${lodUsed.preview}/${lodLimits.preview}`,
      `link tier ${lodUsed.link}/${lodLimits.link}`,
    ].join(" · ");
  };
  const budgetMiss = (memory: Memory, gain: number, sim: { value: number; with: string | null }) => {
    const bound = binding(memory);
    outcomes.set(memory.id, {
      kind: "omitted",
      reason: "token_budget_exceeded",
      gain,
      maxSim: sim.value,
      simWith: sim.with,
      binding: bound,
      detail:
        bound === "quota"
          ? `${SECTION_KEY[memory.section]} quota spent: ${sectionUsed[memory.section]} of ${quotas[memory.section]}`
          : bound === "budget"
            ? `pack budget spent: ${used} of ${budget}`
            : tierDetail(memory),
    });
  };

  // Anti-pattern first: the best failure above the floor is reserved before anything else.
  const reserved = remaining.find((memory) => memory.section === "failures" && memory.relevance >= FLOOR && plan(memory));
  if (reserved) {
    const sim = maxSimilarity(reserved);
    take(reserved, "antiPatternFirst", gainOf(reserved, sim.value), sim);
    remaining = remaining.filter((memory) => memory.id !== reserved.id);
  }

  // Strict MMR: always the candidate of greatest marginal gain; a gain of zero or less waits for coverage fill.
  const coveragePool: { memory: Memory; gain: number; sim: { value: number; with: string | null } }[] = [];
  while (remaining.length > 0) {
    let bestIndex = 0;
    let bestGain = Number.NEGATIVE_INFINITY;
    let bestSim = { value: 0, with: null as string | null };
    remaining.forEach((memory, index) => {
      const sim = maxSimilarity(memory);
      const gain = gainOf(memory, sim.value);
      if (gain > bestGain || (gain === bestGain && compare(memory, remaining[bestIndex]!) < 0)) {
        bestIndex = index;
        bestGain = gain;
        bestSim = sim;
      }
    });
    const memory = remaining[bestIndex]!;
    remaining = remaining.filter((_, index) => index !== bestIndex);
    if (bestGain <= 0) {
      coveragePool.push({ memory, gain: bestGain, sim: bestSim });
      continue;
    }
    if (!take(memory, "mmr", bestGain, bestSim)) budgetMiss(memory, bestGain, bestSim);
  }

  // Coverage fill: by relevance, above the floor, at most as many as the MMR phase chose.
  const limit = order.length;
  let filled = 0;
  for (const { memory, gain, sim } of coveragePool.sort((a, b) => compare(a.memory, b.memory))) {
    if (memory.relevance < FLOOR) {
      outcomes.set(memory.id, {
        kind: "omitted",
        reason: "below_relevance_floor",
        gain,
        maxSim: sim.value,
        simWith: sim.with,
        detail: `relevance ${memory.relevance.toFixed(2)} is under the ${FLOOR} floor`,
      });
      continue;
    }
    if (filled >= limit) {
      outcomes.set(memory.id, {
        kind: "omitted",
        reason: "redundant_candidate",
        gain,
        maxSim: sim.value,
        simWith: sim.with,
        detail: "coverage fill is full",
      });
      continue;
    }
    const now = maxSimilarity(memory);
    if (take(memory, "coverageFill", gainOf(memory, now.value), now)) filled += 1;
    else budgetMiss(memory, gain, sim);
  }

  return { outcomes, order, usedTokens: used, sectionUsed, quotas, lodLimits, lodUsed };
}

// ─── Presentation ────────────────────────────────────────────────────────────

const SECTION_LABEL: Record<PackSection, string> = {
  rules: "Rule",
  decisions: "Decision",
  failures: "Failure",
  evidence: "Evidence",
  artifacts: "Artifact",
};

const BY_ID = new Map(MEMORIES.map((memory) => [memory.id, memory]));
const RANKED = [...MEMORIES].sort(compare);

function chip(outcome: Outcome): { text: string; className: string } {
  if (outcome.kind === "selected") {
    const phase =
      outcome.phase === "antiPatternFirst" ? "what NOT to do" : outcome.phase === "coverageFill" ? "coverage fill" : `gain ${outcome.gain.toFixed(2)}`;
    const tier = outcome.tier === "full" ? "" : ` · ${outcome.tier}`;
    return {
      text: `#${outcome.rank} · ${phase}${tier}`,
      className:
        outcome.phase === "antiPatternFirst"
          ? "border-rose-300/40 bg-rose-400/15 text-rose-100"
          : outcome.phase === "coverageFill"
            ? "border-cyan-300/35 bg-cyan-400/10 text-cyan-100"
            : "border-violet-300/45 bg-violet-400/15 text-violet-100",
    };
  }
  if (outcome.reason === "token_budget_exceeded")
    return {
      text: outcome.binding === "quota" ? "quota full" : outcome.binding === "tiers" ? "tiers spent" : "over budget",
      className: "border-amber-300/30 bg-amber-400/[0.08] text-amber-200",
    };
  if (outcome.reason === "redundant_candidate")
    return { text: "redundant", className: "border-white/15 bg-white/[0.04] text-slate-300" };
  return { text: "below floor", className: "border-white/10 bg-transparent text-slate-500" };
}

/** The arithmetic behind one candidate's fate, the way `ee why-not` would put it. */
function explanation(memory: Memory, outcome: Outcome, mode: Mode): string[] {
  const lines: string[] = [];
  const simNote =
    outcome.simWith !== null && outcome.maxSim > 0
      ? `max similarity ${outcome.maxSim.toFixed(2)} with ${outcome.simWith}${
          diversityKey(memory) === diversityKey(BY_ID.get(outcome.simWith) ?? memory) ? ` (same diversity key ${diversityKey(memory)})` : ""
        }`
      : "max similarity 0.00 (nothing similar chosen yet)";
  if (mode === "mmr" && !(outcome.kind === "selected" && outcome.phase === "antiPatternFirst")) {
    lines.push(
      `gain = 0.75 × ${memory.relevance.toFixed(2)} − 0.25 × ${outcome.maxSim.toFixed(2)} = ${outcome.gain.toFixed(3)}`,
      simNote,
    );
  } else if (mode === "relevance") {
    lines.push(`gain = relevance = ${memory.relevance.toFixed(2)} (redundancy ignored)`);
  }
  if (outcome.kind === "selected") {
    if (outcome.phase === "antiPatternFirst")
      lines.push("reserved first: the highest-relevance failure above the floor, rendered under “What NOT to do”");
    if (outcome.phase === "coverageFill")
      lines.push("its gain had fallen to zero or below, so it waited; coverage fill then admitted it because room was left");
    lines.push(
      outcome.tier === "full"
        ? `rendered in full: ${outcome.cost} tokens`
        : outcome.tier === "preview"
          ? `the full-text tier (70% of the budget) was spent: a ${outcome.cost}-token truncated preview of ${memory.tokens}`
          : `only a link fitted: “Memory ${memory.id}”, ${outcome.cost} tokens, in the peripheral index`,
    );
  } else {
    lines.push(`omitted: ${outcome.reason}${outcome.detail ? ` · ${outcome.detail}` : ""}`);
  }
  return lines;
}

function PackPreview({ assembly, profile, budget }: { assembly: Assembly; profile: Profile; budget: number }) {
  const groups = new Map<string, { label: string; items: { memory: Memory; outcome: Extract<Outcome, { kind: "selected" }> }[] }>();
  const links: Memory[] = [];
  let index = 0;
  for (const id of assembly.order) {
    const memory = BY_ID.get(id);
    const outcome = assembly.outcomes.get(id);
    if (!memory || !outcome || outcome.kind !== "selected") continue;
    if (outcome.tier === "link") {
      links.push(memory);
      continue;
    }
    const key = outcome.phase === "antiPatternFirst" ? "what_not_to_do" : SECTION_KEY[memory.section];
    const group = groups.get(key) ?? { label: key === "what_not_to_do" ? "What NOT to do" : key, items: [] };
    group.items.push({ memory, outcome });
    groups.set(key, group);
  }
  const previewText = (memory: Memory, cost: number) => {
    const words = memory.text.split(" ");
    const keep = Math.max(1, Math.min(words.length - 1, Math.round((words.length * cost) / memory.tokens)));
    return `${words.slice(0, keep).join(" ")} ...`;
  };
  return (
    <div
      className="max-h-[26rem] overflow-y-auto rounded-xl border border-white/[0.08] bg-black/40 p-4 font-mono text-[11.5px] leading-relaxed [scrollbar-width:thin]"
      aria-label="The rendered pack, as Markdown"
      tabIndex={0}
    >
      <p className="text-white"># Context Pack: {TASK}</p>
      <p className="mt-1 text-slate-500">
        **Profile:** {profile} | **Budget:** {assembly.usedTokens}/{budget} tokens
      </p>
      {[...groups.entries()].map(([key, group]) => (
        <div key={key} className="mt-3">
          <p className="text-[color:var(--chapter,#a78bfa)]">## {group.label}</p>
          {group.items.map(({ memory, outcome }) => {
            index += 1;
            return (
              <div key={memory.id} className="type-in mt-1.5">
                <p className="text-slate-400">
                  ### {index}. {memory.id} ({outcome.cost} tokens)
                </p>
                <p className="border-l-2 pl-2 text-slate-200" style={{ borderColor: SECTION_COLOUR[memory.section] }}>
                  {outcome.tier === "preview" ? previewText(memory, outcome.cost) : memory.text}
                </p>
              </div>
            );
          })}
        </div>
      ))}
      {links.length > 0 ? (
        <div className="mt-3">
          <p className="text-[color:var(--chapter,#a78bfa)]">## Peripheral Index</p>
          {links.map((memory) => (
            <p key={memory.id} className="type-in text-slate-400">
              - `{memory.id}` ({SECTION_KEY[memory.section]})
            </p>
          ))}
        </div>
      ) : null}
      {assembly.order.length === 0 ? <p className="mt-3 text-slate-500">*No items in pack.*</p> : null}
    </div>
  );
}

/**
 * The centerpiece: ee's pack selection run on nineteen toy candidates. Profile quotas, the token budget, the
 * reserved anti-pattern slot, strict MMR, coverage fill and the 70/20/10 level-of-detail tiers all follow
 * src/pack/mod.rs; every candidate says why it is in or out.
 */
export function PackBuilder() {
  const [profile, setProfile] = useState<Profile>("balanced");
  const [budget, setBudget] = useState(800);
  const [mode, setMode] = useState<Mode>("mmr");
  const [focus, setFocus] = useState("m04");

  const assembly = useMemo(() => assemble(profile, budget, mode), [profile, budget, mode]);
  const selectedCount = assembly.order.length;
  const tiers = { full: 0, preview: 0, link: 0 };
  let relevanceSum = 0;
  for (const id of assembly.order) {
    const outcome = assembly.outcomes.get(id);
    if (outcome?.kind === "selected") tiers[outcome.tier] += 1;
    relevanceSum += BY_ID.get(id)?.relevance ?? 0;
  }
  const omitted = MEMORIES.length - selectedCount;
  const duplicatesIn = ["m04", "m05", "m07"].filter((id) => assembly.outcomes.get(id)?.kind === "selected").length;

  return (
    <DemoFrame
      eyebrow="Pack assembly · nineteen candidates"
      title="Set a profile and a budget, and see what ee's rule packs."
      basis="rule"
      source={
        <>
          src/pack/mod.rs · DEFAULT_MMR_RELEVANCE_WEIGHT 0.75, DEFAULT_COVERAGE_FILL_RELEVANCE_FLOOR 0.05,
          strict_mmr_marginal_gain, candidate_signature_similarity, normalized_terms, quota_for_basis_points,
          pack_lod_candidate_plan, PackLodBudgetShares::default_70_20_10 · src/models/context_profile.rs ·
          built-in section mixes · src/core/context.rs · diversity_key_for_memory. Candidates, scores and token
          sizes are invented for this page; ee&apos;s submodular profile uses facility location instead of MMR. On a
          real store, run <span className="text-slate-300">ee pack &quot;&lt;task&gt;&quot; --explain --json</span> and{" "}
          <span className="text-slate-300">ee why-not &lt;memory-id&gt; --task &quot;&lt;task&gt;&quot;</span>.
        </>
      }
    >
      <p className="font-mono text-[12px] leading-relaxed break-words text-slate-400">
        <span className="text-slate-600">$ </span>
        <span className="text-violet-200">ee pack</span> &quot;{TASK}&quot; --profile {profile} --max-tokens {budget}
        {mode === "mmr" ? " --explain" : ""}
      </p>

      <div className="mt-5 grid grid-cols-1 gap-4 md:grid-cols-[auto_auto_minmax(0,1fr)] md:items-end">
        <div>
          <p className="mb-1.5 text-[9px] font-black tracking-[0.2em] text-slate-500 uppercase">Profile</p>
          <Segmented
            label="Context profile"
            value={profile}
            onChange={setProfile}
            options={[
              { value: "compact", label: "compact" },
              { value: "balanced", label: "balanced" },
              { value: "thorough", label: "thorough" },
            ]}
          />
        </div>
        <div>
          <p className="mb-1.5 text-[9px] font-black tracking-[0.2em] text-slate-500 uppercase">Selection</p>
          <Segmented
            label="Selection rule"
            value={mode}
            onChange={setMode}
            options={[
              { value: "mmr", label: "ee · MMR", tone: "rule" },
              { value: "relevance", label: "redundancy off · not ee", tone: "wrong" },
            ]}
          />
        </div>
        <Slider
          label="Token budget (toy scale)"
          value={budget}
          min={240}
          max={1600}
          step={40}
          onChange={setBudget}
          display={`${grouped(budget)} tokens`}
        />
      </div>

      <div className="mt-6 grid grid-cols-1 gap-6 lg:grid-cols-12">
        {/* The candidates, in the order search ranked them, each with its fate. */}
        <div className="min-w-0 lg:col-span-7">
          <div className="flex items-baseline justify-between gap-3">
            <p className="text-[9px] font-black tracking-[0.2em] text-slate-500 uppercase">
              Candidates · ranked by relevance
            </p>
            <p className="text-right text-[11px] text-slate-500">select one to see its arithmetic</p>
          </div>
          <ol className="mt-2 space-y-1">
            {RANKED.map((memory) => {
              const outcome = assembly.outcomes.get(memory.id);
              if (!outcome) return null;
              const status = chip(outcome);
              const on = focus === memory.id;
              const included = outcome.kind === "selected";
              return (
                <li
                  key={memory.id}
                  className={cn(
                    "rounded-xl border transition-colors",
                    on ? "border-violet-300/45 bg-violet-400/[0.06]" : "border-transparent hover:border-white/10 hover:bg-white/[0.025]",
                  )}
                  style={{ "--sphere": SECTION_COLOUR[memory.section] } as CSSProperties}
                >
                  <button
                    type="button"
                    aria-expanded={on}
                    aria-controls={`why-${memory.id}`}
                    onClick={() => setFocus(on ? "" : memory.id)}
                    className="block w-full rounded-xl px-3 py-2 text-left"
                  >
                    <span className="flex items-center justify-between gap-2">
                      <span className="flex min-w-0 items-center gap-2 font-mono text-[10px] text-slate-500 tabular-nums">
                        <span aria-hidden="true" className={cn("sphere h-2 w-2 shrink-0", !included && "opacity-40")} />
                        <span className="tracking-wide uppercase">{SECTION_LABEL[memory.section]}</span>
                        <span className="hidden min-[400px]:inline">{memory.id}</span>
                        <span aria-hidden="true" className="h-[3px] w-10 shrink-0 overflow-hidden rounded-full bg-white/[0.07] sm:w-16">
                          <span
                            className="block h-full rounded-full"
                            style={{ width: `${memory.relevance * 100}%`, background: SECTION_COLOUR[memory.section] }}
                          />
                        </span>
                        <span>{memory.relevance.toFixed(2)}</span>
                        <span className="hidden sm:inline">· {memory.tokens} tok</span>
                      </span>
                      <span
                        className={cn(
                          "shrink-0 rounded-full border px-2 py-0.5 font-mono text-[10px] whitespace-nowrap tabular-nums",
                          status.className,
                        )}
                      >
                        {status.text}
                      </span>
                    </span>
                    <span
                      className={cn(
                        "mt-1 block text-[13px] leading-snug",
                        on ? "" : "truncate",
                        included ? "text-slate-100" : "text-slate-400",
                      )}
                    >
                      {memory.text}
                    </span>
                  </button>
                  {on ? (
                    <div id={`why-${memory.id}`} className="type-in px-3 pb-3" aria-live="polite">
                      <p className="text-[9px] font-black tracking-[0.2em] text-violet-200/90 uppercase">
                        Why{outcome.kind === "selected" ? "" : " not"} · {memory.id} · {memory.tokens} tokens ·{" "}
                        {diversityKey(memory)}
                      </p>
                      <ul className="mt-1.5 space-y-1 font-mono text-[11.5px] leading-relaxed text-slate-400">
                        {explanation(memory, outcome, mode).map((line) => (
                          <li key={line} className="flex gap-2">
                            <span aria-hidden="true" className="text-violet-300/60">
                              ›
                            </span>
                            <span className="min-w-0 [overflow-wrap:anywhere]">{line}</span>
                          </li>
                        ))}
                      </ul>
                    </div>
                  ) : null}
                </li>
              );
            })}
          </ol>
        </div>

        {/* The pack: the budget it spent, each section against its quota, and the Markdown it renders. */}
        <div className="min-w-0 space-y-4 lg:sticky lg:top-24 lg:col-span-5 lg:self-start">
          <div>
            <div className="flex items-baseline justify-between gap-3">
              <p className="text-[9px] font-black tracking-[0.2em] text-slate-500 uppercase">Budget spent</p>
              <p className="font-mono text-[11px] text-slate-400 tabular-nums">
                {grouped(assembly.usedTokens)} / {grouped(budget)}
              </p>
            </div>
            <div
              className="mt-2 flex h-4 w-full overflow-hidden rounded-md border border-white/10 bg-black/40"
              role="img"
              aria-label={`${assembly.usedTokens} of ${budget} tokens spent on ${selectedCount} items`}
            >
              {assembly.order.map((id) => {
                const memory = BY_ID.get(id);
                const outcome = assembly.outcomes.get(id);
                if (!memory || outcome?.kind !== "selected") return null;
                return (
                  <span
                    key={id}
                    className="h-full border-r border-black/50 transition-[width] duration-500"
                    style={{
                      width: `${(outcome.cost / budget) * 100}%`,
                      background: SECTION_COLOUR[memory.section],
                      opacity: outcome.tier === "full" ? 0.9 : outcome.tier === "preview" ? 0.55 : 0.3,
                    }}
                    title={`${id} · ${outcome.cost} tokens · ${outcome.tier}`}
                  />
                );
              })}
            </div>
            <div className="mt-1.5 flex flex-wrap gap-x-3 gap-y-1 font-mono text-[10px] text-slate-500">
              <span>full ≤ {assembly.lodLimits.full}</span>
              <span>preview ≤ {assembly.lodLimits.preview}</span>
              <span>link ≤ {assembly.lodLimits.link}</span>
            </div>
          </div>

          <ul className="space-y-2" aria-label="Section quotas">
            {SECTIONS.map((section) => {
              const usedTokens = assembly.sectionUsed[section];
              const quota = assembly.quotas[section];
              const full = usedTokens >= quota - LINK_TOKENS;
              return (
                <li key={section} className="grid grid-cols-[6.5rem_minmax(0,1fr)_auto] items-center gap-3">
                  <span className="flex items-center gap-1.5 font-mono text-[10.5px] text-slate-400">
                    <span aria-hidden="true" className="h-2 w-2 rounded-sm" style={{ background: SECTION_COLOUR[section] }} />
                    {SECTION_KEY[section]}
                  </span>
                  <span className="relative h-2 overflow-hidden rounded-full bg-white/[0.06]">
                    <span
                      className="absolute inset-y-0 left-0 rounded-full transition-[width] duration-500"
                      style={{ width: `${Math.min(100, (usedTokens / quota) * 100)}%`, background: SECTION_COLOUR[section] }}
                    />
                  </span>
                  <span className={cn("font-mono text-[10.5px] tabular-nums", full ? "text-amber-200" : "text-slate-500")}>
                    {usedTokens}/{quota}
                  </span>
                </li>
              );
            })}
          </ul>

          <div className="grid grid-cols-2 gap-2">
            <Readout
              label="Items"
              value={selectedCount}
              note={`${tiers.full} full · ${tiers.preview} preview · ${tiers.link} link`}
              tone="rule"
            />
            <Readout label="Omitted" value={omitted} note="each with a reason" />
            <Readout
              label="Avg relevance"
              value={selectedCount ? (relevanceSum / selectedCount).toFixed(2) : "—"}
              note="of what was chosen"
            />
            <Readout
              label="Near-duplicates in"
              value={`${duplicatesIn} / 3`}
              tone={duplicatesIn > 0 && mode === "relevance" ? "wrong" : "neutral"}
              note={mode === "mmr" ? "redundancy lowers gain" : "no redundancy penalty"}
            />
          </div>

          <PackPreview assembly={assembly} profile={profile} budget={budget} />
        </div>
      </div>
    </DemoFrame>
  );
}
