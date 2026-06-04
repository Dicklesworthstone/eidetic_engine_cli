#!/usr/bin/env bash
# Emit a redacted, read-only Agent Mail snapshot for ee swarm coordination.

set -euo pipefail

exec "${PYTHON:-python3}" - "$@" <<'PY'
from __future__ import annotations

import argparse
import json
import os
import re
import shlex
import shutil
import subprocess
import sys
from datetime import datetime, timezone
from pathlib import Path
from typing import Any


REDACTION_STATUS = "paths_counts_subjects_only_no_content"
DEFAULT_TIMEOUT_SEC = 5.0
DEFAULT_INBOX_LIMIT = 20
DEFAULT_THREAD_LIMIT = 20
SECRET_PATTERNS = [
    (re.compile(r"gh[pousr]_[A-Za-z0-9_]{20,}"), "[REDACTED:github_token]"),
    (re.compile(r"sk-[A-Za-z0-9]{20,}"), "[REDACTED:secret]"),
    (re.compile(r"(?i)(api[_-]?key|token|secret|password)=\S+"), r"\1=[REDACTED:secret]"),
]
PATH_PATTERN = re.compile(
    r"(?<![A-Za-z0-9_])/(Users|Volumes|data|tmp|private/tmp|var/folders|private/var/folders)(?:/[^\s,;:]+)?"
)


def utc_now() -> str:
    return datetime.now(timezone.utc).replace(microsecond=0).isoformat().replace("+00:00", "Z")


def redact_secrets(value: Any) -> str | None:
    if value is None:
        return None
    text = str(value)
    for pattern, replacement in SECRET_PATTERNS:
        text = pattern.sub(replacement, text)
    return text


def redact_text(value: Any) -> str | None:
    text = redact_secrets(value)
    if text is None:
        return None
    text = PATH_PATTERN.sub("[REDACTED:path]", text)
    return text


def safe_workspace_path(value: Any, project: Path) -> str | None:
    if value is None:
        return None
    text = str(value)
    text = redact_secrets(text) or ""
    if not text:
        return None
    if text.startswith("[REDACTED:"):
        return text
    path = Path(text)
    if path.is_absolute():
        path = path.resolve()
        try:
            relative = path.relative_to(project).as_posix()
        except ValueError:
            return "[REDACTED:absolute_path]"
        return "." if relative == "." else relative
    if ".." in path.parts:
        return "[REDACTED:relative_parent_path]"
    return text


def pick_string(item: dict[str, Any], keys: list[str]) -> str | None:
    for key in keys:
        value = item.get(key)
        if isinstance(value, str) and value:
            return value
        if value is not None and not isinstance(value, (dict, list, bool)):
            return str(value)
    return None


def pick_bool(item: dict[str, Any], keys: list[str], default: bool = False) -> bool:
    for key in keys:
        value = item.get(key)
        if isinstance(value, bool):
            return value
        if isinstance(value, str):
            normalized = value.strip().lower()
            if normalized in {"true", "yes", "1", "exclusive"}:
                return True
            if normalized in {"false", "no", "0", "shared"}:
                return False
    return default


def list_from_json(value: Any, keys: list[str]) -> list[Any]:
    if isinstance(value, list):
        return value
    if isinstance(value, dict):
        for key in keys:
            nested = value.get(key)
            if isinstance(nested, list):
                return nested
        data = value.get("data")
        if isinstance(data, dict):
            for key in keys:
                nested = data.get(key)
                if isinstance(nested, list):
                    return nested
    return []


def load_json(text: str) -> Any:
    try:
        return json.loads(text)
    except json.JSONDecodeError:
        return None


def safe_command_arg(value: Any, project: Path) -> str:
    text = redact_secrets(value) or ""
    if not text:
        return ""
    path = Path(text)
    if path.is_absolute():
        path = path.resolve()
        try:
            relative = path.relative_to(project).as_posix()
        except ValueError:
            return "[REDACTED:absolute_path]"
        return "<workspace>" if relative in {"", "."} else f"<workspace>/{relative}"
    return redact_text(text) or ""


def command_display(argv: list[str], project: Path) -> str:
    return " ".join(shlex.quote(safe_command_arg(part, project)) for part in argv)


