#!/usr/bin/env bash
# bd-f6jfs.10 - shard fan-out closeout proof matrix.
#
# This is a read-only aggregator. It never runs Cargo directly; the emitted
# verification commands are RCH-only so closeout evidence cannot accidentally
# come from a local build.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
ALLOW_INCOMPLETE=0
FORMAT="json"

usage() {
    cat <<'EOF'
Usage: scripts/shard_fanout_closeout_matrix.sh [--json] [--markdown] [--allow-incomplete]

Options:
  --json              Emit machine-readable JSON (default).
  --markdown          Emit a Markdown summary for tracker comments.
  --allow-incomplete  Exit 0 even when the matrix proves closeout is blocked.
  -h, --help          Show this help.

The script is read-only and never invokes cargo directly. Full Rust gates must
be run through one of the emitted rch exec commands.
EOF
}

while [ "$#" -gt 0 ]; do
    case "$1" in
        --json)
            FORMAT="json"
            shift
            ;;
        --markdown)
            FORMAT="markdown"
            shift
            ;;
        --allow-incomplete)
            ALLOW_INCOMPLETE=1
            shift
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            echo "shard_fanout_closeout_matrix: unknown argument: $1" >&2
            usage >&2
            exit 2
            ;;
    esac
done

cd "$REPO_ROOT"

python3 - "$FORMAT" "$ALLOW_INCOMPLETE" "$REPO_ROOT" <<'PY'
import json
import pathlib
import subprocess
import sys
from typing import Any

fmt = sys.argv[1]
allow_incomplete = sys.argv[2] == "1"
repo_root = pathlib.Path(sys.argv[3])

FULL_RCH_COMMANDS = [
    ["rch", "exec", "--", "cargo", "check", "--all-targets"],
    ["rch", "exec", "--", "cargo", "test", "--workspace", "--all-targets"],
    ["rch", "exec", "--", "cargo", "clippy", "--all-targets", "--", "-D", "warnings"],
]

CHILDREN = [
    {
        "id": "bd-f6jfs.1",
        "criterion": "ADR/docs/schema contract landed and linked",
        "artifacts": [
            "docs/adr/0040-per-workspace-shard-fanout.md",
            "docs/architecture/shard-fanout.md",
            "docs/schemas/ee.migration.shard_fanout.v1.json",
        ],
        "needles": [
            ("docs/adr/0040-per-workspace-shard-fanout.md", "ee.migration.shard_fanout.v1"),
            ("docs/architecture/shard-fanout.md", "ee migrate shard-fanout"),
        ],
    },
    {
        "id": "bd-f6jfs.2",
        "criterion": "Env registry, Failure-mode fixtures, and shard resolver docs/tests landed",
        "artifacts": [
            "src/config/env_registry.rs",
            "docs/env_vars.md",
            "src/db/shard.rs",
            "tests/fixtures/failure_modes/shard_fanout_catalog_missing.json",
            "tests/fixtures/failure_modes/shard_fanout_home_unavailable.json",
            "tests/fixtures/failure_modes/shard_fanout_root_unsafe.json",
            "tests/fixtures/failure_modes/shard_fanout_shard_missing.json",
            "tests/fixtures/failure_modes/shard_fanout_workspace_id_unsafe.json",
            "tests/fixtures/failure_modes/shard_fanout_workspace_unavailable.json",
        ],
        "needles": [
            ("src/config/env_registry.rs", "EE_SHARD_FANOUT_ENABLED"),
            ("src/config/env_registry.rs", "EE_SHARDS_DIR"),
            ("docs/env_vars.md", "EE_SHARD_FANOUT_ENABLED"),
            ("docs/env_vars.md", "EE_SHARDS_DIR"),
        ],
    },
    {
        "id": "bd-f6jfs.3",
        "criterion": "DbShardRouter and per-shard write ownership implemented",
        "artifacts": ["src/db/shard.rs", "src/db/mod.rs"],
        "needles": [
            ("src/db/shard.rs", "DbShardRouter"),
            ("src/db/shard.rs", "DbShardRoutingMode::ShardFanout"),
            ("src/db/mod.rs", "WriteOwnerKey::File"),
        ],
    },
    {
        "id": "bd-f6jfs.4",
        "criterion": "Migration dry-run/apply/idempotence/partial failure evidence",
        "artifacts": ["src/db/shard.rs", "src/cli/mod.rs"],
        "needles": [
            ("src/db/shard.rs", "plan_shard_fanout_migration"),
            ("src/cli/mod.rs", "migrate shard-fanout"),
        ],
    },
    {
        "id": "bd-f6jfs.5",
        "criterion": "Cross-shard read/search/context parity evidence",
        "artifacts": ["src/db/shard.rs", "src/core/search.rs", "src/core/context.rs"],
        "needles": [
            ("src/db/shard.rs", "plan_peer_shard_attach"),
            ("src/db/shard.rs", "PeerShardAttachPlan"),
        ],
    },
    {
        "id": "bd-f6jfs.6",
        "criterion": "Per-shard audit-chain continuity and deterministic global timeline evidence",
        "artifacts": ["src/db/shard.rs", "src/db/mod.rs"],
        "needles": [
            ("src/db/shard.rs", "SHARD_FANOUT_MIGRATION_AUDIT_SCHEMA_V1"),
            ("src/db/mod.rs", "insert_audit_batch"),
        ],
    },
    {
        "id": "bd-f6jfs.7",
        "criterion": "Backup/restore side-path parity evidence",
        "artifacts": ["src/core/backup.rs", "tests/e2e_backup_restore_roundtrip.rs"],
        "needles": [
            ("src/core/backup.rs", "manifest"),
            ("tests/e2e_backup_restore_roundtrip.rs", "restore"),
        ],
    },
    {
        "id": "bd-f6jfs.8",
        "criterion": "Concurrency e2e/benchmark throughput evidence",
        "artifacts": ["tests/shard_fanout_concurrency.rs"],
        "needles": [
            ("tests/shard_fanout_concurrency.rs", "ee.test_event.v1"),
            ("tests/shard_fanout_concurrency.rs", "SPEEDUP_GATE"),
            ("tests/shard_fanout_concurrency.rs", "enqueue_to_grant_ms"),
            ("tests/shard_fanout_concurrency.rs", "grant_to_commit_ms"),
        ],
    },
    {
        "id": "bd-f6jfs.9",
        "criterion": "Rollback/off-switch/fail-closed evidence",
        "artifacts": ["src/db/shard.rs", "docs/architecture/shard-fanout.md"],
        "needles": [
            ("src/db/shard.rs", "PRE_SHARD_FANOUT_FILE_NAME"),
            ("docs/architecture/shard-fanout.md", "Rollback And Off Switch"),
        ],
    },
]


