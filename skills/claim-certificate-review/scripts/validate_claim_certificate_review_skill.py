#!/usr/bin/env python3
"""Validate the claim-certificate-review project-local skill."""

from __future__ import annotations

import hashlib
import json
import sys
from pathlib import Path
from typing import Any


SKILL = Path("skills/claim-certificate-review/SKILL.md")
FIXTURES = Path("skills/claim-certificate-review/fixtures/e2e-fixtures.json")
REFERENCES = [
    Path("skills/claim-certificate-review/references/claim-review-template.md"),
    Path("skills/claim-certificate-review/references/certificate-evidence-checklist-template.md"),
]
LOG_PATH = Path("target/e2e/skills/claim-certificate-review.json")

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
    "ee --workspace <workspace> --json claim show <claim-id> --claims-file <path> --include-manifest",
    "ee --workspace <workspace> --json claim verify <claim-id> --claims-file <path> --artifacts-dir <path> --fail-fast",
    "ee --workspace <workspace> --json claim verify all --claims-file <path> --artifacts-dir <path>",
    "ee --workspace <workspace> --json certificate show <certificate-id> --manifest <path>",
    "ee --workspace <workspace> --json certificate verify <certificate-id> --manifest <path>",
    "ee --workspace <workspace> --json certificate list --manifest <path>",
    "ee --workspace <workspace> --json schema export ee.claim_verify.v1",
    "ee --workspace <workspace> --json schema export ee.certificate.verify.v1",
]

REQUIRED_TEMPLATE_FIELDS = [
    "verifiedFacts",
    "failedStaleChecks",
    "assumptions",
    "overclaimRisks",
    "missingEvidence",
    "followUpCommands",
    "claimIds",
    "certificateIds",
    "manifestPaths",
    "hashStatus",
    "schemaStatus",
    "expiryStatus",
    "assumptionStatus",
    "redactionStatus",
    "degradedState",
    "mayStrengthenClaim",
]

