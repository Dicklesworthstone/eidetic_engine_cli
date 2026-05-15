#!/bin/sh
set -eu

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(dirname "$SCRIPT_DIR")"
INPUT=""
README="$PROJECT_ROOT/README.md"
HARDWARE_CLASSES="$PROJECT_ROOT/benches/baselines/hardware_classes.toml"
CHECK=false

usage() {
    cat <<'USAGE'
Usage: scripts/sync_perf_table.sh --input <ee-perf.v1.json> [--readme README.md] [--check]

Updates the README performance table from an ee.perf.v1 or
ee.perf.baseline.v1 artifact pinned to a declared hardware class.

With --check, exits non-zero if the README table is stale.
USAGE
}

while [ "$#" -gt 0 ]; do
    case "$1" in
        --input)
            shift
            [ "$#" -gt 0 ] || { echo "Missing value for --input" >&2; exit 1; }
            INPUT="$1"
            ;;
        --input=*) INPUT="${1#--input=}" ;;
        --readme)
            shift
            [ "$#" -gt 0 ] || { echo "Missing value for --readme" >&2; exit 1; }
            README="$1"
            ;;
        --readme=*) README="${1#--readme=}" ;;
        --hardware-classes)
            shift
            [ "$#" -gt 0 ] || { echo "Missing value for --hardware-classes" >&2; exit 1; }
            HARDWARE_CLASSES="$1"
            ;;
        --hardware-classes=*) HARDWARE_CLASSES="${1#--hardware-classes=}" ;;
        --check) CHECK=true ;;
        --help|-h)
            usage
            exit 0
            ;;
        *)
            echo "Unknown argument: $1" >&2
            usage >&2
            exit 1
            ;;
    esac
    shift
done

[ -n "$INPUT" ] || { echo "Missing required --input" >&2; usage >&2; exit 1; }
if [ ! -f "$INPUT" ]; then
    echo "Warning: performance artifact not found, leaving README untouched: $INPUT" >&2
    exit 0
fi
[ -f "$README" ] || { echo "README not found: $README" >&2; exit 1; }
[ -f "$HARDWARE_CLASSES" ] || { echo "Hardware class manifest not found: $HARDWARE_CLASSES" >&2; exit 1; }

if ! command -v jq >/dev/null 2>&1; then
    echo "jq is required to validate performance artifact JSON" >&2
    exit 1
fi

artifact_class="$(jq -r '
    .hardware_class
    // .hardwareClass
    // .hardware.class
    // .hardware.class_id
    // .hardwareClassId
    // empty
' "$INPUT")"

artifact_schema="$(jq -r '.schema // empty' "$INPUT")"
case "$artifact_schema" in
    ee.perf.v1|ee.perf.baseline.v1) ;;
    *)
        echo "Unsupported performance artifact schema '$artifact_schema'" >&2
        exit 2
        ;;
esac

