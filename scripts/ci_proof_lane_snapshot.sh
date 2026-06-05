#!/bin/bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
WORKSPACE="$REPO_ROOT"
INPUT_FILE=""
HEAD_SHA=""
LIMIT="20"
JSON_MODE=false

usage() {
    cat <<'USAGE'
ci_proof_lane_snapshot.sh

Read-only GitHub Actions proof-lane snapshot producer.

Usage:
  scripts/ci_proof_lane_snapshot.sh [--workspace <path>] [--head-sha <sha>] [--limit <n>] [--json]
  scripts/ci_proof_lane_snapshot.sh --input <fixture.json> [--head-sha <sha>] [--json]

The producer never dispatches workflows, cancels runs, downloads artifacts,
reserves files, mutates Beads, acknowledges Agent Mail, runs Cargo, or builds.
Live mode reads bounded GitHub Actions state with gh. Offline mode transforms a
documented fixture with schema ee.ci_proof_lane_input.v1 into the public
ee.ci_proof_lane_snapshot.v1 contract.
USAGE
}

while [ "$#" -gt 0 ]; do
    case "$1" in
        --workspace)
            if [ "$#" -lt 2 ]; then
                printf 'ci_proof_lane_snapshot: --workspace requires a path\n' >&2
                exit 2
            fi
            WORKSPACE="$2"
            shift 2
            ;;
        --input)
            if [ "$#" -lt 2 ]; then
                printf 'ci_proof_lane_snapshot: --input requires a fixture path\n' >&2
                exit 2
            fi
            INPUT_FILE="$2"
            shift 2
            ;;
        --head-sha)
            if [ "$#" -lt 2 ]; then
                printf 'ci_proof_lane_snapshot: --head-sha requires a SHA\n' >&2
                exit 2
            fi
            HEAD_SHA="$2"
            shift 2
            ;;
        --limit)
            if [ "$#" -lt 2 ]; then
                printf 'ci_proof_lane_snapshot: --limit requires a positive integer\n' >&2
                exit 2
            fi
            LIMIT="$2"
            shift 2
            ;;
        --json)
            JSON_MODE=true
            shift
            ;;
        --help|-h)
            usage
            exit 0
            ;;
        *)
            printf 'ci_proof_lane_snapshot: unknown argument: %s\n' "$1" >&2
            usage >&2
            exit 2
            ;;
    esac
done

case "$LIMIT" in
    ''|*[!0-9]*)
        printf 'ci_proof_lane_snapshot: --limit must be a positive integer\n' >&2
        exit 2
        ;;
esac
if [ "$LIMIT" -lt 1 ]; then
    printf 'ci_proof_lane_snapshot: --limit must be a positive integer\n' >&2
    exit 2
fi

if [ "$JSON_MODE" != true ]; then
    :
fi

if ! command -v ruby >/dev/null 2>&1; then
    printf 'ci_proof_lane_snapshot: required tool missing: ruby\n' >&2
    exit 2
fi

if [ ! -d "$WORKSPACE" ]; then
    printf 'ci_proof_lane_snapshot: workspace not found: %s\n' "$WORKSPACE" >&2
    exit 2
fi

ruby - "$WORKSPACE" "$INPUT_FILE" "$HEAD_SHA" "$LIMIT" <<'RUBY'
require "digest"
require "json"
require "open3"
require "time"

workspace = File.expand_path(ARGV.fetch(0))
input_file = ARGV.fetch(1)
head_sha_arg = ARGV.fetch(2)
limit = ARGV.fetch(3).to_i
gh_bin = ENV.fetch("EE_CI_PROOF_LANE_GH_BIN", "gh")

EXPECTED_ARTIFACT = "ee-aarch64-apple-darwin-debug".freeze
WORKFLOW_PATHS = {
  "CI" => ".github/workflows/ci.yml",
  "macOS EE Artifact" => ".github/workflows/macos-ee-artifact.yml"
}.freeze
PROOF_KINDS = {
  "CI" => "ci",
  "macOS EE Artifact" => "dedicated_artifact"
}.freeze
CONCURRENCY = {
  "CI" => "${{ github.workflow }}-${{ github.ref }}",
  "macOS EE Artifact" => "macos-ee-artifact-${{ github.workflow }}-${{ github.sha }}"
}.freeze
DISPATCH_POLICY = {
  "CI" => "manual_review_required",
  "macOS EE Artifact" => "reuse_active_same_head"
}.freeze

