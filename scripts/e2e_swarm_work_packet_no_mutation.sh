#!/usr/bin/env bash
# E2E smoke (bd-2z5ly.4, bd-2z5ly.9, bd-13dmm.4): proves that swarm
# work-packet generation and command-action consumption stay advisory — no
# `br update`, no `br sync`, no edits to `.beads/`, no staged git changes, no
# Agent Mail writes, and no Cargo/RCH execution while parsing safe argv metadata.
#
# Strategy: build a sandbox workspace under $artifact_root/work that
# contains a copy of `.beads/` (and a synthetic merge artifact + a
# malformed JSONL tail for the degraded path), snapshot its contents,
# run packet generation through a PATH-shimmed `br` that records every
# invocation and refuses mutating subcommands, then re-snapshot and
# diff. Sandboxing isolates the test from concurrent peer activity in
# the real repo, so the snapshot/diff harness measures only the system
# under test.
#
# The shim refuses anything other than read-only `br ready` / `br doctor` /
# `br list` / `br show` / `br comments` invocations so an accidental mutation
# in the packet collector trips the script immediately rather than corrupting
# the tracker.
#
# This script does NOT invoke Cargo. Real-Cargo verification is RCH-only per
# AGENTS.md; this is the static / shell smoke half of the proof. Callers may set
# EE_PACKET_NO_MUTATION_CMD to drive a built `ee swarm work-packet --json`
# binary through the same no-mutation harness.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

ts="$(date -u +%Y%m%dT%H%M%SZ)"
artifact_root="${EE_PACKET_NO_MUTATION_ARTIFACT_ROOT:-/tmp/ee_packet_no_mutation_${ts}_$$}"
sandbox="$artifact_root/work"
shim_bin="$artifact_root/bin"
call_log="$artifact_root/br_calls.log"
beads_before="$artifact_root/beads_before.sha"
beads_after="$artifact_root/beads_after.sha"
mail_root="$artifact_root/mail"
mail_before="$artifact_root/mail_before.sha"
mail_after="$artifact_root/mail_after.sha"
summary="$artifact_root/summary.jsonl"
event_log="${EE_PACKET_NO_MUTATION_EVENT_LOG:-$artifact_root/events.jsonl}"
packet_json="$artifact_root/work_packet.json"
packet_stderr="$artifact_root/work_packet.stderr"
action_summary="$artifact_root/action_summary.json"
consumer_decision="$artifact_root/consumer_decision.json"
consumer_stderr="$artifact_root/consumer_decision.stderr"
consumer_summary="$artifact_root/consumer_summary.json"
fixture_matrix_dir="${EE_PACKET_NO_MUTATION_FIXTURE_DIR:-$REPO_ROOT/tests/fixtures/swarm_work_packet}"
install_fixture_matrix_dir="${EE_PACKET_NO_MUTATION_INSTALL_FIXTURE_DIR:-$REPO_ROOT/tests/fixtures/golden/install}"
fixture_matrix_root="$artifact_root/fixture_matrix"
fixture_matrix_summary="$artifact_root/fixture_matrix_summary.json"
git_index_before="$artifact_root/git_index_before.txt"
git_index_after="$artifact_root/git_index_after.txt"
forbidden_log_dir="$artifact_root/forbidden_calls"
cargo_log="$forbidden_log_dir/cargo.log"
rch_log="$forbidden_log_dir/rch.log"

mkdir -p "$shim_bin" "$sandbox/.beads" "$mail_root" "$forbidden_log_dir" "$fixture_matrix_root"

# shellcheck source=scripts/lib/e2e_logger.sh
source "$REPO_ROOT/scripts/lib/e2e_logger.sh"
e2e_log_start "swarm_work_packet_no_mutation" "$event_log"

emit_phase() {
    local phase="${1:?phase required}"
    shift
    _e2e_emit_event "note" "phase" "$phase" "$@"
}

