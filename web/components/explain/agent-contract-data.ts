// The record behind components/explain/agent-contract.tsx: six situations, each with the JSON ee
// prints for it and the reading an agent makes of it.
//
// Every record is verbatim from the file named in its `source` field:
// - a pinned snapshot or golden (`kind: "pinned"`), checked field for field by
//   tests/agent-contract.test.ts;
// - or, where no snapshot pins the whole envelope, built from the source that emits it
//   (`kind: "source"`). The test then checks that every key and string in the record appears in
//   the files named.
// Two marks keep any cut visible:
// - a `"…": true` key stands for fields left out of an object;
// - an array item `{ "…": true }` stands for entries left out of an array.
// `<…>` inside a string is a runtime value. Nothing is left out without a mark: the test fails
// if an object here lacks a key its file has, unless the object carries the mark.

export type Json = null | boolean | number | string | Json[] | { [key: string]: Json };

/** The elision mark, as a key (`"…": true`) and as an array item (`{ "…": true }`). */
export const ELIDED = "…";

// ─── Exit codes (src/models/mod.rs, enum ProcessExitCode) ───────────────────

export const EXIT_CODES: readonly { code: number; variant: string; meaning: string }[] = [
  { code: 0, variant: "Success", meaning: "success" },
  { code: 1, variant: "Usage", meaning: "usage error" },
  { code: 2, variant: "Configuration", meaning: "configuration error" },
  { code: 3, variant: "Storage", meaning: "storage error" },
  { code: 4, variant: "SearchIndex", meaning: "search/index error" },
  { code: 5, variant: "Import", meaning: "import error" },
  { code: 6, variant: "UnsatisfiedDegradedMode", meaning: "a required mode could not be met" },
  { code: 7, variant: "PolicyDenied", meaning: "policy denied" },
  { code: 8, variant: "MigrationRequired", meaning: "migration required" },
  { code: 9, variant: "EvalFailure", meaning: "evaluation found regressions" },
  { code: 10, variant: "WorkspaceStoreMissing", meaning: "workspace store missing" },
  { code: 130, variant: "Cancelled", meaning: "cancelled" },
];

// ─── The repair safety rule (src/core/preflight_guard.rs) ───────────────────

/** RepairActionRiskClass::as_str (src/models/mod.rs), in the source's order. */
export const RISK_CLASSES = [
  "read_only_probe",
  "idempotent_refresh",
  "mutating_local_repair",
  "mutating_external_coordination_repair",
  "approval_required_repair",
  "destructive_or_irreversible_repair",
  "unavailable_or_manual_only",
] as const;

export type RiskClass = (typeof RISK_CLASSES)[number];

/** RepairCommandNextAction::as_str (src/core/preflight_guard.rs). */
export type NextAction = "run_directly" | "run_preflight_first" | "coordinate_first" | "ask_human" | "manual_only";

/**
 * repair_command_preflight_policy, line for line: what an agent should do with a repair, from
 * its risk class, whether a preflight command came with it, and whether a human must approve.
 */
export function nextAction(riskClass: RiskClass, hasPreflight: boolean, requiresHumanApproval: boolean): NextAction {
  switch (riskClass) {
    case "read_only_probe":
      return "run_directly";
    case "idempotent_refresh":
    case "mutating_local_repair":
      return requiresHumanApproval ? "ask_human" : hasPreflight ? "run_preflight_first" : "run_directly";
    case "mutating_external_coordination_repair":
      return requiresHumanApproval ? "ask_human" : "coordinate_first";
    case "approval_required_repair":
    case "destructive_or_irreversible_repair":
      return "ask_human";
    case "unavailable_or_manual_only":
      return "manual_only";
  }
}

/** The same rule as a table to read: each class, and where it leads. */
export const RULE_ROWS: readonly { riskClass: RiskClass; leads: string }[] = [
  { riskClass: "read_only_probe", leads: "run it" },
  { riskClass: "idempotent_refresh", leads: "run it · preflight if given · ask if approval" },
  { riskClass: "mutating_local_repair", leads: "run it · preflight if given · ask if approval" },
  { riskClass: "mutating_external_coordination_repair", leads: "coordinate first · ask if approval" },
  { riskClass: "approval_required_repair", leads: "ask a human" },
  { riskClass: "destructive_or_irreversible_repair", leads: "ask a human" },
  { riskClass: "unavailable_or_manual_only", leads: "manual only" },
];

