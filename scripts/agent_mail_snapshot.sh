#!/usr/bin/env bash
# Emit a redacted, read-only Agent Mail snapshot for ee swarm coordination.

set -euo pipefail

exec "${PYTHON:-python3}" - "$@" <<'PY'
from __future__ import annotations

import argparse
import hashlib
import http.client
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
AGENT_MAIL_SNAPSHOT_SCHEMA = "ee.agent_mail.snapshot.v1"
DEFAULT_TIMEOUT_SEC = 5.0
DEFAULT_INBOX_LIMIT = 20
DEFAULT_THREAD_LIMIT = 20
MAX_WIRE_COUNT = (1 << 64) - 1
HEALTH_HOST = "127.0.0.1"
HEALTH_PORT = 8765
HEALTH_PATH = "/health"
DURABILITY_PATH = "/health/durability"
SECRET_PATTERNS = [
    (re.compile(r"gh[pousr]_[A-Za-z0-9_]{20,}"), "[REDACTED:github_token]"),
    (re.compile(r"sk-[A-Za-z0-9]{20,}"), "[REDACTED:secret]"),
    (re.compile(r"(?i)(api[_-]?key|token|secret|password)=\S+"), r"\1=[REDACTED:secret]"),
]
PATH_PATTERN = re.compile(
    r"(?<![A-Za-z0-9_])/(Users|Volumes|data|tmp|private/tmp|var/folders|private/var/folders)(?:/[^\s,;:]+)?"
)
RFC3339_PATTERN = re.compile(
    r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:\d{2})$"
)


def utc_now() -> str:
    return datetime.now(timezone.utc).replace(microsecond=0).isoformat().replace("+00:00", "Z")


def rfc3339_timestamp_valid(value: Any) -> bool:
    if not isinstance(value, str) or RFC3339_PATTERN.fullmatch(value) is None:
        return False
    candidate = f"{value[:-1]}+00:00" if value.endswith("Z") else value
    try:
        parsed = datetime.fromisoformat(candidate)
    except ValueError:
        return False
    return parsed.tzinfo is not None and parsed.utcoffset() is not None


def optional_timestamp_aliases_valid(item: dict[str, Any], keys: list[str]) -> bool:
    return all(key not in item or rfc3339_timestamp_valid(item[key]) for key in keys)


def normalize_workspace_identity(value: str, *, windows: bool | None = None) -> str:
    try:
        value.encode("utf-8", errors="strict")
    except UnicodeEncodeError as error:
        raise ValueError("canonical workspace path must be valid UTF-8") from error
    if windows is None:
        windows = os.name == "nt"
    if not windows:
        return value

    normalized = value.replace("\\", "/")
    folded = normalized.upper()
    if folded.startswith("//?/UNC/"):
        normalized = f"//{normalized[8:]}"
    elif folded.startswith("//?/"):
        normalized = normalized[4:]
    if len(normalized) >= 2 and normalized[0].isalpha() and normalized[1] == ":":
        normalized = normalized[0].lower() + normalized[1:]
    return normalized


def physical_workspace_path(value: Path) -> Path:
    """Resolve symlinks and recover the filesystem's canonical path spelling."""
    candidate = value.expanduser().resolve()
    if os.name == "nt":
        return candidate

    previous_directory = os.open(".", os.O_RDONLY)
    try:
        os.chdir(candidate)
        return Path(os.getcwd())
    finally:
        os.fchdir(previous_directory)
        os.close(previous_directory)


def workspace_project_key(project: Path) -> str:
    identity = normalize_workspace_identity(str(project))
    digest = hashlib.sha256(identity.encode("utf-8")).hexdigest()
    return f"sha256:{digest}"


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


def recognized_list_from_json(value: Any, keys: list[str]) -> list[Any] | None:
    """Return a recognized collection while preserving invalid-shape evidence."""
    if isinstance(value, list):
        return value
    if not isinstance(value, dict):
        return None

    collections: list[list[Any]] = []
    for key in keys:
        if key not in value:
            continue
        nested = value.get(key)
        if not isinstance(nested, list):
            return None
        collections.append(nested)
    data = value.get("data")
    if data is not None and not isinstance(data, dict):
        return None
    if isinstance(data, dict):
        for key in keys:
            if key not in data:
                continue
            nested = data.get(key)
            if not isinstance(nested, list):
                return None
            collections.append(nested)

    if not collections:
        return None
    first = collections[0]
    if any(collection != first for collection in collections[1:]):
        return None
    return first


def canonical_string_alias(item: dict[str, Any], keys: list[str]) -> str | None:
    values = [item.get(key) for key in keys if key in item]
    if not values or any(not isinstance(value, str) or not value.strip() for value in values):
        return None
    first = values[0]
    if any(value != first for value in values[1:]):
        return None
    return first


def has_recognized_bool(item: dict[str, Any], keys: list[str]) -> bool:
    for key in keys:
        if key not in item:
            continue
        value = item.get(key)
        if isinstance(value, bool):
            return True
        if isinstance(value, str) and value.strip().lower() in {
            "true",
            "yes",
            "1",
            "exclusive",
            "false",
            "no",
            "0",
            "shared",
        }:
            return True
        return False
    return False


def canonical_thread_identifier(item: dict[str, Any]) -> str | None:
    thread_id = canonical_string_alias(item, ["thread_id", "threadId"])
    if "thread_id" in item or "threadId" in item:
        return thread_id
    message_id = item.get("id")
    if isinstance(message_id, str) and message_id.strip():
        return message_id
    if type(message_id) is int and message_id >= 0:
        return str(message_id)
    return None


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


def first_symlink_component(path: Path) -> Path | None:
    path = path if path.is_absolute() else Path.cwd() / path
    current = Path(path.anchor) if path.is_absolute() else Path.cwd()
    parts = path.parts[1:] if path.is_absolute() else path.parts
    for part in parts:
        current = current / part
        if current.is_symlink():
            return current
        if not current.exists():
            return None
    return None


def validate_output_path(label: str, value: str | None) -> int:
    if not value:
        return 0
    path = Path(value)
    symlink = first_symlink_component(path)
    if symlink is None:
        return 0
    resolved = path.resolve(strict=False)
    print(
        (
            f"agent_mail_snapshot: {label} path traverses symlink component "
            f"{symlink}; use a resolved non-symlink path such as {resolved}"
        ),
        file=sys.stderr,
    )
    return 2


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


