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

surfaces_json="$("$SURFACE_EXTRACTOR" --prefix bd-)"
issues_json="$(br list --all --limit 0 --json)"
claim_rows_file="$(mktemp)"
status_rows_file="$(mktemp)"
match_rows_file="$(mktemp)"
trap 'rm -f "$claim_rows_file" "$status_rows_file" "$match_rows_file"' EXIT

printf '%s\n' "$issues_json" | jq -r --argjson surfaces "$surfaces_json" '
  .[]
  | select(.status == "open" or .status == "in_progress" or .status == "closed")
  | .id as $id
  | .status as $status
  | ($surfaces[$id] // [])[]?
  | [$id, $status, .]
  | @tsv
' > "$claim_rows_file"

git status --porcelain=v1 | awk '
  {
    status = substr($0, 1, 2)
    path = substr($0, 4)
    marker = " -> "
    marker_pos = index(path, marker)
    if (marker_pos > 0) {
      path = substr(path, marker_pos + length(marker))
    }
    print status "\t" path
  }
' > "$status_rows_file"

awk -F '\t' '
  function trim(value) {
    gsub(/^[[:space:]]+/, "", value)
    gsub(/[[:space:]]+$/, "", value)
    return value
  }

  function normalize(pattern) {
    sub(/ \(.*/, "", pattern)
    return trim(pattern)
  }

  function starts_with(value, prefix) {
    return substr(value, 1, length(prefix)) == prefix
  }

  function glob_to_regex(glob,    i, c, out, rest, close_pos) {
    out = "^"
    for (i = 1; i <= length(glob); i++) {
      c = substr(glob, i, 1)
      if (c == "*") {
        out = out ".*"
      } else if (c == "?") {
        out = out "."
      } else if (c == "[") {
        rest = substr(glob, i + 1)
        close_pos = index(rest, "]")
        if (close_pos > 0) {
          out = out "[" substr(rest, 1, close_pos)
          i += close_pos
        } else {
          out = out "\\["
        }
      } else if (index("\\.^$+(){}|]", c) > 0) {
        out = out "\\" c
      } else {
        out = out c
      }
    }
    return out "$"
  }

  function path_matches_surface(path, pattern,    normalized, prefix) {
    normalized = normalize(pattern)
    if (normalized == "") {
      return 0
    }
    if (normalized ~ /[[:space:]]/) {
      return 0
    }
    if (index(normalized, "all bd-") > 0) {
      return 0
    }
    if (path == normalized) {
      return 1
    }
    if (substr(normalized, length(normalized) - 2) == "/**") {
      prefix = substr(normalized, 1, length(normalized) - 3)
      return starts_with(path, prefix "/")
    }
    if (normalized ~ /[*?[]/) {
      return path ~ glob_to_regex(normalized)
    }
    if (substr(normalized, length(normalized), 1) == "/") {
      return starts_with(path, normalized)
    }
    return 0
  }

  NR == FNR {
    claim_count++
    claim_ids[claim_count] = $1
    claim_statuses[claim_count] = $2
    claim_surfaces[claim_count] = $3
    next
  }

  {
    item_index++
    git_status = $1
    path = $2
    matched = 0
    for (i = 1; i <= claim_count; i++) {
      if (path_matches_surface(path, claim_surfaces[i])) {
        matched = 1
        print item_index "\t" git_status "\t" path "\t" claim_ids[i] "\t" claim_statuses[i] "\t" claim_surfaces[i]
      }
    }
    if (!matched) {
      print item_index "\t" git_status "\t" path "\t\t\t"
    }
  }
' "$claim_rows_file" "$status_rows_file" > "$match_rows_file"

report_json="$(
  jq -Rn '
    def suggested_title($path): "Track file surface for \($path)";

    [
      inputs
      | split("\t")
      | {
          itemIndex: (.[0] | tonumber),
          gitStatus: .[1],
          path: .[2],
          beadId: .[3],
          beadStatus: .[4],
          surface: .[5]
        }
    ]
    | group_by(.itemIndex)
    | map(
        sort_by(.beadId, .surface)
        | .[0] as $first
        | [ .[] | select(.beadId != "") | {
            id: .beadId,
            status: .beadStatus,
            surface: .surface
          } ] as $claims
        | {
            path: $first.path,
            gitStatus: $first.gitStatus,
            claimedBy: $claims,
            warning: (if ($claims | length) == 0 then "untracked_work_orphan" else null end),
            suggestedBeadTitle: (if ($claims | length) == 0 then suggested_title($first.path) else null end)
          }
      )
    | {
      schema: "ee.untracked_work_audit.v1",
      summary: {
        changed: length,
        claimed: map(select((.claimedBy | length) > 0)) | length,
        orphan: map(select(.warning == "untracked_work_orphan")) | length
      },
      items: .
    }
  ' < "$match_rows_file"
)"

printf '%s\n' "$report_json"

orphan_count="$(printf '%s\n' "$report_json" | jq '.summary.orphan')"
if [ "$strict" = true ] && [ "$orphan_count" != "0" ]; then
  exit 1
fi
