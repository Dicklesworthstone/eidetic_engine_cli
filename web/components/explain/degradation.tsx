"use client";

import { Plug, PlugZap } from "lucide-react";
import { type CSSProperties, Fragment, useState } from "react";

import { DemoButton, DemoFrame, Readout } from "@/components/explain/demo";
import { Cmd } from "@/components/instrument";
import { cn } from "@/lib/utils";

/**
 * What still works when a part is missing, and the `degraded` entry ee reports
 * for it. Each entry's code, severity, message and repair are the source's own
 * (the files are named per entry); each code has a fixture under
 * tests/fixtures/failure_modes/ that triggers it and checks the emission.
 */

type Part = "model" | "index" | "graph" | "cass" | "reranker" | "network";

const PARTS: readonly { id: Part; name: string; detail: string; off: string }[] = [
  { id: "model", name: "Semantic model", detail: "Model2Vec potion-multilingual-128M · 256-d", off: "missing" },
  { id: "index", name: "Search index", detail: "Frankensearch, one generation behind the store", off: "stale" },
  { id: "graph", name: "Graph snapshot", detail: "FrankenNetworkX projection of memory links", off: "stale" },
  { id: "cass", name: "cass binary", detail: "the session-history evidence source", off: "missing" },
  { id: "reranker", name: "Local reranker", detail: "optional cross-encoder, rerank auto mode", off: "absent" },
  { id: "network", name: "Network", detail: "after the one-time pinned model download", off: "offline" },
];

type Entry = { code: string; severity: string; message: string; repair: string | null; from: string };

/** The `degraded` entry for each part, verbatim from the source except where `…` marks a cut or `<…>` a runtime value. */
const ENTRY: Partial<Record<Part, Entry>> = {
  model: {
    code: "embed_model_unavailable",
    severity: "warning",
    message:
      "Embedding model unavailable (<reason>); semantic similarity is disabled and lexical search remains available.",
    repair: "ee index reembed --workspace .",
    from: "src/core/search.rs",
  },
  index: {
    code: "search_index_stale",
    severity: "medium",
    message:
      "Search index is stale; returning lexical fallback results from the current index. Database generation is <n>; index generation is <m>. Newer memories may be omitted until the index is rebuilt.",
    repair: "ee index rebuild --workspace .",
    from: "src/core/search.rs",
  },
  graph: {
    code: "graph_snapshot_stale",
    severity: "medium",
    message: "Graph snapshot is marked stale; enrichment may lag source memory links.",
    repair: "ee graph centrality-refresh",
    from: "src/graph/mod.rs",
  },
  cass: {
    code: "cass_unavailable",
    severity: "medium",
    message: "cass unavailable: cass binary '/missing/cass' is not allowed: <reason>",
    repair: "set EE_CASS_BINARY to an absolute, trusted cass executable … or set [cass.binary] in config; …",
    from: "src/output/mod.rs · src/cass/error.rs",
  },
  reranker: {
    code: "rerank_model_unavailable",
    severity: "low",
    message:
      "No usable local reranker is registered. Search is using fusion-only ranking. Network download is unavailable, but a verified offline reranker artifact can be imported explicitly.",
    repair: null,
    from: "src/core/search.rs",
  },
};

type State = "ok" | "degraded" | "off";
type Off = Record<Part, boolean>;

/** What each capability does with the parts that are missing. */
const CAPABILITIES: readonly { name: string; judge: (off: Off) => { state: State; note: string } }[] = [
  {
    name: "Capture · remember, journal, outcome",
    judge: () => ({ state: "ok", note: "writes go to the source of truth" }),
  },
  {
    name: "Lexical search · BM25",
    judge: (off) =>
      off.index ? { state: "degraded", note: "current index; the newest memories may be missing" } : { state: "ok", note: "full" },
  },
  {
    name: "Semantic similarity",
    judge: (off) =>
      off.model
        ? { state: "off", note: "lexical search carries the query" }
        : off.index
          ? { state: "off", note: "lexical fallback until the rebuild" }
          : { state: "ok", note: off.network ? "from the cached model" : "local Model2Vec" },
  },
  {
    name: "Hybrid fusion · RRF",
    judge: (off) =>
      off.model || off.index ? { state: "degraded", note: "lexical arm only" } : { state: "ok", note: "lexical + semantic" },
  },
  {
    name: "Reranking",
    judge: (off) =>
      off.model
        ? { state: "off", note: "needs the semantic arm" }
        : off.reranker
          ? { state: "off", note: "fusion-only ranking" }
          : { state: "ok", note: "optional, local" },
  },
  {
    name: "Graph boosts and enrichment",
    judge: (off) =>
      off.graph ? { state: "degraded", note: "retrieval without fresh graph boosts" } : { state: "ok", note: "current links" },
  },
  {
    name: "Session evidence import",
    judge: (off) =>
      off.cass ? { state: "off", note: "explicit ee remember still captures" } : { state: "ok", note: "ee import cass" },
  },
  {
    name: "Context packs · ee pack",
    judge: (off) =>
      off.model || off.index || off.graph || off.reranker
        ? { state: "degraded", note: "built from what remains, and says so" }
        : { state: "ok", note: "every section, every arm" },
  },
  {
    name: "Provenance and the JSON contract",
    judge: () => ({ state: "ok", note: "every response, always" }),
  },
];

