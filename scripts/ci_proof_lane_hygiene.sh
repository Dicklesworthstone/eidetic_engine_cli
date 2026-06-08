#!/bin/bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
WORKSPACE="$REPO_ROOT"
STRICT=false
SELF_TEST=false

usage() {
    cat <<'USAGE'
ci_proof_lane_hygiene.sh

Static, network-free GitHub Actions proof-lane hygiene check.

Usage:
  scripts/ci_proof_lane_hygiene.sh [--workspace <path>] [--json] [--strict] [--self-test]

The check reads workflow YAML only. It does not call gh, dispatch workflows,
cancel runs, download artifacts, reserve files, mutate Beads, or run Cargo.

Options:
  --self-test  Run synthetic workflow policy tests without reading the workspace.
USAGE
}

while [ "$#" -gt 0 ]; do
    case "$1" in
        --workspace)
            if [ "$#" -lt 2 ]; then
                printf 'ci_proof_lane_hygiene: --workspace requires a path\n' >&2
                exit 2
            fi
            WORKSPACE="$2"
            shift 2
            ;;
        --json)
            shift
            ;;
        --strict)
            STRICT=true
            shift
            ;;
        --self-test)
            SELF_TEST=true
            shift
            ;;
        --help|-h)
            usage
            exit 0
            ;;
        *)
            printf 'ci_proof_lane_hygiene: unknown argument: %s\n' "$1" >&2
            usage >&2
            exit 2
            ;;
    esac
done

if ! command -v ruby >/dev/null 2>&1; then
    printf '{"schema":"ee.ci_proof_lane_hygiene.v1","status":"blocked","summary":{"workflowCount":0,"findingCount":1,"highestSeverity":"high"},"findings":[{"code":"required_tool_missing","severity":"high","workflowPath":null,"verdict":"abstain_manual_review","message":"required tool missing: ruby","guidance":"Install ruby or run this check in an environment with Ruby stdlib YAML support."}],"workflows":[]}\n'
    exit 2
fi

if [ "$SELF_TEST" != true ] && [ ! -d "$WORKSPACE" ]; then
    printf 'ci_proof_lane_hygiene: workspace not found: %s\n' "$WORKSPACE" >&2
    exit 2
fi

report="$(
    ruby - "$WORKSPACE" "$SELF_TEST" <<'RUBY'
require "json"
require "yaml"

repo_root = File.expand_path(ARGV.fetch(0))
SELF_TEST = ARGV.fetch(1) == "true"

SEVERITY_RANK = {
  "info" => 0,
  "low" => 1,
  "warning" => 2,
  "medium" => 3,
  "high" => 4,
  "critical" => 5
}.freeze

def workflow_paths_for(repo_root)
  workflow_dir = File.join(repo_root, ".github/workflows")
  if File.directory?(workflow_dir)
    Dir.children(workflow_dir)
       .select { |entry| entry.end_with?(".yml", ".yaml") }
       .sort
       .map { |entry| ".github/workflows/#{entry}" }
  else
    [".github/workflows/ci.yml", ".github/workflows/macos-ee-artifact.yml"]
  end
end

def add_finding(findings, code:, severity:, workflow_path:, verdict:, message:, guidance:, job: nil)
  finding = {
    "code" => code,
    "severity" => severity,
    "workflowPath" => workflow_path,
    "job" => job,
    "verdict" => verdict,
    "message" => message,
    "guidance" => guidance
  }
  findings << finding
end

def raw_on(data)
  return data["on"] if data.key?("on")
  return data[true] if data.key?(true)

  nil
end

def trigger_names(data)
  value = raw_on(data)
  case value
  when Hash
    value.keys.map(&:to_s).sort
  when Array
    value.map(&:to_s).sort
  when String
    [value]
  else
    []
  end
end

def concurrency(data)
  value = data["concurrency"]
  case value
  when Hash
    {
      "group" => value["group"].to_s,
      "cancelInProgress" => value.fetch("cancel-in-progress", nil)
    }
  when String
    {
      "group" => value,
      "cancelInProgress" => nil
    }
  else
    {
      "group" => nil,
      "cancelInProgress" => nil
    }
  end
end

def upload_artifact_steps(job)
  steps = job.fetch("steps", [])
  uploads = []
  steps.each_with_index do |step, index|
    uses = step.fetch("uses", "").to_s
    next unless uses.include?("actions/upload-artifact")

    with = step.fetch("with", {}) || {}
    uploads << {
      "stepIndex" => index,
      "name" => step.fetch("name", "upload artifact").to_s,
      "artifactName" => with.fetch("name", "").to_s,
      "retentionDays" => with.fetch("retention-days", nil),
      "ifNoFilesFound" => with.fetch("if-no-files-found", nil),
      "condition" => step.fetch("if", nil)
    }
  end
  uploads
