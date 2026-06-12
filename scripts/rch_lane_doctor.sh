#!/usr/bin/env bash
# Read-only RCH lane doctor for the Mac checkout (bd-2qpgn).
#
# Classifies the host-environment state that decides whether
# scripts/rch_verify.sh can reach remote Cargo at all, and emits the
# known-good env override when the USB-detached dual blocker is active.
#
# Background (bd-2qpgn, 2026-06-12): with the external build drive
# detached, /data dangles and the sibling path-dependency roots that are
# symlinks into the user dp dir escape the default canonical project
# root. Both Cargo.toml path forms are then refused before Cargo:
#   - absolute /data/projects/<dep> passes the planner's textual
#     allowed-root check but dies in sibling rsync (ENOENT);
#   - relative ../<dep> canonicalizes through the symlink to a path
#     outside the canonical root and the dependency planner refuses
#     with RCH-E327.
# The verified remediation is broadening the topology roots for the
# dispatch only:
#   RCH_CANONICAL_PROJECT_ROOT=$HOME RCH_ALIAS_PROJECT_ROOT=/dp
# which keeps every canonicalized sibling under the canonical root and
# lets the sync closure ship the dp checkouts to the worker.

set -uo pipefail

usage() {
    cat <<'EOF'
Usage: scripts/rch_lane_doctor.sh [--json] [--emit-env]

Modes:
  --json      Emit the full ee.rch_lane_doctor.v1 report on stdout (default).
  --emit-env  Emit eval-able export lines for the broadened-root override
              when (and only when) the usb_detached_dual_blocker state is
              detected; emits nothing when the lane is healthy.

Exit codes:
  0  lane healthy (default roots work; no override needed)
  2  usb_detached_dual_blocker detected (override available and emitted)
  3  indeterminate (unexpected layout; inspect the JSON report)

The script is read-only: no Cargo, no writes, no network.
Remediation tracking bead: bd-2qpgn.
EOF
}

MODE="json"
case "${1:-}" in
    "" ) ;;
    --json ) MODE="json" ;;
    --emit-env ) MODE="emit-env" ;;
    -h|--help ) usage; exit 0 ;;
    * ) usage >&2; exit 64 ;;
esac

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd -P)"
DEFAULT_CANONICAL_ROOT="$(dirname "$REPO_ROOT")"

usb_mounted=false
[ -d /Volumes/USBNVME16TB ] && usb_mounted=true

resolve_dir() {
    # Print the physical path of a directory, or nothing if unresolvable.
    (cd "$1" 2>/dev/null && pwd -P) || true
}

data_target=""
data_resolves=false
if [ -L /data ]; then
    data_target="$(readlink /data)"
    [ -n "$(resolve_dir /data)" ] && data_resolves=true
elif [ -d /data ]; then
    data_target="/data"
    data_resolves=true
fi

dp_target=""
dp_resolves=false
if [ -L /dp ]; then
    dp_target="$(readlink /dp)"
    [ -n "$(resolve_dir /dp)" ] && dp_resolves=true
elif [ -d /dp ]; then
    dp_target="/dp"
    dp_resolves=true
fi

# Collect path-dependency roots from the [patch.crates-io] table. Each
# unique first path component (relative to the repo) is one sibling root.
sibling_json=""
escaped_sibling_count=0
unresolvable_sibling_count=0
sibling_total=0
while IFS= read -r root_decl; do
    canonical="$(resolve_dir "$root_decl")"
    sibling_total=$((sibling_total + 1))
    under_root=false
    resolvable=false
    if [ -n "$canonical" ]; then
        resolvable=true
        case "$canonical" in
            "$DEFAULT_CANONICAL_ROOT"/* ) under_root=true ;;
        esac
    else
        unresolvable_sibling_count=$((unresolvable_sibling_count + 1))
    fi
    if [ "$resolvable" = true ] && [ "$under_root" = false ]; then
        escaped_sibling_count=$((escaped_sibling_count + 1))
    fi
    entry=$(printf '{"declaredPath":"%s","canonicalPath":"%s","resolvable":%s,"underDefaultCanonicalRoot":%s}' \
        "$root_decl" "${canonical:-}" "$resolvable" "$under_root")
    if [ -n "$sibling_json" ]; then
        sibling_json="$sibling_json,$entry"
    else
        sibling_json="$entry"
    fi
done < <(sed -n '/^\[patch\.crates-io\]/,/^\[/p' "$REPO_ROOT/Cargo.toml" \
    | sed -n 's/.*path = "\([^"]*\)".*/\1/p' \
    | while IFS= read -r dep_path; do
        case "$dep_path" in
            /* ) declared="$dep_path" ;;
            *  ) declared="$REPO_ROOT/$dep_path" ;;
        esac
        # Reduce to the sibling root (the directory above crates/<crate>).
        case "$declared" in
            */crates/* ) declared="${declared%%/crates/*}" ;;
        esac
        printf '%s\n' "$declared"
    done | sort -u)

lane_state="indeterminate"
exit_code=3
if [ "$sibling_total" -gt 0 ] && [ "$escaped_sibling_count" -eq 0 ] && [ "$unresolvable_sibling_count" -eq 0 ]; then
    lane_state="healthy"
    exit_code=0
elif [ "$usb_mounted" = false ] && { [ "$escaped_sibling_count" -gt 0 ] || [ "$unresolvable_sibling_count" -gt 0 ]; }; then
    lane_state="usb_detached_dual_blocker"
    exit_code=2
fi

override_canonical="$HOME"
override_alias="/dp"
[ "$dp_resolves" = true ] || override_alias="/data"

if [ "$MODE" = "emit-env" ]; then
    if [ "$lane_state" = "usb_detached_dual_blocker" ]; then
        printf 'export RCH_CANONICAL_PROJECT_ROOT=%s\n' "$override_canonical"
        printf 'export RCH_ALIAS_PROJECT_ROOT=%s\n' "$override_alias"
    fi
    exit "$exit_code"
fi

printf '{"schema":"ee.rch_lane_doctor.v1","laneState":"%s","remediationBead":"bd-2qpgn","usbVolumeMounted":%s,"dataSymlink":{"target":"%s","resolves":%s},"dpSymlink":{"target":"%s","resolves":%s},"defaultCanonicalRoot":"%s","siblingRoots":[%s],"escapedSiblingCount":%s,"unresolvableSiblingCount":%s,"recommendation":{"envOverride":{"RCH_CANONICAL_PROJECT_ROOT":"%s","RCH_ALIAS_PROJECT_ROOT":"%s"},"appliesWhen":"usb_detached_dual_blocker","exampleCommand":"eval \\"$(scripts/rch_lane_doctor.sh --emit-env)\\" && TMPDIR=/private/tmp RCH_VISIBILITY=summary RCH_TEST_TIMEOUT_SEC=3600 scripts/rch_verify.sh --summary -- cargo test --lib","note":"override broadens the dispatch-local topology roots only; project hash changes (fresh lane, cold first build - set RCH_TEST_TIMEOUT_SEC=3600 or the remote 1800s test budget times out mid-compile with RCH-E104). Verified 2026-06-12 on bd-2qpgn."}}\n' \
    "$lane_state" "$usb_mounted" "$data_target" "$data_resolves" "$dp_target" "$dp_resolves" \
    "$DEFAULT_CANONICAL_ROOT" "$sibling_json" "$escaped_sibling_count" "$unresolvable_sibling_count" \
    "$override_canonical" "$override_alias"
exit "$exit_code"
