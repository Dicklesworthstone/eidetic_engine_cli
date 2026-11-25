// A campaign, compounding: the model behind components/explain/campaign.tsx.
//
// A toy. Five invented campaigns of 26 weeks each, simulated day by day with a
// seeded generator so the server and every browser draw the same picture. What
// is ee's is the arithmetic the toy borrows, each constant cited where it is
// declared:
//   - half-lives and the decay thresholds: src/policy/memory_decay.rs
//     (MemoryDecayHalfLives::default, DEFAULT_DECAY_DEMOTE/FORGET_THRESHOLD);
//   - the pack budget and the section quotas: src/pack/mod.rs
//     (DEFAULT_CONTEXT_MAX_TOKENS) and src/models/context_profile.rs (balanced);
//   - the trust classes: src/models/trust.rs.
// Everything else (how many notes a session writes, which memories get
// recalled, the events, the pack's contents) is invented for the page, and the
// pack here is ranked by trust and recency, not by ee's MMR selection.

import type { Level, PackSection } from "@/lib/palette";
import { seeded } from "@/lib/utils";

export const WEEKS = 26;
export const DAYS = WEEKS * 7;

/** DEFAULT_CONTEXT_MAX_TOKENS (src/pack/mod.rs): what `ee pack` spends when no --max-tokens is given. */
export const BUDGET = 4000;

/** The balanced profile's section mix, in basis points (src/models/context_profile.rs). */
export const BALANCED_BPS: Record<PackSection, number> = {
  rules: 3000,
  decisions: 2000,
  failures: 2000,
  evidence: 2000,
  artifacts: 1000,
};

export const SECTIONS: readonly PackSection[] = ["rules", "decisions", "failures", "evidence", "artifacts"];

/** The sections' names in a rendered pack (ContextProfileSection::as_str). */
export const SECTION_KEY: Record<PackSection, string> = {
  rules: "procedural_rules",
  decisions: "decisions",
  failures: "failures",
  evidence: "evidence",
  artifacts: "artifacts",
};

/** quota_for_basis_points: ceil(budget × bps / 10000), at least 1. A cap, never redistributed. */
export const quota = (section: PackSection) => Math.max(1, Math.ceil((BUDGET * BALANCED_BPS[section]) / 10000));

/** MemoryDecayHalfLives::default(), in days. */
export const HALF_LIFE = { working: 1, event: 30, failure: 90, semantic: 180, procedural: 365 } as const;
/** DEFAULT_DECAY_DEMOTE_THRESHOLD and DEFAULT_DECAY_FORGET_THRESHOLD. */
export const DEMOTE_THRESHOLD = 0.05;
export const FORGET_THRESHOLD = 0.01;

/** memory_decay_freshness_score: exp(-ln 2 · age / half-life). */
export const freshness = (ageDays: number, halfLife: number) => Math.exp((-Math.LN2 * Math.max(0, ageDays)) / halfLife);

/** The six stored trust classes (TrustClass::as_str), strongest first. */
export const TRUSTS = [
  "human_explicit",
  "peer_human_attested",
  "agent_validated",
  "agent_assertion",
  "cass_evidence",
  "legacy_import",
] as const;
export type Trust = (typeof TRUSTS)[number];
const TRUST_RANK = Object.fromEntries(TRUSTS.map((trust, index) => [trust, index])) as Record<Trust, number>;

export type EventType = "import" | "decision" | "promote" | "antipattern" | "validated" | "revisit" | "demote" | "tombstone";

export interface CampaignEvent {
  day: number;
  type: EventType;
  title: string;
  detail: string;
  /** The ee command this step corresponds to. */
  command: string;
  /** Change in procedural rules in force. */
  rules: number;
}


export interface PackItem {
  id: string;
  section: PackSection;
  level: Level;
  kind: string;
  trust: Trust;
  /** A trust-class transition after outcome feedback. */
  upgrade?: { day: number; to: Trust };
  source: string;
  text: string;
  tokens: number;
  from: number;
  /** Superseded, deprecated or tombstoned on this day. */
  until?: number;
  revisitBy?: number;
}

export type DomainId = "perf" | "equities" | "security" | "growth" | "legal";

export interface Domain {
  id: DomainId;
  label: string;
  title: string;
  who: string;
  workspace: string;
  task: string;
  /** A privileged workspace kept out of the user-global memory lane. */
  isolated?: boolean;
  seed: number;
  /** Per working day: scratch notes, episodic captures, semantic facts; the share of captures that are failures. */
  rates: { working: number; episodic: number; failureShare: number; semantic: number; promote: number; weekend: number };
  /** [from day, to day, factor, label]: kickoffs, incidents, earnings weeks, deadlines. */
  bursts: readonly (readonly [number, number, number, string])[];
  /** The share of episodic memories that keep being recalled (and so never age). */
  recalled: number;
  avoidRate: number;
  /** A tombstone event's day may be left to the simulation (-1): it lands on the sweep that retired the most; `{n}` is that sweep's count and `{total}` the count so far. */
  events: readonly CampaignEvent[];
  items: readonly PackItem[];
}

/** Day of week `dow` (0 = Monday) in week `week` (1-based). */
const w = (week: number, dow = 0) => (week - 1) * 7 + dow;

// ─── The five campaigns ──────────────────────────────────────────────────────

