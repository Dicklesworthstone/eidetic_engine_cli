"use client";

import { ChevronLeft, ChevronRight, Fingerprint, History, Terminal } from "lucide-react";
import { type ReactNode, useEffect, useRef, useState } from "react";

import { BasisBadge, DemoButton, DemoFrame, Segmented } from "@/components/explain/demo";
import { useInView, useReducedMotion } from "@/lib/hooks";
import { cn } from "@/lib/utils";

// ─── The masking rule (src/core/error_recall.rs, canonical_message_template) ──
// Backtick spans become <id>; then, per whitespace token: anything holding a
// path separator is <path>, a 0x-prefixed hex run is <hex>, a number-like token
// (digits plus `: . , - _`) is <num>, and everything else is lower-cased.
// Errors that differ only by identifier, path, line or number collapse to one
// class.

type Masked = { raw: string; out: string; rule: "id" | "path" | "hex" | "num" | "lower" };

/** mask_token: path, then 0x hex, then number-like, else lower-cased (ASCII only, as to_ascii_lowercase). */
function maskToken(raw: string): Masked {
  if (raw.includes("/") || raw.includes("\\")) return { raw, out: "<path>", rule: "path" };
  const lower = raw.replace(/[A-Z]/g, (letter) => letter.toLowerCase());
  if (lower.startsWith("0x") && lower.length > 2 && /^[0-9a-f]+$/.test(lower.slice(2))) return { raw, out: "<hex>", rule: "hex" };
  if (/\d/.test(raw) && /^[\d:.,\-_]+$/.test(raw)) return { raw, out: "<num>", rule: "num" };
  return { raw, out: lower, rule: "lower" };
}

/**
 * canonical_message_template: backtick spans become <id> BEFORE the split, so a multi-word identifier is one
 * <id>; each token is then masked. Each token's original text is kept beside its mask so the page can show both.
 */
export function template(message: string): Masked[] {
  // Each span is marked with a private-use character while the tokens are split, so a literal "<id>" typed into
  // the message is not mistaken for one. The mask itself sees "<id>", as mask_backtick_spans writes it.
  const SPAN = String.fromCharCode(0xe000);
  const spans: string[] = [];
  let masked = "";
  let inSpan = false;
  let current = "";
  for (const ch of message) {
    if (ch === "`") {
      if (inSpan) spans.push(current);
      else {
        masked += SPAN;
        current = "";
      }
      inSpan = !inSpan;
    } else if (inSpan) current += ch;
    else masked += ch;
  }
  if (inSpan) spans.push(current);
  let next = 0;
  return masked
    .split(/\s+/)
    .filter(Boolean)
    .map((token) => {
      const result = maskToken(token.replaceAll(SPAN, "<id>"));
      if (!token.includes(SPAN)) return result;
      const raw = token.replaceAll(SPAN, () => `\`${spans[next++] ?? ""}\``);
      return { raw, out: result.out, rule: result.rule === "lower" ? "id" : result.rule };
    });
}

/** The template as ee stores it: the masked tokens, joined by single spaces. */
export const maskedTemplate = (message: string) =>
  template(message)
    .map((token) => token.out)
    .join(" ");

// ─── The examples ───────────────────────────────────────────────────────────

type Role = "tool_call" | "tool_result" | "assistant";
type Line = { role: Role; text: string; part?: "failure" | "narration" | "fix" | "proof" };

type Example = {
  label: string;
  /** The failing command family, reduced to program + subcommand. */
  family: string;
  transcript: Line[];
  /** The diagnostic line the extractor keys on (before the tool's own prefix). */
  diagnostic: string;
  /** The extractor's input to the masking rule. */
  templateInput: string;
  /** How the extractor builds that input from the output. */
  compose: string;
  code: string | null;
  tool: "rustc" | "shell";
  /**
   * The card's Symptom facet (cass_error_recall.rs · symptom_line). A cargo error keeps its line and `-->` location
   * (cargo_text::symptom); a parsed go test, Vitest or Jest report shows its masked template, so this is null.
   */
  symptom: string | null;
  fix: string;
  recall: string;
};

const SESSION = "ses_01J…7Q";

