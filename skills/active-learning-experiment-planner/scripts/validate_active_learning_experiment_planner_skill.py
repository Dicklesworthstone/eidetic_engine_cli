#!/usr/bin/env python3
"""Validate the active-learning experiment planner project-local skill."""

from __future__ import annotations

import hashlib
import json
import sys
from pathlib import Path
from typing import Any


SKILL = Path("skills/active-learning-experiment-planner/SKILL.md")
FIXTURES = Path("skills/active-learning-experiment-planner/fixtures/e2e-fixtures.json")
REFERENCES = [
    Path("skills/active-learning-experiment-planner/references/experiment-plan-template.md"),
    Path("skills/active-learning-experiment-planner/references/observation-log-template.md"),
    Path("skills/active-learning-experiment-planner/references/closeout-summary-template.md"),
]
LOG_PATH = Path("target/e2e/skills/active-learning-experiment-planner.json")

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
    "ee --workspace <workspace> learn summary --period week --detailed --json",
    "ee --workspace <workspace> learn uncertainty --min-uncertainty 0.3 --json",
    "ee --workspace <workspace> learn experiment propose --safety-boundary dry_run_only --json",
    "ee --workspace <workspace> learn experiment run --id <experiment-id> --dry-run --json",
    "ee learn observe <experiment-id> --measurement-name <name> --signal neutral --evidence-id <evidence-id> --redaction-status redacted --dry-run --json",
    "ee learn close <experiment-id> --status inconclusive --decision-impact \"<impact>\" --safety-note \"<note>\" --dry-run --json",
    "ee context \"<task>\" --workspace <workspace> --json",
    "ee causal compare --workspace <workspace> --json",
    "ee economy score --workspace <workspace> --json",
]

REQUIRED_TEMPLATE_FIELDS = [
    "candidateExperiments",
    "measurableHypothesis",
    "requiredFixturesData",
    "stopCondition",
    "costRisk",
    "expectedInformationValue",
    "expectedDecisionImpact",
    "followUpEeCommands",
    "askForDataCollection",
    "agentGenerated: true",
    "unsupportedClaims",
    "degradedState",
]

