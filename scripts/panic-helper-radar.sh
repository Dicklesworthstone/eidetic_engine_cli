#!/usr/bin/env bash
# panic-helper-radar.sh -- Cargo-free scanner for touched Rust panic helpers.
#
# This catches newly introduced .expect* / .unwrap* calls before agents spend an
# RCH or CI Clippy slot on clippy::expect_used / clippy::unwrap_used fallout.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
GENERATED_AT="$(date -u +"%Y-%m-%dT%H:%M:%SZ")"
JSON_FLAG=0
QUIET=0
ADVISORY=0
SCAN_ALL=0
OUTPUT_PATH=""

declare -a INPUTS=()
declare -a SCAN_PATHS=()
declare -a VIOLATIONS=()
declare -a SKIPPED=()
declare -a SCANNED_DISPLAY_PATHS=()

usage() {
  cat <<'USAGE'
Usage: scripts/panic-helper-radar.sh [--json] [--quiet] [--advisory] [--all] [--output <path>] [file-or-dir ...]

  --json          Emit the JSON report to stdout.
  --quiet         Suppress the human-readable summary.
  --advisory      Exit 0 even when violations are found.
  --all           Scan all tracked Rust files. Without this and without file
                  arguments, only dirty/staged/untracked Rust files are scanned.
  --output <path> Write the JSON report to a file.

The report schema is ee.panic_helper_radar.v1. The scanner never runs Cargo,
never edits files, and only treats explicit clippy::expect_used /
clippy::unwrap_used allow annotations as suppressions.
USAGE
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --json)
      JSON_FLAG=1
      shift
      ;;
    --quiet)
      QUIET=1
      shift
      ;;
    --advisory)
      ADVISORY=1
      shift
      ;;
    --all)
      SCAN_ALL=1
      shift
      ;;
    --output)
      OUTPUT_PATH="${2:-}"
      shift 2
      ;;
    --help|-h)
      usage
      exit 0
      ;;
    --)
      shift
      while [ "$#" -gt 0 ]; do
        INPUTS+=("$1")
        shift
      done
      ;;
    -*)
      printf 'panic-helper-radar: unknown argument: %s\n' "$1" >&2
      usage >&2
      exit 1
      ;;
    *)
      INPUTS+=("$1")
      shift
      ;;
  esac
done

if ! command -v jq >/dev/null 2>&1; then
  printf 'panic-helper-radar: jq required but not found\n' >&2
  exit 2
fi

display_path_for() {
  local abs="$1"
  case "$abs" in
    "$ROOT"/*) printf '%s' "${abs#"$ROOT"/}" ;;
    *) printf '%s' "$abs" ;;
  esac
}

json_array_from_lines() {
  if [ "$#" -eq 0 ]; then
    printf '[]'
  else
    printf '%s\n' "$@" | jq -s '.'
  fi
}

json_string_array_from_lines() {
  if [ "$#" -eq 0 ]; then
    printf '[]'
  else
    printf '%s\n' "$@" | jq -R -s 'split("\n") | map(select(length > 0))'
  fi
}

add_skipped() {
  local path="$1"
  local reason="$2"
  local obj
  obj=$(jq -cn --arg path "$path" --arg reason "$reason" \
    '{path:$path,reason:$reason}')
  SKIPPED+=("$obj")
}

add_scan_path() {
  local input="$1"
  local abs rel existing

  case "$input" in
    /*) abs="$input" ;;
    *) abs="$ROOT/$input" ;;
  esac

  if [ ! -e "$abs" ]; then
    add_skipped "$input" "missing"
    return
  fi
  if [ -d "$abs" ]; then
    while IFS= read -r nested; do
      add_scan_path "$nested"
    done < <(find "$abs" -type f -name '*.rs' -print | sort)
    return
  fi
  if [ ! -f "$abs" ]; then
    add_skipped "$input" "not_regular_file"
    return
  fi
  case "$abs" in
    *.rs) ;;
    *)
      add_skipped "$input" "not_rust_file"
      return
      ;;
  esac

  rel="$(display_path_for "$abs")"
  for existing in "${SCAN_PATHS[@]}"; do
    [ "$existing" = "$abs" ] && return
  done
  SCAN_PATHS+=("$abs")
  SCANNED_DISPLAY_PATHS+=("$rel")
}

collect_default_paths() {
  if [ "$SCAN_ALL" -eq 1 ]; then
    git -C "$ROOT" ls-files '*.rs'
    return
  fi

  {
    git -C "$ROOT" diff --name-only -- '*.rs'
    git -C "$ROOT" diff --name-only --cached -- '*.rs'
    git -C "$ROOT" ls-files --others --exclude-standard -- '*.rs'
  } | sort -u
}

file_allows_lint() {
  local path="$1"
  local lint="$2"
  grep -Eq "^[[:space:]]*#![[:space:]]*\\[[[:space:]]*allow[[:space:]]*\\([^]]*clippy::${lint}" "$path"
}

nearby_allows_lint() {
  local path="$1"
  local line="$2"
  local lint="$3"
  local start=$((line - 8))
  if [ "$start" -lt 1 ]; then
    start=1
  fi
  sed -n "${start},${line}p" "$path" |
    grep -Eq "^[[:space:]]*#\\[[[:space:]]*allow[[:space:]]*\\([^]]*clippy::${lint}"
}

helper_for_line() {
  local text="$1"
  if [[ "$text" =~ \.expect_err[[:space:]]*\( ]]; then
    printf 'expect_err expect_used'
  elif [[ "$text" =~ \.expect[[:space:]]*\( ]]; then
    printf 'expect expect_used'
  elif [[ "$text" =~ \.unwrap_err[[:space:]]*\( ]]; then
    printf 'unwrap_err unwrap_used'
  elif [[ "$text" =~ \.unwrap[[:space:]]*\( ]]; then
    printf 'unwrap unwrap_used'
  fi
}

scan_file() {
  local path="$1"
  local rel line text helper lint helper_info obj

  rel="$(display_path_for "$path")"

  while IFS=: read -r line text; do
    [ -n "$line" ] || continue
    [[ "$text" =~ ^[[:space:]]*// ]] && continue

    helper_info="$(helper_for_line "$text")"
    [ -n "$helper_info" ] || continue
    read -r helper lint <<<"$helper_info"

    if file_allows_lint "$path" "$lint" || nearby_allows_lint "$path" "$line" "$lint"; then
      continue
    fi

    obj=$(jq -cn \
      --arg path "$rel" \
      --argjson line "$line" \
      --arg helper "$helper" \
      --arg lint "$lint" \
      --arg text "$(printf '%s' "$text" | sed 's/^[[:space:]]*//')" \
      '{path:$path,line:$line,helper:$helper,lint:$lint,text:$text}')
    VIOLATIONS+=("$obj")
  done < <(grep -nE '\.(expect_err|expect|unwrap_err|unwrap)[[:space:]]*\(' "$path" || true)
}