const PERF: Domain = {
  id: "perf",
  label: "Code perf",
  title: "Query-engine performance",
  who: "A coding agent and its benchmarking skill, one repository",
  workspace: "./optimization/query-engine",
  task: "find the next p99 win on oltp-mixed",
  seed: 1101,
  rates: { working: 11, episodic: 0.72, failureShare: 0.3, semantic: 0.62, promote: 0.06, weekend: 0.1 },
  bursts: [
    [0, 14, 1.5, "kickoff"],
    [77, 91, 1.3, "push"],
  ],
  recalled: 0.16,
  avoidRate: 0.065,
  events: [
    {
      day: 8,
      type: "decision",
      title: "Target recorded: p99 latency",
      detail: "It is chosen over a throughput goal and stored with the reason and a revisit date 90 days out.",
      command: 'ee decide record "perf target" --chosen "p99 ≤ 2 ms" --alternative "throughput +20%" --rationale "tail latency drives the SLO" --revisit-by +90d',
      rules: 0,
    },
    {
      day: w(4, 3),
      type: "antipattern",
      title: "Three prefetch regressions become an anti-pattern",
      detail: "Three failures are recorded with family=aggressive-prefetch, each with its bench run and revert SHA. Curation clusters them, and a reviewer validates and applies the anti-pattern.",
      command: "ee curate validate <candidate-id> && ee curate apply <candidate-id>",
      rules: 1,
    },
    {
      day: w(6, 4),
      type: "promote",
      title: "Playbook extract: four rules",
      detail: "Three or more notes carrying one lesson become a procedural-rule candidate. Four candidates pass review.",
      command: "ee playbook extract --dry-run --json",
      rules: 4,
    },
    {
      day: w(9, 1),
      type: "validated",
      title: "The bench-noise rule is validated",
      detail: "Helpful outcomes from six runs lift its 90% interval above 0.70: agent_assertion becomes agent_validated, with an audit row.",
      command: "ee outcome <memory-id> --signal helpful --source-id run-0912",
      rules: 0,
    },
    {
      day: w(12, 2),
      type: "promote",
      title: "Five more rules from the profiling work",
      detail: "Profiling, fsync batching and arena-sizing lessons, each backed by three runs, become rules.",
      command: "ee curate candidates --json",
      rules: 5,
    },
    {
      day: 98,
      type: "revisit",
      title: "Revisit due: the p99 target",
      detail: "After 90 days, ee decide revisit lists the decision. It is re-decided at 2.0 ms, and the new decision supersedes the old one.",
      command: "ee decide revisit --json",
      rules: 0,
    },
    {
      day: -1,
      type: "tombstone",
      title: "Decay sweeps: {total} stale notes tombstoned",
      detail: "This week's sweep took {n}. Bench notes unrecalled for months fall below lifecycle 0.01 and are tombstoned: an audited, reversible removal from retrieval.",
      command: "ee maintenance run --job decay_sweep --include-decay",
      rules: 0,
    },
    {
      day: w(21, 3),
      type: "demote",
      title: "The arena rule is deprecated",
      detail: "After an allocator switch, two runs report the 64 KiB-arena rule harmful. Two distinct sources meet the harmful quorum, and curation deprecates the rule.",
      command: "ee outcome <rule-id> --target-type rule --signal harmful --source-id run-2104",
      rules: -1,
    },
    {
      day: w(23, 2),
      type: "promote",
      title: "Four rules from the second half",
      detail: "Lessons from the WAL and allocator work, each seen three times, become rules.",
      command: "ee playbook extract --json",
      rules: 4,
    },
  ],
  items: [
    { id: "p-r1", section: "rules", level: "procedural", kind: "anti-pattern", trust: "agent_validated", source: "bench-run://2026-09-12T14:23/oltp-mixed-small-n", text: "Don't prefetch B-tree leaves on a fixed stride: three regressions on small-N reads, all cache pollution.", tokens: 90, from: w(4, 3) },
    { id: "p-r2", section: "rules", level: "procedural", kind: "rule", trust: "agent_assertion", upgrade: { day: w(9, 1), to: "agent_validated" }, source: "bench-run://oltp-mixed/variance-study", text: "Run oltp-mixed three times and report the median: one run's noise is ±4%.", tokens: 70, from: w(6, 4) },
    { id: "p-r3", section: "rules", level: "procedural", kind: "rule", trust: "agent_assertion", source: "flamegraph://runs/0412/cpu.svg", text: "Profile the release build with debug symbols; debug builds misrank the hot path.", tokens: 75, from: w(12, 2) },
    { id: "p-r4", section: "rules", level: "procedural", kind: "rule", trust: "agent_validated", source: "bench-run://alloc/arena-sweep", text: "Size the page arena at 64 KiB per worker.", tokens: 55, from: w(12, 2), until: w(21, 3) },
    { id: "p-r5", section: "rules", level: "procedural", kind: "rule", trust: "human_explicit", source: "git-sha://5d1e0a7", text: "Every optimisation lands with a median-of-3 bench diff and its revert SHA.", tokens: 70, from: w(23, 2) },
    { id: "p-d1", section: "decisions", level: "semantic", kind: "decision", trust: "human_explicit", source: "charter://oltp-campaign/v1", text: "Optimise p99, not throughput, for the OLTP mix. Rejected: throughput +20%.", tokens: 80, from: 8, until: 101, revisitBy: 98 },
    { id: "p-d2", section: "decisions", level: "semantic", kind: "decision", trust: "human_explicit", source: "design-review://w08/storage", text: "Keep the B-tree. An LSM rewrite was rejected as too much risk for one campaign.", tokens: 85, from: 51, revisitBy: 171 },
    { id: "p-d3", section: "decisions", level: "semantic", kind: "decision", trust: "human_explicit", source: "charter://oltp-campaign/v2", text: "p99 target reaffirmed at 2.0 ms after the revisit; supersedes the first.", tokens: 75, from: 101, revisitBy: 191 },
    { id: "p-f1", section: "failures", level: "episodic", kind: "failure", trust: "cass_evidence", source: "bench-run://prefetch/adaptive-w4", text: "Adaptive prefetch: −3% on small-N reads. Third in family aggressive-prefetch. Reverted at 9af3c21.", tokens: 95, from: w(4, 2) },
    { id: "p-f2", section: "failures", level: "episodic", kind: "failure", trust: "cass_evidence", source: "bench-run://soak/freelist-6h", text: "Lock-free freelist: ABA bug after a 6-hour soak. Reverted at 41c07e2.", tokens: 80, from: w(10, 3) },
    { id: "p-f3", section: "failures", level: "episodic", kind: "failure", trust: "cass_evidence", source: "bench-run://alloc/post-switch", text: "After the allocator switch, 64 KiB arenas cost +18% p99 on two runs.", tokens: 80, from: w(21, 1) },
    { id: "p-e1", section: "evidence", level: "episodic", kind: "fact", trust: "cass_evidence", source: "bench-run://baseline/oltp-mixed", text: "Baseline: p99 3.1 ms on oltp-mixed, median of three runs.", tokens: 90, from: 1 },
    { id: "p-e2", section: "evidence", level: "episodic", kind: "fact", trust: "cass_evidence", source: "bench-run://sharding/w8", text: "Page-cache sharding: p99 3.1 → 2.6 ms.", tokens: 85, from: w(8, 4) },
    { id: "p-e3", section: "evidence", level: "episodic", kind: "fact", trust: "cass_evidence", source: "bench-run://wal-batch/w16", text: "Batched WAL fsync: p99 2.6 → 2.2 ms.", tokens: 85, from: w(16, 3) },
    { id: "p-e4", section: "evidence", level: "episodic", kind: "fact", trust: "cass_evidence", source: "bench-run://oltp-mixed/w24", text: "p99 at 1.9 ms: under target for the first time.", tokens: 80, from: w(24, 1) },
    { id: "p-a1", section: "artifacts", level: "semantic", kind: "artifact", trust: "human_explicit", source: "file://benches/oltp_mixed.rs", text: "benches/oltp_mixed.rs: the campaign's harness.", tokens: 45, from: 0 },
    { id: "p-a2", section: "artifacts", level: "semantic", kind: "artifact", trust: "agent_assertion", source: "file://docs/perf/negative-ledger.md", text: "docs/perf/negative-ledger.md: every attempt that lost, and why.", tokens: 55, from: w(5, 1) },
  ],
};

