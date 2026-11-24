// `bun test`: the recovery explorer (components/explain/agent-contract.tsx) quotes ee's JSON as
// "From the record". These checks hold it to that. A pinned record must equal its snapshot or
// golden field for field, with nothing left out unless a "…" marks the cut. A record built from
// the source must use only keys and strings the named files contain. The exit codes, the risk
// classes and the next-action rule must match the Rust source and the failure-mode fixtures.

import { describe, expect, test } from "bun:test";
import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";

import {
  at,
  ELIDED,
  EXIT_CODES,
  firstAction,
  type Json,
  nextAction,
  RISK_CLASSES,
  type RiskClass,
  RULE_ROWS,
  SITUATIONS,
} from "@/components/explain/agent-contract-data";

const REPO = join(import.meta.dir, "..", "..");
const read = (path: string) => readFileSync(join(REPO, path), "utf8");

/** A record file as JSON: an insta snapshot loses its `---` header first. */
function recordFile(path: string): Json {
  const text = read(path);
  if (path.endsWith(".snap")) {
    const body = text.split(/^---$/m);
    return JSON.parse(body.slice(2).join("---")) as Json;
  }
  return JSON.parse(text) as Json;
}

const isObject = (value: Json | undefined): value is { [key: string]: Json } =>
  value !== null && value !== undefined && typeof value === "object" && !Array.isArray(value);

const isCut = (value: Json) => isObject(value) && Object.keys(value).length === 1 && value[ELIDED] === true;

/**
 * Whether `ours` is `theirs` with only marked cuts. Objects without the mark must have exactly the
 * same keys, in the same order; with it, a subset in the same order. Arrays without a cut item
 * must match item for item; with one, the uncut items must match the first items of `theirs`.
 */
function sameRecord(ours: Json, theirs: Json | undefined, path: string, problems: string[]) {
  if (theirs === undefined) {
    problems.push(`${path}: not in the file`);
    return;
  }
  if (Array.isArray(ours)) {
    if (!Array.isArray(theirs)) {
      problems.push(`${path}: an array here, not in the file`);
      return;
    }
    const kept = ours.filter((item) => !isCut(item));
    const cut = kept.length !== ours.length;
    if (!cut && kept.length !== theirs.length) problems.push(`${path}: ${kept.length} items, the file has ${theirs.length}`);
    if (cut && kept.length > theirs.length) problems.push(`${path}: more items than the file`);
    kept.forEach((item, index) => sameRecord(item, theirs[index], `${path}/${index}`, problems));
    return;
  }
  if (isObject(ours)) {
    if (!isObject(theirs)) {
      problems.push(`${path}: an object here, not in the file`);
      return;
    }
    const cut = ours[ELIDED] === true;
    const keys = Object.keys(ours).filter((key) => key !== ELIDED);
    const theirKeys = Object.keys(theirs);
    if (!cut) {
      const missing = theirKeys.filter((key) => !(key in ours));
      if (missing.length > 0) problems.push(`${path}: leaves out ${missing.join(", ")} without a "…"`);
    }
    const order = theirKeys.filter((key) => keys.includes(key));
    if (order.join(",") !== keys.filter((key) => key in theirs).join(",")) problems.push(`${path}: keys out of the file's order`);
    for (const key of keys) sameRecord(ours[key] as Json, theirs[key], `${path}/${key}`, problems);
    return;
  }
  if (ours !== theirs) problems.push(`${path}: ${JSON.stringify(ours)} here, ${JSON.stringify(theirs)} in the file`);
}

/** Every key and every string leaf of a record (cuts excluded). */
function leaves(value: Json, keys: Set<string>, strings: Set<string>) {
  if (typeof value === "string") strings.add(value);
  else if (Array.isArray(value)) for (const item of value) leaves(item, keys, strings);
  else if (isObject(value))
    for (const [key, child] of Object.entries(value)) {
      if (key === ELIDED) continue;
      keys.add(key);
      leaves(child, keys, strings);
    }
}

/** Rust source with string continuations (`\` + newline + indent) joined, as the compiler reads them. */
const rustText = (text: string) => text.replace(/\\\n\s*/g, "");