if [ "${#INPUTS[@]}" -eq 0 ]; then
  while IFS= read -r path; do
    [ -n "$path" ] && add_scan_path "$path"
  done < <(collect_default_paths)
else
  for input in "${INPUTS[@]}"; do
    add_scan_path "$input"
  done
fi

if [ "${#SCAN_PATHS[@]}" -gt 0 ]; then
  sorted_paths="$(printf '%s\n' "${SCAN_PATHS[@]}" | sort -u)"
  SCAN_PATHS=()
  SCANNED_DISPLAY_PATHS=()
  while IFS= read -r path; do
    [ -n "$path" ] || continue
    SCAN_PATHS+=("$path")
    SCANNED_DISPLAY_PATHS+=("$(display_path_for "$path")")
  done <<<"$sorted_paths"
fi

for path in "${SCAN_PATHS[@]}"; do
  scan_file "$path"
done

violations_json="$(json_array_from_lines "${VIOLATIONS[@]}")"
skipped_json="$(json_array_from_lines "${SKIPPED[@]}")"
scanned_paths_json="$(json_string_array_from_lines "${SCANNED_DISPLAY_PATHS[@]}")"
violation_count="${#VIOLATIONS[@]}"
skipped_count="${#SKIPPED[@]}"
scanned_count="${#SCAN_PATHS[@]}"
verdict="pass"
if [ "$violation_count" -gt 0 ]; then
  verdict="fail"
fi

summary_json="$(jq -cn \
  --argjson scannedFileCount "$scanned_count" \
  --argjson violationCount "$violation_count" \
  --argjson skippedCount "$skipped_count" \
  --argjson scannedPaths "$scanned_paths_json" \
  '{scannedFileCount:$scannedFileCount,violationCount:$violationCount,skippedCount:$skippedCount,scannedPaths:$scannedPaths}')"

report="$(jq -cn \
  --arg schema "ee.panic_helper_radar.v1" \
  --arg generatedAt "$GENERATED_AT" \
  --arg verdict "$verdict" \
  --argjson summary "$summary_json" \
  --argjson violations "$violations_json" \
  --argjson skipped "$skipped_json" \
  '{schema:$schema,generatedAt:$generatedAt,verdict:$verdict,summary:$summary,violations:$violations,skipped:$skipped}')"

if [ -n "$OUTPUT_PATH" ]; then
  case "$OUTPUT_PATH" in
    /*) output_abs="$OUTPUT_PATH" ;;
    *) output_abs="$ROOT/$OUTPUT_PATH" ;;
  esac
  printf '%s\n' "$report" > "$output_abs"
fi

if [ "$QUIET" -ne 1 ]; then
  printf 'panic-helper radar: verdict=%s scanned=%s violations=%s skipped=%s\n' \
    "$verdict" "$scanned_count" "$violation_count" "$skipped_count"
  if [ "$violation_count" -gt 0 ]; then
    printf '%s\n' "$report" |
      jq -r '.violations[] | "  \(.path):\(.line): \(.lint) via \(.helper) - \(.text)"'
  fi
fi

if [ "$JSON_FLAG" -eq 1 ]; then
  printf '%s\n' "$report"
fi

if [ "$violation_count" -gt 0 ] && [ "$ADVISORY" -ne 1 ]; then
  exit 4
fi
