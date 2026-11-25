// `bun test`: the campaign explainer's model. It is a toy, but it claims ee's constants and the shape of ee's
// rules, so these check that no week of any campaign shows a reader something those rules would not allow:
// a pack over its budget or a section over its quota, a rule count that moves without an event, a decay
// verdict that disagrees with the forget threshold, or a pack item the store could not hold. The first block
// reads the Rust source, so a constant or rule the figure borrows fails here when ee changes it.

import { describe, expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { join } from "node:path";

import {
  BALANCED_BPS,
  BUDGET,
  DAYS,
  DEMOTE_THRESHOLD,
  DOMAINS,
  FORGET_THRESHOLD,
  freshness,
  HALF_LIFE,
  isLive,
  lifecycleAt,
  LEVELS_BOTTOM_UP,
  mondayOf,
  packAt,
  quota,
  SECTION_KEY,
  SECTIONS,
  simulate,
  totalAt,
  TRUSTS,
  weekOf,
} from "@/components/explain/campaign-model";

const REPO = join(import.meta.dir, "..", "..");
const source = (path: string) => readFileSync(join(REPO, path), "utf8");
/** An integer literal the way ee's source writes it: 4000 is `4_000`. */
const rustInt = (n: number) => n.toLocaleString("en-US").replaceAll(",", "_");
/** The text of one Rust item, from its signature to its closing brace (at column 0, or at `indent` for a method). */
const body = (text: string, signature: string, indent = "") => {
  const start = text.indexOf(signature);
  expect(start).toBeGreaterThanOrEqual(0);
  const end = text.indexOf(`\n${indent}}\n`, start);
  expect(end).toBeGreaterThan(start);
  return text.slice(start, end + indent.length + 2);
};
const CAPTION = source("web/components/explain/campaign.tsx");

describe("the constants the toy borrows from ee", () => {
  test("budget, quotas and half-lives match the source", () => {
    expect(BUDGET).toBe(4000);
    expect(source("src/pack/mod.rs")).toContain(`pub const DEFAULT_CONTEXT_MAX_TOKENS: u32 = ${rustInt(BUDGET)};`);

    const profile = source("src/models/context_profile.rs");
    // ContextProfileSectionMix::new takes the sections in this order, and the balanced profile passes these.
    const order = SECTIONS.map((section) => `${SECTION_KEY[section]}_bps: u16,`).join("\\s*");
    expect(profile).toMatch(new RegExp(`pub const fn new\\(\\s*${order}\\s*\\)`));
    const mix = SECTIONS.map((section) => rustInt(BALANCED_BPS[section])).join(", ");
    expect(profile).toMatch(new RegExp(`ContextProfileName::Balanced => Self \\{[^}]*section_mix: ContextProfileSectionMix::new\\(${mix}\\)`));
    for (const section of SECTIONS) expect(profile).toContain(`=> "${SECTION_KEY[section]}"`);
    expect(Object.values(BALANCED_BPS).reduce((sum, bps) => sum + bps, 0)).toBe(10000);
    expect(SECTIONS.map(quota)).toEqual([1200, 800, 800, 800, 400]);

    const decay = source("src/policy/memory_decay.rs");
    const halfLives = body(decay, "impl Default for MemoryDecayHalfLives");
    for (const [field, days] of [
      ["working", HALF_LIFE.working],
      ["episodic_event", HALF_LIFE.event],
      ["episodic_failure", HALF_LIFE.failure],
      ["semantic_fact", HALF_LIFE.semantic],
      ["procedural_rule", HALF_LIFE.procedural],
    ] as const) {
      expect(halfLives).toContain(`${field}: ${days.toFixed(1)},`);
    }
    expect(decay).toContain(`pub const DEFAULT_DECAY_DEMOTE_THRESHOLD: f32 = ${DEMOTE_THRESHOLD};`);
    expect(decay).toContain(`pub const DEFAULT_DECAY_FORGET_THRESHOLD: f32 = ${FORGET_THRESHOLD};`);
    expect(freshness(30, 30)).toBeCloseTo(0.5, 10);
  });

  test("the trust classes are ee's six, strongest first", () => {
    const confidence = body(source("src/models/trust.rs"), "pub const fn initial_confidence(self) -> f32 {", "    ");
    const declared = [...confidence.matchAll(/Self::(\w+) => ([\d.]+),/g)].map(([, name, value]) => ({
      name: name!.replace(/([a-z])([A-Z])/g, "$1_$2").toLowerCase(),
      value: Number(value),
    }));
    expect(declared.map((each) => each.name)).toEqual([...TRUSTS]);
    for (let i = 1; i < declared.length; i++) expect(declared[i]!.value).toBeLessThan(declared[i - 1]!.value);
  });

  test("the rules the events follow, as the caption states them", () => {
    expect(source("src/core/rule.rs")).toContain("const PLAYBOOK_MIN_EVIDENCE: usize = 3;");
    expect(CAPTION).toContain("PLAYBOOK_MIN_EVIDENCE 3");
    const outcome = source("src/core/outcome.rs");
    expect(outcome).toContain("const ANTI_PATTERN_PROPOSAL_THRESHOLD: usize = 3;");
    expect(outcome).toContain("&& self.distinct_harmful_sources >= 2");
    expect(CAPTION).toContain("ANTI_PATTERN_PROPOSAL_THRESHOLD 3 from two sources");
    const quorum = source("src/models/rule.rs");
    expect(quorum).toContain("self.distinct_harmful_sources < 2");
    expect(quorum).toContain("self.harmful_outcomes >= 2");
    // The "validated" events: a helpful outcome on a native rule moves its confidence and utility, never its
    // trust class or maturity, so the promotion is an explicit `ee rule update --trust-class`.
    expect(quorum).toContain('"helpful outcome adjusts confidence and utility without silent promotion"');
    const ruleUpdate = source("src/cli/mod.rs").split("pub struct RuleUpdateArgs {")[1]?.split("\n}\n")[0] ?? "";
    expect(ruleUpdate).toContain("pub trust_class: Option<String>,");
  });

  test("an applied anti-pattern starts at agent_assertion, and an import caps trust at agent_validated", () => {
    expect(source("src/core/outcome.rs")).toMatch(
      /candidate_type: CandidateType::AntiPatternProposal\.as_str\(\)\.to_owned\(\),[^}]*proposed_trust_class: None,/,
    );
    expect(source("src/core/curate.rs")).toMatch(
      /let rule_trust_class = stored\s*\.proposed_trust_class\s*\.clone\(\)\s*\.unwrap_or_else\(\|\| "agent_assertion"\.to_owned\(\)\);/,
    );
    const cap = body(source("src/core/rule.rs"), "fn portable_import_trust_class<");
    expect(cap.match(/return TrustClass::AgentValidated\.as_str\(\);/g)).toHaveLength(2);
    for (const domain of DOMAINS) {
      for (const item of domain.items) {
        if (item.kind === "anti-pattern") expect(item.trust).toBe("agent_assertion");
      }
      const imports = domain.events.filter((event) => event.type === "import").map((event) => event.day);
      for (const item of domain.items) {
        if (item.section !== "rules" || !imports.includes(item.from)) continue;
        expect(TRUSTS.indexOf(item.trust)).toBeGreaterThanOrEqual(TRUSTS.indexOf("agent_validated"));
      }
    }
  });

  test("weeks and Mondays", () => {
    expect(DAYS).toBe(182);
    expect(weekOf(0)).toBe(1);
    expect(weekOf(DAYS - 1)).toBe(26);
    expect(mondayOf(13)).toBe(7);
    expect(mondayOf(DAYS + 40)).toBe(175);
  });
});