REQUIRED_FIXTURES = {
    "alp_empty_records",
    "alp_high_uncertainty",
    "alp_contradictory_outcomes",
    "alp_insufficient_sample_size",
    "alp_redacted_evidence",
    "alp_degraded_dependencies",
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
        "schema": "ee.skill.active_learning_experiment_planner.e2e_log.v1",
        "skillPath": str(SKILL),
        "requiredFiles": [str(SKILL), *map(str, REFERENCES), str(FIXTURES)],
        "referencedEeCommands": REQUIRED_COMMANDS,
        "fixtureIds": [],
        "fixtureHashes": {},
        "observationRecordIds": [],
        "evalRecordIds": [],
        "evidenceIds": [],
        "evidenceBundlePath": str(FIXTURES),
        "evidenceBundleHash": None,
        "redactionStatuses": [],
        "degradedCodes": [],
        "generatedExperimentCount": 0,
        "outputArtifactPath": str(LOG_PATH),
        "requiredSectionCheck": "not_run",
        "templateFieldCheck": "not_run",
        "evidenceGateCheck": "not_run",
        "followUpCommandCheck": "not_run",
        "firstFailureDiagnosis": None,
    }

    missing = [path for path in [SKILL, FIXTURES, *REFERENCES] if not path.is_file()]
    if missing:
        return fail(f"missing required file: {missing[0]}", log)

    skill_text = SKILL.read_text(encoding="utf-8")
    if not skill_text.startswith("---\nname: active-learning-experiment-planner\n"):
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
        "Without evidence, emit a data-collection request, not an experiment plan.",
        "learning_records_unavailable",
        "learning_observations_empty",
        "learning_sample_underpowered",
        "learning_redaction_unverified",
        "learning_prompt_injection_unquarantined",
        "rawSecretsIncluded=true",
        "direct DB",
        "prompt-injection",
        "trust class",
        "Durable memory mutation is forbidden",
        "empty or degraded `ee learn` outputs ask for data collection",
    ]:
        if phrase not in skill_text:
            return fail(f"missing evidence gate or safety phrase: {phrase}", log)
    log["evidenceGateCheck"] = "passed"

    try:
        fixtures = json.JSONDecoder().decode(FIXTURES.read_text(encoding="utf-8"))
    except json.JSONDecodeError as exc:
        return fail(f"fixture JSON is malformed: {exc.msg}", log)

    if fixtures.get("schema") != "ee.skill.active_learning_experiment_planner.fixtures.v1":
        return fail("fixture schema is missing or wrong", log)

    fixture_list = fixtures.get("fixtures", [])
    seen = {fixture["id"] for fixture in fixture_list}
    missing_fixtures = sorted(REQUIRED_FIXTURES - seen)
    if missing_fixtures:
        return fail(f"missing fixture: {missing_fixtures[0]}", log)

    command_error = all_referenced_commands_are_machine_json(fixture_list)
    if command_error is not None:
        return fail(command_error, log)

    observation_ids: list[str] = []
    eval_ids: list[str] = []
    evidence_ids: list[str] = []
    degraded_codes: list[str] = []
    redaction_statuses: list[str] = []
    generated_experiments = 0
    follow_up_observe = False
    follow_up_close = False

    for fixture in fixture_list:
        fixture_id = fixture["id"]
        log["fixtureIds"].append(fixture_id)
        log["fixtureHashes"][fixture_id] = fixture_hash(fixture)
        observation_ids.extend(fixture.get("observationRecordIds", []))
        eval_ids.extend(fixture.get("evalRecordIds", []))
        evidence_ids.extend(fixture.get("evidenceIds", []))
        degraded_codes.extend(fixture.get("degradedCodes", []))
        redaction_statuses.append(fixture.get("redactionStatus", "unknown"))
        generated_experiments += int(fixture.get("expectedGeneratedExperimentCount", 0))
        follow_up_observe = follow_up_observe or any(
            "ee learn observe" in command for command in fixture.get("eeCommands", [])
        )
        follow_up_close = follow_up_close or "ee learn close" in skill_text

        if fixture_id == "alp_empty_records":
            if fixture["expectedGeneratedExperimentCount"] != 0:
                return fail("empty records fixture must not generate experiments", log)
            if fixture["expectedDisposition"] != "request_data_collection":
                return fail("empty records fixture must request data collection", log)
        if fixture_id == "alp_high_uncertainty":
            if fixture["expectedGeneratedExperimentCount"] < 1:
                return fail("high uncertainty fixture must generate a plan", log)
        if fixture_id == "alp_contradictory_outcomes":
            if fixture["expectedDisposition"] != "propose_replication":
                return fail("contradictory outcomes fixture must propose replication", log)
        if fixture_id == "alp_insufficient_sample_size":
            if int(fixture.get("sampleSize", 0)) > 9:
                return fail("insufficient sample fixture must remain underpowered", log)
            if fixture["firstFailureDiagnosis"] != "learning_sample_underpowered":
                return fail("insufficient sample fixture must name underpowered gate", log)
        if fixture_id == "alp_redacted_evidence":
            if not fixture["rawSecretsIncluded"] or fixture["expectedDisposition"] != "refuse":
                return fail("redacted evidence fixture must refuse raw secrets", log)
        if fixture_id == "alp_degraded_dependencies":
            if "learning_records_unavailable" not in fixture["degradedCodes"]:
                return fail("degraded fixture must preserve learn degraded code", log)
            if fixture["expectedGeneratedExperimentCount"] != 0:
                return fail("degraded fixture must not invent experiments", log)
        if fixture["expectedDisposition"] in {"refuse", "request_data_collection", "request_repair"}:
            if fixture["firstFailureDiagnosis"] is None:
                return fail(f"{fixture_id} needs firstFailureDiagnosis", log)

    if not follow_up_observe or not follow_up_close:
        return fail("follow-up command rendering must include observe and close", log)
    log["followUpCommandCheck"] = "passed"

    log["observationRecordIds"] = sorted(set(observation_ids))
    log["evalRecordIds"] = sorted(set(eval_ids))
    log["evidenceIds"] = sorted(set(evidence_ids))
    log["degradedCodes"] = sorted(set(degraded_codes))
    log["redactionStatuses"] = sorted(set(redaction_statuses))
    log["generatedExperimentCount"] = generated_experiments
    log["evidenceBundleHash"] = digest_path(FIXTURES)

    write_log(log)
    print(json.dumps(log, sort_keys=True))
    return 0


if __name__ == "__main__":
    sys.exit(main())