def run_command(command: list[str], timeout: int = 20) -> dict[str, Any]:
    try:
        completed = subprocess.run(
            command,
            cwd=repo_root,
            check=False,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=timeout,
        )
        return {
            "command": command,
            "exitCode": completed.returncode,
            "stdout": completed.stdout.strip(),
            "stderr": completed.stderr.strip(),
            "timedOut": False,
        }
    except subprocess.TimeoutExpired as error:
        return {
            "command": command,
            "exitCode": None,
            "stdout": (error.stdout or "").strip() if isinstance(error.stdout, str) else "",
            "stderr": (error.stderr or "").strip() if isinstance(error.stderr, str) else "",
            "timedOut": True,
        }
    except FileNotFoundError as error:
        return {
            "command": command,
            "exitCode": None,
            "stdout": "",
            "stderr": str(error),
            "timedOut": False,
        }


def read_text(relative: str) -> str:
    return (repo_root / relative).read_text(encoding="utf-8")


def br_status(bead_id: str) -> dict[str, Any]:
    result = run_command(
        ["br", "show", bead_id, "--json", "--allow-stale", "--no-auto-import"],
        timeout=10,
    )
    status = None
    title = None
    if result["exitCode"] == 0 and result["stdout"]:
        try:
            payload = json.loads(result["stdout"])
            if isinstance(payload, list) and payload:
                status = payload[0].get("status")
                title = payload[0].get("title")
        except json.JSONDecodeError:
            pass
    return {
        "id": bead_id,
        "title": title,
        "status": status,
        "command": result["command"],
        "exitCode": result["exitCode"],
        "stderr": result["stderr"],
    }


def artifact_state(relative: str) -> dict[str, Any]:
    path = repo_root / relative
    exists = path.exists()
    size = path.stat().st_size if exists else None
    return {"path": relative, "exists": exists, "size": size}


def needle_state(relative: str, needle: str) -> dict[str, Any]:
    path = repo_root / relative
    if not path.exists():
        return {"path": relative, "needle": needle, "present": False}
    return {"path": relative, "needle": needle, "present": needle in read_text(relative)}


def row_for_child(child: dict[str, Any]) -> dict[str, Any]:
    bead = br_status(child["id"])
    artifacts = [artifact_state(path) for path in child["artifacts"]]
    needles = [needle_state(path, needle) for path, needle in child["needles"]]
    missing_artifacts = [item["path"] for item in artifacts if not item["exists"]]
    missing_needles = [
        f"{item['path']}::{item['needle']}" for item in needles if not item["present"]
    ]
    closed = bead["status"] == "closed"
    evidence_present = not missing_artifacts and not missing_needles
    if closed and evidence_present:
        status = "pass"
    elif bead["status"] in {"open", "in_progress", "blocked", "deferred"}:
        status = "blocked"
    else:
        status = "missing_evidence"
    return {
        "child": bead,
        "criterion": child["criterion"],
        "status": status,
        "artifacts": artifacts,
        "needles": needles,
        "missingArtifacts": missing_artifacts,
        "missingNeedles": missing_needles,
    }


