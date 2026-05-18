#!/usr/bin/env python3
"""Read-only franken-stack publish readiness status.

The tool intentionally performs no Cargo command, no publish attempt, and no
mutation in sibling repositories. It combines crates.io API state with static
manifest/workflow checks into a deterministic JSON or Markdown report.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
import tomllib
import urllib.error
import urllib.request
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

SCHEMA = "ee.franken_publish_status.v1"
CRATES_API = "https://crates.io/api/v1/crates/{crate}"

FNX_PUBLISH_ORDER = [
    "fnx-runtime",
    "fnx-cgse",
    "fnx-classes",
    "fnx-dispatch",
    "fnx-convert",
    "fnx-algorithms",
    "fnx-generators",
]

GROUPS: dict[str, dict[str, Any]] = {
    "fnx": {
        "display_name": "franken_networkx",
        "default_root": "/Users/jemanuel/projects/franken_networkx",
        "crate_root": "crates",
        "workflow": ".github/workflows/release.yml",
        "crate_names": FNX_PUBLISH_ORDER,
        "expected_publish_order": FNX_PUBLISH_ORDER,
    },
    "sqlmodel": {
        "display_name": "sqlmodel_rust",
        "default_root": "/Users/jemanuel/projects/sqlmodel_rust",
        "crate_root": "crates",
        "workflow": ".github/workflows/release.yml",
        "crate_names": ["sqlmodel", "sqlmodel-frankensqlite"],
        "expected_publish_order": ["sqlmodel-frankensqlite", "sqlmodel"],
    },
    "frankensearch": {
        "display_name": "frankensearch",
        "default_root": "/Users/jemanuel/projects/frankensearch",
        "crate_root": "crates",
        "workflow": ".github/workflows/release.yml",
        "manifest_overrides": {"frankensearch": "frankensearch/Cargo.toml"},
        "crate_names": [
            "frankensearch-core",
            "frankensearch-embed",
            "frankensearch-index",
            "frankensearch-lexical",
            "frankensearch-fusion",
            "frankensearch-storage",
            "frankensearch",
        ],
        "expected_publish_order": [
            "frankensearch-core",
            "frankensearch-embed",
            "frankensearch-index",
            "frankensearch-lexical",
            "frankensearch-fusion",
            "frankensearch-storage",
            "frankensearch",
        ],
    },
    "fsqlite": {
        "display_name": "frankensqlite",
        "default_root": "/Users/jemanuel/projects/frankensqlite",
        "crate_root": "crates",
        "workflow": ".github/workflows/release.yml",
        "crate_names": [
            "fsqlite-core",
            "fsqlite-types",
            "fsqlite-error",
            "fsqlite",
            "fsqlite-ext-fts5",
            "fsqlite-ext-json",
        ],
        "expected_publish_order": [
            "fsqlite-core",
            "fsqlite-types",
            "fsqlite-error",
            "fsqlite",
            "fsqlite-ext-fts5",
            "fsqlite-ext-json",
        ],
    },
}


@dataclass(frozen=True)
class ApiStatus:
    status: str
    newest_version: str | None
    repository: str | None
    http_status: int | None
    error: str | None = None


def utc_now() -> str:
    override = os.environ.get("EE_FRANKEN_PUBLISH_STATUS_NOW")
    if override:
        return override
    return datetime.now(timezone.utc).isoformat(timespec="seconds").replace("+00:00", "Z")


def stable_json(value: Any) -> str:
    return json.dumps(value, sort_keys=True, separators=(",", ":"))


def load_toml(path: Path) -> dict[str, Any] | None:
    try:
        return tomllib.loads(path.read_text(encoding="utf-8"))
    except (OSError, tomllib.TOMLDecodeError):
        return None


def workspace_version(root_manifest: dict[str, Any] | None) -> str | None:
    if not root_manifest:
        return None
    package = root_manifest.get("workspace", {}).get("package", {})
    version = package.get("version")
    return version if isinstance(version, str) else None


def manifest_version(package: dict[str, Any], root_version: str | None) -> str | None:
    version = package.get("version")
    if isinstance(version, str):
        return version
    if isinstance(version, dict) and version.get("workspace") == True:
        return root_version
    return None


def manifest_for_crate(
    root: Path,
    crate_root: str,
    crate_name: str,
    manifest_overrides: dict[str, str] | None = None,
) -> tuple[Path | None, dict[str, Any] | None]:
    override = (manifest_overrides or {}).get(crate_name)
    direct = root / (override or f"{crate_root}/{crate_name}/Cargo.toml")
    candidates = [direct] if direct.exists() else sorted((root / crate_root).glob("*/Cargo.toml"))
    for candidate in candidates:
        manifest = load_toml(candidate)
        package = (manifest or {}).get("package", {})
        if package.get("name") == crate_name:
            return candidate, manifest
    return None, None


def classify_crates_io_payload(crate_name: str, required_version: str, http_status: int, body: str) -> ApiStatus:
    if http_status == 404:
        return ApiStatus("missing", None, None, http_status)
    if http_status < 200 or http_status >= 300:
        return ApiStatus("network_unavailable", None, None, http_status, f"http_{http_status}")
    try:
        payload = json.loads(body)
    except json.JSONDecodeError:
        return ApiStatus("network_unavailable", None, None, http_status, "invalid_json")
    crate = payload.get("crate")
    if not isinstance(crate, dict):
        return ApiStatus("network_unavailable", None, None, http_status, "missing_crate_object")
    newest = crate.get("newest_version")
    repository = crate.get("repository")
    versions = payload.get("versions") if isinstance(payload.get("versions"), list) else []
    version_available = any(
        isinstance(item, dict)
        and item.get("num") == required_version
        and item.get("yanked") is not True
        for item in versions
    )
    status = "available" if version_available else "wrong_version"
    return ApiStatus(
        status=status,
        newest_version=newest if isinstance(newest, str) else None,
        repository=repository if isinstance(repository, str) else None,
        http_status=http_status,
    )


def fixture_response(fixtures_dir: Path, crate_name: str) -> tuple[int, str] | None:
    path = fixtures_dir / f"{crate_name}.json"
    if not path.exists():
        return None
    payload = json.loads(path.read_text(encoding="utf-8"))
    status = int(payload.get("http_status", 200))
    body = payload.get("body", "")
    if not isinstance(body, str):
        body = json.dumps(body, sort_keys=True)
    return status, body


def fetch_crates_io(crate_name: str, fixtures_dir: Path | None, timeout: float) -> tuple[int, str]:
    if fixtures_dir is not None:
        response = fixture_response(fixtures_dir, crate_name)
        if response is None:
            return 599, "fixture_missing"
        return response
    request = urllib.request.Request(
        CRATES_API.format(crate=crate_name),
        headers={"User-Agent": "ee-franken-publish-status/1.0"},
    )
    try:
        with urllib.request.urlopen(request, timeout=timeout) as response:
            return int(response.status), response.read().decode("utf-8", errors="replace")
    except urllib.error.HTTPError as error:
        body = error.read().decode("utf-8", errors="replace")
        return int(error.code), body
    except (OSError, TimeoutError) as error:
        return 599, str(error)


def parse_publish_order(workflow_text: str) -> list[str]:
    match = re.search(r"(?ms)crates=\(\s*(.*?)\s*\)", workflow_text)
    if not match:
        return []
    return re.findall(r"([A-Za-z0-9][A-Za-z0-9_-]*)", match.group(1))


def has_tag_gate(workflow_text: str) -> bool:
    return bool(
        re.search(
            r"startsWith\(\s*github\.ref\s*,\s*['\"]refs/tags/(?:v)?['\"]\s*\)",
            workflow_text,
        )
    )


def workflow_status(root: Path, workflow_rel: str, expected_order: list[str]) -> dict[str, Any]:
    workflow_path = root / workflow_rel
    try:
        text = workflow_path.read_text(encoding="utf-8")
    except OSError:
        return {
            "status": "missing",
            "path": workflow_rel,
            "tag_gate": False,
            "token_required": False,
            "publish_job_present": False,
            "publish_order": [],
            "missing_from_publish_order": expected_order,
            "dependency_order_ok": False,
        }
    publish_order = parse_publish_order(text)
    missing = [crate for crate in expected_order if crate not in publish_order]
    relative_positions = [publish_order.index(crate) for crate in expected_order if crate in publish_order]
    dependency_order_ok = not missing and relative_positions == sorted(relative_positions)
    return {
        "status": "ok",
        "path": workflow_rel,
        "tag_gate": has_tag_gate(text),
        "token_required": "CARGO_REGISTRY_TOKEN" in text,
        "publish_job_present": "cargo publish" in text,
        "publish_order": publish_order,
        "missing_from_publish_order": missing,
        "dependency_order_ok": dependency_order_ok,
    }


def git_status_summary(root: Path, enabled: bool) -> dict[str, Any]:
    if not enabled:
        return {"checked": False, "dirty": None, "entry_count": None}
    try:
        # nosec B603
        output = subprocess.check_output(
            ["git", "-C", str(root), "status", "--short"],
            text=True,
            stderr=subprocess.DEVNULL,
            timeout=5,
        )
    except (OSError, subprocess.SubprocessError):
        return {"checked": True, "dirty": None, "entry_count": None, "status": "unavailable"}
    entries = [line for line in output.splitlines() if line.strip()]
    return {"checked": True, "dirty": bool(entries), "entry_count": len(entries), "status": "ok"}


def crate_blocking_reasons(
    api: ApiStatus,
    local_manifest: dict[str, Any],
    workflow: dict[str, Any],
    crate_name: str,
) -> list[str]:
    reasons: list[str] = []
    if local_manifest["status"] != "ok":
        reasons.append("local_manifest_unavailable")
    if api.status == "missing":
        reasons.append("crates_io_missing")
    elif api.status == "wrong_version":
        reasons.append("crates_io_wrong_version")
    elif api.status == "network_unavailable":
        reasons.append("crates_io_network_unavailable")
    if not workflow.get("publish_job_present"):
        reasons.append("workflow_publish_job_missing")
    if crate_name not in workflow.get("publish_order", []):
        reasons.append("workflow_missing_publish_crate")
    if not workflow.get("tag_gate"):
        reasons.append("workflow_tag_gate_missing")
    if not workflow.get("token_required"):
        reasons.append("workflow_publish_token_check_missing")
    return reasons


def evaluate_group(
    group: str,
    root: Path,
    fixtures_dir: Path | None,
    timeout: float,
    git_status_enabled: bool,
) -> dict[str, Any]:
    spec = GROUPS[group]
    root_manifest = load_toml(root / "Cargo.toml")
    root_version = workspace_version(root_manifest)
    workflow = workflow_status(root, spec["workflow"], spec["expected_publish_order"])
    crates = []
    for crate_name in spec["crate_names"]:
        manifest_path, manifest = manifest_for_crate(
            root,
            spec["crate_root"],
            crate_name,
            spec.get("manifest_overrides"),
        )
        package = (manifest or {}).get("package", {})
        version = manifest_version(package, root_version)
        local_manifest = {
            "status": "ok" if manifest is not None and version else "missing",
            "path": str(manifest_path.relative_to(root)) if manifest_path else None,
            "version": version,
            "description_present": bool(package.get("description")),
            "license_present": bool(package.get("license") or package.get("license-file")),
        }
        required_version = version or "unknown"
        http_status, body = fetch_crates_io(crate_name, fixtures_dir, timeout)
        api = classify_crates_io_payload(crate_name, required_version, http_status, body)
        reasons = crate_blocking_reasons(api, local_manifest, workflow, crate_name)
        crates.append(
            {
                "crate_name": crate_name,
                "required_version": required_version,
                "source_of_truth": local_manifest["path"],
                "crates_io": {
                    "status": api.status,
                    "http_status": api.http_status,
                    "newest_version": api.newest_version,
                    "repository": api.repository,
                    "error": api.error,
                },
                "local_manifest": local_manifest,
                "workflow": {
                    "status": workflow["status"],
                    "in_publish_order": crate_name in workflow.get("publish_order", []),
                    "order_index": (
                        workflow.get("publish_order", []).index(crate_name)
                        if crate_name in workflow.get("publish_order", [])
                        else None
                    ),
                    "tag_gate": workflow.get("tag_gate"),
                    "token_required": workflow.get("token_required"),
                },
                "blocking_reasons": reasons,
                "ready_to_publish": not reasons,
            }
        )
    unresolved = [crate for crate in crates if not crate["ready_to_publish"]]
    return {
        "group": group,
        "display_name": spec["display_name"],
        "source": {
            "kind": "sibling_repo",
            "repo_key": spec["display_name"],
            "root_status": "present" if root.exists() else "missing",
        },
        "workspace_version": root_version,
        "git": git_status_summary(root, git_status_enabled),
        "workflow": workflow,
        "summary": {
            "crate_count": len(crates),
            "available_count": sum(1 for crate in crates if crate["crates_io"]["status"] == "available"),
            "missing_count": sum(1 for crate in crates if crate["crates_io"]["status"] == "missing"),
            "wrong_version_count": sum(1 for crate in crates if crate["crates_io"]["status"] == "wrong_version"),
            "network_unavailable_count": sum(
                1 for crate in crates if crate["crates_io"]["status"] == "network_unavailable"
            ),
            "ready_count": sum(1 for crate in crates if crate["ready_to_publish"]),
            "blocked_count": len(unresolved),
        },
        "blocking_reason": (
            "all_required_crates_ready"
            if not unresolved
            else "crates_io_publication_or_workflow_blocked"
        ),
        "crates": crates,
    }


def aggregate_summary(groups: list[dict[str, Any]]) -> dict[str, Any]:
    total_crates = sum(group["summary"]["crate_count"] for group in groups)
    available_crates = sum(group["summary"].get("available_count", 0) for group in groups)
    ready_crates = sum(group["summary"]["ready_count"] for group in groups)
    blocked_crates = sum(group["summary"]["blocked_count"] for group in groups)
    missing_crates = sum(group["summary"]["missing_count"] for group in groups)
    wrong_version_crates = sum(group["summary"]["wrong_version_count"] for group in groups)
    network_unavailable_crates = sum(
        group["summary"]["network_unavailable_count"] for group in groups
    )
    return {
        "group_count": len(groups),
        "crate_count": total_crates,
        "available_count": available_crates,
        "ready_count": ready_crates,
        "blocked_count": blocked_crates,
        "missing_count": missing_crates,
        "wrong_version_count": wrong_version_crates,
        "network_unavailable_count": network_unavailable_crates,
        "all_required_crates_ready": blocked_crates == 0,
    }


def render_markdown(report: dict[str, Any]) -> str:
    groups = report["groups"]
    aggregate = report.get("aggregate") or aggregate_summary(groups)
    lines = [
        f"Franken publish status `{report['schema']}` generated `{report['generated_at']}`.",
        (
            f"Aggregate: `{aggregate['ready_count']}/{aggregate['crate_count']}` crates ready; "
            f"`{aggregate['blocked_count']}` blocked "
            f"(`{aggregate['available_count']}` available on crates.io; "
            f"`{aggregate['missing_count']}` missing, "
            f"`{aggregate['wrong_version_count']}` wrong-version, "
            f"`{aggregate['network_unavailable_count']}` network-unavailable)."
        ),
        "",
    ]
    for group in groups:
        summary = group["summary"]
        lines.append(
            f"## {group['display_name']} ({group['group']}) — {summary['ready_count']}/{summary['crate_count']} ready"
        )
        lines.append(f"- blocking_reason: `{group['blocking_reason']}`")
        git = group["git"]
        if git.get("checked"):
            lines.append(
                f"- sibling_git_dirty: `{str(git.get('dirty')).lower()}` entries: `{git.get('entry_count')}`"
            )
        lines.append(
            "- workflow: "
            f"`publish_job={str(group['workflow'].get('publish_job_present')).lower()}` "
            f"`tag_gate={str(group['workflow'].get('tag_gate')).lower()}` "
            f"`dependency_order_ok={str(group['workflow'].get('dependency_order_ok')).lower()}`"
        )
        for crate in group["crates"]:
            reasons = ", ".join(f"`{reason}`" for reason in crate["blocking_reasons"]) or "none"
            lines.append(
                f"- `{crate['crate_name']}` {crate['required_version']}: "
                f"`{crate['crates_io']['status']}`; blocking: {reasons}"
            )
        lines.append("")
    return "\n".join(lines).rstrip() + "\n"


def selected_groups(args: argparse.Namespace) -> list[str]:
    return sorted(GROUPS) if args.all_groups else (args.group if args.group else ["fnx"])


def build_report(args: argparse.Namespace) -> dict[str, Any]:
    groups_to_evaluate = selected_groups(args)
    groups = []
    for group in groups_to_evaluate:
        spec = GROUPS[group]
        root = Path(args.root_override or args.roots.get(group) or spec["default_root"]).expanduser()
        fixtures_dir = Path(args.fixtures_dir).expanduser() if args.fixtures_dir else None
        groups.append(
            evaluate_group(
                group=group,
                root=root,
                fixtures_dir=fixtures_dir,
                timeout=args.timeout,
                git_status_enabled=not args.no_git_status,
            )
        )
    return {
        "schema": SCHEMA,
        "generated_at": args.generated_at or utc_now(),
        "mode": "fixture" if args.fixtures_dir else "live",
        "aggregate": aggregate_summary(groups),
        "groups": groups,
    }


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--group", action="append", choices=sorted(GROUPS), help="dependency group to inspect")
    parser.add_argument(
        "--all-groups",
        action="store_true",
        help="inspect every known dependency group in deterministic order",
    )
    parser.add_argument("--fixtures-dir", help="directory of crates.io response fixtures named <crate>.json")
    parser.add_argument("--generated-at", help="override generated_at for deterministic tests")
    parser.add_argument("--timeout", type=float, default=10.0, help="crates.io API timeout in seconds")
    parser.add_argument("--markdown", action="store_true", help="emit Beads-ready Markdown instead of JSON")
    parser.add_argument("--no-git-status", action="store_true", help="skip sibling repo git status for stable fixtures")
    parser.add_argument("--root-override", help="use the same sibling root for every selected group")
    for group in sorted(GROUPS):
        parser.add_argument(f"--{group}-root", dest=f"{group}_root", help=f"override {group} sibling root")
    args = parser.parse_args(argv)
    if args.all_groups and args.group:
        parser.error("--all-groups cannot be combined with --group")
    args.roots = {group: getattr(args, f"{group}_root") for group in GROUPS}
    return args


def main(argv: list[str]) -> int:
    args = parse_args(argv)
    report = build_report(args)
    if args.markdown:
        sys.stdout.write(render_markdown(report))
    else:
        sys.stdout.write(stable_json(report) + "\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