const EQUITIES: Domain = {
  id: "equities",
  label: "Equities",
  title: "An equity position, re-underwritten",
  who: "A research agent and an analyst, one position in an invented freight carrier",
  workspace: "./positions/TDWL-long",
  task: "re-underwrite TDWL after the Q3 print",
  seed: 2203,
  rates: { working: 9, episodic: 0.68, failureShare: 0.22, semantic: 0.7, promote: 0.07, weekend: 0.06 },
  bursts: [
    [0, 14, 1.4, "initiation"],
    [28, 35, 1.9, "Q2 print"],
    [119, 126, 1.9, "Q3 print"],
  ],
  recalled: 0.2,
  avoidRate: 0.06,
  events: [
    {
      day: 0,
      type: "import",
      title: "Playbook import: three rules from a prior freight position",
      detail: "Rules move between workspaces only through an explicit import. Unauthenticated human_explicit rules are capped at agent_validated.",
      command: "ee playbook import --source ./freight-2025-playbook.json --apply",
      rules: 3,
    },
    {
      day: 3,
      type: "decision",
      title: "Thesis recorded: initiate a 2% long",
      detail: "It is chosen over waiting for the Q2 print and stored with the reason and a revisit date 90 days out.",
      command: 'ee decide record "TDWL position" --chosen "2% long" --alternative "wait for Q2" --rationale "contract repricing is underappreciated" --revisit-by +90d',
      rules: 0,
    },
    {
      day: w(5, 2),
      type: "antipattern",
      title: "Three failed screens become an anti-pattern",
      detail: "Three EV/EBITDA screens surfaced 'cheap' carriers at peak volumes, and all three were value traps. Curation clusters them; a reviewer validates and applies the anti-pattern.",
      command: "ee curate validate <candidate-id> && ee curate apply <candidate-id>",
      rules: 1,
    },
    {
      day: w(8, 1),
      type: "promote",
      title: "Five rules from the Q2 work",
      detail: "Lessons that showed up in three or more notes (reading order, guidance haircuts, peer sets) become rules.",
      command: "ee playbook extract --dry-run --json",
      rules: 5,
    },
    {
      day: 93,
      type: "revisit",
      title: "Revisit due: the 2% long",
      detail: "The thesis reaches its revisit date. After the Q3 guidance cut it is re-decided as a trim to 1%, which supersedes the original.",
      command: "ee decide revisit --json",
      rules: 0,
    },
    {
      day: w(16, 2),
      type: "demote",
      title: "The spot-rate rule is deprecated",
      detail: "Contract share passed 70%, and 'spot leads contract by two quarters' was wrong in two reviews by two analysts. That meets the harmful quorum, and curation deprecates it.",
      command: "ee outcome <rule-id> --target-type rule --signal harmful --source-id review-w16",
      rules: -1,
    },
    {
      day: -1,
      type: "tombstone",
      title: "Decay sweeps: {total} stale notes tombstoned",
      detail: "This week's sweep took {n}. Old channel checks and price-target notes that nobody recalled fall below lifecycle 0.01 and are tombstoned, which is audited and reversible.",
      command: "ee maintenance run --job decay_sweep --include-decay",
      rules: 0,
    },
    {
      day: w(22, 3),
      type: "promote",
      title: "Four rules from the re-underwriting",
      detail: "Sizing and guidance-credibility lessons, each seen three times, become rules.",
      command: "ee curate candidates --json",
      rules: 4,
    },
    {
      day: 160,
      type: "revisit",
      title: "Revisit due: the trim",
      detail: "The 1% sizing carried a 60-day revisit. It falls due before the next print, with the evidence gathered since.",
      command: "ee decide revisit --json",
      rules: 0,
    },
  ],
  items: [
    { id: "q-r1", section: "rules", level: "procedural", kind: "anti-pattern", trust: "agent_validated", source: "analyst-note://screens/ev-ebitda-w5", text: "Don't buy freight on trailing EV/EBITDA at peak volumes: three cheap screens, three value traps.", tokens: 85, from: w(5, 2) },
    { id: "q-r2", section: "rules", level: "procedural", kind: "rule", trust: "agent_assertion", upgrade: { day: w(19, 1), to: "agent_validated" }, source: "sec-filing://TDWL/10-Q/2026-Q2", text: "Read segment volumes in the 10-Q before the press release; the release leads with revenue per load.", tokens: 80, from: w(8, 1) },
    { id: "q-r3", section: "rules", level: "procedural", kind: "rule", trust: "agent_validated", source: "playbook://freight-2025/rule-07", text: "Spot rates lead contract rates by about two quarters.", tokens: 55, from: 0, until: w(16, 2) },
    { id: "q-r4", section: "rules", level: "procedural", kind: "rule", trust: "agent_validated", source: "playbook://freight-2025/rule-02", text: "Weight guidance by management's hit rate over the last eight quarters.", tokens: 65, from: 0 },
    { id: "q-r5", section: "rules", level: "procedural", kind: "rule", trust: "human_explicit", source: "ic-minutes://2026-w22", text: "Cap any single freight name at 2% until two prints confirm the thesis.", tokens: 65, from: w(22, 3) },
    { id: "q-d1", section: "decisions", level: "semantic", kind: "decision", trust: "human_explicit", source: "ic-minutes://2026-w01", text: "Initiate a 2% long. Rejected: wait for the Q2 print.", tokens: 70, from: 3, until: 100, revisitBy: 93 },
    { id: "q-d2", section: "decisions", level: "semantic", kind: "decision", trust: "human_explicit", source: "ic-minutes://2026-w15", text: "Trim to 1% after the Q3 guidance cut; supersedes the initial sizing.", tokens: 70, from: 100, revisitBy: 160 },
    { id: "q-f1", section: "failures", level: "episodic", kind: "failure", trust: "agent_assertion", source: "analyst-note://screens/ev-ebitda-w5", text: "Third EV/EBITDA screen pick was a value trap: volumes −11% the next quarter.", tokens: 80, from: w(5, 1) },
    { id: "q-f2", section: "failures", level: "episodic", kind: "failure", trust: "agent_assertion", source: "earnings-call://TDWL/2026-Q2", text: "Took the guidance mid-point at face value; it missed by 6%.", tokens: 70, from: w(11, 2) },
    { id: "q-f3", section: "failures", level: "episodic", kind: "failure", trust: "agent_assertion", source: "analyst-note://tdwl/margin-model-v3", text: "The spot-to-contract lag predicted a margin rebound that never came.", tokens: 75, from: w(16, 1) },
    { id: "q-e1", section: "evidence", level: "episodic", kind: "fact", trust: "human_explicit", source: "sec-filing://TDWL/10-K/2025", text: "10-K: contract freight is 64% of revenue, up from 51%.", tokens: 80, from: 1 },
    { id: "q-e2", section: "evidence", level: "episodic", kind: "fact", trust: "agent_assertion", source: "earnings-call://TDWL/2026-Q2", text: "Q2 call: contract renewals repriced +7%.", tokens: 75, from: w(5, 3) },
    { id: "q-e3", section: "evidence", level: "episodic", kind: "fact", trust: "agent_assertion", source: "channel-check://shipper-panel/w12", text: "Channel check: three shippers moved volume to rail.", tokens: 80, from: w(12, 2) },
    { id: "q-e4", section: "evidence", level: "episodic", kind: "fact", trust: "agent_assertion", source: "earnings-call://TDWL/2026-Q3", text: "Q3: volumes −4%, yield +5%, guidance cut.", tokens: 75, from: w(18, 3) },
    { id: "q-a1", section: "artifacts", level: "semantic", kind: "artifact", trust: "human_explicit", source: "file://models/tdwl-model.xlsx", text: "tdwl-model.xlsx: the three-statement model.", tokens: 45, from: 0 },
    { id: "q-a2", section: "artifacts", level: "semantic", kind: "artifact", trust: "human_explicit", source: "file://memos/tdwl-reunderwrite-v2.md", text: "The re-underwriting memo, v2.", tokens: 40, from: w(14, 2) },
  ],
};

