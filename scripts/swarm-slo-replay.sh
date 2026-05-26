#!/usr/bin/env bash
set -euo pipefail

usage() {
    cat >&2 <<'EOF'
usage: scripts/swarm-slo-replay.sh --input TRACE.jsonl [--output OUT.jsonl] [--summary SUMMARY.json] [--verify-determinism]

Replays a redaction-safe swarm SLO JSONL trace without executing recorded
commands or mutating Beads, Agent Mail, git, ee storage, or RCH state.

Accepted input rows:
  - ee.test_event.v1
  - ee.agent_workload_trace.v1
  - CASS/session-shaped JSON rows with schema fields preserved
EOF
}

INPUT=""
OUTPUT="-"
SUMMARY=""
VERIFY_DETERMINISM=0

while [ $# -gt 0 ]; do
    case "$1" in
        --input)
            INPUT="${2:-}"
            shift 2
            ;;
        --output)
            OUTPUT="${2:-}"
            shift 2
            ;;
        --summary)
            SUMMARY="${2:-}"
            shift 2
            ;;
        --verify-determinism)
            VERIFY_DETERMINISM=1
            shift
            ;;
        --help|-h)
            usage
            exit 0
            ;;
        *)
            printf 'swarm-slo-replay: unknown argument: %s\n' "$1" >&2
            usage
            exit 2
            ;;
    esac
done

if [ -z "$INPUT" ]; then
    printf 'swarm-slo-replay: --input is required\n' >&2
    usage
    exit 2
fi

if [ ! -f "$INPUT" ]; then
    printf 'swarm-slo-replay: input not found: %s\n' "$INPUT" >&2
    exit 2
fi

python3 - "$INPUT" "$OUTPUT" "$SUMMARY" "$VERIFY_DETERMINISM" <<'PY'
import hashlib
import json
import os
import sys
from collections import Counter
from pathlib import Path

input_path = Path(sys.argv[1])
output_path = sys.argv[2]
summary_path = sys.argv[3]
verify_determinism = sys.argv[4] == "1"

DEFAULT_MAX_INPUT_BYTES = 8 * 1024 * 1024
DEFAULT_MAX_LINE_BYTES = 256 * 1024
DEFAULT_MAX_ROWS = 100_000


def positive_int_env(name: str, default: int) -> int:
    raw = os.environ.get(name)
    if raw is None or raw.strip() == "":
        return default
    try:
        value = int(raw)
    except ValueError as error:
        raise SystemExit(f"swarm-slo-replay: {name} must be a positive integer") from error
    if value <= 0:
        raise SystemExit(f"swarm-slo-replay: {name} must be a positive integer")
    return value


MAX_INPUT_BYTES = positive_int_env(
    "EE_SWARM_SLO_REPLAY_MAX_INPUT_BYTES",
    DEFAULT_MAX_INPUT_BYTES,
)
MAX_LINE_BYTES = positive_int_env(
    "EE_SWARM_SLO_REPLAY_MAX_LINE_BYTES",
    DEFAULT_MAX_LINE_BYTES,
)
MAX_ROWS = positive_int_env("EE_SWARM_SLO_REPLAY_MAX_ROWS", DEFAULT_MAX_ROWS)


def read_rows(path: Path):
    try:
        input_bytes = path.stat().st_size
    except OSError as error:
        raise SystemExit(f"swarm-slo-replay: cannot stat input: {error}") from error
    if input_bytes > MAX_INPUT_BYTES:
        raise SystemExit(
            f"swarm-slo-replay: input exceeds max bytes ({input_bytes} > {MAX_INPUT_BYTES})"
        )

    rows = []
    try:
        handle = path.open("rb")
    except OSError as error:
        raise SystemExit(f"swarm-slo-replay: cannot open input: {error}") from error
    with handle:
        for line_no, raw_bytes in enumerate(handle, start=1):
            raw_bytes = raw_bytes.rstrip(b"\r\n")
            if raw_bytes == b"":
                continue
            if len(raw_bytes) > MAX_LINE_BYTES:
                raise SystemExit(
                    "swarm-slo-replay: line "
                    f"{line_no} exceeds max bytes ({len(raw_bytes)} > {MAX_LINE_BYTES})"
                )
            if len(rows) >= MAX_ROWS:
                raise SystemExit(
                    f"swarm-slo-replay: input exceeds max rows ({len(rows) + 1} > {MAX_ROWS})"
                )
            try:
                raw = raw_bytes.decode("utf-8")
            except UnicodeDecodeError as error:
                raise SystemExit(
                    f"swarm-slo-replay: invalid UTF-8 at line {line_no}: {error}"
                ) from error
            try:
                payload = json.loads(raw)
            except json.JSONDecodeError as error:
                raise SystemExit(
                    f"swarm-slo-replay: invalid JSONL at line {line_no}: {error}"
                ) from error
            if not isinstance(payload, dict):
                raise SystemExit(f"swarm-slo-replay: line {line_no} is not a JSON object")
            schema = payload.get("schema")
            # Reject non-string schemas (None, int, list, dict, ...) here
            # instead of coercing through str(...) — the prior shape
            # `str(payload.get("schema", ""))` accepted `{"schema": null}`
            # as the literal string "None" and bypassed the missing-schema
            # check, leaking a corrupt row through replay with the spurious
            # schema "None" in downstream counts.
            if not isinstance(schema, str) or schema == "":
                raise SystemExit(f"swarm-slo-replay: line {line_no} missing schema")
            rows.append((line_no, raw, payload))
    return rows


