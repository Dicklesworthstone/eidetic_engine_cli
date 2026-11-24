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

  test("the flags each command uses are declared in src/cli/mod.rs", () => {
    const cli = read("src/cli/mod.rs");
    for (const station of STATIONS) {
      for (const token of commandTokens(station.command ?? "")) {
        if (token.kind !== "flag") continue;
        const name = token.text.slice(2);
        const declared =
          cli.includes(`long = "${name}"`) ||
          cli.includes(`pub ${name.replace(/-/g, "_")}:`) ||
          (name === "json" && cli.includes("global = true"));
        if (!declared) throw new Error(`${station.id}: --${name} is not declared`);
      }
    }
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