export const EXAMPLES: Record<"rustc" | "go" | "vitest" | "jest", Example> = {
  rustc: {
    label: "rustc",
    family: "cargo test",
    transcript: [
      { role: "tool_call", text: "cargo test --lib" },
      {
        role: "tool_result",
        part: "failure",
        text: "error[E0277]: the trait bound `Widget: Serialize` is not satisfied\n  --> src/widget.rs:41:9",
      },
      { role: "assistant", part: "narration", text: "Let me look at the widget module." },
      {
        role: "assistant",
        part: "fix",
        text: "Widget needs to derive Serialize: add #[derive(Serialize)] to the struct.",
      },
      { role: "tool_call", text: "cargo test --lib" },
      { role: "tool_result", part: "proof", text: "test result: ok. 3 passed; 0 failed" },
    ],
    diagnostic: "error[E0277]: the trait bound `Widget: Serialize` is not satisfied",
    templateInput: "the trait bound `Widget: Serialize` is not satisfied",
    compose: "The E-code is kept whole as the class; the message is masked beside it.",
    code: "E0277",
    tool: "rustc",
    symptom: "error[E0277]: the trait bound `Widget: Serialize` is not satisfied (src/widget.rs:41:9)",
    fix: "Widget needs to derive Serialize: add #[derive(Serialize)] to the struct.",
    // Single quotes: inside double quotes a shell would run `Order: Serialize` as a command.
    recall: "ee diagnose-error --tool rustc 'error[E0277]: the trait bound `Order: Serialize` is not satisfied' --json",
  },
  go: {
    label: "go test",
    family: "go test",
    transcript: [
      { role: "tool_call", text: "go test -v ./billing/..." },
      {
        role: "tool_result",
        part: "failure",
        text: "=== RUN   TestInvoiceRounding\n    invoice_test.go:42: got 41.99, want 42.00\n--- FAIL: TestInvoiceRounding (0.00s)\nFAIL\nFAIL\texample.com/shop/billing\t0.012s\nFAIL",
      },
      { role: "assistant", part: "narration", text: "Checking how totals are formatted." },
      {
        role: "assistant",
        part: "fix",
        text: "Round half-even in Money.Total before formatting, not after summing floats.",
      },
      { role: "tool_call", text: "go test -v ./billing/..." },
      {
        role: "tool_result",
        part: "proof",
        text: "=== RUN   TestInvoiceRounding\n--- PASS: TestInvoiceRounding (0.00s)\nPASS\nok  \texample.com/shop/billing\t0.011s",
      },
    ],
    diagnostic: "--- FAIL: TestInvoiceRounding (0.00s) · FAIL example.com/shop/billing",
    templateInput: "go test failure: example.com/shop/billing test TestInvoiceRounding",
    compose:
      "No error code, so ee writes a sentence from the package and the test: go test failure: <package> test <name>. A package path holds a slash, so it masks to <path>.",
    code: null,
    tool: "shell",
    symptom: null,
    fix: "Round half-even in Money.Total before formatting, not after summing floats.",
    recall: "ee diagnose-error --tool shell --error-log go-test.log --json",
  },
  vitest: {
    label: "vitest",
    family: "vitest",
    transcript: [
      { role: "tool_call", text: "vitest run --reporter=verbose" },
      {
        role: "tool_result",
        part: "failure",
        text:
          " × src/cart.test.ts > cart > applies the member discount 3ms\n   → expected 41 to be 42 // Object.is equality\n\n FAIL  src/cart.test.ts > cart > applies the member discount\nAssertionError: expected 41 to be 42 // Object.is equality",
      },
      { role: "assistant", part: "narration", text: "Let me read the cart module first." },
      {
        role: "assistant",
        part: "fix",
        text: "The discount ran before tax; apply memberDiscount after computeTax in checkout.",
      },
      { role: "tool_call", text: "vitest run --reporter=verbose" },
      { role: "tool_result", part: "proof", text: " ✓ src/cart.test.ts > cart > applies the member discount 2ms\n      Tests  12 passed (12)" },
    ],
    diagnostic: "AssertionError: expected 41 to be 42 // Object.is equality",
    templateInput: "javascript-test: AssertionError: expected 41 to be 42 // Object.is equality",
    compose:
      "No error code, so the template is the first line under the test's FAIL header, prefixed javascript-test:. The diff, code frame and stack are left out.",
    code: null,
    tool: "shell",
    symptom: null,
    fix: "The discount ran before tax; apply memberDiscount after computeTax in checkout.",
    recall: "ee diagnose-error --tool shell --error-log vitest.log --json",
  },
  jest: {
    label: "jest",
    family: "jest",
    transcript: [
      { role: "tool_call", text: "jest --verbose" },
      {
        role: "tool_result",
        part: "failure",
        text: "  ● orders › totals an empty basket\n\n    TypeError: Cannot read properties of undefined (reading 'total')",
      },
      { role: "assistant", part: "narration", text: "Looking at the orders fixture." },
      {
        role: "assistant",
        part: "fix",
        text: "An empty basket has no summary; default summary to { total: 0 } in buildOrder.",
      },
      { role: "tool_call", text: "jest --verbose" },
      { role: "tool_result", part: "proof", text: "  ✓ totals an empty basket (3 ms)\nTests:       9 passed, 9 total" },
    ],
    diagnostic: "TypeError: Cannot read properties of undefined (reading 'total')",
    templateInput: "javascript-test: TypeError: Cannot read properties of undefined (reading 'total')",
    compose:
      "No error code, so the template is the first line under the test's ● header, prefixed javascript-test:. The code frame and stack are left out.",
    code: null,
    tool: "shell",
    symptom: null,
    fix: "An empty basket has no summary; default summary to { total: 0 } in buildOrder.",
    recall: "ee diagnose-error --tool shell --error-log jest.log --json",
  },
};

