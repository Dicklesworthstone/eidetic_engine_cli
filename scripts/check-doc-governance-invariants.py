#!/usr/bin/env python3
"""Mechanical doc-governance invariant gate (bd-reality-core-convergence-1azkt.78).

Enforces consistency across AGENTS.md, README.md, CHANGELOG.md,
PUBLISH_CHECKLIST.md, COMPREHENSIVE_PLAN.md, docs/feature_flag_registry.md,
docs/env_vars.md, Cargo.toml, and .beads/issues.jsonl in <100ms without
invoking Cargo.

Exit codes:
  0  all invariants hold (or --self-test passed)
  1  one or more doc-governance invariants violated
  2  CLI usage error
  3  cannot answer (missing/malformed repository metadata)
"""

from __future__ import annotations

import argparse
import datetime
import json
import os
import pathlib
import re
import sys
import tempfile
import tomllib
from typing import Any

SCHEMA = "ee.doc_governance_invariants.v1"
EXIT_OK = 0
EXIT_RED = 1
EXIT_USAGE = 2
EXIT_CANNOT = 3


class CannotAnswer(Exception):
    """Raised when required source metadata is missing or unreadable."""


def load_beads_statuses(beads_path: pathlib.Path) -> dict[str, str]:
    if not beads_path.is_file():
        raise CannotAnswer(f"missing beads ledger: {beads_path}")
    statuses: dict[str, str] = {}
    try:
        text = beads_path.read_text(encoding="utf-8")
    except OSError as exc:
        raise CannotAnswer(f"failed to read {beads_path}: {exc}") from exc
    for lineno, raw in enumerate(text.splitlines(), 1):
        line = raw.strip()
        if not line:
            continue
        try:
            obj = json.loads(line)
        except json.JSONDecodeError as exc:
            raise CannotAnswer(f"{beads_path}:{lineno} invalid JSON: {exc}") from exc
        bid = obj.get("id")
        status = obj.get("status")
        if isinstance(bid, str) and isinstance(status, str):
            statuses[bid] = status
    if not statuses:
        raise CannotAnswer(f"{beads_path} yielded 0 bead records")
    return statuses


def load_cargo_version(cargo_path: pathlib.Path) -> str:
    if not cargo_path.is_file():
        raise CannotAnswer(f"missing Cargo.toml: {cargo_path}")
    try:
        data = tomllib.loads(cargo_path.read_text(encoding="utf-8"))
    except (OSError, tomllib.TOMLDecodeError) as exc:
        raise CannotAnswer(f"failed to parse {cargo_path}: {exc}") from exc
    version = data.get("package", {}).get("version")
    if not isinstance(version, str) or not re.fullmatch(r"\d+\.\d+\.\d+(?:[-+].+)?", version):
        raise CannotAnswer(f"missing/invalid [package].version in {cargo_path}")
    return version


def read_required_doc(path: pathlib.Path) -> str:
    if not path.is_file():
        raise CannotAnswer(f"missing required document: {path}")
    try:
        return path.read_text(encoding="utf-8")
    except OSError as exc:
        raise CannotAnswer(f"failed to read {path}: {exc}") from exc


def parse_semver_tuple(ver: str) -> tuple[int, int, int]:
    base = ver.split("-", 1)[0].split("+", 1)[0]
    parts = [int(p) for p in base.split(".")]
    while len(parts) < 3:
        parts.append(0)
    return (parts[0], parts[1], parts[2])


