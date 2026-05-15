#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
usage: scripts/extract_bead_file_surfaces.sh [--strict] [--prefix PREFIX] [--bead ID]

Emit a JSON object mapping bead IDs to paths declared in anchored
"FILE SURFACE:" description lines.

Options:
  --strict         fail if an implements-surface bead has no anchored surface
  --prefix PREFIX  only inspect bead IDs with this prefix (default: bd-3usjw.)
  --bead ID        emit {"bead_id": ID, "paths": [...]} for one bead
EOF
}

strict=false
prefix="bd-3usjw."
bead_id=""

while [ "$#" -gt 0 ]; do
  case "$1" in
    --strict)
      strict=true
      shift
      ;;
    --prefix)
      if [ "$#" -lt 2 ]; then
        echo "error: --prefix requires a value" >&2
        exit 2
      fi
      prefix="$2"
      shift 2
      ;;
    --bead)
      if [ "$#" -lt 2 ]; then
        echo "error: --bead requires a value" >&2
        exit 2
      fi
      bead_id="$2"
      shift 2
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

issues_json="$(br list --all --limit 0 --json)"

surfaces_json="$(
  printf '%s\n' "$issues_json" | jq --arg prefix "$prefix" '
    def trim:
      gsub("^[[:space:]]+"; "") | gsub("[[:space:]]+$"; "");

    def surface_payloads($description):
      ($description // "" | split("\n"))
      | map(select(test("^FILE SURFACE:[[:space:]]*")))
      | map(sub("^FILE SURFACE:[[:space:]]*"; "") | trim);

    def split_surface_paths($description):
      surface_payloads($description)
      | join(",")
      | split(",")
      | map(trim)
      | map(select(length > 0));

    reduce .[] as $issue ({};
      if ($issue.id | startswith($prefix)) then
        (split_surface_paths($issue.description)) as $paths
        | if ($paths | length) > 0 then
            . + {($issue.id): $paths}
          else
            .
          end
      else
        .
      end
    )
  '
)"

if [ "$strict" = true ]; then
  missing_json="$(
    printf '%s\n' "$issues_json" | jq --arg prefix "$prefix" --argjson surfaces "$surfaces_json" '
      [
        .[]
        | select(.id | startswith($prefix))
        | select(((.labels // []) | any(startswith("implements-surface:"))))
        | select(($surfaces[.id] // []) | length == 0)
        | {id, title}
      ]
    '
  )"
  missing_count="$(printf '%s\n' "$missing_json" | jq 'length')"
  if [ "$missing_count" != "0" ]; then
    echo "error: implements-surface beads missing FILE SURFACE annotations:" >&2
    printf '%s\n' "$missing_json" | jq -r '.[] | "  - \(.id): \(.title)"' >&2
    exit 1
  fi
fi

if [ -n "$bead_id" ]; then
  printf '%s\n' "$surfaces_json" | jq --arg bead_id "$bead_id" '
    {
      bead_id: $bead_id,
      paths: (.[$bead_id] // [])
    }
  '
  exit 0
fi

printf '%s\n' "$surfaces_json"