for (const domain of DOMAINS) {
  describe(`campaign: ${domain.label}`, () => {
    const campaign = simulate(domain);

    test("it is deterministic", () => {
      const again = simulate(domain);
      for (const level of LEVELS_BOTTOM_UP) expect(again.levels[level]).toEqual(campaign.levels[level]);
      expect(again.timeline).toEqual(campaign.timeline);
      expect(again.avoided).toEqual(campaign.avoided);
    });

    test("every series covers every day with whole, non-negative counts", () => {
      const series = [
        ...LEVELS_BOTTOM_UP.map((level) => campaign.levels[level]),
        campaign.failures,
        campaign.episodes,
        campaign.written,
        campaign.retired,
        campaign.rules,
        campaign.avoided,
      ];
      for (const values of series) {
        expect(values).toHaveLength(DAYS);
        for (const value of values) {
          expect(Number.isInteger(value)).toBe(true);
          expect(value).toBeGreaterThanOrEqual(0);
        }
      }
      for (let day = 0; day < DAYS; day++) {
        expect((campaign.failures[day] ?? 0) + (campaign.episodes[day] ?? 0)).toBe(campaign.levels.episodic[day] ?? -1);
        expect(campaign.retired[day] ?? 0).toBeLessThanOrEqual(campaign.written[day] ?? 0);
        expect(totalAt(campaign, day)).toBeLessThanOrEqual(campaign.maxTotal);
        // The readout prints all three side by side, so they must reconcile: no memory counted twice or lost.
        expect(totalAt(campaign, day)).toBe((campaign.written[day] ?? 0) - (campaign.retired[day] ?? 0));
      }
    });

    test("cumulative series never fall, and the store retires most of what it is given", () => {
      for (const values of [campaign.written, campaign.retired, campaign.avoided, campaign.promoted, campaign.deprecated]) {
        for (let day = 1; day < DAYS; day++) expect(values[day] ?? 0).toBeGreaterThanOrEqual(values[day - 1] ?? 0);
      }
      const last = DAYS - 1;
      expect(campaign.retired[last] ?? 0).toBeGreaterThan((campaign.written[last] ?? 0) / 2);
      expect(campaign.avoided[last] ?? 0).toBeGreaterThan(0);
    });

    test("procedural rules move only on an event's day, by that event's amount", () => {
      for (let day = 0; day < DAYS; day++) {
        const before = day === 0 ? 0 : (campaign.rules[day - 1] ?? 0);
        const expected = campaign.timeline.filter((event) => event.day === day).reduce((sum, event) => sum + event.rules, 0);
        expect((campaign.rules[day] ?? 0) - before).toBe(expected);
        expect(campaign.levels.procedural[day]).toBe(campaign.rules[day] ?? -1);
      }
      expect(campaign.rules[DAYS - 1] ?? 0).toBe((campaign.promoted[DAYS - 1] ?? 0) - (campaign.deprecated[DAYS - 1] ?? 0));
    });

    test("events lie inside the campaign, in order, with their placeholders filled", () => {
      let previous = -1;
      for (const event of campaign.timeline) {
        expect(event.day).toBeGreaterThanOrEqual(0);
        expect(event.day).toBeLessThan(DAYS);
        expect(event.day).toBeGreaterThanOrEqual(previous);
        previous = event.day;
        expect(`${event.title} ${event.detail}`).not.toContain("{");
        expect(event.command.startsWith("ee ")).toBe(true);
      }
      const sweep = campaign.timeline.find((event) => event.type === "tombstone");
      expect(sweep).toBeDefined();
      expect(campaign.swept[sweep?.day ?? 0] ?? 0).toBeGreaterThan(0);
      expect(campaign.timeline.some((event) => event.type === "demote" && event.rules < 0)).toBe(true);
      expect(campaign.timeline.some((event) => event.type === "revisit")).toBe(true);
    });

    test("the decay rule crosses the forget threshold where each half-life says, and the sweep runs weekly", () => {
      // Re-derive the verdicts from the rule itself: lifecycle = freshness × confidence × utility, with the event
      // half-life for an episodic event and the failure half-life for a failure.
      for (const [failure, halfLife] of [
        [false, HALF_LIFE.event],
        [true, HALF_LIFE.failure],
      ] as const) {
        const episode = { born: 0, failure, cu: 0.2, every: 0 };
        const cutoff = halfLife * Math.log2(episode.cu / FORGET_THRESHOLD);
        expect(lifecycleAt(episode, Math.floor(cutoff))).toBeGreaterThanOrEqual(FORGET_THRESHOLD);
        expect(lifecycleAt(episode, Math.ceil(cutoff))).toBeLessThan(FORGET_THRESHOLD);
      }
      // A memory recalled every week never ages past seven days.
      const recalled = { born: 0, failure: false, cu: 0.2, every: 7 };
      expect(lifecycleAt(recalled, 170)).toBeGreaterThan(0.2 * freshness(7, HALF_LIFE.event) - 1e-12);
      // The simulation's sweep: Sundays only, and it does take something over the campaign.
      for (let day = 0; day < DAYS; day++) {
        if (day % 7 !== 6) expect(campaign.swept[day] ?? 0).toBe(0);
      }
      expect(campaign.swept.reduce((sum, count) => sum + count, 0)).toBeGreaterThan(0);
    });

    test("an outcome names the kind of target its step needs", () => {
      // Every rule in the panel is a native rule (from a playbook extract or a curation apply), and `ee outcome`
      // reaches one only with --target-type rule: it defaults to memory, and a rule id is not a memory id.
      // Outcomes move a memory's trust class on their own; a rule's moves only by `ee rule update`.
      for (const event of campaign.timeline) {
        if (!event.command.startsWith("ee outcome ")) continue;
        if (event.command.includes("<rule-id>")) expect(event.command).toContain("--target-type rule");
        if (event.type === "validated") expect(event.command).toContain("ee rule update <rule-id> --trust-class agent_validated");
        if (event.type === "demote") expect(event.command).toContain("--target-type rule");
      }
    });

    test("every Monday's pack keeps the budget and every quota, and shows only what the store holds", () => {
      for (let day = 0; day < DAYS; day += 7) {
        const pack = packAt(campaign, day);
        expect(pack.used).toBeLessThanOrEqual(BUDGET);
        expect(pack.used).toBe(pack.sections.reduce((sum, section) => sum + section.used, 0));
        const namedRules = domain.items.filter((item) => item.section === "rules" && isLive(item, day)).length;
        expect(namedRules).toBeLessThanOrEqual(campaign.rules[day] ?? 0);
        for (const section of pack.sections) {
          expect(section.used).toBeLessThanOrEqual(section.quota);
          expect(section.lines.reduce((sum, line) => sum + line.item.tokens, 0)).toBeLessThanOrEqual(section.used);
          for (const line of section.lines) {
            expect(line.item.section).toBe(section.section);
            expect(isLive(line.item, day)).toBe(true);
            expect(line.due).toBe(line.item.revisitBy !== undefined && day >= line.item.revisitBy);
          }
        }
      }
    });

    test("the pack fills as the campaign earns rules, and the store outgrows it", () => {
      const early = packAt(campaign, 7);
      const late = packAt(campaign, DAYS - 1);
      expect(late.used).toBeGreaterThan(early.used);
      expect(late.used).toBeGreaterThan(BUDGET * 0.8);
      const rulesLate = late.sections.find((section) => section.section === "rules");
      expect(rulesLate?.used ?? 0).toBeGreaterThan(quota("rules") / 2);
      expect(totalAt(campaign, DAYS - 1)).toBeGreaterThan(totalAt(campaign, 7) * 4);
    });

    test("every item cites a source URI, carries a known trust class, and fits the campaign", () => {
      const ids = new Set<string>();
      for (const item of domain.items) {
        expect(ids.has(item.id)).toBe(false);
        ids.add(item.id);
        expect(item.source).toMatch(/^[a-z][a-z0-9-]*:\/\/\S+$/);
        expect(TRUSTS).toContain(item.trust);
        if (item.upgrade) expect(TRUSTS).toContain(item.upgrade.to);
        expect(item.from).toBeGreaterThanOrEqual(0);
        expect(item.from).toBeLessThan(DAYS);
        if (item.until !== undefined) expect(item.until).toBeGreaterThan(item.from);
        if (item.revisitBy !== undefined) expect(item.revisitBy).toBeGreaterThan(item.from);
        expect(item.tokens).toBeLessThanOrEqual(quota(item.section));
      }
    });

    test("once the anti-pattern is in force, it is the first rule every pack shows (include_anti_pattern_first)", () => {
      const antiPattern = domain.items.find((item) => item.kind === "anti-pattern");
      expect(antiPattern).toBeDefined();
      if (!antiPattern) return;
      let seen = 0;
      for (let day = 0; day < DAYS; day += 7) {
        if (!isLive(antiPattern, day)) continue;
        seen += 1;
        expect(packAt(campaign, day).sections[0]?.lines[0]?.item.id).toBe(antiPattern.id);
      }
      expect(seen).toBeGreaterThan(0);
    });
  });
}
