#!/usr/bin/env bash
# bd-reality-core-convergence-1azkt.62 — Own-corpus dogfood: scheduled redacted
# real-history probe with the current candidate binary.
#
# Usage:
#   scripts/dogfood_own_corpus.sh [--ee <path>] [--workspace <path>] [--since <dur>]
#       [--limit <n>] [--out <dir>] [--previous <report.json>] [--post-comment]
#   scripts/dogfood_own_corpus.sh --self-test
#   scripts/dogfood_own_corpus.sh --print-schedule
#
# Never invokes Cargo locally. Emits `ee.dogfood_report.v1` JSON containing only
# counts, latencies, metric values, code/reason histograms, and hashes — never
# memory content, transcript content, filesystem paths, hostnames, or emails.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

# shellcheck source=scripts/lib/ee_binary_resolution.sh
source "$REPO_ROOT/scripts/lib/ee_binary_resolution.sh"

EE_BIN="${EE_BIN:-${EE_BINARY:-}}"
SOURCE_WORKSPACE="$REPO_ROOT"
SINCE_WINDOW="14d"
IMPORT_LIMIT="1000"
OUT_DIR=""
PREVIOUS_REPORT=""
POST_COMMENT=0
PRINT_SCHEDULE=0
SELF_TEST=0

usage() {
    cat <<'EOF'
Usage: scripts/dogfood_own_corpus.sh [OPTIONS]

Options:
  --ee <path>           Candidate ee binary (default: resolved from CARGO_TARGET_DIR)
  --workspace <path>    Source workspace to query CASS sessions for (default: repo root)
  --since <duration>    Bounded import window passed to `ee import cass --since` (default: 14d)
  --limit <n>           Maximum sessions/messages to import (default: 1000)
  --out <dir>           Output directory for gitignored artifacts (default: .ntm/swarm/dogfood/<ts>)
  --previous <file>     Previous ee.dogfood_report.v1 JSON to compute metric deltas against
  --post-comment        Post redacted summary comment to bd-reality-core-convergence-1azkt
  --print-schedule      Print optional launchd / cron schedule recipes and exit
  --self-test           Run redaction, non-vacuity, and delta self-tests and exit
  -h, --help            Show this help
EOF
}

while [ $# -gt 0 ]; do
    case "$1" in
        --ee) EE_BIN="${2:?--ee requires a path}"; shift 2 ;;
        --workspace) SOURCE_WORKSPACE="${2:?--workspace requires a path}"; shift 2 ;;
        --since) SINCE_WINDOW="${2:?--since requires a duration}"; shift 2 ;;
        --limit) IMPORT_LIMIT="${2:?--limit requires a number}"; shift 2 ;;
        --out) OUT_DIR="${2:?--out requires a directory}"; shift 2 ;;
        --previous) PREVIOUS_REPORT="${2:?--previous requires a file}"; shift 2 ;;
        --post-comment) POST_COMMENT=1; shift ;;
        --print-schedule) PRINT_SCHEDULE=1; shift ;;
        --self-test) SELF_TEST=1; shift ;;
        -h|--help) usage; exit 0 ;;
        *) echo "dogfood_own_corpus: unknown option: $1" >&2; usage >&2; exit 64 ;;
    esac
done

print_schedule_recipes() {
    cat <<'EOF'
# Optional Scheduling Recipes for scripts/dogfood_own_corpus.sh
# Default operation is manual; scheduling is the operator's choice.

## 1. cron (weekly on Sunday at 03:15 local time)
15 3 * * 0 cd /path/to/eidetic_engine_cli && ./scripts/dogfood_own_corpus.sh --since 14d --post-comment >/dev/null 2>&1

## 2. macOS launchd (~/Library/LaunchAgents/com.dicklesworthstone.ee.dogfood.plist)
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>com.dicklesworthstone.ee.dogfood</string>
  <key>ProgramArguments</key>
  <array>
    <string>/bin/bash</string>
    <string>scripts/dogfood_own_corpus.sh</string>
    <string>--since</string>
    <string>14d</string>
  </array>
  <key>StartCalendarInterval</key>
  <dict>
    <key>Weekday</key>
    <integer>0</integer>
    <key>Hour</key>
    <integer>3</integer>
    <key>Minute</key>
    <integer>15</integer>
  </dict>
</dict>
</plist>
EOF
}

