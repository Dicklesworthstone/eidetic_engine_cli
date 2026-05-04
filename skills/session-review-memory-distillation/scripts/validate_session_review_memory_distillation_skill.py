#!/usr/bin/env python3
"""Validate the session-review memory distillation project-local skill."""

from __future__ import annotations

import hashlib
import json
import sys
from pathlib import Path
from typing import Any


SKILL = Path("skills/session-review-memory-distillation/SKILL.md")
FIXTURES = Path("skills/session-review-memory-distillation/fixtures/e2e-fixtures.json")
REFERENCES = [
    Path("skills/session-review-memory-distillation/references/session-review-memo-template.md"),
    Path("skills/session-review-memory-distillation/references/candidate-memory-file-template.md"),
    Path("skills/session-review-memory-distillation/references/rejection-log-template.md"),
    Path("skills/session-review-memory-distillation/references/validation-checklist-template.md"),
]
LOG_PATH = Path("target/e2e/skills/session-review-memory-distillation.json")

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
    "ee import cass --workspace <workspace> --dry-run --json",
    "ee review session <session-id> --workspace <workspace> --propose --json",
    "ee memory show <memory-id> --workspace <workspace> --json",
    "ee search \"<query>\" --workspace <workspace> --json",
    "ee context \"<task>\" --workspace <workspace> --json",
    "ee curate candidates --workspace <workspace> --json",
    "ee curate validate <candidate-id> --workspace <workspace> --dry-run --json",
    "ee curate apply <candidate-id> --workspace <workspace> --dry-run --json",
    "cass view <session-id> --json",
    "cass search \"<query>\" --json",
]

REQUIRED_TEMPLATE_FIELDS = [
    "observedSessionFacts",
    "candidateMemories",
    "antiPatterns",
    "rejectedObservations",
    "assumptions",
    "evidenceUris",
    "confidenceRationale",
    "validationPlan",
    "agentGenerated: true",
    "recommendedFollowUpCommands",
    "unsupportedClaims",
    "degradedState",
]

