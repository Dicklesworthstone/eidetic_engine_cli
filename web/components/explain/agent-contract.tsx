"use client";

import { ArrowRight, Bot, Check, ChevronLeft, ChevronRight, Pause, Play, Terminal } from "lucide-react";
import { Fragment, type ReactNode, useEffect, useMemo, useRef, useState } from "react";

import {
  at,
  ELIDED,
  EXIT_CODES,
  firstAction,
  type Json,
  RULE_ROWS,
  type Situation,
  type SituationId,
  SITUATIONS,
  sortedRecovery,
  VERDICT,
} from "@/components/explain/agent-contract-data";
import { BasisBadge, DemoButton, DemoFrame, Readout, Segmented } from "@/components/explain/demo";
import { useInView, useReducedMotion } from "@/lib/hooks";
import { cn } from "@/lib/utils";

/**
 * An agent reading ee's machine contract. Pick a situation: on the left, what a typical CLI
 * prints and what ee prints (the record, verbatim from the file named under it); on the right,
 * the seven reads an agent makes of that JSON, each lighting the fields it reads, ending in a
 * verdict. The safety step applies ee's own rule (repair_command_preflight_policy in
 * src/core/preflight_guard.rs), ported line for line in agent-contract-data.ts.
 */

const STEPS = [
  { id: "envelope", label: "Check the envelope", field: "schema" },
  { id: "exit", label: "Read the exit code", field: "exit" },
  { id: "broke", label: "What broke, how badly", field: "code · severity" },
  { id: "plan", label: "Sort the recovery plan", field: "recovery[]" },
  { id: "safety", label: "Is it safe to run?", field: "riskClass" },
  { id: "act", label: "Act", field: "command" },
  { id: "recheck", label: "Re-check", field: "exit · degraded[]" },
] as const;

const LAST = STEPS.length - 1;
const STEP_MS = 2200;

/** Fields the compact view folds into a "… n more" line; the full record shows them. */
const FOLDED = new Set([
  "mutatesExternalState",
  "mutatesTrackerState",
  "privacyClass",
  "evidence",
  "preconditions",
  "resultsIn",
  "modelId",
  "featureFlag",
  "addressedStorePath",
  "addressedWorkspacePath",
]);

/** Rows of the rule that stop at a person. */
const HUMAN_ENDS = new Set(["ask a human", "manual only"]);

const SEVERITY_TONE: Record<string, string> = {
  info: "border-sky-400/35 bg-sky-400/10 text-sky-200",
  low: "border-sky-400/35 bg-sky-400/10 text-sky-200",
  warning: "border-amber-400/35 bg-amber-400/10 text-amber-200",
  medium: "border-orange-400/40 bg-orange-400/10 text-orange-200",
  high: "border-rose-400/40 bg-rose-400/10 text-rose-200",
  critical: "border-rose-400/60 bg-rose-400/20 text-rose-100",
};