matrix = [row_for_child(child) for child in CHILDREN]
# Graph hygiene proof: br dep cycles plus bv robot insights.
cycles = run_command(["br", "dep", "cycles", "--json", "--allow-stale", "--no-auto-import"], 20)
bv_insights = run_command(["bv", "-f", "json", "--robot-insights"], 30)
rch_queue = run_command(["rch", "queue"], 20)
rch_status = run_command(["rch", "status"], 20)

cycle_count = None
if cycles["exitCode"] == 0 and cycles["stdout"]:
    try:
        cycle_count = json.loads(cycles["stdout"]).get("count")
    except json.JSONDecodeError:
        cycle_count = None

bv_anomaly = None
if bv_insights["timedOut"]:
    bv_anomaly = "bv_robot_insights_timeout"
elif bv_insights["exitCode"] not in (0,):
    bv_anomaly = "bv_robot_insights_failed"

rch_text = "\n".join([rch_queue["stdout"], rch_status["stdout"], rch_status["stderr"]])
rch_remote_ready = "Posture : remote-ready" in rch_text or "remote-ready" in rch_text
rch_critical_pressure = "critical pressure" in rch_text or "failed preflight" in rch_text
full_verification_status = "not_run"
if not rch_remote_ready or rch_critical_pressure:
    full_verification_status = "blocked_by_rch_topology"

blocked_rows = [row for row in matrix if row["status"] != "pass"]
graph_hygiene_status = "pass" if cycle_count == 0 and bv_anomaly is None else "blocked"
can_close = not blocked_rows and graph_hygiene_status == "pass" and full_verification_status == "pass"

payload = {
    "schema": "ee.shard_fanout.closeout_matrix.v1",
    "beadId": "bd-f6jfs.10",
    "parentBeadId": "bd-f6jfs",
    "canClose": can_close,
    "overallStatus": "pass" if can_close else "blocked",
    "matrix": matrix,
    "graphHygiene": {
        "status": graph_hygiene_status,
        "brDepCycles": {
            "count": cycle_count,
            "exitCode": cycles["exitCode"],
            "stderr": cycles["stderr"],
        },
        "bvRobotInsights": {
            "status": "pass" if bv_anomaly is None else bv_anomaly,
            "exitCode": bv_insights["exitCode"],
            "timedOut": bv_insights["timedOut"],
            "stderr": bv_insights["stderr"],
        },
    },
    "rchVerification": {
        "status": full_verification_status,
        "commands": FULL_RCH_COMMANDS,
        "queue": {
            "exitCode": rch_queue["exitCode"],
            "stdout": rch_queue["stdout"],
            "stderr": rch_queue["stderr"],
        },
        "statusCommand": {
            "exitCode": rch_status["exitCode"],
            "stdout": rch_status["stdout"],
            "stderr": rch_status["stderr"],
        },
        "localCargoAllowed": False,
    },
    "residualBlockers": [
        {
            "childId": row["child"]["id"],
            "childStatus": row["child"]["status"],
            "criterion": row["criterion"],
            "missingArtifacts": row["missingArtifacts"],
            "missingNeedles": row["missingNeedles"],
        }
        for row in blocked_rows
    ],
}

if fmt == "markdown":
    print("# bd-f6jfs.10 Closeout Matrix")
    print()
    print(f"- Overall status: `{payload['overallStatus']}`")
    print(f"- Can close: `{str(payload['canClose']).lower()}`")
    print(f"- Graph hygiene: `{graph_hygiene_status}`")
    print(f"- RCH verification: `{full_verification_status}`")
    print()
    print("| Child | Status | Criterion |")
    print("| --- | --- | --- |")
    for row in matrix:
        print(
            f"| `{row['child']['id']}` | `{row['status']}` / `{row['child']['status']}` | {row['criterion']} |"
        )
    if payload["residualBlockers"]:
        print()
        print("## Residual Blockers")
        for blocker in payload["residualBlockers"]:
            print(
                f"- `{blocker['childId']}` is `{blocker['childStatus']}`: {blocker['criterion']}"
            )
else:
    print(json.dumps(payload, indent=2, sort_keys=True))

if payload["canClose"] or allow_incomplete:
    raise SystemExit(0)
raise SystemExit(1)
PY
