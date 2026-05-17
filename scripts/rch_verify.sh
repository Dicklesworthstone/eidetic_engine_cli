#!/usr/bin/env bash
# RCHVC.1 - stable remote verification wrapper for focused Rust checks.
#
# This script is intentionally repo-local. It makes the explicit RCH path the
# easy path for agents and emits a JSON proof that can be pasted into Beads.

set -euo pipefail

usage() {
    cat <<'EOF'
Usage: scripts/rch_verify.sh [options] -- <verifier command...>

Options:
  --dry-run                 Do not execute; emit the planned explicit rch exec proof
  --allow-raw               Allow non-Cargo commands; still runs through rch exec
  --bead-id <id>            Optional bead id for ledger rows and summaries
  --ledger <path>           Append one derived JSONL evidence row
  --event-log <path>        Append one ee.test_event.v1 command_end event row
  --summary                 Include bead-ready Markdown summary in the JSON proof
  --no-write                Do not write --ledger; render proof/summary only
  --rch-bin <path>          RCH binary (default: $RCH_BIN or rch)
  --project-root <path>     Local project root (default: cwd)
  --env <NAME=VALUE>        Pass an explicit environment override to the remote verifier command
  --require-clean-tree      Refuse before RCH when the git checkout is dirty
  --committed-tree          Verify the committed --treeish from a generated source export when safe
  --treeish <ref>           Committed-tree ref to prove (default: HEAD)
  --json                    Accepted for symmetry; output is always JSON
  -h, --help                Show this help

Accepted Cargo verifier shapes:
  cargo check ...
  cargo test ...
  cargo bench ...
  cargo clippy ...
  cargo fmt --check ...
EOF
}

DRY_RUN=0
ALLOW_RAW=0
BEAD_ID=""
LEDGER_PATH=""
EVENT_LOG_PATH=""
INCLUDE_SUMMARY=0
NO_WRITE=0
ENV_OVERRIDES=()
REQUIRE_CLEAN_TREE=0
COMMITTED_TREE=0
TREEISH="HEAD"
DEFAULT_RCH_BIN="/Users/jemanuel/projects/remote_compilation_helper/target-local/release/rch"
if [ -z "${RCH_BIN:-}" ] && [ -x "$DEFAULT_RCH_BIN" ]; then
    RCH_BIN="$DEFAULT_RCH_BIN"
elif [ -z "${RCH_BIN:-}" ]; then
    RCH_BIN="rch"
fi
PROJECT_ROOT="$PWD"

validate_env_override() {
    local item="${1:?environment override required}"
    local name="${item%%=*}"
    if [ "$item" = "$name" ] || [ -z "$name" ]; then
        echo "rch_verify: --env requires NAME=VALUE, got: $item" >&2
        exit 2
    fi
    case "$name" in
        [A-Za-z_]*)
            case "$name" in
                *[!A-Za-z0-9_]*)
                    echo "rch_verify: invalid --env name: $name" >&2
                    exit 2
                    ;;
            esac
            ;;
        *)
            echo "rch_verify: invalid --env name: $name" >&2
            exit 2
            ;;
    esac
    printf '%s' "$item"
}

while [ "$#" -gt 0 ]; do
    case "$1" in
        --dry-run) DRY_RUN=1; shift ;;
        --allow-raw) ALLOW_RAW=1; shift ;;
        --bead-id) BEAD_ID="${2:?--bead-id requires a value}"; shift 2 ;;
        --ledger) LEDGER_PATH="${2:?--ledger requires a value}"; shift 2 ;;
        --event-log) EVENT_LOG_PATH="${2:?--event-log requires a value}"; shift 2 ;;
        --summary) INCLUDE_SUMMARY=1; shift ;;
        --no-write) NO_WRITE=1; shift ;;
        --rch-bin) RCH_BIN="${2:?--rch-bin requires a value}"; shift 2 ;;
        --project-root) PROJECT_ROOT="${2:?--project-root requires a value}"; shift 2 ;;
        --env) ENV_OVERRIDES+=("$(validate_env_override "${2:?--env requires NAME=VALUE}")"); shift 2 ;;
        --require-clean-tree) REQUIRE_CLEAN_TREE=1; shift ;;
        --committed-tree) COMMITTED_TREE=1; shift ;;
        --treeish) TREEISH="${2:?--treeish requires a value}"; shift 2 ;;
        --json) shift ;;
        -h|--help) usage; exit 0 ;;
        --) shift; break ;;
        *)
            echo "rch_verify: unknown argument: $1" >&2
            usage >&2
            exit 2
            ;;
    esac
done

if [ "$#" -eq 0 ]; then
    echo "rch_verify: verifier command is required after --" >&2
    usage >&2
    exit 2
fi

COMMAND=("$@")

command_string() {
    local out="" arg
    for arg in "$@"; do
        if [ -z "$out" ]; then
            out="$arg"
        else
            out="$out $arg"
        fi
    done
    printf '%s' "$out"
}

contains_forbidden_text() {
    local text
    text="$(command_string "$@")"
    case "$text" in
        *"rm -rf"*|*"rm -f"*|*"git reset"*|*"git clean"*|*"git checkout"*|*"git stash"*|*"mkfs"*|*" dd "*|*"drop database"*)
            return 0
            ;;
        *)
            return 1
            ;;
    esac
}

classify_command() {
    if [ "${COMMAND[0]}" != "cargo" ]; then
        if [ "$ALLOW_RAW" -eq 1 ]; then
            printf 'raw'
            return 0
        fi
        printf 'rejected'
        return 0
    fi

    local subcommand="${COMMAND[1]:-}"
    case "$subcommand" in
        check) printf 'cargo_check' ;;
        test) printf 'cargo_test' ;;
        bench) printf 'cargo_bench' ;;
        clippy) printf 'cargo_clippy' ;;
        fmt)
            local arg
            for arg in "${COMMAND[@]}"; do
                if [ "$arg" = "--check" ]; then
                    printf 'cargo_fmt_check'
                    return 0
                fi
            done
            printf 'rejected'
            ;;
        *) printf 'rejected' ;;
    esac
}

json_array() {
    python3 -c 'import json, sys; print(json.dumps(sys.argv[1:], separators=(",", ":")))' "$@"
}

json_quote() {
    python3 -c 'import json, sys; print(json.dumps(sys.argv[1]))' "$1"
}

csv_json_array() {
    CSV_INPUT="${1:-}" python3 - <<'PY'
import json
import os

seen = []
for item in os.environ.get("CSV_INPUT", "").split(","):
    item = item.strip()
    if item and item not in seen:
        seen.append(item)
print(json.dumps(seen, separators=(",", ":")))
PY
}

csv_contains() {
    CSV_INPUT="${1:-}" CSV_NEEDLE="${2:-}" python3 - <<'PY'
import os
import sys

needle = os.environ.get("CSV_NEEDLE", "").strip()
items = {
    item.strip()
    for item in os.environ.get("CSV_INPUT", "").split(",")
    if item.strip()
}
sys.exit(0 if needle and needle in items else 1)
PY
}

