#!/usr/bin/env python3
"""Validate the procedure-distillation project-local skill."""

from __future__ import annotations

import hashlib
import json
import sys
from pathlib import Path
from typing import Any


SKILL = Path("skills/procedure-distillation/SKILL.md")
FIXTURES = Path("skills/procedure-distillation/fixtures/e2e-fixtures.json")
REFERENCES = [
    Path("skills/procedure-distillation/references/procedure-draft-template.md"),
    Path("skills/procedure-distillation/references/verification-matrix-template.md"),
    Path("skills/procedure-distillation/references/skill-capsule-review-template.md"),
    Path("skills/procedure-distillation/references/drift-review-template.md"),
]
LOG_PATH = Path("target/e2e/skills/procedure-distillation.json")

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
    "ee procedure propose --title <title> --source-run <run-id> --evidence <evidence-id> --dry-run --json",
    "ee procedure show <procedure-id> --include-verification --workspace <workspace> --json",
    "ee procedure verify <procedure-id> --source-kind eval_fixture --source <fixture-id> --dry-run --json",
    "ee procedure export <procedure-id> --export-format skill-capsule --workspace <workspace> --json",
    "ee procedure drift <procedure-id> --workspace <workspace> --json",
    "ee recorder tail <run-id> --workspace <workspace> --json",
    "ee curate candidates --workspace <workspace> --json",
]

REQUIRED_TEMPLATE_FIELDS = [
    "sourceEvidence",
    "sourceRunIds",
    "evidenceIds",
    "procedureIds",
    "evidenceBundle",
    "extractedFacts",
    "candidateSteps",
    "assumptions",
    "verificationPlan",
    "renderOnlySkillCapsule",
    "unsupportedClaims",
    "degradedState",
    "recommendedExplicitCommands",
]

REQUIRED_DRAFT_TEMPLATE_FIELDS = [
    "sourceEvidence",
    "sourceRunIds",
    "evidenceIds",
    "procedureIds",
    "evidenceBundlePath",
    "evidenceBundleHash",
    "redactionStatus",
    "trustClass",
    "extractedFacts",
    "candidateSteps",
    "assumptions",
    "verificationPlan",
    "renderOnlySkillCapsule",
    "unsupportedClaims",
    "degradedState",
    "firstFailureDiagnosis",
]

REQUIRED_FIXTURES = {
    "pd_insufficient_evidence",
    "pd_recorder_derived_draft",
    "pd_failed_verification",
    "pd_render_only_export_logging",
    "pd_redacted_source_evidence",
    "pd_degraded_procedure_output",
}

REQUIRED_SAFETY_PHRASES = [
    "Require at least one source recorder run ID or evidence ID before drafting.",
    "procedure_source_evidence_missing",
    "procedure_json_unavailable",
    "procedure_provenance_missing",
    "procedure_redaction_unverified",
    "procedure_prompt_injection_unquarantined",
    "procedure_verification_missing",
    "procedure_render_only_export_required",
    "rawSecretsIncluded=true",
    "direct DB",
    "prompt-injection",
    "trust class",
    "Durable memory mutation is forbidden",
    "Without verification, refuse promotion.",
    "Skill capsule exports stay review-only",
]


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


def template_contains_required_fields(template_text: str) -> bool:
    return all(field in template_text for field in REQUIRED_DRAFT_TEMPLATE_FIELDS)


