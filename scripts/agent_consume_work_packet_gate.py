#!/usr/bin/env python3
"""Reference consumer for swarm work-packet claim decisions.

Reads an `ee swarm work-packet --json` or
`ee swarm work-packet --claim-gate --json` response from stdin and emits a
small JSON decision for agent harnesses. The consumer never executes commands;
legacy command strings are used only as diagnostic provenance.
"""

import argparse
import json
import re
import shlex
import sys

OUTPUT_SCHEMA = "ee.agent.work_packet_gate_decision.v1"
CLAIM_GATE_SCHEMA = "ee.swarm.work_packet.claim_gate.v1"
WORK_PACKET_SCHEMA = "ee.swarm.work_packet.v1"
INSTALL_CHECK_SCHEMA = "ee.install.check.v1"
INSTALL_FRESHNESS_SCHEMA = "ee.install.freshness.v1"
SAFE_COPY = "safe_structured_argv"
DECISION_DIAGNOSTIC_LIMIT = 16
DECISION_ACTION_LIMIT = 16
DECISION_ARGV_PART_LIMIT = 32
COPY_SAFETY_VALUES = {
    "safe_structured_argv",
    "display_only",
    "shell_required_review",
    "forbidden_until_human_approval",
}
SHELL_REQUIRED_COPY_SAFETY_VALUES = {
    "shell_required_review",
    "forbidden_until_human_approval",
}
COMMAND_SUBSTRATE_VALUES = {
    "agent_mail",
    "beads",
    "bv",
    "ee",
    "git",
    "human",
    "jq",
    "rch",
    "static_local",
    "none",
}
COMMAND_ID_PATTERN = re.compile(r"^[A-Za-z0-9_.:-]+$")
CLAIM_GATE_REQUIRED_FIELDS = [
    ("gateId", "missing_claim_gate_gate_id"),
    ("packetId", "missing_claim_gate_packet_id"),
    ("workspace", "missing_claim_gate_workspace"),
    ("redactionStatus", "missing_claim_gate_redaction_status"),
    ("requestedCandidateId", "missing_claim_gate_requested_candidate_id"),
    ("verdict", "missing_claim_gate_verdict"),
    ("safeToClaim", "missing_claim_gate_safe_to_claim"),
    ("selectedCandidate", "missing_claim_gate_selected_candidate"),
    ("recommendedAction", "missing_claim_gate_recommended_action"),
    ("recommendedSafeToClaim", "missing_claim_gate_recommended_safe_to_claim"),
    ("sourceAuthority", "missing_claim_gate_source_authority"),
    ("actionableQueue", "missing_claim_gate_actionable_queue"),
    ("unsafeReasons", "missing_claim_gate_unsafe_reasons"),
    ("staleReasons", "missing_claim_gate_stale_reasons"),
    ("sourceRefs", "missing_claim_gate_source_refs"),
    ("degradedCodes", "missing_claim_gate_degraded_codes"),
    ("nextCommandActions", "missing_claim_gate_next_command_actions"),
    ("claimCommandAction", "missing_claim_gate_claim_command_action"),
    ("recoveryActions", "missing_claim_gate_recovery_actions"),
]
CLAIM_GATE_SOURCE_AUTHORITY_REQUIRED_FIELDS = [
    ("trackerAuthoritative", "missing_claim_gate_tracker_authoritative"),
    ("trackerHealth", "missing_claim_gate_tracker_health"),
    ("agentMailStatus", "missing_claim_gate_agent_mail_status"),
    ("reservationAuthoritative", "missing_claim_gate_reservation_authoritative"),
    ("inboxAuthoritative", "missing_claim_gate_inbox_authoritative"),
    ("rchRemoteOnlyRequired", "missing_claim_gate_rch_remote_only_required"),
    (
        "rchSafeToLaunchCargoVerification",
        "missing_claim_gate_rch_safe_to_launch_cargo_verification",
    ),
    ("environmentVerdict", "missing_claim_gate_environment_verdict"),
    ("sourceTestVerdict", "missing_claim_gate_source_test_verdict"),
    (
        "remoteVerificationAdmitted",
        "missing_claim_gate_remote_verification_admitted",
    ),
    (
        "localCargoFallbackObserved",
        "missing_claim_gate_local_cargo_fallback_observed",
    ),
    (
        "installFreshnessVerdict",
        "missing_claim_gate_install_freshness_verdict",
    ),
    (
        "installFreshnessAuthoritative",
        "missing_claim_gate_install_freshness_authoritative",
    ),
    ("installFreshnessRepair", "missing_claim_gate_install_freshness_repair"),
    ("sourceCount", "missing_claim_gate_source_count"),
]
CLAIM_GATE_ACTIONABLE_QUEUE_REQUIRED_FIELDS = [
    ("commandId", "command_id"),
    ("displayCommand", "display_command"),
    ("mutatesState", "mutates_state"),
    ("collectionMode", "collection_mode"),
    ("queueState", "queue_state"),
    ("exitClass", "exit_class"),
    ("authoritative", "authoritative"),
    ("rowCount", "row_count"),
    ("candidateIds", "candidate_ids"),
    ("truncatedCandidateCount", "truncated_candidate_count"),
    ("filterContract", "filter_contract"),
    ("exclusionAccounting", "exclusion_accounting"),
    ("candidateState", "candidate_state"),
    ("bvAdvisoryContradiction", "bv_advisory_contradiction"),
    ("trackerAuthorityDegraded", "tracker_authority_degraded"),
    ("contradictionEvidence", "contradiction_evidence"),
]
CLAIM_GATE_COMMAND_ACTION_REQUIRED_FIELDS = [
    ("commandId", "command_id"),
    ("displayCommand", "display_command"),
    ("argv", "argv"),
    ("shellRequired", "shell_required"),
    ("copySafety", "copy_safety"),
    ("mutatesState", "mutates_state"),
    ("requiredSubstrate", "required_substrate"),
    ("when", "when"),
    ("rationale", "rationale"),
]
COMMAND_ACTION_ALLOWED_FIELDS = {
    field for field, _suffix in CLAIM_GATE_COMMAND_ACTION_REQUIRED_FIELDS
}
COMMAND_ACTION_SAFE_STRING_FIELDS = [
    ("displayCommand", "display_command"),
    ("when", "when"),
    ("rationale", "rationale"),
]
STALE_CLAIM_GATE_UNEXPECTED_ARG_PATTERN = re.compile(
    r"unexpected argument [`'\"](--claim-gate|--candidate)[`'\"]"
)
STALE_ENVIRONMENT_ATTESTATION_UNRECOGNIZED_PATTERN = re.compile(
    r"unrecognized subcommand [`'\"]environment-attestation[`'\"]"
)
AUTHORITY_DEGRADED_BLOCKER_CODES = {
    "agent_mail_probe_mismatch",
    "agent_mail_semantic_readiness_failed",
    "agent_mail_unavailable",
    "beads_command_timeout",
    "beads_db_jsonl_count_mismatch",
    "beads_metadata_only_stale",
    "beads_tracker_metadata_drift",
    "beads_tracker_stale",
    "build_admission_blocked",
    "bv_command_timeout",
    "bv_no_output",
    "bv_recommendation_stale",
    "bv_unavailable",
    "local_cargo_bypass_detected",
    "reservation_evidence_stale",
    "rch_remote_required_fallback_prevented",
    "rch_selector_admission_contradiction",
    "rch_unavailable",
    "rch_verify_local_fallback_refused",
    "rch_verify_topology_blocked",
    "rch_worker_topology_blocked",
    "source_authority_ambiguous",
    "stale_binary_suspected",
}
MACHINE_RESPONSE_SCHEMAS = {
    "ee.error.v2",
    "ee.response.v2",
    CLAIM_GATE_SCHEMA,
    WORK_PACKET_SCHEMA,
    INSTALL_CHECK_SCHEMA,
}