// ─── The situations ─────────────────────────────────────────────────────────

export type SituationId = "index" | "store" | "migration" | "budget" | "model" | "cursor";

export type Verdict = "retry" | "recheck" | "preflight" | "adjust" | "proceed" | "fresh" | "ask";

export const VERDICT: Record<Verdict, { label: string; tone: string }> = {
  retry: { label: "Retry automatically", tone: "border-emerald-400/45 bg-emerald-400/15 text-emerald-100" },
  recheck: { label: "Re-check the path", tone: "border-sky-400/45 bg-sky-400/15 text-sky-100" },
  preflight: { label: "Preflight, then migrate", tone: "border-amber-400/45 bg-amber-400/15 text-amber-100" },
  adjust: { label: "Raise the budget, re-run", tone: "border-cyan-400/45 bg-cyan-400/15 text-cyan-100" },
  proceed: { label: "Proceed, and report the degradation", tone: "border-amber-400/45 bg-amber-400/15 text-amber-100" },
  fresh: { label: "Start a fresh page sequence", tone: "border-violet-400/45 bg-violet-400/15 text-violet-100" },
  ask: { label: "Ask a human", tone: "border-rose-400/45 bg-rose-400/15 text-rose-100" },
};

export type Situation = {
  id: SituationId;
  label: string;
  short: string;
  exit: number;
  /** The command that produced the record, when the record says. */
  invocation: string | null;
  /** What a typical CLI prints instead: invented for the contrast, and labelled so on the page. */
  typical: { lines: string[]; exit: number; missing: string };
  record: Json;
  /** Where the record comes from, and how it is checked. */
  source: {
    kind: "pinned" | "source";
    file: string;
    pinnedBy?: string;
    files?: string[];
    /** For a record built from the source: what the files settle and what they leave open. */
    note?: string;
  };
  /** Path to the object carrying `code` and `severity`. */
  entry: (string | number)[];
  /** Path to `recovery[]`, when there is one. */
  recovery: (string | number)[] | null;
  /** Extra paths step 3 lights (evidence beyond code and severity). */
  evidence: string[];
  /** The next action ee's rule gives the first recovery entry (checked by the test). */
  expectNext: NextAction | null;
  reading: {
    envelope: string;
    broke: string;
    plan: string;
    safety: string;
    act: { commands: string[]; note: string };
    recheck: string;
  };
  verdict: Verdict;
};