end

def command_surface_probe?(job, pattern)
  job.fetch("steps", []).any? do |step|
    text = [step["name"], step["run"]].compact.join("\n")
    text.include?(pattern)
  end
end

def analyze_workflow(data, workflow_path, findings)
  triggers = trigger_names(data)
  flow_concurrency = concurrency(data)
  jobs = data.fetch("jobs", {}) || {}
  artifact_lanes = []

  jobs.each do |job_name, job|
    uploads = upload_artifact_steps(job || {})
    next if uploads.empty?

    artifact_lanes << {
      "job" => job_name.to_s,
      "runsOn" => job.fetch("runs-on", nil),
      "jobCondition" => job.fetch("if", nil),
      "uploads" => uploads
    }
  end

  workflow = {
    "path" => workflow_path,
    "name" => data.fetch("name", nil),
    "triggers" => triggers,
    "concurrency" => flow_concurrency,
    "artifactLanes" => artifact_lanes,
    "policyVerdicts" => []
  }

  if workflow_path == ".github/workflows/ci.yml"
    if artifact_lanes.any? && flow_concurrency["cancelInProgress"] == true && triggers.include?("push")
      workflow["policyVerdicts"] << "run_cancelled_before_artifact"
      add_finding(
        findings,
        code: "proof_lane_artifact_cancellable_by_push",
        severity: "warning",
        workflow_path: workflow_path,
        job: artifact_lanes.map { |lane| lane["job"] }.join(","),
        verdict: "run_cancelled_before_artifact",
        message: "artifact upload lanes share CI concurrency with cancel-in-progress=true",
        guidance: "Use artifacts from a completed run only. For fresh binary proof during active pushes, prefer a dedicated artifact workflow."
      )
    end

    artifact_lanes.each do |lane|
      job_condition = lane["jobCondition"].to_s
      if !job_condition.empty? && job_condition != "always()"
        add_finding(
          findings,
          code: "proof_lane_artifact_job_conditionally_skipped",
          severity: "low",
          workflow_path: workflow_path,
          job: lane["job"],
          verdict: "abstain_manual_review",
          message: "artifact-producing job has a condition and may be skipped for some events",
          guidance: "If a terminal run lacks the expected artifact, inspect job condition before dispatching a replacement."
        )
      end

      lane["uploads"].each do |upload|
        condition = upload["condition"].to_s
        next if condition.empty? || condition == "always()"

        add_finding(
          findings,
          code: "proof_lane_artifact_upload_conditionally_skipped",
          severity: "low",
          workflow_path: workflow_path,
          job: lane["job"],
          verdict: "abstain_manual_review",
          message: "artifact upload step has a condition and may be skipped even when the job runs",
          guidance: "If a terminal run lacks the expected artifact, inspect upload-step condition before dispatching a replacement."
        )
      end
    end
  end

  if workflow_path == ".github/workflows/macos-ee-artifact.yml"
    workflow["policyVerdicts"] << "wait_for_active_run"

    unless triggers.include?("workflow_dispatch")
      add_finding(
        findings,
        code: "proof_lane_manual_dispatch_missing",
        severity: "high",
        workflow_path: workflow_path,
        verdict: "abstain_manual_review",
        message: "dedicated proof lane lacks workflow_dispatch",
        guidance: "Add workflow_dispatch before relying on manual artifact production."
      )
    end

    group = flow_concurrency["group"].to_s
    if group.include?("github.run_id")
      add_finding(
        findings,
        code: "proof_lane_concurrency_group_per_run",
        severity: "medium",
        workflow_path: workflow_path,
        verdict: "duplicate_dispatch_detected",
        message: "concurrency group is keyed by github.run_id, so duplicate dispatches never share a group",
        guidance: "Group by workflow and source SHA; agents should wait for an active run with the same head SHA before dispatching another."
      )
    elsif !group.include?("github.sha")
      add_finding(
        findings,
        code: "proof_lane_concurrency_missing_source_sha",
        severity: "warning",
        workflow_path: workflow_path,
        verdict: "wait_for_active_run",
        message: "concurrency group does not include github.sha",
        guidance: "Include the source SHA so unrelated main-branch changes do not collapse distinct artifact-source runs."
      )
    end

    if flow_concurrency["cancelInProgress"] != false
      add_finding(
        findings,
        code: "proof_lane_cancel_in_progress_not_false",
        severity: "medium",
        workflow_path: workflow_path,
        verdict: "run_cancelled_before_artifact",
        message: "dedicated artifact workflow should not cancel an in-flight proof run",
        guidance: "Set cancel-in-progress: false and wait for the active run unless it is terminal."
      )
    end

    if artifact_lanes.empty?
      add_finding(
        findings,
        code: "proof_lane_artifact_missing",
        severity: "high",
        workflow_path: workflow_path,
        verdict: "artifact_missing",
        message: "dedicated proof lane has no upload-artifact step",
        guidance: "Upload the archive and checksum as a named artifact before consuming the run."
      )
    end

    unless jobs.values.any? { |job| command_surface_probe?(job || {}, "diag environment-attestation --help") }
      add_finding(
        findings,
        code: "proof_lane_surface_probe_missing",
        severity: "medium",
        workflow_path: workflow_path,
        verdict: "surface_probe_failed",
        message: "dedicated proof lane does not probe the environment-attestation surface",
        guidance: "Run `ee diag environment-attestation --help` before packaging the artifact."
      )
    end
  end

  if workflow_path == ".github/workflows/release.yml" && artifact_lanes.any?
    workflow["policyVerdicts"] << "abstain_manual_review"
    add_finding(
      findings,
      code: "proof_lane_release_artifact_requires_manual_review",
      severity: "low",
      workflow_path: workflow_path,
      job: artifact_lanes.map { |lane| lane["job"] }.join(","),
      verdict: "abstain_manual_review",
      message: "release workflow artifacts require release-specific provenance and checksum review",
      guidance: "Do not substitute release artifacts for a current-head proof lane unless provenance, checksum, source SHA, and surface probes are recorded."
    )
  end

  if artifact_lanes.any? && workflow["policyVerdicts"].empty?
    workflow["policyVerdicts"] << "abstain_manual_review"
    add_finding(
      findings,
      code: "proof_lane_artifact_workflow_unclassified",
      severity: "low",
      workflow_path: workflow_path,
      job: artifact_lanes.map { |lane| lane["job"] }.join(","),
      verdict: "abstain_manual_review",
      message: "artifact-producing workflow is not classified by proof-lane hygiene policy",
      guidance: "Classify this workflow as ci, dedicated artifact, release, or external before relying on its artifacts for proof."
    )
  end

  workflow