relative_input="$INPUT"
case "$INPUT" in
    "$PROJECT_ROOT"/*) relative_input="${INPUT#"$PROJECT_ROOT"/}" ;;
esac

artifact_baseline_file="$(jq -r '.baseline_file // .baselineFile // empty' "$INPUT")"
relative_baseline_file="$artifact_baseline_file"
case "$artifact_baseline_file" in
    "$PROJECT_ROOT"/*) relative_baseline_file="${artifact_baseline_file#"$PROJECT_ROOT"/}" ;;
esac

manifest_class_for_target() {
    target="$1"
    awk -v target="$target" '
        /^\[\[classes\.[^]]+\.baselines\]\]/ {
            class = $0
            sub(/^\[\[classes\./, "", class)
            sub(/\.baselines\]\]$/, "", class)
            next
        }
        /^[[:space:]]*file[[:space:]]*=/ {
            value = $0
            sub(/^[^=]*=[[:space:]]*"/, "", value)
            sub(/".*$/, "", value)
            if (value == target && class != "") {
                print class
                exit
            }
        }
    ' "$HARDWARE_CLASSES"
}

manifest_class_for_file() {
    manifest_class_for_target "$relative_input"
}

manifest_class_for_baseline_file() {
    [ -n "$relative_baseline_file" ] || return 0
    manifest_class_for_target "$relative_baseline_file"
}

if [ -z "$artifact_class" ]; then
    artifact_class="$(manifest_class_for_file)"
fi
if [ -z "$artifact_class" ]; then
    artifact_class="$(manifest_class_for_baseline_file)"
fi

if [ -z "$artifact_class" ]; then
    echo "Performance artifact is not pinned to a hardware class: $relative_input" >&2
    echo "Add a hardware_class field to the artifact or list it in $HARDWARE_CLASSES." >&2
    exit 2
fi

if ! grep -Eq "^\[classes\.${artifact_class}\]$" "$HARDWARE_CLASSES"; then
    echo "Unknown performance hardware class '$artifact_class' for $relative_input" >&2
    exit 2
fi

if ! grep -q '<!-- perf:begin' "$README" || ! grep -q '<!-- perf:end -->' "$README"; then
    echo "README performance markers are missing" >&2
    exit 2
fi

canonical_baseline="$relative_input"
if [ -n "$relative_baseline_file" ]; then
    canonical_baseline="$relative_baseline_file"
fi

artifact_hash() {
    if command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$INPUT" | awk '{print $1}'
        return
    fi
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$INPUT" | awk '{print $1}'
        return
    fi
    echo "sha256-unavailable"
}

artifact_timestamp() {
    json_timestamp="$(jq -r '.timestamp // .generated_at // .generatedAt // empty' "$INPUT")"
    if [ -n "$json_timestamp" ]; then
        printf '%s\n' "$json_timestamp"
        return
    fi
    if stat -f '%Sm' -t '%Y-%m-%dT%H:%M:%SZ' "$INPUT" >/dev/null 2>&1; then
        TZ=UTC stat -f '%Sm' -t '%Y-%m-%dT%H:%M:%SZ' "$INPUT"
        return
    fi
    date -u -r "$INPUT" '+%Y-%m-%dT%H:%M:%SZ'
}

format_ms() {
    awk -v ms="$1" 'BEGIN {
        if (ms >= 1000) {
            seconds = ms / 1000
            if (seconds == int(seconds) && seconds >= 10) {
                printf "%d s", seconds
            } else {
                printf "%.1f s", seconds
            }
        } else if (ms == int(ms)) {
            printf "%d ms", ms
        } else {
            printf "%.1f ms", ms
        }
    }'
}

operation_value() {
    key="$1"
    field="$2"
    value="$(jq -r --arg key "$key" --arg field "$field" '.operations[$key][$field] // empty' "$INPUT")"
    if [ -z "$value" ]; then
        echo "Performance artifact missing operations.$key.$field" >&2
        exit 2
    fi
    printf '%s\n' "$value"
}

generated_block() {
    hash="$(artifact_hash)"
    synced_at="$(artifact_timestamp)"

    printf '<!-- perf:begin hardware-class=%s baseline=%s -->\n' "$artifact_class" "$canonical_baseline"
    printf '| Operation | Hardware class | p50 | p99 |\n'
    printf '|---|---|---:|---:|\n'
    while IFS='|' read -r key label; do
        [ -n "$key" ] || continue
        p50_ms="$(operation_value "$key" p50_ms)"
        p99_ms="$(operation_value "$key" p99_ms)"
        p50="$(format_ms "$p50_ms")"
        p99="$(format_ms "$p99_ms")"
        printf '| %s | `%s` | %s | %s |\n' "$label" "$artifact_class" "$p50" "$p99"
    done <<'ROWS'
ee_remember|`ee remember` (single record)
ee_search|`ee search "<q>"` (hybrid)
ee_context|`ee context "<task>"` (markdown, 4k tokens)
ee_why|`ee why <id>`
ee_workspace_init|`ee init --workspace <dir>` (clean)
ee_audit_query|`ee audit timeline --limit 1000`
ee_import_cass|`ee import cass --limit 50` (cold)
ee_graph_pagerank|`ee graph centrality-refresh` (PageRank, 5k links)
ee_index_rebuild|`ee index rebuild` (full)
ee_concurrent_writes|4 concurrent audited memory writers
ROWS
    printf 'Last synced: %s from sha256:%s\n' "$synced_at" "$hash"
    printf '<!-- perf:end -->\n'
}

current_block() {
    awk '
        /<!-- perf:begin/ { in_block = 1 }
        in_block { print }
        /<!-- perf:end -->/ && in_block { exit }
    ' "$README"
}

rewrite_readme() {
    tmp="${TMPDIR:-/tmp}/ee-readme-perf-sync.$$"
    block="${TMPDIR:-/tmp}/ee-readme-perf-block.$$"
    generated_block > "$block"
    awk -v block_file="$block" '
        BEGIN {
            while ((getline line < block_file) > 0) {
                block = block line "\n"
            }
            close(block_file)
            replaced = 0
        }
        /<!-- perf:begin/ {
            printf "%s", block
            in_block = 1
            replaced = 1
            next
        }
        /<!-- perf:end -->/ && in_block {
            in_block = 0
            next
        }
        !in_block { print }
        END {
            if (in_block || !replaced) {
                exit 3
            }
        }
    ' "$README" > "$tmp" || {
        echo "Failed to rewrite README performance block" >&2
        exit 2
    }
    cat "$tmp" > "$README"
}

if [ "$CHECK" = "true" ]; then
    expected="$(generated_block)"
    actual="$(current_block)"
    if [ "$expected" != "$actual" ]; then
        echo "README performance table is stale for $relative_input" >&2
        exit 1
    fi
    echo "perf_table_sync_check_ok class=$artifact_class artifact=$relative_input"
    exit 0
fi

rewrite_readme
echo "perf_table_sync_updated class=$artifact_class artifact=$relative_input readme=$README"
