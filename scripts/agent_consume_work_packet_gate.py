#!/usr/bin/env python3
"""Reference consumer for swarm work-packet claim decisions.

Reads an `ee swarm work-packet --json` or
`ee swarm work-packet --claim-gate --json` response from stdin and emits a
small JSON decision for agent harnesses. The consumer never executes commands
and never shell-parses legacy command strings.
"""

import argparse
import json
import re
import sys

OUTPUT_SCHEMA = "ee.agent.work_packet_gate_decision.v1"
CLAIM_GATE_SCHEMA = "ee.swarm.work_packet.claim_gate.v1"
WORK_PACKET_SCHEMA = "ee.swarm.work_packet.v1"
SAFE_COPY = "safe_structured_argv"

SECRET_PATTERNS = [
    re.compile(r"BEGIN (?:OPENSSH )?PRIVATE KEY"),
    re.compile(r"ghp_[A-Za-z0-9_]+"),
    re.compile(r"Bearer [A-Za-z0-9._-]+"),
    re.compile(r"DATABASE_URL=[^\s]+"),
    re.compile(r"\b(?:From|Subject|Message-ID):\s*[^\n]+"),
    re.compile(r"\b(?:raw_inbox|stdout|stderr):[^\n]*"),
    re.compile(r"/(?:Users|home)/[^\s]+"),
]


def redact_text(value, limit=160):
    if value is None:
        return None
    text = str(value).replace("\r", " ").replace("\n", " ").strip()
    for pattern in SECRET_PATTERNS:
        text = pattern.sub("[redacted]", text)
    if len(text) > limit:
        text = text[: limit - 3].rstrip() + "..."
    return text


def compact_list(values, limit=16):
    if not isinstance(values, list):
        return []
    result = []
    seen = set()
    for value in values:
        text = redact_text(value)
        if not text or text in seen:
            continue
        seen.add(text)
        result.append(text)
        if len(result) >= limit:
            break
    return result


def get_payload(response):
    if isinstance(response, dict) and response.get("success") is False:
        return None
    if isinstance(response, dict) and isinstance(response.get("data"), dict):
        return response["data"]
    return response if isinstance(response, dict) else None


def degraded_summary(payload, envelope_degraded=None):
    entries = []

    def add(code, source=None, severity=None):
        code = redact_text(code, 96)
        if not code:
            return
        entry = {
            "code": code,
            "source": redact_text(source, 48),
            "severity": redact_text(severity, 32),
        }
        if entry not in entries:
            entries.append(entry)

    def add_objects(values):
        if not isinstance(values, list):
            return
        for item in values:
            if isinstance(item, dict):
                add(item.get("code"), item.get("source"), item.get("severity"))
            else:
                add(item)

    if isinstance(payload, dict):
        add_objects(payload.get("degraded"))
        add_objects(payload.get("degradedCodes"))
        add_objects(payload.get("sourceProvenance"))
        agent_mail = payload.get("coordination", {}).get("agentMail", {})
        for code in compact_list(agent_mail.get("degradedCodes")):
            add(code, "agent_mail")
        rch = payload.get("rchProofPosture", {})
        for code in compact_list(rch.get("blockerCodes")):
            add(code, "rch")
        for blocker in rch.get("knownBlockers", []) if isinstance(rch, dict) else []:
            if not isinstance(blocker, dict):
                continue
            add(blocker.get("code"), "rch")
            for code in compact_list(blocker.get("degradedCodes")):
                add(code, "rch")
    add_objects(envelope_degraded)
    return entries


def source_summary_from_gate(gate):
    authority = gate.get("sourceAuthority")
    if not isinstance(authority, dict):
        authority = {}
    return {
        "trackerHealth": redact_text(authority.get("trackerHealth"), 64),
        "trackerAuthoritative": authority.get("trackerAuthoritative"),
        "requiresCandidateDowngrade": None,
        "agentMailStatus": redact_text(authority.get("agentMailStatus"), 64),
        "reservationAuthoritative": authority.get("reservationAuthoritative"),
        "inboxAuthoritative": authority.get("inboxAuthoritative"),
        "rchPosture": None,
        "rchSafeToLaunchCargoVerification": authority.get(
            "rchSafeToLaunchCargoVerification"
        ),
        "sourceCount": authority.get("sourceCount"),
    }