const SECURITY: Domain = {
  id: "security",
  label: "Security",
  title: "Threat intel and incident response",
  who: "A security team's triage agents, across three incidents",
  workspace: "./ir/2026-h2",
  task: "triage the new phishing alert",
  seed: 3307,
  rates: { working: 10, episodic: 0.8, failureShare: 0.26, semantic: 0.5, promote: 0.06, weekend: 0.3 },
  bursts: [
    [14, 21, 2.3, "IR-014"],
    [70, 77, 2.0, "IR-022"],
    [133, 140, 2.4, "IR-031"],
  ],
  recalled: 0.13,
  avoidRate: 0.055,
  events: [
    {
      day: 0,
      type: "import",
      title: "Playbook import: the team's four response rules",
      detail: "The runbook's rules are imported as procedural rules with their provenance, and trust is capped at agent_validated.",
      command: "ee playbook import --source ./runbook-playbook.json --apply",
      rules: 4,
    },
    {
      day: 2,
      type: "decision",
      title: "Risk accepted: the legacy VPN",
      detail: "The risk is accepted until the migration, with MFA enforced and a 45-day revisit.",
      command: 'ee decide record "legacy VPN" --chosen "accept with MFA" --alternative "shut it now" --rationale "migration in flight" --revisit-by +45d',
      rules: 0,
    },
    {
      day: w(3, 2),
      type: "promote",
      title: "Incident IR-014 closed: three rules",
      detail: "The postmortem's lessons, confirmed in three notes each, become procedural rules.",
      command: "ee curate validate <candidate-id> && ee curate apply <candidate-id>",
      rules: 3,
    },
    {
      day: w(5, 4),
      type: "antipattern",
      title: "Three evaded IP blocks become an anti-pattern",
      detail: "Blocking single IPs failed three times against one actor, with harmful outcomes from two responders. ee proposes an anti-pattern; the lead accepts.",
      command: "ee curate candidates --json",
      rules: 1,
    },
    {
      day: 47,
      type: "revisit",
      title: "Revisit due: the VPN risk",
      detail: "The migration slipped. The acceptance is renewed for 30 days with geo-fencing, and an owner is named.",
      command: "ee decide revisit --json",
      rules: 0,
    },
    {
      day: w(11, 3),
      type: "promote",
      title: "Incident IR-022 closed: four rules",
      detail: "The same actor returns with a new lure. Capturing volatile memory first works this time, and four more lessons are promoted.",
      command: "ee playbook extract --json",
      rules: 4,
    },
    {
      day: w(15, 2),
      type: "demote",
      title: "A detection rule is deprecated",
      detail: "After an add-in update the rundll32-from-Office rule fired 400 times a day, and two analysts reported it harmful. Curation deprecates it, and a narrower rule replaces it.",
      command: "ee outcome <rule-id> --target-type rule --signal harmful --source-id triage-w15",
      rules: -1,
    },
    {
      day: -1,
      type: "tombstone",
      title: "Decay sweeps: {total} stale indicators tombstoned",
      detail: "This week's sweep took {n}. Few indicator notes are recalled, so most decay quickly. Below lifecycle 0.01 they are tombstoned: an audited, reversible removal from retrieval.",
      command: "ee maintenance run --job decay_sweep --include-decay",
      rules: 0,
    },
    {
      day: w(20, 4),
      type: "promote",
      title: "Incident IR-031 closed: three rules",
      detail: "IR-031 is contained in 38 minutes; IR-014 took six hours. Three more lessons are promoted.",
      command: "ee curate candidates --json",
      rules: 3,
    },
  ],
  items: [
    { id: "s-r1", section: "rules", level: "procedural", kind: "playbook-step", trust: "agent_assertion", upgrade: { day: w(17, 1), to: "agent_validated" }, source: "incident://IR-2026-014/postmortem", text: "Capture volatile memory before isolating a host: isolation killed the loader's only copy.", tokens: 80, from: w(3, 2) },
    { id: "s-r2", section: "rules", level: "procedural", kind: "anti-pattern", trust: "agent_validated", source: "incident://IR-2026-017/timeline", text: "Don't answer this actor with single-IP blocks: it rotated infrastructure within 40 minutes, three times.", tokens: 85, from: w(5, 4) },
    { id: "s-r3", section: "rules", level: "procedural", kind: "rule", trust: "agent_validated", source: "sigma://rules/office-rundll32", text: "Alert on rundll32 spawned by an Office application.", tokens: 50, from: 0, until: w(15, 2) },
    { id: "s-r4", section: "rules", level: "procedural", kind: "rule", trust: "agent_assertion", source: "sigma://rules/office-rundll32-v2", text: "Alert on rundll32 from Office only when the DLL is unsigned and user-writable.", tokens: 65, from: w(15, 2) },
    { id: "s-r5", section: "rules", level: "procedural", kind: "rule", trust: "agent_validated", source: "runbook://identity/token-theft", text: "After a token-theft phish, revoke sessions as well as resetting passwords.", tokens: 60, from: 0 },
    { id: "s-d1", section: "decisions", level: "semantic", kind: "decision", trust: "human_explicit", source: "risk-register://R-031", text: "Accept the legacy-VPN risk until migration, with MFA enforced.", tokens: 70, from: 2, until: 50, revisitBy: 47 },
    { id: "s-d2", section: "decisions", level: "semantic", kind: "decision", trust: "human_explicit", source: "risk-register://R-031/v2", text: "Renewed 30 days with geo-fencing; migration owner named.", tokens: 65, from: 50, until: 88, revisitBy: 80 },
    { id: "s-d3", section: "decisions", level: "semantic", kind: "decision", trust: "human_explicit", source: "risk-register://R-031/closed", text: "Legacy VPN decommissioned; the risk is closed.", tokens: 50, from: 88 },
    { id: "s-d4", section: "decisions", level: "semantic", kind: "decision", trust: "human_explicit", source: "change://edr/server-policy", text: "Keep EDR in block mode on servers. Rejected: audit-only.", tokens: 60, from: w(9, 1), revisitBy: w(9, 1) + 120 },
    { id: "s-f1", section: "failures", level: "episodic", kind: "failure", trust: "agent_assertion", source: "incident://IR-2026-014/postmortem", text: "Isolated the host first; the in-memory loader was lost.", tokens: 70, from: w(3, 1) },
    { id: "s-f2", section: "failures", level: "episodic", kind: "failure", trust: "agent_assertion", source: "incident://IR-2026-017/timeline", text: "Third single-IP block, evaded within 40 minutes.", tokens: 65, from: w(5, 3) },
    { id: "s-f3", section: "failures", level: "episodic", kind: "failure", trust: "agent_assertion", source: "edr://alerts/storm-w15", text: "rundll32 rule: 400 false positives in a day after an add-in update.", tokens: 70, from: w(15, 1) },
    { id: "s-e1", section: "evidence", level: "episodic", kind: "fact", trust: "peer_human_attested", source: "mitre://T1566.001", text: "IR-014: spearphishing attachment, then scheduled-task persistence.", tokens: 75, from: w(3, 0) },
    { id: "s-e2", section: "evidence", level: "episodic", kind: "fact", trust: "agent_assertion", source: "incident://IR-2026-022", text: "IR-022: same actor, new lure; memory capture recovered the loader.", tokens: 80, from: w(11, 2) },
    { id: "s-e3", section: "evidence", level: "episodic", kind: "fact", trust: "agent_assertion", source: "incident://IR-2026-031", text: "IR-031: contained in 38 minutes, against six hours for IR-014.", tokens: 75, from: w(20, 3) },
    { id: "s-a1", section: "artifacts", level: "semantic", kind: "artifact", trust: "human_explicit", source: "file://runbooks/phishing.md", text: "runbooks/phishing.md: the team's response runbook.", tokens: 45, from: 0 },
    { id: "s-a2", section: "artifacts", level: "semantic", kind: "artifact", trust: "agent_assertion", source: "file://detections/office-rundll32-v2.yml", text: "detections/office-rundll32-v2.yml: the narrower rule.", tokens: 45, from: w(15, 2) },
  ],
};

