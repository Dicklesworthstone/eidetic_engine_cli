// `bun test`: the scale ladder's drawn swarm and lane rules must never show a reader
// something ee does not do: two writes committing at once without the write owner, a
// batch past the daemon's bound, or a less specific lane silently beating a more specific one.
// The last block reads the Rust source and docs, so the numbers the ladder states fail here
// when ee changes them.

import { describe, expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { join } from "node:path";

import { CYCLE, LANE_RANK, LANES, MAX_BATCH, overlap, RUNGS, swarmCycle } from "@/components/explain/scale-lanes";

describe("the drawn swarm", () => {
  for (const mode of ["daemon", "oneshot"] as const) {
    test(`${mode}: every write commits exactly once, after it arrives and inside its cycle`, () => {
      for (const agents of [1, 8, 10]) {
        for (let cycle = 0; cycle < 300; cycle++) {
          const { writes } = swarmCycle(cycle, agents, mode);
          expect(writes.length).toBeGreaterThan(0);
          expect(new Set(writes.map((write) => write.agent)).size).toBe(writes.length);
          for (const write of writes) {
            expect(write.agent).toBeGreaterThanOrEqual(0);
            expect(write.agent).toBeLessThan(agents);
            expect(write.arrive).toBeGreaterThan(write.emit);
            expect(write.commitStart).toBeGreaterThanOrEqual(write.arrive);
            expect(write.commitEnd).toBeGreaterThan(write.commitStart);
            expect(write.commitEnd).toBeLessThanOrEqual(CYCLE);
          }
        }
      }
    });
  }

  test("one-shot writes take the lock one at a time: no two commits overlap", () => {
    for (let cycle = 0; cycle < 300; cycle++) {
      const { writes, commits } = swarmCycle(cycle, 10, "oneshot");
      expect(commits).toBe(writes.length);
      const ordered = [...writes].sort((a, b) => a.commitStart - b.commitStart);
      for (let i = 1; i < ordered.length; i++) {
        expect(ordered[i]!.commitStart).toBeGreaterThanOrEqual(ordered[i - 1]!.commitEnd - 1e-9);
      }
    }
  });

  test("the daemon commits a batch as one, and never more than 64 writes in it", () => {
    for (const agents of [10, 150]) {
      for (let cycle = 0; cycle < 100; cycle++) {
        const { writes, commits } = swarmCycle(cycle, agents, "daemon");
        const batches = new Map<number, typeof writes>();
        for (const write of writes) batches.set(write.batch, [...(batches.get(write.batch) ?? []), write]);
        expect(commits).toBe(batches.size);
        expect(commits).toBe(Math.ceil(writes.length / MAX_BATCH));
        for (const batch of batches.values()) {
          expect(batch.length).toBeLessThanOrEqual(MAX_BATCH);
          expect(new Set(batch.map((write) => write.commitStart)).size).toBe(1);
        }
      }
    }
  });

  test("it is deterministic", () => {
    expect(swarmCycle(7, 10, "daemon")).toEqual(swarmCycle(7, 10, "daemon"));
  });
});

describe("the lanes", () => {
  test("workspace is more specific than team, and team than global (lane_specificity_rank)", () => {
    expect(LANE_RANK.workspace).toBeLessThan(LANE_RANK.team);
    expect(LANE_RANK.team).toBeLessThan(LANE_RANK.global);
    expect(LANES.map((lane) => lane.id)).toEqual(["workspace", "team", "global"]);
  });

  test("same text: the more specific lane takes recorded precedence, whichever order the rows come in", () => {
    for (const [a, b, winner] of [
      ["workspace", "team", "workspace"],
      ["global", "team", "team"],
      ["global", "workspace", "workspace"],
    ] as const) {
      for (const result of [overlap({ lane: a, text: "x" }, { lane: b, text: "x" }), overlap({ lane: b, text: "x" }, { lane: a, text: "x" })]) {
        expect(result?.kind).toBe("corroboration");
        expect(result?.moreSpecific).toBe(winner);
        expect(result?.moreSpecificOverrides).toBe(true);
        expect(result?.bothSurfaced).toBe(true);
      }
    }
  });

  test("different text: both stay visible and neither wins", () => {
    const result = overlap({ lane: "global", text: "rebase" }, { lane: "workspace", text: "merge" });
    expect(result?.kind).toBe("contradiction");
    expect(result?.moreSpecificOverrides).toBe(false);
    expect(result?.bothSurfaced).toBe(true);
    expect(overlap({ lane: "team", text: "a" }, { lane: "team", text: "b" })).toBeNull();
  });
});

describe("the ladder's text", () => {
  test("four rungs, each with three or four sourced facts, ee commands and four readouts", () => {
    expect(RUNGS.map((rung) => rung.id)).toEqual(["one", "swarm", "team", "everywhere"]);
    for (const rung of RUNGS) {
      expect(rung.facts.length).toBeGreaterThanOrEqual(3);
      expect(rung.facts.length).toBeLessThanOrEqual(4);
      for (const fact of rung.facts) {
        expect(fact.source.length).toBeGreaterThan(0);
        // Inline code spans must be balanced, or a backtick would show on the page.
        expect((fact.text.match(/`/g) ?? []).length % 2).toBe(0);
      }
      expect(rung.commands.length).toBeGreaterThan(0);
      for (const command of rung.commands) expect(command.startsWith("ee ")).toBe(true);
      expect(rung.readouts.map((readout) => readout.label)).toEqual([
        "Agents",
        "Writers at once",
        "Stores",
        "Incoming trust class",
      ]);
    }
  });

  test("the lanes come into play as the ladder zooms out, and never leave", () => {
    const inPlay = RUNGS.map((rung) => rung.lanes);
    expect(inPlay).toEqual([1, 1, 2, 3]);
  });
});

describe("the ee facts the figure states", () => {
  const repo = join(import.meta.dir, "..", "..");
  const source = (path: string) => readFileSync(join(repo, path), "utf8");
  /** The text of one Rust function, from its signature to the closing brace at column 0. */
  const body = (text: string, signature: string) => {
    const start = text.indexOf(signature);
    expect(start).toBeGreaterThanOrEqual(0);
    return text.slice(start, text.indexOf("\n}\n", start) + 2);
  };
  const facts = RUNGS.flatMap((rung) => rung.facts.map((fact) => fact.text)).join("\n");
  const rung = (id: string) => RUNGS.find((each) => each.id === id);

  test("the daemon's batch: at most 64 rows in a 1,000 µs window, and only for ops that can share a transaction", () => {
    const server = source("src/daemon/server.rs");
    expect(server).toContain(`group_commit_max_rows: ${MAX_BATCH},`);
    expect(server).toContain("group_commit_max_us: 1_000,");
    expect(facts).toContain(`up to ${MAX_BATCH} in a 1 ms window`);
    // A batch shares one transaction only when every op in it is coalescible, so the figure says writes "can" share one.
    expect(server).toContain("fn batch_is_daemon_txn_coalescible(");
    expect(facts).toContain("can share a single transaction");
  });

  test("lane ranks are lane_specificity_rank's", () => {
    const rank = body(source("src/core/global_store.rs"), "pub const fn lane_specificity_rank(");
    for (const [lane, value] of Object.entries(LANE_RANK)) {
      expect(rank).toContain(`MemoryLane::${lane[0]!.toUpperCase()}${lane.slice(1)} => ${value},`);
    }
  });

  test("global rows that overlap no workspace row get at most 15% of the budget", () => {
    const declared = source("src/core/global_store.rs").match(/pub const DEFAULT_GLOBAL_FAN_IN_BASIS_POINTS: u32 = ([\d_]+);/);
    const basisPoints = Number(declared?.[1]?.replaceAll("_", ""));
    expect(basisPoints).toBe(1500);
    expect(facts).toContain(`take at most ${basisPoints / 100}% of the pack's token budget`);
    // Rows in a lane conflict are kept whatever the cap: the reason the fact is limited to rows that do not overlap.
    expect(body(source("src/core/context.rs"), "fn apply_global_store_pack_policy(")).toContain(
      "|| protected_global_ids.contains(&memory_id)",
    );
  });

  test("only human_explicit and agent_validated memories are promoted to global, and a refusal exits 7", () => {
    expect(source("src/core/global_promotion.rs")).toContain(
      'const PROMOTABLE_TRUST_CLASSES: [&str; 2] = ["human_explicit", "agent_validated"];',
    );
    expect(rung("everywhere")?.readouts.at(-1)?.value).toBe("human_explicit · agent_validated");
    const promote = body(source("src/cli/mod.rs"), "fn handle_memory_promote_global<");
    expect(promote).toContain("DomainError::PolicyDeniedWithDetails {");
    // Both config keys close the lane to promotion.
    expect(promote).toContain("memory_config.include_global.unwrap_or(true)");
    expect(promote).toContain("memory_config.participate.unwrap_or(true)");
    expect(source("src/models/mod.rs")).toContain("PolicyDenied = 7,");
    expect(facts).toContain("a typed refusal with exit code 7");
  });

  test("a teammate's memory lands as peer_human_attested, and twenty members is the plan's v1 bound", () => {
    expect(body(source("src/mesh/team.rs"), "pub fn project_inbound_team_memory(")).toContain(
      'trust_class: "peer_human_attested".to_owned(),',
    );
    expect(rung("team")?.readouts.at(-1)?.value).toBe("peer_human_attested");
    expect(source("docs/mesh/team_confederation_plan.md")).toContain("Twenty is a hard v1 active-member protocol bound");
    expect(facts).toContain("2 to 20 people");
  });
});