if [ "$PRINT_SCHEDULE" -eq 1 ]; then
    print_schedule_recipes
    exit 0
fi

# Redaction validator: verifies that an ee.dogfood_report.v1 JSON document
# contains zero filesystem paths, zero hostnames/IPv4s, zero emails, and zero
# raw memory/transcript content keys.
validate_redacted_report() {
    local report_file="$1"
    python3 - "$report_file" <<'PY'
import json
import re
import sys

path = sys.argv[1]
raw = open(path, encoding="utf-8").read()
doc = json.loads(raw)

FORBIDDEN_KEYS = {
    "content",
    "text",
    "preview",
    "transcript",
    "workspace",
    "root",
    "sourcePath",
    "source_path",
    "path",
    "host",
    "hostname",
}

PATH_RE = re.compile(
    r"(?:/Users/|/home/|/data/|/private/|/var/folders/|/Volumes/|/tmp/ee-|[A-Za-z]:\\\\)"
)
EMAIL_RE = re.compile(r"\b[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}\b")
IPV4_RE = re.compile(r"\b(?:10|172\.(?:1[6-9]|2\d|3[01])|192\.168|100\.64)\.\d{1,3}\.\d{1,3}\b")
HOST_RE = re.compile(r"\b(?:macbook|imac|mac-mini|ubuntu-|[a-z0-9-]+-worker-\d+)\b", re.IGNORECASE)

problems = []


def walk(node, trail="$"):
    if isinstance(node, dict):
        for k, v in node.items():
            if k in FORBIDDEN_KEYS:
                problems.append(f"forbidden unredacted key at {trail}.{k}")
            walk(v, f"{trail}.{k}")
    elif isinstance(node, list):
        for idx, item in enumerate(node):
            walk(item, f"{trail}[{idx}]")
    elif isinstance(node, str):
        if PATH_RE.search(node):
            problems.append(f"filesystem path pattern at {trail}: {node!r}")
        if EMAIL_RE.search(node):
            problems.append(f"email pattern at {trail}: {node!r}")
        if IPV4_RE.search(node):
            problems.append(f"private IPv4 pattern at {trail}: {node!r}")
        if HOST_RE.search(node):
            problems.append(f"hostname pattern at {trail}: {node!r}")


walk(doc)
if doc.get("schema") != "ee.dogfood_report.v1":
    problems.append(f"unexpected schema: {doc.get('schema')!r}")

# Non-vacuity check (Refinement Pass 3):
imp = doc.get("import") or {}
if not isinstance(imp.get("sessions_imported"), int) or imp.get("sessions_imported", 0) <= 0:
    problems.append(f"vacuous import: sessions_imported={imp.get('sessions_imported')!r}")

metrics = doc.get("metrics") or {}
measured_count = 0
for name, val in metrics.items():
    if isinstance(val, dict) and val.get("status") != "unavailable":
        measured_count += 1
if measured_count == 0:
    problems.append("vacuous report: every metric in metrics{} is unavailable")

if problems:
    for p in problems:
        sys.stderr.write(f"redaction/non-vacuity violation: {p}\n")
    sys.exit(1)
PY
}