export function RecoveryExplorer() {
  const [id, setId] = useState<SituationId>("index");
  // The server's HTML, and a reader who asks for less motion, get the whole reading at once.
  const [step, setStep] = useState(LAST);
  const [playing, setPlaying] = useState(false);
  const [view, setView] = useState<"compact" | "full">("compact");
  const reduced = useReducedMotion();
  const frame = useRef<HTMLDivElement>(null);
  const inView = useInView(frame);
  const started = useRef(false);
  const situation = SITUATIONS.find((entry) => entry.id === id) ?? (SITUATIONS[0] as Situation);

  // The first time the figure comes on screen, the agent starts reading from the top.
  useEffect(() => {
    if (!inView || reduced || started.current) return;
    started.current = true;
    setStep(0);
    setPlaying(true);
  }, [inView, reduced]);

  useEffect(() => {
    if (!reduced) return;
    setPlaying(false);
    setStep(LAST);
  }, [reduced]);

  // One read per tick, only while on screen; it stops at the verdict.
  useEffect(() => {
    if (!playing || reduced || !inView) return;
    if (step >= LAST) {
      setPlaying(false);
      return;
    }
    const timer = window.setTimeout(() => setStep((current) => Math.min(LAST, current + 1)), STEP_MS);
    return () => window.clearTimeout(timer);
  }, [playing, reduced, inView, step]);

  const choose = (next: SituationId) => {
    setId(next);
    started.current = true;
    setStep(reduced ? LAST : 0);
    setPlaying(!reduced);
  };

  const go = (next: number) => {
    setPlaying(false);
    setStep(Math.max(0, Math.min(LAST, next)));
  };

  const toggle = () => {
    if (reduced) return;
    if (playing) {
      setPlaying(false);
      return;
    }
    if (step >= LAST) setStep(0);
    setPlaying(true);
  };

  const animating = playing && !reduced && inView;

  return (
    <DemoFrame
      eyebrow="For agents · the error contract"
      title="Errors an agent can act on."
      basis="record"
      source={
        <>
          src/models/mod.rs · ProcessExitCode (0–10, 130), ERROR_SCHEMA_V2, RESPONSE_SCHEMA_V2, RecoveryKind,
          RepairActionRiskClass, repair_action_safety · src/output/mod.rs · error_response_json,
          build_recovery_action_fields · src/core/preflight_guard.rs · repair_command_preflight_policy ·
          docs/schemas/ee.error.v2.json · this record:{" "}
          {situation.source.kind === "pinned"
            ? `${situation.source.file}, pinned by ${situation.source.pinnedBy ?? "its test"}`
            : `built from ${(situation.source.files ?? []).join(", ")}${situation.source.pinnedBy ? `, checked by ${situation.source.pinnedBy}` : ""}`}{" "}
          · “…” marks a cut and &lt;…&gt; a runtime value · the typical CLI is invented for the contrast
        </>
      }
    >
      <div ref={frame} className="space-y-5">
        <Segmented
          label="Situation"
          value={id}
          onChange={choose}
          options={SITUATIONS.map((entry) => ({
            value: entry.id,
            label: (
              <span className="inline-flex items-baseline gap-1.5">
                <span className="font-mono text-[10px] tabular-nums opacity-60">{entry.exit}</span>
                <span className="sm:hidden">{entry.short}</span>
                <span className="hidden sm:inline">{entry.label}</span>
              </span>
            ),
          }))}
        />

        <div className="grid grid-cols-1 gap-5 lg:grid-cols-[minmax(0,1.08fr)_minmax(0,0.92fr)] lg:gap-6">
          {/* What the agent is handed */}
          <div className="min-w-0 space-y-3">
            <TypicalCli situation={situation} />
            <RecordPane situation={situation} step={step} view={view} onView={setView} inView={inView} reduced={reduced} />
          </div>

          {/* What the agent makes of it */}
          <div className="min-w-0 space-y-3">
            <div className="flex flex-wrap items-center justify-between gap-2">
              <p className="flex items-center gap-2 text-[10px] font-black tracking-[0.2em] text-slate-500 uppercase">
                <Bot className="h-3.5 w-3.5 text-[color:var(--chapter)]" aria-hidden="true" /> An agent&rsquo;s reading
              </p>
              <div className="flex items-center gap-1.5">
                <DemoButton onClick={() => go(step - 1)} disabled={step === 0} label="Previous read" className="px-2.5">
                  <ChevronLeft className="h-3.5 w-3.5" aria-hidden="true" />
                </DemoButton>
                <DemoButton
                  onClick={toggle}
                  disabled={reduced}
                  pressed={playing}
                  label={playing ? "Pause the reading" : "Play the reading"}
                  className="px-2.5"
                >
                  {playing ? <Pause className="h-3.5 w-3.5" aria-hidden="true" /> : <Play className="h-3.5 w-3.5" aria-hidden="true" />}
                </DemoButton>
                <DemoButton onClick={() => go(step + 1)} disabled={step === LAST} label="Next read" className="px-2.5">
                  <ChevronRight className="h-3.5 w-3.5" aria-hidden="true" />
                </DemoButton>
              </div>
            </div>

            <ol className="space-y-1">
              {STEPS.map((entry, index) => {
                const current = index === step;
                const done = index < step;
                const read = current || done;
                return (
                  <li key={entry.id}>
                    <button
                      type="button"
                      onClick={() => go(index)}
                      aria-current={current ? "step" : undefined}
                      className={cn(
                        "grid w-full grid-cols-[1.375rem_minmax(0,1fr)] items-start gap-2.5 rounded-lg border px-2.5 py-1.5 text-left transition-colors focus-visible:ring-2 focus-visible:ring-violet-400 focus-visible:outline-none",
                        current
                          ? "border-[color:var(--chapter)]/45 bg-[color:var(--chapter)]/[0.07]"
                          : done
                            ? "border-white/[0.07] bg-black/20 hover:border-white/15"
                            : "border-white/[0.04] hover:border-white/10",
                      )}
                    >
                      <span
                        className={cn(
                          "mt-px flex h-[1.375rem] w-[1.375rem] items-center justify-center rounded-full border font-mono text-[10.5px]",
                          read ? "border-[color:var(--chapter)]/60 text-[color:var(--chapter)]" : "border-white/15 text-slate-500",
                        )}
                      >
                        {done ? <Check className="h-3 w-3" aria-hidden="true" /> : index + 1}
                      </span>
                      <span className="min-w-0">
                        <span className="flex items-baseline justify-between gap-x-2">
                          <span
                            className={cn(
                              "truncate text-[13px] font-bold",
                              current ? "text-white" : done ? "text-slate-200" : "text-slate-500",
                            )}
                          >
                            {entry.label}
                          </span>
                          <span className="shrink-0 font-mono text-[10px] text-slate-500">{entry.field}</span>
                        </span>
                        {/* Always laid out, shown once read: the list keeps one height while the reading plays. */}
                        <span
                          aria-hidden={read ? undefined : true}
                          className={cn("block truncate font-mono text-[11px] leading-[1.5]", read ? "text-slate-400" : "invisible")}
                        >
                          {summary(situation, index)}
                        </span>
                      </span>
                    </button>
                  </li>
                );
              })}
            </ol>

            {/* Every finding is laid out in one cell and only the current one shows, so the panel is as tall as the
                tallest and the page below never moves while the reading plays. */}
            <div className="relative overflow-hidden rounded-xl border border-[color:var(--chapter)]/35 bg-[color:var(--chapter)]/[0.04] px-3 pt-2.5 pb-3.5 sm:px-3.5">
              <p className="truncate font-mono text-[10px] tracking-[0.14em] text-[color:var(--chapter)] uppercase">
                Read {step + 1} of {STEPS.length} · {STEPS[step]?.label}
              </p>
              <div className="mt-2 grid">
                {STEPS.map((entry, index) => (
                  <div
                    key={`${id}-${entry.id}`}
                    aria-hidden={index === step ? undefined : true}
                    className={cn("min-w-0 [grid-area:1/1]", index === step ? "type-in" : "invisible")}
                  >
                    <Finding situation={situation} index={index} />
                  </div>
                ))}
              </div>
              {animating ? (
                <span
                  key={`${id}-${step}-bar`}
                  aria-hidden="true"
                  className="absolute inset-x-0 bottom-0 h-[2px] origin-left bg-[color:var(--chapter)]/70"
                  style={{ animation: `agent-read ${STEP_MS}ms linear forwards` }}
                />
              ) : null}
            </div>
            <style>{`@keyframes agent-read{from{transform:scaleX(0)}to{transform:scaleX(1)}}`}</style>

            <div aria-live="polite" className="flex min-h-[2.5rem] flex-wrap items-center gap-2.5">
              {step === LAST ? (
                <>
                  <span className="text-[10px] font-black tracking-[0.2em] text-slate-500 uppercase">Verdict</span>
                  <span
                    className={cn(
                      "type-in rounded-full border px-3.5 py-1.5 text-[13px] font-bold",
                      VERDICT[situation.verdict].tone,
                    )}
                  >
                    {VERDICT[situation.verdict].label}
                  </span>
                </>
              ) : (
                <span className="text-[12px] text-slate-500">The verdict comes after the re-check.</span>
              )}
            </div>
          </div>
        </div>

        <RuleTable situation={situation} lit={step >= 4} />
      </div>
    </DemoFrame>
  );
}

