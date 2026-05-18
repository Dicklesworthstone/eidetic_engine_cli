#!/usr/bin/env python3
"""Route RCH compile failures to the likely owner without mutating state."""

from __future__ import annotations

import argparse
import fnmatch
import json
import os
import re
import sys
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Any


SCHEMA = "ee.rch.compile_blocker_route.v1"
PREFLIGHT_SCHEMA = "ee.swarm_compile_blockers.v1"
LOCAL_REMOTE_ROOT = "/data/projects/eidetic_engine_cli/"
UPSTREAM_ROOT = "/data/projects/"


@dataclass(frozen=True)
class FirstError:
    file: str | None
    line: int | None
    code: str | None
    message: str


@dataclass(frozen=True)
class Reservation:
    agent: str
    pattern: str
    expires_ts: str | None


@dataclass(frozen=True)
class DirtyPath:
    path: str
    state: str


@dataclass(frozen=True)
class VerifierEvidence:
    file: str | None
    line: int | None
    command: str | None
    command_hash: str | None
    status: str | None
    degraded_codes: tuple[str, ...]


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Route a failed RCH transcript to an owner decision."
    )
    parser.add_argument("transcript", nargs="?", help="Transcript path, default stdin")
    parser.add_argument(
        "--preflight",
        action="store_true",
        help="Emit a pre-RCH shared-checkout compile-blocker recommendation",
    )
    parser.add_argument("--command", default="", help="Verifier command text")
    parser.add_argument("--bead-id", default="", help="Current bead id")
    parser.add_argument("--agent-name", default="", help="Current agent name")
    parser.add_argument("--reservations", help="Reservation JSON path")
    parser.add_argument("--dirty-paths", help="Dirty-path snapshot JSON path")
    parser.add_argument("--verifier-evidence", help="Recent ee.rch.verify.v1 JSON path")
    parser.add_argument("--now", help="UTC timestamp for deterministic expiry checks")
    parser.add_argument("--json", action="store_true", help="Accepted for symmetry")
    return parser.parse_args()


def read_text(path: str | None) -> str:
    if not path or path == "-":
        return sys.stdin.read()
    return Path(path).read_text(encoding="utf-8")


def load_json(path: str | None) -> Any:
    if not path:
        return None
    return json.loads(Path(path).read_text(encoding="utf-8"))


def parse_now(raw: str | None) -> datetime:
    if not raw:
        return datetime.now(timezone.utc)
    normalized = raw.replace("Z", "+00:00")
    parsed = datetime.fromisoformat(normalized)
    if parsed.tzinfo is None:
        return parsed.replace(tzinfo=timezone.utc)
    return parsed.astimezone(timezone.utc)


def normalize_file(path: str | None) -> str | None:
    if not path:
        return None
    if path.startswith(LOCAL_REMOTE_ROOT):
        return path[len(LOCAL_REMOTE_ROOT) :]
    return path


def is_compile_critical(path: str) -> bool:
    if path == "Cargo.toml" or path == "Cargo.lock":
        return True
    if path.endswith(".rs") or path.endswith("/Cargo.toml") or path.endswith("/build.rs"):
        return True
    return False


def first_error(transcript: str) -> FirstError:
    lines = transcript.splitlines()
    pending_code: str | None = None
    pending_message = ""
    for index, line in enumerate(lines):
        match = re.search(r"\berror(?:\[([A-Z]\d{4})\])?:\s*(.+)$", line)
        if match:
            pending_code = match.group(1)
            pending_message = match.group(2).strip()
        loc = re.search(r"-->\s+(.+?):(\d+):\d+", line)
        if loc:
            return FirstError(
                file=normalize_file(loc.group(1).strip()),
                line=int(loc.group(2)),
                code=pending_code,
                message=pending_message or "Rust compiler error",
            )
        if pending_message and index + 1 < len(lines):
            next_loc = re.search(r"-->\s+(.+?):(\d+):\d+", lines[index + 1])
            if next_loc:
                return FirstError(
                    file=normalize_file(next_loc.group(1).strip()),
                    line=int(next_loc.group(2)),
                    code=pending_code,
                    message=pending_message,
                )
    for line in lines:
        if "linking with" in line and "failed" in line:
            return FirstError(None, None, None, line.strip())
        if "failed to run custom build command" in line:
            return FirstError(None, None, None, line.strip())
    return FirstError(None, None, None, "No compiler diagnostic found")