def run_health_probe(path: str, timeout_sec: float) -> dict[str, Any]:
    argv = ["agent-mail-health", f"http://{HEALTH_HOST}:{HEALTH_PORT}{path}"]
    conn: http.client.HTTPConnection | None = None
    try:
        conn = http.client.HTTPConnection(HEALTH_HOST, HEALTH_PORT, timeout=timeout_sec)
        conn.request("GET", path, headers={"Host": f"{HEALTH_HOST}:{HEALTH_PORT}"})
        response = conn.getresponse()
        status = int(response.status)
        raw = response.read(128 * 1024)
    except TimeoutError:
        return {
            "argv": argv,
            "ok": False,
            "exit_code": None,
            "timed_out": True,
            "json": None,
            "error_class": "timeout",
        }
    except OSError:
        return {
            "argv": argv,
            "ok": False,
            "exit_code": None,
            "timed_out": False,
            "json": None,
            "error_class": "command_failed",
        }
    finally:
        if conn is not None:
            conn.close()

    parsed = load_json(raw.decode("utf-8", "replace"))
    parsed_ok = isinstance(parsed, dict)
    status_ok = status == 200
    ok = parsed_ok and status_ok
    return {
        "argv": argv,
        "ok": ok,
        "exit_code": status,
        "timed_out": False,
        "json": parsed if parsed_ok else None,
        "error_class": None if ok else ("http_status" if parsed_ok else "invalid_json"),
    }


def normalize_agents(value: Any) -> list[dict[str, Any]]:
    rows_by_name: dict[str, dict[str, Any]] = {}
    for item in recognized_list_from_json(value, ["agents", "result", "items"]) or []:
        if not isinstance(item, dict):
            continue
        name = canonical_string_alias(item, ["name", "agent_name", "agent", "mailbox"])
        if not name:
            continue
        row: dict[str, Any] = {"name": redact_text(name)}
        last_active = pick_string(item, ["last_active_at", "lastActiveAt", "last_active_ts", "lastActiveTs"])
        if last_active:
            row["last_active_ts"] = redact_text(last_active)
        previous = rows_by_name.get(row["name"])
        if previous is None or (row.get("last_active_ts") or "") > (previous.get("last_active_ts") or ""):
            rows_by_name[row["name"]] = row
    rows = list(rows_by_name.values())
    return sorted(rows, key=lambda row: (row.get("name") or "", row.get("last_active_ts") or ""))


def normalize_reservations(value: Any, project: Path) -> list[dict[str, Any]]:
    rows = []
    for item in recognized_list_from_json(
        value,
        ["all_active", "active", "reservations", "file_reservations", "items"],
    ) or []:
        if not isinstance(item, dict):
            continue
        path_pattern = safe_workspace_path(
            canonical_string_alias(item, ["path_pattern", "path", "pattern"]),
            project,
        )
        holder = canonical_string_alias(item, ["holder", "agent_name", "agent", "owner"])
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


def normalize_inbox_messages(value: Any) -> list[dict[str, Any]]:
    return [
        item
        for item in recognized_list_from_json(
            value,
            ["inbox", "messages", "result", "items"],
        ) or []
        if isinstance(item, dict)
    ]


def normalize_status_counts(value: Any, agent: str) -> list[dict[str, Any]]:
    if not isinstance(value, dict):
        return []
    unread = value.get("unread")
    ack_required = value.get("ack_required")
    if type(unread) is not int or not 0 <= unread <= MAX_WIRE_COUNT:
        return []
    if type(ack_required) is not int or not 0 <= ack_required <= MAX_WIRE_COUNT:
        return []
    return [{
        "mailbox": redact_text(agent),
        "unread_count": unread,
        "ack_required_count": ack_required,
    }]


def normalize_threads(messages: list[dict[str, Any]], limit: int) -> list[dict[str, Any]]:
    by_thread: dict[str, dict[str, Any]] = {}
    for item in messages:
        raw_thread_id = canonical_thread_identifier(item)
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


def bounded_health_level(value: Any) -> str | None:
    if not isinstance(value, str):
        return None
    normalized = value.strip().lower()
    if normalized in {"green", "yellow", "red"}:
        return normalized
    return None


def bounded_service_health(value: Any) -> str | None:
    if not isinstance(value, str):
        return None
    normalized = value.strip().lower()
    if normalized in {"ok", "ready", "healthy"}:
        return "green"
    if normalized in {"degraded", "warning"}:
        return "yellow"
    if normalized in {"blocked", "error", "failed", "internal_error", "unhealthy"}:
        return "red"
    return None


def bounded_semantic_readiness_status(value: Any) -> str | None:
    if not isinstance(value, str):
        return None
    normalized = value.strip().lower()
    if normalized == "ok":
        return "pass"
    if normalized in {"pass", "fail", "unknown"}:
        return normalized
    return None


def semantic_readiness_reason_class(value: Any) -> str:
    if not isinstance(value, str):
        return "unknown"
    normalized = value.lower()
    if (
        normalized == "malformed_sqlite"
        or ("sqlite" in normalized and "malformed" in normalized)
        or "database disk image is malformed" in normalized
    ):
        return "malformed_sqlite"
    if (
        normalized == "archive_corruption"
        or (
            "archive" in normalized
            and ("corrupt" in normalized or "parse" in normalized or "jsonl" in normalized)
        )
    ):
        return "archive_corruption"
    if normalized == "index_rebuild_required" or (
        "index" in normalized and ("rebuild" in normalized or "missing" in normalized or "stale" in normalized)
    ):
        return "index_rebuild_required"
    if normalized == "permission_denied" or "permission denied" in normalized:
        return "permission_denied"
    return "unknown"


def bounded_recovery_mode(value: Any) -> str | None:
    if not isinstance(value, str):
        return None
    normalized = value.strip().lower()
    if normalized in {"", "ok", "none", "normal", "clean", "idle", "healthy", "ready"}:
        return "ok"
    if normalized == "corrupt":
        return "corrupt"
    if normalized in {
        "repair",
        "repair_required",
        "repairing",
        "recover",
        "recovering",
        "recovery_required",
        "restore",
        "restoring",
        "reconstruct",
    }:
        return "repair_required"
    return "unknown_recovery"