def run_json_command(argv: list[str], timeout_sec: float) -> dict[str, Any]:
    executable = argv[0]
    if shutil.which(executable) is None:
        return {
            "argv": argv,
            "ok": False,
            "exit_code": 127,
            "timed_out": False,
            "json": None,
            "error_class": "command_unavailable",
        }
    try:
        completed = subprocess.run(
            argv,
            check=False,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            timeout=timeout_sec,
        )
    except subprocess.TimeoutExpired:
        return {
            "argv": argv,
            "ok": False,
            "exit_code": None,
            "timed_out": True,
            "json": None,
            "error_class": "timeout",
        }

    parsed = load_json(completed.stdout)
    ok = completed.returncode == 0 and parsed is not None
    return {
        "argv": argv,
        "ok": ok,
        "exit_code": completed.returncode,
        "timed_out": False,
        "json": parsed,
        "error_class": None if ok else ("invalid_json" if completed.returncode == 0 else "command_failed"),
    }


def normalize_agents(value: Any) -> list[dict[str, Any]]:
    rows = []
    for item in list_from_json(value, ["agents", "result", "items"]):
        if not isinstance(item, dict):
            continue
        name = pick_string(item, ["name", "agent_name", "agent", "mailbox"])
        if not name:
            continue
        row: dict[str, Any] = {"name": redact_text(name)}
        last_active = pick_string(item, ["last_active_at", "lastActiveAt", "last_active_ts", "lastActiveTs"])
        if last_active:
            row["last_active_ts"] = redact_text(last_active)
        rows.append(row)
    return sorted(rows, key=lambda row: (row.get("name") or "", row.get("last_active_ts") or ""))


def normalize_reservations(value: Any, project: Path) -> list[dict[str, Any]]:
    rows = []
    for item in list_from_json(value, ["all_active", "active", "reservations", "file_reservations", "items"]):
        if not isinstance(item, dict):
            continue
        path_pattern = safe_workspace_path(
            pick_string(item, ["path_pattern", "path", "pattern"]),
            project,
        )
        holder = pick_string(item, ["holder", "agent_name", "agent", "owner"])
        if not path_pattern or not holder:
            continue
        row: dict[str, Any] = {
            "path_pattern": path_pattern,
            "holder": redact_text(holder),
            "exclusive": pick_bool(item, ["exclusive"], default=False),
        }
        expires = pick_string(item, ["expires_ts", "expires_at", "expires", "expiresAt"])
        if expires:
            row["expires_ts"] = redact_text(expires)
        rows.append(row)
    return sorted(
        rows,
        key=lambda row: (
            row.get("path_pattern") or "",
            row.get("holder") or "",
            bool(row.get("exclusive")),
            row.get("expires_ts") or "",
        ),
    )


def normalize_inbox(value: Any, agent: str) -> tuple[list[dict[str, Any]], list[dict[str, Any]]]:
    messages = [
        item for item in list_from_json(value, ["inbox", "messages", "result", "items"])
        if isinstance(item, dict)
    ]
    ack_required = sum(1 for item in messages if bool(item.get("ack_required") or item.get("ackRequired")))
    inbox = [{
        "mailbox": redact_text(agent),
        "unread_count": len(messages),
        "ack_required_count": ack_required,
    }]
    return inbox, messages


def normalize_threads(messages: list[dict[str, Any]], limit: int) -> list[dict[str, Any]]:
    by_thread: dict[str, dict[str, Any]] = {}
    for item in messages:
        raw_thread_id = pick_string(item, ["thread_id", "threadId", "id"])
        if not raw_thread_id:
            continue
        thread_id = redact_text(raw_thread_id) or ""
        if not thread_id:
            continue
        existing = by_thread.setdefault(
            thread_id,
            {
                "thread_id": thread_id,
                "message_count": 0,
                "last_activity_at": None,
                "subject": None,
            },
        )
        existing["message_count"] += 1
        created = pick_string(item, ["created_ts", "created_at", "createdAt", "last_activity_at", "lastActivityAt"])
        if created and (existing["last_activity_at"] is None or created > existing["last_activity_at"]):
            existing["last_activity_at"] = redact_text(created)
        if existing["subject"] is None:
            subject = pick_string(item, ["subject"])
            if subject:
                existing["subject"] = redact_text(subject)

    rows = []
    for item in by_thread.values():
        row = {
            "thread_id": item["thread_id"],
            "message_count": item["message_count"],
        }
        if item.get("subject"):
            row["subject"] = item["subject"]
        if item.get("last_activity_at"):
            row["last_activity_at"] = item["last_activity_at"]
        rows.append(row)
    rows.sort(key=lambda row: (row.get("last_activity_at") or "", row.get("thread_id") or ""), reverse=True)
    return rows[:limit]