const STATE_STYLE: Record<State, { pill: string; label: string }> = {
  ok: { pill: "border-emerald-400/30 bg-emerald-400/10 text-emerald-200", label: "works" },
  degraded: { pill: "border-amber-400/35 bg-amber-400/10 text-amber-200", label: "degraded" },
  off: { pill: "border-rose-400/35 bg-rose-400/10 text-rose-200", label: "off" },
};

const NONE: Off = { model: false, index: false, graph: false, cass: false, reranker: false, network: false };
const ALL: Off = { model: true, index: true, graph: true, cass: true, reranker: true, network: true };

/** Documented failure modes: one fixture per degraded code (ls tests/fixtures/failure_modes/*.json, at 2026-11-24). */
const FAILURE_MODES = 630;

export function DegradationBoard() {
  const [off, setOff] = useState<Off>({ ...NONE, model: true });
  const entries = PARTS.flatMap((part) => (off[part.id] && ENTRY[part.id] ? [ENTRY[part.id] as Entry] : []));
  const judged = CAPABILITIES.map((capability) => ({ name: capability.name, ...capability.judge(off) }));
  const working = judged.filter((row) => row.state !== "off").length;
  const pulled = PARTS.filter((part) => off[part.id]).length;

  return (
    <DemoFrame
      eyebrow="Graceful degradation"
      title="Pull a plug. Read what ee says."
      basis="record"
      source={
        <>
          tests/fixtures/failure_modes/*.json ({FAILURE_MODES} fixtures, one per degraded code: embed_model_unavailable,
          search_index_stale, graph_snapshot_stale, cass_unavailable, rerank_model_unavailable, …) · the messages from the
          files named on each entry · README.md · Design Philosophy §7, Graceful Degradation
        </>
      }
    >
      <div className="grid grid-cols-1 gap-6 lg:grid-cols-[18.5rem_minmax(0,1fr)]">
        {/* The parts */}
        <div className="space-y-2">
          {PARTS.map((part) => {
            const down = off[part.id];
            return (
              <button
                key={part.id}
                type="button"
                role="switch"
                aria-checked={!down}
                aria-label={`${part.name}: ${down ? part.off : "available"}`}
                onClick={() => setOff((current) => ({ ...current, [part.id]: !current[part.id] }))}
                className={cn(
                  "flex w-full items-center gap-3 rounded-xl border px-3 py-2.5 text-left transition focus-visible:ring-2 focus-visible:ring-violet-400 focus-visible:outline-none",
                  down ? "border-rose-400/30 bg-rose-400/[0.06]" : "border-white/10 bg-black/30 hover:border-white/20",
                )}
              >
                <span
                  aria-hidden="true"
                  className="sphere h-2.5 w-2.5 shrink-0"
                  style={{ "--sphere": down ? "#fb7185" : "#34d399" } as CSSProperties}
                />
                <span className="min-w-0 flex-1">
                  <span className="block text-[13px] font-semibold text-white">{part.name}</span>
                  <span className="block truncate text-[11px] text-slate-500">{part.detail}</span>
                </span>
                <span
                  aria-hidden="true"
                  className={cn(
                    "relative h-5 w-9 shrink-0 rounded-full border transition-colors",
                    down ? "border-rose-400/40 bg-rose-400/15" : "border-emerald-400/40 bg-emerald-400/20",
                  )}
                >
                  <span
                    className={cn(
                      "absolute top-0.5 h-3.5 w-3.5 rounded-full transition-all duration-300",
                      down ? "left-0.5 bg-rose-300" : "left-[1.1rem] bg-emerald-300",
                    )}
                  />
                </span>
                <span
                  className={cn(
                    "w-[3.6rem] shrink-0 text-right font-mono text-[10px] tracking-[0.12em] uppercase",
                    down ? "text-rose-300" : "text-emerald-300/80",
                  )}
                >
                  {down ? part.off : "ok"}
                </span>
              </button>
            );
          })}
          <div className="flex flex-wrap gap-2 pt-2">
            <DemoButton onClick={() => setOff(ALL)} disabled={pulled === PARTS.length}>
              <PlugZap className="h-3.5 w-3.5" /> Pull every plug
            </DemoButton>
            <DemoButton onClick={() => setOff(NONE)} disabled={pulled === 0}>
              <Plug className="h-3.5 w-3.5" /> Restore all
            </DemoButton>
          </div>
        </div>

        <div className="min-w-0 space-y-5">
          <div className="grid grid-cols-3 gap-2">
            <Readout label="Still working" value={`${working}/${judged.length}`} tone="rule" note="capabilities not switched off" />
            <Readout
              label="degraded[]"
              value={entries.length}
              tone={entries.length > 0 ? "wrong" : "neutral"}
              note="entries in the JSON"
            />
            <Readout label="Failure modes" value={FAILURE_MODES} note="documented, one fixture each" />
          </div>

          {/* The capabilities */}
          <ul className="ruled rounded-2xl border border-white/[0.08] bg-black/20 px-4" aria-live="polite">
            {judged.map((row) => (
              <li key={row.name} className="flex flex-wrap items-center gap-x-3 gap-y-1 py-2.5">
                <span
                  className={cn(
                    "w-[4.9rem] shrink-0 rounded-full border px-2 py-0.5 text-center font-mono text-[10px] tracking-[0.08em] uppercase",
                    STATE_STYLE[row.state].pill,
                  )}
                >
                  {STATE_STYLE[row.state].label}
                </span>
                <span className="min-w-0 flex-1 text-[13px] text-slate-200">{row.name}</span>
                <span className="w-full text-[12px] text-slate-500 sm:w-auto sm:text-right">{row.note}</span>
              </li>
            ))}
          </ul>

          {/* What ee says */}
          <div className="rounded-2xl border border-white/[0.08] bg-[#030209] p-4">
            <p className="font-mono text-[10px] tracking-[0.2em] text-slate-500 uppercase">ee search … --json · data.degraded</p>
            <pre className="mt-3 overflow-x-auto font-mono text-[12px] leading-[1.7] whitespace-pre-wrap [overflow-wrap:anywhere] text-slate-300">
              <code>
                <span className="text-slate-500">{'"degraded": ['}</span>
                {entries.length === 0 ? <span className="text-slate-500">{"]"}</span> : null}
                {entries.map((entry, index) => (
                  <Fragment key={entry.code}>
                    {"\n  {"}
                    <Field name="code" value={entry.code} className="text-[color:var(--chapter)]" />
                    <Field name="severity" value={entry.severity} className={entry.severity === "low" ? "text-sky-200" : "text-amber-200"} />
                    <Field name="message" value={entry.message} className="text-violet-100" />
                    <Field name="repair" value={entry.repair} className="text-emerald-200" last />
                    {`\n  }${index < entries.length - 1 ? "," : ""}`}
                    <span className="text-slate-600">{`  // ${entry.from}`}</span>
                  </Fragment>
                ))}
                {entries.length > 0 ? <span className="text-slate-500">{"\n]"}</span> : null}
              </code>
            </pre>
          </div>

          <div className="grid grid-cols-1 gap-3 text-[13px] leading-relaxed text-slate-400 md:grid-cols-2">
            <p>
              <span className="font-semibold text-slate-200">Offline is not a failure.</span> With the model cached, going offline
              changes nothing. A machine that has never fetched it falls back to deterministic hash embeddings and says so with{" "}
              <Cmd>embed_model_unavailable</Cmd>.
            </p>
            <p>
              <span className="font-semibold text-slate-200">A stale index is not a dead end.</span> When a read-only request&rsquo;s
              whole corpus fits 4,096 rows and 16 MiB, ee searches a complete in-memory snapshot instead and reports{" "}
              <Cmd>search_live_snapshot_lexical</Cmd>.
            </p>
          </div>
        </div>
      </div>
    </DemoFrame>
  );
}

function Field({
  name,
  value,
  className,
  last = false,
}: {
  name: string;
  value: string | null;
  className: string;
  last?: boolean;
}) {
  return (
    <>
      {"\n    "}
      <span className="text-slate-500">{`"${name}": `}</span>
      {value === null ? <span className="text-slate-400">null</span> : <span className={className}>{JSON.stringify(value)}</span>}
      {last ? "" : ","}
    </>
  );
}