def source_summary_from_packet(packet):
    tracker = packet.get("trackerIntegrity", {})
    coordination = packet.get("coordination", {})
    agent_mail = coordination.get("agentMail", {})
    agent_mail_status = agent_mail.get("status") or coordination.get("agentMailHealth")
    rch = packet.get("rchProofPosture", {})
    legacy_verification = packet.get("verification", {})
    return {
        "trackerHealth": redact_text(tracker.get("health"), 64),
        "trackerAuthoritative": tracker.get("brReadsAuthoritative"),
        "requiresCandidateDowngrade": tracker.get("requiresCandidateDowngrade"),
        "agentMailStatus": redact_text(agent_mail_status, 64),
        "reservationAuthoritative": agent_mail.get("reservationAuthoritative"),
        "inboxAuthoritative": agent_mail.get("inboxAuthoritative"),
        "rchPosture": redact_text(
            rch.get("posture") or legacy_verification.get("rchPosture"), 64
        ),
        "rchSafeToLaunchCargoVerification": rch.get("safeToLaunchCargoVerification")
        if "safeToLaunchCargoVerification" in rch
        else legacy_verification.get("remoteOnlySafe"),
        "sourceCount": len(packet.get("sourceProvenance", [])),
    }


def classify_action(action, safe_to_claim, action_kind):
    argv_input = action.get("argv")
    argv = []
    argv_redacted = False
    if isinstance(argv_input, list):
        for part in argv_input:
            raw = str(part)
            redacted = redact_text(raw, 120)
            argv_redacted = argv_redacted or redacted != raw
            argv.append(redacted)

    copy_safety = redact_text(action.get("copySafety") or "display_only", 48)
    shell_required = action.get("shellRequired") is True
    mutates_state = action.get("mutatesState") is True
    has_safe_argv = (
        copy_safety == SAFE_COPY
        and not shell_required
        and bool(argv)
        and not argv_redacted
    )
    runnable = has_safe_argv and (not mutates_state or safe_to_claim)

    if not argv:
        reason = "missing_structured_argv"
    elif argv_redacted:
        reason = "argv_redacted"
    elif shell_required:
        reason = "shell_required_review"
    elif copy_safety != SAFE_COPY:
        reason = f"copy_safety:{copy_safety}"
    elif mutates_state and not safe_to_claim:
        reason = "mutating_action_requires_safe_gate"
    else:
        reason = SAFE_COPY

    return {
        "commandId": redact_text(action.get("commandId") or "unknown", 96),
        "actionKind": action_kind,
        "argv": argv if has_safe_argv else [],
        "runnable": runnable,
        "reviewRequired": not runnable,
        "mutatesState": mutates_state,
        "requiredSubstrate": redact_text(action.get("requiredSubstrate"), 48),
        "when": redact_text(action.get("when"), 96),
        "copySafety": copy_safety,
        "reason": reason,
    }


def command_actions_from_gate(gate, safe_to_claim):
    actions = []
    for action in gate.get("nextCommandActions", []):
        if isinstance(action, dict):
            actions.append(classify_action(action, safe_to_claim, "inspection"))
    claim_action = gate.get("claimCommandAction")
    if isinstance(claim_action, dict):
        actions.append(classify_action(claim_action, safe_to_claim, "claim"))
    return actions