def run_command(argv, cwd:)
  stdout, stderr, status = Open3.capture3(*argv, chdir: cwd)
  [stdout, stderr, status.exitstatus]
rescue SystemCallError => error
  ["", error.message, 127]
end

def parse_json(text, context)
  JSON.parse(text)
rescue JSON::ParserError => error
  raise "#{context}: #{error.message}"
end

def git_value(workspace, *args)
  stdout, _stderr, status = run_command(["git", *args], cwd: workspace)
  status.zero? ? stdout.strip : nil
end

def repository_from_git(workspace, head_sha_arg)
  remote = git_value(workspace, "config", "--get", "remote.origin.url").to_s
  owner = "unknown"
  name = File.basename(workspace)
  case remote
  when %r{github.com[:/](.+?)/([^/]+?)(?:\.git)?$}
    owner = Regexp.last_match(1)
    name = Regexp.last_match(2)
  end

  head_sha = head_sha_arg
  head_sha = git_value(workspace, "rev-parse", "HEAD").to_s if head_sha.empty?
  head_sha = "0" * 40 unless head_sha.match?(/\A[a-f0-9]{40}\z/)

  {
    "owner" => owner,
    "name" => name,
    "defaultBranch" => "main",
    "headSha" => head_sha
  }
end

def now_iso
  Time.now.utc.iso8601
end

def snapshot_id(repository, generated_at, runs)
  seed = [
    repository.fetch("owner"),
    repository.fetch("name"),
    repository.fetch("headSha"),
    generated_at,
    runs.map { |run| run["databaseId"] || run["runId"] }.join(",")
  ].join("|")
  "ci_proof_lane_#{Digest::SHA256.hexdigest(seed)[0, 24]}"
end

def active_run?(run)
  %w[queued in_progress].include?(run["status"].to_s)
end

def completed_run?(run)
  run["status"].to_s == "completed"
end

def source_freshness(run, head_sha)
  run["headSha"].to_s == head_sha ? "current" : "stale"
end

def normalize_conclusion(value)
  return nil if value.nil? || value.to_s.empty?

  allowed = %w[success failure cancelled skipped timed_out action_required neutral]
  allowed.include?(value.to_s) ? value.to_s : "failure"
end

def normalize_status(value)
  allowed = %w[queued in_progress completed unknown]
  allowed.include?(value.to_s) ? value.to_s : "unknown"
end

def normalize_time(value)
  value.nil? || value.to_s.empty? ? now_iso : value.to_s
end

def artifact_from_input(raw, run_head_sha)
  return nil if raw.nil?

  name = raw["name"].to_s
  return nil if name.empty?

  expired = raw["expired"] == true || raw["status"].to_s == "expired"
  status =
    if raw["status"]
      raw["status"].to_s
    elsif expired
      "expired"
    else
      "available"
    end

  {
    "name" => name,
    "status" => status,
    "retentionExpiresAt" => raw["retentionExpiresAt"] || raw["expires_at"] || raw["expiresAt"],
    "checksumStatus" => raw["checksumStatus"] || "not_checked",
    "sourceSha" => raw["sourceSha"] || run_head_sha,
    "architecture" => raw["architecture"] || "aarch64-apple-darwin",
    "surfaceProbes" => raw["surfaceProbes"] || [
      {
        "commandTemplate" => "ee diag environment-attestation --help",
        "status" => "not_run",
        "expectedSurface" => "diag environment-attestation",
        "firstFailureDiagnosis" => nil
      }
    ]
  }
end

