#!/bin/bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
WORKSPACE="$REPO_ROOT"
STRICT=false

usage() {
    cat <<'USAGE'
ci_proof_lane_hygiene.sh

Static, network-free GitHub Actions proof-lane hygiene check.

Usage:
  scripts/ci_proof_lane_hygiene.sh [--workspace <path>] [--json] [--strict]

The check reads workflow YAML only. It does not call gh, dispatch workflows,
cancel runs, download artifacts, reserve files, mutate Beads, or run Cargo.
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

if [ ! -d "$WORKSPACE" ]; then
    printf 'ci_proof_lane_hygiene: workspace not found: %s\n' "$WORKSPACE" >&2
    exit 2
fi

report="$(
    ruby - "$WORKSPACE" <<'RUBY'
require "json"
require "yaml"

repo_root = File.expand_path(ARGV.fetch(0))
workflow_paths = [
  ".github/workflows/ci.yml",
  ".github/workflows/macos-ee-artifact.yml"
]

SEVERITY_RANK = {
  "info" => 0,
  "low" => 1,
  "warning" => 2,
  "medium" => 3,
  "high" => 4,
  "critical" => 5
}.freeze

findings = []
workflows = []

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

workflow_paths.each do |workflow_path|
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

  workflows << workflow
end

highest = findings.map { |finding| finding["severity"] }.max_by { |severity| SEVERITY_RANK.fetch(severity, -1) } || "info"
status =
  if SEVERITY_RANK.fetch(highest) >= SEVERITY_RANK.fetch("high")
    "blocked"
  elsif findings.empty?
    "pass"
  else
    "warning"
  end

puts JSON.generate(
  "schema" => "ee.ci_proof_lane_hygiene.v1",
  "status" => status,
  "summary" => {
    "workflowCount" => workflows.length,
    "findingCount" => findings.length,
    "highestSeverity" => highest
  },
  "findings" => findings,
  "workflows" => workflows
)
RUBY
)"

printf '%s\n' "$report"

if [ "$STRICT" = true ]; then
    status="$(printf '%s\n' "$report" | ruby -rjson -e 'print JSON.parse(STDIN.read).fetch("status")')"
    if [ "$status" != "pass" ]; then
        exit 1
    fi
fi
