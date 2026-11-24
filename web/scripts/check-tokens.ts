// The site's half of tests/fleet_identifier_guard.rs, run before every build.
//
// That Rust test scans every tracked file in the repository, web/ included, for
// private fleet identifiers: host words, account words, a few substrings and
// globally routable IPv4 addresses, in file CONTENTS and in file PATHS. It only
// runs on the pinned remote lane, so a word written here would otherwise be
// found hours after it was committed. This script applies the same rules to
// web/'s own files (tracked, plus untracked ones git does not ignore) in well
// under a second.
//
// The token lists are READ FROM THE RUST FILE rather than copied here. Copying
// them would put every banned word into a tracked file of this site, which is
// the one thing the guard exists to prevent, and the lists would drift.
//
// One consequence shapes the whole site: one of the banned host words is the
// lowercase name of the stylesheet language, matched as a whole word and in
// paths. So no tracked file here is named with that extension and no source
// text spells it in lowercase; scripts/build-style.ts compiles
// styles/site.tailwind into the untracked .generated/ directory instead, and
// app/layout.tsx inlines the result.

import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { join, relative } from "node:path";

const WEB = join(import.meta.dir, "..");
const REPO = join(WEB, "..");
const GUARD = join(REPO, "tests", "fleet_identifier_guard.rs");

/** The string literals of one `const NAME: ... = &[ ... ];` array in the guard. */
function constant(source: string, name: string): string {
  const start = source.indexOf(`const ${name}:`);
  if (start < 0) throw new Error(`check-tokens: ${GUARD} has no const ${name}; the guard changed shape`);
  const end = source.indexOf("];", start);
  if (end < 0) throw new Error(`check-tokens: const ${name} is not terminated`);
  return source.slice(start, end);
}

const guard = readFileSync(GUARD, "utf8");

/** `("word", ignore_case)` pairs. */
const hostWords = [...constant(guard, "BANNED_HOST_WORDS").matchAll(/\("([^"]+)",\s*(true|false)\)/g)].map(
  (match) => ({ word: match[1] as string, ignoreCase: match[2] === "true" }),
);
const substrings = [...constant(guard, "BANNED_SUBSTRINGS").matchAll(/"([^"]+)"/g)].map((match) => match[1] as string);
const accountWords = [...constant(guard, "BANNED_ACCOUNT_WORDS").matchAll(/"([^"]+)"/g)].map(
  (match) => match[1] as string,
);
const allowedIps = new Set(
  [...constant(guard, "ALLOWLISTED_PUBLIC_IPS").matchAll(/\("([^"]+)",/g)].map((match) => match[1] as string),
);

// A parse that finds nothing would pass everything: fail closed instead.
if (hostWords.length === 0 || substrings.length === 0 || accountWords.length === 0) {
  throw new Error("check-tokens: parsed an empty token list from the guard; refusing to report a green");
}

const isWordByte = (code: number) =>
  (code >= 48 && code <= 57) || (code >= 65 && code <= 90) || (code >= 97 && code <= 122) || code === 95;

/** contains_word: delimited by non-word bytes, or preceded by a backslash escape. */
function containsWord(haystack: string, needle: string, ignoreCase: boolean): boolean {
  const text = ignoreCase ? haystack.toLowerCase() : haystack;
  let from = 0;
  for (;;) {
    const begin = text.indexOf(needle, from);
    if (begin < 0) return false;
    const end = begin + needle.length;
    const beforeOk = begin === 0 || !isWordByte(text.charCodeAt(begin - 1)) || (begin >= 2 && text[begin - 2] === "\\");
    const afterOk = end === text.length || !isWordByte(text.charCodeAt(end));
    if (beforeOk && afterOk) return true;
    from = begin + 1;
  }
}

function containsVpsHostname(line: string): boolean {
  return /(^|[^A-Za-z0-9_])vmi\d{6,}/.test(line.toLowerCase());
}

function isNonRoutable([a, b, c]: number[]): boolean {
  if (a === undefined || b === undefined || c === undefined) return true;
  if (a === 0 || a === 10 || a === 127) return true;
  if (a === 100 && b >= 64 && b <= 127) return true;
  if ((a === 169 && b === 254) || (a === 192 && b === 168)) return true;
  if (a === 172 && b >= 16 && b <= 31) return true;
  if ((a === 192 && b === 0 && (c === 0 || c === 2)) || (a === 192 && b === 88 && c === 99)) return true;
  if ((a === 198 && (b === 18 || b === 19)) || (a === 198 && b === 51 && c === 100) || (a === 203 && b === 0 && c === 113))
    return true;
  return a >= 224;
}

function publicIp(line: string): string | null {
  for (const match of line.matchAll(/[0-9.]+/g)) {
    const at = match.index ?? 0;
    const before = at === 0 ? "" : (line[at - 1] as string);
    if (before !== "" && (isWordByte(before.charCodeAt(0)) || before === ".")) continue;
    const candidate = match[0].replace(/\.+$/, "");
    const after = line[at + candidate.length];
    if (after !== undefined && isWordByte(after.charCodeAt(0))) continue;
    const parts = candidate.split(".");
    if (parts.length !== 4 || parts.some((part) => part === "" || part.length > 3 || Number(part) > 255)) continue;
    if (!isNonRoutable(parts.map(Number)) && !allowedIps.has(candidate)) return candidate;
  }
  return null;
}

function bannedToken(line: string): string | null {
  if (containsVpsHostname(line)) return "vmi<digits> host name";
  const address = publicIp(line);
  if (address) return `routable address ${address}`;
  for (const needle of substrings) if (line.includes(needle)) return needle;
  for (const { word, ignoreCase } of hostWords) if (containsWord(line, word, ignoreCase)) return word;
  for (const word of accountWords) if (containsWord(line, word, true)) return word;
  return null;
}

/** web/'s files as git will publish them: tracked, plus untracked files that are not ignored. */
function files(): string[] {
  const list = (args: string[]) =>
    execFileSync("git", ["-C", REPO, "ls-files", "-z", ...args, "--", "web"], { encoding: "utf8" })
      .split("\0")
      .filter(Boolean);
  return [...new Set([...list([]), ...list(["--others", "--exclude-standard"])])].sort();
}

const found: string[] = [];
const scanned = files();
for (const path of scanned) {
  // The token is reported by name only, never with the surrounding text: the
  // name is already public in the guard, the line might not be.
  const inPath = bannedToken(path);
  if (inPath) found.push(`${path}: (in the path itself) ${inPath}`);
  let text: string;
  try {
    text = readFileSync(join(REPO, path), "utf8");
  } catch {
    continue;
  }
  text.split("\n").forEach((line, index) => {
    const token = bannedToken(line);
    if (token) found.push(`${path}:${index + 1}: ${token}`);
  });
}

if (found.length > 0) {
  console.error(`check-tokens: ${found.length} line(s) would fail tests/fleet_identifier_guard.rs:`);
  for (const line of found) console.error(`  ${line}`);
  process.exit(1);
}
console.log(
  `check-tokens: ${scanned.length} file(s) under ${relative(REPO, WEB)}/ clear of the fleet guard's ${hostWords.length + substrings.length + accountWords.length} tokens and routable addresses`,
);
