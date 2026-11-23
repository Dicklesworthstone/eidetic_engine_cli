#!/usr/bin/env python3
"""Emit, populate, and cryptographically verify ee.release_candidate_proof.v1 capsules.

Supports three modes:
1. Skeleton mode (used by scripts/rch_run.sh for single-command dispatches):
   emit_proof_capsule.py --log L --base SHA --dirty N --host H --run-exit E
                         --command C --out PATH
   Always emits `completeness.populated = false` with structured
   `completeness.unestablished` objects `{item, reason, blockedBy}`.

2. Candidate population mode (bd-reality-core-convergence-1azkt.76):
   emit_proof_capsule.py --populate-candidate --candidate-manifest M [--out PATH]
   Ingests a manifest and hashes the supplied stage logs, candidate binaries,
   and evidence. Native entries can bind their staged archive and probe too.
   `completeness.populated` describes the evidence declared in that manifest.

3. Cryptographic verification mode (bd-reality-core-convergence-1azkt.76):
   emit_proof_capsule.py --verify-capsule PATH --expected-sha SHA
                         [--expected-binary BIN] [--verify-logs]
   Recomputes `capsuleHash` over every field except `capsuleHash` and
   `generatedAt`, validates recursively against the committed schema, and fails
   closed (`EE_CAPSULE_RC=1`, exit 1) on tamper, SHA mismatch, missing stages,
   or `completeness.populated != true`.
   With --release-archive PATH --release-probe PATH --expected-version VERSION,
   it checks stage log evidence, the candidate's committed lockfiles, and the
   native archive, executable and probe bytes for that target.
   With --verify-rch-policy it requires the committed Rust stage inventory,
   with matching pinned RCH receipts and their retained terminal streams.
   With --dsr-manifest PATH --staging-dir PATH --expected-version VERSION, it
   applies the Rust and archive target policies at DSR staging.

4. Self-test mode:
   emit_proof_capsule.py --self-test
"""

from __future__ import annotations

import argparse
import datetime
import hashlib
import hmac
import json
import math
import pathlib
import platform
import re
import subprocess
import sys
import tarfile
from typing import Any

HERE = pathlib.Path(__file__).resolve().parent
REPO = HERE.parents[1]
SCHEMA = REPO / "docs" / "schemas" / "ee.release_candidate_proof.v1.json"

HEX40_RE = re.compile(r"^[0-9a-f]{40}$")
HEX64_RE = re.compile(r"^[0-9a-f]{64}$")
RFC3339_RE = re.compile(
    r"^\d{4}-\d{2}-\d{2}[Tt]\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:[Zz]|[+-]\d{2}:\d{2})$"
)
ZERO_SHA40 = "0" * 40
RELEASE_PROBE_SCHEMA = "ee.release_probe.v1"
RELEASE_PROBE_CHECKS = {
    "neural_backend_active": "precondition",
    "walking_skeleton": "check",
    "search_deterministic_parallel": "check",
    "pack_deterministic_parallel": "check",
    "ask_direct_hit": "check",
    "rule_searchable": "check",
    "rule_packs_beside_source": "check",
    "unrelated_query_abstains": "check",
    "status_doctor_agree": "check",
    "model_missing_fallback": "check",
    "model_receipt_fresh": "check",
    "learn_loop_applies": "check",
    "concurrent_writes_intact": "check",
    "target_triple_matches": "check",
}


def _reject_json_constant(const: str) -> Any:
    raise ValueError(f"invalid non-standard JSON constant {const!r}")