tail_text() {
    python3 -c 'import sys; text=sys.stdin.read(); print(text[-4000:])'
}

extract_worker_id() {
    sed -n \
        -e 's/^.*Selected worker: \([A-Za-z0-9_.-][A-Za-z0-9_.-]*\) .*/\1/p' \
        -e 's/^\[RCH\] remote \([A-Za-z0-9_.-][A-Za-z0-9_.-]*\) (.*/\1/p' \
        -e 's/^\[RCH\] remote \([A-Za-z0-9_.-][A-Za-z0-9_.-]*\) failed.*/\1/p' \
        | tail -n 1
}

is_worker_disk_full_output() {
    grep -Eiq "No space left on device|disk full|ENOSPC"
}

configured_workers() {
    CONFIGURED_WORKERS="${RCH_VERIFY_CONFIGURED_WORKERS:-}" \
    FAKE_OUTPUT_PRESENT="${RCH_VERIFY_FAKE_OUTPUT:+1}" \
    RCH_BIN_PATH="$RCH_BIN" \
    python3 - <<'PY'
import json
import os
import subprocess

explicit = os.environ.get("CONFIGURED_WORKERS", "")

def emit(ids):
    seen = []
    for item in ids:
        item = item.strip()
        if item and item not in seen:
            seen.append(item)
    print(",".join(seen))

if explicit:
    emit(explicit.split(","))
    raise SystemExit(0)

if os.environ.get("FAKE_OUTPUT_PRESENT"):
    print("")
    raise SystemExit(0)

try:
    listed = subprocess.run(
        [os.environ["RCH_BIN_PATH"], "workers", "list", "--json"],
        check=False,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        text=True,
        timeout=10,
    )
    payload = json.loads(listed.stdout)
    workers = payload.get("data", {}).get("workers", [])
    emit(worker.get("id", "") for worker in workers)
except Exception:
    print("")
PY
}

daemon_workers() {
    DAEMON_WORKERS="${RCH_VERIFY_DAEMON_WORKERS:-}" \
    FAKE_OUTPUT_PRESENT="${RCH_VERIFY_FAKE_OUTPUT:+1}" \
    RCH_BIN_PATH="$RCH_BIN" \
    python3 - <<'PY'
import json
import os
import subprocess

explicit = os.environ.get("DAEMON_WORKERS", "")

def emit(ids):
    seen = []
    for item in ids:
        item = item.strip()
        if item and item not in seen:
            seen.append(item)
    print(",".join(seen))

if explicit:
    emit(explicit.split(","))
    raise SystemExit(0)

if os.environ.get("FAKE_OUTPUT_PRESENT"):
    print("")
    raise SystemExit(0)

try:
    status = subprocess.run(
        [os.environ["RCH_BIN_PATH"], "status", "--workers", "--jobs", "--json"],
        check=False,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        text=True,
        timeout=10,
    )
    payload = json.loads(status.stdout)
    workers = payload.get("data", {}).get("daemon", {}).get("workers", [])
    emit(worker.get("id", "") for worker in workers)
except Exception:
    print("")
PY
}

stale_disk_full_daemon_workers() {
    CONFIGURED_WORKERS="${1:-}" \
    DAEMON_WORKERS="${2:-}" \
    DISK_FULL_WORKERS="${3:-}" \
    python3 - <<'PY'
import os

configured = {
    item.strip()
    for item in os.environ.get("CONFIGURED_WORKERS", "").split(",")
    if item.strip()
}
daemon = [
    item.strip()
    for item in os.environ.get("DAEMON_WORKERS", "").split(",")
    if item.strip()
]
disk_full = {
    item.strip()
    for item in os.environ.get("DISK_FULL_WORKERS", "").split(",")
    if item.strip()
}
stale = [
    item
    for item in daemon
    if item not in configured and item in disk_full
]
print(",".join(dict.fromkeys(stale)))
PY
}

recent_failed_excluded_daemon_workers() {
    CONFIGURED_WORKERS="${1:-}" \
    DAEMON_WORKERS="${2:-}" \
    RECENT_FAILURE_MAX_MS="${3:-${RCH_VERIFY_RECENT_FAILURE_MAX_MS:-10000}}" \
    STATUS_JSON="${RCH_VERIFY_STATUS_JSON:-}" \
    FAKE_OUTPUT_PRESENT="${RCH_VERIFY_FAKE_OUTPUT:+1}" \
    RCH_BIN_PATH="$RCH_BIN" \
    python3 - <<'PY'
import json
import os
import subprocess

configured = {
    item.strip()
    for item in os.environ.get("CONFIGURED_WORKERS", "").split(",")
    if item.strip()
}
daemon = {
    item.strip()
    for item in os.environ.get("DAEMON_WORKERS", "").split(",")
    if item.strip()
}

try:
    max_duration_ms = int(os.environ.get("RECENT_FAILURE_MAX_MS") or "10000")
except ValueError:
    max_duration_ms = 10000

status_json = os.environ.get("STATUS_JSON", "")
if not status_json and os.environ.get("FAKE_OUTPUT_PRESENT"):
    print("")
    raise SystemExit(0)

try:
    if status_json:
        payload = json.loads(status_json)
    else:
        result = subprocess.run(
            [os.environ["RCH_BIN_PATH"], "status", "--workers", "--jobs", "--json"],
            check=False,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            text=True,
            timeout=10,
        )
        payload = json.loads(result.stdout)
except Exception:
    print("")
    raise SystemExit(0)

recent = payload.get("data", {}).get("daemon", {}).get("recent_builds", [])
stale = []
for build in recent:
    worker = str(build.get("worker_id") or "").strip()
    if not worker or worker in configured or worker not in daemon:
        continue
    exit_code = build.get("exit_code")
    try:
        duration_ms = int(build.get("duration_ms") or 0)
    except (TypeError, ValueError):
        duration_ms = 0
    if exit_code not in (None, 0) and 0 < duration_ms <= max_duration_ms:
        if worker not in stale:
            stale.append(worker)

print(",".join(stale))
PY
}

healthy_alternate_workers() {
    local failed_worker="${1:?failed worker required}"
    local allowed_workers="${2:-}"
    HEALTHY_WORKERS="${RCH_VERIFY_HEALTHY_WORKERS:-}" \
    ALLOWED_WORKERS="$allowed_workers" \
    FAILED_WORKER="$failed_worker" \
    RCH_BIN_PATH="$RCH_BIN" \
    python3 - <<'PY'
import json
import os
import subprocess

failed = os.environ["FAILED_WORKER"]
explicit = os.environ.get("HEALTHY_WORKERS", "")
allowed_raw = os.environ.get("ALLOWED_WORKERS", "")
allowed = [
    item.strip()
    for item in allowed_raw.split(",")
    if item.strip()
]

def emit(ids):
    seen = []
    for item in ids:
        item = item.strip()
        if allowed and item not in allowed:
            continue
        if item and item != failed and item not in seen:
            seen.append(item)
    print(",".join(seen))

if explicit:
    emit(explicit.split(","))
    raise SystemExit(0)

rch_bin = os.environ["RCH_BIN_PATH"]
try:
    status = subprocess.run(
        [rch_bin, "status", "--workers", "--jobs", "--json"],
        check=False,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        text=True,
        timeout=10,
    )
    payload = json.loads(status.stdout)
    workers = payload.get("data", {}).get("daemon", {}).get("workers", [])
    healthy = [
        worker.get("id", "")
        for worker in workers
        if worker.get("status") == "healthy"
    ]
    if healthy:
        emit(healthy)
        raise SystemExit(0)
except Exception:
    pass

try:
    listed = subprocess.run(
        [rch_bin, "workers", "list", "--json"],
        check=False,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        text=True,
        timeout=10,
    )
    payload = json.loads(listed.stdout)
    workers = payload.get("data", {}).get("workers", [])
    emit(worker.get("id", "") for worker in workers)
except Exception:
    print("")
PY
}