def stable_event_tie_key(raw, payload):
    stable_parts = {
        "schema": payload.get("schema"),
        "traceId": payload.get("traceId", payload.get("trace_id")),
        "agentId": payload.get("agentId", payload.get("agent_id")),
        "runId": payload.get("runId", payload.get("run_id")),
        "source": payload.get("surface", payload.get("source")),
        "phase": payload.get("phase"),
        "kind": payload.get("kind"),
    }
    material = json.dumps(stable_parts, sort_keys=True, separators=(",", ":")) + "\0" + raw
    return hashlib.sha256(material.encode("utf-8")).hexdigest()


def canonical_event_key(row):
    line_no, raw, payload = row
    event_index = payload.get("eventIndex", payload.get("event_index"))
    if isinstance(event_index, int):
        return (0, event_index, stable_event_tie_key(raw, payload), line_no)
    return (1, line_no, "", line_no)


def replay_bytes(rows):
    # Deterministic event ordering: explicit eventIndex first; colliding
    # explicit indexes use a content-derived tie-breaker so parallel producer
    # merge order cannot change replay bytes. Unindexed CASS/test-event streams
    # retain source order. The original row bytes are emitted unchanged, so
    # replay cannot smuggle runtime timestamps into output.
    ordered = sorted(rows, key=canonical_event_key)
    if not ordered:
        return b""
    return ("\n".join(raw for _line_no, raw, _payload in ordered) + "\n").encode("utf-8")


def sha256_hex(data: bytes) -> str:
    return "sha256:" + hashlib.sha256(data).hexdigest()


rows = read_rows(input_path)
first = replay_bytes(rows)

if verify_determinism:
    second = replay_bytes(rows)
    if first != second:
        raise SystemExit("swarm-slo-replay: deterministic replay diverged for identical input")

if output_path == "-":
    sys.stdout.buffer.write(first)
else:
    out = Path(output_path)
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_bytes(first)

if summary_path:
    schema_counts = Counter(str(payload.get("schema", "")) for _line_no, _raw, payload in rows)
    kind_counts = Counter(str(payload.get("kind", payload.get("phase", "unknown"))) for _line_no, _raw, payload in rows)
    mutating_rows = []
    mutating_verbs = {
        "remember",
        "import",
        "migrate",
        "apply",
        "git",
        "br",
        "bd",
    }
    for line_no, _raw, payload in rows:
        command = payload.get("command")
        command_words = []
        if isinstance(command, str):
            command_words = command.lower().split()
        elif isinstance(command, dict):
            verbs = command.get("verbs")
            if isinstance(verbs, list):
                command_words = [str(verb).lower() for verb in verbs]
        if any(word in mutating_verbs for word in command_words):
            mutating_rows.append(line_no)
    summary = {
        "schema": "ee.swarm_slo.replay.v1",
        "inputPath": str(input_path),
        "eventCount": len(rows),
        "replayHash": sha256_hex(first),
        "deterministic": True,
        "dryRunOnly": True,
        "mutationExecuted": False,
        "mutatingCommandRows": mutating_rows,
        "schemaCounts": dict(sorted(schema_counts.items())),
        "kindCounts": dict(sorted(kind_counts.items())),
    }
    out = Path(summary_path)
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(summary, sort_keys=True, indent=2) + "\n", encoding="utf-8")
PY