def check_repo(
    root: pathlib.Path,
    *,
    today: datetime.date | None = None,
) -> tuple[list[dict[str, Any]], list[str]]:
    check_date = today or datetime.date.today()
    violations: list[dict[str, Any]] = []
    checks_run: list[str] = []

    def add_violation(
        code: str,
        file: str,
        message: str,
        *,
        line: int | None = None,
    ) -> None:
        item: dict[str, Any] = {"code": code, "file": file, "message": message}
        if line is not None:
            item["line"] = line
        violations.append(item)

    cargo_version = load_cargo_version(root / "Cargo.toml")
    beads = load_beads_statuses(root / ".beads" / "issues.jsonl")

    agents = read_required_doc(root / "AGENTS.md")
    readme = read_required_doc(root / "README.md")
    changelog = read_required_doc(root / "CHANGELOG.md")
    checklist = read_required_doc(root / "PUBLISH_CHECKLIST.md")
    comp_plan = read_required_doc(root / "COMPREHENSIVE_PLAN.md")
    ff_registry = read_required_doc(root / "docs" / "feature_flag_registry.md")
    env_vars_doc = read_required_doc(root / "docs" / "env_vars.md")

    # 1. AGENTS.md DSR-Only vs hosted CI Static contradiction
    checks_run.append("agents_dsr_only_no_hosted_ci_claim")
    if "Standing Policy: DSR Only — Never GitHub Actions" in agents:
        for idx, line in enumerate(agents.splitlines(), 1):
            if "The hosted executing gate for cargo-free invariants is" in line:
                add_violation(
                    "agents_hosted_ci_contradiction",
                    "AGENTS.md",
                    "AGENTS.md claims hosted CI Static executes cargo-free invariants despite Standing Policy: DSR Only — Never GitHub Actions",
                    line=idx,
                )
        if re.search(r"Actions signing\s+job being disabled repo-wide", agents) and "DSR-only" not in agents[max(0, agents.find("Actions signing") - 160):agents.find("Actions signing") + 160]:
            add_violation(
                "agents_hosted_ci_signing_framing",
                "AGENTS.md",
                "AGENTS.md signing note should explicitly frame DSR-only policy alongside hosted Actions disablement",
            )

    # 2. AGENTS.md master branch RULE-1 disposition note
    checks_run.append("agents_master_branch_disposition")
    if "Git Branch: ONLY Use `main`, NEVER `master`" in agents:
        has_disposition = (
            "Disposition note (RULE 1)" in agents
            and "origin/master" in agents
            and "git branch -d master" in agents
        )
        if not has_disposition:
            add_violation(
                "agents_master_disposition_missing",
                "AGENTS.md",
                "AGENTS.md must record the RULE-1 non-destructive disposition and operator-only removal commands for stray master / origin/master refs",
            )

    # 3. README.md: tracked-on/at closed beads, PPR comparison row, EE_FS_BOUNDARY
    checks_run.append("readme_tracked_beads_open")
    for idx, line in enumerate(readme.splitlines(), 1):
        # Match "Tracked on `bd-...`" / "tracked at `bd-...`"
        for m in re.finditer(
            r"\btracked\s+(?:on|at)\s+`?(bd-[a-z0-9.-]+)`?",
            line,
            flags=re.IGNORECASE,
        ):
            bid = m.group(1).rstrip(".,;:)")
            status = beads.get(bid)
            if status is None:
                add_violation(
                    "readme_unknown_bead_tracker",
                    "README.md",
                    f"README cites tracking bead `{bid}` which is absent from .beads/issues.jsonl",
                    line=idx,
                )
            elif status == "closed":
                add_violation(
                    "readme_closed_bead_tracker",
                    "README.md",
                    f"README claims work is tracked on/at `{bid}`, but that bead is already closed",
                    line=idx,
                )

    checks_run.append("readme_ppr_consistency")
    ppr_unavailable = "Personalized PageRank" in readme and "**unavailable**" in readme
    for idx, line in enumerate(readme.splitlines(), 1):
        if line.lstrip().startswith("|") and "Graph analytics" in line and "PPR" in line:
            if ppr_unavailable and "✅" in line and "pending upstream" not in line.lower() and "unavailable" not in line.lower():
                add_violation(
                    "readme_ppr_contradiction",
                    "README.md",
                    "README Comparison table marks PPR as ✅ while Limitations states Personalized PageRank is unavailable",
                    line=idx,
                )

    checks_run.append("readme_ee_fs_boundary_documented")
    if "`EE_FS_BOUNDARY`" in env_vars_doc and "`EE_FS_BOUNDARY`" not in readme:
        add_violation(
            "readme_missing_ee_fs_boundary",
            "README.md",
            "README.md omits `EE_FS_BOUNDARY` even though it is documented in docs/env_vars.md",
        )

    # 4. CHANGELOG.md: Release surface version and Version timeline completeness
    checks_run.append("changelog_release_surface_and_timeline")
    scope_versions: set[str] = set()
    in_scope = False
    for line in changelog.splitlines():
        if line.startswith("## Scope and methodology"):
            in_scope = True
            continue
        if in_scope and line.startswith("Release surface"):
            break
        if in_scope and line.startswith("|"):
            for m in re.finditer(r"\*\*`(\d+\.\d+\.\d+)`\*\*", line):
                ver = m.group(1)
                if parse_semver_tuple(ver) >= (0, 16, 0):
                    scope_versions.add(ver)

    release_surface_match = re.search(
        r"Latest \*\*published\*\* GitHub Release:\s*\[`v(\d+\.\d+\.\d+)`\]",
        changelog,
    )
    if not release_surface_match:
        add_violation(
            "changelog_release_header_missing",
            "CHANGELOG.md",
            "CHANGELOG.md is missing the `Latest **published** GitHub Release: [`vX.Y.Z`]` header",
        )
    else:
        rel_ver = release_surface_match.group(1)
        has_cargo_heading = bool(
            re.search(rf"^## \[{re.escape(cargo_version)}\]", changelog, flags=re.MULTILINE)
        )
        if has_cargo_heading and parse_semver_tuple(rel_ver) < parse_semver_tuple(cargo_version):
            add_violation(
                "changelog_release_stale",
                "CHANGELOG.md",
                f"CHANGELOG.md Release surface reports v{rel_ver} while Cargo.toml and CHANGELOG section [{cargo_version}] are already at {cargo_version}",
            )

    timeline_versions: set[str] = set()
    in_timeline = False
    for line in changelog.splitlines():
        if line.startswith("### Version timeline"):
            in_timeline = True
            continue
        if in_timeline:
            if line.startswith("## ") or (line.strip() and not line.startswith("|") and not line.startswith("Versions ")):
                if timeline_versions:
                    break
            if line.startswith("|"):
                m = re.match(r"^\|\s*\[(\d+\.\d+\.\d+)\]", line)
                if m:
                    timeline_versions.add(m.group(1))
    missing_timeline = sorted(scope_versions - timeline_versions, key=parse_semver_tuple)
    if missing_timeline:
        add_violation(
            "changelog_timeline_missing_version",
            "CHANGELOG.md",
            f"CHANGELOG.md Version timeline table omits released versions present in Scope: {', '.join(missing_timeline)}",
        )

    # 5. PUBLISH_CHECKLIST.md & CHANGELOG.md phantom build manifest claim
    checks_run.append("manifest_absence_consistency")
    agents_says_no_manifest = "not produced by the hand-cut flow" in agents
    if agents_says_no_manifest:
        for idx, line in enumerate(checklist.splitlines(), 1):
            if "carry SHA-256 checksums and a build manifest" in line:
                add_violation(
                    "manifest_claim_contradiction",
                    "PUBLISH_CHECKLIST.md",
                    "PUBLISH_CHECKLIST.md claims current manual releases carry a build manifest, contradicting AGENTS.md",
                    line=idx,
                )
        for idx, line in enumerate(changelog.splitlines(), 1):
            if "checksums, a build manifest," in line or "SHA-256 checksums and a build manifest" in line:
                add_violation(
                    "manifest_claim_contradiction",
                    "CHANGELOG.md",
                    "CHANGELOG.md release surface claims current manual release ships a build manifest, contradicting AGENTS.md",
                    line=idx,
                )

    # 6. docs/feature_flag_registry.md: serve unconditional compilation + 90-day reservation contract
    checks_run.append("feature_flag_registry_reservations")
    serve_compiled_unconditionally = "serve` adapter is compiled unconditionally" in readme
    in_registry = False
    for idx, line in enumerate(ff_registry.splitlines(), 1):
        if line.startswith("## Registry"):
            in_registry = True
            continue
        if in_registry and line.startswith("## "):
            break
        if not in_registry or not line.strip().startswith("| `"):
            continue
        cells = [c.strip() for c in line.strip().strip("|").split("|")]
        if len(cells) < 6:
            continue
        flag_name = cells[0].strip("`")
        status_val = cells[1]
        gate_surface = cells[3]
        owner_cell = cells[4]

        if flag_name == "serve" and serve_compiled_unconditionally and "Not implemented in v0.1" in gate_surface:
            add_violation(
                "feature_registry_serve_stale",
                "docs/feature_flag_registry.md",
                "docs/feature_flag_registry.md claims `serve` is 'Not implemented in v0.1', contradicting unconditional compilation in src/serve.rs and README.md",
                line=idx,
            )

        if status_val == "reserved":
            # Must reference an active non-closed bead (not only a closed bead) AND have an unexpired renewal/review-by date.
            bead_ids = re.findall(r"bd-[a-z0-9.-]+", owner_cell)
            open_beads = [b for b in bead_ids if beads.get(b) not in (None, "closed")]
            if not open_beads:
                add_violation(
                    "feature_registry_reserved_no_open_bead",
                    "docs/feature_flag_registry.md",
                    f"Reserved flag `{flag_name}` has no open tracking bead in Owner column ({owner_cell}); found statuses: {{b: beads.get(b) for b in bead_ids}}",
                    line=idx,
                )
            review_match = re.search(r"review by `?(\d{4}-\d{2}-\d{2})`?", gate_surface)
            renewed_match = re.search(r"renewed `?(\d{4}-\d{2}-\d{2})`?", gate_surface)
            if not review_match or not renewed_match:
                add_violation(
                    "feature_registry_reserved_missing_renewal",
                    "docs/feature_flag_registry.md",
                    f"Reserved flag `{flag_name}` must record explicit `renewed YYYY-MM-DD, review by YYYY-MM-DD` within the 90-day reservation window",
                    line=idx,
                )
            else:
                try:
                    renewed_dt = datetime.date.fromisoformat(renewed_match.group(1))
                    review_dt = datetime.date.fromisoformat(review_match.group(1))
                except ValueError:
                    add_violation(
                        "feature_registry_reserved_bad_date",
                        "docs/feature_flag_registry.md",
                        f"Reserved flag `{flag_name}` has invalid ISO renewal/review date",
                        line=idx,
                    )
                else:
                    if (review_dt - renewed_dt).days > 95 or review_dt < renewed_dt:
                        add_violation(
                            "feature_registry_reserved_window_exceeded",
                            "docs/feature_flag_registry.md",
                            f"Reserved flag `{flag_name}` renewal window ({renewed_dt} -> {review_dt}) exceeds 90-day policy",
                            line=idx,
                        )
                    elif check_date > review_dt:
                        add_violation(
                            "feature_registry_reserved_expired",
                            "docs/feature_flag_registry.md",
                            f"Reserved flag `{flag_name}` review deadline {review_dt} expired as of {check_date}",
                            line=idx,
                        )

    # 7. COMPREHENSIVE_PLAN.md stale 0.14.x / franken-stack.lock banner
    checks_run.append("comprehensive_plan_version_banner")
    banner = "\n".join(comp_plan.splitlines()[:20])
    if "0.14.x" in banner or "sibling franken-stack" in banner:
        add_violation(
            "comprehensive_plan_stale_banner",
            "COMPREHENSIVE_PLAN.md",
            "COMPREHENSIVE_PLAN.md top status banner still carries stale `0.14.x` / `sibling franken-stack` text",
            line=7,
        )
    if cargo_version not in banner and f"{parse_semver_tuple(cargo_version)[0]}.{parse_semver_tuple(cargo_version)[1]}." not in banner:
        add_violation(
            "comprehensive_plan_missing_current_version",
            "COMPREHENSIVE_PLAN.md",
            f"COMPREHENSIVE_PLAN.md top status banner does not reflect current Cargo.toml version {cargo_version}",
            line=7,
        )

    return violations, checks_run