critical_checkout_manifest() {
    GIT_LS_FILES="${RCH_VERIFY_GIT_LS_FILES:-}" \
    PROJECT_ROOT_PATH="$PROJECT_ROOT" \
    python3 - <<'PY'
import os
import subprocess

explicit = os.environ.get("GIT_LS_FILES", "")
project_root = os.environ["PROJECT_ROOT_PATH"]

if explicit:
    tracked = explicit.splitlines()
else:
    try:
        result = subprocess.run(
            ["git", "-C", project_root, "ls-files"],
            check=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            text=True,
            timeout=10,
        )
        tracked = result.stdout.splitlines()
    except Exception:
        tracked = []

critical = set()
for path in tracked:
    path = path.strip()
    if not path:
        continue
    if path in {"src/lib.rs", "src/main.rs"}:
        critical.add(path)
        continue
    if path.startswith("src/") and path.endswith(".rs"):
        parts = path.split("/")
        if len(parts) == 2 or (len(parts) == 3 and parts[2] == "mod.rs"):
            critical.add(path)

for path in sorted(critical):
    print(path)
PY
}

compute_source_state_json() {
    PROJECT_ROOT_PATH="$PROJECT_ROOT" \
    REQUIRE_CLEAN_TREE="$REQUIRE_CLEAN_TREE" \
    python3 - <<'PY'
import hashlib
import json
import os
import subprocess

project_root = os.environ["PROJECT_ROOT_PATH"]
require_clean = os.environ.get("REQUIRE_CLEAN_TREE") == "1"

def git(args):
    try:
        return subprocess.run(
            ["git", "-C", project_root, *args],
            check=False,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            text=True,
            timeout=10,
        )
    except Exception:
        return None

def git_stdout(args):
    result = git(args)
    if result is None or result.returncode != 0:
        return None
    return result.stdout.strip()

def path_from_porcelain_v2(line):
    if line.startswith("? ") or line.startswith("! "):
        return line[2:].strip()
    if line.startswith("#"):
        return ""
    if "\t" in line:
        return line.rsplit("\t", 1)[-1].strip()
    parts = line.split()
    return parts[-1] if parts else ""

def status_kind(line, path):
    if path == ".beads/issues.jsonl" or path.startswith(".beads/"):
        return "beads"
    if line.startswith("? "):
        name = path.rsplit("/", 1)[-1]
        if (
            path in {"--help", ".plan-drift-report.json", "critical.json", "functions.txt"}
            or name.startswith("ubs")
            or name.startswith("test_ln_")
            or name.startswith("test_multibyte")
        ):
            return "scratch"
        if any(token in path.lower() for token in ("secret", "token", "credential", "password")):
            return "secret_risk"
        return "untracked"
    if line.startswith("! "):
        return "ignored"
    return "tracked"

def tracked_state(line):
    if not (line.startswith("1 ") or line.startswith("2 ") or line.startswith("u ")):
        return False, False
    xy = line[2:4]
    if len(xy) != 2:
        return False, False
    return xy[0] != ".", xy[1] != "."

head = git_stdout(["rev-parse", "HEAD"])
tree = git_stdout(["rev-parse", "HEAD^{tree}"]) if head else None
status = git(["status", "--porcelain=v2", "--untracked-files=all", "--ignored=no"])
status_lines = []
if status is not None and status.returncode == 0:
    status_lines = [line.rstrip("\n") for line in status.stdout.splitlines() if line.strip()]

normalized = "\n".join(sorted(status_lines))
dirty_hash = "sha256:" + hashlib.sha256(normalized.encode("utf-8")).hexdigest()
summary = {
    "total": 0,
    "tracked": 0,
    "tracked_staged": 0,
    "tracked_unstaged": 0,
    "untracked": 0,
    "beads": 0,
    "scratch": 0,
    "secret_risk": 0,
    "ignored": 0,
    "unknown": 0,
}

sample = []
for line in sorted(status_lines):
    path = path_from_porcelain_v2(line)
    if not path:
        continue
    kind = status_kind(line, path)
    if kind not in summary:
        kind = "unknown"
    summary["total"] += 1
    summary[kind] += 1
    staged, unstaged = tracked_state(line)
    if kind == "tracked" and staged:
        summary["tracked_staged"] += 1
    if kind == "tracked" and unstaged:
        summary["tracked_unstaged"] += 1
    if len(sample) < 12:
        item = {"path": path, "kind": kind}
        if kind == "tracked":
            item["staged"] = staged
            item["unstaged"] = unstaged
        sample.append(item)

source_codes = []
if require_clean and summary["total"]:
    source_codes.append("rch_verify_dirty_tree_refused")
    if summary["tracked"]:
        source_codes.append("rch_verify_dirty_tracked_paths")
    if summary["tracked_staged"]:
        source_codes.append("rch_verify_dirty_staged_paths")
    if summary["tracked_unstaged"]:
        source_codes.append("rch_verify_dirty_unstaged_paths")
    if summary["beads"]:
        source_codes.append("rch_verify_dirty_beads_metadata")
    if summary["scratch"]:
        source_codes.append("rch_verify_dirty_untracked_scratch")
    if summary["untracked"] or summary["secret_risk"] or summary["unknown"]:
        source_codes.append("rch_verify_dirty_untracked_paths")

print(json.dumps({
    "verification_attribution": "strict_clean_tree" if require_clean and not summary["total"] else "live_dirty_checkout",
    "git_head": head,
    "git_tree": tree,
    "dirty_status_hash": dirty_hash,
    "dirty_summary": summary,
    "dirty_paths_sample": sample,
    "source_state_degraded_codes": source_codes,
}, sort_keys=True, separators=(",", ":")))
PY
}