def recovery_reason_class(mode: str, health: dict[str, Any], recovery: dict[str, Any] | None) -> str:
    if mode == "corrupt":
        return "archive_corruption"
    text_parts: list[str] = []
    if recovery:
        for key in ("reason", "next_action", "nextAction", "detail", "message", "bundle_path", "bundlePath"):
            value = recovery.get(key)
            if isinstance(value, str):
                text_parts.append(value)
    for key in ("detail", "message", "status"):
        value = health.get(key)
        if isinstance(value, str):
            text_parts.append(value)
    normalized = " ".join(text_parts).lower()
    if ("doctor" in normalized and "repair" in normalized) or "restore" in normalized or "reconstruct" in normalized:
        return "storage_recovery_required"
    if "permission denied" in normalized:
        return "permission_denied"
    return "unknown"


def readiness_payload_valid(value: Any) -> bool:
    if not isinstance(value, dict):
        return False
    recognized = False
    if "status" in value:
        if bounded_service_health(value.get("status")) is None:
            return False
        recognized = True
    for key in ("health_level", "healthLevel"):
        if key in value:
            if bounded_health_level(value.get(key)) is None:
                return False
            recognized = True
    for key in ("semantic_readiness", "semanticReadiness"):
        if key not in value:
            continue
        semantic = value.get(key)
        semantic_status = (
            bounded_semantic_readiness_status(semantic.get("status"))
            if isinstance(semantic, dict)
            else bounded_semantic_readiness_status(semantic)
        )
        if semantic_status is None:
            return False
    if "recovery" in value:
        recovery = value.get("recovery")
        if not isinstance(recovery, dict):
            return False
        recovery_values = [recovery.get(key) for key in ("mode", "status") if key in recovery]
        if not recovery_values or any(
            not isinstance(item, str)
            or not item.strip()
            or bounded_recovery_mode(item) is None
            for item in recovery_values
        ):
            return False
    for key in ("durability_state", "durabilityState"):
        if key not in value:
            continue
        durability = value.get(key)
        if not isinstance(durability, str) or not durability.strip():
            return False
        if durability.strip().lower() != "not_probed" and bounded_recovery_mode(durability) is None:
            return False
    return recognized


def durability_payload_valid(value: Any) -> bool:
    if not isinstance(value, dict):
        return False
    state = value.get("durability_state")
    return (
        isinstance(state, str)
        and bool(state.strip())
        and type(value.get("allows_reads")) is bool
        and type(value.get("allows_writes")) is bool
    )


def agents_payload_valid(value: Any) -> bool:
    rows = recognized_list_from_json(value, ["agents", "result", "items"])
    return rows is not None and all(
        isinstance(item, dict)
        and canonical_string_alias(item, ["name", "agent_name", "agent", "mailbox"])
        is not None
        and optional_timestamp_aliases_valid(
            item,
            ["last_active_at", "lastActiveAt", "last_active_ts", "lastActiveTs"],
        )
        for item in rows
    )


def reservations_payload_valid(value: Any) -> bool:
    rows = recognized_list_from_json(
        value,
        ["all_active", "active", "reservations", "file_reservations", "items"],
    )
    return rows is not None and all(
        isinstance(item, dict)
        and canonical_string_alias(item, ["path_pattern", "path", "pattern"])
        is not None
        and canonical_string_alias(item, ["holder", "agent_name", "agent", "owner"])
        is not None
        and has_recognized_bool(item, ["exclusive"])
        and optional_timestamp_aliases_valid(
            item,
            ["expires_ts", "expires_at", "expires", "expiresAt"],
        )
        for item in rows
    )


def inbox_payload_valid(value: Any) -> bool:
    rows = recognized_list_from_json(value, ["inbox", "messages", "result", "items"])
    return rows is not None and all(
        isinstance(item, dict)
        and canonical_thread_identifier(item) is not None
        and optional_timestamp_aliases_valid(
            item,
            ["created_ts", "created_at", "createdAt", "last_activity_at", "lastActivityAt"],
        )
        for item in rows
    )


def status_payload_valid(value: Any) -> bool:
    if not isinstance(value, dict):
        return False
    unread = value.get("unread")
    ack_required = value.get("ack_required")
    return (
        type(unread) is int
        and 0 <= unread <= MAX_WIRE_COUNT
        and type(ack_required) is int
        and 0 <= ack_required <= MAX_WIRE_COUNT
    )


def invalid_response(command: dict[str, Any]) -> dict[str, Any]:
    invalid = dict(command)
    invalid["ok"] = False
    invalid["json"] = None
    invalid["error_class"] = "invalid_response"
    return invalid


def prepare_snapshot_commands(commands: list[dict[str, Any]]) -> list[dict[str, Any]]:
    prepared = [dict(command) for command in commands]
    validators = {
        0: agents_payload_valid,
        1: reservations_payload_valid,
        2: inbox_payload_valid,
        3: status_payload_valid,
        4: readiness_payload_valid,
        5: durability_payload_valid,
    }
    for index, validator in validators.items():
        if index >= len(prepared):
            continue
        command = prepared[index]
        if command["ok"] and not validator(command["json"]):
            prepared[index] = invalid_response(command)
    return prepared


def normalize_readiness_health(value: Any) -> dict[str, Any]:
    if not isinstance(value, dict):
        return {}
    summary: dict[str, Any] = {}

    explicit_levels = [
        bounded_health_level(value.get(key))
        for key in ("health_level", "healthLevel")
        if key in value
    ]
    service_health = bounded_service_health(value.get("status"))
    health_rank = {"green": 0, "yellow": 1, "red": 2}
    health_level = max(
        (level for level in (*explicit_levels, service_health) if level is not None),
        key=lambda level: health_rank[level],
        default=None,
    )
    if health_level:
        summary["health_level"] = health_level

    semantic_rank = {"pass": 0, "unknown": 1, "fail": 2}
    semantic_candidates: list[tuple[str, Any]] = []
    for key in ("semantic_readiness", "semanticReadiness"):
        if key not in value:
            continue
        semantic = value.get(key)
        if isinstance(semantic, dict):
            semantic_status = bounded_semantic_readiness_status(semantic.get("status"))
            semantic_reason = semantic.get("reason") or semantic.get("detail") or semantic.get("message")
        else:
            semantic_status = bounded_semantic_readiness_status(semantic)
            semantic_reason = value.get("semantic_readiness_reason") or value.get("semanticReadinessReason")
        if semantic_status:
            semantic_candidates.append((semantic_status, semantic_reason))
    if semantic_candidates:
        semantic_status, semantic_reason = max(
            semantic_candidates,
            key=lambda pair: semantic_rank[pair[0]],
        )
        readiness = {"status": semantic_status}
        if semantic_status == "fail":
            readiness["reason"] = semantic_readiness_reason_class(semantic_reason)
        summary["semantic_readiness"] = readiness

    recovery = value.get("recovery")
    recovery_rank = {"ok": 0, "unknown_recovery": 1, "repair_required": 2, "corrupt": 3}
    recovery_candidates: list[tuple[str, str]] = []
    if isinstance(recovery, dict):
        for key in ("mode", "status"):
            if key not in recovery:
                continue
            mode = bounded_recovery_mode(recovery.get(key))
            if mode:
                recovery_candidates.append((mode, recovery_reason_class(mode, value, recovery)))
    for key in ("durability_state", "durabilityState"):
        if key not in value:
            continue
        raw_durability = value.get(key)
        if isinstance(raw_durability, str) and raw_durability.strip().lower() == "not_probed":
            continue
        mode = bounded_recovery_mode(raw_durability)
        if mode:
            recovery_candidates.append((mode, recovery_reason_class(mode, value, None)))

    if recovery_candidates:
        recovery_mode, recovery_reason = max(
            recovery_candidates,
            key=lambda pair: recovery_rank[pair[0]],
        )
        summary["durability_state"] = recovery_mode
    else:
        recovery_mode = None
        recovery_reason = None
    if recovery_mode and recovery_mode != "ok":
        summary["recovery"] = {
            "mode": recovery_mode,
            "reason": recovery_reason or "unknown",
        }
    return summary


