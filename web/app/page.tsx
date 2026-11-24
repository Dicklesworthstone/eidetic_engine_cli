import { ArrowDown, ArrowRight, Check, ExternalLink, Minus } from "lucide-react";
import type { CSSProperties, ReactNode } from "react";

import { CopyCommand } from "@/components/copy-command";
import { Amnesia } from "@/components/explain/amnesia";
import { DecayCurves } from "@/components/explain/decay";
import { DegradationBoard } from "@/components/explain/degradation";
import { GraphLens } from "@/components/explain/graph-lens";
import { HashPrefix } from "@/components/explain/hash-prefix";
import { IncidentCard } from "@/components/explain/incident-card";
import { MemoryLoop } from "@/components/explain/loop";
import { PackBuilder } from "@/components/explain/pack-builder";
import { PackPipeline } from "@/components/explain/pipeline";
import { TrustPosterior } from "@/components/explain/trust";
import { Hero } from "@/components/hero";
import { Cmd, MicroLabel, RecallTitle } from "@/components/instrument";
import { Reveal } from "@/components/reveal";
import { RevealController } from "@/components/reveal-controller";
import { ChapterBlock, Section } from "@/components/section";
import { SiteHeader } from "@/components/site-header";
import { SkipOffscreen } from "@/components/skip-offscreen";
import {
  COUNTED,
  type Chapter,
  chapters,
  comparison,
  curationSteps,
  firstCommands,
  forbidden,
  graphTools,
  guards,
  harnesses,
  heroStats,
  install,
  limitations,
  memoryKinds,
  memoryLevels,
  releases,
  rigor,
  siteConfig,
  sixQuestions,
  stack,
} from "@/lib/content";
import { LEVEL_COLOUR, type Level } from "@/lib/palette";
import { cn } from "@/lib/utils";

function chapter(id: string): Chapter {
  const found = chapters.find((item) => item.id === id);
  if (!found) throw new Error(`no chapter ${id}`);
  return found;
}

/** A section's number within its chapter: "03.2". */
function index(chapterId: string, sectionId: string): string {
  const owner = chapter(chapterId);
  const at = owner.sections.findIndex((section) => section.id === sectionId);
  if (at < 0) throw new Error(`chapter ${chapterId} has no section ${sectionId}`);
  return `${owner.number}.${at + 1}`;
}

const link = "text-violet-300 underline decoration-violet-300/30 underline-offset-4 transition-colors hover:decoration-violet-300";

function HeroWords() {
  return (
    <>
      <p className="inline-flex items-center gap-3 rounded-full border border-violet-300/20 bg-violet-400/[0.06] py-1.5 pr-4 pl-2">
        <span aria-hidden="true" className="sphere h-3 w-3 [--sphere:#a78bfa]" />
        <span className="font-mono text-[10.5px] font-medium tracking-[0.22em] text-violet-200/90 uppercase sm:text-[11px]">
          Memory for coding agents · Rust · local-first
        </span>
      </p>
      <h1 id="hero-heading" className="mt-7 text-[clamp(3rem,5.4vw,5.7rem)] leading-[0.94] tracking-[-0.05em] text-white">
        Your agents forget.
        <br />
        <RecallTitle lit text="[ee remembers.]" />
      </h1>
      <p className="lede mt-7 max-w-[37rem]">
        Eidetic Engine is a durable memory for coding agents. It keeps the rules, decisions, failures and evidence your
        agents learn, and hands every new session a compact context pack: fitted to a token budget, free of
        near-duplicates, and with every line saying where it came from and why it is there. One Rust binary. No cloud,
        no paid API, no daemon required.
      </p>
      <p className="mt-4 max-w-[37rem] text-[15px] leading-relaxed text-slate-400">
        <span className="font-semibold text-slate-200">New in {siteConfig.version}:</span> a fix your agents found once
        is recalled by the error&apos;s fingerprint, across rustc, Go, Vitest and Jest output, and a warm daemon serves
        the same pack, hash for hash, in a fraction of the time.{" "}
        <a className={link} href={siteConfig.changelog}>
          What changed
        </a>
        .
      </p>
      <div className="mt-9 flex flex-wrap gap-3">
        <a
          href="#builder"
          className="inline-flex min-h-12 items-center gap-2 rounded-2xl bg-violet-400 px-5 text-[15px] font-extrabold text-black shadow-[0_0_40px_-10px_rgba(139,92,246,0.9)] transition-colors hover:bg-violet-300"
        >
          Build a pack yourself <ArrowDown className="h-4 w-4" aria-hidden="true" />
        </a>
        <a
          href="#install"
          className="inline-flex min-h-12 items-center gap-2 rounded-2xl border border-white/15 bg-white/[0.02] px-5 text-[15px] font-bold text-white transition-colors hover:border-violet-400/50 hover:text-violet-200"
        >
          Install in a minute
        </a>
      </div>
      <p className="mt-6 text-sm text-slate-500">
        Open source on{" "}
        <a className={link} href={siteConfig.github}>
          GitHub
        </a>
        . Every figure on this page names where it comes from.
      </p>
    </>
  );
}

