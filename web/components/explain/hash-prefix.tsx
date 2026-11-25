"use client";

import { Fragment, useId, useState } from "react";

import { DemoFrame, Readout, Segmented } from "@/components/explain/demo";
import { cn } from "@/lib/utils";

// ─── BLAKE3, the hash ee uses, for inputs up to one chunk (1,024 bytes) ────────
// The reference algorithm's single-chunk path (chunk counter 0, the last block
// flagged CHUNK_END | ROOT). Checked against the official test vectors for the
// empty input, "abc", and the 1, 64, 65, 1,023 and 1,024-byte vectors. The
// fields below are capped well under one chunk, so nothing here needs the tree.

const IV = [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19];
const PERMUTATION = [2, 6, 3, 10, 7, 0, 4, 13, 1, 11, 12, 5, 9, 14, 15, 8];
const CHUNK_START = 1;
const CHUNK_END = 2;
const ROOT = 8;

const rotr = (x: number, n: number) => (x >>> n) | (x << (32 - n));

function mix(s: number[], a: number, b: number, c: number, d: number, x: number, y: number) {
  s[a] = (s[a]! + s[b]! + x) | 0;
  s[d] = rotr(s[d]! ^ s[a]!, 16);
  s[c] = (s[c]! + s[d]!) | 0;
  s[b] = rotr(s[b]! ^ s[c]!, 12);
  s[a] = (s[a]! + s[b]! + y) | 0;
  s[d] = rotr(s[d]! ^ s[a]!, 8);
  s[c] = (s[c]! + s[d]!) | 0;
  s[b] = rotr(s[b]! ^ s[c]!, 7);
}

function compress(cv: number[], block: number[], length: number, flags: number): number[] {
  const s = [...cv, IV[0]!, IV[1]!, IV[2]!, IV[3]!, 0, 0, length, flags];
  let m = block;
  for (let round = 0; round < 7; round++) {
    mix(s, 0, 4, 8, 12, m[0]!, m[1]!);
    mix(s, 1, 5, 9, 13, m[2]!, m[3]!);
    mix(s, 2, 6, 10, 14, m[4]!, m[5]!);
    mix(s, 3, 7, 11, 15, m[6]!, m[7]!);
    mix(s, 0, 5, 10, 15, m[8]!, m[9]!);
    mix(s, 1, 6, 11, 12, m[10]!, m[11]!);
    mix(s, 2, 7, 8, 13, m[12]!, m[13]!);
    mix(s, 3, 4, 9, 14, m[14]!, m[15]!);
    if (round < 6) m = PERMUTATION.map((index) => m[index]!);
  }
  return Array.from({ length: 8 }, (_, i) => (s[i]! ^ s[i + 8]!) >>> 0);
}

/** BLAKE3-256 of at most 1,024 bytes, as lowercase hex. */
export function blake3(bytes: Uint8Array): string {
  const input = bytes.length > 1024 ? bytes.subarray(0, 1024) : bytes;
  let cv = IV.slice();
  const blocks = Math.max(1, Math.ceil(input.length / 64));
  for (let b = 0; b < blocks; b++) {
    const chunk = input.subarray(b * 64, b * 64 + 64);
    const words = new Array<number>(16).fill(0);
    for (let i = 0; i < chunk.length; i++) words[i >> 2]! |= chunk[i]! << (8 * (i & 3));
    const flags = (b === 0 ? CHUNK_START : 0) | (b === blocks - 1 ? CHUNK_END | ROOT : 0);
    cv = compress(cv, words, chunk.length, flags);
  }
  return cv.map((w) => [0, 8, 16, 24].map((shift) => ((w >>> shift) & 255).toString(16).padStart(2, "0")).join("")).join("");
}

// ─── ee's field encoding ─────────────────────────────────────────────────────

const encoder = new TextEncoder();

/** `hash_labeled_bytes`: u64 little-endian label length, label, u64 little-endian value length, value. */
function u64le(value: number): Uint8Array {
  const out = new Uint8Array(8);
  let rest = value;
  for (let i = 0; i < 8; i++) {
    out[i] = rest & 255;
    rest = Math.floor(rest / 256);
  }
  return out;
}

type Segment = { kind: "length" | "label" | "uri" | "note"; bytes: Uint8Array; text: string };

export function flatFeed(uri: string, note: string): Segment[] {
  return [
    { kind: "uri", bytes: encoder.encode(uri), text: uri },
    { kind: "note", bytes: encoder.encode(note), text: note },
  ];
}

/** The labels ee gives a selected item's provenance fields (`hash_pack_hash_provenance(hasher, "item.provenance", ...)`). */
export const PROVENANCE_LABELS = { uri: "item.provenance.uri", note: "item.provenance.note" } as const;

