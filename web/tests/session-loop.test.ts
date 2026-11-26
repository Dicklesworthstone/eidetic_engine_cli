// `bun test`: the session loop's stations against the repository they describe. The figure is a
// diagram, not program output, but every command, field name and enum value it prints should still
// be true of the code: each station cites files, and every JSON key and identifier-like value in its
// excerpt must appear, quoted, in one of them. A rename in ee turns this red rather than the page
// quietly going stale.

import { describe, expect, test } from "bun:test";
import { existsSync, readFileSync } from "node:fs";
import { join } from "node:path";

import {
  commandTokens,
  jsonTokens,
  lastKey,
  lineKeys,
  ringGeometry,
  shortestDelta,
  STATION_COUNT,
  type Source,
  STATIONS,
} from "@/components/explain/session-loop-data";

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

/** Every response carries these: the envelope's own contract. */
const ENVELOPE = "docs/schemas/ee.response.v2.json";

/** A struct's body in src/cli/mod.rs, from its opening line to the first unindented closing brace. */
function cliStruct(name: string): string {
  const cli = read("src/cli/mod.rs");
  const start = cli.indexOf(`pub struct ${name} {`);
  if (start < 0) throw new Error(`src/cli/mod.rs has no struct ${name}`);
  return cli.slice(start, cli.indexOf("\n}", start));
}

/** A Rust fn's body, from its signature to the first unindented closing brace. */
function rustFn(file: string, signature: string): string {
  const text = read(file);
  const start = text.indexOf(signature);
  if (start < 0) throw new Error(`${file} has no ${signature}`);
  return text.slice(start, text.indexOf("\n}\n", start));
}

const stationById = (id: string) => {
  const station = STATIONS.find((entry) => entry.id === id);
  if (!station) throw new Error(`no station ${id}`);
  return station;
};

/** Everything a station's panel says in prose. */
const prose = (id: string) => {
  const station = stationById(id);
  return [station.returns, station.because, station.note ?? "", station.footnote ?? ""].join(" ");
};

/**
 * What a citation vouches for. A small file (a schema, a golden) is read whole. A large one, such as the
 * 3.9 MB src/cli/mod.rs, holds nearly every word somewhere, so only the lines around the cited mark count.
 */
function cited(source: Source): string {
  const text = read(source.file);
  if (text.length <= 200_000) return text;
  const lines = text.split("\n");
  const windows: string[] = [];
  lines.forEach((line, index) => {
    if (line.includes(source.mark)) windows.push(lines.slice(Math.max(0, index - 90), index + 90).join("\n"));
  });
  return windows.join("\n");
}

describe("the stations", () => {
  test("nine, in the loop's order, with unique ids", () => {
    expect(STATIONS.map((station) => station.id)).toEqual([
      "resume",
      "orient",
      "pack",
      "recall",
      "preflight",
      "work",
      "remember",
      "outcome",
      "handoff",
    ]);
    expect(STATION_COUNT).toBe(9);
  });

  test("every command is an ee command naming its station, and only the work has none", () => {
    for (const station of STATIONS) {
      if (station.id === "work") {
        expect(station.command).toBeNull();
        expect(station.json).toBeNull();
        expect(station.reads).toHaveLength(0);
        continue;
      }
      expect(station.command?.startsWith(`ee ${station.id} `)).toBe(true);
      expect(station.command).toContain("--json");
      expect(station.command).toContain("--workspace .");
    }
  });

  test("the effect matches src/core/effect.rs: reads before the work, writes after it", () => {
    const kinds = STATIONS.map((station) => station.effect);
    expect(kinds).toEqual(["read", "read", "read", "read", "read", "harness", "write", "write", "write"]);

    // Each command's CommandEffect constructor, as the manifest declares it.
    const manifest = read("src/core/effect.rs");
    const READS = new Set(["read_only", "read_only_db"]);
    const WRITES = new Set(["durable_write", "append_only_write", "workspace_file_write"]);
    for (const station of STATIONS) {
      if (station.command === null) continue;
      const name = commandTokens(station.command)
        .filter((token) => token.kind === "sub")
        .map((token) => token.text)
        .join(" ");
      // `ee pack "<task>"` is the `pack build` shim (src/cli/mod.rs maps PackCommand::Build | None to it).
      const key = name === "pack" ? "pack build" : name;
      const escaped = key.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
      const constructors = [...manifest.matchAll(new RegExp(`CommandEffect::(\\w+)\\(\\s*"${escaped}"`, "g"))].map(
        (match) => match[1] ?? "",
      );
      if (constructors.length === 0) throw new Error(`${station.id}: no CommandEffect for "${key}"`);
      for (const constructor of constructors) {
        if (station.effect === "write") {
          if (!WRITES.has(constructor)) throw new Error(`${station.id}: "${key}" is ${constructor}, not a write`);
        } else if (key === "pack build") {
          // An append-only write that --read-only turns off: the station must pass the flag.
          expect(constructor).toBe("append_only_write");
          expect(station.command).toContain("--read-only");
        } else if (!READS.has(constructor)) {
          throw new Error(`${station.id}: "${key}" is ${constructor}, not read-only`);
        }
      }
    }
  });

  test("every cited file exists and still holds the line it was cited for", () => {
    for (const station of STATIONS) {
      expect(station.sources.length).toBeGreaterThan(0);
      for (const source of station.sources) {
        expect(existsSync(join(REPO, source.file))).toBe(true);
        if (!read(source.file).includes(source.mark)) {
          throw new Error(`${station.id}: ${source.file} no longer contains ${JSON.stringify(source.mark)}`);
        }
      }
    }
  });
});

