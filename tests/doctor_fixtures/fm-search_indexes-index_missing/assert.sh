#!/usr/bin/env bash
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=tests/doctor_fixtures/lib.sh
. "$SCRIPT_DIR/../lib.sh"
if [ "${EE_DOCTOR_FIXTURE_RUN_EE:-0}" != "1" ]; then
    printf 'index_missing assertion requires EE_DOCTOR_FIXTURE_RUN_EE=1; marker-only checks are insufficient\n' >&2
    exit 2
fi
# Guidance-only FM (bd-2oh15 decision C): an index rebuild writes SQLite rows
# that cannot route through doctor_runtime::mutate() or be undone, so doctor
# records guidance. Assert that honestly instead of a repair round trip.
doctor_fixture_assert_guidance_only "fm-search_indexes-index_missing" \
    "search_index_missing" "search_index" "EE-E300" ".ee/index"
