#!/usr/bin/env bash
# bridge-staleness.sh — advisory gate that detects when
# CLOSE_THE_GAP_PLAN.md has drifted into "Part III needed" territory
# (bd-3usjw.33 / CLOSE_THE_GAP §36).
#
# Three signals are evaluated against the live tree and the live
# .vision-coverage-report.json:
#
#   1. plan_mtime_age_days — mtime of CLOSE_THE_GAP_PLAN.md older
#      than 30 days. Severity: medium. Trigger phrase: "bridge plan
#      mtime exceeds 30 day staleness budget".
#
#   2. vision_coverage_gap_low — .vision-coverage-report.json
#      gap_percentage < 2%. Severity: low. Phase-aware since
#      bd-reality-core-convergence-1azkt.35: a low gap means documented
#      files and registrations are present, which is METADATA coverage,
#      not product completion. The advice therefore follows the active
#      plan's declared phase:
#        - ACTIVE  -> metadata-complete / behavior-unproven, naming the
#                     owner and its open requirement counts. Never
#                     recommends authoring the part that is already
#                     active (AGENTS.md forbids a competing successor).
#        - ARCHIVED -> may recommend a successor, but only when no
#                     requirements under the plan's owner are still live.
#        - unknown  -> inconclusive; never "closed".
#
#   3. in_progress_beads_mtime — requirements under the ACTIVE plan's own
#      bead root with no tracker movement for > 7 days. Severity: medium.
#      Falls back to the historical reality-check-2026-05-14 / wave-4
#      label set only when the plan names no owner. Counting a bridge
#      that closed months ago was the original defect: it reported zero
#      live work and concluded the current bridge was finished.
#
# Phase and requirement ownership are read from the plan document itself
# (its `**Status: ...**` marker and the bead root it references most), so
# there is exactly one active-bridge contract and no second manifest.
#
# Output: .bridge-staleness-report.json with schema
# 'ee.bridge.staleness.v1', signals[], generated_at,
# data_hash.
#
# Exit code: always 0 (advisory only). Non-blocking by design.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# Active bridge plan lives at CLOSE_THE_GAP_PLAN.md. When archived
# (e.g. docs/archive/close_the_gap_<YYYY-MM>.md), the active slot is
# missing — bridge-staleness then degrades signal 1 cleanly and the
# vision-coverage / Part II signals continue to advise authorship of
# the next bridge part.
PLAN_PATH="${ROOT}/CLOSE_THE_GAP_PLAN.md"
VISION_REPORT="${ROOT}/.vision-coverage-report.json"
BEADS_JSONL="${ROOT}/.beads/issues.jsonl"
OUTPUT_PATH="${ROOT}/.bridge-staleness-report.json"

JSON_FLAG=""
QUIET_FLAG=""
SELF_TEST=""

require_flag_value() {
  local flag="$1"
  if [ "$#" -lt 2 ] || [ -z "${2:-}" ] || [[ "${2:-}" == --* ]]; then
    echo "error: ${flag} requires a path" >&2
    exit 2
  fi
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --json)
      JSON_FLAG="1"
      shift
      ;;
    --quiet)
      QUIET_FLAG="1"
      shift
      ;;
    --self-test)
      SELF_TEST="1"
      shift
      ;;
    --plan)
      require_flag_value "$@"
      PLAN_PATH="$2"
      shift 2
      ;;
    --vision)
      require_flag_value "$@"
      VISION_REPORT="$2"
      shift 2
      ;;
    --beads)
      require_flag_value "$@"
      BEADS_JSONL="$2"
      shift 2
      ;;
    --output)
      require_flag_value "$@"
      OUTPUT_PATH="$2"
      shift 2
      ;;
    --help)
      cat <<'USAGE'
Usage: scripts/bridge-staleness.sh [--json] [--quiet] [--self-test] [--plan <path>] [--vision <path>] [--beads <path>] [--output <path>]

  --json   Emit only the JSON report to stdout; diagnostics on stderr.
  --quiet  Suppress human-readable summary (still writes JSON to disk).
  --self-test Run synthetic bridge-staleness fixture checks without reading the workspace.
  --plan <path>   Read bridge plan mtime from this path.
  --vision <path> Read vision coverage JSON from this path.
  --beads <path>  Read bead records from this JSONL path.
  --output <path> Write the JSON report to this path.