SECRET_PATTERNS = [
    re.compile(r"BEGIN (?:[A-Z0-9]+ )?PRIVATE KEY"),
    re.compile(r"\bgh[opurs]_[A-Za-z0-9_]+\b"),
    re.compile(r"\bgithub_pat_[A-Za-z0-9_]{20,}\b"),
    re.compile(r"\bsk-(?:proj-)?[A-Za-z0-9_-]{20,}\b"),
    re.compile(r"\b(?:AKIA|ASIA)[A-Z0-9]{16}\b"),
    re.compile(r"\bxox[abprs]-[A-Za-z0-9-]{10,}\b"),
    re.compile(r"Bearer [A-Za-z0-9._-]+"),
    re.compile(r"\b(?:DATABASE_URL|TOKEN|API_KEY|SECRET|PASSWORD)=[^\s]*"),
    re.compile(r"\b(?:From|Subject|Message-ID):[^\n]*"),
    re.compile(r"\bbody:[^\n]*"),
    re.compile(r"\braw_inbox\b|\b(?:stdout|stderr):[^\n]*"),
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


def compact_list(values, limit=DECISION_DIAGNOSTIC_LIMIT):
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


def list_items(value):
    return value if isinstance(value, list) else []


def dict_or_empty(value):
    return value if isinstance(value, dict) else {}


def field_value(container, *names):
    if not isinstance(container, dict):
        return None
    for name in names:
        if name in container:
            return container.get(name)
    return None


def bool_or_none(value):
    return value if isinstance(value, bool) else None


def nonnegative_int_or_none(value):
    if isinstance(value, bool):
        return None
    if isinstance(value, int) and value >= 0:
        return value
    return None


def text_requires_redaction(value):
    if not isinstance(value, str):
        return False
    return any(pattern.search(value) for pattern in SECRET_PATTERNS)


def safe_command_string_malformed(value):
    return (
        not isinstance(value, str)
        or not value.strip()
        or text_requires_redaction(value)
    )


def malformed_boolean_field_reasons(container, field_reasons):
    if not isinstance(container, dict):
        return []

    reasons = []
    for field, reason in field_reasons:
        value = container.get(field)
        if field in container and value is not None and not isinstance(value, bool):
            reasons.append(reason)
    return reasons


def malformed_packet_map_reasons(packet):
    reasons = []
    for key, reason in [
        ("recommendedAction", "malformed_recommended_action"),
        ("trackerIntegrity", "malformed_tracker_integrity"),
        ("coordination", "malformed_coordination"),
        ("rchProofPosture", "malformed_rch_proof_posture"),
        ("verification", "malformed_verification"),
    ]:
        if (
            key in packet
            and packet.get(key) is not None
            and not isinstance(packet.get(key), dict)
        ):
            reasons.append(reason)

    coordination = dict_or_empty(packet.get("coordination"))
    agent_mail = coordination.get("agentMail")
    if agent_mail is not None and not isinstance(agent_mail, dict):
        reasons.append("malformed_agent_mail")
    return reasons


def malformed_packet_scalar_reasons(packet):
    tracker = dict_or_empty(packet.get("trackerIntegrity"))
    coordination = dict_or_empty(packet.get("coordination"))
    agent_mail = dict_or_empty(coordination.get("agentMail"))
    rch = dict_or_empty(packet.get("rchProofPosture"))
    legacy_verification = dict_or_empty(packet.get("verification"))

    reasons = []
    reasons.extend(
        malformed_boolean_field_reasons(
            tracker,
            [
                ("brReadsAuthoritative", "malformed_tracker_br_reads_authoritative"),
                (
                    "requiresCandidateDowngrade",
                    "malformed_tracker_requires_candidate_downgrade",
                ),
            ],
        )
    )
    reasons.extend(
        malformed_boolean_field_reasons(
            agent_mail,
            [
                (
                    "reservationAuthoritative",
                    "malformed_agent_mail_reservation_authoritative",
                ),
                ("inboxAuthoritative", "malformed_agent_mail_inbox_authoritative"),
            ],
        )
    )
    reasons.extend(
        malformed_boolean_field_reasons(
            rch,
            [
                ("remoteOnlyRequired", "malformed_rch_remote_only_required"),
                (
                    "safeToLaunchCargoVerification",
                    "malformed_rch_safe_to_launch_cargo_verification",
                )
            ],
        )
    )
    reasons.extend(
        malformed_boolean_field_reasons(
            legacy_verification,
            [
                (
                    "remoteOnlyRequired",
                    "malformed_verification_remote_only_required",
                ),
                ("remoteOnlySafe", "malformed_verification_remote_only_safe"),
            ],
        )
    )
    return reasons


def malformed_claim_gate_authority_reasons(gate):
    authority = gate.get("sourceAuthority")
    if not isinstance(authority, dict):
        return []

    reasons = []
    for field, reason in CLAIM_GATE_SOURCE_AUTHORITY_REQUIRED_FIELDS:
        if field not in authority:
            reasons.append(reason)

    reasons.extend(
        malformed_boolean_field_reasons(
            authority,
            [
                ("trackerAuthoritative", "malformed_claim_gate_tracker_authoritative"),
                (
                    "reservationAuthoritative",
                    "malformed_claim_gate_reservation_authoritative",
                ),
                ("inboxAuthoritative", "malformed_claim_gate_inbox_authoritative"),
                (
                    "rchRemoteOnlyRequired",
                    "malformed_claim_gate_rch_remote_only_required",
                ),
                (
                    "rchSafeToLaunchCargoVerification",
                    "malformed_claim_gate_rch_safe_to_launch_cargo_verification",
                ),
                (
                    "remoteVerificationAdmitted",
                    "malformed_claim_gate_remote_verification_admitted",
                ),
                (
                    "localCargoFallbackObserved",
                    "malformed_claim_gate_local_cargo_fallback_observed",
                ),
                (
                    "installFreshnessAuthoritative",
                    "malformed_claim_gate_install_freshness_authoritative",
                ),
            ],
        )
    )
    for field, reason in [
        ("trackerHealth", "malformed_claim_gate_tracker_health"),
        ("agentMailStatus", "malformed_claim_gate_agent_mail_status"),
        ("environmentVerdict", "malformed_claim_gate_environment_verdict"),
        ("sourceTestVerdict", "malformed_claim_gate_source_test_verdict"),
        (
            "installFreshnessVerdict",
            "malformed_claim_gate_install_freshness_verdict",
        ),
        ("installFreshnessRepair", "malformed_claim_gate_install_freshness_repair"),
    ]:
        value = authority.get(field)
        if field in authority and value is not None and not isinstance(value, str):
            reasons.append(reason)

    source_count = authority.get("sourceCount")
    if (
        "sourceCount" in authority
        and source_count is not None
        and nonnegative_int_or_none(source_count) is None
    ):
        reasons.append("malformed_claim_gate_source_count")
    return reasons


def malformed_actionable_queue_reasons(gate):
    queue = gate.get("actionableQueue")
    if not isinstance(queue, dict):
        return []

    reasons = []
    for field, suffix in CLAIM_GATE_ACTIONABLE_QUEUE_REQUIRED_FIELDS:
        if field not in queue:
            reasons.append(f"missing_claim_gate_actionable_queue_{suffix}")

    for field, suffix in [
        ("commandId", "command_id"),
        ("displayCommand", "display_command"),
        ("collectionMode", "collection_mode"),
        ("queueState", "queue_state"),
        ("exitClass", "exit_class"),
        ("candidateState", "candidate_state"),
    ]:
        value = queue.get(field)
        if field in queue and not isinstance(value, str):
            reasons.append(f"malformed_claim_gate_actionable_queue_{suffix}")

    for field, reason in [
        ("mutatesState", "malformed_claim_gate_actionable_queue_mutates_state"),
        ("authoritative", "malformed_claim_gate_actionable_queue_authoritative"),
        (
            "bvAdvisoryContradiction",
            "malformed_claim_gate_actionable_queue_bv_advisory_contradiction",
        ),
        (
            "trackerAuthorityDegraded",
            "malformed_claim_gate_actionable_queue_tracker_authority_degraded",
        ),
    ]:
        value = queue.get(field)
        if field in queue and not isinstance(value, bool):
            reasons.append(reason)

    row_count = queue.get("rowCount")
    if (
        "rowCount" in queue
        and row_count is not None
        and nonnegative_int_or_none(row_count) is None
    ):
        reasons.append("malformed_claim_gate_actionable_queue_row_count")

    truncated_count = queue.get("truncatedCandidateCount")
    if "truncatedCandidateCount" in queue and nonnegative_int_or_none(truncated_count) is None:
        reasons.append("malformed_claim_gate_actionable_queue_truncated_candidate_count")

    for field, suffix in [
        ("candidateIds", "candidate_ids"),
        ("contradictionEvidence", "contradiction_evidence"),
    ]:
        value = queue.get(field)
        if field in queue and not isinstance(value, list):
            reasons.append(f"malformed_claim_gate_actionable_queue_{suffix}")

    for field, suffix in [
        ("filterContract", "filter_contract"),
        ("exclusionAccounting", "exclusion_accounting"),
    ]:
        value = queue.get(field)
        if field in queue and not isinstance(value, dict):
            reasons.append(f"malformed_claim_gate_actionable_queue_{suffix}")

    return reasons


def malformed_command_action_reasons(action, reason_prefix, reason_scope="claim_gate"):
    if not isinstance(action, dict):
        return []

    reasons = []
    for field in action:
        if field not in COMMAND_ACTION_ALLOWED_FIELDS:
            reasons.append(f"malformed_{reason_scope}_{reason_prefix}_unexpected_field")
            break

    for field, suffix in CLAIM_GATE_COMMAND_ACTION_REQUIRED_FIELDS:
        if field not in action:
            reasons.append(f"missing_{reason_scope}_{reason_prefix}_{suffix}")

    for field, suffix in [
        ("commandId", "command_id"),
        ("copySafety", "copy_safety"),
        ("requiredSubstrate", "required_substrate"),
    ]:
        value = action.get(field)
        if field in action and not isinstance(value, str):
            reasons.append(f"malformed_{reason_scope}_{reason_prefix}_{suffix}")

    command_id = action.get("commandId")
    if (
        "commandId" in action
        and isinstance(command_id, str)
        and (
            not command_id.strip()
            or text_requires_redaction(command_id)
            or not COMMAND_ID_PATTERN.match(command_id)
        )
    ):
        reasons.append(f"malformed_{reason_scope}_{reason_prefix}_command_id")

    for field, suffix in COMMAND_ACTION_SAFE_STRING_FIELDS:
        value = action.get(field)
        if field in action and safe_command_string_malformed(value):
            reasons.append(f"malformed_{reason_scope}_{reason_prefix}_{suffix}")

    rationale = action.get("rationale")
    if "rationale" in action and isinstance(rationale, str) and len(rationale) > 240:
        reasons.append(f"malformed_{reason_scope}_{reason_prefix}_rationale")

    copy_safety = action.get("copySafety")
    if (
        "copySafety" in action
        and isinstance(copy_safety, str)
        and copy_safety not in COPY_SAFETY_VALUES
    ):
        reasons.append(f"malformed_{reason_scope}_{reason_prefix}_copy_safety")

    required_substrate = action.get("requiredSubstrate")
    if (
        "requiredSubstrate" in action
        and isinstance(required_substrate, str)
        and required_substrate not in COMMAND_SUBSTRATE_VALUES
    ):
        reasons.append(
            f"malformed_{reason_scope}_{reason_prefix}_required_substrate"
        )

    for field, suffix in [
        ("shellRequired", "shell_required"),
        ("mutatesState", "mutates_state"),
    ]:
        value = action.get(field)
        if field in action and not isinstance(value, bool):
            reasons.append(f"malformed_{reason_scope}_{reason_prefix}_{suffix}")

    argv = action.get("argv")
    if "argv" in action and not isinstance(argv, list):
        reasons.append(f"malformed_{reason_scope}_{reason_prefix}_argv")
    elif isinstance(argv, list) and (
        len(argv) > DECISION_ARGV_PART_LIMIT
        or any(safe_command_string_malformed(part) for part in argv)
    ):
        reasons.append(f"malformed_{reason_scope}_{reason_prefix}_argv")

    shell_required = action.get("shellRequired")
    if isinstance(shell_required, bool) and isinstance(copy_safety, str):
        if shell_required and copy_safety not in SHELL_REQUIRED_COPY_SAFETY_VALUES:
            reasons.append(f"malformed_{reason_scope}_{reason_prefix}_copy_safety")
        if copy_safety == SAFE_COPY and shell_required is not False:
            reasons.append(f"malformed_{reason_scope}_{reason_prefix}_shell_required")

    return reasons


def malformed_packet_command_action_reasons(packet):
    reasons = []

    recommended = dict_or_empty(packet.get("recommendedAction"))
    suggested_actions = recommended.get("suggestedCommandActions")
    if suggested_actions is not None and not isinstance(suggested_actions, list):
        reasons.append("malformed_packet_suggested_command_actions")
    for action in list_items(suggested_actions):
        if not isinstance(action, dict):
            reasons.append("malformed_packet_recommended_command_action")
            continue
        reasons.extend(
            malformed_command_action_reasons(
                action,
                "recommended_command_action",
                "packet",
            )
        )

    verification = dict_or_empty(packet.get("verification"))
    for list_key, list_reason, reason_prefix in [
        (
            "requiredCommands",
            "malformed_packet_required_commands",
            "required_command_action",
        ),
        ("staticChecks", "malformed_packet_static_checks", "static_check_action"),
    ]:
        commands = verification.get(list_key)
        if commands is not None and not isinstance(commands, list):
            reasons.append(list_reason)
            continue
        for command in list_items(commands):
            if not isinstance(command, dict):
                reasons.append(f"malformed_packet_{reason_prefix}")
                continue
            if "commandAction" not in command or command.get("commandAction") is None:
                continue
            command_action = command.get("commandAction")
            if not isinstance(command_action, dict):
                reasons.append(f"malformed_packet_{reason_prefix}")
                continue
            reasons.extend(
                malformed_command_action_reasons(
                    command_action,
                    reason_prefix,
                    "packet",
                )
            )

    coordination = dict_or_empty(packet.get("coordination"))
    agent_mail = dict_or_empty(coordination.get("agentMail"))
    fallback_actions = agent_mail.get("fallbackActions")
    if fallback_actions is not None and not isinstance(fallback_actions, list):
        reasons.append("malformed_packet_fallback_actions")
    for fallback in list_items(fallback_actions):
        if not isinstance(fallback, dict):
            reasons.append("malformed_packet_fallback_action")
            continue
        if "commandAction" not in fallback or fallback.get("commandAction") is None:
            continue
        command_action = fallback.get("commandAction")
        if not isinstance(command_action, dict):
            reasons.append("malformed_packet_fallback_command_action")
            continue
        reasons.extend(
            malformed_command_action_reasons(
                command_action,
                "fallback_command_action",
                "packet",
            )
        )

    return reasons


def malformed_claim_gate_reasons(gate):
    reasons = []
    for field, reason in CLAIM_GATE_REQUIRED_FIELDS:
        if field not in gate:
            reasons.append(reason)

    reasons.extend(
        malformed_boolean_field_reasons(
            gate,
            [
                ("safeToClaim", "malformed_claim_gate_safe_to_claim"),
                (
                    "recommendedSafeToClaim",
                    "malformed_claim_gate_recommended_safe_to_claim",
                ),
            ],
        )
    )

    for field, reason in [
        ("requestedCandidateId", "malformed_claim_gate_requested_candidate_id"),
        ("verdict", "malformed_claim_gate_verdict"),
        ("recommendedAction", "malformed_claim_gate_recommended_action"),
    ]:
        value = gate.get(field)
        if field in gate and value is not None and not isinstance(value, str):
            reasons.append(reason)

    for field, reason in [
        ("unsafeReasons", "malformed_claim_gate_unsafe_reasons"),
        ("staleReasons", "malformed_claim_gate_stale_reasons"),
        ("sourceRefs", "malformed_claim_gate_source_refs"),
        ("degradedCodes", "malformed_claim_gate_degraded_codes"),
        ("nextCommandActions", "malformed_claim_gate_next_command_actions"),
        ("recoveryActions", "malformed_claim_gate_recovery_actions"),
    ]:
        value = gate.get(field)
        if field in gate and value is not None and not isinstance(value, list):
            reasons.append(reason)

    if "actionableQueue" in gate and not isinstance(gate.get("actionableQueue"), dict):
        reasons.append("malformed_claim_gate_actionable_queue")
    reasons.extend(malformed_actionable_queue_reasons(gate))

    next_actions = gate.get("nextCommandActions")
    if isinstance(next_actions, list):
        for action in next_actions:
            if not isinstance(action, dict):
                reasons.append("malformed_claim_gate_next_command_action")
                break
            reasons.extend(
                malformed_command_action_reasons(action, "next_command_action")
            )

    candidate = gate.get("selectedCandidate")
    if candidate is not None and not isinstance(candidate, dict):
        reasons.append("malformed_claim_gate_selected_candidate")
    elif isinstance(candidate, dict):
        for field, reason in [
            ("id", "malformed_claim_gate_candidate_id"),
            ("decision", "malformed_claim_gate_candidate_decision"),
        ]:
            value = candidate.get(field)
            if field in candidate and value is not None and not isinstance(value, str):
                reasons.append(reason)

    claim_action = gate.get("claimCommandAction")
    if isinstance(claim_action, dict):
        reasons.extend(
            malformed_command_action_reasons(claim_action, "claim_command_action")
        )

    return reasons


def agent_mail_authority_reasons(agent_mail):
    reasons = []
    if agent_mail.get("reservationAuthoritative") is not True:
        reasons.append("reservation_evidence_not_authoritative")
    if agent_mail.get("inboxAuthoritative") is not True:
        reasons.append("inbox_evidence_not_authoritative")
    return reasons


def tracker_not_authoritative_reason(tracker):
    if tracker.get("brReadsAuthoritative") is True:
        return None
    return f"beads_tracker_not_authoritative:{redact_text(tracker.get('health') or 'unknown', 64)}"


def rch_remote_verification_required(packet):
    rch = dict_or_empty(packet.get("rchProofPosture"))
    if "remoteOnlyRequired" in rch:
        return rch.get("remoteOnlyRequired") is True
    legacy_verification = dict_or_empty(packet.get("verification"))
    return legacy_verification.get("remoteOnlyRequired") is True


def rch_safe_to_launch_cargo_verification(packet):
    rch = dict_or_empty(packet.get("rchProofPosture"))
    if "safeToLaunchCargoVerification" in rch:
        return rch.get("safeToLaunchCargoVerification")
    legacy_verification = dict_or_empty(packet.get("verification"))
    return legacy_verification.get("remoteOnlySafe")


def rch_remote_verification_reason(packet):
    rch_safe = rch_safe_to_launch_cargo_verification(packet)
    if rch_safe is False:
        return "rch_remote_verification_blocked"
    if rch_remote_verification_required(packet) and rch_safe is not True:
        return "rch_remote_verification_required"
    return None


def candidate_status_reason(candidate):
    if not isinstance(candidate, dict):
        return None
    status = candidate.get("status")
    if status is None:
        return None
    if isinstance(status, str) and status == "open":
        return None
    return f"candidate_status_not_open:{redact_text(status or 'unknown', 64)}"


def count_legacy_command_strings(value):
    if isinstance(value, str):
        return 1 if redact_text(value) else 0
    if not isinstance(value, list):
        return 0
    return sum(1 for item in value if isinstance(item, str) and redact_text(item))


def get_payload(response):
    if isinstance(response, dict) and response.get("success") is False:
        return None
    if isinstance(response, dict) and "data" in response:
        return response["data"]
    return response if isinstance(response, dict) else None


def degraded_summary(payload, envelope_degraded=None):
    entries = []

    def add(code, source=None, severity=None):
        if len(entries) >= DECISION_DIAGNOSTIC_LIMIT:
            return
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
        error = dict_or_empty(payload.get("error"))
        error_details = dict_or_empty(error.get("details"))
        add_objects(error.get("degraded"))
        add_objects(error.get("degradedCodes"))
        add_objects(error_details.get("degraded"))
        add_objects(error_details.get("degradedCodes"))
        add_objects(error_details.get("sourceProvenance"))
        coordination = dict_or_empty(payload.get("coordination"))
        agent_mail = dict_or_empty(coordination.get("agentMail"))
        for code in compact_list(agent_mail.get("degradedCodes")):
            add(code, "agent_mail")
        rch = dict_or_empty(payload.get("rchProofPosture"))
        for code in compact_list(rch.get("blockerCodes")):
            add(code, "rch")
        for blocker in list_items(rch.get("knownBlockers")):
            if not isinstance(blocker, dict):
                continue
            add(blocker.get("code"), "rch")
            for code in compact_list(blocker.get("degradedCodes")):
                add(code, "rch")
    add_objects(envelope_degraded)
    return entries


def authority_degraded_blocker_codes(payload, envelope_degraded=None):
    codes = []

    def add_code(code):
        if not isinstance(code, str):
            return
        code = code.strip()
        if code in AUTHORITY_DEGRADED_BLOCKER_CODES and code not in codes:
            codes.append(code)

    def add_degraded_codes(container, *field_names):
        if not isinstance(container, dict):
            return
        for field_name in field_names:
            for code in list_items(container.get(field_name)):
                add_code(code)

    def add_degraded_items(items):
        for item in list_items(items):
            if isinstance(item, dict):
                add_code(item.get("code"))
            else:
                add_code(item)

    add_degraded_items(envelope_degraded)

    if not isinstance(payload, dict):
        return codes
    add_degraded_codes(payload, "degradedCodes")
    add_degraded_items(payload.get("degraded"))
    for source in list_items(payload.get("sourceProvenance")):
        if not isinstance(source, dict):
            continue
        add_code(source.get("code"))
        add_degraded_codes(source, "degradedCodes")

    source_authority = dict_or_empty(payload.get("sourceAuthority"))
    add_degraded_codes(source_authority, "degradedCodes", "blockerCodes")
    tracker = dict_or_empty(payload.get("trackerIntegrity"))
    add_degraded_codes(tracker, "degradedCodes", "blockerCodes")
    coordination = dict_or_empty(payload.get("coordination"))
    agent_mail = dict_or_empty(coordination.get("agentMail"))
    add_degraded_codes(agent_mail, "degradedCodes", "blockerCodes")
    rch = dict_or_empty(payload.get("rchProofPosture"))
    add_degraded_codes(rch, "degradedCodes", "blockerCodes")
    for selector in [
        dict_or_empty(rch.get("selectorAdmissionProbe")),
        dict_or_empty(rch.get("selector_admission_probe")),
    ]:
        if (
            selector.get("workersVsSelectionContradiction") is True
            or selector.get("workers_vs_selection_contradiction") is True
        ):
            add_code("rch_selector_admission_contradiction")
    for blocker in list_items(rch.get("knownBlockers")):
        if not isinstance(blocker, dict):
            continue
        add_code(blocker.get("code"))
        add_degraded_codes(blocker, "degradedCodes")
    return codes


def authority_degraded_reasons(payload, prefix, envelope_degraded=None):
    return [
        f"{prefix}:{redact_text(code, 96)}"
        for code in authority_degraded_blocker_codes(payload, envelope_degraded)
    ]


def source_summary_from_gate(gate):
    authority = gate.get("sourceAuthority")
    if not isinstance(authority, dict):
        authority = {}
    return {
        "trackerHealth": redact_text(authority.get("trackerHealth"), 64),
        "trackerAuthoritative": bool_or_none(authority.get("trackerAuthoritative")),
        "requiresCandidateDowngrade": None,
        "agentMailStatus": redact_text(authority.get("agentMailStatus"), 64),
        "reservationAuthoritative": bool_or_none(
            authority.get("reservationAuthoritative")
        ),
        "inboxAuthoritative": bool_or_none(authority.get("inboxAuthoritative")),
        "rchPosture": None,
        "rchRemoteOnlyRequired": bool_or_none(
            authority.get("rchRemoteOnlyRequired")
        ),
        "rchSafeToLaunchCargoVerification": bool_or_none(
            authority.get("rchSafeToLaunchCargoVerification")
        ),
        "sourceCount": nonnegative_int_or_none(authority.get("sourceCount")),
    }


def source_summary_from_packet(packet):
    tracker = dict_or_empty(packet.get("trackerIntegrity"))
    coordination = dict_or_empty(packet.get("coordination"))
    agent_mail = dict_or_empty(coordination.get("agentMail"))
    agent_mail_status = agent_mail.get("status") or coordination.get("agentMailHealth")
    rch = dict_or_empty(packet.get("rchProofPosture"))
    legacy_verification = dict_or_empty(packet.get("verification"))
    return {
        "trackerHealth": redact_text(tracker.get("health"), 64),
        "trackerAuthoritative": bool_or_none(tracker.get("brReadsAuthoritative")),
        "requiresCandidateDowngrade": bool_or_none(
            tracker.get("requiresCandidateDowngrade")
        ),
        "agentMailStatus": redact_text(agent_mail_status, 64),
        "reservationAuthoritative": bool_or_none(
            agent_mail.get("reservationAuthoritative")
        ),
        "inboxAuthoritative": bool_or_none(agent_mail.get("inboxAuthoritative")),
        "rchPosture": redact_text(
            rch.get("posture") or legacy_verification.get("rchPosture"), 64
        ),
        "rchRemoteOnlyRequired": rch_remote_verification_required(packet),
        "rchSafeToLaunchCargoVerification": bool_or_none(
            rch_safe_to_launch_cargo_verification(packet)
        ),
        "sourceCount": len(list_items(packet.get("sourceProvenance"))),
    }


def source_summary_for_install_check():
    return {
        "trackerHealth": None,
        "trackerAuthoritative": None,
        "requiresCandidateDowngrade": None,
        "agentMailStatus": None,
        "reservationAuthoritative": None,
        "inboxAuthoritative": None,
        "rchPosture": None,
        "rchRemoteOnlyRequired": None,
        "rchSafeToLaunchCargoVerification": None,
        "sourceCount": 1,
    }


def classify_action(action, safe_to_claim, action_kind):
    argv_input = action.get("argv")
    argv = []
    argv_invalid = False
    argv_too_long = False
    argv_redacted = False
    metadata_invalid = any(
        reason
        for reason in malformed_command_action_reasons(
            action, "runtime_command_action", "action"
        )
        if not reason.endswith("_argv")
    )
    if isinstance(argv_input, list):
        argv_too_long = len(argv_input) > DECISION_ARGV_PART_LIMIT
        for part in argv_input[:DECISION_ARGV_PART_LIMIT]:
            if not isinstance(part, str):
                argv_invalid = True
                continue
            raw = part
            redacted = redact_text(raw, 120)
            if not redacted:
                argv_invalid = True
                continue
            argv_redacted = argv_redacted or redacted != raw
            argv.append(redacted)

    copy_safety = redact_text(action.get("copySafety") or "display_only", 48)
    shell_required = action.get("shellRequired") is True
    mutates_state = (
        action.get("mutatesState") is True
        or action_looks_like_beads_mutation(action)
        or action_kind == "claim"
    )
    has_safe_argv = (
        copy_safety == SAFE_COPY
        and not shell_required
        and bool(argv)
        and not argv_too_long
        and not argv_invalid
        and not argv_redacted
        and not metadata_invalid
    )
    runnable = has_safe_argv and (not mutates_state or safe_to_claim)

    if metadata_invalid:
        reason = "malformed_command_action"
    elif argv_too_long:
        reason = "argv_too_long"
    elif argv_invalid:
        reason = "invalid_argv_item"
    elif not argv:
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


def claim_action_candidate_id(action):
    if not isinstance(action, dict):
        return None
    argv = action.get("argv")
    if not isinstance(argv, list) or len(argv) < 3:
        return None
    if not all(isinstance(part, str) and part.strip() for part in argv[:3]):
        return None
    if argv[0] not in ("br", "bd"):
        return None
    if argv[1] != "update":
        return None
    return argv[2]


def claim_action_sets_in_progress(action):
    if not isinstance(action, dict):
        return False
    argv = action.get("argv")
    if not isinstance(argv, list):
        return False
    status_values = []
    for index, part in enumerate(argv):
        if part == "--status":
            if index + 1 >= len(argv) or not isinstance(argv[index + 1], str):
                return False
            status_values.append(argv[index + 1])
        if isinstance(part, str) and part == "--status=in_progress":
            status_values.append("in_progress")
        elif isinstance(part, str) and part.startswith("--status="):
            status_values.append(part.split("=", 1)[1])
    return status_values == ["in_progress"]


def claim_action_emits_json(action):
    if not isinstance(action, dict):
        return False
    argv = action.get("argv")
    if not isinstance(argv, list):
        return False
    return any(part == "--json" for part in argv if isinstance(part, str))


def claim_action_is_safe_structured_argv(action):
    if not isinstance(action, dict):
        return False
    argv = action.get("argv")
    return (
        action.get("copySafety") == SAFE_COPY
        and action.get("shellRequired") is False
        and isinstance(argv, list)
        and bool(argv)
        and len(argv) <= DECISION_ARGV_PART_LIMIT
        and not any(safe_command_string_malformed(part) for part in argv)
    )


def action_looks_like_beads_mutation(action):
    if not isinstance(action, dict):
        return False
    argv = action.get("argv")
    if not isinstance(argv, list) or len(argv) < 2:
        return False
    if not all(isinstance(part, str) and part.strip() for part in argv[:2]):
        return False
    if argv[0] not in ("br", "bd"):
        return False
    subcommand = argv[1]
    if subcommand in {"claim", "close", "create", "reopen", "sync", "update"}:
        return True
    if subcommand == "dep" and len(argv) >= 3 and argv[2] in {
        "add",
        "rm",
        "remove",
    }:
        return True
    return False


def command_actions_from_gate(gate, safe_to_claim):
    actions = []
    claim_action = gate.get("claimCommandAction")
    inspection_limit = (
        DECISION_ACTION_LIMIT - 1
        if isinstance(claim_action, dict)
        else DECISION_ACTION_LIMIT
    )
    for action in list_items(gate.get("nextCommandActions")):
        if len(actions) >= inspection_limit:
            break
        if isinstance(action, dict):
            actions.append(classify_action(action, safe_to_claim, "inspection"))
    if isinstance(claim_action, dict) and len(actions) < DECISION_ACTION_LIMIT:
        actions.append(classify_action(claim_action, safe_to_claim, "claim"))
    return actions


def command_actions_from_packet(packet, safe_to_claim):
    actions = []
    legacy_refused = 0

    def add_action(action, action_kind):
        if len(actions) < DECISION_ACTION_LIMIT:
            actions.append(classify_action(action, safe_to_claim, action_kind))

    recommended = dict_or_empty(packet.get("recommendedAction"))
    for action in list_items(recommended.get("suggestedCommandActions")):
        if isinstance(action, dict):
            add_action(action, "recommended")
    legacy_refused += count_legacy_command_strings(recommended.get("suggestedCommands"))

    verification = dict_or_empty(packet.get("verification"))
    for key in ("requiredCommands", "staticChecks"):
        for command in list_items(verification.get(key)):
            if not isinstance(command, dict):
                continue
            if isinstance(command.get("commandAction"), dict):
                add_action(command["commandAction"], key)
            elif command.get("commandTemplate"):
                legacy_refused += 1

    coordination = dict_or_empty(packet.get("coordination"))
    agent_mail = dict_or_empty(coordination.get("agentMail"))
    for fallback in list_items(agent_mail.get("fallbackActions")):
        if not isinstance(fallback, dict):
            continue
        if isinstance(fallback.get("commandAction"), dict):
            add_action(fallback["commandAction"], "fallback")
        elif fallback.get("command"):
            legacy_refused += 1

    for action in list_items(packet.get("requiredActions")):
        if isinstance(action, str) and redact_text(action):
            legacy_refused += 1
        elif isinstance(action, dict) and action.get("command"):
            legacy_refused += 1
    return actions, legacy_refused


def mutating_actions_require_human(argv_actions, safe_to_claim):
    mutating = [action for action in argv_actions if action["mutatesState"]]
    return bool(mutating) and (
        not safe_to_claim or any(action["reviewRequired"] for action in mutating)
    )


def selected_candidate(packet):
    recommended = dict_or_empty(packet.get("recommendedAction"))
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
        decision = lane.get("decision")
        return {
            "id": lane.get("beadId"),
            "decision": decision,
            "status": lane.get("status"),
            "unsafeReasons": []
            if decision == "safe_to_claim"
            else lane.get("decisionReasons", []),
            "staleReasons": [],
            "sourceRefs": [],
        }
    return None


def packet_safe_to_claim(packet, candidate, envelope_degraded=None):
    recommended = dict_or_empty(packet.get("recommendedAction"))
    raw_safe = packet.get("safeToClaim")
    if raw_safe is None:
        raw_safe = recommended.get("safeToClaim")
    decision = candidate.get("decision") if isinstance(candidate, dict) else None
    unsafe_reasons = (
        compact_list(candidate.get("unsafeReasons")) if isinstance(candidate, dict) else []
    )
    stale_reasons = (
        compact_list(candidate.get("staleReasons")) if isinstance(candidate, dict) else []
    )
    tracker = dict_or_empty(packet.get("trackerIntegrity"))
    coordination = dict_or_empty(packet.get("coordination"))
    agent_mail = dict_or_empty(coordination.get("agentMail"))
    tracker_authoritative = tracker.get("brReadsAuthoritative")
    return (
        raw_safe is True
        and decision == "safe_to_claim"
        and not unsafe_reasons
        and not stale_reasons
        and candidate_status_reason(candidate) is None
        and not compact_list(packet.get("doNotProceedBecause"))
        and not malformed_packet_map_reasons(packet)
        and not malformed_packet_scalar_reasons(packet)
        and not malformed_packet_command_action_reasons(packet)
        and not authority_degraded_reasons(
            packet, "packet_degraded_authority", envelope_degraded
        )
        and tracker_authoritative is True
        and not agent_mail_authority_reasons(agent_mail)
        and agent_mail.get("status") != "semantic_readiness_failed"
        and rch_remote_verification_reason(packet) is None
    )


def packet_action(packet, candidate, safe_to_claim):
    recommended = dict_or_empty(packet.get("recommendedAction"))
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


def packet_why_not_safe(packet, candidate, safe_to_claim, envelope_degraded=None):
    if safe_to_claim:
        return []

    reasons = []
    recommended = dict_or_empty(packet.get("recommendedAction"))
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
        status_reason = candidate_status_reason(candidate)
        if status_reason:
            reasons.append(status_reason)
        reasons.extend(compact_list(candidate.get("unsafeReasons")))
        reasons.extend(compact_list(candidate.get("staleReasons")))
        if decision == "stale_but_reclaimable":
            reasons.append("stale_but_reclaimable_requires_inspection")

    reasons.extend(malformed_packet_map_reasons(packet))
    reasons.extend(malformed_packet_scalar_reasons(packet))
    reasons.extend(malformed_packet_command_action_reasons(packet))
    reasons.extend(
        authority_degraded_reasons(
            packet, "packet_degraded_authority", envelope_degraded
        )
    )

    tracker = dict_or_empty(packet.get("trackerIntegrity"))
    tracker_reason = tracker_not_authoritative_reason(tracker)
    if tracker_reason:
        reasons.append(tracker_reason)
    if tracker.get("requiresCandidateDowngrade") is True:
        reasons.append("tracker_requires_candidate_downgrade")

    coordination = dict_or_empty(packet.get("coordination"))
    agent_mail = dict_or_empty(coordination.get("agentMail"))
    if agent_mail.get("status") == "semantic_readiness_failed":
        reasons.append("agent_mail_semantic_readiness_failed")
    reasons.extend(agent_mail_authority_reasons(agent_mail))
    if raw_safe is not True:
        reasons.append(f"packet_recommendation_not_claim_safe:{redact_text(action, 64)}")

    rch_reason = rch_remote_verification_reason(packet)
    if rch_reason:
        reasons.append(rch_reason)
    reasons.extend(compact_list(packet.get("doNotProceedBecause")))
    return compact_list(reasons)


def claim_gate_consistency_reasons(gate, envelope_degraded=None):
    reasons = []
    reasons.extend(malformed_claim_gate_reasons(gate))
    reasons.extend(
        authority_degraded_reasons(
            gate, "claim_gate_degraded_authority", envelope_degraded
        )
    )
    requested_candidate_id = gate.get("requestedCandidateId")
    if gate.get("safeToClaim") is not True:
        reasons.append("claim_gate_safe_flag_not_true")

    verdict = gate.get("verdict")
    if verdict != "safe_to_claim":
        reasons.append(f"claim_gate_verdict:{redact_text(verdict or 'unknown', 64)}")

    if gate.get("recommendedSafeToClaim") is not True:
        reasons.append("claim_gate_recommended_not_safe")
    if compact_list(gate.get("unsafeReasons")):
        reasons.append("claim_gate_unsafe_reasons_present")
    if compact_list(gate.get("staleReasons")):
        reasons.append("claim_gate_stale_reasons_present")

    for action in list_items(gate.get("nextCommandActions")):
        if not isinstance(action, dict):
            continue
        if action.get("mutatesState") is True or action_looks_like_beads_mutation(
            action
        ):
            command_id = redact_text(action.get("commandId") or "unknown", 64)
            reasons.append(f"claim_gate_next_action_mutates_state:{command_id}")

    candidate = gate.get("selectedCandidate")
    if not isinstance(candidate, dict):
        reasons.append("claim_gate_candidate_missing")
    else:
        candidate_id = candidate.get("id")
        if (
            isinstance(requested_candidate_id, str)
            and isinstance(candidate_id, str)
            and requested_candidate_id != candidate_id
        ):
            reasons.append(
                "claim_gate_candidate_id_mismatch:"
                f"{redact_text(requested_candidate_id, 64)}:"
                f"{redact_text(candidate_id, 64)}"
            )
        candidate_decision = candidate.get("decision")
        if candidate_decision != "safe_to_claim":
            reasons.append(
                "claim_gate_candidate_decision:"
                f"{redact_text(candidate_decision or 'unknown', 64)}"
            )

    if not isinstance(gate.get("claimCommandAction"), dict):
        reasons.append("claim_gate_missing_claim_action")
    else:
        claim_action = gate.get("claimCommandAction")
        if claim_action.get("mutatesState") is not True:
            reasons.append("claim_gate_claim_action_not_mutating")
        claim_candidate_id = claim_action_candidate_id(claim_action)
        if claim_candidate_id is None:
            reasons.append("claim_gate_claim_action_not_bead_update")
        elif not claim_action_sets_in_progress(claim_action):
            reasons.append("claim_gate_claim_action_not_in_progress")
        if not claim_action_emits_json(claim_action):
            reasons.append("claim_gate_claim_action_not_json")
        if not claim_action_is_safe_structured_argv(claim_action):
            reasons.append("claim_gate_claim_action_not_safe_structured_argv")
        expected_candidate_id = (
            candidate.get("id") if isinstance(candidate, dict) else None
        )
        if not isinstance(expected_candidate_id, str):
            expected_candidate_id = requested_candidate_id
        if (
            isinstance(expected_candidate_id, str)
            and isinstance(claim_candidate_id, str)
            and expected_candidate_id != claim_candidate_id
        ):
            reasons.append(
                "claim_gate_claim_action_candidate_mismatch:"
                f"{redact_text(expected_candidate_id, 64)}:"
                f"{redact_text(claim_candidate_id, 64)}"
            )

    authority = gate.get("sourceAuthority")
    if not isinstance(authority, dict):
        reasons.append("claim_gate_source_authority_missing")
        return compact_list(reasons)

    reasons.extend(malformed_claim_gate_authority_reasons(gate))

    if authority.get("trackerAuthoritative") is not True:
        tracker_health = redact_text(authority.get("trackerHealth") or "unknown", 64)
        reasons.append(f"claim_gate_tracker_not_authoritative:{tracker_health}")
    if authority.get("reservationAuthoritative") is not True:
        reasons.append("claim_gate_reservation_evidence_not_authoritative")
    if authority.get("inboxAuthoritative") is not True:
        reasons.append("claim_gate_inbox_evidence_not_authoritative")
    if authority.get("agentMailStatus") == "semantic_readiness_failed":
        reasons.append("agent_mail_semantic_readiness_failed")
    if authority.get("rchSafeToLaunchCargoVerification") is False:
        reasons.append("rch_remote_verification_blocked")
    if authority.get("installFreshnessAuthoritative") is False:
        freshness_verdict = redact_text(
            authority.get("installFreshnessVerdict") or "unknown",
            64,
        )
        reasons.append(f"claim_gate_install_freshness_not_authoritative:{freshness_verdict}")
    if "rchRemoteOnlyRequired" not in authority:
        reasons.append("claim_gate_rch_remote_only_required_missing")
    elif (
        authority.get("rchRemoteOnlyRequired") is True
        and authority.get("rchSafeToLaunchCargoVerification") is not True
    ):
        reasons.append("rch_remote_verification_required")

    actionable_queue = gate.get("actionableQueue")
    if isinstance(actionable_queue, dict):
        candidate_state = actionable_queue.get("candidateState")
        if gate.get("safeToClaim") is True and candidate_state not in (
            "candidate_present_actionable",
            "not_evaluated",
        ):
            reasons.append(
                "claim_gate_actionable_queue_candidate_state:"
                f"{redact_text(candidate_state or 'unknown', 64)}"
            )
        if actionable_queue.get("bvAdvisoryContradiction") is True:
            reasons.append("claim_gate_bv_advisory_contradiction")

    return compact_list(reasons)


def consume_claim_gate(gate, envelope_degraded=None):
    consistency_reasons = claim_gate_consistency_reasons(gate, envelope_degraded)
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
    safe_to_claim = packet_safe_to_claim(packet, candidate, envelope_degraded)
    recommended = dict_or_empty(packet.get("recommendedAction"))
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
        "whyNotSafe": packet_why_not_safe(
            packet, candidate, safe_to_claim, envelope_degraded
        ),
        "sourceSummary": source_summary_from_packet(packet),
        "degradedSummary": degraded_summary(packet, envelope_degraded),
        "legacyCommandStringsRefused": legacy_refused,
    }