export default function Home() {
  return (
    <>
      <SiteHeader />
      <RevealController />
      <SkipOffscreen />
      <main id="main" className="relative isolate">
        <span id="top" aria-hidden="true" className="absolute top-0" />

        {/* Hero: the specimen and the words, then the strip that drives the specimen. */}
        <section data-hero aria-labelledby="hero-heading" className="relative isolate overflow-hidden">
          <div aria-hidden="true" className="field-light absolute inset-0 -z-10" />
          <div aria-hidden="true" className="lattice absolute inset-0 -z-10" />
          <Hero words={<HeroWords />} />
        </section>

        {/* The specification strip: four facts, each with its own qualification. */}
        <section aria-label="In four figures" className="relative border-y border-white/[0.06] bg-black/20">
          <dl className="mx-auto grid max-w-7xl grid-cols-2 lg:grid-cols-4">
            {heroStats.map((stat, at) => (
              <div
                key={stat.label}
                className={cn(
                  "min-w-0 border-white/[0.06] px-5 py-7 sm:px-7 lg:py-9",
                  at % 2 === 1 && "border-l",
                  at >= 2 && "border-t lg:border-t-0",
                  at === 2 && "lg:border-l",
                )}
              >
                <dt className="font-mono text-[10px] font-medium tracking-[0.18em] text-violet-300/80 uppercase">{stat.label}</dt>
                <dd className="mt-2">
                  <span className="block text-[1.7rem] font-extrabold tracking-[-0.035em] text-white tabular-nums sm:text-[2.1rem]">
                    {stat.value}
                  </span>
                  <span className="mt-2 block text-[13px] leading-snug text-slate-400">{stat.note}</span>
                </dd>
              </div>
            ))}
          </dl>
          <p className="mx-auto max-w-7xl border-t border-white/[0.06] px-5 py-2.5 font-mono text-[10.5px] text-slate-500 sm:px-7">
            Counted in the repository at {COUNTED.commit}, {COUNTED.date}.
          </p>
        </section>

        {/* ─── 01 · Amnesia ───────────────────────────────────────────── */}
        <ChapterBlock chapter={chapter("forget")}>
          <Section
            id="amnesia"
            index={index("forget", "amnesia")}
            layout="wide"
            eyebrow="Six sessions, two agents"
            title="Session six walks into session two's [trap]."
            kicker={
              <>
                The harness gives every session a fresh context window. Without a durable place to put what it learned,
                an agent spends the start of each session re-learning the project, and the middle of it rediscovering
                failures that someone already paid for.
              </>
            }
          >
            <Amnesia />
          </Section>

          <Section
            id="layer"
            index={index("forget", "layer")}
            layout="wide"
            eyebrow="Where ee sits"
            title="The harness owns the loop. ee owns [memory]."
            kicker={
              <>
                Claude Code, Codex or any shell keeps its tools, approvals and prompt loop. ee is what it calls: agents
                push evidence in and pull context out, through one CLI with a stable JSON contract.
              </>
            }
          >
            <MemoryLoop />
          </Section>

          <Section
            id="first"
            index={index("forget", "first")}
            eyebrow="Five commands"
            title="A memory, [used], in five commands."
            kicker={
              <>
                Open a workspace, teach it something, pack it into the next session, say whether it helped, and ask why
                it was chosen. That is the whole loop; everything else on this page makes one of these five better.
              </>
            }
          >
            <ol className="space-y-4">
              {firstCommands.map((item, at) => (
                <Reveal key={item.step} delay={at * 0.05}>
                  <li className="panel lift grid grid-cols-[auto_minmax(0,1fr)] gap-x-4 gap-y-3 rounded-2xl p-5 sm:gap-x-6 sm:p-6">
                    <span className="flex h-9 w-9 items-center justify-center rounded-full border border-[color:var(--chapter)]/40 font-mono text-sm text-[color:var(--chapter)] tabular-nums">
                      {at + 1}
                    </span>
                    <div className="min-w-0">
                      <p className="text-lg font-bold tracking-tight text-white">{item.step}</p>
                      <pre className="mt-3 overflow-x-auto rounded-xl max-sm:whitespace-pre-wrap border border-white/[0.06] bg-black/40 px-4 py-3 text-[12.5px] leading-relaxed text-violet-100 [scrollbar-width:thin]">
                        <code>{item.command}</code>
                      </pre>
                      <p className="mt-3 text-[14px] leading-relaxed text-slate-400">{item.note}</p>
                    </div>
                  </li>
                </Reveal>
              ))}
            </ol>
          </Section>
        </ChapterBlock>

        {/* ─── 02 · Memory ────────────────────────────────────────────── */}
        <ChapterBlock chapter={chapter("memory")}>
          <Section
            id="decay"
            index={index("memory", "decay")}
            layout="wide"
            eyebrow="Four levels, five half-lives"
            title="Forgetting is a [feature]."
            kicker={
              <>
                A memory&apos;s freshness halves on a schedule set by what it is: a day for working notes, a year for a
                procedural rule. Freshness times confidence times utility decides when it is demoted and when it is
                tombstoned, with an audit entry either way.
              </>
            }
          >
            <DecayCurves />
            <div className="mt-6 grid grid-cols-1 gap-3 sm:grid-cols-2 lg:grid-cols-4">
              {memoryLevels.map((level, at) => (
                <Reveal key={level.level} delay={at * 0.05}>
                  <div
                    className="panel lift h-full rounded-2xl p-5"
                    style={{ "--sphere": LEVEL_COLOUR[level.level as Level] } as CSSProperties}
                  >
                    <p className="flex items-center gap-2.5">
                      <span aria-hidden="true" className="sphere h-2.5 w-2.5" />
                      <span className="font-mono text-[13px] font-medium text-white">{level.level}</span>
                    </p>
                    <p className="mt-3 text-[14px] leading-snug text-slate-300">{level.examples}</p>
                    <dl className="mt-4 grid grid-cols-2 gap-2 font-mono text-[11px]">
                      <div>
                        <dt className="text-slate-500">decay</dt>
                        <dd className="text-slate-200">{level.decay}</dd>
                      </div>
                      <div>
                        <dt className="text-slate-500">packing</dt>
                        <dd className="text-slate-200">{level.priority}</dd>
                      </div>
                    </dl>
                  </div>
                </Reveal>
              ))}
            </div>
          </Section>

          <Section
            id="kinds"
            index={index("memory", "kinds")}
            eyebrow="Typed fields"
            title="Typed, not [prose]."
            kicker={
              <>
                A failure knows its cause and the commit that reverted it; a decision knows its options and when to
                revisit it. Search filters on those fields by name, so an agent asks for{" "}
                <Cmd>--kind failure --field family^rustc</Cmd> instead of hoping the wording matches. Bare prose stays
                bare: ee never invents a field.
              </>
            }
          >
            <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
              {memoryKinds.map((kind, at) => (
                <Reveal key={kind.kind} delay={at * 0.04}>
                  <div
                    className="panel lift h-full rounded-2xl p-5"
                    style={{ "--sphere": LEVEL_COLOUR[kind.level as Level] } as CSSProperties}
                  >
                    <div className="flex items-center justify-between gap-3">
                      <p className="flex items-center gap-2.5">
                        <span aria-hidden="true" className="sphere h-2.5 w-2.5" />
                        <span className="font-mono text-[15px] font-semibold text-white">{kind.kind}</span>
                      </p>
                      <span className="font-mono text-[10px] tracking-[0.16em] text-slate-500 uppercase">{kind.level}</span>
                    </div>
                    <ul className="mt-4 flex flex-wrap gap-1.5">
                      {kind.fields.map((field) => (
                        <li
                          key={field}
                          className="rounded-md border border-[color:var(--chapter)]/25 bg-[color:var(--chapter)]/[0.06] px-2 py-0.5 font-mono text-[11px] text-amber-100"
                        >
                          {field}
                        </li>
                      ))}
                    </ul>
                    <p className="mt-4 border-t border-white/[0.06] pt-3 font-mono text-[11.5px] leading-relaxed text-slate-400">
                      {kind.example}
                    </p>
                  </div>
                </Reveal>
              ))}
            </div>
            <p className="mt-5 text-[13px] leading-relaxed text-slate-500">
              Every memory also carries its level, kind, content hash, tags, confidence, utility, importance, access
              count, source, evidence spans, links and trust class. README, &ldquo;Memory Model&rdquo;.
            </p>
          </Section>
        </ChapterBlock>

        {/* ─── 03 · The pack ──────────────────────────────────────────── */}
        <ChapterBlock chapter={chapter("pack")}>
          <Section
            id="pipeline"
            index={index("pack", "pipeline")}
            layout="wide"
            eyebrow="ee pack, stage by stage"
            title="Twelve timed stages, one [pack]."
            kicker={
              <>
                A pack is not a top-k query. It is a fixed pipeline: validate, retrieve with lexical and semantic search
                fused by rank, resolve and admit candidates, select under quotas, assemble with provenance, and report
                every stage&apos;s time and every degradation.
              </>
            }
          >
            <PackPipeline />
          </Section>

          <Section
            id="builder"
            index={index("pack", "builder")}
            layout="wide"
            eyebrow="ee's selection rule, on your screen"
            title="Build a pack [yourself]."
            kicker={
              <>
                Change the profile and the budget and watch the pack change. Every candidate says why it is in or out:
                its marginal gain, the memory it duplicates, the quota or the budget it hit. Then switch redundancy off
                and watch near-duplicates crowd out the failure you needed.
              </>
            }
          >
            <PackBuilder />
          </Section>

          <Section
            id="explain"
            index={index("pack", "explain")}
            eyebrow="Explainable retrieval"
            title="Six questions every item [answers]."
            kicker={
              <>
                An agent that cannot see why it was told something cannot weigh it. So every returned memory carries its
                answers to six questions, and <Cmd>ee why</Cmd> and <Cmd>ee pack --explain</Cmd> show the rest.
              </>
            }
          >
            <div className="grid grid-cols-1 gap-3 sm:grid-cols-2 lg:grid-cols-3">
              {sixQuestions.map((item, at) => (
                <Reveal key={item.q} delay={at * 0.04}>
                  <div className="panel lift h-full rounded-2xl p-5">
                    <p className="font-mono text-[11px] text-[color:var(--chapter)] tabular-nums">0{at + 1}</p>
                    <p className="mt-2 text-lg font-bold tracking-tight text-white">{item.q}</p>
                    <p className="mt-2 text-[14px] leading-relaxed text-slate-400">{item.a}</p>
                  </div>
                </Reveal>
              ))}
            </div>
            <div className="panel mt-4 rounded-2xl p-5 sm:p-6">
              <MicroLabel className="text-slate-400">And what the scores are not</MicroLabel>
              <p className="mt-3 text-[15px] leading-relaxed text-slate-300">
                Relevance scores rank results; they are not probabilities that a result answers the task. When results
                exist, ee reports retrieval quality as <Cmd>unknown</Cmd> rather than inventing a number, because score
                size, result count and interval coverage do not establish that an answer is correct. Judge a memory by
                its content and its evidence.
              </p>
              <p className="mt-3 font-mono text-[11px] text-slate-500">README, &ldquo;Explainable Retrieval&rdquo;</p>
            </div>
          </Section>

          <Section
            id="hash"
            index={index("pack", "hash")}
            layout="wide"
            eyebrow="Deterministic by default"
            title="Same inputs, same [hash]."
            kicker={
              <>
                Given the same snapshots, config, profile, budget and query, ties break the same way and the pack hash
                reproduces, so a pack can be cited, compared and replayed. That took more than hashing the output: the
                first version of the hash had a collision, and this is it.
              </>
            }
          >
            <HashPrefix />
          </Section>
        </ChapterBlock>

        {/* ─── 04 · Learning ──────────────────────────────────────────── */}
        <ChapterBlock chapter={chapter("learn")}>
          <Section
            id="trust"
            index={index("learn", "trust")}
            layout="wide"
            eyebrow="Bayesian trust classes"
            title="Trust is earned, one [outcome] at a time."
            kicker={
              <>
                Every memory starts at the trust its source earns: 0.85 for a human typing it, 0.50 for an agent&apos;s
                say-so, 0.45 for an imported session line. Then outcomes move a Beta posterior, and the class changes only
                when the 90% interval clears a threshold. Press the buttons.
              </>
            }
          >
            <TrustPosterior />
          </Section>

          <Section
            id="fixes"
            index={index("learn", "fixes")}
            layout="wide"
            eyebrow="Failure-to-fix recall"
            title="A fix, remembered by its [fingerprint]."
            kicker={
              <>
                ee imports your past agent sessions through cass, finds the failures that were fixed and verified, and
                keeps each as a short incident card keyed by the error&apos;s fingerprint. The next time that error class
                appears, in any session, the fix comes back with its provenance.
              </>
            }
          >
            <IncidentCard />
          </Section>

          <Section
            id="curate"
            index={index("learn", "curate")}
            eyebrow="No silent memory mutation"
            title="Propose. Validate. [Apply]."
            kicker={
              <>
                Memory that rewrites itself in the dark cannot be trusted. Every promotion, merge, replacement and
                tombstone goes through a reviewable candidate and leaves an audit entry. The steward proposes; it never
                silently rewrites procedural memory, and turning a session journal into candidates needs no LLM.
              </>
            }
          >
            <ol className="relative grid grid-cols-1 gap-4">
              {curationSteps.map((step, at) => (
                <Reveal key={step.verb} delay={at * 0.06}>
                  <li className="panel lift relative grid grid-cols-[auto_minmax(0,1fr)] gap-x-5 rounded-2xl p-5 sm:p-6">
                    <span className="flex flex-col items-center">
                      <span className="sphere h-4 w-4" aria-hidden="true" />
                      {at < curationSteps.length - 1 ? (
                        <span aria-hidden="true" className="mt-2 w-px flex-1 bg-gradient-to-b from-[color:var(--chapter)]/50 to-transparent" />
                      ) : null}
                    </span>
                    <div className="min-w-0">
                      <p className="text-xl font-extrabold tracking-tight text-white">{step.verb}</p>
                      <p className="mt-2 text-[14.5px] leading-relaxed text-slate-400">{step.text}</p>
                      <pre className="mt-3 overflow-x-auto font-mono max-sm:whitespace-pre-wrap text-[12px] text-sky-100/90 [scrollbar-width:thin]">
                        <code>{step.command}</code>
                      </pre>
                    </div>
                  </li>
                </Reveal>
              ))}
            </ol>
          </Section>

          <Section
            id="guard"
            index={index("learn", "guard")}
            eyebrow="Privacy and trust"
            title="Memory that can't be turned [against you]."
            kicker={
              <>
                A memory store is an attack surface: whatever gets in reaches every future session. So secrets are
                stripped on the way in, injected instructions are screened on the way out, and nothing ee says can stop
                your agent from acting.{" "}
                <a className={link} href={siteConfig.trustModel}>
                  The trust model
                </a>
                .
              </>
            }
          >
            <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
              {guards.map((guard, at) => (
                <Reveal key={guard.title} delay={at * 0.05}>
                  <div className="panel lift h-full rounded-2xl p-5 sm:p-6">
                    <span aria-hidden="true" className="sphere h-2.5 w-2.5" />
                    <p className="mt-3 text-lg font-bold tracking-tight text-white">{guard.title}</p>
                    <p className="mt-2 text-[14px] leading-relaxed text-slate-400">{guard.text}</p>
                  </div>
                </Reveal>
              ))}
            </div>
            <pre className="panel mt-4 overflow-x-auto rounded-2xl max-sm:whitespace-pre-wrap max-sm:break-all p-5 font-mono text-[12px] leading-relaxed text-slate-300 [scrollbar-width:thin]">
              <code>
                <span className="text-slate-500">$ </span>ee remember &quot;DATABASE_URL=postgres://user:hunter2@host/db&quot;
                {"\n"}
                <span className="text-slate-500"># stored as: </span>
                <span className="text-sky-200">&quot;DATABASE_URL=postgres://user:***REDACTED:password***@host/db&quot;</span>
              </code>
            </pre>
          </Section>
        </ChapterBlock>

        {/* ─── 05 · Graph ─────────────────────────────────────────────── */}
        <ChapterBlock chapter={chapter("graph")}>
          <Section
            id="lenses"
            index={index("graph", "lenses")}
            layout="wide"
            eyebrow="Structure, not just similarity"
            title="One memory graph, five [lenses]."
            kicker={
              <>
                Similarity finds memories that look like the task. Structure finds the ones that matter: the rule that
                twelve decisions rest on, the fact that is the only bridge between two topics. Tap a lens, then a memory.
              </>
            }
          >
            <GraphLens />
          </Section>

          <Section
            id="toolbox"
            index={index("graph", "toolbox")}
            eyebrow="FrankenNetworkX, in the CLI"
            title="Real graph algorithms, each a [question]."
            kicker={
              <>
                Each one is a command with deterministic JSON output. Graph views explain and navigate memory; they never
                replace a memory&apos;s own provenance, and the features that let graph structure move a pack&apos;s
                ranking are opt-in.
              </>
            }
          >
            <ul className="ruled panel rounded-2xl px-5 sm:px-6">
              {graphTools.map((tool) => (
                <li key={tool.name} className="grid grid-cols-1 gap-1.5 py-4 sm:grid-cols-[11rem_minmax(0,1fr)] sm:gap-6">
                  <p className="font-semibold text-white">{tool.name}</p>
                  <div className="min-w-0">
                    <p className="text-[14.5px] leading-snug text-slate-300">{tool.answers}</p>
                    <p className="mt-1.5 font-mono text-[11.5px] break-words text-emerald-200/80">{tool.command}</p>
                  </div>
                </li>
              ))}
            </ul>
          </Section>
        </ChapterBlock>

        {/* ─── 06 · Proof ─────────────────────────────────────────────── */}
        <ChapterBlock chapter={chapter("proof")}>
          <Section
            id="compare"
            index={index("proof", "compare")}
            layout="wide"
            eyebrow="Against the alternatives"
            title="Not another [vector store]."
            kicker={
              <>
                A vector database finds text that looks similar. A notes file is whatever someone last remembered to write.
                ee is the layer between: it knows what kind of thing each memory is, how much to trust it, how old it is,
                and why it belongs in this pack.
              </>
            }
          >
            <ComparisonTable />
          </Section>

          <Section
            id="degrade"
            index={index("proof", "degrade")}
            layout="wide"
            eyebrow="Graceful degradation"
            title="Pull a plug. It still [works]."
            kicker={
              <>
                Every optional part of ee can be missing. When one is, the command still answers, says what it lost in a
                machine-readable <Cmd>degraded</Cmd> entry, and names the repair. Try it.
              </>
            }
          >
            <DegradationBoard />
          </Section>

          <Section
            id="rigor"
            index={index("proof", "rigor")}
            eyebrow="Hard requirements"
            title="Shortcuts are [forbidden]."
            kicker={
              <>
                Each line below is a dependency the build refuses, with what ee uses instead. They are why ee runs as one
                binary with no async runtime it does not own, no database it did not choose, and no network in the core.
              </>
            }
          >
            <ul className="space-y-3">
              {forbidden.map((rule, at) => (
                <Reveal key={rule.no} delay={at * 0.04}>
                  <li className="panel grid grid-cols-1 gap-3 rounded-2xl p-5 sm:grid-cols-[minmax(0,1fr)_auto_minmax(0,1fr)] sm:items-center sm:gap-5">
                    <p className="font-mono text-[13px] text-rose-300/90 line-through decoration-rose-400/50">{rule.no}</p>
                    <ArrowRight className="hidden h-4 w-4 text-slate-600 sm:block" aria-hidden="true" />
                    <div className="min-w-0">
                      <p className="font-semibold text-white">{rule.instead}</p>
                      <p className="mt-0.5 text-[13px] leading-snug text-slate-400">{rule.why}</p>
                    </div>
                  </li>
                </Reveal>
              ))}
            </ul>

            <dl className="mt-8 grid grid-cols-2 gap-px overflow-hidden rounded-2xl border border-white/[0.07] bg-white/[0.06] lg:grid-cols-4">
              {rigor.map((item) => (
                <div key={item.label} className="bg-[#08061a] p-5">
                  <dt className="font-mono text-[10px] tracking-[0.16em] text-fuchsia-200/80 uppercase">{item.label}</dt>
                  <dd>
                    <span className="mt-2 block text-3xl font-extrabold tracking-[-0.04em] text-white tabular-nums">{item.value}</span>
                    <span className="mt-1.5 block text-[12.5px] leading-snug text-slate-400">{item.note}</span>
                  </dd>
                </div>
              ))}
            </dl>

            <div className="mt-8">
              <MicroLabel className="text-slate-400">Built on the FrankenSuite</MicroLabel>
              <ul className="mt-4 grid grid-cols-1 gap-2.5 sm:grid-cols-2 xl:grid-cols-3">
                {stack.map((item) => (
                  <li key={item.name} className="panel rounded-xl px-4 py-3">
                    <p className="flex items-baseline justify-between gap-3">
                      <span className="font-semibold text-white">{item.name}</span>
                      <span className="font-mono text-[10px] tracking-[0.16em] text-slate-500 uppercase">{item.role}</span>
                    </p>
                    <p className="mt-1 text-[13px] leading-snug text-slate-400">{item.text}</p>
                  </li>
                ))}
              </ul>
            </div>

            <div className="mt-10">
              <MicroLabel className="text-slate-400">Releases</MicroLabel>
              <ol className="relative mt-5 space-y-0 border-l border-white/[0.08] pl-6">
                {releases.map((release, at) => (
                  <li key={release.version} className="relative pb-5 last:pb-0">
                    <span
                      aria-hidden="true"
                      className={cn(
                        "absolute top-1.5 -left-[1.82rem] h-2.5 w-2.5 rounded-full border-2 border-[#05040d]",
                        at === releases.length - 1 ? "bg-fuchsia-300 shadow-[0_0_12px_rgba(232,121,249,0.8)]" : "bg-slate-600",
                      )}
                    />
                    <p className="flex flex-wrap items-baseline gap-x-3">
                      <span className="font-mono text-[14px] font-semibold text-white">v{release.version}</span>
                      <span className="font-mono text-[11px] text-slate-500 tabular-nums">{release.date}</span>
                    </p>
                    <p className="mt-0.5 text-[14px] leading-snug text-slate-400">{release.what}</p>
                  </li>
                ))}
              </ol>
              <p className="mt-4 text-[13px] text-slate-500">
                From the tags and{" "}
                <a className={link} href={siteConfig.changelog}>
                  CHANGELOG.md
                </a>
                .
              </p>
            </div>
          </Section>

          <Section
            id="status"
            index={index("proof", "status")}
            eyebrow="What is not done"
            title="Honest [status]."
            kicker={
              <>
                A memory you cannot trust is worse than none, and the same goes for a project page. These are the limits
                as the README states them today.
              </>
            }
          >
            <ul className="grid grid-cols-1 gap-3 sm:grid-cols-2">
              {limitations.map((item, at) => (
                <Reveal key={item.title} delay={at * 0.04}>
                  <li className="panel h-full rounded-2xl p-5">
                    <p className="flex items-center gap-2.5">
                      <Minus className="h-4 w-4 shrink-0 text-amber-300" aria-hidden="true" />
                      <span className="font-semibold text-white">{item.title}</span>
                    </p>
                    <p className="mt-2 text-[13.5px] leading-relaxed text-slate-400">{item.text}</p>
                  </li>
                </Reveal>
              ))}
            </ul>
          </Section>

          <Section
            id="install"
            index={index("proof", "install")}
            eyebrow={`${siteConfig.version} · ${siteConfig.released}`}
            title="Running in [one minute]."
            kicker={
              <>
                Install the binary, open a workspace, and let your harness call it. The installer verifies the release
                checksum, adds shell completions, runs a self-test and tells you which agent harnesses it found; it does
                not touch their settings.
              </>
            }
          >
            <div className="space-y-3">
              {install.map((item) => (
                <CopyCommand key={item.comment} comment={item.comment} command={item.command} />
              ))}
            </div>
            <div className="mt-8">
              <MicroLabel className="text-slate-400">Then wire it into your harness</MicroLabel>
              <ul className="mt-4 grid grid-cols-1 gap-3 sm:grid-cols-2">
                {harnesses.map((harness) => (
                  <li key={harness.name} className="panel lift rounded-2xl p-5">
                    <p className="font-semibold text-white">{harness.name}</p>
                    <p className="mt-2 font-mono text-[11.5px] break-words text-fuchsia-100/90">{harness.command}</p>
                    <p className="mt-2.5 text-[13px] leading-relaxed text-slate-400">{harness.note}</p>
                  </li>
                ))}
              </ul>
            </div>
          </Section>
        </ChapterBlock>
      </main>

      <footer data-outside className="dock-clearance relative isolate overflow-hidden border-t border-white/[0.06]">
        <div aria-hidden="true" className="field-light absolute inset-0 -z-10" />
        <div className="mx-auto max-w-7xl px-4 pt-24 pb-14 sm:px-6 md:pt-32">
          <div className="grid grid-cols-1 gap-12 lg:grid-cols-12">
            <div className="lg:col-span-7">
              <p className="display-2 font-extrabold text-white">
                <RecallTitle text="Stop re-teaching your [agents]." />
              </p>
              <p className="mt-6 max-w-2xl text-[15px] leading-relaxed text-slate-400">
                ee is open source, MIT-licensed with an OpenAI/Anthropic rider, and built in the open: every decision
                record, schema and failure mode is in the repository.
              </p>
              <div className="mt-8 flex flex-wrap gap-3">
                <a
                  href="#install"
                  className="inline-flex min-h-12 items-center gap-2 rounded-2xl bg-violet-400 px-5 text-[15px] font-extrabold text-black transition-colors hover:bg-violet-300"
                >
                  Install ee <ArrowRight className="h-4 w-4" aria-hidden="true" />
                </a>
                <a
                  href={siteConfig.github}
                  className="inline-flex min-h-12 items-center gap-2 rounded-2xl border border-white/15 bg-white/[0.02] px-5 text-[15px] font-bold text-white transition-colors hover:border-violet-400/50 hover:text-violet-200"
                >
                  Open the repository <ExternalLink className="h-4 w-4" aria-hidden="true" />
                </a>
              </div>
            </div>
            <nav aria-label="Project" className="lg:col-span-5">
              <ul className="grid grid-cols-1 gap-2.5 text-sm min-[420px]:grid-cols-2">
                <FooterLink href={siteConfig.github} label="Repository" />
                <FooterLink href={siteConfig.readme} label="README" />
                <FooterLink href={siteConfig.changelog} label="Changelog" />
                <FooterLink href={siteConfig.adrs} label="Decision records" />
                <FooterLink href={siteConfig.schemas} label="JSON schemas" />
                <FooterLink href={siteConfig.trustModel} label="Trust model" />
                <FooterLink href={siteConfig.crate} label="crates.io" />
                <FooterLink href={siteConfig.license} label="License (MIT + rider)" />
              </ul>
            </nav>
          </div>
          <p className="mt-16 flex flex-wrap items-center gap-x-6 gap-y-2 border-t border-white/[0.06] pt-6 font-mono text-[11px] text-slate-500">
            <span>{siteConfig.name} · ee</span>
            <span>Every figure on this page names its source</span>
            <span>
              Counted at {COUNTED.commit}, {COUNTED.date}
            </span>
          </p>
        </div>
      </footer>
    </>
  );
}