def normalize_durability_health(value: Any) -> dict[str, Any]:
    if not durability_payload_valid(value):
        return {}
    state = bounded_recovery_mode(value["durability_state"]) or "unknown_recovery"
    if state == "ok" and (not value["allows_reads"] or not value["allows_writes"]):
        state = "repair_required"
    summary: dict[str, Any] = {"durability_state": state}
    if state != "ok":
        summary["recovery"] = {
            "mode": state,
            "reason": (
                "archive_corruption"
                if state == "corrupt"
                else "storage_recovery_required"
                if state == "repair_required"
                else "unknown"
            ),
        }
    return summary


def merge_health_summaries(*summaries: dict[str, Any]) -> dict[str, Any]:
    merged: dict[str, Any] = {}
    health_rank = {"green": 0, "yellow": 1, "red": 2}
    semantic_rank = {"pass": 0, "unknown": 1, "fail": 2}
    recovery_rank = {"ok": 0, "unknown_recovery": 1, "repair_required": 2, "corrupt": 3}

    levels = [item["health_level"] for item in summaries if item.get("health_level") in health_rank]
    if levels:
        merged["health_level"] = max(levels, key=lambda level: health_rank[level])

    semantics = [
        item["semantic_readiness"]
        for item in summaries
        if isinstance(item.get("semantic_readiness"), dict)
        and item["semantic_readiness"].get("status") in semantic_rank
    ]
    if semantics:
        merged["semantic_readiness"] = max(
            semantics,
            key=lambda semantic: semantic_rank[semantic["status"]],
        )

    modes: list[tuple[str, str]] = []
    for item in summaries:
        recovery = item.get("recovery")
        if isinstance(recovery, dict) and recovery.get("mode") in recovery_rank:
            modes.append((recovery["mode"], recovery.get("reason") or "unknown"))
        elif item.get("durability_state") in recovery_rank:
            modes.append((item["durability_state"], "unknown"))
    if modes:
        mode, reason = max(modes, key=lambda pair: recovery_rank[pair[0]])
        merged["durability_state"] = mode
        if mode != "ok":
            merged["recovery"] = {"mode": mode, "reason": reason}
    return merged


def health_requires_fallback(health: dict[str, Any]) -> bool:
    health_level = health.get("health_level")
    if health_level in {"yellow", "red"}:
        return True
    semantic = health.get("semantic_readiness")
    if isinstance(semantic, dict) and semantic.get("status") != "pass":
        return True
    recovery = health.get("recovery")
    if isinstance(recovery, dict) and recovery.get("mode") not in {None, "ok"}:
        return True
    durability_state = health.get("durability_state")
    return isinstance(durability_state, str) and durability_state not in {"", "ok"}


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


def source_degradations(command: dict[str, Any], project: Path) -> list[dict[str, Any]]:
    if command["ok"]:
        return []
    display = command_display(command["argv"], project)
    error_class = command["error_class"] or "unknown"
    return [
        {
            "code": "agent_mail_snapshot_source_unavailable",
            "severity": "warning",
            "message": f"Agent Mail snapshot source unavailable: {display} ({error_class}).",
            "repair": "Regenerate the redacted Agent Mail snapshot after the source is available.",
        }
    ]


def coordination_source(
    kind: str,
    status: str,
    entries: list[dict[str, Any]],
    degraded: list[dict[str, Any]] | None = None,
) -> dict[str, Any]:
    return {
        "kind": kind,
        "source_id": kind,
        "status": status,
        "freshness_ms": 0,
        "entries": entries,
        "degraded": degraded or [],
    }


def reservation_coordination_entries(reservations: list[dict[str, Any]]) -> list[dict[str, Any]]:
    entries = []
    for index, row in enumerate(reservations):
        path_pattern = row.get("path_pattern") or f"reservation:{index}"
        holder = row.get("holder")
        exclusive = bool(row.get("exclusive"))
        entry: dict[str, Any] = {
            "kind": "file_reservation",
            "id": path_pattern,
            "path_pattern": path_pattern,
            "status": "active",
            "severity": "warning" if exclusive else "info",
            "conflict": exclusive,
            "summary": (
                f"{'exclusive ' if exclusive else ''}reservation on {path_pattern}"
                + (f" held by {holder}" if holder else "")
            ),
            "provenance": ["agent-mail://file-reservations"],
        }
        if holder:
            entry["holder"] = holder
        entries.append(entry)
    return entries


def agent_coordination_entries(agents: list[dict[str, Any]]) -> list[dict[str, Any]]:
    entries = []
    for row in agents:
        name = row.get("name")
        if not name:
            continue
        entry: dict[str, Any] = {
            "kind": "agent",
            "id": name,
            "status": "known",
            "severity": "info",
            "conflict": False,
            "summary": f"Agent Mail identity {name}",
            "provenance": ["agent-mail://agents"],
        }
        if row.get("last_active_ts"):
            entry["summary"] = f"Agent Mail identity {name} last active {row['last_active_ts']}"
        entries.append(entry)
    return entries


