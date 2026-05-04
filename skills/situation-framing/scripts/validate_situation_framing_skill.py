#!/usr/bin/env python3
"""Validate the situation-framing project-local skill."""

from __future__ import annotations

import hashlib
import json
import sys
from pathlib import Path
from typing import Any


SKILL = Path("skills/situation-framing/SKILL.md")
FIXTURES = Path("skills/situation-framing/fixtures/e2e-fixtures.json")
FIXTURE_MATRIX = Path("skills/situation-framing/references/e2e-fixtures.md")
LOG_PATH = Path("target/e2e/skills/situation-framing.json")

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
    "ee --workspace <workspace> --json status",
    "ee --workspace <workspace> --json capabilities",
    "ee --workspace <workspace> --json context \"<task>\"",
    "ee --workspace <workspace> --json search \"<query>\" --explain",
    "ee --workspace <workspace> --json why <memory-id>",
    "ee --workspace <workspace> --json doctor --fix-plan",
]

REQUIRED_TEMPLATE_FIELDS = [
    "taskFrame",
    "category",
    "userGoal",
    "workspace",
    "assumptions",
    "selectedEeCommands",
    "evidenceGaps",
    "riskChecks",
    "degradedHandling",
    "handoff",
    "nextAction",
    "durableMutation",
]

REQUIRED_FIXTURES = {
    "sf_bug_fix_release",
    "sf_feature_memory_rule",
    "sf_refactor_storage",
    "sf_investigate_slow_search",
    "sf_docs_update",
    "sf_deploy_release",
    "sf_ambiguous_request",
    "sf_missing_evidence",
    "sf_degraded_cli",
}