# Seed the sandbox with a minimal Beads layout and a synthetic
# malformed tail so the smoke run exercises the degraded path the
# bead targets.
cat >"$sandbox/.beads/issues.jsonl" <<'JSONL'
{"id":"bd-fixture-1","title":"sandbox fixture","status":"open","priority":2}
{"id":"bd-fixture-2","title":"second fixture","status":"open","priority":3}
{"id":"bd-fixture-3","title":"third fixture","status":"open","priority":3}
{"id":"bd-malformed-tail","title":"WIP - record was truncated mid
JSONL
# Synthetic merge artifact next to issues.jsonl.
printf '%s\n' "merge-artifact placeholder" >"$sandbox/.beads/issues.jsonl.orig"
# Empty SQLite stand-in; the shim does not open it.
: >"$sandbox/.beads/beads.db"

snapshot_dir() {
    local root="$1"
    local out="$2"
    if [ -d "$root" ]; then
        ( cd "$root" && find . -type f -print0 \
            | LC_ALL=C sort -z \
            | xargs -0 shasum -a 256 ) >"$out"
    else
        : >"$out"
    fi
}

json_field() {
    local path="$1"
    local pointer="$2"
    python3 - "$path" "$pointer" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as handle:
    value = json.load(handle)

for part in [p for p in sys.argv[2].split("/") if p]:
    if isinstance(value, dict):
        value = value.get(part, "")
    else:
        value = ""
        break
if isinstance(value, (dict, list)):
    print(json.dumps(value, sort_keys=True))
elif value is None:
    print("")
else:
    print(value)
PY
}

generate_fixture_packet() {
    cat <<'JSON'
{
  "schema": "ee.response.v2",
  "success": true,
  "data": {
    "schema": "ee.swarm.work_packet.v1",
    "workspace": "fixture:redacted",
    "observedStateClass": "degraded_mail_rch_topology",
    "recommendedAction": {
      "action": "coordinate_before_claim",
      "candidateId": "bd-fixture-1",
      "safeToClaim": false,
      "reasons": [
        "agent_mail_unavailable",
        "rch_remote_verification_blocked",
        "fixture_inspection_only"
      ],
      "suggestedCommands": [
        "br show bd-fixture-1 --json",
        "printf packet | jq ."
      ],
      "suggestedCommandActions": [
        {
          "commandId": "bead_show_candidate",
          "displayCommand": "br show bd-fixture-1 --json",
          "argv": ["br", "show", "bd-fixture-1", "--json"],
          "shellRequired": false,
          "copySafety": "safe_structured_argv",
          "mutatesState": false,
          "requiredSubstrate": "beads",
          "when": "before_claim",
          "rationale": "Inspect the selected bead before deciding whether to claim it."
        },
        {
          "commandId": "fixture_shell_review",
          "displayCommand": "printf packet | jq .",
          "argv": ["sh", "-c", "printf packet | jq ."],
          "shellRequired": true,
          "copySafety": "shell_required_review",
          "mutatesState": false,
          "requiredSubstrate": "static_local",
          "when": "manual_review_only",
          "rationale": "Fixture-only shell-review action proves consumers do not execute display text."
        }
      ]
    },
    "safeToClaim": false,
    "candidates": [
      {
        "id": "bd-fixture-1",
        "title": "sandbox fixture",
        "source": "beads_ready",
        "status": "open",
        "priority": 2,
        "assignee": null,
        "decision": "stale_or_advisory",
        "collisionRisk": "low",
        "unsafeReasons": [
          "agent_mail_reservation_evidence_unavailable",
          "rch_remote_verification_blocked"
        ],
        "staleReasons": [
          "agent_mail_unavailable"
        ],
        "sourceRefs": [
          "br://bd-fixture-1",
          "agent-mail://unavailable",
          "rch://topology_blocked"
        ]
      }
    ],
    "coordination": {
      "agentMail": {
        "status": "degraded_read_only",
        "reservationAuthoritative": false,
        "inboxAuthoritative": false,
        "degradedCodes": [
          "agent_mail_unavailable"
        ],
        "fallbackActions": []
      }
    },
    "trackerIntegrity": {
      "health": "ok",
      "brReadsAuthoritative": true,
      "requiresCandidateDowngrade": false
    },
    "rchProofPosture": {
      "sourceEnabled": true,
      "remoteOnlyRequired": true,
      "posture": "topology_blocked",
      "safeToLaunchCargoVerification": false,
      "localFallbackPrevented": true,
      "blockerCodes": [
        "rch_worker_topology_blocked"
      ],
      "knownBlockers": []
    },
    "sourceProvenance": [
      {
        "source": "beads",
        "status": "read_only",
        "ref": "br://bd-fixture-1"
      },
      {
        "source": "agent-mail",
        "status": "degraded_read_only",
        "ref": "agent-mail://unavailable"
      },
      {
        "source": "rch",
        "status": "topology_blocked",
        "ref": "rch://topology_blocked"
      }
    ],
    "verification": {
      "requiredCommands": [],
      "staticChecks": [
        {
          "commandId": "diff_check",
          "commandTemplate": "git diff --check",
          "commandAction": {
            "commandId": "diff_check",
            "displayCommand": "git diff --check",
            "argv": ["git", "diff", "--check"],
            "shellRequired": false,
            "copySafety": "safe_structured_argv",
            "mutatesState": false,
            "requiredSubstrate": "git",
            "when": "before_closeout",
            "rationale": "Reject whitespace errors before preparing a closeout commit."
          },
          "requiredSubstrate": "static_local",
          "when": "before_closeout",
          "lastOutcome": "not_run",
          "lastCommandHash": null
        }
      ],
      "closeoutEvidenceRequired": true
    },
    "mutationPolicy": {
      "sideEffectFree": true,
      "claimsBeads": false,
      "reservesFiles": false,
      "sendsAgentMail": false,
      "runsCargo": false,
      "stagesGit": false,
      "deletesFiles": false
    },
    "degraded": [
      {
        "code": "agent_mail_unavailable",
        "source": "agent-mail",
        "severity": "warning",
        "message": "Agent Mail unavailable in fixture.",
        "repair": null
      }
    ]
  },
  "degraded": [
    {
      "code": "agent_mail_unavailable",
      "source": "agent-mail",
      "severity": "warning",
      "message": "Agent Mail unavailable in fixture.",
      "repair": null
    }
  ]
}
JSON
}

parse_packet_actions() {
    python3 - "$packet_json" "$action_summary" <<'PY'
import hashlib
import json
import re
import sys

packet_path, summary_path = sys.argv[1], sys.argv[2]

failures = []
assertions = []

def check(name, passed, detail=""):
    assertions.append(name)
    if not passed:
        failures.append(f"{name}{':' + detail if detail else ''}")

try:
    with open(packet_path, encoding="utf-8") as handle:
        root = json.load(handle)
    check("packet_json_parseable", True)
except json.JSONDecodeError as error:
    root = {}
    check(
        "packet_json_parseable",
        False,
        f"line_{error.lineno}_column_{error.colno}",
    )

def dict_or_empty(value):
    return value if isinstance(value, dict) else {}

def list_items(value):
    return value if isinstance(value, list) else []

root_is_object = isinstance(root, dict)
check("root_json_object", root_is_object)
root_obj = dict_or_empty(root)
payload = root_obj.get("data", root_obj)
check("packet_payload_object", isinstance(payload, dict))
packet = dict_or_empty(payload)

actions = []

def add_action(path, action):
    if isinstance(action, dict):
        actions.append((path, action))
    elif action is not None:
        check("command_action_object", False, path)

recommended_raw = packet.get("recommendedAction")
if recommended_raw is not None:
    check("recommended_action_object", isinstance(recommended_raw, dict))
recommended = dict_or_empty(recommended_raw)
for index, action in enumerate(list_items(recommended.get("suggestedCommandActions"))):
    add_action(f"recommendedAction.suggestedCommandActions[{index}]", action)

verification_raw = packet.get("verification")
if verification_raw is not None:
    check("verification_object", isinstance(verification_raw, dict))
verification = dict_or_empty(verification_raw)
for section in ("requiredCommands", "staticChecks"):
    for index, command in enumerate(list_items(verification.get(section))):
        path = f"verification.{section}[{index}].commandAction"
        if isinstance(command, dict):
            add_action(path, command.get("commandAction"))
        elif command is not None:
            check("verification_command_object", False, path)

coordination_raw = packet.get("coordination")
if coordination_raw is not None:
    check("coordination_object", isinstance(coordination_raw, dict))
coordination = dict_or_empty(coordination_raw)
agent_mail_raw = coordination.get("agentMail")
if agent_mail_raw is not None:
    check("agent_mail_object", isinstance(agent_mail_raw, dict))
agent_mail = dict_or_empty(agent_mail_raw)
for index, fallback in enumerate(list_items(agent_mail.get("fallbackActions"))):
    path = f"coordination.agentMail.fallbackActions[{index}].commandAction"
    if isinstance(fallback, dict):
        add_action(path, fallback.get("commandAction"))
    elif fallback is not None:
        check("agent_mail_fallback_object", False, path)

command_ids = []
copy_safety_values = []
argv_hashes = []
safe_static_count = 0
review_count = 0
risky_display = re.compile(r"(\|\||\||\$\(|>|<|`)")

check("command_actions_present", bool(actions))
for path, action in actions:
    command_id = str(action.get("commandId") or path)
    copy_safety = str(action.get("copySafety") or "")
    argv = action.get("argv")
    shell_required = action.get("shellRequired")
    display_command = str(action.get("displayCommand") or "")
    required_substrate = str(action.get("requiredSubstrate") or "")
    command_ids.append(command_id)
    copy_safety_values.append(copy_safety)

    if isinstance(argv, list):
        argv_payload = "\0".join(str(part) for part in argv)
        argv_hashes.append(f"{command_id}=sha256:{hashlib.sha256(argv_payload.encode()).hexdigest()[:16]}")

    if copy_safety == "safe_structured_argv":
        check("safe_command_has_argv", isinstance(argv, list) and len(argv) > 0, command_id)
        check("safe_command_is_shell_free", shell_required is False, command_id)
        check("display_only_command_not_marked_safe", not risky_display.search(display_command), command_id)
        if required_substrate in {"git", "static_local"}:
            safe_static_count += 1
    elif copy_safety in {"display_only", "shell_required_review"}:
        review_count += 1
        if copy_safety == "shell_required_review":
            check("shell_review_requires_shell", shell_required is True, command_id)
    else:
        check("known_copy_safety", False, command_id)

check("safe_static_command_present", safe_static_count > 0)
check("display_or_shell_review_action_present", review_count > 0)

mutation_policy_raw = packet.get("mutationPolicy")
check("mutation_policy_object", isinstance(mutation_policy_raw, dict))
mutation_policy = dict_or_empty(mutation_policy_raw)
for field in (
    "claimsBeads",
    "reservesFiles",
    "sendsAgentMail",
    "runsCargo",
    "stagesGit",
    "deletesFiles",
):
    check(f"mutation_policy_{field}_false", mutation_policy.get(field) is False)
check("mutation_policy_side_effect_free", mutation_policy.get("sideEffectFree") is True)

packet_degraded = list_items(packet.get("degraded"))
root_degraded = list_items(root_obj.get("degraded"))
degraded = packet_degraded or root_degraded
degraded_codes = sorted({
    str(item.get("code"))
    for item in degraded
    if isinstance(item, dict) and item.get("code")
})

summary = {
    "action_count": len(actions),
    "command_ids": ",".join(sorted(command_ids)),
    "copy_safety_values": ",".join(sorted(set(copy_safety_values))),
    "argv_hashes": ",".join(sorted(argv_hashes)),
    "degraded_codes": ",".join(degraded_codes),
    "assertion_names": ",".join(assertions),
    "failures": failures,
}
with open(summary_path, "w", encoding="utf-8") as handle:
    json.dump(summary, handle, sort_keys=True)
    handle.write("\n")

if failures:
    for failure in failures:
        print(f"ASSERTION FAILED: {failure}", file=sys.stderr)
    sys.exit(23)
PY
}

parse_consumer_decision() {
    python3 - "$consumer_decision" "$consumer_summary" <<'PY'
import json
import sys

decision_path, summary_path = sys.argv[1], sys.argv[2]

failures = []
assertions = []


def check(name, passed, detail=""):
    assertions.append(name)
    if not passed:
        failures.append(f"{name}{':' + detail if detail else ''}")


try:
    with open(decision_path, encoding="utf-8") as handle:
        decision = json.load(handle)
    check("consumer_json_parseable", True)
except json.JSONDecodeError as error:
    decision = {}
    check(
        "consumer_json_parseable",
        False,
        f"line_{error.lineno}_column_{error.colno}",
    )

check("consumer_decision_object", isinstance(decision, dict))
if not isinstance(decision, dict):
    decision = {}

safe_to_claim = decision.get("safeToClaim")
argv_actions = decision.get("argvActions")
why_not_safe = decision.get("whyNotSafe")
degraded_summary = decision.get("degradedSummary")
max_argv_part_count = 0

check(
    "consumer_schema_current",
    decision.get("schema") == "ee.agent.work_packet_gate_decision.v1",
)
check("consumer_safe_to_claim_boolean", isinstance(safe_to_claim, bool))
check("consumer_decision_string", isinstance(decision.get("decision"), str))
check("consumer_action_string", isinstance(decision.get("action"), str))
check("consumer_argv_actions_array", isinstance(argv_actions, list))
check("consumer_why_not_safe_array", isinstance(why_not_safe, list))
check("consumer_degraded_summary_array", isinstance(degraded_summary, list))

if isinstance(argv_actions, list):
    check("consumer_argv_actions_bounded", len(argv_actions) <= 16, str(len(argv_actions)))
if isinstance(why_not_safe, list):
    check("consumer_why_not_safe_bounded", len(why_not_safe) <= 16, str(len(why_not_safe)))
if isinstance(degraded_summary, list):
    check(
        "consumer_degraded_summary_bounded",
        len(degraded_summary) <= 16,
        str(len(degraded_summary)),
    )

if safe_to_claim is False:
    check(
        "unsafe_consumer_has_reasons",
        isinstance(why_not_safe, list) and len(why_not_safe) > 0,
    )

runnable_mutating = []
runnable_claim = []
if isinstance(argv_actions, list):
    for index, action in enumerate(argv_actions):
        if not isinstance(action, dict):
            check("consumer_argv_action_object", False, str(index))
            continue
        command_id = str(action.get("commandId") or index)
        argv = action.get("argv")
        check("consumer_argv_action_argv_array", isinstance(argv, list), command_id)
        if isinstance(argv, list):
            max_argv_part_count = max(max_argv_part_count, len(argv))
            check(
                "consumer_argv_action_argv_bounded",
                len(argv) <= 32,
                f"{command_id}:{len(argv)}",
            )
        if action.get("runnable") is True and action.get("mutatesState") is True:
            runnable_mutating.append(command_id)
        if (
            safe_to_claim is False
            and action.get("runnable") is True
            and action.get("actionKind") == "claim"
        ):
            runnable_claim.append(command_id)

check("consumer_has_no_runnable_mutation", not runnable_mutating, ",".join(runnable_mutating))
check("unsafe_consumer_has_no_runnable_claim", not runnable_claim, ",".join(runnable_claim))

summary = {
    "schema": decision.get("schema") or "",
    "safe_to_claim": "true"
    if safe_to_claim is True
    else "false"
    if safe_to_claim is False
    else "",
    "decision": decision.get("decision") or "",
    "action": decision.get("action") or "",
    "why_not_safe_count": len(why_not_safe) if isinstance(why_not_safe, list) else 0,
    "argv_action_count": len(argv_actions) if isinstance(argv_actions, list) else 0,
    "degraded_summary_count": len(degraded_summary)
    if isinstance(degraded_summary, list)
    else 0,
    "max_argv_part_count": max_argv_part_count,
    "assertion_names": ",".join(assertions),
    "failures": failures,
}
with open(summary_path, "w", encoding="utf-8") as handle:
    json.dump(summary, handle, sort_keys=True)
    handle.write("\n")

if failures:
    for failure in failures:
        print(f"ASSERTION FAILED: {failure}", file=sys.stderr)
    sys.exit(24)
PY
}

run_consumer_fixture_matrix() {
    python3 - "$fixture_matrix_dir" "$install_fixture_matrix_dir" "$fixture_matrix_root" \
        "$fixture_matrix_summary" "$REPO_ROOT/scripts/agent_consume_work_packet_gate.py" <<'PY'
import json
import os
import pathlib
import subprocess
import sys

fixture_dir = pathlib.Path(sys.argv[1])
install_fixture_dir = pathlib.Path(sys.argv[2])
matrix_root = pathlib.Path(sys.argv[3])
summary_path = pathlib.Path(sys.argv[4])
consumer = pathlib.Path(sys.argv[5])

failures = []
assertions = []


def check(name, passed, detail=""):
    assertions.append(name)
    if not passed:
        failures.append(f"{name}{':' + detail if detail else ''}")


check("fixture_matrix_dir_exists", fixture_dir.is_dir(), str(fixture_dir))
swarm_fixtures = sorted(fixture_dir.glob("*.json")) if fixture_dir.is_dir() else []
check("fixture_matrix_non_empty", bool(swarm_fixtures))
check("install_fixture_matrix_dir_exists", install_fixture_dir.is_dir(), str(install_fixture_dir))
install_fixtures = (
    sorted(install_fixture_dir.glob("*_check.json.golden"))
    if install_fixture_dir.is_dir()
    else []
)
check("install_fixture_matrix_non_empty", bool(install_fixtures))
fixtures = [(fixture, "swarm") for fixture in swarm_fixtures] + [
    (fixture, "install_check") for fixture in install_fixtures
]

env = os.environ.copy()
env["PYTHONDONTWRITEBYTECODE"] = "1"
rows = []

for fixture, fixture_kind in fixtures:
    fixture_document = json.loads(fixture.read_text(encoding="utf-8"))
    outer_schema = fixture_document.get("schema") if isinstance(fixture_document, dict) else None
    payload = (
        fixture_document.get("data")
        if outer_schema == "ee.response.v2" and isinstance(fixture_document.get("data"), dict)
        else fixture_document
    )
    payload_schema = payload.get("schema") if isinstance(payload, dict) else None
    supported_payload_schemas = {
        "ee.swarm.work_packet.claim_gate.v1",
        "ee.swarm.work_packet.v1",
        "ee.install.check.v1",
    }
    expects_consumer_error = (
        outer_schema == "ee.error.v2" or payload_schema not in supported_payload_schemas
    )
    fixture_key = "".join(
        char if char.isalnum() or char in "-_" else "_"
        for char in f"{fixture_kind}_{fixture.name}"
    )
    decision_path = matrix_root / f"{fixture_key}.decision.json"
    stderr_path = matrix_root / f"{fixture_key}.stderr"
    proc = subprocess.run(
        [sys.executable, "-B", str(consumer)],
        input=fixture.read_bytes(),
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        env=env,
        check=False,
    )
    decision_path.write_bytes(proc.stdout)
    stderr_path.write_bytes(proc.stderr)

    try:
        decision = json.loads(proc.stdout.decode("utf-8"))
        check(f"{fixture.name}:decision_json_parseable", True)
    except json.JSONDecodeError as error:
        decision = {}
        check(
            f"{fixture.name}:decision_json_parseable",
            False,
            f"line_{error.lineno}_column_{error.colno}",
        )

    expected_safe = fixture_kind == "swarm" and fixture.name == "healthy_small.json"
    expected_exit = 0 if expected_safe else (2 if expects_consumer_error else 3)
    safe_to_claim = decision.get("safeToClaim")
    argv_actions = decision.get("argvActions")
    why_not_safe = decision.get("whyNotSafe")
    degraded_summary = decision.get("degradedSummary")
    decision_name = str(decision.get("decision") or "")
    max_argv_part_count = 0

    check(
        f"{fixture.name}:consumer_schema_current",
        decision.get("schema") == "ee.agent.work_packet_gate_decision.v1",
    )
    check(f"{fixture.name}:exit_code_expected", proc.returncode == expected_exit, str(proc.returncode))
    check(f"{fixture.name}:safe_to_claim_expected", safe_to_claim is expected_safe, str(safe_to_claim))
    check(f"{fixture.name}:argv_actions_array", isinstance(argv_actions, list))
    check(f"{fixture.name}:why_not_safe_array", isinstance(why_not_safe, list))
    check(f"{fixture.name}:degraded_summary_array", isinstance(degraded_summary, list))
    if isinstance(argv_actions, list):
        check(
            f"{fixture.name}:argv_actions_bounded",
            len(argv_actions) <= 16,
            str(len(argv_actions)),
        )
    if isinstance(why_not_safe, list):
        check(
            f"{fixture.name}:why_not_safe_bounded",
            len(why_not_safe) <= 16,
            str(len(why_not_safe)),
        )
    if isinstance(degraded_summary, list):
        check(
            f"{fixture.name}:degraded_summary_bounded",
            len(degraded_summary) <= 16,
            str(len(degraded_summary)),
        )

    if expected_safe:
        check(f"{fixture.name}:safe_has_no_unsafe_reasons", why_not_safe == [])
    else:
        check(
            f"{fixture.name}:unsafe_has_reasons",
            isinstance(why_not_safe, list) and len(why_not_safe) > 0,
        )

    runnable_mutating = []
    runnable_claim = []
    if isinstance(argv_actions, list):
        for index, action in enumerate(argv_actions):
            if not isinstance(action, dict):
                check(f"{fixture.name}:argv_action_object", False, str(index))
                continue
            command_id = str(action.get("commandId") or index)
            argv = action.get("argv")
            check(f"{fixture.name}:argv_action_argv_array", isinstance(argv, list), command_id)
            if isinstance(argv, list):
                max_argv_part_count = max(max_argv_part_count, len(argv))
                check(
                    f"{fixture.name}:argv_action_argv_bounded",
                    len(argv) <= 32,
                    f"{command_id}:{len(argv)}",
                )
            if action.get("runnable") is True and action.get("mutatesState") is True:
                runnable_mutating.append(command_id)
            if (
                not expected_safe
                and action.get("runnable") is True
                and action.get("actionKind") == "claim"
            ):
                runnable_claim.append(command_id)

    if not expected_safe:
        check(
            f"{fixture.name}:unsafe_has_no_runnable_mutation",
            not runnable_mutating,
            ",".join(runnable_mutating),
        )
        check(
            f"{fixture.name}:unsafe_has_no_runnable_claim",
            not runnable_claim,
            ",".join(runnable_claim),
        )

    rows.append(
        {
            "fixture": fixture.name,
            "fixture_kind": fixture_kind,
            "exit_code": proc.returncode,
            "safe_to_claim": safe_to_claim,
            "decision": decision_name,
            "why_not_safe_count": len(why_not_safe)
            if isinstance(why_not_safe, list)
            else 0,
            "degraded_summary_count": len(degraded_summary)
            if isinstance(degraded_summary, list)
            else 0,
            "argv_action_count": len(argv_actions)
            if isinstance(argv_actions, list)
            else 0,
            "max_argv_part_count": max_argv_part_count,
        }
    )

safe_count = sum(1 for row in rows if row["safe_to_claim"] is True)
unsafe_count = sum(1 for row in rows if row["safe_to_claim"] is False)
install_count = sum(1 for row in rows if row["fixture_kind"] == "install_check")
check("fixture_matrix_single_safe_fixture", safe_count == 1, str(safe_count))
check("fixture_matrix_unsafe_remainder", unsafe_count == max(len(rows) - 1, 0), str(unsafe_count))
check("install_fixture_matrix_all_unsafe", install_count > 0 and install_count <= unsafe_count, str(install_count))

summary = {
    "fixture_count": len(rows),
    "swarm_fixture_count": len(swarm_fixtures),
    "install_fixture_count": install_count,
    "safe_fixture_count": safe_count,
    "unsafe_fixture_count": unsafe_count,
    "fixture_names": ",".join(row["fixture"] for row in rows),
    "install_fixture_names": ",".join(
        row["fixture"] for row in rows if row["fixture_kind"] == "install_check"
    ),
    "decision_summary": ",".join(
        f"{row['fixture_kind']}:{row['fixture']}:{row['decision']}:{str(row['safe_to_claim']).lower()}:{row['exit_code']}"
        for row in rows
    ),
    "max_why_not_safe_count": max(
        (row["why_not_safe_count"] for row in rows),
        default=0,
    ),
    "max_degraded_summary_count": max(
        (row["degraded_summary_count"] for row in rows),
        default=0,
    ),
    "max_argv_action_count": max(
        (row["argv_action_count"] for row in rows),
        default=0,
    ),
    "max_argv_part_count": max(
        (row["max_argv_part_count"] for row in rows),
        default=0,
    ),
    "assertion_names": ",".join(assertions),
    "failures": failures,
}
with open(summary_path, "w", encoding="utf-8") as handle:
    json.dump(summary, handle, sort_keys=True)
    handle.write("\n")

if failures:
    for failure in failures:
        print(f"ASSERTION FAILED: {failure}", file=sys.stderr)
    sys.exit(25)
PY
}

# `br` shim — records the call, allows read-only subcommands, refuses
# anything that would mutate the tracker. Refuse means non-zero exit so
# the caller fails loudly.
cat >"$shim_bin/br" <<'SHIM'
#!/usr/bin/env bash
set -euo pipefail
log="${EE_PACKET_NO_MUTATION_BR_LOG:?EE_PACKET_NO_MUTATION_BR_LOG required}"
printf '%s\n' "br $*" >>"$log"

# Find the first non-flag argument; that is the subcommand.
sub=""
for arg in "$@"; do
    case "$arg" in
        --*) continue ;;
        *) sub="$arg"; break ;;
    esac