type ToolId = keyof typeof EXAMPLES;

const STEPS = [
  { id: "session", label: "The session", short: "Session", icon: Terminal },
  { id: "fingerprint", label: "The fingerprint", short: "Key", icon: Fingerprint },
  { id: "card", label: "Card & recall", short: "Recall", icon: History },
] as const;

const STEP_MS = 6500;

const RULE_TONE: Record<Masked["rule"], string> = {
  id: "border-violet-400/35 bg-violet-400/10 text-violet-200",
  path: "border-amber-400/35 bg-amber-400/10 text-amber-200",
  hex: "border-fuchsia-400/35 bg-fuchsia-400/10 text-fuchsia-200",
  num: "border-cyan-400/35 bg-cyan-400/10 text-cyan-200",
  lower: "border-white/10 bg-white/[0.03] text-slate-300",
};

export function IncidentCard() {
  const [toolId, setToolId] = useState<ToolId>("rustc");
  const [step, setStep] = useState(0);
  const [auto, setAuto] = useState(true);
  const reduced = useReducedMotion();
  const frame = useRef<HTMLDivElement>(null);
  const inView = useInView(frame);
  const example = EXAMPLES[toolId];

  // Walks through the three steps while on screen, until the reader takes the controls.
  useEffect(() => {
    if (!auto || reduced || !inView) return;
    const timer = window.setTimeout(() => setStep((current) => (current + 1) % STEPS.length), STEP_MS);
    return () => window.clearTimeout(timer);
  }, [auto, reduced, inView, step]);

  const go = (next: number) => {
    setAuto(false);
    setStep(Math.max(0, Math.min(STEPS.length - 1, next)));
  };

  return (
    <DemoFrame
      eyebrow="CASS import · failure-to-fix recall"
      title="Fixes recalled by error fingerprint."
      basis="diagram"
      source={
        <>
          docs/adr/0091-derived-incident-cards.md · extractive facets, ≤ 120 tokens (INCIDENT_CARD_TOKEN_BUDGET) ·
          src/core/incident_card.rs · header and facet format · src/core/error_recall.rs · canonical_message_template,
          layered_key · src/core/cass_error_recall_go.rs, cass_error_recall_js_tests.rs · go test and Vitest/Jest
          diagnostics · the transcripts are shortened, illustrative excerpts; ee parses a test report only when it is
          complete and its counts agree
        </>
      }
    >
      {/* Focus inside the figure (the tool switch, the rail, the masking input) takes the controls: the walk-through
          would otherwise unmount the input under a reader who is typing in it. */}
      <div ref={frame} className="space-y-6" onFocus={() => setAuto(false)}>
        <div className="flex flex-wrap items-center justify-between gap-3">
          <Segmented
            label="Test or compiler output"
            value={toolId}
            onChange={(value) => setToolId(value)}
            options={(Object.keys(EXAMPLES) as ToolId[]).map((id) => ({
              value: id,
              label: <span className="font-mono">{EXAMPLES[id].label}</span>,
              tone: "rule" as const,
            }))}
          />
          <div className="flex items-center gap-2">
            <DemoButton onClick={() => go(step - 1)} disabled={step === 0} label="Previous step">
              <ChevronLeft className="h-3.5 w-3.5" aria-hidden="true" /> Back
            </DemoButton>
            <DemoButton onClick={() => go(step + 1)} disabled={step === STEPS.length - 1} label="Next step">
              Next <ChevronRight className="h-3.5 w-3.5" aria-hidden="true" />
            </DemoButton>
          </div>
        </div>

        {/* The rail: three steps, the current one lit. */}
        <ol className="grid grid-cols-3 gap-2">
          {STEPS.map((entry, index) => {
            const Icon = entry.icon;
            const on = index === step;
            const done = index < step;
            return (
              <li key={entry.id}>
                <button
                  type="button"
                  onClick={() => go(index)}
                  aria-current={on ? "step" : undefined}
                  className={cn(
                    "relative flex w-full items-center gap-2 overflow-hidden rounded-xl border px-2.5 py-2 text-left transition-colors focus-visible:ring-2 focus-visible:ring-violet-400 focus-visible:outline-none sm:px-3",
                    on
                      ? "border-[color:var(--chapter)]/50 bg-[color:var(--chapter)]/[0.08] text-white"
                      : "border-white/[0.07] bg-black/20 text-slate-400 hover:text-white",
                  )}
                >
                  <span
                    className={cn(
                      "flex h-6 w-6 shrink-0 items-center justify-center rounded-full border font-mono text-[11px]",
                      on || done ? "border-[color:var(--chapter)]/60 text-[color:var(--chapter)]" : "border-white/15",
                    )}
                  >
                    {index + 1}
                  </span>
                  <Icon className="hidden h-3.5 w-3.5 shrink-0 sm:block" aria-hidden="true" />
                  <span className="min-w-0 truncate text-[12px] font-bold sm:text-[13px]">
                    <span className="sm:hidden">{entry.short}</span>
                    <span className="hidden sm:inline">{entry.label}</span>
                  </span>
                  {on && auto && !reduced ? (
                    <span
                      key={`${step}-${toolId}`}
                      aria-hidden="true"
                      className="absolute inset-x-0 bottom-0 h-[2px] origin-left bg-[color:var(--chapter)]/70"
                      style={{ animation: `incident-progress ${STEP_MS}ms linear forwards` }}
                    />
                  ) : null}
                </button>
              </li>
            );
          })}
        </ol>
        <style>{`@keyframes incident-progress{from{transform:scaleX(0)}to{transform:scaleX(1)}}`}</style>

        {/* The panel is not a live region: it would read a whole panel every 6.5 s, and the token list on every
            keystroke. Once the reader takes the controls, the step and the tool are announced instead. */}
        <p className="sr-only" aria-live="polite">
          {auto ? "" : `Step ${step + 1} of ${STEPS.length}: ${STEPS[step]?.label ?? ""}, ${example.label}`}
        </p>
        <div className="min-h-[22rem]">
          {step === 0 ? <SessionStep example={example} /> : null}
          {step === 1 ? <FingerprintStep example={example} /> : null}
          {step === 2 ? <CardStep example={example} /> : null}
        </div>
      </div>
    </DemoFrame>
  );
}