// ─── The left column ────────────────────────────────────────────────────────

function TypicalCli({ situation }: { situation: Situation }) {
  const { lines, exit, missing } = situation.typical;
  return (
    <div className="rounded-2xl border border-white/[0.06] bg-black/20 p-3.5">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <p className="flex items-center gap-2 text-[10px] font-black tracking-[0.2em] text-slate-500 uppercase">
          <Terminal className="h-3.5 w-3.5" aria-hidden="true" /> A typical CLI
        </p>
        <BasisBadge basis="toy" />
      </div>
      <div className="mt-2.5 flex items-start justify-between gap-3 font-mono text-[12px] leading-relaxed">
        {lines.length > 0 ? (
          <pre className="min-w-0 whitespace-pre-wrap text-slate-400 line-through decoration-rose-400/60 [overflow-wrap:anywhere]">
            {lines.join("\n")}
          </pre>
        ) : (
          <span className="text-slate-500 italic">(no output)</span>
        )}
        <span className="shrink-0 text-[10px] text-slate-500 tabular-nums">exit {exit}</span>
      </div>
      <p className="mt-2 text-[12px] leading-snug text-slate-400">{missing}</p>
    </div>
  );
}

function RecordPane({
  situation,
  step,
  view,
  onView,
  inView,
  reduced,
}: {
  situation: Situation;
  step: number;
  view: "compact" | "full";
  onView: (view: "compact" | "full") => void;
  inView: boolean;
  reduced: boolean;
}) {
  const lines = useMemo(() => jsonLines(situation.record, view === "compact"), [situation, view]);
  const { keys, group } = useMemo(() => litFields(situation, step), [situation, step]);
  const scroller = useRef<HTMLDivElement>(null);

  // Bring the fields being read into the pane's own view, never scrolling the page.
  useEffect(() => {
    const box = scroller.current;
    if (!box || !inView) return;
    const target = box.querySelector<HTMLElement>('[data-lit="true"]');
    if (!target) return;
    const top = target.offsetTop;
    const bottom = top + target.offsetHeight;
    if (top >= box.scrollTop + 8 && bottom <= box.scrollTop + box.clientHeight - 8) return;
    box.scrollTo({ top: Math.max(0, top - box.clientHeight * 0.3), behavior: reduced ? "auto" : "smooth" });
  }, [keys, view, inView, reduced]);

  return (
    <div className="overflow-hidden rounded-2xl border border-white/[0.08] bg-[#030209]">
      <div className="flex flex-wrap items-center justify-between gap-2 px-4 pt-3">
        <p className="flex items-center gap-2 font-mono text-[10px] tracking-[0.2em] text-slate-500 uppercase">
          ee · stdout
          <span
            className={cn(
              "rounded-md border px-1.5 py-px tracking-normal normal-case tabular-nums transition-colors duration-300",
              step === 1
                ? "border-[color:var(--chapter)]/60 bg-[color:var(--chapter)]/20 text-white"
                : "border-white/10 text-slate-400",
            )}
          >
            exit {situation.exit}
          </span>
        </p>
        <Segmented
          label="How much of the record"
          value={view}
          onChange={onView}
          options={[
            { value: "compact", label: "Compact" },
            { value: "full", label: "Full record" },
          ]}
        />
      </div>
      <p className="px-4 pt-2 font-mono text-[11px] leading-relaxed [overflow-wrap:anywhere] text-violet-200/90">
        $ {situation.invocation ?? "ee … --json"}
      </p>
      <div
        ref={scroller}
        className="relative max-h-[20rem] overflow-y-auto px-3 pt-2 pb-3 [scrollbar-width:thin] sm:max-h-[28rem] lg:max-h-[31rem]"
      >
        <pre className="font-mono text-[11px] leading-[1.7] whitespace-pre-wrap [overflow-wrap:anywhere] text-slate-300 [--ind:1.1ch] sm:text-[11.5px] sm:[--ind:2ch]">
          <code>
            {lines.map((line) => {
              const isLit = line.path !== null && keys.has(line.path);
              const inGroup = group !== null && (line.owner === group || line.owner.startsWith(`${group}/`));
              return (
                <span
                  key={line.id}
                  data-lit={isLit || undefined}
                  className={cn(
                    "block rounded-[3px] pr-1 transition-colors duration-300",
                    isLit
                      ? "bg-[color:var(--chapter)]/[0.16] shadow-[inset_2px_0_0_var(--chapter)]"
                      : inGroup
                        ? "bg-white/[0.04]"
                        : "",
                  )}
                  style={{ paddingLeft: `calc(var(--ind) * ${line.depth} + 2ch + 4px)`, textIndent: "-2ch" }}
                >
                  {line.content}
                </span>
              );
            })}
          </code>
        </pre>
      </div>
      <p className="border-t border-white/[0.06] px-4 py-2 font-mono text-[10px] leading-relaxed [overflow-wrap:anywhere] text-slate-500">
        {situation.source.kind === "pinned" ? "pinned: " : "built from the source; fixture: "}
        {situation.source.file}
        {situation.source.note ? <span className="block text-slate-500/90">{situation.source.note}</span> : null}
      </p>
    </div>
  );
}