def inbox_coordination_entries(inbox: list[dict[str, Any]]) -> list[dict[str, Any]]:
    entries = []
    for row in inbox:
        mailbox = row.get("mailbox")
        if not mailbox:
            continue
        unread = int(row.get("unread_count") or 0)
        ack_required = int(row.get("ack_required_count") or 0)
        entries.append(
            {
                "kind": "agent_mail_inbox",
                "id": mailbox,
                "status": "ack_required" if ack_required else "ready",
                "severity": "warning" if ack_required else "info",
                "conflict": False,
                "summary": (
                    f"{mailbox} inbox has {unread} unread message(s), "
                    f"{ack_required} requiring acknowledgement"
                ),
                "provenance": ["agent-mail://inbox"],
            }
        )
    return entries


def thread_coordination_entries(threads: list[dict[str, Any]]) -> list[dict[str, Any]]:
    entries = []
    for row in threads:
        thread_id = row.get("thread_id")
        if not thread_id:
            continue
        message_count = int(row.get("message_count") or 0)
        summary = row.get("subject") or f"Agent Mail thread {thread_id}"
        entries.append(
            {
                "kind": "agent_mail_thread",
                "id": thread_id,
                "status": "recent" if row.get("last_activity_at") else "known",
                "severity": "info",
                "conflict": False,
                "summary": f"{summary} ({message_count} message(s))",
                "provenance": ["agent-mail://threads"],
            }
        )
    return entries


def coordination_snapshot(
    output: dict[str, Any],
    agents_cmd: dict[str, Any],
    reservations_cmd: dict[str, Any],
    inbox_cmd: dict[str, Any],
    status_cmd: dict[str, Any],
    project: Path,
) -> dict[str, Any]:
    degraded = output["degraded"]
    fallback_active = output["fallback_active"]
    health_status = "degraded" if fallback_active else "fresh"
    return {
        "schema": "ee.coordination_snapshot.v1",
        "captured_at": output["generated_at"],
        "scope": "workspace",
        "sources": [
            coordination_source(
                "agent_mail_reservations",
                "fresh" if reservations_cmd["ok"] else "unavailable",
                reservation_coordination_entries(output["file_reservations"]),
                source_degradations(reservations_cmd, project),
            ),
            coordination_source(
                "agent_mail_agents",
                "fresh" if agents_cmd["ok"] else "unavailable",
                agent_coordination_entries(output["agents"]),
                source_degradations(agents_cmd, project),
            ),
            coordination_source(
                "agent_mail_inbox",
                "fresh" if status_cmd["ok"] else "unavailable",
                inbox_coordination_entries(output["inbox"]),
                source_degradations(status_cmd, project),
            ),
            coordination_source(
                "agent_mail_threads",
                "fresh" if inbox_cmd["ok"] else "unavailable",
                thread_coordination_entries(output["threads"]),
                source_degradations(inbox_cmd, project),
            ),
            coordination_source(
                "agent_mail_snapshot_health",
                health_status,
                [
                    {
                        "kind": "agent_mail_snapshot_health",
                        "id": "producer",
                        "status": output["producer_status"],
                        "severity": "warning" if fallback_active else "info",
                        "conflict": False,
                        "summary": f"Agent Mail snapshot producer {output['producer_status']}",
                        "provenance": ["agent-mail://snapshot-producer"],
                    }
                ],
                degraded,
            ),
        ],
    }


def build_snapshot_output(
    project: Path,
    agent: str,
    commands: list[dict[str, Any]],
    thread_limit: int,
) -> dict[str, Any]:
    prepared = prepare_snapshot_commands(commands)
    commands[:] = prepared
    agents_cmd, reservations_cmd, inbox_cmd, status_cmd, health_cmd, durability_cmd = commands[:6]

    agents = normalize_agents(agents_cmd["json"]) if agents_cmd["ok"] else []
    reservations = normalize_reservations(reservations_cmd["json"], project) if reservations_cmd["ok"] else []
    inbox = normalize_status_counts(status_cmd["json"], agent) if status_cmd["ok"] else []
    if inbox_cmd["ok"]:
        messages = normalize_inbox_messages(inbox_cmd["json"])
        threads = normalize_threads(messages, thread_limit)
    else:
        threads = []

    health = merge_health_summaries(
        normalize_readiness_health(health_cmd["json"] if health_cmd["ok"] else None),
        normalize_durability_health(durability_cmd["json"] if durability_cmd["ok"] else None),
    )
    degraded = degraded_entries(commands, project)
    fallback_active = bool(degraded) or health_requires_fallback(health)
    output = {
        "schema": AGENT_MAIL_SNAPSHOT_SCHEMA,
        "generated_at": utc_now(),
        "project_key": workspace_project_key(project),
        "agent_name": redact_text(agent),
        "redaction_status": REDACTION_STATUS,
        "producer_status": "degraded" if fallback_active else "ok",
        "source_commands": [command_display(command["argv"], project) for command in commands],
        "command_statuses": [command_status(command, project) for command in commands],
        "fallback_active": fallback_active,
        "am_agents_list_ok": agents_cmd["ok"],
        "summary": {
            "agent_count": len(agents),
            "file_reservation_count": len(reservations),
            "inbox_mailbox_count": len(inbox),
            "thread_count": len(threads),
            "source_command_count": len(commands),
            "degraded_count": len(degraded),
        },
        "degraded": degraded,
        "file_reservations": reservations,
        "agents": agents,
        "inbox": inbox,
        "threads": threads,
    }
    output.update(health)
    return output


def synthetic_command(
    argv: list[str],
    json_value: Any = None,
    ok: bool = True,
    exit_code: int | None = 0,
    timed_out: bool = False,
    error_class: str | None = None,
) -> dict[str, Any]:
    if ok and argv and argv[0] == "agent-mail-health" and exit_code == 0:
        exit_code = 200
    return {
        "argv": argv,
        "ok": ok,
        "exit_code": exit_code,
        "timed_out": timed_out,
        "json": json_value if ok else None,
        "error_class": error_class if not ok else None,
    }


def assert_absent(rendered: str, forbidden: list[str]) -> None:
    for needle in forbidden:
        if needle in rendered:
            raise AssertionError(f"snapshot leaked forbidden text: {needle}")