compute_committed_tree_state_json() {
    PROJECT_ROOT_PATH="$PROJECT_ROOT" \
    REQUESTED_TREEISH="$TREEISH" \
    python3 - <<'PY'
import hashlib
import json
import os
import subprocess

project_root = os.environ["PROJECT_ROOT_PATH"]
treeish = os.environ.get("REQUESTED_TREEISH") or "HEAD"

def git(args):
    try:
        return subprocess.run(
            ["git", "-C", project_root, *args],
            check=False,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            timeout=10,
        )
    except Exception as error:
        return subprocess.CompletedProcess(args, 1, "", str(error))

def empty_state(codes):
    return {
        "verification_attribution": "committed_tree",
        "git_head": None,
        "git_tree": None,
        "dirty_status_hash": "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        "dirty_summary": {
            "total": 0,
            "tracked": 0,
            "untracked": 0,
            "beads": 0,
            "scratch": 0,
            "secret_risk": 0,
            "ignored": 0,
            "unknown": 0,
        },
        "dirty_paths_sample": [],
        "source_state_degraded_codes": codes,
        "requested_treeish": treeish,
        "resolved_commit": None,
        "source_manifest_hash": None,
        "source_manifest_file_count": 0,
        "source_manifest_byte_count": 0,
        "source_manifest_excluded_path_classes": ["dirty_tracked", "untracked", "ignored"],
    }

commit_result = git(["rev-parse", "--verify", "--quiet", f"{treeish}^{{commit}}"])
if commit_result.returncode != 0:
    print(json.dumps(empty_state([
        "rch_verify_committed_tree_ref_unresolved",
        "rch_verify_committed_tree_unsupported",
    ]), sort_keys=True, separators=(",", ":")))
    raise SystemExit(0)

commit = commit_result.stdout.strip()
tree_result = git(["rev-parse", "--verify", "--quiet", f"{commit}^{{tree}}"])
if tree_result.returncode != 0:
    print(json.dumps(empty_state([
        "rch_verify_committed_tree_ref_unresolved",
        "rch_verify_committed_tree_unsupported",
    ]), sort_keys=True, separators=(",", ":")))
    raise SystemExit(0)
tree = tree_result.stdout.strip()

ls_tree = subprocess.run(
    ["git", "-C", project_root, "ls-tree", "-r", "-l", "-z", "--full-tree", commit],
    check=False,
    stdout=subprocess.PIPE,
    stderr=subprocess.DEVNULL,
    timeout=10,
)

entries = []
byte_count = 0
if ls_tree.returncode == 0:
    for raw in ls_tree.stdout.split(b"\0"):
        if not raw:
            continue
        meta, _, raw_path = raw.partition(b"\t")
        parts = meta.decode("utf-8", "replace").split()
        if len(parts) < 4:
            continue
        mode, kind, object_id, size_text = parts[:4]
        path = raw_path.decode("utf-8", "replace")
        try:
            size = int(size_text)
        except ValueError:
            size = 0
        byte_count += max(size, 0)
        entries.append((path, mode, kind, object_id, size))

manifest = "\n".join(
    f"{path}\0{mode}\0{kind}\0{object_id}\0{size}"
    for path, mode, kind, object_id, size in sorted(entries)
)
manifest_hash = "sha256:" + hashlib.sha256(manifest.encode("utf-8")).hexdigest()

codes = []
show_cargo = git(["show", f"{commit}:Cargo.toml"])
if show_cargo.returncode == 0 and "path" in show_cargo.stdout and "path =" in show_cargo.stdout:
    codes.append("rch_verify_committed_tree_unsupported")
    codes.append("rch_verify_committed_tree_path_deps_unsupported")

print(json.dumps({
    "verification_attribution": "committed_tree",
    "git_head": commit,
    "git_tree": tree,
    "dirty_status_hash": "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
    "dirty_summary": {
        "total": 0,
        "tracked": 0,
        "untracked": 0,
        "beads": 0,
        "scratch": 0,
        "secret_risk": 0,
        "ignored": 0,
        "unknown": 0,
    },
    "dirty_paths_sample": [],
    "source_state_degraded_codes": codes,
    "requested_treeish": treeish,
    "resolved_commit": commit,
    "source_manifest_hash": manifest_hash,
    "source_manifest_file_count": len(entries),
    "source_manifest_byte_count": byte_count,
    "source_manifest_excluded_path_classes": ["dirty_tracked", "untracked", "ignored"],
}, sort_keys=True, separators=(",", ":")))
PY
}

json_field_string() {
    JSON_INPUT="${1:?json input required}" \
    JSON_FIELD="${2:?json field required}" \
    python3 - <<'PY'
import json
import os

payload = json.loads(os.environ["JSON_INPUT"])
value = payload.get(os.environ["JSON_FIELD"])
print("" if value is None else str(value))
PY
}

materialize_committed_tree() {
    local commit export_base export_root short_commit
    commit="$(json_field_string "$SOURCE_STATE_JSON" "resolved_commit")"
    if [ -z "$commit" ]; then
        echo "rch_verify: committed-tree materialization missing resolved commit" >&2
        return 1
    fi

    short_commit="${commit:0:12}"
    export_base="${RCH_VERIFY_COMMITTED_TREE_BASE:-${TMPDIR:-/tmp}/ee-rch-committed-tree}"
    mkdir -p "$export_base"
    export_root="$(mktemp -d "$export_base/$short_commit.XXXXXX")"

    git -C "$PROJECT_ROOT" archive --format=tar "$commit" | tar -x -f - -C "$export_root"
    PROJECT_ROOT="$export_root"
    REMOTE_PROJECT_ROOT="/data/projects/$(basename "$PROJECT_ROOT")"
    REMOTE_PROJECT_ROOT_JSON="$(json_quote "$REMOTE_PROJECT_ROOT")"
}

remote_checkout_missing_tracked_paths() {
    CHECKOUT_OUTPUT="${1:-}" \
    CRITICAL_MANIFEST="$(critical_checkout_manifest)" \
    python3 - <<'PY'
import os
import re

ansi = re.compile(r"\x1b\[[0-9;]*m")
text = ansi.sub("", os.environ.get("CHECKOUT_OUTPUT", ""))
manifest = {
    line.strip()
    for line in os.environ.get("CRITICAL_MANIFEST", "").splitlines()
    if line.strip()
}

if "E0583" not in text:
    raise SystemExit(0)

candidates = []
for match in re.finditer(r'"(src/[^"]+\.rs)"', text):
    candidates.append(match.group(1))

missing = []
for path in candidates:
    if path in manifest and path not in missing:
        missing.append(path)

print(",".join(missing))
PY
}

run_rch_invocation_once() {
    if [ -n "${RCH_VERIFY_FAKE_OUTPUT:-}" ]; then
        printf '%s' "$RCH_VERIFY_FAKE_OUTPUT"
        return "${RCH_VERIFY_FAKE_EXIT_CODE:-0}"
    fi

    cd "$PROJECT_ROOT" && \
        RCH_WORKERS="${RCH_WORKERS:-}" \
        RCH_COMPRESSION="${RCH_COMPRESSION:-0}" \
        RCH_REQUIRE_REMOTE=1 \
        RCH_QUEUE_WHEN_BUSY="${RCH_QUEUE_WHEN_BUSY:-1}" \
        RCH_TEST_SLOTS="${RCH_TEST_SLOTS:-2}" \
        RCH_DAEMON_WAIT_RESPONSE_TIMEOUT_SECS="${RCH_DAEMON_WAIT_RESPONSE_TIMEOUT_SECS:-900}" \
        RCH_DAEMON_RESPONSE_TIMEOUT_SECS="${RCH_DAEMON_RESPONSE_TIMEOUT_SECS:-900}" \
        RCH_CANONICAL_PROJECT_ROOT="${RCH_CANONICAL_PROJECT_ROOT:-$(dirname "$PROJECT_ROOT")}" \
        RCH_ALIAS_PROJECT_ROOT="${RCH_ALIAS_PROJECT_ROOT:-/data/projects}" \
        RCH_VISIBILITY="${RCH_VISIBILITY:-summary}" \
        "${RCH_INVOCATION[@]}" 2>&1
}

