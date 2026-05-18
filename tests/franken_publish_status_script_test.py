#!/usr/bin/env python3
"""Fixture tests for scripts/franken_publish_status.py."""

from __future__ import annotations

import importlib.util
import io
import json
import subprocess
import sys
import tempfile
import unittest
from contextlib import redirect_stderr
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

    def test_fnx_publish_order_requires_generators_after_algorithms(self) -> None:
        expected = self.mod.GROUPS["fnx"]["expected_publish_order"]
        self.assertIn("fnx-generators", expected)
        self.assertLess(expected.index("fnx-algorithms"), expected.index("fnx-generators"))

    def test_workflow_tag_gate_accepts_generic_tag_refs(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            workflow_dir = root / ".github" / "workflows"
            workflow_dir.mkdir(parents=True)
            (workflow_dir / "release.yml").write_text(
                """
name: release
jobs:
  publish:
    if: ${{ (startsWith(github.ref, 'refs/tags/') || github.event_name == 'workflow_dispatch') && !contains(github.ref, '-') }}
    steps:
      - run: |
          crates=(sqlmodel-frankensqlite sqlmodel)
          cargo publish -p "${crate}"
        env:
          CARGO_REGISTRY_TOKEN: ${{ secrets.CARGO_REGISTRY_TOKEN }}
""",
                encoding="utf-8",
            )

            status = self.mod.workflow_status(
                root,
                ".github/workflows/release.yml",
                ["sqlmodel-frankensqlite", "sqlmodel"],
            )

        self.assertTrue(status["tag_gate"])
        self.assertEqual(status["missing_from_publish_order"], [])
        self.assertTrue(status["dependency_order_ok"])

    def test_sqlmodel_expected_publish_order_puts_driver_before_umbrella(self) -> None:
        expected = self.mod.GROUPS["sqlmodel"]["expected_publish_order"]
        self.assertLess(expected.index("sqlmodel-frankensqlite"), expected.index("sqlmodel"))

    def test_fixture_run_emits_golden_fnx_missing_status(self) -> None:
        # Run script with default arguments
        # nosec B603
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
        self.assertEqual(
            report["aggregate"],
            {
                "all_required_crates_ready": False,
                "blocked_count": 7,
                "crate_count": 7,
                "group_count": 1,
                "missing_count": 7,
                "network_unavailable_count": 0,
                "ready_count": 0,
                "wrong_version_count": 0,
            },
        )
        group = report["groups"][0]
        self.assertIn("fnx-generators", group["workflow"]["missing_from_publish_order"])
        generator = next(crate for crate in group["crates"] if crate["crate_name"] == "fnx-generators")
        self.assertEqual(generator["local_manifest"]["status"], "ok")
        self.assertEqual(generator["local_manifest"]["version"], "0.1.0")
        self.assertEqual(generator["crates_io"]["status"], "missing")
        self.assertIn("crates_io_missing", generator["blocking_reasons"])
        self.assertIn("workflow_missing_publish_crate", generator["blocking_reasons"])

    def test_markdown_summary_is_beads_ready_and_redaction_safe(self) -> None:
        # Run script with format arg
        # nosec B603
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
        self.assertIn("Aggregate: `0/7` crates ready; `7` blocked", output)
        self.assertIn("`7` missing, `0` wrong-version, `0` network-unavailable", output)
        self.assertIn("crates_io_missing", output)
        self.assertNotIn("/Users/", output)
        self.assertNotIn("CARGO_REGISTRY_TOKEN=", output)

    def test_markdown_aggregate_counts_multiple_groups(self) -> None:
        report = {
            "schema": "ee.franken_publish_status.v1",
            "generated_at": "2026-05-16T00:00:00Z",
            "groups": [
                {
                    "display_name": "first",
                    "group": "first",
                    "summary": {
                        "crate_count": 3,
                        "ready_count": 1,
                        "blocked_count": 2,
                        "missing_count": 1,
                        "wrong_version_count": 1,
                        "network_unavailable_count": 0,
                    },
                    "blocking_reason": "blocked",
                    "git": {"checked": False},
                    "workflow": {
                        "publish_job_present": True,
                        "tag_gate": True,
                        "dependency_order_ok": True,
                    },
                    "crates": [],
                },
                {
                    "display_name": "second",
                    "group": "second",
                    "summary": {
                        "crate_count": 2,
                        "ready_count": 1,
                        "blocked_count": 1,
                        "missing_count": 0,
                        "wrong_version_count": 0,
                        "network_unavailable_count": 1,
                    },
                    "blocking_reason": "blocked",
                    "git": {"checked": False},
                    "workflow": {
                        "publish_job_present": True,
                        "tag_gate": True,
                        "dependency_order_ok": True,
                    },
                    "crates": [],
                },
            ],
        }

        output = self.mod.render_markdown(report)

        self.assertIn("Aggregate: `2/5` crates ready; `3` blocked", output)
        self.assertIn("`1` missing, `1` wrong-version, `1` network-unavailable", output)

    def test_json_aggregate_counts_multiple_groups(self) -> None:
        groups = [
            {
                "summary": {
                    "crate_count": 3,
                    "ready_count": 1,
                    "blocked_count": 2,
                    "missing_count": 1,
                    "wrong_version_count": 1,
                    "network_unavailable_count": 0,
                }
            },
            {
                "summary": {
                    "crate_count": 2,
                    "ready_count": 1,
                    "blocked_count": 1,
                    "missing_count": 0,
                    "wrong_version_count": 0,
                    "network_unavailable_count": 1,
                }
            },
        ]

        self.assertEqual(
            self.mod.aggregate_summary(groups),
            {
                "all_required_crates_ready": False,
                "blocked_count": 3,
                "crate_count": 5,
                "group_count": 2,
                "missing_count": 1,
                "network_unavailable_count": 1,
                "ready_count": 2,
                "wrong_version_count": 1,
            },
        )

    def test_parse_args_all_groups_selects_every_known_group(self) -> None:
        args = self.mod.parse_args(["--all-groups"])
        self.assertEqual(
            self.mod.selected_groups(args),
            sorted(self.mod.GROUPS),
        )

    def test_parse_args_rejects_all_groups_with_explicit_group(self) -> None:
        with redirect_stderr(io.StringIO()), self.assertRaises(SystemExit):
            self.mod.parse_args(["--all-groups", "--group", "fnx"])


if __name__ == "__main__":
    unittest.main()
