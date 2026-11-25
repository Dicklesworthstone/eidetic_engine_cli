// `bun test`: the rules four figures compute with, read against the Rust that defines them. Decay (02.1), trust
// (04.1), the operating loop (01.3) and the incident card (04.2) each restate a constant, a threshold or a masking
// rule from ee. Those numbers were pinned only by a caption; a change in ee now turns this red instead.

import { describe, expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { join } from "node:path";

import { classify, DEMOTE, DEMOTED, FORGET, KINDS } from "@/components/explain/decay";
import { EXAMPLES, maskedTemplate, template } from "@/components/explain/incident-card";
import { COMMANDS } from "@/components/explain/loop";
import { betaQuantile, HARMFUL_WEIGHT, type Klass, LADDER, PRIOR, replay, STORY, transition } from "@/components/explain/trust";

const REPO = join(import.meta.dir, "..", "..");
const cache = new Map<string, string>();
function read(file: string): string {
  let text = cache.get(file);
  if (text === undefined) {
    text = readFileSync(join(REPO, file), "utf8");
    cache.set(file, text);
  }
  return text;
}

/** A `pub const NAME: T = value;` from a Rust file, as a number. */
function rustConst(file: string, name: string): number {
  const match = read(file).match(new RegExp(`pub const ${name}: \\w+ = ([0-9._]+);`));
  if (!match?.[1]) throw new Error(`${file} has no pub const ${name}`);
  return Number(match[1].replace(/_/g, ""));
}

/** The text from `start` to the first unindented closing brace after it. */
function block(file: string, start: string): string {
  const text = read(file);
  const at = text.indexOf(start);
  if (at < 0) throw new Error(`${file} has no ${start}`);
  return text.slice(at, text.indexOf("\n}", at));
}

describe("decay (src/policy/memory_decay.rs)", () => {
  const FILE = "src/policy/memory_decay.rs";

  test("the demote and tombstone thresholds", () => {
    expect(DEMOTE).toBe(rustConst(FILE, "DEFAULT_DECAY_DEMOTE_THRESHOLD"));
    expect(FORGET).toBe(rustConst(FILE, "DEFAULT_DECAY_FORGET_THRESHOLD"));
  });

  test("both comparisons are strict, as evaluate_memory_decay_with_settings writes them", () => {
    const body = read(FILE);
    expect(body).toContain("if lifecycle_score < forget_threshold {");
    expect(body).toContain("} else if lifecycle_score < demote_threshold {");
    expect(classify(FORGET)).toBe("demote");
    expect(classify(FORGET - 1e-9)).toBe("tombstone");
    expect(classify(DEMOTE)).toBe("preserve");
    expect(classify(DEMOTE - 1e-9)).toBe("demote");
  });

  test("every half-life is MemoryDecayHalfLives::default()", () => {
    const defaults = new Map(
      [...block(FILE, "impl Default for MemoryDecayHalfLives").matchAll(/(\w+): ([0-9.]+),/g)].map((m) => [m[1], Number(m[2])]),
    );
    const FIELD: Record<(typeof KINDS)[number]["id"], string> = {
      working: "working",
      event: "episodic_event",
      failure: "episodic_failure",
      fact: "semantic_fact",
      rule: "procedural_rule",
    };
    for (const kind of KINDS) expect<number>(kind.half).toBe(defaults.get(FIELD[kind.id]) ?? Number.NaN);
  });

  test("demotion moves one level down, and only from procedural or semantic", () => {
    const body = block(FILE, "fn demoted_memory_level(");
    const rust = Object.fromEntries([...body.matchAll(/"(\w+)" => Some\("(\w+)"\)/g)].map((m) => [m[1], m[2]]));
    expect(DEMOTED).toEqual(rust);
  });
});

describe("trust (src/core/bayes.rs, src/models/trust.rs)", () => {
  const BAYES = "src/core/bayes.rs";

  test("the Jeffreys prior and the harmful weight", () => {
    expect(PRIOR).toBe(rustConst(BAYES, "DEFAULT_PRIOR_ALPHA"));
    expect(PRIOR).toBe(rustConst(BAYES, "DEFAULT_PRIOR_BETA"));
    expect(HARMFUL_WEIGHT).toBe(rustConst(BAYES, "DEFAULT_HARMFUL_WEIGHT"));
  });

  test("the interval is the equal-tailed 90% one: the prior's bounds are the arcsine quantiles", () => {
    expect(read(BAYES)).toContain("const TRUST_TRANSITION_CI_LEVEL: f64 = 0.90;");
    // Beta(1/2, 1/2) has CDF (2/π)·asin(√x), so its p-quantile is sin²(πp/2): a closed form for the bisection to hit.
    const [prior] = replay("agent_assertion", []);
    expect(prior?.lo).toBeCloseTo(Math.sin((Math.PI * 0.05) / 2) ** 2, 9);
    expect(prior?.hi).toBeCloseTo(Math.sin((Math.PI * 0.95) / 2) ** 2, 9);
    // Beta(1, b) has quantile 1 − (1 − p)^(1/b).
    expect(betaQuantile(0.05, 1, 3.5)).toBeCloseTo(1 - 0.95 ** (1 / 3.5), 9);
  });

  test("each class starts at src/models/trust.rs's initial_confidence", () => {
    const body = block("src/models/trust.rs", "pub const fn initial_confidence(");
    const rust = new Map(
      [...body.matchAll(/Self::(\w+) => ([0-9.]+),/g)].map((m) => [
        (m[1] ?? "").replace(/(?<!^)([A-Z])/g, "_$1").toLowerCase(),
        Number(m[2]),
      ]),
    );
    expect(rust.size).toBe(LADDER.length);
    for (const rung of LADDER) expect(rung.initial).toBe(rust.get(rung.id) ?? Number.NaN);
  });

  test("every interval threshold in trust_class_transition, its reason code and its target class", () => {
    const rules = [
      ...read(BAYES).matchAll(
        /if ci90_(lower|upper) ([<>]) (0\.\d+)[^"]*?TrustClassTransition::(promote|demote)\(\s*current_class,\s*TrustClass::(\w+),[^"]*"(\w+)"/g,
      ),
    ];
    // legacy, cass ×2, agent_assertion ×2, agent_validated, peer, human: eight interval rules.
    expect(rules).toHaveLength(8);
    const ids = LADDER.map((rung) => rung.id);
    for (const [, bound, , value, direction, target, reason] of rules) {
      const current = ids.find((id) => reason?.startsWith(`${id}_`)) as Klass;
      const next = (target ?? "").replace(/(?<!^)([A-Z])/g, "_$1").toLowerCase();
      const t = Number(value);
      // Just past the threshold the rule fires; exactly on it, it does not.
      const past = bound === "lower" ? transition(current, t + 1e-6, 1, 100, 1, false) : transition(current, 0, t - 1e-6, 100, 1, false);
      const on = bound === "lower" ? transition(current, t, 1, 100, 1, false) : transition(current, 0, t, 100, 1, false);
      expect({ reason: past.reason, next: past.next, direction: past.direction }).toEqual({
        reason: reason ?? "",
        next: next as Klass,
        direction: direction as "promote" | "demote",
      });
      expect(on.reason).not.toBe(reason);
    }
  });

  test("agent_assertion's promotion also needs α + β ≥ 6, and only a named human reaches human_explicit", () => {
    expect(read(BAYES)).toContain("ci90_lower > 0.70 && posterior.effective_sample_size() >= 6.0");
    expect(transition("agent_assertion", 0.8, 1, 5.99, 6, false).reason).toBe("agent_assertion_sample_size_gate_unmet");
    expect(transition("agent_validated", 0.99, 1, 100, 100, false).next).toBe("agent_validated");
    expect(transition("agent_validated", 0, 1, 1, 0, true).next).toBe("human_explicit");
    const outcome = read("src/core/outcome.rs");
    expect(outcome).toContain('let explicit_human_promotion = source_type == "human_explicit"');
  });

  test("the story: the sixth helpful outcome promotes, and harm later demotes", () => {
    const steps = replay("agent_assertion", STORY);
    const moves = steps.flatMap((step, index) =>
      step.transition && step.transition.direction !== "stable" ? [{ index, direction: step.transition.direction, klass: step.klass }] : [],
    );
    expect(moves[0]).toEqual({ index: 6, direction: "promote", klass: "agent_validated" });
    expect(moves[1]?.direction).toBe("demote");
  });
});

describe("the operating loop (01.3)", () => {
  /** The long flags a clap struct declares: `long = "name"`, or a bare `long` on `pub name:`. */
  function longFlags(struct: string): Map<string, boolean> {
    const body = block("src/cli/mod.rs", `pub struct ${struct} {`);
    const flags = new Map<string, boolean>();
    let from = 0;
    for (const field of body.matchAll(/\n\s*pub (\w+):/g)) {
      const attributes = body.slice(from, field.index);
      from = (field.index ?? 0) + field[0].length;
      const global = /global\s*=\s*true/.test(attributes);
      const named = attributes.match(/\blong\s*=\s*"([^"]+)"/);
      if (named?.[1]) flags.set(named[1], global);
      else if (/#\[arg\([^]*?\blong\b(?!\s*=)/.test(attributes)) flags.set((field[1] ?? "").replace(/_/g, "-"), global);
    }
    return flags;
  }

  const ARGS: Record<string, string> = {
    remember: "RememberArgs",
    journal: "JournalAppendArgs",
    outcome: "OutcomeArgs",
    cass: "CassImportArgs",
    pack: "PackArgs",
    recall: "RecallArgs",
    search: "SearchArgs",
    ask: "AskArgs",
    why: "WhyArgs",
    preflight: "PreflightGuardArgs",
  };

  test("every flag a command line uses is on that command's own Args struct or is global", () => {
    const globals = longFlags("Cli");
    for (const name of ["json", "workspace", "format"]) expect(globals.get(name)).toBe(true);
    for (const command of COMMANDS) {
      const own = longFlags(ARGS[command.id] ?? `<no Args struct for ${command.id}>`);
      for (const [, flag] of command.line.matchAll(/(?:^|\s)--([a-z][a-z-]*)/g)) {
        if (!own.has(flag ?? "") && globals.get(flag ?? "") !== true) {
          throw new Error(`${command.id}: --${flag} is neither on ${ARGS[command.id]} nor a global flag`);
        }
      }
    }
    // Teeth: a real flag on the wrong command is refused.
    expect(longFlags("PackArgs").has("budget-tokens")).toBe(false);
    expect(longFlags("Cli").has("max-tokens")).toBe(false);
  });

  test("the outcome card's harmful weight is DEFAULT_HARMFUL_WEIGHT", () => {
    const outcome = COMMANDS.find((command) => command.id === "outcome");
    expect(outcome?.what).toContain(`${rustConst("src/core/bayes.rs", "DEFAULT_HARMFUL_WEIGHT")} times`);
  });
});

describe("the incident card (04.2)", () => {
  const RECALL = "src/core/error_recall.rs";

  test("the masking matches canonical_message_template's own unit tests", () => {
    const a = maskedTemplate("cannot find value `foo` in path /a/b/c.rs:12:5");
    expect(a).toBe(maskedTemplate("cannot find value `bar` in path /x/y/z.rs:99:1"));
    expect(a).toContain("<id>");
    expect(a).toContain("<path>");
    const hex = maskedTemplate("Segmentation fault at 0xDEADBEEF after 4096 bytes");
    expect(hex).toContain("<hex>");
    expect(hex).toContain("<num>");
    expect(hex).not.toContain("4096");
    expect(maskedTemplate("trap at 0XFEEDFACE")).toBe("trap at <hex>");
    // Those three cases are the ones src/core/error_recall.rs asserts.
    for (const pinned of ["cannot find value `foo` in path /a/b/c.rs:12:5", "0xDEADBEEF after 4096 bytes", "trap at 0XFEEDFACE"]) {
      expect(read(RECALL)).toContain(pinned);
    }
  });

  test("lower-casing is ASCII only, and a typed <id> is not a backtick span", () => {
    expect(maskedTemplate("Ünïcode ÉRROR")).toBe("Ünïcode Érror");
    const tokens = template("got <id> not `Span`");
    expect(tokens.map((token) => token.rule)).toEqual(["lower", "lower", "lower", "id"]);
    expect(tokens[3]?.raw).toBe("`Span`");
  });

  test("go test: the sentence ee writes, masked", () => {
    expect(read("src/core/cass_error_recall_go.rs")).toContain('format!("go test failure: {package} test {test}")');
    expect(maskedTemplate(EXAMPLES.go.templateInput)).toBe("go test failure: <path> test testinvoicerounding");
    // A complete verbose report: the package summary line that go_terminal requires is there in both runs.
    const [failure, proof] = ["failure", "proof"].map((part) =>
      EXAMPLES.go.transcript.find((line) => line.part === part)?.text.split("\n"),
    );
    expect(failure?.[0]?.startsWith("=== RUN   ")).toBe(true);
    expect(failure).toContain("FAIL");
    expect(proof?.[0]?.startsWith("=== RUN   ")).toBe(true);
    expect(proof).toContain("PASS");
  });

  test("Vitest and Jest: the first line under the failing test's header, prefixed javascript-test:", () => {
    expect(read("src/core/cass_error_recall_js_tests.rs")).toContain('"javascript-test: {safe}"');
    const header = { vitest: /^ FAIL {2}/, jest: /^ {2}● / } as const;
    for (const id of ["vitest", "jest"] as const) {
      const example = EXAMPLES[id];
      expect(example.templateInput).toBe(`javascript-test: ${example.diagnostic}`);
      const lines = example.transcript.find((line) => line.part === "failure")?.text.split("\n") ?? [];
      const at = lines.findIndex((line) => header[id].test(line));
      expect(at).toBeGreaterThanOrEqual(0);
      const first = lines.slice(at + 1).find((line) => line.trim() !== "");
      expect(first?.trim()).toBe(example.diagnostic);
    }
  });

  test("a shell recall command never puts a backtick inside double quotes", () => {
    for (const example of Object.values(EXAMPLES)) expect(example.recall).not.toMatch(/"[^"]*`[^"]*"/);
  });

  test("the simhash distance and the card budget the figure prints", () => {
    const figure = read("web/components/explain/incident-card.tsx");
    const distance = rustConst(RECALL, "SIMHASH_TAIL_MAX_DISTANCE");
    expect(read(RECALL)).toContain("simhash_hamming_distance(left, right) <= max_distance");
    expect(read(RECALL)).toContain("simhash_128(");
    expect(figure).toContain(`within ${distance} of 128 simhash bits`);
    expect(figure).toContain(`≤ ${rustConst("src/core/incident_card.rs", "INCIDENT_CARD_TOKEN_BUDGET")} tokens`);
  });
});