done

case "$sub" in
    ""|ready|list|show|doctor|stats|blocked|comments)
        # Emit an empty but well-formed JSON envelope for `--json`
        # consumers; otherwise emit nothing.
        for arg in "$@"; do
            if [ "$arg" = "--json" ]; then
                printf '{"schema":"br.shim.v1","sub":"%s","issues":[],"checks":[],"ok":true}\n' "$sub"
                exit 0
            fi
        done
        exit 0
        ;;
    *)
        printf 'PACKET-NO-MUTATION shim refused mutating subcommand: %s\n' "$sub" >&2
        exit 64
        ;;
esac
SHIM
chmod +x "$shim_bin/br"

cat >"$shim_bin/cargo" <<'SHIM'
#!/usr/bin/env bash
set -euo pipefail
log="${EE_PACKET_NO_MUTATION_CARGO_LOG:?EE_PACKET_NO_MUTATION_CARGO_LOG required}"
printf '%s\n' "cargo $*" >>"$log"
printf 'PACKET-NO-MUTATION shim refused Cargo execution\n' >&2
exit 64
SHIM
chmod +x "$shim_bin/cargo"

cat >"$shim_bin/rch" <<'SHIM'
#!/usr/bin/env bash
set -euo pipefail
log="${EE_PACKET_NO_MUTATION_RCH_LOG:?EE_PACKET_NO_MUTATION_RCH_LOG required}"
printf '%s\n' "rch $*" >>"$log"
printf 'PACKET-NO-MUTATION shim refused RCH execution\n' >&2
exit 64
SHIM
chmod +x "$shim_bin/rch"