def command_actions_from_packet(packet, safe_to_claim):
    actions = []
    legacy_refused = 0
    recommended = packet.get("recommendedAction", {})
    for action in recommended.get("suggestedCommandActions", []):
        if isinstance(action, dict):
            actions.append(classify_action(action, safe_to_claim, "recommended"))
    legacy_refused += len(recommended.get("suggestedCommands", []))

    verification = packet.get("verification", {})
    for key in ("requiredCommands", "staticChecks"):
        for command in verification.get(key, []):
            if not isinstance(command, dict):
                continue
            if isinstance(command.get("commandAction"), dict):
                actions.append(
                    classify_action(command["commandAction"], safe_to_claim, key)
                )
            elif command.get("commandTemplate"):
                legacy_refused += 1

    agent_mail = packet.get("coordination", {}).get("agentMail", {})
    for fallback in agent_mail.get("fallbackActions", []):
        if not isinstance(fallback, dict):
            continue
        if isinstance(fallback.get("commandAction"), dict):
            actions.append(
                classify_action(fallback["commandAction"], safe_to_claim, "fallback")
            )
        elif fallback.get("command"):
            legacy_refused += 1

    legacy_refused += sum(
        1
        for action in packet.get("requiredActions", [])
        if isinstance(action, dict) and action.get("command")
    )
    return actions, legacy_refused


def mutating_actions_require_human(argv_actions, safe_to_claim):
    mutating = [action for action in argv_actions if action["mutatesState"]]
    return bool(mutating) and (
        not safe_to_claim or any(action["reviewRequired"] for action in mutating)
    )


def selected_candidate(packet):
    recommended = packet.get("recommendedAction", {})
    candidate_id = recommended.get("candidateId")
    candidates = packet.get("candidates")
    if isinstance(candidates, list):
        for candidate in candidates:
            if isinstance(candidate, dict) and candidate.get("id") == candidate_id:
                return candidate
        for candidate in candidates:
            if isinstance(candidate, dict):
                return candidate
    lane = packet.get("candidateLane")
    if isinstance(lane, dict):
        return {
            "id": lane.get("beadId"),
            "decision": lane.get("decision"),
            "unsafeReasons": lane.get("decisionReasons", []),
            "staleReasons": [],
            "sourceRefs": [],
        }
    return None


def packet_safe_to_claim(packet, candidate):
    recommended = packet.get("recommendedAction", {})
    raw_safe = packet.get("safeToClaim")
    if raw_safe is None:
        raw_safe = recommended.get("safeToClaim")
    decision = candidate.get("decision") if isinstance(candidate, dict) else None
    tracker = packet.get("trackerIntegrity", {})
    agent_mail = packet.get("coordination", {}).get("agentMail", {})
    rch = packet.get("rchProofPosture", {})
    legacy_verification = packet.get("verification", {})
    tracker_authoritative = tracker.get("brReadsAuthoritative")
    rch_safe = (
        rch.get("safeToLaunchCargoVerification")
        if "safeToLaunchCargoVerification" in rch
        else legacy_verification.get("remoteOnlySafe")
    )
    return (
        raw_safe is True
        and decision == "safe_to_claim"
        and tracker_authoritative is not False
        and agent_mail.get("status") != "semantic_readiness_failed"
        and rch_safe is not False
    )


def packet_action(packet, candidate, safe_to_claim):
    recommended = packet.get("recommendedAction", {})
    if recommended.get("action"):
        return recommended["action"]
    if safe_to_claim:
        return "inspect_and_claim"
    decision = candidate.get("decision") if isinstance(candidate, dict) else None
    if decision == "stale_but_reclaimable":
        return "reopen_stale_work"
    if decision in ("coordinate_first", "unsafe_due_to_conflict"):
        return "coordinate_before_claim"
    return "blocked_no_action"