REQUIRED_FIXTURES = {
    "ccr_verified",
    "ccr_stale_payload",
    "ccr_stale_schema",
    "ccr_expired_certificate",
    "ccr_missing_manifest",
    "ccr_failed_assumption",
    "ccr_redacted_evidence",
    "ccr_malformed_verification_output",
    "ccr_degraded_verification",
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
        "schema": "ee.skill.claim_certificate_review.e2e_log.v1",
        "skillPath": str(SKILL),
        "requiredFiles": [str(SKILL), *map(str, REFERENCES), str(FIXTURES)],
        "referencedEeCommands": REQUIRED_COMMANDS,
        "fixtureIds": [],
        "fixtureHashes": {},
        "claimIds": [],
        "certificateIds": [],
        "manifestPaths": [],
        "evidenceIds": [],
        "evidenceBundlePath": str(FIXTURES),
        "evidenceBundleHash": None,
        "hashStatuses": [],
        "schemaStatuses": [],
        "expiryStatuses": [],
        "assumptionStatuses": [],
        "redactionStatuses": [],
        "trustClasses": [],
        "degradedCodes": [],
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
    reference_text = "\n".join(path.read_text(encoding="utf-8") for path in REFERENCES)
    combined_template_text = skill_text + "\n" + reference_text
    if not skill_text.startswith("---\nname: claim-certificate-review\n"):
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
        if field not in combined_template_text:
            return fail(f"missing output template field: {field}", log)
    log["templateFieldCheck"] = "passed"

    for phrase in [
        "Refuse to strengthen a claim when hash/schema verification is missing or degraded.",
        "claim_certificate_evidence_missing",
        "claim_certificate_json_unavailable",
        "claim_certificate_manifest_missing",
        "claim_certificate_hash_unverified",
        "claim_certificate_schema_stale",
        "claim_certificate_expired",
        "claim_certificate_assumption_failed",
        "rawSecretsIncluded=true",
        "ee.skill_evidence_bundle.v1",
        "direct DB",
        "prompt-injection",
        "trust class",
        "Durable memory mutation is forbidden",
        "stale payload",
        "expired certificate",
        "failed assumption",
        "degraded verification",
    ]:
        if phrase not in skill_text:
            return fail(f"missing evidence gate or safety phrase: {phrase}", log)
    log["evidenceGateCheck"] = "passed"

    try:
        fixtures = json.JSONDecoder().decode(FIXTURES.read_text(encoding="utf-8"))
    except json.JSONDecodeError as exc:
        return fail(f"fixture JSON is malformed: {exc.msg}", log)
    if fixtures.get("schema") != "ee.skill.claim_certificate_review.fixtures.v1":
        return fail("fixture schema is missing or wrong", log)

    fixture_list = fixtures.get("fixtures", [])
    seen = {fixture["id"] for fixture in fixture_list}
    missing_fixtures = sorted(REQUIRED_FIXTURES - seen)
    if missing_fixtures:
        return fail(f"missing fixture: {missing_fixtures[0]}", log)

    command_error = all_referenced_commands_are_machine_json(fixture_list)
    if command_error is not None:
        return fail(command_error, log)

    claim_ids: list[str] = []
    certificate_ids: list[str] = []
    manifest_paths: list[str] = []
    evidence_ids: list[str] = []
    hash_statuses: list[str] = []
    schema_statuses: list[str] = []
    expiry_statuses: list[str] = []
    assumption_statuses: list[str] = []
    redaction_statuses: list[str] = []
    trust_classes: list[str] = []
    degraded_codes: list[str] = []

    for fixture in fixture_list:
        fixture_id = fixture["id"]
        log["fixtureIds"].append(fixture_id)
        log["fixtureHashes"][fixture_id] = fixture_hash(fixture)
        claim_ids.extend(fixture.get("claimIds", []))
        certificate_ids.extend(fixture.get("certificateIds", []))
        manifest_paths.extend(fixture.get("manifestPaths", []))
        evidence_ids.extend(fixture.get("evidenceIds", []))
        degraded_codes.extend(fixture.get("degradedCodes", []))
        hash_statuses.append(fixture.get("hashStatus", "unknown"))
        schema_statuses.append(fixture.get("schemaStatus", "unknown"))
        expiry_statuses.append(fixture.get("expiryStatus", "unknown"))
        assumption_statuses.append(fixture.get("assumptionStatus", "unknown"))
        redaction_statuses.append(fixture.get("redactionStatus", "unknown"))
        trust_classes.append(fixture.get("trustClass", "unknown"))

        if fixture_id == "ccr_verified":
            if fixture["expectedDisposition"] != "review_supported" or not fixture["mayStrengthenClaim"]:
                return fail("verified fixture must allow bounded claim strengthening", log)
            if fixture["hashStatus"] != "passed" or fixture["schemaStatus"] != "passed":
                return fail("verified fixture must have passed hash and schema", log)
        if fixture_id == "ccr_stale_payload":
            if fixture["hashStatus"] != "stale":
                return fail("stale payload fixture must report stale hash", log)
        if fixture_id == "ccr_stale_schema":
            if fixture["schemaStatus"] != "stale":
                return fail("stale schema fixture must report stale schema", log)
        if fixture_id == "ccr_expired_certificate":
            if fixture["expiryStatus"] != "expired":
                return fail("expired certificate fixture must report expired status", log)
        if fixture_id == "ccr_missing_manifest":
            if fixture["manifestPaths"]:
                return fail("missing manifest fixture must not contain manifest paths", log)
        if fixture_id == "ccr_failed_assumption":
            if fixture["assumptionStatus"] != "failed":
                return fail("failed assumption fixture must report failed assumption", log)
        if fixture_id == "ccr_redacted_evidence":
            if not fixture["rawSecretsIncluded"] or fixture["expectedDisposition"] != "refuse":
                return fail("redacted evidence fixture must refuse raw secrets", log)
        if fixture_id == "ccr_malformed_verification_output":
            if not fixture["malformedVerificationOutput"]:
                return fail("malformed verification fixture must mark malformed output", log)
        if fixture_id == "ccr_degraded_verification":
            required_codes = {"claim_verification_unavailable", "certificate_store_unavailable"}
            if not required_codes.issubset(set(fixture["degradedCodes"])):
                return fail("degraded fixture must preserve claim and certificate degraded codes", log)
            if fixture["mayStrengthenClaim"]:
                return fail("degraded fixture must not allow claim strengthening", log)
        if fixture["expectedDisposition"] in {"refuse", "request_evidence", "request_repair", "refuse_strengthen"}:
            if fixture["firstFailureDiagnosis"] is None:
                return fail(f"{fixture_id} needs firstFailureDiagnosis", log)
            if fixture["mayStrengthenClaim"]:
                return fail(f"{fixture_id} must not allow claim strengthening", log)

    for command in [
        "ee --workspace <workspace> --json claim verify <claim-id> --claims-file <path> --artifacts-dir <path> --fail-fast",
        "ee --workspace <workspace> --json certificate verify <certificate-id> --manifest <path>",
    ]:
        if command not in skill_text:
            return fail(f"missing follow-up command rendering: {command}", log)
    log["followUpCommandCheck"] = "passed"

    log["claimIds"] = sorted(set(claim_ids))
    log["certificateIds"] = sorted(set(certificate_ids))
    log["manifestPaths"] = sorted(set(manifest_paths))
    log["evidenceIds"] = sorted(set(evidence_ids))
    log["hashStatuses"] = sorted(set(hash_statuses))
    log["schemaStatuses"] = sorted(set(schema_statuses))
    log["expiryStatuses"] = sorted(set(expiry_statuses))
    log["assumptionStatuses"] = sorted(set(assumption_statuses))
    log["redactionStatuses"] = sorted(set(redaction_statuses))
    log["trustClasses"] = sorted(set(trust_classes))
    log["degradedCodes"] = sorted(set(degraded_codes))
    log["evidenceBundleHash"] = digest_path(FIXTURES)

    write_log(log)
    print(json.dumps(log, sort_keys=True))
    return 0


if __name__ == "__main__":
    sys.exit(main())