function ComparisonTable() {
  const cell = (value: string, first: boolean): ReactNode => {
    if (value === "yes") {
      return (
        <span className={cn("inline-flex items-center gap-1.5", first ? "font-semibold text-fuchsia-100" : "text-slate-300")}>
          <Check className={cn("h-4 w-4", first ? "text-fuchsia-300" : "text-slate-400")} aria-hidden="true" />
          yes
        </span>
      );
    }
    if (value === "no") {
      return (
        <span className="inline-flex items-center gap-1.5 text-slate-500">
          <Minus className="h-4 w-4" aria-hidden="true" />
          no
        </span>
      );
    }
    return <span className="text-slate-400">{value}</span>;
  };
  return (
    <figure className="panel relative overflow-hidden rounded-[1.75rem]">
      <p className="border-b border-white/[0.06] px-5 py-2.5 font-mono text-[10px] tracking-[0.18em] text-violet-300/80 uppercase md:hidden">Swipe the table for the other three →</p>
      <div className="overflow-x-auto [scrollbar-width:thin]">
        <table className="w-full min-w-[44rem] border-collapse text-left text-[13.5px]">
          <caption className="sr-only">ee compared with a vector database, an MCP memory server and plain notes</caption>
          <thead>
            <tr className="border-b border-white/[0.08]">
              <th scope="col" className="px-5 py-4 font-mono text-[10px] font-medium tracking-[0.18em] text-slate-500 uppercase sm:px-6">
                Feature
              </th>
              {comparison.columns.map((column, at) => (
                <th
                  key={column}
                  scope="col"
                  className={cn(
                    "px-4 py-4 text-[13px] font-bold whitespace-nowrap",
                    at === 0 ? "bg-fuchsia-400/[0.07] text-fuchsia-200" : "text-slate-300",
                  )}
                >
                  {column}
                </th>
              ))}
            </tr>
          </thead>
          <tbody>
            {comparison.rows.map((row) => (
              <tr key={row.feature} className="border-b border-white/[0.05] last:border-b-0">
                <th scope="row" className="px-5 py-3 font-medium text-slate-200 sm:px-6">
                  {row.feature}
                </th>
                {row.cells.map((value, at) => (
                  <td key={comparison.columns[at]} className={cn("px-4 py-3 whitespace-nowrap", at === 0 && "bg-fuchsia-400/[0.07]")}>
                    {cell(value, at === 0)}
                  </td>
                ))}
              </tr>
            ))}
          </tbody>
        </table>
      </div>
      <figcaption className="border-t border-white/[0.06] px-5 py-3.5 font-mono text-[11px] text-slate-500 sm:px-6">
        README, &ldquo;Comparison&rdquo;. &ldquo;Graph analytics&rdquo; excludes personalized PageRank, which is pending upstream.
      </figcaption>
    </figure>
  );
}

function FooterLink({ href, label }: { href: string; label: string }) {
  return (
    <li>
      <a
        href={href}
        className="flex min-h-12 items-center justify-between gap-2 rounded-xl border border-white/[0.08] bg-white/[0.015] px-4 text-slate-300 transition-colors hover:border-violet-400/40 hover:text-violet-200"
      >
        {label} <ExternalLink className="h-3.5 w-3.5" aria-hidden="true" />
      </a>
    </li>
  );
}