end

def build_report(workflows, findings)
  highest = findings.map { |finding| finding["severity"] }.max_by { |severity| SEVERITY_RANK.fetch(severity, -1) } || "info"
  status =
    if SEVERITY_RANK.fetch(highest) >= SEVERITY_RANK.fetch("high")
      "blocked"
    elsif findings.empty?
      "pass"
    else
      "warning"
    end

  {
    "schema" => "ee.ci_proof_lane_hygiene.v1",
    "status" => status,
    "summary" => {
      "workflowCount" => workflows.length,
      "findingCount" => findings.length,
      "highestSeverity" => highest
    },
    "findings" => findings,
    "workflows" => workflows
  }
end

def report_for_workspace(repo_root)
  findings = []
  workflows = []

  workflow_paths_for(repo_root).each do |workflow_path|
    absolute_path = File.join(repo_root, workflow_path)
    unless File.file?(absolute_path)
      add_finding(
        findings,
        code: "proof_lane_workflow_missing",
        severity: "high",
        workflow_path: workflow_path,
        verdict: "abstain_manual_review",
        message: "workflow file is missing",
        guidance: "Restore the workflow before treating this proof lane as usable."
      )
      next
    end

    begin
      data = YAML.load_file(absolute_path) || {}
    rescue Psych::Exception => error
      add_finding(
        findings,
        code: "proof_lane_workflow_yaml_invalid",
        severity: "high",
        workflow_path: workflow_path,
        verdict: "abstain_manual_review",
        message: "workflow YAML could not be parsed: #{error.class}",
        guidance: "Fix workflow YAML before using this proof lane."
      )
      next
    end

    workflows << analyze_workflow(data, workflow_path, findings)
  end

  build_report(workflows, findings)
end

def workflow_from_yaml(text)
  YAML.load(text) || {}
end

def synthetic_report(fixtures)
  findings = []
  workflows = fixtures.map do |workflow_path, yaml|
    analyze_workflow(workflow_from_yaml(yaml), workflow_path, findings)
  end
  build_report(workflows, findings)
end

def assert_self_test(condition, message)
  raise "ci_proof_lane_hygiene self-test failed: #{message}" unless condition
end

def assert_codes(report, expected_codes)
  actual = report.fetch("findings").map { |finding| finding.fetch("code") }
  expected_codes.each do |code|
    assert_self_test(actual.include?(code), "missing expected finding code #{code}; actual=#{actual.inspect}")
  end
end