const ROLE_LABEL: Record<Role, string> = { tool_call: "tool call", tool_result: "tool result", assistant: "assistant" };

const PART_STYLE: Record<NonNullable<Line["part"]>, { row: string; note: string; text: string }> = {
  failure: { row: "border-rose-400/30 bg-rose-400/[0.05]", note: "the failure", text: "text-rose-200" },
  narration: { row: "border-white/[0.06] bg-transparent opacity-60", note: "narration: dropped", text: "text-slate-400 line-through decoration-slate-600" },
  fix: { row: "border-[color:var(--chapter)]/40 bg-[color:var(--chapter)]/[0.06]", note: "the fix", text: "text-white" },
  proof: { row: "border-emerald-400/30 bg-emerald-400/[0.05]", note: "the proof", text: "text-emerald-200" },
};

function SessionStep({ example }: { example: Example }) {
  return (
    <div className="type-in grid grid-cols-1 gap-5 lg:grid-cols-12">
      <div className="min-w-0 lg:col-span-8">
        <div className="mb-2 flex flex-wrap items-center justify-between gap-2">
          <p className="font-mono text-[11px] text-slate-500">cass-session://{SESSION} · an agent&apos;s session, as cass recorded it</p>
          <BasisBadge basis="toy" />
        </div>
        <ol className="space-y-1.5">
          {example.transcript.map((line, index) => {
            const style = line.part ? PART_STYLE[line.part] : null;
            return (
              <li
                key={index}
                className={cn(
                  "grid grid-cols-[2.2rem_minmax(0,1fr)] gap-2 rounded-xl border px-2.5 py-2 sm:grid-cols-[2.4rem_6.2rem_minmax(0,1fr)]",
                  style?.row ?? "border-white/[0.06] bg-black/20",
                )}
              >
                <span className="font-mono text-[11px] text-slate-500 tabular-nums">L{index + 1}</span>
                <span className="hidden font-mono text-[10px] tracking-wide text-slate-500 uppercase sm:block sm:pt-px">
                  {ROLE_LABEL[line.role]}
                </span>
                <div className="min-w-0">
                  <pre className={cn("font-mono text-[12px] leading-relaxed whitespace-pre-wrap [overflow-wrap:anywhere]", style?.text ?? "text-slate-200")}>
                    {line.role === "tool_call" ? "$ " : ""}
                    {line.text}
                  </pre>
                  {style ? <p className="mt-1 text-[10px] font-bold tracking-[0.14em] text-slate-500 uppercase">{style.note}</p> : null}
                </div>
              </li>
            );
          })}
        </ol>
      </div>
      <div className="space-y-3 text-sm leading-relaxed text-slate-300 lg:col-span-4">
        <p>
          <span className="font-semibold text-white">ee import cass</span> reads the sessions your agents already
          recorded. It pairs each failing tool result with the assistant turns that explained the fix and a later
          successful run of the identical command that proved it.
        </p>
        <p className="text-slate-400">
          Only admitted assistant sentences can become the fix. Narration is dropped, and a bare exit code is never an
          error class on its own.
        </p>
      </div>
    </div>
  );
}

