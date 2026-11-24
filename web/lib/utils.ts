import { type ClassValue, clsx } from "clsx";
import { twMerge } from "tailwind-merge";

export function cn(...inputs: ClassValue[]) {
  return twMerge(clsx(inputs));
}

const GROUPED = new Intl.NumberFormat("en-US");

/** `12345` as `12,345`, the same on the server and in every browser. */
export function grouped(value: number): string {
  return GROUPED.format(value);
}

/** "3.000" → "3", "0.639355" stays: a number with at most `digits` decimals and no trailing zeros. */
export function fixed(value: number, digits = 3): string {
  if (!Number.isFinite(value)) return "—";
  const rounded = Number(value.toFixed(digits));
  return (Object.is(rounded, -0) ? 0 : rounded).toLocaleString("en-US", { maximumFractionDigits: digits });
}

/** A small deterministic PRNG (mulberry32), so a toy draws the same picture on the server and in every browser. */
export function seeded(seed: number): () => number {
  let state = seed >>> 0;
  return () => {
    state = (state + 0x6d2b79f5) >>> 0;
    let t = state;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

export const clamp = (value: number, low: number, high: number) => Math.min(high, Math.max(low, value));
