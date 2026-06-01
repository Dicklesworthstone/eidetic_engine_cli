#!/usr/bin/env bash
# Cross-check ee command discovery surfaces and emit an e2e command inventory
# report. This script intentionally does not build the binary; pass EE_BINARY
# or run it after an RCH/local build has produced the executable.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

# shellcheck source=scripts/lib/ee_binary_resolution.sh
source "${REPO_ROOT}/scripts/lib/ee_binary_resolution.sh"

EE_BINARY="$(ee_resolve_binary debug)"
if [[ ! -x "${EE_BINARY}" ]]; then
    echo "[ERROR] ee binary not found at ${EE_BINARY}; set EE_BINARY to an existing executable" >&2
    exit 3
fi

WORK_DIR="$(mktemp -d -t ee-command-inventory.XXXXXX)"
ARTIFACT="${WORK_DIR}/ee.e2e.command_inventory.v1.json"
HELP_JSON="${WORK_DIR}/help_json.json"
INTROSPECT_JSON="${WORK_DIR}/introspect_json.json"

echo "[INFO] workspace: ${WORK_DIR}" >&2
echo "[INFO] artifact: ${ARTIFACT}" >&2

"${EE_BINARY}" --help-json >"${HELP_JSON}"
"${EE_BINARY}" introspect --json >"${INTROSPECT_JSON}"

python3 - "${HELP_JSON}" "${INTROSPECT_JSON}" "${ARTIFACT}" "${EE_BINARY}" <<'PY'
import json
import sys
from pathlib import Path

help_path = Path(sys.argv[1])
introspect_path = Path(sys.argv[2])
artifact_path = Path(sys.argv[3])
binary = sys.argv[4]

help_doc = json.loads(help_path.read_text())
introspect_doc = json.loads(introspect_path.read_text())


def help_commands(doc):
    out = {}
    for command in doc.get("data", {}).get("commands", []):
        name = command.get("name")
        if not name:
            continue
        subcommands = sorted(
            sub.get("name")
            for sub in command.get("subcommands", [])
            if sub.get("name")
        )
        out[name] = subcommands
    return out


def introspect_commands(doc):
    out = {}
    commands = doc.get("data", {}).get("commands", {})
    for name, command in commands.items():
        subcommands = sorted(
            sub.get("name")
            for sub in command.get("subcommands", [])
            if sub.get("name")
        )
        out[name] = subcommands
    return out


help_inventory = help_commands(help_doc)
introspect_inventory = introspect_commands(introspect_doc)
help_names = set(help_inventory)
introspect_names = set(introspect_inventory)

checks = []


def check(assertion, passed, details=None):
    checks.append({
        "assertion": assertion,
        "status": "PASS" if passed else "FAIL",
        "details": details or {},
    })


check(
    "help_json_uses_response_v2",
    help_doc.get("schema") == "ee.response.v2",
    {"actual": help_doc.get("schema")},
)
check(
    "introspect_uses_response_v2",
    introspect_doc.get("schema") == "ee.response.v2",
    {"actual": introspect_doc.get("schema")},
)
check(
    "help_and_introspect_top_level_commands_match",
    help_names == introspect_names,
    {
        "missingFromHelpJson": sorted(introspect_names - help_names),
        "missingFromIntrospect": sorted(help_names - introspect_names),
    },
)

subcommand_drift = {}
for name in sorted(help_names | introspect_names):
    help_subcommands = set(help_inventory.get(name, []))
    introspect_subcommands = set(introspect_inventory.get(name, []))
    if help_subcommands != introspect_subcommands:
        subcommand_drift[name] = {
            "missingFromHelpJson": sorted(introspect_subcommands - help_subcommands),
            "missingFromIntrospect": sorted(help_subcommands - introspect_subcommands),
        }

check(
    "help_and_introspect_subcommands_match",
    not subcommand_drift,
    {"commands": subcommand_drift},
)

commands = []
for name in sorted(help_names | introspect_names):
    status = "PASS" if name in help_names and name in introspect_names else "FAIL"
    commands.append({
        "path": name,
        "source": {
            "helpJson": name in help_names,
            "introspect": name in introspect_names,
        },
        "subcommands": sorted(set(help_inventory.get(name, [])) | set(introspect_inventory.get(name, []))),
        "status": status,
    })

report = {
    "schema": "ee.e2e.command_inventory.v1",
    "success": all(check["status"] == "PASS" for check in checks),
    "binary": binary,
    "helpJson": str(help_path),
    "introspectJson": str(introspect_path),
    "checks": checks,
    "commands": commands,
}

artifact_path.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n")

for check_row in checks:
    print(f"[{check_row['status']}] {check_row['assertion']}")
print(f"[ARTIFACT] {artifact_path}")

if not report["success"]:
    sys.exit(2)
PY
