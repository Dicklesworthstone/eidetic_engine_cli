#!/usr/bin/env python3
"""Self-test for `agent_consume_work_packet_gate.py`.

Run:
    python3 scripts/agent_consume_work_packet_gate_test.py
"""

import importlib.util
import json
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

        self.assertTrue(decision["safeToClaim"])
        self.assertTrue(decision["mutatingActionsRequireHuman"])
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

    def test_secret_shaped_strings_are_redacted_and_not_runnable(self):
        gate = safe_gate()
        gate["nextCommandActions"] = [
            safe_action(
                "leaky",
                ["ee", "show", "ghp_abcdef123456789", "stdout: raw command output"],
            )
        ]

        decision = consumer.consume(envelope(gate))
        serialized = json.dumps(decision)
        self.assertNotIn("ghp_", serialized)
        self.assertNotIn("stdout:", serialized)
        action = decision["argvActions"][0]
        self.assertFalse(action["runnable"])
        self.assertTrue(action["reviewRequired"])
        self.assertEqual(action["argv"], [])
        self.assertEqual(action["reason"], "argv_redacted")


class ErrorHandling(unittest.TestCase):
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


class WorkPacketDocsContract(unittest.TestCase):
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