def install_check_finding_codes(report, freshness):
    codes = []

    def add_code(value):
        text = redact_text(value, 96)
        if text and text not in codes:
            codes.append(text)

    for code in list_items(
        field_value(freshness, "blockingFindings", "blocking_findings")
    ):
        add_code(code)

    for finding in list_items(report.get("findings")):
        if isinstance(finding, dict):
            add_code(finding.get("code"))
        else:
            add_code(finding)

    return codes


def consume_install_check(report, envelope_degraded=None):
    freshness = dict_or_empty(report.get("freshness"))
    freshness_schema = freshness.get("schema")
    verdict = freshness.get("verdict") if isinstance(freshness.get("verdict"), str) else None
    if freshness_schema != INSTALL_FRESHNESS_SCHEMA:
        verdict = "missing_freshness"
    verdict = redact_text(verdict or "unknown_freshness", 64)

    why_not_safe = ["install_check_is_not_claim_gate"]
    if verdict != "fresh":
        why_not_safe.insert(0, f"install_freshness:{verdict}")

    for code in install_check_finding_codes(report, freshness):
        why_not_safe.append(f"install_finding:{code}")

    repair = field_value(freshness, "repair")
    if isinstance(repair, str) and repair.strip():
        why_not_safe.append(f"install_repair:{redact_text(repair, 120)}")

    return {
        "schema": OUTPUT_SCHEMA,
        "sourceSchema": INSTALL_CHECK_SCHEMA,
        "safeToClaim": False,
        "candidateId": None,
        "decision": f"install_freshness_{verdict}",
        "action": "run_claim_gate_after_fresh_install_check"
        if verdict == "fresh"
        else "repair_install_freshness",
        "argvActions": [],
        "mutatingActionsRequireHuman": False,
        "whyNotSafe": compact_list(why_not_safe),
        "sourceSummary": source_summary_for_install_check(),
        "degradedSummary": degraded_summary(report, envelope_degraded),
        "legacyCommandStringsRefused": 0,
    }