describe("the excerpts", () => {
  const withJson = STATIONS.filter((station) => station.json !== null);

  test("each is one balanced object", () => {
    for (const station of withJson) {
      const lines = station.json ?? [];
      expect(lines[0]?.text).toBe("{");
      expect(lines[lines.length - 1]?.text).toBe("}");
      let depth = 0;
      for (const line of lines) {
        for (const token of jsonTokens(line.text)) {
          if (token.kind !== "punct") continue;
          if (token.text === "{" || token.text === "[") depth += 1;
          if (token.text === "}" || token.text === "]") depth -= 1;
          expect(depth).toBeGreaterThanOrEqual(0);
        }
      }
      expect(depth).toBe(0);
    }
  });

  test("every path the agent reads ends on a highlighted key, and every highlight is a path it reads", () => {
    for (const station of withJson) {
      const lines = station.json ?? [];
      const hotKeys = new Set(lines.filter((line) => line.hot).flatMap((line) => lineKeys(line.text)));
      for (const path of station.reads) {
        if (!hotKeys.has(lastKey(path))) throw new Error(`${station.id}: ${path} has no highlighted line`);
      }
      const segments = new Set(station.reads.flatMap((path) => path.replace(/\[\]/g, "").split(".")));
      for (const line of lines.filter((entry) => entry.hot)) {
        const keys = lineKeys(line.text);
        if (!keys.some((key) => segments.has(key))) throw new Error(`${station.id}: stray highlight ${line.text}`);
      }
    }
  });

  test("every key and identifier-like value is spelled as the cited files spell it", () => {
    const identifier = /^[a-z][A-Za-z0-9_.:]*$/;
    for (const station of withJson) {
      const haystack = [read(ENVELOPE), ...station.sources.map(cited)].join("\n");
      for (const line of station.json ?? []) {
        for (const token of jsonTokens(line.text)) {
          if (token.kind !== "key" && token.kind !== "string") continue;
          const bare = token.text.slice(1, -1);
          if (token.kind === "string" && !identifier.test(bare)) continue;
          if (!haystack.includes(`"${bare}"`)) {
            throw new Error(`${station.id}: "${bare}" is not in ${station.sources.map((s) => s.file).join(", ")}`);
          }
        }
      }
    }
  });

  test("the check has teeth: a plausible misspelling is not in what the pack station cites", () => {
    const pack = STATIONS.find((station) => station.id === "pack");
    const haystack = [read(ENVELOPE), ...(pack?.sources ?? []).map(cited)].join("\n");
    expect(haystack.includes('"skippedTotal"')).toBe(true);
    expect(haystack.includes('"skipped_total"')).toBe(false);
    expect(haystack.includes('"omittedTotal"')).toBe(false);
  });

  test("the flags each command uses are declared in src/cli/mod.rs, on its own Args struct or as global flags", () => {
    const struct = cliStruct;
    /** `--name` as a clap long flag in `body`: `long = "name"`, or `#[arg(long …)]` on `pub name:`. */
    const declares = (body: string, name: string, global = false) =>
      (!global && body.includes(`long = "${name}"`)) ||
      new RegExp(`#\\[arg\\(long\\b[^\\]]*${global ? "global = true[^\\]]*" : ""}\\]\\s*pub ${name.replace(/-/g, "_")}:`).test(
        body,
      );
    const ARGS: Record<string, string> = {
      resume: "ResumeArgs",
      orient: "OrientArgs",
      pack: "PackArgs",
      recall: "RecallArgs",
      preflight: "PreflightGuardArgs",
      remember: "RememberArgs",
      outcome: "OutcomeArgs",
      handoff: "HandoffCreateArgs",
    };
    const globals = struct("Cli");
    for (const station of STATIONS) {
      if (station.command === null) continue;
      const own = struct(ARGS[station.id] ?? `<no Args struct for ${station.id}>`);
      for (const token of commandTokens(station.command)) {
        if (token.kind !== "flag") continue;
        const name = token.text.slice(2);
        if (!declares(own, name) && !declares(globals, name, true)) {
          throw new Error(`${station.id}: --${name} is neither on ${ARGS[station.id]} nor a global flag`);
        }
      }
    }
    // Teeth: a real flag on the wrong command, and a local flag passed off as global, are both refused.
    expect(declares(struct("PackArgs"), "budget-tokens")).toBe(false);
    expect(declares(globals, "max-tokens", true)).toBe(false);
  });

  test("the preflight excerpt's memory severity is the one ee infers from that memory's kind", () => {
    const body = rustFn("src/core/preflight_guard.rs", "fn inferred_trauma_guard_severity(");
    const severityOf = new Map<string, string>();
    let fallback: string | undefined;
    for (const [, left, severity] of body.matchAll(/^\s*(_|"[^\n]*?")\s*=>\s*"([a-z]+)",/gm)) {
      if (left === "_") fallback = severity;
      else for (const [, kind] of (left ?? "").matchAll(/"([a-z-]+)"/g)) severityOf.set(kind ?? "", severity ?? "");
    }
    expect(severityOf.size).toBeGreaterThanOrEqual(2);
    expect(fallback).toBeDefined();

    const line = (stationById("preflight").json ?? []).find((entry) => entry.text.includes('"matchedMemories"'));
    const kind = /"kind": "([a-z-]+)"/.exec(line?.text ?? "")?.[1];
    const severity = /"severity": "([a-z]+)"/.exec(line?.text ?? "")?.[1];
    expect(kind).toBeDefined();
    // Only these kinds are matched at all (trauma_guard_memory_kind).
    const matched = rustFn("src/core/preflight_guard.rs", "fn trauma_guard_memory_kind(");
    expect(matched).toContain(`"${kind}"`);
    expect(severity).toBe(severityOf.get(kind ?? "") ?? fallback);
  });
});