def missing_artifact(run_head_sha)
  {
    "name" => EXPECTED_ARTIFACT,
    "status" => "missing",
    "retentionExpiresAt" => nil,
    "checksumStatus" => "missing",
    "sourceSha" => run_head_sha,
    "architecture" => "aarch64-apple-darwin",
    "surfaceProbes" => [
      {
        "commandTemplate" => "ee diag environment-attestation --help",
        "status" => "not_run",
        "expectedSurface" => "diag environment-attestation",
        "firstFailureDiagnosis" => "expected artifact was missing after successful proof-lane completion"
      }
    ]
  }
end

def run_artifacts(raw_run, repository, artifact_index)
  raw_artifacts = raw_run["artifacts"] || artifact_index.fetch(raw_run["databaseId"].to_s, [])
  artifacts = raw_artifacts.map { |artifact| artifact_from_input(artifact, raw_run["headSha"].to_s) }.compact

  if completed_run?(raw_run) &&
     normalize_conclusion(raw_run["conclusion"]) == "success" &&
     raw_run["workflowName"].to_s == "macOS EE Artifact" &&
     raw_run["headSha"].to_s == repository.fetch("headSha") &&
     artifacts.none? { |artifact| artifact["name"] == EXPECTED_ARTIFACT }
    artifacts << missing_artifact(raw_run["headSha"].to_s)
  end

  artifacts
end

def normalize_run(raw_run, repository, artifact_index)
  run_id = (raw_run["databaseId"] || raw_run["runId"]).to_s
  jobs = raw_run["jobs"] || []
  job_ids = jobs.map { |job| (job["databaseId"] || job["id"]).to_s }.reject(&:empty?).uniq
  status = normalize_status(raw_run["status"])
  conclusion = normalize_conclusion(raw_run["conclusion"])
  artifacts = run_artifacts(raw_run, repository, artifact_index)
  freshness = source_freshness(raw_run, repository.fetch("headSha"))
  artifact_freshness =
    if active_run?(raw_run)
      "unknown"
    elsif conclusion == "cancelled"
      "not_applicable"
    elsif artifacts.any? { |artifact| artifact["status"] == "available" }
      freshness
    elsif artifacts.any? { |artifact| artifact["status"] == "missing" }
      "unknown"
    else
      "not_applicable"
    end

  first_failure =
    if conclusion == "cancelled"
      "run cancelled before artifact upload; this is not a source/test verdict"
    elsif artifacts.any? { |artifact| artifact["status"] == "missing" }
      "expected artifact was missing after successful proof-lane completion"
    elsif freshness == "stale" && artifacts.any? { |artifact| artifact["status"] == "available" }
      "artifact source SHA is older than the requested repository head SHA"
    else
      raw_run["firstFailureDiagnosis"]
    end

  {
    "runId" => run_id,
    "jobIds" => job_ids,
    "event" => raw_run["event"].to_s.empty? ? "unknown" : raw_run["event"].to_s,
    "headSha" => raw_run["headSha"].to_s.match?(/\A[a-f0-9]{40}\z/) ? raw_run["headSha"].to_s : repository.fetch("headSha"),
    "ref" => raw_run["ref"] || "refs/heads/#{raw_run["headBranch"] || repository.fetch("defaultBranch")}",
    "status" => status,
    "conclusion" => conclusion,
    "createdAt" => normalize_time(raw_run["createdAt"]),
    "updatedAt" => normalize_time(raw_run["updatedAt"]),
    "completedAt" => status == "completed" ? normalize_time(raw_run["completedAt"] || raw_run["updatedAt"]) : nil,
    "sourceFreshness" => freshness,
    "artifactFreshness" => artifact_freshness,
    "artifacts" => artifacts,
    "firstFailureDiagnosis" => first_failure
  }
end

def workflow_object(name, runs, verdict)
  workflow_runs = runs.select { |run| run["workflowName"].to_s == name }
  status =
    if verdict == "gh_unavailable"
      "unavailable"
    elsif workflow_runs.any? { |run| normalize_conclusion(run["conclusion"]) == "cancelled" }
      "degraded"
    elsif workflow_runs.empty?
      "unknown"
    else
      "ok"
    end

  {
    "workflowName" => name,
    "workflowPath" => WORKFLOW_PATHS.fetch(name),
    "proofLaneKind" => PROOF_KINDS.fetch(name),
    "concurrencyGroup" => CONCURRENCY.fetch(name),
    "dispatchPolicy" => DISPATCH_POLICY.fetch(name),
    "status" => status,
    "runs" => []
  }
