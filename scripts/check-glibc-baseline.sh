#!/usr/bin/env bash
# Check the actual GNU/Linux binary, including requirements from native libraries.
# Used after CI and DSR builds; a target suffix alone is not compatibility proof.
set -euo pipefail

if [[ $# -lt 1 || $# -gt 2 ]]; then
    echo "Usage: bash scripts/check-glibc-baseline.sh BINARY [MAX_GLIBC_VERSION]" >&2
    exit 2
fi

python3 - "$1" "${2:-2.28}" <<'PY'
import re
import subprocess
import sys

binary, baseline = sys.argv[1:]
if not re.fullmatch(r"[0-9]+\.[0-9]+(?:\.[0-9]+)?", baseline):
    sys.exit(f"invalid glibc baseline: {baseline}")

def version(value):
    parts = tuple(map(int, value.split(".")))
    return parts + (0,) * (3 - len(parts))

try:
    output = subprocess.run(
        ["readelf", "--wide", "--version-info", binary],
        check=True, capture_output=True, text=True,
    ).stdout
except (OSError, subprocess.CalledProcessError) as error:
    sys.exit(f"cannot inspect GNU/Linux binary {binary}: {error}")

requirements = set(re.findall(r"Name: GLIBC_([A-Za-z0-9_.]+)", output))
if not requirements:
    sys.exit(f"no glibc requirements found in {binary}; expected a dynamic GNU/Linux binary")
invalid = sorted(value for value in requirements if not re.fullmatch(r"[0-9]+\.[0-9]+(?:\.[0-9]+)?", value))
if invalid:
    sys.exit(f"unsupported glibc requirements in {binary}: {', '.join(invalid)}")
maximum = max(requirements, key=version)
if version(maximum) > version(baseline):
    sys.exit(f"{binary} requires GLIBC_{maximum}, above release baseline GLIBC_{baseline}")
print(f"{binary}: maximum GLIBC_{maximum} <= release baseline GLIBC_{baseline}")
PY
