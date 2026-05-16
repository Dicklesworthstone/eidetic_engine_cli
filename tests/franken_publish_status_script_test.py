#!/usr/bin/env python3
"""Fixture tests for scripts/franken_publish_status.py."""

from __future__ import annotations

import importlib.util
import json
import subprocess
import sys
import unittest
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[1]
SCRIPT_PATH = REPO_ROOT / "scripts" / "franken_publish_status.py"
FIXTURES = REPO_ROOT / "tests" / "fixtures" / "franken_publish_status"


def load_module():
    spec = importlib.util.spec_from_file_location("franken_publish_status", SCRIPT_PATH)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"could not load {SCRIPT_PATH}")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


class FrankenPublishStatusScriptTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.mod = load_module()

    def test_crates_io_payload_classifier_distinguishes_statuses(self) -> None:
        available = self.mod.classify_crates_io_payload(
            "fnx-runtime",
            "0.1.0",
            200,
            json.dumps(
                {
                    "crate": {"newest_version": "0.1.0", "repository": "https://example.test/fnx"},
                    "versions": [{"num": "0.1.0", "yanked": False}],
                }
            ),
        )
        self.assertEqual(available.status, "available")

        missing = self.mod.classify_crates_io_payload("fnx-runtime", "0.1.0", 404, "{}")
        self.assertEqual(missing.status, "missing")

        wrong = self.mod.classify_crates_io_payload(
            "fnx-runtime",
            "0.1.0",
            200,
            json.dumps(
                {
                    "crate": {"newest_version": "0.0.9"},
                    "versions": [{"num": "0.0.9", "yanked": False}],
                }
            ),
        )
        self.assertEqual(wrong.status, "wrong_version")

        unavailable = self.mod.classify_crates_io_payload("fnx-runtime", "0.1.0", 599, "timeout")
        self.assertEqual(unavailable.status, "network_unavailable")

    def test_workflow_publish_order_parser_covers_dependency_order(self) -> None:
        workflow = (FIXTURES / "fnx_release_workflow_excerpt.yml").read_text(encoding="utf-8")
        order = self.mod.parse_publish_order(workflow)
        self.assertEqual(
            order,
            [
                "fnx-runtime",
                "fnx-cgse",
                "fnx-classes",
                "fnx-dispatch",
                "fnx-convert",
                "fnx-algorithms",
            ],
        )

    def test_fixture_run_emits_golden_fnx_missing_status(self) -> None:
        output = subprocess.check_output(
            [
                sys.executable,
                str(SCRIPT_PATH),
                "--group",
                "fnx",
                "--fixtures-dir",
                str(FIXTURES / "api_missing"),
                "--root-override",
                str(FIXTURES / "fnx_repo"),
                "--generated-at",
                "2026-05-16T00:00:00Z",
                "--no-git-status",
            ],
            text=True,
        )
        report = json.loads(output)
        golden = json.loads((FIXTURES / "fnx_all_missing_golden.json").read_text(encoding="utf-8"))
        self.assertEqual(report, golden)

    def test_markdown_summary_is_beads_ready_and_redaction_safe(self) -> None:
        output = subprocess.check_output(
            [
                sys.executable,
                str(SCRIPT_PATH),
                "--group",
                "fnx",
                "--fixtures-dir",
                str(FIXTURES / "api_missing"),
                "--root-override",
                str(FIXTURES / "fnx_repo"),
                "--generated-at",
                "2026-05-16T00:00:00Z",
                "--no-git-status",
                "--markdown",
            ],
            text=True,
        )
        self.assertIn("franken_networkx", output)
        self.assertIn("crates_io_missing", output)
        self.assertNotIn("/Users/", output)
        self.assertNotIn("CARGO_REGISTRY_TOKEN=", output)


if __name__ == "__main__":
    unittest.main()