snapshot_dir "$sandbox/.beads" "$beads_before"
snapshot_dir "$mail_root" "$mail_before"
git -C "$REPO_ROOT" diff --cached --name-only >"$git_index_before"
: >"$call_log"
: >"$cargo_log"
: >"$rch_log"

emit_phase "setup" \
    "assertion_names" "sandbox_seeded,br_shim_installed,cargo_rch_shims_installed" \
    "artifact_root_hash" "$(_e2e_hash_string "$artifact_root")" \
    "workspace_mode" "isolated_fixture" \
    "degraded_codes" "agent_mail_unavailable"

cmd="${EE_PACKET_NO_MUTATION_CMD:-}"
packet_exit=0
if [ -n "$cmd" ]; then
    set +e
    (
        cd "$sandbox"
        PATH="$shim_bin:$PATH" \
        EE_PACKET_NO_MUTATION_BR_LOG="$call_log" \
        EE_PACKET_NO_MUTATION_CARGO_LOG="$cargo_log" \
        EE_PACKET_NO_MUTATION_RCH_LOG="$rch_log" \
        AGENT_MAIL_HOME="$mail_root" \
            bash -c "$cmd"
    ) >"$packet_json" 2>"$packet_stderr"
    packet_exit=$?
    set -e
else
    generate_fixture_packet >"$packet_json"
    : >"$packet_stderr"