run_rch_invocation_retry() {
    local preferred_workers="${1:?preferred workers required}"
    if [ -n "${RCH_VERIFY_FAKE_RETRY_OUTPUT:-}" ]; then
        printf '%s' "$RCH_VERIFY_FAKE_RETRY_OUTPUT"
        return "${RCH_VERIFY_FAKE_RETRY_EXIT_CODE:-0}"
    fi

    cd "$PROJECT_ROOT" && \
        RCH_WORKERS="$preferred_workers" \
        RCH_COMPRESSION="${RCH_COMPRESSION:-0}" \
        RCH_REQUIRE_REMOTE=1 \
        RCH_QUEUE_WHEN_BUSY="${RCH_QUEUE_WHEN_BUSY:-1}" \
        RCH_TEST_SLOTS="${RCH_TEST_SLOTS:-2}" \
        RCH_DAEMON_WAIT_RESPONSE_TIMEOUT_SECS="${RCH_DAEMON_WAIT_RESPONSE_TIMEOUT_SECS:-900}" \
        RCH_DAEMON_RESPONSE_TIMEOUT_SECS="${RCH_DAEMON_RESPONSE_TIMEOUT_SECS:-900}" \
        RCH_CANONICAL_PROJECT_ROOT="${RCH_CANONICAL_PROJECT_ROOT:-$(dirname "$PROJECT_ROOT")}" \
        RCH_ALIAS_PROJECT_ROOT="${RCH_ALIAS_PROJECT_ROOT:-/data/projects}" \
        RCH_VISIBILITY="${RCH_VISIBILITY:-summary}" \
        "${RCH_INVOCATION[@]}" 2>&1
}

now_iso() {
    if [ -n "${RCH_VERIFY_NOW:-}" ]; then
        printf '%s' "$RCH_VERIFY_NOW"
    else
        python3 -c 'from datetime import datetime, timezone; print(datetime.now(timezone.utc).isoformat(timespec="microseconds").replace("+00:00","Z"))'
    fi
}

now_ms() {
    python3 -c 'import time; print(int(time.time() * 1000))'
}

RUN_STARTED_AT="$(now_iso)"

emit_json() {
    local success="$1"
    local exit_code_json="$2"
    local elapsed_ms="$3"
    local stdout_tail="$4"
    local stderr_tail="$5"
    shift 5
    local degraded_codes_json
    degraded_codes_json="$(json_array "$@")"
    local command_json rch_invocation_json command_text_json remote_env_json stdout_json stderr_json requested_workers_json configured_workers_json daemon_workers_json
    command_json="$(json_array "${COMMAND[@]}")"
    rch_invocation_json="$(json_array "${RCH_INVOCATION[@]}")"
    remote_env_json="$(json_array "${ENV_OVERRIDES[@]}")"
    command_text_json="$(json_quote "$(command_string "${ENV_OVERRIDES[@]}" "${COMMAND[@]}")")"
    stdout_json="$(json_quote "$stdout_tail")"
    stderr_json="$(json_quote "$stderr_tail")"
    requested_workers_json="$(csv_json_array "${REQUESTED_WORKERS_CSV:-}")"
    configured_workers_json="$(csv_json_array "${CONFIGURED_WORKERS_CSV:-}")"
    daemon_workers_json="$(csv_json_array "${DAEMON_WORKERS_CSV:-}")"
    local source_state_json
    if [ -n "${SOURCE_STATE_JSON:-}" ]; then
        source_state_json="$SOURCE_STATE_JSON"
    else
        source_state_json='{"verification_attribution":"live_dirty_checkout","git_head":null,"git_tree":null,"dirty_status_hash":"sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855","dirty_summary":{"total":0,"tracked":0,"untracked":0,"beads":0,"scratch":0,"secret_risk":0,"ignored":0,"unknown":0},"dirty_paths_sample":[],"source_state_degraded_codes":[]}'
    fi
    local json_payload
    json_payload="$(cat <<EOF
{"schema":"ee.rch.verify.v1","success":$success,"generated_at":"$(now_iso)","command":$command_json,"command_text":$command_text_json,"command_kind":"$COMMAND_KIND","remote_env":$remote_env_json,"remote_required":true,"would_offload":$WOULD_OFFLOAD,"worker_id":$WORKER_ID_JSON,"requested_workers":$requested_workers_json,"configured_workers":$configured_workers_json,"daemon_workers":$daemon_workers_json,"remote_project_root":$REMOTE_PROJECT_ROOT_JSON,"remote_target_dir":$REMOTE_TARGET_DIR_JSON,"exit_code":$exit_code_json,"elapsed_ms":$elapsed_ms,"stdout_tail":$stdout_json,"stderr_tail":$stderr_json,"degraded_codes":$degraded_codes_json,"rch_invocation":$rch_invocation_json,"source_state":$source_state_json}
EOF
)"
    JSON_PAYLOAD="$json_payload" \
    BEAD_ID="$BEAD_ID" \
    LEDGER_PATH="$LEDGER_PATH" \
    EVENT_LOG_PATH="$EVENT_LOG_PATH" \
    INCLUDE_SUMMARY="$INCLUDE_SUMMARY" \
    NO_WRITE="$NO_WRITE" \
    RUN_STARTED_AT="$RUN_STARTED_AT" \
    python3 - <<'PY'
import hashlib
import json
import os
import re
from pathlib import Path

proof = json.loads(os.environ["JSON_PAYLOAD"])
source_state = proof.pop("source_state", {})
for key in (
    "verification_attribution",
    "git_head",
    "git_tree",
    "dirty_status_hash",
    "dirty_summary",
    "dirty_paths_sample",
    "source_state_degraded_codes",
    "requested_treeish",
    "resolved_commit",
    "source_manifest_hash",
    "source_manifest_file_count",
    "source_manifest_byte_count",
    "source_manifest_excluded_path_classes",
):
    proof[key] = source_state.get(key)
bead_id = os.environ.get("BEAD_ID", "")
ledger_path = os.environ.get("LEDGER_PATH", "")
event_log_path = os.environ.get("EVENT_LOG_PATH", "")
include_summary = os.environ.get("INCLUDE_SUMMARY") == "1"
no_write = os.environ.get("NO_WRITE") == "1"
started_at = os.environ.get("RUN_STARTED_AT") or proof.get("generated_at")

