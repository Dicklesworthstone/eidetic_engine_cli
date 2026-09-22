#!/usr/bin/env python3
"""Exercise the shell assertion oracle, not doctor repairs, with a test double.

Run directly or through scripts/run-safety-harness.sh. All workspaces and command
receipts are retained; EE_DOCTOR_ASSERTION_TEST_ROOT selects their parent.
"""

import json
import os
from pathlib import Path
import shlex
import subprocess
import sys
import tempfile
import unittest


HERE = Path(__file__).resolve().parent
FM = "fm-workspace_config-config-toml-malformed"
CORRUPT = b"broken = [\n"
REPAIRED = b"[workspace]\nname = 'repaired'\n"


def doctor_double():
    """Model independent report, process-exit, and byte-restoration outcomes."""
    scenario = json.loads(Path(os.environ["DOCTOR_ASSERTION_SCENARIO"]).read_text())
    args = sys.argv[2:]
    workspace = Path(args[args.index("--workspace") + 1])
    phase = next(flag for flag in ("--fix", "--only", "--undo") if flag in args)
    with Path(os.environ["DOCTOR_ASSERTION_CALLS"]).open("a", encoding="utf-8") as handle:
        handle.write(json.dumps({"phase": phase, "args": args}) + "\n")
    exit_code = scenario.get("exits", {}).get(phase, 0)
    if exit_code:
        return exit_code
    if phase == "--fix":
        (workspace / ".ee/config.toml").write_bytes(REPAIRED)
        if scenario.get("extra_file"):
            (workspace / "unexpected-file").write_bytes(b"not restored\n")
        if scenario.get("excluded_noise"):
            for name in (".doctor/run.json", ".fixture_baseline/capture.json",
                         ".ee/doctor-fixtures/noise.json", ".assert.stdout",
                         ".assert.stderr", "._sidecar"):
                path = workspace / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(b"audit-only noise\n")
        data = {"schema": "ee.doctor.fix_summary.v1", "actionCount": 1}
        if not scenario.get("missing_run_id"):
            data["runId"] = "assertion-control"
    elif phase == "--only":
        print(scenario["report"])
        return 0
    else:
        if not scenario.get("wrong_bytes"):
            (workspace / ".ee/config.toml").write_bytes(CORRUPT)
        data = {"schema": "ee.doctor.undo_summary.v1", "runId": "assertion-control",
                "status": "undone", "actionsUndone": 1, "firstError": None}
    print(json.dumps({"schema": "ee.response.v2", "success": True,
                      "data": data, "degraded": []}))
    return 0