run_self_test() {
    local st_dir
    st_dir="$(mktemp -d "${TMPDIR:-/tmp}/ee-dogfood-selftest.XXXXXX")"
    local clean="$st_dir/clean.json"
    local planted_path="$st_dir/planted_path.json"
    local planted_host="$st_dir/planted_host.json"
    local planted_email="$st_dir/planted_email.json"
    local planted_vacuous="$st_dir/planted_vacuous.json"

    cat >"$clean" <<'JSON'
{
  "schema": "ee.dogfood_report.v1",
  "bead": "bd-reality-core-convergence-1azkt.62",
  "generated_at": "2026-11-16T13:30:00Z",
  "scope": {
    "workspace_hash": "blake3:0123456789abcdef",
    "since": "14d",
    "limit": 1000,
    "corpus_source": "host_cass"
  },
  "binary": {
    "version": "0.18.0",
    "sha256": "sha256:abcdef0123456789",
    "cass_sha256": "sha256:fedcba9876543210"
  },
  "import": {
    "sessions_imported": 12,
    "spans_imported": 148,
    "elapsed_seconds": 1.42
  },
  "metrics": {
    "UTR": {"value": 0.9991, "packed_chars": 4120, "target": ">= 0.95"},
    "TTFUC": {"pack_p50_seconds": 0.612, "pack_p95_seconds": 0.781, "target": "<= 2 s cold"},
    "search_latency": {"p50_seconds": 0.442, "p95_seconds": 0.591},
    "FAR_proxy": {"negative_flagged_rate": 1.0, "negative_queries": 4},
    "PAP_proxy": {"proposals": 4, "scaffold_free_rate": 1.0, "polluted": 0},
    "WCS": {"remember_p50_seconds": 0.318, "remember_p95_seconds": 0.402},
    "retained_generations": {"count": 1, "target": "<= 2"}
  },
  "histograms": {
    "degraded_codes": {"weak_query_recall": 4},
    "selection_reasons": {"hybrid_top_score": 15}
  }
}
JSON

    jq '.scope.leaked = "/Users/alice/projects/eidetic_engine_cli"' "$clean" >"$planted_path"
    jq '.scope.leaked = "ubuntu-worker-01.internal"' "$clean" >"$planted_host"
    jq '.scope.leaked = "operator@example.com"' "$clean" >"$planted_email"
    jq '.metrics = {"UTR": {"status": "unavailable", "reason": "none"}}' "$clean" >"$planted_vacuous"

    local failures=0
    if validate_redacted_report "$clean" 2>/dev/null; then
        echo "  [ok  ] clean redacted report passes redaction + non-vacuity validator"
    else
        echo "  [FAIL] clean redacted report was rejected" >&2
        failures=$((failures + 1))
    fi

    if ! validate_redacted_report "$planted_path" 2>/dev/null; then
        echo "  [ok  ] planted filesystem path fails redaction validator"
    else
        echo "  [FAIL] planted filesystem path was accepted" >&2
        failures=$((failures + 1))
    fi

    if ! validate_redacted_report "$planted_host" 2>/dev/null; then
        echo "  [ok  ] planted hostname fails redaction validator"
    else
        echo "  [FAIL] planted hostname was accepted" >&2
        failures=$((failures + 1))
    fi

    if ! validate_redacted_report "$planted_email" 2>/dev/null; then
        echo "  [ok  ] planted email fails redaction validator"
    else
        echo "  [FAIL] planted email was accepted" >&2
        failures=$((failures + 1))
    fi

    if ! validate_redacted_report "$planted_vacuous" 2>/dev/null; then
        echo "  [ok  ] all-unavailable report fails non-vacuity guard"
    else
        echo "  [FAIL] all-unavailable report was accepted" >&2
        failures=$((failures + 1))
    fi

    if [ "$failures" -ne 0 ]; then
        echo "dogfood_own_corpus --self-test: FAILED ($failures arm(s))" >&2
        return 1
    fi
    echo "dogfood_own_corpus --self-test: PASS (5/5 arms)"
    return 0
}

if [ "$SELF_TEST" -eq 1 ]; then
    run_self_test
    exit $?
fi

if [ -z "$EE_BIN" ]; then
    EE_BIN="$(ee_resolve_binary release)"
fi
if ! ee_require_current_binary "$EE_BIN" "dogfood_own_corpus"; then
    exit 3
fi

