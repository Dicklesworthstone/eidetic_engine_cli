// The page's colours, in roles. Ink and recall violet are the ground and the
// one accent (styles/site.tailwind); everything else here is a memory level or
// a chapter wearing one.

export const INK = "#05040d";
export const RECALL = "#a78bfa";

/**
 * One colour per memory level (src/models: working, episodic, semantic,
 * procedural), used wherever a memory is drawn: the hero's constellation, the
 * decay curves, the pack builder. Graph structure gets its own mint.
 */
export const LEVEL_COLOUR = {
  working: "#fbbf24",
  episodic: "#fb7185",
  semantic: "#38bdf8",
  procedural: "#a78bfa",
} as const;

export type Level = keyof typeof LEVEL_COLOUR;

export const GRAPH = "#34d399";
export const EVIDENCE = "#22d3ee";
export const WARNING = "#fb923c";
export const PROOF = "#e879f9";

/** The pack's five sections (src/models/context_profile.rs, ContextProfileSectionMix), each with a colour. */
export const SECTION_COLOUR = {
  rules: "#a78bfa",
  decisions: "#38bdf8",
  failures: "#fb7185",
  evidence: "#22d3ee",
  artifacts: "#fbbf24",
} as const;

export type PackSection = keyof typeof SECTION_COLOUR;