export const SITUATIONS: readonly Situation[] = [
  {
    id: "index",
    label: "Index behind the store",
    short: "Index",
    exit: 4,
    invocation: null,
    typical: {
      lines: ["Error: search index is out of date.", "Please rebuild the index and try again."],
      exit: 1,
      missing: "The message names no command and does not say whether a rebuild is safe to run unattended.",
    },
    record: {
      schema: "ee.error.v2",
      error: {
        code: "search_index",
        message: "Index is stale (generation 9, database generation 12).",
        severity: "medium",
        repair: "ee index rebuild",
        repairKind: "actionable",
        details: {
          recovery: [
            {
              priority: 1,
              kind: "migration",
              rationale: "Rebuild the index from current memory state; idempotent.",
              riskClass: "idempotent_refresh",
              requiresHumanApproval: false,
              mutatesExternalState: false,
              mutatesTrackerState: false,
              privacyClass: "bounded_command_no_raw_state",
              evidence: ["derived_asset_refresh_command"],
              preconditions: ["workspace_must_be_explicit"],
              command: "ee index rebuild --workspace .",
            },
            {
              priority: 2,
              kind: "migration",
              rationale: "Inspect index state before rebuilding (faster diagnosis).",
              riskClass: "read_only_probe",
              requiresHumanApproval: false,
              mutatesExternalState: false,
              mutatesTrackerState: false,
              privacyClass: "bounded_command_no_raw_state",
              evidence: ["read_only_probe_command"],
              command: "ee index status --workspace . --json",
            },
          ],
        },
      },
    },
    source: {
      kind: "pinned",
      file: "tests/snapshots/error_envelope_golden__error_envelope_search_index.snap",
      pinnedBy: "tests/error_envelope_golden.rs · error_envelope_search_index",
    },
    entry: ["error"],
    recovery: ["error", "details", "recovery"],
    evidence: ["error/message"],
    expectNext: "run_directly",
    reading: {
      envelope: "schema is ee.error.v2, so the command failed and the details are in the error object.",
      broke: "The index is built from the database and is three generations behind it (9 against 12). Severity is medium; no data is lost.",
      plan: "Priority 1 rebuilds the index and priority 2 only inspects it. The structured command adds --workspace ., which the repair string leaves out.",
      safety: "An idempotent refresh with no approval and no preflight command. ee's rule says to run it directly, and a second run does no harm.",
      act: { commands: ["ee index rebuild --workspace ."], note: "then re-run the command that failed, unchanged" },
      recheck: "Exit 0, and no index code left in degraded[].",
    },
    verdict: "retry",
  },
  {
    id: "store",
    label: "Wrong workspace path",
    short: "Path",
    exit: 10,
    invocation: 'ee remember "…" --workspace /workspace/missing --json',
    typical: {
      lines: ["Error: database not found.", "Run `init` to create one."],
      exit: 1,
      missing: "Following that advice creates a new, empty store at a path that may be a typo.",
    },
    record: {
      schema: "ee.error.v2",
      error: {
        code: "workspace_store_missing",
        message: "Database not found at /workspace/missing/.ee/ee.db",
        severity: "high",
        repair:
          "Re-check --workspace addressing with --workspace /workspace/missing (looked for /workspace/missing/.ee/ee.db). Nearby-store discovery completed and found no populated stores. Only if you intended to create a NEW store here: ee init --workspace /workspace/missing",
        repairKind: "unknown",
        details: {
          addressedStorePath: "/workspace/missing/.ee/ee.db",
          addressedWorkspacePath: "/workspace/missing",
          storeDiscovery: {
            outcome: "complete",
            nearbyStores: [],
          },
          recovery: [
            {
              priority: 1,
              kind: "flag",
              rationale: "Re-check the exact addressed workspace before selecting another store.",
              riskClass: "read_only_probe",
              requiresHumanApproval: false,
              mutatesExternalState: false,
              mutatesTrackerState: false,
              privacyClass: "metadata_only",
              evidence: ["recovery_kind_without_command"],
              valueHint: "/workspace/missing",
              flagName: "--workspace",
              example: "--workspace /workspace/missing",
            },
            {
              priority: 2,
              kind: "seed",
              rationale: "Only initialize when this exact addressed workspace was intentionally chosen for a new store.",
              riskClass: "mutating_local_repair",
              preflightCommand: "ee preflight check --cmd 'ee init --workspace /workspace/missing' --workspace . --json",
              requiresHumanApproval: false,
              mutatesExternalState: false,
              mutatesTrackerState: false,
              privacyClass: "bounded_command_no_raw_state",
              manualStep: "Review the dry-run or recovery details before applying.",
              evidence: ["local_state_repair_command"],
              preconditions: ["workspace_must_be_explicit"],
              command: "ee init --workspace /workspace/missing",
            },
          ],
        },
      },
    },
    source: {
      kind: "pinned",
      file: "tests/fixtures/golden/error/workspace_store_missing.golden",
      pinnedBy:
        "tests/error_envelope_golden.rs · error_envelope_workspace_store_missing_matches_required_fixture; exit 10 from ee remember: tests/exit_code_conformance.rs",
    },
    entry: ["error"],
    recovery: ["error", "details", "recovery"],
    evidence: ["error/details/storeDiscovery"],
    expectNext: "run_directly",
    reading: {
      envelope: "schema is ee.error.v2, so the command failed and the details are in the error object.",
      broke: "Exit 10 means the addressed workspace has no store; a storage failure would be exit 3. Discovery completed and found no populated store nearby.",
      plan: "Re-check the workspace path first. ee init is last, and only for a store you meant to create.",
      safety: "A read-only probe. The entry is a flag to re-check and carries no command.",
      act: {
        commands: ["--workspace /workspace/missing"],
        note: "compare it with the workspace the task names, and re-run with the right one",
      },
      recheck: "Exit 0 from the intended store, with no ee init unless this path was meant to hold a new store.",
    },
    verdict: "recheck",
  },
  {
    id: "migration",
    label: "Migration required",
    short: "Migration",
    exit: 8,
    invocation: null,
    typical: {
      lines: ["Error: unsupported database version."],
      exit: 1,
      missing: "The message names no upgrade command and does not say whether the upgrade can be undone.",
    },
    record: {
      schema: "ee.error.v2",
      error: {
        code: "migration_required",
        message: "Database schema is at version 5, current is 7.",
        severity: "medium",
        repair: "ee migrate run",
        repairKind: "actionable",
        details: {
          recovery: [
            {
              priority: 1,
              kind: "migration",
              rationale: "Apply outstanding migrations; idempotent and audit-logged.",
              riskClass: "mutating_local_repair",
              preflightCommand: "ee preflight check --cmd 'ee migrate run' --workspace . --json",
              requiresHumanApproval: false,
              mutatesExternalState: false,
              mutatesTrackerState: false,
              privacyClass: "bounded_command_no_raw_state",
              manualStep: "Review the dry-run or recovery details before applying.",
              evidence: ["local_state_repair_command"],
              preconditions: ["workspace_must_be_explicit"],
              command: "ee migrate run",
            },
          ],
        },
      },
    },
    source: {
      kind: "pinned",
      file: "tests/snapshots/error_envelope_golden__error_envelope_migration_required.snap",
      pinnedBy: "tests/error_envelope_golden.rs · error_envelope_migration_required",
    },
    entry: ["error"],
    recovery: ["error", "details", "recovery"],
    evidence: ["error/message"],
    expectNext: "run_preflight_first",
    reading: {
      envelope: "schema is ee.error.v2, so the command failed and the details are in the error object.",
      broke: "The store's schema is two versions behind this binary (5 against 7). Severity is medium; the data is intact, but this command cannot run on it.",
      plan: "The plan has one entry: apply the outstanding migrations.",
      safety: "It changes durable local state, so ee attaches a preflight command. No human approval is required.",
      act: {
        commands: ["ee preflight check --cmd 'ee migrate run' --workspace . --json", "ee migrate run"],
        note: "the preflight reports risk and never blocks",
      },
      recheck: "Re-run the command that failed and expect exit 0.",
    },
    verdict: "preflight",
  },
  {
    id: "budget",
    label: "Budget too small",
    short: "Budget",
    exit: 0,
    invocation: "ee pack 'release ritual' --workspace . --max-tokens 1 --json",
    typical: {
      lines: [],
      exit: 0,
      missing: "The empty answer looks like nothing matched, when the budget was too small to fit anything.",
    },
    record: {
      schema: "ee.response.v2",
      success: true,
      data: { [ELIDED]: true },
      degraded: [
        {
          code: "pack_budget_too_small",
          severity: "warning",
          message: "Pack budget could not fit any candidate. Items=0, pool=<pool>, used_tokens=<used>/1.",
          sources: ["pack"],
          details: {
            recovery: [
              {
                priority: 1,
                kind: "flag",
                rationale: "Most common cause; raise budget until a section fits.",
                riskClass: "read_only_probe",
                requiresHumanApproval: false,
                mutatesExternalState: false,
                mutatesTrackerState: false,
                privacyClass: "metadata_only",
                evidence: ["recovery_kind_without_command"],
                valueHint: "8000",
                flagName: "--max-tokens",
              },
              {
                priority: 2,
                kind: "flag",
                rationale: "Switch to the lowest-quota profile if budget can't be raised.",
                riskClass: "read_only_probe",
                requiresHumanApproval: false,
                mutatesExternalState: false,
                mutatesTrackerState: false,
                privacyClass: "metadata_only",
                evidence: ["recovery_kind_without_command"],
                valueHint: "compact",
                flagName: "--profile",
              },
              {
                priority: 3,
                kind: "broaden",
                rationale: "Candidate pool may be too narrow; broaden query text or relax EQL tag filters.",
                riskClass: "read_only_probe",
                requiresHumanApproval: false,
                mutatesExternalState: false,
                mutatesTrackerState: false,
                privacyClass: "metadata_only",
                evidence: ["recovery_kind_without_command"],
              },
            ],
          },
        },
        { [ELIDED]: true },
      ],
    },
    source: {
      kind: "source",
      file: "tests/fixtures/failure_modes/pack_budget_too_small.json",
      files: [
        "tests/fixtures/failure_modes/pack_budget_too_small.json",
        "src/core/context.rs",
        "src/output/mod.rs",
        "src/models/mod.rs",
      ],
      pinnedBy: "tests/pack_budget_too_small_test.rs (exit 0, an empty pack, the three recovery entries)",
      note:
        "The test pins this entry in data.degraded; the top-level degraded[] mirrors it (normalize_response_degradation). Where it falls among the pack's other entries is not pinned.",
    },
    entry: ["degraded", 0],
    recovery: ["degraded", 0, "details", "recovery"],
    evidence: ["degraded/0/message"],
    expectNext: "run_directly",
    reading: {
      envelope: "success is true, so the agent reads degraded[] before it uses the result.",
      broke: "The pack is empty because none of the candidates fit in a one-token budget. Severity is warning.",
      plan: "Raise the budget first, then switch to the compact profile, then broaden the query.",
      safety: "The entry is a flag with no command. The agent re-runs its own read with a larger budget.",
      act: { commands: ["ee pack 'release ritual' --workspace . --max-tokens 8000 --json"], note: "priority 1's flag and value" },
      recheck: "pack.items is no longer empty, and pack_budget_too_small is gone from degraded[].",
    },
    verdict: "adjust",
  },
  {
    id: "model",
    label: "No embedding model",
    short: "Model",
    exit: 0,
    invocation: "ee search 'cargo test before release' --json",
    typical: {
      lines: ["Found 3 results."],
      exit: 0,
      missing: "The output does not say that semantic search was off, so the reader cannot tell the ranking is keyword-only.",
    },
    record: {
      schema: "ee.response.v2",
      success: true,
      data: { command: "search", resultCount: 3, [ELIDED]: true },
      degraded: [
        {
          code: "embed_model_unavailable",
          severity: "warning",
          message:
            "Embedding model unavailable (active embedder source frankensearch_hash_fallback reports semantic=false); semantic similarity is disabled and lexical search remains available.",
          repair: "ee index reembed --workspace .",
          sources: ["search"],
          details: {
            recovery: [
              {
                priority: 1,
                kind: "rebuild",
                rationale: "The embedder model is unloadable; re-embed forces a fresh initialization.",
                riskClass: "idempotent_refresh",
                requiresHumanApproval: false,
                mutatesExternalState: false,
                mutatesTrackerState: false,
                privacyClass: "bounded_command_no_raw_state",
                evidence: ["derived_asset_refresh_command"],
                preconditions: ["workspace_must_be_explicit", "toolchain_or_feature_profile_must_match_request"],
                command: "ee index reembed --workspace .",
                resultsIn: "Rebuilds the embedding index against the current embed-fast feature and model configuration.",
              },
              {
                priority: 2,
                kind: "rebuild",
                rationale:
                  "If this binary was built without the dense embedder, rebuild with the supported embed-fast feature.",
                riskClass: "mutating_local_repair",
                requiresHumanApproval: false,
                mutatesExternalState: false,
                mutatesTrackerState: false,
                privacyClass: "bounded_command_no_raw_state",
                evidence: ["local_toolchain_or_install_command"],
                preconditions: ["use_rch_for_cargo_when_applicable", "toolchain_or_feature_profile_must_match_request"],
                command: "cargo build --features embed-fast",
              },
            ],
            modelId: "EE_EMBED_MODEL_PATH",
            featureFlag: "embed-fast",
            lexicalAvailable: true,
          },
        },
      ],
    },
    source: {
      kind: "pinned",
      file: "tests/fixtures/golden/agent/search_deterministic_ranking.json.golden",
      pinnedBy: "tests/search_deterministic_golden.rs",
    },
    entry: ["degraded", 0],
    recovery: ["degraded", 0, "details", "recovery"],
    evidence: ["degraded/0/details/lexicalAvailable", "data/resultCount"],
    expectNext: "run_directly",
    reading: {
      envelope: "success is true, so the agent reads degraded[] before it uses the result.",
      broke: "Semantic similarity is off, lexicalAvailable is true, and 3 results came back. They are usable, ranked by lexical match only.",
      plan: "Re-embed first. Rebuilding the binary comes second, and only if it was built without the embedder.",
      safety: "An idempotent refresh that is safe to run. The 3 results can be used now, so the answer does not wait for it.",
      act: {
        commands: ["ee index reembed --workspace ."],
        note: "run it after answering, and say in the answer that semantic search was off",
      },
      recheck: "The next search's degraded[] no longer lists embed_model_unavailable.",
    },
    verdict: "proceed",
  },
  {
    id: "cursor",
    label: "Stale page cursor",
    short: "Cursor",
    exit: 0,
    invocation: "ee schema list --cursor <captured-cursor> --max-output-tokens 150 --json",
    typical: {
      lines: ["Showing results 1–20 of 84"],
      exit: 0,
      missing: "The listing restarted at page one without saying so, so the agent reads and counts those items twice.",
    },
    record: {
      schema: "ee.response.v2",
      success: true,
      data: { [ELIDED]: true },
      degraded: [
        {
          code: "cursor_stale",
          severity: "low",
          message:
            "Continuation cursor was issued at DB generation <n> but the workspace is now at generation <m>; pages cannot partition the result set honestly across writes.",
          repair: "Re-run the command without --cursor to start a fresh page sequence.",
        },
      ],
    },
    source: {
      kind: "source",
      file: "tests/fixtures/failure_modes/cursor_stale.json",
      files: ["tests/fixtures/failure_modes/cursor_stale.json", "src/output/governor.rs", "src/models/mod.rs", "src/output/mod.rs"],
      note:
        "cursor_stale_degraded_entry, appended after any entry the command raised (append_degraded_entry); the page array is emptied (empty_truncation_point).",
    },
    entry: ["degraded", 0],
    recovery: null,
    evidence: ["degraded/0/message"],
    expectNext: null,
    reading: {
      envelope: "success is true, so the agent reads degraded[] before it uses the result.",
      broke: "A write advanced the store's generation between pages. ee returns an empty page, so the sequence neither restarts nor repeats items.",
      plan: "There is no recovery[], so the agent runs the repair as written and does not make one up.",
      safety: "There is no riskClass. The repair is ee's exact instruction, and it only re-runs a read.",
      act: { commands: ["ee schema list --max-output-tokens 150 --json"], note: "the same command, without --cursor" },
      recheck: "A new first page and a new continuationCursor, both from one database generation.",
    },
    verdict: "fresh",
  },
];