def extract_reservations(payload: Any, now: datetime) -> list[Reservation]:
    reservations: list[Reservation] = []

    def add_holder(holder: dict[str, Any], fallback_pattern: str | None = None) -> None:
        agent = str(holder.get("agent") or holder.get("agent_name") or "")
        pattern = str(
            holder.get("path_pattern")
            or holder.get("path")
            or fallback_pattern
            or ""
        )
        expires_ts = holder.get("expires_ts")
        if not agent or not pattern:
            return
        if expires_ts:
            try:
                expires = datetime.fromisoformat(str(expires_ts).replace("Z", "+00:00"))
                if expires.tzinfo is None:
                    expires = expires.replace(tzinfo=timezone.utc)
                if expires.astimezone(timezone.utc) <= now:
                    return
            except ValueError:
                pass
        reservations.append(Reservation(agent=agent, pattern=pattern, expires_ts=expires_ts))

    if isinstance(payload, list):
        for item in payload:
            if isinstance(item, dict):
                add_holder(item)
    elif isinstance(payload, dict):
        for item in payload.get("granted", []) or []:
            if isinstance(item, dict):
                add_holder(item)
        for conflict in payload.get("conflicts", []) or []:
            if not isinstance(conflict, dict):
                continue
            fallback = conflict.get("path")
            for holder in conflict.get("holders", []) or []:
                if isinstance(holder, dict):
                    add_holder(holder, fallback)
    return reservations


def extract_dirty_paths(payload: Any) -> list[DirtyPath]:
    paths: list[DirtyPath] = []

    def add_path(item: Any) -> None:
        if isinstance(item, str):
            path = normalize_file(item)
            if path:
                paths.append(DirtyPath(path=path, state="dirty"))
            return
        if not isinstance(item, dict):
            return
        path = normalize_file(
            item.get("path")
            or item.get("file")
            or item.get("file_path")
            or item.get("relative_path")
        )
        if not path:
            return
        state = str(
            item.get("state")
            or item.get("git_state")
            or item.get("entry_kind")
            or item.get("bucket")
            or "dirty"
        )
        paths.append(DirtyPath(path=path, state=state))

    def walk(value: Any) -> None:
        if isinstance(value, list):
            for item in value:
                add_path(item)
        elif isinstance(value, dict):
            add_path(value)
            for key in (
                "dirtyPaths",
                "dirty_paths",
                "dirty_paths_sample",
                "paths",
                "entries",
                "classificationRows",
                "classification_rows",
            ):
                if key in value:
                    walk(value[key])
            source_state = value.get("source_state")
            if isinstance(source_state, dict):
                walk(source_state)

    walk(payload)
    dedup: dict[str, DirtyPath] = {}
    for path in paths:
        if is_compile_critical(path.path):
            dedup.setdefault(path.path, path)
    return [dedup[path] for path in sorted(dedup)]


def extract_verifier_evidence(payload: Any) -> list[VerifierEvidence]:
    evidence: list[VerifierEvidence] = []

    def strings(value: Any) -> tuple[str, ...]:
        if not isinstance(value, list):
            return ()
        return tuple(str(item) for item in value if isinstance(item, str))

    def add_item(item: Any) -> None:
        if not isinstance(item, dict):
            return
        first = item.get("first_error") or item.get("firstError") or {}
        first_file = None
        first_line = None
        if isinstance(first, dict):
            first_file = first.get("file") or first.get("path")
            first_line = first.get("line")
        first_file = normalize_file(
            item.get("first_error_file")
            or item.get("firstErrorFile")
            or item.get("first_error_path")
            or first_file
        )
        if not first_file:
            return
        raw_line = item.get("first_error_line") or item.get("firstErrorLine") or first_line
        line = int(raw_line) if isinstance(raw_line, int) or str(raw_line).isdigit() else None
        degraded_codes = strings(item.get("degraded_codes") or item.get("degradedCodes"))
        status = item.get("status") or item.get("result")
        failure_like = (
            status in {"remote_failure", "failed", "failure"}
            or "rch_verify_remote_command_failed" in degraded_codes
        )
        if not failure_like:
            return
        evidence.append(
            VerifierEvidence(
                file=first_file,
                line=line,
                command=item.get("command_text") or item.get("command"),
                command_hash=item.get("command_hash") or item.get("commandHash"),
                status=str(status) if status is not None else None,
                degraded_codes=degraded_codes,
            )
        )

    def walk(value: Any) -> None:
        if isinstance(value, list):
            for item in value:
                add_item(item)
        elif isinstance(value, dict):
            add_item(value)
            for key in ("runs", "proofs", "entries", "ledger", "items"):
                if key in value:
                    walk(value[key])

    walk(payload)
    evidence.sort(
        key=lambda item: (
            item.file or "",
            item.command_hash or "",
            item.command or "",
            item.line if item.line is not None else -1,
        )
    )
    return evidence