Reads:
  CLOSE_THE_GAP_PLAN.md            (plan mtime check)
  .vision-coverage-report.json     (gap-percentage check)
  .beads/issues.jsonl              (Part II in-progress mtime check)

Writes:
  .bridge-staleness-report.json    (always, regardless of --json)

Exit code: always 0 (advisory gate).
USAGE
      exit 0
      ;;
    *)
      echo "unknown argument: $1" >&2
      exit 1
      ;;
  esac
done

now_epoch=$(date +%s)
generated_at=$(date -u +"%Y-%m-%dT%H:%M:%SZ")

assert_report_jq() {
  local report="$1"
  local filter="$2"
  local message="$3"

  if ! printf '%s\n' "$report" | jq -e "$filter" >/dev/null; then
    echo "error: bridge-staleness self-test failed: $message" >&2
    echo "       jq filter: $filter" >&2
    return 1
  fi
}

run_self_test() {
  local report
  report=$(bash "${BASH_SOURCE[0]}" --json --quiet --output /dev/null \
    --plan <(cat <<'PLAN'
# Synthetic Active Bridge

Fresh plan content is enough for the self-test because process substitutions
carry a current mtime and should not trigger plan_mtime_age_days.
PLAN
    ) \
    --vision <(printf '%s\n' '{"gap_percentage":1.5}') \
    --beads <(cat <<'JSONL'
{"id":"bd-bridge.self-open","status":"open","labels":["wave-4"],"created_at":"2000-01-01T00:00:00Z","updated_at":"2000-01-01T00:00:00Z"}
{"id":"bd-bridge.self-ignored","status":"closed","labels":["wave-4"],"created_at":"2000-01-01T00:00:00Z","updated_at":"2000-01-01T00:00:00Z"}
JSONL
    ))

  assert_report_jq "$report" '.schema == "ee.bridge.staleness.v1"' "schema mismatch"
  assert_report_jq "$report" '.inputs.planPresent == true' "synthetic plan should be present"
  assert_report_jq "$report" '.inputs.visionCoverageReportPresent == true' "synthetic vision report should be present"
  assert_report_jq "$report" '.inputs.partIIOpenCount == 1' "expected one stale open Part II bead"
  assert_report_jq "$report" '.inputs.partIIInProgressCount == 0' "expected no in-progress Part II beads"
  assert_report_jq "$report" '.inputs.partIIMaxStaleDays > 7' "expected stale Part II age above threshold"
  assert_report_jq "$report" '(.signals | map(.code) | index("vision_coverage_gap_low")) != null' "missing low vision-gap signal"
  assert_report_jq "$report" '(.signals | map(.code) | index("in_progress_beads_mtime")) != null' "missing Part II inactivity signal"
  assert_report_jq "$report" '(.signals | map(.severity) | index("low")) != null' "missing low-severity signal"
  assert_report_jq "$report" '(.signals | map(.severity) | index("medium")) != null' "missing medium-severity signal"

  local quiet_report
  quiet_report=$(bash "${BASH_SOURCE[0]}" --json --quiet --output /dev/null \
    --plan <(printf '%s\n' '# Synthetic Active Bridge') \
    --vision <(printf '%s\n' '{"gap_percentage":5.0}') \
    --beads <(cat <<'JSONL'
{"id":"bd-bridge.self-active","status":"in_progress","labels":["reality-check-2026-05-14"],"created_at":"2000-01-01T00:00:00Z","updated_at":"2000-01-01T00:00:00Z"}
JSONL
    ))
  assert_report_jq "$quiet_report" '.signals | length == 0' "active Part II or high vision gap should not emit advisory signals"

  local malformed_gap_report
  malformed_gap_report=$(bash "${BASH_SOURCE[0]}" --json --quiet --output /dev/null \
    --plan <(printf '%s\n' '# Synthetic Active Bridge') \
    --vision <(printf '%s\n' '{"gap_percentage":"1); system(\"false\")"}') \
    --beads <(printf '%s\n' ''))
  assert_report_jq "$malformed_gap_report" '.inputs.visionCoverageReportPresent == true' "malformed vision report should still be recorded as present"
  assert_report_jq "$malformed_gap_report" '(.signals | map(.code) | index("vision_coverage_gap_low")) == null' "malformed vision gap should not emit low-gap signal"

  # ── Phase-aware advice (bd-reality-core-convergence-1azkt.35) ─────────────
  #
  # Table-driven over the four phase/maturity combinations. The companion
  # harness in .36 owns retained stdout/stderr artifacts; these cases prove the
  # classification itself, and they run in verify.sh Gate 3.59.

  # Case A — ACTIVE part with live requirements. The original defect: a low
  # metadata gap advised authoring the very part that was already active.
  local active_report
  active_report=$(bash "${BASH_SOURCE[0]}" --json --quiet --output /dev/null \
    --plan <(cat <<'PLAN'
# Synthetic Bridge

> **Status: ACTIVE (Part III).** Requirements are tracked under
> bd-synthetic-owner.1, bd-synthetic-owner.2 and bd-synthetic-owner.3.
PLAN
    ) \
    --vision <(printf '%s\n' '{"gap_percentage":0.5}') \
    --beads <(cat <<'JSONL'
{"id":"bd-synthetic-owner.1","status":"open","labels":[],"created_at":"2026-09-01T00:00:00Z","updated_at":"2026-09-01T00:00:00Z"}
{"id":"bd-synthetic-owner.2","status":"in_progress","labels":[],"created_at":"2026-09-01T00:00:00Z","updated_at":"2026-09-01T00:00:00Z"}
{"id":"bd-synthetic-owner.3","status":"closed","labels":[],"created_at":"2026-09-01T00:00:00Z","updated_at":"2026-09-01T00:00:00Z"}
JSONL
    ))
  assert_report_jq "$active_report" '.inputs.activePlanStatus == "active"' "active plan status must parse"
  assert_report_jq "$active_report" '.inputs.activePlanPhase == "Part III"' "active plan phase must parse"
  assert_report_jq "$active_report" '.inputs.activeOwnerRoot == "bd-synthetic-owner"' "owner root must come from the plan, not a frozen label set"
  assert_report_jq "$active_report" '.inputs.activeOwnerOpenCount == 1' "one open requirement expected"
  assert_report_jq "$active_report" '.inputs.activeOwnerInProgressCount == 1' "one in-progress requirement expected"
  # Acceptance 1: never advise authoring the phase that is already active.
  assert_report_jq "$active_report" \
    '[.signals[] | select(.code == "vision_coverage_gap_low")] | length == 1' \
    "active plan should still report the low-gap signal"
  assert_report_jq "$active_report" \
    '(.signals[] | select(.code == "vision_coverage_gap_low") | .message | test("consider planning"; "i")) | not' \
    "active plan must not be advised to plan its own already-active part"
  # Acceptance 2: metadata-complete/behavior-unproven, never finished.
  assert_report_jq "$active_report" \
    '.signals[] | select(.code == "vision_coverage_gap_low") | .message | test("behavior unproven")' \
    "low metadata gap on an active plan must read as behavior-unproven"
  assert_report_jq "$active_report" \
    '.signals[] | select(.code == "vision_coverage_gap_low") | .message | test("substantially closed"; "i") | not' \
    "metadata coverage must never be reported as bridge closure"
  # Acceptance 3: name the affected owner and the next action.
  assert_report_jq "$active_report" \
    '.signals[] | select(.code == "vision_coverage_gap_low") | .repair | test("bd-synthetic-owner")' \
    "advice must name the affected requirement owner"
  assert_report_jq "$active_report" \
    '.signals[] | select(.code == "vision_coverage_gap_low") | .details.activeOwnerOpenCount == 1' \
    "signal details must carry the owner maturity counts"

  # Case B — ARCHIVED with no live requirements: a successor may be scoped,
  # and only here (acceptance 4).
  local archived_clear_report
  archived_clear_report=$(bash "${BASH_SOURCE[0]}" --json --quiet --output /dev/null \
    --plan <(cat <<'PLAN'
# Synthetic Bridge

> **Status: ARCHIVED (2026-05 bridge).** Closed requirements were tracked under
> bd-synthetic-owner.1, bd-synthetic-owner.2 and bd-synthetic-owner.3.
PLAN
    ) \
    --vision <(printf '%s\n' '{"gap_percentage":0.5}') \
    --beads <(cat <<'JSONL'
{"id":"bd-synthetic-owner.1","status":"closed","labels":[],"created_at":"2026-09-01T00:00:00Z","updated_at":"2026-09-01T00:00:00Z"}
JSONL
    ))
  assert_report_jq "$archived_clear_report" '.inputs.activePlanStatus == "archived"' "archived plan status must parse"
  assert_report_jq "$archived_clear_report" \
    '.signals[] | select(.code == "vision_coverage_gap_low") | .message | test("successor part may be scoped")' \
    "a completed archived bridge may recommend a successor"

  # Case C — ARCHIVED but requirements are still live: completion evidence is
  # incomplete, so no successor recommendation (acceptance 4's negative).
  local archived_live_report
  archived_live_report=$(bash "${BASH_SOURCE[0]}" --json --quiet --output /dev/null \
    --plan <(cat <<'PLAN'
# Synthetic Bridge

> **Status: ARCHIVED (2026-05 bridge).** Requirements were tracked under
> bd-synthetic-owner.1, bd-synthetic-owner.2 and bd-synthetic-owner.3.
PLAN
    ) \
    --vision <(printf '%s\n' '{"gap_percentage":0.5}') \
    --beads <(cat <<'JSONL'
{"id":"bd-synthetic-owner.1","status":"open","labels":[],"created_at":"2026-09-01T00:00:00Z","updated_at":"2026-09-01T00:00:00Z"}
JSONL
    ))
  assert_report_jq "$archived_live_report" \
    '.signals[] | select(.code == "vision_coverage_gap_low") | .message | test("completion evidence is incomplete")' \
    "an archived plan with live requirements must not claim completion"
  assert_report_jq "$archived_live_report" \
    '(.signals[] | select(.code == "vision_coverage_gap_low") | .message | test("successor part may be scoped")) | not' \
    "an archived plan with live requirements must not recommend a successor"

  # Case D — no readable status marker: honest inconclusive, never closed.
  local unknown_report
  unknown_report=$(bash "${BASH_SOURCE[0]}" --json --quiet --output /dev/null \
    --plan <(printf '%s\n' '# Synthetic Bridge with no status marker') \
    --vision <(printf '%s\n' '{"gap_percentage":0.5}') \
    --beads <(printf '%s\n' ''))
  assert_report_jq "$unknown_report" '.inputs.activePlanStatus == "unknown"' "missing marker must resolve to unknown"
  assert_report_jq "$unknown_report" '.inputs.activePlanPhase == null' "missing marker must not invent a phase"
  assert_report_jq "$unknown_report" \
    '.signals[] | select(.code == "vision_coverage_gap_low") | .message | test("inconclusive")' \
    "unreadable plan status must yield inconclusive advice"
  assert_report_jq "$unknown_report" \
    '(.signals[] | select(.code == "vision_coverage_gap_low") | .message | test("closed"; "i")) | not' \
    "unreadable plan status must never be reported as closed"

  # Case E — an incidental bead mention is not an owner. Below the mention
  # floor the field stays null rather than guessing.
  local weak_owner_report
  weak_owner_report=$(bash "${BASH_SOURCE[0]}" --json --quiet --output /dev/null \
    --plan <(cat <<'PLAN'
# Synthetic Bridge

> **Status: ACTIVE (Part IV).** See bd-passing-mention.1 for background.
PLAN
    ) \
    --vision <(printf '%s\n' '{"gap_percentage":0.5}') \
    --beads <(printf '%s\n' ''))
  assert_report_jq "$weak_owner_report" '.inputs.activeOwnerRoot == null' "a single incidental mention must not become the owner"
  assert_report_jq "$weak_owner_report" '.inputs.activePlanPhase == "Part IV"' "phase parsing must not be hard-coded to Part III"

  echo "ok: bridge-staleness self-test passed"
}