def _unique_json_object(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result = dict(pairs)
    if len(result) != len(pairs):
        raise ValueError("JSON object has duplicate fields")
    return result


def _is_strict_int(value: Any) -> bool:
    return isinstance(value, int) and not isinstance(value, bool)

sys.path.insert(0, str(HERE))


def _unestablished(item: str, reason: str, blocked_by: str | None = None) -> dict[str, Any]:
    return {"item": item, "reason": reason, "blockedBy": blocked_by}


def _tool_version(binary: str) -> str | None:
    try:
        out = subprocess.run(
            [binary, "--version"], capture_output=True, text=True, timeout=30
        )
        return out.stdout.strip() or None
    except (FileNotFoundError, subprocess.TimeoutExpired):
        return None


def _resolve_commit_sha(base: str, repo_root: pathlib.Path) -> tuple[str, bool]:
    """Return (40-hex commit, resolved_cleanly)."""
    candidate = base.strip().lower()
    if HEX40_RE.fullmatch(candidate):
        return candidate, True
    rev = "HEAD" if not base.strip() or base.strip() == "unknown" else base.strip()
    try:
        out = subprocess.run(
            ["git", "rev-parse", "--verify", f"{rev}^{{commit}}"],
            cwd=str(repo_root),
            capture_output=True,
            text=True,
            timeout=10,
        )
        resolved = out.stdout.strip().lower()
        if out.returncode == 0 and HEX40_RE.fullmatch(resolved):
            return resolved, True
    except (FileNotFoundError, subprocess.TimeoutExpired):
        pass
    return ZERO_SHA40, False


def _sha256_file(path: pathlib.Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(65536), b""):
            h.update(chunk)
    return h.hexdigest()


def _sha256_bytes(payload: bytes) -> str:
    return hashlib.sha256(payload).hexdigest()


def _committed_blob(repo_root: pathlib.Path, commit: str, path: str) -> bytes:
    if not HEX40_RE.fullmatch(commit) or commit == ZERO_SHA40:
        raise ValueError("expected a full candidate commit SHA")
    command = ["git", "show", "--no-textconv", f"{commit}^{{commit}}:{path}"]
    result = subprocess.run(command, cwd=repo_root, capture_output=True, timeout=10)
    if result.returncode != 0:
        raise ValueError(f"could not read {path} from the candidate tree")
    return result.stdout


def verify_candidate_tree(
    capsule: dict[str, Any], repo_root: pathlib.Path, expected_version: str
) -> list[str]:
    try:
        import tomllib
    except ModuleNotFoundError:
        return ["candidate tree verification requires Python 3.11 or later"]

    commit = capsule["source"]["commit"]
    try:
        cargo_manifest = tomllib.loads(_committed_blob(repo_root, commit, "Cargo.toml").decode())
        cargo_lock = _sha256_bytes(_committed_blob(repo_root, commit, "Cargo.lock"))
        franken_lock = _sha256_bytes(_committed_blob(repo_root, commit, "franken-stack.lock"))
    except (OSError, ValueError, subprocess.TimeoutExpired) as exc:
        return [str(exc)]
    if cargo_manifest.get("package", {}).get("version") != expected_version:
        return ["expected release version differs from candidate Cargo.toml"]
    dependencies = capsule["effectiveDependencies"]
    expected = {
        "cargoLockSha256": cargo_lock,
        "frankenStackLockSha256": franken_lock,
        "lockfileHash": franken_lock,
    }
    return [
        f"candidate tree {field} differs from capsule"
        for field, digest in expected.items()
        if dependencies.get(field) != digest
    ]


def _archive_executable_sha256(archive: pathlib.Path, executable_name: str) -> str:
    executable_sha = None
    with tarfile.open(archive, mode="r|xz") as contents:
        for member in contents:
            name = pathlib.PurePosixPath(member.name)
            if name.name != executable_name:
                continue
            if (
                executable_sha is not None
                or not member.isfile()
                or name.is_absolute()
                or ".." in name.parts
            ):
                raise ValueError(f"expected exactly one regular {executable_name} member")
            handle = contents.extractfile(member)
            if handle is None:
                raise ValueError(f"could not read {executable_name} member")
            with handle:
                digest = hashlib.sha256()
                for chunk in iter(lambda: handle.read(65536), b""):
                    digest.update(chunk)
                executable_sha = digest.hexdigest()
    if executable_sha is None:
        raise ValueError(f"archive contains no {executable_name} member")
    return executable_sha


def native_release_artifact_bindings(
    binary: dict[str, Any], archive: pathlib.Path, probe_path: pathlib.Path, commit: str
) -> dict[str, str]:
    """Validate the full native probe and return digests over the staged bytes."""
    target = binary["target"]
    if archive.name != f"ee-{target}.tar.xz":
        raise ValueError(f"archive filename does not match target {target}")
    probe_bytes = probe_path.read_bytes()
    probe = json.loads(
        probe_bytes, parse_constant=_reject_json_constant, object_pairs_hook=_unique_json_object,
    )
    if not isinstance(probe, dict) or probe.get("schema") != RELEASE_PROBE_SCHEMA:
        raise ValueError("invalid release probe schema")
    if probe.get("verdict") != "pass" or probe.get("failed") != []:
        raise ValueError("release probe did not report a complete pass")
    checks = probe.get("checks")
    if not isinstance(checks, list):
        raise ValueError("release probe lacks its check inventory")
    seen = set()
    for row in checks:
        if not isinstance(row, dict):
            raise ValueError("invalid release probe check")
        check_id = row.get("id")
        if not isinstance(check_id, str) or check_id not in RELEASE_PROBE_CHECKS or check_id in seen:
            raise ValueError("release probe contains an unknown or duplicate check")
        seen.add(check_id)
        if row.get("kind") != RELEASE_PROBE_CHECKS[check_id] or row.get("pass") is not True:
            raise ValueError(f"release probe check {check_id} did not pass")
        observed = row.get("observed")
        if not isinstance(observed, str) or not observed.strip():
            raise ValueError(f"release probe check {check_id} did not retain an observation")
        if check_id == "neural_backend_active" and observed != "embed_backend=neural_local":
            raise ValueError("release probe did not observe the neural backend")
    if seen != RELEASE_PROBE_CHECKS.keys():
        raise ValueError("release probe omitted mandatory checks")

    identity = probe.get("binary")
    if not isinstance(identity, dict) or identity.get("gitDirty") is not False:
        raise ValueError("release probe must report a clean executable")
    expected = {"gitCommit": commit, "version": binary["version"], "targetTriple": target}
    if any(identity.get(field) != value for field, value in expected.items()):
        raise ValueError("release probe executable source, version or target differs from candidate")

    archive_sha = _sha256_file(archive)
    archive_receipt = probe.get("archive")
    if not isinstance(archive_receipt, dict) or archive_receipt.get("sha256") != archive_sha:
        raise ValueError("staged archive SHA-256 differs from release probe")
    executable_sha = _archive_executable_sha256(archive, "ee")
    if executable_sha != identity.get("sha256") or executable_sha != binary["sha256"]:
        raise ValueError("archived executable SHA-256 differs from probe or candidate binary")
    return {"archiveSha256": archive_sha, "probeSha256": _sha256_bytes(probe_bytes)}


def compute_capsule_hash(capsule: dict[str, Any]) -> str:
    """Content address of every field except capsuleHash and generatedAt."""
    hash_input = {
        key: value
        for key, value in capsule.items()
        if key not in ("capsuleHash", "generatedAt")
    }
    encoded = json.dumps(
        hash_input, sort_keys=True, separators=(",", ":"), allow_nan=False
    ).encode("utf-8")
    return hashlib.blake2b(encoded, digest_size=32).hexdigest()


def host_cfg_label() -> str:
    system = platform.system().lower()
    family = "unix" if system in {"darwin", "linux", "freebsd"} else system
    return f"target_family={family}, target_os={system}"


def classify_terminator(run_exit: int, no_verdict_cause: str | None) -> str:
    """Classify a dispatched verdict's terminator (bd-reality-core-convergence-1azkt.70)."""
    if run_exit == 124 or no_verdict_cause == "TIMEOUT":
        return "timeout"
    if (
        run_exit in (130, 137, 143)
        or (128 < run_exit <= 192)
        or no_verdict_cause == "CANCELLED"
    ):
        return "signal"
    if (
        no_verdict_cause in ("NOT_RUN", "INFRA_ERROR")
        or run_exit == 3
        or (70 <= run_exit <= 79)
    ):
        return "infra_refusal"
    return "product_verdict"


def build_capsule(
    log: pathlib.Path | None,
    base: str,
    dirty: int,
    host: str | None,
    run_exit: int,
    command: str,
    repo_root: pathlib.Path = REPO,
) -> dict[str, Any]:
    from grade_test_log import parse, strip_ansi  # noqa: F401  (same directory)

    unestablished: list[dict[str, Any]] = []
    results: list[dict[str, Any]] = []

    commit_sha, commit_resolved = _resolve_commit_sha(base, repo_root)
    if not commit_resolved:
        unestablished.append(
            _unestablished(
                "source.commit",
                f"could not resolve 40-hex commit SHA from base {base!r}",
            )
        )

    pairs: list[dict[str, Any]] = []
    problems: list[str] = []
    announced_targets: list[str] = []
    if log is not None and log.is_file():
        pairs, problems, announced_targets = parse(
            log.read_text(errors="replace").splitlines()
        )
    else:
        unestablished.append(
            _unestablished("results", "no readable log was supplied")
        )

    for pair in pairs:
        reconciles = pair["announced"] == pair["counted"]
        if pair["announced"] == 0 or not reconciles:
            status, cause = "INFRA_ERROR", "NOT_RUN"
        elif run_exit == 124:
            status, cause = "TIMEOUT", "TIMEOUT"
        elif run_exit in (130, 143):
            status, cause = "CANCELLED", "CANCELLED"
        elif run_exit == 137 or (128 < run_exit <= 192):
            status, cause = "INFRA_ERROR", "INFRA_ERROR"
        elif pair["verdict"] == "ok" and pair["failed"] == 0 and run_exit == 0:
            status, cause = "PASS", None
        else:
            status, cause = "FAIL", None
        results.append(
            {
                "stage": pair["target"],
                "status": status,
                "exitCode": run_exit,
                "target": pair["target"],
                "announced": pair["announced"],
                "executed": pair["counted"],
                "workerHost": host,
                "noVerdictCause": cause,
                "terminatorClass": classify_terminator(run_exit, cause),
            }
        )
    for problem in problems:
        results.append(
            {
                "stage": "unattributed",
                "status": "INFRA_ERROR",
                "noVerdictCause": "NOT_RUN",
                "terminatorClass": classify_terminator(run_exit, "NOT_RUN"),
                "reason": problem,
                "workerHost": host,
            }
        )

    if len(announced_targets) > 1:
        unestablished.append(
            _unestablished(
                "results[].stage/target",
                f"{len(announced_targets)} targets announced in one log "
                f"({', '.join(announced_targets)}), so each summary's attribution to "
                "a target is ADVISORY -- cargo interleaves `Running` lines under "
                "`-j N` and nothing in the log binds a summary to a target. "
                "Dispatch one target per invocation when per-target attribution "
                "has to be established.",
            )
        )

    if host is None:
        unestablished.append(
            _unestablished(
                "results[].workerHost",
                "the run log did not name a worker; a verdict without a host is reported, not reproducible",
            )
        )

    local_host = platform.node().split(".")[0]
    run_was_remote = host is not None and host != local_host
    if run_was_remote:
        evaluated_cfg = None
        unestablished.append(
            _unestablished(
                "toolchain.evaluatedUnderCfg",
                f"the tests ran on {host}, not on this machine ({local_host}), so the cfg set they were evaluated under is not knowable here.",
            )
        )
    else:
        evaluated_cfg = host_cfg_label()
    if dirty:
        unestablished.append(
            _unestablished(
                "source.treeish",
                f"the tree carried {dirty} uncommitted file(s), so this verdict is bound to a working state rather than to a commit",
            )
        )

    for item, reason, blocked_by in (
        ("effectiveDependencies.siblingPins", "not resolved by this emitter", None),
        ("binaries", "no candidate binary was hashed by this run", None),
        ("testInventory", "no cross-shard logical-test inventory was computed", None),
        ("performance.samples", "this emitter records no timing samples", None),
        ("evidenceHashes", "no evidence artifacts were hashed", None),
        (
            "runIdentifiers.hostedCi",
            "hosted workflows are disabled (bd-hvlm2), so there is no hosted run id to bind -- recorded as null, never invented",
            "bd-hvlm2",
        ),
        (
            "manifest.manifestHash",
            "this run did not execute through scripts/verify.sh, so no manifest governed it.",
            "bd-pnj3s",
        ),
    ):
        unestablished.append(_unestablished(item, reason, blocked_by))

    franken_lock = repo_root / "franken-stack.lock"
    cargo_lock = repo_root / "Cargo.lock"
    franken_lock_hash = _sha256_file(franken_lock) if franken_lock.is_file() else None
    cargo_lock_hash = _sha256_file(cargo_lock) if cargo_lock.is_file() else None
    lock_hash = franken_lock_hash or cargo_lock_hash
    if lock_hash is None:
        unestablished.append(
            _unestablished(
                "effectiveDependencies.lockfileHash",
                "neither franken-stack.lock nor Cargo.lock was present to hash",
            )
        )

    capsule: dict[str, Any] = {
        "schema": "ee.release_candidate_proof.v1",
        "capsuleHash": ZERO_SHA40 + "0" * 24,
        "generatedAt": datetime.datetime.now(datetime.timezone.utc).isoformat(),
        "completeness": {"populated": False, "unestablished": unestablished},
        "source": {"treeish": base, "commit": commit_sha, "dirty": bool(dirty)},
        "effectiveDependencies": {
            "lockfileHash": lock_hash,
            "cargoLockSha256": cargo_lock_hash,
            "frankenStackLockSha256": franken_lock_hash,
            "siblingPins": [],
        },
        "toolchain": {
            "rustc": _tool_version("rustc") or "unknown",
            "cargo": _tool_version("cargo") or "unknown",
            "channel": "unknown",
            "target": platform.machine(),
            "evaluatedUnderCfg": evaluated_cfg,
        },
        "manifest": {"stageCount": len(results), "manifestHash": None},
        "binaries": [],
        "testInventory": {"logicalTests": None, "shards": []},
        "results": results,
        "performance": {"samples": []},
        "runIdentifiers": {"rch": command, "hostedCi": None},
        "evidenceHashes": [],
    }
    capsule["capsuleHash"] = compute_capsule_hash(capsule)
    return capsule


def populate_candidate_capsule(
    candidate_manifest: dict[str, Any],
    repo_root: pathlib.Path = REPO,
    base_override: str | None = None,
    dirty_override: int | None = None,
) -> dict[str, Any]:
    """Assemble a candidate proof capsule from verified stage/binary inputs."""
    unestablished: list[dict[str, Any]] = []

    raw_base = base_override if base_override is not None else str(candidate_manifest.get("base", ""))
    commit_sha, commit_resolved = _resolve_commit_sha(raw_base, repo_root)
    if not commit_resolved or commit_sha == ZERO_SHA40:
        unestablished.append(
            _unestablished(
                "source.commit",
                f"candidate base {raw_base!r} is not a valid 40-hex commit SHA",
            )
        )

    if dirty_override is not None:
        dirty_val: Any = dirty_override
        dirty = bool(dirty_val)
    else:
        raw_dirty = candidate_manifest.get("dirty", 0)
        if raw_dirty is None:
            dirty_val = 0
            dirty = False
        elif isinstance(raw_dirty, bool):
            dirty_val = int(raw_dirty)
            dirty = raw_dirty
        elif _is_strict_int(raw_dirty) and raw_dirty >= 0:
            dirty_val = raw_dirty
            dirty = bool(raw_dirty)
        else:
            dirty_val = raw_dirty
            dirty = True
            unestablished.append(
                _unestablished(
                    "source.dirty",
                    f"invalid dirty value {raw_dirty!r}; expected non-negative integer or boolean",
                )
            )
    if dirty and not any(u["item"] == "source.dirty" for u in unestablished):
        unestablished.append(
            _unestablished(
                "source.dirty",
                f"working tree reported dirty={dirty_val}; candidate requires a clean tree",
            )
        )

    def lock_hash_for(path: str) -> str | None:
        try:
            return _sha256_bytes(_committed_blob(repo_root, commit_sha, path))
        except (OSError, ValueError, subprocess.TimeoutExpired):
            lock_path = repo_root / path
            return _sha256_file(lock_path) if lock_path.is_file() else None

    cargo_lock_hash = lock_hash_for("Cargo.lock")
    franken_lock_hash = lock_hash_for("franken-stack.lock")
    lock_hash = franken_lock_hash or cargo_lock_hash
    if lock_hash is None:
        unestablished.append(
            _unestablished(
                "effectiveDependencies.lockfileHash",
                "neither franken-stack.lock nor Cargo.lock exists under repo root",
            )
        )

    raw_manifest_path = candidate_manifest.get("manifestPath")
    manifest_rel = (
        raw_manifest_path
        if isinstance(raw_manifest_path, str) and raw_manifest_path
        else "scripts/verify-budget.toml"
    )
    if raw_manifest_path is not None and not isinstance(raw_manifest_path, str):
        unestablished.append(
            _unestablished(
                "manifest.manifestPath",
                f"manifestPath must be a string, got {type(raw_manifest_path).__name__}",
            )
        )
    manifest_file = (
        pathlib.Path(manifest_rel)
        if pathlib.Path(manifest_rel).is_absolute()
        else (repo_root / manifest_rel)
    )
    raw_required_stages = candidate_manifest.get("requiredStages", [])
    if isinstance(raw_required_stages, list) and all(
        isinstance(s, str) for s in raw_required_stages
    ):
        required_stages = list(raw_required_stages)
    else:
        required_stages = []
        unestablished.append(
            _unestablished(
                "manifest.requiredStages",
                "requiredStages must be a list of strings",
            )
        )
    if manifest_file.is_file():
        manifest_payload = {
            "manifestPath": manifest_rel,
            "verifyBudgetSha256": _sha256_file(manifest_file),
            "requiredStages": sorted(required_stages),
        }
        manifest_hash = _sha256_bytes(
            json.dumps(
                manifest_payload, sort_keys=True, separators=(",", ":"), allow_nan=False
            ).encode("utf-8")
        )
    else:
        manifest_hash = None
        unestablished.append(
            _unestablished(
                "manifest.manifestHash",
                f"verification manifest file missing: {manifest_rel}",
            )
        )

    binaries: list[dict[str, Any]] = []
    raw_binaries = candidate_manifest.get("binaries", [])
    if not isinstance(raw_binaries, list):
        unestablished.append(
            _unestablished(
                "binaries",
                f"binaries must be a list, got {type(raw_binaries).__name__}",
            )
        )
        raw_binaries = []
    for idx, entry in enumerate(raw_binaries):
        if not isinstance(entry, dict):
            unestablished.append(
                _unestablished(f"binaries[{idx}]", "binary entry must be an object")
            )
            continue
        raw_path_val = entry.get("path", "")
        if not isinstance(raw_path_val, str):
            unestablished.append(
                _unestablished(
                    f"binaries[{idx}].path",
                    f"binary path must be a string, got {type(raw_path_val).__name__}",
                )
            )
            continue
        raw_path = raw_path_val
        bin_path = (
            pathlib.Path(raw_path)
            if pathlib.Path(raw_path).is_absolute()
            else (repo_root / raw_path)
        )
        if not raw_path or not bin_path.is_file():
            unestablished.append(
                _unestablished(
                    f"binaries[{idx}]",
                    f"candidate binary file missing: {raw_path!r}",
                )
            )
            continue
        computed_sha = _sha256_file(bin_path)
        declared_sha = entry.get("sha256")
        if declared_sha is not None and (
            not isinstance(declared_sha, str) or declared_sha.lower() != computed_sha
        ):
            unestablished.append(
                _unestablished(
                    f"binaries[{idx}].sha256",
                    f"binary SHA-256 mismatch for {raw_path}: declared {declared_sha}, actual {computed_sha}",
                )
            )
            continue
        bin_obj: dict[str, Any] = {
            "path": raw_path,
            "sha256": computed_sha,
            "version": str(entry.get("version") or "unknown"),
            "target": str(entry.get("target") or platform.machine()),
        }
        if isinstance(entry.get("features"), list):
            bin_obj["features"] = [str(f) for f in entry["features"]]
        archive_path, probe_path = entry.get("archivePath"), entry.get("probePath")
        if archive_path is not None or probe_path is not None:
            try:
                if not isinstance(archive_path, str) or not archive_path:
                    raise ValueError("archivePath is required for artifact bindings")
                archive = repo_root / archive_path
                if probe_path is None:
                    executable = "ee.exe" if bin_obj["target"].endswith("-windows-msvc") else "ee"
                    if _archive_executable_sha256(archive, executable) != computed_sha:
                        raise ValueError("archive executable differs from the candidate binary")
                    bin_obj["archiveSha256"] = _sha256_file(archive)
                else:
                    if not isinstance(probe_path, str) or not probe_path:
                        raise ValueError("probePath must name the native probe receipt")
                    bin_obj.update(native_release_artifact_bindings(
                        bin_obj, archive, repo_root / probe_path, commit_sha,
                    ))
            except (OSError, EOFError, ValueError, tarfile.TarError) as exc:
                unestablished.append(_unestablished(f"binaries[{idx}].archive", str(exc)))
        binaries.append(bin_obj)
    if not binaries:
        unestablished.append(
            _unestablished("binaries", "at least one verified candidate binary is required")
        )

    evidence_hashes: list[dict[str, str]] = []
    repo_resolved = repo_root.resolve()
    raw_evidence_paths = candidate_manifest.get("evidencePaths", [])
    if not isinstance(raw_evidence_paths, list):
        unestablished.append(
            _unestablished(
                "evidenceHashes",
                f"evidencePaths must be a list of strings, got {type(raw_evidence_paths).__name__}",
            )
        )
        raw_evidence_paths = []
    for raw_ev in raw_evidence_paths:
        if not isinstance(raw_ev, str):
            unestablished.append(
                _unestablished(
                    "evidenceHashes",
                    f"evidence path must be a string, got {type(raw_ev).__name__}",
                )
            )
            continue
        rel_str = raw_ev
        rel_posix = pathlib.PurePosixPath(rel_str)
        if (
            not rel_str
            or rel_str.startswith("/")
            or pathlib.Path(rel_str).is_absolute()
            or ".." in rel_posix.parts
            or "\\" in rel_str
        ):
            unestablished.append(
                _unestablished(
                    "evidenceHashes",
                    f"evidence path must stay repo-relative without traversal, got {rel_str!r}",
                )
            )
            continue
        ev_path = (repo_root / rel_str).resolve()
        try:
            ev_path.relative_to(repo_resolved)
        except ValueError:
            unestablished.append(
                _unestablished(
                    "evidenceHashes",
                    f"evidence path resolves outside repo root: {rel_str!r}",
                )
            )
            continue
        if not ev_path.is_file():
            unestablished.append(
                _unestablished(
                    "evidenceHashes",
                    f"evidence artifact missing on disk: {rel_str}",
                )
            )
            continue
        evidence_hashes.append({"path": rel_str, "sha256": _sha256_file(ev_path)})

    raw_approved_ignored = candidate_manifest.get("approvedIgnoredStages", [])
    if isinstance(raw_approved_ignored, list) and all(
        isinstance(s, str) for s in raw_approved_ignored
    ):
        approved_ignored = set(raw_approved_ignored)
    else:
        approved_ignored = set()
        unestablished.append(
            _unestablished(
                "manifest.approvedIgnoredStages",
                "approvedIgnoredStages must be a list of strings",
            )
        )
    raw_stages = candidate_manifest.get("stages", [])
    if not isinstance(raw_stages, list):
        unestablished.append(
            _unestablished(
                "results",
                f"stages must be a list, got {type(raw_stages).__name__}",
            )
        )
        raw_stages = []
    results: list[dict[str, Any]] = []
    shards: list[dict[str, Any]] = []
    logical_tests = 0
    seen_stages: set[str] = set()

    for idx, st in enumerate(raw_stages):
        if not isinstance(st, dict):
            unestablished.append(
                _unestablished(f"results[{idx}]", "stage entry must be an object")
            )
            continue
        stage_name = str(st.get("stage", "")).strip()
        if not stage_name:
            unestablished.append(
                _unestablished(f"results[{idx}].stage", "stage name is missing")
            )
            continue
        if stage_name in seen_stages:
            unestablished.append(
                _unestablished(
                    f"results[{idx}].stage",
                    f"duplicate stage name {stage_name!r} in candidate stages",
                )
            )
            continue
        seen_stages.add(stage_name)

        stage_commit = st.get("commit") or st.get("headSha")
        if stage_commit is not None and str(stage_commit).lower() != commit_sha:
            unestablished.append(
                _unestablished(
                    f"results[{stage_name}].commit",
                    f"stage {stage_name} ran at {stage_commit}, expected single candidate SHA {commit_sha}",
                )
            )

        status = str(st.get("status", "")).upper()
        exit_code = st.get("exitCode")
        no_verdict_cause = st.get("noVerdictCause")
        terminator_class = st.get("terminatorClass") or classify_terminator(
            int(exit_code) if _is_strict_int(exit_code) else 1,
            no_verdict_cause,
        )

        log_path_str = st.get("logPath")
        declared_log_sha = st.get("logSha256")
        computed_log_sha: str | None = None
        if log_path_str:
            lp = (
                pathlib.Path(str(log_path_str))
                if pathlib.Path(str(log_path_str)).is_absolute()
                else (repo_root / str(log_path_str))
            )
            if not lp.is_file():
                unestablished.append(
                    _unestablished(
                        f"results[{stage_name}].logPath",
                        f"stage {stage_name} log file missing on disk: {log_path_str}",
                    )
                )
            else:
                computed_log_sha = _sha256_file(lp)
                if declared_log_sha and str(declared_log_sha).lower() != computed_log_sha:
                    unestablished.append(
                        _unestablished(
                            f"results[{stage_name}].logSha256",
                            f"stage {stage_name} log SHA-256 mismatch: declared {declared_log_sha}, actual {computed_log_sha}",
                        )
                    )
        else:
            unestablished.append(
                _unestablished(
                    f"results[{stage_name}].logPath",
                    f"stage {stage_name} omitted logPath so log digest cannot be verified",
                )
            )

        announced = st.get("announced")
        executed = st.get("executed")
        failed = st.get("failed")
        ignored = st.get("ignored")

        if status != "PASS":
            unestablished.append(
                _unestablished(
                    f"results[{stage_name}].status",
                    f"stage {stage_name} status is {status!r}, expected 'PASS'",
                )
            )
        if not _is_strict_int(exit_code) or exit_code != 0:
            unestablished.append(
                _unestablished(
                    f"results[{stage_name}].exitCode",
                    f"stage {stage_name} exitCode is {exit_code!r}, expected 0",
                )
            )
        if terminator_class != "product_verdict":
            unestablished.append(
                _unestablished(
                    f"results[{stage_name}].terminatorClass",
                    f"stage {stage_name} terminatorClass is {terminator_class!r}, expected 'product_verdict'",
                )
            )
        if no_verdict_cause is not None:
            unestablished.append(
                _unestablished(
                    f"results[{stage_name}].noVerdictCause",
                    f"stage {stage_name} carries noVerdictCause={no_verdict_cause!r}",
                )
            )
        if failed is not None and (not _is_strict_int(failed) or failed != 0):
            unestablished.append(
                _unestablished(
                    f"results[{stage_name}].failed",
                    f"test stage {stage_name} reported failed={failed!r}",
                )
            )
        if (
            ignored is not None
            and (not _is_strict_int(ignored) or ignored != 0)
            and stage_name not in approved_ignored
        ):
            unestablished.append(
                _unestablished(
                    f"results[{stage_name}].ignored",
                    f"test stage {stage_name} reported unapproved ignored={ignored!r}",
                )
            )
        if announced is not None or executed is not None:
            if not _is_strict_int(announced) or announced <= 0:
                unestablished.append(
                    _unestablished(
                        f"results[{stage_name}].announced",
                        f"test stage {stage_name} has invalid/zero announced count: {announced!r}",
                    )
                )
            elif not _is_strict_int(executed) or executed != announced:
                unestablished.append(
                    _unestablished(
                        f"results[{stage_name}].executed",
                        f"test stage {stage_name} executed={executed!r} != announced={announced!r}",
                    )
                )
            else:
                logical_tests += announced
                shards.append({"name": stage_name, "tests": announced})

        row: dict[str, Any] = {
            "stage": stage_name,
            "status": status or "INFRA_ERROR",
            "exitCode": exit_code if _is_strict_int(exit_code) else None,
            "durationSeconds": st.get("durationSeconds"),
            "attempts": st.get("attempts"),
            "reason": st.get("reason"),
            "target": st.get("target"),
            "announced": announced,
            "executed": executed,
            "filteredOut": st.get("filteredOut"),
            "failed": failed,
            "ignored": ignored,
            "logPath": str(log_path_str) if log_path_str else None,
            "logSha256": computed_log_sha or declared_log_sha,
            "workerHost": st.get("workerHost") or candidate_manifest.get("host"),
            "noVerdictCause": no_verdict_cause,
            "terminatorClass": terminator_class,
        }
        if isinstance(stage_commit, str) and HEX40_RE.fullmatch(stage_commit):
            row["commit"] = stage_commit
        receipt_path = st.get("receiptPath")
        if receipt_path is not None:
            receipt = repo_root / receipt_path if isinstance(receipt_path, str) else None
            if not receipt or not receipt.is_file():
                unestablished.append(_unestablished(
                    f"results[{stage_name}].receiptPath", "native receipt could not be read",
                ))
            else:
                row["receiptPath"] = receipt_path
                row["receiptSha256"] = _sha256_file(receipt)
        results.append(row)

    if not results:
        unestablished.append(
            _unestablished("results", "at least one executed stage result is required")
        )
    if logical_tests <= 0 or not shards or logical_tests != sum(s["tests"] for s in shards):
        unestablished.append(
            _unestablished(
                "testInventory.logicalTests",
                "at least one executed test stage with announced == executed > 0 is required",
            )
        )
    for req_stage in required_stages:
        if req_stage not in seen_stages:
            unestablished.append(
                _unestablished(
                    f"results[{req_stage}]",
                    f"mandatory stage {req_stage!r} is missing from candidate results",
                )
            )

    raw_tc = candidate_manifest.get("toolchain")
    tc_in = raw_tc if isinstance(raw_tc, dict) else {}
    default_host = candidate_manifest.get("host")
    local_host = platform.node().split(".")[0]
    evaluated_cfg = tc_in.get("evaluatedUnderCfg")
    if evaluated_cfg is None and (default_host is None or default_host == local_host):
        evaluated_cfg = host_cfg_label()
    toolchain: dict[str, Any] = {
        "rustc": str(tc_in.get("rustc") or _tool_version("rustc") or "unknown"),
        "cargo": str(tc_in.get("cargo") or _tool_version("cargo") or "unknown"),
        "channel": str(tc_in.get("channel") or "nightly"),
        "target": str(tc_in.get("target") or platform.machine()),
        "evaluatedUnderCfg": evaluated_cfg,
    }
    if isinstance(tc_in.get("features"), list):
        toolchain["features"] = [str(f) for f in tc_in["features"]]

    sibling_pins: list[dict[str, Any]] = []
    raw_sibling_pins = candidate_manifest.get("siblingPins", [])
    if isinstance(raw_sibling_pins, list):
        for pin in raw_sibling_pins:
            if isinstance(pin, dict) and "crate" in pin and "resolved" in pin:
                sibling_pins.append(
                    {
                        "crate": str(pin["crate"]),
                        "resolved": str(pin["resolved"]),
                        "path": pin.get("path"),
                    }
                )

    raw_run_ids = candidate_manifest.get("runIdentifiers")
    run_ids_in = raw_run_ids if isinstance(raw_run_ids, dict) else {}
    run_identifiers = {
        "rch": run_ids_in.get("rch"),
        "hostedCi": None,
    }
    dsr_path = candidate_manifest.get("dsrManifestPath")
    if dsr_path is not None:
        path = repo_root / dsr_path if isinstance(dsr_path, str) else None
        if not path or not path.is_file():
            unestablished.append(_unestablished("runIdentifiers.dsrManifestSha256", "DSR manifest could not be read"))
        else:
            run_identifiers["dsrManifestSha256"] = _sha256_file(path)
    raw_perf_samples = candidate_manifest.get("performanceSamples", [])
    perf_samples = (
        [sample for sample in raw_perf_samples if isinstance(sample, dict)]
        if isinstance(raw_perf_samples, list)
        else []
    )

    capsule: dict[str, Any] = {
        "schema": "ee.release_candidate_proof.v1",
        "capsuleHash": ZERO_SHA40 + "0" * 24,
        "generatedAt": datetime.datetime.now(datetime.timezone.utc).isoformat(),
        "completeness": {
            "populated": False,
            "unestablished": unestablished,
        },
        "source": {
            "treeish": str(candidate_manifest.get("treeish") or commit_sha),
            "commit": commit_sha,
            "dirty": dirty,
        },
        "effectiveDependencies": {
            "lockfileHash": lock_hash,
            "cargoLockSha256": cargo_lock_hash,
            "frankenStackLockSha256": franken_lock_hash,
            "siblingPins": sibling_pins,
        },
        "toolchain": toolchain,
        "manifest": {
            "stageCount": len(results),
            "manifestHash": manifest_hash,
            "manifestPath": manifest_rel,
        },
        "binaries": binaries,
        "testInventory": {
            "logicalTests": logical_tests,
            "shards": shards,
        },
        "results": results,
        "performance": {"samples": perf_samples},
        "runIdentifiers": run_identifiers,
        "evidenceHashes": evidence_hashes,
    }

    schema_problems = validate(capsule)
    for prob in schema_problems:
        unestablished.append(_unestablished("schema", prob))

    capsule["completeness"]["populated"] = len(unestablished) == 0
    capsule["capsuleHash"] = compute_capsule_hash(capsule)
    return capsule


def _matches_json_type(value: Any, expected: str) -> bool:
    if expected == "null":
        return value is None
    if expected == "boolean":
        return isinstance(value, bool)
    if expected == "integer":
        return isinstance(value, int) and not isinstance(value, bool)
    if expected == "number":
        return isinstance(value, (int, float)) and not isinstance(value, bool)
    if expected == "string":
        return isinstance(value, str)
    if expected == "array":
        return isinstance(value, list)
    if expected == "object":
        return isinstance(value, dict)
    return True


def _validate_node(
    value: Any, spec: dict[str, Any], path: str, problems: list[str]
) -> None:
    if "const" in spec and value != spec["const"]:
        problems.append(f"{path}: expected const {spec['const']!r}, got {value!r}")
        return

    if "type" in spec:
        allowed_types = (
            spec["type"] if isinstance(spec["type"], list) else [spec["type"]]
        )
        if not any(_matches_json_type(value, t) for t in allowed_types):
            problems.append(
                f"{path}: expected type {allowed_types}, got {type(value).__name__}"
            )
            return

    if "enum" in spec and value not in spec["enum"]:
        problems.append(f"{path}: value {value!r} not in enum {spec['enum']!r}")

    if isinstance(value, str):
        if "pattern" in spec and not re.fullmatch(spec["pattern"], value):
            problems.append(
                f"{path}: value {value!r} does not match pattern {spec['pattern']!r}"
            )
        if spec.get("format") == "date-time":
            if not RFC3339_RE.fullmatch(value):
                problems.append(f"{path}: invalid RFC3339 date-time {value!r}")
            else:
                try:
                    dt = datetime.datetime.fromisoformat(value.replace("Z", "+00:00"))
                    if dt.tzinfo is None:
                        problems.append(f"{path}: RFC3339 date-time missing timezone {value!r}")
                except ValueError:
                    problems.append(f"{path}: invalid RFC3339/ISO date-time {value!r}")

    if isinstance(value, (int, float)) and not isinstance(value, bool):
        if isinstance(value, float) and (math.isnan(value) or math.isinf(value)):
            problems.append(f"{path}: non-finite number {value!r} not permitted in JSON")
        elif "minimum" in spec and value < spec["minimum"]:
            problems.append(f"{path}: value {value} < minimum {spec['minimum']}")

    if isinstance(value, dict):
        for req in spec.get("required", []):
            if req not in value:
                problems.append(f"{path}: missing required field {req!r}")
        props = spec.get("properties") or {}
        if spec.get("additionalProperties") is False:
            for key in value:
                if key not in props:
                    problems.append(
                        f"{path}.{key}: field not permitted by additionalProperties:false"
                    )
        for key, child_spec in props.items():
            if key in value:
                _validate_node(value[key], child_spec, f"{path}.{key}", problems)

    if isinstance(value, list) and "items" in spec:
        item_spec = spec["items"]
        for idx, item in enumerate(value):
            _validate_node(item, item_spec, f"{path}[{idx}]", problems)


def validate(capsule: dict[str, Any]) -> list[str]:
    """Full recursive validation against the committed JSON Schema."""
    if not isinstance(capsule, dict):
        return ["capsule root must be a JSON object"]
    schema = json.loads(SCHEMA.read_text(encoding="utf-8"))
    problems: list[str] = []
    _validate_node(capsule, schema, "$", problems)
    return problems


def verify_capsule(
    capsule: dict[str, Any],
    expected_sha: str,
    expected_binary: pathlib.Path | None = None,
    verify_logs: bool = False,
    repo_root: pathlib.Path = REPO,
    approved_ignored_stages: set[str] | None = None,
) -> list[str]:
    """Fail-closed cryptographic + completeness verification of a proof capsule."""
    problems = validate(capsule)
    if problems:
        return problems

    expected_sha_norm = expected_sha.strip().lower()
    if not HEX40_RE.fullmatch(expected_sha_norm):
        return [f"expected_sha {expected_sha!r} is not a 40-hex commit SHA"]

    actual_hash = str(capsule.get("capsuleHash", ""))
    recomputed_hash = compute_capsule_hash(capsule)
    if not hmac.compare_digest(actual_hash, recomputed_hash):
        problems.append(
            f"capsuleHash mismatch: recorded {actual_hash}, recomputed {recomputed_hash}"
        )

    completeness = capsule["completeness"]
    if completeness.get("populated") is not True:
        problems.append("completeness.populated is not true (unpopulated skeleton refused)")
    if completeness.get("unestablished"):
        problems.append(
            f"completeness.unestablished is non-empty ({len(completeness['unestablished'])} item(s))"
        )

    source = capsule["source"]
    if source.get("dirty") is not False:
        problems.append("source.dirty must be false")
    commit = str(source.get("commit", "")).lower()
    if commit == ZERO_SHA40:
        problems.append("source.commit cannot be the zero placeholder SHA")
    elif commit != expected_sha_norm:
        problems.append(
            f"source.commit {commit} does not match expected SHA {expected_sha_norm}"
        )

    if not capsule["effectiveDependencies"].get("lockfileHash"):
        problems.append("effectiveDependencies.lockfileHash must be non-null when populated")
    if not capsule["manifest"].get("manifestHash"):
        problems.append("manifest.manifestHash must be non-null when populated")

    test_inv = capsule["testInventory"]
    logical_tests = test_inv.get("logicalTests")
    shards = test_inv.get("shards") or []
    if not _is_strict_int(logical_tests) or logical_tests <= 0:
        problems.append(f"testInventory.logicalTests must be > 0 when populated, got {logical_tests!r}")
    if not shards:
        problems.append("testInventory.shards must be non-empty when populated")
    shard_map: dict[str, int] = {}
    for s_idx, shard in enumerate(shards):
        s_name = shard.get("name", "")
        s_tests = shard.get("tests")
        if not isinstance(s_name, str) or not s_name.strip():
            problems.append(f"testInventory.shards[{s_idx}].name must be non-empty")
        elif s_name in shard_map:
            problems.append(f"testInventory.shards[{s_name!r}] is duplicated")
        elif not _is_strict_int(s_tests) or s_tests <= 0:
            problems.append(f"testInventory.shards[{s_name!r}].tests must be > 0, got {s_tests!r}")
        else:
            shard_map[s_name] = s_tests
    if _is_strict_int(logical_tests) and shards and sum(shard_map.values()) != logical_tests:
        problems.append(
            f"testInventory.logicalTests ({logical_tests}) != sum(shards[].tests) ({sum(shard_map.values())})"
        )

    approved_ignored = approved_ignored_stages or set()
    results = capsule["results"]
    if not results:
        problems.append("results[] must be non-empty")
    seen_stages: set[str] = set()
    test_stage_counts: dict[str, int] = {}
    for idx, row in enumerate(results):
        raw_stage = row.get("stage", "")
        stage = raw_stage if isinstance(raw_stage, str) and raw_stage.strip() else f"#{idx}"
        if not isinstance(raw_stage, str) or not raw_stage.strip():
            problems.append(f"results[{idx}].stage must be a non-empty string")
        elif raw_stage in seen_stages:
            problems.append(f"results[{raw_stage}] is duplicated in results[]")
        else:
            seen_stages.add(raw_stage)
        if row.get("status") != "PASS":
            problems.append(f"results[{stage}].status is {row.get('status')!r}, want 'PASS'")
        exit_code = row.get("exitCode")
        if not _is_strict_int(exit_code) or exit_code != 0:
            problems.append(f"results[{stage}].exitCode is {exit_code!r}, want 0")
        if row.get("terminatorClass") != "product_verdict":
            problems.append(
                f"results[{stage}].terminatorClass is {row.get('terminatorClass')!r}, want 'product_verdict'"
            )
        if row.get("noVerdictCause") is not None:
            problems.append(
                f"results[{stage}].noVerdictCause must be null, got {row.get('noVerdictCause')!r}"
            )
        failed = row.get("failed")
        if failed is not None and (not _is_strict_int(failed) or failed != 0):
            problems.append(f"results[{stage}].failed is {failed!r}, want 0")
        ignored = row.get("ignored")
        if (
            ignored is not None
            and (not _is_strict_int(ignored) or ignored != 0)
            and stage not in approved_ignored
        ):
            problems.append(
                f"results[{stage}].ignored is {ignored!r} without approval"
            )
        announced = row.get("announced")
        executed = row.get("executed")
        if announced is not None or executed is not None:
            if not _is_strict_int(announced) or announced <= 0:
                problems.append(f"results[{stage}].announced must be > 0, got {announced!r}")
            elif not _is_strict_int(executed) or executed != announced:
                problems.append(
                    f"results[{stage}].executed ({executed!r}) != announced ({announced!r})"
                )
            elif isinstance(raw_stage, str) and raw_stage.strip():
                test_stage_counts[raw_stage] = announced
        log_sha = row.get("logSha256")
        log_path = row.get("logPath")
        if not log_sha:
            problems.append(f"results[{stage}].logSha256 must be non-null when populated")
        if log_path is not None:
            lp = (
                pathlib.Path(str(log_path))
                if pathlib.Path(str(log_path)).is_absolute()
                else (repo_root / str(log_path))
            )
            if lp.is_file():
                actual_log_sha = _sha256_file(lp)
                if not log_sha or not hmac.compare_digest(str(log_sha), actual_log_sha):
                    problems.append(
                        f"results[{stage}].logSha256 mismatch for {log_path}: recorded {log_sha!r}, actual {actual_log_sha}"
                    )
            elif verify_logs:
                problems.append(f"results[{stage}].logPath missing on disk: {log_path}")
        elif verify_logs:
            problems.append(f"results[{stage}].logPath must be non-null under --verify-logs")

    if shard_map != test_stage_counts:
        problems.append(
            f"testInventory.shards ({shard_map}) does not match executed test stages ({test_stage_counts})"
        )

    for ev_idx, ev in enumerate(capsule.get("evidenceHashes", [])):
        ev_rel = ev.get("path", "")
        if (
            not isinstance(ev_rel, str)
            or not ev_rel
            or ev_rel.startswith("/")
            or pathlib.Path(ev_rel).is_absolute()
            or ".." in pathlib.PurePosixPath(ev_rel).parts
            or "\\" in ev_rel
        ):
            problems.append(f"evidenceHashes[{ev_idx}].path {ev_rel!r} is invalid or uses traversal")

    binaries = capsule["binaries"]
    if not binaries:
        problems.append("binaries[] must be non-empty")
    if expected_binary is not None:
        if not expected_binary.is_file():
            problems.append(f"expected binary does not exist: {expected_binary}")
        else:
            want_sha = _sha256_file(expected_binary)
            if not any(
                hmac.compare_digest(str(b.get("sha256", "")), want_sha) for b in binaries
            ):
                problems.append(
                    f"expected binary {expected_binary} SHA-256 {want_sha} not present in capsule binaries[]"
                )

    return problems


def verify_release_artifact(
    capsule: dict[str, Any], archive: pathlib.Path, probe: pathlib.Path, expected_version: str
) -> list[str]:
    """Additional checks for the exact bytes addressed by a staging directory."""
    matching = [
        binary for binary in capsule["binaries"]
        if archive.name == f"ee-{binary['target']}.tar.xz"
    ]
    if len(matching) != 1:
        return ["staged archive must have exactly one matching target in the proof capsule"]
    binary = matching[0]
    if binary["version"] != expected_version:
        return ["staged binary version differs from the expected release version"]
    try:
        bindings = native_release_artifact_bindings(binary, archive, probe, capsule["source"]["commit"])
    except (OSError, EOFError, ValueError, tarfile.TarError) as exc:
        return [str(exc)]
    return [
        f"candidate capsule {field} does not bind the staged evidence"
        for field, digest in bindings.items()
        if binary.get(field) != digest
    ]


def verify_retained_test_counts(
    capsule: dict[str, Any], repo_root: pathlib.Path,
) -> list[str]:
    from grade_test_log import RUNNING_TARGET, parse, strip_ansi

    problems = []
    commit = capsule["source"]["commit"]
    for row in capsule["results"]:
        stage = row["stage"]
        if row.get("commit") != commit:
            problems.append(f"stage {stage} did not name the full candidate commit")
        if row.get("announced") is None:
            continue
        log_path = repo_root / row["logPath"]
        try:
            lines = log_path.read_text(encoding="utf-8", errors="replace").splitlines()
        except OSError as exc:
            problems.append(f"stage {stage} log could not be read: {exc.strerror}")
            continue
        pairs, parse_errors, targets = parse(lines)
        if parse_errors or len(pairs) != 1 or len(targets) != 1:
            problems.append(f"stage {stage} needs one complete, attributable libtest result")
            continue
        running = [match for line in lines if (match := RUNNING_TARGET.match(strip_ansi(line)))]
        target = row.get("target")
        if len(running) != 1 or target not in (running[0]["what"], running[0]["bin"]):
            problems.append(f"stage {stage} did not execute its declared test target")
        actual = pairs[0]
        if (
            actual["verdict"] != "ok"
            or actual["passed"] != row.get("announced")
            or actual["passed"] != row.get("executed")
            or actual["announced"] != actual["passed"]
            or actual["failed"] != 0
            or actual["ignored"] != 0
            or row.get("filteredOut") not in (None, actual["filtered"])
        ):
            problems.append(f"stage {stage} reported counts differ from its retained libtest result")
    return problems


def required_rch_commands(
    policy: dict[str, Any], cargo_manifest: dict[str, Any],
) -> dict[str, tuple[list[str], str | None, bool]]:
    if policy.get("schema") != "ee.release_rch_policy.v1":
        raise ValueError("candidate is missing its supported RCH qualification policy")
    commands = {
        "all-targets-check": (["cargo", "check", "--locked", "--all-targets"], None, False),
        "clippy": (["cargo", "clippy", "--locked", "--all-targets", "--", "-D", "warnings"], None, False),
        "lib": (["cargo", "test", "--locked", "--lib"], "src/lib.rs", False),
        "mcp-lib": (["cargo", "test", "--locked", "--features", "mcp", "--lib", "mcp::"], "src/lib.rs", True),
    }
    target_paths = {test["name"]: test.get("path") for test in cargo_manifest.get("test", [])}
    for profile, flags in (("targets", []), ("mcp_targets", ["--features", "mcp"])):
        targets = policy.get(profile)
        if not isinstance(targets, list) or not targets:
            raise ValueError(f"candidate RCH policy has no {profile}")
        for target in targets:
            if not isinstance(target, str) or target in commands or not target_paths.get(target):
                raise ValueError(f"candidate RCH policy contains an unknown or duplicate test target: {target!r}")
            command = ["cargo", "test", "--locked", *flags, "--test", target]
            commands[target] = (command, target_paths[target], False)
    return commands


def verify_rch_stage(
    row: dict[str, Any], command: list[str], test_target: str | None, selective: bool,
    source: dict[str, str], repo_root: pathlib.Path,
) -> None:
    from grade_test_log import RUNNING_TARGET, parse, strip_ansi

    def checked_bytes(path: str, digest: str) -> bytes:
        content = (repo_root / path).read_bytes()
        if _sha256_bytes(content) != digest:
            raise ValueError("retained evidence SHA-256 mismatch")
        return content

    receipt = json.loads(
        checked_bytes(row["receiptPath"], row["receiptSha256"]),
        parse_constant=_reject_json_constant, object_pairs_hook=_unique_json_object,
    )
    if row.get("commit") != source["commit"] or receipt.get("resolved_commit") != source["commit"]:
        raise ValueError("RCH receipt was not bound to the candidate commit")
    if receipt.get("git_tree") != source["tree"] or receipt.get("verification_attribution") != "pinned_franken_stack":
        raise ValueError("RCH receipt did not use the candidate's pinned source tree")
    if receipt.get("command") != command:
        raise ValueError("RCH command differs from committed qualification policy")
    if (
        receipt.get("schema") != "ee.rch.verify.v1"
        or receipt.get("success") is not True
        or not _is_strict_int(receipt.get("exit_code"))
        or receipt["exit_code"] != 0
        or receipt.get("verdict") != "passed"
        or receipt.get("timed_out") is not False
        or receipt.get("remote_required") is not True
        or receipt.get("remote_source_materialized") is not True
        or receipt.get("would_offload") is not True
        or not receipt.get("worker_id")
    ):
        raise ValueError("RCH receipt lacks a completed remote pass")
    stack = receipt["franken_stack"]
    if (
        stack.get("status") != "pinned"
        or stack.get("remote_source_verified") is not True
        or stack.get("cargo_lock_unchanged") is not True
        or stack.get("cargo_lock_hash") != f"sha256:{source['cargo_lock']}"
        or stack.get("lock_hash") != f"sha256:{source['franken_lock']}"
    ):
        raise ValueError("RCH receipt did not verify the candidate lockfiles")
    artifacts = receipt["artifacts"]
    attempt = "retry" if any(artifact["attempt"] == "retry" for artifact in artifacts) else "primary"
    outputs = {}
    for artifact in artifacts:
        if artifact["attempt"] != attempt:
            continue
        kind = artifact["kind"]
        if kind not in ("stdout", "stderr") or kind in outputs:
            raise ValueError("RCH receipt contains ambiguous terminal output")
        output = checked_bytes(artifact["path"], artifact["sha256"])
        if not _is_strict_int(artifact["bytes"]) or len(output) != artifact["bytes"]:
            raise ValueError("RCH terminal output size differs from its receipt")
        outputs[kind] = output
    if outputs.keys() != {"stdout", "stderr"} or not any(outputs.values()):
        raise ValueError("RCH receipt did not retain both terminal streams")

    if test_target is None:
        if row.get("announced") is not None or row.get("executed") is not None:
            raise ValueError("non-test RCH stage declared test counts")
        return

    lines = b"\n".join((outputs["stderr"], outputs["stdout"])).decode("utf-8", "replace").splitlines()
    pairs, problems, targets = parse(lines)
    running = [match["what"] for line in lines if (match := RUNNING_TARGET.match(strip_ansi(line)))]
    if problems or len(pairs) != 1 or len(targets) != 1 or running != [test_target]:
        raise ValueError("RCH receipt lacks one complete result for the required libtest target")
    actual = pairs[0]
    if (
        actual["announced"] <= 0
        or actual["verdict"] != "ok"
        or actual["failed"] != 0
        or actual["ignored"] != 0
        or actual["passed"] != actual["announced"]
        or actual["passed"] != row.get("executed")
        or actual["announced"] != row.get("announced")
        or actual["filtered"] != row.get("filteredOut")
        or (actual["filtered"] > 0) != selective
    ):
        raise ValueError("RCH test coverage differs from the declared counts or selection policy")


def verify_release_rch_policy(capsule: dict[str, Any], repo_root: pathlib.Path) -> list[str]:
    try:
        import tomllib
    except ModuleNotFoundError:
        return ["RCH policy verification requires Python 3.11 or later"]

    commit = capsule["source"]["commit"]
    try:
        tree = subprocess.run(
            ["git", "rev-parse", "--verify", f"{commit}^{{tree}}"],
            cwd=repo_root, capture_output=True, text=True, check=True, timeout=10,
        ).stdout.strip()
        source = {
            "commit": commit, "tree": tree,
            "cargo_lock": _sha256_bytes(_committed_blob(repo_root, commit, "Cargo.lock")),
            "franken_lock": _sha256_bytes(_committed_blob(repo_root, commit, "franken-stack.lock")),
        }
        policy = tomllib.loads(_committed_blob(repo_root, commit, "scripts/verify-budget.toml").decode())
        cargo_manifest = tomllib.loads(_committed_blob(repo_root, commit, "Cargo.toml").decode())
        required = required_rch_commands(policy.get("release_rch", {}), cargo_manifest)
    except (AttributeError, OSError, ValueError, KeyError, TypeError, subprocess.SubprocessError) as exc:
        return [f"could not load release RCH policy: {exc}"]

    results = {row["stage"]: row for row in capsule["results"]}
    problems = []
    for stage, (command, target, selective) in required.items():
        if stage not in results:
            problems.append(f"RCH qualification requires missing stage {stage}")
            continue
        try:
            verify_rch_stage(results[stage], command, target, selective, source, repo_root)
        except (AttributeError, OSError, KeyError, TypeError, ValueError) as exc:
            problems.append(f"RCH stage {stage} refused: {exc}")
    return problems


def verify_dsr_artifacts(
    capsule: dict[str, Any], manifest: dict[str, Any], policy: dict[str, Any],
    staging_dir: pathlib.Path, version: str,
) -> list[str]:
    if policy.get("schema") != "ee.release_artifact_policy.v1":
        return ["candidate does not declare the required release target policy"]
    native = policy["native"]
    compile_only = policy["compile_only"]
    all_targets = set(native) | set(compile_only)
    if not native or len(all_targets) != len(native) + len(compile_only) or not all(compile_only.values()):
        return ["candidate has an ambiguous or incomplete release target policy"]
    if (
        manifest.get("schema_version") != "1.0.0"
        or manifest.get("tool") != "ee"
        or manifest.get("version") != f"v{version}"
        or manifest.get("source", {}).get("git_sha") != capsule["source"]["commit"]
        or manifest.get("status") != "success"
        or manifest.get("publishable") is not True
        or manifest.get("build_purpose") != "release"
        or capsule["runIdentifiers"].get("hostedCi") is not None
    ):
        return ["DSR manifest did not report a completed release build for the candidate source and version"]

    expected_archives = {f"ee-{target}.tar.xz": target for target in all_targets}
    staged = {
        path.name for path in staging_dir.glob("ee-*")
        if path.is_file() and path.name.endswith((".tar.xz", ".tar.gz", ".zip"))
    }
    if staged != expected_archives.keys():
        missing = sorted(expected_archives.keys() - staged)
        unexpected = sorted(staged - expected_archives.keys())
        return [f"staged archive coverage differs from candidate policy (missing={missing}, extra={unexpected})"]
    binaries = {binary["target"]: binary for binary in capsule["binaries"]}
    if set(binaries) != all_targets or len(binaries) != len(capsule["binaries"]):
        return ["capsule must bind exactly one binary for every committed release target"]

    manifest_by_name = {}
    for record in manifest["artifacts"]:
        name = record["name"]
        if name in expected_archives:
            if name in manifest_by_name:
                return [f"DSR manifest has duplicate records for {name}"]
            manifest_by_name[name] = record

    problems = []
    for name, target in sorted(expected_archives.items()):
        archive = staging_dir / name
        record = manifest_by_name.get(name, {})
        binary = binaries[target]
        reported_target = record.get("target_triple")
        # Native receipts independently report the triple. The Windows
        # compile-only disposition needs that observation from DSR itself.
        target_matches = reported_target == target or (reported_target is None and target in native)
        if (
            archive.is_symlink()
            or binary["version"] != version
            or not target_matches
            or record.get("archive_format") != "tar.xz"
            or not _is_strict_int(record.get("size_bytes"))
            or record["size_bytes"] != archive.stat().st_size
            or record.get("sha256") != binary.get("archiveSha256")
        ):
            problems.append(f"DSR manifest lacks exact candidate archive binding for {target}")
            continue
        if target in native:
            probe = staging_dir / f"release-probe-{target}.json"
            problems.extend(f"{target}: {problem}" for problem in verify_release_artifact(capsule, archive, probe, version))
            continue
        try:
            if _sha256_file(archive) != binary["archiveSha256"]:
                raise ValueError("compiled archive SHA-256 differs from DSR and capsule")
            if _archive_executable_sha256(archive, "ee.exe") != binary["sha256"]:
                raise ValueError("compiled executable SHA-256 differs from capsule")
        except (OSError, EOFError, ValueError, tarfile.TarError) as exc:
            problems.append(f"{target}: {exc}")
    return problems


def verify_dsr_staging(
    capsule: dict[str, Any], manifest_path: pathlib.Path, staging_dir: pathlib.Path,
    version: str, repo_root: pathlib.Path,
) -> list[str]:
    import tomllib

    try:
        commit = capsule["source"]["commit"]
        budget = tomllib.loads(_committed_blob(repo_root, commit, "scripts/verify-budget.toml").decode())
        raw = manifest_path.read_bytes()
        if _sha256_bytes(raw) != capsule["runIdentifiers"].get("dsrManifestSha256"):
            return ["DSR build manifest bytes differ from the candidate capsule"]
        manifest = json.loads(raw, parse_constant=_reject_json_constant, object_pairs_hook=_unique_json_object)
        return verify_dsr_artifacts(capsule, manifest, budget.get("release_artifacts", {}), staging_dir, version)
    except (AttributeError, OSError, KeyError, TypeError, ValueError, subprocess.SubprocessError) as exc:
        return [f"could not verify DSR staging: {exc}"]


GREEN_LOG = """     Running unittests src/lib.rs (target/debug/deps/ee-aaa)
running 4 tests
test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 10 filtered out
"""
ZERO_LOG = """     Running unittests src/lib.rs (target/debug/deps/ee-aaa)
running 0 tests
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 99 filtered out
"""
MULTI_TARGET_LOG = """     Running unittests src/lib.rs (target/debug/deps/ee-aaa)
     Running tests/contracts.rs (target/debug/deps/contracts-bbb)
running 1 test
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
running 1 test
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
"""


def self_test() -> int:
    import copy
    import tempfile

    failures = 0

    def check(name: str, condition: bool) -> None:
        nonlocal failures
        if condition:
            print(f"  [self-test] OK   {name}")
        else:
            print(f"  [self-test] FAIL {name}")
            failures += 1

    sha_a = "a" * 40
    sha_b = "b" * 40

    with tempfile.TemporaryDirectory() as directory:
        root = pathlib.Path(directory)
        green = root / "green.log"
        green.write_text(GREEN_LOG, encoding="utf-8")
        zero = root / "zero.log"
        zero.write_text(ZERO_LOG, encoding="utf-8")

        capsule = build_capsule(green, sha_a, 0, "worker-a", 0, "rch exec ...")
        check("skeleton green run validates recursively against schema", not validate(capsule))
        check("skeleton capsuleHash is 64 lowercase hex chars", bool(HEX64_RE.fullmatch(capsule["capsuleHash"])))
        check("skeleton populated is false", capsule["completeness"]["populated"] is False)
        check("skeleton unestablished contains schema objects", all(isinstance(u, dict) and "item" in u and "reason" in u for u in capsule["completeness"]["unestablished"]))
        check("host is recorded", capsule["results"][0]["workerHost"] == "worker-a")
        check("announced is recorded", capsule["results"][0]["announced"] == 4)
        check("remote run does not claim local cfg", capsule["toolchain"]["evaluatedUnderCfg"] is None)
        check(
            "unestablished names evaluatedUnderCfg",
            any(u["item"] == "toolchain.evaluatedUnderCfg" for u in capsule["completeness"]["unestablished"]),
        )
        local = build_capsule(green, sha_a, 0, platform.node().split(".")[0], 0, "x")
        check("local run records cfg", bool(local["toolchain"]["evaluatedUnderCfg"]))
        check("hostedCi is null", capsule["runIdentifiers"]["hostedCi"] is None)
        check("green run reports PASS", capsule["results"][0]["status"] == "PASS")

        single_caveat = [
            u for u in capsule["completeness"]["unestablished"] if u["item"] == "results[].stage/target"
        ]
        check("single-target log does not emit multi-target advisory", single_caveat == [])

        multi = root / "multi.log"
        multi.write_text(MULTI_TARGET_LOG, encoding="utf-8")
        multi_capsule = build_capsule(multi, sha_a, 0, "worker-a", 0, "rch exec ...")
        multi_caveat = [
            u for u in multi_capsule["completeness"]["unestablished"] if u["item"] == "results[].stage/target"
        ]
        check(
            "multi-target log names targets in advisory object",
            len(multi_caveat) == 1
            and "2 targets announced" in multi_caveat[0]["reason"]
            and "target/debug/deps/ee-aaa" in multi_caveat[0]["reason"]
            and "target/debug/deps/contracts-bbb" in multi_caveat[0]["reason"],
        )
        check("multi-target capsule validates against schema", not validate(multi_capsule))

        vacuous = build_capsule(zero, sha_a, 0, "worker-a", 0, "rch exec ...")
        check("zero-test run is not PASS", vacuous["results"][0]["status"] != "PASS")
        check("zero-test run carries noVerdictCause=NOT_RUN", vacuous["results"][0]["noVerdictCause"] == "NOT_RUN")
        check("green run terminatorClass is product_verdict", capsule["results"][0]["terminatorClass"] == "product_verdict")
        check("zero-test run terminatorClass is infra_refusal", vacuous["results"][0]["terminatorClass"] == "infra_refusal")
        timed_out = build_capsule(green, sha_a, 0, "worker-a", 124, "rch exec ...")
        check(
            "exit 124 classifies terminator as timeout",
            timed_out["results"][0]["terminatorClass"] == "timeout"
            and timed_out["results"][0]["status"] == "TIMEOUT",
        )
        killed = build_capsule(green, sha_a, 0, "worker-a", 137, "rch exec ...")
        check(
            "exit 137 classifies terminator as signal",
            killed["results"][0]["terminatorClass"] == "signal"
            and killed["results"][0]["status"] == "INFRA_ERROR",
        )

        # capsuleHash covers every field except capsuleHash & generatedAt.
        same_again = copy.deepcopy(capsule)
        same_again["generatedAt"] = "2026-01-01T00:00:00+00:00"
        check(
            "capsuleHash ignores generatedAt",
            compute_capsule_hash(same_again) == capsule["capsuleHash"],
        )
        moved = build_capsule(green, sha_b, 0, "worker-a", 0, "rch exec ...")
        check("capsuleHash changes when commit changes", capsule["capsuleHash"] != moved["capsuleHash"])
        dirty = build_capsule(green, sha_a, 3, "worker-a", 0, "rch exec ...")
        check(
            "dirty tree is named in unestablished",
            any(u["item"] == "source.treeish" for u in dirty["completeness"]["unestablished"]),
        )
        check("capsuleHash changes when dirty changes", capsule["capsuleHash"] != dirty["capsuleHash"])

        # Negative validation checks: short commit SHA and legacy string unestablished must fail.
        bad_sha = copy.deepcopy(capsule)
        bad_sha["source"]["commit"] = "abc123"
        check("validate() rejects non-40-hex source.commit", bool(validate(bad_sha)))

        # Candidate population + cryptographic verification (.76).
        cand_repo = root / "repo"
        (cand_repo / "scripts").mkdir(parents=True)
        (cand_repo / "Cargo.lock").write_text("cargo-lock-content\n", encoding="utf-8")
        (cand_repo / "franken-stack.lock").write_text("franken-lock-content\n", encoding="utf-8")
        (cand_repo / "scripts" / "verify-budget.toml").write_text("total_expected_seconds = 608\n", encoding="utf-8")
        bin_path = root / "ee-candidate"
        bin_path.write_bytes(b"fake-ee-binary-bytes")
        ev_rel = "scripts/verify-budget.toml"
        log_path = root / "stage-lib.log"
        log_path.write_text(GREEN_LOG, encoding="utf-8")

        manifest = {
            "base": sha_a,
            "treeish": "main",
            "dirty": 0,
            "host": "worker-01",
            "toolchain": {
                "rustc": "rustc 1.90.0-nightly",
                "cargo": "cargo 1.90.0-nightly",
                "channel": "nightly",
                "target": "x86_64-unknown-linux-gnu",
                "features": ["fts5", "json", "embed-fast", "lexical-bm25", "graph"],
                "evaluatedUnderCfg": "target_family=unix, target_os=linux",
            },
            "manifestPath": "scripts/verify-budget.toml",
            "requiredStages": ["lib"],
            "binaries": [
                {
                    "path": str(bin_path),
                    "version": "0.19.0",
                    "target": "x86_64-unknown-linux-gnu",
                    "features": ["fts5", "json"],
                }
            ],
            "siblingPins": [{"crate": "fsqlite", "resolved": "=0.4.10", "path": None}],
            "stages": [
                {
                    "stage": "lib",
                    "status": "PASS",
                    "exitCode": 0,
                    "terminatorClass": "product_verdict",
                    "target": "src/lib.rs",
                    "announced": 4,
                    "executed": 4,
                    "failed": 0,
                    "ignored": 0,
                    "workerHost": "worker-01",
                    "logPath": str(log_path),
                    "logSha256": _sha256_file(log_path),
                    "commit": sha_a,
                    "durationSeconds": 1.25,
                    "attempts": 1,
                }
            ],
            "evidencePaths": [ev_rel],
            "runIdentifiers": {"rch": "rch-run-1"},
        }
        populated = populate_candidate_capsule(manifest, repo_root=cand_repo)
        check("populated candidate validates recursively", not validate(populated))
        check("populated candidate sets populated=true", populated["completeness"]["populated"] is True)
        check(
            "verify_capsule passes valid populated candidate",
            not verify_capsule(populated, sha_a, expected_binary=bin_path, verify_logs=True, repo_root=cand_repo),
        )
        check("native verification parses retained libtest", not verify_retained_test_counts(populated, cand_repo))
        for field, value in (("commit", sha_b), ("announced", 12), ("target", "tests/contracts.rs")):
            forged = copy.deepcopy(populated)
            forged["results"][0][field] = value
            check(f"native verification rejects stage {field} mismatch",
                  bool(verify_retained_test_counts(forged, cand_repo)))
        log_path.write_text(ZERO_LOG, encoding="utf-8")
        check("native verification rejects zero-result log", bool(verify_retained_test_counts(populated, cand_repo)))
        log_path.write_text(GREEN_LOG, encoding="utf-8")

        policy = {"schema": "ee.release_rch_policy.v1", "targets": ["contracts"], "mcp_targets": ["mcp_parity"]}
        config = {"test": [{"name": target, "path": f"tests/{target}.rs"}
                           for target in ("contracts", "mcp_parity")]}
        commands = required_rch_commands(policy, config)
        check("RCH inventory requires policy integration and feature stages", set(commands) == {
            "all-targets-check", "clippy", "lib", "mcp-lib", "contracts", "mcp_parity",
        })
        command, test_target, selective = commands["lib"]
        split_log = GREEN_LOG.replace("10 filtered out", "0 filtered out").splitlines(keepends=True)
        streams = []
        for kind, content in (("stderr", split_log[0]), ("stdout", "".join(split_log[1:]))):
            path = root / f"stage-{kind}.log"
            data = content.encode()
            path.write_bytes(data)
            streams.append({"kind": kind, "path": str(path), "attempt": "primary",
                            "sha256": _sha256_bytes(data), "bytes": len(data)})
        source = {"commit": sha_a, "tree": sha_b, "cargo_lock": "c" * 64, "franken_lock": "d" * 64}
        receipt = {
            "schema": "ee.rch.verify.v1", "command": command, "resolved_commit": sha_a,
            "git_tree": sha_b, "verification_attribution": "pinned_franken_stack", "success": True,
            "exit_code": 0, "verdict": "passed", "timed_out": False, "remote_required": True,
            "would_offload": True, "remote_source_materialized": True, "worker_id": "worker-01",
            "artifacts": streams,
            "franken_stack": {
                "status": "pinned", "remote_source_verified": True, "cargo_lock_unchanged": True,
                "cargo_lock_hash": f"sha256:{source['cargo_lock']}", "lock_hash": f"sha256:{source['franken_lock']}",
            },
        }
        receipt_path = root / "rch-receipt.json"
        row = copy.deepcopy(populated["results"][0])
        row.update(receiptPath=str(receipt_path), filteredOut=0)

        def rch_problem(candidate_receipt):
            receipt_path.write_text(json.dumps(candidate_receipt), encoding="utf-8")
            row["receiptSha256"] = _sha256_file(receipt_path)
            try:
                verify_rch_stage(row, command, test_target, selective, source, cand_repo)
            except (KeyError, ValueError) as exc:
                return str(exc)
            return None

        check("retained RCH receipt reconciles source and test output", rch_problem(receipt) is None)
        bad_command = dict(receipt, command=[*command, "--no-run"])
        check("RCH policy rejects a compile-only test invocation", "command differs" in (rch_problem(bad_command) or ""))
        timed_out = dict(receipt, timed_out=True)
        check("RCH policy rejects timeout despite pass fields", "remote pass" in (rch_problem(timed_out) or ""))
        pathlib.Path(streams[1]["path"]).write_text("forged output\n", encoding="utf-8")
        check("RCH policy rejects replaced terminal output", "SHA-256 mismatch" in (rch_problem(receipt) or ""))

        # Native release receipts bind the archive member bytes and every
        # named check in the retained receipt.
        archive = root / "ee-x86_64-unknown-linux-gnu.tar.xz"
        probe_path = root / "release-probe-x86_64-unknown-linux-gnu.json"
        with tarfile.open(archive, mode="w:xz") as output:
            output.add(bin_path, arcname="ee")
        probe = {
            "schema": RELEASE_PROBE_SCHEMA,
            "verdict": "pass",
            "failed": [],
            "archive": {"sha256": _sha256_file(archive)},
            "binary": {
                "gitCommit": sha_a,
                "gitDirty": False,
                "version": "0.19.0",
                "targetTriple": "x86_64-unknown-linux-gnu",
                "sha256": _sha256_file(bin_path),
            },
            "checks": [
                {
                    "id": check_id, "kind": kind, "pass": True,
                    "observed": "embed_backend=neural_local" if kind == "precondition" else "ok",
                }
                for check_id, kind in RELEASE_PROBE_CHECKS.items()
            ],
        }
        probe_path.write_text(json.dumps(probe), encoding="utf-8")
        bound_manifest = copy.deepcopy(manifest)
        bound_manifest["binaries"][0].update(archivePath=str(archive), probePath=str(probe_path))
        bound = populate_candidate_capsule(bound_manifest, repo_root=cand_repo)
        check("native archive and receipt bind during population", not verify_capsule(bound, sha_a))
        check("native staging validates exact bytes", not verify_release_artifact(bound, archive, probe_path, "0.19.0"))
        check("staging refuses an unbound capsule", bool(verify_release_artifact(populated, archive, probe_path, "0.19.0")))
        check("staging refuses wrong operator version", bool(verify_release_artifact(bound, archive, probe_path, "0.19.1")))
        win_bin = root / "ee.exe"
        win_bin.write_bytes(b"compile-only control bytes")
        win_target = "x86_64-pc-windows-msvc"
        win_archive = root / f"ee-{win_target}.tar.xz"
        with tarfile.open(win_archive, mode="w:xz") as output:
            output.add(win_bin, arcname="ee.exe")
        bound_manifest["binaries"].append({
            "path": str(win_bin), "archivePath": str(win_archive), "target": win_target, "version": "0.19.0",
        })
        release = populate_candidate_capsule(bound_manifest, repo_root=cand_repo)
        artifact_policy = {
            "schema": "ee.release_artifact_policy.v1", "native": ["x86_64-unknown-linux-gnu"],
            "compile_only": {win_target: "native probe unavailable"},
        }
        dsr_manifest = {
            "schema_version": "1.0.0", "tool": "ee", "version": "v0.19.0",
            "source": {"git_sha": sha_a}, "status": "success", "publishable": True,
            "build_purpose": "release", "artifacts": [],
        }
        for path, target in ((archive, "x86_64-unknown-linux-gnu"), (win_archive, win_target)):
            dsr_manifest["artifacts"].append({
                "name": path.name, "target_triple": target, "archive_format": "tar.xz",
                "size_bytes": path.stat().st_size, "sha256": _sha256_file(path),
            })
        check("DSR staging verifies native and policy-exempt compiled archives",
              not verify_dsr_artifacts(release, dsr_manifest, artifact_policy, root, "0.19.0"))
        partial = dict(dsr_manifest, status="partial")
        check("DSR staging rejects a partial build",
              bool(verify_dsr_artifacts(release, partial, artifact_policy, root, "0.19.0")))
        check("DSR staging rejects omitted capsule target",
              bool(verify_dsr_artifacts(bound, dsr_manifest, artifact_policy, root, "0.19.0")))
        wrong_build = copy.deepcopy(dsr_manifest)
        wrong_build["artifacts"][1]["target_triple"] = "x86_64-unknown-linux-gnu"
        check("DSR staging checks compiled target attribution",
              bool(verify_dsr_artifacts(release, wrong_build, artifact_policy, root, "0.19.0")))
        for name, change in (
            ("failed check", lambda receipt: receipt["checks"][0].update({"pass": False})),
            ("unobserved backend", lambda receipt: receipt["checks"][0].update(observed="embed_backend=hash_fallback")),
            ("duplicate check", lambda receipt: receipt["checks"].append(receipt["checks"][0])),
            ("absent check", lambda receipt: receipt["checks"].pop()),
            ("null dirty bit", lambda receipt: receipt["binary"].update(gitDirty=None)),
            ("wrong executable digest", lambda receipt: receipt["binary"].update(sha256="0" * 64)),
        ):
            broken = copy.deepcopy(probe)
            change(broken)
            probe_path.write_text(json.dumps(broken), encoding="utf-8")
            check(f"native staging refuses {name}", bool(verify_release_artifact(bound, archive, probe_path, "0.19.0")))
        probe_path.write_text(json.dumps(probe, indent=2), encoding="utf-8")
        check("native staging detects changed probe bytes", verify_release_artifact(bound, archive, probe_path, "0.19.0") == [
            "candidate capsule probeSha256 does not bind the staged evidence"
        ])
        with tarfile.open(archive, mode="w:xz") as output:
            output.add(probe_path, arcname="ee")
        probe["archive"]["sha256"] = _sha256_file(archive)
        probe_path.write_text(json.dumps(probe), encoding="utf-8")
        check("staging rehashes the archive member", verify_release_artifact(bound, archive, probe_path, "0.19.0") == [
            "archived executable SHA-256 differs from probe or candidate binary"
        ])

        # Negative verification controls:
        check(
            "verify_capsule refuses unpopulated skeleton",
            bool(verify_capsule(capsule, sha_a, repo_root=cand_repo)),
        )
        check(
            "verify_capsule refuses wrong expected SHA",
            bool(verify_capsule(populated, sha_b, expected_binary=bin_path, repo_root=cand_repo)),
        )
        tampered = copy.deepcopy(populated)
        tampered["results"][0]["workerHost"] = "worker-99"
        check(
            "verify_capsule detects capsule tampering via capsuleHash",
            bool(verify_capsule(tampered, sha_a, expected_binary=bin_path, repo_root=cand_repo)),
        )
        other_bin = root / "other-ee"
        other_bin.write_bytes(b"different-binary")
        check(
            "verify_capsule rejects mismatched expected binary",
            bool(verify_capsule(populated, sha_a, expected_binary=other_bin, repo_root=cand_repo)),
        )
        dirty_manifest = copy.deepcopy(manifest)
        dirty_manifest["dirty"] = 1
        dirty_cand = populate_candidate_capsule(dirty_manifest, repo_root=cand_repo)
        check(
            "populate_candidate forces populated=false on dirty tree",
            dirty_cand["completeness"]["populated"] is False,
        )
        missing_stage_manifest = copy.deepcopy(manifest)
        missing_stage_manifest["requiredStages"] = ["lib", "contracts"]
        missing_stage_cand = populate_candidate_capsule(missing_stage_manifest, repo_root=cand_repo)
        check(
            "populate_candidate forces populated=false when mandatory stage missing",
            missing_stage_cand["completeness"]["populated"] is False,
        )

        # Fresh-Eyes Pass 1 (.76) hardened negative controls:
        zero_test_manifest = copy.deepcopy(manifest)
        zero_test_manifest["stages"][0].pop("announced", None)
        zero_test_manifest["stages"][0].pop("executed", None)
        zero_test_cand = populate_candidate_capsule(zero_test_manifest, repo_root=cand_repo)
        check(
            "populate_candidate forces populated=false when logicalTests == 0",
            zero_test_cand["completeness"]["populated"] is False,
        )

        dup_stage_manifest = copy.deepcopy(manifest)
        dup_stage_manifest["stages"].append(copy.deepcopy(manifest["stages"][0]))
        dup_stage_cand = populate_candidate_capsule(dup_stage_manifest, repo_root=cand_repo)
        check(
            "populate_candidate forces populated=false on duplicate stage names",
            dup_stage_cand["completeness"]["populated"] is False,
        )

        traversal_manifest = copy.deepcopy(manifest)
        traversal_manifest["evidencePaths"] = ["../escape.txt"]
        traversal_cand = populate_candidate_capsule(traversal_manifest, repo_root=cand_repo)
        check(
            "populate_candidate rejects .. traversal in evidencePaths",
            traversal_cand["completeness"]["populated"] is False,
        )

        bool_exit_manifest = copy.deepcopy(manifest)
        bool_exit_manifest["stages"][0]["exitCode"] = False
        bool_exit_cand = populate_candidate_capsule(bool_exit_manifest, repo_root=cand_repo)
        check(
            "populate_candidate rejects bool exitCode=False",
            bool_exit_cand["completeness"]["populated"] is False,
        )

        zero_tests_cap = copy.deepcopy(populated)
        zero_tests_cap["testInventory"] = {"logicalTests": 0, "shards": []}
        zero_tests_cap["results"][0]["announced"] = None
        zero_tests_cap["results"][0]["executed"] = None
        zero_tests_cap["capsuleHash"] = compute_capsule_hash(zero_tests_cap)
        check(
            "verify_capsule rejects logicalTests == 0",
            bool(verify_capsule(zero_tests_cap, sha_a, expected_binary=bin_path, repo_root=cand_repo)),
        )

        shard_sum_cap = copy.deepcopy(populated)
        shard_sum_cap["testInventory"]["logicalTests"] = 99
        shard_sum_cap["capsuleHash"] = compute_capsule_hash(shard_sum_cap)
        check(
            "verify_capsule rejects shard sum != logicalTests",
            bool(verify_capsule(shard_sum_cap, sha_a, expected_binary=bin_path, repo_root=cand_repo)),
        )

        dup_stage_cap = copy.deepcopy(populated)
        dup_stage_cap["results"].append(copy.deepcopy(populated["results"][0]))
        dup_stage_cap["capsuleHash"] = compute_capsule_hash(dup_stage_cap)
        check(
            "verify_capsule rejects duplicate stage names",
            bool(verify_capsule(dup_stage_cap, sha_a, expected_binary=bin_path, repo_root=cand_repo)),
        )

        null_log_cap = copy.deepcopy(populated)
        null_log_cap["results"][0]["logPath"] = None
        null_log_cap["results"][0]["logSha256"] = None
        null_log_cap["capsuleHash"] = compute_capsule_hash(null_log_cap)
        check(
            "verify_capsule rejects logPath=null under --verify-logs",
            bool(verify_capsule(null_log_cap, sha_a, expected_binary=bin_path, verify_logs=True, repo_root=cand_repo)),
        )

        ignored_cap = copy.deepcopy(populated)
        ignored_cap["results"][0]["ignored"] = 2
        ignored_cap["capsuleHash"] = compute_capsule_hash(ignored_cap)
        check(
            "verify_capsule rejects unapproved ignored > 0",
            bool(verify_capsule(ignored_cap, sha_a, expected_binary=bin_path, repo_root=cand_repo)),
        )
        check(
            "verify_capsule allows approved ignored stage when explicitly listed",
            not verify_capsule(
                ignored_cap,
                sha_a,
                expected_binary=bin_path,
                repo_root=cand_repo,
                approved_ignored_stages={"lib"},
            ),
        )

        bad_dt_cap = copy.deepcopy(populated)
        bad_dt_cap["generatedAt"] = "2026-11-21"
        check("validate() rejects date-only generatedAt", bool(validate(bad_dt_cap)))
        bad_dt_cap["generatedAt"] = "2026-11-21T00:00:00"
        check("validate() rejects naive generatedAt without timezone", bool(validate(bad_dt_cap)))

        nan_cap = copy.deepcopy(populated)
        nan_cap["performance"]["samples"] = [{"metric": "x", "value": float("nan"), "unit": "ms"}]
        check("validate() rejects NaN sample values", bool(validate(nan_cap)))

        malformed_manifest = root / "malformed.json"
        malformed_manifest.write_text("{bad json", encoding="utf-8")
        proc_bad = subprocess.run(
            [
                sys.executable,
                str(pathlib.Path(__file__).resolve()),
                "--populate-candidate",
                "--candidate-manifest",
                str(malformed_manifest),
            ],
            capture_output=True,
            text=True,
        )
        check("CLI --populate-candidate rejects malformed JSON cleanly", proc_bad.returncode != 0)
        list_manifest = root / "list.json"
        list_manifest.write_text("[1, 2, 3]", encoding="utf-8")
        proc_list = subprocess.run(
            [
                sys.executable,
                str(pathlib.Path(__file__).resolve()),
                "--populate-candidate",
                "--candidate-manifest",
                str(list_manifest),
            ],
            capture_output=True,
            text=True,
        )
        check("CLI --populate-candidate rejects non-object manifest cleanly", proc_list.returncode != 0)

        malformed_fields_cap = populate_candidate_capsule(
            {
                "base": sha_a,
                "dirty": "not-an-int",
                "stages": "not-a-list",
                "binaries": {"path": "x"},
                "evidencePaths": [123, "nonexistent.txt"],
                "requiredStages": "lib",
                "approvedIgnoredStages": 42,
            },
            repo_root=cand_repo,
        )
        check(
            "populate_candidate_capsule rejects malformed field types cleanly without traceback",
            malformed_fields_cap["completeness"]["populated"] is False
            and not validate(malformed_fields_cap)
            and len(malformed_fields_cap["completeness"]["unestablished"]) >= 5,
        )

    if failures:
        print(f"[emit-proof-capsule] SELF-TEST FAILED: {failures} arm(s)")
        return 1
    print("[emit-proof-capsule] self-test: all arms passed")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(add_help=True)
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument("--populate-candidate", action="store_true")
    parser.add_argument("--candidate-manifest")
    parser.add_argument("--verify-capsule")
    parser.add_argument("--expected-sha")
    parser.add_argument("--expected-binary")
    parser.add_argument("--release-archive", type=pathlib.Path)
    parser.add_argument("--release-probe", type=pathlib.Path)
    parser.add_argument("--expected-version")
    parser.add_argument("--dsr-manifest", type=pathlib.Path)
    parser.add_argument("--staging-dir", type=pathlib.Path)
    parser.add_argument("--verify-logs", action="store_true")
    parser.add_argument("--verify-rch-policy", action="store_true")
    parser.add_argument("--repo-root")
    parser.add_argument("--log")
    parser.add_argument("--base", default="unknown")
    parser.add_argument("--dirty", type=int, default=0)
    parser.add_argument("--host")
    parser.add_argument("--run-exit", type=int, default=0)
    parser.add_argument("--command", default="")
    parser.add_argument("--out")
    args = parser.parse_args()

    repo_root = pathlib.Path(args.repo_root).resolve() if args.repo_root else REPO
    artifact_args = (args.release_archive, args.release_probe, args.expected_version)
    dsr_args = (args.dsr_manifest, args.staging_dir, args.expected_version)
    artifact_check = args.release_archive is not None or args.release_probe is not None
    dsr_check = args.dsr_manifest is not None or args.staging_dir is not None
    if artifact_check and (not args.verify_capsule or not all(artifact_args) or dsr_check):
        parser.error("--release-archive, --release-probe and --expected-version require one another and --verify-capsule")
    if dsr_check and (not args.verify_capsule or not all(dsr_args)):
        parser.error("--dsr-manifest, --staging-dir and --expected-version require one another and --verify-capsule")
    if args.expected_version and not (artifact_check or dsr_check):
        parser.error("--expected-version requires archive or DSR staging verification")
    if args.verify_rch_policy and not args.verify_capsule:
        parser.error("--verify-rch-policy requires --verify-capsule")

    if args.self_test:
        return self_test()

    if args.verify_capsule:
        if not args.expected_sha:
            print("[emit-proof-capsule] --verify-capsule requires --expected-sha", file=sys.stderr)
            print("EE_CAPSULE_RC=1")
            return 1
        cap_path = pathlib.Path(args.verify_capsule)
        if not cap_path.is_file():
            print(f"[emit-proof-capsule] capsule not found: {cap_path}", file=sys.stderr)
            print("EE_CAPSULE_RC=1")
            return 1
        try:
            capsule = json.loads(
                cap_path.read_text(encoding="utf-8"),
                parse_constant=_reject_json_constant,
                object_pairs_hook=_unique_json_object,
            )
        except Exception as exc:
            print(f"[emit-proof-capsule] invalid capsule JSON: {exc}", file=sys.stderr)
            print("EE_CAPSULE_RC=1")
            return 1
        if not isinstance(capsule, dict):
            print("[emit-proof-capsule] capsule root must be a JSON object", file=sys.stderr)
            print("EE_CAPSULE_RC=1")
            return 1
        exp_bin = pathlib.Path(args.expected_binary) if args.expected_binary else None
        problems = verify_capsule(
            capsule,
            args.expected_sha,
            expected_binary=exp_bin,
            verify_logs=args.verify_logs or artifact_check or dsr_check,
            repo_root=repo_root,
        )
        if not problems and (artifact_check or dsr_check):
            problems = verify_candidate_tree(capsule, repo_root, args.expected_version)
            if not problems:
                problems = verify_retained_test_counts(capsule, repo_root)
            if not problems and artifact_check:
                problems = verify_release_artifact(capsule, *artifact_args)
        if not problems and (args.verify_rch_policy or dsr_check):
            problems = verify_release_rch_policy(capsule, repo_root)
        if not problems and dsr_check:
            problems = verify_dsr_staging(capsule, *dsr_args, repo_root)
        if problems:
            for prob in problems:
                print(f"[emit-proof-capsule] VERIFY FAILED: {prob}", file=sys.stderr)
            print("EE_CAPSULE_RC=1")
            return 1
        print(
            f"[emit-proof-capsule] verified {cap_path} sha={capsule['source']['commit']} "
            f"stages={len(capsule['results'])} hash={capsule['capsuleHash']}"
        )
        print("EE_CAPSULE_RC=0")
        return 0

    if args.populate_candidate:
        if not args.candidate_manifest:
            print(
                "[emit-proof-capsule] --populate-candidate requires --candidate-manifest",
                file=sys.stderr,
            )
            return 1
        manifest_path = pathlib.Path(args.candidate_manifest)
        if not manifest_path.is_file():
            print(
                f"[emit-proof-capsule] candidate manifest not found: {manifest_path}",
                file=sys.stderr,
            )
            return 1
        try:
            manifest_data = json.loads(
                manifest_path.read_text(encoding="utf-8"),
                parse_constant=_reject_json_constant,
            )
        except Exception as exc:
            print(f"[emit-proof-capsule] invalid candidate manifest JSON: {exc}", file=sys.stderr)
            return 1
        if not isinstance(manifest_data, dict):
            print("[emit-proof-capsule] candidate manifest root must be a JSON object", file=sys.stderr)
            return 1
        base_override = args.base if args.base != "unknown" else None
        dirty_override = args.dirty if args.dirty != 0 else None
        capsule = populate_candidate_capsule(
            manifest_data,
            repo_root=repo_root,
            base_override=base_override,
            dirty_override=dirty_override,
        )
    else:
        capsule = build_capsule(
            pathlib.Path(args.log) if args.log else None,
            args.base,
            args.dirty,
            args.host,
            args.run_exit,
            args.command,
            repo_root=repo_root,
        )

    problems = validate(capsule)
    if problems:
        for problem in problems:
            print(f"[emit-proof-capsule] INVALID: {problem}", file=sys.stderr)
        return 1
    text = json.dumps(capsule, indent=2, allow_nan=False) + "\n"
    if args.out:
        out_path = pathlib.Path(args.out)
        out_path.parent.mkdir(parents=True, exist_ok=True)
        out_path.write_text(text, encoding="utf-8")
        print(f"[emit-proof-capsule] wrote {args.out}")
    else:
        print(text)
    return 0


if __name__ == "__main__":
    sys.exit(main())