describe("the numbers in the prose", () => {
  test("resume's three sessions is ResumeArgs' default", () => {
    expect(prose("resume")).toContain("The last three sessions'");
    expect(cliStruct("ResumeArgs")).toMatch(/default_value_t = 3,[^\]]*\]\s*pub sessions: usize,/);
  });

  test("remember's 0.8 confidence is RememberArgs' default, and the excerpt shows it", () => {
    expect(prose("remember")).toContain("(0.8 unless you give one)");
    expect(stationById("remember").json?.some((line) => line.text.includes('"confidence": 0.8,'))).toBe(true);
    expect(cliStruct("RememberArgs")).toMatch(/#\[arg\(long, default_value = "0\.8"\)\]\s*pub confidence: f32,/);
  });

  test("outcome's five harmful reports an hour are the [feedback] defaults, and the sixth is the one held", () => {
    expect(prose("outcome")).toContain("By default, after five harmful reports in an hour from one source, the next is quarantined");
    const outcome = read("src/core/outcome.rs");
    expect(outcome).toContain("pub const DEFAULT_HARMFUL_PER_SOURCE_PER_HOUR: u32 = 5;");
    expect(outcome).toContain("pub const DEFAULT_HARMFUL_BURST_WINDOW_SECONDS: u32 = 3600;");
    // An omitted flag follows the merged [feedback] value (fe120811d), built in as 5 per 3600 s,
    // and falls back to the constants above.
    const args = cliStruct("OutcomeArgs");
    expect(args).toMatch(/pub harmful_per_source_per_hour: Option<u32>,/);
    expect(args).toMatch(/pub harmful_burst_window_seconds: Option<u32>,/);
    const merge = read("src/config/merge.rs");
    expect(merge).toContain("harmful_per_source_per_hour: Some(5),");
    expect(merge).toContain("harmful_burst_window_seconds: Some(3600),");
    const limits = rustFn("src/core/config_surface.rs", "fn harmful_feedback_limits_from(");
    expect(limits).toContain(".unwrap_or(crate::core::outcome::DEFAULT_HARMFUL_PER_SOURCE_PER_HOUR)");
    expect(limits).toContain(".unwrap_or(crate::core::outcome::DEFAULT_HARMFUL_BURST_WINDOW_SECONDS)");
    // Five already counted pass; the one after them is quarantined.
    expect(rustFn("src/core/outcome.rs", "fn harmful_quarantine_preview(")).toContain("if existing_count < limit {");
  });

  test("pack's 50 listed omissions is MAX_PACK_SKIPPED_ITEMS, ranked by relevance plus utility", () => {
    expect(prose("pack")).toContain("the 50 with the highest relevance plus utility are listed");
    const pack = read("src/pack/mod.rs");
    expect(pack).toContain("pub const MAX_PACK_SKIPPED_ITEMS: usize = 50;");
    const order = rustFn("src/pack/mod.rs", "fn compare_omissions_for_output(");
    expect(order).toContain("left.relevance.into_inner() + left.utility.into_inner()");
    expect(order).toContain("right_score\n        .total_cmp(&left_score)");
  });
});

