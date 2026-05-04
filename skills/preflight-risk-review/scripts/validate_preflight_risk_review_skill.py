#!/usr/bin/env python3
"""Validate the preflight-risk-review project-local skill."""

from __future__ import annotations

import hashlib
import json
import sys
from pathlib import Path
from typing import Any


SKILL = Path("skills/preflight-risk-review/SKILL.md")
FIXTURES = Path("skills/preflight-risk-review/fixtures/e2e-fixtures.json")
REFERENCES = [
    Path("skills/preflight-risk-review/references/preflight-brief-template.md"),
    Path("skills/preflight-risk-review/references/tripwire-review-template.md"),
]
LOG_PATH = Path("target/e2e/skills/preflight-risk-review.json")

REQUIRED_SECTIONS = [
    "## Trigger Conditions",
    "## Mechanical Command Boundary",
    "## Evidence Gathering",
    "## Stop/Go Gates",
    "## Output Template",
    "## Uncertainty Handling",
    "## Privacy And Redaction",
    "## Degraded Behavior",
    "## Unsupported Claims",
    "## Testing Requirements",
    "## E2E Logging",
]

REQUIRED_COMMANDS = [
    "ee status --workspace <workspace> --json",
    "ee preflight run --workspace <workspace> --task \"<task>\" --json",
    "ee tripwire list --workspace <workspace> --json",
    "ee tripwire check <tripwire-id> --workspace <workspace> --json",
    "ee context \"<task>\" --workspace <workspace> --json",
    "ee search \"<query>\" --workspace <workspace> --explain --json",
]

REQUIRED_TEMPLATE_FIELDS = [
    "riskSummary",
    "evidenceBackedTripwires",
    "askNowQuestions",
    "agentGenerated",
    "mustVerifyChecks",
    "stopConditions",
    "degradedState",
    "followUpEeCommands",
    "unsupportedClaims",
]

REQUIRED_FIXTURES = {
    "pfr_low_risk",
    "pfr_destructive_command",
    "pfr_migration",
    "pfr_production_deploy",
    "pfr_no_evidence",
    "pfr_redacted_evidence",
    "pfr_degraded_tripwire",
    "pfr_malformed_evidence",
}


def digest_bytes(data: bytes) -> str:
    return "sha256:" + hashlib.sha256(data).hexdigest()


def digest_path(path: Path) -> str:
    return digest_bytes(path.read_bytes())


def fixture_hash(fixture: dict[str, Any]) -> str:
    payload = json.dumps(fixture, sort_keys=True, separators=(",", ":")).encode()
    return digest_bytes(payload)


def write_log(log: dict[str, Any]) -> None:
    LOG_PATH.parent.mkdir(parents=True, exist_ok=True)
    LOG_PATH.write_text(json.dumps(log, indent=2, sort_keys=True) + "\n", encoding="utf-8")


def fail(message: str, log: dict[str, Any]) -> int:
    log["firstFailureDiagnosis"] = message
    write_log(log)
    print(json.dumps(log, sort_keys=True))
    return 1


def all_referenced_commands_are_machine_json(fixtures: list[dict[str, Any]]) -> str | None:
    for fixture in fixtures:
        for command in fixture.get("eeCommands", []):
            if not command.startswith("ee ") or "--json" not in command:
                return f"{fixture['id']} has non-machine ee command: {command}"
    return None