// ─── The JSON, one line at a time ───────────────────────────────────────────

type Line = {
  id: string;
  depth: number;
  /** The field this line shows (null for closing brackets and cuts). */
  path: string | null;
  /** The container the line belongs to, for tinting a whole recovery entry. */
  owner: string;
  content: ReactNode;
};

function isCut(value: Json): boolean {
  return (
    value !== null &&
    typeof value === "object" &&
    !Array.isArray(value) &&
    Object.keys(value).length === 1 &&
    (value as Record<string, Json>)[ELIDED] === true
  );
}

function join(path: string, key: string | number): string {
  return path === "" ? String(key) : `${path}/${key}`;
}

/** The record as display lines, with cuts and folded fields marked "…". */
function jsonLines(root: Json, compact: boolean): Line[] {
  const lines: Line[] = [];
  const push = (depth: number, path: string | null, owner: string, content: ReactNode) =>
    lines.push({ id: String(lines.length), depth, path, owner, content });

  const walk = (value: Json, path: string, depth: number, key: string | null, comma: boolean) => {
    const label = key === null ? null : <Key name={key} />;
    const tail = comma ? <span className="text-slate-600">,</span> : null;
    if (value === null || typeof value !== "object") {
      push(depth, path, path, <>{label}<Scalar field={key} value={value} />{tail}</>);
      return;
    }
    if (Array.isArray(value)) {
      const flat = value.every((item) => item === null || typeof item !== "object");
      if (value.length === 0 || (flat && JSON.stringify(value).length <= 64)) {
        push(
          depth,
          path,
          path,
          <>
            {label}
            <span className="text-slate-500">[</span>
            {value.map((item, index) => (
              <Fragment key={index}>
                {index > 0 ? <span className="text-slate-600">, </span> : null}
                <Scalar field={key} value={item} />
              </Fragment>
            ))}
            <span className="text-slate-500">]</span>
            {tail}
          </>,
        );
        return;
      }
      push(depth, path, path, <>{label}<span className="text-slate-500">[</span></>);
      value.forEach((item, index) => {
        if (isCut(item)) push(depth + 1, null, path, <Cut />);
        else walk(item, join(path, index), depth + 1, null, index < value.length - 1);
      });
      push(depth, null, path, <><span className="text-slate-500">]</span>{tail}</>);
      return;
    }
    if (isCut(value)) {
      push(depth, path, path, <>{label}<span className="text-slate-500">{"{ "}</span><Cut /><span className="text-slate-500">{" }"}</span>{tail}</>);
      return;
    }
    type Member = { key: string; value: Json } | { folded: number | null };
    const members: Member[] = [];
    let folded = 0;
    let foldAt = -1;
    let cut = false;
    for (const [name, child] of Object.entries(value)) {
      if (name === ELIDED) {
        cut = true;
        continue;
      }
      if (compact && FOLDED.has(name)) {
        if (foldAt < 0) foldAt = members.length;
        folded += 1;
        continue;
      }
      members.push({ key: name, value: child });
    }
    if (folded > 0) members.splice(foldAt, 0, { folded });
    if (cut) members.push({ folded: null });
    push(depth, path === "" ? null : path, path, <>{label}<span className="text-slate-500">{"{"}</span></>);
    members.forEach((member, index) => {
      if ("folded" in member) push(depth + 1, null, path, <Cut count={member.folded} />);
      else walk(member.value, join(path, member.key), depth + 1, member.key, index < members.length - 1);
    });
    push(depth, null, path, <><span className="text-slate-500">{"}"}</span>{tail}</>);
  };

  walk(root, "", 0, null, false);
  return lines;
}