def build_synthetic_clean_tree(root: pathlib.Path) -> None:
    (root / ".beads").mkdir(parents=True, exist_ok=True)
    (root / "docs").mkdir(parents=True, exist_ok=True)

    (root / "Cargo.toml").write_text(
        '[package]\nname = "eidetic-engine"\nversion = "0.18.0"\n',
        encoding="utf-8",
    )
    beads_lines = [
        {"id": "bd-reality-core-convergence-1azkt.15", "status": "closed"},
        {"id": "bd-reality-core-convergence-1azkt.48", "status": "closed"},
        {"id": "bd-reality-core-convergence-1azkt.77", "status": "open"},
        {"id": "bd-reality-core-convergence-1azkt.82", "status": "open"},
        {"id": "bd-17c65.11.7", "status": "closed"},
    ]
    (root / ".beads" / "issues.jsonl").write_text(
        "\n".join(json.dumps(x) for x in beads_lines) + "\n",
        encoding="utf-8",
    )
    (root / "AGENTS.md").write_text(
        "# AGENTS\n\n"
        "## Standing Policy: DSR Only — Never GitHub Actions\n\n"
        "## Git Branch: ONLY Use `main`, NEVER `master`\n\n"
        "**Disposition note (RULE 1):** Legacy `master` and `origin/master` exist; "
        "remove only with explicit approval via `git branch -d master && git push origin --delete master`.\n\n"
        "Static checks run via `./scripts/verify.sh --static-only` and local `dsr` / `act`.\n"
        "`ee-v{version}-manifest.json` is not produced by the hand-cut flow.\n"
        "Consistent with the DSR-only policy and Actions signing job being disabled repo-wide.\n",
        encoding="utf-8",
    )
    (root / "README.md").write_text(
        "# README\n\n"
        "| Graph analytics (HITS, PageRank, proximity, causal paths; PPR pending upstream) | ✅ |\n"
        "| `EE_FS_BOUNDARY` | sandbox root |\n"
        "The loopback-only `ee serve` adapter is compiled unconditionally.\n"
        "| Personalized PageRank | **unavailable**; Tracked on `bd-reality-core-convergence-1azkt.82` |\n",
        encoding="utf-8",
    )
    (root / "docs" / "env_vars.md").write_text(
        "| `EE_FS_BOUNDARY` | paths | absolute existing directory | none |\n",
        encoding="utf-8",
    )
    (root / "CHANGELOG.md").write_text(
        "# Changelog\n\n"
        "## Scope and methodology\n\n"
        "| Window | Coverage |\n| --- | --- |\n"
        "| 2026-09-26 | **`0.16.0`** |\n"
        "| 2026-10-02 | **`0.17.0`** |\n"
        "| 2026-10-14 | **`0.18.0`** |\n\n"
        "Release surface (as of 2026-10-14):\n\n"
        "- Latest **published** GitHub Release: [`v0.18.0`](https://example.invalid/v0.18.0)\n\n"
        "### Version timeline (tags and GitHub Releases)\n\n"
        "| Version | Date | GitHub Release | Notes |\n| --- | --- | --- | --- |\n"
        "| [0.18.0](#0180) | 2026-10-14 | yes | ... |\n"
        "| [0.17.0](#0170) | 2026-10-02 | yes | ... |\n"
        "| [0.16.0](#0160) | 2026-09-26 | yes | ... |\n\n"
        "## [0.18.0] - 2026-10-14\n",
        encoding="utf-8",
    )
    (root / "PUBLISH_CHECKLIST.md").write_text(
        "# Publish Checklist\n\n"
        "The current manual releases carry SHA-256 checksums and minisign signatures "
        "(without a release `manifest.json` build manifest). They do not carry Sigstore bundles or SLSA attestations.\n",
        encoding="utf-8",
    )
    (root / "COMPREHENSIVE_PLAN.md").write_text(
        "# COMPREHENSIVE PLAN\n\n"
        "> Status: durable vision document (package version `0.18.0`, crates.io registry pins).\n",
        encoding="utf-8",
    )
    (root / "docs" / "feature_flag_registry.md").write_text(
        "# Feature Flag Registry\n\n"
        "## Registry\n\n"
        "| Flag | Status | Cargo value | Gate surface | Owner bead | Since version |\n"
        "| --- | --- | --- | --- | --- | --- |\n"
        "| `json` | reserved | `[]` | JSON-only profile (renewed `2026-11-21`, review by `2027-02-19`). | `bd-reality-core-convergence-1azkt.77` | 0.1.0 |\n"
        "| `serve` | reserved | `[]` | Loopback adapter compiled unconditionally (renewed `2026-11-21`, review by `2027-02-19`). | `bd-reality-core-convergence-1azkt.77` | 0.1.0 |\n"
        "| `science-analytics` | reserved | `[]` | Future backend (renewed `2026-11-21`, review by `2027-02-19`). | `bd-reality-core-convergence-1azkt.77` | 0.1.0 |\n",
        encoding="utf-8",
    )