if [ -n "$SELF_TEST" ]; then
  run_self_test
  exit 0
fi

signals_json=""

# Bounded read of the plan document; see the active-bridge contract below.
PLAN_READ_LIMIT_BYTES=262144
# Minimum mentions before a bead root counts as the plan's requirement owner.
ACTIVE_OWNER_MIN_MENTIONS=3

# Signal 1: plan mtime age.
plan_present=false
plan_age_days=0
plan_status="unknown"
plan_phase=""
active_owner_root=""
active_owner_open_count=0
active_owner_in_progress_count=0
active_owner_max_stale_days=0
if [ -r "$PLAN_PATH" ]; then
  plan_present=true
  mtime_epoch=""
  if mtime_candidate=$(stat -f %m "$PLAN_PATH" 2>/dev/null) && [[ "$mtime_candidate" =~ ^[0-9]+$ ]]; then
    mtime_epoch="$mtime_candidate"
  elif mtime_candidate=$(stat -c %Y "$PLAN_PATH" 2>/dev/null) && [[ "$mtime_candidate" =~ ^[0-9]+$ ]]; then
    mtime_epoch="$mtime_candidate"
  else
    mtime_epoch="$now_epoch"
  fi
  plan_age_days=$(( (now_epoch - mtime_epoch) / 86400 ))

  # Active-bridge contract (bd-reality-core-convergence-1azkt.35).
  #
  # The plan document is the single source of truth for phase and requirement
  # ownership. Both the active plan and the archived one already carry a
  # `**Status: ...**` marker and name their own bead root, so no second
  # manifest is invented and no competing source-of-truth format is kept.
  #
  # Read ONCE into a bounded buffer: `--plan` may be a process substitution
  # (the self-test uses one), which is a pipe and cannot be re-read, and an
  # unbounded read would let a large or hostile plan set the report's size.
  plan_body=$(head -c "$PLAN_READ_LIMIT_BYTES" "$PLAN_PATH" 2>/dev/null || true)
  # Matched with a bash `case`, not `printf | grep -q`: under `set -o pipefail`
  # a `grep -q` that matches exits immediately, the upstream `printf` takes
  # SIGPIPE, and the pipeline reports failure even though the pattern was
  # found — which silently left every plan reporting status "unknown".
  case "$plan_body" in
    *'**Status: ACTIVE'*) plan_status="active" ;;
    *'**Status: ARCHIVED'*) plan_status="archived" ;;
    *) plan_status="unknown" ;;
  esac
  plan_phase=$(printf '%s' "$plan_body" \
    | grep -oE '\*\*Status: (ACTIVE|ARCHIVED) \(Part [IVXLC]+\)' \
    | grep -oE 'Part [IVXLC]+' \
    | head -1 || true)
  # Requirement owner = the bead root this plan actually talks about, not a
  # frozen label set from a bridge that closed months ago. Ties break by name
  # so the field is stable across runs. The mention floor keeps an incidental
  # one-off reference from being mistaken for the owner.
  active_owner_root=$(printf '%s' "$plan_body" \
    | grep -oE 'bd-[a-z0-9][a-z0-9-]*' \
    | sed 's/-*$//' \
    | sort \
    | uniq -c \
    | sort -k1,1rn -k2,2 \
    | awk -v min="$ACTIVE_OWNER_MIN_MENTIONS" 'NR == 1 && $1 + 0 >= min + 0 { print $2 }' \
    || true)
  plan_body=""