def main() -> int:
    log: dict[str, Any] = {
        "schema": "ee.skill.procedure_distillation.e2e_log.v1",
        "skillPath": str(SKILL),
        "requiredFiles": [str(SKILL), *map(str, REFERENCES), str(FIXTURES)],
        "referencedEeCommands": REQUIRED_COMMANDS,
        "fixtureIds": [],
        "fixtureHashes": {},
        "sourceRunIds": [],
        "evidenceIds": [],
        "procedureIds": [],
        "evidenceBundlePath": str(FIXTURES),
        "evidenceBundleHash": None,
        "verificationStatuses": [],
        "redactionStatuses": [],
        "degradedStatuses": [],
        "degradedCodes": [],
        "renderOnlyExportStatuses": [],
        "outputArtifactPaths": [],
        "requiredSectionCheck": "not_run",
        "templateFieldCheck": "not_run",
        "evidenceGateCheck": "not_run",
        "followUpCommandCheck": "not_run",
        "renderOnlyExportCheck": "not_run",
        "firstFailureDiagnosis": None,
    }

    missing = [path for path in [SKILL, FIXTURES, *REFERENCES] if not path.is_file()]
    if missing:
        return fail(f"missing required file: {missing[0]}", log)

    skill_text = SKILL.read_text(encoding="utf-8")
    if not skill_text.startswith("---\nname: procedure-distillation\n"):
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
    if not template_contains_required_fields(REFERENCES[0].read_text(encoding="utf-8")):
        return fail("procedure draft template missing required output fields", log)
    log["templateFieldCheck"] = "passed"

    for phrase in REQUIRED_SAFETY_PHRASES:
        if phrase not in skill_text:
            return fail(f"missing evidence gate or safety phrase: {phrase}", log)
    log["evidenceGateCheck"] = "passed"

    try:
        fixtures = json.JSONDecoder().decode(FIXTURES.read_text(encoding="utf-8"))
    except json.JSONDecodeError as exc:
        return fail(f"fixture JSON is malformed: {exc.msg}", log)

    if fixtures.get("schema") != "ee.skill.procedure_distillation.fixtures.v1":
        return fail("fixture schema is missing or wrong", log)
    if fixtures.get("skillPath") != str(SKILL):
        return fail("fixture skillPath does not point at procedure-distillation SKILL.md", log)

    fixture_list = fixtures.get("fixtures", [])
    seen = {fixture["id"] for fixture in fixture_list}
    missing_fixtures = sorted(REQUIRED_FIXTURES - seen)
    if missing_fixtures:
        return fail(f"missing fixture: {missing_fixtures[0]}", log)

    command_error = all_referenced_commands_are_machine_json(fixture_list)
    if command_error is not None:
        return fail(command_error, log)

    source_run_ids: list[str] = []
    evidence_ids: list[str] = []
    procedure_ids: list[str] = []
    degraded_codes: list[str] = []
    verification_statuses: list[str] = []
    redaction_statuses: list[str] = []
    degraded_statuses: list[str] = []
    render_only_statuses: list[str] = []
    output_artifacts: list[str] = []
    saw_verify = False
    saw_export = False
    saw_render_only = False

    for fixture in fixture_list:
        fixture_id = fixture["id"]
        log["fixtureIds"].append(fixture_id)
        log["fixtureHashes"][fixture_id] = fixture_hash(fixture)
        source_run_ids.extend(fixture.get("sourceRunIds", []))
        evidence_ids.extend(fixture.get("evidenceIds", []))
        procedure_ids.extend(fixture.get("procedureIds", []))
        degraded_codes.extend(fixture.get("degradedCodes", []))
        verification_statuses.append(fixture.get("verificationStatus", "unknown"))
        redaction_statuses.append(fixture.get("redactionStatus", "unknown"))
        degraded_statuses.append(fixture.get("degradedStatus", "unknown"))
        render_only_statuses.append(fixture.get("renderOnlyExportStatus", "unknown"))
        output_artifacts.append(fixture.get("outputArtifactPath", ""))
        saw_verify = saw_verify or any(
            "ee procedure verify" in command for command in fixture.get("eeCommands", [])
        )
        saw_export = saw_export or any(
            "ee procedure export" in command for command in fixture.get("eeCommands", [])
        )
        saw_render_only = saw_render_only or fixture.get("renderOnlyExportStatus") == "render_only"

        if not fixture.get("fixtureHash", "").startswith("blake3:"):
            return fail(f"{fixture_id} fixtureHash must use blake3 prefix", log)
        if not fixture.get("evidenceBundleHash", "").startswith("blake3:"):
            return fail(f"{fixture_id} evidenceBundleHash must use blake3 prefix", log)
        if not fixture.get("evidenceBundlePath", "").startswith("target/e2e/skills/"):
            return fail(f"{fixture_id} evidence bundle path must stay under target/e2e/skills", log)
        if not fixture.get("outputArtifactPath", "").startswith("target/e2e/skills/"):
            return fail(f"{fixture_id} output artifact path must stay under target/e2e/skills", log)
        if fixture.get("requiredSectionCheck") != "passed":
            return fail(f"{fixture_id} must log required-section check", log)
        if fixture.get("rawSecretsIncluded"):
            return fail(f"{fixture_id} must not include raw secrets", log)

        if fixture_id == "pd_insufficient_evidence":
            if fixture.get("sourceRunIds") or fixture.get("evidenceIds"):
                return fail("insufficient evidence fixture must omit source and evidence IDs", log)
            if fixture.get("expectedDisposition") != "refuse":
                return fail("insufficient evidence fixture must refuse drafting", log)
            if fixture.get("degradedCodes") != ["procedure_source_evidence_missing"]:
                return fail("insufficient evidence fixture must name source evidence blocker", log)
        elif not fixture.get("procedureIds"):
            return fail(f"{fixture_id} must log procedure IDs once procedure evidence exists", log)

        if fixture_id == "pd_recorder_derived_draft":
            if fixture.get("verificationStatus") != "missing":
                return fail("recorder-derived draft must preserve missing verification", log)
            if fixture.get("expectedDisposition") != "draft_only":
                return fail("recorder-derived draft must remain draft-only", log)
        if fixture_id == "pd_failed_verification":
            if fixture.get("verificationStatus") != "failed":
                return fail("failed verification fixture must preserve failed status", log)
            if fixture.get("expectedDisposition") != "refuse_promotion":
                return fail("failed verification fixture must refuse promotion", log)
        if fixture_id == "pd_render_only_export_logging":
            if fixture.get("verificationStatus") != "passed":
                return fail("render-only export fixture must have passed verification", log)
            if fixture.get("renderOnlyExportStatus") != "render_only":
                return fail("render-only export fixture must log render_only", log)
        if fixture_id == "pd_degraded_procedure_output":
            if "procedure_store_unavailable" not in fixture.get("degradedCodes", []):
                return fail("degraded procedure fixture must preserve procedure_store_unavailable", log)
            if fixture.get("expectedDisposition") != "refuse_promotion":
                return fail("degraded procedure fixture must refuse promotion", log)

        if fixture.get("expectedDisposition") in {"refuse", "refuse_promotion"}:
            if fixture.get("firstFailureDiagnosis") is None:
                return fail(f"{fixture_id} needs firstFailureDiagnosis", log)

    if not saw_verify or not saw_export:
        return fail("fixtures must include verify and export command paths", log)
    log["followUpCommandCheck"] = "passed"

    if not saw_render_only or "installMode: render_only" not in REFERENCES[2].read_text(encoding="utf-8"):
        return fail("render-only export review must be covered by fixtures and template", log)
    log["renderOnlyExportCheck"] = "passed"

    log["sourceRunIds"] = sorted(set(source_run_ids))
    log["evidenceIds"] = sorted(set(evidence_ids))
    log["procedureIds"] = sorted(set(procedure_ids))
    log["degradedCodes"] = sorted(set(degraded_codes))
    log["verificationStatuses"] = sorted(set(verification_statuses))
    log["redactionStatuses"] = sorted(set(redaction_statuses))
    log["degradedStatuses"] = sorted(set(degraded_statuses))
    log["renderOnlyExportStatuses"] = sorted(set(render_only_statuses))
    log["outputArtifactPaths"] = sorted(set(output_artifacts))
    log["evidenceBundleHash"] = digest_path(FIXTURES)

    write_log(log)
    print(json.dumps(log, sort_keys=True))
    return 0


if __name__ == "__main__":
    sys.exit(main())