def consume(response):
    if not isinstance(response, dict):
        return error_decision("invalid_json_shape")
    if response.get("schema") == "ee.error.v2":
        return error_decision(classify_error_code(response.get("error", {})), response)
    if response.get("success") is False:
        error = response.get("error", {})
        return error_decision(classify_error_code(error), response)

    payload = get_payload(response)
    if not isinstance(payload, dict):
        return error_decision("missing_payload", response)

    envelope_degraded = response.get("degraded") if response is not payload else None
    schema = payload.get("schema")
    if schema == CLAIM_GATE_SCHEMA:
        return consume_claim_gate(payload, envelope_degraded)
    if schema == WORK_PACKET_SCHEMA:
        return consume_work_packet(payload, envelope_degraded)
    if schema == INSTALL_CHECK_SCHEMA:
        return consume_install_check(payload, envelope_degraded)
    return error_decision(
        f"unsupported_schema:{redact_text(schema or 'unknown', 64)}",
        payload,
        envelope_degraded,
    )


def classify_error_code(error):
    if not isinstance(error, dict):
        return "unknown_error"

    if stale_claim_gate_binary_error(error):
        return "stale_claim_gate_binary"
    if stale_environment_attestation_binary_error(error):
        return "stale_environment_attestation_binary"

    code = error.get("code")
    return code or "unknown_error"


