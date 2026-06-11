#!/usr/bin/env python3
"""Self-test for `agent_consume_work_packet_gate.py`.

Run:
    python3 scripts/agent_consume_work_packet_gate_test.py
"""

import importlib.util
import json
import subprocess
import sys
import unittest
from pathlib import Path

SCRIPT_DIR = Path(__file__).resolve().parent
REPO_ROOT = SCRIPT_DIR.parent

_SPEC = importlib.util.spec_from_file_location(
    "agent_consume_work_packet_gate",
    SCRIPT_DIR / "agent_consume_work_packet_gate.py",
)
assert _SPEC is not None and _SPEC.loader is not None
consumer = importlib.util.module_from_spec(_SPEC)
_SPEC.loader.exec_module(consumer)


def envelope(data, degraded=None):
    return {
        "schema": "ee.response.v2",
        "success": True,
        "data": data,
        "degraded": degraded or [],
    }


def safe_action(command_id, argv, mutates=False, copy_safety="safe_structured_argv", shell=False):
    return {
        "commandId": command_id,
        "displayCommand": " ".join(str(part) for part in argv),
        "argv": argv,
        "shellRequired": shell,
        "copySafety": copy_safety,
        "mutatesState": mutates,
        "requiredSubstrate": "beads" if mutates else "static_local",
        "when": "after_gate",
        "rationale": "fixture action",
    }


def safe_actionable_queue():
    return {
        "commandId": "beads_actionable_queue",
        "displayCommand": "scripts/br_retry.sh actionable --json",
        "mutatesState": False,
        "collectionMode": "br_retry_script",
        "queueState": "ready",
        "exitClass": "ok",
        "authoritative": True,
        "rowCount": 1,
        "candidateIds": ["bd-safe.1"],
        "truncatedCandidateCount": 0,
        "filterContract": {
            "excludesEpics": True,
            "excludesAssigned": True,
            "excludesBlocked": True,
            "excludesDeferred": True,
            "excludesInProgress": True,
        },
        "exclusionAccounting": {
            "rawReadyCount": 1,
            "excludedEpicCount": 0,
            "excludedAssignedCount": 0,
            "excludedBlockedCount": 0,
            "excludedDeferredCount": 0,
            "excludedInProgressCount": 0,
            "excludedOtherCount": 0,
        },
        "candidateState": "candidate_present_actionable",
        "bvAdvisoryContradiction": False,
        "trackerAuthorityDegraded": False,
        "contradictionEvidence": [],
    }


def safe_gate():
    return {
        "schema": "ee.swarm.work_packet.claim_gate.v1",
        "gateId": "swarm_work_packet_claim_gate_111111111111111111111111",
        "packetId": "swarm_work_packet_222222222222222222222222",
        "workspace": "repo:25e38e130474e7f0292de2a3",
        "redactionStatus": "counts_ids_statuses_path_patterns_command_templates_no_mail_body_no_file_content",
        "requestedCandidateId": "bd-safe.1",
        "verdict": "safe_to_claim",
        "safeToClaim": True,
        "selectedCandidate": {
            "id": "bd-safe.1",
            "title": "Document a small schema improvement",
            "source": "beads_ready",
            "status": "open",
            "priority": 2,
            "assignee": None,
            "decision": "safe_to_claim",
            "collisionRisk": "none",
        },
        "recommendedAction": "inspect_and_claim",
        "recommendedSafeToClaim": True,
        "sourceAuthority": {
            "trackerAuthoritative": True,
            "trackerHealth": "ok",
            "agentMailStatus": "healthy",
            "reservationAuthoritative": True,
            "inboxAuthoritative": True,
            "rchRemoteOnlyRequired": True,
            "rchSafeToLaunchCargoVerification": True,
            "environmentVerdict": "remote_verification_admitted",
            "sourceTestVerdict": "not_evaluated",
            "remoteVerificationAdmitted": True,
            "localCargoFallbackObserved": None,
            "installFreshnessVerdict": "not_evaluated",
            "installFreshnessAuthoritative": None,
            "installFreshnessRepair": None,
            "sourceCount": 4,
        },
        "actionableQueue": safe_actionable_queue(),
        "unsafeReasons": [],
        "staleReasons": [],
        "sourceRefs": ["br://bd-safe.1"],
        "degradedCodes": [],
        "nextCommandActions": [
            safe_action("bead_show_candidate", ["br", "show", "bd-safe.1", "--json"])
        ],
        "claimCommandAction": safe_action(
            "bead_claim_candidate",
            ["br", "update", "bd-safe.1", "--status", "in_progress", "--json"],
            mutates=True,
        ),
        "recoveryActions": [],
    }


def install_check_report(verdict="stale", blocking_findings=None, findings=None):
    return {
        "schema": "ee.install.check.v1",
        "version": "0.5.0",
        "freshness": {
            "schema": "ee.install.freshness.v1",
            "verdict": verdict,
            "authoritative": verdict == "fresh",
            "comparison": "installed_older_than_source"
            if verdict == "stale"
            else "equal",
            "blocking_findings": blocking_findings
            if blocking_findings is not None
            else ["installed_binary_stale"],
            "repair": "Adopt a current release artifact or request an operator exception.",
        },
        "findings": findings
        if findings is not None
        else [
            {"code": "duplicate_path_binary", "severity": "warning"},
            {"code": "path_binary_version_mismatch", "severity": "warning"},
        ],
    }


def load_fixture(relative_path):
    with (REPO_ROOT / relative_path).open(encoding="utf-8") as fh:
        return json.load(fh)


def load_text(relative_path):
    return (REPO_ROOT / relative_path).read_text(encoding="utf-8")


def run_consumer_cli(payload, extra_args=None):
    result = subprocess.run(
        [
            sys.executable,
            str(SCRIPT_DIR / "agent_consume_work_packet_gate.py"),
            *(extra_args or []),
        ],
        input=json.dumps(payload),
        text=True,
        capture_output=True,
        check=False,
    )
    return result, json.loads(result.stdout)


def normalize_whitespace(text):
    return " ".join(text.split())


SCHEMA_FORBIDDEN_SAFE_STRING_EXAMPLES = {
    "BEGIN PRIVATE KEY": "BEGIN PRIVATE KEY",
    "BEGIN OPENSSH PRIVATE KEY": "BEGIN OPENSSH PRIVATE KEY",
    "ghp_[A-Za-z0-9_]+": "ghp_a",
    "Bearer [A-Za-z0-9._-]+": "Bearer x",
    "DATABASE_URL=": "DATABASE_URL=",
    "From: ": "From: ",
    "Subject: ": "Subject: ",
    "Message-ID:": "Message-ID:",
    "body:": "body:",
    "raw_inbox": "raw_inbox",
    "stdout:": "stdout:",
    "stderr:": "stderr:",
    "/Users/[^\\s]+": "/Users/a",
    "/home/[^\\s]+": "/home/a",
}


def safe_string_forbidden_patterns(schema, definition_name):
    safe_string = schema["definitions"][definition_name]
    return [entry["pattern"] for entry in safe_string["not"]["anyOf"]]