def run_self_test
  dedicated_pass = <<~YAML
    name: macOS EE Artifact
    on: [workflow_dispatch]
    concurrency:
      group: "macos-ee-artifact-${{ github.sha }}"
      cancel-in-progress: false
    jobs:
      package:
        runs-on: macos-latest
        steps:
          - name: Probe environment attestation
            run: ee diag environment-attestation --help
          - name: Upload artifact
            uses: actions/upload-artifact@v4
            with:
              name: ee-macos
              retention-days: 7
              if-no-files-found: error
  YAML

  report = synthetic_report(".github/workflows/macos-ee-artifact.yml" => dedicated_pass)
  assert_self_test(report.fetch("schema") == "ee.ci_proof_lane_hygiene.v1", "schema mismatch")
  assert_self_test(report.fetch("status") == "pass", "dedicated pass workflow should pass")
  assert_self_test(report.fetch("summary").fetch("workflowCount") == 1, "expected one workflow")
  assert_self_test(report.fetch("findings").empty?, "dedicated pass workflow should not emit findings")
  assert_self_test(report.fetch("workflows").first.fetch("policyVerdicts").include?("wait_for_active_run"), "dedicated workflow should advise waiting for active run")

  ci_cancellable = <<~YAML
    name: CI
    on: [push]
    concurrency:
      group: "ci-${{ github.ref }}"
      cancel-in-progress: true
    jobs:
      test:
        runs-on: ubuntu-latest
        steps:
          - name: Upload logs
            uses: actions/upload-artifact@v4
            with:
              name: test-logs
  YAML

  report = synthetic_report(".github/workflows/ci.yml" => ci_cancellable)
  assert_self_test(report.fetch("status") == "warning", "cancellable CI artifact lane should warn")
  assert_codes(report, ["proof_lane_artifact_cancellable_by_push"])
  assert_self_test(report.fetch("workflows").first.fetch("policyVerdicts").include?("run_cancelled_before_artifact"), "CI workflow should classify cancellable artifact")

  duplicate_dispatch = <<~YAML
    name: macOS EE Artifact
    on: [workflow_dispatch]
    concurrency:
      group: "macos-ee-artifact-${{ github.run_id }}"
      cancel-in-progress: true
    jobs:
      package:
        runs-on: macos-latest
        steps:
          - name: Upload artifact
            uses: actions/upload-artifact@v4
            with:
              name: ee-macos
  YAML

  report = synthetic_report(".github/workflows/macos-ee-artifact.yml" => duplicate_dispatch)
  assert_self_test(report.fetch("status") == "warning", "duplicate-dispatch workflow should warn")
  assert_codes(
    report,
    [
      "proof_lane_concurrency_group_per_run",
      "proof_lane_cancel_in_progress_not_false",
      "proof_lane_surface_probe_missing"
    ]
  )

  unusable_dedicated = <<~YAML
    name: macOS EE Artifact
    on: [push]
    jobs:
      package:
        runs-on: macos-latest
        steps:
          - name: Build placeholder
            run: echo placeholder
  YAML

  report = synthetic_report(".github/workflows/macos-ee-artifact.yml" => unusable_dedicated)
  assert_self_test(report.fetch("status") == "blocked", "dedicated workflow with no dispatch/artifact should block")
  assert_codes(
    report,
    [
      "proof_lane_manual_dispatch_missing",
      "proof_lane_artifact_missing",
      "proof_lane_surface_probe_missing"
    ]
  )

  release_artifact = <<~YAML
    name: Release
    on: [workflow_dispatch]
    jobs:
      release:
        runs-on: ubuntu-latest
        steps:
          - uses: actions/upload-artifact@v4
            with:
              name: release-archive
  YAML

  report = synthetic_report(".github/workflows/release.yml" => release_artifact)
  assert_self_test(report.fetch("status") == "warning", "release artifacts should require manual review")
  assert_codes(report, ["proof_lane_release_artifact_requires_manual_review"])

  unclassified_artifact = <<~YAML
    name: Extra Artifact
    on: [workflow_dispatch]
    jobs:
      package:
        runs-on: ubuntu-latest
        steps:
          - uses: actions/upload-artifact@v4
            with:
              name: misc
  YAML

  report = synthetic_report(".github/workflows/extra-artifact.yml" => unclassified_artifact)
  assert_self_test(report.fetch("status") == "warning", "unknown artifact workflow should warn")
  assert_codes(report, ["proof_lane_artifact_workflow_unclassified"])

  puts "ci proof-lane hygiene self-test passed"
end

if SELF_TEST
  run_self_test
else
  puts JSON.generate(report_for_workspace(repo_root))
end
RUBY
)"

printf '%s\n' "$report"

if [ "$SELF_TEST" = true ]; then
    exit 0
fi

if [ "$STRICT" = true ]; then
    status="$(printf '%s\n' "$report" | ruby -rjson -e 'print JSON.parse(STDIN.read).fetch("status")')"
    if [ "$status" != "pass" ]; then
        exit 1
    fi
fi