function FingerprintStep({ example }: { example: Example }) {
  const tokens = template(example.templateInput);
  const masked = maskedTemplate(example.templateInput);
  const key = example.code ? `${example.tool}:${example.code}` : `${example.tool}:tmpl:blake3(template)`;
  const [own, setOwn] = useState("error: failed to open /tmp/build-7f3a/cache.db at 0x7ffd5e8c in `CacheStore::open` after 3 attempts");
  const ownTokens = template(own);

  return (
    <div className="type-in grid grid-cols-1 gap-6 lg:grid-cols-12">
      <div className="min-w-0 space-y-4 lg:col-span-7">
        <div>
          <p className="text-[9px] font-black tracking-[0.2em] text-slate-500 uppercase">From the output</p>
          <p className="mt-1.5 rounded-xl border border-rose-400/25 bg-rose-400/[0.05] px-3 py-2 font-mono text-[12px] leading-relaxed [overflow-wrap:anywhere] text-rose-200">
            {example.diagnostic}
          </p>
          <p className="mt-1.5 text-[12px] leading-snug text-slate-400">{example.compose}</p>
        </div>
        <div>
          <div className="flex flex-wrap items-center justify-between gap-2">
            <p className="text-[9px] font-black tracking-[0.2em] text-slate-500 uppercase">Masked, token by token</p>
            <BasisBadge basis="rule" />
          </div>
          <TokenRow tokens={tokens} />
          <p className="mt-2 rounded-lg bg-black/30 px-3 py-2 font-mono text-[12px] [overflow-wrap:anywhere] text-white">{masked}</p>
        </div>
        <div className="rounded-xl border border-white/10 bg-black/30 p-3">
          <label className="block">
            <span className="text-[9px] font-black tracking-[0.2em] text-slate-500 uppercase">Type an error line to mask it</span>
            <input
              value={own}
              onChange={(event) => setOwn(event.target.value.slice(0, 220))}
              spellCheck={false}
              className="mt-1.5 w-full rounded-lg border border-white/10 bg-black/40 px-3 py-2 font-mono text-[12px] text-slate-100 focus-visible:border-[color:var(--chapter)]/60 focus-visible:outline-none"
            />
          </label>
          <TokenRow tokens={ownTokens} />
        </div>
      </div>

      <div className="min-w-0 space-y-3 lg:col-span-5">
        <p className="text-[9px] font-black tracking-[0.2em] text-slate-500 uppercase">The layered key</p>
        <ol className="space-y-2">
          <KeyLayer
            n={1}
            name="canonical_code"
            active={Boolean(example.code)}
            detail="Exact (tool, code): a rustc E-code, an ee.error.v2 code, an RCH blocker kind:stage."
            value={example.code ? key : "no structured code here"}
          />
          <KeyLayer
            n={2}
            name="message_template"
            active={!example.code}
            detail="The masked template, hashed with BLAKE3, when no code exists."
            value={example.code ? "not needed" : key}
          />
          <KeyLayer
            n={3}
            name="simhash_tail"
            active={false}
            detail="The long-tail fallback: templates within 6 of 128 simhash bits count as one class."
            value="near-duplicates only"
          />
        </ol>
        <p className="text-[13px] leading-relaxed text-slate-400">
          <span className="text-slate-200">Rule legend:</span> <Chip rule="id">`span` → &lt;id&gt;</Chip>{" "}
          <Chip rule="path">a/b → &lt;path&gt;</Chip> <Chip rule="hex">0x1f → &lt;hex&gt;</Chip>{" "}
          <Chip rule="num">41:9 → &lt;num&gt;</Chip> <Chip rule="lower">Word → word</Chip>
        </p>
      </div>
    </div>
  );
}

