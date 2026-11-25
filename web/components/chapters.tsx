// The site's eight chapters. The home page reads them in order; each one also has a page of its
// own (app/[chapter]/page.tsx), with its own share images, so a reader can send one chapter on.

import { ArrowRight, Check, Minus } from "lucide-react";
import type { CSSProperties, ReactNode } from "react";

import { CopyCommand } from "@/components/copy-command";
import { RecoveryExplorer } from "@/components/explain/agent-contract";
import { Amnesia } from "@/components/explain/amnesia";
import { CampaignTimeline } from "@/components/explain/campaign";
import { DecayCurves } from "@/components/explain/decay";
import { DegradationBoard } from "@/components/explain/degradation";
import { GraphLens } from "@/components/explain/graph-lens";
import { HashPrefix } from "@/components/explain/hash-prefix";
import { IncidentCard } from "@/components/explain/incident-card";
import { NegativeLedger } from "@/components/explain/ledger";
import { MemoryLoop } from "@/components/explain/loop";
import { PackBuilder } from "@/components/explain/pack-builder";
import { PackPipeline } from "@/components/explain/pipeline";
import { MarkdownVsMemory } from "@/components/explain/scale";
import { ScaleLadder } from "@/components/explain/scale-lanes";
import { SessionLoop } from "@/components/explain/session-loop";
import { TrustPosterior } from "@/components/explain/trust";
import { Cmd, MicroLabel } from "@/components/instrument";
import { Reveal } from "@/components/reveal";
import { ChapterBlock, Section } from "@/components/section";
import {
  agentSurfaces,
  chapterById,
  comparison,
  curationSteps,
  domains,
  exitCodes,
  firstCommands,
  forbidden,
  graphTools,
  guards,
  harnesses,
  install,
  limitations,
  memoryKinds,
  memoryLevels,
  postures,
  releases,
  rigor,
  sectionIndex,
  siteConfig,
  sixQuestions,
  stack,
} from "@/lib/content";
import { LEVEL_COLOUR, type Level } from "@/lib/palette";
import { cn } from "@/lib/utils";

const link = "text-violet-300 underline decoration-violet-300/30 underline-offset-4 transition-colors hover:decoration-violet-300";

/** How a chapter is being read: as one of eight on the home page, or alone on its own page. */
export type ChapterMode = "home" | "page";

/** One chapter, opener and sections, by its id. */
export function ChapterContent({ id, mode = "home" }: { id: string; mode?: ChapterMode }) {
  const chapter = chapterById(id);
  const body = BODIES[id];
  if (!body) throw new Error(`no body for chapter ${id}`);
  return (
    <ChapterBlock chapter={chapter} mode={mode}>
      {body()}
    </ChapterBlock>
  );
}

const BODIES: Record<string, () => ReactNode> = {
  forget: Amnesiac,
  memory: MemoryChapter,
  pack: PackChapter,
  learn: LearningChapter,
  campaigns: CampaignsChapter,
  agents: AgentsChapter,
  graph: GraphChapter,
  proof: ProofChapter,
};

function Amnesiac() {
  return (
    <>
      <Section
        id="amnesia"
        index={sectionIndex("forget", "amnesia")}
        layout="wide"
        eyebrow="Six sessions, two agents"
        title="Without memory, session six repeats session two's [mistake]."
        kicker={
          <>
            The harness gives every session a fresh context window. With nowhere durable to put what it learned, an agent
            spends the start of each session re-learning the project and the middle of it working through problems an
            earlier session already solved.
          </>
        }
      >
        <Amnesia />
      </Section>

      <Section
        id="scale"
        index={sectionIndex("forget", "scale")}
        layout="wide"
        eyebrow="CLAUDE.md, AGENTS.md, notes.md"
        title="What happens as the notes file [grows]."
        kicker={
          <>
            Most agent memory today is a markdown file that the harness loads whole at the start of every session. It
            grows with every lesson, nothing in it marks a line as stale, and every line costs tokens in every session
            whether the task needs it or not. A vector store keeps the size down by returning only the closest matches,
            but on its own it ranks by similarity and does not track whether a chunk is stale or repeated. Move through
            the weeks to compare all three.
          </>
        }
      >
        <MarkdownVsMemory />
      </Section>

      <Section
        id="layer"
        index={sectionIndex("forget", "layer")}
        layout="wide"
        eyebrow="Where ee sits"
        title="ee is a command the harness [calls]."
        kicker={
          <>
            Claude Code, Codex or a shell script keeps its own tools, approvals and prompt loop. Agents call ee to record
            evidence and to fetch context, through one CLI with a stable JSON contract.
          </>
        }
      >
        <MemoryLoop />
      </Section>

      <Section
        id="first"
        index={sectionIndex("forget", "first")}
        eyebrow="Five commands"
        title="The basic loop in five [commands]."
        kicker={
          <>
            Open a workspace, record a rule, pack it into the next session, report whether it helped, and ask why it was
            chosen. The later chapters show what each of these steps does in detail.
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
                  <pre className="mt-3 overflow-x-auto rounded-xl border border-white/[0.06] bg-black/40 px-4 py-3 text-[12.5px] leading-relaxed text-violet-100 [scrollbar-width:thin] max-sm:whitespace-pre-wrap">
                    <code>{item.command}</code>
                  </pre>
                  <p className="mt-3 text-[14px] leading-relaxed text-slate-400">{item.note}</p>
                </div>
              </li>
            </Reveal>
          ))}
        </ol>
      </Section>
    </>
  );
}