class AssertionContract(unittest.TestCase):
    def setUp(self):
        evidence_root = os.environ.get("EE_DOCTOR_ASSERTION_TEST_ROOT")
        if evidence_root:
            Path(evidence_root).mkdir(parents=True, exist_ok=True)
        self.root = Path(tempfile.mkdtemp(prefix=self._testMethodName + "-", dir=evidence_root))
        self.workspace = self.root / "workspace"
        (self.workspace / ".ee").mkdir(parents=True)
        (self.workspace / ".ee/config.toml").write_bytes(CORRUPT)
        # Real bytes and awkward filenames must survive a correct undo too.
        (self.workspace / "binary file\nwith newline").write_bytes(bytes(range(256)))
        self.scenario = {}
        self.report = {
            "schema": "ee.response.v2", "success": True, "degraded": [],
            "data": {"command": "doctor", "mode": "concise", "posture": "ok",
                     "healthy": True, "actionable": [], "coreChecks": [
                         {"name": "workspace_config", "tier": "core",
                          "severity": "ok", "message": "configuration parses"}]}}
        double = self.root / "doctor-double"
        double.write_text("#!/usr/bin/env bash\nexec " + shlex.quote(sys.executable)
                          + " " + shlex.quote(str(Path(__file__).resolve()))
                          + ' --probe "$@"\n')
        double.chmod(0o700)
        self.env = dict(os.environ, EE_DOCTOR_FIXTURE_TARGET=str(self.workspace),
                        EE_DOCTOR_FIXTURE_RUN_EE="1", EE_DOCTOR_FIXTURE_BINARY=str(double),
                        DOCTOR_ASSERTION_SCENARIO=str(self.root / "scenario.json"),
                        DOCTOR_ASSERTION_CALLS=str(self.root / "calls.jsonl"))
        prepared = self.run_script("corrupt.sh")
        self.assertEqual(prepared.returncode, 0, prepared.stderr)

    def run_script(self, name):
        # Negative controls intentionally return nonzero. Retain and assert
        # that status below; never interpolate the command into a shell string.
        result = subprocess.run(["bash", str(HERE / FM / name)], env=self.env,
                                capture_output=True, text=True, timeout=30,
                                shell=False, check=False)
        (self.root / (name + ".receipt.json")).write_text(json.dumps({
            "command": ["bash", str(HERE / FM / name)], "exit": result.returncode,
            "stdout": result.stdout, "stderr": result.stderr}, indent=2) + "\n")
        return result

    def assert_outcome(self, rejected, message=None):
        self.scenario.setdefault("report", json.dumps(self.report))
        (self.root / "scenario.json").write_text(json.dumps(self.scenario))
        result = self.run_script("assert.sh")
        print(f"{self._testMethodName}: helper exit={result.returncode}; evidence={self.root}",
              flush=True)
        if rejected:
            self.assertNotEqual(result.returncode, 0, "false green: " + result.stderr)
            if message:
                self.assertIn(message, result.stderr)
        else:
            self.assertEqual(result.returncode, 0, result.stderr)
        return result

    def test_healthy_report_and_restored_bytes_pass(self):
        self.assert_outcome(False)
        self.assertEqual((self.workspace / ".ee/config.toml").read_bytes(), CORRUPT)
        calls = [json.loads(row)["phase"] for row in
                 (self.root / "calls.jsonl").read_text().splitlines()]
        self.assertEqual(calls, ["--fix", "--only", "--undo"])

    def test_unhealthy_report_is_rejected(self):
        self.report["data"].update(posture="blocked", healthy=False)
        self.report["data"]["coreChecks"][0]["severity"] = "error"
        self.assert_outcome(True, "post-fix health")

    def test_degraded_report_is_rejected(self):
        self.report["data"].update(posture="degraded_recoverable", healthy=False)
        self.report["data"]["coreChecks"][0]["severity"] = "warning"
        self.assert_outcome(True, "post-fix health")

    def test_non_ok_check_under_ok_topline_is_rejected(self):
        self.report["data"]["coreChecks"][0]["severity"] = "error"
        self.assert_outcome(True, "post-fix health")

    def test_empty_check_population_is_rejected(self):
        self.report["data"]["coreChecks"] = []
        self.assert_outcome(True, "post-fix health")

    def test_missing_health_is_rejected(self):
        self.report["data"].pop("healthy")
        self.assert_outcome(True, "post-fix health")

    def test_empty_report_is_rejected(self):
        self.scenario["report"] = ""
        self.assert_outcome(True, "post-fix health")

    def test_malformed_report_is_rejected(self):
        self.scenario["report"] = "{not-json"
        self.assert_outcome(True, "post-fix health")

    def test_multiple_reports_are_rejected(self):
        self.scenario["report"] = '{}\n' + json.dumps(self.report)
        self.assert_outcome(True, "post-fix health")

    def test_changed_contents_at_same_path_are_rejected(self):
        self.scenario["wrong_bytes"] = True
        self.assert_outcome(True, "undo contents")

    def test_added_filename_is_rejected(self):
        self.scenario["extra_file"] = True
        self.assert_outcome(True)

    def test_audit_and_capture_noise_is_excluded(self):
        self.scenario["excluded_noise"] = True
        self.assert_outcome(False)

    def test_nonzero_fix_exit_is_rejected(self):
        self.scenario["exits"] = {"--fix": 9}
        self.assertEqual(self.assert_outcome(True).returncode, 9)

    def test_nonzero_report_exit_is_rejected(self):
        self.scenario["exits"] = {"--only": 7}
        self.assertEqual(self.assert_outcome(True).returncode, 7)

    def test_nonzero_undo_exit_is_rejected(self):
        self.scenario["exits"] = {"--undo": 5}
        self.assertEqual(self.assert_outcome(True).returncode, 5)

    def test_missing_run_id_is_rejected(self):
        self.scenario["missing_run_id"] = True
        self.assert_outcome(True, "could not extract runId")


if __name__ == "__main__":
    if sys.argv[1:2] == ["--probe"]:
        sys.exit(doctor_double())
    unittest.main(verbosity=2)