/** The value at `path` in `json`, or undefined. */
export function at(json: Json, path: readonly (string | number)[]): Json | undefined {
  let node: Json | undefined = json;
  for (const key of path) {
    if (node === null || node === undefined || typeof node !== "object") return undefined;
    node = Array.isArray(node) ? node[key as number] : (node as Record<string, Json>)[key as string];
  }
  return node;
}

/** The recovery entries of a situation, sorted by ascending priority (lower runs first). */
export function sortedRecovery(situation: Situation): { index: number; entry: Record<string, Json> }[] {
  if (!situation.recovery) return [];
  const list = at(situation.record, situation.recovery);
  if (!Array.isArray(list)) return [];
  return list
    .map((entry, index) => ({ index, entry: entry as Record<string, Json> }))
    .filter(({ entry }) => typeof entry.priority === "number")
    .sort((a, b) => (a.entry.priority as number) - (b.entry.priority as number) || a.index - b.index);
}

/** The first entry to act on, and what ee's rule makes of it. */
export function firstAction(situation: Situation):
  | { index: number; entry: Record<string, Json>; riskClass: RiskClass; next: NextAction }
  | null {
  const first = sortedRecovery(situation)[0];
  if (!first) return null;
  const riskClass = first.entry.riskClass as RiskClass;
  const next = nextAction(riskClass, typeof first.entry.preflightCommand === "string", first.entry.requiresHumanApproval === true);
  return { ...first, riskClass, next };
}