const GROWTH: Domain = {
  id: "growth",
  label: "Growth",
  title: "Lifecycle email experiments",
  who: "A growth team's analysis agent, some forty tests",
  workspace: "./campaigns/lifecycle-h2",
  task: "plan next week's onboarding-email test",
  seed: 4409,
  rates: { working: 8, episodic: 0.75, failureShare: 0.28, semantic: 0.6, promote: 0.07, weekend: 0.04 },
  bursts: [
    [0, 14, 1.3, "kickoff"],
    [49, 56, 1.8, "launch"],
    [140, 147, 1.8, "launch"],
  ],
  recalled: 0.15,
  avoidRate: 0.06,
  events: [
    {
      day: 0,
      type: "import",
      title: "Playbook import: three house rules",
      detail: "The team's standing rules are imported with their provenance, and trust is capped at agent_validated.",
      command: "ee playbook import --source ./house-rules.json --apply",
      rules: 3,
    },
    {
      day: 1,
      type: "decision",
      title: "Primary metric recorded: activation",
      detail: "Seven-day activation is chosen over open rate, with a revisit date 150 days out.",
      command: 'ee decide record "primary metric" --chosen "7-day activation" --alternative "open rate" --rationale "opens do not pay" --revisit-by +150d',
      rules: 0,
    },
    {
      day: w(4, 2),
      type: "antipattern",
      title: "Three losing discount subject lines become an anti-pattern",
      detail: "Discount-first subject lines lost three tests in a row on activation. Curation clusters them; a reviewer validates and applies the anti-pattern.",
      command: "ee curate validate <candidate-id> && ee curate apply <candidate-id>",
      rules: 1,
    },
    {
      day: w(6, 3),
      type: "promote",
      title: "Playbook extract: four rules",
      detail: "One variable per test, a minimum arm size, a standing holdout and a copy rule each appear in three notes and pass review.",
      command: "ee playbook extract --dry-run --json",
      rules: 4,
    },
    {
      day: w(10, 1),
      type: "decision",
      title: "Budget shift recorded",
      detail: "30% of paid social moves to search, with a 60-day revisit.",
      command: 'ee decide record "channel mix" --chosen "shift 30% to search" --alternative "hold" --rationale "lower CAC in test" --revisit-by +60d',
      rules: 0,
    },
    {
      day: w(13, 4),
      type: "demote",
      title: "The send-time rule is deprecated",
      detail: "A mail-client privacy change inflated opens. 'Tuesday 10:00 wins' was based on opens, and it lost twice on activation for two marketers. Curation deprecates it.",
      command: "ee outcome <rule-id> --target-type rule --signal harmful --source-id test-w13",
      rules: -1,
    },
    {
      day: 124,
      type: "revisit",
      title: "Revisit due: the budget shift",
      detail: "Search CAC rose 40% in eight weeks. Half the shift is reversed and the new decision supersedes the old.",
      command: "ee decide revisit --json",
      rules: 0,
    },
    {
      day: -1,
      type: "tombstone",
      title: "Decay sweeps: {total} stale drafts tombstoned",
      detail: "This week's sweep took {n}. Copy drafts and test notes that nobody recalled fall below lifecycle 0.01 and are tombstoned, which is audited and reversible.",
      command: "ee maintenance run --job decay_sweep --include-decay",
      rules: 0,
    },
    {
      day: w(22, 3),
      type: "promote",
      title: "Four rules from the second launch",
      detail: "Activation-first judging and three copy lessons, each seen three times, become rules.",
      command: "ee curate candidates --json",
      rules: 4,
    },
  ],
  items: [
    { id: "g-r1", section: "rules", level: "procedural", kind: "anti-pattern", trust: "agent_validated", source: "experiment://onboarding/subject-discount-3", text: "Don't lead onboarding subject lines with a discount: −9%, −4% and −6% activation in three tests.", tokens: 85, from: w(4, 2) },
    { id: "g-r2", section: "rules", level: "procedural", kind: "rule", trust: "agent_assertion", upgrade: { day: w(16, 2), to: "agent_validated" }, source: "ga4://property/onboarding/holdout-w6", text: "Keep a 5% holdout on every lifecycle flow; lift without one is guesswork.", tokens: 70, from: w(6, 3) },
    { id: "g-r3", section: "rules", level: "procedural", kind: "rule", trust: "agent_validated", source: "mixpanel://reports/send-time-2025", text: "Send at Tuesday 10:00 local: the highest open rate.", tokens: 50, from: 0, until: w(13, 4) },
    { id: "g-r4", section: "rules", level: "procedural", kind: "rule", trust: "agent_assertion", source: "experiment://playbook/power-calc", text: "One variable per test, and at least 20,000 recipients per arm.", tokens: 60, from: w(6, 3) },
    { id: "g-r5", section: "rules", level: "procedural", kind: "rule", trust: "human_explicit", source: "ga4://explorations/activation-7d", text: "Judge every send by 7-day activation, never by opens.", tokens: 55, from: w(22, 3) },
    { id: "g-d1", section: "decisions", level: "semantic", kind: "decision", trust: "human_explicit", source: "campaign://lifecycle/charter", text: "Primary metric is 7-day activation. Rejected: open rate.", tokens: 60, from: 1, revisitBy: 151 },
    { id: "g-d2", section: "decisions", level: "semantic", kind: "decision", trust: "human_explicit", source: "campaign://budget/2026-w10", text: "Shift 30% of paid-social budget to search.", tokens: 55, from: w(10, 1), until: 128, revisitBy: 124 },
    { id: "g-d3", section: "decisions", level: "semantic", kind: "decision", trust: "human_explicit", source: "ga4://acquisition/cac-w18", text: "Reverse half the shift: search CAC rose 40% in eight weeks.", tokens: 65, from: 128, revisitBy: 188 },
    { id: "g-f1", section: "failures", level: "episodic", kind: "failure", trust: "agent_assertion", source: "experiment://onboarding/subject-discount-3", text: "Third discount-led subject line: −6% activation against control.", tokens: 70, from: w(4, 1) },
    { id: "g-f2", section: "failures", level: "episodic", kind: "failure", trust: "agent_assertion", source: "experiment://onboarding/hero-gif", text: "Animated hero image: +12% clicks, −3% activation. The clicks were curiosity.", tokens: 75, from: w(9, 2) },
    { id: "g-f3", section: "failures", level: "episodic", kind: "failure", trust: "agent_assertion", source: "experiment://send-time/w13", text: "The Tuesday-10:00 send won on opens and lost on activation, twice.", tokens: 70, from: w(13, 2) },
    { id: "g-e1", section: "evidence", level: "episodic", kind: "fact", trust: "agent_assertion", source: "ga4://explorations/activation-7d", text: "Baseline: 31% of sign-ups activate within seven days.", tokens: 70, from: 1 },
    { id: "g-e2", section: "evidence", level: "episodic", kind: "fact", trust: "agent_assertion", source: "campaign://launch/2026-w08", text: "Launch week: 48k sends; activation 33.5% against a 31% holdout.", tokens: 80, from: w(8, 3) },
    { id: "g-e3", section: "evidence", level: "episodic", kind: "fact", trust: "agent_assertion", source: "mixpanel://reports/opens-w12", text: "Opens jumped 22 points overnight with clicks flat: opens no longer measure reading.", tokens: 85, from: w(12, 4) },
    { id: "g-e4", section: "evidence", level: "episodic", kind: "fact", trust: "agent_assertion", source: "campaign://launch/2026-w21", text: "Second launch: activation 35.2% against a 31.4% holdout.", tokens: 75, from: w(21, 2) },
    { id: "g-a1", section: "artifacts", level: "semantic", kind: "artifact", trust: "human_explicit", source: "file://briefs/lifecycle-h2.md", text: "briefs/lifecycle-h2.md: the programme brief.", tokens: 45, from: 0 },
    { id: "g-a2", section: "artifacts", level: "semantic", kind: "artifact", trust: "agent_assertion", source: "file://calc/power.ipynb", text: "calc/power.ipynb: sample size per arm.", tokens: 40, from: w(6, 3) },
  ],
};

