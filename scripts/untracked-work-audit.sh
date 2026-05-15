#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
usage: scripts/untracked-work-audit.sh [--strict]

Classify current git modified/untracked paths against Beads FILE SURFACE
annotations. Default mode is advisory and exits 0; --strict exits 1 when any
changed path is not covered by an open or in-progress bead.
EOF
}

strict=false

while [ "$#" -gt 0 ]; do
  case "$1" in
    --strict)
      strict=true
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "error: unknown argument: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

if ! command -v br >/dev/null 2>&1; then
  echo "error: br is required" >&2
  exit 2
fi

if ! command -v jq >/dev/null 2>&1; then
  echo "error: jq is required" >&2
  exit 2
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
SURFACE_EXTRACTOR="${SCRIPT_DIR}/extract_bead_file_surfaces.sh"

if [ ! -x "$SURFACE_EXTRACTOR" ]; then
  echo "error: missing executable $SURFACE_EXTRACTOR" >&2
  exit 2
fi

cd "$REPO_ROOT"

normalize_surface_pattern() {
  local pattern="$1"
  pattern="${pattern%% \(*}"
  pattern="${pattern#"${pattern%%[![:space:]]*}"}"
  pattern="${pattern%"${pattern##*[![:space:]]}"}"
  printf '%s\n' "$pattern"
}

path_matches_surface() {
  local path="$1"
  local pattern="$2"

  pattern="$(normalize_surface_pattern "$pattern")"
  [ -n "$pattern" ] || return 1
  case "$pattern" in
    *" "*)
      return 1
      ;;
    *"all bd-"*)
      return 1
      ;;
  esac

  if [ "$path" = "$pattern" ]; then
    return 0
  fi

  if [ "${pattern: -3}" = "/**" ]; then
    local prefix="${pattern%/**}"
    [[ "$path" == "$prefix/"* ]]
    return $?
  fi

  if [[ "$pattern" == *'*'* || "$pattern" == *'?'* || "$pattern" == *'['* ]]; then
    [[ "$path" == $pattern ]]
    return $?
  fi

  if [ "${pattern: -1}" = "/" ]; then
    [[ "$path" == "$pattern"* ]]
    return $?
  fi

  return 1
}

suggested_bead_title() {
  local path="$1"
  printf 'Track file surface for %s\n' "$path"
}

surfaces_json="$("$SURFACE_EXTRACTOR" --prefix bd-)"
issues_json="$(br list --all --limit 0 --json)"
claim_rows="$(
  printf '%s\n' "$issues_json" | jq -r --argjson surfaces "$surfaces_json" '
    .[]
    | select(.status == "open" or .status == "in_progress" or .status == "closed")
    | .id as $id
    | .status as $status
    | ($surfaces[$id] // [])[]?
    | [$id, $status, .]
    | @tsv
  '
)"

items_json="[]"

while IFS= read -r status_line; do
  [ -n "$status_line" ] || continue
  status_code="${status_line:0:2}"
  path="${status_line:3}"
  case "$path" in
    *" -> "*)
      path="${path##* -> }"
      ;;
  esac

  claims_json="[]"
  while IFS=$'\t' read -r bead_id bead_status surface_pattern; do
    [ -n "${bead_id:-}" ] || continue
    if path_matches_surface "$path" "$surface_pattern"; then
      claims_json="$(
        printf '%s\n' "$claims_json" | jq \
          --arg id "$bead_id" \
          --arg status "$bead_status" \
          --arg surface "$surface_pattern" \
          '. + [{id: $id, status: $status, surface: $surface}]'
      )"
    fi
  done <<< "$claim_rows"

  claim_count="$(printf '%s\n' "$claims_json" | jq 'length')"
  warning="null"
  suggested="null"
  if [ "$claim_count" = "0" ]; then
    warning='"untracked_work_orphan"'
    suggested="$(jq -Rn --arg title "$(suggested_bead_title "$path")" '$title')"
  fi

  items_json="$(
    printf '%s\n' "$items_json" | jq \
      --arg path "$path" \
      --arg status "$status_code" \
      --argjson claims "$claims_json" \
      --argjson warning "$warning" \
      --argjson suggested "$suggested" \
      '. + [{
        path: $path,
        gitStatus: $status,
        claimedBy: $claims,
        warning: $warning,
        suggestedBeadTitle: $suggested
      }]'
  )"
done < <(git status --porcelain=v1)

report_json="$(
  printf '%s\n' "$items_json" | jq '
    {
      schema: "ee.untracked_work_audit.v1",
      summary: {
        changed: length,
        claimed: map(select((.claimedBy | length) > 0)) | length,
        orphan: map(select(.warning == "untracked_work_orphan")) | length
      },
      items: .
    }
  '
)"

printf '%s\n' "$report_json"

orphan_count="$(printf '%s\n' "$report_json" | jq '.summary.orphan')"
if [ "$strict" = true ] && [ "$orphan_count" != "0" ]; then
  exit 1
fi
