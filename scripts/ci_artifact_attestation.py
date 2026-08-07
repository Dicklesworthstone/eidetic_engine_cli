#!/usr/bin/env python3
"""Generate and verify source-bound attestations for CI-built ee artifacts.

This is repository verification tooling, not an ee command gate.  Generation
binds the exact source, effective Cargo inputs, build command, packaged bytes,
and post-package behavior probes into one deterministic manifest.  Verification
recomputes every locally observable identity and rejects missing evidence as
well as explicit mismatches.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import subprocess
import sys
import tarfile
from pathlib import Path
from typing import Any


MANIFEST_SCHEMA = "ee.remote_build_artifact_manifest.v1"
VERIFICATION_SCHEMA = "ee.remote_build_artifact_manifest.verification.v1"
PROVENANCE_SCHEMA = "ee.remote_build.cargo_provenance.v1"
SOURCE_MANIFEST_ALGORITHM = "git_ls_tree_z_v1"
REWRITE_ID = "franken_stack_path_rewrite_v1"
ARTIFACT_NAME = "ee-aarch64-apple-darwin-debug"
TARGET_TRIPLE = "aarch64-apple-darwin"
FRANKEN_REPOSITORIES = (
    "asupersync",
    "franken_agent_detection",
    "franken_networkx",
    "frankensearch",
    "frankensqlite",
    "sqlmodel_rust",
    "toon_rust",
)


class AttestationError(Exception):
    """A deterministic artifact-attestation rejection."""


def sha256_bytes(data: bytes) -> str:
    return "sha256:" + hashlib.sha256(data).hexdigest()


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return "sha256:" + digest.hexdigest()


def canonical_bytes(value: Any) -> bytes:
    return json.dumps(
        value,
        ensure_ascii=False,
        sort_keys=True,
        separators=(",", ":"),
    ).encode("utf-8")


def canonical_hash(value: Any) -> str:
    return sha256_bytes(canonical_bytes(value))


def manifest_hash(manifest: dict[str, Any]) -> str:
    body = dict(manifest)
    body.pop("manifestHash", None)
    return canonical_hash(body)


def run_command(
    argv: list[str],
    *,
    cwd: Path,
    check: bool = True,
    timeout_seconds: int | None = None,
    env: dict[str, str] | None = None,
) -> subprocess.CompletedProcess[bytes]:
    try:
        result = subprocess.run(
            argv,
            cwd=cwd,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
            timeout=timeout_seconds,
            env=env,
        )
    except subprocess.TimeoutExpired as error:
        raise AttestationError(
            f"command timed out after {timeout_seconds} seconds: {argv[0]}"
        ) from error
    except OSError as error:
        raise AttestationError(f"command unavailable: {argv[0]}: {error}") from error
    if check and result.returncode != 0:
        raise AttestationError(
            f"command failed with exit {result.returncode}: {' '.join(argv)}"
        )
    return result


def git_bytes(workspace: Path, *args: str) -> bytes:
    return run_command(["git", *args], cwd=workspace).stdout


def git_text(workspace: Path, *args: str) -> str:
    return git_bytes(workspace, *args).decode("utf-8", errors="strict").strip()


def require_full_commit(value: str, label: str) -> str:
    if not re.fullmatch(r"[0-9a-f]{40}", value):
        raise AttestationError(f"{label} is not a full lowercase Git commit: {value}")
    return value


def resolve_source(workspace: Path, requested_commit: str) -> dict[str, Any]:
    resolved_commit = require_full_commit(
        git_text(workspace, "rev-parse", "--verify", f"{requested_commit}^{{commit}}"),
        "resolved commit",
    )
    requested_commit = require_full_commit(requested_commit, "requested commit")
    if resolved_commit != requested_commit:
        raise AttestationError(
            f"requested commit resolved to {resolved_commit}, expected {requested_commit}"
        )
    git_tree = git_text(workspace, "rev-parse", f"{resolved_commit}^{{tree}}")
    if not re.fullmatch(r"[0-9a-f]{40}", git_tree):
        raise AttestationError("resolved Git tree is malformed")

    listing = git_bytes(workspace, "ls-tree", "-r", "--full-tree", "-z", resolved_commit)
    file_count = listing.count(b"\0")
    if file_count == 0:
        raise AttestationError("source manifest is empty")

    cargo_lock = git_bytes(workspace, "show", f"{resolved_commit}:Cargo.lock")
    cargo_toml = git_bytes(workspace, "show", f"{resolved_commit}:Cargo.toml")
    franken_lock = git_bytes(workspace, "show", f"{resolved_commit}:franken-stack.lock")
    return {
        "requestedCommit": requested_commit,
        "resolvedCommit": resolved_commit,
        "gitTree": git_tree,
        "sourceManifest": {
            "algorithm": SOURCE_MANIFEST_ALGORITHM,
            "hash": sha256_bytes(listing),
            "fileCount": file_count,
            "byteCount": len(listing),
        },
        "cargoLockHash": sha256_bytes(cargo_lock),
        "frankenStackLockHash": sha256_bytes(franken_lock),
        "cargoTomlSourceHash": sha256_bytes(cargo_toml),
        "cargoTomlSource": cargo_toml,
    }


def rewrite_pattern() -> re.Pattern[str]:
    names = "|".join(re.escape(name) for name in FRANKEN_REPOSITORIES)
    return re.compile(rf"(?:/data/projects|\.\.)/({names})(?=/|[\"'])")


def canonical_rewrite(text: str) -> str:
    return rewrite_pattern().sub(r"<CARGO_PATH_ROOT>/\1", text)


def normalize_effective_cargo_toml(text: str, cargo_path_root: str | None) -> str:
    normalized = text
    if cargo_path_root:
        root = str(Path(cargo_path_root).resolve()).rstrip("/")
        normalized = normalized.replace(root + "/", "<CARGO_PATH_ROOT>/")
    return canonical_rewrite(normalized)


def workspace_cargo_configs(workspace: Path) -> list[dict[str, Any]]:
    records: list[dict[str, Any]] = []
    for relative in (Path(".cargo/config"), Path(".cargo/config.toml")):
        path = workspace / relative
        if path.is_file():
            records.append(
                {
                    "path": relative.as_posix(),
                    "hash": sha256_file(path),
                    "sizeBytes": path.stat().st_size,
                }
            )
    return records


def committed_cargo_configs(
    workspace: Path, resolved_commit: str
) -> list[dict[str, Any]]:
    records: list[dict[str, Any]] = []
    for relative in (".cargo/config", ".cargo/config.toml"):
        result = run_command(
            ["git", "show", f"{resolved_commit}:{relative}"],
            cwd=workspace,
            check=False,
        )
        if result.returncode == 0:
            records.append(
                {
                    "path": relative,
                    "hash": sha256_bytes(result.stdout),
                    "sizeBytes": len(result.stdout),
                }
            )
        elif result.returncode not in (1, 128):
            raise AttestationError(
                f"could not inspect committed Cargo config {relative}"
            )
    return records


def assert_build_workspace_matches_source(
    workspace: Path, resolved_commit: str
) -> None:
    head = require_full_commit(git_text(workspace, "rev-parse", "HEAD"), "workspace HEAD")
    if head != resolved_commit:
        raise AttestationError(
            f"workspace HEAD {head} differs from requested commit {resolved_commit}"
        )
    # Cargo.toml is the one permitted tracked mutation and is validated against
    # the exact normalized rewrite below. Every other tracked build input must
    # still match the commit being attested.
    result = run_command(
        [
            "git",
            "diff",
            "--quiet",
            resolved_commit,
            "--",
            ".",
            ":(exclude)Cargo.toml",
        ],
        cwd=workspace,
        check=False,
    )
    if result.returncode == 1:
        raise AttestationError(
            "tracked build inputs other than Cargo.toml differ from the requested commit"
        )
    if result.returncode != 0:
        raise AttestationError("could not compare tracked build inputs to source")


def build_provenance(
    workspace: Path,
    source: dict[str, Any],
    rewrite_id: str,
    cargo_path_root: str | None,
) -> dict[str, Any]:
    current_cargo_toml = (workspace / "Cargo.toml").read_text(encoding="utf-8")
    source_text = source["cargoTomlSource"].decode("utf-8", errors="strict")
    if rewrite_id == REWRITE_ID:
        normalized_effective = normalize_effective_cargo_toml(
            current_cargo_toml, cargo_path_root
        )
        expected_effective = canonical_rewrite(source_text)
    elif rewrite_id == "none":
        normalized_effective = current_cargo_toml
        expected_effective = source_text
    else:
        raise AttestationError(f"unsupported rewrite id: {rewrite_id}")
    if normalized_effective != expected_effective:
        raise AttestationError(
            "effective Cargo.toml does not match the declared canonical rewrite"
        )

    current_lock_hash = sha256_file(workspace / "Cargo.lock")
    if current_lock_hash != source["cargoLockHash"]:
        raise AttestationError("working Cargo.lock differs from the requested commit")

    actual_configs = workspace_cargo_configs(workspace)
    expected_configs = committed_cargo_configs(workspace, source["resolvedCommit"])
    if actual_configs != expected_configs:
        raise AttestationError(
            "effective workspace Cargo configs differ from the requested commit"
        )

    cargo_home = os.environ.get("CARGO_HOME", "")
    cargo_home_class = "unconfigured"
    if cargo_home:
        try:
            Path(cargo_home).resolve().relative_to(workspace.resolve())
            cargo_home_class = "workspace_isolated"
        except ValueError:
            cargo_home_class = "external"

    if cargo_home_class != "workspace_isolated":
        raise AttestationError(
            "remote artifact builds require a workspace-isolated CARGO_HOME"
        )

    provenance: dict[str, Any] = {
        "schema": PROVENANCE_SCHEMA,
        "rewriteId": rewrite_id,
        "cargoTomlSourceHash": source["cargoTomlSourceHash"],
        # Effective build inputs are hashed after replacing the ephemeral CI
        # checkout root with a stable token.  The raw runner path is neither
        # authority-bearing nor deterministic across otherwise identical runs.
        "cargoTomlEffectiveHash": sha256_bytes(normalized_effective.encode("utf-8")),
        "cargoTomlNormalizedHash": sha256_bytes(normalized_effective.encode("utf-8")),
        "cargoTomlPatchHash": sha256_bytes(
            (source_text + "\0" + normalized_effective).encode("utf-8")
        ),
        "workspaceCargoConfigs": actual_configs,
        "cargoHomeClass": cargo_home_class,
        "frankenStackLockHash": source["frankenStackLockHash"],
    }
    provenance["provenanceHash"] = canonical_hash(provenance)
    return provenance


def expected_provenance(
    workspace: Path,
    source: dict[str, Any],
    rewrite_id: str,
) -> dict[str, Any]:
    source_text = source["cargoTomlSource"].decode("utf-8", errors="strict")
    if rewrite_id == REWRITE_ID:
        normalized_effective = canonical_rewrite(source_text)
    elif rewrite_id == "none":
        normalized_effective = source_text
    else:
        raise AttestationError(f"unsupported rewrite id: {rewrite_id}")
    provenance: dict[str, Any] = {
        "schema": PROVENANCE_SCHEMA,
        "rewriteId": rewrite_id,
        "cargoTomlSourceHash": source["cargoTomlSourceHash"],
        "cargoTomlEffectiveHash": sha256_bytes(normalized_effective.encode("utf-8")),
        "cargoTomlNormalizedHash": sha256_bytes(normalized_effective.encode("utf-8")),
        "cargoTomlPatchHash": sha256_bytes(
            (source_text + "\0" + normalized_effective).encode("utf-8")
        ),
        "workspaceCargoConfigs": committed_cargo_configs(
            workspace, source["resolvedCommit"]
        ),
        "cargoHomeClass": "workspace_isolated",
        "frankenStackLockHash": source["frankenStackLockHash"],
    }
    provenance["provenanceHash"] = canonical_hash(provenance)
    return provenance


def parse_build_command(raw: str) -> list[str]:
    try:
        value = json.loads(raw)
    except json.JSONDecodeError as error:
        raise AttestationError(f"build command is not valid JSON: {error}") from error
    if (
        not isinstance(value, list)
        or not value
        or not all(isinstance(item, str) and item for item in value)
    ):
        raise AttestationError("build command must be a non-empty JSON string array")
    if value[0] != "cargo" or "build" not in value or "--locked" not in value:
        raise AttestationError("build command must be a cargo build invocation with --locked")
    return value


def canonical_build_command(target: str, profile: str) -> list[str]:
    argv = [
        "cargo",
        "build",
        "--locked",
        "--workspace",
        "--bin",
        "ee",
        "--target",
        target,
    ]
    if profile != "debug":
        argv.extend(["--profile", profile])
    return argv


def archive_member_bytes(archive: Path, member_name: str) -> bytes:
    try:
        with tarfile.open(archive, mode="r:gz") as bundle:
            members = bundle.getmembers()
            if len(members) != 1 or members[0].name != member_name or not members[0].isfile():
                raise AttestationError(
                    f"archive must contain only one regular {member_name} member"
                )
            if members[0].size <= 0 or members[0].size > 1024 * 1024 * 1024:
                raise AttestationError("archive ee member size is outside the accepted range")
            extracted = bundle.extractfile(members[0])
            if extracted is None:
                raise AttestationError(f"archive member {member_name} is unreadable")
            return extracted.read()
    except (tarfile.TarError, OSError) as error:
        raise AttestationError(f"archive is unreadable: {error}") from error


def artifact_record(binary: Path, archive: Path) -> dict[str, Any]:
    if not binary.is_file():
        raise AttestationError(f"binary is missing: {binary}")
    if not archive.is_file():
        raise AttestationError(f"archive is missing: {archive}")
    binary_bytes = binary.read_bytes()
    archived_bytes = archive_member_bytes(archive, "ee")
    if archived_bytes != binary_bytes:
        raise AttestationError("archive ee member differs from the probed binary")
    return {
        "binary": {
            "fileName": "ee",
            "hash": sha256_bytes(binary_bytes),
            "sizeBytes": len(binary_bytes),
        },
        "archive": {
            "fileName": archive.name,
            "format": "tar_gz",
            "member": "ee",
            "hash": sha256_file(archive),
            "sizeBytes": archive.stat().st_size,
        },
    }


def probe_specs(
    source_commit: str,
    target: str,
    profile: str,
) -> list[dict[str, Any]]:
    return [
        {
            "id": "version_json",
            "argv": ["ee", "version", "--json"],
            "expectedStdoutContains": ["\"schema\":\"ee.response.v2\""],
            "expectedJsonPointers": {
                "/schema": "ee.response.v2",
                "/success": True,
                "/data/command": "version",
                "/data/schema": "ee.version.provenance.v1",
                "/data/source/gitCommit": source_commit,
                "/data/source/gitDirty": True,
                "/data/source/state": "dirty",
                "/data/build/targetTriple": target,
                "/data/build/profile": profile,
                "/data/provenance/available": True,
            },
        },
        {
            "id": "environment_attestation_help",
            "argv": ["ee", "diag", "environment-attestation", "--help"],
            "expectedStdoutContains": ["environment-attestation"],
            "expectedJsonPointers": {},
        },
    ]


def json_pointer(value: Any, pointer: str) -> Any:
    current = value
    for raw_part in pointer.lstrip("/").split("/"):
        part = raw_part.replace("~1", "/").replace("~0", "~")
        if isinstance(current, dict) and part in current:
            current = current[part]
        else:
            return None
    return current


def run_probe(binary: Path, spec: dict[str, Any]) -> dict[str, Any]:
    argv = [str(binary), *spec["argv"][1:]]
    probe_env = {
        "LANG": "C",
        "LC_ALL": "C",
        "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
    }
    result = run_command(
        argv,
        cwd=binary.parent,
        check=False,
        timeout_seconds=15,
        env=probe_env,
    )
    stdout_text = result.stdout.decode("utf-8", errors="replace")
    missing = [
        marker
        for marker in spec["expectedStdoutContains"]
        if marker not in stdout_text
    ]
    parsed_json: Any = None
    if spec["expectedJsonPointers"]:
        try:
            parsed_json = json.loads(stdout_text)
        except json.JSONDecodeError:
            parsed_json = None
    assertions = []
    for pointer, expected in spec["expectedJsonPointers"].items():
        observed = json_pointer(parsed_json, pointer)
        assertions.append(
            {
                "path": pointer,
                "expected": expected,
                "observed": observed,
                "matched": observed == expected,
            }
        )
    status = (
        "passed"
        if result.returncode == 0
        and not missing
        and all(assertion["matched"] for assertion in assertions)
        else "failed"
    )
    return {
        "id": spec["id"],
        "argv": spec["argv"],
        "argvHash": canonical_hash(spec["argv"]),
        "expectedStdoutContains": spec["expectedStdoutContains"],
        "exitCode": result.returncode,
        "stdoutHash": sha256_bytes(result.stdout),
        "stderrHash": sha256_bytes(result.stderr),
        "status": status,
        "missingSemanticMarkers": missing,
        "semanticAssertions": assertions,
    }


def generate_manifest(args: argparse.Namespace) -> dict[str, Any]:
    workspace = args.workspace.resolve()
    binary = args.binary.resolve()
    archive = args.archive.resolve()
    source = resolve_source(workspace, args.source_commit)
    assert_build_workspace_matches_source(workspace, source["resolvedCommit"])
    provenance = build_provenance(
        workspace, source, args.rewrite_id, args.cargo_path_root
    )
    build_argv = parse_build_command(args.build_command_json)
    expected_argv = canonical_build_command(args.target, args.profile)
    if build_argv != expected_argv:
        raise AttestationError(
            "build command does not match the canonical target/profile invocation"
        )
    command_hash = canonical_hash(build_argv)
    probes = [
        run_probe(binary, spec)
        for spec in probe_specs(source["resolvedCommit"], args.target, args.profile)
    ]
    if any(probe["status"] != "passed" for probe in probes):
        raise AttestationError("post-package behavior probe failed")

    current_lock_hash = sha256_file(workspace / "Cargo.lock")
    source.pop("cargoTomlSource")
    effective_input = {
        "gitTree": source["gitTree"],
        "sourceManifestHash": source["sourceManifest"]["hash"],
        "cargoLockHash": source["cargoLockHash"],
        "frankenStackLockHash": source["frankenStackLockHash"],
        "provenanceHash": provenance["provenanceHash"],
        "commandHash": command_hash,
    }
    source["effectiveInputHash"] = canonical_hash(effective_input)

    manifest: dict[str, Any] = {
        "schema": MANIFEST_SCHEMA,
        "artifactName": args.artifact_name,
        "producer": {
            "repository": args.repository,
            "workflow": args.workflow,
            "runId": args.run_id,
            "runAttempt": args.run_attempt,
        },
        "source": source,
        "build": {
            "argv": build_argv,
            "commandHash": command_hash,
            "target": args.target,
            "profile": args.profile,
            "cargoLocked": "--locked" in build_argv,
            "cargoLockHashBefore": source["cargoLockHash"],
            "cargoLockHashAfter": current_lock_hash,
            "cargoLockUnchanged": current_lock_hash == source["cargoLockHash"],
            "provenance": provenance,
        },
        "artifact": artifact_record(binary, archive),
        "probes": probes,
        "status": "verified",
    }
    manifest["manifestHash"] = manifest_hash(manifest)
    return manifest


def load_manifest(path: Path) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise AttestationError(f"manifest is unreadable: {error}") from error
    if not isinstance(value, dict):
        raise AttestationError("manifest must be a JSON object")
    return value


def validate_object_shape(
    rejections: list[str],
    label: str,
    value: Any,
    expected_keys: set[str],
) -> dict[str, Any]:
    if not isinstance(value, dict):
        rejections.append(f"{label}_not_object")
        return {}
    actual_keys = set(value)
    for key in sorted(expected_keys - actual_keys):
        rejections.append(f"{label}_{key}_missing")
    if actual_keys - expected_keys:
        rejections.append(f"{label}_unknown_fields")
    return value


def validate_manifest_shape(manifest: dict[str, Any], rejections: list[str]) -> None:
    validate_object_shape(
        rejections,
        "manifest",
        manifest,
        {
            "schema",
            "artifactName",
            "producer",
            "source",
            "build",
            "artifact",
            "probes",
            "status",
            "manifestHash",
        },
    )
    validate_object_shape(
        rejections,
        "producer",
        manifest.get("producer"),
        {"repository", "workflow", "runId", "runAttempt"},
    )
    source = validate_object_shape(
        rejections,
        "source",
        manifest.get("source"),
        {
            "requestedCommit",
            "resolvedCommit",
            "gitTree",
            "sourceManifest",
            "cargoLockHash",
            "frankenStackLockHash",
            "cargoTomlSourceHash",
            "effectiveInputHash",
        },
    )
    validate_object_shape(
        rejections,
        "source_manifest",
        source.get("sourceManifest"),
        {"algorithm", "hash", "fileCount", "byteCount"},
    )
    build = validate_object_shape(
        rejections,
        "build",
        manifest.get("build"),
        {
            "argv",
            "commandHash",
            "target",
            "profile",
            "cargoLocked",
            "cargoLockHashBefore",
            "cargoLockHashAfter",
            "cargoLockUnchanged",
            "provenance",
        },
    )
    provenance = validate_object_shape(
        rejections,
        "provenance",
        build.get("provenance"),
        {
            "schema",
            "rewriteId",
            "cargoTomlSourceHash",
            "cargoTomlEffectiveHash",
            "cargoTomlNormalizedHash",
            "cargoTomlPatchHash",
            "workspaceCargoConfigs",
            "cargoHomeClass",
            "frankenStackLockHash",
            "provenanceHash",
        },
    )
    configs = provenance.get("workspaceCargoConfigs")
    if not isinstance(configs, list):
        rejections.append("provenance_workspaceCargoConfigs_not_array")
    else:
        for config in configs:
            validate_object_shape(
                rejections,
                "cargo_config",
                config,
                {"path", "hash", "sizeBytes"},
            )
    artifact = validate_object_shape(
        rejections,
        "artifact",
        manifest.get("artifact"),
        {"binary", "archive"},
    )
    validate_object_shape(
        rejections,
        "binary",
        artifact.get("binary"),
        {"fileName", "hash", "sizeBytes"},
    )
    validate_object_shape(
        rejections,
        "archive",
        artifact.get("archive"),
        {"fileName", "format", "member", "hash", "sizeBytes"},
    )
    probes = manifest.get("probes")
    if not isinstance(probes, list):
        rejections.append("probes_not_array")
        return
    for probe in probes:
        probe_object = validate_object_shape(
            rejections,
            "probe",
            probe,
            {
                "id",
                "argv",
                "argvHash",
                "expectedStdoutContains",
                "exitCode",
                "stdoutHash",
                "stderrHash",
                "status",
                "missingSemanticMarkers",
                "semanticAssertions",
            },
        )
        assertions = probe_object.get("semanticAssertions")
        if not isinstance(assertions, list):
            rejections.append("probe_semanticAssertions_not_array")
            continue
        for assertion in assertions:
            validate_object_shape(
                rejections,
                "probe_semantic_assertion",
                assertion,
                {"path", "expected", "observed", "matched"},
            )


def validate_producer_args(args: argparse.Namespace, run_id_field: str) -> None:
    if not re.fullmatch(r"[^/\s]+/[^/\s]+", args.repository):
        raise AttestationError("repository must have owner/name form")
    run_id = getattr(args, run_id_field)
    if not re.fullmatch(r"[1-9][0-9]*", run_id):
        raise AttestationError("run id must be a positive decimal GitHub run ID")
    if args.workflow != "macOS EE Artifact":
        raise AttestationError("unsupported artifact-producing workflow")


def expect_equal(rejections: list[str], label: str, actual: Any, expected: Any) -> None:
    if actual != expected:
        rejections.append(f"{label}_mismatch")


def verify_checksum_sidecar(archive: Path, checksum: Path) -> str:
    if not checksum.is_file():
        return "missing"
    try:
        fields = checksum.read_text(encoding="utf-8").strip().split()
    except (OSError, UnicodeError):
        return "mismatch"
    if not fields:
        return "mismatch"
    expected = fields[0].lower()
    actual = sha256_file(archive).split(":", 1)[1]
    return "verified" if re.fullmatch(r"[0-9a-f]{64}", expected) and expected == actual else "mismatch"


def verify_manifest(args: argparse.Namespace) -> tuple[dict[str, Any], int]:
    workspace = args.workspace.resolve()
    binary = args.binary.resolve()
    archive = args.archive.resolve()
    checksum = args.checksum.resolve()
    rejections: list[str] = []
    probe_reports: list[dict[str, Any]] = []
    manifest: dict[str, Any]
    try:
        manifest = load_manifest(args.manifest.resolve())
    except AttestationError as error:
        manifest = {}
        rejections.append("manifest_unreadable")
        return verification_report(
            manifest, rejections, "missing", probe_reports, args.artifact_id
        ), 1

    validate_manifest_shape(manifest, rejections)

    if manifest.get("schema") != MANIFEST_SCHEMA:
        rejections.append("manifest_schema_unsupported")
    claimed_manifest_hash = manifest.get("manifestHash")
    if not isinstance(claimed_manifest_hash, str) or claimed_manifest_hash != manifest_hash(manifest):
        rejections.append("manifest_hash_mismatch")
    if manifest.get("status") != "verified":
        rejections.append("manifest_status_unverified")

    producer = (
        manifest.get("producer") if isinstance(manifest.get("producer"), dict) else {}
    )
    expect_equal(rejections, "producer_repository", producer.get("repository"), args.repository)
    expect_equal(rejections, "producer_workflow", producer.get("workflow"), args.workflow)
    expect_equal(rejections, "producer_run_id", producer.get("runId"), args.expected_run_id)
    if not isinstance(producer.get("runAttempt"), int) or producer.get("runAttempt", 0) < 1:
        rejections.append("producer_run_attempt_invalid")

    try:
        expected_source = resolve_source(workspace, args.expected_commit)
    except AttestationError:
        expected_source = {}
        rejections.append("expected_source_unavailable")
    source = manifest.get("source") if isinstance(manifest.get("source"), dict) else {}
    for key in (
        "requestedCommit",
        "resolvedCommit",
        "gitTree",
        "cargoLockHash",
        "frankenStackLockHash",
        "cargoTomlSourceHash",
    ):
        expect_equal(rejections, f"source_{key}", source.get(key), expected_source.get(key))
    expect_equal(
        rejections,
        "source_manifest",
        source.get("sourceManifest"),
        expected_source.get("sourceManifest"),
    )

    build = manifest.get("build") if isinstance(manifest.get("build"), dict) else {}
    argv = build.get("argv")
    if not isinstance(argv, list) or not argv or not all(isinstance(item, str) for item in argv):
        rejections.append("build_argv_invalid")
        argv = []
    expected_argv = canonical_build_command(args.target, args.profile)
    expect_equal(rejections, "build_argv", argv, expected_argv)
    expect_equal(rejections, "build_command_hash", build.get("commandHash"), canonical_hash(argv))
    if "--locked" not in argv or build.get("cargoLocked") is not True:
        rejections.append("build_not_locked")
    expect_equal(rejections, "build_target", build.get("target"), args.target)
    expect_equal(rejections, "build_profile", build.get("profile"), args.profile)
    expect_equal(
        rejections,
        "cargo_lock_before",
        build.get("cargoLockHashBefore"),
        expected_source.get("cargoLockHash"),
    )
    expect_equal(
        rejections,
        "cargo_lock_after",
        build.get("cargoLockHashAfter"),
        expected_source.get("cargoLockHash"),
    )
    if build.get("cargoLockUnchanged") is not True:
        rejections.append("cargo_lock_changed")

    provenance = build.get("provenance") if isinstance(build.get("provenance"), dict) else {}
    rewrite_id = provenance.get("rewriteId")
    expected_build_provenance: dict[str, Any] = {}
    if expected_source and isinstance(rewrite_id, str):
        try:
            expected_build_provenance = expected_provenance(
                workspace, expected_source, rewrite_id
            )
        except AttestationError:
            rejections.append("provenance_rewrite_unknown")
    if not expected_build_provenance:
        rejections.append("provenance_unverifiable")
    else:
        expect_equal(
            rejections,
            "provenance",
            provenance,
            expected_build_provenance,
        )
    claimed_provenance_hash = provenance.get("provenanceHash")

    if expected_source and argv and claimed_provenance_hash:
        effective_input = {
            "gitTree": expected_source["gitTree"],
            "sourceManifestHash": expected_source["sourceManifest"]["hash"],
            "cargoLockHash": expected_source["cargoLockHash"],
            "frankenStackLockHash": expected_source["frankenStackLockHash"],
            "provenanceHash": claimed_provenance_hash,
            "commandHash": canonical_hash(argv),
        }
        expect_equal(
            rejections,
            "source_effective_input",
            source.get("effectiveInputHash"),
            canonical_hash(effective_input),
        )

    artifact = manifest.get("artifact") if isinstance(manifest.get("artifact"), dict) else {}
    binary_record = artifact.get("binary") if isinstance(artifact.get("binary"), dict) else {}
    archive_record = artifact.get("archive") if isinstance(artifact.get("archive"), dict) else {}
    observed_artifact: dict[str, Any] | None = None
    try:
        observed_artifact = artifact_record(binary, archive)
        expect_equal(rejections, "binary_record", binary_record, observed_artifact["binary"])
        expect_equal(rejections, "archive_record", archive_record, observed_artifact["archive"])
    except AttestationError:
        rejections.append("artifact_bytes_mismatch")

    checksum_status = verify_checksum_sidecar(archive, checksum)
    if checksum_status != "verified":
        rejections.append(f"checksum_{checksum_status}")

    manifest_probes = manifest.get("probes")
    expected_specs = probe_specs(args.expected_commit, args.target, args.profile)
    if not isinstance(manifest_probes, list) or len(manifest_probes) != len(expected_specs):
        rejections.append("probes_missing")
        manifest_probes = []
    for index, spec in enumerate(expected_specs):
        observed = run_probe(binary, spec)
        probe_reports.append(observed)
        if index >= len(manifest_probes) or not isinstance(manifest_probes[index], dict):
            rejections.append(f"probe_{spec['id']}_missing")
            continue
        recorded = manifest_probes[index]
        for key in (
            "id",
            "argv",
            "argvHash",
            "expectedStdoutContains",
            "exitCode",
            "stdoutHash",
            "stderrHash",
            "status",
            "missingSemanticMarkers",
            "semanticAssertions",
        ):
            expect_equal(
                rejections,
                f"probe_{spec['id']}_{key}",
                recorded.get(key),
                observed.get(key),
            )
        if recorded.get("status") != "passed" or observed.get("status") != "passed":
            rejections.append(f"probe_{spec['id']}_failed")

    if observed_artifact is not None:
        try:
            expect_equal(
                rejections,
                "artifact_post_probe",
                artifact_record(binary, archive),
                observed_artifact,
            )
        except AttestationError:
            rejections.append("artifact_post_probe_unreadable")

    if manifest.get("artifactName") != args.artifact_name:
        rejections.append("artifact_name_mismatch")
    rejections = sorted(set(rejections))
    return verification_report(
        manifest, rejections, checksum_status, probe_reports, args.artifact_id
    ), (0 if not rejections else 1)


def verification_report(
    manifest: dict[str, Any],
    rejections: list[str],
    checksum_status: str,
    probes: list[dict[str, Any]],
    artifact_id: str | None,
) -> dict[str, Any]:
    source = manifest.get("source") if isinstance(manifest.get("source"), dict) else {}
    producer = manifest.get("producer") if isinstance(manifest.get("producer"), dict) else {}
    build = manifest.get("build") if isinstance(manifest.get("build"), dict) else {}
    artifact = manifest.get("artifact") if isinstance(manifest.get("artifact"), dict) else {}
    binary = artifact.get("binary") if isinstance(artifact.get("binary"), dict) else {}
    archive = artifact.get("archive") if isinstance(artifact.get("archive"), dict) else {}
    report = {
        "schema": VERIFICATION_SCHEMA,
        "status": "verified" if not rejections else "rejected",
        "accepted": not rejections,
        "artifactName": manifest.get("artifactName"),
        "artifactId": artifact_id,
        "repository": producer.get("repository"),
        "workflow": producer.get("workflow"),
        "runId": producer.get("runId"),
        "runAttempt": producer.get("runAttempt"),
        "sourceCommit": source.get("resolvedCommit"),
        "gitTree": source.get("gitTree"),
        "manifestHash": manifest.get("manifestHash"),
        "buildCommandHash": build.get("commandHash"),
        "effectiveInputHash": source.get("effectiveInputHash"),
        "provenanceHash": (
            build.get("provenance", {}).get("provenanceHash")
            if isinstance(build.get("provenance"), dict)
            else None
        ),
        "target": build.get("target"),
        "profile": build.get("profile"),
        "binaryHash": binary.get("hash"),
        "archiveHash": archive.get("hash"),
        "archiveSizeBytes": archive.get("sizeBytes"),
        "checksumStatus": checksum_status,
        "probeStatus": "passed"
        if probes and all(probe.get("status") == "passed" for probe in probes)
        else "failed",
        "probes": [
            {
                "id": probe.get("id"),
                "argvHash": probe.get("argvHash"),
                "exitCode": probe.get("exitCode"),
                "stdoutHash": probe.get("stdoutHash"),
                "stderrHash": probe.get("stderrHash"),
                "status": probe.get("status"),
                "semanticAssertions": probe.get("semanticAssertions", []),
            }
            for probe in probes
        ],
        "rejections": sorted(set(rejections)),
        "rawOutputIncluded": False,
    }
    report["verificationHash"] = canonical_hash(report)
    return report


def write_json(value: dict[str, Any], output: Path | None) -> None:
    rendered = json.dumps(value, ensure_ascii=False, sort_keys=True, indent=2) + "\n"
    if output is None:
        sys.stdout.write(rendered)
    else:
        output.parent.mkdir(parents=True, exist_ok=True)
        output.write_text(rendered, encoding="utf-8")


def parser() -> argparse.ArgumentParser:
    root = argparse.ArgumentParser(description=__doc__)
    subparsers = root.add_subparsers(dest="command", required=True)

    generate = subparsers.add_parser("generate", help="generate a strict artifact manifest")
    generate.add_argument("--workspace", type=Path, required=True)
    generate.add_argument("--binary", type=Path, required=True)
    generate.add_argument("--archive", type=Path, required=True)
    generate.add_argument("--source-commit", required=True)
    generate.add_argument("--build-command-json", required=True)
    generate.add_argument("--rewrite-id", choices=(REWRITE_ID, "none"), required=True)
    generate.add_argument("--cargo-path-root")
    generate.add_argument("--artifact-name", default=ARTIFACT_NAME)
    generate.add_argument("--repository", required=True)
    generate.add_argument("--workflow", default="macOS EE Artifact")
    generate.add_argument("--run-id", required=True)
    generate.add_argument("--run-attempt", type=int, required=True)
    generate.add_argument("--target", default=TARGET_TRIPLE)
    generate.add_argument("--profile", default="debug")
    generate.add_argument("--output", type=Path, required=True)

    verify = subparsers.add_parser("verify", help="verify and behavior-probe an artifact")
    verify.add_argument("--workspace", type=Path, required=True)
    verify.add_argument("--binary", type=Path, required=True)
    verify.add_argument("--archive", type=Path, required=True)
    verify.add_argument("--checksum", type=Path, required=True)
    verify.add_argument("--manifest", type=Path, required=True)
    verify.add_argument("--expected-commit", required=True)
    verify.add_argument("--repository", required=True)
    verify.add_argument("--workflow", default="macOS EE Artifact")
    verify.add_argument("--expected-run-id", required=True)
    verify.add_argument("--artifact-id")
    verify.add_argument("--artifact-name", default=ARTIFACT_NAME)
    verify.add_argument("--target", default=TARGET_TRIPLE)
    verify.add_argument("--profile", default="debug")
    verify.add_argument("--output", type=Path)
    return root


def main() -> int:
    args = parser().parse_args()
    try:
        if args.command == "generate":
            validate_producer_args(args, "run_id")
            if args.run_attempt < 1:
                raise AttestationError("run attempt must be positive")
            write_json(generate_manifest(args), args.output)
            return 0
        validate_producer_args(args, "expected_run_id")
        if args.artifact_id is not None and not re.fullmatch(r"[1-9][0-9]*", args.artifact_id):
            raise AttestationError("artifact id must be a positive decimal GitHub artifact ID")
        report, exit_code = verify_manifest(args)
        write_json(report, args.output)
        return exit_code
    except (AttestationError, OSError, UnicodeError) as error:
        if getattr(args, "command", None) == "verify":
            write_json(
                verification_report(
                    {}, ["verification_internal_error"], "missing", [], None
                ),
                getattr(args, "output", None),
            )
        else:
            print(f"ci_artifact_attestation: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
