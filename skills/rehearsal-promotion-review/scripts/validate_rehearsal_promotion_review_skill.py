#!/usr/bin/env python3
"""
Validate the rehearsal-promotion-review skill against e2e fixtures.

Records schema: ee.skill.rehearsal_promotion_review.e2e_log.v1
"""

import hashlib
import json
import sys
from dataclasses import dataclass, field
from datetime import datetime, timezone
from pathlib import Path
from typing import Any


@dataclass
class E2ELog:
    """E2E log record for skill validation."""

    schema: str = "ee.skill.rehearsal_promotion_review.e2e_log.v1"
    skill_path: str = ""
    fixture_id: str = ""
    fixture_hash: str = ""
    ee_commands: list[str] = field(default_factory=list)
    degraded_states: list[dict] = field(default_factory=list)
    evidence_ids: list[str] = field(default_factory=list)
    evidence_bundle_path: str = ""
    evidence_bundle_hash: str = ""
    rehearsal_artifact_ids: list[str] = field(default_factory=list)
    rehearsal_artifact_paths: list[str] = field(default_factory=list)
    mutation_posture: str = ""
    redaction_status: str = ""
    go_no_go_class: str = ""
    output_artifact_path: str = ""
    required_section_check: dict = field(default_factory=dict)
    refusal_checks: list[str] = field(default_factory=list)
    first_failure_diagnosis: str = ""
    passed: bool = False
    timestamp: str = ""

    def to_dict(self) -> dict:
        return {
            "schema": self.schema,
            "skillPath": self.skill_path,
            "fixtureId": self.fixture_id,
            "fixtureHash": self.fixture_hash,
            "eeCommands": self.ee_commands,
            "degradedStates": self.degraded_states,
            "evidenceIds": self.evidence_ids,
            "evidenceBundlePath": self.evidence_bundle_path,
            "evidenceBundleHash": self.evidence_bundle_hash,
            "rehearsalArtifactIds": self.rehearsal_artifact_ids,
            "rehearsalArtifactPaths": self.rehearsal_artifact_paths,
            "mutationPosture": self.mutation_posture,
            "redactionStatus": self.redaction_status,
            "goNoGoClass": self.go_no_go_class,
            "outputArtifactPath": self.output_artifact_path,
            "requiredSectionCheck": self.required_section_check,
            "refusalChecks": self.refusal_checks,
            "firstFailureDiagnosis": self.first_failure_diagnosis,
            "passed": self.passed,
            "timestamp": self.timestamp,
        }


SKILL_DIR = Path(__file__).parent.parent
SKILL_MD = SKILL_DIR / "SKILL.md"
FIXTURES_FILE = SKILL_DIR / "fixtures" / "e2e-fixtures.json"

REQUIRED_SECTIONS = [
    "Trigger Conditions",
    "Required Evidence",
    "Stop/Go Gates",
    "Evidence Gathering",
    "Output Template",
    "Uncertainty Handling",
    "Destructive Command Escalation",
    "Degraded Behavior",
    "Unavailable Handling",
    "Testing Requirements",
    "E2E Logging",
]

REQUIRED_COMMANDS = [
    "ee rehearse plan",
    "ee rehearse",  # Covers run/inspect via "ee rehearse plan/run/inspect"
    "ee status",
]

# Alternative patterns that satisfy command requirements
COMMAND_ALTERNATIVES = {
    "ee rehearse plan": ["ee rehearse plan"],
    "ee rehearse": ["ee rehearse", "rehearse plan/run/inspect"],
    "ee status": ["ee status"],
}

REQUIRED_STOP_GATES = [
    "rehearsal_unavailable",
    "ee rehearse",
]

GO_NO_GO_CLASSES = ["go", "no-go", "needs-escalation", "unavailable"]


def compute_hash(content: str) -> str:
    """Compute SHA-256 hash of content."""
    return hashlib.sha256(content.encode()).hexdigest()[:16]