const LEGAL: Domain = {
  id: "legal",
  label: "Legal",
  title: "A commercial contract dispute",
  who: "A litigation team's drafting agent, one invented matter",
  workspace: "./matters/harrow-v-kestrel",
  task: "prepare for the expert depositions",
  isolated: true,
  seed: 5511,
  rates: { working: 9, episodic: 0.65, failureShare: 0.2, semantic: 0.72, promote: 0.07, weekend: 0.15 },
  bursts: [
    [0, 14, 1.4, "intake"],
    [56, 63, 1.8, "discovery"],
    [119, 126, 1.9, "experts"],
    [161, 175, 1.5, "MSJ"],
  ],
  recalled: 0.22,
  avoidRate: 0.06,
  events: [
    {
      day: 0,
      type: "import",
      title: "Playbook import: three rules from a prior matter",
      detail: "Rules reach a new matter only through an explicit, audited import, and this workspace is kept out of the user-global lane.",
      command: "ee playbook import --source ./contract-matters-playbook.json --apply",
      rules: 3,
    },
    {
      day: 7,
      type: "decision",
      title: "Strategy recorded: early mediation",
      detail: "It is chosen over an early motion to dismiss, to be revisited once the expert reports are in.",
      command: 'ee decide record "case strategy" --chosen "early mediation" --alternative "motion to dismiss" --rationale "cap clause favours a settlement" --revisit-by +112d',
      rules: 0,
    },
    {
      day: w(6, 3),
      type: "antipattern",
      title: "Three discovery losses become an anti-pattern",
      detail: "Two motions to compel and a letter brief failed for want of a dated meet-and-confer log. Curation clusters them; a reviewer validates and applies the anti-pattern.",
      command: "ee curate validate <candidate-id> && ee curate apply <candidate-id>",
      rules: 1,
    },
    {
      day: w(9, 2),
      type: "promote",
      title: "Four rules from discovery",
      detail: "Calendaring, privilege logging, exhibit handling and production, each seen in three notes, become rules.",
      command: "ee playbook extract --dry-run --json",
      rules: 4,
    },
    {
      day: w(14, 1),
      type: "validated",
      title: "The calendaring rule is validated",
      detail: "Helpful outcomes on six deadlines lift its 90% interval above 0.70: agent_assertion becomes agent_validated.",
      command: "ee outcome <memory-id> --signal helpful --source-id dkt-58",
      rules: 0,
    },
    {
      day: 119,
      type: "revisit",
      title: "Revisit due: early mediation",
      detail: "The expert reports are in and the damages theories differ fourfold. The strategy changes from mediation to summary judgment on the cap clause.",
      command: "ee decide revisit --json",
      rules: 0,
    },
    {
      day: w(19, 1),
      type: "demote",
      title: "The extension rule is deprecated",
      detail: "After the case was reassigned, 'This court grants stipulated extensions' failed on two filings for two associates. Curation deprecates it.",
      command: "ee outcome <rule-id> --target-type rule --signal harmful --source-id dkt-77",
      rules: -1,
    },
    {
      day: -1,
      type: "tombstone",
      title: "Decay sweeps: {total} stale notes tombstoned",
      detail: "This week's sweep took {n}. Superseded clause drafts and scheduling notes fall below lifecycle 0.01 and are tombstoned, which is audited and reversible.",
      command: "ee maintenance run --job decay_sweep --include-decay",
      rules: 0,
    },
    {
      day: w(23, 3),
      type: "promote",
      title: "Four rules from expert discovery",
      detail: "Disclosure and deposition-prep lessons, each seen three times, become rules for the matter.",
      command: "ee curate candidates --json",
      rules: 4,
    },
  ],
  items: [
    { id: "l-r1", section: "rules", level: "procedural", kind: "anti-pattern", trust: "agent_validated", source: "docket://harrow-v-kestrel/dkt-41", text: "Don't move to compel without a dated meet-and-confer log: two motions and a letter brief failed on it.", tokens: 85, from: w(6, 3) },
    { id: "l-r2", section: "rules", level: "procedural", kind: "rule", trust: "agent_assertion", upgrade: { day: w(14, 1), to: "agent_validated" }, source: "docket://harrow-v-kestrel/dkt-12", text: "Calendar every deadline from the docket entry, never from the service email.", tokens: 65, from: w(9, 2) },
    { id: "l-r3", section: "rules", level: "procedural", kind: "rule", trust: "agent_validated", source: "playbook://contract-matters/rule-04", text: "This court grants stipulated extensions as a matter of course.", tokens: 55, from: 0, until: w(19, 1) },
    { id: "l-r4", section: "rules", level: "procedural", kind: "rule", trust: "agent_validated", source: "playbook://contract-matters/rule-01", text: "Log privilege calls the day they are made; reconstructing them costs a week.", tokens: 65, from: 0 },
    { id: "l-r5", section: "rules", level: "procedural", kind: "rule", trust: "human_explicit", source: "depo://expert-prep/rebuttal", text: "Serve rebuttal-expert disclosures with the full materials-considered list.", tokens: 60, from: w(23, 3) },
    { id: "l-d1", section: "decisions", level: "semantic", kind: "decision", trust: "human_explicit", source: "case://harrow-v-kestrel/strategy-memo-1", text: "Pursue early mediation. Rejected: an early motion to dismiss.", tokens: 65, from: 7, until: 126, revisitBy: 119 },
    { id: "l-d2", section: "decisions", level: "semantic", kind: "decision", trust: "human_explicit", source: "contract-clause://msa/section-9.2", text: "Move for summary judgment on the limitation-of-liability clause.", tokens: 65, from: 126, revisitBy: 171 },
    { id: "l-f1", section: "failures", level: "episodic", kind: "failure", trust: "agent_assertion", source: "docket://harrow-v-kestrel/dkt-19", text: "Response date calendared from the service email; two days lost.", tokens: 65, from: w(4, 1) },
    { id: "l-f2", section: "failures", level: "episodic", kind: "failure", trust: "agent_assertion", source: "docket://harrow-v-kestrel/dkt-41", text: "Motion to compel denied: no record of a meet-and-confer.", tokens: 60, from: w(6, 2) },
    { id: "l-f3", section: "failures", level: "episodic", kind: "failure", trust: "agent_assertion", source: "docket://harrow-v-kestrel/dkt-77", text: "Stipulated extension denied by the new judge; the brief was written in four days.", tokens: 75, from: w(19, 0) },
    { id: "l-e1", section: "evidence", level: "episodic", kind: "fact", trust: "human_explicit", source: "contract-clause://msa/section-9.2", text: "MSA §9.2 caps liability at twelve months of fees.", tokens: 70, from: 1 },
    { id: "l-e2", section: "evidence", level: "episodic", kind: "fact", trust: "agent_assertion", source: "depo://harrow-v-kestrel/cfo-day-1", text: "CFO deposition: the cap was negotiated, not boilerplate.", tokens: 75, from: w(10, 2) },
    { id: "l-e3", section: "evidence", level: "episodic", kind: "fact", trust: "agent_assertion", source: "case://harrow-v-kestrel/expert-reports", text: "Expert reports: the damages theories differ by a factor of four.", tokens: 75, from: w(18, 2) },
    { id: "l-a1", section: "artifacts", level: "semantic", kind: "artifact", trust: "human_explicit", source: "file://matter/chronology.xlsx", text: "chronology.xlsx: the matter chronology.", tokens: 40, from: 0 },
    { id: "l-a2", section: "artifacts", level: "semantic", kind: "artifact", trust: "agent_assertion", source: "file://matter/drafts/msj-v3.docx", text: "drafts/msj-v3.docx: the summary-judgment brief.", tokens: 45, from: w(19, 3) },
  ],
};