TS="$(date -u +%Y%m%dT%H%M%SZ)"
if [ -z "$OUT_DIR" ]; then
    OUT_DIR="$REPO_ROOT/.ntm/swarm/dogfood/$TS"
fi
mkdir -p "$OUT_DIR"
REPORT_PATH="$OUT_DIR/dogfood_report.json"

python3 - "$REPO_ROOT" "$EE_BIN" "$SOURCE_WORKSPACE" "$SINCE_WINDOW" "$IMPORT_LIMIT" "$OUT_DIR" "$PREVIOUS_REPORT" <<'PY' >"$REPORT_PATH"
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time
from datetime import datetime, timedelta, timezone

repo_root, ee_bin, source_ws, since_window, import_limit, out_dir, prev_report_path = sys.argv[1:8]
repo_root = os.path.realpath(os.path.abspath(repo_root))
ee_bin = os.path.abspath(ee_bin)
source_ws = os.path.realpath(os.path.abspath(source_ws))
sys.path.insert(0, os.path.join(repo_root, "scripts", "real_shape_corpus"))
import generate  # noqa: E402
import probe as real_probe  # noqa: E402


def sha256_file(path):
    h = hashlib.sha256()
    with open(path, "rb") as fh:
        for chunk in iter(lambda: fh.read(65536), b""):
            h.update(chunk)
    return "sha256:" + h.hexdigest()


def sha256_text(text):
    return "sha256:" + hashlib.sha256(text.encode("utf-8")).hexdigest()[:16]


def pct(xs, q):
    if not xs:
        return None
    s = sorted(xs)
    idx = min(len(s) - 1, max(0, int(round((len(s) - 1) * q))))
    return round(s[idx], 3)


root = real_probe.allocate_physical_root(prefix="ee-dogfood-own-")
ws = os.path.join(root, "ws")
os.makedirs(ws, exist_ok=True)

env = {k: v for k, v in os.environ.items() if not k.startswith("EE_")}
for name, sub in (
    ("HOME", "home"),
    ("XDG_CONFIG_HOME", "xdg/config"),
    ("XDG_DATA_HOME", "xdg/data"),
    ("XDG_CACHE_HOME", "xdg/cache"),
    ("XDG_STATE_HOME", "xdg/state"),
):
    target = os.path.join(root, sub)
    os.makedirs(target, exist_ok=True)
    env[name] = target

host_xdg = os.environ.get("XDG_DATA_HOME") or os.path.join(os.path.expanduser("~"), ".local", "share")
host_models = os.path.join(host_xdg, "ee", "models")
if os.path.isdir(host_models):
    env["EE_EMBED_MODEL_DIR"] = host_models
env["EE_EMBED_DOWNLOAD"] = "off"
env["NO_COLOR"] = "1"

# Provision trusted cass binary (.44 flow): try host cass first if it has sessions
# for source_ws; otherwise fall back to real_shape_corpus emulator with explicit
# trust recorded in the report.
corpus_source = "host_cass"
host_cass = shutil.which("cass")
use_emulator = True
if host_cass:
    real_cass = os.path.realpath(host_cass)
    try:
        probe_proc = subprocess.run(
            [real_cass, "sessions", "--workspace", source_ws, "--json"],
            capture_output=True,
            text=True,
            timeout=15,
        )
        if probe_proc.returncode == 0 and probe_proc.stdout.strip():
            parsed_sessions = json.loads(probe_proc.stdout)
            rows = parsed_sessions if isinstance(parsed_sessions, list) else parsed_sessions.get("sessions", [])
            if isinstance(rows, list) and len(rows) > 0:
                use_emulator = False
                bin_dir = os.path.join(root, "bin")
                os.makedirs(bin_dir, exist_ok=True)
                os.chmod(bin_dir, 0o755)
                cass_bin = os.path.join(bin_dir, "cass")
                op_home = os.path.expanduser("~")
                with open(cass_bin, "w", encoding="utf-8") as fh:
                    fh.write(
                        "#!/usr/bin/env python3\n"
                        "import os, subprocess, sys\n"
                        f"real_cass = {real_cass!r}\n"
                        f"source_ws = {source_ws!r}\n"
                        f"op_home = {op_home!r}\n"
                        "args = list(sys.argv[1:])\n"
                        "if args and args[0] == 'sessions' and '--workspace' in args:\n"
                        "    i = args.index('--workspace')\n"
                        "    if i + 1 < len(args):\n"
                        "        args[i + 1] = source_ws\n"
                        "env = dict(os.environ)\n"
                        "env['HOME'] = op_home\n"
                        "for k in ('XDG_DATA_HOME', 'XDG_CONFIG_HOME', 'XDG_CACHE_HOME', 'XDG_STATE_HOME'):\n"
                        "    env.pop(k, None)\n"
                        "sys.exit(subprocess.call([real_cass, *args], env=env))\n"
                    )
                os.chmod(cass_bin, 0o555)
    except Exception:
        use_emulator = True