def validate_skill_md() -> dict[str, bool]:
    """Validate SKILL.md has required sections and content."""
    results = {}

    if not SKILL_MD.exists():
        return {"skill_md_exists": False}

    content = SKILL_MD.read_text()
    results["skill_md_exists"] = True

    # Check frontmatter
    results["has_frontmatter"] = content.startswith("---")
    results["has_name"] = "name: rehearsal-promotion-review" in content
    results["has_description"] = "description:" in content

    # Check required sections
    for section in REQUIRED_SECTIONS:
        key = f"section_{section.lower().replace(' ', '_').replace('/', '_')}"
        results[key] = f"## {section}" in content

    # Check required commands (with alternative patterns)
    for cmd in REQUIRED_COMMANDS:
        key = f"command_{cmd.replace(' ', '_').replace('-', '_')}"
        alternatives = COMMAND_ALTERNATIVES.get(cmd, [cmd])
        results[key] = any(alt in content for alt in alternatives)

    # Check stop gates
    for gate in REQUIRED_STOP_GATES:
        key = f"gate_{gate.replace(' ', '_').replace('-', '_')}"
        results[key] = gate in content

    # Check output template
    results["has_output_template"] = "Rehearsal Review Summary" in content

    # Check go/no-go decision coverage
    results["has_go_no_go_decision"] = "Go/No-Go Recommendation" in content

    # Check destructive command handling
    results["has_destructive_warning"] = "destructive" in content.lower()
    results["has_escalation"] = "needs-escalation" in content

    # Check unavailable handling
    results["has_unavailable_handling"] = "rehearsal_unavailable" in content

    # Check redaction handling
    results["has_redaction_handling"] = "redaction" in content.lower()

    # Check degraded behavior
    results["has_degraded_behavior"] = "degraded" in content.lower()

    return results


def validate_fixture(fixture: dict, log: E2ELog) -> list[str]:
    """Validate a single fixture against expected output."""
    errors = []

    log.fixture_id = fixture.get("id", "unknown")
    log.fixture_hash = compute_hash(json.dumps(fixture, sort_keys=True))
    log.ee_commands = fixture.get("eeCommands", [])
    log.evidence_ids = fixture.get("evidenceIds", [])
    log.rehearsal_artifact_ids = fixture.get("rehearsalArtifactIds", [])
    log.rehearsal_artifact_paths = fixture.get("rehearsalArtifactPaths", [])
    log.mutation_posture = fixture.get("mutationPosture", "")
    log.redaction_status = fixture.get("redactionStatus", "")
    log.go_no_go_class = fixture.get("expectedGoNoGoClass", "")

    # Extract degraded codes
    degraded_codes = fixture.get("degradedCodes", [])
    log.degraded_states = [{"code": code} for code in degraded_codes]

    # Validate required fixture fields
    required_fields = [
        "id",
        "description",
        "taskClass",
        "eeCommands",
        "expectedGoNoGoClass",
    ]
    for field_name in required_fields:
        if field_name not in fixture:
            errors.append(f"Missing required field: {field_name}")

    # Validate go/no-go class is valid
    expected_class = fixture.get("expectedGoNoGoClass", "")
    if expected_class and expected_class not in GO_NO_GO_CLASSES:
        errors.append(f"Invalid expectedGoNoGoClass: {expected_class}")

    # Validate that unavailable fixtures have appropriate degraded codes
    if expected_class == "unavailable":
        if not degraded_codes:
            errors.append("Unavailable fixture should have degraded codes")

    # Validate that needs-escalation fixtures have escalation flag
    if expected_class == "needs-escalation":
        if not fixture.get("expectedEscalationRequired", False):
            errors.append("needs-escalation fixture should have expectedEscalationRequired=true")

    # Validate redaction status for go fixtures
    if expected_class == "go":
        redaction = fixture.get("redactionStatus", "")
        if redaction not in ["passed", "redacted"]:
            errors.append(f"Go fixture should have valid redaction status, got: {redaction}")

    # Validate mutation posture
    posture = fixture.get("mutationPosture", "")
    valid_postures = ["read-only", "dry-run", "live"]
    if posture and posture not in valid_postures:
        errors.append(f"Invalid mutationPosture: {posture}")

    # Record refusal checks
    if fixture.get("firstFailureDiagnosis"):
        log.refusal_checks.append(fixture["firstFailureDiagnosis"])

    return errors