function MemoryChapter() {
  return (
    <>
      <Section
        id="decay"
        index={sectionIndex("memory", "decay")}
        layout="wide"
        eyebrow="Decay and demotion"
        title="How fast each level [fades]."
        kicker={
          <>
            A memory&apos;s freshness halves on a schedule set by what it is: a day for working notes, a year for a
            procedural rule. When the decay pass runs (<Cmd>ee maintenance run --include-decay</Cmd>), freshness times
            confidence times utility decides whether it is demoted or tombstoned, with an audit entry either way.
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
        index={sectionIndex("memory", "kinds")}
        eyebrow="Typed fields"
        title="Fields a search can [filter on]."
        kicker={
          <>
            A failure records its cause and the commit that reverted it; a decision records its options and when to
            revisit it. Search filters on those fields by name, so an agent can ask for{" "}
            <Cmd>--kind failure --field family^rustc</Cmd> instead of relying on matching words. A memory written as
            plain prose gets no typed fields; ee does not invent them.
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
          Every memory also carries its level, kind, content hash, tags, confidence, utility, importance, access count,
          source, evidence spans, links and trust class. README, &ldquo;Memory Model&rdquo;.
        </p>
      </Section>
    </>
  );
}

function PackChapter() {
  return (
    <>
      <Section
        id="pipeline"
        index={sectionIndex("pack", "pipeline")}
        layout="wide"
        eyebrow="ee pack, stage by stage"
        title="A pipeline of timed [stages]."
        kicker={
          <>
            <Cmd>ee pack</Cmd> runs a fixed pipeline. It validates the request, retrieves with lexical and semantic
            search fused by rank, resolves and admits candidates, selects under quotas, and assembles the pack with
            provenance. It reports every degradation, and <Cmd>--explain-performance</Cmd> adds each stage&apos;s time.
          </>
        }
      >
        <PackPipeline />
      </Section>

      <Section
        id="builder"
        index={sectionIndex("pack", "builder")}
        layout="wide"
        eyebrow="ee's selection rule, on your screen"
        title="Build a pack [yourself]."
        kicker={
          <>
            Change the profile and the budget to see the pack change. Each candidate shows why it is in or out: its
            marginal gain, the memory it duplicates, or the quota or budget it hit. With the redundancy penalty switched
            off, near-duplicates fill the budget and the failure the task needed drops out.
          </>
        }
      >
        <PackBuilder />
      </Section>

      <Section
        id="explain"
        index={sectionIndex("pack", "explain")}
        eyebrow="Explainable retrieval"
        title="Six questions every item [answers]."
        kicker={
          <>
            To weigh a memory, an agent needs to know why it was returned. Every returned memory carries its answers to
            six questions, and <Cmd>ee why</Cmd> and <Cmd>ee pack --explain</Cmd> show the full detail.
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
          <MicroLabel className="text-slate-400">What the scores do not mean</MicroLabel>
          <p className="mt-3 text-[15px] leading-relaxed text-slate-300">
            Relevance scores rank results; they are not probabilities that a result answers the task. When results
            exist, ee reports retrieval quality as <Cmd>unknown</Cmd> rather than inventing a number, because score
            size, result count and interval coverage do not establish that an answer is correct. Judge a memory by its
            content and its evidence.
          </p>
          <p className="mt-3 font-mono text-[11px] text-slate-500">README, &ldquo;Explainable Retrieval&rdquo;</p>
        </div>
      </Section>

      <Section
        id="hash"
        index={sectionIndex("pack", "hash")}
        layout="wide"
        eyebrow="Deterministic by default"
        title="A pack hash that [reproduces]."
        kicker={
          <>
            Given the same snapshots, config, profile, budget and query, ties break the same way and the pack hash
            reproduces, so a pack can be cited, compared and replayed. The first version of the hash had a collision,
            which the figure below lets you recreate.
          </>
        }
      >
        <HashPrefix />
      </Section>
    </>
  );
}

function LearningChapter() {
  return (
    <>
      <Section
        id="trust"
        index={sectionIndex("learn", "trust")}
        layout="wide"
        eyebrow="Bayesian trust classes"
        title="A posterior for every [memory]."
        kicker={
          <>
            A memory&apos;s trust class comes from how it was written: <Cmd>human_explicit</Cmd> for an ordinary{" "}
            <Cmd>ee remember</Cmd>, <Cmd>agent_assertion</Cmd> for a registered agent&apos;s attempt-family write,{" "}
            <Cmd>cass_evidence</Cmd> for a line imported from a session. Outcomes update a Beta posterior that starts
            from the Jeffreys prior, and the class changes only when the 90% interval crosses a threshold. Use the
            buttons to record outcomes.
          </>
        }
      >
        <TrustPosterior />
      </Section>

      <Section
        id="fixes"
        index={sectionIndex("learn", "fixes")}
        layout="wide"
        eyebrow="Failure-to-fix recall"
        title="Fixes from past [sessions]."
        kicker={
          <>
            ee imports your past agent sessions through cass, finds the failures that were fixed and verified, and keeps
            each as a short incident card keyed by the error&apos;s fingerprint. When that error class appears again,{" "}
            <Cmd>ee diagnose-error</Cmd> or <Cmd>ee pack --error-log</Cmd> returns the fix with its provenance.
          </>
        }
      >
        <IncidentCard />
      </Section>

      <Section
        id="curate"
        index={sectionIndex("learn", "curate")}
        eyebrow="No silent memory mutation"
        title="Proposed changes go through [review]."
        kicker={
          <>
            Changes ee proposes start as candidates that can be reviewed, and applying one writes an audit entry. A
            person can also tombstone a memory directly, and the decay pass demotes and tombstones on its own when run
            with <Cmd>--include-decay</Cmd>; both are audited. Turning a session journal into candidates does not use
            an LLM.
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
                  <pre className="mt-3 overflow-x-auto font-mono text-[12px] text-sky-100/90 [scrollbar-width:thin] max-sm:whitespace-pre-wrap">
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
        index={sectionIndex("learn", "guard")}
        eyebrow="Privacy and trust"
        title="Checks on the way in and [out]."
        kicker={
          <>
            Anything written to the store can reach every later session. ee refuses to store a memory that contains a
            secret, screens stored memories for injected instructions before it packs them, and only advises on
            commands: no ee check can stop the agent from running one.{" "}
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
        <pre className="panel mt-4 overflow-x-auto rounded-2xl p-5 font-mono text-[12px] leading-relaxed text-slate-300 [scrollbar-width:thin] max-sm:break-all max-sm:whitespace-pre-wrap">
          <code>
            <span className="text-slate-500">$ </span>ee remember &quot;DATABASE_URL=postgres://user:hunter2@host/db&quot;
            {"\n"}
            <span className="text-slate-500"># refused: exit 7, </span>
            <span className="text-sky-200">policy_denied</span>
            <span className="text-slate-500">, detectedPattern </span>
            <span className="text-sky-200">database_url</span>
            <span className="text-slate-500">; nothing is written</span>
          </code>
        </pre>
      </Section>
    </>
  );
}

function CampaignsChapter() {
  return (
    <>
      <Section
        id="campaign"
        index={sectionIndex("campaigns", "campaign")}
        layout="wide"
        eyebrow="A simulated campaign"
        title="Twenty-six weeks of one [campaign]."
        kicker={
          <>
            Drag through half a year of one campaign. Working notes expire, failures accumulate and are distilled into
            rules, decisions come up for review, and a rule that starts misleading is demoted. For any day, the figure
            also shows the pack the next session would start with.
          </>
        }
      >
        <CampaignTimeline />
      </Section>

      <Section
        id="ledger"
        index={sectionIndex("campaigns", "ledger")}
        layout="wide"
        eyebrow="Negative evidence"
        title="A ledger of failed [attempts]."
        kicker={
          <>
            In a long optimization or research campaign most attempts fail, and each one costs time. A failure recorded
            with its family and cause warns later sessions before they repeat it, and after review, repeated failures in
            one family can become an anti-pattern rule for that family.
          </>
        }
      >
        <NegativeLedger />
      </Section>

      <Section
        id="domains"
        index={sectionIndex("campaigns", "domains")}
        eyebrow="Beyond code"
        title="Any work with sources and [decisions]."
        kicker={
          <>
            Nothing in the memory model is specific to software. Where the work has durable facts, recurring decisions
            and sources worth citing, the same loop applies: one workspace per matter, position or incident, and every
            memory pointing back at its source. Privileged work can opt out of the shared lane entirely.
          </>
        }
      >
        <ul className="grid grid-cols-1 gap-3 sm:grid-cols-2">
          {domains.map((domain, at) => (
            <Reveal key={domain.name} delay={at * 0.04}>
              <li className="panel lift relative h-full overflow-hidden rounded-2xl p-5">
                <span
                  aria-hidden="true"
                  className="absolute -top-10 -right-10 h-28 w-28 rounded-full opacity-20 blur-2xl"
                  style={{ background: "var(--chapter)" }}
                />
                <p className="flex items-baseline justify-between gap-3">
                  <span className="text-lg font-bold tracking-tight text-white">{domain.name}</span>
                  <span className="shrink-0 font-mono text-[10px] tracking-[0.16em] text-orange-200/70 uppercase">per {domain.unit}</span>
                </p>
                <p className="mt-2 text-[14px] leading-relaxed text-slate-400">{domain.memories}</p>
                <p className="mt-3 font-mono text-[11.5px] break-all text-orange-100/80">
                  <span className="text-slate-500">$ </span>ee init --workspace {domain.workspace}
                </p>
                <ul className="mt-3 flex flex-wrap gap-1.5">
                  {domain.uris.map((uri) => (
                    <li
                      key={uri}
                      className="rounded-md border border-[color:var(--chapter)]/25 bg-[color:var(--chapter)]/[0.07] px-2 py-0.5 font-mono text-[11px] text-orange-100"
                    >
                      {uri}…
                    </li>
                  ))}
                </ul>
              </li>
            </Reveal>
          ))}
          <Reveal delay={0.3}>
            <li className="panel h-full rounded-2xl border-dashed p-5">
              <MicroLabel className="text-slate-400">Privileged work</MicroLabel>
              <p className="mt-3 text-[14px] leading-relaxed text-slate-300">
                A matter-confidential or patient-adjacent workspace should neither read shared memories nor leak its own
                into them. One setting takes it out of the user-global lane; retrieval then says so in{" "}
                <Cmd>degraded</Cmd> instead of silently narrowing.
              </p>
              <p className="mt-3 font-mono text-[11.5px] break-all text-orange-100/80">
                <span className="text-slate-500">$ </span>ee config set memory.participate false
              </p>
            </li>
          </Reveal>
        </ul>
        <p className="mt-5 text-[13px] leading-relaxed text-slate-500">
          README, &ldquo;Beyond Coding&rdquo;. The URIs are what you pass to <Cmd>ee remember --source</Cmd>; ee records
          them as provenance and does not fetch them. Session import through cass is coding-specific; elsewhere, use{" "}
          <Cmd>ee remember</Cmd> or <Cmd>ee import jsonl</Cmd>.
        </p>
      </Section>

      <Section
        id="team"
        index={sectionIndex("campaigns", "team")}
        layout="wide"
        eyebrow="Scale"
        title="From one agent to a [team]."
        kicker={
          <>
            One store serves one agent on one repository or a swarm of agents working at once. A small team can link
            their stores to share what each member learns, and a user-global lane follows you across every workspace you
            open. When two lanes disagree, ee records the conflict.
          </>
        }
      >
        <ScaleLadder />
      </Section>
    </>
  );
}

function AgentsChapter() {
  const tone = (code: number) =>
    code === 0 ? "bg-lime-300 text-black" : code === 7 || code === 10 ? "bg-amber-300/90 text-black" : code === 130 ? "bg-slate-300 text-black" : "bg-white/[0.08] text-lime-100";
  return (
    <>
      <Section
        id="session"
        index={sectionIndex("agents", "session")}
        layout="wide"
        eyebrow="The agent's loop"
        title="A session, the way an agent [runs] it."
        kicker={
          <>
            Orient without changing anything, pull a pack and read what it left out, check the code you are about to
            touch, do the work, write down what you learned, say what helped, and leave a capsule for whoever comes next.
            Every step but the work itself is one ee command whose answer is JSON, with named fields the next step reads.
          </>
        }
      >
        <SessionLoop />
      </Section>

      <Section
        id="recovery"
        index={sectionIndex("agents", "recovery")}
        layout="wide"
        eyebrow="ee.error.v2"
        title="Errors that say what to do [next]."
        kicker={
          <>
            An error message written for a person leaves an agent guessing. An ee error states what broke and how serious
            it is, and where ee knows the repair, it names the command, whether it is safe to run unattended, and whether
            a person has to approve it first.
          </>
        }
      >
        <RecoveryExplorer />
      </Section>

      <Section
        id="discover"
        index={sectionIndex("agents", "discover")}
        eyebrow="Self-documenting"
        title="It documents itself to the [agent]."
        kicker={
          <>
            An agent can learn ee without a person explaining it. The documentation, the command tree and the schemas are
            all commands that return JSON, and every outcome has an exit code the agent can branch on.
          </>
        }
      >
        <ul className="ruled panel rounded-2xl px-5 sm:px-6">
          {agentSurfaces.map((surface) => (
            <li key={surface.command} className="grid grid-cols-1 gap-1.5 py-4 sm:grid-cols-[13rem_minmax(0,1fr)] sm:gap-6">
              <p className="font-mono text-[13px] font-medium break-words text-lime-200">{surface.command}</p>
              <p className="text-[14.5px] leading-snug text-slate-300">{surface.answers}</p>
            </li>
          ))}
        </ul>

        <div className="panel mt-4 rounded-2xl p-5 sm:p-6">
          <MicroLabel className="text-slate-400">Exit codes · one per class of failure</MicroLabel>
          <ol className="mt-4 grid grid-cols-2 gap-1.5 sm:grid-cols-3 lg:grid-cols-4">
            {exitCodes.map((item) => (
              <li key={item.code} className="flex min-w-0 items-stretch overflow-hidden rounded-xl border border-white/[0.06] bg-black/30">
                <span className={cn("flex w-11 shrink-0 items-center justify-center font-mono text-[13px] font-bold tabular-nums", tone(item.code))}>
                  {item.code}
                </span>
                <span className="min-w-0 px-2.5 py-2">
                  <span className="block text-[12.5px] leading-tight font-semibold text-white">{item.name}</span>
                  <span className="mt-0.5 block font-mono text-[10.5px] leading-tight text-slate-500">{item.act}</span>
                </span>
              </li>
            ))}
          </ol>
          <p className="mt-4 font-mono text-[11px] text-slate-500">ProcessExitCode, src/models/mod.rs · stdout carries JSON, stderr carries diagnostics</p>
        </div>

        <div className="panel mt-4 rounded-2xl p-5 sm:p-6">
          <MicroLabel className="text-slate-400">Five postures</MicroLabel>
          <p className="mt-3 text-[14.5px] leading-relaxed text-slate-300">
            <Cmd>ee status</Cmd> reports one of five postures, so an agent can tell a store that is still warming up
            from one that needs a repair and one it must not touch. <Cmd>ee doctor</Cmd> uses three of them: ok,
            degraded_recoverable and blocked.
          </p>
          <ol className="mt-4 flex flex-wrap items-center gap-1.5 font-mono text-[11.5px]">
            {postures.map((posture, at) => (
              <li key={posture} className="flex items-center gap-1.5">
                <span
                  className="rounded-lg border px-2.5 py-1"
                  style={{
                    borderColor: `color-mix(in oklab, ${["#a3e635", "#67e8f9", "#fbbf24", "#fb923c", "#fb7185"][at]} 45%, transparent)`,
                    color: ["#d9f99d", "#a5f3fc", "#fde68a", "#fed7aa", "#fecdd3"][at],
                  }}
                >
                  {posture}
                </span>
                {at < postures.length - 1 ? <ArrowRight className="h-3 w-3 text-slate-600" aria-hidden="true" /> : null}
              </li>
            ))}
          </ol>
          <p className="mt-3 font-mono text-[11px] text-slate-500">src/models/posture.rs</p>
        </div>
      </Section>
    </>
  );
}

function GraphChapter() {
  return (
    <>
      <Section
        id="lenses"
        index={sectionIndex("graph", "lenses")}
        layout="wide"
        eyebrow="Graph lenses"
        title="What the links [show]."
        kicker={
          <>
            Similarity search finds memories whose text looks like the task. The links find others that text similarity
            misses, such as the rule twelve decisions rest on or the only fact connecting two topics. Tap a lens, then a
            memory.
          </>
        }
      >
        <GraphLens />
      </Section>

      <Section
        id="toolbox"
        index={sectionIndex("graph", "toolbox")}
        eyebrow="FrankenNetworkX, in the CLI"
        title="Each algorithm answers a [question]."
        kicker={
          <>
            Each one is a command with deterministic JSON output. The first six work as installed; the rest name the
            config flag that turns them on, because every graph feature flag is off by default. Graph views help
            explain and navigate memory, but a memory&apos;s own provenance stays the record of where it came from.
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
                {tool.flag ? (
                  <p className="mt-1 font-mono text-[11px] break-words text-slate-500">
                    needs ee config set {tool.flag} true
                  </p>
                ) : null}
              </div>
            </li>
          ))}
        </ul>
      </Section>
    </>
  );
}

function ProofChapter() {
  return (
    <>
      <Section
        id="compare"
        index={sectionIndex("proof", "compare")}
        layout="wide"
        eyebrow="Comparison"
        title="ee beside the [alternatives]."
        kicker={
          <>
            A vector database returns text that looks similar to the query. A notes file holds whatever someone last
            wrote down. ee also records what kind of thing each memory is, how far to trust it, how old it is, and why it
            was put in this pack.
          </>
        }
      >
        <ComparisonTable />
      </Section>

      <Section
        id="degrade"
        index={sectionIndex("proof", "degrade")}
        layout="wide"
        eyebrow="Graceful degradation"
        title="Commands still answer with parts [missing]."
        kicker={
          <>
            Any optional part of ee can be missing. Search, packs and the graph still answer without one, and report what
            they lost as a machine-readable entry: a code, a severity and, where ee has one, a repair. An import from
            cass is the exception: without cass it fails, with the same entry inside its error. Switch parts off below
            to see what each command reports.
          </>
        }
      >
        <DegradationBoard />
      </Section>

      <Section
        id="rigor"
        index={sectionIndex("proof", "rigor")}
        eyebrow="Hard requirements"
        title="Dependencies the checks [refuse]."
        kicker={
          <>
            Each line below is a dependency that fails ee&apos;s forbidden-dependency check, and what ee uses instead.
            Together they keep ee a single binary with its own runtime and storage stack and no HTTP code in the core.
            Hybrid search comes from one crate, Frankensearch, by convention; no check enforces that one.
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
        index={sectionIndex("proof", "status")}
        eyebrow="Limitations"
        title="What is not [done yet]."
        kicker={<>Drawn from the README, the ADRs and the CHANGELOG, as of {siteConfig.version}.</>}
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
        index={sectionIndex("proof", "install")}
        eyebrow={`${siteConfig.version} · ${siteConfig.released}`}
        title="Install it and connect your [harness]."
        kicker={
          <>
            Install the binary, open a workspace, and let your harness call it. The installer verifies the release
            checksum, adds shell completions, runs a self-test and tells you which agent harnesses it found; it does not
            touch their settings.
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
      <p className="border-b border-white/[0.06] px-5 py-2.5 font-mono text-[10px] tracking-[0.18em] text-violet-300/80 uppercase md:hidden">
        Swipe the table for the other three →
      </p>
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
                  className={cn("px-4 py-4 text-[13px] font-bold whitespace-nowrap", at === 0 ? "bg-fuchsia-400/[0.07] text-fuchsia-200" : "text-slate-300")}
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