class ClaimGateConsumer(unittest.TestCase):
    def test_safe_claim_gate_returns_structured_argv_actions(self):
        decision = consumer.consume(envelope(safe_gate()))

        self.assertTrue(decision["safeToClaim"])
        self.assertEqual(decision["candidateId"], "bd-safe.1")
        self.assertEqual(decision["decision"], "safe_to_claim")
        self.assertEqual(decision["action"], "inspect_and_claim")
        self.assertFalse(decision["mutatingActionsRequireHuman"])
        self.assertEqual(decision["whyNotSafe"], [])
        self.assertEqual(len(decision["argvActions"]), 2)
        claim = [a for a in decision["argvActions"] if a["actionKind"] == "claim"][0]
        self.assertTrue(claim["runnable"])
        self.assertFalse(claim["reviewRequired"])
        self.assertEqual(claim["argv"][:3], ["br", "update", "bd-safe.1"])

    def test_many_inspection_actions_are_bounded_but_claim_is_preserved(self):
        gate = safe_gate()
        gate["nextCommandActions"] = [
            safe_action(
                f"inspect_{index}",
                ["br", "show", f"bd-extra.{index}", "--json"],
            )
            for index in range(32)
        ]

        decision = consumer.consume(envelope(gate))

        self.assertTrue(decision["safeToClaim"])
        self.assertEqual(
            len(decision["argvActions"]),
            consumer.DECISION_ACTION_LIMIT,
        )
        self.assertEqual(decision["argvActions"][0]["commandId"], "inspect_0")
        self.assertEqual(
            decision["argvActions"][-2]["commandId"],
            f"inspect_{consumer.DECISION_ACTION_LIMIT - 2}",
        )
        self.assertEqual(decision["argvActions"][-1]["actionKind"], "claim")
        self.assertEqual(
            decision["argvActions"][-1]["commandId"],
            "bead_claim_candidate",
        )

    def test_oversized_claim_action_argv_fails_gate_closed(self):
        gate = safe_gate()
        gate["claimCommandAction"]["argv"] = [
            "br",
            "update",
            "bd-safe.1",
            "--status",
            "in_progress",
            "--json",
            *[
                f"--extra-{index}"
                for index in range(consumer.DECISION_ARGV_PART_LIMIT)
            ],
        ]

        decision = consumer.consume(envelope(gate))

        self.assertFalse(decision["safeToClaim"])
        self.assertIn(
            "claim_gate_claim_action_not_safe_structured_argv",
            decision["whyNotSafe"],
        )
        claim = [a for a in decision["argvActions"] if a["actionKind"] == "claim"][0]
        self.assertFalse(claim["runnable"])
        self.assertEqual(claim["argv"], [])
        self.assertEqual(claim["reason"], "argv_too_long")

    def test_recommended_safe_mismatch_fails_closed(self):
        gate = safe_gate()
        gate["recommendedSafeToClaim"] = False

        decision = consumer.consume(envelope(gate))

        self.assertFalse(decision["safeToClaim"])
        self.assertTrue(decision["mutatingActionsRequireHuman"])
        self.assertIn("claim_gate_recommended_not_safe", decision["whyNotSafe"])
        claim = [a for a in decision["argvActions"] if a["actionKind"] == "claim"][0]
        self.assertFalse(claim["runnable"])
        self.assertTrue(claim["reviewRequired"])
        self.assertEqual(claim["reason"], "mutating_action_requires_safe_gate")

    def test_source_authority_mismatch_fails_closed(self):
        gate = safe_gate()
        gate["sourceAuthority"]["trackerAuthoritative"] = False
        gate["sourceAuthority"]["trackerHealth"] = "db_jsonl_count_mismatch"

        decision = consumer.consume(envelope(gate))

        self.assertFalse(decision["safeToClaim"])
        self.assertIn(
            "claim_gate_tracker_not_authoritative:db_jsonl_count_mismatch",
            decision["whyNotSafe"],
        )
        claim = [a for a in decision["argvActions"] if a["actionKind"] == "claim"][0]
        self.assertFalse(claim["runnable"])

    def test_metadata_only_pending_import_health_does_not_override_authority(self):
        gate = safe_gate()
        gate["sourceAuthority"]["trackerHealth"] = "external_changes_pending_import"
        gate["sourceAuthority"]["trackerAuthoritative"] = True

        decision = consumer.consume(envelope(gate))

        self.assertTrue(decision["safeToClaim"])
        self.assertEqual(decision["sourceSummary"]["trackerAuthoritative"], True)
        self.assertEqual(
            decision["sourceSummary"]["trackerHealth"],
            "external_changes_pending_import",
        )
        self.assertNotIn(
            "claim_gate_tracker_not_authoritative:external_changes_pending_import",
            decision["whyNotSafe"],
        )
        self.assertEqual(decision["whyNotSafe"], [])
        claim = [a for a in decision["argvActions"] if a["actionKind"] == "claim"][0]
        self.assertTrue(claim["runnable"])

    def test_source_authority_agent_mail_not_authoritative_fails_closed(self):
        gate = safe_gate()
        gate["sourceAuthority"]["reservationAuthoritative"] = False
        gate["sourceAuthority"]["inboxAuthoritative"] = None

        decision = consumer.consume(envelope(gate))

        self.assertFalse(decision["safeToClaim"])
        self.assertEqual(decision["sourceSummary"]["reservationAuthoritative"], False)
        self.assertIsNone(decision["sourceSummary"]["inboxAuthoritative"])
        self.assertIn(
            "claim_gate_reservation_evidence_not_authoritative",
            decision["whyNotSafe"],
        )
        self.assertIn(
            "claim_gate_inbox_evidence_not_authoritative",
            decision["whyNotSafe"],
        )
        claim = [a for a in decision["argvActions"] if a["actionKind"] == "claim"][0]
        self.assertFalse(claim["runnable"])

    def test_claim_gate_install_freshness_not_authoritative_fails_closed(self):
        gate = safe_gate()
        gate["sourceAuthority"]["installFreshnessVerdict"] = "stale"
        gate["sourceAuthority"]["installFreshnessAuthoritative"] = False
        gate["sourceAuthority"]["installFreshnessRepair"] = (
            "Run ee install check --json --offline before claiming."
        )

        decision = consumer.consume(envelope(gate))

        self.assertFalse(decision["safeToClaim"])
        self.assertIn(
            "claim_gate_install_freshness_not_authoritative:stale",
            decision["whyNotSafe"],
        )
        claim = [a for a in decision["argvActions"] if a["actionKind"] == "claim"][0]
        self.assertFalse(claim["runnable"])

    def test_missing_required_source_authority_fields_fail_closed(self):
        for field, reason in consumer.CLAIM_GATE_SOURCE_AUTHORITY_REQUIRED_FIELDS:
            with self.subTest(field=field):
                gate = safe_gate()
                gate["sourceAuthority"].pop(field)

                decision = consumer.consume(envelope(gate))

                self.assertFalse(decision["safeToClaim"])
                self.assertIn(reason, decision["whyNotSafe"])
                claim = [
                    action
                    for action in decision["argvActions"]
                    if action["actionKind"] == "claim"
                ][0]
                self.assertFalse(claim["runnable"])

    def test_malformed_source_authority_scalars_fail_closed_and_emit_schema_types(self):
        gate = safe_gate()
        gate["sourceAuthority"].update(
            {
                "trackerAuthoritative": "true",
                "trackerHealth": ["ok"],
                "agentMailStatus": {"status": "healthy"},
                "reservationAuthoritative": ["true"],
                "inboxAuthoritative": {"value": True},
                "rchRemoteOnlyRequired": "true",
                "rchSafeToLaunchCargoVerification": 1,
                "environmentVerdict": ["remote_verification_admitted"],
                "sourceTestVerdict": {"verdict": "not_evaluated"},
                "remoteVerificationAdmitted": "true",
                "localCargoFallbackObserved": "false",
                "installFreshnessVerdict": ["not_evaluated"],
                "installFreshnessAuthoritative": "false",
                "installFreshnessRepair": ["Run install check."],
                "sourceCount": -1,
            }
        )

        decision = consumer.consume(envelope(gate))

        self.assertFalse(decision["safeToClaim"])
        self.assertIsNone(decision["sourceSummary"]["trackerAuthoritative"])
        self.assertIsNone(decision["sourceSummary"]["reservationAuthoritative"])
        self.assertIsNone(decision["sourceSummary"]["inboxAuthoritative"])
        self.assertIsNone(decision["sourceSummary"]["rchRemoteOnlyRequired"])
        self.assertIsNone(
            decision["sourceSummary"]["rchSafeToLaunchCargoVerification"]
        )
        self.assertIsNone(decision["sourceSummary"]["sourceCount"])
        for reason in [
            "malformed_claim_gate_tracker_authoritative",
            "malformed_claim_gate_tracker_health",
            "malformed_claim_gate_agent_mail_status",
            "malformed_claim_gate_reservation_authoritative",
            "malformed_claim_gate_inbox_authoritative",
            "malformed_claim_gate_rch_remote_only_required",
            "malformed_claim_gate_rch_safe_to_launch_cargo_verification",
            "malformed_claim_gate_environment_verdict",
            "malformed_claim_gate_source_test_verdict",
            "malformed_claim_gate_remote_verification_admitted",
            "malformed_claim_gate_local_cargo_fallback_observed",
            "malformed_claim_gate_install_freshness_verdict",
            "malformed_claim_gate_install_freshness_authoritative",
            "malformed_claim_gate_install_freshness_repair",
            "malformed_claim_gate_source_count",
        ]:
            self.assertIn(reason, decision["whyNotSafe"])
        claim = [a for a in decision["argvActions"] if a["actionKind"] == "claim"][0]
        self.assertFalse(claim["runnable"])

    def test_malformed_claim_gate_scalars_and_arrays_fail_closed(self):
        gate = safe_gate()
        gate.update(
            {
                "safeToClaim": "true",
                "recommendedSafeToClaim": 1,
                "requestedCandidateId": ["bd-safe.1"],
                "verdict": ["safe_to_claim"],
                "recommendedAction": {"action": "inspect_and_claim"},
                "unsafeReasons": "peer_dirty_file",
                "staleReasons": {"reason": "stale_assignee"},
                "sourceRefs": "br://bd-safe.1",
                "degradedCodes": {"code": "tracker_mismatch"},
                "nextCommandActions": {"commandId": "bead_show_candidate"},
                "recoveryActions": {"kind": "verify_source_version"},
            }
        )
        gate["selectedCandidate"]["id"] = ["bd-safe.1"]
        gate["selectedCandidate"]["decision"] = 1

        decision = consumer.consume(envelope(gate))

        self.assertFalse(decision["safeToClaim"])
        self.assertTrue(decision["mutatingActionsRequireHuman"])
        for reason in [
            "malformed_claim_gate_safe_to_claim",
            "malformed_claim_gate_recommended_safe_to_claim",
            "malformed_claim_gate_requested_candidate_id",
            "malformed_claim_gate_verdict",
            "malformed_claim_gate_recommended_action",
            "malformed_claim_gate_unsafe_reasons",
            "malformed_claim_gate_stale_reasons",
            "malformed_claim_gate_source_refs",
            "malformed_claim_gate_degraded_codes",
            "malformed_claim_gate_next_command_actions",
            "malformed_claim_gate_recovery_actions",
            "malformed_claim_gate_candidate_id",
            "malformed_claim_gate_candidate_decision",
        ]:
            self.assertIn(reason, decision["whyNotSafe"])
        self.assertIn("claim_gate_safe_flag_not_true", decision["whyNotSafe"])
        self.assertIn("claim_gate_recommended_not_safe", decision["whyNotSafe"])
        claim = [a for a in decision["argvActions"] if a["actionKind"] == "claim"][0]
        self.assertFalse(claim["runnable"])
        self.assertEqual(claim["reason"], "mutating_action_requires_safe_gate")

    def test_missing_required_claim_gate_fields_fail_closed(self):
        for field, reason in consumer.CLAIM_GATE_REQUIRED_FIELDS:
            with self.subTest(field=field):
                gate = safe_gate()
                gate.pop(field)

                decision = consumer.consume(envelope(gate))

                self.assertFalse(decision["safeToClaim"])
                self.assertIn(reason, decision["whyNotSafe"])
                self.assertFalse(
                    any(
                        action["runnable"] and action["mutatesState"]
                        for action in decision["argvActions"]
                    )
                )

    def test_malformed_next_command_action_entry_fails_closed(self):
        gate = safe_gate()
        gate["nextCommandActions"].append("br show bd-safe.1 --json")

        decision = consumer.consume(envelope(gate))

        self.assertFalse(decision["safeToClaim"])
        self.assertTrue(decision["mutatingActionsRequireHuman"])
        self.assertIn(
            "malformed_claim_gate_next_command_action",
            decision["whyNotSafe"],
        )
        claim = [a for a in decision["argvActions"] if a["actionKind"] == "claim"][0]
        self.assertFalse(claim["runnable"])
        self.assertEqual(claim["reason"], "mutating_action_requires_safe_gate")

    def test_malformed_actionable_queue_shape_fails_closed(self):
        gate = safe_gate()
        gate["actionableQueue"] = ["bd-safe.1"]

        decision = consumer.consume(envelope(gate))

        self.assertFalse(decision["safeToClaim"])
        self.assertIn("malformed_claim_gate_actionable_queue", decision["whyNotSafe"])
        self.assertFalse(
            any(
                action["runnable"] and action["mutatesState"]
                for action in decision["argvActions"]
            )
        )

    def test_missing_required_actionable_queue_fields_fail_closed(self):
        for field, suffix in consumer.CLAIM_GATE_ACTIONABLE_QUEUE_REQUIRED_FIELDS:
            with self.subTest(field=field):
                gate = safe_gate()
                gate["actionableQueue"].pop(field)

                decision = consumer.consume(envelope(gate))

                self.assertFalse(decision["safeToClaim"])
                self.assertIn(
                    f"missing_claim_gate_actionable_queue_{suffix}",
                    decision["whyNotSafe"],
                )
                self.assertFalse(
                    any(
                        action["runnable"] and action["mutatesState"]
                        for action in decision["argvActions"]
                    )
                )

    def test_null_required_actionable_queue_fields_fail_closed_except_row_count(self):
        for field, suffix in consumer.CLAIM_GATE_ACTIONABLE_QUEUE_REQUIRED_FIELDS:
            if field == "rowCount":
                continue
            with self.subTest(field=field):
                gate = safe_gate()
                gate["actionableQueue"][field] = None

                decision = consumer.consume(envelope(gate))

                self.assertFalse(decision["safeToClaim"])
                self.assertIn(
                    f"malformed_claim_gate_actionable_queue_{suffix}",
                    decision["whyNotSafe"],
                )
                self.assertFalse(
                    any(
                        action["runnable"] and action["mutatesState"]
                        for action in decision["argvActions"]
                    )
                )

    def test_unsafe_actionable_queue_state_fails_closed(self):
        gate = safe_gate()
        gate["actionableQueue"]["candidateState"] = "candidate_absent_from_actionable"
        gate["actionableQueue"]["bvAdvisoryContradiction"] = True

        decision = consumer.consume(envelope(gate))

        self.assertFalse(decision["safeToClaim"])
        self.assertIn(
            "claim_gate_actionable_queue_candidate_state:candidate_absent_from_actionable",
            decision["whyNotSafe"],
        )
        self.assertIn("claim_gate_bv_advisory_contradiction", decision["whyNotSafe"])
        claim = [a for a in decision["argvActions"] if a["actionKind"] == "claim"][0]
        self.assertFalse(claim["runnable"])
        self.assertEqual(claim["reason"], "mutating_action_requires_safe_gate")

    def test_missing_required_next_command_action_fields_fail_closed(self):
        for field, suffix in consumer.CLAIM_GATE_COMMAND_ACTION_REQUIRED_FIELDS:
            with self.subTest(field=field):
                gate = safe_gate()
                gate["nextCommandActions"][0].pop(field)

                decision = consumer.consume(envelope(gate))

                self.assertFalse(decision["safeToClaim"])
                self.assertIn(
                    f"missing_claim_gate_next_command_action_{suffix}",
                    decision["whyNotSafe"],
                )
                claim = [
                    action
                    for action in decision["argvActions"]
                    if action["actionKind"] == "claim"
                ][0]
                self.assertFalse(claim["runnable"])

    def test_missing_required_claim_command_action_fields_fail_closed(self):
        for field, suffix in consumer.CLAIM_GATE_COMMAND_ACTION_REQUIRED_FIELDS:
            with self.subTest(field=field):
                gate = safe_gate()
                gate["claimCommandAction"].pop(field)

                decision = consumer.consume(envelope(gate))

                self.assertFalse(decision["safeToClaim"])
                self.assertIn(
                    f"missing_claim_gate_claim_command_action_{suffix}",
                    decision["whyNotSafe"],
                )
                self.assertFalse(
                    any(
                        action["runnable"] and action["mutatesState"]
                        for action in decision["argvActions"]
                    )
                )

    def test_null_required_command_action_fields_fail_closed(self):
        for field, suffix in consumer.CLAIM_GATE_COMMAND_ACTION_REQUIRED_FIELDS:
            with self.subTest(field=field):
                gate = safe_gate()
                gate["nextCommandActions"][0][field] = None

                decision = consumer.consume(envelope(gate))

                self.assertFalse(decision["safeToClaim"])
                self.assertIn(
                    f"malformed_claim_gate_next_command_action_{suffix}",
                    decision["whyNotSafe"],
                )
                action = [
                    action
                    for action in decision["argvActions"]
                    if action["actionKind"] == "inspection"
                ][0]
                self.assertFalse(action["runnable"])

    def test_extra_command_action_fields_fail_closed(self):
        gate = safe_gate()
        gate["nextCommandActions"][0]["shellCommand"] = "br show bd-safe.1 --json"

        decision = consumer.consume(envelope(gate))

        self.assertFalse(decision["safeToClaim"])
        self.assertIn(
            "malformed_claim_gate_next_command_action_unexpected_field",
            decision["whyNotSafe"],
        )
        action = [
            action
            for action in decision["argvActions"]
            if action["actionKind"] == "inspection"
        ][0]
        self.assertFalse(action["runnable"])
        self.assertEqual(action["reason"], "malformed_command_action")

    def test_malformed_command_action_fields_fail_closed(self):
        gate = safe_gate()
        gate["nextCommandActions"][0].update(
            {
                "commandId": ["bead_show_candidate"],
                "displayCommand": {"command": "br show bd-safe.1 --json"},
                "shellRequired": "false",
                "copySafety": ["safe_structured_argv"],
                "requiredSubstrate": 1,
                "when": ["after_gate"],
                "rationale": {"why": "fixture action"},
            }
        )
        gate["claimCommandAction"]["argv"] = "br update bd-safe.1 --status in_progress --json"
        gate["claimCommandAction"]["mutatesState"] = "true"

        decision = consumer.consume(envelope(gate))

        self.assertFalse(decision["safeToClaim"])
        for reason in [
            "malformed_claim_gate_next_command_action_command_id",
            "malformed_claim_gate_next_command_action_display_command",
            "malformed_claim_gate_next_command_action_shell_required",
            "malformed_claim_gate_next_command_action_copy_safety",
            "malformed_claim_gate_next_command_action_required_substrate",
            "malformed_claim_gate_next_command_action_when",
            "malformed_claim_gate_next_command_action_rationale",
            "malformed_claim_gate_claim_command_action_argv",
            "malformed_claim_gate_claim_command_action_mutates_state",
        ]:
            self.assertIn(reason, decision["whyNotSafe"])
        self.assertFalse(
            any(
                action["runnable"] and action["mutatesState"]
                for action in decision["argvActions"]
            )
        )

    def test_command_action_schema_enum_and_cross_field_violations_fail_closed(self):
        gate = safe_gate()
        gate["nextCommandActions"][0]["copySafety"] = "safe_structured_argv"
        gate["nextCommandActions"][0]["shellRequired"] = True
        gate["claimCommandAction"]["requiredSubstrate"] = "shell"

        decision = consumer.consume(envelope(gate))

        self.assertFalse(decision["safeToClaim"])
        self.assertIn(
            "malformed_claim_gate_next_command_action_copy_safety",
            decision["whyNotSafe"],
        )
        self.assertIn(
            "malformed_claim_gate_next_command_action_shell_required",
            decision["whyNotSafe"],
        )
        self.assertIn(
            "malformed_claim_gate_claim_command_action_required_substrate",
            decision["whyNotSafe"],
        )
        claim = [a for a in decision["argvActions"] if a["actionKind"] == "claim"][0]
        self.assertFalse(claim["runnable"])
        self.assertEqual(claim["reason"], "malformed_command_action")

    def test_safe_command_string_redaction_slots_fail_closed(self):
        gate = safe_gate()
        token = "ghp_" + "0123456789abcdef0123456789abcdef0123"
        gate["claimCommandAction"]["displayCommand"] = (
            "br update bd-safe.1 --status in_progress /Users/jemanuel/private"
        )
        gate["claimCommandAction"]["argv"].append(f"TOKEN={token}")
        gate["claimCommandAction"]["when"] = f"after Bearer {token}"
        gate["claimCommandAction"]["rationale"] = "From: mailbox header"

        decision = consumer.consume(envelope(gate))

        self.assertFalse(decision["safeToClaim"])
        for reason in [
            "malformed_claim_gate_claim_command_action_display_command",
            "malformed_claim_gate_claim_command_action_argv",
            "malformed_claim_gate_claim_command_action_when",
            "malformed_claim_gate_claim_command_action_rationale",
            "claim_gate_claim_action_not_safe_structured_argv",
        ]:
            self.assertIn(reason, decision["whyNotSafe"])
        claim = [a for a in decision["argvActions"] if a["actionKind"] == "claim"][0]
        self.assertFalse(claim["runnable"])
        self.assertEqual(claim["reason"], "malformed_command_action")

    def test_safe_command_string_body_marker_fails_closed(self):
        gate = safe_gate()
        gate["claimCommandAction"]["when"] = "body: raw mailbox content"

        decision = consumer.consume(envelope(gate))

        self.assertFalse(decision["safeToClaim"])
        self.assertIn(
            "malformed_claim_gate_claim_command_action_when",
            decision["whyNotSafe"],
        )
        claim = [a for a in decision["argvActions"] if a["actionKind"] == "claim"][0]
        self.assertFalse(claim["runnable"])
        self.assertEqual(claim["reason"], "malformed_command_action")

    def test_schema_forbidden_safe_string_markers_fail_closed_in_all_claim_slots(self):
        claim_schema = load_fixture(
            "docs/schemas/swarm/ee.swarm.work_packet.claim_gate.v1.json"
        )
        work_packet_schema = load_fixture(
            "docs/schemas/swarm/ee.swarm.work_packet.v1.json"
        )
        claim_patterns = safe_string_forbidden_patterns(claim_schema, "safeString")
        work_packet_patterns = safe_string_forbidden_patterns(
            work_packet_schema,
            "safeCommandString",
        )

        self.assertEqual(claim_patterns, work_packet_patterns)
        self.assertEqual(
            set(SCHEMA_FORBIDDEN_SAFE_STRING_EXAMPLES),
            set(claim_patterns),
        )

        claim_slots = [
            (
                "displayCommand",
                "malformed_claim_gate_claim_command_action_display_command",
                "malformed_command_action",
                lambda action, marker: action.__setitem__(
                    "displayCommand",
                    f"br update bd-safe.1 --status in_progress --json {marker}",
                ),
            ),
            (
                "argv",
                "malformed_claim_gate_claim_command_action_argv",
                "argv_redacted",
                lambda action, marker: action["argv"].append(marker),
            ),
            (
                "when",
                "malformed_claim_gate_claim_command_action_when",
                "malformed_command_action",
                lambda action, marker: action.__setitem__("when", marker),
            ),
            (
                "rationale",
                "malformed_claim_gate_claim_command_action_rationale",
                "malformed_command_action",
                lambda action, marker: action.__setitem__("rationale", marker),
            ),
        ]

        for pattern in claim_patterns:
            marker = SCHEMA_FORBIDDEN_SAFE_STRING_EXAMPLES[pattern]
            for slot, reason, action_reason, mutate_action in claim_slots:
                with self.subTest(pattern=pattern, slot=slot):
                    gate = safe_gate()
                    mutate_action(gate["claimCommandAction"], marker)

                    decision = consumer.consume(envelope(gate))

                    self.assertFalse(decision["safeToClaim"])
                    self.assertIn(reason, decision["whyNotSafe"])
                    claim = [
                        action
                        for action in decision["argvActions"]
                        if action["actionKind"] == "claim"
                    ][0]
                    self.assertFalse(claim["runnable"])
                    self.assertTrue(claim["reviewRequired"])
                    self.assertEqual(claim["reason"], action_reason)

    def test_display_only_claim_action_fails_closed_even_when_schema_valid(self):
        gate = safe_gate()
        gate["claimCommandAction"]["copySafety"] = "display_only"

        decision = consumer.consume(envelope(gate))

        self.assertFalse(decision["safeToClaim"])
        self.assertIn(
            "claim_gate_claim_action_not_safe_structured_argv",
            decision["whyNotSafe"],
        )
        claim = [a for a in decision["argvActions"] if a["actionKind"] == "claim"][0]
        self.assertFalse(claim["runnable"])
        self.assertEqual(claim["reason"], "copy_safety:display_only")

    def test_claim_gate_remote_required_without_positive_proof_fails_closed(self):
        gate = safe_gate()
        gate["sourceAuthority"]["rchSafeToLaunchCargoVerification"] = None

        decision = consumer.consume(envelope(gate))

        self.assertFalse(decision["safeToClaim"])
        self.assertEqual(decision["sourceSummary"]["rchRemoteOnlyRequired"], True)
        self.assertIsNone(
            decision["sourceSummary"]["rchSafeToLaunchCargoVerification"]
        )
        self.assertIn("rch_remote_verification_required", decision["whyNotSafe"])
        claim = [a for a in decision["argvActions"] if a["actionKind"] == "claim"][0]
        self.assertFalse(claim["runnable"])

    def test_claim_gate_missing_remote_required_authority_fails_closed(self):
        gate = safe_gate()
        gate["sourceAuthority"].pop("rchRemoteOnlyRequired")

        decision = consumer.consume(envelope(gate))

        self.assertFalse(decision["safeToClaim"])
        self.assertIsNone(decision["sourceSummary"]["rchRemoteOnlyRequired"])
        self.assertIn(
            "claim_gate_rch_remote_only_required_missing",
            decision["whyNotSafe"],
        )
        claim = [a for a in decision["argvActions"] if a["actionKind"] == "claim"][0]
        self.assertFalse(claim["runnable"])

    def test_claim_gate_unsafe_or_stale_reason_contradiction_fails_closed(self):
        gate = safe_gate()
        gate["unsafeReasons"] = ["peer_dirty_file"]
        gate["staleReasons"] = ["stale_assignee"]

        decision = consumer.consume(envelope(gate))

        self.assertFalse(decision["safeToClaim"])
        self.assertTrue(decision["mutatingActionsRequireHuman"])
        self.assertIn("claim_gate_unsafe_reasons_present", decision["whyNotSafe"])
        self.assertIn("claim_gate_stale_reasons_present", decision["whyNotSafe"])
        self.assertIn("peer_dirty_file", decision["whyNotSafe"])
        self.assertIn("stale_assignee", decision["whyNotSafe"])
        claim = [a for a in decision["argvActions"] if a["actionKind"] == "claim"][0]
        self.assertFalse(claim["runnable"])
        self.assertEqual(claim["reason"], "mutating_action_requires_safe_gate")

    def test_claim_gate_authority_degraded_codes_fail_closed(self):
        gate = safe_gate()
        gate["degradedCodes"] = [
            "beads_tracker_stale",
            "beads_metadata_only_stale",
            "bv_recommendation_stale",
        ]

        decision = consumer.consume(envelope(gate))

        self.assertFalse(decision["safeToClaim"])
        self.assertTrue(decision["mutatingActionsRequireHuman"])
        for reason in [
            "claim_gate_degraded_authority:beads_tracker_stale",
            "claim_gate_degraded_authority:beads_metadata_only_stale",
            "claim_gate_degraded_authority:bv_recommendation_stale",
        ]:
            self.assertIn(reason, decision["whyNotSafe"])
        claim = [a for a in decision["argvActions"] if a["actionKind"] == "claim"][0]
        self.assertFalse(claim["runnable"])
        self.assertEqual(claim["reason"], "mutating_action_requires_safe_gate")

    def test_claim_gate_envelope_authority_degraded_codes_fail_closed(self):
        decision = consumer.consume(
            envelope(
                safe_gate(),
                degraded=[
                    {"code": "beads_tracker_stale", "severity": "high"},
                    {"code": "no_relevant_results", "severity": "info"},
                ],
            )
        )

        self.assertFalse(decision["safeToClaim"])
        self.assertTrue(decision["mutatingActionsRequireHuman"])
        self.assertIn(
            "claim_gate_degraded_authority:beads_tracker_stale",
            decision["whyNotSafe"],
        )
        self.assertNotIn(
            "claim_gate_degraded_authority:no_relevant_results",
            decision["whyNotSafe"],
        )
        claim = [a for a in decision["argvActions"] if a["actionKind"] == "claim"][0]
        self.assertFalse(claim["runnable"])
        self.assertEqual(claim["reason"], "mutating_action_requires_safe_gate")

    def test_candidate_decision_mismatch_fails_closed(self):
        gate = safe_gate()
        gate["selectedCandidate"]["decision"] = "blocked_by_dependency"

        decision = consumer.consume(envelope(gate))

        self.assertFalse(decision["safeToClaim"])
        self.assertIn(
            "claim_gate_candidate_decision:blocked_by_dependency",
            decision["whyNotSafe"],
        )
        claim = [a for a in decision["argvActions"] if a["actionKind"] == "claim"][0]
        self.assertFalse(claim["runnable"])

    def test_requested_candidate_mismatch_fails_closed(self):
        gate = safe_gate()
        gate["selectedCandidate"]["id"] = "bd-other.1"

        decision = consumer.consume(envelope(gate))

        self.assertFalse(decision["safeToClaim"])
        self.assertTrue(decision["mutatingActionsRequireHuman"])
        self.assertIn(
            "claim_gate_candidate_id_mismatch:bd-safe.1:bd-other.1",
            decision["whyNotSafe"],
        )
        claim = [a for a in decision["argvActions"] if a["actionKind"] == "claim"][0]
        self.assertFalse(claim["runnable"])
        self.assertEqual(claim["reason"], "mutating_action_requires_safe_gate")

    def test_claim_action_candidate_mismatch_fails_closed(self):
        gate = safe_gate()
        gate["claimCommandAction"] = safe_action(
            "bead_claim_candidate",
            ["br", "update", "bd-other.1", "--status", "in_progress", "--json"],
            mutates=True,
        )

        decision = consumer.consume(envelope(gate))

        self.assertFalse(decision["safeToClaim"])
        self.assertTrue(decision["mutatingActionsRequireHuman"])
        self.assertIn(
            "claim_gate_claim_action_candidate_mismatch:bd-safe.1:bd-other.1",
            decision["whyNotSafe"],
        )
        claim = [a for a in decision["argvActions"] if a["actionKind"] == "claim"][0]
        self.assertFalse(claim["runnable"])
        self.assertEqual(claim["reason"], "mutating_action_requires_safe_gate")

    def test_claim_action_must_set_candidate_in_progress(self):
        gate = safe_gate()
        gate["claimCommandAction"] = safe_action(
            "bead_claim_candidate",
            ["br", "update", "bd-safe.1", "--title", "Retitled", "--json"],
            mutates=True,
        )

        decision = consumer.consume(envelope(gate))

        self.assertFalse(decision["safeToClaim"])
        self.assertTrue(decision["mutatingActionsRequireHuman"])
        self.assertIn(
            "claim_gate_claim_action_not_in_progress",
            decision["whyNotSafe"],
        )
        claim = [a for a in decision["argvActions"] if a["actionKind"] == "claim"][0]
        self.assertFalse(claim["runnable"])
        self.assertEqual(claim["reason"], "mutating_action_requires_safe_gate")

    def test_claim_action_rejects_duplicate_status_flags(self):
        duplicate_status_argvs = [
            [
                "br",
                "update",
                "bd-safe.1",
                "--status",
                "in_progress",
                "--status",
                "open",
                "--json",
            ],
            [
                "br",
                "update",
                "bd-safe.1",
                "--status=in_progress",
                "--status=closed",
                "--json",
            ],
        ]
        for argv in duplicate_status_argvs:
            with self.subTest(argv=argv):
                gate = safe_gate()
                gate["claimCommandAction"] = safe_action(
                    "bead_claim_candidate",
                    argv,
                    mutates=True,
                )

                decision = consumer.consume(envelope(gate))

                self.assertFalse(decision["safeToClaim"])
                self.assertTrue(decision["mutatingActionsRequireHuman"])
                self.assertIn(
                    "claim_gate_claim_action_not_in_progress",
                    decision["whyNotSafe"],
                )
                claim = [
                    action
                    for action in decision["argvActions"]
                    if action["actionKind"] == "claim"
                ][0]
                self.assertFalse(claim["runnable"])
                self.assertEqual(claim["reason"], "mutating_action_requires_safe_gate")

    def test_claim_action_accepts_equals_status_in_progress(self):
        gate = safe_gate()
        gate["claimCommandAction"] = safe_action(
            "bead_claim_candidate",
            ["br", "update", "bd-safe.1", "--status=in_progress", "--json"],
            mutates=True,
        )

        decision = consumer.consume(envelope(gate))

        self.assertTrue(decision["safeToClaim"])
        self.assertEqual(decision["whyNotSafe"], [])
        claim = [a for a in decision["argvActions"] if a["actionKind"] == "claim"][0]
        self.assertTrue(claim["runnable"])

    def test_claim_action_must_emit_machine_readable_json(self):
        gate = safe_gate()
        gate["claimCommandAction"] = safe_action(
            "bead_claim_candidate",
            ["br", "update", "bd-safe.1", "--status=in_progress"],
            mutates=True,
        )

        decision = consumer.consume(envelope(gate))

        self.assertFalse(decision["safeToClaim"])
        self.assertTrue(decision["mutatingActionsRequireHuman"])
        self.assertIn("claim_gate_claim_action_not_json", decision["whyNotSafe"])
        claim = [a for a in decision["argvActions"] if a["actionKind"] == "claim"][0]
        self.assertFalse(claim["runnable"])
        self.assertEqual(claim["reason"], "mutating_action_requires_safe_gate")

    def test_missing_claim_action_fails_closed(self):
        gate = safe_gate()
        gate["claimCommandAction"] = None

        decision = consumer.consume(envelope(gate))

        self.assertFalse(decision["safeToClaim"])
        self.assertIn("claim_gate_missing_claim_action", decision["whyNotSafe"])
        self.assertFalse(any(a["actionKind"] == "claim" for a in decision["argvActions"]))

    def test_shell_required_and_display_only_actions_are_review_required(self):
        gate = safe_gate()
        gate["nextCommandActions"].append(
            safe_action(
                "legacy_shell",
                ["sh", "-c", "br show bd-safe.1 --json"],
                copy_safety="shell_required_review",
                shell=True,
            )
        )
        gate["nextCommandActions"].append(
            safe_action(
                "display_only",
                ["br", "show", "bd-safe.1", "--json"],
                copy_safety="display_only",
            )
        )

        decision = consumer.consume(envelope(gate))
        by_id = {action["commandId"]: action for action in decision["argvActions"]}
        self.assertTrue(by_id["legacy_shell"]["reviewRequired"])
        self.assertFalse(by_id["legacy_shell"]["runnable"])
        self.assertEqual(by_id["legacy_shell"]["argv"], [])
        self.assertEqual(by_id["legacy_shell"]["reason"], "shell_required_review")
        self.assertTrue(by_id["display_only"]["reviewRequired"])
        self.assertEqual(by_id["display_only"]["argv"], [])
        self.assertEqual(by_id["display_only"]["reason"], "copy_safety:display_only")

    def test_mutating_next_command_action_fails_closed(self):
        gate = safe_gate()
        gate["nextCommandActions"].append(
            safe_action(
                "bad_reopen",
                ["br", "reopen", "bd-safe.1", "--json"],
                mutates=True,
            )
        )

        decision = consumer.consume(envelope(gate))

        self.assertFalse(decision["safeToClaim"])
        self.assertTrue(decision["mutatingActionsRequireHuman"])
        self.assertIn(
            "claim_gate_next_action_mutates_state:bad_reopen",
            decision["whyNotSafe"],
        )
        by_id = {action["commandId"]: action for action in decision["argvActions"]}
        self.assertFalse(by_id["bad_reopen"]["runnable"])
        self.assertEqual(
            by_id["bad_reopen"]["reason"],
            "mutating_action_requires_safe_gate",
        )

    def test_hidden_beads_mutation_in_next_command_action_fails_closed(self):
        gate = safe_gate()
        gate["nextCommandActions"] = [
            safe_action(
                "mislabeled_update",
                ["br", "update", "bd-safe.1", "--status", "in_progress", "--json"],
                mutates=False,
            )
        ]

        decision = consumer.consume(envelope(gate))

        self.assertFalse(decision["safeToClaim"])
        self.assertIn(
            "claim_gate_next_action_mutates_state:mislabeled_update",
            decision["whyNotSafe"],
        )
        by_id = {action["commandId"]: action for action in decision["argvActions"]}
        self.assertFalse(by_id["mislabeled_update"]["runnable"])

    def test_claim_action_must_be_mutating_beads_update(self):
        gate = safe_gate()
        gate["claimCommandAction"] = safe_action(
            "bead_claim_candidate",
            ["br", "show", "bd-safe.1", "--json"],
            mutates=False,
        )

        decision = consumer.consume(envelope(gate))

        self.assertFalse(decision["safeToClaim"])
        self.assertIn(
            "claim_gate_claim_action_not_mutating",
            decision["whyNotSafe"],
        )
        self.assertIn(
            "claim_gate_claim_action_not_bead_update",
            decision["whyNotSafe"],
        )
        claim = [a for a in decision["argvActions"] if a["actionKind"] == "claim"][0]
        self.assertFalse(claim["runnable"])

    def test_malformed_structured_argv_entries_are_review_required(self):
        gate = safe_gate()
        gate["nextCommandActions"] = [
            safe_action("non_string_argv", ["br", "show", 123, "--json"]),
            safe_action("empty_argv", ["br", "", "--json"]),
        ]
        gate["claimCommandAction"] = safe_action(
            "malformed_claim",
            ["br", "update", "", "--status", "in_progress", "--json"],
            mutates=True,
        )

        decision = consumer.consume(envelope(gate))

        self.assertFalse(decision["safeToClaim"])
        self.assertTrue(decision["mutatingActionsRequireHuman"])
        self.assertIn(
            "claim_gate_claim_action_not_bead_update",
            decision["whyNotSafe"],
        )
        by_id = {action["commandId"]: action for action in decision["argvActions"]}
        for command_id in ("non_string_argv", "empty_argv", "malformed_claim"):
            self.assertFalse(by_id[command_id]["runnable"])
            self.assertTrue(by_id[command_id]["reviewRequired"])
            self.assertEqual(by_id[command_id]["argv"], [])
            self.assertEqual(by_id[command_id]["reason"], "invalid_argv_item")

    def test_candidate_not_found_is_not_safe(self):
        gate = safe_gate()
        gate["safeToClaim"] = False
        gate["verdict"] = "candidate_not_found"
        gate["selectedCandidate"] = None
        gate["requestedCandidateId"] = "bd-missing"
        gate["unsafeReasons"] = ["candidate_not_found:bd-missing"]
        gate["claimCommandAction"] = None

        decision = consumer.consume(envelope(gate))
        self.assertFalse(decision["safeToClaim"])
        self.assertEqual(decision["candidateId"], "bd-missing")
        self.assertIn("candidate_not_found:bd-missing", decision["whyNotSafe"])
        self.assertFalse(any(a["actionKind"] == "claim" for a in decision["argvActions"]))