fi

emit_phase "packet_generation" \
    "command_configured" "$( [ -n "$cmd" ] && printf true || printf false )" \
    "exit_code" "$packet_exit" \
    "stdout_hash" "$(_e2e_hash_file "$packet_json")" \
    "stderr_hash" "$(_e2e_hash_file "$packet_stderr")" \
    "assertion_names" "packet_generation_completed"

fail=0
if [ "$packet_exit" -ne 0 ]; then
    fail=1
    printf 'FAIL: packet generation command exited %s\n' "$packet_exit" >&2
fi

if parse_packet_actions; then
    emit_phase "parse_actions" \
        "command_ids" "$(json_field "$action_summary" "/command_ids")" \
        "copy_safety_values" "$(json_field "$action_summary" "/copy_safety_values")" \
        "argv_hashes" "$(json_field "$action_summary" "/argv_hashes")" \
        "degraded_codes" "$(json_field "$action_summary" "/degraded_codes")" \
        "assertion_names" "command_actions_present"
    emit_phase "safety_assertions" \
        "command_ids" "$(json_field "$action_summary" "/command_ids")" \
        "copy_safety_values" "$(json_field "$action_summary" "/copy_safety_values")" \
        "argv_hashes" "$(json_field "$action_summary" "/argv_hashes")" \
        "degraded_codes" "$(json_field "$action_summary" "/degraded_codes")" \
        "assertion_names" "$(json_field "$action_summary" "/assertion_names")"