def matching_owner(path: str | None, reservations: list[Reservation]) -> Reservation | None:
    if not path:
        return None
    candidates = [path]
    if path.startswith(LOCAL_REMOTE_ROOT):
        candidates.append(path[len(LOCAL_REMOTE_ROOT) :])
    for reservation in reservations:
        pattern = reservation.pattern
        for candidate in candidates:
            if candidate == pattern or fnmatch.fnmatchcase(candidate, pattern):
                return reservation
    return None


def decision_for(
    transcript: str,
    error: FirstError,
    owner: Reservation | None,
    agent_name: str,
) -> str:
    if "RCH-E327" in transcript or "remote required; refusing local fallback" in transcript:
        return "environment_failure"
    if error.file and error.file.startswith(UPSTREAM_ROOT) and not error.file.startswith(
        LOCAL_REMOTE_ROOT
    ):
        return "upstream_dependency_failure"
    if owner and owner.agent != agent_name:
        return "reserved_by_other_agent"
    if owner and owner.agent == agent_name:
        return "self_fix_allowed"
    if error.file:
        return "no_owner_found"
    return "environment_failure" if error.message == "No compiler diagnostic found" else "no_owner_found"


def markdown_summary(
    command: str,
    bead_id: str,
    error: FirstError,
    owner: Reservation | None,
    decision: str,
) -> str:
    lines = [f"RCH compile blocker route => `{decision}`."]
    if bead_id:
        lines.append(f"- bead_id: `{bead_id}`")
    if command:
        lines.append(f"- command: `{command}`")
    lines.append(f"- first_error_file: `{error.file or 'unknown'}`")
    lines.append(f"- first_error_line: `{error.line if error.line is not None else 'unknown'}`")
    lines.append(f"- first_error_code: `{error.code or 'unknown'}`")
    lines.append(f"- message: `{error.message}`")
    if owner:
        lines.append(f"- owner_agent: `{owner.agent}`")
        lines.append(f"- owner_pattern: `{owner.pattern}`")
    else:
        lines.append("- owner_agent: `unknown`")
    return "\n".join(lines)


def recent_first_error_payload(evidence: VerifierEvidence | None) -> dict[str, Any] | None:
    if not evidence:
        return None
    return {
        "file": evidence.file,
        "line": evidence.line,
        "command": evidence.command,
        "commandHash": evidence.command_hash,
        "status": evidence.status,
        "degradedCodes": list(evidence.degraded_codes),
    }


def blocker_mail_template(
    bead_id: str,
    command: str,
    path: str,
    owner: Reservation | None,
    evidence: VerifierEvidence | None,
) -> str | None:
    if not owner:
        return None
    lines = [
        f"[compile-blocker] `{path}` is blocking an RCH proof I was about to launch.",
    ]
    if bead_id:
        lines.append(f"- bead_id: `{bead_id}`")
    if command:
        lines.append(f"- requested_command: `{command}`")
    lines.append(f"- owner_pattern: `{owner.pattern}`")
    if evidence and evidence.line is not None:
        lines.append(f"- recent_first_error: `{path}:{evidence.line}`")
    elif evidence:
        lines.append(f"- recent_first_error: `{path}`")
    lines.append("Can you send a status or a no-edit proof when your slice is ready?")
    return "\n".join(lines)