function Chip({ rule, children }: { rule: Masked["rule"]; children: ReactNode }) {
  return <span className={cn("inline-block rounded-md border px-1.5 font-mono text-[11px]", RULE_TONE[rule])}>{children}</span>;
}

function TokenRow({ tokens }: { tokens: Masked[] }) {
  return (
    <ul className="mt-2 flex flex-wrap gap-1.5" aria-label="Tokens after masking">
      {tokens.map((token, index) => (
        <li key={`${index}-${token.raw}`} className={cn("rounded-lg border px-2 py-1 font-mono text-[11px] leading-tight", RULE_TONE[token.rule])}>
          {token.rule === "lower" && token.raw === token.out ? (
            token.out
          ) : (
            <>
              <span className="text-slate-500 line-through decoration-slate-600">{token.raw}</span>
              <span aria-hidden="true" className="px-1 text-slate-500">
                →
              </span>
              {token.out}
            </>
          )}
        </li>
      ))}
    </ul>
  );
}

function KeyLayer({ n, name, active, detail, value }: { n: number; name: string; active: boolean; detail: string; value: string }) {
  return (
    <li
      className={cn(
        "rounded-xl border px-3 py-2.5",
        active ? "border-[color:var(--chapter)]/45 bg-[color:var(--chapter)]/[0.07]" : "border-white/[0.06] bg-black/20",
      )}
    >
      <p className="flex items-center gap-2">
        <span className={cn("font-mono text-[10px]", active ? "text-[color:var(--chapter)]" : "text-slate-500")}>{n}</span>
        <span className={cn("font-mono text-[12px]", active ? "text-white" : "text-slate-400")}>{name}</span>
        {active ? <span className="ml-auto text-[9px] font-black tracking-[0.18em] text-[color:var(--chapter)] uppercase">matches here</span> : null}
      </p>
      <p className="mt-1 text-[12px] leading-snug text-slate-400">{detail}</p>
      <p className={cn("mt-1.5 font-mono text-[11px] [overflow-wrap:anywhere]", active ? "text-white" : "text-slate-500")}>{value}</p>
    </li>
  );
}

