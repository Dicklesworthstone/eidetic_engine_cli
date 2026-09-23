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
    phase = next((flag for flag in ("--fix", "--only", "--undo") if flag in args), "report")
    with Path(os.environ["DOCTOR_ASSERTION_CALLS"]).open("a", encoding="utf-8") as handle:
        handle.write(json.dumps({"phase": phase, "args": args}) + "\n")
    exit_code = scenario.get("exits", {}).get(phase, 0)
    if exit_code:
        return exit_code
    if phase == "--fix" and "fix_data" in scenario:
        # Guidance-only mode: the double reports what the scenario says the
        # fixers did, and changes the workspace only when told to.
        if scenario.get("create_path"):
            (workspace / scenario["create_path"]).mkdir(parents=True)
        print(json.dumps({"schema": "ee.response.v2", "success": True,
                          "data": scenario["fix_data"], "degraded": []}))
        return 0
    if phase == "--fix":
        (workspace / ".ee/config.toml").write_bytes(REPAIRED)
        if scenario.get("extra_file"):
            (workspace / "unexpected-file").write_bytes(b"not restored\n")
        if scenario.get("excluded_noise"):
            for name in (".doctor/run.json", ".fixture_baseline/capture.json",
                         ".ee/doctor-fixtures/noise.json", ".assert.stdout",
                         ".assert.stderr", "._sidecar", ".ee/ee.db-shm"):
                path = workspace / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(b"audit-only noise\n")
        if scenario.get("wal_changed"):
            # A source write that lands only in the WAL: undo does not
            # restore it, so the round trip must fail.
            (workspace / ".ee/ee.db-wal").write_bytes(b"source frame written by the fix\n")
        if "write_lock" in scenario:
            (workspace / ".ee/ee.write.lock").write_bytes(scenario["write_lock"].encode())
        if scenario.get("write_lock_missing"):
            # Move aside (never unlink) into the excluded run directory.
            moved = workspace / ".doctor/moved-write.lock"
            moved.parent.mkdir(parents=True, exist_ok=True)
            os.replace(workspace / ".ee/ee.write.lock", moved)
        data = {"schema": "ee.doctor.fix_summary.v1", "actionCount": 1}
        if not scenario.get("missing_run_id"):
            data["runId"] = "assertion-control"
    elif phase in ("--only", "report"):
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
        # A real store carries a monotonic write-lock epoch (20 digits + LF).
        (self.workspace / ".ee/ee.write.lock").write_bytes(b"00000000000000000005\n")
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

    def test_source_wal_change_is_rejected(self):
        # ee.db-shm is excluded and ee.write.lock is classified; the WAL holds
        # source frames and must stay byte-compared.
        self.scenario["wal_changed"] = True
        self.assert_outcome(True, "undo contents")

    def test_write_lock_epoch_advance_is_accepted(self):
        self.scenario["write_lock"] = "00000000000000000007\n"
        self.assert_outcome(False)

    def test_write_lock_epoch_regression_is_rejected(self):
        self.scenario["write_lock"] = "00000000000000000003\n"
        self.assert_outcome(True, "went backwards")

    def test_unreadable_write_lock_is_rejected(self):
        self.scenario["write_lock"] = "not-an-epoch\n"
        self.assert_outcome(True, "missing or unreadable")

    def test_short_write_lock_is_rejected(self):
        # 20 digits with no trailing newline is not the stored format.
        self.scenario["write_lock"] = "00000000000000000009"
        self.assert_outcome(True, "missing or unreadable")

    def test_missing_write_lock_is_rejected(self):
        self.scenario["write_lock_missing"] = True
        self.assert_outcome(True, "missing or unreadable")

    def test_lock_created_when_absent_at_baseline_is_rejected(self):
        # Move the seeded lock out of the workspace (never unlink), then take
        # a fresh baseline that records it as absent.
        os.replace(self.workspace / ".ee/ee.write.lock", self.root / "moved-baseline.lock")
        prepared = self.run_script("corrupt.sh")
        self.assertEqual(prepared.returncode, 0, prepared.stderr)
        self.scenario["write_lock"] = "00000000000000000001\n"
        self.assert_outcome(True, "absent at the baseline")

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


GUIDANCE_FM = "fm-search_indexes-index_missing"