export const DOMAINS: readonly Domain[] = [PERF, EQUITIES, SECURITY, GROWTH, LEGAL];

// ─── The simulation ──────────────────────────────────────────────────────────

export interface Campaign {
  domain: Domain;
  /** Live memories at each level, per day. */
  levels: Record<Level, number[]>;
  /** Live episodic failures and episodic events, per day (their sum is levels.episodic). */
  failures: number[];
  episodes: number[];
  /** Memories ever written, and those expired, tombstoned or deprecated, cumulative per day. */
  written: number[];
  retired: number[];
  /** Episodic memories the weekly decay sweep tombstoned on each day. */
  swept: number[];
  /** Procedural rules in force, per day. */
  rules: number[];
  promoted: number[];
  deprecated: number[];
  /** Cumulative dead ends avoided: a known failure or anti-pattern surfaced before a retry. */
  avoided: number[];
  /** The events, resolved and in order of day. */
  timeline: CampaignEvent[];
  maxTotal: number;
  maxRules: number;
}

interface Episode {
  born: number;
  failure: boolean;
  /** confidence × utility. */
  cu: number;
  /** Recalled every `every` days (which resets its clock), or never (0). */
  every: number;
  dead: number;
}

const zeros = () => new Array<number>(DAYS).fill(0);

/** The weekly decay sweep's verdict on one episodic memory: lifecycle = freshness × confidence × utility. */
export function lifecycleAt(episode: { born: number; failure: boolean; cu: number; every: number }, day: number): number {
  const lastUse = episode.every > 0 ? episode.born + Math.floor((day - episode.born) / episode.every) * episode.every : episode.born;
  return freshness(day - lastUse, episode.failure ? HALF_LIFE.failure : HALF_LIFE.event) * episode.cu;
}

export function simulate(domain: Domain): Campaign {
  const rand = seeded(domain.seed);
  const levels: Record<Level, number[]> = { working: zeros(), episodic: zeros(), semantic: zeros(), procedural: zeros() };
  const failures = zeros();
  const episodes = zeros();
  const written = zeros();
  const retired = zeros();
  const swept = zeros();
  const rules = zeros();
  const promoted = zeros();
  const deprecated = zeros();
  const avoided = zeros();

  const fixed = domain.events.filter((event) => event.day >= 0);
  const burst = (day: number) => domain.bursts.reduce((factor, [from, to, k]) => (day >= from && day < to ? factor * k : factor), 1);
  const live: Episode[] = [];
  let semantic = 0;
  let inForce = 0;
  let wrote = 0;
  let gone = 0;
  let up = 0;
  let down = 0;
  let saved = 0;
  let carry = 0;
  let antiPatterns = 0;
  let expiring = 0;

  const capture = (day: number, failure: boolean) => {
    const recalled = rand() < domain.recalled;
    live.push({ born: day, failure, cu: 0.04 + 0.24 * rand(), every: recalled ? 7 + Math.floor(rand() * 22) : 0, dead: Number.POSITIVE_INFINITY });
  };

  for (let day = 0; day < DAYS; day++) {
    const weekday = day % 7;
    const works = weekday < 5 || rand() < domain.rates.weekend;
    const factor = burst(day);

    // Working scratch: written during the session; at workflow close the important few become episodic, the rest expire.
    // A kept note is counted once, as episodic from today; the rest stay live until the day closes and count as
    // expired from the next, so the live total is always exactly what was written less what was retired.
    const notes = works ? Math.max(2, Math.round(domain.rates.working * factor * (0.6 + 0.8 * rand()))) : rand() < 0.3 ? 1 : 0;
    wrote += notes;
    const kept = works ? Math.round(notes * domain.rates.promote) : 0;
    gone += expiring;
    expiring = notes - kept;
    levels.working[day] = expiring;
    for (let i = 0; i < kept; i++) capture(day, false);

    if (works) {
      const captures = Math.round(domain.rates.episodic * factor * (0.4 + 1.2 * rand()));
      for (let i = 0; i < captures; i++) capture(day, rand() < domain.rates.failureShare);
      wrote += captures;
      const facts = Math.round(domain.rates.semantic * factor * (0.3 + 1.4 * rand()));
      semantic += facts;
      wrote += facts;
    }

    // Curation and outcome feedback move the procedural level, on the events' days.
    for (const event of fixed) {
      if (event.day !== day || event.rules === 0) continue;
      inForce += event.rules;
      if (event.rules > 0) {
        up += event.rules;
        wrote += event.rules;
      } else {
        down -= event.rules;
        gone -= event.rules;
      }
      if (event.type === "antipattern") antiPatterns += 1;
    }

    // The decay sweep, weekly on Sunday (a toy cadence): an episodic memory under the forget threshold is tombstoned.
    if (weekday === 6) {
      let count = 0;
      for (const episode of live) {
        if (episode.dead <= day) continue;
        if (lifecycleAt(episode, day) < FORGET_THRESHOLD) {
          episode.dead = day;
          count += 1;
        }
      }
      swept[day] = count;
      gone += count;
    }

    let liveFailures = 0;
    let liveEvents = 0;
    for (const episode of live) {
      if (episode.dead <= day) continue;
      if (episode.failure) liveFailures += 1;
      else liveEvents += 1;
    }

    // Dead ends avoided: each session's chance of meeting a known dead end grows with what is known about them.
    if (works) {
      carry += domain.avoidRate * (Math.sqrt(liveFailures) + 1.5 * antiPatterns) * Math.min(1.6, factor);
      while (carry >= 1) {
        saved += 1;
        carry -= 1;
      }
    }

    levels.episodic[day] = liveFailures + liveEvents;
    levels.semantic[day] = semantic;
    levels.procedural[day] = inForce;
    failures[day] = liveFailures;
    episodes[day] = liveEvents;
    rules[day] = inForce;
    promoted[day] = up;
    deprecated[day] = down;
    written[day] = wrote;
    retired[day] = gone;
    avoided[day] = saved;
  }

  // The tombstone event lands on the sweep that retired the most, away from the other markers.
  const timeline: CampaignEvent[] = domain.events.map((event) => {
    if (event.day >= 0) return { ...event };
    const sundays = [];
    for (let day = 83; day < DAYS - 7; day += 7) sundays.push(day);
    sundays.sort((a, b) => (swept[b] ?? 0) - (swept[a] ?? 0) || a - b);
    const clear = sundays.find((day) => fixed.every((other) => Math.abs(other.day - day) >= 9)) ?? sundays[0] ?? DAYS - 1;
    let total = 0;
    for (let day = 0; day <= clear; day++) total += swept[day] ?? 0;
    const fill = (template: string) => template.replace("{n}", String(swept[clear] ?? 0)).replace("{total}", String(total));
    return { ...event, day: clear, title: fill(event.title), detail: fill(event.detail) };
  });
  timeline.sort((a, b) => a.day - b.day);

  let maxTotal = 0;
  for (let day = 0; day < DAYS; day++) maxTotal = Math.max(maxTotal, totalAt({ levels }, day));
  return {
    domain,
    levels,
    failures,
    episodes,
    written,
    retired,
    swept,
    rules,
    promoted,
    deprecated,
    avoided,
    timeline,
    maxTotal,
    maxRules: Math.max(...rules),
  };
}