def stale_claim_gate_binary_error(error):
    details = dict_or_empty(error.get("details"))
    invocation = details.get("invocation")
    if invocation is not None and not invocation_is_swarm_work_packet(invocation):
        return False
    for message in [error.get("message"), details.get("message")]:
        if (
            isinstance(message, str)
            and STALE_CLAIM_GATE_UNEXPECTED_ARG_PATTERN.search(message)
        ):
            return True
    return False


def stale_environment_attestation_binary_error(error):
    details = dict_or_empty(error.get("details"))
    invocation = details.get("invocation")
    if invocation is not None and not invocation_is_diag_environment_attestation(
        invocation
    ):
        return False
    for message in [error.get("message"), details.get("message")]:
        if (
            isinstance(message, str)
            and STALE_ENVIRONMENT_ATTESTATION_UNRECOGNIZED_PATTERN.search(message)
        ):
            return True
    return False


def invocation_is_swarm_work_packet(invocation):
    command_parts = ee_command_parts_from_invocation(invocation)
    return command_parts[:2] == ["swarm", "work-packet"]


def invocation_is_diag_environment_attestation(invocation):
    command_parts = ee_command_parts_from_invocation(invocation)
    return command_parts[:2] == ["diag", "environment-attestation"]


def ee_command_parts_from_invocation(invocation):
    if isinstance(invocation, str):
        try:
            parts = shlex.split(invocation)
        except ValueError:
            return []
    elif isinstance(invocation, list):
        parts = [part for part in invocation if isinstance(part, str) and part]
    else:
        return []
    if parts and parts[0].endswith("/ee"):
        parts[0] = "ee"
    if not parts or parts[0] != "ee":
        return []
    return first_ee_command_parts(parts[1:])