describe("the recovery explorer's records", () => {
  for (const situation of SITUATIONS) {
    if (situation.source.kind === "pinned") {
      test(`${situation.id}: equals ${situation.source.file}, with every cut marked`, () => {
        const problems: string[] = [];
        sameRecord(situation.record, recordFile(situation.source.file), "", problems);
        expect(problems).toEqual([]);
      });
    } else {
      test(`${situation.id}: every key and string comes from the files it names`, () => {
        const files = situation.source.files ?? [];
        expect(files.length).toBeGreaterThan(0);
        const corpus = files.map((file) => rustText(read(file))).join("\n");
        const keys = new Set<string>();
        const strings = new Set<string>();
        leaves(situation.record, keys, strings);
        const missingKeys = [...keys].filter((key) => !corpus.includes(`"${key}"`));
        const missingText = [...strings].flatMap((value) =>
          value
            .split(/<[^>]+>/)
            .filter((fragment) => fragment.trim().length >= 4)
            .filter((fragment) => !corpus.includes(fragment))
            .map((fragment) => `${JSON.stringify(fragment)} (from ${JSON.stringify(value)})`),
        );
        expect(missingKeys).toEqual([]);
        expect(missingText).toEqual([]);
      });

      test(`${situation.id}: agrees with its failure-mode fixture`, () => {
        const fixture = JSON.parse(read(situation.source.file)) as {
          code: string;
          severity: string;
          expected_emission: { message_contains?: string[]; repair_contains?: string };
        };
        const entry = at(situation.record, situation.entry) as { [key: string]: Json };
        expect(entry.code).toBe(fixture.code);
        expect(entry.severity).toBe(fixture.severity);
        for (const part of fixture.expected_emission.message_contains ?? []) expect(String(entry.message)).toContain(part);
        if (fixture.expected_emission.repair_contains) {
          expect(String(entry.repair)).toContain(fixture.expected_emission.repair_contains);
        }
      });
    }

    test(`${situation.id}: the first recovery entry leads where the page says`, () => {
      expect(firstAction(situation)?.next ?? null).toBe(situation.expectNext);
    });
  }
});

describe("the contract the records follow", () => {
  const models = read("src/models/mod.rs");

  test("the envelope schema ids are the source's constants", () => {
    expect(models).toContain('pub const ERROR_SCHEMA_V2: &str = "ee.error.v2";');
    expect(models).toContain('pub const RESPONSE_SCHEMA_V2: &str = "ee.response.v2";');
    for (const situation of SITUATIONS) {
      const schema = at(situation.record, ["schema"]);
      expect(schema).toBe(situation.exit === 0 ? "ee.response.v2" : "ee.error.v2");
      if (situation.exit === 0) expect(at(situation.record, ["success"])).toBe(true);
    }
  });

  test("the exit codes are ProcessExitCode, every variant and value", () => {
    const body = models.slice(models.indexOf("pub enum ProcessExitCode {"));
    const block = body.slice(0, body.indexOf("\n}"));
    const parsed = [...block.matchAll(/^\s*(\w+) = (\d+),/gm)].map((match) => ({
      variant: match[1] ?? "",
      code: Number(match[2]),
    }));
    expect(parsed.length).toBeGreaterThan(10);
    expect(EXIT_CODES.map(({ variant, code }) => ({ variant, code }))).toEqual(parsed);
  });

  test("each error record's code maps to the exit code the page shows", () => {
    const codeOf = new Map(
      [...models.matchAll(/Self::(\w+) \{ \.\. \} => "([a-z_]+)",/g)].map((match) => [match[2], match[1]]),
    );
    const exitOf = new Map(
      [...models.matchAll(/Self::(\w+) \{ \.\. \} => ProcessExitCode::(\w+),/g)].map((match) => [match[1], match[2]]),
    );
    for (const situation of SITUATIONS.filter((entry) => entry.exit !== 0)) {
      const code = String(at(situation.record, ["error", "code"]));
      const variant = exitOf.get(codeOf.get(code) ?? "");
      expect(EXIT_CODES.find((entry) => entry.variant === variant)?.code).toBe(situation.exit);
    }
  });

  test("risk classes, kinds and required fields follow docs/schemas/ee.error.v2.json", () => {
    const schema = JSON.parse(read("docs/schemas/ee.error.v2.json")) as {
      $defs: { recoveryAction: { required: string[]; properties: { riskClass: { enum: string[] }; kind: { enum: string[] } } } };
    };
    const action = schema.$defs.recoveryAction;
    expect<string[]>([...RISK_CLASSES]).toEqual(action.properties.riskClass.enum);
    expect<string[]>(RULE_ROWS.map((row) => row.riskClass)).toEqual([...RISK_CLASSES]);
    for (const situation of SITUATIONS) {
      const recovery = situation.recovery ? at(situation.record, situation.recovery) : undefined;
      for (const entry of Array.isArray(recovery) ? recovery : []) {
        if (!isObject(entry)) continue;
        for (const field of action.required) expect(Object.keys(entry)).toContain(field);
        expect(action.properties.kind.enum).toContain(entry.kind as string);
        expect(action.properties.riskClass.enum).toContain(entry.riskClass as string);
      }
    }
  });

  test("the next-action rule reproduces every classification the fixtures pin", () => {
    const guard = read("src/core/preflight_guard.rs");
    expect(guard).toContain("fn repair_command_preflight_policy(");
    const dir = join(REPO, "tests/fixtures/failure_modes");
    let checked = 0;
    for (const name of readdirSync(dir).filter((file) => file.endsWith(".json"))) {
      const fixture = JSON.parse(readFileSync(join(dir, name), "utf8")) as {
        expected_emission?: {
          repair_safety?: { riskClass: RiskClass; preflightCommand: string | null; requiresHumanApproval: boolean; nextAction: string }[];
        };
      };
      for (const safety of fixture.expected_emission?.repair_safety ?? []) {
        expect(nextAction(safety.riskClass, safety.preflightCommand !== null, safety.requiresHumanApproval)).toBe(
          safety.nextAction as ReturnType<typeof nextAction>,
        );
        checked += 1;
      }
    }
    expect(checked).toBeGreaterThan(10);
  });
});