export function labeledFeed(uri: string, note: string): Segment[] {
  const field = (label: string, kind: "uri" | "note", value: string): Segment[] => {
    const labelBytes = encoder.encode(label);
    const valueBytes = encoder.encode(value);
    return [
      { kind: "length", bytes: u64le(labelBytes.length), text: String(labelBytes.length) },
      { kind: "label", bytes: labelBytes, text: label },
      { kind: "length", bytes: u64le(valueBytes.length), text: String(valueBytes.length) },
      { kind, bytes: valueBytes, text: value },
    ];
  };
  return [...field(PROVENANCE_LABELS.uri, "uri", uri), ...field(PROVENANCE_LABELS.note, "note", note)];
}

export function concat(segments: Segment[]): Uint8Array {
  const total = segments.reduce((sum, segment) => sum + segment.bytes.length, 0);
  const out = new Uint8Array(total);
  let at = 0;
  for (const segment of segments) {
    out.set(segment.bytes, at);
    at += segment.bytes.length;
  }
  return out;
}

const hex = (bytes: Uint8Array) => Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join(" ");

// ─── Q20.12 ──────────────────────────────────────────────────────────────────

/**
 * `q20_12_le_bytes`: the scores ee hashes are f32, widened to f64 and passed to `quantize_q20_12`, which is
 * round(value × 4096) clamped to u32. `Math.fround` gives the f32 the decimal would be stored as.
 */
const scaled = (value: number) => Math.fround(value) * 4096;
export const quantize = (value: number) => {
  const rounded = Math.round(scaled(value));
  return Number.isFinite(rounded) ? Math.min(0xffffffff, Math.max(0, rounded)) : 0;
};

/** One f32 step above 0.75 is 2^-24, about 0.00000006. */
export const PAIRS = {
  noise: { label: "Float noise", a: 0.75, b: 0.75000006 },
  quantum: { label: "Under a quantum", a: 0.75, b: 0.7501 },
  halfStep: { label: "Across a half-step", a: 0.75012, b: 0.75013 },
} as const;
type PairKey = keyof typeof PAIRS;

type Mode = "flat" | "labeled";

const SEGMENT_STYLE: Record<Segment["kind"], string> = {
  length: "border-white/10 bg-white/[0.03] text-slate-500",
  label: "border-white/15 bg-white/[0.05] text-slate-300",
  uri: "border-sky-400/35 bg-sky-400/10 text-sky-200",
  note: "border-amber-400/35 bg-amber-400/10 text-amber-200",
};

/**
 * Two provenance fields side by side, and the exact bytes they feed the hash. ee also feeds the provenance
 * count and the item's other fields around them; those are the same for both packs here, so they are not drawn.
 */
function PackColumn({
  name,
  uri,
  note,
  mode,
  onUri,
  onNote,
  digest,
  bytes,
}: {
  name: string;
  uri: string;
  note: string;
  mode: Mode;
  onUri: (value: string) => void;
  onNote: (value: string) => void;
  digest: string;
  bytes: number;
}) {
  const id = useId();
  const segments = mode === "flat" ? flatFeed(uri, note) : labeledFeed(uri, note);
  const input =
    "mt-1.5 w-full rounded-lg border border-white/10 bg-black/40 px-2.5 py-2 font-mono text-[13px] text-white outline-none transition focus:border-violet-400/60";
  return (
    <div className="min-w-0 rounded-2xl border border-white/[0.08] bg-black/25 p-4 sm:p-5">
      <p className="font-mono text-[10px] font-medium tracking-[0.22em] text-slate-400 uppercase">{name}</p>
      <div className="mt-3 grid grid-cols-2 gap-3">
        <label className="min-w-0" htmlFor={`${id}-uri`}>
          <span className="text-[11px] text-sky-200/90">provenance uri</span>
          <input
            id={`${id}-uri`}
            className={input}
            value={uri}
            maxLength={48}
            spellCheck={false}
            autoComplete="off"
            onChange={(event) => onUri(event.target.value)}
          />
        </label>
        <label className="min-w-0" htmlFor={`${id}-note`}>
          <span className="text-[11px] text-amber-200/90">note</span>
          <input
            id={`${id}-note`}
            className={input}
            value={note}
            maxLength={48}
            spellCheck={false}
            autoComplete="off"
            onChange={(event) => onNote(event.target.value)}
          />
        </label>
      </div>

      <p className="mt-4 text-[9px] font-black tracking-[0.2em] text-slate-500 uppercase">Bytes fed to the hash</p>
      <div className="mt-2 flex min-h-[3.25rem] flex-wrap items-center gap-y-1.5" role="group" aria-label={`${name}: bytes fed to the hash`}>
        {segments.map((segment, index) => (
          <Fragment key={index}>
            <span
              className={cn(
                "inline-flex min-h-7 items-center border px-1.5 font-mono text-[12px] leading-none whitespace-pre",
                SEGMENT_STYLE[segment.kind],
                // A flat feed's fields touch: nothing in the bytes marks where one ends.
                mode === "flat"
                  ? index === 0
                    ? "rounded-l-md border-r-0"
                    : "rounded-r-md border-l-0"
                  : "mr-1 rounded-md",
              )}
              title={segment.kind === "length" ? `u64 little-endian: ${hex(segment.bytes)}` : undefined}
            >
              {segment.kind === "length" ? (
                <>
                  <span className="mr-1 text-[9px] tracking-wider text-slate-600 uppercase">len</span>
                  {segment.text}
                </>
              ) : (
                segment.text || <span className="text-slate-600 italic">empty</span>
              )}
            </span>
          </Fragment>
        ))}
      </div>
      <p className="mt-3 font-mono text-[11px] text-slate-500 tabular-nums">
        {bytes} bytes · blake3
      </p>
      <p className="mt-1 font-mono text-[12px] leading-relaxed break-all text-slate-200 tabular-nums">
        <span className="text-white">{digest.slice(0, 16)}</span>
        <span className="text-slate-500">{digest.slice(16)}</span>
      </p>
    </div>
  );
}