def redact(text):
    if not text:
        return text
    text = re.sub(r"\x1b\[[0-9;]*m", "", text)
    text = re.sub(r"/Users/[^/\s]+", "/Users/<redacted>", text)
    text = re.sub(r"(?i)(token|secret|password|api[_-]?key)=\S+", r"\1=<redacted>", text)
    return text

def first_error_location(text):
    if not text:
        return (None, None)
    for line in text.splitlines():
        match = re.search(r"-->\s+([^:\s][^:]*):(\d+):\d+", line)
        if match:
            return (redact(match.group(1)), int(match.group(2)))
    return (None, None)

def error_codes(text):
    if not text:
        return []
    return sorted(set(re.findall(r"\bE\d{4}\b|RCH-E\d{3}\b", text)))

raw_stdout_tail = proof.get("stdout_tail") or ""
raw_stderr_tail = proof.get("stderr_tail") or ""
combined_tail = "\n".join(part for part in [raw_stdout_tail, raw_stderr_tail] if part)
proof["stdout_tail"] = redact(raw_stdout_tail)
proof["stderr_tail"] = redact(raw_stderr_tail)
first_error_file, first_error_line = first_error_location(combined_tail)
codes = error_codes(combined_tail)

exit_code = proof.get("exit_code")
degraded = list(proof.get("degraded_codes") or [])
source_state_degraded = list(proof.get("source_state_degraded_codes") or [])
source_state_code_set = set(source_state_degraded)

worker_state_code_set = {
    "rch_verify_capacity_or_timeout",
    "rch_verify_local_fallback_refused",
    "rch_verify_not_offloaded",
    "rch_verify_remote_checkout_incomplete",
    "rch_verify_remote_marker_missing",
    "rch_verify_retry_after_worker_disk_full",
    "rch_verify_topology_blocked",
    "rch_verify_worker_disk_full",
    "rch_verify_worker_filter_ignored",
    "rch_verify_worker_quarantine_ignored",
}
worker_state_degraded = [
    code
    for code in degraded
    if code in worker_state_code_set and code not in source_state_code_set
]
if proof.get("success") is not True:
    status = "refused"
elif exit_code is None:
    status = "dry_run"
elif exit_code == 0 and proof.get("worker_id"):
    status = "remote_pass"
elif exit_code == 0:
    status = "pass_without_remote_marker"
elif "rch_verify_committed_tree_unsupported" in degraded:
    status = "committed_tree_unsupported"
elif (
    "rch_verify_dirty_tree_refused" in degraded
):
    status = "source_state_refused"
elif (
    "rch_verify_topology_blocked" in degraded
    or "rch_verify_local_fallback_refused" in degraded
    or "rch_verify_worker_disk_full" in degraded
    or "rch_verify_worker_quarantine_ignored" in degraded
    or "rch_verify_worker_filter_ignored" in degraded
    or "rch_verify_remote_checkout_incomplete" in degraded
):
    status = "rch_environment_failure"
elif "rch_verify_capacity_or_timeout" in degraded:
    status = "capacity_or_timeout"
else:
    status = "remote_failure"

command_text = proof.get("command_text", "")
command_hash = hashlib.sha256(command_text.encode("utf-8")).hexdigest()
proof["status"] = status
proof["command_hash"] = command_hash
proof["started_at"] = started_at
proof["completed_at"] = proof.get("generated_at")
proof["first_error_file"] = first_error_file
proof["first_error_line"] = first_error_line
proof["error_codes"] = codes
proof["worker_state_degraded_codes"] = worker_state_degraded
if bead_id:
    proof["bead_id"] = bead_id

summary_lines = [
    f"RCH verifier `{command_text}` => `{status}`.",
    f"- command_kind: `{proof.get('command_kind')}`",
    f"- verification_attribution: `{proof.get('verification_attribution')}`",
    f"- git_head: `{proof.get('git_head') or 'unknown'}`",
    f"- git_tree: `{proof.get('git_tree') or 'unknown'}`",
    f"- dirty_status_hash: `{proof.get('dirty_status_hash') or 'unknown'}`",
    f"- remote_env: `{', '.join(proof.get('remote_env') or []) or 'none'}`",
    f"- remote_required: `{str(proof.get('remote_required')).lower()}`",
    f"- would_offload: `{str(proof.get('would_offload')).lower()}`",
    f"- worker_id: `{proof.get('worker_id') or 'unknown'}`",
    f"- exit_code: `{exit_code if exit_code is not None else 'not_run'}`",
    f"- elapsed_ms: `{proof.get('elapsed_ms')}`",
    f"- command_hash: `{command_hash}`",
]
for key in ("requested_workers", "configured_workers", "daemon_workers"):
    workers = proof.get(key) or []
    if workers:
        summary_lines.append(f"- {key}: `{', '.join(workers)}`")
if bead_id:
    summary_lines.insert(1, f"- bead_id: `{bead_id}`")
if first_error_file:
    summary_lines.append(f"- first_error: `{first_error_file}:{first_error_line}`")
if codes:
    summary_lines.append("- error_codes: `" + "`, `".join(codes) + "`")
if degraded:
    summary_lines.append("- degraded_codes: `" + "`, `".join(degraded) + "`")
else:
    summary_lines.append("- degraded_codes: none")
if source_state_degraded:
    summary_lines.append("- source_state_degraded_codes: `" + "`, `".join(source_state_degraded) + "`")
if worker_state_degraded:
    summary_lines.append("- worker_state_degraded_codes: `" + "`, `".join(worker_state_degraded) + "`")
if proof.get("requested_treeish"):
    summary_lines.append(f"- requested_treeish: `{proof.get('requested_treeish')}`")
if proof.get("source_manifest_hash"):
    summary_lines.append(f"- source_manifest_hash: `{proof.get('source_manifest_hash')}`")
summary = "\n".join(summary_lines)

if include_summary:
    proof["summary_markdown"] = summary

if ledger_path:
    proof["ledger_path"] = ledger_path
    if no_write:
        proof.setdefault("degraded_codes", []).append("rch_verify_ledger_write_suppressed")
    else:
        row = {
            "schema": "ee.rch.verify.ledger.v1",
            "verifier_id": proof.get("generated_at"),
            "bead_id": bead_id or None,
            "command": proof.get("command"),
            "command_text": proof.get("command_text"),
            "command_hash": command_hash,
            "command_kind": proof.get("command_kind"),
            "remote_env": proof.get("remote_env") or [],
            "started_at": started_at,
            "completed_at": proof.get("generated_at"),
            "elapsed_ms": proof.get("elapsed_ms"),
            "worker_id": proof.get("worker_id"),
            "remote_project_root": proof.get("remote_project_root"),
            "remote_target_dir": proof.get("remote_target_dir"),
            "rch_location": "explicit_rch_exec",
            "exit_code": proof.get("exit_code"),
            "status": status,
            "first_error_file": first_error_file,
            "first_error_line": first_error_line,
            "stdout_tail": proof.get("stdout_tail"),
            "stderr_tail": proof.get("stderr_tail"),
            "transcript_path": None,
            "degraded_codes": proof.get("degraded_codes") or [],
            "source_state_degraded_codes": proof.get("source_state_degraded_codes") or [],
            "worker_state_degraded_codes": proof.get("worker_state_degraded_codes") or [],
            "error_codes": codes,
            "summary_markdown": summary,
        }
        path = Path(ledger_path)
        path.parent.mkdir(parents=True, exist_ok=True)
        with path.open("a", encoding="utf-8") as handle:
            handle.write(json.dumps(row, sort_keys=True, separators=(",", ":")) + "\n")