describe("the ring", () => {
  const overlap = (a: { x: number; y: number; w: number; h: number }, b: typeof a) =>
    a.x < b.x + b.w && b.x < a.x + a.w && a.y < b.y + b.h && b.y < a.y + a.h;

  test("labels stay inside the figure and clear of each other at every width it is drawn at", () => {
    for (let width = 420; width <= 640; width += 10) {
      const ring = ringGeometry(width);
      ring.nodes.forEach((node, index) => {
        expect(node.box.x).toBeGreaterThanOrEqual(0);
        expect(node.box.x + node.box.w).toBeLessThanOrEqual(ring.W);
        expect(node.box.y).toBeGreaterThanOrEqual(0);
        expect(node.box.y + node.box.h).toBeLessThanOrEqual(ring.H);
        ring.nodes.forEach((other, otherIndex) => {
          if (otherIndex <= index) return;
          if (overlap(node.box, other.box)) throw new Error(`width ${width}: labels ${index} and ${otherIndex} overlap`);
        });
        // A label never sits on a node.
        for (const other of ring.nodes) {
          const nodeBox = { x: other.x - ring.nodeR, y: other.y - ring.nodeR, w: 2 * ring.nodeR, h: 2 * ring.nodeR };
          if (overlap(node.box, nodeBox)) throw new Error(`width ${width}: label ${index} covers a node`);
        }
      });
    }
  });

  test("the marker takes the short way round, and goes forward from the handoff to the next resume", () => {
    expect(shortestDelta(8, 0)).toBe(1);
    expect(shortestDelta(0, 8)).toBe(-1);
    expect(shortestDelta(0, 4)).toBe(4);
    expect(shortestDelta(0, 5)).toBe(-4);
    expect(shortestDelta(3, 3)).toBe(0);
  });
});

describe("the words", () => {
  test("no station text uses a word the fleet guard bans", () => {
    const banned = new RegExp(`\\b${"c"}${"ss"}\\b`);
    expect(banned.test(JSON.stringify(STATIONS))).toBe(false);
  });
});