/**
 * Why ee length-prefixes every field it hashes. Version 1 of the pack hash fed a provenance's uri and note as
 * adjacent raw bytes, so ("file://a", "bc") and ("file://ab", "c") produced one hash; ee's regression test for
 * it is named below. The digests here are real BLAKE3, computed in this page.
 */
export function HashPrefix() {
  const [mode, setMode] = useState<Mode>("flat");
  const [a, setA] = useState({ uri: "file://a", note: "bc" });
  const [b, setB] = useState({ uri: "file://ab", note: "c" });
  const [pair, setPair] = useState<PairKey>("noise");

  const feed = mode === "flat" ? flatFeed : labeledFeed;
  const bytesA = concat(feed(a.uri, a.note));
  const bytesB = concat(feed(b.uri, b.note));
  const digestA = blake3(bytesA);
  const digestB = blake3(bytesB);
  const sameInputs = a.uri === b.uri && a.note === b.note;
  const collide = digestA === digestB;
  const verdict = sameInputs ? "same" : collide ? "collision" : "distinct";

  const chosen = PAIRS[pair];
  const qa = quantize(chosen.a);
  const qb = quantize(chosen.b);
  const leBytes = (value: number) => hex(Uint8Array.from([0, 8, 16, 24], (shift) => (value >>> shift) & 255));

  return (
    <DemoFrame
      eyebrow="Pack identity · ee.pack.hash_input.v7"
      title="The bytes behind a pack's hash."
      basis="rule"
      source={
        <>
          src/core/context.rs · hash_labeled_bytes, hash_pack_hash_provenance (the labels item.provenance.uri and
          item.provenance.note), compute_context_pack_hash_components (ten components: request,
          items, omitted, degraded, effective_config, quality_scoring, reference_time, rendered_text, store_scopes,
          coordination) · src/core/context_test_module.rs · pack_hash_v2_separates_flat_feed_provenance_collision ·
          src/pack/mod.rs · quantize_q20_12 · docs/adr/0087-canonical-deterministic-response-contract.md
        </>
      }
    >
      <p className="max-w-3xl text-[15px] leading-relaxed text-slate-300">
        A pack&apos;s hash is a BLAKE3 digest of ten component digests (nine when the pack leaves out its skipped
        list), each over labeled fields. The first version fed a provenance&apos;s uri and note as raw adjacent
        bytes, so two different packs could get the same hash. Edit the fields below; the digests are BLAKE3,
        computed in your browser.
      </p>

      <div className="mt-6 flex flex-wrap items-center gap-3">
        <Segmented
          label="How fields are fed to the hash"
          value={mode}
          onChange={setMode}
          options={[
            { value: "flat", label: "v1 · flat feed", tone: "wrong" },
            { value: "labeled", label: "v7 · labeled, length-prefixed", tone: "rule" },
          ]}
        />
        <span className="font-mono text-[11px] text-slate-500">
          {mode === "flat" ? "uri ‖ note" : "len(label) ‖ label ‖ len(value) ‖ value, per field"}
        </span>
      </div>

      <div className="mt-5 grid grid-cols-1 gap-3 lg:grid-cols-2">
        <PackColumn
          name="Pack A"
          uri={a.uri}
          note={a.note}
          mode={mode}
          onUri={(uri) => setA((prev) => ({ ...prev, uri }))}
          onNote={(note) => setA((prev) => ({ ...prev, note }))}
          digest={digestA}
          bytes={bytesA.length}
        />
        <PackColumn
          name="Pack B"
          uri={b.uri}
          note={b.note}
          mode={mode}
          onUri={(uri) => setB((prev) => ({ ...prev, uri }))}
          onNote={(note) => setB((prev) => ({ ...prev, note }))}
          digest={digestB}
          bytes={bytesB.length}
        />
      </div>

      <div
        role="status"
        className={cn(
          "mt-3 flex flex-wrap items-center gap-x-4 gap-y-2 rounded-2xl border px-4 py-3",
          verdict === "collision" && "border-rose-400/40 bg-rose-400/[0.08]",
          verdict === "distinct" && "border-violet-400/35 bg-violet-400/[0.07]",
          verdict === "same" && "border-white/10 bg-white/[0.03]",
        )}
      >
        <span
          className={cn(
            "rounded-full px-2.5 py-0.5 text-[10px] font-black tracking-[0.16em] uppercase",
            verdict === "collision" && "bg-rose-400 text-black",
            verdict === "distinct" && "bg-violet-400 text-black",
            verdict === "same" && "bg-white/15 text-white",
          )}
        >
          {verdict === "collision" ? "Same hash" : verdict === "distinct" ? "Distinct" : "Same pack"}
        </span>
        <span className="min-w-0 flex-1 text-sm text-slate-300">
          {verdict === "collision"
            ? "These packs differ but get the same hash, so replay, caching and audit would treat them as one pack."
            : verdict === "distinct"
              ? mode === "labeled"
                ? "Each field is preceded by its length, so moving characters between fields changes the bytes."
                : "These two feed different bytes. Move a character from one field to the other to make them collide."
              : "The inputs are identical, so the bytes and the hash are identical too."}
        </span>
      </div>

      {/* Q20.12: the second half of determinism. */}
      <div className="mt-8 border-t border-white/[0.06] pt-6">
        <div className="flex flex-wrap items-end justify-between gap-3">
          <div className="max-w-2xl">
            <p className="text-[9px] font-black tracking-[0.2em] text-[color:var(--chapter,#a78bfa)] uppercase">
              Scores, quantized before hashing
            </p>
            <p className="mt-2 text-sm leading-relaxed text-slate-400">
              Every score the hash consumes is first rounded to Q20.12 fixed point (round(x × 4096) as a u32), so
              floating-point noise far below a quantum (1/4096 ≈ 0.000244) usually leaves the bytes, and the hash,
              unchanged. The exception is two scores on either side of a half-step: they round to different quanta
              however close they are.
            </p>
          </div>
          <Segmented
            label="Pair of scores"
            value={pair}
            onChange={setPair}
            options={(Object.keys(PAIRS) as PairKey[]).map((key) => ({ value: key, label: PAIRS[key].label }))}
          />
        </div>
        <div className="mt-4 grid grid-cols-2 gap-2 sm:grid-cols-4">
          <Readout label="Score A (f32)" value={chosen.a.toString()} note={`× 4096 = ${scaled(chosen.a).toFixed(4)}`} />
          <Readout label="Score B (f32)" value={chosen.b.toString()} note={`× 4096 = ${scaled(chosen.b).toFixed(4)}`} />
          <Readout label="Q20.12 A" value={qa} note={<span className="font-mono">{leBytes(qa)}</span>} />
          <Readout
            label="Q20.12 B"
            value={qb}
            tone={qa === qb ? "rule" : "wrong"}
            note={
              <span className="font-mono">
                {leBytes(qb)} · {qa === qb ? "same bytes" : "different bytes"}
              </span>
            }
          />
        </div>
      </div>

      <div className="mt-6 grid grid-cols-1 gap-3 text-[13px] leading-snug sm:grid-cols-2">
        <div className="rounded-xl border border-emerald-300/20 bg-emerald-300/[0.04] px-4 py-3">
          <p className="font-mono text-[10.5px] tracking-[0.16em] text-emerald-200/90 uppercase">Guaranteed</p>
          <ul className="mt-2 space-y-1.5 text-slate-300">
            <li>
              Same source and index snapshots, config, profile, budget, seed, query and decision inputs: ties resolve the
              same way and the pack hash reproduces.
            </li>
            <li>The composite is built from tagged component digests, so the component two packs differ in can be found.</li>
          </ul>
        </div>
        <div className="rounded-xl border border-amber-300/20 bg-amber-300/[0.04] px-4 py-3">
          <p className="font-mono text-[10.5px] tracking-[0.16em] text-amber-200/90 uppercase">Not yet</p>
          <ul className="mt-2 space-y-1.5 text-slate-400">
            <li>Selection itself still compares raw f32 scores.</li>
            <li>v7 does not yet establish cross-process or cross-host determinism gates (ADR 0087).</li>
          </ul>
        </div>
      </div>
    </DemoFrame>
  );
}
