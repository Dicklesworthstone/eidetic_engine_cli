// `bun test`: the campaign explainer's model. It is a toy, but it claims ee's constants and the shape of ee's
// rules, so these check that no week of any campaign shows a reader something those rules would not allow:
// a pack over its budget or a section over its quota, a rule count that moves without an event, a decay
// verdict that disagrees with the forget threshold, or a pack item the store could not hold.

import { describe, expect, test } from "bun:test";

import {
  BALANCED_BPS,
  BUDGET,
  DAYS,
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
  SECTIONS,
  simulate,
  totalAt,
  TRUSTS,
  weekOf,
} from "@/components/explain/campaign-model";

describe("the constants the toy borrows from ee", () => {
  test("budget, quotas and half-lives match the source", () => {
    expect(BUDGET).toBe(4000);
    expect(Object.values(BALANCED_BPS).reduce((sum, bps) => sum + bps, 0)).toBe(10000);
    expect(SECTIONS.map(quota)).toEqual([1200, 800, 800, 800, 400]);
    expect(HALF_LIFE).toEqual({ working: 1, event: 30, failure: 90, semantic: 180, procedural: 365 });
    expect(FORGET_THRESHOLD).toBe(0.01);
    expect(freshness(30, 30)).toBeCloseTo(0.5, 10);
    expect(TRUSTS).toHaveLength(6);
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

    test("the decay sweep tombstones exactly what falls under the forget threshold", () => {
      // Re-derive one sweep's verdicts from the rule itself: lifecycle = freshness × confidence × utility.
      const episode = { born: 0, failure: false, cu: 0.2, every: 0 };
      const cutoff = HALF_LIFE.event * Math.log2(episode.cu / FORGET_THRESHOLD);
      expect(lifecycleAt(episode, Math.floor(cutoff))).toBeGreaterThanOrEqual(FORGET_THRESHOLD);
      expect(lifecycleAt(episode, Math.ceil(cutoff))).toBeLessThan(FORGET_THRESHOLD);
      // A memory recalled every week never ages past seven days.
      expect(lifecycleAt({ ...episode, every: 7 }, 170)).toBeGreaterThan(0.2 * freshness(7, HALF_LIFE.event) - 1e-12);
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
  });
}