def compile_blocker_preflight(args: argparse.Namespace) -> dict[str, Any]:
    degraded_codes: list[str] = []
    reservations_payload = load_json(args.reservations)
    if args.reservations is None:
        degraded_codes.append("agent_mail_reservations_unavailable")
    reservations = extract_reservations(reservations_payload, parse_now(args.now))
    dirty_paths = extract_dirty_paths(load_json(args.dirty_paths))
    verifier_evidence = extract_verifier_evidence(load_json(args.verifier_evidence))
    dirty_by_path = {item.path: item for item in dirty_paths}
    evidence_by_path: dict[str, VerifierEvidence] = {}
    for item in verifier_evidence:
        if item.file in dirty_by_path:
            evidence_by_path.setdefault(str(item.file), item)

    compile_blockers: list[dict[str, Any]] = []
    for dirty in dirty_paths:
        owner = matching_owner(dirty.path, reservations)
        evidence = evidence_by_path.get(dirty.path)
        if owner and owner.agent != args.agent_name:
            severity = "high"
            reason = "dirty_compile_critical_path_reserved_by_other_agent"
            action = "message_owner_before_rch"
        elif evidence:
            severity = "high"
            reason = "recent_rch_first_error_matches_dirty_path"
            action = "fix_or_coordinate_before_rch"
        else:
            severity = "medium"
            reason = "dirty_compile_critical_path_without_recent_first_error"
            action = "prefer_static_or_non_cargo_work_until_compile_health_is_known"
        template = blocker_mail_template(args.bead_id, args.command, dirty.path, owner, evidence)
        compile_blockers.append(
            {
                "path": dirty.path,
                "severity": severity,
                "reason": reason,
                "dirtyState": dirty.state,
                "ownerAgent": owner.agent if owner else None,
                "ownerPattern": owner.pattern if owner else None,
                "reservationExpiresTs": owner.expires_ts if owner else None,
                "recentFirstError": recent_first_error_payload(evidence),
                "suggestedNextAction": action,
                "mailTemplate": template,
            }
        )

    severity_rank = {"high": 0, "medium": 1, "low": 2}
    compile_blockers.sort(
        key=lambda item: (
            severity_rank.get(str(item["severity"]), 9),
            0 if item["ownerAgent"] else 1,
            str(item["path"]),
        )
    )
    if any(item["severity"] == "high" for item in compile_blockers):
        safe_to_launch: bool | str = False
    elif compile_blockers:
        safe_to_launch = "unknown"
    else:
        safe_to_launch = True

    alternatives: list[dict[str, str]] = []
    if safe_to_launch is not True:
        alternatives.extend(
            [
                {
                    "kind": "static_check",
                    "message": "Run non-Cargo static checks or JSON/schema validation for your files.",
                },
                {
                    "kind": "coordination",
                    "message": "Coordinate with the owner before spending an RCH slot.",
                },
            ]
        )
    else:
        alternatives.append(
            {
                "kind": "rch",
                "message": "No dirty compile-critical blocker was found in the supplied snapshots.",
            }
        )

    mail_template = next(
        (
            item["mailTemplate"]
            for item in compile_blockers
            if item.get("mailTemplate")
        ),
        None,
    )
    degraded_codes.sort()
    return {
        "schema": PREFLIGHT_SCHEMA,
        "success": True,
        "command": args.command or None,
        "beadId": args.bead_id or None,
        "safeToLaunchRch": safe_to_launch,
        "compileBlockers": compile_blockers,
        "recommendedAlternativeWork": alternatives,
        "mailTemplate": mail_template,
        "degradedCodes": degraded_codes,
    }


def main() -> int:
    args = parse_args()
    if args.preflight:
        print(
            json.dumps(
                compile_blocker_preflight(args),
                sort_keys=True,
                separators=(",", ":"),
            )
        )
        return 0

    transcript = read_text(args.transcript)
    reservations = extract_reservations(load_json(args.reservations), parse_now(args.now))
    error = first_error(transcript)
    owner = matching_owner(error.file, reservations)
    decision = decision_for(transcript, error, owner, args.agent_name)
    summary = markdown_summary(args.command, args.bead_id, error, owner, decision)
    payload = {
        "schema": SCHEMA,
        "success": True,
        "command": args.command or None,
        "bead_id": args.bead_id or None,
        "routing_decision": decision,
        "first_error": {
            "file": error.file,
            "line": error.line,
            "code": error.code,
            "message": error.message,
        },
        "owner_agent": owner.agent if owner else None,
        "owner_pattern": owner.pattern if owner else None,
        "summary_markdown": summary,
        "degraded_codes": [],
    }
    print(json.dumps(payload, sort_keys=True, separators=(",", ":")))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