def command_status(command: dict[str, Any], project: Path) -> dict[str, Any]:
    return {
        "command": command_display(command["argv"], project),
        "ok": command["ok"],
        "exit_code": command["exit_code"],
        "timed_out": command["timed_out"],
        "error_class": command["error_class"],
    }


def degraded_entries(commands: list[dict[str, Any]], project: Path) -> list[dict[str, Any]]:
    entries = []
    for command in commands:
        if command["ok"]:
            continue
        entries.append(
            {
                "code": "agent_mail_snapshot_source_unavailable",
                "severity": "warning",
                "source": "agent_mail",
                "command": command_display(command["argv"], project),
                "error_class": command["error_class"],
                "exit_code": command["exit_code"],
                "timed_out": command["timed_out"],
            }
        )
    return entries


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Emit a redacted read-only Agent Mail snapshot for ee swarm brief.",
    )
    parser.add_argument("--project", default=os.environ.get("AGENT_MAIL_PROJECT") or os.getcwd())
    parser.add_argument("--agent", default=os.environ.get("AGENT_MAIL_AGENT") or os.environ.get("AGENT_NAME"))
    parser.add_argument("--am-bin", default=os.environ.get("AGENT_MAIL_AM_BIN", "am"))
    parser.add_argument("--inbox-limit", type=int, default=DEFAULT_INBOX_LIMIT)
    parser.add_argument("--thread-limit", type=int, default=DEFAULT_THREAD_LIMIT)
    parser.add_argument("--timeout-sec", type=float, default=DEFAULT_TIMEOUT_SEC)
    parser.add_argument("--output", help="Write snapshot JSON to this path instead of stdout.")
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    project = Path(args.project).resolve()
    agent = args.agent
    if not agent:
        print("agent_mail_snapshot: --agent or AGENT_NAME is required", file=sys.stderr)
        return 2
    if args.inbox_limit < 0 or args.thread_limit < 0:
        print("agent_mail_snapshot: limits must be non-negative", file=sys.stderr)
        return 2
    if args.timeout_sec <= 0:
        print("agent_mail_snapshot: --timeout-sec must be positive", file=sys.stderr)
        return 2

    am_bin = args.am_bin
    commands = [
        run_json_command([am_bin, "agents", "list", "--project", str(project), "--json"], args.timeout_sec),
        run_json_command([am_bin, "robot", "reservations", "--project", str(project), "--all", "--format", "json"], args.timeout_sec),
        run_json_command([am_bin, "mail", "inbox", "--project", str(project), "--agent", agent, "--limit", str(args.inbox_limit), "--json"], args.timeout_sec),
    ]
    agents_cmd, reservations_cmd, inbox_cmd = commands

    agents = normalize_agents(agents_cmd["json"]) if agents_cmd["ok"] else []
    reservations = normalize_reservations(reservations_cmd["json"], project) if reservations_cmd["ok"] else []
    if inbox_cmd["ok"]:
        inbox, messages = normalize_inbox(inbox_cmd["json"], agent)
        threads = normalize_threads(messages, args.thread_limit)
    else:
        inbox = []
        threads = []

    degraded = degraded_entries(commands, project)
    fallback_active = bool(degraded)
    output = {
        "generated_at": utc_now(),
        "project_key": "<workspace>",
        "redaction_status": REDACTION_STATUS,
        "producer_status": "degraded" if fallback_active else "ok",
        "source_commands": [command_display(command["argv"], project) for command in commands],
        "command_statuses": [command_status(command, project) for command in commands],
        "fallback_active": fallback_active,
        "am_agents_list_ok": agents_cmd["ok"],
        "degraded": degraded,
        "file_reservations": reservations,
        "agents": agents,
        "inbox": inbox,
        "threads": threads,
    }

    rendered = json.dumps(output, sort_keys=True, separators=(",", ":")) + "\n"
    if args.output:
        Path(args.output).write_text(rendered, encoding="utf-8")
    else:
        sys.stdout.write(rendered)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
PY