function Key({ name }: { name: string }) {
  return (
    <>
      <span className="text-slate-500">&quot;{name}&quot;</span>
      <span className="text-slate-600">: </span>
    </>
  );
}

function Cut({ count }: { count?: number | null }) {
  return <span className="text-slate-500">{count ? `… ${count} more` : "…"}</span>;
}

const COMMAND_FIELDS = new Set(["command", "repair", "preflightCommand", "example"]);

function Scalar({ field, value }: { field: string | null; value: Json }) {
  if (value === null) return <span className="text-slate-400">null</span>;
  if (typeof value === "boolean") return <span className="text-sky-300">{String(value)}</span>;
  if (typeof value === "number") return <span className="text-cyan-200">{value}</span>;
  const tone =
    field === "code" || field === "schema"
      ? "text-[color:var(--chapter)]"
      : field === "severity" || field === "riskClass"
        ? "text-amber-200"
        : field !== null && COMMAND_FIELDS.has(field)
          ? "text-emerald-200"
          : "text-violet-100";
  return <span className={tone}>{JSON.stringify(value)}</span>;
}

/** The fields one read lights, and the recovery entry being acted on. */
function litFields(situation: Situation, step: number): { keys: Set<string>; group: string | null } {
  const entry = situation.entry.join("/");
  const recovery = situation.recovery ? situation.recovery.join("/") : null;
  const first = firstAction(situation);
  const chosen = recovery && first ? `${recovery}/${first.index}` : null;
  const keys = new Set<string>();
  const light = (names: readonly string[]) => {
    if (!chosen || !first) {
      keys.add(`${entry}/repair`);
      return;
    }
    for (const name of names) if (first.entry[name] !== undefined) keys.add(`${chosen}/${name}`);
  };
  if (step === 0) {
    keys.add("schema");
    if (at(situation.record, ["success"]) !== undefined) keys.add("success");
  } else if (step === 2) {
    keys.add(`${entry}/code`);
    keys.add(`${entry}/severity`);
    for (const path of situation.evidence) keys.add(path);
  } else if (step === 3) {
    if (recovery) for (const { index } of sortedRecovery(situation)) keys.add(`${recovery}/${index}/priority`);
    else keys.add(`${entry}/repair`);
  } else if (step === 4) {
    light(["riskClass", "requiresHumanApproval", "preflightCommand"]);
  } else if (step === 5) {
    light(
      first?.next === "run_preflight_first"
        ? ["preflightCommand", "command"]
        : ["command", "flagName", "valueHint", "example", "envName"],
    );
  } else if (step === 6) {
    keys.add(`${entry}/code`);
  }
  return { keys, group: step >= 3 ? chosen : null };
}

