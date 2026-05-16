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


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Route a failed RCH transcript to an owner decision."
    )
    parser.add_argument("transcript", nargs="?", help="Transcript path, default stdin")
    parser.add_argument("--command", default="", help="Verifier command text")
    parser.add_argument("--bead-id", default="", help="Current bead id")
    parser.add_argument("--agent-name", default="", help="Current agent name")
    parser.add_argument("--reservations", help="Reservation JSON path")
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


def main() -> int:
    args = parse_args()
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