else
    fail=1
    emit_phase "parse_actions" \
        "assertion_names" "command_action_parse_failed" \
        "degraded_codes" "unknown"
    emit_phase "safety_assertions" \
        "assertion_names" "command_action_safety_failed" \
        "degraded_codes" "unknown"
fi

consumer_exit=0
set +e
PYTHONDONTWRITEBYTECODE=1 python3 -B "$REPO_ROOT/scripts/agent_consume_work_packet_gate.py" \
    <"$packet_json" >"$consumer_decision" 2>"$consumer_stderr"
consumer_exit=$?
set -e
if [ "$consumer_exit" -ne 0 ] && [ "$consumer_exit" -ne 3 ]; then
    fail=1
    printf 'FAIL: reference consumer exited %s\n' "$consumer_exit" >&2
fi

if parse_consumer_decision; then
    emit_phase "consumer_decision" \
        "exit_code" "$consumer_exit" \
        "stdout_hash" "$(_e2e_hash_file "$consumer_decision")" \
        "stderr_hash" "$(_e2e_hash_file "$consumer_stderr")" \
        "schema" "$(json_field "$consumer_summary" "/schema")" \
        "safe_to_claim" "$(json_field "$consumer_summary" "/safe_to_claim")" \
        "decision" "$(json_field "$consumer_summary" "/decision")" \
        "action" "$(json_field "$consumer_summary" "/action")" \
        "why_not_safe_count" "$(json_field "$consumer_summary" "/why_not_safe_count")" \
        "degraded_summary_count" "$(json_field "$consumer_summary" "/degraded_summary_count")" \
        "argv_action_count" "$(json_field "$consumer_summary" "/argv_action_count")" \
        "max_argv_part_count" "$(json_field "$consumer_summary" "/max_argv_part_count")" \
        "assertion_names" "$(json_field "$consumer_summary" "/assertion_names")"
