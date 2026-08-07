#!/bin/bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
WORKSPACE="$REPO_ROOT"
INPUT_FILE=""
ARTIFACT_VERIFICATION_FILE=""
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

Optional downloaded-artifact evidence:
  --artifact-verification <report.json>
      Read a consumer-generated ee.remote_build_artifact_manifest.verification.v1
      report. The snapshot never executes the artifact; generate this report by
      running scripts/ci_artifact_attestation.py verify after download.

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
        --artifact-verification)
            if [ "$#" -lt 2 ]; then
                printf 'ci_proof_lane_snapshot: --artifact-verification requires a path\n' >&2
                exit 2
            fi
            ARTIFACT_VERIFICATION_FILE="$2"
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
if [ -n "$HEAD_SHA" ] && [[ ! "$HEAD_SHA" =~ ^[a-f0-9]{40}$ ]]; then
    printf 'ci_proof_lane_snapshot: --head-sha must be a 40-character lowercase hex Git SHA\n' >&2
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

if [ -n "$ARTIFACT_VERIFICATION_FILE" ] && [ ! -f "$ARTIFACT_VERIFICATION_FILE" ]; then
    printf 'ci_proof_lane_snapshot: artifact verification report not found: %s\n' "$ARTIFACT_VERIFICATION_FILE" >&2
    exit 2
fi

ruby - "$WORKSPACE" "$INPUT_FILE" "$HEAD_SHA" "$LIMIT" "$ARTIFACT_VERIFICATION_FILE" <<'RUBY'
require "digest"
require "json"
require "open3"
require "time"

workspace = File.expand_path(ARGV.fetch(0))
input_file = ARGV.fetch(1)
head_sha_arg = ARGV.fetch(2)
limit = ARGV.fetch(3).to_i
artifact_verification_file = ARGV.fetch(4)
gh_bin = ENV.fetch("EE_CI_PROOF_LANE_GH_BIN", "gh")

EXPECTED_ARTIFACT = "ee-aarch64-apple-darwin-debug".freeze
ACTIVE_RUN_STALE_SECONDS = 30 * 60
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

def canonical_json(value)
  normalized =
    case value
    when Hash
      value.keys.sort.each_with_object({}) do |key, output|
        output[key] = canonical_json(value.fetch(key))
      end
    when Array
      value.map { |item| canonical_json(item) }
    else
      value
    end
  normalized.is_a?(String) ? normalized : normalized
end

def canonical_hash(value)
  "sha256:" + Digest::SHA256.hexdigest(JSON.generate(canonical_json(value)))
end

def load_artifact_verification(path)
  return nil if path.empty?

  report = parse_json(File.read(path), "parse artifact verification report")
  unless report.is_a?(Hash)
    raise "artifact verification report must be a JSON object"
  end
  claimed_hash = report["verificationHash"]
  body = report.reject { |key, _value| key == "verificationHash" }
  hash_matches = claimed_hash.is_a?(String) && claimed_hash == canonical_hash(body)
  schema_matches =
    report["schema"] == "ee.remote_build_artifact_manifest.verification.v1"
  return report if hash_matches && schema_matches

  rejected = report.dup
  rejected["accepted"] = false
  rejected["status"] = "rejected"
  rejected["rejections"] = Array(report["rejections"]) + [
    schema_matches ? "verification_hash_mismatch" : "verification_schema_unsupported"
  ]
  rejected
end

artifact_verification = load_artifact_verification(artifact_verification_file)

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
    "headSha" => head_sha,
    "headShaReachability" => "not_checked"
  }
end

def normalize_repository(repository)
  normalized = repository.dup
  normalized["headShaReachability"] ||= "not_checked"
  normalized
end

def github_head_sha_reachability(workspace, gh_bin, repository)
  owner = repository.fetch("owner").to_s
  name = repository.fetch("name").to_s
  head_sha = repository.fetch("headSha").to_s
  return "unknown" if owner.empty? || owner == "unknown" || name.empty?
  return "unknown" unless head_sha.match?(/\A[a-f0-9]{40}\z/)

  path = "repos/#{owner}/#{name}/commits/#{head_sha}"
  stdout, stderr, status = run_command([gh_bin, "api", path, "--jq", ".sha"], cwd: workspace)
  return "github_reachable" if status.zero? && stdout.strip == head_sha

  diagnostic = "#{stdout}\n#{stderr}"
  return "github_unreachable" if diagnostic.include?("No commit found") ||
                                  diagnostic.include?("HTTP 422") ||
                                  diagnostic.include?("HTTP 404") ||
                                  diagnostic.include?("Not Found")

  "unknown"
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