proof_json = json.dumps(proof, sort_keys=True, separators=(",", ":"))

if event_log_path:
    fake_invocation_count = 0
    fake_invocations_path = os.environ.get("FAKE_RCH_INVOCATIONS", "")
    if fake_invocations_path:
        fake_path = Path(fake_invocations_path)
        if fake_path.exists():
            fake_invocation_count = len(fake_path.read_text(encoding="utf-8").splitlines())
    event = {
        "schema": "ee.test_event.v1",
        "ts": proof.get("generated_at"),
        "test_id": bead_id or "rch_verify",
        "kind": "command_end",
        "command": "scripts/rch_verify.sh",
        "args": proof.get("command") or [],
        "stdout_hash": "sha256:" + hashlib.sha256(proof_json.encode("utf-8")).hexdigest(),
        "stderr_excerpt": proof.get("stderr_tail") or "",
        "exit_code": int(proof.get("exit_code") or 0),
        "elapsed_ms": proof.get("elapsed_ms") or 0,
        "fields": {
            "bead_id": bead_id or None,
            "status": status,
            "command_hash": command_hash,
            "cwd": redact(os.getcwd()),
            "git_head": proof.get("git_head"),
            "git_tree": proof.get("git_tree"),
            "dirty_status_hash": proof.get("dirty_status_hash"),
            "verification_attribution": proof.get("verification_attribution"),
            "source_state_degraded_codes": proof.get("source_state_degraded_codes") or [],
            "worker_state_degraded_codes": proof.get("worker_state_degraded_codes") or [],
            "fake_rch_invoked": fake_invocation_count > 0,
            "fake_rch_invocation_count": fake_invocation_count,
            "source_manifest_hash": proof.get("source_manifest_hash"),
            "stdout_artifact_path": None,
            "stderr_artifact_path": None,
            "schema_validation_status": "not_run",
            "deterministic_rerun_hash": proof.get("source_manifest_hash") or proof.get("dirty_status_hash"),
            "first_failure_diagnosis": status,
        },
    }
    path = Path(event_log_path)
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("a", encoding="utf-8") as handle:
        handle.write(json.dumps(event, sort_keys=True, separators=(",", ":")) + "\n")

print(proof_json)
PY
}

COMMAND_KIND="$(classify_command)"
WOULD_OFFLOAD=false
WORKER_ID_JSON=null
REMOTE_PROJECT_ROOT="/data/projects/eidetic_engine_cli"
REMOTE_TARGET_DIR="/tmp/ee-rch-verify-target"
REMOTE_PROJECT_ROOT_JSON="$(json_quote "$REMOTE_PROJECT_ROOT")"
REMOTE_TARGET_DIR_JSON="$(json_quote "$REMOTE_TARGET_DIR")"
REQUESTED_WORKERS_CSV="${RCH_WORKERS:-}"
CONFIGURED_WORKERS_CSV=""
DAEMON_WORKERS_CSV=""

if contains_forbidden_text "${COMMAND[@]}"; then
    RCH_INVOCATION=()
    emit_json false null 0 "" "refused forbidden command text" "rch_verify_refused_forbidden_command"
    exit 2
fi

if [ "${RCH_VERIFY_PRINT_CRITICAL_MANIFEST:-0}" = "1" ]; then
    critical_checkout_manifest
    exit 0
fi

if [ "$COMMAND_KIND" = "rejected" ]; then
    RCH_INVOCATION=()
    emit_json false null 0 "" "unsupported verification command; pass --allow-raw for an explicitly raw remote command" "rch_verify_refused_unknown_command"
    exit 2
fi

if [ "$COMMITTED_TREE" -eq 1 ] && [ "$REQUIRE_CLEAN_TREE" -eq 1 ]; then
    RCH_INVOCATION=()
    emit_json false null 0 "" "choose either --committed-tree or --require-clean-tree, not both" "rch_verify_refused_conflicting_source_modes"
    exit 2
fi

if [ "$COMMITTED_TREE" -eq 1 ]; then
    SOURCE_STATE_JSON="$(compute_committed_tree_state_json)"
else
    SOURCE_STATE_JSON="$(compute_source_state_json)"
fi
SOURCE_STATE_DEGRADED_CODES="$(
    SOURCE_STATE_JSON="$SOURCE_STATE_JSON" python3 - <<'PY'
import json
import os
state = json.loads(os.environ["SOURCE_STATE_JSON"])
for code in state.get("source_state_degraded_codes") or []:
    print(code)
PY
)"
if [ "$REQUIRE_CLEAN_TREE" -eq 1 ] && [ -n "$SOURCE_STATE_DEGRADED_CODES" ]; then
    RCH_INVOCATION=()
    mapfile -t source_degraded_array <<<"$SOURCE_STATE_DEGRADED_CODES"
    emit_json true 1 0 "strict clean-tree preflight refused dirty checkout" "" "${source_degraded_array[@]}"
    exit 1
fi
if [ "$COMMITTED_TREE" -eq 1 ]; then
    if [ -n "$SOURCE_STATE_DEGRADED_CODES" ]; then
        RCH_INVOCATION=()
        mapfile -t source_degraded_array <<<"$SOURCE_STATE_DEGRADED_CODES"
        emit_json true 1 0 "committed-tree preflight computed source manifest but cannot safely materialize it for RCH" "" "${source_degraded_array[@]}"
        exit 1
    fi
    materialize_committed_tree
fi

if [ "$COMMAND_KIND" = "raw" ] || [ "$COMMAND_KIND" = "cargo_fmt_check" ]; then
    WOULD_OFFLOAD=false
else
    WOULD_OFFLOAD=true
fi
RCH_INVOCATION=(
    "$RCH_BIN" "exec" "--"
    "env" "TMPDIR=/tmp" "CARGO_TARGET_DIR=$REMOTE_TARGET_DIR"
    "${ENV_OVERRIDES[@]}"
    "${COMMAND[@]}"
)

if [ "$DRY_RUN" -eq 1 ]; then
    dry_run_degraded=("rch_verify_dry_run")
    if [ "$COMMAND_KIND" = "raw" ]; then
        dry_run_degraded+=("rch_verify_raw_command_may_not_offload")
    fi
    emit_json true null 0 "dry run: explicit rch exec planned" "" "${dry_run_degraded[@]}"
    exit 0
fi

CONFIGURED_WORKERS_CSV="$(configured_workers)"
DAEMON_WORKERS_CSV="$(daemon_workers)"
REQUESTED_WORKERS_CSV="${RCH_WORKERS:-}"