function CardStep({ example }: { example: Example }) {
  const end = example.transcript.length;
  const header = `Incident card (derived by ee from lines 1-${end}): \`${example.family}\` failed, then passed after a fix.`;
  const symptom = example.symptom ?? maskedTemplate(example.templateInput);
  const lines: { facet: string; text: string }[] = [
    { facet: "header", text: header },
    { facet: "Symptom", text: `Symptom: ${symptom}` },
    { facet: "Fix", text: `Fix: ${example.fix}` },
    { facet: "Verified", text: `Verified: \`${example.family}\` succeeded afterwards.` },
  ];
  const text = lines.map((line) => line.text).join("\n");
  // A rough estimate for the page only: ee counts the bound with cl100k_base.
  const approxTokens = Math.ceil(text.length / 3.5);
  const key = example.code ? `${example.tool}:${example.code}` : `${example.tool}:tmpl:blake3:…`;
  const provenance = `cass-session://${SESSION}#L1-${end}`;

  return (
    <div className="type-in grid grid-cols-1 gap-6 lg:grid-cols-12">
      <div className="min-w-0 lg:col-span-6">
        <div className="flex items-center justify-between gap-3">
          <p className="text-[9px] font-black tracking-[0.2em] text-slate-500 uppercase">The incident card</p>
          <p className="font-mono text-[10px] text-slate-500 tabular-nums">≈{approxTokens} of ≤ 120 tokens</p>
        </div>
        <div className="panel-strong relative mt-2 rounded-2xl border bg-black/40 p-4">
          <ul className="space-y-2.5">
            {lines.map((line) => (
              <li key={line.facet} className="grid grid-cols-[4.6rem_minmax(0,1fr)] gap-3">
                <span className="pt-0.5 text-right font-mono text-[9px] tracking-[0.14em] text-[color:var(--chapter)] uppercase">
                  {line.facet}
                </span>
                <span className="font-mono text-[12px] leading-relaxed [overflow-wrap:anywhere] text-slate-100">{line.text}</span>
              </li>
            ))}
          </ul>
        </div>
        <p className="mt-3 text-[13px] leading-relaxed text-slate-400">
          The card is copied from the session rather than generated: fixed facet labels, the failing line (for a parsed
          test report, its masked template), the admitted fix sentences and the verifying command. It is stored as
          evidence with <span className="font-mono text-[12px] text-slate-300">cass_evidence</span> trust, linked under the
          error&apos;s fingerprint, and becomes a durable memory only through review and curation.
        </p>
      </div>

      <div className="min-w-0 lg:col-span-6">
        <p className="text-[9px] font-black tracking-[0.2em] text-slate-500 uppercase">Weeks later, a different session</p>
        <pre className="mt-2 overflow-x-auto rounded-xl border border-white/10 bg-black/40 px-3 py-2.5 font-mono text-[11.5px] leading-relaxed whitespace-pre-wrap [overflow-wrap:anywhere] text-violet-200">
          $ {example.recall}
        </pre>
        <pre className="mt-2 overflow-x-auto rounded-xl border border-white/[0.07] bg-black/25 px-3 py-2.5 font-mono text-[11px] leading-relaxed text-slate-300 [scrollbar-width:thin]">
          {`"data": {
  "schema": "ee.diagnose_error.v1",
  "fingerprintKey": "${key}",
  "layer": "${example.code ? "canonical_code" : "message_template"}",
  "isKnown": true,
  "repairEvidence": [
    { "role": "incident_card",
      "provenanceUri": "${provenance}",
      "text": "Incident card (derived by ee …" },
    { "role": "proof",
      "provenanceUri": "cass-session://${SESSION}#L${end}",
      "text": null }
  ]
}`}
        </pre>
        <p className="mt-3 text-[13px] leading-relaxed text-slate-400">
          A later error of the same class, from a different file and line, recalls the card first, along with the
          session lines that prove the fix. <span className="font-mono text-[12px] text-slate-300">ee pack --error-log</span> folds the
          same recall into a context pack. Proof spans are tool output, so only their locator is shown.
        </p>
      </div>
    </div>
  );
}