if use_emulator:
    corpus_source = "real_shape_corpus_fallback"
    corpus_dir = os.path.join(root, "corpus")
    manifest = generate.generate(corpus_dir, ws, 43, 0)
    now_dt = datetime.now(timezone.utc)
    for idx, sess in enumerate(manifest.get("sessions", [])):
        start_dt = now_dt - timedelta(hours=(idx + 1) * 4)
        end_dt = start_dt + timedelta(minutes=25)
        sess["started_at"] = start_dt.strftime("%Y-%m-%dT%H:%M:%SZ")
        sess["ended_at"] = end_dt.strftime("%Y-%m-%dT%H:%M:%SZ")
    with open(os.path.join(corpus_dir, "manifest.json"), "w", encoding="utf-8") as mfh:
        json.dump(manifest, mfh, indent=2)
    cass_bin = real_probe.install_cass_fixture(root)
    env["CASS_FIXTURE_ROOT"] = corpus_dir
    env["CASS_FIXTURE_LOG"] = os.path.join(out_dir, "cass-fixture.log")

cass_sha = sha256_file(cass_bin)
env["EE_CASS_BINARY"] = cass_bin

# Initialize isolated workspace and configure explicit cass trust (.44 flow)
code, payload, _, err = real_probe.run_ee(ee_bin, ["init", "--workspace", ws, "--json"], env, ws)
if code != 0:
    raise SystemExit(f"dogfood init failed ({code}): {real_probe.describe_envelope(payload, err)}")

real_probe.run_ee(ee_bin, ["config", "set", "cass.binary", cass_bin, "--workspace", ws, "--json"], env, ws)
real_probe.run_ee(
    ee_bin,
    ["config", "set", "cass.sha256", cass_sha.removeprefix("sha256:"), "--workspace", ws, "--json"],
    env,
    ws,
)

# Import bounded window (--since, --limit)
import_args = [
    "import",
    "cass",
    "--workspace",
    ws,
    "--since",
    since_window,
    "--limit",
    str(import_limit),
    "--json",
]
code, imp_payload, imp_elapsed, err = real_probe.run_ee(ee_bin, import_args, env, ws, timeout=1800)
if code != 0 or imp_payload is None:
    raise SystemExit(f"dogfood import failed ({code}): {real_probe.describe_envelope(imp_payload, err)}")

sessions_out = real_probe.find_key(imp_payload, "sessions") or []
spans_imported = real_probe.find_key(imp_payload, "spansImported")
if spans_imported is None:
    spans_imported = real_probe.find_key(imp_payload, "spans_imported") or 0

ver_code, ver_payload, _, _ = real_probe.run_ee(ee_bin, ["version", "--json"], env, ws)
ee_version = "unknown"
if ver_payload and isinstance(ver_payload, dict):
    data_node = ver_payload.get("data") if isinstance(ver_payload.get("data"), dict) else ver_payload
    ee_version = str(data_node.get("version") or data_node.get("packageVersion") or "unknown")