class WorkPacketConsumer(unittest.TestCase):
    def test_no_safe_claim_fixture_matrix_blocks_claim_fields(self):
        cases = [
            (
                "tests/fixtures/swarm_work_packet/bv_timeout_no_output.json",
                {"bv", "beads"},
            ),
            (
                "tests/fixtures/swarm_work_packet/beads_command_timeout_no_output.json",
                {"beads", "agent_mail"},
            ),
            (
                "tests/fixtures/swarm_work_packet/rollup_only_no_claimable_child.json",
                {"beads"},
            ),
            (
                "tests/fixtures/swarm_work_packet/crowded_checkout.json",
                {"beads", "agent_mail", "git"},
            ),
            (
                "tests/fixtures/swarm_work_packet/agent_mail_database_contention_timeout.json",
                {"agent_mail", "beads", "rch"},
            ),
            (
                "tests/fixtures/swarm_work_packet/agent_mail_degraded_read_only.json",
                {"agent_mail", "rch"},
            ),
            (
                "tests/fixtures/swarm_work_packet/degraded_mail_rch_topology.json",
                {"agent_mail", "rch"},
            ),
            (
                "tests/fixtures/swarm_work_packet/tracker_mismatch.json",
                {"beads"},
            ),
        ]

        for relative_path, expected_sources in cases:
            with self.subTest(relative_path=relative_path):
                root = load_fixture(relative_path)
                packet = consumer.get_payload(root)
                self.assertIsInstance(packet, dict)

                decision = consumer.consume(root)
                self.assertFalse(decision["safeToClaim"])
                self.assertTrue(decision["whyNotSafe"])
                self.assertFalse(
                    any(
                        action["runnable"] and action["mutatesState"]
                        for action in decision["argvActions"]
                    )
                )
                self.assertFalse(
                    any(
                        action["runnable"] and action["actionKind"] == "claim"
                        for action in decision["argvActions"]
                    )
                )

                recommended = consumer.dict_or_empty(packet.get("recommendedAction"))
                safe_fields = []
                if "safeToClaim" in packet:
                    safe_fields.append(("packet.safeToClaim", packet.get("safeToClaim")))
                if "safeToClaim" in recommended:
                    safe_fields.append(
                        ("recommendedAction.safeToClaim", recommended.get("safeToClaim"))
                    )
                self.assertTrue(safe_fields, f"{relative_path} lacks safeToClaim field")
                for field, value in safe_fields:
                    self.assertIs(
                        value,
                        False,
                        f"{relative_path} must not mark {field} claim-safe",
                    )

                candidate_decisions = []
                for candidate in consumer.list_items(packet.get("candidates")):
                    if isinstance(candidate, dict):
                        candidate_decisions.append(candidate.get("decision"))
                lane = packet.get("candidateLane")
                if isinstance(lane, dict):
                    candidate_decisions.append(lane.get("decision"))
                self.assertNotIn(
                    "safe_to_claim",
                    candidate_decisions,
                    f"{relative_path} must keep unsafe candidates diagnostic-only",
                )

                sources = {
                    str(source.get("source")).replace("-", "_")
                    for source in consumer.list_items(packet.get("sourceProvenance"))
                    if isinstance(source, dict)
                }
                self.assertTrue(
                    expected_sources.issubset(sources),
                    f"{relative_path} missing sources {expected_sources - sources}",
                )

    def test_bv_timeout_no_output_fixture_surfaces_liveness_blockers(self):
        root = load_fixture("tests/fixtures/swarm_work_packet/bv_timeout_no_output.json")

        decision = consumer.consume(root)

        self.assertFalse(decision["safeToClaim"])
        self.assertIn(
            "packet_degraded_authority:bv_command_timeout",
            decision["whyNotSafe"],
        )
        self.assertIn(
            "packet_degraded_authority:bv_no_output",
            decision["whyNotSafe"],
        )
        self.assertIn("bv_timeout_no_output", decision["whyNotSafe"])
        self.assertEqual(decision["argvActions"], [])
        self.assertEqual(
            [
                (entry["code"], entry["source"], entry["severity"])
                for entry in decision["degradedSummary"][0:2]
            ],
            [
                ("bv_command_timeout", "bv", "warning"),
                ("bv_no_output", "bv", "warning"),
            ],
        )

    def test_envelope_degraded_fixture_blocks_optimistic_payload(self):
        root = load_fixture(
            "tests/fixtures/swarm_work_packet/envelope_beads_authority_degraded.json"
        )
        packet = consumer.get_payload(root)
        self.assertTrue(packet["safeToClaim"])
        self.assertTrue(packet["recommendedAction"]["safeToClaim"])
        self.assertEqual(packet["candidates"][0]["decision"], "safe_to_claim")

        decision = consumer.consume(root)

        self.assertFalse(decision["safeToClaim"])
        self.assertIn(
            "packet_degraded_authority:beads_tracker_stale",
            decision["whyNotSafe"],
        )
        self.assertNotIn(
            "packet_degraded_authority:no_relevant_results",
            decision["whyNotSafe"],
        )
        self.assertFalse(
            any(
                action["runnable"] and action["mutatesState"]
                for action in decision["argvActions"]
            )
        )

    def test_healthy_fixture_is_claim_safe_positive_control(self):
        packet = load_fixture("tests/fixtures/swarm_work_packet/healthy_small.json")
        decision = consumer.consume(packet)

        self.assertTrue(decision["safeToClaim"])
        self.assertEqual(decision["decision"], "safe_to_claim")
        self.assertEqual(decision["whyNotSafe"], [])
        self.assertEqual(decision["sourceSummary"]["reservationAuthoritative"], True)
        self.assertEqual(decision["sourceSummary"]["inboxAuthoritative"], True)

    def test_work_packet_authority_degraded_codes_fail_closed(self):
        packet = load_fixture("tests/fixtures/swarm_work_packet/healthy_small.json")
        source_provenance = packet["data"]["sourceProvenance"]
        source_provenance[0]["degradedCodes"] = ["beads_tracker_stale"]
        source_provenance[1]["degradedCodes"] = ["bv_recommendation_stale"]

        decision = consumer.consume(packet)

        self.assertFalse(decision["safeToClaim"])
        self.assertIn(
            "packet_degraded_authority:beads_tracker_stale",
            decision["whyNotSafe"],
        )
        self.assertIn(
            "packet_degraded_authority:bv_recommendation_stale",
            decision["whyNotSafe"],
        )
        self.assertFalse(
            any(
                action["runnable"] and action["mutatesState"]
                for action in decision["argvActions"]
            )
        )

    def test_work_packet_nested_authority_blocker_codes_fail_closed(self):
        packet = load_fixture("tests/fixtures/swarm_work_packet/healthy_small.json")
        data = packet["data"]
        data["trackerIntegrity"]["degradedCodes"] = ["beads_tracker_stale"]
        data["coordination"]["agentMail"]["degradedCodes"] = [
            "agent_mail_unavailable"
        ]
        data["rchProofPosture"] = {
            "remoteOnlyRequired": True,
            "safeToLaunchCargoVerification": True,
            "blockerCodes": ["rch_verify_topology_blocked"],
            "knownBlockers": [
                {
                    "code": "stale_binary_suspected",
                    "degradedCodes": ["rch_verify_local_fallback_refused"],
                }
            ],
        }

        decision = consumer.consume(packet)

        self.assertFalse(decision["safeToClaim"])
        for reason in [
            "packet_degraded_authority:beads_tracker_stale",
            "packet_degraded_authority:agent_mail_unavailable",
            "packet_degraded_authority:rch_verify_topology_blocked",
            "packet_degraded_authority:stale_binary_suspected",
            "packet_degraded_authority:rch_verify_local_fallback_refused",
        ]:
            self.assertIn(reason, decision["whyNotSafe"])
        self.assertFalse(
            any(
                action["runnable"] and action["mutatesState"]
                for action in decision["argvActions"]
            )
        )

    def test_work_packet_rch_selector_contradiction_fails_closed(self):
        packet = load_fixture("tests/fixtures/swarm_work_packet/healthy_small.json")
        data = packet["data"]
        data["recommendedAction"] = {
            "action": "inspect_and_claim",
            "candidateId": "bd-safe",
            "safeToClaim": True,
            "suggestedCommands": [],
            "suggestedCommandActions": [
                safe_action(
                    "bead_claim_candidate",
                    ["br", "update", "bd-safe", "--status", "in_progress", "--json"],
                    mutates=True,
                )
            ],
        }
        data["rchProofPosture"] = {
            "remoteOnlyRequired": True,
            "safeToLaunchCargoVerification": True,
            "blockerCodes": [],
            "knownBlockers": [],
            "selectorAdmissionProbe": {
                "workersVsSelectionContradiction": True,
                "selectionFailureReason": "no_workers_with_rust_installed",
                "selectedWorker": None,
            },
        }

        decision = consumer.consume(packet)

        self.assertFalse(decision["safeToClaim"])
        self.assertIn(
            "packet_degraded_authority:rch_selector_admission_contradiction",
            decision["whyNotSafe"],
        )
        claim = decision["argvActions"][0]
        self.assertFalse(claim["runnable"])
        self.assertTrue(claim["reviewRequired"])

    def test_rch_selector_contradiction_fixture_blocks_optimistic_payload(self):
        root = load_fixture(
            "tests/fixtures/swarm_work_packet/rch_selector_contradiction.json"
        )
        packet = consumer.get_payload(root)
        self.assertFalse(packet["safeToClaim"])
        self.assertFalse(packet["recommendedAction"]["safeToClaim"])
        self.assertEqual(packet["candidates"][0]["decision"], "safe_to_claim")
        self.assertFalse(
            any(
                action["mutatesState"]
                for action in packet["recommendedAction"]["suggestedCommandActions"]
            )
        )
        self.assertIn(
            "rch_selector_admission_contradiction",
            packet["rchProofPosture"]["blockerCodes"],
        )
        self.assertFalse(
            packet["rchProofPosture"]["safeToLaunchCargoVerification"]
        )

        decision = consumer.consume(root)

        self.assertFalse(decision["safeToClaim"])
        self.assertIn(
            "packet_degraded_authority:rch_selector_admission_contradiction",
            decision["whyNotSafe"],
        )
        self.assertFalse(
            any(
                action["runnable"] and action["mutatesState"]
                for action in decision["argvActions"]
            )
        )

    def test_work_packet_blocked_candidate_status_fails_closed(self):
        packet = load_fixture("tests/fixtures/swarm_work_packet/healthy_small.json")
        data = packet["data"]
        data["candidateLane"]["status"] = "blocked"
        data["recommendedAction"] = {
            "action": "inspect_and_claim",
            "candidateId": "bd-safe",
            "safeToClaim": True,
            "suggestedCommands": [],
            "suggestedCommandActions": [
                safe_action(
                    "bead_claim_candidate",
                    ["br", "update", "bd-safe", "--status", "in_progress", "--json"],
                    mutates=True,
                )
            ],
        }

        decision = consumer.consume(packet)

        self.assertFalse(decision["safeToClaim"])
        self.assertIn("candidate_status_not_open:blocked", decision["whyNotSafe"])
        claim = decision["argvActions"][0]
        self.assertFalse(claim["runnable"])
        self.assertTrue(claim["reviewRequired"])

    def test_blocked_candidate_claimable_fixture_blocks_optimistic_payload(self):
        root = load_fixture(
            "tests/fixtures/swarm_work_packet/blocked_candidate_claimable.json"
        )
        packet = consumer.get_payload(root)
        self.assertTrue(packet["safeToClaim"])
        self.assertTrue(packet["recommendedAction"]["safeToClaim"])
        self.assertEqual(packet["candidates"][0]["status"], "blocked")
        self.assertEqual(packet["candidates"][0]["decision"], "safe_to_claim")

        decision = consumer.consume(root)

        self.assertFalse(decision["safeToClaim"])
        self.assertIn("candidate_status_not_open:blocked", decision["whyNotSafe"])
        self.assertFalse(
            any(
                action["runnable"] and action["mutatesState"]
                for action in decision["argvActions"]
            )
        )

    def test_work_packet_envelope_authority_degraded_codes_fail_closed(self):
        packet = load_fixture("tests/fixtures/swarm_work_packet/healthy_small.json")
        packet["degraded"] = [
            {"code": "beads_tracker_stale", "severity": "high"},
            {"code": "no_relevant_results", "severity": "info"},
        ]

        decision = consumer.consume(packet)

        self.assertFalse(decision["safeToClaim"])
        self.assertIn(
            "packet_degraded_authority:beads_tracker_stale",
            decision["whyNotSafe"],
        )
        self.assertNotIn(
            "packet_degraded_authority:no_relevant_results",
            decision["whyNotSafe"],
        )
        self.assertFalse(
            any(
                action["runnable"] and action["mutatesState"]
                for action in decision["argvActions"]
            )
        )

    def test_packet_command_actions_are_bounded(self):
        packet = load_fixture("tests/fixtures/swarm_work_packet/healthy_small.json")
        packet["data"]["recommendedAction"] = {
            "action": "inspect_and_claim",
            "candidateId": "bd-bounded-actions.1",
            "safeToClaim": True,
            "suggestedCommands": [],
            "suggestedCommandActions": [
                safe_action(
                    f"recommended_{index}",
                    ["ee", "status", "--json"],
                )
                for index in range(32)
            ],
        }
        packet["data"]["candidates"] = [
            {
                "id": "bd-bounded-actions.1",
                "decision": "safe_to_claim",
                "unsafeReasons": [],
                "staleReasons": [],
            }
        ]

        decision = consumer.consume(packet)

        self.assertTrue(decision["safeToClaim"])
        self.assertEqual(
            len(decision["argvActions"]),
            consumer.DECISION_ACTION_LIMIT,
        )
        self.assertEqual(decision["argvActions"][0]["commandId"], "recommended_0")
        self.assertEqual(
            decision["argvActions"][-1]["commandId"],
            f"recommended_{consumer.DECISION_ACTION_LIMIT - 1}",
        )
        self.assertNotIn(
            f"recommended_{consumer.DECISION_ACTION_LIMIT}",
            {action["commandId"] for action in decision["argvActions"]},
        )

    def test_packet_oversized_argv_is_not_runnable(self):
        packet = load_fixture("tests/fixtures/swarm_work_packet/healthy_small.json")
        packet["data"]["recommendedAction"] = {
            "action": "inspect_and_claim",
            "candidateId": "bd-oversized-argv.1",
            "safeToClaim": True,
            "suggestedCommands": [],
            "suggestedCommandActions": [
                safe_action(
                    "oversized_argv",
                    [
                        "ee",
                        *[
                            f"arg-{index}"
                            for index in range(consumer.DECISION_ARGV_PART_LIMIT)
                        ],
                    ],
                )
            ],
        }
        packet["data"]["candidates"] = [
            {
                "id": "bd-oversized-argv.1",
                "decision": "safe_to_claim",
                "unsafeReasons": [],
                "staleReasons": [],
            }
        ]

        decision = consumer.consume(packet)

        self.assertFalse(decision["safeToClaim"])
        self.assertIn(
            "malformed_packet_recommended_command_action_argv",
            decision["whyNotSafe"],
        )
        action = decision["argvActions"][0]
        self.assertEqual(action["commandId"], "oversized_argv")
        self.assertFalse(action["runnable"])
        self.assertTrue(action["reviewRequired"])
        self.assertEqual(action["argv"], [])
        self.assertEqual(action["reason"], "argv_too_long")

    def test_missing_required_packet_recommended_command_action_fields_fail_closed(self):
        for field, suffix in consumer.CLAIM_GATE_COMMAND_ACTION_REQUIRED_FIELDS:
            with self.subTest(field=field):
                packet = load_fixture(
                    "tests/fixtures/swarm_work_packet/healthy_small.json"
                )
                action = safe_action(
                    "bead_claim_candidate",
                    ["br", "update", "bd-safe", "--status", "in_progress", "--json"],
                    mutates=True,
                )
                action.pop(field)
                packet["data"]["recommendedAction"] = {
                    "action": "inspect_and_claim",
                    "candidateId": "bd-safe",
                    "safeToClaim": True,
                    "suggestedCommands": [],
                    "suggestedCommandActions": [action],
                }
                packet["data"]["candidates"] = [
                    {
                        "id": "bd-safe",
                        "decision": "safe_to_claim",
                        "unsafeReasons": [],
                        "staleReasons": [],
                    }
                ]

                decision = consumer.consume(packet)

                self.assertFalse(decision["safeToClaim"])
                self.assertIn(
                    f"missing_packet_recommended_command_action_{suffix}",
                    decision["whyNotSafe"],
                )
                self.assertFalse(
                    any(
                        action["runnable"] and action["mutatesState"]
                        for action in decision["argvActions"]
                    )
                )

    def test_extra_packet_command_action_fields_fail_closed(self):
        packet = load_fixture("tests/fixtures/swarm_work_packet/healthy_small.json")
        action = safe_action(
            "bead_claim_candidate",
            ["br", "update", "bd-safe", "--status", "in_progress", "--json"],
            mutates=True,
        )
        action["shellCommand"] = "br update bd-safe --status in_progress --json"
        packet["data"]["recommendedAction"] = {
            "action": "inspect_and_claim",
            "candidateId": "bd-safe",
            "safeToClaim": True,
            "suggestedCommands": [],
            "suggestedCommandActions": [action],
        }
        packet["data"]["candidates"] = [
            {
                "id": "bd-safe",
                "decision": "safe_to_claim",
                "unsafeReasons": [],
                "staleReasons": [],
            }
        ]

        decision = consumer.consume(packet)

        self.assertFalse(decision["safeToClaim"])
        self.assertIn(
            "malformed_packet_recommended_command_action_unexpected_field",
            decision["whyNotSafe"],
        )
        claim = [
            action
            for action in decision["argvActions"]
            if action["actionKind"] == "recommended"
        ][0]
        self.assertFalse(claim["runnable"])
        self.assertEqual(claim["reason"], "malformed_command_action")

    def test_null_packet_command_action_required_fields_fail_closed(self):
        for field, suffix in consumer.CLAIM_GATE_COMMAND_ACTION_REQUIRED_FIELDS:
            with self.subTest(field=field):
                packet = load_fixture(
                    "tests/fixtures/swarm_work_packet/healthy_small.json"
                )
                action = safe_action(
                    "bead_claim_candidate",
                    ["br", "update", "bd-safe", "--status", "in_progress", "--json"],
                    mutates=True,
                )
                action[field] = None
                packet["data"]["recommendedAction"] = {
                    "action": "inspect_and_claim",
                    "candidateId": "bd-safe",
                    "safeToClaim": True,
                    "suggestedCommands": [],
                    "suggestedCommandActions": [action],
                }
                packet["data"]["candidates"] = [
                    {
                        "id": "bd-safe",
                        "decision": "safe_to_claim",
                        "unsafeReasons": [],
                        "staleReasons": [],
                    }
                ]

                decision = consumer.consume(packet)

                self.assertFalse(decision["safeToClaim"])
                self.assertIn(
                    f"malformed_packet_recommended_command_action_{suffix}",
                    decision["whyNotSafe"],
                )
                self.assertFalse(
                    any(
                        action["runnable"] and action["mutatesState"]
                        for action in decision["argvActions"]
                    )
                )

    def test_malformed_packet_command_action_fields_fail_closed(self):
        packet = load_fixture("tests/fixtures/swarm_work_packet/healthy_small.json")
        packet["data"]["recommendedAction"] = {
            "action": "inspect_and_claim",
            "candidateId": "bd-safe",
            "safeToClaim": True,
            "suggestedCommands": [],
            "suggestedCommandActions": [
                "not-a-command-action",
                {
                    **safe_action(
                        "bead_claim_candidate",
                        [
                            "br",
                            "update",
                            "bd-safe",
                            "--status",
                            "in_progress",
                            "--json",
                        ],
                        mutates=True,
                    ),
                    "shellRequired": "false",
                },
            ],
        }
        packet["data"]["candidates"] = [
            {
                "id": "bd-safe",
                "decision": "safe_to_claim",
                "unsafeReasons": [],
                "staleReasons": [],
            }
        ]
        packet["data"]["verification"] = {
            "requiredCommands": [
                {
                    "commandAction": {
                        **safe_action(
                            "json_schema_parse",
                            [
                                "jq",
                                "empty",
                                "docs/schemas/ee.agent.work_packet_gate_decision.v1.json",
                            ],
                        ),
                        "displayCommand": {"cmd": "jq empty schema"},
                    }
                }
            ],
            "staticChecks": [
                {
                    "commandAction": {
                        **safe_action(
                            "diff_check",
                            [
                                "git",
                                "diff",
                                "--check",
                                "--",
                                "scripts/agent_consume_work_packet_gate.py",
                            ],
                        ),
                        "argv": "git diff --check",
                    }
                }
            ],
        }
        packet["data"]["coordination"]["agentMail"]["fallbackActions"] = [
            {
                "kind": "support_bundle",
                "commandAction": {
                    **safe_action(
                        "agent_mail_support_bundle",
                        ["ee", "support-bundle", "--agent-mail", "--json"],
                    ),
                    "mutatesState": "false",
                },
            }
        ]

        decision = consumer.consume(packet)

        self.assertFalse(decision["safeToClaim"])
        for reason in [
            "malformed_packet_recommended_command_action",
            "malformed_packet_recommended_command_action_shell_required",
            "malformed_packet_required_command_action_display_command",
            "malformed_packet_static_check_action_argv",
            "malformed_packet_fallback_command_action_mutates_state",
        ]:
            self.assertIn(reason, decision["whyNotSafe"])
        self.assertFalse(
            any(
                action["runnable"] and action["mutatesState"]
                for action in decision["argvActions"]
            )
        )

    def test_agent_mail_authority_flags_downgrade_otherwise_safe_packet(self):
        packet = load_fixture("tests/fixtures/swarm_work_packet/healthy_small.json")
        agent_mail = packet["data"]["coordination"]["agentMail"]
        agent_mail["reservationAuthoritative"] = False
        agent_mail["inboxAuthoritative"] = None

        decision = consumer.consume(packet)

        self.assertFalse(decision["safeToClaim"])
        self.assertEqual(decision["sourceSummary"]["reservationAuthoritative"], False)
        self.assertIsNone(decision["sourceSummary"]["inboxAuthoritative"])
        self.assertIn("reservation_evidence_not_authoritative", decision["whyNotSafe"])
        self.assertIn("inbox_evidence_not_authoritative", decision["whyNotSafe"])

    def test_bv_timeout_fixture_blocks_claim_and_refuses_legacy_commands(self):
        packet = load_fixture("tests/fixtures/swarm_work_packet/bv_timeout_no_output.json")
        decision = consumer.consume(packet)

        self.assertFalse(decision["safeToClaim"])
        self.assertEqual(decision["candidateId"], "bd-docs.1")
        self.assertEqual(decision["decision"], "blocked")
        self.assertIn("candidate_decision:blocked", decision["whyNotSafe"])
        self.assertIn("bv_timeout_no_output", decision["whyNotSafe"])
        self.assertGreaterEqual(decision["legacyCommandStringsRefused"], 2)
        codes = {entry["code"] for entry in decision["degradedSummary"]}
        self.assertIn("agent_mail_unavailable", codes)
        self.assertIn("rch_worker_topology_blocked", codes)

    def test_beads_timeout_fixture_blocks_claim_from_stale_fallback(self):
        packet = load_fixture(
            "tests/fixtures/swarm_work_packet/beads_command_timeout_no_output.json"
        )
        decision = consumer.consume(packet)

        self.assertFalse(decision["safeToClaim"])
        self.assertEqual(decision["candidateId"], "bd-owned")
        self.assertEqual(decision["decision"], "blocked")
        self.assertIn("source_health:beads_timeout_no_output", decision["whyNotSafe"])
        self.assertIn("fallback_row_already_owned", decision["whyNotSafe"])
        self.assertIn("fallback_rows_not_authoritative", decision["whyNotSafe"])
        self.assertIn(
            "beads_tracker_not_authoritative:db_jsonl_count_mismatch",
            decision["whyNotSafe"],
        )
        self.assertIn("tracker_requires_candidate_downgrade", decision["whyNotSafe"])
        self.assertGreaterEqual(decision["legacyCommandStringsRefused"], 2)
        self.assertEqual(decision["sourceSummary"]["trackerAuthoritative"], False)
        codes = {entry["code"] for entry in decision["degradedSummary"]}
        self.assertIn("beads_unavailable", codes)

    def test_crowded_checkout_fixture_exposes_collision_reasons(self):
        packet = load_fixture("tests/fixtures/swarm_work_packet/crowded_checkout.json")
        decision = consumer.consume(packet)

        self.assertFalse(decision["safeToClaim"])
        self.assertEqual(decision["candidateId"], "bd-contested")
        self.assertEqual(decision["decision"], "coordinate_first")
        self.assertIn("candidate_decision:coordinate_first", decision["whyNotSafe"])
        self.assertIn("dirty_path_overlap", decision["whyNotSafe"])
        self.assertEqual(decision["sourceSummary"]["agentMailStatus"], "healthy")

    def test_agent_mail_semantic_readiness_failed_blocks_claim(self):
        packet = load_fixture(
            "tests/fixtures/swarm_work_packet/agent_mail_semantic_readiness_failed.json"
        )
        decision = consumer.consume(packet)

        self.assertFalse(decision["safeToClaim"])
        self.assertEqual(decision["sourceSummary"]["agentMailStatus"], "semantic_readiness_failed")
        self.assertIn("agent_mail_semantic_readiness_failed", decision["whyNotSafe"])
        support = [a for a in decision["argvActions"] if a["commandId"] == "agent_mail_support_bundle"][0]
        self.assertTrue(support["runnable"])
        self.assertFalse(support["mutatesState"])

    def test_tracker_mismatch_downgrades_otherwise_safe_packet(self):
        packet = load_fixture("tests/fixtures/swarm_work_packet/tracker_mismatch.json")

        decision = consumer.consume(packet)
        self.assertFalse(decision["safeToClaim"])
        self.assertIn(
            "beads_tracker_not_authoritative:db_jsonl_count_mismatch",
            decision["whyNotSafe"],
        )
        self.assertIn("tracker_requires_candidate_downgrade", decision["whyNotSafe"])
        self.assertEqual(decision["legacyCommandStringsRefused"], 4)
        self.assertTrue(
            all(not action["mutatesState"] for action in decision["argvActions"])
        )

    def test_missing_tracker_authority_downgrades_otherwise_safe_packet(self):
        packet = load_fixture("tests/fixtures/swarm_work_packet/healthy_small.json")
        packet["data"]["recommendedAction"] = {
            "candidateId": "bd-safe",
            "safeToClaim": True,
            "suggestedCommands": [],
            "suggestedCommandActions": [
                safe_action(
                    "bead_claim_candidate",
                    ["br", "update", "bd-safe", "--status", "in_progress", "--json"],
                    mutates=True,
                )
            ],
        }
        packet["data"]["trackerIntegrity"].pop("brReadsAuthoritative")
        packet["data"]["trackerIntegrity"].pop("health")

        decision = consumer.consume(packet)

        self.assertFalse(decision["safeToClaim"])
        self.assertIsNone(decision["sourceSummary"]["trackerAuthoritative"])
        self.assertIn(
            "beads_tracker_not_authoritative:unknown",
            decision["whyNotSafe"],
        )
        claim = decision["argvActions"][0]
        self.assertFalse(claim["runnable"])
        self.assertEqual(claim["reason"], "mutating_action_requires_safe_gate")

    def test_rollup_only_fixture_blocks_claim_without_mutating_actions(self):
        packet = load_fixture(
            "tests/fixtures/swarm_work_packet/rollup_only_no_claimable_child.json"
        )
        decision = consumer.consume(packet)

        self.assertFalse(decision["safeToClaim"])
        self.assertEqual(decision["candidateId"], "bd-rollup")
        self.assertEqual(decision["decision"], "blocked_rollup")
        self.assertEqual(decision["action"], "blocked_no_action")
        self.assertFalse(decision["mutatingActionsRequireHuman"])
        self.assertIn("candidate_decision:blocked_rollup", decision["whyNotSafe"])
        self.assertIn("candidate_is_rollup_not_leaf", decision["whyNotSafe"])
        self.assertIn("rollup_has_no_claimable_child", decision["whyNotSafe"])
        self.assertIn(
            "packet_recommendation_not_claim_safe:blocked_no_action",
            decision["whyNotSafe"],
        )
        self.assertTrue(
            all(not action["mutatesState"] for action in decision["argvActions"])
        )
        self.assertGreaterEqual(decision["legacyCommandStringsRefused"], 1)

    def test_rch_topology_blocked_case_is_explicit(self):
        packet = load_fixture("tests/fixtures/swarm_work_packet/degraded_mail_rch_topology.json")
        decision = consumer.consume(packet)

        self.assertFalse(decision["safeToClaim"])
        self.assertIn("rch_remote_verification_blocked", decision["whyNotSafe"])
        self.assertEqual(
            decision["sourceSummary"]["rchPosture"],
            "remote_required_fallback_prevented",
        )
        codes = {entry["code"] for entry in decision["degradedSummary"]}
        self.assertIn("rch_remote_required_fallback_prevented", codes)

    def test_remote_required_without_positive_rch_proof_fails_closed(self):
        packet = load_fixture("tests/fixtures/swarm_work_packet/healthy_small.json")
        data = packet["data"]
        data["recommendedAction"] = {
            "action": "inspect_and_claim",
            "candidateId": "bd-safe",
            "safeToClaim": True,
            "suggestedCommands": [],
            "suggestedCommandActions": [
                safe_action(
                    "bead_claim_candidate",
                    ["br", "update", "bd-safe", "--status", "in_progress", "--json"],
                    mutates=True,
                )
            ],
        }
        data["rchProofPosture"] = {
            "remoteOnlyRequired": True,
            "safeToLaunchCargoVerification": None,
            "blockerCodes": [],
        }

        decision = consumer.consume(packet)

        self.assertFalse(decision["safeToClaim"])
        self.assertIsNone(
            decision["sourceSummary"]["rchSafeToLaunchCargoVerification"]
        )
        self.assertIn("rch_remote_verification_required", decision["whyNotSafe"])
        claim = decision["argvActions"][0]
        self.assertFalse(claim["runnable"])
        self.assertEqual(claim["reason"], "mutating_action_requires_safe_gate")

    def test_stale_but_reclaimable_is_not_auto_claimable(self):
        packet = {
            "schema": "ee.swarm.work_packet.v1",
            "recommendedAction": {
                "action": "reopen_stale_work",
                "candidateId": "bd-stale.1",
                "safeToClaim": False,
                "suggestedCommands": [],
            },
            "candidates": [
                {
                    "id": "bd-stale.1",
                    "decision": "stale_but_reclaimable",
                    "unsafeReasons": [],
                    "staleReasons": ["stale_assignee"],
                }
            ],
            "coordination": {"agentMail": {"status": "healthy"}},
            "trackerIntegrity": {"health": "ok", "brReadsAuthoritative": True},
            "rchProofPosture": {"safeToLaunchCargoVerification": True, "blockerCodes": []},
            "sourceProvenance": [],
            "degraded": [],
        }

        decision = consumer.consume(packet)
        self.assertFalse(decision["safeToClaim"])
        self.assertEqual(decision["decision"], "stale_but_reclaimable")
        self.assertIn("stale_assignee", decision["whyNotSafe"])
        self.assertIn("stale_but_reclaimable_requires_inspection", decision["whyNotSafe"])

    def test_safe_work_packet_with_unsafe_reasons_fails_closed(self):
        packet = {
            "schema": "ee.swarm.work_packet.v1",
            "safeToClaim": True,
            "recommendedAction": {
                "action": "inspect_and_claim",
                "candidateId": "bd-contradiction.1",
                "safeToClaim": True,
                "suggestedCommands": [],
                "suggestedCommandActions": [
                    safe_action(
                        "bead_claim_candidate",
                        [
                            "br",
                            "update",
                            "bd-contradiction.1",
                            "--status",
                            "in_progress",
                            "--json",
                        ],
                        mutates=True,
                    )
                ],
            },
            "candidates": [
                {
                    "id": "bd-contradiction.1",
                    "decision": "safe_to_claim",
                    "unsafeReasons": ["peer_dirty_file"],
                    "staleReasons": ["stale_assignee"],
                }
            ],
            "coordination": {"agentMail": {"status": "healthy"}},
            "trackerIntegrity": {"health": "ok", "brReadsAuthoritative": True},
            "rchProofPosture": {
                "safeToLaunchCargoVerification": True,
                "blockerCodes": [],
            },
            "verification": {"requiredCommands": [], "staticChecks": []},
            "doNotProceedBecause": ["global_stop_reason"],
        }

        decision = consumer.consume(packet)

        self.assertFalse(decision["safeToClaim"])
        self.assertTrue(decision["mutatingActionsRequireHuman"])
        self.assertIn("peer_dirty_file", decision["whyNotSafe"])
        self.assertIn("stale_assignee", decision["whyNotSafe"])
        self.assertIn("global_stop_reason", decision["whyNotSafe"])
        action = decision["argvActions"][0]
        self.assertFalse(action["runnable"])
        self.assertEqual(action["reason"], "mutating_action_requires_safe_gate")

    def test_work_packet_unsafe_reasons_are_bounded_in_why_not_safe(self):
        packet = load_fixture("tests/fixtures/swarm_work_packet/healthy_small.json")
        packet["data"]["safeToClaim"] = True
        packet["data"]["recommendedAction"] = {
            "action": "inspect_and_claim",
            "candidateId": "bd-bounded.1",
            "safeToClaim": True,
            "suggestedCommands": [],
            "suggestedCommandActions": [
                safe_action(
                    "bead_claim_candidate",
                    [
                        "br",
                        "update",
                        "bd-bounded.1",
                        "--status",
                        "in_progress",
                        "--json",
                    ],
                    mutates=True,
                )
            ],
        }
        packet["data"]["candidates"] = [
            {
                "id": "bd-bounded.1",
                "decision": "blocked",
                "unsafeReasons": [
                    f"unsafe_reason_{index}" for index in range(32)
                ],
                "staleReasons": [],
            }
        ]

        decision = consumer.consume(packet)

        self.assertFalse(decision["safeToClaim"])
        self.assertEqual(
            len(decision["whyNotSafe"]),
            consumer.DECISION_DIAGNOSTIC_LIMIT,
        )
        self.assertEqual(decision["whyNotSafe"][0], "candidate_decision:blocked")
        self.assertEqual(decision["whyNotSafe"][1], "unsafe_reason_0")
        self.assertEqual(
            decision["whyNotSafe"][-1],
            f"unsafe_reason_{consumer.DECISION_DIAGNOSTIC_LIMIT - 2}",
        )
        self.assertNotIn(
            f"unsafe_reason_{consumer.DECISION_DIAGNOSTIC_LIMIT - 1}",
            decision["whyNotSafe"],
        )

    def test_no_candidate_packet_is_not_safe(self):
        packet = {
            "schema": "ee.swarm.work_packet.v1",
            "recommendedAction": {
                "action": "blocked_no_action",
                "candidateId": None,
                "safeToClaim": None,
                "suggestedCommands": [],
            },
            "candidates": [],
            "coordination": {"agentMail": {"status": "healthy"}},
            "trackerIntegrity": {"health": "ok", "brReadsAuthoritative": True},
            "rchProofPosture": {"safeToLaunchCargoVerification": True, "blockerCodes": []},
            "sourceProvenance": [],
            "degraded": [],
        }

        decision = consumer.consume(packet)
        self.assertFalse(decision["safeToClaim"])
        self.assertIsNone(decision["candidateId"])
        self.assertEqual(decision["decision"], "no_candidate")
        self.assertIn("no_candidate_available", decision["whyNotSafe"])

    def test_malformed_legacy_command_fields_do_not_crash_or_use_string_length(self):
        packet = {
            "schema": "ee.swarm.work_packet.v1",
            "recommendedAction": {
                "action": "blocked_no_action",
                "candidateId": "bd-malformed.1",
                "safeToClaim": False,
                "suggestedCommands": "br update bd-malformed.1 --status in_progress",
                "suggestedCommandActions": None,
            },
            "candidates": [
                {
                    "id": "bd-malformed.1",
                    "decision": "blocked",
                    "unsafeReasons": ["malformed_command_surface"],
                    "staleReasons": [],
                }
            ],
            "coordination": {
                "agentMail": {"status": "healthy", "fallbackActions": "not-a-list"}
            },
            "trackerIntegrity": {"health": "ok", "brReadsAuthoritative": True},
            "rchProofPosture": {"safeToLaunchCargoVerification": True, "blockerCodes": []},
            "verification": {
                "requiredCommands": None,
                "staticChecks": [{"commandTemplate": "cargo check --all-targets"}, None],
            },
            "requiredActions": [
                "br show bd-malformed.1 --json",
                {"command": "git status --short --branch"},
                None,
            ],
            "sourceProvenance": None,
            "degraded": [],
        }

        decision = consumer.consume(packet)
        self.assertFalse(decision["safeToClaim"])
        self.assertEqual(decision["candidateId"], "bd-malformed.1")
        self.assertEqual(decision["legacyCommandStringsRefused"], 4)
        self.assertEqual(decision["argvActions"], [])
        self.assertEqual(decision["sourceSummary"]["sourceCount"], 0)
        self.assertIn("malformed_command_surface", decision["whyNotSafe"])

    def test_malformed_nested_authority_objects_downgrade_otherwise_safe_packet(self):
        packet = {
            "schema": "ee.swarm.work_packet.v1",
            "safeToClaim": True,
            "recommendedAction": "not-a-map",
            "candidates": [
                {
                    "id": "bd-safe-looking.1",
                    "decision": "safe_to_claim",
                    "unsafeReasons": [],
                    "staleReasons": [],
                }
            ],
            "coordination": {"agentMail": "not-a-map"},
            "trackerIntegrity": "not-a-map",
            "rchProofPosture": "not-a-map",
            "verification": "not-a-map",
            "sourceProvenance": "not-a-list",
            "degraded": [],
        }

        decision = consumer.consume(packet)
        self.assertFalse(decision["safeToClaim"])
        self.assertEqual(decision["candidateId"], "bd-safe-looking.1")
        self.assertEqual(decision["decision"], "safe_to_claim")
        self.assertEqual(decision["action"], "blocked_no_action")
        self.assertEqual(decision["sourceSummary"]["sourceCount"], 0)
        self.assertIn("malformed_recommended_action", decision["whyNotSafe"])
        self.assertIn("malformed_tracker_integrity", decision["whyNotSafe"])
        self.assertIn("malformed_agent_mail", decision["whyNotSafe"])
        self.assertIn("malformed_rch_proof_posture", decision["whyNotSafe"])
        self.assertIn("malformed_verification", decision["whyNotSafe"])

    def test_malformed_authority_scalar_fields_downgrade_otherwise_safe_packet(self):
        packet = {
            "schema": "ee.swarm.work_packet.v1",
            "safeToClaim": True,
            "recommendedAction": {
                "action": "inspect_and_claim",
                "candidateId": "bd-safe-looking.2",
                "safeToClaim": True,
                "suggestedCommands": [],
                "suggestedCommandActions": [
                    safe_action(
                        "bead_claim_candidate",
                        [
                            "br",
                            "update",
                            "bd-safe-looking.2",
                            "--status",
                            "in_progress",
                            "--json",
                        ],
                        mutates=True,
                    )
                ],
            },
            "candidates": [
                {
                    "id": "bd-safe-looking.2",
                    "decision": "safe_to_claim",
                    "unsafeReasons": [],
                    "staleReasons": [],
                }
            ],
            "coordination": {
                "agentMail": {
                    "status": "healthy",
                    "reservationAuthoritative": "true",
                    "inboxAuthoritative": [],
                }
            },
            "trackerIntegrity": {
                "health": "ok",
                "brReadsAuthoritative": "true",
                "requiresCandidateDowngrade": "false",
            },
            "rchProofPosture": {
                "remoteOnlyRequired": "true",
                "safeToLaunchCargoVerification": "true",
                "blockerCodes": [],
            },
            "verification": {
                "remoteOnlyRequired": "false",
                "remoteOnlySafe": "true",
            },
            "sourceProvenance": [{"code": "beads_ready"}],
            "degraded": [],
        }

        decision = consumer.consume(packet)

        self.assertFalse(decision["safeToClaim"])
        self.assertTrue(decision["mutatingActionsRequireHuman"])
        self.assertIsNone(decision["sourceSummary"]["trackerAuthoritative"])
        self.assertIsNone(decision["sourceSummary"]["requiresCandidateDowngrade"])
        self.assertIsNone(decision["sourceSummary"]["reservationAuthoritative"])
        self.assertIsNone(decision["sourceSummary"]["inboxAuthoritative"])
        self.assertIsNone(
            decision["sourceSummary"]["rchSafeToLaunchCargoVerification"]
        )
        self.assertEqual(decision["sourceSummary"]["sourceCount"], 1)
        for reason in [
            "malformed_tracker_br_reads_authoritative",
            "malformed_tracker_requires_candidate_downgrade",
            "malformed_agent_mail_reservation_authoritative",
            "malformed_agent_mail_inbox_authoritative",
            "malformed_rch_remote_only_required",
            "malformed_rch_safe_to_launch_cargo_verification",
            "malformed_verification_remote_only_required",
            "malformed_verification_remote_only_safe",
        ]:
            self.assertIn(reason, decision["whyNotSafe"])
        claim = decision["argvActions"][0]
        self.assertFalse(claim["runnable"])
        self.assertEqual(claim["reason"], "mutating_action_requires_safe_gate")

    def test_secret_shaped_strings_are_redacted_and_not_runnable(self):
        gate = safe_gate()
        github_classic = "ghp_" + "0123456789abcdef0123456789abcdef0123"
        github_fine_grained = (
            "github_" + "pat_" + "0123456789abcdefghijklmnopqrstuvwxyz0123456789"
        )
        openai_key = "sk-" + "proj-" + "abcdefghijklmnopqrstuvwxyz0123456789"
        aws_access_key = "AKIA" + "0123456789ABCDEF"
        slack_token = "xox" + "b-" + "123456789012-abcdefghijklmnopqrstuvwxyz"
        api_key_env = "API_" + "KEY=abcdefghijklmnopqrstuvwxyz0123456789"
        gate["nextCommandActions"] = [
            safe_action(
                "leaky",
                [
                    "ee",
                    "show",
                    github_classic,
                    github_fine_grained,
                    openai_key,
                    aws_access_key,
                    slack_token,
                    api_key_env,
                    "stdout: raw command output",
                ],
            )
        ]

        decision = consumer.consume(envelope(gate))
        serialized = json.dumps(decision)
        self.assertNotIn("ghp_", serialized)
        self.assertNotIn("github_" + "pat_", serialized)
        self.assertNotIn("sk-" + "proj-", serialized)
        self.assertNotIn(aws_access_key, serialized)
        self.assertNotIn("xox" + "b-", serialized)
        self.assertNotIn("API_" + "KEY=", serialized)
        self.assertNotIn("stdout:", serialized)
        action = decision["argvActions"][0]
        self.assertFalse(action["runnable"])
        self.assertTrue(action["reviewRequired"])
        self.assertEqual(action["argv"], [])
        self.assertEqual(action["reason"], "malformed_command_action")
        self.assertIn(
            "malformed_claim_gate_next_command_action_display_command",
            decision["whyNotSafe"],
        )
        self.assertIn(
            "malformed_claim_gate_next_command_action_argv",
            decision["whyNotSafe"],
        )