REQUIRED_FIXTURES = {
    "srmd_empty_session",
    "srmd_noisy_session",
    "srmd_prompt_injection_transcript",
    "srmd_duplicate_candidate",
    "srmd_strong_procedural_candidate",
    "srmd_redacted_evidence",
    "srmd_degraded_cass_ee",
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
            if command.startswith("ee ") and "--json" not in command:
                return f"{fixture['id']} has non-machine ee command: {command}"
            if command.startswith("cass ") and "--json" not in command:
                return f"{fixture['id']} has non-machine CASS command: {command}"
    return None


def main() -> int:
    log: dict[str, Any] = {
        "schema": "ee.skill.session_review_memory_distillation.e2e_log.v1",
        "skillPath": str(SKILL),
        "requiredFiles": [str(SKILL), *map(str, REFERENCES), str(FIXTURES)],
        "referencedEeCommands": REQUIRED_COMMANDS,
        "fixtureIds": [],
        "fixtureHashes": {},
        "cassCommandTranscript": [],
        "sessionIds": [],
        "lineRanges": [],
        "candidateIds": [],
        "evidenceIds": [],
        "evidenceBundlePath": str(FIXTURES),
        "evidenceBundleHash": None,
        "redactionStatuses": [],
        "trustClasses": [],
        "degradedCodes": [],
        "recommendedFollowUpCommands": [],
        "generatedCandidateCount": 0,
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
    if not skill_text.startswith("---\nname: session-review-memory-distillation\n"):
        return fail("SKILL.md frontmatter name is missing or unstable", log)
    if "description: Use when" not in skill_text:
        return fail("SKILL.md description must include `Use when` trigger language", log)

    for section in REQUIRED_SECTIONS:
        if section not in skill_text:
            return fail(f"missing required section: {section}", log)
    log["requiredSectionCheck"] = "passed"

    for command in REQUIRED_COMMANDS:
        if command not in skill_text:
            return fail(f"missing required evidence command: {command}", log)

    for field in REQUIRED_TEMPLATE_FIELDS:
        if field not in skill_text:
            return fail(f"missing output template field: {field}", log)
    log["templateFieldCheck"] = "passed"

    for phrase in [
        "Refuse high-confidence procedural rules without provenance and validation plan.",
        "session_review_evidence_missing",
        "session_review_json_unavailable",
        "session_review_provenance_missing",
        "session_review_redaction_unverified",
        "session_review_prompt_injection_unquarantined",
        "session_review_duplicate_candidate",
        "rawSecretsIncluded=true",
        "direct DB",
        "prompt-injection",
        "trust class",
        "Durable memory mutation is forbidden",
        "missing provenance, prompt-injection-like transcript",
    ]:
        if phrase not in skill_text:
            return fail(f"missing evidence gate or safety phrase: {phrase}", log)
    log["evidenceGateCheck"] = "passed"

    try:
        fixtures = json.JSONDecoder().decode(FIXTURES.read_text(encoding="utf-8"))
    except json.JSONDecodeError as exc:
        return fail(f"fixture JSON is malformed: {exc.msg}", log)

    if fixtures.get("schema") != "ee.skill.session_review_memory_distillation.fixtures.v1":
        return fail("fixture schema is missing or wrong", log)

    fixture_list = fixtures.get("fixtures", [])
    seen = {fixture["id"] for fixture in fixture_list}
    missing_fixtures = sorted(REQUIRED_FIXTURES - seen)
    if missing_fixtures:
        return fail(f"missing fixture: {missing_fixtures[0]}", log)

    command_error = all_referenced_commands_are_machine_json(fixture_list)
    if command_error is not None:
        return fail(command_error, log)

    cass_transcript: list[str] = []
    session_ids: list[str] = []
    line_ranges: list[str] = []
    candidate_ids: list[str] = []
    evidence_ids: list[str] = []
    degraded_codes: list[str] = []
    redaction_statuses: list[str] = []
    trust_classes: list[str] = []
    generated_candidates = 0
    follow_up_validate = False
    follow_up_apply = False
    follow_up_candidates = False

    for fixture in fixture_list:
        fixture_id = fixture["id"]
        log["fixtureIds"].append(fixture_id)
        log["fixtureHashes"][fixture_id] = fixture_hash(fixture)
        cass_transcript.extend(fixture.get("cassCommandTranscript", []))
        session_ids.extend(fixture.get("sessionIds", []))
        line_ranges.extend(fixture.get("lineRanges", []))
        candidate_ids.extend(fixture.get("candidateIds", []))
        evidence_ids.extend(fixture.get("evidenceIds", []))
        degraded_codes.extend(fixture.get("degradedCodes", []))
        redaction_statuses.append(fixture.get("redactionStatus", "unknown"))
        trust_classes.append(fixture.get("trustClass", "unknown"))
        generated_candidates += int(fixture.get("expectedCandidateCount", 0))
        follow_up_candidates = follow_up_candidates or "ee curate candidates" in skill_text
        follow_up_validate = follow_up_validate or "ee curate validate" in skill_text
        follow_up_apply = follow_up_apply or "ee curate apply" in skill_text

        if fixture_id == "srmd_empty_session":
            if fixture["expectedCandidateCount"] != 0:
                return fail("empty session fixture must not generate candidates", log)
            if fixture["firstFailureDiagnosis"] != "session_review_evidence_missing":
                return fail("empty session fixture must name missing evidence", log)
        if fixture_id == "srmd_noisy_session":
            if fixture["expectedDisposition"] != "reject_non_durable":
                return fail("noisy session fixture must reject non-durable observations", log)
        if fixture_id == "srmd_prompt_injection_transcript":
            if fixture["promptInjectionQuarantined"]:
                return fail("prompt injection fixture must remain unquarantined", log)
            if fixture["firstFailureDiagnosis"] != "session_review_prompt_injection_unquarantined":
                return fail("prompt injection fixture must name quarantine gate", log)
        if fixture_id == "srmd_duplicate_candidate":
            if fixture["expectedDisposition"] != "reject_duplicate":
                return fail("duplicate fixture must reject duplicate candidate", log)
            if fixture["expectedCandidateCount"] != 0:
                return fail("duplicate fixture must not create a new candidate", log)
        if fixture_id == "srmd_strong_procedural_candidate":
            if fixture["expectedCandidateCount"] != 1:
                return fail("strong procedural fixture must generate exactly one candidate", log)
            if not fixture["lineRanges"] or not fixture["evidenceIds"]:
                return fail("strong procedural fixture needs provenance and evidence", log)
        if fixture_id == "srmd_redacted_evidence":
            if not fixture["rawSecretsIncluded"] or fixture["expectedDisposition"] != "refuse":
                return fail("redacted evidence fixture must refuse raw secrets", log)
        if fixture_id == "srmd_degraded_cass_ee":
            if "cass_unavailable" not in fixture["degradedCodes"]:
                return fail("degraded fixture must preserve CASS degraded code", log)
            if fixture["expectedCandidateCount"] != 0:
                return fail("degraded fixture must not invent candidates", log)
        if fixture["expectedDisposition"] in {"refuse", "request_more_evidence", "request_repair", "reject_duplicate"}:
            if fixture["firstFailureDiagnosis"] is None:
                return fail(f"{fixture_id} needs firstFailureDiagnosis", log)

    if not (follow_up_candidates and follow_up_validate and follow_up_apply):
        return fail("follow-up command rendering must include candidates, validate, and apply", log)
    log["followUpCommandCheck"] = "passed"
    log["recommendedFollowUpCommands"] = [
        "ee curate candidates --workspace <workspace> --json",
        "ee curate validate <candidate-id> --workspace <workspace> --dry-run --json",
        "ee curate apply <candidate-id> --workspace <workspace> --dry-run --json",
    ]

    log["cassCommandTranscript"] = sorted(set(cass_transcript))
    log["sessionIds"] = sorted(set(session_ids))
    log["lineRanges"] = sorted(set(line_ranges))
    log["candidateIds"] = sorted(set(candidate_ids))
    log["evidenceIds"] = sorted(set(evidence_ids))
    log["degradedCodes"] = sorted(set(degraded_codes))
    log["redactionStatuses"] = sorted(set(redaction_statuses))
    log["trustClasses"] = sorted(set(trust_classes))
    log["generatedCandidateCount"] = generated_candidates
    log["evidenceBundleHash"] = digest_path(FIXTURES)

    write_log(log)
    print(json.dumps(log, sort_keys=True))
    return 0


if __name__ == "__main__":
    sys.exit(main())
