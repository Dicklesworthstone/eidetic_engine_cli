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
            "sourceCount": 4,
        },
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

    def test_malformed_source_authority_scalars_fail_closed_and_emit_schema_types(self):
        gate = safe_gate()
        gate["sourceAuthority"].update(
            {
                "trackerAuthoritative": "true",
                "reservationAuthoritative": ["true"],
                "inboxAuthoritative": {"value": True},
                "rchRemoteOnlyRequired": "true",
                "rchSafeToLaunchCargoVerification": 1,
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
            "malformed_claim_gate_reservation_authoritative",
            "malformed_claim_gate_inbox_authoritative",
            "malformed_claim_gate_rch_remote_only_required",
            "malformed_claim_gate_rch_safe_to_launch_cargo_verification",
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
            "malformed_claim_gate_candidate_id",
            "malformed_claim_gate_candidate_decision",
        ]:
            self.assertIn(reason, decision["whyNotSafe"])
        self.assertIn("claim_gate_safe_flag_not_true", decision["whyNotSafe"])
        self.assertIn("claim_gate_recommended_not_safe", decision["whyNotSafe"])
        claim = [a for a in decision["argvActions"] if a["actionKind"] == "claim"][0]
        self.assertFalse(claim["runnable"])
        self.assertEqual(claim["reason"], "mutating_action_requires_safe_gate")

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

    def test_healthy_fixture_is_claim_safe_positive_control(self):
        packet = load_fixture("tests/fixtures/swarm_work_packet/healthy_small.json")
        decision = consumer.consume(packet)

        self.assertTrue(decision["safeToClaim"])
        self.assertEqual(decision["decision"], "safe_to_claim")
        self.assertEqual(decision["whyNotSafe"], [])
        self.assertEqual(decision["sourceSummary"]["reservationAuthoritative"], True)
        self.assertEqual(decision["sourceSummary"]["inboxAuthoritative"], True)

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
        self.assertEqual(action["reason"], "argv_redacted")


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

        self.assertEqual(result.returncode, 0)
        self.assertEqual(result.stderr, "")
        self.assertTrue(decision["safeToClaim"])
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
        self.assertEqual(action["reason"], "copy_safety:copy:[redacted]")

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


class ConsumerDecisionSchemaContract(unittest.TestCase):
    def assert_decision_matches_schema_constraints(self, decision, schema):
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
        self.assertIsInstance(decision["decision"], str)
        self.assertIsInstance(decision["action"], str)
        self.assertIsInstance(decision["argvActions"], list)
        self.assertIsInstance(decision["mutatingActionsRequireHuman"], bool)
        self.assertIsInstance(decision["whyNotSafe"], list)
        self.assertTrue(
            all(isinstance(reason, str) for reason in decision["whyNotSafe"])
        )
        self.assertIsInstance(decision["degradedSummary"], list)
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
            self.assertIsInstance(action["commandId"], str)
            self.assertIn(action["actionKind"], action_kind_enum)
            self.assertIsInstance(action["argv"], list)
            self.assertTrue(all(isinstance(part, str) for part in action["argv"]))
            self.assertIsInstance(action["runnable"], bool)
            self.assertIsInstance(action["reviewRequired"], bool)
            self.assertIsInstance(action["mutatesState"], bool)
            self.assertTrue(
                action["requiredSubstrate"] is None
                or isinstance(action["requiredSubstrate"], str)
            )
            self.assertTrue(action["when"] is None or isinstance(action["when"], str))
            self.assertIsInstance(action["copySafety"], str)
            self.assertIsInstance(action["reason"], str)

        for entry in decision["degradedSummary"]:
            self.assertIsInstance(entry["code"], str)
            self.assertTrue(entry["source"] is None or isinstance(entry["source"], str))
            self.assertTrue(
                entry["severity"] is None or isinstance(entry["severity"], str)
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


if __name__ == "__main__":
    runner = unittest.main(exit=False, verbosity=2, module="__main__")
    sys.exit(0 if runner.result.wasSuccessful() else 1)