fi

# Requirement maturity for the ACTIVE owner. Counts in_progress alongside open:
# a requirement parked in_progress with no movement is exactly the "evidence
# absent or stale" case this advisory exists to surface.
if [ -n "$active_owner_root" ] && [ -r "$BEADS_JSONL" ]; then
  active_owner_stats=$(jq -s --arg root "$active_owner_root" '
    [.[]
      | select((.id // "") | startswith($root))
      | select(.status == "open" or .status == "in_progress")]
    | {
        open_count: ([.[] | select(.status == "open")] | length),
        in_progress_count: ([.[] | select(.status == "in_progress")] | length),
        max_stale_days: (
          [.[]
            | (.updated_at // .created_at // "")
            | select(length > 0)
            | sub("\\.[0-9]+Z$"; "Z")
            | sub("Z$"; "+0000")
            | strptime("%Y-%m-%dT%H:%M:%S%z")
            | mktime]
          | if length == 0 then 0
            else (max | ((now - .) / 86400) | floor)
            end
        )
      }
  ' "$BEADS_JSONL" 2>/dev/null || echo '{"open_count":0,"in_progress_count":0,"max_stale_days":0}')
  active_owner_open_count=$(echo "$active_owner_stats" | jq -r '.open_count // 0')
  active_owner_in_progress_count=$(echo "$active_owner_stats" | jq -r '.in_progress_count // 0')
  active_owner_max_stale_days=$(echo "$active_owner_stats" | jq -r '.max_stale_days // 0')
fi

if [ "$plan_present" = true ] && [ "$plan_age_days" -gt 30 ]; then
  # An ACTIVE bridge is refreshed in place. AGENTS.md (*Reality-Check Cadence*)
  # is explicit that a competing successor file must not be opened at the repo
  # root while the current part is active, so a stale mtime on an ACTIVE plan
  # is never grounds to recommend authoring one.
  if [ "$plan_status" = "active" ]; then
    signal_one_repair="Refresh the active ${plan_phase:-bridge} plan in place with a current status block; do not open a successor part while it is ACTIVE."
  else
    signal_one_repair="Refresh CLOSE_THE_GAP_PLAN.md with a status block, or open the next bridge part if this one is archived."
  fi
  signal_one=$(jq -n \
    --arg code "plan_mtime_age_days" \
    --arg severity "medium" \
    --arg message "bridge plan mtime exceeds 30 day staleness budget" \
    --arg repair "$signal_one_repair" \
    --argjson plan_age_days "$plan_age_days" \
    --arg plan_path "CLOSE_THE_GAP_PLAN.md" \
    --arg plan_status "$plan_status" \
    --arg plan_phase "$plan_phase" \
    '{code: $code, severity: $severity, message: $message, repair: $repair, details: {planAgeDays: $plan_age_days, planPath: $plan_path, planStatus: $plan_status, planPhase: (if $plan_phase == "" then null else $plan_phase end)}}')
  signals_json="${signals_json}${signal_one},"
fi

# Signal 2: vision-coverage gap percentage.
vision_present=false
gap_percentage=null
if [ -r "$VISION_REPORT" ]; then
  vision_present=true
  gap_percentage=$(jq -r 'if (.gap_percentage | type) == "number" then .gap_percentage else empty end' "$VISION_REPORT" 2>/dev/null || true)
fi

if [ "$vision_present" = true ] && [ -n "$gap_percentage" ] && [ "$gap_percentage" != "null" ]; then
  if jq -e -n --argjson gap "$gap_percentage" '$gap < 2.0' >/dev/null; then
    # A low vision-coverage gap means documented files and registrations are
    # present. That is metadata coverage, NOT product completion, and it is
    # never by itself grounds to call a bridge closed. The advice therefore
    # follows the active phase and the maturity of that phase's requirements.
    active_owner_label="${active_owner_root:-owner unknown}"
    open_total=$(( active_owner_open_count + active_owner_in_progress_count ))
    case "$plan_status" in
      active)
        # Acceptance 1 + 2: never recommend authoring the phase that is already
        # active, and report metadata-complete/behavior-unproven rather than
        # finished while requirements remain.
        if [ "$open_total" -gt 0 ]; then
          signal_two_message="metadata coverage complete; ${plan_phase:-active bridge} behavior unproven with ${active_owner_open_count} open and ${active_owner_in_progress_count} in-progress requirements"
          signal_two_repair="Close the open requirements of the active ${plan_phase:-bridge} under ${active_owner_label}; metadata coverage is not product completion and no successor plan may be authored while this part is ACTIVE."
        else
          signal_two_message="metadata coverage complete and no open ${plan_phase:-active bridge} requirements remain; behavioral evidence still unproven by this advisory"
          signal_two_repair="Confirm behavioral evidence for ${plan_phase:-the active bridge} through its own verification gates before archiving; this advisory reads document and tracker state only."
        fi
        ;;
      archived)
        # Acceptance 4: a successor may be recommended only from explicit
        # completion evidence — an ARCHIVED marker AND no live requirements.
        if [ "$open_total" -eq 0 ]; then
          signal_two_message="archived bridge reports no open requirements; a successor part may be scoped"
          signal_two_repair="Scope the next bridge part from the archived plan's recorded completion evidence."
        else
          signal_two_message="plan is archived but ${open_total} requirements under ${active_owner_label} are still live; completion evidence is incomplete"
          signal_two_repair="Resolve or reassign the live requirements before scoping a successor part."
        fi
        ;;
      *)
        # Acceptance: missing/malformed inputs yield honest unknown advice,
        # never "closed".
        signal_two_message="metadata coverage complete but the bridge plan declares no readable status; completion is inconclusive"
        signal_two_repair="Add a '**Status: ACTIVE (Part N)**' or '**Status: ARCHIVED ...**' marker to the bridge plan so phase-aware advice is possible."
        ;;
    esac
    signal_two=$(jq -n \
      --arg code "vision_coverage_gap_low" \
      --arg severity "low" \
      --arg message "$signal_two_message" \
      --arg repair "$signal_two_repair" \
      --argjson gap "$gap_percentage" \
      --arg plan_status "$plan_status" \
      --arg plan_phase "$plan_phase" \
      --arg owner_root "$active_owner_root" \
      --argjson owner_open "$active_owner_open_count" \
      --argjson owner_in_progress "$active_owner_in_progress_count" \
      '{code: $code, severity: $severity, message: $message, repair: $repair, details: {gapPercentage: $gap, planStatus: $plan_status, planPhase: (if $plan_phase == "" then null else $plan_phase end), activeOwnerRoot: (if $owner_root == "" then null else $owner_root end), activeOwnerOpenCount: $owner_open, activeOwnerInProgressCount: $owner_in_progress}}')
    signals_json="${signals_json}${signal_two},"
  fi
fi

# Signal 3: open Part II beads with no in_progress activity for > 7 days.
part_ii_open_count=0
part_ii_in_progress_count=0
part_ii_max_stale_days=0
if [ -r "$BEADS_JSONL" ]; then
  # Filter beads to those tagged reality-check-2026-05-14 OR wave-4.
  part_ii_stats=$(jq -s '
    [.[]
      | select(.labels // [] | any(test("reality-check-2026-05-14|wave-4")))
      | select(.status == "open" or .status == "in_progress")]
    | {
        open_count: ([.[] | select(.status == "open")] | length),
        in_progress_count: ([.[] | select(.status == "in_progress")] | length),
        max_stale_days: (
          [.[]
            | select(.status == "open")
            | (.updated_at // .created_at // "")
            | select(length > 0)
            | sub("\\.[0-9]+Z$"; "Z")
            | sub("Z$"; "+0000")
            | strptime("%Y-%m-%dT%H:%M:%S%z")
            | mktime]
          | if length == 0 then 0
            else (max | ((now - .) / 86400) | floor)
            end
        )
      }
  ' "$BEADS_JSONL" 2>/dev/null || echo '{"open_count":0,"in_progress_count":0,"max_stale_days":0}')
  part_ii_open_count=$(echo "$part_ii_stats" | jq -r '.open_count // 0')
  part_ii_in_progress_count=$(echo "$part_ii_stats" | jq -r '.in_progress_count // 0')
  part_ii_max_stale_days=$(echo "$part_ii_stats" | jq -r '.max_stale_days // 0')
fi

# Inactivity is evaluated against the CURRENT requirement owner named by the
# active plan, falling back to the historical Part II label set only when the
# plan names no owner. Counting a bridge that closed months ago produced the
# advisory's original defect: it reported zero live work and concluded the
# current bridge was finished.
if [ -n "$active_owner_root" ]; then
  stale_scope="$active_owner_root"
  stale_open_count="$active_owner_open_count"
  stale_in_progress_count="$active_owner_in_progress_count"
  stale_max_days="$active_owner_max_stale_days"
  stale_message="${plan_phase:-active bridge} requirements under ${active_owner_root} show no recent tracker activity"
  stale_repair="Triage at least one ${active_owner_root} requirement per day, or record why the ${plan_phase:-active bridge} is paused."
else
  stale_scope="reality-check-2026-05-14|wave-4"
  stale_open_count="$part_ii_open_count"
  stale_in_progress_count="$part_ii_in_progress_count"
  stale_max_days="$part_ii_max_stale_days"
  stale_message="Part II swarm not eating the bridge"
  stale_repair="Triage at least one reality-check-2026-05-14 bead per day or close the bridge plan."
fi

if [ "$stale_open_count" -gt 0 ] && [ "$stale_in_progress_count" -eq 0 ] && [ "$stale_max_days" -gt 7 ]; then
  signal_three=$(jq -n \
    --arg code "in_progress_beads_mtime" \
    --arg severity "medium" \
    --arg message "$stale_message" \
    --arg repair "$stale_repair" \
    --arg scope "$stale_scope" \
    --argjson open_count "$part_ii_open_count" \
    --argjson in_progress_count "$part_ii_in_progress_count" \
    --argjson max_stale_days "$part_ii_max_stale_days" \
    --argjson scope_open_count "$stale_open_count" \
    --argjson scope_in_progress_count "$stale_in_progress_count" \
    --argjson scope_max_stale_days "$stale_max_days" \
    '{code: $code, severity: $severity, message: $message, repair: $repair, details: {partIIOpenCount: $open_count, partIIInProgressCount: $in_progress_count, partIIMaxStaleDays: $max_stale_days, scope: $scope, scopeOpenCount: $scope_open_count, scopeInProgressCount: $scope_in_progress_count, scopeMaxStaleDays: $scope_max_stale_days}}')
  signals_json="${signals_json}${signal_three},"
fi

# Trim trailing comma and wrap in a JSON array.
signals_array="[${signals_json%,}]"

# Compute deterministic data hash of input state for the report.
data_hash_input=$(printf 'plan=%s|gap=%s|open=%s|inprog=%s|stale=%s|status=%s|phase=%s|owner=%s|owneropen=%s|ownerinprog=%s|ownerstale=%s' \
  "$plan_age_days" "$gap_percentage" \
  "$part_ii_open_count" "$part_ii_in_progress_count" "$part_ii_max_stale_days" \
  "$plan_status" "$plan_phase" "$active_owner_root" \
  "$active_owner_open_count" "$active_owner_in_progress_count" "$active_owner_max_stale_days")
if command -v shasum >/dev/null 2>&1; then
  data_hash=$(printf '%s' "$data_hash_input" | shasum -a 256 | awk '{print $1}')
elif command -v sha256sum >/dev/null 2>&1; then
  data_hash=$(printf '%s' "$data_hash_input" | sha256sum | awk '{print $1}')
else
  data_hash="unavailable"
fi

report=$(jq -n \
  --arg schema "ee.bridge.staleness.v1" \
  --arg generated_at "$generated_at" \
  --arg data_hash "$data_hash" \
  --argjson signals "$signals_array" \
  --argjson plan_present "$( [ "$plan_present" = true ] && echo true || echo false )" \
  --argjson plan_age_days "$plan_age_days" \
  --argjson vision_present "$( [ "$vision_present" = true ] && echo true || echo false )" \
  --argjson part_ii_open_count "$part_ii_open_count" \
  --argjson part_ii_in_progress_count "$part_ii_in_progress_count" \
  --argjson part_ii_max_stale_days "$part_ii_max_stale_days" \
  --arg plan_status "$plan_status" \
  --arg plan_phase "$plan_phase" \
  --arg active_owner_root "$active_owner_root" \
  --argjson active_owner_open_count "$active_owner_open_count" \
  --argjson active_owner_in_progress_count "$active_owner_in_progress_count" \
  --argjson active_owner_max_stale_days "$active_owner_max_stale_days" \
  '{
    schema: $schema,
    generatedAt: $generated_at,
    dataHash: $data_hash,
    inputs: {
      planPresent: $plan_present,
      planAgeDays: $plan_age_days,
      visionCoverageReportPresent: $vision_present,
      partIIOpenCount: $part_ii_open_count,
      partIIInProgressCount: $part_ii_in_progress_count,
      partIIMaxStaleDays: $part_ii_max_stale_days,
      activePlanStatus: $plan_status,
      activePlanPhase: (if $plan_phase == "" then null else $plan_phase end),
      activeOwnerRoot: (if $active_owner_root == "" then null else $active_owner_root end),
      activeOwnerOpenCount: $active_owner_open_count,
      activeOwnerInProgressCount: $active_owner_in_progress_count,
      activeOwnerMaxStaleDays: $active_owner_max_stale_days
    },
    signals: $signals
  }')

printf '%s\n' "$report" > "$OUTPUT_PATH"

if [ -n "$JSON_FLAG" ]; then
  printf '%s\n' "$report"
  exit 0
fi

if [ -z "$QUIET_FLAG" ]; then
  signal_count=$(printf '%s' "$signals_array" | jq 'length')
  echo "Bridge staleness report → $OUTPUT_PATH" >&2
  echo "  signals: $signal_count" >&2
  echo "  plan_mtime_age_days: $plan_age_days" >&2
  echo "  vision_coverage_gap_percentage: ${gap_percentage:-unknown}" >&2
  echo "  part_ii_open_count: $part_ii_open_count (in_progress=$part_ii_in_progress_count, max_stale_days=$part_ii_max_stale_days)" >&2
  echo "  active_plan: ${plan_status}${plan_phase:+ (${plan_phase})}" >&2
  echo "  active_owner: ${active_owner_root:-unknown} (open=$active_owner_open_count, in_progress=$active_owner_in_progress_count, max_stale_days=$active_owner_max_stale_days)" >&2
fi

exit 0
