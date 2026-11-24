// `bun test`: the Worker's routing, and the invariants every pack the page draws must keep.
// The page's two pack builders are toys of ee's rule; these check that neither one ever shows
// a reader a pack that breaks the rule it claims to follow.

import { describe, expect, test } from "bun:test";

import { assemble } from "@/components/explain/pack-builder";
import { BALANCED_BPS, BUDGETS, CANDIDATE_FLOOR, pack, STORE_SIZE, TASKS, toyHash } from "@/components/hero";
import { SHARE_IMAGE_PATHS, SHARE_IMAGE_TYPE } from "@/lib/share";
import worker from "../worker";

type Env = Parameters<typeof worker.fetch>[1];

/** An ASSETS binding that answers every request with the same body and records what it was asked for. */
function assets(status = 200): { env: Env; seen: string[] } {
  const seen: string[] = [];
  const env: Env = {
    ASSETS: {
      async fetch(request: Request) {
        seen.push(new URL(request.url).pathname);
        return new Response("asset", { status, headers: { "Content-Type": "application/octet-stream" } });
      },
    },
  };
  return { env, seen };
}

describe("worker", () => {
  test("www is sent to the apex with path and query kept, without touching the assets", async () => {
    const { env, seen } = assets();
    const response = await worker.fetch(new Request("https://www.eidetic-engine.org/some/path?x=1"), env);
    expect(response.status).toBe(301);
    expect(response.headers.get("Location")).toBe("https://eidetic-engine.org/some/path?x=1");
    expect(seen).toEqual([]);
  });

  test("the share images are served as PNG", async () => {
    for (const path of SHARE_IMAGE_PATHS) {
      const { env } = assets();
      const response = await worker.fetch(new Request(`https://eidetic-engine.org${path}`), env);
      expect(response.headers.get("Content-Type")).toBe(SHARE_IMAGE_TYPE);
      expect(response.headers.get("Cache-Control")).toBe("public, max-age=86400");
    }
  });

  test("a missing share image keeps its status and the assets' own type", async () => {
    const { env } = assets(404);
    const response = await worker.fetch(new Request(`https://eidetic-engine.org${SHARE_IMAGE_PATHS[0]}`), env);
    expect(response.status).toBe(404);
    expect(response.headers.get("Content-Type")).toBe("application/octet-stream");
  });

  test("everything else passes through unchanged", async () => {
    const { env, seen } = assets();
    const response = await worker.fetch(new Request("https://eidetic-engine.org/"), env);
    expect(response.status).toBe(200);
    expect(await response.text()).toBe("asset");
    expect(seen).toEqual(["/"]);
  });
});

const budgets: number[] = [];
for (let budget = BUDGETS.min; budget <= BUDGETS.max; budget += BUDGETS.step) budgets.push(budget);

describe("the hero's pack", () => {
  test("the slider's range covers ee's default budget", () => {
    expect(budgets).toContain(BUDGETS.initial);
    expect(BUDGETS.initial).toBe(4000);
  });

  for (const task of TASKS) {
    test(`"${task.label}": every budget fits, keeps its quotas, and packs only candidates`, () => {
      for (const budget of budgets) {
        const result = pack(task.value, budget);
        expect(result.total).toBeLessThanOrEqual(budget);
        for (const [section, bps] of Object.entries(BALANCED_BPS)) {
          const used = result.used[section as keyof typeof BALANCED_BPS];
          expect(used).toBeLessThanOrEqual(Math.ceil((budget * bps) / 10000));
        }
        const candidates = new Set(result.candidates);
        expect(result.selected.every((id) => candidates.has(id))).toBe(true);
        expect(new Set(result.selected).size).toBe(result.selected.length);
        expect(result.candidates.every((id) => (result.relevance[id] ?? 0) >= CANDIDATE_FLOOR)).toBe(true);
        expect(result.relevance).toHaveLength(STORE_SIZE);
      }
    });

    test(`"${task.label}": the same inputs give the same pack and the same hash`, () => {
      const first = pack(task.value, BUDGETS.initial);
      const second = pack(task.value, BUDGETS.initial);
      expect(second.selected).toEqual(first.selected);
      expect(toyHash(task.value, second.selected)).toBe(toyHash(task.value, first.selected));
    });
  }

  test("the default budget packs something for every task, and different tasks give different hashes", () => {
    const hashes = TASKS.map((task) => {
      const result = pack(task.value, BUDGETS.initial);
      expect(result.selected.length).toBeGreaterThan(0);
      return toyHash(task.value, result.selected);
    });
    expect(new Set(hashes).size).toBe(TASKS.length);
  });
});

describe("the pack builder", () => {
  const profiles = ["compact", "balanced", "thorough"] as const;
  const modes = ["mmr", "relevance"] as const;
  for (const profile of profiles) {
    for (const mode of modes) {
      test(`${profile}, ${mode}: the spend never passes the budget, a quota or a tier`, () => {
        for (let budget = 200; budget <= 2000; budget += 50) {
          const assembly = assemble(profile, budget, mode);
          expect(assembly.usedTokens).toBeLessThanOrEqual(budget);
          for (const [section, used] of Object.entries(assembly.sectionUsed)) {
            expect(used).toBeLessThanOrEqual(assembly.quotas[section as keyof typeof assembly.quotas]);
          }
          for (const [tier, used] of Object.entries(assembly.lodUsed)) {
            expect(used).toBeLessThanOrEqual(assembly.lodLimits[tier as keyof typeof assembly.lodLimits]);
          }
          const selected = [...assembly.outcomes.values()].filter((outcome) => outcome.kind === "selected");
          expect(selected).toHaveLength(assembly.order.length);
          expect(selected.reduce((sum, outcome) => sum + outcome.cost, 0)).toBe(assembly.usedTokens);
        }
      });
    }
  }

  test("it is deterministic", () => {
    const first = assemble("balanced", 800, "mmr");
    const second = assemble("balanced", 800, "mmr");
    expect(second.order).toEqual(first.order);
    expect(second.usedTokens).toBe(first.usedTokens);
  });
});