else
    fail=1
    emit_phase "consumer_decision" \
        "exit_code" "$consumer_exit" \
        "stdout_hash" "$(_e2e_hash_file "$consumer_decision")" \
        "stderr_hash" "$(_e2e_hash_file "$consumer_stderr")" \
        "assertion_names" "consumer_decision_parse_failed"
fi

if run_consumer_fixture_matrix; then
    emit_phase "fixture_matrix_consumer" \
        "summary_hash" "$(_e2e_hash_file "$fixture_matrix_summary")" \
        "fixture_count" "$(json_field "$fixture_matrix_summary" "/fixture_count")" \
        "install_fixture_count" "$(json_field "$fixture_matrix_summary" "/install_fixture_count")" \
        "safe_fixture_count" "$(json_field "$fixture_matrix_summary" "/safe_fixture_count")" \
        "unsafe_fixture_count" "$(json_field "$fixture_matrix_summary" "/unsafe_fixture_count")" \
        "fixture_names" "$(json_field "$fixture_matrix_summary" "/fixture_names")" \
        "install_fixture_names" "$(json_field "$fixture_matrix_summary" "/install_fixture_names")" \
        "decision_summary" "$(json_field "$fixture_matrix_summary" "/decision_summary")" \
        "max_why_not_safe_count" "$(json_field "$fixture_matrix_summary" "/max_why_not_safe_count")" \
        "max_degraded_summary_count" "$(json_field "$fixture_matrix_summary" "/max_degraded_summary_count")" \
        "max_argv_action_count" "$(json_field "$fixture_matrix_summary" "/max_argv_action_count")" \
        "max_argv_part_count" "$(json_field "$fixture_matrix_summary" "/max_argv_part_count")" \
        "assertion_names" "$(json_field "$fixture_matrix_summary" "/assertion_names")"
else
    fail=1
    emit_phase "fixture_matrix_consumer" \
        "summary_hash" "$(_e2e_hash_file "$fixture_matrix_summary")" \
        "assertion_names" "fixture_matrix_consumer_failed"
fi

snapshot_dir "$sandbox/.beads" "$beads_after"
snapshot_dir "$mail_root" "$mail_after"
git -C "$REPO_ROOT" diff --cached --name-only >"$git_index_after"

if ! diff -u "$beads_before" "$beads_after" >"$artifact_root/beads.diff"; then
    fail=1
    printf 'FAIL: .beads/ changed during packet generation\n' >&2
