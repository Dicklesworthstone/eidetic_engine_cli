// `bun test`: a drift check for the three pack figures in chapter 03 (components/explain/pipeline.tsx,
// pack-builder.tsx and hash-prefix.tsx). Each test reads the Rust source the figure cites and fails when a stage,
// constant or label the figure states stops being what ee does.

import { describe, expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { join } from "node:path";

import { blake3, concat, flatFeed, labeledFeed, PAIRS, PROVENANCE_LABELS, quantize } from "@/components/explain/hash-prefix";
import { assemble, FLOOR, LOD_BPS, MEMORIES, PROFILES, RELEVANCE_WEIGHT } from "@/components/explain/pack-builder";
import { MULTIPLIERS, STAGES } from "@/components/explain/pipeline";

const repo = join(import.meta.dir, "..", "..");
const source = (path: string) => readFileSync(join(repo, path), "utf8");
const context = source("src/core/context.rs");
const pack = source("src/pack/mod.rs");

describe("figure 03.1: the stages of ee pack", () => {
  test("twelve distinct spans, each one ee records", () => {
    expect(STAGES.length).toBe(12);
    expect(new Set(STAGES.map((stage) => stage.span)).size).toBe(12);
    for (const stage of STAGES) expect(context).toContain(`trace.record_elapsed("${stage.span}",`);
  });

  test("in the order they start", () => {
    // Where each span's clock starts. The cache lookup is timed inside a helper, so its place is the call site.
    const startOf = (span: string) => {
      if (span === "packL2Lookup") return context.indexOf("context_pack_l2_try_hit(");
      const clock = context.match(new RegExp(`trace\\.record_elapsed\\("${span}", (\\w+)\\)`))?.[1];
      expect(clock).toBeDefined();
      return context.indexOf(`let ${clock} = Instant::now();`);
    };
    const starts = STAGES.map((stage) => startOf(stage.span));
    for (const at of starts) expect(at).toBeGreaterThan(0);
    for (let i = 1; i < starts.length; i++) expect(starts[i]!).toBeGreaterThan(starts[i - 1]!);
  });

  test("each degradation code a stage names is one ee emits", () => {
    const tree = ["src/core/context.rs", "src/core/search.rs", "src/core/memory_drift.rs", "src/models/degradation.rs"]
      .map(source)
      .join("\n");
    for (const stage of STAGES) if (stage.code) expect(tree).toContain(`"${stage.code}"`);
  });

  test("the pool, the fusion weights and the storage-tier nudge", () => {
    expect(STAGES[0]!.what).toContain("a pool of 64");
    const pools = [...source("src/models/context_profile.rs").matchAll(/^ {16}default_candidate_pool: (\d+),$/gm)];
    expect(pools.length).toBe(6);
    for (const pool of pools) expect(pool[1]).toBe("64");

    const search = source("src/core/search.rs");
    expect(search).toContain("const DEFAULT_SEARCH_LEXICAL_WEIGHT: f32 = 0.45;");
    expect(search).toContain("const DEFAULT_SEARCH_SEMANTIC_WEIGHT: f32 = 0.45;");

    const tier = STAGES.find((stage) => stage.span === "memoryTierAdmission")!;
    expect(tier.what).toContain("0.025");
    expect(tier.what).toContain("0.010");
    expect(context).toContain("const CONTEXT_MEMORY_TIER_HOT_BOOST: f32 = 0.025;");
    expect(context).toContain("const CONTEXT_MEMORY_TIER_WARM_BOOST: f32 = 0.010;");
    expect(tier.status).toBe("off");
    expect(source("src/config/merge.rs")).toContain("memory_tier_admission: Some(false),");
  });

  test("personalized PageRank: off by default, FrankenNetworkX's when on", () => {
    const ppr = STAGES.find((stage) => stage.span === "pprRerank")!;
    expect(ppr.status).toBe("off");
    expect(source("src/config/merge.rs")).toContain("ppr_enabled: Some(false),");
    // With the feature off, ee pack's weight resolves to 0 and the stage returns before ranking anything.
    expect(context).toContain("if !config.graph.feature.ppr_enabled.unwrap_or(false) {");
    expect(context).toContain("if candidates.is_empty() || ppr_weight == 0.0 {");
    // When on, it runs in production (no test-only gate) and blends as the figure says.
    expect(context).not.toContain("graph_ppr_upstream_unavailable");
    expect(context).toContain("let blended = (ppr_weight * ppr_score) + ((1.0 - ppr_weight) * base);");
    expect(context).toContain("pub const DEFAULT_CONTEXT_PPR_WEIGHT: f32 = 0.30;");
    expect(ppr.what).toContain("0.30");
    expect(source("src/graph/ppr.rs")).toContain("fnx_algorithms::personalized_pagerank(");
    expect(ppr.code).toBe("graph_ppr_snapshot_stale");
    expect(source("src/models/degradation.rs")).toContain('GRAPH_PPR_SNAPSHOT_STALE_CODE: &str = "graph_ppr_snapshot_stale"');
    // The figure says only ee context takes --ppr-weight; ee pack reads graph.ppr.alpha.
    const packArgs = source("src/cli/mod.rs").match(/pub struct PackArgs \{[\s\S]*?\n\}/)?.[0] ?? "";
    expect(packArgs).toContain("pub max_tokens");
    expect(packArgs).not.toContain("ppr_weight");
  });

  test("the quality multipliers and their floors", () => {
    const scoring = source("src/search/scoring.rs");
    const rule = (name: string) => MULTIPLIERS.find((multiplier) => multiplier.name === name)!;
    expect(scoring).toContain("pub const DEFAULT_RECENCY_TAU_DAYS: f32 = 30.0;");
    expect(scoring).toContain(`pub const DEFAULT_RECENCY_FLOOR: f32 = ${rule("recency").low};`);
    expect(scoring).toContain(`pub const DEFAULT_CONFIDENCE_FLOOR: f32 = ${rule("confidence").low};`);
    expect(scoring).toContain(`pub const DEFAULT_UTILITY_FLOOR: f32 = ${rule("utility").low};`);
    expect(scoring).toContain("pub const DEFAULT_HARMFUL_PENALTY_PER_HIT: f32 = 0.1;");
    expect(scoring).toContain(`pub const DEFAULT_HARMFUL_PENALTY_FLOOR: f32 = ${rule("harmful").low};`);
    expect(scoring).toContain(`pub const DEFAULT_SCOPE_MATCH_BONUS: f32 = ${rule("scope").high};`);
  });
});

describe("figure 03.2: the toy pack builder", () => {
  test("MMR weight, coverage-fill floor and level-of-detail shares", () => {
    expect(pack).toContain("pub const DEFAULT_MMR_RELEVANCE_WEIGHT: f32 = crate::config::DEFAULT_PACK_MMR_LAMBDA;");
    expect(source("src/config/merge.rs")).toContain(`pub const DEFAULT_PACK_MMR_LAMBDA: f32 = ${RELEVANCE_WEIGHT};`);
    expect(pack).toContain(`pub const DEFAULT_COVERAGE_FILL_RELEVANCE_FLOOR: f32 = ${FLOOR};`);
    const thousands = (bps: number) => `${bps / 1000}_000`;
    expect(pack).toContain(
      `pub const fn default_70_20_10() -> Self {\n        Self::new(${thousands(LOD_BPS.full)}, ${thousands(LOD_BPS.preview)}, ${thousands(LOD_BPS.link)})`,
    );
    expect(pack).toContain("let tokens = product.div_ceil(10_000).min(u64::from(u32::MAX)).max(1);");
  });

  test("each profile's section mix is the built-in one", () => {
    const profiles = source("src/models/context_profile.rs");
    const rust = (bps: number) => (bps >= 1000 ? `${Math.floor(bps / 1000)}_${String(bps % 1000).padStart(3, "0")}` : String(bps));
    for (const [name, mix] of Object.entries(PROFILES)) {
      const display = name[0]!.toUpperCase() + name.slice(1);
      const args = [mix.rules, mix.decisions, mix.failures, mix.evidence, mix.artifacts].map(rust).join(", ");
      expect(profiles).toMatch(
        new RegExp(`display_name: "${display}",[^}]*?section_mix: ContextProfileSectionMix::new\\(${args}\\),`),
      );
    }
  });

  test("a preview or a link says how much room its full text had, and that it was too little", () => {
    const tokens = new Map(MEMORIES.map((memory) => [memory.id, memory.tokens]));
    for (const profile of ["compact", "balanced", "thorough"] as const) {
      for (let budget = 200; budget <= 2000; budget += 50) {
        for (const mode of ["mmr", "relevance"] as const) {
          for (const [id, outcome] of assemble(profile, budget, mode).outcomes) {
            if (outcome.kind !== "selected") continue;
            if (outcome.tier === "full") {
              expect(outcome.room).toBeUndefined();
              continue;
            }
            expect(outcome.room).toBeDefined();
            expect(outcome.room!.tokens).toBeLessThan(tokens.get(id)!);
            expect(outcome.room!.limit).toMatch(/^(full-text tier|pack budget|\w+ quota)/);
          }
        }
      }
    }
  });
});

describe("figure 03.4: the bytes behind a pack's hash", () => {
  test("the BLAKE3 is BLAKE3", () => {
    const encode = (text: string) => new TextEncoder().encode(text);
    expect(blake3(encode(""))).toBe("af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262");
    expect(blake3(encode("abc"))).toBe("6437b3ac38465133ffb63b75273a8db548c558465d79db03fd359c6cd5bd9d85");
  });

  test("ee's field encoding and the labels it gives an item's provenance", () => {
    expect(context).toContain(
      [
        "fn hash_labeled_bytes(hasher: &mut blake3::Hasher, label: &str, value: &[u8]) {",
        "    hasher.update(&(label.len() as u64).to_le_bytes());",
        "    hasher.update(label.as_bytes());",
        "    hasher.update(&(value.len() as u64).to_le_bytes());",
        "    hasher.update(value);",
      ].join("\n"),
    );
    expect(context).toContain('hash_pack_hash_provenance(hasher, "item.provenance", &item.provenance);');
    expect(context).toContain('&format!("{label}.uri"),');
    expect(context).toContain('hash_labeled_bytes(hasher, &format!("{label}.note"), entry.note.as_bytes());');
    expect(PROVENANCE_LABELS).toEqual({ uri: "item.provenance.uri", note: "item.provenance.note" });
  });

  test("the flat feed collides and the labelled feed does not", () => {
    const flatA = concat(flatFeed("file://a", "bc"));
    const flatB = concat(flatFeed("file://ab", "c"));
    expect(flatA).toEqual(flatB);
    const labelledA = concat(labeledFeed("file://a", "bc"));
    const labelledB = concat(labeledFeed("file://ab", "c"));
    expect(blake3(labelledA)).not.toBe(blake3(labelledB));
    // u64 LE length, label, u64 LE length, value; twice.
    expect(labelledA.length).toBe(8 + 19 + 8 + 8 + 8 + 20 + 8 + 2);
    expect(source("src/core/context_test_module.rs")).toContain("fn pack_hash_v2_separates_flat_feed_provenance_collision()");
  });

  test("ten component digests, nine without the skipped list", () => {
    const composite = context.match(/let mut composite_hasher = blake3::Hasher::new\(\);[\s\S]*?finalize_blake3\(composite_hasher\)/)?.[0];
    expect(composite).toBeDefined();
    const labels = [...composite!.matchAll(/&mut composite_hasher,\s*"(\w+)"/g)].map((match) => match[1]);
    expect(labels).toEqual([
      "schema",
      "request",
      "reference_time",
      "quality_scoring",
      "items",
      "omitted",
      "degraded",
      "coordination",
      "rendered_text",
      "effective_config",
      "store_scopes",
    ]);
    expect(composite).toMatch(/if include_skipped \{\s*hash_labeled_bytes\(&mut composite_hasher, "omitted"/);
  });

  test("Q20.12 over f32 scores, and pairs that do what their labels say", () => {
    expect(pack).toContain("let scaled = (value * 4096.0).round();");
    expect(pack).toContain("pub(crate) fn q20_12_le_bytes(value: f32) -> [u8; 4] {");
    expect(context).toContain("fn hash_labeled_q20_12(hasher: &mut blake3::Hasher, label: &str, value: f32) {");
    for (const pair of Object.values(PAIRS)) {
      // Two different f32 scores, closer together than one quantum.
      expect(Math.fround(pair.a)).not.toBe(Math.fround(pair.b));
      expect(Math.abs(pair.b - pair.a)).toBeLessThan(1 / 4096);
    }
    expect(quantize(PAIRS.noise.a)).toBe(quantize(PAIRS.noise.b));
    expect(quantize(PAIRS.quantum.a)).toBe(quantize(PAIRS.quantum.b));
    expect(quantize(PAIRS.halfStep.a)).not.toBe(quantize(PAIRS.halfStep.b));
  });
});
