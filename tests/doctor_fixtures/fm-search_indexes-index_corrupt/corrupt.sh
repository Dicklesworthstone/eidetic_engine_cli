#!/usr/bin/env bash
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=tests/doctor_fixtures/lib.sh
. "$SCRIPT_DIR/../lib.sh"

# REPAIR (bd-2oh15): a real populated index whose largest vector/segment file is
# replaced by same-size random bytes. doctor reports search_index EE-E301 and
# --fix dispatches run_index_rebuild. The original file is MOVED into the
# baseline; nothing is deleted.
FM="fm-search_indexes-index_corrupt"
target="$(doctor_fixture_target)"
ee_bin="${EE_DOCTOR_FIXTURE_BINARY:-ee}"
command -v "$ee_bin" >/dev/null
export EE_EMBED_DOWNLOAD="${EE_EMBED_DOWNLOAD:-off}"
doctor_fixture_prepare_target "$target"
doctor_fixture_healthy_store "$FM" "$target" "$ee_bin"
base="$target/.fixture_baseline"

# Provision the persistent coordination lock through a healthy no-op run before
# the baseline digest, so undo is compared against a digest that includes it.
"$ee_bin" doctor --workspace "$target" --fix --json > "$base/doctor-initialize-lock.json"
jq -e '.schema == "ee.response.v2" and .success == true and .data.actionCount == 0' \
    "$base/doctor-initialize-lock.json" >/dev/null
test -f "$target/.ee/.doctor.lock"

# Largest non-metadata index file, found portably (no GNU find -printf).
rel="" largest=-1
while IFS= read -r path; do
    size="$(wc -c < "$path" | tr -d ' ')"
    if [ "$size" -gt "$largest" ]; then
        largest="$size"
        rel="${path#"$target/.ee/index/"}"
    fi
done < <(find "$target/.ee/index" -type f ! -name meta.json)
test -n "$rel"
mkdir -p "$base/index-original/$(dirname "$rel")"
mv "$target/.ee/index/$rel" "$base/index-original/$rel"
head -c "$(wc -c < "$base/index-original/$rel" | tr -d ' ')" /dev/urandom > "$target/.ee/index/$rel"
printf '%s\n' "$rel" > "$base/corrupted-index-file"

doctor_fixture_corrupt "$FM" "P1" "search_indexes"
"$ee_bin" doctor --workspace "$target" --json > "$base/doctor-corrupt.json"
if ! jq -e '
    .schema == "ee.response.v2" and .success == true and .data.healthy == false and
    any(.data.actionable[]; .name == "search_index" and .errorCode == "EE-E301")
' "$base/doctor-corrupt.json" >/dev/null; then
    printf 'index_corrupt fixture did not produce search_index EE-E301; see %s\n' "$base/doctor-corrupt.json" >&2
    exit 1
fi
printf 'real corruption confirmed: index_corrupt (%s random bytes, EE-E301)\n' "$rel" >&2