// ─── The right column ───────────────────────────────────────────────────────

function str(value: Json | undefined): string {
  return typeof value === "string" ? value : value === undefined ? "" : JSON.stringify(value);
}

/** What a recovery entry would have the agent do, in a few words. */
function actionText(entry: Record<string, Json>): string {
  if (typeof entry.command === "string") return entry.command;
  if (typeof entry.example === "string") return entry.example;
  if (typeof entry.flagName === "string") return `${entry.flagName} ${str(entry.valueHint)}`.trim();
  return str(entry.rationale);
}

/** One line for a read already made. */
function summary(situation: Situation, index: number): string {
  const entry = at(situation.record, situation.entry) as Record<string, Json>;
  const first = firstAction(situation);
  const exit = EXIT_CODES.find((code) => code.code === situation.exit);
  switch (index) {
    case 0:
      return `schema ${JSON.stringify(at(situation.record, ["schema"]))}${at(situation.record, ["success"]) === true ? " · success true" : ""}`;
    case 1:
      return `exit ${situation.exit} · ${exit?.meaning ?? ""}`;
    case 2:
      return `${str(entry.code)} · ${str(entry.severity)}`;
    case 3:
      return first ? `p${str(first.entry.priority)} · ${actionText(first.entry)}` : "no recovery[] · use the repair as written";
    case 4:
      return first ? `${first.riskClass} → ${first.next}` : "no riskClass · ee's exact repair";
    case 5:
      return situation.reading.act.commands.map((command) => (command.startsWith("ee ") ? `$ ${command}` : command)).join(" · ");
    default:
      return `then: ${VERDICT[situation.verdict].label}`;
  }
}