class ErrorHandling(unittest.TestCase):
    def test_cli_healthy_fixture_exits_zero_with_machine_readable_decision(self):
        result, decision = run_consumer_cli(
            load_fixture("tests/fixtures/swarm_work_packet/healthy_small.json")
        )

        self.assertEqual(result.returncode, 0)
        self.assertEqual(result.stderr, "")
        self.assertTrue(decision["safeToClaim"])
        self.assertEqual(decision["sourceSchema"], "ee.swarm.work_packet.v1")
        self.assertEqual(decision["decision"], "safe_to_claim")
        self.assertEqual(decision["action"], "inspect_and_claim")
        self.assertEqual(decision["whyNotSafe"], [])

    def test_cli_blocked_fixture_exits_three_with_non_runnable_claim_posture(self):
        result, decision = run_consumer_cli(
            load_fixture("tests/fixtures/swarm_work_packet/crowded_checkout.json")
        )

        self.assertEqual(result.returncode, 3)
        self.assertEqual(result.stderr, "")
        self.assertFalse(decision["safeToClaim"])
        self.assertEqual(decision["sourceSchema"], "ee.swarm.work_packet.v1")
        self.assertEqual(decision["decision"], "coordinate_first")
        self.assertEqual(decision["action"], "coordinate_before_claim")
        self.assertIn("dirty_path_overlap", decision["whyNotSafe"])
        self.assertFalse(
            any(
                action["runnable"] and action["mutatesState"]
                for action in decision["argvActions"]
            )
        )

    def test_cli_safe_claim_gate_exits_zero_with_runnable_claim_action(self):
        result, decision = run_consumer_cli(envelope(safe_gate()))

        self.assertEqual(result.returncode, 0)
        self.assertEqual(result.stderr, "")
        self.assertTrue(decision["safeToClaim"])
        self.assertEqual(decision["sourceSchema"], "ee.swarm.work_packet.claim_gate.v1")
        self.assertEqual(decision["candidateId"], "bd-safe.1")
        claim = [a for a in decision["argvActions"] if a["actionKind"] == "claim"][0]
        self.assertTrue(claim["runnable"])
        self.assertFalse(claim["reviewRequired"])
        self.assertEqual(
            claim["argv"],
            ["br", "update", "bd-safe.1", "--status", "in_progress", "--json"],
        )

    def test_cli_redacts_secret_shaped_action_metadata(self):
        gate = safe_gate()
        token = "ghp_" + "fedcba9876543210fedcba9876543210fedc"
        home_path = "/Users/jemanuel/private/project"
        gate["nextCommandActions"] = [
            {
                "commandId": token,
                "displayCommand": "ee inspect metadata",
                "argv": ["ee", "inspect", "metadata"],
                "shellRequired": False,
                "copySafety": f"copy:{home_path}",
                "mutatesState": False,
                "requiredSubstrate": f"static:{home_path}",
                "when": f"after Bearer {token}",
                "rationale": "fixture action",
            }
        ]

        result, decision = run_consumer_cli(envelope(gate))
        serialized = json.dumps(decision)

        self.assertEqual(result.returncode, 3)
        self.assertEqual(result.stderr, "")
        self.assertFalse(decision["safeToClaim"])
        self.assertIn(
            "malformed_claim_gate_next_command_action_command_id",
            decision["whyNotSafe"],
        )
        self.assertIn(
            "malformed_claim_gate_next_command_action_copy_safety",
            decision["whyNotSafe"],
        )
        self.assertNotIn("ghp_", result.stdout)
        self.assertNotIn("/Users/", result.stdout)
        self.assertNotIn("ghp_", serialized)
        self.assertNotIn("/Users/", serialized)
        action = [a for a in decision["argvActions"] if a["actionKind"] == "inspection"][0]
        self.assertFalse(action["runnable"])
        self.assertTrue(action["reviewRequired"])
        self.assertEqual(action["commandId"], "[redacted]")
        self.assertEqual(action["requiredSubstrate"], "static:[redacted]")
        self.assertEqual(action["when"], "after Bearer [redacted]")
        self.assertEqual(action["copySafety"], "copy:[redacted]")
        self.assertEqual(action["reason"], "malformed_command_action")

    def test_cli_redacts_secret_shaped_source_summary_fields(self):
        gate = safe_gate()
        token = "ghp_" + "8899aabbccddeeff00112233445566778899"
        home_path = "/Users/jemanuel/private/project"
        gate["sourceAuthority"]["trackerHealth"] = f"ok:{home_path}"
        gate["sourceAuthority"]["agentMailStatus"] = f"healthy:{token}"

        result, decision = run_consumer_cli(envelope(gate))
        serialized = json.dumps(decision)

        self.assertEqual(result.returncode, 0)
        self.assertEqual(result.stderr, "")
        self.assertTrue(decision["safeToClaim"])
        self.assertNotIn("ghp_", result.stdout)
        self.assertNotIn("/Users/", result.stdout)
        self.assertNotIn("ghp_", serialized)
        self.assertNotIn("/Users/", serialized)
        self.assertEqual(decision["sourceSummary"]["trackerHealth"], "ok:[redacted]")
        self.assertEqual(
            decision["sourceSummary"]["agentMailStatus"],
            "healthy:[redacted]",
        )

    def test_cli_from_stdin_flag_keeps_machine_readable_decision(self):
        result, decision = run_consumer_cli(envelope(safe_gate()), ["--from-stdin"])

        self.assertEqual(result.returncode, 0)
        self.assertEqual(result.stderr, "")
        self.assertTrue(decision["safeToClaim"])
        self.assertEqual(decision["sourceSchema"], "ee.swarm.work_packet.claim_gate.v1")

    def test_cli_pretty_flag_emits_parseable_indented_json(self):
        result, decision = run_consumer_cli(
            load_fixture("tests/fixtures/swarm_work_packet/crowded_checkout.json"),
            ["--pretty"],
        )

        self.assertEqual(result.returncode, 3)
        self.assertEqual(result.stderr, "")
        self.assertFalse(decision["safeToClaim"])
        self.assertIn("{\n", result.stdout)
        self.assertIn('  "action": "coordinate_before_claim"', result.stdout)

    def test_cli_inconsistent_claim_gate_exits_three_and_blocks_claim_action(self):
        gate = safe_gate()
        gate["recommendedSafeToClaim"] = False

        result, decision = run_consumer_cli(envelope(gate))

        self.assertEqual(result.returncode, 3)
        self.assertEqual(result.stderr, "")
        self.assertFalse(decision["safeToClaim"])
        self.assertEqual(decision["sourceSchema"], "ee.swarm.work_packet.claim_gate.v1")
        self.assertIn("claim_gate_recommended_not_safe", decision["whyNotSafe"])
        claim = [a for a in decision["argvActions"] if a["actionKind"] == "claim"][0]
        self.assertFalse(claim["runnable"])
        self.assertTrue(claim["reviewRequired"])
        self.assertEqual(claim["reason"], "mutating_action_requires_safe_gate")

    def test_cli_invalid_json_returns_machine_readable_fail_closed_decision(self):
        result = subprocess.run(
            [sys.executable, str(SCRIPT_DIR / "agent_consume_work_packet_gate.py")],
            input='{"schema": "ee.response.v2", "success": true,',
            text=True,
            capture_output=True,
            check=False,
        )

        self.assertEqual(result.returncode, 2)
        self.assertEqual(result.stderr, "")
        decision = json.loads(result.stdout)
        self.assertFalse(decision["safeToClaim"])
        self.assertEqual(decision["decision"], "error")
        self.assertEqual(decision["action"], "blocked_no_action")
        self.assertEqual(decision["argvActions"], [])
        self.assertFalse(decision["mutatingActionsRequireHuman"])
        self.assertIn("error:invalid_json", decision["whyNotSafe"])

    def test_cli_noisy_error_output_uses_last_machine_envelope(self):
        noisy_log = {
            "timestamp": "2026-06-04T11:09:01Z",
            "level": "WARN",
            "fields": {
                "message": "emitting error envelope",
                "schema": "ee.error.v2",
                "code": "usage",
            },
            "target": "ee::output::error",
        }
        error_envelope = {
            "schema": "ee.error.v2",
            "error": {
                "code": "usage",
                "message": "unexpected argument '--claim-gate' found",
                "details": {},
            },
        }
        result = subprocess.run(
            [sys.executable, str(SCRIPT_DIR / "agent_consume_work_packet_gate.py")],
            input=json.dumps(noisy_log) + "\n" + json.dumps(error_envelope) + "\n",
            text=True,
            capture_output=True,
            check=False,
        )

        self.assertEqual(result.returncode, 2)
        self.assertEqual(result.stderr, "")
        decision = json.loads(result.stdout)
        self.assertFalse(decision["safeToClaim"])
        self.assertEqual(decision["decision"], "error")
        self.assertIn("error:stale_claim_gate_binary", decision["whyNotSafe"])
        self.assertNotIn("error:invalid_json", decision["whyNotSafe"])

    def test_cli_line_mode_uses_last_machine_envelope_over_earlier_safe_packet(self):
        safe_envelope = envelope(safe_gate())
        noisy_log = {"schema": "log.event.v1", "message": "interleaved diagnostic"}
        stale_error = {
            "schema": "ee.error.v2",
            "error": {
                "code": "usage",
                "message": "unexpected argument '--candidate' found",
            },
        }

        result = subprocess.run(
            [sys.executable, str(SCRIPT_DIR / "agent_consume_work_packet_gate.py")],
            input="\n".join(
                [
                    json.dumps(safe_envelope),
                    json.dumps(noisy_log),
                    json.dumps(stale_error),
                ]
            ),
            text=True,
            capture_output=True,
            check=False,
        )

        self.assertEqual(result.returncode, 2)
        self.assertEqual(result.stderr, "")
        decision = json.loads(result.stdout)
        self.assertFalse(decision["safeToClaim"])
        self.assertEqual(decision["decision"], "error")
        self.assertEqual(decision["argvActions"], [])
        self.assertIn("error:stale_claim_gate_binary", decision["whyNotSafe"])

    def test_error_envelope_returns_machine_readable_block(self):
        decision = consumer.consume(
            {
                "schema": "ee.error.v2",
                "success": False,
                "error": {"code": "migration_required"},
            }
        )
        self.assertFalse(decision["safeToClaim"])
        self.assertEqual(decision["decision"], "error")
        self.assertIn("error:migration_required", decision["whyNotSafe"])

    def test_install_check_stale_report_fails_closed_with_findings(self):
        decision = consumer.consume(envelope(install_check_report()))

        self.assertFalse(decision["safeToClaim"])
        self.assertEqual(decision["sourceSchema"], "ee.install.check.v1")
        self.assertEqual(decision["decision"], "install_freshness_stale")
        self.assertEqual(decision["action"], "repair_install_freshness")
        self.assertEqual(decision["argvActions"], [])
        self.assertFalse(decision["mutatingActionsRequireHuman"])
        self.assertIn("install_freshness:stale", decision["whyNotSafe"])
        self.assertIn("install_check_is_not_claim_gate", decision["whyNotSafe"])
        self.assertIn(
            "install_finding:installed_binary_stale", decision["whyNotSafe"]
        )
        self.assertIn("install_finding:duplicate_path_binary", decision["whyNotSafe"])
        self.assertIn(
            "install_finding:path_binary_version_mismatch", decision["whyNotSafe"]
        )
        self.assertTrue(
            any(reason.startswith("install_repair:") for reason in decision["whyNotSafe"])
        )
        self.assertEqual(decision["sourceSummary"]["sourceCount"], 1)

    def test_install_check_golden_fixtures_fail_closed(self):
        cases = [
            (
                "tests/fixtures/golden/install/duplicate_path_check.json.golden",
                "install_freshness_shadowed_binary",
                [
                    "install_freshness:shadowed_binary",
                    "install_finding:current_binary_shadowed",
                    "install_finding:duplicate_path_binary",
                ],
            ),
            (
                "tests/fixtures/golden/install/permission_denied_check.json.golden",
                "install_freshness_path_binary_missing",
                [
                    "install_freshness:path_binary_missing",
                    "install_finding:binary_not_on_path",
                    "install_finding:install_dir_not_writable",
                ],
            ),
        ]

        for relative_path, decision_name, required_reasons in cases:
            with self.subTest(relative_path=relative_path):
                decision = consumer.consume(load_fixture(relative_path))

                self.assertFalse(decision["safeToClaim"])
                self.assertEqual(decision["sourceSchema"], "ee.install.check.v1")
                self.assertEqual(decision["decision"], decision_name)
                self.assertEqual(decision["action"], "repair_install_freshness")
                self.assertEqual(decision["argvActions"], [])
                self.assertFalse(decision["mutatingActionsRequireHuman"])
                self.assertIn(
                    "install_check_is_not_claim_gate", decision["whyNotSafe"]
                )
                for reason in required_reasons:
                    self.assertIn(reason, decision["whyNotSafe"])
                self.assertTrue(
                    any(
                        reason.startswith("install_repair:")
                        for reason in decision["whyNotSafe"]
                    )
                )

    def test_install_check_missing_freshness_fails_closed(self):
        report = install_check_report()
        report.pop("freshness")

        decision = consumer.consume(envelope(report))

        self.assertFalse(decision["safeToClaim"])
        self.assertEqual(decision["sourceSchema"], "ee.install.check.v1")
        self.assertEqual(decision["decision"], "install_freshness_missing_freshness")
        self.assertIn("install_freshness:missing_freshness", decision["whyNotSafe"])
        self.assertIn("install_check_is_not_claim_gate", decision["whyNotSafe"])

    def test_install_check_fresh_report_still_requires_claim_gate(self):
        decision = consumer.consume(
            envelope(
                install_check_report(
                    verdict="fresh",
                    blocking_findings=[],
                    findings=[{"code": "offline_no_manifest", "severity": "info"}],
                )
            )
        )

        self.assertFalse(decision["safeToClaim"])
        self.assertEqual(decision["decision"], "install_freshness_fresh")
        self.assertEqual(decision["action"], "run_claim_gate_after_fresh_install_check")
        self.assertIn("install_check_is_not_claim_gate", decision["whyNotSafe"])
        self.assertNotIn("install_freshness:fresh", decision["whyNotSafe"])

    def test_cli_line_mode_accepts_raw_install_check_payload(self):
        noisy_log = {"schema": "log.event.v1", "message": "interleaved diagnostic"}
        report = install_check_report()

        result = subprocess.run(
            [sys.executable, str(SCRIPT_DIR / "agent_consume_work_packet_gate.py")],
            input=json.dumps(noisy_log) + "\n" + json.dumps(report) + "\n",
            text=True,
            capture_output=True,
            check=False,
        )

        self.assertEqual(result.returncode, 3)
        self.assertEqual(result.stderr, "")
        decision = json.loads(result.stdout)
        self.assertFalse(decision["safeToClaim"])
        self.assertEqual(decision["sourceSchema"], "ee.install.check.v1")
        self.assertIn("install_freshness:stale", decision["whyNotSafe"])

    def test_error_envelope_preserves_degraded_summary(self):
        decision = consumer.consume(
            {
                "schema": "ee.error.v2",
                "error": {
                    "code": "build_admission_refused",
                    "details": {
                        "degraded": [
                            {
                                "code": "rch_verify_build_admission_denied",
                                "source": "rch",
                                "severity": "high",
                            }
                        ],
                        "sourceProvenance": [
                            {
                                "code": "rch_worker_topology_blocked",
                                "source": "rch",
                            }
                        ],
                    },
                },
                "degraded": [
                    {
                        "code": "agent_mail_unavailable",
                        "source": "agent_mail",
                        "severity": "warning",
                    }
                ],
            }
        )

        self.assertFalse(decision["safeToClaim"])
        self.assertEqual(decision["decision"], "error")
        self.assertIn("error:build_admission_refused", decision["whyNotSafe"])
        codes = {entry["code"] for entry in decision["degradedSummary"]}
        self.assertIn("agent_mail_unavailable", codes)
        self.assertIn("rch_verify_build_admission_denied", codes)
        self.assertIn("rch_worker_topology_blocked", codes)

    def test_error_envelope_degraded_summary_is_bounded(self):
        decision = consumer.consume(
            {
                "schema": "ee.error.v2",
                "error": {
                    "code": "usage",
                    "degraded": [
                        {
                            "code": f"code_{index}",
                            "source": "ee",
                            "severity": "low",
                        }
                        for index in range(32)
                    ],
                },
            }
        )

        self.assertFalse(decision["safeToClaim"])
        self.assertEqual(
            len(decision["degradedSummary"]),
            consumer.DECISION_DIAGNOSTIC_LIMIT,
        )
        self.assertEqual(decision["degradedSummary"][0]["code"], "code_0")
        self.assertEqual(
            decision["degradedSummary"][-1]["code"],
            f"code_{consumer.DECISION_DIAGNOSTIC_LIMIT - 1}",
        )

    def test_cli_error_degraded_summary_redacts_secret_shaped_fields(self):
        token = "ghp_" + "00112233445566778899aabbccddeeff0011"
        fine_grained = "github_" + "pat_" + "00112233445566778899aabbccddeeff"
        home_path = "/Users/jemanuel/private/project"
        payload = {
            "schema": "ee.error.v2",
            "error": {
                "code": "build_admission_refused",
                "details": {
                    "degraded": [
                        {
                            "code": f"blocked:{token}",
                            "source": home_path,
                            "severity": f"Bearer {token}",
                        }
                    ],
                    "sourceProvenance": [
                        {
                            "code": f"from:{home_path}",
                            "source": fine_grained,
                            "severity": "warning",
                        }
                    ],
                },
            },
            "degraded": [
                {
                    "code": f"envelope:{home_path}",
                    "source": "agent_mail",
                    "severity": "high",
                }
            ],
        }

        result, decision = run_consumer_cli(payload)
        serialized = json.dumps(decision)

        self.assertEqual(result.returncode, 2)
        self.assertEqual(result.stderr, "")
        self.assertFalse(decision["safeToClaim"])
        self.assertEqual(decision["decision"], "error")
        self.assertNotIn("ghp_", result.stdout)
        self.assertNotIn("github_" + "pat_", result.stdout)
        self.assertNotIn("/Users/", result.stdout)
        self.assertNotIn("ghp_", serialized)
        self.assertNotIn("github_" + "pat_", serialized)
        self.assertNotIn("/Users/", serialized)
        self.assertIn(
            {
                "code": "blocked:[redacted]",
                "source": "[redacted]",
                "severity": "Bearer [redacted]",
            },
            decision["degradedSummary"],
        )
        self.assertIn(
            {
                "code": "from:[redacted]",
                "source": "[redacted]",
                "severity": "warning",
            },
            decision["degradedSummary"],
        )

    def test_non_object_input_fails_closed(self):
        decision = consumer.consume(["not", "a", "response"])

        self.assertFalse(decision["safeToClaim"])
        self.assertEqual(decision["decision"], "error")
        self.assertEqual(decision["argvActions"], [])
        self.assertIn("error:invalid_json_shape", decision["whyNotSafe"])

    def test_success_envelope_without_object_payload_fails_closed(self):
        decision = consumer.consume(
            {
                "schema": "ee.response.v2",
                "success": True,
                "data": ["not", "a", "payload"],
            }
        )

        self.assertFalse(decision["safeToClaim"])
        self.assertEqual(decision["decision"], "error")
        self.assertEqual(decision["argvActions"], [])
        self.assertIn("error:missing_payload", decision["whyNotSafe"])

    def test_unsupported_payload_schema_fails_closed(self):
        decision = consumer.consume(
            {
                "schema": "ee.response.v2",
                "success": True,
                "data": {
                    "schema": "ee.swarm.brief.v1",
                    "recommendedAction": {
                        "candidateId": "bd-unsafe.1",
                        "safeToClaim": True,
                    },
                },
            }
        )

        self.assertFalse(decision["safeToClaim"])
        self.assertEqual(decision["decision"], "error")
        self.assertEqual(decision["argvActions"], [])
        self.assertIn(
            "error:unsupported_schema:ee.swarm.brief.v1",
            decision["whyNotSafe"],
        )

    def test_unsupported_payload_schema_redacts_secret_shaped_schema_name(self):
        token = "ghp_" + "0123456789abcdef0123456789abcdef0123"
        home_path = "/Users/jemanuel/private/project"

        decision = consumer.consume(
            {
                "schema": "ee.response.v2",
                "success": True,
                "data": {
                    "schema": f"ee.unexpected.{token}.{home_path}",
                    "recommendedAction": {
                        "candidateId": "bd-unsafe.1",
                        "safeToClaim": True,
                    },
                },
            }
        )

        serialized = json.dumps(decision)
        self.assertFalse(decision["safeToClaim"])
        self.assertEqual(decision["decision"], "error")
        self.assertEqual(decision["argvActions"], [])
        self.assertNotIn("ghp_", serialized)
        self.assertNotIn("/Users/", serialized)
        self.assertIn(
            "error:unsupported_schema:ee.unexpected.[redacted].[redacted]",
            decision["whyNotSafe"],
        )

    def test_cli_unsupported_payload_schema_redacts_secret_shaped_schema_name(self):
        token = "ghp_" + "abcdef0123456789abcdef0123456789abcd"
        home_path = "/home/agent/private/project"
        payload = {
            "schema": "ee.response.v2",
            "success": True,
            "data": {
                "schema": f"ee.unexpected.{token}.{home_path}",
                "recommendedAction": {
                    "candidateId": "bd-unsafe.1",
                    "safeToClaim": True,
                },
            },
        }

        result, decision = run_consumer_cli(payload)
        serialized = json.dumps(decision)

        self.assertEqual(result.returncode, 2)
        self.assertEqual(result.stderr, "")
        self.assertFalse(decision["safeToClaim"])
        self.assertEqual(decision["decision"], "error")
        self.assertEqual(decision["argvActions"], [])
        self.assertNotIn("ghp_", result.stdout)
        self.assertNotIn("/home/", result.stdout)
        self.assertNotIn("ghp_", serialized)
        self.assertNotIn("/home/", serialized)
        self.assertIn(
            "error:unsupported_schema:ee.unexpected.[redacted].[redacted]",
            decision["whyNotSafe"],
        )

    def test_stale_claim_gate_binary_error_fails_closed(self):
        decision = consumer.consume(
            {
                "schema": "ee.error.v2",
                "success": False,
                "error": {
                    "code": "usage_error",
                    "message": "unexpected argument '--claim-gate' found",
                    "details": {
                        "invocation": [
                            "ee",
                            "swarm",
                            "work-packet",
                            "--claim-gate",
                            "--candidate",
                            "bd-safe.1",
                            "--json",
                        ]
                    },
                },
            }
        )

        self.assertFalse(decision["safeToClaim"])
        self.assertEqual(decision["decision"], "error")
        self.assertEqual(decision["action"], "blocked_no_action")
        self.assertEqual(decision["argvActions"], [])
        self.assertFalse(decision["mutatingActionsRequireHuman"])
        self.assertIn("error:stale_claim_gate_binary", decision["whyNotSafe"])

    def test_stale_claim_gate_binary_detection_requires_exact_rejected_argument(self):
        decision = consumer.consume(
            {
                "schema": "ee.error.v2",
                "error": {
                    "code": "usage",
                    "message": "unexpected argument '--candidate-pool' found",
                    "details": {
                        "invocation": [
                            "ee",
                            "pack",
                            "task",
                            "--candidate-pool",
                            "250",
                        ]
                    },
                },
            }
        )

        self.assertFalse(decision["safeToClaim"])
        self.assertEqual(decision["decision"], "error")
        self.assertEqual(decision["argvActions"], [])
        self.assertIn("error:usage", decision["whyNotSafe"])
        self.assertNotIn("error:stale_claim_gate_binary", decision["whyNotSafe"])

    def test_stale_claim_gate_binary_detection_respects_invocation_surface(self):
        decision = consumer.consume(
            {
                "schema": "ee.error.v2",
                "error": {
                    "code": "usage",
                    "message": "unexpected argument '--candidate' found",
                    "details": {
                        "invocation": [
                            "ee",
                            "perf",
                            "compare",
                            "--candidate",
                            "candidate.json",
                        ]
                    },
                },
            }
        )

        self.assertFalse(decision["safeToClaim"])
        self.assertEqual(decision["decision"], "error")
        self.assertEqual(decision["argvActions"], [])
        self.assertIn("error:usage", decision["whyNotSafe"])
        self.assertNotIn("error:stale_claim_gate_binary", decision["whyNotSafe"])

    def test_stale_claim_gate_binary_detection_respects_string_invocation_surface(self):
        decision = consumer.consume(
            {
                "schema": "ee.error.v2",
                "error": {
                    "code": "usage",
                    "message": "unexpected argument '--candidate' found",
                    "details": {
                        "invocation": (
                            "ee perf compare --candidate candidate.json"
                        )
                    },
                },
            }
        )

        self.assertFalse(decision["safeToClaim"])
        self.assertEqual(decision["decision"], "error")
        self.assertEqual(decision["argvActions"], [])
        self.assertIn("error:usage", decision["whyNotSafe"])
        self.assertNotIn("error:stale_claim_gate_binary", decision["whyNotSafe"])

    def test_stale_claim_gate_binary_detection_skips_global_flags_for_surface(self):
        decision = consumer.consume(
            {
                "schema": "ee.error.v2",
                "error": {
                    "code": "usage",
                    "message": "unexpected argument '--candidate' found",
                    "details": {
                        "invocation": [
                            "ee",
                            "--workspace",
                            ".",
                            "swarm",
                            "work-packet",
                            "--candidate",
                            "bd-safe.1",
                        ]
                    },
                },
            }
        )

        self.assertFalse(decision["safeToClaim"])
        self.assertEqual(decision["decision"], "error")
        self.assertEqual(decision["argvActions"], [])
        self.assertIn("error:stale_claim_gate_binary", decision["whyNotSafe"])

    def test_stale_claim_gate_binary_detection_accepts_string_work_packet_invocation(self):
        decision = consumer.consume(
            {
                "schema": "ee.error.v2",
                "error": {
                    "code": "usage",
                    "message": "unexpected argument '--candidate' found",
                    "details": {
                        "invocation": (
                            "ee --workspace . swarm work-packet --candidate bd-safe.1"
                        )
                    },
                },
            }
        )

        self.assertFalse(decision["safeToClaim"])
        self.assertEqual(decision["decision"], "error")
        self.assertEqual(decision["argvActions"], [])
        self.assertIn("error:stale_claim_gate_binary", decision["whyNotSafe"])

    def test_stale_claim_gate_binary_detection_accepts_quoted_string_global_flag_value(self):
        decision = consumer.consume(
            {
                "schema": "ee.error.v2",
                "error": {
                    "code": "usage",
                    "message": "unexpected argument '--candidate' found",
                    "details": {
                        "invocation": (
                            'ee --workspace "/tmp/project root" swarm work-packet '
                            "--candidate bd-safe.1"
                        )
                    },
                },
            }
        )

        self.assertFalse(decision["safeToClaim"])
        self.assertEqual(decision["decision"], "error")
        self.assertEqual(decision["argvActions"], [])
        self.assertIn("error:stale_claim_gate_binary", decision["whyNotSafe"])

    def test_stale_claim_gate_binary_detection_malformed_string_invocation_does_not_force_surface(self):
        decision = consumer.consume(
            {
                "schema": "ee.error.v2",
                "error": {
                    "code": "usage",
                    "message": "unexpected argument '--candidate' found",
                    "details": {
                        "invocation": (
                            'ee --workspace "/tmp/project root swarm work-packet '
                            "--candidate bd-safe.1"
                        )
                    },
                },
            }
        )

        self.assertFalse(decision["safeToClaim"])
        self.assertEqual(decision["decision"], "error")
        self.assertEqual(decision["argvActions"], [])
        self.assertIn("error:usage", decision["whyNotSafe"])
        self.assertNotIn("error:stale_claim_gate_binary", decision["whyNotSafe"])

    def test_stale_claim_gate_binary_detection_accepts_equals_global_flags(self):
        decision = consumer.consume(
            {
                "schema": "ee.error.v2",
                "error": {
                    "code": "usage",
                    "message": "unexpected argument '--candidate' found",
                    "details": {
                        "invocation": [
                            "ee",
                            "--workspace=.",
                            "--format=json",
                            "swarm",
                            "work-packet",
                            "--candidate",
                            "bd-safe.1",
                        ]
                    },
                },
            }
        )

        self.assertFalse(decision["safeToClaim"])
        self.assertEqual(decision["decision"], "error")
        self.assertEqual(decision["argvActions"], [])
        self.assertIn("error:stale_claim_gate_binary", decision["whyNotSafe"])

    def test_stale_claim_gate_binary_detection_global_flags_do_not_force_surface(self):
        decision = consumer.consume(
            {
                "schema": "ee.error.v2",
                "error": {
                    "code": "usage",
                    "message": "unexpected argument '--candidate' found",
                    "details": {
                        "invocation": [
                            "ee",
                            "--workspace",
                            ".",
                            "perf",
                            "compare",
                            "--candidate",
                            "candidate.json",
                        ]
                    },
                },
            }
        )

        self.assertFalse(decision["safeToClaim"])
        self.assertEqual(decision["decision"], "error")
        self.assertEqual(decision["argvActions"], [])
        self.assertIn("error:usage", decision["whyNotSafe"])
        self.assertNotIn("error:stale_claim_gate_binary", decision["whyNotSafe"])

    def test_stale_claim_gate_binary_detection_help_path_does_not_force_surface(self):
        decision = consumer.consume(
            {
                "schema": "ee.error.v2",
                "error": {
                    "code": "usage",
                    "message": "unexpected argument '--candidate' found",
                    "details": {
                        "invocation": [
                            "ee",
                            "help",
                            "swarm",
                            "work-packet",
                            "--candidate",
                            "bd-safe.1",
                        ]
                    },
                },
            }
        )

        self.assertFalse(decision["safeToClaim"])
        self.assertEqual(decision["decision"], "error")
        self.assertEqual(decision["argvActions"], [])
        self.assertIn("error:usage", decision["whyNotSafe"])
        self.assertNotIn("error:stale_claim_gate_binary", decision["whyNotSafe"])

    def test_stale_claim_gate_binary_detection_accepts_work_packet_invocation(self):
        decision = consumer.consume(
            {
                "schema": "ee.error.v2",
                "error": {
                    "code": "usage",
                    "message": "unexpected argument '--candidate' found",
                    "details": {
                        "invocation": [
                            "/Users/jemanuel/.cargo/bin/ee",
                            "swarm",
                            "work-packet",
                            "--candidate",
                            "bd-safe.1",
                        ]
                    },
                },
            }
        )

        self.assertFalse(decision["safeToClaim"])
        self.assertEqual(decision["decision"], "error")
        self.assertEqual(decision["argvActions"], [])
        self.assertIn("error:stale_claim_gate_binary", decision["whyNotSafe"])

    def test_stale_claim_gate_binary_detection_accepts_backtick_quotes(self):
        decision = consumer.consume(
            {
                "schema": "ee.error.v2",
                "error": {
                    "code": "usage",
                    "message": "unexpected argument `--candidate` found",
                },
            }
        )

        self.assertFalse(decision["safeToClaim"])
        self.assertEqual(decision["decision"], "error")
        self.assertEqual(decision["argvActions"], [])
        self.assertIn("error:stale_claim_gate_binary", decision["whyNotSafe"])

    def test_root_error_envelope_stale_claim_gate_binary_fails_closed(self):
        decision = consumer.consume(
            {
                "schema": "ee.error.v2",
                "error": {
                    "code": "usage",
                    "message": "unexpected argument '--claim-gate' found",
                    "severity": "low",
                    "repair": "ee --help",
                    "repairKind": "actionable",
                    "details": {},
                },
            }
        )

        self.assertFalse(decision["safeToClaim"])
        self.assertEqual(decision["decision"], "error")
        self.assertEqual(decision["action"], "blocked_no_action")
        self.assertEqual(decision["argvActions"], [])
        self.assertFalse(decision["mutatingActionsRequireHuman"])
        self.assertIn("error:stale_claim_gate_binary", decision["whyNotSafe"])

    def test_stale_environment_attestation_binary_error_fails_closed(self):
        decision = consumer.consume(
            {
                "schema": "ee.error.v2",
                "error": {
                    "code": "usage",
                    "message": "unrecognized subcommand 'environment-attestation'",
                    "severity": "low",
                    "repair": "ee --help",
                    "repairKind": "actionable",
                    "details": {},
                },
            }
        )

        self.assertFalse(decision["safeToClaim"])
        self.assertEqual(decision["decision"], "error")
        self.assertEqual(decision["action"], "blocked_no_action")
        self.assertEqual(decision["argvActions"], [])
        self.assertFalse(decision["mutatingActionsRequireHuman"])
        self.assertIn(
            "error:stale_environment_attestation_binary",
            decision["whyNotSafe"],
        )

    def test_stale_environment_attestation_binary_detection_accepts_invocation(self):
        decision = consumer.consume(
            {
                "schema": "ee.error.v2",
                "error": {
                    "code": "usage",
                    "message": "unrecognized subcommand `environment-attestation`",
                    "details": {
                        "invocation": (
                            "ee --workspace . diag environment-attestation "
                            "--include-rch --json"
                        )
                    },
                },
            }
        )

        self.assertFalse(decision["safeToClaim"])
        self.assertEqual(decision["decision"], "error")
        self.assertEqual(decision["argvActions"], [])
        self.assertIn(
            "error:stale_environment_attestation_binary",
            decision["whyNotSafe"],
        )

    def test_stale_environment_attestation_binary_detection_respects_invocation_surface(self):
        decision = consumer.consume(
            {
                "schema": "ee.error.v2",
                "error": {
                    "code": "usage",
                    "message": "unrecognized subcommand 'environment-attestation'",
                    "details": {
                        "invocation": [
                            "ee",
                            "help",
                            "diag",
                            "environment-attestation",
                        ]
                    },
                },
            }
        )

        self.assertFalse(decision["safeToClaim"])
        self.assertEqual(decision["decision"], "error")
        self.assertEqual(decision["argvActions"], [])
        self.assertIn("error:usage", decision["whyNotSafe"])
        self.assertNotIn(
            "error:stale_environment_attestation_binary",
            decision["whyNotSafe"],
        )