DOGFOOD_QUERIES = [
    "sqlite busy lock retry migration",
    "cargo test verification gate timeout",
    "context pack token budget deterministic",
    "cass import session redaction trust",
    "tailscale mesh peer sync status",
    "doctor fix undo replay run state",
]
NEGATIVE_QUERIES = [
    "quantum chromodynamics lattice gauge boson",
    "medieval sourdough starter hydration ratio",
    "apollo lunar module guidance gimbal lock",
    "baroque harpsichord temperament tuning fork",
]

search_times = []
pack_times = []
useful_chars = 0
total_chars = 0
packed_items_total = 0
degraded_hist = {}
reason_hist = {}

for q in DOGFOOD_QUERIES:
     scode, sp, selapsed, _ = real_probe.run_ee(
         ee_bin, ["search", q, "--workspace", ws, "--limit", "10", "--json"], env, ws
     )
     search_times.append(selapsed)
     for dcode in real_probe.degraded_codes(sp or {}):
         degraded_hist[dcode] = degraded_hist.get(dcode, 0) + 1

     pcode, pk, pelapsed, _ = real_probe.run_ee(
         ee_bin, ["pack", q, "--workspace", ws, "--max-tokens", "2000", "--json"], env, ws
     )
     pack_times.append(pelapsed)
     for dcode in real_probe.degraded_codes(pk or {}):
         degraded_hist[dcode] = degraded_hist.get(dcode, 0) + 1
     for item in real_probe.pack_items(pk or {}):
         content = item.get("content") or ""
         ratio = real_probe.scaffold_ratio(content)
         total_chars += len(content)
         useful_chars += len(content) * (1.0 - ratio)
         packed_items_total += 1
         reason = item.get("why") or item.get("section") or "selected"
         reason_key = str(reason).split(":")[0][:32]
         reason_hist[reason_key] = reason_hist.get(reason_key, 0) + 1

neg_flagged = 0
for nq in NEGATIVE_QUERIES:
    _, sp, _, _ = real_probe.run_ee(
        ee_bin, ["search", nq, "--workspace", ws, "--limit", "5", "--json"], env, ws
    )
    codes = real_probe.degraded_codes(sp or {})
    if codes & {"weak_query_recall", "no_relevant_results", "weak_semantic_evidence", "low_recall_after_floor"} or not real_probe.result_nodes(sp or {}):
        neg_flagged += 1

# PAP proxy: sample up to 3 imported sessions with `review session --propose`
proposals_total = 0
proposals_polluted = 0
for row in sessions_out[:3]:
    sid = row.get("sessionId") or row.get("session_id")
    if not sid:
        continue
    _, rp, _, _ = real_probe.run_ee(
        ee_bin, ["review", "session", sid, "--propose", "--workspace", ws, "--json"], env, ws
    )
    for cand in real_probe.find_key(rp or {}, "candidates") or []:
        if not isinstance(cand, dict):
            continue
        proposals_total += 1
        ctext = cand.get("proposedContent") or cand.get("content") or ""
        if real_probe.scaffold_ratio(ctext) > 0.25:
            proposals_polluted += 1

# WCS: 3 remember samples
write_times = []
for idx in range(3):
    wcode, wp, welapsed, werr = real_probe.run_ee(
        ee_bin,
        [
            "remember",
            f"Dogfood write sample {idx}: verify release binary probe capture before tagging.",
            "--workspace",
            ws,
            "--json",
        ],
        env,
        ws,
    )
    if wcode == 0:
        write_times.append(welapsed)

# Retained index generations under workspace .ee/index
index_root = os.path.join(ws, ".ee", "index")
retained_gens = 0
if os.path.isdir(index_root):
    retained_gens = len([name for name in os.listdir(index_root) if not name.startswith(".")])