class GuidanceOnlyContract(unittest.TestCase):
    """doctor_fixture_assert_guidance_only (bd-2oh15 decision C): the fix must
    record guidance, never claim a repair, and leave the finding in place."""

    def setUp(self):
        evidence_root = os.environ.get("EE_DOCTOR_ASSERTION_TEST_ROOT")
        if evidence_root:
            Path(evidence_root).mkdir(parents=True, exist_ok=True)
        self.root = Path(tempfile.mkdtemp(prefix=self._testMethodName + "-", dir=evidence_root))
        self.workspace = self.root / "workspace"
        (self.workspace / ".ee").mkdir(parents=True)
        self.scenario = {
            "fix_data": {
                "schema": "ee.doctor.fix_summary.v1", "runId": "guidance-control",
                "guidanceOnlyFixerCount": 1,
                "fixerResults": [{"findingCode": "search_index_missing",
                                  "operation": "run_index_rebuild",
                                  "outcome": "guidance_recorded", "actionSequence": 1}]},
        }
        self.report = {
            "schema": "ee.response.v2", "success": True, "degraded": [],
            "data": {"command": "doctor", "posture": "degraded_recoverable",
                     "healthy": False, "actionable": [
                         {"name": "search_index", "tier": "core",
                          "severity": "warning", "errorCode": "EE-E300"}]}}
        double = self.root / "doctor-double"
        double.write_text("#!/usr/bin/env bash\nexec " + shlex.quote(sys.executable)
                          + " " + shlex.quote(str(Path(__file__).resolve()))
                          + ' --probe "$@"\n')
        double.chmod(0o700)
        self.env = dict(os.environ, EE_DOCTOR_FIXTURE_TARGET=str(self.workspace),
                        EE_DOCTOR_FIXTURE_RUN_EE="1", EE_DOCTOR_FIXTURE_BINARY=str(double),
                        DOCTOR_ASSERTION_SCENARIO=str(self.root / "scenario.json"),
                        DOCTOR_ASSERTION_CALLS=str(self.root / "calls.jsonl"))
        marker = self.run_bash('doctor_fixture_corrupt "$2" P1 search_indexes')
        self.assertEqual(marker.returncode, 0, marker.stderr)

    def run_bash(self, body):
        command = ["bash", "-c", 'set -euo pipefail; source "$1"; ' + body, "guidance-control",
                   str(HERE / "lib.sh"), GUIDANCE_FM, "search_index_missing",
                   "search_index", "EE-E300", ".ee/index"]
        return subprocess.run(command, env=self.env, capture_output=True, text=True,
                              timeout=30, shell=False, check=False)

    def assert_outcome(self, rejected, message=None):
        self.scenario.setdefault("report", json.dumps(self.report))
        (self.root / "scenario.json").write_text(json.dumps(self.scenario))
        result = self.run_bash('doctor_fixture_assert_guidance_only "$2" "$3" "$4" "$5" "$6"')
        (self.root / "assert.receipt.json").write_text(json.dumps({
            "exit": result.returncode, "stdout": result.stdout,
            "stderr": result.stderr}, indent=2) + "\n")
        print(f"{self._testMethodName}: helper exit={result.returncode}; evidence={self.root}",
              flush=True)
        if rejected:
            self.assertNotEqual(result.returncode, 0, "false green: " + result.stderr)
            if message:
                self.assertIn(message, result.stderr)
        else:
            self.assertEqual(result.returncode, 0, result.stderr)
        return result

    def test_guidance_recorded_and_unchanged_finding_pass(self):
        self.assert_outcome(False)
        calls = [json.loads(row)["phase"] for row in
                 (self.root / "calls.jsonl").read_text().splitlines()]
        self.assertEqual(calls, ["--fix", "report"])

    def test_applied_outcome_is_rejected(self):
        self.scenario["fix_data"]["fixerResults"][0]["outcome"] = "applied"
        self.assert_outcome(True, "guidance_recorded")

    def test_applied_beside_guidance_is_rejected(self):
        self.scenario["fix_data"]["fixerResults"].append(
            {"findingCode": "search_index_missing", "operation": "run_index_rebuild",
             "outcome": "applied", "actionSequence": 2})
        self.assert_outcome(True, "guidance_recorded")

    def test_missing_finding_result_is_rejected(self):
        self.scenario["fix_data"]["fixerResults"] = []
        self.assert_outcome(True, "guidance_recorded")

    def test_zero_guidance_count_is_rejected(self):
        self.scenario["fix_data"]["guidanceOnlyFixerCount"] = 0
        self.assert_outcome(True, "guidance_recorded")

    def test_cleared_finding_is_rejected(self):
        self.report["data"].update(posture="ok", healthy=True, actionable=[])
        self.assert_outcome(True, "no longer reports")

    def test_other_error_code_is_rejected(self):
        self.report["data"]["actionable"][0]["errorCode"] = "EE-E301"
        self.assert_outcome(True, "no longer reports")

    def test_created_index_is_rejected(self):
        self.scenario["create_path"] = ".ee/index"
        self.assert_outcome(True, "created .ee/index")

    def test_nonzero_fix_exit_is_rejected(self):
        self.scenario["exits"] = {"--fix": 9}
        self.assertEqual(self.assert_outcome(True).returncode, 9)

    def test_empty_report_is_rejected(self):
        self.scenario["report"] = ""
        self.assert_outcome(True, "no longer reports")

    def test_multiple_reports_are_rejected(self):
        self.scenario["report"] = '{}\n' + json.dumps(self.report)
        self.assert_outcome(True, "no longer reports")

    def test_marker_only_mode_is_refused(self):
        self.env["EE_DOCTOR_FIXTURE_RUN_EE"] = "0"
        self.assertEqual(self.assert_outcome(True, "marker-only").returncode, 2)


if __name__ == "__main__":
    if sys.argv[1:2] == ["--probe"]:
        sys.exit(doctor_double())
    unittest.main(verbosity=2)
