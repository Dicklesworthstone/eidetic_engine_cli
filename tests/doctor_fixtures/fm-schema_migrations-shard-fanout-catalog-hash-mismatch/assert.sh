#!/usr/bin/env bash
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=tests/doctor_fixtures/lib.sh
. "$SCRIPT_DIR/../lib.sh"
doctor_fixture_assert "fm-schema_migrations-shard-fanout-catalog-hash-mismatch" "P0" "schema_migrations"