end

def recommendation(verdict, run_id)
  case verdict
  when "fresh_artifact_available"
    ["macOS EE Artifact", run_id, "download_and_verify_artifact", "Current-head artifact metadata exists; download, verify checksum, and run the surface probe before using it as binary proof."]
  when "wait_for_active_run"
    ["macOS EE Artifact", run_id, "wait", "Active current-head proof run exists; wait rather than dispatching another run."]
  when "duplicate_dispatch_detected"
    ["macOS EE Artifact", run_id, "wait", "Multiple active runs target the same proof lane and head SHA; coordinate before dispatching anything else."]
  when "run_cancelled_before_artifact"
    ["CI", run_id, "file_followup_bead", "Terminal run was cancelled before artifact evidence existed; this is not a source/test verdict."]
  when "artifact_missing"
    ["macOS EE Artifact", run_id, "file_followup_bead", "Completed proof-lane run did not expose the expected artifact; do not treat it as binary proof."]
  when "artifact_stale"
    ["macOS EE Artifact", run_id, "dispatch_new_run", "Available artifact is stale relative to the requested head SHA; coordinate before dispatching a current-head run."]
  when "gh_unavailable"
    [nil, nil, "abstain_manual_review", "GitHub Actions state could not be read; preserve the first gh error and abstain."]
  else
    [nil, nil, "dispatch_new_run", "No matching proof-lane run exists for the requested head SHA."]
  end
end

def degraded_for(verdict)
  case verdict
  when "duplicate_dispatch_detected"
    [["ci_proof_lane_duplicate_dispatch", "warning", "Multiple active workflow_dispatch runs target the same artifact proof lane and head SHA.", "Reuse one active run; do not dispatch another run."]]
  when "run_cancelled_before_artifact"
    [["ci_proof_lane_cancelled_before_artifact", "warning", "The CI run was cancelled before the artifact upload step completed.", "Use a non-cancelling dedicated artifact workflow or wait for an active current-head run."]]
  when "artifact_missing"
    [["ci_proof_lane_artifact_missing", "warning", "The proof-lane run completed but the expected artifact is unavailable.", "Inspect the artifact upload job and file a workflow follow-up before reusing this lane."]]
  when "artifact_stale"
    [["ci_proof_lane_artifact_stale", "warning", "The artifact source SHA is stale relative to the requested repository head SHA.", "Wait for or dispatch a current-head proof-lane run before using the artifact."]]
  when "gh_unavailable"
    [["ci_proof_lane_gh_unavailable", "warning", "The producer could not read GitHub Actions state.", "Check gh authentication/network state or rerun with --input fixture JSON."]]
  when "no_matching_run"
    [["ci_proof_lane_no_matching_run", "info", "No proof-lane run exists for the requested head SHA.", "Coordinate through Agent Mail before dispatching a new proof-lane run."]]
  else
    []
  end.map do |code, severity, message, repair|
    {"code" => code, "severity" => severity, "message" => message, "repair" => repair}
  end
end

def recovery_for(verdict, run_id)
  case verdict
  when "fresh_artifact_available"
    [["download", "gh run download #{run_id} --name #{EXPECTED_ARTIFACT} --dir <external-temp>", false, "Download into external temp, verify checksum, then run the no-mock harness with EE_BINARY."]]
  when "wait_for_active_run", "duplicate_dispatch_detected"
    [["wait", "gh run view #{run_id} --json status,conclusion,jobs", false, "Poll the active artifact run until it reaches a terminal conclusion."]]
  when "run_cancelled_before_artifact", "artifact_missing", "artifact_stale", "gh_unavailable"
    [["manual_review", "preserve first-failure diagnosis", false, "Do not treat this proof-lane state as source/test evidence."]]
  else
    [["coordinate", "send Agent Mail before workflow_dispatch", false, "Avoid duplicate dispatches before creating a new proof-lane run."]]
  end.each_with_index.map do |(kind, command, mutates, rationale), index|
    {
      "priority" => index,
      "kind" => kind,
      "command" => command,
      "mutatesState" => mutates,
      "rationale" => rationale
    }
  end