report = {
    "schema": "ee.dogfood_report.v1",
    "bead": "bd-reality-core-convergence-1azkt.62",
    "generated_at": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
    "scope": {
        "workspace_hash": sha256_text(source_ws),
        "since": since_window,
        "limit": int(import_limit),
        "corpus_source": corpus_source,
    },
    "binary": {
        "version": ee_version,
        "sha256": sha256_file(ee_bin),
        "cass_sha256": cass_sha,
    },
    "import": {
        "sessions_imported": len(sessions_out),
        "spans_imported": int(spans_imported) if isinstance(spans_imported, int) else 0,
        "elapsed_seconds": round(imp_elapsed, 3),
    },
    "metrics": {
        "UTR": {
            "value": round(useful_chars / total_chars, 4) if total_chars else None,
            "packed_chars": total_chars,
            "packed_items": packed_items_total,
            "target": ">= 0.95",
        },
        "TTFUC": {
            "pack_p50_seconds": pct(pack_times, 0.50),
            "pack_p95_seconds": pct(pack_times, 0.95),
            "pack_max_seconds": round(max(pack_times), 3) if pack_times else None,
            "target": "<= 2 s cold",
        },
        "search_latency": {
            "p50_seconds": pct(search_times, 0.50),
            "p95_seconds": pct(search_times, 0.95),
        },
        "FAR_proxy": {
            "negative_flagged_rate": round(neg_flagged / len(NEGATIVE_QUERIES), 4),
            "negative_queries": len(NEGATIVE_QUERIES),
        },
        "PAP_proxy": {
            "proposals": proposals_total,
            "scaffold_free_rate": round((proposals_total - proposals_polluted) / proposals_total, 4)
            if proposals_total
            else 1.0,
            "polluted": proposals_polluted,
        },
        "WCS": {
            "remember_p50_seconds": pct(write_times, 0.50),
            "remember_p95_seconds": pct(write_times, 0.95),
        },
        "retained_generations": {
            "count": retained_gens,
            "target": "<= 2",
        },
    },
    "histograms": {
        "degraded_codes": dict(sorted(degraded_hist.items())),
        "selection_reasons": dict(sorted(reason_hist.items())),
    },
}

if prev_report_path and os.path.isfile(prev_report_path):
    prev = json.load(open(prev_report_path, encoding="utf-8"))
    pm = prev.get("metrics") or {}
    cm = report["metrics"]
    deltas = {}
    for key, subkey in (
        ("UTR", "value"),
        ("TTFUC", "pack_p50_seconds"),
        ("search_latency", "p50_seconds"),
        ("FAR_proxy", "negative_flagged_rate"),
        ("WCS", "remember_p50_seconds"),
        ("retained_generations", "count"),
    ):
        before = (pm.get(key) or {}).get(subkey)
        after = (cm.get(key) or {}).get(subkey)
        if isinstance(before, (int, float)) and isinstance(after, (int, float)):
            deltas[f"{key}.{subkey}"] = {
                "before": before,
                "after": after,
                "delta": round(after - before, 4),
            }
    report["deltas"] = deltas

print(json.dumps(report, indent=2))
PY

validate_redacted_report "$REPORT_PATH"
cat "$REPORT_PATH"

if [ "$POST_COMMENT" -eq 1 ]; then
    SUMMARY_LINE="$(jq -r '"[dogfood_own_corpus] schema=\(.schema) source=\(.scope.corpus_source) since=\(.scope.since) sessions=\(.import.sessions_imported) spans=\(.import.spans_imported) UTR=\(.metrics.UTR.value) pack_p50=\(.metrics.TTFUC.pack_p50_seconds)s search_p50=\(.metrics.search_latency.p50_seconds)s FAR_flag_rate=\(.metrics.FAR_proxy.negative_flagged_rate) PAP_proposals=\(.metrics.PAP_proxy.proposals) PAP_polluted=\(.metrics.PAP_proxy.polluted) WCS_p50=\(.metrics.WCS.remember_p50_seconds)s retained_gens=\(.metrics.retained_generations.count)"' "$REPORT_PATH")"
    export PATH="$PATH:/Users/jemanuel/.local/bin"
    br comments add bd-reality-core-convergence-1azkt "$SUMMARY_LINE"
fi