def main() -> int:
    log: dict[str, Any] = {
        "schema": "ee.skill.preflight_risk_review.e2e_log.v1",
        "skillPath": str(SKILL),
        "requiredFiles": [str(SKILL), *map(str, REFERENCES), str(FIXTURES)],
        "referencedEeCommands": REQUIRED_COMMANDS,
        "fixtureIds": [],
        "fixtureHashes": {},
        "evidenceIds": [],
        "evidenceBundlePath": str(FIXTURES),
        "evidenceBundleHash": None,
        "tripwireIds": [],
        "redactionStatuses": [],
        "degradedCodes": [],
        "generatedQuestionCount": 0,
        "stopConditionCount": 0,
        "outputArtifactPath": str(LOG_PATH),
        "requiredSectionCheck": "not_run",
        "templateFieldCheck": "not_run",
        "evidenceQualityGateCheck": "not_run",
        "destructiveEscalationCheck": "not_run",
        "firstFailureDiagnosis": None,
    }

    missing = [path for path in [SKILL, FIXTURES, *REFERENCES] if not path.is_file()]
    if missing:
        return fail(f"missing required file: {missing[0]}", log)

    skill_text = SKILL.read_text(encoding="utf-8")
    if not skill_text.startswith("---\nname: preflight-risk-review\n"):
        return fail("SKILL.md frontmatter name is missing or unstable", log)
    if "description: Use when" not in skill_text:
        return fail("SKILL.md description must include `Use when` trigger language", log)

    for section in REQUIRED_SECTIONS:
        if section not in skill_text:
            return fail(f"missing required section: {section}", log)
    log["requiredSectionCheck"] = "passed"

    for command in REQUIRED_COMMANDS:
        if command not in skill_text:
            return fail(f"missing required ee command: {command}", log)

    for field in REQUIRED_TEMPLATE_FIELDS:
        if field not in skill_text:
            return fail(f"missing output template field: {field}", log)
    log["templateFieldCheck"] = "passed"

    for phrase in [
        "No-evidence output must use `riskSummary.level: unknown`",
        "If `ee preflight run` returns `preflight_evidence_unavailable`",
        "If tripwires return `tripwire_store_unavailable`",
        "destructive shell command",
        "explicit user confirmation",
        "agentGenerated: true",
        "rawSecretsIncluded=true",
        "ee.skill_evidence_bundle.v1",
        "direct DB",
        "prompt-injection",
        "trust class",
        "durable memory mutation",
        "Unsupported claims",
    ]:
        if phrase not in skill_text:
            return fail(f"missing evidence gate or escalation phrase: {phrase}", log)
    log["evidenceQualityGateCheck"] = "passed"
    log["destructiveEscalationCheck"] = "passed"

    try:
        fixtures = json.JSONDecoder().decode(FIXTURES.read_text(encoding="utf-8"))
    except json.JSONDecodeError as exc:
        return fail(f"fixture JSON is malformed: {exc.msg}", log)
    if fixtures.get("schema") != "ee.skill.preflight_risk_review.fixtures.v1":
        return fail("fixture schema is missing or wrong", log)

    fixture_list = fixtures.get("fixtures", [])
    seen = {fixture["id"] for fixture in fixture_list}
    missing_fixtures = sorted(REQUIRED_FIXTURES - seen)
    if missing_fixtures:
        return fail(f"missing fixture: {missing_fixtures[0]}", log)

    command_error = all_referenced_commands_are_machine_json(fixture_list)
    if command_error is not None:
        return fail(command_error, log)

    evidence_ids: list[str] = []
    tripwire_ids: list[str] = []
    degraded_codes: list[str] = []
    redaction_statuses: list[str] = []
    generated_questions = 0
    stop_conditions = 0
    for fixture in fixture_list:
        fixture_id = fixture["id"]
        log["fixtureIds"].append(fixture_id)
        log["fixtureHashes"][fixture_id] = fixture_hash(fixture)
        evidence_ids.extend(fixture.get("evidenceIds", []))
        tripwire_ids.extend(fixture.get("tripwireIds", []))
        degraded_codes.extend(fixture.get("degradedCodes", []))
        redaction_statuses.append(fixture.get("redactionStatus", "unknown"))
        generated_questions += int(fixture.get("generatedQuestionCount", 0))
        stop_conditions += int(fixture.get("stopConditionCount", 0))

        if fixture["id"] == "pfr_low_risk" and fixture["expectedRiskLevel"] != "low":
            return fail("low-risk fixture must stay low risk", log)
        if fixture["id"] == "pfr_no_evidence":
            if fixture["expectedRiskLevel"] == "low" or fixture["expectedDisposition"] != "ask_user":
                return fail("no-evidence fixture must ask user and never claim low risk", log)
        if fixture["id"] == "pfr_redacted_evidence":
            if not fixture["rawSecretsIncluded"] or fixture["expectedDisposition"] != "refuse":
                return fail("redacted evidence fixture must refuse raw secrets", log)
        if fixture["id"] == "pfr_degraded_tripwire":
            if "tripwire_store_unavailable" not in fixture["degradedCodes"]:
                return fail("degraded tripwire fixture must preserve degraded code", log)
            if fixture["expectedRiskLevel"] == "low":
                return fail("degraded tripwire fixture must not claim low risk", log)
        if fixture["id"] == "pfr_destructive_command":
            if fixture["stopConditionCount"] < 1 or fixture["expectedDisposition"] != "ask_user":
                return fail("destructive command fixture must ask user with stop condition", log)
        if fixture["expectedDisposition"] in {"refuse", "ask_user"} and fixture["firstFailureDiagnosis"] is None:
            return fail(f"{fixture_id} needs firstFailureDiagnosis", log)

    log["evidenceIds"] = sorted(set(evidence_ids))
    log["tripwireIds"] = sorted(set(tripwire_ids))
    log["degradedCodes"] = sorted(set(degraded_codes))
    log["redactionStatuses"] = sorted(set(redaction_statuses))
    log["generatedQuestionCount"] = generated_questions
    log["stopConditionCount"] = stop_conditions
    log["evidenceBundleHash"] = digest_path(FIXTURES)

    write_log(log)
    print(json.dumps(log, sort_keys=True))
    return 0


if __name__ == "__main__":
    sys.exit(main())