function Prose({ children }: { children: ReactNode }) {
  return <p className="mt-2 text-[12.5px] leading-snug text-slate-300">{children}</p>;
}

function Finding({ situation, index }: { situation: Situation; index: number }) {
  const entry = at(situation.record, situation.entry) as Record<string, Json>;
  const { reading } = situation;

  if (index === 0) {
    const success = at(situation.record, ["success"]);
    return (
      <>
        <p className="font-mono text-[11.5px]">
          <span className="text-slate-500">schema </span>
          <span className="text-[color:var(--chapter)]">{JSON.stringify(at(situation.record, ["schema"]))}</span>
          {success !== undefined ? (
            <>
              <span className="text-slate-500"> · success </span>
              <span className="text-sky-300">{String(success)}</span>
            </>
          ) : null}
        </p>
        <Prose>{reading.envelope}</Prose>
      </>
    );
  }

  if (index === 1) {
    const exit = EXIT_CODES.find((code) => code.code === situation.exit);
    return (
      <>
        <ul className="flex flex-wrap gap-1" aria-label="ee's exit codes">
          {EXIT_CODES.map((code) => (
            <li
              key={code.code}
              title={code.meaning}
              className={cn(
                "inline-flex h-5 min-w-5 items-center justify-center rounded-md border px-1 font-mono text-[10px] tabular-nums",
                code.code === situation.exit
                  ? "border-[color:var(--chapter)]/60 bg-[color:var(--chapter)]/20 text-white"
                  : "border-white/[0.07] text-slate-500",
              )}
            >
              {code.code}
            </li>
          ))}
        </ul>
        <Prose>
          <span className="font-mono text-[11.5px] text-white">exit {situation.exit}</span> is {exit?.meaning}
          {situation.exit === 0
            ? ". A success can still be degraded, and only degraded[] says so."
            : `, ProcessExitCode::${exit?.variant}: the class is known before a byte of JSON is read.`}
        </Prose>
      </>
    );
  }

  if (index === 2) {
    const severity = str(entry.severity);
    return (
      <>
        <p className="flex flex-wrap items-center gap-2">
          <span className="font-mono text-[12px] text-[color:var(--chapter)]">{str(entry.code)}</span>
          <span className={cn("rounded-full border px-2 py-px font-mono text-[10px]", SEVERITY_TONE[severity] ?? SEVERITY_TONE.low)}>
            {severity}
          </span>
        </p>
        <Prose>{reading.broke}</Prose>
      </>
    );
  }

  if (index === 3) {
    const rows = sortedRecovery(situation);
    return (
      <>
        {rows.length > 0 ? (
          <ul className="space-y-1">
            {rows.map(({ index: position, entry: action }, order) => (
              <li
                key={position}
                className={cn(
                  "grid grid-cols-[1.9rem_minmax(0,1fr)] gap-2 rounded-lg px-2 py-1 font-mono text-[11px] leading-snug",
                  order === 0 ? "bg-[color:var(--chapter)]/[0.1] text-white" : "text-slate-400",
                )}
              >
                <span className="tabular-nums">p{str(action.priority)}</span>
                <span className="min-w-0 [overflow-wrap:anywhere]">
                  <span className="text-slate-500">{str(action.kind)} · </span>
                  {actionText(action)}
                </span>
              </li>
            ))}
          </ul>
        ) : (
          <p className="font-mono text-[11px] leading-snug [overflow-wrap:anywhere] text-slate-300">
            <span className="text-slate-500">recovery[] </span>none ·<span className="text-slate-500"> repair </span>
            <span className="text-emerald-200">{JSON.stringify(entry.repair)}</span>
          </p>
        )}
        <Prose>{reading.plan}</Prose>
      </>
    );
  }

  if (index === 4) {
    const first = firstAction(situation);
    if (!first) {
      return (
        <>
          <p className="font-mono text-[11px] text-slate-400">
            riskClass <span className="text-slate-500">— not given</span>
          </p>
          <Prose>{reading.safety}</Prose>
        </>
      );
    }
    const approval = first.entry.requiresHumanApproval === true;
    return (
      <>
        <p className="font-mono text-[11px] [overflow-wrap:anywhere]">
          <span className="text-slate-500">riskClass </span>
          <span className="text-amber-200">{first.riskClass}</span>
        </p>
        <div className="mt-2 grid grid-cols-2 gap-2">
          <Readout
            label="Human approval"
            value={String(approval)}
            tone={approval ? "wrong" : "neutral"}
            note={<span className="font-mono">requiresHumanApproval</span>}
          />
          <Readout
            label="Preflight"
            value={typeof first.entry.preflightCommand === "string" ? "given" : "none"}
            note={<span className="font-mono">preflightCommand</span>}
          />
        </div>
        <p className="mt-2 flex flex-wrap items-center gap-x-2 gap-y-1 font-mono text-[11px]">
          <span className="text-slate-500">ee&rsquo;s next action</span>
          <ArrowRight className="h-3 w-3 text-slate-500" aria-hidden="true" />
          <span className="rounded-md border border-[color:var(--chapter)]/50 px-1.5 py-px text-[color:var(--chapter)]">
            {first.next}
          </span>
        </p>
        <Prose>{reading.safety}</Prose>
      </>
    );
  }

  if (index === 5) {
    return (
      <>
        <div className="space-y-1">
          {reading.act.commands.map((command) => (
            <pre
              key={command}
              className="rounded-lg border border-white/10 bg-black/40 px-2.5 py-1.5 font-mono text-[11px] leading-relaxed whitespace-pre-wrap [overflow-wrap:anywhere] text-emerald-200"
            >
              {command.startsWith("ee ") ? `$ ${command}` : command}
            </pre>
          ))}
        </div>
        <p className="mt-1.5 text-[12px] leading-snug text-slate-400">{reading.act.note}</p>
      </>
    );
  }

  return <Prose>{reading.recheck}</Prose>;
}