def run_fixtures() -> tuple[list[E2ELog], list[str]]:
    """Run all fixtures and collect logs."""
    logs = []
    errors = []

    if not FIXTURES_FILE.exists():
        errors.append(f"Fixtures file not found: {FIXTURES_FILE}")
        return logs, errors

    fixtures_content = FIXTURES_FILE.read_text()
    try:
        fixtures_data = json.loads(fixtures_content)
    except json.JSONDecodeError as e:
        errors.append(f"Invalid JSON in fixtures file: {e}")
        return logs, errors

    # Validate schema
    schema = fixtures_data.get("schema", "")
    if not schema.startswith("ee.skill.rehearsal_promotion_review"):
        errors.append(f"Unexpected schema: {schema}")

    for fixture in fixtures_data.get("fixtures", []):
        log = E2ELog(
            skill_path=str(SKILL_DIR),
            timestamp=datetime.now(timezone.utc).isoformat(),
        )

        fixture_errors = validate_fixture(fixture, log)
        if fixture_errors:
            log.first_failure_diagnosis = fixture_errors[0]
            errors.extend(fixture_errors)
        else:
            log.passed = True

        logs.append(log)

    return logs, errors


def validate_fixture_coverage(fixtures: list[dict]) -> list[str]:
    """Validate that fixtures cover all required test cases."""
    errors = []

    task_classes = {f.get("taskClass", "") for f in fixtures}
    go_no_go_classes = {f.get("expectedGoNoGoClass", "") for f in fixtures}

    # Required task classes per SKILL.md Testing Requirements
    required_task_classes = [
        "successful-dry-run",
        "partial-failure",
        "unavailable-rehearsal",
        "unsupported-command",
        "redacted-artifact",
        "malformed-artifact",
        "degraded-cli",
        "destructive-promotion",
    ]

    for tc in required_task_classes:
        if tc not in task_classes:
            errors.append(f"Missing required task class: {tc}")

    # Must cover all go/no-go classes
    for gnc in GO_NO_GO_CLASSES:
        if gnc not in go_no_go_classes:
            errors.append(f"Missing go/no-go class coverage: {gnc}")

    return errors


def main() -> int:
    """Main entry point."""
    print(f"Validating skill: {SKILL_DIR}")
    print()

    all_errors = []

    # Validate SKILL.md
    print("Checking SKILL.md...")
    section_results = validate_skill_md()

    failed_sections = [k for k, v in section_results.items() if not v]
    passed_sections = [k for k, v in section_results.items() if v]

    print(f"  Passed: {len(passed_sections)}")
    print(f"  Failed: {len(failed_sections)}")

    if failed_sections:
        print("\n  Missing or invalid:")
        for section in failed_sections:
            print(f"    - {section}")
        all_errors.extend(failed_sections)

    # Run fixtures
    print("\nRunning fixtures...")
    logs, fixture_errors = run_fixtures()
    all_errors.extend(fixture_errors)

    passed_fixtures = [log for log in logs if log.passed]
    failed_fixtures = [log for log in logs if not log.passed]

    print(f"  Passed: {len(passed_fixtures)}")
    print(f"  Failed: {len(failed_fixtures)}")

    if failed_fixtures:
        print("\n  Failed fixtures:")
        for log in failed_fixtures:
            print(f"    - {log.fixture_id}: {log.first_failure_diagnosis}")

    # Validate fixture coverage
    print("\nValidating fixture coverage...")
    if FIXTURES_FILE.exists():
        fixtures_data = json.loads(FIXTURES_FILE.read_text())
        coverage_errors = validate_fixture_coverage(fixtures_data.get("fixtures", []))
        if coverage_errors:
            print(f"  Coverage gaps: {len(coverage_errors)}")
            for err in coverage_errors:
                print(f"    - {err}")
            all_errors.extend(coverage_errors)
        else:
            print("  All required test cases covered")

    # Write logs
    output_dir = Path("target/e2e/skills/rehearsal-promotion-review")
    output_dir.mkdir(parents=True, exist_ok=True)

    log_file = output_dir / "validation_log.json"
    with open(log_file, "w") as f:
        json.dump(
            {
                "schema": "ee.skill.rehearsal_promotion_review.e2e_log.v1",
                "skillPath": str(SKILL_DIR),
                "sectionResults": section_results,
                "fixtureLogs": [log.to_dict() for log in logs],
                "fixtureErrors": fixture_errors,
                "coverageErrors": coverage_errors if FIXTURES_FILE.exists() else [],
                "timestamp": datetime.now(timezone.utc).isoformat(),
                "passed": len(all_errors) == 0,
            },
            f,
            indent=2,
        )

    print(f"\nLog written to: {log_file}")

    # Summary
    print(f"\n{'='*60}")
    if all_errors:
        print(f"FAILED: {len(all_errors)} errors found")
        return 1
    else:
        print("PASSED: All validations successful")
        return 0


if __name__ == "__main__":
    sys.exit(main())
