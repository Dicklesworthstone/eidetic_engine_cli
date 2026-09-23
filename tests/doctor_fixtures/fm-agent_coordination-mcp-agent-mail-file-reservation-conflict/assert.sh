#!/usr/bin/env bash
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=tests/doctor_fixtures/lib.sh
. "$SCRIPT_DIR/../lib.sh"
FM="fm-agent_coordination-mcp-agent-mail-file-reservation-conflict"
if [ "${EE_DOCTOR_FIXTURE_RUN_EE:-0}" != "1" ]; then
    printf '%s assertion requires EE_DOCTOR_FIXTURE_RUN_EE=1; marker-only checks are insufficient\n' "$FM" >&2
    exit 2
fi
target="$(doctor_fixture_target)"
ee_bin="${EE_DOCTOR_FIXTURE_BINARY:-ee}"
base="$target/.fixture_baseline"
test -f "$(doctor_fixture_marker_dir "$target")/$FM.json"
leased="src/leased.rs"

# Independent witness: ee's coordination overlay still reads the workspace's
# lease as an expired exclusive reservation on a dirty path. The overlay only
# trusts a snapshot generated in the last few minutes, so the witness reads a
# copy with a fresh generated_at; the damaged snapshot itself is never touched.
jq --arg now "$(date -u +%Y-%m-%dT%H:%M:%SZ)" '.generated_at = $now' \
    "$target/.ee/agent-mail-snapshot.json" > "$base/agent-mail-snapshot.witness.json"
(cd "$target" && "$ee_bin" --workspace "$target" --json workspace hygiene \
    --agent-name DoctorFixture --agent-mail-snapshot "$base/agent-mail-snapshot.witness.json") \
    > "$base/hygiene-assert.json"
if ! jq -e --arg path "$leased" '
    .schema == "ee.response.v2" and .success == true and
    .data.coordinationState.agentMailAvailable == true and
    (.data.coordinationState.blockedByCoordination | length == 0) and
    any(.data.coordinationState.ignoredReservations[];
        .path == $path and .holderAgent == "StaleHolder" and .exclusive == true and
        (.reasons | index("expired_reservation_ignored")))
' "$base/hygiene-assert.json" >/dev/null; then
    printf 'fixture assert: %s witness lost: the lease is no longer an expired exclusive reservation; see %s\n' \
        "$FM" "$base/hygiene-assert.json" >&2
    exit 1
fi
doctor_fixture_assert_pinned_gap "$FM" ".ee/agent-mail-snapshot.json"