def normalize_nullable_time(value)
  value.nil? || value.to_s.empty? ? nil : value.to_s
end

def parse_time_or_nil(value)
  Time.parse(value.to_s)
rescue ArgumentError
  nil
end

def active_run_age_seconds(run, generated_at)
  started_at = parse_time_or_nil(run["createdAt"])
  generated = parse_time_or_nil(generated_at)
  return nil if started_at.nil? || generated.nil?

  [(generated - started_at).to_i, 0].max
end

def verification_surface_probes(report)
  Array(report["probes"]).each_with_object([]) do |probe, probes|
    next unless probe.is_a?(Hash)

    probe_id = probe["id"].to_s
    command_template, expected_surface =
      case probe_id
      when "version"
        ["ee --version", "ee version"]
      when "environment_attestation_help"
        ["ee diag environment-attestation --help", "diag environment-attestation"]
      else
        ["ee artifact behavior probe", "attested artifact behavior"]
      end
    status = probe["status"].to_s
    status = "failed" unless %w[passed failed not_run].include?(status)
    probes << {
      "commandTemplate" => command_template,
      "status" => status,
      "expectedSurface" => expected_surface,
      "firstFailureDiagnosis" => status == "passed" ? nil : "consumer behavior probe did not match the attested packaged binary"
    }
  end
end