end

def choose_verdict(raw_runs, normalized_runs, head_sha)
  dedicated = raw_runs.select { |run| run["workflowName"].to_s == "macOS EE Artifact" }
  current = dedicated.select { |run| run["headSha"].to_s == head_sha }
  active_current = current.select { |run| active_run?(run) }
  return ["duplicate_dispatch_detected", (active_current.first["databaseId"] || active_current.first["runId"]).to_s] if active_current.length > 1
  return ["wait_for_active_run", (active_current.first["databaseId"] || active_current.first["runId"]).to_s] if active_current.length == 1

  current_completed = normalized_runs.select do |run|
    run["headSha"] == head_sha && run["status"] == "completed"
  end
  current_success = current_completed.select { |run| run["conclusion"] == "success" }
  current_success.each do |run|
    artifact = run["artifacts"].find { |item| item["name"] == EXPECTED_ARTIFACT }
    if artifact && artifact["status"] == "available"
      return ["fresh_artifact_available", run["runId"]]
    elsif artifact && artifact["status"] == "missing"
      return ["artifact_missing", run["runId"]]
    end
  end

  cancelled = current_completed.find { |run| run["conclusion"] == "cancelled" }
  return ["run_cancelled_before_artifact", cancelled["runId"]] if cancelled

  stale = normalized_runs.find do |run|
    run["sourceFreshness"] == "stale" && run["artifacts"].any? { |artifact| artifact["status"] == "available" }
  end
  return ["artifact_stale", stale["runId"]] if stale

  ["no_matching_run", nil]
end

def build_snapshot(repository:, generated_at:, raw_runs:, artifact_index: {}, gh_unavailable: false)
  if gh_unavailable
    verdict = "gh_unavailable"
    verdict_run_id = nil
    normalized_runs = []
  else
    normalized_runs = raw_runs.map { |run| normalize_run(run, repository, artifact_index) }
    verdict, verdict_run_id = choose_verdict(raw_runs, normalized_runs, repository.fetch("headSha"))
  end

  workflows = WORKFLOW_PATHS.keys.map do |name|
    object = workflow_object(name, raw_runs, verdict)
    object["runs"] = normalized_runs.select do |run|
      raw_runs.any? do |raw|
        (raw["databaseId"] || raw["runId"]).to_s == run["runId"] &&
          raw["workflowName"].to_s == name
      end
    end
    object
  end

  active_runs = normalized_runs.count { |run| %w[queued in_progress].include?(run["status"]) && run["headSha"] == repository.fetch("headSha") }
  duplicate_count = [active_runs, 0].max
  cancelled_count = normalized_runs.count { |run| run["conclusion"] == "cancelled" && run["headSha"] == repository.fetch("headSha") }
  stale_count = normalized_runs.count { |run| run["sourceFreshness"] == "stale" && run["artifacts"].any? { |artifact| artifact["status"] == "available" } }
  checksum_mismatch_count = normalized_runs.sum { |run| run["artifacts"].count { |artifact| artifact["checksumStatus"] == "mismatch" } }

  workflow_name, run_id, next_action, rationale = recommendation(verdict, verdict_run_id)

  {
    "schema" => "ee.ci_proof_lane_snapshot.v1",
    "snapshotId" => snapshot_id(repository, generated_at, raw_runs),
    "repository" => repository,
    "generatedAt" => generated_at,
    "redactionStatus" => "workflow_run_job_artifact_ids_statuses_hashes_no_logs_no_tokens_no_local_paths",
    "summary" => {
      "verdict" => verdict,
      "freshArtifactAvailable" => verdict == "fresh_artifact_available",
      "duplicateDispatchCount" => verdict == "duplicate_dispatch_detected" ? duplicate_count : 0,
      "activeRunCount" => active_runs,
      "cancelledBeforeArtifactCount" => cancelled_count,
      "staleArtifactCount" => stale_count,
      "checksumMismatchCount" => checksum_mismatch_count,
      "localCargoFallbackAllowed" => false,
      "sourceTestVerdict" => verdict == "fresh_artifact_available" || verdict == "artifact_stale" ? "artifact_authority_only" : "not_evaluated"
    },
    "workflows" => workflows,
    "activeRecommendation" => {
      "verdict" => verdict,
      "workflowName" => workflow_name,
      "runId" => run_id,
      "nextAction" => next_action,
      "rationale" => rationale
    },
    "recoveryActions" => recovery_for(verdict, run_id),
    "degraded" => degraded_for(verdict)
  }