def run_self_test() -> int:
    fixed_today = datetime.date(2026, 11, 21)
    arms_passed = 0
    arms_failed = 0

    def assert_arm(name: str, mutate_fn: Any, expected_code: str | None) -> None:
        nonlocal arms_passed, arms_failed
        with tempfile.TemporaryDirectory(prefix="ee-doc-gov-st-") as tmp:
            root = pathlib.Path(tmp)
            build_synthetic_clean_tree(root)
            if mutate_fn is not None:
                mutate_fn(root)
            violations, _ = check_repo(root, today=fixed_today)
            codes = [v["code"] for v in violations]
            if expected_code is None:
                if not codes:
                    arms_passed += 1
                    print(f"ok: self-test positive arm `{name}` passed (0 violations)")
                else:
                    arms_failed += 1
                    print(f"error: self-test positive arm `{name}` failed: {violations}", file=sys.stderr)
            else:
                if expected_code in codes:
                    arms_passed += 1
                    print(f"ok: self-test negative arm `{name}` caught `{expected_code}`")
                else:
                    arms_failed += 1
                    print(
                        f"error: self-test negative arm `{name}` missed `{expected_code}`; got {codes}",
                        file=sys.stderr,
                    )

    # Positive arm
    assert_arm("clean_synthetic_tree", None, None)

    # Planted negative arms covering all 7 contradiction classes
    assert_arm(
        "planted_agents_hosted_ci",
        lambda r: (r / "AGENTS.md").write_text(
            (r / "AGENTS.md").read_text(encoding="utf-8")
            + "\nThe hosted executing gate for cargo-free invariants is `.github/workflows/ci-static.yml`.\n",
            encoding="utf-8",
        ),
        "agents_hosted_ci_contradiction",
    )
    assert_arm(
        "planted_missing_master_disposition",
        lambda r: (r / "AGENTS.md").write_text(
            (r / "AGENTS.md").read_text(encoding="utf-8").replace("Disposition note (RULE 1)", "Note"),
            encoding="utf-8",
        ),
        "agents_master_disposition_missing",
    )
    assert_arm(
        "planted_readme_closed_bead_ref",
        lambda r: (r / "README.md").write_text(
            (r / "README.md").read_text(encoding="utf-8").replace(
                "bd-reality-core-convergence-1azkt.82",
                "bd-reality-core-convergence-1azkt.15",
            ),
            encoding="utf-8",
        ),
        "readme_closed_bead_tracker",
    )
    assert_arm(
        "planted_readme_ppr_table_lie",
        lambda r: (r / "README.md").write_text(
            (r / "README.md").read_text(encoding="utf-8").replace(
                "HITS, PageRank, proximity, causal paths; PPR pending upstream",
                "PPR, HITS, PageRank, proximity, causal paths",
            ),
            encoding="utf-8",
        ),
        "readme_ppr_contradiction",
    )
    assert_arm(
        "planted_readme_missing_ee_fs_boundary",
        lambda r: (r / "README.md").write_text(
            (r / "README.md").read_text(encoding="utf-8").replace("EE_FS_BOUNDARY", "EE_OTHER"),
            encoding="utf-8",
        ),
        "readme_missing_ee_fs_boundary",
    )
    assert_arm(
        "planted_changelog_stale_release",
        lambda r: (r / "CHANGELOG.md").write_text(
            (r / "CHANGELOG.md").read_text(encoding="utf-8").replace("[`v0.18.0`]", "[`v0.15.2`]"),
            encoding="utf-8",
        ),
        "changelog_release_stale",
    )
    assert_arm(
        "planted_changelog_missing_timeline_row",
        lambda r: (r / "CHANGELOG.md").write_text(
            (r / "CHANGELOG.md").read_text(encoding="utf-8").replace("| [0.17.0](#0170) | 2026-10-02 | yes | ... |\n", ""),
            encoding="utf-8",
        ),
        "changelog_timeline_missing_version",
    )
    assert_arm(
        "planted_publish_checklist_phantom_manifest",
        lambda r: (r / "PUBLISH_CHECKLIST.md").write_text(
            "The current manual releases carry SHA-256 checksums and a build manifest.\n",
            encoding="utf-8",
        ),
        "manifest_claim_contradiction",
    )
    assert_arm(
        "planted_feature_registry_serve_stale",
        lambda r: (r / "docs" / "feature_flag_registry.md").write_text(
            (r / "docs" / "feature_flag_registry.md").read_text(encoding="utf-8").replace(
                "Loopback adapter compiled unconditionally",
                "Not implemented in v0.1",
            ),
            encoding="utf-8",
        ),
        "feature_registry_serve_stale",
    )
    assert_arm(
        "planted_feature_registry_closed_owner_and_expired_window",
        lambda r: (r / "docs" / "feature_flag_registry.md").write_text(
            (r / "docs" / "feature_flag_registry.md").read_text(encoding="utf-8")
            .replace("bd-reality-core-convergence-1azkt.77", "bd-17c65.11.7")
            .replace("2027-02-19", "2026-08-15"),
            encoding="utf-8",
        ),
        "feature_registry_reserved_no_open_bead",
    )
    assert_arm(
        "planted_feature_registry_expired_date",
        lambda r: (r / "docs" / "feature_flag_registry.md").write_text(
            (r / "docs" / "feature_flag_registry.md").read_text(encoding="utf-8")
            .replace("2026-11-21", "2026-05-15")
            .replace("2027-02-19", "2026-08-13"),
            encoding="utf-8",
        ),
        "feature_registry_reserved_expired",
    )
    assert_arm(
        "planted_comprehensive_plan_stale_banner",
        lambda r: (r / "COMPREHENSIVE_PLAN.md").write_text(
            "# COMPREHENSIVE PLAN\n\n> Status: (package version `0.14.x`, sibling franken-stack pins in `franken-stack.lock`).\n",
            encoding="utf-8",
        ),
        "comprehensive_plan_stale_banner",
    )

    print(f"EE_RC20261120_78_RC={0 if arms_failed == 0 else 1} self_test_passed={arms_passed} self_test_failed={arms_failed}")
    return EXIT_OK if arms_failed == 0 else EXIT_RED


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="Check doc-governance invariants.")
    parser.add_argument("--json", action="store_true", help="Emit structured JSON verdict.")
    parser.add_argument("--self-test", action="store_true", help="Run positive and negative self-test arms.")
    parser.add_argument(
        "--root",
        type=pathlib.Path,
        default=pathlib.Path(__file__).resolve().parent.parent,
        help="Repository root to inspect.",
    )
    try:
        args = parser.parse_args(argv)
    except SystemExit as exc:
        return EXIT_USAGE if exc.code != 0 else EXIT_OK

    if args.self_test:
        return run_self_test()

    try:
        violations, checks_run = check_repo(args.root.resolve())
    except CannotAnswer as exc:
        if args.json:
            print(json.dumps({"schema": SCHEMA, "verdict": "cannot_answer", "error": str(exc)}, indent=2))
        else:
            print(f"error: {exc}", file=sys.stderr)
        return EXIT_CANNOT

    rc = EXIT_OK if not violations else EXIT_RED
    if args.json:
        payload = {
            "schema": SCHEMA,
            "verdict": "pass" if rc == EXIT_OK else "fail",
            "controlMarker": f"EE_RC20261120_78_RC={rc}",
            "checksRun": checks_run,
            "violationCount": len(violations),
            "violations": violations,
        }
        print(json.dumps(payload, indent=2))
    else:
        if violations:
            print(f"FAIL: {len(violations)} doc-governance invariant violation(s):", file=sys.stderr)
            for v in violations:
                loc = f"{v['file']}:{v['line']}" if "line" in v else v["file"]
                print(f"  - [{v['code']}] {loc}: {v['message']}", file=sys.stderr)
        else:
            print(
                f"ok: all {len(checks_run)} doc-governance invariant checks passed "
                f"(EE_RC20261120_78_RC=0)"
            )
    return rc


if __name__ == "__main__":
    sys.exit(main())