class ConsumerDecisionSchemaContract(unittest.TestCase):
    def test_claim_gate_consumer_constants_match_claim_gate_schema(self):
        schema = load_fixture(
            "docs/schemas/swarm/ee.swarm.work_packet.claim_gate.v1.json"
        )
        source_authority = schema["definitions"]["sourceAuthority"]
        actionable_queue = schema["definitions"]["actionableQueueAuthority"]
        command_action = schema["definitions"]["commandAction"]

        self.assertEqual(schema["title"], consumer.CLAIM_GATE_SCHEMA)
        self.assertFalse(schema["additionalProperties"])
        self.assertEqual(
            [
                "schema",
                *[field for field, _reason in consumer.CLAIM_GATE_REQUIRED_FIELDS],
            ],
            schema["required"],
        )
        optional_claim_gate_fields = {"resourceAdmission"}
        self.assertEqual(
            set(schema["required"]) | optional_claim_gate_fields,
            set(schema["properties"]),
        )
        self.assertEqual(
            [
                field
                for field, _reason in consumer.CLAIM_GATE_SOURCE_AUTHORITY_REQUIRED_FIELDS
            ],
            source_authority["required"],
        )
        optional_source_authority_fields = {
            "environmentVerdict",
            "sourceTestVerdict",
            "remoteVerificationAdmitted",
            "localCargoFallbackObserved",
        }
        self.assertEqual(
            set(source_authority["required"]) | optional_source_authority_fields,
            set(source_authority["properties"]),
        )
        self.assertFalse(source_authority["additionalProperties"])
        self.assertEqual(
            [
                field
                for field, _suffix in consumer.CLAIM_GATE_ACTIONABLE_QUEUE_REQUIRED_FIELDS
            ],
            actionable_queue["required"],
        )
        self.assertFalse(actionable_queue["additionalProperties"])
        self.assertEqual(
            [
                field
                for field, _suffix in consumer.CLAIM_GATE_COMMAND_ACTION_REQUIRED_FIELDS
            ],
            command_action["required"],
        )
        self.assertEqual(
            consumer.COMMAND_ACTION_ALLOWED_FIELDS,
            set(command_action["properties"]),
        )
        self.assertFalse(command_action["additionalProperties"])
        self.assertEqual(
            [{"$ref": "#/definitions/safeString"}],
            command_action["properties"]["rationale"]["allOf"],
        )
        self.assertEqual(
            240,
            command_action["properties"]["rationale"]["maxLength"],
        )

    def test_command_action_consumer_constants_match_work_packet_schema(self):
        schema = load_fixture("docs/schemas/swarm/ee.swarm.work_packet.v1.json")
        command_action = schema["definitions"]["commandAction"]

        self.assertEqual(
            consumer.COPY_SAFETY_VALUES,
            set(schema["definitions"]["copySafety"]["enum"]),
        )
        self.assertEqual(
            consumer.SHELL_REQUIRED_COPY_SAFETY_VALUES,
            set(
                command_action["allOf"][0]["then"]["properties"]["copySafety"]["enum"]
            ),
        )
        self.assertEqual(
            consumer.COMMAND_SUBSTRATE_VALUES,
            set(schema["definitions"]["commandSubstrate"]["enum"]),
        )
        self.assertEqual(
            [
                field
                for field, _suffix in consumer.CLAIM_GATE_COMMAND_ACTION_REQUIRED_FIELDS
            ],
            command_action["required"],
        )
        self.assertEqual(
            consumer.COMMAND_ACTION_ALLOWED_FIELDS,
            set(command_action["properties"]),
        )
        self.assertFalse(command_action["additionalProperties"])
        self.assertEqual(
            consumer.COMMAND_ID_PATTERN.pattern,
            command_action["properties"]["commandId"]["pattern"],
        )
        self.assertEqual(
            [field for field, _suffix in consumer.COMMAND_ACTION_SAFE_STRING_FIELDS],
            ["displayCommand", "when", "rationale"],
        )
        for field, _suffix in consumer.COMMAND_ACTION_SAFE_STRING_FIELDS:
            with self.subTest(field=field):
                self.assertGreaterEqual(
                    command_action["properties"][field].get("minLength", 1),
                    1,
                )
        self.assertEqual(
            [{"$ref": "#/definitions/safeCommandString"}],
            command_action["properties"]["rationale"]["allOf"],
        )
        self.assertEqual(
            240,
            command_action["properties"]["rationale"]["maxLength"],
        )

    def test_decision_schema_string_caps_match_consumer_redaction_limits(self):
        schema = load_fixture(
            "docs/schemas/ee.agent.work_packet_gate_decision.v1.json"
        )
        action = schema["$defs"]["argvAction"]["properties"]
        source = schema["$defs"]["sourceSummary"]["properties"]
        degraded = schema["$defs"]["degradedSummaryEntry"]["properties"]

        def nullable_ref_max_length(ref):
            definition_name = ref.rsplit("/", 1)[1]
            string_branch = schema["$defs"][definition_name]["oneOf"][1]
            return string_branch["maxLength"]

        self.assertEqual(
            96,
            schema["properties"]["candidateId"]["oneOf"][1]["maxLength"],
        )
        self.assertEqual(64, schema["properties"]["decision"]["maxLength"])
        self.assertEqual(64, schema["properties"]["action"]["maxLength"])
        self.assertEqual(
            consumer.DECISION_ACTION_LIMIT,
            schema["properties"]["argvActions"]["maxItems"],
        )
        self.assertEqual(
            consumer.DECISION_ARGV_PART_LIMIT,
            action["argv"]["maxItems"],
        )
        self.assertEqual(
            160,
            schema["properties"]["whyNotSafe"]["items"]["maxLength"],
        )
        self.assertEqual(
            consumer.DECISION_DIAGNOSTIC_LIMIT,
            schema["properties"]["whyNotSafe"]["maxItems"],
        )
        self.assertEqual(
            consumer.DECISION_DIAGNOSTIC_LIMIT,
            schema["properties"]["degradedSummary"]["maxItems"],
        )

        self.assertEqual(96, action["commandId"]["maxLength"])
        self.assertEqual(120, action["argv"]["items"]["maxLength"])
        self.assertEqual(
            48,
            nullable_ref_max_length(action["requiredSubstrate"]["$ref"]),
        )
        self.assertEqual(96, nullable_ref_max_length(action["when"]["$ref"]))
        self.assertEqual(48, action["copySafety"]["maxLength"])
        self.assertEqual(64, action["reason"]["maxLength"])

        for key in ("trackerHealth", "agentMailStatus", "rchPosture"):
            with self.subTest(source_summary=key):
                self.assertEqual(
                    64,
                    nullable_ref_max_length(source[key]["$ref"]),
                )

        self.assertEqual(96, degraded["code"]["maxLength"])
        self.assertEqual(48, nullable_ref_max_length(degraded["source"]["$ref"]))
        self.assertEqual(32, nullable_ref_max_length(degraded["severity"]["$ref"]))

    def assert_decision_matches_schema_constraints(self, decision, schema):
        def assert_max_length(value, limit, path):
            if value is not None:
                self.assertLessEqual(
                    len(value),
                    limit,
                    f"{path} exceeds maxLength {limit}",
                )

        def nullable_ref_max_length(ref):
            definition_name = ref.rsplit("/", 1)[1]
            return schema["$defs"][definition_name]["oneOf"][1]["maxLength"]

        source_schema = decision["sourceSchema"]
        self.assertTrue(
            source_schema is None
            or source_schema
            in schema["properties"]["sourceSchema"]["oneOf"][1]["enum"]
        )
        self.assertIsInstance(decision["safeToClaim"], bool)
        self.assertTrue(
            decision["candidateId"] is None
            or isinstance(decision["candidateId"], str)
        )
        assert_max_length(
            decision["candidateId"],
            schema["properties"]["candidateId"]["oneOf"][1]["maxLength"],
            "candidateId",
        )
        self.assertIsInstance(decision["decision"], str)
        assert_max_length(
            decision["decision"],
            schema["properties"]["decision"]["maxLength"],
            "decision",
        )
        self.assertIsInstance(decision["action"], str)
        assert_max_length(
            decision["action"],
            schema["properties"]["action"]["maxLength"],
            "action",
        )
        self.assertIsInstance(decision["argvActions"], list)
        self.assertLessEqual(
            len(decision["argvActions"]),
            schema["properties"]["argvActions"]["maxItems"],
        )
        self.assertIsInstance(decision["mutatingActionsRequireHuman"], bool)
        self.assertIsInstance(decision["whyNotSafe"], list)
        self.assertLessEqual(
            len(decision["whyNotSafe"]),
            schema["properties"]["whyNotSafe"]["maxItems"],
        )
        self.assertTrue(
            all(isinstance(reason, str) for reason in decision["whyNotSafe"])
        )
        for index, reason in enumerate(decision["whyNotSafe"]):
            assert_max_length(
                reason,
                schema["properties"]["whyNotSafe"]["items"]["maxLength"],
                f"whyNotSafe[{index}]",
            )
        self.assertIsInstance(decision["degradedSummary"], list)
        self.assertLessEqual(
            len(decision["degradedSummary"]),
            schema["properties"]["degradedSummary"]["maxItems"],
        )
        self.assertIsInstance(decision["legacyCommandStringsRefused"], int)
        self.assertNotIsInstance(decision["legacyCommandStringsRefused"], bool)
        self.assertGreaterEqual(decision["legacyCommandStringsRefused"], 0)

        nullable_source_strings = {
            "trackerHealth",
            "agentMailStatus",
            "rchPosture",
        }
        nullable_source_bools = {
            "trackerAuthoritative",
            "requiresCandidateDowngrade",
            "reservationAuthoritative",
            "inboxAuthoritative",
            "rchRemoteOnlyRequired",
            "rchSafeToLaunchCargoVerification",
        }
        source_summary = decision["sourceSummary"]
        for key in nullable_source_strings:
            self.assertTrue(
                source_summary[key] is None or isinstance(source_summary[key], str),
                f"sourceSummary.{key} has invalid type",
            )
            assert_max_length(
                source_summary[key],
                nullable_ref_max_length(
                    schema["$defs"]["sourceSummary"]["properties"][key]["$ref"]
                ),
                f"sourceSummary.{key}",
            )
        for key in nullable_source_bools:
            self.assertTrue(
                source_summary[key] is None or isinstance(source_summary[key], bool),
                f"sourceSummary.{key} has invalid type",
            )
        source_count = source_summary["sourceCount"]
        self.assertTrue(source_count is None or isinstance(source_count, int))
        self.assertNotIsInstance(source_count, bool)
        if source_count is not None:
            self.assertGreaterEqual(source_count, 0)

        action_kind_enum = set(
            schema["$defs"]["argvAction"]["properties"]["actionKind"]["enum"]
        )
        for action in decision["argvActions"]:
            action_schema = schema["$defs"]["argvAction"]["properties"]
            self.assertIsInstance(action["commandId"], str)
            assert_max_length(
                action["commandId"],
                action_schema["commandId"]["maxLength"],
                "argvAction.commandId",
            )
            self.assertIn(action["actionKind"], action_kind_enum)
            self.assertIsInstance(action["argv"], list)
            self.assertLessEqual(
                len(action["argv"]),
                action_schema["argv"]["maxItems"],
            )
            self.assertTrue(all(isinstance(part, str) for part in action["argv"]))
            for index, part in enumerate(action["argv"]):
                assert_max_length(
                    part,
                    action_schema["argv"]["items"]["maxLength"],
                    f"argvAction.argv[{index}]",
                )
            self.assertIsInstance(action["runnable"], bool)
            self.assertIsInstance(action["reviewRequired"], bool)
            self.assertIsInstance(action["mutatesState"], bool)
            self.assertTrue(
                action["requiredSubstrate"] is None
                or isinstance(action["requiredSubstrate"], str)
            )
            assert_max_length(
                action["requiredSubstrate"],
                nullable_ref_max_length(action_schema["requiredSubstrate"]["$ref"]),
                "argvAction.requiredSubstrate",
            )
            self.assertTrue(action["when"] is None or isinstance(action["when"], str))
            assert_max_length(
                action["when"],
                nullable_ref_max_length(action_schema["when"]["$ref"]),
                "argvAction.when",
            )
            self.assertIsInstance(action["copySafety"], str)
            assert_max_length(
                action["copySafety"],
                action_schema["copySafety"]["maxLength"],
                "argvAction.copySafety",
            )
            self.assertIsInstance(action["reason"], str)
            assert_max_length(
                action["reason"],
                action_schema["reason"]["maxLength"],
                "argvAction.reason",
            )

        for entry in decision["degradedSummary"]:
            degraded_schema = schema["$defs"]["degradedSummaryEntry"]["properties"]
            self.assertIsInstance(entry["code"], str)
            assert_max_length(
                entry["code"],
                degraded_schema["code"]["maxLength"],
                "degradedSummary.code",
            )
            self.assertTrue(entry["source"] is None or isinstance(entry["source"], str))
            assert_max_length(
                entry["source"],
                nullable_ref_max_length(degraded_schema["source"]["$ref"]),
                "degradedSummary.source",
            )
            self.assertTrue(
                entry["severity"] is None or isinstance(entry["severity"], str)
            )
            assert_max_length(
                entry["severity"],
                nullable_ref_max_length(degraded_schema["severity"]["$ref"]),
                "degradedSummary.severity",
            )

    def test_consumer_decisions_match_schema_required_properties(self):
        schema = load_fixture(
            "docs/schemas/ee.agent.work_packet_gate_decision.v1.json"
        )
        self.assertEqual(schema["title"], consumer.OUTPUT_SCHEMA)
        self.assertEqual(
            schema["properties"]["schema"]["const"],
            consumer.OUTPUT_SCHEMA,
        )

        required = set(schema["required"])
        properties = set(schema["properties"])
        self.assertEqual(required, properties)

        action_def = schema["$defs"]["argvAction"]
        action_required = set(action_def["required"])
        self.assertEqual(action_required, set(action_def["properties"]))

        source_def = schema["$defs"]["sourceSummary"]
        source_required = set(source_def["required"])
        self.assertEqual(source_required, set(source_def["properties"]))

        degraded_def = schema["$defs"]["degradedSummaryEntry"]
        degraded_required = set(degraded_def["required"])
        self.assertEqual(degraded_required, set(degraded_def["properties"]))

        samples = [
            consumer.consume(envelope(safe_gate())),
            consumer.consume(
                load_fixture("tests/fixtures/swarm_work_packet/crowded_checkout.json")
            ),
            consumer.consume(
                load_fixture("tests/fixtures/swarm_work_packet/bv_timeout_no_output.json")
            ),
            consumer.consume(envelope(install_check_report())),
            consumer.consume({"schema": "ee.error.v2", "error": {"code": "usage"}}),
        ]

        for index, decision in enumerate(samples):
            with self.subTest(sample=index):
                self.assertEqual(set(decision), required)
                self.assertEqual(decision["schema"], consumer.OUTPUT_SCHEMA)
                self.assertEqual(set(decision["sourceSummary"]), source_required)
                for action in decision["argvActions"]:
                    self.assertEqual(set(action), action_required)
                for entry in decision["degradedSummary"]:
                    self.assertEqual(set(entry), degraded_required)
                self.assert_decision_matches_schema_constraints(decision, schema)

    def test_schema_examples_match_consumer_decision_shape(self):
        schema = load_fixture(
            "docs/schemas/ee.agent.work_packet_gate_decision.v1.json"
        )
        required = set(schema["required"])
        action_required = set(schema["$defs"]["argvAction"]["required"])
        source_required = set(schema["$defs"]["sourceSummary"]["required"])
        degraded_required = set(schema["$defs"]["degradedSummaryEntry"]["required"])
        examples = schema.get("examples")

        self.assertIsInstance(examples, list)
        self.assertGreaterEqual(len(examples), 2)

        saw_safe = False
        saw_blocked = False
        for index, example in enumerate(examples):
            with self.subTest(example=index):
                self.assertEqual(set(example), required)
                self.assertEqual(example["schema"], consumer.OUTPUT_SCHEMA)
                self.assertEqual(set(example["sourceSummary"]), source_required)
                for action in example["argvActions"]:
                    self.assertEqual(set(action), action_required)
                for entry in example["degradedSummary"]:
                    self.assertEqual(set(entry), degraded_required)
                self.assert_decision_matches_schema_constraints(example, schema)

                if example["safeToClaim"]:
                    saw_safe = True
                    self.assertEqual(example["whyNotSafe"], [])
                    self.assertTrue(
                        any(
                            action["actionKind"] == "claim"
                            and action["runnable"]
                            and action["mutatesState"]
                            for action in example["argvActions"]
                        )
                    )
                else:
                    saw_blocked = True
                    self.assertTrue(example["whyNotSafe"])
                    self.assertFalse(
                        any(
                            action["runnable"] and action["mutatesState"]
                            for action in example["argvActions"]
                        )
                    )

        self.assertTrue(saw_safe)
        self.assertTrue(saw_blocked)
        self.assertEqual(
            [example for example in examples if example["safeToClaim"]],
            [consumer.consume(envelope(safe_gate()))],
        )
        self.assertEqual(
            [
                example
                for example in examples
                if "error:stale_claim_gate_binary" in example["whyNotSafe"]
            ],
            [consumer.error_decision("stale_claim_gate_binary")],
        )