fi
if ! diff -u "$mail_before" "$mail_after" >"$artifact_root/mail.diff"; then
    fail=1
    printf 'FAIL: agent mail store changed during packet generation\n' >&2
fi
if ! diff -u "$git_index_before" "$git_index_after" >"$artifact_root/git_index.diff"; then
    fail=1
    printf 'FAIL: staged git state changed during packet generation\n' >&2
fi

# Refuse any mutating br subcommand that slipped past the shim's
# allowlist. The shim already exits non-zero on those, but we
# double-check the recorded call log for `update`, `sync`, `claim`,
# and `close` strings to catch a regression where the collector
# bypasses the shim by hard-coding /usr/local/bin/br.
mutating_calls=0
if grep -E '(^|[[:space:]])(update|sync|claim|close)([[:space:]]|$)|comments[[:space:]]+add' \
    "$call_log" >/dev/null 2>&1; then
    fail=1
    mutating_calls=1
    printf 'FAIL: mutating br subcommand observed in call log\n' >&2
fi
cargo_calls="$(wc -l <"$cargo_log" | tr -d ' ')"
rch_calls="$(wc -l <"$rch_log" | tr -d ' ')"
if [ "$cargo_calls" -ne 0 ]; then
    fail=1
    printf 'FAIL: Cargo execution observed during packet generation\n' >&2
fi
if [ "$rch_calls" -ne 0 ]; then
    fail=1
    printf 'FAIL: RCH execution observed during packet generation\n' >&2
fi

call_count="$(wc -l <"$call_log" | tr -d ' ')"
emit_phase "no_mutation_checks" \
    "br_call_count" "$call_count" \
    "mutating_calls" "$mutating_calls" \
    "cargo_calls" "$cargo_calls" \
    "rch_calls" "$rch_calls" \
    "assertion_names" "beads_snapshot_unchanged,agent_mail_snapshot_unchanged,git_index_unchanged,no_mutating_br_calls,no_cargo_calls,no_rch_calls"

ok="$( [ "$fail" -eq 0 ] && printf true || printf false )"
emit_phase "final_result" \
    "ok" "$ok" \
    "assertion_names" "final_exit_status"

python3 - "$summary" "$ts" "$artifact_root" "$sandbox" "$call_count" \
    "$mutating_calls" "$cargo_calls" "$rch_calls" "$consumer_exit" \
    "$consumer_summary" "$fixture_matrix_summary" "$ok" <<'PY'
import json
import sys

(
    summary_path,
    ts,
    artifact_root,
    sandbox,
    br_call_count,
    mutating_calls,
    cargo_calls,
    rch_calls,
    consumer_exit,
    consumer_summary_path,
    fixture_matrix_summary_path,
    ok,
) = sys.argv[1:]


def as_int(value):
    try:
        return int(value)
    except (TypeError, ValueError):
        return 0


def load_json(path):
    try:
        with open(path, encoding="utf-8") as handle:
            value = json.load(handle)
    except (FileNotFoundError, json.JSONDecodeError):
        return {}
    return value if isinstance(value, dict) else {}


consumer = load_json(consumer_summary_path)
fixture_matrix = load_json(fixture_matrix_summary_path)

payload = {
    "schema": "ee.packet_no_mutation.v1",
    "ts": ts,
    "artifact_root": artifact_root,
    "sandbox": sandbox,
    "br_call_count": as_int(br_call_count),
    "mutating_calls": as_int(mutating_calls),
    "cargo_calls": as_int(cargo_calls),
    "rch_calls": as_int(rch_calls),
    "consumer_exit": as_int(consumer_exit),
    "consumer_schema": consumer.get("schema") or "",
    "consumer_safe_to_claim": consumer.get("safe_to_claim") or "",
    "consumer_decision": consumer.get("decision") or "",
    "consumer_action": consumer.get("action") or "",
    "consumer_why_not_safe_count": as_int(consumer.get("why_not_safe_count")),
    "consumer_degraded_summary_count": as_int(consumer.get("degraded_summary_count")),
    "consumer_argv_action_count": as_int(consumer.get("argv_action_count")),
    "consumer_max_argv_part_count": as_int(consumer.get("max_argv_part_count")),
    "fixture_count": as_int(fixture_matrix.get("fixture_count")),
    "install_fixture_count": as_int(fixture_matrix.get("install_fixture_count")),
    "safe_fixture_count": as_int(fixture_matrix.get("safe_fixture_count")),
    "unsafe_fixture_count": as_int(fixture_matrix.get("unsafe_fixture_count")),
    "fixture_names": fixture_matrix.get("fixture_names") or "",
    "install_fixture_names": fixture_matrix.get("install_fixture_names") or "",
    "fixture_decision_summary": fixture_matrix.get("decision_summary") or "",
    "fixture_max_why_not_safe_count": as_int(
        fixture_matrix.get("max_why_not_safe_count")
    ),
    "fixture_max_degraded_summary_count": as_int(
        fixture_matrix.get("max_degraded_summary_count")
    ),
    "fixture_max_argv_action_count": as_int(
        fixture_matrix.get("max_argv_action_count")
    ),
    "fixture_max_argv_part_count": as_int(
        fixture_matrix.get("max_argv_part_count")
    ),
    "ok": ok == "true",
}

with open(summary_path, "w", encoding="utf-8") as handle:
    json.dump(payload, handle, sort_keys=True, separators=(",", ":"))
    handle.write("\n")
PY

cat "$summary"

exit "$fail"
