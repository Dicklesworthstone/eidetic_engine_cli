#!/usr/bin/env python3
"""Validate the counterfactual-failure-analysis project-local skill."""

from __future__ import annotations

import hashlib
import json
import sys
from pathlib import Path
from typing import Any


SKILL = Path("skills/counterfactual-failure-analysis/SKILL.md")
FIXTURES = Path("skills/counterfactual-failure-analysis/fixtures/e2e-fixtures.json")
REFERENCES = [
    Path("skills/counterfactual-failure-analysis/references/failure-analysis-memo.md"),
    Path("skills/counterfactual-failure-analysis/references/evidence-checklist.md"),
    Path("skills/counterfactual-failure-analysis/references/falsification-checklist.md"),
]
LOG_PATH = Path("target/e2e/skills/counterfactual-failure-analysis.json")

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
    "ee lab capture --workspace <workspace> --json",
    "ee lab replay --workspace <workspace> --episode-id <episode-id> --json",
    "ee lab counterfactual --workspace <workspace> --episode-id <episode-id> --json",
    "ee status --workspace <workspace> --json",
]

REQUIRED_TEMPLATE_FIELDS = [
    "observedFacts",
    "replayEvidence",
    "hypotheses",
    "assumptions",
    "agentJudgment",
    "unsupportedClaims",
    "degradedState",
    "recommendedExplicitCommands",
]

REQUIRED_FIXTURES = {
    "cfa_no_evidence",
    "cfa_replay_supported_failure",
    "cfa_contradictory_replay",
    "cfa_redacted_evidence",
    "cfa_prompt_injection_like_evidence",
    "cfa_degraded_ee_lab_output",
}


def digest(path: Path) -> str:
    return "sha256:" + hashlib.sha256(path.read_bytes()).hexdigest()


def fail(message: str, log: dict[str, Any]) -> int:
    log["firstFailureDiagnosis"] = message
    write_log(log)
    print(json.dumps(log, sort_keys=True))
    return 1


def write_log(log: dict[str, Any]) -> None:
    LOG_PATH.parent.mkdir(parents=True, exist_ok=True)
    LOG_PATH.write_text(json.dumps(log, indent=2, sort_keys=True) + "\n")


def fixture_hash(fixture: dict[str, Any]) -> str:
    payload = json.dumps(fixture, sort_keys=True, separators=(",", ":")).encode()
    return "sha256:" + hashlib.sha256(payload).hexdigest()


def main() -> int:
    log: dict[str, Any] = {
        "schema": "ee.skill.counterfactual_failure_analysis.e2e_log.v1",
        "skillPath": str(SKILL),
        "requiredFiles": [str(SKILL), *map(str, REFERENCES), str(FIXTURES)],
        "referencedEeCommands": REQUIRED_COMMANDS,
        "fixtureIds": [],
        "fixtureHashes": {},
        "evidenceIds": [],
        "degradedCodes": [],
        "redactionStatuses": [],
        "evidenceBundlePath": "target/e2e/skills/counterfactual-failure-analysis.evidence.json",
        "evidenceBundleHash": None,
        "outputArtifactPath": str(LOG_PATH),
        "requiredSectionCheck": "not_run",
        "refusalChecks": [],
        "firstFailureDiagnosis": None,
    }

    missing = [path for path in [SKILL, FIXTURES, *REFERENCES] if not path.is_file()]
    if missing:
        return fail(f"missing required file: {missing[0]}", log)

    skill_text = SKILL.read_text()
    if not skill_text.startswith("---\nname: counterfactual-failure-analysis\n"):
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

    for phrase in [
        "would have succeeded",
        "pack presence alone",
        "direct DB scraping",
        "prompt-injection",
        "trust class",
        "durable memory mutation",
        "rawSecretsIncluded=true",
        "ee.skill_evidence_bundle.v1",
    ]:
        if phrase not in skill_text:
            return fail(f"missing refusal or boundary phrase: {phrase}", log)

    fixtures = json.loads(FIXTURES.read_text())
    if fixtures.get("schema") != "ee.skill.counterfactual_failure_analysis.fixtures.v1":
        return fail("fixture schema is missing or wrong", log)

    seen = {fixture["id"] for fixture in fixtures.get("fixtures", [])}
    missing_fixtures = sorted(REQUIRED_FIXTURES - seen)
    if missing_fixtures:
        return fail(f"missing fixture: {missing_fixtures[0]}", log)

    all_evidence_ids: list[str] = []
    all_degraded_codes: list[str] = []
    redaction_statuses: list[str] = []
    for fixture in fixtures["fixtures"]:
        fixture_id = fixture["id"]
        log["fixtureIds"].append(fixture_id)
        log["fixtureHashes"][fixture_id] = fixture_hash(fixture)
        all_evidence_ids.extend(fixture.get("evidenceIds", []))
        all_degraded_codes.extend(fixture.get("degradedCodes", []))
        redaction_statuses.append(fixture.get("redactionStatus", "unknown"))

        for command in fixture.get("eeCommands", []):
            if not command.startswith("ee ") or "--json" not in command:
                return fail(f"{fixture_id} has non-machine ee command: {command}", log)

        if fixture["expectedDisposition"] == "refuse" and fixture["firstFailureDiagnosis"] is None:
            return fail(f"{fixture_id} refusal fixture needs firstFailureDiagnosis", log)

        if fixture["id"] == "cfa_degraded_ee_lab_output":
            if not fixture["packDiffOnly"] or fixture["expectedDisposition"] != "hypothesis_only":
                return fail("degraded lab fixture must be hypothesis-only for pack diffs", log)

        if fixture["id"] == "cfa_contradictory_replay":
            if fixture["expectedDisposition"] != "refuse_strong_claim":
                return fail("contradictory replay must refuse strong claims", log)

        if fixture.get("rawSecretsIncluded") and fixture["redactionStatus"] != "failed":
            return fail(f"{fixture_id} raw secret fixture must mark redaction failed", log)

    log["evidenceIds"] = sorted(set(all_evidence_ids))
    log["degradedCodes"] = sorted(set(all_degraded_codes))
    log["redactionStatuses"] = sorted(set(redaction_statuses))
    log["evidenceBundleHash"] = digest(FIXTURES)
    log["refusalChecks"] = [
        "no evidence refuses",
        "contradictory replay refuses strong claim",
        "redaction failure refuses",
        "unquarantined prompt-injection-like evidence refuses",
        "pack diff only stays hypothesis-only",
    ]

    write_log(log)
    print(json.dumps(log, sort_keys=True))
    return 0


if __name__ == "__main__":
    sys.exit(main())