EE_GLOBAL_FLAGS_WITH_VALUE = {
    "--config",
    "--database",
    "--fields",
    "--format",
    "--profile",
    "--workspace",
}

EE_GLOBAL_FLAGS_WITHOUT_VALUE = {
    "--help",
    "--json",
    "--markdown",
    "--no-color",
    "--quiet",
    "--version",
}


def first_ee_command_parts(parts):
    command_parts = []
    skip_next = False
    for part in parts:
        if skip_next:
            skip_next = False
            continue
        if not command_parts:
            if part in EE_GLOBAL_FLAGS_WITH_VALUE:
                skip_next = True
                continue
            if any(
                part.startswith(f"{flag}=") for flag in EE_GLOBAL_FLAGS_WITH_VALUE
            ):
                continue
            if part in EE_GLOBAL_FLAGS_WITHOUT_VALUE:
                continue
        command_parts.append(part)
    return command_parts


def error_decision(code, source=None, envelope_degraded=None):
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
            "rchRemoteOnlyRequired": None,
            "rchSafeToLaunchCargoVerification": None,
            "sourceCount": 0,
        },
        "degradedSummary": degraded_summary(source, envelope_degraded),
        "legacyCommandStringsRefused": 0,
    }


def load_response():
    text = sys.stdin.read()
    try:
        return json.loads(text)
    except json.JSONDecodeError:
        candidates = []
        for line in text.splitlines():
            line = line.strip()
            if not line:
                continue
            try:
                value = json.loads(line)
            except json.JSONDecodeError:
                continue
            if (
                isinstance(value, dict)
                and value.get("schema") in MACHINE_RESPONSE_SCHEMAS
            ):
                candidates.append(value)
        if candidates:
            return candidates[-1]
        return {
            "schema": "ee.error.v2",
            "success": False,
            "error": {"code": "invalid_json"},
        }


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