def run_self_test() -> int:
    project = Path("/Users/example/workspaces/eidetic_engine_cli").resolve()
    agent = "AzureElm"
    assert normalize_workspace_identity(str(project), windows=False) == str(project)
    assert normalize_workspace_identity(r"\\?\C:\Users\Example\repo", windows=True) == "c:/Users/Example/repo"
    assert normalize_workspace_identity(r"\\?\UNC\server\share\repo", windows=True) == "//server/share/repo"
    current_physical = physical_workspace_path(Path.cwd())
    assert current_physical.samefile(Path.cwd())
    if sys.platform == "darwin" and str(current_physical).startswith("/Users/"):
        case_alias = Path("/users") / current_physical.relative_to("/Users")
        if case_alias.exists() and case_alias.samefile(current_physical):
            assert physical_workspace_path(case_alias) == current_physical
            assert workspace_project_key(physical_workspace_path(case_alias)) == workspace_project_key(current_physical)
    assert rfc3339_timestamp_valid("2026-07-11T15:16:17.123456Z")
    assert rfc3339_timestamp_valid("2026-07-11T11:16:17-04:00")
    for invalid_timestamp in (
        "2026-W01-1T00:00:00+00:00",
        "20260101T000000+00:00",
        "2026-01-01T00:00:00+00:00:30",
        "2026-01-01T00:00:00,5Z",
    ):
        assert not rfc3339_timestamp_valid(invalid_timestamp)
    github_token = "ghp_" + ("A" * 24)
    api_secret = "api_key=" + ("B" * 24)
    def healthy_commands() -> list[dict[str, Any]]:
        return [
            synthetic_command(
                ["am", "agents", "list", "--project", str(project), "--json"],
                {
                    "agents": [
                        {"name": agent, "last_active_ts": "2026-06-08T00:00:00Z"},
                        {"name": agent, "last_active_ts": "2026-06-08T00:10:00Z"},
                        {
                            "agent_name": "CreamSwan",
                            "lastActiveAt": "2026-06-08T00:05:00Z",
                            "diagnostic": f"token={github_token}",
                        },
                    ]
                },
            ),
            synthetic_command(
                ["am", "robot", "reservations", "--project", str(project), "--all", "--format", "json"],
                {
                    "all_active": [
                        {
                            "path_pattern": str(project / "scripts" / "agent_mail_snapshot.sh"),
                            "holder": "CreamSwan",
                            "exclusive": "exclusive",
                            "expires_ts": "2026-06-08T01:00:00Z",
                        },
                        {
                            "path": "/Users/example/.ssh/id_rsa",
                            "agent_name": "IcyCat",
                            "exclusive": False,
                        },
                    ]
                },
            ),
            synthetic_command(
                ["am", "mail", "inbox", "--project", str(project), "--agent", agent, "--limit", "20", "--json"],
                {
                    "messages": [
                        {
                            "id": 1,
                            "thread_id": "T-1",
                            "subject": f"Review path /Users/example/private/file.rs and {api_secret}",
                            "created_ts": "2026-06-08T01:00:00Z",
                            "ack_required": True,
                            "body_md": "body-only secret should never enter snapshot",
                        },
                        {
                            "id": 2,
                            "thread_id": "T-1",
                            "subject": "Older thread message",
                            "created_ts": "2026-06-08T00:30:00Z",
                        },
                        {
                            "id": 3,
                            "threadId": "T-2",
                            "subject": "Claim gate handoff",
                            "createdAt": "2026-06-08T01:15:00Z",
                            "ackRequired": True,
                        },
                    ]
                },
            ),
            synthetic_command(
                ["am", "status", "--project", str(project), "--agent", agent, "--json"],
                {"health": "ok", "unread": 0, "ack_required": 0},
            ),
            synthetic_command(
                ["agent-mail-health", "http://127.0.0.1:8765/health"],
                {"status": "ready", "durability_state": "not_probed"},
            ),
            synthetic_command(
                ["agent-mail-health", "http://127.0.0.1:8765/health/durability"],
                {"durability_state": "healthy", "allows_reads": True, "allows_writes": True},
            ),
        ]

    commands = healthy_commands()
    output = build_snapshot_output(project, agent, commands, thread_limit=10)
    coordination = coordination_snapshot(
        output,
        commands[0],
        commands[1],
        commands[2],
        commands[3],
        project,
    )

    assert output["schema"] == AGENT_MAIL_SNAPSHOT_SCHEMA
    assert output["producer_status"] == "ok"
    assert output["summary"]["agent_count"] == 2
    assert output["agents"][0]["last_active_ts"] == "2026-06-08T00:10:00Z"
    assert output["summary"]["file_reservation_count"] == 2
    assert output["summary"]["inbox_mailbox_count"] == 1
    assert output["summary"]["thread_count"] == 2
    assert output["summary"]["source_command_count"] == 6
    assert output["health_level"] == "green"
    assert output["durability_state"] == "ok"
    assert [status["exit_code"] for status in output["command_statuses"][4:]] == [200, 200]
    assert output["inbox"][0]["unread_count"] == 0
    assert output["inbox"][0]["ack_required_count"] == 0
    assert output["file_reservations"][0]["path_pattern"] == "[REDACTED:absolute_path]"
    assert output["file_reservations"][1]["path_pattern"] == "scripts/agent_mail_snapshot.sh"
    assert coordination["schema"] == "ee.coordination_snapshot.v1"
    assert len(coordination["sources"]) == 5
    assert coordination["sources"][0]["status"] == "fresh"
    assert coordination["sources"][4]["status"] == "fresh"

    degraded_commands = healthy_commands()
    degraded_commands[1] = synthetic_command(
        ["am", "robot", "reservations", "--project", str(project), "--all", "--format", "json"],
        ok=False,
        exit_code=1,
        error_class="command_failed",
    )
    degraded_output = build_snapshot_output(project, agent, degraded_commands, thread_limit=10)
    degraded_coordination = coordination_snapshot(
        degraded_output,
        degraded_commands[0],
        degraded_commands[1],
        degraded_commands[2],
        degraded_commands[3],
        project,
    )
    assert degraded_output["producer_status"] == "degraded"
    assert degraded_output["summary"]["degraded_count"] == 1
    assert degraded_coordination["sources"][0]["status"] == "unavailable"
    assert degraded_coordination["sources"][4]["status"] == "degraded"

    recovery_commands = healthy_commands()
    recovery_commands[4] = synthetic_command(
        ["agent-mail-health", "http://127.0.0.1:8765/health"],
        {
            "status": "ready",
            "semantic_readiness": {"status": "ok"},
            "recovery": {
                "mode": "corrupt",
                "next_action": "Run Agent Mail repair or restore from /Users/example/.local/share/mcp_agent_mail/storage.sqlite3 after B-tree page 283 failed",
                "bundle_path": "/Users/example/.local/share/mcp_agent_mail/doctor/forensics/storage.sqlite3/reconstruct-20260602_030410_115",
            },
        },
    )
    recovery_output = build_snapshot_output(project, agent, recovery_commands, thread_limit=10)
    assert recovery_output["producer_status"] == "degraded"
    assert recovery_output["fallback_active"] is True
    assert recovery_output["summary"]["degraded_count"] == 0
    assert recovery_output["recovery"]["mode"] == "corrupt"
    assert recovery_output["recovery"]["reason"] == "archive_corruption"

    recovery_coordination = coordination_snapshot(
        recovery_output,
        recovery_commands[0],
        recovery_commands[1],
        recovery_commands[2],
        recovery_commands[3],
        project,
    )
    assert recovery_coordination["sources"][4]["status"] == "degraded"
    assert recovery_coordination["sources"][4]["entries"][0]["severity"] == "warning"

    contradictory_readiness_commands = healthy_commands()
    contradictory_readiness_commands[4] = synthetic_command(
        ["agent-mail-health", "http://127.0.0.1:8765/health"],
        {
            "status": "ready",
            "health_level": "green",
            "healthLevel": "red",
            "recovery": {"mode": "ok", "status": "corrupt"},
        },
    )
    contradictory_readiness_output = build_snapshot_output(
        project,
        agent,
        contradictory_readiness_commands,
        thread_limit=10,
    )
    assert contradictory_readiness_output["fallback_active"] is True
    assert contradictory_readiness_output["health_level"] == "red"
    assert contradictory_readiness_output["recovery"]["mode"] == "corrupt"

    repair_commands = healthy_commands()
    repair_commands[4] = synthetic_command(
        ["agent-mail-health", "http://127.0.0.1:8765/health"],
        {
            "health_level": "yellow",
            "semantic_readiness": {"status": "ok"},
            "recovery": {"mode": "repair_required"},
        },
    )
    repair_output = build_snapshot_output(project, agent, repair_commands, thread_limit=10)
    assert repair_output["producer_status"] == "degraded"
    assert repair_output["fallback_active"] is True
    assert repair_output["recovery"]["mode"] == "repair_required"

    dedicated_corrupt_commands = healthy_commands()
    dedicated_corrupt_commands[5] = synthetic_command(
        ["agent-mail-health", "http://127.0.0.1:8765/health/durability"],
        {"durability_state": "corrupt", "allows_reads": False, "allows_writes": False},
    )
    dedicated_corrupt_output = build_snapshot_output(
        project,
        agent,
        dedicated_corrupt_commands,
        thread_limit=10,
    )
    assert dedicated_corrupt_output["fallback_active"] is True
    assert dedicated_corrupt_output["recovery"]["mode"] == "corrupt"

    read_disabled_commands = healthy_commands()
    read_disabled_commands[5] = synthetic_command(
        ["agent-mail-health", "http://127.0.0.1:8765/health/durability"],
        {"durability_state": "healthy", "allows_reads": False, "allows_writes": True},
    )
    read_disabled_output = build_snapshot_output(project, agent, read_disabled_commands, thread_limit=10)
    assert read_disabled_output["fallback_active"] is True
    assert read_disabled_output["recovery"]["mode"] == "repair_required"
    assert read_disabled_output["summary"]["degraded_count"] == 0

    malformed_durability_commands = healthy_commands()
    malformed_durability_commands[5] = synthetic_command(
        ["agent-mail-health", "http://127.0.0.1:8765/health/durability"],
        {"durability_state": "healthy", "allows_reads": True},
    )
    malformed_durability_output = build_snapshot_output(
        project,
        agent,
        malformed_durability_commands,
        thread_limit=10,
    )
    assert malformed_durability_output["fallback_active"] is True
    assert malformed_durability_output["summary"]["degraded_count"] == 1
    assert malformed_durability_output["degraded"][0]["error_class"] == "invalid_response"

    malformed_collection_cases = [
        (0, {}, "agents"),
        (1, {}, "file_reservations"),
        (2, {}, "threads"),
        (0, {"agents": [{"name": ""}]}, "agents"),
        (
            1,
            {
                "all_active": [
                    {
                        "path": "scripts/agent_mail_snapshot.sh",
                        "agent": "CreamSwan",
                        "exclusive": "unknown",
                    }
                ]
            },
            "file_reservations",
        ),
        (
            1,
            {
                "all_active": [],
                "reservations": [
                    {
                        "path": "src/core/**",
                        "agent": "OtherAgent",
                        "exclusive": True,
                    }
                ],
            },
            "file_reservations",
        ),
        (
            1,
            {
                "all_active": [
                    {
                        "path_pattern": 0,
                        "path": "src/core/**",
                        "holder": "OtherAgent",
                        "exclusive": True,
                    }
                ]
            },
            "file_reservations",
        ),
        (2, {"messages": [{"subject": "missing thread identity"}]}, "threads"),
    ]
    for source_index, malformed_payload, normalized_field in malformed_collection_cases:
        malformed_collection_commands = healthy_commands()
        malformed_collection_commands[source_index] = synthetic_command(
            malformed_collection_commands[source_index]["argv"],
            malformed_payload,
        )
        malformed_collection_output = build_snapshot_output(
            project,
            agent,
            malformed_collection_commands,
            thread_limit=10,
        )
        assert malformed_collection_output["fallback_active"] is True
        assert malformed_collection_output["summary"]["degraded_count"] == 1
        assert malformed_collection_output["degraded"][0]["error_class"] == "invalid_response"
        assert malformed_collection_output[normalized_field] == []

    for missing_readiness in (
        {"durability_state": "not_probed"},
        {"semantic_readiness": {"status": "pass"}},
        {"recovery": {"mode": "ok"}},
    ):
        missing_readiness_commands = healthy_commands()
        missing_readiness_commands[4] = synthetic_command(
            ["agent-mail-health", "http://127.0.0.1:8765/health"],
            missing_readiness,
        )
        missing_readiness_output = build_snapshot_output(
            project,
            agent,
            missing_readiness_commands,
            thread_limit=10,
        )
        assert missing_readiness_output["fallback_active"] is True
        assert missing_readiness_output["summary"]["degraded_count"] == 1
        assert missing_readiness_output["degraded"][0]["error_class"] == "invalid_response"

    for invalid_counts in (
        {"unread": False, "ack_required": 0},
        {"unread": -1, "ack_required": 0},
        {"unread": "1", "ack_required": 0},
        {"unread": MAX_WIRE_COUNT + 1, "ack_required": 0},
        {"unread": 0},
    ):
        malformed_status_commands = healthy_commands()
        malformed_status_commands[3] = synthetic_command(
            ["am", "status", "--project", str(project), "--agent", agent, "--json"],
            invalid_counts,
        )
        malformed_status_output = build_snapshot_output(
            project,
            agent,
            malformed_status_commands,
            thread_limit=10,
        )
        assert malformed_status_output["fallback_active"] is True
        assert malformed_status_output["inbox"] == []
        assert malformed_status_output["summary"]["degraded_count"] == 1
        assert malformed_status_output["degraded"][0]["error_class"] == "invalid_response"

    http_error_commands = healthy_commands()
    http_error_commands[4] = synthetic_command(
        ["agent-mail-health", "http://127.0.0.1:8765/health"],
        {"status": "internal_error"},
        ok=False,
        exit_code=500,
        error_class="http_status",
    )
    http_error_output = build_snapshot_output(project, agent, http_error_commands, thread_limit=10)
    assert http_error_output["producer_status"] == "degraded"
    assert http_error_output["fallback_active"] is True
    assert http_error_output["summary"]["degraded_count"] == 1

    rendered = json.dumps({"snapshot": output, "coordination": coordination}, sort_keys=True)
    recovery_rendered = json.dumps(recovery_output, sort_keys=True)
    assert_absent(
        rendered + recovery_rendered,
        [
            str(project),
            "/Users/example",
            github_token,
            api_secret,
            "body-only secret",
            "storage.sqlite3",
            "B-tree",
            "page 283",
            "reconstruct-20260602_030410_115",
        ],
    )
    print("agent_mail_snapshot: self-test passed", file=sys.stderr)
    return 0


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Emit a redacted read-only Agent Mail snapshot for ee swarm brief.",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog=(
            "examples:\n"
            "  scripts/agent_mail_snapshot.sh --project \"$PWD\" --agent \"$AGENT_NAME\" --json\n"
            "  scripts/agent_mail_snapshot.sh --project \"$PWD\" --agent \"$AGENT_NAME\" --output /private/tmp/ee-agent-mail-snapshot.json\n"
            "  scripts/agent_mail_snapshot.sh --project \"$PWD\" --agent \"$AGENT_NAME\" --json --output /private/tmp/ee-agent-mail-snapshot.json\n"
            "  scripts/agent_mail_snapshot.sh --self-test"
        ),
    )
    parser.add_argument("--project", default=os.environ.get("AGENT_MAIL_PROJECT") or os.getcwd())
    parser.add_argument("--agent", default=os.environ.get("AGENT_MAIL_AGENT") or os.environ.get("AGENT_NAME"))
    parser.add_argument("--am-bin", default=os.environ.get("AGENT_MAIL_AM_BIN", "am"))
    parser.add_argument("--inbox-limit", type=int, default=DEFAULT_INBOX_LIMIT)
    parser.add_argument("--thread-limit", type=int, default=DEFAULT_THREAD_LIMIT)
    parser.add_argument("--timeout-sec", type=float, default=DEFAULT_TIMEOUT_SEC)
    parser.add_argument(
        "--json",
        action="store_true",
        help="Emit snapshot JSON to stdout. With --output, also write the same snapshot JSON file.",
    )
    parser.add_argument(
        "--stdout",
        action="store_true",
        help="Alias for --json; useful when --output is also set.",
    )
    parser.add_argument(
        "--output",
        help="Write snapshot JSON to this path. Without --json/--stdout, suppress stdout.",
    )
    parser.add_argument(
        "--coordination-output",
        help="Also write a pack-compatible ee.coordination_snapshot.v1 companion JSON file.",
    )
    parser.add_argument(
        "--self-test",
        action="store_true",
        help="Run synthetic normalization, redaction, and coordination snapshot checks without calling Agent Mail.",
    )
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    if args.self_test:
        return run_self_test()

    try:
        project = physical_workspace_path(Path(args.project))
        workspace_project_key(project)
    except (OSError, ValueError) as error:
        print(f"agent_mail_snapshot: {error}", file=sys.stderr)
        return 2
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
    if args.output and args.coordination_output:
        if Path(args.output).resolve() == Path(args.coordination_output).resolve():
            print(
                "agent_mail_snapshot: --output and --coordination-output must differ",
                file=sys.stderr,
            )
            return 2
    output_path_error = validate_output_path("--output", args.output)
    if output_path_error:
        return output_path_error
    coordination_path_error = validate_output_path("--coordination-output", args.coordination_output)
    if coordination_path_error:
        return coordination_path_error

    am_bin = args.am_bin
    commands = [
        run_json_command([am_bin, "agents", "list", "--project", str(project), "--json"], args.timeout_sec),
        run_json_command([am_bin, "robot", "reservations", "--project", str(project), "--all", "--format", "json"], args.timeout_sec),
        run_json_command([am_bin, "mail", "inbox", "--project", str(project), "--agent", agent, "--limit", str(args.inbox_limit), "--json"], args.timeout_sec),
        run_json_command([am_bin, "status", "--project", str(project), "--agent", agent, "--json"], args.timeout_sec),
        run_health_probe(HEALTH_PATH, args.timeout_sec),
        run_health_probe(DURABILITY_PATH, args.timeout_sec),
    ]

    output = build_snapshot_output(project, agent, commands, args.thread_limit)
    agents_cmd, reservations_cmd, inbox_cmd, status_cmd = commands[:4]

    rendered = json.dumps(output, sort_keys=True, separators=(",", ":")) + "\n"
    write_stdout = args.json or args.stdout or not args.output
    if args.output:
        Path(args.output).write_text(rendered, encoding="utf-8")
    if write_stdout:
        sys.stdout.write(rendered)
    if args.coordination_output:
        coordination = coordination_snapshot(
            output,
            agents_cmd,
            reservations_cmd,
            inbox_cmd,
            status_cmd,
            project,
        )
        rendered_coordination = (
            json.dumps(coordination, sort_keys=True, separators=(",", ":")) + "\n"
        )
        Path(args.coordination_output).write_text(rendered_coordination, encoding="utf-8")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
PY
