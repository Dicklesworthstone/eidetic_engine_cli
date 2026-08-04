#!/usr/bin/env bash
# shellcheck disable=SC2016
# bd-3usjw.9.1 - release provenance smoke test.
#
# Default/static mode validates that the release workflow, installer, README,
# checklist, and audit script advertise the SLSA provenance contract.
#
# Asset-dir mode validates an already-produced release asset directory:
#   scripts/e2e_release_provenance.sh --asset-dir /path/to/release

set -euo pipefail

MODE="static"
ASSET_DIR=""
VERIFY_SIGNATURE="${VERIFY_SIGNATURE:-auto}"

usage() {
  cat <<'EOF'
Usage: scripts/e2e_release_provenance.sh [--static] [--asset-dir DIR] [--verify-signature]

Options:
  --static            Validate in-repo workflow and installer contracts (default)
  --asset-dir DIR     Validate ee-*.tar.xz assets and provenance files in DIR
  --verify-signature  Require cosign verification for provenance bundles
EOF
}

while [ $# -gt 0 ]; do
  case "$1" in
    --static)
      MODE="static"
      shift
      ;;
    --asset-dir)
      if [ -z "${2:-}" ] || [[ "${2:-}" == -* ]]; then
        echo "missing value for --asset-dir" >&2
        usage >&2
        exit 2
      fi
      MODE="asset-dir"
      ASSET_DIR="$2"
      shift 2
      ;;
    --verify-signature)
      VERIFY_SIGNATURE="required"
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "unknown option: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"

require_contains() {
  local file="$1"
  local needle="$2"
  if ! grep -qF -- "$needle" "$file"; then
    echo "missing expected provenance marker in $file: $needle" >&2
    exit 1
  fi
}

static_contract() {
  local workflow="$REPO_ROOT/.github/workflows/release.yml"
  local installer="$REPO_ROOT/install.sh"
  local readme="$REPO_ROOT/README.md"
  local checklist="$REPO_ROOT/PUBLISH_CHECKLIST.md"
  local audit="$REPO_ROOT/scripts/audit_install_pipeline.sh"

  require_contains "$workflow" "name: Generate SLSA provenance"
  require_contains "$workflow" "https://slsa.dev/provenance/v1"
  require_contains "$workflow" "b3sum ../Cargo.lock"
  require_contains "$workflow" 'ee-${{ matrix.target }}.provenance.json'
  require_contains "$workflow" 'ee-${{ matrix.target }}.provenance.json.sigstore.json'
  require_contains "$workflow" "Verify Sigstore bundles and provenance"
  require_contains "$workflow" "missing Cargo.lock blake3 dependency"
  require_contains "$workflow" "cosign verify-blob"

  require_contains "$installer" "--require-provenance"
  require_contains "$installer" 'verify_provenance_bundle "$TMP/$TAR" "$URL"'
  require_contains "$installer" '${artifact_url%.tar.xz}.provenance.json'
  require_contains "$installer" "provenance subject sha256 does not match downloaded artifact"
  require_contains "$installer" "provenance is missing Cargo.lock blake3 dependency"
  require_contains "$installer" "cosign verify-blob"

  require_contains "$readme" "always verifies its"
  require_contains "$readme" 'Pass `--require-provenance` to require both'
  require_contains "$readme" "verified SLSA provenance attestation"
  require_contains "$readme" "missing bundle is reported"
  require_contains "$checklist" "Signed release provenance ready"
  require_contains "$audit" "release_verifies_provenance_before_publish"
  require_contains "$audit" "unix_installer_supports_required_provenance"
}

validate_asset_dir() {
  local dir="$1"
  if [ ! -d "$dir" ]; then
    echo "asset directory not found: $dir" >&2
    exit 2
  fi
  if ! command -v python3 >/dev/null 2>&1; then
    echo "python3 is required for provenance JSON validation" >&2
    exit 2
  fi

  shopt -s nullglob
  local artifacts=("$dir"/ee-*.tar.xz)
  if [ "${#artifacts[@]}" -eq 0 ]; then
    echo "no ee-*.tar.xz artifacts found in $dir" >&2
    exit 1
  fi

  local artifact provenance bundle
  for artifact in "${artifacts[@]}"; do
    provenance="${artifact%.tar.xz}.provenance.json"
    bundle="${provenance}.sigstore.json"
    if [ ! -f "$provenance" ]; then
      echo "missing provenance for $artifact: $provenance" >&2
      exit 1
    fi
    if [ ! -f "$bundle" ]; then
      echo "missing provenance Sigstore bundle for $provenance: $bundle" >&2
      exit 1
    fi

    python3 - "$artifact" "$provenance" <<'PY'
import hashlib
import json
import pathlib
import sys

artifact = pathlib.Path(sys.argv[1])
provenance = pathlib.Path(sys.argv[2])
data = json.loads(provenance.read_text(encoding="utf-8"))
artifact_sha = hashlib.sha256(artifact.read_bytes()).hexdigest()

if data.get("predicateType") != "https://slsa.dev/provenance/v1":
    raise SystemExit(f"{provenance}: predicateType is not SLSA v1")
subjects = data.get("subject")
if not isinstance(subjects, list) or not subjects:
    raise SystemExit(f"{provenance}: subject is missing")
subject = subjects[0]
if subject.get("name") != artifact.name:
    raise SystemExit(f"{provenance}: subject name does not match artifact")
if subject.get("digest", {}).get("sha256") != artifact_sha:
    raise SystemExit(f"{provenance}: subject sha256 does not match artifact")
dependencies = data.get("predicate", {}).get("buildDefinition", {}).get("resolvedDependencies", [])
if not any(dep.get("uri") == "file://Cargo.lock" and dep.get("digest", {}).get("blake3") for dep in dependencies):
    raise SystemExit(f"{provenance}: missing Cargo.lock blake3 dependency")
if not any(dep.get("digest", {}).get("gitCommit") for dep in dependencies):
    raise SystemExit(f"{provenance}: missing source git commit dependency")
PY

    if command -v cosign >/dev/null 2>&1; then
      cosign verify-blob \
        --bundle "$bundle" \
        --certificate-identity-regexp '^https://github\.com/Dicklesworthstone/eidetic_engine_cli/\.github/workflows/release\.yml@refs/(tags/v[0-9].*|heads/main)$' \
        --certificate-oidc-issuer 'https://token.actions.githubusercontent.com' \
        "$provenance"
    elif [ "$VERIFY_SIGNATURE" = "required" ]; then
      echo "cosign is required by --verify-signature" >&2
      exit 2
    else
      echo "cosign not found; provenance signature verification skipped for $provenance" >&2
    fi
  done
}

case "$MODE" in
  static) static_contract ;;
  asset-dir) validate_asset_dir "$ASSET_DIR" ;;
  *) echo "invalid mode: $MODE" >&2; exit 2 ;;
esac

echo "release provenance smoke passed"