def packet_why_not_safe(packet, candidate, safe_to_claim):
    if safe_to_claim:
        return []

    reasons = []
    recommended = packet.get("recommendedAction", {})
    action = packet_action(packet, candidate, safe_to_claim)
    raw_safe = packet.get("safeToClaim")
    if raw_safe is None:
        raw_safe = recommended.get("safeToClaim")

    if not isinstance(candidate, dict):
        reasons.append("no_candidate_available")
    else:
        decision = candidate.get("decision") or "unknown"
        if decision != "safe_to_claim":
            reasons.append(f"candidate_decision:{decision}")
        reasons.extend(compact_list(candidate.get("unsafeReasons")))
        reasons.extend(compact_list(candidate.get("staleReasons")))
        if decision == "stale_but_reclaimable":
            reasons.append("stale_but_reclaimable_requires_inspection")

    tracker = packet.get("trackerIntegrity", {})
    if tracker.get("brReadsAuthoritative") is False:
        reasons.append(
            f"beads_tracker_not_authoritative:{redact_text(tracker.get('health'), 64)}"
        )
    if tracker.get("requiresCandidateDowngrade") is True:
        reasons.append("tracker_requires_candidate_downgrade")

    agent_mail = packet.get("coordination", {}).get("agentMail", {})
    if agent_mail.get("status") == "semantic_readiness_failed":
        reasons.append("agent_mail_semantic_readiness_failed")
    if raw_safe is not True:
        reasons.append(f"packet_recommendation_not_claim_safe:{redact_text(action, 64)}")

    rch = packet.get("rchProofPosture", {})
    legacy_verification = packet.get("verification", {})
    rch_safe = (
        rch.get("safeToLaunchCargoVerification")
        if "safeToLaunchCargoVerification" in rch
        else legacy_verification.get("remoteOnlySafe")
    )
    if rch_safe is False:
        reasons.append("rch_remote_verification_blocked")
    reasons.extend(compact_list(packet.get("doNotProceedBecause")))
    return compact_list(reasons)


def claim_gate_consistency_reasons(gate):
    reasons = []
    if gate.get("safeToClaim") is not True:
        reasons.append("claim_gate_safe_flag_not_true")

    verdict = gate.get("verdict")
    if verdict != "safe_to_claim":
        reasons.append(f"claim_gate_verdict:{redact_text(verdict or 'unknown', 64)}")

    if gate.get("recommendedSafeToClaim") is not True:
        reasons.append("claim_gate_recommended_not_safe")

    candidate = gate.get("selectedCandidate")
    if not isinstance(candidate, dict):
        reasons.append("claim_gate_candidate_missing")
    else:
        candidate_decision = candidate.get("decision")
        if candidate_decision != "safe_to_claim":
            reasons.append(
                "claim_gate_candidate_decision:"
                f"{redact_text(candidate_decision or 'unknown', 64)}"
            )

    if not isinstance(gate.get("claimCommandAction"), dict):
        reasons.append("claim_gate_missing_claim_action")

    authority = gate.get("sourceAuthority")
    if not isinstance(authority, dict):
        reasons.append("claim_gate_source_authority_missing")
        return compact_list(reasons)

    if authority.get("trackerAuthoritative") is not True:
        tracker_health = redact_text(authority.get("trackerHealth") or "unknown", 64)
        reasons.append(f"claim_gate_tracker_not_authoritative:{tracker_health}")
    if authority.get("agentMailStatus") == "semantic_readiness_failed":
        reasons.append("agent_mail_semantic_readiness_failed")
    if authority.get("rchSafeToLaunchCargoVerification") is False:
        reasons.append("rch_remote_verification_blocked")

    return compact_list(reasons)


def consume_claim_gate(gate, envelope_degraded=None):
    consistency_reasons = claim_gate_consistency_reasons(gate)
    safe_to_claim = not consistency_reasons
    candidate = gate.get("selectedCandidate")
    candidate_id = None
    if isinstance(candidate, dict):
        candidate_id = candidate.get("id")
    candidate_id = candidate_id or gate.get("requestedCandidateId")

    why_not_safe = list(consistency_reasons)
    why_not_safe.extend(compact_list(gate.get("unsafeReasons")))
    why_not_safe.extend(compact_list(gate.get("staleReasons")))
    if not safe_to_claim and not why_not_safe:
        why_not_safe.append(f"verdict:{redact_text(gate.get('verdict') or 'unknown', 64)}")

    argv_actions = command_actions_from_gate(gate, safe_to_claim)
    return {
        "schema": OUTPUT_SCHEMA,
        "sourceSchema": CLAIM_GATE_SCHEMA,
        "safeToClaim": safe_to_claim,
        "candidateId": redact_text(candidate_id, 96),
        "decision": redact_text(gate.get("verdict") or "unknown", 64),
        "action": redact_text(gate.get("recommendedAction") or "unknown", 64),
        "argvActions": argv_actions,
        "mutatingActionsRequireHuman": mutating_actions_require_human(
            argv_actions, safe_to_claim
        ),
        "whyNotSafe": compact_list(why_not_safe),
        "sourceSummary": source_summary_from_gate(gate),
        "degradedSummary": degraded_summary(gate, envelope_degraded),
        "legacyCommandStringsRefused": 0,
    }