end

def fixture_runs(input)
  input.fetch("runs", []).map do |run|
    run.merge("databaseId" => (run["databaseId"] || run["runId"]).to_s)
  end
end

def live_runs(workspace, gh_bin, limit, repository)
  run_fields = "databaseId,workflowName,status,conclusion,headSha,event,createdAt,updatedAt,headBranch,displayTitle"
  stdout, stderr, status = run_command([gh_bin, "run", "list", "--limit", limit.to_s, "--json", run_fields], cwd: workspace)
  return [nil, {}, stderr.empty? ? "gh run list failed" : stderr] unless status.zero?

  runs = parse_json(stdout, "parse gh run list").map { |run| run.merge("databaseId" => run["databaseId"].to_s) }
  artifact_index = {}

  runs.each do |run|
    run_id = run["databaseId"].to_s
    jobs_stdout, _jobs_stderr, jobs_status = run_command([gh_bin, "run", "view", run_id, "--json", "jobs"], cwd: workspace)
    if jobs_status.zero?
      jobs = parse_json(jobs_stdout, "parse gh run view #{run_id}").fetch("jobs", [])
      run["jobs"] = jobs
    else
      run["jobs"] = []
    end

    next unless run["workflowName"].to_s == "macOS EE Artifact" && run["status"].to_s == "completed"

    artifacts_stdout, _artifacts_stderr, artifacts_status = run_command(
      [gh_bin, "api", "repos/#{repository.fetch("owner")}/#{repository.fetch("name")}/actions/runs/#{run_id}/artifacts"],
      cwd: workspace
    )
    next unless artifacts_status.zero?

    artifact_index[run_id] = parse_json(artifacts_stdout, "parse gh artifacts #{run_id}").fetch("artifacts", [])
  end

  [runs, artifact_index, nil]
end

if !input_file.empty?
  input = parse_json(File.read(input_file), "parse input fixture")
  unless input["schema"] == "ee.ci_proof_lane_input.v1"
    raise "input fixture schema must be ee.ci_proof_lane_input.v1"
  end
  repository = input.fetch("repository")
  repository["headSha"] = head_sha_arg unless head_sha_arg.empty?
  generated_at = input["generatedAt"] || now_iso
  snapshot = build_snapshot(
    repository: repository,
    generated_at: generated_at,
    raw_runs: fixture_runs(input),
    artifact_index: input.fetch("artifactIndex", {})
  )
  puts JSON.pretty_generate(snapshot)
else
  repository = repository_from_git(workspace, head_sha_arg)
  generated_at = now_iso
  if !system("command", "-v", gh_bin, out: File::NULL, err: File::NULL)
    snapshot = build_snapshot(repository: repository, generated_at: generated_at, raw_runs: [], gh_unavailable: true)
    puts JSON.pretty_generate(snapshot)
    exit 0
  end

  runs, artifact_index, error = live_runs(workspace, gh_bin, limit, repository)
  if error
    warn "ci_proof_lane_snapshot: #{error.lines.first.to_s.strip}"
    snapshot = build_snapshot(repository: repository, generated_at: generated_at, raw_runs: [], gh_unavailable: true)
  else
    snapshot = build_snapshot(repository: repository, generated_at: generated_at, raw_runs: runs, artifact_index: artifact_index)
  end
  puts JSON.pretty_generate(snapshot)
end
RUBY