function RuleTable({ situation, lit }: { situation: Situation; lit: boolean }) {
  const first = firstAction(situation);
  const current = lit && first ? first.riskClass : null;
  return (
    <div className="rounded-xl border border-white/[0.07] bg-black/20 p-3">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <p className="text-[10px] font-black tracking-[0.2em] text-slate-500 uppercase">Every risk class, and where it leads</p>
        <BasisBadge basis="rule" />
      </div>
      <ul className="mt-2 grid gap-x-6 gap-y-0.5 lg:grid-flow-col lg:grid-rows-4">
        {RULE_ROWS.map((row) => (
          <li
            key={row.riskClass}
            className={cn(
              "grid grid-cols-1 gap-x-3 rounded-md px-2 py-[3px] font-mono text-[10.5px] leading-snug transition-colors duration-300 sm:grid-cols-[minmax(0,1fr)_auto] sm:py-1",
              row.riskClass === current ? "bg-[color:var(--chapter)]/[0.14] text-white" : "text-slate-400",
            )}
          >
            <span className="min-w-0 [overflow-wrap:anywhere]">{row.riskClass}</span>
            <span className={cn("sm:text-right", HUMAN_ENDS.has(row.leads) ? "text-rose-200/80" : "text-slate-500")}>
              {row.leads}
            </span>
          </li>
        ))}
      </ul>
      <p className="mt-2 text-[11px] leading-snug text-slate-500">
        repair_command_preflight_policy, src/core/preflight_guard.rs. Two classes always end at a human and one at an
        operator; three more do when <span className="font-mono">requiresHumanApproval</span> is true.
      </p>
    </div>
  );
}