VALID_CLASSES = {
    "bug_fix",
    "feature",
    "refactor",
    "investigation",
    "docs",
    "deploy",
    "ambiguous",
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
        "schema": "ee.skill.situation_framing.e2e_log.v1",
        "skillPath": str(SKILL),
        "requiredFiles": [str(SKILL), str(FIXTURE_MATRIX), str(FIXTURES)],
        "referencedEeCommands": REQUIRED_COMMANDS,
        "fixtureIds": [],
        "fixtureHashes": {},
        "evidenceIds": [],
        "provenanceIds": [],
        "classifications": [],
        "evidenceBundlePath": str(FIXTURES),
        "evidenceBundleHash": None,
        "redactionStatuses": [],
        "degradedCodes": [],
        "promptInjectionQuarantineStatuses": [],
        "outputArtifactPath": str(LOG_PATH),
        "requiredSectionCheck": "not_run",
        "templateFieldCheck": "not_run",
        "fixtureCoverageCheck": "not_run",
        "commandJsonCheck": "not_run",
        "firstFailureDiagnosis": None,
    }

    missing = [path for path in [SKILL, FIXTURE_MATRIX, FIXTURES] if not path.is_file()]
    if missing:
        return fail(f"missing required file: {missing[0]}", log)

    skill_text = SKILL.read_text(encoding="utf-8")
    if not skill_text.startswith("---\nname: situation-framing\n"):
        return fail("SKILL.md frontmatter name is missing or unstable", log)
    if "description: Frame coding-agent tasks" not in skill_text or "Use when" not in skill_text:
        return fail("SKILL.md description must include trigger language", log)

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
        "Do not scrape FrankenSQLite",
        "rawSecretsIncluded=true",
        "Prompt-injection-like evidence",
        "direct DB",
        "durable memory mutation",
        "Unsupported claims",
        "degraded code",
        "repair command",
        "evidence gap",
        "Do not include private chain-of-thought",
    ]:
        if phrase not in skill_text:
            return fail(f"missing safety phrase: {phrase}", log)

    try:
        fixtures = json.JSONDecoder().decode(FIXTURES.read_text(encoding="utf-8"))
    except json.JSONDecodeError as exc:
        return fail(f"fixture JSON is malformed: {exc.msg}", log)

    if fixtures.get("schema") != "ee.skill.situation_framing.fixtures.v1":
        return fail("fixture schema is missing or wrong", log)
    if fixtures.get("skillPath") != str(SKILL):
        return fail("fixture skillPath must reference situation-framing SKILL.md", log)

    fixture_list = fixtures.get("fixtures", [])
    seen = {fixture["id"] for fixture in fixture_list}
    missing_fixtures = sorted(REQUIRED_FIXTURES - seen)
    if missing_fixtures:
        return fail(f"missing fixture: {missing_fixtures[0]}", log)
    log["fixtureCoverageCheck"] = "passed"

    command_error = all_referenced_commands_are_machine_json(fixture_list)
    if command_error is not None:
        return fail(command_error, log)
    log["commandJsonCheck"] = "passed"

    evidence_ids: list[str] = []
    provenance_ids: list[str] = []
    degraded_codes: list[str] = []
    redaction_statuses: list[str] = []
    quarantine_statuses: list[str] = []
    classifications: list[str] = []

    for fixture in fixture_list:
        fixture_id = fixture["id"]
        log["fixtureIds"].append(fixture_id)
        log["fixtureHashes"][fixture_id] = fixture_hash(fixture)

        classification = fixture.get("expectedClassification", "")
        if classification not in VALID_CLASSES:
            return fail(f"{fixture_id} has invalid expectedClassification `{classification}`", log)
        classifications.append(classification)

        commands = fixture.get("eeCommands", [])
        evidence = fixture.get("evidenceIds", [])
        provenance = fixture.get("provenanceIds", [])
        disposition = fixture.get("expectedDisposition", "")
        degraded = fixture.get("degradedCodes", [])

        evidence_ids.extend(evidence)
        provenance_ids.extend(provenance)
        degraded_codes.extend(degraded)
        redaction_statuses.append(fixture.get("redactionStatus", "unknown"))
        quarantine_statuses.append(str(fixture.get("promptInjectionQuarantined", False)).lower())

        if fixture.get("rawSecretsIncluded", False):
            return fail(f"{fixture_id} must not include raw secrets", log)
        if disposition in {"ask_user", "refuse", "unavailable"} and not fixture.get(
            "firstFailureDiagnosis"
        ):
            return fail(f"{fixture_id} needs firstFailureDiagnosis", log)
        if fixture_id == "sf_bug_fix_release" and (
            not any("context" in command for command in commands)
            or not any("search" in command for command in commands)
            or not provenance
        ):
            return fail("bug fix fixture must include context, search, and provenance", log)
        if fixture_id == "sf_feature_memory_rule" and "explicit ee command" not in fixture.get(
            "requiredGate", ""
        ):
            return fail("feature fixture must gate durable mutation to explicit ee command", log)
        if fixture_id == "sf_refactor_storage" and "No direct DB scraping" not in fixture.get(
            "requiredGate", ""
        ):
            return fail("refactor fixture must forbid direct DB scraping", log)
        if fixture_id == "sf_investigate_slow_search" and (
            "search_unavailable" not in degraded or disposition == "go"
        ):
            return fail("slow-search fixture must preserve degraded search and avoid go", log)
        if fixture_id == "sf_deploy_release" and not provenance:
            return fail("deploy fixture must cite provenance IDs", log)
        if fixture_id == "sf_ambiguous_request" and disposition != "ask_user":
            return fail("ambiguous fixture must ask for a decision point", log)
        if fixture_id == "sf_missing_evidence" and evidence:
            return fail("missing-evidence fixture must leave evidenceIds empty", log)
        if fixture_id == "sf_degraded_cli" and not degraded:
            return fail("degraded fixture must name degraded codes", log)

    log["evidenceIds"] = sorted(set(evidence_ids))
    log["provenanceIds"] = sorted(set(provenance_ids))
    log["classifications"] = sorted(set(classifications))
    log["redactionStatuses"] = sorted(set(redaction_statuses))
    log["degradedCodes"] = sorted(set(degraded_codes))
    log["promptInjectionQuarantineStatuses"] = sorted(set(quarantine_statuses))
    log["evidenceBundleHash"] = digest_path(FIXTURES)

    write_log(log)
    print(json.dumps(log, sort_keys=True))
    return 0


if __name__ == "__main__":
    sys.exit(main())