def artifact_from_input(raw, run_head_sha, verification_report)
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

  report_matches_artifact =
    verification_report.is_a?(Hash) &&
    verification_report["artifactName"].to_s == name
  report_source_matches =
    report_matches_artifact && verification_report["sourceCommit"].to_s == run_head_sha
  report_verified =
    report_source_matches &&
    verification_report["accepted"] == true &&
    verification_report["status"] == "verified" &&
    verification_report["checksumStatus"] == "verified" &&
    verification_report["probeStatus"] == "passed"

  checksum_status = raw["checksumStatus"] || "not_checked"
  surface_probes = raw["surfaceProbes"] || [
    {
      "commandTemplate" => "ee diag environment-attestation --help",
      "status" => "not_run",
      "expectedSurface" => "diag environment-attestation",
      "firstFailureDiagnosis" => nil
    }
  ]
  attestation_status = raw["attestationStatus"] || "not_checked"
  # GitHub's artifact-list API cannot establish manifest verification.  Only a
  # separately supplied consumer-verification report may promote an artifact
  # to verified; input metadata can preserve rejection/missing posture only.
  attestation_status = "not_checked" if attestation_status == "verified"
  attested_source_commit = raw["attestedSourceCommit"]
  attested_git_tree = raw["attestedGitTree"]
  manifest_hash = raw["manifestHash"]
  verification_hash = raw["verificationHash"]
  attestation_rejections = Array(raw["attestationRejections"])

  if report_matches_artifact
    checksum_status = verification_report["checksumStatus"] || "not_checked"
    report_probes = verification_surface_probes(verification_report)
    surface_probes = report_probes unless report_probes.empty?
    attestation_status = report_verified ? "verified" : "rejected"
    attested_source_commit = verification_report["sourceCommit"]
    attested_git_tree = verification_report["gitTree"]
    manifest_hash = verification_report["manifestHash"]
    verification_hash = verification_report["verificationHash"]
    attestation_rejections = Array(verification_report["rejections"])
    attestation_rejections << "attested_source_commit_mismatch" unless report_source_matches
  end

  {
    "name" => name,
    "status" => status,
    "retentionExpiresAt" => raw["retentionExpiresAt"] || raw["expires_at"] || raw["expiresAt"],
    "checksumStatus" => checksum_status,
    "sourceSha" => raw["sourceSha"] || run_head_sha,
    "architecture" => raw["architecture"] || "aarch64-apple-darwin",
    "attestationStatus" => attestation_status,
    "attestedSourceCommit" => attested_source_commit,
    "attestedGitTree" => attested_git_tree,
    "manifestHash" => manifest_hash,
    "verificationHash" => verification_hash,
    "attestationRejections" => attestation_rejections.map(&:to_s).reject(&:empty?).uniq.sort.first(32),
    "surfaceProbes" => surface_probes
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
    "attestationStatus" => "missing",
    "attestedSourceCommit" => nil,
    "attestedGitTree" => nil,
    "manifestHash" => nil,
    "verificationHash" => nil,
    "attestationRejections" => ["artifact_missing"],
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

def run_artifacts(raw_run, repository, artifact_index, verification_report)
  raw_artifacts = raw_run["artifacts"] || artifact_index.fetch(raw_run["databaseId"].to_s, [])
  artifacts = raw_artifacts.map do |artifact|
    artifact_from_input(artifact, raw_run["headSha"].to_s, verification_report)
  end.compact

  if completed_run?(raw_run) &&
     normalize_conclusion(raw_run["conclusion"]) == "success" &&
     raw_run["workflowName"].to_s == "macOS EE Artifact" &&
     raw_run["headSha"].to_s == repository.fetch("headSha") &&
     artifacts.none? { |artifact| artifact["name"] == EXPECTED_ARTIFACT }
    artifacts << missing_artifact(raw_run["headSha"].to_s)
  end

  artifacts
end

def string_field(raw, *keys)
  keys.each do |key|
    value = raw[key]
    next if value.nil?

    text = value.to_s
    return text unless text.empty?
  end

  nil
end

def normalized_job_labels(raw_job)
  raw_labels = raw_job["labels"] || raw_job["runnerLabels"] || raw_job["runner_labels"] || []
  raw_labels = [raw_labels] unless raw_labels.is_a?(Array)
  raw_labels.map { |label| label.to_s.strip }.reject(&:empty?).uniq.sort.first(16)
end

def runner_assignment(status, runner_name, runner_group_name)
  return "assigned" if runner_name || runner_group_name
  return "unassigned" if status == "queued"
  return "not_applicable" if status == "completed"

  "unknown"
end

def normalize_job(raw_job, raw_run, generated_at)
  job_id = (raw_job["databaseId"] || raw_job["id"]).to_s
  return nil if job_id.empty?

  status = normalize_status(raw_job["status"] || raw_run["status"])
  started_at = normalize_nullable_time(raw_job["startedAt"] || raw_job["started_at"])
  runner_name = string_field(raw_job, "runnerName", "runner_name")
  runner_group_name = string_field(raw_job, "runnerGroupName", "runner_group_name")
  assignment = runner_assignment(status, runner_name, runner_group_name)
  queue_age_seconds =
    if status == "queued" && assignment == "unassigned"
      active_run_age_seconds(raw_run, generated_at)
    else
      nil
    end

  {
    "jobId" => job_id,
    "name" => string_field(raw_job, "name") || "unknown",
    "status" => status,
    "conclusion" => normalize_conclusion(raw_job["conclusion"]),
    "labels" => normalized_job_labels(raw_job),
    "runnerName" => runner_name,
    "runnerGroupName" => runner_group_name,
    "startedAt" => started_at,
    "completedAt" => normalize_nullable_time(raw_job["completedAt"] || raw_job["completed_at"]),
    "queueAgeSeconds" => queue_age_seconds,
    "runnerAssignment" => assignment
  }
end

def normalize_jobs(raw_run, generated_at)
  jobs = raw_run["jobs"].is_a?(Array) ? raw_run["jobs"] : []
  jobs.map { |job| normalize_job(job, raw_run, generated_at) }.compact.first(16)
end

def run_workflow_name(raw_runs, run_id)
  raw_runs.find { |run| (run["databaseId"] || run["runId"]).to_s == run_id }.to_h["workflowName"].to_s
end

def run_job_labels(run)
  run.fetch("jobEvidence", []).flat_map { |job| job.fetch("labels", []) }.uniq.sort
end

def artifact_attestation_verified?(artifact)
  probes = artifact.fetch("surfaceProbes", [])
  artifact["checksumStatus"] == "verified" &&
    artifact["attestationStatus"] == "verified" &&
    artifact["sourceSha"] == artifact["attestedSourceCommit"] &&
    artifact["manifestHash"].to_s.match?(/\Asha256:[a-f0-9]{64}\z/) &&
    artifact["verificationHash"].to_s.match?(/\Asha256:[a-f0-9]{64}\z/) &&
    !artifact["attestedGitTree"].to_s.empty? &&
    !probes.empty? &&
    probes.all? { |probe| probe["status"] == "passed" }
end

def artifact_attestation_rejected?(artifact)
  artifact["attestationStatus"] == "rejected" ||
    !artifact.fetch("attestationRejections", []).empty?
end

def runner_detail(run, key)
  run.fetch("jobEvidence", []).map { |job| job[key] }.find { |value| !value.nil? && !value.to_s.empty? }
end

def comparable_prior_success(run, normalized_runs, raw_runs)
  labels = run_job_labels(run)
  workflow_name = run_workflow_name(raw_runs, run["runId"])
  return nil if labels.empty? || workflow_name.empty?

  normalized_runs.find do |candidate|
    next false if candidate["runId"] == run["runId"]
    next false unless run_workflow_name(raw_runs, candidate["runId"]) == workflow_name
    next false unless candidate["status"] == "completed" && candidate["conclusion"] == "success"
    next false if runner_detail(candidate, "runnerName").nil? && runner_detail(candidate, "runnerGroupName").nil?

    !(run_job_labels(candidate) & labels).empty?
  end
end

def queue_diagnosis(run, normalized_runs, raw_runs, generated_at)
  return nil unless run["status"] == "queued" || run["status"] == "in_progress"

  age_seconds = active_run_age_seconds(run, generated_at)
  stale = age_seconds && age_seconds >= ACTIVE_RUN_STALE_SECONDS
  jobs = run.fetch("jobEvidence", [])
  unassigned = jobs.empty? || jobs.any? { |job| job["runnerAssignment"] == "unassigned" }
  assigned = jobs.any? { |job| job["runnerAssignment"] == "assigned" }
  comparable = comparable_prior_success(run, normalized_runs, raw_runs)

  status =
    if run["status"] == "queued" && stale && unassigned && comparable
      "github_hosted_runner_capacity"
    elsif run["status"] == "queued" && stale && unassigned
      "runner_label_or_settings_unverified"
    elsif run["status"] == "queued" && stale && assigned
      "runner_assigned_but_not_started"
    elsif run["status"] == "in_progress" && stale
      "workflow_execution_stale"
    else
      "ordinary_wait"
    end

  next_action =
    case status
    when "github_hosted_runner_capacity", "runner_label_or_settings_unverified"
      "inspect_github_runner_capacity_or_labels"
    when "runner_assigned_but_not_started", "workflow_execution_stale"
      "inspect_workflow_or_runner_if_authorized"
    else
      "handoff_run_id_and_keep_polling"
    end

  {
    "status" => status,
    "queueAgeSeconds" => age_seconds,
    "staleAfterSeconds" => ACTIVE_RUN_STALE_SECONDS,
    "comparablePriorRunId" => comparable&.fetch("runId"),
    "comparablePriorRunnerName" => comparable ? runner_detail(comparable, "runnerName") : nil,
    "comparablePriorRunnerGroupName" => comparable ? runner_detail(comparable, "runnerGroupName") : nil,
    "nextAction" => next_action
  }
end

def attach_queue_diagnostics!(normalized_runs, raw_runs, generated_at)
  normalized_runs.each do |run|
    diagnosis = queue_diagnosis(run, normalized_runs, raw_runs, generated_at)
    run["queueDiagnosis"] = diagnosis if diagnosis
  end
end

def normalize_run(raw_run, repository, artifact_index, generated_at, verification_report)
  run_id = (raw_run["databaseId"] || raw_run["runId"]).to_s
  jobs = normalize_jobs(raw_run, generated_at)
  job_ids = jobs.map { |job| job["jobId"] }.reject(&:empty?).uniq
  status = normalize_status(raw_run["status"])
  conclusion = normalize_conclusion(raw_run["conclusion"])
  artifacts = run_artifacts(raw_run, repository, artifact_index, verification_report)
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

  expected_artifact = artifacts.find { |artifact| artifact["name"] == EXPECTED_ARTIFACT }
  failed_surface_probe = expected_artifact &&
    expected_artifact.fetch("surfaceProbes", []).find { |probe| probe["status"] == "failed" }
  first_failure =
    if conclusion == "cancelled"
      "run cancelled before artifact upload; this is not a source/test verdict"
    elsif expected_artifact && expected_artifact["status"] == "missing"
      "expected artifact was missing after successful proof-lane completion"
    elsif expected_artifact && expected_artifact["checksumStatus"] == "mismatch"
      "artifact checksum mismatch; reject artifact before binary proof reuse"
    elsif failed_surface_probe
      failed_surface_probe["firstFailureDiagnosis"] ||
        "artifact surface probe failed required command-surface validation"
    elsif expected_artifact && artifact_attestation_rejected?(expected_artifact)
      "artifact attestation was rejected; source, build inputs, command, packaged bytes, or probe evidence did not match"
    elsif expected_artifact && !artifact_attestation_verified?(expected_artifact)
      "artifact exists but checksum, source-bound manifest, or consumer behavior probe is not verified"
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
    "jobEvidence" => jobs,
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
    ["macOS EE Artifact", run_id, "reuse_verified_artifact", "Current-head artifact manifest, checksum, packaged bytes, and consumer behavior probe are verified."]
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
  when "checksum_mismatch"
    ["macOS EE Artifact", run_id, "file_followup_bead", "Artifact checksum mismatch rejects this binary proof; repair the proof lane before reuse."]
  when "surface_probe_failed"
    ["macOS EE Artifact", run_id, "file_followup_bead", "Artifact surface probe failed the required command surface; repair the artifact or probe before reuse."]
  when "artifact_attestation_invalid"
    ["macOS EE Artifact", run_id, "file_followup_bead", "Downloaded artifact attestation did not match its source, build inputs, command, packaged bytes, or behavior probe; reject it."]
  when "artifact_attestation_required"
    ["macOS EE Artifact", run_id, "download_and_verify_artifact", "Artifact metadata exists, but source-bound manifest verification and a consumer behavior probe are still required before reuse."]
  when "gh_unavailable"
    [nil, nil, "abstain_manual_review", "GitHub Actions state could not be read; preserve the first gh error and abstain."]
  when "local_only_head_unavailable"
    [nil, nil, "abstain_manual_review", "Requested head SHA is not reachable from GitHub; reconcile the checkout before dispatching an artifact proof lane."]
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
  when "checksum_mismatch"
    [["ci_proof_lane_checksum_mismatch", "high", "The proof-lane artifact checksum did not verify.", "Reject the artifact and repair checksum provenance before reuse."]]
  when "surface_probe_failed"
    [["ci_proof_lane_surface_probe_failed", "high", "The proof-lane artifact failed the required command-surface probe.", "Reject the artifact until the expected ee surface is proven."]]
  when "artifact_attestation_invalid"
    [["ci_proof_lane_artifact_attestation_invalid", "high", "The proof-lane artifact attestation did not match its source, build inputs, command, packaged bytes, or behavior probe.", "Reject the artifact and rerun the consumer verifier before relying on this lane."]]
  when "artifact_attestation_required"
    [["ci_proof_lane_unknown_source", "warning", "Artifact metadata alone cannot establish source authority without a verified manifest and consumer behavior probe.", "Download the artifact and run scripts/ci_artifact_attestation.py verify before reuse."]]
  when "gh_unavailable"
    [["ci_proof_lane_gh_unavailable", "warning", "The producer could not read GitHub Actions state.", "Check gh authentication/network state or rerun with --input fixture JSON."]]
  when "local_only_head_unavailable"
    [["ci_proof_lane_local_only_head_unavailable", "warning", "The requested head SHA is not reachable from GitHub Actions.", "Reconcile the checkout with the remote or use an approved push path before dispatching a proof-lane workflow."]]
  when "no_matching_run"
    [["ci_proof_lane_no_matching_run", "info", "No proof-lane run exists for the requested head SHA.", "Coordinate through Agent Mail before dispatching a new proof-lane run."]]
  else
    []
  end.map do |code, severity, message, repair|
    {"code" => code, "severity" => severity, "message" => message, "repair" => repair}
  end
end

def stale_active_run_degraded(normalized_runs, repository, generated_at)
  stale_active_run = normalized_runs.find do |run|
    next false unless run["headSha"] == repository.fetch("headSha")
    next false unless run["status"] == "queued" || run["status"] == "in_progress"

    age_seconds = active_run_age_seconds(run, generated_at)
    age_seconds && age_seconds >= ACTIVE_RUN_STALE_SECONDS
  end
  return [] unless stale_active_run

  [
    {
      "code" => "ci_proof_lane_active_run_stale",
      "severity" => "warning",
      "message" => "The active proof-lane run is still queued or running beyond the normal handoff window.",
      "repair" => "Keep polling or hand off the authoritative run id; do not dispatch a duplicate run or cancel without human approval."
    }
  ]
end

def recovery_for(verdict, run_id)
  case verdict
  when "fresh_artifact_available"
    [["reuse", "cite verified artifact manifest and verification hashes", false, "Reuse only the artifact whose downloaded bytes produced this verification report."]]
  when "wait_for_active_run", "duplicate_dispatch_detected"
    [["wait", "gh run view #{run_id} --json status,conclusion,jobs", false, "Poll the active artifact run until it reaches a terminal conclusion."]]
  when "run_cancelled_before_artifact", "artifact_missing", "artifact_stale", "checksum_mismatch", "surface_probe_failed", "artifact_attestation_invalid", "gh_unavailable", "local_only_head_unavailable"
    [["manual_review", "preserve first-failure diagnosis", false, "Do not treat this proof-lane state as source/test evidence."]]
  when "artifact_attestation_required"
    [["download", "gh run download #{run_id} --name #{EXPECTED_ARTIFACT} --dir <external-temp>", false, "Download, extract, verify the manifest and checksum, and rerun the packaged-binary behavior probe."]]
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

def choose_verdict(raw_runs, normalized_runs, repository)
  head_sha = repository.fetch("headSha")
  return ["local_only_head_unavailable", nil] if repository["headShaReachability"] == "github_unreachable"

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
      return ["checksum_mismatch", run["runId"]] if artifact["checksumStatus"] == "mismatch"
      return ["surface_probe_failed", run["runId"]] if artifact.fetch("surfaceProbes", []).any? { |probe| probe["status"] == "failed" }
      return ["artifact_attestation_invalid", run["runId"]] if artifact_attestation_rejected?(artifact)
      return ["artifact_attestation_required", run["runId"]] unless artifact_attestation_verified?(artifact)
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

def build_snapshot(repository:, generated_at:, raw_runs:, artifact_index: {}, verification_report: nil, gh_unavailable: false)
  repository = normalize_repository(repository)
  if gh_unavailable
    verdict = "gh_unavailable"
    verdict_run_id = nil
    normalized_runs = []
  else
    normalized_runs = raw_runs.map do |run|
      normalize_run(run, repository, artifact_index, generated_at, verification_report)
    end
    attach_queue_diagnostics!(normalized_runs, raw_runs, generated_at)
    verdict, verdict_run_id = choose_verdict(raw_runs, normalized_runs, repository)
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
  attestation_invalid_count = normalized_runs.sum do |run|
    run["artifacts"].count do |artifact|
      artifact["status"] == "available" && artifact_attestation_rejected?(artifact)
    end
  end
  attestation_required_count = normalized_runs.sum do |run|
    run["artifacts"].count do |artifact|
      artifact["status"] == "available" &&
        !artifact_attestation_rejected?(artifact) &&
        !artifact_attestation_verified?(artifact)
    end
  end
  artifact_authority_verdicts = %w[fresh_artifact_available artifact_stale checksum_mismatch surface_probe_failed artifact_attestation_invalid artifact_attestation_required]

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
      "attestationInvalidCount" => attestation_invalid_count,
      "attestationRequiredCount" => attestation_required_count,
      "localCargoFallbackAllowed" => false,
      "sourceTestVerdict" => artifact_authority_verdicts.include?(verdict) ? "artifact_authority_only" : "not_evaluated"
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
    "degraded" => degraded_for(verdict) + stale_active_run_degraded(normalized_runs, repository, generated_at)
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
  repository = normalize_repository(input.fetch("repository"))
  unless head_sha_arg.empty?
    repository["headSha"] = head_sha_arg
    repository["headShaReachability"] = "not_checked"
  end
  generated_at = input["generatedAt"] || now_iso
  snapshot = build_snapshot(
    repository: repository,
    generated_at: generated_at,
    raw_runs: fixture_runs(input),
    artifact_index: input.fetch("artifactIndex", {}),
    verification_report: artifact_verification
  )
  puts JSON.pretty_generate(snapshot)
else
  repository = repository_from_git(workspace, head_sha_arg)
  generated_at = now_iso
  if !system("command", "-v", gh_bin, out: File::NULL, err: File::NULL)
    snapshot = build_snapshot(repository: repository, generated_at: generated_at, raw_runs: [], verification_report: artifact_verification, gh_unavailable: true)
    puts JSON.pretty_generate(snapshot)
    exit 0
  end

  repository["headShaReachability"] = github_head_sha_reachability(workspace, gh_bin, repository)
  runs, artifact_index, error = live_runs(workspace, gh_bin, limit, repository)
  if error
    warn "ci_proof_lane_snapshot: #{error.lines.first.to_s.strip}"
    snapshot = build_snapshot(repository: repository, generated_at: generated_at, raw_runs: [], verification_report: artifact_verification, gh_unavailable: true)
  else
    snapshot = build_snapshot(repository: repository, generated_at: generated_at, raw_runs: runs, artifact_index: artifact_index, verification_report: artifact_verification)
  end
  puts JSON.pretty_generate(snapshot)
end
RUBY