class WorkPacketDocsContract(unittest.TestCase):
    def test_work_packet_docs_reference_consumer_decision_schema(self):
        body = normalize_whitespace(load_text("docs/swarm/work_packet.md"))
        for marker in [
            "ee.agent.work_packet_gate_decision.v1",
            "docs/schemas/ee.agent.work_packet_gate_decision.v1.json",
            "scripts/agent_consume_work_packet_gate.py",
            "consumer_decision",
        ]:
            self.assertIn(marker, body)

    def test_schema_examples_do_not_mark_degraded_authority_claim_safe(self):
        schema = load_fixture("docs/schemas/swarm/ee.swarm.work_packet.v1.json")
        examples = schema.get("examples")
        self.assertIsInstance(examples, list)

        checked = 0
        for index, example in enumerate(examples):
            coordination = consumer.dict_or_empty(example.get("coordination"))
            agent_mail = consumer.dict_or_empty(coordination.get("agentMail"))
            rch = consumer.dict_or_empty(example.get("rchProofPosture"))
            degraded_authority = (
                agent_mail.get("reservationAuthoritative") is False
                or agent_mail.get("inboxAuthoritative") is False
                or rch.get("safeToLaunchCargoVerification") is False
            )
            if not degraded_authority:
                continue

            checked += 1
            recommended = consumer.dict_or_empty(example.get("recommendedAction"))
            self.assertIs(
                recommended.get("safeToClaim"),
                False,
                f"schema example {index} must not recommend safe claim",
            )
            candidate_decisions = [
                candidate.get("decision")
                for candidate in consumer.list_items(example.get("candidates"))
                if isinstance(candidate, dict)
            ]
            self.assertNotIn(
                "safe_to_claim",
                candidate_decisions,
                f"schema example {index} must keep degraded candidates advisory",
            )

        self.assertGreater(checked, 0)

    def test_work_packet_docs_pin_stale_binary_stop_condition(self):
        required_markers = [
            "rejects `--claim-gate` or `--candidate`",
            "unexpected argument",
            "stale relative to the current source/docs contract",
            "Stop at inspection",
            "no BV claim command",
            "local Cargo",
            "RCH/release-path rebuild",
        ]
        docs = [
            "AGENTS.md",
            "README.md",
            "docs/agent-ux/swarm-work-packet.md",
            "docs/swarm/work_packet.md",
        ]

        for relative_path in docs:
            body = normalize_whitespace(load_text(relative_path))
            for marker in required_markers:
                self.assertIn(marker, body, f"{relative_path} missing {marker!r}")

    def test_work_packet_docs_pin_bv_liveness_fail_closed_runbook(self):
        body = normalize_whitespace(load_text("docs/swarm/work_packet.md"))
        required_markers = [
            "Raw `bv --robot-*` probes must be externally bounded",
            "`bv_command_timeout` and `bv_no_output` are source-authority degradations",
            "not evidence that no work exists",
            "emit no runnable claim action",
            "ignore any legacy BV copy-paste claim",
            "br --no-auto-import --allow-stale",
            "rerun `ee swarm work-packet --claim-gate`",
        ]

        for marker in required_markers:
            self.assertIn(marker, body, f"docs/swarm/work_packet.md missing {marker!r}")

    def test_work_packet_docs_pin_metadata_only_beads_sync_warning_authority(self):
        body = normalize_whitespace(load_text("docs/swarm/work_packet.md"))
        required_markers = [
            "brReadsAuthoritative` means the collected parity evidence is sufficient",
            "can remain true for a metadata-only `external_changes_pending_import` warning",
            "DB/JSONL counts match",
            "dirtyIssueCount=0",
            "pendingImportCount=0",
            "A prose `br doctor` message alone is not tracker corruption evidence",
            "requiresCandidateDowngrade` is true when tracker evidence is not authoritative",
            "dirty DB issues",
            "non-benign merge artifacts",
        ]

        for marker in required_markers:
            self.assertIn(marker, body, f"docs/swarm/work_packet.md missing {marker!r}")

    def test_agent_docs_pin_claim_gate_rch_remote_authority_rule(self):
        required_markers = [
            "sourceAuthority.rchRemoteOnlyRequired",
            "sourceAuthority.rchSafeToLaunchCargoVerification",
            "fail closed",
            "remote-only verification is required",
            "positive RCH proof is missing or false",
            "green local compile posture is not enough",
        ]
        docs = [
            "AGENTS.md",
            "README.md",
            "docs/agent-ux/swarm-work-packet.md",
            "docs/swarm/work_packet.md",
        ]

        for relative_path in docs:
            body = normalize_whitespace(load_text(relative_path))
            for marker in required_markers:
                self.assertIn(marker, body, f"{relative_path} missing {marker!r}")

    def test_agent_docs_pin_stale_swarm_brief_field_projection_guard(self):
        required_markers = [
            "ee swarm brief --fields summary --workspace . --json",
            "ee --fields summary swarm brief --workspace . --json",
            "usage_unknown_field",
            "presetsAvailable",
            "stale relative to the current source/docs contract",
            "ee swarm brief --workspace . --json",
            "read-only inspection",
            "does not authorize Beads mutation",
            "work-packet claim gate succeeds",
            "RCH/release-path rebuild",
            "ee.agent.work_packet_gate_decision.v1.json",
        ]
        docs = [
            "README.md",
            "docs/agent_integration.md",
        ]

        for relative_path in docs:
            body = normalize_whitespace(load_text(relative_path))
            for marker in required_markers:
                self.assertIn(marker, body, f"{relative_path} missing {marker!r}")

    def test_agent_docs_pin_install_check_consumer_fail_closed(self):
        required_markers = [
            "ee install check --json --offline",
            "scripts/agent_consume_work_packet_gate.py",
            "ee.install.check.v1",
            "safeToClaim=false",
            "install_freshness:<verdict>",
            "install_finding:<code>",
            "install_check_is_not_claim_gate",
            "not a claim ticket",
            "work-packet claim gate",
        ]
        docs = [
            "docs/agent_integration.md",
            "docs/agent-ux/swarm-work-packet.md",
        ]

        for relative_path in docs:
            body = normalize_whitespace(load_text(relative_path))
            for marker in required_markers:
                self.assertIn(marker, body, f"{relative_path} missing {marker!r}")

    def test_agent_docs_pin_no_local_cargo_install_adoption_workflow(self):
        integration_markers = [
            "No-Local-Cargo Install Freshness",
            "command -v ee",
            "ee --version",
            "ee install check --json --offline",
            "ee install plan --json --offline",
            "--manifest <release-manifest.json>",
            "--artifact-root <release-artifact-dir>",
            "--install-dir \"$HOME/.local/bin\"",
            "--target aarch64-apple-darwin",
            "data.schema=ee.install.plan.v1",
            "data.status",
            "ready",
            "idempotent",
            "selected artifact target matches the host",
            "data.verification.checksumStatus=verified",
            "checksumStatus=planned",
            "Applying a plan is a mutating install action",
            "must not run it unless the user explicitly approves the overwrite path and artifact source",
            "RCH Linux proof and macOS install freshness are different claims",
            "operator exception request",
            "local build would violate the RCH-only policy",
        ]
        integration_body = normalize_whitespace(load_text("docs/agent_integration.md"))
        for marker in integration_markers:
            self.assertIn(marker, integration_body, f"docs/agent_integration.md missing {marker!r}")

        work_packet_markers = [
            "macOS adoption without local Cargo",
            "docs/agent_integration.md",
            "ee install plan --json --offline",
            "--artifact-root <release-artifact-dir>",
            "--target aarch64-apple-darwin",
            "data.verification.checksumStatus=verified",
            "Running `ee update`, copying from `target/`, or using `cargo install`",
            "requires explicit approval of the overwrite path and artifact source",
        ]
        work_packet_body = normalize_whitespace(
            load_text("docs/agent-ux/swarm-work-packet.md")
        )
        for marker in work_packet_markers:
            self.assertIn(
                marker,
                work_packet_body,
                f"docs/agent-ux/swarm-work-packet.md missing {marker!r}",
            )

    def test_environment_attestation_docs_pin_beads_bv_authority_stop_rule(self):
        body = normalize_whitespace(load_text("docs/environment_attestation.md"))
        required_markers = [
            "Beads plus the claim gate wins",
            "Ignore stale BV copy-paste claim commands",
            "Known upstream Beads/BV failure signatures",
            "source-authority evidence, not as permission to mutate the tracker",
            "Duplicate or stale metadata witnesses",
            "doctor.ok=true",
            "jsonl_newer=true",
            "br show --no-auto-import --no-auto-flush",
            "br sync --import-only",
            "br sync --flush-only",
            "BV robot output can recommend blocked or dependency-blocked work",
            "`br ready` can include `in_progress` issues",
            "read-only evidence collection",
            "Agent Mail coordination",
            "upstream issue/comment",
            "Do not claim, reopen, close, or create Beads",
            "live tracker authority and claim gate agree",
            "beads_rust/issues/324",
            "beads_rust/issues/325",
            "beads_rust/issues/330",
            "beads_rust/issues/331",
            "beads_rust/issues/332",
            "beads_rust/issues/333",
        ]

        for marker in required_markers:
            self.assertIn(marker, body, f"environment attestation docs missing {marker!r}")


if __name__ == "__main__":
    runner = unittest.main(exit=False, verbosity=2, module="__main__")
    sys.exit(0 if runner.result.wasSuccessful() else 1)
