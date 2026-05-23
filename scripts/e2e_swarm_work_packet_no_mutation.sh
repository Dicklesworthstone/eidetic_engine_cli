#!/usr/bin/env bash
# E2E smoke (bd-2z5ly.9, bd-13dmm.4): proves that swarm work-packet generation
# and command-action consumption stay advisory — no `br update`, no `br sync`,
# no edits to `.beads/`, no staged git changes, no Agent Mail writes, and no
# Cargo/RCH execution while parsing safe argv metadata.
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
# The shim refuses anything other than read-only `br ready` /
# `br doctor` / `br list` invocations so an accidental mutation in the
# packet collector trips the script immediately rather than corrupting
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
git_index_before="$artifact_root/git_index_before.txt"
git_index_after="$artifact_root/git_index_after.txt"
forbidden_log_dir="$artifact_root/forbidden_calls"
cargo_log="$forbidden_log_dir/cargo.log"
rch_log="$forbidden_log_dir/rch.log"

mkdir -p "$shim_bin" "$sandbox/.beads" "$mail_root" "$forbidden_log_dir"

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
    "coordination": {
      "agentMail": {
        "fallbackActions": []
      }
    },
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
with open(packet_path, encoding="utf-8") as handle:
    root = json.load(handle)
packet = root.get("data", root) if isinstance(root, dict) else {}

actions = []

def add_action(path, action):
    if isinstance(action, dict):
        actions.append((path, action))

recommended = packet.get("recommendedAction") or {}
for index, action in enumerate(recommended.get("suggestedCommandActions") or []):
    add_action(f"recommendedAction.suggestedCommandActions[{index}]", action)

verification = packet.get("verification") or {}
for section in ("requiredCommands", "staticChecks"):
    for index, command in enumerate(verification.get(section) or []):
        add_action(f"verification.{section}[{index}].commandAction", command.get("commandAction"))

agent_mail = ((packet.get("coordination") or {}).get("agentMail") or {})
for index, fallback in enumerate(agent_mail.get("fallbackActions") or []):
    add_action(f"coordination.agentMail.fallbackActions[{index}].commandAction", fallback.get("commandAction"))

failures = []
assertions = []

def check(name, passed, detail=""):
    assertions.append(name)
    if not passed:
        failures.append(f"{name}{':' + detail if detail else ''}")

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

mutation_policy = packet.get("mutationPolicy") or {}
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

degraded = packet.get("degraded") or root.get("degraded") or []
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

printf '{"schema":"ee.packet_no_mutation.v1","ts":"%s","artifact_root":"%s","sandbox":"%s","br_call_count":%s,"mutating_calls":%s,"ok":%s}\n' \
    "$ts" "$artifact_root" "$sandbox" "$call_count" "$mutating_calls" \
    "$ok" \
    >"$summary"

cat "$summary"

exit "$fail"