if [ "${RCH_VERIFY_FAIL_FAST_STALE_WORKER:-1}" = "1" ]; then
    allowed_workers_csv="${REQUESTED_WORKERS_CSV:-$CONFIGURED_WORKERS_CSV}"
    allowed_workers_note="configured"
    recent_failure_max_ms="${RCH_VERIFY_RECENT_FAILURE_MAX_MS:-10000}"
    if [ -n "$REQUESTED_WORKERS_CSV" ]; then
        allowed_workers_note="requested"
        recent_failure_max_ms="${RCH_VERIFY_REQUESTED_RECENT_FAILURE_MAX_MS:-120000}"
    fi
    stale_disk_full_workers="$(stale_disk_full_daemon_workers "$allowed_workers_csv" "$DAEMON_WORKERS_CSV" "${RCH_VERIFY_DISK_FULL_WORKERS:-}")"
    stale_recent_failed_workers="$(recent_failed_excluded_daemon_workers "$allowed_workers_csv" "$DAEMON_WORKERS_CSV" "$recent_failure_max_ms")"
    if [ -n "$stale_disk_full_workers" ]; then
        first_stale_worker="${stale_disk_full_workers%%,*}"
        WORKER_ID_JSON="$(json_quote "$first_stale_worker")"
        preflight_note="[RCH_VERIFY] stale daemon worker(s) excluded from $allowed_workers_note workers and recently disk-full: $stale_disk_full_workers"
        emit_json true 1 0 "$preflight_note" "" \
            "rch_verify_remote_command_failed" \
            "rch_verify_worker_disk_full" \
            "rch_verify_worker_filter_ignored"
        exit 1
    elif [ -n "$stale_recent_failed_workers" ]; then
        first_stale_worker="${stale_recent_failed_workers%%,*}"
        WORKER_ID_JSON="$(json_quote "$first_stale_worker")"
        preflight_note="[RCH_VERIFY] stale daemon worker(s) excluded from $allowed_workers_note workers and recently failed fast: $stale_recent_failed_workers"
        emit_json true 1 0 "$preflight_note" "" \
            "rch_verify_remote_command_failed" \
            "rch_verify_worker_filter_ignored"
        exit 1
    fi
fi

start_ms="$(now_ms)"
set +e
combined_output="$(run_rch_invocation_once)"
exit_code=$?
set -e
end_ms="$(now_ms)"
elapsed_ms=$((end_ms - start_ms))
if [ -n "${RCH_VERIFY_FAKE_ELAPSED_MS:-}" ]; then
    elapsed_ms="${RCH_VERIFY_FAKE_ELAPSED_MS}"
fi

worker_id="$(printf '%s' "$combined_output" | extract_worker_id)"
disk_full_worker=""
retried_after_disk_full=0
retry_worker=""
worker_filter_ignored=0
if [ "$exit_code" -ne 0 ] \
    && printf '%s' "$combined_output" | is_worker_disk_full_output \
    && [ -n "$worker_id" ] \
    && [ "${RCH_VERIFY_DISABLE_DISK_FULL_RETRY:-0}" != "1" ]; then
    disk_full_worker="$worker_id"
    alternate_workers="$(healthy_alternate_workers "$disk_full_worker" "${REQUESTED_WORKERS_CSV:-$CONFIGURED_WORKERS_CSV}")"
    if [ -n "$alternate_workers" ]; then
        retried_after_disk_full=1
        retry_note="[RCH_VERIFY] worker $disk_full_worker hit disk-full transfer failure; retrying once with RCH_WORKERS=$alternate_workers"
        start_retry_ms="$(now_ms)"
        set +e
        retry_output="$(run_rch_invocation_retry "$alternate_workers")"
        retry_exit_code=$?
        set -e
        end_retry_ms="$(now_ms)"
        elapsed_ms=$((elapsed_ms + end_retry_ms - start_retry_ms))
        combined_output="${combined_output}
${retry_note}
${retry_output}"
        exit_code="$retry_exit_code"
        retry_worker="$(printf '%s' "$retry_output" | extract_worker_id)"
        if [ -n "$retry_worker" ]; then
            worker_id="$retry_worker"
        fi
    fi
fi
if [ -n "$worker_id" ]; then
    WORKER_ID_JSON="$(json_quote "$worker_id")"
    allowed_workers_csv="${REQUESTED_WORKERS_CSV:-$CONFIGURED_WORKERS_CSV}"
    if [ -n "$allowed_workers_csv" ] && ! csv_contains "$allowed_workers_csv" "$worker_id"; then
        worker_filter_ignored=1
    fi
fi

remote_checkout_missing_paths="$(remote_checkout_missing_tracked_paths "$combined_output")"
if [ -n "$remote_checkout_missing_paths" ]; then
    combined_output="${combined_output}
[RCH_VERIFY] remote checkout missing tracked files: $remote_checkout_missing_paths"
fi

stdout_tail="$(printf '%s' "$combined_output" | tail_text)"
degraded=()
if [ "$exit_code" -ne 0 ]; then
    degraded+=("rch_verify_remote_command_failed")
fi
if [ -n "$disk_full_worker" ] || printf '%s' "$combined_output" | is_worker_disk_full_output; then
    degraded+=("rch_verify_worker_disk_full")
fi
if [ "$retried_after_disk_full" -eq 1 ]; then
    degraded+=("rch_verify_retry_after_worker_disk_full")
fi
if [ -n "$disk_full_worker" ] && [ "${retry_worker:-}" = "$disk_full_worker" ]; then
    degraded+=("rch_verify_worker_quarantine_ignored")
fi
if [ "$worker_filter_ignored" -eq 1 ]; then
    degraded+=("rch_verify_worker_filter_ignored")
fi
if [ -n "$remote_checkout_missing_paths" ]; then
    degraded+=("rch_verify_remote_checkout_incomplete")
fi
if [ "$COMMAND_KIND" = "raw" ]; then
    degraded+=("rch_verify_raw_command_may_not_offload")
fi
if printf '%s' "$combined_output" | grep -q "RCH-E327"; then
    degraded+=("rch_verify_topology_blocked")
fi
if printf '%s' "$combined_output" | grep -q "remote required; refusing local fallback"; then
    degraded+=("rch_verify_local_fallback_refused")
fi
if [ "$exit_code" -ne 0 ] && [ -z "$worker_id" ] && printf '%s' "$combined_output" | grep -Eiq "timed out|timeout|capacity|busy|no workers|workers_healthy: 0|all_workers_offline"; then
    degraded+=("rch_verify_capacity_or_timeout")
fi
if printf '%s' "$combined_output" | grep -q "non-compilation command"; then
    degraded+=("rch_verify_not_offloaded")
elif [ "$WOULD_OFFLOAD" = true ] && [ -z "$worker_id" ]; then
    degraded+=("rch_verify_remote_marker_missing")
fi

emit_json true "$exit_code" "$elapsed_ms" "$stdout_tail" "" "${degraded[@]}"
exit "$exit_code"