def consume_work_packet(packet, envelope_degraded=None):
    candidate = selected_candidate(packet)
    safe_to_claim = packet_safe_to_claim(packet, candidate)
    recommended = packet.get("recommendedAction", {})
    candidate_id = None
    decision = "no_candidate"
    if isinstance(candidate, dict):
        candidate_id = candidate.get("id")
        decision = candidate.get("decision") or decision
    candidate_id = candidate_id or recommended.get("candidateId")
    action = packet_action(packet, candidate, safe_to_claim)

    argv_actions, legacy_refused = command_actions_from_packet(packet, safe_to_claim)
    return {
        "schema": OUTPUT_SCHEMA,
        "sourceSchema": WORK_PACKET_SCHEMA,
        "safeToClaim": safe_to_claim,
        "candidateId": redact_text(candidate_id, 96),
        "decision": redact_text(decision, 64),
        "action": redact_text(action, 64),
        "argvActions": argv_actions,
        "mutatingActionsRequireHuman": mutating_actions_require_human(
            argv_actions, safe_to_claim
        ),
        "whyNotSafe": packet_why_not_safe(packet, candidate, safe_to_claim),
        "sourceSummary": source_summary_from_packet(packet),
        "degradedSummary": degraded_summary(packet, envelope_degraded),
        "legacyCommandStringsRefused": legacy_refused,
    }


def consume(response):
    if not isinstance(response, dict):
        return error_decision("invalid_json_shape")
    if response.get("success") is False:
        error = response.get("error", {})
        code = error.get("code") if isinstance(error, dict) else "unknown_error"
        return error_decision(code or "unknown_error")

    payload = get_payload(response)
    if not isinstance(payload, dict):
        return error_decision("missing_payload")

    envelope_degraded = response.get("degraded") if response is not payload else None
    schema = payload.get("schema")
    if schema == CLAIM_GATE_SCHEMA:
        return consume_claim_gate(payload, envelope_degraded)
    if schema == WORK_PACKET_SCHEMA:
        return consume_work_packet(payload, envelope_degraded)
    return error_decision(f"unsupported_schema:{redact_text(schema or 'unknown', 64)}")


def error_decision(code):
    code = redact_text(code or "unknown_error", 96)
    return {
        "schema": OUTPUT_SCHEMA,
        "sourceSchema": None,
        "safeToClaim": False,
        "candidateId": None,
        "decision": "error",
        "action": "blocked_no_action",
        "argvActions": [],
        "mutatingActionsRequireHuman": False,
        "whyNotSafe": [f"error:{code}"],
        "sourceSummary": {
            "trackerHealth": None,
            "trackerAuthoritative": None,
            "requiresCandidateDowngrade": None,
            "agentMailStatus": None,
            "reservationAuthoritative": None,
            "inboxAuthoritative": None,
            "rchPosture": None,
            "rchSafeToLaunchCargoVerification": None,
            "sourceCount": 0,
        },
        "degradedSummary": [],
        "legacyCommandStringsRefused": 0,
    }


def load_response():
    try:
        return json.load(sys.stdin)
    except json.JSONDecodeError as error:
        print(f"Error decoding JSON response: {error}", file=sys.stderr)
        sys.exit(1)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--from-stdin", action="store_true")
    parser.add_argument("--pretty", action="store_true")
    args = parser.parse_args()

    decision = consume(load_response())
    if decision["safeToClaim"]:
        exit_code = 0
    elif decision["decision"] == "error":
        exit_code = 2
    else:
        exit_code = 3

    if args.pretty:
        print(json.dumps(decision, indent=2, sort_keys=True))
    else:
        print(json.dumps(decision, sort_keys=True, separators=(",", ":")))
    sys.exit(exit_code)


if __name__ == "__main__":
    main()
