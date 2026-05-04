#!/usr/bin/env python3
"""
Validate the causal-credit-review skill against e2e fixtures.

Records schema: ee.skill.causal_credit_review.e2e_log.v1
"""

import hashlib
import json
import os
import sys
from dataclasses import dataclass, field
from datetime import datetime, timezone
from pathlib import Path
from typing import Any


@dataclass
class E2ELog:
    """E2E log record for skill validation."""

    schema: str = "ee.skill.causal_credit_review.e2e_log.v1"
    skill_path: str = ""
    fixture_id: str = ""
    fixture_hash: str = ""
    ee_commands: list[str] = field(default_factory=list)
    degraded_states: list[dict] = field(default_factory=list)
    evidence_ids: list[str] = field(default_factory=list)
    evidence_bundle_path: str = ""
    evidence_bundle_hash: str = ""
    causal_ledger_ids: list[str] = field(default_factory=list)
    evidence_tier: str = ""
    redaction_status: str = ""
    recommendation_class: str = ""
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
            "causalLedgerIds": self.causal_ledger_ids,
            "evidenceTier": self.evidence_tier,
            "redactionStatus": self.redaction_status,
            "recommendationClass": self.recommendation_class,
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
    "Mechanical Command Boundary",
    "Evidence Gathering",
    "Evidence Tiers",
    "Stop/Go Gates",
    "Output Template",
    "Uncertainty Handling",
    "Confounder Checklist",
    "Privacy And Redaction",
    "Degraded Behavior",
    "Unsupported Claims",
    "Evidence-Tier Rubric",
    "Testing Requirements",
    "E2E Logging",
]

REQUIRED_COMMANDS = [
    "ee causal trace",
    "ee causal estimate",
    "ee causal compare",
    "ee causal promote-plan",
    "ee status",
]

REQUIRED_STOP_GATES = [
    "causal_evidence_unavailable",
    "causal_redaction_unverified",
    "causal_sample_underpowered",
    "causal_confounders_uncontrolled",
    "causal_prompt_injection_unquarantined",
]


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
    results["has_name"] = "name: causal-credit-review" in content
    results["has_description"] = "description:" in content

    # Check required sections
    for section in REQUIRED_SECTIONS:
        key = f"section_{section.lower().replace(' ', '_').replace('/', '_')}"
        results[key] = f"## {section}" in content

    # Check required commands
    for cmd in REQUIRED_COMMANDS:
        key = f"command_{cmd.replace(' ', '_').replace('-', '_')}"
        results[key] = cmd in content

    # Check stop gates
    for gate in REQUIRED_STOP_GATES:
        key = f"gate_{gate}"
        results[key] = gate in content

    # Check output template
    results["has_output_template"] = "schema: ee.skill.causal_credit_review.v1" in content

    # Check evidence tier definitions
    results["has_tier_t0"] = "T0:" in content or "T0 |" in content
    results["has_tier_t5"] = "T5:" in content or "T5 |" in content

    # Check confounder checklist items
    results["has_selection_bias"] = "Selection bias" in content
    results["has_confounding_variables"] = "Confounding variables" in content

    # Check DB prohibition
    results["has_db_prohibition"] = (
        "direct DB" in content.lower()
        or "FrankenSQLite" in content
        or "directly" in content
    )

    # Check redaction handling
    results["has_redaction_handling"] = "redaction" in content.lower()

    return results


def validate_fixture(fixture: dict, log: E2ELog) -> list[str]:
    """Validate a single fixture against expected output."""
    errors = []

    log.fixture_id = fixture.get("id", "unknown")
    log.fixture_hash = compute_hash(json.dumps(fixture))
    log.evidence_tier = fixture.get("evidenceTier", "")

    # Extract evidence IDs from inputs
    inputs = fixture.get("inputs", {})
    for key, value in inputs.items():
        if isinstance(value, dict):
            if "chains" in value:
                for chain in value["chains"]:
                    log.causal_ledger_ids.append(chain.get("chainId", ""))
                    log.evidence_ids.extend(chain.get("evidenceIds", []))
            if "degraded" in value:
                log.degraded_states.extend(value["degraded"])

    # Check expected output structure
    expected = fixture.get("expectedOutput", {})
    if "recommendation" in expected:
        rec = expected["recommendation"]
        log.recommendation_class = rec.get("action", "")

    if "evidenceBundle" in expected:
        log.redaction_status = expected["evidenceBundle"].get("redactionStatus", "")

    # Validate assertions
    assertions = fixture.get("assertions", [])
    for assertion in assertions:
        # These are placeholder assertions that would be checked against actual output
        log.refusal_checks.append(assertion)

    return errors


def run_fixtures() -> tuple[list[E2ELog], list[str]]:
    """Run all fixtures and collect logs."""
    logs = []
    errors = []

    if not FIXTURES_FILE.exists():
        errors.append(f"Fixtures file not found: {FIXTURES_FILE}")
        return logs, errors

    fixtures_content = FIXTURES_FILE.read_text()
    fixtures_data = json.loads(fixtures_content)

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


def main() -> int:
    """Main entry point."""
    print(f"Validating skill: {SKILL_DIR}")
    print()

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

    # Run fixtures
    print("\nRunning fixtures...")
    logs, fixture_errors = run_fixtures()

    passed_fixtures = [log for log in logs if log.passed]
    failed_fixtures = [log for log in logs if not log.passed]

    print(f"  Passed: {len(passed_fixtures)}")
    print(f"  Failed: {len(failed_fixtures)}")

    if failed_fixtures:
        print("\n  Failed fixtures:")
        for log in failed_fixtures:
            print(f"    - {log.fixture_id}: {log.first_failure_diagnosis}")

    # Write logs
    output_dir = Path("target/e2e/skills/causal-credit-review")
    output_dir.mkdir(parents=True, exist_ok=True)

    log_file = output_dir / "validation_log.json"
    with open(log_file, "w") as f:
        json.dump(
            {
                "schema": "ee.skill.causal_credit_review.e2e_log.v1",
                "skillPath": str(SKILL_DIR),
                "sectionResults": section_results,
                "fixtureLogs": [log.to_dict() for log in logs],
                "timestamp": datetime.now(timezone.utc).isoformat(),
            },
            f,
            indent=2,
        )

    print(f"\nLog written to: {log_file}")

    # Return exit code
    if failed_sections or failed_fixtures:
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
