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
    if exit_code and not (phase == "--fix" and "fix_data" in scenario and exit_code == 6):
        return exit_code
    if phase == "--fix" and "fix_data" in scenario:
        # Guidance-only mode: the double reports what the scenario says the
        # fixers did, and changes the workspace only when told to.
        if scenario.get("create_path"):
            (workspace / scenario["create_path"]).mkdir(parents=True)
        print(json.dumps({"schema": "ee.response.v2", "success": True,
                          "data": scenario["fix_data"], "degraded": []}))
        return exit_code
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
        # The oracle under test is lib.sh, driven through a marker-only fixture
        # pair. Write that pair here (the exact marker-only template, sourcing
        # the real lib.sh) rather than borrowing a manifest fixture, which stops
        # being marker-only the day it is built for real (bd-2oh15).
        self.fixture_dir = self.root / "fixture"
        self.fixture_dir.mkdir()
        for name, helper in (("corrupt.sh", "doctor_fixture_corrupt"),
                             ("assert.sh", "doctor_fixture_assert")):
            (self.fixture_dir / name).write_text(
                "#!/usr/bin/env bash\nset -euo pipefail\n"
                f". {shlex.quote(str(HERE / 'lib.sh'))}\n"
                f'{helper} "{FM}" "P1" "workspace_config"\n')
        prepared = self.run_script("corrupt.sh")
        self.assertEqual(prepared.returncode, 0, prepared.stderr)

    def run_script(self, name):
        # Negative controls intentionally return nonzero. Retain and assert
        # that status below; never interpolate the command into a shell string.
        result = subprocess.run(["bash", str(self.fixture_dir / name)], env=self.env,
                                capture_output=True, text=True, timeout=30,
                                shell=False, check=False)
        (self.root / (name + ".receipt.json")).write_text(json.dumps({
            "command": ["bash", str(self.fixture_dir / name)], "exit": result.returncode,
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
            "exits": {"--fix": 6},
            "fix_data": {
                "schema": "ee.doctor.fix_summary.v1", "runId": "guidance-control",
                "status": "completed_partial", "fixerDispatchPending": True,
                "unresolvedCoreCheckCount": 1,
                "unresolvedCoreChecks": [{"name": "search_index", "errorCode": "EE-E300"}],
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

    def test_zero_exit_cannot_hide_required_core_recovery(self):
        self.scenario["exits"]["--fix"] = 0
        self.assert_outcome(True, "must exit 6")

    def test_completed_ok_cannot_hide_required_core_recovery(self):
        self.scenario["fix_data"]["status"] = "completed_ok"
        self.assert_outcome(True, "guidance_recorded")

    def test_pending_flag_cannot_disagree_with_required_core_recovery(self):
        self.scenario["fix_data"]["fixerDispatchPending"] = False
        self.assert_outcome(True, "guidance_recorded")

    def test_required_core_check_must_remain_in_the_summary(self):
        self.scenario["fix_data"]["unresolvedCoreChecks"] = []
        self.assert_outcome(True, "guidance_recorded")

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


class UntestedRatchetContract(unittest.TestCase):
    """doctor_fixture_untested_ratchet (bd-2oh15): the one UNTESTED pin every
    counting sub-harness enforces. The real tree must sit exactly at the pin;
    a planted extra untested fixture, and a pin left above reality, must both
    be rejected."""

    def setUp(self):
        evidence_root = os.environ.get("EE_DOCTOR_ASSERTION_TEST_ROOT")
        if evidence_root:
            Path(evidence_root).mkdir(parents=True, exist_ok=True)
        self.root = Path(tempfile.mkdtemp(prefix=self._testMethodName + "-", dir=evidence_root))
        self.manifest = json.loads((HERE / "manifest.json").read_text())

    def ratchet(self, src):
        command = ["bash", "-c", 'set -euo pipefail; source "$1"; doctor_fixture_untested_ratchet ratchet-control "$2"',
                   "ratchet-control", str(HERE / "lib.sh"), str(src)]
        result = subprocess.run(command, capture_output=True, text=True, timeout=60,
                                shell=False, check=False)
        (self.root / "ratchet.receipt.json").write_text(json.dumps({
            "exit": result.returncode, "stdout": result.stdout,
            "stderr": result.stderr}, indent=2) + "\n")
        print(f"{self._testMethodName}: ratchet exit={result.returncode}; evidence={self.root}",
              flush=True)
        return result

    def planted(self, relabel):
        """A fixture source whose manifest relabels ONE fixture, with an empty
        directory per fixture (the ratchet counts directories by label)."""
        src = self.root / "src"
        src.mkdir()
        manifest = json.loads(json.dumps(self.manifest))
        old, new = relabel
        victim = next(f for f in manifest["fixtures"] if f["label"] == old)
        victim["label"] = new
        (src / "manifest.json").write_text(json.dumps(manifest))
        for fixture in manifest["fixtures"]:
            (src / fixture["id"]).mkdir()
        return src

    def test_real_tree_sits_exactly_at_the_pin(self):
        result = self.ratchet(HERE)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("UNCLASSIFIED (not tested, pin", result.stderr)

    def test_planted_extra_unclassified_is_rejected(self):
        result = self.ratchet(self.planted(("REPAIR", "UNCLASSIFIED")))
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("ratchet exceeded", result.stderr)

    def test_planted_extra_unresolved_is_rejected(self):
        result = self.ratchet(self.planted(("NOT-DETECTED", "UNRESOLVED")))
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("ratchet exceeded", result.stderr)

    def test_classified_fixture_with_unlowered_pin_is_rejected(self):
        result = self.ratchet(self.planted(("UNCLASSIFIED", "REPAIR")))
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("pin is stale", result.stderr)

    def test_new_out_of_scope_id_cannot_satisfy_the_pin(self):
        # Relabelling an UNCLASSIFIED fixture OUT-OF-SCOPE would lower the
        # UNCLASSIFIED count; the exact-id set must reject it.
        result = self.ratchet(self.planted(("UNCLASSIFIED", "OUT-OF-SCOPE")))
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("OUT-OF-SCOPE set changed", result.stderr)

    def test_dropping_a_pinned_out_of_scope_id_is_rejected(self):
        result = self.ratchet(self.planted(("OUT-OF-SCOPE", "UNCLASSIFIED")))
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("OUT-OF-SCOPE set changed", result.stderr)

    def test_real_tree_prints_the_out_of_scope_line(self):
        result = self.ratchet(HERE)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("OUT-OF-SCOPE (not doctor failure modes; never run, never a pass)",
                      result.stderr)


CRASH_HARNESS = HERE.parent.parent / "scripts" / "verify-crash-recovery.sh"
GUIDANCE_ONLY_FIX = {
    "schema": "ee.response.v2", "success": True, "degraded": [],
    "data": {"schema": "ee.doctor.fix_summary.v1", "status": "completed_partial",
             "unresolvedCoreChecks": [
                 {"name": "database", "severity": "error", "errorCode": "EE-E200",
                  "fixMode": "auto_guidance", "fixFinding": "database_missing"},
                 {"name": "search_index", "severity": "error", "errorCode": "EE-E300",
                  "fixMode": "manual", "fixFinding": None}],
             "fixerResults": [{"findingCode": "database_missing", "operation": "manual",
                               "outcome": "guidance_recorded"}]}}


class CrashRecoveryContract(unittest.TestCase):
    """scripts/verify-crash-recovery.sh (bd-2oh15 ruling c10026): a retry that
    exits 6 passes ONLY in the admissible guidance-only state -- no error
    envelope, every unresolved core check guidance-class, every fixer receipt
    guidance_recorded, and store bytes unchanged. A doctor double stands in for
    ee, so these controls verify the harness, not doctor."""

    def setUp(self):
        evidence_root = os.environ.get("EE_DOCTOR_ASSERTION_TEST_ROOT")
        if evidence_root:
            Path(evidence_root).mkdir(parents=True, exist_ok=True)
        self.root = Path(tempfile.mkdtemp(prefix=self._testMethodName + "-", dir=evidence_root))
        self.double = self.root / "ee-double"
        self.double.write_text(
            "#!/usr/bin/env bash\n"
            'ws=""; prev=""; for a in "$@"; do [ "$prev" = "--workspace" ] && ws="$a"; prev="$a"; done\n'
            'n=$(( $(cat "$DOUBLE_COUNT" 2>/dev/null || echo 0) + 1 )); printf \'%s\' "$n" > "$DOUBLE_COUNT"\n'
            # Write only during the RETRY, which the harness brackets with
            # store digests. Keying on the harness's own before-retry digest,
            # not on a call count, keeps this deterministic: the first run is
            # SIGKILLed 0.1 s after start and, on a loaded host, dies before
            # it can count itself (measured 2026-09-24: calls=1, so a
            # count-keyed write never happened and the control passed).
            'set -- "$TMPDIR"/ee-doctor-crash-work.*/store-before-retry.sha256\n'
            'if [ -f "$1" ] && [ -n "${DOUBLE_WRITE:-}" ]; then printf x > "$ws/.ee/$DOUBLE_WRITE"; fi\n'
            'cat "$DOUBLE_BODY"\n'
            'exit "$DOUBLE_EXIT"\n')
        self.double.chmod(0o700)

    def run_harness(self, body, exit_code, write=None):
        (self.root / "body.json").write_text(body if isinstance(body, str) else json.dumps(body))
        env = dict(os.environ, EE_DOCTOR_FIXTURE_BINARY=str(self.double),
                   DOUBLE_BODY=str(self.root / "body.json"), DOUBLE_EXIT=str(exit_code),
                   DOUBLE_COUNT=str(self.root / "calls"), TMPDIR=str(self.root))
        if write:
            env["DOUBLE_WRITE"] = write
        result = subprocess.run(["bash", str(CRASH_HARNESS)], env=env, capture_output=True,
                                text=True, timeout=60, shell=False, check=False)
        (self.root / "harness.receipt.json").write_text(json.dumps({
            "exit": result.returncode, "stdout": result.stdout,
            "stderr": result.stderr}, indent=2) + "\n")
        print(f"{self._testMethodName}: harness exit={result.returncode}; evidence={self.root}",
              flush=True)
        return result

    def fix_with(self, **changes):
        body = json.loads(json.dumps(GUIDANCE_ONLY_FIX))
        body["data"].update(changes)
        return body

    def test_guidance_only_exit_6_passes(self):
        result = self.run_harness(GUIDANCE_ONLY_FIX, 6)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("PASS (retry exit=6, guidance-only", result.stderr)

    def test_non_guidance_unresolved_entry_fails(self):
        checks = json.loads(json.dumps(GUIDANCE_ONLY_FIX["data"]["unresolvedCoreChecks"]))
        checks[1]["fixMode"] = "auto_repair"
        result = self.run_harness(self.fix_with(unresolvedCoreChecks=checks), 6)
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("outside the admissible guidance-only state", result.stderr)

    def test_error_envelope_with_exit_6_fails(self):
        body = {"schema": "ee.error.v2",
                "error": {"code": "doctor_runtime_io", "message": "build doctor index repair"}}
        result = self.run_harness(body, 6)
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("outside the admissible guidance-only state", result.stderr)

    def test_non_guidance_fixer_receipt_fails(self):
        receipts = [{"findingCode": "search_index_missing", "operation": "run_index_rebuild",
                     "outcome": "applied"}]
        result = self.run_harness(self.fix_with(fixerResults=receipts), 6)
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("outside the admissible guidance-only state", result.stderr)

    def test_empty_unresolved_set_with_exit_6_fails(self):
        result = self.run_harness(self.fix_with(unresolvedCoreChecks=[]), 6)
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("outside the admissible guidance-only state", result.stderr)

    def test_store_byte_change_with_exit_6_fails(self):
        result = self.run_harness(GUIDANCE_ONLY_FIX, 6, write="ee.db")
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("changed store bytes", result.stderr)

    def test_completed_retry_still_passes(self):
        body = {"schema": "ee.response.v2", "success": True,
                "data": {"schema": "ee.doctor.fix_summary.v1", "status": "completed_ok"}}
        result = self.run_harness(body, 0)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("PASS (retry exit=0)", result.stderr)

    def test_runtime_io_crash_still_fails(self):
        body = {"schema": "ee.error.v2", "error": {"code": "doctor_runtime_io"}}
        result = self.run_harness(body, 3)
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("unexpected exit=3", result.stderr)


class ConditionContract(unittest.TestCase):
    """doctor_fixture_under_condition (bd-2oh15 ruling t2250 R2): a fixture
    whose damage is outside the store's bytes runs every doctor call through
    its condition.sh. Without one, the command runs as given. With one, the
    command runs under the condition and its status comes back, and a
    condition that could not be applied returns 97, which a harness must never
    count as a pass."""

    def setUp(self):
        evidence_root = os.environ.get("EE_DOCTOR_ASSERTION_TEST_ROOT")
        if evidence_root:
            Path(evidence_root).mkdir(parents=True, exist_ok=True)
        self.root = Path(tempfile.mkdtemp(prefix=self._testMethodName + "-", dir=evidence_root))
        self.fm_dir = self.root / "fm-condition-double"
        self.fm_dir.mkdir()
        self.target = self.root / "target"
        self.target.mkdir()

    def under(self, *command):
        script = ('set -euo pipefail; source "$1"; shift; '
                  'status=0; doctor_fixture_under_condition "$@" || status=$?; '
                  'printf "status=%s\\n" "$status"')
        result = subprocess.run(
            ["bash", "-c", script, "condition-control", str(HERE / "lib.sh"),
             str(self.fm_dir), str(self.target), *command],
            capture_output=True, text=True, timeout=60, shell=False, check=False)
        (self.root / "condition.receipt.json").write_text(json.dumps({
            "exit": result.returncode, "stdout": result.stdout,
            "stderr": result.stderr}, indent=2) + "\n")
        print(f"{self._testMethodName}: stdout={result.stdout.strip()!r}; evidence={self.root}",
              flush=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        return result.stdout

    def condition(self, body):
        (self.fm_dir / "condition.sh").write_text(
            "#!/usr/bin/env bash\nset -euo pipefail\n" + body)

    def test_without_condition_the_command_runs_as_given(self):
        out = self.under("bash", "-c", 'printf "ran target=%s\\n" "${EE_DOCTOR_FIXTURE_TARGET:-unset}"; exit 6')
        self.assertIn("ran target=unset", out)
        self.assertIn("status=6", out)

    def test_condition_applies_and_passes_the_status_through(self):
        self.condition('export FIXTURE_CONDITION=applied\n'
                       'printf "condition target=%s\\n" "$EE_DOCTOR_FIXTURE_TARGET"\n'
                       'exec "$@"\n')
        out = self.under("bash", "-c", 'printf "ran %s\\n" "${FIXTURE_CONDITION:-missing}"; exit 6')
        self.assertIn(f"condition target={self.target}", out)
        self.assertIn("ran applied", out)
        self.assertIn("status=6", out)

    def test_condition_not_applied_returns_97_and_skips_the_command(self):
        self.condition('printf "condition: not applied\\n" >&2\nexit 97\n')
        out = self.under("bash", "-c", 'printf "ran anyway\\n"')
        self.assertNotIn("ran anyway", out)
        self.assertIn("status=97", out)


if __name__ == "__main__":
    if sys.argv[1:2] == ["--probe"]:
        sys.exit(doctor_double())
    unittest.main(verbosity=2)