export const LEVELS_BOTTOM_UP: readonly Level[] = ["procedural", "semantic", "episodic", "working"];

export function totalAt(campaign: Pick<Campaign, "levels">, day: number): number {
  return LEVELS_BOTTOM_UP.reduce((sum, level) => sum + (campaign.levels[level][day] ?? 0), 0);
}

/** A toy size per level, for the store's rough token mass. */
export const STORE_TOKENS: Record<Level, number> = { working: 40, episodic: 110, semantic: 90, procedural: 85 };

export function storeTokensAt(campaign: Campaign, day: number): number {
  return LEVELS_BOTTOM_UP.reduce((sum, level) => sum + (campaign.levels[level][day] ?? 0) * STORE_TOKENS[level], 0);
}

// ─── Monday's pack ───────────────────────────────────────────────────────────

/** The toy's size for an unnamed memory in each section. */
export const GENERIC_TOKENS: Record<PackSection, number> = { rules: 90, decisions: 95, failures: 100, evidence: 125, artifacts: 60 };
/** How many named items each section shows; the rest are counted. */
export const SHOWN: Record<PackSection, number> = { rules: 2, decisions: 2, failures: 1, evidence: 1, artifacts: 1 };

export interface PackLine {
  item: PackItem;
  trust: Trust;
  /** Arrived since last Monday. */
  fresh: boolean;
  /** Its trust class moved up since last Monday. */
  upgraded: boolean;
  due: boolean;
}

export interface SectionPack {
  section: PackSection;
  quota: number;
  used: number;
  lines: PackLine[];
  more: number;
  moreTokens: number;
}

export interface Pack {
  day: number;
  sections: SectionPack[];
  used: number;
}

export const mondayOf = (day: number) => {
  const whole = Math.max(0, Math.min(DAYS - 1, Math.floor(day)));
  return whole - (whole % 7);
};
export const weekOf = (day: number) => Math.floor(Math.max(0, Math.min(DAYS - 1, day)) / 7) + 1;
export const trustAt = (item: PackItem, day: number): Trust => (item.upgrade && day >= item.upgrade.day ? item.upgrade.to : item.trust);
export const isLive = (item: PackItem, day: number) => item.from <= day && (item.until === undefined || day < item.until);

/** How many unnamed candidates the store holds for a section on a day (the named ones are examples of the same). */
function pool(campaign: Campaign, section: PackSection, day: number, named: number): number {
  switch (section) {
    case "rules":
      return Math.max(0, (campaign.rules[day] ?? 0) - named);
    case "decisions":
      return Math.floor((campaign.levels.semantic[day] ?? 0) * 0.1);
    case "failures":
      return Math.max(0, (campaign.failures[day] ?? 0) - named);
    case "evidence":
      return Math.max(0, (campaign.episodes[day] ?? 0) - named);
    case "artifacts":
      return Math.floor(day / 14);
  }
}

/**
 * The pack `ee pack` would hand Monday's session, at toy scale: each section filled up to its balanced quota, named
 * items first (decisions coming due, then by trust, then newest), then unnamed ones of a toy size. The quotas are caps
 * and are not redistributed, so early on, with no rules yet, the pack runs under its budget.
 */
export function packAt(campaign: Campaign, day: number): Pack {
  const monday = mondayOf(day);
  const sections = SECTIONS.map((section): SectionPack => {
    const cap = quota(section);
    const lines = campaign.domain.items
      .filter((item) => item.section === section && isLive(item, monday))
      .map((item): PackLine => {
        const trust = trustAt(item, monday);
        return {
          item,
          trust,
          fresh: item.from > monday - 7,
          upgraded: item.upgrade !== undefined && item.upgrade.day <= monday && item.upgrade.day > monday - 7,
          due: item.revisitBy !== undefined && monday >= item.revisitBy,
        };
      })
      .sort((a, b) => {
        if (section === "rules" || section === "decisions") {
          return Number(b.due) - Number(a.due) || TRUST_RANK[a.trust] - TRUST_RANK[b.trust] || b.item.from - a.item.from;
        }
        return b.item.from - a.item.from;
      });
    let used = 0;
    let more = 0;
    let moreTokens = 0;
    const shown: PackLine[] = [];
    for (const line of lines) {
      if (used + line.item.tokens > cap) continue;
      used += line.item.tokens;
      if (shown.length < SHOWN[section]) shown.push(line);
      else {
        more += 1;
        moreTokens += line.item.tokens;
      }
    }
    const size = GENERIC_TOKENS[section];
    const extra = Math.min(pool(campaign, section, monday, lines.length), Math.floor((cap - used) / size));
    used += extra * size;
    more += extra;
    moreTokens += extra * size;
    return { section, quota: cap, used, lines: shown, more, moreTokens };
  });
  return { day: monday, sections, used: Math.min(BUDGET, sections.reduce((sum, entry) => sum + entry.used, 0)) };
}
